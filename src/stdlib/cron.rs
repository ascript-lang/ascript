//! `std/cron` — 5-field Vixie cron parsing + `next`/`nextN`/`matches`/`schedule`.
//!
//! BATT T2-1 (spec §11). A schedule is a tagged Object (`{__cron:"schedule", expr,
//! fields}`) — NO new `Value` variant — reusable and inspectable. Parsing is
//! Tier-1 (malformed expressions are frequently user/config data → `[nil, err]`);
//! wrong argument TYPES are Tier-2 panics. The `@reboot` macro is rejected as a
//! Tier-2 programmer-literal misuse (it is meaningless without a process to reboot).
//!
//! ## Fields & forms (§11.2)
//!
//! `min hour dom month dow` — `*`, lists `1,2,3`, ranges `1-5`, steps `*/5` and
//! `10-30/5`, names (`jan`..`dec`, `sun`..`sat`, case-insensitive), `dow` 0 and 7
//! both Sunday. Plus the `@yearly @annually @monthly @weekly @daily @hourly`
//! shortcuts.
//!
//! ## The DOM/DOW OR rule (load-bearing, §11.2)
//!
//! When BOTH day-of-month AND day-of-week are restricted (neither is `*`), a time
//! matches if it matches DOM **OR** DOW (the Vixie OR rule) — e.g. `0 0 13 * 5`
//! fires on the 13th of every month AND on every Friday. When only one of the two
//! is restricted, only that one constrains the day.
//!
//! ## TZ honesty (§11.3)
//!
//! UTC by default; `tzOffset` (minutes) shifts the civil-time interpretation — a
//! **fixed offset with NO DST transitions**, the exact posture `std/date` already
//! documents. The chrono-tz named-zone upgrade is one shared recorded-future for
//! `std/date` + `std/cron`. Civil math runs on chrono `DateTime<Utc>`; an offset
//! shifts UTC into "local civil" before the field match and back afterward.
//!
//! ## Determinism (§11.3) — schedule routes sleep through the time seam
//!
//! `cron.schedule` computes the delay from [`Interp::clock_now_ms`] (the SP9 §3
//! determinism clock seam) and sleeps via the SAME `call_time("sleep", …)` path
//! `std/time` uses — so under `--seed` / `--frozen-time` / a workflow the virtual
//! clock advances instantly and fire times are replay-deterministic BY
//! CONSTRUCTION (no new seam). The handle's loop is a `spawn_local` task held by an
//! [`AbortOnDrop`]; `stop()` flips a graceful flag, `close()` aborts.

use super::{arg, bi, want_number, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::{Value, ValueKind};
use indexmap::IndexMap;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("parse", bi("cron.parse")),
        ("next", bi("cron.next")),
        ("nextN", bi("cron.nextN")),
        ("matches", bi("cron.matches")),
        ("schedule", bi("cron.schedule")),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// The parsed schedule — bitmask sets over each field.
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed 5-field cron schedule as bitmask sets.
///
/// `minutes` uses 60 bits (0..59), `hours` 24 bits (0..23), `dom` bits 1..31
/// (bit 0 unused), `months` bits 1..12 (bit 0 unused), `dow` bits 0..6 (Sun..Sat).
/// The `dom_restricted` / `dow_restricted` flags drive the Vixie OR rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CronSchedule {
    minutes: u64,
    hours: u32,
    dom: u32,
    months: u16,
    dow: u8,
    dom_restricted: bool,
    dow_restricted: bool,
}

/// A field's numeric domain `[min, max]` and optional name table.
struct FieldSpec {
    min: u32,
    max: u32,
    names: &'static [(&'static str, u32)],
}

const MONTH_NAMES: &[(&str, u32)] = &[
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

const DOW_NAMES: &[(&str, u32)] = &[
    ("sun", 0),
    ("mon", 1),
    ("tue", 2),
    ("wed", 3),
    ("thu", 4),
    ("fri", 5),
    ("sat", 6),
];

/// Parse one cron field into a bitmask over `[spec.min, spec.max]`. Returns the
/// set bits in domain units (so the caller can detect `*` and the
/// dom/dow-restricted flags). A field is "restricted" iff it is not a bare `*`
/// (and not `*/1`, which is equivalent to `*`).
fn parse_field(field: &str, spec: &FieldSpec) -> Result<(u64, bool), String> {
    if field.is_empty() {
        return Err("empty field".to_string());
    }
    // `*` and `*/1` mean every value → unrestricted.
    let mut restricted = true;
    let mut bits: u64 = 0;
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("empty list element in '{field}'"));
        }
        // Split off an optional `/step`.
        let (range_part, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .parse()
                    .map_err(|_| format!("invalid step '{s}' in '{field}'"))?;
                if step == 0 {
                    return Err(format!("step must be > 0 in '{field}'"));
                }
                (r, step)
            }
            None => (part, 1),
        };
        // Resolve the range the step applies over.
        let (lo, hi, is_star) = if range_part == "*" {
            (spec.min, spec.max, true)
        } else if let Some((a, b)) = range_part.split_once('-') {
            let lo = resolve_token(a, spec)?;
            let hi = resolve_token(b, spec)?;
            (lo, hi, false)
        } else {
            let v = resolve_token(range_part, spec)?;
            // A bare single value with a step (`5/15`) spans value..max.
            if step > 1 {
                (v, spec.max, false)
            } else {
                (v, v, false)
            }
        };
        if lo < spec.min || hi > spec.max || lo > hi {
            return Err(format!(
                "value out of range [{}, {}] in '{field}'",
                spec.min, spec.max
            ));
        }
        // A bare `*` (step 1) is the only thing that leaves the field unrestricted.
        if is_star && step == 1 {
            restricted = false;
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1u64 << v;
            v += step;
        }
    }
    Ok((bits, restricted))
}

/// Resolve a single token (a number or a case-insensitive name) to its value.
fn resolve_token(tok: &str, spec: &FieldSpec) -> Result<u32, String> {
    let t = tok.trim();
    if t.is_empty() {
        return Err("empty token".to_string());
    }
    if let Ok(n) = t.parse::<u32>() {
        return Ok(n);
    }
    let lower = t.to_ascii_lowercase();
    for (name, val) in spec.names {
        if *name == lower {
            return Ok(*val);
        }
    }
    Err(format!("invalid token '{tok}'"))
}

/// Expand a `@`-macro literal to its 5-field equivalent. `@reboot` is rejected
/// (Tier-2 — handled by the caller); the rest map to standard expressions.
fn expand_macro(expr: &str) -> Option<&'static str> {
    match expr {
        "@yearly" | "@annually" => Some("0 0 1 1 *"),
        "@monthly" => Some("0 0 1 * *"),
        "@weekly" => Some("0 0 * * 0"),
        "@daily" | "@midnight" => Some("0 0 * * *"),
        "@hourly" => Some("0 * * * *"),
        _ => None,
    }
}

impl CronSchedule {
    /// Parse a 5-field cron expression (after `@`-macro expansion by the caller).
    /// Returns `Err(msg)` on any malformed field (Tier-1 at the call site).
    pub(crate) fn parse(expr: &str) -> Result<CronSchedule, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "expected 5 fields (min hour dom month dow), got {}",
                fields.len()
            ));
        }
        let minute_spec = FieldSpec { min: 0, max: 59, names: &[] };
        let hour_spec = FieldSpec { min: 0, max: 23, names: &[] };
        let dom_spec = FieldSpec { min: 1, max: 31, names: &[] };
        let month_spec = FieldSpec { min: 1, max: 12, names: MONTH_NAMES };
        // dow domain is 0..7 at parse time (7 ≡ 0); normalized below.
        let dow_spec = FieldSpec { min: 0, max: 7, names: DOW_NAMES };

        let (minutes, _) = parse_field(fields[0], &minute_spec)?;
        let (hours, _) = parse_field(fields[1], &hour_spec)?;
        let (dom, dom_restricted) = parse_field(fields[2], &dom_spec)?;
        let (months, _) = parse_field(fields[3], &month_spec)?;
        let (dow_raw, dow_restricted) = parse_field(fields[4], &dow_spec)?;

        // Normalize dow: bit 7 (Sunday alias) folds onto bit 0.
        let mut dow = dow_raw;
        if dow & (1 << 7) != 0 {
            dow |= 1 << 0;
            dow &= !(1 << 7);
        }

        Ok(CronSchedule {
            minutes,
            hours: hours as u32,
            dom: dom as u32,
            months: months as u16,
            dow: dow as u8,
            dom_restricted,
            dow_restricted,
        })
    }

    /// Does this schedule match the given civil-time components? Implements the
    /// Vixie DOM/DOW OR rule. `weekday` is 0..6 (Sun..Sat).
    fn matches_civil(&self, minute: u32, hour: u32, day: u32, month: u32, weekday: u32) -> bool {
        if self.minutes & (1u64 << minute) == 0 {
            return false;
        }
        if self.hours & (1u32 << hour) == 0 {
            return false;
        }
        if self.months & (1u16 << month) == 0 {
            return false;
        }
        let dom_hit = self.dom & (1u32 << day) != 0;
        let dow_hit = self.dow & (1u8 << weekday) != 0;
        // The Vixie OR rule: when BOTH dom and dow are restricted, match either.
        // When only one is restricted, only that one applies; when neither is,
        // any day matches.
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom_hit || dow_hit,
            (true, false) => dom_hit,
            (false, true) => dow_hit,
            (false, false) => true,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// next: minute-by-minute scan over civil time with month/day skip acceleration.
// ─────────────────────────────────────────────────────────────────────────────

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};

/// Scan bound: 5 years of minutes (catches impossible dates like `0 0 30 2 *`).
const MAX_SCAN_MINUTES: i64 = 5 * 366 * 24 * 60;

/// Find the next epoch-ms strictly after `after_ms` that matches `sched`, with the
/// civil interpretation shifted by `tz_offset_min`. Returns `None` if no match
/// occurs within 5 years (Tier-1 at the call site).
///
/// Algorithm: convert `after_ms` to civil time (UTC + offset), advance to the next
/// whole minute, then scan minute-by-minute. Month/day mismatches skip to the next
/// candidate day at 00:00 (month/day skip acceleration) rather than stepping a
/// minute at a time across a non-matching month or day.
fn next_after(sched: &CronSchedule, after_ms: i64, tz_offset_min: i64) -> Option<i64> {
    let off = Duration::minutes(tz_offset_min);
    // Civil "local" time = UTC + offset.
    let base_utc = Utc.timestamp_millis_opt(after_ms).single()?;
    let mut civil = base_utc + off;
    // Advance to the start of the NEXT minute (strictly after `after`), dropping
    // sub-minute precision.
    civil = (civil + Duration::minutes(1))
        .with_second(0)?
        .with_nanosecond(0)?;

    let mut scanned: i64 = 0;
    while scanned < MAX_SCAN_MINUTES {
        let month = civil.month();
        let day = civil.day();
        let weekday = civil.weekday().num_days_from_sunday();
        // Month skip: if the month doesn't match, jump to the 1st of the next
        // matching month at 00:00.
        if sched.months & (1u16 << month) == 0 {
            let next = first_of_next_month(civil)?;
            scanned += minutes_between(civil, next);
            civil = next;
            continue;
        }
        // Day skip: if the day (per the OR rule) doesn't match, jump to 00:00 of
        // the next day.
        if !day_matches(sched, day, weekday) {
            let next = next_day_midnight(civil)?;
            scanned += minutes_between(civil, next);
            civil = next;
            continue;
        }
        // The day matches; scan hours/minutes within it.
        let hour = civil.hour();
        let minute = civil.minute();
        if sched.hours & (1u32 << hour) != 0 && sched.minutes & (1u64 << minute) != 0 {
            // Convert the matching civil time back to UTC and to epoch-ms.
            let utc = civil - off;
            return Some(utc.timestamp_millis());
        }
        civil += Duration::minutes(1);
        scanned += 1;
        // If we rolled past midnight the day/month guards re-check on the next
        // iteration.
    }
    None
}

/// The day-match half of the Vixie OR rule (factored out so the scanner can skip
/// whole non-matching days).
fn day_matches(sched: &CronSchedule, day: u32, weekday: u32) -> bool {
    let dom_hit = sched.dom & (1u32 << day) != 0;
    let dow_hit = sched.dow & (1u8 << weekday) != 0;
    match (sched.dom_restricted, sched.dow_restricted) {
        (true, true) => dom_hit || dow_hit,
        (true, false) => dom_hit,
        (false, true) => dow_hit,
        (false, false) => true,
    }
}

fn first_of_next_month(dt: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let (y, m) = if dt.month() == 12 {
        (dt.year() + 1, 1)
    } else {
        (dt.year(), dt.month() + 1)
    };
    Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).single()
}

fn next_day_midnight(dt: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let midnight = dt.with_hour(0)?.with_minute(0)?.with_second(0)?.with_nanosecond(0)?;
    Some(midnight + Duration::days(1))
}

fn minutes_between(a: DateTime<Utc>, b: DateTime<Utc>) -> i64 {
    (b - a).num_minutes().max(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Schedule value encode/decode (tagged Object — no new Value variant).
// ─────────────────────────────────────────────────────────────────────────────

/// Build the tagged-Object representation of a parsed schedule. Reusable and
/// inspectable: carries the canonical `expr` plus a `fields` array of the five
/// raw field strings.
fn schedule_value(expr: &str, sched: &CronSchedule) -> Value {
    let mut o: IndexMap<String, Value> = IndexMap::new();
    o.insert("__cron".to_string(), Value::str("schedule"));
    o.insert("expr".to_string(), Value::str(expr));
    // Store the bitmask sets so a re-parse isn't needed when a schedule Object is
    // passed back into next/matches.
    o.insert("minutes".to_string(), Value::float(sched.minutes as f64));
    o.insert("hours".to_string(), Value::float(sched.hours as f64));
    o.insert("dom".to_string(), Value::float(sched.dom as f64));
    o.insert("months".to_string(), Value::float(sched.months as f64));
    o.insert("dow".to_string(), Value::float(sched.dow as f64));
    o.insert(
        "domRestricted".to_string(),
        Value::bool_(sched.dom_restricted),
    );
    o.insert(
        "dowRestricted".to_string(),
        Value::bool_(sched.dow_restricted),
    );
    Value::object(o)
}

/// Reconstruct a [`CronSchedule`] from a tagged-Object schedule value.
fn schedule_from_object(o: &IndexMap<String, Value>) -> Option<CronSchedule> {
    if o.get("__cron").and_then(|v| match v.kind() {
        ValueKind::Str(s) => Some(s.to_string()),
        _ => None,
    })? != "schedule"
    {
        return None;
    }
    let f = |k: &str| o.get(k).and_then(|v| v.as_f64());
    Some(CronSchedule {
        minutes: f("minutes")? as u64,
        hours: f("hours")? as u32,
        dom: f("dom")? as u32,
        months: f("months")? as u16,
        dow: f("dow")? as u8,
        dom_restricted: matches!(o.get("domRestricted").map(|v| v.kind()), Some(ValueKind::Bool(true))),
        dow_restricted: matches!(o.get("dowRestricted").map(|v| v.kind()), Some(ValueKind::Bool(true))),
    })
}

/// Resolve the first argument of `next`/`nextN`/`matches`/`schedule` — either a
/// raw expression string or an already-parsed schedule Object — into a
/// [`CronSchedule`]. A bad expression STRING is Tier-1 (`Ok(Err(msg))`); a
/// wrong-typed first arg is Tier-2 (`Err(Control)`).
fn resolve_schedule(v: &Value, span: Span, ctx: &str) -> Result<Result<CronSchedule, String>, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(parse_expr(s.as_ref())),
        ValueKind::Object(o) => {
            let map = o.borrow();
            match schedule_from_object(&map) {
                Some(sched) => Ok(Ok(sched)),
                None => Err(AsError::at(
                    format!("{ctx}: object is not a cron schedule (missing __cron tag)"),
                    span,
                )
                .into()),
            }
        }
        _ => Err(AsError::at(
            format!(
                "{ctx}: expected a cron expression string or schedule, got {}",
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

/// Parse an expression string, applying `@`-macro expansion. `@reboot` is the one
/// macro that is NOT expanded here — the caller raises it as Tier-2.
fn parse_expr(expr: &str) -> Result<CronSchedule, String> {
    let trimmed = expr.trim();
    if let Some(expanded) = expand_macro(trimmed) {
        return CronSchedule::parse(expanded);
    }
    CronSchedule::parse(trimmed)
}

/// Read the optional `{tzOffset, after}` opts object. Returns `(after_ms_override,
/// tz_offset_min)`. Missing/`nil` opts → `(None, 0)`.
fn read_opts(v: &Value, span: Span, ctx: &str) -> Result<(Option<i64>, i64), Control> {
    match v.kind() {
        ValueKind::Nil => Ok((None, 0)),
        ValueKind::Object(o) => {
            let map = o.borrow();
            let after = match map.get("after") {
                Some(a) if !matches!(a.kind(), ValueKind::Nil) => {
                    Some(want_number(a, span, ctx)? as i64)
                }
                _ => None,
            };
            let tz = match map.get("tzOffset") {
                Some(t) if !matches!(t.kind(), ValueKind::Nil) => want_number(t, span, ctx)? as i64,
                _ => 0,
            };
            Ok((after, tz))
        }
        _ => Err(AsError::at(format!("{ctx}: opts must be an object"), span).into()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// schedule: a spawned loop driving `next → sleep → callback` (det-seam routed).
// ─────────────────────────────────────────────────────────────────────────────

use crate::interp::{Interp, ResourceState};
use crate::stdlib::task_mod::AbortOnDrop;
use crate::value::{NativeKind, NativeMethod};
use std::cell::Cell;
use std::rc::Rc;

/// The backing state of a `cron.schedule` handle ([`ResourceState::CronJob`]).
///
/// `running` is a shared flag the spawned loop reads before each fire: `stop()`
/// clears it (graceful — the loop exits before the next sleep). `_task` is the
/// `AbortOnDrop` guard for the loop: dropping the state (last-drop or `close()`)
/// aborts the loop, so there is never a zombie task (cancel-on-drop discipline).
pub(crate) struct CronJobState {
    pub(crate) running: Rc<Cell<bool>>,
    pub(crate) _task: AbortOnDrop,
}

impl Interp {
    /// `std/cron` dispatch. `parse`/`next`/`nextN`/`matches` are pure (Tier-1 on a
    /// bad expression); `schedule` spawns the firing loop and returns a handle.
    pub(crate) async fn call_cron(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "parse" => {
                let expr = want_string(&arg(args, 0), span, "cron.parse")?;
                self.cron_reject_reboot(&expr, span)?;
                match parse_expr(&expr) {
                    Ok(sched) => Ok(make_pair(schedule_value(&expr, &sched), Value::nil())),
                    Err(e) => Ok(make_pair(
                        Value::nil(),
                        make_error(Value::str(format!("invalid cron expression: {e}"))),
                    )),
                }
            }
            "next" => {
                // cron.next(expr|schedule, opts?)
                self.cron_reject_reboot_arg(&arg(args, 0), span)?;
                let sched = match resolve_schedule(&arg(args, 0), span, "cron.next")? {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(make_pair(
                            Value::nil(),
                            make_error(Value::str(format!("invalid cron expression: {e}"))),
                        ))
                    }
                };
                let (after_override, tz) = read_opts(&arg(args, 1), span, "cron.next")?;
                let after = after_override.unwrap_or_else(|| self.clock_now_ms() as i64);
                match next_after(&sched, after, tz) {
                    Some(n) => Ok(make_pair(Value::float(n as f64), Value::nil())),
                    None => Ok(make_pair(
                        Value::nil(),
                        make_error(Value::str(
                            "no cron match within 5 years (impossible schedule?)".to_string(),
                        )),
                    )),
                }
            }
            "nextN" => {
                // cron.nextN(expr|schedule, n, opts?)
                self.cron_reject_reboot_arg(&arg(args, 0), span)?;
                let sched = match resolve_schedule(&arg(args, 0), span, "cron.nextN")? {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(make_pair(
                            Value::nil(),
                            make_error(Value::str(format!("invalid cron expression: {e}"))),
                        ))
                    }
                };
                let n = want_number(&arg(args, 1), span, "cron.nextN")? as i64;
                if n < 0 {
                    return Err(AsError::at("cron.nextN: count must be >= 0", span).into());
                }
                let (after_override, tz) = read_opts(&arg(args, 2), span, "cron.nextN")?;
                let mut after = after_override.unwrap_or_else(|| self.clock_now_ms() as i64);
                let mut out: Vec<Value> = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    match next_after(&sched, after, tz) {
                        Some(next) => {
                            out.push(Value::float(next as f64));
                            after = next;
                        }
                        None => {
                            return Ok(make_pair(
                                Value::nil(),
                                make_error(Value::str(
                                    "no cron match within 5 years (impossible schedule?)"
                                        .to_string(),
                                )),
                            ))
                        }
                    }
                }
                Ok(make_pair(Value::array(out), Value::nil()))
            }
            "matches" => {
                // cron.matches(expr|schedule, epochMs, opts?)
                self.cron_reject_reboot_arg(&arg(args, 0), span)?;
                let sched = match resolve_schedule(&arg(args, 0), span, "cron.matches")? {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(make_pair(
                            Value::nil(),
                            make_error(Value::str(format!("invalid cron expression: {e}"))),
                        ))
                    }
                };
                let epoch = want_number(&arg(args, 1), span, "cron.matches")? as i64;
                let (_after, tz) = read_opts(&arg(args, 2), span, "cron.matches")?;
                let hit = matches_at(&sched, epoch, tz);
                Ok(make_pair(Value::bool_(hit), Value::nil()))
            }
            "schedule" => self.cron_schedule(args, span).await,
            _ => Err(AsError::at(format!("std/cron has no function '{func}'"), span).into()),
        }
    }

    /// `@reboot` is meaningless without a process to reboot → Tier-2 (programmer
    /// literal misuse, §11.2). Checked at the cron entry points that accept an
    /// expression STRING (a schedule Object can never carry `@reboot`).
    fn cron_reject_reboot(&self, expr: &str, span: Span) -> Result<(), Control> {
        if expr.trim() == "@reboot" {
            return Err(AsError::at(
                "cron: '@reboot' is not supported (it has no meaning without a process to reboot)",
                span,
            )
            .into());
        }
        Ok(())
    }

    fn cron_reject_reboot_arg(&self, v: &Value, span: Span) -> Result<(), Control> {
        if let ValueKind::Str(s) = v.kind() {
            self.cron_reject_reboot(s.as_ref(), span)?;
        }
        Ok(())
    }

    /// `cron.schedule(expr, fn, opts?)` — spawn a `next → sleep → callback` loop and
    /// return a `cronJob` handle. A bad expression is Tier-1 (`[nil, err]`); a
    /// non-function callback is Tier-2.
    async fn cron_schedule(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let expr = want_string(&arg(args, 0), span, "cron.schedule")?;
        self.cron_reject_reboot(&expr, span)?;
        let callback = arg(args, 1);
        if !matches!(
            callback.kind(),
            ValueKind::Function(_) | ValueKind::Builtin(_) | ValueKind::Closure(_)
        ) {
            return Err(AsError::at(
                format!(
                    "cron.schedule: callback must be a function, got {}",
                    crate::interp::type_name(&callback)
                ),
                span,
            )
            .into());
        }
        let sched = match parse_expr(&expr) {
            Ok(s) => s,
            Err(e) => {
                return Ok(make_pair(
                    Value::nil(),
                    make_error(Value::str(format!("invalid cron expression: {e}"))),
                ))
            }
        };
        let (_after, tz) = read_opts(&arg(args, 2), span, "cron.schedule")?;

        // The shared graceful-stop flag (cloned into the loop).
        let running = Rc::new(Cell::new(true));
        let running_loop = running.clone();
        // Recover the owning `Rc<Interp>` so the spawned task can `self.call_value`
        // and route sleep through `call_time` without crossing a `Send` boundary
        // (the exact `process.on` / `task.spawn` shape).
        let interp_rc = self.rc();
        let callback_loop = callback;
        let handle = tokio::task::spawn_local(async move {
            loop {
                if !running_loop.get() {
                    break;
                }
                // Compute the next fire time from the (possibly virtual) clock.
                let now = interp_rc.clock_now_ms() as i64;
                let next = match next_after(&sched, now, tz) {
                    Some(n) => n,
                    None => break, // impossible schedule: nothing more to fire.
                };
                let delay_ms = (next - now).max(0) as f64;
                // Sleep through the SAME `call_time("sleep", …)` path std/time uses
                // — so under a frozen/virtual clock the delay fast-forwards and fire
                // times are replay-deterministic BY CONSTRUCTION (§11.3).
                let _ = interp_rc
                    .call_time("sleep", &[Value::float(delay_ms)], span)
                    .await;
                if !running_loop.get() {
                    break;
                }
                // Fire the callback. A PANICKING callback must NOT kill the
                // scheduler (the server-handler rule): log + continue.
                match interp_rc
                    .call_value(callback_loop.clone(), Vec::new(), span)
                    .await
                {
                    Err(Control::Exit(code)) => {
                        interp_rc.flush_output();
                        std::process::exit(code);
                    }
                    Err(Control::Panic(e)) => {
                        eprintln!("error in cron schedule callback: {}", e.message);
                    }
                    Err(Control::Propagate(_)) | Ok(_) => {}
                }
            }
        });

        let state = CronJobState {
            running,
            _task: AbortOnDrop(handle.abort_handle()),
        };
        let mut fields: IndexMap<String, Value> = IndexMap::new();
        fields.insert("expr".to_string(), Value::str(expr.to_string()));
        let handle_value =
            self.register_resource(NativeKind::CronJob, fields, ResourceState::CronJob(state));
        Ok(make_pair(handle_value, Value::nil()))
    }

    /// Dispatch a `cronJob` handle method: `start`/`stop`/`running`/`close`.
    pub(crate) async fn call_cron_method(
        &self,
        m: &NativeMethod,
        _args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "stop" => {
                self.with_resource_mut(id, |r| {
                    if let Some(ResourceState::CronJob(s)) = r {
                        s.running.set(false);
                    }
                });
                Ok(Value::nil())
            }
            "start" => {
                // v1: the loop starts firing at schedule(); `start` re-arms a
                // gracefully-stopped flag for the still-live loop (no-op if the
                // loop already exited — documented limitation).
                self.with_resource_mut(id, |r| {
                    if let Some(ResourceState::CronJob(s)) = r {
                        s.running.set(true);
                    }
                });
                Ok(Value::nil())
            }
            "running" => {
                let running = self.with_resource_mut(id, |r| match r {
                    Some(ResourceState::CronJob(s)) => s.running.get(),
                    _ => false,
                });
                Ok(Value::bool_(running))
            }
            "close" => {
                // Take the resource out → drop its `AbortOnDrop` → abort the loop.
                if let Some(ResourceState::CronJob(s)) = self.take_resource(id) {
                    s.running.set(false);
                    drop(s); // explicit: aborts the spawned loop.
                }
                Ok(Value::nil())
            }
            other => Err(AsError::at(
                format!("cronJob handle has no method '{other}'"),
                span,
            )
            .into()),
        }
    }
}

/// Does `sched` fire at the given epoch-ms (with civil offset `tz_offset_min`)?
fn matches_at(sched: &CronSchedule, epoch_ms: i64, tz_offset_min: i64) -> bool {
    let off = Duration::minutes(tz_offset_min);
    match Utc.timestamp_millis_opt(epoch_ms).single() {
        Some(utc) => {
            let civil = utc + off;
            sched.matches_civil(
                civil.minute(),
                civil.hour(),
                civil.day(),
                civil.month(),
                civil.weekday().num_days_from_sunday(),
            )
        }
        None => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(expr: &str) -> CronSchedule {
        parse_expr(expr).unwrap_or_else(|e| panic!("parse '{expr}' failed: {e}"))
    }

    /// 2026-01-01T12:00:00Z = ms.
    fn ms(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s)
            .unwrap()
            .timestamp_millis()
    }

    fn next(expr: &str, after: &str) -> String {
        let s = sched(expr);
        let n = next_after(&s, ms(after), 0).expect("no next match");
        Utc.timestamp_millis_opt(n).unwrap().to_rfc3339()
    }

    // ── parse matrix ────────────────────────────────────────────────────────

    #[test]
    fn parse_star_every_field_unrestricted() {
        let s = sched("* * * * *");
        assert_eq!(s.minutes, (1u64 << 60) - 1); // all 60 minute bits
        assert!(!s.dom_restricted && !s.dow_restricted);
    }

    #[test]
    fn parse_lists_ranges_steps() {
        let s = sched("1,2,3 * * * *");
        assert_eq!(s.minutes & 0b1110, 0b1110);
        let r = sched("0 1-5 * * *");
        // hours 1..5
        for h in 1..=5 {
            assert!(r.hours & (1 << h) != 0, "hour {h} missing");
        }
        assert!(r.hours & (1 << 0) == 0);
        let step = sched("*/15 * * * *");
        for m in [0u32, 15, 30, 45] {
            assert!(step.minutes & (1 << m) != 0, "minute {m} missing");
        }
        assert!(step.minutes & (1 << 7) == 0);
        let rs = sched("10-30/5 * * * *");
        for m in [10u32, 15, 20, 25, 30] {
            assert!(rs.minutes & (1 << m) != 0, "minute {m} missing");
        }
        assert!(rs.minutes & (1 << 11) == 0);
    }

    #[test]
    fn parse_names_case_insensitive() {
        let s = sched("0 0 * jan,DEC *");
        assert!(s.months & (1 << 1) != 0 && s.months & (1 << 12) != 0);
        let d = sched("0 0 * * SUN,fri");
        assert!(d.dow & (1 << 0) != 0 && d.dow & (1 << 5) != 0);
    }

    #[test]
    fn parse_dow_seven_is_sunday() {
        let s = sched("0 0 * * 7");
        assert!(s.dow & (1 << 0) != 0, "dow 7 must fold onto bit 0 (Sunday)");
        assert!(s.dow_restricted);
    }

    #[test]
    fn parse_macros() {
        assert_eq!(sched("@daily"), sched("0 0 * * *"));
        assert_eq!(sched("@hourly"), sched("0 * * * *"));
        assert_eq!(sched("@weekly"), sched("0 0 * * 0"));
        assert_eq!(sched("@monthly"), sched("0 0 1 * *"));
        assert_eq!(sched("@yearly"), sched("0 0 1 1 *"));
    }

    // ── parse errors (Tier-1 from parse_expr) ───────────────────────────────

    #[test]
    fn parse_six_fields_errs() {
        assert!(parse_expr("0 0 0 0 0 0").is_err());
    }

    #[test]
    fn parse_out_of_range_errs() {
        assert!(parse_expr("99 * * * *").is_err());
        assert!(parse_expr("* 25 * * *").is_err());
        assert!(parse_expr("* * 32 * *").is_err());
        assert!(parse_expr("* * * 13 *").is_err());
    }

    #[test]
    fn parse_step_zero_errs() {
        assert!(parse_expr("*/0 * * * *").is_err());
    }

    #[test]
    fn parse_garbage_errs() {
        assert!(parse_expr("hello world").is_err());
        assert!(parse_expr("* * * foo *").is_err());
        assert!(parse_expr("").is_err());
    }

    // ── next known-vector table ─────────────────────────────────────────────

    #[test]
    fn next_daily_midnight_from_midday() {
        // 0 0 * * * from 2026-01-01T12:00 → next midnight 2026-01-02T00:00.
        assert_eq!(
            next("0 0 * * *", "2026-01-01T12:00:00+00:00"),
            "2026-01-02T00:00:00+00:00"
        );
    }

    #[test]
    fn next_minute_rollover() {
        // */15 from 12:07 → 12:15.
        assert_eq!(
            next("*/15 * * * *", "2026-01-01T12:07:00+00:00"),
            "2026-01-01T12:15:00+00:00"
        );
        // exactly on a boundary advances to the NEXT one (strictly after).
        assert_eq!(
            next("*/15 * * * *", "2026-01-01T12:15:00+00:00"),
            "2026-01-01T12:30:00+00:00"
        );
    }

    #[test]
    fn next_month_rollover() {
        // 0 0 1 * * (1st of month) from Jan 15 → Feb 1.
        assert_eq!(
            next("0 0 1 * *", "2026-01-15T00:00:00+00:00"),
            "2026-02-01T00:00:00+00:00"
        );
    }

    #[test]
    fn next_skips_short_months() {
        // 0 0 31 * * — the 31st. From Feb 1 2026 the next 31st is March 31
        // (Feb has no 31st; April/June/etc. handled across the year).
        assert_eq!(
            next("0 0 31 * *", "2026-02-01T00:00:00+00:00"),
            "2026-03-31T00:00:00+00:00"
        );
        // From March 31 (strictly after) → May 31 (April has only 30 days).
        assert_eq!(
            next("0 0 31 * *", "2026-03-31T00:00:00+00:00"),
            "2026-05-31T00:00:00+00:00"
        );
    }

    #[test]
    fn next_leap_day() {
        // 0 0 29 2 * — Feb 29. 2026/2027 are non-leap; next is 2028-02-29.
        assert_eq!(
            next("0 0 29 2 *", "2026-03-01T00:00:00+00:00"),
            "2028-02-29T00:00:00+00:00"
        );
    }

    #[test]
    fn next_year_rollover() {
        // 59 23 31 12 * — 23:59 on Dec 31. From mid-2026 → 2026-12-31T23:59.
        assert_eq!(
            next("59 23 31 12 *", "2026-06-15T00:00:00+00:00"),
            "2026-12-31T23:59:00+00:00"
        );
    }

    // ── the DOM/DOW OR rule — both directions ───────────────────────────────

    #[test]
    fn dom_dow_or_rule_fires_on_dom() {
        // 0 0 13 * 5 — midnight on the 13th OR on any Friday.
        // 2026-02-13 is a Friday, so it'd hit either way; pick a 13th that is
        // NOT a Friday to prove the DOM half. 2026-01-13 is a Tuesday.
        let s = sched("0 0 13 * 5");
        assert!(
            s.matches_civil(0, 0, 13, 1, 2),
            "the 13th (a Tuesday) must match via the DOM half of the OR rule"
        );
    }

    #[test]
    fn dom_dow_or_rule_fires_on_dow() {
        // Same schedule: any Friday must match via the DOW half, even when the
        // day-of-month is not 13. 2026-01-02 is a Friday (day 2).
        let s = sched("0 0 13 * 5");
        assert!(
            s.matches_civil(0, 0, 2, 1, 5),
            "a Friday (not the 13th) must match via the DOW half of the OR rule"
        );
        // A Tuesday that is not the 13th must NOT match.
        assert!(
            !s.matches_civil(0, 0, 6, 1, 2),
            "a non-13th non-Friday must not match"
        );
    }

    #[test]
    fn dom_only_restricted_ignores_dow() {
        // 0 0 15 * * — only the 15th, any weekday.
        let s = sched("0 0 15 * *");
        assert!(s.dom_restricted && !s.dow_restricted);
        assert!(s.matches_civil(0, 0, 15, 1, 3)); // 15th, a Wednesday → match
        assert!(!s.matches_civil(0, 0, 16, 1, 3)); // 16th → no
    }

    #[test]
    fn next_with_or_rule_picks_earliest() {
        // 0 0 13 * 5 from 2026-01-01 (Thu). The earliest hit is the first Friday
        // 2026-01-02 (DOW half), BEFORE the 13th.
        assert_eq!(
            next("0 0 13 * 5", "2026-01-01T00:00:00+00:00"),
            "2026-01-02T00:00:00+00:00"
        );
    }

    // ── tzOffset shift ──────────────────────────────────────────────────────

    #[test]
    fn next_tz_offset_shifts_civil() {
        // 0 0 * * * at tzOffset -300 (UTC-5). Civil midnight = 05:00 UTC.
        // From 2026-01-01T12:00Z, civil local time is 07:00 (Jan 1), so the next
        // civil midnight is Jan 2 00:00 local = Jan 2 05:00 UTC.
        let s = sched("0 0 * * *");
        let n = next_after(&s, ms("2026-01-01T12:00:00+00:00"), -300).unwrap();
        assert_eq!(
            Utc.timestamp_millis_opt(n).unwrap().to_rfc3339(),
            "2026-01-02T05:00:00+00:00"
        );
    }

    // ── no match within 5 years ─────────────────────────────────────────────

    #[test]
    fn next_impossible_date_no_match() {
        // 0 0 30 2 * — Feb 30 never exists → no match within 5 years.
        let s = sched("0 0 30 2 *");
        assert!(next_after(&s, ms("2026-01-01T00:00:00+00:00"), 0).is_none());
    }

    // ── matches ─────────────────────────────────────────────────────────────

    #[test]
    fn matches_known_time() {
        let s = sched("0 0 * * *");
        // 2026-01-02T00:00:00Z is a midnight → matches.
        let t = ms("2026-01-02T00:00:00+00:00");
        let dt = Utc.timestamp_millis_opt(t).unwrap();
        assert!(s.matches_civil(
            dt.minute(),
            dt.hour(),
            dt.day(),
            dt.month(),
            dt.weekday().num_days_from_sunday()
        ));
        // 00:01 does not match.
        let t2 = ms("2026-01-02T00:01:00+00:00");
        let dt2 = Utc.timestamp_millis_opt(t2).unwrap();
        assert!(!s.matches_civil(
            dt2.minute(),
            dt2.hour(),
            dt2.day(),
            dt2.month(),
            dt2.weekday().num_days_from_sunday()
        ));
    }

    // ── nextN ───────────────────────────────────────────────────────────────

    #[test]
    fn next_n_sequence() {
        let s = sched("0 0 * * *");
        let mut after = ms("2026-01-01T12:00:00+00:00");
        let mut got = Vec::new();
        for _ in 0..3 {
            let n = next_after(&s, after, 0).unwrap();
            got.push(Utc.timestamp_millis_opt(n).unwrap().to_rfc3339());
            after = n;
        }
        assert_eq!(
            got,
            vec![
                "2026-01-02T00:00:00+00:00",
                "2026-01-03T00:00:00+00:00",
                "2026-01-04T00:00:00+00:00",
            ]
        );
    }

    // ── call-level: Tier-1 parse / Tier-2 @reboot ───────────────────────────

    #[tokio::test]
    async fn call_parse_tier1_on_bad_expr() {
        use crate::interp::Interp;
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // A garbage expression → Tier-1 [nil, {message}] (Ok, not a panic).
                let r = interp
                    .call_cron("parse", &[Value::str("not a cron")], Span::new(0, 0))
                    .await
                    .expect("parse must be Tier-1, not a panic");
                let txt = r.to_string();
                assert!(txt.starts_with("[nil, {message:"), "expected Tier-1 err, got {txt}");
            })
            .await;
    }

    #[tokio::test]
    async fn call_reboot_macro_is_tier2() {
        use crate::interp::{Control, Interp};
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // @reboot is a programmer-literal misuse → Tier-2 panic (§11.2).
                let r = interp
                    .call_cron("parse", &[Value::str("@reboot")], Span::new(0, 0))
                    .await;
                assert!(
                    matches!(r, Err(Control::Panic(_))),
                    "@reboot must be a Tier-2 panic, got {r:?}"
                );
            })
            .await;
    }

    #[tokio::test]
    async fn call_next_no_match_is_tier1() {
        use crate::interp::Interp;
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // 0 0 30 2 * never matches → Tier-1 err from next.
                let after = ms("2026-01-01T00:00:00+00:00");
                let opts = {
                    let mut o: IndexMap<String, Value> = IndexMap::new();
                    o.insert("after".to_string(), Value::float(after as f64));
                    Value::object(o)
                };
                let r = interp
                    .call_cron(
                        "next",
                        &[Value::str("0 0 30 2 *"), opts],
                        Span::new(0, 0),
                    )
                    .await
                    .expect("next must be Tier-1, not a panic");
                let txt = r.to_string();
                assert!(
                    txt.starts_with("[nil, {message:"),
                    "expected Tier-1 no-match err, got {txt}"
                );
            })
            .await;
    }

    // ── schedule under a frozen clock (the C1 det seam) ─────────────────────

    /// Install a FROZEN determinism clock, schedule `*/1 * * * *` (every minute),
    /// and assert the fire count advances with VIRTUAL time — no real wall-clock
    /// delay — and that `stop()` halts the loop. The schedule loop sleeps through
    /// `call_time("sleep", …)`, which under a frozen clock advances the virtual
    /// clock and returns instantly, so each fire is deterministic by construction.
    #[tokio::test]
    async fn schedule_under_frozen_clock_advances_and_stops() {
        use crate::interp::{global_env, Interp};
        use crate::lexer::lex;
        use crate::parser::parse;

        // The callback increments `count`; once it reaches 3 it stops the job (so
        // the otherwise-infinite per-minute loop terminates). `job` is a forward-
        // referenced module global the schedule pair binds before the loop fires.
        let src = r#"
import * as cron from "std/cron"
let count = 0
fn tick() {
    count = count + 1
    if (count >= 3) {
        job.stop()
    }
}
let [job, err] = cron.schedule("*/1 * * * *", tick)
"#;
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        // Freeze the clock at a fixed civil epoch (deterministic, no real time).
        interp.install_determinism(crate::det::DeterminismContext::record(
            7,
            ms("2026-01-01T00:00:00+00:00") as f64,
        ));
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                interp
                    .exec_program(&stmts, &env)
                    .await
                    .expect("program panicked");
            })
            .await;
        // Drain the spawned schedule loop (it self-stops after 3 fires). A bound on
        // real time guards against a hang if the stop logic regresses.
        let drained =
            tokio::time::timeout(std::time::Duration::from_secs(5), local).await;
        assert!(drained.is_ok(), "schedule loop did not terminate (stop() regressed?)");

        // The fire count reached exactly 3 (virtual time advanced it; stop() halted).
        let count = env
            .get("count")
            .and_then(|v| v.as_f64())
            .expect("count global");
        assert_eq!(count, 3.0, "schedule must fire exactly 3 times then stop");
    }
}
