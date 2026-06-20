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

pub use error::{EmbedDiagnostic, EmbedError, EmbedPanic};

use std::cell::RefCell;
use std::rc::Rc;

use crate::interp::Interp;
use crate::stdlib::caps::{Cap, CapSet};
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
/// re-walked.
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
}

impl Default for IsolateBuilder {
    fn default() -> Self {
        IsolateBuilder {
            caps: Caps::deny_all(),
            stdlib: StdlibFilter::Full,
            output: OutputMode::Inherit,
            args: Vec::new(),
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
        // The `StdlibFilter` field is carried for a later unit (host-module phase wires
        // it into the import chokepoint); the facade core stores the default unchanged.
        let _ = &self.stdlib;
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

        Ok(Isolate {
            vm,
            rt,
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
    rt: tokio::runtime::Runtime,
    session_src: RefCell<String>,
    output: OutputMode,
}

impl Isolate {
    /// Start building a new isolate.
    pub fn builder() -> IsolateBuilder {
        IsolateBuilder::default()
    }

    /// Drain the capture buffer (under [`OutputMode::Capture`]); an empty string under
    /// [`OutputMode::Inherit`] (where `print` already streamed to stdout).
    pub fn take_output(&self) -> String {
        match self.output {
            OutputMode::Capture => self.vm.interp().output(),
            OutputMode::Inherit => String::new(),
        }
    }

    /// Crate-internal accessor for the owned runtime (used by `eval`/`call` in Task 1.2).
    #[allow(dead_code)]
    pub(crate) fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.rt
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
    }
}
