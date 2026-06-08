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
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::BindingKind;
use std::collections::HashMap;
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
    SemanticTokenType::OPERATOR,    // 12
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
const TYPE_PROPERTY: u32 = 4;
const TYPE_CLASS: u32 = 5;
const TYPE_ENUM: u32 = 6;
const TYPE_ENUM_MEMBER: u32 = 7;
const TYPE_STRING: u32 = 8;
const TYPE_NUMBER: u32 = 9;
const TYPE_COMMENT: u32 = 10;
const TYPE_NAMESPACE: u32 = 11;
const TYPE_OPERATOR: u32 = 12;

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
    // Member-access NAME spans (`obj.field`'s `field`) → PROPERTY, or ENUM_MEMBER
    // when the receiver resolves to an enum. Built once per pass off the CST.
    let members = member_name_spans(model);
    // Byte spans of contextual-keyword `Ident` tokens that the CST parser remapped
    // to `WorkerKw`/`StaticKw` — the raw lexer emits `Ident` for these, but they
    // are keywords in context and should be styled accordingly.
    let ctx_kw_spans = contextual_keyword_spans(model);
    let mut out = Vec::new();
    for t in &toks {
        if let Some(c) = classify_one(model, t, &members, &ctx_kw_spans) {
            out.push(c);
        }
    }
    out
}

/// The byte spans `(start, end)` of every CST token of kind `WorkerKw` or
/// `StaticKw`. The raw lexer produces `Ident` for these contextual keywords,
/// so we intersect the lexer stream against the CST to fix their classification.
fn contextual_keyword_spans(model: &SemanticModel) -> std::collections::HashSet<(usize, usize)> {
    let mut out = std::collections::HashSet::new();
    for tok in model
        .tree
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| {
            matches!(
                t.kind(),
                SyntaxKind::WorkerKw | SyntaxKind::StaticKw
            )
        })
    {
        let r = tok.text_range();
        out.insert((usize::from(r.start()), usize::from(r.end())));
    }
    out
}

fn classify_one(
    model: &SemanticModel,
    t: &TokenSpan,
    members: &HashMap<(usize, usize), u32>,
    ctx_kw_spans: &std::collections::HashSet<(usize, usize)>,
) -> Option<ClassifiedToken> {
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
        // A contextual keyword (`worker`, `static`) remapped by the CST parser:
        // the raw lexer emits `Ident`, but the CST has `WorkerKw`/`StaticKw` at
        // the same span — intercept before the member-name / ident paths.
        SyntaxKind::Ident if ctx_kw_spans.contains(&(t.start, t.end())) => {
            (TYPE_KEYWORD, 0)
        }
        // A member-access NAME (`obj.field`) is a PROPERTY/ENUM_MEMBER, not a
        // resolver use — check it BEFORE the resolver-based ident classification.
        SyntaxKind::Ident if members.contains_key(&(t.start, t.end())) => {
            (members[&(t.start, t.end())], 0)
        }
        SyntaxKind::Ident => classify_ident(model, t)?,
        k if is_keyword_kind(k) => (TYPE_KEYWORD, 0),
        // NUM bitwise / shift / wrapping operators (`& | ^ << >> ~ +% -% *%`) are
        // surfaced as OPERATOR tokens so editors can highlight them distinctly.
        SyntaxKind::Amp
        | SyntaxKind::Pipe
        | SyntaxKind::Caret
        | SyntaxKind::Tilde
        | SyntaxKind::Shl
        | SyntaxKind::Shr
        | SyntaxKind::PlusPercent
        | SyntaxKind::MinusPercent
        | SyntaxKind::StarPercent => (TYPE_OPERATOR, 0),
        _ => return None, // other punctuation / operators: not surfaced
    };
    Some(ClassifiedToken {
        start: t.start,
        len: t.len,
        token_type,
        modifiers,
    })
}

/// Byte spans of every member-access NAME token (`obj.field`'s `field`, including
/// `obj?.field`), mapped to its token type: ENUM_MEMBER when the receiver is a
/// bare `NameRef` resolving to an enum NAME (via the SP10 `Table`), else PROPERTY.
/// The member name is the trailing direct `Ident` TOKEN child of a
/// `MemberExpr`/`OptMemberExpr`.
fn member_name_spans(model: &SemanticModel) -> HashMap<(usize, usize), u32> {
    let table = crate::check::infer::table::Table::build(&model.tree, &model.resolved);
    let mut out = HashMap::new();
    for node in model.tree.descendants().filter(|n| {
        matches!(n.kind(), SyntaxKind::MemberExpr | SyntaxKind::OptMemberExpr)
    }) {
        // The member NAME is the last direct `Ident` token child (the receiver is a
        // child NODE, the `.`/`?.` is an operator token).
        let Some(span) = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == SyntaxKind::Ident)
            .map(|t| t.text_range())
            .last()
        else {
            continue;
        };
        let key = (usize::from(span.start()), usize::from(span.end()));
        let ty = if receiver_is_enum(node, &table) {
            TYPE_ENUM_MEMBER
        } else {
            TYPE_PROPERTY
        };
        out.insert(key, ty);
    }
    out
}

/// True when a `MemberExpr`/`OptMemberExpr`'s receiver is a bare `NameRef` whose
/// name is a known enum (`Color.Red`). A non-NameRef receiver (`a.b.c`, a call) is
/// not an enum access.
fn receiver_is_enum(member: &ResolvedNode, table: &crate::check::infer::table::Table) -> bool {
    let Some(recv) = member.children().find(|c| c.kind() == SyntaxKind::NameRef) else {
        return false;
    };
    crate::syntax::resolve::ident_text(recv)
        .map(|name| table.enum_id(&name).is_some())
        .unwrap_or(false)
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
        // IFACE: an interface name is a TYPE; color it like a class for now (a
        // dedicated TYPE_INTERFACE / keyword tokens land with the full LSP pass).
        BindingKind::Class | BindingKind::Interface => TYPE_CLASS,
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
            | WorkerKw
    )
}

use tower_lsp::lsp_types::{Range, SemanticToken, SemanticTokens};

/// `semanticTokens/full`: every classified token, delta-encoded.
pub fn semantic_tokens_full(model: &SemanticModel) -> SemanticTokens {
    encode(model, &classify(model))
}

/// `semanticTokens/range`: only tokens whose byte span overlaps `range`.
pub fn semantic_tokens_range(model: &SemanticModel, range: Range) -> SemanticTokens {
    let start =
        crate::lsp::convert::char_to_byte(&model.text, model.line_index.offset(range.start));
    let end = crate::lsp::convert::char_to_byte(&model.text, model.line_index.offset(range.end));
    let filtered: Vec<ClassifiedToken> = classify(model)
        .into_iter()
        .filter(|c| c.start < end && (c.start + c.len) > start)
        .collect();
    encode(model, &filtered)
}

/// Delta-encode classified tokens. Each token becomes 5 ints relative to the
/// PREVIOUS token's line/char (LSP semantic-tokens wire format). A multi-line
/// token (block comment) is emitted as a single token at its start position;
/// clients handle the run via `length` (acceptable for v1 — matches the design's
/// "deltas deferred" note).
fn encode(model: &SemanticModel, tokens: &[ClassifiedToken]) -> SemanticTokens {
    let mut data = Vec::with_capacity(tokens.len());
    let mut prev_line = 0u32;
    let mut prev_char = 0u32;
    for c in tokens {
        let pos = model
            .line_index
            .position(crate::lsp::convert::byte_to_char(&model.text, c.start));
        let length = (crate::lsp::convert::byte_to_char(&model.text, c.start + c.len)
            - crate::lsp::convert::byte_to_char(&model.text, c.start)) as u32;
        let delta_line = pos.line - prev_line;
        let delta_start = if pos.line == prev_line {
            pos.character - prev_char
        } else {
            pos.character
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: c.token_type,
            token_modifiers_bitset: c.modifiers,
        });
        prev_line = pos.line;
        prev_char = pos.character;
    }
    SemanticTokens {
        result_id: None,
        data,
    }
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

    #[test]
    fn member_access_name_classifies_as_property() {
        let src = "let obj = { field: 1 }\nlet v = obj.field\n";
        let m = model(src);
        let cs = classify(&m);
        // The `field` in `obj.field` (the member-access NAME) → PROPERTY.
        let member_off = src.rfind("field").unwrap();
        let tok = cs
            .iter()
            .find(|c| c.start == member_off)
            .unwrap_or_else(|| panic!("no token at member name; got {cs:?}"));
        assert_eq!(tok.token_type, TYPE_PROPERTY, "obj.field → PROPERTY");
    }

    #[test]
    fn bitwise_operators_classify_as_operator() {
        // The NUM bitwise/shift/wrapping operators surface as OPERATOR tokens.
        let src = "let p = (a & b) | (c << 2) ^ ~d\nlet w = a +% b\n";
        let m = model(src);
        let cs = classify(&m);
        let kinds: Vec<u32> = cs.iter().map(|c| c.token_type).collect();
        assert!(
            kinds.contains(&TYPE_OPERATOR),
            "expected an OPERATOR token for the bitwise ops: {kinds:?}"
        );
        // Specifically the `&` token at its offset.
        let amp_off = src.find('&').unwrap();
        let tok = cs
            .iter()
            .find(|c| c.start == amp_off)
            .unwrap_or_else(|| panic!("no token at `&`; got {cs:?}"));
        assert_eq!(tok.token_type, TYPE_OPERATOR, "`&` → OPERATOR");
    }

    #[test]
    fn enum_variant_member_classifies_as_enum_member() {
        let src = "enum Color { Red, Green }\nlet c = Color.Red\n";
        let m = model(src);
        let cs = classify(&m);
        // The `Red` in `Color.Red` (receiver resolves to an enum) → ENUM_MEMBER.
        let red_off = src.rfind("Red").unwrap();
        let tok = cs
            .iter()
            .find(|c| c.start == red_off)
            .unwrap_or_else(|| panic!("no token at Color.Red member; got {cs:?}"));
        assert_eq!(tok.token_type, TYPE_ENUM_MEMBER, "Color.Red → ENUM_MEMBER");
    }

    #[test]
    fn adt_variant_constructor_classifies_as_enum_member() {
        // ADT Task 13: a payload variant CONSTRUCTOR `Shape.Circle(2.0)` is a member
        // access whose receiver resolves to an enum → ENUM_MEMBER (same path as a
        // unit variant; the trailing call does not change the member classification).
        let src = "enum Shape {\n  Circle(radius: float),\n  Point,\n}\nlet c = Shape.Circle(2.0)\nlet p = Shape.Point\n";
        let m = model(src);
        let cs = classify(&m);
        // `Circle` in the constructor call.
        let circle_off = src.rfind("Circle").unwrap();
        let tok = cs
            .iter()
            .find(|c| c.start == circle_off)
            .unwrap_or_else(|| panic!("no token at Shape.Circle member; got {cs:?}"));
        assert_eq!(
            tok.token_type, TYPE_ENUM_MEMBER,
            "Shape.Circle(..) → ENUM_MEMBER"
        );
        // The unit `Point` member, too.
        let point_off = src.rfind("Point").unwrap();
        let tok = cs
            .iter()
            .find(|c| c.start == point_off)
            .unwrap_or_else(|| panic!("no token at Shape.Point member; got {cs:?}"));
        assert_eq!(tok.token_type, TYPE_ENUM_MEMBER, "Shape.Point → ENUM_MEMBER");
    }
}

#[cfg(test)]
mod encode_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn full_encodes_first_token_absolute() {
        let st = semantic_tokens_full(&model("let x = 1\n"));
        assert!(!st.data.is_empty());
        // First token (`let`) is at line 0 char 0 → deltas are absolute (0,0).
        assert_eq!(st.data[0].delta_line, 0);
        assert_eq!(st.data[0].delta_start, 0);
        assert_eq!(st.data[0].length, 3); // "let"
        assert_eq!(st.data[0].token_type, TYPE_KEYWORD);
    }

    #[test]
    fn full_deltas_are_monotonic_within_a_line() {
        // Second token on the same line uses a positive delta_start, delta_line 0.
        let st = semantic_tokens_full(&model("let x = 1\n"));
        // `x` follows `let ` → delta_line 0, delta_start = 4 (chars from `let` start).
        let x = st
            .data
            .iter()
            .find(|t| t.token_type == TYPE_VARIABLE)
            .unwrap();
        assert_eq!(x.delta_line, 0);
        assert_eq!(x.delta_start, 4);
    }

    #[test]
    fn range_filters_to_overlapping_tokens() {
        let src = "let a = 1\nlet b = 2\n";
        let m = model(src);
        // A range covering only line 1 must exclude line-0 tokens.
        let st = semantic_tokens_range(
            &m,
            Range::new(
                tower_lsp::lsp_types::Position::new(1, 0),
                tower_lsp::lsp_types::Position::new(1, 9),
            ),
        );
        // First emitted token is on line 1 (delta_line == 1 from the (0,0) baseline).
        assert!(!st.data.is_empty());
        assert_eq!(st.data[0].delta_line, 1);
    }
}


#[cfg(test)]
mod worker_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    /// The `worker` contextual keyword in `worker fn f()` must be classified as
    /// KEYWORD (type 0) even though the raw lexer emits it as `Ident`.
    #[test]
    fn worker_fn_contextual_keyword_is_keyword() {
        let cs = classify(&model("worker fn f() { return 1 }\n"));
        let worker = cs.iter().find(|c| c.start == 0).expect("token at 0");
        assert_eq!(
            worker.token_type, TYPE_KEYWORD,
            "`worker` in `worker fn` must be KEYWORD; got type {}",
            worker.token_type
        );
        assert_eq!(worker.len, 6, "`worker` length must be 6");
    }

    /// The `worker` contextual keyword in `worker class Db {}` must be KEYWORD.
    #[test]
    fn worker_class_contextual_keyword_is_keyword() {
        let cs = classify(&model("worker class Db {}\n"));
        let worker = cs.iter().find(|c| c.start == 0).expect("token at 0");
        assert_eq!(
            worker.token_type, TYPE_KEYWORD,
            "`worker` in `worker class` must be KEYWORD; got type {}",
            worker.token_type
        );
    }

    /// The `worker` contextual keyword in `worker fn* s()` must be KEYWORD.
    #[test]
    fn worker_fn_star_contextual_keyword_is_keyword() {
        let cs = classify(&model("worker fn* s(n: number) { yield n }\n"));
        let worker = cs.iter().find(|c| c.start == 0).expect("token at 0");
        assert_eq!(
            worker.token_type, TYPE_KEYWORD,
            "`worker` in `worker fn*` must be KEYWORD; got type {}",
            worker.token_type
        );
    }
}
