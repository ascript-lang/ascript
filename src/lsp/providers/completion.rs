//! `textDocument/completion` (and `completionItem/resolve`) over the cached
//! [`SemanticModel`].
//!
//! Scope-aware completion driven by the resolver result rather than raw lexing:
//! keywords + builtins, in-scope user bindings, member access on a class / enum /
//! module namespace (offering its members or exports), import-path string context,
//! curated control-flow snippets, and auto-import items that add the matching
//! `import … from "std/…"` edit for a known stdlib export. `resolve_completion`
//! lazily fills in detail/documentation for builtins and keywords.
//!
//! In-scope bindings are FRAME-PRECISE (Task 13): a local/param/inner-fn binding is
//! offered only when its owning frame encloses the cursor (the cursor frame + the
//! parent-frame / upvalue chain), reusing navigation's frame model; module-globals,
//! builtins, and keywords are in scope everywhere and always offered. Member access
//! on a TYPED VALUE receiver resolves the receiver's type through `crate::check::infer`
//! and offers that class's fields + methods.

use crate::lsp::model::SemanticModel;
use crate::syntax::resolve::types::BindingKind;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, InsertTextFormat, MarkupContent, MarkupKind,
    Position, Range, TextEdit,
};

/// The AScript keywords offered as completions (KEYWORD kind). Mirrors the lexer's
/// keyword table plus `match`.
const KEYWORDS: &[&str] = &[
    "let", "const", "fn", "return", "if", "else", "while", "for", "of", "in", "match", "async",
    "await", "yield", "class", "enum", "import", "export", "nil", "true", "false", "break",
    "continue", "worker", "defer",
];

/// The global builtins offered as completions (FUNCTION kind). Mirrors `builtin_doc`.
const BUILTINS: &[&str] = &[
    "print", "len", "type", "assert", "range", "Ok", "Err", "recover", "test", "exit",
];

/// The built-in primitive TYPE names offered as completions (TYPE_PARAMETER kind),
/// so `int`/`float`/`number` (NUM §5) and the other scalars surface where a type
/// annotation is being written. Offered in the always-on baseline (the completion
/// context is parser-free and does not yet distinguish type vs value position; an
/// offered type name in value position is a benign, non-misleading suggestion).
const TYPE_NAMES: &[&str] = &[
    "int", "float", "number", "string", "bool", "nil", "any", "object", "bytes", "regex", "error",
];

/// The stdlib module paths offered when completing an `import ... from "..."`
/// string and scanned for auto-import candidates. This IS the canonical,
/// feature-independent `stdlib::STD_MODULES` list (a direct reuse, so the
/// completion surface can never drift from the language's module set) — stable
/// regardless of which cargo features are enabled at build time, so editors see
/// every documented module path. A module whose exports are feature-gated out of
/// this particular build simply yields no auto-import items
/// (`std_module_exports` returns `None` and the caller skips it).
use crate::stdlib::STD_MODULES as STD_MODULE_PATHS;

/// A baseline completion item (keyword or builtin).
fn item(label: &str, kind: CompletionItemKind) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        ..CompletionItem::default()
    }
}

/// ADT: a completion item for an enum variant offered after `Enum.`. A unit variant
/// is a plain `ENUM_MEMBER` insert (`Point`); a payload variant gets a `detail`
/// showing its signature (`(radius: float)`) and a SNIPPET insert with one tab-stop
/// per field placeholder (`Circle(${1:radius})`), so the call form is pre-filled.
fn variant_completion_item(
    variant: &str,
    info: &crate::check::infer::table::EnumInfo,
    table: &crate::check::infer::table::Table,
) -> CompletionItem {
    let mut ci = item(variant, CompletionItemKind::ENUM_MEMBER);
    let fields = info.fields_of(variant).unwrap_or(&[]);
    if fields.is_empty() {
        // Unit/scalar variant — a bare reference, no payload.
        return ci;
    }
    // Signature detail, e.g. `(radius: float)` or `(int, int)`.
    let sig: Vec<String> = fields
        .iter()
        .map(|f| match &f.name {
            Some(n) => format!("{n}: {}", f.ty.display(table)),
            None => f.ty.display(table),
        })
        .collect();
    ci.detail = Some(format!("({})", sig.join(", ")));
    // Snippet insert with a placeholder per field. A MULTI-field named variant MUST
    // be constructed with named args — the engine rejects a positional call
    // (`interp.rs`: `is_named() && fields.len() > 1` → "requires named fields"), so the
    // snippet emits the `name: <type>` call form, otherwise tab-completing it would
    // lead straight into a runtime error. Single-field named (`Circle(radius)`) and
    // positional variants accept positional args, so they keep the bare placeholder
    // (field name as label for named, type for positional).
    let named_call = fields.len() > 1 && fields.iter().all(|f| f.name.is_some());
    let placeholders: Vec<String> = fields
        .iter()
        .enumerate()
        .map(|(i, f)| match &f.name {
            Some(n) if named_call => format!("{n}: ${{{}:{}}}", i + 1, f.ty.display(table)),
            Some(n) => format!("${{{}:{}}}", i + 1, n),
            None => format!("${{{}:{}}}", i + 1, f.ty.display(table)),
        })
        .collect();
    ci.insert_text = Some(format!("{variant}({})", placeholders.join(", ")));
    ci.insert_text_format = Some(InsertTextFormat::SNIPPET);
    ci
}

/// The always-offered baseline completions: every keyword + every global builtin.
fn baseline_completions() -> Vec<CompletionItem> {
    let mut out = Vec::with_capacity(KEYWORDS.len() + BUILTINS.len() + TYPE_NAMES.len());
    for kw in KEYWORDS {
        out.push(item(kw, CompletionItemKind::KEYWORD));
    }
    for b in BUILTINS {
        out.push(item(b, CompletionItemKind::FUNCTION));
    }
    for t in TYPE_NAMES {
        out.push(item(t, CompletionItemKind::TYPE_PARAMETER));
    }
    out
}

/// In-scope user bindings as completion items, FRAME-PRECISE (Task 13). A binding is
/// offered iff it is live at the cursor's frame:
/// - a MODULE-SCOPE user-global (`is_global`) is in scope everywhere → always offered;
/// - a local/param/inner-fn binding is offered ONLY when its OWNING frame ENCLOSES the
///   cursor (the cursor frame's locals/params + the parent-frame / upvalue chain) — a
///   sibling-scope name that does not enclose the cursor is NOT over-offered.
///
/// "Which frames enclose the cursor" reuses navigation's frame model
/// (`frame_chain_at` + `binding_live_at`), the SAME structures `definition`/
/// `document-highlight` walk — NOT a second resolver. The binding KIND maps to an icon.
fn binding_completions(model: &SemanticModel, char_offset: usize) -> Vec<CompletionItem> {
    use crate::lsp::providers::navigation::{binding_live_at, frame_chain_at};
    // Frame ranges (cstree `TextRange`) are BYTE-based; the completion provider works
    // in CHAR offsets — convert before consulting the frame model.
    let byte_offset = crate::lsp::convert::char_to_byte(&model.text, char_offset);
    let chain = frame_chain_at(model, byte_offset);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for b in &model.resolved.bindings {
        // A non-global binding must have its owning frame on the cursor's frame chain.
        if !b.is_global && !binding_live_at(model, b.decl_range, &chain) {
            continue;
        }
        if !seen.insert(b.name.clone()) {
            continue;
        }
        let kind = match b.kind {
            BindingKind::Fn => CompletionItemKind::FUNCTION,
            BindingKind::Class => CompletionItemKind::CLASS,
            BindingKind::Enum => CompletionItemKind::ENUM,
            BindingKind::Const => CompletionItemKind::CONSTANT,
            BindingKind::Param => CompletionItemKind::VARIABLE,
            BindingKind::Import => CompletionItemKind::MODULE,
            _ => CompletionItemKind::VARIABLE,
        };
        out.push(item(&b.name, kind));
    }
    out
}

/// Curated control-flow snippets. Each is a SNIPPET-format item with `${n:…}`
/// tab-stops and a `$0` final cursor.
fn snippet_completions() -> Vec<CompletionItem> {
    const SNIPPETS: &[(&str, &str)] = &[
        ("fn", "fn ${1:name}(${2:params}) {\n  $0\n}"),
        ("if", "if (${1:cond}) {\n  $0\n}"),
        ("for", "for (${1:item} of ${2:iter}) {\n  $0\n}"),
        ("while", "while (${1:cond}) {\n  $0\n}"),
        ("defer", "defer ${1:resource}.close()"),
        ("match", "match ${1:subject} {\n  ${2:pattern} => $0,\n}"),
        ("class", "class ${1:Name} {\n  $0\n}"),
    ];
    SNIPPETS
        .iter()
        .map(|(label, body)| CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::SNIPPET),
            insert_text: Some(body.to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..CompletionItem::default()
        })
        .collect()
}

/// Lazily-built, process-scoped cache of ALL auto-import completion items across
/// every std module. Built once on first call and shared across all subsequent
/// requests — avoids re-iterating ~60 modules × N exports on every keystroke.
///
/// Each entry carries:
/// - label    = export name
/// - kind     = FUNCTION or CONSTANT (HandleMethod entries are skipped)
/// - detail   = `"auto-import from <module>"`
/// - sort_text = `"zz<name>"` — sorts AFTER in-scope bindings and builtins
/// - additional_text_edits = the import insertion at line 0
///
/// The caller (`auto_import_candidates`) filters out already-imported names by
/// iterating this slice — a cheap O(n) scan over a shared `&'static` allocation.
pub fn auto_import_entries() -> &'static [CompletionItem] {
    use crate::check::std_sigs::{module_members, MemberKind};
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<CompletionItem>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut out = Vec::new();
        for path in STD_MODULE_PATHS {
            let Some(members) = module_members(path) else { continue; };
            for &(name, ref kind) in members {
                let ck = match kind {
                    MemberKind::Fn => CompletionItemKind::FUNCTION,
                    MemberKind::Const(_) => CompletionItemKind::CONSTANT,
                    MemberKind::HandleMethod => continue,
                };
                let mut ci = item(name, ck);
                ci.detail = Some(format!("auto-import from {path}"));
                ci.sort_text = Some(format!("zz{name}"));
                ci.additional_text_edits = Some(vec![TextEdit {
                    range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                    new_text: format!("import {{ {name} }} from \"{path}\"\n"),
                }]);
                out.push(ci);
            }
        }
        out
    })
}

/// Auto-import candidates: every std export NOT already imported, each carrying an
/// `additionalTextEdits` that inserts an `import { name } from "<path>"` line at the
/// top of the file. Reuses the `STD_MODULE_PATHS` set the import-path-string context
/// offers; the LSP never stores a `Value` (mirrors the namespace-member branch — it
/// takes ONLY the export name).
fn auto_import_candidates(model: &SemanticModel) -> Vec<CompletionItem> {
    // Names already imported (named or namespace) — never re-offer these.
    let imported: std::collections::HashSet<String> = model
        .resolved
        .bindings
        .iter()
        .filter(|b| matches!(b.kind, BindingKind::Import))
        .map(|b| b.name.clone())
        .collect();

    auto_import_entries()
        .iter()
        .filter(|ci| !imported.contains(&ci.label))
        .cloned()
        .collect()
}

/// Fill `documentation` from the stdlib signature table for a completion item that
/// carries `data: { module, name }`. Returns `true` if the item was handled (whether
/// or not a doc was found). This is the fast, model-free path used in
/// `completionItem/resolve` before falling back to the SemanticModel-based resolver.
pub fn resolve_completion_static(item: &mut CompletionItem) -> bool {
    use crate::check::std_sigs::std_sig;
    let Some(data) = item.data.as_ref() else { return false; };
    let Some(module) = data.get("module").and_then(|v| v.as_str()) else { return false; };
    let Some(name) = data.get("name").and_then(|v| v.as_str()) else { return false; };
    if let Some(sig) = std_sig(module, name) {
        if !sig.doc.is_empty() {
            item.documentation = Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: sig.doc.to_string(),
            }));
        }
    }
    true
}

/// Completions at char `offset` in the model's text. Pure and robust: never panics,
/// and always returns at least the baseline (keywords + builtins) even on partial or
/// syntactically broken input (completion is requested mid-edit).
///
/// Context detection is done by simple, parser-free scanning of the raw text around
/// the cursor, so it works on documents that do not yet parse:
/// - inside an `import ... from "..."` / `'...'` string → stdlib module paths;
/// - right after `<ident>.` (with an optional partial identifier after the dot, e.g.
///   `math.sq`) where `<ident>` is a `import * as <ident>` namespace of a known std
///   module → that module's exports (with `filter_text` = the typed prefix so the
///   client filters correctly);
/// - inside a plain string body or comment → NO items (C8 suppression).
pub fn completions(model: &SemanticModel, offset: usize) -> Vec<CompletionItem> {
    let text = &model.text;
    let chars: Vec<char> = text.chars().collect();
    let offset = offset.min(chars.len());

    // Context 1: inside an import-from string literal → offer module paths.
    // This is checked BEFORE the C8 string-suppression so import paths still complete.
    if in_import_path_string(&chars, offset) {
        return STD_MODULE_PATHS
            .iter()
            .map(|p| item(p, CompletionItemKind::MODULE))
            .collect();
    }

    // C8: suppress inside plain string bodies and comments.
    // Template INTERPOLATION interiors land on ordinary tokens (Ident etc.) and are
    // NOT suppressed — only the TemplateStart/Middle/End/TemplateStr literal parts are.
    if token_kind_at_cursor(&model.tokens, offset, &chars).is_some_and(suppressed_token_kind) {
        return Vec::new();
    }

    // Context 2: member access `<ident>.<prefix>` where ident is a namespace import.
    // C1: also fires when the cursor is after a partial identifier (`math.sq|`).
    if let Some((alias, prefix)) = member_access_alias(&chars, offset) {
        if let Some(module) = namespace_import_module(text, &alias) {
            // Use the curated member table when available (provides real kinds,
            // detail, and data for lazy doc resolution). Fall back to the runtime
            // exports table for modules not yet covered by std_sigs.
            use crate::check::std_sigs::{module_members, std_sig, render_sig_detail, MemberKind};
            if let Some(members) = module_members(&module) {
                if !members.is_empty() {
                    return members
                        .iter()
                        .filter_map(|&(name, ref kind)| {
                            match kind {
                                MemberKind::HandleMethod => None,
                                MemberKind::Fn => {
                                    let mut it = item(name, CompletionItemKind::FUNCTION);
                                    it.detail = std_sig(&module, name).map(render_sig_detail);
                                    it.data = Some(serde_json::json!({"module": module, "name": name}));
                                    if !prefix.is_empty() {
                                        it.filter_text = Some(prefix.clone());
                                    }
                                    Some(it)
                                }
                                MemberKind::Const(ty) => {
                                    let mut it = item(name, CompletionItemKind::CONSTANT);
                                    it.detail = Some((*ty).to_string());
                                    it.data = Some(serde_json::json!({"module": module, "name": name}));
                                    if !prefix.is_empty() {
                                        it.filter_text = Some(prefix.clone());
                                    }
                                    Some(it)
                                }
                            }
                        })
                        .collect();
                }
            } else if let Some(exports) = crate::stdlib::std_module_exports(&module) {
                if !exports.is_empty() {
                    return exports
                        .into_iter()
                        .map(|(name, _)| {
                            let mut it = item(&name, CompletionItemKind::FUNCTION);
                            if !prefix.is_empty() {
                                it.filter_text = Some(prefix.clone());
                            }
                            it
                        })
                        .collect();
                }
            }
        }

        // Context 3: member access on a class/enum NAME — offer its static surface
        // (enum variants, or the class's declared fields + methods) from the SP10
        // table. This only runs when the alias is NOT a namespace import.
        let table = crate::check::infer::table::Table::build(&model.tree, &model.resolved);
        if let Some(eid) = table.enum_id(&alias) {
            if let Some(info) = table.enum_info(eid) {
                return info
                    .variants
                    .iter()
                    .map(|v| {
                        let mut it = variant_completion_item(v, info, &table);
                        if !prefix.is_empty() {
                            it.filter_text = Some(prefix.clone());
                        }
                        it
                    })
                    .collect();
            }
        }
        if let Some(cid) = table.class_id(&alias) {
            if let Some(info) = table.class(cid) {
                let mut items = class_member_items(info);
                if !prefix.is_empty() {
                    for it in &mut items {
                        it.filter_text = Some(prefix.clone());
                    }
                }
                return items;
            }
        }

        // Context 4: member access on a TYPED VALUE receiver — `c.` where `c` is a
        // VALUE whose inferred type is a known class/shape (NOT the class NAME itself).
        // Resolve the receiver's type through `crate::check::infer` (the same
        // `hover_type_at` entry point hover/inlay use), extract its class name, and
        // offer that class's fields + methods. The LSP runs NO code — `hover_type_at`
        // is a pure static inference pass.
        // Compute the dot position: offset - prefix.len() - 1 (the char before the prefix).
        let dot_pos = offset.saturating_sub(prefix.chars().count()).saturating_sub(1);
        if let Some(info) = receiver_class_info(model, &chars, dot_pos, &alias, &table) {
            let mut items = class_member_items(info);
            if !prefix.is_empty() {
                for it in &mut items {
                    it.filter_text = Some(prefix.clone());
                }
            }
            return items;
        }
    }

    let mut base = baseline_completions();
    base.extend(binding_completions(model, offset));
    base.extend(snippet_completions());
    base.extend(auto_import_candidates(model));
    base
}

/// Fill `detail`/`documentation` for an item that the cheap pass left bare. v1
/// resolves builtins/keywords from the shared docs table; bindings are left as-is
/// (their kind icon already conveys the essential info).
///
/// API adaptation vs the plan: `docs::keyword_doc` takes a `SyntaxKind`, not a
/// `&str`, so a keyword label is lexed to its leading token kind before lookup;
/// `docs::builtin_doc` takes the `&str` label directly.
pub fn resolve_completion(_model: &SemanticModel, item: &mut CompletionItem) {
    if item.documentation.is_some() {
        return;
    }
    let doc = crate::lsp::providers::docs::builtin_doc(&item.label)
        .map(str::to_string)
        .or_else(|| keyword_doc_for_label(&item.label).map(str::to_string));
    if let Some(doc) = doc {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc,
        }));
    }
}

/// The keyword doc for a label string, by lexing it to its leading token kind and
/// consulting the shared `docs::keyword_doc(SyntaxKind)` table.
fn keyword_doc_for_label(label: &str) -> Option<&'static str> {
    let kind = crate::syntax::lex(label).first().map(|t| t.kind)?;
    crate::lsp::providers::docs::keyword_doc(kind)
}

/// Whether `offset` sits inside the still-open string of a `from "..."` / `from '...'`
/// on the current line. Scans backward from the cursor within the current line for an
/// opening quote with no closing quote before the cursor, then checks the text before
/// that quote ends with `from`.
fn in_import_path_string(chars: &[char], offset: usize) -> bool {
    // Restrict to the current line (imports are single-line).
    let line_start = chars[..offset]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |p| p + 1);
    let line = &chars[line_start..offset];

    // The cursor is inside a string iff the most recent quote on the line has no
    // matching close before the cursor — i.e. it's the last quote on the line.
    let Some(rel_quote) = line.iter().rposition(|&c| c == '"' || c == '\'') else {
        return false;
    };
    // Check the text before that opening quote ends with `from` (allowing whitespace).
    let before: String = line[..rel_quote].iter().collect();
    before.trim_end().ends_with("from")
}

/// A class's member completion surface: its declared FIELDS (FIELD kind) + METHODS
/// (METHOD kind). Shared by the class-NAME static case and the typed-VALUE-receiver
/// case (Context 3/4).
fn class_member_items(info: &crate::check::infer::table::ClassInfo) -> Vec<CompletionItem> {
    let mut out: Vec<CompletionItem> = info
        .fields
        .keys()
        .map(|f| item(f, CompletionItemKind::FIELD))
        .collect();
    out.extend(
        info.methods
            .keys()
            .map(|m| item(m, CompletionItemKind::METHOD)),
    );
    out
}

/// Context 4: if the receiver `alias` ending just before the dot (at `dot_pos` in
/// char space) is a VALUE whose inferred type names a known class, return that
/// `ClassInfo`. The receiver's type is resolved via
/// `crate::check::infer::hover_type_at` (the static inference pass hover/inlay
/// already use — runs NO code), the leading class identifier is extracted from the
/// rendered type, and looked up in the SP10 table. `None` when the type is a
/// primitive / `Any` / a non-class (then completion falls back to the baseline).
fn receiver_class_info<'t>(
    model: &SemanticModel,
    chars: &[char],
    dot_pos: usize,
    alias: &str,
    table: &'t crate::check::infer::table::Table,
) -> Option<&'t crate::check::infer::table::ClassInfo> {
    // The receiver identifier occupies `[dot - alias_len, dot)` in CHAR space; aim at
    // its middle char so the inference hover-span lookup lands inside the name. Convert
    // to the BYTE offset `hover_type_at` expects (it operates on the raw source bytes).
    let alias_chars = alias.chars().count();
    let recv_start = dot_pos.checked_sub(alias_chars)?;
    let recv_mid_char = recv_start + alias_chars / 2;
    let byte_off = crate::lsp::convert::char_to_byte(&model.text, recv_mid_char.min(chars.len()));
    let rendered = crate::check::infer::hover_type_at(&model.text, byte_off)?;
    let class_name = first_class_ident(&rendered)?;
    let cid = table.class_id(&class_name)?;
    table.class(cid)
}

/// Extract the leading user-CLASS identifier from a rendered `CheckTy` string,
/// skipping builtin/container type names (`User` from `User`, `User?`,
/// `array<User>`). Mirrors `navigation::first_type_ident`.
fn first_class_ident(rendered: &str) -> Option<String> {
    const BUILTIN: &[&str] = &[
        "number", "string", "bool", "nil", "any", "array", "map", "future", "bytes", "regex",
        "object", "void", "never", "int", "float", "set",
    ];
    let mut cur = String::new();
    for ch in rendered.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            if !cur.is_empty() && !BUILTIN.contains(&cur.as_str()) {
                return Some(cur);
            }
            cur.clear();
        }
    }
    if !cur.is_empty() && !BUILTIN.contains(&cur.as_str()) {
        return Some(cur);
    }
    None
}

/// If the text at `offset` is `<ident>.<prefix>` (prefix may be empty, i.e. the
/// cursor is right after the dot), return `(alias, prefix)` where `alias` is the
/// identifier ending just before the LAST dot before the prefix, and `prefix` is the
/// typed portion after the dot up to the cursor.
///
/// C1 support: `math.|` → `("math", "")`, `math.sq|` → `("math", "sq")`.
/// Chain case: for `a.b.c|`, prefix = `"c"`, alias = `"b"` (the ident immediately
/// before the dot that precedes the prefix).
fn member_access_alias(chars: &[char], offset: usize) -> Option<(String, String)> {
    if offset == 0 {
        return None;
    }
    // Backtrack over trailing ident chars to find where the prefix starts.
    let prefix_end = offset;
    let mut prefix_start = prefix_end;
    while prefix_start > 0 && is_ident_char(chars[prefix_start - 1]) {
        prefix_start -= 1;
    }
    // The first ident-start char of the prefix must not be a digit (a digit-led run
    // would be a number, not an identifier prefix). If the run is empty (cursor right
    // after dot) the check is vacuously fine.
    if prefix_start < prefix_end && chars[prefix_start].is_ascii_digit() {
        return None;
    }
    // There must be a dot immediately before the prefix.
    if prefix_start == 0 || chars[prefix_start - 1] != '.' {
        return None;
    }
    let dot = prefix_start - 1;
    // Collect the identifier ending just before the dot (the alias).
    let alias_end = dot;
    let mut alias_start = alias_end;
    while alias_start > 0 && is_ident_char(chars[alias_start - 1]) {
        alias_start -= 1;
    }
    if alias_start == alias_end {
        return None; // no ident before the dot
    }
    // The first char of the alias must be a valid identifier start (not a digit).
    if chars[alias_start].is_ascii_digit() {
        return None;
    }
    let alias: String = chars[alias_start..alias_end].iter().collect();
    let prefix: String = chars[prefix_start..prefix_end].iter().collect();
    Some((alias, prefix))
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Public re-export for the signature provider (same logic, pub(crate) visibility).
pub(crate) fn namespace_import_module_pub(text: &str, alias: &str) -> Option<String> {
    namespace_import_module(text, alias)
}

/// Scan `text` for `import * as <alias> from "std/<mod>"` and return the module path.
/// Parser-free (works on broken docs): a regex-like manual scan per line.
fn namespace_import_module(text: &str, alias: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("import") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('*') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("as") else {
            continue;
        };
        let rest = rest.trim_start();
        // The alias is the next identifier.
        let name: String = rest.chars().take_while(|&c| is_ident_char(c)).collect();
        if name != alias {
            continue;
        }
        let rest = &rest[name.len()..];
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("from") else {
            continue;
        };
        let rest = rest.trim_start();
        // Extract the quoted module path.
        let mut chars = rest.chars();
        let q = chars.next()?;
        if q != '"' && q != '\'' {
            continue;
        }
        let path: String = chars.take_while(|&c| c != q).collect();
        return Some(path);
    }
    None
}

/// C8: return the `SyntaxKind` of the token whose character range CONTAINS the cursor
/// at `char_offset`, or `None` if the cursor falls between tokens (e.g. at a boundary
/// between whitespace and code, or past the end of the file).
///
/// `LexToken` carries no byte/char ranges — compute them by walking the token list and
/// accumulating char lengths. The token stream is lossless (concatenating all texts
/// reproduces the source), so char positions are exact.
///
/// We want the token that OWNS the cursor: the one where
/// `tok_start < char_offset <= tok_end` (exclusive on the left, inclusive on the
/// right — a cursor right at the end of a token is still "inside" it for suppression
/// purposes, e.g. `"hel|` with cursor after `l` is inside the Str token).
fn token_kind_at_cursor(
    tokens: &[crate::syntax::lexer::LexToken],
    char_offset: usize,
    _chars: &[char],
) -> Option<crate::syntax::kind::SyntaxKind> {
    let mut pos = 0usize;
    for tok in tokens {
        let tok_len = tok.text.chars().count();
        let tok_end = pos + tok_len;
        // The cursor is inside this token if it falls in (pos, tok_end].
        // We use a half-open interval: cursor > pos (not at the very start of this
        // token, which belongs to the previous) AND cursor <= tok_end.
        if char_offset > pos && char_offset <= tok_end {
            return Some(tok.kind);
        }
        pos = tok_end;
    }
    None
}

/// C8: whether a `SyntaxKind` is a suppressed token (plain string body or comment).
/// Template INTERPOLATION interiors use normal token kinds (Ident, Number, etc.) so
/// they are NOT in this set — completion inside `${n` works normally.
/// TemplateStr (a template with no interpolation, `` `plain` ``) IS suppressed because
/// the cursor is inside the literal string body.
/// TemplateStart/Middle/End contain the literal parts (`` `text${ `` / ``}text${ `` /
/// ``}text` ``) — suppress cursor positions inside those literal parts.
fn suppressed_token_kind(kind: crate::syntax::kind::SyntaxKind) -> bool {
    use crate::syntax::kind::SyntaxKind;
    matches!(
        kind,
        SyntaxKind::Str
            | SyntaxKind::TemplateStr
            | SyntaxKind::TemplateStart
            | SyntaxKind::TemplateMiddle
            | SyntaxKind::TemplateEnd
            | SyntaxKind::LineComment
            | SyntaxKind::BlockComment
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    /// Build a model and complete at the END of `src` (its char count).
    fn items(src: &str) -> Vec<CompletionItem> {
        let model = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        completions(&model, src.chars().count())
    }

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn completions_baseline_has_keywords_and_builtins() {
        let it = items("let x = 1\n");
        let ls = labels(&it);
        for expected in ["fn", "let", "match", "print", "Ok"] {
            assert!(
                ls.contains(&expected),
                "baseline should contain {expected:?}: {ls:?}"
            );
        }
        // Kinds are set.
        let fnkw = it.iter().find(|i| i.label == "fn").unwrap();
        assert_eq!(fnkw.kind, Some(CompletionItemKind::KEYWORD));
        let pr = it.iter().find(|i| i.label == "print").unwrap();
        assert_eq!(pr.kind, Some(CompletionItemKind::FUNCTION));
    }

    #[test]
    fn completions_baseline_offers_primitive_type_names() {
        let it = items("let x: \n");
        let ls = labels(&it);
        for expected in ["int", "float", "number", "string", "bool"] {
            assert!(
                ls.contains(&expected),
                "baseline should offer type name {expected:?}: {ls:?}"
            );
        }
        let intt = it.iter().find(|i| i.label == "int").unwrap();
        assert_eq!(intt.kind, Some(CompletionItemKind::TYPE_PARAMETER));
    }

    #[test]
    fn completions_in_import_path_offers_module_paths() {
        let it = items("import { x } from \"std/");
        let ls = labels(&it);
        for expected in ["std/string", "std/json", "std/net/http"] {
            assert!(
                ls.contains(&expected),
                "import ctx should contain {expected:?}: {ls:?}"
            );
        }
        assert!(it.iter().all(|i| i.kind == Some(CompletionItemKind::MODULE)));
    }

    #[test]
    fn completions_in_import_path_single_quote() {
        let it = items("import { x } from 'std/ma");
        assert!(labels(&it).contains(&"std/math"));
    }

    #[test]
    fn completions_offer_shared_module_path() {
        // SRV Task 10: `std/shared` is an importable module — completion must offer
        // it (it was missing from STD_MODULE_PATHS otherwise, an orphan like the docs
        // NAV gotcha). Hover/completion never breaks on a frozen value because a
        // `Value::Shared` is typed gradually (`Any`) by the checker.
        let it = items("import * as shared from \"std/sh");
        assert!(
            labels(&it).contains(&"std/shared"),
            "import ctx should offer std/shared: {:?}",
            labels(&it)
        );
    }

    #[test]
    fn completions_member_access_offers_module_exports() {
        let it = items("import * as math from \"std/math\"\nlet y = math.");
        let ls = labels(&it);
        for expected in ["sqrt", "abs", "pi"] {
            assert!(
                ls.contains(&expected),
                "math. should contain {expected:?}: {ls:?}"
            );
        }
    }

    #[test]
    fn completions_member_access_unknown_alias_falls_back_to_baseline() {
        // `foo` is not a namespace import → baseline.
        let it = items("let foo = 1\nfoo.");
        assert!(labels(&it).contains(&"print"));
    }

    #[test]
    fn completions_on_garbage_returns_baseline_no_panic() {
        // Non-string, non-comment garbage → baseline still offered.
        for src in ["", "@#$%^", "fn fn fn (((", "import * as"] {
            let it = items(src);
            assert!(
                labels(&it).contains(&"let"),
                "garbage {src:?} should still yield baseline"
            );
        }
        // Unterminated string: C8 suppresses (returns empty — not baseline). Must NOT panic.
        let _ = items("\"unterminated");
        // An out-of-range offset must not panic.
        let model = SemanticModel::build("let x".to_string(), None, &LintConfig::default());
        let _ = completions(&model, 9999);
    }

    #[test]
    fn std_module_paths_complete_against_std_modules() {
        // COMPLETENESS: the completion list is the canonical feature-independent
        // `stdlib::STD_MODULES` (a direct reuse — drift is impossible by
        // construction; this guards against the reuse ever being replaced by a
        // copy again). The old hardcoded list had 29 entries vs the canonical ~56,
        // silently hiding std/task, std/os, std/log, std/schema, etc.
        assert_eq!(STD_MODULE_PATHS, crate::stdlib::STD_MODULES);
        // And every advertised path resolves under default features (cargo test
        // enables all default features, so every default-gated module is present).
        for path in STD_MODULE_PATHS {
            assert!(
                crate::stdlib::std_module_exports(path).is_some(),
                "STD_MODULE_PATHS entry {path:?} is not a known stdlib module"
            );
        }
    }

    #[test]
    fn newly_included_modules_complete_and_auto_import() {
        // FIX 3 regression: modules the old hardcoded list omitted now surface in
        // import-path completion…
        let it = items("import { x } from \"std/");
        let ls = labels(&it);
        for expected in ["std/task", "std/os", "std/log", "std/schema", "std/net/udp"] {
            assert!(
                ls.contains(&expected),
                "import ctx should offer {expected:?}: {ls:?}"
            );
        }
        // …and their exports resolve, so auto-import path construction works for
        // them (e.g. `task.spawn` from std/task, `os.platform` from std/os).
        for (module, export) in [("std/task", "spawn"), ("std/os", "platform")] {
            let exports = crate::stdlib::std_module_exports(module)
                .unwrap_or_else(|| panic!("{module} must resolve under default features"));
            assert!(
                exports.iter().any(|(n, _)| n == export),
                "{module} should export {export:?}"
            );
        }
        // An export unique to a newly-included module is offered as an auto-import
        // with the matching import edit.
        let model = SemanticModel::build("sp\n".to_string(), None, &LintConfig::default());
        let items = completions(&model, 2);
        let spawn = items
            .iter()
            .find(|i| {
                i.label == "spawn"
                    && i.detail.as_deref() == Some("auto-import from std/task")
            })
            .expect("spawn auto-import from std/task offered");
        let edits = spawn.additional_text_edits.as_ref().expect("has import edit");
        assert!(
            edits[0].new_text.contains("import { spawn } from \"std/task\""),
            "import edit text: {:?}",
            edits[0].new_text
        );
    }

    #[test]
    fn completions_in_import_path_offers_process() {
        let it = items("import { run } from \"std/proc");
        assert!(labels(&it).contains(&"std/process"));
    }

    #[test]
    fn completions_baseline_includes_yield_keyword() {
        let it = items("");
        let y = it
            .iter()
            .find(|i| i.label == "yield")
            .expect("yield keyword in baseline");
        assert_eq!(y.kind, Some(CompletionItemKind::KEYWORD));
    }

    #[test]
    fn resolve_fills_detail_for_builtin() {
        let model = SemanticModel::build("x\n".to_string(), None, &crate::check::LintConfig::default());
        let mut print_item = item("print", CompletionItemKind::FUNCTION);
        assert!(print_item.documentation.is_none());
        resolve_completion(&model, &mut print_item);
        assert!(
            print_item.documentation.is_some() || print_item.detail.is_some(),
            "resolve should add detail/docs for a builtin"
        );
    }

    #[test]
    fn resolve_fills_detail_for_keyword() {
        let model = SemanticModel::build("x\n".to_string(), None, &crate::check::LintConfig::default());
        let mut let_item = item("let", CompletionItemKind::KEYWORD);
        resolve_completion(&model, &mut let_item);
        assert!(
            let_item.documentation.is_some(),
            "resolve should add docs for a keyword"
        );
    }

    #[test]
    fn auto_import_offers_import_edit_for_known_std_export() {
        // `abs` is exported by std/math; nothing imports it yet.
        let src = "ab\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        let items = completions(&model, 2);
        // `abs` is exported by more than one module (std/decimal AND std/math, both
        // in the canonical list) — each gets its own auto-import item; pick math's.
        let abs = items
            .iter()
            .find(|i| {
                i.label == "abs" && i.detail.as_deref() == Some("auto-import from std/math")
            })
            .expect("abs auto-import from std/math offered");
        let edits = abs.additional_text_edits.as_ref().expect("has import edit");
        assert_eq!(edits.len(), 1);
        assert!(
            edits[0].new_text.contains("import") && edits[0].new_text.contains("std/math"),
            "import edit text: {:?}",
            edits[0].new_text
        );
        // The import is inserted at the top of the file (line 0).
        assert_eq!(edits[0].range.start.line, 0);
    }

    #[test]
    fn baseline_includes_snippets() {
        let model = SemanticModel::build("x\n".to_string(), None, &crate::check::LintConfig::default());
        let items = completions(&model, 1);
        let fn_snip = items
            .iter()
            .find(|i| i.label == "fn" && i.insert_text_format == Some(InsertTextFormat::SNIPPET))
            .expect("fn snippet present");
        assert!(
            fn_snip.insert_text.as_deref().unwrap_or("").contains("$0")
                || fn_snip.insert_text.as_deref().unwrap_or("").contains("${1"),
            "snippet has a tab-stop: {:?}",
            fn_snip.insert_text
        );
    }

    #[test]
    fn member_access_offers_enum_variants_and_class_members() {
        let src = "enum Color { Red, Green }\nclass Point { x: number\n  fn dist() { return 0 } }\nColor.\nPoint.\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());

        let color_off = src.find("Color.\n").unwrap() + "Color.".len();
        let cl = completions(&model, color_off);
        let cls: Vec<&str> = cl.iter().map(|i| i.label.as_str()).collect();
        assert!(cls.contains(&"Red") && cls.contains(&"Green"), "enum variants: {cls:?}");

        let point_off = src.find("Point.\n").unwrap() + "Point.".len();
        let pl = completions(&model, point_off);
        let pls: Vec<&str> = pl.iter().map(|i| i.label.as_str()).collect();
        assert!(pls.contains(&"x"), "class field: {pls:?}");
        assert!(pls.contains(&"dist"), "class method: {pls:?}");
    }

    #[test]
    fn member_access_offers_adt_payload_variants_with_signatures() {
        // ADT Task 13: after `Shape.`, payload variants are offered with a signature
        // `detail` and a snippet insert carrying field placeholders; the unit variant
        // is a plain reference.
        let src = "enum Shape {\n  Circle(radius: float),\n  Rect(w: float, h: float),\n  Pair(int, int),\n  Point,\n}\nShape.\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        let off = src.rfind("Shape.\n").unwrap() + "Shape.".len();
        let items = completions(&model, off);
        let find = |label: &str| items.iter().find(|i| i.label == label).cloned();

        let circle = find("Circle").expect("Circle offered");
        assert_eq!(circle.detail.as_deref(), Some("(radius: float)"));
        assert_eq!(circle.insert_text.as_deref(), Some("Circle(${1:radius})"));
        assert_eq!(circle.insert_text_format, Some(InsertTextFormat::SNIPPET));

        // A MULTI-field named variant must be CALLED with named args (the engine
        // rejects a positional call), so the snippet emits the `name: <type>` form —
        // a positional `Rect(${1:w}, ${2:h})` would tab-complete straight into a
        // runtime "requires named fields" error. Single-field named `Circle` and the
        // positional `Pair` accept positional args, so they keep bare placeholders.
        let rect = find("Rect").expect("Rect offered");
        assert_eq!(rect.detail.as_deref(), Some("(w: float, h: float)"));
        assert_eq!(rect.insert_text.as_deref(), Some("Rect(w: ${1:float}, h: ${2:float})"));

        let pair = find("Pair").expect("Pair offered");
        assert_eq!(pair.detail.as_deref(), Some("(int, int)"));
        assert_eq!(pair.insert_text.as_deref(), Some("Pair(${1:int}, ${2:int})"));

        let point = find("Point").expect("Point offered");
        assert!(point.detail.is_none(), "unit variant has no signature detail");
        assert!(point.insert_text.is_none(), "unit variant is a plain insert");
    }

    #[test]
    fn baseline_includes_in_scope_bindings() {
        // A top-level `let` and a fn name are in scope at the cursor and should be
        // offered alongside keywords/builtins.
        let src = "let total = 1\nfn helper() {}\nt\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        let off = src.rfind('t').unwrap() + 1; // just after the `t` on the last line
        let items = completions(&model, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"total"), "missing local binding: {labels:?}");
        assert!(labels.contains(&"helper"), "missing fn binding: {labels:?}");
        // Keywords + builtins still present (subset preserved).
        assert!(labels.contains(&"let"), "keyword missing: {labels:?}");
        assert!(labels.contains(&"print"), "builtin missing: {labels:?}");
    }

    #[test]
    fn completions_baseline_includes_test_builtin() {
        let it = items("");
        let t = it
            .iter()
            .find(|i| i.label == "test")
            .expect("test builtin in baseline");
        assert_eq!(t.kind, Some(CompletionItemKind::FUNCTION));
    }

    // ── Task 13: frame-precise identifier completion ─────────────────────────────

    #[test]
    fn frame_precise_excludes_sibling_scope_local() {
        // Two sibling functions, each with a distinct local. The cursor sits in `a`'s
        // body: `foo` (a's local) is offered, but `bar` (b's local — a sibling scope
        // that does NOT enclose the cursor) is NOT. A module-global and a builtin are
        // still offered.
        let src = "let g = 0\nfn a() {\n  let foo = 1\n  f\n}\nfn b() {\n  let bar = 2\n}\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        // Cursor just after the lone `f` on line 3 (inside a's body).
        let off = src.find("  f\n").unwrap() + "  f".len();
        let items = completions(&model, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"foo"), "a's own local foo must be offered: {labels:?}");
        assert!(!labels.contains(&"bar"), "sibling b's local bar must NOT be offered: {labels:?}");
        // Module-global, builtin, keyword all still offered (in scope everywhere).
        assert!(labels.contains(&"g"), "module-global g must be offered: {labels:?}");
        assert!(labels.contains(&"print"), "builtin print must be offered: {labels:?}");
        assert!(labels.contains(&"let"), "keyword let must be offered: {labels:?}");
    }

    #[test]
    fn frame_precise_inner_closure_sees_enclosing_locals() {
        // An inner closure sees its enclosing function's local via the upvalue chain.
        let src = "fn outer() {\n  let captured = 1\n  fn inner() {\n    c\n  }\n}\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        let off = src.find("    c\n").unwrap() + "    c".len();
        let items = completions(&model, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.contains(&"captured"),
            "inner closure must see the enclosing local `captured`: {labels:?}"
        );
    }

    #[test]
    fn frame_precise_excludes_later_sibling_scope() {
        // A name declared LATER in a sibling scope is not offered (frame chain, not
        // declaration order).
        let src = "fn a() {\n  x\n}\nfn b() {\n  let later = 2\n}\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        let off = src.find("  x\n").unwrap() + "  x".len();
        let items = completions(&model, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            !labels.contains(&"later"),
            "a later sibling-scope binding must NOT be offered: {labels:?}"
        );
    }

    // ── Task 13: member completion via infer ─────────────────────────────────────

    #[test]
    fn member_access_on_typed_instance_offers_fields_and_methods() {
        // `c.` where `c: C` (inferred) offers C's fields + methods.
        let src = "class C {\n  x: number\n  fn m() {}\n}\nlet c = C()\nc.\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        let off = src.rfind("c.\n").unwrap() + "c.".len();
        let items = completions(&model, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"x"), "instance field x must be offered: {labels:?}");
        assert!(labels.contains(&"m"), "instance method m must be offered: {labels:?}");
    }

    #[test]
    fn member_access_on_namespace_offers_module_exports() {
        // `math.` where `import * as math from "std/math"` offers module exports.
        let src = "import * as math from \"std/math\"\nmath.\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        let off = src.rfind("math.\n").unwrap() + "math.".len();
        let items = completions(&model, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"abs"), "module export abs must be offered: {labels:?}");
        assert!(labels.contains(&"sqrt"), "module export sqrt must be offered: {labels:?}");
    }

    // ── SIG Task 2.3: stdlib member kind/detail/docs + cached auto-import ───────

    #[test]
    fn member_items_carry_real_kind_detail_and_resolve_docs() {
        let it = items("import * as math from \"std/math\"\nlet y = math.");
        let pi = it.iter().find(|i| i.label == "pi").expect("pi offered");
        assert_eq!(pi.kind, Some(CompletionItemKind::CONSTANT), "pi is a constant");
        assert_eq!(pi.detail.as_deref(), Some("float"));
        let pow = it.iter().find(|i| i.label == "pow").expect("pow offered");
        assert_eq!(pow.kind, Some(CompletionItemKind::FUNCTION));
        assert_eq!(pow.detail.as_deref(), Some("(base: number, exp: number) -> float"));
        let mut pow = pow.clone();
        resolve_completion_static(&mut pow);
        let Some(Documentation::MarkupContent(mk)) = &pow.documentation else {
            panic!("resolve should fill stdlib docs")
        };
        assert!(mk.value.contains("Raise a base"));
    }

    #[test]
    fn auto_import_items_are_deprioritized_and_cached() {
        let a = items("ab\n");
        let abs = a.iter()
            .find(|i| i.detail.as_deref() == Some("auto-import from std/math") && i.label == "abs")
            .expect("abs auto-import");
        assert!(abs.sort_text.as_deref().unwrap_or("").starts_with("zz"),
            "auto-import must sort after locals: {:?}", abs.sort_text);
        assert!(std::ptr::eq(auto_import_entries(), auto_import_entries()));
    }

    // ── SIG Task 3.1 C1: partial-identifier member completion ────────────────────

    #[test]
    fn member_completion_with_partial_identifier_after_dot() {
        // C1: manual invoke at `math.sq|` must offer members (filtered client-side).
        let it = items("import * as math from \"std/math\"\nlet y = math.sq");
        let sqrt = it.iter().find(|i| i.label == "sqrt").expect("sqrt offered at math.sq|");
        assert_eq!(sqrt.filter_text.as_deref(), Some("sq"), "typed prefix as filter_text");
    }

    // ── SIG Task 3.1 C8: string/comment suppression ──────────────────────────────

    #[test]
    fn no_completion_inside_strings_and_comments() {
        // C8: a plain string body and a comment body yield NO items…
        assert!(items("let s = \"hel").is_empty(), "inside plain string must yield no items");
        assert!(items("// com").is_empty(), "inside line comment must yield no items");
        assert!(items("/* blo").is_empty(), "inside block comment must yield no items");
        // …but the import-path context still completes,
        assert!(!items("import { x } from \"std/").is_empty(), "import path must still complete");
        // and a template INTERPOLATION completes normally.
        let it = items("let n = 1\nlet s = `v: ${n");
        assert!(it.iter().any(|i| i.label == "print"), "template interp should complete");
    }
}
