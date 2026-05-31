//! The AScript standard library: `std/*` modules implemented as native Rust
//! over the existing `Value` model. Each module exposes an `exports()` binding
//! list (imported names → `Value`) and a `call` entry the interpreter routes
//! qualified builtin names (`"math.abs"`) to. Per spec §11.3, native functions
//! are ordinary `function` values; argument-type misuse is a Tier-2 panic.

pub mod array;
pub mod bytes;
pub mod cli;
pub mod color;
#[cfg(feature = "compress")]
pub mod compress;
pub mod convert;
#[cfg(feature = "crypto")]
pub mod crypto;
#[cfg(feature = "data")]
pub mod csv;
#[cfg(feature = "datetime")]
pub mod date;
#[cfg(feature = "data")]
pub mod encoding;
#[cfg(feature = "sys")]
pub mod env;
#[cfg(feature = "sys")]
pub mod fs;
#[cfg(feature = "net")]
pub mod http_server;
#[cfg(feature = "intl")]
pub mod intl;
#[cfg(feature = "sys")]
pub mod io;
#[cfg(feature = "data")]
pub mod json;
#[cfg(feature = "log")]
pub mod log;
pub mod map;
pub mod math;
#[cfg(feature = "net")]
pub mod net_http;
#[cfg(feature = "net")]
pub mod net_tcp;
#[cfg(feature = "net")]
pub mod net_ws;
pub mod object;
#[cfg(feature = "sys")]
pub mod process;
#[cfg(feature = "data")]
pub mod regex;
#[cfg(feature = "sql")]
pub mod sqlite;
pub mod string;
pub mod task_mod;
pub mod time;
#[cfg(feature = "data")]
pub mod toml;
#[cfg(feature = "tui")]
pub mod tui;
#[cfg(feature = "data")]
pub mod url;
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
        "std/cli" => cli::exports(),
        "std/color" => color::exports(),
        "std/math" => math::exports(),
        "std/string" => string::exports(),
        "std/array" => array::exports(),
        "std/object" => object::exports(),
        "std/map" => map::exports(),
        "std/bytes" => bytes::exports(),
        "std/convert" => convert::exports(),
        "std/task" => task_mod::exports(),
        "std/time" => time::exports(),
        #[cfg(feature = "datetime")]
        "std/date" => date::exports(),
        #[cfg(feature = "intl")]
        "std/intl" => intl::exports(),
        #[cfg(feature = "data")]
        "std/json" => json::exports(),
        #[cfg(feature = "log")]
        "std/log" => log::exports(),
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
        #[cfg(feature = "sys")]
        "std/io" => io::exports(),
        #[cfg(feature = "sys")]
        "std/process" => process::exports(),
        #[cfg(feature = "net")]
        "std/net/tcp" => net_tcp::exports(),
        #[cfg(feature = "net")]
        "std/net/http" => net_http::exports(),
        #[cfg(feature = "net")]
        "std/http/server" => http_server::exports(),
        #[cfg(feature = "net")]
        "std/net/ws" => net_ws::exports(),
        #[cfg(feature = "data")]
        "std/regex" => regex::exports(),
        #[cfg(feature = "sql")]
        "std/sqlite" => sqlite::exports(),
        #[cfg(feature = "data")]
        "std/url" => url::exports(),
        #[cfg(feature = "data")]
        "std/uuid" => uuid::exports(),
        #[cfg(feature = "data")]
        "std/csv" => csv::exports(),
        #[cfg(feature = "data")]
        "std/toml" => toml::exports(),
        #[cfg(feature = "data")]
        "std/yaml" => yaml::exports(),
        #[cfg(feature = "tui")]
        "std/tui" => tui::exports(),
        _ => return None,
    };
    Some(list.into_iter().map(|(n, v)| (n.to_string(), v)).collect())
}

impl Interp {
    /// Dispatch a qualified stdlib builtin (`module` = "math", `func` = "abs").
    pub(crate) async fn call_stdlib(
        &self,
        module: &str,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        // Typed parse: json.parse(text, Class) — parse, then validate against the
        // class, fusing a parse failure and a shape mismatch into one Tier-1
        // [val, err] pair. With no class argument this falls through to the normal
        // 1-arg parse below (unchanged behavior).
        #[cfg(feature = "data")]
        if module == "json" && func == "parse" {
            if let Some(Value::Class(c)) = args.get(1) {
                // `json.parse(text, Class, strict?)` — optional trailing bool.
                let strict = matches!(args.get(2), Some(Value::Bool(true)));
                let parsed = json::call(func, &args[..1], span)?; // [val, err]
                if let Value::Array(a) = &parsed {
                    let (val, err) = {
                        let b = a.borrow();
                        (b[0].clone(), b[1].clone())
                    };
                    if err != Value::Nil {
                        return Ok(parsed); // parse error stays in the err channel
                    }
                    return match self.validate_into(c, &val, strict, "", span).await {
                        Ok(inst) => Ok(crate::interp::make_pair(inst, Value::Nil)),
                        Err(e) => Ok(crate::interp::make_pair(
                            Value::Nil,
                            crate::interp::make_error(Value::Str(e.message.into())),
                        )),
                    };
                }
            }
        }
        match module {
            "cli" => self.call_cli(func, args, span).await,
            "color" => color::call(func, args, span),
            "math" => math::call(func, args, span),
            "string" => string::call(func, args, span),
            "array" => self.call_array(func, args, span).await,
            "object" => self.call_object(func, args, span).await,
            "map" => map::call(func, args, span),
            "bytes" => bytes::call(func, args, span),
            "convert" => convert::call(func, args, span),
            "task" => self.call_task(func, args, span).await,
            "time" => self.call_time(func, args, span).await,
            #[cfg(feature = "datetime")]
            "date" => date::call(func, args, span),
            #[cfg(feature = "intl")]
            "intl" => intl::call(func, args, span),
            #[cfg(feature = "data")]
            "json" => json::call(func, args, span),
            #[cfg(feature = "log")]
            "log" => self.call_log(func, args, span).await,
            #[cfg(feature = "data")]
            "encoding" => encoding::call(func, args, span),
            #[cfg(feature = "crypto")]
            "crypto" => crypto::call(func, args, span),
            #[cfg(feature = "compress")]
            "compress" => compress::call(func, args, span),
            #[cfg(feature = "sys")]
            "env" => {
                // `args()` must see the interpreter's stored CLI args, so it is
                // handled here before the pure `env::call` fallthrough.
                if func == "args" {
                    return Ok(self.get_cli_args());
                }
                env::call(func, args, span)
            }
            #[cfg(feature = "sys")]
            "fs" => fs::call(func, args, span),
            #[cfg(feature = "sys")]
            "io" => self.call_io(func, args, span).await,
            #[cfg(feature = "sys")]
            "process" => self.call_process(func, args, span).await,
            #[cfg(feature = "net")]
            "net_tcp" => self.call_net_tcp(func, args, span).await,
            #[cfg(feature = "net")]
            "net_http" => self.call_http(func, args, span).await,
            #[cfg(feature = "net")]
            "http_server" => self.call_http_server(func, args, span).await,
            #[cfg(feature = "net")]
            "net_ws" => self.call_net_ws(func, args, span).await,
            #[cfg(feature = "data")]
            "regex" => regex::call(func, args, span),
            #[cfg(feature = "sql")]
            "sqlite" => self.call_sqlite_open(func, args, span),
            #[cfg(feature = "data")]
            "url" => url::call(func, args, span),
            #[cfg(feature = "data")]
            "uuid" => uuid::call(func, args, span),
            #[cfg(feature = "data")]
            "csv" => csv::call(func, args, span),
            #[cfg(feature = "data")]
            "toml" => toml::call(func, args, span),
            #[cfg(feature = "data")]
            "yaml" => yaml::call(func, args, span),
            #[cfg(feature = "tui")]
            "tui" => self.call_tui(func, args, span),
            _ => Err(AsError::at(format!("unknown stdlib module '{}'", module), span).into()),
        }
    }

    /// `std/time` dispatch. `sleep` is async (the first async stdlib fn) and is
    /// awaited here on the tokio loop; all other time functions are synchronous
    /// and delegate to `time::call`.
    pub(crate) async fn call_time(
        &self,
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
        _ => Err(AsError::at(
            format!(
                "{} expects a number, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

pub(crate) fn want_string(v: &Value, span: Span, ctx: &str) -> Result<Rc<str>, Control> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        _ => Err(AsError::at(
            format!(
                "{} expects a string, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

pub(crate) fn want_array(
    v: &Value,
    span: Span,
    ctx: &str,
) -> Result<Rc<std::cell::RefCell<Vec<Value>>>, Control> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        _ => Err(AsError::at(
            format!(
                "{} expects an array, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

// want_object: used by the std/object module; the type-error message shape is
// defined here so all std modules stay consistent.
pub(crate) fn want_object(
    v: &Value,
    span: Span,
    ctx: &str,
) -> Result<Rc<std::cell::RefCell<indexmap::IndexMap<String, Value>>>, Control> {
    match v {
        Value::Object(o) => Ok(o.clone()),
        _ => Err(AsError::at(
            format!(
                "{} expects an object, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

pub(crate) fn want_bytes(
    v: &Value,
    span: Span,
    ctx: &str,
) -> Result<Rc<std::cell::RefCell<Vec<u8>>>, Control> {
    match v {
        Value::Bytes(b) => Ok(b.clone()),
        _ => Err(AsError::at(
            format!("{} expects bytes, got {}", ctx, crate::interp::type_name(v)),
            span,
        )
        .into()),
    }
}

/// Resolve a possibly-negative index against a length, clamping into `0..=len`.
/// Negative counts from the end. Fractional inputs truncate toward zero (e.g.
/// `slice(1.9)` → index 1). Used by string/array `slice`.
pub(crate) fn clamp_index(i: f64, len: usize) -> usize {
    if i < 0.0 {
        let from_end = len as f64 + i;
        if from_end < 0.0 {
            0
        } else {
            from_end as usize
        }
    } else if i as usize > len {
        len
    } else {
        i as usize
    }
}
