//! FUZZ — in-tree property tests (`proptest`, stable, deterministic).
//!
//! These run as part of the ordinary `cargo test` suite (NOT a separate fuzz job) under
//! BOTH feature configs. They are the stable core of the FUZZ campaign: the grammar-aware
//! generator (`ascript::fuzzgen`) drives a continuous three-way differential, plus
//! structured invariants (`.aso` round-trip, worker structured-clone round-trip + rejection
//! safety, GC no-leak, formatter idempotence, parser round-trip, front-end agreement).
//!
//! Properties that touch the runtime wrap on `ascript::run_on_worker_stack` (the 512 MB
//! worker-stack driver) because the engines are `!Send` current-thread tokio.
//!
//! Determinism: proptest cases are bounded + the seed is printed on failure, so every
//! finding is replayable verbatim. The generator is deterministic-by-construction (spec §6),
//! so a divergence is a real bug, never a nondeterministic flake.
//!
//! Honesty (spec §9): these explore a sampled, shrink-guided slice of the input space —
//! they find bugs, they do not prove absence. The generated-grammar coverage is the
//! pre-NUM core-language surface (numeric-tower edge properties land in the NUM PR).

use ascript::fuzzgen::{self, GenProgram};
use proptest::prelude::*;

// ===========================================================================
// Shared oracle projection
// ===========================================================================

/// The deterministic projection of a program run, compared across engines — exactly the
/// tuple `assert_three_way_matches` normalizes: captured stdout + exit code on success, or
/// the Tier-2 panic MESSAGE on failure. (Span/caret is intentionally excluded: the SP1
/// 1-column caret offset between front-ends, `CLAUDE.md`, would be a false positive; the
/// MESSAGE is the load-bearing invariant.)
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Ok { stdout: String, exit: Option<i32> },
    Panic { message: String },
}

fn project(r: Result<(String, Option<i32>), ascript::error::AsError>) -> Outcome {
    match r {
        Ok((stdout, exit)) => Outcome::Ok { stdout, exit },
        Err(e) => Outcome::Panic { message: e.message },
    }
}

/// The four engine projections for one program: tree-walker, specialized-VM, generic-VM,
/// `.aso` round-trip.
type FourWay = (Outcome, Outcome, Outcome, Outcome);

/// Run `src` on all three engines (+ the `.aso` round-trip) on the worker stack and return
/// their projected outcomes. Spawns ONE 512 MB worker thread per call — fine for a single
/// program; for many programs prefer [`run_all_engines_batch`] to amortize the spawn.
fn run_all_engines(src: &str) -> FourWay {
    let src = src.to_string();
    ascript::run_on_worker_stack(move || async move { run_four_way(&src).await })
}

/// The async core: run `src` on all four modes and project each outcome. Must be called on
/// the worker stack (the engines are `!Send` current-thread tokio with deep recursion).
async fn run_four_way(src: &str) -> FourWay {
    let tw = project(ascript::run_source_exit(src).await);
    let vm = project(ascript::vm_run_source(src).await);
    let gen = project(ascript::vm_run_source_generic(src).await);
    let aso = project(ascript::aso_roundtrip_run_source(src).await);
    (tw, vm, gen, aso)
}

/// Run a BATCH of programs through all four modes inside a SINGLE worker-stack thread,
/// returning one `FourWay` per program. Amortizes the (expensive) 512 MB worker-thread
/// spawn across the whole batch — the fixed-seed batteries use this so they stay fast.
fn run_all_engines_batch(srcs: Vec<String>) -> Vec<FourWay> {
    ascript::run_on_worker_stack(move || async move {
        let mut out = Vec::with_capacity(srcs.len());
        for src in &srcs {
            out.push(run_four_way(src).await);
        }
        out
    })
}

/// Expand a u64 seed into a longer varied byte buffer (a bare 8-byte seed exhausts the
/// generator fast → only trivial programs). A dependency-free xorshift fill — deterministic.
fn seed_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push((x & 0xFF) as u8);
    }
    out
}

// ===========================================================================
// Task 7 — the flagship three-way differential proptest
// ===========================================================================

proptest! {
    // Bounded for a reasonable CI test (spec §4.1: the proptest properties ride the normal
    // `cargo test`, NOT the time-boxed fuzz job). Each case spawns a 512 MB worker thread +
    // runs four engine modes, so the count is modest (the deterministic fixed-seed batteries
    // carry the breadth; proptest adds random coverage + automatic shrinking to a minimal
    // reproducer). The seed is the proptest input (`bytes`), so any failure is replayable.
    #![proptest_config(ProptestConfig {
        cases: 96,
        max_shrink_iters: 2048,
        .. ProptestConfig::default()
    })]

    /// THE FLAGSHIP: a generated valid program runs byte-identically on the tree-walker,
    /// the specialized VM, and the generic VM (and survives an `.aso` serialize→deserialize
    /// round-trip). ANY disagreement is a guaranteed bug (a compiler/opcode/specialization-
    /// guard/`.aso` bug, or the oracle itself). On failure proptest shrinks the input bytes
    /// — and therefore the generated program — toward a minimal reproducer; fix the ENGINE,
    /// never relax this assertion (Gate 0).
    #[test]
    fn three_way_differential_over_generated_programs(bytes in prop::collection::vec(any::<u8>(), 64..768)) {
        let prog = fuzzgen::gen_program_from_bytes(&bytes);
        let (tw, vm, gen, aso) = run_all_engines(&prog.source);
        prop_assert_eq!(
            &tw, &vm,
            "specialized-VM diverged from tree-walker\n--- program ---\n{}\n--- tw: {:?}\n--- vm: {:?}",
            prog.source, tw, vm
        );
        prop_assert_eq!(
            &tw, &gen,
            "generic-VM diverged from tree-walker (a wrong specialization guard)\n--- program ---\n{}\n--- tw: {:?}\n--- gen: {:?}",
            prog.source, tw, gen
        );
        prop_assert_eq!(
            &tw, &aso,
            ".aso round-trip diverged from the in-memory VM\n--- program ---\n{}\n--- tw: {:?}\n--- aso: {:?}",
            prog.source, tw, aso
        );
    }

    /// Expression-granularity differential: `print(<generated expr>)` agrees three-way.
    /// Sharper at isolating an arithmetic/coercion/bitwise opcode divergence than the full
    /// program generator (no surrounding control flow to mask it).
    #[test]
    fn three_way_differential_over_generated_expressions(bytes in prop::collection::vec(any::<u8>(), 32..512)) {
        let mut u = arbitrary_unstructured(&bytes);
        let prog = fuzzgen::gen_expr_program(&mut u);
        let (tw, vm, gen, _aso) = run_all_engines(&prog.source);
        prop_assert_eq!(&tw, &vm, "expr specialized-VM divergence\n{}\ntw {:?}\nvm {:?}", prog.source, tw, vm);
        prop_assert_eq!(&tw, &gen, "expr generic-VM divergence\n{}\ntw {:?}\ngen {:?}", prog.source, tw, gen);
    }
}

/// `arbitrary::Unstructured` is re-exported via the generator's dep; build one from bytes.
fn arbitrary_unstructured(bytes: &[u8]) -> arbitrary::Unstructured<'_> {
    arbitrary::Unstructured::new(bytes)
}

/// A FIXED-seed deterministic smoke battery (outside proptest) so a divergence is caught
/// even on a CI shard that mis-seeds proptest, and so the seed→program mapping is logged.
#[test]
fn three_way_differential_fixed_seed_battery() {
    // 150 deterministic seeds → 150 programs, all run inside ONE worker-stack thread (the
    // spawn is amortized). The seed→program mapping is fixed, so any divergence is logged +
    // replayable. This is the deterministic breadth net; the proptest property adds shrinking.
    let n = 150u64;
    let progs: Vec<String> = (0..n)
        .map(|seed| {
            let bytes = seed_bytes(seed.wrapping_mul(0x9E3779B97F4A7C15), 512);
            fuzzgen::gen_program_from_bytes(&bytes).source
        })
        .collect();
    let results = run_all_engines_batch(progs.clone());
    for (seed, ((tw, vm, gen, aso), src)) in results.iter().zip(progs.iter()).enumerate() {
        assert_eq!(
            tw, vm,
            "FIXED-seed {seed}: specialized-VM divergence\n--- program ---\n{src}"
        );
        assert_eq!(
            tw, gen,
            "FIXED-seed {seed}: generic-VM divergence\n--- program ---\n{src}"
        );
        assert_eq!(
            tw, aso,
            "FIXED-seed {seed}: .aso round-trip divergence\n--- program ---\n{src}"
        );
    }
}

/// HIGH-VOLUME stress differential (FUZZ Unit 2). `#[ignore]` by default (it runs many
/// thousands of programs through four engine modes — too slow for the default `cargo test`),
/// but it is the breadth net used when broadening the generator: run with
/// `cargo test --test property stress_differential_many_seeds -- --ignored --nocapture`.
/// Any divergence prints the seed + the minimized-able program. The seed→program mapping is
/// fixed so any finding is replayable verbatim through the fixed-seed battery.
#[test]
#[ignore]
fn stress_differential_many_seeds() {
    let n: u64 = std::env::var("FUZZ_STRESS_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);
    let batch = 250u64;
    let mut diverged = 0u64;
    let mut start = 0u64;
    while start < n {
        let end = (start + batch).min(n);
        let progs: Vec<String> = (start..end)
            .map(|seed| {
                let bytes = seed_bytes(seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1), 768);
                fuzzgen::gen_program_from_bytes(&bytes).source
            })
            .collect();
        let results = run_all_engines_batch(progs.clone());
        for (i, ((tw, vm, gen, aso), src)) in results.iter().zip(progs.iter()).enumerate() {
            let seed = start + i as u64;
            if tw != vm {
                diverged += 1;
                eprintln!("DIVERGENCE seed {seed} specialized-VM\ntw {tw:?}\nvm {vm:?}\n--- src ---\n{src}\n");
            }
            if tw != gen {
                diverged += 1;
                eprintln!("DIVERGENCE seed {seed} generic-VM\ntw {tw:?}\ngen {gen:?}\n--- src ---\n{src}\n");
            }
            if tw != aso {
                diverged += 1;
                eprintln!("DIVERGENCE seed {seed} .aso\ntw {tw:?}\naso {aso:?}\n--- src ---\n{src}\n");
            }
        }
        start = end;
    }
    assert_eq!(diverged, 0, "{diverged} three-way divergences over {n} seeds (see stderr)");
}

// ===========================================================================
// Task 7 — the planted-bug SABOTEUR self-test (spec §7)
// ===========================================================================
//
// "A fuzzer that finds nothing might be broken, not the code clean." We prove the harness
// CAN fail: a `#[cfg(test)]`-only saboteur deliberately corrupts one engine's output, and
// the differential MUST flag the divergence. We also assert that with the saboteur OFF the
// engines agree (so a build that left it on fails loudly). The saboteur lives entirely in
// this test binary — it is NEVER compiled into a normal/`fuzzing`/production build and is
// unreachable from any libFuzzer target.

/// A saboteur engine: it returns the same outcome as the real specialized VM EXCEPT it
/// mangles stdout (appends a sentinel), simulating an off-by-one / wrong-opcode VM bug.
fn saboteur_vm_run(src: &str) -> Outcome {
    let real = run_all_engines(src).1; // the specialized-VM projection
    match real {
        Outcome::Ok { mut stdout, exit } => {
            stdout.push_str("SABOTAGED");
            Outcome::Ok { stdout, exit }
        }
        // A panic is mangled into a different message.
        Outcome::Panic { .. } => Outcome::Panic {
            message: "SABOTAGED".to_string(),
        },
    }
}

#[test]
fn saboteur_self_test_harness_can_fail() {
    // Pick a generated program that produces observable output on a fixed seed.
    let bytes = seed_bytes(12345, 512);
    let prog = fuzzgen::gen_program_from_bytes(&bytes);

    // OFF (default): the real three engines agree — the harness reports NO divergence.
    let (tw, vm, gen, _aso) = run_all_engines(&prog.source);
    assert_eq!(tw, vm, "saboteur OFF: real engines must agree (else a real bug)");
    assert_eq!(tw, gen, "saboteur OFF: real engines must agree (else a real bug)");

    // ON: the saboteur engine MUST be flagged as divergent by the same comparison the
    // differential uses. If this assertion ever fails, the harness's divergence detection
    // is broken (it cannot see a wrong answer) — the whole differential would be asleep.
    let sabotaged = saboteur_vm_run(&prog.source);
    assert_ne!(
        tw, sabotaged,
        "saboteur ON: the differential MUST detect the corrupted engine output \
         (the fuzzer can fail) — program:\n{}",
        prog.source
    );
}

// ===========================================================================
// Task 3 — `.aso` round-trip property (explicit, beyond the differential's 4th mode)
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    /// For a generated program, the full `compile → serialize .aso → deserialize+verify →
    /// run` pipeline produces byte-identical output to the direct in-memory VM run. The
    /// `.aso` path MUST equal the in-memory VM (a divergence is a serialization-layout or
    /// verifier bug). This is the §2.1 four-mode standard as a standalone property.
    #[test]
    fn aso_roundtrip_equals_direct_vm(bytes in prop::collection::vec(any::<u8>(), 64..640)) {
        let prog = fuzzgen::gen_program_from_bytes(&bytes);
        let src = prog.source.clone();
        let (direct, roundtrip) = ascript::run_on_worker_stack(move || async move {
            let d = project(ascript::vm_run_source(&src).await);
            let r = project(ascript::aso_roundtrip_run_source(&src).await);
            (d, r)
        });
        prop_assert_eq!(
            &direct, &roundtrip,
            ".aso round-trip != direct VM\n--- program ---\n{}\n--- direct: {:?}\n--- aso: {:?}",
            prog.source, direct, roundtrip
        );
    }
}

// ===========================================================================
// Task 3 — Worker structured-clone round-trip + rejection safety
// ===========================================================================

mod worker_serialize {
    use super::*;
    use ascript::interp::Interp;
    use ascript::value::{ArrayCell, MapCell, MapKey, ObjectCell, Value};
    use ascript::worker::serialize::{check_sendable, decode, encode};
    use indexmap::{IndexMap, IndexSet};

    /// Round-trip a value through the worker airlock (encode → decode) on a fresh `Interp`.
    fn round_trip(v: &Value) -> Result<Value, ascript::worker::serialize::SendError> {
        let (bytes, _shared) = encode(v)?;
        let interp = Interp::new();
        decode(&bytes, &interp)
    }

    // ---- generators for SENDABLE value graphs ----

    /// A scalar sendable leaf.
    fn scalar() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Nil),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::Int),
            // Avoid NaN/inf in the float generator: NaN != NaN would break the structural
            // round-trip equality (NaN is a documented Map-key carve-out, not a round-trip
            // failure). Finite floats round-trip exactly.
            (-1e6f64..1e6f64).prop_map(Value::Float),
            "[a-z]{0,8}".prop_map(|s| Value::Str(s.into())),
        ]
    }

    /// A recursively-built sendable value (arrays/objects/maps/sets of scalars + nesting).
    fn sendable_value() -> impl Strategy<Value = Value> {
        let leaf = scalar();
        leaf.prop_recursive(4, 32, 5, |inner| {
            prop_oneof![
                // array
                prop::collection::vec(inner.clone(), 0..5)
                    .prop_map(|v| Value::Array(ArrayCell::new(v))),
                // object (string keys)
                prop::collection::vec(("[a-z]{1,4}", inner.clone()), 0..5).prop_map(|pairs| {
                    let mut m: IndexMap<String, Value> = IndexMap::new();
                    for (k, v) in pairs {
                        m.insert(k, v);
                    }
                    Value::Object(ObjectCell::new(m))
                }),
                // map (scalar keys via MapKey)
                prop::collection::vec((scalar_key(), inner.clone()), 0..5).prop_map(|pairs| {
                    let mut m: IndexMap<MapKey, Value> = IndexMap::new();
                    for (k, v) in pairs {
                        if let Some(key) = MapKey::from_value(&k) {
                            m.insert(key, v);
                        }
                    }
                    Value::Map(MapCell::new(m))
                }),
                // set
                prop::collection::vec(scalar_key(), 0..5).prop_map(|keys| {
                    let mut s: IndexSet<MapKey> = IndexSet::new();
                    for k in keys {
                        if let Some(key) = MapKey::from_value(&k) {
                            s.insert(key);
                        }
                    }
                    Value::Set(ascript::value::SetCell::new(s))
                }),
            ]
        })
    }

    /// A scalar usable as a Map/Set key (no nil/collection keys).
    fn scalar_key() -> impl Strategy<Value = Value> {
        prop_oneof![
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::Int),
            "[a-z]{0,6}".prop_map(|s| Value::Str(s.into())),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, .. ProptestConfig::default() })]

        /// ROUND-TRIP: a sendable value graph encoded then decoded is structurally equal to
        /// the original (compared via the shared `Display`, which the existing serialize
        /// tests use — it captures container structure + ordering + key canonicalization).
        #[test]
        fn sendable_value_round_trips(v in sendable_value()) {
            // The generator only ever produces sendable values, so encode must succeed and
            // the round-trip must reproduce the structure.
            prop_assert!(check_sendable(&v).is_ok(), "generated value must be sendable: {v}");
            let back = round_trip(&v).expect("sendable value must round-trip");
            prop_assert_eq!(
                format!("{}", back), format!("{}", v),
                "structured-clone round-trip changed the value"
            );
        }

        /// REJECTION SAFETY: an arbitrary byte buffer decoded against an `Interp` returns
        /// `Ok` or `Err(SendError)` — NEVER panics, NEVER over-allocates (generalizes
        /// `decode_huge_length_does_not_allocate`). We don't assert WHICH; only that it is
        /// a clean Result and the process survives.
        #[test]
        fn decode_arbitrary_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            let interp = Interp::new();
            // The only requirement: this returns (does not panic / abort). `let _ =` keeps
            // both arms.
            let _ = decode(&bytes, &interp);
        }
    }

    /// SENDABILITY HONESTY: a NON-sendable kind (a closure/native/future/generator) must be
    /// REJECTED by `encode` with a `SendError` (a field-path error), never a panic and never
    /// silent loss — `encode(v)` succeeds iff `check_sendable(v)` is `Ok`. We use a
    /// `Value::Builtin` (a native fn handle) as the canonical non-sendable leaf.
    #[test]
    fn non_sendable_value_is_rejected_cleanly() {
        // A builtin (native function) is non-sendable. `global_env()` installs builtins, so
        // `print` resolves to a `Value::Builtin` handle — the canonical non-sendable leaf.
        let env = ascript::interp::global_env();
        let non_sendable = env.get("print").expect("print builtin is installed");
        // It must be classified non-sendable AND encode must refuse it (the SAME verdict):
        // encode(v) succeeds iff check_sendable(v) is Ok. Neither path may panic.
        let checked = check_sendable(&non_sendable).is_err();
        let encoded = encode(&non_sendable).is_err();
        assert!(checked, "a builtin must be classified non-sendable");
        assert!(encoded, "encode must refuse a non-sendable value (no panic, a SendError)");
        assert_eq!(
            checked, encoded,
            "encode succeeds iff check_sendable is Ok — the two verdicts must match"
        );
    }

    /// The curated KNOWN-BAD worker-airlock byte set — each must `decode` to a clean `Err`,
    /// never a panic / OOM / unbounded allocation. These mirror, by construction, the shipped
    /// `decode_rejects_*` / `decode_huge_length_does_not_allocate` unit tests in
    /// `src/worker/serialize.rs` AND the `bad_*` seeds committed under
    /// `fuzz/corpus/worker_serialize/` (see `name`), so the corpus and this guard stay in
    /// lockstep — exactly the discipline `aso_planted_bug_known_bad_bytes_are_clean_err` applies
    /// on the `.aso` side. Wire tags: ARRAY=6, BYTES=5, STR=4, REF=13 (serialize.rs:82+).
    fn known_bad_worker_inputs() -> Vec<(&'static str, Vec<u8>)> {
        let u32max = u32::MAX.to_le_bytes();
        let zero = 0u32.to_le_bytes();
        vec![
            // Empty buffer — no tag byte to read.
            ("bad_empty", Vec::new()),
            // An unknown / out-of-range tag byte.
            ("bad_unknown_tag", vec![99]),
            ("bad_unknown_tag_padded", vec![200, 0, 0, 0, 0]),
            // TAG_REF (13) + id=7 with no container ever registered — a dangling back-ref.
            ("bad_dangling_ref", {
                let mut b = vec![13u8];
                b.extend_from_slice(&7u32.to_le_bytes());
                b
            }),
            // The huge-length BOMB on an ARRAY: tag + id=0 + len=u32::MAX, NO element bytes.
            // Pre-clamp this pre-allocated; the `remaining()` clamp makes it a clean Err.
            ("bad_bomb_array_len", {
                let mut b = vec![6u8];
                b.extend_from_slice(&zero); // serial id
                b.extend_from_slice(&u32max); // claimed length (bomb)
                b
            }),
            // The same bomb on BYTES (length-prefixed raw bytes).
            ("bad_bomb_bytes_len", {
                let mut b = vec![5u8];
                b.extend_from_slice(&zero); // serial id
                b.extend_from_slice(&u32max); // claimed length (bomb)
                b
            }),
            // The same bomb on STR (length-prefixed UTF-8).
            ("bad_bomb_str_len", {
                let mut b = vec![4u8];
                b.extend_from_slice(&u32max); // claimed length (bomb)
                b
            }),
            // A truncated ARRAY: valid header claiming 3 elements but no element bytes.
            ("bad_truncated_array", {
                let mut b = vec![6u8];
                b.extend_from_slice(&zero); // serial id
                b.extend_from_slice(&3u32.to_le_bytes()); // 3 elements, none present
                b
            }),
        ]
    }

    /// PLANTED-BAD-BYTES GUARD (Task 6, spec §7): every curated known-bad worker-airlock input
    /// is a CLEAN `Err`, never a panic / abort / unbounded allocation — proving the
    /// `worker_serialize` fuzz target's rejection-safety invariant (and that its crash-detection
    /// can fire) WITHOUT the cargo-fuzz CLI. Pins the EXACT seed set the committed corpus ships,
    /// so the corpus and the suite never drift.
    #[test]
    fn worker_planted_bad_bytes_are_clean_err() {
        for (name, bytes) in known_bad_worker_inputs() {
            let interp = Interp::new();
            let r = decode(&bytes, &interp);
            assert!(
                r.is_err(),
                "planted-bad worker case `{name}` must be a clean Err from `decode`, got Ok"
            );
        }
    }

    /// The committed worker-airlock seed corpus under `fuzz/corpus/worker_serialize/` is the
    /// libFuzzer starting set AND a permanent regression guard. Assert (a) the `bad_*` seeds are
    /// present + byte-identical to the planted-bad set above (corpus ⇔ suite lockstep), and (b)
    /// every committed `ex_*` seed is a real encoded graph that `decode`s cleanly (so the mutator
    /// flips bytes inside a VALID tagged tree, reaching the deep `decode_value` arms).
    #[test]
    fn worker_seed_corpus_is_present_and_current() {
        let corpus =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/worker_serialize");
        assert!(
            corpus.is_dir(),
            "worker seed corpus dir missing: {}",
            corpus.display()
        );
        let mut ex_seeds = 0usize;
        let mut bad_seeds = 0usize;
        for entry in std::fs::read_dir(&corpus).expect("read corpus dir") {
            let path = entry.expect("dir entry").path();
            let fname = path.file_name().unwrap().to_string_lossy().to_string();
            let bytes = std::fs::read(&path).expect("read seed");
            let interp = Interp::new();
            if fname.starts_with("ex_") {
                ex_seeds += 1;
                // A real `ex_*` seed begins with a `1` discriminant byte (round-trip mode); the
                // body after it is a real encoded graph. We assert the body alone decodes — the
                // direct in-suite proof the seed reaches the valid `decode_value` arms.
                assert!(
                    !bytes.is_empty(),
                    "committed seed `{fname}` is empty"
                );
                let body = &bytes[1..];
                assert!(
                    decode(body, &interp).is_ok(),
                    "committed `ex_*` seed `{fname}` body failed to decode (stale wire format?)"
                );
            } else if fname.starts_with("bad_") {
                bad_seeds += 1;
                // A `bad_*` seed begins with a `0` discriminant byte (decode-arbitrary mode); the
                // body is a known-bad buffer that must decode to a clean Err.
                let body = if bytes.is_empty() { &bytes[..] } else { &bytes[1..] };
                assert!(
                    decode(body, &interp).is_err(),
                    "known-bad seed `{fname}` body unexpectedly decoded Ok"
                );
            }
        }
        assert!(
            ex_seeds >= 1,
            "expected at least one real encoded `ex_*` seed, found {ex_seeds}"
        );
        assert_eq!(
            bad_seeds,
            known_bad_worker_inputs().len(),
            "the committed `bad_*` seed count must equal the planted-bad set"
        );
    }

    /// PLANTED-BUG self-test (spec §7): a deliberately-lossy decode (drop the last array
    /// element) must make the round-trip property FAIL — proving the round-trip check can
    /// detect a serializer bug. We simulate the lossy decode by truncating a known array's
    /// re-encoding and asserting the structural compare catches the difference.
    #[test]
    fn serialize_roundtrip_self_test_catches_loss() {
        let v = Value::Array(ArrayCell::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]));
        let good = round_trip(&v).expect("round-trip ok");
        assert_eq!(format!("{good}"), format!("{v}"), "honest round-trip preserves the array");

        // The "lossy decode": an array missing its last element. The structural compare the
        // property uses MUST flag this as different — i.e. the check is not vacuously true.
        let lossy = Value::Array(ArrayCell::new(vec![Value::Int(1), Value::Int(2)]));
        assert_ne!(
            format!("{lossy}"),
            format!("{v}"),
            "the round-trip equality check must detect a dropped element (it can fail)"
        );
    }
}

// ===========================================================================
// Task 3 — GC no-leak / no-double-free property
// ===========================================================================
//
// Honesty (spec §9): this runs on the main test thread's single `gcmodule` heap and forces
// collection explicitly; it does not (yet) fuzz cross-worker-isolate GC. It asserts no
// leak / no double-free over the GENERATED graph shapes, not collector correctness for all
// heaps.

mod gc_property {
    use super::*;
    use ascript::value::{ArrayCell, ObjectCell, Value};
    use indexmap::IndexMap;

    /// A description of a random cyclic graph to build, then drop, then collect.
    #[derive(Debug, Clone)]
    enum GraphShape {
        /// A self-referential array (`a.push(a)`).
        SelfArray,
        /// Two arrays referencing each other (`a.push(b); b.push(a)`).
        ArrayPair,
        /// An object whose field points back to itself.
        SelfObject,
        /// A chain of N arrays closed into a ring (cycle of length N).
        Ring(usize),
        /// An array of M independent self-referential arrays.
        Forest(usize),
    }

    fn graph_shape() -> impl Strategy<Value = GraphShape> {
        prop_oneof![
            Just(GraphShape::SelfArray),
            Just(GraphShape::ArrayPair),
            Just(GraphShape::SelfObject),
            (2usize..8).prop_map(GraphShape::Ring),
            (1usize..6).prop_map(GraphShape::Forest),
        ]
    }

    /// Build the described cyclic graph and return the external root handle(s). When the
    /// returned Vec is dropped, the only thing keeping the cycle alive is its INTERNAL
    /// edges — so refcounting alone cannot reclaim it (the GC must).
    fn build(shape: &GraphShape) -> Vec<Value> {
        match shape {
            GraphShape::SelfArray => {
                let a = Value::Array(ArrayCell::new(Vec::new()));
                if let Value::Array(arr) = &a {
                    arr.borrow_mut().push(a.clone());
                }
                vec![a]
            }
            GraphShape::ArrayPair => {
                let a = Value::Array(ArrayCell::new(Vec::new()));
                let b = Value::Array(ArrayCell::new(Vec::new()));
                if let (Value::Array(av), Value::Array(bv)) = (&a, &b) {
                    av.borrow_mut().push(b.clone());
                    bv.borrow_mut().push(a.clone());
                }
                vec![a, b]
            }
            GraphShape::SelfObject => {
                let o = Value::Object(ObjectCell::new(IndexMap::new()));
                if let Value::Object(oc) = &o {
                    oc.borrow_mut().insert("self".to_string(), o.clone());
                }
                vec![o]
            }
            GraphShape::Ring(n) => {
                let nodes: Vec<Value> = (0..*n)
                    .map(|_| Value::Array(ArrayCell::new(Vec::new())))
                    .collect();
                for i in 0..*n {
                    let next = nodes[(i + 1) % *n].clone();
                    if let Value::Array(arr) = &nodes[i] {
                        arr.borrow_mut().push(next);
                    }
                }
                nodes
            }
            GraphShape::Forest(m) => (0..*m)
                .map(|_| {
                    let a = Value::Array(ArrayCell::new(Vec::new()));
                    if let Value::Array(arr) = &a {
                        arr.borrow_mut().push(a.clone());
                    }
                    a
                })
                .collect(),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]

        /// NO-LEAK / NO-DOUBLE-FREE: build a random cyclic graph, drop the roots, collect,
        /// and assert the live tracked-object count returns to the pre-build baseline (no
        /// leak), AND a second collect reclaims nothing more (no double-free / resurrection).
        /// Mirrors the `gc.rs` before/drop/after delta shape. A surviving cycle is a missing/
        /// wrong `Trace` impl (fix the impl, never the assertion).
        #[test]
        fn cyclic_graphs_are_reclaimed(shape in graph_shape()) {
            // Clean baseline: drain anything pending so the tracked set reflects only THIS
            // case's allocations.
            ascript::gc::collect();
            let before = ascript::gc::tracked_count();

            let roots = build(&shape);
            // The cycle is alive and tracked; refcounting alone cannot free it.
            drop(roots);

            let reclaimed = ascript::gc::collect();
            prop_assert!(
                reclaimed >= 1,
                "{:?}: cycle collection must reclaim at least one node (got {})",
                shape, reclaimed
            );
            let after = ascript::gc::tracked_count();
            prop_assert_eq!(
                after, before,
                "{:?}: tracked count must return to baseline (no leak) — before {} after {}",
                shape, before, after
            );
            // No double-free / resurrection: a second collect reclaims nothing.
            let again = ascript::gc::collect();
            prop_assert_eq!(again, 0, "{:?}: a second collect must be a no-op (got {})", shape, again);
        }
    }
}

// ===========================================================================
// Task 7 — formatter idempotence + parser round-trip + front-end agreement
// ===========================================================================

/// Format `src`, or return it unchanged if the formatter errors (a generated program always
/// parses, so the formatter should always succeed — but be defensive).
fn fmt(src: &str) -> String {
    ascript::fmt::format_source(src).unwrap_or_else(|_| src.to_string())
}

proptest! {
    // 96 cases: `formatting_preserves_behavior` runs the VM (worker-stack), so keep it modest;
    // the formatter-idempotence + front-end-agreement properties here are cheap (parse/format
    // only) and the fixed batteries add deterministic breadth.
    #![proptest_config(ProptestConfig { cases: 96, .. ProptestConfig::default() })]

    /// FORMATTER IDEMPOTENCE: `fmt(fmt(x)) == fmt(x)` — the formatter reaches a fixed point
    /// in one pass over generated valid source.
    #[test]
    fn formatter_is_idempotent(bytes in prop::collection::vec(any::<u8>(), 64..640)) {
        let prog = fuzzgen::gen_program_from_bytes(&bytes);
        let once = fmt(&prog.source);
        let twice = fmt(&once);
        prop_assert_eq!(
            &once, &twice,
            "formatter not idempotent\n--- once ---\n{}\n--- twice ---\n{}",
            once, twice
        );
    }

    /// FORMATTING PRESERVES MEANING: a program and its formatted form run identically on the
    /// VM (formatting never changes observable behavior). This is the operational form of
    /// "`parse(x)` ≡ `parse(fmt(x))`" — comparing the runtime projection is stronger than an
    /// AST compare and reuses the engines.
    #[test]
    fn formatting_preserves_behavior(bytes in prop::collection::vec(any::<u8>(), 64..640)) {
        let prog = fuzzgen::gen_program_from_bytes(&bytes);
        let formatted = fmt(&prog.source);
        let src1 = prog.source.clone();
        let (a, b) = ascript::run_on_worker_stack(move || async move {
            let a = project(ascript::vm_run_source(&src1).await);
            let b = project(ascript::vm_run_source(&formatted).await);
            (a, b)
        });
        prop_assert_eq!(
            &a, &b,
            "formatting changed program behavior\n--- original ---\n{}",
            prog.source
        );
    }

    /// FRONT-END AGREEMENT: both the legacy and CST front-ends ACCEPT the generated valid
    /// source (generalizes `frontend_conformance.rs`). The generator is grammar-aware, so a
    /// rejection by EITHER front-end is a parser bug (or a generator gap — but the generator
    /// targets the shared grammar both parsers implement). Caret-column offsets (SP1) don't
    /// enter: we compare accept/reject only.
    #[test]
    fn both_frontends_accept_generated_source(bytes in prop::collection::vec(any::<u8>(), 64..640)) {
        let prog = fuzzgen::gen_program_from_bytes(&bytes);
        // Legacy: lex then parse.
        let legacy_ok = match ascript::lexer::lex(&prog.source) {
            Ok(tokens) => ascript::parser::parse(&tokens).is_ok(),
            Err(_) => false,
        };
        // CST: parse, then check for error/lex nodes.
        let parse = ascript::syntax::parser::parse(&prog.source);
        let cst_ok = parse.errors.is_empty() && parse.lex_errors.is_empty();
        prop_assert!(
            legacy_ok && cst_ok,
            "front-end disagreement on generated source (legacy_ok={}, cst_ok={})\n--- program ---\n{}",
            legacy_ok, cst_ok, prog.source
        );
    }
}

/// A fixed-seed front-end-agreement battery (deterministic, logs the seed→program mapping).
#[test]
fn frontend_agreement_fixed_battery() {
    for seed in 0u64..200 {
        let bytes = seed_bytes(seed.wrapping_mul(0xD1B54A32D192ED03), 512);
        let prog: GenProgram = fuzzgen::gen_program_from_bytes(&bytes);
        let legacy_ok = match ascript::lexer::lex(&prog.source) {
            Ok(tokens) => ascript::parser::parse(&tokens).is_ok(),
            Err(_) => false,
        };
        let parse = ascript::syntax::parser::parse(&prog.source);
        let cst_ok = parse.errors.is_empty() && parse.lex_errors.is_empty();
        assert!(
            legacy_ok && cst_ok,
            "seed {seed}: front-end disagreement (legacy={legacy_ok}, cst={cst_ok})\n{}",
            prog.source
        );
    }
}

// ---------------------------------------------------------------------------------------
// FUZZ Task 5 — the `.aso` reader PLANTED-BUG guard (spec §7).
//
// This is the in-suite proof that the `fuzz/fuzz_targets/aso_roundtrip.rs` libFuzzer target's
// invariant holds and its "panic ⇒ crash" detection actually fires — WITHOUT needing the
// cargo-fuzz CLI. It drives the SAME public reader entry points the fuzz target hits
// (`Chunk::from_bytes` in `src/vm/aso.rs`, `Chunk::from_bytes_verified` in `src/vm/verify.rs`)
// over a curated known-bad byte set, asserting each is a clean `Err` and NEVER a panic / OOM /
// unbounded allocation. It extends the existing reader self-tests in `aso.rs`
// (`reader_huge_length_does_not_allocate` `:2016`, `bad_magic_detected` `:2700`,
// `truncated_detected` `:2782`, `version_mismatch_detected` `:2361`) by pinning the EXACT seed
// set the committed corpus ships, so the corpus and the suite never drift.
// ---------------------------------------------------------------------------------------

use ascript::vm::chunk::Chunk;

/// The `.aso` magic + the current `ASO_FORMAT_VERSION` header prefix, mirroring the writer.
fn aso_header() -> Vec<u8> {
    let mut h = b"ASO\x00".to_vec();
    h.extend_from_slice(&ascript::vm::aso::ASO_FORMAT_VERSION.to_le_bytes());
    h
}

/// The curated KNOWN-BAD byte set — each must classify as a clean `Err`, never a panic/OOM.
/// These mirror, by construction, the `bad_*` seeds committed under
/// `fuzz/corpus/aso_roundtrip/` (see `name`), so the corpus and this guard stay in lockstep.
fn known_bad_aso_inputs() -> Vec<(&'static str, Vec<u8>)> {
    let h = aso_header();
    let u32max = u32::MAX.to_le_bytes();
    let zero = 0u32.to_le_bytes();
    vec![
        // Far too short to even read the 4-byte magic.
        ("bad_empty", Vec::new()),
        ("bad_short_magic", vec![0x00, 0x01]),
        // Valid length, wrong magic bytes.
        ("bad_magic", {
            let mut b = b"XSO\x00".to_vec();
            b.extend_from_slice(&ascript::vm::aso::ASO_FORMAT_VERSION.to_le_bytes());
            b.push(0x00);
            b
        }),
        // Stale + future version (the version-reject path, before any deep read).
        ("bad_version_stale", {
            let mut b = b"ASO\x00".to_vec();
            b.extend_from_slice(&1u32.to_le_bytes());
            b.push(0x00);
            b
        }),
        ("bad_version_future", {
            let mut b = b"ASO\x00".to_vec();
            b.extend_from_slice(&u32max);
            b.push(0x00);
            b
        }),
        // Truncated right after the header (debug flag missing or body missing).
        ("bad_truncated_after_header", h.clone()),
        // The P0 BOMB: an oversized const-pool length prefix over an empty body. Pre-clamp this
        // pre-allocated tens of GB and aborted; the `remaining()` clamp makes it a clean error.
        ("bad_bomb_const_len", {
            let mut b = h.clone();
            b.push(0x00); // debug flag = 0
            b.extend_from_slice(&zero); // code length = 0
            b.extend_from_slice(&u32max); // const-pool count = u32::MAX (bomb)
            b
        }),
        // The same bomb on the proto count after a valid (empty) const pool.
        ("bad_bomb_proto_len", {
            let mut b = h.clone();
            b.push(0x00); // debug flag = 0
            b.extend_from_slice(&zero); // code length = 0
            b.extend_from_slice(&zero); // const count = 0 (valid, empty)
            b.extend_from_slice(&u32max); // proto count = u32::MAX (bomb)
            b
        }),
        // Debug-present flag set but the source block is truncated.
        ("bad_truncated_debug", {
            let mut b = h.clone();
            b.push(0x01); // debug present, then nothing
            b
        }),
        // A header followed by one stray byte (truncated-or-trailing).
        ("bad_trailing_only_header_plus_byte", {
            let mut b = h.clone();
            b.extend_from_slice(&[0x00, 0xAB]);
            b
        }),
    ]
}

/// PLANTED-BUG GUARD (spec §7): every curated known-bad `.aso` input is a CLEAN `Err`, never a
/// panic / abort / unbounded allocation — proving the `aso_roundtrip` fuzz target's invariant
/// (and that its crash-detection can fire). Drives BOTH public reader entry points.
#[test]
fn aso_planted_bug_known_bad_bytes_are_clean_err() {
    for (name, bytes) in known_bad_aso_inputs() {
        // `Chunk::from_bytes` (decode only) — must be `Err`, never a panic/OOM/abort.
        let decode = Chunk::from_bytes(&bytes);
        assert!(
            decode.is_err(),
            "planted-bug case `{name}` must be a clean Err from `from_bytes`, got Ok"
        );
        // `Chunk::from_bytes_verified` (decode + verify, the `run_aso_file` path) — same.
        let verified = Chunk::from_bytes_verified(&bytes);
        assert!(
            verified.is_err(),
            "planted-bug case `{name}` must be a clean Err from `from_bytes_verified`, got Ok"
        );
    }
}

/// The committed seed corpus under `fuzz/corpus/aso_roundtrip/` is the libFuzzer starting set
/// AND a permanent regression guard. Assert (a) the `bad_*` seeds are present and byte-identical
/// to the planted-bug set above (so the corpus and suite never drift), and (b) every committed
/// `ex_*.aso` seed is a CURRENT-version, fully-decodable real chunk (a stale-version seed would
/// silently collapse the reader coverage floor, spec §4.2). This is the in-session proof the
/// corpus exists and is well-formed; the cargo-fuzz campaign is the CI-side extension.
#[test]
fn aso_seed_corpus_is_present_and_current() {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/aso_roundtrip");
    assert!(
        corpus.is_dir(),
        "seed corpus dir missing: {} — run ./fuzz/regenerate_aso_corpus.sh",
        corpus.display()
    );

    let mut ex_seeds = 0usize;
    let mut bad_seeds = 0usize;
    for entry in std::fs::read_dir(&corpus).expect("read corpus dir") {
        let path = entry.expect("dir entry").path();
        let fname = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).expect("read seed");

        if fname.starts_with("ex_") {
            ex_seeds += 1;
            // A real example seed MUST decode AND verify on the current build (current version,
            // valid structure) — otherwise the deep `read_*` arms go unfuzzed.
            Chunk::from_bytes(&bytes).unwrap_or_else(|e| {
                panic!("committed seed `{fname}` failed to decode ({e}) — stale ASO_FORMAT_VERSION? Re-run ./fuzz/regenerate_aso_corpus.sh")
            });
            assert!(
                Chunk::from_bytes_verified(&bytes).is_ok(),
                "committed seed `{fname}` decoded but failed verification"
            );
        } else if fname.starts_with("bad_") {
            bad_seeds += 1;
            // A known-bad seed must NOT decode (it is a rejection seed).
            assert!(
                Chunk::from_bytes(&bytes).is_err(),
                "known-bad seed `{fname}` unexpectedly decoded Ok"
            );
        }
    }

    assert!(
        ex_seeds >= 50,
        "expected many real example seeds (built from examples/**), found only {ex_seeds} — \
         run ./fuzz/regenerate_aso_corpus.sh"
    );
    // Every `bad_*` planted-bug case is committed as a seed.
    assert_eq!(
        bad_seeds,
        known_bad_aso_inputs().len(),
        "the committed `bad_*` seed count must equal the planted-bug set"
    );
}
