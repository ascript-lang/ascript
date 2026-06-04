# SP3 — Runtime robustness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each task is bite-sized TDD: write the failing test → run it, watch it fail for the right reason → implement → run it, watch it pass → run the phase gate → commit.

**Goal:** Make the runtime fail *cleanly* on two classes of large-but-valid input that crash the process today: (A) bytecode-capacity `panic!`/`.expect` in the compiler/`.aso` emit path → clean `CompileError`/serialization errors with actionable messages + non-zero exit; (B) deep non-yielding recursion / deeply nested expressions that `SIGABRT` (exit 134) → a clean, catchable Tier-2 panic `maximum recursion depth exceeded` at a fixed **logical** depth, **byte-identical on both engines**.

**Architecture:** Two phases plus a closing docs/holistic phase. **Phase A** = capacity panics → diagnostics (compile + `.aso`). **Phase B** = recursion-depth guard on both engines. **Phase C** = docs + holistic review. Each phase is TDD, ends green on both feature configs + clippy both + the whole-corpus three-way differential, and gets an independent review before the next. The tree-walker (`ascript run --tree-walker`) is the byte-identical oracle — never weaken it.

**Tech Stack:** Rust. CST front-end → resolver (`src/syntax/resolve`) → compiler (`src/compile/mod.rs`) → `Chunk` (`src/vm/chunk.rs`) → VM (`src/vm/*`). Legacy front-end → tree-walker (`src/interp.rs`). `.aso` versioned bytecode (`src/vm/aso.rs`, v7 after SP1).

**Spec:** `docs/superpowers/specs/2026-06-04-sp3-runtime-robustness-design.md`.

**Branch:** `feat/sp1-engine-parity` (continue here per the sub-project program; the spec is committed alongside this plan). If a fresh branch is preferred, branch from it before Task A1.

---

## Conventions for every task

- **Differential test harness:** `tests/vm_differential.rs` compares `ascript::vm_run_source(src)` (specialized VM), `ascript::vm_run_source_generic(src)` (generic VM), and `ascript::run_source_exit(src)` (tree-walker). "Byte-identical" = identical stdout + exit on all three. Add cases in the file's existing per-snippet style (read a few neighbors first; the file's helper, not an invented one).
- **Capacity / limit tests:** new `tests/vm_limits.rs` for §A oversize-module + §B over-limit-recursion CLI-level and unit-level cases; `tests/aso.rs` for `.aso` writer caps; `src/vm/{chunk,aso}.rs` inline `#[test]` for the unit-level capacity-error paths.
- **Per-engine manual smoke:** `cargo build` then `target/debug/ascript run X.as` (VM) vs `target/debug/ascript run --tree-walker X.as`. NOTE the tree-walker's legacy front-end requires `if (cond)` parens — write smoke `.as` with parenthesized conditions.
- **Gate after each phase (paste tails):**
  - `cargo test --test vm_differential 2>&1 | tail`
  - `cargo test --test vm_limits 2>&1 | tail` (Phase A/B), `cargo test --test aso 2>&1 | tail`
  - `cargo test 2>&1 | tail` (0 failures, all binaries)
  - `cargo test --no-default-features 2>&1 | tail` (0 failures)
  - `cargo clippy --all-targets 2>&1 | tail` AND `cargo clippy --no-default-features --all-targets 2>&1 | tail` (clean)
  - `grep await_holding_refcell_ref Cargo.toml` (still `deny`)
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Never** edit a passing tree-walker test or weaken a differential assertion to make the VM pass. **Never** add `unsafe`/`#[allow]`/`#[ignore]`/a stub/a TODO. A divergence on valid code = fix the root cause.
- **No 4 GB / no-multi-GB allocations in tests** — exercise the `u32` overflow path by feeding a fake length to the writer helper (Task A4), never by materializing the data.

---

## Phase A — Capacity panics → clean diagnostics

**Files:** `src/vm/chunk.rs` (pool/jump `.expect`/`panic!` → sticky overflow), `src/compile/mod.rs` (check the sticky flag at `finish`/top-level + audit the silent-skip site), `src/vm/aso.rs` (writer `.expect` → typed `AsoError`), the `build` command call site, new `tests/vm_limits.rs`, `tests/aso.rs`, `tests/cli.rs`. No production change in `src/vm/{opcode,verify}.rs` (sweep proves the negative). No tree-walker change (it has no bytecode caps).

### Task A1: failing oversize-module test (const pool)

- [ ] **Step 1 — Write the failing test.** In a new `tests/vm_limits.rs`, add a Rust helper that builds an AScript source string with > 65535 **distinct** constants (e.g. `let mut s=String::new(); for i in 0..70_000 { s.push_str(&format!("let _v{i} = {i}.5\n")); }` — distinct numbers so dedup does not collapse them; `{i}.5` keeps them fractional/distinct). Then:
```rust
#[tokio::test]
async fn const_pool_overflow_is_clean_error() {
    let src = gen_distinct_consts(70_000);
    let err = ascript::vm_run_source(&src).await.err()
        .expect("oversize module must error, not succeed");
    assert!(err.message.contains("65535 constants"),
        "expected const-pool capacity message, got: {}", err.message);
}
```
(Use the real `vm_run_source` error type — read `src/lib.rs:196` for the `Result` shape; the `.err()` field access matches `AsError`/`CompileError`'s `message`. Adjust to the actual error type.)
- [ ] **Step 2 — Run, verify it CRASHES the test process today** (`SIGABRT`, not a clean assertion failure): `cargo test --test vm_limits const_pool 2>&1 | tail -20` → the test binary aborts with `const pool exceeded u16::MAX` panic. (This is the "run-fails" evidence: a panic, not a returned error.)

### Task A2: sticky overflow flag on `Chunk` + capacity errors

- [ ] **Step 3 — Read** `src/vm/chunk.rs:312–364` (`add_const`/`add_proto`/`add_class_proto`/`add_import`, `emit_jump`/`emit_loop` at `:280–310`) and `src/compile/mod.rs` top-level `compile`/`compile_source` + each nested-proto `finish` (where a `Chunk` is sealed). Find where the compiler returns `Result<_, CompileError>`.
- [ ] **Step 4 — Implement** the sticky flag (spec §A3): add `overflow: std::cell::Cell<Option<ChunkLimit>>` to `Chunk` (new small enum `ChunkLimit { Consts(Span), Protos(Span), ClassProtos(Span), Imports(Span), Jump(Span), Loop(Span) }`; pool sites use the chunk's current recorded span, jump/loop use their `Span`). In each `add_*`/`emit_*` capacity site, replace the `.expect`/`panic!` with: record the **first** overflow (first-wins) into `overflow` and return a safe placeholder (`u16::MAX` index / skip the emit). Add `Chunk::take_overflow() -> Option<ChunkLimit>` and a `ChunkLimit::into_compile_error(self) -> CompileError` mapping each variant to the exact spec §A2 message.
- [ ] **Step 5 — Wire the check** in the compiler: after building the top chunk AND each nested proto's chunk, `if let Some(limit) = chunk.take_overflow() { return Err(limit.into_compile_error()) }`. (One check per sealed chunk so a nested-function overflow is caught too.)
- [ ] **Step 6 — Run** `cargo test --test vm_limits const_pool 2>&1 | tail` → PASS (clean `Err`, no panic).

### Task A3: proto / class-proto / import / jump-loop over-cap tests + verify

- [ ] **Step 7 — Add failing tests** (`tests/vm_limits.rs`): `proto_table_overflow` (> 65535 `fn` defs), `class_proto_overflow` (> 65535 `class` defs), `import_overflow` (> 65535 `import`s — keep generated source lean; may be slow, do NOT `#[ignore]`), `jump_displacement_overflow` (one function body with enough straight-line statements to force a > 32 KB forward jump — e.g. a single block of tens of thousands of `print(0)` so an enclosing `if`/loop jump exceeds `i16`). Each asserts the matching spec §A2 message.
- [ ] **Step 8 — Run** → the proto/class/import ones pass already (covered by the A2 sticky flag); the jump/loop ones drive the `emit_jump`/`emit_loop` sticky-flag conversion if not already covered — implement that conversion (same pattern). `cargo test --test vm_limits 2>&1 | tail` → all PASS.

### Task A4: `.aso` writer capacity → typed `AsoError`

- [ ] **Step 9 — Read** `src/vm/aso.rs:215–259` (`Writer::bytes`/`Writer::len`/`Writer::str`), `:340–360` (`to_bytes`/`write_chunk`), the `AsoError` enum (`:83`+), and the `build` command call site (`grep -rn 'to_bytes' src/ src/bin/ 2>/dev/null` / the CLI build path).
- [ ] **Step 10 — Failing unit test** (`src/vm/aso.rs` `#[cfg(test)]`): a helper that exercises the length-overflow path WITHOUT allocating — e.g. add a private `fn write_len(buf, n: usize) -> Result<(), AsoError>` and test `write_len(&mut w, (u32::MAX as usize) + 1)` → `Err(AsoError::TooLarge { .. })`; same for the byte-field length. Run → fails to compile / panics today.
- [ ] **Step 11 — Implement:** add `AsoError::TooLarge { what: &'static str, len: usize }` (Display per spec §A2 messages). Make `Writer::bytes`/`Writer::len`/`Writer::str` return `Result<(), AsoError>` (or, if the diff is smaller, a sticky `Writer.overflow: Option<AsoError>` checked once at the end of `to_bytes` — spec §A3 / open question O3; pick the smaller diff after reading the call graph). Thread the `Result` (or sticky check) so `to_bytes` returns `Result<Vec<u8>, AsoError>`; update the `build` command + tests to handle the `Result` with a clean message + non-zero exit. Keep the `:353` `.expect("…literals-only…")` (genuine compiler invariant, NOT a capacity site).
- [ ] **Step 12 — Run** `cargo test --test aso 2>&1 | tail` + the new unit tests → PASS, no allocation.

### Task A5: silent-skip audit + negative-sweep guard + CLI exit test

- [ ] **Step 13 — Audit the silent-skip site** (`src/compile/mod.rs:886` fresh-cell `if let Ok(slot) = u16::try_from(b.slot)`): prove it is unreachable past the `slot_count` guard (`:693`/`:2230` reject a frame with > 65535 slots before any fresh-cell emit). If provably unreachable, add a clarifying comment citing the guard; if NOT, convert it to record a `ChunkLimit` (no silent truncation may remain). Document the conclusion in the commit message.
- [ ] **Step 14 — Negative-sweep guard** (`tests/vm_limits.rs`): a test that reads `src/vm/chunk.rs` and `src/vm/aso.rs` as strings and asserts they contain **zero** non-`#[cfg(test)]` capacity `.expect("…exceed…")` / `panic!("…range…")` (a simple substring scan excluding lines inside `mod tests`). Trips if a future capacity `.expect` is re-introduced.
- [ ] **Step 15 — CLI exit test** (`tests/cli.rs`): run the oversize-const program through the built binary (`env!("CARGO_BIN_EXE_ascript")`) via `ascript run`; assert exit is the normal error exit (**not 134**, not a panic abort) and the actionable message appears on stderr.
- [ ] **Step 16 — Phase-A gate** (full gate set) + manual smoke (`target/debug/ascript build` of a normal program still works; an oversize one errors cleanly).
- [ ] **Step 17 — Commit:** `feat(vm): capacity panics → clean CompileError/AsoError diagnostics (no SIGABRT on oversize modules)`.

---

## Phase B — Recursion-depth guard (both engines, byte-identical)

**Files:** `src/interp.rs` (`call_depth: Cell<u32>` on `Interp`; `MAX_CALL_DEPTH` const; `DepthGuard`; increment in `run_body` + `eval_expr`), `src/vm/run.rs` (increment per frame push + `invoke_compiled_method` re-entry; increment per `compile_expr` nesting in `src/compile/mod.rs`), `src/lib.rs` + the CLI entry (enlarged worker stack so the limit fits under native capacity). Tests `tests/vm_differential.rs`, `tests/vm_limits.rs`, `tests/cli.rs`.

### Task B1: pin the limit + stack size empirically (spike, then fix the const)

- [ ] **Step 1 — Measure.** Write a throwaway recursion program and bisect the tree-walker's native overflow depth under (a) the stock stack and (b) an enlarged worker stack (a `std::thread` with `stack_size(64<<20)` hosting the `current_thread` runtime + `LocalSet`, or `tokio::runtime::Builder::new_current_thread().…thread_stack_size`). Confirm the tree-walker (largest per-frame budget) survives the intended `MAX_CALL_DEPTH` with ≥2× headroom under the enlarged stack. Record the chosen pair back into spec §B6 (proposed `MAX_CALL_DEPTH = 3000`, stack ≥ 64 MB; if 3000 does not fit with margin, drop to the largest value that does).
- [ ] **Step 2 — Implement the enlarged worker stack** at the entry points (`src/lib.rs` `run_file`/`run_source`/`run_tests` and the CLI `main`) so EVERY engine run gets the headroom. Keep it `current_thread` + `LocalSet` (the runtime model is fixed by `!Send`). Add `const MAX_CALL_DEPTH: u32 = <measured>;` in `src/interp.rs` (single source of truth, referenced by the VM).
- [ ] **Step 3 — Commit:** `chore(runtime): enlarged worker stack + MAX_CALL_DEPTH constant (groundwork for the recursion guard)`.

### Task B2: failing differential tests for the recursion limit

- [ ] **Step 4 — Write failing tests** (`tests/vm_differential.rs`), each asserting VM (spec+generic) == tree-walker byte-identical. Use parenthesized conditions (tree-walker front-end). A helper that generates a recursion driver to depth `N`:
```rust
fn rec_src(n: usize) -> String {
    format!("fn f(n) {{\n  if (n <= 0) {{ return 0 }}\n  return 1 + f(n - 1)\n}}\nprint(f({n}))\n")
}
// at limit - 1: completes, identical output
diff_case("recursion_at_limit_ok", &rec_src(MAX - 1));
// over limit: both panic "maximum recursion depth exceeded", identical, non-134 exit
diff_case("recursion_over_limit_panics", &rec_src(MAX + 50));
// mutual recursion over limit: per-logical-call counter
diff_case("mutual_recursion_over_limit",
    "fn a(n) { if (n <= 0) { return 0 } return b(n - 1) }\nfn b(n) { if (n <= 0) { return 0 } return a(n - 1) }\nprint(a(<MAX+50>))\n");
// recover catches it
diff_case("recover_catches_recursion_limit",
    "fn f(n) { return f(n + 1) }\nlet r = recover(() => f(0))\nprint(r[1].message)\n");
// deeply nested expression over the limit (B4)
diff_case("nested_expr_over_limit_panics", &nested_parens(MAX + 50)); // "let x = (((…1…)))"
```
(Use the file's real helper, the real `MAX_CALL_DEPTH` value, and the real `diff` API. `nested_parens` builds `"let x = " + "("*k + "1" + ")"*k`.)
- [ ] **Step 5 — Run, verify they FAIL the right way:** `cargo test --test vm_differential recursion mutual recover nested_expr 2>&1 | tail -30` → the over-limit cases currently `SIGABRT` (exit 134) the test binary OR diverge (VM 134 at a different depth than the tree-walker). That divergence/crash is the "run-fails" evidence.

### Task B3: tree-walker depth guard (`run_body` + `eval_expr`)

- [ ] **Step 6 — Read** `src/interp.rs:2302 run_body` (the call funnel) and `:1366 eval_expr` (the expr-nesting recursion), and the `Interp` struct fields (`:275`+, beside `inflight: Cell<u64>` at `:305`).
- [ ] **Step 7 — Implement:** add `call_depth: Cell<u32>` to `Interp` (init 0 in both `new`/`with_sink`). Add a `DepthGuard<'a>` RAII type that, on construction, increments `call_depth` and returns `Err(Control::Panic("maximum recursion depth exceeded" @ span))` if the new depth > `MAX_CALL_DEPTH`; on `Drop`, decrements. In `run_body`, acquire a guard at the top (before binding args / `exec`) anchored at `span`. In `eval_expr`, acquire a guard at the top anchored at the expr's span (this covers nested-expression recursion, spec §B4). (`Cell`, never held across `.await` — the guard is a stack value that the `?`/panic unwinds correctly; verify clippy `await_holding_refcell_ref` stays clean — it is a `Cell`, so trivially.)
- [ ] **Step 8 — Run** the tree-walker side: `cargo test --test vm_differential recursion_over_limit 2>&1 | tail` — the tree-walker now emits the clean panic (VM may still differ until B4). Confirm no SIGABRT on the tree-walker.

### Task B4: VM depth guard (frame push + native re-entry + compile nesting)

- [ ] **Step 9 — Read** `src/vm/run.rs:994` (frame push, the in-loop call path), `:3392/3414 invoke_compiled_method` (native re-entry via `self.run`), `:1026` (`call_value` "other"-callee re-entry), and `src/compile/mod.rs` `compile_expr`/`eval_chain` (the compile-time expr nesting). The VM holds `interp: Rc<Interp>` (`run.rs:52`) → reach the SAME `call_depth`.
- [ ] **Step 10 — Implement:** increment `interp.call_depth` (via the SAME `DepthGuard` / a shared check helper) at **every logical call-frame establishment**: the in-loop `fiber.frames.push` (`:994`) and the native re-entries (`invoke_compiled_method` entry `:3392`, `call_value` "other" branch `:1026`); decrement on the matching RETURN / call return (RAII guard around the re-entrant `self.run`; for the in-loop frame push, increment on push and decrement on the RETURN that pops that frame — so the VM's logical depth tracks `fiber.frames` total depth across re-entrant fibers). Over the limit → the same `Control::Panic` "maximum recursion depth exceeded" anchored at the call span. Additionally, increment per `compile_expr` nesting in the compiler so a deeply nested **source expression** trips at compile time (wrapped at the lib boundary into the same `Control::Panic` so stdout+exit match the tree-walker — spec §B4/O1).
- [ ] **Step 11 — Run** all Phase-B differential tests: `cargo test --test vm_differential recursion mutual recover nested_expr 2>&1 | tail -30` → **byte-identical both engines** (spec+generic == tree-walker). If the over-limit depth differs by one between engines, align the increment point (one increment per logical call on each engine — adjust which funnel increments so the counts match exactly).

### Task B5: no-SIGABRT CLI guard + margin guard + corpus re-check

- [ ] **Step 12 — CLI no-134 test** (`tests/cli.rs` or `tests/vm_limits.rs`): run an over-limit recursion through the built binary under BOTH `ascript run` (VM) and `ascript run --tree-walker`; assert exit is the clean recoverable-panic exit (**not 134**) with `maximum recursion depth exceeded` on stderr. Also run a `MAX-1`-deep program and assert exit 0 + correct output on both.
- [ ] **Step 13 — Margin guard** (`tests/vm_limits.rs`, runs under the enlarged worker stack): a `MAX_CALL_DEPTH`-deep tree-walker recursion (largest per-frame budget) reaches the **clean panic** with no SIGABRT — proving the stack-size/limit pair has headroom.
- [ ] **Step 14 — Whole-corpus differential unchanged:** `cargo test --test vm_differential 2>&1 | tail` — the existing three-way corpus + goldens stay byte-identical (the limit is far above any corpus recursion; the guard must not alter any normal program's output).
- [ ] **Step 15 — Perf:** `cargo test --release --test vm_bench -- --ignored --nocapture` — geomean ≥2×, no spec-vs-generic regression (the per-call `Cell` inc/dec must not regress it). If it regresses, the increment is in too hot a path — move it to the single call funnel, not per-bytecode.
- [ ] **Step 16 — Phase-B gate** (full gate set) + manual smoke (over-limit + `recover`-wrapped + mutual + nested-expr on both engines).
- [ ] **Step 17 — Commit:** `feat(runtime): recursion-depth guard → clean catchable panic on both engines (byte-identical, no SIGABRT)`.

---

## Phase C — Docs + holistic review

**Files:** `CLAUDE.md`, `docs/superpowers/specs/2026-05-29-ascript-design.md`, `docs/content/*`.

### Task C1: docs

- [ ] **Step 1 — `CLAUDE.md`:** add a note (in the VM/interp robustness area) covering (a) the §A capacity-error asymmetry (VM rejects oversize modules cleanly; tree-walker has no bytecode caps — documented, not a parity hole), and (b) the §B recursion-depth guard (`MAX_CALL_DEPTH`, single `Interp.call_depth` shared by both engines, increment points = `run_body`+`eval_expr` (tree-walker) and frame-push+`invoke_compiled_method`+`compile_expr` (VM), the enlarged worker stack, the residual divergence = SP9 non-goal).
- [ ] **Step 2 — Language spec** (`docs/superpowers/specs/2026-05-29-ascript-design.md`) error-model section: document the Tier-2 `maximum recursion depth exceeded` panic (catchable by `recover`) and the bytecode-capacity compile errors. Cross-reference the §7 async non-goals for the unbounded-recursion residual.
- [ ] **Step 3 — `docs/content`** error-handling page: mention the recursion limit + that `recover` catches it; verify any snippet against the binary.
- [ ] **Step 4 — Commit:** `docs: capacity-error asymmetry + recursion-depth guard (CLAUDE.md, spec, content)`.

### Task C2: holistic gate + independent review

- [ ] **Step 5 — Full gate set** both feature configs + clippy both + `vm_bench` (≥2×, no spec-vs-generic regression) + the whole-corpus three-way differential (byte-identical).
- [ ] **Step 6 — Independent review** (re-read spec, re-run gates, adversarial hunt): oversize-module shapes (consts/protos/classes/imports/jump/`.aso` byte+collection); over-limit recursion via plain functions, methods (IC fast path AND `invoke_compiled_method` re-entry), mutual recursion, `recover`-wrapped, `async fn` bodies, generator bodies, deeply nested expressions; confirm the negative-sweep guard trips on a re-introduced `.expect`; confirm no SIGABRT (exit 134) on any input on either engine. Fix any divergence/crash at the root.
- [ ] **Step 7 — Final commit** if review surfaced fixes; otherwise the sub-project is complete.

---

## Self-review (author)

**Spec coverage:** §A capacity panics → Phase A (A1–A5: const/proto/class/import + jump/loop in `chunk.rs`; `.aso` writer in `aso.rs`; silent-skip audit; negative-sweep guard; CLI exit). §B recursion guard → Phase B (B1 limit/stack spike; B2 failing differential; B3 tree-walker `run_body`+`eval_expr`; B4 VM frame-push+re-entry+compile-nesting; B5 no-SIGABRT+margin+corpus+perf). Docs/residual-divergence → Phase C. All §A and §B scope items covered; the `opcode.rs`/`verify.rs` sweep is the explicit "prove the negative" with the negative-sweep guard (Task A5/Step 14).

**Placeholder scan:** No "TBD/handle edge cases". The ONE deferred-to-implementer item is the exact `MAX_CALL_DEPTH` constant + worker stack size — and that is deliberately a measured spike (Task B1) recorded back into spec §B6, not a placeholder (the *method* is fully specified: single shared const, enlarged stack, bisect-to-fit with ≥2× margin; proposed 3000 / 64 MB). Test programs are concrete AScript with parenthesized conditions (tree-walker front-end). `diff_case`/helper names are illustrative — the implementer uses the file's actual harness.

**Consistency:** `MAX_CALL_DEPTH` is one `const` in `src/interp.rs`, referenced by the VM via `interp: Rc<Interp>` — both engines increment the SAME `Interp.call_depth: Cell<u32>`, so the limit is provably identical → byte-identical at/over the limit. The panic message string `maximum recursion depth exceeded` is identical in spec §B2 and every test here. `Cell` (not `RefCell`) keeps `await_holding_refcell_ref` trivially clean. Phase A returns typed errors (`CompileError`/`AsoError`) up existing channels — no new exit path, no panic, no exit 134. Both phases are CORE (build + pass under `--no-default-features`).

**Byte-identical risk (the crux):** the differential stays byte-identical because (a) the limit is far above any corpus/normal program (no corpus run changes), and (b) at/over the limit both engines increment the same shared counter at one-increment-per-logical-call granularity and emit the same `Control::Panic` message+span → identical stdout+exit. Task B4/Step 11 explicitly aligns the increment points if the engines' over-limit depth differs by one.
