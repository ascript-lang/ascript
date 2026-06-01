//! `std/sync` — FIFO channels and semaphores for coordinating between tasks.
//!
//! NOT feature-gated: tokio (with `sync`) is core infrastructure already present
//! under `--no-default-features`.
//!
//! API:
//! - `sync.channel(capacity?)` → channel handle
//!   - `capacity` omitted or 0 → **unbounded** (send never blocks)
//!   - `capacity > 0`         → **bounded** (send awaits when the queue is full)
//! - `sync.send(ch, v)   → [true, nil] | [false, err]`  — async; awaits on full bounded
//! - `sync.recv(ch)      → value | nil`                  — async; nil when closed+drained
//! - `sync.tryRecv(ch)   → [value, true] | [nil, false]` — non-blocking.
//!   `[nil, false]` cannot distinguish empty-open from closed-drained.
//! - `sync.close(ch)`    → nil                           — close the sending side
//!
//! - `sync.semaphore(permits)` → semaphore handle (permits must be a positive integer)
//! - `sync.acquire(s)`   → nil  — async; awaits until a permit is available, takes one
//! - `sync.release(s)`   → nil  — return one permit to the semaphore
//! - `sync.withPermit(s, fn)` → value — async; acquire → await fn() → release on ALL
//!   paths (including fn panics); returns fn's result, re-raises its panic after release
//! - `sync.available(s)` → number — current free permits
//!
//! **Backing:** `VecDeque<Value>` + two `Rc<tokio::sync::Notify>` (not_empty /
//! not_full). Using `tokio::sync::mpsc` would require `T: Send`, which `Value`
//! cannot satisfy (it uses `Rc` internally). The `Rc`-based design is safe because
//! the single-thread runtime (`current_thread` / `LocalSet`) guarantees no data
//! races.
//!
//! **Semaphore backing:** `RefCell<usize>` (available count) + `Rc<tokio::sync::Notify>`.
//! acquire loops: enable() notified() future BEFORE re-checking count (same lost-wakeup-
//! safe pattern as the channel WaitEmpty loop). No RefCell borrow is held across .await.
//!
//! **Borrow discipline:** The `Notify` handles are stored as separate `Rc<Notify>`
//! fields *outside* the `RefCell`-guarded state, so we can clone them before
//! releasing the borrow and then await outside any borrow — no unsafe required.

use super::{arg, bi, want_number};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

// ── Semaphore data structures ─────────────────────────────────────────────────

/// A counting semaphore: an available-permits counter behind a `RefCell` plus an
/// `Rc<Notify>` for wakeups (stored *outside* the `RefCell` so it can be cloned
/// and awaited without holding a borrow — identical discipline to `Channel`).
pub struct Semaphore {
    /// Current free permits (>= 0, <= `max`).
    available: Rc<RefCell<usize>>,
    /// Fires when a permit is released — wakes parked `acquire` callers.
    permit_available: Rc<tokio::sync::Notify>,
    /// The initial permit count. `release` is capped at this so the pool can never
    /// inflate past its declared size (a `release` with no matching `acquire` is a
    /// no-op rather than silently growing the concurrency limit).
    max: usize,
}

impl Semaphore {
    fn new(permits: usize) -> Self {
        Semaphore {
            available: Rc::new(RefCell::new(permits)),
            permit_available: Rc::new(tokio::sync::Notify::new()),
            max: permits,
        }
    }
}

// ── Channel data structures ───────────────────────────────────────────────────

/// The queue and metadata for a channel (inside a `RefCell` so mutation is shared).
struct ChannelQueue {
    /// Buffered values waiting to be received (FIFO).
    queue: VecDeque<Value>,
    /// Maximum queue depth. `0` means unbounded.
    capacity: usize,
    /// `true` after `sync.close(ch)` — no more sends.
    closed: bool,
}

/// The complete channel handle: a ref-counted queue plus two `Rc<Notify>` for
/// wakeups.  The `Notify`s live *outside* the `RefCell` so they can be cloned
/// (and awaited) without holding a borrow on the queue.
pub struct Channel {
    queue: Rc<RefCell<ChannelQueue>>,
    /// Fires when a value is pushed — wakes parked `recv` callers.
    not_empty: Rc<tokio::sync::Notify>,
    /// Fires when a value is popped — wakes parked bounded `send` callers.
    not_full: Rc<tokio::sync::Notify>,
}

impl Channel {
    fn new(capacity: usize) -> Self {
        Channel {
            queue: Rc::new(RefCell::new(ChannelQueue {
                queue: VecDeque::new(),
                capacity,
                closed: false,
            })),
            not_empty: Rc::new(tokio::sync::Notify::new()),
            not_full: Rc::new(tokio::sync::Notify::new()),
        }
    }
}

// ── exports ───────────────────────────────────────────────────────────────────

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("channel", bi("sync.channel")),
        ("send", bi("sync.send")),
        ("recv", bi("sync.recv")),
        ("tryRecv", bi("sync.tryRecv")),
        ("close", bi("sync.close")),
        ("semaphore", bi("sync.semaphore")),
        ("acquire", bi("sync.acquire")),
        ("release", bi("sync.release")),
        ("withPermit", bi("sync.withPermit")),
        ("available", bi("sync.available")),
    ]
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn ok_send() -> Value {
    make_pair(Value::Bool(true), Value::Nil)
}

fn err_closed() -> Value {
    make_pair(
        Value::Bool(false),
        make_error(Value::Str("sync.send: channel is closed".into())),
    )
}

/// Extract the `Channel` from the resource table by cloning both `Rc`s.
/// Returns `None` when the id is absent or maps to a non-Channel resource.
fn get_channel(interp: &Interp, id: u64) -> Option<Channel> {
    interp.with_resource(id, |r| match r {
        Some(ResourceState::Channel(ch)) => Some(Channel {
            queue: ch.queue.clone(),
            not_empty: ch.not_empty.clone(),
            not_full: ch.not_full.clone(),
        }),
        _ => None,
    })
}

/// Extract the `Semaphore` from the resource table by cloning both `Rc`s.
/// Returns `None` when the id is absent or maps to a non-Semaphore resource.
fn get_semaphore(interp: &Interp, id: u64) -> Option<Semaphore> {
    interp.with_resource(id, |r| match r {
        Some(ResourceState::Semaphore(s)) => Some(Semaphore {
            available: s.available.clone(),
            permit_available: s.permit_available.clone(),
            max: s.max,
        }),
        _ => None,
    })
}

// ── Interp impl ───────────────────────────────────────────────────────────────

impl Interp {
    /// Module-level dispatch for `std/sync`.
    pub(crate) async fn call_sync(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "channel" => self.sync_channel(args, span),
            "send" => self.sync_send(args, span).await,
            "recv" => self.sync_recv(args, span).await,
            "tryRecv" => self.sync_try_recv(args, span),
            "close" => self.sync_close(args, span),
            "semaphore" => self.sync_semaphore(args, span),
            "acquire" => self.sync_acquire(args, span).await,
            "release" => self.sync_release(args, span),
            "withPermit" => self.sync_with_permit(args, span).await,
            "available" => self.sync_available(args, span),
            _ => Err(AsError::at(format!("std/sync has no function '{}'", func), span).into()),
        }
    }

    // ── sync.channel ─────────────────────────────────────────────────────────

    fn sync_channel(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let capacity = match arg(args, 0) {
            Value::Nil => 0usize, // omitted → unbounded
            v => {
                let n = want_number(&v, span, "sync.channel capacity")?;
                if n < 0.0 || n.fract() != 0.0 {
                    return Err(AsError::at(
                        "sync.channel capacity must be a non-negative integer",
                        span,
                    )
                    .into());
                }
                n as usize
            }
        };
        let ch = Channel::new(capacity);
        let handle = self.register_resource(
            NativeKind::Channel,
            indexmap::IndexMap::new(),
            ResourceState::Channel(ch),
        );
        Ok(handle)
    }

    // ── sync.send ────────────────────────────────────────────────────────────

    async fn sync_send(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let (id, v) = require_channel_and_value(args, span, "sync.send")?;

        loop {
            // Snapshot the notifies *before* borrow (so the Rc outlives the borrow).
            let ch = match get_channel(self, id) {
                Some(c) => c,
                None => {
                    return Err(
                        AsError::at("sync.send: first argument is not a channel", span).into(),
                    );
                }
            };

            // Inspect state under a short borrow.
            enum Action {
                WaitFull,
                SendOk,
                Closed,
            }
            let action = {
                let inner = ch.queue.borrow();
                if inner.closed {
                    Action::Closed
                } else if inner.capacity > 0 && inner.queue.len() >= inner.capacity {
                    Action::WaitFull
                } else {
                    Action::SendOk
                }
            }; // borrow released here

            match action {
                Action::Closed => return Ok(err_closed()),
                Action::WaitFull => {
                    // Park until a consumer pops a value (or the channel closes).
                    //
                    // Lost-wakeup avoidance (mirrors `task::ResultCell::get`): a
                    // `Notify` only registers a waiter when the `notified()` future
                    // is first polled. We must therefore create + `enable()` the
                    // future (registering the waiter NOW) *before* re-checking the
                    // queue, so any `notify_one()` issued by a recv between our
                    // check and our await is captured rather than dropped.
                    let notified = ch.not_full.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable(); // register the waiter before re-check
                    {
                        // Short synchronous borrow — no .await held across it.
                        let inner = ch.queue.borrow();
                        let still_full =
                            inner.capacity > 0 && inner.queue.len() >= inner.capacity;
                        if inner.closed || !still_full {
                            // State changed under us: loop to re-handle without parking.
                            continue;
                        }
                    }
                    // Any notify_one() after enable() is now guaranteed observed.
                    notified.await;
                    // Re-loop to recheck state (competing senders / spurious wakeup).
                }
                Action::SendOk => {
                    // Push the value and wake a waiting recv.
                    {
                        let mut inner = ch.queue.borrow_mut();
                        // Re-check under the lock in case of a concurrent close.
                        if inner.closed {
                            return Ok(err_closed());
                        }
                        inner.queue.push_back(v);
                    }
                    ch.not_empty.notify_one();
                    return Ok(ok_send());
                }
            }
        }
    }

    // ── sync.recv ────────────────────────────────────────────────────────────

    async fn sync_recv(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_channel_id(&arg(args, 0), span, "sync.recv")?;

        loop {
            let ch = match get_channel(self, id) {
                Some(c) => c,
                None => {
                    return Err(
                        AsError::at("sync.recv: first argument is not a channel", span).into(),
                    );
                }
            };

            enum Action {
                Value(Value),
                WaitEmpty,
                Closed,
            }
            let action = {
                let mut inner = ch.queue.borrow_mut();
                if let Some(v) = inner.queue.pop_front() {
                    Action::Value(v)
                } else if inner.closed {
                    Action::Closed
                } else {
                    Action::WaitEmpty
                }
            }; // borrow released

            match action {
                Action::Value(v) => {
                    // Notify a blocked bounded sender that there's space.
                    ch.not_full.notify_one();
                    return Ok(v);
                }
                Action::Closed => return Ok(Value::Nil),
                Action::WaitEmpty => {
                    // Park until a value is pushed (or the channel closes).
                    //
                    // Lost-wakeup avoidance (mirrors `task::ResultCell::get`): a
                    // `Notify` only registers a waiter when its `notified()` future
                    // is first polled. We create + `enable()` the future
                    // (registering the waiter NOW) *before* re-checking the queue,
                    // so a `notify_one()` issued by a send between our check and our
                    // await is captured rather than dropped.
                    let notified = ch.not_empty.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable(); // register the waiter before re-check
                    {
                        // Short synchronous borrow — no .await held across it.
                        let inner = ch.queue.borrow();
                        if !inner.queue.is_empty() || inner.closed {
                            // State changed under us: loop to pop / observe close.
                            continue;
                        }
                    }
                    // Any notify_one() after enable() is now guaranteed observed.
                    notified.await;
                    // Re-loop to recheck.
                }
            }
        }
    }

    // ── sync.tryRecv ─────────────────────────────────────────────────────────

    /// Non-blocking receive. NOTE: `[nil, false]` means "no value right now" — it
    /// does NOT distinguish an empty-but-open channel from a closed-and-drained
    /// one. Use a blocking `recv` (which returns `nil` only on closed+drained) when
    /// that distinction matters.
    fn sync_try_recv(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_channel_id(&arg(args, 0), span, "sync.tryRecv")?;

        match get_channel(self, id) {
            None => {
                Err(AsError::at("sync.tryRecv: first argument is not a channel", span).into())
            }
            Some(ch) => {
                let mut inner = ch.queue.borrow_mut();
                match inner.queue.pop_front() {
                    Some(v) => {
                        drop(inner);
                        ch.not_full.notify_one();
                        Ok(make_pair(v, Value::Bool(true)))
                    }
                    None => Ok(make_pair(Value::Nil, Value::Bool(false))),
                }
            }
        }
    }

    // ── sync.close ───────────────────────────────────────────────────────────

    fn sync_close(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_channel_id(&arg(args, 0), span, "sync.close")?;

        match get_channel(self, id) {
            None => Err(AsError::at("sync.close: first argument is not a channel", span).into()),
            Some(ch) => {
                {
                    let mut inner = ch.queue.borrow_mut();
                    inner.closed = true;
                }
                // Wake all parked recvs (they will observe `closed=true`).
                ch.not_empty.notify_waiters();
                // Wake all parked bounded senders (they will see `closed=true`).
                ch.not_full.notify_waiters();
                Ok(Value::Nil)
            }
        }
    }

    // ── sync.semaphore ───────────────────────────────────────────────────────

    fn sync_semaphore(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let n = want_number(&arg(args, 0), span, "sync.semaphore permits")?;
        if n < 1.0 || n.fract() != 0.0 {
            return Err(AsError::at(
                "sync.semaphore: permits must be a positive integer",
                span,
            )
            .into());
        }
        let sem = Semaphore::new(n as usize);
        let handle = self.register_resource(
            NativeKind::Semaphore,
            indexmap::IndexMap::new(),
            ResourceState::Semaphore(sem),
        );
        Ok(handle)
    }

    // ── sync.acquire ─────────────────────────────────────────────────────────

    async fn sync_acquire(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_semaphore_id(&arg(args, 0), span, "sync.acquire")?;

        loop {
            // Clone the Rc<Notify> out before the borrow so we can await later
            // without holding any RefCell borrow.
            let sem = match get_semaphore(self, id) {
                Some(s) => s,
                None => {
                    return Err(
                        AsError::at("sync.acquire: first argument is not a semaphore", span)
                            .into(),
                    );
                }
            };

            // Check (and decrement) under a short borrow.
            {
                let mut avail = sem.available.borrow_mut();
                if *avail > 0 {
                    *avail -= 1;
                    return Ok(Value::Nil);
                }
            } // borrow released

            // No permits available — park until one is released.
            //
            // Lost-wakeup avoidance (mirrors channel WaitEmpty): create + enable()
            // the notified() future (registering the waiter NOW) *before* the
            // re-check, so any release() + notify_one() that races between our check
            // and our await is captured rather than dropped.
            let notified = sem.permit_available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable(); // register waiter before re-check

            {
                // Short synchronous borrow — no .await held across it.
                let avail = sem.available.borrow();
                if *avail > 0 {
                    // State changed under us: loop to decrement without parking.
                    continue;
                }
            }
            // Any notify_one() after enable() is now guaranteed observed.
            notified.await;
            // Re-loop to recheck (competing acquirers / spurious wakeup).
        }
    }

    // ── sync.release ─────────────────────────────────────────────────────────

    fn sync_release(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_semaphore_id(&arg(args, 0), span, "sync.release")?;

        match get_semaphore(self, id) {
            None => {
                Err(AsError::at("sync.release: first argument is not a semaphore", span).into())
            }
            Some(sem) => {
                let grew = {
                    let mut avail = sem.available.borrow_mut();
                    // Cap at the initial permit count: a release with no matching
                    // acquire is a no-op, never inflating the concurrency limit.
                    if *avail < sem.max {
                        *avail += 1;
                        true
                    } else {
                        false
                    }
                };
                // Wake one parked acquire (only if a permit actually became free).
                if grew {
                    sem.permit_available.notify_one();
                }
                Ok(Value::Nil)
            }
        }
    }

    // ── sync.withPermit ───────────────────────────────────────────────────────

    async fn sync_with_permit(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        // Validate that arg[0] is a semaphore (Tier-2 panic if not).
        let sem_val = arg(args, 0);
        require_semaphore_id(&sem_val, span, "sync.withPermit")?;
        let func = arg(args, 1);

        // Acquire one permit (async, may park).
        self.sync_acquire(std::slice::from_ref(&sem_val), span).await?;

        // Call fn() and capture the result — OK or Control (Panic/Propagate).
        let call_result = self.call_value(func, vec![], span).await;

        // Release the permit on ALL paths before returning or re-raising.
        // (sync, never fails — only increments the counter and notify_one)
        let _ = self.sync_release(std::slice::from_ref(&sem_val), span);

        call_result
    }

    // ── sync.available ────────────────────────────────────────────────────────

    fn sync_available(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_semaphore_id(&arg(args, 0), span, "sync.available")?;

        match get_semaphore(self, id) {
            None => {
                Err(AsError::at("sync.available: first argument is not a semaphore", span).into())
            }
            Some(sem) => {
                let count = *sem.available.borrow() as f64;
                Ok(Value::Number(count))
            }
        }
    }
}

// ── argument helpers ──────────────────────────────────────────────────────────

/// Extract the handle `id` from a `Value::Native(Channel)` argument.
fn require_channel_id(v: &Value, span: Span, ctx: &str) -> Result<u64, Control> {
    match v {
        Value::Native(obj) if obj.kind == NativeKind::Channel => Ok(obj.id),
        other => Err(AsError::at(
            format!(
                "{} expects a channel, got {}",
                ctx,
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

/// Extract the handle `id` from a `Value::Native(Semaphore)` argument.
fn require_semaphore_id(v: &Value, span: Span, ctx: &str) -> Result<u64, Control> {
    match v {
        Value::Native(obj) if obj.kind == NativeKind::Semaphore => Ok(obj.id),
        other => Err(AsError::at(
            format!(
                "{} expects a semaphore, got {}",
                ctx,
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

/// Extract (channel_id, value_to_send) from the first two args to `sync.send`.
fn require_channel_and_value(
    args: &[Value],
    span: Span,
    ctx: &str,
) -> Result<(u64, Value), Control> {
    let id = require_channel_id(&arg(args, 0), span, ctx)?;
    let v = arg(args, 1);
    Ok((id, v))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    // ── unbounded channel: basic FIFO ─────────────────────────────────────────

    #[tokio::test]
    async fn unbounded_fifo() {
        let out = run(r#"
import { channel, send, recv } from "std/sync"
let ch = channel()
await send(ch, 1)
await send(ch, 2)
await send(ch, 3)
print(await recv(ch))
print(await recv(ch))
print(await recv(ch))
"#)
        .await;
        assert_eq!(out, "1\n2\n3\n");
    }

    // ── close then recv on drained channel → nil ──────────────────────────────

    #[tokio::test]
    async fn close_then_recv_nil() {
        let out = run(r#"
import { channel, send, recv, close } from "std/sync"
let ch = channel()
await send(ch, 42)
close(ch)
print(await recv(ch))
print(await recv(ch))
"#)
        .await;
        assert_eq!(out, "42\nnil\n");
    }

    // ── send to closed channel → [false, err] ────────────────────────────────

    #[tokio::test]
    async fn send_to_closed_is_error() {
        let out = run(r#"
import { channel, send, close } from "std/sync"
let ch = channel()
close(ch)
let [ok, err] = await send(ch, 99)
print(ok)
print(type(err))
"#)
        .await;
        assert_eq!(out, "false\nobject\n");
    }

    // ── tryRecv: empty → [nil, false]; after send → [v, true] ────────────────

    #[tokio::test]
    async fn try_recv() {
        let out = run(r#"
import { channel, send, tryRecv } from "std/sync"
let ch = channel()
let [v1, ok1] = tryRecv(ch)
print(v1)
print(ok1)
await send(ch, "hello")
let [v2, ok2] = tryRecv(ch)
print(v2)
print(ok2)
"#)
        .await;
        assert_eq!(out, "nil\nfalse\nhello\ntrue\n");
    }

    // ── bounded(1) backpressure ───────────────────────────────────────────────
    // A producer sends 2 values into a cap-1 channel.  The 2nd send blocks until
    // the consumer does a recv.  We prove ordering via an unbounded "signal"
    // channel: the producer writes "done" to the signal channel AFTER the 2nd
    // data send.  On the consumer side we first recv the data (unblocking the
    // producer), then read from the signal channel — which must already contain
    // "done" because the producer finished before we checked.

    #[tokio::test]
    async fn bounded_backpressure() {
        let out = run(r#"
import { channel, send, recv } from "std/sync"
import { spawn } from "std/task"

let data = channel(1)
let sig  = channel()

async fn producer() {
    await send(data, "first")
    await send(data, "second")
    await send(sig, "done")
}

let handle = spawn(producer())

let first  = await recv(data)
let signal = await recv(sig)
let second = await recv(data)

print(first)
print(second)
print(signal)
await handle
"#)
        .await;
        assert_eq!(out, "first\nsecond\ndone\n");
    }

    // ── recv parks BEFORE the producer is spawned ─────────────────────────────
    // This is the lost-wakeup regression test: the consumer reaches `recv` and
    // parks on an empty channel; only THEN is the producer spawned and the value
    // sent.  Because `recv` registers its `Notify` waiter (via `.enable()`) before
    // its final state re-check, the producer's `notify_one()` is captured and the
    // consumer wakes.  With the old (buggy) "drop borrow, then poll notified()"
    // ordering this would deadlock and the test would hang.

    #[tokio::test]
    async fn recv_parks_then_late_producer_wakes_it() {
        let out = run(r#"
import { channel, send, recv } from "std/sync"
import { spawn } from "std/task"

let ch = channel()

// Spawn a consumer that parks on the (empty) channel immediately.
let consumer = spawn(async () => {
    let v = await recv(ch)
    print(v)
})

// Spawn the producer AFTER the consumer; it must wake the parked recv.
let producer = spawn(async () => {
    await send(ch, "woke")
})

await consumer
await producer
"#)
        .await;
        assert_eq!(out, "woke\n");
    }

    // ── many interleaved producers/consumers across spawned tasks ─────────────
    // N senders each push one value into a cap-1 (bounded, heavily contended)
    // channel; N receivers each pull one.  All 2N tasks run concurrently via
    // gather.  We don't assert ordering (concurrent), only that every value
    // arrives — a single dropped wakeup would deadlock and hang the test.
    // The received values are summed (order-independent) and compared to the
    // known total, proving none were lost or duplicated.

    #[tokio::test]
    async fn many_interleaved_tasks_no_lost_values() {
        let out = run(r#"
import { channel, send, recv } from "std/sync"
import { spawn, gather } from "std/task"
import * as array from "std/array"

let ch = channel(1)        // tight bound → maximal send backpressure
let results = channel()    // unbounded sink for received values

async fn producer(n) {
    await send(ch, n)
}
async fn consumer() {
    let v = await recv(ch)
    await send(results, v)
}

let tasks = []
let i = 0
while (i < 20) {
    array.push(tasks, spawn(producer(i + 1)))   // values 1..=20
    array.push(tasks, spawn(consumer()))
    i = i + 1
}
await gather(tasks)

// Drain the 20 received values and sum them.
let total = 0
let k = 0
while (k < 20) {
    let v = await recv(results)
    total = total + v
    k = k + 1
}
print(total)   // 1+2+...+20 = 210
"#)
        .await;
        assert_eq!(out, "210\n");
    }

    // ── tryRecv after close + drain → [nil, false] ────────────────────────────
    // tryRecv never blocks and cannot distinguish empty-open from closed-drained:
    // both yield [nil, false].

    #[tokio::test]
    async fn try_recv_after_close_and_drain() {
        let out = run(r#"
import { channel, send, tryRecv, close } from "std/sync"
let ch = channel()
await send(ch, 7)
close(ch)
let [a, oka] = tryRecv(ch)   // drains the buffered 7
print(a)
print(oka)
let [b, okb] = tryRecv(ch)   // closed + drained → [nil, false]
print(b)
print(okb)
"#)
        .await;
        assert_eq!(out, "7\ntrue\nnil\nfalse\n");
    }

    // ── non-channel arg to send → Tier-2 panic ────────────────────────────────

    #[tokio::test]
    async fn send_non_channel_panics() {
        let result = crate::run_source(r#"
import { send } from "std/sync"
await send(42, "oops")
"#)
        .await;
        assert!(result.is_err(), "expected Tier-2 panic, got: {:?}", result);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Semaphore tests
    // ══════════════════════════════════════════════════════════════════════════

    // ── semaphore: basic available() after create / acquire / release ─────────

    #[tokio::test]
    async fn semaphore_available_tracks_permits() {
        let out = run(r#"
import { semaphore, acquire, release, available } from "std/sync"
let s = semaphore(2)
print(available(s))   // 2
await acquire(s)
print(available(s))   // 1
await acquire(s)
print(available(s))   // 0
release(s)
print(available(s))   // 1
release(s)
print(available(s))   // 2
"#)
        .await;
        assert_eq!(out, "2\n1\n0\n1\n2\n");
    }

    // ── semaphore: 3rd acquire blocks until a release (ordering proof) ────────
    // Two permits are immediately acquired; a 3rd acquire in a spawned task must
    // BLOCK until the main task releases one.  We prove the ordering via a signal
    // channel: the spawned task sends "acquired" AFTER the 3rd acquire succeeds;
    // the main task sends a release, then drains the signal.  If the 3rd acquire
    // had not blocked, the "acquired" signal would arrive BEFORE the release
    // (deadlock would reveal the bug instead of a wrong order).

    #[tokio::test]
    async fn semaphore_third_acquire_blocks_until_release() {
        let out = run(r#"
import { semaphore, acquire, release, available } from "std/sync"
import { channel, send, recv } from "std/sync"
import { spawn } from "std/task"

let s = semaphore(2)
let sig = channel()

await acquire(s)
await acquire(s)
print(available(s))   // 0

// This task will block on acquire until main releases a permit.
let waiter = spawn(async () => {
    await acquire(s)
    await send(sig, "acquired")
})

// Release one permit — this unblocks the waiter.
release(s)
let msg = await recv(sig)
print(msg)
print(available(s))   // 0 (waiter holds it)
release(s)            // clean up
await waiter
"#)
        .await;
        assert_eq!(out, "0\nacquired\n0\n");
    }

    // ── withPermit: returns fn result and restores available() ────────────────

    #[tokio::test]
    async fn with_permit_returns_result_and_releases() {
        let out = run(r#"
import { semaphore, withPermit, available } from "std/sync"
let s = semaphore(1)
print(available(s))   // 1
let result = await withPermit(s, async () => {
    return 42
})
print(result)         // 42
print(available(s))   // 1 — permit was released
"#)
        .await;
        assert_eq!(out, "1\n42\n1\n");
    }

    // ── withPermit: permit is released even when fn panics ────────────────────
    // The fn panics; withPermit must release the permit BEFORE propagating the
    // panic.  We drive the panicking withPermit to completion in a spawned task,
    // then `await` it inside a (synchronous) `recover` closure so `recover` runs
    // the await, the worker's panic propagates out of `withPermit`, and `recover`
    // catches it into `[nil, err]`.  We then assert (a) the panic propagated
    // (`err != nil`) and (b) `available()` is back to 1 — proving the permit was
    // released on the panic path.  (An `async` closure passed to `recover` would
    // NOT work: `recover` wraps the returned future without driving it, so the
    // panic would never run before the assert — the bug the reviewer caught.)

    #[tokio::test]
    async fn with_permit_releases_on_fn_panic() {
        let out = run(r#"
import { semaphore, withPermit, available } from "std/sync"
import { spawn } from "std/task"

let s = semaphore(1)
let worker = spawn(async () => {
    await withPermit(s, async () => {
        assert(false, "oops from fn")
    })
})
// Synchronous recover closure: the `await` runs inside recover, so the worker's
// panic propagates out of withPermit and is caught here.
let [_v, err] = recover(() => {
    await worker
    return nil
})
print(err != nil)     // true  — panic propagated out of withPermit
print(available(s))   // 1     — permit was released despite the panic
"#)
        .await;
        assert_eq!(out, "true\n1\n");
    }

    // ── release is capped at the initial permit count ─────────────────────────
    // An extra `release` with no matching `acquire` must NOT inflate the pool past
    // its declared size (otherwise the concurrency limit silently grows).

    #[tokio::test]
    async fn release_capped_at_initial_permits() {
        let out = run(r#"
import { semaphore, acquire, release, available } from "std/sync"
let s = semaphore(2)
print(available(s))   // 2
release(s)            // no matching acquire → capped, stays 2
print(available(s))   // 2
await acquire(s)
print(available(s))   // 1
release(s)            // back to 2
print(available(s))   // 2
release(s)            // capped again
print(available(s))   // 2
"#)
        .await;
        assert_eq!(out, "2\n2\n1\n2\n2\n");
    }

    // ── semaphore(0) → Tier-2 panic ───────────────────────────────────────────

    #[tokio::test]
    async fn semaphore_zero_permits_panics() {
        let result = crate::run_source(r#"
import { semaphore } from "std/sync"
let s = semaphore(0)
"#)
        .await;
        assert!(result.is_err(), "expected Tier-2 panic for 0 permits, got: {:?}", result);
    }

    // ── semaphore(negative) → Tier-2 panic ───────────────────────────────────

    #[tokio::test]
    async fn semaphore_negative_permits_panics() {
        let result = crate::run_source(r#"
import { semaphore } from "std/sync"
let s = semaphore(-3)
"#)
        .await;
        assert!(result.is_err(), "expected Tier-2 panic for negative permits, got: {:?}", result);
    }

    // ── non-semaphore arg to acquire → Tier-2 panic ──────────────────────────

    #[tokio::test]
    async fn acquire_non_semaphore_panics() {
        let result = crate::run_source(r#"
import { acquire } from "std/sync"
await acquire(42)
"#)
        .await;
        assert!(result.is_err(), "expected Tier-2 panic for non-semaphore arg, got: {:?}", result);
    }
}
