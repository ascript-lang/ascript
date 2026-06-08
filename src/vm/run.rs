//! The VM's async run loop (`Vm::run`).
//!
//! V2 implements the **synchronous core**: constants, literal pushes, stack
//! `Pop`/`Dup`, locals/globals, calls, templates, the full binary/unary operators
//! (string concat / decimal / range / cross-type equality / numeric) and `Return`.
//! Every other opcode is a documented `not yet implemented` Tier-2 panic that
//! later VM slices fill in. Panics carry the faulting instruction's [`Span`] so
//! ariadne points at the source exactly like the tree-walker.
//!
//! The binary/unary arms call the SAME `apply_binop`/`apply_unop` free functions
//! the tree-walker uses (`src/interp.rs`), so the two engines cannot drift on
//! arithmetic semantics or panic messages — there is one implementation.

use crate::ast::{BinOp, UnOp};
use crate::error::AsError;
use crate::interp::{error_message, Control, Interp};
use crate::span::Span;
use crate::value::Value;
use crate::vm::fiber::Fiber;
use crate::vm::opcode::Op;
use crate::vm::value_ext::{Closure, RunOutcome};
use gcmodule::Cc;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// A module's export collector (V12-T4): an insertion-ordered name→value map behind
/// shared interior mutability, so `Op::DefineExport` records into it and an importer
/// reads it back (the namespace form clones it into a `Value::Object`).
type ModuleExports = Rc<RefCell<indexmap::IndexMap<String, Value>>>;

/// A module-scope user-global slot: its current value plus its REASSIGNABILITY,
/// mirroring the tree-walker's per-binding `Environment` mutability flag. A `let`
/// (or `param`, never a global) is mutable; `const`/`fn`/`class`/`enum`/`import` are
/// immutable. `Op::SetGlobal` consults `mutable` at RUNTIME so an immutable global
/// reassigned from a LATER, separately-compiled chunk (REPL line-to-line, or a main
/// module reassigning an import) errors `cannot assign to immutable binding` exactly
/// like the tree-walker — the compile-time `Op::ImmutableError` only sees same-chunk
/// assignments. The value is a plain owned `Value` (the `Vm` is the GC root, so it
/// stays reachable — NO `Cc` cell, preserving the deterministic native-Drop gate).
struct GlobalSlot {
    value: Value,
    mutable: bool,
}

/// The bytecode virtual machine.
///
/// Holds the shared [`Interp`] (the runtime state the VM and tree-walker share)
/// and a self-`Weak` mirroring [`Interp`]'s pattern, so a `&self` method can
/// recover an owned `Rc<Vm>` to hand to a spawned task in V7.
pub struct Vm {
    interp: Rc<Interp>,
    self_weak: RefCell<Weak<Vm>>,
    /// Per-class compiled-method table (V9). `value.rs`'s `Class`/`Method` is
    /// frozen and holds a TREE-WALKER body the VM cannot run, so the VM compiles
    /// each method to a `Value::Closure` and stores it HERE instead — keyed by the
    /// class's `Rc` IDENTITY (`Rc::as_ptr` address) → method name → compiled
    /// closure. A class's `Value::Class.methods` map is left empty; method dispatch
    /// goes through this table (`compiled_method`). The key is stable because the
    /// `Rc<Class>` is created once at compile time and shared by every instance.
    class_methods: RefCell<HashMap<usize, HashMap<String, Cc<Closure>>>>,
    /// Per-class STATIC method table (SP1 §3): class `Rc` identity → static name →
    /// compiled closure. A SEPARATE namespace from `class_methods`; a static is
    /// called as `C.name(args)` with NO receiver (a plain `Value::Closure` call),
    /// resolved up the superclass chain by `find_compiled_static_method`.
    class_static_methods: RefCell<HashMap<usize, HashMap<String, Cc<Closure>>>>,
    /// Per-class field-default thunk table (V9): class `Rc` identity → field name →
    /// a zero-arg closure that produces the field's default value. Run once per
    /// constructed instance (so a mutable default yields a fresh value each time,
    /// matching the tree-walker's per-construct default eval).
    class_defaults: RefCell<HashMap<usize, HashMap<String, Cc<Closure>>>>,
    /// Per-VM hidden-class registry (V11-T2). Assigns a `shape_id` to every
    /// object/instance key-LAYOUT via a transition tree; V11-T3 inline caches key
    /// on these ids. Only VM code paths touch it (the tree-walker leaves shapes 0).
    shapes: RefCell<crate::vm::shape::ShapeRegistry>,
    /// Cache of each class's BASE shape (its declared-field layout, declaration
    /// order), keyed by the class's `Rc` identity (`Rc::as_ptr`) like the method
    /// tables. Computed once per class so every instance shares the same base id.
    class_base_shapes: RefCell<HashMap<usize, u32>>,
    /// A shared `def_env` for every VM-created class (task #157). The compiler
    /// leaves `Class.def_env` as an inert `global_env()` placeholder because the VM
    /// has no tree-walker Environment; but the SHARED `Interp::validate_into`
    /// (powering `ClassName.from` / typed-parse) resolves a NESTED-class field-type
    /// name and a default-expr name via `def_class.def_env.get(name)`. So `Op::Class`
    /// (a) rebuilds the class with `def_env` set to this env, and (b) registers the
    /// new class into it. The env is a single CHILD of `global_env()` shared by all
    /// classes — mirroring the tree-walker, where every top-level class's `def_env`
    /// is the SAME module `env` (so siblings/forward refs resolve, late-bound). The
    /// init is deferred to first use (built lazily) so a VM that never declares a
    /// class allocates nothing.
    class_env: RefCell<Option<crate::env::Environment>>,
    /// **The `--no-specialize` KILL SWITCH (V11-T5).** When `true` (the default),
    /// every specialization fast path is active: the polymorphic field/method
    /// inline caches (`GET_PROP`/`SET_PROP`/`CALL_METHOD`) and the PEP-659 adaptive
    /// arithmetic + `GET_GLOBAL` caches are consulted and recorded in front of the
    /// generic path. When `false`, ALL of those fast paths are skipped — every
    /// property read/write, method dispatch, arithmetic op, and global resolve goes
    /// straight through the generic lookup with NO IC/adaptive consult or record.
    ///
    /// The two modes MUST produce byte-identical results (both correct); the only
    /// difference is speed. The three-way differential in `tests/vm_differential.rs`
    /// asserts `generic-VM == specialized-VM == tree-walker` over the whole corpus,
    /// so any IC/adaptive guard bug makes generic and specialized diverge instantly.
    specialize: bool,
    /// The CURRENT module's export collector (V12-T4). `Op::DefineExport` records
    /// each `export`ed top-level binding here. While running an imported file module
    /// (`Vm::run_file_module`), this points at THAT module's fresh exports map; while
    /// running the entry program it points at a throwaway map (a main program's
    /// exports are unused, mirroring the tree-walker). Swapped on a stack-discipline
    /// basis around a nested module run so transitive imports collect into the right
    /// module. Insertion-ordered so a namespace import reflects declaration order.
    module_exports: RefCell<ModuleExports>,
    /// Cache of already-loaded FILE modules (V12-T4), keyed by canonical path →
    /// the module's exports map. Mirrors the tree-walker's `Interp::modules` cache:
    /// a module's top-level runs at most once; repeated `import`s reuse the cached
    /// exports. Inserted BEFORE the module body runs so a circular import resolves to
    /// the (then partially-populated) in-progress entry instead of re-running.
    file_modules: RefCell<HashMap<std::path::PathBuf, ModuleExports>>,
    /// The directory of the module currently executing (V12-T4), used to resolve a
    /// relative file import (`from "./mod"`). Mirrors `Interp::module_dir`. Swapped
    /// around a nested module run and restored after.
    module_dir: RefCell<std::path::PathBuf>,
    /// MODULE-SCOPE USER-GLOBALS: every DIRECT-child top-level binding of the entry
    /// program (`let`/`const`/`fn`/`class`/`enum`/`import`) is a late-bound global
    /// stored here by name, NOT a SourceFile-frame slot-local. `Op::DefineGlobal`
    /// inserts, `Op::SetGlobal` updates, and `Op::GetGlobal` consults this table
    /// BEFORE the bare builtins — so a function/thunk body that references a top-level
    /// binding declared LATER resolves at run time, matching the tree-walker's single
    /// shared module `Environment`. Plain owned `Value`s (the `Vm` is the GC root, so
    /// they stay reachable) in insertion (declaration) order. This table is ALSO the
    /// REPL's cross-line persistence: one `Vm` kept alive across lines carries its
    /// globals forward. (A file module's exports use the separate `module_exports`
    /// path; only the entry chunk defines into this table.)
    user_globals: RefCell<indexmap::IndexMap<Rc<str>, GlobalSlot>>,
    /// Monotonic version counter, bumped on every global (re)definition or
    /// assignment. The V11-T4 GET_GLOBAL inline cache (`adapt::GlobalCache`) guards
    /// its cached value with this version: a cache entry recorded at version V is
    /// valid only while the version is still V, so any global write invalidates it.
    /// Top-level defines run once at load, then the version is stable, so the caches
    /// stay hot for the steady-state hot loops.
    global_version: std::cell::Cell<u64>,
    /// STRUCTURAL generation (SP8). Bumped ONLY when a NEW global is DEFINED/inserted
    /// (`define_user_global`), NEVER on a plain reassignment (`update_user_global`).
    /// The SP8 index-stable `GET_GLOBAL`/`SET_GLOBAL` cache (`GlobalCache::IndexBound`)
    /// guards its cached `IndexMap` index with this generation: only a define can
    /// change which index a name maps to (or introduce a shadow), so a hot reassigned
    /// top-level `let` loop never bumps it — the index cache stays hot every iteration
    /// (no thrash). Distinct from `global_version`, which keeps serving the builtin
    /// `Cached` path (and DOES bump on define).
    struct_gen: std::cell::Cell<u64>,
    /// The MODULE source of the frame most recently about to execute (SP4 §3).
    /// Updated each instruction; read by [`run`] to bind a span's own module
    /// source onto an escaping panic, so a cross-module panic renders its caret in
    /// the module the span belongs to. `None` until the first sourced frame runs.
    last_fault_source: RefCell<Option<Rc<crate::error::SourceInfo>>>,
}

impl Vm {
    /// Build a VM over `interp` and install its self-`Weak` (mirroring
    /// [`Interp::install_self`]).
    pub fn new(interp: Rc<Interp>) -> Rc<Self> {
        Self::with_specialize(interp, true)
    }

    /// Build a NON-specializing ("generic") VM — the `--no-specialize` kill switch
    /// (V11-T5). All inline-cache and adaptive fast paths are disabled; every
    /// dispatch takes the generic path. Used by `vm_run_source_generic` and the
    /// three-way differential to prove the fast paths never change a result.
    pub fn new_generic(interp: Rc<Interp>) -> Rc<Self> {
        Self::with_specialize(interp, false)
    }

    /// Shared constructor: build a VM with `specialize` set explicitly and install
    /// its self-`Weak` (mirroring [`Interp::install_self`]).
    pub fn with_specialize(interp: Rc<Interp>, specialize: bool) -> Rc<Self> {
        let vm = Rc::new(Vm {
            interp,
            self_weak: RefCell::new(Weak::new()),
            class_methods: RefCell::new(HashMap::new()),
            class_static_methods: RefCell::new(HashMap::new()),
            class_defaults: RefCell::new(HashMap::new()),
            shapes: RefCell::new(crate::vm::shape::ShapeRegistry::new()),
            class_base_shapes: RefCell::new(HashMap::new()),
            class_env: RefCell::new(None),
            specialize,
            module_exports: RefCell::new(Rc::new(RefCell::new(indexmap::IndexMap::new()))),
            file_modules: RefCell::new(HashMap::new()),
            module_dir: RefCell::new(std::env::current_dir().unwrap_or_else(|_| ".".into())),
            user_globals: RefCell::new(indexmap::IndexMap::new()),
            global_version: std::cell::Cell::new(0),
            struct_gen: std::cell::Cell::new(0),
            last_fault_source: RefCell::new(None),
        });
        *vm.self_weak.borrow_mut() = Rc::downgrade(&vm);
        // Register the VM on the shared interpreter so a native higher-order
        // stdlib function (e.g. `array.map`, `recover`) can re-enter the VM to
        // run a `Value::Closure` callback (the `native → VM` half of the bridge;
        // see `Interp::call_value`'s `Closure` arm and `Vm::call_value`).
        vm.interp.set_vm(Rc::downgrade(&vm));
        vm
    }

    /// Set the directory used to resolve relative FILE imports from the ENTRY
    /// program (V12-T4). The entry program is not loaded via `load_file_module`, so
    /// its `module_dir` must be seeded here before `run` (e.g. to the `.aso`/`.as`
    /// file's parent directory) so `import ... from "./mod"` resolves correctly.
    pub fn set_module_dir(&self, dir: std::path::PathBuf) {
        *self.module_dir.borrow_mut() = dir;
    }

    /// Recover an owned `Rc<Vm>` from `&self`. Used by the async-fn eager-spawn in
    /// the `Op::Call` arm (V7) to hand an owned VM into the `'static` spawned task.
    pub fn rc(&self) -> Rc<Vm> {
        self.self_weak
            .borrow()
            .upgrade()
            .expect("Vm self-ref not installed")
    }

    /// The shared interpreter state.
    pub fn interp(&self) -> &Rc<Interp> {
        &self.interp
    }

    /// Workers Spec A: dispatch a `worker fn` closure to a pooled isolate, returning
    /// the `Value::Future`. Builds the shippable code slice — preferring the source
    /// recompile path (via `Interp::worker_source`) when source is available (the normal
    /// run-from-source path, shared with the tree-walker), or falling back to building
    /// the slice directly from the stored pre-compiled top-level chunk (the `.aso`
    /// run path, via `Interp::worker_aso_bytes`) when no source is recorded. The entry name
    /// is the closure's compiled chunk name (a top-level `worker fn`).
    fn dispatch_worker_closure(
        &self,
        callee: &crate::vm::value_ext::Closure,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let entry_name = callee.proto.chunk.name.as_deref().ok_or_else(|| {
            Control::Panic(crate::error::AsError::at(
                "worker fn has no name (internal invariant)".to_string(),
                span,
            ))
        })?;
        // Inline-nesting: a worker fn called from inside an isolate runs locally (no
        // pool round-trip, no slice build) — the entry is already a global on the VM.
        if crate::worker::pool::in_isolate() {
            return crate::worker::dispatch_worker_inline(&self.interp, entry_name, args, span);
        }
        // Route to the static-method or free-function slice builder depending on
        // whether the proto carries an owning class name. A `static worker fn`
        // compiled on a class has `proto.owning_class = Some(class_name)` (set by
        // the compiler when emitting static method protos); a free `worker fn` has
        // `owning_class = None` and goes through the ordinary top-level path.
        let class_name: Option<&str> = callee.proto.owning_class.as_deref();

        // Prefer the source recompile path (produces an identical slice for any run
        // mode that has source). Fall back to the pre-compiled chunk derived from the raw
        // `.aso` bytes stored by `run_aso_file` when no source is recorded (the .aso path).
        let slice = if self.interp.worker_source().is_some() {
            if let Some(cls) = class_name {
                crate::worker::build_code_slice_for_static_method_from_source(
                    &self.interp, cls, entry_name,
                )?
            } else {
                crate::worker::build_code_slice_from_source(&self.interp, entry_name, None)?
            }
        } else if let Some(raw) = self.interp.worker_aso_bytes() {
            let top = crate::vm::chunk::Chunk::from_bytes(&raw).map_err(|e| {
                Control::Panic(crate::error::AsError::at(
                    format!("cannot re-parse .aso for worker dispatch: {e:?}"),
                    span,
                ))
            })?;
            if let Some(cls) = class_name {
                crate::worker::build_code_slice_for_static_method(&top, cls, entry_name)?
            } else {
                crate::worker::build_code_slice(&top, entry_name, None)?
            }
        } else {
            return Err(Control::Panic(crate::error::AsError::at(
                format!(
                    "cannot dispatch worker '{entry_name}': the program source is unavailable \
                     (worker fns require running via `ascript run`)"
                ),
                span,
            )));
        };
        crate::worker::dispatch_worker(&self.interp, slice, args, span)
    }

    /// SP3 §B: increment the SHARED logical call-depth on establishing a new VM
    /// call frame, returning the clean Tier-2 panic if it would exceed
    /// [`crate::interp::MAX_CALL_DEPTH`]. Called at the in-loop `fiber.frames.push`
    /// sites (the frame-Vec call path) — one increment per logical call, matching
    /// the tree-walker's one-per-`run_body`. The matching decrement is in
    /// [`Vm::return_from_frame`] on the non-root pop, so the count tracks the live
    /// frame depth. The counter is `Interp.call_depth` (a `Cell`), never held
    /// across an `.await`.
    fn enter_frame_depth(&self, span: crate::span::Span) -> Result<(), Control> {
        let depth = self.interp.call_depth_cell();
        let next = depth.get() + 1;
        if next > crate::interp::MAX_CALL_DEPTH {
            return Err(Control::Panic(crate::error::AsError::at(
                "maximum recursion depth exceeded",
                span,
            )));
        }
        depth.set(next);
        Ok(())
    }

    /// SP3 §B: decrement the shared logical call-depth when a non-root frame is
    /// popped (the matching dec for [`Vm::enter_frame_depth`]). The ROOT/initial
    /// frame of a fiber is NOT decremented here — its depth unit is owned by the
    /// program root (counter returns to 0 at program end) or by the re-entrant
    /// `self.run`'s RAII [`crate::interp::DepthGuard`] (`invoke_compiled_method` /
    /// `call_value`), so it unwinds exactly once.
    fn leave_frame_depth(&self) {
        let depth = self.interp.call_depth_cell();
        depth.set(depth.get() - 1);
    }

    /// Force a full cycle collection (V13-T3). Thin pass-through to
    /// [`crate::gc::collect`] so tests (V13-T4's soundness gate) can deterministically
    /// trigger trial-deletion at a known point and assert a cycle was reclaimed. The
    /// collector is thread-local; the VM runs single-threaded so this collects this
    /// VM's whole `Cc` graph. Returns the number of objects reclaimed.
    pub fn collect(&self) -> usize {
        crate::gc::collect()
    }

    /// The shared `def_env` for VM-created classes (task #157), built lazily as a
    /// single child of `global_env()` and reused for every class. See the
    /// `class_env` field doc for why this mirrors the tree-walker's module env.
    fn class_env(&self) -> crate::env::Environment {
        let mut slot = self.class_env.borrow_mut();
        if slot.is_none() {
            // First build: seed with the module-scope user-globals already defined, so
            // the SHARED `validate_into` (`ClassName.from` / typed-parse) resolves a
            // field default that references a top-level `let`/`const`/`fn`/`class`
            // (e.g. `n: number = LATER` where `const LATER` is a module global) — the
            // VM's construct path reads those via `GET_GLOBAL`, but `.from` evaluates
            // the default through this `def_env`. Kept in sync by `define_user_global`.
            let env = crate::interp::global_env().child();
            for (name, gslot) in self.user_globals.borrow().iter() {
                let _ = env.define(name, gslot.value.clone(), true);
            }
            *slot = Some(env);
        }
        slot.as_ref().unwrap().clone()
    }

    /// Resolve, load, and run a FILE module on the VM, returning its exports map
    /// (V12-T4). Mirrors the tree-walker's `Interp::load_module` + the `.aso`/`.as`
    /// precedence rule:
    ///
    /// - `source` is resolved relative to the CURRENT module's directory
    ///   (`self.module_dir`). The extension defaults to `.as` if absent.
    /// - Both `mod.aso` (compiled) and `mod.as` (source) are considered. The `.aso`
    ///   is PREFERRED when there is no source present OR the `.aso` is at least as new
    ///   as the source (`aso_mtime >= src_mtime`) — Python's rule. Otherwise (source
    ///   newer, or `.aso` absent) the source is compiled fresh. A present-but-stale or
    ///   version-mismatched / unverifiable `.aso` falls back to recompiling the source
    ///   when source is present, else surfaces a clear error.
    /// - The module top-level runs on a fresh fiber with `module_exports` and
    ///   `module_dir` swapped to this module; `Op::DefineExport` collects its exports.
    /// - The result is cached by canonical path; a repeated import reuses it (and a
    ///   circular import resolves to the in-progress entry, populated so far).
    ///
    /// `fault_ip`/`fiber` anchor any error at the importing `Op::Import` site.
    #[async_recursion::async_recursion(?Send)]
    async fn load_file_module(
        &self,
        source: &str,
        fault_ip: usize,
        fiber: &Fiber,
    ) -> Result<ModuleExports, Control> {
        use std::path::PathBuf;

        // Resolve the requested module path relative to the importer's dir; default
        // the extension to `.as` (so `./mod` finds `mod.as`/`mod.aso`).
        let requested = self.module_dir.borrow().join(source);
        let stem_path: PathBuf = if requested.extension().is_some() {
            // An explicit `.aso`/`.as` extension — honor it literally.
            requested.clone()
        } else {
            requested.with_extension("as")
        };
        let as_path = stem_path.with_extension("as");
        let aso_path = stem_path.with_extension("aso");

        // Canonical cache key: prefer the source path's canonical form, else the
        // `.aso`'s, else the requested path (so a missing-file error is reported
        // against a stable key and the cache dedups regardless of which file exists).
        let canon = as_path
            .canonicalize()
            .or_else(|_| aso_path.canonicalize())
            .unwrap_or_else(|_| stem_path.clone());

        if let Some(entry) = self.file_modules.borrow().get(&canon) {
            return Ok(entry.clone()); // cached (or in-progress: circular import)
        }

        // Decide whether to load the `.aso` or compile the `.as`, by mtime.
        let src_meta = std::fs::metadata(&as_path).ok();
        let aso_meta = std::fs::metadata(&aso_path).ok();
        let src_mtime = src_meta.as_ref().and_then(|m| m.modified().ok());
        let aso_mtime = aso_meta.as_ref().and_then(|m| m.modified().ok());

        // Prefer `.aso` when present AND (no source, OR aso is at least as new).
        let prefer_aso = aso_meta.is_some()
            && match (aso_mtime, src_mtime) {
                (_, None) => true,            // no source: must use .aso
                (Some(a), Some(s)) => a >= s, // .aso fresh enough
                (None, Some(_)) => false,     // can't read .aso mtime: recompile
            };

        let chunk: crate::vm::chunk::Chunk = if prefer_aso {
            match std::fs::read(&aso_path) {
                Ok(bytes) => match crate::vm::chunk::Chunk::from_bytes_verified(&bytes) {
                    Ok(c) => c,
                    Err(e) => {
                        // Stale/invalid `.aso`: recompile from source if present,
                        // else surface a clear error.
                        if src_meta.is_some() {
                            self.compile_module_file(&as_path, fault_ip, fiber)?
                        } else {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "cannot load compiled module {}: {} (and no source to recompile)",
                                    aso_path.display(),
                                    e
                                ),
                            ));
                        }
                    }
                },
                Err(e) => {
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        format!("cannot read compiled module {}: {}", aso_path.display(), e),
                    ))
                }
            }
        } else if src_meta.is_some() {
            self.compile_module_file(&as_path, fault_ip, fiber)?
        } else {
            return Err(self.panic_at(
                fiber,
                fault_ip,
                format!(
                    "cannot find module '{source}' (looked for {} and {})",
                    as_path.display(),
                    aso_path.display()
                ),
            ));
        };

        // Build a fresh exports map and cache it BEFORE running the body so a
        // circular import resolves to this (in-progress) entry rather than re-running.
        let exports: ModuleExports = Rc::new(RefCell::new(indexmap::IndexMap::new()));
        self.file_modules
            .borrow_mut()
            .insert(canon.clone(), exports.clone());

        // Swap in this module's exports + dir for the duration of its top-level run.
        let module_dir = canon
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let prev_exports = self.module_exports.replace(exports.clone());
        let prev_dir = self.module_dir.replace(module_dir);

        // Run the module's top-level on its own fiber. Build a zero-arg top-level
        // closure exactly like `vm_run_source_with`.
        let proto = Rc::new(crate::vm::chunk::FnProto {
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
        let mut module_fiber = Fiber::new(closure);
        let run_result = self.run(&mut module_fiber).await;

        // Restore the importer's exports/dir regardless of outcome.
        self.module_exports.replace(prev_exports);
        self.module_dir.replace(prev_dir);

        match run_result {
            Ok(RunOutcome::Done(_)) => Ok(exports),
            Ok(RunOutcome::Yielded(_)) => Err(self.panic_at(
                fiber,
                fault_ip,
                "module top-level unexpectedly yielded".to_string(),
            )),
            Err(c) => {
                // On failure, drop the half-built cache entry so a retry can re-run.
                self.file_modules.borrow_mut().remove(&canon);
                Err(c)
            }
        }
    }

    /// Compile a module's `.as` source file to a [`Chunk`], mapping a read or
    /// compile error to a Tier-2 panic anchored at the importing site.
    fn compile_module_file(
        &self,
        as_path: &std::path::Path,
        fault_ip: usize,
        fiber: &Fiber,
    ) -> Result<crate::vm::chunk::Chunk, Control> {
        let src = std::fs::read_to_string(as_path).map_err(|e| {
            self.panic_at(
                fiber,
                fault_ip,
                format!("cannot read module {}: {}", as_path.display(), e),
            )
        })?;
        let chunk = crate::compile::compile_source(&src).map_err(|e| {
            self.panic_at(
                fiber,
                fault_ip,
                format!(
                    "compile error in module {}: {}",
                    as_path.display(),
                    e.message
                ),
            )
        })?;
        // Bind THIS module's source onto its whole proto tree (SP4 §3) so a panic
        // raised in any of its functions — even when invoked from a different
        // module — renders its caret in this module's own file.
        let src_info = Rc::new(crate::error::SourceInfo {
            path: as_path.display().to_string(),
            text: src,
        });
        chunk.set_module_source(&src_info);
        Ok(chunk)
    }

    /// Drive `fiber` until it returns (or panics). V1 runs the synchronous
    /// arithmetic subset only.
    ///
    /// The faulting `ip` is captured *before* advancing past the opcode and its
    /// operands so diagnostics point at the instruction that faulted. The current
    /// chunk is re-borrowed per access (`&fiber.frame().closure.proto.chunk`) and
    /// never held across a suspension point, keeping
    /// `clippy::await_holding_refcell_ref` clean once V7 introduces awaits.
    pub async fn run(&self, fiber: &mut Fiber) -> Result<RunOutcome, Control> {
        let result = self.run_loop(fiber).await;
        // SP4 §3: bind the FAULTING frame's module source onto an escaping panic
        // that has a span but no span-source yet. The fault propagates
        // synchronously up this `run` (no `.await` between the raise and here), so
        // `last_fault_source` still holds the chunk source of the frame that
        // faulted — the module the span belongs to. Innermost-wins (a nested
        // `run` already bound it). `None` (e.g. an `.aso` with no source) leaves
        // the error untouched, so the driver's entry-source fallback applies.
        if let Err(Control::Panic(e)) = &result {
            if e.span.is_some() && e.span_source.is_none() {
                if let Some(src) = self.last_fault_source.borrow().clone() {
                    return Err(Control::Panic(
                        e.clone().with_span_source(src),
                    ));
                }
            }
        }
        result
    }

    /// The instruction-dispatch loop. Wrapped by [`run`] which binds the faulting
    /// module's source onto an escaping panic (SP4 §3 cross-module provenance).
    async fn run_loop(&self, fiber: &mut Fiber) -> Result<RunOutcome, Control> {
        loop {
            // Capture the faulting ip (the opcode byte's offset) before advancing.
            let fault_ip = fiber.frame().ip;
            // SP4 §3: remember the source of the frame about to execute, so a panic
            // it raises can be bound to its own module's text on the way out.
            if let Some(src) = fiber.frame().closure.proto.chunk.source.borrow().as_ref() {
                *self.last_fault_source.borrow_mut() = Some(src.clone());
            }
            let byte = fiber.frame().closure.proto.chunk.code[fault_ip];
            let op = Op::from_u8(byte)
                .unwrap_or_else(|| panic!("invalid opcode byte {byte:#x} at ip {fault_ip}"));

            // Advance ip past the opcode byte and its inline operands.
            let operand_at = fault_ip + 1;
            fiber.frame_mut().ip = operand_at + op.operand_width();

            match op {
                Op::Const => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.frame().closure.proto.chunk.consts[idx].clone();
                    fiber.push(v);
                }
                Op::Nil => fiber.push(Value::Nil),
                Op::True => fiber.push(Value::Bool(true)),
                Op::False => fiber.push(Value::Bool(false)),
                Op::Pop => {
                    fiber.pop();
                }
                Op::Dup => {
                    let top = fiber.peek(0).clone();
                    fiber.push(top);
                }
                Op::Swap => {
                    // `a b -- b a`. Both operands are compiler-produced, so the
                    // stack always has the two values (a non-empty stack is a
                    // compiler invariant, not user-reachable).
                    let b = fiber.pop();
                    let a = fiber.pop();
                    fiber.push(b);
                    fiber.push(a);
                }
                Op::Rot3 => {
                    // `a b c -- b c a` (the value 3rd from the top rotates to the
                    // top). Compiler-produced three-value group; never user-reachable
                    // with fewer than three on the stack.
                    let c = fiber.pop();
                    let b = fiber.pop();
                    let a = fiber.pop();
                    fiber.push(b);
                    fiber.push(c);
                    fiber.push(a);
                }

                Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Div
                | Op::Mod
                | Op::Pow
                | Op::Lt
                | Op::Le
                | Op::Gt
                | Op::Ge
                | Op::Eq
                | Op::Ne
                | Op::InstanceOf
                | Op::Range => {
                    // The two operands were pushed lhs-then-rhs, so pop rhs first.
                    // The op's span anchors any Tier-2 panic so the VM's
                    // diagnostics are byte-identical to the tree-walker.
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let binop = binop_of(op);
                    // ONE shared dispatch with the tree-walker (`apply_binop`):
                    // string concat / decimal / range / cross-type equality /
                    // numeric, plus every exact panic message. And/Or/Coalesce are
                    // never lowered to these ops (they short-circuit via jumps), so
                    // `binop_of` never maps to one of them.
                    //
                    // V11-T4 PEP-659 adaptive specialization: a fast path IN FRONT
                    // of `apply_binop` for the common monomorphic operand kinds,
                    // guarded so it can never diverge from the generic result.
                    let v = self.eval_binop_adaptive(fiber, fault_ip, binop, a, b)?;
                    fiber.push(v);
                }

                Op::RangeInclusive => {
                    // Inclusive value-range `a..=b` — eager `array<number>`,
                    // ascending/step-1, byte-identical to the tree-walker's
                    // value-position `..=` materialization (shared materializer so
                    // the bounds-panic message matches `Op::Range`).
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::materialize_range(&a, &b, true, span)?;
                    fiber.push(v);
                }

                Op::RangeStepValue => {
                    // `lo hi step -- array<number>`. flags bit0 = inclusive,
                    // bit1 = step PRESENT. Delegates to the SHARED stepped
                    // materializer so direction, validation, and panic messages are
                    // byte-identical to the tree-walker's value-position `..`/`..=`.
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at);
                    let inclusive = (flags & 0b01) != 0;
                    let present = (flags & 0b10) != 0;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let step = fiber.pop();
                    let hi = fiber.pop();
                    let lo = fiber.pop();
                    let step_v = if present {
                        match step {
                            Value::Float(s) => Some(s),
                            _ => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    "range step must be a number".to_string(),
                                ))
                            }
                        }
                    } else {
                        None
                    };
                    let v = crate::interp::materialize_range_stepped(
                        &lo, &hi, inclusive, step_v, span,
                    )?;
                    fiber.push(v);
                }

                Op::RangeResolveStep => {
                    // For-range SETUP: `lo hi step -- lo hi resolved_step`. Peek
                    // lo/hi (already CHECK_NUMBERS-verified), take step, run the
                    // SHARED `resolve_step` (panics on zero/non-finite/mismatch at
                    // this op's span = the START bound's), push the resolved step.
                    let present = fiber.frame().closure.proto.chunk.read_u8(operand_at) == 1;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let step = fiber.pop();
                    // Peek lo/hi without disturbing them (they stay on the stack).
                    let hi = match fiber.peek(0) {
                        Value::Float(n) => *n,
                        _ => unreachable!("RANGE_RESOLVE_STEP hi must be a number (CHECK_NUMBERS)"),
                    };
                    let lo = match fiber.peek(1) {
                        Value::Float(n) => *n,
                        _ => unreachable!("RANGE_RESOLVE_STEP lo must be a number (CHECK_NUMBERS)"),
                    };
                    let step_v = if present {
                        match step {
                            Value::Float(s) => Some(s),
                            _ => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    "for-range step must be a number".to_string(),
                                ))
                            }
                        }
                    } else {
                        None
                    };
                    let resolved = crate::interp::resolve_step(lo, hi, step_v, span)?;
                    fiber.push(Value::Float(resolved));
                }

                Op::RangeHasNext => {
                    // For-range CONDITION: `i hi step -- ok:bool`. Direction-aware
                    // continue predicate via the SHARED `range_has_next` (positive
                    // step: i < hi / i <= hi; negative: i > hi / i >= hi). Never
                    // panics (validation done in RANGE_RESOLVE_STEP).
                    let inclusive = fiber.frame().closure.proto.chunk.read_u8(operand_at) == 1;
                    let step = fiber.pop();
                    let hi = fiber.pop();
                    let i = fiber.pop();
                    let ok = match (&i, &hi, &step) {
                        (Value::Float(i), Value::Float(hi), Value::Float(step)) => {
                            crate::interp::range_has_next(*i, *hi, *step, inclusive)
                        }
                        _ => unreachable!("RANGE_HAS_NEXT operands must be numbers"),
                    };
                    fiber.push(Value::Bool(ok));
                }

                Op::Neg | Op::Not => {
                    let a = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::apply_unop(unop_of(op), a, span)?;
                    fiber.push(v);
                }

                Op::GetLocal => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.local(slot).clone();
                    fiber.push(v);
                }
                Op::SetLocal => {
                    // Clean stack discipline: SET_LOCAL POPS the value and stores
                    // it. Assignment-as-expression `DUP`s beforehand so a copy
                    // remains as the expression's result (see `compile_assign`).
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    fiber.set_local(slot, v);
                }

                Op::GetGlobal => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_GLOBAL operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    // Resolution ORDER (matching the tree-walker's module
                    // `Environment`, a child of the builtins): consult the
                    // module-scope USER-GLOBALS first, THEN the bare builtins, ELSE
                    // the tree-walker's exact runtime message (`undefined variable
                    // '<n>'`, see `Interp::eval_expr`'s `ExprKind::Ident` arm). A user
                    // global thus SHADOWS a builtin of the same name (e.g.
                    // `import { test }` shadowing the builtin `test`), exactly as in
                    // the tree-walker.
                    // V11-T4 GET_GLOBAL_CACHED: cache the resolved value at this op's
                    // offset, guarded by the global-table VERSION. A version hit
                    // returns the cached value (skipping the lookup); a version miss
                    // (any global write bumps the version) re-resolves. Correctness:
                    // the cached value is exactly what the resolve below produces, and
                    // the version guard invalidates it on every global mutation.
                    // KILL SWITCH (V11-T5): with specialization OFF, NEVER consult or
                    // record the global cache — always re-resolve generically. The
                    // resolved value is identical either way, so generic and
                    // specialized stay byte-identical.
                    // SP8 INDEX-STABLE user-global cache: when specializing, consult
                    // the site cache for an `IndexBound { idx, struct_gen }` entry. A
                    // `struct_gen` hit reads the user-global by its STABLE IndexMap
                    // index (no string hash) — this is the regression recovery for a
                    // hot reassigned top-level `let` (a SET never bumps `struct_gen`).
                    let version = self.global_version();
                    let cache = fiber.frame().closure.proto.chunk.global_cache(fault_ip);
                    if self.specialize {
                        if let Some(idx) = cache.get_index(self.struct_gen()) {
                            fiber.push(self.user_global_value_at(idx));
                            continue;
                        }
                    }
                    if let Some(v) = cache.get(version).filter(|_| self.specialize) {
                        fiber.push(v);
                    } else if let Some((idx, v)) = self.get_user_global_full(&name) {
                        // A user global resolves by name (the cold/miss path); when
                        // specializing, RECORD its stable IndexMap index so subsequent
                        // executions of this site hit the index fast path above. We
                        // cache the INDEX (not the value), so a value reassignment is
                        // immediately visible (the next read re-reads the slot) — no
                        // thrash, no stale value.
                        if self.specialize {
                            fiber.frame().closure.proto.chunk.set_global_cache(
                                fault_ip,
                                crate::vm::adapt::GlobalCache::index_bound(idx, self.struct_gen()),
                            );
                        }
                        fiber.push(v);
                    } else if crate::interp::BUILTIN_NAMES.contains(&name.as_ref()) {
                        let v = Value::Builtin(name);
                        if self.specialize {
                            fiber.frame().closure.proto.chunk.set_global_cache(
                                fault_ip,
                                crate::vm::adapt::GlobalCache::set(v.clone(), version),
                            );
                        }
                        fiber.push(v);
                    } else {
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("undefined variable '{name}'"),
                        ));
                    }
                }

                Op::DefineGlobal => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    // The u8 mutability flag follows the u16 name const (1 = `let`,
                    // 0 = immutable `const`/`fn`/`class`/`enum`/`import`).
                    let mutable = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) != 0;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "DEFINE_GLOBAL operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    let v = fiber.pop();
                    // A REDECLARATION (the name is already a module global — e.g.
                    // `let x; let x`, `fn f; fn f`, `fn f; let f`) is the tree-walker's
                    // runtime same-scope `Environment::define` rejection, fired when the
                    // SECOND define executes. It uses `AsError::new` (NO span — span
                    // `None`), so we match byte-for-byte (message + absent span). Because
                    // this fires on EXECUTION, a redeclaration in dead/unreached code (an
                    // un-entered block, an uncalled function — those are slot-locals, not
                    // globals, anyway) never triggers, exactly like the tree-walker.
                    if self.user_globals.borrow().contains_key(name.as_ref()) {
                        return Err(Control::Panic(AsError::new(format!(
                            "'{name}' is already defined in this scope"
                        ))));
                    }
                    self.define_user_global(name, v, mutable);
                }

                Op::SetGlobal => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("SET_GLOBAL operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    // Top-level reassignment `x = …` of an EXISTING module global. A
                    // SET_LOCAL-style assignment leaves the value on the stack (an
                    // assignment is an expression yielding the assigned value), so we
                    // PEEK (clone TOS) rather than pop.
                    //
                    // RUNTIME mutability check (the single source of truth for GLOBAL
                    // assignment targets — the compiler always lowers a global-target
                    // assignment to SET_GLOBAL, never the compile-time IMMUTABLE_ERROR):
                    //   - IMMUTABLE global (`const`/`fn`/`class`/`enum`/`import`) → the
                    //     tree-walker's `cannot assign to immutable binding '<n>'`,
                    //     anchored at the TARGET span (this op's span). This fires even
                    //     when the immutable decl was in an EARLIER, separately-compiled
                    //     chunk (REPL line-to-line; a main module reassigning an import),
                    //     which the compile-time IMMUTABLE_ERROR cannot see. It is
                    //     RUNTIME-timed: only an EXECUTED store errors (a dead
                    //     `if false { k = 2 }` never runs this op), matching the
                    //     tree-walker's `Environment::assign`.
                    //   - Absent name → `cannot assign to undefined variable '<n>'`.
                    //   - Mutable global (`let`) → update in place. We do NOT bump the
                    //     global version OR `struct_gen`: a SET is not a define, so it
                    //     cannot move any index or change a cached name's target — no
                    //     cache can go stale. Keeps a hot reassignment loop cheap (no
                    //     per-iteration cache invalidation), matching the generic VM.
                    //
                    // SP8 INDEX-STABLE set cache: when specializing, consult the site
                    // cache (a distinct bytecode offset from any GET_GLOBAL, so the
                    // offset-keyed `global_caches` disambiguates GET vs SET sites). A
                    // `struct_gen` hit writes by the stable index (one `get_index_mut`,
                    // no string hash). On a miss, fall through to the name-keyed path
                    // AND record the index for next time.
                    let v = fiber.peek(0).clone();
                    let cache = fiber.frame().closure.proto.chunk.global_cache(fault_ip);
                    if self.specialize {
                        if let Some(idx) = cache.get_index(self.struct_gen()) {
                            match self.set_user_global_at(idx, v.clone()) {
                                Some(true) => continue,
                                Some(false) => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!("cannot assign to immutable binding '{name}'"),
                                    ));
                                }
                                // Out-of-range cached index (impossible while the
                                // struct_gen matches, since user-globals are never
                                // removed) — defensively fall through to re-resolve.
                                None => {}
                            }
                        }
                    }
                    match self.user_global_mutable(name.as_ref()) {
                        Some(true) => {
                            self.update_user_global(&name, v);
                            if self.specialize {
                                if let Some((idx, _)) = self.get_user_global_full(name.as_ref()) {
                                    fiber.frame().closure.proto.chunk.set_global_cache(
                                        fault_ip,
                                        crate::vm::adapt::GlobalCache::index_bound(
                                            idx,
                                            self.struct_gen(),
                                        ),
                                    );
                                }
                            }
                        }
                        Some(false) => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("cannot assign to immutable binding '{name}'"),
                            ));
                        }
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("cannot assign to undefined variable '{name}'"),
                            ));
                        }
                    }
                }

                Op::ImmutableError => {
                    // Unconditionally raise the tree-walker's immutable-binding panic.
                    // Emitted at the store position of an assignment whose target is an
                    // immutable binding (const/fn/class/enum/import/loop-var/const-pattern
                    // bind), AFTER the RHS has been evaluated — so the timing (RHS
                    // side-effects first; dead/unreached assignments never trigger),
                    // message, and span all match the tree-walker's `Environment::assign`
                    // immutable error byte-for-byte.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "IMMUTABLE_ERROR operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        format!("cannot assign to immutable binding '{name}'"),
                    ));
                }

                Op::Call | Op::CallSpread => {
                    // `Op::Call` carries a STATIC `u8` argc; `Op::CallSpread` carries
                    // none — its arguments arrived as a single runtime `Value::Array`
                    // (built by the array/spread builder ops) sitting on top of the
                    // callee `[..., callee, argsArray]`. For `CallSpread` we POP the
                    // args array and re-push its elements as individual stack slots,
                    // so the stack becomes `[..., callee, arg0, .., arg{n-1}]` — the
                    // EXACT shape `Op::Call` expects — and dispatch is shared below
                    // (arity/contracts then apply to the flattened list, byte-
                    // identical to the tree-walker's `eval_call_args` → call).
                    let argc = if matches!(op, Op::CallSpread) {
                        let args = match fiber.pop() {
                            Value::Array(a) => a,
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "CALL_SPREAD args are not an array: {}",
                                        crate::interp::type_name(&other)
                                    ),
                                ))
                            }
                        };
                        let items: Vec<Value> = args.borrow().iter().cloned().collect();
                        let n = items.len();
                        for v in items {
                            fiber.push(v);
                        }
                        n
                    } else {
                        fiber.frame().closure.proto.chunk.read_u8(operand_at) as usize
                    };
                    // The callee sits just below its `argc` arguments on the stack:
                    // `[..., callee, arg0, .., arg{argc-1}]`. Its stack index is the
                    // base where, for a Closure callee, the args become the callee
                    // frame's first local slots (the CALL convention).
                    let callee_idx = fiber.stack.len() - argc - 1;
                    match fiber.stack[callee_idx].clone() {
                        // A generator closure (`fn*` / `async fn*`) is NOT run and
                        // NOT spawned: calling it builds a NOT-STARTED Fiber for the
                        // closure (args bound into its slots, ip 0) and wraps it in a
                        // VM-backed `GeneratorHandle`, pushing a `Value::Generator`
                        // immediately. The body runs only when the consumer calls
                        // `gen.next()` (→ `GeneratorHandle::resume`), exactly like the
                        // tree-walker's `is_generator` branch of `call_function`.
                        // Both sync and async generators take this path (the async-
                        // generator yield+await fusion is V8-T5; for now we build the
                        // generator the same way). Arg binding reuses the SAME
                        // `check_call_args` the tree-walker / plain-call path uses, so
                        // arity/contract panics are byte-identical and surface eagerly
                        // at the call (the tree-walker also binds args eagerly when
                        // building the generator). AWAIT DISCIPLINE: no await here;
                        // the fiber is built synchronously and handed to the handle.
                        // A `worker fn*` (Spec B Task 6) is a STREAMING generator: its
                        // body runs in a DEDICATED isolate, consumed via a cross-thread
                        // demand-driven driver. Must precede the plain-generator arm (a
                        // `worker fn*` has BOTH flags) and the `worker fn` arm. Same
                        // `Interp::spawn_worker_stream` as the tree-walker → byte-
                        // identical. AWAIT DISCIPLINE: pop the args synchronously, then
                        // `.await` the spawn with no fiber borrow held.
                        Value::Closure(callee)
                            if callee.proto.is_worker && callee.proto.is_generator =>
                        {
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let entry_name = callee
                                .proto
                                .chunk
                                .name
                                .clone()
                                .ok_or_else(|| {
                                    Control::Panic(crate::error::AsError::at(
                                        "worker fn* must be a named top-level function"
                                            .to_string(),
                                        call_span,
                                    ))
                                })?;
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                            let gen = self
                                .interp
                                .spawn_worker_stream(&entry_name, args, call_span)
                                .await?;
                            fiber.push(gen);
                        }
                        Value::Closure(callee) if callee.proto.is_generator => {
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let what = callee.proto.chunk.name.as_deref().unwrap_or("function");
                            // Pop the args, then drop the callee value beneath them.
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                                         // Bind args (arity + per-param contracts + rest) — shared
                                         // with every other call path. A mismatch is a Tier-2
                                         // panic at the call site, eager (like the tree-walker).
                            let bound = crate::interp::check_call_args(
                                &callee.proto.params,
                                args,
                                call_span,
                                what,
                            )?;
                            // Build a NOT-STARTED one-frame Fiber for the closure and
                            // place the bound params into its slots (cell slot → cell,
                            // plain slot → stack). `Fiber::new` reserved the locals
                            // and the cell vector. We do NOT run it.
                            let mut gfiber = Fiber::new(callee);
                            gfiber.frame_mut().ret_span = call_span;
                            gfiber.frame_mut().argc = bound.supplied;
                            let cells = gfiber.frame().cells.clone();
                            for (slot, v) in bound.values.into_iter().enumerate() {
                                if let Some(cell) = &cells[slot] {
                                    *cell.borrow_mut() = v;
                                } else {
                                    gfiber.stack[slot] = v;
                                }
                            }
                            let handle = crate::coro::GeneratorHandle::new_vm(
                                gfiber,
                                Rc::downgrade(&self.rc()),
                            );
                            fiber.push(Value::Generator(Rc::new(handle)));
                        }
                        // An `async fn` closure is NOT run inline: it is scheduled
                        // eagerly (M17 model 2a), exactly like the tree-walker's
                        // `is_async` branch of `call_function`. We build a body future
                        // that re-enters the VM via `Vm::call_value` (which sets up a
                        // fresh one-frame fiber, binds args via `check_call_args`, and
                        // runs to Done), `spawn_local` it onto the current-thread
                        // LocalSet, and hand back a `Value::Future` IMMEDIATELY; the
                        // caller `await`s it later. Because `call_value` runs the arity
                        // /contract check INSIDE the spawned task, an async arity or
                        // contract violation surfaces LAZILY — it resolves into the
                        // SharedFuture and re-emerges at the `await` site — byte-
                        // identical to the tree-walker. AWAIT DISCIPLINE: the closure
                        // and its args move into the `'static` spawned task; `vm` is an
                        // owned `Rc<Vm>`; no `fiber` RefCell borrow is held across the
                        // spawn/await below.
                        // A `worker fn` closure dispatches to a pooled isolate
                        // (Workers Spec A): pop the args, build the code slice from the
                        // entry program source, ship + return a `Value::Future`. Must
                        // precede the `is_async` branch (a worker fn is not async).
                        Value::Closure(callee) if callee.proto.is_worker => {
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                            let fut = self.dispatch_worker_closure(&callee, args, call_span)?;
                            fiber.push(fut);
                        }
                        Value::Closure(callee) if callee.proto.is_async => {
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            // Pop the `argc` args into an owned vec (top of stack is
                            // the LAST arg), then drop the callee value beneath them.
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                                         // Reuse the shared M17 dance (mirrors `call_function`'s
                                         // async branch and `BoundMethod`'s): an owned `Rc<Vm>`
                                         // (Vm self-weak, installed at `Vm::new`) drives the body;
                                         // the task resolves the CELL (never a `SharedFuture` clone)
                                         // so cancel-on-drop works; the inflight guard provides
                                         // backpressure (reused from the shared interp).
                            let vm = self.rc();
                            let fut = crate::task::SharedFuture::new();
                            let cell = fut.cell();
                            let guard = self.interp.inflight_guard();
                            let handle = tokio::task::spawn_local(async move {
                                let _g = guard;
                                let r =
                                    vm.call_value(Value::Closure(callee), args, call_span).await;
                                cell.resolve(r);
                            });
                            fut.set_abort(handle.abort_handle());
                            self.interp.maybe_yield_for_inflight().await;
                            fiber.push(Value::Future(fut));
                        }
                        Value::Closure(callee) => {
                            // The call-site span anchors arity/contract/return
                            // panics exactly where the tree-walker's do.
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            // `what` mirrors the tree-walker's `func.name.as_deref()
                            // .unwrap_or("function")` so the wording matches.
                            let what = callee.proto.chunk.name.as_deref().unwrap_or("function");
                            // Pop the `argc` args into an owned vec (top of stack is
                            // the LAST arg), then drop the callee value beneath them.
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                                         // Arity + per-param contracts + rest collection, shared
                                         // verbatim with the tree-walker via `check_call_args`. On
                                         // a mismatch this returns a `Control::Panic` carrying the
                                         // identical message anchored at `call_span`.
                            let bound = crate::interp::check_call_args(
                                &callee.proto.params,
                                args,
                                call_span,
                                what,
                            )?;
                            // The args/rest array are gone from the stack; the new
                            // frame's window starts where the callee value was.
                            let slot_base = callee_idx;
                            let slot_count = callee.proto.chunk.slot_count as usize;
                            // Allocate cells, then place each bound param into its
                            // slot (cell slot → cell; plain slot → stack). Reserve
                            // the remaining locals as Nil so the window is full.
                            let cells = super::fiber::alloc_cells(
                                slot_count,
                                &callee.proto.chunk.cell_slots,
                            );
                            fiber.stack.resize(slot_base + slot_count, Value::Nil);
                            let supplied = bound.supplied;
                            for (slot, v) in bound.values.into_iter().enumerate() {
                                if let Some(cell) = &cells[slot] {
                                    *cell.borrow_mut() = v;
                                } else {
                                    fiber.stack[slot_base + slot] = v;
                                }
                            }
                            // SP3 §B: one logical-call increment per frame push
                            // (matches the tree-walker's one-per-`run_body`); the
                            // matching decrement is in `return_from_frame`. Over the
                            // limit → the clean Tier-2 panic anchored at the call.
                            self.enter_frame_depth(call_span)?;
                            fiber.frames.push(super::fiber::CallFrame {
                                closure: callee,
                                ip: 0,
                                slot_base,
                                cells,
                                ret_span: call_span,
                                // A plain in-VM function/closure call is never a
                                // method frame; only `invoke_compiled_method` sets a
                                // `def_class` (so `super` is unavailable here, which
                                // is correct — `super` only appears in method bodies).
                                def_class: None,
                                argc: supplied,
                            });
                            // Continue the loop in the new frame (the run loop reads
                            // `fiber.frame()` at the top of each iteration). RETURN
                            // pops this frame and restores the caller.
                        }
                        other => {
                            // Native callee (Builtin/Function/Class/BoundMethod/...):
                            // delegate to the VM-aware `call_value`, which routes a
                            // VM class constructor / VM bound method to COMPILED code
                            // (V9) and everything else to the shared `Interp`
                            // dispatch. Pop the args and the callee into owned locals
                            // BEFORE the await so no borrow of `fiber` is held across
                            // the suspension point (`await_holding_refcell_ref` stays
                            // clean).
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            let _callee = fiber.pop(); // the Value at callee_idx
                            let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let result = self.call_value(other, args, span).await?;
                            fiber.push(result);
                        }
                    }
                }

                Op::CallMethod => {
                    // A method call `recv.<name>(args)`. Mirrors the tree-walker's
                    // `eval_chain` Call arm for a `Member` callee: the schema
                    // fluent-method hook, else `read_member(recv, name)` →
                    // `call_value`. The receiver sits below its args on the stack.
                    //
                    // ORDERING NOTE: the tree-walker reads the member BEFORE
                    // evaluating the call args (so a member-read error preempts arg
                    // side effects). Here the compiler already evaluated the args
                    // (they are on the stack), so a member-read error does NOT
                    // preempt arg side effects. This sub-case (a side-effecting arg
                    // AND an erroring member read) is the documented deviation
                    // deferred to the full V9 method-call slice; the generator
                    // consumer API (`gen.next(v)`/`gen.close()`) and the rest of the
                    // gated corpus do not hit it. Everything else is byte-identical.
                    let name = match &fiber.frame().closure.proto.chunk.consts
                        [fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize]
                    {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("CALL_METHOD name is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let argc = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) as usize;
                    // Pop the args (top is the LAST arg), then the receiver beneath.
                    let mut args = vec![Value::Nil; argc];
                    for slot in args.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let recv = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // The entire dispatch (schema hook → IC compiled-method fast path
                    // → `read_member`→`call_value` fallback) is shared with
                    // `Op::CallMethodSpread` — see `dispatch_method`. It either pushes
                    // the result or pushes a frame (the IC in-frame fast path) and lets
                    // the run loop continue.
                    self.dispatch_method(fiber, recv, &name, args, fault_ip, span)
                        .await?;
                }

                Op::CallMethodSpread => {
                    // A method call `recv.<name>(...args)` whose argument list contains
                    // a spread (dynamic arity). Mirrors `Op::CallMethod` EXACTLY for
                    // dispatch — the only difference is how the arg list is obtained:
                    // the args arrived as a single runtime `Value::Array` (built by the
                    // array/spread builder ops), sitting on top of the receiver
                    // `[..., recv, argsArray]`. Pop the args array and flatten it into
                    // a positional `Vec`, then pop the receiver — yielding the SAME
                    // `(recv, args)` shape `Op::CallMethod` produces — and dispatch via
                    // the shared `dispatch_method`. Arity/contracts apply to the
                    // FLATTENED list, byte-identical to the tree-walker's
                    // `eval_call_args` (spread flatten) → method dispatch.
                    //
                    // ORDERING NOTE: identical to `Op::CallMethod` — the compiler
                    // already evaluated the receiver and the (spread-flattened) args
                    // onto the stack, so a member-read error does NOT preempt arg side
                    // effects. This is the SAME documented deviation as `Op::CallMethod`.
                    let name = match &fiber.frame().closure.proto.chunk.consts
                        [fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize]
                    {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "CALL_METHOD_SPREAD name is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    // Pop the runtime args array (built by NEW_ARRAY + spread ops) and
                    // re-materialize its elements as a positional `Vec`. (The builder
                    // always produces a `Value::Array`; a non-array OPERAND was already
                    // rejected by `SPREAD_ARGS` with the byte-identical message.)
                    let args = match fiber.pop() {
                        Value::Array(a) => a.borrow().iter().cloned().collect::<Vec<_>>(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "CALL_METHOD_SPREAD args are not an array: {}",
                                    crate::interp::type_name(&other)
                                ),
                            ))
                        }
                    };
                    let recv = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    self.dispatch_method(fiber, recv, &name, args, fault_ip, span)
                        .await?;
                }

                Op::Template => {
                    // Pop `n` parts (pushed left-to-right) and concatenate their
                    // string coercions in source order. The coercion is exactly
                    // the tree-walker's `Value::to_string()` (the `Display` impl
                    // shared with `print`), so a template interpolating any value
                    // renders byte-identically to `ExprKind::Template`.
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut parts = vec![Value::Nil; n];
                    for slot in parts.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let mut out = String::new();
                    for v in &parts {
                        out.push_str(&v.to_string());
                    }
                    fiber.push(Value::Str(out.into()));
                }

                Op::Jump => {
                    // Unconditional relative jump. The displacement is measured
                    // from the byte AFTER the operand to the target (see
                    // `Chunk::patch_jump`/`emit_loop`). At this point we have
                    // already advanced `ip` past the opcode and its 2-byte
                    // operand, so `fiber.frame().ip == operand_at + 2` is exactly
                    // that base; add the signed displacement to land on target.
                    let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                    let base = fiber.frame().ip as isize;
                    fiber.frame_mut().ip = (base + disp as isize) as usize;
                }
                Op::Loop => {
                    // Unconditional backward (relative) jump used for loop
                    // back-edges. Identical mechanics to `Op::Jump` — the
                    // displacement (negative for a real backward jump) is measured
                    // from the byte AFTER the operand to the target (see
                    // `Chunk::emit_loop`).
                    let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                    let base = fiber.frame().ip as isize;
                    fiber.frame_mut().ip = (base + disp as isize) as usize;
                }
                Op::JumpIfFalse => {
                    // Pop the tested value; jump iff it is falsy. Short-circuit
                    // lowering `DUP`s the operand beforehand so the un-tested copy
                    // survives as the expression's result when we jump.
                    let v = fiber.pop();
                    if !v.is_truthy() {
                        let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfTrue => {
                    // Pop the tested value; jump iff it is truthy.
                    let v = fiber.pop();
                    if v.is_truthy() {
                        let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfNotNil => {
                    // Pop the tested value; jump iff it is NOT `nil`. Mirrors the
                    // tree-walker's `??` test (`l == Value::Nil` selects the RHS;
                    // anything else keeps the left), so the jump fires on "keep
                    // the non-nil left operand".
                    let v = fiber.pop();
                    if v != Value::Nil {
                        let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfArgSupplied => {
                    // Default-parameter prologue guard. If the caller SUPPLIED this
                    // positional param (frame `argc` > param-index), jump forward
                    // past its default-eval code; otherwise fall through and run
                    // the default. Touches no operand stack. The i16 jump offset is
                    // the SECOND operand (after the u16 param-index), and `ip` is
                    // already past the whole instruction.
                    let chunk = &fiber.frame().closure.proto.chunk;
                    let param = chunk.read_u16(operand_at) as usize;
                    let disp = chunk.read_i16(operand_at + 2);
                    if fiber.frame().argc > param {
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::CheckParam => {
                    // Contract-check the just-evaluated default value (TOS, left in
                    // place) against the param's declared type, byte-identical to
                    // the tree-walker's default contract (same message; span = the
                    // frame's call site `ret_span`). Untyped params emit no
                    // CHECK_PARAM, so a type is always present here.
                    let param = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let span = fiber.frame().ret_span;
                    let ty = fiber.frame().closure.proto.params[param].ty.clone();
                    if let Some(ty) = ty {
                        let v = fiber.peek(0).clone();
                        if !crate::interp::check_type(&v, &ty) {
                            return Err(crate::interp::contract_panic(&ty, &v, span));
                        }
                    }
                }

                Op::NewArray => {
                    // Pop `n` elements (pushed in source order, so the last
                    // pushed is on top) into a Vec preserving source order, then
                    // push `Value::Array`. Matches the tree-walker's
                    // `ExprKind::Array` construction (`Rc<RefCell<Vec>>`).
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut values = vec![Value::Nil; n];
                    for slot in values.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    fiber.push(Value::Array(crate::value::ArrayCell::new(values)));
                }

                Op::NewObject => {
                    // Pop `n` (key, value) pairs. Each pair was pushed key-first
                    // then value, and the pairs were pushed in source order, so
                    // the stack top-down is: vN, kN, …, v1, k1. Pop into a
                    // source-order list, then insert into an `IndexMap` in source
                    // order — a later duplicate key overwrites the value but keeps
                    // the first-seen position (IndexMap semantics), byte-identical
                    // to the tree-walker's `ExprKind::Object`.
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut pairs: Vec<(Rc<str>, Value)> = vec![(Rc::from(""), Value::Nil); n];
                    for slot in pairs.iter_mut().rev() {
                        let value = fiber.pop();
                        let key = match fiber.pop() {
                            Value::Str(s) => s,
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("NEW_OBJECT key is not a string constant: {other:?}"),
                                ))
                            }
                        };
                        *slot = (key, value);
                    }
                    let mut map = indexmap::IndexMap::with_capacity(n);
                    for (k, v) in pairs {
                        map.insert(k.to_string(), v);
                    }
                    // Assign the object's hidden-class shape from its final ordered
                    // keys (V11-T2). Pure metadata — does not change behavior.
                    let cell = crate::value::ObjectCell::new(map);
                    let shape = self.object_shape_for(cell.map.borrow().keys().map(|s| s.as_str()));
                    cell.shape.set(shape);
                    fiber.push(Value::Object(cell));
                }

                Op::NewMap => {
                    // Push a fresh, empty `Value::Map`. The `#{…}` builder runs one
                    // `MAP_ENTRY` per entry after this (or nothing for `#{}`).
                    let cell = crate::value::MapCell::new(indexmap::IndexMap::new());
                    fiber.push(Value::Map(cell));
                }

                Op::MapEntry => {
                    // `[map, key, val] -- [map]` — convert `key` to a `MapKey` and
                    // insert later-wins into the builder `map`. Byte-identical to the
                    // tree-walker's `ExprKind::Map`: an unhashable key is the SAME
                    // Tier-2 panic `cannot use {type} as a map key`, anchored at this
                    // op's span (the key's trivia-trimmed code span).
                    let val = fiber.pop();
                    let key_val = fiber.pop();
                    let key = match crate::value::MapKey::from_value(&key_val) {
                        Some(k) => k,
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "cannot use {} as a map key",
                                    crate::interp::type_name(&key_val)
                                ),
                            ))
                        }
                    };
                    match fiber.peek(0) {
                        Value::Map(m) => {
                            m.borrow_mut().insert(key, val);
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "MAP_ENTRY target is not a map: {}",
                                    crate::interp::type_name(other)
                                ),
                            ))
                        }
                    }
                }

                Op::Spread | Op::SpreadArgs => {
                    // `[arr, operand] -- [arr]` — flatten the spread `operand` (an
                    // Array) into the under-construction array `arr` below it.
                    // Mirrors the tree-walker's `ExprKind::Array` / `eval_call_args`
                    // spread arm: a non-array is the SAME Tier-2 panic, anchored at
                    // this op's span (the operand's trivia-trimmed code span). The
                    // ONLY difference between SPREAD and SPREAD_ARGS is the message
                    // ("into an array" vs "as call arguments").
                    let operand = fiber.pop();
                    match operand {
                        Value::Array(src) => {
                            // Clone elements out FIRST so a self-spread (`[...a]`
                            // where `arr` aliased `a`) cannot observe a borrow
                            // conflict, then extend the builder array.
                            let items: Vec<Value> = src.borrow().iter().cloned().collect();
                            match fiber.peek(0) {
                                Value::Array(arr) => arr.borrow_mut().extend(items),
                                other => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "SPREAD target is not an array: {}",
                                            crate::interp::type_name(other)
                                        ),
                                    ))
                                }
                            }
                        }
                        other => {
                            let msg = if matches!(op, Op::SpreadArgs) {
                                format!(
                                    "can only spread an array as call arguments, got {}",
                                    crate::interp::type_name(&other)
                                )
                            } else {
                                format!(
                                    "can only spread an array into an array, got {}",
                                    crate::interp::type_name(&other)
                                )
                            };
                            return Err(self.panic_at(fiber, fault_ip, msg));
                        }
                    }
                }

                Op::AppendArray => {
                    // `[arr, item] -- [arr]` — push one `item` onto the builder
                    // array `arr` below it.
                    let item = fiber.pop();
                    match fiber.peek(0) {
                        Value::Array(arr) => arr.borrow_mut().push(item),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "APPEND_ARRAY target is not an array: {}",
                                    crate::interp::type_name(other)
                                ),
                            ))
                        }
                    }
                }

                Op::AppendObject => {
                    // `[obj, key, val] -- [obj]` — insert `key -> val` into the
                    // builder object `obj`. Later-wins + first-position (IndexMap
                    // insert), byte-identical to the tree-walker's `ExprKind::Object`.
                    let val = fiber.pop();
                    let key = match fiber.pop() {
                        Value::Str(s) => s,
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("APPEND_OBJECT key is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    match fiber.peek(0) {
                        Value::Object(obj) => {
                            obj.borrow_mut().insert(key.to_string(), val);
                            // A new key may have been added → resync the shape.
                            let obj = obj.clone();
                            self.resync_object_shape(&obj);
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "APPEND_OBJECT target is not an object: {}",
                                    crate::interp::type_name(other)
                                ),
                            ))
                        }
                    }
                }

                Op::SpreadObject => {
                    // `[obj, operand] -- [obj]` — merge the operand object's entries
                    // into the builder object `obj`. Mirrors the tree-walker's
                    // `ExprKind::Object` spread arm: a non-object is the SAME Tier-2
                    // panic at this op's span; entries insert later-wins/first-pos.
                    let operand = fiber.pop();
                    match operand {
                        Value::Object(src) => {
                            // Snapshot the source entries FIRST (avoids a borrow
                            // conflict if `obj` aliases `src` via a self-spread).
                            let entries: Vec<(String, Value)> = src
                                .borrow()
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect();
                            match fiber.peek(0) {
                                Value::Object(obj) => {
                                    {
                                        let mut m = obj.borrow_mut();
                                        for (k, v) in entries {
                                            m.insert(k, v);
                                        }
                                    }
                                    // The merge may have added keys → resync shape.
                                    let obj = obj.clone();
                                    self.resync_object_shape(&obj);
                                }
                                other => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "SPREAD_OBJECT target is not an object: {}",
                                            crate::interp::type_name(other)
                                        ),
                                    ))
                                }
                            }
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "can only spread an object into an object, got {}",
                                    crate::interp::type_name(&other)
                                ),
                            ))
                        }
                    }
                }

                Op::GetIndex => {
                    // `obj idx -- obj[idx]`. The two operands were pushed
                    // obj-then-idx, so pop idx first. The shared `index_get`
                    // dispatch (with the tree-walker) anchors every panic at the
                    // op's span; the VM has a single instruction span, so it is
                    // passed for both the receiver-span and index-span parameters.
                    let idx = fiber.pop();
                    let obj = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::index_get(&obj, &idx, span, span)?;
                    fiber.push(v);
                }

                Op::SetIndex => {
                    // `obj idx val -- val` — store `obj[idx] = val`. The operands
                    // were pushed obj-then-idx-then-val, so pop val, idx, obj. The
                    // shared `index_set` dispatch (with the tree-walker) anchors
                    // every panic at the op's span; the VM has a single instruction
                    // span, so it is passed for both the receiver-span and
                    // index-span parameters. Leaves the assigned value on the stack
                    // (assignment is an expression).
                    let val = fiber.pop();
                    let idx = fiber.pop();
                    let obj = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::index_set(&obj, &idx, val, span, span)?;
                    // Setting `obj[key] = v` on an object may have ADDED a key →
                    // transition the shape (reassigning an existing key is a no-op).
                    if let Value::Object(cell) = &obj {
                        self.resync_object_shape(cell);
                    }
                    fiber.push(v);
                }

                Op::GetProp | Op::GetPropOpt => {
                    // `obj -- obj.<name>` (the optional form short-circuits to
                    // `nil` when the receiver is `nil`). `read_member` is the SAME
                    // member-access dispatch the tree-walker runs (fields, methods
                    // → BoundMethod, enum variants, native handles, nil-receiver
                    // errors), so the two engines cannot drift.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_PROP operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let obj = fiber.pop();
                    if op == Op::GetPropOpt && obj == Value::Nil {
                        // `?.` short-circuit guard: a nil receiver never consults
                        // the IC (and never resolves a field), matching the generic
                        // path's nil short-circuit exactly.
                        fiber.push(Value::Nil);
                    } else {
                        let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                        // Try the field inline cache (fast path for FIELD reads on a
                        // shaped Object/Instance). On a hit it returns the cached
                        // field's value, which is byte-identical to `vm_read_member`
                        // (whose Object/Instance field arm clones the same stored
                        // value). On a miss it returns `None` and we fall to the SAME
                        // generic member read — which also handles methods (→
                        // BoundMethod), non-shaped receivers, enum/native/nil, etc.
                        // Resolve `proto` out of the fiber so the chunk borrow does
                        // not collide with the later `fiber.push`.
                        let proto = fiber.frame().closure.proto.clone();
                        // KILL SWITCH (V11-T5): only consult/record the field IC
                        // when specialization is ON. Generic mode always takes the
                        // shared `vm_read_member` path (same value, byte-identical).
                        let cached = if self.specialize {
                            self.ic_get_field(&proto.chunk, fault_ip, &obj, &name)
                        } else {
                            None
                        };
                        let v = match cached {
                            Some(v) => v,
                            None => self.vm_read_member(&obj, &name, span)?,
                        };
                        fiber.push(v);
                    }
                }

                Op::CheckNumbers => {
                    // Peek-only bounds guard for for-range: the top two stack
                    // values (start below, end on top) must both be numbers.
                    // Leaves them in place so the surrounding lowering can store
                    // them into slots. The op's span is the START bound's span, so
                    // the panic is byte-identical to the tree-walker's
                    // `Stmt::ForRange` ("for-range bounds must be numbers" at
                    // `start.span`).
                    let end_ok = matches!(fiber.peek(0), Value::Float(_));
                    let start_ok = matches!(fiber.peek(1), Value::Float(_));
                    if !(end_ok && start_ok) {
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            "for-range bounds must be numbers".to_string(),
                        ));
                    }
                }

                Op::IterSnapshot => {
                    // Materialize the SYNC for-of snapshot from the iterable on
                    // TOS. Byte-identical to the tree-walker's `Stmt::ForOf` (sync,
                    // `for_await == false`) `items` build: an `Array` snapshots a
                    // CLONE of its current elements (so the iteration is fixed even
                    // if the body mutates the source array), a `Str` snapshots its
                    // chars each as a 1-char string, and ANYTHING ELSE — including
                    // object/map/set, which are NOT iterable in sync for-of —
                    // raises the Tier-2 panic at this op's span (the iterable
                    // expression's trivia-trimmed code span), exactly like
                    // `AsError::at(format!("value of type {} is not iterable", ...))`.
                    let iterable = fiber.pop();
                    let items: Vec<Value> = match iterable {
                        Value::Array(arr) => arr.borrow().clone(),
                        Value::Str(s) => s
                            .chars()
                            .map(|c| Value::Str(c.to_string().into()))
                            .collect(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "value of type {} is not iterable",
                                    crate::interp::type_name(&other)
                                ),
                            ))
                        }
                    };
                    fiber.push(Value::Array(crate::value::ArrayCell::new(items)));
                }

                Op::ArrayLen => {
                    // Pop a (compiler-produced) snapshot array and push its element
                    // count as a `Number`. The operand is never user input — the
                    // compiler emits this only over an `IterSnapshot` result — so a
                    // non-array is a compiler bug surfaced as a Tier-2 panic.
                    let v = fiber.pop();
                    match v {
                        Value::Array(arr) => {
                            let len = arr.borrow().len();
                            fiber.push(Value::Float(len as f64));
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_LEN operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::Closure => {
                    // Build a closure over a nested proto, capturing its upvalues per
                    // the proto's capture plan (`proto.chunk.upvalues`, indexed by
                    // upvalue number):
                    //   - ParentLocal { slot, by_value: false }: BY REFERENCE — clone
                    //     the CURRENT frame's cell `Cc` for that slot, so the closure
                    //     sees later mutation. The resolver guarantees a `mutated`
                    //     captured local is a cell slot, so `cells[slot]` is `Some`; a
                    //     `None` is a compiler/resolver bug (clear panic).
                    //   - ParentLocal { slot, by_value: true } (SP8 #136): BY VALUE —
                    //     the source binding is never reassigned, so its slot is a PLAIN
                    //     stack local (no cell in the declaring frame). Copy the slot's
                    //     value into a FRESH private cell owned solely by this closure.
                    //     Per-iteration loop freshness is automatic: each iteration's
                    //     Op::Closure copies that iteration's slot value. Byte-identical
                    //     to a shared cell (the value can never change after capture).
                    //   - ParentUpvalue(idx): clone the CURRENT closure's upvalue cell
                    //     (a transitive capture; keeps the source's representation).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let proto = fiber.frame().closure.proto.chunk.protos[idx].clone();
                    let mut upvalues = Vec::with_capacity(proto.chunk.upvalues.len());
                    for desc in &proto.chunk.upvalues {
                        let cell = match *desc {
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentLocal {
                                slot,
                                by_value: false,
                            } => fiber
                                .frame()
                                .cells
                                .get(slot as usize)
                                .and_then(|c| c.as_ref())
                                .unwrap_or_else(|| {
                                    panic!(
                                        "CLOSURE captures parent local slot {slot} that is not a cell (compiler/resolver bug)"
                                    )
                                })
                                .clone(),
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentLocal {
                                slot,
                                by_value: true,
                            } => {
                                let v = fiber.local(slot as usize).clone();
                                gcmodule::Cc::new(std::cell::RefCell::new(v))
                            }
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentUpvalue(up) => {
                                fiber.frame().closure.upvalues[up as usize].clone()
                            }
                        };
                        upvalues.push(cell);
                    }
                    let closure = crate::vm::value_ext::Closure::with_upvalues(proto, upvalues);
                    fiber.push(Value::Closure(closure));
                }

                Op::GetLocalCell => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.get_local_cell(slot);
                    fiber.push(v);
                }
                Op::SetLocalCell => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    fiber.set_local_cell(slot, v);
                }
                Op::FreshCell => {
                    // Install a brand-new heap cell into this slot, dropping the
                    // frame's ref to the previous cell (any closure that captured
                    // it keeps its own `Rc`, so it retains that iteration's value).
                    // Emitted at the top of each loop iteration for per-iteration
                    // capture freshness.
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    fiber.fresh_cell(slot);
                }

                Op::Import => {
                    // Read the descriptor (cloned out of the chunk so no chunk borrow
                    // is held). For a `std/*` source resolve via the SAME
                    // `load_std_module` the tree-walker uses (V12-T1); for a FILE
                    // source (`./mod`, `../mod`, …) resolve+compile/load+run the file
                    // module on the VM and bind its `export`ed values (V12-T4). The op
                    // leaves nothing on the stack — byte-identical to the tree-walker's
                    // `Stmt::Import` arm.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let desc = fiber.frame().closure.proto.chunk.imports[idx].clone();
                    let source = desc.source().to_string();

                    // Resolve the export accessor once: a closure that, given an export
                    // name, returns its value (or None if absent), plus an ordered list
                    // of export names for the namespace form. For std we wrap the
                    // `ModuleEntry`; for a file module we use its IndexMap directly.
                    //
                    // SP6 §6: the SHARED `classify_specifier` drives the split (the
                    // SAME helper the tree-walker's `Stmt::Import` uses, so the two
                    // engines route identically). `Std` → the static registry;
                    // `Relative`/`Package` → the SAME `load_file_module` (a
                    // package's resolved `target` is an absolute path, so
                    // `module_dir.join(target)` yields `target` unchanged and
                    // package-internal `./` imports still resolve within the store
                    // root); `UnknownPackage` → a Tier-2 error, message identical
                    // to the tree-walker. The owned `target` string is taken out of
                    // the resolver borrow before this `.await`.
                    let exports: ModuleExports =
                        match self.interp.classify_specifier(&source) {
                            crate::interp::SpecifierKind::Std => {
                                let entry = self.interp.import_std(&source)?;
                                // Materialize the std module's exports into an ordered
                                // map so both import forms share one code path. The std
                                // export set is unordered (a HashSet); order is
                                // irrelevant for the named form and matches the
                                // tree-walker's unordered namespace object.
                                let mut m = indexmap::IndexMap::new();
                                for name in entry.exports.borrow().iter() {
                                    m.insert(
                                        name.clone(),
                                        entry.env.get(name).unwrap_or(Value::Nil),
                                    );
                                }
                                Rc::new(RefCell::new(m))
                            }
                            crate::interp::SpecifierKind::Relative(_) => {
                                self.load_file_module(&source, fault_ip, fiber).await?
                            }
                            crate::interp::SpecifierKind::Package { target, .. } => {
                                let target = target.to_string_lossy().into_owned();
                                self.load_file_module(&target, fault_ip, fiber).await?
                            }
                            crate::interp::SpecifierKind::UnknownPackage(key) => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "unknown package '{key}' — add it with 'ascript add'"
                                    ),
                                ));
                            }
                        };

                    match desc {
                        crate::vm::chunk::ImportDesc::Named { source, names } => {
                            for (name, slot, is_cell, is_global) in names {
                                let v = {
                                    let ex = exports.borrow();
                                    match ex.get(&name) {
                                        Some(v) => v.clone(),
                                        None => {
                                            drop(ex);
                                            return Err(self.panic_at(
                                                fiber,
                                                fault_ip,
                                                format!("module '{source}' has no export '{name}'"),
                                            ));
                                        }
                                    }
                                };
                                if is_global {
                                    // An imported name is an IMMUTABLE module global
                                    // (tree-walker `define(..., false)`).
                                    self.define_user_global(Rc::from(name.as_str()), v, false);
                                } else if is_cell {
                                    fiber.set_local_cell(slot as usize, v);
                                } else {
                                    fiber.set_local(slot as usize, v);
                                }
                            }
                        }
                        crate::vm::chunk::ImportDesc::Namespace {
                            alias,
                            slot,
                            is_cell,
                            is_global,
                            ..
                        } => {
                            let map = exports.borrow().clone();
                            let ns = Value::Object(crate::value::ObjectCell::new(map));
                            if is_global {
                                // A namespace alias is an IMMUTABLE module global.
                                self.define_user_global(Rc::from(alias.as_str()), ns, false);
                            } else if is_cell {
                                fiber.set_local_cell(slot as usize, ns);
                            } else {
                                fiber.set_local(slot as usize, ns);
                            }
                        }
                    }
                }

                Op::DefineExport => {
                    // `value -- `. Pop the exported binding's value and record it
                    // under its name (`consts[idx]`, a Str) in the CURRENT module's
                    // export map. Mirrors the tree-walker's `Stmt::Export`. When the
                    // top-level chunk is the entry program the recorded map is a
                    // throwaway (its exports are unused), exactly as the tree-walker
                    // discards the main program's `current_exports`.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.to_string(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "DEFINE_EXPORT name const is not a string: {}",
                                    crate::interp::type_name(other)
                                ),
                            ))
                        }
                    };
                    let v = fiber.pop();
                    self.module_exports.borrow().borrow_mut().insert(name, v);
                }

                Op::CheckArrayDestructure => {
                    // Peek the RHS on TOS and validate it is an Array, exactly like
                    // the tree-walker's `Stmt::LetDestructure` type check (which runs
                    // ONCE before binding any name). Leaves the source in place so the
                    // surrounding lowering can stash it in a temp slot.
                    if !matches!(fiber.peek(0), Value::Array(_)) {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("cannot destructure a non-array value of type {t}"),
                        ));
                    }
                }

                Op::CheckObjectDestructure => {
                    // Peek the RHS on TOS and validate it is an Object or Instance,
                    // exactly like the tree-walker's `Stmt::LetDestructureObject` type
                    // check. Leaves the source in place.
                    if !matches!(fiber.peek(0), Value::Object(_) | Value::Instance(_)) {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("cannot destructure a non-object value of type {t}"),
                        ));
                    }
                }

                Op::ArrayElem => {
                    // `src -- src[index]`. Pop the (already-validated) array and push
                    // the element at `index`, or `nil` for an out-of-bounds position
                    // (positions past the length bind nil — `items.get(i).cloned()
                    // .unwrap_or(Value::Nil)`).
                    let index = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let src = fiber.pop();
                    match src {
                        Value::Array(arr) => {
                            let v = arr.borrow().get(index).cloned().unwrap_or(Value::Nil);
                            fiber.push(v);
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_ELEM operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::ObjectKey => {
                    // `src -- src[key]` where `key = consts[idx]`. Pop the
                    // (already-validated) Object/Instance and push the value under
                    // `key`, or `nil` if absent. Mirrors the tree-walker's destructure
                    // `get` closure EXACTLY: an Instance reads only its `fields` (it
                    // does NOT fall back to methods like `read_member` would).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_KEY operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let src = fiber.pop();
                    let v = match src {
                        Value::Object(o) => {
                            o.borrow().get(key.as_ref()).cloned().unwrap_or(Value::Nil)
                        }
                        Value::Instance(i) => i
                            .borrow()
                            .fields
                            .get(key.as_ref())
                            .cloned()
                            .unwrap_or(Value::Nil),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_KEY operand is not an object: {other:?}"),
                            ))
                        }
                    };
                    fiber.push(v);
                }

                Op::ArrayRest => {
                    // `src -- src[start..]`. Pop the (already-validated) array and push
                    // a NEW array of its elements from `start` to the end — the `...rest`
                    // collector (`items.iter().skip(names.len())`).
                    let start = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let src = fiber.pop();
                    match src {
                        Value::Array(arr) => {
                            let tail: Vec<Value> =
                                arr.borrow().iter().skip(start).cloned().collect();
                            fiber.push(Value::Array(crate::value::ArrayCell::new(tail)));
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_REST operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::ObjectRest => {
                    // `src -- leftover` where `consts[idx]` is an Array of the bound
                    // key strings. Pop the (already-validated) Object/Instance and push
                    // a NEW object of its entries whose key is NOT bound, in source
                    // order — the object-rest collector (excludes already-bound SOURCE
                    // keys).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let bound: std::collections::HashSet<Rc<str>> =
                        match &fiber.frame().closure.proto.chunk.consts[idx] {
                            Value::Array(keys) => keys
                                .borrow()
                                .iter()
                                .filter_map(|v| match v {
                                    Value::Str(s) => Some(s.clone()),
                                    _ => None,
                                })
                                .collect(),
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("OBJECT_REST operand is not a key array: {other:?}"),
                                ))
                            }
                        };
                    let src = fiber.pop();
                    let mut remaining: indexmap::IndexMap<String, Value> =
                        indexmap::IndexMap::new();
                    match src {
                        Value::Object(o) => {
                            for (k, v) in o.borrow().iter() {
                                if !bound.contains(k.as_str()) {
                                    remaining.insert(k.clone(), v.clone());
                                }
                            }
                        }
                        Value::Instance(i) => {
                            for (k, v) in i.borrow().fields.iter() {
                                if !bound.contains(k.as_str()) {
                                    remaining.insert(k.clone(), v.clone());
                                }
                            }
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_REST operand is not an object: {other:?}"),
                            ))
                        }
                    }
                    fiber.push(Value::Object(crate::value::ObjectCell::new(remaining)));
                }

                Op::MatchArray => {
                    // `subject -- ok:bool`. Pop the subject; push whether it is an
                    // Array whose length is exactly `len` (exact == 1) or at least
                    // `len` (exact == 0, the `...rest` case). A non-array → false.
                    // Mirrors the tree-walker's `Pattern::Array` length/type guard.
                    let len = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let exact = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) == 1;
                    let subject = fiber.pop();
                    let ok = match &subject {
                        Value::Array(a) => {
                            let n = a.borrow().len();
                            if exact {
                                n == len
                            } else {
                                n >= len
                            }
                        }
                        _ => false,
                    };
                    fiber.push(Value::Bool(ok));
                }

                Op::MatchObject => {
                    // `subject -- ok:bool`. Pop the subject; push whether it is an
                    // Object or Instance. Mirrors the head guard of the tree-walker's
                    // `Pattern::Object` (any other value is a structural mismatch).
                    let subject = fiber.pop();
                    let ok = matches!(subject, Value::Object(_) | Value::Instance(_));
                    fiber.push(Value::Bool(ok));
                }

                Op::MatchHasKey => {
                    // `subject -- ok:bool`. Pop the subject (an Object/Instance per
                    // `MatchObject`) and push whether it has the field `consts[idx]`.
                    // Mirrors the per-entry `fields.get(key)` presence check. Popping
                    // (not peeking) avoids orphaning the subject on a missing-key
                    // fail-jump; the matched path reloads the subject temp.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "MATCH_HAS_KEY operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    let subject = fiber.pop();
                    let ok = match &subject {
                        Value::Object(o) => o.borrow().contains_key(key.as_ref()),
                        Value::Instance(i) => i.borrow().fields.contains_key(key.as_ref()),
                        _ => false,
                    };
                    fiber.push(Value::Bool(ok));
                }

                Op::MatchRange => {
                    // `subject lo hi step -- ok:bool` (step on top). flags bit0 =
                    // inclusive, bit1 = step PRESENT. Pop all four; push whether the
                    // subject is a Number that matches the range. With step OMITTED
                    // (placeholder `nil`) this is the plain in-bounds test; with step
                    // PRESENT it is strided membership (spec §3.7) anchored at `lo`,
                    // via the SHARED `resolve_step` (validates → PANICS on
                    // zero/non-finite/mismatch, byte-identical to iteration) +
                    // `range_pattern_contains`. A non-number subject OR bound → false
                    // (a non-panic mismatch), mirroring the tree-walker exactly.
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at);
                    let inclusive = (flags & 0b01) != 0;
                    let present = (flags & 0b10) != 0;
                    let step = fiber.pop();
                    let hi = fiber.pop();
                    let lo = fiber.pop();
                    let subject = fiber.pop();
                    let ok = match (&subject, &lo, &hi) {
                        (Value::Float(n), Value::Float(lo), Value::Float(hi)) => {
                            let step_v = if present {
                                match step {
                                    Value::Float(s) => Some(s),
                                    _ => {
                                        return Err(self.panic_at(
                                            fiber,
                                            fault_ip,
                                            "range step must be a number".to_string(),
                                        ))
                                    }
                                }
                            } else {
                                None
                            };
                            // Validate an EXPLICIT step (PANICS on a bad step, at
                            // this op's span = the START bound's), then test
                            // membership. A plain pattern (step omitted) keeps its
                            // no-stride behavior via the raw `Option`.
                            if step_v.is_some() {
                                let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                                crate::interp::resolve_step(*lo, *hi, step_v, span)?;
                            }
                            crate::interp::range_pattern_contains(*n, *lo, *hi, step_v, inclusive)
                        }
                        _ => false,
                    };
                    fiber.push(Value::Bool(ok));
                }

                Op::MatchNoArm => {
                    // No arm matched: raise the Tier-2 panic at this op's span (the
                    // `MatchExpr`'s code span), byte-identical to the tree-walker's
                    // `AsError::at("no matching arm in match expression", expr.span)`.
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        "no matching arm in match expression".to_string(),
                    ));
                }

                Op::GetUpvalue => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.frame().closure.upvalues[idx].borrow().clone();
                    fiber.push(v);
                }
                Op::SetUpvalue => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    *fiber.frame().closure.upvalues[idx].borrow_mut() = v;
                }

                Op::Return => {
                    // Pop the result and unwind one frame, returning that value to
                    // the caller (or ending the program if this was the root frame).
                    // The shared `return_from_frame` helper applies the return-type
                    // contract, drops the frame (releasing its cell `Rc`s — captured
                    // cells stay alive via the closures' own refs), truncates the
                    // stack to `slot_base`, and pushes the result into the caller.
                    // `PROPAGATE` reuses this SAME unwind on a propagated error.
                    let result = fiber.pop();
                    if let Some(outcome) = self.return_from_frame(fiber, result)? {
                        return Ok(outcome);
                    }
                }

                Op::Propagate => {
                    // The `?` operator. Mirrors the tree-walker's `ExprKind::Try`
                    // exactly: the operand must be a 2-element `[value, err]` Result
                    // pair (else a Tier-2 panic with the identical message, anchored
                    // at this op's span = the `TryExpr`'s code span). If `err == nil`
                    // the `value` is left on the stack (the `?` expression's result);
                    // otherwise it does a FUNCTION-LEVEL early return of `[nil, err]`
                    // — the SAME unwind-one-frame logic as `Op::Return` — so the
                    // enclosing function returns the propagated pair (and at the top
                    // level the program ends with that pair, treated as `Ok` by the
                    // driver, just like `Control::Propagate` in `run_file`).
                    let v = fiber.pop();
                    let (value, err) = match &v {
                        Value::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "the ? operator requires a Result pair [value, err]".to_string(),
                            ))
                        }
                    };
                    if err == Value::Nil {
                        fiber.push(value);
                    } else {
                        let pair = crate::interp::make_pair(Value::Nil, err);
                        if let Some(outcome) = self.return_from_frame(fiber, pair)? {
                            return Ok(outcome);
                        }
                    }
                }

                Op::Unwrap => {
                    // The `!` force-unwrap operator. Mirrors the tree-walker's
                    // `ExprKind::Unwrap` exactly: the operand must be a 2-element
                    // `[value, err]` Result pair (else a Tier-2 panic with the
                    // identical message, anchored at this op's span = the
                    // `UnwrapExpr`'s code span). If `err == nil` the `value` is
                    // left on the stack (the `!` expression's result); otherwise
                    // it raises a RECOVERABLE `Control::Panic` carrying the
                    // original error's message (`error_message`), so `recover`
                    // round-trips it into `[nil, err]` IDENTICALLY to the
                    // tree-walker's `AsError::at(error_message(&err), span)`.
                    let v = fiber.pop();
                    let (value, err) = match &v {
                        Value::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "the ! operator requires a Result pair [value, err]".to_string(),
                            ))
                        }
                    };
                    if err == Value::Nil {
                        fiber.push(value);
                    } else {
                        return Err(self.panic_at(fiber, fault_ip, error_message(&err)));
                    }
                }

                Op::Await => {
                    // `await expr`. Mirrors the tree-walker's `ExprKind::Await`
                    // EXACTLY: if the operand is a `Value::Future`, drive it to
                    // completion (`f.get().await`) — a panic/propagation raised in
                    // the spawned task re-surfaces HERE (cross-task propagation),
                    // byte-identical to the tree-walker; otherwise `await` on a
                    // non-future is identity (`await 5 == 5`). Pop the operand into
                    // an owned local BEFORE the await so no `fiber` RefCell borrow is
                    // held across the suspension point (`await_holding_refcell_ref`
                    // stays clean).
                    let v = fiber.pop();
                    match v {
                        Value::Future(f) => {
                            let r = f.get().await?;
                            fiber.push(r);
                        }
                        other => fiber.push(other),
                    }
                }

                Op::Yield => {
                    // `yield expr`. The Fiber model makes this trivial: the yielded
                    // value is on TOS; pop it and return `RunOutcome::Yielded(v)`
                    // WITHOUT unwinding any frames — the frame stack stays live in
                    // the Fiber and `ip` is already past this op, so the next
                    // `resume` continues exactly here. The consumer's `next(v)`
                    // (driven via `GeneratorHandle::resume_vm`) pushes its `v` back
                    // onto the Fiber's stack, where the bytecode after `Op::Yield`
                    // expects the yield expression's value — that is the value-
                    // injection mechanism. `yield` with no operand pushed a `Nil`
                    // (the compiler emits NIL), so the popped value is `nil`.
                    let v = fiber.pop();
                    fiber.state = crate::vm::FiberState::Suspended;
                    return Ok(RunOutcome::Yielded(v));
                }

                Op::GetIter => {
                    // `for await` async-iterable validation: TOS must be a
                    // `Value::Generator` (driven by `resume`) or a native stream
                    // handle (WebSocket `recv` / SSE `next`). ANYTHING ELSE is the
                    // Tier-2 panic `value of type {t} is not async-iterable`,
                    // byte-identical to the tree-walker's `exec_for_await` (the
                    // `other =>` and the Native-with-no-stream-method arms both
                    // produce this message). We PEEK (leave the value in place): the
                    // compiler immediately stores it into a scratch slot to drive
                    // lazily across iterations.
                    let ok = match fiber.peek(0) {
                        Value::Generator(_) => true,
                        Value::Native(n) => crate::interp::native_stream_method(n.kind).is_some(),
                        _ => false,
                    };
                    if !ok {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("value of type {t} is not async-iterable"),
                        ));
                    }
                }

                Op::IterNext => {
                    // Drive one lazy `for await` step over the async-iterable on TOS.
                    // Pop it into an owned local BEFORE any `.await` so no `fiber`
                    // RefCell borrow is held across the suspension point
                    // (`await_holding_refcell_ref` stays clean), then push back the
                    // produced `value` and a `done` boolean. Byte-identical to
                    // `exec_for_await` (`src/interp.rs`).
                    // The op's span (the iterable expression's code span), captured
                    // before any borrow/await so a native-stream call has a site.
                    let op_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let iterable = fiber.pop();
                    match iterable {
                        Value::Generator(g) => {
                            // `resume(nil)` drives the backing Fiber to its next
                            // `Op::Yield` (awaiting any inner futures along the way —
                            // this is how an async generator's await+yield fuse).
                            // `Some(v)` -> a value; `None` -> done.
                            match g.resume(Value::Nil).await? {
                                Some(v) => {
                                    fiber.push(v);
                                    fiber.push(Value::Bool(false));
                                }
                                None => {
                                    fiber.push(Value::Nil);
                                    fiber.push(Value::Bool(true));
                                }
                            }
                        }
                        Value::Native(n) => {
                            // A native stream: call its `recv`/`next` method for a
                            // `[value, err]` pair (a non-nil `err` is a Tier-2 panic,
                            // a nil `value` ends the stream), mirroring
                            // `exec_for_await`'s `Value::Native` arm exactly.
                            // `GetIter` already validated the handle, so a missing
                            // stream method here is a wiring bug — surface it as a
                            // defensive Tier-2 panic rather than an `unwrap`.
                            let method = match crate::interp::native_stream_method(n.kind) {
                                Some(m) => m,
                                None => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "value of type {} is not async-iterable",
                                            crate::interp::type_name(&Value::Native(n))
                                        ),
                                    ))
                                }
                            };
                            let bound = Value::NativeMethod(Rc::new(crate::value::NativeMethod {
                                receiver: n,
                                method: method.to_string(),
                            }));
                            // Box this edge: `call_value` may re-enter `run`, so
                            // the recursive future needs a finite size.
                            let pair =
                                Box::pin(self.call_value(bound, Vec::new(), op_span)).await?;
                            let (value, err) = match &pair {
                                Value::Array(a) if a.borrow().len() == 2 => {
                                    let b = a.borrow();
                                    (b[0].clone(), b[1].clone())
                                }
                                // Defensive: a non-pair return ends iteration.
                                _ => {
                                    fiber.push(Value::Nil);
                                    fiber.push(Value::Bool(true));
                                    continue;
                                }
                            };
                            if err != Value::Nil {
                                let msg = crate::interp::error_message(&err);
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("for await stream error: {msg}"),
                                ));
                            }
                            if value == Value::Nil {
                                fiber.push(Value::Nil);
                                fiber.push(Value::Bool(true));
                            } else {
                                fiber.push(value);
                                fiber.push(Value::Bool(false));
                            }
                        }
                        other => {
                            // GetIter validated the iterable, so this is unreachable
                            // in practice; surface defensively rather than panic the
                            // host.
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "value of type {} is not async-iterable",
                                    crate::interp::type_name(&other)
                                ),
                            ));
                        }
                    }
                }

                Op::IterClose => {
                    // Close the async-iterable on TOS on a `break`/early-`return` out
                    // of a `for await` over a generator — `g.close()` drops the
                    // backing Fiber so it is reclaimed promptly, byte-identical to
                    // the tree-walker. A native stream is reclaimed at scope end, so
                    // closing it is a no-op here.
                    let iterable = fiber.pop();
                    if let Value::Generator(g) = iterable {
                        g.close();
                    }
                }

                Op::SetProp => {
                    // `obj value -- value` — store `obj.<name> = value`, applying a
                    // declared field-type contract on an Instance field. The SAME
                    // `set_member` the tree-walker's `assign_to` Member arm uses, so
                    // the field contract panic (message + span) is byte-identical.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("SET_PROP operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let value = fiber.pop();
                    let obj = fiber.pop();
                    // The op's span is the VALUE's span (see the compiler), matching
                    // the tree-walker's `value_span` for the contract panic; reuse it
                    // for the "cannot set property" error too (single VM span).
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // Resolve `proto` out of the fiber so the chunk IC borrow does not
                    // collide with the later `fiber.push`.
                    let proto = fiber.frame().closure.proto.clone();
                    let v = self.vm_set_prop(&proto.chunk, fault_ip, &obj, &name, value, span)?;
                    fiber.push(v);
                }

                Op::Class => {
                    // Build a class value (V9). The compiler emitted, just below this
                    // op, one closure per defaulted field (declaration order) then
                    // one closure per method (declaration order); the class proto
                    // carries the prebuilt `Rc<Class>` and the parallel name lists.
                    // Register the default thunks and method closures in the VM side
                    // tables keyed by the class's `Rc` identity, then push the class.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let cp = fiber.frame().closure.proto.chunk.class_protos[idx].clone();
                    let n_methods = cp.method_names.len();
                    let n_statics = cp.static_method_names.len();
                    let n_defaults = cp.default_fields.len();
                    // Pop in reverse push order: static closures (top), then instance
                    // method closures, then default thunks (SP1 §3 stack layout
                    // `[super?, ..thunks.., ..methods.., ..statics..]`).
                    let mut statics = vec![Value::Nil; n_statics];
                    for slot in statics.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let mut methods = vec![Value::Nil; n_methods];
                    for slot in methods.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let mut defaults = vec![Value::Nil; n_defaults];
                    for slot in defaults.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    // For an `extends` clause, the superclass class-value was pushed
                    // FIRST (it is the bottom of the group), so it pops LAST. Build a
                    // FRESH `Rc<Class>` with `superclass` set (the prebuilt template
                    // had `superclass: None`); the method/default tables are then
                    // registered under the NEW class's identity key. Mirrors the
                    // tree-walker's `Stmt::Class`, which sets `superclass` to the
                    // resolved parent `Value::Class`.
                    // The shared `def_env` for VM classes (task #157): the SHARED
                    // `validate_into` (`.from`/typed-parse) resolves nested-class
                    // field-type names and default-expr names through it, so EVERY
                    // class gets it (not just the `extends` case). This means we
                    // always build a FRESH `Rc<Class>` (the compiler's template had
                    // the inert `global_env()` placeholder + no superclass).
                    let def_env = self.class_env();
                    let superclass = if cp.has_super {
                        let sup = fiber.pop();
                        match sup {
                            Value::Class(c) => Some(c),
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("'{other}' is not a class"),
                                ))
                            }
                        }
                    } else {
                        None
                    };
                    let class: Rc<crate::value::Class> = Rc::new(crate::value::Class {
                        name: cp.class.name.clone(),
                        superclass,
                        fields: cp.class.fields.clone(),
                        methods: cp.class.methods.clone(),
                        // VM static methods live in a separate per-class proto
                        // table (keyed by Rc::as_ptr); this runtime `Class` value
                        // carries an empty namespace (populated for the tree-walker
                        // path only). The VM resolves `C.name` statics via that
                        // table (SP1 §3, C5).
                        static_methods: indexmap::IndexMap::new(),
                        def_env: def_env.clone(),
                        is_worker: cp.class.is_worker,
                    });
                    // Register the class into the shared env so a sibling/forward
                    // nested-class field type (or a default-expr name) resolves at
                    // `.from` time — late-bound exactly like the tree-walker's module
                    // env. A redefinition (same name re-run) overwrites the binding.
                    if def_env
                        .define(&class.name, Value::Class(class.clone()), false)
                        .is_err()
                    {
                        let _ = def_env.assign(&class.name, Value::Class(class.clone()));
                    }
                    let key = Rc::as_ptr(&class) as usize;
                    let mut method_map: HashMap<String, Cc<Closure>> = HashMap::new();
                    for (name, mv) in cp.method_names.iter().zip(methods) {
                        match mv {
                            Value::Closure(c) => {
                                method_map.insert(name.clone(), c);
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("class method '{name}' is not a closure: {other:?}"),
                                ))
                            }
                        }
                    }
                    let mut default_map: HashMap<String, Cc<Closure>> = HashMap::new();
                    for (i, (name, dv)) in cp.default_fields.iter().zip(defaults).enumerate() {
                        match dv {
                            Value::Closure(c) => {
                                // Mirror the enclosing-scope names this default
                                // captures into `def_env` (read from the thunk's
                                // captured upvalue cells), so the SHARED
                                // `validate_into` (`.from`/typed-parse) resolves the
                                // same binding the construct-time thunk closes over.
                                // The construct path still runs the thunk unchanged.
                                // Mirror as MUTABLE so a default that ASSIGNS to a
                                // captured name (`x: number = (g = 5)`) evaluates on
                                // the `.from` path exactly as the tree-walker does
                                // against its real `def_env` chain (where the captured
                                // `let` keeps its declared mutability). For the common
                                // read-only default the mutability flag is irrelevant.
                                if let Some(caps) = cp.default_captures.get(i) {
                                    for (cap_name, up_idx) in caps {
                                        if let Some(cell) = c.upvalues.get(*up_idx as usize) {
                                            let val = cell.borrow().clone();
                                            if def_env.define(cap_name, val.clone(), true).is_err()
                                            {
                                                let _ = def_env.assign(cap_name, val);
                                            }
                                        }
                                    }
                                }
                                default_map.insert(name.clone(), c);
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "field default '{name}' thunk is not a closure: {other:?}"
                                    ),
                                ))
                            }
                        }
                    }
                    let mut static_map: HashMap<String, Cc<Closure>> = HashMap::new();
                    for (name, sv) in cp.static_method_names.iter().zip(statics) {
                        match sv {
                            Value::Closure(c) => {
                                static_map.insert(name.clone(), c);
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("static method '{name}' is not a closure: {other:?}"),
                                ))
                            }
                        }
                    }
                    self.class_methods.borrow_mut().insert(key, method_map);
                    self.class_static_methods.borrow_mut().insert(key, static_map);
                    self.class_defaults.borrow_mut().insert(key, default_map);
                    fiber.push(Value::Class(class));
                }

                Op::GetSuper => {
                    // `super.<name>` (V9-T2): resolve `name` starting at the CURRENT
                    // method's DEFINING class's superclass, bound to `self` (slot 0).
                    // Mirrors the tree-walker: `super` is a `Value::Super` whose
                    // `start` is `defining_class.superclass`, and `read_member` on it
                    // finds the method up that chain and produces a BoundMethod on
                    // `self` (which the subsequent CALL invokes). The `defining_class`
                    // we stamp onto the BoundMethod is the ANCESTOR that actually
                    // declared the method, so a NESTED `super` resolves from the right
                    // link too.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_SUPER name is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // The defining class of the running method (set by
                    // `invoke_compiled_method`). Absent only if `super` somehow
                    // appears outside a method frame — a compiler invariant violation.
                    let def_class = match &fiber.frame().def_class {
                        Some(c) => c.clone(),
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "'super' used outside of a method".to_string(),
                            ))
                        }
                    };
                    // self = slot 0, read cell-aware (it is a cell slot whenever a
                    // nested closure captured it).
                    let receiver = match &fiber.frame().cells[0] {
                        Some(cell) => cell.borrow().clone(),
                        None => fiber.local(0).clone(),
                    };
                    // Resolve up from the DEFINING class's superclass (NOT the
                    // instance's class), matching `SuperRef { start: superclass }`.
                    let start = def_class.superclass.clone();
                    let bound = match start
                        .as_ref()
                        .and_then(|s| self.find_compiled_method(s, &name))
                    {
                        Some((_closure, found_class)) => {
                            Value::BoundMethod(Rc::new(crate::value::BoundMethod {
                                receiver,
                                method: Rc::new(crate::value::Method {
                                    params: Vec::new(),
                                    ret: None,
                                    body: Vec::new(),
                                    is_async: false,
                                    is_generator: false,
                                    is_worker: false,
                                }),
                                defining_class: found_class,
                                name: name.to_string(),
                            }))
                        }
                        None => {
                            // Mirror the tree-walker's `Value::Super` member-read
                            // error wording (with/without a superclass).
                            let msg = if start.is_some() {
                                format!("no superclass method '{name}'")
                            } else {
                                format!("no superclass method '{name}' (no superclass)")
                            };
                            return Err(Control::Panic(AsError::at(msg, span)));
                        }
                    };
                    fiber.push(bound);
                }

                other => {
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        format!("opcode {other:?} not yet implemented"),
                    ))
                }
            }
        }
    }

    /// Call ANY value, the single primitive both engines re-enter through.
    ///
    /// This is the bridge in BOTH directions:
    /// - A `Value::Closure` (`native → VM`): a native higher-order stdlib function
    ///   (`array.map`, a sort comparator, `recover`, …) invokes a user callback
    ///   the VM produced. We build a fresh one-frame [`Fiber`] whose sole frame is
    ///   the closure called with `args`, then drive it to completion. Each closure
    ///   invocation gets its OWN Fiber, so the reentrant nesting (VM run → native
    ///   HOF → `call_value` → `Vm::call_value` → `run(new fiber)`) is naturally
    ///   recursive and self-contained.
    /// - Anything else (`VM → native`): delegate to the shared
    ///   [`Interp::call_value`] — identical to the `Op::Call` non-Closure arm.
    ///
    /// Arity / per-param contracts / rest collection use the SAME
    /// [`check_call_args`](crate::interp::check_call_args) the tree-walker and the
    /// `Op::Call` arm use, so a closure called from native code binds its args and
    /// surfaces arity/contract panics byte-identically. The return-type contract is
    /// enforced by `Op::Return` against the frame's `ret_span` (the call span),
    /// exactly as for an in-VM call.
    /// Workers Spec B §Task 5 (actor isolate side): call the method `name` on a VM
    /// instance `receiver` with `args`, resolving the method through the VM's
    /// per-class method side table (`vm_read_member` → `BoundMethod`) and driving any
    /// returned `Value::Future` (an `async` method) to its value. Used by the actor
    /// mailbox loop, which runs on the isolate's own `Vm` — `Interp::read_member`
    /// cannot be used because a VM-built class keeps its methods in the side table,
    /// not in `Class.methods`.
    pub async fn call_method_named(
        &self,
        receiver: Value,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let bound = self.vm_read_member(&receiver, name, span)?;
        let r = self.call_value(bound, args, span).await?;
        match r {
            Value::Future(f) => f.get().await,
            other => Ok(other),
        }
    }

    #[async_recursion::async_recursion(?Send)]
    pub async fn call_value(
        &self,
        callee: Value,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match callee {
            Value::Closure(closure) => {
                // Workers Spec A: a `worker fn` passed as a higher-order value (e.g.
                // `array.map(seeds, workerFn)`) must be dispatched to a pooled isolate
                // when running on the CALLER thread — otherwise the worker body runs
                // inline and NO parallelism occurs. Mirror the `Op::Call` arm's
                // `is_worker` branch so that the native → VM re-entry path produces a
                // `Value::Future` (dispatched) rather than a synchronously-computed
                // result.
                //
                // INSIDE an isolate, this path is NOT taken: the `Op::Call` handler
                // dispatches `dispatch_worker_inline` for the inline-nesting case, and
                // `dispatch_worker_inline`'s `spawn_local` task calls `vm.call_value`
                // on the entry — at that point we are still inside the isolate thread
                // and the entry must run as a plain closure (the code-slice already
                // defined it; running it inline IS the intended behavior). Skip the
                // re-dispatch to avoid infinite recursion:
                //   dispatch_worker_closure → in_isolate → dispatch_worker_inline
                //   → vm.call_value → is_worker → dispatch_worker_closure → ...
                // Only re-dispatch when there is something to build a slice FROM;
                // if neither worker_source nor worker_aso_bytes is set, we are in a
                // test harness or a fresh isolate that loaded the slice directly —
                // fall through to the plain inline path (which is correct there).
                if closure.proto.is_worker
                    && !closure.proto.is_generator
                    && !crate::worker::pool::in_isolate()
                    && (self.interp.worker_source().is_some()
                        || self.interp.worker_aso_bytes().is_some())
                {
                    return self.dispatch_worker_closure(&closure, args, span);
                }
                // A GENERATOR closure (`fn*` / `async fn*` / `worker fn*`) is NOT run to
                // completion here — it builds a NOT-STARTED VM fiber wrapped in a
                // `GeneratorHandle`, returning a `Value::Generator` (the consumer drives
                // it via `resume`). This mirrors the `Op::Call` generator arm and is the
                // path taken when a `worker fn*` runs ON ITS DEDICATED ISOLATE: the
                // isolate's `build_producer` calls `call_value(entry, ..)` and expects a
                // LOCAL generator back (the cross-thread streaming is the CALLER-side
                // driver, not the isolate's). Without this, the body would run inline and
                // hit "a closure cannot yield".
                if closure.proto.is_generator {
                    let what = closure.proto.chunk.name.as_deref().unwrap_or("function");
                    let bound =
                        crate::interp::check_call_args(&closure.proto.params, args, span, what)?;
                    let mut gfiber = Fiber::new(closure);
                    gfiber.frame_mut().ret_span = span;
                    gfiber.frame_mut().argc = bound.supplied;
                    let cells = gfiber.frame().cells.clone();
                    for (slot, v) in bound.values.into_iter().enumerate() {
                        if let Some(cell) = &cells[slot] {
                            *cell.borrow_mut() = v;
                        } else {
                            gfiber.stack[slot] = v;
                        }
                    }
                    let handle =
                        crate::coro::GeneratorHandle::new_vm(gfiber, Rc::downgrade(&self.rc()));
                    return Ok(Value::Generator(Rc::new(handle)));
                }
                // `what` mirrors the tree-walker's
                // `func.name.as_deref().unwrap_or("function")` so an arity/contract
                // panic message matches.
                let what = closure.proto.chunk.name.as_deref().unwrap_or("function");
                // Arity + per-param contracts + rest collection, shared verbatim
                // with the tree-walker and the `Op::Call` arm.
                let bound =
                    crate::interp::check_call_args(&closure.proto.params, args, span, what)?;
                // Build a one-frame Fiber whose sole frame is the closure, then
                // place the bound params into its slots (cell slot → cell, plain
                // slot → stack). `Fiber::new` already reserved `slot_count` Nil
                // locals and allocated the cell vector, so we only overwrite the
                // param slots; the rest stay Nil.
                let mut fiber = Fiber::new(closure);
                fiber.frame_mut().ret_span = span;
                fiber.frame_mut().argc = bound.supplied;
                // Snapshot the cell `Rc`s for the param slots so we don't hold a
                // frame borrow while also writing `fiber.stack` (plain slots).
                let cells = fiber.frame().cells.clone();
                for (slot, v) in bound.values.into_iter().enumerate() {
                    if let Some(cell) = &cells[slot] {
                        *cell.borrow_mut() = v;
                    } else {
                        fiber.stack[slot] = v;
                    }
                }
                // SP3 §B: this re-enters `Vm::run` on a FRESH native stack frame —
                // the fiber's initial frame is one logical call. Guard it (RAII, the
                // counter unwinds on drop); the initial frame's RETURN hits the root
                // path in `return_from_frame` which does NOT decrement, so the guard
                // owns exactly that unit. A `Cell`, never held as a RefCell borrow
                // across the `.await`.
                let _depth = self.interp.enter_call_depth_scoped(span)?;
                // Drive the fresh fiber to completion. A top-level closure body
                // cannot `yield` (yield is only valid inside a generator, which is
                // driven differently), so `Done(v)` is the only outcome; a `yield`
                // here would be a compiler bug.
                // SP9 §1: this is a native re-entry funnel for higher-order stdlib
                // callbacks (`array.map`/`reduce`/comparators) — a deep `map`-of-`map`
                // nests Rust frames here. Grow the native stack per poll so the
                // re-entry reaches the logical cap cleanly instead of SIGABRTing.
                match crate::vm::stack::grow_future(self.run(&mut fiber)).await? {
                    RunOutcome::Done(v) => Ok(v),
                    RunOutcome::Yielded(_) => {
                        unreachable!("a closure called via Vm::call_value cannot yield")
                    }
                }
            }
            // A class constructor (V9): build an instance VM-side (defaults via
            // thunks + compiled `init`) so the init method runs as COMPILED code.
            Value::Class(class) if self.is_vm_class(&class) => {
                self.vm_construct(class, args, span).await
            }
            // A bound method (V9) on a VM-registered class: run the COMPILED method
            // closure with `self` bound to the receiver (slot 0).
            Value::BoundMethod(bm) if self.bound_method_is_vm(&bm).is_some() => {
                let closure = self.bound_method_is_vm(&bm).expect("checked above");
                // The BoundMethod's `defining_class` is the class that actually
                // declared the method (set by `vm_read_member` / `Op::GetSuper` via
                // the chain walk), so a `super.<name>` inside it resolves correctly.
                self.invoke_compiled_method(
                    closure,
                    bm.receiver.clone(),
                    args,
                    span,
                    Some(bm.defining_class.clone()),
                )
                .await
            }
            // Native callee: delegate to the shared dispatch (same as the
            // `Op::Call` non-Closure arm).
            other => self.interp.call_value(other, args, span).await,
        }
    }

    /// Whether `class` is a VM-registered class (it has a compiled-method table).
    /// A class minted by the tree-walker (e.g. via a native module) is NOT here, so
    /// it falls through to the shared `Interp` dispatch.
    fn is_vm_class(&self, class: &Rc<crate::value::Class>) -> bool {
        let key = Rc::as_ptr(class) as usize;
        self.class_methods.borrow().contains_key(&key)
    }

    /// The compiled method closure for `(class identity, name)` looked up ON the
    /// given class ONLY (no chain walk), if registered.
    fn compiled_method_own(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<Cc<Closure>> {
        let key = Rc::as_ptr(class) as usize;
        self.class_methods
            .borrow()
            .get(&key)
            .and_then(|m| m.get(name))
            .cloned()
    }

    /// Walk the superclass chain from `class` upward, returning the first compiled
    /// method named `name` plus the ANCESTOR class that DEFINED it. The VM method
    /// side-table is keyed by `Rc::as_ptr(class)`, so walking the chain means
    /// probing each ancestor's table in turn. Mirrors the tree-walker's
    /// `value::find_method` (own class first, then up `superclass`), so an
    /// inherited method runs the ancestor's COMPILED closure and a `super` lookup
    /// gets the correct defining class.
    fn find_compiled_method(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<(Cc<Closure>, Rc<crate::value::Class>)> {
        let mut cur = Some(class.clone());
        while let Some(c) = cur {
            if let Some(closure) = self.compiled_method_own(&c, name) {
                return Some((closure, c));
            }
            cur = c.superclass.clone();
        }
        None
    }

    /// The compiled STATIC closure for `(class identity, name)` looked up ON the
    /// given class ONLY (no chain walk), if registered (SP1 §3).
    fn compiled_static_own(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<Cc<Closure>> {
        let key = Rc::as_ptr(class) as usize;
        self.class_static_methods
            .borrow()
            .get(&key)
            .and_then(|m| m.get(name))
            .cloned()
    }

    /// Walk the superclass chain for a compiled STATIC method `name` (SP1 §3),
    /// mirroring `find_compiled_method` over the static side-table. A subclass
    /// resolves an unknown static up its superclass chain.
    fn find_compiled_static_method(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<Cc<Closure>> {
        let mut cur = Some(class.clone());
        while let Some(c) = cur {
            if let Some(closure) = self.compiled_static_own(&c, name) {
                return Some(closure);
            }
            cur = c.superclass.clone();
        }
        None
    }

    /// If `bm` is a bound method on a VM-registered class, return its compiled
    /// method closure (resolved up the chain); else `None` (so a tree-walker
    /// BoundMethod delegates).
    fn bound_method_is_vm(&self, bm: &crate::value::BoundMethod) -> Option<Cc<Closure>> {
        if let Value::Instance(inst) = &bm.receiver {
            let class = inst.borrow().class.clone();
            // Resolve from the method's DEFINING class (set by `vm_read_member` /
            // `Op::GetSuper`) so an inherited or super-dispatched method runs the
            // right ancestor's closure; fall back to the instance's class chain for
            // a BoundMethod minted elsewhere.
            return self
                .find_compiled_method(&bm.defining_class, &bm.name)
                .or_else(|| self.find_compiled_method(&class, &bm.name))
                .map(|(closure, _)| closure);
        }
        None
    }

    /// VM member read (V9). For an `Instance` of a VM-registered class, a method
    /// name resolves to a `Value::BoundMethod` carrying the receiver + class +
    /// method name (the compiled closure is looked up at CALL time via
    /// `bound_method_is_vm`); a field name reads the stored field; anything else
    /// (and any non-VM receiver) delegates to the shared `Interp::read_member` so
    /// the two engines share field/enum/native member-access semantics. The dummy
    /// `Method` carried by the `BoundMethod` is never executed by the VM — its body
    /// is empty — it exists only to satisfy the frozen `value.rs` `BoundMethod`
    /// shape; method dispatch always runs the COMPILED closure.
    /// FIELD inline-cache fast path for `GET_PROP` (V11-T3). Returns `Some(value)`
    /// when `name` resolves to a FIELD of a shaped `Object`/`Instance` — either
    /// from a cache HIT (`recv.shape == cached.shape` → read `get_index(idx)`) or
    /// after a fresh generic field resolution that is then RECORDED into the cache.
    /// Returns `None` when the field fast path does not apply — in which case the
    /// caller MUST take the generic `vm_read_member` path (which resolves methods,
    /// enums, natives, nil, non-shaped receivers, …). The returned value is always
    /// byte-identical to what `vm_read_member` would return for the same input.
    ///
    /// GUARDS (force `None`, i.e. generic path):
    /// - a receiver that is not an `Object`/`Instance` (modules, strings, enums,
    ///   classes, generators, nil → handled by `read_member`);
    /// - a shape of `0` (unset — a tree-walker-built value the IC cannot key on);
    /// - a SCHEMA-VALUE object (`is_schema_value`): never cached, so a schema
    ///   object's member access always flows through the generic path;
    /// - a name that is NOT a field (`get_index_of` → `None`): on an Instance this
    ///   is a METHOD (→ BoundMethod via generic) or a missing field; either way the
    ///   IC neither caches nor answers, so it can never return a wrong value for a
    ///   method-named access.
    fn ic_get_field(
        &self,
        chunk: &crate::vm::chunk::Chunk,
        op_off: usize,
        obj: &Value,
        name: &str,
    ) -> Option<Value> {
        match obj {
            Value::Object(cell) => {
                let shape = cell.shape.get();
                if shape == 0 || crate::stdlib::schema::is_schema_value(obj) {
                    return None;
                }
                // Cache hit: read the field directly by its stable index.
                let ic = chunk.field_ic(op_off);
                if let Some(idx) = ic.lookup(shape) {
                    let map = cell.map.borrow();
                    // The index is keyed by shape (V11-T2: shape ⇒ key layout), so
                    // it is always in range for an object of that shape.
                    if let Some((_k, v)) = map.get_index(idx as usize) {
                        return Some(v.clone());
                    }
                    // Defensive: a stale/out-of-range index never feeds a wrong
                    // value — fall through to re-resolve generically below.
                }
                // Miss: resolve the field index generically and RECORD it.
                let map = cell.map.borrow();
                match map.get_index_of(name) {
                    Some(idx) => {
                        let v = map.get_index(idx).map(|(_, v)| v.clone());
                        drop(map);
                        let mut ic = chunk.field_ic(op_off);
                        ic.record(shape, idx as u32);
                        chunk.set_field_ic(op_off, ic);
                        v
                    }
                    // Not a field on this object → generic path (returns nil).
                    None => None,
                }
            }
            Value::Instance(inst) => {
                let b = inst.borrow();
                let shape = b.shape_id.get();
                if shape == 0 {
                    return None;
                }
                // Cache hit: read the field directly by its stable index.
                let ic = chunk.field_ic(op_off);
                if let Some(idx) = ic.lookup(shape) {
                    if let Some((_k, v)) = b.fields.get_index(idx as usize) {
                        return Some(v.clone());
                    }
                    // Defensive fall-through (see Object arm).
                }
                // Miss: resolve generically and record IF it is a FIELD. A
                // method-named access yields `None` here → generic path →
                // BoundMethod (never cached, never mis-answered).
                match b.fields.get_index_of(name) {
                    Some(idx) => {
                        let v = b.fields.get_index(idx).map(|(_, v)| v.clone());
                        drop(b);
                        let mut ic = chunk.field_ic(op_off);
                        ic.record(shape, idx as u32);
                        chunk.set_field_ic(op_off, ic);
                        v
                    }
                    None => None,
                }
            }
            // Every other receiver kind: generic path.
            _ => None,
        }
    }

    /// METHOD inline-cache fast path for `CALL_METHOD` (V11-T3). Returns
    /// `Some((closure, defining_class))` when `recv` is a VM `Instance` whose
    /// `name` resolves up the class chain to a COMPILED method AND is NOT shadowed
    /// by an instance field — exactly the case the generic
    /// `vm_read_member → BoundMethod → bound_method_is_vm` path would dispatch to
    /// the same compiled closure. Returns `None` (→ generic path) for every other
    /// receiver, a schema value, a name that is an instance FIELD (a field shadows
    /// a method), or a name with no compiled method.
    ///
    /// On a hit it serves the cached `(closure, defining_class)`; on a miss it
    /// resolves via `find_compiled_method` and RECORDS the result keyed by the
    /// receiver's CLASS IDENTITY (`Rc` pointer) — never the field shape, because two
    /// distinct classes may share a field layout but resolve methods differently.
    /// The SHARED method-dispatch body for `Op::CallMethod` and
    /// `Op::CallMethodSpread`. Both ops produce the SAME `(recv, args)` (CallMethod
    /// from a static argc; CallMethodSpread from a flattened runtime args array),
    /// then call this with the method `name`, the op's bytecode offset `fault_ip`
    /// (which keys the per-site method IC), and the trivia-trimmed call `span`.
    ///
    /// Dispatch mirrors the tree-walker's `eval_chain` Member-callee Call arm:
    ///   1. Schema fluent-method hook (`is_schema_value` + `is_schema_method`) →
    ///      `call_schema(name, [recv, ...args])`.
    ///   2. METHOD inline-cache fast path (V11-T3/T6): a VM instance whose `name`
    ///      resolves up the chain to a COMPILED method (not shadowed by a field).
    ///      For a plain method, push a frame onto THIS fiber and continue the run
    ///      loop in place (no fresh Fiber, no recursive `run`); for async/generator
    ///      methods, `invoke_compiled_method`.
    ///   3. Generic fallback: `vm_read_member(recv, name)` → `call_value`.
    ///
    /// On every path EXCEPT the plain-method in-frame fast path it pushes the result
    /// onto the stack; the fast path pushes a `CallFrame` and the run loop continues
    /// (RETURN pops it and pushes the result onto the caller's stack). The behavior
    /// is byte-identical between the two callers — the only difference upstream is
    /// how the arg list was obtained.
    async fn dispatch_method(
        &self,
        fiber: &mut Fiber,
        recv: Value,
        name: &str,
        args: Vec<Value>,
        fault_ip: usize,
        span: Span,
    ) -> Result<(), Control> {
        // Resolve the calling frame's `proto` so the chunk's method IC (keyed by this
        // op's bytecode offset) can be consulted without holding a fiber borrow
        // across the dispatch.
        let proto = fiber.frame().closure.proto.clone();
        // (1) Schema fluent-method hook (same predicate the tree-walker uses).
        if crate::stdlib::schema::is_schema_value(&recv)
            && crate::stdlib::schema::is_schema_method(name)
        {
            let mut sargs = Vec::with_capacity(args.len() + 1);
            sargs.push(recv);
            sargs.extend(args);
            let v = self.interp.call_schema(name, &sargs, span).await?;
            fiber.push(v);
            return Ok(());
        }
        // (1a) SP9 §2: workflow `ctx.<method>()` hook (same predicate + shape as the
        // schema hook, same routing to the shared `Interp`).
        #[cfg(feature = "workflow")]
        if crate::stdlib::workflow::is_ctx_value(&recv)
            && crate::stdlib::workflow::is_ctx_method(name)
        {
            let mut wargs = Vec::with_capacity(args.len() + 1);
            wargs.push(recv);
            wargs.extend(args);
            let v = self.interp.call_workflow_ctx(name, &wargs, span).await?;
            fiber.push(v);
            return Ok(());
        }
        // (1a') Workers Spec B §Task 5: `WorkerClass.spawn(args)` → spawn an actor
        // isolate, return `future<handle>`. Mirrors the tree-walker `eval_chain` hook
        // exactly (same `Interp::spawn_actor`), so the VM matches byte-for-byte. A
        // bare `WorkerClass(args)` construction is UNCHANGED (handled by `Op::Call`).
        if let Value::Class(class) = &recv {
            if class.is_worker && name == "spawn" {
                let v = self.interp.spawn_actor(class, args, span).await?;
                fiber.push(v);
                return Ok(());
            }
        }
        // (1a'') Actor-handle async method dispatch: a member-CALL on a
        // `Value::Native(WorkerActor)` sends an `ActorMsg::Call` (or `close()`) and
        // returns `future<T>`. Same `Interp::actor_handle_call` as the tree-walker.
        if let Value::Native(n) = &recv {
            if n.kind == crate::value::NativeKind::WorkerActor {
                let v = self.interp.actor_handle_call(n, name, args, span).await?;
                fiber.push(v);
                return Ok(());
            }
        }
        // (1b) STATIC method call `C.name(args)` (SP1 §3): the receiver is a VM
        // class whose `name` resolves (up the chain) to a compiled STATIC closure.
        // Dispatch with NO receiver, with full generator/async/sync handling
        // matching the `Op::Call` closure arm (so a `static fn*` returns a
        // `Value::Generator` and a `static async fn` a `Value::Future`, byte-
        // identical to the tree-walker's `call_static_method`). A non-static name
        // (the built-in `from`, or an error) falls through to the shared dispatch.
        if let Value::Class(class) = &recv {
            if self.is_vm_class(class) {
                if let Some(closure) = self.find_compiled_static_method(class, name) {
                    let v = self.invoke_compiled_static(closure, args, span).await?;
                    fiber.push(v);
                    return Ok(());
                }
            }
        }
        // (2) METHOD inline-cache fast path (V11-T3): the receiver is a VM instance
        // whose `name` resolves (up the chain) to a COMPILED method and is NOT
        // shadowed by an instance field. Byte-identical to the generic
        // `vm_read_member → BoundMethod → call_value → invoke_compiled_method` path.
        if let Some((closure, def_class)) = self
            .specialize
            .then(|| self.ic_resolve_method(&proto.chunk, fault_ip, &recv, name))
            .flatten()
        {
            // V11-T6 TUNING: for a plain (non-async, non-generator) method, push a
            // frame onto THIS fiber and continue the run loop in place — exactly like
            // the `Op::Call` VM-closure arm. Same arity/contract check, slot binding
            // (self→0, args→1..), `def_class` for `super`, and return-contract check.
            if !closure.proto.is_async && !closure.proto.is_generator {
                let what = closure.proto.chunk.name.as_deref().unwrap_or("method");
                let bound =
                    crate::interp::check_call_args(&closure.proto.params, args, span, what)?;
                let slot_base = fiber.stack.len();
                let slot_count = closure.proto.chunk.slot_count as usize;
                let cells = super::fiber::alloc_cells(slot_count, &closure.proto.chunk.cell_slots);
                fiber.stack.resize(slot_base + slot_count, Value::Nil);
                // self -> slot 0 (cell-aware).
                if let Some(cell) = &cells[0] {
                    *cell.borrow_mut() = recv;
                } else {
                    fiber.stack[slot_base] = recv;
                }
                // bound args -> slots 1..n+1 (cell-aware).
                let supplied = bound.supplied;
                for (i, v) in bound.values.into_iter().enumerate() {
                    let slot = i + 1;
                    if let Some(cell) = &cells[slot] {
                        *cell.borrow_mut() = v;
                    } else {
                        fiber.stack[slot_base + slot] = v;
                    }
                }
                // SP3 §B: one logical-call increment per method-call frame push
                // (matches the tree-walker `run_body`); decremented in
                // `return_from_frame`.
                self.enter_frame_depth(span)?;
                fiber.frames.push(super::fiber::CallFrame {
                    closure,
                    ip: 0,
                    slot_base,
                    cells,
                    ret_span: span,
                    def_class: Some(def_class),
                    argc: supplied,
                });
                // Continue the loop in the new frame; RETURN pops it and pushes the
                // result onto the caller's stack.
            } else {
                let v = self
                    .invoke_compiled_method(closure, recv, args, span, Some(def_class))
                    .await?;
                fiber.push(v);
            }
            return Ok(());
        }
        // (3) Fallback: read the member, then call it. `vm_read_member` yields a VM
        // `BoundMethod` for an Instance method on a VM class (dispatched to COMPILED
        // code by `call_value`), else the SAME dispatch the tree-walker runs (a
        // BoundMethod / GeneratorMethod / NativeMethod / Builtin / … bound to
        // `recv`). This also covers an instance FIELD holding a callable (a field
        // shadows a method — the IC fast path declines those).
        let callee_v = self.vm_read_member(&recv, name, span)?;
        let v = self.call_value(callee_v, args, span).await?;
        fiber.push(v);
        Ok(())
    }

    fn ic_resolve_method(
        &self,
        chunk: &crate::vm::chunk::Chunk,
        op_off: usize,
        recv: &Value,
        name: &str,
    ) -> Option<(Cc<Closure>, Rc<crate::value::Class>)> {
        let Value::Instance(inst) = recv else {
            return None;
        };
        // A field SHADOWS a method — never fast-path a field-named access (it is
        // not a method dispatch; the generic path reads the field instead).
        let (class, has_field) = {
            let b = inst.borrow();
            (b.class.clone(), b.fields.contains_key(name))
        };
        if has_field {
            return None;
        }
        let class_id = Rc::as_ptr(&class) as usize;
        // Cache hit: serve the resolved compiled method for this exact class.
        let ic = chunk.method_ic(op_off);
        if let Some(hit) = ic.lookup(class_id) {
            return Some(hit);
        }
        // Miss: resolve the compiled method up the chain and record it.
        let resolved = self.find_compiled_method(&class, name)?;
        let mut ic = chunk.method_ic(op_off);
        ic.record(class_id, resolved.0.clone(), resolved.1.clone());
        chunk.set_method_ic(op_off, ic);
        Some(resolved)
    }

    fn vm_read_member(&self, obj: &Value, name: &str, span: Span) -> Result<Value, Control> {
        if let Value::Instance(inst) = obj {
            let (class, has_field) = {
                let b = inst.borrow();
                (b.class.clone(), b.fields.contains_key(name))
            };
            if !has_field {
                // Walk the chain so an INHERITED method binds with the ANCESTOR
                // class as `defining_class` (so a `super` inside it resolves from
                // the right link), mirroring `value::find_method`.
                if let Some((_closure, def_class)) = self.find_compiled_method(&class, name) {
                    let bm = crate::value::BoundMethod {
                        receiver: obj.clone(),
                        method: Rc::new(crate::value::Method {
                            params: Vec::new(),
                            ret: None,
                            body: Vec::new(),
                            is_async: false,
                            is_generator: false,
                            is_worker: false,
                        }),
                        defining_class: def_class,
                        name: name.to_string(),
                    };
                    return Ok(Value::BoundMethod(Rc::new(bm)));
                }
            }
        }
        // `C.name` static-method read (SP1 §3): a VM-compiled static resolves up
        // the superclass chain to its closure, returned as a plain `Value::Closure`
        // (called with NO receiver). Falls through to the shared dispatch for the
        // built-in `from` and the "no static member" error (C3 generalization).
        if let Value::Class(class) = obj {
            if self.is_vm_class(class) {
                if let Some(closure) = self.find_compiled_static_method(class, name) {
                    return Ok(Value::Closure(closure));
                }
            }
        }
        // Field / non-VM receiver: shared dispatch (also yields the correct
        // nil-field / nil-receiver behavior, byte-identical to the tree-walker).
        self.interp
            .read_member(obj, name, span)
            .map_err(Control::from)
    }

    /// The BASE shape for `class`'s instances — the declared-field key layout in
    /// declaration order, MERGED base-class first (mirrors `merged_field_schema`,
    /// which is the order `vm_construct`/`construct` populate fields). Cached per
    /// class by `Rc` identity so every instance of one class shares the same id.
    fn class_base_shape(&self, class: &Rc<crate::value::Class>) -> u32 {
        let key = Rc::as_ptr(class) as usize;
        if let Some(&s) = self.class_base_shapes.borrow().get(&key) {
            return s;
        }
        let schema = crate::value::merged_field_schema(class);
        let shape = {
            let mut reg = self.shapes.borrow_mut();
            reg.shape_for(schema.keys().map(|k| k.as_str()))
        };
        self.class_base_shapes.borrow_mut().insert(key, shape);
        shape
    }

    /// The shape id for an object literal's final ordered key list. Used by the
    /// VM's `NEW_OBJECT`/`APPEND_OBJECT`/`SPREAD_OBJECT` arms once the entry map is
    /// fully built.
    fn object_shape_for<'a, I>(&self, keys: I) -> u32
    where
        I: IntoIterator<Item = &'a str>,
    {
        self.shapes.borrow_mut().shape_for(keys)
    }

    /// The current global-table version (V11-T4). Bumped on every user-global
    /// (re)definition (`Op::DefineGlobal`) or assignment (`Op::SetGlobal`). The
    /// [`GET_GLOBAL`](Op::GetGlobal) inline cache guards its cached value with this
    /// version, so a global write invalidates every cached entry. Top-level defines
    /// run once at load, then the version is stable and the caches stay hot.
    fn global_version(&self) -> u64 {
        self.global_version.get()
    }

    /// Bump the global-table version (invalidates every `GET_GLOBAL` cache entry
    /// recorded at the previous version). Saturating to avoid wraparound issues over
    /// an extremely long-lived `Vm` (a wrap is harmless for correctness — a stale
    /// cache hit would still be re-validated by the value's identity — but saturation
    /// keeps the invariant "any write changes the version" exact).
    fn bump_global_version(&self) {
        self.global_version
            .set(self.global_version.get().saturating_add(1));
    }

    /// The current STRUCTURAL generation (SP8). Bumped ONLY on a user-global DEFINE
    /// (insertion), never on a reassignment. The SP8 `IndexBound` global cache guards
    /// its stable `IndexMap` index with this generation.
    fn struct_gen(&self) -> u64 {
        self.struct_gen.get()
    }

    /// Read a module-scope user-global's (cloned) `Value` by name, or `None` if it
    /// is not (yet) defined. Public so the worker subsystem can fetch a freshly-run
    /// code-slice's ENTRY function out of a fresh isolate's globals and call it
    /// (`src/worker/dispatch.rs`); also the natural read hook for the REPL/embedders.
    pub fn user_global(&self, name: &str) -> Option<Value> {
        self.user_globals
            .borrow()
            .get(name)
            .map(|s| s.value.clone())
    }

    /// Resolve a module-scope user-global by name, returning BOTH its stable
    /// `IndexMap` index and its (cloned) `Value`, or `None` if not yet defined (SP8).
    /// The index is stable for the `Vm`'s life (user-globals are only ever inserted),
    /// so the `GET_GLOBAL` site can cache it as `GlobalCache::IndexBound`.
    fn get_user_global_full(&self, name: &str) -> Option<(usize, Value)> {
        self.user_globals
            .borrow()
            .get_full(name)
            .map(|(idx, _k, s)| (idx, s.value.clone()))
    }

    /// Read a user-global's (cloned) `Value` by its stable index (SP8 fast path).
    /// The caller has a live `IndexBound` cache entry, so the index is in range.
    fn user_global_value_at(&self, idx: usize) -> Value {
        self.user_globals
            .borrow()
            .get_index(idx)
            .map(|(_k, s)| s.value.clone())
            .expect("IndexBound cache holds an in-range user-global index")
    }

    /// Update a user-global's value IN PLACE by its stable index, returning its
    /// `mutable` flag for the SET mutability check (`Some(true)` → updated;
    /// `Some(false)` → immutable, caller errors; `None` → index out of range, caller
    /// re-resolves). Keeps the class `def_env` in sync (the same invariant as
    /// `update_user_global`). Does NOT bump any generation (a SET is not a define).
    fn set_user_global_at(&self, idx: usize, value: Value) -> Option<bool> {
        let (mutable, name) = {
            let map = self.user_globals.borrow();
            let (name, slot) = map.get_index(idx)?;
            (slot.mutable, name.clone())
        };
        if mutable {
            if let Some((_k, slot)) = self.user_globals.borrow_mut().get_index_mut(idx) {
                slot.value = value.clone();
            }
            if let Some(env) = self.class_env.borrow().as_ref() {
                let _ = env.assign(&name, value);
            }
        }
        Some(mutable)
    }

    /// Whether a module-scope user-global named `name` exists and is REASSIGNABLE.
    /// `None` if not yet defined; `Some(false)` if it is an immutable binding
    /// (`const`/`fn`/`class`/`enum`/`import`). Consulted by `Op::SetGlobal`.
    fn user_global_mutable(&self, name: &str) -> Option<bool> {
        self.user_globals.borrow().get(name).map(|s| s.mutable)
    }

    /// Update an EXISTING module-scope user-global's value (preserving its mutability
    /// flag) WITHOUT bumping the global version OR `struct_gen` (a value update cannot
    /// invalidate any cache — the SP8 user-global cache stores the STABLE INDEX, which
    /// an in-place value update does not move, and builtin caches key on the NAME's
    /// resolution target, which a value update does not change). This is exactly why a
    /// hot reassigned top-level `let` loop keeps the index cache hot every iteration.
    /// Keeps the class `def_env` in sync for `.from`/typed-parse default resolution.
    /// Caller has confirmed the key exists AND is mutable.
    fn update_user_global(&self, name: &str, value: Value) {
        if let Some(slot) = self.user_globals.borrow_mut().get_mut(name) {
            slot.value = value.clone();
        }
        if let Some(env) = self.class_env.borrow().as_ref() {
            let _ = env.assign(name, value);
        }
    }

    /// Define (create/overwrite) a module-scope user-global with its REASSIGNABILITY
    /// (`mutable` = a `let`; `false` = `const`/`fn`/`class`/`enum`/`import`) and bump
    /// the version.
    fn define_user_global(&self, name: Rc<str>, value: Value, mutable: bool) {
        self.user_globals.borrow_mut().insert(
            name.clone(),
            GlobalSlot {
                value: value.clone(),
                mutable,
            },
        );
        self.bump_global_version();
        // SP8: a DEFINE (insertion) is the ONLY event that can change which stable
        // index a name maps to (a new entry) or introduce a shadow, so it invalidates
        // every `IndexBound` cache. A plain reassignment (`update_user_global`) does
        // NOT bump this — that is the whole point (a hot reassigned-`let` loop keeps
        // the index cache hot). Saturating for an extremely long-lived `Vm`.
        self.struct_gen.set(self.struct_gen.get().saturating_add(1));
        // Keep the lazily-built class `def_env` (used by the SHARED `validate_into`
        // for `.from`/typed-parse field-default resolution) in sync, so a default
        // that references this top-level binding resolves on the `.from` path too.
        if let Some(env) = self.class_env.borrow().as_ref() {
            if env.define(&name, value.clone(), true).is_err() {
                let _ = env.assign(&name, value);
            }
        }
    }

    /// PEP-659 adaptive arithmetic (V11-T4): a guarded fast path in FRONT of the
    /// shared [`crate::interp::apply_binop`], specializing a hot arithmetic site to
    /// the monomorphic operand kind it keeps seeing.
    ///
    /// CORRECTNESS: the fast path runs ONLY after its guard confirms the exact
    /// operand kinds it specialized for, and then performs the SAME computation
    /// `apply_binop` would for those kinds (the `f64`/`Decimal`/concat arms are
    /// copied from `apply_binop`'s own arms). Every other case — a guard miss, a
    /// non-specializable op, or an as-yet-unspecialized site — falls through to
    /// `apply_binop`, which produces the canonical result and panic messages. A
    /// guard miss additionally DEOPTs the site (revert to a fresh warmup). So
    /// specialization can never change a result or a diagnostic; it only skips the
    /// generic dispatch when the kinds match. The whole-corpus differential and
    /// goldens stay byte-identical.
    fn eval_binop_adaptive(
        &self,
        fiber: &Fiber,
        fault_ip: usize,
        op: BinOp,
        a: Value,
        b: Value,
    ) -> Result<Value, Control> {
        use crate::vm::adapt::{ArithCache, ArithKind};

        let chunk = &fiber.frame().closure.proto.chunk;
        // KILL SWITCH (V11-T5): with specialization OFF, never observe/specialize/
        // deopt — go straight through the shared generic `apply_binop`. The result
        // and every panic message are identical to the specialized fast path (which
        // only ever runs `apply_binop`'s own arms behind a guard), so generic and
        // specialized stay byte-identical; the only difference is speed.
        if !self.specialize {
            let span = chunk.span_at(fault_ip);
            return crate::interp::apply_binop(op, a, b, span);
        }
        // Only `+ - * /`-style arithmetic participates; comparisons, equality and
        // range have no monomorphic fast path here (they go straight to generic).
        let arith_op = matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Pow
        );
        if !arith_op {
            let span = chunk.span_at(fault_ip);
            return crate::interp::apply_binop(op, a, b, span);
        }

        let cache = chunk.arith_cache(fault_ip);

        // Already specialized: GUARD the operands; on a hit take the inline fast
        // path, on a miss DEOPT and fall to generic.
        if let Some(kind) = cache.specialized() {
            match (kind, &a, &b) {
                (ArithKind::Number, Value::Float(x), Value::Float(y)) => {
                    // SAME f64 arithmetic as apply_binop's final numeric arm.
                    return Ok(number_fast(op, *x, *y));
                }
                (ArithKind::Decimal, Value::Decimal(x), Value::Decimal(y))
                    if ArithCache::decimal_specializable(op) =>
                {
                    // SAME rust_decimal op as apply_binop's decimal arm. Both
                    // operands are real Decimals (always finite), Add/Sub/Mul only
                    // — no coercion, no div-by-zero.
                    return Ok(decimal_fast(op, *x, *y));
                }
                (ArithKind::ConcatStr, Value::Str(x), Value::Str(y))
                    if matches!(op, BinOp::Add) =>
                {
                    // SAME concat as apply_binop's string arm.
                    return Ok(Value::Str(format!("{}{}", x, y).into()));
                }
                _ => {
                    // Guard miss: deopt and run the generic path.
                    chunk.set_arith_cache(fault_ip, cache.deopt());
                    let span = chunk.span_at(fault_ip);
                    return crate::interp::apply_binop(op, a, b, span);
                }
            }
        }

        // Not specialized yet: OBSERVE this execution's operand kinds (warmup),
        // then run the generic path (the result is identical regardless of warmup).
        let observed = match (&a, &b) {
            (Value::Float(_), Value::Float(_)) => Some(ArithKind::Number),
            (Value::Decimal(_), Value::Decimal(_)) if ArithCache::decimal_specializable(op) => {
                Some(ArithKind::Decimal)
            }
            (Value::Str(_), Value::Str(_)) if matches!(op, BinOp::Add) => {
                Some(ArithKind::ConcatStr)
            }
            _ => None,
        };
        chunk.set_arith_cache(fault_ip, cache.observe(observed));
        let span = chunk.span_at(fault_ip);
        crate::interp::apply_binop(op, a, b, span)
    }

    /// Recompute and store the shape of `obj`'s ObjectCell from its CURRENT keys.
    /// Called after a mutation that may have ADDED a key (reassigning an existing
    /// key leaves the layout — and thus the shape — unchanged, which V11-T3's IC
    /// validity relies on). Walks the full key list through the transition tree;
    /// a no-op-cost path because shared prefixes are deduped.
    fn resync_object_shape(&self, obj: &Cc<crate::value::ObjectCell>) {
        let keys: Vec<String> = obj.map.borrow().keys().cloned().collect();
        let shape = self.object_shape_for(keys.iter().map(|s| s.as_str()));
        obj.shape.set(shape);
    }

    /// Recompute and store an `Instance`'s `shape_id` from its CURRENT field keys.
    /// Called after a `SET_PROP` that may have ADDED an (undeclared) field — which
    /// the runtime allows (`set_member` inserts unconditionally). Re-deriving the
    /// shape keeps the GET_PROP/SET_PROP field IC sound: a changed field LAYOUT
    /// yields a changed shape, so any cache entry keyed by the OLD shape simply
    /// MISSES (and re-resolves) instead of reading a stale index. Reassigning an
    /// existing field leaves the layout — and thus the shape — unchanged.
    fn resync_instance_shape(&self, inst: &Cc<RefCell<crate::value::Instance>>) {
        let keys: Vec<String> = inst.borrow().fields.keys().cloned().collect();
        let shape = self.object_shape_for(keys.iter().map(|s| s.as_str()));
        inst.borrow().shape_id.set(shape);
    }

    /// `SET_PROP` with the field inline cache (V11-T3). Stores `obj.<name> = value`
    /// and returns `value`, BYTE-IDENTICALLY to the generic `set_member` path:
    ///
    /// - **Object, existing field, cache hit (shape unchanged):** write the value
    ///   in place at the cached index via `get_index_mut`. This is identical to
    ///   `IndexMap::insert` of an EXISTING key (same slot, same position), so the
    ///   shape does not change and no resync is needed. Objects carry no field-type
    ///   contracts, so there is nothing to check.
    /// - **Object, miss:** fall to `set_member` (which may ADD a key), then resync
    ///   the object's shape and RECORD the (now-existing) field's index for next
    ///   time. Adding a key transitions the shape, so a prior cache entry for the
    ///   old shape correctly misses.
    /// - **Instance (always):** go through `set_member` so the declared FIELD-TYPE
    ///   CONTRACT is applied exactly as the tree-walker (same panic message/span) —
    ///   the IC never bypasses the contract. Then resync the instance shape (a set
    ///   may have added an undeclared field) and record the field's index.
    /// - **Any other receiver:** `set_member` raises the same Tier-2 "cannot set
    ///   property" panic.
    fn vm_set_prop(
        &self,
        chunk: &crate::vm::chunk::Chunk,
        op_off: usize,
        obj: &Value,
        name: &str,
        value: Value,
        span: Span,
    ) -> Result<Value, Control> {
        // `object.freeze` guard (SP2 §4): BEFORE any write — incl. the IC fast
        // path below, which bypasses `set_member`. Byte-identical to the
        // tree-walker's `set_member` frozen check.
        crate::interp::check_not_frozen(obj, span)?;
        match obj {
            Value::Object(cell) => {
                let shape = cell.shape.get();
                // Fast path (specialize ON only): a shaped, non-schema object whose
                // key already exists at the cached index — write in place (no
                // layout/shape change). KILL SWITCH (V11-T5): skipped when OFF.
                if self.specialize && shape != 0 && !crate::stdlib::schema::is_schema_value(obj) {
                    let ic = chunk.field_ic(op_off);
                    if let Some(idx) = ic.lookup(shape) {
                        let mut map = cell.map.borrow_mut();
                        if let Some((_k, slot)) = map.get_index_mut(idx as usize) {
                            *slot = value.clone();
                            return Ok(value);
                        }
                        // Defensive: stale index → fall through to generic set.
                    }
                }
                // Generic store (may add a key), then resync shape + record index.
                let v = self.interp.set_member(obj, name, value, span, span)?;
                self.resync_object_shape(cell);
                let new_shape = cell.shape.get();
                if self.specialize && new_shape != 0 && !crate::stdlib::schema::is_schema_value(obj)
                {
                    if let Some(idx) = cell.map.borrow().get_index_of(name) {
                        let mut ic = chunk.field_ic(op_off);
                        ic.record(new_shape, idx as u32);
                        chunk.set_field_ic(op_off, ic);
                    }
                }
                Ok(v)
            }
            Value::Instance(inst) => {
                // ALWAYS run the contract check via the shared `set_member`.
                let v = self.interp.set_member(obj, name, value, span, span)?;
                // A set may have added an undeclared field → re-derive the shape so
                // the field IC stays sound, then record this field's index.
                self.resync_instance_shape(inst);
                // KILL SWITCH (V11-T5): only record the field IC when specialize ON.
                let recorded = self.specialize.then(|| {
                    let b = inst.borrow();
                    let new_shape = b.shape_id.get();
                    (new_shape != 0)
                        .then(|| b.fields.get_index_of(name).map(|idx| (new_shape, idx)))
                        .flatten()
                });
                if let Some(Some((new_shape, idx))) = recorded {
                    let mut ic = chunk.field_ic(op_off);
                    ic.record(new_shape, idx as u32);
                    chunk.set_field_ic(op_off, ic);
                }
                Ok(v)
            }
            // Non-settable receiver: shared Tier-2 panic (byte-identical).
            _ => self.interp.set_member(obj, name, value, span, span),
        }
    }

    /// Construct an instance of a VM-registered class (V9). Mirrors the
    /// tree-walker's `construct`: create the instance, apply field DEFAULTS (each
    /// via its compiled thunk closure, so a mutable default is fresh per instance),
    /// checking each default against its field-type contract, then run the compiled
    /// `init` method (if present) with the args; a class with no `init` rejects any
    /// args, byte-identically.
    #[async_recursion::async_recursion(?Send)]
    async fn vm_construct(
        &self,
        class: Rc<crate::value::Class>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let instance = Cc::new(RefCell::new(crate::value::Instance {
            class: class.clone(),
            fields: indexmap::IndexMap::new(),
            // Give the instance its class's BASE shape (the declared-field layout,
            // in declaration order). V11-T3 inline caches key on this.
            shape_id: std::cell::Cell::new(self.class_base_shape(&class)),
            frozen: std::cell::Cell::new(false),
        }));
        let inst_val = Value::Instance(instance.clone());

        // Apply field defaults BASE-CLASS FIRST so a subclass default overrides a
        // base one with the same name (mirrors the tree-walker's `construct`, which
        // iterates `merged_field_schema` — base-first). For each class in the chain
        // (deepest ancestor first), run its defaulted fields' compiled thunks (each
        // thunk is registered under THAT class's identity key) to get a fresh value,
        // check the contract, then store it. The contract panic span is the
        // construct call site (`span`), matching `construct`.
        let mut chain: Vec<Rc<crate::value::Class>> = Vec::new();
        {
            let mut cur = Some(class.clone());
            while let Some(c) = cur {
                cur = c.superclass.clone();
                chain.push(c);
            }
        }
        for c in chain.iter().rev() {
            let key = Rc::as_ptr(c) as usize;
            // Defaulted field names for THIS class, in declared (schema) order.
            let default_names: Vec<String> = self
                .class_defaults
                .borrow()
                .get(&key)
                .map(|m| {
                    c.fields
                        .keys()
                        .filter(|k| m.contains_key(*k))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            for fname in default_names {
                let thunk = self
                    .class_defaults
                    .borrow()
                    .get(&key)
                    .and_then(|m| m.get(&fname))
                    .cloned();
                let Some(thunk) = thunk else { continue };
                let dv = self
                    .call_value(Value::Closure(thunk), Vec::new(), span)
                    .await?;
                if let Some(schema) = c.fields.get(&fname) {
                    if !crate::interp::check_type(&dv, &schema.ty) {
                        return Err(crate::interp::contract_panic(&schema.ty, &dv, span));
                    }
                }
                instance.borrow_mut().fields.insert(fname, dv);
            }
        }

        // Run the compiled `init`, if any — resolved up the chain (a subclass may
        // inherit the base init). `def_class` is the class that DEFINED init, so a
        // `super.init(...)` inside it resolves from the correct link.
        if let Some((init, def_class)) = self.find_compiled_method(&class, "init") {
            self.invoke_compiled_method(init, inst_val.clone(), args, span, Some(def_class))
                .await?;
        } else {
            // SP2 §5 records: no explicit `init` → auto-derive a positional
            // constructor over the declared fields (merged base-first order).
            // Defaults were already applied above; the positional args OVERRIDE
            // the supplied leading fields, each contract-checked via the SHARED
            // `auto_init_bindings` helper — byte-identical arity/contract messages
            // to the tree-walker's `construct`. A zero-field class with no args is
            // unchanged (empty params → only `C()` valid).
            let fields = crate::value::merged_field_schema(&class);
            let bindings = crate::interp::auto_init_bindings(&fields, &class.name, args, span)?;
            for (fname, v) in bindings {
                instance.borrow_mut().fields.insert(fname, v);
            }
        }
        // Re-derive the shape from the instance's ACTUAL fields now that defaults +
        // `init` have populated them. The base shape set above reflects the FULL
        // declared schema, but `fields` only holds what was actually inserted (and
        // in insertion order), so a field IC keying on `shape_id` must see the real
        // layout — otherwise two instances sharing the base shape but with different
        // actual layouts could read a wrong index. (V11-T3 IC soundness.)
        self.resync_instance_shape(&instance);
        Ok(inst_val)
    }

    /// Invoke a COMPILED method closure with `self`=`receiver` bound to slot 0 and
    /// the arguments bound to slots `1..n+1`. The method proto's `arity`/`params`
    /// EXCLUDE `self` (the resolver declares `self` as the method frame's slot 0,
    /// the compiler builds the params from the user params), so arity + per-param
    /// contracts use the SAME `check_call_args` every other call path uses — the
    /// arg contract panic is byte-identical. Drives a fresh one-frame Fiber to
    /// completion (a non-generator/non-async method body cannot `yield`). Async
    /// methods are out of scope for V9-T1 (deferred — a sync `init`/method is the
    /// T1 surface).
    #[async_recursion::async_recursion(?Send)]
    async fn invoke_compiled_method(
        &self,
        closure: Cc<Closure>,
        receiver: Value,
        args: Vec<Value>,
        span: Span,
        def_class: Option<Rc<crate::value::Class>>,
    ) -> Result<Value, Control> {
        let what = closure.proto.chunk.name.as_deref().unwrap_or("method");
        // Bind the user args (arity + per-param contracts + rest) against the
        // method's declared params (which EXCLUDE self) — shared with every call
        // path. The bound values land in slots 1.. (self is slot 0).
        let bound = crate::interp::check_call_args(&closure.proto.params, args, span, what)?;
        // A generator method (`fn*` / `async fn*`) is NOT run inline: it binds `self`
        // and args into a NOT-STARTED fiber and wraps it in a VM-backed
        // `GeneratorHandle`, returning a `Value::Generator` immediately — exactly like
        // the standalone-generator CALL path (`Op::Call`) and the tree-walker's
        // `invoke_method` generator branch. The body runs only when the consumer
        // drives it via `gen.next()` / `for await`; `self` (slot 0) is visible to a
        // `yield self.x`. Both sync and async generator methods take this path.
        if closure.proto.is_generator {
            let mut gfiber = Fiber::new(closure);
            gfiber.frame_mut().ret_span = span;
            gfiber.frame_mut().def_class = def_class;
            gfiber.frame_mut().argc = bound.supplied;
            let cells = gfiber.frame().cells.clone();
            // self -> slot 0 (cell-aware).
            if let Some(cell) = &cells[0] {
                *cell.borrow_mut() = receiver;
            } else {
                gfiber.stack[0] = receiver;
            }
            // bound args -> slots 1..n+1 (cell-aware).
            for (i, v) in bound.values.into_iter().enumerate() {
                let slot = i + 1;
                if let Some(cell) = &cells[slot] {
                    *cell.borrow_mut() = v;
                } else {
                    gfiber.stack[slot] = v;
                }
            }
            let handle =
                crate::coro::GeneratorHandle::new_vm(gfiber, Rc::downgrade(&self.rc()));
            return Ok(Value::Generator(Rc::new(handle)));
        }
        let mut fiber = Fiber::new(closure);
        fiber.frame_mut().ret_span = span;
        // Record the DEFINING class so a `super.<name>` in this method body
        // (Op::GetSuper) resolves up from `def_class.superclass`, exactly like the
        // tree-walker's `invoke_method` super binding.
        fiber.frame_mut().def_class = def_class;
        fiber.frame_mut().argc = bound.supplied;
        let cells = fiber.frame().cells.clone();
        // self -> slot 0 (cell-aware, in case a nested closure captured self).
        if let Some(cell) = &cells[0] {
            *cell.borrow_mut() = receiver;
        } else {
            fiber.stack[0] = receiver;
        }
        // bound args -> slots 1..n+1.
        for (i, v) in bound.values.into_iter().enumerate() {
            let slot = i + 1;
            if let Some(cell) = &cells[slot] {
                *cell.borrow_mut() = v;
            } else {
                fiber.stack[slot] = v;
            }
        }
        // SP3 §B: native re-entry into `Vm::run` — guard the fiber's initial
        // (method) frame as one logical call (RAII, the root pop does not
        // decrement, so the guard owns this unit). Matches one tree-walker
        // `run_body`.
        let _depth = self.interp.enter_call_depth_scoped(span)?;
        // SP9 §1: native re-entry funnel for non-IC method dispatch — grow the
        // native stack per poll (see `call_value`).
        match crate::vm::stack::grow_future(self.run(&mut fiber)).await? {
            RunOutcome::Done(v) => Ok(v),
            RunOutcome::Yielded(_) => {
                unreachable!("a non-generator method cannot yield")
            }
        }
    }

    /// Dispatch a compiled STATIC method (SP1 §3): a class-level call with NO
    /// receiver. Args bind to slots `0..n` (no `self` slot). A `static fn*` returns
    /// a `Value::Generator`; a `static async fn` is scheduled eagerly and returns a
    /// `Value::Future`; a plain static runs to completion. Mirrors the `Op::Call`
    /// closure arms and the tree-walker's `call_static_method` so the engines agree.
    #[async_recursion::async_recursion(?Send)]
    async fn invoke_compiled_static(
        &self,
        closure: Cc<Closure>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        // `static async fn`: schedule eagerly (M17), return a `Value::Future`. The
        // body re-enters via `Vm::call_value` inside the spawned task with the RAW
        // args (so the arity/contract check runs INSIDE the task and surfaces
        // lazily at `await`, byte-identical to the tree-walker and the `Op::Call`
        // async-closure arm). Handled before arg-binding so the bind happens once.
        if closure.proto.is_async {
            let vm = self.rc();
            let fut = crate::task::SharedFuture::new();
            let cell = fut.cell();
            let guard = self.interp.inflight_guard();
            let handle = tokio::task::spawn_local(async move {
                let _g = guard;
                let r = vm.call_value(Value::Closure(closure), args, span).await;
                cell.resolve(r);
            });
            fut.set_abort(handle.abort_handle());
            self.interp.maybe_yield_for_inflight().await;
            return Ok(Value::Future(fut));
        }
        let what = closure.proto.chunk.name.as_deref().unwrap_or("function");
        let bound = crate::interp::check_call_args(&closure.proto.params, args, span, what)?;
        // `static fn*` / `static async fn*`: build a NOT-STARTED fiber, bind args
        // into slots 0.., wrap in a VM `GeneratorHandle`. No receiver/self slot.
        if closure.proto.is_generator {
            let mut gfiber = Fiber::new(closure);
            gfiber.frame_mut().ret_span = span;
            gfiber.frame_mut().argc = bound.supplied;
            let cells = gfiber.frame().cells.clone();
            for (slot, v) in bound.values.into_iter().enumerate() {
                if let Some(cell) = &cells[slot] {
                    *cell.borrow_mut() = v;
                } else {
                    gfiber.stack[slot] = v;
                }
            }
            let handle = crate::coro::GeneratorHandle::new_vm(gfiber, Rc::downgrade(&self.rc()));
            return Ok(Value::Generator(Rc::new(handle)));
        }
        // Plain sync static: run a fresh one-frame fiber to completion (args bound
        // into slots 0.., no receiver) — mirrors `invoke_compiled_method`'s sync
        // tail without the `self` slot.
        let mut fiber = Fiber::new(closure);
        fiber.frame_mut().ret_span = span;
        fiber.frame_mut().argc = bound.supplied;
        let cells = fiber.frame().cells.clone();
        for (slot, v) in bound.values.into_iter().enumerate() {
            if let Some(cell) = &cells[slot] {
                *cell.borrow_mut() = v;
            } else {
                fiber.stack[slot] = v;
            }
        }
        // SP9 §1: native re-entry funnel for static-method dispatch — grow the
        // native stack per poll (see `call_value`).
        match crate::vm::stack::grow_future(self.run(&mut fiber)).await? {
            RunOutcome::Done(v) => Ok(v),
            RunOutcome::Yielded(_) => unreachable!("a non-generator static cannot yield"),
        }
    }

    /// Build a Tier-2 [`Control::Panic`] whose [`AsError`] is anchored at the span
    /// of the instruction at `ip`, so ariadne points at the source exactly like
    /// the tree-walker.
    fn panic_at(&self, fiber: &Fiber, ip: usize, msg: String) -> Control {
        let chunk = &fiber.frame().closure.proto.chunk;
        let span = chunk.span_at(ip);
        // Bind the span to its OWN module's source (SP4 §3) so a panic raised in
        // one module renders its caret in that module's file even when the error
        // propagates up to a caller in a different module. `None` (no module
        // source bound — e.g. an `.aso` with no source) falls back to the entry
        // source at the top of the run, preserving single-module behavior.
        match chunk.source.borrow().as_ref() {
            Some(src) => Control::Panic(AsError::at_in(msg, span, src.clone())),
            None => Control::Panic(AsError::at(msg, span)),
        }
    }

    /// Unwind ONE call frame, returning `value` from it.
    ///
    /// Shared by `Op::Return` (a normal `return v`) and `Op::Propagate` (a `?`
    /// early-return of a `[nil, err]` pair) — the two have the same mechanics:
    /// pop the current frame; if it declared a `: T` return contract, check the
    /// returned value against it (panicking exactly as the tree-walker's
    /// `run_body` does — anchored at the CALL-site span `frame.ret_span`, with the
    /// identical message — and note the tree-walker applies this same contract to a
    /// `Control::Propagate`-derived value too); truncate the stack back to the
    /// frame's `slot_base` (discarding the callee's locals/operands). Dropping the
    /// frame releases ITS cell `Rc`s — closures that captured them keep their own
    /// strong refs, so by-reference captures stay alive. Recursion is heap-bounded:
    /// each CALL pushed a heap frame and this pops one, so the Rust stack stays flat.
    ///
    /// Returns `Ok(Some(outcome))` when the ROOT frame was popped — the program is
    /// done and `outcome` is its result (the driver treats a top-level propagated
    /// pair as `Ok`, exactly like `run_file`'s `Control::Propagate => Ok`). Returns
    /// `Ok(None)` when a caller frame remains — `value` was pushed onto its stack
    /// and execution continues there.
    fn return_from_frame(
        &self,
        fiber: &mut Fiber,
        value: Value,
    ) -> Result<Option<RunOutcome>, Control> {
        let frame = fiber
            .frames
            .pop()
            .expect("return/propagate with no active frame (VM bug)");
        if let Some(ret_ty) = &frame.closure.proto.ret {
            if !crate::interp::check_type(&value, ret_ty) {
                return Err(crate::interp::contract_panic(
                    ret_ty,
                    &value,
                    frame.ret_span,
                ));
            }
        }
        fiber.stack.truncate(frame.slot_base);
        if fiber.frames.is_empty() {
            // ROOT/initial frame of this fiber — its logical-depth unit is owned by
            // the program root (counter returns to 0 at program end) or by the
            // re-entrant `self.run`'s RAII guard, so do NOT decrement here.
            return Ok(Some(RunOutcome::Done(value)));
        }
        // SP3 §B: a non-root frame was popped — match the `enter_frame_depth`
        // increment from when it was pushed.
        self.leave_frame_depth();
        fiber.push(value);
        Ok(None)
    }
}

/// Map a binary-operator opcode to the shared [`BinOp`] the tree-walker uses, so
/// both engines run the SAME `apply_binop` dispatch. Short-circuit operators
/// (`&&`/`||`/`??`) are never lowered to a single binary opcode — the compiler
/// emits jumps for them (V2-T6) — so they have no opcode and never reach here.
fn binop_of(op: Op) -> BinOp {
    match op {
        Op::Add => BinOp::Add,
        Op::Sub => BinOp::Sub,
        Op::Mul => BinOp::Mul,
        Op::Div => BinOp::Div,
        Op::Mod => BinOp::Mod,
        Op::Pow => BinOp::Pow,
        Op::Lt => BinOp::Lt,
        Op::Le => BinOp::Le,
        Op::Gt => BinOp::Gt,
        Op::Ge => BinOp::Ge,
        Op::Eq => BinOp::Eq,
        Op::Ne => BinOp::Ne,
        Op::Range => BinOp::Range,
        Op::InstanceOf => BinOp::InstanceOf,
        _ => unreachable!("binop_of called with non-binary opcode {op:?}"),
    }
}

/// Inline numeric arithmetic for the `ADD_NUMBER`-family fast path (V11-T4).
/// BYTE-IDENTICAL to [`crate::interp::apply_binop`]'s final two-`Number` arm — the
/// same `f64` ops, so the specialized result equals the generic one bit-for-bit
/// (incl. `NaN`/`Infinity`/`-0.0`). Only the arithmetic ops reach here (the
/// adaptive guard restricts to Add/Sub/Mul/Div/Mod/Pow over two `Number`s).
#[inline]
fn number_fast(op: BinOp, a: f64, b: f64) -> Value {
    match op {
        BinOp::Add => Value::Float(a + b),
        BinOp::Sub => Value::Float(a - b),
        BinOp::Mul => Value::Float(a * b),
        BinOp::Div => Value::Float(a / b),
        BinOp::Mod => Value::Float(a % b),
        BinOp::Pow => Value::Float(a.powf(b)),
        _ => unreachable!("number_fast called with non-arithmetic op {op:?}"),
    }
}

/// Inline decimal arithmetic for the `ADD_DECIMAL`-family fast path (V11-T4).
/// BYTE-IDENTICAL to [`crate::interp::apply_binop`]'s decimal Add/Sub/Mul arms.
/// Restricted by the adaptive guard to Add/Sub/Mul over two real `Decimal`s
/// (always finite), so there is no coercion, no non-finite check and no
/// div-by-zero to defer — those operators/operands never specialize and always go
/// generic.
#[inline]
fn decimal_fast(op: BinOp, a: rust_decimal::Decimal, b: rust_decimal::Decimal) -> Value {
    match op {
        BinOp::Add => Value::Decimal(a + b),
        BinOp::Sub => Value::Decimal(a - b),
        BinOp::Mul => Value::Decimal(a * b),
        _ => unreachable!("decimal_fast called with non-specializable op {op:?}"),
    }
}

/// Map a unary-operator opcode to the shared [`UnOp`].
fn unop_of(op: Op) -> UnOp {
    match op {
        Op::Neg => UnOp::Neg,
        Op::Not => UnOp::Not,
        _ => unreachable!("unop_of called with non-unary opcode {op:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;
    use crate::vm::chunk::{Chunk, FnProto};
    use crate::vm::value_ext::Closure;
    use tokio::task::LocalSet;

    /// Wrap a chunk in a closure + fiber and run it to completion on a
    /// current-thread runtime inside a `LocalSet` (the runtime is `!Send`).
    fn run_chunk(chunk: Chunk) -> Result<RunOutcome, Control> {
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
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp);
            vm.run(&mut fiber).await
        })
    }

    fn expect_number(chunk: Chunk) -> f64 {
        match run_chunk(chunk).expect("run ok") {
            RunOutcome::Done(Value::Float(n)) => n,
            other => panic!("expected Done(Number), got {other:?}"),
            #[allow(unreachable_patterns)]
            _ => unreachable!(),
        }
    }

    // `RunOutcome` has no Debug; small helper for assert messages.
    impl std::fmt::Debug for RunOutcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                RunOutcome::Done(v) => write!(f, "Done({v:?})"),
                RunOutcome::Yielded(v) => write!(f, "Yielded({v:?})"),
            }
        }
    }

    fn s() -> Span {
        Span::new(0, 1)
    }

    #[test]
    fn arithmetic_one_plus_two_times_four() {
        // (1 + 2) * 4 == 12
        let mut c = Chunk::new();
        let k1 = c.add_const(Value::Float(1.0));
        let k2 = c.add_const(Value::Float(2.0));
        let k4 = c.add_const(Value::Float(4.0));
        c.emit_u16(Op::Const, k1, s());
        c.emit_u16(Op::Const, k2, s());
        c.emit(Op::Add, s());
        c.emit_u16(Op::Const, k4, s());
        c.emit(Op::Mul, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 12.0);
    }

    #[test]
    fn negate() {
        let mut c = Chunk::new();
        let k = c.add_const(Value::Float(5.0));
        c.emit_u16(Op::Const, k, s());
        c.emit(Op::Neg, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), -5.0);
    }

    #[test]
    fn modulo() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Float(7.0));
        let b = c.add_const(Value::Float(3.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Mod, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 1.0);
    }

    #[test]
    fn power() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Float(2.0));
        let b = c.add_const(Value::Float(10.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Pow, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 1024.0);
    }

    #[test]
    fn less_than_true() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Float(1.0));
        let b = c.add_const(Value::Float(2.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Lt, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("run ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn not_on_truthy() {
        let mut c = Chunk::new();
        c.emit(Op::True, s());
        c.emit(Op::Not, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("run ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(!b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn eq_numbers() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Float(3.0));
        let b = c.add_const(Value::Float(3.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Eq, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("run ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn neg_non_number_panics_with_span() {
        // Push a Str const, then NEG -> "cannot negate" panic with a real span.
        let mut c = Chunk::new();
        let k = c.add_const(Value::Str(Rc::from("nope")));
        c.emit_u16(Op::Const, k, s());
        // give NEG a distinct, non-empty span so we can assert it is carried.
        let neg_span = Span::new(5, 9);
        c.emit(Op::Neg, neg_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("cannot negate"),
                    "message was: {}",
                    e.message
                );
                let span = e.span.expect("panic carries a span");
                assert_eq!(span, neg_span, "panic carries the faulting op's span");
                assert!(span.end > span.start, "span is non-empty");
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn add_non_numbers_panics() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Str(Rc::from("a")));
        let b = c.add_const(Value::Float(1.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Add, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => assert!(
                e.message.contains("operator requires two numbers"),
                "message was: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    /// A `Value::Decimal` from a decimal string literal (test helper). The VM
    /// compiler cannot yet *produce* a decimal (that needs `import`/member-access
    /// for `std/decimal`), so the decimal arithmetic path is exercised by pushing
    /// decimal consts directly. The semantics themselves are the SAME shared
    /// `apply_binop` the tree-walker runs, so these tests pin the VM's dispatch to
    /// it.
    fn dec(s: &str) -> Value {
        use std::str::FromStr;
        Value::Decimal(rust_decimal::Decimal::from_str(s).expect("valid decimal literal"))
    }

    /// Push two decimal consts and apply `op`, returning the run outcome.
    fn run_decimal_binop(a: &str, op: Op, b: &str) -> Result<RunOutcome, Control> {
        let mut c = Chunk::new();
        let ka = c.add_const(dec(a));
        let kb = c.add_const(dec(b));
        c.emit_u16(Op::Const, ka, s());
        c.emit_u16(Op::Const, kb, s());
        c.emit(op, s());
        c.emit(Op::Return, s());
        run_chunk(c)
    }

    #[test]
    fn decimal_arithmetic_through_shared_dispatch() {
        // Add / Sub / Mul / Div over two decimals → Decimal, formatted exactly.
        // Expected renderings preserve rust_decimal's scale exactly (the same
        // `Value::Display` the tree-walker uses), so e.g. `3 / 2` is `1.50`.
        for (a, op, b, want) in [
            ("1.5", Op::Add, "2.5", "4.0"),
            ("2.5", Op::Sub, "0.5", "2.0"),
            ("1.5", Op::Mul, "2", "3.0"),
            ("3", Op::Div, "2", "1.50"),
        ] {
            match run_decimal_binop(a, op, b).expect("decimal arith ok") {
                RunOutcome::Done(v) => {
                    assert_eq!(v.to_string(), want, "{a} {op:?} {b} rendered wrong")
                }
                other => panic!("expected Done, got {other:?}"),
            }
        }
    }

    #[test]
    fn decimal_division_by_zero_panics() {
        match run_decimal_binop("1", Op::Div, "0") {
            Err(Control::Panic(e)) => {
                assert_eq!(e.message, "decimal division by zero", "msg: {}", e.message)
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn decimal_remainder_by_zero_panics() {
        match run_decimal_binop("1", Op::Mod, "0") {
            Err(Control::Panic(e)) => {
                assert_eq!(e.message, "decimal remainder by zero", "msg: {}", e.message)
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn decimal_pow_is_unsupported() {
        match run_decimal_binop("2", Op::Pow, "3") {
            Err(Control::Panic(e)) => assert_eq!(
                e.message,
                "exponentiation (**) is not supported for decimal; use math.pow or convert to number",
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn decimal_ordering_through_shared_dispatch() {
        match run_decimal_binop("1.5", Op::Lt, "2.5").expect("ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
        match run_decimal_binop("3", Op::Ge, "3").expect("ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn decimal_vs_number_cross_equality() {
        // decimal("1") == 1 → true (cross-type Decimal↔Number equality), exactly
        // as the tree-walker's `decimal_cross_eq`.
        let mut c = Chunk::new();
        let kd = c.add_const(dec("1"));
        let kn = c.add_const(Value::Float(1.0));
        c.emit_u16(Op::Const, kd, s());
        c.emit_u16(Op::Const, kn, s());
        c.emit(Op::Eq, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b, "decimal(1) == 1 should be true"),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn range_op_builds_half_open_array() {
        // 0 .. 5 → [0, 1, 2, 3, 4].
        let mut c = Chunk::new();
        let k0 = c.add_const(Value::Float(0.0));
        let k5 = c.add_const(Value::Float(5.0));
        c.emit_u16(Op::Const, k0, s());
        c.emit_u16(Op::Const, k5, s());
        c.emit(Op::Range, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Array(a)) => {
                let got: Vec<f64> = a
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Float(n) => *n,
                        other => panic!("non-number in range array: {other:?}"),
                    })
                    .collect();
                assert_eq!(got, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
            }
            other => panic!("expected Done(Array), got {other:?}"),
        }
    }

    #[test]
    fn range_op_non_number_bounds_panics() {
        let mut c = Chunk::new();
        let ks = c.add_const(Value::Str(Rc::from("x")));
        let k5 = c.add_const(Value::Float(5.0));
        c.emit_u16(Op::Const, ks, s());
        c.emit_u16(Op::Const, k5, s());
        c.emit(Op::Range, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert_eq!(
                    e.message, "range bounds must be numbers",
                    "msg: {}",
                    e.message
                )
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn string_concat_through_add() {
        let mut c = Chunk::new();
        let ka = c.add_const(Value::Str(Rc::from("foo")));
        let kb = c.add_const(Value::Str(Rc::from("bar")));
        c.emit_u16(Op::Const, ka, s());
        c.emit_u16(Op::Const, kb, s());
        c.emit(Op::Add, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Str(st)) => assert_eq!(&*st, "foobar"),
            other => panic!("expected Done(Str), got {other:?}"),
        }
    }

    /// Run a chunk and return the shared interp's captured output alongside the
    /// outcome — for exercising the `print` builtin via `CALL`.
    fn run_chunk_with_output(chunk: Chunk) -> (Result<RunOutcome, Control>, String) {
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
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp.clone());
            let outcome = vm.run(&mut fiber).await;
            (outcome, interp.output())
        })
    }

    #[test]
    fn call_print_writes_to_shared_sink() {
        // GET_GLOBAL print; CONST 42; CALL 1; RETURN (CALL leaves print's nil
        // result, which RETURN pops).
        let mut c = Chunk::new();
        let name = c.add_const(Value::Str(Rc::from("print")));
        c.emit_u16(Op::GetGlobal, name, s());
        let k = c.add_const(Value::Float(42.0));
        c.emit_u16(Op::Const, k, s());
        c.emit_u8(Op::Call, 1, s());
        c.emit(Op::Return, s());
        let (outcome, out) = run_chunk_with_output(c);
        assert!(matches!(outcome, Ok(RunOutcome::Done(_))), "ran ok");
        assert_eq!(out, "42\n", "print wrote to the shared capture sink");
    }

    #[test]
    fn get_global_undefined_panics() {
        let mut c = Chunk::new();
        let name = c.add_const(Value::Str(Rc::from("not_a_builtin")));
        let gg_span = Span::new(3, 16);
        c.emit_u16(Op::GetGlobal, name, gg_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                // The message matches the tree-walker's runtime undefined-name
                // error exactly (`undefined variable '<name>'`), so the two
                // engines stay byte-identical even on this defence-in-depth path.
                assert!(
                    e.message.contains("undefined variable"),
                    "message was: {}",
                    e.message
                );
                assert_eq!(e.span, Some(gg_span));
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn unimplemented_op_panics() {
        // An opcode with no exec arm must surface a span-carrying "not yet
        // implemented" Tier-2 panic. `MAKE_GENERATOR` is never emitted by the
        // compiler (a `fn*` CALL builds the generator directly in the CALL arm,
        // mirroring the tree-walker), so it remains unimplemented — a good probe
        // for the catch-all guard. (JUMP/JUMP_IF_* land in V2-T6, AWAIT in V7,
        // YIELD in V8.)
        let mut c = Chunk::new();
        let op_span = Span::new(2, 4);
        c.emit(Op::Nil, s());
        c.emit(Op::MakeGenerator, op_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("not yet implemented"),
                    "message was: {}",
                    e.message
                );
                assert_eq!(e.span, Some(op_span));
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    // ---- await exec arm (V7) ---------------------------------------------

    #[test]
    fn await_non_future_is_identity() {
        // `await 5` is identity on a non-future, exactly like the tree-walker's
        // `ExprKind::Await` (`other => Ok(other)`).
        let mut c = Chunk::new();
        let k = c.add_const(Value::Float(5.0));
        c.emit_u16(Op::Const, k, s());
        c.emit(Op::Await, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 5.0);
    }

    // ---- jump exec arms (V2-T6) -------------------------------------------

    #[test]
    fn jump_skips_intervening_code() {
        // NIL is pushed, then an unconditional JUMP hops over a CONST 999, so the
        // result is `nil` (proving the jump landed past the skipped push).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let site = c.emit_jump(Op::Jump, s());
        let k = c.add_const(Value::Float(999.0));
        c.emit_u16(Op::Const, k, s()); // skipped
        c.patch_jump(site); // land here, leaving only NIL
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Nil) => {}
            other => panic!("expected Done(Nil), got {other:?}"),
        }
    }

    #[test]
    fn jump_if_false_pops_and_branches_on_falsy() {
        // FALSE on the stack -> JUMP_IF_FALSE pops it and jumps; the CONST 1 in
        // between is skipped, so RETURN sees the trailing CONST 2.
        let mut c = Chunk::new();
        c.emit(Op::False, s());
        let site = c.emit_jump(Op::JumpIfFalse, s());
        let k1 = c.add_const(Value::Float(1.0));
        c.emit_u16(Op::Const, k1, s()); // skipped (would otherwise be the result)
        c.patch_jump(site);
        let k2 = c.add_const(Value::Float(2.0));
        c.emit_u16(Op::Const, k2, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 2.0);
    }

    #[test]
    fn jump_if_true_pops_and_falls_through_on_falsy() {
        // FALSE -> JUMP_IF_TRUE pops, does NOT jump, falls through to CONST 7.
        let mut c = Chunk::new();
        c.emit(Op::False, s());
        let site = c.emit_jump(Op::JumpIfTrue, s());
        let k7 = c.add_const(Value::Float(7.0));
        c.emit_u16(Op::Const, k7, s()); // executed (no jump)
        c.emit(Op::Return, s());
        c.patch_jump(site); // target is past RETURN; never reached
        assert_eq!(expect_number(c), 7.0);
    }

    #[test]
    fn jump_if_not_nil_pops_and_branches_on_non_nil() {
        // CONST 5 (non-nil) -> JUMP_IF_NOT_NIL pops & jumps over CONST 1; RETURN
        // sees the trailing CONST 2.
        let mut c = Chunk::new();
        let k5 = c.add_const(Value::Float(5.0));
        c.emit_u16(Op::Const, k5, s());
        let site = c.emit_jump(Op::JumpIfNotNil, s());
        let k1 = c.add_const(Value::Float(1.0));
        c.emit_u16(Op::Const, k1, s()); // skipped
        c.patch_jump(site);
        let k2 = c.add_const(Value::Float(2.0));
        c.emit_u16(Op::Const, k2, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 2.0);
    }

    // ---- collections: literals + index/member read (V2-T4b) ---------------

    #[test]
    fn new_array_preserves_source_order() {
        // CONST 1; CONST 2; CONST 3; NEW_ARRAY 3 → [1, 2, 3].
        let mut c = Chunk::new();
        for n in [1.0, 2.0, 3.0] {
            let k = c.add_const(Value::Float(n));
            c.emit_u16(Op::Const, k, s());
        }
        c.emit_u16(Op::NewArray, 3, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Array(a)) => {
                let got: Vec<f64> = a
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Float(n) => *n,
                        other => panic!("non-number: {other:?}"),
                    })
                    .collect();
                assert_eq!(got, vec![1.0, 2.0, 3.0]);
            }
            other => panic!("expected Done(Array), got {other:?}"),
        }
    }

    #[test]
    fn new_object_builds_indexmap_in_order() {
        // CONST "a"; CONST 1; CONST "b"; CONST 2; NEW_OBJECT 2 → {a:1, b:2}.
        let mut c = Chunk::new();
        for (k, v) in [("a", 1.0), ("b", 2.0)] {
            let ki = c.add_const(Value::Str(Rc::from(k)));
            c.emit_u16(Op::Const, ki, s());
            let vi = c.add_const(Value::Float(v));
            c.emit_u16(Op::Const, vi, s());
        }
        c.emit_u16(Op::NewObject, 2, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Object(o)) => {
                let b = o.borrow();
                let keys: Vec<&str> = b.keys().map(|k| k.as_str()).collect();
                assert_eq!(keys, vec!["a", "b"], "keys in insertion order");
                assert_eq!(b.get("a"), Some(&Value::Float(1.0)));
                assert_eq!(b.get("b"), Some(&Value::Float(2.0)));
            }
            other => panic!("expected Done(Object), got {other:?}"),
        }
    }

    #[test]
    fn get_index_array() {
        // [10, 20, 30]; CONST 1; GET_INDEX → 20.
        let mut c = Chunk::new();
        for n in [10.0, 20.0, 30.0] {
            let k = c.add_const(Value::Float(n));
            c.emit_u16(Op::Const, k, s());
        }
        c.emit_u16(Op::NewArray, 3, s());
        let i = c.add_const(Value::Float(1.0));
        c.emit_u16(Op::Const, i, s());
        c.emit(Op::GetIndex, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 20.0);
    }

    #[test]
    fn get_index_out_of_bounds_panics() {
        let mut c = Chunk::new();
        let k = c.add_const(Value::Float(10.0));
        c.emit_u16(Op::Const, k, s());
        c.emit_u16(Op::NewArray, 1, s());
        let i = c.add_const(Value::Float(5.0));
        c.emit_u16(Op::Const, i, s());
        c.emit(Op::GetIndex, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(e.message.contains("out of bounds"), "msg: {}", e.message)
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn get_index_object_missing_key_is_nil() {
        // {a:1}["b"] → nil (missing object key is nil, not a panic).
        let mut c = Chunk::new();
        let ka = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::Const, ka, s());
        let v1 = c.add_const(Value::Float(1.0));
        c.emit_u16(Op::Const, v1, s());
        c.emit_u16(Op::NewObject, 1, s());
        let kb = c.add_const(Value::Str(Rc::from("b")));
        c.emit_u16(Op::Const, kb, s());
        c.emit(Op::GetIndex, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Nil) => {}
            other => panic!("expected Done(Nil), got {other:?}"),
        }
    }

    #[test]
    fn get_prop_object_field() {
        // {a:1}.a → 1 via GET_PROP "a".
        let mut c = Chunk::new();
        let ka = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::Const, ka, s());
        let v1 = c.add_const(Value::Float(1.0));
        c.emit_u16(Op::Const, v1, s());
        c.emit_u16(Op::NewObject, 1, s());
        let name = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::GetProp, name, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 1.0);
    }

    #[test]
    fn get_prop_opt_nil_receiver_is_nil() {
        // nil?.a → nil (short-circuit, no read_member call).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let name = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::GetPropOpt, name, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Nil) => {}
            other => panic!("expected Done(Nil), got {other:?}"),
        }
    }

    #[test]
    fn get_prop_nil_receiver_panics() {
        // nil.a → "cannot read property 'a' of nil" (NOT short-circuited).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let name = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::GetProp, name, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => assert!(
                e.message.contains("cannot read property 'a' of nil"),
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    // ---- Vm::call_value bridge (native → VM closures), V4-T5 ---------------

    /// Compile a program whose trailing expression evaluates to a closure, run it
    /// on the VM, and return that `Value::Closure`. This is how a native
    /// higher-order function would *receive* a user callback (e.g. the `f` arg of
    /// `array.map`). The closure is self-contained (proto + captured upvalue
    /// cells), so a fresh VM can later drive it via `Vm::call_value`.
    fn compile_closure(src: &str) -> Value {
        let chunk = crate::compile::compile_source(src).expect("compile ok");
        match run_chunk(chunk).expect("run ok") {
            RunOutcome::Done(v @ Value::Closure(_)) => v,
            other => panic!("expected the program to yield a closure, got {other:?}"),
        }
    }

    /// Run `body(vm)` on a current-thread runtime inside a `LocalSet` with a fresh
    /// `Vm` over a fresh `Interp`, mirroring the production entry points. Returns
    /// whatever the async body returns.
    fn with_vm<F, Fut, T>(body: F) -> T
    where
        F: FnOnce(Rc<Vm>) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp);
            body(vm).await
        })
    }

    #[test]
    fn call_value_runs_a_vm_closure_with_native_supplied_args() {
        // The exact `array.map` shape: a native caller hands the closure ONE arg
        // per element. `(x) => x * 2` called with 21 → 42.
        let f = compile_closure("(x) => x * 2");
        let got = with_vm(|vm| async move {
            vm.call_value(f, vec![Value::Float(21.0)], s())
                .await
                .expect("call ok")
        });
        assert!(matches!(got, Value::Float(n) if n == 42.0), "got {got:?}");
    }

    #[test]
    fn call_value_invokes_a_closure_repeatedly_each_on_its_own_fiber() {
        // A native HOF calls the SAME closure once per element; each invocation is
        // an independent Fiber, so there is no cross-call state leakage.
        let f = compile_closure("(x) => x + 1");
        let got = with_vm(|vm| async move {
            let mut out = Vec::new();
            for n in [10.0, 20.0, 30.0] {
                let v = vm
                    .call_value(f.clone(), vec![Value::Float(n)], s())
                    .await
                    .expect("call ok");
                out.push(v);
            }
            out
        });
        let nums: Vec<f64> = got
            .iter()
            .map(|v| match v {
                Value::Float(n) => *n,
                other => panic!("non-number: {other:?}"),
            })
            .collect();
        assert_eq!(nums, vec![11.0, 21.0, 31.0]);
    }

    #[test]
    fn call_value_closure_observes_its_captured_upvalue() {
        // A closure capturing an outer FUNCTION-LOCAL `k` and applied to a
        // native-supplied arg — exactly `array.map([..], (x) => x + k)` inside a fn.
        // The captured cell travels WITH the closure value (it is a genuine upvalue,
        // not a module global), so a fresh VM driving it still sees k = 10. (A
        // top-level `let k` would instead be a module global read via GET_GLOBAL.)
        let f = compile_closure("fn make() {\n let k = 10\n return (x) => x + k\n}\nmake()");
        let got = with_vm(|vm| async move {
            vm.call_value(f, vec![Value::Float(5.0)], s())
                .await
                .expect("call ok")
        });
        assert!(matches!(got, Value::Float(n) if n == 15.0), "got {got:?}");
    }

    // ---- V7-T4: structured-concurrency over VM-produced futures -----------
    //
    // The std/task ops (`gather`/`race`/`timeout`/`spawn`) are native fns on the
    // shared `Interp` that await/select over `Value::Future`s. The VM produces
    // ordinary `Value::Future`s (the SAME `SharedFuture` the tree-walker uses;
    // see the `Op::Call` async-fn arm). These tests de-risk the V12 end-to-end
    // structured-concurrency differential (`concurrency.as` /
    // `structured_concurrency.as`, which need `import` — not compiled until V12)
    // by exercising a task op DIRECTLY over a VM-produced future, with no
    // `import`. They prove the bridge is sound today: `task.gather` over two VM
    // async-fn futures awaits both and preserves order.

    /// Spawn a VM async-fn call exactly the way the `Op::Call` async arm does:
    /// `spawn_local` a task that drives `Vm::call_value(closure, args)` and
    /// resolves a `SharedFuture` cell, returning the `Value::Future` handle
    /// immediately. This is the canonical "VM-produced future".
    fn spawn_vm_future(vm: &Rc<Vm>, closure: Value, args: Vec<Value>) -> Value {
        let vm2 = vm.rc();
        let fut = crate::task::SharedFuture::new();
        let cell = fut.cell();
        let handle = tokio::task::spawn_local(async move {
            let r = vm2.call_value(closure, args, s()).await;
            cell.resolve(r);
        });
        fut.set_abort(handle.abort_handle());
        Value::Future(fut)
    }

    /// Compile + run a whole `.as` program `src` on a fresh Vm (mirroring the
    /// `vm_run_source` entry point) and return the shared `Interp`'s in-flight
    /// high-water mark — used to prove un-awaited async tasks are reaped (bounded),
    /// not leaked (the M17 memory-leak guard, on the VM).
    fn run_program_max_inflight(src: &str) -> u64 {
        let chunk = crate::compile::compile_source(src).expect("compile ok");
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
        let mut fiber = Fiber::new(closure);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::new(interp.clone());
        local.block_on(&rt, async {
            local.run_until(vm.run(&mut fiber)).await.expect("run ok");
        });
        interp.max_inflight()
    }

    #[test]
    fn unawaited_async_loop_keeps_inflight_bounded_on_the_vm() {
        // M17 leak guard, on the VM: a tight loop spawning async calls WITHOUT
        // awaiting them must stay bounded. Each un-awaited future is dropped → its
        // task is cancelled; the cooperative yield above `INFLIGHT_YIELD_CAP`
        // (256) reaps finished/cancelled tasks so the in-flight high-water mark
        // stays well below the iteration count. Without reaping a 5000-iteration
        // loop would peak near 5000. Mirrors the interp's
        // `unawaited_async_loop_keeps_inflight_bounded`.
        let src = "async fn work(n) { return n }\n\
                   let i = 0\n\
                   while (i < 5000) {\n  work(i)\n  i = i + 1\n}\n\
                   print(\"done\")\n";
        let peak = run_program_max_inflight(src);
        assert!(
            peak < 1000,
            "in-flight high-water mark should stay bounded (got {peak})"
        );
    }

    #[test]
    fn task_gather_awaits_vm_produced_futures_in_order() {
        // `(n) => n + 1` invoked as two independent VM futures, gathered. The
        // native `task.gather` op awaits each `Value::Future` and returns the
        // values in input order — proving the VM's futures interoperate with the
        // structured-concurrency machinery (Part C de-risk; full e2e is V12).
        let f = compile_closure("(n) => n + 1");
        let out = with_vm(|vm| async move {
            let a = spawn_vm_future(&vm, f.clone(), vec![Value::Float(10.0)]);
            let b = spawn_vm_future(&vm, f, vec![Value::Float(20.0)]);
            let arr = Value::Array(crate::value::ArrayCell::new(vec![a, b]));
            vm.interp()
                .call_task("gather", &[arr], s())
                .await
                .expect("gather ok")
        });
        match out {
            Value::Array(a) => {
                let got: Vec<f64> = a
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Float(n) => *n,
                        other => panic!("non-number in gather result: {other:?}"),
                    })
                    .collect();
                assert_eq!(
                    got,
                    vec![11.0, 21.0],
                    "gather preserves order over VM futures"
                );
            }
            other => panic!("gather should return an array, got {other:?}"),
        }
    }

    #[test]
    fn task_race_resolves_a_vm_produced_future() {
        // A single VM-produced future raced resolves to its value — `task.race`
        // selects over `Value::Future`s and the VM's future drives to completion.
        let f = compile_closure("(n) => n * 2");
        let out = with_vm(|vm| async move {
            let a = spawn_vm_future(&vm, f, vec![Value::Float(21.0)]);
            let arr = Value::Array(crate::value::ArrayCell::new(vec![a]));
            vm.interp()
                .call_task("race", &[arr], s())
                .await
                .expect("race ok")
        });
        assert!(matches!(out, Value::Float(n) if n == 42.0), "got {out:?}");
    }

    #[test]
    fn call_value_propagates_a_closure_panic() {
        // A native HOF whose callback panics must see the SAME `Control::Panic`
        // surface out of `call_value` (so e.g. `array.map` aborts identically).
        // `(x) => x[9]` indexes a 1-element array out of bounds at runtime.
        let f = compile_closure("(x) => x[9]");
        let err = with_vm(|vm| async move {
            let arr = Value::Array(crate::value::ArrayCell::new(vec![Value::Float(0.0)]));
            vm.call_value(f, vec![arr], s())
                .await
                .expect_err("expected a panic")
        });
        match err {
            Control::Panic(e) => assert!(e.message.contains("out of bounds"), "msg: {}", e.message),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn call_value_arity_mismatch_panics_like_the_tree_walker() {
        // Calling a 1-param closure with 0 args from native code surfaces the
        // shared `check_call_args` arity panic (same wording as the tree-walker).
        let f = compile_closure("(x) => x");
        let err = with_vm(|vm| async move {
            vm.call_value(f, Vec::new(), s())
                .await
                .expect_err("expected an arity panic")
        });
        match err {
            Control::Panic(e) => assert!(
                e.message.contains("expected 1 argument(s), got 0"),
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn call_value_delegates_native_callees_to_the_interp() {
        // A non-closure callee (here the `print` builtin) routes to the shared
        // `Interp::call_value`, exactly like the `Op::Call` non-Closure arm.
        let out = with_vm(|vm| async move {
            let r = vm
                .call_value(
                    Value::Builtin(Rc::from("print")),
                    vec![Value::Float(7.0)],
                    s(),
                )
                .await
                .expect("call ok");
            // print returns nil and writes to the shared sink.
            assert!(matches!(r, Value::Nil), "print returns nil");
            vm.interp().output()
        });
        assert_eq!(out, "7\n", "print wrote through the delegated path");
    }

    #[test]
    fn jump_if_not_nil_falls_through_on_nil() {
        // NIL -> JUMP_IF_NOT_NIL pops, does NOT jump, falls through to CONST 9.
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let site = c.emit_jump(Op::JumpIfNotNil, s());
        let k9 = c.add_const(Value::Float(9.0));
        c.emit_u16(Op::Const, k9, s()); // executed (no jump)
        c.emit(Op::Return, s());
        c.patch_jump(site); // never reached
        assert_eq!(expect_number(c), 9.0);
    }

    // ---- PROPAGATE (? operator) at the bytecode level (V6-T1) -------------

    /// A success pair `[7, nil]` through PROPAGATE leaves `7` on the stack
    /// (the `?` expression's result), so the surrounding RETURN yields `7`.
    #[test]
    fn propagate_success_yields_value() {
        let mut c = Chunk::new();
        let pair = c.add_const(crate::interp::make_pair(Value::Float(7.0), Value::Nil));
        c.emit_u16(Op::Const, pair, s());
        c.emit(Op::Propagate, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 7.0);
    }

    /// A failure pair `[nil, "boom"]` through PROPAGATE early-returns the
    /// `[nil, err]` pair from the (root) frame — the trailing CONST 999 / RETURN
    /// never run, so the program result is the propagated pair.
    #[test]
    fn propagate_failure_early_returns_pair_from_frame() {
        let mut c = Chunk::new();
        let pair = c.add_const(crate::interp::make_pair(
            Value::Nil,
            Value::Str(Rc::from("boom")),
        ));
        c.emit_u16(Op::Const, pair, s());
        c.emit(Op::Propagate, s());
        // Never reached: PROPAGATE early-returned from the root frame.
        let k999 = c.add_const(Value::Float(999.0));
        c.emit_u16(Op::Const, k999, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Array(a)) => {
                let b = a.borrow();
                assert_eq!(b.len(), 2);
                assert_eq!(b[0], Value::Nil);
                assert_eq!(b[1], Value::Str(Rc::from("boom")));
            }
            other => panic!("expected Done([nil, \"boom\"]), got {other:?}"),
        }
    }

    /// Compile + run `src` on the VM and return the top-level program's value.
    /// (Mirrors the production `vm_eval_source` path; used by the shape tests to
    /// inspect the `shape_id` the VM assigned to the returned object/instance.)
    fn eval_src(src: &str) -> Value {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        rt.block_on(async { crate::vm_eval_source(src).await.expect("vm eval ok") })
    }

    fn obj_shape(v: &Value) -> u32 {
        match v {
            Value::Object(o) => o.shape.get(),
            other => panic!("expected an Object, got {other:?}"),
        }
    }

    // V11-T2: the VM assigns each object literal a hidden-class shape; two literals
    // with the SAME ordered keys converge on the SAME id, a different key set
    // differs, and key ORDER matters. We bundle them in one array so a single VM
    // (hence one ShapeRegistry) assigns all the ids.
    #[test]
    fn vm_object_literals_share_shape_by_layout() {
        // [{a,b}, {a,b}, {a,c}, {b,a}]
        let v = eval_src("[{a: 1, b: 2}, {a: 9, b: 8}, {a: 1, c: 2}, {b: 1, a: 2}]");
        let arr = match v {
            Value::Array(a) => a.borrow().clone(),
            other => panic!("expected array, got {other:?}"),
        };
        let s_ab1 = obj_shape(&arr[0]);
        let s_ab2 = obj_shape(&arr[1]);
        let s_ac = obj_shape(&arr[2]);
        let s_ba = obj_shape(&arr[3]);
        assert_eq!(s_ab1, s_ab2, "same ordered keys → same shape");
        assert_ne!(s_ab1, s_ac, "different key set → different shape");
        assert_ne!(s_ab1, s_ba, "different key ORDER → different shape");
        assert_ne!(s_ab1, 0, "a non-empty object is not the empty shape");
    }

    #[test]
    fn vm_empty_object_literal_is_shape_zero() {
        // `{}` at statement position parses as a block, so bind it first.
        let v = eval_src("let o = {}\no");
        assert_eq!(obj_shape(&v), 0);
    }

    // Adding a NEW key via `o.newkey = v` transitions the shape; REASSIGNING an
    // existing key keeps it (V11-T3's inline-cache validity relies on this). One
    // VM (one registry) builds all three objects so the ids are comparable.
    #[test]
    fn vm_adding_key_transitions_shape_reassign_keeps_it() {
        // Build {a}, then a mutated copy where `a` is reassigned, then one where a
        // NEW key `b` is added — return all three to compare their live shapes.
        let v = eval_src(
            "let base = {a: 1}\n\
             let reassigned = {a: 1}\n\
             reassigned.a = 5\n\
             let added = {a: 1}\n\
             added.b = 9;\n\
             [base, reassigned, added]",
        );
        let arr = match v {
            Value::Array(a) => a.borrow().clone(),
            other => panic!("expected array, got {other:?}"),
        };
        let s_base = obj_shape(&arr[0]);
        let s_reassigned = obj_shape(&arr[1]);
        let s_added = obj_shape(&arr[2]);
        assert_eq!(
            s_base, s_reassigned,
            "reassigning an existing key keeps the shape"
        );
        assert_ne!(s_base, s_added, "adding a new key transitions the shape");
        assert_ne!(s_added, 0);
    }

    // A class gives its instances a stable BASE shape (declared-field layout).
    #[test]
    fn vm_instance_has_class_base_shape() {
        let v = eval_src(
            "class P { x: number = 0\n y: number = 0\n }\n\
             [P(), P()]",
        );
        let arr = match v {
            Value::Array(a) => a.borrow().clone(),
            other => panic!("expected array, got {other:?}"),
        };
        let s0 = match &arr[0] {
            Value::Instance(i) => i.borrow().shape_id.get(),
            other => panic!("expected instance, got {other:?}"),
        };
        let s1 = match &arr[1] {
            Value::Instance(i) => i.borrow().shape_id.get(),
            other => panic!("expected instance, got {other:?}"),
        };
        assert_eq!(s0, s1, "two instances of one class share the base shape");
        assert_ne!(s0, 0, "a class with declared fields has a non-empty shape");
    }

    // ---- V11-T4 adaptive specialization -----------------------------------

    use crate::vm::adapt::{ArithCache, ArithKind, GlobalCache, WARMUP_THRESHOLD};
    use rust_decimal::Decimal;

    /// Build a `(Vm, Fiber)` over a single-op chunk whose op at offset 0 is `op`
    /// (with a real span), so a test can repeatedly call `eval_binop_adaptive` at
    /// `fault_ip = 0` and read back `chunk.arith_cache(0)` to watch specialization.
    fn adaptive_harness(op: Op) -> (Rc<Vm>, Fiber) {
        let mut c = Chunk::new();
        c.emit(op, Span::new(0, 3));
        c.emit(Op::Return, s());
        let proto = Rc::new(FnProto {
            chunk: c,
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
        let fiber = Fiber::new(closure);
        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::new(interp);
        (vm, fiber)
    }

    /// Like [`adaptive_harness`] but builds a NON-specializing VM (the
    /// `--no-specialize` kill switch). Used to prove the fast paths never run.
    fn generic_adaptive_harness(op: Op) -> (Rc<Vm>, Fiber) {
        let mut c = Chunk::new();
        c.emit(op, Span::new(0, 3));
        c.emit(Op::Return, s());
        let proto = Rc::new(FnProto {
            chunk: c,
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
        let fiber = Fiber::new(closure);
        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::new_generic(interp);
        (vm, fiber)
    }

    // ---- V11-T5 KILL SWITCH (--no-specialize) -----------------------------

    #[test]
    fn kill_switch_never_specializes_arithmetic_and_stays_correct() {
        // With specialization OFF, driving FAR past the warmup threshold must leave
        // the arith cache COLD (never warmed, never specialized) — yet every result
        // is byte-identical to the specializing path's result.
        let (vm, fiber) = generic_adaptive_harness(Op::Add);
        for i in 0..(WARMUP_THRESHOLD + 50) {
            let v = vm
                .eval_binop_adaptive(
                    &fiber,
                    0,
                    BinOp::Add,
                    Value::Float(i as f64),
                    Value::Float(1.0),
                )
                .expect("ok");
            assert_eq!(v, Value::Float(i as f64 + 1.0));
        }
        // The cache MUST still be at its default cold state — the generic path
        // never observes (no warmup candidate, count 0) and never specializes.
        assert_eq!(
            fiber.frame().closure.proto.chunk.arith_cache(0),
            ArithCache::default(),
            "kill switch must leave the arith cache cold (no warmup/specialize)"
        );
    }

    #[test]
    fn kill_switch_default_constructor_specializes() {
        // The DEFAULT `Vm::new` specializes; only `new_generic` disables it. This
        // pins the default so a future refactor cannot silently flip the switch.
        let (vm, fiber) = adaptive_harness(Op::Add);
        for _ in 0..WARMUP_THRESHOLD {
            vm.eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::Float(1.0),
                Value::Float(1.0),
            )
            .expect("ok");
        }
        assert!(
            fiber
                .frame()
                .closure
                .proto
                .chunk
                .arith_cache(0)
                .specialized()
                .is_some(),
            "default Vm::new must specialize a hot monomorphic site"
        );
    }

    #[test]
    fn add_warms_up_then_specializes_to_number() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        // Drive N number adds at offset 0; each returns the correct sum and the
        // last one flips the side-map cache to Specialized(Number).
        for i in 0..WARMUP_THRESHOLD {
            let v = vm
                .eval_binop_adaptive(
                    &fiber,
                    0,
                    BinOp::Add,
                    Value::Float(i as f64),
                    Value::Float(1.0),
                )
                .expect("ok");
            assert_eq!(v, Value::Float(i as f64 + 1.0));
        }
        let cache = fiber.frame().closure.proto.chunk.arith_cache(0);
        assert_eq!(
            cache,
            ArithCache::Specialized {
                kind: ArithKind::Number
            }
        );
        // A subsequent number add still takes the (now specialized) fast path with
        // the byte-identical result.
        let v = vm
            .eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::Float(40.0),
                Value::Float(2.0),
            )
            .expect("ok");
        assert_eq!(v, Value::Float(42.0));
    }

    #[test]
    fn specialized_number_add_deopts_on_string_operand_and_stays_correct() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        for _ in 0..WARMUP_THRESHOLD {
            vm.eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::Float(1.0),
                Value::Float(1.0),
            )
            .expect("ok");
        }
        assert!(fiber
            .frame()
            .closure
            .proto
            .chunk
            .arith_cache(0)
            .specialized()
            .is_some());
        // Now feed two strings: the Number guard misses → deopt → generic concat.
        let v = vm
            .eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::Str("a".into()),
                Value::Str("b".into()),
            )
            .expect("ok");
        assert_eq!(v, Value::Str("ab".into()), "generic path gave the concat");
        // The site deoptimized back to a fresh warmup (the deopt branch reverts and
        // runs generic without re-observing in the same step); a subsequent
        // execution starts observing anew.
        let cache = fiber.frame().closure.proto.chunk.arith_cache(0);
        assert_eq!(cache, ArithCache::default());
        assert!(cache.specialized().is_none());
    }

    #[test]
    fn add_specializes_to_concat_str() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        for _ in 0..WARMUP_THRESHOLD {
            let v = vm
                .eval_binop_adaptive(
                    &fiber,
                    0,
                    BinOp::Add,
                    Value::Str("x".into()),
                    Value::Str("y".into()),
                )
                .expect("ok");
            assert_eq!(v, Value::Str("xy".into()));
        }
        let cache = fiber.frame().closure.proto.chunk.arith_cache(0);
        assert_eq!(
            cache,
            ArithCache::Specialized {
                kind: ArithKind::ConcatStr
            }
        );
        // Specialized concat still byte-identical (incl. a key containing braces).
        let v = vm
            .eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::Str("1".into()),
                Value::Str("2".into()),
            )
            .expect("ok");
        assert_eq!(v, Value::Str("12".into()));
    }

    #[test]
    fn add_specializes_to_decimal() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        let a = Decimal::new(15, 1); // 1.5
        let b = Decimal::new(25, 1); // 2.5
        for _ in 0..WARMUP_THRESHOLD {
            let v = vm
                .eval_binop_adaptive(&fiber, 0, BinOp::Add, Value::Decimal(a), Value::Decimal(b))
                .expect("ok");
            assert_eq!(v, Value::Decimal(a + b));
        }
        let cache = fiber.frame().closure.proto.chunk.arith_cache(0);
        assert_eq!(
            cache,
            ArithCache::Specialized {
                kind: ArithKind::Decimal
            }
        );
        // Specialized decimal add equals the generic apply_binop result bit-exact.
        let v = vm
            .eval_binop_adaptive(&fiber, 0, BinOp::Add, Value::Decimal(a), Value::Decimal(b))
            .expect("ok");
        let generic =
            crate::interp::apply_binop(BinOp::Add, Value::Decimal(a), Value::Decimal(b), s())
                .expect("ok");
        assert_eq!(v, generic);
    }

    #[test]
    fn polymorphic_add_never_specializes_and_stays_correct() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        for i in 0..(WARMUP_THRESHOLD as usize * 4) {
            let (a, b, want) = if i % 2 == 0 {
                (Value::Float(2.0), Value::Float(3.0), Value::Float(5.0))
            } else {
                (
                    Value::Str("a".into()),
                    Value::Str("b".into()),
                    Value::Str("ab".into()),
                )
            };
            let v = vm
                .eval_binop_adaptive(&fiber, 0, BinOp::Add, a, b)
                .expect("ok");
            assert_eq!(v, want);
            // Alternating kinds reset the warmup, so the site never specializes.
            assert!(
                fiber
                    .frame()
                    .closure
                    .proto
                    .chunk
                    .arith_cache(0)
                    .specialized()
                    .is_none(),
                "polymorphic site stays generic at i={i}"
            );
        }
    }

    #[test]
    fn specialized_number_add_panics_identically_on_non_number_after_deopt() {
        // After specializing to Number, a number+nil add must produce the SAME
        // Tier-2 panic the generic apply_binop gives (it deopts, then runs generic).
        let (vm, fiber) = adaptive_harness(Op::Add);
        for _ in 0..WARMUP_THRESHOLD {
            vm.eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::Float(1.0),
                Value::Float(1.0),
            )
            .expect("ok");
        }
        let got = vm.eval_binop_adaptive(&fiber, 0, BinOp::Add, Value::Float(1.0), Value::Nil);
        let generic =
            crate::interp::apply_binop(BinOp::Add, Value::Float(1.0), Value::Nil, Span::new(0, 3));
        match (got, generic) {
            (Err(Control::Panic(a)), Err(Control::Panic(b))) => {
                assert_eq!(a.message, b.message);
                assert_eq!(a.span, b.span, "deopt path carries the op's span");
            }
            other => panic!("expected matching panics, got {other:?}"),
        }
    }

    #[test]
    fn get_global_cached_returns_same_builtin() {
        // Manually populate + read the global cache for a GET_GLOBAL site.
        let mut c = Chunk::new();
        let name = c.add_const(Value::Str(Rc::from("print")));
        c.emit_u16(Op::GetGlobal, name, s());
        c.emit(Op::Return, s());
        let version = 0u64;
        assert!(c.global_cache(0).get(version).is_none(), "cold initially");
        c.set_global_cache(0, GlobalCache::set(Value::Builtin("print".into()), version));
        match c.global_cache(0).get(version) {
            Some(Value::Builtin(n)) => assert_eq!(&*n, "print"),
            other => panic!("expected cached print builtin, got {other:?}"),
        }
        // A version bump invalidates it (defence-in-depth; never happens today).
        assert!(c.global_cache(0).get(version + 1).is_none());
    }

    #[test]
    fn hot_global_loop_resolves_print_consistently() {
        // End-to-end: a loop that references `print` many times prints each line —
        // the GET_GLOBAL_CACHED path must resolve the same builtin every iteration.
        let (out, _code) = {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let local = LocalSet::new();
            local.block_on(&rt, async {
                crate::vm_run_source("for (i in range(0, 5)) { print(i) }")
                    .await
                    .unwrap()
            })
        };
        assert_eq!(out, "0\n1\n2\n3\n4\n");
    }

    /// `expr?` where `expr` is not a 2-element array is a Tier-2 panic carrying
    /// the exact message and the PROPAGATE op's span (the `TryExpr`'s code span).
    #[test]
    fn propagate_non_pair_panics_with_span() {
        let mut c = Chunk::new();
        let k = c.add_const(Value::Float(5.0));
        c.emit_u16(Op::Const, k, s());
        let prop_span = Span::new(8, 10);
        c.emit(Op::Propagate, prop_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert_eq!(
                    e.message, "the ? operator requires a Result pair [value, err]",
                    "msg: {}",
                    e.message
                );
                assert_eq!(e.span, Some(prop_span), "panic carries the op's span");
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }
}
