//! BATT negative space — the spec promises NO engine surface change (spec §16).
//! These pins fail if any BATT unit accidentally grows the engine.

#[test]
fn aso_format_version_unchanged() {
    // BATT touches no opcode/serialization layout. Merge-base-relative, NOT a campaign
    // literal: recorded from this branch's merge-base (`main` @ the SIG merge `cb17495`),
    // where ELIDE had already bumped the format to 29. The assertion is "unchanged by
    // THIS branch" — re-record the const if rebasing over a spec that bumps it.
    const ASO_AT_MERGE_BASE: u32 = 29; // ELIDE bumped 28→29 (Op::CallElided); BATT must not move it.
    assert_eq!(
        ascript::vm::aso::ASO_FORMAT_VERSION,
        ASO_AT_MERGE_BASE,
        "BATT must not bump the .aso format — asserts no change vs this branch's \
         merge-base (re-record the const if rebasing over a spec that bumped it)"
    );
}

#[test]
fn value_size_unchanged() {
    // No new Value variant / no size growth (24 bytes, NANB's FINAL size, src/value.rs).
    assert_eq!(std::mem::size_of::<ascript::value::Value>(), 24);
}
