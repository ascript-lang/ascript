//! `unresolved-import` (conservative): an `import … from "<path>"` whose path
//! does not resolve.
//!
//! V1 scope — **`std/*` specifiers only**. A `std/*` path is checked against the
//! authoritative, feature-INDEPENDENT module registry
//! ([`crate::stdlib::is_known_std_module`] / `STD_MODULES`, mirrored 1:1 against
//! `std_module_exports`): a `std/*` specifier not in that set (e.g. a typo like
//! `"std/maths"`) is flagged. The registry is feature-independent on purpose so
//! the checker behaves identically under `--no-default-features` (a source that
//! imports `std/json` is valid AScript regardless of the binary's Cargo
//! features).
//!
//! **Relative file paths** (`"./mod"`, `"../x"`, `"mod.as"`) are deliberately
//! NOT flagged here: the static analysis entry point (`analyze` / `analyze_with_config`
//! in `src/check/analyze.rs`) is PATH-LESS — it receives only the source text,
//! not the importing file's path — so it cannot resolve a relative path against
//! the filesystem. Rather than guess (and risk false positives), V1 leaves file
//! imports untouched. File-path resolution is a documented follow-up that will
//! require threading the source path into the analysis driver.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    for import in tree.descendants().filter(|n| n.kind() == ImportStmt) {
        // The module specifier string token (`from "<path>"`).
        let Some(str_tok) = import
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == Str)
        else {
            continue; // malformed import (missing path) → syntax-error covers it
        };
        let path = strip_quotes(str_tok.text());

        // EMBED §6.3: a `host:` specifier is resolved-by-construction at RUNTIME
        // registration time (the host registers the module on the isolate before the
        // script runs) — statically unknowable here, so the checker SKIPS it. Never a
        // false positive on an embed-targeted script. (This arm is explicit even though
        // the `!std/` guard below would also skip it — the skip is intentional, not
        // incidental, so it is named.)
        if path.starts_with("host:") {
            continue;
        }
        // V1: only `std/*` specifiers are resolvable here. A relative/file path
        // cannot be checked without the importing file's location (path-less
        // analysis) — see the module doc — so it is left alone.
        if !path.starts_with("std/") {
            continue;
        }
        if crate::stdlib::is_known_std_module(path) {
            continue; // a real std module → OK
        }
        // DX D4 §5.2 "did you mean": suggest the closest real std module (e.g.
        // `std/maths` → `std/math`). Candidate set = the feature-independent
        // `STD_MODULES` registry. The suggestion only annotates the message (a CLI
        // `help` note); no auto-fix is offered for an import path (an import edit
        // can shift the bound names, so it stays a manual change).
        let suggestion = crate::check::suggest::closest(path, crate::stdlib::STD_MODULES.iter().copied());
        let message = match suggestion {
            Some(s) => format!("unresolved import `{path}`: no such std module — did you mean `{s}`?"),
            None => format!("unresolved import `{path}`: no such std module"),
        };
        out.push(AsDiagnostic {
            range: code_range(import),
            severity: Severity::Warning,
            code: "unresolved-import".to_string(),
            message,
            fix: None,
        });
    }
    out
}

/// Strip the surrounding quote characters from a string-literal token's text.
/// Module specifiers are plain ASCII paths with no escapes, so a first/last
/// char trim is exact (mirrors `strip_quotes` in `src/compile/mod.rs`).
fn strip_quotes(s: &str) -> &str {
    let mut chars = s.chars();
    chars.next();
    chars.next_back();
    chars.as_str()
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;

    fn count(src: &str, code: &str) -> usize {
        analyze(src)
            .diagnostics
            .iter()
            .filter(|d| d.code == code)
            .count()
    }
    fn has(src: &str, code: &str) -> bool {
        count(src, code) > 0
    }

    #[test]
    fn typo_std_module_flagged() {
        let src = "import { abs } from \"std/maths\"\nprint(1)\n";
        assert_eq!(
            count(src, "unresolved-import"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn real_std_module_not_flagged() {
        let src = "import { abs } from \"std/math\"\nprint(abs(-1))\n";
        assert!(
            !has(src, "unresolved-import"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn nonexistent_std_module_flagged() {
        let src = "import * as x from \"std/doesnotexist\"\nprint(1)\n";
        assert_eq!(
            count(src, "unresolved-import"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn nested_std_module_not_flagged() {
        let src = "import { connect } from \"std/net/tcp\"\nprint(1)\n";
        assert!(
            !has(src, "unresolved-import"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn host_specifier_not_flagged() {
        // EMBED §6.3: a `host:` import is resolved-by-host at runtime — the checker
        // skips it (never a false positive on an embed-targeted script).
        let src = "import * as app from \"host:app\"\nprint(1)\n";
        assert!(
            !has(src, "unresolved-import"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn relative_path_not_flagged() {
        // path-less analysis cannot resolve a file import; V1 leaves it alone.
        let src = "import { a } from \"./mod\"\nprint(1)\n";
        assert!(
            !has(src, "unresolved-import"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn message_names_the_path() {
        let src = "import { abs } from \"std/maths\"\nprint(1)\n";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "unresolved-import")
            .unwrap();
        // `std/maths` is one edit from `std/math` → the message names the path AND
        // suggests the close match.
        assert_eq!(
            d.message,
            "unresolved import `std/maths`: no such std module — did you mean `std/math`?"
        );
    }

    #[test]
    fn far_std_module_has_no_suggestion() {
        // `std/doesnotexist` is far from every real module → no "did you mean".
        let d = analyze("import * as x from \"std/doesnotexist\"\nprint(1)\n")
            .diagnostics
            .into_iter()
            .find(|d| d.code == "unresolved-import")
            .unwrap();
        assert!(!d.message.contains("did you mean"), "got: {}", d.message);
    }
}
