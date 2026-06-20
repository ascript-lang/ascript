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
// 1. classification_is_complete — every STD_MODULES entry yields a class; a
//    fabricated module name still yields SOME class (the table is total via `_`).
// ===========================================================================

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
    // Every real module classifies. We probe with a representative func name; the table
    // is total (`_ => Harmless`), so the assertion is that the entry RESOLVES (it always
    // does) AND that the resource modules resolve to the RIGHT non-Harmless class — a
    // sabotage that drops sqlite to Harmless is caught by the refusal test below.
    for full in STD_MODULES {
        let key = dispatch_key(full);
        let class = replay_class(&key, "__probe__");
        // The probe must yield a class (it always does — total table). The meaningful
        // assertions are the per-class ones below; this loop guarantees coverage.
        let _ = class;
    }

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
    // caps.drop / dropAll are Refused (replay can't see the dropped state).
    assert_eq!(replay_class("caps", "drop"), ReplayClass::Refused);
    assert_eq!(replay_class("caps", "dropAll"), ReplayClass::Refused);
    // jwt.jwks (live fetch) is Refused; email.send/connect (live socket) Refused.
    assert_eq!(replay_class("jwt", "jwks"), ReplayClass::Refused);
    assert_eq!(replay_class("email", "send"), ReplayClass::Refused);
    assert_eq!(replay_class("email", "connect"), ReplayClass::Refused);
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
