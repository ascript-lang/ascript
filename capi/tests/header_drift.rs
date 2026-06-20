//! EMBED §8.3 — the header-drift guard.
//!
//! `include/ascript.h` is hand-written and checked in. This test extracts the
//! `as_*`/`ascript_*` C-function declarations from the header AND the exported
//! `#[no_mangle] pub [unsafe] extern "C" fn <name>` symbols from `src/lib.rs`, and asserts
//! SET EQUALITY both directions. A new exported fn with no header decl (or a header decl
//! with no fn) fails here — the header can never silently drift from the ABI.
//!
//! (Confirmed failing-first during development by removing a single header decl and a
//! single fn in turn — each produced a clear one-sided diff.)

use std::collections::BTreeSet;

/// Function-name prefixes that are part of the C ABI surface.
fn is_abi_name(name: &str) -> bool {
    (name.starts_with("as_") || name.starts_with("ascript_"))
        // `as__test_panic` (double underscore) is the `#[cfg(test)]`-only panic-injection
        // seam — NOT part of the ABI, NOT in the header. Exclude it by its convention.
        && !name.starts_with("as__")
}

/// Extract the exported symbol names from the crate source: every
/// `#[no_mangle] pub [unsafe] extern "C" fn <name>(`.
fn crate_exports() -> BTreeSet<String> {
    let src = include_str!("../src/lib.rs");
    let mut out = BTreeSet::new();
    // A simple line scan: find lines containing `extern "C" fn` after a `pub` (the
    // `#[no_mangle]` is on the preceding line). We accept `pub extern "C" fn` and
    // `pub unsafe extern "C" fn`. The fn name is the token after `fn` up to `(` or `<`.
    for line in src.lines() {
        let line = line.trim_start();
        let after = if let Some(rest) = line.strip_prefix("pub extern \"C\" fn ") {
            rest
        } else if let Some(rest) = line.strip_prefix("pub unsafe extern \"C\" fn ") {
            rest
        } else {
            continue;
        };
        // The name runs until `(` or `<` or whitespace.
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && is_abi_name(&name) {
            out.insert(name);
        }
    }
    out
}

/// Extract the declared function names from the header: every `<ret> <name>(` where
/// `<name>` is an ABI name. The header uses one declaration per logical signature; a
/// declaration may span multiple lines, but the NAME always sits on the line with the
/// return type immediately before `(`.
fn header_decls() -> BTreeSet<String> {
    let hdr = include_str!("../include/ascript.h");
    let mut out = BTreeSet::new();
    for line in hdr.lines() {
        let line = line.trim();
        // Skip preprocessor + comment lines + the typedef'd callback (it's a TYPE, not an
        // exported fn — `typedef as_status (*as_host_fn)(...)`).
        if line.starts_with('#')
            || line.starts_with('*')
            || line.starts_with("/*")
            || line.starts_with("//")
            || line.starts_with("typedef")
        {
            continue;
        }
        // Find an ABI name immediately followed by `(`.
        if let Some(name) = find_decl_name(line) {
            out.insert(name);
        }
    }
    out
}

/// If `line` contains `<name>(` where `<name>` is an ABI name (a maximal ident ending at
/// the `(`), return it. Handles `as_value *as_nil(void)` (a `*`-prefixed return) too.
fn find_decl_name(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let paren = line.find('(')?;
    // Walk back from `(` over identifier chars to get the name.
    let mut start = paren;
    while start > 0 {
        let c = bytes[start - 1] as char;
        if c.is_alphanumeric() || c == '_' {
            start -= 1;
        } else {
            break;
        }
    }
    let name = &line[start..paren];
    if is_abi_name(name) {
        Some(name.to_string())
    } else {
        None
    }
}

#[test]
fn header_matches_exported_symbols() {
    let exports = crate_exports();
    let decls = header_decls();

    let missing_in_header: Vec<_> = exports.difference(&decls).collect();
    let missing_in_crate: Vec<_> = decls.difference(&exports).collect();

    assert!(
        missing_in_header.is_empty() && missing_in_crate.is_empty(),
        "ascript.h has drifted from the crate's exported #[no_mangle] symbols.\n\
         Exported fns with NO header declaration: {missing_in_header:?}\n\
         Header declarations with NO exported fn: {missing_in_crate:?}\n\
         (exports = {exports:?})\n\
         (header  = {decls:?})"
    );

    // Sanity: we actually found a meaningful set (guards against a broken extractor that
    // matches nothing and trivially "passes").
    assert!(
        exports.len() >= 18,
        "expected >= 18 ABI fns, found {}: {exports:?}",
        exports.len()
    );
}
