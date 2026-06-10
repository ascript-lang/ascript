//! FUZZ Task 6 — the worker structured-clone airlock (`src/worker/serialize.rs`).
//!
//! The worker serializer is THE airlock that keeps the runtime `!Send`: it is the only
//! thing that crosses an isolate boundary, and it parses untrusted bytes on `decode`. This
//! libFuzzer target asserts the airlock's three load-bearing invariants (spec §2.3) over an
//! unbounded input stream. Two sub-modes share one entry (a leading discriminant byte routes
//! between them, so libFuzzer reaches BOTH from one corpus):
//!
//!   (a) ROUND-TRIP + SENDABILITY HONESTY — an `arbitrary`-built *sendable* `Value` graph
//!       (incl. cycles) is encoded then decoded and must be structurally equal to the
//!       original. AND the LOAD-BEARING coupling: `encode(v)` succeeds **iff**
//!       `check_sendable(v)` is `Ok` — a non-sendable value (closure / native handle /
//!       future / generator) must be a CLEAN field-path `SendError`, NEVER a panic and NEVER
//!       silent loss. We also synthesize non-sendable leaves to exercise the rejection arm.
//!
//!   (b) DECODE-ARBITRARY-BYTES — raw libFuzzer bytes straight into `decode`: only
//!       `Ok(Value)` / `Err(SendError)`, NEVER a panic / OOB / unbounded allocation / hang
//!       (generalizes `decode_rejects_*` / `decode_huge_length_does_not_allocate`).
//!
//! `Value` is `!Send`/`Rc`+`Cc`-based, so the graph is built WITHIN the target on the
//! current (single) libFuzzer thread — no `Value` ever crosses a thread here. The generator
//! is a small bounded recursive builder local to this target (there is no shared `Value`
//! `Arbitrary` generator in `src/fuzzgen`, which emits SOURCE; a fuzz-only `Arbitrary` impl
//! must never burden production `Value`, so it lives here).
//!
//! The in-suite proof of these invariants (no cargo-fuzz needed) lives in the NORMAL suite:
//! `tests/property.rs::worker_serialize` — the sendability-honesty + round-trip + rejection
//! properties + the planted-lossy-decode self-test. This target is the coverage-guided
//! extension; CI runs it from the committed `fuzz/corpus/worker_serialize/` seeds.
//!
//! Seed corpus: `fuzz/corpus/worker_serialize/` — `bad_*` mirror the shipped `decode_rejects_*`
//! byte buffers, `ex_*` are real encoded graphs (so the mutator flips bytes inside a valid
//! tagged tree, reaching the deep `decode_value` arms).

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use ascript::interp::Interp;
use ascript::value::{ArrayCell, MapCell, MapKey, ObjectCell, SetCell, Value};
use ascript::worker::serialize::{check_sendable, decode, encode};
use indexmap::{IndexMap, IndexSet};

// LeakSanitizer: this target builds + serializes managed-runtime `Value` graphs, whose
// process-lifetime state (interned strings, lazy_static/once_cell globals, gcmodule internals,
// thread-locals) is NOT a leak. Disable LSan for THIS binary — the libFuzzer `-detect_leaks=0`
// flag does NOT stop the shutdown LSan check; only this compiled-in default does.
#[no_mangle]
pub extern "C" fn __lsan_default_options() -> *const std::os::raw::c_char {
    c"detect_leaks=0".as_ptr()
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    // A leading discriminant byte routes between the two sub-modes so one corpus reaches both.
    let (mode, rest) = (data[0], &data[1..]);
    if mode & 1 == 0 {
        // (b) DECODE-ARBITRARY-BYTES — raw bytes → decode → only Ok/Err, never a panic/OOB/OOM.
        // CALLING it under the fuzzer IS the assertion (libFuzzer flags any panic/abort/hang).
        let interp = Interp::new();
        let _ = decode(rest, &interp);
    } else {
        // (a) ROUND-TRIP + SENDABILITY HONESTY — build a bounded Value graph, then assert the
        // encode⇔check_sendable coupling and (for the sendable case) structural round-trip.
        let mut u = Unstructured::new(rest);
        let mut depth = 6u32;
        let v = build_value(&mut u, &mut depth);
        check_invariants(&v);
    }
});

/// The two load-bearing invariants (spec §2.3), asserted on EVERY built graph:
///   1. SENDABILITY HONESTY: `encode(v).is_ok() == check_sendable(v).is_ok()` — the static
///      check and the actual serializer agree, always; neither may panic.
///   2. ROUND-TRIP (when sendable): `decode(encode(v))` is structurally equal to `v`.
///
/// A violation of either is a serializer/airlock bug (libFuzzer records the input).
fn check_invariants(v: &Value) {
    let sendable = check_sendable(v).is_ok();
    let encoded = encode(v);
    assert_eq!(
        sendable,
        encoded.is_ok(),
        "sendability honesty violated: check_sendable={sendable} but encode.is_ok()={}",
        encoded.is_ok()
    );
    if let Ok((bytes, shared)) = encoded {
        // A non-empty `shared` side-vector only arises for a frozen `Value::Shared`, which the
        // builder below never produces; use the shared-aware decode so the assertion is total.
        let interp = Interp::new();
        let back = ascript::worker::serialize::decode_with_shared(&bytes, &shared, &interp)
            .expect("a sendable value must decode (round-trip)");
        // Compare via the shared `Display` — exactly what the shipped serialize round-trip
        // tests use; it captures container structure + ordering + key canonicalization.
        assert_eq!(
            format!("{back}"),
            format!("{v}"),
            "structured-clone round-trip changed the value"
        );
    }
}

/// A small BOUNDED recursive `Value` builder driven by `arbitrary::Unstructured`. It can
/// build: sendable scalars (nil/bool/int/float/str), sendable containers
/// (array/object/map/set), a SELF-REFERENTIAL array cycle, AND (rarely) a NON-sendable leaf
/// (a builtin native-fn handle) so the rejection arm of invariant 1 is exercised. The depth
/// budget decremented per recursion guarantees termination.
fn build_value(u: &mut Unstructured, depth: &mut u32) -> Value {
    // Out of budget or bytes → a trivial sendable leaf.
    if *depth == 0 || u.is_empty() {
        return Value::Nil;
    }
    *depth -= 1;
    let choice = u8::arbitrary(u).unwrap_or(0) % 10;
    match choice {
        0 => Value::Nil,
        1 => Value::Bool(bool::arbitrary(u).unwrap_or(false)),
        // Numeric edges matter: bias toward the i64/float boundaries.
        2 => Value::Int(int_edge(u)),
        3 => Value::Float(f64_finite(u)),
        4 => Value::Str(short_str(u).into()),
        5 => {
            // array
            let n = (u8::arbitrary(u).unwrap_or(0) % 5) as usize;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                items.push(build_value(u, depth));
            }
            Value::Array(ArrayCell::new(items))
        }
        6 => {
            // object (string keys)
            let n = (u8::arbitrary(u).unwrap_or(0) % 5) as usize;
            let mut m: IndexMap<String, Value> = IndexMap::new();
            for _ in 0..n {
                let k = short_str(u);
                let val = build_value(u, depth);
                m.insert(k, val);
            }
            Value::Object(ObjectCell::new(m))
        }
        7 => {
            // map (scalar keys via MapKey canonicalization)
            let n = (u8::arbitrary(u).unwrap_or(0) % 5) as usize;
            let mut m: IndexMap<MapKey, Value> = IndexMap::new();
            for _ in 0..n {
                let kv = scalar_key(u);
                let val = build_value(u, depth);
                if let Some(key) = MapKey::from_value(&kv) {
                    m.insert(key, val);
                }
            }
            Value::Map(MapCell::new(m))
        }
        8 => {
            // set
            let n = (u8::arbitrary(u).unwrap_or(0) % 5) as usize;
            let mut s: IndexSet<MapKey> = IndexSet::new();
            for _ in 0..n {
                let kv = scalar_key(u);
                if let Some(key) = MapKey::from_value(&kv) {
                    s.insert(key);
                }
            }
            Value::Set(SetCell::new(s))
        }
        9 => {
            // Either a self-referential array CYCLE (sendable; the airlock must preserve
            // topology) or a NON-sendable leaf — both stress an invariant arm.
            if bool::arbitrary(u).unwrap_or(false) {
                let a = Value::Array(ArrayCell::new(Vec::new()));
                if let Value::Array(arr) = &a {
                    arr.borrow_mut().push(a.clone());
                    // Add one more sendable element so the cycle is non-trivial.
                    let extra = build_value(u, depth);
                    arr.borrow_mut().push(extra);
                }
                a
            } else {
                // A builtin (native fn) handle is the canonical NON-sendable leaf — its
                // presence must flip both `check_sendable` and `encode` to a clean error.
                let env = ascript::interp::global_env();
                env.get("print").unwrap_or(Value::Nil)
            }
        }
        _ => Value::Nil,
    }
}

/// An i64 biased toward the arithmetic boundaries (the NUM divergence farm) but also random.
fn int_edge(u: &mut Unstructured) -> i64 {
    let edges = [
        0i64,
        1,
        -1,
        i64::MAX,
        i64::MIN,
        (1i64 << 53),
        (1i64 << 53) + 1,
        -(1i64 << 53),
    ];
    if bool::arbitrary(u).unwrap_or(false) {
        let i = (u8::arbitrary(u).unwrap_or(0) as usize) % edges.len();
        edges[i]
    } else {
        i64::arbitrary(u).unwrap_or(0)
    }
}

/// A FINITE f64 (no NaN/inf): NaN != NaN would break the structural round-trip equality (NaN
/// is a documented Map-key carve-out, not a round-trip failure). The exact-bits-preserving
/// finite path is what the round-trip pins.
fn f64_finite(u: &mut Unstructured) -> f64 {
    let f = f64::arbitrary(u).unwrap_or(0.0);
    if f.is_finite() {
        f
    } else {
        0.0
    }
}

/// A short lowercase string (small key/value space → more sharing + collisions to stress).
fn short_str(u: &mut Unstructured) -> String {
    let n = (u8::arbitrary(u).unwrap_or(0) % 5) as usize;
    let mut s = String::with_capacity(n);
    for _ in 0..n {
        let c = (b'a' + (u8::arbitrary(u).unwrap_or(0) % 26)) as char;
        s.push(c);
    }
    s
}

/// A scalar usable as a Map/Set key (bool / int-edge / short string — no nil/collection keys).
fn scalar_key(u: &mut Unstructured) -> Value {
    match u8::arbitrary(u).unwrap_or(0) % 3 {
        0 => Value::Bool(bool::arbitrary(u).unwrap_or(false)),
        1 => Value::Int(int_edge(u)),
        _ => Value::Str(short_str(u).into()),
    }
}
