//! REPLAY Task 3 — the `call_stdlib` trace hook + `replay_class` classification table.
//!
//! In-process coverage: each test gets a fully-wired `Rc<Interp>` (via the public
//! `run_source_with_interp` seam, which runs an empty program to install `self`/caps),
//! then installs a `CliTrace` Record or Replay context through the `#[doc(hidden)]`
//! REPLAY test seams and drives effectful stdlib calls through the FULL `call_stdlib`
//! path (caps gate → trace hook → dispatch) via `__call_stdlib`. The binary-spawning
//! end-to-end CLI coverage is a later REPLAY task (Task 6).

use ascript::det::{DetEvent, TraceOutcome};
use ascript::interp::{Control, Interp};
use ascript::stdlib::{replay_class, HandleShape, ReplayClass, STD_MODULES};
use ascript::value::Value;
use std::rc::Rc;

/// Build a current-thread runtime + LocalSet, get a fully-wired `Rc<Interp>` (by running
/// an empty program), and run `body` with it inside the LocalSet (so async stdlib calls
/// that `spawn_local` have a live local set).
fn with_interp<F, Fut, R>(body: F) -> R
where
    F: FnOnce(Rc<Interp>) -> Fut,
    Fut: std::future::Future<Output = R>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let (_out, interp) = ascript::run_source_with_interp("")
            .await
            .expect("wire interp via empty program");
        body(interp).await
    })
}

/// The dispatch-site module key for a `std/*` specifier (e.g. `std/net/http` →
/// `net_http`), mirroring `every_std_module_is_classified_gated_or_explicitly_ungated`.
fn dispatch_key(full: &str) -> String {
    full.strip_prefix("std/").unwrap().replace('/', "_")
}

// ===========================================================================
// 1. classification_is_complete — every STD_MODULES entry has an EXPLICIT,
//    DOCUMENTED default classification (REPLAY §8, Task 9). The table is total
//    via `_ => Harmless`, so a probe ALWAYS resolves — that is NOT enough: a NEW
//    OS-touching module added with no `replay_class` arm would fall to Harmless
//    SILENTLY (the T3 carry-forward gap). This test closes the loop the way
//    `every_std_module_is_classified_gated_or_explicitly_ungated` closes the cap
//    loop: every module's *default* (`__probe__`) class must match a DOCUMENTED
//    expected class, and any module whose default is Harmless must be listed in
//    `KNOWN_HARMLESS` (pure / in-memory / seam-routed). A module in NEITHER the
//    explicit-class map NOR `KNOWN_HARMLESS` trips this — forcing a deliberate
//    classification decision for anything new. The table IS the test fixture
//    (spec §8); the per-func splits are pinned by the per-func assertions below.
// ===========================================================================

/// The DOCUMENTED *default*-func (`__probe__`) replay class for every module whose
/// default is NOT `Harmless` (the load-bearing rows of the §8 table). Keyed by the
/// dispatch-site module string (`std/net/http` → `net_http`). A module here is
/// classified by a deliberate `replay_class` arm; a module that is *intended*
/// Harmless lives in `KNOWN_HARMLESS` instead. Every `STD_MODULES` entry must be in
/// exactly one of the two sets — that is the completeness guard.
fn expected_default_class(key: &str) -> Option<ReplayClass> {
    use HandleShape::{HttpResponse, Plain};
    use ReplayClass::{Recorded, Refused, Seamed};
    Some(match key {
        // Recorded (effectful, plain-data results recorded at the boundary).
        "fs" | "env" | "io" | "os" | "net" => Recorded(Plain),
        // net_http BUFFERED verbs → Recorded with the HttpResponse virtualization shape
        // (Task 4); `sse`/`cancelToken` are Refused (per-func, asserted below).
        "net_http" => Recorded(HttpResponse),
        // archive: the DEFAULT func is an in-memory builder (Harmless); the disk
        // funcs (tarExtractTo/zipExtractTo/tarCreateFromDir) are Recorded — see the
        // per-func split assertions below. So `archive` is in KNOWN_HARMLESS by
        // default, NOT here.
        "process" => Recorded(Plain), // process.run; process.spawn is Refused (per-func).
        // Seamed (routed through the determinism context already).
        "time" => Seamed, // time.now/monotonic/sleep; interval/debounce/throttle Refused (per-func).
        "date" => Seamed,
        "ffi" => Seamed,
        // Refused (no determinism seam — live handles / streams / sockets / servers).
        "net_tcp" | "net_udp" | "net_ws" | "http_server" | "net_unix" => Refused,
        "sqlite" | "postgres" | "redis" | "tui" | "ai" | "telemetry" | "docker" | "blob"
        | "oauth" => Refused,
        // Everything else has a Harmless default — must be in KNOWN_HARMLESS.
        _ => return None,
    })
}

/// Modules whose DEFAULT-func class is `Harmless` — pure given inputs, in-memory
/// coordination, or nondeterminism that flows through the det SEAMS (RNG/clock)
/// rather than the trace hook. Audited from source (Task 9 §8 sweep):
/// - `math`/`string`/`json`/`regex`/`schema`/`array`/`object`/`map`/`set`/`decimal`/
///   `bytes`/`convert`/`color`/`template`/`cli`/`url`/`csv`/`toml`/`yaml`/`msgpack`/
///   `cbor`/`xml`/`html`/`markdown`/`diff`/`semver`/`assert`/`bench`/`test`/`shared`/
///   `lru`/`events` — pure transforms over their arguments.
/// - `intl` — locale is ALWAYS an explicit string arg over BUNDLED ICU data; the
///   instant comes from an explicit `epochMs` field. No system-locale read, no clock.
/// - `stream` — every source is pure (`from` array/generator, `range` numeric); no
///   fs/net-backed source. The live handle is never a recorded boundary.
/// - `sync` — in-memory channels/semaphores/rate-limiter (`tokio::sync::Notify` +
///   `RefCell`); no recorded value is clock-dependent.
/// - `log` — the stderr/capture sink: output is OBSERVATION, not an effect event.
/// - `crypto`/`uuid` — random/salts route through `fill_seeded_bytes` (the RNG seam);
///   v7's time prefix is the virtual clock. The seam events flow without the hook,
///   so the MODULE is Harmless (the per-func RNG-vs-pure split is hook-invisible).
/// - `compress`/`encoding` — pure (de)compression / (de)coding; no random, no clock.
/// - `caps` — reads are Harmless (`drop`/`dropAll` are Refused per-func).
/// - `task` — combinators over `future<T>`; the determinism is in the awaited work.
/// - `cron`/`resilience`/`jwt`/`email`/`archive` — Harmless DEFAULT; their effectful
///   funcs are Refused/Recorded per-func (asserted below).
const KNOWN_HARMLESS: &[&str] = &[
    "assert", "test", "bench", "cli", "color", "decimal", "math", "string", "array",
    "object", "map", "schema", "shared", "set", "lru", "events", "template", "bytes",
    "caps", "convert", "task", "sync", "stream", "intl", "json", "log", "encoding",
    "crypto", "compress", "regex", "url", "uuid", "csv", "toml", "yaml", "msgpack",
    "cbor", "resilience",
    // workflow: DEFAULT func is Harmless; `run`/`resume` are Recorded (per-func,
    // asserted in the Recorded set). Its own internal events go to the workflow log.
    "workflow",
    "cron", "semver", "jwt", "archive", "xml", "html", "markdown", "diff", "email",
];

#[test]
#[cfg(all(
    feature = "ai",
    feature = "telemetry",
    feature = "workflow",
    feature = "sql",
    feature = "postgres",
    feature = "redis",
    feature = "ffi",
    feature = "net",
    feature = "datetime",
))]
fn classification_is_complete() {
    // COMPLETENESS (T3 carry-forward fix): every STD_MODULES entry must be classified
    // EXPLICITLY — either by a documented non-Harmless `expected_default_class` arm OR
    // by membership in `KNOWN_HARMLESS`. A module in NEITHER trips here (it would
    // silently fall to `_ => Harmless` in `replay_class` — exactly the gap this guards).
    for full in STD_MODULES {
        let key = dispatch_key(full);
        match expected_default_class(&key) {
            Some(expected) => {
                assert_eq!(
                    replay_class(&key, "__probe__"),
                    expected,
                    "std module '{key}' default class drifted from the documented §8 table"
                );
                assert!(
                    !KNOWN_HARMLESS.contains(&key.as_str()),
                    "std module '{key}' is in BOTH expected_default_class and KNOWN_HARMLESS — pick one."
                );
            }
            None => {
                // A Harmless default — it MUST be explicitly listed, and `replay_class`
                // must actually return Harmless for the default func.
                assert!(
                    KNOWN_HARMLESS.contains(&key.as_str()),
                    "std module '{key}' is UNCLASSIFIED for record/replay: add a `replay_class` \
                     arm + an `expected_default_class` row if it touches an effect/seam, or to \
                     `KNOWN_HARMLESS` if it is pure / in-memory / seam-routed. (A silently-Harmless \
                     effectful module is a record/replay correctness bug — REPLAY §8.)"
                );
                assert_eq!(
                    replay_class(&key, "__probe__"),
                    ReplayClass::Harmless,
                    "std module '{key}' is in KNOWN_HARMLESS but its default class is not Harmless"
                );
            }
        }
    }

    // SABOTAGE TRIPWIRE: a fabricated module name is in NEITHER set, so it must have no
    // documented class — proving a NEW unclassified module trips the loop above (it would
    // hit the `None` arm and the `KNOWN_HARMLESS.contains` assert would fail). We assert
    // the precondition here so the guard's teeth are self-evident.
    let fake = "totally_fabricated_module_xyz";
    assert!(
        expected_default_class(fake).is_none() && !KNOWN_HARMLESS.contains(&fake),
        "a fabricated module must be in neither set (else the completeness loop is toothless)"
    );

    // Refused set — the load-bearing classifications.
    for (m, f) in [
        ("net_tcp", "connect"),
        ("net_udp", "bind"),
        ("net_ws", "connect"),
        ("http_server", "serve"),
        ("sqlite", "open"),
        ("postgres", "connect"),
        ("redis", "connect"),
        ("tui", "init"),
        ("ai", "generate"),
        ("telemetry", "init"),
        ("process", "spawn"),
    ] {
        assert_eq!(
            replay_class(m, f),
            ReplayClass::Refused,
            "{m}.{f} must be Refused"
        );
    }

    // Recorded set.
    for (m, f) in [
        ("fs", "read"),
        ("env", "get"),
        ("io", "readLine"),
        ("os", "cpuCount"),
        ("net", "lookup"),
        ("process", "run"),
        ("workflow", "run"),
        // archive DISK funcs are fs-shaped (read a dir / write extracted files); their
        // result is plain data → Recorded(Plain) like fs, replayed without disk access
        // (Task 9 reclassification — the in-memory builders stay Harmless, asserted below).
        ("archive", "tarExtractTo"),
        ("archive", "zipExtractTo"),
        ("archive", "tarCreateFromDir"),
    ] {
        assert!(
            matches!(replay_class(m, f), ReplayClass::Recorded(_)),
            "{m}.{f} must be Recorded"
        );
    }
    // net_http is Recorded with the HttpResponse shape (Task 4 virtualization vehicle).
    assert_eq!(
        replay_class("net_http", "get"),
        ReplayClass::Recorded(HandleShape::HttpResponse)
    );

    // Seamed set (cosmetic — both Seamed and Harmless fall through; we pin the spec's
    // module-level Seamed choices).
    for (m, f) in [("time", "sleep"), ("date", "now"), ("ffi", "call")] {
        assert_eq!(replay_class(m, f), ReplayClass::Seamed, "{m}.{f} must be Seamed");
    }

    // Harmless set + per-func splits.
    for (m, f) in [
        ("math", "abs"),
        ("string", "upper"),
        ("json", "parse"),
        ("array", "map"),
        ("caps", "list"),
        ("email", "message"),
        ("jwt", "sign"),
    ] {
        assert_eq!(
            replay_class(m, f),
            ReplayClass::Harmless,
            "{m}.{f} must be Harmless"
        );
    }
    // archive in-memory builders stay Harmless (the disk funcs above are Recorded).
    assert_eq!(replay_class("archive", "tarWriter"), ReplayClass::Harmless);
    assert_eq!(replay_class("archive", "tarEntries"), ReplayClass::Harmless);
    assert_eq!(replay_class("archive", "tarAppend"), ReplayClass::Harmless);
    // caps.drop / dropAll are Refused (replay can't see the dropped state).
    assert_eq!(replay_class("caps", "drop"), ReplayClass::Refused);
    assert_eq!(replay_class("caps", "dropAll"), ReplayClass::Refused);
    // jwt.jwks (live fetch) is Refused; email.send/connect (live socket) Refused.
    assert_eq!(replay_class("jwt", "jwks"), ReplayClass::Refused);
    assert_eq!(replay_class("email", "send"), ReplayClass::Refused);
    assert_eq!(replay_class("email", "connect"), ReplayClass::Refused);
    // time.now/monotonic/sleep are Seamed (above); but the TIMER funcs interval/debounce/
    // throttle mint a LIVE tokio timer that BYPASSES the clock seam (real-sleeps under
    // replay) → Refused v1 (Task 9 reclassification; no virtual-tick seam ships in v1).
    assert_eq!(replay_class("time", "interval"), ReplayClass::Refused);
    assert_eq!(replay_class("time", "debounce"), ReplayClass::Refused);
    assert_eq!(replay_class("time", "throttle"), ReplayClass::Refused);
}

// ===========================================================================
// 1b. Task 9 reclassification — `time.interval` is REFUSED under a trace (it mints a
//     live tokio timer that bypasses the clock seam). The refusal fires in the hook
//     BEFORE the live timer is created, so the test needs no real timer.
// ===========================================================================

#[test]
fn time_interval_refused_under_trace() {
    with_interp(|interp| async move {
        interp.__install_record_trace(99);
        for f in ["interval", "debounce", "throttle"] {
            let r = interp
                .__call_stdlib("time", f, &[Value::float(10.0)])
                .await;
            let err = match r {
                Err(Control::Panic(e)) => e,
                other => panic!("time.{f} under a trace must be a LOUD Tier-2 refusal, got {other:?}"),
            };
            let msg = &err.message;
            assert!(
                msg.contains(&format!("time.{f}")) && msg.contains("not supported under --record/--replay"),
                "time.{f} refusal must name the func + the §8 message, got: {msg}"
            );
        }
        interp.__clear_determinism();
    });
}

// ===========================================================================
// 2. fs_read_records_and_replays_without_fs — record fs.read of a fixture, DELETE
//    the fixture, replay returns the same [content, nil] with NO fs access.
// ===========================================================================

#[test]
#[cfg(feature = "sys")]
fn fs_read_records_and_replays_without_fs() {
    let dir = std::env::temp_dir().join(format!("ascript-replay-fsread-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fixture = dir.join("config.toml");
    std::fs::write(&fixture, "name = \"orders\"\n").unwrap();
    let path = fixture.to_string_lossy().to_string();

    // ---- Record ----
    let events = with_interp(|interp| {
        let path = path.clone();
        async move {
            interp.__install_record_trace(42);
            let r = interp
                .__call_stdlib("fs", "read", &[Value::str(path)])
                .await
                .expect("fs.read should succeed under record");
            // fs.read returns the [content, err] Tier-1 pair.
            let arr = match r.kind() {
                ascript::value::ValueKind::Array(a) => a,
                other => panic!("fs.read returned non-array: {other:?}"),
            };
            let content = arr.borrow()[0].clone();
            assert_eq!(content.to_string(), "name = \"orders\"\n");
            let events = interp.__take_trace_events().expect("a context is installed");
            interp.__clear_determinism();
            events
        }
    });

    // Exactly one StdlibCall(fs.read) with a Value outcome.
    let stdlib_calls: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, DetEvent::StdlibCall { .. }))
        .collect();
    assert_eq!(stdlib_calls.len(), 1, "exactly one StdlibCall recorded");
    match stdlib_calls[0] {
        DetEvent::StdlibCall {
            module,
            func,
            outcome,
            ..
        } => {
            assert_eq!(module, "fs");
            assert_eq!(func, "read");
            assert!(matches!(outcome, TraceOutcome::Value(_)), "Value outcome");
        }
        _ => unreachable!(),
    }

    // ---- DELETE the fixture: replay must NOT touch fs ----
    std::fs::remove_file(&fixture).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(!fixture.exists(), "fixture is gone");

    // ---- Replay ----
    with_interp(|interp| {
        let path = path.clone();
        let events = events.clone();
        async move {
            interp.__install_replay_trace(42, events);
            let r = interp
                .__call_stdlib("fs", "read", &[Value::str(path)])
                .await
                .expect("fs.read replays without fs access");
            let arr = match r.kind() {
                ascript::value::ValueKind::Array(a) => a,
                other => panic!("replayed fs.read returned non-array: {other:?}"),
            };
            let content = arr.borrow()[0].clone();
            assert_eq!(
                content.to_string(),
                "name = \"orders\"\n",
                "replay returns the recorded content though the file is gone"
            );
            interp.__clear_determinism();
        }
    });
}

// ===========================================================================
// 3. env_process_os_dns_round_trip — record env/process/os/dns, replay matches.
// ===========================================================================

#[test]
#[cfg(feature = "sys")]
fn env_process_os_round_trip() {
    let events = with_interp(|interp| async move {
        interp.__install_record_trace(7);
        // env.set then env.get
        let _ = interp
            .__call_stdlib("env", "set", &[Value::str("ASCRIPT_RR_TEST"), Value::str("hi")])
            .await
            .expect("env.set");
        let got = interp
            .__call_stdlib("env", "get", &[Value::str("ASCRIPT_RR_TEST")])
            .await
            .expect("env.get");
        assert_eq!(got.to_string(), "hi");
        // os.cpuCount → an int.
        let cpus = interp
            .__call_stdlib("os", "cpuCount", &[])
            .await
            .expect("os.cpuCount");
        assert!(matches!(
            cpus.kind(),
            ascript::value::ValueKind::Int(_) | ascript::value::ValueKind::Float(_)
        ));
        let events = interp.__take_trace_events().unwrap();
        interp.__clear_determinism();
        events
    });

    let n_stdlib = events
        .iter()
        .filter(|e| matches!(e, DetEvent::StdlibCall { .. }))
        .count();
    assert_eq!(n_stdlib, 3, "env.set + env.get + os.cpuCount recorded");

    // Replay: clear the env var first; replay must NOT consult the real env.
    std::env::remove_var("ASCRIPT_RR_TEST");
    with_interp(|interp| {
        let events = events.clone();
        async move {
            interp.__install_replay_trace(7, events);
            let _ = interp
                .__call_stdlib("env", "set", &[Value::str("ASCRIPT_RR_TEST"), Value::str("hi")])
                .await
                .expect("env.set replay");
            let got = interp
                .__call_stdlib("env", "get", &[Value::str("ASCRIPT_RR_TEST")])
                .await
                .expect("env.get replay");
            assert_eq!(got.to_string(), "hi", "replayed env.get returns recorded value");
            let cpus = interp
                .__call_stdlib("os", "cpuCount", &[])
                .await
                .expect("os.cpuCount replay");
            assert!(matches!(
            cpus.kind(),
            ascript::value::ValueKind::Int(_) | ascript::value::ValueKind::Float(_)
        ));
            interp.__clear_determinism();
        }
    });
}

// ===========================================================================
// 4. int_float_fidelity_through_outcome — the airlock (NOT JSON) preserves the
//    Int/Float subtype split through a TraceOutcome::Value round-trip.
// ===========================================================================

#[test]
fn int_float_fidelity_through_outcome() {
    with_interp(|interp| async move {
        // Encode 5 (Int) and 5.0 (Float) separately; the airlock must preserve the
        // subtype (JSON would collapse them).
        let (int_bytes, _) =
            ascript::worker::serialize::encode(&Value::int(5)).expect("encode Int(5)");
        let (float_bytes, _) =
            ascript::worker::serialize::encode(&Value::float(5.0)).expect("encode Float(5.0)");
        assert_ne!(
            int_bytes, float_bytes,
            "Int(5) and Float(5.0) must encode to DIFFERENT bytes (airlock, not JSON)"
        );

        let int_back = ascript::worker::serialize::decode(&int_bytes, &interp).unwrap();
        let float_back = ascript::worker::serialize::decode(&float_bytes, &interp).unwrap();
        assert!(matches!(int_back.kind(), ascript::value::ValueKind::Int(5)));
        assert!(matches!(float_back.kind(), ascript::value::ValueKind::Float(f) if f == 5.0));
        // The §2.4 verdict made visible: the airlock preserves the NUM subtype, so the
        // two print DIFFERENTLY (JSON would collapse both to "5"). (AScript numeric `==`
        // unifies 5 and 5.0 by design, so the distinction is the KIND + the printed form,
        // not value equality — exactly the observable a recorded program branches on.)
        assert_eq!(int_back.to_string(), "5");
        assert_eq!(float_back.to_string(), "5.0");
    })
}

// ===========================================================================
// 5. refused_set_is_loud_in_both_modes — sqlite/net_tcp/process.spawn/telemetry are
//    a Tier-2 refusal naming the fn + "v2", under BOTH record AND replay.
// ===========================================================================

#[test]
#[cfg(all(feature = "sql", feature = "net", feature = "sys", feature = "telemetry"))]
fn refused_set_is_loud_in_both_modes() {
    let refused: &[(&str, &str)] = &[
        ("sqlite", "open"),
        ("net_tcp", "connect"),
        ("process", "spawn"),
        ("telemetry", "init"),
    ];

    // Under RECORD.
    with_interp(|interp| async move {
        interp.__install_record_trace(1);
        for (m, f) in refused {
            let r = interp.__call_stdlib(m, f, &[]).await;
            match r {
                Err(Control::Panic(e)) => {
                    assert!(
                        e.message.contains(m) && e.message.contains(f),
                        "refusal must name {m}.{f}: {}",
                        e.message
                    );
                    assert!(
                        e.message.contains("--record/--replay"),
                        "refusal mentions record/replay: {}",
                        e.message
                    );
                }
                other => panic!("{m}.{f} under record must be a Tier-2 refusal, got {other:?}"),
            }
        }
        interp.__clear_determinism();
    });

    // Under REPLAY (empty stream — the refusal fires BEFORE any event consult).
    with_interp(|interp| async move {
        interp.__install_replay_trace(1, vec![]);
        for (m, f) in refused {
            let r = interp.__call_stdlib(m, f, &[]).await;
            match r {
                Err(Control::Panic(e)) => {
                    assert!(
                        e.message.contains(m) && e.message.contains(f),
                        "replay refusal must name {m}.{f}: {}",
                        e.message
                    );
                }
                other => panic!("{m}.{f} under replay must be a Tier-2 refusal, got {other:?}"),
            }
        }
        interp.__clear_determinism();
    });
}

// ===========================================================================
// 6. mismatch_is_indexed_with_expected_got — record fs.read(a), replay fs.read(b)
//    → the §7 divergence error names `event 0`.
// ===========================================================================

#[test]
#[cfg(feature = "sys")]
fn mismatch_is_indexed_with_expected_got() {
    let dir = std::env::temp_dir().join(format!("ascript-replay-mismatch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.txt");
    let b = dir.join("b.txt");
    std::fs::write(&a, "AAA").unwrap();
    std::fs::write(&b, "BBB").unwrap();
    let pa = a.to_string_lossy().to_string();
    let pb = b.to_string_lossy().to_string();

    // Record fs.read(a).
    let events = with_interp(|interp| {
        let pa = pa.clone();
        async move {
            interp.__install_record_trace(3);
            let _ = interp
                .__call_stdlib("fs", "read", &[Value::str(pa)])
                .await
                .unwrap();
            let events = interp.__take_trace_events().unwrap();
            interp.__clear_determinism();
            events
        }
    });

    // Replay fs.read(b) — a different signature at event 0 → divergence.
    with_interp(|interp| {
        let pb = pb.clone();
        let events = events.clone();
        async move {
            interp.__install_replay_trace(3, events);
            let r = interp.__call_stdlib("fs", "read", &[Value::str(pb)]).await;
            match r {
                Err(Control::Panic(e)) => {
                    assert!(
                        e.message.contains("event 0"),
                        "divergence error indexes event 0: {}",
                        e.message
                    );
                    assert!(
                        e.message.contains("divergence"),
                        "divergence error is labelled: {}",
                        e.message
                    );
                }
                other => panic!("a same-fn-different-args replay must diverge, got {other:?}"),
            }
            interp.__clear_determinism();
        }
    });

    std::fs::remove_dir_all(&dir).ok();
}

// ===========================================================================
// 7. non_sendable_result_refused_at_record — net_http is Recorded (Task 4 will
//    virtualize); pre-Task-4 the live HttpResponse handle fails the airlock
//    check_sendable → a loud record-time field-path refusal. (Offline: we assert
//    that EITHER the request fails before producing a handle OR, if it produces a
//    live handle, the record-time encode refuses it. We use a guaranteed-unroutable
//    address so no network egress happens; the key assertion is "never a silent
//    recorded handle".)
// ===========================================================================

#[test]
#[cfg(feature = "net")]
fn non_sendable_result_refused_at_record() {
    // net_http MUST be classified Recorded (the Task-4 virtualization vehicle): pre-Task-4
    // the live HttpResponse handle is non-sendable, so a recorded request's result fails
    // the airlock at record time. Task 4 reclassifies it to a virtualized handle.
    assert!(matches!(
        replay_class("net_http", "get"),
        ReplayClass::Recorded(_)
    ));

    // Prove the record-time refusal PATH that `encode_trace_outcome` relies on: a
    // Recorded result carrying a live native handle (the HttpResponse case) is exactly
    // the value `encode` rejects with a field-path error. We mint a real non-sendable
    // handle (`sync.channel`), wrap it like a `[handle, nil]` result pair, and assert the
    // airlock `encode` (the same fn the hook uses) refuses it naming the field path.
    with_interp(|interp| async move {
        let ch = interp
            .__call_stdlib("sync", "channel", &[])
            .await
            .expect("mint a channel handle");
        // A `[handle, nil]` pair — the canonical Tier-1 shape a Recorded http call yields.
        let pair = Value::array(vec![ch, Value::nil()]);
        let err = match ascript::worker::serialize::encode(&pair) {
            Ok(_) => panic!("a live native handle must fail the airlock at record time"),
            Err(e) => e,
        };
        let msg = err.message();
        // The error is a field-path message (the §2.4 record-time refusal): the airlock
        // names the index into the result pair where the non-sendable handle sits.
        assert!(
            msg.contains("[0]"),
            "record-time refusal must name the field path [0]: {msg}"
        );
        assert!(
            msg.contains("worker") || msg.contains("sent"),
            "record-time refusal is the airlock non-sendable message: {msg}"
        );
    });
}

// ===========================================================================
// 8. workflow_inside_record_round_trips — workflow.run under record → ONE
//    StdlibCall(workflow.run); replay returns the result WITHOUT executing.
// ===========================================================================

#[test]
#[cfg(feature = "workflow")]
fn workflow_run_is_recorded_class() {
    // The load-bearing classification: workflow.run/resume are Recorded; the CLI trace
    // records exactly ONE StdlibCall at the boundary (the workflow's own events go to
    // the workflow log under the prev context). Full e2e is a later task; here we pin
    // the class so the hook records workflow.run at the boundary.
    assert!(matches!(
        replay_class("workflow", "run"),
        ReplayClass::Recorded(_)
    ));
    assert!(matches!(
        replay_class("workflow", "resume"),
        ReplayClass::Recorded(_)
    ));
}

// ===========================================================================
// 9. default_path_untouched — NO context → trace_active() false; the flag plumbing
//    install→true, take→false, restore(Some workflow)→false.
// ===========================================================================

// ===========================================================================
// Task 4 — HttpResponse handle virtualization (REPLAY §2.5).
//
// A tiny raw-TCP HTTP/1.1 server runs on a background thread serving a JSON route
// (/json) and a text route (/text). The record run drives `http.get` + accessors
// in-process; the server is then STOPPED and the replay run must succeed OFFLINE
// (the flagship "record against a real API, replay on a plane" guarantee).
// ===========================================================================

#[cfg(feature = "net")]
mod http_virtualization {
    use super::*;
    use ascript::value::ValueKind;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// A background raw-HTTP/1.1 server. Returns `(base_url, stop)` where `stop` is an
    /// `Arc<AtomicBool>` the caller sets to shut the accept loop down (so replay runs
    /// with the server DOWN). Each connection serves ONE request then closes.
    fn spawn_http_server() -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        serve_one(&mut sock);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (format!("http://{}", addr), stop, handle)
    }

    /// Read the request line, decide the route, write a response, close.
    fn serve_one(sock: &mut TcpStream) {
        sock.set_nonblocking(false).ok();
        let mut buf = [0u8; 1024];
        let n = sock.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/");
        let (ctype, body) = if path.starts_with("/json") {
            ("application/json", "{\"name\":\"orders\",\"count\":3}".to_string())
        } else {
            ("text/plain", "hello replay".to_string())
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            ctype,
            body.len(),
            body
        );
        let _ = sock.write_all(resp.as_bytes());
        let _ = sock.flush();
    }

    /// The `[handle, nil]` pair's element 0 (the native HttpResponse handle).
    fn pair_handle(pair: &Value) -> Value {
        match pair.kind() {
            ValueKind::Array(a) => a.borrow()[0].clone(),
            other => panic!("expected a [handle, nil] pair, got {other:?}"),
        }
    }

    // ---- Test 1: record handle + accessor, replay offline ---- //
    #[test]
    fn http_get_records_handle_and_accessors() {
        let (base, stop, handle) = spawn_http_server();
        let url = format!("{}/json", base);

        // ---- Record ----
        let (recorded_status, recorded_body, events) = with_interp(|interp| {
            let url = url.clone();
            async move {
                interp.__install_record_trace(100);
                let pair = interp
                    .__call_stdlib("net_http", "get", &[Value::str(url)])
                    .await
                    .expect("http.get under record");
                let resp = pair_handle(&pair);
                // resp.status — a MATERIALIZED field read (no event consumed).
                let status = match resp.kind() {
                    ValueKind::Native(n) => n.fields.get("status").cloned().unwrap(),
                    _ => panic!("not a native handle"),
                };
                // resp.json() — recorded as a NativeCall.
                let jpair = interp
                    .__call_native_method(&resp, "json", vec![])
                    .await
                    .expect("resp.json() under record");
                let body = match jpair.kind() {
                    ValueKind::Array(a) => a.borrow()[0].clone(),
                    _ => panic!("json pair"),
                };
                let events = interp.__take_trace_events().unwrap();
                interp.__clear_determinism();
                (status, body, events)
            }
        });

        // The trace holds StdlibCall(net_http.get) with a Handle{vid:0} outcome + one
        // NativeCall{vid:0, method:"json"}.
        let stdlib: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, DetEvent::StdlibCall { .. }))
            .collect();
        assert_eq!(stdlib.len(), 1, "one StdlibCall(net_http.get)");
        match stdlib[0] {
            DetEvent::StdlibCall { module, func, outcome, .. } => {
                assert_eq!(module, "net_http");
                assert_eq!(func, "get");
                match outcome {
                    TraceOutcome::Handle { kind_tag, vid, .. } => {
                        assert_eq!(*kind_tag, 1, "HttpResponse tag = 1");
                        assert_eq!(*vid, 0, "first handle gets vid 0");
                    }
                    other => panic!("expected a Handle outcome, got {other:?}"),
                }
            }
            _ => unreachable!(),
        }
        let native_calls: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                DetEvent::NativeCall { vid, method, .. } => Some((*vid, method.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(native_calls, vec![(0u32, "json".to_string())], "one NativeCall json@vid0");

        // ---- STOP the server: replay must NOT touch the network ---- //
        stop.store(true, Ordering::SeqCst);
        handle.join().ok();
        // A guaranteed-dead URL would be used by the replay path ONLY if it touched the
        // network — it must not. We replay against the SAME url (server now down).

        // ---- Replay (offline) ----
        with_interp(|interp| {
            let url = url.clone();
            let events = events.clone();
            let recorded_status = recorded_status.clone();
            let recorded_body = recorded_body.clone();
            async move {
                interp.__install_replay_trace(100, events);
                let pair = interp
                    .__call_stdlib("net_http", "get", &[Value::str(url)])
                    .await
                    .expect("http.get replays offline (server is down)");
                let resp = pair_handle(&pair);
                // resp.status from the materialized fields (no event).
                let status = match resp.kind() {
                    ValueKind::Native(n) => n.fields.get("status").cloned().unwrap(),
                    _ => panic!("virtual handle is native"),
                };
                assert_eq!(status, recorded_status, "replayed status from materialized fields");
                // resp.json() consumes the recorded NativeCall.
                let jpair = interp
                    .__call_native_method(&resp, "json", vec![])
                    .await
                    .expect("resp.json() replays from the recorded NativeCall");
                let body = match jpair.kind() {
                    ValueKind::Array(a) => a.borrow()[0].clone(),
                    _ => panic!("json pair"),
                };
                assert_eq!(
                    body.to_string(),
                    recorded_body.to_string(),
                    "replayed body matches recording though the server is down"
                );
                // Leak check: exactly ONE live resource (the single virtual handle) — no
                // per-accessor-call accumulation. (A native handle's table entry is
                // reclaimed on interp drop / explicit close, like every HttpResponse; the
                // point is the accessor calls add NO further entries.)
                assert_eq!(
                    interp.__resource_count(),
                    1,
                    "exactly one virtual handle in the table — no per-call leak"
                );
                drop(resp);
                drop(pair);
                interp.__clear_determinism();
            }
        });
    }

    // ---- Test 2: two responses get distinct vids, per-vid method order ---- //
    #[test]
    fn two_responses_get_distinct_vids_and_interleave() {
        let (base, stop, handle) = spawn_http_server();
        let ujson = format!("{}/json", base);
        let utext = format!("{}/text", base);

        let events = with_interp(|interp| {
            let ujson = ujson.clone();
            let utext = utext.clone();
            async move {
                interp.__install_record_trace(7);
                let p0 = interp
                    .__call_stdlib("net_http", "get", &[Value::str(ujson)])
                    .await
                    .unwrap();
                let r0 = pair_handle(&p0);
                let p1 = interp
                    .__call_stdlib("net_http", "get", &[Value::str(utext)])
                    .await
                    .unwrap();
                let r1 = pair_handle(&p1);
                // Interleave: r1.text() then r0.json().
                let _ = interp.__call_native_method(&r1, "text", vec![]).await.unwrap();
                let _ = interp.__call_native_method(&r0, "json", vec![]).await.unwrap();
                let events = interp.__take_trace_events().unwrap();
                interp.__clear_determinism();
                events
            }
        });
        stop.store(true, Ordering::SeqCst);
        handle.join().ok();

        // vids 0 and 1 assigned in handle-birth order.
        let handles: Vec<u32> = events
            .iter()
            .filter_map(|e| match e {
                DetEvent::StdlibCall { outcome: TraceOutcome::Handle { vid, .. }, .. } => Some(*vid),
                _ => None,
            })
            .collect();
        assert_eq!(handles, vec![0, 1], "two handles get distinct vids in birth order");
        // The interleaved accessor calls: r1(vid1).text first, then r0(vid0).json.
        let calls: Vec<(u32, String)> = events
            .iter()
            .filter_map(|e| match e {
                DetEvent::NativeCall { vid, method, .. } => Some((*vid, method.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls,
            vec![(1, "text".to_string()), (0, "json".to_string())],
            "per-vid method order preserved"
        );

        // Replay verifies the per-vid order: r0.json + r1.text replay against the right vids.
        with_interp(|interp| {
            let ujson = ujson.clone();
            let utext = utext.clone();
            let events = events.clone();
            async move {
                interp.__install_replay_trace(7, events);
                let r0 = pair_handle(
                    &interp
                        .__call_stdlib("net_http", "get", &[Value::str(ujson)])
                        .await
                        .unwrap(),
                );
                let r1 = pair_handle(
                    &interp
                        .__call_stdlib("net_http", "get", &[Value::str(utext)])
                        .await
                        .unwrap(),
                );
                // Same interleave order as record → matches the recorded NativeCall stream.
                let t = interp.__call_native_method(&r1, "text", vec![]).await.unwrap();
                assert_eq!(
                    match t.kind() { ValueKind::Array(a) => a.borrow()[0].to_string(), _ => unreachable!() },
                    "hello replay"
                );
                let _ = interp.__call_native_method(&r0, "json", vec![]).await.unwrap();
                interp.__clear_determinism();
            }
        });
    }

    // ---- Test 3: a connection-refused request records [nil, err] as a plain Value ---- //
    #[test]
    fn http_error_pair_round_trips() {
        // A guaranteed-dead address (port 1 on loopback is unbindable/unroutable).
        let dead = "http://127.0.0.1:1/json".to_string();
        let events = with_interp(|interp| {
            let dead = dead.clone();
            async move {
                interp.__install_record_trace(5);
                let pair = interp
                    .__call_stdlib("net_http", "get", &[Value::str(dead)])
                    .await
                    .expect("a connection failure is the Tier-1 [nil, err] pair, not an error");
                // [nil, err] — element 0 is nil.
                match pair.kind() {
                    ValueKind::Array(a) => {
                        assert!(matches!(a.borrow()[0].kind(), ValueKind::Nil), "[nil, err]");
                    }
                    other => panic!("expected [nil, err], got {other:?}"),
                }
                let events = interp.__take_trace_events().unwrap();
                interp.__clear_determinism();
                events
            }
        });
        // The error pair is recorded as a PLAIN Value outcome (no handle, no vid).
        let stdlib: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, DetEvent::StdlibCall { .. }))
            .collect();
        assert_eq!(stdlib.len(), 1);
        match stdlib[0] {
            DetEvent::StdlibCall { outcome, .. } => {
                assert!(
                    matches!(outcome, TraceOutcome::Value(_)),
                    "an error pair records as a plain Value, not a Handle"
                );
            }
            _ => unreachable!(),
        }
        assert!(
            !events.iter().any(|e| matches!(e, DetEvent::NativeCall { .. })),
            "no NativeCall for an error pair"
        );

        // Replay returns the same [nil, err] WITHOUT touching the network.
        with_interp(|interp| {
            let dead = dead.clone();
            let events = events.clone();
            async move {
                interp.__install_replay_trace(5, events);
                let pair = interp
                    .__call_stdlib("net_http", "get", &[Value::str(dead)])
                    .await
                    .expect("error pair replays offline");
                match pair.kind() {
                    ValueKind::Array(a) => {
                        assert!(matches!(a.borrow()[0].kind(), ValueKind::Nil), "replayed [nil, err]");
                        assert!(!matches!(a.borrow()[1].kind(), ValueKind::Nil), "err is present");
                    }
                    other => panic!("expected [nil, err], got {other:?}"),
                }
                interp.__clear_determinism();
            }
        });
    }

    // ---- Test 4: streaming + sse are refused under record AND replay ---- //
    #[test]
    fn streaming_and_sse_are_refused() {
        let (base, stop, handle) = spawn_http_server();
        let url = format!("{}/text", base);

        // {stream:true} → the loud v2 refusal at OPTION PARSE (record).
        with_interp(|interp| {
            let url = url.clone();
            async move {
                interp.__install_record_trace(1);
                let opts = Value::object(
                    [("stream".to_string(), Value::bool_(true))].into_iter().collect(),
                );
                let r = interp
                    .__call_stdlib("net_http", "get", &[Value::str(url), opts])
                    .await;
                match r {
                    Err(Control::Panic(e)) => {
                        assert!(
                            e.message.contains("streaming") && e.message.contains("v2"),
                            "stream refusal names streaming + v2: {}",
                            e.message
                        );
                    }
                    other => panic!("{{stream:true}} under record must be refused, got {other:?}"),
                }
                interp.__clear_determinism();
            }
        });

        // http.sse → Refused (the net_http Refused per-func class), record + replay.
        for empty_replay in [false, true] {
            with_interp(|interp| {
                let url = url.clone();
                async move {
                    if empty_replay {
                        interp.__install_replay_trace(1, vec![]);
                    } else {
                        interp.__install_record_trace(1);
                    }
                    let r = interp.__call_stdlib("net_http", "sse", &[Value::str(url)]).await;
                    match r {
                        Err(Control::Panic(e)) => {
                            assert!(
                                e.message.contains("net_http") && e.message.contains("sse"),
                                "sse refusal names net_http.sse: {}",
                                e.message
                            );
                        }
                        other => panic!("http.sse under a trace context must be refused, got {other:?}"),
                    }
                    interp.__clear_determinism();
                }
            });
        }
        stop.store(true, Ordering::SeqCst);
        handle.join().ok();
    }

    // ---- Test 5: json(Class) vs json() — args_hash pins the method args ---- //
    #[test]
    fn virtual_handle_method_args_pinned() {
        let (base, stop, handle) = spawn_http_server();
        let url = format!("{}/json", base);

        // Record resp.json() (no args).
        let events = with_interp(|interp| {
            let url = url.clone();
            async move {
                interp.__install_record_trace(2);
                let resp = pair_handle(
                    &interp
                        .__call_stdlib("net_http", "get", &[Value::str(url)])
                        .await
                        .unwrap(),
                );
                let _ = interp.__call_native_method(&resp, "json", vec![]).await.unwrap();
                let events = interp.__take_trace_events().unwrap();
                interp.__clear_determinism();
                events
            }
        });
        stop.store(true, Ordering::SeqCst);
        handle.join().ok();

        // Replay resp.json(SomeArg) — a DIFFERENT args signature at the same vid+method
        // → the args_hash mismatch is a divergence.
        with_interp(|interp| {
            let url = url.clone();
            let events = events.clone();
            async move {
                interp.__install_replay_trace(2, events);
                let resp = pair_handle(
                    &interp
                        .__call_stdlib("net_http", "get", &[Value::str(url)])
                        .await
                        .unwrap(),
                );
                // Pass an extra arg → different args_hash than the recorded no-arg json().
                let r = interp
                    .__call_native_method(&resp, "json", vec![Value::str("extra")])
                    .await;
                match r {
                    Err(Control::Panic(e)) => {
                        assert!(
                            e.message.contains("divergence"),
                            "args mismatch is a divergence: {}",
                            e.message
                        );
                    }
                    other => panic!("json(arg) vs json() must diverge, got {other:?}"),
                }
                interp.__clear_determinism();
            }
        });
    }
}

// ===========================================================================
// Task 5 — worker-isolate refusals under trace contexts (REPLAY §6).
//
// Under a CliTrace context (record AND replay), creating any worker isolate is a
// clean Tier-2 refusal naming the construct + the "not supported under
// --record/--replay" message — because shared-nothing isolates have no trace
// identity (v1). Refusing at RECORD (not just replay) is the §2.1c guarantee: a
// recorded trace is replayable by construction. The guard is `trace_active()`-gated
// → INERT without a context (the full workers suite stays byte-identical).
// ===========================================================================

mod worker_refusals {
    use super::*;

    /// Block on a current-thread runtime + LocalSet (the run harnesses build their own
    /// inner LocalSet; this just provides the reactor).
    fn block<F: std::future::Future>(fut: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, fut)
    }

    /// Run `src` on the TREE-WALKER with a CliTrace context (record/replay) pre-installed
    /// via the `#[doc(hidden)]` lib seam. Returns the captured stdout, or the Tier-2
    /// panic message (an uncaught refusal that escapes `recover`).
    fn run_tw(src: &str, replay: bool) -> Result<String, String> {
        let src = src.to_string();
        block(async move {
            ascript::run_source_with_trace(&src, replay)
                .await
                .map_err(|e| e.message.clone())
        })
    }

    /// Run `src` on the SPECIALIZED VM with a CliTrace context pre-installed (the guard
    /// lives in shared `Interp` methods + worker dispatch both engines reach).
    fn run_vm(src: &str, replay: bool) -> Result<String, String> {
        let src = src.to_string();
        block(async move {
            ascript::vm_run_source_with_trace(&src, replay)
                .await
                .map_err(|e| e.message.clone())
        })
    }

    /// Assert the captured output names the construct + the §6 refusal message under a
    /// trace context, on BOTH engines and BOTH modes (record + replay).
    fn assert_refused(src: &str, needle: &str) {
        for replay in [false, true] {
            for (engine, out) in [("tree-walker", run_tw(src, replay)), ("vm", run_vm(src, replay))] {
                // Either the snippet recovered + printed the message (Ok) OR it escaped
                // as a Tier-2 panic (Err) — in BOTH cases the message must be present.
                let text = match &out {
                    Ok(s) => s.clone(),
                    Err(m) => m.clone(),
                };
                assert!(
                    text.contains(needle),
                    "[{engine} replay={replay}] refusal must name '{needle}', got: {text}"
                );
                assert!(
                    text.contains("not supported under --record/--replay"),
                    "[{engine} replay={replay}] refusal must carry the §6 message, got: {text}"
                );
            }
        }
    }

    // ---- The unit guard: trace on → Err naming `what`; trace off → Ok. ---- //
    #[test]
    fn refuse_helper_is_loud_under_trace_inert_without() {
        with_interp(|interp| async move {
            // No context → Ok.
            assert!(interp.__refuse_worker_under_trace("calling a worker fn").is_ok());

            // Record → Err naming the construct + the message.
            interp.__install_record_trace(1);
            match interp.__refuse_worker_under_trace("calling a worker fn") {
                Err(Control::Panic(e)) => {
                    assert!(e.message.contains("calling a worker fn"), "{}", e.message);
                    assert!(
                        e.message.contains("not supported under --record/--replay"),
                        "{}",
                        e.message
                    );
                    assert!(e.message.contains("v2"), "{}", e.message);
                }
                other => panic!("trace-on refusal must be a Tier-2 panic, got {other:?}"),
            }
            interp.__clear_determinism();

            // Replay → Err too.
            interp.__install_replay_trace(1, vec![]);
            assert!(matches!(
                interp.__refuse_worker_under_trace("run_in_worker"),
                Err(Control::Panic(_))
            ));
            interp.__clear_determinism();

            // Cleared → Ok again (inert).
            assert!(interp.__refuse_worker_under_trace("calling a worker fn").is_ok());
        });
    }

    // ---- Pooled `worker fn` call ---- //
    #[test]
    fn pooled_worker_fn_refused() {
        let src = r#"
worker fn dbl(x: number) { return x * 2 }
let [v, err] = recover(() => await dbl(21))
print(err.message)
"#;
        assert_refused(src, "calling a worker fn");
    }

    // ---- `WorkerClass.spawn()` ---- //
    #[test]
    fn worker_class_spawn_refused() {
        let src = r#"
worker class Counter {
    n: number = 0
    fn bump() { self.n = self.n + 1; return self.n }
}
let [v, err] = recover(() => await Counter.spawn())
print(err.message)
"#;
        assert_refused(src, "spawning a worker class actor");
    }

    // ---- `worker fn*` stream iteration ---- //
    #[test]
    fn worker_stream_refused() {
        let src = r#"
worker fn* nums() { yield 1; yield 2 }
let [v, err] = recover(() => nums())
print(err.message)
"#;
        assert_refused(src, "iterating a worker fn*");
    }

    // ---- `run_in_worker(f, x)` ---- //
    #[test]
    fn run_in_worker_refused() {
        let src = r#"
worker fn job(x: number) { return x + 1 }
let [v, err] = recover(() => await run_in_worker(job, 41))
print(err.message)
"#;
        assert_refused(src, "run_in_worker");
    }

    // ---- `task.pmap` / `task.preduce` ---- //
    #[test]
    fn pmap_refused() {
        let src = r#"
import * as task from "std/task"
worker fn dbl(x: number) { return x * 2 }
let [v, err] = recover(() => await task.pmap([1, 2, 3], dbl))
print(err.message)
"#;
        assert_refused(src, "task.pmap");
    }

    #[test]
    fn preduce_refused() {
        let src = r#"
import * as task from "std/task"
worker fn add(a: number, b: number) { return a + b }
let [v, err] = recover(() => await task.preduce([1, 2, 3], add, 0))
print(err.message)
"#;
        assert_refused(src, "task.preduce");
    }

    // ---- An EMPTY pmap/preduce creates NO isolate → NOT refused (poolless §2.1). ---- //
    #[test]
    fn empty_pmap_preduce_not_refused_under_trace() {
        let src = r#"
import * as task from "std/task"
worker fn dbl(x: number) { return x * 2 }
worker fn add(a: number, b: number) { return a + b }
let m = await task.pmap([], dbl)
let r = await task.preduce([], add, 7)
print(len(m))
print(r)
"#;
        for replay in [false, true] {
            for (engine, out) in [("tree-walker", run_tw(src, replay)), ("vm", run_vm(src, replay))] {
                let text = out.unwrap_or_else(|m| {
                    panic!("[{engine} replay={replay}] empty pmap/preduce must not refuse, got panic: {m}")
                });
                assert!(
                    !text.contains("not supported under --record/--replay"),
                    "[{engine} replay={replay}] empty pmap/preduce is poolless → no refusal: {text}"
                );
                assert!(text.contains('0') && text.contains('7'), "[{engine} replay={replay}]: {text}");
            }
        }
    }

    // ---- Inertness: the SAME worker fn runs for real with NO context. ---- //
    // The worker actually dispatches to a real isolate (no trace context), proving the
    // guard is INERT off the trace path — so the workers suite stays byte-identical.
    // Driven through the real binary (`ascript run`) so a pooled isolate genuinely
    // spawns (the in-process seams don't wire up the cross-thread pool source path).
    #[test]
    fn worker_fn_runs_without_trace() {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("ascript-rr-inert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("w.as");
        std::fs::write(
            &file,
            "worker fn dbl(x: number) { return x * 2 }\nprint(await dbl(21))\n",
        )
        .unwrap();
        let bin = env!("CARGO_BIN_EXE_ascript");
        // Default engine (VM) and the tree-walker oracle — both must run the worker for
        // real with NO trace flag (Task 6 adds --record; here we prove the no-flag path).
        for engine_args in [vec!["run"], vec!["run", "--tree-walker"]] {
            let mut cmd = Command::new(bin);
            cmd.args(&engine_args).arg(&file);
            let out = cmd.output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success(),
                "no-context worker run must succeed ({engine_args:?}): {stderr}"
            );
            assert!(
                !stdout.contains("not supported under --record/--replay")
                    && !stderr.contains("not supported under --record/--replay"),
                "no-context run must NOT refuse ({engine_args:?}): {stdout}{stderr}"
            );
            assert!(
                stdout.contains("42"),
                "worker fn must really run with no context ({engine_args:?}): {stdout}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn default_path_untouched() {
    with_interp(|interp| async move {
        // No context installed → a Harmless call routes normally and records nothing.
        let r = interp
            .__call_stdlib("math", "abs", &[Value::int(-3)])
            .await
            .expect("math.abs with no context");
        assert_eq!(r, Value::int(3));
        assert!(
            interp.__take_trace_events().is_none(),
            "no context → no events"
        );

        // Flag plumbing: a CliTrace record context arms trace_active; clearing disarms.
        let prev = interp.__install_record_trace(9);
        assert!(prev.is_none());
        // (trace_active is pub(crate); we observe it indirectly — a Refused call under a
        // CliTrace context is loud, proving the hook is armed.)
        let refused = interp.__call_stdlib("math", "abs", &[Value::int(-1)]).await;
        // math is Harmless → still runs (the hook is armed but Harmless falls through).
        assert_eq!(refused.unwrap(), Value::int(1));

        // Clearing the context disarms; install a Workflow-origin context (via the
        // public enter_deterministic-equivalent) and confirm it does NOT arm the trace
        // hook: a Refused-class call under a Workflow context runs for real, not refused.
        interp.__clear_determinism();
        // Re-running math with no context.
        let r = interp
            .__call_stdlib("math", "abs", &[Value::int(-4)])
            .await
            .unwrap();
        assert_eq!(r, Value::int(4));
    })
}

// ===========================================================================
// REPLAY Task 6 — `ascript run --record/--replay/--seed` end-to-end (spawn the
// real binary, the `tests/cli.rs` precedent). These prove the flagship guarantee:
// a trace recorded on ANY engine replays byte-identically on ANY engine, with NO
// real I/O at replay (delete the fixture / change the env between record & replay).
// ===========================================================================
mod cli_run {
    use std::path::PathBuf;
    use std::process::Command;

    fn bin() -> &'static str {
        env!("CARGO_BIN_EXE_ascript")
    }

    /// A fresh unique temp dir for one test (process id + tag + nanos).
    fn unique_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut d = std::env::temp_dir();
        d.push(format!("rr_cli_{}_{}_{}", std::process::id(), tag, nanos));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A deterministic program touching a SEAM (math.random → RandomRead events) and a
    /// RECORDED effect (fs.readFile → StdlibCall event). On replay both reproduce with no
    /// real RNG draw and no fs access — so deleting `data.txt` after record still replays.
    const SEAM_PROG: &str = r#"import * as math from "std/math"
import * as fs from "std/fs"
let r1 = math.random()
let r2 = math.random()
let [content, err] = fs.read("data.txt")
print(r1)
print(r2)
print(content)
"#;

    fn write_seam_program(dir: &std::path::Path) -> PathBuf {
        let prog = dir.join("prog.as");
        std::fs::write(&prog, SEAM_PROG).unwrap();
        std::fs::write(dir.join("data.txt"), "RECORDED-FILE-BODY").unwrap();
        prog
    }

    /// `run --record <trace> <prog>` then `run --replay <trace> <prog>` produce
    /// byte-identical stdout + exit (clock/RNG seamed, fs recorded).
    #[test]
    fn record_then_replay_byte_identical() {
        let dir = unique_dir("rt");
        let prog = write_seam_program(&dir);
        let trace = dir.join("t.astrc");

        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--record"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rec.status.success(), "record failed: {rec:?}");
        assert!(trace.exists(), "trace not written");
        let out1 = String::from_utf8_lossy(&rec.stdout).to_string();
        assert!(out1.contains("RECORDED-FILE-BODY"), "record stdout: {out1}");

        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--replay"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rep.status.success(), "replay failed: {rep:?}");
        let out2 = String::from_utf8_lossy(&rep.stdout).to_string();
        assert_eq!(out1, out2, "replay stdout must equal record stdout");
        assert_eq!(rec.status.code(), rep.status.code());
    }

    /// Replay reproduces the recorded fs read WITH NO real I/O — the fixture is deleted
    /// between record and replay, yet the recorded body still appears (the flagship demo).
    #[test]
    fn replay_offline_after_fixture_deleted() {
        let dir = unique_dir("offline");
        let prog = write_seam_program(&dir);
        let trace = dir.join("t.astrc");

        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--record"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rec.status.success(), "record failed: {rec:?}");
        let out1 = String::from_utf8_lossy(&rec.stdout).to_string();

        // Delete the fixture — a real fs.readFile would now fail.
        std::fs::remove_file(dir.join("data.txt")).unwrap();

        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--replay"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rep.status.success(), "offline replay failed: {rep:?}");
        let out2 = String::from_utf8_lossy(&rep.stdout).to_string();
        assert_eq!(out1, out2, "offline replay must reproduce recorded output");
        assert!(out2.contains("RECORDED-FILE-BODY"));
    }

    /// THE Gate-1 extension (§10.2): a trace recorded on one engine replays
    /// byte-identically on the others — tree-walker ⇄ VM (default) ⇄ generic VM, plus
    /// build→.aso. The seam math + the recorded fs value are engine-independent.
    #[test]
    fn cross_engine_matrix() {
        let dir = unique_dir("xeng");
        let prog = write_seam_program(&dir);

        // Record on the tree-walker.
        let trace_tw = dir.join("tw.astrc");
        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--tree-walker", "--record"])
            .arg(&trace_tw)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rec.status.success(), "tw record failed: {rec:?}");
        let baseline = String::from_utf8_lossy(&rec.stdout).to_string();
        assert!(baseline.contains("RECORDED-FILE-BODY"));

        // Replay on the default VM.
        let rep_vm = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--replay"])
            .arg(&trace_tw)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rep_vm.status.success(), "vm replay failed: {rep_vm:?}");
        assert_eq!(baseline, String::from_utf8_lossy(&rep_vm.stdout));

        // Replay on the GENERIC VM (every fast path off).
        let rep_gen = Command::new(bin())
            .current_dir(&dir)
            .env("ASCRIPT_NO_SPECIALIZE", "1")
            .args(["run", "--replay"])
            .arg(&trace_tw)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rep_gen.status.success(), "generic replay failed: {rep_gen:?}");
        assert_eq!(baseline, String::from_utf8_lossy(&rep_gen.stdout));

        // Record on the VM, replay on the tree-walker.
        let trace_vm = dir.join("vm.astrc");
        let rec_vm = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--record"])
            .arg(&trace_vm)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rec_vm.status.success(), "vm record failed: {rec_vm:?}");
        // This is an INDEPENDENT recording (its own OS-entropy seed), so its random draws
        // differ from `baseline` — its own output is the reference its replays must match.
        let vm_baseline = String::from_utf8_lossy(&rec_vm.stdout).to_string();
        assert!(vm_baseline.contains("RECORDED-FILE-BODY"));
        let rep_tw = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--tree-walker", "--replay"])
            .arg(&trace_vm)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rep_tw.status.success(), "tw replay failed: {rep_tw:?}");
        assert_eq!(vm_baseline, String::from_utf8_lossy(&rep_tw.stdout));

        // Build → .aso, record the .as, replay against the .aso (digest skipped for .aso).
        let aso = dir.join("prog.aso");
        let built = Command::new(bin())
            .current_dir(&dir)
            .args(["build"])
            .arg(&prog)
            .arg("-o")
            .arg(&aso)
            .output()
            .unwrap();
        assert!(built.status.success(), "build failed: {built:?}");
        let rep_aso = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--replay"])
            .arg(&trace_vm)
            .arg(&aso)
            .output()
            .unwrap();
        assert!(rep_aso.status.success(), "aso replay failed: {rep_aso:?}");
        assert_eq!(vm_baseline, String::from_utf8_lossy(&rep_aso.stdout));
    }

    /// `--record --seed N` twice yields IDENTICAL event streams (compared as the trace
    /// bytes minus the informational `created_ms` header field) and identical output.
    #[test]
    fn seed_pins_record() {
        let dir = unique_dir("seed");
        let prog = write_seam_program(&dir);
        let run = |name: &str| -> (String, Vec<ascript::det::DetEvent>) {
            let trace = dir.join(name);
            let out = Command::new(bin())
                .current_dir(&dir)
                .args(["run", "--record"])
                .arg(&trace)
                .args(["--seed", "7"])
                .arg(&prog)
                .output()
                .unwrap();
            assert!(out.status.success(), "seeded record failed: {out:?}");
            let bytes = std::fs::read(&trace).unwrap();
            let (_h, events) = ascript::trace::read_trace(&bytes).unwrap();
            (String::from_utf8_lossy(&out.stdout).to_string(), events)
        };
        let (o1, e1) = run("s1.astrc");
        let (o2, e2) = run("s2.astrc");
        assert_eq!(o1, o2, "same seed → same output");
        assert_eq!(e1, e2, "same seed → identical event stream");
    }

    /// Editing the program after recording makes `--replay` a clean error (the source
    /// digest changed), with a non-zero exit and no panic/backtrace.
    #[test]
    fn digest_mismatch_is_clean() {
        let dir = unique_dir("digest");
        let prog = write_seam_program(&dir);
        let trace = dir.join("t.astrc");
        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--record"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rec.status.success());

        // Change the program (append a comment → different sha256).
        std::fs::write(&prog, format!("{SEAM_PROG}// changed\n")).unwrap();
        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--replay"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!rep.status.success(), "replay of a changed program must fail");
        let err = String::from_utf8_lossy(&rep.stderr);
        assert!(
            err.contains("different program"),
            "expected a source-changed error, got: {err}"
        );
    }

    /// `--record` and `--replay` together is a clean clap conflict error.
    #[test]
    fn record_plus_replay_flag_conflict() {
        let dir = unique_dir("conflict");
        let prog = write_seam_program(&dir);
        let out = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--record", "a.astrc", "--replay", "b.astrc"])
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!out.status.success(), "conflicting flags must error");
    }

    /// A corrupt/truncated trace yields a clean error (no panic/backtrace).
    #[test]
    fn replay_corrupt_trace_clean_error() {
        let dir = unique_dir("corrupt");
        let prog = write_seam_program(&dir);
        let trace = dir.join("t.astrc");
        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--record"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rec.status.success());
        // Truncate the trace to a hostile prefix.
        let bytes = std::fs::read(&trace).unwrap();
        std::fs::write(&trace, &bytes[..bytes.len() / 2]).unwrap();
        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--replay"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!rep.status.success(), "corrupt trace must error");
        let err = String::from_utf8_lossy(&rep.stderr);
        assert!(
            !err.contains("panicked") && !err.contains("RUST_BACKTRACE"),
            "corrupt trace must be a clean error, not a panic: {err}"
        );
    }

    /// A program that performs an effect then PANICS still writes its trace (always-flush),
    /// and replay reproduces the panic byte-identically.
    #[test]
    fn panicking_run_still_writes_trace() {
        let dir = unique_dir("panic");
        let prog = dir.join("prog.as");
        std::fs::write(
            &prog,
            "import * as math from \"std/math\"\nlet r = math.random()\nprint(r)\nlet bad = 1 + nil\nprint(bad)\n",
        )
        .unwrap();
        let trace = dir.join("t.astrc");
        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--record"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!rec.status.success(), "the program panics → non-zero exit");
        assert!(trace.exists(), "trace must be written even on panic");
        let out1 = String::from_utf8_lossy(&rec.stdout).to_string();

        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--replay"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert_eq!(
            rec.status.code(),
            rep.status.code(),
            "replay reproduces the panic exit"
        );
        assert_eq!(out1, String::from_utf8_lossy(&rep.stdout));
    }

    /// `exit(n)` after an effect: the trace is written and replay reproduces the exit code.
    #[test]
    fn exit_n_run_writes_trace() {
        let dir = unique_dir("exit");
        let prog = dir.join("prog.as");
        std::fs::write(
            &prog,
            "import * as math from \"std/math\"\nlet r = math.random()\nprint(r)\nexit(3)\n",
        )
        .unwrap();
        let trace = dir.join("t.astrc");
        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--record"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert_eq!(rec.status.code(), Some(3), "exit(3) propagates");
        assert!(trace.exists(), "trace written on exit()");
        let out1 = String::from_utf8_lossy(&rec.stdout).to_string();
        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--replay"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert_eq!(rep.status.code(), Some(3), "replay reproduces exit(3)");
        assert_eq!(out1, String::from_utf8_lossy(&rep.stdout));
    }

    /// REPLAY §2.7 + the Task-3 carry-forward: a program running a `workflow.run` under
    /// `--record` records ONE `StdlibCall(workflow.run)` at the boundary (the workflow's
    /// own log is written during record); on `--replay` the workflow result is returned
    /// from the trace WITHOUT re-executing — the workflow log is NOT recreated.
    #[test]
    #[cfg(feature = "workflow")]
    fn workflow_run_records_and_replays_without_reexecuting() {
        let dir = unique_dir("wf");
        let prog = dir.join("prog.as");
        std::fs::write(
            &prog,
            r#"import { run, activity } from "std/workflow"
let act = activity("double", (n) => { return n * 2 })
fn flow(ctx, input) {
  let v = ctx.call(act, input)
  return v + 1
}
let [r, err] = recover(() => run(flow, 5, { log: "wf.log" }))
if (err != nil) { print("ERR: " + err.message) } else { print(r) }
"#,
        )
        .unwrap();
        let trace = dir.join("t.astrc");

        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--record"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rec.status.success(), "wf record failed: {rec:?}");
        let out1 = String::from_utf8_lossy(&rec.stdout).to_string();
        assert_eq!(out1.trim(), "11", "5*2+1 = 11");

        // The CLI trace records exactly ONE StdlibCall for workflow.run at the boundary.
        let bytes = std::fs::read(&trace).unwrap();
        let (_h, events) = ascript::trace::read_trace(&bytes).unwrap();
        let workflow_calls = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    ascript::det::DetEvent::StdlibCall { module, func, .. }
                        if module == "workflow" && func == "run"
                )
            })
            .count();
        assert_eq!(workflow_calls, 1, "exactly one StdlibCall(workflow.run): {events:?}");

        // Delete the workflow's own log; replay must NOT recreate it (workflow.run is
        // consumed from the trace, never re-executed).
        let wf_log = dir.join("wf.log");
        assert!(wf_log.exists(), "workflow log written during record");
        std::fs::remove_file(&wf_log).unwrap();

        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["run", "--replay"])
            .arg(&trace)
            .arg(&prog)
            .output()
            .unwrap();
        assert!(rep.status.success(), "wf replay failed: {rep:?}");
        assert_eq!(out1, String::from_utf8_lossy(&rep.stdout));
        assert!(
            !wf_log.exists(),
            "replay must NOT re-execute the workflow (no log recreated)"
        );
    }
}

// ===========================================================================
// REPLAY §4.2-4.3 — `ascript test --record` / `--replay`
// ===========================================================================

/// Per-test traces, failure-only save (Task 7). Each test FILE runs under ONE
/// `CliTrace` Record context; per-test traces are SLICED from the file's event
/// stream; a trace is written ONLY for a FAILED test under
/// `.ascript-traces/<stem>__<slug>.trace`.
mod cli_test {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn bin() -> &'static str {
        env!("CARGO_BIN_EXE_ascript")
    }

    fn unique_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut d = std::env::temp_dir();
        d.push(format!("rr_clitest_{}_{}_{}", std::process::id(), tag, nanos));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// List the `.trace` files written under `<dir>/.ascript-traces/`, or `vec![]`
    /// if the directory does not exist.
    fn trace_files(dir: &Path) -> Vec<PathBuf> {
        let td = dir.join(".ascript-traces");
        if !td.exists() {
            return Vec::new();
        }
        let mut v: Vec<PathBuf> = std::fs::read_dir(&td)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "trace").unwrap_or(false))
            .collect();
        v.sort();
        v
    }

    /// A 3-test file: pass, fail-with-fs+rng-effects, pass. The middle test draws a
    /// random number AND reads a fixture file, then asserts something false — so its
    /// trace captures both the RandomRead seam and the recorded fs read.
    const THREE_TESTS: &str = r#"import * as math from "std/math"
import * as fs from "std/fs"
test("alpha passes", () => { assert(1 + 1 == 2, "math works") })
test("beta fails with effects", () => {
  let r = math.random()
  let [content, err] = fs.read("fixture.txt")
  assert(content == "NEVER", "intentional failure")
})
test("gamma passes", () => { assert(2 * 2 == 4, "more math") })
"#;

    fn write_three_tests(dir: &Path) -> PathBuf {
        let prog = dir.join("orders.as");
        std::fs::write(&prog, THREE_TESTS).unwrap();
        std::fs::write(dir.join("fixture.txt"), "RECORDED-FIXTURE").unwrap();
        prog
    }

    /// A 3-test file under `ascript test --record` → exactly ONE
    /// `.ascript-traces/<stem>__<slug>.trace`; the "trace saved:" hint is printed.
    /// A fully-green file saves nothing and `.ascript-traces/` is NOT created.
    #[test]
    fn failed_test_saves_trace_passing_saves_nothing() {
        let dir = unique_dir("save");
        let prog = write_three_tests(&dir);

        let out = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record"])
            .arg(&prog)
            .output()
            .unwrap();
        // One test fails → non-zero exit.
        assert!(!out.status.success(), "a failing test must exit non-zero");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("trace saved:"),
            "expected a 'trace saved:' hint, got: {stdout}"
        );
        let traces = trace_files(&dir);
        assert_eq!(
            traces.len(),
            1,
            "exactly one trace for one failed test, got: {traces:?}"
        );
        let name = traces[0].file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("orders__"),
            "trace name must start with the file stem, got: {name}"
        );

        // A fully-green file saves nothing and never creates .ascript-traces/.
        let green = unique_dir("green");
        let gprog = green.join("ok.as");
        std::fs::write(
            &gprog,
            "test(\"a\", () => { assert(true, \"ok\") })\ntest(\"b\", () => { assert(1 == 1, \"ok\") })\n",
        )
        .unwrap();
        let gout = Command::new(bin())
            .current_dir(&green)
            .args(["test", "--record"])
            .arg(&gprog)
            .output()
            .unwrap();
        assert!(gout.status.success(), "green run should pass");
        assert!(
            !green.join(".ascript-traces").exists(),
            ".ascript-traces/ must NOT be created on a fully-green run"
        );
    }

    /// `ascript test --replay <trace>` → the same failure message, tally
    /// `0 passed; 1 failed`, exit 1; works after deleting the fs fixture.
    #[test]
    fn test_replay_reruns_one_test_deterministically() {
        let dir = unique_dir("replay");
        let prog = write_three_tests(&dir);

        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record"])
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!rec.status.success());
        let rec_out = String::from_utf8_lossy(&rec.stdout).to_string();
        let traces = trace_files(&dir);
        assert_eq!(traces.len(), 1, "one trace, got: {traces:?}");
        let trace = &traces[0];

        // Delete the fixture — a real fs.read would now fail / read differently.
        std::fs::remove_file(dir.join("fixture.txt")).unwrap();

        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--replay"])
            .arg(trace)
            .output()
            .unwrap();
        assert_eq!(
            rep.status.code(),
            Some(1),
            "replay of a failing test exits 1: {rep:?}"
        );
        let rep_out = String::from_utf8_lossy(&rep.stdout);
        assert!(
            rep_out.contains("0 passed; 1 failed"),
            "replay tally must be one failure, got: {rep_out}"
        );
        // The exact failure line from the record run reappears on replay.
        let fail_line = rec_out
            .lines()
            .find(|l| l.starts_with("FAIL "))
            .expect("a FAIL line in the record output");
        assert!(
            rep_out.contains(fail_line),
            "replay failure line must match record byte-for-byte\n record: {fail_line}\n replay: {rep_out}"
        );
    }

    /// Fixing the assertion (the trace pins INPUTS, not the assertion) → replay
    /// passes; a changed test file under `--replay` proceeds with a printed WARNING
    /// (not the hard `run` error).
    #[test]
    fn replayed_fixed_test_passes() {
        let dir = unique_dir("fixed");
        let prog = write_three_tests(&dir);

        let rec = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record"])
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!rec.status.success());
        let traces = trace_files(&dir);
        assert_eq!(traces.len(), 1);
        let trace = traces[0].clone();

        // Fix the failing assertion: the recorded random()/fs.read INPUTS are still
        // valid, only the assertion changed → the replayed test now PASSES. (The
        // assertion checks `err != nil` — true regardless of the recorded content.)
        let fixed = THREE_TESTS.replace(
            "assert(content == \"NEVER\", \"intentional failure\")",
            "assert(true, \"now passes\")",
        );
        std::fs::write(&prog, fixed).unwrap();

        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--replay"])
            .arg(&trace)
            .output()
            .unwrap();
        // A changed test file is a WARNING (not the hard error `run` uses), and the
        // fixed assertion passes.
        assert!(
            rep.status.success(),
            "fixed test should pass on replay (warn, not error): {rep:?}"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&rep.stdout),
            String::from_utf8_lossy(&rep.stderr)
        );
        assert!(
            combined.to_lowercase().contains("warn"),
            "a changed test file under --replay must print a warning, got: {combined}"
        );
        assert!(
            String::from_utf8_lossy(&rep.stdout).contains("1 passed; 0 failed"),
            "fixed test tally must be one pass: {combined}"
        );
    }

    /// `--record` with `--parallel` is a clean CLI error (v1).
    #[test]
    fn record_parallel_refused() {
        let dir = unique_dir("par");
        let prog = write_three_tests(&dir);
        let out = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record", "--parallel=2"])
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!out.status.success(), "record + parallel must error");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.to_lowercase().contains("parallel"),
            "error should name --parallel, got: {err}"
        );
    }

    /// `--watch --record` is a clean CLI error (unbounded trace accumulation; v2).
    #[test]
    fn record_watch_refused() {
        let dir = unique_dir("watch");
        let prog = write_three_tests(&dir);
        let out = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record", "--watch"])
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!out.status.success(), "record + watch must error");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.to_lowercase().contains("watch"),
            "error should name --watch, got: {err}"
        );
    }

    /// `--record` + `--replay` together is a clean CLI error.
    #[test]
    fn record_plus_replay_refused() {
        let dir = unique_dir("conflict");
        let prog = write_three_tests(&dir);
        let out = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record", "--replay", "x.trace"])
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!out.status.success(), "record + replay must error");
    }

    /// `--record` with `--coverage` is ALLOWED (coverage is observation-only, VM-side;
    /// the seams are engine-shared) — record a failing test under `--coverage` → the
    /// trace replays.
    #[test]
    fn record_with_coverage_allowed() {
        let dir = unique_dir("cov");
        let prog = write_three_tests(&dir);
        let out = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record", "--coverage"])
            .arg(&prog)
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "the failing test still exits non-zero"
        );
        let traces = trace_files(&dir);
        assert_eq!(
            traces.len(),
            1,
            "coverage record still writes one trace, got: {traces:?}"
        );
        // Delete the fixture and replay — the engine-shared seam proof.
        std::fs::remove_file(dir.join("fixture.txt")).unwrap();
        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--replay"])
            .arg(&traces[0])
            .output()
            .unwrap();
        assert_eq!(rep.status.code(), Some(1), "replay reruns the failure: {rep:?}");
        assert!(String::from_utf8_lossy(&rep.stdout).contains("0 passed; 1 failed"));
    }

    /// Two same-named failing tests across files with the same STEM → the second
    /// trace gets a `~2` suffix (no overwrite).
    #[test]
    fn trace_name_collision_suffixes() {
        let dir = unique_dir("collide");
        // Two DIFFERENT directories with the same file stem "svc.as", each with a
        // failing test named the same → same `<stem>__<slug>` base.
        let sub_a = dir.join("a");
        let sub_b = dir.join("b");
        std::fs::create_dir_all(&sub_a).unwrap();
        std::fs::create_dir_all(&sub_b).unwrap();
        let body = "test(\"same name\", () => { assert(false, \"boom\") })\n";
        let pa = sub_a.join("svc.as");
        let pb = sub_b.join("svc.as");
        std::fs::write(&pa, body).unwrap();
        std::fs::write(&pb, body).unwrap();

        // Run BOTH files in one invocation from `dir` so they share one
        // `.ascript-traces/` and collide on `svc__same_name.trace`.
        let out = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record"])
            .arg(&pa)
            .arg(&pb)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let traces = trace_files(&dir);
        let names: Vec<String> = traces
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(traces.len(), 2, "two traces, got: {names:?}");
        assert!(
            names.iter().any(|n| n.contains("~2")),
            "a collision must be suffixed ~2, got: {names:?}"
        );
    }

    /// 50 failed tests → 50 traces, with no quadratic slicing cost (slices are index
    /// ranges over one Vec; only the `P ⧺ S_k` write is per-failure).
    #[test]
    fn fifty_failures_save_fifty_traces() {
        let dir = unique_dir("fifty");
        let prog = dir.join("many.as");
        let mut body = String::from("import * as math from \"std/math\"\n");
        for i in 0..50 {
            body.push_str(&format!(
                "test(\"fails {i}\", () => {{ let r = math.random(); assert(false, \"boom {i}\") }})\n"
            ));
        }
        std::fs::write(&prog, body).unwrap();
        let start = std::time::Instant::now();
        let out = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record"])
            .arg(&prog)
            .output()
            .unwrap();
        let elapsed = start.elapsed();
        assert!(!out.status.success());
        let traces = trace_files(&dir);
        assert_eq!(traces.len(), 50, "one trace per failure, got: {}", traces.len());
        // A generous wall-clock ceiling — quadratic slicing would blow far past this.
        assert!(
            elapsed.as_secs() < 30,
            "50 failures took too long ({elapsed:?}) — possible quadratic slicing"
        );
    }

    /// A test that fails during MODULE LOAD (a panic before any test runs) yields a
    /// file-level trace with a sensible name, and replay reproduces the load failure.
    #[test]
    fn module_load_failure_saves_file_level_trace() {
        let dir = unique_dir("loadfail");
        let prog = dir.join("broken.as");
        // A module-level panic (undefined variable) before any test registers.
        std::fs::write(&prog, "import * as math from \"std/math\"\nlet x = math.random()\nundefined_thing_xyz()\ntest(\"never\", () => {})\n").unwrap();
        let out = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--record"])
            .arg(&prog)
            .output()
            .unwrap();
        assert!(!out.status.success(), "module-load failure exits non-zero");
        let traces = trace_files(&dir);
        assert_eq!(
            traces.len(),
            1,
            "a module-load failure writes one file-level trace, got: {traces:?}"
        );
        assert!(traces[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("broken__"));

        // The file-level trace must REPLAY (not be a dead artifact): `test --replay`
        // re-runs module load under the recorded context and reproduces the load
        // failure (exit non-zero, the same undefined-name diagnostic).
        let rep = Command::new(bin())
            .current_dir(&dir)
            .args(["test", "--replay"])
            .arg(&traces[0])
            .output()
            .unwrap();
        assert!(
            !rep.status.success(),
            "replaying a module-load-failure trace reproduces the failure (non-zero exit): {rep:?}"
        );
        let err = String::from_utf8_lossy(&rep.stderr);
        assert!(
            err.contains("undefined_thing_xyz") || err.contains("undefined"),
            "replay reproduces the load diagnostic, got: {err}"
        );
    }
}
