# Goal — Serious Language Campaign (brief)

Make AScript a serious general-purpose language — **capability, correctness, performance, DX**; pre-1.0,
no backward-compat constraint. Full tracker: `goal.md`. Specs/plans:
`superpowers/{specs,plans}/2026-06-08-*.md`.

## Specs (dependency order; each = a branch off `main`, merged `--no-ff`)
- **P0** — fix the live `.aso` reader unclamped-allocation bug (`.min(remaining)`). Gates BIN.
- **NUM** — int(i64) default, float(f64); type-directed `/` (no `//`); checked overflow + `+% -% *%`;
  bitwise (Go precedence); code-points-as-int; truthiness `0/0.0/0m/NaN/""` falsy (collections truthy).
  **Breaking. Merges FIRST** (`Value::Number→Float`, +`Value::Int`).
- **VAL** — compact `Value` 32B→≤16B→8B (NaN-box gated on gcmodule `Cc::into_raw` else 16B). Perf-only.
- **ADT** — enum payload variants + exhaustive match. **IFACE** — structural interfaces. → **TYPE** —
  sound-for-annotated gradual types + invariant generics.
- **FFI** — `std/ffi` + opt-out per-isolate caps (untrusted→dedicated-isolate sandbox).
- **SRV** — multi-isolate HTTP (SO_REUSEPORT) + `std/shared` frozen `Arc` heap (first Send value).
- **BIN** — `ascript build --native` single binary (gated on FUZZ).
- **DBG** — DAP debugger + profiler (zero-cost-off: breakpoint-patching + unified `Vm.instrument`).
- **FUZZ** (continuous) — differential + `.aso`/clone fuzzing + property tests.
- **DX** (continuous) — `ascript doc`, parallel tests, LSP completion, diagnostics, docs repositioning.
- **JIT** (deferred) — Cranelift baseline; only after NUM+VAL+profiling.

**Order:** P0 → NUM → {VAL,ADT,IFACE,FFI,SRV,BIN,DBG} → TYPE(after ADT+IFACE); FUZZ+DX continuous.

## How to work
Per spec: lock → plan → subagent-driven TDD (fresh implementer + independent reviewer who runs commands
+ probes edges) → holistic review → merge. Per task: failing test → minimal code → green → commit
(trailer per `CLAUDE.md`).

## Gates (non-negotiable — fix the code, never the assertion)
0. **PRODUCTION-GRADE, ZERO LINGERING BUGS.** Validate + harden every trust boundary (parse/`.aso`/
   clone/FFI/net/caps); deterministic cleanup; exhaustive error handling. No stub/`unwrap`/`expect`/
   `panic`/`unreachable` on reachable paths, no swallowed error, no silent overflow/truncation/coercion.
   **ANY bug found — ours OR pre-existing, direct OR incidental — MUST be fixed in-branch with a
   failing-test regression guard, never deferred** (if genuinely too large: owner note + a failing test,
   never silent). Reviewer hunts missed validation + latent bugs in the touched code AND neighbors, and
   runs the commands. Evidence before "done."
1. Four-mode byte-identity: tree-walker == specialized == generic == `.aso`, both feature configs.
2. Clippy clean (`--all-targets` AND `--no-default-features --all-targets`).
3. `cargo test` AND `cargo test --no-default-features` green.
4. No `await` across a `RefCell`/resource borrow; native handles stay GC-opaque.
5. Zero `type-*` false positives on `examples/**`.
6. No placeholders/silent deferrals; no silent overflow/truncation/cap-bypass.
7. Breaking changes migrate the corpus (never delete to dodge a break).
8. Examples + unit tests cover happy AND edge (both configs).
9. Tooling parity VERIFIED green: both parsers, tree-sitter (regen+publish+pins), formatter, LSP, REPL.
10. Zero perf regression: instrumentation zero-cost-off (benched); VM geomean ≥2× tree-walker.
11. Docs updated (+NAV); scripting→general-purpose repositioning.

## Cross-spec reconciliation
- `.aso` version + wire/const tag numbers SEQUENTIAL — read current, next-free, never hardcode. NUM
  merges first (rebase onto `Int`/`Float`); one grammar publish per merge wave.
- `emit` gains a severity arg (ADT+TYPE, first adds it). DX/DBG: first introduces `Vm.instrument`. TYPE
  owns `conforms`, IFACE emits `implements-violation`. Variant-adders land before VAL Stage-2.
