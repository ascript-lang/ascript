//! The flat set of syntax kinds: every token kind, every trivia kind, and the
//! node kinds. This single enum is the contract between the lexer, the tree
//! builder, cstree, and (later) the generated typed-AST layer.

/// `cstree`'s derive requires a fieldless `#[repr(u32)]` enum. Variants with a
/// fixed spelling get `#[static_text("…")]` so cstree can intern them once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
#[derive(cstree::Syntax)]
pub enum SyntaxKind {
    // --- nodes ---
    /// The whole-document root node.
    Root,

    // --- trivia (text varies → no static_text) ---
    Whitespace,
    Newline,
    LineComment,
    BlockComment,

    // --- a single real token, just for the spike ---
    /// Numeric literal (text varies).
    Number,
}

impl SyntaxKind {
    /// Trivia = tokens that carry no semantic meaning (whitespace + comments).
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::Whitespace
                | SyntaxKind::Newline
                | SyntaxKind::LineComment
                | SyntaxKind::BlockComment
        )
    }
}
