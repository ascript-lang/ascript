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
//! The closure ships top-level FUNCTIONS and LITERAL-initializer `const`s
//! (transitively). A referenced name that this builder cannot classify as a
//! shippable definition — a `const` with a COMPUTED initializer, a `class`/`enum`/
//! `import` binding, or a plain builtin — is NOT an error here: it is simply left as
//! a late-bound `GET_GLOBAL` reference in the shipped bytecode. On the far isolate
//! that reference resolves against the isolate's own globals (builtins are present
//! there) or, if the name is genuinely absent, raises the STANDARD recoverable
//! `undefined variable '<name>'` runtime panic at the call site — exactly as any
//! unbound reference would. So `build_code_slice` returns `Ok` with a slice that
//! omits such a name; the failure (if any) surfaces LATER, loudly, at isolate
//! runtime — never as a wrong or silently-partial result.
//!
//! TODO(Task 8 / Spec B): two follow-ups make computed-const / class deps work for
//! workers that need them: (1) dispatch-time structured-clone of computed-`const`
//! VALUES into the isolate (the plan's "consts structured-clone'd at dispatch", §4),
//! so a worker reading a computed top-level const sees its value; (2) shipping
//! `class`/`enum` definitions for worker fns that construct or return class
//! instances. Until then those deps stay late-bound as described above.

use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use crate::vm::chunk::{Chunk, FnProto};
use crate::vm::opcode::Op;
use crate::worker::WorkerCodeSlice;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// A resolved top-level binding the closure can ship: either a `fn` (its compiled
/// prototype) or a `const`/`let` whose initializer is a single literal-producing op.
#[derive(Clone)]
enum TopDef {
    /// A top-level `fn` — ships as its `FnProto` (re-`CLOSURE`d in the fragment).
    Fn(Rc<FnProto>),
    /// A top-level `const`/`let` bound to a literal value — ships as that value.
    Const(Value),
}

/// Scan a program's top-level [`Chunk`] code stream and build a map from each
/// DIRECT-child top-level global NAME to the definition it binds. A binding is a
/// `<value-producing op>; DEFINE_GLOBAL name` pair; we look at the op IMMEDIATELY
/// preceding each `DEFINE_GLOBAL` to classify it:
///   - `CLOSURE idx`  → a top-level `fn`  → [`TopDef::Fn`] (`protos[idx]`).
///   - `CONST idx`    → a literal const   → [`TopDef::Const`] (`consts[idx]`).
///   - `NIL/TRUE/FALSE` → a literal const → [`TopDef::Const`].
///
/// Any other producer (a top-level `const` whose initializer is a computed
/// expression, or a `class`/`enum`/`import` binding) is left OUT of the map: it is
/// not a simple shippable closure member. A worker fn that references such a name
/// is reported by [`build_code_slice`] as an unsupported dependency (never silently
/// dropped — see the conventions in CLAUDE.md). The common cases (top-level helper
/// fns + literal consts) are covered exactly.
fn top_level_defs(top: &Chunk) -> HashMap<Rc<str>, TopDef> {
    let mut defs: HashMap<Rc<str>, TopDef> = HashMap::new();
    // Track the (op, operand-as-u16) of the previous instruction so a DEFINE_GLOBAL
    // can classify what produced the value it binds.
    let mut prev: Option<(Op, u16)> = None;
    let mut ip = 0usize;
    while ip < top.code.len() {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        // The leading u16 operand (CONST/CLOSURE/DEFINE_GLOBAL all lead with a u16).
        let operand_u16 = if width >= 2 {
            top.read_u16(ip + 1)
        } else {
            0
        };
        if op == Op::DefineGlobal {
            // The name is consts[operand_u16] (a Str); the producer is `prev`.
            if let Some(Value::Str(name)) = top.consts.get(operand_u16 as usize) {
                if let Some(def) = prev.and_then(|(pop, parg)| classify_producer(top, pop, parg)) {
                    defs.entry(name.clone()).or_insert(def);
                }
            }
        }
        prev = Some((op, operand_u16));
        ip += 1 + width;
    }
    defs
}

/// Classify the value-producing instruction that precedes a `DEFINE_GLOBAL` into a
/// shippable [`TopDef`], or `None` if it is not a simple fn/literal-const binding.
fn classify_producer(top: &Chunk, op: Op, operand: u16) -> Option<TopDef> {
    match op {
        Op::Closure => top.protos.get(operand as usize).cloned().map(TopDef::Fn),
        Op::Const => top.consts.get(operand as usize).cloned().map(TopDef::Const),
        Op::Nil => Some(TopDef::Const(Value::Nil)),
        Op::True => Some(TopDef::Const(Value::Bool(true))),
        Op::False => Some(TopDef::Const(Value::Bool(false))),
        _ => None,
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
/// The fragment, when loaded and run on a FRESH `Vm`, emits — for each closure
/// member in a deterministic order — its define (a literal `const` → `CONST;
/// DEFINE_GLOBAL`, a `fn` → `CLOSURE; DEFINE_GLOBAL`) and finally the entry fn's
/// own define, then `NIL; RETURN`. Running it defines exactly the closure's globals
/// (and the entry) and NOTHING else from the original module — so the isolate can
/// then fetch and call the entry with zero access to the original heap.
///
/// `class_name` is `Some` for a `static worker fn` (Task 8 binds the class); for a
/// free `worker fn` it is `None`.
///
/// Returns a recoverable `Control::Panic` ONLY when the entry itself is missing or
/// is not a top-level function. A referenced DEPENDENCY that cannot be classified as
/// a shippable def (a computed-initializer `const`, a `class`/`enum`/`import`, or a
/// builtin) is NOT a build-time error: this returns `Ok` with a slice that omits the
/// name, leaving it as a late-bound `GET_GLOBAL` reference. On the far isolate that
/// reference resolves against the isolate's own globals/builtins or, if genuinely
/// absent, raises the standard recoverable `undefined variable '<name>'` panic at
/// run time (see the module-level docs). It is never a wrong or silently-partial
/// result — an unsatisfiable dependency fails loudly at isolate runtime.
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
// closure of every method body, plus any superclass classes (recursively).
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
/// (recursively), the transitive top-level fn/const deps of all method bodies, and
/// the class itself (as a top-level `DEFINE_GLOBAL <class_name>`). The actor then
/// constructs the instance by looking up the class global and calling its `init`.
///
/// `fn_id`/`entry_name` are set to the class name (the actor's `ActorMsg::Init`
/// fetches the class global by `entry_name`).
pub fn build_class_slice(top: &Chunk, class_name: &str) -> Result<WorkerCodeSlice, Control> {
    let defs = top_level_defs(top);

    let mut frag = Chunk::new();
    frag.name = Some("<worker-class-slice>".to_string());
    let span = Span::new(0, 0);

    // Emit each class (superclasses first, then the target) and collect the union of
    // their method bodies' GET_GLOBAL references for the fn/const dep closure.
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

    // Compute the transitive top-level fn/const dependency closure of the collected
    // method references (same fixpoint as `materialize_slice`), and emit those defs
    // BEFORE the classes would need them. They are late-bound `GET_GLOBAL`s, so
    // emission order between deps and classes does not matter for correctness, but
    // we emit deps first for readability.
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

/// Like [`build_class_slice`] but recompiling from the program source retained on
/// the [`crate::interp::Interp`] (the SINGLE slice path shared by both engines — the
/// tree-walker has no compiled chunk of its own). Mirrors
/// [`build_code_slice_from_source`].
pub fn build_class_slice_from_source(
    interp: &crate::interp::Interp,
    class_name: &str,
) -> Result<WorkerCodeSlice, Control> {
    let src = interp.worker_source().ok_or_else(|| {
        Control::Panic(crate::error::AsError::new(format!(
            "cannot spawn worker class '{class_name}': the program source is \
             unavailable (worker classes require running via `ascript run`)"
        )))
    })?;
    let top = crate::compile::compile_source(&src)
        .map_err(|e| Control::Panic(crate::error::AsError::at(e.message, e.span)))?;
    build_class_slice(&top, class_name)
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

/// Emit the transitive top-level fn/const dependency closure of `roots` into `frag`
/// (same fixpoint + source-order emit as `materialize_slice`, minus the entry).
fn emit_dep_closure(
    top: &Chunk,
    defs: &HashMap<Rc<str>, TopDef>,
    roots: &[Rc<str>],
    frag: &mut Chunk,
    span: Span,
) -> Result<(), Control> {
    let mut seen: HashSet<Rc<str>> = HashSet::new();
    let mut included: HashSet<Rc<str>> = HashSet::new();
    let mut work: Vec<Rc<str>> = roots.to_vec();
    while let Some(name) = work.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        included.insert(name.clone());
        if let Some(TopDef::Fn(proto)) = defs.get(name.as_ref()) {
            let mut refs = Vec::new();
            collect_get_global_names(&proto.chunk, &mut refs);
            for r in refs {
                if !seen.contains(&r) {
                    work.push(r);
                }
            }
        }
    }
    // Emit in original source (DEFINE_GLOBAL) order.
    let mut ip = 0usize;
    while ip < top.code.len() {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        if op == Op::DefineGlobal {
            let idx = top.read_u16(ip + 1) as usize;
            if let Some(Value::Str(name)) = top.consts.get(idx) {
                if included.contains(name) && defs.contains_key(name.as_ref()) {
                    match defs.get(name.as_ref()).expect("included ⊆ defs") {
                        TopDef::Const(v) => emit_const_load(frag, v.clone(), span),
                        TopDef::Fn(proto) => {
                            let proto_idx = frag.add_proto(proto.clone());
                            frag.emit_u16(Op::Closure, proto_idx, span);
                        }
                    }
                    let name_idx = frag.add_const(Value::Str(name.clone()));
                    frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
                }
            }
        }
        ip += 1 + width;
    }
    Ok(())
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
    // Fixpoint over names: seed with the entry proto's own GET_GLOBAL refs, then pull
    // in every referenced top-level fn/const, recursing into newly-added fns. `seen`
    // de-dups; `order` is unused beyond membership (the emit walk reuses source order).
    let mut seen: HashSet<Rc<str>> = HashSet::new();
    let mut included: HashSet<Rc<str>> = HashSet::new();
    let mut work: Vec<Rc<str>> = Vec::new();

    // Seed from the entry proto's references (for a top-level entry this also re-adds
    // the entry's own deps; the entry name itself is handled via the source walk /
    // emit_entry).
    {
        let mut refs = Vec::new();
        collect_get_global_names(&entry_proto.chunk, &mut refs);
        for r in refs {
            work.push(r);
        }
    }
    // For a top-level entry, also include the entry NAME so the source walk emits it.
    if !emit_entry {
        work.push(Rc::from(entry_name));
    }

    while let Some(name) = work.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        included.insert(name.clone());
        if let Some(TopDef::Fn(proto)) = defs.get(name.as_ref()) {
            let mut refs = Vec::new();
            collect_get_global_names(&proto.chunk, &mut refs);
            for r in refs {
                if !seen.contains(&r) {
                    work.push(r);
                }
            }
        }
    }

    let mut frag = Chunk::new();
    frag.name = Some("<worker-slice>".to_string());

    // Walk the original DEFINE_GLOBAL order so dep members emit in source order.
    let mut emit_order: Vec<Rc<str>> = Vec::new();
    let mut ip = 0usize;
    while ip < top.code.len() {
        let Some(op) = Op::from_u8(top.code[ip]) else {
            break;
        };
        let width = op.operand_width();
        if op == Op::DefineGlobal {
            let idx = top.read_u16(ip + 1) as usize;
            if let Some(Value::Str(name)) = top.consts.get(idx) {
                if included.contains(name) && defs.contains_key(name.as_ref()) {
                    emit_order.push(name.clone());
                }
            }
        }
        ip += 1 + width;
    }

    let span = Span::new(0, 0);
    for name in &emit_order {
        match defs.get(name.as_ref()).expect("emit_order ⊆ defs") {
            TopDef::Const(v) => {
                emit_const_load(&mut frag, v.clone(), span);
            }
            TopDef::Fn(proto) => {
                let proto_idx = frag.add_proto(proto.clone());
                frag.emit_u16(Op::Closure, proto_idx, span);
            }
        }
        let name_idx = frag.add_const(Value::Str(name.clone()));
        frag.emit_u16_u8(Op::DefineGlobal, name_idx, 0, span);
    }

    // For a static method, the entry has no top-level DEFINE_GLOBAL; emit it last.
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
        let out = run_slice_in_fresh_isolate(&slice, "g", vec![Value::Number(5.0)]).await;
        assert_eq!(out.unwrap(), Value::Number(15.0));
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
}
