//! Cross-thread transport + the dependency-closure / code-slice builder.
//!
//! Task 7 implements only the **code-slice** half: from a `worker fn`'s compiled
//! entry, compute its transitive top-level dependency closure (the other top-level
//! `fn`s and `const`s it references, and what THOSE reference, …) and materialize a
//! self-contained "module fragment" `.aso` that, when loaded into a FRESH isolate's
//! `Vm`, defines exactly those globals (and the entry) — nothing else from the
//! original module. The `Send` byte-channel transport + the isolate pool are Task 8;
//! this module deliberately leaves that seam open.
//!
//! ## The closure algorithm (engine-agnostic, over global NAMES)
//!
//! A DIRECT-child top-level binding compiles to a `<value-producing op>;
//! DEFINE_GLOBAL "name"` pair in the program's top-level chunk — a top-level `fn`
//! is `CLOSURE proto_idx; DEFINE_GLOBAL name`, a top-level `const` is `CONST idx;
//! DEFINE_GLOBAL name` (or a bare `NIL`/`TRUE`/`FALSE` for those literals). A
//! function body references a top-level binding via `GET_GLOBAL "name"` (late-bound,
//! never an upvalue — verified: top-level fn protos have empty `upvalues`). So the
//! closure is a fixpoint over names: seed with the entry, scan each included fn's
//! chunk (recursively through nested `protos`) for `GET_GLOBAL` names, and pull in
//! any that resolve to a shippable top-level `fn` or LITERAL-initializer `const`,
//! recursing into newly-added fns. Unrelated top-level fns are never reached, so
//! they are never shipped.
//!
//! ## What gets shipped — and what is left late-bound
//!
//! The closure ships, transitively, every top-level binding a `worker fn` body
//! references:
//!
//!   - **FUNCTIONS** — re-`CLOSURE`d from their `FnProto`.
//!   - **`enum`s and literal-initializer `const`s** — copied by VALUE.
//!   - **COMPUTED-initializer `const`s** — the initializer's instruction range is
//!     copied (pool-remapped) into the fragment and RE-RUN on the isolate, which
//!     recomputes the value (`copy_code_range` / `TopDef::ComputedConst`). The helper
//!     fn / imported module the initializer uses is itself pulled into the closure.
//!   - **`class`es** — the full class (superclass chain + method table + field
//!     defaults) is shipped via the SAME `emit_class_recursive` machinery the actor
//!     class-slice uses, so a worker fn can construct / return a class instance.
//!   - **Top-level `import`s** — ALL top-level imports are shipped wholesale via
//!     `emit_top_imports` (the actor path's mechanism), so a worker body can call
//!     `math.max(...)`, `array.sort(...)`, `json.parse(...)`, etc. (std imports are
//!     side-effect-free; a file import re-runs its module on the isolate — the
//!     shared-nothing analog of a fresh process).
//!
//! A referenced name that is none of the above (a plain builtin, or a genuinely
//! unbound name) is left as a late-bound `GET_GLOBAL`: on the far isolate it resolves
//! against the isolate's own globals/builtins, or — if truly absent — raises the
//! STANDARD recoverable `undefined variable '<name>'` panic at the call site, exactly
//! as any unbound reference would. So `build_code_slice` returns `Ok` with the slice;
//! a genuine miss surfaces LATER, loudly, at isolate runtime — never a wrong or
//! silently-partial result.
//!
//! Remaining non-goal: a NON-top-level dep (a `class`/`fn` nested inside another
//! function whose members capture an enclosing local via an upvalue) is not shippable
//! — the `worker-capture` checker rejects such captures up front, and the actor
//! class-slice path reports a recoverable build-time panic for a captured class
//! member.

use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use crate::vm::chunk::{Chunk, FnProto, InterfaceProto};
use crate::vm::opcode::Op;
use crate::worker::WorkerCodeSlice;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
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
    /// A top-level `class` — ships via [`emit_class_recursive`] (full method table +
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
///     (shipped via [`emit_class_recursive`], not from this map's range).
///   - ANY OTHER value-producing run     → a computed const/let → [`TopDef::ComputedConst`]
///     (the run's byte-range is copied + re-run on the isolate).
///
/// `import` bindings are NOT in this map (they are shipped wholesale by
/// [`emit_top_imports`]). For a computed const the tracked range `[start, define_ip)`
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
fn collect_get_global_names(chunk: &Chunk, out: &mut Vec<Rc<str>>) {
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
            if let Ok((members, _cp)) = locate_class_group(top, name) {
                for m in &members {
                    match m {
                        GroupMember::SuperGlobal(sup) => out.push(sup.clone()),
                        GroupMember::Closure(proto) => {
                            collect_get_global_names(&proto.chunk, out)
                        }
                    }
                }
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
        TopDef::Const(_) => {}
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

/// A stable identity hash for a worker entry: its `class_name` (if any) + its
/// function name. Used to key the per-isolate code-slice cache so a repeatedly
/// dispatched worker ships its bytecode at most once per isolate.
///
/// NOTE (Task 8): this hashes ONLY name + class — NOT the module path or the entry's
/// def-span. It is therefore safe as a SINGLE-PROGRAM per-isolate cache key (one
/// running program, distinct worker fn names), but two DIFFERENT programs with a
/// same-named worker fn would collide if a cache is ever SHARED across programs.
/// If Task 8 introduces a cross-program/shared isolate cache, fold the module
/// identity (path + def-span) into this hash.
fn entry_fn_id(name: &str, class_name: Option<&str>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    class_name.hash(&mut h);
    name.hash(&mut h);
    h.finish()
}

/// Build the shippable [`WorkerCodeSlice`] for the worker entry named `entry_name`
/// out of the program's top-level [`Chunk`] `top`. Computes the transitive
/// dependency closure (see the module docs) and materializes it as a fresh
/// "module fragment" top-level chunk serialized via the `.aso` writer:
///
/// The fragment, when loaded and run on a FRESH `Vm`, emits, in order: every
/// top-level `import`; each referenced `class` (superclass-first); then each fn /
/// literal-const / computed-const dep in source order (a literal `const` → `CONST;
/// DEFINE_GLOBAL`, a `fn` → `CLOSURE; DEFINE_GLOBAL`, a computed const → its
/// initializer instruction range + `DEFINE_GLOBAL`); and finally the entry fn's own
/// define, then `NIL; RETURN`. Running it defines exactly the closure's globals (and
/// the entry) and NOTHING else from the original module — so the isolate can fetch and
/// call the entry with zero access to the original heap. See the module docs for the
/// full "what gets shipped" list.
///
/// `class_name` is `Some` for a `static worker fn` (binds the class); for a free
/// `worker fn` it is `None`.
///
/// Returns a recoverable `Control::Panic` ONLY when the entry itself is missing or is
/// not a top-level function (or a class member captures an enclosing local — see
/// `locate_class_group`). A referenced name that is neither a shippable def nor a
/// builtin is NOT a build-time error: this returns `Ok` with a slice that omits it,
/// leaving a late-bound `GET_GLOBAL` that resolves against the isolate's own
/// globals/builtins or raises the standard recoverable `undefined variable '<name>'`
/// panic at run time. It is never a wrong or silently-partial result — an
/// unsatisfiable dependency fails loudly at isolate runtime.
pub fn build_code_slice(
    top: &Chunk,
    entry_name: &str,
    class_name: Option<Rc<str>>,
) -> Result<WorkerCodeSlice, Control> {
    let defs = top_level_defs(top);

    // The entry must be a top-level worker fn.
    let entry_def = defs.get(entry_name).cloned().ok_or_else(|| {
        Control::Panic(crate::error::AsError::new(format!(
            "worker entry '{entry_name}' is not a top-level function"
        )))
    })?;
    let TopDef::Fn(entry_proto) = entry_def else {
        return Err(Control::Panic(crate::error::AsError::new(format!(
            "worker entry '{entry_name}' is not a function"
        ))));
    };

    // The top-level entry is itself one of the closure's emitted members (it has a
    // top-level DEFINE_GLOBAL), so it does NOT need to be emitted separately.
    materialize_slice(top, &defs, entry_name, &entry_proto, class_name, false)
}

// ---------------------------------------------------------------------------
// Workers Spec B — CLASS code slice (actor support).
//
// An actor needs the FULL class — superclass chain + method table + field
// defaults — shipped into its dedicated isolate so the isolate can construct the
// instance and run methods locally. `build_code_slice` ships only top-level `fn`s
// and literal `const`s; this builds the analogous "module fragment" for a
// `worker class`: it copies the class's `Op::Class` instruction group (the
// contiguous `Op::Closure` run for default thunks + methods + statics, then
// `Op::Class`, then `DEFINE_GLOBAL`), remapping the proto/const/class-proto
// indices into the fragment, plus the transitive top-level fn/const dependency
// closure of every method body, plus any superclass classes (recursively), plus
// any OTHER top-level `class`/`enum` a method constructs or references — shipped
// via the SAME `emit_closure_classes` machinery the worker-fn slice uses, fully
// transitively (a shipped class whose own method constructs yet another class
// pulls that one in too). So an actor method can construct/reference any top-level
// class or enum, identical to a `worker fn` body.
//
// SUPPORTED: a DIRECT-child top-level `worker class` whose methods reference only
// globals (`GET_GLOBAL`) + their own params/`self` — i.e. no enclosing-frame
// upvalue captures (the normal case for a top-level class). UNSUPPORTED cases
// (a non-top-level worker class, or method/default closures that capture an
// enclosing LOCAL via an upvalue) are reported as a recoverable `Control::Panic`
// at slice-build time — never a wrong or silently-partial result.
// ---------------------------------------------------------------------------

/// One copied member of a class group, in stack order below `Op::Class`.
enum GroupMember {
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
fn locate_class_group(
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

/// Build the shippable [`WorkerCodeSlice`] for a `worker class` named `class_name`.
/// The fragment, when loaded on a fresh isolate `Vm`, defines: every superclass
/// (recursively), every OTHER top-level `class`/`enum` the method bodies construct or
/// reference (transitively, via [`emit_closure_classes`] — the same machinery the
/// worker-fn slice uses), the transitive top-level fn/const deps of all method bodies,
/// and the class itself (as a top-level `DEFINE_GLOBAL <class_name>`). The actor then
/// constructs the instance by looking up the class global and calling its `init`.
///
/// `fn_id`/`entry_name` are set to the class name (the actor's `ActorMsg::Init`
/// fetches the class global by `entry_name`).
pub fn build_class_slice(top: &Chunk, class_name: &str) -> Result<WorkerCodeSlice, Control> {
    let defs = top_level_defs(top);

    let mut frag = Chunk::new();
    frag.name = Some("<worker-class-slice>".to_string());
    let span = Span::new(0, 0);

    // Emit the actor's OWN class (superclasses first, then the target) and collect the
    // union of its method bodies' GET_GLOBAL references for the dependency closure.
    let mut emitted_classes: HashSet<String> = HashSet::new();
    let mut method_refs: Vec<Rc<str>> = Vec::new();
    emit_class_recursive(
        top,
        class_name,
        &mut frag,
        &mut emitted_classes,
        &mut method_refs,
        span,
    )?;

    // Compute the transitive top-level dependency closure of the actor's method-body
    // references (the SAME `compute_closure` fixpoint `materialize_slice` uses). It
    // contains every fn/const AND every CLASS/ENUM reached transitively — including a
    // class another method constructs, and THAT class's own method/superclass refs.
    let included = compute_closure(top, &defs, method_refs.clone());

    // Emit every OTHER top-level class the methods reference (the actor's own class is
    // already in `emitted_classes`, so the shared dedup set skips it), reusing the
    // SAME `emit_closure_classes` machinery the worker-fn slice uses. This is the fix
    // for the actor class-dep gap: an actor method can now construct/reference any
    // top-level class (+ its superclass chain, transitively). `emit_class_recursive`
    // appends those classes' own method refs into `method_refs` — already covered by
    // `included` above (the closure walked them via `collect_def_refs`).
    emit_closure_classes(
        top,
        &defs,
        &included,
        &mut emitted_classes,
        &mut method_refs,
        &mut frag,
        span,
    )?;

    // Emit the transitive fn / literal-const / computed-const deps (same closure),
    // BEFORE the classes would need them. They are late-bound `GET_GLOBAL`s, so the
    // deps/classes emission order does not matter for correctness; we emit deps first
    // for readability. `emit_dep_closure` skips `TopDef::Class` (emitted above).
    let mut frag_deps = Chunk::new();
    frag_deps.name = Some("<worker-class-deps>".to_string());
    emit_dep_closure(top, &defs, &method_refs, &mut frag_deps, span)?;

    // Splice: top-level imports first (so a method's `GET_GLOBAL` of an imported name
    // resolves), then fn/const deps, then classes. Rebuild one fragment in order.
    let mut whole = Chunk::new();
    whole.name = Some("<worker-class-slice>".to_string());
    emit_top_imports(top, &mut whole, span);
    append_chunk_defs(&mut whole, &frag_deps);
    append_chunk_defs(&mut whole, &frag);
    whole.emit(Op::Nil, span);
    whole.emit(Op::Return, span);

    if let Some(limit) = whole.take_overflow() {
        let ce = limit.into_compile_error();
        return Err(Control::Panic(crate::error::AsError::at(ce.message, ce.span)));
    }

    let bytes = whole.to_bytes().map_err(|e| {
        Control::Panic(crate::error::AsError::new(format!(
            "worker class slice could not be serialized: {e:?}"
        )))
    })?;

    Ok(WorkerCodeSlice {
        fn_id: entry_fn_id(class_name, None),
        entry_aso: Rc::from(bytes.into_boxed_slice()),
        class_name: Some(Rc::from(class_name)),
        entry_name: Rc::from(class_name),
    })
}


/// Emit `class_name`'s definition into `frag`, recursively emitting any superclass
/// first. Accumulates the union of all method/default GET_GLOBAL references into
/// `method_refs`. `emitted` de-dups classes (a diamond superclass is emitted once).
fn emit_class_recursive(
    top: &Chunk,
    class_name: &str,
    frag: &mut Chunk,
    emitted: &mut HashSet<String>,
    method_refs: &mut Vec<Rc<str>>,
    span: Span,
) -> Result<(), Control> {
    if !emitted.insert(class_name.to_string()) {
        return Ok(());
    }
    let (members, cp) = locate_class_group(top, class_name)?;

    // Emit any superclass FIRST (so its global is defined before this class's
    // `Op::Class` pops it). Also collect its members' refs.
    for m in &members {
        if let GroupMember::SuperGlobal(sup) = m {
            emit_class_recursive(top, sup, frag, emitted, method_refs, span)?;
        }
    }

    // Re-emit the group into the fragment: superclass push (GET_GLOBAL) + each
    // CLOSURE (proto copied into the fragment) + Op::Class (class proto copied) +
    // DEFINE_GLOBAL <class_name>.
    for m in &members {
        match m {
            GroupMember::SuperGlobal(sup) => {
                let name_idx = frag.add_const(Value::Str(sup.clone()));
                frag.emit_u16(Op::GetGlobal, name_idx, span);
            }
            GroupMember::Closure(proto) => {
                // Collect this member's transitive top-level refs for the dep closure.
                collect_get_global_names(&proto.chunk, method_refs);
                let proto_idx = frag.add_proto(proto.clone());
                frag.emit_u16(Op::Closure, proto_idx, span);
            }
        }
    }
    let cp_idx = frag.add_class_proto(cp);
    frag.emit_u16(Op::Class, cp_idx, span);
    let name_idx = frag.add_const(Value::Str(Rc::from(class_name)));
    frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
    Ok(())
}

/// Re-emit every top-level `Op::Import` into `frag`. A `worker class` method may
/// reference an imported binding (e.g. `import { open } from "std/sqlite"`); the
/// import statement is NOT a fn/const dep (so the closure misses it) and is left
/// late-bound. Shipping all top-level imports makes those names resolve on the
/// isolate (std imports are side-effect-free; a file import re-runs its module on the
/// isolate, the shared-nothing analog of a fresh process).
fn emit_top_imports(top: &Chunk, frag: &mut Chunk, span: Span) {
    let mut ip = 0usize;
    while ip < top.code.len() {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        if op == Op::Import {
            let idx = top.read_u16(ip + 1) as usize;
            if let Some(desc) = top.imports.get(idx) {
                let new_idx = frag.add_import(desc.clone());
                frag.emit_u16(Op::Import, new_idx, span);
            }
        }
        ip += 1 + width;
    }
}

/// Emit the transitive top-level fn / literal-const / computed-const dependency
/// closure of `roots` into `frag`, in original source order (used by the actor
/// class-slice for the dep closure of all method bodies). Shares the
/// [`compute_closure`] fixpoint with `materialize_slice`. Classes/enums referenced by
/// a method body are emitted by the class slice's [`emit_closure_classes`] pass, not
/// here (so a `TopDef::Class` in the closure is skipped at this emit site; enums are
/// `TopDef::Const` and DO ship here as values).
fn emit_dep_closure(
    top: &Chunk,
    defs: &HashMap<Rc<str>, TopDef>,
    roots: &[Rc<str>],
    frag: &mut Chunk,
    span: Span,
) -> Result<(), Control> {
    let included = compute_closure(top, defs, roots.to_vec());
    for name in source_order_define_names(top) {
        if !included.contains(&name) {
            continue;
        }
        match defs.get(name.as_ref()) {
            Some(TopDef::Const(v)) => {
                emit_const_load(frag, v.clone(), span);
                let name_idx = frag.add_const(Value::Str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::Fn(proto)) => {
                let proto_idx = frag.add_proto(proto.clone());
                frag.emit_u16(Op::Closure, proto_idx, span);
                let name_idx = frag.add_const(Value::Str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::ComputedConst { start, end }) => {
                copy_code_range(frag, top, *start, *end, span);
                let name_idx = frag.add_const(Value::Str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::Interface(proto)) => {
                emit_interface_def(frag, proto.clone(), &name, span);
            }
            Some(TopDef::Class) | None => {}
        }
    }
    Ok(())
}

/// IFACE: re-emit an interface descriptor into a worker fragment — add its proto to
/// the fragment's `interface_protos`, emit `Op::DefineInterface` (which rebuilds a
/// fresh `InterfaceDef` `Rc` on the isolate, §5.3), then bind it as a module-global
/// (`DEFINE_GLOBAL`). The `extends` names reload as module-globals (already shipped
/// transitively via [`collect_def_refs`]); the lazy flatten resolves them on first use.
fn emit_interface_def(frag: &mut Chunk, proto: Rc<InterfaceProto>, name: &Rc<str>, span: Span) {
    let idx = frag.add_interface_proto(proto);
    frag.emit_u16(Op::DefineInterface, idx, span);
    let name_idx = frag.add_const(Value::Str(name.clone()));
    frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
}

/// Emit every `class` in the dependency-closure `included` set into `frag`, in
/// original source order, superclass-first (via [`emit_class_recursive`], which
/// de-dups). SHARED by the worker-fn slice ([`materialize_slice`] step 2) and the
/// actor class-slice ([`build_class_slice`]) so both ship referenced classes
/// identically: a worker fn / actor method that constructs or references any
/// top-level class pulls that class (+ its superclass chain) into the fragment.
///
/// The closure `included` already contains every transitively-referenced class name
/// (`compute_closure` walks a `TopDef::Class`'s own method/superclass refs via
/// [`collect_def_refs`]), so a class whose method constructs ANOTHER top-level class
/// is reached and emitted here too — fully transitive. The `method_refs`
/// [`emit_class_recursive`] re-accumulates are therefore already in `included` and are
/// discarded.
fn emit_closure_classes(
    top: &Chunk,
    defs: &HashMap<Rc<str>, TopDef>,
    included: &HashSet<Rc<str>>,
    emitted: &mut HashSet<String>,
    method_refs: &mut Vec<Rc<str>>,
    frag: &mut Chunk,
    span: Span,
) -> Result<(), Control> {
    for name in source_order_define_names(top) {
        if included.contains(&name) && matches!(defs.get(name.as_ref()), Some(TopDef::Class)) {
            emit_class_recursive(top, &name, frag, emitted, method_refs, span)?;
        }
    }
    Ok(())
}

/// Copy the top-level instruction byte-range `[start, end)` of `top.code` verbatim
/// into `frag`, remapping every pool-indexing operand (const/name-const/proto/
/// class-proto/import) into `frag`'s own pools. Used to ship a computed-`const`
/// initializer (`TopDef::ComputedConst`) so the isolate RE-RUNS it and recomputes the
/// value.
///
/// JUMP/`Loop` displacements are RELATIVE (measured from the byte after the operand),
/// so copying the WHOLE range contiguously at the same relative positions preserves
/// them: a computed-const initializer is one self-contained expression whose only
/// jumps — ternary `?:` and short-circuit `&&`/`||` — target WITHIN its own range, so
/// no displacement escapes the copied span. (The bounded-range invariant is enforced
/// upstream by `top_level_defs`, which sets the range to exactly the const's own
/// statement.)
///
/// The op→pool classification below mirrors `verify::check_operands` (the
/// authoritative operand table). A leading-u16 op that names NO pool — local/upvalue
/// slots, jump displacements, and inline counts/argc — is copied byte-for-byte
/// unchanged. The catch-all `debug_assert!`s (via `op_indexes_pool`) that no
/// pool-indexing op slips through unremapped, so a FUTURE opcode that adds a pool
/// index fails loudly here instead of silently shipping a stale index.
fn copy_code_range(frag: &mut Chunk, top: &Chunk, start: usize, end: usize, span: Span) {
    let mut ip = start;
    while ip < end {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        match op {
            // ---- const pool (value or name-const) leading u16 ----
            // Width-2 forms re-emit as a plain u16; the width-3 forms (CALL_METHOD =
            // name+argc, DEFINE_GLOBAL = name+mutability) carry a trailing u8 to copy.
            Op::Const
            | Op::GetGlobal
            | Op::SetGlobal
            | Op::ImmutableError
            | Op::GetProp
            | Op::SetProp
            | Op::GetPropOpt
            | Op::Method
            | Op::GetSuper
            | Op::ObjectKey
            | Op::MatchHasKey
            | Op::CallMethodSpread
            | Op::DefineExport
            | Op::ObjectRest => {
                let v = top.consts[top.read_u16(ip + 1) as usize].clone();
                let new_idx = frag.add_const(v);
                frag.emit_u16(op, new_idx, span);
            }
            Op::DefineGlobal | Op::CallMethod => {
                // u16 const index + trailing u8 (mutability flag / argc).
                let v = top.consts[top.read_u16(ip + 1) as usize].clone();
                let new_idx = frag.add_const(v);
                let trailing = top.read_u8(ip + 3);
                frag.emit_u16_u8(op, new_idx, trailing, span);
            }
            Op::Closure => {
                let p = top.protos[top.read_u16(ip + 1) as usize].clone();
                let new_idx = frag.add_proto(p);
                frag.emit_u16(op, new_idx, span);
            }
            Op::Class => {
                let cp = top.class_protos[top.read_u16(ip + 1) as usize].clone();
                let new_idx = frag.add_class_proto(cp);
                frag.emit_u16(op, new_idx, span);
            }
            Op::Import => {
                let desc = top.imports[top.read_u16(ip + 1) as usize].clone();
                let new_idx = frag.add_import(desc);
                frag.emit_u16(op, new_idx, span);
            }
            // ---- no pool reference: copy the op + its operand bytes verbatim ----
            // (slots, upvalues, jump displacements — RELATIVE, preserved by a
            // contiguous copy — and counts/argc on zero-pool ops.)
            _ => {
                // Fail-loud: any op that DOES carry a pool index must have a remap arm
                // above. If this trips, an opcode added a pool operand without a
                // matching `copy_code_range` arm — the verbatim copy would ship a stale
                // index. Keep this in sync with `verify::check_operands`.
                debug_assert!(
                    !op_indexes_pool(op),
                    "copy_code_range: pool-indexing op {op:?} reached the verbatim \
                     catch-all — add a remap arm (see verify::check_operands)"
                );
                frag.emit_raw(op, &top.code[ip + 1..ip + 1 + width], span);
            }
        }
        ip += 1 + width;
    }
}

/// Whether `op`'s leading u16 operand indexes one of the chunk's POOLS (const /
/// proto / class-proto / import) and therefore MUST be remapped when relocated into a
/// fresh chunk. This is the exact set of pool-indexing ops in `verify::check_operands`
/// (the authoritative operand table); [`copy_code_range`] uses it as a fail-loud guard
/// so a future pool-carrying opcode cannot be silently copied with a stale index.
fn op_indexes_pool(op: Op) -> bool {
    matches!(
        op,
        // const pool (value or name-const)
        Op::Const
            | Op::GetGlobal
            | Op::DefineGlobal
            | Op::SetGlobal
            | Op::ImmutableError
            | Op::GetProp
            | Op::SetProp
            | Op::GetPropOpt
            | Op::Method
            | Op::GetSuper
            | Op::ObjectKey
            | Op::MatchHasKey
            | Op::CallMethodSpread
            | Op::DefineExport
            | Op::ObjectRest
            | Op::CallMethod
            // proto / class-proto / import pools
            | Op::Closure
            | Op::Class
            | Op::Import
    )
}

/// Append `src`'s definition instructions (everything before its trailing
/// `Nil; Return`, if any) into `dst`, remapping const/proto/class-proto indices.
/// Used to splice the dep fragment and the class fragment into one ordered chunk.
fn append_chunk_defs(dst: &mut Chunk, src: &Chunk) {
    // Map src indices → dst indices as we copy referenced pool entries.
    let mut ip = 0usize;
    let span = Span::new(0, 0);
    while ip < src.code.len() {
        let Some(op) = Op::from_u8(src.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        match op {
            Op::Nil if ip + 1 < src.code.len() && Op::from_u8(src.code[ip + 1]) == Some(Op::Return) => {
                // The trailing `Nil; Return` terminator — skip it (dst adds its own).
                break;
            }
            Op::Const => {
                let v = src.consts[src.read_u16(ip + 1) as usize].clone();
                let idx = dst.add_const(v);
                dst.emit_u16(Op::Const, idx, span);
            }
            Op::Closure => {
                let p = src.protos[src.read_u16(ip + 1) as usize].clone();
                let idx = dst.add_proto(p);
                dst.emit_u16(Op::Closure, idx, span);
            }
            Op::Class => {
                let cp = src.class_protos[src.read_u16(ip + 1) as usize].clone();
                let idx = dst.add_class_proto(cp);
                dst.emit_u16(Op::Class, idx, span);
            }
            Op::GetGlobal => {
                let v = src.consts[src.read_u16(ip + 1) as usize].clone();
                let idx = dst.add_const(v);
                dst.emit_u16(Op::GetGlobal, idx, span);
            }
            Op::DefineGlobal => {
                let v = src.consts[src.read_u16(ip + 1) as usize].clone();
                let idx = dst.add_const(v);
                let mutable = src.code[ip + 3];
                dst.emit_u16_u8(Op::DefineGlobal, idx, mutable, span);
            }
            Op::Nil => dst.emit(Op::Nil, span),
            Op::True => dst.emit(Op::True, span),
            Op::False => dst.emit(Op::False, span),
            other => {
                // Defensive: the dep/class fragments only ever emit the ops handled
                // above. Anything else is a builder bug — skip it (it would only
                // appear if a future change emits a new op into these fragments).
                debug_assert!(false, "append_chunk_defs: unexpected op {other:?}");
            }
        }
        ip += 1 + width;
    }
}

/// Build a [`WorkerCodeSlice`] for a `static worker fn` (Spec A): the entry is a
/// static METHOD body (no `self`; it may reference top-level fns/consts + its own
/// params), located in the compiled program by `(class_name, method_name)`. The
/// method's compiled `FnProto` becomes the slice's entry fn — emitted as a top-level
/// `CLOSURE; DEFINE_GLOBAL <method_name>` in the fragment — and its transitive
/// top-level dependency closure ships exactly as for a free `worker fn`. `fn_id`
/// folds in `class_name`, so two same-named static workers on different classes get
/// distinct per-isolate cache keys.
pub fn build_code_slice_for_static_method(
    top: &Chunk,
    class_name: &str,
    method_name: &str,
) -> Result<WorkerCodeSlice, Control> {
    let defs = top_level_defs(top);
    let entry_proto = find_static_method_proto(top, class_name, method_name).ok_or_else(|| {
        Control::Panic(crate::error::AsError::new(format!(
            "static worker '{class_name}.{method_name}' could not be located in the program"
        )))
    })?;
    // The static method is NOT a top-level DEFINE_GLOBAL, so emit it explicitly as the
    // entry member (emit_entry = true) named by the method name.
    materialize_slice(
        top,
        &defs,
        method_name,
        &entry_proto,
        Some(Rc::from(class_name)),
        true,
    )
}

/// Locate the compiled `FnProto` of a static method `class_name.method_name` in the
/// top-level chunk. Static-method closures are emitted (in declaration order) right
/// before each `Op::Class`, after the default-field thunks and the instance methods:
/// `[super?, ..thunks.., ..methods.., ..statics..]`. We track the rolling run of
/// `Op::Closure` proto-indices and, at the matching `Op::Class`, index into the
/// STATIC tail by the method's position in `static_method_names`.
fn find_static_method_proto(
    top: &Chunk,
    class_name: &str,
    method_name: &str,
) -> Option<Rc<FnProto>> {
    // `closures` accumulates the proto-indices of a CONTIGUOUS run of `Op::Closure`
    // ops. A class group emits its thunk + method + static closures as one such
    // uninterrupted run ending at `Op::Class`; any other op (e.g. the `DEFINE_GLOBAL`
    // after a top-level `fn`'s `CLOSURE`) breaks the run and clears it.
    let mut closures: Vec<u16> = Vec::new();
    let mut ip = 0usize;
    while ip < top.code.len() {
        let op = Op::from_u8(top.code[ip])?;
        let width = op.operand_width();
        if op == Op::Closure {
            closures.push(top.read_u16(ip + 1));
        } else if op == Op::Class {
            let cp_idx = top.read_u16(ip + 1) as usize;
            if let Some(cp) = top.class_protos.get(cp_idx) {
                if cp.class.name == class_name {
                    if let Some(pos) = cp.static_method_names.iter().position(|n| n == method_name) {
                        // The static run is the LAST `static_method_names.len()`
                        // closures of this class group; thunks + instance methods
                        // precede them in the same contiguous run.
                        let n_static = cp.static_method_names.len();
                        let static_start = closures.len().checked_sub(n_static)?;
                        let proto_idx = *closures.get(static_start + pos)? as usize;
                        return top.protos.get(proto_idx).cloned();
                    }
                }
            }
            closures.clear();
        } else {
            closures.clear();
        }
        ip += 1 + width;
    }
    None
}

/// Shared fragment builder for both the top-level and static-method slice paths.
/// Computes the entry's transitive top-level dependency closure and materializes a
/// self-contained "module fragment" chunk. When `emit_entry` is true the entry fn is
/// appended as an explicit `CLOSURE; DEFINE_GLOBAL <entry_name>` (used for a static
/// method, which has no top-level DEFINE_GLOBAL of its own); when false the entry is
/// already among the top-level members emitted in source order.
fn materialize_slice(
    top: &Chunk,
    defs: &HashMap<Rc<str>, TopDef>,
    entry_name: &str,
    entry_proto: &Rc<FnProto>,
    class_name: Option<Rc<str>>,
    emit_entry: bool,
) -> Result<WorkerCodeSlice, Control> {
    // Seed the transitive closure with the entry proto's GET_GLOBAL refs (and, for a
    // top-level entry, the entry NAME so the source walk emits it). `compute_closure`
    // pulls in every referenced top-level fn, literal/computed const, and class.
    let mut roots: Vec<Rc<str>> = Vec::new();
    collect_get_global_names(&entry_proto.chunk, &mut roots);
    if !emit_entry {
        roots.push(Rc::from(entry_name));
    }
    let included = compute_closure(top, defs, roots);

    let span = Span::new(0, 0);
    let mut frag = Chunk::new();
    frag.name = Some("<worker-slice>".to_string());

    // 1) Top-level imports first (so a body/initializer GET_GLOBAL of an imported
    //    module name resolves on the isolate). Same machinery as the actor path.
    emit_top_imports(top, &mut frag, span);

    // 2) Classes (with superclass chains) the closure references — emitted before the
    //    fn/const deps so a computed-const initializer that constructs a class finds
    //    it defined. `emit_class_recursive` de-dups and orders superclasses first.
    {
        // Emit classes into their own fragment, then splice (the class fragment uses
        // fresh pool indices remapped by `append_chunk_defs`).
        let mut class_frag = Chunk::new();
        class_frag.name = Some("<worker-slice-classes>".to_string());
        let mut emitted_classes: HashSet<String> = HashSet::new();
        let mut class_method_refs: Vec<Rc<str>> = Vec::new();
        emit_closure_classes(
            top,
            defs,
            &included,
            &mut emitted_classes,
            &mut class_method_refs,
            &mut class_frag,
            span,
        )?;
        append_chunk_defs(&mut frag, &class_frag);
    }

    // 3) The fn / literal-const / computed-const deps, in original source order. (A
    //    class's method bodies reference top-level fns/consts via late-bound
    //    GET_GLOBAL, so emitting these after the classes is fine — the references
    //    resolve at call time, not class-definition time.)
    for name in source_order_define_names(top) {
        if !included.contains(&name) {
            continue;
        }
        match defs.get(name.as_ref()) {
            Some(TopDef::Const(v)) => {
                emit_const_load(&mut frag, v.clone(), span);
                let name_idx = frag.add_const(Value::Str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::Fn(proto)) => {
                let proto_idx = frag.add_proto(proto.clone());
                frag.emit_u16(Op::Closure, proto_idx, span);
                let name_idx = frag.add_const(Value::Str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::ComputedConst { start, end }) => {
                // Copy the initializer instruction range verbatim (pool-remapped), then
                // bind the result with DEFINE_GLOBAL. The range excludes the trailing
                // DEFINE_GLOBAL, so emit it here.
                copy_code_range(&mut frag, top, *start, *end, span);
                let name_idx = frag.add_const(Value::Str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::Interface(proto)) => {
                emit_interface_def(&mut frag, proto.clone(), &name, span);
            }
            // Classes were emitted in step 2; anything else is left late-bound.
            Some(TopDef::Class) | None => {}
        }
    }

    // 4) For a static method, the entry has no top-level DEFINE_GLOBAL; emit it last.
    if emit_entry {
        let proto_idx = frag.add_proto(entry_proto.clone());
        frag.emit_u16(Op::Closure, proto_idx, span);
        let name_idx = frag.add_const(Value::Str(Rc::from(entry_name)));
        frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
    }

    frag.emit(Op::Nil, span);
    frag.emit(Op::Return, span);

    if let Some(limit) = frag.take_overflow() {
        let ce = limit.into_compile_error();
        return Err(Control::Panic(crate::error::AsError::at(ce.message, ce.span)));
    }

    let bytes = frag.to_bytes().map_err(|e| {
        Control::Panic(crate::error::AsError::new(format!(
            "worker code slice could not be serialized: {e:?}"
        )))
    })?;

    Ok(WorkerCodeSlice {
        fn_id: entry_fn_id(entry_name, class_name.as_deref()),
        entry_aso: Rc::from(bytes.into_boxed_slice()),
        class_name,
        entry_name: Rc::from(entry_name),
    })
}

/// Collect every top-level `DEFINE_GLOBAL` NAME in original source order. The emit
/// walks reuse this so closure members materialize in declaration order (a dep that
/// reads an earlier dep sees it already defined).
fn source_order_define_names(top: &Chunk) -> Vec<Rc<str>> {
    let mut names = Vec::new();
    let mut ip = 0usize;
    while ip < top.code.len() {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        if op == Op::DefineGlobal {
            let idx = top.read_u16(ip + 1) as usize;
            if let Some(Value::Str(name)) = top.consts.get(idx) {
                names.push(name.clone());
            }
        }
        ip += 1 + width;
    }
    names
}

/// Build a [`WorkerCodeSlice`] for the worker entry named `entry_name` by recompiling
/// the entry program's source (retained on the [`crate::interp::Interp`] at run time
/// — see `Interp::set_worker_source`). This is the SINGLE slice path shared by BOTH
/// engines: the tree-walker has no compiled chunk of its own, so it (like the VM)
/// recompiles the source here and ships the resulting `.aso` fragment to the isolate,
/// whose own VM runs it — guaranteeing byte-identical worker behavior across engines.
///
/// The recompiled slice is keyed by `fn_id` and cached per-isolate (Task 8), so the
/// per-dispatch recompile cost is paid at most once per distinct worker entry; the
/// caller [`crate::worker::dispatch_worker`] does the encode/transport.
///
/// Returns a recoverable `Control::Panic` when no source is recorded (an embedder
/// that drove the engine without `set_worker_source`) or the entry is missing / not a
/// top-level function (mirrors [`build_code_slice`]).
pub fn build_code_slice_from_source(
    interp: &crate::interp::Interp,
    entry_name: &str,
    class_name: Option<Rc<str>>,
) -> Result<WorkerCodeSlice, Control> {
    let src = interp.worker_source().ok_or_else(|| {
        Control::Panic(crate::error::AsError::new(format!(
            "cannot dispatch worker '{entry_name}': the program source is unavailable \
             (worker fns require running via `ascript run`)"
        )))
    })?;
    let top = crate::compile::compile_source(&src).map_err(|e| {
        Control::Panic(crate::error::AsError::at(e.message, e.span))
    })?;
    build_code_slice(&top, entry_name, class_name)
}

/// Like [`build_code_slice_from_source`] but for a `static worker fn` (Spec A): builds
/// the slice from the static method `class_name.method_name` (see
/// [`build_code_slice_for_static_method`]). Shared by both engines.
pub fn build_code_slice_for_static_method_from_source(
    interp: &crate::interp::Interp,
    class_name: &str,
    method_name: &str,
) -> Result<WorkerCodeSlice, Control> {
    let src = interp.worker_source().ok_or_else(|| {
        Control::Panic(crate::error::AsError::new(format!(
            "cannot dispatch worker '{class_name}.{method_name}': the program source is \
             unavailable (worker fns require running via `ascript run`)"
        )))
    })?;
    let top = crate::compile::compile_source(&src)
        .map_err(|e| Control::Panic(crate::error::AsError::at(e.message, e.span)))?;
    build_code_slice_for_static_method(&top, class_name, method_name)
}

/// Resolve the top-level program [`Chunk`] for a worker slice build, mirroring the
/// source-vs-`.aso`-bytes branch in [`crate::vm::Vm::dispatch_worker_closure`]
/// (Plan A Task 15). Prefers recompiling the retained program SOURCE (the path
/// shared by both engines for any run-from-source mode); falls back to re-parsing
/// the stored `.aso` bytes (`Interp::worker_aso_bytes`, set by `run_aso_file`) when
/// no source is recorded — the 4th execution mode (`ascript run x.aso`). Returns a
/// recoverable `Control::Panic` (never `.expect`/`panic!`) when neither is available
/// or the `.aso` is malformed. `what` describes the entity for the diagnostic
/// (e.g. `"worker class 'C'"`).
fn resolve_worker_top_chunk(
    interp: &crate::interp::Interp,
    what: &str,
    require_kind: &str,
) -> Result<Chunk, Control> {
    if let Some(src) = interp.worker_source() {
        return crate::compile::compile_source(&src)
            .map_err(|e| Control::Panic(crate::error::AsError::at(e.message, e.span)));
    }
    if let Some(raw) = interp.worker_aso_bytes() {
        return Chunk::from_bytes(&raw).map_err(|e| {
            Control::Panic(crate::error::AsError::new(format!(
                "cannot re-parse .aso for {what}: {e:?}"
            )))
        });
    }
    Err(Control::Panic(crate::error::AsError::new(format!(
        "cannot dispatch {what}: the program source is unavailable \
         ({require_kind} require running via `ascript run` or a compiled `.aso`)"
    ))))
}

/// Build the `worker class` slice for `class_name`, resolving the top-level chunk
/// from either retained source (run-from-source) or the stored `.aso` bytes
/// (`ascript run x.aso`) via [`resolve_worker_top_chunk`] — the `.aso`-mode
/// fallback (Plan A Task 15 mechanism extended to actor spawn).
pub fn build_class_slice_for_interp(
    interp: &crate::interp::Interp,
    class_name: &str,
) -> Result<WorkerCodeSlice, Control> {
    let top = resolve_worker_top_chunk(
        interp,
        &format!("worker class '{class_name}'"),
        "worker classes",
    )?;
    build_class_slice(&top, class_name)
}

/// Build a plain `worker fn` slice for `entry_name`, resolving the top-level chunk
/// from either retained source (run-from-source) or the stored `.aso` bytes
/// (`ascript run x.aso`) via [`resolve_worker_top_chunk`]. Mirrors
/// [`build_code_slice_from_source`] but adds the `.aso`-mode fallback — required by
/// the DEDICATED-isolate `run_in_worker({caps})` path, which (unlike a bare
/// `worker fn` call) is the SAME shared method on both engines and so must work in
/// the 4th execution mode (`ascript run x.aso`) too, not just run-from-source.
pub fn build_code_slice_for_interp(
    interp: &crate::interp::Interp,
    entry_name: &str,
) -> Result<WorkerCodeSlice, Control> {
    let top = resolve_worker_top_chunk(
        interp,
        &format!("worker fn '{entry_name}'"),
        "worker fns",
    )?;
    build_code_slice(&top, entry_name, None)
}

/// Build the `worker fn*` stream slice for `entry_name`, resolving the top-level
/// chunk from either retained source or the stored `.aso` bytes. Mirrors
/// [`build_code_slice_from_source`] but adds the `.aso`-mode fallback (Plan A
/// Task 15 mechanism extended to the worker-generator stream path).
pub fn build_stream_slice_for_interp(
    interp: &crate::interp::Interp,
    entry_name: &str,
) -> Result<WorkerCodeSlice, Control> {
    let top = resolve_worker_top_chunk(
        interp,
        &format!("worker fn* '{entry_name}'"),
        "worker generators",
    )?;
    build_code_slice(&top, entry_name, None)
}

/// Emit a value-producing instruction that pushes `v` onto the stack. Literal
/// scalars use their dedicated ops where available (matching the compiler) and
/// otherwise pool the value as a `CONST`. Only literal kinds reach here (a
/// [`TopDef::Const`] is built from a literal-producing op), so the `CONST` fallback
/// stays inside the `.aso` literal-only pool invariant.
fn emit_const_load(frag: &mut Chunk, v: Value, span: Span) {
    match v {
        Value::Nil => frag.emit(Op::Nil, span),
        Value::Bool(true) => frag.emit(Op::True, span),
        Value::Bool(false) => frag.emit(Op::False, span),
        other => {
            let idx = frag.add_const(other);
            frag.emit_u16(Op::Const, idx, span);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Interp;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    /// Extension methods the plan's tests use on a built slice.
    impl WorkerCodeSlice {
        /// The set of top-level dependency NAMES the slice ships (the closure
        /// members materialized into the fragment), reconstructed from the fragment
        /// `.aso` — exactly what a fresh isolate would define on load. Test-only.
        pub fn dep_names(&self) -> HashSet<String> {
            let chunk = Chunk::from_bytes(&self.entry_aso).expect("slice .aso decodes");
            let mut names = HashSet::new();
            let mut ip = 0usize;
            while ip < chunk.code.len() {
                let Some(op) = Op::from_u8(chunk.code[ip]) else {
                    break;
                };
                let width = op.operand_width();
                if op == Op::DefineGlobal {
                    let idx = chunk.read_u16(ip + 1) as usize;
                    if let Some(Value::Str(name)) = chunk.consts.get(idx) {
                        names.insert(name.to_string());
                    }
                }
                ip += 1 + width;
            }
            names
        }
    }

    /// Compile `src`, find the top-level `worker fn` named `entry_name`, and build
    /// its code slice. Async only to mirror the plan's test signatures and the Task
    /// 8 dispatch path; the build itself is synchronous.
    async fn build_slice_for_test(src: &str, entry_name: &str) -> WorkerCodeSlice {
        let top = crate::compile::compile_source(src).expect("compiles");
        build_code_slice(&top, entry_name, None).expect("slice builds")
    }

    /// Load the slice's fragment `.aso` into a FRESH `Interp`/`Vm` (no access to the
    /// original heap), run it to define the closure globals, then fetch the entry
    /// global and call it with `args` — the synchronous in-process analog of the
    /// Task 8 isolate run loop, validating that the shipped bytecode is complete and
    /// runnable in isolation.
    async fn run_slice_in_fresh_isolate(
        slice: &WorkerCodeSlice,
        entry_name: &str,
        args: Vec<Value>,
    ) -> Result<Value, Control> {
        let chunk = Chunk::from_bytes(&slice.entry_aso).expect("slice .aso decodes");
        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
            debug_name: None,
        });
        let closure = Closure::new(proto);

        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::new(interp.clone());

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Run the fragment top-level to define the closure globals.
                let mut fiber = crate::vm::fiber::Fiber::new(closure);
                match vm.run(&mut fiber).await? {
                    RunOutcome::Done(_) => {}
                    RunOutcome::Yielded(_) => unreachable!("fragment top-level cannot yield"),
                }
                // Fetch the entry and call it — no original-heap access anywhere.
                let entry = vm
                    .user_global(entry_name)
                    .expect("entry global defined by the fragment");
                vm.call_value(entry, args, Span::new(0, 0)).await
            })
            .await
    }

    // worker fn `g` calls top-level `helper` and reads top-level const `K`;
    // the code slice must include g, helper, and K (transitively), but NOT an
    // unrelated top-level fn `other`.
    const SRC: &str = "
        const K = 10
        fn helper(x) { return x + K }
        fn other() { return 999 }
        worker fn g(n) { return helper(n) }
    ";

    #[tokio::test]
    async fn code_slice_includes_transitive_deps_only() {
        let slice = build_slice_for_test(SRC, "g").await;
        let names = slice.dep_names();
        assert!(names.contains("g"), "missing g: {names:?}");
        assert!(names.contains("helper"), "missing helper: {names:?}");
        assert!(names.contains("K"), "missing K: {names:?}");
        assert!(!names.contains("other"), "should not ship other: {names:?}");
    }

    #[tokio::test]
    async fn slice_aso_roundtrips_and_runs() {
        // The shipped bytecode (entry_aso) deserializes via the .aso reader and
        // runs g(5) -> 15 on a FRESH interp/vm (no access to the original heap).
        let slice = build_slice_for_test(SRC, "g").await;
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::Float(5.0)]).await;
        assert_eq!(out.unwrap(), Value::Float(15.0));
    }

    #[tokio::test]
    async fn slice_excludes_unrelated_const() {
        // A second unrelated const must not be shipped either.
        let src = "
            const K = 10
            const UNUSED = 42
            fn helper(x) { return x + K }
            worker fn g(n) { return helper(n) }
        ";
        let slice = build_slice_for_test(src, "g").await;
        let names = slice.dep_names();
        assert!(names.contains("K"));
        assert!(!names.contains("UNUSED"), "shipped unused const: {names:?}");
    }

    #[tokio::test]
    async fn slice_ships_computed_const_and_recomputes_it() {
        // A computed-initializer const (`K = expensive()`) is shipped as its
        // initializer CODE + the helper it calls, recomputed on the fresh isolate.
        let src = "
            fn expensive() { return 21 * 2 }
            const K = expensive()
            worker fn g(n) { return K + n }
        ";
        let slice = build_slice_for_test(src, "g").await;
        let names = slice.dep_names();
        assert!(names.contains("K"), "missing computed const K: {names:?}");
        assert!(names.contains("expensive"), "missing helper: {names:?}");
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::Float(8.0)]).await;
        assert_eq!(out.unwrap(), Value::Float(50.0));
    }

    #[tokio::test]
    async fn slice_excludes_unreferenced_computed_const() {
        // A computed const the worker never references is not shipped.
        let src = "
            fn expensive() { return 99 }
            const UNUSED = expensive()
            worker fn g(n) { return n + 1 }
        ";
        let slice = build_slice_for_test(src, "g").await;
        let names = slice.dep_names();
        assert!(!names.contains("UNUSED"), "shipped unused computed const: {names:?}");
        assert!(!names.contains("expensive"), "shipped unreachable helper: {names:?}");
    }

    #[tokio::test]
    async fn slice_ships_referenced_class_and_superclass() {
        // A worker fn constructing a subclass ships the class + its superclass chain.
        let src = "
            class Shape {
                kind: string
                fn init(k) { self.kind = k }
            }
            class Circle extends Shape {
                r: number
                fn init(r) { super.init(\"c\"); self.r = r }
            }
            class Unused { fn init() {} }
            worker fn g(r) { return Circle(r) }
        ";
        let slice = build_slice_for_test(src, "g").await;
        let names = slice.dep_names();
        assert!(names.contains("Circle"), "missing Circle: {names:?}");
        assert!(names.contains("Shape"), "missing superclass Shape: {names:?}");
        assert!(!names.contains("Unused"), "shipped unrelated class: {names:?}");
    }

    #[tokio::test]
    async fn slice_ships_referenced_interface() {
        // IFACE (Task 8): a worker fn doing `x instanceof Reader` ships Reader's
        // descriptor (and the class it constructs), but NOT an unrelated interface.
        let src = "
            interface Reader { fn read(b): int }
            interface Unused { fn frob() }
            class File {
                fn read(b) { return 0 }
            }
            worker fn g(n) {
                let f = File()
                return f instanceof Reader
            }
        ";
        let slice = build_slice_for_test(src, "g").await;
        let names = slice.dep_names();
        assert!(names.contains("Reader"), "missing Reader descriptor: {names:?}");
        assert!(names.contains("File"), "missing File class: {names:?}");
        assert!(!names.contains("Unused"), "shipped unrelated interface: {names:?}");
    }

    #[tokio::test]
    async fn slice_ships_transitive_extends_interfaces() {
        // IFACE (Task 8): a worker fn using a composed interface pulls in its
        // `extends` parents transitively (the lazy-flatten dependency edge, §4).
        let src = "
            interface Reader { fn read(b): int }
            interface Writer { fn write(b): int }
            interface ReadWriter extends Reader, Writer {}
            class Conn {
                fn read(b) { return 1 }
                fn write(b) { return 1 }
            }
            worker fn g(n) {
                let c = Conn()
                return c instanceof ReadWriter
            }
        ";
        let slice = build_slice_for_test(src, "g").await;
        let names = slice.dep_names();
        assert!(names.contains("ReadWriter"), "missing ReadWriter: {names:?}");
        assert!(names.contains("Reader"), "missing transitive Reader: {names:?}");
        assert!(names.contains("Writer"), "missing transitive Writer: {names:?}");
    }

    #[tokio::test]
    async fn slice_interface_instanceof_runs_in_fresh_isolate() {
        // IFACE (Task 8): the shipped interface descriptor is rebuilt on a fresh
        // isolate and `instanceof Interface` evaluates correctly across the boundary
        // (a conforming class -> true; a transitively-composed interface conforms too).
        let src = "
            interface Reader { fn read(b): int }
            interface Writer { fn write(b): int }
            interface ReadWriter extends Reader, Writer {}
            class Conn {
                fn read(b) { return 1 }
                fn write(b) { return 1 }
            }
            worker fn g(n) {
                let c = Conn()
                return [c instanceof Reader, c instanceof ReadWriter]
            }
        ";
        let slice = build_slice_for_test(src, "g").await;
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::Float(0.0)])
            .await
            .expect("runs");
        match out {
            Value::Array(arr) => {
                let a = arr.borrow();
                assert_eq!(a[0], Value::Bool(true), "Reader conformance across isolate");
                assert_eq!(a[1], Value::Bool(true), "ReadWriter conformance across isolate");
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn computed_const_range_is_bounded_to_its_own_initializer() {
        // A `for` loop and a bare call statement precede the computed const. The
        // const's slice range must be EXACTLY `expensive()` — NOT absorb the loop
        // (which would ship a backward Loop + GET_LOCAL and crash the isolate) nor the
        // `noisy()` call (which would over-ship `noisy` into the closure). The slice
        // must run cleanly in a fresh isolate AND not pull in `noisy`.
        let src = "
            fn noisy() { return 7 }
            fn expensive() { return 42 }
            noisy()
            for (i in 0..3) { i + 1 }
            const K = expensive()
            worker fn g(n) { return K + n }
        ";
        let slice = build_slice_for_test(src, "g").await;
        let names = slice.dep_names();
        assert!(names.contains("K"), "missing K: {names:?}");
        assert!(names.contains("expensive"), "missing expensive: {names:?}");
        assert!(
            !names.contains("noisy"),
            "over-shipped absorbed `noisy` into the slice: {names:?}"
        );
        // The slice runs to completion in a fresh isolate (no `set_local` slot panic).
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::Float(8.0)]).await;
        assert_eq!(out.unwrap(), Value::Float(50.0));
    }
}



