//! `AsValue` — the value bridge across the embedding boundary (spec §5).
//!
//! Scalars cross **by value**, containers cross **by handle** (an `Rc`/`Cc` clone of
//! the *same* cell the script holds — aliasing + identity preserved, §5.1). The deep
//! conversion is the *explicit* JSON/serde bridge (`to_json`/`Isolate::json_parse`),
//! never an implicit walk.

use crate::value::{Value, ValueKind};

/// A coarse classification of an [`AsValue`]'s crossing class (spec §5.2).
///
/// This is the engine's runtime kind, projected onto the host vocabulary: value kinds
/// (cross by value), live aliasing handles (`Array`/`Object`/`Map`/`Set`/`Bytes`),
/// `Callable` (a function/closure invokable via [`Isolate::call_value`]), `Future`
/// (auto-awaited by `Isolate::call*`), and `Opaque` (everything else — passed back into
/// the engine unchanged, identity preserved).
///
/// [`Isolate::call_value`]: crate::embed::Isolate::call_value
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AsKind {
    /// `nil`.
    Nil,
    /// `bool`.
    Bool,
    /// `int` (`i64`).
    Int,
    /// `float` (`f64`).
    Float,
    /// Exact `decimal` (crosses by display string — [`AsValue::as_decimal_str`]).
    Decimal,
    /// `string`.
    Str,
    /// `array` — a live aliasing handle.
    Array,
    /// `object` — a live aliasing handle.
    Object,
    /// `map` — a live aliasing handle (read-only host-side; constructed script-side).
    Map,
    /// `set` — a live aliasing handle (read-only host-side; constructed script-side).
    Set,
    /// `bytes` — a live aliasing handle.
    Bytes,
    /// A callable (function/closure/builtin/bound-method/class-method/enum-ctor):
    /// invokable via [`Isolate::call_value`].
    ///
    /// [`Isolate::call_value`]: crate::embed::Isolate::call_value
    Callable,
    /// A `future<T>` — auto-awaited by `Isolate::call*`; a held handle keeps the task
    /// alive (cancel-on-drop preserved).
    Future,
    /// Any other runtime kind (generator/native/class/enum/instance/interface/regex/
    /// shared/…): an opaque, pass-back-able handle.
    Opaque,
}

/// A value crossing the host ⇄ script boundary (spec §5.1).
///
/// A newtype over the engine's `Value`. Because every AScript container is already
/// `Rc`/`Cc`-backed shared state, **a clone of the `Value` IS a live handle** — host
/// writes are visible to the script and vice versa, identity (`==` for identity-equal
/// kinds) is preserved, and crossing the boundary costs one refcount bump. Scalars and
/// strings cross by value. `!Send + !Sync` by construction (it holds a `Value`, which is
/// asserted `!Send`).
#[derive(Clone)]
pub struct AsValue(pub(crate) Value);

impl AsValue {
    /// The `nil` value.
    pub fn nil() -> Self {
        AsValue(Value::nil())
    }

    /// Construct an exact `decimal` from its display string (lossless; scale preserved,
    /// e.g. `"1.50"`). Returns an error string if `s` is not a valid decimal literal.
    pub fn decimal(s: &str) -> Result<Self, String> {
        use std::str::FromStr;
        rust_decimal::Decimal::from_str(s)
            .map(|d| AsValue(Value::decimal(d)))
            .map_err(|e| format!("invalid decimal '{s}': {e}"))
    }

    /// Construct a host-side `bytes` handle from a byte vector.
    pub fn bytes(b: Vec<u8>) -> Self {
        AsValue(Value::bytes(b))
    }

    /// Construct a host-side `array` from a vector of values (a live handle the script
    /// can then alias once it crosses in).
    pub fn array(items: Vec<AsValue>) -> Self {
        let vec: Vec<Value> = items.into_iter().map(AsValue::into_value).collect();
        AsValue(Value::array_cell(crate::value::ArrayCell::new(vec)))
    }

    /// Construct a host-side `object` from `(key, value)` pairs (later-key-wins;
    /// insertion-ordered, a live handle).
    pub fn object(entries: Vec<(String, AsValue)>) -> Self {
        let mut m = indexmap::IndexMap::new();
        for (k, v) in entries {
            m.insert(k, v.into_value());
        }
        AsValue(Value::object_cell(crate::value::ObjectCell::new(m)))
    }

    /// This value's crossing-class [`AsKind`] (spec §5.2).
    pub fn kind(&self) -> AsKind {
        match self.0.kind() {
            ValueKind::Nil => AsKind::Nil,
            ValueKind::Bool(_) => AsKind::Bool,
            ValueKind::Int(_) => AsKind::Int,
            ValueKind::Float(_) => AsKind::Float,
            ValueKind::Decimal(_) => AsKind::Decimal,
            ValueKind::Str(_) => AsKind::Str,
            ValueKind::Array(_) => AsKind::Array,
            ValueKind::Object(_) => AsKind::Object,
            ValueKind::Map(_) => AsKind::Map,
            ValueKind::Set(_) => AsKind::Set,
            ValueKind::Bytes(_) => AsKind::Bytes,
            // Callables: every kind invokable via `call_value`.
            ValueKind::Function(_)
            | ValueKind::Closure(_)
            | ValueKind::Builtin(_)
            | ValueKind::BoundMethod(_)
            | ValueKind::ClassMethod(_) => AsKind::Callable,
            // An enum variant is a callable ONLY when it is a payload-constructor; a
            // constructed/unit variant is an opaque value. The engine encodes this on
            // the variant; treat the bare variant as opaque (it round-trips identically)
            // and reserve `Callable` for the value kinds that are unambiguously fns.
            ValueKind::Future(_) => AsKind::Future,
            _ => AsKind::Opaque,
        }
    }

    /// Is this `nil`?
    pub fn is_nil(&self) -> bool {
        matches!(self.0.kind(), ValueKind::Nil)
    }

    /// The integer value, if this is an `int`.
    pub fn as_int(&self) -> Option<i64> {
        self.0.as_int()
    }

    /// The float value, if this is a `float`.
    pub fn as_float(&self) -> Option<f64> {
        self.0.as_float()
    }

    /// The bool value, if this is a `bool`.
    pub fn as_bool(&self) -> Option<bool> {
        match self.0.kind() {
            ValueKind::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// The string slice, if this is a `string` (borrows the underlying `Rc<str>`).
    pub fn as_str(&self) -> Option<&str> {
        self.0.as_str()
    }

    /// The canonical display string of a `decimal` (lossless, scale-preserving — e.g.
    /// `"1.50"`), or `None` if this is not a decimal. The inverse of [`AsValue::decimal`].
    pub fn as_decimal_str(&self) -> Option<String> {
        match self.0.kind() {
            ValueKind::Decimal(d) => Some(d.to_string()),
            _ => None,
        }
    }

    /// `true` if this value is callable (invokable via
    /// [`Isolate::call_value`](crate::embed::Isolate::call_value)).
    pub fn is_callable(&self) -> bool {
        matches!(self.kind(), AsKind::Callable)
    }

    /// The engine's stable type name for this value (`"nil"`, `"int"`, `"object"`, …).
    ///
    /// Delegates to the engine's `type_name` — the single source of truth; the host
    /// vocabulary is NOT re-spelled here.
    pub fn type_name(&self) -> &'static str {
        crate::interp::type_name(&self.0)
    }

    // ── container handles (LIVE aliasing) ───────────────────────────────────
    //
    // Container `AsValue`s wrap the SAME `Rc`/`Cc` cell the script holds, so a host
    // read/write IS a script read/write (and vice-versa) with no copy — the §5.1
    // Lua-table model. Reads use the slab-safe sealed accessors (never the panicking
    // `ObjectCell::borrow()` shim). Mutators route through the SAME engine helpers the
    // stdlib/VM use so type contracts + the frozen-`Shared` guard + slab-shape
    // bookkeeping are honored, never bypassed (§5.2).

    /// The element/entry count for a container (`array`/`object`/`map`/`set`/`bytes`),
    /// or `None` for a non-container.
    pub fn len(&self) -> Option<usize> {
        match self.0.kind() {
            ValueKind::Array(a) => Some(a.borrow().len()),
            ValueKind::Object(o) => Some(o.len()),
            ValueKind::Map(m) => Some(m.map.borrow().len()),
            ValueKind::Set(s) => Some(s.set.borrow().len()),
            ValueKind::Bytes(b) => Some(b.borrow().len()),
            _ => None,
        }
    }

    /// `true` if this is an empty container; `None` for a non-container.
    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|n| n == 0)
    }

    /// Read element `i` of an `array` (or a `bytes` element as an `int`); `None` for an
    /// out-of-range index or a non-indexable value.
    pub fn get(&self, i: usize) -> Option<AsValue> {
        match self.0.kind() {
            ValueKind::Array(a) => a.borrow().get(i).cloned().map(AsValue),
            ValueKind::Bytes(b) => b.borrow().get(i).map(|&byte| AsValue::from(byte as i64)),
            _ => None,
        }
    }

    /// Read the value at string `key` of an `object` (slab-safe), or a string-keyed
    /// `map` entry; `None` for a missing key or a non-keyed value.
    pub fn get_key(&self, key: &str) -> Option<AsValue> {
        match self.0.kind() {
            ValueKind::Object(o) => o.get(key).map(AsValue),
            ValueKind::Map(m) => {
                let mk = crate::value::MapKey::from_value(&Value::str(key))?;
                m.map.borrow().get(&mk).cloned().map(AsValue)
            }
            _ => None,
        }
    }

    /// Write `value` to element `i` of an `array`. Routes through the engine's
    /// `index_set` (frozen guard + bounds check preserved); a host error surfaces as
    /// [`EmbedError`].
    ///
    /// `map`/`set` are READ-ONLY host-side (their `MapKey` canonicalization stays
    /// engine-owned — construct them script-side, §5.2).
    pub fn set(&self, i: usize, value: AsValue) -> Result<(), crate::embed::EmbedError> {
        use crate::span::Span;
        match self.0.kind() {
            ValueKind::Array(_) => {
                crate::interp::index_set(
                    &self.0,
                    &Value::int(i as i64),
                    value.into_value(),
                    Span::new(0, 0),
                    Span::new(0, 0),
                )
                .map(|_| ())
                .map_err(|e| crate::embed::EmbedError::from_panic(&e))
            }
            _ => Err(crate::embed::EmbedError::Config(format!(
                "set(index) is only valid on an array, got {}",
                self.type_name()
            ))),
        }
    }

    /// Write `value` at string `key` of an `object`. Routes through the SAME engine
    /// write the stdlib/VM use: the frozen guard (so a frozen `Object`/`Shared`
    /// receiver surfaces the engine's `cannot mutate a frozen …` panic as
    /// [`EmbedError::Panic`]) and the slab-safe sealed `ObjectCell::insert` accessor
    /// (so slab-shape bookkeeping + the SHAPE delete-bug invariant hold — never the
    /// panicking `borrow_mut()` shim).
    ///
    /// `map`/`set` are READ-ONLY host-side (§5.2).
    ///
    /// [`EmbedError::Panic`]: crate::embed::EmbedError::Panic
    pub fn set_key(&self, key: &str, value: AsValue) -> Result<(), crate::embed::EmbedError> {
        use crate::span::Span;
        // The frozen guard FIRST — identical to the engine's `check_not_frozen` /
        // `index_set` chokepoint, so a frozen `Object` OR a frozen `Shared` (whose
        // `frozen_kind` reports its underlying container kind) surfaces the byte-
        // identical `cannot mutate a frozen {kind}` panic, NOT a bypass.
        if let Some(kind) = crate::value::frozen_kind(&self.0) {
            let err = crate::error::AsError::at(
                format!("cannot mutate a frozen {kind}"),
                Span::new(0, 0),
            );
            return Err(crate::embed::EmbedError::from_panic(&err));
        }
        match self.0.kind() {
            ValueKind::Object(o) => {
                // The sealed slab-safe accessor (NOT `borrow_mut()`): a new key on a
                // slab demotes to dict + resets shape 0 internally, an existing key
                // updates in place — the SHAPE invariant.
                o.insert(key, value.into_value());
                Ok(())
            }
            _ => Err(crate::embed::EmbedError::Config(format!(
                "set_key is only valid on an object, got {}",
                self.type_name()
            ))),
        }
    }

    /// A snapshot of an `array`'s elements (or a `set`'s values in insertion order);
    /// empty for a non-list value. The snapshot is a `Vec` of `Rc`-bumped handles — it
    /// does NOT alias subsequent script mutations of the container's length.
    pub fn items(&self) -> Vec<AsValue> {
        match self.0.kind() {
            ValueKind::Array(a) => a.borrow().iter().cloned().map(AsValue).collect(),
            ValueKind::Set(s) => s.set.borrow().iter().map(|k| AsValue(k.to_value())).collect(),
            ValueKind::Bytes(b) => b
                .borrow()
                .iter()
                .map(|&byte| AsValue::from(byte as i64))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A snapshot of an `object`'s `(key, value)` pairs in insertion order (or a
    /// `map`'s string-keyed pairs; non-string map keys are skipped); empty for a
    /// non-keyed value.
    pub fn entries(&self) -> Vec<(String, AsValue)> {
        match self.0.kind() {
            ValueKind::Object(o) => o
                .entries()
                .into_iter()
                .map(|(k, v)| (k.to_string(), AsValue(v)))
                .collect(),
            ValueKind::Map(m) => m
                .map
                .borrow()
                .iter()
                .filter_map(|(k, v)| {
                    k.to_value()
                        .as_str()
                        .map(|s| (s.to_string(), AsValue(v.clone())))
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Copy the bytes out of a `bytes` handle, or `None` for a non-bytes value.
    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        match self.0.kind() {
            ValueKind::Bytes(b) => Some(b.borrow().clone()),
            _ => None,
        }
    }

    /// The underlying `Value` (crate-internal — the engine side of the boundary).
    pub(crate) fn into_value(self) -> Value {
        self.0
    }

    /// Borrow the underlying `Value` (crate-internal).
    pub(crate) fn value(&self) -> &Value {
        &self.0
    }

    /// Wrap an engine `Value` (crate-internal — produced by `eval`/`call`/`global`).
    pub(crate) fn from_value(v: Value) -> Self {
        AsValue(v)
    }
}

impl std::fmt::Debug for AsValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Show the type name + the value's Display form — enough to debug a test
        // failure without leaking the unstable internal `Value` Debug shape.
        write!(f, "AsValue({}: {})", self.type_name(), self.0)
    }
}

impl From<i64> for AsValue {
    fn from(n: i64) -> Self {
        AsValue(Value::int(n))
    }
}

impl From<f64> for AsValue {
    fn from(n: f64) -> Self {
        AsValue(Value::float(n))
    }
}

impl From<bool> for AsValue {
    fn from(b: bool) -> Self {
        AsValue(Value::bool_(b))
    }
}

impl From<&str> for AsValue {
    fn from(s: &str) -> Self {
        AsValue(Value::str(s))
    }
}

impl From<String> for AsValue {
    fn from(s: String) -> Self {
        AsValue(Value::str(s))
    }
}
