//! EMBED Unit E — the rust-host embedding example (spec §12, Gate 9).
//!
//! A game-loop host that drives an embedded AScript isolate. It demonstrates the
//! full §12 surface AND its edge cases (not happy-path-only):
//!
//!   * deny-all capabilities (the embedded default, §7) + a script-side denial probe;
//!   * a `host:game` module with a plain `log` FUNC and a `rand_seeded` FALLIBLE FUNC
//!     (the Tier-1 `[value, err]` tier) plus a `boom` FUNC that raises
//!     `HostError::Panic` (recovered script-side);
//!   * `OutputMode::Capture` — `print` is buffered, drained per tick;
//!   * `Isolate::call` AUTO-AWAITING an `async fn on_save`;
//!   * shared game state read/written across the boundary by CONTAINER HANDLE
//!     (`get_key`/`set_key` on a script-owned `state` object).
//!
//! The example IS its own check: any assertion failure prints a diagnostic and exits
//! non-zero; on success it prints the sentinel `EMBED-RUST-HOST-OK` and exits 0. The
//! runner test in `tests/embed.rs` asserts exactly that.
//!
//! Run: `cargo run --example embed-rust-host --features embed`

use std::cell::Cell;
use std::rc::Rc;

use ascript::embed::{AsValue, Caps, HostError, Isolate, OutputMode};

/// Fail the example loudly (the host IS the test harness — Gate 9).
macro_rules! check {
    ($cond:expr, $($msg:tt)*) => {
        if !($cond) {
            eprintln!("EMBED-RUST-HOST FAIL: {}", format!($($msg)*));
            std::process::exit(1);
        }
    };
}

fn main() {
    // A tiny piece of host state the `rand_seeded` fallible fn closes over — proving a
    // host fn can carry its own state (an `Rc<Cell<..>>` is fine; the isolate is
    // single-threaded, so no `Send` bound applies to a non-factory host module).
    let calls = Rc::new(Cell::new(0u64));
    let calls_for_rand = Rc::clone(&calls);

    // Build the isolate: deny-all caps (§7), captured output, and the `host:game`
    // module. NOTE deny_all is the DEFAULT — shown explicitly for emphasis.
    let iso = Isolate::builder()
        .caps(Caps::deny_all())
        .output(OutputMode::Capture)
        .host_module("host:game", |m| {
            // A plain FUNC: record a log line via the ctx output sink, return nil.
            m.func("log", |ctx, args| {
                let line = args.first().and_then(AsValue::as_str).unwrap_or("");
                ctx.print(&format!("[game.log] {line}\n"));
                Ok(AsValue::nil())
            });
            // A FALLIBLE FUNC (Tier-1 `[value, err]`): a deterministic LCG step keyed
            // by the tick. A NEGATIVE seed is the documented Recoverable failure (the
            // err half) — the script never passes one, but the path is wired.
            m.fallible_func("rand_seeded", move |_ctx, args| {
                calls_for_rand.set(calls_for_rand.get() + 1);
                let seed = args.first().and_then(AsValue::as_int).unwrap_or(0);
                if seed < 0 {
                    return Err(HostError::Recoverable("seed must be non-negative".into()));
                }
                // A pure, deterministic pseudo-random in [0, 100).
                let r = (seed.wrapping_mul(1103515245).wrapping_add(12345) >> 8) % 100;
                Ok(AsValue::from(r.abs()))
            });
            // A FUNC that ALWAYS raises HostError::Panic (a Tier-2 recoverable panic)
            // — the script recovers it via the arrow-form `recover`.
            m.func("boom", |_ctx, args| {
                let what = args.first().and_then(AsValue::as_str).unwrap_or("?");
                Err(HostError::Panic(format!("host detonated: {what}")))
            });
        })
        .expect("register host:game")
        .build()
        .expect("build isolate");

    // Load the script (defines on_tick/on_save/probe_host_panic/probe_caps_denial +
    // the module-scope `state` object).
    let game_src = include_str!("game.as");
    iso.eval(game_src).expect("load game.as");
    // The boot-time print buffer (game.as has none at module scope) — drain so per-tick
    // capture is clean.
    let _ = iso.take_output();

    // Grab the script-owned `state` object as a live HANDLE. Host writes via set_key
    // are visible to the script and vice-versa (same ObjectCell — no deep copy).
    let state = iso.global("state").expect("script `state` global");
    check!(
        state.get_key("tick").and_then(|v| v.as_int()) == Some(0),
        "initial state.tick should be 0"
    );

    // ── EDGE 1: capabilities denial (deny-all → fs.read denied). ────────────────
    let denial = iso
        .call("probe_caps_denial", &[])
        .expect("call probe_caps_denial");
    let denial_msg = denial.as_str().unwrap_or("");
    check!(
        denial_msg.contains("fs denied") && denial_msg.contains("capability 'fs' denied"),
        "deny-all isolate must deny fs.read; got: {denial_msg:?}"
    );
    println!("denial: {denial_msg}");

    // ── EDGE 2: host panic recovered script-side (HostError::Panic). ────────────
    let recovered = iso
        .call("probe_host_panic", &[])
        .expect("call probe_host_panic");
    let recovered_msg = recovered.as_str().unwrap_or("");
    check!(
        recovered_msg.contains("recovered host panic")
            && recovered_msg.contains("host detonated: detonate"),
        "host panic must be recoverable script-side; got: {recovered_msg:?}"
    );
    println!("recovered: {recovered_msg}");

    // ── The 5-tick game loop. ───────────────────────────────────────────────────
    let mut total_score: i64 = 0;
    for n in 1..=5i64 {
        // Call on_tick(n) — it invokes the host log FUNC + the fallible rand FUNC.
        let delta = iso
            .call("on_tick", &[AsValue::from(n)])
            .expect("call on_tick");
        // The host log line is in the capture buffer; drain + assert it landed.
        let tick_out = iso.take_output();
        check!(
            tick_out.contains(&format!("tick {n}")),
            "tick {n}: expected host log line; got: {tick_out:?}"
        );

        // on_tick returned a state DELTA object: { delta: int, note: str }.
        check!(
            delta.kind() == ascript::embed::AsKind::Object,
            "tick {n}: on_tick must return an object delta"
        );
        let d = delta.get_key("delta").and_then(|v| v.as_int()).unwrap_or(0);
        let note = delta
            .get_key("note")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        check!(note == "ok", "tick {n}: note should be ok; got {note:?}");

        // Apply the delta to the SHARED state object BY HANDLE (host writes, script
        // sees). Read tick/score, write back the updated values.
        total_score += d;
        state
            .set_key("tick", AsValue::from(n))
            .expect("set state.tick");
        state
            .set_key("score", AsValue::from(total_score))
            .expect("set state.score");

        // Read the state back through the SAME handle — proves the write is live.
        let observed_tick = state.get_key("tick").and_then(|v| v.as_int());
        let observed_score = state.get_key("score").and_then(|v| v.as_int());
        check!(
            observed_tick == Some(n) && observed_score == Some(total_score),
            "tick {n}: shared-state read-back mismatch (tick={observed_tick:?}, score={observed_score:?})"
        );
        println!("state: tick={n} score={total_score} delta={d}");
    }

    // The script must observe the host's writes too — call back into the script and
    // read `state.score` from the script's side (auto-await on_save proves the value
    // crossed the boundary live, not a host-side copy).
    let saved = iso.call("on_save", &[]).expect("call on_save (async, auto-await)");
    let saved_msg = saved.as_str().unwrap_or("");
    check!(
        saved_msg.contains("tick 5") && saved_msg.contains(&format!("score {total_score}")),
        "async on_save must observe host-written shared state; got: {saved_msg:?}"
    );
    println!("saved: {saved_msg}");

    // The fallible host fn was called once per tick (5×).
    check!(
        calls.get() == 5,
        "rand_seeded should have been called 5 times; got {}",
        calls.get()
    );

    println!("EMBED-RUST-HOST-OK");
}
