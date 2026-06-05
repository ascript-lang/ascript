//! Provider/model handles, credential resolution, and the genai `Client` lifetime.
//!
//! Phase A: the `AiClient` state cell + its lazy-init hook. Phases B–F fill in the
//! genai `Client` cache, the provider registry, and the `Value`→`ChatRequest`
//! mapping.

/// Per-`Interp` AI state: caches the lazily-built genai `Client` (one per `Interp`,
/// with our pooled reqwest client injected) and the registered provider handles.
/// Phase B materializes the cache + registry; Phase A only proves the lazy-init
/// hook is wired through `Interp::ai_state()` / `dispatch`.
#[derive(Default)]
pub struct AiClient {
    initialized: bool,
}

impl AiClient {
    /// Lazily mark the state as touched on first use. Phases B–F replace the body
    /// with the genai `Client` construction (injected reqwest client + resolver).
    /// Idempotent: re-entry is a no-op once initialized.
    pub(crate) fn ensure_initialized(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;
    }
}
