//! `contract-mismatch` (conservative): flag a literal argument that is PROVABLY
//! the wrong primitive for an annotated parameter — e.g. `f("x")` for
//! `fn f(n: number)`, or `nil` for a non-`T?` param. Silent on anything uncertain.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::{code_range, fn_name};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Resolution, ResolveResult};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LitKind {
    Number,
    String,
    Bool,
    Nil,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compat {
    Yes,
    No,
    Unknown,
}

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    // Map fn name → its FnDecl node, but ONLY for names declared exactly once
    // (ambiguous/overloaded-by-shadowing names are skipped — conservative).
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut by_name: HashMap<String, ResolvedNode> = HashMap::new();
    for f in tree.descendants().filter(|n| n.kind() == FnDecl) {
        if let Some(name) = fn_name(f) {
            *counts.entry(name.clone()).or_default() += 1;
            by_name.insert(name, f.clone());
        }
    }
    let unique = |name: &str| counts.get(name).copied() == Some(1);

    let mut out = Vec::new();
    for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else {
            continue;
        };
        let name = crate::syntax::resolve::ident_text(callee).unwrap_or_default();
        // callee must resolve to a binding declared in this file — a local/upvalue
        // OR a module-scope user-global (a top-level `fn`, the common case) — and be
        // a uniquely-named declared function (so a bare builtin is excluded).
        let user_fn = match resolved.uses.get(&callee.text_range()) {
            Some(Resolution::Local(_) | Resolution::Upvalue(_)) => true,
            Some(Resolution::Global(gname)) => {
                resolved.bindings.iter().any(|b| b.is_global && &b.name == gname)
            }
            _ => false,
        };
        if !user_fn || !unique(&name) {
            continue;
        }
        let Some(fn_decl) = by_name.get(&name) else {
            continue;
        };

        let params = param_types(fn_decl);
        // If the fn has a rest param, only fixed positions are safe to check.
        let fixed = params.len();
        let Some(arg_list) = call.children().find(|c| c.kind() == ArgList) else {
            continue;
        };
        // A spread arg makes positions uncertain → skip the whole call.
        if arg_list.children().any(|c| c.kind() == SpreadElem) {
            continue;
        }
        let args: Vec<_> = arg_list.children().filter(|c| is_expr(c.kind())).collect();

        for (i, arg) in args.iter().enumerate() {
            if i >= fixed {
                break; // beyond fixed params (rest) — unknown types
            }
            let Some(lit) = literal_kind(arg) else {
                continue;
            }; // only literals
            let Some(ptype) = &params[i] else {
                continue;
            }; // only annotated params
            if param_compat(ptype, lit) == Compat::No {
                out.push(AsDiagnostic {
                    range: code_range(arg),
                    severity: Severity::Warning,
                    code: "contract-mismatch".to_string(),
                    message: format!(
                        "argument {} of `{name}` is a {} literal but the parameter is declared `{}`",
                        i + 1,
                        lit_name(lit),
                        ptype.text().to_string().trim()
                    ),
                    fix: None,
                });
            }
        }
    }
    out
}

/// Per-parameter declared type node (None if a param is unannotated or is a rest).
fn param_types(fn_decl: &ResolvedNode) -> Vec<Option<ResolvedNode>> {
    use SyntaxKind::*;
    let Some(list) = fn_decl.children().find(|c| c.kind() == ParamList) else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == Param)
        // A rest param (`...x`) ends the fixed positions.
        .take_while(|p| {
            !p.children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == DotDotDot)
        })
        .map(|p| p.children().find(|c| is_type(c.kind())).cloned())
        .collect()
}

fn literal_kind(arg: &ResolvedNode) -> Option<LitKind> {
    use SyntaxKind::*;
    match arg.kind() {
        TemplateExpr => Some(LitKind::String),
        Literal => {
            let t = arg
                .children_with_tokens()
                .filter_map(|el| el.into_token())
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
fn param_compat(ty: &ResolvedNode, lit: LitKind) -> Compat {
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
                param_compat(inner, lit)
            } else {
                Compat::Unknown
            }
        }
        UnionType => {
            let members: Vec<_> = ty.children().filter(|c| is_type(c.kind())).collect();
            let mut all_no = !members.is_empty();
            for m in &members {
                match param_compat(m, lit) {
                    Compat::Yes => return Compat::Yes, // any member accepts → accepts
                    Compat::Unknown => all_no = false, // a member might accept → uncertain
                    Compat::No => {}
                }
            }
            if all_no {
                Compat::No
            } else {
                Compat::Unknown
            }
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
    if lit == expected {
        Compat::Yes
    } else {
        Compat::No
    }
}

fn lit_name(lit: LitKind) -> &'static str {
    match lit {
        LitKind::Number => "number",
        LitKind::String => "string",
        LitKind::Bool => "bool",
        LitKind::Nil => "nil",
    }
}

fn is_type(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        NamedType | GenericType | OptionalType | UnionType | TupleType
    )
}

fn is_expr(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        Literal
            | NameRef
            | UnaryExpr
            | BinaryExpr
            | ParenExpr
            | CallExpr
            | MemberExpr
            | IndexExpr
            | ArrowExpr
            | AssignExpr
            | ArrayExpr
            | ObjectExpr
            | TemplateExpr
            | OptMemberExpr
            | TryExpr
            | UnwrapExpr
            | TernaryExpr
            | AwaitExpr
            | YieldExpr
            | MatchExpr
            | RangeExpr
    )
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
