// AScript playground — browser Web Worker driver (WASM spec §5.5).
//
// This is a *browser* Web Worker (JS-side plumbing only) — NOT the AScript worker
// subsystem (which is unavailable on wasm and refuses with a Tier-2 platform error).
// It runs the wasm engine off the UI thread so the page stays responsive, and so the
// page can kill a runaway program with worker.terminate() + a lazy respawn (the only
// reliable way to stop an infinite loop in wasm).

let ready = (async () => {
  const mod = await import('./playground/pkg/ascript_wasm.js');
  await mod.default();
  return mod;
})();

self.onmessage = async (e) => {
  const { id, source } = e.data;
  try {
    const mod = await ready;
    const result = await mod.run_program(source);
    self.postMessage({ id, result });
  } catch (err) {
    self.postMessage({ id, result: { ok: false, output: '', error: String(err),
      diagnostics: [], exitCode: null, durationMs: 0 } });
  }
};
