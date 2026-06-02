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
    let mut brace_depth = 0usize;
    let mut template_stack: Vec<usize> = Vec::new();

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

        // numbers: decimal/float/hex/bin/scientific (legacy-faithful)
        if c.is_ascii_digit() {
            i = scan_number(&chars, i);
            push!(SyntaxKind::Number, start, i);
            continue;
        }

        // identifiers & keywords
        if c.is_alphabetic() || c == '_' {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let text: String = chars[i..j].iter().collect();
            let kind = keyword_kind(&text).unwrap_or(SyntaxKind::Ident);
            out.push(LexToken { kind, text });
            i = j;
            continue;
        }

        // plain strings: "..." and '...'
        if c == '"' || c == '\'' {
            let j = scan_string_end(&chars, i, c);
            push!(SyntaxKind::Str, start, j);
            i = j;
            continue;
        }

        // templates: `...` with ${ } interpolations
        if c == '`' {
            let (kind, j) = scan_template_chunk(&chars, i, /*from_backtick=*/ true);
            push!(kind, start, j);
            i = j;
            if kind == SyntaxKind::TemplateStart {
                template_stack.push(brace_depth);
            }
            continue;
        }

        // a `}` that closes a template interpolation resumes template text
        if c == '}' && template_stack.last() == Some(&brace_depth) {
            let (kind, j) = scan_template_chunk(&chars, i, /*from_backtick=*/ false);
            push!(kind, start, j);
            i = j;
            if kind == SyntaxKind::TemplateEnd {
                template_stack.pop();
            }
            continue;
        }

        // multi-char operators first (longest match), then single-char
        if let Some((kind, len)) = match_operator(&chars, i) {
            match kind {
                SyntaxKind::LBrace => brace_depth += 1,
                SyntaxKind::RBrace => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
            i += len;
            push!(kind, start, i);
            continue;
        }

        // genuinely unrecognized char
        i += 1;
        push!(SyntaxKind::Error, start, i);
    }

    out
}

/// Advance past a numeric literal starting at `i` (a digit). Mirrors the legacy
/// lexer: hex/bin prefixes, decimal with `_`, a fraction only when `.` is
/// followed by a digit (so `0..5` and `a.0` are NOT consumed as floats), and an
/// optional exponent.
fn scan_number(chars: &[char], mut i: usize) -> usize {
    let n = chars.len();
    if chars[i] == '0' && i + 1 < n && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
        i += 2;
        while i < n && (chars[i].is_ascii_hexdigit() || chars[i] == '_') {
            i += 1;
        }
        return i;
    }
    if chars[i] == '0' && i + 1 < n && (chars[i + 1] == 'b' || chars[i + 1] == 'B') {
        i += 2;
        while i < n && (chars[i] == '0' || chars[i] == '1' || chars[i] == '_') {
            i += 1;
        }
        return i;
    }
    while i < n && (chars[i].is_ascii_digit() || chars[i] == '_') {
        i += 1;
    }
    if i + 1 < n && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
        i += 1;
        while i < n && (chars[i].is_ascii_digit() || chars[i] == '_') {
            i += 1;
        }
    }
    if i < n && (chars[i] == 'e' || chars[i] == 'E') {
        let mut j = i + 1;
        if j < n && (chars[j] == '+' || chars[j] == '-') {
            j += 1;
        }
        if j < n && chars[j].is_ascii_digit() {
            j += 1;
            while j < n && chars[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    i
}

/// Longest-match operator/punctuation table → (kind, char-length). 3-char before
/// 2-char before 1-char; `**` before `*=` to match the legacy lexer exactly.
fn match_operator(chars: &[char], i: usize) -> Option<(SyntaxKind, usize)> {
    use SyntaxKind::*;
    let n = chars.len();
    let c0 = chars[i];
    let c1 = if i + 1 < n { Some(chars[i + 1]) } else { None };
    let c2 = if i + 2 < n { Some(chars[i + 2]) } else { None };

    match (c0, c1, c2) {
        ('.', Some('.'), Some('=')) => return Some((DotDotEq, 3)),
        ('.', Some('.'), Some('.')) => return Some((DotDotDot, 3)),
        _ => {}
    }
    if let Some(c1) = c1 {
        let two = match (c0, c1) {
            ('*', '*') => Some(StarStar),
            ('=', '=') => Some(EqEq),
            ('!', '=') => Some(BangEq),
            ('<', '=') => Some(Le),
            ('>', '=') => Some(Ge),
            ('&', '&') => Some(AmpAmp),
            ('|', '|') => Some(PipePipe),
            ('?', '?') => Some(QuestionQuestion),
            ('?', '.') => Some(QuestionDot),
            ('+', '=') => Some(PlusEq),
            ('-', '=') => Some(MinusEq),
            ('*', '=') => Some(StarEq),
            ('/', '=') => Some(SlashEq),
            ('.', '.') => Some(DotDot),
            ('=', '>') => Some(FatArrow),
            _ => None,
        };
        if let Some(k) = two {
            return Some((k, 2));
        }
    }
    let one = match c0 {
        '+' => Plus,
        '-' => Minus,
        '*' => Star,
        '/' => Slash,
        '%' => Percent,
        '(' => LParen,
        ')' => RParen,
        '{' => LBrace,
        '}' => RBrace,
        '[' => LBracket,
        ']' => RBracket,
        ',' => Comma,
        '.' => Dot,
        ':' => Colon,
        ';' => Semicolon,
        '!' => Bang,
        '=' => Eq,
        '<' => Lt,
        '>' => Gt,
        '|' => Pipe,
        '?' => Question,
        _ => return None,
    };
    Some((one, 1))
}

/// Index just past the closing quote of a "..."/'...' string starting at `i`
/// (the opening quote `q`). Honors backslash escapes. Unterminated → chars.len().
fn scan_string_end(chars: &[char], i: usize, q: char) -> usize {
    let n = chars.len();
    let mut j = i + 1;
    while j < n {
        match chars[j] {
            '\\' if j + 1 < n => j += 2,
            c if c == q => return j + 1,
            _ => j += 1,
        }
    }
    n
}

/// Scan a template text chunk. `from_backtick=true` starts at a backtick;
/// `false` starts at a `}` closing an interpolation. Returns (kind, end_index).
/// Stops at an unescaped `${` (more interpolation) or the closing backtick.
/// Lossless: the slice includes the opening `` ` ``/`}` and the closing
/// `` ` ``/`${`.
fn scan_template_chunk(chars: &[char], i: usize, from_backtick: bool) -> (SyntaxKind, usize) {
    use SyntaxKind::*;
    let n = chars.len();
    let mut j = i + 1; // skip opening ` or }
    while j < n {
        match chars[j] {
            '\\' if j + 1 < n => j += 2,
            '`' => {
                let kind = if from_backtick { TemplateStr } else { TemplateEnd };
                return (kind, j + 1);
            }
            '$' if j + 1 < n && chars[j + 1] == '{' => {
                let kind = if from_backtick { TemplateStart } else { TemplateMiddle };
                return (kind, j + 2); // include the ${
            }
            _ => j += 1,
        }
    }
    let kind = if from_backtick { TemplateStr } else { TemplateEnd };
    (kind, n)
}

/// Map a reserved word to its keyword kind. Mirrors the legacy keyword set
/// (`src/token.rs`); `as` is a soft keyword (stays `Ident`), `of` is a keyword.
fn keyword_kind(s: &str) -> Option<SyntaxKind> {
    use SyntaxKind::*;
    Some(match s {
        "true" => TrueKw, "false" => FalseKw, "nil" => NilKw,
        "let" => LetKw, "const" => ConstKw, "if" => IfKw, "else" => ElseKw,
        "while" => WhileKw, "for" => ForKw, "in" => InKw, "of" => OfKw,
        "return" => ReturnKw, "break" => BreakKw, "continue" => ContinueKw,
        "fn" => FnKw, "enum" => EnumKw, "match" => MatchKw, "class" => ClassKw,
        "import" => ImportKw, "export" => ExportKw, "async" => AsyncKw,
        "await" => AwaitKw, "yield" => YieldKw,
        _ => return None,
    })
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

    #[test]
    fn identifiers_and_keywords() {
        use SyntaxKind::*;
        assert_eq!(kinds("let x"), vec![LetKw, Whitespace, Ident]);
        assert_eq!(kinds("return"), vec![ReturnKw]);
        assert_eq!(kinds("await x"), vec![AwaitKw, Whitespace, Ident]);
        assert_eq!(kinds("as"), vec![Ident]);       // soft keyword stays Ident
        assert_eq!(kinds("trueish"), vec![Ident]);  // not the `true` keyword
        assert_eq!(kinds("_foo123"), vec![Ident]);
    }

    #[test]
    fn operators_and_numbers() {
        use SyntaxKind::*;
        assert_eq!(kinds("1 + 2"), vec![Number, Whitespace, Plus, Whitespace, Number]);
        assert_eq!(kinds("a**=b"), vec![Ident, StarStar, Eq, Ident]); // ** wins, then =
        assert_eq!(
            kinds("x ?? y ?. z"),
            vec![
                Ident, Whitespace, QuestionQuestion, Whitespace, Ident, Whitespace, QuestionDot,
                Whitespace, Ident,
            ]
        );
        assert_eq!(kinds("0..=10"), vec![Number, DotDotEq, Number]);
        assert_eq!(kinds("a...b"), vec![Ident, DotDotDot, Ident]);
        assert_eq!(render(&lex("3.14 + 0xFF")), "3.14 + 0xFF");
    }

    #[test]
    fn strings_and_templates_are_lossless() {
        for src in [
            r#""hello\nworld""#,
            r#"'single \'quoted\''"#,
            "`plain template`",
            "`a${x}b`",
            "`outer ${ `inner ${y}` } end`", // nested template
            r#""has } and { and ${ literally""#,
        ] {
            assert_eq!(render(&lex(src)), src, "not lossless: {src}");
        }
    }

    #[test]
    fn string_kinds() {
        use SyntaxKind::*;
        assert_eq!(kinds(r#""hi""#), vec![Str]);
        assert_eq!(kinds("`plain`"), vec![TemplateStr]);
        // `a${x}b` => TemplateStart "a${", Ident x, TemplateEnd "}b`"
        assert_eq!(kinds("`a${x}b`"), vec![TemplateStart, Ident, TemplateEnd]);
    }

    #[test]
    fn operators_and_numbers_isolated() {
        use SyntaxKind::*;
        assert_eq!(kinds("1 + 2"), vec![Number, Whitespace, Plus, Whitespace, Number]);
        assert_eq!(
            kinds("** = =="),
            vec![StarStar, Whitespace, Eq, Whitespace, EqEq]
        );
        assert_eq!(
            kinds("?? ?. ..= ..."),
            vec![
                QuestionQuestion,
                Whitespace,
                QuestionDot,
                Whitespace,
                DotDotEq,
                Whitespace,
                DotDotDot,
            ]
        );
        assert_eq!(kinds("0..=10"), vec![Number, DotDotEq, Number]);
        assert_eq!(
            render(&lex("3.14 + 0xFF + 0b1010 + 1_000 + 1e9")),
            "3.14 + 0xFF + 0b1010 + 1_000 + 1e9"
        );
    }
}
