//! Trivia-emitting lexer for the lossless CST front-end. Unlike the legacy
//! lexer (which discards whitespace and comments), this one emits EVERY lexeme
//! as a text-carrying token. Concatenating all token texts reproduces the
//! source exactly — the losslessness invariant.

use crate::syntax::kind::SyntaxKind;

/// One lexeme: its kind plus the exact source text it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexToken {
    pub kind: SyntaxKind,
    pub text: String,
}

/// Reconstruct source from a token stream — used by the losslessness invariant.
pub fn render(tokens: &[LexToken]) -> String {
    tokens.iter().map(|t| t.text.as_str()).collect()
}

pub fn lex(src: &str) -> Vec<LexToken> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut out: Vec<LexToken> = Vec::new();

    macro_rules! push {
        ($kind:expr, $start:expr, $end:expr) => {{
            let text: String = chars[$start..$end].iter().collect();
            out.push(LexToken { kind: $kind, text });
        }};
    }

    while i < chars.len() {
        let c = chars[i];
        let start = i;

        if c == '\n' {
            i += 1;
            push!(SyntaxKind::Newline, start, i);
            continue;
        }
        if c.is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() && chars[i] != '\n' {
                i += 1;
            }
            push!(SyntaxKind::Whitespace, start, i);
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            push!(SyntaxKind::LineComment, start, i);
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            if i + 1 < chars.len() {
                i += 2;
            } else {
                i = chars.len();
            }
            push!(SyntaxKind::BlockComment, start, i);
            continue;
        }

        // non-trivia: refined in Tasks 5-7. For now, one Error char (keeps lossless).
        i += 1;
        push!(SyntaxKind::Error, start, i);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<SyntaxKind> {
        lex(src).into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lossless_trivia_only() {
        let src = "  \n\t// a line comment\n/* block\n comment */  \n";
        assert_eq!(render(&lex(src)), src, "lexer must be lossless");
    }

    #[test]
    fn classifies_trivia_kinds() {
        use SyntaxKind::*;
        assert_eq!(kinds("  \n// c\n"), vec![Whitespace, Newline, LineComment, Newline]);
    }

    #[test]
    fn unterminated_block_comment_is_lossless() {
        let src = "/* never closed";
        assert_eq!(render(&lex(src)), src);
    }
}
