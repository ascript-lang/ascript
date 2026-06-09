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
        Out {
            buf: String::new(),
            indent: 0,
            at_line_start: true,
        }
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
    fn indent(&mut self) {
        self.indent += 1;
    }
    fn dedent(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }

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
    let mut p = Printer {
        out: &mut out,
        comments: &comments,
    };
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
        let stmts: Vec<&ResolvedNode> = node
            .children()
            .filter(|c| !matches!(c.kind(), SyntaxKind::Error | SyntaxKind::Tombstone))
            .collect();
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

    /// Format a statement. 4a handles ExprStmt/LetStmt/ReturnStmt/Block/FnDecl + a fallback.
    fn stmt(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
            ExprStmt => {
                if let Some(e) = node.children().next() {
                    self.expr(e);
                }
                self.out.newline();
            }
            LetStmt => {
                use SyntaxKind::*;
                let kw = first_kw_text(node);
                self.out.text(&kw);
                self.out.text(" ");
                if let Some(arr) = node.children().find(|c| c.kind() == ArrayBindPat) {
                    self.bind_pat(arr, "[", "]");
                } else if let Some(obj) = node.children().find(|c| c.kind() == ObjectBindPat) {
                    self.bind_pat(obj, "{", "}");
                } else if let Some(name) = first_ident_text(node) {
                    self.out.text(&name);
                }
                if let Some(ty) = node.children().find(|c| is_type_kind(c.kind())) {
                    self.out.text(": ");
                    self.type_ann(ty);
                }
                if let Some(init) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.out.text(" = ");
                    self.expr(init);
                }
                self.out.newline();
            }
            ReturnStmt => {
                self.out.text("return");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.out.text(" ");
                    self.expr(e);
                }
                self.out.newline();
            }
            Block => self.block(node),
            FnDecl => self.fn_decl(node),
            ClassDecl => self.class_decl(node),
            IfStmt => {
                self.out.text("if (");
                let parts: Vec<&ResolvedNode> = node.children().collect();
                if let Some(cond) = parts.iter().copied().find(|c| is_expr_kind(c.kind())) {
                    self.expr(cond);
                }
                self.out.text(") ");
                let blocks: Vec<&ResolvedNode> = parts
                    .iter()
                    .copied()
                    .filter(|c| c.kind() == Block)
                    .collect();
                if let Some(then) = blocks.first() {
                    self.block_inline(then);
                }
                if let Some(elif) = parts.iter().copied().find(|c| c.kind() == IfStmt) {
                    self.out.text(" else ");
                    self.stmt(elif);
                } else if let Some(els) = blocks.get(1) {
                    self.out.text(" else ");
                    self.block(els);
                } else {
                    self.out.newline();
                }
            }
            WhileStmt => {
                self.out.text("while (");
                if let Some(cond) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(cond);
                }
                self.out.text(") ");
                if let Some(b) = node.children().find(|c| c.kind() == Block) {
                    self.block(b);
                }
            }
            ForStmt => self.for_stmt(node),
            BreakStmt => {
                self.out.text("break");
                self.out.newline();
            }
            ContinueStmt => {
                self.out.text("continue");
                self.out.newline();
            }
            EnumDecl => self.enum_decl(node),
            InterfaceDecl => self.interface_decl(node),
            ImportStmt => {
                self.out.text(&normalize_import(node));
                self.out.newline();
            }
            ExportStmt => {
                self.out.text("export ");
                if let Some(inner) = node.children().next() {
                    self.stmt(inner);
                }
            }
            _ => {
                self.out.text(&node.text().to_string());
                self.out.newline();
            }
        }
    }

    fn block(&mut self, node: &ResolvedNode) {
        self.block_inline(node);
        self.out.newline();
    }

    fn block_inline(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("{");
        self.out.newline();
        self.out.indent();
        let stmts: Vec<&ResolvedNode> = node
            .children()
            .filter(|c| !matches!(c.kind(), Error | Tombstone))
            .collect();
        for (i, s) in stmts.iter().enumerate() {
            if i > 0 {
                let blank = self
                    .comments
                    .leading
                    .get(&s.text_range())
                    .and_then(|l| l.first())
                    .map(|c| c.blank_before)
                    .unwrap_or(false);
                if blank {
                    self.out.blank();
                }
            }
            self.emit_leading(s);
            self.stmt(s);
            self.emit_trailing(s);
        }
        self.out.dedent();
        self.out.text("}");
    }

    fn fn_decl(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        let toks: Vec<SyntaxKind> = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .map(|t| t.kind())
            .collect();
        // Canonical modifier order: `worker? async? fn` (Spec A + Plan A).
        if toks.contains(&WorkerKw) {
            self.out.text("worker ");
        }
        if toks.contains(&AsyncKw) {
            self.out.text("async ");
        }
        self.out.text("fn");
        if toks.contains(&Star) {
            self.out.text("*");
        }
        self.out.text(" ");
        if let Some(name) = first_ident_text(node) {
            self.out.text(&name);
        }
        self.type_params(node);
        self.params(node);
        if let Some(rt) = node.children().find(|c| c.kind() == RetType) {
            self.out.text(": ");
            if let Some(ty) = rt.children().find(|c| is_type_kind(c.kind())) {
                self.type_ann(ty);
            }
        }
        self.out.text(" ");
        if let Some(body) = node.children().find(|c| c.kind() == Block) {
            self.block(body);
        }
    }

    fn class_decl(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        // Emit leading `worker ` modifier when present (Spec B actor class).
        let has_worker = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == WorkerKw);
        if has_worker {
            self.out.text("worker ");
        }
        self.out.text("class ");
        let idents: Vec<String> = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == Ident)
            .map(|t| t.text().to_string())
            .collect();
        if let Some(name) = idents.first() {
            self.out.text(name);
        }
        self.type_params(node);
        // Emit `extends SuperClass` if present.
        // `extends` is a soft keyword parsed as Ident; idents = [ClassName, "extends", SuperName].
        if let Some(p) = idents.iter().position(|s| s == "extends") {
            if let Some(sup) = idents.get(p + 1) {
                self.out.text(" extends ");
                self.out.text(sup);
            }
        }
        // IFACE: canonical order is `extends Super implements A, B` (after extends,
        // before the body). The names live inside the `ImplementsClause` child.
        if let Some(im) = node.children().find(|c| c.kind() == ImplementsClause) {
            let names = iface_clause_names(im);
            if !names.is_empty() {
                self.out.text(" implements ");
                self.out.text(&names.join(", "));
            }
        }
        self.out.text(" {");
        self.out.newline();
        self.out.indent();

        let members: Vec<&ResolvedNode> = node
            .children()
            .filter(|c| matches!(c.kind(), FieldDecl | MethodDecl))
            .collect();
        let ordered: Vec<&ResolvedNode> = members
            .iter()
            .copied()
            .filter(|m| m.kind() == FieldDecl)
            .chain(members.iter().copied().filter(|m| m.kind() == MethodDecl))
            .collect();

        for (i, m) in ordered.iter().enumerate() {
            if i > 0 {
                let blank = self
                    .comments
                    .leading
                    .get(&m.text_range())
                    .and_then(|l| l.first())
                    .map(|c| c.blank_before)
                    .unwrap_or(false);
                if blank {
                    self.out.blank();
                }
            }
            self.emit_leading(m);
            self.member(m);
            self.emit_trailing(m);
        }

        self.out.dedent();
        self.out.text("}");
        self.out.newline();
    }

    /// IFACE §3: render `interface Name [extends A, B] { fn m(...)[: T] }`. The
    /// `extends` composition list is comma-joined; method requirements render one
    /// per line (a signature with NO body). An empty body collapses to `{\n}`.
    fn interface_decl(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("interface ");
        if let Some(name) = first_ident_text(node) {
            self.out.text(&name);
        }
        self.type_params(node);
        if let Some(ext) = node.children().find(|c| c.kind() == ExtendsList) {
            let names = iface_clause_names(ext);
            if !names.is_empty() {
                self.out.text(" extends ");
                self.out.text(&names.join(", "));
            }
        }
        self.out.text(" {");
        self.out.newline();
        self.out.indent();

        let reqs: Vec<&ResolvedNode> =
            node.children().filter(|c| c.kind() == MethodReq).collect();
        for (i, m) in reqs.iter().enumerate() {
            if i > 0 {
                let blank = self
                    .comments
                    .leading
                    .get(&m.text_range())
                    .and_then(|l| l.first())
                    .map(|c| c.blank_before)
                    .unwrap_or(false);
                if blank {
                    self.out.blank();
                }
            }
            self.emit_leading(m);
            self.method_req(m);
            self.emit_trailing(m);
        }

        self.out.dedent();
        self.out.text("}");
        self.out.newline();
    }

    /// IFACE §3: one interface method requirement — `fn name(params)[: ret]`, no body.
    fn method_req(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("fn ");
        if let Some(name) = first_ident_text(node) {
            self.out.text(&name);
        }
        self.params(node);
        if let Some(rt) = node.children().find(|c| c.kind() == RetType) {
            self.out.text(": ");
            if let Some(ty) = rt.children().find(|c| is_type_kind(c.kind())) {
                self.type_ann(ty);
            }
        }
        self.out.newline();
    }

    fn member(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
            FieldDecl => {
                if let Some(name) = first_ident_text(node) {
                    self.out.text(&name);
                }
                self.out.text(": ");
                // `name?: T` and `name: T?` BOTH normalize to `name: T?`. If the
                // field has the `?` marker token, append `?` to the printed type
                // (unless the type is already OptionalType).
                let has_marker = node
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .any(|t| t.kind() == Question);
                if let Some(ty) = node.children().find(|c| is_type_kind(c.kind())) {
                    self.type_ann(ty);
                    if has_marker && ty.kind() != OptionalType {
                        self.out.text("?");
                    }
                }
                if let Some(def) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.out.text(" = ");
                    self.expr(def);
                }
                self.out.newline();
            }
            MethodDecl => {
                let toks: Vec<SyntaxKind> = node
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .map(|t| t.kind())
                    .collect();
                // Canonical modifier order: `static? worker? async? fn`
                // (SP1 §3 + Spec A workers + Spec B worker class methods).
                if toks.contains(&StaticKw) {
                    self.out.text("static ");
                }
                if toks.contains(&WorkerKw) {
                    self.out.text("worker ");
                }
                if toks.contains(&AsyncKw) {
                    self.out.text("async ");
                }
                self.out.text("fn");
                if toks.contains(&Star) {
                    self.out.text("*");
                }
                self.out.text(" ");
                if let Some(name) = first_ident_text(node) {
                    self.out.text(&name);
                }
                self.params(node);
                if let Some(rt) = node.children().find(|c| c.kind() == RetType) {
                    self.out.text(": ");
                    if let Some(ty) = rt.children().find(|c| is_type_kind(c.kind())) {
                        self.type_ann(ty);
                    }
                }
                self.out.text(" ");
                if let Some(body) = node.children().find(|c| c.kind() == Block) {
                    self.block(body);
                }
            }
            _ => {
                self.out.text(node.text().to_string().trim());
                self.out.newline();
            }
        }
    }

    fn params(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("(");
        if let Some(list) = node.children().find(|c| c.kind() == ParamList) {
            let params: Vec<&ResolvedNode> =
                list.children().filter(|c| c.kind() == Param).collect();
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    self.out.text(", ");
                }
                if p.children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .any(|t| t.kind() == DotDotDot)
                {
                    self.out.text("...");
                }
                if let Some(name) = first_ident_text(p) {
                    self.out.text(&name);
                }
                if let Some(ty) = p.children().find(|c| is_type_kind(c.kind())) {
                    self.out.text(": ");
                    self.type_ann(ty);
                }
                if let Some(default) = p.children().find(|c| is_expr_kind(c.kind())) {
                    self.out.text(" = ");
                    self.expr(default);
                }
            }
        }
        self.out.text(")");
    }

    /// Emit a parenthesized param list given the ParamList node directly.
    fn params_from_list(&mut self, list: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("(");
        let params: Vec<&ResolvedNode> = list.children().filter(|c| c.kind() == Param).collect();
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                self.out.text(", ");
            }
            if p.children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == DotDotDot)
            {
                self.out.text("...");
            }
            if let Some(name) = first_ident_text(p) {
                self.out.text(&name);
            }
            if let Some(ty) = p.children().find(|c| is_type_kind(c.kind())) {
                self.out.text(": ");
                self.type_ann(ty);
            }
            if let Some(default) = p.children().find(|c| is_expr_kind(c.kind())) {
                self.out.text(" = ");
                self.expr(default);
            }
        }
        self.out.text(")");
    }

    fn for_stmt(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("for ");
        if node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == AwaitKw)
        {
            self.out.text("await ");
        }
        self.out.text("(");
        if let Some(var) = first_ident_text(node) {
            self.out.text(&var);
        }
        let kw = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), InKw | OfKw))
            .map(|t| t.text().to_string())
            .unwrap_or_else(|| "of".into());
        self.out.text(&format!(" {kw} "));
        if let Some(it) = node.children().find(|c| is_expr_kind(c.kind())) {
            self.expr(it);
        }
        self.out.text(") ");
        if let Some(b) = node.children().find(|c| c.kind() == Block) {
            self.block(b);
        }
    }

    fn enum_decl(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("enum ");
        if let Some(name) = first_ident_text(node) {
            self.out.text(&name);
        }
        self.type_params(node);
        self.out.text(" {");
        self.out.newline();
        self.out.indent();
        for v in node.children().filter(|c| c.kind() == EnumVariant) {
            self.emit_leading(v);
            if let Some(name) = first_ident_text(v) {
                self.out.text(&name);
            }
            if let Some(val) = v.children().find(|c| is_expr_kind(c.kind())) {
                self.out.text(" = ");
                self.expr(val);
            }
            // ADT: a payload variant renders its declared field list —
            // `Circle(radius: float)` (named) / `Pair(int, int)` (positional).
            let fields: Vec<ResolvedNode> = v
                .children()
                .filter(|c| c.kind() == VariantField)
                .cloned()
                .collect();
            if !fields.is_empty() {
                self.out.text("(");
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        self.out.text(", ");
                    }
                    // A named field has an `Ident` token before the type node.
                    if let Some(fname) = first_ident_text(field) {
                        self.out.text(&fname);
                        self.out.text(": ");
                    }
                    if let Some(ty) = field.children().find(|c| is_type_kind(c.kind())) {
                        self.type_ann(ty);
                    }
                }
                self.out.text(")");
            }
            self.out.text(",");
            self.out.newline();
            self.emit_trailing(v);
        }
        self.out.dedent();
        self.out.text("}");
        self.out.newline();
    }

    fn bind_pat(&mut self, node: &ResolvedNode, open: &str, close: &str) {
        use SyntaxKind::*;
        self.out.text(open);
        let items: Vec<&ResolvedNode> = node
            .children()
            .filter(|c| matches!(c.kind(), BindEntry | RestBind))
            .collect();
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                self.out.text(", ");
            }
            match it.kind() {
                BindEntry => self.out.text(&bind_entry_text(it)),
                RestBind => {
                    self.out.text("...");
                    if let Some(n) = first_ident_text(it) {
                        self.out.text(&n);
                    }
                }
                _ => {}
            }
        }
        self.out.text(close);
    }

    fn expr(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
            Literal => self.out.text(&self.literal_text(node)),
            NameRef => {
                let tok_text = node
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .find(|t| !t.kind().is_trivia())
                    .map(|t| t.text().to_string())
                    .unwrap_or_else(|| node.text().to_string().trim().to_string());
                self.out.text(&tok_text);
            }
            UnaryExpr => {
                let op = leading_op(node);
                self.out.text(&op);
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(e);
                }
            }
            BinaryExpr => {
                let kids: Vec<&ResolvedNode> =
                    node.children().filter(|c| is_expr_kind(c.kind())).collect();
                let op = binary_op(node);
                if let Some(l) = kids.first() {
                    self.expr(l);
                }
                self.out.text(&format!(" {op} "));
                if let Some(r) = kids.get(1) {
                    self.expr(r);
                }
            }
            ParenExpr => {
                self.out.text("(");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(e);
                }
                self.out.text(")");
            }
            CallExpr => {
                let kids: Vec<&ResolvedNode> = node.children().collect();
                if let Some(callee) = kids.iter().copied().find(|c| is_expr_kind(c.kind())) {
                    self.expr(callee);
                }
                if let Some(args) = kids.iter().copied().find(|c| c.kind() == ArgList) {
                    self.arg_list(args);
                }
            }
            MemberExpr => {
                if let Some(o) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(o);
                }
                self.out.text(".");
                self.out.text(&member_name(node));
            }
            OptMemberExpr => {
                if let Some(o) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(o);
                }
                self.out.text("?.");
                self.out.text(&member_name(node));
            }
            IndexExpr => {
                let kids: Vec<&ResolvedNode> =
                    node.children().filter(|c| is_expr_kind(c.kind())).collect();
                if let Some(o) = kids.first() {
                    self.expr(o);
                }
                self.out.text("[");
                if let Some(i) = kids.get(1) {
                    self.expr(i);
                }
                self.out.text("]");
            }
            ArrayExpr => self.comma_seq("[", "]", node),
            ObjectExpr => self.object_expr(node),
            MapExpr => self.map_expr(node),
            SpreadElem => {
                self.out.text("...");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(e);
                }
            }
            TemplateExpr => {
                // templates verbatim (interpolation preserved)
                self.out.text(node.text().to_string().trim())
            }
            TryExpr => self.unary_postfix(node, "?"),
            UnwrapExpr => self.unary_postfix(node, "!"),
            TernaryExpr => {
                let kids: Vec<&ResolvedNode> =
                    node.children().filter(|c| is_expr_kind(c.kind())).collect();
                if let Some(c) = kids.first() {
                    self.expr(c);
                }
                self.out.text(" ? ");
                if let Some(t) = kids.get(1) {
                    self.expr(t);
                }
                self.out.text(" : ");
                if let Some(e) = kids.get(2) {
                    self.expr(e);
                }
            }
            AwaitExpr => {
                self.out.text("await ");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(e);
                }
            }
            YieldExpr => {
                self.out.text("yield");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.out.text(" ");
                    self.expr(e);
                }
            }
            AssignExpr => {
                let kids: Vec<&ResolvedNode> =
                    node.children().filter(|c| is_expr_kind(c.kind())).collect();
                if let Some(t) = kids.first() {
                    self.expr(t);
                }
                self.out.text(&format!(" {} ", assign_op(node)));
                if let Some(v) = kids.get(1) {
                    self.expr(v);
                }
            }
            ArrowExpr => self.arrow_expr(node),
            MatchExpr => self.match_expr(node),
            RangeExpr => {
                let kids: Vec<&ResolvedNode> =
                    node.children().filter(|c| is_expr_kind(c.kind())).collect();
                if let Some(s) = kids.first() {
                    self.expr(s);
                }
                self.out.text(range_op(node));
                if let Some(e) = kids.get(1) {
                    self.expr(e);
                }
                // Optional trailing contextual `step <expr>` (the third expr child;
                // the `step` keyword itself is an Ident token, not an expr node).
                if let Some(step) = kids.get(2) {
                    self.out.text(" step ");
                    self.expr(step);
                }
            }
            _ => self.out.text(node.text().to_string().trim()),
        }
    }

    fn unary_postfix(&mut self, node: &ResolvedNode, op: &str) {
        if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
            self.expr(e);
        }
        self.out.text(op);
    }

    fn arg_list(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("(");
        let items: Vec<&ResolvedNode> = node
            .children()
            .filter(|c| is_expr_kind(c.kind()) || c.kind() == SpreadElem || c.kind() == NamedArg)
            .collect();
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                self.out.text(", ");
            }
            if it.kind() == NamedArg {
                // ADT §3.2: `name: value` — render the field name then the value expr.
                if let Some(name) = first_ident_text(it) {
                    self.out.text(&name);
                    self.out.text(": ");
                }
                if let Some(v) = it.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(v);
                }
            } else {
                self.expr(it);
            }
        }
        self.out.text(")");
    }

    fn comma_seq(&mut self, open: &str, close: &str, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text(open);
        let items: Vec<&ResolvedNode> = node
            .children()
            .filter(|c| is_expr_kind(c.kind()) || c.kind() == SpreadElem)
            .collect();
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                self.out.text(", ");
            }
            self.expr(it);
        }
        self.out.text(close);
    }

    fn object_expr(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("{");
        let items: Vec<&ResolvedNode> = node
            .children()
            .filter(|c| matches!(c.kind(), ObjectField | SpreadElem))
            .collect();
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                self.out.text(", ");
            }
            match it.kind() {
                ObjectField => {
                    self.out.text(&self.object_key(it));
                    self.out.text(": ");
                    if let Some(v) = it.children().find(|c| is_expr_kind(c.kind())) {
                        self.expr(v);
                    }
                }
                SpreadElem => self.expr(it),
                _ => {}
            }
        }
        self.out.text("}");
    }

    /// Format a `#{ keyExpr: valueExpr, … }` map literal; `#{}` for empty. Unlike
    /// `object_expr`, the key is an arbitrary EXPRESSION (not the object-key quoting
    /// logic), so each entry formats both children as expressions.
    fn map_expr(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        let entries: Vec<&ResolvedNode> = node
            .children()
            .filter(|c| c.kind() == MapEntry)
            .collect();
        if entries.is_empty() {
            self.out.text("#{}");
            return;
        }
        self.out.text("#{ ");
        for (i, ent) in entries.iter().enumerate() {
            if i > 0 {
                self.out.text(", ");
            }
            let mut kids = ent.children().filter(|c| is_expr_kind(c.kind()));
            if let Some(key) = kids.next() {
                self.expr(key);
            }
            self.out.text(": ");
            if let Some(value) = kids.next() {
                self.expr(value);
            }
        }
        self.out.text(" }");
    }

    fn literal_text(&self, node: &ResolvedNode) -> String {
        // numbers/bools/nil verbatim; strings re-quoted canonically (double quotes).
        let tok = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| !t.kind().is_trivia());
        match tok {
            Some(t) if t.kind() == SyntaxKind::Str => requote(t.text()),
            Some(t) => t.text().to_string(),
            None => node.text().to_string().trim().to_string(),
        }
    }

    fn object_key(&self, node: &ResolvedNode) -> String {
        use SyntaxKind::*;
        let key_tok = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), Ident | Str));
        match key_tok {
            Some(t) if t.kind() == Ident => t.text().to_string(),
            Some(t) => {
                let inner = unquote(t.text());
                if crate::token::is_ident_like(&inner) {
                    inner
                } else {
                    requote(t.text())
                }
            }
            None => String::new(),
        }
    }

    fn arrow_expr(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        if node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == AsyncKw)
        {
            self.out.text("async ");
        }
        // All arrow forms (bare `x =>`, parenthesized `(x,y) =>`) produce a ParamList.
        // A bare single-param ParamList has no LParen token — detect that to avoid
        // wrapping `x => e` in spurious parens.
        if let Some(list) = node.children().find(|c| c.kind() == ParamList) {
            let has_parens = list
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == LParen);
            if has_parens {
                self.params_from_list(list);
            } else {
                // bare single param: the ident is inside the single Param child
                if let Some(p) = list.children().find(|c| c.kind() == Param) {
                    if let Some(name) = first_ident_text(p) {
                        self.out.text(&name);
                    }
                }
            }
        }
        self.out.text(" => ");
        if let Some(body) = node.children().find(|c| c.kind() == Block) {
            self.block_inline(body);
        } else if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
            self.expr(e);
        }
    }

    fn match_expr(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        self.out.text("match ");
        if let Some(subj) = node.children().find(|c| is_expr_kind(c.kind())) {
            self.expr(subj);
        }
        self.out.text(" {");
        self.out.newline();
        self.out.indent();
        for arm in node.children().filter(|c| c.kind() == MatchArm) {
            self.emit_leading(arm);
            self.match_arm(arm);
            self.emit_trailing(arm);
        }
        self.out.dedent();
        self.out.text("}");
        // match is an expression; the enclosing stmt emits the trailing newline.
    }

    fn match_arm(&mut self, arm: &ResolvedNode) {
        use SyntaxKind::*;
        let pats: Vec<&ResolvedNode> = arm
            .children()
            .filter(|c| is_pattern_kind(c.kind()))
            .collect();
        for (i, p) in pats.iter().enumerate() {
            if i > 0 {
                self.out.text(" | ");
            }
            self.out.text(p.text().to_string().trim()); // patterns re-emit compactly via text
        }
        if let Some(g) = arm.children().find(|c| c.kind() == MatchGuard) {
            self.out.text(" if ");
            if let Some(e) = g.children().find(|c| is_expr_kind(c.kind())) {
                self.expr(e);
            }
        }
        self.out.text(" => ");
        if let Some(body) = arm.children().filter(|c| is_expr_kind(c.kind())).last() {
            self.expr(body);
        }
        self.out.text(",");
        self.out.newline();
    }

    /// TYPE §6 (Task 13): render a decl-level type-parameter list `<T, U>` (or with
    /// bounds, `<T, C: Container<T>>`) from the `TypeParams` child of a fn/class/enum/
    /// interface declaration. A no-op when the decl has no `TypeParams`. The list is
    /// retained in the CST (`TypeParams` → `TypeParam` → optional `TypeBound`), so this
    /// is purely a re-emit; dropping it (the carried-over bug) was lossy + non-idempotent.
    fn type_params(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        let Some(list) = node.children().find(|c| c.kind() == TypeParams) else {
            return;
        };
        let params: Vec<&ResolvedNode> =
            list.children().filter(|c| c.kind() == TypeParam).collect();
        if params.is_empty() {
            return;
        }
        self.out.text("<");
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                self.out.text(", ");
            }
            if let Some(name) = first_ident_text(p) {
                self.out.text(&name);
            }
            // Optional bound `: Type` (a `TypeBound` child holding the bound type).
            if let Some(bound) = p.children().find(|c| c.kind() == TypeBound) {
                if let Some(ty) = bound.children().find(|c| is_type_kind(c.kind())) {
                    self.out.text(": ");
                    self.type_ann(ty);
                }
            }
        }
        self.out.text(">");
    }

    fn type_ann(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
            NamedType => self.out.text(node_first_ident_or_text(node).trim()),
            GenericType => {
                if let Some(name) = first_ident_text(node) {
                    self.out.text(&name);
                }
                self.out.text("<");
                if let Some(args) = node.children().find(|c| c.kind() == TypeArgs) {
                    let ts: Vec<&ResolvedNode> =
                        args.children().filter(|c| is_type_kind(c.kind())).collect();
                    for (i, t) in ts.iter().enumerate() {
                        if i > 0 {
                            self.out.text(", ");
                        }
                        self.type_ann(t);
                    }
                }
                self.out.text(">");
            }
            // TYPE §6: a generic type-param reference renders as its bare name.
            ParamType => self.out.text(node_first_ident_or_text(node).trim()),
            // TYPE §6: `fn(A) -> B` — params are the type children except the LAST,
            // which is the return type. Canonical `fn(A, B) -> R` spacing.
            FnType => {
                let ts: Vec<&ResolvedNode> =
                    node.children().filter(|c| is_type_kind(c.kind())).collect();
                self.out.text("fn(");
                // Last type child is the return; the rest are params.
                let param_count = ts.len().saturating_sub(1);
                for (i, t) in ts.iter().take(param_count).enumerate() {
                    if i > 0 {
                        self.out.text(", ");
                    }
                    self.type_ann(t);
                }
                self.out.text(") -> ");
                if let Some(ret) = ts.last() {
                    self.type_ann(ret);
                }
            }
            OptionalType => {
                if let Some(inner) = node.children().find(|c| is_type_kind(c.kind())) {
                    self.type_ann(inner);
                }
                self.out.text("?");
            }
            UnionType => {
                let ts: Vec<&ResolvedNode> =
                    node.children().filter(|c| is_type_kind(c.kind())).collect();
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        self.out.text(" | ");
                    }
                    self.type_ann(t);
                }
            }
            TupleType => {
                self.out.text("[");
                let ts: Vec<&ResolvedNode> =
                    node.children().filter(|c| is_type_kind(c.kind())).collect();
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        self.out.text(", ");
                    }
                    self.type_ann(t);
                }
                self.out.text("]");
            }
            _ => self.out.text(node.text().to_string().trim()),
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

fn first_kw_text(node: &ResolvedNode) -> String {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| matches!(t.kind(), SyntaxKind::LetKw | SyntaxKind::ConstKw))
        .map(|t| t.text().to_string())
        .unwrap_or_else(|| "let".to_string())
}

fn bind_entry_text(node: &ResolvedNode) -> String {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.text().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_ident_text(node: &ResolvedNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// IFACE: the interface NAMES in an `ExtendsList`/`ImplementsClause`. The clause's
/// first ident token is the contextual introducer (`extends`/`implements`, lexed as
/// `Ident`); the remaining idents are the comma-separated interface names.
fn iface_clause_names(node: &ResolvedNode) -> Vec<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::Ident)
        .skip(1) // the introducing `extends`/`implements` keyword
        .map(|t| t.text().to_string())
        .collect()
}

fn is_pattern_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        WildcardPat | IdentPat | LiteralPat | RangePat | ArrayPat | ObjectPat | OrPat | VariantPat
    )
}

fn is_type_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        NamedType | GenericType | OptionalType | UnionType | TupleType
            // TYPE §6: a generic type-param reference and a `fn(A)->B` function type.
            | ParamType
            | FnType
    )
}

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
            | MemberExpr
            | IndexExpr
            | ArrowExpr
            | AssignExpr
            | ArrayExpr
            | ObjectExpr
            | MapExpr
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

fn is_binary_op(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        Plus | Minus
            | Star
            | Slash
            | Percent
            | StarStar
            | EqEq
            | BangEq
            | Lt
            | Le
            | Gt
            | Ge
            | AmpAmp
            | PipePipe
            | QuestionQuestion
            | InstanceofKw
            // Bitwise / shift / wrapping (NUM §3.2). `Pipe` is bitwise-OR here (in a
            // BinaryExpr); or-patterns/union types are different nodes, so this never
            // mis-renders them.
            | Amp
            | Caret
            | Shl
            | Shr
            | Pipe
            | PlusPercent
            | MinusPercent
            | StarPercent
    )
}

fn leading_op(node: &ResolvedNode) -> String {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| !t.kind().is_trivia())
        .map(|t| t.text().to_string())
        .unwrap_or_default()
}

fn binary_op(node: &ResolvedNode) -> String {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| is_binary_op(t.kind()))
        .map(|t| t.text().to_string())
        .unwrap_or_default()
}

fn assign_op(node: &ResolvedNode) -> String {
    use SyntaxKind::*;
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| matches!(t.kind(), Eq | PlusEq | MinusEq | StarEq | SlashEq))
        .map(|t| t.text().to_string())
        .unwrap_or_else(|| "=".into())
}

fn range_op(node: &ResolvedNode) -> &'static str {
    if node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::DotDotEq)
    {
        "..="
    } else {
        ".."
    }
}

fn normalize_import(node: &ResolvedNode) -> String {
    use SyntaxKind::*;
    let src = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == Str)
        .map(|t| t.text().to_string())
        .unwrap_or_default();
    if let Some(list) = node.children().find(|c| c.kind() == ImportList) {
        let names: Vec<String> = list
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == Ident)
            .map(|t| t.text().to_string())
            .collect();
        format!("import {{ {} }} from {}", names.join(", "), src)
    } else {
        // Namespace import: `import * as alias from "..."`.
        // Tokens in order: Star, Ident("as"), Ident(alias), Ident("from"), Str.
        // We want the ident that immediately follows the "as" ident.
        let idents: Vec<String> = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == Ident)
            .map(|t| t.text().to_string())
            .collect();
        // idents[0] = "as", idents[1] = alias, idents[2] = "from"
        let alias = idents.get(1).cloned().unwrap_or_default();
        format!("import * as {alias} from {src}")
    }
}

fn member_name(node: &ResolvedNode) -> String {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::Ident)
        .last()
        .map(|t| t.text().to_string())
        .unwrap_or_default()
}

fn node_first_ident_or_text(node: &ResolvedNode) -> String {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| !t.kind().is_trivia())
        .map(|t| t.text().to_string())
        .unwrap_or_else(|| node.text().to_string())
}

/// Strip surrounding quotes from a string literal's raw text.
fn unquote(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2 && (s.starts_with('"') || s.starts_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Canonical double-quoted string.
///
/// - Already double-quoted → return verbatim (idempotent; escape sequences
///   are already correct for double-quote context).
/// - Single-quoted → convert: unescape `\'` → `'`, escape bare `"` → `\"`,
///   leave all other escape sequences (`\\`, `\n`, `\t`, …) intact.
fn requote(raw: &str) -> String {
    let s = raw.trim();
    // Already double-quoted: pass through unchanged.
    if s.starts_with('"') {
        return s.to_string();
    }
    // Single-quoted: convert to double-quoted.
    let inner = if s.len() >= 2 && s.starts_with('\'') {
        &s[1..s.len() - 1]
    } else {
        s
    };
    let mut out = String::from("\"");
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                // Peek at the escape character.
                match chars.peek() {
                    Some('\'') => {
                        // \' in single-quoted becomes bare ' in double-quoted.
                        chars.next();
                        out.push('\'');
                    }
                    Some('"') => {
                        // \" is not a valid escape in single-quoted strings
                        // (the `"` was bare), but just in case, keep it.
                        chars.next();
                        out.push_str("\\\"");
                    }
                    _ => {
                        // All other escape sequences (\\, \n, \t, \r, \uXXXX, …)
                        // are identical in both quote styles — copy verbatim.
                        out.push('\\');
                        if let Some(&next) = chars.peek() {
                            chars.next();
                            out.push(next);
                        }
                    }
                }
            }
            '"' => out.push_str("\\\""), // bare " in single-quoted → escaped
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
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
        assert_eq!(fmt("a\n\nb\n"), "a\n\nb\n"); // one blank preserved
        assert_eq!(fmt("a\nb\n"), "a\nb\n"); // none stays none
    }

    #[test]
    fn formats_let_and_fn() {
        assert_eq!(fmt("let   x=1"), "let x = 1\n");
        assert_eq!(
            fmt("fn f(a,b){return a+b}"),
            "fn f(a, b) {\n  return a + b\n}\n"
        );
    }

    #[test]
    fn class_reorders_fields_before_methods_carrying_comments() {
        // Source: a method appears BEFORE a field, each with its own comment.
        // Canonical layout puts fields first — and each comment must travel.
        let src = "class C {\n  // the greet method\n  fn greet() { return 1 }\n  // the name field\n  name: string\n}\n";
        let out = fmt(src);
        let name_pos = out.find("name: string").expect("field present");
        let greet_pos = out.find("fn greet").expect("method present");
        assert!(
            name_pos < greet_pos,
            "fields must be reordered before methods:\n{out}"
        );
        let name_c = out
            .find("// the name field")
            .expect("field comment present");
        let greet_c = out
            .find("// the greet method")
            .expect("method comment present");
        assert!(
            name_c < name_pos && name_pos < greet_c,
            "each comment must travel with its member:\n{out}"
        );
    }

    #[test]
    fn formats_expressions() {
        assert_eq!(fmt("f( 1,2 )\n"), "f(1, 2)\n");
        assert_eq!(fmt("a . b [ c ]\n"), "a.b[c]\n");
        assert_eq!(fmt("a?.b\n"), "a?.b\n");
        assert_eq!(fmt("[ 1 ,2, 3 ]\n"), "[1, 2, 3]\n");
        assert_eq!(fmt("- x\n"), "-x\n");
        assert_eq!(fmt("a ?b: c\n"), "a ? b : c\n");
        assert_eq!(fmt("f()?\n"), "f()?\n");
        assert_eq!(fmt("g()!\n"), "g()!\n");
        assert_eq!(fmt("await  f()\n"), "await f()\n");
    }

    #[test]
    fn formats_statements() {
        assert_eq!(
            fmt("if(x){return 1}else{return 2}\n"),
            "if (x) {\n  return 1\n} else {\n  return 2\n}\n"
        );
        assert_eq!(fmt("while(x){ x=0 }\n"), "while (x) {\n  x = 0\n}\n");
        assert_eq!(
            fmt("for(i in 1..6){print(i)}\n"),
            "for (i in 1..6) {\n  print(i)\n}\n"
        );
        assert_eq!(fmt("x=5\n"), "x = 5\n");
        assert_eq!(fmt("break\n"), "break\n");
        assert_eq!(
            fmt(r#"import * as t from "std/task""#),
            "import * as t from \"std/task\"\n"
        );
        assert_eq!(fmt("enum E{A,B=2}\n"), "enum E {\n  A,\n  B = 2,\n}\n");
    }

    #[test]
    fn formats_functions_arrows_match() {
        assert_eq!(
            fmt("async fn f(){return 1}\n"),
            "async fn f() {\n  return 1\n}\n"
        );
        assert_eq!(fmt("fn* g(){yield 1}\n"), "fn* g() {\n  yield 1\n}\n");
        assert_eq!(
            fmt("fn add(a:number,b:number):number{return a+b}\n"),
            "fn add(a: number, b: number): number {\n  return a + b\n}\n"
        );
        assert_eq!(
            fmt("fn v(first,...rest){return rest}\n"),
            "fn v(first, ...rest) {\n  return rest\n}\n"
        );
        assert_eq!(fmt("let f=(x)=>x+1\n"), "let f = (x) => x + 1\n");
        assert_eq!(fmt("let g=x=>x+1\n"), "let g = x => x + 1\n");
        assert_eq!(fmt("let h=async (x)=>x\n"), "let h = async (x) => x\n");
        assert_eq!(
            fmt(r#"let r=match n{0=>"z",_=>"o"}"#),
            "let r = match n {\n  0 => \"z\",\n  _ => \"o\",\n}\n"
        );
    }

    #[test]
    fn formats_full_class() {
        // `name?: T` normalizes to `name: T?`; fields before methods; extends.
        let src = "class Dog extends Animal{ fn greet(){return 1} nickname?:string id:number=0 }\n";
        let out = fmt(src);
        assert!(out.contains("class Dog extends Animal {"), "{out}");
        assert!(
            out.contains("nickname: string?"),
            "name?: T -> name: T?:\n{out}"
        );
        assert!(out.contains("id: number = 0"), "{out}");
        let id = out.find("id:").unwrap();
        let greet = out.find("fn greet").unwrap();
        assert!(id < greet, "fields before methods:\n{out}");
    }

    #[test]
    fn formats_types_and_keys() {
        assert_eq!(
            fmt("let x: array< number > = []\n"),
            "let x: array<number> = []\n"
        );
        assert_eq!(
            fmt("let x: map<string,number> = m\n"),
            "let x: map<string, number> = m\n"
        );
        assert_eq!(
            fmt("let x: number|string = 1\n"),
            "let x: number | string = 1\n"
        );
        assert_eq!(fmt("let x: number ? = nil\n"), "let x: number? = nil\n");
        // non-identifier object keys quoted; identifier-like keys bare.
        assert_eq!(
            fmt(r#"let o = { "a-b": 1, c: 2 }"#),
            "let o = {\"a-b\": 1, c: 2}\n"
        );
    }

    #[test]
    fn formats_destructuring_let() {
        assert_eq!(
            fmt("let [a, b, ...rest]=xs\n"),
            "let [a, b, ...rest] = xs\n"
        );
        assert_eq!(
            fmt("let {a, b as local, ...rest}=obj\n"),
            "let {a, b as local, ...rest} = obj\n"
        );
    }

    #[test]
    fn idempotent_on_slice() {
        for src in [
            "1+2\n",
            "// hi\nx\n",
            "x // tail\n",
            "a\n\n\nb\n",
            "let x=1\n",
            "fn f(a,b){return a+b}\n",
            "class C {\n  // m\n  fn greet() { return 1 }\n  // f\n  name: string\n}\n",
        ] {
            let once = fmt(src);
            let twice = fmt(&once);
            assert_eq!(
                once, twice,
                "fmt not idempotent for {src:?}:\n{once}\n---\n{twice}"
            );
        }
    }

    // ---- Spec B Task 3: worker class + worker fn* formatter round-trip ----

    #[test]
    fn formats_worker_class_canonical() {
        // `worker` prefix is preserved and extra whitespace is normalized.
        let out = fmt("worker  class  Db{fn f(){return 1}}");
        assert_eq!(
            out,
            "worker class Db {\n  fn f() {\n    return 1\n  }\n}\n",
            "worker class should emit 'worker class Name {{...}}'"
        );
    }

    #[test]
    fn worker_class_fmt_is_idempotent() {
        // Formatting the canonical output a second time must be a no-op.
        let once = fmt("worker  class  Db{fn f(){return 1}}");
        let twice = fmt(&once);
        assert_eq!(once, twice, "worker class fmt not idempotent");
    }

    #[test]
    fn formats_worker_class_with_field_and_init() {
        // Full actor class: field, init method, query method — exercises class_decl
        // + member ordering (fields before methods) with worker prefix.
        let src = "worker class Cache { ttl: number = 60 fn init(t) { self.ttl = t } fn get(k) { return k } }";
        let out = fmt(src);
        assert!(out.starts_with("worker class Cache {"), "missing 'worker class': {out}");
        let ttl = out.find("ttl:").expect("field present");
        let init = out.find("fn init").expect("init method present");
        assert!(ttl < init, "field should appear before method in: {out}");
        // Idempotent.
        let twice = fmt(&out);
        assert_eq!(out, twice, "worker class with field+init not idempotent");
    }

    #[test]
    fn formats_worker_fn_star_canonical() {
        // `worker fn*` (free generator): both modifiers preserved and normalized.
        let out = fmt("worker fn*  g(){yield 1}");
        assert_eq!(
            out,
            "worker fn* g() {\n  yield 1\n}\n",
            "worker fn* should emit 'worker fn* name {{...}}'"
        );
    }

    #[test]
    fn worker_fn_star_fmt_is_idempotent() {
        let once = fmt("worker fn*  g(){yield 1}");
        let twice = fmt(&once);
        assert_eq!(once, twice, "worker fn* fmt not idempotent");
    }

    #[test]
    fn formats_worker_method_in_class() {
        // A `worker fn` method inside a plain class (Spec A pooled method).
        let out = fmt("class C { worker fn  run(x){return x} }");
        assert!(
            out.contains("worker fn run"),
            "worker method should emit 'worker fn run': {out}"
        );
        let twice = fmt(&out);
        assert_eq!(out, twice, "worker method in class not idempotent");
    }

    #[test]
    fn formats_static_worker_fn_star_in_class() {
        // Canonical modifier order: `static worker fn*`.
        let out = fmt("class C { static  worker  fn*  gen(x){yield x} }");
        assert!(
            out.contains("static worker fn* gen"),
            "canonical order should be 'static worker fn* gen': {out}"
        );
        let twice = fmt(&out);
        assert_eq!(out, twice, "static worker fn* in class not idempotent");
    }
}
