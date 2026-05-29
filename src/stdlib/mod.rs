//! The AScript standard library: `std/*` modules implemented as native Rust
//! over the existing `Value` model. Each module exposes an `exports()` binding
//! list (imported names → `Value`) and a `call` entry the interpreter routes
//! qualified builtin names (`"math.abs"`) to. Per spec §11.3, native functions
//! are ordinary `function` values; argument-type misuse is a Tier-2 panic.

pub mod array;
pub mod convert;
pub mod map;
pub mod math;
pub mod object;
pub mod string;

use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use std::rc::Rc;

/// A native builtin value with a qualified name (`"math.abs"`).
pub(crate) fn bi(qualified: &str) -> Value {
    Value::Builtin(qualified.into())
}

/// The export list (binding name → value) for a `std/*` module path, or `None`
/// if `path` is not a known stdlib module.
pub fn std_module_exports(path: &str) -> Option<Vec<(String, Value)>> {
    let list: Vec<(&'static str, Value)> = match path {
        "std/math" => math::exports(),
        "std/string" => string::exports(),
        "std/array" => array::exports(),
        "std/object" => object::exports(),
        "std/map" => map::exports(),
        "std/convert" => convert::exports(),
        _ => return None,
    };
    Some(list.into_iter().map(|(n, v)| (n.to_string(), v)).collect())
}

impl Interp {
    /// Dispatch a qualified stdlib builtin (`module` = "math", `func` = "abs").
    pub(crate) async fn call_stdlib(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match module {
            "math" => math::call(func, args, span),
            "string" => string::call(func, args, span),
            "array" => self.call_array(func, args, span).await,
            "object" => object::call(func, args, span),
            "map" => map::call(func, args, span),
            "convert" => convert::call(func, args, span),
            _ => Err(AsError::at(format!("unknown stdlib module '{}'", module), span).into()),
        }
    }
}

// ---- shared argument helpers (Tier-2 panic on type misuse, spec §11.3) ----

pub(crate) fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Nil)
}

pub(crate) fn want_number(v: &Value, span: Span, ctx: &str) -> Result<f64, Control> {
    match v {
        Value::Number(n) => Ok(*n),
        _ => Err(AsError::at(format!("{} expects a number, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

pub(crate) fn want_string(v: &Value, span: Span, ctx: &str) -> Result<Rc<str>, Control> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        _ => Err(AsError::at(format!("{} expects a string, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

pub(crate) fn want_array(v: &Value, span: Span, ctx: &str) -> Result<Rc<std::cell::RefCell<Vec<Value>>>, Control> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        _ => Err(AsError::at(format!("{} expects an array, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

// want_object: used by the std/object module; the type-error message shape is
// defined here so all std modules stay consistent.
pub(crate) fn want_object(v: &Value, span: Span, ctx: &str) -> Result<Rc<std::cell::RefCell<indexmap::IndexMap<String, Value>>>, Control> {
    match v {
        Value::Object(o) => Ok(o.clone()),
        _ => Err(AsError::at(format!("{} expects an object, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

/// Resolve a possibly-negative index against a length, clamping into `0..=len`.
/// Negative counts from the end. Fractional inputs truncate toward zero (e.g.
/// `slice(1.9)` → index 1). Used by string/array `slice`.
pub(crate) fn clamp_index(i: f64, len: usize) -> usize {
    if i < 0.0 {
        let from_end = len as f64 + i;
        if from_end < 0.0 { 0 } else { from_end as usize }
    } else if i as usize > len {
        len
    } else {
        i as usize
    }
}
