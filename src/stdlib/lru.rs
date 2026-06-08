//! `std/lru` — a bounded least-recently-used cache (core, NOT feature-gated).
//!
//! `lru.new(capacity)` returns a `Value::Native` handle backed by
//! `ResourceState::Lru` (a stateful, mutable resource — the honest fit for a
//! cache, matching the sqlite/tui handle style). Methods:
//!
//! - `get(key) -> value | nil` — returns the value and marks it most-recently-used.
//! - `set(key, value)` — inserts/updates and marks MRU; evicts the LRU entry when
//!   the size would exceed `capacity`.
//! - `has(key) -> bool` — membership WITHOUT changing recency.
//! - `delete(key) -> bool` — remove; true if it was present.
//! - `clear()` — drop all entries.
//! - `len() -> number` — current entry count.
//! - `keys() -> array` — keys in LRU→MRU order.
//!
//! Keys use the existing hashable `MapKey`, so any hashable Value is a key (same
//! as `std/map`). Recency is the insertion order of the backing `IndexMap`: a
//! touched entry is moved to the end (MRU); eviction removes index 0 (LRU).

use super::{arg, bi};
use crate::error::AsError;
use crate::interp::{Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{MapKey, NativeKind, NativeMethod, Value};
use indexmap::IndexMap;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("new", bi("lru.new"))]
}

/// The backing state for an LRU cache resource.
pub struct LruState {
    /// Entries in LRU→MRU order; the front (index 0) is the least-recently-used.
    map: IndexMap<MapKey, Value>,
    /// Maximum number of entries. A `set` beyond this evicts the front entry.
    capacity: usize,
}

impl LruState {
    fn new(capacity: usize) -> Self {
        LruState {
            map: IndexMap::new(),
            capacity,
        }
    }

    /// Move `key` to the MRU position (the end). Caller has verified presence.
    fn touch(&mut self, key: &MapKey) {
        if let Some(idx) = self.map.get_index_of(key) {
            // Move to the end without disturbing other entries' relative order.
            let last = self.map.len() - 1;
            if idx != last {
                self.map.move_index(idx, last);
            }
        }
    }
}

impl Interp {
    /// `std/lru` module dispatch (only `new`).
    pub(crate) fn call_lru_new(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "new" => {
                let cap = match arg(args, 0) {
                    Value::Float(n) if n.is_finite() && n >= 1.0 => n as usize,
                    Value::Nil => {
                        return Err(AsError::at("lru.new requires a capacity (number >= 1)", span)
                            .into())
                    }
                    other => {
                        return Err(AsError::at(
                            format!(
                                "lru.new capacity must be a number >= 1, got {}",
                                crate::interp::type_name(&other)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                let handle = self.register_resource(
                    NativeKind::Lru,
                    IndexMap::new(),
                    ResourceState::Lru(Box::new(LruState::new(cap))),
                );
                Ok(handle)
            }
            _ => Err(AsError::at(format!("std/lru has no function '{}'", func), span).into()),
        }
    }

    /// Dispatch a method on an LRU handle. Synchronous (no awaits), so a short
    /// `with_resource_mut` borrow is held only for the closure body.
    pub(crate) fn call_lru_method(
        &self,
        m: &Rc<NativeMethod>,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "get" => {
                let key = match MapKey::from_value(&arg(args, 0)) {
                    Some(k) => k,
                    None => return Ok(Value::Nil),
                };
                Ok(self.with_resource_mut(id, |r| match r {
                    Some(ResourceState::Lru(s)) => {
                        if s.map.contains_key(&key) {
                            s.touch(&key);
                            s.map.get(&key).cloned().unwrap_or(Value::Nil)
                        } else {
                            Value::Nil
                        }
                    }
                    _ => Value::Nil,
                }))
            }
            "set" => {
                let key = match MapKey::from_value(&arg(args, 0)) {
                    Some(k) => k,
                    None => {
                        return Err(AsError::at(
                            format!(
                                "lru.set: key type {} is not hashable",
                                crate::interp::type_name(&arg(args, 0))
                            ),
                            span,
                        )
                        .into())
                    }
                };
                let val = arg(args, 1);
                self.with_resource_mut(id, |r| {
                    if let Some(ResourceState::Lru(s)) = r {
                        if s.map.contains_key(&key) {
                            // Update value + mark MRU (no eviction; size unchanged).
                            s.map.insert(key.clone(), val);
                            s.touch(&key);
                        } else {
                            // Evict the LRU (front) entry if at capacity.
                            while s.map.len() >= s.capacity && !s.map.is_empty() {
                                s.map.shift_remove_index(0);
                            }
                            s.map.insert(key, val);
                        }
                    }
                });
                Ok(Value::Nil)
            }
            "has" => {
                let key = match MapKey::from_value(&arg(args, 0)) {
                    Some(k) => k,
                    None => return Ok(Value::Bool(false)),
                };
                Ok(Value::Bool(self.with_resource(id, |r| match r {
                    Some(ResourceState::Lru(s)) => s.map.contains_key(&key),
                    _ => false,
                })))
            }
            "delete" => {
                let key = match MapKey::from_value(&arg(args, 0)) {
                    Some(k) => k,
                    None => return Ok(Value::Bool(false)),
                };
                Ok(Value::Bool(self.with_resource_mut(id, |r| match r {
                    Some(ResourceState::Lru(s)) => s.map.shift_remove(&key).is_some(),
                    _ => false,
                })))
            }
            "clear" => {
                self.with_resource_mut(id, |r| {
                    if let Some(ResourceState::Lru(s)) = r {
                        s.map.clear();
                    }
                });
                Ok(Value::Nil)
            }
            "len" => Ok(Value::Float(self.with_resource(id, |r| match r {
                Some(ResourceState::Lru(s)) => s.map.len() as f64,
                _ => 0.0,
            }))),
            "keys" => {
                let keys = self.with_resource(id, |r| match r {
                    Some(ResourceState::Lru(s)) => {
                        s.map.keys().map(|k| k.to_value()).collect::<Vec<_>>()
                    }
                    _ => Vec::new(),
                });
                Ok(Value::Array(crate::value::ArrayCell::new(keys)))
            }
            other => Err(AsError::at(format!("lru cache has no method '{}'", other), span).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    #[tokio::test]
    async fn eviction_and_promotion() {
        let out = run(r#"
import { new } from "std/lru"
let cache = new(2)
cache.set("a", 1)
cache.set("b", 2)
cache.get("a")        // promotes "a" to MRU
cache.set("c", 3)     // evicts the LRU ("b")
print(cache.has("a")) // true
print(cache.has("b")) // false
print(cache.has("c")) // true
print(cache.len())    // 2
print(cache.get("a")) // 1
"#)
        .await;
        assert_eq!(out, "true\nfalse\ntrue\n2\n1\n");
    }

    #[tokio::test]
    async fn update_does_not_grow_or_evict() {
        let out = run(r#"
import { new } from "std/lru"
let c = new(2)
c.set("a", 1)
c.set("b", 2)
c.set("a", 10)        // update, not insert
print(c.len())        // 2
print(c.get("a"))     // 10
print(c.has("b"))     // true
"#)
        .await;
        assert_eq!(out, "2\n10\ntrue\n");
    }

    #[tokio::test]
    async fn delete_clear_keys() {
        let out = run(r#"
import { new } from "std/lru"
let c = new(3)
c.set("x", 1)
c.set("y", 2)
print(c.delete("x"))  // true
print(c.delete("x"))  // false (already gone)
print(len(c.keys()))  // 1
c.clear()
print(c.len())        // 0
"#)
        .await;
        assert_eq!(out, "true\nfalse\n1\n0\n");
    }

    #[tokio::test]
    async fn keys_in_lru_to_mru_order() {
        let out = run(r#"
import { new } from "std/lru"
let c = new(3)
c.set("a", 1)
c.set("b", 2)
c.set("c", 3)
c.get("a")            // a becomes MRU; order now b, c, a
let ks = c.keys()
print(ks[0])          // b
print(ks[1])          // c
print(ks[2])          // a
"#)
        .await;
        assert_eq!(out, "b\nc\na\n");
    }

    #[tokio::test]
    async fn new_requires_capacity() {
        let err = match crate::run_source("import { new } from \"std/lru\"\nlet c = new()").await {
            Ok(_) => panic!("expected a panic"),
            Err(e) => e.message,
        };
        assert!(err.contains("capacity"), "got: {}", err);
    }
}
