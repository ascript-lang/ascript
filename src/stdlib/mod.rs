//! The AScript standard library: `std/*` modules implemented as native Rust
//! over the existing `Value` model. Each module exposes an `exports()` binding
//! list (imported names → `Value`) and a `call` entry the interpreter routes
//! qualified builtin names (`"math.abs"`) to. Per spec §11.3, native functions
//! are ordinary `function` values; argument-type misuse is a Tier-2 panic.

pub mod array;
pub mod bytes;
pub mod convert;
#[cfg(feature = "crypto")]
pub mod crypto;
#[cfg(feature = "datetime")]
pub mod date;
#[cfg(feature = "compress")]
pub mod compress;
#[cfg(feature = "data")]
pub mod csv;
#[cfg(feature = "data")]
pub mod encoding;
#[cfg(feature = "sys")]
pub mod env;
#[cfg(feature = "sys")]
pub mod fs;
#[cfg(feature = "intl")]
pub mod intl;
#[cfg(feature = "data")]
pub mod json;
pub mod map;
pub mod math;
pub mod object;
#[cfg(feature = "data")]
pub mod regex;
pub mod string;
pub mod time;
#[cfg(feature = "data")]
pub mod toml;
#[cfg(feature = "data")]
pub mod uuid;
#[cfg(feature = "data")]
pub mod yaml;

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
        "std/bytes" => bytes::exports(),
        "std/convert" => convert::exports(),
        "std/time" => time::exports(),
        #[cfg(feature = "datetime")]
        "std/date" => date::exports(),
        #[cfg(feature = "intl")]
        "std/intl" => intl::exports(),
        #[cfg(feature = "data")]
        "std/json" => json::exports(),
        #[cfg(feature = "data")]
        "std/encoding" => encoding::exports(),
        #[cfg(feature = "crypto")]
        "std/crypto" => crypto::exports(),
        #[cfg(feature = "compress")]
        "std/compress" => compress::exports(),
        #[cfg(feature = "sys")]
        "std/env" => env::exports(),
        #[cfg(feature = "sys")]
        "std/fs" => fs::exports(),
        #[cfg(feature = "data")]
        "std/regex" => regex::exports(),
        #[cfg(feature = "data")]
        "std/uuid" => uuid::exports(),
        #[cfg(feature = "data")]
        "std/csv" => csv::exports(),
        #[cfg(feature = "data")]
        "std/toml" => toml::exports(),
        #[cfg(feature = "data")]
        "std/yaml" => yaml::exports(),
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
            "bytes" => bytes::call(func, args, span),
            "convert" => convert::call(func, args, span),
            "time" => self.call_time(func, args, span).await,
            #[cfg(feature = "datetime")]
            "date" => date::call(func, args, span),
            #[cfg(feature = "intl")]
            "intl" => intl::call(func, args, span),
            #[cfg(feature = "data")]
            "json" => json::call(func, args, span),
            #[cfg(feature = "data")]
            "encoding" => encoding::call(func, args, span),
            #[cfg(feature = "crypto")]
            "crypto" => crypto::call(func, args, span),
            #[cfg(feature = "compress")]
            "compress" => compress::call(func, args, span),
            #[cfg(feature = "sys")]
            "env" => env::call(func, args, span),
            #[cfg(feature = "sys")]
            "fs" => fs::call(func, args, span),
            #[cfg(feature = "data")]
            "regex" => regex::call(func, args, span),
            #[cfg(feature = "data")]
            "uuid" => uuid::call(func, args, span),
            #[cfg(feature = "data")]
            "csv" => csv::call(func, args, span),
            #[cfg(feature = "data")]
            "toml" => toml::call(func, args, span),
            #[cfg(feature = "data")]
            "yaml" => yaml::call(func, args, span),
            _ => Err(AsError::at(format!("unknown stdlib module '{}'", module), span).into()),
        }
    }

    /// `std/time` dispatch. `sleep` is async (the first async stdlib fn) and is
    /// awaited here on the tokio loop; all other time functions are synchronous
    /// and delegate to `time::call`.
    pub(crate) async fn call_time(
        &mut self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        if func == "sleep" {
            let ms = want_number(&arg(args, 0), span, "time.sleep")?;
            if ms < 0.0 {
                return Err(AsError::at("time.sleep duration must be non-negative", span).into());
            }
            // `ms as u64` truncates toward zero: a fractional `sleep(20.7)`
            // sleeps for 20 whole milliseconds.
            tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
            return Ok(Value::Nil);
        }
        time::call(func, args, span)
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

pub(crate) fn want_bytes(v: &Value, span: Span, ctx: &str) -> Result<Rc<std::cell::RefCell<Vec<u8>>>, Control> {
    match v {
        Value::Bytes(b) => Ok(b.clone()),
        _ => Err(AsError::at(format!("{} expects bytes, got {}", ctx, crate::interp::type_name(v)), span).into()),
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
