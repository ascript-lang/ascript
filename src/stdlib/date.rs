//! `std/date` — civil dates over UTC epoch-millis, backed by chrono. An
//! "instant" is a plain object snapshot; `epochMs` is canonical for arithmetic.
//!
//! Timezones are OFFSET-based: `format(instant, fmt, tzOffsetMinutes?)` shifts
//! the displayed wall-clock by a fixed offset (default UTC). Named IANA zones
//! (e.g. `America/New_York`) are a documented deferral (would need `chrono-tz`).

use super::{arg, bi, want_number, want_object, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use chrono::{Datelike, TimeZone, Timelike, Utc};
use indexmap::IndexMap;

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
///
/// Callers validate `epoch_ms` is in chrono's representable range: `fromEpochMs`
/// guards user input directly, while `now`/`parse`/`add*` only feed epochs that
/// are already in range. The `unwrap_or_else(1970)` below is a defensive fallback
/// that should never trigger in practice.
fn make_instant(epoch_ms: i64) -> Value {
    let dt = Utc
        .timestamp_millis_opt(epoch_ms)
        .single()
        .unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap());
    let mut o: IndexMap<String, Value> = IndexMap::new();
    o.insert("epochMs".into(), Value::Float(epoch_ms as f64));
    o.insert("year".into(), Value::Float(dt.year() as f64));
    o.insert("month".into(), Value::Float(dt.month() as f64));
    o.insert("day".into(), Value::Float(dt.day() as f64));
    o.insert("hour".into(), Value::Float(dt.hour() as f64));
    o.insert("minute".into(), Value::Float(dt.minute() as f64));
    o.insert("second".into(), Value::Float(dt.second() as f64));
    o.insert(
        "millisecond".into(),
        Value::Float(dt.timestamp_subsec_millis() as f64),
    );
    o.insert(
        "weekday".into(),
        Value::Float(dt.weekday().num_days_from_sunday() as f64),
    );
    o.insert("iso".into(), Value::Str(dt.to_rfc3339().into()));
    Value::Object(crate::value::ObjectCell::new(o))
}

/// Read the canonical `epochMs` field from an instant object (Tier-2 panic if absent).
fn instant_epoch(v: &Value, span: Span, ctx: &str) -> Result<i64, Control> {
    let o = want_object(v, span, ctx)?;
    match o.get("epochMs").and_then(|v| v.as_f64()) {
        Some(n) => Ok(n as i64),
        _ => Err(AsError::at(
            format!(
                "{} expects an instant object (with epochMs), got an object without it",
                ctx
            ),
            span,
        )
        .into()),
    }
}

/// Build the `date.now` instant from an explicit ms-epoch — the SP9 §3 determinism
/// seam for `date.now` (the dispatcher passes the virtual/recorded clock value).
/// Identical to `date.now`'s default `make_instant(Utc::now().timestamp_millis())`
/// apart from the time source.
pub fn now_from_ms(epoch_ms: f64) -> Value {
    make_instant(epoch_ms as i64)
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("date.{}", f);
    match func {
        "now" => Ok(make_instant(Utc::now().timestamp_millis())),
        "fromEpochMs" => {
            let ms = want_number(&arg(args, 0), span, &ctx("fromEpochMs"))?;
            // Reject epochs outside chrono's representable range instead of
            // silently clamping to 1970 inside make_instant (Tier-2: bad input).
            if Utc.timestamp_millis_opt(ms as i64).single().is_none() {
                return Err(AsError::at(
                    "date.fromEpochMs: epoch milliseconds out of representable range",
                    span,
                )
                .into());
            }
            Ok(make_instant(ms as i64))
        }
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            let parsed = match args.get(1) {
                None | Some(Value::Nil) => {
                    // default: RFC3339 / ISO8601
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|dt| dt.with_timezone(&Utc))
                        .or_else(|_| {
                            chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S")
                                .map(|ndt| Utc.from_utc_datetime(&ndt))
                        })
                        .map_err(|e| e.to_string())
                }
                Some(fmt_v) => {
                    let fmt = want_string(fmt_v, span, &ctx("parse"))?;
                    chrono::NaiveDateTime::parse_from_str(&s, &fmt)
                        .map(|ndt| Utc.from_utc_datetime(&ndt))
                        .map_err(|e| e.to_string())
                }
            };
            match parsed {
                Ok(dt) => Ok(make_pair(make_instant(dt.timestamp_millis()), Value::Nil)),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("cannot parse date: {}", e).into())),
                )),
            }
        }
        "format" => {
            let epoch = instant_epoch(&arg(args, 0), span, &ctx("format"))?;
            let fmt = want_string(&arg(args, 1), span, &ctx("format"))?;
            let offset_min = match args.get(2) {
                None | Some(Value::Nil) => 0i64,
                Some(v) => want_number(v, span, &ctx("format"))? as i64,
            };
            let dt = Utc
                .timestamp_millis_opt(epoch)
                .single()
                .unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap());
            let shifted = dt + chrono::Duration::minutes(offset_min);
            // format with the shifted naive time (offset applied for display)
            Ok(Value::Str(
                shifted.naive_utc().format(&fmt).to_string().into(),
            ))
        }
        "addDays" | "addHours" | "addMinutes" | "addSeconds" | "addMonths" | "addYears" => {
            let epoch = instant_epoch(&arg(args, 0), span, &ctx(func))?;
            let n = want_number(&arg(args, 1), span, &ctx(func))? as i64;
            let dt = Utc
                .timestamp_millis_opt(epoch)
                .single()
                .unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap());
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
            Ok(Value::Float((a - b) as f64))
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
    Utc.with_ymd_and_hms(year, month0 + 1, day, dt.hour(), dt.minute(), dt.second())
        .single()
        .map(|d| d + chrono::Duration::milliseconds(dt.timestamp_subsec_millis() as i64))
        // Only triggers on year-overflow past chrono's ~±262143-year range; the
        // day is already clamped to the month, so the constructed date is valid.
        .unwrap_or(dt)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }

    #[test]
    fn from_epoch_components_and_iso() {
        // 2021-01-01T00:00:00Z = 1609459200000 ms
        let inst = call("fromEpochMs", &[Value::Float(1609459200000.0)], sp()).unwrap();
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
        } else {
            panic!("expected pair");
        }
    }

    #[test]
    fn parse_invalid_is_tier1_err() {
        assert!(call("parse", &[s("not a date")], sp())
            .unwrap()
            .to_string()
            .starts_with("[nil, {message:"));
    }

    #[test]
    fn arithmetic_and_diff() {
        let base = call("fromEpochMs", &[Value::Float(1609459200000.0)], sp()).unwrap();
        let plus1 = call("addDays", &[base.clone(), Value::Float(1.0)], sp()).unwrap();
        let d = call("diffMs", &[plus1, base], sp()).unwrap();
        assert_eq!(d, Value::Float(86_400_000.0)); // one day in ms
    }

    #[test]
    fn add_months_clamps_day() {
        // Jan 31 + 1 month → Feb 28 (2021 non-leap)
        let jan31 = call("parse", &[s("2021-01-31T00:00:00Z")], sp()).unwrap();
        if let Value::Array(a) = &jan31 {
            let inst = a.borrow()[0].clone();
            let feb = call("addMonths", &[inst, Value::Float(1.0)], sp()).unwrap();
            assert!(feb.to_string().contains("month: 2"));
            assert!(feb.to_string().contains("day: 28"));
        } else {
            panic!();
        }
    }

    #[test]
    fn tz_offset_shifts_display() {
        // 2021-01-01T00:00:00Z formatted at +120 min → 02:00; at -300 → prev day 19:00
        let inst = call("fromEpochMs", &[Value::Float(1609459200000.0)], sp()).unwrap();
        assert_eq!(
            call(
                "format",
                &[inst.clone(), s("%Y-%m-%d %H:%M"), Value::Float(120.0)],
                sp()
            )
            .unwrap(),
            s("2021-01-01 02:00")
        );
        assert_eq!(
            call(
                "format",
                &[inst, s("%Y-%m-%d %H:%M"), Value::Float(-300.0)],
                sp()
            )
            .unwrap(),
            s("2020-12-31 19:00")
        );
    }

    #[test]
    fn month_arithmetic_edges() {
        // Jan 31 2020 (leap) + 1 month → Feb 29; negative; cross-year
        let leap = call("parse", &[s("2020-01-31T00:00:00Z")], sp()).unwrap();
        if let Value::Array(a) = &leap {
            let inst = a.borrow()[0].clone();
            let feb = call("addMonths", &[inst.clone(), Value::Float(1.0)], sp()).unwrap();
            assert!(feb.to_string().contains("month: 2") && feb.to_string().contains("day: 29"));
            let dec = call("addMonths", &[inst, Value::Float(-1.0)], sp()).unwrap(); // Jan 31 2020 - 1mo → Dec 31 2019
            assert!(
                dec.to_string().contains("year: 2019")
                    && dec.to_string().contains("month: 12")
                    && dec.to_string().contains("day: 31")
            );
        } else {
            panic!();
        }
    }

    #[test]
    fn from_epoch_out_of_range_panics() {
        assert!(matches!(
            call("fromEpochMs", &[Value::Float(9.0e18)], sp()),
            Err(Control::Panic(_))
        ));
    }
}
