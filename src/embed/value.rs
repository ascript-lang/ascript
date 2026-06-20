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
