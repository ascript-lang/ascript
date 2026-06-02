//! CST-walking pretty-printer. Imposes canonical layout while re-emitting
//! comments (see comments.rs). This plan (4a) covers the machinery + a
//! representative node slice; Plan 4b completes per-node coverage.

pub mod comments;

use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;

/// Indentation-aware output builder.
struct Out {
    buf: String,
    indent: usize,
    at_line_start: bool,
}

impl Out {
    fn new() -> Self {
        Out { buf: String::new(), indent: 0, at_line_start: true }
    }
    /// Emit raw text on the current line (writing pending indentation first).
    fn text(&mut self, s: &str) {
        if self.at_line_start && !s.is_empty() {
            for _ in 0..self.indent {
                self.buf.push_str("  ");
            }
            self.at_line_start = false;
        }
        self.buf.push_str(s);
    }
    /// End the current line (trimming trailing spaces).
    fn newline(&mut self) {
        while self.buf.ends_with(' ') {
            self.buf.pop();
        }
        self.buf.push('\n');
        self.at_line_start = true;
    }
    /// Emit ONE blank line. Precondition: buffer ends with a newline (every
    /// statement/comment emitter ends with `newline()`), so one extra '\n'
    /// yields exactly one blank line. Used by the blank-line rule.
    fn blank(&mut self) {
        debug_assert!(self.buf.ends_with('\n'));
        self.buf.push('\n');
        self.at_line_start = true;
    }
    #[allow(dead_code)]
    fn indent(&mut self) { self.indent += 1; }
    #[allow(dead_code)]
    fn dedent(&mut self) { self.indent = self.indent.saturating_sub(1); }

    /// Append ` <comment>` at the end of the last non-empty line (before its
    /// trailing newline). For same-line trailing comments.
    fn append_to_prev_line(&mut self, comment: &str) {
        while self.buf.ends_with('\n') {
            self.buf.pop();
        }
        self.buf.push(' ');
        self.buf.push_str(comment);
        self.buf.push('\n');
        self.at_line_start = true;
    }
}

/// Format a parsed source tree into canonical text.
pub fn format(root: &ResolvedNode) -> String {
    let comments = comments::attach(root);
    let mut out = Out::new();
    let mut p = Printer { out: &mut out, comments: &comments };
    p.source_file(root);
    let mut s = out.buf;
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
    s
}

struct Printer<'a> {
    out: &'a mut Out,
    comments: &'a comments::CommentMap,
}

impl Printer<'_> {
    fn source_file(&mut self, node: &ResolvedNode) {
        let stmts: Vec<&ResolvedNode> = node.children().collect();
        for (i, stmt) in stmts.iter().enumerate() {
            if i > 0 {
                let lead = self.comments.leading.get(&stmt.text_range());
                let want_blank = lead
                    .and_then(|l| l.first())
                    .map(|c| c.blank_before)
                    .unwrap_or_else(|| blank_between_bare(stmt));
                if want_blank {
                    self.out.blank();
                }
            }
            self.emit_leading(stmt);
            self.stmt(stmt);
            self.emit_trailing(stmt);
        }
    }

    fn emit_leading(&mut self, node: &ResolvedNode) {
        if let Some(comments) = self.comments.leading.get(&node.text_range()).cloned() {
            for (i, c) in comments.iter().enumerate() {
                if i > 0 && c.blank_before {
                    self.out.blank();
                }
                self.out.text(&c.text);
                self.out.newline();
            }
        }
    }

    fn emit_trailing(&mut self, node: &ResolvedNode) {
        if let Some(c) = self.comments.trailing.get(&node.text_range()).cloned() {
            self.out.append_to_prev_line(&c);
        }
    }

    /// Format a statement. 4a handles ExprStmt + a fallback; 4b completes it.
    fn stmt(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
            ExprStmt => {
                if let Some(e) = node.children().next() {
                    self.expr(e);
                }
                self.out.newline();
            }
            _ => {
                self.out.text(&node.text().to_string());
                self.out.newline();
            }
        }
    }

    /// Format an expression (representative subset for 4a).
    fn expr(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
            Literal | NameRef => {
                // Emit only the non-trivia token text (the node may contain
                // leading-whitespace trivia tokens in the lossless tree).
                let tok_text = node
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .find(|t| !t.kind().is_trivia())
                    .map(|t| t.text().to_string())
                    .unwrap_or_else(|| node.text().to_string());
                self.out.text(&tok_text);
            }
            BinaryExpr => {
                let kids: Vec<&ResolvedNode> = node.children().collect();
                let op = node
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .find(|t| !t.kind().is_trivia() && is_binary_op(t.kind()))
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();
                if let Some(l) = kids.first() {
                    self.expr(l);
                }
                self.out.text(&format!(" {op} "));
                if let Some(r) = kids.get(1) {
                    self.expr(r);
                }
            }
            _ => self.out.text(&node.text().to_string()),
        }
    }
}

/// Blank-line preservation between two bare statements (no leading comment):
/// preserve one blank when the source had ≥1 blank line (≥2 newlines) between
/// them.
///
/// The tree builder flushes trivia (including newlines) as leading trivia of the
/// NEXT token/node, so the newlines separating `prev` from `next` live at the
/// very beginning of `next`'s range. We count consecutive leading Newline tokens
/// in `next` (stopping at the first non-trivia token) to measure the gap.
fn blank_between_bare(next: &ResolvedNode) -> bool {
    let mut newlines = 0usize;
    for el in next.descendants_with_tokens() {
        if let Some(t) = el.into_token() {
            match t.kind() {
                SyntaxKind::Newline => newlines += 1,
                SyntaxKind::Whitespace => {}
                _ => break, // reached real content; stop
            }
        }
    }
    newlines >= 2
}

fn is_binary_op(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(kind, Plus | Minus | Star | Slash | Percent | StarStar | EqEq | BangEq
        | Lt | Le | Gt | Ge | AmpAmp | PipePipe | QuestionQuestion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_to_tree;

    fn fmt(src: &str) -> String {
        format(&parse_to_tree(src))
    }

    #[test]
    fn canonicalizes_binary_spacing() {
        assert_eq!(fmt("1+2"), "1 + 2\n");
        assert_eq!(fmt("1   +    2"), "1 + 2\n");
    }

    #[test]
    fn preserves_leading_comment() {
        assert_eq!(fmt("// hi\nx\n"), "// hi\nx\n");
    }

    #[test]
    fn preserves_trailing_comment() {
        assert_eq!(fmt("x // tail\n"), "x // tail\n");
    }

    #[test]
    fn blank_line_rule() {
        assert_eq!(fmt("a\n\n\n\nb\n"), "a\n\nb\n"); // 2+ blanks collapse to 1
        assert_eq!(fmt("a\n\nb\n"), "a\n\nb\n");       // one blank preserved
        assert_eq!(fmt("a\nb\n"), "a\nb\n");           // none stays none
    }
}
