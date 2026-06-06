# Checker — Scope & Control-Flow Lints (Plan C2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the name resolver into the analysis driver and add the first real lints — `undefined-variable`, `unused-binding`, `unused-import` (with fixes), `shadowing`, `unreachable-code`, `missing-return` — each conservative (no false positives on idiomatic code) and verified against the example corpus.

**Architecture:** `analyze` (Plan C1) gains a resolve step; each rule is a function `(&SyntaxNode, &ResolveResult) -> Vec<AsDiagnostic>` in `src/check/rules/`. The resolver (Plan 3) is amended to expose the per-binding data the lints need (`bindings` + `shadows`); the builtin allow-list is centralized in `interp` so `undefined-variable` and the runtime can't drift.

**Tech Stack:** Rust, the Plan 1/2/3 pipeline + Plan C1 checker core.

**Scope note:** Checker sub-project #4 (spec: `docs/superpowers/specs/2026-06-02-checker-design.md`). Depends on Plans 2, 3, and C1. AScript-specific lints (`unawaited-future`, …) are C3 (#5); contract checking is C4 (#6).

**Conservatism rule:** every lint here must produce **zero diagnostics on the clean example corpus** (Task 7 enforces it). When a rule can't be sure, it stays silent.

---

## File Structure

- Modify `src/syntax/resolve/types.rs` + `mod.rs` — expose `bindings: Vec<Binding>` on `ResolveResult`; add `shadows` to `Binding`.
- Modify `src/interp.rs` — extract `pub const BUILTIN_NAMES: &[&str]`.
- Modify `src/check/analyze.rs` — build CST, resolve, run rules; restructure so syntax diagnostics are computed before the tree consumes `Parse`.
- Create `src/check/rules/mod.rs` + one file per rule: `undefined.rs`, `unused.rs`, `shadowing.rs`, `unreachable.rs`, `missing_return.rs`.
- Modify `tests/check.rs` — corpus zero-false-positive guard.

---

## Task 1: Resolver amendments + builtin allow-list + wire resolve into analyze

**Files:**
- Modify: `src/syntax/resolve/types.rs`, `src/syntax/resolve/mod.rs`
- Modify: `src/interp.rs`
- Modify: `src/check/analyze.rs`

- [ ] **Step 1: Expose bindings + shadowing from the resolver**

In `src/syntax/resolve/types.rs`, add `shadows` to `Binding` and `bindings` to `ResolveResult`:

```rust
// in `pub struct Binding { ... }`, add:
    /// If this binding shadows an outer binding, the outer's decl range.
    pub shadows: Option<TextRange>,
```

```rust
// in `pub struct ResolveResult { ... }`, add:
    /// Every binding declared anywhere (across all frames), for the checker.
    pub bindings: Vec<Binding>,
```

In `src/syntax/resolve/mod.rs`:
- In `declare`, initialize `shadows`: before inserting, check whether the name is resolvable in an *enclosing* scope (current frame's outer scopes or any enclosing frame) and record it.

```rust
    fn declare(&mut self, name: &str, kind: BindingKind, decl_range: TextRange) -> u32 {
        // shadowing: resolvable in an enclosing scope/frame before we declare?
        let shadows = self.resolve_local(name).is_some()
            || self.resolve_upvalue_readonly(name);
        let shadow_range = if shadows { self.find_decl_range(name) } else { None };

        let slot = self.frame().next_slot;
        self.frame().next_slot += 1;
        self.frame().bindings.push(Binding {
            name: name.to_string(), kind, slot, decl_range,
            captured: false, mutated: false, use_count: 0,
            shadows: shadow_range,
        });
        self.scopes.last_mut().expect("scope").names.insert(name.to_string(), slot);
        slot
    }
```

- Add a non-mutating upvalue check + decl-range lookup helper:

```rust
    /// Like resolve_upvalue but read-only (does not mark captures) — for shadowing.
    fn resolve_upvalue_readonly(&self, name: &str) -> bool {
        // any enclosing frame has it as a local?
        (0..self.frames.len().saturating_sub(1))
            .any(|fi| self.resolve_local_in(fi, name).is_some())
    }
    /// The decl range of the nearest enclosing binding with `name`.
    fn find_decl_range(&self, name: &str) -> Option<TextRange> {
        for fi in (0..self.frames.len()).rev() {
            if let Some(slot) = self.resolve_local_in(fi, name) {
                if let Some(b) = self.frames[fi].bindings.iter().find(|b| b.slot == slot) {
                    return Some(b.decl_range);
                }
            }
        }
        None
    }
```

- In `resolve_function` and `resolve_file`, before discarding each popped frame, push its bindings into the result:

```rust
        let frame = self.frames.pop().unwrap();
        self.result.bindings.extend(frame.bindings.iter().cloned());
        self.result.frames.insert(frame.key, FrameInfo { /* …as before… */ });
```

> `shadows` is computed at `declare` time (before the new name is inserted), so it sees the *enclosing* binding only — exactly the shadow target. The `bindings` vector accumulates every frame's bindings as frames pop.

- [ ] **Step 2: Centralize the builtin allow-list in interp**

In `src/interp.rs`, replace the inline array in `global_env` with a referenced const so the checker and runtime share one source of truth:

```rust
/// The bare (unqualified) builtin names installed in every program's global env.
/// Shared with the checker (`undefined-variable`) so they cannot drift.
pub const BUILTIN_NAMES: &[&str] = &[
    "print", "Ok", "Err", "assert", "recover", "test", "len", "type", "range", "exit",
];
```

and in `global_env`, iterate `for name in BUILTIN_NAMES { ... }` instead of the inline literal.

- [ ] **Step 3: Wire resolve into `analyze`**

In `src/check/analyze.rs`, restructure `analyze` to compute syntax diagnostics first (before `Parse` is consumed), then build the tree, resolve, and run rules:

```rust
pub fn analyze(src: &str) -> Analysis {
    use crate::syntax::{parser, tree_builder, resolve};

    let parsed = parser::parse(src);
    // Syntax diagnostics use parsed.errors + parsed.tokens BEFORE the tree
    // consumes `parsed`.
    let mut diagnostics: Vec<AsDiagnostic> = parsed.errors.iter()
        .map(|err| AsDiagnostic {
            range: error_range(&parsed, err),
            severity: Severity::Error,
            code: "syntax-error".to_string(),
            message: err.message.clone(),
            fix: None,
        })
        .collect();

    // Build the tree + resolve, then run the lint rules.
    let tree = tree_builder::build_tree(parsed);
    let resolved = resolve::resolve(&tree);
    for rule in crate::check::rules::ALL {
        diagnostics.extend(rule(&tree, &resolved, src));
    }

    // Suppression + sort (config severity applied by the CLI/caller in C1/C3).
    let supp = suppressions(src);
    let line_starts = line_start_offsets(src);
    diagnostics.retain(|d| !supp.suppressed_on_line(line_of(&line_starts, d.range.start), &d.code));
    diagnostics.sort_by(|a, b| a.range.start.cmp(&b.range.start).then(a.code.cmp(&b.code)));
    Analysis { diagnostics }
}
```

Create `src/check/rules/mod.rs` with the rule registry (filled per task):

```rust
//! Lint rules. Each is `fn(&SyntaxNode, &ResolveResult, &str) -> Vec<AsDiagnostic>`.

use crate::check::diagnostic::AsDiagnostic;
use crate::syntax::cst::SyntaxNode;
use crate::syntax::resolve::types::ResolveResult;

pub mod undefined;
pub mod unused;
pub mod shadowing;
pub mod unreachable;
pub mod missing_return;

pub type Rule = fn(&SyntaxNode, &ResolveResult, &str) -> Vec<AsDiagnostic>;

/// All enabled rules. Each task appends its rule here.
pub static ALL: &[Rule] = &[
    undefined::check,
    unused::check,
    shadowing::check,
    unreachable::check,
    missing_return::check,
];
```

> Until each rule module exists, create empty stubs (`pub fn check(_,_,_) -> Vec<AsDiagnostic> { Vec::new() }`) so `ALL` compiles; each task replaces its stub. In `src/check/mod.rs` add `pub mod rules;`.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib check syntax::resolve 2>&1 | tail -15`
Expected: existing tests still PASS (rules are no-op stubs; `analyze` produces the same syntax diagnostics).

```bash
git add src/syntax/resolve/ src/interp.rs src/check/
git commit -m "feat(check): expose resolver bindings/shadows; centralize builtins; wire resolve into analyze"
```

---

## Task 2: `undefined-variable`

**Files:**
- Modify: `src/check/rules/undefined.rs`

- [ ] **Step 1: Tests**

Replace `src/check/rules/undefined.rs`:

```rust
//! `undefined-variable`: a NameRef the resolver classifies `Global` whose name
//! is neither a builtin, nor `self`/`super`, nor an imported/local binding.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::SyntaxNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{ResolveResult, Resolution};

/// Names always available even though the resolver marks them Global.
fn is_allowed_global(name: &str) -> bool {
    name == "self" || name == "super" || crate::interp::BUILTIN_NAMES.contains(&name)
}

pub fn check(tree: &SyntaxNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    let mut out = Vec::new();
    for n in tree.descendants().filter(|n| n.kind() == SyntaxKind::NameRef) {
        if let Some(Resolution::Global(name)) = resolved.uses.get(&n.text_range()) {
            if !is_allowed_global(name) {
                out.push(AsDiagnostic {
                    range: ByteSpan::from(n.text_range()),
                    severity: Severity::Warning,
                    code: "undefined-variable".to_string(),
                    message: format!("`{name}` is not defined"),
                    fix: None,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;

    fn codes(src: &str) -> Vec<String> {
        analyze(src).diagnostics.into_iter().map(|d| d.code).collect()
    }

    #[test]
    fn flags_genuinely_undefined() {
        assert!(codes("print(nope)\n").contains(&"undefined-variable".to_string()));
    }

    #[test]
    fn does_not_flag_builtins_locals_or_imports() {
        // print/len are builtins; x is local; t is an imported alias used.
        let src = "import * as t from \"std/task\"\nlet x = 1\nprint(len([x]))\nt.spawn\n";
        assert!(!codes(src).contains(&"undefined-variable".to_string()),
            "no undefined-variable expected: {:?}", codes(src));
    }

    #[test]
    fn does_not_flag_self_super() {
        // inside a method `self`/`super` are allowed even though Global.
        let src = "class C {\n  fn f() { return self }\n}\n";
        assert!(!codes(src).contains(&"undefined-variable".to_string()));
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib check::rules::undefined 2>&1 | tail -15`
Expected: PASS.

```bash
git add src/check/rules/undefined.rs
git commit -m "feat(check): undefined-variable lint"
```

---

## Task 3: `unused-binding` + `unused-import` (with fixes)

**Files:**
- Modify: `src/check/rules/unused.rs`

- [ ] **Step 1: Tests**

Replace `src/check/rules/unused.rs`:

```rust
//! `unused-binding` / `unused-import`: a binding with zero read uses. Parameters
//! are exempt (often intentionally unused). Imports/lets get a removal fix.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Fix, Severity, TextEdit};
use crate::syntax::cst::SyntaxNode;
use crate::syntax::resolve::types::{Binding, BindingKind, ResolveResult};

pub fn check(_tree: &SyntaxNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    let mut out = Vec::new();
    for b in &resolved.bindings {
        if b.use_count != 0 {
            continue;
        }
        match b.kind {
            BindingKind::Param => {} // params are exempt
            BindingKind::Import => out.push(unused(b, "unused-import", "remove unused import")),
            BindingKind::Let | BindingKind::Const | BindingKind::PatternBind => {
                out.push(unused(b, "unused-binding", "remove unused binding"))
            }
            // fn/class/enum/loop-var: skip (often public API / loop counters)
            _ => {}
        }
    }
    out
}

fn unused(b: &Binding, code: &str, fix_title: &str) -> AsDiagnostic {
    let range = ByteSpan::from(b.decl_range);
    AsDiagnostic {
        range,
        severity: Severity::Warning,
        code: code.to_string(),
        message: format!("`{}` is never used", b.name),
        // Conservative fix: replace the declared name's range with nothing is
        // unsafe (leaves dangling syntax); emit the fix as a no-op-safe removal of
        // the binding's full decl range. The decl_range here is the name token; a
        // safe whole-statement removal is computed by the CLI's --fix using the
        // enclosing statement. For now, attach a title-only fix marker.
        fix: Some(Fix { title: fix_title.to_string(), edits: vec![TextEdit { range, replacement: String::new() }] }),
    }
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn codes(src: &str) -> Vec<String> {
        analyze(src).diagnostics.into_iter().map(|d| d.code).collect()
    }
    #[test]
    fn flags_unused_let() {
        assert!(codes("let x = 1\n").contains(&"unused-binding".to_string()));
    }
    #[test]
    fn used_let_not_flagged() {
        assert!(!codes("let x = 1\nprint(x)\n").contains(&"unused-binding".to_string()));
    }
    #[test]
    fn flags_unused_import() {
        assert!(codes("import * as t from \"std/task\"\nprint(1)\n")
            .contains(&"unused-import".to_string()));
    }
    #[test]
    fn unused_param_is_exempt() {
        assert!(!codes("fn f(a) { return 1 }\nf(0)\n").contains(&"unused-binding".to_string()));
    }
}
```

> Fix scope: the `decl_range` is the *name token*; a clean removal needs the enclosing statement's range. This plan attaches a title + a name-range edit as a marker; the actual safe-removal edit (whole `import`/`let` statement) is computed when `--fix` lands (per the checker spec, `--fix` ships after a couple of fixable rules — the model carries the intent now). Keep the edit conservative (the `--fix` task will widen it to the statement range).

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib check::rules::unused 2>&1 | tail -15`
Expected: PASS.

```bash
git add src/check/rules/unused.rs
git commit -m "feat(check): unused-binding + unused-import lints"
```

---

## Task 4: `shadowing`

**Files:**
- Modify: `src/check/rules/shadowing.rs`

- [ ] **Step 1: Tests**

Replace `src/check/rules/shadowing.rs`:

```rust
//! `shadowing` (Hint): a binding whose name shadows an enclosing binding.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::SyntaxNode;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(_tree: &SyntaxNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    resolved.bindings.iter().filter(|b| b.shadows.is_some()).map(|b| AsDiagnostic {
        range: ByteSpan::from(b.decl_range),
        severity: Severity::Hint,
        code: "shadowing".to_string(),
        message: format!("`{}` shadows an outer binding", b.name),
        fix: None,
    }).collect()
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }
    #[test]
    fn flags_shadowing() {
        // inner `x` shadows outer `x`
        assert!(has("let x = 1\n{ let x = 2\n print(x) }\nprint(x)\n", "shadowing"));
    }
    #[test]
    fn no_shadow_no_flag() {
        assert!(!has("let x = 1\nlet y = 2\nprint(x)\nprint(y)\n", "shadowing"));
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib check::rules::shadowing 2>&1 | tail -15`
Expected: PASS.

```bash
git add src/check/rules/shadowing.rs
git commit -m "feat(check): shadowing lint (Hint)"
```

---

## Task 5: `unreachable-code`

**Files:**
- Modify: `src/check/rules/unreachable.rs`

- [ ] **Step 1: Tests**

Replace `src/check/rules/unreachable.rs`:

```rust
//! `unreachable-code`: statements following a `return`/`break`/`continue` in the
//! same block can never execute.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::SyntaxNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &SyntaxNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    // Every block-like node (SourceFile, Block) is a statement sequence.
    for block in tree.descendants().filter(|n| matches!(n.kind(), SourceFile | Block)) {
        let stmts: Vec<_> = block.children().filter(|c| is_stmt(c.kind())).collect();
        if let Some(term_idx) = stmts.iter().position(|s| is_terminator(s)) {
            // statements after the FIRST terminator are unreachable.
            if let Some(first_dead) = stmts.get(term_idx + 1) {
                out.push(AsDiagnostic {
                    range: ByteSpan::from(first_dead.text_range()),
                    severity: Severity::Warning,
                    code: "unreachable-code".to_string(),
                    message: "unreachable code".to_string(),
                    fix: None,
                });
            }
        }
    }
    out
}

fn is_terminator(node: &SyntaxNode) -> bool {
    matches!(node.kind(), SyntaxKind::ReturnStmt | SyntaxKind::BreakStmt | SyntaxKind::ContinueStmt)
}
fn is_stmt(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(kind, LetStmt | ExprStmt | Block | IfStmt | WhileStmt | ReturnStmt | FnDecl
        | ForStmt | BreakStmt | ContinueStmt | EnumDecl | ClassDecl | ImportStmt | ExportStmt)
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }
    #[test]
    fn flags_after_return() {
        assert!(has("fn f() { return 1\n print(2) }\nf()\n", "unreachable-code"));
    }
    #[test]
    fn no_unreachable_normal_flow() {
        assert!(!has("fn f() { print(1)\n return 2 }\nf()\n", "unreachable-code"));
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib check::rules::unreachable 2>&1 | tail -15`
Expected: PASS.

```bash
git add src/check/rules/unreachable.rs
git commit -m "feat(check): unreachable-code lint"
```

---

## Task 6: `missing-return` (conservative)

**Files:**
- Modify: `src/check/rules/missing_return.rs`

- [ ] **Step 1: Tests**

Replace `src/check/rules/missing_return.rs`:

```rust
//! `missing-return` (conservative): a function with a declared non-`nil` return
//! type whose body can fall off the end without returning. To avoid false
//! positives, a body is considered to return when it DEFINITELY returns (last
//! stmt is `return`, or an if/else where both branches definitely return, or
//! ends in an expression statement that may be the value). Uncertain → silent.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::SyntaxNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &SyntaxNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    for f in tree.descendants().filter(|n| matches!(n.kind(), FnDecl | MethodDecl)) {
        // Only check functions with a declared, non-nil return type.
        let Some(rt) = f.children().find(|c| c.kind() == RetType) else { continue };
        if returns_nil(&rt) {
            continue;
        }
        let Some(body) = f.children().find(|c| c.kind() == Block) else { continue };
        if !definitely_returns(&body) {
            // point at the function's name token for a clear location
            let range = f.text_range();
            out.push(AsDiagnostic {
                range: ByteSpan::from(range),
                severity: Severity::Warning,
                code: "missing-return".to_string(),
                message: "function with a declared return type may not return a value".to_string(),
                fix: None,
            });
        }
    }
    out
}

/// `: nil` (or `nil?`) return type → no value required.
fn returns_nil(ret_type: &SyntaxNode) -> bool {
    ret_type.text().to_string().contains("nil")
}

/// Conservative: a block DEFINITELY returns if its last statement is a `return`,
/// or an `if/else` whose both branches definitely return. An ending expression
/// statement is treated as a possible value (no false positive). Everything else
/// (ends in let/while/assignment/for) is treated as NOT definitely returning.
fn definitely_returns(block: &SyntaxNode) -> bool {
    use SyntaxKind::*;
    let last = block.children().filter(|c| is_block_stmt(c.kind())).last();
    match last.as_ref().map(|n| (n.kind(), n.clone())) {
        Some((ReturnStmt, _)) => true,
        Some((ExprStmt, _)) => true, // may be the implicit value; don't flag
        Some((IfStmt, n)) => {
            // both then-block and an else-block must definitely return
            let blocks: Vec<_> = n.children().filter(|c| c.kind() == Block).collect();
            let has_else = blocks.len() == 2 || n.children().any(|c| c.kind() == IfStmt);
            if !has_else {
                return false;
            }
            let then_ok = blocks.first().map(definitely_returns).unwrap_or(false);
            let else_ok = if let Some(elif) = n.children().find(|c| c.kind() == IfStmt) {
                // else-if chain: treat as a nested block requirement
                definitely_returns_ifchain(&elif)
            } else {
                blocks.get(1).map(definitely_returns).unwrap_or(false)
            };
            then_ok && else_ok
        }
        Some((Block, n)) => definitely_returns(&n),
        _ => false,
    }
}

fn definitely_returns_ifchain(if_stmt: &SyntaxNode) -> bool {
    use SyntaxKind::*;
    let blocks: Vec<_> = if_stmt.children().filter(|c| c.kind() == Block).collect();
    let then_ok = blocks.first().map(definitely_returns).unwrap_or(false);
    let else_ok = if let Some(elif) = if_stmt.children().find(|c| c.kind() == IfStmt) {
        definitely_returns_ifchain(&elif)
    } else {
        blocks.get(1).map(definitely_returns).unwrap_or(false)
    };
    then_ok && else_ok
}

fn is_block_stmt(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(kind, LetStmt | ExprStmt | Block | IfStmt | WhileStmt | ReturnStmt | FnDecl
        | ForStmt | BreakStmt | ContinueStmt)
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }
    #[test]
    fn flags_obvious_missing_return() {
        // declared : number but body ends in a let → cannot return a value
        assert!(has("fn f(): number { let x = 1 }\nf()\n", "missing-return"));
    }
    #[test]
    fn no_flag_when_returns() {
        assert!(!has("fn f(): number { return 1 }\nf()\n", "missing-return"));
    }
    #[test]
    fn no_flag_if_else_both_return() {
        assert!(!has("fn f(x): number { if x { return 1 } else { return 2 } }\nf(1)\n", "missing-return"));
    }
    #[test]
    fn no_flag_when_ends_in_expression() {
        // ends in a match/expr statement — treated as a possible value (no FP)
        assert!(!has("fn f(x): number { match x { _ => 0 } }\nf(1)\n", "missing-return"));
    }
}
```

> Deliberately conservative: ending in an `ExprStmt` (a bare `match`/call/expression that *might* be the value) is **not** flagged, so we never false-positive on value-ending bodies. Only clearly-non-returning endings (let/while/for/assignment) are flagged. This trades coverage for trust, per the checker's no-false-positives philosophy.

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib check::rules::missing_return 2>&1 | tail -15`
Expected: PASS.

```bash
git add src/check/rules/missing_return.rs
git commit -m "feat(check): missing-return lint (conservative)"
```

---

## Task 7: Corpus zero-false-positive guard

**Files:**
- Modify: `tests/check.rs`

- [ ] **Step 1: The guard**

Add to `tests/check.rs`:

```rust
//! The checker must NOT false-positive on idiomatic code: every example program
//! should produce zero diagnostics (or only ones a maintainer has suppressed in
//! the source). Any new false positive fails this and must be fixed (rule made
//! more conservative) or suppressed in the example with a reason.

use std::fs;
use std::path::{Path, PathBuf};

fn corpus() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { walk(&p, out); }
            else if p.extension().and_then(|x| x.to_str()) == Some("as") { out.push(p); }
        }
    }
    let mut v = Vec::new();
    walk(Path::new("examples"), &mut v);
    v.sort();
    v
}

#[test]
fn checker_is_clean_on_the_corpus() {
    use ascript::check::Severity;
    let mut offenders = Vec::new();
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        // The gate is about no false ERRORS/WARNINGS on idiomatic code. Advisory
        // Hint/Info (e.g. `shadowing`) may legitimately appear and are allowed.
        let actionable: Vec<_> = ascript::check::analyze(&src).diagnostics.into_iter()
            .filter(|d| matches!(d.severity, Severity::Error | Severity::Warning))
            .map(|d| format!("{}@{}", d.code, d.range.start))
            .collect();
        if !actionable.is_empty() {
            offenders.push(format!("{}: {:?}", path.display(), actionable));
        }
    }
    assert!(offenders.is_empty(),
        "checker false-positived (error/warning) on idiomatic examples (make the rule conservative or suppress with a reason):\n{}",
        offenders.join("\n"));
}
```

- [ ] **Step 2: Run + iterate to zero**

Run: `cargo test --test check checker_is_clean_on_the_corpus 2>&1 | tail -30`
Expected: PASS. Each offender names a file + rule + offset. For each: decide whether it's a **real** issue in the example (fix the example), or a **false positive** (make the rule more conservative — the common outcome for `missing-return`/`unused-binding` on idiomatic code), or an intentional case (add `// ascript-ignore[code]` with a comment in the example). Do not silence by disabling the rule globally.

- [ ] **Step 3: Full suite + clippy both configs**

Run: `cargo test 2>&1 | tail -15`
Expected: green.
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both (the rules live in the feature-independent `check` core).

- [ ] **Step 4: Commit**

```bash
git add tests/check.rs
git commit -m "test(check): corpus zero-false-positive guard for scope/control-flow lints"
```

---

## Done criteria for Plan C2

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs.
- [ ] `undefined-variable` flags genuinely-undefined names but not builtins/`self`/`super`/imports/locals.
- [ ] `unused-binding`/`unused-import` flag zero-use bindings (params exempt); `shadowing` flags enclosing-scope shadows (Hint).
- [ ] `unreachable-code` flags statements after a terminator; `missing-return` flags only clearly non-returning typed functions (no false positives).
- [ ] **The checker produces zero error/warning diagnostics on the clean example corpus** (conservatism gate; advisory `Hint`/`Info` allowed).
- [ ] The resolver exposes `bindings`/`shadows`; the builtin list is centralized; the interpreter/runtime are otherwise unchanged.

**Next plan:** `checker-ascript-specific-lints.md` (Plan C3, sub-project #5) — the AScript-specific lints: **`unawaited-future`** (the flagship — a script `async fn` / known future-returning call whose result is dropped, the exact M17 leak class), `ignored-result` (a statically-known `[value, err]` Result used as a bare statement without `?`/`!`/inspection), and `dead-recover`. Conservative detection via resolver + known-async signatures; same corpus zero-FP guard.
