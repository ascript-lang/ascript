//! The flat set of syntax kinds: every token kind, every trivia kind, and the
//! node kinds. This single enum is the contract between the lexer, the tree
//! builder, cstree, and (later) the generated typed-AST layer.

/// `cstree`'s derive requires a fieldless `#[repr(u32)]` enum. Variants with a
/// fixed spelling get `#[static_text("…")]` so cstree can intern them once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
#[derive(cstree::Syntax)]
pub enum SyntaxKind {
    // --- core node kinds ---
    Root,

    // --- nodes (Plan 2) ---
    SourceFile,
    // statements
    LetStmt,
    ExprStmt,
    Block,
    IfStmt,
    WhileStmt,
    ReturnStmt,
    DeferStmt,
    FnDecl,
    ParamList,
    Param,
    // expressions
    Literal,
    NameRef,
    UnaryExpr,
    BinaryExpr,
    ParenExpr,
    CallExpr,
    ArgList,
    MemberExpr,
    IndexExpr,
    ArrowExpr,
    AssignExpr,

    // --- expression nodes (Plan 2b-i) ---
    ArrayExpr,
    ObjectExpr,
    ObjectField,
    MapExpr,
    MapEntry,
    SpreadElem,
    /// ADT §3.2: a named call argument `name: expr` (enum-variant construction).
    NamedArg,
    TemplateExpr,
    OptMemberExpr,
    TryExpr,
    UnwrapExpr,
    TernaryExpr,
    AwaitExpr,
    YieldExpr,

    // --- declarations / control flow (Plan 2b-ii) ---
    ForStmt,
    RangeExpr,
    BreakStmt,
    ContinueStmt,
    EnumDecl,
    EnumVariant,
    /// ADT: one declared payload field of an enum variant (`radius: float` /
    /// positional `int`). Children: an optional name `Ident` + `:`, then a type node.
    VariantField,
    ClassDecl,
    FieldDecl,
    MethodDecl,
    // IFACE: structural interface declaration + its parts.
    InterfaceDecl,
    /// One method REQUIREMENT in an interface body (signature, no block).
    MethodReq,
    /// The `extends A, B` interface-composition list.
    ExtendsList,
    /// The `implements A, B` clause on a class.
    ImplementsClause,
    ImportStmt,
    ExportStmt,
    ImportList,
    // let-destructuring binding patterns
    ArrayBindPat,
    ObjectBindPat,
    BindEntry,
    RestBind,
    // match
    MatchExpr,
    MatchArm,
    MatchGuard,
    WildcardPat,
    IdentPat,
    LiteralPat,
    RangePat,
    ArrayPat,
    ObjectPat,
    ObjPatEntry,
    OrPat,
    PatRest,
    /// ADT: a variant-destructuring pattern (`Circle(r)` / `Shape.Circle(r)` /
    /// `Rect(w: ww)`). Children: the variant-ref (name or member) + a paren list of
    /// sub-patterns (positional) or `VariantPatField` entries (named).
    VariantPat,
    /// ADT: one named field entry of a `VariantPat` (`w: ww` or shorthand `w`).
    VariantPatField,
    // types
    NamedType,
    GenericType,
    OptionalType,
    UnionType,
    TupleType,
    TypeArgs,
    RetType,
    // TYPE §6: generics surface syntax
    /// A generic type-parameter LIST on a decl: `<T, U: Bound>`.
    TypeParams,
    /// One declared type parameter inside a `TypeParams` list (`T` or `C: Bound`).
    TypeParam,
    /// A type-parameter's optional interface bound (`: Container<T>`).
    TypeBound,
    /// A parameterized function type: `fn(A) -> B`.
    FnType,
    /// A reference to an in-scope generic type parameter in TYPE position (`T`).
    /// Lowered to `ast::Type::Param`. (Structurally a name; tagged distinctly so the
    /// `cst_type` lowering can map it to `Param` without re-deriving scope.)
    ParamType,

    // --- trivia ---
    Whitespace,
    Newline,
    LineComment,
    BlockComment,

    // --- literals / identifiers (variable text) ---
    Number,
    Str,
    Ident,
    TemplateStr,
    TemplateStart,
    TemplateMiddle,
    TemplateEnd,

    // --- operators & punctuation (fixed text) ---
    #[static_text("+")]
    Plus,
    #[static_text("-")]
    Minus,
    #[static_text("*")]
    Star,
    #[static_text("/")]
    Slash,
    #[static_text("%")]
    Percent,
    #[static_text("**")]
    StarStar,
    #[static_text("(")]
    LParen,
    #[static_text(")")]
    RParen,
    #[static_text("{")]
    LBrace,
    #[static_text("}")]
    RBrace,
    #[static_text("#{")]
    HashLBrace,
    #[static_text("[")]
    LBracket,
    #[static_text("]")]
    RBracket,
    #[static_text(",")]
    Comma,
    #[static_text(".")]
    Dot,
    #[static_text(":")]
    Colon,
    #[static_text(";")]
    Semicolon,
    #[static_text("!")]
    Bang,
    #[static_text("!=")]
    BangEq,
    #[static_text("==")]
    EqEq,
    #[static_text("=")]
    Eq,
    #[static_text("<")]
    Lt,
    #[static_text("<=")]
    Le,
    #[static_text(">")]
    Gt,
    #[static_text(">=")]
    Ge,
    #[static_text("&&")]
    AmpAmp,
    #[static_text("||")]
    PipePipe,
    #[static_text("??")]
    QuestionQuestion,
    #[static_text("?")]
    Question,
    #[static_text("?.")]
    QuestionDot,
    #[static_text("|")]
    Pipe,
    #[static_text("&")]
    Amp,
    #[static_text("^")]
    Caret,
    #[static_text("~")]
    Tilde,
    #[static_text("<<")]
    Shl,
    #[static_text(">>")]
    Shr,
    #[static_text("+%")]
    PlusPercent,
    #[static_text("-%")]
    MinusPercent,
    #[static_text("*%")]
    StarPercent,
    #[static_text("+=")]
    PlusEq,
    #[static_text("-=")]
    MinusEq,
    #[static_text("*=")]
    StarEq,
    #[static_text("/=")]
    SlashEq,
    #[static_text("..")]
    DotDot,
    #[static_text("..=")]
    DotDotEq,
    #[static_text("...")]
    DotDotDot,
    #[static_text("=>")]
    FatArrow,

    // --- keywords (fixed text) ---
    #[static_text("true")]
    TrueKw,
    #[static_text("false")]
    FalseKw,
    #[static_text("nil")]
    NilKw,
    #[static_text("let")]
    LetKw,
    #[static_text("const")]
    ConstKw,
    #[static_text("if")]
    IfKw,
    #[static_text("else")]
    ElseKw,
    #[static_text("while")]
    WhileKw,
    #[static_text("for")]
    ForKw,
    #[static_text("in")]
    InKw,
    #[static_text("of")]
    OfKw,
    #[static_text("instanceof")]
    InstanceofKw,
    #[static_text("return")]
    ReturnKw,
    #[static_text("break")]
    BreakKw,
    #[static_text("continue")]
    ContinueKw,
    #[static_text("fn")]
    FnKw,
    #[static_text("enum")]
    EnumKw,
    #[static_text("match")]
    MatchKw,
    #[static_text("class")]
    ClassKw,
    #[static_text("interface")]
    InterfaceKw,
    #[static_text("defer")]
    DeferKw,
    #[static_text("import")]
    ImportKw,
    #[static_text("export")]
    ExportKw,
    #[static_text("async")]
    AsyncKw,
    #[static_text("await")]
    AwaitKw,
    #[static_text("yield")]
    YieldKw,
    // `static` is a CONTEXTUAL/soft keyword: the lexer never produces it (an
    // identifier `static` lexes as `Ident`). The parser REMAPS the `static`
    // identifier to this kind only in class-member-modifier position (before
    // `fn`/`async fn`/`fn*`), so `let static = 1` keeps `static` an `Ident`.
    // No `#[static_text]` — it is never matched by `keyword_kind`.
    StaticKw,
    // `worker` is a CONTEXTUAL/soft keyword: the lexer never produces it (an
    // identifier `worker` lexes as `Ident`). The parser REMAPS the `worker`
    // identifier to this kind only in fn/method-modifier position (before
    // `fn` or `async`), so `let worker = 1` or `worker(x)` keep `worker` an
    // `Ident`.  No `#[static_text]` — it is never matched by `keyword_kind`.
    WorkerKw,

    // --- sentinel for unrecognized input ---
    Error,

    // --- internal parser sentinel (never appears in a completed tree) ---
    Tombstone,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivia_classification() {
        assert!(SyntaxKind::Whitespace.is_trivia());
        assert!(SyntaxKind::LineComment.is_trivia());
        assert!(SyntaxKind::BlockComment.is_trivia());
        assert!(SyntaxKind::Newline.is_trivia());
        assert!(!SyntaxKind::Number.is_trivia());
        assert!(!SyntaxKind::Plus.is_trivia());
        assert!(!SyntaxKind::LetKw.is_trivia());
    }

    #[test]
    fn expression_node_kinds_exist() {
        for k in [
            SyntaxKind::ArrayExpr,
            SyntaxKind::ObjectExpr,
            SyntaxKind::ObjectField,
            SyntaxKind::SpreadElem,
            SyntaxKind::TemplateExpr,
            SyntaxKind::OptMemberExpr,
            SyntaxKind::TryExpr,
            SyntaxKind::UnwrapExpr,
            SyntaxKind::TernaryExpr,
            SyntaxKind::AwaitExpr,
            SyntaxKind::YieldExpr,
        ] {
            assert!(!k.is_trivia(), "{k:?}");
        }
    }

    #[test]
    fn declaration_node_kinds_exist() {
        for k in [
            SyntaxKind::ForStmt,
            SyntaxKind::RangeExpr,
            SyntaxKind::BreakStmt,
            SyntaxKind::ContinueStmt,
            SyntaxKind::EnumDecl,
            SyntaxKind::EnumVariant,
            SyntaxKind::ClassDecl,
            SyntaxKind::FieldDecl,
            SyntaxKind::MethodDecl,
            SyntaxKind::InterfaceDecl,
            SyntaxKind::MethodReq,
            SyntaxKind::ExtendsList,
            SyntaxKind::ImplementsClause,
            SyntaxKind::ImportStmt,
            SyntaxKind::ExportStmt,
            SyntaxKind::ImportList,
            SyntaxKind::ArrayBindPat,
            SyntaxKind::ObjectBindPat,
            SyntaxKind::BindEntry,
            SyntaxKind::RestBind,
            SyntaxKind::MatchExpr,
            SyntaxKind::MatchArm,
            SyntaxKind::MatchGuard,
            SyntaxKind::WildcardPat,
            SyntaxKind::IdentPat,
            SyntaxKind::LiteralPat,
            SyntaxKind::RangePat,
            SyntaxKind::ArrayPat,
            SyntaxKind::ObjectPat,
            SyntaxKind::ObjPatEntry,
            SyntaxKind::OrPat,
            SyntaxKind::PatRest,
            SyntaxKind::NamedType,
            SyntaxKind::GenericType,
            SyntaxKind::OptionalType,
            SyntaxKind::UnionType,
            SyntaxKind::TupleType,
            SyntaxKind::TypeArgs,
            SyntaxKind::RetType,
        ] {
            assert!(!k.is_trivia(), "{k:?}");
        }
    }

    #[test]
    fn node_kinds_exist_and_are_not_trivia() {
        for k in [
            SyntaxKind::SourceFile,
            SyntaxKind::LetStmt,
            SyntaxKind::ExprStmt,
            SyntaxKind::BinaryExpr,
            SyntaxKind::UnaryExpr,
            SyntaxKind::ParenExpr,
            SyntaxKind::CallExpr,
            SyntaxKind::ArgList,
            SyntaxKind::MemberExpr,
            SyntaxKind::IndexExpr,
            SyntaxKind::Literal,
            SyntaxKind::NameRef,
            SyntaxKind::Block,
            SyntaxKind::IfStmt,
            SyntaxKind::WhileStmt,
            SyntaxKind::ReturnStmt,
            SyntaxKind::FnDecl,
            SyntaxKind::ParamList,
            SyntaxKind::Param,
            SyntaxKind::ArrowExpr,
            SyntaxKind::AssignExpr,
            SyntaxKind::Error,
        ] {
            assert!(!k.is_trivia(), "{k:?} must not be trivia");
        }
    }
}
