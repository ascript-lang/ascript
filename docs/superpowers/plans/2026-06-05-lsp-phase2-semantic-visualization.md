# LSP Phase 2 — Semantic Visualization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the four *semantic-visualization* LSP capabilities on top of the Phase 0 `SemanticModel`: **semanticTokens** (`full` + `range`), **inlayHint** (inferred-type hints at un-annotated `let`s + parameter-name hints at call args, with `inlayHint/resolve`), **documentHighlight** (read/write occurrences of the identifier under the cursor), and **signatureHelp** (callee signature with the active parameter highlighted while typing a call). Every provider is a pure `fn(&SemanticModel, …)` reading the cached `model.tokens` / `model.resolved` / `check::infer` — no provider re-parses and none imports `crate::{ast,lexer,parser,token}`.

**Architecture:** Each capability is one new pure provider module under `src/lsp/providers/` reading the Phase 0 `SemanticModel` (`model.tokens` for the ordered lossless lexeme stream, `model.tree` for CST structure, `model.resolved.{uses,bindings}` for name resolution, and `crate::check::infer::{hover_type_at, table}` for inferred types). `model.tokens` (`Vec<LexToken>`) carries NO positions — a helper accumulates byte offsets by summing `LexToken.text.len()` across the stream (the lexer is lossless, so concatenated token texts reproduce the source exactly). The `Backend` in `src/lsp/server.rs` gains four handlers, each fetching the cached model via `store.get(&uri)` and converting positions with the Phase 0 `crate::lsp::convert` + `providers::docs::byte_offset_at` helpers. `server_capabilities()` advertises a `SemanticTokensLegend`, `inlay_hint_provider`, `document_highlight_provider`, and a `signature_help_provider` with trigger chars `(` and `,`.

**Tech Stack:** Rust, `tower-lsp`, `cstree` (red/green CST), the existing `src/syntax/` + `src/check/` crates. No interpreter, no `Rc`/`RefCell`/`Value` — the LSP stays `Send + Sync`.

**Reference (read before starting):**
- `docs/superpowers/specs/2026-06-05-lsp-first-class-design.md` §4 (capability matrix rows: semanticTokens, signatureHelp, inlayHint, documentHighlight), §6 Phase 2, §7 (testing — zero-FP corpus gate).
- `docs/superpowers/plans/2026-06-05-lsp-phase0-unification-foundation.md` — the `SemanticModel`/`DocumentStore`/`convert`/`providers` foundation this phase builds on. Reuse its types verbatim.
- `src/syntax/lexer.rs:9` — `LexToken { kind: SyntaxKind, text: String }` (NO position field).
- `src/syntax/kind.rs` — `SyntaxKind` (`Ident`, `NameRef`, `FnDecl`, `Param`, `ParamList`, `CallExpr`, `ArgList`, `LetStmt`, `ClassDecl`, `EnumDecl`, `EnumVariant`, `MethodDecl`, `FieldDecl`, `Colon`, `LParen`, `Comma`, …); `SyntaxKind::is_trivia()`.
- `src/syntax/resolve/types.rs` — `ResolveResult.uses: HashMap<TextRange, Resolution>`, `.bindings: Vec<Binding>`; `Resolution::{Local,Upvalue,Global,Unresolved}`; `Binding { name, kind: BindingKind, decl_range: TextRange, mutable, … }`; `BindingKind::{Let,Const,Param,Fn,Class,Enum,Import,PatternBind,LoopVar}`.
- `src/syntax/resolve/mod.rs:19` — `ident_text(node) -> Option<String>`; `:27` — `use_key`.
- `src/check/infer/mod.rs:37` — `hover_type_at(src, byte_offset) -> Option<String>`; `:42` — `collect_hover_types`.
- `src/check/infer/pass.rs:35` — `pub struct HoverType { pub range: ByteSpan, pub ty: String }`; `:43` — `collect_hover_types(tree, resolved, src, &table) -> Vec<HoverType>`.
- `src/check/infer/table.rs` — `Table::build(tree, resolved)`, `table.class_id(name)`, `table.class(id) -> Option<&ClassInfo>` (`fields`/`methods`), `table.enum_id`/`enum`.
- `src/check/diagnostic.rs:13` — `impl From<cstree::text::TextRange> for ByteSpan` (`start = usize::from(r.start())`).
- `src/lsp/convert.rs` (Phase 0) — `byte_to_char`, `byte_span_to_range(src, &line_index, ByteSpan)`.
- `src/lsp/providers/docs.rs` (Phase 0) — `byte_offset_at(model, Position) -> usize`.
- `src/lsp/workspace.rs` — CST-walk idioms: `tree.children()`, `tree.descendants()`, `node.children_with_tokens()`, `el.into_token()`, `node.text_range()`, `ByteSpan::from(node.text_range())`, `name_range_of` (the Ident-token finder pattern at `:648`).

**Run the whole suite with:** `cargo test --lib lsp` (LSP unit tests) and `cargo test` (full). Clippy gate: `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` must be clean.

> **API note on `LexToken` positions.** `LexToken` (`src/syntax/lexer.rs:9`) has only `kind` + `text` — NO byte offset. Because the lexer is lossless (`render` concatenates all token texts back to the source — `src/syntax/lexer.rs:25`), a provider derives each token's byte span by running a cumulative `offset += tok.text.len()` over `model.tokens` in order. Task 1 builds this once into a reusable `TokenSpan { start, len, kind, text }` helper so the three token-walking providers (semanticTokens, documentHighlight cursor-token lookup, signatureHelp) share it.

---

## File Structure

- Create `src/lsp/providers/token_spans.rs` — `TokenSpan` + `positioned_tokens(model)` (byte-positioned view over `model.tokens`); `token_at(model, offset)`.
- Create `src/lsp/providers/semantic_tokens.rs` — `SemanticTokensProvider`: `legend()`, `semantic_tokens_full(model)`, `semantic_tokens_range(model, Range)`; classification + LSP delta encoding.
- Create `src/lsp/providers/inlay.rs` — `inlay_hints(model, Range)` + `resolve(model, hint)`.
- Create `src/lsp/providers/highlight.rs` — `document_highlights(model, offset)`.
- Create `src/lsp/providers/signature.rs` — `signature_help(model, offset)`.
- Modify `src/lsp/providers/mod.rs` — declare the five new modules.
- Modify `src/lsp/server.rs` — four new handlers + capability registration (legend, inlay, highlight, signature-help trigger chars).

---

## Task 1: `token_spans.rs` — byte-positioned view over `model.tokens`

`LexToken` carries no position. This task builds the shared positioned view the token-walking providers reuse.

**Files:**
- Create: `src/lsp/providers/token_spans.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod token_spans;`)
- Test: inline in `src/lsp/providers/token_spans.rs`

- [ ] **Step 1: Declare the module**

In `src/lsp/providers/mod.rs`, add alongside the existing `pub mod` lines:

```rust
pub mod token_spans;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/providers/token_spans.rs`:

```rust
//! A byte-positioned view over `model.tokens`. `LexToken` (src/syntax/lexer.rs)
//! carries only `kind` + `text` and NO offset, but the lexer is lossless
//! (concatenated token texts reproduce the source), so a single cumulative pass
//! assigns each token its byte span. Shared by `semantic_tokens`, `highlight`,
//! and `signature` (every token-walking provider).

use crate::lsp::model::SemanticModel;
use crate::syntax::kind::SyntaxKind;

/// One token with its byte span. `start..start+len` indexes into `model.text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSpan {
    pub kind: SyntaxKind,
    pub start: usize,
    pub len: usize,
    pub text: String,
}

impl TokenSpan {
    pub fn end(&self) -> usize {
        self.start + self.len
    }
}

/// The model's lexeme stream with byte spans assigned by a cumulative pass.
pub fn positioned_tokens(model: &SemanticModel) -> Vec<TokenSpan> {
    let mut out = Vec::with_capacity(model.tokens.len());
    let mut offset = 0usize;
    for t in &model.tokens {
        let len = t.text.len();
        out.push(TokenSpan {
            kind: t.kind,
            start: offset,
            len,
            text: t.text.clone(),
        });
        offset += len;
    }
    out
}

/// The non-trivia token whose byte span CONTAINS `offset` (`start <= offset <
/// end`), or — when `offset` sits exactly at a token boundary (e.g. the cursor is
/// just after the last char of an identifier) — the token ENDING at `offset`.
/// Trivia tokens are skipped so the cursor "snaps" to the nearest real lexeme.
pub fn token_at(model: &SemanticModel, offset: usize) -> Option<TokenSpan> {
    let toks = positioned_tokens(model);
    // Prefer a token strictly containing the offset.
    if let Some(t) = toks
        .iter()
        .find(|t| !t.kind.is_trivia() && offset >= t.start && offset < t.end())
    {
        return Some(t.clone());
    }
    // Else a non-trivia token ENDING exactly at the offset (cursor after the name).
    toks.iter()
        .rev()
        .find(|t| !t.kind.is_trivia() && t.end() == offset)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn positions_reconstruct_source() {
        let src = "let x = 1\nprint(x)\n";
        let m = model(src);
        let toks = positioned_tokens(&m);
        // Concatenated texts equal the source (losslessness preserved).
        let joined: String = toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, src);
        // Each token's span slices the right text out of model.text.
        for t in &toks {
            assert_eq!(&m.text[t.start..t.end()], t.text);
        }
    }

    #[test]
    fn token_at_finds_identifier() {
        let src = "let value = 1\n";
        let m = model(src);
        // byte 4 is inside "value".
        let t = token_at(&m, 4).expect("token");
        assert_eq!(t.kind, SyntaxKind::Ident);
        assert_eq!(t.text, "value");
        // byte 9 is exactly the end of "value" (cursor just after it).
        let t2 = token_at(&m, 9).expect("token at boundary");
        assert_eq!(t2.text, "value");
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --lib lsp::providers::token_spans`
Expected: FAILS TO COMPILE first if `model.tokens` is not `Vec<LexToken>` — confirm against Phase 0 `SemanticModel` (`tokens: Vec<LexToken>`). Once compiling, tests should PASS (pure). If `LexToken.text` is not `String`, adapt the `.len()`/`.clone()` calls to the real type.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib lsp::providers::token_spans`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/lsp/providers/token_spans.rs src/lsp/providers/mod.rs
git commit -m "feat(lsp): token_spans — byte-positioned view over the cached lexeme stream"
```

---

## Task 2: `semantic_tokens.rs` — legend + classification

Classify each non-trivia token into an LSP semantic token type, then (Task 3) delta-encode. This task builds the legend and the per-token classifier.

**Files:**
- Create: `src/lsp/providers/semantic_tokens.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod semantic_tokens;`)
- Test: inline in `src/lsp/providers/semantic_tokens.rs`

- [ ] **Step 1: Declare the module**

In `src/lsp/providers/mod.rs` add:

```rust
pub mod semantic_tokens;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/providers/semantic_tokens.rs`. The legend's index ORDER is the contract — a token's `token_type` field is the index into `legend().token_types`. Build a stable ordered list and an enum mapping into it.

```rust
//! `textDocument/semanticTokens/full` + `/range` over the cached lexeme stream.
//!
//! Each non-trivia token is classified into one LSP semantic token TYPE (the
//! legend below). A `NameRef`/`Ident` is refined via `model.resolved.uses` +
//! `.bindings`: a use resolving to a param → parameter, to a fn → function, to a
//! class/enum binding → type/enum, etc. Keywords/strings/numbers/comments come
//! straight off the `SyntaxKind`. The result is the LSP delta-position-encoded
//! `Vec<u32>` (Task 3); this task builds the legend + per-token classifier.

use crate::lsp::model::SemanticModel;
use crate::lsp::providers::token_spans::{positioned_tokens, TokenSpan};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{BindingKind, Resolution};
use tower_lsp::lsp_types::{SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};

/// The semantic token TYPES we emit, in legend-index order. A token's wire
/// `token_type` is this slice's index.
const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,    // 0
    SemanticTokenType::FUNCTION,   // 1
    SemanticTokenType::PARAMETER,  // 2
    SemanticTokenType::VARIABLE,   // 3
    SemanticTokenType::PROPERTY,   // 4
    SemanticTokenType::CLASS,      // 5
    SemanticTokenType::ENUM,       // 6
    SemanticTokenType::ENUM_MEMBER,// 7
    SemanticTokenType::STRING,     // 8
    SemanticTokenType::NUMBER,     // 9
    SemanticTokenType::COMMENT,    // 10
    SemanticTokenType::NAMESPACE,  // 11
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
        k if k.is_trivia() && !matches!(k, SyntaxKind::LineComment | SyntaxKind::BlockComment) => {
            return None
        }
        SyntaxKind::LineComment | SyntaxKind::BlockComment => (TYPE_COMMENT, 0),
        SyntaxKind::Number => (TYPE_NUMBER, 0),
        SyntaxKind::Str | SyntaxKind::TemplateStr | SyntaxKind::TemplateStart
        | SyntaxKind::TemplateMiddle | SyntaxKind::TemplateEnd => (TYPE_STRING, 0),
        SyntaxKind::Ident => classify_ident(model, t)?,
        // Any keyword token: cstree static-text keyword kinds all end in `Kw`.
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
    // Is this the DECL site of a binding? (decl_range covers the whole decl, but
    // its NAME token range starts at the binding's name; match by name-token start.)
    if let Some(b) = model
        .resolved
        .bindings
        .iter()
        .find(|b| binding_name_start(b) == t.start && b.name == t.text)
    {
        let ty = type_for_binding_kind(b.kind);
        let mut m = MOD_DECLARATION;
        if !b.mutable {
            m |= MOD_READONLY;
        }
        return Some((ty, m));
    }
    // Else a USE: look up its resolution by the token's byte span.
    let res = model.resolved.uses.iter().find_map(|(range, r)| {
        if usize::from(range.start()) == t.start && usize::from(range.end()) == t.end() {
            Some(r.clone())
        } else {
            None
        }
    });
    match res {
        Some(Resolution::Local(_)) | Some(Resolution::Upvalue(_)) | Some(Resolution::Global(_)) => {
            // Find the binding it refers to (by name) to pick fn/param/class/etc.
            let kind = model
                .resolved
                .bindings
                .iter()
                .find(|b| b.name == t.text)
                .map(|b| type_for_binding_kind(b.kind))
                .unwrap_or(TYPE_VARIABLE);
            Some((kind, 0))
        }
        _ => Some((TYPE_VARIABLE, 0)), // unresolved bare ident → plain variable
    }
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

/// The byte START of a binding's NAME token. `decl_range` covers the whole decl;
/// the name token starts after the leading keyword + whitespace. We approximate
/// the name start as the first `Ident`-text match within the decl range via the
/// resolver's `bindings` already storing the name; here we use `decl_range.start`
/// adjusted by scanning is unnecessary because the classifier matches on the
/// USE-range path for references — the decl path matches by `decl_range` START.
fn binding_name_start(b: &crate::syntax::resolve::types::Binding) -> usize {
    usize::from(b.decl_range.start())
}

/// Keyword kinds are exactly the `*Kw` variants (cstree static-text keywords).
/// We classify them via a closed match so a future keyword fails the build here.
fn is_keyword_kind(k: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        k,
        TrueKw | FalseKw | NilKw | LetKw | ConstKw | IfKw | ElseKw | WhileKw | ForKw | InKw
            | OfKw | InstanceofKw | ReturnKw | BreakKw | ContinueKw | FnKw | EnumKw | MatchKw
            | ClassKw | ImportKw | ExportKw | AsyncKw | AwaitKw | YieldKw | StaticKw
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
        assert_eq!(l.token_types[TYPE_KEYWORD as usize], SemanticTokenType::KEYWORD);
        assert_eq!(l.token_types[TYPE_FUNCTION as usize], SemanticTokenType::FUNCTION);
        assert_eq!(l.token_types[TYPE_PARAMETER as usize], SemanticTokenType::PARAMETER);
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
```

Adapt the binding/use matching to the REAL key shapes: `uses` keys on `TextRange` (`usize::from(range.start())`), and a `Binding`'s name-token start may not equal `decl_range.start()` for keyworded decls (`let x` — the decl range starts at `let`). If a decl test fails because `binding_name_start` is off, switch the decl-detection to: a token is a decl site iff there is a binding whose `name == t.text` AND `t.start` falls within `[decl_range.start, decl_range.end)` AND no `uses` entry keys on this token's span (a decl name is not a use). The `add`/`a` test pins the resolver-refinement path regardless.

- [ ] **Step 3: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::semantic_tokens`
Expected: PASS once classification matches the real resolver keys. If `classifies_function_decl_and_param` fails on the param, confirm a `Param` name produces a `BindingKind::Param` binding (`src/syntax/resolve/types.rs:18` + the resolver records params as bindings).

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/semantic_tokens.rs src/lsp/providers/mod.rs
git commit -m "feat(lsp): semantic-token legend + per-token classification off the resolver"
```

---

## Task 3: semanticTokens delta encoding + `full`/`range`

Encode the classified tokens into the LSP delta-position `Vec<u32>` (5 ints per token: `deltaLine, deltaStartChar, length, tokenType, tokenModifiers`). Positions are UTF-16 line/char, so reuse the model's `line_index` + `convert`.

**Files:**
- Modify: `src/lsp/providers/semantic_tokens.rs`
- Test: inline in `src/lsp/providers/semantic_tokens.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/semantic_tokens.rs`:

```rust
use tower_lsp::lsp_types::{Range, SemanticToken, SemanticTokens};

/// `semanticTokens/full`: every classified token, delta-encoded.
pub fn semantic_tokens_full(model: &SemanticModel) -> SemanticTokens {
    encode(model, &classify(model))
}

/// `semanticTokens/range`: only tokens whose byte span overlaps `range`.
pub fn semantic_tokens_range(model: &SemanticModel, range: Range) -> SemanticTokens {
    let start = char_to_byte(&model.text, model.line_index.offset(range.start));
    let end = char_to_byte(&model.text, model.line_index.offset(range.end));
    let filtered: Vec<ClassifiedToken> = classify(model)
        .into_iter()
        .filter(|c| c.start < end && (c.start + c.len) > start)
        .collect();
    encode(model, &filtered)
}

/// Byte offset of char offset `chars` in `s`.
fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(b, _)| b).unwrap_or(s.len())
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
        let x = st.data.iter().find(|t| t.token_type == TYPE_VARIABLE).unwrap();
        assert_eq!(x.delta_line, 0);
        assert_eq!(x.delta_start, 4);
    }

    #[test]
    fn range_filters_to_overlapping_tokens() {
        let src = "let a = 1\nlet b = 2\n";
        let m = model(src);
        // A range covering only line 1 must exclude line-0 tokens.
        let st = semantic_tokens_range(&m, Range::new(
            tower_lsp::lsp_types::Position::new(1, 0),
            tower_lsp::lsp_types::Position::new(1, 9),
        ));
        // First emitted token is on line 1 (delta_line == 1 from the (0,0) baseline).
        assert!(!st.data.is_empty());
        assert_eq!(st.data[0].delta_line, 1);
    }
}
```

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test --lib lsp::providers::semantic_tokens`
Expected: PASS. If `full_deltas_are_monotonic_within_a_line` fails because whitespace is being classified, confirm `classify_one` drops `Whitespace`/`Newline` (the early `is_trivia` arm returns `None` for non-comment trivia).

- [ ] **Step 3: Commit**

```bash
git add src/lsp/providers/semantic_tokens.rs
git commit -m "feat(lsp): semanticTokens full + range with LSP delta-position encoding"
```

---

## Task 4: Register semanticTokens in capabilities + server handlers

**Files:**
- Modify: `src/lsp/server.rs` (`server_capabilities`, two new handlers)
- Test: extend the capability assertions in `src/lsp/server.rs` tests

- [ ] **Step 1: Advertise the capability**

In `src/lsp/server.rs` `server_capabilities()`, add the semantic-tokens registration (full + range, with the legend):

```rust
        semantic_tokens_provider: Some(
            SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                legend: crate::lsp::providers::semantic_tokens::legend(),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                range: Some(true),
                work_done_progress_options: Default::default(),
            }),
        ),
```

Add the needed imports to the `use tower_lsp::lsp_types::*;`/explicit import list at the top of `server.rs`: `SemanticTokensServerCapabilities`, `SemanticTokensOptions`, `SemanticTokensFullOptions`.

- [ ] **Step 2: Add the two handlers**

In the `impl LanguageServer for Backend` block, add:

```rust
    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> tower_lsp::jsonrpc::Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let tokens = crate::lsp::providers::semantic_tokens::semantic_tokens_full(model);
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> tower_lsp::jsonrpc::Result<Option<SemanticTokensRangeResult>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let tokens = crate::lsp::providers::semantic_tokens::semantic_tokens_range(model, range);
        Ok(Some(SemanticTokensRangeResult::Tokens(tokens)))
    }
```

Add imports: `SemanticTokensParams`, `SemanticTokensResult`, `SemanticTokensRangeParams`, `SemanticTokensRangeResult`.

- [ ] **Step 3: Write the failing capability test**

Append to the existing `#[cfg(test)] mod tests` in `src/lsp/server.rs`:

```rust
    #[test]
    fn capabilities_advertise_semantic_tokens() {
        let caps = server_capabilities();
        assert!(
            caps.semantic_tokens_provider.is_some(),
            "expected a semantic-tokens provider"
        );
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS — capability advertised, model-backed tokens flow.

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): register semanticTokens full/range + legend in capabilities"
```

---

## Task 5: `highlight.rs` — documentHighlight (read/write occurrences)

The identifier under the cursor → every occurrence (the binding's decl + all uses), each classified Read or Write. A use is a Write iff it is the target of an assignment.

**Files:**
- Create: `src/lsp/providers/highlight.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod highlight;`)
- Test: inline in `src/lsp/providers/highlight.rs`

- [ ] **Step 1: Declare the module**

In `src/lsp/providers/mod.rs` add:

```rust
pub mod highlight;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/providers/highlight.rs`. The cursor's token → its name; gather the matching binding's `decl_range` (a Write — it's the declaration init/binding site) plus every `uses` entry resolving to that binding. Classify each use Write iff its CST `NameRef` is the direct child of an `AssignExpr` in target position.

```rust
//! `textDocument/documentHighlight`: read/write occurrences of the identifier
//! under the cursor. The decl + every resolved use of the same binding, each
//! tagged Read or Write (an assignment target is a Write).

use crate::lsp::model::SemanticModel;
use crate::lsp::providers::token_spans::token_at;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{DocumentHighlight, DocumentHighlightKind};

/// Highlights for the identifier at byte `offset`. Returns `None` when the cursor
/// is not on an identifier or it resolves to nothing in-file.
pub fn document_highlights(model: &SemanticModel, offset: usize) -> Option<Vec<DocumentHighlight>> {
    let tok = token_at(model, offset)?;
    if tok.kind != SyntaxKind::Ident {
        return None;
    }
    let name = tok.text.clone();

    // The set of write-target byte spans: every NameRef that is the LHS of an
    // AssignExpr (its first NameRef child).
    let write_spans = assignment_target_spans(model);

    let mut out: Vec<DocumentHighlight> = Vec::new();

    // The declaration name span (Write — the binding/decl site).
    if let Some(b) = model.resolved.bindings.iter().find(|b| b.name == name) {
        // Find the NAME-token sub-span of the decl by locating the Ident token at
        // or after decl_range.start whose text == name.
        if let Some(name_span) = decl_name_span(model, b, &name) {
            out.push(highlight(model, name_span.0, name_span.1, DocumentHighlightKind::WRITE));
        }
    }

    // Every resolved use of this name.
    for (range, _res) in model.resolved.uses.iter() {
        let (s, e) = (usize::from(range.start()), usize::from(range.end()));
        if &model.text[s..e] != name {
            continue;
        }
        let kind = if write_spans.contains(&(s, e)) {
            DocumentHighlightKind::WRITE
        } else {
            DocumentHighlightKind::READ
        };
        out.push(highlight(model, s, e, kind));
    }

    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Byte spans of every assignment-TARGET `NameRef` (the first NameRef child of an
/// `AssignExpr`).
fn assignment_target_spans(model: &SemanticModel) -> std::collections::HashSet<(usize, usize)> {
    let mut set = std::collections::HashSet::new();
    for assign in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::AssignExpr)
    {
        if let Some(target) = assign.children().find(|c| c.kind() == SyntaxKind::NameRef) {
            let r = target.text_range();
            set.insert((usize::from(r.start()), usize::from(r.end())));
        }
    }
    set
}

/// The NAME-token byte span of a binding's declaration: the first `Ident` token
/// within `[decl_range.start, decl_range.end)` whose text equals `name`.
fn decl_name_span(
    model: &SemanticModel,
    b: &crate::syntax::resolve::types::Binding,
    name: &str,
) -> Option<(usize, usize)> {
    let lo = usize::from(b.decl_range.start());
    let hi = usize::from(b.decl_range.end());
    let mut off = 0usize;
    for t in &model.tokens {
        let start = off;
        off += t.text.len();
        if t.kind == SyntaxKind::Ident && t.text == name && start >= lo && off <= hi {
            return Some((start, off));
        }
    }
    None
}

fn highlight(
    model: &SemanticModel,
    start: usize,
    end: usize,
    kind: DocumentHighlightKind,
) -> DocumentHighlight {
    DocumentHighlight {
        range: crate::lsp::convert::byte_span_to_range(
            &model.text,
            &model.line_index,
            crate::check::ByteSpan { start, end },
        ),
        kind: Some(kind),
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
    fn highlights_read_and_write_occurrences() {
        let src = "let count = 0\ncount = count + 1\n";
        let m = model(src);
        // Cursor on the `count` in the assignment target (line 1, char 0 → byte 14).
        let off = src.find("count = count").unwrap();
        let hs = document_highlights(&m, off).expect("highlights");
        // At least: decl (write) + LHS (write) + RHS (read).
        let writes = hs.iter().filter(|h| h.kind == Some(DocumentHighlightKind::WRITE)).count();
        let reads = hs.iter().filter(|h| h.kind == Some(DocumentHighlightKind::READ)).count();
        assert!(writes >= 1, "{hs:?}");
        assert!(reads >= 1, "{hs:?}");
    }

    #[test]
    fn none_off_an_identifier() {
        let m = model("let x = 1\n");
        // byte 6 is the `=` operator region (after "let x ").
        assert!(document_highlights(&m, 6).is_none());
    }
}
```

If `decl_name_span` double-counts (an init expr re-using the name), the `start >= lo && off <= hi` bound plus the first-match return keeps it to the decl's own name token; the use-loop separately covers references. Confirm `model.resolved.uses` is keyed by `TextRange` byte offsets (`src/syntax/resolve/types.rs:92`).

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::providers::highlight`
Expected: PASS. If `highlights_read_and_write_occurrences` reports the assignment LHS as READ, confirm the resolver records an assignment-target `NameRef` in `uses` (it does — see `mark_mutated_target` in `src/syntax/resolve/mod.rs`), so the span lands in both `uses` and `assignment_target_spans`.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/highlight.rs src/lsp/providers/mod.rs
git commit -m "feat(lsp): documentHighlight — read/write occurrences off the resolver"
```

---

## Task 6: Wire documentHighlight into the server

**Files:**
- Modify: `src/lsp/server.rs` (`server_capabilities`, `document_highlight` handler, capability test)

- [ ] **Step 1: Advertise + handle**

In `server_capabilities()` add:

```rust
        document_highlight_provider: Some(OneOf::Left(true)),
```

In `impl LanguageServer for Backend` add (mirror the existing `hover` handler's offset computation — `providers::docs::byte_offset_at` from Phase 0):

```rust
    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        Ok(crate::lsp::providers::highlight::document_highlights(model, offset))
    }
```

Add imports `DocumentHighlightParams`, `DocumentHighlight`.

- [ ] **Step 2: Capability test**

Append to `server.rs` tests:

```rust
    #[test]
    fn capabilities_advertise_document_highlight() {
        let caps = server_capabilities();
        assert!(
            matches!(caps.document_highlight_provider, Some(OneOf::Left(true)) | Some(OneOf::Right(_))),
            "expected a document-highlight provider"
        );
    }
```

- [ ] **Step 3: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): register + wire documentHighlight handler"
```

---

## Task 7: `signature.rs` — signatureHelp (active parameter)

When the cursor is inside a call's argument list, show the callee's signature (param names from its `FnDecl`/`MethodDecl` `ParamList`) with the active parameter index = the comma count before the cursor.

**Files:**
- Create: `src/lsp/providers/signature.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod signature;`)
- Test: inline in `src/lsp/providers/signature.rs`

- [ ] **Step 1: Declare the module**

In `src/lsp/providers/mod.rs` add:

```rust
pub mod signature;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/providers/signature.rs`. Find the innermost `CallExpr` whose `ArgList` byte span contains the cursor; its callee `NameRef` name → the matching in-file `FnDecl`'s `ParamList` param names; active param = the number of top-level `Comma` tokens in the arg list before the cursor.

```rust
//! `textDocument/signatureHelp`: while the cursor is inside a call's argument
//! list, surface the callee's parameter list with the active parameter
//! highlighted. In-file only (cross-file deferred to Phase 3's hierarchy work) —
//! the callee must resolve to a unique in-file `FnDecl`.

use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{
    ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
};

/// Signature help at byte `offset`, or `None` when the cursor is not inside a
/// resolvable call's argument list.
pub fn signature_help(model: &SemanticModel, offset: usize) -> Option<SignatureHelp> {
    let (callee_name, arg_list) = enclosing_call(model, offset)?;
    let fn_decl = find_fn_decl(model, &callee_name)?;
    let params = param_names(&fn_decl);
    if params.is_empty() {
        // Still offer a zero-arg signature label.
        return Some(make_help(&callee_name, &[], 0));
    }
    let active = active_param_index(model, &arg_list, offset);
    Some(make_help(&callee_name, &params, active))
}

/// The callee name + the `ArgList` node of the INNERMOST `CallExpr` whose arg
/// list span contains `offset`.
fn enclosing_call(model: &SemanticModel, offset: usize) -> Option<(String, ResolvedNode)> {
    let mut best: Option<(String, ResolvedNode, usize)> = None; // (name, arglist, span_len)
    for call in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CallExpr)
    {
        let Some(arg_list) = call.children().find(|c| c.kind() == SyntaxKind::ArgList) else {
            continue;
        };
        let r = arg_list.text_range();
        let (s, e) = (usize::from(r.start()), usize::from(r.end()));
        if offset >= s && offset <= e {
            let Some(callee) = call.children().find(|c| c.kind() == SyntaxKind::NameRef) else {
                continue;
            };
            let Some(name) = crate::syntax::resolve::ident_text(&callee) else {
                continue;
            };
            let len = e - s;
            if best.as_ref().map(|b| len < b.2).unwrap_or(true) {
                best = Some((name, arg_list, len));
            }
        }
    }
    best.map(|(n, a, _)| (n, a))
}

/// The unique in-file `FnDecl` named `name`, if exactly one exists.
fn find_fn_decl(model: &SemanticModel, name: &str) -> Option<ResolvedNode> {
    let mut found = model.tree.descendants().filter(|n| {
        n.kind() == SyntaxKind::FnDecl
            && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
    });
    let first = found.next()?;
    if found.next().is_some() {
        return None; // ambiguous — skip (zero-FP)
    }
    Some(first)
}

/// Param NAMES of a `FnDecl` (each `Param`'s `Ident` text, in order).
fn param_names(fn_decl: &ResolvedNode) -> Vec<String> {
    let Some(list) = fn_decl
        .children()
        .find(|c| c.kind() == SyntaxKind::ParamList)
    else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == SyntaxKind::Param)
        .filter_map(|p| crate::syntax::resolve::ident_text(&p))
        .collect()
}

/// Active parameter index = the count of top-level `Comma` tokens in `arg_list`
/// that occur at a byte position < `offset`.
fn active_param_index(_model: &SemanticModel, arg_list: &ResolvedNode, offset: usize) -> u32 {
    let mut commas = 0u32;
    for el in arg_list.children_with_tokens() {
        if let Some(tok) = el.into_token() {
            if tok.kind() == SyntaxKind::Comma {
                let pos = usize::from(tok.text_range().start());
                if pos < offset {
                    commas += 1;
                }
            }
        }
    }
    commas
}

fn make_help(name: &str, params: &[String], active: u32) -> SignatureHelp {
    let label = format!("{name}({})", params.join(", "));
    let parameters: Vec<ParameterInformation> = params
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::Simple(p.clone()),
            documentation: None,
        })
        .collect();
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: None,
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
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
    fn shows_signature_and_first_param() {
        let src = "fn add(a, b) { return a + b }\nadd(1, 2)\n";
        let m = model(src);
        // Cursor right after `add(` — inside the arg list, before any comma.
        let off = src.rfind("add(").unwrap() + "add(".len();
        let help = signature_help(&m, off).expect("help");
        assert_eq!(help.signatures[0].label, "add(a, b)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn active_param_advances_past_comma() {
        let src = "fn add(a, b) { return a + b }\nadd(1, 2)\n";
        let m = model(src);
        // Cursor after the comma (on `2`).
        let off = src.rfind('2').unwrap();
        let help = signature_help(&m, off).expect("help");
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn none_outside_a_call() {
        let m = model("let x = 1\n");
        assert!(signature_help(&m, 4).is_none());
    }
}
```

If `enclosing_call` can't find the `NameRef` callee (e.g. a method call `obj.m(...)` whose callee is a `MemberExpr`), it returns `None` — that's the documented in-file limitation for v1; the `add(...)` tests pin the bare-call path. Confirm `CallExpr` has an `ArgList` child and a `NameRef` callee child against `src/lsp/workspace.rs:276` (which uses exactly this shape).

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::providers::signature`
Expected: PASS. If `active_param` is off by one, confirm the comma is a DIRECT token child of `ArgList` (not nested in an arg expr) — the `< offset` guard over direct `Comma` tokens is the correct count.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/signature.rs src/lsp/providers/mod.rs
git commit -m "feat(lsp): signatureHelp — in-file callee params with active-parameter index"
```

---

## Task 8: Wire signatureHelp into the server (trigger chars `(` and `,`)

**Files:**
- Modify: `src/lsp/server.rs` (`server_capabilities`, `signature_help` handler, capability test)

- [ ] **Step 1: Advertise with trigger characters**

In `server_capabilities()` add:

```rust
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: Some(vec![",".to_string()]),
            work_done_progress_options: Default::default(),
        }),
```

Add imports `SignatureHelpOptions`, and for the handler `SignatureHelpParams`, `SignatureHelp`.

- [ ] **Step 2: Add the handler**

```rust
    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> tower_lsp::jsonrpc::Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        Ok(crate::lsp::providers::signature::signature_help(model, offset))
    }
```

- [ ] **Step 3: Capability test**

Append to `server.rs` tests:

```rust
    #[test]
    fn capabilities_advertise_signature_help() {
        let caps = server_capabilities();
        let sig = caps.signature_help_provider.expect("signature-help provider");
        let triggers = sig.trigger_characters.expect("trigger chars");
        assert!(triggers.contains(&"(".to_string()));
        assert!(triggers.contains(&",".to_string()));
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): register + wire signatureHelp handler (trigger chars '(' and ',')"
```

---

## Task 9: `inlay.rs` — inferred-type + parameter-name hints

Two hint families: (1) at an un-annotated `let`/`const` binding site, the SP10 inferred type after the name; (2) at each call argument, the callee's parameter name before the argument. `inlayHint/resolve` lazily fills a tooltip.

**Files:**
- Create: `src/lsp/providers/inlay.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod inlay;`)
- Test: inline in `src/lsp/providers/inlay.rs`

- [ ] **Step 1: Declare the module**

In `src/lsp/providers/mod.rs` add:

```rust
pub mod inlay;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/providers/inlay.rs`. A `LetStmt` is un-annotated iff it has no `Colon` direct token child (per `let_stmt` in `src/syntax/parser.rs:314` — the annotation is `Colon` + a type node). Its name token is the first `Ident` token; the inferred type is `hover_type_at(model.text, name_start)`. Parameter-name hints reuse the signature provider's `find_fn_decl`/`param_names` over each `CallExpr`'s positional args.

```rust
//! `textDocument/inlayHint` (+ resolve): inferred-type hints at un-annotated
//! `let`/`const` sites, and parameter-name hints at call arguments. Types come
//! from the SP10 inferencer (`check::infer::hover_type_at`); names from the
//! callee's in-file `FnDecl` param list. Hints in a requested range only.

use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Position, Range};

/// Inlay hints whose position falls within `range` (byte-filtered after build).
pub fn inlay_hints(model: &SemanticModel, range: Range) -> Vec<InlayHint> {
    let lo = char_to_byte(&model.text, model.line_index.offset(range.start));
    let hi = char_to_byte(&model.text, model.line_index.offset(range.end));
    let mut out = Vec::new();
    out.extend(type_hints(model));
    out.extend(param_name_hints(model));
    out.into_iter()
        .filter(|h| {
            let b = char_to_byte(&model.text, model.line_index.offset(h.position));
            b >= lo && b <= hi
        })
        .collect()
}

/// Inferred-type hints at un-annotated `let`/`const` bindings: `let x⟦: number⟧`.
fn type_hints(model: &SemanticModel) -> Vec<InlayHint> {
    let mut out = Vec::new();
    for stmt in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::LetStmt)
    {
        // Skip if already annotated (a `Colon` direct token child).
        let annotated = stmt
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::Colon);
        if annotated {
            continue;
        }
        // The binding's NAME token (first Ident token in the stmt).
        let Some(name_tok) = stmt
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::Ident)
        else {
            continue;
        };
        let name_end = usize::from(name_tok.text_range().end());
        let name_start = usize::from(name_tok.text_range().start());
        // Inferred type at the name use.
        let Some(ty) = crate::check::infer::hover_type_at(&model.text, name_start) else {
            continue;
        };
        // Don't emit a noise hint for an unknown/`any` type.
        if ty == "any" {
            continue;
        }
        let pos = model
            .line_index
            .position(crate::lsp::convert::byte_to_char(&model.text, name_end));
        out.push(type_hint(pos, &ty));
    }
    out
}

/// Parameter-name hints: `f(⟦a:⟧ 1, ⟦b:⟧ 2)`.
fn param_name_hints(model: &SemanticModel) -> Vec<InlayHint> {
    let mut out = Vec::new();
    for call in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CallExpr)
    {
        let Some(callee) = call.children().find(|c| c.kind() == SyntaxKind::NameRef) else {
            continue;
        };
        let Some(name) = crate::syntax::resolve::ident_text(&callee) else {
            continue;
        };
        let Some(fn_decl) = find_unique_fn_decl(model, &name) else {
            continue;
        };
        let params = fn_param_names(&fn_decl);
        let Some(arg_list) = call.children().find(|c| c.kind() == SyntaxKind::ArgList) else {
            continue;
        };
        // Positional argument expressions, in order.
        let args: Vec<ResolvedNode> = arg_list
            .children()
            .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
            .collect();
        for (i, arg) in args.iter().enumerate() {
            let Some(pname) = params.get(i) else { break };
            let arg_start = usize::from(arg.text_range().start());
            let pos = model
                .line_index
                .position(crate::lsp::convert::byte_to_char(&model.text, arg_start));
            out.push(param_hint(pos, pname));
        }
    }
    out
}

fn find_unique_fn_decl(model: &SemanticModel, name: &str) -> Option<ResolvedNode> {
    let mut it = model.tree.descendants().filter(|n| {
        n.kind() == SyntaxKind::FnDecl
            && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
    });
    let first = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(first)
}

fn fn_param_names(fn_decl: &ResolvedNode) -> Vec<String> {
    let Some(list) = fn_decl
        .children()
        .find(|c| c.kind() == SyntaxKind::ParamList)
    else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == SyntaxKind::Param)
        .filter_map(|p| crate::syntax::resolve::ident_text(&p))
        .collect()
}

fn type_hint(pos: Position, ty: &str) -> InlayHint {
    InlayHint {
        position: pos,
        label: InlayHintLabel::String(format!(": {ty}")),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: Some(false),
        padding_right: Some(false),
        data: None,
    }
}

fn param_hint(pos: Position, name: &str) -> InlayHint {
    InlayHint {
        position: pos,
        label: InlayHintLabel::String(format!("{name}:")),
        kind: Some(InlayHintKind::PARAMETER),
        text_edits: None,
        tooltip: None,
        padding_left: Some(false),
        padding_right: Some(true),
        data: None,
    }
}

/// `inlayHint/resolve`: attach a tooltip lazily. v1 reflects the label into a
/// plain-string tooltip (the heavy detail computation hook for later).
pub fn resolve(hint: InlayHint) -> InlayHint {
    let mut h = hint;
    if h.tooltip.is_none() {
        if let InlayHintLabel::String(s) = &h.label {
            h.tooltip = Some(tower_lsp::lsp_types::InlayHintTooltip::String(s.clone()));
        }
    }
    h
}

fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(b, _)| b).unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    fn full_range(m: &SemanticModel) -> Range {
        let end = m.line_index.position(m.text.chars().count());
        Range::new(Position::new(0, 0), end)
    }

    #[test]
    fn type_hint_on_unannotated_let() {
        let src = "let n = 1\n";
        let m = model(src);
        let hints = inlay_hints(&m, full_range(&m));
        let type_hints: Vec<&InlayHint> =
            hints.iter().filter(|h| h.kind == Some(InlayHintKind::TYPE)).collect();
        assert!(!type_hints.is_empty(), "expected a type hint, got {hints:?}");
        if let InlayHintLabel::String(s) = &type_hints[0].label {
            assert!(s.contains("number"), "got {s}");
        } else {
            panic!("expected a string label");
        }
    }

    #[test]
    fn no_type_hint_when_annotated() {
        let m = model("let n: number = 1\n");
        let hints = inlay_hints(&m, full_range(&m));
        assert!(
            hints.iter().all(|h| h.kind != Some(InlayHintKind::TYPE)),
            "annotated let must not get a type hint"
        );
    }

    #[test]
    fn parameter_name_hints_at_call_args() {
        let src = "fn add(a, b) { return a + b }\nadd(1, 2)\n";
        let m = model(src);
        let hints = inlay_hints(&m, full_range(&m));
        let names: Vec<String> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
            .filter_map(|h| match &h.label {
                InlayHintLabel::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"a:".to_string()), "{names:?}");
        assert!(names.contains(&"b:".to_string()), "{names:?}");
    }

    #[test]
    fn resolve_fills_tooltip() {
        let h = type_hint(Position::new(0, 5), "number");
        let r = resolve(h);
        assert!(r.tooltip.is_some());
    }
}
```

Confirm: `is_expr_kind` is `pub(crate)` (`src/check/rules/mod.rs:202`) and reachable from `crate::check::rules::is_expr_kind` within the same crate (the LSP is in the same crate). If it is not re-exported at that path, the alternative is to filter args by "node child of ArgList that is NOT a `Comma`" via `children()` (node children exclude tokens, so all `ArgList` node children are arg expressions). If the `type_hint_on_unannotated_let` test yields no hint because `hover_type_at` returns `None` for a literal at the decl name offset, switch the offset to the INIT expression's first byte (the literal `1`), which the inferencer types — adjust `name_start` to the init-expr start when present.

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::providers::inlay`
Expected: PASS. Investigate any `None` from `hover_type_at` by checking which byte offset the SP10 `collect_hover_types` ranges cover (`src/check/infer/pass.rs:638` records a `NameRef` use's range) — a `let` binding's NAME may not be a `NameRef` use, so prefer the init-expr offset as noted.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/inlay.rs src/lsp/providers/mod.rs
git commit -m "feat(lsp): inlayHint — inferred-type + parameter-name hints (+ resolve)"
```

---

## Task 10: Wire inlayHint into the server (+ resolve)

**Files:**
- Modify: `src/lsp/server.rs` (`server_capabilities`, `inlay_hint` + `inlay_hint_resolve` handlers, capability test)

- [ ] **Step 1: Advertise**

In `server_capabilities()` add:

```rust
        inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
            InlayHintOptions {
                resolve_provider: Some(true),
                work_done_progress_options: Default::default(),
            },
        ))),
```

Add imports `InlayHintServerCapabilities`, `InlayHintOptions`, and for the handlers `InlayHintParams`, `InlayHint`.

- [ ] **Step 2: Handlers**

```rust
    async fn inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::inlay::inlay_hints(model, range)))
    }

    async fn inlay_hint_resolve(
        &self,
        hint: InlayHint,
    ) -> tower_lsp::jsonrpc::Result<InlayHint> {
        Ok(crate::lsp::providers::inlay::resolve(hint))
    }
```

- [ ] **Step 3: Capability test**

Append to `server.rs` tests:

```rust
    #[test]
    fn capabilities_advertise_inlay_hints() {
        let caps = server_capabilities();
        assert!(caps.inlay_hint_provider.is_some(), "expected an inlay-hint provider");
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): register + wire inlayHint + inlayHint/resolve handlers"
```

---

## Task 11: Zero-false-positive corpus gate + protocol smoke test

The design's §7 testing strategy requires a corpus gate for noise-capable providers (every token classifiable; no contradictory inlay hints) and a protocol smoke assertion that the new capabilities advertise.

**Files:**
- Create: `tests/lsp_phase2.rs` (integration: corpus zero-FP + capability surface)
- Test: the file itself

- [ ] **Step 1: Write the corpus + capability test**

Create `tests/lsp_phase2.rs`:

```rust
//! Phase 2 gates: semantic-token classification + inlay hints produce NO crashes
//! and no contradictory output over the whole `examples/**` corpus, and the four
//! new capabilities are advertised.

use ascript::check::LintConfig;
use ascript::lsp::model::SemanticModel;
use ascript::lsp::providers::{inlay, semantic_tokens, signature};
use ascript::lsp::server::server_capabilities;
use tower_lsp::lsp_types::{Position, Range};

fn corpus_files() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for dir in ["examples", "examples/advanced"] {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("as") {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn semantic_tokens_classify_every_corpus_file_without_panic() {
    for path in corpus_files() {
        let src = std::fs::read_to_string(&path).unwrap();
        let m = SemanticModel::build(src.clone(), None, &LintConfig::default());
        let st = semantic_tokens::semantic_tokens_full(&m);
        // Every emitted token's length is non-zero and its type is in legend range.
        let legend_len = semantic_tokens::legend().token_types.len() as u32;
        for t in &st.data {
            assert!(t.length > 0, "{}: zero-length token", path.display());
            assert!(t.token_type < legend_len, "{}: type out of legend", path.display());
        }
    }
}

#[test]
fn inlay_hints_are_consistent_over_the_corpus() {
    for path in corpus_files() {
        let src = std::fs::read_to_string(&path).unwrap();
        let m = SemanticModel::build(src.clone(), None, &LintConfig::default());
        let end = m.line_index.position(m.text.chars().count());
        let hints = inlay::inlay_hints(&m, Range::new(Position::new(0, 0), end));
        // No two hints occupy the exact same position with conflicting labels.
        let mut seen: std::collections::HashMap<(u32, u32), String> = std::collections::HashMap::new();
        for h in &hints {
            let key = (h.position.line, h.position.character);
            if let tower_lsp::lsp_types::InlayHintLabel::String(s) = &h.label {
                if let Some(prev) = seen.get(&key) {
                    assert_eq!(prev, s, "{}: contradictory inlay hints at {key:?}", path.display());
                } else {
                    seen.insert(key, s.clone());
                }
            }
        }
    }
}

#[test]
fn signature_help_never_panics_over_corpus_offsets() {
    for path in corpus_files() {
        let src = std::fs::read_to_string(&path).unwrap();
        let m = SemanticModel::build(src.clone(), None, &LintConfig::default());
        // Probe every byte offset that is a char boundary; must not panic.
        for (off, _) in src.char_indices() {
            let _ = signature::signature_help(&m, off);
        }
    }
}

#[test]
fn phase2_capabilities_advertised() {
    let caps = server_capabilities();
    assert!(caps.semantic_tokens_provider.is_some());
    assert!(caps.inlay_hint_provider.is_some());
    assert!(caps.document_highlight_provider.is_some());
    assert!(caps.signature_help_provider.is_some());
}
```

Confirm the public re-export paths: `ascript::lsp::model::SemanticModel`, `ascript::lsp::providers::{inlay, semantic_tokens, signature}`, and `ascript::lsp::server::server_capabilities` must all be `pub`. If any is private, make the minimal `pub`/`pub use` change in `src/lsp/mod.rs` / `src/lsp/server.rs` (Phase 0 already made `model` + `providers` public; verify `server_capabilities` is `pub` — it is, `src/lsp/server.rs:69`).

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test --test lsp_phase2`
Expected: PASS. Any contradictory-hint or out-of-legend failure is a real classifier bug — fix the provider (default to the silent/`Unknown` path), NEVER relax the gate (mirrors the SP10 zero-FP discipline, design §7).

- [ ] **Step 3: Commit**

```bash
git add tests/lsp_phase2.rs
git commit -m "test(lsp): Phase 2 zero-FP corpus gate + capability-surface assertions"
```

---

## Task 12: Import-guard + full gate

Phase 0 added a guard test asserting the LSP never imports the legacy front-end. Extend it to the new provider files, then run the full gate.

**Files:**
- Modify: the Phase 0 guard test (`src/lsp/convert.rs` `lsp_does_not_import_legacy_frontend`)

- [ ] **Step 1: Extend the guard's file list**

In `src/lsp/convert.rs`, add the new provider files to the guard's `for file in [...]` list:

```rust
    for file in [
        "analysis.rs", "server.rs", "model.rs", "convert.rs",
        "providers/token_spans.rs", "providers/semantic_tokens.rs",
        "providers/highlight.rs", "providers/signature.rs", "providers/inlay.rs",
    ] {
```

- [ ] **Step 2: Run the guard**

Run: `cargo test --lib lsp::convert::tests::lsp_does_not_import_legacy_frontend`
Expected: PASS — none of the new providers import `crate::{ast,lexer,parser,token}` (they use `crate::syntax::*` + `crate::check::*` only).

- [ ] **Step 3: Full gate**

Run:
```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
```
Expected: all green/clean. The LSP is gated behind the `lsp` feature; under `--no-default-features` the lsp tests are simply absent (the providers don't build), so the build must still be clean.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/convert.rs
git commit -m "test(lsp): extend legacy-import guard to the Phase 2 providers"
```

---

## Phase 2 Done — Gate

- [ ] `semanticTokens/full` + `/range` classify keyword/function/parameter/variable/property/type/enum/enumMember/string/number/comment/namespace tokens and delta-encode correctly; the legend is registered. (Deltas — `semanticTokens/full/delta` — are deferred to Phase 7's incremental work; documented, not a gap.)
- [ ] `inlayHint` shows inferred-type hints at un-annotated `let`/`const` and parameter-name hints at call args; `inlayHint/resolve` attaches a tooltip; `inlay_hint_provider` advertised.
- [ ] `documentHighlight` returns read/write occurrences of the identifier under the cursor (assignment target → Write).
- [ ] `signatureHelp` shows the in-file callee signature with the active parameter highlighted; trigger chars `(` and `,` advertised.
- [ ] Zero-FP corpus gate green (every token classifiable; no contradictory inlay hints; signatureHelp never panics over corpus offsets).
- [ ] Legacy-import guard extended to the new providers and passing.
- [ ] `cargo test`, `cargo test --no-default-features`, and both clippy configs are green.

**Next plan:** `docs/superpowers/plans/2026-06-05-lsp-phase3-navigation-structure-depth.md` (declaration / typeDefinition / implementation; foldingRange / selectionRange / documentLink; callHierarchy / typeHierarchy; workspaceSymbol resolve).
