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
use crate::value::{Value, ValueKind};
use crate::vm::bcanalysis::{
    collect_get_global_names, compute_closure, locate_class_group, top_level_defs, GroupMember,
    TopDef,
};
use crate::vm::chunk::{Chunk, FnProto, InterfaceProto};
use crate::vm::opcode::Op;
use crate::worker::WorkerCodeSlice;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

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
        let (msg, span) = limit.into_message_span();
        return Err(Control::Panic(crate::error::AsError::at(msg, span)));
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
                let name_idx = frag.add_const(Value::str(sup.clone()));
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
    let name_idx = frag.add_const(Value::str(Rc::from(class_name)));
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
                let name_idx = frag.add_const(Value::str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::Fn(proto)) => {
                let proto_idx = frag.add_proto(proto.clone());
                frag.emit_u16(Op::Closure, proto_idx, span);
                let name_idx = frag.add_const(Value::str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::ComputedConst { start, end }) => {
                copy_code_range(frag, top, *start, *end, span);
                let name_idx = frag.add_const(Value::str(name.clone()));
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
    let name_idx = frag.add_const(Value::str(name.clone()));
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
                let name_idx = frag.add_const(Value::str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::Fn(proto)) => {
                let proto_idx = frag.add_proto(proto.clone());
                frag.emit_u16(Op::Closure, proto_idx, span);
                let name_idx = frag.add_const(Value::str(name.clone()));
                frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
            }
            Some(TopDef::ComputedConst { start, end }) => {
                // Copy the initializer instruction range verbatim (pool-remapped), then
                // bind the result with DEFINE_GLOBAL. The range excludes the trailing
                // DEFINE_GLOBAL, so emit it here.
                copy_code_range(&mut frag, top, *start, *end, span);
                let name_idx = frag.add_const(Value::str(name.clone()));
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
        let name_idx = frag.add_const(Value::str(Rc::from(entry_name)));
        frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
    }

    frag.emit(Op::Nil, span);
    frag.emit(Op::Return, span);

    if let Some(limit) = frag.take_overflow() {
        let (msg, span) = limit.into_message_span();
        return Err(Control::Panic(crate::error::AsError::at(msg, span)));
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
            if let Some(ValueKind::Str(name)) = top.consts.get(idx).map(|c| c.kind()) {
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
    // RT §2.3(b): the runtime-only build has no compiler — collapse the source-mode
    // recompile to the SAME no-source recoverable panic. A bundle never sets
    // `worker_source` (it sets `worker_aso_bytes`/`worker_archive_bytes`, the chunk
    // path), so a stub never reaches a *needed* compile here; the refusal is the
    // honest narrowing if an embedder somehow set source on a stub. Non-rt unchanged.
    #[cfg(ascript_rt)]
    {
        let _ = (&src, &class_name);
        Err(Control::Panic(crate::error::AsError::new(format!(
            "cannot dispatch worker '{entry_name}': the program source is unavailable \
             (worker fns require running via `ascript run`)"
        ))))
    }
    #[cfg(not(ascript_rt))]
    {
        let top = crate::compile::compile_source(&src).map_err(|e| {
            Control::Panic(crate::error::AsError::at(e.message, e.span))
        })?;
        build_code_slice(&top, entry_name, class_name)
    }
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
    // RT §2.3(b): no compiler in the runtime build — collapse to the no-source panic.
    #[cfg(ascript_rt)]
    {
        let _ = &src;
        Err(Control::Panic(crate::error::AsError::new(format!(
            "cannot dispatch worker '{class_name}.{method_name}': the program source is \
             unavailable (worker fns require running via `ascript run`)"
        ))))
    }
    #[cfg(not(ascript_rt))]
    {
        let top = crate::compile::compile_source(&src)
            .map_err(|e| Control::Panic(crate::error::AsError::at(e.message, e.span)))?;
        build_code_slice_for_static_method(&top, class_name, method_name)
    }
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
    // RT §2.3(b): the source-preferred arm needs the compiler — gated OUT of the
    // runtime build. A bundle resolves its worker top chunk from `worker_aso_bytes`/
    // `worker_archive_bytes` (the chunk path below), so a stub does not lose
    // functionality; it simply never recompiles source. Non-rt unchanged.
    #[cfg(not(ascript_rt))]
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
    match v.kind() {
        ValueKind::Nil => frag.emit(Op::Nil, span),
        ValueKind::Bool(true) => frag.emit(Op::True, span),
        ValueKind::Bool(false) => frag.emit(Op::False, span),
        _ => {
            let idx = frag.add_const(v);
            frag.emit_u16(Op::Const, idx, span);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::OwnedKind;
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
                    if let Some(ValueKind::Str(name)) = chunk.consts.get(idx).map(|c| c.kind()) {
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
            name_span: None,
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
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::float(5.0)]).await;
        assert_eq!(out.unwrap(), Value::float(15.0));
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
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::float(8.0)]).await;
        assert_eq!(out.unwrap(), Value::float(50.0));
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

    /// validate_into SOUNDNESS (Phase 2): a class referenced ONLY as a FIELD TYPE of a
    /// shipped class is itself shipped. `Inner` has no `GET_GLOBAL` edge — it appears
    /// only as the declared type of `Outer.inner` — yet `Outer.from({inner:{...}})`
    /// coerces the nested Object into an `Inner` instance via `coerce_field`, so the
    /// worker slice MUST keep `Inner`. This guards the shared `collect_def_refs` fix on
    /// the worker path (the bundle tree-shaker shares the same function). Also asserts the
    /// element type of an `array<Item>` field is shipped, and an unrelated class is not.
    #[tokio::test]
    async fn slice_ships_class_referenced_only_as_field_type() {
        let src = "
            class Inner { v: number }
            class Item { n: number }
            class Unrelated { x: number }
            class Outer {
                inner: Inner
                items: array<Item>
            }
            worker fn g(n) {
                let o = Outer.from({ inner: { v: n }, items: [{ n: n }] })
                return o.inner.v + o.items[0].n
            }
        ";
        let slice = build_slice_for_test(src, "g").await;
        let names = slice.dep_names();
        assert!(names.contains("Outer"), "missing Outer: {names:?}");
        assert!(
            names.contains("Inner"),
            "missing field-type class Inner (validate_into soundness): {names:?}"
        );
        assert!(
            names.contains("Item"),
            "missing array<Item> element-type class Item: {names:?}"
        );
        assert!(
            !names.contains("Unrelated"),
            "shipped unrelated class (over-shake): {names:?}"
        );

        // End-to-end: the shipped fragment runs in a FRESH isolate with NO access to the
        // original heap and `Outer.from` validates the nested `Inner`/`Item` correctly —
        // the exact path that errored before the fix (`type contract violated … expected
        // Inner, got object`). g(5) => inner.v(5) + items[0].n(5) == 10.
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::int(5)])
            .await
            .expect("fragment runs in a fresh isolate");
        assert_eq!(out, Value::int(10), "got: {out:?}");
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
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::float(0.0)])
            .await
            .expect("runs");
        match out.into_kind() {
            OwnedKind::Array(arr) => {
                let a = arr.borrow();
                assert_eq!(a[0], Value::bool_(true), "Reader conformance across isolate");
                assert_eq!(a[1], Value::bool_(true), "ReadWriter conformance across isolate");
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
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::float(8.0)]).await;
        assert_eq!(out.unwrap(), Value::float(50.0));
    }
}



