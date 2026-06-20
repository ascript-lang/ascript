//! Host modules — the `host:` namespace registration surface (EMBED, spec §6).
//!
//! A host module is a collection of constant values + host functions the embedder
//! hands the script under a collision-proof `host:` import scheme
//! (`import * as app from "host:app"`). Host functions are native Rust the host
//! wrote — they run synchronously on the isolate thread and **bypass the capability
//! gate** (§6.3): a host fn is the host's *own* trusted code, so if it proxies a
//! dangerous effect, gating is the host's job. The registry, the import arm, and the
//! dispatch all live on the shared `Interp` (the CORE `HostModuleDef`/`HostFnEntry`
//! types in `src/interp.rs`), so both engines see identical behavior.
//!
//! This module is the `embed`-gated, `AsValue`-typed BUILDER that adapts the host's
//! closures into the engine-`Value` CORE closures the registry stores.

use crate::embed::value::AsValue;
use crate::interp::{HostCallCtx, HostFnEntry, HostInvokeError, HostModuleDef};

/// The error a host function returns — the FFI §3.1 two-tier split, host-chosen
/// per function (spec §6.2).
#[derive(Debug, Clone)]
pub enum HostError {
    /// A recoverable, data-shaped failure (the FFI "dlopen failed" class). In a
    /// [`HostModuleBuilder::fallible_func`] it becomes the Tier-1 `[nil, err]` pair's
    /// err half; in a plain [`HostModuleBuilder::func`] it is upgraded to a Tier-2
    /// recoverable panic (a plain fn has no err channel — upgrading beats silently
    /// swallowing).
    Recoverable(String),
    /// Programmer-misuse / invariant violation (the FFI marshalling-misuse class): a
    /// Tier-2 recoverable panic carrying the message, catchable by `recover`. Identical
    /// from either builder form.
    Panic(String),
}

impl HostError {
    /// Adapt to the CORE `HostInvokeError` the registry closure raises.
    fn into_core(self) -> HostInvokeError {
        match self {
            HostError::Recoverable(message) => HostInvokeError {
                message,
                recoverable: true,
            },
            HostError::Panic(message) => HostInvokeError {
                message,
                recoverable: false,
            },
        }
    }
}

/// The per-call context handed to a host function (spec §6.2). v1 exposes the call
/// span and a `print`-equivalent output hook — NOT the `Interp` itself (re-entrant
/// `eval` from inside a host fn under a live dispatch borrow is the classic embedding
/// footgun; rejected v1, §10).
pub struct HostCtx<'a, 'b> {
    core: &'a HostCallCtx<'b>,
}

impl HostCtx<'_, '_> {
    /// The call-site span as `(start, end)` char offsets.
    pub fn span(&self) -> (usize, usize) {
        (self.core.span.start, self.core.span.end)
    }

    /// Emit output (the `print`-equivalent — routes to the isolate's configured output
    /// sink, capture buffer or stdout).
    pub fn print(&mut self, s: &str) {
        (self.core.out)(s);
    }
}

/// Builds a single host module's exports (spec §6.2). Handed to the
/// `host_module`/`host_module_factory` closure.
///
/// # Security (§6.3) — host functions BYPASS capabilities
///
/// A host function is **native Rust the host wrote**: it runs with the host process's
/// full authority and is **NOT subject to the [`Caps`](crate::embed::Caps) gate**. If a
/// host fn proxies a dangerous effect (a file read, a network call, a subprocess),
/// **gating that effect is the host's responsibility** — the embedded capability model
/// governs the *script's* `std/*` calls, not the host's own functions. A worker isolate
/// can never reach a host fn its isolate did not register (a registry miss is a clean
/// recoverable panic, §6.4).
pub struct HostModuleBuilder {
    values: Vec<(String, crate::value::Value)>,
    fns: std::collections::HashMap<String, HostFnEntry>,
}

impl HostModuleBuilder {
    pub(crate) fn new() -> Self {
        HostModuleBuilder {
            values: Vec::new(),
            fns: std::collections::HashMap::new(),
        }
    }

    /// Register a constant export (any [`AsValue`]).
    pub fn value(&mut self, name: &str, v: AsValue) {
        self.values.push((name.to_string(), v.into_value()));
    }

    /// Register a plain host function: `Ok(v)` returns `v`; `Err(HostError::Recoverable)`
    /// is **upgraded to a Tier-2 recoverable panic** (a plain fn has no err channel);
    /// `Err(HostError::Panic)` is a Tier-2 recoverable panic.
    ///
    /// # Security (§6.3) — host functions BYPASS capabilities
    ///
    /// This function runs as **native Rust with the host process's full authority** and
    /// **BYPASSES the [`Caps`](crate::embed::Caps) gate**. Gating any dangerous effect it
    /// proxies is the host's responsibility — capabilities govern the script's `std/*`
    /// calls, not the host's own functions.
    pub fn func(
        &mut self,
        name: &str,
        f: impl Fn(&mut HostCtx, &[AsValue]) -> Result<AsValue, HostError> + 'static,
    ) {
        self.fns.insert(name.to_string(), wrap(f, false));
    }

    /// Register a Tier-1 fallible host function: it ALWAYS returns the `[value, err]`
    /// pair — `Ok(v)` → `[v, nil]`; `Err(HostError::Recoverable(e))` → `[nil, {message: e}]`.
    /// `Err(HostError::Panic)` still raises a Tier-2 recoverable panic (misuse is misuse).
    ///
    /// # Security (§6.3) — host functions BYPASS capabilities
    ///
    /// This function runs as **native Rust with the host process's full authority** and
    /// **BYPASSES the [`Caps`](crate::embed::Caps) gate**. Gating any dangerous effect it
    /// proxies is the host's responsibility — capabilities govern the script's `std/*`
    /// calls, not the host's own functions.
    pub fn fallible_func(
        &mut self,
        name: &str,
        f: impl Fn(&mut HostCtx, &[AsValue]) -> Result<AsValue, HostError> + 'static,
    ) {
        self.fns.insert(name.to_string(), wrap(f, true));
    }

    /// Finish: produce the CORE `HostModuleDef` the registry stores.
    pub(crate) fn finish(self) -> HostModuleDef {
        HostModuleDef {
            values: self.values,
            fns: self.fns,
        }
    }
}

/// Wrap an `AsValue`-typed host closure into the CORE `Value`-typed `HostFnEntry` the
/// registry stores. The CORE closure adapts the args (`Value` → `AsValue` handles, a
/// refcount bump — the §5 by-handle model), builds the `AsValue` `HostCtx` over the
/// core ctx, runs the host fn, and adapts the result/error back to `Value`/core.
fn wrap(
    f: impl Fn(&mut HostCtx, &[AsValue]) -> Result<AsValue, HostError> + 'static,
    fallible: bool,
) -> HostFnEntry {
    let core = move |ctx: &HostCallCtx, args: &[crate::value::Value]| {
        let as_args: Vec<AsValue> = args.iter().cloned().map(AsValue::from_value).collect();
        let mut hctx = HostCtx { core: ctx };
        f(&mut hctx, &as_args)
            .map(AsValue::into_value)
            .map_err(HostError::into_core)
    };
    HostFnEntry {
        f: std::rc::Rc::new(core),
        fallible,
    }
}

/// A per-isolate host-module factory (spec §6.4): the validated full `host:<name>` +
/// an `Arc<dyn Fn(&mut HostModuleBuilder) + Send + Sync>`. The closure runs INSIDE a
/// freshly-spawned worker isolate thread (so it is `Send + Sync`, and the host fns it
/// builds may close over `Send + Sync` host state only). Carried beside `caps` on the
/// worker spawn paths.
pub(crate) type HostModuleFactory = (
    std::rc::Rc<str>,
    std::sync::Arc<dyn Fn(&mut HostModuleBuilder) + Send + Sync>,
);

/// Run a factory closure on a fresh builder and produce the CORE `HostModuleDef` —
/// called INSIDE a worker isolate thread to install a host module per §6.4.
pub(crate) fn build_factory(
    f: &std::sync::Arc<dyn Fn(&mut HostModuleBuilder) + Send + Sync>,
) -> HostModuleDef {
    let mut b = HostModuleBuilder::new();
    f(&mut b);
    b.finish()
}

/// Validate a host-module name (spec §6.1): `host:` + a `/`-segmented identifier path
/// where each segment is `[a-z][a-z0-9_]*`. NO dots (the builtin dispatch splits the
/// qualified fn name at the first `.`, so a dotted module name would mis-split), no
/// empty segments. Returns `Ok(())` on success, or an error string describing the
/// violation.
///
/// Hand-rolled (NO regex dependency in core — `host_modules` is a CORE field that must
/// compile under `--no-default-features`).
pub(crate) fn validate_module_name(full: &str) -> Result<(), String> {
    let Some(rest) = full.strip_prefix("host:") else {
        return Err(format!(
            "host module name '{full}' must start with the 'host:' scheme \
             (e.g. \"host:app\")"
        ));
    };
    if rest.is_empty() {
        return Err(format!(
            "host module name '{full}' is empty after the 'host:' scheme"
        ));
    }
    // `/`-segmented; every segment a lowercase identifier. No dots anywhere (a dot
    // would mis-split the qualified `host:app.fn` dispatch name).
    for segment in rest.split('/') {
        if segment.is_empty() {
            return Err(format!(
                "host module name '{full}' has an empty path segment"
            ));
        }
        let mut chars = segment.chars();
        let first = chars.next().unwrap();
        if !first.is_ascii_lowercase() {
            return Err(format!(
                "host module name '{full}': segment '{segment}' must start with a \
                 lowercase ascii letter [a-z]"
            ));
        }
        for c in chars {
            if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                return Err(format!(
                    "host module name '{full}': segment '{segment}' may contain only \
                     [a-z0-9_] (no dots, no uppercase)"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names_accepted() {
        assert!(validate_module_name("host:app").is_ok());
        assert!(validate_module_name("host:my_app").is_ok());
        assert!(validate_module_name("host:app/sub").is_ok());
        assert!(validate_module_name("host:a1/b2_c").is_ok());
    }

    #[test]
    fn missing_prefix_rejected() {
        assert!(validate_module_name("app").is_err());
    }

    #[test]
    fn empty_after_prefix_rejected() {
        assert!(validate_module_name("host:").is_err());
    }

    #[test]
    fn dotted_name_rejected() {
        // A dot would mis-split the qualified `host:My.App.fn` dispatch name.
        assert!(validate_module_name("host:My.App").is_err());
        assert!(validate_module_name("host:app.sub").is_err());
    }

    #[test]
    fn uppercase_rejected() {
        assert!(validate_module_name("host:App").is_err());
    }

    #[test]
    fn empty_segment_rejected() {
        assert!(validate_module_name("host:app//sub").is_err());
    }
}
