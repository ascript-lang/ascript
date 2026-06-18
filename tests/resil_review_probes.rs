//! RESIL Phase-4 holistic-review probe tests (Task 4.6).
//!
//! Independent reviewer probes for the TASK_LOCALS engine seam: generator
//! resume-time deadline semantics, the worker airlock boundary, and panic-unwind
//! local restoration. These run via the wrapped `run_source`/VM entry points
//! (which DO establish `ambient_root_scope`).
//!
//! NOTE (reviewer finding, Critical): the PRODUCTION CLI runner
//! `run_file_with_packages` (src/lib.rs) does NOT wrap `load_module` in
//! `ambient_root_scope`, so on `ascript run --tree-walker <file>` a
//! `resilience.deadline` body sees `deadlineRemaining() == nil` (the deadline is
//! silently dropped — `TASK_LOCALS.try_with` errs with no scope). The VM CLI path
//! happens to work because its async spawn sites establish a scope. These probes
//! deliberately use the wrapped test entries so they document the INTENDED
//! semantics; a CLI-path regression belongs in tests/cli.rs once the runner is fixed.

#[cfg(feature = "workflow")]
mod probes {
    use ascript::run_source;

    /// §5.1: a generator body reads the RESUMER's ambient deadline (resume-time
    /// semantics — generators are not spawn-wrapped, the body is polled inside the
    /// resuming task). Driven from inside a `deadline` scope, the step sees a budget;
    /// driven from outside, it sees nil.
    #[tokio::test]
    async fn generator_step_sees_resumer_deadline() {
        let out = run_source(
            r#"
import * as resilience from "std/resilience"
fn* g() {
    yield resilience.deadlineRemaining()
    yield resilience.deadlineRemaining()
}
let it = g()
resilience.deadline(60000, () => {
    let a = it.next()
    print(a != nil)
    print(a <= 60000)
    return nil
})
let b = it.next()
print(b)
"#,
        )
        .await
        .expect("run");
        assert_eq!(out, "true\ntrue\nnil\n");
    }

    /// §5.1/§7.1: ambient locals do NOT cross the worker airlock — a `worker fn`
    /// body starts with EMPTY locals, so `deadlineRemaining()` is nil inside it even
    /// when the caller set a deadline.
    #[tokio::test]
    async fn worker_body_starts_with_empty_locals() {
        let out = run_source(
            r#"
import * as resilience from "std/resilience"
worker fn inspect(n: number): number {
    let r = resilience.deadlineRemaining()
    if (r == nil) { return -1.0 }
    return r
}
let [v, err] = resilience.deadline(60000, async () => {
    let x = await inspect(1)
    return x
})
print(v)
"#,
        )
        .await
        .expect("run");
        assert_eq!(out, "-1.0\n");
    }

    /// Task 4.1: a panicking `deadline` body restores the previous locals on the
    /// panic-unwind path (the panic is a `Control::Panic` captured as `Err`, so the
    /// `do_restore` after the race still runs).
    #[tokio::test]
    async fn deadline_local_restored_after_panic() {
        let out = run_source(
            r#"
import * as resilience from "std/resilience"
let [v, err] = recover(() => resilience.deadline(60000, () => { assert(false, "boom") }))
print(resilience.deadlineRemaining())
"#,
        )
        .await
        .expect("run");
        assert_eq!(out, "nil\n");
    }

    /// Task 4.3 twin: a panicking `withTrace` body restores the previous trace id.
    #[tokio::test]
    async fn trace_id_restored_after_panic() {
        let out = run_source(
            r#"
import * as resilience from "std/resilience"
let [v, err] = recover(() => resilience.withTrace("req-1", () => { assert(false, "boom") }))
print(resilience.traceId())
"#,
        )
        .await
        .expect("run");
        assert_eq!(out, "nil\n");
    }
}
