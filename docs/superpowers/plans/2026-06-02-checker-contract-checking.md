# Checker — Conservative Contract Checking (Plan C4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `contract-mismatch` — a static check that flags **only provably-wrong** literal-argument-vs-annotated-parameter cases (a `string` literal where a `number` is declared; `nil` to a non-`T?` param), with **zero false positives**. It says nothing whenever a value's type isn't statically certain.

**Architecture:** One rule module, `src/check/rules/contract.rs`, appended to `rules::ALL`. It walks `CallExpr`s whose callee is a bare name resolving to a *uniquely-named, locally-declared* function with typed params, matches **literal** positional args to params, and reports a mismatch only when the literal's primitive kind is *provably incompatible* with the annotation. Everything uncertain (non-literal args, `any`, named/generic types, unions that could accept, spreads, rest params, method calls) → silent.

**Tech Stack:** Rust, the Plan 1/2/3 pipeline + Plan C1/C2/C3 checker core.

**Scope note:** Checker sub-project #6 — the **last** checker plan (spec: `docs/superpowers/specs/2026-06-02-checker-design.md`). Depends on Plans 2, 3, C1–C3. The **full inference-based type-checker** (flow narrowing, non-literal values, class-shape verification) is a recorded **future sub-project** — this plan is the trustworthy floor, not the ceiling.

**Philosophy (from the spec):** conservative — never flag when uncertain. A `contract-mismatch` that fires is *always* a real bug, so users trust it.

---

## File Structure

- Create `src/check/rules/contract.rs`.
- Modify `src/check/rules/mod.rs` — declare the module + append to `ALL`.
- (Corpus zero-false-positive guard from C2 covers it.)

---

## Task 1: The `contract-mismatch` rule

**Files:**
- Modify: `src/check/rules/mod.rs`
- Create: `src/check/rules/contract.rs`

- [ ] **Step 1: Register the module**

In `src/check/rules/mod.rs` add `pub mod contract;` and append `contract::check` to `ALL`.

- [ ] **Step 2: Write the rule + tests**

Create `src/check/rules/contract.rs`:

```rust
//! `contract-mismatch` (conservative): flag a literal argument that is PROVABLY
//! the wrong primitive for an annotated parameter — e.g. `f("x")` for
//! `fn f(n: number)`, or `nil` for a non-`T?` param. Silent on anything uncertain.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::SyntaxNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Resolution, ResolveResult};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LitKind { Number, String, Bool, Nil }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compat { Yes, No, Unknown }

pub fn check(tree: &SyntaxNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    // Map fn name → its FnDecl node, but ONLY for names declared exactly once
    // (ambiguous/overloaded-by-shadowing names are skipped — conservative).
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut by_name: HashMap<String, SyntaxNode> = HashMap::new();
    for f in tree.descendants().filter(|n| n.kind() == FnDecl) {
        if let Some(name) = fn_name(&f) {
            *counts.entry(name.clone()).or_default() += 1;
            by_name.insert(name, f);
        }
    }
    let unique = |name: &str| counts.get(name).copied() == Some(1);

    let mut out = Vec::new();
    for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else { continue };
        let name = crate::syntax::resolve::ident_text(&callee).unwrap_or_default();
        // callee must resolve to a local/upvalue binding (a user fn, not a builtin)
        // and be a uniquely-named declared function.
        let is_local = matches!(
            resolved.uses.get(&callee.text_range()),
            Some(Resolution::Local(_) | Resolution::Upvalue(_))
        );
        if !is_local || !unique(&name) {
            continue;
        }
        let Some(fn_decl) = by_name.get(&name) else { continue };

        let params = param_types(fn_decl);
        // If the fn has a rest param, only fixed positions are safe to check.
        let fixed = params.len();
        let Some(arg_list) = call.children().find(|c| c.kind() == ArgList) else { continue };
        // A spread arg makes positions uncertain → skip the whole call.
        if arg_list.children().any(|c| c.kind() == SpreadElem) {
            continue;
        }
        let args: Vec<_> = arg_list.children().filter(|c| is_expr(c.kind())).collect();

        for (i, arg) in args.iter().enumerate() {
            if i >= fixed {
                break; // beyond fixed params (rest) — unknown types
            }
            let Some(lit) = literal_kind(arg) else { continue }; // only literals
            let Some(ptype) = &params[i] else { continue };       // only annotated params
            if param_compat(ptype, lit) == Compat::No {
                out.push(AsDiagnostic {
                    range: ByteSpan::from(arg.text_range()),
                    severity: Severity::Warning,
                    code: "contract-mismatch".to_string(),
                    message: format!(
                        "argument {} of `{name}` is a {} literal but the parameter is declared `{}`",
                        i + 1, lit_name(lit), ptype.text().to_string().trim()
                    ),
                    fix: None,
                });
            }
        }
    }
    out
}

/// Per-parameter declared type node (None if a param is unannotated or is a rest).
fn param_types(fn_decl: &SyntaxNode) -> Vec<Option<SyntaxNode>> {
    use SyntaxKind::*;
    let Some(list) = fn_decl.children().find(|c| c.kind() == ParamList) else { return Vec::new() };
    list.children()
        .filter(|c| c.kind() == Param)
        // A rest param (`...x`) ends the fixed positions.
        .take_while(|p| !p.children_with_tokens().filter_map(|el| el.into_token()).any(|t| t.kind() == DotDotDot))
        .map(|p| p.children().find(|c| is_type(c.kind())))
        .collect()
}

fn literal_kind(arg: &SyntaxNode) -> Option<LitKind> {
    use SyntaxKind::*;
    match arg.kind() {
        TemplateExpr => Some(LitKind::String),
        Literal => {
            let t = arg.children_with_tokens().filter_map(|el| el.into_token())
                .find(|t| !t.kind().is_trivia())?;
            Some(match t.kind() {
                Number => LitKind::Number,
                Str => LitKind::String,
                TrueKw | FalseKw => LitKind::Bool,
                NilKw => LitKind::Nil,
                _ => return None,
            })
        }
        _ => None,
    }
}

/// Is the literal PROVABLY incompatible with the (possibly composite) type?
/// Yes = definitely accepts; No = definitely rejects (the only thing we flag);
/// Unknown = can't tell (any / named class / generic / partial union) → silent.
fn param_compat(ty: &SyntaxNode, lit: LitKind) -> Compat {
    use SyntaxKind::*;
    match ty.kind() {
        NamedType => match ty.text().to_string().trim() {
            "any" => Compat::Yes,
            "number" => prim(lit, LitKind::Number),
            "string" => prim(lit, LitKind::String),
            "bool" => prim(lit, LitKind::Bool),
            "nil" => prim(lit, LitKind::Nil),
            _ => Compat::Unknown, // a class / named type — unknowable from a literal
        },
        OptionalType => {
            if lit == LitKind::Nil {
                Compat::Yes // T? accepts nil
            } else if let Some(inner) = ty.children().find(|c| is_type(c.kind())) {
                param_compat(&inner, lit)
            } else {
                Compat::Unknown
            }
        }
        UnionType => {
            let members: Vec<_> = ty.children().filter(|c| is_type(c.kind())).collect();
            let mut all_no = !members.is_empty();
            for m in &members {
                match param_compat(m, lit) {
                    Compat::Yes => return Compat::Yes,   // any member accepts → accepts
                    Compat::Unknown => all_no = false,    // a member might accept → uncertain
                    Compat::No => {}
                }
            }
            if all_no { Compat::No } else { Compat::Unknown }
        }
        // array<T> / map / tuple / future: a scalar literal *could* be wrong, but
        // proving it requires more than a literal kind → stay silent.
        GenericType | TupleType => Compat::Unknown,
        _ => Compat::Unknown,
    }
}

/// A known-primitive annotation: matches the expected kind → Yes, else No
/// (every LitKind is a concrete primitive, so a mismatch is provable).
fn prim(lit: LitKind, expected: LitKind) -> Compat {
    if lit == expected { Compat::Yes } else { Compat::No }
}

fn lit_name(lit: LitKind) -> &'static str {
    match lit { LitKind::Number => "number", LitKind::String => "string", LitKind::Bool => "bool", LitKind::Nil => "nil" }
}

fn fn_name(fn_decl: &SyntaxNode) -> Option<String> {
    fn_decl.children_with_tokens().filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident).map(|t| t.text().to_string())
}

fn is_type(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(kind, NamedType | GenericType | OptionalType | UnionType | TupleType)
}

fn is_expr(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(kind, Literal | NameRef | UnaryExpr | BinaryExpr | ParenExpr | CallExpr | MemberExpr
        | IndexExpr | ArrowExpr | AssignExpr | ArrayExpr | ObjectExpr | TemplateExpr | OptMemberExpr
        | TryExpr | UnwrapExpr | TernaryExpr | AwaitExpr | YieldExpr | MatchExpr | RangeExpr)
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }

    #[test]
    fn flags_wrong_primitive_literal() {
        let src = "fn f(n: number) { return n }\nf(\"x\")\n";
        assert!(has(src, "contract-mismatch"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn flags_nil_to_non_optional() {
        let src = "fn f(n: number) { return n }\nf(nil)\n";
        assert!(has(src, "contract-mismatch"));
    }

    #[test]
    fn correct_literal_not_flagged() {
        let src = "fn f(n: number) { return n }\nf(42)\n";
        assert!(!has(src, "contract-mismatch"));
    }

    #[test]
    fn optional_accepts_nil() {
        let src = "fn f(n: number?) { return n }\nf(nil)\n";
        assert!(!has(src, "contract-mismatch"));
    }

    #[test]
    fn union_member_accepts() {
        let src = "fn f(x: number | string) { return x }\nf(\"ok\")\nf(1)\n";
        assert!(!has(src, "contract-mismatch"));
    }

    #[test]
    fn any_and_unannotated_and_nonliteral_silent() {
        // `any` accepts; unannotated param: silent; non-literal arg: silent.
        let src = "fn a(x: any) { return x }\nfn b(y) { return y }\nlet v = 1\nfn c(n: number) { return n }\na(\"s\")\nb(\"s\")\nc(v)\n";
        assert!(!has(src, "contract-mismatch"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn class_typed_param_is_silent() {
        // a named class type — a literal can't be proven wrong → silent.
        let src = "class User {}\nfn f(u: User) { return u }\nf(1)\n";
        assert!(!has(src, "contract-mismatch"));
    }
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test --lib check::rules::contract 2>&1 | tail -25`
Expected: PASS — wrong-primitive + nil-to-non-optional flagged; correct/optional/union/any/unannotated/non-literal/class-typed all silent (the zero-false-positive negatives).

```bash
git add src/check/rules/mod.rs src/check/rules/contract.rs
git commit -m "feat(check): conservative contract-mismatch (literal vs annotated param)"
```

---

## Task 2: Corpus guard + full suite

**Files:**
- (uses `tests/check.rs` from C2)

- [ ] **Step 1: Re-run the corpus zero-false-positive guard**

Run: `cargo test --test check checker_is_clean_on_the_corpus 2>&1 | tail -30`
Expected: PASS. `contract-mismatch` must produce **no** error/warning false positives on the corpus. If it fires on an example, it should be a **true** mismatch (fix the example) — by construction it only flags provably-wrong literals, so any firing is a real bug. If somehow uncertain, the rule is too aggressive → tighten it back to "Compat::No only" (it already is).

- [ ] **Step 2: Full suite + clippy both configs**

Run: `cargo test 2>&1 | tail -15`
Expected: green — the whole checker (syntax + all lint tiers) passing.
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both (the contract rule lives in the feature-independent `check` core).

- [ ] **Step 3: Commit (if any example fixed)**

```bash
git add examples/ 2>/dev/null; git commit -q -m "fix(examples): resolve contract-mismatch finding" || echo "no example changes needed"
```

---

## Done criteria for Plan C4 (and the checker)

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs.
- [ ] `contract-mismatch` flags a wrong-primitive literal arg and `nil`-to-non-optional, and is **silent** on correct literals, `T?`, accepting unions, `any`, named/class/generic types, unannotated params, and non-literal args.
- [ ] **Zero error/warning false positives on the corpus.**
- [ ] The interpreter/runtime are unchanged.

**Checker complete.** Sub-projects #3–#6 are fully planned: `ascript check` (CLI + LSP, all syntax errors), scope/control-flow lints, AScript-specific lints (incl. `unawaited-future`), and conservative contract checking. The original "fully powerful checker" ask is realized as a *trustworthy* checker (zero false positives) with a clear, recorded path to the full inference-based type-checker as a future sub-project.

**Remaining for the whole effort:** only the **bytecode VM + GC vertical-slice plans**, written **just-in-time** during execution (against the concrete typed-AST + resolver APIs that Plans 1–3 produce) — writing them now would be speculative against APIs that don't yet exist.
