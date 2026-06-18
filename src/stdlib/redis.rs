//! `std/redis` — async Redis client (feature `redis`, backed by the `redis`
//! crate's `tokio-comp` async connection).
//!
//! Native-resource pattern over a network connection on the `!Send` runtime:
//!
//! - `await redis.connect(url) -> [conn, err]` — open a multiplexed async
//!   connection; a bad URL / unreachable server → Tier-1 err.
//! - Connection methods (async, Tier-1):
//!   `command(name, ...args) -> [value, err]` — a generic command,
//!   plus conveniences `get(key)`, `set(key, value)`, `del(key)`, `incr(key)`,
//!   `expire(key, secs)`, `exists(key)`, and `close()`.
//!
//! ## `!Send` / borrow discipline
//! The async connection's command methods take `&mut self`; we take it out of the
//! resource table across the await (`take_resource` → await → `return_resource`),
//! never holding a `resources`/`RefCell` borrow across an `.await`.
//!
//! ## Reply map (redis::Value → Value)
//! Nil→Nil; Int→Number; bulk string → Str if valid UTF-8 else Bytes; simple
//! status string → Str; array → Array (recursive); a Redis error reply →
//! the err channel.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value, ValueKind};
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("connect", bi("redis.connect"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg)))
}

impl Interp {
    /// `std/redis` module dispatch (only `connect`).
    pub(crate) async fn call_redis(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "connect" => self.redis_connect(args, span).await,
            _ => Err(AsError::at(format!("std/redis has no function '{}'", func), span).into()),
        }
    }

    async fn redis_connect(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let url = want_string(&arg(args, 0), span, "redis.connect")?;
        let client = match redis::Client::open(url.as_ref()) {
            Ok(c) => c,
            Err(e) => return Ok(err_pair(format!("redis.connect failed: {}", e))),
        };
        let conn = match client.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(e) => return Ok(err_pair(format!("redis.connect failed: {}", e))),
        };
        let handle = self.register_resource(
            NativeKind::RedisConnection,
            indexmap::IndexMap::new(),
            ResourceState::RedisConnection(Box::new(conn)),
        );
        Ok(make_pair(handle, Value::nil()))
    }

    /// Dispatch a method on a Redis connection handle.
    pub(crate) async fn call_redis_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        // Build the (command, args) for the convenience methods; `command` is generic.
        let (cmd, cmd_args): (String, Vec<Value>) = match m.method.as_str() {
            "close" => {
                // Dropping the connection closes it.
                self.take_resource(id);
                return Ok(Value::nil());
            }
            "command" => {
                let name = want_string(&arg(&args, 0), span, "connection.command")?.to_string();
                (name, args[1.min(args.len())..].to_vec())
            }
            "get" => ("GET".to_string(), vec![arg(&args, 0)]),
            "set" => ("SET".to_string(), vec![arg(&args, 0), arg(&args, 1)]),
            "del" => ("DEL".to_string(), vec![arg(&args, 0)]),
            "incr" => ("INCR".to_string(), vec![arg(&args, 0)]),
            "expire" => ("EXPIRE".to_string(), vec![arg(&args, 0), arg(&args, 1)]),
            "exists" => ("EXISTS".to_string(), vec![arg(&args, 0)]),
            other => {
                return Err(
                    AsError::at(format!("redis connection has no method '{}'", other), span).into(),
                )
            }
        };
        // Build the redis command, binding each arg as a string/number/bytes.
        let mut command = redis::cmd(&cmd);
        for a in &cmd_args {
            match a.kind() {
                ValueKind::Str(s) => {
                    command.arg(s.as_ref());
                }
                // NUM §4: an `Int` binds as an integer argument directly.
                ValueKind::Int(i) => {
                    command.arg(i);
                }
                ValueKind::Float(n) => {
                    // Integers as integers; fractions as their text form.
                    if n.fract() == 0.0 && n.is_finite() {
                        command.arg(n as i64);
                    } else {
                        command.arg(n.to_string());
                    }
                }
                ValueKind::Bool(b) => {
                    command.arg(if b { 1i64 } else { 0i64 });
                }
                ValueKind::Bytes(b) => {
                    command.arg(b.borrow().as_slice());
                }
                ValueKind::Nil => {
                    command.arg("");
                }
                _ => {
                    return Err(AsError::at(
                        format!(
                            "connection.{}: cannot use a {} as a Redis argument",
                            m.method,
                            crate::interp::type_name(a)
                        ),
                        span,
                    )
                    .into())
                }
            }
        }

        // RESIL §5.4 pre-check: an exhausted `resilience.deadline` budget → refuse
        // before taking the connection / issuing the command. NO deadline → the
        // `None` fast path (byte-identical).
        if matches!(self.deadline_remaining_ms(), Some(r) if r <= 0.0) {
            return Ok(crate::interp::deadline_exceeded_pair());
        }

        // Take the connection out across the await (it needs &mut self).
        let mut conn = match self.take_resource(id) {
            Some(ResourceState::RedisConnection(c)) => c,
            other => {
                if let Some(o) = other {
                    self.return_resource(id, o);
                }
                return Ok(err_pair("connection is closed".to_string()));
            }
        };
        // RESIL §5.4 budget-wrap: race the command against the remaining budget. On
        // the deadline branch the `query_async` future is DROPPED. The redis crate's
        // multiplexed connection pipelines commands over a shared socket and matches
        // replies to requests in FIFO order; dropping an in-flight command does NOT
        // cancel it server-side (no UNSUBSCRIBE/RESET is sent) and risks the next
        // reply being mis-correlated to the abandoned request — so a connection that
        // suffered a deadline-abandoned op should be DISCARDED rather than reused. The
        // resource is ALWAYS returned (so `close()` still works). NO deadline → the
        // `None` branch awaits unchanged (byte-identical).
        let result: Option<redis::RedisResult<redis::Value>> = match self.deadline_remaining_ms() {
            Some(r) => {
                tokio::select! {
                    res = command.query_async(conn.as_mut()) => Some(res),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(r as u64)) => None,
                }
            }
            None => Some(command.query_async(conn.as_mut()).await),
        };
        self.return_resource(id, ResourceState::RedisConnection(conn));
        match result {
            Some(Ok(v)) => Ok(make_pair(redis_to_value(&v), Value::nil())),
            Some(Err(e)) => Ok(err_pair(format!("connection.{} failed: {}", m.method, e))),
            None => Ok(crate::interp::deadline_exceeded_pair()),
        }
    }
}

/// Map a `redis::Value` reply to an AScript value.
fn redis_to_value(v: &redis::Value) -> Value {
    use std::cell::RefCell;
    match v {
        redis::Value::Nil => Value::nil(),
        // NUM §4: a Redis integer reply decodes to `Int`.
        redis::Value::Int(i) => Value::int(*i),
        redis::Value::BulkString(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => Value::str(s),
            Err(_) => Value::bytes_rc(Rc::new(RefCell::new(bytes.clone()))),
        },
        redis::Value::Array(items) => {
            Value::array_cell(crate::value::ArrayCell::new(items.iter().map(redis_to_value).collect()))
        }
        redis::Value::SimpleString(s) => Value::str(s.as_str()),
        redis::Value::Okay => Value::str("OK"),
        redis::Value::Map(pairs) => {
            // A RESP3 map → an Object when all keys stringify, else an array of pairs.
            let mut m = indexmap::IndexMap::new();
            for (k, val) in pairs {
                let kv = redis_to_value(k);
                let key = match kv.kind() {
                    ValueKind::Str(s) => s.to_string(),
                    _ => format!("{}", kv),
                };
                m.insert(key, redis_to_value(val));
            }
            Value::object_cell(crate::value::ObjectCell::new(m))
        }
        redis::Value::Double(d) => Value::float(*d),
        redis::Value::Boolean(b) => Value::bool_(*b),
        redis::Value::BigNumber(n) => Value::str(n.to_string()),
        redis::Value::VerbatimString { text, .. } => Value::str(text.as_str()),
        redis::Value::Set(items) => {
            Value::array_cell(crate::value::ArrayCell::new(items.iter().map(redis_to_value).collect()))
        }
        // Push/Attribute and any future RESP3 variants: best-effort Nil.
        _ => Value::nil(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn reply_map_basic_kinds() {
        assert_eq!(redis_to_value(&redis::Value::Nil), Value::nil());
        assert_eq!(redis_to_value(&redis::Value::Int(7)), Value::int(7));
        assert_eq!(redis_to_value(&redis::Value::Okay), Value::str("OK"));
        assert_eq!(
            redis_to_value(&redis::Value::SimpleString("PONG".into())),
            Value::str("PONG")
        );
        assert_eq!(
            redis_to_value(&redis::Value::BulkString(b"hello".to_vec())),
            Value::str("hello")
        );
        // Non-UTF-8 bulk string → Bytes.
        match redis_to_value(&redis::Value::BulkString(vec![0xff, 0x00])).kind() {
            ValueKind::Bytes(b) => assert_eq!(*b.borrow(), vec![0xff, 0x00]),
            other => panic!("expected bytes, got {:?}", other),
        }
        // Array recursion.
        let arr = redis::Value::Array(vec![redis::Value::Int(1), redis::Value::Int(2)]);
        match redis_to_value(&arr).kind() {
            ValueKind::Array(a) => assert_eq!(a.borrow().len(), 2),
            other => panic!("expected array, got {:?}", other),
        }
    }

    // Dead-port connect → clean Tier-1 err (NOT a panic).
    #[tokio::test(flavor = "current_thread")]
    async fn dead_port_connect_is_tier1_err() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let interp = std::rc::Rc::new(Interp::new());
                interp.install_self();
                let pair = interp
                    .call_redis(
                        "connect",
                        &[Value::str("redis://127.0.0.1:1")],
                        sp(),
                    )
                    .await
                    .expect("connect must not panic on a dead port");
                if let ValueKind::Array(a) = pair.kind() {
                    let b = a.borrow();
                    assert_eq!(b[0], Value::nil());
                    assert!(matches!(b[1].kind(), ValueKind::Object(_)), "err slot should be set");
                } else {
                    panic!("expected a [value, err] pair");
                }
            })
            .await;
    }

    // Live round-trip — skipped (passes) when ASCRIPT_TEST_REDIS_URL is unset.
    #[tokio::test(flavor = "current_thread")]
    async fn redis_roundtrip_live() {
        let Ok(url) = std::env::var("ASCRIPT_TEST_REDIS_URL") else {
            return; // no live server → no-op pass
        };
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let interp = std::rc::Rc::new(Interp::new());
                interp.install_self();
                let key = format!("sp5:redis:{}", uuid::Uuid::new_v4().simple());
                let pair = interp
                    .call_redis("connect", &[Value::str(url.clone())], sp())
                    .await
                    .unwrap();
                let conn = match pair.kind() {
                    ValueKind::Array(a) => a.borrow()[0].clone(),
                    _ => panic!("connect pair"),
                };
                let m = |method: &str| -> Rc<NativeMethod> {
                    match conn.kind() {
                        ValueKind::Native(n) => Rc::new(NativeMethod {
                            receiver: n.clone(),
                            method: method.to_string(),
                        }),
                        _ => unreachable!(),
                    }
                };
                // set key value
                interp
                    .call_redis_method(
                        &m("set"),
                        vec![Value::str(key.clone()), Value::str("v1")],
                        sp(),
                    )
                    .await
                    .unwrap();
                // get key → "v1"
                let got = interp
                    .call_redis_method(&m("get"), vec![Value::str(key.clone())], sp())
                    .await
                    .unwrap();
                if let ValueKind::Array(a) = got.kind() {
                    let b = a.borrow();
                    assert_eq!(b[1], Value::nil());
                    assert_eq!(b[0], Value::str("v1"));
                }
                // cleanup
                interp
                    .call_redis_method(&m("del"), vec![Value::str(key.clone())], sp())
                    .await
                    .unwrap();
                interp.call_redis_method(&m("close"), vec![], sp()).await.unwrap();
            })
            .await;
    }

    // ── RESIL §5.4: deadline-aware Redis consult site ────────────────────────

    use crate::interp::{task_locals_scope, TaskLocals};
    use crate::value::NativeObject;

    fn assert_deadline_pair(pair: &Value) {
        match pair.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                assert_eq!(b.len(), 2);
                assert_eq!(b[0], Value::nil());
                match b[1].kind() {
                    ValueKind::Object(o) => assert_eq!(
                        o.get("code"),
                        Some(Value::str("deadline-exceeded")),
                        "err code should be deadline-exceeded"
                    ),
                    other => panic!("err slot should be an object, got {:?}", other),
                }
            }
            other => panic!("expected a pair, got {:?}", other),
        }
    }

    // §5.4 pre-check (NO live server): an ALREADY-EXPIRED deadline → the command
    // dispatch returns the deadline-exceeded pair BEFORE taking the connection, so a
    // non-existent handle id never reaches the wire. (Without the pre-check, the same
    // bogus id would instead hit "connection is closed" — a {message}-only pair.)
    #[tokio::test(flavor = "current_thread")]
    async fn redis_expired_deadline_pre_check_no_op() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let interp = std::rc::Rc::new(Interp::new());
                interp.install_self();
                let now = interp.clock_monotonic_ms(crate::stdlib::time::real_monotonic_ms());
                let locals = std::rc::Rc::new(TaskLocals {
                    deadline_at_ms: Some(now - 1000.0),
                    trace_id: None,
                });
                let recv = std::rc::Rc::new(NativeObject {
                    id: u64::MAX, // not registered
                    kind: NativeKind::RedisConnection,
                    fields: indexmap::IndexMap::new(),
                });
                let m = std::rc::Rc::new(NativeMethod {
                    receiver: recv,
                    method: "get".to_string(),
                });
                let pair = task_locals_scope(Some(locals), async {
                    interp
                        .call_redis_method(&m, vec![Value::str("k")], sp())
                        .await
                        .expect("must not panic")
                })
                .await;
                assert_deadline_pair(&pair);
            })
            .await;
    }

    // Live budget-wrap — gated on ASCRIPT_TEST_REDIS_URL. A BLPOP on a missing key
    // blocks for `timeout` seconds; under a 100ms deadline the command returns
    // `deadline-exceeded` well before the 5s BLPOP timeout.
    #[tokio::test(flavor = "current_thread")]
    async fn redis_budget_wrap_live() {
        let Ok(url) = std::env::var("ASCRIPT_TEST_REDIS_URL") else {
            return; // no live server → no-op pass
        };
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let interp = std::rc::Rc::new(Interp::new());
                interp.install_self();
                let key = format!("sp5:resil:{}", uuid::Uuid::new_v4().simple());
                let pair = interp
                    .call_redis("connect", &[Value::str(url.clone())], sp())
                    .await
                    .unwrap();
                let conn = match pair.kind() {
                    ValueKind::Array(a) => a.borrow()[0].clone(),
                    _ => panic!("connect pair"),
                };
                let recv = match conn.kind() {
                    ValueKind::Native(n) => n.clone(),
                    _ => panic!("handle"),
                };
                let m = std::rc::Rc::new(NativeMethod {
                    receiver: recv,
                    method: "command".to_string(),
                });
                let now = interp.clock_monotonic_ms(crate::stdlib::time::real_monotonic_ms());
                let locals = std::rc::Rc::new(TaskLocals {
                    deadline_at_ms: Some(now + 100.0),
                    trace_id: None,
                });
                let started = std::time::Instant::now();
                let res = task_locals_scope(Some(locals), async {
                    interp
                        .call_redis_method(
                            &m,
                            vec![Value::str("BLPOP"), Value::str(key.clone()), Value::int(5)],
                            sp(),
                        )
                        .await
                        .unwrap()
                })
                .await;
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(3),
                    "budget-wrap must return well before the 5s BLPOP timeout"
                );
                assert_deadline_pair(&res);
            })
            .await;
    }
}
