//! SP12 `std/telemetry` capture-mode tests. The `telemetry` feature is in
//! `default`, so these run in the default config; the whole file is
//! `#[cfg(feature = "telemetry")]`-gated and is empty/compiles-clean only under
//! `--no-default-features` (or any build dropping the feature). No socket, no secret: telemetry
//! runs in capture mode and the recorded exporter HTTP payloads are read back via
//! `interp.telemetry_capture()`.

#![cfg(feature = "telemetry")]

use ascript::run_source_with_interp;
use ascript::value::Value;
use std::rc::Rc;

/// Run `.as` source on the tree-walker, returning (stdout, captured requests).
async fn run(src: &str) -> (String, Vec<ascript::CapturedRequest>) {
    let (out, interp) = run_source_with_interp(src)
        .await
        .expect("program should run");
    (out, interp.telemetry_capture())
}

/// Run `.as` source, returning (stdout, owning interp) so a test can read
/// `telemetry_spans_debug()` (buffered spans) or `telemetry_capture()`.
async fn run_i(src: &str) -> (String, Rc<ascript::interp::Interp>) {
    run_source_with_interp(src)
        .await
        .expect("program should run")
}

/// RESIL Gate-14 fix #1: run `.as` source on the SPECIALIZED VM, returning (stdout,
/// owning interp). Used to prove a VM-mode async-fn body's spans parent correctly —
/// the spawn-site `telemetry_scope` wrap the VM previously lacked.
async fn run_vm_i(src: &str) -> (String, Rc<ascript::interp::Interp>) {
    ascript::vm_run_source_with_interp(src)
        .await
        .expect("program should run")
}

// ---- F0: feature scaffold / no-op-until-init ----

#[tokio::test]
async fn no_op_when_uninitialized() {
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

// ---- F1: tracing — span lifecycle, scoped helper, parenting, concurrency ----

const INIT: &str = r#"
import * as telemetry from "std/telemetry"
telemetry.init({ service: "t", exporters: [ telemetry.otlp({ endpoint: "http://localhost:4318" }) ] })
"#;

#[tokio::test]
async fn span_lifecycle_buffers_one_span_with_attrs_event_status() {
    let (_out, interp) = run_i(&format!(
        r#"{INIT}
let s = telemetry.startSpan("s", {{ attributes: {{ a: 1 }} }})
s.setAttribute("user.id", "u9")
s.addEvent("cache-miss", {{ key: "k" }})
s.setStatus("ok")
s.end()
"#
    ))
    .await;
    let spans = interp.telemetry_spans_debug();
    assert_eq!(spans.len(), 1, "exactly one buffered span");
    let s = &spans[0];
    assert_eq!(s.name, "s");
    assert!(s.attributes.iter().any(|(k, _)| k == "a"), "init attr: {:?}", s.attributes);
    assert!(s.attributes.iter().any(|(k, _)| k == "user.id"), "set attr");
    assert_eq!(s.events, vec!["cache-miss".to_string()]);
    assert_eq!(s.status_code, 1, "ok = 1");
    assert!(s.parent_id.is_none(), "root span has no parent");
}

#[tokio::test]
async fn method_after_end_is_a_noop() {
    let (_out, interp) = run_i(&format!(
        r#"{INIT}
let s = telemetry.startSpan("once")
s.end()
s.setAttribute("late", 1)   // no-op
s.end()                      // no second span
"#
    ))
    .await;
    let spans = interp.telemetry_spans_debug();
    assert_eq!(spans.len(), 1, "exactly one span, no duplicate from second end()");
    assert!(!spans[0].attributes.iter().any(|(k, _)| k == "late"), "late attr ignored");
}

#[tokio::test]
async fn scoped_span_happy_path_returns_value_and_ok_status() {
    let (out, interp) = run_i(&format!(
        r#"{INIT}
let [v, e] = await telemetry.span("op", async () => {{ return 42 }})
print(v)
print(e)
"#
    ))
    .await;
    assert_eq!(out, "42\nnil\n");
    let spans = interp.telemetry_spans_debug();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "op");
    assert_eq!(spans[0].status_code, 1, "ok");
}

#[tokio::test]
async fn scoped_span_panic_becomes_error_pair_and_status() {
    let (out, interp) = run_i(&format!(
        r#"{INIT}
let [v, e] = await telemetry.span("boom", async () => {{ assert(false, "kaboom") }})
print(v)
print(e != nil)
print("after")
"#
    ))
    .await;
    assert_eq!(out, "nil\ntrue\nafter\n");
    let spans = interp.telemetry_spans_debug();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].status_code, 2, "error");
    assert_eq!(spans[0].status_message.as_deref(), Some("kaboom"));
}

#[tokio::test]
async fn nested_span_parents_to_scoped_span() {
    let (_out, interp) = run_i(&format!(
        r#"{INIT}
await telemetry.span("outer", async () => {{
  let inner = telemetry.startSpan("inner")
  inner.end()
}})
"#
    ))
    .await;
    let spans = interp.telemetry_spans_debug();
    let outer = spans.iter().find(|s| s.name == "outer").expect("outer");
    let inner = spans.iter().find(|s| s.name == "inner").expect("inner");
    assert_eq!(inner.trace_id, outer.trace_id, "same trace");
    assert_eq!(inner.parent_id.as_deref(), Some(outer.span_id.as_str()), "inner parents to outer");
}

/// RESIL Gate-14 fix #1 (VM-mode span lineage). The VM async-closure / static-async
/// spawn sites previously LACKED the `telemetry_scope` wrap the tree-walker's async
/// arms have — so a span opened INSIDE a VM-mode spawned async-fn body did NOT
/// parent to the spawning task's current span. This runs ON THE VM: the body of
/// `telemetry.span("outer", …)` calls a top-level `async fn inner_work()` (a real
/// VM spawn_local), and that spawned body opens "inner". With the wrap, "inner"
/// parents to "outer" — exactly as the tree-walker does (asserted above).
#[tokio::test]
async fn vm_async_fn_body_span_parents_to_scoped_span() {
    let (_out, interp) = run_vm_i(&format!(
        r#"{INIT}
async fn inner_work() {{
  let inner = telemetry.startSpan("inner")
  inner.end()
}}
await telemetry.span("outer", async () => {{
  await inner_work()   // a VM spawn_local; its body must inherit "outer" as current
}})
"#
    ))
    .await;
    let spans = interp.telemetry_spans_debug();
    let outer = spans.iter().find(|s| s.name == "outer").expect("outer");
    let inner = spans.iter().find(|s| s.name == "inner").expect("inner");
    assert_eq!(inner.trace_id, outer.trace_id, "same trace (VM mode)");
    assert_eq!(
        inner.parent_id.as_deref(),
        Some(outer.span_id.as_str()),
        "VM-mode inner parents to outer (Gate-14 fix #1)"
    );
}

#[tokio::test]
async fn concurrent_scoped_spans_do_not_cross_parent() {
    // Two telemetry.span calls driven on concurrent spawn_local tasks must each
    // parent THEIR child to THEIR own span — never cross (spec §9.3).
    // Adversarial interleave: A pushes its span then sleeps briefly; B pushes its
    // span and sleeps long; A WAKES FIRST and creates A-child while B is the most
    // recently started span. A naive shared "current" stack would mis-parent
    // A-child to B; correct per-task isolation parents it to A.
    let (_out, interp) = run_i(&format!(
        r#"{INIT}
import {{ sleep }} from "std/time"
import {{ spawn, gather }} from "std/task"
fn worker(tag, first, second) {{
  return telemetry.span(tag, async () => {{
    await sleep(first)
    let child = telemetry.startSpan(tag + "-child")
    await sleep(second)
    child.end()
  }})
}}
let a = spawn(async () => {{ await worker("A", 2, 20) }})
let b = spawn(async () => {{ await worker("B", 4, 4) }})
await gather([a, b])
"#
    ))
    .await;
    let spans = interp.telemetry_spans_debug();
    let a = spans.iter().find(|s| s.name == "A").expect("A");
    let a_child = spans.iter().find(|s| s.name == "A-child").expect("A-child");
    let b = spans.iter().find(|s| s.name == "B").expect("B");
    let b_child = spans.iter().find(|s| s.name == "B-child").expect("B-child");
    assert_eq!(a_child.parent_id.as_deref(), Some(a.span_id.as_str()), "A-child→A");
    assert_eq!(b_child.parent_id.as_deref(), Some(b.span_id.as_str()), "B-child→B");
    assert_eq!(a_child.trace_id, a.trace_id);
    assert_eq!(b_child.trace_id, b.trace_id);
    assert_ne!(a.trace_id, b.trace_id, "A and B are distinct traces");
}

// ---- RESIL §5.5: trace-id local attaches as a span attribute ----

/// A span opened inside a `resilience.withTrace` scope carries the trace id as a
/// `trace_id` span attribute; a span opened outside any trace scope does not.
#[tokio::test]
async fn span_carries_trace_id_attribute_from_local() {
    let (_out, interp) = run_i(&format!(
        r#"{INIT}
import * as resilience from "std/resilience"
resilience.withTrace("req-42", () => {{
  telemetry.startSpan("traced").end()
  return nil
}})
telemetry.startSpan("untraced").end()
"#
    ))
    .await;
    let spans = interp.telemetry_spans_debug();
    let traced = spans.iter().find(|s| s.name == "traced").expect("traced span");
    let untraced = spans.iter().find(|s| s.name == "untraced").expect("untraced span");
    assert!(
        traced.attributes.iter().any(|(k, v)| k == "trace_id" && v == "req-42"),
        "traced span should carry trace_id=req-42 attribute: {:?}",
        traced.attributes
    );
    assert!(
        !untraced.attributes.iter().any(|(k, _)| k == "trace_id"),
        "untraced span must NOT carry a trace_id attribute: {:?}",
        untraced.attributes
    );
}

// ---- F2: OTLP exporter — span / metric / log HTTP payloads (capture seam) ----

#[tokio::test]
async fn otlp_span_export_shape() {
    let (_out, caps) = run(&format!(
        r#"{INIT}
let s = telemetry.startSpan("handle-request", {{ attributes: {{ route: "/users" }} }})
s.setStatus("ok")
s.end()
await telemetry.flush()
"#
    ))
    .await;
    let req = caps.iter().find(|r| r.signal == "traces").expect("a traces request");
    assert_eq!(req.exporter, "otlp");
    assert!(req.url.ends_with("/v1/traces"), "url: {}", req.url);
    let body = &req.body;
    // OTLP ResourceSpans shape: resource.attributes carries service.name.
    assert!(body.contains("resourceSpans"), "{body}");
    assert!(body.contains("service.name"), "{body}");
    assert!(body.contains("\"name\":\"handle-request\""), "{body}");
    // hex trace/span ids (16/8 hex chars), NOT base64 (no '=' / '+').
    let trace = field_after(body, "traceId");
    assert_eq!(trace.len(), 32, "16-byte hex traceId: {trace}");
    assert!(trace.chars().all(|c| c.is_ascii_hexdigit()), "hex: {trace}");
    let span = field_after(body, "spanId");
    assert_eq!(span.len(), 16, "8-byte hex spanId: {span}");
    // *UnixNano are DECIMAL STRINGS.
    assert!(body.contains("\"startTimeUnixNano\":\""), "ns string: {body}");
    assert!(body.contains("\"code\":1"), "ok status: {body}");
    assert!(body.contains("\"route\""), "attr: {body}");
}

#[tokio::test]
async fn otlp_counter_is_cumulative_sum() {
    let (_out, caps) = run(&format!(
        r#"{INIT}
let reqs = telemetry.counter("http.requests", {{ unit: "1" }})
reqs.add(1, {{ route: "/x" }})
reqs.add(1, {{ route: "/x" }})
reqs.add(5, {{ route: "/y" }})
await telemetry.flush()
"#
    ))
    .await;
    let req = caps.iter().find(|r| r.signal == "metrics").expect("metrics request");
    assert!(req.url.ends_with("/v1/metrics"), "url: {}", req.url);
    let body = &req.body;
    assert!(body.contains("resourceMetrics"), "{body}");
    assert!(body.contains("\"name\":\"http.requests\""), "{body}");
    assert!(body.contains("\"sum\""), "sum metric: {body}");
    assert!(body.contains("\"aggregationTemporality\":2"), "cumulative: {body}");
    // /x accumulated to 2, /y to 5.
    assert!(body.contains("\"asDouble\":2.0") || body.contains("\"asDouble\":2"), "/x=2: {body}");
    assert!(body.contains("\"asDouble\":5.0") || body.contains("\"asDouble\":5"), "/y=5: {body}");
}

#[tokio::test]
async fn otlp_histogram_and_gauge() {
    let (_out, caps) = run(&format!(
        r#"{INIT}
let lat = telemetry.histogram("http.latency", {{ unit: "ms" }})
lat.record(12.5)
lat.record(7.5)
let inflight = telemetry.gauge("http.inflight")
inflight.set(7)
await telemetry.flush()
"#
    ))
    .await;
    let body = &caps.iter().find(|r| r.signal == "metrics").unwrap().body;
    assert!(body.contains("\"histogram\""), "histogram: {body}");
    assert!(body.contains("\"count\":\"2\""), "2 samples: {body}");
    assert!(body.contains("\"gauge\""), "gauge: {body}");
}

#[tokio::test]
async fn idempotent_instrument_registration() {
    // Re-fetching the same counter name returns the same instrument: one add of 1
    // via each handle accumulates to 2 on ONE data point.
    let (_out, caps) = run(&format!(
        r#"{INIT}
telemetry.counter("hits").add(1)
telemetry.counter("hits").add(1)
await telemetry.flush()
"#
    ))
    .await;
    let body = &caps.iter().find(|r| r.signal == "metrics").unwrap().body;
    assert_eq!(body.matches("\"name\":\"hits\"").count(), 1, "one instrument: {body}");
    assert!(body.contains("\"asDouble\":2.0") || body.contains("\"asDouble\":2"), "sum=2: {body}");
}

/// The first `"key":"<value>"` after the needle.
fn field_after(body: &str, key: &str) -> String {
    let needle = format!("\"{}\":\"", key);
    let i = body.find(&needle).unwrap_or_else(|| panic!("{key} not in {body}"));
    let after = &body[i + needle.len()..];
    let end = after.find('"').unwrap();
    after[..end].to_string()
}

// ---- F3: Sentry exporter — DSN parse, transaction + error envelopes ----

const INIT_SENTRY: &str = r#"
import * as telemetry from "std/telemetry"
let [ok, err] = telemetry.init({
  service: "t",
  exporters: [ telemetry.sentry({ dsn: "https://pub123@o9.ingest.sentry.io/42" }) ],
})
"#;

#[tokio::test]
async fn sentry_init_malformed_dsn_is_tier1() {
    let (out, _caps) = run(r#"
import * as telemetry from "std/telemetry"
let [ok, err] = telemetry.init({
  service: "t",
  exporters: [ telemetry.sentry({ dsn: "not-a-dsn" }) ],
})
print(ok)
print(err != nil)
"#)
    .await;
    assert_eq!(out, "nil\ntrue\n");
}

#[tokio::test]
async fn sentry_transaction_envelope() {
    let (_out, caps) = run(&format!(
        r#"{INIT_SENTRY}
await telemetry.span("outer", async () => {{
  let inner = telemetry.startSpan("inner")
  inner.end()
}})
await telemetry.flush()
"#
    ))
    .await;
    let req = caps.iter().find(|r| r.signal == "envelope").expect("envelope request");
    assert_eq!(req.exporter, "sentry");
    // DSN → envelope URL + auth header.
    assert_eq!(req.url, "https://o9.ingest.sentry.io/api/42/envelope/");
    assert!(
        req.headers.iter().any(|(k, v)| k == "X-Sentry-Auth" && v.contains("sentry_key=pub123")),
        "auth header: {:?}",
        req.headers
    );
    // Newline-delimited envelope: header line, item-header line, payload line.
    let lines: Vec<&str> = req.body.lines().collect();
    assert!(lines.len() >= 3, "envelope lines: {:?}", lines);
    assert!(req.body.contains(r#""type":"transaction""#), "transaction item: {}", req.body);
    assert!(req.body.contains(r#""transaction":"outer""#), "tx name: {}", req.body);
    // The inner span is embedded as a child span.
    assert!(req.body.contains(r#""op":"inner""#), "child span: {}", req.body);
}

#[tokio::test]
async fn sentry_error_status_adds_error_event() {
    let (_out, caps) = run(&format!(
        r#"{INIT_SENTRY}
await telemetry.span("boom", async () => {{ assert(false, "kaboom") }})
await telemetry.flush()
"#
    ))
    .await;
    let req = caps.iter().find(|r| r.signal == "envelope").expect("envelope request");
    // Both a transaction AND an error event item.
    assert!(req.body.contains(r#""type":"transaction""#), "tx: {}", req.body);
    assert!(req.body.contains(r#""type":"event""#), "error event: {}", req.body);
    assert!(req.body.contains(r#""level":"error""#), "error level: {}", req.body);
    assert!(req.body.contains("kaboom"), "message: {}", req.body);
}

// ---- F4: PostHog exporter — capture / identify ----

const INIT_POSTHOG: &str = r#"
import * as telemetry from "std/telemetry"
telemetry.init({
  service: "t",
  exporters: [ telemetry.posthog({ apiKey: "phc_test" }) ],
})
"#;

#[tokio::test]
async fn posthog_capture_batches_to_endpoint() {
    let (_out, caps) = run(&format!(
        r#"{INIT_POSTHOG}
telemetry.capture("signup_completed", {{ distinctId: "u1", properties: {{ plan: "pro" }} }})
await telemetry.flush()
"#
    ))
    .await;
    let req = caps.iter().find(|r| r.signal == "events").expect("events request");
    assert_eq!(req.exporter, "posthog");
    assert!(req.url.ends_with("/batch/"), "url: {}", req.url);
    let body = &req.body;
    assert!(body.contains(r#""api_key":"phc_test""#), "api_key: {body}");
    assert!(body.contains(r#""event":"signup_completed""#), "event: {body}");
    assert!(body.contains(r#""distinct_id":"u1""#), "distinct_id: {body}");
    assert!(body.contains(r#""plan":"pro""#), "props: {body}");
}

#[tokio::test]
async fn posthog_identify_sets_person_props() {
    let (_out, caps) = run(&format!(
        r#"{INIT_POSTHOG}
telemetry.identify("u1", {{ email: "a@b.com", plan: "pro" }})
await telemetry.flush()
"#
    ))
    .await;
    let body = &caps.iter().find(|r| r.signal == "events").unwrap().body;
    assert!(body.contains(r#""event":"$identify""#), "$identify: {body}");
    assert!(body.contains(r#""$set""#), "$set: {body}");
    assert!(body.contains(r#""email":"a@b.com""#), "person prop: {body}");
}

#[tokio::test]
async fn capture_is_noop_without_posthog_or_mirror() {
    // OTLP-only init, mirroring off: capture has nowhere to go → no events request.
    let (_out, caps) = run(&format!(
        r#"{INIT}
telemetry.capture("evt", {{ distinctId: "u1" }})
await telemetry.flush()
"#
    ))
    .await;
    assert!(
        caps.iter().all(|r| r.signal != "events"),
        "no events request expected: {:?}",
        caps
    );
}

#[tokio::test]
async fn mirror_events_to_otlp_emits_log_records() {
    let (_out, caps) = run(r#"
import * as telemetry from "std/telemetry"
telemetry.init({
  service: "t",
  mirrorEventsToOtlp: true,
  exporters: [ telemetry.otlp({ endpoint: "http://localhost:4318" }) ],
})
telemetry.capture("signup", { distinctId: "u1", properties: { plan: "pro" } })
await telemetry.flush()
"#)
    .await;
    let req = caps.iter().find(|r| r.signal == "logs").expect("logs request");
    assert!(req.url.ends_with("/v1/logs"), "url: {}", req.url);
    assert!(req.body.contains("resourceLogs"), "{}", req.body);
    assert!(req.body.contains(r#""stringValue":"signup""#), "event body: {}", req.body);
    assert!(req.body.contains("distinct.id"), "distinct id attr: {}", req.body);
}

// ---- F5: error model, flush-on-exit, re-init/shutdown, the SP11 hook ----

#[tokio::test]
async fn flush_failure_is_tier1_not_a_panic() {
    // A forced send failure surfaces as [nil, err] from flush; the program
    // continues (prints "after"). Telemetry failure never aborts the program.
    ascript::telemetry_test_force_send_error(true);
    let (out, _interp) = run_i(&format!(
        r#"{INIT}
telemetry.startSpan("s").end()
let [ok, err] = await telemetry.flush()
print(ok)
print(err != nil)
print("after")
"#
    ))
    .await;
    ascript::telemetry_test_force_send_error(false);
    assert_eq!(out, "nil\ntrue\nafter\n");
}

#[tokio::test]
async fn init_non_object_is_tier2_panic() {
    let r = run_source_with_interp(r#"
import * as telemetry from "std/telemetry"
telemetry.init(42)
"#)
    .await;
    assert!(r.is_err(), "init(42) should be a Tier-2 panic");
}

#[tokio::test]
async fn unknown_exporter_kind_is_tier2_panic() {
    let r = run_source_with_interp(r#"
import * as telemetry from "std/telemetry"
telemetry.init({ service: "t", exporters: [ { foo: 1 } ] })
"#)
    .await;
    assert!(r.is_err(), "a non-descriptor exporter should be a Tier-2 panic");
}

#[tokio::test]
async fn setstatus_bogus_is_tier2_panic() {
    let r = run_source_with_interp(&format!(
        r#"{INIT}
let s = telemetry.startSpan("s")
s.setStatus("bogus")
"#
    ))
    .await;
    assert!(r.is_err(), "setStatus(\"bogus\") should be a Tier-2 panic");
}

#[tokio::test]
async fn reinit_replaces_pipeline_flushing_old() {
    // The first pipeline buffers a span; re-init flushes it (one traces request)
    // then installs the new pipeline.
    let (_out, caps) = run(&format!(
        r#"{INIT}
telemetry.startSpan("old").end()
telemetry.init({{ service: "t2", exporters: [ telemetry.otlp({{ endpoint: "http://localhost:4318" }}) ] }})
telemetry.startSpan("new").end()
await telemetry.flush()
"#
    ))
    .await;
    let traces: Vec<_> = caps.iter().filter(|r| r.signal == "traces").collect();
    // One flush from re-init (carrying "old") + one explicit flush (carrying "new").
    assert_eq!(traces.len(), 2, "re-init flush + explicit flush: {:?}", caps);
    assert!(traces[0].body.contains("\"name\":\"old\""), "first flush has old span");
    assert!(traces[1].body.contains("\"name\":\"new\""), "second flush has new span");
}

#[tokio::test]
async fn shutdown_returns_to_noop() {
    let (_out, caps) = run(&format!(
        r#"{INIT}
telemetry.startSpan("before").end()
await telemetry.shutdown()
telemetry.startSpan("after").end()   // no-op (pipeline torn down)
await telemetry.flush()              // no-op
"#
    ))
    .await;
    let traces: Vec<_> = caps.iter().filter(|r| r.signal == "traces").collect();
    // shutdown flushed "before"; nothing after.
    assert_eq!(traces.len(), 1, "only shutdown's flush: {:?}", caps);
    assert!(traces[0].body.contains("\"name\":\"before\""));
    assert!(!traces[0].body.contains("\"name\":\"after\""));
}

#[tokio::test]
async fn flush_on_exit_emits_unflushed_spans() {
    // A program that creates spans but never calls flush(): the exit-path flush
    // exports them (the SP11 GenAI-span use case — spans recorded by the library,
    // flushed automatically at process end).
    let (_out, interp) = run_i(&format!(
        r#"{INIT}
telemetry.startSpan("auto").end()
"#
    ))
    .await;
    // No flush yet: the span is buffered, nothing captured.
    assert!(interp.telemetry_capture().is_empty());
    interp.telemetry_flush_on_exit().await;
    let traces: Vec<_> = interp.telemetry_capture().into_iter().filter(|r| r.signal == "traces").collect();
    assert_eq!(traces.len(), 1, "flush-on-exit exports the buffered span");
    assert!(traces[0].body.contains("\"name\":\"auto\""));
}

#[tokio::test]
async fn sp11_soft_hook_contract() {
    // The EXACT surface std/ai (SP11) will call: telemetry_span_start → _set →
    // _event → _end produces a captured OTLP span with the attributes; with
    // telemetry uninitialized the hook is inert.
    let (_out, interp) = run_i(INIT).await;
    assert!(interp.telemetry_active());
    let id = interp
        .telemetry_span_start(
            "chat openai:gpt-4.1",
            vec![("gen_ai.system".into(), Value::str("openai"))],
        )
        .expect("active → Some span id");
    interp.telemetry_span_set(id, "gen_ai.request.model", Value::str("gpt-4.1"));
    interp.telemetry_span_event(id, "first-token", vec![]);
    interp.telemetry_span_end(id, ascript::interp::SpanStatus::Ok);
    interp.telemetry_flush_on_exit().await;
    let body = &interp.telemetry_capture().into_iter().find(|r| r.signal == "traces").unwrap().body;
    assert!(body.contains("chat openai:gpt-4.1"), "span name: {body}");
    assert!(body.contains("gen_ai.system"), "attr: {body}");
    assert!(body.contains("gen_ai.request.model"), "set attr: {body}");
    assert!(body.contains("\"code\":1"), "ok status: {body}");

    // Uninitialized interp: hook inert.
    let fresh = ascript::interp::Interp::new();
    assert!(!fresh.telemetry_active());
    assert!(fresh.telemetry_span_start("x", vec![]).is_none());
}
