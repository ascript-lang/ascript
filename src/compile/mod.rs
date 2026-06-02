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
    ArrayExpr, AssignExpr, AstNode, BinaryExpr, Block, BreakStmt, CallExpr, ContinueStmt, Expr,
    ForStmt, IfStmt, IndexExpr, LetStmt, Literal, MemberExpr, NameRef, ObjectExpr, OptMemberExpr,
    ParenExpr, RangeExpr, SourceFile, Stmt, TemplateExpr, TernaryExpr, UnaryExpr, WhileStmt,
};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{ResolveResult, Resolution};
use crate::syntax::{parse_to_tree, resolve::resolve};
use crate::value::Value;
use crate::vm::chunk::Chunk;
use crate::vm::opcode::Op;
use cstree::text::TextRange;
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
    let slot_count = resolved
        .frames
        .get(&top_key)
        .map(|f| f.slot_count)
        .unwrap_or(0);
    chunk.slot_count = u16::try_from(slot_count).map_err(|_| {
        CompileError::new(
            "too many local slots in top-level frame (max 65535)",
            Span::new(0, src.len()),
        )
    })?;

    // Scratch temporaries are allocated ABOVE the named-local window, so seed the
    // temp cursor from the same slot count the chunk was sized with.
    let next_temp = chunk.slot_count;
    let mut compiler = Compiler {
        chunk,
        resolved,
        loops: Vec::new(),
        next_temp,
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
}

impl Compiler {
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
            other => Err(CompileError::new(
                "expression kind not yet supported in V2",
                node_span(other),
            )),
        }
    }

    /// Lower a bare identifier reference (`NameRef`). The resolver classifies the
    /// use via its `text_range()`: a `Local(slot)` reads the frame's slot
    /// (`GET_LOCAL`); a `Global(name)` that is a known builtin is a first-class
    /// builtin reference (`GET_GLOBAL`, yielding the `Value::Builtin` — e.g.
    /// `let p = print`, exactly as the tree-walker treats a bare builtin name);
    /// `Upvalue` is a closure capture (V5) and a non-builtin `Global` is a
    /// user-global reference, which does not exist at runtime (top-level `let`s
    /// are frame-locals) so it is a documented V4 deferral.
    fn compile_name_ref(&mut self, name_ref: &NameRef) -> Result<(), CompileError> {
        let span = node_span(name_ref);
        let key = name_ref.syntax().text_range();
        match self.resolved.uses.get(&key) {
            Some(Resolution::Local(slot)) => {
                let slot = u16::try_from(*slot).map_err(|_| {
                    CompileError::new("local slot index exceeds 65535", span)
                })?;
                self.chunk.emit_u16(Op::GetLocal, slot, span);
                Ok(())
            }
            Some(Resolution::Upvalue(_)) => Err(CompileError::new(
                "upvalue (closure capture) reads not yet supported (V5)",
                span,
            )),
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

    /// Lower an assignment expression `target = value`. V2 supports only a plain
    /// `=` to a local-binding target (a `NameRef` resolving to `Local(slot)`).
    ///
    /// Stack convention: `SET_LOCAL` POPS the value and stores it (clean stack
    /// discipline). Assignment is an *expression* that yields the assigned value,
    /// so we compile the value, `DUP` it (leaving the result copy on the stack),
    /// then `SET_LOCAL`. Used as a statement, the surrounding `POP` discards the
    /// leftover copy. This mirrors the tree-walker's `ExprKind::Assign`, which
    /// evaluates the value and returns it as the expression result.
    ///
    /// Compound assignment (`+=`/`-=`/`*=`/`/=`) and non-`NameRef` targets
    /// (index/member) are later deferrals (V9+).
    fn compile_assign(&mut self, assign: &AssignExpr) -> Result<(), CompileError> {
        let span = node_span(assign);
        // Only a plain `=` operator is supported; reject compound assignment.
        let is_plain_eq = assign
            .syntax()
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::Eq);
        if !is_plain_eq {
            return Err(CompileError::new(
                "compound assignment (+=/-=/*=//=) not yet supported (V9)",
                span,
            ));
        }

        let target = assign
            .target()
            .ok_or_else(|| CompileError::new("assignment missing target", span))?;
        let Expr::NameRef(name_ref) = &target else {
            return Err(CompileError::new(
                "assignment to non-identifier targets (index/member) not yet supported (V9)",
                node_span(&target),
            ));
        };
        let slot = match self.resolved.uses.get(&name_ref.syntax().text_range()) {
            Some(Resolution::Local(slot)) => u16::try_from(*slot)
                .map_err(|_| CompileError::new("local slot index exceeds 65535", span))?,
            Some(Resolution::Upvalue(_)) => {
                return Err(CompileError::new(
                    "assignment to a captured upvalue not yet supported (V5)",
                    node_span(&target),
                ))
            }
            _ => {
                return Err(CompileError::new(
                    "assignment to a non-local target not yet supported (V4)",
                    node_span(&target),
                ))
            }
        };

        let value = assign
            .value()
            .ok_or_else(|| CompileError::new("assignment missing value", span))?;
        self.compile_expr(&value)?;
        // Leave the assigned value as the expression's result, then store a copy.
        self.chunk.emit(Op::Dup, span);
        self.chunk.emit_u16(Op::SetLocal, slot, span);
        Ok(())
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

        // `for await` is async iteration (V7); neither a range loop nor a sync
        // for-of.
        if for_stmt.await_token().is_some() {
            return Err(CompileError::new("for await: V7", span));
        }

        let body = for_stmt
            .body()
            .ok_or_else(|| CompileError::new("for statement missing body", span))?;

        // The CST head holds the iterable/bounds expression plus an `in`/`of`
        // operator. A for-RANGE is `in` + a `RangeExpr` iterable; the
        // iterator-driven `for (x of ...)` form is a sync for-of — INCLUDING `for
        // (x of a..b)`, which materializes the range ARRAY then iterates it (a
        // different construct from the range loop).
        let iter = for_stmt
            .iter()
            .ok_or_else(|| CompileError::new("for statement missing iterable/start bound", span))?;

        // `of` → sync for-of (snapshot iteration over Array/Str). The iterable can
        // be any expression (array literal, name, even a `RangeExpr` that builds
        // the range array).
        if for_stmt.op() == Some(SyntaxKind::OfKw) {
            return self.compile_for_of(for_stmt, &iter, &body);
        }

        let is_in = for_stmt.op() == Some(SyntaxKind::InKw);
        let Expr::RangeExpr(range) = &iter else {
            return Err(CompileError::new(
                "for-of (iterator-based for) not yet supported (V3-T4)",
                span,
            ));
        };
        if !is_in {
            return Err(CompileError::new(
                "for-of (iterator-based for) not yet supported (V3-T4)",
                span,
            ));
        }

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
        // Anchor the bounds-numbers panic at the START bound's CODE start (trivia
        // trimmed), byte-identical to the tree-walker's `AsError::at(_, start.span)`.
        let start_span = node_code_span(&start);

        // Evaluate start then end (start below, end on top), guard both are
        // numbers (panic anchored at the START bound's span, matching the
        // tree-walker), then store end ONCE and seed `i = start`.
        self.compile_expr(&start)?;
        self.compile_expr(&end)?;
        self.chunk.emit(Op::CheckNumbers, start_span);
        self.chunk.emit_u16(Op::SetLocal, end_slot, span);
        self.chunk.emit_u16(Op::SetLocal, var_slot, span);

        // Condition: re-test `i < end` each iteration.
        let cond_start = self.chunk.code.len();
        self.chunk.emit_u16(Op::GetLocal, var_slot, span);
        self.chunk.emit_u16(Op::GetLocal, end_slot, span);
        self.chunk.emit(Op::Lt, span);
        let exit = self.chunk.emit_jump(Op::JumpIfFalse, span);

        // The continue target is the INCREMENT (so `continue` runs `i += 1` then
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

        // Increment: `i = i + 1`. This is where every `continue` lands — at the
        // CURRENT end of code, which is exactly what `patch_jump` targets — so
        // patch every recorded forward `continue` site here, BEFORE emitting the
        // increment instructions.
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
        self.chunk.emit_u16(Op::GetLocal, var_slot, span);
        let one = self.chunk.add_const(Value::Number(1.0));
        self.chunk.emit_u16(Op::Const, one, span);
        self.chunk.emit(Op::Add, span);
        self.chunk.emit_u16(Op::SetLocal, var_slot, span);

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

        // Bind the loop var to `arr[idx]` at the top of each iteration.
        self.chunk.emit_u16(Op::GetLocal, arr_slot, span);
        self.chunk.emit_u16(Op::GetLocal, idx_slot, span);
        self.chunk.emit(Op::GetIndex, span);
        self.chunk.emit_u16(Op::SetLocal, var_slot, span);

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
        // of a plain ident token; defer it to V10.
        if let_stmt
            .syntax()
            .children()
            .any(|c| matches!(c.kind(), SyntaxKind::ArrayBindPat | SyntaxKind::ObjectBindPat))
        {
            return Err(CompileError::new(
                "destructuring let (array/object pattern) not yet supported (V10)",
                span,
            ));
        }

        let slot = self.let_slot(let_stmt)?;

        match let_stmt.expr() {
            Some(init) => self.compile_expr(&init)?,
            // `let x` with no initializer binds nil (mirrors the tree-walker).
            None => self.chunk.emit(Op::Nil, span),
        }
        self.chunk.emit_u16(Op::SetLocal, slot, span);
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

    /// Lower a call whose callee is a bare builtin name (`print`, `len`, `type`,
    /// …): `GET_GLOBAL <name>`, then each argument, then `CALL argc`.
    ///
    /// V2 supports only calls to bare builtins. The callee is classified via the
    /// resolver: a `NameRef` whose use resolves to `Resolution::Global(name)`
    /// where `name` is a known builtin. Anything else (method calls, calls to
    /// user functions/locals/upvalues) is a documented V4 deferral.
    fn compile_call(&mut self, call: &CallExpr) -> Result<(), CompileError> {
        let span = node_span(call);
        let callee = call
            .expr()
            .ok_or_else(|| CompileError::new("call expression missing callee", span))?;

        // Only a bare `NameRef` callee is supported in V2.
        let Expr::NameRef(name_ref) = &callee else {
            return Err(CompileError::new(
                "calls to non-builtins not yet supported (V4)",
                node_span(&callee),
            ));
        };

        // Classify the callee via the resolver: it must be a Global builtin.
        let key = name_ref.syntax().text_range();
        let builtin_name = match self.resolved.uses.get(&key) {
            Some(Resolution::Global(name)) if crate::interp::BUILTIN_NAMES.contains(&name.as_str()) => {
                name.clone()
            }
            _ => {
                return Err(CompileError::new(
                    "calls to non-builtins not yet supported (V4)",
                    node_span(&callee),
                ));
            }
        };

        // GET_GLOBAL <name-const>
        let name_idx = self.chunk.add_const(Value::Str(Rc::from(builtin_name.as_str())));
        self.chunk.emit_u16(Op::GetGlobal, name_idx, span);

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

    fn compile_literal(&mut self, lit: &Literal) -> Result<(), CompileError> {
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
            SyntaxKind::Str => {
                Value::Str(Rc::from(unescape_str_body(strip_quotes(&text)).as_str()))
            }
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
        let idx = self.chunk.add_const(value);
        self.chunk.emit_u16(Op::Const, idx, span);
        Ok(())
    }

    fn compile_binary(&mut self, bin: &BinaryExpr) -> Result<(), CompileError> {
        let span = node_span(bin);
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
        let span = node_span(range);
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
    /// A spread element `[...a]` is a documented V10 deferral: the CST records it
    /// as a `SpreadElem` child (NOT an `Expr`), so its presence is detected by a
    /// `SpreadElem` child node and rejected with a clear `CompileError`.
    fn compile_array(&mut self, arr: &ArrayExpr) -> Result<(), CompileError> {
        let span = node_span(arr);
        if arr
            .syntax()
            .children()
            .any(|c| c.kind() == SyntaxKind::SpreadElem)
        {
            return Err(CompileError::new(
                "spread in an array literal ([...a]) not yet supported (V10)",
                span,
            ));
        }
        let mut n: u16 = 0;
        for elem in arr.exprs() {
            self.compile_expr(&elem)?;
            n = n
                .checked_add(1)
                .ok_or_else(|| CompileError::new("array literal has too many elements", span))?;
        }
        self.chunk.emit_u16(Op::NewArray, n, span);
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
    /// Object-spread `{...o}` is a documented V10 deferral (a `SpreadElem` child).
    fn compile_object(&mut self, obj: &ObjectExpr) -> Result<(), CompileError> {
        let span = node_span(obj);
        if obj
            .syntax()
            .children()
            .any(|c| c.kind() == SyntaxKind::SpreadElem)
        {
            return Err(CompileError::new(
                "spread in an object literal ({...o}) not yet supported (V10)",
                span,
            ));
        }
        let mut n: u16 = 0;
        for field in obj.object_fields() {
            let fspan = node_span(&field);
            // The key token is an `Ident` or a `Str`; decode it to the same
            // string the tree-walker keys by.
            let key = object_field_key(&field)
                .ok_or_else(|| CompileError::new("object field has no key", fspan))?;
            let value = field
                .value()
                .ok_or_else(|| CompileError::new("object field has no value", fspan))?;
            let key_idx = self.chunk.add_const(Value::Str(Rc::from(key.as_str())));
            self.chunk.emit_u16(Op::Const, key_idx, fspan);
            self.compile_expr(&value)?;
            n = n
                .checked_add(1)
                .ok_or_else(|| CompileError::new("object literal has too many fields", span))?;
        }
        self.chunk.emit_u16(Op::NewObject, n, span);
        Ok(())
    }

    /// Lower an index read `a[i]`: compile the receiver, compile the index, then
    /// `GET_INDEX`. The op carries the whole `IndexExpr`'s span (the tree-walker's
    /// `expr.span`, used for the array-index / out-of-bounds / non-string-key
    /// panics). Index ASSIGNMENT (`a[i] = x`) is a V9 deferral handled in
    /// `compile_assign` (its target is not a `NameRef`).
    fn compile_index(&mut self, ix: &IndexExpr) -> Result<(), CompileError> {
        let span = node_span(ix);
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
        let obj_span = node_span(&object);
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
        let obj_span = node_span(&object);
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
        // operand span so the VM's diagnostics are byte-identical.
        let operand_span = node_span(&operand);
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
    fn rejects_call_to_non_builtin() {
        // `foo` is not a builtin; resolver classifies it Global("foo") which is
        // not in BUILTIN_NAMES → documented V4 deferral.
        let err = compile_source("foo(1)").unwrap_err();
        assert!(
            err.message.contains("non-builtins not yet supported (V4)"),
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
    fn rejects_destructuring_let() {
        let err = compile_source("let [a, b] = arr").unwrap_err();
        assert!(err.message.contains("destructuring let"), "got {err:?}");
    }

    #[test]
    fn rejects_compound_assignment() {
        let err = compile_source("let x = 1\nx += 2").unwrap_err();
        assert!(err.message.contains("compound assignment"), "got {err:?}");
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
    fn array_spread_is_deferred() {
        let err = compile_source("let a = [1]\nlet b = [...a]\nb").unwrap_err();
        assert!(err.message.contains("spread in an array literal"), "got {err:?}");
    }

    #[test]
    fn object_spread_is_deferred() {
        let err = compile_source("let o = {a: 1}\nlet p = {...o}\np").unwrap_err();
        assert!(err.message.contains("spread in an object literal"), "got {err:?}");
    }

    #[test]
    fn index_assignment_is_deferred() {
        // Index ASSIGNMENT routes through compile_assign (target is not a NameRef).
        let err = compile_source("let a = [1]\na[0] = 9").unwrap_err();
        assert!(err.message.contains("index/member"), "got {err:?}");
    }

    #[test]
    fn member_assignment_is_deferred() {
        let err = compile_source("let o = {a: 1}\no.a = 9").unwrap_err();
        assert!(err.message.contains("index/member"), "got {err:?}");
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
    fn for_await_is_deferred() {
        // `for await` (async iteration) is V7.
        let err = compile_source("for await (x of [1, 2]) { print(x) }").unwrap_err();
        assert!(err.message.contains("for await: V7"), "got {err:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn for_range_accumulates() {
        // 1+2+3+4 = 10; the trailing read proves the outer local survived the loop.
        assert_eq!(
            eval_number("let sum = 0\nfor (i in 1..5) { sum = sum + i }\nsum").await,
            10.0
        );
    }
}
