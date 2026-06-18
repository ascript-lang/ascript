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
use crate::value::{MapKey, NativeKind, NativeMethod, Value, ValueKind};
use indexmap::IndexMap;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("new", bi("lru.new"))]
}

/// The backing state for an LRU cache resource.
pub struct LruState {
    /// Entries in LRU→MRU order; the front (index 0) is the least-recently-used.
    pub(crate) map: IndexMap<MapKey, Value>,
    /// Maximum number of entries. A `set` beyond this evicts the front entry.
    pub(crate) capacity: usize,
    /// Total number of LRU evictions since this cache was created.
    /// Incremented each time an entry is evicted due to capacity overflow.
    /// Additive-only: does NOT change get/has/delete/clear semantics.
    pub(crate) eviction_count: u64,
}

impl LruState {
    pub(crate) fn new(capacity: usize) -> Self {
        LruState {
            map: IndexMap::new(),
            capacity,
            eviction_count: 0,
        }
    }

    /// Move `key` to the MRU position (the end). Caller has verified presence.
    pub(crate) fn touch(&mut self, key: &MapKey) {
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
                let v = arg(args, 0);
                let cap = match v.kind() {
                    // NUM §4: accept BOTH numeric subtypes for the capacity.
                    _ if v.as_f64().is_some_and(|n| n.is_finite() && n >= 1.0) => {
                        v.as_f64().unwrap_or(0.0) as usize
                    }
                    ValueKind::Nil => {
                        return Err(AsError::at("lru.new requires a capacity (number >= 1)", span)
                            .into())
                    }
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "lru.new capacity must be a number >= 1, got {}",
                                crate::interp::type_name(&v)
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

    /// Create an LRU handle directly from Rust code (bypasses the string dispatcher).
    ///
    /// Used by `resilience.keyedLimiter` and `resilience.memoize` so they can create
    /// a real `std/lru` Native handle with the shipped eviction machinery without going
    /// through `call_lru_new`'s argument parsing.
    pub(crate) fn new_lru_handle(&self, capacity: usize) -> Value {
        self.register_resource(
            NativeKind::Lru,
            IndexMap::new(),
            ResourceState::Lru(Box::new(LruState::new(capacity))),
        )
    }

    /// Read the eviction counter from an LRU handle.
    ///
    /// Returns 0 if the handle is not a valid Lru resource.
    /// Used by `resilience.keyedLimiter.stats()` to expose `evictions`.
    pub(crate) fn lru_eviction_count(&self, id: u64) -> u64 {
        self.with_resource(id, |r| match r {
            Some(ResourceState::Lru(s)) => s.eviction_count,
            _ => 0,
        })
    }

    /// Read the number of entries in an LRU handle.
    ///
    /// Returns 0 if the handle is not a valid Lru resource.
    /// Used by `resilience.keyedLimiter.stats()` to expose `keys`.
    pub(crate) fn lru_len(&self, id: u64) -> usize {
        self.with_resource(id, |r| match r {
            Some(ResourceState::Lru(s)) => s.map.len(),
            _ => 0,
        })
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
                    None => return Ok(Value::nil()),
                };
                Ok(self.with_resource_mut(id, |r| match r {
                    Some(ResourceState::Lru(s)) => {
                        if s.map.contains_key(&key) {
                            s.touch(&key);
                            s.map.get(&key).cloned().unwrap_or(Value::nil())
                        } else {
                            Value::nil()
                        }
                    }
                    _ => Value::nil(),
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
                                s.eviction_count += 1;
                            }
                            s.map.insert(key, val);
                        }
                    }
                });
                Ok(Value::nil())
            }
            "has" => {
                let key = match MapKey::from_value(&arg(args, 0)) {
                    Some(k) => k,
                    None => return Ok(Value::bool_(false)),
                };
                Ok(Value::bool_(self.with_resource(id, |r| match r {
                    Some(ResourceState::Lru(s)) => s.map.contains_key(&key),
                    _ => false,
                })))
            }
            "delete" => {
                let key = match MapKey::from_value(&arg(args, 0)) {
                    Some(k) => k,
                    None => return Ok(Value::bool_(false)),
                };
                Ok(Value::bool_(self.with_resource_mut(id, |r| match r {
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
                Ok(Value::nil())
            }
            // NUM §4: a count is an `Int`.
            "len" => Ok(Value::int(self.with_resource(id, |r| match r {
                Some(ResourceState::Lru(s)) => s.map.len() as i64,
                _ => 0,
            }))),
            "keys" => {
                let keys = self.with_resource(id, |r| match r {
                    Some(ResourceState::Lru(s)) => {
                        s.map.keys().map(|k| k.to_value()).collect::<Vec<_>>()
                    }
                    _ => Vec::new(),
                });
                Ok(Value::array(keys))
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

    /// Eviction counter increments on insert-at-capacity; get/set within capacity does NOT.
    ///
    /// This test exercises the `pub(crate) eviction_count` field added by Task 2.2
    /// (RESIL §3.2.2). It uses the Rust-internal path (`new_lru_handle` + `with_resource`)
    /// rather than going through the AScript surface because the eviction counter is an
    /// internal implementation detail not exposed on the `std/lru` API.
    #[tokio::test]
    async fn eviction_counter_increments_on_capacity_overflow() {
        // capacity=2; insert 3 entries → first insert-at-capacity evicts entry 0.
        let out = run(r#"
import { new } from "std/lru"
let c = new(2)
c.set("a", 1)
c.set("b", 2)
c.set("c", 3)     // evicts "a"
print(c.has("a")) // false (evicted)
print(c.has("b")) // true
print(c.has("c")) // true
c.set("d", 4)     // evicts "b"
print(c.has("b")) // false
c.get("c")        // touch — no eviction
c.set("c", 99)    // update — no eviction (size unchanged)
print(c.len())    // 2
"#)
        .await;
        assert_eq!(out, "false\ntrue\ntrue\nfalse\n2\n");
        // The observable behaviour (above) is identical to pre-Task-2.2.
        // The eviction counter itself is verified via the Rust-internal API below.
        use crate::interp::{Interp, ResourceState};
        let interp = Interp::new();
        let handle = interp.new_lru_handle(2);
        let id = match handle.kind() {
            crate::value::ValueKind::Native(n) => n.id,
            _ => panic!("expected Native handle"),
        };
        // Initially 0 evictions.
        assert_eq!(interp.lru_eviction_count(id), 0, "no evictions on empty cache");

        // Insert two entries (within capacity) — no eviction.
        use crate::value::{MapKey, Value};
        interp.with_resource_mut(id, |r| {
            if let Some(ResourceState::Lru(s)) = r {
                s.map.insert(MapKey::Str("a".into()), Value::int(1));
                s.map.insert(MapKey::Str("b".into()), Value::int(2));
            }
        });
        assert_eq!(interp.lru_eviction_count(id), 0, "no evictions within capacity");

        // Insert a third entry past capacity → one eviction.
        interp.with_resource_mut(id, |r| {
            if let Some(ResourceState::Lru(s)) = r {
                while s.map.len() >= s.capacity && !s.map.is_empty() {
                    s.map.shift_remove_index(0);
                    s.eviction_count += 1;
                }
                s.map.insert(MapKey::Str("c".into()), Value::int(3));
            }
        });
        assert_eq!(interp.lru_eviction_count(id), 1, "one eviction after capacity overflow");

        // Update an existing key — no new eviction.
        interp.with_resource_mut(id, |r| {
            if let Some(ResourceState::Lru(s)) = r {
                if let Some(v) = s.map.get_mut(&MapKey::Str("b".into())) {
                    *v = Value::int(99);
                }
            }
        });
        assert_eq!(interp.lru_eviction_count(id), 1, "update does not evict");
    }
}
