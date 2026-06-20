//! `ascript::embed` — the stable host-embedding facade (EMBED, spec §3).
//!
//! # Stability
//!
//! Everything in this module is the semver-contracted Rust surface of the crate
//! (spec §9). The rest of the crate is `pub` for the bin target and the integration
//! tests and carries NO stability promise — host code embeds through `ascript::embed`.
//!
//! # The model
//!
//! AScript's runtime is `!Send` per isolate (`Rc`/`RefCell` on a current-thread
//! reactor); parallelism comes from *more isolates*, never from sharing. An
//! [`Isolate`] is therefore `!Send + !Sync` **by construction** — one isolate per host
//! thread, zero global VM lock, zero cross-isolate interference. A host that wants N
//! threads creates N isolates.
//!
//! ```no_run
//! use ascript::embed::{Isolate, OutputMode};
//!
//! let iso = Isolate::builder()
//!     .output(OutputMode::Capture)
//!     .build()
//!     .expect("build isolate");
//! drop(iso);
//! ```

mod error;
mod host;
mod value;

pub use error::{EmbedDiagnostic, EmbedError, EmbedPanic};
pub use host::{HostCtx, HostError, HostModuleBuilder};
pub use value::{AsKind, AsValue};

use std::cell::RefCell;
use std::rc::Rc;

use crate::interp::{ambient_root_scope, Control, Interp};
use crate::stdlib::caps::{Cap, CapSet};
use crate::vm::chunk::{Chunk, FnProto};
use crate::vm::fiber::Fiber;
use crate::vm::value_ext::{Closure, RunOutcome};
use crate::vm::Vm;

/// Where a script's `print` output goes (spec §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// `print` streams to the process stdout (the CLI's behavior). [`Isolate::take_output`]
    /// returns an empty string. This is the default.
    #[default]
    Inherit,
    /// `print` is buffered; [`Isolate::take_output`] drains the buffer.
    Capture,
}

/// Which compiled-in stdlib modules are *available* to scripts in this isolate
/// (spec §6.5). This is an **availability** knob, NOT a security boundary —
/// capabilities ([`Caps`]) are the security boundary. The filter is enforced at the
/// import chokepoint; an allowlisted module's transitively-reachable builtins are not
/// re-walked. **To sandbox untrusted scripts, set deny-all [`Caps`] (the embedded
/// default) — do NOT rely on `StdlibFilter::Core` alone**, which only hides modules,
/// not the authority they would grant.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum StdlibFilter {
    /// Every compiled-in module is importable (the default).
    #[default]
    Full,
    /// Only the no-OS subset (everything whose required capability is `None`, minus
    /// `std/ffi`).
    Core,
    /// Only the listed `std/*` modules are importable.
    Allow(Vec<String>),
}

impl StdlibFilter {
    /// Adapt to the CORE `StdlibFilterCore` the `Interp` import chokepoint checks.
    fn to_core(&self) -> crate::interp::StdlibFilterCore {
        match self {
            StdlibFilter::Full => crate::interp::StdlibFilterCore::Full,
            StdlibFilter::Core => crate::interp::StdlibFilterCore::Core,
            StdlibFilter::Allow(list) => crate::interp::StdlibFilterCore::Allow(list.clone()),
        }
    }
}

/// Host-decided capabilities for an embedded isolate (spec §7).
///
/// The embedded default is **deny-all** — the loud inversion of the CLI's
/// all-granted default. A CLI program is the artifact the *user* chose to run; an
/// embedded script is characteristically *someone else's plugin inside the host's
/// process*, so the host grants, explicitly, at construction time.
#[derive(Debug, Clone)]
pub struct Caps(CapSet);

impl Caps {
    /// The default: `fs`/`net`/`process`/`ffi`/`env` all denied.
    pub fn deny_all() -> Self {
        let mut set = CapSet::all_granted();
        set.deny_all_dangerous();
        Caps(set)
    }

    /// CLI-equivalent: every capability granted (for *trusted* scripts only).
    pub fn all_granted() -> Self {
        Caps(CapSet::all_granted())
    }

    /// Grant exactly the listed capabilities, deny the rest — decided AT construction
    /// (which precedes all in-script `caps.drop`s, so it does not violate cap
    /// monotonicity: construction grants, drops are irreversible thereafter).
    ///
    /// `CapSet` is deny-only (monotone, no `grant` inverse), so this is expressed as
    /// "start all-granted, deny every capability NOT in `caps`".
    pub fn granting(caps: &[Cap]) -> Self {
        let mut set = CapSet::all_granted();
        for cap in Cap::ALL {
            if !caps.contains(&cap) {
                set.deny(cap);
            }
        }
        Caps(set)
    }

    /// The underlying `CapSet` (crate-internal — installed on the `Interp` at build).
    pub(crate) fn into_capset(self) -> CapSet {
        self.0
    }
}

impl Default for Caps {
    fn default() -> Self {
        Caps::deny_all()
    }
}

/// A builder for an [`Isolate`]. All methods are additive; [`build`](Self::build)
/// validates and constructs (spec §3.2).
pub struct IsolateBuilder {
    caps: Caps,
    stdlib: StdlibFilter,
    output: OutputMode,
    args: Vec<String>,
    /// Validated host modules to register on the isolate at `build()` (spec §6.2).
    /// Each is `(full "host:<name>", HostModuleDef)`; a validation/duplicate error is
    /// deferred to `build()` (the builder methods are infallible-chaining and return
    /// `Result` per the spec signature, surfacing the error at registration).
    host_modules: Vec<(String, crate::interp::HostModuleDef)>,
    /// Per-isolate host-module FACTORIES (spec §6.4): `Arc<dyn Fn(&mut Builder) + Send +
    /// Sync>` that also install the module into every worker isolate this Isolate spawns.
    /// Carried as a side-channel beside `caps` on the worker spawn paths (§6.4).
    host_factories: Vec<host::HostModuleFactory>,
}

impl std::fmt::Debug for IsolateBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Closures (host modules/factories) are not `Debug`; show only the shape.
        f.debug_struct("IsolateBuilder")
            .field("caps", &self.caps)
            .field("stdlib", &self.stdlib)
            .field("output", &self.output)
            .field("args", &self.args)
            .field("host_modules", &self.host_modules.len())
            .field("host_factories", &self.host_factories.len())
            .finish()
    }
}

impl Default for IsolateBuilder {
    fn default() -> Self {
        IsolateBuilder {
            caps: Caps::deny_all(),
            stdlib: StdlibFilter::Full,
            output: OutputMode::Inherit,
            args: Vec::new(),
            host_modules: Vec::new(),
            host_factories: Vec::new(),
        }
    }
}

impl IsolateBuilder {
    /// Set the isolate's capabilities (default: [`Caps::deny_all`], spec §7).
    pub fn caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }

    /// Set the stdlib availability filter (default: [`StdlibFilter::Full`], spec §6.5).
    pub fn stdlib(mut self, filter: StdlibFilter) -> Self {
        self.stdlib = filter;
        self
    }

    /// Set where script `print` output goes (default: [`OutputMode::Inherit`]).
    pub fn output(mut self, mode: OutputMode) -> Self {
        self.output = mode;
        self
    }

    /// Set the script's `cli.args` (default: empty).
    pub fn args(mut self, args: &[&str]) -> Self {
        self.args = args.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Register a host module under the `host:` namespace (spec §6.2). The script
    /// imports it as `import * as app from "host:app"`. The `name` is validated (§6.1:
    /// `host:` + a `/`-segmented lowercase identifier path, no dots) and a duplicate is
    /// rejected — both as [`EmbedError::Config`].
    ///
    /// # Security (§6.3)
    ///
    /// Host functions registered here are **native Rust** and **bypass the
    /// [`Caps`](Caps) gate** — see [`HostModuleBuilder`] for the full note.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Config`] on an invalid name (§6.1) or a duplicate registration.
    pub fn host_module(
        mut self,
        name: &str,
        f: impl FnOnce(&mut HostModuleBuilder),
    ) -> Result<Self, EmbedError> {
        host::validate_module_name(name).map_err(EmbedError::Config)?;
        if self.host_modules.iter().any(|(n, _)| n == name)
            || self.host_factories.iter().any(|(n, _)| &**n == name)
        {
            return Err(EmbedError::Config(format!(
                "host module '{name}' is already registered in this isolate"
            )));
        }
        let mut builder = HostModuleBuilder::new();
        f(&mut builder);
        self.host_modules.push((name.to_string(), builder.finish()));
        Ok(self)
    }

    /// Register a per-isolate host-module FACTORY (spec §6.4): like [`host_module`], but
    /// the module is ALSO installed into every worker isolate this Isolate spawns
    /// (pooled `worker fn` AND dedicated `run_in_worker`). The closure runs INSIDE the
    /// freshly-spawned isolate thread, so it is `Send + Sync` and the host fns it builds
    /// may close over `Send + Sync` host state only.
    ///
    /// # Security (§6.3)
    ///
    /// Factory-built host functions are **native Rust** and **bypass the [`Caps`](Caps)
    /// gate** — see [`HostModuleBuilder`].
    ///
    /// # Errors
    ///
    /// [`EmbedError::Config`] on an invalid name (§6.1) or a duplicate registration.
    pub fn host_module_factory(
        mut self,
        name: &str,
        f: std::sync::Arc<dyn Fn(&mut HostModuleBuilder) + Send + Sync>,
    ) -> Result<Self, EmbedError> {
        host::validate_module_name(name).map_err(EmbedError::Config)?;
        if self.host_modules.iter().any(|(n, _)| n == name)
            || self.host_factories.iter().any(|(n, _)| &**n == name)
        {
            return Err(EmbedError::Config(format!(
                "host module '{name}' is already registered in this isolate"
            )));
        }
        // A factory ALSO registers the module on the MAIN isolate (so the host can call
        // into it directly), by running it once at build time.
        let def = host::build_factory(&f);
        self.host_modules.push((name.to_string(), def));
        self.host_factories.push((std::rc::Rc::from(name), f));
        Ok(self)
    }

    /// Validate the configuration and construct the isolate.
    ///
    /// Does what the worker isolate bootstrap does, on the **calling** thread:
    /// construct `Interp` per the output mode → `set_caps` → `set_cli_args` →
    /// `install_self` → `Vm::new`, plus an **owned** current-thread tokio runtime the
    /// blocking entry points drive (spec §4.1). No thread is spawned — the isolate
    /// *is* the calling thread's.
    pub fn build(self) -> Result<Isolate, EmbedError> {
        // Output mode → the matching Interp sink (Capture buffers; Inherit streams to
        // stdout, the CLI behavior).
        let interp = match self.output {
            OutputMode::Capture => Interp::new(),
            OutputMode::Inherit => Interp::new_live(),
        };
        interp.set_caps(self.caps.into_capset());
        interp.set_cli_args(&self.args);
        // EMBED §6.5: install the stdlib AVAILABILITY filter, checked at the
        // `load_std_module` import chokepoint. An availability knob, NOT a security
        // boundary — capabilities (above) are the security boundary.
        interp.set_stdlib_filter(self.stdlib.to_core());
        // EMBED §6.3: register the validated host modules on the isolate BEFORE any code
        // runs. Names were validated + de-duplicated at builder time, so a registration
        // error here is unreachable; surface it as Config defensively rather than panic.
        for (name, def) in self.host_modules {
            interp
                .register_host_module(&name, def)
                .map_err(EmbedError::Config)?;
        }
        let interp = Rc::new(interp);
        interp.install_self();
        let vm = Vm::new(interp);

        // The owned current-thread runtime blocking eval/call drives (spec §4.1). A
        // construction failure (extremely unlikely on a current-thread builder) is a
        // typed Config error, never a panic.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| EmbedError::Config(format!("could not build isolate runtime: {e}")))?;

        // EMBED §6.4: stash the host-module factories on the isolate (adapted to the
        // CORE `Fn() -> HostModuleDef` form) so the worker spawn paths can ship them
        // per-isolate. Each adapter runs the `Send + Sync` builder closure INSIDE the
        // worker isolate to produce a fresh `HostModuleDef`.
        let core_factories: Vec<crate::interp::HostFactoryEntry> = self
            .host_factories
            .into_iter()
            .map(|(name, f)| {
                let adapter: std::sync::Arc<crate::interp::HostModuleFactoryCore> =
                    std::sync::Arc::new(move || host::build_factory(&f));
                (name, adapter)
            })
            .collect();
        vm.interp().set_host_factories(core_factories);

        Ok(Isolate {
            vm,
            rt: Some(rt),
            session_src: RefCell::new(String::new()),
            output: self.output,
        })
    }
}

/// A live, `!Send` AScript engine instance — one per host thread (spec §3.2).
///
/// Holds a persistent `Vm` (its `user_globals` table IS the session scope, persisting
/// across [`eval`](Self::eval) calls) and an owned current-thread tokio runtime the
/// blocking entry points drive. `!Send + !Sync` by construction (it holds `Rc<Vm>`).
pub struct Isolate {
    vm: Rc<Vm>,
    // `Option` so `Drop` can `take()` the runtime and `shutdown_background()` it:
    // dropping a `tokio::runtime::Runtime` directly from inside an async context
    // panics ("cannot drop a runtime in a context where blocking is not allowed"),
    // which would happen if a host drops an `Isolate` inside its own `#[tokio::test]`
    // / async fn. `shutdown_background` never blocks, so it is async-context-safe.
    // Always `Some` between `build()` and `drop`.
    rt: Option<tokio::runtime::Runtime>,
    session_src: RefCell<String>,
    output: OutputMode,
}

impl Isolate {
    /// Start building a new isolate.
    pub fn builder() -> IsolateBuilder {
        IsolateBuilder::default()
    }

    /// Compile + run `src` on this isolate's persistent `Vm`, **blocking** the calling
    /// thread until the program (and everything it spawned) is quiescent (spec §3.3).
    /// Returns the trailing-expression value (`nil` for a statement-terminated input).
    ///
    /// Lifts the REPL's `eval_line_vm` substrate. The session persists across calls:
    /// a binding defined in an earlier `eval` is visible in a later one (the
    /// `user_globals` table IS the session scope). A compile error returns
    /// [`EmbedError::Compile`] with NO session mutation; a Tier-2 panic returns
    /// [`EmbedError::Panic`] and the session survives.
    ///
    /// # Errors
    ///
    /// Returns [`EmbedError::NestedRuntime`] if called from inside an ambient tokio
    /// runtime (where `block_on` would panic) — use [`eval_async`](Self::eval_async).
    pub fn eval(&self, src: &str) -> Result<AsValue, EmbedError> {
        self.guard_no_ambient_runtime()?;
        // Compile + accumulate session source BEFORE entering the async block: a
        // compile error must short-circuit with no session mutation (the REPL rule).
        let fiber = self.prepare_fiber(src)?;
        // Drive the !Send eval future on the owned current-thread runtime under a fresh
        // LocalSet (so spawned tasks join — structured concurrency), then DRAIN the
        // LocalSet (the REPL's two-step `run_until` + `local.await`, here via the owned
        // runtime). `block_on` on the owned reactor lets timers/IO inside the script
        // work. `run_until` borrows `&local`, so `local` is still owned afterward and
        // the second `block_on(local)` joins the spawned tasks.
        let local = tokio::task::LocalSet::new();
        let vm = Rc::clone(&self.vm);
        let rt = self.rt();
        let result = rt.block_on(local.run_until(async move { local_run(&vm, fiber).await }));
        rt.block_on(local); // drain spawned tasks (structured join)
        map_outcome(result)
    }

    /// The async variant of [`eval`](Self::eval): a `!Send` future the **host** drives
    /// (spec §4.2). It never touches the owned runtime — the host's ambient reactor +
    /// `LocalSet` serve I/O and `spawn_local`. Supported host configurations: a
    /// current-thread runtime (await under `LocalSet::run_until`), or a multi-thread
    /// runtime driven from a non-worker thread (`LocalSet::block_on`). Awaiting from a
    /// `tokio::spawn`ed task is a compile error by construction (the future is `!Send`).
    pub async fn eval_async(&self, src: &str) -> Result<AsValue, EmbedError> {
        // No ambient-runtime guard: the host IS providing the runtime here.
        let fiber = self.prepare_fiber(src)?;
        let outcome = local_run(&self.vm, fiber).await;
        map_outcome(outcome)
    }

    /// Call a module-scope global function by name (spec §3.3). If the callee is an
    /// `async fn` (its call returns a `future<T>`, eager-scheduled per M17), the future
    /// is driven to completion and its resolved value returned (**auto-await**).
    ///
    /// # Errors
    ///
    /// [`EmbedError::Undefined`] if no global named `name` exists;
    /// [`EmbedError::Panic`] if the callee is not callable or the call panics;
    /// [`EmbedError::NestedRuntime`] from inside an ambient runtime (use
    /// [`call_async`](Self::call_async)).
    pub fn call(&self, name: &str, args: &[AsValue]) -> Result<AsValue, EmbedError> {
        self.guard_no_ambient_runtime()?;
        let callee = self.lookup_global(name)?;
        self.call_blocking(callee, args)
    }

    /// The async variant of [`call`](Self::call): a `!Send` future the host drives.
    pub async fn call_async(&self, name: &str, args: &[AsValue]) -> Result<AsValue, EmbedError> {
        let callee = self.lookup_global(name)?;
        call_inner(&self.vm, callee, marshal_args(args)).await
    }

    /// Call a callable [`AsValue`] (a function handle previously read out via
    /// [`global`](Self::global)). Auto-awaits a returned future, like [`call`](Self::call).
    pub fn call_value(&self, callee: &AsValue, args: &[AsValue]) -> Result<AsValue, EmbedError> {
        self.guard_no_ambient_runtime()?;
        self.call_blocking(callee.value().clone(), args)
    }

    /// The async variant of [`call_value`](Self::call_value).
    pub async fn call_value_async(
        &self,
        callee: &AsValue,
        args: &[AsValue],
    ) -> Result<AsValue, EmbedError> {
        call_inner(&self.vm, callee.value().clone(), marshal_args(args)).await
    }

    /// Read a module-scope global by name (`None` if undefined).
    pub fn global(&self, name: &str) -> Option<AsValue> {
        self.vm.user_global(name).map(AsValue::from_value)
    }

    /// Define or overwrite a module-scope global, defined MUTABLE (like a top-level
    /// `let`) so it can be set repeatedly and read by later evals.
    pub fn set_global(&self, name: &str, value: AsValue) -> Result<(), EmbedError> {
        self.vm
            .define_user_global_mutable(name, value.into_value());
        Ok(())
    }

    /// Register (or REPLACE) a host module on this **already-built** isolate (spec §8.2,
    /// the capi late-registration hook — the C API's `as_register_host_fn` accumulates a
    /// module's functions one call at a time and re-installs the whole module each call).
    ///
    /// Most hosts register host modules on the [`IsolateBuilder`] before [`build`]; this
    /// is the post-construction path the C API needs (where there is no builder to thread
    /// closures through).
    ///
    /// # The memoization rule (the contract)
    ///
    /// A host module is memoized the FIRST time a script `import`s it; a late registration
    /// of an ALREADY-IMPORTED module is a hard [`EmbedError::Config`] (the cached module
    /// is not retro-patched, so a silently-invisible function would be a footgun). Register
    /// a module's functions **before the first `import "host:<name>"`**. A never-imported
    /// module re-resolves against the fresh registry, so replacing it is sound.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Config`] on an invalid name (§6.1) or if the module was already
    /// imported by a script.
    ///
    /// [`build`]: IsolateBuilder::build
    pub fn register_host_module_late(
        &self,
        name: &str,
        f: impl FnOnce(&mut HostModuleBuilder),
    ) -> Result<(), EmbedError> {
        host::validate_module_name(name).map_err(EmbedError::Config)?;
        let mut builder = HostModuleBuilder::new();
        f(&mut builder);
        self.vm
            .interp()
            .register_host_module_late(name, builder.finish())
            .map_err(EmbedError::Config)
    }

    /// Parse a JSON string into a fresh [`AsValue`] (a DEEP COPY — explicitly distinct
    /// from the live aliasing handles, spec §5.3). Routes through `std/json`'s total
    /// parser. The inverse of [`AsValue::to_json`](AsValue::to_json).
    ///
    /// # Errors
    ///
    /// [`EmbedError::Config`] when the crate is built without the `data` feature (the
    /// `std/json` parser is absent — documented, not silently compiled away);
    /// otherwise [`EmbedError::Config`] on invalid JSON (carrying the parser's message).
    #[cfg(feature = "data")]
    pub fn json_parse(&self, text: &str) -> Result<AsValue, EmbedError> {
        let jv: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| EmbedError::Config(format!("invalid JSON: {e}")))?;
        Ok(AsValue::from_value(crate::stdlib::json::to_ascript(&jv)))
    }

    /// Parse a JSON string into a fresh [`AsValue`].
    ///
    /// # Errors
    ///
    /// Always [`EmbedError::Config`] in this build: the crate was compiled without the
    /// `data` feature, so the `std/json` parser is absent (spec §5.3).
    #[cfg(not(feature = "data"))]
    pub fn json_parse(&self, _text: &str) -> Result<AsValue, EmbedError> {
        Err(EmbedError::Config(
            "json_parse requires the 'data' Cargo feature (the std/json parser)".to_string(),
        ))
    }

    /// Load + run a compiled module archive (a single `.aso` `ascript build` output) as
    /// the entry program on this isolate, verified through the same `.aso` trust
    /// boundary the CLI uses ([`Chunk::from_bytes_verified`](crate::vm::chunk::Chunk)).
    /// Returns the program's trailing value.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Archive`] on a decode/verify failure (the verifier's message
    /// verbatim); [`EmbedError::Panic`]/[`EmbedError::Exit`] for a run outcome;
    /// [`EmbedError::NestedRuntime`] from inside an ambient runtime.
    pub fn load_archive(&self, bytes: &[u8]) -> Result<AsValue, EmbedError> {
        self.guard_no_ambient_runtime()?;
        let fiber = prepare_archive_fiber(bytes)?;
        let local = tokio::task::LocalSet::new();
        let vm = Rc::clone(&self.vm);
        let rt = self.rt();
        let result = rt.block_on(local.run_until(async move { local_run(&vm, fiber).await }));
        rt.block_on(local);
        map_outcome(result)
    }

    /// The async variant of [`load_archive`](Self::load_archive).
    pub async fn load_archive_async(&self, bytes: &[u8]) -> Result<AsValue, EmbedError> {
        let fiber = prepare_archive_fiber(bytes)?;
        let outcome = local_run(&self.vm, fiber).await;
        map_outcome(outcome)
    }

    /// Resolve a module-scope global by name → its value, or `Undefined`.
    fn lookup_global(&self, name: &str) -> Result<crate::value::Value, EmbedError> {
        self.vm
            .user_global(name)
            .ok_or_else(|| EmbedError::Undefined(format!("'{name}' is not defined")))
    }

    /// Drive a `call` to completion on the owned runtime (blocking), with the per-call
    /// LocalSet + drain (mirrors `eval`).
    fn call_blocking(
        &self,
        callee: crate::value::Value,
        args: &[AsValue],
    ) -> Result<AsValue, EmbedError> {
        let argv = marshal_args(args);
        let local = tokio::task::LocalSet::new();
        let vm = Rc::clone(&self.vm);
        let rt = self.rt();
        let result =
            rt.block_on(local.run_until(async move { call_inner(&vm, callee, argv).await }));
        rt.block_on(local); // drain spawned tasks
        result
    }

    /// Compile `src`, accumulate it onto the session source (so a `worker fn` defined
    /// in an earlier eval stays sliceable), and build the not-started top-level fiber.
    /// A compile error short-circuits with NO session mutation (the REPL rule).
    fn prepare_fiber(&self, src: &str) -> Result<Fiber, EmbedError> {
        let chunk = crate::compile::compile_source(src).map_err(|e| {
            // A compile error carries a message + span; render it against the input.
            let src_info = Rc::new(crate::error::SourceInfo {
                path: "<embed>".to_string(),
                text: src.to_string(),
            });
            let as_err = crate::error::AsError::at(e.message, e.span).with_source(src_info);
            EmbedError::from_compile(&as_err)
        })?;
        // Accumulate session source AFTER compilation succeeds (don't accumulate
        // syntax-invalid input) but BEFORE running (so a define-then-call-worker-fn in
        // one eval has its source available) — the REPL `session_src` discipline.
        {
            let mut session = self.session_src.borrow_mut();
            if !session.is_empty() {
                session.push('\n');
            }
            session.push_str(src);
            self.vm.interp().set_worker_source(&session);
        }
        Ok(make_top_fiber(chunk))
    }

    /// `EmbedError::NestedRuntime` if a tokio runtime is ambient (blocking would
    /// panic). Cheap: a TLS read.
    fn guard_no_ambient_runtime(&self) -> Result<(), EmbedError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            Err(EmbedError::NestedRuntime)
        } else {
            Ok(())
        }
    }

    /// Drain the capture buffer (under [`OutputMode::Capture`]); an empty string under
    /// [`OutputMode::Inherit`] (where `print` already streamed to stdout).
    pub fn take_output(&self) -> String {
        match self.output {
            // DRAIN (take + clear) so repeated calls return only NEW output.
            OutputMode::Capture => self.vm.interp().take_output(),
            OutputMode::Inherit => String::new(),
        }
    }

    /// The owned runtime (always `Some` for a live isolate; `None` only mid-`drop`).
    fn rt(&self) -> &tokio::runtime::Runtime {
        self.rt
            .as_ref()
            .expect("isolate runtime is present for a live isolate")
    }

    /// Crate-internal accessor for the persistent `Vm`.
    #[allow(dead_code)]
    pub(crate) fn vm(&self) -> &Rc<Vm> {
        &self.vm
    }

    /// Crate-internal accessor for the accumulated session source.
    #[allow(dead_code)]
    pub(crate) fn session_src(&self) -> &RefCell<String> {
        &self.session_src
    }
}

impl Drop for Isolate {
    fn drop(&mut self) {
        // End-of-session cycle collection (the sweep every VM entry point performs).
        crate::gc::collect();
        // Shut the owned runtime down WITHOUT blocking: dropping a `Runtime` directly
        // from inside an async context (a host dropping the `Isolate` in its own
        // `#[tokio::test]` / async fn) panics. `shutdown_background` is async-safe.
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// Build the top-level `FnProto` for a compiled chunk (the exact shape
/// `eval_line_vm` uses, `src/repl.rs:229`).
fn make_top_proto(chunk: Chunk) -> Rc<FnProto> {
    Rc::new(FnProto {
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
    })
}

/// Build a not-started top-level fiber for a compiled chunk.
fn make_top_fiber(chunk: Chunk) -> Fiber {
    Fiber::new(Closure::new(make_top_proto(chunk)))
}

/// Run `fiber` on `vm` to quiescence under the CURRENT `LocalSet` (the caller has
/// already entered one via `run_until`/`block_on`), draining spawned tasks. The
/// `ambient_root_scope` wrap matches every shipped entry point (it carries the
/// telemetry/deadline task-locals seam — `None`/inert in the embed default).
async fn local_run(vm: &Rc<Vm>, mut fiber: Fiber) -> Result<RunOutcome, Control> {
    ambient_root_scope(vm.run(&mut fiber)).await
}

/// Marshal host `AsValue` args into engine `Value`s (a refcount bump per container —
/// the §5 "containers cross by handle" model; scalars copy).
fn marshal_args(args: &[AsValue]) -> Vec<crate::value::Value> {
    args.iter().map(|a| a.value().clone()).collect()
}

/// Invoke `callee` with `argv` on `vm`, auto-awaiting a returned future (spec §3.3 —
/// an `async fn` callee returns an eager-scheduled `future<T>` which is driven to
/// completion and its resolved value returned). The caller has already entered a
/// `LocalSet`. A non-callable callee surfaces the engine's own "value is not callable"
/// Tier-2 panic as `EmbedError::Panic`.
async fn call_inner(
    vm: &Rc<Vm>,
    callee: crate::value::Value,
    argv: Vec<crate::value::Value>,
) -> Result<AsValue, EmbedError> {
    let result = ambient_root_scope(async {
        let r = vm
            .call_value(callee, argv, crate::span::Span::new(0, 0))
            .await?;
        // Auto-await an `async fn` callee's returned future (the §3.3 rule, mirroring
        // the VM's own native re-entry pattern `Future(f) => f.get().await`). Use the
        // BORROWING `kind()` view + clone the `SharedFuture` (identity-equal, an `Rc`
        // bump) so the non-future case returns `r` untouched without a rebuild.
        let fut = match r.kind() {
            crate::value::ValueKind::Future(f) => Some(f.clone()),
            _ => None,
        };
        match fut {
            Some(f) => f.get().await,
            None => Ok(r),
        }
    })
    .await;
    match result {
        Ok(v) => Ok(AsValue::from_value(v)),
        Err(Control::Panic(e)) => Err(EmbedError::from_panic(&e)),
        // A `?`-propagation out of the called fn → the `[nil, err]` pair is the result.
        Err(Control::Propagate(pair)) => Ok(AsValue::from_value(pair)),
        Err(Control::Exit(code)) => Err(EmbedError::Exit(code)),
    }
}

/// Decode + verify a single `.aso` archive's bytes through the same trust boundary the
/// CLI uses, building a not-started top-level fiber. A decode/verify failure is an
/// `EmbedError::Archive` carrying the verifier's message.
fn prepare_archive_fiber(bytes: &[u8]) -> Result<Fiber, EmbedError> {
    let chunk = Chunk::from_bytes_verified(bytes)
        .map_err(|e| EmbedError::Archive(format!("{e:?}")))?;
    Ok(make_top_fiber(chunk))
}

/// Map a VM run outcome to the embed result (spec §3.3.4): the trailing-expression
/// value on `Done`; a typed error for panic/exit; `nil` for a top-level `?`-propagate
/// (CLI parity). The per-eval fiber is discarded either way — the session survives.
fn map_outcome(outcome: Result<RunOutcome, Control>) -> Result<AsValue, EmbedError> {
    match outcome {
        Ok(RunOutcome::Done(v)) => Ok(AsValue::from_value(v)),
        // A top-level program cannot yield (no enclosing generator). Defensive: nil.
        Ok(RunOutcome::Yielded(_)) => Ok(AsValue::nil()),
        Err(Control::Panic(e)) => Err(EmbedError::from_panic(&e)),
        // A top-level `?` ends the program with no value (CLI parity, `lib.rs`).
        Err(Control::Propagate(_)) => Ok(AsValue::nil()),
        Err(Control::Exit(code)) => Err(EmbedError::Exit(code)),
    }
}

#[cfg(test)]
mod engine_parity {
    //! EMBED §6.3 / §11 — the LOAD-BEARING engine-parity test: the SAME host-module
    //! program runs on the tree-walker (a raw `Interp` + the crate-internal
    //! `register_host_module` + `exec_program`) AND via the VM (an `Isolate`), asserting
    //! BYTE-IDENTICAL captured output — including the registry-miss panic message. This
    //! proves four-mode byte-identity for the `host:` surface (the import + dispatch arms
    //! live on the shared `Interp`, so both engines reach the same loader + dispatch).
    use super::*;
    use crate::embed::host::HostModuleBuilder;

    /// Build the shared `host:app` `HostModuleDef` both engines register.
    fn app_def() -> crate::interp::HostModuleDef {
        let mut b = HostModuleBuilder::new();
        b.value("version", AsValue::from("1.0"));
        b.func("double", |_c, a| {
            Ok(AsValue::from(a[0].as_int().unwrap_or(0) * 2))
        });
        b.fallible_func("lookup", |_c, a| match a[0].as_str() {
            Some("k") => Ok(AsValue::from(42i64)),
            _ => Err(HostError::Recoverable("no such key".into())),
        });
        b.finish()
    }

    const PROG: &str = r#"
import * as app from "host:app"
print(app.version, app.double(21))
let [v, e1] = app.lookup("k")
let [n, e2] = app.lookup("x")
print(v, e1 == nil, n, e2.message)
"#;

    /// Run `src` on the TREE-WALKER against a raw capture-mode `Interp` with `host:app`
    /// registered. Returns the captured output (or the error message on a panic).
    async fn tree_walker_run(src: &str) -> String {
        let interp = Rc::new(Interp::new());
        interp.install_self();
        interp.register_host_module("host:app", app_def()).unwrap();
        interp.set_worker_source(src);
        let tokens = crate::lexer::lex(src).expect("lex");
        let program = crate::parser::parse(&tokens).expect("parse");
        let env = crate::interp::global_env().child();
        let local = tokio::task::LocalSet::new();
        let result = local
            .run_until(ambient_root_scope(interp.exec_program(&program, &env)))
            .await;
        match result {
            Ok(_) | Err(Control::Propagate(_)) => interp.output(),
            Err(Control::Panic(e)) => e.message,
            Err(Control::Exit(_)) => interp.output(),
        }
    }

    /// Run `src` via the VM through an `Isolate` (capture). Returns captured output,
    /// or the panic message — mirroring `tree_walker_run`'s shaping.
    fn vm_run(src: &str) -> String {
        let iso = Isolate::builder()
            .output(OutputMode::Capture)
            .host_module("host:app", |m: &mut HostModuleBuilder| {
                m.value("version", AsValue::from("1.0"));
                m.func("double", |_c, a| {
                    Ok(AsValue::from(a[0].as_int().unwrap_or(0) * 2))
                });
                m.fallible_func("lookup", |_c, a| match a[0].as_str() {
                    Some("k") => Ok(AsValue::from(42i64)),
                    _ => Err(HostError::Recoverable("no such key".into())),
                });
            })
            .unwrap()
            .build()
            .unwrap();
        match iso.eval(src) {
            Ok(_) => iso.take_output(),
            Err(EmbedError::Panic(p)) => p.message,
            Err(other) => format!("{other:?}"),
        }
    }

    #[test]
    fn host_module_program_is_byte_identical_across_engines() {
        // The VM side blocks on its own owned runtime; the tree-walker side needs a
        // current-thread runtime to drive its LocalSet. Run the VM side first (it owns
        // its runtime), then the tree-walker side under a fresh runtime.
        let vm_out = vm_run(PROG);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tw_out = rt.block_on(tree_walker_run(PROG));
        assert_eq!(vm_out, tw_out, "tree-walker == VM for the host program");
        assert_eq!(vm_out, "1.0 42\n42 true nil no such key\n");
    }

    #[test]
    fn host_miss_panic_message_is_byte_identical_across_engines() {
        // A NON-registered module on each engine raises the SAME miss message. Use a raw
        // tree-walker interp with NO registration vs an Isolate with NO host module.
        const MISS: &str = "import * as a from \"host:nope\"\n";
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Tree-walker: no registration → the miss message.
        let tw = rt.block_on(async {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            interp.set_worker_source(MISS);
            let tokens = crate::lexer::lex(MISS).unwrap();
            let program = crate::parser::parse(&tokens).unwrap();
            let env = crate::interp::global_env().child();
            let local = tokio::task::LocalSet::new();
            match local
                .run_until(ambient_root_scope(interp.exec_program(&program, &env)))
                .await
            {
                Err(Control::Panic(e)) => e.message,
                other => format!("expected panic, got {other:?}"),
            }
        });
        let iso = Isolate::builder().output(OutputMode::Capture).build().unwrap();
        let vm = match iso.eval(MISS) {
            Err(EmbedError::Panic(p)) => p.message,
            other => format!("expected panic, got {other:?}"),
        };
        assert_eq!(tw, vm, "miss panic message identical across engines");
        assert_eq!(
            vm,
            "host module 'host:nope' is not registered in this isolate"
        );
    }
}
