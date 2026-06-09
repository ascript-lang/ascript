//! `std/shared` — the shared, immutable, zero-copy-across-isolates read-only heap
//! (SRV §3/§4). `shared.freeze(v)` deep-converts a value into an immutable,
//! `Arc`-backed `Value::Shared(Arc<SharedNode>)` — AScript's FIRST `Send` value,
//! so a 5 MB routing table can be shared across N worker isolates by an `Arc`
//! pointer bump instead of a per-request structured-clone copy.
//!
//! The freeze walk uses TWO identity tables (SRV §3.3), both keyed by the input
//! container's identity pointer (`gc::cc_addr` for `Cc` containers, `Rc::as_ptr`
//! for `Bytes`):
//!   1. `in_progress: HashSet<usize>` — the on-stack cycle marker. A pointer is
//!      inserted BEFORE recursing into a container's children and removed AFTER the
//!      child `Arc` is built; re-encountering an `in_progress` pointer is a genuine
//!      CYCLE (`a.push(a)`) → a recoverable Tier-2 panic (an `Arc` graph cannot
//!      represent a cycle without leaking).
//!   2. `completed: HashMap<usize, Arc<SharedNode>>` — finished-node DIAMOND
//!      sharing. A pointer already in `completed` (and NOT in_progress) is the same
//!      sub-tree reachable by two paths → reuse the existing `Arc` (a refcount bump,
//!      no re-walk), keeping freeze O(distinct nodes).
//!
//! Order matters: check `in_progress` FIRST (reject), then `completed` (reuse).

use super::{arg, bi};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::{SharedKey, SharedNode, SharedValue, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("freeze", bi("shared.freeze")),
        ("isShared", bi("shared.isShared")),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "freeze" => freeze(&arg(args, 0), span),
        "isShared" => Ok(Value::Bool(matches!(arg(args, 0), Value::Shared(_)))),
        other => Err(AsError::at(format!("unknown shared function '{}'", other), span).into()),
    }
}

/// `shared.freeze(v)` — deep-convert + freeze. Idempotent (a `Shared` returns the
/// SAME `Arc`); a non-sendable or cyclic value is a recoverable Tier-2 panic naming
/// the field path.
pub fn freeze(v: &Value, span: Span) -> Result<Value, Control> {
    // Fast path: freeze of an already-frozen value is the identity (the SAME Arc).
    if let Value::Shared(arc) = v {
        return Ok(Value::Shared(arc.clone()));
    }
    let mut walker = Freezer {
        in_progress: HashSet::new(),
        completed: HashMap::new(),
    };
    let node = walker.walk(v, &mut String::new(), span)?;
    Ok(Value::Shared(node))
}

/// A freeze-time failure: a non-freezable value kind, or a cycle, at a field path.
/// Anchored at the `shared.freeze(...)` call span so the diagnostic points at the
/// call site (a recoverable Tier-2 panic, catchable by `recover`).
struct FreezeError {
    message: String,
    span: Span,
}

impl From<FreezeError> for Control {
    fn from(e: FreezeError) -> Self {
        Control::Panic(AsError::at(e.message, e.span))
    }
}

struct Freezer {
    in_progress: HashSet<usize>,
    completed: HashMap<usize, Arc<SharedNode>>,
}

impl Freezer {
    /// Deep-freeze `v` into a `SharedValue`, threading `path` for error messages.
    fn walk(&mut self, v: &Value, path: &mut String, span: Span) -> Result<SharedValue, Control> {
        match v {
            // Scalars freeze directly — no identity, no recursion.
            Value::Nil => Ok(Arc::new(SharedNode::Nil)),
            Value::Bool(b) => Ok(Arc::new(SharedNode::Bool(*b))),
            Value::Int(i) => Ok(Arc::new(SharedNode::Int(*i))),
            Value::Float(n) => Ok(Arc::new(SharedNode::Float(*n))),
            Value::Decimal(d) => Ok(Arc::new(SharedNode::Decimal(*d))),
            Value::Str(s) => Ok(Arc::new(SharedNode::Str(Arc::from(&**s)))),

            // Freezing an already-frozen sub-value is the identity (shares the Arc).
            Value::Shared(arc) => Ok(arc.clone()),

            Value::Bytes(b) => {
                let ptr = Rc_as_ptr(b);
                if let Some(reused) = self.enter(ptr, path, span)? {
                    return Ok(reused);
                }
                let bytes: Arc<[u8]> = Arc::from(b.borrow().as_slice());
                let node = Arc::new(SharedNode::Bytes(bytes));
                self.finish(ptr, node)
            }

            Value::Array(a) => {
                let ptr = crate::gc::cc_addr(a);
                if let Some(reused) = self.enter(ptr, path, span)? {
                    return Ok(reused);
                }
                // Snapshot the elements (no borrow held across the recursive walk).
                let elems: Vec<Value> = a.borrow().clone();
                let mut frozen: Vec<SharedValue> = Vec::with_capacity(elems.len());
                for (i, el) in elems.iter().enumerate() {
                    let len = path.len();
                    path.push_str(&format!("[{i}]"));
                    frozen.push(self.walk(el, path, span)?);
                    path.truncate(len);
                }
                let node = Arc::new(SharedNode::Array(Arc::from(frozen)));
                self.finish(ptr, node)
            }

            Value::Object(o) => {
                let ptr = crate::gc::cc_addr(o);
                if let Some(reused) = self.enter(ptr, path, span)? {
                    return Ok(reused);
                }
                let entries: Vec<(String, Value)> = o
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let mut frozen: crate::value::SharedMap = Vec::with_capacity(entries.len());
                for (k, val) in &entries {
                    let len = path.len();
                    push_member(path, k);
                    let fv = self.walk(val, path, span)?;
                    path.truncate(len);
                    frozen.push((Arc::from(&**k), fv));
                }
                let node = Arc::new(SharedNode::Object(Arc::new(frozen)));
                self.finish(ptr, node)
            }

            Value::Map(m) => {
                let ptr = crate::gc::cc_addr(m);
                if let Some(reused) = self.enter(ptr, path, span)? {
                    return Ok(reused);
                }
                let entries: Vec<(crate::value::MapKey, Value)> = m
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let mut frozen: crate::value::SharedMapKeyed = Vec::with_capacity(entries.len());
                for (k, val) in &entries {
                    let len = path.len();
                    path.push_str(&format!("[{}]", display_key(k)));
                    let fv = self.walk(val, path, span)?;
                    path.truncate(len);
                    // Keys are already canonical (-0.0→+0.0, NaN unified) per NUM.
                    frozen.push((SharedKey::from_map_key(k), fv));
                }
                let node = Arc::new(SharedNode::Map(Arc::new(frozen)));
                self.finish(ptr, node)
            }

            Value::Set(s) => {
                let ptr = crate::gc::cc_addr(s);
                if let Some(reused) = self.enter(ptr, path, span)? {
                    return Ok(reused);
                }
                // Set elements are canonical scalar `MapKey`s — Send-safe directly.
                let keys: Vec<SharedKey> =
                    s.borrow().iter().map(SharedKey::from_map_key).collect();
                let node = Arc::new(SharedNode::Set(Arc::new(keys)));
                self.finish(ptr, node)
            }

            Value::EnumVariant(ev) => {
                // The payload value must also freeze. A variant's identity is its
                // (enum, name); the payload is the cycle-capable part.
                let len = path.len();
                path.push_str(".value");
                let value: SharedValue = match &ev.payload {
                    None => self.walk(&ev.value, path, span)?,
                    Some(crate::value::Payload::Positional(a)) => {
                        self.walk(&Value::Array(a.clone()), path, span)?
                    }
                    Some(crate::value::Payload::Named(o)) => {
                        self.walk(&Value::Object(o.clone()), path, span)?
                    }
                };
                path.truncate(len);
                Ok(Arc::new(SharedNode::EnumVariant {
                    enum_name: Arc::from(&*ev.enum_name),
                    name: Arc::from(&*ev.name),
                    value,
                }))
            }

            #[cfg(feature = "data")]
            Value::Regex(r) => Ok(Arc::new(SharedNode::Regex {
                source: Arc::from(&*r.source),
            })),

            Value::Instance(inst) => {
                let ptr = crate::gc::cc_addr(inst);
                if let Some(reused) = self.enter(ptr, path, span)? {
                    return Ok(reused);
                }
                let borrow = inst.borrow();
                let class_name: Arc<str> = Arc::from(borrow.class.name.as_str());
                let fields: Vec<(String, Value)> = borrow
                    .fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                drop(borrow);
                let mut frozen: crate::value::SharedMap = Vec::with_capacity(fields.len());
                for (k, val) in &fields {
                    let len = path.len();
                    push_member(path, k);
                    let fv = self.walk(val, path, span)?;
                    path.truncate(len);
                    frozen.push((Arc::from(&**k), fv));
                }
                let node = Arc::new(SharedNode::Instance {
                    class_name,
                    fields: Arc::new(frozen),
                });
                self.finish(ptr, node)
            }

            // Non-freezable kinds — a clean recoverable Tier-2 panic naming the kind
            // and the field path (mirrors the airlock's SendError message style).
            other => Err(FreezeError {
                message: format!(
                    "value of kind {} cannot be frozen at {}",
                    crate::interp::type_name(other),
                    if path.is_empty() { "<root>" } else { path }
                ),
                span,
            }
            .into()),
        }
    }

    /// Enter a container: reject a cycle (`in_progress` hit), reuse a diamond
    /// (`completed` hit), else mark `in_progress` and return `None` (caller builds).
    fn enter(
        &mut self,
        ptr: usize,
        path: &mut String,
        span: Span,
    ) -> Result<Option<SharedValue>, Control> {
        // Cycle FIRST: a pointer still on the freeze stack.
        if self.in_progress.contains(&ptr) {
            return Err(FreezeError {
                message: format!(
                    "shared.freeze does not support cyclic values at {}",
                    if path.is_empty() { "<root>" } else { path }
                ),
                span,
            }
            .into());
        }
        // Diamond: a finished node reachable by another path → reuse its Arc.
        if let Some(existing) = self.completed.get(&ptr) {
            return Ok(Some(existing.clone()));
        }
        self.in_progress.insert(ptr);
        Ok(None)
    }

    /// Finish a container: record it in `completed` (diamond reuse) and clear the
    /// `in_progress` marker (cycle scope ends), returning the built `Arc`.
    fn finish(&mut self, ptr: usize, node: Arc<SharedNode>) -> Result<SharedValue, Control> {
        self.in_progress.remove(&ptr);
        self.completed.insert(ptr, node.clone());
        Ok(node)
    }
}

/// `Rc::as_ptr`-derived identity for a `Bytes` cell.
#[allow(non_snake_case)]
fn Rc_as_ptr(b: &std::rc::Rc<std::cell::RefCell<Vec<u8>>>) -> usize {
    std::rc::Rc::as_ptr(b) as *const () as usize
}

/// Append a member access to `path` (`.name` or `["name"]`). Mirrors the airlock.
fn push_member(path: &mut String, key: &str) {
    if is_ident(key) {
        path.push('.');
        path.push_str(key);
    } else {
        path.push_str(&format!("[{:?}]", key));
    }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn display_key(k: &crate::value::MapKey) -> String {
    match k {
        crate::value::MapKey::Str(s) => format!("{:?}", s),
        other => format!("{}", other.to_value()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ArrayCell, MapCell, MapKey, ObjectCell, Value};
    use indexmap::IndexMap;

    fn s() -> Span {
        Span::new(0, 0)
    }

    fn obj(pairs: Vec<(&str, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Value::Object(ObjectCell::new(m))
    }

    #[test]
    fn freeze_scalars_and_containers() {
        let v = obj(vec![
            ("region", Value::Str("us".into())),
            ("n", Value::Int(5)),
            ("f", Value::Float(1.5)),
            (
                "limits",
                Value::Array(ArrayCell::new(vec![Value::Int(10), Value::Int(100)])),
            ),
        ]);
        let frozen = freeze(&v, s()).unwrap();
        match &frozen {
            Value::Shared(node) => match &**node {
                SharedNode::Object(map) => {
                    assert_eq!(map.len(), 4);
                    assert_eq!(&*map[0].0, "region");
                    assert!(matches!(&*map[0].1, SharedNode::Str(x) if &**x == "us"));
                    assert!(matches!(&*map[1].1, SharedNode::Int(5)));
                    assert!(matches!(&*map[2].1, SharedNode::Float(f) if *f == 1.5));
                    assert!(matches!(&*map[3].1, SharedNode::Array(_)));
                }
                _ => panic!("expected frozen object"),
            },
            _ => panic!("expected Shared"),
        }
    }

    #[test]
    fn freeze_is_idempotent_same_arc() {
        let v = obj(vec![("x", Value::Int(1))]);
        let f1 = freeze(&v, s()).unwrap();
        let f2 = freeze(&f1, s()).unwrap();
        // freeze(freeze(x)) returns the SAME Arc (identity-equal).
        assert_eq!(f1, f2);
        match (&f1, &f2) {
            (Value::Shared(a), Value::Shared(b)) => assert!(Arc::ptr_eq(a, b)),
            _ => panic!(),
        }
    }

    #[test]
    fn freeze_diamond_reuses_one_arc() {
        // One shared inner array reachable by two object keys → ONE frozen Arc.
        let inner = Value::Array(ArrayCell::new(vec![Value::Int(7)]));
        let v = obj(vec![("a", inner.clone()), ("b", inner)]);
        let frozen = freeze(&v, s()).unwrap();
        let node = match &frozen {
            Value::Shared(n) => n.clone(),
            _ => panic!(),
        };
        match &*node {
            SharedNode::Object(map) => {
                let a = match &*map[0].1 {
                    SharedNode::Array(_) => map[0].1.clone(),
                    _ => panic!(),
                };
                let b = map[1].1.clone();
                // The diamond is preserved: both keys point at the SAME frozen Arc.
                assert!(Arc::ptr_eq(&a, &b), "diamond must reuse one Arc");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn freeze_cycle_is_a_clean_panic_not_a_hang() {
        // a = [ ... ]; a.push(a)  — a self-referential array.
        let cell = ArrayCell::new(vec![Value::Int(1)]);
        let a = Value::Array(cell.clone());
        cell.borrow_mut().push(a.clone());
        let err = freeze(&a, s()).unwrap_err();
        match err {
            Control::Panic(e) => assert!(
                e.message.contains("cyclic"),
                "expected a cyclic-value panic, got: {}",
                e.message
            ),
            _ => panic!("expected a recoverable Tier-2 panic"),
        }
    }

    #[test]
    fn freeze_diamond_and_cycle_together() {
        // A value containing BOTH a diamond (reused) and a cycle (rejected).
        let shared_inner = Value::Array(ArrayCell::new(vec![Value::Int(9)]));
        let cyclic = ArrayCell::new(vec![shared_inner.clone(), shared_inner.clone()]);
        // Make `cyclic` reference itself → a cycle.
        cyclic.borrow_mut().push(Value::Array(cyclic.clone()));
        let v = Value::Array(cyclic);
        // The cycle wins (it is reached) → a clean panic, NOT a hang.
        let err = freeze(&v, s()).unwrap_err();
        assert!(matches!(err, Control::Panic(ref e) if e.message.contains("cyclic")));
    }

    #[test]
    fn freeze_diamond_without_cycle_succeeds() {
        // The same inner reached twice but NO self-reference → diamond reuse, no panic.
        let inner = Value::Array(ArrayCell::new(vec![Value::Int(9)]));
        let v = Value::Array(ArrayCell::new(vec![inner.clone(), inner]));
        let frozen = freeze(&v, s()).unwrap();
        let node = match &frozen {
            Value::Shared(n) => n.clone(),
            _ => panic!(),
        };
        if let SharedNode::Array(arr) = &*node {
            assert!(Arc::ptr_eq(&arr[0], &arr[1]), "diamond reuse");
        } else {
            panic!();
        }
    }

    #[test]
    fn freeze_nonfreezable_names_the_path() {
        // A function value inside a field → "value of kind function cannot be frozen
        // at .handler".
        let v = obj(vec![("handler", Value::Builtin("len".into()))]);
        let err = freeze(&v, s()).unwrap_err();
        match err {
            Control::Panic(e) => {
                assert!(e.message.contains("function"), "{}", e.message);
                assert!(e.message.contains(".handler"), "{}", e.message);
            }
            _ => panic!("expected panic"),
        }
    }

    #[test]
    fn freeze_map_canonical_keys() {
        let mut m = IndexMap::new();
        m.insert(MapKey::Str("k".into()), Value::Int(1));
        m.insert(MapKey::Int(2), Value::Int(3));
        let v = Value::Map(MapCell::new(m));
        let frozen = freeze(&v, s()).unwrap();
        match &frozen {
            Value::Shared(n) => match &**n {
                SharedNode::Map(map) => {
                    assert_eq!(map.len(), 2);
                    assert_eq!(map[0].0, SharedKey::Str("k".into()));
                    assert_eq!(map[1].0, SharedKey::Int(2));
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn is_shared_reflection() {
        let v = obj(vec![("x", Value::Int(1))]);
        let frozen = freeze(&v, s()).unwrap();
        assert_eq!(call("isShared", &[frozen], s()).unwrap(), Value::Bool(true));
        assert_eq!(call("isShared", &[v], s()).unwrap(), Value::Bool(false));
    }
}
