# LSP Phase 4 — Advanced Editing & Workspace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the advanced-editing + workspace capability tier on top of the Phase 0 unified `SemanticModel` core: **documentColor / colorPresentation** (the extensible recognizer subsystem with context-gated string recognizers and the `#100` false-positive guard), **linkedEditingRange** (local identifiers), **codeLens** (run-`test`/run-`main` + reference counts) backed by `executeCommand`, **pull diagnostics** (`textDocument/diagnostic` + `workspace/diagnostic`), and the **file-operations / workspace** surface (`willRenameFiles`/`didRenameFiles` import rewrite, `didChangeWatchedFiles`, `didChangeConfiguration`/`workspace/configuration`, multi-root folders, work-done progress for initial indexing).

**Architecture:** Every new feature is a pure provider `fn(&SemanticModel, …) -> …` in `src/lsp/providers/<name>.rs` reading only the cached model (`tree`/`resolved`/`diagnostics`/`tokens`/`line_index`/`text`) — no provider imports `crate::{ast,lexer,parser,token}` (guard-tested in Phase 0). Color is a **recognizer registry**: an internal `Rgba { r, g, b, a }` plus a `Vec<Recognizer>` each yielding `(ByteSpan, Rgba)`; string-based recognizers gate on a **color-sink context registry** (argument positions of `color.*` / tui style) so `p.label == "#100"` never produces a swatch. Workspace-level features (pull `workspace/diagnostic`, `willRenameFiles` import rewrite) read the `RwLock<WorkspaceIndex>`; file-ops rewrite import specifiers via the index's `ImportEdge`/`import_specifier` machinery. The server wires each provider into a handler in `src/lsp/server.rs` and advertises it in `server_capabilities()`.

**Tech Stack:** Rust, `tower-lsp` (`lsp_types`), the `src/syntax/` CST front-end, `src/check/` analysis core, the Phase 0 `src/lsp/{model,convert,providers}` modules.

**Reference (read before starting):**
- Spec: `docs/superpowers/specs/2026-06-05-lsp-first-class-design.md` §4 (Color recognizer subsystem + color-sink context registry + `colorPresentation`), the `linkedEditingRange`/`codeLens`/pull-diagnostics/file-ops rows of §4's matrix, and §6 Phase 4.
- Phase 0 plan: `docs/superpowers/plans/2026-06-05-lsp-phase0-unification-foundation.md` (match the task/test/commit format; reuse `SemanticModel`, `DocumentStore`, `convert.rs`, `providers/`).
- Color API: `src/stdlib/color.rs` — `color.rgb(r,g,b,text)` / `color.bgRgb(r,g,b,text)` (truecolor; `exports()` lines 88-112; `want_u8` range `0..=255` lines 114-125).
- tui colors: `src/stdlib/tui.rs:778-848` `parse_color` — `[r,g,b]` integer arrays / name strings / `0..=255` index in `fg`/`bg` style fields.
- Workspace index: `src/lsp/workspace.rs` — `ImportEdge` (lines 67-73), `import_specifier` (lines 765-778), `resolve_specifier` (lines 629-643), `rename_edits` (lines 484-527), `byte_span_to_range` (lines 556-561), `canon` (lines 575-577), `discover_as_files` (lines 581-585).
- CST kinds: `src/syntax/kind.rs` — `CallExpr`/`ArgList`/`MemberExpr`/`ArrayExpr`/`Literal`/`Str`/`Number`/`NameRef`/`Ident`/`FnDecl`/`ClassDecl`/`EnumDecl` (lines 23-103); the authoritative CST-walk pattern is `src/lsp/workspace.rs` (`build_file_index`, `collect_uses`, `name_range_of`).
- Resolve: `src/syntax/resolve/types.rs` — `ResolveResult.uses: HashMap<TextRange, Resolution>`, `.bindings: Vec<Binding>` (`name`/`decl_range`); `crate::syntax::resolve::ident_text`.
- Diagnostics: `src/check/analyze.rs` (`analyze_with_config(src, &LintConfig) -> Analysis { diagnostics }`); `src/check/diagnostic.rs` (`AsDiagnostic`/`Severity`/`ByteSpan`). The model already exposes `SemanticModel::lsp_diagnostics()` (Phase 0 Task 5).
- Server: `src/lsp/server.rs` — `server_capabilities()` (Phase 0 form), `initialize`/`initialized` (roots + index warm), `url_to_canon`/`canon_to_url`, `reindex_uri`.

**Run the whole suite with:** `cargo test --lib lsp` (LSP unit tests) and `cargo test` (full). Clippy gate: `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` must be clean.

---

## File Structure

- Create `src/lsp/providers/color.rs` — the `Rgba` type, recognizer registry, color-sink context registry, `document_colors`, `color_presentations`.
- Create `src/lsp/providers/lens.rs` — `code_lenses` (run-`test`/run-`main` + ref-count) and `resolve_code_lens`.
- Create `src/lsp/providers/diagnostic.rs` — pull-diagnostics adapters off the model + workspace.
- Modify `src/lsp/providers/rename.rs` — add `linked_editing_ranges` (Phase 3 created this file for rename/prepareRename; if absent, create it here with the rename providers ported, but prefer extending the existing one).
- Modify `src/lsp/providers/mod.rs` — declare `pub mod color;`, `pub mod lens;`, `pub mod diagnostic;`.
- Modify `src/lsp/server.rs` — wire `document_color`/`color_presentation`, `linked_editing_range`, `code_lens`/`code_lens_resolve`, `diagnostic`/`workspace_diagnostic`, `execute_command`, `will_rename_files`/`did_rename_files`, `did_change_watched_files`, `did_change_configuration`; extend `server_capabilities()`; add work-done progress to `initialized`.
- Modify `src/lsp/server.rs` — add a `settings: RwLock<LspSettings>` field (carries `color.detectHexStringsEverywhere`).

---

## Task 1: `Rgba` + hex/functional color parsing primitives

**Files:**
- Create: `src/lsp/providers/color.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod color;`)
- Test: inline in `src/lsp/providers/color.rs`

- [ ] **Step 1: Declare the module**

In `src/lsp/providers/mod.rs` add alongside the existing `pub mod` lines:

```rust
pub mod color;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/providers/color.rs`:

```rust
//! `textDocument/documentColor` + `textDocument/colorPresentation`.
//!
//! An EXTENSIBLE recognizer subsystem (spec §4): an internal `Rgba`, a registry of
//! recognizers each yielding `(ByteSpan, Rgba)`, and a color-sink context registry
//! that gates string-based recognizers to argument positions of color-aware APIs
//! (`color.*` / tui style) so a plain label like `"#100"` never becomes a swatch.

/// 8-bit-per-channel RGBA. The LSP wire `Color` is f32 0..1, so alpha round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Rgba { r, g, b, a: 255 }
    }

    /// The LSP wire color (each channel 0.0..=1.0).
    pub fn to_lsp(self) -> tower_lsp::lsp_types::Color {
        tower_lsp::lsp_types::Color {
            red: self.r as f32 / 255.0,
            green: self.g as f32 / 255.0,
            blue: self.b as f32 / 255.0,
            alpha: self.a as f32 / 255.0,
        }
    }

    /// From an LSP wire color (rounded to nearest 0..=255).
    pub fn from_lsp(c: tower_lsp::lsp_types::Color) -> Self {
        let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgba {
            r: q(c.red),
            g: q(c.green),
            b: q(c.blue),
            a: q(c.alpha),
        }
    }
}

/// Parse a hex color string body (no leading `#`): `rgb`, `rgba`, `rrggbb`,
/// `rrggbbaa`. Returns `None` for any other shape (so `#abcde` is rejected).
pub fn parse_hex_body(body: &str) -> Option<Rgba> {
    let b = body.as_bytes();
    if !b.iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let hx = |s: &str| u8::from_str_radix(s, 16).ok();
    let dup = |c: char| {
        let s: String = std::iter::repeat(c).take(2).collect();
        u8::from_str_radix(&s, 16).ok()
    };
    match body.len() {
        3 => {
            let mut it = body.chars();
            Some(Rgba {
                r: dup(it.next()?)?,
                g: dup(it.next()?)?,
                b: dup(it.next()?)?,
                a: 255,
            })
        }
        4 => {
            let mut it = body.chars();
            Some(Rgba {
                r: dup(it.next()?)?,
                g: dup(it.next()?)?,
                b: dup(it.next()?)?,
                a: dup(it.next()?)?,
            })
        }
        6 => Some(Rgba {
            r: hx(&body[0..2])?,
            g: hx(&body[2..4])?,
            b: hx(&body[4..6])?,
            a: 255,
        }),
        8 => Some(Rgba {
            r: hx(&body[0..2])?,
            g: hx(&body[2..4])?,
            b: hx(&body[4..6])?,
            a: hx(&body[6..8])?,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_bodies_parse_all_shapes() {
        assert_eq!(parse_hex_body("f00"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_hex_body("ff0000"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_hex_body("00ff0080").unwrap().a, 0x80);
        assert_eq!(parse_hex_body("100"), Some(Rgba::rgb(0x11, 0x00, 0x00)));
        // Malformed shapes are rejected.
        assert_eq!(parse_hex_body("xyz"), None);
        assert_eq!(parse_hex_body("abcde"), None);
    }

    #[test]
    fn rgba_round_trips_through_lsp() {
        let c = Rgba { r: 10, g: 20, b: 30, a: 128 };
        assert_eq!(Rgba::from_lsp(c.to_lsp()), c);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails/compiles**

Run: `cargo test --lib lsp::providers::color`
Expected: compiles then PASS (pure). If `tower_lsp::lsp_types::Color` field names differ, confirm against the `tower-lsp` version in `Cargo.lock` (`red`/`green`/`blue`/`alpha` are the standard LSP names) and adjust.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib lsp::providers::color`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/lsp/providers/color.rs src/lsp/providers/mod.rs
git commit -m "feat(lsp): color Rgba + hex/rgba body parsing primitives"
```

---

## Task 2: Truecolor-call + tui-array recognizers (`color.rgb`/`color.bgRgb`, `[r,g,b]`)

These two recognizers are **value-position** (numeric literals) — they need no string-context gate.

**Files:**
- Modify: `src/lsp/providers/color.rs`
- Test: inline in `src/lsp/providers/color.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/color.rs`:

```rust
use crate::check::ByteSpan;
use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;

/// One detected color: the source span of the editable color token + its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorHit {
    pub span: ByteSpan,
    pub color: Rgba,
    /// How the source spells the color (drives format-preserving presentation).
    pub form: ColorForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorForm {
    /// `color.rgb(r,g,b,…)` / `color.bgRgb(…)` — edit the numeric args.
    RgbCall { bg: bool, args: ByteSpan },
    /// A `[r,g,b]` tui style array — edit the array literal.
    ArrayLiteral,
    /// A hex string literal (`"#rrggbb"`), span = the string token incl. quotes.
    HexString { quote: char },
    /// A functional string (`"rgb(...)"`/`"rgba(...)"`/`"hsl(...)"`).
    FunctionalString { quote: char },
}

/// The integer value of a `Number` literal node, if it is a whole 0..=255.
fn u8_literal(node: &ResolvedNode) -> Option<u8> {
    if node.kind() != SyntaxKind::Literal {
        return None;
    }
    let tok = node
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Number)?;
    let n: f64 = tok.text().parse().ok()?;
    if n.fract() != 0.0 || !(0.0..=255.0).contains(&n) {
        return None;
    }
    Some(n as u8)
}

/// Recognizer 1: `color.rgb(r,g,b,text)` / `color.bgRgb(...)` truecolor calls.
fn recognize_rgb_calls(model: &SemanticModel, out: &mut Vec<ColorHit>) {
    for call in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CallExpr)
    {
        // Callee must be a `MemberExpr` `color.rgb` / `color.bgRgb`.
        let Some(member) = call.children().find(|c| c.kind() == SyntaxKind::MemberExpr) else {
            continue;
        };
        let Some((recv, method)) = member_recv_method(&member) else {
            continue;
        };
        if recv != "color" {
            continue;
        }
        let bg = match method.as_str() {
            "rgb" => false,
            "bgRgb" => true,
            _ => continue,
        };
        let Some(args) = call.children().find(|c| c.kind() == SyntaxKind::ArgList) else {
            continue;
        };
        let nums: Vec<ResolvedNode> = args
            .children()
            .filter(|c| c.kind() == SyntaxKind::Literal)
            .collect();
        if nums.len() < 3 {
            continue;
        }
        let (Some(r), Some(g), Some(b)) =
            (u8_literal(&nums[0]), u8_literal(&nums[1]), u8_literal(&nums[2]))
        else {
            continue;
        };
        // Editable span = from the first numeric arg start to the third arg end.
        let args_span = ByteSpan {
            start: ByteSpan::from(nums[0].text_range()).start,
            end: ByteSpan::from(nums[2].text_range()).end,
        };
        out.push(ColorHit {
            span: args_span,
            color: Rgba::rgb(r, g, b),
            form: ColorForm::RgbCall { bg, args: args_span },
        });
    }
}

/// `recv.method` of a `MemberExpr`: the receiver `NameRef` text + the member ident.
fn member_recv_method(member: &ResolvedNode) -> Option<(String, String)> {
    let recv = member
        .children()
        .find(|c| c.kind() == SyntaxKind::NameRef)
        .and_then(|n| crate::syntax::resolve::ident_text(&n))?;
    // The member name is the trailing `Ident` token of the MemberExpr.
    let method = member
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .filter(|t| t.kind() == SyntaxKind::Ident)
        .next_back()?;
    Some((recv, method.text().to_string()))
}

/// Recognizer 2: a `[r,g,b]` integer-triple array literal.
fn recognize_rgb_arrays(model: &SemanticModel, out: &mut Vec<ColorHit>) {
    for arr in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ArrayExpr)
    {
        let elems: Vec<ResolvedNode> = arr
            .children()
            .filter(|c| c.kind() == SyntaxKind::Literal)
            .collect();
        if elems.len() != 3 {
            continue;
        }
        let (Some(r), Some(g), Some(b)) =
            (u8_literal(&elems[0]), u8_literal(&elems[1]), u8_literal(&elems[2]))
        else {
            continue;
        };
        // Only when EVERY child of the array is one of the three numeric literals
        // (no extra elements / spreads) — keeps it an unambiguous color triple.
        if arr
            .children()
            .filter(|c| c.kind() != SyntaxKind::Literal)
            .any(|_| true)
        {
            continue;
        }
        out.push(ColorHit {
            span: ByteSpan::from(arr.text_range()),
            color: Rgba::rgb(r, g, b),
            form: ColorForm::ArrayLiteral,
        });
    }
}

#[cfg(test)]
mod recognizer_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn detects_color_rgb_call() {
        let m = model("let s = color.rgb(255, 0, 0, \"x\")\n");
        let mut out = Vec::new();
        recognize_rgb_calls(&m, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].color, Rgba::rgb(255, 0, 0));
        assert!(matches!(out[0].form, ColorForm::RgbCall { bg: false, .. }));
        // The swatch span covers the three numeric channels.
        assert_eq!(&m.text[out[0].span.start..out[0].span.end], "255, 0, 0");
    }

    #[test]
    fn detects_bg_rgb_call() {
        let m = model("let s = color.bgRgb(0, 128, 255, \"x\")\n");
        let mut out = Vec::new();
        recognize_rgb_calls(&m, &mut out);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].form, ColorForm::RgbCall { bg: true, .. }));
    }

    #[test]
    fn detects_rgb_triple_array() {
        let m = model("let style = #{ fg: [10, 20, 30] }\n");
        let mut out = Vec::new();
        recognize_rgb_arrays(&m, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].color, Rgba::rgb(10, 20, 30));
    }

    #[test]
    fn ignores_non_color_array() {
        // A 4-element array is not an rgb triple.
        let m = model("let xs = [1, 2, 3, 4]\n");
        let mut out = Vec::new();
        recognize_rgb_arrays(&m, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }
}
```

> Confirm the object-literal syntax used in tests (`#{ ... }` vs `{ ... }`) against `examples/*.as` (a map literal is `#{...}`; an object literal is `{...}` — pick whichever the corpus uses for a tui style; the recognizer itself only walks `ArrayExpr`, so the wrapper does not matter to the assertion). If `MemberExpr`'s member token is not the last `Ident` (e.g. the grammar stores it as a distinct child node), adapt `member_recv_method` to `src/lsp/workspace.rs`'s member-walk if one exists, otherwise to the `MemberExpr` shape in `src/syntax/parser.rs`.

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::color`
Expected: PASS after adapting to the real `MemberExpr`/`Literal`/`Number` shapes.

- [ ] **Step 3: Commit**

```bash
git add src/lsp/providers/color.rs
git commit -m "feat(lsp): color recognizers — color.rgb/bgRgb calls + [r,g,b] tui arrays"
```

---

## Task 3: Color-sink context registry + gated hex/functional string recognizers (the `#100` guard)

This is the **zero-false-positive** core: string recognizers run ONLY when a string literal sits in an argument position of a color-aware API.

**Files:**
- Modify: `src/lsp/providers/color.rs`
- Test: inline in `src/lsp/providers/color.rs` (includes the `#100` label guard)

- [ ] **Step 1: Write the failing test (with the `#100` guard)**

Append to `src/lsp/providers/color.rs`:

```rust
/// A color-SINK descriptor: a `recv.method` whose given argument INDICES accept a
/// color string. Today: `color.rgb`/`color.bgRgb` (none — those take numbers, not
/// color strings) and the tui style fields are object-valued, so string color
/// SINKS are reserved for the coming CSS/HTML modules. The registry is the single
/// extension point: add a row, no recognizer change.
struct ColorSink {
    recv: &'static str,
    method: &'static str,
    /// Argument indices (0-based) that accept a color string.
    color_args: &'static [usize],
}

/// The color-sink registry. EMPTY of string-arg sinks today (the truecolor APIs
/// take numbers); it exists so a future `css.color("#fff")` adds one row here and
/// the hex-string recognizer lights up with no other change. Object-FIELD sinks
/// (tui `fg`/`bg`) are handled by the value-position array recognizer, not here.
const COLOR_SINKS: &[ColorSink] = &[];

/// Whether the string token at `span` sits in a registered color-sink argument
/// position. Walks up to the enclosing `CallExpr` and checks the callee + arg
/// index against `COLOR_SINKS`.
fn string_in_color_sink(model: &SemanticModel, str_node: &ResolvedNode) -> bool {
    // The string's enclosing `ArgList` and `CallExpr`.
    let Some(arglist) = ancestor(str_node, SyntaxKind::ArgList) else {
        return false;
    };
    let Some(call) = ancestor(&arglist, SyntaxKind::CallExpr) else {
        return false;
    };
    let Some(member) = call.children().find(|c| c.kind() == SyntaxKind::MemberExpr) else {
        return false;
    };
    let Some((recv, method)) = member_recv_method(&member) else {
        return false;
    };
    // The string's arg index = its position among the arglist's expression children.
    let target = ByteSpan::from(str_node.text_range());
    let mut idx = 0usize;
    for c in arglist.children().filter(|c| is_expr_node(c.kind())) {
        if ByteSpan::from(c.text_range()) == target
            || (target.start >= ByteSpan::from(c.text_range()).start
                && target.end <= ByteSpan::from(c.text_range()).end)
        {
            break;
        }
        idx += 1;
    }
    COLOR_SINKS.iter().any(|s| {
        s.recv == recv && s.method == method && s.color_args.contains(&idx)
    })
}

/// The nearest ancestor of `node` with `kind`.
fn ancestor(node: &ResolvedNode, kind: SyntaxKind) -> Option<ResolvedNode> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == kind {
            return Some(n.clone());
        }
        cur = n.parent();
    }
    None
}

/// Whether `kind` is an expression node (an arg slot). Mirror the set used by
/// `crate::check::rules::is_expr_kind` (re-use it if visible).
fn is_expr_node(kind: SyntaxKind) -> bool {
    crate::check::rules::is_expr_kind(kind)
}

/// The string body (unquoted) + quote char of a `Str` token node.
fn string_body(str_node: &ResolvedNode) -> Option<(String, char)> {
    let tok = str_node
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Str)?;
    let raw = tok.text();
    let q = raw.chars().next()?;
    if (q != '"' && q != '\'') || !raw.ends_with(q) || raw.len() < 2 {
        return None;
    }
    Some((raw[1..raw.len() - 1].to_string(), q))
}

/// Recognizer 3 + 4: hex (`#rgb`/`#rrggbb`/…) and functional (`rgb()`/`hsl()`)
/// strings — gated by the color-sink context UNLESS `detect_everywhere`.
fn recognize_color_strings(model: &SemanticModel, detect_everywhere: bool, out: &mut Vec<ColorHit>) {
    for s in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Literal)
    {
        let Some((body, quote)) = string_body(&s) else {
            continue;
        };
        if !detect_everywhere && !string_in_color_sink(model, &s) {
            continue; // THE #100 GUARD: an ungated label string is never a swatch.
        }
        let span = ByteSpan::from(s.text_range());
        if let Some(hex) = body.strip_prefix('#').and_then(parse_hex_body) {
            out.push(ColorHit { span, color: hex, form: ColorForm::HexString { quote } });
        } else if let Some(c) = parse_functional(&body) {
            out.push(ColorHit {
                span,
                color: c,
                form: ColorForm::FunctionalString { quote },
            });
        }
    }
}

/// Parse `rgb(r,g,b)` / `rgba(r,g,b,a)` / `hsl(h,s%,l%)` / `hsla(...)`. Returns
/// `None` for any other shape.
fn parse_functional(body: &str) -> Option<Rgba> {
    let body = body.trim();
    let (name, rest) = body.split_once('(')?;
    let inner = rest.strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    match name.trim() {
        "rgb" if parts.len() == 3 => Some(Rgba::rgb(u8c(parts[0])?, u8c(parts[1])?, u8c(parts[2])?)),
        "rgba" if parts.len() == 4 => Some(Rgba {
            r: u8c(parts[0])?,
            g: u8c(parts[1])?,
            b: u8c(parts[2])?,
            a: alpha_u8(parts[3])?,
        }),
        "hsl" if parts.len() == 3 => Some(hsl_to_rgba(deg(parts[0])?, pct(parts[1])?, pct(parts[2])?, 255)),
        "hsla" if parts.len() == 4 => {
            Some(hsl_to_rgba(deg(parts[0])?, pct(parts[1])?, pct(parts[2])?, alpha_u8(parts[3])?))
        }
        _ => None,
    }
}

fn u8c(s: &str) -> Option<u8> {
    let n: f64 = s.parse().ok()?;
    if n.fract() != 0.0 || !(0.0..=255.0).contains(&n) {
        return None;
    }
    Some(n as u8)
}
fn alpha_u8(s: &str) -> Option<u8> {
    let n: f64 = s.parse().ok()?;
    if !(0.0..=1.0).contains(&n) {
        return None;
    }
    Some((n * 255.0).round() as u8)
}
fn deg(s: &str) -> Option<f64> {
    s.trim_end_matches("deg").trim().parse().ok()
}
fn pct(s: &str) -> Option<f64> {
    let s = s.strip_suffix('%')?;
    let n: f64 = s.trim().parse().ok()?;
    Some(n / 100.0)
}

/// HSL (h in degrees, s/l in 0..1) → RGBA with the given alpha.
fn hsl_to_rgba(h: f64, s: f64, l: f64, a: u8) -> Rgba {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let q = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgba { r: q(r1), g: q(g1), b: q(b1), a }
}

#[cfg(test)]
mod string_context_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn label_hash_100_is_not_a_color() {
        // The `#100` false-positive case from the spec (a 3-digit hex SHAPE that is
        // really a label). Ungated, it must produce NO swatch.
        let m = model("let p = #{ label: \"#100\" }\n");
        let mut out = Vec::new();
        recognize_color_strings(&m, /*detect_everywhere=*/ false, &mut out);
        assert!(out.is_empty(), "a label string must not be a color: {out:?}");
    }

    #[test]
    fn detect_everywhere_opt_in_finds_hash_100() {
        let m = model("let p = #{ label: \"#100\" }\n");
        let mut out = Vec::new();
        recognize_color_strings(&m, /*detect_everywhere=*/ true, &mut out);
        assert_eq!(out.len(), 1, "opt-in detects the hex shape: {out:?}");
        assert_eq!(out[0].color, Rgba::rgb(0x11, 0x00, 0x00));
    }

    #[test]
    fn functional_rgb_string_parses() {
        assert_eq!(parse_functional("rgb(255, 0, 0)"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_functional("rgba(0, 0, 0, 0.5)").unwrap().a, 128);
        assert_eq!(parse_functional("hsl(0, 100%, 50%)"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_functional("nope(1,2)"), None);
    }
}
```

> The `#100` guard is the centerpiece test. Note: with `COLOR_SINKS` empty today, `string_in_color_sink` always returns `false`, so the GATED path finds nothing for any string — which is exactly correct (no string sink exists yet) and makes `label_hash_100_is_not_a_color` pass for the right reason. When CSS/HTML modules land, adding a `ColorSink` row turns the recognizer on without touching the `#100` guarantee for non-sink strings. If `ResolvedNode::parent()` is not the method name, confirm the red-tree API in `src/syntax/cst.rs` (cstree red nodes expose `.parent()`).

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::color`
Expected: PASS — the `#100` guard green.

- [ ] **Step 3: Commit**

```bash
git add src/lsp/providers/color.rs
git commit -m "feat(lsp): color-sink context registry + gated hex/functional string recognizers (#100 guard)"
```

---

## Task 4: `document_colors` registry driver + `color_presentations`

**Files:**
- Modify: `src/lsp/providers/color.rs`
- Test: inline in `src/lsp/providers/color.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/color.rs`:

```rust
use tower_lsp::lsp_types::{ColorInformation, ColorPresentation, TextEdit};

/// All detected colors in the document (the documentColor provider). Runs the
/// recognizer registry; `detect_hex_strings_everywhere` broadens the string
/// recognizers past the color-sink gate (default off).
pub fn document_colors(
    model: &SemanticModel,
    detect_hex_strings_everywhere: bool,
) -> Vec<ColorInformation> {
    let mut hits: Vec<ColorHit> = Vec::new();
    recognize_rgb_calls(model, &mut hits);
    recognize_rgb_arrays(model, &mut hits);
    recognize_color_strings(model, detect_hex_strings_everywhere, &mut hits);
    hits.into_iter()
        .map(|h| ColorInformation {
            range: crate::lsp::convert::byte_span_to_range(&model.text, &model.line_index, h.span),
            color: h.color.to_lsp(),
        })
        .collect()
}

/// Format-preserving presentations for the color picked at `range`, plus
/// cross-format alternatives. The FIRST presentation preserves the source form;
/// the rest offer hex6/hex8/rgb/rgba (and a `color.rgb(...)` arg edit for a call
/// form). The `form` is recovered by re-detecting the hit at `range`.
pub fn color_presentations(
    model: &SemanticModel,
    color: Rgba,
    range: tower_lsp::lsp_types::Range,
) -> Vec<ColorPresentation> {
    // Re-detect the hit covering `range` to know its source form.
    let target = range_to_byte_span(model, range);
    let form = find_form_at(model, target);
    let mut out: Vec<ColorPresentation> = Vec::new();
    let edit = |label: String, text: String| ColorPresentation {
        label: label.clone(),
        text_edit: Some(TextEdit { range, new_text: text }),
        additional_text_edits: None,
    };
    let hex6 = format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b);
    let hex8 = format!("#{:02x}{:02x}{:02x}{:02x}", color.r, color.g, color.b, color.a);
    let rgb = format!("rgb({}, {}, {})", color.r, color.g, color.b);
    let rgba = format!("rgba({}, {}, {}, {:.3})", color.r, color.g, color.b, color.a as f32 / 255.0);
    match form {
        Some(ColorForm::RgbCall { bg, .. }) => {
            // Edit the numeric args in place (the range is the args span).
            out.push(edit(
                format!("color.{}", if bg { "bgRgb" } else { "rgb" }),
                format!("{}, {}, {}", color.r, color.g, color.b),
            ));
        }
        Some(ColorForm::ArrayLiteral) => {
            out.push(edit("[r, g, b]".into(), format!("[{}, {}, {}]", color.r, color.g, color.b)));
        }
        Some(ColorForm::HexString { quote }) | Some(ColorForm::FunctionalString { quote }) => {
            let h = if color.a == 255 { &hex6 } else { &hex8 };
            out.push(edit(h.clone(), format!("{quote}{h}{quote}")));
            out.push(edit(rgb.clone(), format!("{quote}{rgb}{quote}")));
            out.push(edit(rgba.clone(), format!("{quote}{rgba}{quote}")));
        }
        None => {
            // Unknown form (range did not re-detect): offer bare hex.
            out.push(edit(hex6.clone(), hex6.clone()));
        }
    }
    out
}

fn range_to_byte_span(model: &SemanticModel, range: tower_lsp::lsp_types::Range) -> ByteSpan {
    let start = char_to_byte(&model.text, model.line_index.offset(range.start));
    let end = char_to_byte(&model.text, model.line_index.offset(range.end));
    ByteSpan { start, end }
}

fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(b, _)| b).unwrap_or(s.len())
}

fn find_form_at(model: &SemanticModel, target: ByteSpan) -> Option<ColorForm> {
    let mut hits = Vec::new();
    recognize_rgb_calls(model, &mut hits);
    recognize_rgb_arrays(model, &mut hits);
    recognize_color_strings(model, true, &mut hits);
    hits.into_iter()
        .find(|h| h.span == target || (h.span.start <= target.start && h.span.end >= target.end))
        .map(|h| h.form)
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn document_colors_finds_rgb_call() {
        let m = model("let s = color.rgb(255, 0, 0, \"x\")\n");
        let cs = document_colors(&m, false);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].color.red, 1.0);
    }

    #[test]
    fn presentation_for_rgb_call_edits_args() {
        let m = model("let s = color.rgb(255, 0, 0, \"x\")\n");
        let cs = document_colors(&m, false);
        let r = cs[0].range;
        let ps = color_presentations(&m, Rgba::rgb(0, 128, 255), r);
        assert!(!ps.is_empty());
        let te = ps[0].text_edit.as_ref().unwrap();
        assert_eq!(te.new_text, "0, 128, 255");
    }
}
```

> Confirm `ColorPresentation`'s field is `text_edit` / `additional_text_edits` (the `lsp_types` names) and `ColorInformation { range, color }`. `model.line_index.offset(Position) -> usize` (char offset) is the Phase 0 `LineIndex` API (`src/lsp/line_index.rs`).

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test --lib lsp::providers::color`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/lsp/providers/color.rs
git commit -m "feat(lsp): documentColor driver + format-preserving colorPresentation"
```

---

## Task 5: Wire documentColor / colorPresentation into the server (+ capability + setting)

**Files:**
- Modify: `src/lsp/server.rs` (handlers, `server_capabilities`, a `settings` field)
- Test: extend `tests/lsp.rs` capability assertion + a handler smoke test

- [ ] **Step 1: Add an `LspSettings` field carrying the color setting**

In `src/lsp/server.rs`, add near the `Backend` struct:

```rust
/// Client-configurable settings (subset). Updated by `didChangeConfiguration`.
#[derive(Debug, Clone, Default)]
pub struct LspSettings {
    /// `ascript.color.detectHexStringsEverywhere` — broaden hex-string color
    /// detection past the color-sink gate. Default `false`.
    pub detect_hex_strings_everywhere: bool,
}
```

Add the field to `Backend` + `Backend::new`:

```rust
    settings: RwLock<LspSettings>,
```
```rust
            settings: RwLock::new(LspSettings::default()),
```

- [ ] **Step 2: Advertise the capability**

In `server_capabilities()` add:

```rust
        color_provider: Some(ColorProviderCapability::Simple(true)),
```

- [ ] **Step 3: Implement the handlers**

Add to the `impl LanguageServer for Backend` block:

```rust
    async fn document_color(
        &self,
        params: DocumentColorParams,
    ) -> tower_lsp::jsonrpc::Result<Vec<ColorInformation>> {
        let uri = params.text_document.uri;
        let everywhere = self
            .settings
            .read()
            .map(|s| s.detect_hex_strings_everywhere)
            .unwrap_or(false);
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(Vec::new());
        };
        Ok(crate::lsp::providers::color::document_colors(model, everywhere))
    }

    async fn color_presentation(
        &self,
        params: ColorPresentationParams,
    ) -> tower_lsp::jsonrpc::Result<Vec<ColorPresentation>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(Vec::new());
        };
        let rgba = crate::lsp::providers::color::Rgba::from_lsp(params.color);
        Ok(crate::lsp::providers::color::color_presentations(model, rgba, params.range))
    }
```

Add the imports (`DocumentColorParams`, `ColorInformation`, `ColorPresentationParams`, `ColorPresentation`, `ColorProviderCapability`) to the `lsp_types` use list.

- [ ] **Step 4: Extend the protocol/capability test**

In `tests/lsp.rs`, in the capability smoke test, assert `caps.color_provider.is_some()`. Add a unit test in `server.rs` `#[cfg(test)]` (mirroring the existing `server_capabilities()` tests at the bottom of the file) asserting `server_capabilities().color_provider.is_some()`.

- [ ] **Step 5: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS.

```bash
git add src/lsp/server.rs tests/lsp.rs
git commit -m "feat(lsp): wire documentColor/colorPresentation + detectHexStringsEverywhere setting"
```

---

## Task 6: `linkedEditingRange` — same-file local identifier occurrences

**Files:**
- Modify: `src/lsp/providers/rename.rs` (Phase 3 owns rename/prepareRename here; if the file does not exist, create it and note it for Phase 3 reconciliation)
- Modify: `src/lsp/providers/mod.rs` (ensure `pub mod rename;`)
- Test: inline in `src/lsp/providers/rename.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/rename.rs` (create the file with this module doc if missing):

```rust
//! `textDocument/rename` family. `linked_editing_ranges` (Phase 4) returns the
//! same-file occurrences of the local identifier under the cursor so the editor
//! renames them live as one types. Tag-pairs are a documented future hook (HTML
//! templates) and are NOT implemented (spec §2 non-goals).

use crate::check::ByteSpan;
use crate::lsp::model::SemanticModel;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::Resolution;
use tower_lsp::lsp_types::Range;

/// The same-file ranges (decl + every use) of the LOCAL/UPVALUE binding the
/// identifier at byte `offset` resolves to. Returns `None` for a global, a member
/// access, an unresolved name, or when the cursor is not on an identifier — those
/// are not safe for live linked editing within one file.
pub fn linked_editing_ranges(model: &SemanticModel, offset: usize) -> Option<Vec<Range>> {
    // Find the NameRef (or binding decl) token under the cursor.
    let nameref = model.tree.descendants().find(|n| {
        n.kind() == SyntaxKind::NameRef && span_covers(ByteSpan::from(n.text_range()), offset)
    })?;
    let name = crate::syntax::resolve::ident_text(&nameref)?;
    let res = model.resolved.uses.get(&nameref.text_range())?;
    // Only LOCAL/UPVALUE identifiers are linked-edit safe (one frame, no imports).
    let slot = match res {
        Resolution::Local(s) | Resolution::Upvalue(s) => *s,
        _ => return None,
    };
    // The binding for this name+slot (nearest binding matching name; refine with
    // slot if the resolver disambiguates shadowing by slot).
    let binding = model
        .resolved
        .bindings
        .iter()
        .find(|b| b.name == name && b.slot == slot)?;
    let mut spans: Vec<ByteSpan> = vec![ByteSpan::from(binding.decl_range)];
    // Every use that resolves to the same Local/Upvalue slot with the same name.
    for nr in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NameRef)
    {
        if crate::syntax::resolve::ident_text(&nr).as_deref() != Some(name.as_str()) {
            continue;
        }
        match model.resolved.uses.get(&nr.text_range()) {
            Some(Resolution::Local(s) | Resolution::Upvalue(s)) if *s == slot => {
                spans.push(ByteSpan::from(nr.text_range()));
            }
            _ => {}
        }
    }
    spans.sort_by_key(|s| s.start);
    spans.dedup();
    Some(
        spans
            .into_iter()
            .map(|s| crate::lsp::convert::byte_span_to_range(&model.text, &model.line_index, s))
            .collect(),
    )
}

fn span_covers(span: ByteSpan, offset: usize) -> bool {
    offset >= span.start && offset < span.end
}

#[cfg(test)]
mod linked_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn links_local_let_occurrences() {
        let src = "fn f() {\n  let y = 1\n  return y + y\n}\n";
        let m = model(src);
        let off = src.find("let y").unwrap() + 4; // on the `y` of `let y`
        let ranges = linked_editing_ranges(&m, off).expect("linked ranges");
        // decl + two uses = 3 occurrences.
        assert_eq!(ranges.len(), 3, "{ranges:?}");
    }

    #[test]
    fn refuses_global_identifier() {
        let src = "fn top() {}\ntop()\n";
        let m = model(src);
        let off = src.rfind("top").unwrap();
        assert!(linked_editing_ranges(&m, off).is_none());
    }
}
```

> If the resolver's `uses` map does not key by the same `TextRange` as the `NameRef` node (it does per `collect_uses` in `workspace.rs`, which looks up `resolved.uses.get(&nameref.text_range())`), mirror that exact lookup. If shadowing makes name+slot ambiguous, scope the use scan to the binding's enclosing frame node — but the corpus's local linked-edit cases are single-frame, so name+slot is sufficient for the gate; document any refinement.

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test --lib lsp::providers::rename`
Expected: PASS.

- [ ] **Step 3: Wire the server handler + capability**

In `server_capabilities()` add:

```rust
        linked_editing_range_provider: Some(LinkedEditingRangeServerCapabilities::Simple(true)),
```

Add the handler:

```rust
    async fn linked_editing_range(
        &self,
        params: LinkedEditingRangeParams,
    ) -> tower_lsp::jsonrpc::Result<Option<LinkedEditingRanges>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = char_to_byte(&model.text, model.line_index.offset(position));
        Ok(crate::lsp::providers::rename::linked_editing_ranges(model, offset)
            .map(|ranges| LinkedEditingRanges { ranges, word_pattern: None }))
    }
```

Add a small `char_to_byte` helper to `server.rs` (or reuse `convert`'s if exported). Add the `lsp_types` imports.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/rename.rs src/lsp/providers/mod.rs src/lsp/server.rs
git commit -m "feat(lsp): linkedEditingRange — same-file local identifier occurrences"
```

---

## Task 7: `codeLens` — run-`test`/run-`main` + reference-count lenses

**Files:**
- Create: `src/lsp/providers/lens.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod lens;`)
- Test: inline in `src/lsp/providers/lens.rs`

- [ ] **Step 1: Write the failing test**

Create `src/lsp/providers/lens.rs`:

```rust
//! `textDocument/codeLens` (+ `codeLens/resolve`): a "▶ Run test" lens above each
//! `test("name", …)` registration, a "▶ Run" lens above `main`, and a reference-
//! count lens above each top-level declaration. The run lenses carry the
//! `ascript.runTest` / `ascript.run` commands (backed by `executeCommand`); the
//! count lens is resolved lazily via the workspace ref count.

use crate::check::ByteSpan;
use crate::lsp::model::SemanticModel;
use crate::syntax::kind::SyntaxKind;
use serde_json::json;
use tower_lsp::lsp_types::{CodeLens, Command, Range};

/// The (unresolved) lenses for `model`. The run lenses are fully resolved here;
/// the reference-count lenses carry `data` and are completed in `codeLens/resolve`.
pub fn code_lenses(model: &SemanticModel, uri: &str) -> Vec<CodeLens> {
    let mut out = Vec::new();
    // 1. Run-test lenses: top-level `test("name", fn)` calls.
    for call in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CallExpr)
    {
        let Some(callee) = call.children().find(|c| c.kind() == SyntaxKind::NameRef) else {
            continue;
        };
        if crate::syntax::resolve::ident_text(&callee).as_deref() != Some("test") {
            continue;
        }
        let Some(args) = call.children().find(|c| c.kind() == SyntaxKind::ArgList) else {
            continue;
        };
        let Some(name_lit) = args.children().find(|c| c.kind() == SyntaxKind::Literal) else {
            continue;
        };
        let Some(test_name) = string_literal_body(&name_lit) else {
            continue;
        };
        out.push(CodeLens {
            range: line_start_range(model, ByteSpan::from(call.text_range())),
            command: Some(Command {
                title: "▶ Run test".to_string(),
                command: "ascript.runTest".to_string(),
                arguments: Some(vec![json!(uri), json!(test_name)]),
            }),
            data: None,
        });
    }
    // 2. Run lens above a top-level `fn main`.
    for decl in model.tree.children().filter(|n| n.kind() == SyntaxKind::FnDecl) {
        if crate::syntax::resolve::ident_text(&decl).as_deref() == Some("main") {
            out.push(CodeLens {
                range: line_start_range(model, ByteSpan::from(decl.text_range())),
                command: Some(Command {
                    title: "▶ Run".to_string(),
                    command: "ascript.run".to_string(),
                    arguments: Some(vec![json!(uri)]),
                }),
                data: None,
            });
        }
    }
    // 3. Reference-count lens above each top-level decl (resolved lazily).
    for decl in model.tree.children() {
        if !matches!(
            decl.kind(),
            SyntaxKind::FnDecl | SyntaxKind::ClassDecl | SyntaxKind::EnumDecl
        ) {
            continue;
        }
        let Some(name) = crate::syntax::resolve::ident_text(&decl) else {
            continue;
        };
        out.push(CodeLens {
            range: line_start_range(model, ByteSpan::from(decl.text_range())),
            command: None, // unresolved
            data: Some(json!({ "kind": "refs", "uri": uri, "name": name })),
        });
    }
    out
}

/// Resolve a reference-count lens by counting same-file `NameRef` uses of its name.
/// (Cross-file counts are added by the server, which has the workspace index.)
pub fn resolve_same_file_ref_count(model: &SemanticModel, name: &str) -> usize {
    model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NameRef)
        .filter(|n| crate::syntax::resolve::ident_text(n).as_deref() == Some(name))
        .count()
}

fn string_literal_body(lit: &crate::syntax::cst::ResolvedNode) -> Option<String> {
    let tok = lit
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Str)?;
    let raw = tok.text();
    let q = raw.chars().next()?;
    if (q != '"' && q != '\'') || raw.len() < 2 || !raw.ends_with(q) {
        return None;
    }
    Some(raw[1..raw.len() - 1].to_string())
}

/// A zero-width range at the START of the line `span` begins on (lenses render
/// above the line).
fn line_start_range(model: &SemanticModel, span: ByteSpan) -> Range {
    let r = crate::lsp::convert::byte_span_to_range(&model.text, &model.line_index, span);
    Range { start: tower_lsp::lsp_types::Position { line: r.start.line, character: 0 }, end: r.start }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn run_test_lens_for_test_call() {
        let m = model("test(\"adds\", fn() { return 1 })\n");
        let lenses = code_lenses(&m, "file:///t.as");
        let run = lenses.iter().find(|l| {
            l.command.as_ref().map(|c| c.command.as_str()) == Some("ascript.runTest")
        });
        let cmd = run.expect("run-test lens").command.as_ref().unwrap();
        assert_eq!(cmd.arguments.as_ref().unwrap()[1], json!("adds"));
    }

    #[test]
    fn run_lens_for_main() {
        let m = model("fn main() {}\n");
        let lenses = code_lenses(&m, "file:///t.as");
        assert!(lenses
            .iter()
            .any(|l| l.command.as_ref().map(|c| c.command.as_str()) == Some("ascript.run")));
    }

    #[test]
    fn ref_count_lens_is_unresolved() {
        let m = model("fn helper() {}\nhelper()\nhelper()\n");
        let lenses = code_lenses(&m, "file:///t.as");
        let refs = lenses.iter().find(|l| l.data.is_some()).expect("a refs lens");
        assert!(refs.command.is_none(), "ref lens starts unresolved");
        // helper appears as decl name + 2 uses + ... count the NameRef uses.
        assert!(resolve_same_file_ref_count(&m, "helper") >= 2);
    }
}
```

> `serde_json` is already a dependency (used widely in the crate). Confirm `CodeLens`/`Command` field names against `lsp_types` (`title`/`command`/`arguments`, `range`/`command`/`data`). If `serde_json` is not in scope for the `lsp` cfg, add `use serde_json::json;` and confirm the dep is non-optional.

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test --lib lsp::providers::lens`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/lsp/providers/lens.rs src/lsp/providers/mod.rs
git commit -m "feat(lsp): codeLens provider — run-test/run-main + reference-count lenses"
```

---

## Task 8: Wire codeLens / codeLens-resolve / executeCommand into the server

**Files:**
- Modify: `src/lsp/server.rs` (handlers, capabilities)
- Test: extend `tests/lsp.rs` capability assertion + a unit test in `server.rs`

- [ ] **Step 1: Advertise the capabilities**

In `server_capabilities()` add:

```rust
        code_lens_provider: Some(CodeLensOptions { resolve_provider: Some(true) }),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![
                "ascript.run".to_string(),
                "ascript.runTest".to_string(),
            ],
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
```

> If Phase 1 already added an `execute_command_provider` for `source.fixAll`, EXTEND its `commands` vec with `ascript.run`/`ascript.runTest` rather than overwriting it.

- [ ] **Step 2: Implement the handlers**

```rust
    async fn code_lens(
        &self,
        params: CodeLensParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::lens::code_lenses(model, uri.as_str())))
    }

    async fn code_lens_resolve(
        &self,
        mut lens: CodeLens,
    ) -> tower_lsp::jsonrpc::Result<CodeLens> {
        // A refs lens carries `{ kind:"refs", uri, name }`; fill its title.
        let Some(data) = lens.data.clone() else {
            return Ok(lens);
        };
        let (Some(uri_s), Some(name)) = (
            data.get("uri").and_then(|v| v.as_str()).map(str::to_string),
            data.get("name").and_then(|v| v.as_str()).map(str::to_string),
        ) else {
            return Ok(lens);
        };
        let Ok(uri) = Url::parse(&uri_s) else { return Ok(lens) };
        // Same-file count from the cached model + cross-file from the index.
        let mut count = {
            let store = self.documents.lock().await;
            store
                .get(&uri)
                .map(|m| crate::lsp::providers::lens::resolve_same_file_ref_count(m, &name))
                .unwrap_or(0)
        };
        if let Some(path) = url_to_canon(&uri) {
            if let Ok(idx) = self.index.read() {
                // cross-file references to this name (importers' uses).
                count += idx
                    .references_at(&path, /*offset on decl*/ 0, false)
                    .len();
            }
            let _ = path;
        }
        lens.command = Some(Command {
            title: format!("{count} reference(s)"),
            command: String::new(),
            arguments: None,
        });
        Ok(lens)
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
        match params.command.as_str() {
            "ascript.run" | "ascript.runTest" => {
                // The server does not execute programs (static-only invariant). It
                // surfaces the command for the CLIENT to run `ascript run`/`test`;
                // here we just acknowledge + log the intent.
                self.client
                    .log_message(
                        MessageType::INFO,
                        format!("execute {}: {:?}", params.command, params.arguments),
                    )
                    .await;
                Ok(None)
            }
            other => {
                self.client
                    .log_message(MessageType::WARNING, format!("unknown command {other}"))
                    .await;
                Ok(None)
            }
        }
    }
```

> The cross-file count via `references_at(&path, 0, false)` is a placeholder; refine to anchor on the decl's name-range offset if a precise count is needed (look up the decl's `name_range.start` in `idx.files[path].defs`). Keep the same-file count authoritative for the gate test; the cross-file add is best-effort. The static-only invariant means the SERVER never runs the interpreter — `execute_command` only logs/acknowledges; the editor extension binds `ascript.run`/`ascript.runTest` to a terminal task (Phase 6). Document this.

- [ ] **Step 3: Capability test**

In `tests/lsp.rs` assert `caps.code_lens_provider.is_some()` and `caps.execute_command_provider.is_some()`. Add a unit test in `server.rs` `#[cfg(test)]`.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/server.rs tests/lsp.rs
git commit -m "feat(lsp): wire codeLens/codeLens-resolve/executeCommand (run/runTest backing)"
```

---

## Task 9: Pull diagnostics — `textDocument/diagnostic` + `workspace/diagnostic`

**Files:**
- Create: `src/lsp/providers/diagnostic.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod diagnostic;`)
- Modify: `src/lsp/server.rs` (handlers + capability)
- Test: inline in `src/lsp/providers/diagnostic.rs`

- [ ] **Step 1: Write the failing provider test**

Create `src/lsp/providers/diagnostic.rs`:

```rust
//! Pull diagnostics: `textDocument/diagnostic` (one document) and
//! `workspace/diagnostic` (project-wide). They return the SAME diagnostics as the
//! push path (`SemanticModel::lsp_diagnostics()`), so the editor sees one truth.

use crate::lsp::model::SemanticModel;
use tower_lsp::lsp_types::{
    Diagnostic, DocumentDiagnosticReportResult, FullDocumentDiagnosticReport,
    RelatedFullDocumentDiagnosticReport, DocumentDiagnosticReport,
};

/// The full document diagnostic report for `model` (config-aware, off the cache).
pub fn document_report(model: &SemanticModel) -> DocumentDiagnosticReportResult {
    let items: Vec<Diagnostic> = model.lsp_diagnostics();
    DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
        RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: None,
                items,
            },
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    #[test]
    fn document_report_matches_push_diagnostics() {
        let m = SemanticModel::build("let = 1\n".to_string(), None, &LintConfig::default());
        let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(r)) =
            document_report(&m)
        else {
            panic!("expected a full report");
        };
        assert_eq!(r.full_document_diagnostic_report.items, m.lsp_diagnostics());
        assert!(!r.full_document_diagnostic_report.items.is_empty());
    }
}
```

> Confirm the `lsp_types` pull-diagnostic type names against the `tower-lsp` version (these are the LSP 3.17 names: `DocumentDiagnosticReportResult`, `RelatedFullDocumentDiagnosticReport`, `FullDocumentDiagnosticReport`). If `tower-lsp` gates these behind a feature, check `Cargo.toml`'s `tower-lsp` features.

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test --lib lsp::providers::diagnostic`
Expected: PASS.

- [ ] **Step 3: Wire the server handlers + capability**

In `server_capabilities()` add:

```rust
        diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
            identifier: Some("ascript".to_string()),
            inter_file_dependencies: true,
            workspace_diagnostics: true,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
```

Add the handlers:

```rust
    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> tower_lsp::jsonrpc::Result<DocumentDiagnosticReportResult> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        match store.get(&uri) {
            Some(model) => Ok(crate::lsp::providers::diagnostic::document_report(model)),
            None => Ok(DocumentDiagnosticReportResult::Report(
                DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport::default()),
            )),
        }
    }

    async fn workspace_diagnostic(
        &self,
        _params: WorkspaceDiagnosticParams,
    ) -> tower_lsp::jsonrpc::Result<WorkspaceDiagnosticReportResult> {
        // Project-wide: every indexed file → a workspace full report. Build a model
        // per file off its cached text (config-aware via DocumentStore-style config).
        let mut items: Vec<WorkspaceDocumentDiagnosticReport> = Vec::new();
        let files: Vec<(PathBuf, String)> = {
            let idx = self.index.read().ok();
            match idx {
                Some(idx) => idx
                    .files
                    .iter()
                    .map(|(p, f)| (p.clone(), f.text.clone()))
                    .collect(),
                None => Vec::new(),
            }
        };
        for (path, text) in files {
            let Some(uri) = canon_to_url(&path) else { continue };
            let config = crate::lsp::model::config_for_path(&path);
            let model = SemanticModel::build(text, None, &config);
            items.push(WorkspaceDocumentDiagnosticReport::Full(
                WorkspaceFullDocumentDiagnosticReport {
                    uri,
                    version: None,
                    full_document_diagnostic_report: FullDocumentDiagnosticReport {
                        result_id: None,
                        items: model.lsp_diagnostics(),
                    },
                },
            ));
        }
        Ok(WorkspaceDiagnosticReportResult::Report(WorkspaceDiagnosticReport { items }))
    }
```

Add the `lsp_types` imports. Confirm `crate::lsp::model::config_for_path` is the Phase 0 public fn (Task 4 of Phase 0 exposes it).

- [ ] **Step 4: Capability test + run + commit**

In `tests/lsp.rs` assert `caps.diagnostic_provider.is_some()`.

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/diagnostic.rs src/lsp/providers/mod.rs src/lsp/server.rs tests/lsp.rs
git commit -m "feat(lsp): pull diagnostics — textDocument/diagnostic + workspace/diagnostic"
```

---

## Task 10: `willRenameFiles` — rewrite import specifiers pointing at the moved file

**Files:**
- Modify: `src/lsp/workspace.rs` (add a pure `import_rewrite_edits` method)
- Test: inline in `src/lsp/workspace.rs` (hermetic temp-dir fixture, mirroring the existing tests)

- [ ] **Step 1: Write the failing index test**

Append to `src/lsp/workspace.rs` (in the impl + tests):

```rust
    /// Compute the import-specifier rewrites needed when `old_path` is renamed to
    /// `new_path`: for every file that imports `old_path`, the byte range of its
    /// `from "<spec>"` string token + the NEW relative specifier. Returns
    /// `(importer_path, specifier_string_range, new_specifier)`. The new specifier
    /// is the importer-relative path to `new_path` WITHOUT the `.as` extension and
    /// WITH a leading `./` (mirroring `resolve_specifier`'s accepted forms).
    pub fn import_rewrite_edits(
        &self,
        old_path: &Path,
        new_path: &Path,
    ) -> Vec<(PathBuf, ByteSpan, String)> {
        let old = canonicalize(old_path);
        let mut out = Vec::new();
        let Some(importers) = self.importers.get(&old) else {
            return out;
        };
        for imp in importers {
            let Some(file) = self.files.get(imp) else { continue };
            let importer_dir = imp.parent().map(Path::to_path_buf).unwrap_or_default();
            let new_spec = relative_specifier(&importer_dir, new_path);
            // Re-parse the importer to find the import statement whose resolved
            // target is `old`, and the byte range of its `from "<spec>"` STRING.
            let parsed = crate::syntax::parser::parse(&file.text);
            if !parsed.errors.is_empty() {
                continue;
            }
            let tree = crate::syntax::tree_builder::build_tree(parsed);
            for import in tree
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::ImportStmt)
            {
                let Some(spec) = import_specifier(&import) else { continue };
                if resolve_specifier(&spec, &importer_dir).as_deref() != Some(old.as_path()) {
                    continue;
                }
                // The string TOKEN range, INNER (between the quotes).
                if let Some(tok) = import
                    .children_with_tokens()
                    .filter_map(|el| el.into_token().cloned())
                    .find(|t| t.kind() == SyntaxKind::Str)
                {
                    let r = ByteSpan::from(tok.text_range());
                    // Inner range excludes the surrounding quotes.
                    let inner = ByteSpan { start: r.start + 1, end: r.end - 1 };
                    out.push((imp.clone(), inner, new_spec.clone()));
                }
            }
        }
        out
    }
```

Add the helper:

```rust
/// The importer-relative specifier for `target` (a leading `./`/`../`, no `.as`).
fn relative_specifier(importer_dir: &Path, target: &Path) -> String {
    let target = canonicalize(target);
    let target_noext = {
        let mut t = target.clone();
        if t.extension().and_then(|e| e.to_str()) == Some("as") {
            t.set_extension("");
        }
        t
    };
    let rel = pathdiff_lexical(importer_dir, &target_noext);
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.starts_with("./") || s.starts_with("../") {
        s
    } else {
        format!("./{s}")
    }
}

/// A lexical relative path from `base` to `target` (no fs access), enough for
/// sibling/`../` import rewrites.
fn pathdiff_lexical(base: &Path, target: &Path) -> PathBuf {
    let base: Vec<_> = base.components().collect();
    let targ: Vec<_> = target.components().collect();
    let common = base.iter().zip(&targ).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for c in &targ[common..] {
        out.push(c.as_os_str());
    }
    out
}
```

Add the test:

```rust
    #[test]
    fn import_rewrite_on_move_points_at_new_path() {
        // a.as is imported by b.as via "./a"; moving a.as → lib/a.as rewrites b's
        // specifier to "./lib/a".
        let a = (PathBuf::from("/ws/a.as"), "export fn f() {}\n".to_string());
        let b = (
            PathBuf::from("/ws/b.as"),
            "import { f } from \"./a\"\nf()\n".to_string(),
        );
        let idx = WorkspaceIndex::build_from_files(&[a, b]);
        let edits = idx.import_rewrite_edits(
            &PathBuf::from("/ws/a.as"),
            &PathBuf::from("/ws/lib/a.as"),
        );
        assert_eq!(edits.len(), 1, "{edits:?}");
        let (importer, range, new_spec) = &edits[0];
        assert_eq!(importer, &PathBuf::from("/ws/b.as"));
        assert_eq!(new_spec, "./lib/a");
        // The range is the inner specifier (between the quotes).
        let b_text = "import { f } from \"./a\"\nf()\n";
        assert_eq!(&b_text[range.start..range.end], "./a");
    }
```

> `import_specifier`/`resolve_specifier`/`canonicalize` are already in `workspace.rs`. If a `pathdiff` crate is already a dependency, use it instead of `pathdiff_lexical`; otherwise the lexical helper avoids a new dep (the LSP must stay `--no-default-features`-buildable).

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test --lib lsp::workspace::tests::import_rewrite_on_move_points_at_new_path`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/lsp/workspace.rs
git commit -m "feat(lsp): workspace import_rewrite_edits — rewrite specifiers on file move"
```

---

## Task 11: Wire willRenameFiles / didRenameFiles into the server

**Files:**
- Modify: `src/lsp/server.rs` (handlers + capability)
- Test: extend `tests/lsp.rs` capability assertion

- [ ] **Step 1: Advertise the file-operations capability**

In `server_capabilities()`, populate the `workspace` field (create or extend the `WorkspaceServerCapabilities`):

```rust
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                will_rename: Some(FileOperationRegistrationOptions {
                    filters: vec![FileOperationFilter {
                        scheme: Some("file".to_string()),
                        pattern: FileOperationPattern {
                            glob: "**/*.as".to_string(),
                            matches: Some(FileOperationPatternKind::File),
                            options: None,
                        },
                    }],
                }),
                did_rename: Some(FileOperationRegistrationOptions {
                    filters: vec![FileOperationFilter {
                        scheme: Some("file".to_string()),
                        pattern: FileOperationPattern {
                            glob: "**/*.as".to_string(),
                            matches: Some(FileOperationPatternKind::File),
                            options: None,
                        },
                    }],
                }),
                ..WorkspaceFileOperationsServerCapabilities::default()
            }),
        }),
```

> If Phase 0/3 already set `workspace`, EXTEND it (keep `workspace_folders`). Multi-root `workspace_folders.supported = true` covers the spec's "multi-root" line.

- [ ] **Step 2: Implement `will_rename_files`**

```rust
    async fn will_rename_files(
        &self,
        params: RenameFilesParams,
    ) -> tower_lsp::jsonrpc::Result<Option<WorkspaceEdit>> {
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        let idx = self.index.read().ok();
        let Some(idx) = idx else { return Ok(None) };
        for f in &params.files {
            let (Ok(old), Ok(new)) = (
                Url::parse(&f.old_uri).and_then(|u| u.to_file_path().map_err(|_| url_parse_err())),
                Url::parse(&f.new_uri).and_then(|u| u.to_file_path().map_err(|_| url_parse_err())),
            ) else {
                continue;
            };
            for (importer, range, new_spec) in idx.import_rewrite_edits(&old, &new) {
                let Some(uri) = canon_to_url(&importer) else { continue };
                let Some(text) = idx.files.get(&workspace::canon(&importer)).map(|f| f.text.clone())
                else {
                    continue;
                };
                changes.entry(uri).or_default().push(TextEdit {
                    range: workspace::byte_span_to_range(&text, range),
                    new_text: new_spec,
                });
            }
        }
        if changes.is_empty() {
            return Ok(None);
        }
        Ok(Some(WorkspaceEdit { changes: Some(changes), ..WorkspaceEdit::default() }))
    }

    async fn did_rename_files(&self, params: RenameFilesParams) {
        // Re-key the index: drop the old path, index the new one.
        for f in &params.files {
            if let (Ok(old), Ok(new)) = (
                Url::parse(&f.old_uri).and_then(|u| u.to_file_path().map_err(|_| url_parse_err())),
                Url::parse(&f.new_uri).and_then(|u| u.to_file_path().map_err(|_| url_parse_err())),
            ) {
                if let Ok(mut idx) = self.index.write() {
                    idx.files.remove(&workspace::canon(&old));
                    if let Ok(text) = std::fs::read_to_string(&new) {
                        idx.reindex_file(&new, &text);
                    }
                }
            }
        }
    }
```

Add a tiny `url_parse_err()` helper returning a `tower_lsp::jsonrpc::Error` (or use `.ok()`/`let-else` to avoid the `map_err` dance — prefer whichever compiles cleanly; the `to_file_path()` returns `Result<_, ()>`, so `.ok()` + `let-else` is simplest). Add the `lsp_types` imports.

- [ ] **Step 3: Capability test + run + commit**

In `tests/lsp.rs` assert the `workspace.file_operations.will_rename` capability is present.

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/server.rs tests/lsp.rs
git commit -m "feat(lsp): willRenameFiles/didRenameFiles — import-rewrite + index re-key on move"
```

---

## Task 12: `didChangeWatchedFiles` + `didChangeConfiguration` (reindex / reconfig)

**Files:**
- Modify: `src/lsp/server.rs` (handlers + dynamic registration of the watcher in `initialized`)
- Test: unit test in `server.rs` for the settings parse

- [ ] **Step 1: Implement `did_change_watched_files`**

```rust
    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in &params.changes {
            let Ok(path) = change.uri.to_file_path() else { continue };
            let is_as = path.extension().and_then(|e| e.to_str()) == Some("as");
            let is_toml = path.file_name().and_then(|n| n.to_str()) == Some("ascript.toml");
            if is_as {
                match change.typ {
                    FileChangeType::DELETED => {
                        if let Ok(mut idx) = self.index.write() {
                            idx.files.remove(&workspace::canon(&path));
                        }
                    }
                    _ => {
                        if let Ok(text) = std::fs::read_to_string(&path) {
                            if let Ok(mut idx) = self.index.write() {
                                idx.reindex_file(&path, &text);
                            }
                        }
                    }
                }
            } else if is_toml {
                // Config changed: re-publish diagnostics for all open documents
                // (their lint config may have changed). Rebuild each cached model.
                self.republish_all_open().await;
            }
        }
    }
```

Add a `republish_all_open` helper on `Backend` that, for each open URI in the store, rebuilds its model (`store.set(uri, text, version)` re-discovers config) and re-publishes diagnostics.

- [ ] **Step 2: Implement `did_change_configuration`**

```rust
    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        // Parse the `ascript.color.detectHexStringsEverywhere` setting.
        if let Some(detect) = params
            .settings
            .get("ascript")
            .and_then(|a| a.get("color"))
            .and_then(|c| c.get("detectHexStringsEverywhere"))
            .and_then(|v| v.as_bool())
        {
            if let Ok(mut s) = self.settings.write() {
                s.detect_hex_strings_everywhere = detect;
            }
        }
    }
```

- [ ] **Step 3: Register the watcher dynamically in `initialized`**

In `initialized`, after warming the index, register a file watcher so the client sends `didChangeWatchedFiles`:

```rust
        let watchers = vec![
            FileSystemWatcher { glob_pattern: GlobPattern::String("**/*.as".to_string()), kind: None },
            FileSystemWatcher {
                glob_pattern: GlobPattern::String("**/ascript.toml".to_string()),
                kind: None,
            },
        ];
        let registration = Registration {
            id: "ascript-watch".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                watchers,
            })
            .ok(),
        };
        let _ = self.client.register_capability(vec![registration]).await;
```

- [ ] **Step 4: Write a settings-parse unit test**

In `server.rs` `#[cfg(test)]`, test that a `serde_json::json!({"ascript":{"color":{"detectHexStringsEverywhere":true}}})` value parses to `true` via the same accessor chain used in `did_change_configuration` (factor the accessor into a small `parse_detect_setting(&serde_json::Value) -> Option<bool>` fn and test it directly).

- [ ] **Step 5: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): didChangeWatchedFiles + didChangeConfiguration (reindex/reconfig + color setting)"
```

---

## Task 13: Multi-root + work-done progress for initial indexing

**Files:**
- Modify: `src/lsp/server.rs` (`initialized` progress, `did_change_workspace_folders`)
- Test: unit test that indexing covers multiple roots

- [ ] **Step 1: Emit work-done progress around the initial index walk**

In `initialized`, wrap the index warm in a progress report:

```rust
        let token = NumberOrString::String("ascript-index".to_string());
        let _ = self
            .client
            .send_request::<tower_lsp::lsp_types::request::WorkDoneProgressCreate>(
                WorkDoneProgressCreateParams { token: token.clone() },
            )
            .await;
        self.client
            .send_notification::<tower_lsp::lsp_types::notification::Progress>(ProgressParams {
                token: token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                    WorkDoneProgressBegin {
                        title: "Indexing AScript workspace".to_string(),
                        cancellable: Some(false),
                        message: None,
                        percentage: None,
                    },
                )),
            })
            .await;
        // ... existing walk + reindex_file loop ...
        self.client
            .send_notification::<tower_lsp::lsp_types::notification::Progress>(ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some(format!("{} files indexed", files.len())),
                })),
            })
            .await;
```

> Confirm the `tower-lsp` `Client` progress API. If `send_request::<WorkDoneProgressCreate>` is not available, fall back to `self.client.progress(token, title)` if `tower-lsp` exposes a `progress` helper (check the version). The progress is best-effort; degrade gracefully (ignore send errors) so a client without progress support is unaffected.

- [ ] **Step 2: Handle `did_change_workspace_folders` (multi-root add/remove)**

```rust
    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        if let Ok(mut roots) = self.roots.write() {
            for added in &params.event.added {
                if let Ok(p) = added.uri.to_file_path() {
                    if !roots.contains(&p) {
                        roots.push(p);
                    }
                }
            }
            for removed in &params.event.removed {
                if let Ok(p) = removed.uri.to_file_path() {
                    roots.retain(|r| r != &p);
                }
            }
        }
        // Re-warm the index over the new root set.
        let roots = self.roots.read().map(|r| r.clone()).unwrap_or_default();
        if let Ok(mut idx) = self.index.write() {
            for root in &roots {
                for path in workspace::discover_as_files(root) {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        idx.reindex_file(&path, &text);
                    }
                }
            }
        }
    }
```

- [ ] **Step 3: Write the multi-root unit test**

In `server.rs` `#[cfg(test)]`, construct a `WorkspaceIndex`, reindex files under two distinct temp-dir roots, and assert `idx.files` contains entries from BOTH roots (a thin index-level test; the protocol path is covered by `tests/lsp.rs`). This exercises that `discover_as_files` + `reindex_file` compose across roots.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): multi-root workspace folders + work-done progress for initial indexing"
```

---

## Task 14: Color zero-false-positive corpus gate

The spec (§7.2) requires a corpus zero-FP gate for noise-capable providers. Pin documentColor against the whole `examples/**` corpus + the explicit `#100` case.

**Files:**
- Modify: `src/lsp/providers/color.rs` (a corpus test) OR a new `tests/lsp_color.rs`
- Test: as above

- [ ] **Step 1: Write the corpus gate test**

Append a test that builds a model from each `examples/*.as` (and `examples/advanced/*.as`) and asserts `document_colors(&model, /*everywhere=*/ false)` only ever yields colors at REAL color forms — concretely, that no detected color span overlaps a string literal that is NOT in a color sink. The simplest robust assertion: for the specific spec case, include `examples/typed_fields.as` (which contains the `p.label == "#100"` comparison) and assert `document_colors` returns no swatch whose source text is `"#100"`:

```rust
#[test]
fn corpus_no_false_positive_color_swatches() {
    use crate::check::LintConfig;
    let mut roots = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")];
    roots.push(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/advanced"));
    for dir in roots {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("as") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let model = SemanticModel::build(src.clone(), None, &LintConfig::default());
            for c in document_colors(&model, false) {
                // No swatch may land on a `"#..."` label string (the #100 class).
                let span = range_to_byte_span(&model, c.range);
                let text = &src[span.start..span.end.min(src.len())];
                assert!(
                    !text.starts_with("\"#") && !text.starts_with("'#"),
                    "false-positive color swatch on label {text:?} in {:?}",
                    path
                );
            }
        }
    }
}
```

> If `examples/typed_fields.as` does not contain `#100`, this still asserts the invariant (no string-literal swatch escapes the gate) across the whole corpus, which is the real guarantee. Keep `range_to_byte_span` visible to the test (it already exists in this module).

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib lsp::providers::color::corpus_no_false_positive_color_swatches`
Expected: PASS (no false positives).

```bash
git add src/lsp/providers/color.rs
git commit -m "test(lsp): color zero-false-positive corpus gate (#100 label guard over examples/**)"
```

---

## Phase 4 Done — Gate

- [ ] **documentColor / colorPresentation** live: truecolor calls + `[r,g,b]` arrays + gated hex/functional strings; `colorPresentation` is format-preserving with cross-format choices; `color_provider` advertised; `detectHexStringsEverywhere` setting wired.
- [ ] **Color round-trip + `#100` false-positive guard** green (`label_hash_100_is_not_a_color` + the corpus zero-FP gate).
- [ ] **linkedEditingRange** returns same-file local identifier occurrences; refuses globals/members; `linked_editing_range_provider` advertised.
- [ ] **codeLens** shows run-`test`/run-`main` + reference-count lenses; `codeLens/resolve` fills counts; `executeCommand` (`ascript.run`/`ascript.runTest`) acknowledges without running code (static-only invariant intact).
- [ ] **Pull diagnostics** (`textDocument/diagnostic` + `workspace/diagnostic`) return the same diagnostics as the push path; `diagnostic_provider` advertised.
- [ ] **File operations**: `willRenameFiles` rewrites importer specifiers (import-rewrite-on-rename test green); `didRenameFiles` re-keys the index; `didChangeWatchedFiles` (`.as` + `ascript.toml`) reindexes/reconfigs; `didChangeConfiguration` updates the color setting.
- [ ] **Multi-root** workspace folders supported (add/remove re-warms the index); **work-done progress** wraps the initial index walk.
- [ ] No provider imports `crate::{ast,lexer,parser,token}` (Phase 0 guard test still green).
- [ ] `cargo test`, `cargo test --no-default-features`, and BOTH clippy configs (`cargo clippy --all-targets` + `cargo clippy --no-default-features --all-targets`) are green/clean.

**Next plan:** `docs/superpowers/plans/2026-06-05-lsp-phase5-grammar-promotion.md` (standalone `tree-sitter-ascript` + full query set + conformance/drift guard).
