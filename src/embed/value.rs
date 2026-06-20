//! `AsValue` — the value bridge across the embedding boundary (spec §5).
//!
//! **Unit A scope (this file at this stage):** the `!Send` newtype over `Value` plus
//! the scalar constructors/accessors `eval`/`call`/`global` need. Unit B (a later
//! task) adds container handles, the kind table, and the JSON/serde deep bridge.

use crate::value::{Value, ValueKind};

/// A value crossing the host ⇄ script boundary (spec §5.1).
///
/// A newtype over the engine's `Value`. Because every AScript container is already
/// `Rc`/`Cc`-backed shared state, a clone of the `Value` IS a live handle — but
/// scalars and strings cross by value. `!Send + !Sync` by construction (it holds a
/// `Value`, which is asserted `!Send`).
#[derive(Clone)]
pub struct AsValue(pub(crate) Value);

impl AsValue {
    /// The `nil` value.
    pub fn nil() -> Self {
        AsValue(Value::nil())
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

    /// The engine's stable type name for this value (`"nil"`, `"int"`, `"object"`, …).
    pub fn type_name(&self) -> &'static str {
        crate::interp::type_name(&self.0)
    }

    /// The underlying `Value` (crate-internal — the engine side of the boundary).
    /// Used by `call`/`set_global` arg marshalling in Task 1.3.
    #[allow(dead_code)]
    pub(crate) fn into_value(self) -> Value {
        self.0
    }

    /// Borrow the underlying `Value` (crate-internal).
    /// Used by `call_value`/`set_global` in Task 1.3.
    #[allow(dead_code)]
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
