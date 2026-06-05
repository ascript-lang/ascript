//! The AScript standard library: `std/*` modules implemented as native Rust
//! over the existing `Value` model. Each module exposes an `exports()` binding
//! list (imported names → `Value`) and a `call` entry the interpreter routes
//! qualified builtin names (`"math.abs"`) to. Per spec §11.3, native functions
//! are ordinary `function` values; argument-type misuse is a Tier-2 panic.

pub mod array;
pub mod assert_mod;
pub mod bench;
pub mod bytes;
#[cfg(feature = "binary")]
pub mod cbor;
pub mod cli;
pub mod color;
#[cfg(feature = "compress")]
pub mod compress;
pub mod convert;
#[cfg(feature = "crypto")]
pub mod crypto;
pub mod events;
#[cfg(feature = "data")]
pub mod csv;
#[cfg(feature = "datetime")]
pub mod date;
pub mod decimal;
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
pub mod lru;
pub mod map;
pub mod math;
#[cfg(feature = "binary")]
pub mod msgpack;
#[cfg(feature = "net")]
pub mod net_host;
#[cfg(feature = "net")]
pub mod net_http;
#[cfg(feature = "net")]
pub mod net_tcp;
#[cfg(feature = "net")]
pub mod net_udp;
#[cfg(feature = "net")]
pub mod net_ws;
pub mod object;
#[cfg(feature = "sys")]
pub mod os;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "sys")]
pub mod process;
#[cfg(feature = "redis")]
pub mod redis;
#[cfg(feature = "data")]
pub mod regex;
pub mod schema;
pub mod set;
#[cfg(feature = "sql")]
pub mod sqlite;
pub mod stream;
pub mod string;
pub mod sync;
pub mod task_mod;
#[cfg(feature = "telemetry")]
pub mod telemetry;
pub mod template;
pub mod time;
pub mod time_timers;
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
        "std/assert" => assert_mod::exports(),
        "std/bench" => bench::exports(),
        "std/cli" => cli::exports(),
        "std/color" => color::exports(),
        "std/decimal" => decimal::exports(),
        "std/math" => math::exports(),
        "std/string" => string::exports(),
        "std/array" => array::exports(),
        "std/object" => object::exports(),
        "std/map" => map::exports(),
        "std/schema" => schema::exports(),
        "std/set" => set::exports(),
        "std/lru" => lru::exports(),
        "std/events" => events::exports(),
        "std/template" => template::exports(),
        "std/bytes" => bytes::exports(),
        "std/convert" => convert::exports(),
        "std/task" => task_mod::exports(),
        "std/time" => time::exports(),
        "std/sync" => sync::exports(),
        "std/stream" => stream::exports(),
        #[cfg(feature = "datetime")]
        "std/date" => date::exports(),
        #[cfg(feature = "intl")]
        "std/intl" => intl::exports(),
        #[cfg(feature = "data")]
        "std/json" => json::exports(),
        #[cfg(feature = "log")]
        "std/log" => log::exports(),
        #[cfg(feature = "telemetry")]
        "std/telemetry" => telemetry::exports(),
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
        "std/os" => os::exports(),
        #[cfg(feature = "sys")]
        "std/io" => io::exports(),
        #[cfg(feature = "sys")]
        "std/process" => process::exports(),
        #[cfg(feature = "net")]
        "std/net" => net_host::exports(),
        #[cfg(feature = "net")]
        "std/net/tcp" => net_tcp::exports(),
        #[cfg(feature = "net")]
        "std/net/http" => net_http::exports(),
        #[cfg(feature = "net")]
        "std/http/server" => http_server::exports(),
        #[cfg(feature = "net")]
        "std/net/udp" => net_udp::exports(),
        #[cfg(feature = "net")]
        "std/net/ws" => net_ws::exports(),
        #[cfg(feature = "data")]
        "std/regex" => regex::exports(),
        #[cfg(feature = "sql")]
        "std/sqlite" => sqlite::exports(),
        #[cfg(feature = "postgres")]
        "std/postgres" => postgres::exports(),
        #[cfg(feature = "redis")]
        "std/redis" => redis::exports(),
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
        #[cfg(feature = "binary")]
        "std/msgpack" => msgpack::exports(),
        #[cfg(feature = "binary")]
        "std/cbor" => cbor::exports(),
        #[cfg(feature = "tui")]
        "std/tui" => tui::exports(),
        _ => return None,
    };
    Some(list.into_iter().map(|(n, v)| (n.to_string(), v)).collect())
}

/// The complete, **feature-independent** set of canonical `std/*` module
/// specifiers the language knows about. This is the authoritative list mirrored
/// 1:1 against the `std_module_exports` match arms above, but WITHOUT the
/// `#[cfg(feature = …)]` gating: a `.as` source that imports `std/json` is valid
/// AScript regardless of which Cargo features a given `ascript` binary was built
/// with, so the static checker (`unresolved-import`) must recognise every module
/// here even in a `--no-default-features` build. Keep this in sync with
/// `std_module_exports` (and the `call` routing) whenever a module is added.
pub const STD_MODULES: &[&str] = &[
    "std/assert",
    "std/bench",
    "std/cli",
    "std/color",
    "std/decimal",
    "std/math",
    "std/string",
    "std/array",
    "std/object",
    "std/map",
    "std/schema",
    "std/set",
    "std/lru",
    "std/events",
    "std/template",
    "std/bytes",
    "std/convert",
    "std/task",
    "std/time",
    "std/sync",
    "std/stream",
    "std/date",
    "std/intl",
    "std/json",
    "std/log",
    "std/telemetry",
    "std/encoding",
    "std/crypto",
    "std/compress",
    "std/env",
    "std/fs",
    "std/os",
    "std/io",
    "std/process",
    "std/net",
    "std/net/tcp",
    "std/net/http",
    "std/http/server",
    "std/net/udp",
    "std/net/ws",
    "std/regex",
    "std/sqlite",
    "std/postgres",
    "std/redis",
    "std/url",
    "std/uuid",
    "std/csv",
    "std/toml",
    "std/yaml",
    "std/msgpack",
    "std/cbor",
    "std/tui",
];

/// Is `path` a known canonical `std/*` module specifier? Feature-independent
/// (see [`STD_MODULES`]).
pub fn is_known_std_module(path: &str) -> bool {
    STD_MODULES.contains(&path)
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
        // Typed parse: json.parse(text, Class|schema) — parse JSON text, then
        // validate the result against the 2nd argument. Two dispatch paths:
        //
        //   1. `Value::Class` → existing validate_into path (unchanged).
        //   2. Tagged-Object schema (Object with `__kind` field) → run
        //      `Interp::parse_value` on the decoded JSON value, fusing a JSON
        //      parse failure and a schema mismatch into one Tier-1 [val, err].
        //
        // Disambiguation is unambiguous:
        //   - absent / `Value::Bool` → plain parse (or Bool → strict flag in
        //     the Class path — not a valid schema).
        //   - `Value::Class` → Class path (validate_into).
        //   - `Value::Object` with `__kind` → schema path (parse_value).
        //   - `Value::Object` without `__kind` → fall through to plain parse
        //     (a raw object is not a valid 2nd arg but also not a schema).
        //
        // With no 2nd arg this falls through to the normal 1-arg parse below.
        // Typed parse for the whole-document text formats (json/toml/yaml): when a
        // 2nd arg is a `Value::Class` or a tagged-Object schema, run the module's
        // plain 1-arg `parse` to get the decoded value, then validate-into / parse
        // it, FUSING a decode failure and a shape mismatch into one Tier-1 pair.
        // The csv block below is row-oriented and handled separately.
        #[cfg(feature = "data")]
        if func == "parse" && matches!(module, "json" | "toml" | "yaml") {
            if let Some(type_arg) = args.get(1) {
                let is_class = matches!(type_arg, Value::Class(_));
                let is_schema = schema::schema_kind(type_arg).is_some();
                if is_class || is_schema {
                    // `json.parse(text, Class, strict?)` — optional trailing bool.
                    let strict = matches!(args.get(2), Some(Value::Bool(true)));
                    // Module-specific 1-arg decode → [val, err].
                    let parsed = match module {
                        "json" => json::call(func, &args[..1], span)?,
                        "toml" => toml::call(func, &args[..1], span)?,
                        "yaml" => yaml::call(func, &args[..1], span)?,
                        _ => unreachable!(),
                    };
                    let type_arg = type_arg.clone();
                    return self.typed_decode(parsed, &type_arg, strict, "", span).await;
                }
            }
        }
        // Typed decode for the binary formats (msgpack/cbor): a 2nd Class|schema
        // arg validates the decoded value, reusing the shared typed_decode helper.
        #[cfg(feature = "binary")]
        if func == "decode" && matches!(module, "msgpack" | "cbor") {
            if let Some(type_arg) = args.get(1) {
                let is_class = matches!(type_arg, Value::Class(_));
                let is_schema = schema::schema_kind(type_arg).is_some();
                if is_class || is_schema {
                    let parsed = match module {
                        "msgpack" => msgpack::call(func, &args[..1], span)?,
                        "cbor" => cbor::call(func, &args[..1], span)?,
                        _ => unreachable!(),
                    };
                    let type_arg = type_arg.clone();
                    return self.typed_decode(parsed, &type_arg, false, "", span).await;
                }
            }
        }
        // Typed parse for csv (row-oriented): validate EACH decoded row against the
        // Class/schema, fail-fast on the first bad row with a `row[N]` path prefix.
        #[cfg(feature = "data")]
        if module == "csv" && func == "parse" {
            if let Some(type_arg) = args.get(1) {
                let is_class = matches!(type_arg, Value::Class(_));
                let is_schema = schema::schema_kind(type_arg).is_some();
                if is_class || is_schema {
                    // Decode rows with the original args MINUS the type arg, so any
                    // trailing `{header: true}` options object is still honored.
                    // csv.parse(text, Type, options?) → forward [text, options?].
                    let mut decode_args: Vec<Value> = vec![args[0].clone()];
                    if let Some(opts) = args.get(2) {
                        decode_args.push(opts.clone());
                    }
                    let parsed = csv::call(func, &decode_args, span)?; // [rows, err]
                    let type_arg = type_arg.clone();
                    return self.typed_decode_rows(parsed, &type_arg, span).await;
                }
            }
        }
        match module {
            "assert" => self.call_assert(func, args, span).await,
            "bench" => self.call_bench(func, args, span).await,
            "cli" => self.call_cli(func, args, span).await,
            "color" => color::call(func, args, span),
            "decimal" => decimal::call(func, args, span),
            "math" => math::call(func, args, span),
            "string" => string::call(func, args, span),
            "array" => self.call_array(func, args, span).await,
            "object" => self.call_object(func, args, span).await,
            "map" => map::call(func, args, span),
            "schema" => self.call_schema(func, args, span).await,
            "set" => set::call(func, args, span),
            "lru" => self.call_lru_new(func, args, span),
            "events" => self.call_events_new(func, args, span),
            "template" => template::call(func, args, span),
            "bytes" => bytes::call(func, args, span),
            "convert" => convert::call(func, args, span),
            "task" => self.call_task(func, args, span).await,
            "time" => self.call_time(func, args, span).await,
            "sync" => self.call_sync(func, args, span).await,
            "stream" => self.call_stream(func, args, span).await,
            #[cfg(feature = "datetime")]
            "date" => date::call(func, args, span),
            #[cfg(feature = "intl")]
            "intl" => intl::call(func, args, span),
            #[cfg(feature = "data")]
            "json" => json::call(func, args, span),
            #[cfg(feature = "log")]
            "log" => self.call_log(func, args, span).await,
            #[cfg(feature = "telemetry")]
            "telemetry" => self.call_telemetry(func, args, span).await,
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
            "os" => self.call_os(func, args, span).await,
            #[cfg(feature = "sys")]
            "io" => self.call_io(func, args, span).await,
            #[cfg(feature = "sys")]
            "process" => self.call_process(func, args, span).await,
            #[cfg(feature = "net")]
            "net" => self.call_net(func, args, span).await,
            #[cfg(feature = "net")]
            "net_tcp" => self.call_net_tcp(func, args, span).await,
            #[cfg(feature = "net")]
            "net_http" => self.call_http(func, args, span).await,
            #[cfg(feature = "net")]
            "http_server" => self.call_http_server(func, args, span).await,
            #[cfg(feature = "net")]
            "net_udp" => self.call_net_udp(func, args, span).await,
            #[cfg(feature = "net")]
            "net_ws" => self.call_net_ws(func, args, span).await,
            #[cfg(feature = "data")]
            "regex" => regex::call(func, args, span),
            #[cfg(feature = "sql")]
            "sqlite" => self.call_sqlite_open(func, args, span),
            #[cfg(feature = "postgres")]
            "postgres" => self.call_postgres(func, args, span).await,
            #[cfg(feature = "redis")]
            "redis" => self.call_redis(func, args, span).await,
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
            #[cfg(feature = "binary")]
            "msgpack" => msgpack::call(func, args, span),
            #[cfg(feature = "binary")]
            "cbor" => cbor::call(func, args, span),
            #[cfg(feature = "tui")]
            "tui" => self.call_tui(func, args, span),
            _ => Err(AsError::at(format!("unknown stdlib module '{}'", module), span).into()),
        }
    }

    /// Shared typed-decode core (SP5 §3/§4): given a `[value, err]` decode pair
    /// (the result of a module's plain 1-arg `parse`) and a 2nd `Class | schema`
    /// argument, validate the decoded value and return a single Tier-1
    /// `[value, err]` pair that FUSES a decode failure and a shape mismatch.
    ///
    /// - decode error (`err != nil`) → returned as-is (stays in the err channel).
    /// - `Value::Class` → `validate_into` (defaults/nested coercion; no `init`).
    /// - tagged-Object schema → `parse_value` (coerce=false).
    /// - A malformed schema → Tier-2 panic; a refine-fn `Control` → re-raised.
    ///
    /// Used by json/toml/yaml/msgpack/cbor whole-document typed parse.
    #[cfg(feature = "data")]
    pub(crate) async fn typed_decode(
        &self,
        parsed: Value,
        type_arg: &Value,
        strict: bool,
        path: &str,
        span: Span,
    ) -> Result<Value, Control> {
        // `parsed` is a [val, err] pair from the module's 1-arg parse.
        let (val, err) = match &parsed {
            Value::Array(a) if a.borrow().len() == 2 => {
                let b = a.borrow();
                (b[0].clone(), b[1].clone())
            }
            // Defensive: a non-pair decode result is treated as the raw value.
            other => (other.clone(), Value::Nil),
        };
        if err != Value::Nil {
            return Ok(parsed); // decode error stays in the err channel
        }
        match type_arg {
            Value::Class(c) => match self.validate_into(c, &val, strict, path, span).await {
                Ok(inst) => Ok(crate::interp::make_pair(inst, Value::Nil)),
                Err(e) => Ok(crate::interp::make_pair(
                    Value::Nil,
                    crate::interp::make_error(Value::Str(e.message.into())),
                )),
            },
            _ => match self.parse_value(type_arg, &val, path, false, span).await {
                Ok(v) => Ok(crate::interp::make_pair(v, Value::Nil)),
                Err(crate::stdlib::schema::ParseFail::Mismatch(e)) => {
                    Ok(crate::interp::make_pair(Value::Nil, e))
                }
                Err(crate::stdlib::schema::ParseFail::InvalidSchema(msg)) => {
                    Err(crate::error::AsError::at(msg, span).into())
                }
                Err(crate::stdlib::schema::ParseFail::Control(c)) => Err(c),
            },
        }
    }

    /// Row-oriented typed decode for `csv.parse(text, Class|schema, options?)`:
    /// validate EACH decoded row against the Class/schema, fail-fast on the first
    /// bad row, threading a `row[N]` path prefix into the error. Returns
    /// `[array<Instance|value>, err]`.
    #[cfg(feature = "data")]
    pub(crate) async fn typed_decode_rows(
        &self,
        parsed: Value,
        type_arg: &Value,
        span: Span,
    ) -> Result<Value, Control> {
        let (rows_val, err) = match &parsed {
            Value::Array(a) if a.borrow().len() == 2 => {
                let b = a.borrow();
                (b[0].clone(), b[1].clone())
            }
            other => (other.clone(), Value::Nil),
        };
        if err != Value::Nil {
            return Ok(parsed); // decode error stays in the err channel
        }
        let rows: Vec<Value> = match &rows_val {
            Value::Array(a) => a.borrow().clone(),
            _ => {
                return Ok(crate::interp::make_pair(
                    Value::Nil,
                    crate::interp::make_error(Value::Str(
                        "csv.parse typed: expected an array of rows".into(),
                    )),
                ))
            }
        };
        // CSV cells are inherently strings, so typed CSV rows are validated with
        // COERCION on (a `number` field accepts the cell "36" → 36). For a Class,
        // derive its object schema (the same `fromClass` mapping) and coerce-parse
        // the row, then validate_into the coerced object to build the Instance. For
        // a tagged schema, coerce-parse directly.
        let class_schema: Option<Value> = match type_arg {
            Value::Class(_) => {
                Some(self.call_schema("fromClass", std::slice::from_ref(type_arg), span).await?)
            }
            _ => None,
        };
        let mut out: Vec<Value> = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let path = format!("row[{}]", i);
            match type_arg {
                Value::Class(c) => {
                    // Step 1: coerce the row against the class-derived schema.
                    let schema = class_schema.as_ref().expect("class schema present");
                    let coerced = match self.parse_value(schema, row, &path, true, span).await {
                        Ok(v) => v,
                        Err(crate::stdlib::schema::ParseFail::Mismatch(e)) => {
                            return Ok(crate::interp::make_pair(Value::Nil, e));
                        }
                        Err(crate::stdlib::schema::ParseFail::InvalidSchema(msg)) => {
                            return Err(crate::error::AsError::at(msg, span).into());
                        }
                        Err(crate::stdlib::schema::ParseFail::Control(ctl)) => return Err(ctl),
                    };
                    // Step 2: validate the coerced object into a class Instance.
                    match self.validate_into(c, &coerced, false, &path, span).await {
                        Ok(inst) => out.push(inst),
                        Err(e) => {
                            return Ok(crate::interp::make_pair(
                                Value::Nil,
                                crate::interp::make_error(Value::Str(e.message.into())),
                            ))
                        }
                    }
                }
                _ => match self.parse_value(type_arg, row, &path, true, span).await {
                    Ok(v) => out.push(v),
                    Err(crate::stdlib::schema::ParseFail::Mismatch(e)) => {
                        return Ok(crate::interp::make_pair(Value::Nil, e));
                    }
                    Err(crate::stdlib::schema::ParseFail::InvalidSchema(msg)) => {
                        return Err(crate::error::AsError::at(msg, span).into());
                    }
                    Err(crate::stdlib::schema::ParseFail::Control(ctl)) => return Err(ctl),
                },
            }
        }
        Ok(crate::interp::make_pair(
            Value::Array(crate::value::ArrayCell::new(out)),
            Value::Nil,
        ))
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
        if func == "interval" {
            return time_timers::create_interval(self, args, span);
        }
        if func == "debounce" {
            return time_timers::create_debounce(self, args, span);
        }
        if func == "throttle" {
            return time_timers::create_throttle(self, args, span);
        }
        time::call(func, args, span)
    }

    /// `std/os` dispatch. Most functions are synchronous and delegate to
    /// `os::call`. The `cpuUsage` function is async: it performs two
    /// `refresh_cpu_usage` calls with `MINIMUM_CPU_UPDATE_INTERVAL` between
    /// them (≈200 ms on most platforms) so sysinfo can compute a meaningful
    /// utilisation delta. No resources or RefCell borrows are held across the
    /// sleep; `sysinfo::System` lives entirely on the stack.
    #[cfg(feature = "sys")]
    pub(crate) async fn call_os(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        #[cfg(feature = "sysinfo")]
        if func == "cpuUsage" {
            use sysinfo::{CpuRefreshKind, RefreshKind, System, MINIMUM_CPU_UPDATE_INTERVAL};
            let mut sys = System::new_with_specifics(
                RefreshKind::new().with_cpu(CpuRefreshKind::new().with_cpu_usage()),
            );
            // First measurement (baseline).
            sys.refresh_cpu_usage();
            // Hold no borrow across the await; `sys` is a plain stack local.
            tokio::time::sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;
            // Second measurement (delta).
            sys.refresh_cpu_usage();
            let pct = sys.global_cpu_usage() as f64;
            return Ok(Value::Number(pct.clamp(0.0, 100.0)));
        }
        os::call(func, args, span)
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
) -> Result<gcmodule::Cc<crate::value::ArrayCell>, Control> {
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
) -> Result<gcmodule::Cc<crate::value::ObjectCell>, Control> {
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
