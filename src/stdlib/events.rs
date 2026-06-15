//! `std/events` — an event-emitter / pub-sub (core, NOT feature-gated).
//!
//! `events.new()` returns a `Value::Native` handle backed by
//! `ResourceState::Events`. Methods:
//!
//! - `on(event, fn)` — register a listener.
//! - `once(event, fn)` — register a one-shot listener (removed after it fires).
//! - `off(event, fn?)` — remove a specific listener (by identity) or, with no fn,
//!   all listeners for `event`. Returns the number removed.
//! - `await emit(event, ...args)` — call each listener for `event` in registration
//!   order, awaiting each (a listener may be an `async fn`). Returns the count of
//!   listeners invoked. A listener panic propagates as a Tier-2 panic.
//! - `listenerCount(event) -> number`.
//!
//! ## Borrow / await discipline
//! `emit` snapshots the matching listeners (cloned out) under a short borrow, and
//! removes any `once` listeners, BEFORE awaiting — no `resources`/`RefCell` borrow
//! is held across the `call_value` await.

use super::{arg, bi};
use crate::error::AsError;
use crate::interp::{Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value, ValueKind};
use indexmap::IndexMap;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("new", bi("events.new"))]
}

/// One registered listener.
pub struct Listener {
    pub event: String,
    pub callback: Value,
    pub once: bool,
}

/// The backing state for an emitter resource: listeners in registration order.
pub struct EventsState {
    pub listeners: Vec<Listener>,
}

impl EventsState {
    fn new() -> Self {
        EventsState {
            listeners: Vec::new(),
        }
    }
}

fn is_callable(v: &Value) -> bool {
    matches!(
        v.kind(),
        ValueKind::Function(_) | ValueKind::Builtin(_) | ValueKind::Closure(_)
    )
}

impl Interp {
    /// `std/events` module dispatch (only `new`).
    pub(crate) fn call_events_new(
        &self,
        func: &str,
        _args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "new" => {
                let handle = self.register_resource(
                    NativeKind::Events,
                    IndexMap::new(),
                    ResourceState::Events(Box::new(EventsState::new())),
                );
                Ok(handle)
            }
            _ => Err(AsError::at(format!("std/events has no function '{}'", func), span).into()),
        }
    }

    /// Dispatch a method on an emitter handle. `emit` is async; the rest are sync.
    pub(crate) async fn call_events_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "on" | "once" => {
                let event = want_event(&arg(&args, 0), span, &m.method)?;
                let cb = arg(&args, 1);
                if !is_callable(&cb) {
                    return Err(AsError::at(
                        format!("events.{}: listener must be a function", m.method),
                        span,
                    )
                    .into());
                }
                let once = m.method == "once";
                self.with_resource_mut(id, |r| {
                    if let Some(ResourceState::Events(s)) = r {
                        s.listeners.push(Listener {
                            event,
                            callback: cb,
                            once,
                        });
                    }
                });
                Ok(Value::nil())
            }
            "off" => {
                let event = want_event(&arg(&args, 0), span, "off")?;
                let target = args.get(1).cloned();
                let removed = self.with_resource_mut(id, |r| match r {
                    Some(ResourceState::Events(s)) => {
                        let before = s.listeners.len();
                        s.listeners.retain(|l| {
                            if l.event != event {
                                return true;
                            }
                            match &target {
                                // No fn given → remove all for this event.
                                None => false,
                                Some(t) if matches!(t.kind(), ValueKind::Nil) => false,
                                // Remove the listener that is identity-equal to `target`.
                                Some(t) => l.callback != *t,
                            }
                        });
                        before - s.listeners.len()
                    }
                    _ => 0,
                });
                // NUM §4: a count of removed listeners is an `int`.
                Ok(Value::int(removed as i64))
            }
            "listenerCount" => {
                let event = want_event(&arg(&args, 0), span, "listenerCount")?;
                let n = self.with_resource(id, |r| match r {
                    Some(ResourceState::Events(s)) => {
                        s.listeners.iter().filter(|l| l.event == event).count()
                    }
                    _ => 0,
                });
                // NUM §4: a listener count is an `int`.
                Ok(Value::int(n as i64))
            }
            "emit" => {
                let event = want_event(&arg(&args, 0), span, "emit")?;
                let call_args: Vec<Value> = args.iter().skip(1).cloned().collect();
                // Snapshot the matching listeners (in registration order) and remove
                // the `once` ones BEFORE awaiting — no borrow held across the await.
                let to_call: Vec<Value> = self.with_resource_mut(id, |r| match r {
                    Some(ResourceState::Events(s)) => {
                        let matched: Vec<Value> = s
                            .listeners
                            .iter()
                            .filter(|l| l.event == event)
                            .map(|l| l.callback.clone())
                            .collect();
                        // Drop the one-shot listeners for this event.
                        s.listeners.retain(|l| !(l.event == event && l.once));
                        matched
                    }
                    _ => Vec::new(),
                });
                let count = to_call.len();
                for cb in to_call {
                    // A listener panic propagates as a Tier-2 panic (documented).
                    let result = self.call_value(cb, call_args.clone(), span).await?;
                    // An `async fn` listener returns a Future; drive it to
                    // completion so listeners are awaited in registration order.
                    if let crate::value::OwnedKind::Future(f) = result.into_kind() {
                        f.get().await?;
                    }
                }
                // NUM §4: a count of invoked listeners is an `int`.
                Ok(Value::int(count as i64))
            }
            other => Err(AsError::at(format!("emitter has no method '{}'", other), span).into()),
        }
    }
}

/// The event name must be a string.
fn want_event(v: &Value, span: Span, ctx: &str) -> Result<String, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(s.to_string()),
        _ => Err(AsError::at(
            format!(
                "events.{}: event name must be a string, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    #[tokio::test]
    async fn on_emit_calls_in_order_with_args() {
        let out = run(r#"
import { new } from "std/events"
let e = new()
e.on("hi", (name) => print("a:" + name))
e.on("hi", (name) => print("b:" + name))
let n = await e.emit("hi", "ada")
print(n)
"#)
        .await;
        assert_eq!(out, "a:ada\nb:ada\n2\n");
    }

    #[tokio::test]
    async fn once_fires_exactly_once() {
        let out = run(r#"
import { new } from "std/events"
let e = new()
e.once("ping", () => print("pong"))
await e.emit("ping")
await e.emit("ping")   // listener already removed
print(e.listenerCount("ping"))
"#)
        .await;
        assert_eq!(out, "pong\n0\n");
    }

    #[tokio::test]
    async fn off_removes_listener() {
        let out = run(r#"
import { new } from "std/events"
let e = new()
fn handler() { print("fired") }
e.on("x", handler)
print(e.listenerCount("x"))   // 1
print(e.off("x", handler))    // 1 removed
print(e.listenerCount("x"))   // 0
await e.emit("x")             // nothing fires
"#)
        .await;
        assert_eq!(out, "1\n1\n0\n");
    }

    #[tokio::test]
    async fn off_all_for_event() {
        let out = run(r#"
import { new } from "std/events"
let e = new()
e.on("y", () => print("1"))
e.on("y", () => print("2"))
print(e.off("y"))             // 2 removed (no fn → all)
print(e.listenerCount("y"))   // 0
"#)
        .await;
        assert_eq!(out, "2\n0\n");
    }

    #[tokio::test]
    async fn async_listeners_are_awaited() {
        let out = run(r#"
import { new } from "std/events"
import { sleep } from "std/time"
let e = new()
e.on("go", async (x) => {
  await sleep(1)
  print("async:" + x)
})
await e.emit("go", "z")
print("after")
"#)
        .await;
        assert_eq!(out, "async:z\nafter\n");
    }
}
