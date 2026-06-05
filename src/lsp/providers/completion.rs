//! `textDocument/completion` over the model.
//!
//! Phase 0 keeps completion's CURRENT behavior (baseline keywords + builtins,
//! import-path string context, and namespace-member access) but reads from the
//! cached [`SemanticModel`]'s text instead of re-lexing/parsing. The full,
//! resolver-aware rewrite is Phase 1; the logic here is byte-identical to the
//! legacy `analysis::completions`.

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
    "continue",
];

/// The global builtins offered as completions (FUNCTION kind). Mirrors `builtin_doc`.
const BUILTINS: &[&str] = &[
    "print", "len", "type", "assert", "range", "Ok", "Err", "recover", "test", "exit",
];

/// The known stdlib module paths offered when completing an `import ... from "..."`
/// string. Hardcoded (rather than derived from `std_module_exports`) so the list is
/// stable regardless of which cargo features are enabled at build time — editors
/// should see every documented module path. Kept in sync with `std_module_exports`
/// in `src/stdlib/mod.rs`.
const STD_MODULE_PATHS: &[&str] = &[
    "std/string",
    "std/array",
    "std/object",
    "std/map",
    "std/math",
    "std/convert",
    "std/json",
    "std/regex",
    "std/encoding",
    "std/bytes",
    "std/uuid",
    "std/csv",
    "std/toml",
    "std/yaml",
    "std/time",
    "std/date",
    "std/intl",
    "std/env",
    "std/fs",
    "std/process",
    "std/crypto",
    "std/compress",
    "std/sqlite",
    "std/net/tcp",
    "std/net/http",
    "std/http/server",
    "std/net/ws",
    "std/tui",
];

/// A baseline completion item (keyword or builtin).
fn item(label: &str, kind: CompletionItemKind) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        ..CompletionItem::default()
    }
}

/// The always-offered baseline completions: every keyword + every global builtin.
fn baseline_completions() -> Vec<CompletionItem> {
    let mut out = Vec::with_capacity(KEYWORDS.len() + BUILTINS.len());
    for kw in KEYWORDS {
        out.push(item(kw, CompletionItemKind::KEYWORD));
    }
    for b in BUILTINS {
        out.push(item(b, CompletionItemKind::FUNCTION));
    }
    out
}

/// In-scope user bindings as completion items. Phase 1 v1 offers EVERY binding in
/// the resolved set (de-duplicated by name, last decl wins) — precise per-cursor
/// scope filtering by frame is a Phase-2 refinement; over-offering a sibling-scope
/// name is a benign, non-misleading suggestion. The binding KIND maps to an icon.
fn binding_completions(model: &SemanticModel) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for b in &model.resolved.bindings {
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

    let mut out = Vec::new();
    for path in STD_MODULE_PATHS {
        let Some(exports) = crate::stdlib::std_module_exports(path) else {
            continue;
        };
        for (name, _value) in exports {
            if imported.contains(&name) {
                continue;
            }
            let mut ci = item(&name, CompletionItemKind::FUNCTION);
            ci.detail = Some(format!("auto-import from {path}"));
            ci.additional_text_edits = Some(vec![TextEdit {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                new_text: format!("import {{ {name} }} from \"{path}\"\n"),
            }]);
            out.push(ci);
        }
    }
    out
}

/// Completions at char `offset` in the model's text. Pure and robust: never panics,
/// and always returns at least the baseline (keywords + builtins) even on partial or
/// syntactically broken input (completion is requested mid-edit).
///
/// Context detection is done by simple, parser-free scanning of the raw text around
/// the cursor, so it works on documents that do not yet parse:
/// - inside an `import ... from "..."` / `'...'` string → stdlib module paths;
/// - right after `<ident>.` where `<ident>` is a `import * as <ident>` namespace of a
///   known std module → that module's exports.
pub fn completions(model: &SemanticModel, offset: usize) -> Vec<CompletionItem> {
    let text = &model.text;
    let chars: Vec<char> = text.chars().collect();
    let offset = offset.min(chars.len());

    // Context 1: inside an import-from string literal → offer module paths.
    if in_import_path_string(&chars, offset) {
        return STD_MODULE_PATHS
            .iter()
            .map(|p| item(p, CompletionItemKind::MODULE))
            .collect();
    }

    // Context 2: member access `<ident>.` where ident is a namespace import.
    if let Some(alias) = member_access_alias(&chars, offset) {
        if let Some(module) = namespace_import_module(text, &alias) {
            if let Some(exports) = crate::stdlib::std_module_exports(&module) {
                if !exports.is_empty() {
                    return exports
                        .into_iter()
                        .map(|(name, _)| item(&name, CompletionItemKind::FUNCTION))
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
                    .map(|v| item(v, CompletionItemKind::ENUM_MEMBER))
                    .collect();
            }
        }
        if let Some(cid) = table.class_id(&alias) {
            if let Some(info) = table.class(cid) {
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
                return out;
            }
        }
    }

    let mut base = baseline_completions();
    base.extend(binding_completions(model));
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

/// If the text immediately before `offset` is `<ident>.`, return `<ident>`.
fn member_access_alias(chars: &[char], offset: usize) -> Option<String> {
    if offset == 0 {
        return None;
    }
    // The char right before the cursor must be a dot.
    if chars[offset - 1] != '.' {
        return None;
    }
    // Collect the identifier ending just before the dot.
    let dot = offset - 1;
    let mut start = dot;
    while start > 0 && is_ident_char(chars[start - 1]) {
        start -= 1;
    }
    if start == dot {
        return None; // no ident before the dot
    }
    // The first char must be a valid identifier start (not a digit).
    if chars[start].is_ascii_digit() {
        return None;
    }
    Some(chars[start..dot].iter().collect())
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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
        for src in ["", "@#$%^", "fn fn fn (((", "import * as", "\"unterminated"] {
            let it = items(src);
            assert!(
                labels(&it).contains(&"let"),
                "garbage {src:?} should still yield baseline"
            );
        }
        // An out-of-range offset must not panic.
        let model = SemanticModel::build("let x".to_string(), None, &LintConfig::default());
        let _ = completions(&model, 9999);
    }

    #[test]
    fn std_module_paths_all_resolve_under_default_features() {
        // Every advertised import path must be a real registered module, so the const
        // can't silently drift from `std_module_exports`. (cargo test enables all
        // default features, so every default-gated path resolves.)
        for path in STD_MODULE_PATHS {
            assert!(
                crate::stdlib::std_module_exports(path).is_some(),
                "STD_MODULE_PATHS entry {path:?} is not a known stdlib module"
            );
        }
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
        let abs = items
            .iter()
            .find(|i| i.label == "abs")
            .expect("abs auto-import offered");
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
}
