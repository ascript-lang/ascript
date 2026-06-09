//! Structured-clone Value serializer (Workers Spec A §5). The airlock: only bytes
//! cross threads — never a `Value`, never the `Interp`. Semantics follow the WHATWG
//! structured-clone algorithm (cycle table + per-kind copy; class reconstruction by
//! identity + fields). Engine-agnostic: operates purely on `Value`.
//!
//! Wire format (little-endian; lengths are `u32`):
//!   Each value is one TAG byte, then its payload.
//!     0  Nil
//!     1  Bool        u8 (0/1)
//!     2  Number      f64 bits (u64)
//!     3  Decimal     len + UTF-8 (canonical Decimal string)
//!     4  Str         len + UTF-8
//!     5  Bytes       container-id (u32) + len + raw bytes
//!     6  Array       container-id (u32) + len + elements
//!     7  Object      container-id (u32) + len + (keyStr, value)*
//!     8  Map         container-id (u32) + len + (keyValue, value)*  (key is a tagged value)
//!     9  Set         container-id (u32) + len + (keyValue)*         (each a tagged value)
//!    10  EnumVariant enum_name(str) + variant_name(str) + backing value
//!                    + payload_tag(u8): 0 unit, 1 positional (an Array value),
//!                      2 named (an Object value). Unit variants re-intern far-side;
//!                      payload variants rebuild a fresh constructed variant (ADT §6).
//!    11  Regex       source(str)
//!    12  Instance    container-id (u32) + class_name(str) + field-count + (name, value)*
//!    13  Ref         container-id (u32) — a back-reference to an already-emitted container
//!
//! Cycles: every container (Array/Object/Map/Set/Bytes/Instance) is assigned a serial
//! id the first time it is encoded; a second encounter emits tag 13 + that id. On
//! decode the empty container is allocated and registered BEFORE its contents are
//! filled, so a forward `Ref` resolves to the same (cycle-capable) handle.

use crate::interp::Interp;
use crate::value::{MapKey, Value};
use std::collections::HashMap;

/// A value that cannot cross an isolate boundary (our DataCloneError analog).
#[derive(Debug, Clone)]
pub struct SendError {
    /// The kind name, e.g. `"function"`, `"native"`, `"future"`, `"generator"`.
    pub kind: &'static str,
    /// The field path to the offending value, e.g. `"[1].cb"`, `"map[\"k\"].field"`.
    pub path: String,
    /// An optional remediation hint (e.g. the channel/emitter advice).
    pub hint: Option<&'static str>,
}

impl SendError {
    pub fn message(&self) -> String {
        let mut m = format!(
            "value of kind {} cannot be sent to a worker at {}",
            self.kind, self.path
        );
        if let Some(h) = self.hint {
            m.push_str(" — ");
            m.push_str(h);
        }
        m
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl From<SendError> for crate::error::AsError {
    fn from(e: SendError) -> Self {
        // A recoverable Tier-2 panic (catchable by `recover`), not a hard abort.
        crate::error::AsError::new(e.message())
    }
}

impl From<SendError> for crate::interp::Control {
    fn from(e: SendError) -> Self {
        crate::interp::Control::Panic(e.into())
    }
}

const CHANNEL_HINT: &str = "event emitters / channels are isolate-local; communicate \
across workers via worker results (Spec A) or actor/generator messages (Spec B)";

// Wire tags. Kept as named constants so encode/decode never drift.
const TAG_NIL: u8 = 0;
const TAG_BOOL: u8 = 1;
const TAG_NUMBER: u8 = 2;
const TAG_DECIMAL: u8 = 3;
const TAG_STR: u8 = 4;
const TAG_BYTES: u8 = 5;
const TAG_ARRAY: u8 = 6;
const TAG_OBJECT: u8 = 7;
const TAG_MAP: u8 = 8;
const TAG_SET: u8 = 9;
const TAG_ENUM: u8 = 10;
#[cfg(feature = "data")]
const TAG_REGEX: u8 = 11;
const TAG_INSTANCE: u8 = 12;
const TAG_REF: u8 = 13;
/// A 64-bit integer (NUM §3.1). A scalar wire kind, distinct from `TAG_NUMBER`
/// (float) so an `int` round-trips as an `int` (no lossy float fold).
const TAG_INT: u8 = 14;
/// SRV §3.7(b) — a frozen `Value::Shared`, encoded as a u32 INDEX into the encode
/// side-vector `Writer.shared` (a `Vec<Arc<SharedNode>>` shipped alongside the byte
/// buffer). Encoding pushes `arc.clone()` (an atomic bump) + writes the index — NO
/// deep walk of the frozen graph. Decode reconstructs `Value::Shared` from the
/// side-vector by index. WORKER-WIRE ONLY — never an `.aso` constant (no
/// `ASO_FORMAT_VERSION` bump). The next free tag after `TAG_INT = 14`.
const TAG_SHARED: u8 = 15;

// ---------------------------------------------------------------------------
// Sendability gate
// ---------------------------------------------------------------------------

/// Whether `v` is a value that can cross an isolate boundary. A recursive walk that
/// builds a field path and rejects the first non-sendable value it finds. Cycles are
/// guarded by an identity set of visited container pointers so the walk terminates.
pub fn check_sendable(v: &Value) -> Result<(), SendError> {
    let mut seen: HashMap<usize, ()> = HashMap::new();
    check_inner(v, &mut String::new(), &mut seen)
}

/// The kind name + hint for a non-sendable value, or `None` if it is sendable.
fn unsendable_kind(v: &Value) -> Option<(&'static str, Option<&'static str>)> {
    match v {
        Value::Nil
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Decimal(_)
        | Value::Str(_)
        | Value::Bytes(_)
        | Value::Array(_)
        | Value::Object(_)
        | Value::Map(_)
        | Value::Set(_)
        | Value::EnumVariant(_)
        | Value::Instance(_) => None,
        // SRV §3.7: a frozen `Shared` is THE sendable-by-pointer value — it crosses
        // by an `Arc` clone (zero copy), never a structured-clone walk. Sendable leaf.
        Value::Shared(_) => None,
        #[cfg(feature = "data")]
        Value::Regex(_) => None,

        Value::Function(_) => Some(("function", None)),
        Value::Builtin(_) => Some(("function", None)),
        Value::Closure(_) => Some(("function", None)),
        Value::BoundMethod(_) => Some(("function", None)),
        Value::ClassMethod(..) => Some(("function", None)),
        Value::GeneratorMethod(..) => Some(("function", None)),

        Value::NativeMethod(_) => Some(("native", None)),
        Value::Native(n) => {
            // Event emitters and std/sync channels are isolate-local; nudge the
            // author toward worker results / actor messages.
            let hint = matches!(
                n.kind,
                crate::value::NativeKind::Channel | crate::value::NativeKind::Events
            )
            .then_some(CHANNEL_HINT);
            Some(("native", hint))
        }

        Value::Future(_) => Some(("future", None)),
        Value::Generator(_) => Some(("generator", None)),

        Value::Class(_) => Some(("class", None)),
        // IFACE (C5): an interface descriptor is code, non-sendable as a VALUE. This
        // arm makes `check_sendable` reject it with a field path, so `encode_value`'s
        // catch-all `unreachable!` is never reached. (Interfaces cross isolates via
        // code-shipping, Task 8 — not the value serializer.)
        Value::Interface(_) => Some(("interface", None)),
        Value::Enum(_) => Some(("enum", None)),
        Value::Super(_) => Some(("super", None)),
    }
}

fn check_inner(
    v: &Value,
    path: &mut String,
    seen: &mut HashMap<usize, ()>,
) -> Result<(), SendError> {
    if let Some((kind, hint)) = unsendable_kind(v) {
        return Err(SendError {
            kind,
            path: path.clone(),
            hint,
        });
    }
    match v {
        Value::Array(a) => {
            let id = crate::gc::cc_addr(a);
            if seen.insert(id, ()).is_some() {
                return Ok(());
            }
            for (i, elem) in a.borrow().iter().enumerate() {
                let len = path.len();
                path.push_str(&format!("[{i}]"));
                check_inner(elem, path, seen)?;
                path.truncate(len);
            }
        }
        Value::Object(o) => {
            let id = crate::gc::cc_addr(o);
            if seen.insert(id, ()).is_some() {
                return Ok(());
            }
            for (k, val) in o.borrow().iter() {
                let len = path.len();
                push_member(path, k);
                check_inner(val, path, seen)?;
                path.truncate(len);
            }
        }
        Value::Map(m) => {
            let id = crate::gc::cc_addr(m);
            if seen.insert(id, ()).is_some() {
                return Ok(());
            }
            for (k, val) in m.borrow().iter() {
                let len = path.len();
                path.push_str(&format!("[{}]", display_key(k)));
                check_inner(val, path, seen)?;
                path.truncate(len);
            }
        }
        Value::Set(s) => {
            let id = crate::gc::cc_addr(s);
            if seen.insert(id, ()).is_some() {
                return Ok(());
            }
            // Set elements are `MapKey`s — always scalar/sendable — so there is
            // nothing further to recurse into; registering the id is enough.
        }
        Value::Instance(inst) => {
            let id = crate::gc::cc_addr(inst);
            if seen.insert(id, ()).is_some() {
                return Ok(());
            }
            let borrow = inst.borrow();
            for (k, val) in borrow.fields.iter() {
                let len = path.len();
                push_member(path, k);
                check_inner(val, path, seen)?;
                path.truncate(len);
            }
        }
        Value::EnumVariant(ev) => {
            // The backing value of a variant must also be sendable.
            let len = path.len();
            path.push_str(".value");
            check_inner(&ev.value, path, seen)?;
            path.truncate(len);
            // ADT §6: a payload's values must be sendable too; the path threads
            // through the payload (`arg[0].payload.items[2]` / `.payload.<field>`).
            // The payload `Cc` participates in the visited-ref set (cycle-safe).
            match &ev.payload {
                Some(crate::value::Payload::Positional(a)) => {
                    let id = crate::gc::cc_addr(a);
                    if seen.insert(id, ()).is_some() {
                        return Ok(());
                    }
                    for (i, val) in a.borrow().iter().enumerate() {
                        let len = path.len();
                        path.push_str(&format!(".payload.items[{i}]"));
                        check_inner(val, path, seen)?;
                        path.truncate(len);
                    }
                }
                Some(crate::value::Payload::Named(o)) => {
                    let id = crate::gc::cc_addr(o);
                    if seen.insert(id, ()).is_some() {
                        return Ok(());
                    }
                    for (k, val) in o.borrow().iter() {
                        let len = path.len();
                        path.push_str(".payload");
                        push_member(path, k);
                        check_inner(val, path, seen)?;
                        path.truncate(len);
                    }
                }
                None => {}
            }
        }
        // Scalars / leaf containers: nothing to recurse.
        _ => {}
    }
    Ok(())
}

/// Append a member access to `path`: `.name` for an identifier-shaped key, else
/// `["name"]` for a key that is not a bare identifier.
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

/// A display form for a map key, used in error paths (`map["k"]`).
fn display_key(k: &MapKey) -> String {
    match k {
        MapKey::Str(s) => format!("{:?}", s),
        other => format!("{}", other.to_value()),
    }
}

// ---------------------------------------------------------------------------
// Byte writer / reader
// ---------------------------------------------------------------------------

struct Writer {
    buf: Vec<u8>,
    /// SRV §3.7(b) — the shared-table side-vector. A frozen `Value::Shared` is
    /// encoded as a `TAG_SHARED` + a u32 index into this vector (`shared.push(
    /// arc.clone())`, an atomic bump, NO graph walk). Shipped alongside `buf`.
    shared: Vec<std::sync::Arc<crate::value::SharedNode>>,
}

impl Writer {
    fn new() -> Self {
        Writer {
            buf: Vec::new(),
            shared: Vec::new(),
        }
    }
    fn u8(&mut self, b: u8) {
        self.buf.push(b);
    }
    fn u32(&mut self, n: u32) {
        self.buf.extend_from_slice(&n.to_le_bytes());
    }
    fn u64(&mut self, n: u64) {
        self.buf.extend_from_slice(&n.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    /// Bytes left to read. A sound upper bound for any pre-allocation, since every
    /// wire element occupies at least one byte (see `decode_value`'s array arm).
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
    fn u8(&mut self) -> Result<u8, SendError> {
        let b = *self.buf.get(self.pos).ok_or_else(truncated_err)?;
        // `checked_add` so a corrupt/maximal `pos` can never wrap (target-independent).
        self.pos = self.pos.checked_add(1).ok_or_else(truncated_err)?;
        Ok(b)
    }
    fn u32(&mut self) -> Result<u32, SendError> {
        let end = self.pos.checked_add(4).ok_or_else(truncated_err)?;
        let slice = self.buf.get(self.pos..end).ok_or_else(truncated_err)?;
        self.pos = end;
        Ok(u32::from_le_bytes(slice.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, SendError> {
        let end = self.pos.checked_add(8).ok_or_else(truncated_err)?;
        let slice = self.buf.get(self.pos..end).ok_or_else(truncated_err)?;
        self.pos = end;
        Ok(u64::from_le_bytes(slice.try_into().unwrap()))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, SendError> {
        let len = self.u32()? as usize;
        // `checked_add` guards a huge `len` from wrapping `pos`; the `get` then
        // bounds-checks, so a bogus length returns `Err` without allocating.
        let end = self.pos.checked_add(len).ok_or_else(truncated_err)?;
        let slice = self.buf.get(self.pos..end).ok_or_else(truncated_err)?;
        self.pos = end;
        Ok(slice.to_vec())
    }
    fn str(&mut self) -> Result<String, SendError> {
        let raw = self.bytes()?;
        String::from_utf8(raw).map_err(|_| SendError {
            kind: "decode",
            path: "<utf8>".to_string(),
            hint: None,
        })
    }
}

fn truncated_err() -> SendError {
    SendError {
        kind: "decode",
        path: "<truncated>".to_string(),
        hint: None,
    }
}

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

/// Serialize `v` to a structured-clone byte payload PLUS a side-vector of frozen
/// `Arc<SharedNode>`s (SRV §3.7b — a `Value::Shared` crosses by an `Arc` clone, NOT
/// a deep copy). Rejects non-sendable values (a bad value never produces a
/// half-written payload — `check_sendable` runs first). The two members travel
/// together (`WorkerRequest.args` + `WorkerRequest.shared`, etc.).
pub fn encode(v: &Value) -> Result<(Vec<u8>, Vec<std::sync::Arc<crate::value::SharedNode>>), SendError> {
    check_sendable(v)?;
    let mut w = Writer::new();
    // Maps a container's identity pointer to its assigned serial id.
    let mut ids: HashMap<usize, u32> = HashMap::new();
    encode_value(v, &mut w, &mut ids);
    Ok((w.buf, w.shared))
}

/// Assign the next serial id for a container pointer, or `Some(existing)` if it has
/// already been seen (the caller emits a `Ref` in that case).
fn intern(ids: &mut HashMap<usize, u32>, ptr: usize) -> Result<u32, u32> {
    if let Some(existing) = ids.get(&ptr) {
        return Err(*existing);
    }
    let id = ids.len() as u32;
    ids.insert(ptr, id);
    Ok(id)
}

fn encode_value(v: &Value, w: &mut Writer, ids: &mut HashMap<usize, u32>) {
    match v {
        Value::Nil => w.u8(TAG_NIL),
        Value::Bool(b) => {
            w.u8(TAG_BOOL);
            w.u8(*b as u8);
        }
        Value::Int(i) => {
            w.u8(TAG_INT);
            w.u64(*i as u64);
        }
        Value::Float(n) => {
            w.u8(TAG_NUMBER);
            w.u64(n.to_bits());
        }
        Value::Decimal(d) => {
            w.u8(TAG_DECIMAL);
            w.str(&d.to_string());
        }
        Value::Str(s) => {
            w.u8(TAG_STR);
            w.str(s);
        }
        Value::Bytes(b) => match intern(ids, std::rc::Rc::as_ptr(b) as *const () as usize) {
            Ok(id) => {
                w.u8(TAG_BYTES);
                w.u32(id);
                w.bytes(&b.borrow());
            }
            Err(existing) => emit_ref(w, existing),
        },
        Value::Array(a) => match intern(ids, crate::gc::cc_addr(a)) {
            Ok(id) => {
                w.u8(TAG_ARRAY);
                w.u32(id);
                let elems: Vec<Value> = a.borrow().clone();
                w.u32(elems.len() as u32);
                for e in &elems {
                    encode_value(e, w, ids);
                }
            }
            Err(existing) => emit_ref(w, existing),
        },
        Value::Object(o) => match intern(ids, crate::gc::cc_addr(o)) {
            Ok(id) => {
                w.u8(TAG_OBJECT);
                w.u32(id);
                let entries: Vec<(String, Value)> = o
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                w.u32(entries.len() as u32);
                for (k, val) in &entries {
                    w.str(k);
                    encode_value(val, w, ids);
                }
            }
            Err(existing) => emit_ref(w, existing),
        },
        Value::Map(m) => match intern(ids, crate::gc::cc_addr(m)) {
            Ok(id) => {
                w.u8(TAG_MAP);
                w.u32(id);
                let entries: Vec<(MapKey, Value)> = m
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                w.u32(entries.len() as u32);
                for (k, val) in &entries {
                    // The key is canonical-scalar — re-encode it as a tagged value.
                    encode_value(&k.to_value(), w, ids);
                    encode_value(val, w, ids);
                }
            }
            Err(existing) => emit_ref(w, existing),
        },
        Value::Set(s) => match intern(ids, crate::gc::cc_addr(s)) {
            Ok(id) => {
                w.u8(TAG_SET);
                w.u32(id);
                let elems: Vec<MapKey> = s.borrow().iter().cloned().collect();
                w.u32(elems.len() as u32);
                for k in &elems {
                    encode_value(&k.to_value(), w, ids);
                }
            }
            Err(existing) => emit_ref(w, existing),
        },
        Value::EnumVariant(ev) => {
            w.u8(TAG_ENUM);
            w.str(&ev.enum_name);
            w.str(&ev.name);
            encode_value(&ev.value, w, ids);
            // ADT §6: the payload tag — 0 (unit, old format), 1 (positional), 2
            // (named). The payload's `Cc` container is encoded as an ordinary
            // Array/Object so it participates in the visited-ref table (a recursive
            // payload that contains itself serializes once + refers by id).
            match &ev.payload {
                None => w.u8(0),
                Some(crate::value::Payload::Positional(a)) => {
                    w.u8(1);
                    encode_value(&Value::Array(a.clone()), w, ids);
                }
                Some(crate::value::Payload::Named(o)) => {
                    w.u8(2);
                    encode_value(&Value::Object(o.clone()), w, ids);
                }
            }
        }
        #[cfg(feature = "data")]
        Value::Regex(r) => {
            w.u8(TAG_REGEX);
            // Flags are inline in the pattern (`(?i)…`), so source alone round-trips.
            w.str(&r.source);
        }
        Value::Instance(inst) => match intern(ids, crate::gc::cc_addr(inst)) {
            Ok(id) => {
                w.u8(TAG_INSTANCE);
                w.u32(id);
                let borrow = inst.borrow();
                w.str(&borrow.class.name);
                let fields: Vec<(String, Value)> = borrow
                    .fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                drop(borrow);
                w.u32(fields.len() as u32);
                for (k, val) in &fields {
                    w.str(k);
                    encode_value(val, w, ids);
                }
            }
            Err(existing) => emit_ref(w, existing),
        },
        // SRV §3.7(b): a frozen `Shared` crosses by an `Arc` clone — push it into the
        // side-vector and write its index. NO deep walk of the frozen graph (the whole
        // point: a 5 MB frozen table costs one atomic bump, independent of size).
        Value::Shared(arc) => {
            let idx = w.shared.len() as u32;
            w.shared.push(arc.clone());
            w.u8(TAG_SHARED);
            w.u32(idx);
        }
        // Non-sendable kinds are rejected by `check_sendable` before we get here.
        // Encode is total over sendable values; reaching this arm is a bug.
        other => unreachable!("encode reached non-sendable value: {:?}", other),
    }
}

fn emit_ref(w: &mut Writer, id: u32) {
    w.u8(TAG_REF);
    w.u32(id);
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Deserialize a structured-clone byte payload back into a `Value`, reconstructing
/// containers (with cycles), regexes (re-compiled from source), and class instances
/// (by name + cloned fields). `interp` is the destination isolate (its class table is
/// consulted for instance reconstruction; for this task that is the same interp).
pub fn decode(bytes: &[u8], interp: &Interp) -> Result<Value, SendError> {
    decode_with_shared(bytes, &[], interp)
}

/// Deserialize a payload, resolving any `TAG_SHARED` index against the `shared`
/// side-vector (SRV §3.7b). A frozen `Value::Shared` is reconstructed by an `Arc`
/// clone from the side-vector — ZERO graph copy. Callers that carry no shared values
/// pass `&[]` (the plain [`decode`] wrapper).
pub fn decode_with_shared(
    bytes: &[u8],
    shared: &[std::sync::Arc<crate::value::SharedNode>],
    interp: &Interp,
) -> Result<Value, SendError> {
    let mut r = Reader::new(bytes);
    // Indexed by serial id: each container's reconstructed handle. Populated BEFORE
    // a container's contents are read, so forward `Ref`s resolve to the same handle.
    let mut table: Vec<Value> = Vec::new();
    decode_value(&mut r, &mut table, shared, interp)
}

fn decode_value(
    r: &mut Reader<'_>,
    table: &mut Vec<Value>,
    shared: &[std::sync::Arc<crate::value::SharedNode>],
    interp: &Interp,
) -> Result<Value, SendError> {
    let tag = r.u8()?;
    match tag {
        TAG_NIL => Ok(Value::Nil),
        TAG_BOOL => Ok(Value::Bool(r.u8()? != 0)),
        TAG_INT => Ok(Value::Int(r.u64()? as i64)),
        TAG_NUMBER => Ok(Value::Float(f64::from_bits(r.u64()?))),
        TAG_DECIMAL => {
            use rust_decimal::prelude::*;
            let s = r.str()?;
            let d = Decimal::from_str(&s).map_err(|_| SendError {
                kind: "decode",
                path: "<decimal>".to_string(),
                hint: None,
            })?;
            Ok(Value::Decimal(d))
        }
        TAG_STR => Ok(Value::Str(r.str()?.into())),
        TAG_BYTES => {
            let id = r.u32()? as usize;
            let cell = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            register(table, id, Value::Bytes(cell.clone()))?;
            let raw = r.bytes()?;
            *cell.borrow_mut() = raw;
            Ok(Value::Bytes(cell))
        }
        TAG_ARRAY => {
            let id = r.u32()? as usize;
            let cell = crate::value::ArrayCell::new(Vec::new());
            let value = Value::Array(cell.clone());
            register(table, id, value.clone())?;
            let len = r.u32()? as usize;
            // Bound the reservation against bytes remaining: `len` is attacker-
            // controlled and each element is ≥1 byte, so a 9-byte payload claiming
            // `len = 0xFFFFFFFF` cannot force a multi-GB allocation. The push loop
            // still errors on the first short read.
            let mut elems = Vec::with_capacity(len.min(r.remaining()));
            for _ in 0..len {
                elems.push(decode_value(r, table, shared, interp)?);
            }
            *cell.borrow_mut() = elems;
            Ok(value)
        }
        TAG_OBJECT => {
            let id = r.u32()? as usize;
            let cell = crate::value::ObjectCell::new(indexmap::IndexMap::new());
            let value = Value::Object(cell.clone());
            register(table, id, value.clone())?;
            let len = r.u32()? as usize;
            for _ in 0..len {
                let k = r.str()?;
                let v = decode_value(r, table, shared, interp)?;
                cell.borrow_mut().insert(k, v);
            }
            Ok(value)
        }
        TAG_MAP => {
            let id = r.u32()? as usize;
            let cell = crate::value::MapCell::new(indexmap::IndexMap::new());
            let value = Value::Map(cell.clone());
            register(table, id, value.clone())?;
            let len = r.u32()? as usize;
            for _ in 0..len {
                let key_val = decode_value(r, table, shared, interp)?;
                let v = decode_value(r, table, shared, interp)?;
                // Reapply canonicalization on the far side (−0.0→+0.0, NaN unified).
                let key = MapKey::from_value(&key_val).ok_or_else(|| SendError {
                    kind: "decode",
                    path: "<map-key>".to_string(),
                    hint: None,
                })?;
                cell.borrow_mut().insert(key, v);
            }
            Ok(value)
        }
        TAG_SET => {
            let id = r.u32()? as usize;
            let cell = crate::value::SetCell::new(indexmap::IndexSet::new());
            let value = Value::Set(cell.clone());
            register(table, id, value.clone())?;
            let len = r.u32()? as usize;
            for _ in 0..len {
                let key_val = decode_value(r, table, shared, interp)?;
                let key = MapKey::from_value(&key_val).ok_or_else(|| SendError {
                    kind: "decode",
                    path: "<set-elem>".to_string(),
                    hint: None,
                })?;
                cell.borrow_mut().insert(key);
            }
            Ok(value)
        }
        TAG_ENUM => {
            let enum_name = r.str()?;
            let name = r.str()?;
            let backing = decode_value(r, table, shared, interp)?;
            // ADT §6: the payload tag — 0 (unit), 1 (positional), 2 (named).
            let payload_tag = r.u8()?;
            match payload_tag {
                0 => {
                    // A UNIT variant: re-intern against the far-side `EnumDef` so it is
                    // identity-equal to the receiver isolate's own interned constant
                    // (`received == Shape.Point` is `true` within the receiver). The
                    // enum ships as a value const via the global-name fixpoint
                    // (`dispatch.rs`), so it is a global here. Best-effort: if the
                    // far-side `EnumDef` is absent, fall back to a fresh `Rc` (today's
                    // behavior — pinned by the round-trip test).
                    if let Some(interned) = reintern_unit_variant(interp, &enum_name, &name) {
                        return Ok(interned);
                    }
                    Ok(Value::EnumVariant(std::rc::Rc::new(
                        crate::value::EnumVariant {
                            enum_name,
                            name,
                            value: backing,
                            payload: None,
                            ctor: false,
                            def: None,
                        },
                    )))
                }
                1 => {
                    // POSITIONAL payload: decode the Array (cycle-safe via the ref
                    // table) and rebuild a constructed variant. Compares STRUCTURALLY,
                    // so a fresh allocation is correct (no re-interning needed).
                    let arr = decode_value(r, table, shared, interp)?;
                    let cell = match arr {
                        Value::Array(a) => a,
                        _ => {
                            return Err(SendError {
                                kind: "decode",
                                path: "<enum-payload>".to_string(),
                                hint: None,
                            })
                        }
                    };
                    Ok(Value::EnumVariant(std::rc::Rc::new(
                        crate::value::EnumVariant {
                            enum_name,
                            name,
                            value: Value::Nil,
                            payload: Some(crate::value::Payload::Positional(cell)),
                            ctor: false,
                            def: None,
                        },
                    )))
                }
                2 => {
                    // NAMED payload: decode the Object and rebuild a constructed variant.
                    let obj = decode_value(r, table, shared, interp)?;
                    let cell = match obj {
                        Value::Object(o) => o,
                        _ => {
                            return Err(SendError {
                                kind: "decode",
                                path: "<enum-payload>".to_string(),
                                hint: None,
                            })
                        }
                    };
                    Ok(Value::EnumVariant(std::rc::Rc::new(
                        crate::value::EnumVariant {
                            enum_name,
                            name,
                            value: Value::Nil,
                            payload: Some(crate::value::Payload::Named(cell)),
                            ctor: false,
                            def: None,
                        },
                    )))
                }
                other => Err(SendError {
                    kind: "decode",
                    path: format!("<enum-payload-tag {other}>"),
                    hint: None,
                }),
            }
        }
        #[cfg(feature = "data")]
        TAG_REGEX => {
            let source = r.str()?;
            let re = regex::Regex::new(&source).map_err(|_| SendError {
                kind: "decode",
                path: "<regex>".to_string(),
                hint: None,
            })?;
            Ok(Value::Regex(std::rc::Rc::new(crate::value::RegexHandle {
                re,
                source,
            })))
        }
        TAG_INSTANCE => {
            let id = r.u32()? as usize;
            let class_name = r.str()?;
            // Allocate the empty instance and register it BEFORE reading fields so a
            // self-referential field resolves to the same handle.
            let class = resolve_class(interp, &class_name);
            let cell = gcmodule::Cc::new(std::cell::RefCell::new(crate::value::Instance {
                class,
                fields: indexmap::IndexMap::new(),
                shape_id: std::cell::Cell::new(0),
                frozen: std::cell::Cell::new(false),
            }));
            let value = Value::Instance(cell.clone());
            register(table, id, value.clone())?;
            let len = r.u32()? as usize;
            for _ in 0..len {
                let k = r.str()?;
                let v = decode_value(r, table, shared, interp)?;
                cell.borrow_mut().fields.insert(k, v);
            }
            Ok(value)
        }
        TAG_REF => {
            let id = r.u32()? as usize;
            table.get(id).cloned().ok_or_else(|| SendError {
                kind: "decode",
                path: format!("<ref {id}>"),
                hint: None,
            })
        }
        // SRV §3.7(b): reconstruct a frozen `Value::Shared` from the side-vector by
        // index — another `Arc` clone (atomic bump), ZERO graph copy. A bounds-checked
        // index guards a corrupt/mismatched payload.
        TAG_SHARED => {
            let idx = r.u32()? as usize;
            let arc = shared.get(idx).cloned().ok_or_else(|| SendError {
                kind: "decode",
                path: format!("<shared {idx}>"),
                hint: None,
            })?;
            Ok(Value::Shared(arc))
        }
        other => Err(SendError {
            kind: "decode",
            path: format!("<tag {other}>"),
            hint: None,
        }),
    }
}

/// Store a freshly-allocated container under its serial id. Ids are dense and
/// assigned in encode order, so `id` always equals the current table length.
fn register(table: &mut Vec<Value>, id: usize, v: Value) -> Result<(), SendError> {
    if id != table.len() {
        return Err(SendError {
            kind: "decode",
            path: format!("<bad-id {id}>"),
            hint: None,
        });
    }
    table.push(v);
    Ok(())
}

/// ADT §6: re-intern a UNIT variant against the far-side `EnumDef`. Looks up
/// `enum_name` as a global on the receiver isolate (the enum ships as a value const
/// via the global-name fixpoint), then returns THAT isolate's interned variant
/// constant — so a received unit variant is identity-equal to the receiver's own
/// `Enum.Variant` literal. Returns `None` (best-effort fallback to a fresh `Rc`) if
/// the far-side `EnumDef` or the variant is absent.
fn reintern_unit_variant(interp: &Interp, enum_name: &str, name: &str) -> Option<Value> {
    // The receiver isolate runs on the VM, so the enum const is a VM user-global.
    let vm = interp.vm()?;
    match vm.user_global(enum_name) {
        Some(Value::Enum(def)) => def.variants.get(name).cloned(),
        _ => None,
    }
}

/// Reconstruct the `Rc<Class>` for an instance by name. A decoded `Instance` always
/// gets a standalone class shell with the original name, empty methods, and empty
/// fields — because classes are NOT shipped in the code slice (Spec A limitation).
/// The shell is sufficient for `type()` → `"instance"` and field access, but
/// `instanceof` identity and method dispatch are NOT preserved across the isolate
/// boundary.  Class shipping (full method transfer) is a Spec B / future TODO; see
/// `dispatch.rs`.  `_interp` is unused for now but is kept in the hook signature
/// as the intended future insertion point for a class-table registry lookup.
fn resolve_class(_interp: &Interp, name: &str) -> std::rc::Rc<crate::value::Class> {
    std::rc::Rc::new(crate::value::Class {
        name: name.to_string(),
        superclass: None,
        fields: indexmap::IndexMap::new(),
        methods: indexmap::IndexMap::new(),
        static_methods: indexmap::IndexMap::new(),
        def_env: crate::interp::global_env(),
        is_worker: false,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Interp;
    use crate::value::{ArrayCell, MapCell, MapKey, ObjectCell, SetCell, Value};
    use indexmap::{IndexMap, IndexSet};
    use std::cell::RefCell;
    use std::rc::Rc;

    // --- Direct `Value` constructors (the plan prefers these over heavy eval
    // plumbing; they exercise the exact container shapes/identity the serializer
    // sees at runtime). ---

    fn arr(v: Vec<Value>) -> Value {
        Value::Array(ArrayCell::new(v))
    }
    fn obj(entries: &[(&str, Value)]) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in entries {
            m.insert(k.to_string(), v.clone());
        }
        Value::Object(ObjectCell::new(m))
    }
    fn map(entries: Vec<(Value, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in entries {
            m.insert(MapKey::from_value(&k).unwrap(), v);
        }
        Value::Map(MapCell::new(m))
    }
    fn set(elems: Vec<Value>) -> Value {
        let mut s = IndexSet::new();
        for e in elems {
            s.insert(MapKey::from_value(&e).unwrap());
        }
        Value::Set(SetCell::new(s))
    }
    fn num(n: f64) -> Value {
        Value::Float(n)
    }
    fn str(s: &str) -> Value {
        Value::Str(s.into())
    }

    fn rt(v: &Value) -> Value {
        let interp = Interp::new();
        let (bytes, shared) = encode(v).unwrap();
        decode_with_shared(&bytes, &shared, &interp).unwrap()
    }

    #[test]
    fn roundtrips_scalars() {
        use rust_decimal::prelude::*;
        for v in [
            Value::Nil,
            Value::Bool(true),
            Value::Float(3.5),
            Value::Str("hi".into()),
            Value::Decimal(Decimal::from_str("0.1").unwrap()),
        ] {
            assert_eq!(rt(&v), v);
        }
    }

    #[test]
    fn roundtrips_array_object_map_set() {
        // [1, 2, [3, #{"k": 4}]]
        let src = arr(vec![
            num(1.0),
            num(2.0),
            arr(vec![num(3.0), map(vec![(str("k"), num(4.0))])]),
        ]);
        assert_eq!(format!("{}", rt(&src)), format!("{}", src));
        // {a: 1, b: [2, 3]}
        let o = obj(&[("a", num(1.0)), ("b", arr(vec![num(2.0), num(3.0)]))]);
        assert_eq!(format!("{}", rt(&o)), format!("{}", o));
        // set([1, 2, 3])
        let s = set(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(format!("{}", rt(&s)), format!("{}", s));
    }

    #[test]
    fn map_key_canonicalization_preserved() {
        // -0.0 and +0.0 collapse to one key; the last value wins ("b").
        let m = map(vec![(num(-0.0), str("a")), (num(0.0), str("b"))]);
        // The source itself is already a single entry (MapKey canonicalizes on insert).
        let back = rt(&m);
        if let Value::Map(map) = &back {
            assert_eq!(map.borrow().len(), 1);
            let v = map.borrow().get(&MapKey::from_value(&num(0.0)).unwrap()).cloned();
            assert_eq!(v, Some(str("b")));
        } else {
            panic!("expected map");
        }
    }

    #[test]
    fn bytes_roundtrip() {
        let b = Value::Bytes(Rc::new(RefCell::new(vec![1u8, 2, 3, 255])));
        let back = rt(&b);
        if let Value::Bytes(bb) = &back {
            assert_eq!(&*bb.borrow(), &[1u8, 2, 3, 255]);
        } else {
            panic!("expected bytes");
        }
    }

    #[test]
    fn cycles_are_handled() {
        // a = []; a.push(a) — a self-referential array must encode without infinite
        // recursion and decode into a value that is its own first element.
        let a = ArrayCell::new(Vec::new());
        a.borrow_mut().push(Value::Array(a.clone()));
        let back = rt(&Value::Array(a));
        if let Value::Array(arr) = &back {
            let inner = arr.borrow()[0].clone();
            assert!(
                matches!(&inner, Value::Array(inner) if crate::gc::cc_ptr_eq(arr, inner)),
                "decoded array's first element must be the array itself"
            );
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn object_cycle_roundtrips() {
        // An object referring to itself through a field.
        let o = ObjectCell::new(IndexMap::new());
        o.borrow_mut()
            .insert("self".to_string(), Value::Object(o.clone()));
        let back = rt(&Value::Object(o));
        if let Value::Object(obj) = &back {
            let inner = obj.borrow().get("self").cloned().unwrap();
            assert!(matches!(&inner, Value::Object(i) if crate::gc::cc_ptr_eq(obj, i)));
        } else {
            panic!("expected object");
        }
    }

    #[test]
    fn class_instance_reconstructs_by_identity_and_fields() {
        // Build a `P { x, y }` instance directly. The instance round-trips by class
        // name + cloned fields. (Same-interp here; cross-isolate class identity is
        // guaranteed later by code-shipping.)
        let class = Rc::new(crate::value::Class {
            name: "P".to_string(),
            superclass: None,
            fields: IndexMap::new(),
            methods: IndexMap::new(),
            static_methods: IndexMap::new(),
            def_env: crate::interp::global_env(),
            is_worker: false,
        });
        let mut fields = IndexMap::new();
        fields.insert("x".to_string(), num(1.0));
        fields.insert("y".to_string(), num(2.0));
        let inst = Value::Instance(gcmodule::Cc::new(RefCell::new(crate::value::Instance {
            class,
            fields,
            shape_id: std::cell::Cell::new(0),
            frozen: std::cell::Cell::new(false),
        })));
        let back = rt(&inst);
        assert_eq!(format!("{back}"), format!("{inst}"));
        if let Value::Instance(i) = &back {
            let b = i.borrow();
            assert_eq!(b.class.name, "P");
            assert_eq!(b.fields.get("x"), Some(&num(1.0)));
            assert_eq!(b.fields.get("y"), Some(&num(2.0)));
        } else {
            panic!("expected instance");
        }
    }

    #[test]
    fn enum_variant_roundtrips() {
        let v = Value::EnumVariant(Rc::new(crate::value::EnumVariant {
            enum_name: "Color".to_string(),
            name: "Green".to_string(),
            value: num(1.0),
            payload: None,
            ctor: false,
        def: None,
        }));
        let back = rt(&v);
        assert_eq!(format!("{back}"), format!("{v}"));
        if let Value::EnumVariant(ev) = &back {
            assert_eq!(ev.enum_name, "Color");
            assert_eq!(ev.name, "Green");
        } else {
            panic!("expected enum variant");
        }
    }

    // ─────────────────────────── ADT (Task 9) ───────────────────────────

    fn pos_var(en: &str, name: &str, items: Vec<Value>) -> Value {
        Value::EnumVariant(Rc::new(crate::value::EnumVariant {
            enum_name: en.to_string(),
            name: name.to_string(),
            value: Value::Nil,
            payload: Some(crate::value::Payload::Positional(
                crate::value::ArrayCell::new(items),
            )),
            ctor: false,
            def: None,
        }))
    }
    fn named_var(en: &str, name: &str, fields: Vec<(&str, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in fields {
            m.insert(k.to_string(), v);
        }
        Value::EnumVariant(Rc::new(crate::value::EnumVariant {
            enum_name: en.to_string(),
            name: name.to_string(),
            value: Value::Nil,
            payload: Some(crate::value::Payload::Named(crate::value::ObjectCell::new(m))),
            ctor: false,
            def: None,
        }))
    }

    #[test]
    fn adt_positional_payload_roundtrips_structurally() {
        let v = pos_var("Shape", "Pair", vec![Value::Int(3), Value::Int(4)]);
        let back = rt(&v);
        // Structural equality holds after the round-trip (fresh allocation, same data).
        assert_eq!(back, v);
        assert_eq!(format!("{back}"), "Shape.Pair(3, 4)");
    }

    #[test]
    fn adt_named_payload_roundtrips_structurally() {
        let v = named_var("Shape", "Circle", vec![("radius", Value::Float(2.0))]);
        let back = rt(&v);
        assert_eq!(back, v);
        assert_eq!(format!("{back}"), "Shape.Circle(radius: 2.0)");
    }

    #[test]
    fn adt_payload_variant_nested_in_object_and_map() {
        // A payload variant nested as an Object field and a Map value round-trips.
        let circle = named_var("Shape", "Circle", vec![("radius", Value::Float(1.5))]);
        let mut obj = IndexMap::new();
        obj.insert("shape".to_string(), circle.clone());
        let o = Value::Object(crate::value::ObjectCell::new(obj));
        let back = rt(&o);
        // Objects are identity-equal containers, so compare the nested variant (which
        // IS structural). The decoded object must hold an equal `Circle(radius: 1.5)`.
        if let Value::Object(ob) = &back {
            let inner = ob.borrow().get("shape").cloned().expect("shape field");
            assert_eq!(inner, circle);
        } else {
            panic!("expected an object");
        }
    }

    #[test]
    fn adt_cyclic_recursive_payload_roundtrips() {
        // arr = []; v = Json.Arr([arr]); arr.push(v) → a cycle through the payload.
        // The encoder's visited-ref table must serialize it once and refer by id.
        let arr = crate::value::ArrayCell::new(Vec::new());
        let v = Value::EnumVariant(Rc::new(crate::value::EnumVariant {
            enum_name: "Json".to_string(),
            name: "Arr".to_string(),
            value: Value::Nil,
            payload: Some(crate::value::Payload::Positional(arr.clone())),
            ctor: false,
            def: None,
        }));
        arr.borrow_mut().push(v.clone());
        // Encoding must terminate (cycle handled) and decode to a live cyclic value.
        let (bytes, _shared) = encode(&v).expect("cyclic payload encodes");
        let interp = Interp::new();
        let back = decode(&bytes, &interp).expect("cyclic payload decodes");
        // The decoded variant carries a positional payload whose first element is the
        // variant itself (structural identity → the back-edge is present).
        if let Value::EnumVariant(ev) = &back {
            match &ev.payload {
                Some(crate::value::Payload::Positional(a)) => {
                    assert_eq!(a.borrow().len(), 1);
                }
                _ => panic!("expected a positional payload"),
            }
        } else {
            panic!("expected an enum variant");
        }
    }

    #[test]
    fn adt_non_sendable_payload_is_a_path_error() {
        // A payload holding a non-sendable kind (a function) is the recoverable
        // path-error, with the path threaded through the payload.
        let nonsend = Value::Builtin("print".into());
        let v = pos_var("E", "V", vec![Value::Int(1), nonsend]);
        let err = check_sendable(&v).expect_err("a function payload is not sendable");
        assert_eq!(err.kind, "function");
        assert!(
            err.path.contains(".payload.items[1]"),
            "path should thread through the payload, got: {}",
            err.path
        );
    }

    #[cfg(feature = "data")]
    #[test]
    fn regex_recompiles_from_source() {
        let re = regex::Regex::new("(?i)ab+c").unwrap();
        let v = Value::Regex(Rc::new(crate::value::RegexHandle {
            re,
            source: "(?i)ab+c".to_string(),
        }));
        let back = rt(&v);
        if let Value::Regex(r) = &back {
            assert_eq!(r.source, "(?i)ab+c");
            assert!(r.re.is_match("ABBC"));
        } else {
            panic!("expected regex");
        }
    }

    #[test]
    fn rejects_function_with_field_path() {
        // [1, {cb: <function>}]
        let func = Value::Function(Rc::new(crate::value::Function {
            name: None,
            params: Vec::new(),
            ret: None,
            body: Vec::new(),
            closure: crate::interp::global_env(),
            is_async: false,
            is_generator: false,
            is_worker: false,
        }));
        let v = arr(vec![num(1.0), obj(&[("cb", func)])]);
        let err = check_sendable(&v).unwrap_err();
        assert_eq!(err.kind, "function");
        assert_eq!(err.path, "[1].cb");
        assert!(err
            .message()
            .contains("cannot be sent to a worker at [1].cb"));
    }

    #[test]
    fn rejects_interface_value_with_field_path() {
        // IFACE (C5): an interface descriptor is CODE — a non-sendable VALUE. The
        // serializer must reject it with a field path so `encode_value`'s catch-all
        // `unreachable!` is never reached. (Interfaces cross isolates via code-shipping,
        // Task 8 — not the value channel.)
        let iface = Value::Interface(Rc::new(crate::value::InterfaceDef {
            name: "Reader".to_string(),
            own_methods: IndexMap::new(),
            extends: Vec::new(),
            def_env: crate::interp::global_env(),
            flat: std::cell::RefCell::new(None),
        }));
        // Directly:
        let err = check_sendable(&iface).unwrap_err();
        assert_eq!(err.kind, "interface");
        // Nested under a field path:
        let v = arr(vec![num(1.0), obj(&[("r", iface)])]);
        let err = check_sendable(&v).unwrap_err();
        assert_eq!(err.kind, "interface");
        assert_eq!(err.path, "[1].r");
        // `encode` also rejects before writing any bytes (the unreachable! stays dead).
        let iface2 = Value::Interface(Rc::new(crate::value::InterfaceDef {
            name: "Reader".to_string(),
            own_methods: IndexMap::new(),
            extends: Vec::new(),
            def_env: crate::interp::global_env(),
            flat: std::cell::RefCell::new(None),
        }));
        assert!(encode(&iface2).is_err());
    }

    #[test]
    fn rejects_future_and_native() {
        let fut = Value::Future(crate::task::SharedFuture::resolved(Ok(num(1.0))));
        assert_eq!(check_sendable(&fut).unwrap_err().kind, "future");
    }

    #[test]
    fn rejects_native_channel_with_hint() {
        let native = Value::Native(Rc::new(crate::value::NativeObject {
            id: 1,
            kind: crate::value::NativeKind::Channel,
            fields: IndexMap::new(),
        }));
        let err = check_sendable(&native).unwrap_err();
        assert_eq!(err.kind, "native");
        assert!(err.message().contains("isolate-local"));
    }

    #[test]
    fn encode_rejects_before_writing() {
        let func = Value::Function(Rc::new(crate::value::Function {
            name: None,
            params: Vec::new(),
            ret: None,
            body: Vec::new(),
            closure: crate::interp::global_env(),
            is_async: false,
            is_generator: false,
            is_worker: false,
        }));
        assert!(encode(&func).is_err());
    }

    // --- Adversarial decode: `decode` runs on bytes from another isolate, so it
    // must NEVER panic and NEVER OOM on corrupt input — only return `Err`. ---

    #[test]
    fn decode_rejects_truncated_buffer() {
        // A valid array payload cut short mid-element.
        let (full, _shared) = encode(&arr(vec![num(1.0), num(2.0), num(3.0)])).unwrap();
        for cut in 1..full.len() {
            let interp = Interp::new();
            // Truncating anywhere inside must Err, never panic.
            assert!(
                decode(&full[..cut], &interp).is_err(),
                "truncation at {cut} should be Err"
            );
        }
        // Empty buffer too.
        assert!(decode(&[], &Interp::new()).is_err());
    }

    #[test]
    fn decode_rejects_unknown_tag() {
        assert!(decode(&[99], &Interp::new()).is_err());
        assert!(decode(&[200, 0, 0, 0, 0], &Interp::new()).is_err());
    }

    #[test]
    fn decode_rejects_dangling_ref() {
        // TAG_REF (13) + id=7, but no container with id 7 was ever registered.
        let mut bytes = vec![TAG_REF];
        bytes.extend_from_slice(&7u32.to_le_bytes());
        assert!(decode(&bytes, &Interp::new()).is_err());
    }

    #[test]
    fn decode_huge_length_does_not_allocate() {
        // Regression for the bounded-reservation fix: TAG_ARRAY + id=0 + len=u32::MAX
        // with NO element bytes. Must return Err quickly without a giant allocation
        // (the reservation is bounded by `remaining()`, and the first element read
        // hits end-of-buffer).
        let mut bytes = vec![TAG_ARRAY];
        bytes.extend_from_slice(&0u32.to_le_bytes()); // serial id
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // claimed length
        assert!(decode(&bytes, &Interp::new()).is_err());

        // Same shape for Bytes and Str (their length-prefixed `bytes()` reader is
        // bounds-checked before copying, so a bogus length Errs without allocating).
        let mut b2 = vec![TAG_BYTES];
        b2.extend_from_slice(&0u32.to_le_bytes());
        b2.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&b2, &Interp::new()).is_err());

        let mut b3 = vec![TAG_STR];
        b3.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&b3, &Interp::new()).is_err());
    }

    // ---- SRV Task 6: the airlock zero-copy path (b) ----

    fn shared_val() -> Value {
        use crate::value::SharedNode;
        use std::sync::Arc;
        Value::Shared(Arc::new(SharedNode::Object(Arc::new(vec![
            ("k".into(), Arc::new(SharedNode::Int(42))),
        ]))))
    }

    #[test]
    fn shared_is_a_sendable_leaf() {
        // `unsendable_kind` says sendable; `check_inner` treats it as a LEAF (no
        // recursion into the frozen graph).
        let s = shared_val();
        assert!(unsendable_kind(&s).is_none());
        assert!(check_sendable(&s).is_ok());
    }

    #[test]
    fn shared_round_trips_by_arc_bump_not_copy() {
        let s = shared_val();
        let arc_before = match &s {
            Value::Shared(a) => a.clone(),
            _ => unreachable!(),
        };
        // Encode: one TAG_SHARED + index; the side-vector holds the SAME Arc (no walk).
        let (bytes, shared) = encode(&s).unwrap();
        assert_eq!(shared.len(), 1, "exactly one Arc in the side-vector");
        assert!(std::sync::Arc::ptr_eq(&shared[0], &arc_before), "Arc preserved (no copy)");
        // The byte buffer is TINY (a tag + a u32) — NOT the size of the frozen graph.
        assert_eq!(bytes.len(), 1 + 4, "TAG_SHARED + u32 index only");
        // Decode (same-thread inline path): the OUTGOING Arc pointer is PRESERVED.
        let interp = Interp::new();
        let back = decode_with_shared(&bytes, &shared, &interp).unwrap();
        match back {
            Value::Shared(a) => assert!(
                std::sync::Arc::ptr_eq(&a, &arc_before),
                "inline path preserves Arc identity (zero copy)"
            ),
            _ => panic!("expected Shared"),
        }
    }

    #[test]
    fn shared_nested_in_object_crosses() {
        // A Shared nested inside a normal sendable object round-trips correctly.
        let mut m = IndexMap::new();
        m.insert("cfg".to_string(), shared_val());
        m.insert("n".to_string(), Value::Int(1));
        let obj = Value::Object(ObjectCell::new(m));
        let (bytes, shared) = encode(&obj).unwrap();
        assert_eq!(shared.len(), 1);
        let interp = Interp::new();
        let back = decode_with_shared(&bytes, &shared, &interp).unwrap();
        match back {
            Value::Object(o) => {
                let b = o.borrow();
                assert!(matches!(b.get("cfg"), Some(Value::Shared(_))));
                assert_eq!(b.get("n"), Some(&Value::Int(1)));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn decode_shared_out_of_range_index_errors() {
        // A TAG_SHARED with an index past the (empty) side-vector → clean Err, no panic.
        let mut b = vec![TAG_SHARED];
        b.extend_from_slice(&0u32.to_le_bytes());
        assert!(decode_with_shared(&b, &[], &Interp::new()).is_err());
    }
}
