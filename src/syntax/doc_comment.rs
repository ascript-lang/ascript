//! The `///` / `//!` doc-comment convention (DX D1, spec §2).
//!
//! A `///`-prefixed line is, lexically, a [`SyntaxKind::LineComment`] — the CST
//! already preserves it losslessly as leading trivia on the following node. This
//! module *reinterprets* such trivia as a doc-comment; it introduces **no new
//! `SyntaxKind`** and never re-tokenizes. It is the single source of truth shared
//! by `ascript doc` and the LSP hover/docs provider.
//!
//! Attachment rule (LOCKED, spec §2): a contiguous run of `///` `LineComment`
//! trivia immediately preceding a declaration — with **no blank line** between the
//! last `///` and the decl — is that declaration's doc. The extractor walks the
//! CST leading trivia BACKWARD from the decl's first token, collecting the
//! contiguous `///` run, and **stops at the first blank line** where a blank line
//! is **≥ 2 consecutive `Newline` trivia tokens** (intervening `Whitespace`
//! indentation ignored). `////` (≥ 4 slashes) is an ordinary comment, never doc.
//! One leading space after `/// ` is stripped; the first paragraph is the summary.
//!
//! `//!` at the top of a file / block is the module/inner doc.
//!
//! This module is pure over `&str` / byte ranges and the CST — `Send`-able, with
//! no interpreter.

use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;

/// An extracted doc-comment: the rendered Markdown body plus a parsed structured
/// overlay (`@param`/`@returns`/… tags). The `body` is the full Markdown (tags
/// included verbatim — the overlay is additive); `summary` is the first paragraph.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocComment {
    /// The full Markdown body (every `///` line, leading-space-stripped, joined by
    /// newlines). Structured tags appear inline; the `tags` field is an overlay.
    pub body: String,
    /// The first paragraph of `body` (up to the first blank line), used for
    /// one-line symbol lists / hover summaries.
    pub summary: String,
    /// Recognized structured tags (an additive overlay on the Markdown body). An
    /// undocumented `@foo` is NOT collected here and renders literally in `body`.
    pub tags: Vec<DocTag>,
}

/// A recognized structured doc tag (spec §2): `@param`/`@returns`/`@example`/
/// `@deprecated`/`@see`. The `name` is the bareword after `@`; `arg` is the
/// optional first token (e.g. the param name for `@param name — text`); `text` is
/// the remaining free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocTag {
    pub name: String,
    pub arg: Option<String>,
    pub text: String,
}

/// The recognized structured tag names (spec §2). Anything else after `@` renders
/// literally as ordinary Markdown.
const KNOWN_TAGS: &[&str] = &["param", "returns", "example", "deprecated", "see"];

/// Tags that carry a leading bareword ARG before their free text (`@param name —
/// text`, `@see slug`). `@returns`/`@example`/`@deprecated` take only free text.
fn tag_takes_arg(name: &str) -> bool {
    matches!(name, "param" | "see")
}

/// Classify a `LineComment` token's text as doc or not, returning the doc payload
/// (the text after the `///` / `//!` marker, one leading space stripped) and
/// whether it is an inner (`//!`) doc. Returns `None` for ordinary comments
/// (`//`, `////`, …).
fn doc_line_payload(text: &str) -> Option<(DocStyle, String)> {
    // Determine the run of leading slashes.
    let slashes = text.chars().take_while(|&c| c == '/').count();
    if slashes == 3 {
        // `///` outer doc — but NOT `////` (4+ slashes is an ordinary comment).
        let rest = &text[3..];
        Some((DocStyle::Outer, strip_one_space(rest)))
    } else if slashes == 2 && text.as_bytes().get(2) == Some(&b'!') {
        // `//!` inner/module doc.
        let rest = &text[3..];
        Some((DocStyle::Inner, strip_one_space(rest)))
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocStyle {
    Outer,
    Inner,
}

/// Strip exactly ONE leading space (spec §2: "one leading space after `/// `
/// stripped") and any trailing newline/carriage-return left on the token text.
fn strip_one_space(s: &str) -> String {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let s = s.strip_suffix('\r').unwrap_or(s);
    s.strip_prefix(' ').unwrap_or(s).to_string()
}

/// Extract the doc-comment for `decl`, if any: the contiguous `///` `LineComment`
/// run immediately preceding the decl in the CST trivia stream, broken by a blank
/// line (≥ 2 `Newline` trivia). Returns `None` when there is no attached run.
///
/// Walks `prev_sibling_or_token` backward from `decl` (and, if `decl` is the first
/// child of an `ExportStmt`/wrapper, from that wrapper) collecting trivia.
pub fn doc_comment_run(decl: &ResolvedNode) -> Option<DocComment> {
    let lines = collect_outer_run(decl);
    if lines.is_empty() {
        return None;
    }
    Some(build_doc(&lines))
}

/// One element of the leading-trivia stream of a decl, classified.
enum TrivItem {
    Doc(String),
    OtherComment,
    Blank, // a blank line (≥ 2 consecutive newlines) boundary
}

/// The contiguous `///` outer-doc payload lines immediately preceding the decl's
/// first non-trivia token, in source order. The cstree tree builder attaches a
/// decl's leading trivia (the `///` lines, the `Newline`s, indentation) as the
/// decl node's FIRST token-children, before its first keyword/ident token — so we
/// walk the decl's own leading trivia FORWARD, then take the LAST contiguous `///`
/// run (a blank line = ≥ 2 `Newline`s breaks the run; `Whitespace` is ignored; a
/// `////`/`//`/`//!` line resets it). For an `export fn`, the trivia lives inside
/// the `ExportStmt` wrapper, so anchor on the wrapper.
fn collect_outer_run(start: &ResolvedNode) -> Vec<String> {
    let anchor = export_anchor(start);

    // v1 behavior note (review finding 4): a `///` on the SAME line as a preceding
    // statement (`fn a() {} /// x` then, with no blank line, `export fn b()`) is
    // attached to `b` as its leading trivia by the CST, so `x` becomes b's doc. This
    // is spec-literal-conformant (the §2 attachment rule is defined purely on the
    // trivia stream — a single `Newline` does not break a run) but can surprise. A
    // fix would detect a same-line preceding sibling statement; deferred as it is an
    // uncommon authoring pattern and the trivia-stream rule is the spec's SoT.

    // Build the leading-trivia item stream up to (and excluding) the first
    // non-trivia token, collapsing newline-runs into a single `Blank` boundary when
    // ≥ 2 in a row.
    let mut items: Vec<TrivItem> = Vec::new();
    let mut pending_newlines = 0usize;
    for el in anchor.children_with_tokens() {
        let Some(tok) = el.as_token() else {
            // A node child means we've passed the leading trivia (the first real
            // sub-node, e.g. a ParamList) — done.
            break;
        };
        match tok.kind() {
            SyntaxKind::Whitespace => { /* indentation — ignore */ }
            SyntaxKind::Newline => {
                pending_newlines += 1;
            }
            SyntaxKind::LineComment => {
                if pending_newlines >= 2 {
                    items.push(TrivItem::Blank);
                }
                pending_newlines = 0;
                match doc_line_payload(tok.text()) {
                    Some((DocStyle::Outer, payload)) => items.push(TrivItem::Doc(payload)),
                    _ => items.push(TrivItem::OtherComment),
                }
            }
            SyntaxKind::BlockComment => {
                if pending_newlines >= 2 {
                    items.push(TrivItem::Blank);
                }
                pending_newlines = 0;
                items.push(TrivItem::OtherComment);
            }
            // The first non-trivia token (FnKw, ClassKw, Ident, …): the leading
            // trivia ends here. A blank line between the last comment and the decl
            // breaks attachment.
            _ => {
                if pending_newlines >= 2 {
                    items.push(TrivItem::Blank);
                }
                break;
            }
        }
    }

    // The attached doc is the LAST contiguous `Doc` run — i.e. everything after the
    // final `Blank`/`OtherComment` boundary.
    let mut lines: Vec<String> = Vec::new();
    for item in items {
        match item {
            TrivItem::Doc(s) => lines.push(s),
            TrivItem::OtherComment | TrivItem::Blank => lines.clear(),
        }
    }
    lines
}

/// If `node` is the first child of an `ExportStmt`, return that wrapper (whose
/// leading trivia carries the doc run); otherwise return `node` unchanged. This
/// lets `doc_comment_run` accept either the wrapper or the inner decl.
fn export_anchor(node: &ResolvedNode) -> ResolvedNode {
    if let Some(parent) = node.parent() {
        if parent.kind() == SyntaxKind::ExportStmt {
            return parent.clone();
        }
    }
    node.clone()
}

/// Build the [`DocComment`] from the ordered payload lines.
fn build_doc(lines: &[String]) -> DocComment {
    let body = lines.join("\n");
    let summary = first_paragraph(lines);
    let tags = parse_tags(lines);
    DocComment {
        body,
        summary,
        tags,
    }
}

/// The first paragraph (lines up to the first blank payload line), trimmed.
fn first_paragraph(lines: &[String]) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        // A structured-tag line ends the prose summary (tags are an overlay, not
        // part of the one-line description).
        if is_tag_line(t) {
            break;
        }
        out.push(line.as_str());
    }
    out.join(" ").trim().to_string()
}

/// True if `trimmed` begins with a recognized structured tag (`@param`, …).
fn is_tag_line(trimmed: &str) -> bool {
    trimmed
        .strip_prefix('@')
        .map(|rest| {
            let name = rest.split(char::is_whitespace).next().unwrap_or("");
            KNOWN_TAGS.contains(&name)
        })
        .unwrap_or(false)
}

/// Parse the structured-tag overlay (spec §2). A line whose first non-space token
/// is `@param`/`@returns`/`@example`/`@deprecated`/`@see` becomes a [`DocTag`];
/// an undocumented `@foo` is ignored here (it renders literally in `body`).
fn parse_tags(lines: &[String]) -> Vec<DocTag> {
    let mut tags = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix('@') else {
            continue;
        };
        // `name` is the leading bareword; the remainder is its text.
        let mut it = rest.splitn(2, char::is_whitespace);
        let name = it.next().unwrap_or("").to_string();
        if !KNOWN_TAGS.contains(&name.as_str()) {
            continue;
        }
        let remainder = it.next().unwrap_or("").trim();
        let (arg, text) = if tag_takes_arg(&name) {
            // The first token is the arg (param name / see-slug); the rest, with a
            // leading `—`/`-` separator stripped, is the text.
            let mut parts = remainder.splitn(2, char::is_whitespace);
            let a = parts.next().unwrap_or("").to_string();
            let t = parts.next().unwrap_or("").trim();
            let t = t
                .strip_prefix('—')
                .or_else(|| t.strip_prefix('-'))
                .unwrap_or(t)
                .trim();
            (
                if a.is_empty() { None } else { Some(a) },
                t.to_string(),
            )
        } else {
            let t = remainder
                .strip_prefix('—')
                .or_else(|| remainder.strip_prefix('-'))
                .unwrap_or(remainder)
                .trim();
            (None, t.to_string())
        };
        tags.push(DocTag { name, arg, text });
    }
    tags
}

/// Extract the leading `//!` inner/module doc run at the top of `root` (the
/// `SourceFile`), if any — the contiguous `//!` run before the first non-trivia
/// token, broken by a blank line. Spec §2 ("`//!` at file/block top → module doc").
pub fn module_doc(root: &ResolvedNode) -> Option<DocComment> {
    // The file's leading trivia is attached as the FIRST decl node's leading
    // token-children (cstree attaches leading trivia to the following node), so we
    // descend to the first token of the tree and scan the trivia preceding it. The
    // `//!` run must be the FIRST content (before any `///` outer doc, which belongs
    // to the decl, and before any non-trivia token).
    let mut lines: Vec<String> = Vec::new();
    for el in root.descendants_with_tokens() {
        let Some(tok) = el.as_token() else { continue };
        match tok.kind() {
            SyntaxKind::Whitespace | SyntaxKind::Newline => {}
            SyntaxKind::LineComment => match doc_line_payload(tok.text()) {
                Some((DocStyle::Inner, payload)) => lines.push(payload),
                // An outer `///` or ordinary comment ends the module-doc region (an
                // outer doc belongs to the following decl).
                _ => break,
            },
            // Any non-trivia token ends the module-doc region.
            _ => break,
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(build_doc(&lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_to_tree;

    /// The first top-level decl child of the SourceFile (unwrapping `export`).
    fn first_decl(src: &str) -> ResolvedNode {
        let root = parse_to_tree(src);
        let child = root.children().next().expect("a top-level child");
        if child.kind() == SyntaxKind::ExportStmt {
            child.children().next().expect("export inner decl").clone()
        } else {
            child.clone()
        }
    }

    #[test]
    fn contiguous_run_attaches_both_lines() {
        let doc = doc_comment_run(&first_decl("/// a\n/// b\nfn f() {}\n")).expect("doc");
        assert_eq!(doc.body, "a\nb");
        assert_eq!(doc.summary, "a b");
    }

    #[test]
    fn blank_line_breaks_the_run() {
        // `/// a` then a BLANK line then `fn f` — attaches nothing.
        assert!(doc_comment_run(&first_decl("/// a\n\nfn f() {}\n")).is_none());
    }

    #[test]
    fn blank_line_keeps_only_the_near_run() {
        // The near `/// b` attaches; the far `/// a` (separated by a blank) does not.
        let doc = doc_comment_run(&first_decl("/// a\n\n/// b\nfn f() {}\n")).expect("doc");
        assert_eq!(doc.body, "b");
    }

    #[test]
    fn four_slashes_is_not_doc() {
        assert!(doc_comment_run(&first_decl("////x\nfn f() {}\n")).is_none());
    }

    #[test]
    fn ordinary_comment_is_not_doc() {
        assert!(doc_comment_run(&first_decl("// just a comment\nfn f() {}\n")).is_none());
    }

    #[test]
    fn one_leading_space_stripped() {
        // `///  two spaces` keeps ONE leading space (only one is stripped).
        let doc = doc_comment_run(&first_decl("///  two\nfn f() {}\n")).expect("doc");
        assert_eq!(doc.body, " two");
        // `/// one` → `one` (the single space stripped).
        let doc = doc_comment_run(&first_decl("/// one\nfn f() {}\n")).expect("doc");
        assert_eq!(doc.body, "one");
        // `///none` (no space) → `none`.
        let doc = doc_comment_run(&first_decl("///none\nfn f() {}\n")).expect("doc");
        assert_eq!(doc.body, "none");
    }

    #[test]
    fn indented_doc_lines_attach() {
        // Indentation (`Whitespace`) between newline and `///` is ignored.
        let src = "class C {\n  /// a method doc\n  fn m() {}\n}\n";
        let root = parse_to_tree(src);
        // Find the MethodDecl.
        let method = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::MethodDecl)
            .expect("method");
        let doc = doc_comment_run(method).expect("doc on indented method");
        assert_eq!(doc.body, "a method doc");
    }

    #[test]
    fn export_decl_doc_attaches() {
        // The run precedes the `export`, so we look up from the inner decl which is
        // the first child of the ExportStmt. The trivia is a sibling of the ExportStmt
        // wrapper, so walking from the inner decl must climb to the wrapper.
        let src = "/// exported\nexport fn f() {}\n";
        let root = parse_to_tree(src);
        let export = root.children().next().expect("export");
        let doc = doc_comment_run(export).expect("doc on export");
        assert_eq!(doc.body, "exported");
    }

    #[test]
    fn module_doc_at_top() {
        let src = "//! the module\n//! second line\nfn f() {}\n";
        let root = parse_to_tree(src);
        let doc = module_doc(&root).expect("module doc");
        assert_eq!(doc.body, "the module\nsecond line");
    }

    #[test]
    fn module_doc_absent_without_bang() {
        let src = "/// not module\nfn f() {}\n";
        let root = parse_to_tree(src);
        assert!(module_doc(&root).is_none());
    }

    #[test]
    fn structured_tags_parsed_as_overlay() {
        let src = "/// Adds two numbers.\n/// @param a — the first\n/// @returns — the sum\nfn add(a, b) {}\n";
        let doc = doc_comment_run(&first_decl(src)).expect("doc");
        assert_eq!(doc.summary, "Adds two numbers.");
        assert_eq!(doc.tags.len(), 2);
        assert_eq!(doc.tags[0].name, "param");
        assert_eq!(doc.tags[0].arg.as_deref(), Some("a"));
        assert_eq!(doc.tags[0].text, "the first");
        assert_eq!(doc.tags[1].name, "returns");
        assert_eq!(doc.tags[1].text, "the sum");
        // The body still carries the tag lines verbatim (overlay is additive).
        assert!(doc.body.contains("@param a"));
    }

    #[test]
    fn undocumented_tag_renders_literally() {
        let src = "/// @frobnicate something\nfn f() {}\n";
        let doc = doc_comment_run(&first_decl(src)).expect("doc");
        assert!(doc.tags.is_empty(), "unknown @tag is not collected");
        assert!(doc.body.contains("@frobnicate"), "renders literally");
    }

    #[test]
    fn summary_is_first_paragraph() {
        let src = "/// One line summary.\n///\n/// More detail in a second paragraph.\nfn f() {}\n";
        let doc = doc_comment_run(&first_decl(src)).expect("doc");
        assert_eq!(doc.summary, "One line summary.");
        assert!(doc.body.contains("More detail"));
    }
}
