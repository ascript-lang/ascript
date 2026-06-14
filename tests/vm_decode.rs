//! DECODE structural guards (the decoded-dispatch effort).
//!
//! These are cheap source-scan tripwires that keep the invalidation chokepoint
//! intact; the behavioral proofs live in the differential + the Task-6 battery.

use std::path::Path;

/// Recursively walk every `.rs` file under `dir`, calling `f(path, contents)`.
fn visit(dir: &Path, f: &mut dyn FnMut(&Path, &str)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, f);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                f(&path, &text);
            }
        }
    }
}

/// DECODE §4.1: `Code::patch_byte` (the raw UnsafeCell write) must be reachable
/// ONLY through `Chunk::patch_byte` (which bumps `patch_epoch`). A future patch
/// site calling the raw Code method would silently skip invalidation — this
/// source scan trips on it. (The behavioral proof is the Task-6 battery; this
/// is the cheap structural guard.)
#[test]
fn raw_code_patch_byte_has_no_callers_outside_chunk_rs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    visit(&root, &mut |path, text| {
        if path.ends_with("vm/chunk.rs") {
            return; // the definition + the one sanctioned caller
        }
        for (i, line) in text.lines().enumerate() {
            // `chunk.patch_byte(`/-style calls are fine (they bump); the raw form is
            // `code.patch_byte(` / `.code.patch_byte(` — flag those.
            if line.contains("code.patch_byte(") {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "raw Code::patch_byte callers bypass patch_epoch:\n{}",
        offenders.join("\n")
    );
}
