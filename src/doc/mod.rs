//! `ascript doc` — API documentation generated from the CST + the `///`
//! doc-comment convention (DX D1, spec §3).
//!
//! Entirely STATIC: like the LSP and the checker, this NEVER instantiates the
//! interpreter / runs code. It parses each file with the CST front-end
//! (`syntax::parse_to_tree`) and reads each declaration's `///` doc via the shared
//! extractor (`syntax::doc_comment::doc_comment_run`). The extractor itself does
//! NO re-tokenize / no second lex — it reinterprets the existing CST trivia. (The
//! CLI driver `run_doc` does build a `WorkspaceIndex` for the exported-name sets,
//! which parses each file, and `extract_module` re-parses it for the model; that
//! is fine for a static tool — the "no second parse" invariant is specifically
//! about the trivia extractor not re-lexing, not about the whole CLI pipeline.)
//!
//! The doc MODEL ([`DocModule`]/[`DocItem`]) captures the public-API decl kinds
//! (spec §3.2): functions (incl. `async`/`worker`/`static`/`fn*` modifiers),
//! classes (fields, `init`, methods, inheritance), enums (variants), and
//! constants. Signatures are rendered from CST node text AS WRITTEN. The
//! HTML/Markdown emitters live in submodules.

pub mod html;
pub mod markdown;

use crate::syntax::cst::ResolvedNode;
use crate::syntax::doc_comment::{doc_comment_run, module_doc, DocComment};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::{ident_text, is_static_method, is_worker_class, is_worker_fn};
use std::path::{Path, PathBuf};

/// One documented module = one `.as` file's public (or, with `--private`, full)
/// API surface.
#[derive(Debug, Clone)]
pub struct DocModule {
    /// The source file path.
    pub path: PathBuf,
    /// A display name for the module — the path RELATIVE to the input set's common
    /// root, sans extension (e.g. `a/util` for `…/a/util.as`). Disambiguates two
    /// same-stem files in different directories so neither the index link nor the
    /// output file collides (review finding 1).
    pub name: String,
    /// A filesystem-safe slug for the module, derived from `name` (e.g.
    /// `a_util`). The SINGLE source of truth for both the Markdown (`<slug>.md`)
    /// and HTML (`<slug>.html`) output filenames + the index link.
    pub slug: String,
    /// The `//!` module doc, if any.
    pub module_doc: Option<DocComment>,
    /// The documented top-level items, in source order.
    pub items: Vec<DocItem>,
}

/// One documented top-level declaration.
#[derive(Debug, Clone)]
pub struct DocItem {
    pub name: String,
    pub kind: ItemKind,
    /// `true` if the declaration is `export`ed (the public API). Non-exported items
    /// are only present when `--private` was requested.
    pub exported: bool,
    /// The rendered signature line (CST node text, as written).
    pub signature: String,
    /// The `///` doc attached to this declaration, if any.
    pub doc: Option<DocComment>,
    /// For a class: its fields. For an enum: its variants. Empty otherwise.
    pub members: Vec<DocMember>,
    /// For a class: its methods (each with its own doc). Empty otherwise.
    pub methods: Vec<DocItem>,
}

/// The kind of a documented item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Function,
    Class,
    Enum,
    Const,
    Let,
    /// A class method (rendered nested under its class).
    Method,
}

impl ItemKind {
    /// A short human label for the kind.
    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Function => "fn",
            ItemKind::Class => "class",
            ItemKind::Enum => "enum",
            ItemKind::Const => "const",
            ItemKind::Let => "let",
            ItemKind::Method => "method",
        }
    }
}

/// A class field or enum variant (a leaf member with an optional signature/doc).
#[derive(Debug, Clone)]
pub struct DocMember {
    pub name: String,
    /// The rendered member signature (e.g. `x: number = 0`, `Circle(r: float)`).
    pub signature: String,
    pub doc: Option<DocComment>,
}

/// Build a [`DocModule`] from a source file's text. STATIC: parses with the CST
/// front-end only. `name` is the module's display name (the caller passes the path
/// RELATIVE to the input set's common root, sans extension — see [`module_name`] /
/// [`relative_module_name`] — so two same-stem files in different directories stay
/// distinct). `exports` is the set of exported top-level names (from the workspace
/// `FileIndex.exports`); when `include_private` is false, only exported items are
/// kept.
pub fn extract_module(
    path: &Path,
    name: &str,
    text: &str,
    exports: &std::collections::HashSet<String>,
    include_private: bool,
) -> DocModule {
    let root = crate::syntax::parse_to_tree(text);
    let module_doc = module_doc(&root);
    let mut items = Vec::new();

    for child in root.children() {
        // Unwrap a leading `export <decl>`.
        let (decl, is_export): (ResolvedNode, bool) = if child.kind() == SyntaxKind::ExportStmt {
            match child.children().next() {
                Some(d) => (d.clone(), true),
                None => continue,
            }
        } else {
            (child.clone(), false)
        };

        let Some(item) = extract_item(&decl, is_export, exports) else {
            continue;
        };
        if item.exported || include_private {
            items.push(item);
        }
    }

    DocModule {
        path: path.to_path_buf(),
        name: name.to_string(),
        slug: slugify(name),
        module_doc,
        items,
    }
}

/// The fallback display name for a single module when no common-root context is
/// available (the file stem, or the full path string if none). Tests/single-file
/// callers use this; the CLI uses [`relative_module_name`] over the input set.
pub fn module_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// A module's display name RELATIVE to `root` (sans `.as` extension), with path
/// separators normalized to `/`. Falls back to [`module_name`] (the stem) if
/// `path` is not under `root`. This is what disambiguates two same-stem files in
/// different directories (`a/util`, `b/util`).
pub fn relative_module_name(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    // Drop the `.as` extension; keep the directory components.
    let mut parts: Vec<String> = Vec::new();
    for comp in rel.components() {
        if let std::path::Component::Normal(os) = comp {
            parts.push(os.to_string_lossy().to_string());
        }
    }
    if parts.is_empty() {
        return module_name(path);
    }
    // Strip a trailing `.as` from the last component.
    if let Some(last) = parts.last_mut() {
        if let Some(stem) = last.strip_suffix(".as") {
            *last = stem.to_string();
        }
    }
    parts.join("/")
}

/// Turn a module display name (possibly with `/` separators) into a filesystem-
/// safe slug — the SINGLE source of truth for both the `.md`/`.html` output
/// filenames and the index link (so md and html never diverge, and same-stem
/// modules get distinct files). Non-alphanumeric (besides `-`/`_`) → `_`.
pub fn slugify(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "module".to_string()
    } else {
        s
    }
}

/// Extract a [`DocItem`] from a top-level declaration node, or `None` if it is not
/// a documentable kind. `is_export` is the syntactic `export` marker; `exports`
/// confirms it (a name may be exported by a sibling `export` block elsewhere — we
/// honor both).
fn extract_item(
    decl: &ResolvedNode,
    is_export: bool,
    exports: &std::collections::HashSet<String>,
) -> Option<DocItem> {
    let name = ident_text(decl)?;
    let exported = is_export || exports.contains(&name);
    let doc = doc_comment_run(decl);
    let item = match decl.kind() {
        SyntaxKind::FnDecl => DocItem {
            signature: render_fn_signature(decl),
            name,
            kind: ItemKind::Function,
            exported,
            doc,
            members: Vec::new(),
            methods: Vec::new(),
        },
        SyntaxKind::ClassDecl => {
            let (fields, methods) = extract_class_body(decl);
            DocItem {
                signature: render_class_signature(decl),
                name,
                kind: ItemKind::Class,
                exported,
                doc,
                members: fields,
                methods,
            }
        }
        SyntaxKind::EnumDecl => DocItem {
            signature: format!("enum {name}"),
            members: extract_enum_variants(decl),
            name,
            kind: ItemKind::Enum,
            exported,
            doc,
            methods: Vec::new(),
        },
        SyntaxKind::LetStmt => {
            let is_const = decl
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == SyntaxKind::ConstKw);
            DocItem {
                signature: render_let_signature(decl, is_const),
                name,
                kind: if is_const {
                    ItemKind::Const
                } else {
                    ItemKind::Let
                },
                exported,
                doc,
                members: Vec::new(),
                methods: Vec::new(),
            }
        }
        _ => return None,
    };
    Some(item)
}

/// Render a function signature from its CST node — the modifiers, `fn`/`fn*`, the
/// name, the parameter list (as written), and the return type, with the body
/// excluded.
fn render_fn_signature(decl: &ResolvedNode) -> String {
    let mut sig = String::new();
    // Leading modifiers in source order: `async`/`worker`/`static`.
    for tok in decl.children_with_tokens().filter_map(|el| el.into_token()) {
        match tok.kind() {
            SyntaxKind::AsyncKw => sig.push_str("async "),
            SyntaxKind::WorkerKw => sig.push_str("worker "),
            SyntaxKind::StaticKw => sig.push_str("static "),
            _ => {}
        }
    }
    sig.push_str("fn");
    // A generator `fn*` carries a `*` token after `fn`.
    if has_star(decl) {
        sig.push('*');
    }
    if let Some(name) = ident_text(decl) {
        sig.push(' ');
        sig.push_str(&name);
    }
    if let Some(params) = decl.children().find(|c| c.kind() == SyntaxKind::ParamList) {
        sig.push_str(&normalize_ws(&params.text().to_string()));
    } else {
        sig.push_str("()");
    }
    append_ret_type(&mut sig, decl);
    sig
}

/// Append a `RetType` node (`: T`) tightly to `sig` — the `:` is the separator, so
/// no leading space (the param list already closed with `)`).
fn append_ret_type(sig: &mut String, decl: &ResolvedNode) {
    if let Some(ret) = decl.children().find(|c| c.kind() == SyntaxKind::RetType) {
        sig.push_str(normalize_ws(&ret.text().to_string()).trim());
    }
}

/// True if the decl carries a `*` token (a generator `fn*`).
fn has_star(decl: &ResolvedNode) -> bool {
    decl.children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::Star)
}

/// Render a class signature: `worker?` `class Name` `extends Base` `implements …`.
fn render_class_signature(decl: &ResolvedNode) -> String {
    let mut sig = String::new();
    if is_worker_class(decl) {
        sig.push_str("worker ");
    }
    sig.push_str("class");
    if let Some(name) = ident_text(decl) {
        sig.push(' ');
        sig.push_str(&name);
    }
    // Collect the header tokens (everything before the class body `{`), skipping
    // the keywords and the class name; reconstruct `extends Base` / `implements …`.
    let mut header: Vec<String> = Vec::new();
    let mut seen_name = false;
    for tok in decl.children_with_tokens().filter_map(|el| el.into_token()) {
        match tok.kind() {
            SyntaxKind::LBrace => break,
            SyntaxKind::Ident if !seen_name => {
                seen_name = true; // the class name, already emitted
            }
            SyntaxKind::Ident => header.push(tok.text().to_string()),
            SyntaxKind::Comma => header.push(",".to_string()),
            _ => {}
        }
    }
    if !header.is_empty() {
        sig.push(' ');
        sig.push_str(&join_header(&header));
    }
    sig
}

/// Join class-header bareword tokens, spacing words but tightening commas.
fn join_header(words: &[String]) -> String {
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if w == "," {
            out.push(',');
        } else {
            if i > 0 && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push_str(w);
        }
    }
    out
}

/// Render a `let`/`const` signature: `const Name: Type` (the annotation, if any),
/// excluding the initializer.
fn render_let_signature(decl: &ResolvedNode, is_const: bool) -> String {
    let kw = if is_const { "const" } else { "let" };
    let mut sig = kw.to_string();
    if let Some(name) = ident_text(decl) {
        sig.push(' ');
        sig.push_str(&name);
    }
    // A type annotation node, if present.
    if let Some(ty) = decl.children().find(|c| is_type_node(c.kind())) {
        sig.push_str(": ");
        sig.push_str(normalize_ws(&ty.text().to_string()).trim());
    }
    sig
}

/// Extract a class body into `(fields, methods)`. Fields are leaf members; methods
/// are nested [`DocItem`]s with their own `///` doc.
fn extract_class_body(decl: &ResolvedNode) -> (Vec<DocMember>, Vec<DocItem>) {
    let mut fields = Vec::new();
    let mut methods = Vec::new();
    for member in decl.children() {
        match member.kind() {
            SyntaxKind::FieldDecl => {
                if let Some(name) = ident_text(member) {
                    fields.push(DocMember {
                        signature: node_text_no_trivia(member),
                        name,
                        doc: doc_comment_run(member),
                    });
                }
            }
            SyntaxKind::MethodDecl => {
                if let Some(name) = ident_text(member) {
                    methods.push(DocItem {
                        signature: render_method_signature(member),
                        name,
                        kind: ItemKind::Method,
                        exported: true,
                        doc: doc_comment_run(member),
                        members: Vec::new(),
                        methods: Vec::new(),
                    });
                }
            }
            _ => {}
        }
    }
    (fields, methods)
}

/// Render a method signature (same as a fn, but the `static` modifier is honored).
fn render_method_signature(decl: &ResolvedNode) -> String {
    let mut sig = String::new();
    if is_static_method(decl) {
        sig.push_str("static ");
    }
    if decl
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::AsyncKw)
    {
        sig.push_str("async ");
    }
    if is_worker_fn(decl) {
        sig.push_str("worker ");
    }
    sig.push_str("fn");
    if has_star(decl) {
        sig.push('*');
    }
    if let Some(name) = ident_text(decl) {
        sig.push(' ');
        sig.push_str(&name);
    }
    if let Some(params) = decl.children().find(|c| c.kind() == SyntaxKind::ParamList) {
        sig.push_str(&normalize_ws(&params.text().to_string()));
    } else {
        sig.push_str("()");
    }
    append_ret_type(&mut sig, decl);
    sig
}

/// Extract enum variants as leaf members, each with its rendered payload signature
/// and its own `///` doc.
fn extract_enum_variants(decl: &ResolvedNode) -> Vec<DocMember> {
    let mut out = Vec::new();
    for variant in decl
        .children()
        .filter(|c| c.kind() == SyntaxKind::EnumVariant)
    {
        let Some(name) = ident_text(variant) else {
            continue;
        };
        out.push(DocMember {
            signature: node_text_no_trivia(variant),
            name,
            doc: doc_comment_run(variant),
        });
    }
    out
}

/// True if `kind` is a type-annotation CST node.
fn is_type_node(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        NamedType | GenericType | OptionalType | UnionType | TupleType | FnType
    )
}

/// The source text of a node with all trivia (doc/comments/whitespace/newlines)
/// excluded — the bare declaration tokens, spaced and whitespace-collapsed. Used
/// for field/variant signatures so a member's own leading `///` doc does not leak
/// into its rendered signature.
fn node_text_no_trivia(node: &ResolvedNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    for el in node.descendants_with_tokens() {
        if let Some(tok) = el.as_token() {
            if !tok.kind().is_trivia() {
                parts.push(tok.text().to_string());
            }
        }
    }
    normalize_ws(&parts.join(" ")).trim().to_string()
}

/// Collapse internal whitespace runs (incl. newlines) to single spaces and trim.
/// Signature text comes from CST node `.text()` which preserves source whitespace;
/// this canonicalizes a multi-line param list into one readable line.
fn normalize_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    let mut started = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if started && !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
            started = true;
        }
    }
    // Tighten spacing introduced by the token-join collapse: no space inside
    // parens, before a comma/colon, or before an opening paren (a call/variant
    // payload like `Circle (r)` → `Circle(r)`).
    out.replace("( ", "(")
        .replace(" )", ")")
        .replace(" ,", ",")
        .replace(" :", ":")
        .replace(" (", "(")
}

/// Public symbols in `module` that lack a `///` doc (for `ascript doc --check`).
/// A class/enum is checked at the top level; its members are NOT required to be
/// documented in v1 (the public-API gate is the exported top-level declaration).
/// A bare `///` with an EMPTY body counts as UNDOCUMENTED (a stray `///` must not
/// satisfy the CI gate) — aligned with the LSP hover's same empty-body guard.
pub fn undocumented_public(module: &DocModule) -> Vec<String> {
    module
        .items
        .iter()
        .filter(|i| i.exported && !has_doc_body(&i.doc))
        .map(|i| format!("{} {}", i.kind.label(), i.name))
        .collect()
}

/// True if `doc` is present AND its body is non-empty after trimming (an empty
/// `///` does not count as documented).
fn has_doc_body(doc: &Option<DocComment>) -> bool {
    doc.as_ref().map(|d| !d.body.trim().is_empty()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn exports(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn extract(text: &str, exp: &[&str], private: bool) -> DocModule {
        let path = Path::new("m.as");
        extract_module(path, &module_name(path), text, &exports(exp), private)
    }

    #[test]
    fn documents_exported_function_with_signature_and_doc() {
        let src = "/// Adds two numbers.\nexport fn add(a: number, b: number): number { return a + b }\n";
        let m = extract(src, &["add"], false);
        assert_eq!(m.items.len(), 1);
        let f = &m.items[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.kind, ItemKind::Function);
        assert_eq!(f.signature, "fn add(a: number, b: number): number");
        assert_eq!(f.doc.as_ref().unwrap().summary, "Adds two numbers.");
    }

    #[test]
    fn private_excluded_by_default_included_with_flag() {
        let src = "export fn pub_fn() {}\nfn priv_fn() {}\n";
        let public = extract(src, &["pub_fn"], false);
        assert_eq!(public.items.len(), 1);
        assert_eq!(public.items[0].name, "pub_fn");
        let all = extract(src, &["pub_fn"], true);
        assert_eq!(all.items.len(), 2);
        let priv_item = all.items.iter().find(|i| i.name == "priv_fn").unwrap();
        assert!(!priv_item.exported);
    }

    #[test]
    fn captures_fn_modifiers() {
        let src = "export async fn fetch() {}\nexport worker fn render(s) {}\nexport fn* gen() {}\n";
        let m = extract(src, &["fetch", "render", "gen"], false);
        let fetch = m.items.iter().find(|i| i.name == "fetch").unwrap();
        assert!(
            fetch.signature.starts_with("async fn"),
            "{}",
            fetch.signature
        );
        let render = m.items.iter().find(|i| i.name == "render").unwrap();
        assert!(
            render.signature.starts_with("worker fn"),
            "{}",
            render.signature
        );
        let gen = m.items.iter().find(|i| i.name == "gen").unwrap();
        assert!(gen.signature.starts_with("fn*"), "{}", gen.signature);
    }

    #[test]
    fn documents_class_fields_and_methods() {
        let src = "/// A doggo.\nexport class Dog extends Animal {\n  /// the name\n  name: string\n  age: number = 0\n  /// build one\n  fn init(name) { self.name = name }\n  static fn make(): Dog { return Dog() }\n}\n";
        let m = extract(src, &["Dog"], false);
        let c = &m.items[0];
        assert_eq!(c.kind, ItemKind::Class);
        assert!(c.signature.contains("class Dog"), "{}", c.signature);
        assert!(c.signature.contains("extends Animal"), "{}", c.signature);
        // Fields.
        assert_eq!(c.members.len(), 2);
        let name = c.members.iter().find(|f| f.name == "name").unwrap();
        assert_eq!(name.doc.as_ref().unwrap().summary, "the name");
        assert!(
            name.signature.contains("name: string"),
            "{}",
            name.signature
        );
        // Methods.
        let init = c.methods.iter().find(|me| me.name == "init").unwrap();
        assert_eq!(init.doc.as_ref().unwrap().summary, "build one");
        let make = c.methods.iter().find(|me| me.name == "make").unwrap();
        assert!(make.signature.starts_with("static fn"), "{}", make.signature);
    }

    #[test]
    fn documents_enum_variants() {
        let src = "/// A shape.\nexport enum Shape {\n  Circle(r: float),\n  Pair(int, int),\n  Point,\n}\n";
        let m = extract(src, &["Shape"], false);
        let e = &m.items[0];
        assert_eq!(e.kind, ItemKind::Enum);
        assert_eq!(e.members.len(), 3);
        let circle = e.members.iter().find(|v| v.name == "Circle").unwrap();
        assert!(
            circle.signature.contains("Circle(r: float)"),
            "{}",
            circle.signature
        );
    }

    #[test]
    fn documents_const() {
        let src = "/// The answer.\nexport const ANSWER: number = 42\n";
        let m = extract(src, &["ANSWER"], false);
        let c = &m.items[0];
        assert_eq!(c.kind, ItemKind::Const);
        assert!(c.signature.contains("const ANSWER"), "{}", c.signature);
        assert!(c.signature.contains(": number"), "{}", c.signature);
    }

    #[test]
    fn module_doc_captured() {
        let src = "//! The module.\nexport fn f() {}\n";
        let m = extract(src, &["f"], false);
        assert_eq!(m.module_doc.as_ref().unwrap().summary, "The module.");
    }

    #[test]
    fn undocumented_public_lists_missing() {
        let src = "/// documented\nexport fn a() {}\nexport fn b() {}\n";
        let m = extract(src, &["a", "b"], false);
        let missing = undocumented_public(&m);
        assert_eq!(missing, vec!["fn b".to_string()]);
    }

    #[test]
    fn never_runs_interpreter() {
        // A program with side effects that WOULD print if executed — extraction must
        // produce a model without running anything.
        let src = "/// doc\nexport fn boom() { print(\"SHOULD NOT RUN\") }\n";
        let m = extract(src, &["boom"], false);
        assert_eq!(m.items.len(), 1);
    }

    /// Review finding 6: a bare `///` with an empty body must count as
    /// UNDOCUMENTED for `--check` (a stray `///` cannot satisfy the CI gate).
    #[test]
    fn empty_doc_body_is_undocumented() {
        let src = "///\nexport fn a() {}\n";
        let m = extract(src, &["a"], false);
        // The doc is present (a `///` line) but its body is empty.
        assert!(m.items[0].doc.is_some(), "the empty /// is still parsed");
        let missing = undocumented_public(&m);
        assert_eq!(
            missing,
            vec!["fn a".to_string()],
            "empty-body /// must count as undocumented"
        );
    }

    /// Review finding 1: the slug helper disambiguates same-stem files in
    /// different directories, and is a pure function of the root-relative name.
    #[test]
    fn relative_module_name_disambiguates_same_stem() {
        let root = Path::new("/proj");
        let a = relative_module_name(Path::new("/proj/a/util.as"), root);
        let b = relative_module_name(Path::new("/proj/b/util.as"), root);
        assert_eq!(a, "a/util");
        assert_eq!(b, "b/util");
        // And their slugs are distinct + filesystem-safe.
        assert_eq!(slugify(&a), "a_util");
        assert_eq!(slugify(&b), "b_util");
        assert_ne!(slugify(&a), slugify(&b));
    }

    /// The slug is the single source of truth carried on the model.
    #[test]
    fn module_slug_set_from_name() {
        let path = Path::new("/proj/a/util.as");
        let name = relative_module_name(path, Path::new("/proj"));
        let m = extract_module(path, &name, "/// d\nexport fn f() {}\n", &exports(&["f"]), false);
        assert_eq!(m.name, "a/util");
        assert_eq!(m.slug, "a_util");
    }
}
