//! The bytecode compiler — walks the CST typed AST (plus the resolver's binding
//! information) and emits a [`Chunk`] for the VM to run.
//!
//! V1 scope: a source file whose meaningful content is a single trailing
//! expression statement (or one expression statement). It compiles literals,
//! arithmetic (`+ - * / % **`), unary `-`/`!`, and parentheses, then emits
//! `RETURN` so the VM yields the expression's value. Statements, locals, control
//! flow, calls, and the richer literal grammar (templates, escapes, hex/binary/
//! scientific numbers) land in V2+.

use crate::lex_literals::{parse_number_text, unescape_str_body, unescape_template_body};
use crate::span::Span;
use crate::syntax::ast::{
    ArrayExpr, ArrowExpr, AssignExpr, AstNode, AwaitExpr, BinaryExpr, Block, BreakStmt, CallExpr,
    ClassDecl, ContinueStmt, EnumDecl, Expr, FnDecl, ForStmt, IfStmt, IndexExpr, LetStmt, Literal,
    MatchArm, MatchExpr, MemberExpr, MethodDecl, NameRef, ObjectExpr, ObjectField, OptMemberExpr,
    ParenExpr, RangeExpr, ReturnStmt, SourceFile, SpreadElem, Stmt, TemplateExpr, TernaryExpr,
    TryExpr, UnaryExpr, UnwrapExpr, WhileStmt, YieldExpr,
};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{ResolveResult, Resolution};
use crate::syntax::{parse_to_tree, resolve::resolve};
use crate::value::Value;
use crate::vm::chunk::{Chunk, ClassProto, FnProto};
use crate::vm::opcode::Op;
use cstree::text::TextRange;
use std::collections::HashSet;
use std::rc::Rc;

/// A compile-time error: a message plus the source span that triggered it. The
/// lib boundary converts this into an [`crate::error::AsError`] for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub message: String,
    pub span: Span,
}

impl CompileError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        CompileError {
            message: message.into(),
            span,
        }
    }
}

/// The span of a CST node, as byte offsets into the original source.
fn node_span(node: &impl AstNode) -> Span {
    range_span(node.syntax())
}

/// Whether `expr` is the bare name `super` — the implicit super reference used as
/// the receiver of `super.<name>(...)` (V9-T2). `super` is lexed as a plain
/// `Ident` (not a reserved keyword), so it surfaces as a `NameRef`; matching by
/// text mirrors the tree-walker, where `super` is a `Value::Super` bound in the
/// method's call env. There is NO bare `super` value outside a `super.<name>(...)`
/// call position (a bare `super` expression is a compile error via the normal
/// name-resolution path, exactly as the tree-walker rejects `super` not used as a
/// member receiver).
fn is_super_receiver(expr: &Expr) -> bool {
    matches!(expr, Expr::NameRef(n) if n.ident_token().map(|t| t.text().to_string()).as_deref() == Some("super"))
}

/// The span of a raw CST node, as byte offsets into the original source.
fn range_span(node: &crate::syntax::cst::ResolvedNode) -> Span {
    let range = node.text_range();
    Span::new(usize::from(range.start()), usize::from(range.end()))
}

/// The span of an AST node starting at its first *non-trivia* token. A CST node's
/// `text_range()` begins at any leading whitespace/comment/newline trivia, so a
/// raw `node_span` would point at the preceding source. This trims to the real
/// code start so the span matches the tree-walker's (which anchors at the AST
/// node's own start) byte-for-byte — needed for diagnostics parity, e.g. the
/// for-range bounds check anchoring at the START bound. Mirrors
/// `crate::check::rules::code_range`.
fn node_code_span(node: &impl AstNode) -> Span {
    let syntax = node.syntax();
    let full = range_span(syntax);
    let start = syntax
        .descendants_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| !t.kind().is_trivia())
        .map(|t| usize::from(t.text_range().start()))
        .unwrap_or(full.start);
    Span::new(start, full.end)
}

/// Convert a CST type-annotation node into the legacy [`crate::ast::Type`] the
/// runtime contract checker (`check_type`) and its `Display` impl operate on.
///
/// This MUST produce the SAME `ast::Type` the legacy parser's `parse_type_atom`
/// builds for the same source (same name→variant mapping, same nesting), because
/// contract-violation messages render the type via `Type::Display`; any divergence
/// would make a VM contract panic differ from the tree-walker's. A `None` result
/// means the annotation was malformed/empty (treated as "no contract"); the
/// front-end's own parser would already have rejected genuinely invalid syntax.
fn cst_type(node: &crate::syntax::cst::ResolvedNode) -> Option<crate::ast::Type> {
    use crate::ast::Type;
    use crate::syntax::kind::SyntaxKind as K;
    match node.kind() {
        K::NamedType => {
            // `nil` lexes as its own keyword token, not an Ident; everything else
            // is a bare identifier matched against the built-in type names exactly
            // like the legacy parser, falling back to a user-named (class/enum) type.
            if node
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == K::NilKw)
            {
                return Some(Type::Nil);
            }
            if node
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == K::FnKw)
            {
                return Some(Type::Fn);
            }
            let name = cst_first_ident(node)?;
            Some(match name.as_str() {
                "number" => Type::Number,
                "string" => Type::String,
                "bool" => Type::Bool,
                "any" => Type::Any,
                "object" => Type::Object,
                "error" => Type::Error,
                _ => Type::Named(name),
            })
        }
        K::GenericType => {
            let name = cst_first_ident(node)?;
            let args: Vec<crate::ast::Type> = node
                .children()
                .find(|c| c.kind() == K::TypeArgs)
                .map(|ta| ta.children().filter_map(cst_type).collect())
                .unwrap_or_default();
            match name.as_str() {
                "array" => Some(Type::Array(Box::new(args.into_iter().next()?))),
                "Result" => Some(Type::Result(Box::new(args.into_iter().next()?))),
                "future" => Some(Type::Future(Box::new(args.into_iter().next()?))),
                "map" => {
                    let mut it = args.into_iter();
                    let k = it.next()?;
                    let v = it.next()?;
                    Some(Type::Map(Box::new(k), Box::new(v)))
                }
                // Unknown generic head — fall back to a named type (matches the
                // legacy parser, which would treat an unrecognised `Foo<...>`
                // head as `Type::Named` after consuming nothing of the args).
                _ => Some(Type::Named(name)),
            }
        }
        K::OptionalType => {
            let inner = node.children().find_map(cst_type)?;
            Some(Type::Optional(Box::new(inner)))
        }
        K::UnionType => {
            // `A | B | C` is a flat run of type children; fold left-associatively
            // into nested `Union`s exactly as the legacy parser's loop does.
            let mut it = node.children().filter_map(cst_type);
            let mut acc = it.next()?;
            for rhs in it {
                acc = Type::Union(Box::new(acc), Box::new(rhs));
            }
            Some(acc)
        }
        K::TupleType => {
            let parts: Vec<crate::ast::Type> =
                node.children().filter_map(cst_type).collect();
            Some(Type::Tuple(parts))
        }
        _ => None,
    }
}

/// The text of the first `Ident` token directly under a CST node.
fn cst_first_ident(node: &crate::syntax::cst::ResolvedNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == crate::syntax::kind::SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// Build a runtime [`crate::ast::Param`] from a CST `Param` node: its name, its
/// declared type contract (if annotated), and whether it is a `...rest` param.
/// The resulting params feed [`crate::interp::check_call_args`] so VM calls bind
/// and contract-check arguments identically to the tree-walker.
fn cst_param(node: &crate::syntax::cst::ResolvedNode) -> crate::ast::Param {
    use crate::syntax::kind::SyntaxKind as K;
    let rest = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == K::DotDotDot);
    let name = cst_first_ident(node).unwrap_or_default();
    let name_span = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == K::Ident)
        .map(|t| {
            let r = t.text_range();
            Span::new(usize::from(r.start()), usize::from(r.end()))
        })
        .unwrap_or_else(|| range_span(node));
    // The type child (if any) is the annotation after the `:`.
    let ty = node
        .children()
        .find(|c| is_type_node(c.kind()))
        .and_then(cst_type);
    crate::ast::Param {
        name,
        ty,
        name_span,
        rest,
    }
}

/// Whether a [`SyntaxKind`] is one of the type-annotation node kinds.
fn is_type_node(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        NamedType | GenericType | OptionalType | UnionType | TupleType
    )
}

/// Map a short-circuiting binary operator to the conditional-jump opcode whose
/// "fires" condition is "the left operand already decides the result, keep it".
/// Returns `None` for ordinary (both-operands-evaluated) binary operators.
///
/// - `&&` keeps the left when it is FALSY -> `JUMP_IF_FALSE`.
/// - `||` keeps the left when it is TRUTHY -> `JUMP_IF_TRUE`.
/// - `??` keeps the left when it is NON-NIL -> `JUMP_IF_NOT_NIL`.
fn short_circuit_op(op: SyntaxKind) -> Option<Op> {
    match op {
        SyntaxKind::AmpAmp => Some(Op::JumpIfFalse),
        SyntaxKind::PipePipe => Some(Op::JumpIfTrue),
        SyntaxKind::QuestionQuestion => Some(Op::JumpIfNotNil),
        _ => None,
    }
}

/// Compile `src` into a top-level [`Chunk`].
///
/// Pipeline: `parse_to_tree` → `SourceFile::cast` → `resolve` (wired so the full
/// front-end runs even though V1 has no locals/globals to bind) → walk the
/// statements, compiling the trailing expression and emitting `RETURN`.
pub fn compile_source(src: &str) -> Result<Chunk, CompileError> {
    let root = parse_to_tree(src);
    let file =
        SourceFile::cast(root.clone()).ok_or_else(|| CompileError::new("expected a source file", Span::new(0, src.len())))?;

    // Run the resolver so the compiler can classify identifier uses (e.g. a bare
    // builtin callee in a `print(...)` call resolves to `Resolution::Global`).
    let resolved = resolve(&root);

    // Size the top chunk's local-slot window from the resolver's top frame so
    // `Fiber::new` reserves exactly enough Nil locals for every `let`/`const`
    // (including block-scoped ones — slots are frame-flat, see `compile_block`).
    let mut chunk = Chunk::new();
    let top_key = (SyntaxKind::SourceFile, root.text_range());
    let top_frame = resolved.frames.get(&top_key);
    let slot_count = top_frame.map(|f| f.slot_count).unwrap_or(0);
    chunk.slot_count = u16::try_from(slot_count).map_err(|_| {
        CompileError::new(
            "too many local slots in top-level frame (max 65535)",
            Span::new(0, src.len()),
        )
    })?;
    // The top frame's cell slots (captured top-level bindings, e.g. a forward- or
    // self-referenced `fn`) and its upvalue plan (always empty — the file frame
    // has no parent to capture from). `Fiber::new` allocates cells from these.
    chunk.cell_slots = top_frame.map(|f| f.cell_slots.clone()).unwrap_or_default();
    chunk.upvalues = top_frame.map(|f| f.upvalues.clone()).unwrap_or_default();
    let cur_cells: HashSet<u32> = chunk.cell_slots.iter().copied().collect();

    // Scratch temporaries are allocated ABOVE the named-local window, so seed the
    // temp cursor from the same slot count the chunk was sized with.
    let next_temp = chunk.slot_count;
    let mut compiler = Compiler {
        chunk,
        resolved,
        loops: Vec::new(),
        next_temp,
        cur_cells,
    };

    // V2 supports a sequence of statements whose meaningful tail is an
    // expression. Each statement is compiled in source order; a leading
    // expression statement's result is discarded with `POP`. The trailing
    // statement, if it is an expression statement, is left on the stack and
    // `RETURN`ed (so `vm_eval_source` observes the program's value); otherwise
    // the program returns `Nil`.
    let stmts: Vec<Stmt> = file.stmts().collect();
    let trailing_expr_node = stmts.last().and_then(|s| match s {
        Stmt::ExprStmt(e) => Some(e.syntax().clone()),
        _ => None,
    });

    for s in &stmts {
        if let Stmt::ExprStmt(es) = s {
            let is_trailing = trailing_expr_node.as_ref() == Some(es.syntax());
            let expr = es
                .expr()
                .ok_or_else(|| CompileError::new("empty expression statement", node_span(es)))?;
            compiler.compile_expr(&expr)?;
            if is_trailing {
                compiler.chunk.emit(Op::Return, node_span(es));
            } else {
                compiler.chunk.emit(Op::Pop, node_span(es));
            }
        } else {
            compiler.compile_stmt(s)?;
        }
    }

    // If the program did not end in an expression statement, there is no value on
    // the stack to return — push `Nil` and `RETURN` it so the run loop always
    // terminates with a `Done` value.
    if trailing_expr_node.is_none() {
        compiler.chunk.emit(Op::Nil, Span::new(src.len(), src.len()));
        compiler.chunk.emit(Op::Return, Span::new(src.len(), src.len()));
    }

    Ok(compiler.chunk)
}

/// The span of a `Stmt`, by reading its wrapped CST node (the enum does not
/// expose a single `syntax()` accessor, so we match each variant).
fn stmt_span(stmt: &Stmt) -> Span {
    let node = match stmt {
        Stmt::LetStmt(n) => n.syntax(),
        Stmt::ExprStmt(n) => n.syntax(),
        Stmt::Block(n) => n.syntax(),
        Stmt::IfStmt(n) => n.syntax(),
        Stmt::WhileStmt(n) => n.syntax(),
        Stmt::ReturnStmt(n) => n.syntax(),
        Stmt::FnDecl(n) => n.syntax(),
        Stmt::ForStmt(n) => n.syntax(),
        Stmt::BreakStmt(n) => n.syntax(),
        Stmt::ContinueStmt(n) => n.syntax(),
        Stmt::EnumDecl(n) => n.syntax(),
        Stmt::ClassDecl(n) => n.syntax(),
        Stmt::ImportStmt(n) => n.syntax(),
        Stmt::ExportStmt(n) => n.syntax(),
    };
    range_span(node)
}

/// A single enclosing loop's patch context, pushed while compiling a loop body so
/// `break`/`continue` inside it (including nested in `if`s) target THIS loop. The
/// stack's `last` is always the innermost loop.
struct LoopCtx {
    /// The already-emitted code offset a `continue` jumps to, when it is known
    /// BEFORE the body is compiled. For `while` this is the condition re-test
    /// (a backward `LOOP`). For a `for`-range the increment is emitted AFTER the
    /// body, so its offset is not yet known while the body compiles — there the
    /// target is `None` and each `continue` records a forward `Jump` patch site in
    /// `continue_sites` instead.
    continue_target: Option<usize>,
    /// Forward `Jump` patch sites emitted by each `continue` when
    /// `continue_target` is `None` (the target lies AHEAD of the body — e.g. a
    /// `for`-range increment). Patched to the increment once it is emitted.
    continue_sites: Vec<usize>,
    /// Forward `Jump` patch sites emitted by each `break`, patched to land just
    /// after the loop once it is fully compiled.
    break_sites: Vec<usize>,
}

struct Compiler {
    chunk: Chunk,
    resolved: ResolveResult,
    /// Stack of enclosing loops; `break`/`continue` target the innermost (`last`).
    loops: Vec<LoopCtx>,
    /// The next free *scratch* slot index. The resolver allocates slots only for
    /// NAMED bindings; this allocator hands out additional anonymous slots ABOVE
    /// the resolver's frame window for compiler-internal temporaries (e.g. the
    /// hoisted for-range `end` bound, evaluated once). It is seeded from the
    /// resolver's frame `slot_count` (so it never collides with a named local) and
    /// each `alloc_temp` bumps both this cursor and the chunk's `slot_count` so
    /// `Fiber::new` reserves the temp.
    next_temp: u16,
    /// The set of local slots that are heap *cells* in the CURRENT frame (the
    /// resolver's `cell_slots` — every captured local). A `GET_LOCAL`/`SET_LOCAL`
    /// for one of these slots is emitted as `GET_LOCAL_CELL`/`SET_LOCAL_CELL`
    /// instead, so the access goes through the by-reference cell. Swapped on
    /// function entry (saved/restored in `compile_fn_proto`).
    cur_cells: HashSet<u32>,
}

impl Compiler {
    /// Emit a read of local `slot`: `GET_LOCAL_CELL` if `slot` is a cell slot in
    /// the current frame (a captured local, accessed by reference), else the plain
    /// `GET_LOCAL`. The two are byte-distinct opcodes so the run loop stays
    /// branch-free.
    fn emit_get_local(&mut self, slot: u16, span: Span) {
        if self.cur_cells.contains(&u32::from(slot)) {
            self.chunk.emit_u16(Op::GetLocalCell, slot, span);
        } else {
            self.chunk.emit_u16(Op::GetLocal, slot, span);
        }
    }

    /// Emit a store into local `slot`: `SET_LOCAL_CELL` for a cell slot, else
    /// `SET_LOCAL`. Both pop the value.
    fn emit_set_local(&mut self, slot: u16, span: Span) {
        if self.cur_cells.contains(&u32::from(slot)) {
            self.chunk.emit_u16(Op::SetLocalCell, slot, span);
        } else {
            self.chunk.emit_u16(Op::SetLocal, slot, span);
        }
    }

    /// The set of cell slots a loop must refresh at the TOP of every iteration so
    /// captured bindings get per-iteration freshness (matching the tree-walker,
    /// which makes a fresh binding per iteration). This is:
    /// - the loop variable's slot (for for-range / for-of), if it is a cell slot,
    ///   passed via `loop_var_slot`; AND
    /// - every cell slot in the CURRENT frame whose binding's `decl_range` lies
    ///   strictly INSIDE the loop BODY's text range (a captured `let`/`fn` etc.
    ///   declared in the body).
    ///
    /// Only CELL slots matter: a non-captured local is a plain slot overwritten
    /// each iteration (already correct). Scratch/induction slots (`alloc_temp`)
    /// are never resolver cell slots, so they are never refreshed. The returned
    /// slots are sorted (ascending) and de-duplicated for deterministic bytecode.
    fn loop_refresh_slots(&self, body: &Block, loop_var_slot: Option<u16>) -> Vec<u16> {
        let body_range = body.syntax().text_range();
        let mut slots: Vec<u16> = Vec::new();
        if let Some(v) = loop_var_slot {
            if self.cur_cells.contains(&u32::from(v)) {
                slots.push(v);
            }
        }
        for b in &self.resolved.bindings {
            // Only cell slots in THIS frame; only bindings declared inside the
            // body. `contains_range` is inclusive, which is exactly what we want
            // (the body node fully contains its descendant declarations).
            if self.cur_cells.contains(&b.slot)
                && body_range.contains_range(b.decl_range)
                && b.decl_range != body_range
            {
                if let Ok(slot) = u16::try_from(b.slot) {
                    slots.push(slot);
                }
            }
        }
        slots.sort_unstable();
        slots.dedup();
        slots
    }

    /// Emit a `FRESH_CELL` for each slot in `slots` (in order). Installs a fresh
    /// heap cell so closures created in this iteration capture only this
    /// iteration's value.
    fn emit_fresh_cells(&mut self, slots: &[u16], span: Span) {
        for &slot in slots {
            self.chunk.emit_u16(Op::FreshCell, slot, span);
        }
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), CompileError> {
        match expr {
            Expr::Literal(lit) => self.compile_literal(lit),
            Expr::BinaryExpr(bin) => self.compile_binary(bin),
            Expr::UnaryExpr(un) => self.compile_unary(un),
            Expr::ParenExpr(paren) => self.compile_paren(paren),
            Expr::CallExpr(call) => self.compile_call(call),
            Expr::TemplateExpr(t) => self.compile_template(t),
            Expr::NameRef(name_ref) => self.compile_name_ref(name_ref),
            Expr::AssignExpr(assign) => self.compile_assign(assign),
            Expr::RangeExpr(range) => self.compile_range(range),
            Expr::ArrayExpr(arr) => self.compile_array(arr),
            Expr::ObjectExpr(obj) => self.compile_object(obj),
            Expr::IndexExpr(ix) => self.compile_index(ix),
            Expr::MemberExpr(m) => self.compile_member(m),
            Expr::OptMemberExpr(m) => self.compile_opt_member(m),
            Expr::TernaryExpr(t) => self.compile_ternary(t),
            Expr::TryExpr(t) => self.compile_try(t),
            Expr::UnwrapExpr(u) => self.compile_unwrap(u),
            Expr::ArrowExpr(arrow) => self.compile_arrow(arrow),
            Expr::AwaitExpr(a) => self.compile_await(a),
            Expr::YieldExpr(y) => self.compile_yield(y),
            Expr::MatchExpr(m) => self.compile_match(m),
        }
    }

    /// Lower a bare identifier reference (`NameRef`). The resolver classifies the
    /// use via its `text_range()`: a `Local(slot)` reads the frame's slot
    /// (`GET_LOCAL`); a `Global(name)` that is a known builtin is a first-class
    /// builtin reference (`GET_GLOBAL`, yielding the `Value::Builtin` — e.g.
    /// `let p = print`, exactly as the tree-walker treats a bare builtin name);
    /// `Upvalue` is a closure capture, emitted (V4-T3) as `GET_UPVALUE` reading
    /// the captured cell by its index in this frame's upvalue plan; a non-builtin
    /// `Global` is a user-global reference, which does not exist at runtime
    /// (top-level `let`s are frame-locals) so it is a documented V4 deferral.
    fn compile_name_ref(&mut self, name_ref: &NameRef) -> Result<(), CompileError> {
        let span = node_span(name_ref);
        let key = name_ref.syntax().text_range();
        match self.resolved.uses.get(&key) {
            Some(Resolution::Local(slot)) => {
                let slot = u16::try_from(*slot).map_err(|_| {
                    CompileError::new("local slot index exceeds 65535", span)
                })?;
                self.emit_get_local(slot, span);
                Ok(())
            }
            // A captured outer-scope variable: read its upvalue cell by index
            // (the resolver's `Upvalue(idx)` is the position in this frame's
            // upvalue plan, matching the closure's `upvalues` vector).
            Some(Resolution::Upvalue(idx)) => {
                let idx = u16::try_from(*idx)
                    .map_err(|_| CompileError::new("upvalue index exceeds 65535", span))?;
                self.chunk.emit_u16(Op::GetUpvalue, idx, span);
                Ok(())
            }
            // A bare reference to a builtin name is a first-class builtin value:
            // `GET_GLOBAL <name>` resolves it to `Value::Builtin` at runtime, the
            // same value the tree-walker reads from its global env. This makes
            // `let p = print; p("hi")` work identically.
            Some(Resolution::Global(name))
                if crate::interp::BUILTIN_NAMES.contains(&name.as_str()) =>
            {
                let idx = self.chunk.add_const(Value::Str(Rc::from(name.as_str())));
                self.chunk.emit_u16(Op::GetGlobal, idx, span);
                Ok(())
            }
            Some(Resolution::Global(name)) => Err(CompileError::new(
                format!("bare global reference '{name}' not yet supported (V4)"),
                span,
            )),
            Some(Resolution::Unresolved) | None => Err(CompileError::new(
                "undefined name",
                span,
            )),
        }
    }

    /// Lower an assignment expression `target <op> value`, where `<op>` is either a
    /// plain `=` or a compound `+=`/`-=`/`*=`/`/=`, and the target is a `NameRef`
    /// (local/upvalue), a `MemberExpr` (`a.k`), or an `IndexExpr` (`a[i]`).
    ///
    /// **Evaluation order mirrors the tree-walker byte-for-byte.** The tree-walker
    /// evaluates the assignment's *value* first (`ExprKind::Assign` evals `value`),
    /// THEN evaluates the target's receiver/index in `assign_to`. A compound
    /// `a OP= b` is a *literal desugar* to `a = (a OP b)` (parser `make_binary`),
    /// so the tree-walker evaluates the target's sub-expressions **TWICE** — once
    /// reading the current value (the desugared binary's lhs) and once for the
    /// store (`assign_to`). We reproduce exactly this (verified: `a()[i()] += b()`
    /// prints `a i b a i`). So this lowering does NOT cache the receiver/index in a
    /// scratch slot — that would diverge.
    ///
    /// Phases (in emission/eval order):
    /// 1. Push the value-to-store. Plain: compile `value`. Compound: compile the
    ///    target as a *read*, then `value`, then the binop (`ADD`/`SUB`/`MUL`/`DIV`).
    /// 2. Store to the target, leaving the stored value on the stack (assignment is
    ///    an expression). The store re-evaluates the receiver/index, which now sit
    ///    ABOVE the value on the stack; `SWAP`/`ROT3` reorder them into the layout
    ///    `SET_PROP`/`SET_INDEX` consume (`[recv, val]` / `[recv, idx, val]`):
    ///    - `NameRef`:  `DUP`; `SET_LOCAL`/`SET_UPVALUE`. (no receiver to eval)
    ///    - `MemberExpr`: eval object; `SWAP`; `SET_PROP <name>`. Stack:
    ///      `[val] -> [val,obj] -> [obj,val] -> [val]`. Eval order: val, obj.
    ///    - `IndexExpr`: eval object; eval index; `ROT3`; `SET_INDEX`. Stack:
    ///      `[val] -> [val,obj] -> [val,obj,idx] -> [obj,idx,val] -> [val]`. Eval
    ///      order: val, obj, idx.
    fn compile_assign(&mut self, assign: &AssignExpr) -> Result<(), CompileError> {
        let span = node_span(assign);
        // Map the assignment operator token to its compound binop, or `None` for a
        // plain `=`. (The CST `AssignExpr` carries the operator as a child token.)
        let assign_op = assign
            .syntax()
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .map(|t| t.kind())
            .find(|k| {
                matches!(
                    k,
                    SyntaxKind::Eq
                        | SyntaxKind::PlusEq
                        | SyntaxKind::MinusEq
                        | SyntaxKind::StarEq
                        | SyntaxKind::SlashEq
                )
            })
            .ok_or_else(|| CompileError::new("assignment missing operator", span))?;
        let compound = match assign_op {
            SyntaxKind::Eq => None,
            SyntaxKind::PlusEq => Some(Op::Add),
            SyntaxKind::MinusEq => Some(Op::Sub),
            SyntaxKind::StarEq => Some(Op::Mul),
            SyntaxKind::SlashEq => Some(Op::Div),
            other => {
                return Err(CompileError::new(
                    format!("unexpected assignment operator {other:?}"),
                    span,
                ))
            }
        };

        let target = assign
            .target()
            .ok_or_else(|| CompileError::new("assignment missing target", span))?;
        let value = assign
            .value()
            .ok_or_else(|| CompileError::new("assignment missing value", span))?;

        // Phase 1: push the value-to-store. For a compound `a OP= b`, this is the
        // desugared `(a OP b)`: read the target, push `b`, then the binop. The
        // binop's span mirrors the tree-walker's desugared `Binary` node span
        // (`Span::new(target.start, value.end)` from `make_binary`), trivia-trimmed
        // for byte-identical Tier-2 type-panic anchoring (#132).
        if let Some(binop) = compound {
            self.compile_target_read(&target)?;
            self.compile_expr(&value)?;
            let binop_span =
                Span::new(node_code_span(&target).start, node_code_span(&value).end);
            self.chunk.emit(binop, binop_span);
        } else {
            self.compile_expr(&value)?;
        }

        // Phase 2: store to the target, leaving the assigned value on the stack.
        match &target {
            Expr::NameRef(name_ref) => {
                // The store target: a frame-local slot (cell-aware) or an upvalue
                // (a captured outer variable, mutated by reference). `DUP` first so
                // a copy remains as the expression's result after the popping store.
                let store = match self.resolved.uses.get(&name_ref.syntax().text_range()) {
                    Some(Resolution::Local(slot)) => {
                        let slot = u16::try_from(*slot).map_err(|_| {
                            CompileError::new("local slot index exceeds 65535", span)
                        })?;
                        (true, slot)
                    }
                    Some(Resolution::Upvalue(idx)) => {
                        let idx = u16::try_from(*idx).map_err(|_| {
                            CompileError::new("upvalue index exceeds 65535", span)
                        })?;
                        (false, idx)
                    }
                    _ => {
                        return Err(CompileError::new(
                            "assignment to a non-local target not yet supported (V4)",
                            node_span(&target),
                        ))
                    }
                };
                self.chunk.emit(Op::Dup, span);
                let (is_local, slot) = store;
                if is_local {
                    self.emit_set_local(slot, span);
                } else {
                    self.chunk.emit_u16(Op::SetUpvalue, slot, span);
                }
            }
            Expr::MemberExpr(m) => {
                // `obj.field = value` (used pervasively as `self.x = id`). Mirrors
                // the tree-walker's `assign_to` `Member` arm: it evaluates `value`
                // first (already on the stack), THEN the receiver `object`; `SWAP`
                // reorders to `[obj, value]` for `SET_PROP`, which carries the
                // declared field-type contract and leaves the assigned value.
                let object = m
                    .expr()
                    .ok_or_else(|| CompileError::new("member assignment missing object", span))?;
                let field = m.ident_token().map(|t| t.text().to_string()).ok_or_else(|| {
                    CompileError::new("member assignment missing field name", span)
                })?;
                self.compile_expr(&object)?;
                self.chunk.emit(Op::Swap, span);
                let name_idx = self.chunk.add_const(Value::Str(field.into()));
                // The op's span is the value's TRIVIA-TRIMMED span so a field-type
                // contract panic anchors EXACTLY where the tree-walker's does
                // (`assign_to`/`set_member` uses `value.span`). See #132.
                self.chunk
                    .emit_u16(Op::SetProp, name_idx, node_code_span(&value));
            }
            Expr::IndexExpr(ix) => {
                // `obj[idx] = value`. Mirrors the tree-walker's `assign_to` `Index`
                // arm: it evaluates `value` first (already on the stack), THEN the
                // receiver `object` and the `index`; `ROT3` reorders to
                // `[obj, idx, value]` for `SET_INDEX` (shared `index_set` dispatch).
                let object = ix
                    .base()
                    .ok_or_else(|| CompileError::new("index assignment missing receiver", span))?;
                let index = ix
                    .index()
                    .ok_or_else(|| CompileError::new("index assignment missing index", span))?;
                self.compile_expr(&object)?;
                self.compile_expr(&index)?;
                self.chunk.emit(Op::Rot3, span);
                // The op's span is the whole index expr's TRIVIA-TRIMMED span so the
                // OOB / object-index-type panic anchors where the tree-walker's does
                // (`index_set(.., target.span)`). See #132.
                self.chunk.emit(Op::SetIndex, node_code_span(ix));
            }
            _ => {
                return Err(CompileError::new(
                    "invalid assignment target",
                    node_span(&target),
                ))
            }
        }
        Ok(())
    }

    /// Compile a *read* of an assignment target (`NameRef`/`MemberExpr`/`IndexExpr`)
    /// — the lhs load for a compound `a OP= b`. Identical to compiling the target as
    /// an ordinary expression (so the receiver/index sub-expressions are evaluated
    /// exactly as the tree-walker re-evaluates them via the desugared `(a OP b)`).
    fn compile_target_read(&mut self, target: &Expr) -> Result<(), CompileError> {
        match target {
            Expr::NameRef(_) | Expr::MemberExpr(_) | Expr::IndexExpr(_) => {
                self.compile_expr(target)
            }
            _ => Err(CompileError::new(
                "invalid compound-assignment target",
                node_span(target),
            )),
        }
    }

    /// Compile a non-expression statement. V2 supports `let`/`const`
    /// declarations and lexical `Block`s; other statement kinds (if/while/for/
    /// fn/class/...) are later deferrals.
    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        match stmt {
            Stmt::LetStmt(let_stmt) => self.compile_let(let_stmt),
            Stmt::Block(block) => self.compile_block(block),
            Stmt::IfStmt(if_stmt) => self.compile_if(if_stmt),
            Stmt::WhileStmt(while_stmt) => self.compile_while(while_stmt),
            Stmt::ForStmt(for_stmt) => self.compile_for(for_stmt),
            Stmt::BreakStmt(break_stmt) => self.compile_break(break_stmt),
            Stmt::ContinueStmt(continue_stmt) => self.compile_continue(continue_stmt),
            Stmt::FnDecl(fn_decl) => self.compile_fn_decl(fn_decl),
            Stmt::ReturnStmt(ret) => self.compile_return(ret),
            Stmt::ClassDecl(class_decl) => self.compile_class(class_decl),
            Stmt::EnumDecl(enum_decl) => self.compile_enum(enum_decl),
            other => Err(CompileError::new(
                "statement kind not yet supported in V2",
                stmt_span(other),
            )),
        }
    }

    /// Compile an `if` / `else if` / `else` statement. `if` is a *statement* — it
    /// produces no value and leaves nothing extra on the stack. Mirrors the
    /// tree-walker's `Stmt::If`: evaluate the condition, run the then-branch when
    /// truthy, else run the else-branch (which is another `if` for `else if`, a
    /// `Block` for a plain `else`, or absent).
    ///
    /// Lowering:
    /// ```text
    ///   <cond>
    ///   jf = JUMP_IF_FALSE   ; pops cond; jumps to the else target when falsy
    ///   <then block>
    ///   je = JUMP             ; skip the else branch
    ///   patch(jf)             ; else target
    ///   <else branch?>        ; else Block, or recursively the `else if` IfStmt
    ///   patch(je)             ; end
    /// ```
    /// `JUMP_IF_FALSE` already pops the tested condition, and each inner statement
    /// is self-balancing (expression statements `POP` their value), so the `if`
    /// leaves the stack exactly as it found it.
    fn compile_if(&mut self, if_stmt: &IfStmt) -> Result<(), CompileError> {
        let span = node_span(if_stmt);
        let cond = if_stmt
            .cond()
            .ok_or_else(|| CompileError::new("if statement missing condition", span))?;
        let then_block = if_stmt
            .then()
            .ok_or_else(|| CompileError::new("if statement missing then-branch", span))?;

        self.compile_expr(&cond)?;
        // Jump over the then-branch to the else target when the condition is falsy
        // (JUMP_IF_FALSE pops the condition either way).
        let jf = self.chunk.emit_jump(Op::JumpIfFalse, span);
        self.compile_block(&then_block)?;
        // After the then-branch, skip the else branch.
        let je = self.chunk.emit_jump(Op::Jump, span);
        // Else target: when the condition was falsy we land here.
        self.chunk.patch_jump(jf);
        // The else branch is at most one of: an `else if` (chained IfStmt) or a
        // plain `else { ... }` block. The grammar makes these mutually exclusive.
        if let Some(elif) = if_stmt.if_stmt() {
            self.compile_if(&elif)?;
        } else if let Some(else_block) = if_stmt.block() {
            self.compile_block(&else_block)?;
        }
        // End: both the then-branch and the else branch converge here.
        self.chunk.patch_jump(je);
        Ok(())
    }

    /// Compile a ternary `cond ? then : els`. Unlike `if`, this is an
    /// *expression*: it leaves exactly ONE value on the stack — the value of the
    /// chosen branch. Mirrors the tree-walker's `ExprKind::Ternary`: evaluate the
    /// condition, run the then-branch when truthy, else the else-branch.
    ///
    /// Lowering (same jump shape as `if`/`else`, but both arms are expressions):
    /// ```text
    ///   <cond>
    ///   jf = JUMP_IF_FALSE   ; pops cond; jump to the else-branch when falsy
    ///   <then>               ; pushes one value
    ///   je = JUMP             ; skip the else-branch
    ///   patch(jf)            ; else target
    ///   <els>                ; pushes one value
    ///   patch(je)            ; both branches converge here, one value on the stack
    /// ```
    /// `JUMP_IF_FALSE` pops the condition. The jumps route control so EXACTLY ONE
    /// of the two branches runs, and each branch pushes exactly one value — so the
    /// net stack effect is +1 regardless of which branch is taken, and the untaken
    /// branch's side effects (e.g. a `print`) never run.
    fn compile_ternary(&mut self, ternary: &TernaryExpr) -> Result<(), CompileError> {
        let span = node_span(ternary);
        let cond = ternary
            .cond()
            .ok_or_else(|| CompileError::new("ternary missing condition", span))?;
        let then = ternary
            .then()
            .ok_or_else(|| CompileError::new("ternary missing then-branch", span))?;
        let els = ternary
            .els()
            .ok_or_else(|| CompileError::new("ternary missing else-branch", span))?;

        self.compile_expr(&cond)?;
        let jf = self.chunk.emit_jump(Op::JumpIfFalse, span);
        self.compile_expr(&then)?;
        let je = self.chunk.emit_jump(Op::Jump, span);
        self.chunk.patch_jump(jf);
        self.compile_expr(&els)?;
        self.chunk.patch_jump(je);
        Ok(())
    }

    /// Compile a `while (cond) { body }` loop. `while` is a *statement* — it
    /// produces no value and leaves the stack exactly as it found it. Mirrors the
    /// tree-walker's `Stmt::While`: re-test the condition each iteration, run the
    /// body while truthy; `break` exits the loop, `continue` jumps back to the
    /// condition re-test.
    ///
    /// Lowering:
    /// ```text
    ///   cond_start:                ; continue target (re-test the cond)
    ///   <cond>
    ///   exit = JUMP_IF_FALSE       ; pops cond; jump past the loop when falsy
    ///   <body block>
    ///   LOOP cond_start            ; backward jump to re-test
    ///   patch(exit)                ; loop exit lands here
    ///   patch(break_sites...)      ; each `break` lands here too (after the loop)
    /// ```
    /// `JUMP_IF_FALSE` pops the tested condition, the `LOOP` back-edge and the
    /// forward `break` jumps move nothing, and each body statement is
    /// self-balancing, so the loop is stack-neutral.
    fn compile_while(&mut self, while_stmt: &WhileStmt) -> Result<(), CompileError> {
        let span = node_span(while_stmt);
        let cond = while_stmt
            .cond()
            .ok_or_else(|| CompileError::new("while statement missing condition", span))?;
        let body = while_stmt
            .body()
            .ok_or_else(|| CompileError::new("while statement missing body", span))?;

        // Cell slots to refresh per iteration: any captured `let`/`fn` declared in
        // the loop BODY (a `while` has no loop variable). The tree-walker runs the
        // body in a fresh child env each iteration, so a body `let` captured by a
        // closure sees only that iteration's value.
        let refresh_slots = self.loop_refresh_slots(&body, None);

        // The continue target is the start of the condition re-test.
        let cond_start = self.chunk.code.len();
        self.compile_expr(&cond)?;
        // Exit the loop when the condition is falsy (JUMP_IF_FALSE pops the cond).
        let exit = self.chunk.emit_jump(Op::JumpIfFalse, span);

        // Push this loop's context BEFORE compiling the body so any `break`/
        // `continue` nested in the body (including inside `if`s) targets it.
        self.loops.push(LoopCtx {
            continue_target: Some(cond_start),
            continue_sites: Vec::new(),
            break_sites: Vec::new(),
        });
        // Top of the iteration: fresh cells for captured body lets BEFORE the body.
        self.emit_fresh_cells(&refresh_slots, span);
        self.compile_block(&body)?;
        // Backward jump to re-test the condition.
        self.chunk.emit_loop(Op::Loop, cond_start, span);
        let ctx = self
            .loops
            .pop()
            .expect("loop context pushed before body must still be present");

        // Loop exit: a falsy condition lands here; every `break` does too.
        self.chunk.patch_jump(exit);
        for site in ctx.break_sites {
            self.chunk.patch_jump(site);
        }
        Ok(())
    }

    /// Compile a `break`: an unconditional forward `JUMP` whose patch site is
    /// recorded on the innermost enclosing loop, to be patched to land just after
    /// the loop. `break` outside any loop is a compile-time error (the tree-walker
    /// rejects it at runtime with the same message).
    fn compile_break(&mut self, break_stmt: &BreakStmt) -> Result<(), CompileError> {
        let span = node_span(break_stmt);
        let site = self.chunk.emit_jump(Op::Jump, span);
        self.loops
            .last_mut()
            .ok_or_else(|| CompileError::new("'break' outside of a loop", span))?
            .break_sites
            .push(site);
        Ok(())
    }

    /// Compile a `continue`: an unconditional backward `LOOP` jump to the innermost
    /// enclosing loop's continue target (for `while`, the condition re-test).
    /// `continue` outside any loop is a compile-time error (mirroring the
    /// tree-walker's runtime rejection message).
    fn compile_continue(&mut self, continue_stmt: &ContinueStmt) -> Result<(), CompileError> {
        let span = node_span(continue_stmt);
        // Determine the innermost loop's continue mode WITHOUT holding a borrow
        // across the emit (the forward-site path mutates `self.loops`).
        let target = self
            .loops
            .last()
            .ok_or_else(|| CompileError::new("'continue' outside of a loop", span))?
            .continue_target;
        match target {
            // Backward `LOOP` to an already-emitted target (e.g. `while`'s cond
            // re-test).
            Some(target) => self.chunk.emit_loop(Op::Loop, target, span),
            // The target lies AHEAD (e.g. a `for`-range increment emitted after the
            // body): emit a forward `Jump` and record it for later patching.
            None => {
                let site = self.chunk.emit_jump(Op::Jump, span);
                self.loops
                    .last_mut()
                    .expect("loop context present (checked above)")
                    .continue_sites
                    .push(site);
            }
        }
        Ok(())
    }

    /// Compile a `return [expr]` statement: push the returned value (or `Nil` for a
    /// bare `return`), then `RETURN`. Mirrors the tree-walker, whose `Stmt::Return`
    /// yields the expression's value or `nil`. `RETURN` ends the current proto body;
    /// the multi-frame CALL/RETURN frame machinery is wired in V4-T3.
    fn compile_return(&mut self, ret: &ReturnStmt) -> Result<(), CompileError> {
        let span = node_span(ret);
        match ret.expr() {
            Some(e) => self.compile_expr(&e)?,
            None => self.chunk.emit(Op::Nil, span),
        }
        self.chunk.emit(Op::Return, span);
        Ok(())
    }

    /// Compile a `fn name(params) { body }` declaration. The function body becomes
    /// its own [`FnProto`] (see [`Self::compile_fn_proto`]); the enclosing frame
    /// then builds a closure over that proto (`CLOSURE idx`) and binds it to the
    /// function's name slot (`SET_LOCAL`, exactly like a `let name = <closure>`).
    /// The name is a `BindingKind::Fn` binding in the ENCLOSING frame, so its slot
    /// is looked up by the declaration node's `text_range()`, the same scheme
    /// [`Self::let_slot`] uses.
    fn compile_fn_decl(&mut self, fn_decl: &FnDecl) -> Result<(), CompileError> {
        let span = node_span(fn_decl);
        let proto = self.compile_fn_proto(fn_decl.syntax())?;
        let idx = self.chunk.add_proto(proto);
        self.chunk.emit_u16(Op::Closure, idx, span);

        // Bind the closure to the fn name's slot in the enclosing frame. The name
        // may be a cell slot (e.g. a self- or forward-referenced fn), so use the
        // cell-aware store: the cell, allocated nil at frame entry and captured by
        // the closure's own body, is filled HERE — late-binding-correct.
        let slot = self.fn_decl_slot(fn_decl)?;
        self.emit_set_local(slot, span);
        Ok(())
    }

    /// The enclosing-frame local slot for a `fn name` declaration. The resolver
    /// records a `BindingKind::Fn` binding whose `decl_range` is the `FnDecl`
    /// node's `text_range()` (see `resolve_stmt`'s `FnDecl` arm), so we match the
    /// binding by that range — the same scheme [`Self::let_slot`] uses.
    fn fn_decl_slot(&self, fn_decl: &FnDecl) -> Result<u16, CompileError> {
        let span = node_span(fn_decl);
        let decl_range: TextRange = fn_decl.syntax().text_range();
        let binding = self
            .resolved
            .bindings
            .iter()
            .find(|b| b.decl_range == decl_range)
            .ok_or_else(|| {
                CompileError::new(
                    "function declaration has no resolver binding (compiler bug)",
                    span,
                )
            })?;
        u16::try_from(binding.slot)
            .map_err(|_| CompileError::new("local slot index exceeds 65535", span))
    }

    /// Compile a class declaration (V9-T1). Mirrors the tree-walker's
    /// `Stmt::Class` build: a [`crate::value::Class`] with the field schemas
    /// (`FieldDecl` → [`crate::value::FieldSchema`], same lowering) and a method
    /// table. The crux is that `value.rs`'s `Class`/`Method` is FROZEN and holds a
    /// TREE-WALKER body (`Vec<Stmt>`), which the VM cannot run — so the VM compiles
    /// each method body to its OWN [`FnProto`]/closure and dispatches THOSE; the
    /// built `Value::Class.methods` map is left EMPTY and the compiled method
    /// closures are registered in the VM's per-class side table (keyed by the
    /// class's `Rc` identity) at runtime by `Op::Class`.
    ///
    /// Lowering:
    /// ```text
    ///   <default thunk closure 0> .. <default thunk closure D-1>  ; one per defaulted field
    ///   <method closure 0> .. <method closure M-1>                ; one per method
    ///   CLASS <class_proto_idx>   ; pops D+M closures, registers them, pushes the class
    ///   SET_LOCAL <name slot>     ; bind the class to its name (like `fn name`)
    /// ```
    ///
    /// Superclass (`extends`), `super`, and `instanceof` are V9-T2: a class with an
    /// `extends` clause is deferred here with a clear error. Each method is compiled
    /// with `self` already declared by the resolver as the method frame's slot 0
    /// (see `resolve_function`'s `MethodDecl` branch); the receiver is bound into
    /// slot 0 at the method CALL (`Vm::invoke_compiled_method`).
    fn compile_class(&mut self, class_decl: &ClassDecl) -> Result<(), CompileError> {
        let span = node_span(class_decl);
        let name = class_decl
            .ident_token()
            .map(|t| t.text().to_string())
            .ok_or_else(|| CompileError::new("class declaration has no name", span))?;

        // V9-T2: superclass (`class X extends Y`). `extends` is a SOFT keyword
        // parsed as an `Ident` token, so a class with a superclass has the direct
        // idents `[ClassName, "extends", SuperName]` — the SuperName is the second
        // `Ident` token, the one following `extends`. Capture it (if present); the
        // resolver recorded a use-resolution for it (`record_superclass_use`), so it
        // resolves lexically like any name reference (local/upvalue/global class).
        let super_ident = class_decl
            .syntax()
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .skip_while(|t| !(t.kind() == SyntaxKind::Ident && t.text() == "extends"))
            .filter(|t| t.kind() == SyntaxKind::Ident)
            .nth(1); // [0] = "extends", [1] = SuperName
        let has_super = super_ident.is_some();

        // Field schemas, in declaration order (mirrors the tree-walker's
        // `Stmt::Class` field_map build: name → FieldSchema{ty, default}).
        let mut field_map: indexmap::IndexMap<String, crate::value::FieldSchema> =
            indexmap::IndexMap::new();
        // Defaulted fields get a 0-arg thunk closure emitted (in declaration order)
        // BEFORE the method closures; `Op::Class` runs them at construct time so a
        // mutable default (e.g. `[]`) yields a FRESH value per instance, exactly
        // like the tree-walker (which evals the default expr per `construct`).
        let mut default_fields: Vec<String> = Vec::new();
        for field in class_decl.field_decls() {
            let fname = field
                .ident_token()
                .map(|t| t.text().to_string())
                .ok_or_else(|| CompileError::new("field declaration has no name", span))?;
            let mut ty = field
                .r#type()
                .as_ref()
                .map(|t| cst_type(t.syntax()))
                .unwrap_or(None)
                .ok_or_else(|| {
                    CompileError::new(format!("field '{fname}' has no type annotation"), span)
                })?;
            // The `name?: T` marker form (a `?` token between the ident and the
            // `:`) lowers to the SAME `Type::Optional` as `name: T?` — mirror the
            // tree-walker/legacy parser.
            let marker_optional = field
                .syntax()
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == SyntaxKind::Question);
            if marker_optional && !matches!(ty, crate::ast::Type::Optional(_)) {
                ty = crate::ast::Type::Optional(Box::new(ty));
            }
            let default = field.expr();
            if default.is_some() {
                default_fields.push(fname.clone());
            }
            // The default Expr is NOT stored on the FieldSchema for the VM (the VM
            // evals defaults via thunk closures); store `None` so the runtime
            // construct path knows the schema carries the type only. The thunk is
            // the source of truth for the value.
            field_map.insert(
                fname,
                crate::value::FieldSchema {
                    ty,
                    default: None,
                },
            );
        }

        // Build the (method-less) class value. `def_env` is unused by the VM (it
        // never evals against a tree-walker Environment); use the global env as an
        // inert placeholder so the field stays well-typed.
        let class = Rc::new(crate::value::Class {
            name: name.clone(),
            superclass: None,
            fields: field_map,
            methods: indexmap::IndexMap::new(),
            def_env: crate::interp::global_env(),
        });

        // For an `extends` clause, emit the SUPERCLASS class-value FIRST (so it
        // sits at the BOTTOM of the `[super?, ..thunks.., ..methods..]` group below
        // `Op::Class`, which pops methods, then thunks, then the superclass last).
        // The superclass name resolves lexically via the resolver's recorded use
        // (`record_superclass_use`), the same Local/Upvalue/Global-class dispatch a
        // `NameRef` uses — mirroring the tree-walker's `env.get(sup_name)`.
        if let Some(sup) = &super_ident {
            let sup_span = node_span(class_decl);
            let key = sup.text_range();
            match self.resolved.uses.get(&key) {
                Some(Resolution::Local(slot)) => {
                    let slot = u16::try_from(*slot).map_err(|_| {
                        CompileError::new("local slot index exceeds 65535", sup_span)
                    })?;
                    self.emit_get_local(slot, sup_span);
                }
                Some(Resolution::Upvalue(idx)) => {
                    let idx = u16::try_from(*idx).map_err(|_| {
                        CompileError::new("upvalue index exceeds 65535", sup_span)
                    })?;
                    self.chunk.emit_u16(Op::GetUpvalue, idx, sup_span);
                }
                Some(Resolution::Global(gname)) => {
                    return Err(CompileError::new(
                        format!("superclass '{gname}' is not in scope as a class binding (V9)"),
                        sup_span,
                    ));
                }
                Some(Resolution::Unresolved) | None => {
                    return Err(CompileError::new(
                        format!("undefined superclass '{}'", sup.text()),
                        sup_span,
                    ));
                }
            }
        }

        // Emit the default-field thunk closures (declaration order) FIRST, then the
        // method closures, so the stack below `CLASS` is
        // `[..thunks.., ..methods..]` and `Op::Class` pops them in reverse.
        for field in class_decl.field_decls() {
            if let Some(default) = field.expr() {
                let proto = self.compile_default_thunk(&default)?;
                let idx = self.chunk.add_proto(proto);
                self.chunk.emit_u16(Op::Closure, idx, span);
            }
        }
        let mut method_names: Vec<String> = Vec::new();
        for method in class_decl.method_decls() {
            let mname = method
                .ident_token()
                .map(|t| t.text().to_string())
                .ok_or_else(|| CompileError::new("method declaration has no name", span))?;
            let proto = self.compile_method_proto(&method)?;
            let idx = self.chunk.add_proto(proto);
            self.chunk.emit_u16(Op::Closure, idx, span);
            method_names.push(mname);
        }

        let class_proto = Rc::new(ClassProto {
            class,
            default_fields,
            method_names,
            has_super,
        });
        let cp_idx = self.chunk.add_class_proto(class_proto);
        self.chunk.emit_u16(Op::Class, cp_idx, span);

        // Bind the class value to its name's slot in the enclosing frame (the
        // resolver records a `BindingKind::Class` binding whose `decl_range` is the
        // ClassDecl node's `text_range()` — hoisted, see `hoist_decls`).
        let slot = self.class_decl_slot(class_decl)?;
        self.emit_set_local(slot, span);
        Ok(())
    }

    /// The enclosing-frame local slot for a `class Name` declaration. The resolver
    /// records a `BindingKind::Class` binding keyed by the ClassDecl node's
    /// `text_range()` (see `resolve_stmt`/`hoist_decls`).
    fn class_decl_slot(&self, class_decl: &ClassDecl) -> Result<u16, CompileError> {
        let span = node_span(class_decl);
        let decl_range: TextRange = class_decl.syntax().text_range();
        let binding = self
            .resolved
            .bindings
            .iter()
            .find(|b| b.decl_range == decl_range)
            .ok_or_else(|| {
                CompileError::new("class declaration has no resolver binding (compiler bug)", span)
            })?;
        u16::try_from(binding.slot)
            .map_err(|_| CompileError::new("local slot index exceeds 65535", span))
    }

    /// Compile an `enum Name { A, B = 1, ... }` declaration. Mirrors the
    /// tree-walker's `Stmt::Enum`: build a [`crate::value::EnumDef`] whose
    /// `variants` map each name to an interned [`crate::value::EnumVariant`]
    /// (`enum_name`, `name`, backing `value`). The whole `Value::Enum` is an
    /// immutable def, so — unlike a class, whose method closures need runtime
    /// upvalues — it is fully constructible at COMPILE time: build it, store it as
    /// a (non-dedupable) constant, `Const`-load it, and bind it to the enum's slot.
    ///
    /// Because `Value::Enum`/`Value::EnumVariant` are NOT dedupable in the const
    /// pool (`const_is_dedupable` excludes them), each `Const` load returns the SAME
    /// `Rc`, so the interned-variant identity that drives `Color.Red == Color.Red`
    /// (`Rc::ptr_eq` in `Value`'s `PartialEq`) holds byte-identically.
    ///
    /// Variant access (`Color.Red`) and `.name`/`.value` are handled at runtime by
    /// `GetProp` → the SHARED `Interp::read_member` (via `Vm::vm_read_member`), which
    /// already maps `Value::Enum` → its `EnumVariant` and `Value::EnumVariant` →
    /// `.name`/`.value` — no new opcode is needed.
    fn compile_enum(&mut self, enum_decl: &EnumDecl) -> Result<(), CompileError> {
        let span = node_span(enum_decl);
        let name = enum_decl
            .ident_token()
            .map(|t| t.text().to_string())
            .ok_or_else(|| CompileError::new("enum declaration has no name", span))?;

        // Build the variant map in declaration order (mirrors the tree-walker's
        // `IndexMap` insertion order). Each backing value is a compile-time
        // constant: the spec restricts an enum variant's backing to a number/string
        // literal (`enum Status { Ok = 200 }`), so we const-evaluate it here.
        let mut variants = indexmap::IndexMap::new();
        for variant in enum_decl.enum_variants() {
            let v_span = node_span(&variant);
            let v_name = variant
                .ident_token()
                .map(|t| t.text().to_string())
                .ok_or_else(|| CompileError::new("enum variant has no name", v_span))?;
            let backing = match variant.expr() {
                Some(expr) => self.const_eval_enum_backing(&expr)?,
                None => Value::Nil,
            };
            let value = Value::EnumVariant(Rc::new(crate::value::EnumVariant {
                enum_name: name.clone(),
                name: v_name.clone(),
                value: backing,
            }));
            variants.insert(v_name, value);
        }

        let def = Value::Enum(Rc::new(crate::value::EnumDef {
            name: name.clone(),
            variants,
        }));
        let cp_idx = self.chunk.add_const(def);
        self.chunk.emit_u16(Op::Const, cp_idx, span);

        // Bind to the enum's name slot. The resolver records a `BindingKind::Enum`
        // binding keyed by the EnumDecl node's `text_range()` (hoisted, like classes
        // and functions — see `hoist_decls`).
        let slot = self.enum_decl_slot(enum_decl)?;
        self.emit_set_local(slot, span);
        Ok(())
    }

    /// Const-evaluate an enum variant's backing expression. The spec limits a
    /// backing value to a number/string literal (with `nil`/bool also lowered as
    /// plain literals, and `-N` as a negated number literal — the only constant
    /// unary form). Anything else is rejected rather than silently dropped; a
    /// non-constant backing has no tree-walker-faithful compile-time value.
    fn const_eval_enum_backing(&self, expr: &Expr) -> Result<Value, CompileError> {
        match expr {
            Expr::Literal(lit) => literal_const_value(lit),
            Expr::ParenExpr(p) => {
                let inner = p.expr().ok_or_else(|| {
                    CompileError::new("empty parenthesized expression", node_span(p))
                })?;
                self.const_eval_enum_backing(&inner)
            }
            Expr::UnaryExpr(un) if un.op() == Some(SyntaxKind::Minus) => {
                let operand = un
                    .expr()
                    .ok_or_else(|| CompileError::new("unary minus has no operand", node_span(un)))?;
                match self.const_eval_enum_backing(&operand)? {
                    Value::Number(n) => Ok(Value::Number(-n)),
                    _ => Err(CompileError::new(
                        "enum variant backing value must be a number or string literal",
                        node_span(un),
                    )),
                }
            }
            other => Err(CompileError::new(
                "enum variant backing value must be a number or string literal",
                node_span(other),
            )),
        }
    }

    /// The enclosing-frame local slot for an `enum Name` declaration. The resolver
    /// records a `BindingKind::Enum` binding keyed by the EnumDecl node's
    /// `text_range()` (see `resolve_stmt`/`hoist_decls`), exactly as for classes.
    fn enum_decl_slot(&self, enum_decl: &EnumDecl) -> Result<u16, CompileError> {
        let span = node_span(enum_decl);
        let decl_range: TextRange = enum_decl.syntax().text_range();
        let binding = self
            .resolved
            .bindings
            .iter()
            .find(|b| b.decl_range == decl_range)
            .ok_or_else(|| {
                CompileError::new("enum declaration has no resolver binding (compiler bug)", span)
            })?;
        u16::try_from(binding.slot)
            .map_err(|_| CompileError::new("local slot index exceeds 65535", span))
    }

    /// Compile a method body (a `MethodDecl`) to its own [`FnProto`]. A method is a
    /// function with an implicit `self` receiver at slot 0 (declared by the
    /// resolver). The receiver is bound into slot 0 by the method-CALL path; the
    /// declared params occupy slots `1..n+1`. `compile_fn_proto` already builds the
    /// `params`/`ret`/`arity` from the param nodes (which EXCLUDE `self`), so the
    /// proto's `arity` is the user-visible arg count.
    fn compile_method_proto(
        &mut self,
        method: &MethodDecl,
    ) -> Result<Rc<FnProto>, CompileError> {
        // A generator method (`fn*`) is out of scope for V9-T1.
        if method
            .syntax()
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::Star)
        {
            return Err(CompileError::new(
                "generator methods (fn*) not yet supported in the VM",
                node_span(method),
            ));
        }
        self.compile_fn_proto(method.syntax())
    }

    /// Compile a field default expression into a zero-argument thunk [`FnProto`]
    /// (`<default>; RETURN`). The thunk is run at construct time so a mutable
    /// default yields a fresh value per instance (matching the tree-walker, which
    /// re-evals the default per `construct`).
    ///
    /// The default expression is resolved (by the whole-tree resolver) in the
    /// class's ENCLOSING frame, so it may reference globals/builtins (works:
    /// `GET_GLOBAL`) but NOT enclosing locals/upvalues — those would resolve to the
    /// wrong slot in this standalone thunk frame. A default referencing an
    /// enclosing local is therefore deferred with a clear error (V9 polish). Real
    /// defaults are overwhelmingly literals/constants.
    fn compile_default_thunk(&mut self, default: &Expr) -> Result<Rc<FnProto>, CompileError> {
        let span = node_span(default);
        // Reject a default that captures an enclosing local/upvalue (see above).
        self.assert_no_local_capture(default.syntax())?;

        let mut body_chunk = Chunk::new();
        body_chunk.name = Some("<field default>".to_string());

        let saved_chunk = std::mem::replace(&mut self.chunk, body_chunk);
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_next_temp = self.next_temp;
        let saved_cells = std::mem::take(&mut self.cur_cells);
        self.next_temp = 0;

        let result = self.compile_expr(default);

        let body_chunk = std::mem::replace(&mut self.chunk, saved_chunk);
        self.loops = saved_loops;
        self.next_temp = saved_next_temp;
        self.cur_cells = saved_cells;
        let mut body_chunk = match result {
            Ok(()) => body_chunk,
            Err(e) => return Err(e),
        };
        body_chunk.emit(Op::Return, span);

        Ok(Rc::new(FnProto {
            chunk: body_chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        }))
    }

    /// Error if any `NameRef` under `node` resolves to a `Local`/`Upvalue` — used
    /// to keep a field-default thunk self-contained (it may only reference
    /// globals/builtins, never an enclosing local).
    fn assert_no_local_capture(&self, node: &ResolvedNode) -> Result<(), CompileError> {
        for descendant in node.descendants() {
            if descendant.kind() == SyntaxKind::NameRef {
                match self.resolved.uses.get(&descendant.text_range()) {
                    Some(Resolution::Local(_)) | Some(Resolution::Upvalue(_)) => {
                        return Err(CompileError::new(
                            "a class field default referencing an enclosing local is not yet supported in the VM",
                            range_span(node),
                        ));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Compile an arrow expression `(params) => body` (an EXPRESSION): build its
    /// [`FnProto`], add it to the current chunk's proto table, and emit `CLOSURE
    /// idx`, which leaves the resulting `Value::Closure` on the stack. The arrow has
    /// no name, so nothing is bound.
    fn compile_arrow(&mut self, arrow: &ArrowExpr) -> Result<(), CompileError> {
        let span = node_span(arrow);
        let proto = self.compile_fn_proto(arrow.syntax())?;
        let idx = self.chunk.add_proto(proto);
        self.chunk.emit_u16(Op::Closure, idx, span);
        Ok(())
    }

    /// Compile a nested function body (a `FnDecl`, `ArrowExpr`, or — later —
    /// `MethodDecl`) into its own [`FnProto`].
    ///
    /// A fresh sub-context is set up that shares the SAME whole-tree
    /// [`ResolveResult`]: the current `chunk`, loop stack, and temp cursor are
    /// swapped out (saved and restored) so all the existing `compile_*` methods —
    /// which operate on `self.chunk` against `self.resolved` — compile the body
    /// into a NEW chunk sized from the function's OWN resolver frame.
    ///
    /// Frame layout (verified against the resolver): the function's frame is keyed
    /// `(fn_kind, fn_range)`; params are declared FIRST into a fresh frame whose
    /// `next_slot` starts at 0, so they occupy slots `0..arity` in declaration
    /// order — which is the CALL convention V4-T3 relies on (args land in those
    /// slots). `arity` excludes a trailing `...rest` param; `has_rest` is whether
    /// the last param is `...rest`. `is_async`/`is_generator` come from the fn's
    /// `async`/`*` tokens.
    ///
    /// Body lowering mirrors the tree-walker: a `Block` body is compiled
    /// statement-by-statement, then `NIL; RETURN` is appended so a function that
    /// falls off the end returns `nil` (`Flow::Normal => Value::Nil` in
    /// `run_body`); an explicit `return` inside still emits its own `RETURN`. An
    /// arrow EXPRESSION body (no Block) compiles the expression then `RETURN` (an
    /// implicit return of the expression value).
    ///
    /// **Captures/upvalues (V4-T3, wired).** This frame's capture plan
    /// (`frame.upvalues`) and its captured-local cell slots (`frame.cell_slots`)
    /// come straight from the resolver. The upvalue plan is stored on the body
    /// chunk (`body_chunk.upvalues`) so the enclosing frame's `Op::Closure`
    /// materializes the `Value::Closure` with its upvalue cells wired at runtime,
    /// and the cell slots drive both cell allocation at frame entry and the
    /// compiler's cell-aware local opcodes. A body that reads an outer-scope local
    /// (including a `fn`'s reference to its OWN name, declared in the ENCLOSING
    /// frame) therefore compiles and captures correctly today.
    ///
    /// The REMAINING V5 work is optimization/correctness refinement, not "captures
    /// don't work": capture-by-VALUE for never-reassigned bindings (avoiding a cell
    /// where a snapshot would do) and per-iteration loop-variable freshness. These
    /// refine an already-functioning capture path.
    fn compile_fn_proto(&mut self, fn_node: &ResolvedNode) -> Result<Rc<FnProto>, CompileError> {
        let span = range_span(fn_node);
        let fn_kind = fn_node.kind();
        let fn_range = fn_node.text_range();

        let frame = self
            .resolved
            .frames
            .get(&(fn_kind, fn_range))
            .ok_or_else(|| {
                CompileError::new("function body has no resolver frame (compiler bug)", span)
            })?;

        let slot_count = u16::try_from(frame.slot_count).map_err(|_| {
            CompileError::new("too many local slots in function frame (max 65535)", span)
        })?;
        // The function's capture plan (upvalues, indexed by upvalue number) and its
        // cell slots (captured locals) come straight from its resolver frame. The
        // capture plan is stored on the body chunk so `Op::Closure` can wire the
        // upvalue cells at runtime; the cell slots drive both cell allocation at
        // frame entry and the compiler's cell-aware local opcodes.
        let upvalues = frame.upvalues.clone();
        let cell_slots = frame.cell_slots.clone();

        // Calling-convention flags + params. `children()` borrows, so collect the
        // param nodes as references (we only inspect them, never store them).
        let params: Vec<&ResolvedNode> = fn_node
            .children()
            .find(|c| c.kind() == SyntaxKind::ParamList)
            .map(|pl| {
                pl.children()
                    .filter(|c| c.kind() == SyntaxKind::Param)
                    .collect()
            })
            .unwrap_or_default();
        let has_rest = params
            .last()
            .map(|p| {
                p.children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .any(|t| t.kind() == SyntaxKind::DotDotDot)
            })
            .unwrap_or(false);
        // `arity` is the count of NON-rest params (the rest param collects the tail
        // into an array; it is not a positional argument).
        let param_count = params.len();
        let arity_usize = if has_rest {
            param_count.saturating_sub(1)
        } else {
            param_count
        };
        let arity = u8::try_from(arity_usize)
            .map_err(|_| CompileError::new("too many parameters (max 255)", span))?;

        // Build the runtime param specs (name + declared type contract + rest flag)
        // and the declared return-type contract. The VM CALL/RETURN feed these into
        // the SAME `check_call_args` / `check_type` the tree-walker uses, so arity,
        // per-param contracts, rest collection, and the return contract are
        // byte-identical across engines (message + span).
        let proto_params: Vec<crate::ast::Param> = params.iter().map(|p| cst_param(p)).collect();
        let ret_type = fn_node
            .children()
            .find(|c| c.kind() == SyntaxKind::RetType)
            .and_then(|rt| rt.children().find(|c| is_type_node(c.kind())))
            .and_then(cst_type);

        // `async fn` / `fn*` / `async fn*` flags, read from the fn's own tokens.
        let mut is_async = false;
        let mut is_generator = false;
        for tok in fn_node.children_with_tokens().filter_map(|el| el.into_token()) {
            match tok.kind() {
                SyntaxKind::AsyncKw => is_async = true,
                SyntaxKind::Star => is_generator = true,
                _ => {}
            }
        }

        // Build a fresh chunk for the body, sized from the function's own frame,
        // and swap it (plus a fresh loop stack and temp cursor) into `self` so the
        // existing compile_* methods emit into it. `self.resolved` (whole-tree) is
        // left in place and shared.
        let mut body_chunk = Chunk::new();
        body_chunk.slot_count = slot_count;
        body_chunk.name = fn_name_token_text(fn_node);
        body_chunk.upvalues = upvalues;
        body_chunk.cell_slots = cell_slots;

        let saved_chunk = std::mem::replace(&mut self.chunk, body_chunk);
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_next_temp = self.next_temp;
        // Swap in the body frame's cell-slot set so the body's local accesses emit
        // the cell-aware opcodes for ITS captured locals.
        let saved_cells = std::mem::replace(
            &mut self.cur_cells,
            self.chunk.cell_slots.iter().copied().collect(),
        );
        // Scratch temporaries in the body start ABOVE its own named-local window.
        self.next_temp = slot_count;

        // Compile the body. Restore the outer context on EVERY exit path (success
        // or error) so a deferral mid-body cannot corrupt the enclosing compiler.
        let result = self.compile_fn_body(fn_node, span);

        let body_chunk = std::mem::replace(&mut self.chunk, saved_chunk);
        self.loops = saved_loops;
        self.next_temp = saved_next_temp;
        self.cur_cells = saved_cells;
        result?;

        Ok(Rc::new(FnProto {
            chunk: body_chunk,
            arity,
            has_rest,
            is_async,
            is_generator,
            params: proto_params,
            ret: ret_type,
        }))
    }

    /// Emit the body instructions for a function/arrow into the (already swapped-in)
    /// `self.chunk`. A `Block` body is its statements followed by a fall-off-end
    /// `NIL; RETURN`; an arrow EXPRESSION body is the expression followed by
    /// `RETURN`.
    fn compile_fn_body(
        &mut self,
        fn_node: &ResolvedNode,
        span: Span,
    ) -> Result<(), CompileError> {
        if let Some(block_node) = fn_node.children().find(|c| c.kind() == SyntaxKind::Block) {
            let block = Block::cast(block_node.clone())
                .ok_or_else(|| CompileError::new("function body is not a block", span))?;
            self.compile_block(&block)?;
            // Fall-off-end: a function with no explicit trailing `return` returns
            // `nil` (mirrors the tree-walker's `Flow::Normal => Value::Nil`).
            self.chunk.emit(Op::Nil, span);
            self.chunk.emit(Op::Return, span);
            Ok(())
        } else {
            // Expression-body arrow: `(x) => expr` returns `expr`. The body is the
            // direct child node that casts to an `Expr` (the ParamList is not an
            // Expr, so it is skipped).
            let expr = fn_node
                .children()
                .find_map(|c| Expr::cast(c.clone()))
                .ok_or_else(|| {
                    CompileError::new("arrow function has no body expression", span)
                })?;
            self.compile_expr(&expr)?;
            self.chunk.emit(Op::Return, span);
            Ok(())
        }
    }

    /// Allocate a fresh anonymous scratch slot ABOVE the resolver's named-local
    /// window. Bumps both the temp cursor and the chunk's `slot_count` so
    /// `Fiber::new` reserves the slot as a `Nil` local. Used for compiler-internal
    /// temporaries that have no source name — e.g. the for-range `end` bound,
    /// hoisted into a slot so it is evaluated exactly once before the loop.
    fn alloc_temp(&mut self) -> Result<u16, CompileError> {
        let slot = self.next_temp;
        let next = slot.checked_add(1).ok_or_else(|| {
            CompileError::new(
                "too many local slots (scratch temporaries exceed 65535)",
                Span::new(0, 0),
            )
        })?;
        self.next_temp = next;
        // Grow the reserved local window if this temp pushed past it.
        if next > self.chunk.slot_count {
            self.chunk.slot_count = next;
        }
        Ok(slot)
    }

    /// Compile a `for (i in start..end) { body }` RANGE loop. Mirrors the
    /// tree-walker's `Stmt::ForRange` exactly: evaluate `start` and `end` (BOTH
    /// must be numbers, else a Tier-2 panic at the START bound's span), evaluate
    /// `end` ONCE before the loop, then iterate `i` from `start` while `i < end`
    /// (EXCLUSIVE), binding the loop var each iteration. `break` exits, `continue`
    /// runs the increment then re-tests, `return` returns.
    ///
    /// Lowering:
    /// ```text
    ///   <start>; <end>             ; start below, end on top
    ///   CHECK_NUMBERS              ; both-numbers guard @ start.span (peek-only)
    ///   SET_LOCAL end_slot         ; end_slot = alloc_temp(); end evaluated once
    ///   SET_LOCAL var_slot         ; i = start (var_slot from the resolver LoopVar)
    /// cond_start:
    ///   GET_LOCAL var_slot; GET_LOCAL end_slot; LT
    ///   exit = JUMP_IF_FALSE       ; exit when i >= end
    ///   <body block>               ; loop ctx: continue_target = increment below
    ///   increment:                 ; continue lands here → run i += 1 then re-test
    ///   GET_LOCAL var_slot; CONST 1.0; ADD; SET_LOCAL var_slot
    ///   LOOP cond_start
    /// exit:
    ///   patch(break_sites...)      ; each `break` lands here
    /// ```
    /// The INCLUSIVE form `for (i in 0..=5)` is REJECTED here: the legacy parser
    /// (the differential oracle) rejects `..=` in a for-range head outright
    /// ("expected RParen, found DotDotEq"), so inclusive for-range is unsupported
    /// in the tree-walker. The VM must not invent behavior the oracle lacks, so an
    /// `..=` for-range is a documented `CompileError` (both engines reject it).
    ///
    /// A for-of (`for (x of iterable)`, `op() == OfKw`) is lowered by
    /// [`Self::compile_for_of`] (sync snapshot iteration); `for await` async
    /// iteration is V7.
    fn compile_for(&mut self, for_stmt: &ForStmt) -> Result<(), CompileError> {
        let span = node_span(for_stmt);

        let body = for_stmt
            .body()
            .ok_or_else(|| CompileError::new("for statement missing body", span))?;

        // `for await (x of e)` is async iteration over a generator / native stream;
        // it is NEVER a range loop (the `await` token is the discriminator, exactly
        // like the tree-walker's `Stmt::ForOf { for_await: true }`).
        if for_stmt.await_token().is_some() {
            let iter = for_stmt.iter().ok_or_else(|| {
                CompileError::new("for await statement missing iterable", span)
            })?;
            return self.compile_for_await(for_stmt, &iter, &body);
        }

        // The CST head holds the iterable/bounds expression plus an `in`/`of`
        // operator. A for-RANGE is `in` + a `RangeExpr` iterable; the
        // iterator-driven `for (x of ...)` form is a sync for-of — INCLUDING `for
        // (x of a..b)`, which materializes the range ARRAY then iterates it (a
        // different construct from the range loop).
        let iter = for_stmt
            .iter()
            .ok_or_else(|| CompileError::new("for statement missing iterable/start bound", span))?;

        // The legacy parser OVERLOADS `for ... in ...`: only `in` + a LITERAL
        // `RangeExpr` lowers to the allocation-free lazy range loop; `in` over any
        // OTHER value (an array, a range VALUE bound to a name, etc.) falls back to
        // ForOf and iterates the resulting value (src/parser.rs `Tok::In` arm). So
        // the ONLY range-for case here is `in` + a `RangeExpr`; `of`, and `in` over
        // a non-`RangeExpr`, are both sync for-of (snapshot iteration over
        // Array/Str). The iterable can be any expression (array literal, name, even
        // a `RangeExpr` that builds the range array via `of`).
        let is_in = for_stmt.op() == Some(SyntaxKind::InKw);
        let range = match &iter {
            Expr::RangeExpr(range) if is_in => range,
            _ => return self.compile_for_of(for_stmt, &iter, &body),
        };

        // Inclusive `..=` for-range is rejected by the legacy parser (the oracle),
        // so the VM rejects it too — never silently treat it as exclusive.
        if range.op() == Some(SyntaxKind::DotDotEq) {
            return Err(CompileError::new(
                "inclusive for-range (`..=`) is not supported (the interpreter rejects it)",
                span,
            ));
        }

        let start = range
            .start()
            .ok_or_else(|| CompileError::new("for-range missing start bound", span))?;
        let end = range
            .end()
            .ok_or_else(|| CompileError::new("for-range missing end bound", span))?;

        let var_slot = self.for_loop_var_slot(for_stmt)?;
        let end_slot = self.alloc_temp()?;
        // A SCRATCH induction counter drives the loop, separate from the
        // user-visible loop variable `i`. The tree-walker iterates with its own
        // `f64` counter and `define`s a FRESH `var` binding each iteration, so
        // mutating `i` inside the body never changes loop progression. Driving the
        // loop from a scratch slot (never a cell, never captured) reproduces that:
        // we re-derive `i` from the counter at the TOP of each iteration, AFTER
        // installing a fresh cell for it (per-iteration capture freshness).
        let idx_slot = self.alloc_temp()?;
        // Cell slots to refresh per iteration: the loop var `i` (if captured) plus
        // any captured `let`/`fn` declared in the loop BODY.
        let refresh_slots = self.loop_refresh_slots(&body, Some(var_slot));
        // Anchor the bounds-numbers panic at the START bound's CODE start (trivia
        // trimmed), byte-identical to the tree-walker's `AsError::at(_, start.span)`.
        let start_span = node_code_span(&start);

        // Evaluate start then end (start below, end on top), guard both are
        // numbers (panic anchored at the START bound's span, matching the
        // tree-walker), then store end ONCE and seed the counter `idx = start`.
        self.compile_expr(&start)?;
        self.compile_expr(&end)?;
        self.chunk.emit(Op::CheckNumbers, start_span);
        self.chunk.emit_u16(Op::SetLocal, end_slot, span);
        self.chunk.emit_u16(Op::SetLocal, idx_slot, span);

        // Condition: re-test `idx < end` each iteration.
        let cond_start = self.chunk.code.len();
        self.chunk.emit_u16(Op::GetLocal, idx_slot, span);
        self.chunk.emit_u16(Op::GetLocal, end_slot, span);
        self.chunk.emit(Op::Lt, span);
        let exit = self.chunk.emit_jump(Op::JumpIfFalse, span);

        // Top of the iteration: give the loop var (and any loop-body captured
        // lets) a FRESH cell so a closure created this iteration captures only this
        // iteration's value, then bind `i = idx` into the (fresh) loop-var slot.
        self.emit_fresh_cells(&refresh_slots, span);
        self.chunk.emit_u16(Op::GetLocal, idx_slot, span);
        self.emit_set_local(var_slot, span);

        // The continue target is the INCREMENT (so `continue` runs `idx += 1` then
        // re-tests, exactly like the tree-walker's `Flow::Continue` falling through
        // to `i += 1.0`). The increment is emitted AFTER the body, so its offset
        // is not yet known: push the ctx with `continue_target: None` so each
        // `continue` records a forward `Jump` site, then patch them all to the
        // increment below.
        self.loops.push(LoopCtx {
            continue_target: None,
            continue_sites: Vec::new(),
            break_sites: Vec::new(),
        });
        self.compile_block(&body)?;

        // Increment: `idx = idx + 1`. This is where every `continue` lands — at the
        // CURRENT end of code, which is exactly what `patch_jump` targets — so
        // patch every recorded forward `continue` site here, BEFORE emitting the
        // increment instructions. The counter is the scratch slot, NOT the
        // user-visible `i`, so body mutation of `i` cannot affect progression.
        let continue_sites = std::mem::take(
            &mut self
                .loops
                .last_mut()
                .expect("for-range loop context present")
                .continue_sites,
        );
        for site in continue_sites {
            self.chunk.patch_jump(site);
        }
        self.chunk.emit_u16(Op::GetLocal, idx_slot, span);
        let one = self.chunk.add_const(Value::Number(1.0));
        self.chunk.emit_u16(Op::Const, one, span);
        self.chunk.emit(Op::Add, span);
        self.chunk.emit_u16(Op::SetLocal, idx_slot, span);

        // Back-edge to re-test the condition.
        self.chunk.emit_loop(Op::Loop, cond_start, span);
        let ctx = self
            .loops
            .pop()
            .expect("for-range loop context pushed before body must still be present");

        // Loop exit: `i >= end` lands here; every `break` does too.
        self.chunk.patch_jump(exit);
        for site in ctx.break_sites {
            self.chunk.patch_jump(site);
        }
        Ok(())
    }

    /// Compile a `for (x of iterable) { body }` SYNC for-of. Mirrors the
    /// tree-walker's `Stmt::ForOf` (`for_await == false`, src/interp.rs) exactly:
    /// evaluate the iterable, SNAPSHOT it into a fixed list of items (an `Array`
    /// clones its current elements; a `Str` yields its chars each as a 1-char
    /// string; anything else — incl. object/map/set — is the Tier-2 panic `value of
    /// type {t} is not iterable` at the ITERABLE's span), then bind `x` to each item
    /// in turn and run the body. `break` exits, `continue` advances to the next
    /// item, `return` returns. The snapshot means mutating the source array inside
    /// the body does NOT change what is iterated, byte-identically to the
    /// tree-walker.
    ///
    /// Lowering (scratch-slot index iteration, like for-range):
    /// ```text
    ///   <iterable>                 ; iterable on top
    ///   ITER_SNAPSHOT              ; -> snapshot array (panic @ iter.span if not iterable)
    ///   SET_LOCAL arr_slot         ; arr_slot = alloc_temp(); the fixed snapshot
    ///   GET_LOCAL arr_slot; ARRAY_LEN; SET_LOCAL len_slot   ; len_slot = alloc_temp()
    ///   CONST 0.0; SET_LOCAL idx_slot                       ; idx_slot = alloc_temp() = 0
    /// cond_start:
    ///   GET_LOCAL idx_slot; GET_LOCAL len_slot; LT
    ///   exit = JUMP_IF_FALSE       ; exit when idx >= len
    ///   GET_LOCAL arr_slot; GET_LOCAL idx_slot; GET_INDEX; SET_LOCAL var_slot  ; x = arr[idx]
    ///   <body block>               ; loop ctx: continue_target = increment below
    ///   increment:                 ; continue lands here → idx += 1 then re-test
    ///   GET_LOCAL idx_slot; CONST 1.0; ADD; SET_LOCAL idx_slot
    ///   LOOP cond_start
    /// exit:
    ///   patch(break_sites...)      ; each `break` lands here
    /// ```
    /// `var_slot` comes from the resolver's `LoopVar` binding (matched by
    /// `decl_range == for_stmt.text_range()`, same as for-range).
    fn compile_for_of(
        &mut self,
        for_stmt: &ForStmt,
        iter: &Expr,
        body: &Block,
    ) -> Result<(), CompileError> {
        let span = node_span(for_stmt);

        let var_slot = self.for_loop_var_slot(for_stmt)?;
        let arr_slot = self.alloc_temp()?;
        let len_slot = self.alloc_temp()?;
        let idx_slot = self.alloc_temp()?;
        // Cell slots to refresh per iteration: the loop var `x` (if captured) plus
        // any captured `let`/`fn` declared in the loop BODY.
        let refresh_slots = self.loop_refresh_slots(body, Some(var_slot));

        // Anchor the "not iterable" panic at the iterable expression's CODE span
        // (trivia-trimmed), byte-identical to the tree-walker's `AsError::at(_,
        // iter.span)`.
        let iter_span = node_code_span(iter);

        // Build the snapshot once: evaluate the iterable, materialize the fixed
        // items array (panic anchored at the iterable's span), store it, then
        // hoist its (fixed) length into a slot. Seed idx = 0.
        self.compile_expr(iter)?;
        self.chunk.emit(Op::IterSnapshot, iter_span);
        self.chunk.emit_u16(Op::SetLocal, arr_slot, span);
        self.chunk.emit_u16(Op::GetLocal, arr_slot, span);
        self.chunk.emit(Op::ArrayLen, span);
        self.chunk.emit_u16(Op::SetLocal, len_slot, span);
        let zero = self.chunk.add_const(Value::Number(0.0));
        self.chunk.emit_u16(Op::Const, zero, span);
        self.chunk.emit_u16(Op::SetLocal, idx_slot, span);

        // Condition: re-test `idx < len` each iteration.
        let cond_start = self.chunk.code.len();
        self.chunk.emit_u16(Op::GetLocal, idx_slot, span);
        self.chunk.emit_u16(Op::GetLocal, len_slot, span);
        self.chunk.emit(Op::Lt, span);
        let exit = self.chunk.emit_jump(Op::JumpIfFalse, span);

        // Top of the iteration: give the loop var (and any loop-body captured
        // lets) a FRESH cell so a closure created this iteration captures only this
        // iteration's value, then bind the loop var to `arr[idx]`.
        self.emit_fresh_cells(&refresh_slots, span);
        self.chunk.emit_u16(Op::GetLocal, arr_slot, span);
        self.chunk.emit_u16(Op::GetLocal, idx_slot, span);
        self.chunk.emit(Op::GetIndex, span);
        self.emit_set_local(var_slot, span);

        // The continue target is the INCREMENT (so `continue` advances to the next
        // item then re-tests, exactly like the tree-walker's `Flow::Continue`
        // moving to the next `item`). The increment is emitted AFTER the body, so
        // push the ctx with `continue_target: None` and patch each forward
        // `continue` site to the increment below.
        self.loops.push(LoopCtx {
            continue_target: None,
            continue_sites: Vec::new(),
            break_sites: Vec::new(),
        });
        self.compile_block(body)?;

        // Increment: `idx = idx + 1`. Every `continue` lands here (the current end
        // of code), so patch the recorded forward sites BEFORE emitting it.
        let continue_sites = std::mem::take(
            &mut self
                .loops
                .last_mut()
                .expect("for-of loop context present")
                .continue_sites,
        );
        for site in continue_sites {
            self.chunk.patch_jump(site);
        }
        self.chunk.emit_u16(Op::GetLocal, idx_slot, span);
        let one = self.chunk.add_const(Value::Number(1.0));
        self.chunk.emit_u16(Op::Const, one, span);
        self.chunk.emit(Op::Add, span);
        self.chunk.emit_u16(Op::SetLocal, idx_slot, span);

        // Back-edge to re-test the condition.
        self.chunk.emit_loop(Op::Loop, cond_start, span);
        let ctx = self
            .loops
            .pop()
            .expect("for-of loop context pushed before body must still be present");

        // Loop exit: `idx >= len` lands here; every `break` does too.
        self.chunk.patch_jump(exit);
        for site in ctx.break_sites {
            self.chunk.patch_jump(site);
        }
        Ok(())
    }

    /// Compile a `for await (x of iterable) { body }` ASYNC for-of. Mirrors the
    /// tree-walker's `Stmt::ForOf { for_await: true }` → `exec_for_await`
    /// (`src/interp.rs`) exactly: the iterable must be async-iterable — a
    /// `Value::Generator` (driven LAZILY via an awaiting `resume`) or a native
    /// stream handle (WebSocket `recv` / SSE `next`); ANY OTHER value is the Tier-2
    /// panic `value of type {t} is not async-iterable` at the iterable's span
    /// (raised by `GET_ITER`). Each produced value binds `x` in a fresh child scope
    /// and runs the body; `break`/early `return` CLOSE the generator (the
    /// tree-walker's `g.close()`), `continue` advances to the next value, natural
    /// exhaustion (`None`/end-of-stream) ends the loop without closing.
    ///
    /// Unlike sync for-of there is NO snapshot — generators are lazy and cannot be
    /// materialized up front. The iterable is stashed in a scratch slot and driven
    /// one step per iteration by `ITER_NEXT` (async).
    ///
    /// Lowering:
    /// ```text
    ///   <iterable>                 ; iterable on top
    ///   GET_ITER                   ; validate async-iterable (panic @ iter.span)
    ///   SET_LOCAL it_slot          ; it_slot = alloc_temp(); the live iterator
    /// cond_start:
    ///   GET_LOCAL it_slot; ITER_NEXT   ; -> value, done   (async step)
    ///   exit = JUMP_IF_TRUE        ; done==true → exit (pops `done`, leaves value)
    ///   <fresh cells>              ; per-iteration capture freshness
    ///   SET_LOCAL var_slot         ; x = value (pops the value left by ITER_NEXT)
    ///   <body block>               ; loop ctx: continue → cond_start, break → exit
    ///   LOOP cond_start
    /// exit:                        ; natural exhaustion lands here with the leftover
    ///   POP                        ;   `value` (nil) on the stack — discard it
    ///   patch(break_sites...)      ; each `break` jumps to break_exit (closes first)
    ///   ...
    /// ```
    /// On `done`, `ITER_NEXT` pushed `value=nil` (below) and `done=true` (on top);
    /// `JUMP_IF_TRUE` pops only `done`, so the `nil` value remains and is discarded
    /// by the `POP` at the exit label. A `break` cannot reach that `POP` (it has no
    /// leftover value), so breaks jump to a SEPARATE `break_exit` that first
    /// `ITER_CLOSE`s the iterator then merges into the common tail.
    fn compile_for_await(
        &mut self,
        for_stmt: &ForStmt,
        iter: &Expr,
        body: &Block,
    ) -> Result<(), CompileError> {
        let span = node_span(for_stmt);

        let var_slot = self.for_loop_var_slot(for_stmt)?;
        let it_slot = self.alloc_temp()?;
        // Cell slots to refresh per iteration: the loop var `x` (if captured) plus
        // any captured `let`/`fn` declared in the loop BODY.
        let refresh_slots = self.loop_refresh_slots(body, Some(var_slot));

        // Anchor the "not async-iterable" panic at the iterable expression's CODE
        // span (trivia-trimmed), byte-identical to the tree-walker's `AsError::at(_,
        // span)` where `span = iter.span`.
        let iter_span = node_code_span(iter);

        // Evaluate the iterable, validate it is async-iterable, stash it.
        self.compile_expr(iter)?;
        self.chunk.emit(Op::GetIter, iter_span);
        self.chunk.emit_u16(Op::SetLocal, it_slot, span);

        // Condition: drive one lazy step; `done` exits the loop.
        let cond_start = self.chunk.code.len();
        self.chunk.emit_u16(Op::GetLocal, it_slot, span);
        // ITER_NEXT is anchored at the iterable's span so a generator-body panic or
        // a stream error surfaces at the loop's iterable, matching the tree-walker's
        // `exec_for_await` error sites.
        self.chunk.emit(Op::IterNext, iter_span);
        let exit = self.chunk.emit_jump(Op::JumpIfTrue, span);

        // Top of the iteration: fresh cells for captured bindings, then bind the
        // loop var to the produced value (SET_LOCAL pops it — clean stack).
        self.emit_fresh_cells(&refresh_slots, span);
        self.emit_set_local(var_slot, span);

        // The continue target is `cond_start` (re-drive the iterator), exactly like
        // the tree-walker's `Flow::Continue` looping back to the next `resume`.
        self.loops.push(LoopCtx {
            continue_target: Some(cond_start),
            continue_sites: Vec::new(),
            break_sites: Vec::new(),
        });
        self.compile_block(body)?;

        // Back-edge to re-test (drive the next step).
        self.chunk.emit_loop(Op::Loop, cond_start, span);
        let ctx = self
            .loops
            .pop()
            .expect("for-await loop context pushed before body must still be present");

        // Natural-exhaustion exit: ITER_NEXT left a leftover `value` (nil) below the
        // popped `done`, so discard it here.
        self.chunk.patch_jump(exit);
        self.chunk.emit(Op::Pop, span);
        let tail = self.chunk.emit_jump(Op::Jump, span);

        // Break exit: a `break` jumps here with NO leftover value on the stack (it
        // left the loop mid-body). Close the iterator (`g.close()` — the
        // tree-walker's behavior on break), then merge into the common tail.
        for site in ctx.break_sites {
            self.chunk.patch_jump(site);
        }
        self.chunk.emit_u16(Op::GetLocal, it_slot, span);
        self.chunk.emit(Op::IterClose, span);

        // Common tail: both exits converge here.
        self.chunk.patch_jump(tail);
        Ok(())
    }

    /// The local slot for a for-range loop variable. The resolver declares the
    /// `LoopVar` binding with `decl_range` set to the whole `ForStmt`'s
    /// `text_range()` (see `resolve_stmt`'s `ForStmt` arm), so we match the binding
    /// by that range — the same scheme `let_slot` uses.
    fn for_loop_var_slot(&self, for_stmt: &ForStmt) -> Result<u16, CompileError> {
        let span = node_span(for_stmt);
        let decl_range: TextRange = for_stmt.syntax().text_range();
        let binding = self
            .resolved
            .bindings
            .iter()
            .find(|b| b.decl_range == decl_range)
            .ok_or_else(|| {
                CompileError::new(
                    "for-loop variable has no resolver binding (compiler bug)",
                    span,
                )
            })?;
        u16::try_from(binding.slot)
            .map_err(|_| CompileError::new("local slot index exceeds 65535", span))
    }

    /// Compile a `let`/`const` declaration: evaluate the initializer (or push
    /// `Nil` for an initializer-less `let x`), then `SET_LOCAL` into the binding's
    /// slot. `SET_LOCAL` pops the value (clean stack discipline), so no leftover
    /// remains. `const` binds identically at runtime — immutability is enforced by
    /// the resolver/checker, not the VM (the tree-walker's `Stmt::Let` likewise
    /// just binds). Destructuring `let` (`let [..]`/`let {..}`) is V10.
    fn compile_let(&mut self, let_stmt: &LetStmt) -> Result<(), CompileError> {
        let span = node_span(let_stmt);

        // A destructuring binder has an ArrayBindPat/ObjectBindPat child instead
        // of a plain ident token.
        if let Some(pat) = let_stmt
            .syntax()
            .children()
            .find(|c| matches!(c.kind(), SyntaxKind::ArrayBindPat | SyntaxKind::ObjectBindPat))
        {
            return self.compile_let_destructure(let_stmt, pat);
        }

        let slot = self.let_slot(let_stmt)?;

        match let_stmt.expr() {
            Some(init) => self.compile_expr(&init)?,
            // `let x` with no initializer binds nil (mirrors the tree-walker).
            None => self.chunk.emit(Op::Nil, span),
        }
        self.emit_set_local(slot, span);
        Ok(())
    }

    /// The local slot for a `let`/`const` declaration. The resolver records a
    /// `Binding` whose `decl_range` is the declaration node's `text_range()`
    /// (see `declare_let_bindings`), so we match the binding by that range.
    fn let_slot(&self, let_stmt: &LetStmt) -> Result<u16, CompileError> {
        let span = node_span(let_stmt);
        let decl_range: TextRange = let_stmt.syntax().text_range();
        let binding = self
            .resolved
            .bindings
            .iter()
            .find(|b| b.decl_range == decl_range)
            .ok_or_else(|| {
                CompileError::new("let declaration has no resolver binding (compiler bug)", span)
            })?;
        u16::try_from(binding.slot)
            .map_err(|_| CompileError::new("local slot index exceeds 65535", span))
    }

    /// The local slot the resolver assigned to a pattern binding (a `BindEntry` or
    /// `RestBind` node). The resolver records a `BindingKind::PatternBind` binding
    /// whose `decl_range` is the ENTRY node's `text_range()` (see
    /// `declare_pattern_names`), so we match by that range.
    fn pattern_bind_slot(&self, entry: &ResolvedNode, span: Span) -> Result<u16, CompileError> {
        let decl_range: TextRange = entry.text_range();
        let binding = self
            .resolved
            .bindings
            .iter()
            .find(|b| b.decl_range == decl_range)
            .ok_or_else(|| {
                CompileError::new(
                    "destructuring binding has no resolver binding (compiler bug)",
                    span,
                )
            })?;
        u16::try_from(binding.slot)
            .map_err(|_| CompileError::new("local slot index exceeds 65535", span))
    }

    /// Compile a destructuring `let`/`const` — `let [a, b, ...r] = rhs` or
    /// `let {a, b as local, "k" as v, ...rest} = rhs`. Mirrors the tree-walker's
    /// `Stmt::LetDestructure` (array) and `Stmt::LetDestructureObject` (object)
    /// EXACTLY: validate the RHS once (type panic at the RHS span), bind each
    /// position/key (missing → nil), then the optional `...rest` collector (array
    /// tail / leftover object keys).
    ///
    /// Lowering — the RHS is evaluated ONCE into a TEMP slot (it is read once per
    /// binding), then each binding loads that temp and extracts its slice:
    /// ```text
    ///   <rhs>; SET_LOCAL temp                ; temp = rhs (evaluated once)
    ///   GET_LOCAL temp; CHECK_(ARRAY|OBJECT)_DESTRUCTURE; POP   ; validate once
    ///   ; per binding:
    ///   GET_LOCAL temp; (ARRAY_ELEM i | OBJECT_KEY key); SET_LOCAL[_CELL] slot
    ///   ; optional rest:
    ///   GET_LOCAL temp; (ARRAY_REST n | OBJECT_REST bound_keys); SET_LOCAL[_CELL] slot
    /// ```
    fn compile_let_destructure(
        &mut self,
        let_stmt: &LetStmt,
        pat: &ResolvedNode,
    ) -> Result<(), CompileError> {
        use SyntaxKind::*;
        let span = node_span(let_stmt);
        let init = let_stmt.expr().ok_or_else(|| {
            CompileError::new("destructuring let has no initializer expression", span)
        })?;
        // The RHS expression's span (trivia-trimmed) — where the tree-walker anchors
        // the "cannot destructure a non-array/object value" type panic (`value.span`).
        let rhs_span = node_code_span(&init);

        // Evaluate the RHS once into a temp slot (it is read once per binding).
        let temp = self.alloc_temp()?;
        self.compile_expr(&init)?;
        self.chunk.emit_u16(Op::SetLocal, temp, span);

        let is_array = pat.kind() == ArrayBindPat;

        // Validate the RHS type ONCE, before any binding — byte-identical to the
        // tree-walker, which checks the type first and panics at the RHS span.
        self.chunk.emit_u16(Op::GetLocal, temp, rhs_span);
        self.chunk.emit(
            if is_array {
                Op::CheckArrayDestructure
            } else {
                Op::CheckObjectDestructure
            },
            rhs_span,
        );
        self.chunk.emit(Op::Pop, rhs_span);

        let entries: Vec<ResolvedNode> = pat
            .children()
            .filter(|c| matches!(c.kind(), BindEntry | RestBind))
            .cloned()
            .collect();

        // Positional index for array elements (array only).
        let mut pos: u16 = 0;
        // Bound keys, in order, for the object-rest exclusion set (object only).
        let mut bound_keys: Vec<Value> = Vec::new();

        for entry in &entries {
            let espan = range_span(entry);
            match entry.kind() {
                BindEntry if is_array => {
                    let slot = self.pattern_bind_slot(entry, espan)?;
                    self.chunk.emit_u16(Op::GetLocal, temp, span);
                    self.chunk.emit_u16(Op::ArrayElem, pos, span);
                    self.emit_set_local(slot, span);
                    pos = pos.checked_add(1).ok_or_else(|| {
                        CompileError::new("array pattern has too many bindings", span)
                    })?;
                }
                BindEntry => {
                    // Object entry: key by the FIRST significant token (Ident or
                    // Str); the local name is the resolver binding's slot.
                    let key = bind_entry_key(entry)
                        .ok_or_else(|| CompileError::new("destructuring entry has no key", espan))?;
                    let slot = self.pattern_bind_slot(entry, espan)?;
                    let key_idx = self.chunk.add_const(Value::Str(Rc::from(key.as_str())));
                    bound_keys.push(Value::Str(Rc::from(key.as_str())));
                    self.chunk.emit_u16(Op::GetLocal, temp, span);
                    self.chunk.emit_u16(Op::ObjectKey, key_idx, span);
                    self.emit_set_local(slot, span);
                }
                RestBind => {
                    let slot = self.pattern_bind_slot(entry, espan)?;
                    self.chunk.emit_u16(Op::GetLocal, temp, span);
                    if is_array {
                        // `arr[pos..]` — the tail past the named positions.
                        self.chunk.emit_u16(Op::ArrayRest, pos, span);
                    } else {
                        // Leftover keys — those not in `bound_keys`. The bound-key
                        // set is stored as a single Array const referenced by index.
                        let keys =
                            Value::Array(Rc::new(std::cell::RefCell::new(bound_keys.clone())));
                        let keys_idx = self.chunk.add_const(keys);
                        self.chunk.emit_u16(Op::ObjectRest, keys_idx, span);
                    }
                    self.emit_set_local(slot, span);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Compile a `match subject { arm... }` EXPRESSION. Mirrors the tree-walker's
    /// `ExprKind::Match` + `match_pattern` BYTE-FOR-BYTE: the subject is evaluated
    /// once; arms are tested top-to-bottom; within an arm each `|`-alternative is
    /// tried in order; an alternative whose pattern matches AND whose guard passes
    /// runs that arm's body (the match's value). A **failed guard falls through to
    /// the NEXT alternative** (the tree-walker `continue`s its per-pattern loop),
    /// then to the next arm once all alternatives are exhausted. No arm matches →
    /// the Tier-2 panic `no matching arm in match expression` at the match span.
    ///
    /// Lowering:
    /// ```text
    ///   <subject>; SET_LOCAL subj_temp        ; eval the subject ONCE
    ///   ; per arm, per |-alternative:
    ///     <pattern tests against subj_temp>    ; each test: push bool;
    ///                                           ; JUMP_IF_FALSE next_alt
    ///     ; (pattern binds happen eagerly into the resolver's arm-local slots
    ///     ;  during the tests above)
    ///     <guard>? ; JUMP_IF_FALSE next_alt    ; guard failure → next alternative
    ///     <body>; JUMP match_end               ; arm matched → push value, done
    ///   next_alt:                              ; the next alternative / next arm
    ///   ...
    ///   MATCH_NO_ARM                           ; reached only if nothing matched
    ///   match_end:                             ; ONE body value on the stack
    /// ```
    /// Exactly one body runs, each body pushes one value, and the no-match path
    /// diverges (panics) — so the net stack effect is +1.
    fn compile_match(&mut self, m: &MatchExpr) -> Result<(), CompileError> {
        let span = node_code_span(m);
        let subject = m
            .subject()
            .ok_or_else(|| CompileError::new("match expression missing subject", span))?;

        // Evaluate the subject ONCE into a temp slot (read once per pattern test).
        let subj_temp = self.alloc_temp()?;
        self.compile_expr(&subject)?;
        self.chunk.emit_u16(Op::SetLocal, subj_temp, span);

        // Forward jumps from each matched arm body to the match end.
        let mut end_jumps: Vec<usize> = Vec::new();

        for arm in m.match_arms() {
            // The `|`-alternatives. With alternatives the parser wraps them in an
            // `OrPat`; `MatchArm::pats()` returns the typed `Pat`s directly only when
            // there is NO `OrPat`. So gather the alternative pattern NODES robustly:
            // an `OrPat` child's pattern children, else the arm's direct pattern.
            let alts = self.arm_alternatives(&arm)?;
            let guard = arm.match_guard();

            for alt in &alts {
                // Fail sites for THIS alternative — a failed pattern test or a failed
                // guard jumps here, to the start of the next alternative / next arm.
                let mut fail_sites: Vec<usize> = Vec::new();
                // Emit the pattern test (binds eagerly; appends fail-jump sites).
                self.compile_pattern_test(alt, subj_temp, span, &mut fail_sites)?;
                // Guard (if any): evaluated AFTER the binds, in the arm scope. A
                // falsy guard falls through to the next alternative (tree-walker
                // `continue`), so it shares the alternative's fail target.
                if let Some(g) = &guard {
                    let gexpr = g
                        .expr()
                        .ok_or_else(|| CompileError::new("match guard missing condition", span))?;
                    self.compile_expr(&gexpr)?;
                    fail_sites.push(self.chunk.emit_jump(Op::JumpIfFalse, span));
                }
                // Matched (+ guard passed): run the body and jump to the end.
                let body = arm
                    .body()
                    .ok_or_else(|| CompileError::new("match arm missing body", span))?;
                self.compile_expr(&body)?;
                end_jumps.push(self.chunk.emit_jump(Op::Jump, span));
                // The next alternative / next arm begins here: patch every fail jump
                // of THIS alternative to land at the current position.
                for site in fail_sites {
                    self.chunk.patch_jump(site);
                }
            }
        }

        // No arm matched → panic, byte-identical to the tree-walker.
        self.chunk.emit(Op::MatchNoArm, span);

        // All matched-arm bodies converge here with one value on the stack.
        for site in end_jumps {
            self.chunk.patch_jump(site);
        }
        Ok(())
    }

    /// Gather an arm's `|`-alternative pattern NODES. The parser wraps two-or-more
    /// alternatives in an `OrPat` node (`MatchArm = Pat ('|' Pat)*`), so the
    /// alternatives are that `OrPat`'s pattern children; a single-pattern arm has
    /// the pattern as a direct child. Returns the raw `ResolvedNode`s (we resolve
    /// binds/compares per node via the resolver, keyed by `text_range`).
    fn arm_alternatives(
        &self,
        arm: &MatchArm,
    ) -> Result<Vec<ResolvedNode>, CompileError> {
        use SyntaxKind::*;
        let mut out: Vec<ResolvedNode> = Vec::new();
        for child in arm.syntax().children() {
            match child.kind() {
                OrPat => {
                    for sub in child.children().filter(|c| is_pattern_kind(c.kind())) {
                        out.push(sub.clone());
                    }
                }
                k if is_pattern_kind(k) => out.push(child.clone()),
                _ => {}
            }
        }
        if out.is_empty() {
            return Err(CompileError::new(
                "match arm has no pattern",
                range_span(arm.syntax()),
            ));
        }
        Ok(out)
    }

    /// Emit a test for `pat` against the value in local slot `subj_temp`. Each
    /// structural check pushes a boolean and is followed by a `JUMP_IF_FALSE` whose
    /// site is appended to `fail_sites` (the caller patches them to the
    /// next-alternative target). Binds happen eagerly into the resolver's arm-local
    /// slots — a later sub-test failing just discards them (the arm's slots are
    /// overwritten / never read), matching the tree-walker's partial-bind-then-fail.
    fn compile_pattern_test(
        &mut self,
        pat: &ResolvedNode,
        subj_temp: u16,
        span: Span,
        fail_sites: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        use SyntaxKind::*;
        match pat.kind() {
            // `_` always matches: no test, no bind.
            WildcardPat => Ok(()),
            // A bare ident (Option-C) OR a value expression.
            LiteralPat => self.compile_literal_pattern(pat, subj_temp, span, fail_sites),
            RangePat => self.compile_range_pattern(pat, subj_temp, span, fail_sites),
            ArrayPat => self.compile_array_pattern(pat, subj_temp, span, fail_sites),
            ObjectPat => self.compile_object_pattern(pat, subj_temp, span, fail_sites),
            other => Err(CompileError::new(
                format!("unsupported match pattern kind {other:?}"),
                range_span(pat),
            )),
        }
    }

    /// A `LiteralPat` — either a bare-ident pattern (Option-C: COMPARE if the
    /// resolver resolved the ident as a use, BIND if the resolver allocated a
    /// pattern slot for it) or a value expression (eval + `==`). The resolver is the
    /// single source of truth for the bind-vs-compare decision so the VM and checker
    /// agree EXACTLY (`resolve_pattern`'s `LiteralPat` arm).
    fn compile_literal_pattern(
        &mut self,
        pat: &ResolvedNode,
        subj_temp: u16,
        span: Span,
        fail_sites: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        // Is this a single bare `NameRef`? If so it is subject to Option-C.
        if let Some(name_ref) = bare_ident_name_ref(pat) {
            let key = name_ref.text_range();
            // The resolver records a `use` (Resolution) for a COMPARE ident; for a
            // BIND it records NO use and instead allocated a PatternBind slot whose
            // `decl_range` is the LiteralPat node's `text_range()`.
            if let Some(res) = self.resolved.uses.get(&key).cloned() {
                // COMPARE: load subject, push the resolved value, `==`, fail if false.
                self.emit_get_local_temp(subj_temp, span);
                self.emit_resolution_read(&res, span)?;
                self.chunk.emit(Op::Eq, span);
                fail_sites.push(self.chunk.emit_jump(Op::JumpIfFalse, span));
                return Ok(());
            }
            // BIND: store the subject into the pattern slot (always matches).
            let slot = self.pattern_bind_slot(pat, range_span(pat))?;
            self.emit_get_local_temp(subj_temp, span);
            self.emit_set_local(slot, span);
            return Ok(());
        }
        // A value expression (literal / member like `Shape.Circle` / etc.): eval and
        // compare for equality, exactly like the tree-walker's `Pattern::Value`.
        let inner = pat.children().find(|c| is_expr_kind(c.kind())).ok_or_else(|| {
            CompileError::new("literal pattern has no value expression", range_span(pat))
        })?;
        let inner_expr = Expr::cast(inner.clone()).ok_or_else(|| {
            CompileError::new("literal pattern value is not an expression", range_span(pat))
        })?;
        self.emit_get_local_temp(subj_temp, span);
        self.compile_expr(&inner_expr)?;
        self.chunk.emit(Op::Eq, span);
        fail_sites.push(self.chunk.emit_jump(Op::JumpIfFalse, span));
        Ok(())
    }

    /// A `RangePat` `start..end` / `start..=end`. Mirrors the tree-walker's
    /// `Pattern::Range`: a non-number subject OR non-number bound is a (non-panic)
    /// mismatch. Lowering: push subject, lo, hi; `MATCH_RANGE inclusive` → bool.
    fn compile_range_pattern(
        &mut self,
        pat: &ResolvedNode,
        subj_temp: u16,
        span: Span,
        fail_sites: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        let exprs: Vec<ResolvedNode> = pat
            .children()
            .filter(|c| is_expr_kind(c.kind()))
            .cloned()
            .collect();
        if exprs.len() != 2 {
            return Err(CompileError::new(
                "range pattern must have a start and end bound",
                range_span(pat),
            ));
        }
        let inclusive = pat
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::DotDotEq);
        let lo = Expr::cast(exprs[0].clone())
            .ok_or_else(|| CompileError::new("range start is not an expression", range_span(pat)))?;
        let hi = Expr::cast(exprs[1].clone())
            .ok_or_else(|| CompileError::new("range end is not an expression", range_span(pat)))?;
        self.emit_get_local_temp(subj_temp, span);
        self.compile_expr(&lo)?;
        self.compile_expr(&hi)?;
        self.chunk
            .emit_u8(Op::MatchRange, u8::from(inclusive), span);
        fail_sites.push(self.chunk.emit_jump(Op::JumpIfFalse, span));
        Ok(())
    }

    /// An `ArrayPat` `[p0, p1, ...rest]`. Mirrors the tree-walker's `Pattern::Array`:
    /// the subject must be an Array of the right length (exact without a rest, `>=`
    /// the fixed count with one), then each fixed position is tested recursively and
    /// the optional `...rest` collects the tail.
    fn compile_array_pattern(
        &mut self,
        pat: &ResolvedNode,
        subj_temp: u16,
        span: Span,
        fail_sites: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        use SyntaxKind::*;
        let fixed: Vec<ResolvedNode> = pat
            .children()
            .filter(|c| is_pattern_kind(c.kind()))
            .cloned()
            .collect();
        let rest: Option<ResolvedNode> =
            pat.children().find(|c| c.kind() == PatRest).cloned();
        let fixed_len = u16::try_from(fixed.len()).map_err(|_| {
            CompileError::new("array pattern has too many elements", range_span(pat))
        })?;

        // Length/type test: `MATCH_ARRAY fixed_len, exact?` → bool.
        self.emit_get_local_temp(subj_temp, span);
        self.chunk.emit_u16_u8(
            Op::MatchArray,
            fixed_len,
            u8::from(rest.is_none()),
            span,
        );
        fail_sites.push(self.chunk.emit_jump(Op::JumpIfFalse, span));

        // Each fixed position: extract `subject[i]` into a fresh temp, recurse.
        for (i, sub) in fixed.iter().enumerate() {
            let i = u16::try_from(i).map_err(|_| {
                CompileError::new("array pattern index exceeds 65535", range_span(pat))
            })?;
            let elem_temp = self.alloc_temp()?;
            self.emit_get_local_temp(subj_temp, span);
            self.chunk.emit_u16(Op::ArrayElem, i, span);
            self.chunk.emit_u16(Op::SetLocal, elem_temp, span);
            self.compile_pattern_test(sub, elem_temp, span, fail_sites)?;
        }

        // `...rest`: bind the tail (`subject[fixed_len..]`) into the rest slot. A
        // bare `...` (no name) is a discard — collect nothing.
        if let Some(rest) = rest {
            if rest.children_with_tokens().any(|el| {
                el.as_token().map(|t| t.kind() == Ident).unwrap_or(false)
            }) {
                let slot = self.pattern_bind_slot(&rest, range_span(&rest))?;
                self.emit_get_local_temp(subj_temp, span);
                self.chunk.emit_u16(Op::ArrayRest, fixed_len, span);
                self.emit_set_local(slot, span);
            }
        }
        Ok(())
    }

    /// An `ObjectPat` `{k0, k1: sub, ...rest}`. Mirrors the tree-walker's
    /// `Pattern::Object`: the subject must be an Object/Instance; each entry's key
    /// must be present (`MATCH_HAS_KEY`), then the entry binds (shorthand) or tests a
    /// sub-pattern on `subject[key]`; the optional `...rest` collects leftover keys.
    fn compile_object_pattern(
        &mut self,
        pat: &ResolvedNode,
        subj_temp: u16,
        span: Span,
        fail_sites: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        use SyntaxKind::*;
        // Type test: `MATCH_OBJECT` → bool.
        self.emit_get_local_temp(subj_temp, span);
        self.chunk.emit(Op::MatchObject, span);
        fail_sites.push(self.chunk.emit_jump(Op::JumpIfFalse, span));

        // Bound keys (in source order) for the object-rest exclusion set.
        let mut bound_keys: Vec<Value> = Vec::new();

        for entry in pat.children().filter(|c| c.kind() == ObjPatEntry) {
            let key = bind_entry_key(entry).ok_or_else(|| {
                CompileError::new("object pattern entry has no key", range_span(entry))
            })?;
            bound_keys.push(Value::Str(Rc::from(key.as_str())));
            let key_idx = self.chunk.add_const(Value::Str(Rc::from(key.as_str())));

            // Presence test: `MATCH_HAS_KEY key` (pops the subject, pushes bool).
            self.emit_get_local_temp(subj_temp, span);
            self.chunk.emit_u16(Op::MatchHasKey, key_idx, span);
            fail_sites.push(self.chunk.emit_jump(Op::JumpIfFalse, span));

            // A sub-pattern (`key: subpat`) tests `subject[key]`; a shorthand `{key}`
            // is ALWAYS a bind (documented Option-C exception).
            if let Some(subpat) = entry.children().find(|c| is_pattern_kind(c.kind())) {
                let val_temp = self.alloc_temp()?;
                self.emit_get_local_temp(subj_temp, span);
                self.chunk.emit_u16(Op::ObjectKey, key_idx, span);
                self.chunk.emit_u16(Op::SetLocal, val_temp, span);
                self.compile_pattern_test(subpat, val_temp, span, fail_sites)?;
            } else {
                let slot = self.pattern_bind_slot(entry, range_span(entry))?;
                self.emit_get_local_temp(subj_temp, span);
                self.chunk.emit_u16(Op::ObjectKey, key_idx, span);
                self.emit_set_local(slot, span);
            }
        }

        // `...rest`: bind leftover (unbound) keys into the rest slot. A bare `...`
        // (no name) is a discard.
        if let Some(rest) = pat.children().find(|c| c.kind() == PatRest) {
            if rest.children_with_tokens().any(|el| {
                el.as_token().map(|t| t.kind() == Ident).unwrap_or(false)
            }) {
                let slot = self.pattern_bind_slot(rest, range_span(rest))?;
                let keys = Value::Array(Rc::new(std::cell::RefCell::new(bound_keys)));
                let keys_idx = self.chunk.add_const(keys);
                self.emit_get_local_temp(subj_temp, span);
                self.chunk.emit_u16(Op::ObjectRest, keys_idx, span);
                self.emit_set_local(slot, span);
            }
        }
        Ok(())
    }

    /// Load a SCRATCH temp slot (`alloc_temp`). Temps are never resolver cell slots,
    /// so they are always plain `GET_LOCAL` (never the cell variant).
    fn emit_get_local_temp(&mut self, slot: u16, span: Span) {
        self.chunk.emit_u16(Op::GetLocal, slot, span);
    }

    /// Emit the read for a resolved name USE (an Option-C compare ident): the same
    /// dispatch as `compile_name_ref`'s `uses` arm (local / upvalue / bare builtin
    /// global). A compare ident only ever resolves to one of those (the resolver
    /// only records a `use` here when the name was already in scope).
    fn emit_resolution_read(
        &mut self,
        res: &Resolution,
        span: Span,
    ) -> Result<(), CompileError> {
        match res {
            Resolution::Local(slot) => {
                let slot = u16::try_from(*slot).map_err(|_| {
                    CompileError::new("local slot index exceeds 65535", span)
                })?;
                self.emit_get_local(slot, span);
                Ok(())
            }
            Resolution::Upvalue(idx) => {
                let idx = u16::try_from(*idx)
                    .map_err(|_| CompileError::new("upvalue index exceeds 65535", span))?;
                self.chunk.emit_u16(Op::GetUpvalue, idx, span);
                Ok(())
            }
            Resolution::Global(name)
                if crate::interp::BUILTIN_NAMES.contains(&name.as_str()) =>
            {
                let idx = self.chunk.add_const(Value::Str(Rc::from(name.as_str())));
                self.chunk.emit_u16(Op::GetGlobal, idx, span);
                Ok(())
            }
            Resolution::Global(name) => Err(CompileError::new(
                format!("bare global reference '{name}' not yet supported (V4)"),
                span,
            )),
            Resolution::Unresolved => {
                Err(CompileError::new("undefined name in match pattern", span))
            }
        }
    }

    /// Compile a lexical `Block` `{ ... }`. Blocks do NOT push a runtime scope:
    /// the resolver allocates DISTINCT frame-flat slots for block-scoped bindings
    /// (and for shadowing — an inner `let x` gets a different slot than the outer
    /// one), so a block is just its statements compiled in order. A trailing
    /// expression statement inside a block is NOT a value position here (V2 has no
    /// block-expression value), so it is compiled and `POP`ped like any other
    /// statement, matching the tree-walker, which discards block statement values.
    fn compile_block(&mut self, block: &Block) -> Result<(), CompileError> {
        for s in block.stmts() {
            match &s {
                Stmt::ExprStmt(es) => {
                    let expr = es.expr().ok_or_else(|| {
                        CompileError::new("empty expression statement", node_span(es))
                    })?;
                    self.compile_expr(&expr)?;
                    self.chunk.emit(Op::Pop, node_span(es));
                }
                other => self.compile_stmt(other)?,
            }
        }
        Ok(())
    }

    /// Lower a call `callee(arg0, .., argN)`. The general convention places the
    /// callee value on the stack first, then each argument left-to-right, then
    /// `CALL argc` — the run loop reads `[.., callee, arg0, .., arg{argc-1}]`. A
    /// `Value::Closure` callee enters a new VM frame (the args become its first
    /// local slots); any other callee (builtin, native function, class
    /// constructor, bound method) is dispatched through the shared `call_value`.
    ///
    /// The callee is any compilable expression: a bare builtin name
    /// (`Resolution::Global` → `GET_GLOBAL`, yielding a `Value::Builtin`), a
    /// local/upvalue holding a closure (`GET_LOCAL`/`GET_LOCAL_CELL`/`GET_UPVALUE`),
    /// or a parenthesized arrow. Member calls (`a.m(...)`) and calls whose callee
    /// resolves to a non-builtin bare global are later deferrals (V9).
    fn compile_call(&mut self, call: &CallExpr) -> Result<(), CompileError> {
        // The CALL instruction can PANIC at runtime (arity mismatch, per-param
        // contract violation, non-callable callee, …). The tree-walker anchors
        // those at the Call expression's `expr.span`, which (built from the first
        // real token to the last) carries NO leading trivia. A CST node's raw
        // `text_range()` begins at any leading whitespace/newline, so for a bare
        // `f()` statement the raw span would point one byte early (the preceding
        // newline) — the #132 off-by-one. Use the trivia-trimmed code span so the
        // VM's CALL-site diagnostics are byte-identical to the tree-walker.
        let span = node_code_span(call);
        let callee = call
            .expr()
            .ok_or_else(|| CompileError::new("call expression missing callee", span))?;

        // A plain member call `recv.m(args)` lowers to CALL_METHOD, which mirrors
        // the tree-walker's `eval_chain` Member-callee Call arm (schema fluent hook
        // + `read_member` → `call_value`). This is what makes the generator
        // consumer API (`gen.next(v)` / `gen.close()`) work — and array/string/
        // instance methods generally. An OPTIONAL member call `recv?.m(args)` needs
        // the nil-receiver short-circuit-the-whole-call semantics and is a full-V9
        // deferral; reject it here rather than diverge.
        if let Expr::MemberExpr(m) = &callee {
            return self.compile_method_call(call, m, span);
        }
        if matches!(&callee, Expr::OptMemberExpr(_)) {
            return Err(CompileError::new(
                "optional method calls (a?.m(...)) not yet supported (V9)",
                node_span(&callee),
            ));
        }

        // Compile the callee onto the stack. `compile_expr` routes a `NameRef`
        // through `compile_name_ref`, which already handles a bare-builtin name
        // (GET_GLOBAL) and a local/upvalue (GET_LOCAL[_CELL]/GET_UPVALUE), and
        // defers a non-builtin bare global with its own clear error.
        self.compile_expr(&callee)?;

        // A spread anywhere in the argument list makes the argc dynamic: build the
        // flattened arg array at runtime, then dispatch via `CALL_SPREAD`. Without a
        // spread the static fixed-argc `CALL` path is used (byte-identical output).
        if self.arg_list_has_spread(call) {
            self.compile_spread_args(call, span)?;
            self.chunk.emit(Op::CallSpread, span);
            return Ok(());
        }

        // Compile each argument, left to right.
        let mut argc: u8 = 0;
        if let Some(arg_list) = call.arg_list() {
            for arg in arg_list.exprs() {
                self.compile_expr(&arg)?;
                argc = argc.checked_add(1).ok_or_else(|| {
                    CompileError::new("too many call arguments (max 255)", span)
                })?;
            }
        }

        self.chunk.emit_u8(Op::Call, argc, span);
        Ok(())
    }

    /// Whether a call's argument list contains a `...spread` element (a `SpreadElem`
    /// child of the `ArgList`). Drives the dynamic-arity `CALL_SPREAD` lowering.
    fn arg_list_has_spread(&self, call: &CallExpr) -> bool {
        call.arg_list()
            .map(|al| {
                al.syntax()
                    .children()
                    .any(|c| c.kind() == SyntaxKind::SpreadElem)
            })
            .unwrap_or(false)
    }

    /// Build the flattened call-argument array at runtime onto the stack (leaving a
    /// single `Value::Array`): `NEW_ARRAY 0`, then for each source-order arg either
    /// `<item>; APPEND_ARRAY` (one positional arg) or `<operand>; SPREAD_ARGS`
    /// (flatten the operand array's elements in). `SPREAD_ARGS` mirrors the
    /// tree-walker's `eval_call_args` spread arm — a non-array operand panics with
    /// `can only spread an array as call arguments, got {type}` at the operand's
    /// trivia-trimmed span. The arg ORDER (and thus arity/contract application)
    /// matches the tree-walker exactly.
    fn compile_spread_args(&mut self, call: &CallExpr, span: Span) -> Result<(), CompileError> {
        self.chunk.emit_u16(Op::NewArray, 0, span);
        let Some(arg_list) = call.arg_list() else {
            return Ok(());
        };
        for child in arg_list.syntax().children() {
            if let Some(spread) = SpreadElem::cast(child.clone()) {
                let operand = spread.expr().ok_or_else(|| {
                    CompileError::new("call argument spread (...) missing operand", node_span(&spread))
                })?;
                let op_span = node_code_span(&operand);
                self.compile_expr(&operand)?;
                self.chunk.emit(Op::SpreadArgs, op_span);
            } else if let Some(arg) = Expr::cast(child.clone()) {
                self.compile_expr(&arg)?;
                self.chunk.emit(Op::AppendArray, span);
            }
            // Tokens (`(`, `,`, `)`) and trivia are skipped.
        }
        Ok(())
    }

    /// Lower a plain member call `recv.<name>(args)` to `CALL_METHOD name, argc`.
    /// The receiver is compiled first, then the args left-to-right, then the op
    /// (which pops `argc` args + the receiver and dispatches — schema hook or
    /// `read_member` → `call_value`, mirroring the tree-walker's `eval_chain`).
    ///
    /// Argument handling matches `compile_call`'s (each `arg` is compiled via
    /// `compile_expr`, left to right; the `arg_list().exprs()` iterator is the
    /// same one CALL uses).
    fn compile_method_call(
        &mut self,
        call: &CallExpr,
        m: &MemberExpr,
        span: Span,
    ) -> Result<(), CompileError> {
        let object = m
            .expr()
            .ok_or_else(|| CompileError::new("method call missing receiver", span))?;
        let name = m
            .ident_token()
            .ok_or_else(|| CompileError::new("method call missing method name", span))?
            .text()
            .to_string();

        // `super.<name>(args)` (V9-T2): the receiver is the bare name `super`. This
        // is NOT a value to evaluate — it is the implicit super reference. Emit
        // `GET_SUPER name` (which resolves `name` up from the current method's
        // DEFINING class's superclass, bound to `self` at slot 0, producing a
        // BoundMethod), then the args, then a plain `CALL`. Mirrors the tree-walker:
        // `super` is a `Value::Super` whose `read_member` walks from
        // `defining_class.superclass`, and the resulting BoundMethod runs on `self`.
        if is_super_receiver(&object) {
            let name_idx = self.chunk.add_const(Value::Str(Rc::from(name.as_str())));
            self.chunk.emit_u16(Op::GetSuper, name_idx, span);
            // `GET_SUPER` leaves the BoundMethod callee on the stack, so a spread
            // argument list is the same dynamic-arity build + `CALL_SPREAD` as a
            // plain call (the callee is already in place).
            if self.arg_list_has_spread(call) {
                self.compile_spread_args(call, span)?;
                self.chunk.emit(Op::CallSpread, span);
                return Ok(());
            }
            let mut argc: u8 = 0;
            if let Some(arg_list) = call.arg_list() {
                for arg in arg_list.exprs() {
                    self.compile_expr(&arg)?;
                    argc = argc.checked_add(1).ok_or_else(|| {
                        CompileError::new("too many call arguments (max 255)", span)
                    })?;
                }
            }
            self.chunk.emit_u8(Op::Call, argc, span);
            return Ok(());
        }

        // A spread in a member-method call `recv.m(...args)` would need a
        // CALL_METHOD variant that flattens a runtime-length arg list while keeping
        // the schema-hook / `read_member` receiver dispatch. No example/test in the
        // gated corpus hits this (the spread examples use bare-name calls); rather
        // than diverge, reject it with a clear error. (Bare-name and `super.m(...)`
        // spread calls ARE supported.)
        if self.arg_list_has_spread(call) {
            return Err(CompileError::new(
                "spread in a member-method call (recv.m(...args)) not yet supported",
                span,
            ));
        }

        // Receiver, then args (left to right).
        self.compile_expr(&object)?;
        let mut argc: u8 = 0;
        if let Some(arg_list) = call.arg_list() {
            for arg in arg_list.exprs() {
                self.compile_expr(&arg)?;
                argc = argc.checked_add(1).ok_or_else(|| {
                    CompileError::new("too many call arguments (max 255)", span)
                })?;
            }
        }
        let name_idx = self.chunk.add_const(Value::Str(Rc::from(name.as_str())));
        self.chunk.emit_u16_u8(Op::CallMethod, name_idx, argc, span);
        Ok(())
    }

    fn compile_literal(&mut self, lit: &Literal) -> Result<(), CompileError> {
        let span = node_span(lit);
        let value = literal_const_value(lit)?;
        let idx = self.chunk.add_const(value);
        self.chunk.emit_u16(Op::Const, idx, span);
        Ok(())
    }

    fn compile_binary(&mut self, bin: &BinaryExpr) -> Result<(), CompileError> {
        // An arithmetic/comparison op (Add/Sub/.../Lt/.../Range) can PANIC with a
        // Tier-2 type error. The tree-walker anchors these at the BinaryExpr's
        // `expr.span` (`apply_binop(.., expr.span)`), which carries no leading
        // trivia. Use the trivia-trimmed code span so a bare `a + 1` statement's
        // type panic matches the tree-walker byte-for-byte (#132). The
        // short-circuit jumps reuse this span too, but they never panic, so the
        // trimmed start is harmless there.
        let span = node_code_span(bin);
        let lhs = bin
            .lhs()
            .ok_or_else(|| CompileError::new("binary expression missing left operand", span))?;
        let rhs = bin
            .rhs()
            .ok_or_else(|| CompileError::new("binary expression missing right operand", span))?;
        let op = bin
            .op()
            .ok_or_else(|| CompileError::new("binary expression missing operator", span))?;

        // `&&`/`||`/`??` short-circuit: the right operand must NOT be evaluated
        // when the left already decides the result, and the result is the actual
        // OPERAND value (JS-like), not a coerced bool — exactly the tree-walker's
        // `BinOp::And`/`Or`/`Coalesce` arms. Lower each as a `Dup` + conditional
        // jump that POPS the tested copy, leaving precisely one value on the stack:
        //
        //   a && b: a; DUP; jf=JF; POP; b; patch(jf)
        //     - a falsy:  JF pops the dup, jumps to end leaving the falsy `a`.
        //     - a truthy: JF pops the dup, falls through; POP discards `a`; eval b.
        //   a || b: a; DUP; jt=JT; POP; b; patch(jt)
        //     - a truthy: JT pops the dup, jumps to end leaving `a`.
        //     - a falsy:  JT pops the dup, falls through; POP discards `a`; eval b.
        //   a ?? b: a; DUP; jnn=JNN; POP; b; patch(jnn)
        //     - a non-nil: JNN pops the dup, jumps to end leaving `a`.
        //     - a nil:     JNN pops the dup, falls through; POP discards `a`; eval b.
        if let Some(jop) = short_circuit_op(op) {
            self.compile_expr(&lhs)?;
            self.chunk.emit(Op::Dup, span);
            let skip = self.chunk.emit_jump(jop, span);
            self.chunk.emit(Op::Pop, span);
            self.compile_expr(&rhs)?;
            self.chunk.patch_jump(skip);
            return Ok(());
        }

        self.compile_expr(&lhs)?;
        self.compile_expr(&rhs)?;
        let bytecode = match op {
            SyntaxKind::Plus => Op::Add,
            SyntaxKind::Minus => Op::Sub,
            SyntaxKind::Star => Op::Mul,
            SyntaxKind::Slash => Op::Div,
            SyntaxKind::Percent => Op::Mod,
            SyntaxKind::StarStar => Op::Pow,
            SyntaxKind::Lt => Op::Lt,
            SyntaxKind::Le => Op::Le,
            SyntaxKind::Gt => Op::Gt,
            SyntaxKind::Ge => Op::Ge,
            SyntaxKind::EqEq => Op::Eq,
            SyntaxKind::BangEq => Op::Ne,
            // `&&`/`||`/`??` are handled by the short-circuit path above (they
            // never reach this non-short-circuit dispatch).
            other => {
                return Err(CompileError::new(
                    format!("binary operator {other:?} not yet supported in V2"),
                    span,
                ))
            }
        };
        self.chunk.emit(bytecode, span);
        Ok(())
    }

    /// Lower a `RangeExpr` (`a..b`). Mirrors the tree-walker's `BinOp::Range`:
    /// pushes both bounds and emits `RANGE`, which builds the eager half-open
    /// `array<number>`. Only the exclusive `..` form has a tree-walker equivalent
    /// in value position (the old parser produces `..=` only inside match
    /// patterns), so an inclusive `..=` range as a value is a documented deferral.
    fn compile_range(&mut self, range: &RangeExpr) -> Result<(), CompileError> {
        // The RANGE op can PANIC (a non-number bound is a Tier-2 type error in
        // `apply_binop`'s `BinOp::Range` arm). The tree-walker anchors it at the
        // whole range expression's `expr.span`, which carries no leading trivia;
        // use the trivia-trimmed code span for byte-identical diagnostics (#132).
        let span = node_code_span(range);
        let start = range
            .start()
            .ok_or_else(|| CompileError::new("range expression missing start bound", span))?;
        let end = range
            .end()
            .ok_or_else(|| CompileError::new("range expression missing end bound", span))?;
        match range.op() {
            Some(SyntaxKind::DotDot) => {}
            Some(SyntaxKind::DotDotEq) => {
                return Err(CompileError::new(
                    "inclusive range (..=) as a value is not yet supported in V2",
                    span,
                ))
            }
            other => {
                return Err(CompileError::new(
                    format!("range expression has unexpected operator {other:?}"),
                    span,
                ))
            }
        }
        self.compile_expr(&start)?;
        self.compile_expr(&end)?;
        self.chunk.emit(Op::Range, span);
        Ok(())
    }

    /// Lower an array literal `[a, b, c]`: compile each element in source order,
    /// then `NEW_ARRAY n` (which pops `n` values, preserving source order, into a
    /// fresh `Value::Array`). Matches the tree-walker's `ExprKind::Array`.
    ///
    /// A spread element `[...a, x, ...b]` (the CST records each as a `SpreadElem`
    /// child interleaved with the plain `Expr` children) switches the lowering to
    /// the INCREMENTAL builder: `NEW_ARRAY 0` (an empty array), then for each
    /// source-order element either `<item>; APPEND_ARRAY` (push one) or
    /// `<operand>; SPREAD` (flatten the operand array's elements in). This mirrors
    /// the tree-walker's `ExprKind::Array` `Vec` build order exactly, and `SPREAD`
    /// carries the operand's trivia-trimmed span so a non-array spread panics
    /// byte-identically.
    fn compile_array(&mut self, arr: &ArrayExpr) -> Result<(), CompileError> {
        let span = node_span(arr);
        let has_spread = arr
            .syntax()
            .children()
            .any(|c| c.kind() == SyntaxKind::SpreadElem);

        if !has_spread {
            // Fast path (no spread): byte-identical to the prior lowering — push all
            // elements, then `NEW_ARRAY n`.
            let mut n: u16 = 0;
            for elem in arr.exprs() {
                self.compile_expr(&elem)?;
                n = n
                    .checked_add(1)
                    .ok_or_else(|| CompileError::new("array literal has too many elements", span))?;
            }
            self.chunk.emit_u16(Op::NewArray, n, span);
            return Ok(());
        }

        // Incremental builder. Start with an empty array on the stack.
        self.chunk.emit_u16(Op::NewArray, 0, span);
        for child in arr.syntax().children() {
            if let Some(spread) = SpreadElem::cast(child.clone()) {
                let operand = spread.expr().ok_or_else(|| {
                    CompileError::new("array spread (...) missing operand", node_span(&spread))
                })?;
                // The tree-walker anchors the non-array panic at the spread
                // operand's `x.span` (no leading trivia) → the trimmed code span.
                let op_span = node_code_span(&operand);
                self.compile_expr(&operand)?;
                self.chunk.emit(Op::Spread, op_span);
            } else if let Some(elem) = Expr::cast(child.clone()) {
                self.compile_expr(&elem)?;
                self.chunk.emit(Op::AppendArray, span);
            }
            // Tokens (`[`, `,`, `]`) and trivia are skipped.
        }
        Ok(())
    }

    /// Lower an object literal `{a: 1, "k": v}`: for each field, push the KEY (a
    /// `Value::Str` const) then the VALUE expression, all in source order; then
    /// `NEW_OBJECT n` (which pops `n` key/value pairs into an insertion-ordered
    /// `IndexMap`). Matches the tree-walker's `ExprKind::Object` — a later
    /// duplicate key overwrites the value but keeps the first-seen position
    /// (IndexMap semantics).
    ///
    /// Keys mirror the tree-walker's `ObjEntry::KV` key text exactly: an `Ident`
    /// key (`a:`) uses the identifier's raw text; a `Str` key (`"k":`) uses the
    /// UNESCAPED string contents (the legacy lexer pre-decodes `Tok::Str`, so the
    /// CST's raw quoted token must be unescaped here to agree). AScript object
    /// literals have NO shorthand (`{x}`) — both parsers require `key: value` — so
    /// there is no shorthand case to handle.
    ///
    /// Object-spread `{...o, k: v}` (the CST records each spread as a `SpreadElem`
    /// child interleaved with the `ObjectField` children) switches the lowering to
    /// the INCREMENTAL builder: `NEW_OBJECT 0` (an empty object), then for each
    /// source-order entry either `<key>; <value>; APPEND_OBJECT` (insert one pair)
    /// or `<operand>; SPREAD_OBJECT` (merge the operand object's entries in).
    /// `APPEND_OBJECT`/`SPREAD_OBJECT` both `IndexMap::insert`, so later-wins +
    /// first-position is byte-identical to the tree-walker; `SPREAD_OBJECT` carries
    /// the operand's trivia-trimmed span for the identical non-object panic.
    fn compile_object(&mut self, obj: &ObjectExpr) -> Result<(), CompileError> {
        let span = node_span(obj);
        let has_spread = obj
            .syntax()
            .children()
            .any(|c| c.kind() == SyntaxKind::SpreadElem);

        if !has_spread {
            // Fast path (no spread): byte-identical to the prior lowering.
            let mut n: u16 = 0;
            for field in obj.object_fields() {
                let fspan = node_span(&field);
                let key = object_field_key(&field)
                    .ok_or_else(|| CompileError::new("object field has no key", fspan))?;
                let value = field
                    .value()
                    .ok_or_else(|| CompileError::new("object field has no value", fspan))?;
                let key_idx = self.chunk.add_const(Value::Str(Rc::from(key.as_str())));
                self.chunk.emit_u16(Op::Const, key_idx, fspan);
                self.compile_expr(&value)?;
                n = n.checked_add(1).ok_or_else(|| {
                    CompileError::new("object literal has too many fields", span)
                })?;
            }
            self.chunk.emit_u16(Op::NewObject, n, span);
            return Ok(());
        }

        // Incremental builder. Start with an empty object on the stack.
        self.chunk.emit_u16(Op::NewObject, 0, span);
        for child in obj.syntax().children() {
            if let Some(spread) = SpreadElem::cast(child.clone()) {
                let operand = spread.expr().ok_or_else(|| {
                    CompileError::new("object spread (...) missing operand", node_span(&spread))
                })?;
                let op_span = node_code_span(&operand);
                self.compile_expr(&operand)?;
                self.chunk.emit(Op::SpreadObject, op_span);
            } else if let Some(field) = ObjectField::cast(child.clone()) {
                let fspan = node_span(&field);
                let key = object_field_key(&field)
                    .ok_or_else(|| CompileError::new("object field has no key", fspan))?;
                let value = field
                    .value()
                    .ok_or_else(|| CompileError::new("object field has no value", fspan))?;
                let key_idx = self.chunk.add_const(Value::Str(Rc::from(key.as_str())));
                self.chunk.emit_u16(Op::Const, key_idx, fspan);
                self.compile_expr(&value)?;
                self.chunk.emit(Op::AppendObject, fspan);
            }
            // Tokens (`{`, `,`, `}`) and trivia are skipped.
        }
        Ok(())
    }

    /// Lower an index read `a[i]`: compile the receiver, compile the index, then
    /// `GET_INDEX`. The op carries the whole `IndexExpr`'s span (the tree-walker's
    /// `expr.span`, used for the array-index / out-of-bounds / non-string-key
    /// panics). Index ASSIGNMENT (`a[i] = x`) is a V9 deferral handled in
    /// `compile_assign` (its target is not a `NameRef`).
    fn compile_index(&mut self, ix: &IndexExpr) -> Result<(), CompileError> {
        // GET_INDEX can PANIC (out-of-bounds, non-string object key, …). The
        // tree-walker anchors these at the IndexExpr's `expr.span` (see
        // `index_get(.., object.span, expr.span)`), which carries no leading
        // trivia. Use the trivia-trimmed code span so a bare `a[9]` statement's
        // OOB panic matches the tree-walker byte-for-byte (#132).
        let span = node_code_span(ix);
        let base = ix
            .base()
            .ok_or_else(|| CompileError::new("index expression missing receiver", span))?;
        let index = ix
            .index()
            .ok_or_else(|| CompileError::new("index expression missing index", span))?;
        self.compile_expr(&base)?;
        self.compile_expr(&index)?;
        self.chunk.emit(Op::GetIndex, span);
        Ok(())
    }

    /// Lower a member read `a.k`: compile the receiver, then `GET_PROP <name>`.
    /// The op carries the RECEIVER's span (the tree-walker anchors `read_member`
    /// panics — e.g. `cannot read property '<k>' of nil` — at `object.span`).
    /// Member ASSIGNMENT (`a.k = x`) is a V9 deferral.
    fn compile_member(&mut self, m: &MemberExpr) -> Result<(), CompileError> {
        let span = node_span(m);
        let object = m
            .expr()
            .ok_or_else(|| CompileError::new("member expression missing receiver", span))?;
        let name = m
            .ident_token()
            .ok_or_else(|| CompileError::new("member expression missing property name", span))?
            .text()
            .to_string();
        // GET_PROP panics ("cannot read property '<k>' of nil", …) are anchored by
        // the tree-walker at the RECEIVER's `object.span` (`read_member(.., object.span)`),
        // which carries no leading trivia. Use the trivia-trimmed code span so a
        // bare `a.foo` statement's nil-receiver panic matches byte-for-byte (#132).
        let obj_span = node_code_span(&object);
        self.compile_expr(&object)?;
        let name_idx = self.chunk.add_const(Value::Str(Rc::from(name.as_str())));
        self.chunk.emit_u16(Op::GetProp, name_idx, obj_span);
        Ok(())
    }

    /// Lower an optional member read `a?.k`: compile the receiver, then
    /// `GET_PROP_OPT <name>` (a `nil` receiver short-circuits to `nil`; otherwise
    /// it behaves exactly like `GET_PROP`). Mirrors the tree-walker's
    /// `ExprKind::OptMember` (nil receiver → nil, else `read_member`). The op
    /// carries the receiver's span, like `GET_PROP`.
    fn compile_opt_member(&mut self, m: &OptMemberExpr) -> Result<(), CompileError> {
        let span = node_span(m);
        let object = m
            .expr()
            .ok_or_else(|| CompileError::new("optional-member expression missing receiver", span))?;
        let name = m
            .ident_token()
            .ok_or_else(|| {
                CompileError::new("optional-member expression missing property name", span)
            })?
            .text()
            .to_string();
        // Same trivia-trimmed receiver span as GET_PROP — GET_PROP_OPT behaves
        // identically on a non-nil receiver and anchors its panic at the receiver.
        let obj_span = node_code_span(&object);
        self.compile_expr(&object)?;
        let name_idx = self.chunk.add_const(Value::Str(Rc::from(name.as_str())));
        self.chunk.emit_u16(Op::GetPropOpt, name_idx, obj_span);
        Ok(())
    }

    fn compile_unary(&mut self, un: &UnaryExpr) -> Result<(), CompileError> {
        let span = node_span(un);
        let operand = un
            .expr()
            .ok_or_else(|| CompileError::new("unary expression missing operand", span))?;
        let op = un
            .op()
            .ok_or_else(|| CompileError::new("unary expression missing operator", span))?;
        // The tree-walker anchors a unary panic at the OPERAND's span
        // (`apply_unop(op, v, operand.span)` in `eval_expr`), e.g. `cannot negate a
        // non-number` points at the operand, not the `-`. Emit the op with the
        // operand span so the VM's diagnostics are byte-identical. The legacy
        // `operand.span` carries no leading trivia, so use the trivia-trimmed code
        // span (matters when the operand itself begins a bare statement, #132).
        let operand_span = node_code_span(&operand);
        self.compile_expr(&operand)?;
        let bytecode = match op {
            SyntaxKind::Minus => Op::Neg,
            SyntaxKind::Bang => Op::Not,
            other => {
                return Err(CompileError::new(
                    format!("unary operator {other:?} not yet supported in V1"),
                    span,
                ))
            }
        };
        self.chunk.emit(bytecode, operand_span);
        Ok(())
    }

    fn compile_paren(&mut self, paren: &ParenExpr) -> Result<(), CompileError> {
        let inner = paren.expr().ok_or_else(|| {
            CompileError::new("empty parenthesized expression", node_span(paren))
        })?;
        // Parens affect only grouping; no opcode is emitted.
        self.compile_expr(&inner)
    }

    /// Lower the postfix `?` propagate operator (`expr?`). The inner expression
    /// is compiled, then `PROPAGATE` is emitted. The op's span is the `TryExpr`'s
    /// trivia-trimmed code span, matching the tree-walker's `ExprKind::Try` panic
    /// anchor (`expr.span` = the whole Try expression's span), so a non-pair
    /// Tier-2 panic ("the ? operator requires a Result pair [value, err]") points
    /// at the same source byte-for-byte. At runtime `PROPAGATE` checks the value
    /// is a 2-element `[value, err]` Result pair: if `err == nil` the `value`
    /// stays on the stack; otherwise it early-returns `[nil, err]` from the
    /// enclosing function (function-level early return, exactly like the
    /// tree-walker's `Control::Propagate`).
    fn compile_try(&mut self, t: &TryExpr) -> Result<(), CompileError> {
        let span = node_code_span(t);
        let inner = t
            .expr()
            .ok_or_else(|| CompileError::new("? operator missing operand", span))?;
        self.compile_expr(&inner)?;
        self.chunk.emit(Op::Propagate, span);
        Ok(())
    }

    /// Lower `expr!` (force-unwrap) into the inner expression followed by
    /// `UNWRAP`. The op's span is the `UnwrapExpr`'s trivia-trimmed code span,
    /// byte-identical to the tree-walker's `expr.span` for `ExprKind::Unwrap`, so
    /// both the non-pair Tier-2 panic ("the ! operator requires a Result pair
    /// [value, err]") and the recoverable error-promotion panic point at the same
    /// source. At runtime `UNWRAP` checks the value is a 2-element `[value, err]`
    /// Result pair: if `err == nil` the `value` stays on the stack (the `!`
    /// expression's result); otherwise it raises a RECOVERABLE `Control::Panic`
    /// carrying the original error's message (via `error_message`), exactly like
    /// the tree-walker.
    fn compile_unwrap(&mut self, u: &UnwrapExpr) -> Result<(), CompileError> {
        let span = node_code_span(u);
        let inner = u
            .expr()
            .ok_or_else(|| CompileError::new("! operator missing operand", span))?;
        self.compile_expr(&inner)?;
        self.chunk.emit(Op::Unwrap, span);
        Ok(())
    }

    /// Lower `await expr`: compile the inner expression, then emit `AWAIT`. The op
    /// drives a `Value::Future` to completion (re-surfacing any panic/propagation
    /// raised in the spawned task at THIS site) and is identity on a non-future —
    /// byte-identical to the tree-walker's `ExprKind::Await`. The op's span is the
    /// `AwaitExpr`'s trivia-trimmed code span.
    fn compile_await(&mut self, a: &AwaitExpr) -> Result<(), CompileError> {
        let span = node_code_span(a);
        let inner = a
            .expr()
            .ok_or_else(|| CompileError::new("await missing operand", span))?;
        self.compile_expr(&inner)?;
        self.chunk.emit(Op::Await, span);
        Ok(())
    }

    /// Lower a `yield expr` (or bare `yield`) into the operand push followed by
    /// `Op::Yield`. The operand is compiled onto the stack (a bare `yield` pushes
    /// `NIL`); `Op::Yield` pops it as the yielded value (suspending the Fiber and
    /// surfacing it to the consumer's `next()`), and the value the consumer's
    /// `next(v)` injects becomes this `yield` expression's result (pushed back by
    /// `GeneratorHandle::resume_vm`). The span is the `YieldExpr`'s trivia-trimmed
    /// code span. `yield` is only valid inside a generator body; the resolver/
    /// front-end reject a top-level `yield`, so no extra guard is needed here.
    fn compile_yield(&mut self, y: &YieldExpr) -> Result<(), CompileError> {
        let span = node_code_span(y);
        match y.expr() {
            Some(inner) => self.compile_expr(&inner)?,
            None => self.chunk.emit(Op::Nil, span),
        }
        self.chunk.emit(Op::Yield, span);
        Ok(())
    }

    /// Lower a template literal `` `a${e}b` `` into `n` part-pushes followed by
    /// `TEMPLATE n`, where `n` is the total number of parts (literal chunks +
    /// interpolated expressions). The CST `TemplateExpr` node interleaves
    /// template *tokens* (`TemplateStr`/`TemplateStart`/`TemplateMiddle`/
    /// `TemplateEnd`, each carrying its raw delimited source text) with the
    /// interpolated expression *nodes*. We walk `children_with_tokens()` in
    /// source order: every template token contributes a literal string chunk
    /// (delimiters stripped + unescaped, exactly mirroring the tree-walker's
    /// `lex_template_chunk`), and every expression node is compiled in place.
    ///
    /// The tree-walker's `ExprKind::Template` concatenates each chunk and each
    /// interpolated value (coerced via `Value::to_string()`); the VM's
    /// `TEMPLATE n` op performs the identical concatenation/coercion.
    fn compile_template(&mut self, t: &TemplateExpr) -> Result<(), CompileError> {
        let span = node_span(t);
        let mut parts: u16 = 0;
        for child in t.syntax().children_with_tokens() {
            if let Some(tok) = child.as_token() {
                // A template *token* carries a raw, delimited literal chunk
                // (`` `...${ ``, `}...${`, `` }...` ``, or full `` `...` ``).
                match tok.kind() {
                    SyntaxKind::TemplateStr
                    | SyntaxKind::TemplateStart
                    | SyntaxKind::TemplateMiddle
                    | SyntaxKind::TemplateEnd => {
                        let chunk = unescape_template_body(strip_template_delims(tok.text()));
                        let idx = self.chunk.add_const(Value::Str(Rc::from(chunk.as_str())));
                        self.chunk.emit_u16(Op::Const, idx, span);
                    }
                    // Trivia (whitespace/comments) never appears between template
                    // delimiters, but skip it defensively (no part emitted).
                    _ => continue,
                }
            } else if let Some(node) = child.as_node() {
                let expr = Expr::cast((*node).clone()).ok_or_else(|| {
                    CompileError::new("template interpolation is not an expression", span)
                })?;
                self.compile_expr(&expr)?;
            } else {
                continue;
            }
            parts = parts
                .checked_add(1)
                .ok_or_else(|| CompileError::new("template has too many parts", span))?;
        }
        self.chunk.emit_u16(Op::Template, parts, span);
        Ok(())
    }
}

/// The declared name of a function node, for the disassembler / traces. A
/// `FnDecl` has its name as a direct `Ident` token child (after `fn`); an
/// `ArrowExpr` is anonymous (no `Ident` child, since its params live in a nested
/// `ParamList`), so this returns `None` there.
fn fn_name_token_text(fn_node: &ResolvedNode) -> Option<String> {
    fn_node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// Decode an object-literal field's key into the exact string the tree-walker
/// keys by (`ObjEntry::KV` key). The key token is either an `Ident` (raw
/// identifier text, used verbatim) or a `Str` (a quoted literal whose contents
/// must be unescaped — the legacy lexer pre-decodes `Tok::Str`, so the CST's raw
/// quoted token is unescaped here via the shared [`unescape_str_body`] to agree).
/// Returns
/// `None` only on a malformed field with no key token (a parser/compiler bug).
fn object_field_key(field: &crate::syntax::ast::ObjectField) -> Option<String> {
    let tok = field
        .syntax()
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::Str))?;
    match tok.kind() {
        SyntaxKind::Str => Some(unescape_str_body(strip_quotes(tok.text()))),
        // Ident key: raw identifier text, used verbatim.
        _ => Some(tok.text().to_string()),
    }
}

/// Whether a `SyntaxKind` is a match-pattern node kind. Mirrors the resolver's
/// `is_pattern` (minus `IdentPat`, which the parser never produces — bare idents
/// are `LiteralPat`); `OrPat` is handled by the caller, not as a leaf pattern.
fn is_pattern_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(kind, WildcardPat | LiteralPat | RangePat | ArrayPat | ObjectPat)
}

/// Whether a `SyntaxKind` is an expression node kind. Mirrors the resolver's
/// `is_expr` so the compiler picks the same Expr children inside patterns.
fn is_expr_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        Literal
            | NameRef
            | UnaryExpr
            | BinaryExpr
            | ParenExpr
            | CallExpr
            | ArgList
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

/// If a `LiteralPat` is EXACTLY a single bare `NameRef`, return that NameRef node
/// (so the caller can look up the resolver's Option-C classification by its
/// `text_range`). Mirrors the resolver's `bare_ident_pattern` shape check.
fn bare_ident_name_ref(pat: &ResolvedNode) -> Option<ResolvedNode> {
    let mut exprs = pat.children().filter(|c| is_expr_kind(c.kind()));
    let first = exprs.next()?;
    if exprs.next().is_some() {
        return None;
    }
    if first.kind() == SyntaxKind::NameRef {
        Some(first.clone())
    } else {
        None
    }
}

/// Decode an object-destructuring `BindEntry`'s KEY into the exact string the
/// tree-walker keys by (`ObjBinding::key`). The key is the entry's FIRST
/// significant token: an `Ident` (`a` / `b` in `b as local`) used verbatim, or a
/// `Str` (`"k"` in `"k" as v`) whose contents are unescaped (the legacy lexer
/// pre-decodes `Tok::Str`). Returns `None` only on a malformed entry with no key
/// token (a parser/compiler bug).
fn bind_entry_key(entry: &ResolvedNode) -> Option<String> {
    let tok = entry
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::Str))?;
    match tok.kind() {
        SyntaxKind::Str => Some(unescape_str_body(strip_quotes(tok.text()))),
        _ => Some(tok.text().to_string()),
    }
}

/// Build the runtime [`Value`] for a `Literal` node (Number/Str/`true`/`false`/
/// `nil`), reading the value TOKEN's text directly (trivia-free). Shared by the
/// literal compiler (`compile_literal`) and the enum-backing const-evaluator
/// (`const_eval_enum_backing`).
fn literal_const_value(lit: &Literal) -> Result<Value, CompileError> {
    let span = node_span(lit);
    let kind = lit
        .op()
        .ok_or_else(|| CompileError::new("malformed literal (no token)", span))?;
    // Read the literal *token*'s text directly — `node.text()` would include
    // leading/trailing trivia (whitespace/comments) attached to the node.
    let text = literal_token_text(lit)
        .ok_or_else(|| CompileError::new("malformed literal (no token text)", span))?;
    let value = match kind {
        SyntaxKind::Number => {
            let n = parse_number_text(&text).ok_or_else(|| {
                // The lexer already validated the token, so this is a
                // compiler bug rather than a user error if it ever fires.
                CompileError::new(format!("malformed number literal {text:?}"), span)
            })?;
            Value::Number(n)
        }
        SyntaxKind::Str => Value::Str(Rc::from(unescape_str_body(strip_quotes(&text)).as_str())),
        SyntaxKind::TrueKw => Value::Bool(true),
        SyntaxKind::FalseKw => Value::Bool(false),
        SyntaxKind::NilKw => Value::Nil,
        other => {
            return Err(CompileError::new(
                format!("literal token {other:?} not yet supported in V1"),
                span,
            ))
        }
    };
    Ok(value)
}

/// The text of a `Literal` node's value token (Number/Str/keyword), excluding
/// any trivia. Mirrors the kind set in the generated `Literal::op()`.
fn literal_token_text(lit: &Literal) -> Option<String> {
    lit.syntax()
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| {
            matches!(
                t.kind(),
                SyntaxKind::Number
                    | SyntaxKind::Str
                    | SyntaxKind::TrueKw
                    | SyntaxKind::FalseKw
                    | SyntaxKind::NilKw
            )
        })
        .map(|t| t.text().to_string())
}

/// Strip the leading delimiter (`` ` `` or `}`) and trailing delimiter
/// (`` ` `` or `${`) from a raw template-chunk token, yielding the inner text.
/// Mirrors the lossless slicing the CST lexer's `scan_template_chunk` produces.
fn strip_template_delims(s: &str) -> &str {
    // Leading delimiter is a single byte: `` ` `` or `}`.
    let after_open = s.strip_prefix('`').or_else(|| s.strip_prefix('}')).unwrap_or(s);
    // Trailing delimiter is either `${` (interpolation continues) or `` ` ``.
    if let Some(inner) = after_open.strip_suffix("${") {
        inner
    } else if let Some(inner) = after_open.strip_suffix('`') {
        inner
    } else {
        // Unterminated tail (lexer would have flagged it); use as-is.
        after_open
    }
}

/// Strip one leading and one trailing matching quote (`"` or `'`) if present.
fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::disasm::disasm;

    #[test]
    fn compile_one_plus_two_emits_const_const_add_return() {
        let chunk = compile_source("1 + 2").expect("compiles");
        let text = disasm(&chunk);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.iter().any(|l| l.contains("CONST") && l.ends_with("; 1")), "missing CONST 1 in:\n{text}");
        assert!(lines.iter().any(|l| l.contains("CONST") && l.ends_with("; 2")), "missing CONST 2 in:\n{text}");
        assert!(lines.iter().any(|l| l.contains("ADD")), "missing ADD in:\n{text}");
        assert!(lines.iter().any(|l| l.contains("RETURN")), "missing RETURN in:\n{text}");
    }

    #[test]
    fn short_circuit_operators_lower_to_jumps() {
        // V2-T6: `&&`/`||`/`??` short-circuit via a `DUP` + conditional jump +
        // `POP`, NOT a single binary opcode. Assert the expected jump mnemonic
        // appears (and that there is exactly one DUP + one POP per operator).
        for (src, jump_mnemonic) in [
            ("1 && 2", "JUMP_IF_FALSE"),
            ("1 || 2", "JUMP_IF_TRUE"),
            ("1 ?? 2", "JUMP_IF_NOT_NIL"),
        ] {
            let chunk = compile_source(src).expect("compiles");
            let text = disasm(&chunk);
            assert!(
                text.contains(jump_mnemonic),
                "missing {jump_mnemonic} for `{src}` in:\n{text}"
            );
            let dups = text.lines().filter(|l| l.contains("DUP")).count();
            let pops = text.lines().filter(|l| l.contains(" POP")).count();
            assert_eq!(dups, 1, "expected exactly one DUP for `{src}` in:\n{text}");
            assert_eq!(pops, 1, "expected exactly one POP for `{src}` in:\n{text}");
        }
    }

    #[test]
    fn comparison_and_equality_operators_compile() {
        // V2-T5 added the comparison/equality binary opcodes to the compiler.
        for (src, mnemonic) in [
            ("1 < 2", "LT"),
            ("1 <= 2", "LE"),
            ("1 > 2", "GT"),
            ("1 >= 2", "GE"),
            ("1 == 2", "EQ"),
            ("1 != 2", "NE"),
        ] {
            let chunk = compile_source(src).expect("compiles");
            let text = disasm(&chunk);
            assert!(text.contains(mnemonic), "missing {mnemonic} for `{src}` in:\n{text}");
        }
    }

    #[test]
    fn range_expression_compiles_to_range_op() {
        let chunk = compile_source("0..5").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("RANGE"), "missing RANGE in:\n{text}");
    }

    #[test]
    fn inclusive_range_value_is_deferred() {
        let err = compile_source("0..=5").unwrap_err();
        assert!(
            err.message.contains("inclusive range"),
            "expected inclusive-range deferral, got {err:?}"
        );
    }

    #[test]
    fn compile_print_emits_get_global_call() {
        let chunk = compile_source("print(1 + 2)").expect("compiles");
        let text = disasm(&chunk);
        assert!(
            text.contains("GET_GLOBAL") && text.contains("print"),
            "missing GET_GLOBAL print in:\n{text}"
        );
        assert!(text.contains("ADD"), "missing ADD in:\n{text}");
        assert!(text.contains("CALL"), "missing CALL in:\n{text}");
        assert!(text.contains("RETURN"), "missing RETURN in:\n{text}");
    }

    #[test]
    fn leading_print_statement_is_popped() {
        // A non-trailing print(...) compiles a CALL followed by POP; the trailing
        // expression is RETURNed.
        let chunk = compile_source("print(1)\n2").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("CALL"), "missing CALL in:\n{text}");
        assert!(text.contains("POP"), "missing POP in:\n{text}");
        assert!(text.contains("RETURN"), "missing RETURN in:\n{text}");
    }

    #[test]
    fn rejects_call_to_non_builtin_global() {
        // `foo` is a free name; the resolver classifies it Global("foo"), which is
        // not in BUILTIN_NAMES and has no user-global runtime binding (top-level
        // lets are frame-locals) — so compiling the callee defers it (V4). A call
        // to a closure/local/upvalue callee, by contrast, now compiles (V4-T3).
        let err = compile_source("foo(1)").unwrap_err();
        assert!(
            err.message.contains("bare global reference 'foo' not yet supported (V4)"),
            "got {err:?}"
        );
    }

    #[test]
    fn bare_builtin_reference_emits_get_global() {
        // A bare builtin name used as a value (not a call) is a first-class
        // builtin reference: `let p = print` stores the `Value::Builtin`. The
        // initializer compiles to `GET_GLOBAL print`.
        let chunk = compile_source("let p = print\np").expect("compiles");
        let text = disasm(&chunk);
        assert!(
            text.contains("GET_GLOBAL") && text.contains("print"),
            "missing GET_GLOBAL print in:\n{text}"
        );
    }

    #[test]
    fn rejects_bare_non_builtin_global_reference() {
        // `foo` is a free name → resolver classifies it Global("foo"); not a
        // builtin, and there are no user globals (top-level lets are locals), so
        // this is a documented V4 deferral rather than a runtime undefined.
        let err = compile_source("foo").unwrap_err();
        assert!(
            err.message.contains("bare global reference 'foo' not yet supported (V4)"),
            "got {err:?}"
        );
    }

    #[test]
    fn compiles_string_literal() {
        let chunk = compile_source("\"hi\"").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("; \"hi\""), "missing string const in:\n{text}");
    }

    async fn eval_number(src: &str) -> f64 {
        match crate::vm_eval_source(src).await.expect("evaluates") {
            Value::Number(n) => n,
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn precedence_from_cst() {
        assert_eq!(eval_number("1 + 2 * 3").await, 7.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unary_negate() {
        assert_eq!(eval_number("-(4)").await, -4.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parens_group() {
        assert_eq!(eval_number("(1 + 2) * 4").await, 12.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn division_is_float() {
        assert_eq!(eval_number("10 / 4").await, 2.5);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn modulo() {
        assert_eq!(eval_number("7 % 3").await, 1.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn power() {
        assert_eq!(eval_number("2 ** 10").await, 1024.0);
    }

    // Number/escape text→value parsing is exercised exhaustively in
    // `crate::lex_literals` (the single shared source of truth). Here we only
    // cover the compiler's CST delimiter-stripping path feeding those shared
    // routines: quotes/backticks/`${`/`}` are stripped, then the shared
    // unescape/parse runs.
    #[test]
    fn compiler_strips_quotes_then_unescapes() {
        assert_eq!(unescape_str_body(strip_quotes(r#""a\nb""#)), "a\nb");
        assert_eq!(unescape_str_body(strip_quotes(r#"'single'"#)), "single");
        assert_eq!(unescape_str_body(strip_quotes(r#"'\'q\''"#)), "'q'");
        assert_eq!(unescape_str_body(strip_quotes(r#""""#)), "");
    }

    #[test]
    fn compiler_strips_template_delims_then_unescapes() {
        // Full template `` `...` ``.
        assert_eq!(unescape_template_body(strip_template_delims("`plain`")), "plain");
        // Start chunk `` `a${ ``.
        assert_eq!(unescape_template_body(strip_template_delims("`a${")), "a");
        // Middle chunk `}b${`.
        assert_eq!(unescape_template_body(strip_template_delims("}b${")), "b");
        // End chunk `` }c` ``.
        assert_eq!(unescape_template_body(strip_template_delims("}c`")), "c");
        // Empty leading/middle chunks.
        assert_eq!(unescape_template_body(strip_template_delims("`${")), "");
        assert_eq!(unescape_template_body(strip_template_delims("}${")), "");
        // Template escapes survive the strip+unescape: \` -> ` and \$ -> $.
        assert_eq!(unescape_template_body(strip_template_delims("`a\\`b`")), "a`b");
        assert_eq!(unescape_template_body(strip_template_delims("`a\\$b`")), "a$b");
    }

    #[test]
    fn compiles_template_emits_template_op() {
        let chunk = compile_source("`hi ${1}!`").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("TEMPLATE"), "missing TEMPLATE op in:\n{text}");
    }

    async fn eval_string(src: &str) -> String {
        match crate::vm_eval_source(src).await.expect("evaluates") {
            Value::Str(s) => s.to_string(),
            other => panic!("expected Str, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn template_interpolation_evaluates() {
        assert_eq!(eval_string("`hi ${1+2}!`").await, "hi 3!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn template_coerces_non_strings() {
        assert_eq!(eval_string("`b=${true} n=${42}`").await, "b=true n=42");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hex_literal_evaluates() {
        assert_eq!(eval_number("0xff").await, 255.0);
    }

    #[test]
    fn let_emits_set_local_and_sizes_slots() {
        let chunk = compile_source("let x = 1\nx").expect("compiles");
        assert!(chunk.slot_count >= 1, "slot_count not sized: {}", chunk.slot_count);
        let text = disasm(&chunk);
        assert!(text.contains("SET_LOCAL"), "missing SET_LOCAL in:\n{text}");
        assert!(text.contains("GET_LOCAL"), "missing GET_LOCAL in:\n{text}");
    }

    #[test]
    fn assign_dups_then_set_local() {
        // Assignment-as-expression: value, DUP (result stays), SET_LOCAL (stores).
        let chunk = compile_source("let x = 1\nx = 2").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("DUP"), "missing DUP in:\n{text}");
        assert!(text.contains("SET_LOCAL"), "missing SET_LOCAL in:\n{text}");
    }

    #[test]
    fn block_shadowing_uses_distinct_slots() {
        // Outer x and inner x are distinct slots → slot_count is at least 2.
        let chunk = compile_source("let x = 1\n{ let x = 2\n print(x) }\nprint(x)").expect("compiles");
        assert!(chunk.slot_count >= 2, "shadowing should allocate ≥2 slots, got {}", chunk.slot_count);
    }

    #[test]
    fn array_destructure_lowers_to_check_elem_and_setlocal() {
        // `let [a, b] = [1, 2]` validates the RHS once (CHECK_ARRAY_DESTRUCTURE),
        // then reads each position (ARRAY_ELEM) and stores it (SET_LOCAL).
        let chunk = compile_source("let [a, b] = [1, 2]\nprint(a)\nprint(b)").expect("compiles");
        let text = disasm(&chunk);
        assert!(
            text.contains("CHECK_ARRAY_DESTRUCTURE"),
            "expected RHS type check, got:\n{text}"
        );
        assert!(text.contains("ARRAY_ELEM"), "expected ARRAY_ELEM, got:\n{text}");
        assert!(text.contains("SET_LOCAL"), "expected SET_LOCAL store, got:\n{text}");
    }

    #[test]
    fn array_rest_lowers_to_array_rest_op() {
        let chunk =
            compile_source("let [a, ...rest] = [1, 2, 3]\nprint(rest)").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("ARRAY_REST"), "expected ARRAY_REST, got:\n{text}");
    }

    #[test]
    fn object_destructure_lowers_to_check_objectkey_and_objectrest() {
        let chunk = compile_source("let {a, ...rest} = {a: 1, b: 2}\nprint(rest)").expect("compiles");
        let text = disasm(&chunk);
        assert!(
            text.contains("CHECK_OBJECT_DESTRUCTURE"),
            "expected RHS type check, got:\n{text}"
        );
        assert!(text.contains("OBJECT_KEY"), "expected OBJECT_KEY, got:\n{text}");
        assert!(text.contains("OBJECT_REST"), "expected OBJECT_REST, got:\n{text}");
    }

    #[test]
    fn compound_assignment_lowers_to_load_binop_store() {
        // `x += 2` desugars (like the tree-walker) to `x = (x + 2)`: load the
        // current value, push the rhs, ADD, then store back (DUP + SET_LOCAL so the
        // assignment expression yields the new value).
        let chunk = compile_source("let x = 1\nx += 2\nx").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("ADD"), "expected ADD for `+=`, got:\n{text}");
        assert!(
            text.contains("SET_LOCAL"),
            "expected SET_LOCAL store, got:\n{text}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn let_and_local_read_evaluates() {
        assert_eq!(eval_number("let x = 1\nlet y = x + 1\ny").await, 2.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reassignment_evaluates() {
        assert_eq!(eval_number("let x = 1\nx = x + 5\nx").await, 6.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn assignment_expression_yields_value() {
        // The trailing `x = 5` is the program's value: assignment yields the
        // assigned value.
        assert_eq!(eval_number("let x = 1\nx = 5").await, 5.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn block_shadowing_does_not_leak() {
        // After the block, the outer x is still 1.
        assert_eq!(eval_number("let x = 1\n{ let x = 2 }\nx").await, 1.0);
    }

    // ---- V2-T4b: array/object literals + index/member read ---------------

    #[test]
    fn array_literal_emits_new_array() {
        let chunk = compile_source("[1, 2, 3]").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("NEW_ARRAY"), "missing NEW_ARRAY in:\n{text}");
    }

    #[test]
    fn object_literal_emits_new_object() {
        // A top-level `{...}` parses as a block, so the object literal must sit in
        // an unambiguous expression position (an initializer here).
        let chunk = compile_source("let o = {a: 1, b: 2}\no").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("NEW_OBJECT"), "missing NEW_OBJECT in:\n{text}");
    }

    #[test]
    fn index_read_emits_get_index() {
        let chunk = compile_source("[10, 20][1]").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("GET_INDEX"), "missing GET_INDEX in:\n{text}");
    }

    #[test]
    fn member_read_emits_get_prop() {
        let chunk = compile_source("let o = {a: 1}\no.a").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("GET_PROP") && !text.contains("GET_PROP_OPT"),
            "missing GET_PROP in:\n{text}");
    }

    #[test]
    fn opt_member_read_emits_get_prop_opt() {
        let chunk = compile_source("let o = {a: 1}\no?.a").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("GET_PROP_OPT"), "missing GET_PROP_OPT in:\n{text}");
    }

    #[test]
    fn array_spread_uses_incremental_builder() {
        // A spread switches the array literal to the `NEW_ARRAY 0` +
        // `APPEND_ARRAY`/`SPREAD` builder (V10-T2).
        let chunk = compile_source("let a = [1]\nlet b = [0, ...a]\nb").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("APPEND_ARRAY"), "expected APPEND_ARRAY in:\n{text}");
        assert!(
            text.contains("SPREAD") && !text.contains("SPREAD_OBJECT"),
            "expected SPREAD (array) in:\n{text}"
        );
    }

    #[test]
    fn array_without_spread_keeps_fixed_new_array() {
        // No spread → the fast path still emits a single `NEW_ARRAY n` (byte-
        // identical to the prior lowering — no builder ops).
        let chunk = compile_source("let b = [1, 2, 3]\nb").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("NEW_ARRAY"), "expected NEW_ARRAY in:\n{text}");
        assert!(!text.contains("APPEND_ARRAY"), "unexpected APPEND_ARRAY in:\n{text}");
        assert!(!text.contains("SPREAD"), "unexpected SPREAD in:\n{text}");
    }

    #[test]
    fn object_spread_uses_incremental_builder() {
        // A spread switches the object literal to the `NEW_OBJECT 0` +
        // `APPEND_OBJECT`/`SPREAD_OBJECT` builder (V10-T2).
        let chunk = compile_source("let o = {a: 1}\nlet p = {...o, b: 2}\np").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("SPREAD_OBJECT"), "expected SPREAD_OBJECT in:\n{text}");
        assert!(text.contains("APPEND_OBJECT"), "expected APPEND_OBJECT in:\n{text}");
    }

    #[test]
    fn call_spread_uses_call_spread_op() {
        // A spread argument switches the call to the args-array builder +
        // `CALL_SPREAD` (dynamic arity) (V10-T2).
        let chunk =
            compile_source("fn f(x) { return x }\nlet a = [1]\nf(...a)").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("SPREAD_ARGS"), "expected SPREAD_ARGS in:\n{text}");
        assert!(text.contains("CALL_SPREAD"), "expected CALL_SPREAD in:\n{text}");
    }

    #[test]
    fn member_method_spread_is_rejected() {
        // Spread in a member-method call is a documented deferral (clean error,
        // no divergence) — bare-name and `super.m(...)` spread calls ARE supported.
        let err = compile_source("let o = {}\nlet a = [1]\no.m(...a)").unwrap_err();
        assert!(
            err.message.contains("spread in a member-method call"),
            "got {err:?}"
        );
    }

    #[test]
    fn index_assignment_compiles_to_set_index() {
        // Index ASSIGNMENT `a[0] = 9` lowers to `<value> <obj> <idx> ROT3
        // SET_INDEX` (value-first eval order mirrors the tree-walker; ROT3 reorders
        // into the `[obj, idx, value]` layout SET_INDEX consumes).
        let chunk = compile_source("let a = [1]\na[0] = 9").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("SET_INDEX"), "expected SET_INDEX, got:\n{text}");
        assert!(text.contains("ROT3"), "expected ROT3, got:\n{text}");
    }

    #[test]
    fn member_assignment_evaluates_value_before_receiver() {
        // `o.a = 9` lowers to `<value> <obj> SWAP SET_PROP "a"` — value FIRST, then
        // the receiver (matching the tree-walker's `assign_to`), with SWAP putting
        // them in `[obj, value]` order for SET_PROP.
        let chunk = compile_source("let o = {a: 1}\no.a = 9").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("SET_PROP"), "expected SET_PROP, got:\n{text}");
        assert!(text.contains("SWAP"), "expected SWAP, got:\n{text}");
    }

    #[test]
    fn member_assignment_compiles_to_set_prop() {
        // Member ASSIGNMENT `o.a = 9` lowers to `<obj> <value> SET_PROP "a"` (V9).
        let chunk = compile_source("let o = {a: 1}\no.a = 9").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("SET_PROP"), "expected SET_PROP, got:\n{text}");
    }

    #[test]
    fn object_string_key_is_unescaped() {
        // A quoted key with an escape must decode to the same key string the
        // tree-walker uses (the legacy lexer pre-decodes `Tok::Str`).
        let chunk = compile_source("let o = {\"a\\nb\": 1}\no").expect("compiles");
        let text = disasm(&chunk);
        // The key const should contain a real newline, rendered as the escaped
        // form `a\nb` by the disassembler's `{:?}`-style string const printer.
        assert!(text.contains("a\\nb"), "key not unescaped in:\n{text}");
    }

    // ---- V3-T3: for-range + compiler scratch slots -----------------------

    #[test]
    fn for_range_reserves_a_scratch_slot_above_named_locals() {
        // The loop var `i` is the single named local (slot 0). The hoisted `end`
        // bound takes a SCRATCH slot ABOVE it, so the chunk reserves ≥2 slots even
        // though only one source name exists.
        let chunk = compile_source("for (i in 0..3) { print(i) }").expect("compiles");
        assert!(
            chunk.slot_count >= 2,
            "for-range should reserve a scratch slot for the hoisted end bound (got {})",
            chunk.slot_count
        );
    }

    #[test]
    fn for_range_emits_check_numbers_guard_and_loop() {
        // The lowering must include the eager bounds guard (CHECK_NUMBERS), the
        // exclusive comparison (LT), and a backward LOOP back-edge.
        let chunk = compile_source("for (i in 0..3) { print(i) }").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("CHECK_NUMBERS"), "no bounds guard in:\n{text}");
        assert!(text.contains("LT"), "no exclusive comparison in:\n{text}");
        assert!(text.contains("LOOP"), "no loop back-edge in:\n{text}");
    }

    #[test]
    fn for_range_inclusive_is_rejected() {
        // `..=` for-range is rejected (the tree-walker's parser rejects it), never
        // silently treated as exclusive.
        let err = compile_source("for (i in 0..=3) { print(i) }").unwrap_err();
        assert!(err.message.contains("inclusive for-range"), "got {err:?}");
    }

    #[test]
    fn for_of_emits_snapshot_and_loop() {
        // The sync for-of lowering materializes a snapshot (ITER_SNAPSHOT), reads
        // its length once (ARRAY_LEN), index-iterates (GET_INDEX) with the
        // exclusive comparison (LT), and has a backward LOOP back-edge.
        let chunk = compile_source("for (x of [1, 2, 3]) { print(x) }").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("ITER_SNAPSHOT"), "no snapshot in:\n{text}");
        assert!(text.contains("ARRAY_LEN"), "no length read in:\n{text}");
        assert!(text.contains("GET_INDEX"), "no index read in:\n{text}");
        assert!(text.contains("LT"), "no exclusive comparison in:\n{text}");
        assert!(text.contains("LOOP"), "no loop back-edge in:\n{text}");
    }

    #[test]
    fn for_of_over_range_value_compiles() {
        // `for (x of 0..5)` iterates the materialized range ARRAY — a for-OF, not a
        // for-RANGE — so the iterable is a RangeExpr compiled to a RANGE array,
        // then snapshotted and iterated.
        let chunk = compile_source("for (x of 0..5) { print(x) }").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("RANGE"), "no range array build in:\n{text}");
        assert!(text.contains("ITER_SNAPSHOT"), "no snapshot in:\n{text}");
    }

    #[test]
    fn for_await_emits_iter_protocol() {
        // `for await` lowers to the lazy async-iteration protocol (GET_ITER /
        // ITER_NEXT / ITER_CLOSE), NOT a snapshot.
        let chunk = compile_source("fn* g() { yield 1 }\nfor await (x of g()) { print(x) }")
            .expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("GET_ITER"), "no GET_ITER in:\n{text}");
        assert!(text.contains("ITER_NEXT"), "no ITER_NEXT in:\n{text}");
        assert!(
            !text.contains("ITER_SNAPSHOT"),
            "for await must not snapshot:\n{text}"
        );
    }

    #[test]
    fn for_await_break_emits_iter_close() {
        // A `break` out of a `for await` over a generator closes the iterator,
        // mirroring the tree-walker's `g.close()`.
        let chunk =
            compile_source("fn* g() { yield 1 }\nfor await (x of g()) { break }").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("ITER_CLOSE"), "no ITER_CLOSE on break in:\n{text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn for_range_accumulates() {
        // 1+2+3+4 = 10; the trailing read proves the outer local survived the loop.
        assert_eq!(
            eval_number("let sum = 0\nfor (i in 1..5) { sum = sum + i }\nsum").await,
            10.0
        );
    }

    // ---- V4-T2: functions / arrows → FnProto + CLOSURE -------------------

    #[test]
    fn fn_decl_emits_closure_and_set_local() {
        // A `fn` declaration builds a nested proto, makes a closure over it
        // (CLOSURE), and binds it to the name slot (SET_LOCAL). The proto's own
        // body holds the function instructions (`CONST 1; RETURN`).
        let chunk = compile_source("fn greet() { return 1 }\n").expect("compiles");
        let text = disasm(&chunk);
        // Top chunk: CLOSURE 0 referencing proto #0 (named greet) + SET_LOCAL.
        assert!(text.contains("CLOSURE"), "missing CLOSURE in:\n{text}");
        assert!(text.contains("proto #0 greet"), "missing named proto ref in:\n{text}");
        assert!(text.contains("SET_LOCAL"), "missing SET_LOCAL (name bind) in:\n{text}");
        // Nested proto header + its body.
        assert!(
            text.contains("== fn greet (proto #0) =="),
            "missing nested proto header in:\n{text}"
        );
        assert!(
            text.contains("CONST") && text.contains("; 1"),
            "missing CONST 1 in proto body:\n{text}"
        );
        assert!(text.contains("RETURN"), "missing RETURN in proto body:\n{text}");
    }

    #[test]
    fn arrow_emits_closure_with_param_local() {
        // An arrow `(x) => x + 1` is an EXPRESSION: it builds a proto and leaves a
        // CLOSURE on the stack (bound here via `let f`). The proto's param `x` is
        // slot 0 (GET_LOCAL 0), and the implicit-return expression ends in RETURN.
        let chunk = compile_source("let f = (x) => x + 1\n").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("CLOSURE"), "missing CLOSURE in:\n{text}");
        assert!(text.contains("SET_LOCAL"), "missing SET_LOCAL f in:\n{text}");
        // Proto body: GET_LOCAL 0 (x); CONST 1; ADD; RETURN.
        assert!(text.contains("GET_LOCAL"), "missing GET_LOCAL (param x) in:\n{text}");
        assert!(text.contains("ADD"), "missing ADD in proto body:\n{text}");
        assert!(text.contains("RETURN"), "missing RETURN in proto body:\n{text}");
    }

    #[test]
    fn arrow_proto_has_arity_one() {
        // The arrow `(x) => x` has arity 1 (one positional param, no rest).
        let chunk = compile_source("let f = (x) => x\n").expect("compiles");
        let proto = chunk.protos.first().expect("one nested proto");
        assert_eq!(proto.arity, 1);
        assert!(!proto.has_rest);
        assert!(!proto.is_async);
        assert!(!proto.is_generator);
    }

    #[test]
    fn empty_fn_body_returns_nil() {
        // A function that falls off the end returns nil: the proto body is exactly
        // `NIL; RETURN` (mirrors the tree-walker's `Flow::Normal => Value::Nil`).
        let chunk = compile_source("fn noop() {}\n").expect("compiles");
        let proto = chunk.protos.first().expect("one nested proto");
        // The proto's code is exactly the two zero-operand ops NIL then RETURN.
        assert_eq!(
            proto.chunk.code,
            vec![Op::Nil as u8, Op::Return as u8],
            "empty fn body should compile to NIL; RETURN"
        );
    }

    #[test]
    fn fn_capturing_outer_local_compiles_with_upvalue() {
        // A function reading an outer-scope local captures it by reference (V4-T3).
        // `n` is captured, so it is a CELL SLOT in the file frame (SET_LOCAL_CELL
        // binds it) and the inner body reads it via GET_UPVALUE; the inner proto
        // carries a one-entry upvalue capture plan.
        let chunk = compile_source("let n = 1\nfn f() { return n }\nf").expect("compiles");
        let text = disasm(&chunk);
        // The captured top-level binding is filled through its cell.
        assert!(
            text.contains("SET_LOCAL_CELL"),
            "captured `n` should bind via SET_LOCAL_CELL in:\n{text}"
        );
        // The inner proto reads the capture via GET_UPVALUE and has a capture plan.
        let proto = chunk.protos.first().expect("one nested proto");
        assert_eq!(
            proto.chunk.upvalues.len(),
            1,
            "inner fn captures exactly one upvalue (`n`)"
        );
        let inner = disasm(&proto.chunk);
        assert!(
            inner.contains("GET_UPVALUE"),
            "inner body should read `n` via GET_UPVALUE in:\n{inner}"
        );
    }

    #[test]
    fn fn_using_only_params_and_globals_compiles() {
        // Params + builtins (globals) need no captures, so this compiles today.
        let chunk = compile_source("fn add(a, b) { return a + b }\n").expect("compiles");
        let proto = chunk.protos.first().expect("one nested proto");
        assert_eq!(proto.arity, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn arrow_value_is_a_function() {
        // The CLOSURE op materializes a Value::Closure; `type()` reports it as
        // "function" (exercises CLOSURE exec + the type() builtin without CALL).
        assert_eq!(eval_string("let f = (x) => x\ntype(f)").await, "function");
    }
}
