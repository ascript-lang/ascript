//! BATT D3 (§13) regression: `std/markdown` render opts (and the nested `allow`
//! object) must use the slab-safe `ObjectCell::get` accessor, NOT `borrow()`
//! (which PANICS in slab mode). The VM builds source-literal objects in SLAB
//! storage; the tree-walker builds DICT storage. A `borrow()` on the opts/`allow`
//! Object therefore aborts the VM while the tree-walker succeeds — a four-mode
//! byte-identity divergence AND an uncatchable VM crash. These tests run a
//! SOURCE-LITERAL opts object on BOTH engines and assert byte-identical,
//! panic-free output. (The in-module unit tests build opts via `IndexMap` =
//! Dict storage, so they cannot catch this — only a VM run over real source can.)
//!
//! They also pin the sanitize-by-default security posture through the FULL
//! compile→VM pipeline: a `<script>` and a `javascript:` link in markdown come
//! out inert on both engines.

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Run `src` through the binary on the given engine, returning (stdout, success, stderr).
fn run(src: &str, tree_walker: bool) -> (String, bool, String) {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!(
        "ascript_markdown_{}_{}_{}.as",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        if tree_walker { "tw" } else { "vm" }
    ));
    std::fs::write(&file, src).unwrap();
    let mut cmd = Command::new(bin);
    cmd.arg("run").arg(&file);
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_file(&file);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `markdown.render` with a SOURCE-LITERAL opts object must succeed on the VM (no
/// slab panic) and match the tree-walker exactly.
#[test]
fn render_with_literal_opts_object_no_slab_panic_four_mode() {
    let src = "import * as markdown from \"std/markdown\"\n\
        let html = markdown.render(\"# hi\", {sanitize: true, gfmTables: false})\n\
        print(html)\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, _) = run(src, true);
    assert!(
        vm_ok,
        "VM aborted on a source-literal opts object (slab borrow panic):\n{vm_err}"
    );
    assert!(tw_ok, "tree-walker run failed");
    assert_eq!(vm_out, tw_out, "VM and tree-walker diverged on render opts");
    assert!(vm_out.contains("<h1>hi</h1>"), "got {vm_out:?}");
}

/// A literal NESTED `allow` object (also slab-mode on the VM) must not panic and
/// must forward correctly on both engines.
#[test]
fn render_with_literal_nested_allow_object_four_mode() {
    let src = "import * as markdown from \"std/markdown\"\n\
        let html = markdown.render(\"<mark>x</mark>\", {allow: {tags: [\"mark\", \"p\"]}})\n\
        print(html)\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, _) = run(src, true);
    assert!(vm_ok, "VM aborted on a literal nested allow object:\n{vm_err}");
    assert!(tw_ok, "tree-walker run failed");
    assert_eq!(vm_out, tw_out, "VM and tree-walker diverged on allow opts");
    assert!(vm_out.contains("<mark>x</mark>"), "allow.tags not forwarded: {vm_out:?}");
}

/// Sanitize-by-default pin through the full pipeline: a `<script>` in markdown
/// is inert, and a `javascript:` link is dropped — on BOTH engines.
#[test]
fn sanitize_by_default_pin_four_mode() {
    let src = "import * as markdown from \"std/markdown\"\n\
        let a = markdown.render(\"<script>alert(1)</script>\")\n\
        let b = markdown.render(\"[x](javascript:alert(1))\")\n\
        print(a)\n\
        print(b)\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, _) = run(src, true);
    assert!(vm_ok, "VM aborted:\n{vm_err}");
    assert!(tw_ok, "tree-walker run failed");
    assert_eq!(vm_out, tw_out, "VM and tree-walker diverged on sanitize pin");
    let lc = vm_out.to_ascii_lowercase();
    assert!(!lc.contains("<script"), "FAIL-OPEN: live <script: {vm_out:?}");
    assert!(
        !lc.replace(char::is_whitespace, "").contains("javascript:"),
        "FAIL-OPEN: javascript: survived: {vm_out:?}"
    );
}

/// `{sanitize: false}` (a literal opts flag) emits the raw HTML on both engines.
#[test]
fn sanitize_false_escape_hatch_four_mode() {
    let src = "import * as markdown from \"std/markdown\"\n\
        let html = markdown.render(\"<script>alert(1)</script>\", {sanitize: false})\n\
        print(html)\n";
    let (vm_out, vm_ok, vm_err) = run(src, false);
    let (tw_out, tw_ok, _) = run(src, true);
    assert!(vm_ok, "VM aborted:\n{vm_err}");
    assert!(tw_ok, "tree-walker run failed");
    assert_eq!(vm_out, tw_out, "VM and tree-walker diverged on sanitize:false");
    assert!(vm_out.contains("<script>"), "raw HTML not emitted: {vm_out:?}");
}
