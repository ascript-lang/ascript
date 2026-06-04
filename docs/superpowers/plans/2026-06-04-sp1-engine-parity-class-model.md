# SP1 — Engine-parity & class-model completion — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every place the bytecode VM (default engine) rejects/diverges on parser-accepted code, and add static methods to the class model, so all code the front-end accepts runs byte-identical on both engines.

**Architecture:** Five phases (A–E) plus a closing invariant+docs phase (F). Each phase is TDD, ends green on both feature configs + clippy + the whole-corpus three-way differential, and gets an independent review before the next. The tree-walker is the byte-identical oracle (`ascript run --tree-walker`); never weaken it.

**Tech Stack:** Rust. CST front-end → resolver (`src/syntax/resolve`) → compiler (`src/compile/mod.rs`) → `Chunk` → VM (`src/vm/*`). Legacy front-end → tree-walker (`src/interp.rs`). Grammar: ungrammar + hand CST parser + tree-sitter (regen `tree-sitter generate --abi 14`). `.aso` versioned bytecode (`src/vm/aso.rs`, currently v6).

**Spec:** `docs/superpowers/specs/2026-06-04-sp1-engine-parity-class-model-design.md`.

**Branch:** `feat/sp1-engine-parity` (already created; spec committed at 83e8fa4).

---

## Conventions for every task

- **Differential test harness:** `tests/vm_differential.rs` exposes helpers comparing `ascript::vm_run_source(src)` (specialized VM), `ascript::vm_run_source_generic(src)` (generic VM), and `ascript::run_source_exit(src)` (tree-walker). Add new cases with the existing per-snippet pattern (read a few neighbors first). "Byte-identical" = identical stdout + exit on all three.
- **Per-engine manual smoke:** `cargo build` then `target/debug/ascript run X.as` (VM) vs `target/debug/ascript run --tree-walker X.as`.
- **Gate after each phase (paste tails):** `cargo test --test vm_differential 2>&1 | tail`; `cargo test 2>&1` (0 failures all binaries); `cargo test --no-default-features 2>&1` (0 failures); `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` (clean); `grep await_holding_refcell_ref Cargo.toml` (still `deny`).
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Never** edit a passing tree-walker test or weaken a differential assertion to make the VM pass. A divergence on valid code = fix the VM/compiler root cause.

---

## Phase A — `a?.m(args)` optional method call (VM catches up to the tree-walker)

**Files:** Modify `src/compile/mod.rs` (the `Call`/`eval_chain` lowering; remove the rejection at ~`:3605`). Test `tests/vm_differential.rs`. No tree-walker change (it already runs `a?.m()`). No new opcode expected — reuse the existing `OptMember` member-read short-circuit machinery.

### Task A1: failing differential tests for optional calls

- [ ] **Step 1 — Write failing tests.** Add to `tests/vm_differential.rs` (match the neighboring snippet-test style), each asserting VM (spec+generic) == tree-walker, byte-identical:

```rust
// nil receiver: result nil AND the argument's side effect must NOT run.
diff_case("opt_call_nil_skips_args",
    "fn se() { print(\"ARG\")\n  return 1 }\nlet a = nil\nprint(a?.m(se()))\n");
// non-nil receiver: ordinary bound call.
diff_case("opt_call_nonnil",
    "class C { fn m(x) { return x + 1 } }\nlet c = C()\nprint(c?.m(10))\n");
// chained: whole postfix chain short-circuits when receiver is nil.
diff_case("opt_call_chain_nil",
    "let a = nil\nprint(a?.m().n().o)\n");
// non-nil receiver, missing method -> both panic "value is not callable", identical span.
diff_case("opt_call_missing_method",
    "class C {}\nlet c = C()\nprint(c?.nope(1))\n");
// mixed optional member + optional call in one chain.
diff_case("opt_call_mixed",
    "class C { fn m() { return 5 } }\nlet c = C()\nprint(c?.m())\nlet a = nil\nprint(a?.b?.m())\n");
```
(Use the exact helper name the file already uses — read it; `diff_case` is illustrative.)

- [ ] **Step 2 — Run, verify the VM ones fail** (compile rejection): `cargo test --test vm_differential opt_call 2>&1 | tail -20` → expect failures citing `optional method calls (a?.m(...)) not yet supported (V9)`.

### Task A2: implement optional-call lowering in the compiler

- [ ] **Step 3 — Read the current code.** In `src/compile/mod.rs`: the `Call` arm of `compile_expr`/`eval_chain`, how `Member`-callee method calls are compiled (the `CALL_METHOD` path), and how `OptMember` member *reads* short-circuit (find the existing optional-chain jump/short-circuit handling). The rejection is the `OptMember`-callee branch (~`:3605`).

- [ ] **Step 4 — Implement.** When the call's callee is `OptMember { object, name }`: compile `object`; emit the same nil-test + short-circuit jump used by optional member reads, BUT the success path binds the method and performs the call, and the short-circuit path pushes `nil` and jumps to the **end of the enclosing postfix chain** (so subsequent `.`/`[]`/`()` are skipped). Critically: **args are compiled on the success path only** (after the nil test), so a nil receiver never evaluates them. Reuse the chain's short-circuit sink target the optional-member machinery already establishes. Remove the rejection.

- [ ] **Step 5 — Run the tests:** `cargo test --test vm_differential opt_call 2>&1 | tail -20` → all PASS (byte-identical).

- [ ] **Step 6 — Phase-A gate** (run the full gate set from Conventions) + manual smoke on the 5 programs above (VM vs `--tree-walker` identical).

- [ ] **Step 7 — Commit:** `feat(vm): a?.m(args) optional method call — VM matches tree-walker (nil short-circuit, args unevaluated)`.

---

## Phase B — Generator methods `fn*` / `async fn*` (both engines)

**Files:** Modify `src/interp.rs` (method dispatch → generator path), `src/compile/mod.rs` (lift rejection ~`:2054`, compile method body as generator proto), `src/vm/run.rs` (generator-method dispatch binds `self`), `src/syntax/resolve/*` (generator-method frame = method frame + generator body). Test `tests/vm_differential.rs`.

### Task B1: failing differential tests

- [ ] **Step 1 — Write failing tests** (both engines reject today, so these fail on VM *and* tree-walker — that's expected; they pass once both support it):

```rust
diff_case("gen_method_basic",
    "class C { fn* g() { yield 1\n yield 2\n yield 3 } }\nlet c = C()\nfor await (v in c.g()) { print(v) }\n");
diff_case("gen_method_self",
    "class C { fn init() { self.n = 10 }\n fn* g() { yield self.n\n yield self.n + 1 } }\nlet c = C()\nlet it = c.g()\nprint(it.next())\nprint(it.next())\n");
diff_case("async_gen_method",
    "class C { async fn* g() { yield 1\n let x = await 2\n yield x } }\nlet c = C()\nfor await (v in c.g()) { print(v) }\n");
diff_case("gen_method_inherited_override",
    "class A { fn* g() { yield 1 } }\nclass B extends A { fn* g() { yield 2\n yield 3 } }\nfor await (v in B().g()) { print(v) }\n");
```
(Confirm the exact generator-consumption syntax against `examples/generators.as` — use what the corpus uses.)

- [ ] **Step 2 — Run, verify fail:** `cargo test --test vm_differential gen_method async_gen 2>&1 | tail`.

### Task B2: tree-walker generator-method dispatch

- [ ] **Step 3 — Read** how standalone `fn*`/`async fn*` build a `coro::GeneratorHandle` in `src/interp.rs`, and how a class method is dispatched (`call_method`/`run_body`). Find where a `fn*` method currently misfires `'yield' outside of a generator`.
- [ ] **Step 4 — Implement** tree-walker: when dispatching a method whose decl is `fn*`/`async fn*`, build a generator handle bound to `self` (same path as standalone, with `self` in scope), instead of running the body eagerly.
- [ ] **Step 5 — Run:** the tree-walker side of the tests now behaves; (VM still fails). `cargo test --test vm_differential gen_method 2>&1 | tail`.

### Task B3: VM generator-method compile + dispatch

- [ ] **Step 6 — Implement** VM: lift the `compile/mod.rs:2054` rejection; compile the generator method body as a generator `FnProto` (reuse the V8 `MAKE_GENERATOR`/`YIELD` lowering used for standalone `fn*`); ensure method dispatch (`src/vm/run.rs`) binds `self` (slot 0) and produces a `Value::Generator`. Resolver: generator-method frame = method frame (self-slot) + generator-body marking.
- [ ] **Step 7 — `.aso`:** confirm a generator-method proto serializes/round-trips (generator protos already serialize; add a build+run `.aso` assertion in `tests/aso.rs` for a class with a `fn*` method).
- [ ] **Step 8 — Run all Phase-B tests** → byte-identical both engines.
- [ ] **Step 9 — Phase-B gate** + smoke.
- [ ] **Step 10 — Commit:** `feat: generator methods (fn*/async fn*) in classes — both engines`.

---

## Phase C — Static methods (`static fn`/`static async fn`/`static fn*`) — both engines

The largest phase. Build in dependency order: grammar → resolver → value/dispatch → tree-walker → VM → inheritance → `.aso` → fmt → checker.

**Files:** ungrammar grammar + `src/syntax/parser.rs` + `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` (+ regen `parser.c`); `src/syntax/resolve/*`; `src/value.rs` (class-level dispatch generalization — the single sanctioned value.rs change); `src/interp.rs`; `src/compile/mod.rs` + `src/vm/run.rs`; `src/vm/aso.rs` (v6→v7); `src/syntax/format/*`; `src/check/rules/{duplicate_member,super_misuse,...}.rs`. Tests across `vm_differential.rs`, `aso.rs`, `treesitter_conformance.rs`, `cst_format.rs`, `check.rs`.

### Task C1: grammar — `static` member modifier

- [ ] **Step 1 — Failing parser test.** In the CST parser/conformance area, add a program with `class C { static fn make() { return C() } }` and assert it parses with no errors on the CST parser AND tree-sitter. Run → fails (unknown `static`).
- [ ] **Step 2 — Implement grammar.** Add `static` as an optional member modifier before `fn`/`async fn`/`fn*` in the ungrammar class-member rule and the hand CST parser (`src/syntax/parser.rs`); make `static` a CONTEXTUAL/soft keyword (an identifier `static` must still parse where unambiguous — mirror the existing `as`/`step` soft-keyword handling). Update tree-sitter `grammar.js`; regen: `cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14`.
- [ ] **Step 3 — Run** `cargo test --test treesitter_conformance 2>&1 | tail` and `cargo test --test frontend_conformance 2>&1 | tail` → green; the parse test passes.
- [ ] **Step 4 — Commit:** `feat(grammar): static method member modifier (+ parser.c regen)`.

### Task C2: resolver — static method frames

- [ ] **Step 5 — Implement** in `src/syntax/resolve/*`: a `static` method resolves with a method frame that has **no self-slot** (no `self` binding); record it in the class's static-member set distinct from instance methods. A reference to `self` inside a static is unresolved (→ the engines will error / `super-misuse` will flag `super`).
- [ ] **Step 6 — Resolver unit test** asserting a static method's body has no `self` binding and a sibling instance method still does. Run → green.
- [ ] **Step 7 — Commit:** `feat(resolve): static methods get a self-less method frame`.

### Task C3: value model + class-level dispatch generalization

- [ ] **Step 8 — Read** `src/value.rs:408` (`ClassMethod`) and `src/interp.rs:2023-2025` / `:2071-2079` (the `.from` ClassMethod read + call). Add `static_methods: IndexMap<String, Rc<Method>>` to `Class` (`src/value.rs:164` neighbor). Generalize the class-member READ so `C.name` resolves: (1) `static_methods` walking the superclass chain → a bound static callable; (2) else `from` (built-in); (3) else `"class X has no static member 'name'"`. Keep `ClassMethod` as the carrier (change `&'static str` → `Rc<str>` if needed for user names) — this is the ONLY sanctioned value.rs change.
- [ ] **Step 9 — Run** `cargo build` (compiles); existing tests still green (no behavior change yet — no class declares statics). Commit: `feat(value): generalize class-level dispatch for user static methods`.

### Task C4: tree-walker static methods + `from` reservation

- [ ] **Step 10 — Failing tests** (tree-walker side): `diff_case`s — a sync `static fn make()` called `C.make()`; a static calling another static + constructing `C()`; inheritance (`class B extends A`; `B.parentStatic()`); `static fn from` → compile error. (These fail on both engines until C5; that's fine — write them now.)
- [ ] **Step 11 — Implement** tree-walker: populate `static_methods` from the class decl; dispatch `C.name(args)` as an instance-less call; reject a user `static fn from` with `"'from' is reserved on classes"`.
- [ ] **Step 12 — Commit:** `feat(interp): static method storage + dispatch + 'from' reservation`.

### Task C5: VM static methods + inheritance

- [ ] **Step 13 — Implement** VM: in `src/compile/mod.rs` compile static methods into the class's static proto table (keyed by `Rc::as_ptr(class)` like instance methods); lower `C.name(args)` via the generalized class-member read + call; inheritance lookup mirrors instance-method dispatch (`src/vm/run.rs`). Handle `static async fn` (returns a future) and `static fn*` (returns a generator) by reusing Phase-B/async machinery.
- [ ] **Step 14 — Run** the Task-C4 tests → byte-identical both engines. Add `static async fn create()` factory case: `class C { fn init() { self.x = 0 }\n static async fn create() { let c = C()\n c.x = await 5\n return c } }\nlet c = await C.create()\nprint(c.x)\n`.
- [ ] **Step 15 — Commit:** `feat(vm): static method compile + dispatch + inheritance`.

### Task C6: `.aso` v7 — serialize static methods

- [ ] **Step 16 — Implement** `src/vm/aso.rs`: serialize/deserialize the static-method table; bump `ASO_FORMAT_VERSION` 6→7; verifier validates. Add `tests/aso.rs` build+run round-trip for a class with sync + async static methods (built `.aso` output == tree-walker). Confirm an old v6 `.aso` is rejected with the version-mismatch message.
- [ ] **Step 17 — Commit:** `feat(aso): serialize static methods (format v7)`.

### Task C7: formatter

- [ ] **Step 18 — Failing fmt test** (`tests/cst_format.rs` or inline): a class with `static fn`/`static async fn`/`static fn*` formats canonically and is idempotent. Implement emission in `src/syntax/format/*` (emit the `static` modifier; place statics in the canonical member order — keep fields-before-methods; document where statics sit, e.g. with methods).
- [ ] **Step 19 — Run** + idempotence check on a static-method program. Commit: `feat(fmt): format static methods`.

### Task C8: checker

- [ ] **Step 20 — Failing checker tests** (`tests/check.rs`): `duplicate-member` fires for two `static fn x` (and for two instance `fn x`) but NOT for one static + one instance `x` (separate namespaces); `super-misuse` fires for `super` inside a static method; `static fn from` → a clear diagnostic. Implement in `src/check/rules/*`.
- [ ] **Step 21 — Corpus zero-FP guard:** `ascript check examples/*.as examples/advanced/*.as` → 0 diagnostics.
- [ ] **Step 22 — Phase-C gate** (full gate set) + add a corpus example `examples/static_methods.as` (sync + async-factory + inheritance, deterministic, ends `print("static_methods ok")`); ensure it parses on all parsers + is byte-identical VM/tree-walker + builds+runs as `.aso`.
- [ ] **Step 23 — Commit:** `feat(check): static-method-aware duplicate-member/super-misuse + corpus example`.

---

## Phase D — `async fn init` forbidden (both engines)

**Files:** `src/compile/mod.rs` (VM reject), `src/interp.rs` (tree-walker reject). Test `tests/vm_differential.rs`.

### Task D1: reject async init identically

- [x] **Step 1 — Failing test:** `diff_case("async_init_rejected", "class C { async fn init() { self.x = 1 } }\nlet c = C()\nprint(c.x)\n")` — assert BOTH engines error with identical message + exit. (Today they diverge: VM runs it, tree-walker leaves `x` nil.)
- [x] **Step 2 — Run, verify divergence** (test fails because engines differ).
- [x] **Step 3 — Implement** both engines: a class whose `init` is `async` (or `fn*`) → compile/resolve error `"init must be a synchronous constructor; use a static async factory (e.g. \`static async fn create()\`)"`. Runtime-timed is unnecessary — this is a structural error knowable at compile/resolve; ensure BOTH engines emit it at the same point with the same message (a compile-time error in both is fine and symmetric).
- [x] **Step 4 — Run** → both reject identically.
- [x] **Step 5 — Phase-D gate + commit:** `feat: forbid async fn init on both engines (use a static async factory)`.

---

## Phase E — Field-default completeness + arrow/match `.aso`

**Files:** `src/compile/mod.rs` (`cst_default_expr` ~`:218`+, edge forms at `:229/254/298`), `src/vm/aso.rs` (arrow/match default serialization). Test `tests/vm_differential.rs`, `tests/aso.rs`.

### Task E1: audit + close remaining field-default forms

- [ ] **Step 1 — Audit:** enumerate every default-expression form the tree-walker accepts as a field default that `cst_default_expr` still rejects (the `:229/254/298` error sites). For each, write a `diff_case` (`class C { x: T = <form> }` via `C()` and `C.from({})`) and confirm it currently fails on the VM but works on the tree-walker.
- [ ] **Step 2 — Implement** the missing lowerings in `cst_default_expr` (match the existing arms' style). Any form that stays rejected MUST be symmetric (tree-walker rejects too) — verify `..=` and `yield` defaults still error on both (keep those rejections; add a test asserting symmetry).
- [ ] **Step 3 — Run** → new forms byte-identical both engines.

### Task E2: `.aso` serialization for arrow/match field defaults

- [ ] **Step 4 — Failing test** (`tests/aso.rs`): a class with an arrow field default and a `match` field default — `ascript build f.as` then `run f.aso` == `ascript run f.as` (tree-walker). Today build rejects (`NonLiteralConst`).
- [ ] **Step 5 — Implement** arrow/match default serialization in `src/vm/aso.rs` (persist enough to reconstruct — if the compiler re-parses arrow/match defaults via the legacy front-end, persist the source text + re-lower on load; reuse the v7 bump from Phase C, do not double-bump).
- [ ] **Step 6 — Run** → build == run for all field-default forms.
- [ ] **Step 7 — Phase-E gate + commit:** `feat: complete field-default lowering + .aso arrow/match default serialization`.

---

## Phase F — Parser-accepts-runs invariant gate + docs + holistic review

**Files:** `tests/vm_differential.rs`, `docs/content/*`, `docs/superpowers/specs/2026-05-29-ascript-design.md`.

### Task F1: invariant gate

- [ ] **Step 1 — Add** `tests/vm_differential.rs::vm_parser_accepts_runs`: a curated set of programs covering each grammar-accepted construct touched by SP1 (optional calls, generator methods, static methods incl. async/`fn*`, computed field defaults), asserting each RUNS (no compile rejection) on the VM and is byte-identical to the tree-walker. Document that the only legitimate compile rejections are genuine symmetric errors.
- [ ] **Step 2 — Run** → green.

### Task F2: docs

- [ ] **Step 3 — Update** `docs/content` (classes page: static methods + `static async fn create()` async-factory + `async fn init` is forbidden; optional method calls `a?.m()`; generator methods) and the language spec class section (`docs/superpowers/specs/2026-05-29-ascript-design.md`). Verify documented snippets against the binary.
- [ ] **Step 4 — Commit:** `docs: static methods, optional method calls, generator methods, async-factory`.

### Task F3: holistic gate + perf

- [ ] **Step 5 — Full gate set** both feature configs + clippy both + `cargo test --release --test vm_bench -- --ignored --nocapture` (geomean ≥2×, no spec-vs-generic regression).
- [ ] **Step 6 — Independent review** (re-read spec, re-run gates, adversarial divergence hunt over the new surface: optional-call chains, static-method inheritance/shadowing, async factory, generator methods with `self`). Fix any divergence at the root.
- [ ] **Step 7 — Final commit** if review surfaced fixes; otherwise the phase is complete.

---

## Self-review (author)

**Spec coverage:** §1 a?.m()→Phase A; §2 generator methods→Phase B; §3 static methods + async factory + async-init reject→Phases C & D; §4 field defaults + .aso→Phase E; §5 invariant gate→Phase F1. All covered.

**Placeholder scan:** No "TBD/handle edge cases". Test programs are concrete AScript; the one place I defer to the implementer is exact Rust signatures/opcode internals (the implementer must read current `compile/mod.rs`/`vm/run.rs` — the spec gives the change sites with line numbers). Differential-test helper name (`diff_case`) is illustrative — the implementer uses the file's actual helper.

**Type consistency:** `static_methods: IndexMap<String, Rc<Method>>` on `Class`; `ClassMethod` carrier generalized once (C3); `ASO_FORMAT_VERSION` bumped exactly once (6→7, in C6; E2 reuses it). `async fn init` message string identical in spec §3 and Phase D. Consistent.
