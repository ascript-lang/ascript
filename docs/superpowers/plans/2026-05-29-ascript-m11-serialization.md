# AScript Milestone 11 — Serialization & Encoding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement spec §11.2 "Data & text" serialization/encoding modules: `std/bytes`, `std/json`, `std/encoding`, `std/regex`, `std/uuid`, `std/csv`, `std/toml`, `std/yaml`. Introduces **`Value::Bytes`** (a mutable byte buffer) and **`Value::Regex`** (a compiled pattern) value kinds, and brings in the crate-backed parsers under a default-on `data` Cargo feature group (spec §12.4).

**Architecture:** Each module follows the M10 stdlib pattern (`exports()` + a `call`/`call_*` dispatcher registered in `src/stdlib/mod.rs`). Two new `Value` kinds (`Bytes`, `Regex`) get explicit arms in every exhaustive `Value` match (the M10 `Map` discipline). **One shared converter** between AScript `Value` and `serde_json::Value` (in `json.rs`) is reused by `json`, `toml`, and `yaml` — both `toml` and `serde_yaml` deserialize into / serialize from `serde_json::Value`, so the structural mapping lives in exactly one place. Parsers return Tier-1 Results (`[value, err]`); argument-type misuse panics (Tier-2).

**Tech Stack:** Rust 2021. New crates (under a default `data` feature): `serde`/`serde_json`, `regex`, `base64`, `hex`, `uuid` (v4+v7), `csv`, `toml`, `serde_yaml`, `percent-encoding` (url encode/decode). No async (all synchronous).

**Starting state (end of M10, on `main`):** 205 tests (183 lib + 15 cli + 5 module + 2 conformance), clippy clean. `src/stdlib/{mod,math,string,array,object,map,convert}.rs` exist. `Value` kinds: Nil, Bool, Number, Str, Builtin, Function, Array, Object, Map, Enum, EnumVariant, Class, Instance, BoundMethod, Super. Shared helpers in `mod.rs`: `bi`, `arg`, `want_number/string/array/object`, `clamp_index`; Tier-1 builders `make_pair`/`make_error` (`pub(crate)` in interp.rs); `type_name`/`check_type` in interp.rs.

**Conventions:** single-threaded `Rc`/`RefCell` (never `Arc`); `Control` = `Panic`(Tier-2)/`Propagate`(`?`); `AsError::at(msg,span).into()` → panic. Spec §11.3: fallible-on-data → Tier-1 `[value,err]`; wrong-arg-type → Tier-2 panic via `want_*`. Per-module `ctx = |f| format!("module.{}", f)`. Reuse the `run`/`run_err` async test helpers in `src/interp.rs` tests.

## Semantics decided (read before implementing)

- **`Value::Bytes(Rc<RefCell<Vec<u8>>>)`** — mutable reference type. Identity equality (`Rc::ptr_eq`, like arrays/maps). `type` = `"bytes"`. `len(b)` = byte count. Display = `<bytes len N>` (don't dump arbitrary binary). Truthy (automatic). NOT indexable via `b[i]` (use `bytes.get`).
- **`Value::Regex(Rc<RegexHandle>)`** where `RegexHandle { re: regex::Regex, source: String }` — immutable. Identity equality. `type` = `"regex"`. Display = `<regex {source}>`. Truthy.
- **Tier-1 for all parse/decode** (malformed input is data, not a bug): `json/toml/yaml/csv.parse`, `*.stringify`, `encoding.*Decode`, `uuid` never fails. Wrong ARG type (e.g. `json.parse(42)`) → Tier-2 panic.
- **The shared converter** `json::to_ascript(serde_json::Value) -> Value` and `json::from_ascript(&Value) -> Result<serde_json::Value, String>` (Err string on a non-serializable value: function/builtin/regex/bytes/class/instance/enum, a `Map` with a non-string key, or a reference cycle). Used by json/toml/yaml.
- **`data` Cargo feature:** all M11 crates gate behind `data`, which is in `[features] default = ["data"]` so the normal build + tests include them. The stdlib module registration for these modules is `#[cfg(feature = "data")]`; without the feature, importing them yields the existing "unknown standard library module" error.

## File structure

| File | Responsibility | Change |
|---|---|---|
| `Cargo.toml` | `data` feature + the 9 crates | modify |
| `src/value.rs` | `Value::Bytes`, `Value::Regex` + `RegexHandle` + their match arms | modify |
| `src/interp.rs` | `type_name`/`len` arms for Bytes/Regex (no `check_type` type for them — no `bytes`/`regex` contract type in spec) | modify |
| `src/stdlib/mod.rs` | register the 8 modules (cfg-gated); add `want_bytes` helper | modify |
| `src/stdlib/bytes.rs` | `std/bytes` | create |
| `src/stdlib/json.rs` | `std/json` + the shared `to_ascript`/`from_ascript` converter | create |
| `src/stdlib/encoding.rs` | `std/encoding` | create |
| `src/stdlib/regex.rs` | `std/regex` | create |
| `src/stdlib/uuid.rs` | `std/uuid` | create |
| `src/stdlib/csv.rs` | `std/csv` | create |
| `src/stdlib/toml.rs` | `std/toml` | create |
| `src/stdlib/yaml.rs` | `std/yaml` | create |
| `examples/serialization.as` | end-to-end example | create |
| `tests/cli.rs` | example integration test | modify |

## Scope & Justified Deferrals

| Deferred | Why | Owner |
|---|---|---|
| `std/time`, `std/date`, `std/intl` | Time & locale group | **M12** |
| `std/fs`, `std/process`, `std/env`, `std/crypto`, `std/compress`, `std/sqlite` | System group | **M13** |
| net/http/ws/tui | Async + UI | **M14/M15** |
| A `bytes`/`regex` contract TYPE (`let b: bytes`) | Spec §5 type list does not include `bytes`/`regex`; values work fully, just no annotation keyword. Not dependency-blocked — simply out of the spec's type grammar. | n/a (not a spec type) |

Nothing in M11's own module scope is deferred.

---

## Task 1: `Value::Bytes` kind + `std/bytes` module

**Files:** modify `src/value.rs`, `src/interp.rs`, `src/stdlib/mod.rs`; create `src/stdlib/bytes.rs`.

- [ ] **Step 1: `src/value.rs`** — add the variant to `enum Value` after `Map`:
```rust
    Bytes(Rc<RefCell<Vec<u8>>>),
```
Add match arms (compiler will flag each):
- `PartialEq::eq`: `(Value::Bytes(a), Value::Bytes(b)) => Rc::ptr_eq(a, b),`
- `Debug::fmt`: `Value::Bytes(b) => write!(f, "Bytes(len {})", b.borrow().len()),`
- `write_display`: `Value::Bytes(b) => write!(f, "<bytes len {}>", b.borrow().len()),`
- `is_truthy`: no change (automatic).

- [ ] **Step 2: `src/interp.rs`** — `type_name`: add `Value::Bytes(_) => "bytes",`. In the `len` builtin match, add `Value::Bytes(b) => b.borrow().len(),`.

- [ ] **Step 3: `src/stdlib/mod.rs`** — add a `want_bytes` helper (mirrors `want_array`):
```rust
pub(crate) fn want_bytes(v: &Value, span: Span, ctx: &str) -> Result<Rc<std::cell::RefCell<Vec<u8>>>, Control> {
    match v {
        Value::Bytes(b) => Ok(b.clone()),
        _ => Err(AsError::at(format!("{} expects bytes, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}
```
Register the module: `pub mod bytes;`, `"std/bytes" => bytes::exports(),`, `"bytes" => bytes::call(func, args, span),`. (Bytes module is NOT behind the `data` feature — it's pure std, no crate. Keep it always-on like math/string.)

- [ ] **Step 4: Create `src/stdlib/bytes.rs`.** API: `alloc(n)` (zeroed), `fromArray(arr)` (numbers 0–255; out-of-range → panic), `toArray(b)`, `get(b, i)` (→ number or nil), `set(b, i, v)` (mutates; OOB or v∉0..255 → panic), `slice(b, start, end?)` (new bytes), `concat(...)` (new bytes), `readUint(b, offset, byteLen, endian)` / `writeUint(b, offset, value, byteLen, endian)` (`endian` = `"le"`|`"be"`, byteLen 1–8), `readInt`/`writeInt` (two's-complement, sign-extended). Length via the global `len`.
```rust
//! `std/bytes` — a mutable byte buffer with int read/write and endian handling.

use super::{arg, bi, want_array, want_bytes, want_number, clamp_index};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("alloc", bi("bytes.alloc")),
        ("fromArray", bi("bytes.fromArray")),
        ("toArray", bi("bytes.toArray")),
        ("get", bi("bytes.get")),
        ("set", bi("bytes.set")),
        ("slice", bi("bytes.slice")),
        ("concat", bi("bytes.concat")),
        ("readUint", bi("bytes.readUint")),
        ("writeUint", bi("bytes.writeUint")),
        ("readInt", bi("bytes.readInt")),
        ("writeInt", bi("bytes.writeInt")),
    ]
}

fn bytes_val(v: Vec<u8>) -> Value { Value::Bytes(Rc::new(RefCell::new(v))) }

fn want_byte(v: &Value, span: Span, ctx: &str) -> Result<u8, Control> {
    let n = want_number(v, span, ctx)?;
    if n.fract() != 0.0 || !(0.0..=255.0).contains(&n) {
        return Err(AsError::at(format!("{}: byte value must be an integer 0..=255, got {}", ctx, n), span).into());
    }
    Ok(n as u8)
}

fn want_endian(v: &Value, span: Span, ctx: &str) -> Result<bool /*little*/, Control> {
    match v {
        Value::Str(s) if s.as_ref() == "le" => Ok(true),
        Value::Str(s) if s.as_ref() == "be" => Ok(false),
        Value::Nil => Ok(false), // default big-endian (network order)
        _ => Err(AsError::at(format!("{}: endian must be \"le\" or \"be\"", ctx), span).into()),
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("bytes.{}", f);
    match func {
        "alloc" => {
            let n = want_number(&arg(args, 0), span, &ctx("alloc"))?;
            if n.fract() != 0.0 || n < 0.0 {
                return Err(AsError::at("bytes.alloc size must be a non-negative integer", span).into());
            }
            Ok(bytes_val(vec![0u8; n as usize]))
        }
        "fromArray" => {
            let a = want_array(&arg(args, 0), span, &ctx("fromArray"))?;
            let mut out = Vec::with_capacity(a.borrow().len());
            for v in a.borrow().iter() {
                out.push(want_byte(v, span, &ctx("fromArray"))?);
            }
            Ok(bytes_val(out))
        }
        "toArray" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("toArray"))?;
            let arr: Vec<Value> = b.borrow().iter().map(|&x| Value::Number(x as f64)).collect();
            Ok(Value::Array(Rc::new(RefCell::new(arr))))
        }
        "get" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("get"))?;
            let i = want_number(&arg(args, 1), span, &ctx("get"))?;
            if i < 0.0 || i.fract() != 0.0 { return Ok(Value::Nil); }
            Ok(b.borrow().get(i as usize).map(|&x| Value::Number(x as f64)).unwrap_or(Value::Nil))
        }
        "set" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("set"))?;
            let i = want_number(&arg(args, 1), span, &ctx("set"))?;
            let v = want_byte(&arg(args, 2), span, &ctx("set"))?;
            if i < 0.0 || i.fract() != 0.0 {
                return Err(AsError::at("bytes.set index must be a non-negative integer", span).into());
            }
            let idx = i as usize;
            let mut bb = b.borrow_mut();
            if idx >= bb.len() {
                return Err(AsError::at(format!("bytes.set index {} out of bounds (len {})", idx, bb.len()), span).into());
            }
            bb[idx] = v;
            Ok(Value::Nil)
        }
        "slice" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("slice"))?;
            let bb = b.borrow();
            let len = bb.len();
            let start = clamp_index(want_number(&arg(args, 1), span, &ctx("slice"))?, len);
            let end = match args.get(2) {
                None | Some(Value::Nil) => len,
                Some(v) => clamp_index(want_number(v, span, &ctx("slice"))?, len),
            };
            let out = if start < end { bb[start..end].to_vec() } else { Vec::new() };
            Ok(bytes_val(out))
        }
        "concat" => {
            let mut out = Vec::new();
            for (i, v) in args.iter().enumerate() {
                let b = want_bytes(v, span, &format!("{} (argument {})", ctx("concat"), i + 1))?;
                out.extend_from_slice(&b.borrow());
            }
            Ok(bytes_val(out))
        }
        "readUint" | "readInt" => {
            let b = want_bytes(&arg(args, 0), span, &ctx(func))?;
            let offset = want_number(&arg(args, 1), span, &ctx(func))? as usize;
            let n = want_number(&arg(args, 2), span, &ctx(func))? as usize;
            let little = want_endian(&arg(args, 3), span, &ctx(func))?;
            if !(1..=8).contains(&n) {
                return Err(AsError::at(format!("{}: byte length must be 1..=8", ctx(func)), span).into());
            }
            let bb = b.borrow();
            if offset + n > bb.len() {
                return Err(AsError::at(format!("{}: read out of bounds", ctx(func)), span).into());
            }
            let mut buf = [0u8; 8];
            if little {
                buf[..n].copy_from_slice(&bb[offset..offset + n]);
            } else {
                buf[8 - n..].copy_from_slice(&bb[offset..offset + n]);
            }
            let raw = u64::from_le_bytes(buf);
            if func == "readUint" {
                Ok(Value::Number(raw as f64))
            } else {
                // sign-extend from the top bit of the n-byte value
                let shifted = if little { raw } else { raw >> (8 * (8 - n)) };
                let bits = 8 * n as u32;
                let signed = if bits < 64 && (shifted & (1 << (bits - 1))) != 0 {
                    (shifted as i64) - (1i64 << bits)
                } else {
                    shifted as i64
                };
                Ok(Value::Number(signed as f64))
            }
        }
        "writeUint" | "writeInt" => {
            let b = want_bytes(&arg(args, 0), span, &ctx(func))?;
            let offset = want_number(&arg(args, 1), span, &ctx(func))? as usize;
            let value = want_number(&arg(args, 2), span, &ctx(func))?;
            let n = want_number(&arg(args, 3), span, &ctx(func))? as usize;
            let little = want_endian(&arg(args, 4), span, &ctx(func))?;
            if !(1..=8).contains(&n) {
                return Err(AsError::at(format!("{}: byte length must be 1..=8", ctx(func)), span).into());
            }
            let raw = if func == "writeUint" {
                if value < 0.0 { return Err(AsError::at("bytes.writeUint value must be non-negative", span).into()); }
                value as u64
            } else {
                (value as i64) as u64
            };
            let le = raw.to_le_bytes();
            let mut bb = b.borrow_mut();
            if offset + n > bb.len() {
                return Err(AsError::at(format!("{}: write out of bounds", ctx(func)), span).into());
            }
            if little {
                bb[offset..offset + n].copy_from_slice(&le[..n]);
            } else {
                let be = raw.to_be_bytes();
                bb[offset..offset + n].copy_from_slice(&be[8 - n..]);
            }
            Ok(Value::Nil)
        }
        _ => Err(AsError::at(format!("std/bytes has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn num(n: f64) -> Value { Value::Number(n) }

    #[test]
    fn alloc_from_to_array_get_set() {
        let b = call("alloc", &[num(3.0)], sp()).unwrap();
        assert_eq!(call("toArray", std::slice::from_ref(&b), sp()).unwrap().to_string(), "[0, 0, 0]");
        call("set", &[b.clone(), num(1.0), num(255.0)], sp()).unwrap();
        assert_eq!(call("get", &[b.clone(), num(1.0)], sp()).unwrap(), num(255.0));
        assert_eq!(call("get", &[b.clone(), num(9.0)], sp()).unwrap(), Value::Nil);
        // out-of-range byte → panic
        assert!(matches!(call("set", &[b.clone(), num(0.0), num(300.0)], sp()), Err(Control::Panic(_))));
    }

    #[test]
    fn endian_roundtrip() {
        let b = call("alloc", &[num(4.0)], sp()).unwrap();
        call("writeUint", &[b.clone(), num(0.0), num(0x01020304 as f64), num(4.0), Value::Str("be".into())], sp()).unwrap();
        assert_eq!(call("toArray", std::slice::from_ref(&b), sp()).unwrap().to_string(), "[1, 2, 3, 4]");
        assert_eq!(call("readUint", &[b.clone(), num(0.0), num(4.0), Value::Str("be".into())], sp()).unwrap(), num(0x01020304 as f64));
        // little-endian write of the same value
        let b2 = call("alloc", &[num(4.0)], sp()).unwrap();
        call("writeUint", &[b2.clone(), num(0.0), num(0x01020304 as f64), num(4.0), Value::Str("le".into())], sp()).unwrap();
        assert_eq!(call("toArray", std::slice::from_ref(&b2), sp()).unwrap().to_string(), "[4, 3, 2, 1]");
    }

    #[test]
    fn signed_roundtrip_and_concat() {
        let b = call("alloc", &[num(2.0)], sp()).unwrap();
        call("writeInt", &[b.clone(), num(0.0), num(-1.0), num(2.0), Value::Str("be".into())], sp()).unwrap();
        assert_eq!(call("toArray", std::slice::from_ref(&b), sp()).unwrap().to_string(), "[255, 255]");
        assert_eq!(call("readInt", &[b.clone(), num(0.0), num(2.0), Value::Str("be".into())], sp()).unwrap(), num(-1.0));
        let c = call("concat", &[b.clone(), b.clone()], sp()).unwrap();
        assert_eq!(crate::interp::type_name(&c), "bytes");
    }
}
```

- [ ] **Step 5: interp e2e test** in `src/interp.rs` tests:
```rust
    #[tokio::test]
    async fn std_bytes_end_to_end() {
        let src = "import * as bytes from \"std/bytes\"\n\
                   let b = bytes.alloc(2)\n\
                   bytes.set(b, 0, 222)\n\
                   bytes.set(b, 1, 173)\n\
                   print(len(b))\n\
                   print(type(b))\n\
                   print(bytes.toArray(b))\n\
                   print(bytes.readUint(b, 0, 2, \"be\"))";
        assert_eq!(run(src).await, "2\nbytes\n[222, 173]\n57005\n");
    }
```
(`0xDEAD` = 57005 — verify.)

- [ ] **Step 6:** `cargo test` + `cargo clippy --all-targets` green/clean. Commit `feat: Value::Bytes kind + std/bytes module` (with the Co-Authored-By trailer).

---

## Task 2: `data` Cargo feature + `std/json` + the shared `Value`↔`serde_json::Value` converter

**Files:** modify `Cargo.toml`, `src/stdlib/mod.rs`; create `src/stdlib/json.rs`.

- [ ] **Step 1: `Cargo.toml`** — add the feature group + crates. Add a `[features]` section and the deps (use `cargo add` so versions resolve, then make them optional + grouped):
```toml
[features]
default = ["data"]
data = ["dep:serde", "dep:serde_json", "dep:regex", "dep:base64", "dep:hex", "dep:uuid", "dep:csv", "dep:toml", "dep:serde_yaml", "dep:percent-encoding"]
```
Add as **optional** deps:
```toml
serde = { version = "1", features = ["derive"], optional = true }
serde_json = { version = "1", optional = true }
regex = { version = "1", optional = true }
base64 = { version = "0.22", optional = true }
hex = { version = "0.4", optional = true }
uuid = { version = "1", features = ["v4", "v7"], optional = true }
csv = { version = "1", optional = true }
toml = { version = "0.8", optional = true }
serde_yaml = { version = "0.9", optional = true }
percent-encoding = { version = "2", optional = true }
```
Run `cargo build` to confirm resolution (network fetch). If a listed version doesn't resolve, use whatever `cargo add <crate>` selects and record it.

- [ ] **Step 2: Create `src/stdlib/json.rs`** with the shared converter + json module. The converter is `pub(crate)` so toml/yaml reuse it.
```rust
//! `std/json` — JSON parse/stringify, plus the shared AScript<->serde_json
//! converter reused by std/toml and std/yaml.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("parse", bi("json.parse")), ("stringify", bi("json.stringify"))]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("json.{}", f);
    match func {
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(jv) => Ok(make_pair(to_ascript(&jv), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("invalid JSON: {}", e).into())))),
            }
        }
        "stringify" => {
            let v = arg(args, 0);
            let pretty = matches!(args.get(1), Some(Value::Number(n)) if *n > 0.0)
                || matches!(args.get(1), Some(Value::Bool(true)));
            match from_ascript(&v, &mut Vec::new()) {
                Ok(jv) => {
                    let s = if pretty {
                        serde_json::to_string_pretty(&jv)
                    } else {
                        serde_json::to_string(&jv)
                    };
                    match s {
                        Ok(text) => Ok(make_pair(Value::Str(text.into()), Value::Nil)),
                        Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("cannot serialize: {}", e).into())))),
                    }
                }
                Err(msg) => Ok(make_pair(Value::Nil, make_error(Value::Str(msg.into())))),
            }
        }
        _ => Err(AsError::at(format!("std/json has no function '{}'", func), span).into()),
    }
}

/// serde_json::Value -> AScript Value. Objects become insertion-ordered Objects.
pub(crate) fn to_ascript(jv: &serde_json::Value) -> Value {
    match jv {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
        serde_json::Value::String(s) => Value::Str(s.as_str().into()),
        serde_json::Value::Array(a) => {
            Value::Array(Rc::new(RefCell::new(a.iter().map(to_ascript).collect())))
        }
        serde_json::Value::Object(o) => {
            let mut m = IndexMap::new();
            for (k, v) in o {
                m.insert(k.clone(), to_ascript(v));
            }
            Value::Object(Rc::new(RefCell::new(m)))
        }
    }
}

/// AScript Value -> serde_json::Value. Err (String) on a non-serializable value
/// or a reference cycle (`seen` tracks Array/Object/Map Rc pointers in progress).
pub(crate) fn from_ascript(v: &Value, seen: &mut Vec<usize>) -> Result<serde_json::Value, String> {
    match v {
        Value::Nil => Ok(serde_json::Value::Null),
        Value::Bool(b) => Ok(serde_json::Value::Bool(*b)),
        Value::Number(n) => {
            if !n.is_finite() {
                return Err(format!("cannot serialize non-finite number {} to JSON", n));
            }
            // Match AScript's own number Display: an integer-valued float
            // serializes as a JSON integer (`1`, not `1.0`). serde_json's
            // Number::from_f64 always renders a float with a trailing `.0`.
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                Ok(serde_json::Value::Number(serde_json::Number::from(*n as i64)))
            } else {
                serde_json::Number::from_f64(*n)
                    .map(serde_json::Value::Number)
                    .ok_or_else(|| format!("cannot serialize number {} to JSON", n))
            }
        }
        Value::Str(s) => Ok(serde_json::Value::String(s.to_string())),
        Value::Array(a) => {
            let ptr = Rc::as_ptr(a) as usize;
            if seen.contains(&ptr) { return Err("cannot serialize a cyclic structure to JSON".into()); }
            seen.push(ptr);
            let mut out = Vec::new();
            for item in a.borrow().iter() {
                out.push(from_ascript(item, seen)?);
            }
            seen.pop();
            Ok(serde_json::Value::Array(out))
        }
        Value::Object(o) => {
            let ptr = Rc::as_ptr(o) as usize;
            if seen.contains(&ptr) { return Err("cannot serialize a cyclic structure to JSON".into()); }
            seen.push(ptr);
            let mut map = serde_json::Map::new();
            for (k, val) in o.borrow().iter() {
                map.insert(k.clone(), from_ascript(val, seen)?);
            }
            seen.pop();
            Ok(serde_json::Value::Object(map))
        }
        Value::Map(m) => {
            // A Map serializes as a JSON object only if every key is a string.
            let ptr = Rc::as_ptr(m) as usize;
            if seen.contains(&ptr) { return Err("cannot serialize a cyclic structure to JSON".into()); }
            seen.push(ptr);
            let mut map = serde_json::Map::new();
            for (k, val) in m.borrow().iter() {
                match k.to_value() {
                    Value::Str(s) => { map.insert(s.to_string(), from_ascript(val, seen)?); }
                    other => return Err(format!("cannot serialize a map with a non-string key ({}) to JSON", crate::interp::type_name(&other))),
                }
            }
            seen.pop();
            Ok(serde_json::Value::Object(map))
        }
        other => Err(format!("cannot serialize a value of type {} to JSON", crate::interp::type_name(other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }

    #[test]
    fn parse_roundtrip() {
        let parsed = call("parse", &[s("{\"a\": 1, \"b\": [true, null, \"x\"]}")], sp()).unwrap();
        // parsed is [value, nil]; pull the value (index 0)
        assert!(parsed.to_string().starts_with("[{a: 1, b: [true, nil, \"x\"]}, nil]"));
    }

    #[test]
    fn stringify_and_errors() {
        let obj = {
            let mut m = IndexMap::new();
            m.insert("n".to_string(), Value::Number(2.0));
            Value::Object(Rc::new(RefCell::new(m)))
        };
        let out = call("stringify", std::slice::from_ref(&obj), sp()).unwrap();
        assert_eq!(out.to_string(), "[{\"n\":2}, nil]");
        // a function is not serializable → [nil, err]
        let f = Value::Builtin("print".into());
        let err = call("stringify", std::slice::from_ref(&f), sp()).unwrap();
        assert!(err.to_string().starts_with("[nil, {message:"));
    }

    #[test]
    fn parse_invalid_is_tier1_err() {
        let err = call("parse", &[s("{bad")], sp()).unwrap();
        assert!(err.to_string().starts_with("[nil, {message:"));
    }
}
```

- [ ] **Step 3: `src/stdlib/mod.rs`** — register json behind the feature: add `#[cfg(feature = "data")] pub mod json;`. In `std_module_exports`, add `#[cfg(feature = "data")] "std/json" => json::exports(),`. In `call_stdlib`, add `#[cfg(feature = "data")] "json" => json::call(func, args, span),`. (Same cfg pattern for all data-group modules in later tasks.)

- [ ] **Step 4: interp e2e** in `src/interp.rs`:
```rust
    #[tokio::test]
    async fn std_json_end_to_end() {
        let src = "import * as json from \"std/json\"\n\
                   let [v, err] = json.parse(\"{\\\"x\\\": 10, \\\"ys\\\": [1, 2]}\")\n\
                   print(v.x)\n\
                   print(v.ys[1])\n\
                   let [s, e2] = json.stringify({ a: 1, b: \"hi\" })\n\
                   print(s)";
        assert_eq!(run(src).await, "10\n2\n{\"a\":1,\"b\":\"hi\"}\n");
    }
```
Run to confirm exact output (esp. stringify key order = insertion order, and number formatting `1` not `1.0`).

- [ ] **Step 5:** `cargo test` + `cargo clippy --all-targets` green/clean. Commit `feat: data feature + std/json with shared serde_json converter`.

---

## Task 3: `std/encoding`

**Files:** create `src/stdlib/encoding.rs`; register in `mod.rs` (cfg-gated).

API (spec: "base64, hex, url-encode/decode, utf8↔bytes"): `base64Encode(bytesOrStr)→string`, `base64Decode(str)→[bytes,err]`, `hexEncode(bytes)→string`, `hexDecode(str)→[bytes,err]`, `urlEncode(str)→string`, `urlDecode(str)→[str,err]`, `utf8Encode(str)→bytes`, `utf8Decode(bytes)→[str,err]`.

- [ ] **Step 1: Create `src/stdlib/encoding.rs`:**
```rust
//! `std/encoding` — base64, hex, url percent-encoding, utf8<->bytes.

use super::{arg, bi, want_bytes, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use base64::Engine;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("base64Encode", bi("encoding.base64Encode")),
        ("base64Decode", bi("encoding.base64Decode")),
        ("hexEncode", bi("encoding.hexEncode")),
        ("hexDecode", bi("encoding.hexDecode")),
        ("urlEncode", bi("encoding.urlEncode")),
        ("urlDecode", bi("encoding.urlDecode")),
        ("utf8Encode", bi("encoding.utf8Encode")),
        ("utf8Decode", bi("encoding.utf8Decode")),
    ]
}

fn bytes_val(v: Vec<u8>) -> Value { Value::Bytes(Rc::new(RefCell::new(v))) }

/// Accept bytes OR a string (encoded as UTF-8) as a source of raw bytes.
fn source_bytes(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v {
        Value::Bytes(b) => Ok(b.borrow().clone()),
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        _ => Err(AsError::at(format!("{} expects bytes or a string, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("encoding.{}", f);
    match func {
        "base64Encode" => {
            let src = source_bytes(&arg(args, 0), span, &ctx("base64Encode"))?;
            Ok(Value::Str(base64::engine::general_purpose::STANDARD.encode(src).into()))
        }
        "base64Decode" => {
            let s = want_string(&arg(args, 0), span, &ctx("base64Decode"))?;
            match base64::engine::general_purpose::STANDARD.decode(s.as_bytes()) {
                Ok(bytes) => Ok(make_pair(bytes_val(bytes), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("invalid base64: {}", e).into())))),
            }
        }
        "hexEncode" => {
            let src = source_bytes(&arg(args, 0), span, &ctx("hexEncode"))?;
            Ok(Value::Str(hex::encode(src).into()))
        }
        "hexDecode" => {
            let s = want_string(&arg(args, 0), span, &ctx("hexDecode"))?;
            match hex::decode(s.as_ref()) {
                Ok(bytes) => Ok(make_pair(bytes_val(bytes), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("invalid hex: {}", e).into())))),
            }
        }
        "urlEncode" => {
            let s = want_string(&arg(args, 0), span, &ctx("urlEncode"))?;
            let encoded = percent_encoding::utf8_percent_encode(&s, percent_encoding::NON_ALPHANUMERIC).to_string();
            Ok(Value::Str(encoded.into()))
        }
        "urlDecode" => {
            let s = want_string(&arg(args, 0), span, &ctx("urlDecode"))?;
            match percent_encoding::percent_decode_str(&s).decode_utf8() {
                Ok(decoded) => Ok(make_pair(Value::Str(decoded.into_owned().into()), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("invalid url encoding: {}", e).into())))),
            }
        }
        "utf8Encode" => {
            let s = want_string(&arg(args, 0), span, &ctx("utf8Encode"))?;
            Ok(bytes_val(s.as_bytes().to_vec()))
        }
        "utf8Decode" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("utf8Decode"))?;
            match String::from_utf8(b.borrow().clone()) {
                Ok(s) => Ok(make_pair(Value::Str(s.into()), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("invalid utf-8: {}", e).into())))),
            }
        }
        _ => Err(AsError::at(format!("std/encoding has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }

    #[test]
    fn base64_hex_roundtrip() {
        let enc = call("base64Encode", &[s("hello")], sp()).unwrap();
        assert_eq!(enc, s("aGVsbG8="));
        let dec = call("base64Decode", std::slice::from_ref(&enc), sp()).unwrap();
        // dec = [bytes, nil]; decode back to utf8 to check
        assert!(dec.to_string().starts_with("[<bytes len 5>, nil]"));
        assert_eq!(call("hexEncode", &[s("AB")], sp()).unwrap(), s("4142"));
    }

    #[test]
    fn url_and_utf8() {
        assert_eq!(call("urlEncode", &[s("a b&c")], sp()).unwrap(), s("a%20b%26c"));
        assert_eq!(call("urlDecode", &[s("a%20b%26c")], sp()).unwrap().to_string(), "[\"a b&c\", nil]");
        let b = call("utf8Encode", &[s("hi")], sp()).unwrap();
        assert_eq!(crate::interp::type_name(&b), "bytes");
        assert_eq!(call("utf8Decode", std::slice::from_ref(&b), sp()).unwrap().to_string(), "[\"hi\", nil]");
    }

    #[test]
    fn bad_input_is_tier1_err() {
        assert!(call("base64Decode", &[s("!!!notb64")], sp()).unwrap().to_string().starts_with("[nil, {message:"));
        assert!(call("hexDecode", &[s("zz")], sp()).unwrap().to_string().starts_with("[nil, {message:"));
    }
}
```

- [ ] **Step 2:** register encoding in `mod.rs` (cfg-gated, same pattern as json). Add an interp e2e test importing `std/encoding` and round-tripping base64 of a string. `cargo test` + clippy. Commit `feat: std/encoding module`.

---

## Task 4: `Value::Regex` kind + `std/regex`

**Files:** modify `src/value.rs`, `src/interp.rs`; create `src/stdlib/regex.rs`; register in `mod.rs` (cfg-gated).

API (spec: "compile, test, find, findAll, replace, split"): `compile(pattern)→[regex,err]` (invalid pattern → Tier-1 err), `test(reOrPattern, s)→bool`, `find(re, s)→object|nil` (`{ match, index, groups }`), `findAll(re, s)→array`, `replace(re, s, repl)→string` (all matches; `$1` group refs supported by `regex` crate), `split(re, s)→array`. Functions accept either a compiled `regex` value OR a pattern string (compiled on the fly; invalid string pattern → Tier-2 panic since it's a literal misuse, OR compile and Tier-1? — DECISION: a bad pattern STRING passed to test/find/etc. is a Tier-2 panic, because `compile` is the safe/Tier-1 path; inline string patterns are assumed valid like a literal).

- [ ] **Step 1: `src/value.rs`** — add `RegexHandle` + the variant:
```rust
pub struct RegexHandle {
    pub re: regex::Regex,
    pub source: String,
}
```
(gated: put `#[cfg(feature = "data")]` on the struct AND the `Value::Regex` variant? NO — a cfg-gated enum variant is painful for exhaustive matches. Instead make `RegexHandle` always-compiled but only constructed by the data-gated module. Problem: `regex::Regex` only exists with the crate. SOLUTION: gate the whole variant `#[cfg(feature = "data")] Regex(Rc<RegexHandle>),` AND gate every match arm with `#[cfg(feature = "data")]`. This is the documented approach — each `Value` match adds a `#[cfg(feature="data")] Value::Regex(..) => ...` arm. Verify the build compiles BOTH with and without `--no-default-features`.)

Given the cfg complexity, here is the precise approach:
```rust
#[cfg(feature = "data")]
pub struct RegexHandle {
    pub re: regex::Regex,
    pub source: String,
}
```
In `enum Value`:
```rust
    #[cfg(feature = "data")]
    Regex(Rc<RegexHandle>),
```
Each exhaustive match over `Value` gets a cfg-gated arm:
- PartialEq: `#[cfg(feature = "data")] (Value::Regex(a), Value::Regex(b)) => Rc::ptr_eq(a, b),`
- Debug: `#[cfg(feature = "data")] Value::Regex(r) => write!(f, "Regex({:?})", r.source),`
- write_display: `#[cfg(feature = "data")] Value::Regex(r) => write!(f, "<regex {}>", r.source),`
- interp `type_name`: `#[cfg(feature = "data")] Value::Regex(_) => "regex",`

- [ ] **Step 2: Create `src/stdlib/regex.rs`** (the whole file is compiled only with `data`, since it's registered cfg-gated — but to be safe, the file content uses `regex::Regex` directly; it's only `mod`-declared under cfg). Implement compile/test/find/findAll/replace/split. `find` returns `{ match: <whole>, index: <char index>, groups: [<g1>, ...] }` (unmatched optional groups → nil). Provide a `want_regex(v, s, ...)` that accepts a `Value::Regex` or compiles a `Value::Str` pattern (bad string pattern → Tier-2 panic).

Write the module mirroring the established pattern. Full code:
```rust
//! `std/regex` — compiled regular expressions (backed by the `regex` crate).

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::{RegexHandle, Value};
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("compile", bi("regex.compile")),
        ("test", bi("regex.test")),
        ("find", bi("regex.find")),
        ("findAll", bi("regex.findAll")),
        ("replace", bi("regex.replace")),
        ("split", bi("regex.split")),
    ]
}

fn arr(v: Vec<Value>) -> Value { Value::Array(Rc::new(RefCell::new(v))) }

/// Resolve arg 0 to a compiled regex: a `Value::Regex` is used directly; a
/// `Value::Str` is compiled on the fly (a bad inline pattern is a Tier-2 panic —
/// use `compile` for the Tier-1 path on untrusted patterns).
fn want_regex(v: &Value, span: Span, ctx: &str) -> Result<Rc<RegexHandle>, Control> {
    match v {
        Value::Regex(r) => Ok(r.clone()),
        Value::Str(s) => match regex::Regex::new(s) {
            Ok(re) => Ok(Rc::new(RegexHandle { re, source: s.to_string() })),
            Err(e) => Err(AsError::at(format!("{}: invalid regex pattern: {}", ctx, e), span).into()),
        },
        _ => Err(AsError::at(format!("{} expects a regex or pattern string, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

fn char_index(haystack: &str, byte_idx: usize) -> f64 {
    haystack[..byte_idx].chars().count() as f64
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("regex.{}", f);
    match func {
        "compile" => {
            let s = want_string(&arg(args, 0), span, &ctx("compile"))?;
            match regex::Regex::new(&s) {
                Ok(re) => Ok(make_pair(Value::Regex(Rc::new(RegexHandle { re, source: s.to_string() })), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("invalid regex: {}", e).into())))),
            }
        }
        "test" => {
            let re = want_regex(&arg(args, 0), span, &ctx("test"))?;
            let s = want_string(&arg(args, 1), span, &ctx("test"))?;
            Ok(Value::Bool(re.re.is_match(&s)))
        }
        "find" => {
            let re = want_regex(&arg(args, 0), span, &ctx("find"))?;
            let s = want_string(&arg(args, 1), span, &ctx("find"))?;
            match re.re.captures(&s) {
                Some(caps) => {
                    let whole = caps.get(0).unwrap();
                    let groups: Vec<Value> = caps.iter().skip(1)
                        .map(|g| g.map(|m| Value::Str(m.as_str().into())).unwrap_or(Value::Nil))
                        .collect();
                    let mut obj = indexmap::IndexMap::new();
                    obj.insert("match".to_string(), Value::Str(whole.as_str().into()));
                    obj.insert("index".to_string(), Value::Number(char_index(&s, whole.start())));
                    obj.insert("groups".to_string(), arr(groups));
                    Ok(Value::Object(Rc::new(RefCell::new(obj))))
                }
                None => Ok(Value::Nil),
            }
        }
        "findAll" => {
            let re = want_regex(&arg(args, 0), span, &ctx("findAll"))?;
            let s = want_string(&arg(args, 1), span, &ctx("findAll"))?;
            let out: Vec<Value> = re.re.find_iter(&s).map(|m| Value::Str(m.as_str().into())).collect();
            Ok(arr(out))
        }
        "replace" => {
            let re = want_regex(&arg(args, 0), span, &ctx("replace"))?;
            let s = want_string(&arg(args, 1), span, &ctx("replace"))?;
            let repl = want_string(&arg(args, 2), span, &ctx("replace"))?;
            Ok(Value::Str(re.re.replace_all(&s, repl.as_ref()).into_owned().into()))
        }
        "split" => {
            let re = want_regex(&arg(args, 0), span, &ctx("split"))?;
            let s = want_string(&arg(args, 1), span, &ctx("split"))?;
            let out: Vec<Value> = re.re.split(&s).map(|p| Value::Str(p.into())).collect();
            Ok(arr(out))
        }
        _ => Err(AsError::at(format!("std/regex has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }

    #[test]
    fn test_find_findall_replace_split() {
        assert_eq!(call("test", &[s("\\d+"), s("ab12")], sp()).unwrap(), Value::Bool(true));
        let found = call("find", &[s("(\\d)(\\d)"), s("x42y")], sp()).unwrap();
        assert_eq!(found.to_string(), "{match: \"42\", index: 1, groups: [\"4\", \"2\"]}");
        assert_eq!(call("findAll", &[s("\\d"), s("a1b2")], sp()).unwrap().to_string(), "[\"1\", \"2\"]");
        assert_eq!(call("replace", &[s("\\d"), s("a1b2"), s("#")], sp()).unwrap(), s("a#b#"));
        assert_eq!(call("split", &[s(",\\s*"), s("a, b,c")], sp()).unwrap().to_string(), "[\"a\", \"b\", \"c\"]");
    }

    #[test]
    fn compile_ok_and_err_and_reuse() {
        let compiled = call("compile", &[s("[a-z]+")], sp()).unwrap();
        assert!(compiled.to_string().starts_with("[<regex [a-z]+>, nil]"));
        let bad = call("compile", &[s("(")], sp()).unwrap();
        assert!(bad.to_string().starts_with("[nil, {message:"));
    }

    #[test]
    fn bad_inline_pattern_panics() {
        assert!(matches!(call("test", &[s("("), s("x")], sp()), Err(Control::Panic(_))));
    }
}
```

- [ ] **Step 3:** register regex in `mod.rs` (cfg-gated). interp e2e: compile a regex, reuse it across test+findAll. **CRITICAL: build BOTH `cargo build` AND `cargo build --no-default-features` to confirm the cfg-gated `Value::Regex` variant + arms compile with the feature OFF too** (every match arm gated correctly). Run `cargo test` + clippy. Commit `feat: Value::Regex kind + std/regex module`.

---

## Task 5: `std/uuid`

**Files:** create `src/stdlib/uuid.rs`; register (cfg-gated).

API (spec: "v4 (random), v7 (time-ordered)"): `v4()→string`, `v7()→string`.
- [ ] **Step 1: Create `src/stdlib/uuid.rs`:**
```rust
//! `std/uuid` — UUID generation (v4 random, v7 time-ordered).

use super::bi;
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("v4", bi("uuid.v4")), ("v7", bi("uuid.v7"))]
}

pub fn call(func: &str, _args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "v4" => Ok(Value::Str(uuid::Uuid::new_v4().to_string().into())),
        "v7" => Ok(Value::Str(uuid::Uuid::now_v7().to_string().into())),
        _ => Err(AsError::at(format!("std/uuid has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }

    #[test]
    fn v4_v7_format() {
        let v4 = call("v4", &[], sp()).unwrap();
        if let Value::Str(s) = v4 {
            assert_eq!(s.len(), 36);
            assert_eq!(s.chars().filter(|&c| c == '-').count(), 4);
            assert_eq!(&s[14..15], "4"); // version nibble
        } else { panic!("expected string"); }
        let a = call("v4", &[], sp()).unwrap();
        let b = call("v4", &[], sp()).unwrap();
        assert_ne!(a, b); // random → distinct
        let v7 = call("v7", &[], sp()).unwrap();
        if let Value::Str(s) = v7 { assert_eq!(&s[14..15], "7"); } else { panic!(); }
    }
}
```
(Confirm `uuid::Uuid::now_v7()` exists in the chosen uuid version with the `v7` feature; if the API differs, adapt and report.)

- [ ] **Step 2:** register (cfg-gated), interp e2e (`print(len(uuid.v4()))` → 36), `cargo test` + clippy, commit `feat: std/uuid module`.

---

## Task 6: `std/csv`

**Files:** create `src/stdlib/csv.rs`; register (cfg-gated).

API (spec: "parse, stringify"): `parse(text, opts?)→[rows, err]` — without opts, `rows` = array of arrays of strings; with `{ header: true }`, `rows` = array of objects keyed by the first (header) row. `stringify(rows)→[text, err]` — accepts array-of-arrays (each inner = a row of values stringified) OR array-of-objects (keys from the first object become the header row).

- [ ] **Step 1: Create `src/stdlib/csv.rs`.** Use the `csv` crate's `ReaderBuilder`/`WriterBuilder` with `has_headers(false)` (we handle headers ourselves to support both modes). Full implementation:
```rust
//! `std/csv` — CSV parse/stringify (backed by the `csv` crate).

use super::{arg, bi, want_array, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("parse", bi("csv.parse")), ("stringify", bi("csv.stringify"))]
}

fn arr(v: Vec<Value>) -> Value { Value::Array(Rc::new(RefCell::new(v))) }
fn str_v(s: &str) -> Value { Value::Str(s.into()) }

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("csv.{}", f);
    match func {
        "parse" => {
            let text = want_string(&arg(args, 0), span, &ctx("parse"))?;
            let header = matches!(args.get(1), Some(Value::Object(o)) if matches!(o.borrow().get("header"), Some(Value::Bool(true))));
            let mut rdr = csv::ReaderBuilder::new().has_headers(false).flexible(true).from_reader(text.as_bytes());
            let mut records: Vec<Vec<String>> = Vec::new();
            for rec in rdr.records() {
                match rec {
                    Ok(r) => records.push(r.iter().map(|s| s.to_string()).collect()),
                    Err(e) => return Ok(make_pair(Value::Nil, make_error(str_v(&format!("invalid CSV: {}", e))))),
                }
            }
            let rows: Vec<Value> = if header {
                if records.is_empty() {
                    Vec::new()
                } else {
                    let head = records[0].clone();
                    records[1..].iter().map(|row| {
                        let mut o = indexmap::IndexMap::new();
                        for (i, key) in head.iter().enumerate() {
                            o.insert(key.clone(), str_v(row.get(i).map(|s| s.as_str()).unwrap_or("")));
                        }
                        Value::Object(Rc::new(RefCell::new(o)))
                    }).collect()
                }
            } else {
                records.into_iter().map(|row| arr(row.iter().map(|s| str_v(s)).collect())).collect()
            };
            Ok(make_pair(arr(rows), Value::Nil))
        }
        "stringify" => {
            let rows = want_array(&arg(args, 0), span, &ctx("stringify"))?;
            let rows = rows.borrow();
            let mut wtr = csv::WriterBuilder::new().from_writer(vec![]);
            // Detect array-of-objects vs array-of-arrays from the first row.
            let as_objects = matches!(rows.first(), Some(Value::Object(_)));
            if as_objects {
                // header = keys of the first object (insertion order)
                let header: Vec<String> = match rows.first() {
                    Some(Value::Object(o)) => o.borrow().keys().cloned().collect(),
                    _ => Vec::new(),
                };
                if wtr.write_record(&header).is_err() {
                    return Ok(make_pair(Value::Nil, make_error(str_v("CSV write error"))));
                }
                for row in rows.iter() {
                    let o = match row {
                        Value::Object(o) => o,
                        _ => return Ok(make_pair(Value::Nil, make_error(str_v("csv.stringify: mixed row kinds (expected all objects)")))),
                    };
                    let ob = o.borrow();
                    let fields: Vec<String> = header.iter().map(|k| ob.get(k).map(|v| v.to_string()).unwrap_or_default()).collect();
                    let _ = wtr.write_record(&fields);
                }
            } else {
                for row in rows.iter() {
                    let r = match row {
                        Value::Array(a) => a,
                        _ => return Ok(make_pair(Value::Nil, make_error(str_v("csv.stringify expects an array of arrays or an array of objects")))),
                    };
                    let fields: Vec<String> = r.borrow().iter().map(|v| v.to_string()).collect();
                    let _ = wtr.write_record(&fields);
                }
            }
            match wtr.into_inner() {
                Ok(bytes) => Ok(make_pair(str_v(&String::from_utf8_lossy(&bytes)), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(str_v(&format!("CSV write error: {}", e))))),
            }
        }
        _ => Err(AsError::at(format!("std/csv has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }

    #[test]
    fn parse_rows_and_header() {
        let rows = call("parse", &[s("a,b\n1,2\n3,4")], sp()).unwrap();
        assert!(rows.to_string().starts_with("[[[\"a\", \"b\"], [\"1\", \"2\"], [\"3\", \"4\"]], nil]"));
        let mut opt = indexmap::IndexMap::new();
        opt.insert("header".to_string(), Value::Bool(true));
        let withhdr = call("parse", &[s("name,age\nAda,36"), Value::Object(Rc::new(RefCell::new(opt)))], sp()).unwrap();
        assert!(withhdr.to_string().starts_with("[[{name: \"Ada\", age: \"36\"}], nil]"));
    }

    #[test]
    fn stringify_arrays_and_objects() {
        let data = arr(vec![arr(vec![s("x"), s("y")]), arr(vec![Value::Number(1.0), Value::Number(2.0)])]);
        let out = call("stringify", std::slice::from_ref(&data), sp()).unwrap();
        assert_eq!(out.to_string(), "[\"x,y\\n1,2\\n\", nil]");
    }
}
```
(Verify the EXACT stringify output format — the `csv` crate emits `\r\n` or `\n`? By default `csv` uses `\r\n` terminators. If so the expected string is `"x,y\r\n1,2\r\n"` — RUN the test and set the assertion to the real output, and note the line terminator. Optionally configure `.terminator(csv::Terminator::Any(b'\n'))` for `\n`; DECISION: use `\n` terminator via `WriterBuilder::new().terminator(csv::Terminator::Any(b'\n'))` for predictable cross-platform output, and assert `\n`.)

- [ ] **Step 2:** register (cfg-gated), interp e2e, `cargo test` + clippy, commit `feat: std/csv module`.

---

## Task 7: `std/toml` (reuses the json converter)

**Files:** create `src/stdlib/toml.rs`; register (cfg-gated).

API: `parse(text)→[value, err]`, `stringify(value)→[text, err]`. Reuses `crate::stdlib::json::{to_ascript, from_ascript}` via `serde_json::Value` as the bridge (the `toml` crate deserializes into and serializes from any serde type, including `serde_json::Value`).

- [ ] **Step 1: Create `src/stdlib/toml.rs`:**
```rust
//! `std/toml` — TOML parse/stringify, bridged through serde_json::Value
//! (reuses the std/json converter).

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::stdlib::json::{from_ascript, to_ascript};
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("parse", bi("toml.parse")), ("stringify", bi("toml.stringify"))]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("toml.{}", f);
    match func {
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            match toml::from_str::<serde_json::Value>(&s) {
                Ok(jv) => Ok(make_pair(to_ascript(&jv), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("invalid TOML: {}", e).into())))),
            }
        }
        "stringify" => {
            let v = arg(args, 0);
            match from_ascript(&v, &mut Vec::new()) {
                Ok(jv) => match toml::to_string(&jv) {
                    Ok(text) => Ok(make_pair(Value::Str(text.into()), Value::Nil)),
                    Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("cannot serialize to TOML: {}", e).into())))),
                },
                Err(msg) => Ok(make_pair(Value::Nil, make_error(Value::Str(msg.into())))),
            }
        }
        _ => Err(AsError::at(format!("std/toml has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }

    #[test]
    fn parse_basic() {
        let parsed = call("parse", &[s("name = \"Ada\"\nage = 36")], sp()).unwrap();
        assert!(parsed.to_string().starts_with("[{name: \"Ada\", age: 36}, nil]"));
    }

    #[test]
    fn parse_invalid_is_err() {
        assert!(call("parse", &[s("= bad")], sp()).unwrap().to_string().starts_with("[nil, {message:"));
    }

    #[test]
    fn stringify_table() {
        // TOML top level must be a table → an object
        let mut m = indexmap::IndexMap::new();
        m.insert("k".to_string(), Value::Str("v".into()));
        let obj = Value::Object(std::rc::Rc::new(std::cell::RefCell::new(m)));
        let out = call("stringify", std::slice::from_ref(&obj), sp()).unwrap();
        assert_eq!(out.to_string(), "[\"k = \\\"v\\\"\\n\", nil]");
    }
}
```
(RUN `stringify_table` to confirm TOML's exact output format for `{k:"v"}` — likely `k = "v"\n`. Adjust the assertion to the real string. NOTE: TOML cannot serialize a top-level non-table (e.g. a bare number/array) — such a `stringify` returns a Tier-1 err, which is correct; optionally add a test for it.)

- [ ] **Step 2:** register (cfg-gated), interp e2e, `cargo test` + clippy, commit `feat: std/toml module`.

---

## Task 8: `std/yaml` (reuses the json converter)

**Files:** create `src/stdlib/yaml.rs`; register (cfg-gated).

API: `parse(text)→[value, err]`, `stringify(value)→[text, err]`. Same bridge as toml via `serde_json::Value`.

- [ ] **Step 1: Create `src/stdlib/yaml.rs`** (mirror toml.rs, swapping `toml`→`serde_yaml`):
```rust
//! `std/yaml` — YAML parse/stringify, bridged through serde_json::Value.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::stdlib::json::{from_ascript, to_ascript};
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("parse", bi("yaml.parse")), ("stringify", bi("yaml.stringify"))]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("yaml.{}", f);
    match func {
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            match serde_yaml::from_str::<serde_json::Value>(&s) {
                Ok(jv) => Ok(make_pair(to_ascript(&jv), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("invalid YAML: {}", e).into())))),
            }
        }
        "stringify" => {
            let v = arg(args, 0);
            match from_ascript(&v, &mut Vec::new()) {
                Ok(jv) => match serde_yaml::to_string(&jv) {
                    Ok(text) => Ok(make_pair(Value::Str(text.into()), Value::Nil)),
                    Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("cannot serialize to YAML: {}", e).into())))),
                },
                Err(msg) => Ok(make_pair(Value::Nil, make_error(Value::Str(msg.into())))),
            }
        }
        _ => Err(AsError::at(format!("std/yaml has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }

    #[test]
    fn parse_basic() {
        let parsed = call("parse", &[s("name: Ada\nage: 36\ntags:\n  - a\n  - b")], sp()).unwrap();
        assert!(parsed.to_string().starts_with("[{name: \"Ada\", age: 36, tags: [\"a\", \"b\"]}, nil]"));
    }

    #[test]
    fn stringify_roundtrip() {
        let parsed_pair = call("parse", &[s("x: 1")], sp()).unwrap();
        // round-trip: parse then re-stringify the value (index 0 of the pair)
        // (use the interp e2e test for the destructured round-trip; here just check parse)
        assert!(parsed_pair.to_string().starts_with("[{x: 1}, nil]"));
    }

    #[test]
    fn parse_invalid_is_err() {
        assert!(call("parse", &[s("a: b: c: bad")], sp()).unwrap().to_string().starts_with("[nil, {message:") || call("parse", &[s(": : :")], sp()).unwrap().to_string().starts_with("[nil, {message:"));
    }
}
```
(RUN to confirm YAML parse output + that an invalid YAML yields a Tier-1 err — pick an input that serde_yaml genuinely rejects; adjust the `parse_invalid_is_err` input to one that truly fails and assert it.)

- [ ] **Step 2:** register (cfg-gated), interp e2e (a yaml round-trip via destructuring), `cargo test` + clippy, commit `feat: std/yaml module`.

---

## Task 9: End-to-end example + integration test + `--no-default-features` build + holistic

**Files:** create `examples/serialization.as`; modify `tests/cli.rs`.

- [ ] **Step 1: Create `examples/serialization.as`** exercising json, encoding, regex, uuid, csv, toml, yaml, bytes:
```
import * as json from "std/json"
import * as encoding from "std/encoding"
import * as regex from "std/regex"
import * as uuid from "std/uuid"
import * as csv from "std/csv"
import * as bytes from "std/bytes"

// JSON round-trip + destructuring of the Tier-1 Result
let [config, err] = json.parse("{\"name\": \"ascript\", \"version\": 11, \"tags\": [\"lang\", \"rust\"]}")
print(config.name)
print(config.version)
print(config.tags[0])
let [encoded, e2] = json.stringify({ ok: true, n: 3 })
print(encoded)

// encoding
print(encoding.base64Encode("hi"))
let [raw, e3] = encoding.base64Decode("aGVsbG8=")
let [text, e4] = encoding.utf8Decode(raw)
print(text)

// regex
let re = compileOrDie("\\w+")
print(regex.findAll(re, "the quick fox"))

// csv
let [rows, e5] = csv.parse("a,b\n1,2")
print(rows[1][0])

// bytes
let buf = bytes.alloc(2)
bytes.writeUint(buf, 0, 513, 2, "be")
print(bytes.toArray(buf))

// uuid (just length, value is random)
print(len(uuid.v4()))

fn compileOrDie(pattern) {
  let [re, err] = regex.compile(pattern)
  return re
}
```
RUN it (`cargo run --quiet -- run examples/serialization.as`) and capture exact output. Verify each line. (`513` = 0x0201 → bytes [2, 1]; base64 of "hi" = "aGk="; findAll of `\w+` → ["the","quick","fox"].)

- [ ] **Step 2:** add `runs_serialization_example` to `tests/cli.rs` (mirror existing), asserting on stable substrings (base64 "aGk=", `["the", "quick", "fox"]`, `[2, 1]`, the json fields). UUID line: assert it contains a 36-char-ish marker, or just that the program succeeds.

- [ ] **Step 3:** Verify the example parses under tree-sitter conformance (`cargo test`). It uses only existing syntax (imports, destructuring, calls) so the grammar needs no change.

- [ ] **Step 4: feature-gate verification.** Run `cargo build --no-default-features` AND `cargo test --no-default-features 2>&1 | tail`. Confirm: the crate compiles WITHOUT the `data` feature (the cfg-gated `Value::Regex` variant + all its match arms compile away cleanly; the data modules aren't registered; importing `std/json` without the feature yields "unknown standard library module"). Then `cargo build` (default, with data) and full `cargo test` + `cargo clippy --all-targets`. All green/clean. **If `--no-default-features` fails to compile, the cfg gating on `Value::Regex` arms is incomplete — fix every match.**

- [ ] **Step 5:** Commit `test: serialization end-to-end example + integration test + no-default-features build`.

---

## Definition of Done

- `cargo test` (default features) passes: lib unit (incl. all 8 modules' tests), cli integration (incl. `runs_serialization_example`), modules, conformance; `cargo clippy --all-targets` clean.
- `cargo build --no-default-features` compiles (data modules + the `Value::Regex` variant cfg away cleanly).
- Implemented per spec §11.2: `std/bytes`, `std/json`, `std/encoding`, `std/regex`, `std/uuid`, `std/csv`, `std/toml`, `std/yaml`; the `Value::Bytes` and `Value::Regex` kinds.
- Parse/decode are Tier-1 Results; arg-type misuse panics (Tier-2). One shared `Value`↔`serde_json::Value` converter serves json/toml/yaml.
- Nothing in M11's module scope deferred.

## Hand-off to Milestone 12 ("Time & locale")

M12 adds `std/time` (now, monotonic, sleep ⚡, durations), `std/date` (civil dates, parse/format, arithmetic, timezones — `chrono`/`time`), `std/intl` (locale-aware formatting — trimmed `icu4x`). `std/time.sleep` is the FIRST async stdlib function but the async seam already exists (eval is async on current-thread tokio); `sleep` awaits `tokio::time::sleep`. Note: a real future/awaitable `Value` kind is NOT needed until M14 — `time.sleep` can be an `async` builtin (like `std/array`'s `call_array`) that awaits directly inside `call_stdlib`. Durations are likely plain numbers (ms) or a small object. Add a `time`/`date` group under a Cargo feature if desired (or fold into `data`/a new `time` feature). The crate-backed-module + cfg-gating pattern from M11 is the template.
