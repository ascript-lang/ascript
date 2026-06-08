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
use crate::value::{NativeKind, NativeMethod, Value};
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("connect", bi("redis.connect"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
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
        Ok(make_pair(handle, Value::Nil))
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
                return Ok(Value::Nil);
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
            match a {
                Value::Str(s) => {
                    command.arg(s.as_ref());
                }
                Value::Float(n) => {
                    // Integers as integers; fractions as their text form.
                    if n.fract() == 0.0 && n.is_finite() {
                        command.arg(*n as i64);
                    } else {
                        command.arg(n.to_string());
                    }
                }
                Value::Bool(b) => {
                    command.arg(if *b { 1i64 } else { 0i64 });
                }
                Value::Bytes(b) => {
                    command.arg(b.borrow().as_slice());
                }
                Value::Nil => {
                    command.arg("");
                }
                other => {
                    return Err(AsError::at(
                        format!(
                            "connection.{}: cannot use a {} as a Redis argument",
                            m.method,
                            crate::interp::type_name(other)
                        ),
                        span,
                    )
                    .into())
                }
            }
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
        let result: redis::RedisResult<redis::Value> =
            command.query_async(conn.as_mut()).await;
        self.return_resource(id, ResourceState::RedisConnection(conn));
        match result {
            Ok(v) => Ok(make_pair(redis_to_value(&v), Value::Nil)),
            Err(e) => Ok(err_pair(format!("connection.{} failed: {}", m.method, e))),
        }
    }
}

/// Map a `redis::Value` reply to an AScript value.
fn redis_to_value(v: &redis::Value) -> Value {
    use std::cell::RefCell;
    match v {
        redis::Value::Nil => Value::Nil,
        redis::Value::Int(i) => Value::Float(*i as f64),
        redis::Value::BulkString(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => Value::Str(s.into()),
            Err(_) => Value::Bytes(Rc::new(RefCell::new(bytes.clone()))),
        },
        redis::Value::Array(items) => {
            Value::Array(crate::value::ArrayCell::new(items.iter().map(redis_to_value).collect()))
        }
        redis::Value::SimpleString(s) => Value::Str(s.as_str().into()),
        redis::Value::Okay => Value::Str("OK".into()),
        redis::Value::Map(pairs) => {
            // A RESP3 map → an Object when all keys stringify, else an array of pairs.
            let mut m = indexmap::IndexMap::new();
            for (k, val) in pairs {
                let key = match redis_to_value(k) {
                    Value::Str(s) => s.to_string(),
                    other => format!("{}", other),
                };
                m.insert(key, redis_to_value(val));
            }
            Value::Object(crate::value::ObjectCell::new(m))
        }
        redis::Value::Double(d) => Value::Float(*d),
        redis::Value::Boolean(b) => Value::Bool(*b),
        redis::Value::BigNumber(n) => Value::Str(n.to_string().into()),
        redis::Value::VerbatimString { text, .. } => Value::Str(text.as_str().into()),
        redis::Value::Set(items) => {
            Value::Array(crate::value::ArrayCell::new(items.iter().map(redis_to_value).collect()))
        }
        // Push/Attribute and any future RESP3 variants: best-effort Nil.
        _ => Value::Nil,
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
        assert_eq!(redis_to_value(&redis::Value::Nil), Value::Nil);
        assert_eq!(redis_to_value(&redis::Value::Int(7)), Value::Float(7.0));
        assert_eq!(redis_to_value(&redis::Value::Okay), Value::Str("OK".into()));
        assert_eq!(
            redis_to_value(&redis::Value::SimpleString("PONG".into())),
            Value::Str("PONG".into())
        );
        assert_eq!(
            redis_to_value(&redis::Value::BulkString(b"hello".to_vec())),
            Value::Str("hello".into())
        );
        // Non-UTF-8 bulk string → Bytes.
        match redis_to_value(&redis::Value::BulkString(vec![0xff, 0x00])) {
            Value::Bytes(b) => assert_eq!(*b.borrow(), vec![0xff, 0x00]),
            other => panic!("expected bytes, got {:?}", other),
        }
        // Array recursion.
        let arr = redis::Value::Array(vec![redis::Value::Int(1), redis::Value::Int(2)]);
        match redis_to_value(&arr) {
            Value::Array(a) => assert_eq!(a.borrow().len(), 2),
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
                        &[Value::Str("redis://127.0.0.1:1".into())],
                        sp(),
                    )
                    .await
                    .expect("connect must not panic on a dead port");
                if let Value::Array(a) = &pair {
                    let b = a.borrow();
                    assert_eq!(b[0], Value::Nil);
                    assert!(matches!(b[1], Value::Object(_)), "err slot should be set");
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
                    .call_redis("connect", &[Value::Str(url.clone().into())], sp())
                    .await
                    .unwrap();
                let conn = match &pair {
                    Value::Array(a) => a.borrow()[0].clone(),
                    _ => panic!("connect pair"),
                };
                let m = |method: &str| -> Rc<NativeMethod> {
                    match &conn {
                        Value::Native(n) => Rc::new(NativeMethod {
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
                        vec![Value::Str(key.clone().into()), Value::Str("v1".into())],
                        sp(),
                    )
                    .await
                    .unwrap();
                // get key → "v1"
                let got = interp
                    .call_redis_method(&m("get"), vec![Value::Str(key.clone().into())], sp())
                    .await
                    .unwrap();
                if let Value::Array(a) = &got {
                    let b = a.borrow();
                    assert_eq!(b[1], Value::Nil);
                    assert_eq!(b[0], Value::Str("v1".into()));
                }
                // cleanup
                interp
                    .call_redis_method(&m("del"), vec![Value::Str(key.clone().into())], sp())
                    .await
                    .unwrap();
                interp.call_redis_method(&m("close"), vec![], sp()).await.unwrap();
            })
            .await;
    }
}
