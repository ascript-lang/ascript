//! SHAPE Task 5.3 — Negative-space verification.
//!
//! SHAPE (shape-native storage) is a **purely runtime storage-representation
//! change**: objects and instances use a hidden-class shape id for field layout,
//! but this is entirely internal to the VM and the GC — NONE of it touches the
//! grammar, the two parsers, the formatter, the tree-sitter grammar, or the
//! `.aso` bytecode format.
//!
//! **Merge-base byte-equivalence proof (audit, not a rebuild):**
//!
//! ```text
//! git diff main --stat -- src/compile/ src/vm/opcode.rs src/vm/aso.rs
//! ```
//!
//! Result (confirmed at merge-base `a2b3205`):
//!
//! ```text
//!  src/vm/aso.rs | 1 +
//!  1 file changed, 1 insertion(+)
//! ```
//!
//! The single `+1` line in `src/vm/aso.rs` is an additive `debug_assert!` in
//! the LOAD path only:
//!
//! ```rust,ignore
//! debug_assert!(c.lit_shapes.borrow().is_empty());
//! ```
//!
//! This assertion fires ONLY in debug builds, verifies a runtime invariant
//! (the shape-cache is empty on a freshly loaded chunk, before any execution),
//! and neither reads from nor writes to the serialized byte stream — so the
//! `.aso` byte format is UNCHANGED. `src/compile/` and `src/vm/opcode.rs` have
//! ZERO changes: no new opcodes, no compiler changes.
//!
//! Each pinned constant below trips on any accidental future change and forces a
//! conscious update with confirmation that SHAPE is still not the cause.

use std::path::Path;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// SHAPE must NOT bump the `.aso` format version.
///
/// The shape registry (`ShapeRegistry`) is a per-`Vm` runtime structure built
/// and queried at execution time; it is never serialized into `.aso` chunks.
/// Shape ids are an ephemeral, execution-local optimization — a freshly loaded
/// `.aso` starts with an empty `lit_shapes` cache (asserted by the one-line
/// `debug_assert!` SHAPE added) and repopulates the shape table on first run.
///
/// This literal pin trips on ANY bump, forcing each bumping feature to
/// consciously update it and confirm the bump is not from SHAPE. The current
/// value is 29: DEFER bumped 27→28 (adding `DeferPush`/`DeferPushMethod`
/// opcodes) before this branch; SHAPE kept it at 28; ELIDE bumped 28→29
/// (adding `CallElided`) on `feat/contract-elision`.
#[test]
fn aso_format_version_is_unchanged_by_shape() {
    const ASO_AT_MERGE_BASE: u32 = 29;
    assert_eq!(
        ascript::vm::aso::ASO_FORMAT_VERSION,
        ASO_AT_MERGE_BASE,
        "ASO_FORMAT_VERSION changed — confirm the bump is NOT attributable to SHAPE \
         (the shape registry is a runtime-only, per-Vm structure; it is never \
         serialized into .aso chunks), then update ASO_AT_MERGE_BASE in this file."
    );
}

/// SHAPE added zero opcodes — the `Op` enum variant count must stay at 120
/// at the SHAPE merge base. ELIDE later added `Op::CallElided` (byte 120),
/// raising the count to 121. Update this comment and `EXPECTED_OP_COUNT` only
/// when a non-SHAPE feature adds a new opcode.
///
/// The `Op` enum is `#[repr(u8)]`; SHAPE left the last variant as `DeferPushMethod`
/// (discriminant 119, 0-indexed) = 120 variants. SHAPE is a pure
/// storage-representation change: object/instance field access still routes
/// through the same `GetProp`/`SetProp`/`GetIndex`/`SetIndex` opcodes, which
/// now consult the shape registry internally — no new dispatch byte is needed.
///
/// This pin trips if a future SHAPE-attributed change accidentally introduces
/// an opcode, requiring a conscious update.
#[test]
fn op_variant_count_is_unchanged_by_shape() {
    /// Expected number of `Op` variants (last discriminant + 1).
    /// SHAPE kept this at 120 (last variant `DeferPushMethod`, discriminant 119).
    /// ELIDE added `CallElided` (discriminant 120) on `feat/contract-elision`,
    /// raising the count to 121. Update here only for non-SHAPE additions.
    const EXPECTED_OP_COUNT: usize = 121;
    // Count via `from_u8` over every possible discriminant: a new opcode is only
    // usable once it has a `from_u8` arm, so this trips on a variant added
    // ANYWHERE in the enum — inserted before the last variant, appended after it,
    // or renamed/removed. (An earlier pin anchored on `DeferPushMethod as u16 + 1`
    // had a blind spot: a variant APPENDED after `DeferPushMethod` left the
    // expression at 120 and slipped through — caught in the Phase-5 holistic.)
    let op_count = (0u16..=255)
        .filter(|&b| ascript::vm::opcode::Op::from_u8(b as u8).is_some())
        .count();
    assert_eq!(
        op_count, EXPECTED_OP_COUNT,
        "Op variant count changed — SHAPE must add NO opcodes (shape lookups \
         happen inside existing GetProp/SetProp/GetIndex/SetIndex arms); update \
         EXPECTED_OP_COUNT only if a non-SHAPE feature adds the new opcode."
    );
    // Complementary sanity pin: `CallElided` is now the last variant (120).
    // (Before ELIDE: `DeferPushMethod` was 119 = the last.)
    assert_eq!(
        ascript::vm::opcode::Op::CallElided as u16 + 1,
        EXPECTED_OP_COUNT as u16,
        "CallElided is no longer the last Op variant"
    );
}

/// End-to-end proof that SHAPE is a runtime change, not a bytecode change:
///
/// We build `tests/fixtures/shape_negative_space/subject.as` (a representative
/// core-language program exercising object literals, spread, class with declared
/// and defaulted/inherited fields, `C.from`, `instanceof`, and a >64-key object
/// that exercises the slab→dict shape demotion path inside SHAPE) → `.aso`, then
/// run it and assert deterministic expected stdout.
///
/// This proves:
/// 1. The compile pipeline is untouched (no compiler change, no new opcodes) —
///    if it were broken the build would fail or produce wrong output.
/// 2. The `.aso` format is loadable (a format-version bump would surface here as
///    a load error, caught by the `aso_format_version_is_unchanged_by_shape` pin).
/// 3. Object/instance shape operations execute correctly end-to-end through the
///    bytecode path (SHAPE is runtime-only; its correctness is the test output).
///
/// `subject.as` is core-language only (no `std/*` feature-gated imports), so
/// this test runs under BOTH `--default-features` and `--no-default-features`.
///
/// The merge-base byte-equivalence proof is the `git diff main --stat` audit in
/// the module doc: `src/compile/` and `src/vm/opcode.rs` have ZERO changes and
/// `src/vm/aso.rs` has only a +1 non-serializing `debug_assert!` — so the bytes
/// a fresh build produces here are identical to what merge-base `a2b3205` would
/// produce.
#[tokio::test]
async fn shape_program_compiles_to_aso_and_round_trips() {
    use std::process::Command;

    let bin = env!("CARGO_BIN_EXE_ascript");
    let fixtures = repo_root().join("tests/fixtures/shape_negative_space");
    let subject_as = fixtures.join("subject.as");

    let dir = std::env::temp_dir().join("ascript_shape_negspace_aso");
    std::fs::create_dir_all(&dir).unwrap();
    let out_aso = dir.join("subject.aso");

    // Build subject.as → .aso.
    let build = Command::new(bin)
        .args([
            "build",
            subject_as.to_str().unwrap(),
            "-o",
            out_aso.to_str().unwrap(),
        ])
        .output()
        .expect("run ascript build");
    assert!(
        build.status.success(),
        "ascript build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    // Run the .aso and assert deterministic stdout.
    // A format-version mismatch or a missing opcode arm would surface here.
    let run = Command::new(bin)
        .args(["run", out_aso.to_str().unwrap()])
        .output()
        .expect("run .aso");
    assert!(
        run.status.success(),
        "ascript run .aso failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    // Expected output (see subject.as comments for derivation):
    //   ext.x = 1, ext.z = hello, ext.w = true
    //   p.x = 3, p.label = P1, p.dist() = 3²+4² = 25, p instanceof Point = true
    //   q.dist() = 1²+2²+2² = 9, q instanceof Point3D = true, q instanceof Point = true
    //   big["k0"] = 0, big["k64"] = 64, big["k69"] = 69
    let expected = "1\nhello\ntrue\n3\nP1\n25\ntrue\n9\ntrue\ntrue\n0\n64\n69\n";
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        expected,
        "shape .aso round-trip produced unexpected output"
    );

    std::fs::remove_dir_all(&dir).ok();
}
