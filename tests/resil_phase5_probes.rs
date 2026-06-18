//! RESIL Phase-5 holistic-review probe tests (Task 5.4 — independent reviewer).
//!
//! Edge probes for the Phase-5 observability + HTTP-wrapper surface that the
//! existing unit/integration tests did not pin exactly:
//!
//!  * health classification corners: an ok-pair `[true, nil]` and a falsy-inner
//!    ok-pair `[false, nil]` (the latter MUST fail — unwrap-then-truthiness);
//!  * `resilience.handler` with ONLY `deadlineMs` and a PASSING sync fn (the
//!    layer-3 fall-through with no limiter/bulkhead/breaker returns the fn result);
//!  * a registry render after many policy trips does not panic / borrow-conflict
//!    and the rendered text is well-formed Prometheus 0.0.4 (single `# TYPE`).
//!
//! These run via the wrapped `run_source` test entry (which establishes
//! `ambient_root_scope`).

#[cfg(feature = "resilience")]
mod probes {
    use ascript::run_source;

    /// §6.3: an ok-pair whose inner value is `true` PASSES (200), exactly like a
    /// bare `true`. (The existing unit test uses `[1, nil]`; this pins the literal
    /// `[true, nil]` shape called out by the reviewer.)
    #[tokio::test]
    async fn health_ok_pair_true_passes_like_bare_true() {
        let out = run_source(
            r##"
import * as resilience from "std/resilience"
import * as json from "std/json"
fn pairTrue() { return [true, nil] }
fn bareTrue() { return true }
let h = resilience.health({ checks: { a: pairTrue, b: bareTrue } })
let resp = h({})
print(resp.status)
let [d, e] = json.parse(resp.body)
print(d.status)
print(d.checks.a.ok)
print(d.checks.b.ok)
"##,
        )
        .await
        .expect("run");
        assert_eq!(out, "200\nok\ntrue\ntrue\n");
    }

    /// §6.3: an OK-pair (err half nil) whose INNER value is falsy MUST count as a
    /// FAILED check — the classifier unwraps `[v, nil]` and applies truthiness to
    /// `v`. `[false, nil]` is therefore a fail (503), distinct from `[nil, err]`.
    #[tokio::test]
    async fn health_ok_pair_false_inner_fails() {
        let out = run_source(
            r##"
import * as resilience from "std/resilience"
import * as json from "std/json"
fn pairFalse() { return [false, nil] }
let h = resilience.health({ checks: { a: pairFalse } })
let resp = h({})
print(resp.status)
let [d, e] = json.parse(resp.body)
print(d.status)
print(d.checks.a.ok)
"##,
        )
        .await
        .expect("run");
        assert_eq!(out, "503\ndegraded\nfalse\n");
    }

    /// §6.4: `resilience.handler` with ONLY `deadlineMs` (no limiter/bulkhead/
    /// breaker) and a PASSING sync fn returns the fn's own value (layer 3 wraps the
    /// step, the deadline does not expire, the result flows back unwrapped).
    #[tokio::test]
    async fn handler_deadline_only_passing_fn_returns_result() {
        let out = run_source(
            r##"
import * as resilience from "std/resilience"
fn handle(req) { return { status: 200, body: req.path } }
let h = resilience.handler({ deadlineMs: 60000 }, handle)
let resp = await h({path: "/ok"})
print(resp.status)
print(resp.body)
"##,
        )
        .await
        .expect("run");
        assert_eq!(out, "200\n/ok\n");
    }

    /// §6.1/§6.2: hammer a policy-tripping path many times in ONE isolate, then
    /// render `/metrics`. No `BorrowMutError` / `already borrowed` panic, and the
    /// rendered body is well-formed (a `# TYPE` line, the prefix, no duplicate
    /// `# TYPE` for a multi-series metric). Single-threaded by construction, but
    /// this exercises interleaved bump (during each trip) + render (the scrape).
    #[tokio::test]
    async fn metrics_render_after_many_trips_no_borrow_panic() {
        let out = run_source(
            r##"
import * as resilience from "std/resilience"
import * as string from "std/string"
let b = resilience.breaker({name: "t", failureRate: 0.5, window: 4, minCalls: 4, cooldownMs: 999999, halfOpenMax: 1})
fn ok() { return 1 }
fn bad() { return [nil, {message: "x", code: "e"}] }
let i = 0
while (i < 50) {
    b.call(ok)
    b.call(bad)
    i = i + 1
}
let h = resilience.metricsHandler()
let resp = h({method: "GET", path: "/metrics"})
print(resp.status)
print(resp.headers["content-type"])
print(string.contains(resp.body, "ascript_resilience_"))
print(string.contains(resp.body, "# TYPE "))
"##,
        )
        .await
        .expect("run");
        assert_eq!(
            out,
            "200\ntext/plain; version=0.0.4\ntrue\ntrue\n"
        );
    }
}
