# Checker — AScript-Specific Lints (Plan C3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the lints unique to AScript's footguns — **`unawaited-future`** (a dropped result of a call to a locally-declared `async fn`, the exact class of bug that caused M17's 130 MB leak), `ignored-result` (a dropped `[value, err]` Result from a function whose declared return type is `Result<…>`), and `dead-recover` (a `recover(fn)` whose body provably cannot panic) — all conservative (zero false positives on idiomatic code).

**Architecture:** Three more rule modules in `src/check/rules/`, each `fn(&SyntaxNode, &ResolveResult, &str) -> Vec<AsDiagnostic>` appended to `rules::ALL`. Detection uses purely *syntactic + resolver* signals (no type inference): an async/Result-returning callee identified by walking the CST for its declaration, and a "result dropped" shape identified by the call being the direct child of an `ExprStmt` (not under `await`/assign/return/`?`/`!`).

**Tech Stack:** Rust, the Plan 1/2/3 pipeline + Plan C1/C2 checker core.

**Scope note:** Checker sub-project #5 (spec: `docs/superpowers/specs/2026-06-02-checker-design.md`). Depends on Plans 2, 3, C1, C2. Contract checking is C4 (#6). Reuses C2's corpus zero-false-positive guard.

**Why these matter:** `unawaited-future` is the lint the whole checker effort was partly motivated by — a static guard against the un-awaited-async leak class M17 fixed at runtime via cancel-on-drop. Catching it *before* run is the win.

---

## File Structure

- Create `src/check/rules/unawaited.rs`, `src/check/rules/ignored_result.rs`, `src/check/rules/dead_recover.rs`.
- Modify `src/check/rules/mod.rs` — declare the modules + append to `ALL`.
- (Corpus guard already exists from C2; it now also covers these rules.)

**Shared detection helper** (define once, e.g. in `rules/mod.rs`): `fn dropped_call<'a>(expr_stmt: &'a SyntaxNode) -> Option<SyntaxNode>` — returns the `CallExpr` when an `ExprStmt`'s *direct* child is a `CallExpr` (the result is dropped: not `await`ed → would be under `AwaitExpr`; not `?`/`!` → under `TryExpr`/`UnwrapExpr`; not assigned/returned → under `AssignExpr`/`ReturnStmt`).

---

## Task 1: Shared `dropped_call` helper + `unawaited-future`

**Files:**
- Modify: `src/check/rules/mod.rs`
- Create: `src/check/rules/unawaited.rs`

- [ ] **Step 1: Add the shared helper + module wiring**

In `src/check/rules/mod.rs`, add the modules, the helper, and append to `ALL`:

```rust
pub mod unawaited;
pub mod ignored_result;
pub mod dead_recover;

/// The `CallExpr` directly dropped by an `ExprStmt` (result unused). `None` if the
/// statement's expression isn't a bare call (e.g. it's `await f()`, `x = f()`,
/// `f()?`, `f()!`, or `return f()` — those wrap the call in another node).
pub fn dropped_call(expr_stmt: &SyntaxNode) -> Option<SyntaxNode> {
    use crate::syntax::kind::SyntaxKind;
    if expr_stmt.kind() != SyntaxKind::ExprStmt {
        return None;
    }
    expr_stmt.children().find(|c| c.kind() == SyntaxKind::CallExpr)
}
```

and extend `ALL`:

```rust
pub static ALL: &[Rule] = &[
    undefined::check,
    unused::check,
    shadowing::check,
    unreachable::check,
    missing_return::check,
    unawaited::check,
    ignored_result::check,
    dead_recover::check,
];
```

> Stub `ignored_result`/`dead_recover` with no-op `check` until their tasks.

- [ ] **Step 2: Write `unawaited-future` tests**

Create `src/check/rules/unawaited.rs`:

```rust
//! `unawaited-future`: a dropped result of a call to a locally-declared `async fn`.
//! A script `async fn` call returns a `future<T>` that is eagerly scheduled but
//! cancelled-on-drop — dropping it (a bare call statement, not `await`ed) is the
//! M17 leak class and almost always a bug. Conservative: only locally-declared
//! async fns called by bare name (not methods, not stdlib detach like `task.spawn`).

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::check::rules::dropped_call;
use crate::syntax::cst::SyntaxNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Resolution, ResolveResult};
use std::collections::HashSet;

pub fn check(tree: &SyntaxNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    // names of locally-declared async functions
    let async_fns: HashSet<String> = tree
        .descendants()
        .filter(|n| n.kind() == FnDecl && is_async(n))
        .filter_map(fn_name)
        .collect();
    if async_fns.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for es in tree.descendants().filter(|n| n.kind() == ExprStmt) {
        let Some(call) = dropped_call(&es) else { continue };
        // callee must be a bare NameRef that resolves to a local/upvalue binding
        // (so a same-named global isn't mistaken) whose name is an async fn.
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else { continue };
        let name = crate::syntax::resolve::ident_text(&callee).unwrap_or_default();
        let is_local = matches!(
            resolved.uses.get(&callee.text_range()),
            Some(Resolution::Local(_) | Resolution::Upvalue(_))
        );
        if is_local && async_fns.contains(&name) {
            out.push(AsDiagnostic {
                range: ByteSpan::from(call.text_range()),
                severity: Severity::Warning,
                code: "unawaited-future".to_string(),
                message: format!("the future returned by `{name}` is dropped; did you mean `await {name}(...)`?"),
                fix: None,
            });
        }
    }
    out
}

fn is_async(fn_decl: &SyntaxNode) -> bool {
    fn_decl
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::AsyncKw)
}

fn fn_name(fn_decl: SyntaxNode) -> Option<String> {
    fn_decl
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }

    #[test]
    fn flags_dropped_async_call() {
        let src = "async fn work() { return 1 }\nfn main() { work() }\nmain()\n";
        assert!(has(src, "unawaited-future"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn awaited_call_not_flagged() {
        let src = "async fn work() { return 1 }\nasync fn main() { await work() }\n";
        assert!(!has(src, "unawaited-future"));
    }

    #[test]
    fn assigned_or_returned_not_flagged() {
        let src = "async fn work() { return 1 }\nfn a() { let f = work() }\nfn b() { return work() }\n";
        assert!(!has(src, "unawaited-future"));
    }

    #[test]
    fn non_async_call_not_flagged() {
        let src = "fn work() { return 1 }\nfn main() { work() }\nmain()\n";
        assert!(!has(src, "unawaited-future"));
    }
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test --lib check::rules::unawaited 2>&1 | tail -20`
Expected: PASS — dropped async call flagged; awaited/assigned/returned/non-async not.

```bash
git add src/check/rules/mod.rs src/check/rules/unawaited.rs
git commit -m "feat(check): unawaited-future lint (the M17 leak class, statically)"
```

---

## Task 2: `ignored-result`

**Files:**
- Modify: `src/check/rules/ignored_result.rs`

- [ ] **Step 1: Tests**

Replace `src/check/rules/ignored_result.rs`:

```rust
//! `ignored-result`: a dropped call to a function whose declared return type is
//! `Result<…>` — the `[value, err]` pair is discarded, so the error is silently
//! ignored. Conservative: only functions with an explicit `Result<…>` return
//! type (statically known); a `?`/`!`/assignment/return consumes the result and
//! is not flagged (those wrap the call, so `dropped_call` returns None).

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::check::rules::dropped_call;
use crate::syntax::cst::SyntaxNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Resolution, ResolveResult};
use std::collections::HashSet;

pub fn check(tree: &SyntaxNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    // names of functions whose declared return type is `Result<…>`
    let result_fns: HashSet<String> = tree
        .descendants()
        .filter(|n| n.kind() == FnDecl && returns_result(n))
        .filter_map(fn_name)
        .collect();
    if result_fns.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for es in tree.descendants().filter(|n| n.kind() == ExprStmt) {
        let Some(call) = dropped_call(&es) else { continue };
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else { continue };
        let name = crate::syntax::resolve::ident_text(&callee).unwrap_or_default();
        let is_local = matches!(
            resolved.uses.get(&callee.text_range()),
            Some(Resolution::Local(_) | Resolution::Upvalue(_))
        );
        if is_local && result_fns.contains(&name) {
            out.push(AsDiagnostic {
                range: ByteSpan::from(call.text_range()),
                severity: Severity::Warning,
                code: "ignored-result".to_string(),
                message: format!("the Result of `{name}` is ignored; handle it with `?`, `!`, or by inspecting `[value, err]`"),
                fix: None,
            });
        }
    }
    out
}

/// A `FnDecl` whose `RetType` is a `Result<…>` generic type.
fn returns_result(fn_decl: &SyntaxNode) -> bool {
    use SyntaxKind::*;
    let Some(rt) = fn_decl.children().find(|c| c.kind() == RetType) else { return false };
    rt.children().any(|t| {
        t.kind() == GenericType
            && t.children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|tk| tk.kind() == Ident)
                .map(|tk| tk.text() == "Result")
                .unwrap_or(false)
    })
}

fn fn_name(fn_decl: SyntaxNode) -> Option<String> {
    fn_decl
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }

    #[test]
    fn flags_dropped_result() {
        let src = "fn load(): Result<number> { return Ok(1) }\nfn main() { load() }\nmain()\n";
        assert!(has(src, "ignored-result"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn propagated_or_unwrapped_not_flagged() {
        let src = "fn load(): Result<number> { return Ok(1) }\nfn a(): Result<number> { load()?\n return Ok(2) }\nfn b() { load()! }\n";
        assert!(!has(src, "ignored-result"));
    }

    #[test]
    fn non_result_fn_not_flagged() {
        let src = "fn plain(): number { return 1 }\nfn main() { plain() }\nmain()\n";
        assert!(!has(src, "ignored-result"));
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib check::rules::ignored_result 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/check/rules/ignored_result.rs
git commit -m "feat(check): ignored-result lint (conservative, Result<…>-typed fns)"
```

---

## Task 3: `dead-recover` (very conservative)

**Files:**
- Modify: `src/check/rules/dead_recover.rs`

- [ ] **Step 1: Tests**

Replace `src/check/rules/dead_recover.rs`:

```rust
//! `dead-recover` (Hint): a `recover(fn)` whose body provably cannot panic, so the
//! recover is inert. VERY conservative: only flags when the arrow body contains no
//! calls (a call might panic), no `!` (force-unwrap can panic), and no field
//! assignments (a typed-field assignment can panic). Such a body can only produce
//! values that never panic, so the recover does nothing.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::SyntaxNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &SyntaxNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
        // callee must be the bare name `recover`
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else { continue };
        if crate::syntax::resolve::ident_text(&callee).as_deref() != Some("recover") {
            continue;
        }
        // first arg must be an arrow whose body cannot panic
        let Some(args) = call.children().find(|c| c.kind() == ArgList) else { continue };
        let Some(arrow) = args.children().find(|c| c.kind() == ArrowExpr) else { continue };
        if body_cannot_panic(&arrow) {
            out.push(AsDiagnostic {
                range: ByteSpan::from(call.text_range()),
                severity: Severity::Hint,
                code: "dead-recover".to_string(),
                message: "this `recover` wraps a body that cannot panic; it has no effect".to_string(),
                fix: None,
            });
        }
    }
    out
}

/// Conservative: no calls, no `!`, no member/index assignments anywhere in the body.
fn body_cannot_panic(arrow: &SyntaxNode) -> bool {
    use SyntaxKind::*;
    !arrow.descendants().any(|n| {
        matches!(n.kind(), CallExpr | UnwrapExpr)
            || (n.kind() == AssignExpr
                && n.children().next().map(|t| matches!(t.kind(), MemberExpr | IndexExpr)).unwrap_or(false))
    })
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }

    #[test]
    fn flags_inert_recover() {
        // body just returns a literal — cannot panic
        let src = "let r = recover(() => 1)\n";
        assert!(has(src, "dead-recover"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn recover_with_a_call_not_flagged() {
        let src = "fn risky() { return 1 }\nlet r = recover(() => risky())\n";
        assert!(!has(src, "dead-recover"));
    }

    #[test]
    fn recover_with_unwrap_not_flagged() {
        let src = "let r = recover(() => some_pair()!)\n";
        assert!(!has(src, "dead-recover"));
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib check::rules::dead_recover 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/check/rules/dead_recover.rs
git commit -m "feat(check): dead-recover lint (very conservative, Hint)"
```

---

## Task 4: Corpus guard + full suite

**Files:**
- (uses `tests/check.rs` from C2)

- [ ] **Step 1: Re-run the corpus zero-false-positive guard**

The async examples (`examples/concurrency.as`, `examples/advanced/*`, `structured_concurrency.as`) exercise `unawaited-future` and `ignored-result` against real code.

Run: `cargo test --test check checker_is_clean_on_the_corpus 2>&1 | tail -30`
Expected: PASS (no error/warning false positives). If `unawaited-future` fires on an example:
- If the example genuinely drops a future (a real latent bug or a deliberate detach), the *deliberate* case should use `task.spawn(...)` (which this lint does **not** flag, since it's a `spawn` call, not a bare async-fn call) — or, if intentional and idiomatic, add `// ascript-ignore[unawaited-future]` with a one-line reason in the example.
- If it's a true positive (a real missing `await` in an example), **fix the example** (this lint earning its keep on day one).

- [ ] **Step 2: Full suite + clippy both configs**

Run: `cargo test 2>&1 | tail -15`
Expected: green.
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

- [ ] **Step 3: Commit (if any example was fixed/suppressed)**

```bash
git add examples/ 2>/dev/null; git commit -q -m "fix(examples): resolve unawaited-future / ignored-result findings" || echo "no example changes needed"
```

---

## Done criteria for Plan C3

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs.
- [ ] `unawaited-future` flags a dropped call to a locally-declared `async fn`, but not awaited/assigned/returned calls, non-async calls, or `task.spawn` detach.
- [ ] `ignored-result` flags a dropped call to a `Result<…>`-returning fn, but not `?`/`!`/consumed results.
- [ ] `dead-recover` (Hint) flags only provably-inert `recover` bodies (no calls/`!`/field-assigns).
- [ ] **Zero error/warning false positives on the corpus** (any real finding is fixed in the example).
- [ ] The interpreter/runtime are unchanged.

**Next plan:** `checker-contract-checking.md` (Plan C4, sub-project #6) — the conservative `contract-mismatch` rule: flag only provably-wrong literal-vs-annotation cases (a `string` literal to a `number` param; `nil` to a non-`T?` param; a wrong-typed literal field in `init`/`.from`/`json.parse(_, Class)` with literal inputs). Zero false positives — silent whenever a value's type isn't statically certain. The full inference-based type-checker remains a future sub-project.
