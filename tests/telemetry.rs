//! SP12 `std/telemetry` capture-mode tests. Run with `--features telemetry`
//! (the feature is not in `default`, so the whole file is `#[cfg]`-gated and is
//! empty/compiles-clean in the default config). No socket, no secret: telemetry
//! runs in capture mode and the recorded exporter HTTP payloads are read back via
//! `interp.telemetry_capture()`.

#![cfg(feature = "telemetry")]

use ascript::run_source_with_interp;

/// Run `.as` source on the tree-walker, returning (stdout, captured requests).
async fn run(src: &str) -> (String, Vec<ascript::CapturedRequest>) {
    let (out, interp) = run_source_with_interp(src)
        .await
        .expect("program should run");
    (out, interp.telemetry_capture())
}

#[tokio::test]
async fn no_op_when_uninitialized() {
    // Without telemetry.init, every call is an inert no-op: no captured requests,
    // no error, the program runs to completion.
    let (out, caps) = run(r#"
import * as telemetry from "std/telemetry"
telemetry.startSpan("x").end()
telemetry.counter("c").add(1)
telemetry.histogram("h").record(1.5)
telemetry.gauge("g").set(7)
telemetry.capture("evt", { distinctId: "u1" })
telemetry.identify("u1", { email: "a@b.com" })
print("done")
"#)
    .await;
    assert_eq!(out, "done\n");
    assert!(caps.is_empty(), "expected no captured requests, got {:?}", caps);
}

#[tokio::test]
async fn init_returns_ok_and_activates() {
    // init with a service + one OTLP exporter succeeds ([true, nil]); afterwards
    // a span is buffered (we just assert the program runs and init returned true).
    let (out, _caps) = run(r#"
import * as telemetry from "std/telemetry"
let [ok, err] = telemetry.init({
  service: "test-app",
  exporters: [ telemetry.otlp({ endpoint: "http://localhost:4318" }) ],
})
print(ok)
print(err)
"#)
    .await;
    assert_eq!(out, "true\nnil\n");
}

#[tokio::test]
async fn init_missing_service_is_tier1() {
    let (out, _caps) = run(r#"
import * as telemetry from "std/telemetry"
let [ok, err] = telemetry.init({ exporters: [] })
print(ok)
print(err != nil)
"#)
    .await;
    assert_eq!(out, "nil\ntrue\n");
}
