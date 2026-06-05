//! SP12 `std/telemetry` capture-mode tests. Run with `--features telemetry`
//! (the feature is not in `default`, so the whole file is `#[cfg]`-gated and is
//! empty/compiles-clean in the default config). No socket, no secret: telemetry
//! runs in capture mode and the recorded exporter HTTP payloads are read back via
//! `interp.telemetry_capture()`.

#![cfg(feature = "telemetry")]

use ascript::run_source_with_interp;
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
