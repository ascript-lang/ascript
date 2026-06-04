# SP1 — Engine-parity holes & class-model completion — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover gap program (see the gap register in the session handoff; SP2–SP10 follow).

**Goal:** Close the remaining places where the bytecode VM (now the default engine) rejects or diverges on code the parser/grammar accepts, and complete the class model with the surface needed for synchronous-construction discipline. Every construct the front-end accepts must **run, byte-identical on both engines** (VM and the `--tree-walker` reference engine).

**Architecture:** Five focused changes across the compiler (`src/compile/mod.rs`), VM (`src/vm/{run,opcode,disasm,verify,aso}.rs`), tree-walker (`src/interp.rs`), value model (`src/value.rs`), resolver (`src/syntax/resolve`), grammar (CST parser + ungrammar + tree-sitter), formatter (`src/syntax/format`), and checker (`src/check/rules`). Each is gated by the whole-corpus three-way differential (tree-walker == specialized-VM == generic-VM) staying byte-identical, plus new per-feature differential tests.

**Tech stack:** Rust. CST front-end → resolver → compiler → `Chunk` → VM (default); legacy front-end → tree-walker (reference oracle). gcmodule GC. `.aso` versioned bytecode (currently v6).

---

## Non-goals (explicitly out of SP1)

These belong to SP2 (new language features) and are NOT in scope here: `instanceof` as a language feature, `map` literal syntax, `object.freeze`, default parameters on functions/arrows, auto-`init`/records, sync `for (x in generator)`. `..=` and `yield` as field-default expressions stay rejected (symmetric across engines, by design). The `--tree-walker` legacy-front-end syntax strictness (e.g. requires `if (cond)` parens) is unchanged — it is a debug/oracle engine, not a second dialect.

---

## §1 — `a?.m(args)` optional method call (closes A1)

### Current behavior (verified)
- VM (default engine): **rejected at compile** — `optional method calls (a?.m(...)) not yet supported (V9)` at `src/compile/mod.rs:3605`.
- Tree-walker: **runs it correctly.** The `?.` guards only a **nil receiver**.

### Target semantics (match the tree-walker — the oracle; verified by probe)
For `recv?.m(args)`:
1. Evaluate `recv`. **If `recv` is nil:** the whole call yields `nil`, **arguments are NOT evaluated** (short-circuit before the call), and the short-circuit **propagates through the remainder of the postfix chain** — `a?.m().n().o` is `nil` wholesale when `a` is nil.
2. **If `recv` is non-nil:** compile as an ordinary bound method call `(recv.m)(args)`. `?.` does NOT guard a missing method — `c?.nope(1)` on a non-nil `c` still panics `value is not callable` (because `c.nope` is nil and nil is called). This is identical to the tree-walker.

### Implementation
- Compiler (`src/compile/mod.rs`, the `Call`/`eval_chain` path): when the call's callee is an `OptMember` node (`?.`, distinct from `Member`), emit the optional-call sequence rather than erroring. Reuse the existing `OptMember` member-read short-circuit machinery (the VM already short-circuits `a?.b` member reads); the optional **call** extends that: evaluate receiver → if nil, jump past arg-eval + call to push `nil` and set the chain's short-circuit state so subsequent `.`/`[]`/`()` in the same postfix chain also short-circuit; else bind the method and call normally.
- The short-circuit must compose with existing optional member/index in the same chain (one short-circuit "sink" jump target at the end of the postfix chain, as the tree-walker does).
- Tree-walker: unchanged.
- Remove the `compile/mod.rs:3605` rejection.

### Tests
Differential (`vm_run_source` vs tree-walker, byte-identical): nil receiver (result nil, side-effecting arg NOT evaluated), non-nil receiver (normal call), chained `a?.m().n().o` with nil and non-nil `a`, mixed `a?.b.m()` / `a?.m().k`, non-nil receiver with missing method (both panic `value is not callable`, identical span).

---

## §2 — Generator methods `fn*` / `async fn*` (closes A2)

### Current behavior (verified)
Both engines reject a generator method. VM: `generator methods (fn*) not yet supported in the VM` at `src/compile/mod.rs:2054`. Tree-walker: misleading `'yield' outside of a generator`. The CST parser/grammar/formatter accept the syntax. → This is a feature added to **both** engines (kept in differential lockstep), not a VM-catches-up case.

### Target semantics
A class method declared `fn* g(...)` or `async fn* g(...)` is a generator (resp. async-generator) method. `c.g(args)` returns a generator (`Value::Generator`) bound to the instance `self`, behaving exactly like a standalone `fn*`/`async fn*` does today (`yield`, `.next(v)`, `for await`, `gen.close()`). Method dispatch, `self` binding, arity/contracts, and inheritance/`super` work as for ordinary methods.

### Implementation
- Resolver: a generator/async-generator method gets a method frame with the self-slot (slot 0) like any method, plus the generator body treatment standalone generators already receive.
- Tree-walker (`src/interp.rs`): route a `fn*`/`async fn*` method through the same `coro::GeneratorHandle` path standalone generators use, binding `self`. Remove the "yield outside generator" misfire for the method case.
- VM (`src/compile/mod.rs`): lift the `:2054` rejection; compile the method body as a generator `FnProto` (the `MAKE_GENERATOR`/`YIELD` machinery from V8), bound at dispatch with `self`. No new opcodes expected (reuse the standalone-generator path + method dispatch).
- `.aso`: a generator method proto serializes like any generator proto (already supported); verify round-trip.

### Tests
Differential: `class C { fn* g() { yield 1; yield 2 } }` consumed via `for await` and `.next()`; an `async fn* ag()` with `yield`+`await`; a generator method that uses `self`; inheritance (`super`-less) and a subclass overriding a generator method. Byte-identical both engines + `.aso` round-trip.

---

## §3 — Static methods + async factory; `async fn init` forbidden (closes A3)

### Motivation
AScript has **no user static methods** today — `Value::ClassMethod(Rc<Class>, &'static str)` (`src/value.rs:408`) is hardcoded to the built-in `.from`; `C.anything()` errors *"class X has no static member"* (`src/interp.rs:2023-2025`). Synchronous construction (`C()` returns an instance, not a future) means there is no caller to `await` an `async fn init`. The blessed pattern for async construction is a **static async factory**, which requires general static methods.

### Target semantics
**Static methods** — both engines:
- Syntax in a class body: `static fn name(params) { ... }`, `static async fn name(...)`, `static fn* name(...)` (the `static` soft keyword precedes the method).
- Called as `C.name(args)` — a class-level call with **no `self`** / no instance. The body may reference the class by name (the class is a global value) and call other statics or construct instances (`C(...)`).
- Stored in a new `Class.static_methods: IndexMap<String, Rc<Method>>` — a **separate namespace** from instance `methods`. An instance method and a static method may share a name (called differently: `c.x()` vs `C.x()`).
- **Inherited:** a subclass resolves an unknown static up its superclass chain (consistent with instance-method inheritance).
- `from` is **reserved**: declaring `static fn from` is a compile error (collides with the built-in typed-parse `.from`).
- `super` is not valid inside a static method (no instance/parent receiver) — surfaced by the `super-misuse` checker lint and a runtime/compile error consistent with the engines.

**Async factory + `async fn init` rejection** — both engines:
- `async fn init` (async constructor) → **clean compile error on both engines**, identical message: *"init must be a synchronous constructor; use a static async factory (e.g. `static async fn create()`)"*. Removes the current divergence (VM ran the body; tree-walker left fields nil).
- The blessed async-construction pattern: `static async fn create(...) { let c = C(); await c.load(...); return c }`, invoked `C.create()` returning a `future<C>`. No special-casing of the name `create` — it is just a `static async fn`; `create` is a convention, not a keyword.

### Implementation
- **Value model (`src/value.rs`):** generalize class-level dispatch. Either (a) extend `Value::ClassMethod(Rc<Class>, Rc<str>)` to carry an owned name and resolve user statics + the built-in `from`, or (b) add a `Value::StaticMethod(Rc<Class>, Rc<Method>)` bound value. Recommendation: keep the `ClassMethod` access path (`C.name` read) but, on read, look up `name` in `static_methods` (walking the superclass chain) → a callable bound value; fall back to `from`; else *"class X has no static member 'name'"*. This is the single sanctioned value.rs change for SP1.
- **Grammar:** add `static` to the class-member rule in the ungrammar grammar + the CST parser + the tree-sitter grammar (regen `parser.c --abi 14`). `static` is a soft/contextual keyword (a field/method/var named `static` must still be possible where unambiguous — verify against the existing soft-keyword handling for `as`/`step`).
- **Resolver:** a static method is a method frame WITHOUT a self-slot (no `self` binding); record it under the class's static namespace.
- **Tree-walker (`src/interp.rs`):** store + dispatch static methods; `C.name(args)` resolves a static (instance-less) call; reject `async fn init`.
- **VM (`src/compile/mod.rs`, `src/vm/run.rs`):** compile static methods to protos in the class's static table (keyed like instance methods, by `Rc::as_ptr(class)`); `C.name(args)` lowers to the class-member read + call (reusing the `.from` ClassMethod path, generalized); reject `async fn init`. Inheritance lookup mirrors instance-method dispatch.
- **`.aso` (`src/vm/aso.rs`):** serialize the static-method table; bump `ASO_FORMAT_VERSION` (v6 → v7); verifier validates.
- **Formatter (`src/syntax/format`):** emit `static fn` / `static async fn` / `static fn*`; place statics consistently in the canonical member order (decide: statics grouped with methods, after fields — match the existing fields-before-methods rule, with a documented sub-order for statics).
- **Checker (`src/check/rules`):** `duplicate-member` treats static vs instance as separate namespaces (duplicate only fires within one namespace); `super-misuse` fires for `super` in a static; `call-arity`/`field-default-type`/`unknown-enum-variant` unaffected; the new `static fn from` reservation gets a clear diagnostic. Zero-false-positive corpus guard holds.

### Tests
Differential + checker tests, byte-identical both engines: a sync `static fn`, a `static async fn create()` factory (`C.create()` awaited → configured instance), a `static fn*`, static calling another static + constructing `C()`, inheritance (subclass calls parent static), `static fn from` rejected, `async fn init` rejected (identical message + exit both engines), instance + static same-name coexistence, `.aso` round-trip of static methods. New corpus example exercising static methods + the async factory.

---

## §4 — Field-default completeness + `.aso` serialization (closes A6 + arrow/match build gap)

### Current behavior
Computed field defaults (binary/index/ternary/template/call) run on both engines (the `cst_default_expr` completeness work shipped). Remaining gaps: (a) a few edge default-expression forms still error in `cst_default_expr` (`src/compile/mod.rs:229/254/298` — e.g. certain literal/operator forms); (b) **arrow and `match` field defaults run via `ascript run` but `ascript build` rejects them** (`.aso` serialization `NonLiteralConst`) — a build-only divergence from run.

### Target
- Audit `cst_default_expr` for every default-expression form the tree-walker accepts as a field default; lower each so construct and `.from` match the tree-walker. Any form that stays rejected must be **symmetric** (the tree-walker rejects it too) and documented — `..=` and `yield` defaults remain rejected by design.
- Make `.aso` serialization round-trip arrow/match field defaults (whatever representation the compiler uses for them — today they re-parse via the legacy front-end at compile; the `.aso` must persist enough to reconstruct, OR persist the source text to re-parse on load) so `ascript build f.as && ascript run f.aso` == `ascript run f.as` for every field-default form. Bump `.aso` version if the layout changes (coordinate with §3's bump — one version bump for SP1).

### Tests
Differential + build+VM+assert (`tests/aso.rs`): every supported field-default form via `C()` and `C.from({})`, plus an arrow default and a `match` default built to `.aso` and run, byte-identical to the tree-walker. Confirm `..=`/`yield` defaults still error symmetrically.

---

## §5 — Parser-accepts-runs invariant gate

Add a differential test (`tests/vm_differential.rs`) that asserts the invariant SP1 establishes: a curated set of "every grammar-accepted construct" programs all **run** (no compile rejection) on the VM and are byte-identical to the tree-walker. After SP1, the only legitimate compile-time rejections are genuine errors (symmetric across engines) — there must be no "parser accepts but engine rejects valid code" cases. The audit found exactly two such holes (`a?.m()`, `fn*` methods); §1–§3 close them. The gate prevents regressions and new holes.

---

## Testing & quality bar (whole sub-project)

- **Differential oracle never relaxed:** whole-corpus three-way (tree-walker == specialized-VM == generic-VM) byte-identical, plus recorded goldens, plus the new per-feature tests. Any divergence on valid code = fix the root cause, never weaken the assertion or edit a tree-walker test to match the VM.
- **Both feature configs:** `cargo test` green default AND `--no-default-features`.
- **Clippy clean** under `--all-targets` AND `--no-default-features --all-targets`; `await_holding_refcell_ref` stays denied + clean.
- **Perf gate:** geomean ≥2× compute-bound, no spec-vs-generic regression (`tests/vm_bench.rs`).
- **Grammar:** regen `parser.c --abi 14` after the `static` grammar change; `treesitter_conformance` + `frontend_conformance` green.
- **value.rs discipline:** the only sanctioned value.rs change is the class-level dispatch generalization in §3.
- **Per-task commit** with trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Independent per-phase review (re-read spec, re-run gates, adversarial divergence hunt) before sign-off.
- **Docs:** update `docs/content` (classes: static methods + async factory; optional method calls; generator methods) and the language spec (`docs/superpowers/specs/2026-05-29-ascript-design.md`) class section.

## File-touch map (for the plan)

| Area | Files |
|---|---|
| Compiler | `src/compile/mod.rs` (optional-call, generator methods, statics, `cst_default_expr`) |
| VM | `src/vm/{run,opcode,disasm,verify,aso}.rs` (static dispatch, generator-method protos, `.aso` v7) |
| Tree-walker | `src/interp.rs` (optional-call already works; generator methods, static methods, `async fn init` reject) |
| Value | `src/value.rs` (class-level dispatch generalization) |
| Resolver | `src/syntax/resolve/*` (static method frames, no-self) |
| Grammar | ungrammar grammar + CST parser + `docs/.../tree-sitter-ascript/grammar.js` + `parser.c` |
| Formatter | `src/syntax/format/*` (`static fn` emission + ordering) |
| Checker | `src/check/rules/*` (duplicate-member namespaces, super-misuse, `static fn from` reservation) |
| Tests | `tests/vm_differential.rs`, `tests/aso.rs`, `tests/treesitter_conformance.rs`, examples |
| Docs | `docs/content/*`, language spec class section |

## Known/accepted after implementation (SP7 record)

Recorded by SP7 (`docs/superpowers/specs/2026-06-04-sp7-docs-cleanup-design.md`) so these accepted trade-offs are not later mistaken for bugs:

- **1-column caret-span offset (cosmetic, accepted).** Error diagnostics under the CST front-end can differ by one column in the caret position vs the legacy front-end. The error *message* is always correct; only the caret column can be off by one.
- **Perf trade ~2.9x -> ~2.5x geomean (accepted).** Routing top-level vars through `GET_GLOBAL` for tree-walker-parity late-binding cost some geomean speedup; still >=2x (meets the perf gate). SP8 (performance) may recover it.
- **`Op::InstanceOf` reserved for SP2.** The opcode is declared (`src/vm/opcode.rs:290`) but not yet emitted; SP2 reuses it for the `instanceof` operator. Do NOT remove it as dead code.
