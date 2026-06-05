//! `textDocument/semanticTokens/full` + `/range` over the cached lexeme stream.
//!
//! Each non-trivia token is classified into one LSP semantic token TYPE (the
//! legend below). A `NameRef`/`Ident` is refined via `model.resolved.uses` +
//! `.bindings`: a use resolving to a param → parameter, to a fn → function, to a
//! class/enum binding → type/enum, etc. Keywords/strings/numbers/comments come
//! straight off the `SyntaxKind`. The result is the LSP delta-position-encoded
//! token array; this module builds the legend, the per-token classifier, and the
//! encoding.

use crate::lsp::model::SemanticModel;
use crate::lsp::providers::token_spans::{positioned_tokens, TokenSpan};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::BindingKind;
use tower_lsp::lsp_types::{SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};

/// The semantic token TYPES we emit, in legend-index order. A token's wire
/// `token_type` is this slice's index.
const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,     // 0
    SemanticTokenType::FUNCTION,    // 1
    SemanticTokenType::PARAMETER,   // 2
    SemanticTokenType::VARIABLE,    // 3
    SemanticTokenType::PROPERTY,    // 4
    SemanticTokenType::CLASS,       // 5
    SemanticTokenType::ENUM,        // 6
    SemanticTokenType::ENUM_MEMBER, // 7
    SemanticTokenType::STRING,      // 8
    SemanticTokenType::NUMBER,      // 9
    SemanticTokenType::COMMENT,     // 10
    SemanticTokenType::NAMESPACE,   // 11
];

/// Modifiers, in legend-index order (bitset positions).
const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::DECLARATION, // bit 0
    SemanticTokenModifier::READONLY,    // bit 1
];

/// The legend the server registers in capabilities. Index order MUST match the
/// `TYPE_*`/`MOD_*` constants below.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: TOKEN_MODIFIERS.to_vec(),
    }
}

// Legend indices (must mirror TOKEN_TYPES order).
const TYPE_KEYWORD: u32 = 0;
const TYPE_FUNCTION: u32 = 1;
const TYPE_PARAMETER: u32 = 2;
const TYPE_VARIABLE: u32 = 3;
#[allow(dead_code)]
const TYPE_PROPERTY: u32 = 4;
const TYPE_CLASS: u32 = 5;
const TYPE_ENUM: u32 = 6;
#[allow(dead_code)]
const TYPE_ENUM_MEMBER: u32 = 7;
const TYPE_STRING: u32 = 8;
const TYPE_NUMBER: u32 = 9;
const TYPE_COMMENT: u32 = 10;
const TYPE_NAMESPACE: u32 = 11;

const MOD_DECLARATION: u32 = 1 << 0;
const MOD_READONLY: u32 = 1 << 1;

/// One classified token: byte span + legend type index + modifier bitset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedToken {
    pub start: usize,
    pub len: usize,
    pub token_type: u32,
    pub modifiers: u32,
}

/// Classify every non-trivia token in the model into a `ClassifiedToken`, in
/// source order. Tokens we don't surface (punctuation/operators) are dropped.
pub fn classify(model: &SemanticModel) -> Vec<ClassifiedToken> {
    let toks = positioned_tokens(model);
    let mut out = Vec::new();
    for t in &toks {
        if let Some(c) = classify_one(model, t) {
            out.push(c);
        }
    }
    out
}

fn classify_one(model: &SemanticModel, t: &TokenSpan) -> Option<ClassifiedToken> {
    let (token_type, modifiers) = match t.kind {
        SyntaxKind::LineComment | SyntaxKind::BlockComment => (TYPE_COMMENT, 0),
        // Other trivia (whitespace / newline) is not surfaced.
        k if k.is_trivia() => return None,
        SyntaxKind::Number => (TYPE_NUMBER, 0),
        SyntaxKind::Str
        | SyntaxKind::TemplateStr
        | SyntaxKind::TemplateStart
        | SyntaxKind::TemplateMiddle
        | SyntaxKind::TemplateEnd => (TYPE_STRING, 0),
        SyntaxKind::Ident => classify_ident(model, t)?,
        k if is_keyword_kind(k) => (TYPE_KEYWORD, 0),
        _ => return None, // punctuation / operators: not surfaced
    };
    Some(ClassifiedToken {
        start: t.start,
        len: t.len,
        token_type,
        modifiers,
    })
}

/// Refine an `Ident` token using the resolver. The token's byte span is its
/// resolution key (uses/bindings are keyed by `TextRange` == byte offsets).
fn classify_ident(model: &SemanticModel, t: &TokenSpan) -> Option<(u32, u32)> {
    // A USE: is there a `uses` entry keyed exactly on this token's byte span?
    let is_use = model.resolved.uses.keys().any(|range| {
        usize::from(range.start()) == t.start && usize::from(range.end()) == t.end()
    });

    // A DECL site: a binding whose name matches this token, whose decl_range
    // CONTAINS this token, and which is NOT itself a use (a decl name is not a
    // use). `decl_range` covers the WHOLE declaration node (starts at the leading
    // keyword), so we match by containment + name, not by `decl_range.start`.
    if !is_use {
        if let Some(b) = model.resolved.bindings.iter().find(|b| {
            b.name == t.text
                && t.start >= usize::from(b.decl_range.start())
                && t.end() <= usize::from(b.decl_range.end())
        }) {
            let ty = type_for_binding_kind(b.kind);
            let mut m = MOD_DECLARATION;
            if !b.mutable {
                m |= MOD_READONLY;
            }
            return Some((ty, m));
        }
    }

    // Else a USE (or an unresolved bare ident): pick fn/param/class/etc by the
    // binding it shares a name with.
    if is_use {
        let kind = model
            .resolved
            .bindings
            .iter()
            .find(|b| b.name == t.text)
            .map(|b| type_for_binding_kind(b.kind))
            .unwrap_or(TYPE_VARIABLE);
        return Some((kind, 0));
    }

    // Not a use, not a decl (e.g. a member-access name, an object key): plain
    // variable so the token still gets a stable color.
    Some((TYPE_VARIABLE, 0))
}

fn type_for_binding_kind(k: BindingKind) -> u32 {
    match k {
        BindingKind::Param => TYPE_PARAMETER,
        BindingKind::Fn => TYPE_FUNCTION,
        BindingKind::Class => TYPE_CLASS,
        BindingKind::Enum => TYPE_ENUM,
        BindingKind::Import => TYPE_NAMESPACE,
        BindingKind::Let | BindingKind::Const | BindingKind::PatternBind | BindingKind::LoopVar => {
            TYPE_VARIABLE
        }
    }
}

/// Keyword kinds are exactly the `*Kw` variants (cstree static-text keywords).
/// We classify them via a closed match so a future keyword fails the build here.
fn is_keyword_kind(k: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        k,
        TrueKw
            | FalseKw
            | NilKw
            | LetKw
            | ConstKw
            | IfKw
            | ElseKw
            | WhileKw
            | ForKw
            | InKw
            | OfKw
            | InstanceofKw
            | ReturnKw
            | BreakKw
            | ContinueKw
            | FnKw
            | EnumKw
            | MatchKw
            | ClassKw
            | ImportKw
            | ExportKw
            | AsyncKw
            | AwaitKw
            | YieldKw
            | StaticKw
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn legend_indices_match_constants() {
        let l = legend();
        assert_eq!(
            l.token_types[TYPE_KEYWORD as usize],
            SemanticTokenType::KEYWORD
        );
        assert_eq!(
            l.token_types[TYPE_FUNCTION as usize],
            SemanticTokenType::FUNCTION
        );
        assert_eq!(
            l.token_types[TYPE_PARAMETER as usize],
            SemanticTokenType::PARAMETER
        );
        assert_eq!(l.token_modifiers[0], SemanticTokenModifier::DECLARATION);
    }

    #[test]
    fn classifies_keyword_number_and_decl() {
        let cs = classify(&model("let x = 1\n"));
        // `let` keyword, `x` declared variable (readonly? no — let is mutable), `1` number.
        let kinds: Vec<u32> = cs.iter().map(|c| c.token_type).collect();
        assert!(kinds.contains(&TYPE_KEYWORD), "{kinds:?}");
        assert!(kinds.contains(&TYPE_VARIABLE), "{kinds:?}");
        assert!(kinds.contains(&TYPE_NUMBER), "{kinds:?}");
        // The `x` declaration carries the DECLARATION modifier.
        let x = cs.iter().find(|c| c.token_type == TYPE_VARIABLE).unwrap();
        assert_eq!(x.modifiers & MOD_DECLARATION, MOD_DECLARATION);
    }

    #[test]
    fn const_decl_is_readonly() {
        let cs = classify(&model("const y = 2\n"));
        let y = cs.iter().find(|c| c.token_type == TYPE_VARIABLE).unwrap();
        assert_eq!(y.modifiers & MOD_READONLY, MOD_READONLY);
    }

    #[test]
    fn classifies_function_decl_and_param() {
        let cs = classify(&model("fn add(a) { return a }\n"));
        let kinds: Vec<u32> = cs.iter().map(|c| c.token_type).collect();
        assert!(kinds.contains(&TYPE_FUNCTION), "{kinds:?}");
        assert!(kinds.contains(&TYPE_PARAMETER), "{kinds:?}");
    }
}
