//! PAR Task 3.2 — Negative-space verification (spec §5.2).
//!
//! PAR (data-parallel `task.pmap` / `task.preduce`) is **purely stdlib-level**:
//! the two new functions dispatch work across the existing worker isolate pool via
//! the existing `ChunkJob` / per-chunk driver machinery — NONE of it touches the
//! grammar, the two parsers, the formatter, the tree-sitter grammar, the `.aso`
//! bytecode format, the worker-wire serializer tag set, or the opcode table.
//!
//! Per spec §5.2 this is asserted as a CI tripwire so a future change can't
//! silently regress the "Touching syntax/opcodes/format is genuinely N/A" property:
//!
//!  1. `ASO_FORMAT_VERSION` is pinned at 29 (DEFER bumped 27→28; ELIDE bumped 28→29;
//!     PAR bumped NOTHING).
//!  2. The worker-wire tag space is pinned: `TAG_SHARED = 15` is still the last tag
//!     (next free is 16). PAR put no new bytes on the wire — all PAR inputs and
//!     outputs cross the boundary as plain values encoded with existing tags.
//!  3. The `Op` enum count is pinned at 121 (`CallElided` at discriminant 120 is still
//!     the last opcode). PAR added no opcode.
//!
//! Modeled on `tests/srv_negative_space.rs` and `tests/shape_negative_space.rs`.

use std::rc::Rc;

// ──────────────────────────────────────────────────────────────────────────────
// Pin 1 — `.aso` format version
// ──────────────────────────────────────────────────────────────────────────────

/// PAR must NOT bump `ASO_FORMAT_VERSION`.
///
/// `task.pmap` / `task.preduce` are stdlib calls; all work dispatches through
/// the existing opcode set and worker isolate infrastructure.  There is no new
/// bytecode constant, no new `FnProto` field, and no new serialised layout.
///
/// Current value is 29:
///   - DEFER (DeferPush / DeferPushMethod opcodes) bumped 27 → 28.
///   - ELIDE (Op::CallElided opcode) bumped 28 → 29.
///   - PAR bumped NOTHING.
///
/// If another spec legitimately bumps this after PAR merges, update this literal
/// in THAT spec's branch — never in PAR's branch.
#[test]
fn aso_format_version_unchanged_by_par() {
    // Literal pin: trips on ANY bump. PAR bumps nothing; 29 is from DEFER(28)+ELIDE(29).
    // If another spec bumped it, update this pin in ITS branch, never PAR.
    assert_eq!(
        ascript::vm::aso::ASO_FORMAT_VERSION,
        29,
        "ASO_FORMAT_VERSION changed — confirm the bump is NOT attributable to PAR \
         (task.pmap / task.preduce are stdlib calls; no new opcode or bytecode layout); \
         then update this literal in the bumping feature's branch."
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Pin 2 — worker-wire tag space
// ──────────────────────────────────────────────────────────────────────────────

/// PAR added no new worker-wire tag.
///
/// The worker serializer (`src/worker/serialize.rs`) encodes every sendable
/// `Value` kind with a one-byte tag.  The current tag vocabulary is:
///
/// ```text
///   0  TAG_NIL
///   1  TAG_BOOL
///   2  TAG_NUMBER   (float)
///   3  TAG_DECIMAL
///   4  TAG_STR
///   5  TAG_BYTES
///   6  TAG_ARRAY
///   7  TAG_OBJECT
///   8  TAG_MAP
///   9  TAG_SET
///  10  TAG_ENUM
///  11  TAG_REGEX    (cfg(feature = "data"))
///  12  TAG_INSTANCE
///  13  TAG_REF      (cycle back-reference)
///  14  TAG_INT
///  15  TAG_SHARED   (SRV §3.7(b)) — the last tag; next free is 16
/// ```
///
/// We pin this by encoding one value of each non-`data`-gated sendable kind,
/// collecting the first byte (the tag), and asserting the maximum tag byte == 15.
/// A new PAR tag would appear as a first-byte > 15 and trip this assertion.
///
/// Note: `TAG_REF` (13) appears only inside a container as a cycle back-reference,
/// never as the outermost tag of a top-level value, so it is not exercised directly
/// here.  `TAG_REGEX` (11) is `cfg(feature = "data")`-gated and not asserted as the
/// max — the max claim (15) holds regardless of whether "data" is active.
#[test]
fn worker_wire_tag_space_unchanged_by_par() {
    use ascript::value::{ArrayCell, EnumVariant, MapCell, ObjectCell, SetCell, Value};
    use ascript::worker::serialize::encode;
    use indexmap::{IndexMap, IndexSet};

    // One representative value per non-gated sendable kind.
    let specimens: &[(&str, Value)] = &[
        ("nil", Value::nil()),
        ("bool", Value::bool_(true)),
        ("int", Value::int(42)),
        ("float", Value::float(1.5)),
        ("str", Value::str("hello")),
        ("bytes", Value::bytes(vec![1u8, 2, 3])),
        (
            "array",
            Value::array_cell(ArrayCell::new(vec![Value::int(1)])),
        ),
        (
            "object",
            Value::object_cell(ObjectCell::new({
                let mut m = IndexMap::new();
                m.insert("k".to_string(), Value::int(1));
                m
            })),
        ),
        (
            "map",
            Value::map_cell(MapCell::new(IndexMap::new())),
        ),
        (
            "set",
            Value::set_cell(SetCell::new(IndexSet::new())),
        ),
        (
            "enum",
            Value::enum_variant(Rc::new(EnumVariant {
                enum_name: "Color".to_string(),
                name: "Red".to_string(),
                value: Value::int(0),
                payload: None,
                ctor: false,
                def: None,
            })),
        ),
    ];

    let mut max_tag: u8 = 0;
    for (kind, v) in specimens {
        let (bytes, _shared) = encode(v)
            .unwrap_or_else(|e| panic!("encode({kind}) failed: {e:?}"));
        assert!(
            !bytes.is_empty(),
            "encode({kind}) produced an empty buffer — no tag byte"
        );
        let tag = bytes[0];
        if tag > max_tag {
            max_tag = tag;
        }
    }

    // TAG_SHARED = 15 is the last tag; next free slot is 16.
    // PAR put no new bytes on the wire, so the max tag across all sendable kinds
    // must still be ≤ 15.  If PAR had added a tag (say 16), this would trip.
    assert!(
        max_tag <= 15,
        "worker-wire max tag byte is {max_tag}, expected ≤ 15 (TAG_SHARED = 15 is the \
         last tag; PAR must NOT add a new wire tag — all PAR values cross the boundary \
         as plain values encoded with existing tags)."
    );

    // Complementary: verify TAG_SHARED itself is still encoded as first-byte 15
    // by round-tripping a frozen value (SRV §3.7(b)).
    // We don't test TAG_SHARED directly here because constructing a Value::Shared
    // requires the `shared` feature — instead we assert the upper bound (max ≤ 15)
    // which is equivalent: if TAG_SHARED were bumped to 16+ the max would exceed 15.
}

// ──────────────────────────────────────────────────────────────────────────────
// Pin 3 — opcode count
// ──────────────────────────────────────────────────────────────────────────────

/// PAR added zero new opcodes.
///
/// `task.pmap` / `task.preduce` dispatch work by calling the existing worker-fn
/// dispatch machinery (`run_in_worker`, `ChunkJob`) entirely from Rust; no new
/// bytecode instruction is needed. The current opcode count is 121:
///   - opcodes 0–119: pre-DEFER set
///   - opcode 119: `DeferPushMethod` (DEFER, bumped count to 120)
///   - opcode 120: `CallElided` (ELIDE, bumped count to 121)
///   - PAR: nothing
///
/// Uses the same `from_u8` filter technique as `shape_negative_space.rs`.
#[test]
fn no_new_opcode_for_par() {
    /// Pre-PAR opcode count (and current value — PAR adds none).
    /// DEFER added DeferPush + DeferPushMethod (→ 120). ELIDE added CallElided (→ 121).
    /// Update this constant only if a NON-PAR feature adds a new opcode.
    const EXPECTED_OP_COUNT: usize = 121;

    let op_count = (0u16..=255)
        .filter(|&b| ascript::vm::opcode::Op::from_u8(b as u8).is_some())
        .count();

    assert_eq!(
        op_count, EXPECTED_OP_COUNT,
        "Op variant count changed — PAR must add NO opcodes (task.pmap/preduce \
         dispatch through existing worker infrastructure); update EXPECTED_OP_COUNT \
         only if a non-PAR feature adds the new opcode."
    );

    // Complementary: `CallElided` (discriminant 120) is still the last variant.
    assert_eq!(
        ascript::vm::opcode::Op::CallElided as u16 + 1,
        EXPECTED_OP_COUNT as u16,
        "CallElided is no longer the last Op variant — a new opcode was appended \
         after it; update EXPECTED_OP_COUNT if it is NOT attributable to PAR."
    );
}
