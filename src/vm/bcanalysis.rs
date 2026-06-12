//! Shared bytecode-analysis primitives over a compiled [`Chunk`].
//!
//! These are pure ANALYSIS helpers — they READ a `Chunk`/`Op` stream and return
//! FACTS (top-level definition maps, statement boundaries, transitive reference
//! closures) — with NO synthesis: nothing here emits a fragment chunk. They are
//! shared by two consumers that would otherwise create a compile→worker layering
//! smell:
//!
//!   - the worker code-slice builder ([`crate::worker::dispatch`]), which uses the
//!     closure / def-discovery machinery to materialize a shippable module fragment;
//!   - the compile-layer tree-shaker ([`crate::compile::shake`]), which uses the same
//!     primitives to compute per-module keep-sets over a module graph.
//!
//! The boundary is deliberate: **analysis (reads a Chunk, returns facts) lives here;
//! synthesis (emits a fragment Chunk) stays in `worker::dispatch`.** A few helpers
//! ([`locate_class_group`] and its decode/`Control`-panic helpers) are pure analysis
//! that BOTH consumers and the synthesis path call — they live here and the slice
//! builder imports them.
//!
//! ## The closure algorithm (engine-agnostic, over global NAMES)
//!
//! A DIRECT-child top-level binding compiles to a `<value-producing op>;
//! DEFINE_GLOBAL "name"` pair in the program's top-level chunk — a top-level `fn`
//! is `CLOSURE proto_idx; DEFINE_GLOBAL name`, a top-level `const` is `CONST idx;
//! DEFINE_GLOBAL name` (or a bare `NIL`/`TRUE`/`FALSE` for those literals). A
//! function body references a top-level binding via `GET_GLOBAL "name"` (late-bound,
//! never an upvalue — verified: top-level fn protos have empty `upvalues`). So the
//! closure is a fixpoint over names: seed with the roots, scan each included fn's
//! chunk (recursively through nested `protos`) for `GET_GLOBAL` names, and pull in
//! any that resolve to a shippable top-level `fn` or LITERAL-initializer `const`,
//! recursing into newly-added fns. Unrelated top-level fns are never reached.

use crate::interp::Control;
use crate::value::Value;
use crate::vm::chunk::{Chunk, FnProto, InterfaceProto};
use crate::vm::opcode::Op;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A resolved top-level binding the closure can ship.
#[derive(Clone)]
pub(crate) enum TopDef {
    /// A top-level `fn` — ships as its `FnProto` (re-`CLOSURE`d in the fragment).
    Fn(Rc<FnProto>),
    /// A top-level `const`/`let` bound to a literal value — ships as that value.
    Const(Value),
    /// A top-level `const`/`let` whose initializer is a COMPUTED expression (a call,
    /// an arithmetic expression, an imported-module member access, …). The value
    /// cannot be precomputed at slice-build time, so the initializer's instruction
    /// byte-range `[start, end)` in the original top-level `code` is copied verbatim
    /// into the fragment (with pool indices remapped) and RE-RUN on the isolate, which
    /// recomputes the value there. The range covers the value-producing ops only — the
    /// trailing `DEFINE_GLOBAL` is re-emitted by the closure walk. Any `GET_GLOBAL` in
    /// the range is part of the transitive dependency closure (e.g. the helper fn the
    /// initializer calls, or an imported module name).
    ComputedConst { start: usize, end: usize },
    /// A top-level `class` — ships via `emit_class_recursive` (full method table +
    /// superclass chain), the SAME machinery the actor class-slice uses. A worker fn
    /// that constructs/returns a class instance pulls the class definition in here.
    Class,
    /// IFACE: a top-level `interface` — ships as its [`InterfaceProto`] (re-emitted as
    /// `Op::DefineInterface` + `DEFINE_GLOBAL` in the fragment, rebuilding a fresh
    /// descriptor `Rc` on the isolate, §5.3 per-isolate immortality). The proto carries
    /// only DATA (name + own method requirements + `extends` names); its transitive
    /// dependency edge is its `extends` parents (collected in [`collect_def_refs`]).
    Interface(Rc<InterfaceProto>),
}

/// Scan a program's top-level [`Chunk`] code stream and build a map from each
/// DIRECT-child top-level global NAME to the definition it binds. Each non-import
/// top-level binding compiles to a value-producing instruction RUN ending in
/// `DEFINE_GLOBAL name`; the run since the previous statement boundary is the
/// initializer. We classify each binding by inspecting that run:
///   - `CLOSURE idx; DEFINE_GLOBAL`      → a top-level `fn`     → [`TopDef::Fn`].
///   - `CONST idx; DEFINE_GLOBAL`        → a literal const      → [`TopDef::Const`]
///     (a `Value::Enum` const lands here too — enums ship as a value).
///   - `NIL/TRUE/FALSE; DEFINE_GLOBAL`   → a literal const      → [`TopDef::Const`].
///   - a run ending in `CLASS; DEFINE_GLOBAL` → a `class`       → [`TopDef::Class`]
///     (shipped via `emit_class_recursive`, not from this map's range).
///   - ANY OTHER value-producing run     → a computed const/let → [`TopDef::ComputedConst`]
///     (the run's byte-range is copied + re-run on the isolate).
///
/// `import` bindings are NOT in this map (they are shipped wholesale by
/// `emit_top_imports`). For a computed const the tracked range `[start, define_ip)`
/// is EXACTLY its own initializer — the run since the most recent top-level STATEMENT
/// boundary (NOT merely since the last define/import: a preceding bare expression,
/// `for`, `while`, or `if` statement must NOT be absorbed into the const's range). The
/// boundaries come from [`top_level_statement_starts`] (a CFG-aware top-level pass).
pub(crate) fn top_level_defs(top: &Chunk) -> HashMap<Rc<str>, TopDef> {
    let mut defs: HashMap<Rc<str>, TopDef> = HashMap::new();
    // The offsets at which a fresh top-level statement BEGINS (sorted ascending). A
    // computed const's initializer starts at the greatest boundary `<= define_ip`.
    let starts = top_level_statement_starts(top);

    let mut prev: Option<(Op, u16)> = None;
    let mut ip = 0usize;
    while ip < top.code.len() {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        let operand_u16 = if width >= 2 { top.read_u16(ip + 1) } else { 0 };

        if op == Op::DefineGlobal {
            if let Some(Value::Str(name)) = top.consts.get(operand_u16 as usize) {
                // The initializer starts at the last statement boundary at-or-before
                // this DEFINE_GLOBAL (the start of THIS binding's own statement).
                let stmt_start = starts
                    .iter()
                    .copied()
                    .take_while(|&s| s <= ip)
                    .last()
                    .unwrap_or(0);
                let def = classify_binding(top, prev, stmt_start, ip);
                defs.entry(name.clone()).or_insert(def);
            }
        }

        prev = Some((op, operand_u16));
        ip += 1 + width;
    }
    defs
}

/// Compute the byte-offsets at which each TOP-LEVEL statement begins, in ascending
/// order. Used to bound a computed-`const` initializer to exactly its own statement
/// (so a preceding bare-expr / `for` / `while` / `if`, OR the nested statements inside
/// such a control structure, are never absorbed into the const's shipped range).
///
/// ## Algorithm — depth-0 fall-through offsets NOT enclosed by any jump span
///
/// Every top-level statement is stack-NEUTRAL, so the CFG-accurate operand-stack ENTRY
/// depth is 0 at each top-level statement boundary. We compute that depth with a
/// worklist join (the naive LINEAR scan is wrong across branches — `a ? b : c` would
/// double-count both arms), reusing the verifier's authoritative per-op delta
/// ([`crate::vm::verify::op_stack_delta`]) and jump decoding.
///
/// Depth 0 alone is NOT sufficient: a control structure contains nested balanced
/// statements (a `for` body's `i + 1; POP` returns to depth 0 INSIDE the loop), and a
/// ternary's arms start at depth 0. The distinguishing fact: a statement INTERIOR to a
/// control structure is ENCLOSED by that structure's jump — strictly inside `(lo, hi)`
/// of any jump span, or the TARGET of a backward `Loop` (the loop header). So a
/// statement LEADER is: offset 0, OR a reachable depth-0 offset enclosed by NO jump
/// span. (A loop-EXIT offset is the FORWARD target of the loop's exit `JUMP_IF_FALSE`,
/// so it sits outside every span and is correctly a leader; the merge point of a
/// ternary carries the partial value at depth ≥ 1, so the depth-0 filter excludes it
/// without any extra "predecessor is a jump" rule — which would wrongly drop the
/// loop exit, whose physical predecessor is the back-edge `Loop`.)
pub(crate) fn top_level_statement_starts(top: &Chunk) -> Vec<usize> {
    // Decode all top-level instruction offsets + an offset->index map.
    let mut offsets: Vec<usize> = Vec::new();
    let mut ip = 0usize;
    while ip < top.code.len() {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        offsets.push(ip);
        ip += 1 + op.operand_width();
    }
    let index_of: HashMap<usize, usize> = offsets.iter().enumerate().map(|(i, &o)| (o, i)).collect();

    // CFG-accurate entry depth at each instruction (None = unreached); plus the jump
    // SPANS. Each span is `(lo, hi, target_interior)`: an offset is control-structure
    // interior if it lies strictly inside `(lo, hi)`, OR equals the jump TARGET when
    // that target is interior. A BACKWARD jump (`Loop`) targets the loop HEADER, which
    // IS interior (the loop's condition/body), so its target is interior. A FORWARD
    // jump targets the merge/exit, which is the NEXT statement's leader (NOT interior).
    let mut entry: Vec<Option<isize>> = vec![None; offsets.len()];
    let mut spans: Vec<(usize, usize, Option<usize>)> = Vec::new();
    let mut work: Vec<(usize, isize)> = vec![(0, 0)];
    while let Some((i, d)) = work.pop() {
        if entry[i].is_some() {
            continue;
        }
        entry[i] = Some(d);
        let off = offsets[i];
        let op = Op::from_u8(top.code[off]).expect("decoded above");
        let exit = d + crate::vm::verify::op_stack_delta(top, op, off + 1);
        let is_uncond = matches!(op, Op::Jump | Op::Loop);
        let is_cond = matches!(op, Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNotNil);
        if !is_uncond && !matches!(op, Op::Return | Op::Yield) {
            if let Some(&ni) = index_of.get(&(off + 1 + op.operand_width())) {
                work.push((ni, exit));
            }
        }
        if is_uncond || is_cond {
            // Jump operand is an i16 displacement from the byte after the operand.
            let disp = top.read_i16(off + 1) as isize;
            let target = (off + 1 + 2) as isize + disp;
            if target >= 0 {
                let t = target as usize;
                let backward = t < off; // a `Loop` back-edge to the loop header.
                let interior_target = if backward { Some(t) } else { None };
                spans.push((off.min(t), off.max(t), interior_target));
                if let Some(&ti) = index_of.get(&t) {
                    work.push((ti, exit));
                }
            }
        }
    }
    let enclosed = |o: usize| {
        spans
            .iter()
            .any(|&(lo, hi, tgt)| (lo < o && o < hi) || tgt == Some(o))
    };

    // A statement LEADER is offset 0, or any reachable DEPTH-0 offset NOT enclosed by a
    // jump span. Enclosure already excludes control-structure interiors AND a ternary/
    // short-circuit's arm starts; the depth-0 filter excludes an expression's interior
    // merge points (which carry the partial value, depth ≥ 1). No "previous op is a
    // jump" rule is needed — a loop-EXIT leader's physical predecessor IS the back-edge
    // `Loop`, so such a rule would wrongly drop it.
    let mut starts: Vec<usize> = Vec::new();
    for (i, &off) in offsets.iter().enumerate() {
        if i == 0 {
            starts.push(off); // the program's first statement.
            continue;
        }
        if enclosed(off) {
            continue; // interior to a loop/conditional → not a top-level boundary.
        }
        if entry[i] == Some(0) {
            starts.push(off);
        }
    }
    starts
}

/// Classify the binding terminated by the `DEFINE_GLOBAL` at `define_ip`, given the
/// value-producing run `[stmt_start, define_ip)` and the immediately-`prev`ious op.
fn classify_binding(
    top: &Chunk,
    prev: Option<(Op, u16)>,
    stmt_start: usize,
    define_ip: usize,
) -> TopDef {
    match prev {
        // A single-op literal/fn/class producer immediately before the DEFINE_GLOBAL.
        Some((Op::Closure, operand)) if stmt_start == define_ip.saturating_sub(3) => {
            if let Some(p) = top.protos.get(operand as usize) {
                return TopDef::Fn(p.clone());
            }
            TopDef::ComputedConst { start: stmt_start, end: define_ip }
        }
        Some((Op::Const, operand)) if stmt_start == define_ip.saturating_sub(3) => {
            if let Some(v) = top.consts.get(operand as usize) {
                return TopDef::Const(v.clone());
            }
            TopDef::ComputedConst { start: stmt_start, end: define_ip }
        }
        Some((Op::Nil, _)) if stmt_start == define_ip.saturating_sub(1) => {
            TopDef::Const(Value::Nil)
        }
        Some((Op::True, _)) if stmt_start == define_ip.saturating_sub(1) => {
            TopDef::Const(Value::Bool(true))
        }
        Some((Op::False, _)) if stmt_start == define_ip.saturating_sub(1) => {
            TopDef::Const(Value::Bool(false))
        }
        // A run ending in `CLASS` is a `class` declaration — shipped via the class
        // machinery, not from this map's range.
        Some((Op::Class, _)) => TopDef::Class,
        // IFACE: a run ending in `DEFINE_INTERFACE` is an `interface` declaration —
        // ship its proto, re-emitted as DEFINE_INTERFACE + DEFINE_GLOBAL.
        Some((Op::DefineInterface, operand)) if stmt_start == define_ip.saturating_sub(3) => {
            if let Some(p) = top.interface_protos.get(operand as usize) {
                return TopDef::Interface(p.clone());
            }
            TopDef::ComputedConst { start: stmt_start, end: define_ip }
        }
        // Anything else is a computed-initializer binding: ship its instruction range.
        _ => TopDef::ComputedConst { start: stmt_start, end: define_ip },
    }
}

/// Collect every global NAME referenced by `GET_GLOBAL` in `chunk` and, recursively,
/// in its nested function `protos` (a fn defined inside the body, an arrow, a field
/// default thunk, …). Names are appended to `out` (de-duplication is the caller's
/// fixpoint set).
pub(crate) fn collect_get_global_names(chunk: &Chunk, out: &mut Vec<Rc<str>>) {
    let mut ip = 0usize;
    while ip < chunk.code.len() {
        let Some(op) = Op::from_u8(chunk.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        if op == Op::GetGlobal {
            let idx = chunk.read_u16(ip + 1) as usize;
            if let Some(Value::Str(name)) = chunk.consts.get(idx) {
                out.push(name.clone());
            }
        }
        ip += 1 + width;
    }
    // Recurse into nested function bodies (their GET_GLOBALs are part of the entry's
    // transitive references too).
    for proto in &chunk.protos {
        collect_get_global_names(&proto.chunk, out);
    }
}

/// Collect every `GET_GLOBAL` name in the top-level instruction byte-range
/// `[start, end)` of `top.code` (a computed-const initializer), recursing into any
/// `CLOSURE`'d nested proto the range references. These are the computed const's
/// transitive top-level dependencies (the helper fn it calls, an imported module
/// name, another const it reads, …) — fed into the same closure fixpoint.
pub(crate) fn collect_range_refs(top: &Chunk, start: usize, end: usize, out: &mut Vec<Rc<str>>) {
    let mut ip = start;
    while ip < end {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        match op {
            Op::GetGlobal => {
                let idx = top.read_u16(ip + 1) as usize;
                if let Some(Value::Str(name)) = top.consts.get(idx) {
                    out.push(name.clone());
                }
            }
            Op::Closure => {
                // A nested closure literal inside the initializer (e.g. a lambda) —
                // its body's GET_GLOBALs are part of the closure too.
                let idx = top.read_u16(ip + 1) as usize;
                if let Some(proto) = top.protos.get(idx) {
                    collect_get_global_names(&proto.chunk, out);
                }
            }
            _ => {}
        }
        ip += 1 + width;
    }
}

/// Collect the outgoing top-level references of a single resolved [`TopDef`] `name`
/// into `out` — the names the closure must pull in next. A `fn` contributes its
/// proto body's `GET_GLOBAL`s; a computed const contributes its initializer range's;
/// a `class` contributes its method/default bodies' (via [`locate_class_group`]); a
/// literal const contributes nothing.
fn collect_def_refs(top: &Chunk, def: &TopDef, name: &str, out: &mut Vec<Rc<str>>) {
    match def {
        TopDef::Fn(proto) => collect_get_global_names(&proto.chunk, out),
        TopDef::ComputedConst { start, end } => collect_range_refs(top, *start, *end, out),
        TopDef::Class => {
            // Pull the class's method/default bodies' refs (and its superclass names).
            if let Ok((members, cp)) = locate_class_group(top, name) {
                for m in &members {
                    match m {
                        GroupMember::SuperGlobal(sup) => out.push(sup.clone()),
                        GroupMember::Closure(proto) => {
                            collect_get_global_names(&proto.chunk, out)
                        }
                    }
                }
                // SOUNDNESS (validate_into): a class's declared FIELD TYPES carry no
                // bytecode reference, but they ARE load-bearing at runtime. `.from` /
                // typed-parse validation walks each field's declared `Type` through the
                // class's `def_env`: `coerce_field` resolves a `Type::Named` leaf and,
                // finding a `Value::Class`, COERCES a raw Object into a nested class
                // instance (recursing element-wise through `array<Class>` / `map<K,Class>`);
                // `check_type_env` likewise resolves an interface-typed leaf for a
                // structural `conforms`. A class/enum/interface referenced ONLY as a field
                // type therefore has no `GET_GLOBAL` edge yet MUST be kept — dropping it
                // makes the env lookup fail and breaks the contract check. Walk every
                // `Type::Named` leaf of every field type (this is intentionally
                // conservative: walking a leaf that turns out to be a primitive/unbound
                // name is a harmless under-shake, never an unsound drop).
                let mut named: HashSet<Rc<str>> = HashSet::new();
                for fs in cp.class.fields.values() {
                    collect_type_named_refs(&fs.ty, &mut named);
                }
                out.extend(named);
            }
        }
        TopDef::Interface(proto) => {
            // IFACE: an interface's transitive top-level dependency edge is its
            // `extends` parents (the lazy-flatten dependency, §4) — so shipping
            // `ReadWriter` pulls in `Reader` + `Writer`. Method *signatures* carry no
            // executable bodies, so there are no `GET_GLOBAL`s to walk.
            for parent in &proto.extends {
                out.push(Rc::from(parent.as_str()));
            }
        }
        TopDef::Const(v) => {
            // SOUNDNESS (validate_into, conservative): an `enum` ships as a
            // `TopDef::Const` holding a `Value::Enum`. A PAYLOAD variant
            // (`Circle(radius: SomeClass)`) declares per-field `Type`s in its
            // `VariantSchema`. The enum CONSTRUCTOR path (`construct_variant_args`) uses
            // the ENV-FREE `check_type` and does NOT coerce a raw Object into a nested
            // class, so a class used only as an enum payload field type is not, today,
            // strictly load-bearing through `validate_into`. We walk these field types
            // ANYWAY: it is a sound under-shake (never drops live code), it future-proofs
            // the closure against a payload-coercion path being added, and it keeps the
            // worker slice conservative. A non-enum const contributes nothing.
            if let Value::Enum(def) = v {
                let mut named: HashSet<Rc<str>> = HashSet::new();
                for schema in def.variant_schemas.values() {
                    for (_fname, ty) in &schema.fields {
                        collect_type_named_refs(ty, &mut named);
                    }
                }
                out.extend(named);
            }
        }
    }
}

/// Collect every `Type::Named` leaf reachable in `ty`, descending through all type
/// combinators (`Optional`/`T?`, `Array<T>`, `Map<K,V>`, `Union`, `Tuple`, `Result`,
/// `Future`, `FnSig`), into `out`. Used by [`collect_def_refs`] to add a class's /
/// enum's FIELD-TYPE references to the reachability closure (the `validate_into`
/// soundness fix): a class/enum/interface named only as a field type carries no
/// bytecode `GET_GLOBAL`, but is resolved through the env at runtime by `coerce_field`
/// / `check_type_env`, so it MUST be kept by the tree-shaker.
///
/// Intentionally conservative: a `Type::Named` leaf is pushed regardless of whether it
/// ultimately resolves to a class, enum, interface, or an unbound name — walking a
/// non-class leaf is a harmless under-shake (it merely keeps an already-present global),
/// never an unsound drop. `Type::Param` (a runtime-ERASED generic parameter) names no
/// shippable global, so it is NOT walked.
fn collect_type_named_refs(ty: &crate::ast::Type, out: &mut HashSet<Rc<str>>) {
    use crate::ast::Type;
    match ty {
        Type::Named(name) => {
            out.insert(Rc::from(name.as_str()));
        }
        Type::Optional(inner)
        | Type::Array(inner)
        | Type::Result(inner)
        | Type::Future(inner) => collect_type_named_refs(inner, out),
        Type::Map(k, v) | Type::Union(k, v) => {
            collect_type_named_refs(k, out);
            collect_type_named_refs(v, out);
        }
        Type::Tuple(items) => {
            for t in items {
                collect_type_named_refs(t, out);
            }
        }
        Type::FnSig(params, ret) => {
            for t in params {
                collect_type_named_refs(t, out);
            }
            collect_type_named_refs(ret, out);
        }
        // Primitive/erased leaves name no shippable global.
        Type::Number
        | Type::Int
        | Type::Float
        | Type::String
        | Type::Bool
        | Type::Nil
        | Type::Any
        | Type::Fn
        | Type::Object
        | Type::Error
        | Type::Param(_) => {}
    }
}

/// Compute the transitive top-level dependency closure of `roots` over `defs`,
/// returning the set of included NAMES. Shared by the worker-fn slice and the actor
/// class slice so both ship the same closure semantics (fns + literal consts +
/// computed consts + classes, transitively).
pub(crate) fn compute_closure(
    top: &Chunk,
    defs: &HashMap<Rc<str>, TopDef>,
    roots: Vec<Rc<str>>,
) -> HashSet<Rc<str>> {
    let mut seen: HashSet<Rc<str>> = HashSet::new();
    let mut work: Vec<Rc<str>> = roots;
    while let Some(name) = work.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(def) = defs.get(name.as_ref()) {
            let mut refs = Vec::new();
            collect_def_refs(top, def, &name, &mut refs);
            for r in refs {
                if !seen.contains(&r) {
                    work.push(r);
                }
            }
        }
    }
    seen
}

// ---------------------------------------------------------------------------
// Class-group analysis — pure decode of a `class`'s `Op::Class` instruction group.
//
// This is ANALYSIS (reads the chunk, returns ordered members + the `ClassProto`),
// shared by [`collect_def_refs`] (to pull a class's method/superclass refs into the
// closure) AND by the worker slice builder's `emit_class_recursive` synthesis (to
// re-emit the group into a fragment). The synthesis path imports these.
// ---------------------------------------------------------------------------

/// One copied member of a class group, in stack order below `Op::Class`.
pub(crate) enum GroupMember {
    /// A superclass class-value push: `GET_GLOBAL <name>` for a top-level class.
    SuperGlobal(Rc<str>),
    /// A `CLOSURE proto_idx` (default thunk / method / static), carrying its proto.
    Closure(Rc<FnProto>),
}

/// Locate the `Op::Class` instruction for `class_name` in `top` and decode its
/// instruction group into ordered members. Returns the members (bottom-to-top of
/// the stack: optional superclass push, then thunk/method/static closures) plus the
/// `ClassProto`. Errors (recoverable panic) if the class is missing, is not a
/// top-level DEFINE_GLOBAL, or its group contains an unsupported instruction
/// (anything other than a contiguous run of `CLOSURE`/superclass `GET_GLOBAL`).
pub(crate) fn locate_class_group(
    top: &Chunk,
    class_name: &str,
) -> Result<(Vec<GroupMember>, Rc<crate::vm::chunk::ClassProto>), Control> {
    // Walk the code, tracking the run of instructions since the last "break" op.
    // A class group is a contiguous run of `CLOSURE` ops (and, for `extends`, a
    // single leading superclass `GET_GLOBAL`) ending at `Op::Class`.
    let mut run: Vec<(Op, u16)> = Vec::new();
    let mut ip = 0usize;
    while ip < top.code.len() {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        let operand = if width >= 2 { top.read_u16(ip + 1) } else { 0 };
        match op {
            Op::Closure | Op::GetGlobal => run.push((op, operand)),
            Op::Class => {
                let cp = top
                    .class_protos
                    .get(operand as usize)
                    .cloned()
                    .ok_or_else(|| panic_build("class proto index out of range"))?;
                if cp.class.name == class_name {
                    return decode_class_group(top, &run, cp);
                }
                run.clear();
            }
            _ => run.clear(),
        }
        ip += 1 + width;
    }
    Err(panic_build(&format!(
        "worker class '{class_name}' is not a top-level class in this program"
    )))
}

/// Decode a class group's preceding instruction `run` into ordered [`GroupMember`]s.
fn decode_class_group(
    top: &Chunk,
    run: &[(Op, u16)],
    cp: Rc<crate::vm::chunk::ClassProto>,
) -> Result<(Vec<GroupMember>, Rc<crate::vm::chunk::ClassProto>), Control> {
    let n_closures = cp.default_fields.len() + cp.method_names.len() + cp.static_method_names.len();
    // The group's instructions, in stack-push order, are the LAST `expected` ops of
    // `run` (a top-level `fn`'s `CLOSURE; DEFINE_GLOBAL` breaks the run, so the run
    // ends exactly at this class group). For `extends` a leading superclass push
    // (`GET_GLOBAL`) precedes the closure run.
    let expected = n_closures + usize::from(cp.has_super);
    if run.len() < expected {
        return Err(panic_build(
            "worker class group is malformed (too few preceding instructions)",
        ));
    }
    let group = &run[run.len() - expected..];
    let mut members = Vec::with_capacity(expected);
    let mut idx = 0;
    if cp.has_super {
        let (op, operand) = group[0];
        if op != Op::GetGlobal {
            return Err(panic_build(
                "worker class with `extends` must reference a top-level superclass \
                 (a non-global superclass is not yet shippable to an actor isolate)",
            ));
        }
        let name = class_name_from_const(top, operand)?;
        members.push(GroupMember::SuperGlobal(name));
        idx = 1;
    }
    for (op, operand) in &group[idx..] {
        if *op != Op::Closure {
            return Err(panic_build(
                "worker class methods/defaults must not capture an enclosing local \
                 (only top-level worker classes are shippable to an actor isolate)",
            ));
        }
        let proto = top
            .protos
            .get(*operand as usize)
            .cloned()
            .ok_or_else(|| panic_build("class member proto index out of range"))?;
        // A method/default closure that captures an enclosing variable via an upvalue
        // cannot be shipped — the upvalue would dangle in the fresh isolate. Top-level
        // class members reference only globals (`GET_GLOBAL`) + their own params/`self`,
        // so their protos have NO upvalues. A non-empty `upvalues` means the class is
        // nested inside another function scope (unsupported for actor spawning).
        if !proto.chunk.upvalues.is_empty() {
            return Err(panic_build(
                "worker class member captures an enclosing local — only top-level \
                 worker classes (whose members reference globals only) can be spawned",
            ));
        }
        members.push(GroupMember::Closure(proto));
    }
    Ok((members, cp))
}

fn class_name_from_const(top: &Chunk, operand: u16) -> Result<Rc<str>, Control> {
    match top.consts.get(operand as usize) {
        Some(Value::Str(s)) => Ok(s.clone()),
        _ => Err(panic_build("superclass GET_GLOBAL has no name constant")),
    }
}

fn panic_build(msg: &str) -> Control {
    Control::Panic(crate::error::AsError::new(msg.to_string()))
}
