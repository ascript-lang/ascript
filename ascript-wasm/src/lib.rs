//! ascript-wasm — the browser playground entry (WASM spec §5.4).
//!
//! A thin `wasm-bindgen` wrapper over [`ascript::wasm_run_source`]: compile + run a
//! source string on the production bytecode VM with the `OutputSink::Capture` sink and
//! a **deny-ALL** [`CapSet`](ascript::stdlib::caps::CapSet) (fs/net/process/ffi/env all
//! denied — §5.4). Every result, success or failure, is returned as a serde-serialized
//! [`RunResult`] (camelCase) — no panic ever crosses the FFI boundary un-caught.
use wasm_bindgen::prelude::*;

/// The playground result contract (WASM §5.4 / §6). serde-renamed to camelCase for the
/// JS side; `error`/`exitCode` are `null` when absent.
#[derive(serde::Serialize)]
struct RunResult {
    /// `true` iff the program ran to completion without a Tier-2 panic / compile error.
    ok: bool,
    /// `OutputSink::Capture` contents (`print` + captured `log`). Empty string on a
    /// Tier-2 panic / compile error in v1 — see the module note on partial output.
    output: String,
    /// Plain-text (ANSI-free) rendering of a panic / compile error; `null` on success.
    error: Option<String>,
    /// Compile/check diagnostics, ariadne-rendered with color OFF (one entry per error;
    /// empty on a successful run or a pure runtime panic).
    diagnostics: Vec<String>,
    /// The `exit(n)` code if the program called `exit`; `null` otherwise.
    #[serde(rename = "exitCode")]
    exit_code: Option<i32>,
    /// Wall-clock duration of the run in milliseconds (`platform::monotonic_ms` delta).
    #[serde(rename = "durationMs")]
    duration_ms: f64,
}

/// `wasm-bindgen` start hook: install the panic hook so a Rust panic inside the engine
/// surfaces as a readable console error (a playground bug, never a silent dead button —
/// §6 Gate-14 class) instead of an opaque `unreachable` wasm trap.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Compile + run `source` on the VM under deny-ALL caps and return a [`RunResult`] as a
/// `JsValue`. `async` because the engine is async (workers/timers refuse on wasm but
/// `await`/`task.gather`/`time.sleep` work). [`ascript::wasm_run_source`] owns the
/// `LocalSet` the VM runs under (the `!Send`, current-thread runtime model — §4.2's
/// recorded executor config, no enter-guard needed per the Phase-0 spike), so the
/// wrapper just awaits it on the wasm-bindgen-futures microtask executor.
#[wasm_bindgen]
pub async fn run_program(source: String) -> JsValue {
    let t0 = ascript::platform::monotonic_ms();

    // WASM §5.4: deny ALL five dangerous caps (fs/net/process/ffi/env). The playground is
    // a sandbox — a program that touches the OS gets the cap error, never silent access.
    let mut caps = ascript::stdlib::caps::CapSet::all_granted();
    caps.deny_all_dangerous();

    let res = ascript::wasm_run_source(&source, caps).await;

    let result = match res {
        Ok((output, exit_code)) => RunResult {
            ok: true,
            output,
            error: None,
            diagnostics: Vec::new(),
            exit_code,
            duration_ms: ascript::platform::monotonic_ms() - t0,
        },
        Err(e) => {
            // §5.4: ariadne rendered with color OFF — the JS-facing error/diagnostics
            // strings carry NO ANSI escapes (the `<pre>` pane shows plain text).
            let rendered = ascript::diagnostics::render_to_string(&e, false);
            RunResult {
                ok: false,
                // v1: a Tier-2 panic / compile error loses prior captured `print` output
                // (`AsError` does not carry the capture buffer; `wasm_run_source` returns
                // only the `Err`). Documented in tooling/playground.md.
                output: String::new(),
                error: Some(rendered.clone()),
                diagnostics: vec![rendered],
                exit_code: None,
                duration_ms: ascript::platform::monotonic_ms() - t0,
            }
        }
    };

    serde_wasm_bindgen::to_value(&result).expect("RunResult is plain serde data — serializes")
}
