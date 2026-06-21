# Playground

The **playground** compiles and runs AScript **entirely in your browser** — no server, no
install, nothing leaves the page. The same CST front-end, resolver, bytecode compiler, and
async VM that power the `ascript` binary are compiled to WebAssembly and run client-side. You
edit a program, press **Run**, and the captured output appears beside it.

> Open it from the **Playground** link in the top navigation (it is a standalone app page on
> this docs site, at `playground.html`). Your code runs locally in a sandboxed Web Worker.

## How it works

The browser loads a `wasm-bindgen` build of the engine and runs each program **off the UI
thread in a Web Worker** (a browser worker — JS plumbing only, *not* the AScript
[worker subsystem](../language/workers), which is unavailable on this platform). The worker
calls one entry point, `run_program(source)`, which compiles and runs the program on the
production bytecode VM under **deny-all capabilities**, captures `print`/`log` output, and
returns a structured result (output, an ANSI-free error or compile diagnostics, the exit
code, and a wall-clock duration).

Because the program runs in a worker, **Stop** can kill a runaway program — including an
infinite loop — by terminating the worker; the next **Run** lazily respawns it. This is the
only reliable way to interrupt wasm execution.

## What runs here — the stdlib subset

The wasm build ships a curated, platform-portable slice of the standard library. The CORE
language (and the modules below) compile to `wasm32-unknown-unknown`; the OS-touching modules
are compiled out. This table is the documentation contract — it mirrors the feature set in
`ascript-wasm/Cargo.toml`.

| Status | Modules | Notes |
|---|---|---|
| **In — core (always compiled)** | `math`, `string`, `array`, `object`, `map`/`set`, `convert`, `task`, `sync` (channels), `stream`, `time` (clock), `schema`, `caps`, `assert`, `bench`, plus the events / LRU / template utilities | un-gated core language |
| **In — features (`data`, `binary`, `log`, `shared`)** | `data` → `json`, `regex`, `encoding` (base64/hex/url/percent), `csv`, `yaml`, `uuid`, `url`; `binary` → `msgpack`, `cbor`; `log` (routed to the capture buffer); `shared` → `freeze` (pure in-memory) | the shipped wasm feature list |
| **Out — OS surface** | `fs`, `env`, `process`, `io`, `os`, `net`/`http`/`ws`/server, `sqlite`/`postgres`/`redis`, `compress`, `tui`, `workflow`, `ffi`, `intl`, `sysinfo`, `telemetry`, `ai`, `pkg` | the feature is absent, so `import "std/fs"` is the ordinary **unknown-module** error |
| **Out — platform** | workers (all three forms, `run_in_worker`, `task.pmap`/`preduce`), interval/timer resources (`time.interval`), the REPL | a clean **Tier-2 platform error** at the point of use |

> `crypto` and `datetime` are portable-in-principle but are **not** in the current wasm build
> — they remain candidates for a later release. Importing them gives the unknown-module error
> today.

## Platform differences from the native runtime

The playground is honest about what a browser sandbox can and can't do. Four differences from
running `ascript` natively:

1. **Lower recursion ceiling.** wasm uses a smaller `MAX_CALL_DEPTH`, so deeply recursive
   programs hit `maximum recursion depth exceeded` sooner — with the *same* error text as
   native.
2. **No workers, no timers, no OS stdlib.** The `worker` forms, `run_in_worker`,
   `task.pmap`/`preduce`, and `time.interval` raise a Tier-2 `… is not available on this
   platform (wasm)` error; `import "std/fs"` (and the other OS modules) are the ordinary
   unknown-module error. Nothing is silently stubbed.
3. **Captured output only.** `print` and `log` are captured and shown when the run finishes —
   there is no live streaming as the program executes (a long-running program shows nothing
   until it returns or you Stop it).
4. **Deny-all capabilities.** Every capability (`fs`, `net`, `process`, `ffi`, `env`) is
   denied. Even a core module with an OS touchpoint hits the [capability gate](../stdlib/caps)
   and is refused — defense in depth on top of the feature subset.

`await`, `task.gather`, generators, `time.sleep`, and the whole pure-compute and
data/serialization surface all work exactly as they do natively.

## Sharing a program

The **Share** button encodes the editor contents into the URL:

```text
playground.html#code=<base64url(source)>
```

Opening that URL populates the editor with the shared program. The encoding is URL-safe
base64 of the UTF-8 source; nothing is sent to a server.

## Examples to try

Pick from the **Examples** dropdown, or [browse the full example set](../examples) and paste
one in — anything that stays within the subset above will run.

## Report a bug

If a program behaves differently in the playground than with the native `ascript` binary (a
crash, wrong output, or a missing platform error), please
[open an issue](https://github.com/ascript-lang/ascript/issues) with the program and what you
expected — a playground/native output mismatch is a bug we want to fix.
