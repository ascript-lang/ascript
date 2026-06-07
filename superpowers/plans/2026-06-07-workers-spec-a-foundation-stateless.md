# Workers Spec A — Foundation & Stateless Workers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `worker` contextual keyword (`worker fn` / `static worker fn`) that dispatches a function's body to a shared-nothing isolate on another OS thread and returns a `future<T>`, achieving multi-core parallelism without making the `!Send` runtime `Send` — via a structured-clone `Value` serializer, a lazy demand-grown isolate pool, and bytecode code-shipping.

**Architecture:** `worker` is a flag (`is_worker`) on the function-decl node, parsed in BOTH front-ends exactly where `async`/`static` are (contextual soft-keyword, never reserved), and threaded through `FnProto`/`Value::Function`/`MethodDecl`/`Method` and `.aso`. At runtime, calling a `worker fn` builds a structured-clone byte payload of its args + a code slice (compiled `.aso` of the fn + its transitive top-level dependency closure), ships it over a `Send` byte channel to a pooled isolate (a complete fresh `Interp`/`Vm` on its own `WORKER_STACK_SIZE` worker thread), and returns a `Value::Future` whose awaiting future stays on the caller thread — only bytes cross. The serializer (`src/worker/serialize.rs`) and the sendability gate run at the `Value` layer, so both engines are byte-identical.

**Tech Stack:** Rust, tokio (`std::thread` worker threads + per-isolate current-thread runtime + `LocalSet`, `mpsc`/`oneshot` `Send` channels), `num_cpus`, the existing `.aso` serializer (`src/vm/aso.rs`), tree-sitter (`--abi 14` regen), the `gcmodule` `Cc` value model (unchanged).

---

## Shared API Contract (Plan B builds on these — pinned to current code)

**Existing names this plan threads through (verified against the tree):**
- AST: `crate::ast::Stmt::Fn { name, params, ret, body, is_async, is_generator, span, name_span }` (`src/ast.rs:300`) and `crate::ast::MethodDecl { …, is_async, is_generator, is_static, … }` (`src/ast.rs:347`).
- Value: `crate::value::Function { …, is_async, is_generator }` (`src/value.rs:594`) and `crate::value::Method { …, is_async, is_generator }` (`src/value.rs:285`).
- Bytecode: `crate::vm::chunk::FnProto { chunk, arity, has_rest, is_async, is_generator, params, ret }` (`src/vm/chunk.rs:335`); serialized by `write_proto`/`read_proto` (`src/vm/aso.rs:728`) with a flags byte (`has_rest | is_async<<1 | is_generator<<2`); version `ASO_FORMAT_VERSION` (`src/vm/aso.rs:96`, currently `15`).
- SyntaxKind keywords: `AsyncKw` / `StaticKw` / `FnKw` / `Star` (`src/syntax/kind.rs`). Contextual remap via `Parser::bump_remap(SyntaxKind)` + `Parser::at_kw(&str)` (`src/syntax/parser.rs:119,152`); shared `is_static_method` (`src/syntax/resolve/mod.rs:34`).
- Async future handle: `crate::task::SharedFuture` (`src/task.rs`) — `new()`, `resolved(Result<Value,Control>)`, `cell() -> ResultCell`, `set_abort(AbortHandle)`, `get().await`. Wrapped as `Value::Future(SharedFuture)` (`src/value.rs:648`).
- Interp: `crate::interp::Interp` (`src/interp.rs:427`), `Interp::new()`/`new_live()`, `rc() -> Rc<Interp>` (`src/interp.rs:1005`), `WORKER_STACK_SIZE` (`src/interp.rs:622`), `run_on_worker_stack` (`src/lib.rs:52`).
- Checker: rules registered in `crate::check::rules::ALL` (`src/check/rules/mod.rs:29`); `Rule = fn(&ResolvedNode, &ResolveResult, &str) -> Vec<AsDiagnostic>`. Infer async→future at `pass.rs:1077` (`CheckTy::Future(Box::new(ret))`), `is_async`/`is_generator` helpers at `pass.rs:1352/1360`.

**New names this plan introduces (Plan B depends on these — do not rename):**
- `SyntaxKind::WorkerKw` (new keyword kind; remapped, not lexed).
- AST: new field `is_worker: bool` on `Stmt::Fn`, `MethodDecl`, `value::Function`, `value::Method`, and `FnProto`.
- New module tree `src/worker/` with `mod.rs`, `serialize.rs`, `pool.rs`, `isolate.rs`, `dispatch.rs`.
- Serializer public API (in `src/worker/serialize.rs`):
  - `pub fn encode(&Value) -> Result<Vec<u8>, SendError>`
  - `pub fn decode(&[u8], &Interp) -> Result<Value, SendError>`
  - `pub fn check_sendable(&Value) -> Result<(), SendError>`
  - `pub struct SendError { pub kind: &'static str, pub path: String, pub hint: Option<&'static str> }` with `SendError::message(&self) -> String` producing `value of kind <kind> cannot be sent to a worker at <path>` (+ the channel/emitter hint when present). `SendError` converts into a recoverable Tier-2 `AsError`/`Control::Panic`.
- Pool/dispatch public API (in `src/worker/mod.rs`):
  - `pub fn dispatch_worker(interp: &Interp, slice: WorkerCodeSlice, args: Vec<Value>, span: Span) -> Result<Value, Control>` — returns a `Value::Future`.
  - `pub struct WorkerCodeSlice { pub fn_id: u64, pub entry_aso: Rc<[u8]>, pub class_name: Option<Rc<str>> }` (the shipped bytecode payload identity).
  - `pub fn pool_is_initialized() -> bool` (test hook for the lazy-pool proof).
- Checker rule id string: **`"worker-capture"`** (default **Error**), in `src/check/rules/worker_capture.rs`.

---

## File Structure

**New files:**
- `src/worker/mod.rs` — worker subsystem entry: `dispatch_worker`, `WorkerCodeSlice`, `pool_is_initialized`, re-exports `serialize`. One responsibility: the script-facing dispatch API + module wiring.
- `src/worker/serialize.rs` — the structured-clone `Value` serializer + sendability gate (`encode`/`decode`/`check_sendable`/`SendError`). One responsibility: the airlock — turn a `Value` into bytes and back, with cycles + class reconstruction + field-path rejection.
- `src/worker/pool.rs` — the lazy, demand-grown isolate pool + FIFO work queue + backpressure + inline-nesting decision. One responsibility: isolate lifecycle & scheduling.
- `src/worker/isolate.rs` — spawn one isolate (worker thread + per-isolate runtime + `LocalSet` + fresh `Interp`/`Vm`), the per-isolate code-slice cache, and the request/response loop. One responsibility: a single isolate's bootstrap and run loop.
- `src/worker/dispatch.rs` — the `Send` byte-channel transport (request/response/abort messages), the caller-side `Value::Future` bridge, and the dependency-closure / code-slice builder. One responsibility: cross-thread transport + code-slice computation.
- `src/check/rules/worker_capture.rs` — the `worker-capture` checker rule. One responsibility: reject mutable-`let` capture / top-level-global mutation inside a `worker fn` body.
- `examples/workers_parallel_map.as`, `examples/workers_static_method.as`, `examples/workers_nested_inline.as`, `examples/workers_errors.as` — introductory corpus (all-modes tested).
- `examples/advanced/workers_sample_sort.as`, `examples/advanced/workers_monte_carlo.as`, `examples/advanced/workers_parse_files.as` — production-shaped corpus.
- `bench/workers_bench.as` + `bench/run_workers_bench.sh` — the §11.5 performance harness; emits `bench/WORKERS_RESULTS.md`.

**Modified (by responsibility):**
- Front-end (legacy oracle): `src/parser.rs` (`statement` dispatch @76, `export_decl` @178, `fn_decl(is_async)` @218, class-member loop @350), `src/ast.rs` (`Stmt::Fn` @300, `MethodDecl` @347, `Display` @415, `Display` for Fn).
- Front-end (CST/VM): `src/syntax/kind.rs` (`WorkerKw`), `src/syntax/parser.rs` (`statement` @197, `fn_decl` @575, `method_decl` @1342, helpers), `src/syntax/resolve/mod.rs` (worker helper alongside `is_static_method`), `src/compile/mod.rs` (`compile_fn_proto` flag read @2501).
- Runtime: `src/value.rs` (`Function`/`Method` `is_worker`), `src/vm/chunk.rs` (`FnProto.is_worker`), `src/interp.rs` (worker dispatch in `call_function`-async path @3714 region; `Interp` wiring), `src/vm/run.rs` (worker dispatch at the `is_async` call sites @1061 / @3215), `src/lib.rs` (module decl, no behavior change).
- Serialization: `src/vm/aso.rs` (`write_proto`/`read_proto` flags @728; `ASO_FORMAT_VERSION` @96), `src/vm/verify.rs`.
- Formatter: `src/fmt.rs` (`Stmt::Fn` arm @252, `write_method` @359).
- Checker/types: `src/check/rules/mod.rs` (declare + register), `src/check/infer/pass.rs` (`fn_return_type` @1072 wraps worker calls in `Future`; new `is_worker` helper near @1352).
- LSP: `src/lsp/providers/semantic_tokens.rs` (`is_keyword_kind` @230), `src/lsp/providers/completion.rs` (keyword list @25), `src/lsp/providers/hover.rs`.
- Tree-sitter & editors: `tree-sitter-ascript/grammar.js` (@202, @251, new `worker_keyword`), `tree-sitter-ascript/src/parser.c` (regen), `tree-sitter-ascript/queries/highlights.scm` (@20), `editors/vscode/syntaxes/ascript.tmLanguage.json` (@118), `editors/zed/languages/ascript/highlights.scm`, `editors/nvim/queries/ascript/highlights.scm`, `editors/zed/extension.toml` (`commit`), `editors/nvim/lua/ascript/treesitter.lua` (`revision`).
- Tests: `tests/frontend_conformance.rs`, `tests/treesitter_conformance.rs`, `tests/vm_differential.rs` (all-modes corpus), `tests/check.rs`, `tests/lsp.rs`, `tests/cli.rs`.
- Feature flags: `Cargo.toml` (`num_cpus` dep; the worker subsystem is unconditional core, must build under `--no-default-features`).
- Docs: `docs/content/language/modules-async.md` (worker section), `docs/assets/app.js` (NAV — only if a NEW page is added; this plan appends to an existing page, so NAV is unchanged — verify), `README.md` (feature table note). The full doc sweep is Plan B §8.2 — do NOT duplicate it here.

---

## Conventions used in every task

- **Commit trailer (required):** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- **Two-config rule:** `cargo test` AND `cargo test --no-default-features` must pass; `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` must be clean. The worker subsystem is CORE (no feature gate) — it must build with no default features.
- **Tree-sitter regen:** after any `grammar.js` change, from `tree-sitter-ascript/`: `tree-sitter generate --abi 14`, then `cargo build` recompiles `parser.c` via `build.rs`.
- **Borrow-across-await:** never hold a `RefCell` borrow across `.await` (clippy `await_holding_refcell_ref = "deny"`). Worker dispatch clones the slice/args OUT before any await.
- **Both engines byte-identical:** every example runs identically on tree-walker == specialized-VM == generic-VM == `.aso` (§11.3).

---

## Task 1: `is_worker` AST flag + legacy (oracle) parser

**Files:**
- Modify: `src/ast.rs` (`Stmt::Fn` @300, `MethodDecl` @347)
- Modify: `src/parser.rs` (`statement` @76, `export_decl` @178, `fn_decl` @218, class-member loop @350)
- Test: `src/parser.rs` `#[cfg(test)]` (near `parses_async_fn_decl` @2118)

- [x] **Step 1: Write the failing test** — add to `src/parser.rs` test module:

```rust
#[test]
fn parses_worker_fn_decl() {
    let p = parse(&lex("worker fn render(s) { return s }").unwrap()).unwrap();
    match &p[0] {
        Stmt::Fn { name, is_worker, is_async, .. } => {
            assert_eq!(name, "render");
            assert!(*is_worker);
            assert!(!*is_async);
        }
        other => panic!("expected Stmt::Fn, got {other:?}"),
    }
}

#[test]
fn worker_is_contextual_not_reserved() {
    // `worker` as an ordinary identifier still parses.
    assert!(parse(&lex("let worker = 5").unwrap()).is_ok());
    assert!(parse(&lex("fn worker() { return 1 }").unwrap()).is_ok());
}

#[test]
fn parses_static_worker_method() {
    let p = parse(&lex("class Img { static worker fn encode(px) { return px } }").unwrap()).unwrap();
    match &p[0] {
        Stmt::Class { methods, .. } => {
            assert!(methods[0].is_static);
            assert!(methods[0].is_worker);
        }
        other => panic!("expected Stmt::Class, got {other:?}"),
    }
}
```

- [x] **Step 2: Run to verify it fails**
  Run: `cargo test parses_worker_fn_decl worker_is_contextual parses_static_worker -- --nocapture`
  Expected: FAIL — `no field is_worker on Stmt::Fn` / `MethodDecl` (compile error).

- [x] **Step 3: Add the AST fields.** In `src/ast.rs`, add `is_worker: bool,` to `Stmt::Fn` (after `is_generator,` @306) and `pub is_worker: bool,` to `MethodDecl` (after `pub is_generator: bool,` @354). Add a doc line on `MethodDecl::is_worker`: `/// `worker fn` / `static worker fn` — Spec A: dispatched to a pooled isolate, returns future<T>.`

- [x] **Step 4: Thread it through the legacy parser.** In `src/parser.rs`:
  - Change `fn fn_decl(&mut self, is_async: bool)` → `fn fn_decl(&mut self, is_async: bool, is_worker: bool)` (@218); set `is_worker,` in the returned `Stmt::Fn` (@248).
  - In `statement` (@85–91): keep `Tok::Fn => self.fn_decl(false, false)`; keep the `async fn` arm calling `self.fn_decl(true, false)`. Add BEFORE the `Tok::Fn` arm a contextual `worker` arm:
    ```rust
    Tok::Ident(s) if s == "worker" && matches!(self.peek_nth(1), Tok::Fn | Tok::Async) => {
        self.advance(); // consume contextual `worker`
        let is_async = if *self.peek() == Tok::Async { self.advance(); true } else { false };
        self.fn_decl(is_async, true)
    }
    ```
    (Spec A: `async worker fn` is not a thing, but accept-and-ignore-then-flag mirrors the existing `async` handling; the `worker-capture`/inference treat the call as `future<T>` regardless. If `async worker fn` should be rejected, emit nothing here — it stays valid syntactically and the checker owns semantics. Keep it permissive to match `async`.)
  - In `export_decl` (@178–189): add the same contextual `worker` branch returning `self.fn_decl(is_async, true)?`.
  - In the class-member loop (@350–406): extend `is_static_method` recognition. After consuming the optional `static`, add an optional contextual `worker` consume mirroring `is_async`:
    ```rust
    let is_worker = if matches!(self.peek(), Tok::Ident(s) if s == "worker")
        && matches!(self.peek_nth(1), Tok::Async | Tok::Fn) {
        self.advance(); true
    } else { false };
    ```
    Also widen the member-start guard so a bare `worker fn` (no `static`) is recognized as a method: change the `if *self.peek() == Tok::Async || *self.peek() == Tok::Fn || is_static_method` condition to also accept a leading contextual `worker` followed by `fn`/`async`. Set `is_worker,` in the pushed `MethodDecl` (@403 region).

- [x] **Step 5: Run the tests**
  Run: `cargo test parses_worker_fn_decl worker_is_contextual parses_static_worker`
  Expected: PASS.

- [x] **Step 6: Fix the exhaustiveness fallout.** Building now fails wherever `Stmt::Fn`/`MethodDecl` are constructed or destructured (fmt, interp, value lowering, syntax/resolve, tree_builder, tests). For each `Stmt::Fn { … }` / `MethodDecl { … }` construction in non-Task-1 files, set `is_worker: false` for now (later tasks turn the flag on at the real sites). For destructures using `..`, no change. Run `cargo build` and resolve each error minimally. Commit only after `cargo build` is clean.

- [x] **Step 7: Commit**
```bash
git add src/ast.rs src/parser.rs
git commit -m "feat(parser): is_worker flag on Stmt::Fn/MethodDecl; worker contextual keyword in legacy oracle front-end

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: CST parser + `WorkerKw` + compiler flag read

**Files:**
- Modify: `src/syntax/kind.rs` (`WorkerKw`)
- Modify: `src/syntax/parser.rs` (`statement` @197, `fn_decl` @575, `method_decl` @1342)
- Modify: `src/syntax/resolve/mod.rs` (worker helper @34 region)
- Modify: `src/compile/mod.rs` (`compile_fn_proto` flag read @2501)
- Modify: `src/vm/chunk.rs` (`FnProto.is_worker` @335)
- Test: `src/syntax/parser.rs` test module (near `async_and_generator_fns` @2134), `src/compile/mod.rs` test module (near @5831)

- [x] **Step 1: Write the failing parser test** — add to `src/syntax/parser.rs` tests:

```rust
#[test]
fn worker_fn_and_static_worker_parse() {
    for src in [
        "worker fn f() { return 1 }",
        "worker fn g(a, b) { return a }",
        "class C { static worker fn h(x) { return x } }",
        "class C { worker fn m(x) { return x } }",
    ] {
        let r = parse(src);
        assert!(r.errors.is_empty(), "errors for {src}: {:?}", r.errors);
        assert!(
            tree_shape(src).contains(&SyntaxKind::FnDecl)
                || tree_shape(src).contains(&SyntaxKind::MethodDecl),
            "no Fn/Method decl for {src}"
        );
    }
}

#[test]
fn worker_stays_identifier_when_not_a_modifier() {
    assert!(parse("let worker = 5").errors.is_empty());
    assert!(parse("worker(1)").errors.is_empty()); // call to a fn named worker
}
```

- [x] **Step 2: Run to verify it fails**
  Run: `cargo test -p ascript worker_fn_and_static_worker worker_stays_identifier`
  Expected: FAIL — parse errors (`worker` not recognized) / `no variant WorkerKw`.

- [x] **Step 3: Add `WorkerKw`.** In `src/syntax/kind.rs`, add `WorkerKw,` immediately after `StaticKw` (@243). (Place it in the same keyword cluster so `is_keyword_kind` and other closed matches surface it as a compile error to handle — by design.)

- [x] **Step 4: Recognize `worker` in the CST parser.** In `src/syntax/parser.rs`:
  - Add a helper next to `is_async_fn` (@227):
    ```rust
    /// True if the cursor is at the contextual `worker` modifier: an `Ident`
    /// "worker" immediately followed by `fn` or `async` (a fn/method start).
    fn at_worker_modifier(p: &Parser) -> bool {
        p.at_kw("worker")
            && matches!(
                p.nontrivia.get(p.pos + 1).map(|&ti| p.tokens[ti].kind),
                Some(SyntaxKind::FnKw) | Some(SyntaxKind::AsyncKw)
            )
    }
    ```
  - In `statement` (@197 match): add an arm `Ident if at_worker_modifier(p) => fn_decl(p),` (before the `_ => expr_stmt(p)` fallthrough; `Ident` matches the raw token kind for the contextual keyword).
  - In `fn_decl` (@575): at the top, before the `AsyncKw` check, add:
    ```rust
    if at_worker_modifier(p) {
        p.bump_remap(WorkerKw);
    }
    ```
  - In `method_decl` (@1342): after the `at_static_method` bump (@1346), before the `AsyncKw` check, add the same `if at_worker_modifier(p) { p.bump_remap(WorkerKw); }`. Also update `at_static_method` is fine — but ensure the class-member dispatch reaches `method_decl` for a leading `worker fn` with no `static`. Find the class-member loop that decides field-vs-method (the `at_static_method`/`AsyncKw`/`FnKw` predicate) and add `|| at_worker_modifier(p)` to the method predicate.

- [x] **Step 5: Add the `FnProto.is_worker` field.** In `src/vm/chunk.rs` `FnProto` (@335), add `pub is_worker: bool,` after `pub is_generator: bool,` (@340) with a doc line. Fix the two non-aso `FnProto { … }` constructions in `src/compile/mod.rs` (@2358 region and @2552) and `src/vm/run.rs` (@419, @2782) and `src/vm/aso.rs` test (@1639) to set `is_worker` (real value in compiler, `false` in the VM/test stubs).

- [x] **Step 6: Read the flag in the compiler.** In `src/compile/mod.rs` `compile_fn_proto` (@2501–2512), add `is_worker` alongside `is_async`/`is_generator`:
  ```rust
  let mut is_worker = false;
  // … inside the token loop:
  SyntaxKind::WorkerKw => is_worker = true,
  ```
  Set `is_worker,` in the returned `FnProto` (@2556 region). Do the same flag-read in the method-proto path (`compile_method_proto` @2242 delegates to `compile_fn_proto`, so the token loop already covers it — verify the `MethodDecl` node carries the `WorkerKw` child token).

- [x] **Step 7: Add the resolver helper (parallel to `is_static_method`).** In `src/syntax/resolve/mod.rs` near @34:
  ```rust
  /// Spec A: a fn/method declared `worker` (carries a direct `WorkerKw` child token).
  pub fn is_worker_fn(node: &ResolvedNode) -> bool {
      node.children_with_tokens()
          .filter_map(|t| t.into_token())
          .any(|t| t.kind() == SyntaxKind::WorkerKw)
  }
  ```

- [x] **Step 8: Write a compiler unit test** — in `src/compile/mod.rs` tests (near @5831):
  ```rust
  #[test]
  fn compiles_worker_fn_proto_flag() {
      let proto = compile_first_fn_proto("worker fn f() { return 1 }");
      assert!(proto.is_worker);
      assert!(!proto.is_async);
  }
  ```
  (Use whatever helper the existing `is_async`/`is_generator` proto tests at @5831 use to fetch the first `FnProto`; mirror it.)

- [x] **Step 9: Run the tests**
  Run: `cargo test -p ascript worker_fn_and_static_worker worker_stays_identifier compiles_worker_fn_proto_flag`
  Expected: PASS. Then `cargo build` clean.

- [x] **Step 10: Commit**
```bash
git add src/syntax/kind.rs src/syntax/parser.rs src/syntax/resolve/mod.rs src/compile/mod.rs src/vm/chunk.rs src/vm/run.rs src/vm/aso.rs
git commit -m "feat(cst): worker contextual keyword (WorkerKw) in CST parser + FnProto.is_worker compile

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Tree-sitter grammar + queries + regen + editor pins + highlights

**Files:**
- Modify: `tree-sitter-ascript/grammar.js` (@202, @251, new `worker_keyword` @390 region)
- Regen: `tree-sitter-ascript/src/parser.c` (`tree-sitter generate --abi 14`)
- Modify: `tree-sitter-ascript/queries/highlights.scm` (@20)
- Modify: `editors/vscode/syntaxes/ascript.tmLanguage.json` (@118)
- Modify: `editors/zed/languages/ascript/highlights.scm`, `editors/nvim/queries/ascript/highlights.scm`
- Modify (after sync): `editors/zed/extension.toml` (`commit`), `editors/nvim/lua/ascript/treesitter.lua` (`revision`)
- Test: `tests/treesitter_conformance.rs`

- [x] **Step 1: Write the failing conformance test** — add to `tests/treesitter_conformance.rs` (mirror an existing "parses without ERROR" case):

```rust
#[test]
fn treesitter_parses_worker_decls() {
    for src in [
        "worker fn f() { return 1 }",
        "class C { static worker fn h(x) { return x } }",
        "class C { worker fn m(x) { return x } }",
    ] {
        assert!(!parse_has_error(src), "tree-sitter ERROR node in: {src}");
    }
}
```
(Use the file's existing `parse_has_error` helper / pattern.)

- [x] **Step 2: Run to verify it fails**
  Run: `cargo test --test treesitter_conformance treesitter_parses_worker_decls`
  Expected: FAIL — ERROR node (`worker` unknown modifier).

- [x] **Step 3: Add the `worker` modifier to the grammar.** In `tree-sitter-ascript/grammar.js`:
  - Define a contextual rule next to `static_keyword` (@390): `worker_keyword: _ => 'worker',`.
  - `function_declaration` (@202): insert `optional($.worker_keyword),` between `optional('async'),` and `'fn'`. (Order: `async? worker? fn` — the canonical decl order is `static? worker? fn`; for a free fn only `worker?` applies. Keep `worker` after `async` so `worker fn` and `async worker fn` both parse; the checker owns the "`async worker` redundant" semantic.)
  - `method_definition` (@251): insert `optional($.worker_keyword),` between `optional($.static_keyword),` and `optional('async'),` so the order is `static? worker? async? fn`. To match the formatter's canonical `static? worker? fn`, place `worker_keyword` AFTER `static_keyword` and BEFORE `async`.
  - If a GLR conflict arises (a bare `worker` identifier vs the modifier), add it to the grammar's `conflicts` array, mirroring how `static` is handled (the `static_keyword`/`worker_keyword` are precedence-less contextual tokens; do NOT give them a `prec`).

- [x] **Step 4: Regenerate the parser**
  Run: `cd tree-sitter-ascript && tree-sitter generate --abi 14` (use `dangerouslyDisableSandbox` only if the generate needs network — it does not).
  Then from the repo root: `cargo build` (recompiles `parser.c`).

- [x] **Step 5: Tag `worker` as a keyword in the canonical query.** In `tree-sitter-ascript/queries/highlights.scm`, add a node-capture for the contextual keyword (it is a named node `worker_keyword`, like `static_keyword`). Since `static_keyword` currently has no highlight, add BOTH for consistency at the end of the Keywords section:
  ```
  (worker_keyword) @keyword
  (static_keyword) @keyword
  ```
  (If adding `static_keyword` regresses a golden, scope to `(worker_keyword) @keyword` only — `worker` is the spec requirement.)

- [x] **Step 6: Run the conformance test + the highlight goldens**
  Run: `cargo test --test treesitter_conformance`
  Expected: PASS.

- [x] **Step 7: Update the three editor highlight copies.**
  - `editors/zed/languages/ascript/highlights.scm`: add `(worker_keyword) @keyword`.
  - `editors/nvim/queries/ascript/highlights.scm`: add `(worker_keyword) @keyword`.
  - `editors/vscode/syntaxes/ascript.tmLanguage.json` (@118): change the storage-modifier pattern `"\\b(static|async)\\b"` → `"\\b(static|worker|async)\\b"`.
  - If `editors/nvim/tests/treesitter_spec.lua` asserts on keyword tokens, add a `worker fn` case.

- [x] **Step 8: Publish the grammar + bump editor pins.**
  Run: `./scripts/sync-grammar.sh` (prints the new mirror SHA). Then bump that SHA in `editors/zed/extension.toml` (`commit = "<sha>"`) and `editors/nvim/lua/ascript/treesitter.lua` (`revision = "<sha>"`). (If `sync-grammar.sh` requires push credentials unavailable in this environment, record the step as DONE-ON-RELEASE and bump the pins to the locally-generated tree SHA, noting it in the commit body — CI `mirror-grammar.yml` reconciles.)
  **DONE-ON-RELEASE**: Push credentials unavailable; pins left at pre-Task-3 SHA `a075a12`. CI `mirror-grammar.yml` reconciles.

- [x] **Step 9: Commit**
```bash
git add tree-sitter-ascript/grammar.js tree-sitter-ascript/src/parser.c tree-sitter-ascript/queries/highlights.scm editors/
git commit -m "feat(grammar): worker modifier in tree-sitter + highlights + editor pins; regen parser.c (--abi 14)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Formatter + AST Display

**Files:**
- Modify: `src/fmt.rs` (`Stmt::Fn` arm @252, `write_method` @359)
- Modify: `src/ast.rs` (`Display` for `Stmt` @415, Fn arm)
- Test: `src/fmt.rs` test module

- [x] **Step 1: Write the failing test** — add to `src/fmt.rs` tests:

```rust
#[test]
fn formats_worker_modifier_canonical_order() {
    assert_eq!(fmt_str("worker fn   f( )  { return 1 }"), "worker fn f() {\n  return 1\n}\n");
    assert_eq!(
        fmt_str("class C { static   worker   fn h(x){return x} }"),
        "class C {\n  static worker fn h(x) {\n    return x\n  }\n}\n"
    );
}

#[test]
fn worker_fmt_is_idempotent() {
    let once = fmt_str("worker fn f() { return 1 }");
    assert_eq!(fmt_str(&once), once);
}
```
(Use the file's existing formatter test helper — likely `fmt_str` / `format_source`; mirror the nearest fn-formatting test.)

- [x] **Step 2: Run to verify it fails**
  Run: `cargo test --lib formats_worker_modifier worker_fmt_is_idempotent`
  Expected: FAIL — `worker` not emitted.

- [x] **Step 3: Render in `src/fmt.rs`.**
  - `Stmt::Fn` arm (@252): destructure `is_worker,`; emit the modifier in canonical order — `async` then `worker` is wrong; the spec's canonical decl order is `static? worker? fn`. For a free fn (no static) emit `worker ` BEFORE the `fn`/`fn* ` and BEFORE any `async`:
    ```rust
    if *is_worker { out.push_str("worker "); }
    if *is_async { out.push_str("async "); }
    out.push_str(if *is_generator { "fn* " } else { "fn " });
    ```
  - `write_method` (@359): emit in order `static? worker? async? fn`:
    ```rust
    if m.is_static { out.push_str("static "); }
    if m.is_worker { out.push_str("worker "); }
    if m.is_async { out.push_str("async "); }
    out.push_str(if m.is_generator { "fn* " } else { "fn " });
    ```

- [x] **Step 4: Render in `ast.rs` `Display`.** No `Display for Stmt` exists in `ast.rs` — the formatter (`src/fmt.rs`) IS the canonical printer. Step is a no-op: no stale rendering to fix. Consistency is achieved solely through `fmt.rs`.

- [x] **Step 5: Run the tests**
  Run: `cargo test --lib formats_worker_modifier worker_fmt_is_idempotent`
  Expected: PASS.

- [x] **Step 6: Commit**
```bash
git add src/fmt.rs src/ast.rs
git commit -m "feat(fmt): render worker modifier in canonical static? worker? fn order; ast Display matches

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: `.aso` serialization of `is_worker` + version bump + verify

**Files:**
- Modify: `src/vm/aso.rs` (`write_proto`/`read_proto` @728; `ASO_FORMAT_VERSION` @96)
- Modify: `src/vm/verify.rs`
- Test: `src/vm/aso.rs` test module (near @1639)

- [x] **Step 1: Write the failing round-trip test** — add to `src/vm/aso.rs` tests:

```rust
#[test]
fn proto_is_worker_survives_aso_roundtrip() {
    let mut proto = test_fn_proto(); // the existing helper @1639 region
    proto.is_worker = true;
    let mut w = Writer::new();
    write_proto(&mut w, &proto).unwrap();
    let mut r = Reader::new(&w.into_bytes());
    let back = read_proto(&mut r).unwrap();
    assert!(back.is_worker);
}
```
(If there is no `test_fn_proto` helper, construct an `FnProto { …, is_worker: true, … }` inline matching the @1639 literal.)

- [x] **Step 2: Run to verify it fails**
  Run: `cargo test --lib proto_is_worker_survives_aso_roundtrip`
  Expected: FAIL — `is_worker` lost (defaults false) / missing field.

- [x] **Step 3: Extend the flags byte.** In `write_proto` (@730), add bit 3:
  ```rust
  let flags = u8::from(p.has_rest)
      | (u8::from(p.is_async) << 1)
      | (u8::from(p.is_generator) << 2)
      | (u8::from(p.is_worker) << 3);
  ```
  In `read_proto` (@747), add `let is_worker = flags & 8 != 0;` and set `is_worker,` in the returned `FnProto` (@757).

- [x] **Step 4: Bump the format version + comment.** In `src/vm/aso.rs`, bump `ASO_FORMAT_VERSION` from `15` to `16` (@96). Update the module-level "bump on ANY change to … FnProto …" doc note (@34) is already general; add a one-line changelog comment near the constant: `// v16: FnProto flags byte gained bit3 = is_worker (Workers Spec A).`

- [x] **Step 5: Update verify.** In `src/vm/verify.rs`, if it validates the flags byte or asserts on proto layout, allow bit 3. (If verify is structural-only and does not inspect the flags byte, no change — confirm by reading the proto-verify path.)

- [x] **Step 6: Run the tests**
  Run: `cargo test --lib proto_is_worker_survives_aso_roundtrip` and `cargo test --lib aso`
  Expected: PASS (the version-mismatch tests @1720/@1783 still pass against the new constant).

- [x] **Step 7: Commit**
```bash
git add src/vm/aso.rs src/vm/verify.rs
git commit -m "feat(aso): serialize FnProto.is_worker (flags bit3); bump ASO_FORMAT_VERSION 15->16

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: The structured-clone serializer + sendability gate

**Files:**
- Create: `src/worker/serialize.rs`
- Create: `src/worker/mod.rs` (stub for now; full dispatch in Task 8)
- Modify: `src/lib.rs` (add `pub mod worker;`)
- Test: `src/worker/serialize.rs` `#[cfg(test)]`

- [x] **Step 1: Write the failing round-trip + rejection tests** — create `src/worker/serialize.rs` starting with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Interp;
    use crate::value::Value;

    fn rt(v: &Value) -> Value {
        let interp = Interp::new();
        decode(&encode(v).unwrap(), &interp).unwrap()
    }

    #[test]
    fn roundtrips_scalars() {
        for v in [Value::Nil, Value::Bool(true), Value::Number(3.5),
                  Value::Str("hi".into())] {
            assert_eq!(rt(&v), v);
        }
    }

    #[test]
    fn roundtrips_array_object_map_set() {
        let interp = Interp::new();
        let src = interp_eval_to_value(&interp, "[1, 2, [3, #{\"k\": 4}]]");
        assert_eq!(rt(&src), src);
        let obj = interp_eval_to_value(&interp, "{a: 1, b: [2, 3]}");
        assert_eq!(rt(&obj), obj);
        let set = interp_eval_to_value(&interp, "set([1, 2, 3])");
        assert_eq!(rt(&set), set);
    }

    #[test]
    fn map_key_canonicalization_preserved() {
        let interp = Interp::new();
        // -0.0 and +0.0 collapse to one key; NaN unifies — must survive the boundary.
        let m = interp_eval_to_value(&interp, "#{ -0.0: \"a\", 0.0: \"b\" }");
        assert_eq!(rt(&m), m); // single entry, value "b"
    }

    #[test]
    fn cycles_are_handled() {
        // a = []; a.push(a) — a self-referential array must encode without
        // infinite recursion and decode into a value that is its own element.
        let interp = Interp::new();
        let a = interp_eval_to_value(&interp, "(fn(){ let a = []; a.push(a); return a })()");
        let back = rt(&a);
        // The decoded array's first element is identity-equal to the array itself.
        if let Value::Array(arr) = &back {
            assert!(matches!(&arr.borrow()[0], Value::Array(inner) if std::rc::Rc::ptr_eq_cc(arr, inner)));
        } else { panic!("expected array"); }
    }

    #[test]
    fn class_instance_reconstructs_by_identity_and_fields() {
        // The far side has the class def (here the SAME interp), so an Instance
        // round-trips by class name + cloned fields (validate_into machinery).
        let interp = Interp::new();
        let inst = interp_eval_to_value(&interp,
            "(fn(){ class P { x: number; y: number } return P.from({x: 1, y: 2}) })()");
        let back = rt(&inst);
        assert_eq!(format!("{back}"), format!("{inst}"));
    }

    #[test]
    fn rejects_function_with_field_path() {
        let interp = Interp::new();
        let v = interp_eval_to_value(&interp, "[1, {cb: fn(){ return 1 }}]");
        let err = check_sendable(&v).unwrap_err();
        assert_eq!(err.kind, "function");
        assert_eq!(err.path, "[1].cb");
        assert!(err.message().contains("cannot be sent to a worker at [1].cb"));
    }

    #[test]
    fn rejects_future_and_native() {
        let interp = Interp::new();
        let fut = interp_eval_to_value(&interp, "(async fn(){ return 1 })()");
        assert_eq!(check_sendable(&fut).unwrap_err().kind, "future");
    }
}
```
(Add a small `interp_eval_to_value(&Interp, &str) -> Value` test helper in this module — synchronously run a single-expr program via `crate::interp` and return the value; if no sync eval helper exists, gate these tests with `#[tokio::test]` and use `vm_run_source`-style plumbing, or build the `Value`s directly with the public constructors. Prefer direct `Value` construction where eval plumbing is heavy. `ptr_eq_cc` is illustrative — use the actual `Cc` identity check, `gcmodule`'s pointer compare.)

- [x] **Step 2: Run to verify it fails**
  Run: `cargo test --lib worker::serialize`
  Expected: FAIL — module/functions do not exist.

- [x] **Step 3: Implement `SendError`.** At the top of `src/worker/serialize.rs`:
```rust
//! Structured-clone Value serializer (Workers Spec A §5). The airlock: only bytes
//! cross threads — never a `Value`, never the `Interp`. Semantics follow the WHATWG
//! structured-clone algorithm (cycle table + per-kind copy; class reconstruction by
//! identity + fields). Engine-agnostic: operates purely on `Value`.

use crate::interp::Interp;
use crate::value::{MapKey, Value};

/// A value that cannot cross an isolate boundary (our DataCloneError analog).
#[derive(Debug, Clone)]
pub struct SendError {
    pub kind: &'static str,       // e.g. "function", "native", "future", "generator"
    pub path: String,             // e.g. "arg[1].cb"
    pub hint: Option<&'static str>,
}

impl SendError {
    pub fn message(&self) -> String {
        let mut m = format!(
            "value of kind {} cannot be sent to a worker at {}",
            self.kind, self.path
        );
        if let Some(h) = self.hint {
            m.push_str(" — ");
            m.push_str(h);
        }
        m
    }
}

const CHANNEL_HINT: &str = "event emitters / channels are isolate-local; communicate \
across workers via worker results (Spec A) or actor/generator messages (Spec B)";
```

- [x] **Step 4: Implement `check_sendable`.** A recursive walk building a path string; rejects `Value::Function`, `Value::Builtin`, `Value::BoundMethod`, `Value::NativeMethod`, `Value::Native` (append `CHANNEL_HINT` when the native is an events emitter or `std/sync` channel — detect by the `NativeObject` kind), `Value::Future`, `Value::Generator`, `Value::GeneratorMethod`, `Value::ClassMethod`, `Value::Class`, `Value::Enum`. Recurse into `Array`, `Object` (key in path: `.key`), `Map` (`["key"]`), `Set`, `Instance` (fields by name). Guard cycles with an identity `HashSet` of visited container pointers so `check_sendable` itself terminates. Path grammar: array `[i]`, object/instance `.name` (or `["name"]` if not an ident), map `["display(key)"]`.

- [x] **Step 5: Implement `encode`.** A `Writer` (reuse the byte-writer pattern from `src/vm/aso.rs` — a `Vec<u8>` with `u8`/`u32`/`len`/`str` helpers, or a private mini-writer here) plus a **visited table**: a `Vec<*const ()>` (or `IndexMap<ptr,u32>`) assigning each container a serial id on first visit. Tag byte per kind: `0=Nil 1=Bool 2=Number 3=Decimal 4=Str 5=Bytes 6=Array 7=Object 8=Map 9=Set 10=Enum/EnumVariant 11=Regex(source+flags) 12=Instance 13=Ref(id)`. On encountering an already-visited container, emit `13` + its id. `encode` calls `check_sendable` first (so a bad value never produces half a payload), then walks. Returns `Result<Vec<u8>, SendError>`.

- [x] **Step 6: Implement `decode`.** A `Reader` over the bytes + a **reconstruction table** `Vec<Value>` indexed by serial id. For containers, FIRST allocate the empty container, push it into the table (so a forward `Ref(id)` resolves), THEN fill it (this is how cycles round-trip). For `Instance` (tag 12): read the class name, look it up in `interp` (the far isolate's globals/classes), read the field map, and reconstruct via the same shape machinery `validate_into` uses (apply the class's field schema; do NOT run `init`). For `Regex`: re-compile from source+flags. `MapKey` is rebuilt via `MapKey::from_value` so −0.0/NaN canonicalization is reapplied on the far side. Returns `Result<Value, SendError>` (decode errors — e.g. unknown class — are `SendError { kind: "class", path: "<name>", hint: None }`).

- [x] **Step 7: Stub `src/worker/mod.rs`:**
```rust
//! Workers Spec A: shared-nothing isolates. `serialize` is the value airlock;
//! `pool`/`isolate`/`dispatch` (later tasks) host the isolate pool + transport.
pub mod serialize;
```
Add `pub mod worker;` to `src/lib.rs` (near the other `pub mod` declarations).

- [x] **Step 8: Run the tests**
  Run: `cargo test --lib worker::serialize` and `cargo test --no-default-features --lib worker::serialize`
  Expected: PASS in both configs (the serializer is core).

- [x] **Step 9: Commit**
```bash
git add src/worker/serialize.rs src/worker/mod.rs src/lib.rs
git commit -m "feat(worker): structured-clone Value serializer + sendability gate (cycles, class reconstruction, field-path errors)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Dependency-closure / code-slice builder + bytecode shipping

**Files:**
- Create: `src/worker/dispatch.rs` (the closure walker + `WorkerCodeSlice` builder)
- Modify: `src/worker/mod.rs` (export `WorkerCodeSlice`, `build_code_slice`)
- Test: `src/worker/dispatch.rs` `#[cfg(test)]`

- [ ] **Step 1: Write the failing closure test** — in `src/worker/dispatch.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // worker fn `g` calls top-level `helper` and reads top-level const `K`;
    // the code slice must include g, helper, and K (transitively), but NOT an
    // unrelated top-level fn `other`.
    const SRC: &str = "
        const K = 10
        fn helper(x) { return x + K }
        fn other() { return 999 }
        worker fn g(n) { return helper(n) }
    ";

    #[tokio::test]
    async fn code_slice_includes_transitive_deps_only() {
        let slice = build_slice_for_test(SRC, "g").await;
        let names = slice.dep_names();
        assert!(names.contains("g"));
        assert!(names.contains("helper"));
        assert!(names.contains("K"));
        assert!(!names.contains("other"));
    }

    #[tokio::test]
    async fn slice_aso_roundtrips_and_runs() {
        // The shipped bytecode (entry_aso) deserializes via the .aso reader and
        // runs g(5) -> 15 on a FRESH interp/vm (no access to the original heap).
        let slice = build_slice_for_test(SRC, "g").await;
        let out = run_slice_in_fresh_isolate(&slice, vec![Value::Number(5.0)]).await;
        assert_eq!(out.unwrap(), Value::Number(15.0));
    }
}
```
(`build_slice_for_test` / `run_slice_in_fresh_isolate` / `dep_names` are test-only helpers you add in this module; `run_slice_in_fresh_isolate` constructs a new `Interp`, loads the slice's `.aso` (the deps + entry), and calls the entry — the synchronous in-process analog of the isolate run loop, validating the slice before the threading lands.)

- [ ] **Step 2: Run to verify it fails**
  Run: `cargo test --lib worker::dispatch`
  Expected: FAIL — functions do not exist.

- [ ] **Step 3: Define `WorkerCodeSlice`** in `src/worker/mod.rs`:
```rust
use std::rc::Rc;
/// The shippable bytecode payload for one worker fn: its compiled chunk(s) plus
/// its transitive top-level dependency closure, serialized via the `.aso` writer,
/// keyed by a stable function identity for per-isolate caching.
pub struct WorkerCodeSlice {
    pub fn_id: u64,                 // identity for the per-isolate code cache
    pub entry_aso: Rc<[u8]>,        // .aso bytes: deps + the entry fn
    pub class_name: Option<Rc<str>>,// Some for `static worker fn` on a class
}
```

- [ ] **Step 4: Implement the closure walk + slice build** in `src/worker/dispatch.rs`. The closure walks the compiled `Chunk`'s constant pool / global references (the same `GET_GLOBAL` name set + nested `FnProto` consts) to find referenced top-level names; resolve each name to its top-level binding (fn or const); recurse. For consts, the VALUE is structured-clone'd into the isolate at dispatch (per §4); for fns, the `FnProto`/AST is serialized. Materialize the slice via the `.aso` Writer (`src/vm/aso.rs`) — write a small "module fragment": the set of `(name, FnProto|const-bytes)` plus the entry name. `fn_id` is a stable hash of the entry's identity (e.g. its def span + name). Provide `build_code_slice(interp, entry_proto_or_fn, class_name) -> Result<WorkerCodeSlice, Control>`. (For the tree-walker oracle, the equivalent slice ships the AST closure — Task 8 wires the per-engine materialization; here implement the VM/`.aso` path and the closure algorithm engine-agnostically over names.)

- [ ] **Step 5: Run the tests**
  Run: `cargo test --lib worker::dispatch`
  Expected: PASS. Also `--no-default-features`.

- [ ] **Step 6: Commit**
```bash
git add src/worker/dispatch.rs src/worker/mod.rs
git commit -m "feat(worker): dependency-closure code-slice builder reusing .aso; ships entry + transitive top-level deps

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Isolate bootstrap + the lazy demand-grown pool + channel dispatch

**Files:**
- Create: `src/worker/isolate.rs`, `src/worker/pool.rs`
- Modify: `src/worker/mod.rs` (`dispatch_worker`, `pool_is_initialized`)
- Modify: `Cargo.toml` (add `num_cpus`)
- Test: `tests/cli.rs` (integration, spawning the binary)

- [ ] **Step 1: Write the failing integration test** — add to `tests/cli.rs`:

```rust
#[test]
fn worker_parallel_map_runs() {
    let src = r#"
        import { gather } from "std/task"
        worker fn sq(n) { return n * n }
        let fs = [1, 2, 3, 4].map(sq)
        let r = await gather(fs)
        print(r)
    "#;
    let out = run_program(src); // existing helper: writes a temp .as, runs the binary
    assert_eq!(out.trim(), "[1, 4, 9, 16]");
}

#[test]
fn no_worker_program_starts_no_pool() {
    // A program with zero worker fns must not create the pool / any worker thread.
    let src = "print(1 + 1)";
    let out = run_program(src);
    assert_eq!(out.trim(), "2");
    // (The lazy-pool unit proof lives in pool.rs; this just confirms normal scripts
    //  still run unaffected.)
}
```
(Use the existing `tests/cli.rs` helper for spawning `env!("CARGO_BIN_EXE_ascript")` over a temp file; mirror a current test.)

- [ ] **Step 2: Run to verify it fails**
  Run: `cargo test --test cli worker_parallel_map_runs`
  Expected: FAIL — `worker fn` dispatch not implemented (currently runs inline or errors).

- [ ] **Step 3: Add `num_cpus`** to `Cargo.toml` `[dependencies]` (core, not feature-gated). Run `cargo build`.

- [ ] **Step 4: Implement one isolate** in `src/worker/isolate.rs`. An isolate is a `std::thread::Builder::new().stack_size(WORKER_STACK_SIZE).spawn(...)` (mirror `run_on_worker_stack`, `src/lib.rs:52`) hosting a fresh current-thread tokio runtime + `LocalSet` + a fresh `Interp` (`Interp::new()`). It owns a `Send` request channel (`tokio::sync::mpsc`) and replies on a per-request `oneshot`. Request message (all `Send` bytes):
```rust
pub struct WorkerRequest {
    pub fn_id: u64,
    pub slice_bytes: Option<Rc<[u8]>>, // None when fn_id already cached on this isolate
    pub class_name: Option<String>,
    pub entry_name: String,
    pub args: Vec<u8>,                 // structured-clone-encoded
    pub reply: tokio::sync::oneshot::Sender<WorkerReply>,
    pub abort: tokio::sync::oneshot::Receiver<()>, // cancel-on-drop signal
}
pub enum WorkerReply { Ok(Vec<u8>), Panic(String) } // result bytes or worker-side message
```
The isolate loop: on a request, ensure the slice is cached (load the `.aso` into the isolate's `Interp` if `slice_bytes` is `Some` and `fn_id` is new); `decode` the args against the isolate's `Interp`; `select!` the entry-fn run against the `abort` receiver; on completion `encode` the result and send `WorkerReply::Ok`, or `WorkerReply::Panic(message)` on an uncaught Tier-2 panic. A `[value, err]` Result is ordinary data and rides through `Ok`.

- [ ] **Step 5: Implement the pool** in `src/worker/pool.rs`. A process-global `thread_local!`/`OnceCell`-backed `RefCell<Option<Pool>>` (the caller thread owns it; isolates are spawned from here). `Pool`: a cap = `env::var("ASCRIPT_WORKERS").ok().and_then(parse).unwrap_or_else(num_cpus::get)`, a `Vec<IsolateHandle>` (each = the `Send` request sender + a "busy" flag), and a FIFO `VecDeque` of pending jobs. `acquire()`:
  - if an idle isolate exists → use it;
  - else if `live < cap` → spawn a new isolate (demand growth) → use it;
  - else → enqueue the job (backpressure); it dispatches when an isolate frees up.
  `pool_is_initialized()` returns whether the `OnceCell` is set. **Inline nesting:** the pool exposes `in_isolate()` (a thread-local flag set inside an isolate's run loop); `dispatch_worker` checks it FIRST and, when true, runs the worker body INLINE in the current isolate (no re-dispatch) — deadlock-free per §7.

- [ ] **Step 6: Implement `dispatch_worker`** in `src/worker/mod.rs`:
```rust
pub fn dispatch_worker(
    interp: &Interp, slice: WorkerCodeSlice, args: Vec<Value>, span: Span,
) -> Result<Value, Control> { /* … */ }
```
Behavior: if `pool::in_isolate()` → run inline (call the entry locally) and wrap as a resolved `Value::Future`. Otherwise: `serialize::check_sendable` each arg (mapping `SendError` → a recoverable `Control::Panic` carrying `span`); `encode` the args; build a `SharedFuture` (`crate::task::SharedFuture::new()`); create the `oneshot` reply + `oneshot` abort; hand the request to `pool::acquire()`; `spawn_local` a SMALL bridge task on the CALLER thread that awaits the reply `oneshot`, `decode`s the result bytes against `interp` (or converts `WorkerReply::Panic(msg)` into a recoverable `Control::Panic`), and resolves the `SharedFuture`'s cell. Wire `SharedFuture::set_abort` to a handle that drops the abort `oneshot` sender → cancel-on-drop sends the abort signal across the channel (Task 9 tests this). Return `Value::Future(fut)`.

- [ ] **Step 7: Hook the dispatch into both engines.**
  - Legacy interp (`src/interp.rs`, the `call_function` async region @3714): BEFORE the `if func.is_async` block, add:
    ```rust
    if func.is_worker {
        let slice = crate::worker::build_code_slice_treewalker(self, &func)?;
        return crate::worker::dispatch_worker(self, slice, args, span);
    }
    ```
  - VM (`src/vm/run.rs` @1061 and @3215): alongside the `callee.proto.is_async` branch, add a `closure.proto.is_worker` branch that calls `crate::worker::dispatch_worker(...)` with the VM's `Interp` and pushes the returned `Value::Future`. (The VM's `Interp` is reachable via the `Vm`'s interp ref used by the async path — pin it by reading the @1061 context.)
  - For `static worker fn` (a `ClassMethod`/`Method` with `is_worker`), hook the static-method call path the same way (set `class_name = Some(class.name)` in the slice).

- [ ] **Step 8: Run the tests**
  Run: `cargo test --test cli worker_parallel_map_runs no_worker_program_starts_no_pool`
  Expected: PASS.

- [ ] **Step 9: Commit**
```bash
git add src/worker/isolate.rs src/worker/pool.rs src/worker/mod.rs src/interp.rs src/vm/run.rs Cargo.toml
git commit -m "feat(worker): lazy demand-grown isolate pool + Send byte-channel dispatch; worker fn returns future<T>

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Cancel-on-drop, error/panic propagation, oversubscription, inline nesting, lazy-pool proof

**Files:**
- Test: `tests/cli.rs` (integration), `src/worker/pool.rs` `#[cfg(test)]` (lazy proof)
- Modify: `src/worker/dispatch.rs` / `pool.rs` / `isolate.rs` as needed to make the tests pass

- [x] **Step 1: Write the failing tests** — add to `tests/cli.rs`:

```rust
#[test]
fn worker_panic_is_recoverable_on_caller() {
    let src = r#"
        worker fn boom(n) { panic("kaboom " + n) }
        let r = recover(fn() { return await boom(7) })
        print(r[1] != nil)        // an error pair, caught
    "#;
    assert_eq!(run_program(src).trim(), "true");
}

#[test]
fn worker_result_pair_crosses_as_data() {
    let src = r#"
        worker fn parse(s) { return [s.len(), nil] }
        let r = await parse("abcd")?
        print(r)
    "#;
    assert_eq!(run_program(src).trim(), "4");
}

#[test]
fn oversubscription_completes_via_queue() {
    // More calls than the pool cap; all must complete (queue drains).
    let src = r#"
        import { gather } from "std/task"
        worker fn sq(n) { return n * n }
        let fs = (1..=20).toArray().map(sq)
        print((await gather(fs)).sum())
    "#;
    // 1^2+..+20^2 = 2870
    assert_eq!(run_with_env(src, &[("ASCRIPT_WORKERS", "2")]).trim(), "2870");
}

#[test]
fn nested_worker_runs_inline_no_deadlock() {
    let src = r#"
        worker fn inner(n) { return n + 1 }
        worker fn outer(n) { return await inner(n) * 2 }
        print(await outer(10))   // (10+1)*2 = 22, no deadlock at pool size 1
    "#;
    assert_eq!(run_with_env(src, &[("ASCRIPT_WORKERS", "1")]).trim(), "22");
}

#[test]
fn sendability_violation_reports_field_path() {
    let src = r#"
        worker fn f(o) { return 1 }
        let r = recover(fn() { return await f({cb: fn(){ return 1 }}) })
        print(r[1].message)
    "#;
    let out = run_program(src);
    assert!(out.contains("cannot be sent to a worker at"), "got: {out}");
}
```
(Add `run_with_env` to `tests/cli.rs` if absent — like `run_program` but sets env vars on the child.) And a unit lazy-pool proof in `src/worker/pool.rs`:
```rust
#[test]
fn pool_not_initialized_until_first_dispatch() {
    assert!(!crate::worker::pool_is_initialized());
}
```

- [x] **Step 2: Run to verify failures**
  Run: `cargo test --test cli worker_panic_is_recoverable worker_result_pair oversubscription nested_worker sendability_violation` and `cargo test --lib pool_not_initialized`
  Expected: FAIL (cancel/panic/queue/inline paths incomplete).

- [x] **Step 3: Implement panic propagation.** In the isolate run loop, catch an uncaught `Control::Panic(e)` from the entry run and send `WorkerReply::Panic(e.message)` (carry worker-side span context in the message string). In `dispatch_worker`'s bridge task, convert `WorkerReply::Panic(msg)` into a **recoverable** `Control::Panic(AsError::new(msg))` resolved into the future's cell, so `recover` catches it. A `[value, err]` pair returns via `WorkerReply::Ok` (ordinary encoded data) — no special-casing.

- [x] **Step 4: Implement cancel-on-drop across the boundary.** The `SharedFuture` handle owns the abort `oneshot` SENDER (or a guard whose `Drop` fires it). When the last `Value::Future` clone drops, the guard drops → the isolate's `select!` sees the abort `oneshot` resolve → it stops the in-flight job and is reclaimed to the pool. Verify with a focused unit test in `dispatch.rs` if integration timing is flaky (mirror `task.rs::dropping_last_handle_aborts_the_task`).

- [x] **Step 5: Implement the FIFO queue + inline nesting** (if not already complete in Task 8): jobs beyond `cap` enqueue and dispatch on isolate-free; `pool::in_isolate()` short-circuits to inline. Confirm `nested_worker_runs_inline_no_deadlock` passes at cap=1.

- [x] **Step 6: Run the tests**
  Run: the Step-1 commands.
  Expected: PASS.

- [x] **Step 7: Commit**
```bash
git add tests/cli.rs src/worker/
git commit -m "feat(worker): cancel-on-drop, recoverable worker-panic propagation, FIFO oversubscription queue, inline nesting; lazy-pool proof

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: `worker-capture` checker rule (default Error)

**Files:**
- Create: `src/check/rules/worker_capture.rs`
- Modify: `src/check/rules/mod.rs` (declare + register in `ALL`)
- Test: `tests/check.rs`

- [x] **Step 1: Write the failing test** — add to `tests/check.rs`:

```rust
#[test]
fn worker_capture_allows_const_and_params_and_top_fns() {
    let src = "
        const K = 5
        fn helper(x) { return x }
        worker fn g(n) { return helper(n) + K }
    ";
    assert!(!diagnostics(src).iter().any(|d| d.code == "worker-capture"));
}

#[test]
fn worker_capture_rejects_mutable_let_capture() {
    let src = "
        let counter = 0
        worker fn g(n) { return n + counter }
    ";
    let d = diagnostics(src);
    let wc: Vec<_> = d.iter().filter(|d| d.code == "worker-capture").collect();
    assert_eq!(wc.len(), 1);
    assert_eq!(wc[0].severity, Severity::Error);
}

#[test]
fn worker_capture_rejects_top_level_mutation() {
    let src = "
        let total = 0
        worker fn g(n) { total = total + n; return total }
    ";
    assert!(diagnostics(src).iter().any(|d| d.code == "worker-capture"
        && d.severity == Severity::Error));
}
```
(Use `tests/check.rs`'s existing `diagnostics(src)` helper and `Severity` import; mirror a current rule test like `workflow_determinism`.)

- [x] **Step 2: Run to verify it fails**
  Run: `cargo test --test check worker_capture`
  Expected: FAIL — no such code emitted.

- [x] **Step 3: Implement the rule.** Create `src/check/rules/worker_capture.rs` modeled on `src/check/rules/workflow_determinism.rs` (same `fn check(root: &ResolvedNode, res: &ResolveResult, src: &str) -> Vec<AsDiagnostic>` signature, `Rule` type). For each `FnDecl`/`MethodDecl` where `crate::syntax::resolve::is_worker_fn(node)` is true: walk the body's `NameRef`s and `AssignExpr` targets. Using the resolver result (`res`), classify each referenced name's binding:
  - param of THIS worker fn → OK;
  - top-level fn → OK;
  - top-level `const` → OK (copied at dispatch);
  - top-level mutable `let` READ → Error (`worker-capture`): `worker fn cannot capture mutable outer binding '<name>' — consts are copied; make it const or pass it as an argument`;
  - any WRITE to a top-level global from inside the body → Error: `worker fn cannot mutate the top-level binding '<name>' — workers run in a separate isolate`;
  - outer (non-top-level, non-param) mutable `let` capture → Error (same as the mutable-capture case).
  Emit `AsDiagnostic { code: "worker-capture", severity: Severity::Error, span, message }`. Default severity is Error (a correctness gate). Use the `ByteSpan` of the offending `NameRef`/`AssignExpr`.

- [x] **Step 4: Register the rule.** In `src/check/rules/mod.rs`: add `pub mod worker_capture;` (@24) and `worker_capture::check,` to `ALL` (@47).

- [x] **Step 5: Run the tests + the corpus zero-regression check**
  Run: `cargo test --test check worker_capture` then `cargo test --test check` (ensure `examples/**` still emits no unexpected `worker-capture`).
  Expected: PASS.

- [x] **Step 6: Commit**
```bash
git add src/check/rules/worker_capture.rs src/check/rules/mod.rs
git commit -m "feat(check): worker-capture rule (default Error) — reject mutable-let capture / top-level mutation in worker fn

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: Type inference — `worker fn` call synthesizes `future<T>`

**Files:**
- Modify: `src/check/infer/pass.rs` (`fn_return_type` @1072; new `is_worker` helper near @1352)
- Test: `tests/check.rs` (or `src/check/infer` unit test, matching where async-call inference is tested)

- [x] **Step 1: Write the failing test** — add a test asserting a `worker fn` call's awaited value is the scalar return type and that the un-awaited call is a `future<T>` (no `possibly-nil`/`type-mismatch` false positives), AND that `examples/**` stays zero `type-*`:

```rust
#[test]
fn worker_call_infers_future_like_async() {
    // Awaiting a worker fn yields the scalar; the inference must NOT flag a
    // type-mismatch when the awaited number is used as a number.
    let src = "
        worker fn sq(n: number): number { return n * n }
        fn use(): number { return await sq(3) }
    ";
    assert!(diagnostics(src).iter().all(|d| !d.code.starts_with("type-")));
}
```

- [x] **Step 2: Run to verify it fails (or proves the gap)**
  Run: `cargo test --test check worker_call_infers_future`
  Expected: Initially may PASS-by-accident if `worker fn` already falls to the non-async branch returning the bare scalar (so `await scalar` is identity and no mismatch). Make the intent explicit and robust by Step 3 regardless. If it FAILS (a spurious `type-` diagnostic), proceed.

- [x] **Step 3: Wrap worker calls in `Future`.** In `src/check/infer/pass.rs`:
  - Add a helper near `is_async` (@1352):
    ```rust
    fn is_worker(decl: &ResolvedNode) -> bool {
        decl.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::WorkerKw)
    }
    ```
  - In `fn_return_type` (@1072–1082): treat a worker fn like an async fn — wrap the return in `CheckTy::Future`:
    ```rust
    let ret = self.fn_declared_or_inferred(fn_decl);
    if is_async(fn_decl) || is_worker(fn_decl) {
        CheckTy::Future(Box::new(ret))
    } else {
        ret
    }
    ```
  (`await future<T>` already unwraps to `T` at `pass.rs:607`, so downstream reasoning is unchanged.)

- [x] **Step 4: Run the tests + the invariant**
  Run: `cargo test --test check worker_call_infers_future` then `cargo test --test check` AND `cargo test --no-default-features --test check` (the `examples/**` zero-`type-*` invariant in BOTH configs).
  Expected: PASS.

- [x] **Step 5: Commit**
```bash
git add src/check/infer/pass.rs
git commit -m "feat(infer): worker fn call synthesizes future<T> (like async); examples stay zero type-* in both configs

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: LSP — semantic tokens, hover, completion, diagnostics, navigation

**Files:**
- Modify: `src/lsp/providers/semantic_tokens.rs` (`is_keyword_kind` @230)
- Modify: `src/lsp/providers/completion.rs` (keyword list @25)
- Modify: `src/lsp/providers/hover.rs`
- Test: `tests/lsp.rs`

- [ ] **Step 1: Write the failing tests** — add to `tests/lsp.rs`:

```rust
#[test]
fn lsp_worker_is_keyword_token() {
    let toks = semantic_tokens("worker fn f() { return 1 }");
    assert!(toks.iter().any(|t| t.kind == "keyword" && t.text == "worker"));
}

#[test]
fn lsp_offers_worker_completion() {
    let items = completions_at("wor", 3); // cursor after "wor" at top level
    assert!(items.iter().any(|i| i.label == "worker"));
}

#[test]
fn lsp_hover_worker_fn_mentions_future() {
    let h = hover_at("worker fn render(s) { return s }\nlet x = render(1)", /* on render call */);
    assert!(h.contains("worker") && h.contains("future<"));
}

#[test]
fn lsp_worker_capture_flows_to_diagnostics() {
    let diags = lsp_diagnostics("let c = 0\nworker fn g(n) { return n + c }");
    assert!(diags.iter().any(|d| d.code.as_deref() == Some("worker-capture")));
}

#[test]
fn lsp_navigation_finds_worker_fn() {
    // go-to-def / references treat a worker fn as an ordinary named fn.
    let defs = goto_def("worker fn g() { return 1 }\nlet x = g()", /* on the g() call */);
    assert_eq!(defs.len(), 1);
}
```
(Use `tests/lsp.rs`'s existing helpers — `semantic_tokens`, `completions_at`, `hover_at`, `lsp_diagnostics`, `goto_def` — mirror current tests; adjust signatures to the file's actual helpers.)

- [ ] **Step 2: Run to verify failures**
  Run: `cargo test --test lsp lsp_worker`
  Expected: FAIL (`worker` not a keyword token / not completed / hover lacks future).

- [ ] **Step 3: Classify `WorkerKw` as a keyword token.** In `src/lsp/providers/semantic_tokens.rs` `is_keyword_kind` (@230, the closed match @249–258), add `| WorkerKw` next to `StaticKw`. (This is exactly the "future keyword fails the build here" hook from the comment @229 — satisfy it.)

- [ ] **Step 4: Offer `worker` completion.** In `src/lsp/providers/completion.rs`, add `"worker"` to the keyword list (@25, alongside `"async"`).

- [ ] **Step 5: Hover.** In `src/lsp/providers/hover.rs`, where it builds the hover for a fn/method via `infer::hover_type_at`, detect `is_worker` (reuse `crate::syntax::resolve::is_worker_fn` on the decl node) and prepend a line: `worker fn — runs in a pooled isolate; calls return future<T>`. The type line already renders `future<T>` from Task 11's inference.

- [ ] **Step 6: Navigation & diagnostics need no new code** — `worker fn` is an ordinary named fn (the resolver/index already cover it once the parser sets the flag) and `worker-capture` flows through `check::analyze` → the existing LSP diagnostic path. The Step-1 tests confirm this; if `goto_def`/references fail, the cause is a missing parser flag (re-check Task 2), not a new LSP code path.

- [ ] **Step 7: Run the tests**
  Run: `cargo test --test lsp lsp_worker`
  Expected: PASS.

- [ ] **Step 8: Commit**
```bash
git add src/lsp/providers/semantic_tokens.rs src/lsp/providers/completion.rs src/lsp/providers/hover.rs tests/lsp.rs
git commit -m "feat(lsp): worker semantic token + completion + hover (future<T>); diagnostics/nav reuse the existing path

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 13: REPL regression

**Files:**
- Test: `tests/cli.rs` (REPL piped-input test) or `src/repl.rs` test
- Modify: only if a gap surfaces (expected: none — `worker fn` uses braces; delimiter-depth buffering + session `Vm`/`Interp` persistence already cover it).

- [ ] **Step 1: Write the failing/regression test** — add to `tests/cli.rs` (mirror an existing REPL-via-stdin test):

```rust
#[test]
fn repl_accepts_multiline_worker_fn_and_calls_it() {
    let input = "worker fn sq(n) {\n  return n * n\n}\nprint(await sq(6))\n";
    let out = run_repl(input); // existing helper: pipes input to `ascript repl`
    assert!(out.contains("36"), "repl out: {out}");
}
```

- [ ] **Step 2: Run**
  Run: `cargo test --test cli repl_accepts_multiline_worker_fn`
  Expected: PASS (if not, the only plausible cause is `is_incomplete` not counting the worker-fn braces — fix by confirming `worker` doesn't perturb the delimiter-depth tokenizer in `src/repl.rs`; it shouldn't, since `worker` is a plain identifier to the depth counter).

- [ ] **Step 3: Commit**
```bash
git add tests/cli.rs
git commit -m "test(repl): worker fn multi-line entry + cross-line persistence regression

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 14: Example corpus (§11.4) — runnable, order-deterministic

**Files:**
- Create: `examples/workers_parallel_map.as`, `examples/workers_static_method.as`, `examples/workers_nested_inline.as`, `examples/workers_errors.as`
- Create: `examples/advanced/workers_sample_sort.as`, `examples/advanced/workers_monte_carlo.as`, `examples/advanced/workers_parse_files.as`
- Test: verified via `target/release/ascript run <file>` (and Task 15's all-modes gate)

- [ ] **Step 1: Write `examples/workers_parallel_map.as`** (canonical CPU-bound fan-out; order-deterministic via `gather`):
```
// Parallel map: square each block in its own isolate, gather in order.
import { gather } from "std/task"

worker fn square(n: number): number {
  return n * n
}

fn main() {
  let inputs = [1, 2, 3, 4, 5, 6, 7, 8]
  let results = await gather(inputs.map(square))
  print(results)            // [1, 4, 9, 16, 25, 36, 49, 64]
  print(results.sum())      // 204
}

await main()
```

- [ ] **Step 2: Write `examples/workers_static_method.as`**:
```
import { gather } from "std/task"

class Img {
  static worker fn encode(px: number): number {
    return px * 2 + 1
  }
}

fn main() {
  let pixels = [10, 20, 30]
  let encoded = await gather(pixels.map(Img.encode))
  print(encoded)            // [21, 41, 61]
}

await main()
```

- [ ] **Step 3: Write `examples/workers_nested_inline.as`** (inline nesting, no deadlock):
```
worker fn inner(n: number): number {
  return n + 1
}

worker fn outer(n: number): number {
  // Called from inside a pool isolate -> runs inline (no re-dispatch, no deadlock).
  let bumped = await inner(n)
  return bumped * 10
}

fn main() {
  print(await outer(4))     // (4+1)*10 = 50
}

await main()
```

- [ ] **Step 4: Write `examples/workers_errors.as`** (recoverable worker panic + sendability path error):
```
worker fn risky(n: number): number {
  if (n < 0) { panic("negative input") }
  return n * n
}

fn main() {
  print(await risky(5))                                  // 25

  let caught = recover(fn() { return await risky(-1) })
  print(caught[1] != nil)                                // true — panic recovered

  // Sendability: a closure cannot cross the boundary; the message names the path.
  worker fn takesObj(o): number { return 1 }
  let bad = recover(fn() { return await takesObj({ cb: fn() { return 1 } }) })
  print(bad[1].message.contains("cannot be sent to a worker at"))  // true
}

await main()
```

- [ ] **Step 5: Write `examples/advanced/workers_sample_sort.as`** (chunk → parallel sort → k-way merge; fully error-handled):
```
import { gather } from "std/task"

worker fn sortChunk(chunk: array<number>): array<number> {
  return chunk.sorted()
}

fn kwayMerge(chunks: array<array<number>>): array<number> {
  let cursors = chunks.map(fn(_) { return 0 })
  let out = []
  let total = chunks.map(fn(c) { return c.len() }).sum()
  while (out.len() < total) {
    let best = nil
    let bestIdx = -1
    for (i in 0..chunks.len()) {
      if (cursors[i] < chunks[i].len()) {
        let v = chunks[i][cursors[i]]
        if (best == nil || v < best) { best = v; bestIdx = i }
      }
    }
    out.push(best)
    cursors[bestIdx] = cursors[bestIdx] + 1
  }
  return out
}

fn main() {
  let data = [9, 3, 7, 1, 8, 2, 6, 4, 5, 0, 11, 10]
  let chunkSize = 4
  let chunks = []
  for (i in 0..data.len() step chunkSize) {
    chunks.push(data.slice(i, (i + chunkSize).min(data.len())))
  }
  let sortedChunks = await gather(chunks.map(sortChunk))
  print(kwayMerge(sortedChunks))   // 0..=11 in order
}

await main()
```
(Verify the exact stdlib method names — `sorted`, `slice`, `min`, `sum` — against `docs/content/stdlib/*` and adjust to the real API at implementation time; the example must run.)

- [ ] **Step 6: Write `examples/advanced/workers_monte_carlo.as`** (embarrassingly parallel π estimate; deterministic via fixed per-chunk seeds so the all-modes gate matches):
```
import { gather } from "std/task"

// Deterministic LCG per chunk so output is identical across engines & runs.
worker fn countInCircle(seed: number): number {
  let samples = 50000
  let state = seed
  let hits = 0
  for (i in 0..samples) {
    state = (state * 1103515245 + 12345) % 2147483648
    let x = state / 2147483648.0
    state = (state * 1103515245 + 12345) % 2147483648
    let y = state / 2147483648.0
    if (x * x + y * y <= 1.0) { hits = hits + 1 }
  }
  return hits
}

fn main() {
  let seeds = [1, 2, 3, 4, 5, 6, 7, 8]
  let hits = (await gather(seeds.map(countInCircle))).sum()
  let total = seeds.len() * 50000
  let pi = 4.0 * hits / total
  print("pi estimate: " + pi.toFixed(4))
}

await main()
```
(`toFixed` etc. — confirm the real numeric-format method; the point is a FIXED, deterministic estimate string so all four modes agree byte-for-byte.)

- [ ] **Step 7: Write `examples/advanced/workers_parse_files.as`** (parse N inputs in parallel, gather, error-handled — uses in-memory strings, NOT fs, so it is self-isolating and feature-independent):
```
import { gather } from "std/task"
import { parse } from "std/json"

worker fn parseDoc(text: string): array {
  let [doc, err] = parse(text)
  if (err != nil) { return [nil, err] }
  return [doc.id, nil]
}

fn main() {
  let docs = [
    "{\"id\": 1}",
    "{\"id\": 2}",
    "{\"id\": 3}",
  ]
  let results = await gather(docs.map(parseDoc))
  let ids = results.map(fn(r) { return r[0] })
  print(ids)               // [1, 2, 3]
}

await main()
```
(Confirm `std/json`'s `parse` signature/return — `[value, err]` per the typed-parse contract; adjust the destructure accordingly.)

- [ ] **Step 8: Build release + run each example**
  Run: `cargo build --release` then for each file `target/release/ascript run <file>` and confirm the documented output.
  Expected: each runs and prints the expected, order-deterministic output. Fix any stdlib-method-name mismatches against the real API.

- [ ] **Step 9: `ascript check` each example** to confirm zero `worker-capture` / `type-*` diagnostics:
  Run: `for f in examples/workers_*.as examples/advanced/workers_*.as; do target/release/ascript check "$f"; done`
  Expected: clean.

- [ ] **Step 10: Commit**
```bash
git add examples/workers_*.as examples/advanced/workers_*.as
git commit -m "examples: worker fn corpus (parallel map, static method, inline nesting, errors, sample-sort, monte-carlo, parse)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 15: All-modes execution wired into `vm_differential.rs`

**Files:**
- Modify: `tests/vm_differential.rs` (the whole-corpus gate @940; add the generic-VM + `.aso` passes for worker programs)
- Test: the new gate functions themselves

- [ ] **Step 1: Write the failing all-modes test** — add to `tests/vm_differential.rs`:

```rust
/// Workers §11.3: every worker example must produce IDENTICAL, order-deterministic
/// output across all four modes: tree-walker, specialized VM, generic VM, and
/// .aso-compiled. Worker programs are byte-identical by construction (gather +
/// ordered consume).
#[tokio::test]
async fn worker_examples_all_modes_byte_identical() {
    let root = env!("CARGO_MANIFEST_DIR");
    let worker_examples: Vec<String> = all_corpus_examples()
        .into_iter()
        .filter(|p| p.contains("workers_"))
        .collect();
    assert!(!worker_examples.is_empty(), "no worker examples found");
    for rel in worker_examples {
        let path = std::path::Path::new(root).join(&rel);
        let src = std::fs::read_to_string(&path).unwrap();
        if feature_unavailable_in_this_build(&src).await { continue; }
        let tw = ascript::run_source_exit(&src).await.expect("tree-walker");
        let spec = ascript::vm_run_source(&src).await.expect("specialized vm");
        let gen = ascript::vm_run_source_generic(&src).await.expect("generic vm");
        assert_eq!(tw, spec, "tree-walker vs specialized VM diverged for {rel}");
        assert_eq!(tw, gen, "tree-walker vs generic VM diverged for {rel}");
        // .aso mode: build to bytecode, run the .aso, compare.
        let aso_out = build_and_run_aso(&path).await;
        assert_eq!(tw.0, aso_out, ".aso output diverged for {rel}");
    }
}
```
(Add `build_and_run_aso(&Path) -> String`: spawn `ascript build <file> -o tmp.aso` then `ascript run tmp.aso`, capture stdout — mirror the binary-spawning helpers; if `vm_differential.rs` lacks binary spawning, add it via `env!("CARGO_BIN_EXE_ascript")`.)

- [ ] **Step 2: Run to verify it fails (or passes once examples + dispatch are correct)**
  Run: `cargo test --test vm_differential worker_examples_all_modes`
  Expected: initially FAIL if any mode diverges (a real bug — fix the engine/dispatch, NEVER weaken the assertion). PASS once worker dispatch is byte-identical across engines.

- [ ] **Step 3: Ensure the whole-corpus gate includes the worker examples.** The new `examples/workers_*.as` are auto-enumerated by `all_corpus_examples()` (@897). Confirm none are silently on `EXAMPLE_SKIPS` (@792); they must NOT be skipped (they are order-deterministic by design). Run `cargo test --test vm_differential vm_run_whole_corpus_matches_treewalker` and `vm_whole_corpus_skips_are_still_justified`.
  Expected: PASS; the worker examples count toward `ran`.

- [ ] **Step 4: Run both feature configs**
  Run: `cargo test --test vm_differential worker_examples_all_modes` and `cargo test --no-default-features --test vm_differential worker_examples_all_modes`
  Expected: PASS in both (worker subsystem is core; `std/json`-using examples are feature-skipped under `--no-default-features` via the existing mechanism).

- [ ] **Step 5: Commit**
```bash
git add tests/vm_differential.rs
git commit -m "test(differential): worker examples byte-identical across tree-walker, specialized VM, generic VM, and .aso

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 16: Performance benchmark harness (§11.5)

**Files:**
- Create: `bench/workers_bench.as` (the workload, in AScript, using `std/bench`)
- Create: `bench/run_workers_bench.sh` (drives `ASCRIPT_WORKERS=1,2,4,8`, payload sweep, cold-vs-warm; writes the report)
- Output: `bench/WORKERS_RESULTS.md` (generated; sibling to `bench/PROFILING_RESULTS.md`)

- [ ] **Step 1: Write `bench/workers_bench.as`** — a CPU-bound workload (Monte-Carlo / block-hash) timed with `std/bench`. The real API is `measure(fn, iterations?) -> {iterations, totalMs, avgMs, opsPerSec}` (it drives the returned Future each iteration), exported from `std/bench` (`src/stdlib/bench.rs::exports()`):
```
import { measure } from "std/bench"
import { gather } from "std/task"

worker fn work(seed: number): number {
  let n = 200000
  let s = seed
  for (i in 0..n) { s = (s * 1103515245 + 12345) % 2147483648 }
  return s % 1000
}

fn main() {
  let seeds = (1..=64).toArray()
  // Warm the pool first (cold-vs-warm split is measured by the shell driver).
  let _ = await gather(seeds.map(work))
  // `measure` runs the thunk N times and drives each returned future to completion.
  let r = measure(fn() { return gather(seeds.map(work)) }, 5)
  print("parallel avgMs: " + r.avgMs)
}

await main()
```
(The headline numbers are on the VM, the production engine; tree-walker numbers are informational.)

- [ ] **Step 2: Write `bench/run_workers_bench.sh`** — a bash driver:
  - Builds release once.
  - **Speedup vs cores:** runs the workload at `ASCRIPT_WORKERS` = 1, 2, 4, 8 (capped at host cores), records wall-clock; computes speedup vs the 1-worker baseline and parallel efficiency (speedup ÷ workers).
  - **Serialization overhead vs payload size:** a second `.as` (or a parameterized run) varying arg/result array size; records per-call round-trip cost vs payload bytes; identifies the break-even payload size.
  - **Pool warmup:** first-call (cold) vs steady-state (warm) latency.
  - Writes a Markdown table to `bench/WORKERS_RESULTS.md` with the measured figures and an Engine note (VM headline; tree-walker informational). Documented expectation: clear super-1× scaling (e.g. ≳3× on 4 cores for coarse work) — REPORTED, not a hard CI gate.

- [ ] **Step 3: Run the harness**
  Run: `bash bench/run_workers_bench.sh`
  Expected: produces `bench/WORKERS_RESULTS.md` with non-empty speedup/efficiency/overhead/warmup sections. Sanity-check that 4-worker wall-clock < 1-worker wall-clock on the host.

- [ ] **Step 4: Commit**
```bash
git add bench/workers_bench.as bench/run_workers_bench.sh bench/WORKERS_RESULTS.md
git commit -m "bench(worker): speedup-vs-cores, serialization-overhead-vs-payload, cold-vs-warm harness + report

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 17: Docs note (language guide) — minimal; full sweep is Plan B §8.2

**Files:**
- Modify: `docs/content/language/modules-async.md` (append a Workers section)
- Modify: `docs/assets/app.js` (NAV — only if a NEW page is added; this appends to an existing page, so verify NAV needs no change)
- Modify: `README.md` (one-line note in the feature/concurrency area)

- [ ] **Step 1: Append a Workers section** to `docs/content/language/modules-async.md` covering: the shared-nothing model; `worker fn` and `static worker fn` returning `future<T>`; the cost model (~0.5–2 ms birth, ~512 MB virtual stack, amortized to ~0 after warmup; "parallelize coarse work, not tight loops"); the capture rules (params + top-level fns + consts only; mutable-`let` capture / top-level mutation is a `worker-capture` Error); sendability (the structured-clone kinds + the field-path error + the channel/emitter hint); the pool (lazy, demand-grown to `num_cpus`, `ASCRIPT_WORKERS`, FIFO backpressure, inline nesting). Include a runnable snippet mirroring `examples/workers_parallel_map.as`.

- [ ] **Step 2: NAV check.** Since the content is appended to an EXISTING page (`modules-async`), the `NAV` array in `docs/assets/app.js` already lists it — confirm and make NO change. (If, instead, you create a NEW `workers` page, you MUST add its slug to `NAV` or it is unreachable — do not do this; Plan B owns the dedicated page if any.)

- [ ] **Step 3: README note.** Add one line in `README.md` where concurrency/stdlib is described: "`worker fn` runs CPU-bound work on a pooled shared-nothing isolate (multi-core), returning `future<T>`." Do NOT attempt the full README/CLAUDE.md/roadmap sweep — that is Plan B §8.2.

- [ ] **Step 4: Serve-and-eyeball (optional sanity)**
  Run: `cd docs && python3 -m http.server` and load the modules-async page; confirm the Workers section renders and its in-content links resolve.

- [ ] **Step 5: Commit**
```bash
git add docs/content/language/modules-async.md README.md
git commit -m "docs: workers section in the language guide (model, cost, capture, sendability, pool)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Final verification (before holistic review + merge)

- [ ] **Step 1:** `cargo test` — full suite green.
- [ ] **Step 2:** `cargo test --no-default-features` — core-only green (worker subsystem builds and serializer/dispatch tests pass with no stdlib).
- [ ] **Step 3:** `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` — both clean (no `await_holding_refcell_ref`).
- [ ] **Step 4:** `cargo test --test vm_differential` — whole-corpus + worker all-modes byte-identical in both configs.
- [ ] **Step 5:** `cargo test --test frontend_conformance --test treesitter_conformance` — both parsers + tree-sitter accept `worker`.
- [ ] **Step 6:** `cargo build --release && for f in examples/workers_*.as examples/advanced/workers_*.as; do target/release/ascript run "$f"; done` — every example runs.
- [ ] **Step 7:** `bash bench/run_workers_bench.sh` — report regenerated; speedup confirmed.
- [ ] **Step 8:** Holistic review (independent reviewer runs the commands + probes edges: a 10k-item `.map(worker)` doesn't spawn 10k threads; cancel-on-drop reclaims; nested at cap=1 doesn't deadlock; `async worker fn` parses; a class field named `worker` still works). Then merge `--no-ff` per the milestone workflow.

---

## Spec coverage map

| Spec A section | Covered by |
|---|---|
| §2 model (one keyword, intra/inter-isolate, sendability line) | Tasks 1–2 (keyword), 6 (sendability line) |
| §3 surface syntax (`worker fn`, `static worker fn`, call → `future<T>`, body may await, no `async worker`) | Tasks 1, 2, 4, 8, 11 |
| §4 capture & purity (params/top-fns/consts OK; mutable-let / top-mutation Error) | Task 10 (`worker-capture`) |
| §5 structured-clone serializer (sendable kinds, cycles, MapKey canon, class reconstruction, field-path rejection + hint, Value-layer) | Task 6 |
| §6 code shipping (dependency closure + bytecode, `.aso` reuse, per-isolate cache, per-engine slice) | Tasks 7 (closure/slice), 8 (cache) |
| §7 the pool (lazy compile-time + runtime gates, demand-grown to num_cpus / `ASCRIPT_WORKERS`, FIFO backpressure, inline nesting, cost model) | Tasks 8 (pool/inline), 9 (oversubscription/lazy proof), 17 (cost-model docs) |
| §8 cancellation & error propagation (cancel-on-drop across boundary, `[value,err]` as data, recoverable worker panic) | Task 9 |
| §9 determinism & differential oracle (per-Interp determinism unchanged; byte-identical across engines) | Task 15 |
| §10 implementation surface — front-ends (two parsers, `is_worker`) | Tasks 1, 2 |
| §10 — tree-sitter grammar + queries + regen + grammar sync + editor pins | Task 3 |
| §10 — editor integrations (VS Code TextMate, Zed, Neovim highlights, nvim spec) | Task 3 |
| §10 — formatter + ast Display (canonical `static? worker? fn`) | Task 4 |
| §10 — checker `worker-capture` (Error) | Task 10 |
| §10 — type inference (`worker fn` call → `future<T>`; examples zero `type-*`) | Task 11 |
| §10 — call-arity (`std_arity.rs`: none in Spec A core — confirmed no entry) | Task 11 note (no entries needed; documented) |
| §10 — LSP (semantic tokens, hover, diagnostics, navigation, completion) | Task 12 |
| §10 — REPL regression | Task 13 |
| §10 — runtime / new modules (`src/worker/*`, Send byte channels) | Tasks 6, 7, 8, 9 |
| §10 — `.aso` (`is_worker` in layout + `ASO_FORMAT_VERSION` bump + verify) | Task 5 |
| §10 — docs (workers page/section + NAV check + README) | Task 17 |
| §10 — tests (frontend/treesitter conformance, vm_differential both configs, check, lsp) | Tasks 1–3, 10–13, 15 |
| §11.1 unit & checker tests (serializer round-trip/cycles/classes/canon/rejection; worker-capture) | Tasks 6, 10 |
| §11.2 integration (parallel map+gather, oversubscription, nested inline, cancel-on-drop, worker panic, `[value,err]`, lazy-pool proof) | Tasks 8, 9 |
| §11.3 all-modes execution (tree-walker / specialized / generic / `.aso`) | Task 15 |
| §11.4 example corpus (4 intro + 3 advanced) | Task 14 |
| §11.5 performance measurement (speedup vs cores, serialization overhead vs payload, cold-vs-warm, report) | Task 16 |
| §12 scope & rejected alternatives (in-scope items realized; rejected ones not built) | whole plan (in-scope); rejected = explicitly NOT implemented |

**Confirmation:** every Spec A section (§2–§12, including the §10 cross-cutting checklist and the §11 testing/corpus/performance requirements) maps to at least one task above. The `call-arity`/`std_arity.rs` item is covered as a documented no-op (Spec A core exposes no new script-callable stdlib fn; the Spec B `pipe` helper registers there).
