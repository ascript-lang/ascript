# VM Plan V3 — Control flow: if/else, while, for-range, for-of (sync), break/continue, ternary

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Compile and execute all sync control flow byte-identically to the tree-walker: `if/else if/else`, `while`, `for i in a..b` (range), `for x of iterable` (sync iteration over array/object/string/map/set — `for await` is V7), `break`/`continue`, and the `cond ? a : b` ternary. Jump emission/patching + loop back-edges.

**Architecture:** Compiler emit arms for the control-flow statements using `emit_jump`/`patch_jump`/`LOOP`; a loop context stack tracking break/continue patch sites. VM exec arms for `JUMP`/`JUMP_IF_FALSE`/`JUMP_IF_TRUE`/`LOOP`. No new opcodes beyond V1's jump family (+ maybe `GET_ITER`/`ITER_NEXT` for `for-of` — see Task 4). **Depends on V2.**

---

## Ground truth
- Tree-walker control flow uses `Flow::{Break,Continue,Return}` — `src/interp.rs` `exec`/`exec_block`. The VM realizes the SAME semantics structurally via jumps; `break`/`continue` become jumps to patched targets, NOT a `Flow` value.
- `for i in start..end` is `Stmt::ForRange` (exclusive `..`; inclusive `..=` is match-only per CLAUDE.md — confirm range `for` is exclusive). `for x of e` is `Stmt::ForOf { for_await }`; sync when `for_await=false`. Iteration order/semantics over array/object(keys? entries?)/string/map/set MUST match the tree-walker exactly — read `eval_for_of`.
- Truthiness for `JUMP_IF_FALSE` already defined in V2.

---

## Tasks
- [ ] **T1 — if/else.** `IfStmt`: compile cond; `JUMP_IF_FALSE else`; pop; then-block; `JUMP end`; `else:` pop; else-block (which may be another `IfStmt` for `else if`); `end:`. Mind the cond value left on stack (pop after the branch test — match the DUP/POP discipline from V2's short-circuit, or pop-on-test). Tests: `if`, `if/else`, `else if` chains; differential vs tree-walker. Commit.
- [ ] **T2 — while + break/continue.** Maintain a `loop_ctx` stack: `{ start_offset, break_sites: Vec<usize>, continue_sites: Vec<usize> }`. `WhileStmt`: `start:` compile cond; `JUMP_IF_FALSE end`; body; `LOOP start`; `end:`; patch breaks→end, continues→start. `break`/`continue` emit a `JUMP` recorded in the current loop ctx. Error if `break`/`continue` outside a loop (compile-time; match tree-walker/resolver behavior). Tests incl. nested loops + labeled? (AScript has no labels — confirm). Commit.
- [ ] **T3 — for-range.** `ForRange { var, start, end, body }`: compile to an induction variable in `var`'s slot: init `var=start`; `start:` `var < end` test (exclusive) → `JUMP_IF_FALSE end`; body; `var = var + 1`; `LOOP start`; `end:`. `continue` jumps to the increment (so the step still runs) — match tree-walker semantics for `continue` in a range loop (does it skip the increment? verify and mirror). Tests + differential. Commit.
- [ ] **T4 — for-of (sync).** `ForOf { var, iter, body, for_await:false }`: lower to an iteration protocol. Simplest faithful approach: emit a `GET_ITER` (push an internal iterator over the iterable) + a loop that calls `ITER_NEXT` (push `[value, done]` or push value + `JUMP_IF_DONE`). Implement `GET_ITER`/`ITER_NEXT` as VM ops backed by a small `enum VmIter { Array(idx), Str(char-idx), Object(key-idx), Map(idx), Set(idx) }` matching the tree-walker's iteration order EXACTLY (e.g. object iterates keys? or [k,v]? — read `eval_for_of`). `break`/`continue` integrate with the loop ctx. Tests over array/string/object/map/set + differential. Commit.
- [ ] **T5 — ternary.** `TernaryExpr { cond, then, els }` (expression): `cond`; `JUMP_IF_FALSE else`; pop; `then`; `JUMP end`; `else:` pop; `els`; `end:`. Tests incl. nested + the `a ? -b : c` precedence case. Commit.
- [ ] **T6 — widen differential gate.** Add control-flow-using sync examples to the allow-list in `tests/vm_differential.rs` (e.g. `factorial.as` if it's sync, `pattern_matching.as`'s non-match parts excluded, loops in `numbers.as`). Byte-identical stdout. Full suite + clippy both configs. Commit.

## Done criteria (V3)
- [ ] All sync control flow runs identically to the tree-walker; break/continue/nested loops correct; ternary correct.
- [ ] Differential gate widened; `cargo test` green; clippy clean both configs.

**Next:** V4 — functions/calls: `FnDecl`/`ArrowExpr` → `FnProto`+`CLOSURE`, `CALL argc`/`RETURN`, params/rest, the multi-frame `run` loop, and the load-bearing `call_value` native↔VM bridge.
