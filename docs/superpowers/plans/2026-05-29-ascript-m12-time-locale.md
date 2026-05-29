# AScript Milestone 12 — Time & Locale Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement spec §11.2 "Time & locale": `std/time` (now, monotonic, sleep ⚡, durations), `std/date` (civil dates, parse/format, arithmetic, timezones — `chrono`), `std/intl` (locale-aware number/currency/date formatting, case folding, basic collation — a pragmatic subset of `icu`). Introduces the **first async stdlib function** (`time.sleep`).

**Architecture:** Same stdlib pattern (`exports()` + `call`/`call_*` dispatcher in `src/stdlib/mod.rs`). `std/time.sleep` is async, so `std/time` dispatches through `impl Interp { async fn call_time }` (like `std/array`'s `call_array`) and `await`s `tokio::time::sleep` directly — NO new future/awaitable value kind (that arrives in M14). **Dates are plain AScript objects** (`{ epochMs, year, month, … }`), so M12 introduces NO new `Value` kind and NO cfg-gated match arms. Three Cargo features (all default-on): `time` is folded into the always-on build (only needs `tokio`'s `time` feature); `datetime` gates `chrono`/`std/date`; `intl` gates `icu`/`std/intl`.

**Tech Stack:** Rust 2021. tokio gains its `time` feature. New crates: `chrono` (under `datetime`), `icu` with `compiled_data` (under `intl`). All synchronous except `time.sleep`.

**Starting state (end of M11, on `main`):** 266 tests default (236 `--no-default`), clippy clean. `data` feature exists; stdlib modules bytes/json/encoding/regex/uuid/csv/toml/yaml + math/string/array/object/map/convert. `Value::Bytes`/`Value::Regex` kinds. Builtins live in a root scope; programs/modules run in a child (shadowing works). Runtime is `#[tokio::main(flavor = "current_thread")]`. `call_stdlib` routes module names; `"array" => self.call_array(...).await` is the async-dispatch precedent.

**Conventions:** single-threaded `Rc`/`RefCell`; `Control` = Panic(Tier-2)/Propagate(`?`); Tier-1 `[value,err]` via `make_pair`/`make_error` for fallible parse; Tier-2 panic for arg-type misuse via `want_*` helpers; per-module `ctx` closure; `run`/`run_err` test helpers; cfg-gated module registration. NO new Value kind (dates are objects), so NO exhaustive-match changes.

## Semantics decided

- **`std/time` (always-on):** `now()`→unix epoch milliseconds (number, `f64`); `monotonic()`→milliseconds from a monotonic clock (number, since an arbitrary epoch — for measuring elapsed time); `sleep(ms)`→async, suspends `ms` milliseconds, returns nil; duration helpers `millis(n)→n`, `seconds(n)→n*1000`, `minutes(n)→n*60000`, `hours(n)→n*3600000` (return ms numbers, so `time.sleep(time.seconds(1))`).
- **`std/date` (feature `datetime`, chrono):** an **instant** is a plain object snapshot `{ epochMs, year, month (1-12), day (1-31), hour, minute, second, millisecond, weekday (0=Sun..6=Sat), iso (RFC3339 UTC string) }`, all UTC-based; `epochMs` is canonical for arithmetic. Functions: `now()`→instant; `fromEpochMs(ms)`→instant; `parse(str, fmt?)`→`[instant, err]` (no fmt → RFC3339/ISO; with fmt → chrono strftime); `format(instant, fmt, tzOffsetMinutes?)`→string (strftime; optional display offset, default UTC); `addDays/addHours/addMinutes/addSeconds/addMonths/addYears(instant, n)`→new instant; `diffMs(a, b)`→number (a−b). Timezones are OFFSET-based (display via `tzOffsetMinutes`); named IANA zones are a documented pragmatic deferral (would need `chrono-tz`).
- **`std/intl` (feature `intl`, icu `compiled_data`):** `formatNumber(n, locale)`→string (locale grouping/decimal); `formatCurrency(n, currencyCode, locale)`→string; `formatDate(instant, locale, style?)`→string (locale date format; style `"short"|"medium"|"long"` default medium); `caseUpper(s, locale)`/`caseLower(s, locale)`→locale-aware case (e.g. Turkish `i`); `compare(a, b, locale)`→number (-1/0/1, collation). `locale` is a BCP-47 string (e.g. `"en-US"`, `"de-DE"`, `"tr"`); an invalid locale → Tier-2 panic (programmer error) OR a graceful fallback to `und` — DECIDE during impl (prefer Tier-1 `[string,err]` for formatNumber/Currency/Date if locale parse can fail, OR panic — pick one and be consistent; recommendation: invalid locale string → Tier-2 panic since locales are usually literals). icu currency/date formatting that isn't in stable `icu` may use a pragmatic fallback (documented).

## File structure

| File | Responsibility | Change |
|---|---|---|
| `Cargo.toml` | tokio `time` feature; `datetime`+`intl` features; `chrono`, `icu` deps | modify |
| `src/stdlib/mod.rs` | register time (always) + date/intl (cfg-gated); maybe `want_object`-based instant helpers | modify |
| `src/stdlib/time.rs` | `std/time` (async sleep) | create |
| `src/stdlib/date.rs` | `std/date` | create |
| `src/stdlib/intl.rs` | `std/intl` | create |
| `examples/datetime.as` | end-to-end example | create |
| `tests/cli.rs` | example integration test | modify |

## Scope & Justified Deferrals

| Deferred | Why | Owner |
|---|---|---|
| `std/fs`, `std/process`, `std/env`, `std/crypto`, `std/compress`, `std/sqlite` | System group | **M13** |
| net/http/ws/tui | Async I/O + UI | **M14/M15** |
| Named IANA timezones (`America/New_York`) | Needs `chrono-tz`; offset-based tz covers the spec's "timezones" minimally for v1 | future (add `chrono-tz` if required) |
| A real future/awaitable `Value` kind | `time.sleep` awaits directly in dispatch; suspension on a value object only matters for sockets/http | **M14** |

Nothing in M12's own module scope is deferred.

---

## Task 1: `std/time` (always-on) + tokio `time` feature + the first async stdlib fn

**Files:** modify `Cargo.toml`, `src/stdlib/mod.rs`; create `src/stdlib/time.rs`.

- [ ] **Step 1: `Cargo.toml`** — add `"time"` to tokio's feature list: `tokio = { version = "1", features = ["rt", "rt-multi-thread", "macros", "time"] }`. Run `cargo build` to confirm.

- [ ] **Step 2: Create `src/stdlib/time.rs`.** `now`/`monotonic`/duration helpers are pure (a free `fn call` for the sync ones); `sleep` is async and lives on `impl Interp`. Split: a sync `pub fn call(func, args, span)` for now/monotonic/millis/seconds/minutes/hours, and the async `sleep` handled in `call_time` (mod.rs) before delegating to the sync `call`.
```rust
//! `std/time` — wall-clock + monotonic time, async sleep, duration helpers.

use super::{arg, bi, want_number};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("now", bi("time.now")),
        ("monotonic", bi("time.monotonic")),
        ("sleep", bi("time.sleep")),
        ("millis", bi("time.millis")),
        ("seconds", bi("time.seconds")),
        ("minutes", bi("time.minutes")),
        ("hours", bi("time.hours")),
    ]
}

// A process-start instant for monotonic(): lazily initialized.
thread_local! {
    static START: std::time::Instant = std::time::Instant::now();
}

/// Synchronous time functions. `sleep` is handled async in `call_time` (mod.rs)
/// and must NOT be dispatched here.
pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("time.{}", f);
    match func {
        "now" => {
            let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0);
            Ok(Value::Number(ms))
        }
        "monotonic" => {
            let ms = START.with(|s| s.elapsed().as_secs_f64() * 1000.0);
            Ok(Value::Number(ms))
        }
        "millis" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("millis"))?)),
        "seconds" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("seconds"))? * 1000.0)),
        "minutes" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("minutes"))? * 60_000.0)),
        "hours" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("hours"))? * 3_600_000.0)),
        "sleep" => unreachable!("time.sleep is dispatched async in call_time"),
        _ => Err(AsError::at(format!("std/time has no function '{}'", func), span).into()),
    }
}
```

- [ ] **Step 3: `src/stdlib/mod.rs`** — register time (ALWAYS-on, no feature gate) and add an async `call_time` that handles `sleep` then delegates:
  - `pub mod time;`
  - `std_module_exports`: `"std/time" => time::exports(),`
  - `call_stdlib`: `"time" => self.call_time(func, args, span).await,`
  - Add the method (next to where you'd put it, e.g. in mod.rs's `impl Interp`):
```rust
impl Interp {
    pub(crate) async fn call_time(&mut self, func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        if func == "sleep" {
            let ms = want_number(&arg(args, 0), span, "time.sleep")?;
            if ms < 0.0 {
                return Err(AsError::at("time.sleep duration must be non-negative", span).into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
            return Ok(Value::Nil);
        }
        time::call(func, args, span)
    }
}
```
(Ensure `want_number`/`arg`/`AsError`/`Span` are imported in mod.rs — they are, used by the other helpers.)

- [ ] **Step 4: Tests.** Unit tests in `time.rs` for `now` (> a known past epoch ms, e.g. > 1.7e12), `monotonic` (>= 0, and increases across two calls with a tiny busy loop), duration helpers (`seconds(2)`==2000). Interp e2e in `src/interp.rs` (the `run` helper drives `#[tokio::test]`, so `sleep` works):
```rust
    #[tokio::test]
    async fn std_time_now_and_durations() {
        let out = run("import * as time from \"std/time\"\nprint(time.seconds(2))\nprint(time.now() > 1700000000000)").await;
        assert_eq!(out, "2000\ntrue\n");
    }

    #[tokio::test]
    async fn std_time_sleep_suspends() {
        // sleep a tiny amount; just assert it completes and returns nil
        let out = run("import * as time from \"std/time\"\nawait time.sleep(5)\nprint(\"done\")").await;
        assert_eq!(out, "done\n");
    }

    #[tokio::test]
    async fn std_time_monotonic_elapsed() {
        // monotonic measures elapsed; after a sleep it must advance
        let out = run("import * as time from \"std/time\"\n\
                       let a = time.monotonic()\n\
                       await time.sleep(10)\n\
                       let b = time.monotonic()\n\
                       print(b > a)").await;
        assert_eq!(out, "true\n");
    }
```
(VERIFY `await time.sleep(5)` parses + runs: `await` is currently identity over an already-resolved value, but `time.sleep` actually suspends during the call itself in `call_time`. So `time.sleep(5)` alone also suspends; `await` is harmless. Confirm both forms work.)

- [ ] **Step 5:** FULL `cargo test` + `cargo clippy --all-targets` + `cargo build --no-default-features` (time is always-on, so it must work in BOTH configs). Green/clean. Commit `feat: std/time module (now/monotonic/async sleep/durations) + tokio time feature`.

---

## Task 2: `std/date` (feature `datetime`, chrono)

**Files:** modify `Cargo.toml`, `src/stdlib/mod.rs`; create `src/stdlib/date.rs`.

- [ ] **Step 1: `Cargo.toml`** — add a `datetime` feature + `chrono` optional dep:
```toml
[features]
default = ["data", "datetime", "intl"]
datetime = ["dep:chrono"]
intl = ["dep:icu"]
```
(merge with the existing `[features]`; keep `data` as-is). Add:
```toml
chrono = { version = "0.4", default-features = false, features = ["clock", "std"], optional = true }
```
Run `cargo build` (network fetch). Record the resolved version.

- [ ] **Step 2: Create `src/stdlib/date.rs`.** Build instant objects via a `make_instant(epoch_ms) -> Value` helper using `chrono::DateTime<Utc>`. Implement now/fromEpochMs/parse/format/add*/diffMs. Parse/format fallible → Tier-1. Full code:
```rust
//! `std/date` — civil dates over UTC epoch-millis, backed by chrono. An
//! "instant" is a plain object snapshot; `epochMs` is canonical for arithmetic.

use super::{arg, bi, want_number, want_object, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use chrono::{Datelike, TimeZone, Timelike, Utc};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("now", bi("date.now")),
        ("fromEpochMs", bi("date.fromEpochMs")),
        ("parse", bi("date.parse")),
        ("format", bi("date.format")),
        ("addDays", bi("date.addDays")),
        ("addHours", bi("date.addHours")),
        ("addMinutes", bi("date.addMinutes")),
        ("addSeconds", bi("date.addSeconds")),
        ("addMonths", bi("date.addMonths")),
        ("addYears", bi("date.addYears")),
        ("diffMs", bi("date.diffMs")),
    ]
}

/// Build an instant object snapshot from a UTC epoch-millis value.
fn make_instant(epoch_ms: i64) -> Value {
    let dt = Utc.timestamp_millis_opt(epoch_ms).single().unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap());
    let mut o: IndexMap<String, Value> = IndexMap::new();
    o.insert("epochMs".into(), Value::Number(epoch_ms as f64));
    o.insert("year".into(), Value::Number(dt.year() as f64));
    o.insert("month".into(), Value::Number(dt.month() as f64));
    o.insert("day".into(), Value::Number(dt.day() as f64));
    o.insert("hour".into(), Value::Number(dt.hour() as f64));
    o.insert("minute".into(), Value::Number(dt.minute() as f64));
    o.insert("second".into(), Value::Number(dt.second() as f64));
    o.insert("millisecond".into(), Value::Number((dt.timestamp_subsec_millis()) as f64));
    o.insert("weekday".into(), Value::Number(dt.weekday().num_days_from_sunday() as f64));
    o.insert("iso".into(), Value::Str(dt.to_rfc3339().into()));
    Value::Object(Rc::new(RefCell::new(o)))
}

/// Read the canonical `epochMs` field from an instant object (Tier-2 panic if absent).
fn instant_epoch(v: &Value, span: Span, ctx: &str) -> Result<i64, Control> {
    let o = want_object(v, span, ctx)?;
    let b = o.borrow();
    match b.get("epochMs") {
        Some(Value::Number(n)) => Ok(*n as i64),
        _ => Err(AsError::at(format!("{} expects an instant object (with epochMs), got an object without it", ctx), span).into()),
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("date.{}", f);
    match func {
        "now" => Ok(make_instant(Utc::now().timestamp_millis())),
        "fromEpochMs" => {
            let ms = want_number(&arg(args, 0), span, &ctx("fromEpochMs"))?;
            Ok(make_instant(ms as i64))
        }
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            let parsed = match args.get(1) {
                None | Some(Value::Nil) => {
                    // default: RFC3339 / ISO8601
                    chrono::DateTime::parse_from_rfc3339(&s).map(|dt| dt.with_timezone(&Utc))
                        .or_else(|_| chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S").map(|ndt| Utc.from_utc_datetime(&ndt)))
                        .map_err(|e| e.to_string())
                }
                Some(fmt_v) => {
                    let fmt = want_string(fmt_v, span, &ctx("parse"))?;
                    chrono::NaiveDateTime::parse_from_str(&s, &fmt).map(|ndt| Utc.from_utc_datetime(&ndt)).map_err(|e| e.to_string())
                }
            };
            match parsed {
                Ok(dt) => Ok(make_pair(make_instant(dt.timestamp_millis()), Value::Nil)),
                Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(format!("cannot parse date: {}", e).into())))),
            }
        }
        "format" => {
            let epoch = instant_epoch(&arg(args, 0), span, &ctx("format"))?;
            let fmt = want_string(&arg(args, 1), span, &ctx("format"))?;
            let offset_min = match args.get(2) {
                None | Some(Value::Nil) => 0i64,
                Some(v) => want_number(v, span, &ctx("format"))? as i64,
            };
            let dt = Utc.timestamp_millis_opt(epoch).single().unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap());
            let shifted = dt + chrono::Duration::minutes(offset_min);
            // format with the shifted naive time (offset applied for display)
            Ok(Value::Str(shifted.naive_utc().format(&fmt).to_string().into()))
        }
        "addDays" | "addHours" | "addMinutes" | "addSeconds" | "addMonths" | "addYears" => {
            let epoch = instant_epoch(&arg(args, 0), span, &ctx(func))?;
            let n = want_number(&arg(args, 1), span, &ctx(func))? as i64;
            let dt = Utc.timestamp_millis_opt(epoch).single().unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap());
            let new_dt = match func {
                "addDays" => dt + chrono::Duration::days(n),
                "addHours" => dt + chrono::Duration::hours(n),
                "addMinutes" => dt + chrono::Duration::minutes(n),
                "addSeconds" => dt + chrono::Duration::seconds(n),
                "addMonths" => add_months(dt, n),
                "addYears" => add_months(dt, n * 12),
                _ => unreachable!(),
            };
            Ok(make_instant(new_dt.timestamp_millis()))
        }
        "diffMs" => {
            let a = instant_epoch(&arg(args, 0), span, &ctx("diffMs"))?;
            let b = instant_epoch(&arg(args, 1), span, &ctx("diffMs"))?;
            Ok(Value::Number((a - b) as f64))
        }
        _ => Err(AsError::at(format!("std/date has no function '{}'", func), span).into()),
    }
}

/// Month arithmetic clamping the day to the target month's length.
fn add_months(dt: chrono::DateTime<Utc>, months: i64) -> chrono::DateTime<Utc> {
    let total = (dt.year() as i64) * 12 + (dt.month0() as i64) + months;
    let year = total.div_euclid(12) as i32;
    let month0 = total.rem_euclid(12) as u32;
    let day = dt.day().min(days_in_month(year, month0 + 1));
    Utc.with_ymd_and_hms(year, month0 + 1, day, dt.hour(), dt.minute(), dt.second()).single()
        .map(|d| d + chrono::Duration::milliseconds(dt.timestamp_subsec_millis() as i64))
        .unwrap_or(dt)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 { 29 } else { 28 },
        _ => 30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }

    #[test]
    fn from_epoch_components_and_iso() {
        // 2021-01-01T00:00:00Z = 1609459200000 ms
        let inst = call("fromEpochMs", &[Value::Number(1609459200000.0)], sp()).unwrap();
        let txt = inst.to_string();
        assert!(txt.contains("year: 2021"), "{txt}");
        assert!(txt.contains("month: 1"), "{txt}");
        assert!(txt.contains("day: 1"), "{txt}");
    }

    #[test]
    fn parse_format_roundtrip() {
        let pair = call("parse", &[s("2021-06-15T12:30:00Z")], sp()).unwrap();
        assert!(pair.to_string().contains("year: 2021"));
        // extract the instant (index 0) and format it
        if let Value::Array(a) = &pair {
            let inst = a.borrow()[0].clone();
            let formatted = call("format", &[inst, s("%Y-%m-%d")], sp()).unwrap();
            assert_eq!(formatted, s("2021-06-15"));
        } else { panic!("expected pair"); }
    }

    #[test]
    fn parse_invalid_is_tier1_err() {
        assert!(call("parse", &[s("not a date")], sp()).unwrap().to_string().starts_with("[nil, {message:"));
    }

    #[test]
    fn arithmetic_and_diff() {
        let base = call("fromEpochMs", &[Value::Number(1609459200000.0)], sp()).unwrap();
        let plus1 = call("addDays", &[base.clone(), Value::Number(1.0)], sp()).unwrap();
        let d = call("diffMs", &[plus1, base], sp()).unwrap();
        assert_eq!(d, Value::Number(86_400_000.0)); // one day in ms
    }

    #[test]
    fn add_months_clamps_day() {
        // Jan 31 + 1 month → Feb 28 (2021 non-leap)
        let jan31 = call("parse", &[s("2021-01-31T00:00:00Z")], sp()).unwrap();
        if let Value::Array(a) = &jan31 {
            let inst = a.borrow()[0].clone();
            let feb = call("addMonths", &[inst, Value::Number(1.0)], sp()).unwrap();
            assert!(feb.to_string().contains("month: 2"));
            assert!(feb.to_string().contains("day: 28"));
        } else { panic!(); }
    }
}
```
(VERIFY the chrono API against the resolved version — `timestamp_millis_opt`, `with_ymd_and_hms`, `from_utc_datetime`, `parse_from_rfc3339`, `Duration::days`, `num_days_from_sunday`, `month0`. chrono 0.4 has all of these but exact names/deprecations vary by minor version; adapt + report. If `timestamp_millis_opt` returns `LocalResult`, `.single()` extracts it.)

- [ ] **Step 3: `src/stdlib/mod.rs`** — register date cfg-gated under `datetime`: `#[cfg(feature = "datetime")] pub mod date;` + cfg-gated arms in `std_module_exports` (`"std/date"`) and `call_stdlib` (`"date"`).

- [ ] **Step 4: interp e2e** in `src/interp.rs` (cfg-gated `#[cfg(feature = "datetime")]`):
```rust
    #[cfg(feature = "datetime")]
    #[tokio::test]
    async fn std_date_end_to_end() {
        let src = "import * as date from \"std/date\"\n\
                   let [d, err] = date.parse(\"2021-06-15T12:30:00Z\")\n\
                   print(d.year)\n\
                   print(d.month)\n\
                   print(date.format(d, \"%Y/%m/%d\"))\n\
                   let later = date.addDays(d, 10)\n\
                   print(later.day)\n\
                   print(date.diffMs(later, d))";
        assert_eq!(run(src).await, "2021\n6\n2021/06/15\n25\n864000000\n");
    }
```
Run to confirm exact output (10 days = 864000000 ms; June 15 + 10 = June 25).

- [ ] **Step 5:** FULL `cargo test` + `cargo test --no-default-features` + `cargo clippy --all-targets` + `cargo build --no-default-features`. Green/clean/compile. Commit `feat: std/date module (chrono, datetime feature)`.

---

## Task 3: `std/intl` (feature `intl`, pragmatic `icu` subset)

**Files:** modify `Cargo.toml`, `src/stdlib/mod.rs`; create `src/stdlib/intl.rs`.

API: `formatNumber(n, locale)`, `formatCurrency(n, currencyCode, locale)`, `formatDate(instant, locale, style?)`, `caseUpper(s, locale)`, `caseLower(s, locale)`, `compare(a, b, locale)`.

- [ ] **Step 1: `Cargo.toml`** — `icu` optional dep with `compiled_data` (so no manual data provider):
```toml
icu = { version = "1", features = ["compiled_data"], optional = true }
```
(already wired into the `intl` feature in Task 2's `[features]`). Run `cargo build`. Record version. NOTE: `icu` is a large dependency tree; the first build will be slow — that's expected.

- [ ] **Step 2: Create `src/stdlib/intl.rs`.** Use icu's compiled-data formatters. Recommended icu 1.x APIs:
  - **Locale parse:** `let loc: icu::locid::Locale = locale_str.parse().map_err(...)` — invalid locale string → Tier-2 panic ("intl.X: invalid locale '...'").
  - **Number:** `icu::decimal::FixedDecimalFormatter::try_new(&loc.into(), Default::default())` then format a `fixed_decimal::FixedDecimal` built from the number. (Build the FixedDecimal from the f64 — e.g. via `FixedDecimal::try_from_f64(n, FloatPrecision::Floating)`; handle the magnitude/precision pragmatically.)
  - **Case:** `icu::casemap::CaseMapper::new().uppercase_to_string(s, &loc.id)` / `.lowercase_to_string(...)`.
  - **Collation:** `icu::collator::Collator::try_new(&loc.into(), Default::default())` then `.compare(a, b)` → `core::cmp::Ordering` → -1/0/1.
  - **Date:** `icu::datetime` typed/neo formatter with a length/style; convert the instant's epochMs → an icu date input. This is the fiddliest; if the stable `icu::datetime` API for a simple "format this instant in this locale at this style" is awkward, a PRAGMATIC fallback is acceptable (documented): format via chrono in a locale-influenced pattern, OR use icu's `DateTimeFormatter` with `length::Date`. Pick the cleanest path the resolved icu version offers.
  - **Currency:** stable `icu` may not expose currency formatting simply; PRAGMATIC fallback (documented): format the number via FixedDecimalFormatter for the locale, then prefix/suffix the currency code/symbol per a small built-in symbol table (USD→$, EUR→€, GBP→£, JPY→¥, …; unknown → the code). Document this as a pragmatic subset per the spec's "trimmed icu4x".

  Implement with the established module shape (exports/call/ctx). Fallible-on-data is rare here (locale parse is the main failure; treat as Tier-2 panic since locales are literals). Provide thorough unit tests with KNOWN outputs (run to capture exact icu output, since formatting strings are version/locale-data-specific — assert on the ACTUAL observed output, and pick stable cases):
  - `formatNumber(1234567.89, "en-US")` → likely `"1,234,567.89"`; `"de-DE"` → `"1.234.567,89"`. CAPTURE the real output and assert it (icu's exact rounding/precision may differ — use a value that formats cleanly, and set the assertion to what icu actually produces; document).
  - `caseUpper("istanbul", "tr")` → `"İSTANBUL"` (Turkish dotted I) vs `caseUpper("istanbul", "en")` → `"ISTANBUL"`.
  - `compare("apple", "banana", "en")` → -1; `compare("b","a","en")` → 1; equal → 0.
  - `formatCurrency(1234.5, "USD", "en-US")` → e.g. `"$1,234.50"` (per your pragmatic impl — assert your actual output).
  - `formatDate(instant, "en-US", "medium")` → a locale date string (assert the actual icu output).

  Because icu output is data-dependent, the IMPLEMENTER must RUN each case and pin the assertion to the real output, documenting the icu version. Prefer a few robust cases over many brittle ones.

- [ ] **Step 3: `src/stdlib/mod.rs`** — register intl cfg-gated under `intl`: `#[cfg(feature = "intl")] pub mod intl;` + cfg-gated arms in `std_module_exports`/`call_stdlib`.

- [ ] **Step 4: interp e2e** (cfg-gated `#[cfg(feature = "intl")]`) — a robust case, e.g. locale-aware number grouping difference between en-US and de-DE, and Turkish case folding. Pin to actual output.

- [ ] **Step 5:** FULL `cargo test` + `cargo test --no-default-features` + `cargo clippy --all-targets` + `cargo build --no-default-features`. Green/clean/compile. Commit `feat: std/intl module (pragmatic icu subset, intl feature)`.

NOTE on scope/pragmatism: the spec explicitly says "pragmatic subset of ICU" / "trimmed icu4x". If a specific icu API (currency, date) is genuinely impractical in the resolved version, implement the documented pragmatic fallback and RECORD it in the report + a code comment + the roadmap hand-off — do NOT block the milestone. The REQUIRED surface is the 6 functions; their backing may be pragmatic.

---

## Task 4: End-to-end example + integration test + holistic

**Files:** create `examples/datetime.as`; modify `tests/cli.rs`.

- [ ] **Step 1: Create `examples/datetime.as`** exercising time, date, intl (cfg note: examples run with default features = all three on):
```
import * as time from "std/time"
import * as date from "std/date"
import * as intl from "std/intl"

// time: durations + monotonic elapsed around a tiny sleep
let start = time.monotonic()
await time.sleep(5)
let elapsed = time.monotonic() - start
print(elapsed >= 5)
print(time.seconds(3))

// date: parse, components, arithmetic, format
let [d, err] = date.parse("2021-06-15T12:30:00Z")
print(d.year)
print(date.format(d, "%Y/%m/%d"))
let nextWeek = date.addDays(d, 7)
print(nextWeek.day)

// intl: locale-aware number formatting
print(intl.formatNumber(1234567, "en-US"))
print(intl.formatNumber(1234567, "de-DE"))
print(intl.caseUpper("istanbul", "tr"))
```
RUN it; capture exact output; verify each line. (intl outputs are data-dependent — set the integration-test assertions to the ACTUAL observed output.)

- [ ] **Step 2:** add `runs_datetime_example` to `tests/cli.rs`, gated `#[cfg(all(feature = "datetime", feature = "intl"))]` (it imports date+intl). Assert on stable substrings (the date components `2021`, `2021/06/15`, `22` for June 22, the number-format difference between en-US `1,234,567` and de-DE `1.234.567`, the Turkish `İSTANBUL`). UUID-style randomness isn't involved.

- [ ] **Step 3:** `cargo test` (conformance parses the example under both parsers — only existing syntax used). FINAL: `cargo test` + `cargo test --no-default-features` + `cargo clippy --all-targets` + `cargo build --no-default-features` all green/clean/compile. Commit `test: time/date/intl end-to-end example + integration test`.

---

## Definition of Done

- `cargo test` (default) passes: all module unit tests, cli (incl. the new example), conformance; `cargo clippy --all-targets` clean; `cargo build --no-default-features` compiles and `cargo test --no-default-features` passes (date/intl cfg out; time always-on).
- Implemented per spec §11.2 Time & locale: `std/time` (now/monotonic/sleep⚡/durations), `std/date` (civil dates, parse/format, arithmetic, offset timezones), `std/intl` (locale number/currency/date formatting, case folding, collation — pragmatic icu subset).
- `time.sleep` is async and actually suspends on the tokio loop (verified via monotonic elapsed). No new Value kind introduced (dates are objects).
- Tier-1 for fallible parse (date.parse); Tier-2 panic for arg-type/locale misuse. Any intl pragmatic fallback documented.
- Nothing in M12's own scope deferred (named IANA tz + real awaitable kind explicitly deferred with owners).

## Hand-off to Milestone 13 ("System")

M13 adds `std/fs` (read/write/append, exists, stat, mkdir, remove, walk, path manipulation, **grep** — recursive content search returning `[matches, err]` with `{path,line,column,text}`; reuse `std/regex` + a `walkdir`/`ignore` walker, spec §11.3), `std/process` (run + spawn, §11.4 — cross-platform, `tokio::process`; spawn returns a child handle with async reader stdout/stderr following the `read(n?)/readLine()/readToEnd()` idiom — this idiom is ALSO used by M14's http streaming, so design the reader carefully), `std/env` (get/set, dotenv via `dotenvy`), `std/crypto` (sha256/512, md5, hmac, random bytes, argon2/bcrypt — RustCrypto), `std/compress` (gzip/deflate + zip — `flate2`/`zip`), `std/sqlite` (`rusqlite`). Features: `net`? no — `sql`, `crypto`, plus a `sys`/`fs` group. `std/process` + `std/fs` reader handles may motivate a small reader abstraction; the §11.4 reader idiom (`read/readLine/readToEnd`, async) is shared with M14 http streaming — consider a reusable async-reader value/object now. `std/process` is async (tokio::process) — second async stdlib area after `time.sleep`.
