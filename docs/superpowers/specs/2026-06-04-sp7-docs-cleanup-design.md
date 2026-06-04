# SP7 — Docs & cleanup — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover gap program (SP1–SP10). This is the **cleanup** sub-project:
> it carries no new runtime behavior. Every change is either a documentation/comment edit, an
> internal-invariant message reword, a single new (passing) differential assertion, or a pure
> `cargo fmt` reflow.

**Goal:** Bring the repo's docs and in-tree comments back in sync with the post-VM/post-GC/post-CST
reality, retire stale "not yet"/"deferred" markers that describe already-shipped work, resolve the
stale opt-out comment for issue #147, normalize the source tree with `cargo fmt`, and record the
known/accepted SP1 trade-offs in the canonical places. **No behavior change** anywhere except the
one new (already-green) differential assertion and the formatting reflow.

**Tech stack:** Rust. Affected surfaces: Markdown docs (`docs/content/*`, `docs/superpowers/*`),
Rust doc-comments/inline comments, one differential test (`tests/vm_differential.rs`), and a
whole-tree `cargo fmt`. No `Value` change, no opcode change, no grammar change, no `.aso` change.

---

## Invariants (must stay green after EVERY commit)

- **Whole-corpus three-way differential** (`tests/vm_differential.rs`): tree-walker == specialized-VM
  == generic-VM, byte-identical. Nothing here changes engine behavior, so it must stay green
  unchanged (the one new assertion is an *addition* of a case the engines already agree on).
- **Both feature configs:** `cargo test` (default) AND `cargo test --no-default-features`, 0 failures.
- **Clippy clean** under `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets`.
- **`await_holding_refcell_ref = "deny"`** stays in `Cargo.toml` and clean.
- **Per-commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- The `cargo fmt` reflow is **isolated in its own commit** so it never muddies a content diff.

---

## §1 — Stale user/record docs (verified stale)

Each was confirmed against current code. These are user-facing or historical-record docs; per repo
convention the **historical-record** docs (roadmap, design-spec non-goals, the ranges current-state
table) are updated with a **dated note** rather than rewritten, while the **reference** doc
(`net.md`) is corrected outright (it documents current behavior, not history).

| # | Doc location | Stale claim | Current reality (evidence) | Fix |
|---|---|---|---|---|
| 1a | `docs/content/stdlib/net.md:416` | "requests are handled **strictly sequentially** … Concurrent connections are a documented v1 limitation." | Each accepted connection is handled on its **own `spawn_local` task** with a `tokio::sync::Semaphore` concurrency cap (`src/stdlib/http_server.rs:17,24,771,805`; the differential test at `:1641` proves a slow handler no longer blocks others). | Rewrite the paragraph to describe per-connection `spawn_local` + the `Semaphore` cap (concurrent connections, bounded). Reference docs track current behavior. |
| 1b | `docs/superpowers/roadmap.md:611` | "**REPL is single-line:** multi-line blocks typed across lines aren't accumulated" | REPL accumulates multi-line input on a `..` prompt while `is_incomplete` (CLAUDE.md "REPL multi-line input"; `src/repl.rs`). | Append a dated note ("**Update 2026-06-04:** shipped — multi-line accumulation lands via the `is_incomplete` token-depth buffer.") — keep the original line as record. |
| 1c | `docs/superpowers/roadmap.md:613` | "**fmt drops comments** … re-emits string literals with `"` + raw contents" | The CST formatter (`src/syntax/format`) is the shipped formatter and preserves comments/round-trips. | Append a dated note that the lossless CST formatter superseded the AST pretty-printer; keep the original line as record. |
| 1d | `docs/superpowers/specs/2026-05-29-ascript-design.md:33` | §1 non-goal "No bytecode VM or JIT (tree-walker only)." | The bytecode VM is the **default** engine; the tree-walker is the `--tree-walker` reference oracle (CLAUDE.md architecture; `src/lib.rs` `vm_run_source`). | Append a dated parenthetical note to the bullet ("**Superseded 2026-06-04:** a bytecode VM is now the default engine; the tree-walker is the byte-identical reference oracle. JIT remains a non-goal.") — do NOT delete the historical bullet. |
| 1e | `docs/superpowers/specs/2026-06-03-ranges-step-analyzer-design.md:67,68,271` | "current state" table rows mark `..=` in for-range and `..=` as a value as **rejected**, and `:271` says "currently `..=` is rejected outside patterns". | Both work: `print(1..=5)` → `[1,2,3,4,5]` and `for (i in 10..=1 step -2)` iterates `10,8,6,4,2` (verified by running both). | This is the design doc's "status today" snapshot — append a dated note at the table ("**Update 2026-06-04:** the rejections in this snapshot were closed by the ranges/step work; `..=` and descending+`step` now run on both engines.") and a matching note at `:271`. Keep the historical snapshot intact. |

All five are documentation-only; zero behavior impact.

## §2 — Stale code comments (verified stale)

| # | Location | Stale text | Current reality (evidence) | Fix |
|---|---|---|---|---|
| 2a | `src/syntax/mod.rs:4` | "does not yet drive the binary." | The CST front-end **is** the production front-end: `src/compile/mod.rs` consumes `crate::syntax::{parse_to_tree, resolve, ast::*}` (`:13–25`). | Reword: the CST front-end drives compilation/the default VM engine; the legacy `lexer`/`parser`/`ast` front-end now backs only the `--tree-walker` reference oracle. |
| 2b | `src/syntax/kind.rs:11` | "// --- nodes (only Root for now; the parser plan adds the rest) ---" | The enum declares the full node set (`SourceFile`, statements, expressions, …) immediately below. | Reword the section comment to "core node kinds" (drop "only Root for now"). |
| 2c | `src/gc.rs:16` (the "### Phasing" block, V13-T1 bullet) | "V13-T1 (this task): … **WITHOUT** migrating any `Rc` to `Cc` … not yet wired into a `Cc`-backed graph — they become load-bearing in T2." | Migration is done: `src/value.rs` uses `Cc<…>` for the cycle-capable variants (`Closure`/`Array`/`Object`/`Map`/`Set`/`Instance`, `:370–393`), and collection runs end-of-program (`src/gc.rs:444,793`). | Reword the phasing block to past tense — T1/T2/T3 complete; the `Trace` impls are load-bearing (the collector calls them). Keep the "what is traced vs acyclic" invariant text (still accurate). |
| 2d | `src/vm/value_ext.rs:5` | "These live here for now; V4/V5 fold `Closure` into `Value` once CALL/RETURN and upvalue capture are wired." | `Value::Closure(Cc<…>)` already exists (`src/value.rs:370`); CALL/RETURN + upvalue capture are wired. | Reword to reflect that `Closure` is now a `Value` variant; `value_ext` retains it plus the VM-only status enums (`RunOutcome`/`FiberState`) as the VM's run-loop types. |
| 2e | `src/compile/mod.rs:1499–1500` | "Superclass (`extends`), `super`, and `instanceof` are V9-T2: a class with an `extends` clause is **deferred here** with a clear error." | `extends`/`super` are implemented (the compiler handles `extends` at `:1511`+; `super.<name>` at `:3705`). `instanceof` is NOT implemented (reserved for SP2 — see §6). | Reword: `extends`/`super` are implemented; the lingering `instanceof` reservation is owned by SP2 (do NOT claim it shipped). |
| 2f | `src/compile/mod.rs:1820–1825` | "V12-T1 handles **stdlib** imports only … (file module) source is a documented deferral to V12-T4 — a `CompileError` here." | File (`./…`) imports compile (`:5221–5223` notes the V12-T1 deferral was lifted; `compile_export` at `:1916`). | Reword the docstring to reflect that file/relative imports + `export` now compile (drop the "deferral to V12-T4 — a CompileError here" framing). |

A repo-wide grep for `not yet`, `for now`, `V12-T`, `V9-T`, `deferred here` was run; the additional
non-stale hits (e.g. `:800` "offset is not yet known while the body compiles", `:2548` "is not yet
known: push the ctx", `:56` describing `super` lexing) are **accurate descriptions of in-flight
compile state, not shipped-feature markers** — they are explicitly OUT of scope and must NOT be
touched. The `:2046/:2054` (`fn*` methods) and `:3605` (`a?.m()`) rejections are SP1's territory
(SP1 lifts them); SP7 leaves them alone.

All §2 edits are comment-only; zero behavior impact.

## §3 — Dead defensive match arms (verified — REWORD, do NOT delete)

Every listed catch-all matches over a wide enum (`SyntaxKind`, which is the full token/node set) or
over `Option<Resolution>` (4 variants, 3 handled). **Each is REQUIRED for exhaustiveness — deleting
it makes the match non-exhaustive and fails to compile.** The fix is uniform: change the message from
a stale "not yet supported in V1/V2/V4" (which implies an unfinished feature) to a clear
**internal-invariant** message (these arms are unreachable on any parser-accepted input, so reaching
one is a compiler bug). The guard stays; only the string changes.

| Location | Current message | Why unreachable on valid input | Action |
|---|---|---|---|
| `src/compile/mod.rs:1118` | "assignment to a non-local target not yet supported (V4)" | A NameRef assignment target always resolves to `Local`/`Upvalue`/`Global` (an *undeclared* target resolves to `Global` and the runtime `SET_GLOBAL` emits "cannot assign to undefined variable" — verified identical on both engines). The arm only catches `Some(Resolution::Unresolved)`/`None`, neither of which a resolved assignment NameRef produces. | Reword to an internal-invariant message (e.g. "internal: assignment target resolved to an unexpected binding (compiler invariant)"). Keep the arm. |
| `src/compile/mod.rs:1209` | "statement kind not yet supported in V2" | Every `Stmt` variant is handled except `ExprStmt`, and **all three callers intercept `ExprStmt` before delegating** (`compile_source:730`, `compile_block:3554`, and `compile_export:1924` only passes declaration statements per the grammar). So a `Stmt` reaching the catch-all is an internal invariant violation. | Reword to internal-invariant; keep the arm (it covers `ExprStmt` + any future variant for exhaustiveness — the enum match has no `_` otherwise). |
| `src/compile/mod.rs:3258` | "unsupported match pattern kind {other:?}" | Matches over `SyntaxKind`; all real pattern kinds (`WildcardPat`/`LiteralPat`/`RangePat`/`ArrayPat`/`ObjectPat`) handled. | **Already an internal-invariant-style message** — verify wording is consistent ("internal:" prefix) and leave the arm. Optionally normalize the prefix to match the others. |
| `src/compile/mod.rs:3838` | "binary operator {other:?} not yet supported in V2" | Matches over `SyntaxKind`; all arithmetic/comparison ops handled, and `&&`/`||`/`??` are routed through the short-circuit path above. A binary node carrying any other operator token is unreachable from the parser. | Reword to internal-invariant; keep the arm (required — `SyntaxKind` is huge). |
| `src/compile/mod.rs:4124` | "unary operator {other:?} not yet supported in V1" | Matches over `SyntaxKind`; `Minus`/`Bang` are the only unary operators the grammar produces. | Reword to internal-invariant; keep the arm. |
| `src/compile/mod.rs:4396` | "literal token {other:?} not yet supported in V1" | Matches over `SyntaxKind`; `Number`/`Str`/`TrueKw`/`FalseKw`/`NilKw` are the only literal tokens. | Reword to internal-invariant; keep the arm. |

**No arm is deleted.** All matches remain exhaustive and the public-facing behavior (the diagnostic
on genuinely malformed input, which the parser never produces) is unchanged in *kind* (still a
`CompileError`); only the message text improves. This is behavior-neutral for every valid program.

## §4 — Close #147 (held un-awaited future end-of-program drain)

The opt-out comment at `tests/vm_differential.rs:~2955` claims a future *held* in a local until
program end "interacts with end-of-program task draining and the two engines legitimately differ
there (the tree-walker's end-of-program drain runs the still-held task; the VM does not)." **This is
stale.** Both test entry points drive `local.run_until(...).await; local.await;` (`src/lib.rs`:
`run_source` at `:122–124`, `vm_run_source_with` at `:391–393`), so a held future's body that
`await`s and then prints runs on **both** engines at the drain. Verified with a probe over all three
entry points:

```
src = "async fn work() { await 0\n print(\"worked\") }\nlet f = work()\nprint(\"main\")\n"
tree-walker = "main\nworked\n"   specialized-VM = "main\nworked\n"   generic-VM = "main\nworked\n"
```

**Fix:** (a) correct/remove the stale "legitimately differ" sentences in the opt-out comment so it no
longer asserts a divergence that does not exist; (b) add a differential test
(`vm_held_future_drains_identically_to_treewalker`, byte-identical across all three engines) asserting
the held-future-with-await-then-print case; (c) mark task #147 resolved in the gap register / roadmap.
This is behavior-neutral: it adds a passing assertion and corrects documentation; it does not change
any engine. (Note: the *bare* un-awaited case — cancel-on-drop — is unchanged and still asserted by the
neighboring `vm_unawaited_async_call_is_cancelled_like_treewalker`.)

## §5 — `cargo fmt` the Rust tree (isolated commit)

`cargo fmt --check` reports pre-existing drift (the historical gates were test + clippy, never fmt) —
e.g. `build.rs:23,52`. SP7 runs `cargo fmt` once and commits the **formatting-only** result as its
own isolated commit. After the reflow, the full suite + clippy (both feature configs) must stay green
(formatting cannot change behavior). Keeping it isolated prevents the broad whitespace diff from
obscuring the targeted §1–§4 / §6 edits.

## §6 — Document known/accepted SP1 items (CLAUDE.md + spec)

These are accepted trade-offs from the SP1 holistic review with no home yet; SP7 records them so they
are not mistaken for bugs later. Documentation-only.

- **1-column caret-span offset (cosmetic, accepted).** Error diagnostics under the CST front-end can
  differ by one column in the caret position vs the legacy front-end. The error *message* is correct;
  only the caret column can be off by one. Accepted, cosmetic. Record in `CLAUDE.md` "Current
  deferrals" and add a note to the SP1 design spec.
- **Perf regression ~2.9× → ~2.5× geomean (accepted trade).** Routing top-level vars through
  `GET_GLOBAL` to get tree-walker-parity late-binding cost some geomean speedup; still **≥2×**, which
  meets the perf gate. Note that SP8 may recover it. Record in `CLAUDE.md` + the SP1 spec.
- **`Op::InstanceOf` is reserved for SP2, NOT dead-reservable (coordination).** The opcode exists
  (`src/vm/opcode.rs:290`) and is currently **not emitted** by `src/compile/mod.rs`/`src/vm/run.rs`,
  but it is **owned by SP2** (`docs/superpowers/specs/2026-06-04-sp2-language-features-design.md` §1
  reuses it for the `instanceof` operator). SP7 must **not** remove or "clean up" the reservation, and
  the §2e comment reword must say "reserved for SP2's `instanceof`", not "dead — remove". Record the
  coordination so a future cleanup pass does not delete it.

---

## Behavior-neutrality summary

| Group | Touches | Behavior change? |
|---|---|---|
| §1 docs | Markdown only | No |
| §2 comments | Rust comments only | No |
| §3 dead arms | `CompileError` message strings on unreachable arms | No (valid programs never hit them) |
| §4 #147 | one new passing test + comment correction | No (engines already agree) |
| §5 cargo fmt | whitespace/layout | No |
| §6 known items | CLAUDE.md + SP1 spec text | No |

## File-touch map (for the plan)

| Area | Files |
|---|---|
| Docs (§1) | `docs/content/stdlib/net.md`, `docs/superpowers/roadmap.md`, `docs/superpowers/specs/2026-05-29-ascript-design.md`, `docs/superpowers/specs/2026-06-03-ranges-step-analyzer-design.md` |
| Comments (§2) | `src/syntax/mod.rs`, `src/syntax/kind.rs`, `src/gc.rs`, `src/vm/value_ext.rs`, `src/compile/mod.rs` |
| Dead arms (§3) | `src/compile/mod.rs` (`:1118`, `:1209`, `:3258`, `:3838`, `:4124`, `:4396`) |
| #147 (§4) | `tests/vm_differential.rs`, `docs/superpowers/roadmap.md` (gap register / #147 status) |
| cargo fmt (§5) | whole Rust tree (isolated commit) |
| Known items (§6) | `CLAUDE.md`, `docs/superpowers/specs/2026-06-04-sp1-engine-parity-class-model-design.md` |
