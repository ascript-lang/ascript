//! SIG §2.3 drift (b): the docs pages and the curated table may never contradict.
//!
//! Three tests:
//!   1. `docs_and_table_never_contradict` — every parsed doc fact is consistent with the table.
//!   2. `style1_modules_are_fully_documented` — every Fn member of a Style-1 module has a doc fact.
//!   3. `comparator_detects_a_contradiction` — anti-false-green: a deliberate mutation trips the comparator.

use ascript::check::std_sigs::{self, MemberKind};

// ─────────────────────────────────────────────────────────────────────────────
// Data types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DocParam {
    name: String,
    optional: bool,
    variadic: bool,
    ty: Option<String>,
}

#[derive(Debug, Clone)]
struct DocFact {
    page: String,
    line: usize,
    /// The raw module prefix from the heading (e.g. "string", "tcp", "net").
    module_prefix: String,
    func: String,
    params: Vec<DocParam>,
    ret: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Module alias table for net.md sub-module prefixes
// ─────────────────────────────────────────────────────────────────────────────

/// Map a doc-heading module prefix → canonical `std/*` module path.
fn resolve_module(prefix: &str) -> String {
    match prefix {
        "tcp" => "std/net/tcp",
        "udp" => "std/net/udp",
        "unix" => "std/net/unix",
        "ws" => "std/net/ws",
        "http" => "std/net/http",
        "net" => "std/net",
        // Everything else is just std/<prefix>
        other => return format!("std/{other}"),
    }
    .to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Parser
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a single docs page, extracting Style-1 and Style-2 facts.
fn parse_page(page: &str, text: &str) -> Vec<DocFact> {
    let mut facts: Vec<DocFact> = Vec::new();
    let mut current: Option<DocFact> = None;
    let mut in_code_block = false;

    for (lineno, raw_line) in text.lines().enumerate() {
        let lineno = lineno + 1;
        let line = raw_line.trim_end();

        // Track code blocks so we don't parse backtick headings inside them.
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }

        // ── Any heading terminates an open fact (h1/h2/h5/h6 close without starting a new one).
        if line.starts_with('#') && !line.starts_with("### ") && !line.starts_with("#### ") {
            if let Some(f) = current.take() {
                facts.push(f);
            }
            continue;
        }

        // ── Style-1 heading: `### module.fn` or `#### module.fn` ─────────────
        if let Some(heading) = line.strip_prefix("### ").or_else(|| line.strip_prefix("#### ")) {
            // Must be plain `word.word` with no backticks or parens.
            let heading = heading.trim();
            if !heading.starts_with('`') && !heading.contains('(') {
                if let Some((module_part, func_part)) = heading.split_once('.') {
                    let module_part = module_part.trim();
                    let func_part = func_part.trim();
                    // Both parts must be simple identifiers (alpha/underscore/digit).
                    if is_ident(module_part) && is_ident(func_part) {
                        // Commit the previous fact.
                        if let Some(f) = current.take() {
                            facts.push(f);
                        }
                        current = Some(DocFact {
                            page: page.to_string(),
                            line: lineno,
                            module_prefix: module_part.to_string(),
                            func: func_part.to_string(),
                            params: Vec::new(),
                            ret: None,
                        });
                        continue;
                    }
                }
            }

            // ── Style-2 heading: `### `module.fn(args)` ` ───────────────────
            if let Some(inner) = parse_style2_heading(heading) {
                // Commit the previous fact.
                if let Some(f) = current.take() {
                    facts.push(f);
                }
                current = Some(inner.with_page(page, lineno));
                continue;
            }

            // Any other `###` heading terminates the current fact collection.
            if let Some(f) = current.take() {
                facts.push(f);
            }
            continue;
        }

        // ── Body lines — only relevant when inside a fact ────────────────────
        let Some(ref mut fact) = current else { continue };

        // Style-1 param bullet: `- name: type — desc` or `- ...name: type — desc`
        if let Some(rest) = line.strip_prefix("- ") {
            // Ignore sub-list items indented beyond the top level.
            if rest.starts_with("  ") || rest.starts_with('\t') {
                continue;
            }
            // Returns line:
            if let Some(ret_type) = try_parse_returns(rest) {
                fact.ret = Some(ret_type);
                continue;
            }
            // Param bullet — may be multi-param comma-separated like:
            //   `a: int`, `b: int` (`b != 0`)
            // Split by `, `` ` to handle this case.
            let parsed = parse_param_line(rest);
            fact.params.extend(parsed);
        }
    }

    // Commit the final fact.
    if let Some(f) = current {
        facts.push(f);
    }

    facts
}

/// A Style-2 fact before page/line are filled in.
struct Style2Raw {
    module_prefix: String,
    func: String,
    params: Vec<DocParam>,
}

impl Style2Raw {
    fn with_page(self, page: &str, line: usize) -> DocFact {
        DocFact {
            page: page.to_string(),
            line,
            module_prefix: self.module_prefix,
            func: self.func,
            params: self.params,
            ret: None, // Style-2 yields no type info
        }
    }
}

/// Try to parse a Style-2 heading: `` `module.fn(arg list)` `` (optional trailing text after).
/// Returns None if the heading doesn't match.
fn parse_style2_heading(heading: &str) -> Option<Style2Raw> {
    // Strip a leading backtick.
    let rest = heading.strip_prefix('`')?;
    // Find the closing backtick (before any trailing text like ` — desc`).
    let close = rest.find('`')?;
    let sig = &rest[..close];

    // sig must be `module.fn(args)`
    let dot = sig.find('.')?;
    let module_part = &sig[..dot];
    if !is_ident(module_part) {
        return None;
    }
    let after_dot = &sig[dot + 1..];
    let paren = after_dot.find('(')?;
    let func_part = &after_dot[..paren];
    if func_part.is_empty() || !is_ident_camel(func_part) {
        return None;
    }
    let close_paren = after_dot.rfind(')')?;
    let args_str = &after_dot[paren + 1..close_paren];

    let params = parse_inline_args(args_str);

    Some(Style2Raw {
        module_prefix: module_part.to_string(),
        func: func_part.to_string(),
        params,
    })
}

/// Parse the comma-separated arg list from a Style-2 heading.
/// Each arg may have a `?` suffix (optional) or a `...` prefix (variadic).
fn parse_inline_args(args_str: &str) -> Vec<DocParam> {
    let mut params = Vec::new();
    for raw in args_str.split(',') {
        let arg = raw.trim();
        if arg.is_empty() {
            continue;
        }
        let variadic = arg.starts_with("...");
        let arg = if variadic { &arg[3..] } else { arg };
        let optional = arg.ends_with('?');
        let arg = if optional { &arg[..arg.len() - 1] } else { arg };
        let name = arg.trim().to_string();
        if name.is_empty() {
            continue;
        }
        // Strip any remaining punctuation that might appear.
        let name = name.trim_matches(|c: char| !c.is_alphanumeric() && c != '_').to_string();
        if name.is_empty() {
            continue;
        }
        params.push(DocParam { name, optional: optional || variadic, variadic, ty: None });
    }
    params
}

/// Parse a param bullet line that may contain multiple comma-separated param specs.
/// e.g. `` `a: int`, `b: int` (`b != 0`) `` → [DocParam{a,int}, DocParam{b,int}]
/// e.g. `x: number` — desc → [DocParam{x,number}]
fn parse_param_line(rest: &str) -> Vec<DocParam> {
    // Split on `, `` ` (comma-space-backtick) to handle multi-param bullets.
    let chunks: Vec<String> = split_param_chunks(rest);
    let mut params = Vec::new();
    for chunk in &chunks {
        let chunk = chunk.trim().trim_matches('`').trim();
        if chunk.is_empty() { continue; }
        // Strip trailing parenthetical ONLY when it's NOT `(optional)`.
        // We need to preserve `(optional)` so `try_parse_single_param` can detect optionality.
        let chunk = if let Some(p) = chunk.find(" (") {
            let tail = &chunk[p..];
            // Keep the chunk as-is if the paren contains `optional` — we need it for detection.
            if tail.to_lowercase().contains("optional") {
                chunk
            } else {
                chunk[..p].trim()
            }
        } else {
            chunk
        };
        if let Some(p) = try_parse_single_param(chunk) {
            params.push(p);
        }
    }
    params
}

/// Split a param bullet line into chunks (handles both single and multi-param formats).
///
/// ONLY activates for lines that start with a backtick-quoted token (`` `name: type` ``)
/// followed by a comma — this is the multi-param pattern like:
///   `` `a: int`, `b: int` (`b != 0`) ``
/// Plain prose lines that happen to contain backticks + commas are left as a single chunk.
fn split_param_chunks(rest: &str) -> Vec<String> {
    // The multi-param pattern requires the line to start with a backtick.
    let trimmed = rest.trim();
    if !trimmed.starts_with('`') {
        return vec![rest.to_string()];
    }
    // Also require that after the first closing backtick there is a comma+space+backtick.
    // This distinguishes `` `a: int`, `b: int` `` from `` `cmd` `string` — ... ``.
    if !rest.contains("`, `") && !rest.contains("`,`") {
        return vec![rest.to_string()];
    }
    // Only split if the FIRST comma+backtick appears within the first 40 chars
    // (heuristic: multi-param lines have short param names, not long prose).
    let first_split = rest.find("`, `").or_else(|| rest.find("`,`")).unwrap_or(usize::MAX);
    if first_split > 40 {
        return vec![rest.to_string()];
    }

    // Split on comma between backtick sections.
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for ch in rest.chars() {
        match ch {
            '`' => {
                depth = if depth == 0 { 1 } else { 0 };
                current.push(ch);
            }
            ',' if depth == 0 => {
                chunks.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    if chunks.is_empty() {
        return vec![rest.to_string()];
    }
    chunks
}

/// Try to parse a single `name: type` or `...name: type` or `name?` param spec.
fn try_parse_single_param(s: &str) -> Option<DocParam> {
    // Remove leading backtick.
    let s = s.trim().trim_start_matches('`');
    // Detect variadic prefix.
    let variadic = s.starts_with("...");
    let s = if variadic { &s[3..] } else { s };

    // Extract the parameter name up to `:`, `(`, whitespace, `?` or backtick.
    let name_end = s.find([':', '(', ' ', '\t', '`', '?'])?;
    if name_end == 0 {
        return None;
    }
    let raw_name = &s[..name_end];
    if !is_ident(raw_name) && !is_ident_camel(raw_name) {
        return None;
    }
    let name = raw_name.to_string();

    let after_name = s[name_end..].trim_start_matches('`').trim();
    // Optional via `?` suffix on the name.
    let optional_via_q = after_name.starts_with('?');
    let after_name = if optional_via_q { after_name[1..].trim() } else { after_name };

    // Determine if it's optional via text like `(optional)` or `(optional, default ...)` in the tail.
    let tail_lower = after_name.to_lowercase();
    let optional_via_text = tail_lower.contains("(optional") || tail_lower.contains("optional)");

    let optional = optional_via_q || optional_via_text || variadic;

    // Extract type if the separator is `:`.
    let ty = if let Some(colon_rest) = after_name.strip_prefix(':') {
        let after_colon = colon_rest.trim();
        // Take the type token up to the EARLIEST of ` — `, ` (`, or end.
        // Use min() rather than or_else() so we stop at whichever delimiter comes first.
        let type_end = match (after_colon.find(" —"), after_colon.find(" (")) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => after_colon.len(),
        };
        let type_str = after_colon[..type_end].trim().replace('`', "").to_string();
        let type_str = type_str.trim();
        if type_str.is_empty() { None } else { Some(type_str.to_string()) }
    } else {
        None
    };

    Some(DocParam { name, optional, variadic, ty })
}

/// Try to parse a `- Returns: type` bullet.
/// Handles several patterns:
///   `- Returns: `type`` (type directly in backticks)
///   `- Returns: `type` of `subtype`` (compound — first backtick token)
///   `- Returns: a new `array`` (word before backtick — extract first backtick-quoted token)
fn try_parse_returns(rest: &str) -> Option<String> {
    let after = rest.strip_prefix("Returns:")?;
    let after = after.trim();

    // Try direct backtick-start: `` `type` ... ``
    if let Some(rest_after_bt) = after.strip_prefix('`') {
        let ty = rest_after_bt.split('`').next()?.trim().to_string();
        if !ty.is_empty() {
            return Some(ty);
        }
    }

    // Fallback: find the first backtick-quoted token in the rest of the line.
    // This handles "a new `array`" or "the first `string`" patterns.
    if let Some(bt_start) = after.find('`') {
        let inner = &after[bt_start + 1..];
        if let Some(bt_end) = inner.find('`') {
            let ty = inner[..bt_end].trim().to_string();
            if !ty.is_empty() {
                return Some(ty);
            }
        }
    }

    None
}


// ─────────────────────────────────────────────────────────────────────────────
// Identifier helpers
// ─────────────────────────────────────────────────────────────────────────────

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Also allows camelCase / mixed (for camelCase function names).
fn is_ident_camel(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

// ─────────────────────────────────────────────────────────────────────────────
// Comparator
// ─────────────────────────────────────────────────────────────────────────────

/// Compare a parsed doc fact against a table row.
/// Panics with a descriptive message if they contradict.
fn compare(fact: &DocFact, sig: &std_sigs::StdSig) {
    let module = resolve_module(&fact.module_prefix);
    let location = format!("{}:{} ({}.{})", fact.page, fact.line, fact.module_prefix, fact.func);

    // Compare param count and names only up to the shorter list.
    // The docs may document fewer params (truncated Style-2 list) or more (a `msg?` the table omits).
    // We check: for each param that BOTH sides enumerate (by position), the name and flags must agree.
    let doc_params = &fact.params;
    let sig_params = sig.params;

    let min_len = doc_params.len().min(sig_params.len());
    for i in 0..min_len {
        let dp = &doc_params[i];
        let sp = &sig_params[i];

        // Name check.
        if dp.name != sp.name {
            panic!(
                "{location}: param[{i}] name mismatch\n  docs: `{}`\n table: `{}`\n\n  doc params:   {}\n table params: {}",
                dp.name,
                sp.name,
                render_doc_params(doc_params),
                render_sig_params(sig_params),
            );
        }

        // Optional/variadic check.
        let doc_optional = dp.optional || dp.variadic;
        let sig_optional = sp.optional || sp.variadic;
        if doc_optional != sig_optional {
            panic!(
                "{location}: param[{i}] `{}` optionality mismatch\n  docs: optional={doc_optional}\n table: optional={sig_optional}\n\n  doc params:   {}\n table params: {}",
                dp.name,
                render_doc_params(doc_params),
                render_sig_params(sig_params),
            );
        }

        // Variadic check.
        if dp.variadic != sp.variadic {
            panic!(
                "{location}: param[{i}] `{}` variadic mismatch\n  docs: variadic={}\n table: variadic={}",
                dp.name, dp.variadic, sp.variadic,
            );
        }

        // Type check: only when BOTH sides state a type.
        if let (Some(dt), Some(st)) = (&dp.ty, &sp.ty) {
            // Normalize: lowercase, strip spaces, compare the leading type token.
            let dt_norm = normalize_type(dt);
            let st_norm = normalize_type(st);
            if dt_norm != st_norm {
                panic!(
                    "{location}: param[{i}] `{}` type mismatch\n  docs: `{dt}`\n table: `{st}`",
                    dp.name,
                );
            }
        }
    }

    // Return type: only when BOTH sides state it.
    if let (Some(dr), Some(sr)) = (&fact.ret, &sig.ret) {
        let dr_norm = normalize_type(dr);
        let sr_norm = normalize_type(sr);
        if dr_norm != sr_norm {
            panic!(
                "{location}: return type mismatch\n  docs: `{dr}`\n table: `{sr}`\n\n module={module}",
            );
        }
    }
}

fn normalize_type(s: &str) -> String {
    // Take the first token up to space, lowercase, then strip any `(…)` suffix
    // so `fn(item)`, `fn(a, b)` all collapse to `fn`.
    let tok = s.split_whitespace().next().unwrap_or(s).to_lowercase();
    // Strip parenthesised suffix (e.g. `fn(item)` → `fn`).
    let tok = if let Some(p) = tok.find('(') { &tok[..p] } else { &tok[..] };
    // Alias: docs say `function`, table says `fn` — treat them as equivalent.
    if tok == "function" { "fn".to_string() } else { tok.to_string() }
}

fn render_doc_params(params: &[DocParam]) -> String {
    let parts: Vec<_> = params
        .iter()
        .map(|p| {
            let pre = if p.variadic { "..." } else { "" };
            let suf = if p.optional && !p.variadic { "?" } else { "" };
            format!("{pre}{}{suf}", p.name)
        })
        .collect();
    parts.join(", ")
}

fn render_sig_params(params: &[std_sigs::StdParam]) -> String {
    let parts: Vec<_> = params
        .iter()
        .map(|p| {
            let pre = if p.variadic { "..." } else { "" };
            let suf = if p.optional && !p.variadic { "?" } else { "" };
            format!("{pre}{}{suf}", p.name)
        })
        .collect();
    parts.join(", ")
}

// ─────────────────────────────────────────────────────────────────────────────
// Modules that use Style-1 and should be fully covered
// ─────────────────────────────────────────────────────────────────────────────

/// Module tokens (as they appear in doc headings) that use Style-1 and whose
/// `Fn` members must each have at least one doc fact.
///
/// Note: `msgpack` and `cbor` are documented with prose bullets (not `### module.fn` headings)
/// in data.md, so they are excluded from this Style-1 coverage check.
const STYLE1_MODULES: &[&str] = &[
    "string", "array", "object", "map", "set", "bytes", "convert", "decimal",
    "math", "json", "csv", "regex", "encoding", "toml", "yaml", "url", "uuid",
];

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn docs_and_table_never_contradict() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/content/stdlib");
    let mut facts: Vec<DocFact> = Vec::new();

    for entry in std::fs::read_dir(dir).expect("docs/content/stdlib dir") {
        let entry = entry.unwrap();
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let page = p.file_name().unwrap().to_str().unwrap().to_string();
        facts.extend(parse_page(&page, &text));
    }

    assert!(
        facts.len() > 200,
        "parser regression: only {} facts extracted (expected >200)",
        facts.len()
    );

    let mut checked = 0usize;
    for f in &facts {
        let module = resolve_module(&f.module_prefix);

        // Look up the table. If no row, this is a handle method, an unimplemented module,
        // or an un-curated API — skip silently (not a contradiction).
        let Some(sig) = std_sigs::std_sig(&module, &f.func) else {
            continue;
        };

        compare(f, sig);
        checked += 1;
    }

    // Sanity: at least 150 facts matched the table (not everything skipped).
    assert!(
        checked >= 150,
        "too few facts matched the table ({checked}): parser or alias table may be broken"
    );
}

/// Every `Fn` member of a Style-1 module MUST appear at least once in the docs.
#[test]
fn style1_modules_are_fully_documented() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/content/stdlib");

    // Collect all doc facts once.
    let mut all_facts: Vec<DocFact> = Vec::new();
    for entry in std::fs::read_dir(dir).expect("docs/content/stdlib dir") {
        let entry = entry.unwrap();
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let page = p.file_name().unwrap().to_str().unwrap().to_string();
        all_facts.extend(parse_page(&page, &text));
    }

    for module_tok in STYLE1_MODULES {
        let std_module = resolve_module(module_tok);
        let Some(members) = std_sigs::module_members(&std_module) else {
            continue; // feature-gated out
        };

        for (fn_name, kind) in members {
            if !matches!(kind, MemberKind::Fn) {
                continue; // skip Const / HandleMethod
            }
            // Check that at least one doc fact exists for this fn.
            let found = all_facts.iter().any(|f| {
                f.module_prefix == *module_tok && f.func == *fn_name
            });
            assert!(
                found,
                "Style-1 module {std_module}: function `{fn_name}` has a table row but no doc heading in docs/content/stdlib/\n  (expected a `### {module_tok}.{fn_name}` heading)"
            );
        }
    }
}

/// Self-test: a deliberately mutated fact MUST trip the comparator.
#[test]
fn comparator_detects_a_contradiction() {
    // We know math.abs has one required param named `x`.
    // Build a fact that says the param is named `y` instead — this must panic.
    let bad_fact = DocFact {
        page: "synthetic_test".to_string(),
        line: 1,
        module_prefix: "math".to_string(),
        func: "abs".to_string(),
        params: vec![DocParam {
            name: "y".to_string(), // wrong name
            optional: false,
            variadic: false,
            ty: Some("number".to_string()),
        }],
        ret: None,
    };

    let sig = std_sigs::std_sig("std/math", "abs")
        .expect("std/math::abs must be in the table");

    let result = std::panic::catch_unwind(|| compare(&bad_fact, sig));
    assert!(
        result.is_err(),
        "comparator should have panicked on a name mismatch but returned Ok"
    );

    // Also test: a flipped optional flag.
    let bad_fact2 = DocFact {
        page: "synthetic_test".to_string(),
        line: 2,
        module_prefix: "string".to_string(),
        func: "slice".to_string(),
        params: vec![
            DocParam { name: "s".to_string(), optional: false, variadic: false, ty: None },
            DocParam { name: "start".to_string(), optional: false, variadic: false, ty: None },
            DocParam {
                name: "end".to_string(),
                optional: false, // wrong: should be optional
                variadic: false,
                ty: None,
            },
        ],
        ret: None,
    };
    let sig2 = std_sigs::std_sig("std/string", "slice")
        .expect("std/string::slice must be in the table");
    let result2 = std::panic::catch_unwind(|| compare(&bad_fact2, sig2));
    assert!(
        result2.is_err(),
        "comparator should have panicked on an optionality mismatch but returned Ok"
    );
}
