//! The AScript standard library: `std/*` modules implemented as native Rust
//! over the existing `Value` model. Each module exposes an `exports()` binding
//! list (imported names → `Value`) and a `call` entry the interpreter routes
//! qualified builtin names (`"math.abs"`) to. Per spec §11.3, native functions
//! are ordinary `function` values; argument-type misuse is a Tier-2 panic.

#[cfg(feature = "ai")]
pub mod ai;
pub mod array;
pub mod assert_mod;
pub mod bench;
pub mod bytes;
pub mod caps;
#[cfg(feature = "binary")]
pub mod cbor;
pub mod cli;
pub mod color;
#[cfg(feature = "compress")]
pub mod compress;
pub mod convert;
#[cfg(feature = "archive")]
pub mod archive;
#[cfg(feature = "crypto")]
pub mod crypto;
#[cfg(feature = "docker")]
pub mod docker;
pub mod events;
#[cfg(feature = "ffi")]
pub mod ffi;
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
pub mod http1;
#[cfg(feature = "net")]
pub mod http_server;
#[cfg(feature = "intl")]
pub mod intl;
#[cfg(feature = "sys")]
pub mod io;
// BATT §5 — `std/jwt` (feature `auth`): JSON Web Tokens with typed keys that
// structurally kill alg-confusion. A5 wires HS256/384/512 (hmac+sha2).
#[cfg(feature = "auth")]
pub mod jwt;
#[cfg(feature = "data")]
pub mod json;
#[cfg(feature = "log")]
pub mod log;
#[cfg(feature = "workflow")]
pub mod workflow;
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
pub mod net_unix;
#[cfg(feature = "net")]
pub mod net_ws;
pub mod object;
// BATT §5.6 — `std/oauth` (feature `auth`): OAuth2 + PKCE over the SHARED pooled
// reqwest client (no second HTTP stack).
#[cfg(feature = "auth")]
pub mod oauth;
// BATT §4 — TLS shared plumbing (NOT a script module / not in STD_MODULES): PEM
// loading + client/server config builders used by net_tcp.connectTls, http_server
// serve({tls}), and email STARTTLS.
#[cfg(feature = "tls")]
pub mod tls;
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
#[cfg(feature = "resilience")]
pub mod resilience;
#[cfg(feature = "shared")]
pub mod shared;
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
use crate::value::{Value, ValueKind};
use std::rc::Rc;

/// A native builtin value with a qualified name (`"math.abs"`).
pub(crate) fn bi(qualified: &str) -> Value {
    Value::builtin(qualified)
}

/// The export list (binding name → value) for a `std/*` module path, or `None`
/// if `path` is not a known stdlib module.
pub fn std_module_exports(path: &str) -> Option<Vec<(String, Value)>> {
    let list: Vec<(&'static str, Value)> = match path {
        #[cfg(feature = "ai")]
        "std/ai" => ai::exports(),
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
        #[cfg(feature = "resilience")]
        "std/resilience" => resilience::exports(),
        #[cfg(feature = "shared")]
        "std/shared" => shared::exports(),
        "std/set" => set::exports(),
        "std/lru" => lru::exports(),
        "std/events" => events::exports(),
        "std/template" => template::exports(),
        "std/bytes" => bytes::exports(),
        "std/caps" => caps::exports(),
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
        #[cfg(feature = "workflow")]
        "std/workflow" => workflow::exports(),
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
        "std/net/unix" => net_unix::exports(),
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
        #[cfg(feature = "ffi")]
        "std/ffi" => ffi::exports(),
        #[cfg(feature = "docker")]
        "std/docker" => docker::exports(),
        #[cfg(feature = "auth")]
        "std/jwt" => jwt::exports(),
        #[cfg(feature = "auth")]
        "std/oauth" => oauth::exports(),
        #[cfg(feature = "archive")]
        "std/archive" => archive::exports(),
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
    "std/ai",
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
    "std/shared",
    "std/set",
    "std/lru",
    "std/events",
    "std/template",
    "std/bytes",
    "std/caps",
    "std/convert",
    "std/task",
    "std/time",
    "std/sync",
    "std/stream",
    "std/date",
    "std/intl",
    "std/json",
    "std/log",
    "std/workflow",
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
    "std/net/unix",
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
    "std/ffi",
    "std/resilience",
    "std/docker",
    "std/jwt",
    "std/oauth",
    "std/archive",
];

/// Is `path` a known canonical `std/*` module specifier? Feature-independent
/// (see [`STD_MODULES`]).
pub fn is_known_std_module(path: &str) -> bool {
    STD_MODULES.contains(&path)
}

/// FFI §4.4: for a path-bearing `fs` function, return `(path, is_write)` — the
/// path it operates on (arg 0) and whether the operation MUTATES the filesystem.
/// `None` for the pure path-string helpers (`join`/`dirname`/… do not touch the
/// filesystem) or a non-string arg 0 (a type error the fn itself reports). Used by
/// the fs stage-2 carve-out check; never reached unless an `fs` carve-out is
/// configured (Gate-12).
fn fs_path_arg<'a>(func: &str, args: &'a [Value]) -> Option<(&'a str, bool)> {
    let is_write = match func {
        // Filesystem-mutating operations.
        "write" | "append" | "mkdir" | "remove" => true,
        // Filesystem-reading operations.
        "read" | "readBytes" | "exists" | "stat" | "readDir" | "walk" | "grep" => false,
        // Pure path-string ops (join/dirname/basename/extname/isAbsolute) — no fs access.
        _ => return None,
    };
    match args.first().map(Value::kind) {
        Some(ValueKind::Str(s)) => Some((s.as_ref(), is_write)),
        _ => None,
    }
}

/// FFI §4.1/§4.3: the **complete, central** map from a dispatch-site `(module,
/// func)` to the [`Cap`](caps::Cap) it requires, or `None` for a module that
/// touches no dangerous resource. This is the single enumeration the capability
/// gate consults; because it is keyed at the `call_stdlib` dispatch ROOT, every
/// OS-touching path — including **DNS** (`net.lookup`/`lookupOne`, which route
/// through `"net"` but are NOT a connect/bind site), stdin reads (`io`), and
/// host-topology leaks (`os.networkInterfaces`/`localIp`/`hostname`) — is captured
/// **by construction**: there is no per-function path that can slip the gate.
///
/// Feature-independent (pure data): the mapping exists in every build so a
/// `--no-default-features` binary still denies `fs`/`net`/`process`/`ffi`/`env`.
///
/// `os` is the ONE module whose verdict depends on `func` (§4.3a): the
/// topology/identity calls leak the network even without acquiring a socket and
/// are gated by `Net`; the rest of `os` is ambient self-introspection (`pid`,
/// `platform`, `arch`, `cpuCount`, `tempDir`, `uptime`, `disks`, `memory`, …) and
/// is ungated.
pub fn required_cap(module: &str, func: &str) -> caps::CapReq {
    use caps::{Cap, CapReq};
    match module {
        // Filesystem — every fs func reads/writes/lists the host filesystem.
        "fs" => CapReq::one(Cap::Fs),
        // `io` reads process STDIN (a real input channel) → gated as a host-fd read.
        "io" => CapReq::one(Cap::Fs),
        // Environment variables.
        "env" => CapReq::one(Cap::Env),
        // Subprocess spawning.
        "process" => CapReq::one(Cap::Process),
        // FFI: a denied `ffi` blocks `ffi.open`, transitively blocking all native calls.
        "ffi" => CapReq::one(Cap::Ffi),
        // All network modules — sockets, HTTP, DNS, UDP, WebSocket, servers, Unix-domain.
        // `"net"` covers `net.lookup`/`lookupOne` (DNS) by construction. `net_unix` is a
        // UDS byte pipe — single-cap `net` (it conveys no process authority; CNTR §5.1).
        "net" | "net_tcp" | "net_http" | "net_udp" | "net_ws" => CapReq::one(Cap::Net),
        // BATT A8 §5.7 — `http_server` is PER-FUNC (the `jwt` precedent): `create`/`serve`
        // bind + accept sockets → `Net`; the signed-cookie + session helpers
        // (`signCookie`/`verifyCookie`/`setCookie`/`session`) are PURE crypto / string
        // rendering (no I/O) → ungated, so they work under `--sandbox` (a handler may
        // verify a session in a `run_in_worker({deny net})` isolate). `auth`-only funcs.
        "http_server" => match func {
            #[cfg(feature = "auth")]
            "signCookie" | "verifyCookie" | "setCookie" | "session" => CapReq::NONE,
            _ => CapReq::one(Cap::Net),
        },
        #[cfg(feature = "net")]
        "net_unix" => CapReq::one(Cap::Net),
        // CNTR §5.2 — the FIRST conjunction: `docker.*` drives the Engine API over the
        // network AND can spawn arbitrary host processes, so it requires BOTH `net` AND
        // `process`. `--deny net` or `--deny process` (or `--sandbox`) blocks it. Gated
        // like the other feature arms (the `docker` feature pulls `net`).
        #[cfg(feature = "docker")]
        "docker" => CapReq::one(Cap::Net).and(Cap::Process),
        // Database modules open OS resources (BLOCKER 2). `sqlite` opens/creates a DB
        // file → `Fs`; `postgres`/`redis` open TCP sockets → `Net`. Feature-gated the
        // SAME way the dispatch match arms are so `--no-default-features` still builds.
        #[cfg(feature = "sql")]
        "sqlite" => CapReq::one(Cap::Fs),
        #[cfg(feature = "postgres")]
        "postgres" => CapReq::one(Cap::Net),
        #[cfg(feature = "redis")]
        "redis" => CapReq::one(Cap::Net),
        // `ai` + `telemetry` each carry their OWN reqwest network stack (LLM API calls /
        // OTLP-HTTP/Sentry/PostHog exporters) — NOT routed through `net_http`, so they
        // need their own gate or a `--deny net`/`--sandbox`/`run_in_worker({deny net})`
        // isolate could still exfiltrate over the network. Whole-module `Net` gate, the
        // same posture as `net.lookup`. Feature-gated like the dispatch arms.
        #[cfg(feature = "ai")]
        "ai" => CapReq::one(Cap::Net),
        #[cfg(feature = "telemetry")]
        "telemetry" => CapReq::one(Cap::Net),
        // `workflow.run`/`resume` PERSIST an append-only event log to a user-specified
        // `{log}` FILE PATH (`write_log` → `std::fs::File::create`), so a durable workflow
        // writes the host filesystem → `Fs`. (Found by the completeness sweep below; same
        // ungated-OS-module class as ai/telemetry.)
        #[cfg(feature = "workflow")]
        "workflow" => CapReq::one(Cap::Fs),
        // BATT §5.4 — `std/jwt` is PER-FUNC (the `os` precedent): `jwks` fetches keys
        // over the network → `Net`; `sign`/`verify`/`decode`/`hmacKey` are pure crypto
        // → ungated. A5 ships only the ungated funcs (the `jwks` arm is the shape A7
        // fills). Feature-gated like the dispatch arms.
        #[cfg(feature = "auth")]
        "jwt" => match func {
            "jwks" => CapReq::one(Cap::Net),
            _ => CapReq::NONE,
        },
        // BATT §5.6 — `std/oauth` drives OAuth2 token endpoints + OIDC discovery
        // over the network → whole-module `Net` (the same posture as ai/telemetry,
        // which also carry network egress). `--deny net`/`--sandbox` blocks it.
        #[cfg(feature = "auth")]
        "oauth" => CapReq::one(Cap::Net),
        // BATT B1 §6 — `std/archive` is PER-FUNC: the in-memory streaming fns
        // (`tarWriter`/`tarEntries`/`tarAppend`) touch no OS resource → ungated; the
        // disk fns (`tarExtractTo`/`zipExtractTo`/`tarCreateFromDir`, added in B2)
        // read or write the host filesystem → `Fs`. Wiring the `Fs` arm now so the
        // gate is correct the moment B2 lands the disk fns.
        #[cfg(feature = "archive")]
        "archive" => match func {
            "tarExtractTo" | "zipExtractTo" | "tarCreateFromDir" => CapReq::one(Cap::Fs),
            _ => CapReq::NONE,
        },
        // `os` is per-func: topology/identity leak network info → `Net`; the rest
        // is ambient self-introspection and ungated.
        "os" => match func {
            "networkInterfaces" | "localIp" | "hostname" => CapReq::one(Cap::Net),
            _ => CapReq::NONE,
        },
        // Everything else is pure / non-resource-acquiring (math, json, string, …).
        _ => CapReq::NONE,
    }
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
                let is_class = matches!(type_arg.kind(), ValueKind::Class(_));
                let is_schema = schema::schema_kind(type_arg).is_some();
                if is_class || is_schema {
                    // `json.parse(text, Class, strict?)` — optional trailing bool.
                    let strict = matches!(args.get(2).map(Value::kind), Some(ValueKind::Bool(true)));
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
                let is_class = matches!(type_arg.kind(), ValueKind::Class(_));
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
                let is_class = matches!(type_arg.kind(), ValueKind::Class(_));
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
        // FFI §4.3 — THE central capability gate. ONE chokepoint at the dispatch
        // root, keyed by module string (and `func` for `os`), so every OS-touching
        // call (fs, net incl. DNS `net.lookup`, process, env, io, os-topology) is
        // gated by construction — there is no per-function path that can slip it.
        //
        // Gate-12 short-circuit: when ALL caps are granted (the default — every
        // existing program), this is a SINGLE `Copy` bitset flag check that returns
        // immediately, with no per-call `required_cap` lookup. The gate is therefore
        // zero-cost on the hot path and the default run is byte-identical.
        let cap_bits = self.caps_bits(); // Copy snapshot — no borrow held across the await below.
        if !cap_bits.all_granted() {
            // CNTR §5.2: a requirement may be a CONJUNCTION (docker = net ∧ process).
            // The caps are checked in Cap::ALL order, so the FIRST denied cap names the
            // error (the pinned, shipped string). For a single-cap module the loop runs
            // exactly once — byte-identical to the pre-CNTR single-`Cap` gate.
            for cap in required_cap(module, func).iter() {
                self.require_cap(cap, module, func, args, span)?;
            }
            // FFI §4.4 stage-2 (fs carve-out): a configured `fs` carve-out makes the
            // dispatch gate DEFER above; the resolved path is re-checked here, at the
            // fs dispatch (the path-bearing fs funcs take the path as arg 0). Gate-12:
            // `check_fs_path` returns immediately when no `fs` carve-out is configured,
            // so this is a cheap classify + (usually) a no-op. The net stage-2 lives
            // deeper (at connect/bind / DNS) where the resolved host is known.
            if module == "fs" {
                if let Some((path, is_write)) = fs_path_arg(func, args) {
                    self.check_fs_path(std::path::Path::new(path), is_write, span)?;
                }
            }
        }
        match module {
            #[cfg(feature = "ai")]
            "ai" => self.call_ai(func, args, span).await,
            "assert" => self.call_assert(func, args, span).await,
            "bench" => self.call_bench(func, args, span).await,
            "cli" => self.call_cli(func, args, span).await,
            "color" => color::call(func, args, span),
            "decimal" => decimal::call(func, args, span),
            "math" => math::call(self, func, args, span),
            "string" => string::call(func, args, span),
            "array" => self.call_array(func, args, span).await,
            "object" => self.call_object(func, args, span).await,
            "map" => map::call(func, args, span),
            "schema" => self.call_schema(func, args, span).await,
            #[cfg(feature = "resilience")]
            "resilience" => self.call_resilience(func, args, span).await,
            #[cfg(feature = "shared")]
            "shared" => shared::call(func, args, span),
            "set" => set::call(func, args, span),
            "lru" => self.call_lru_new(func, args, span),
            "events" => self.call_events_new(func, args, span),
            "template" => template::call(func, args, span),
            "bytes" => bytes::call(func, args, span),
            "caps" => self.call_caps(func, args, span).await,
            "convert" => convert::call(func, args, span),
            "task" => self.call_task(func, args, span).await,
            "time" => self.call_time(func, args, span).await,
            "sync" => self.call_sync(func, args, span).await,
            "stream" => self.call_stream(func, args, span).await,
            #[cfg(feature = "datetime")]
            // SP9 §3 clock seam: `date.now` reads the virtual/recorded clock in
            // deterministic mode (else the real `Utc::now`). `date::now_from_ms`
            // builds the same instant `date.now` would from an explicit ms-epoch, so
            // the deterministic and default forms are identical apart from the time
            // source. All other `date.*` functions are pure and delegate unchanged.
            "date" if func == "now" && self.is_deterministic() => {
                Ok(date::now_from_ms(self.clock_now_ms()))
            }
            #[cfg(feature = "datetime")]
            "date" => date::call(func, args, span),
            #[cfg(feature = "intl")]
            "intl" => intl::call(func, args, span),
            #[cfg(feature = "data")]
            "json" => json::call(func, args, span),
            #[cfg(feature = "log")]
            "log" => self.call_log(func, args, span).await,
            #[cfg(feature = "workflow")]
            "workflow" => self.call_workflow(func, args, span).await,
            #[cfg(feature = "telemetry")]
            "telemetry" => self.call_telemetry(func, args, span).await,
            #[cfg(feature = "data")]
            "encoding" => encoding::call(func, args, span),
            #[cfg(feature = "crypto")]
            "crypto" => crypto::call(self, func, args, span),
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
            "net_unix" => self.call_net_unix(func, args, span).await,
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
            "uuid" => uuid::call(self, func, args, span),
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
            #[cfg(feature = "ffi")]
            "ffi" => self.call_ffi(func, args, span).await,
            #[cfg(feature = "docker")]
            "docker" => self.call_docker(func, args, span).await,
            #[cfg(feature = "auth")]
            "jwt" => self.call_jwt_async(func, args, span).await,
            #[cfg(feature = "auth")]
            "oauth" => self.call_oauth(func, args, span).await,
            #[cfg(feature = "archive")]
            "archive" => archive::call(self, func, args, span),
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
        let (val, err) = match parsed.kind() {
            ValueKind::Array(a) if a.borrow().len() == 2 => {
                let b = a.borrow();
                (b[0].clone(), b[1].clone())
            }
            // Defensive: a non-pair decode result is treated as the raw value.
            _ => (parsed.clone(), Value::nil()),
        };
        if err != Value::nil() {
            return Ok(parsed); // decode error stays in the err channel
        }
        match type_arg.kind() {
            ValueKind::Class(c) => match self.validate_into(c, &val, strict, path, span).await {
                Ok(inst) => Ok(crate::interp::make_pair(inst, Value::nil())),
                Err(e) => Ok(crate::interp::make_pair(
                    Value::nil(),
                    crate::interp::make_error(Value::str(e.message)),
                )),
            },
            _ => match self.parse_value(type_arg, &val, path, false, span).await {
                Ok(v) => Ok(crate::interp::make_pair(v, Value::nil())),
                Err(crate::stdlib::schema::ParseFail::Mismatch(e)) => {
                    Ok(crate::interp::make_pair(Value::nil(), e))
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
        let (rows_val, err) = match parsed.kind() {
            ValueKind::Array(a) if a.borrow().len() == 2 => {
                let b = a.borrow();
                (b[0].clone(), b[1].clone())
            }
            _ => (parsed.clone(), Value::nil()),
        };
        if err != Value::nil() {
            return Ok(parsed); // decode error stays in the err channel
        }
        let rows: Vec<Value> = match rows_val.kind() {
            ValueKind::Array(a) => a.borrow().clone(),
            _ => {
                return Ok(crate::interp::make_pair(
                    Value::nil(),
                    crate::interp::make_error(Value::str(
                        "csv.parse typed: expected an array of rows",
                    )),
                ))
            }
        };
        // CSV cells are inherently strings, so typed CSV rows are validated with
        // COERCION on (a `number` field accepts the cell "36" → 36). For a Class,
        // derive its object schema (the same `fromClass` mapping) and coerce-parse
        // the row, then validate_into the coerced object to build the Instance. For
        // a tagged schema, coerce-parse directly.
        let class_schema: Option<Value> = match type_arg.kind() {
            ValueKind::Class(_) => {
                Some(self.call_schema("fromClass", std::slice::from_ref(type_arg), span).await?)
            }
            _ => None,
        };
        let mut out: Vec<Value> = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let path = format!("row[{}]", i);
            match type_arg.kind() {
                ValueKind::Class(c) => {
                    // Step 1: coerce the row against the class-derived schema.
                    let schema = class_schema.as_ref().expect("class schema present");
                    let coerced = match self.parse_value(schema, row, &path, true, span).await {
                        Ok(v) => v,
                        Err(crate::stdlib::schema::ParseFail::Mismatch(e)) => {
                            return Ok(crate::interp::make_pair(Value::nil(), e));
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
                                Value::nil(),
                                crate::interp::make_error(Value::str(e.message)),
                            ))
                        }
                    }
                }
                _ => match self.parse_value(type_arg, row, &path, true, span).await {
                    Ok(v) => out.push(v),
                    Err(crate::stdlib::schema::ParseFail::Mismatch(e)) => {
                        return Ok(crate::interp::make_pair(Value::nil(), e));
                    }
                    Err(crate::stdlib::schema::ParseFail::InvalidSchema(msg)) => {
                        return Err(crate::error::AsError::at(msg, span).into());
                    }
                    Err(crate::stdlib::schema::ParseFail::Control(ctl)) => return Err(ctl),
                },
            }
        }
        Ok(crate::interp::make_pair(Value::array(out), Value::nil()))
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
        // SP9 §3 clock seam: in deterministic mode `time.now`/`time.monotonic` read
        // the virtual/recorded clock (so two same-seed runs agree); otherwise the
        // real clock (byte-identical to pre-SP9). Handled here because `call_time`
        // has `&self` (the determinism context lives on the `Interp`); the sync
        // `time::call` keeps its real-clock arms for any direct callers.
        if self.is_deterministic() {
            match func {
                "now" => return Ok(Value::float(self.clock_now_ms())),
                "monotonic" => {
                    let real = time::real_monotonic_ms();
                    return Ok(Value::float(self.clock_monotonic_ms(real)));
                }
                _ => {}
            }
        }
        if func == "sleep" {
            let ms = want_number(&arg(args, 0), span, "time.sleep")?;
            if ms < 0.0 {
                return Err(AsError::at("time.sleep duration must be non-negative", span).into());
            }
            // SP9 §3: in deterministic mode do NOT sleep real time — advance the
            // virtual clock by `ms` and record a durable timer; replay/resume then
            // fast-forwards with no real delay. Default mode sleeps for real.
            if self
                .with_determinism_mut(|ctx| {
                    ctx.clock.advance(ms);
                    let wake = ctx.clock.now_ms();
                    let _ = ctx.record_event(crate::det::DetEvent::TimerSet { wake });
                })
                .is_some()
            {
                return Ok(Value::nil());
            }
            // `ms as u64` truncates toward zero: a fractional `sleep(20.7)`
            // sleeps for 20 whole milliseconds.
            tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
            return Ok(Value::nil());
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
            return Ok(Value::float(pct.clamp(0.0, 100.0)));
        }
        os::call(func, args, span)
    }
}

// ---- shared argument helpers (Tier-2 panic on type misuse, spec §11.3) ----

pub(crate) fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::nil())
}

/// Type name for a "got X" mismatch message. A frozen `Value::Shared` reports its
/// UNDERLYING kind via `type_name` (so a frozen array reads as "array"), which makes
/// a bare "expects an array, got array" message self-contradictory. Prefix "frozen "
/// so the rejection reads truthfully: a frozen (read-only) value can't stand in for a
/// mutable one in a free-function like `array.push(frozen, …)` — the method form
/// `x.push(…)` already reports the canonical "cannot mutate a frozen array".
pub(crate) fn got_type_name(v: &Value) -> String {
    match v.kind() {
        ValueKind::Shared(_) => format!("frozen {}", crate::interp::type_name(v)),
        _ => crate::interp::type_name(v).to_string(),
    }
}

pub(crate) fn want_number(v: &Value, span: Span, ctx: &str) -> Result<f64, Control> {
    match v.as_f64() {
        Some(n) => Ok(n),
        None => Err(AsError::at(
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

/// A validated non-negative *count/size* drawn from a script number, checked
/// BEFORE the `f64 → usize` cast so a pathological value (`Inf`, `NaN`, `1e30`)
/// yields a clean AScript Tier-2 panic rather than a saturating cast that then
/// drives an allocation (`vec![0; n]`, `String::repeat`, `Vec::reserve`,
/// `take(n)`) into a host-aborting `capacity overflow` / OOM.
///
/// `max` is the inclusive upper bound (use `u32::MAX as f64` for a generic
/// buffer/allocation size, mirroring `bytes::want_index`, or a tighter per-call
/// cap). Fractional inputs TRUNCATE toward zero (matching the reader/`repeat`
/// convention); only non-finite, negative, or over-cap values are rejected.
pub(crate) fn want_count(v: &Value, span: Span, ctx: &str, max: f64) -> Result<usize, Control> {
    let n = want_number(v, span, ctx)?;
    if !n.is_finite() || n < 0.0 || n > max {
        return Err(AsError::at(
            format!(
                "{}: expected a finite, in-range, non-negative count (got {})",
                ctx, n
            ),
            span,
        )
        .into());
    }
    Ok(n as usize)
}

/// A generic upper bound for buffer/allocation sizes: `u32::MAX` (= 4 GiB − 1
/// byte; "≈4 GiB" is the rounded user-facing label, so a value tested exactly at
/// the boundary is `4_294_967_295`, not `4_294_967_296`), matching
/// `bytes::want_index`. Large enough for any legitimate read/repeat, small enough
/// that the allocation itself cannot abort the host.
pub(crate) const MAX_ALLOC_COUNT: f64 = u32::MAX as f64;

pub(crate) fn want_string(v: &Value, span: Span, ctx: &str) -> Result<Rc<str>, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(s.clone()),
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
    match v.kind() {
        ValueKind::Array(a) => Ok(a.clone()),
        _ => Err(AsError::at(
            format!("{} expects an array, got {}", ctx, got_type_name(v)),
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
    match v.kind() {
        ValueKind::Object(o) => Ok(o.clone()),
        _ => Err(AsError::at(
            format!("{} expects an object, got {}", ctx, got_type_name(v)),
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
    match v.kind() {
        ValueKind::Bytes(b) => Ok(b.clone()),
        _ => Err(AsError::at(
            format!("{} expects bytes, got {}", ctx, got_type_name(v)),
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

#[cfg(test)]
mod cap_gate_tests {
    use super::*;
    use crate::stdlib::caps::Cap;

    #[test]
    fn required_cap_docker_requires_both_net_and_process() {
        // CNTR §5.2: `docker` is the first CONJUNCTION requirement (net ∧ process),
        // yielded in stable Cap::ALL order. Single-cap modules yield one cap; an
        // ungated module yields the empty requirement.
        #[cfg(feature = "docker")]
        {
            let caps: Vec<Cap> = required_cap("docker", "anything").iter().collect();
            assert_eq!(caps, vec![Cap::Net, Cap::Process], "docker = net AND process, Cap::ALL order");
        }
        assert_eq!(required_cap("fs", "readFile").iter().collect::<Vec<_>>(), vec![Cap::Fs]);
        #[cfg(feature = "net")]
        assert_eq!(required_cap("net_unix", "connect").iter().collect::<Vec<_>>(), vec![Cap::Net]);
        assert!(required_cap("math", "abs").is_empty());
    }

    /// Collect a `required_cap` verdict as a `Vec<Cap>` for assertion (CNTR §5.2:
    /// the verdict is now a `CapReq` conjunction, iterated in Cap::ALL order).
    fn req(module: &str, func: &str) -> Vec<Cap> {
        required_cap(module, func).iter().collect()
    }

    #[test]
    fn required_cap_complete_enumeration() {
        // fs / io → Fs
        assert_eq!(req("fs", "readFile"), vec![Cap::Fs]);
        assert_eq!(req("fs", "writeFile"), vec![Cap::Fs]);
        assert_eq!(req("io", "readAll"), vec![Cap::Fs]);
        assert_eq!(req("io", "readLine"), vec![Cap::Fs]);
        // env → Env
        assert_eq!(req("env", "get"), vec![Cap::Env]);
        // process → Process
        assert_eq!(req("process", "spawn"), vec![Cap::Process]);
        // ffi → Ffi
        assert_eq!(req("ffi", "open"), vec![Cap::Ffi]);
        // all net modules → Net (incl. DNS via the "net" module string)
        for m in ["net", "net_tcp", "net_http", "net_udp", "net_ws", "http_server"] {
            assert_eq!(req(m, "anything"), vec![Cap::Net], "module {m}");
        }
        // DNS specifically: net.lookup / lookupOne route through "net" → Net.
        assert_eq!(req("net", "lookup"), vec![Cap::Net]);
        assert_eq!(req("net", "lookupOne"), vec![Cap::Net]);
        // BATT A8 §5.7 — http_server is per-func (the jwt precedent): create/serve bind
        // + accept sockets → Net; the cookie/session helpers are pure crypto → ungated.
        assert_eq!(req("http_server", "create"), vec![Cap::Net]);
        assert_eq!(req("http_server", "serve"), vec![Cap::Net]);
        #[cfg(feature = "auth")]
        {
            assert!(required_cap("http_server", "signCookie").is_empty());
            assert!(required_cap("http_server", "verifyCookie").is_empty());
            assert!(required_cap("http_server", "setCookie").is_empty());
            assert!(required_cap("http_server", "session").is_empty());
        }
        // CNTR §5.1: net_unix is a single-cap `net` (a UDS pipe conveys no process authority).
        #[cfg(feature = "net")]
        assert_eq!(req("net_unix", "connect"), vec![Cap::Net]);
        // CNTR §5.2: docker is the first conjunction — net ∧ process, in Cap::ALL order.
        #[cfg(feature = "docker")]
        assert_eq!(req("docker", "run"), vec![Cap::Net, Cap::Process]);
        // BLOCKER 2: database modules open OS resources and MUST be gated.
        // sqlite opens/creates a DB file → Fs; postgres/redis open TCP sockets → Net.
        #[cfg(feature = "sql")]
        assert_eq!(req("sqlite", "open"), vec![Cap::Fs]);
        #[cfg(feature = "postgres")]
        assert_eq!(req("postgres", "connect"), vec![Cap::Net]);
        #[cfg(feature = "redis")]
        assert_eq!(req("redis", "connect"), vec![Cap::Net]);
        // os per-func split (§4.3a): topology/identity → Net; ambient → empty.
        assert_eq!(req("os", "networkInterfaces"), vec![Cap::Net]);
        assert_eq!(req("os", "localIp"), vec![Cap::Net]);
        assert_eq!(req("os", "hostname"), vec![Cap::Net]);
        assert!(required_cap("os", "pid").is_empty());
        assert!(required_cap("os", "platform").is_empty());
        assert!(required_cap("os", "cpuCount").is_empty());
        assert!(required_cap("os", "tempDir").is_empty());
        assert!(required_cap("os", "uptime").is_empty());
        assert!(required_cap("os", "disks").is_empty());
        // CNTR §8.2 — inContainer() is ungated (pure filesystem probe, no new OS resource).
        assert!(required_cap("os", "inContainer").is_empty());
        // pure / non-resource modules → empty.
        assert!(required_cap("math", "abs").is_empty());
        assert!(required_cap("json", "parse").is_empty());
        assert!(required_cap("string", "upper").is_empty());
        assert!(required_cap("array", "map").is_empty());
        // caps itself is NOT gated (querying/dropping is always allowed).
        assert!(required_cap("caps", "drop").is_empty());
        // resilience is pure / in-memory.
        assert!(required_cap("resilience", "breaker").is_empty());
        // BATT B1 §6 — archive is PER-FUNC: streaming/in-memory fns ungated; the
        // disk fns (B2) → Fs.
        #[cfg(feature = "archive")]
        {
            assert!(required_cap("archive", "tarWriter").is_empty());
            assert!(required_cap("archive", "tarEntries").is_empty());
            assert!(required_cap("archive", "tarAppend").is_empty());
            assert_eq!(req("archive", "tarExtractTo"), vec![Cap::Fs]);
            assert_eq!(req("archive", "zipExtractTo"), vec![Cap::Fs]);
            assert_eq!(req("archive", "tarCreateFromDir"), vec![Cap::Fs]);
        }
    }

    /// Drift guard: every resource-acquiring module string the dispatch match
    /// routes MUST have an explicit `required_cap` mapping (a `None` is a
    /// deliberate decision, not an omission). Adding a new `std/*` resource module
    /// without an entry here will trip this test. The list mirrors the gated
    /// modules in `call_stdlib`'s `match module`.
    #[test]
    fn every_resource_module_is_mapped() {
        // Modules that MUST require a capability (whole-module).
        let gated: &[(&str, Cap)] = &[
            ("fs", Cap::Fs),
            ("io", Cap::Fs),
            ("env", Cap::Env),
            ("process", Cap::Process),
            ("ffi", Cap::Ffi),
            ("net", Cap::Net),
            ("net_tcp", Cap::Net),
            ("net_http", Cap::Net),
            ("net_udp", Cap::Net),
            ("net_ws", Cap::Net),
            ("http_server", Cap::Net),
            // BLOCKER 2: database modules open OS resources — gate the SAME way the
            // dispatch match feature-gates them, so `--no-default-features` builds.
            #[cfg(feature = "sql")]
            ("sqlite", Cap::Fs),
            #[cfg(feature = "postgres")]
            ("postgres", Cap::Net),
            #[cfg(feature = "redis")]
            ("redis", Cap::Net),
            // ai/telemetry carry their own reqwest network stacks; workflow persists an
            // event-log FILE — all OS-acquiring, all were ungated (holistic-review BLOCKER
            // 1 + completeness sweep).
            #[cfg(feature = "ai")]
            ("ai", Cap::Net),
            #[cfg(feature = "telemetry")]
            ("telemetry", Cap::Net),
            #[cfg(feature = "workflow")]
            ("workflow", Cap::Fs),
        ];
        for (m, want) in gated {
            assert_eq!(
                req(m, "x"),
                vec![*want],
                "resource module {m} must map to {want:?}"
            );
        }
        // CNTR §5.2: docker is the first conjunction requirement (net ∧ process).
        #[cfg(feature = "docker")]
        assert_eq!(req("docker", "x"), vec![Cap::Net, Cap::Process]);
        // os is per-func: at least its topology funcs must be Net.
        assert_eq!(req("os", "networkInterfaces"), vec![Cap::Net]);
    }

    /// COMPLETENESS guard (holistic-review BLOCKER 2): the prior test only checks modules
    /// someone REMEMBERED to list — it cannot catch a NEW OS-touching module added with no
    /// `required_cap` entry (exactly how ai/telemetry/workflow shipped ungated). This test
    /// closes the loop: EVERY module in `STD_MODULES` must be classified as either GATED
    /// (a `required_cap` entry) or EXPLICITLY ungated (in `KNOWN_UNGATED`, pure/in-memory
    /// or process-owned stdio). A module in NEITHER trips this — forcing a deliberate
    /// capability decision for anything new. Run only in the full default build where
    /// every feature-gated module is actually present + dispatchable.
    #[test]
    #[cfg(all(
        feature = "ai",
        feature = "telemetry",
        feature = "workflow",
        feature = "sql",
        feature = "postgres",
        feature = "redis",
        feature = "ffi",
        feature = "net"
    ))]
    fn every_std_module_is_classified_gated_or_explicitly_ungated() {
        // Pure / in-memory / process-owned-stdio modules that acquire NO new OS resource.
        // (`tui` is the controlling terminal's stdout/stdin — like `print` — not a new fd;
        // revisit if raw-tty input ever becomes a gated input channel. `log` writes
        // stderr / a capture buffer, never an arbitrary file. `cli` reads start-time argv.)
        const KNOWN_UNGATED: &[&str] = &[
            "assert", "bench", "cli", "color", "decimal", "math", "string", "array",
            "object", "map", "schema", "shared", "set", "lru", "events", "template", "bytes", "caps",
            "convert", "task", "time", "sync", "stream", "date", "intl", "json", "log",
            "encoding", "crypto", "compress", "regex", "url", "uuid", "csv", "toml", "yaml",
            "msgpack", "cbor", "tui",
            // RESIL §0: pure / in-memory policy kit — no OS resource, no cap.
            "resilience",
        ];
        for full in STD_MODULES {
            let key = full.strip_prefix("std/").unwrap().replace('/', "_");
            if key == "os" || key == "jwt" || key == "archive" {
                // Per-func gating (covered above): os (topology→Net, ambient→None);
                // jwt (BATT §5.4 — jwks→Net, sign/verify/decode/hmacKey→None);
                // archive (BATT B1 §6 — disk fns→Fs, streaming/in-memory→None). The
                // whole-module `__probe__` verdict cannot represent a per-func split.
                continue;
            }
            let gated = !required_cap(&key, "__probe__").is_empty();
            let ungated = KNOWN_UNGATED.contains(&key.as_str());
            assert!(
                gated || ungated,
                "std module '{key}' is UNCLASSIFIED: add it to `required_cap` if it touches \
                 the OS, or to `KNOWN_UNGATED` if it is pure. (A silently-ungated OS module \
                 is a capability bypass — this is exactly how ai/telemetry/workflow slipped.)"
            );
            assert!(
                !(gated && ungated),
                "std module '{key}' is in BOTH required_cap and KNOWN_UNGATED — pick one."
            );
        }
    }

    /// CNTR Phase-1 pin (migrated from the Phase-0 `Option` form to `CapReq`, §5.2) —
    /// `required_cap` verdicts for the resource-class keys `std/docker` JOINS.
    ///
    /// Phase 1 (this commit) adds `"docker" => CapReq::one(Net).and(Process)` — the
    /// first CONJUNCTION. This pin proves the existing entries it sits beside
    /// (`net_tcp → Net`, `process → Process`) are intact through the migration, and
    /// that docker is now the net ∧ process conjunction (was the Phase-0 "not yet"
    /// sanity check).
    ///
    /// Real module keys confirmed by reading the `required_cap` match arms:
    ///   `"net" | "net_tcp" | ... => CapReq::one(Cap::Net)`
    ///   `"process" => CapReq::one(Cap::Process)`
    ///   `"docker" => CapReq::one(Cap::Net).and(Cap::Process)`
    #[test]
    fn cntr_required_cap_preflight_pins() {
        // net_tcp → Net.  Docker daemon communicates over a Unix socket or TCP.
        assert_eq!(
            req("net_tcp", "connect"),
            vec![Cap::Net],
            "required_cap(\"net_tcp\", \"connect\") baseline must be [Net]"
        );
        // process → Process.  docker.exec / docker.run may use a child-process
        // fallback; the existing process gate must be intact.
        assert_eq!(
            req("process", "spawn"),
            vec![Cap::Process],
            "required_cap(\"process\", \"spawn\") baseline must be [Process]"
        );
        // CNTR §5.2: "docker" is now registered as the net ∧ process conjunction.
        #[cfg(feature = "docker")]
        assert_eq!(
            req("docker", "run"),
            vec![Cap::Net, Cap::Process],
            "\"docker\" must require BOTH net AND process (the first CapReq conjunction)"
        );
    }
}

#[cfg(all(test, feature = "shared"))]
mod want_helpers_tests {
    use super::*;
    use crate::span::Span;

    // SRV review MINOR: a frozen value passed to a mutating free-function (e.g.
    // `array.push(frozen, x)`) must reject with a TRUTHFUL "got frozen array", not the
    // self-contradictory "got array" (`type_name` of a frozen array is "array"). The
    // method form `x.push(…)` already reports the canonical "cannot mutate a frozen
    // array"; this keeps the free-function rejection from lying about the type.
    // `Control`/`Value` aren't `Debug`, so avoid `.unwrap()`/`.unwrap_err()` (they'd
    // require the other variant to be `Debug`) — match explicitly instead.
    fn mismatch_msg(v: &Value, ctx: &str) -> String {
        let sp = Span::new(0, 0);
        match want_array(v, sp, ctx) {
            Ok(_) => panic!("want_array unexpectedly accepted the value"),
            Err(crate::interp::Control::Panic(e)) => e.message,
            Err(_) => panic!("expected a Panic control"),
        }
    }

    #[test]
    fn want_array_reports_frozen_in_got_position() {
        let sp = Span::new(0, 0);
        let live = Value::array(vec![Value::int(1)]);
        let frozen = match crate::stdlib::shared::freeze(&live, sp) {
            Ok(v) => v,
            Err(_) => panic!("freeze failed"),
        };
        // Frozen array → truthful "got frozen array" (not the self-contradictory
        // "got array" that `type_name` alone would produce).
        assert_eq!(
            mismatch_msg(&frozen, "array.push"),
            "array.push expects an array, got frozen array"
        );
        // A LIVE non-array still reports its plain kind (no spurious "frozen").
        assert_eq!(
            mismatch_msg(&Value::int(3), "array.push"),
            "array.push expects an array, got int"
        );
    }
}
