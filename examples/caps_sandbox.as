// std/caps — sandbox an untrusted plugin in a capability-restricted worker isolate.
//
// THE KEYSTONE: capabilities are per-isolate, and a worker has its OWN `Interp` +
// heap. `run_in_worker(fn, input, { caps: { deny: [...] } })` spawns a DEDICATED
// (single-tenant) isolate carrying a REDUCED capability set and runs the plugin
// there — a real, memory-isolated sandbox, not an in-process API gate. Denying
// `ffi`/`process`/`net` to the plugin is enforced because the isolate shares no
// memory with the host (only structured-clone bytes cross the airlock).
//
// Capabilities are DEFAULT-ALL-GRANTED and subtracted (opt-OUT). The host keeps its
// own capabilities; only the dedicated plugin isolate is restricted.
import * as caps from "std/caps"
import * as ffi from "std/ffi"

// An untrusted plugin: it tries to open a C library (an `ffi` capability) and
// reports what happened. In a sandbox that denied `ffi`, the attempt is a
// recoverable denial the plugin can observe (and a host can audit).
worker fn plugin(input: number): string {
  let probe = recover(() => ffi.open("libm.so.6"))
  if (probe[1] != nil) {
    return `plugin: ffi denied — ${probe[1].message}`
  }
  return "plugin: ffi allowed (opened a library)"
}

fn main() {
  // The host still has every capability.
  print(`host has ffi: ${caps.has("ffi")}`) // host has ffi: true

  // Run the plugin in a DEDICATED isolate with ffi + process denied.
  let result = await run_in_worker(plugin, 0, {caps: {deny: ["ffi", "process"]}})
  print(result) // plugin: ffi denied — capability 'ffi' denied

  // The host isolate is UNAFFECTED — the drop applied only to the plugin's isolate.
  print(`host still has ffi: ${caps.has("ffi")}`) // host still has ffi: true
}

await main()
