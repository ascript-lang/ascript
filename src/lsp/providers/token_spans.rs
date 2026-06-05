//! A byte-positioned view over `model.tokens`. `LexToken` (src/syntax/lexer.rs)
//! carries only `kind` + `text` and NO offset, but the lexer is lossless
//! (concatenated token texts reproduce the source), so a single cumulative pass
//! assigns each token its byte span. Shared by `semantic_tokens` and `highlight`
//! (every token-walking provider).

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
