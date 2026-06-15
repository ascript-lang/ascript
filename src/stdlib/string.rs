//! `std/string` — string manipulation.

use super::{arg, bi, clamp_index, want_array, want_count, want_number, want_string, MAX_ALLOC_COUNT};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::{Value, ValueKind};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("split", bi("string.split")),
        ("join", bi("string.join")),
        ("slice", bi("string.slice")),
        ("trim", bi("string.trim")),
        ("upper", bi("string.upper")),
        ("lower", bi("string.lower")),
        ("find", bi("string.find")),
        ("replace", bi("string.replace")),
        ("replaceAll", bi("string.replaceAll")),
        ("format", bi("string.format")),
        ("padStart", bi("string.padStart")),
        ("padEnd", bi("string.padEnd")),
        ("repeat", bi("string.repeat")),
        ("startsWith", bi("string.startsWith")),
        ("endsWith", bi("string.endsWith")),
        ("contains", bi("string.contains")),
        ("chars", bi("string.chars")),
        ("lines", bi("string.lines")),
        ("reverse", bi("string.reverse")),
        ("count", bi("string.count")),
        ("splitN", bi("string.splitN")),
        ("codepoints", bi("string.codepoints")),
        ("from_codepoints", bi("string.from_codepoints")),
        ("code_at", bi("string.code_at")),
    ]
}

fn str_val(s: String) -> Value {
    Value::str(s)
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("string.{}", f);
    match func {
        "split" => {
            let s = want_string(&arg(args, 0), span, &ctx("split"))?;
            let sep = want_string(&arg(args, 1), span, &ctx("split"))?;
            let parts: Vec<Value> = if sep.is_empty() {
                s.chars().map(|c| str_val(c.to_string())).collect()
            } else {
                s.split(sep.as_ref())
                    .map(|p| str_val(p.to_string()))
                    .collect()
            };
            Ok(Value::array_cell(crate::value::ArrayCell::new(parts)))
        }
        "join" => {
            let arr = want_array(&arg(args, 0), span, &ctx("join"))?;
            let sep = want_string(&arg(args, 1), span, &ctx("join"))?;
            let pieces: Vec<String> = arr.borrow().iter().map(|v| v.to_string()).collect();
            Ok(str_val(pieces.join(sep.as_ref())))
        }
        "slice" => {
            let s = want_string(&arg(args, 0), span, &ctx("slice"))?;
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len();
            let start = clamp_index(want_number(&arg(args, 1), span, &ctx("slice"))?, len);
            let end = match args.get(2) {
                None => len,
                Some(v) if matches!(v.kind(), ValueKind::Nil) => len,
                Some(v) => clamp_index(want_number(v, span, &ctx("slice"))?, len),
            };
            let slice: String = if start < end {
                chars[start..end].iter().collect()
            } else {
                String::new()
            };
            Ok(str_val(slice))
        }
        "trim" => Ok(str_val(
            want_string(&arg(args, 0), span, &ctx("trim"))?
                .trim()
                .to_string(),
        )),
        "upper" => Ok(str_val(
            want_string(&arg(args, 0), span, &ctx("upper"))?.to_uppercase(),
        )),
        "lower" => Ok(str_val(
            want_string(&arg(args, 0), span, &ctx("lower"))?.to_lowercase(),
        )),
        "find" => {
            let s = want_string(&arg(args, 0), span, &ctx("find"))?;
            let sub = want_string(&arg(args, 1), span, &ctx("find"))?;
            // NUM §4: a character index/count is an `Int`.
            match s.find(sub.as_ref()) {
                Some(byte_idx) => Ok(Value::int(s[..byte_idx].chars().count() as i64)),
                None => Ok(Value::int(-1)),
            }
        }
        "replace" => {
            let s = want_string(&arg(args, 0), span, &ctx("replace"))?;
            let from = want_string(&arg(args, 1), span, &ctx("replace"))?;
            let to = want_string(&arg(args, 2), span, &ctx("replace"))?;
            let result = if from.is_empty() {
                s.to_string()
            } else {
                s.replacen(from.as_ref(), to.as_ref(), 1)
            };
            Ok(str_val(result))
        }
        "replaceAll" => {
            let s = want_string(&arg(args, 0), span, &ctx("replaceAll"))?;
            let from = want_string(&arg(args, 1), span, &ctx("replaceAll"))?;
            let to = want_string(&arg(args, 2), span, &ctx("replaceAll"))?;
            let result = if from.is_empty() {
                s.to_string()
            } else {
                s.replace(from.as_ref(), to.as_ref())
            };
            Ok(str_val(result))
        }
        "format" => {
            let template = want_string(&arg(args, 0), span, &ctx("format"))?;
            Ok(str_val(format_template(
                &template,
                args.get(1..).unwrap_or(&[]),
                span,
            )?))
        }
        "padStart" | "padEnd" => {
            let s = want_string(&arg(args, 0), span, &ctx(func))?;
            // Guard the target width before it drives a `take(need)` allocation: a
            // non-finite / out-of-range width would cast to `usize::MAX` and OOM-abort.
            let width = want_count(&arg(args, 1), span, &ctx(func), MAX_ALLOC_COUNT)?;
            let fill = match args.get(2) {
                None => " ".to_string(),
                Some(v) if matches!(v.kind(), ValueKind::Nil) => " ".to_string(),
                Some(v) => want_string(v, span, &ctx(func))?.to_string(),
            };
            let cur = s.chars().count();
            if cur >= width || fill.is_empty() {
                return Ok(str_val(s.to_string()));
            }
            let need = width - cur;
            let pad: String = fill.chars().cycle().take(need).collect();
            let result = if func == "padStart" {
                format!("{}{}", pad, s)
            } else {
                format!("{}{}", s, pad)
            };
            Ok(str_val(result))
        }
        "repeat" => {
            let s = want_string(&arg(args, 0), span, &ctx("repeat"))?;
            // Guard the count before the `f64 → usize` cast: `repeat("x", 1/0)` would
            // otherwise cast `Inf` to `usize::MAX` and abort the host with a
            // `capacity overflow`. A finite, in-range, non-negative count truncates
            // toward zero (NaN is already rejected).
            let n = want_count(&arg(args, 1), span, &ctx("repeat"), MAX_ALLOC_COUNT)?;
            // Even an in-range count can overflow `s.len() * n`; reject a product that
            // would exceed the generic allocation bound so `String::repeat` cannot
            // panic on capacity overflow.
            if s.len().checked_mul(n).is_none_or(|bytes| bytes as f64 > MAX_ALLOC_COUNT) {
                return Err(AsError::at(
                    format!(
                        "string.repeat: resulting string of {} × {} bytes exceeds the maximum size",
                        s.len(),
                        n
                    ),
                    span,
                )
                .into());
            }
            Ok(str_val(s.repeat(n)))
        }
        "startsWith" => {
            let s = want_string(&arg(args, 0), span, &ctx("startsWith"))?;
            let p = want_string(&arg(args, 1), span, &ctx("startsWith"))?;
            Ok(Value::bool_(s.starts_with(p.as_ref())))
        }
        "endsWith" => {
            let s = want_string(&arg(args, 0), span, &ctx("endsWith"))?;
            let p = want_string(&arg(args, 1), span, &ctx("endsWith"))?;
            Ok(Value::bool_(s.ends_with(p.as_ref())))
        }
        "contains" => {
            let s = want_string(&arg(args, 0), span, &ctx("contains"))?;
            let sub = want_string(&arg(args, 1), span, &ctx("contains"))?;
            Ok(Value::bool_(s.contains(sub.as_ref())))
        }
        "chars" => {
            let s = want_string(&arg(args, 0), span, &ctx("chars"))?;
            let out: Vec<Value> = s
                .chars()
                .map(|c| Value::str(c.to_string()))
                .collect();
            Ok(Value::array_cell(crate::value::ArrayCell::new(out)))
        }
        "lines" => {
            let s = want_string(&arg(args, 0), span, &ctx("lines"))?;
            let out: Vec<Value> = s.lines().map(Value::str).collect();
            Ok(Value::array_cell(crate::value::ArrayCell::new(out)))
        }
        "reverse" => {
            let s = want_string(&arg(args, 0), span, &ctx("reverse"))?;
            Ok(Value::str(s.chars().rev().collect::<String>()))
        }
        "count" => {
            let s = want_string(&arg(args, 0), span, &ctx("count"))?;
            let sub = want_string(&arg(args, 1), span, &ctx("count"))?;
            let n = if sub.is_empty() {
                0
            } else {
                s.matches(sub.as_ref()).count()
            };
            // NUM §4: an occurrence count is an `Int`.
            Ok(Value::int(n as i64))
        }
        "splitN" => {
            let s = want_string(&arg(args, 0), span, &ctx("splitN"))?;
            let sep = want_string(&arg(args, 1), span, &ctx("splitN"))?;
            let n_raw = want_number(&arg(args, 2), span, &ctx("splitN"))?;
            if n_raw < 1.0 {
                return Err(AsError::at("string.splitN requires n >= 1", span).into());
            }
            let n = n_raw as usize;
            let out: Vec<Value> = s
                .splitn(n, sep.as_ref())
                .map(Value::str)
                .collect();
            Ok(Value::array_cell(crate::value::ArrayCell::new(out)))
        }
        "codepoints" => {
            // NUM §1/§4: Unicode scalar values are `int`s (the Go rune model).
            let s = want_string(&arg(args, 0), span, &ctx("codepoints"))?;
            let out: Vec<Value> = s.chars().map(|c| Value::int(c as i64)).collect();
            Ok(Value::array_cell(crate::value::ArrayCell::new(out)))
        }
        "from_codepoints" => {
            // Validate each element is a valid Unicode scalar (0..=0x10FFFF, excluding
            // the surrogate range D800..=DFFF). An invalid scalar is a Tier-2 panic.
            let arr = want_array(&arg(args, 0), span, &ctx("from_codepoints"))?;
            let items = arr.borrow();
            let mut out = String::with_capacity(items.len());
            for (i, v) in items.iter().enumerate() {
                let cp = match v.as_int_exact() {
                    Some(cp) => cp,
                    None => {
                        return Err(AsError::at(
                            format!(
                                "string.from_codepoints: element {} must be an int code point, got {}",
                                i,
                                crate::interp::type_name(v)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                let scalar = u32::try_from(cp)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| {
                        Control::from(AsError::at(
                            format!(
                                "string.from_codepoints: {} is not a valid Unicode scalar value",
                                cp
                            ),
                            span,
                        ))
                    })?;
                out.push(scalar);
            }
            Ok(str_val(out))
        }
        "code_at" => {
            // The Unicode scalar value at char index `i` (an `int`). An out-of-range
            // index is a Tier-2 panic.
            let s = want_string(&arg(args, 0), span, &ctx("code_at"))?;
            let idx_val = arg(args, 1);
            let idx = match idx_val.as_int_exact() {
                Some(i) if i >= 0 => i as usize,
                Some(_) => {
                    return Err(AsError::at(
                        "string.code_at: index must be a non-negative integer",
                        span,
                    )
                    .into())
                }
                None => {
                    return Err(AsError::at(
                        format!(
                            "string.code_at: index must be an int, got {}",
                            crate::interp::type_name(&idx_val)
                        ),
                        span,
                    )
                    .into())
                }
            };
            match s.chars().nth(idx) {
                Some(c) => Ok(Value::int(c as i64)),
                None => Err(AsError::at(
                    format!(
                        "string.code_at: index {} out of range (length {})",
                        idx,
                        s.chars().count()
                    ),
                    span,
                )
                .into()),
            }
        }
        _ => Err(AsError::at(format!("std/string has no function '{}'", func), span).into()),
    }
}

/// `format("Hello {}, you are {}", name, age)`. `{}` consumes the next argument
/// in order; `{{` and `}}` are literal braces. Too few args → panic.
/// A lone `{` (not followed by `{` or `}`) and a lone `}` fall through to literal
/// passthrough — deliberate lenient behavior.
fn format_template(template: &str, args: &[Value], span: Span) -> Result<String, Control> {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    let mut next = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' if chars.peek() == Some(&'}') => {
                chars.next();
                match args.get(next) {
                    Some(v) => out.push_str(&v.to_string()),
                    None => {
                        return Err(AsError::at(
                            "string.format: not enough arguments for placeholders",
                            span,
                        )
                        .into())
                    }
                }
                next += 1;
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(x: &str) -> Value {
        Value::str(x)
    }
    fn sp() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn splits_and_joins() {
        let parts = call("split", &[s("a,b,c"), s(",")], sp()).unwrap();
        assert_eq!(parts.to_string(), "[\"a\", \"b\", \"c\"]");
        let joined = call("join", &[parts, s("-")], sp()).unwrap();
        assert_eq!(joined, s("a-b-c"));
    }

    #[test]
    fn slice_trim_case() {
        assert_eq!(
            call(
                "slice",
                &[s("hello"), Value::float(1.0), Value::float(4.0)],
                sp()
            )
            .unwrap(),
            s("ell")
        );
        assert_eq!(
            call("slice", &[s("hello"), Value::float(-2.0)], sp()).unwrap(),
            s("lo")
        );
        assert_eq!(call("trim", &[s("  hi  ")], sp()).unwrap(), s("hi"));
        assert_eq!(call("upper", &[s("aB")], sp()).unwrap(), s("AB"));
        assert_eq!(call("lower", &[s("aB")], sp()).unwrap(), s("ab"));
    }

    #[test]
    fn find_replace_format_pad_repeat() {
        assert_eq!(
            call("find", &[s("hello"), s("ll")], sp()).unwrap(),
            Value::int(2)
        );
        assert_eq!(
            call("find", &[s("hello"), s("z")], sp()).unwrap(),
            Value::int(-1)
        );
        // replace = FIRST occurrence only
        assert_eq!(
            call("replace", &[s("a.b.c"), s("."), s("-")], sp()).unwrap(),
            s("a-b.c")
        );
        // replaceAll = all occurrences
        assert_eq!(
            call("replaceAll", &[s("a.b.c"), s("."), s("-")], sp()).unwrap(),
            s("a-b-c")
        );
        assert_eq!(
            call(
                "format",
                &[
                    s("{} + {} = {}"),
                    Value::float(1.0),
                    Value::float(2.0),
                    Value::float(3.0)
                ],
                sp()
            )
            .unwrap(),
            // NUM §4: the float args format with a decimal.
            s("1.0 + 2.0 = 3.0")
        );
        assert_eq!(
            call("format", &[s("{{literal}}")], sp()).unwrap(),
            s("{literal}")
        );
        assert_eq!(
            call("padStart", &[s("7"), Value::float(3.0), s("0")], sp()).unwrap(),
            s("007")
        );
        assert_eq!(
            call("padEnd", &[s("7"), Value::float(3.0)], sp()).unwrap(),
            s("7  ")
        );
        assert_eq!(
            call("repeat", &[s("ab"), Value::float(3.0)], sp()).unwrap(),
            s("ababab")
        );
    }

    #[test]
    fn edge_branches() {
        let sp = sp();
        // empty separator splits into chars
        assert_eq!(
            call("split", &[s("abc"), s("")], sp).unwrap().to_string(),
            "[\"a\", \"b\", \"c\"]"
        );
        // padStart when already wide enough returns unchanged
        assert_eq!(
            call("padStart", &[s("hello"), Value::float(3.0), s("0")], sp).unwrap(),
            s("hello")
        );
        // slice start >= end → empty
        assert_eq!(
            call(
                "slice",
                &[s("hello"), Value::float(4.0), Value::float(2.0)],
                sp
            )
            .unwrap(),
            s("")
        );
        // empty `from` leaves input unchanged for both
        assert_eq!(
            call("replace", &[s("abc"), s(""), s("X")], sp).unwrap(),
            s("abc")
        );
        assert_eq!(
            call("replaceAll", &[s("abc"), s(""), s("X")], sp).unwrap(),
            s("abc")
        );
        // negative repeat count → panic
        assert!(matches!(
            call("repeat", &[s("a"), Value::float(-1.0)], sp),
            Err(Control::Panic(_))
        ));
        // standalone }} escape
        assert_eq!(call("format", &[s("a}}b")], sp).unwrap(), s("a}b"));
    }

    #[test]
    fn misuse_panics() {
        assert!(matches!(
            call("split", &[Value::float(1.0), s(",")], sp()),
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            call("format", &[s("{}")], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn string_completeness() {
        assert_eq!(
            call("startsWith", &[s("hello"), s("he")], sp()).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            call("startsWith", &[s("hello"), s("xy")], sp()).unwrap(),
            Value::bool_(false)
        );
        assert_eq!(
            call("endsWith", &[s("hello"), s("lo")], sp()).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            call("contains", &[s("hello"), s("ell")], sp()).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            call("contains", &[s("hello"), s("")], sp()).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            call("contains", &[s("hello"), s("zzz")], sp()).unwrap(),
            Value::bool_(false)
        );
        assert_eq!(
            call("chars", &[s("ab")], sp()).unwrap().to_string(),
            "[\"a\", \"b\"]"
        );
        assert_eq!(
            call("lines", &[s("a\nb\n")], sp()).unwrap().to_string(),
            "[\"a\", \"b\"]"
        );
        assert_eq!(call("reverse", &[s("abc")], sp()).unwrap(), s("cba"));
        assert_eq!(
            call("count", &[s("a.a.a"), s(".")], sp()).unwrap(),
            Value::int(2)
        );
        assert_eq!(
            call("count", &[s("abc"), s("")], sp()).unwrap(),
            Value::int(0)
        );
        assert_eq!(
            call("splitN", &[s("a:b:c"), s(":"), Value::float(2.0)], sp())
                .unwrap()
                .to_string(),
            "[\"a\", \"b:c\"]"
        );
        assert_eq!(
            call("splitN", &[s("a:b:c"), s(":"), Value::float(1.0)], sp())
                .unwrap()
                .to_string(),
            "[\"a:b:c\"]"
        );
        assert!(matches!(
            call("splitN", &[s("a:b"), s(":"), Value::float(0.0)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn codepoints_roundtrip() {
        let sp = sp();
        // codepoints → array<int> of Unicode scalar values.
        let cps = call("codepoints", &[s("Hi")], sp).unwrap();
        assert_eq!(cps.to_string(), "[72, 105]");
        // non-ASCII scalar (é U+00E9, 233).
        let cps2 = call("codepoints", &[s("é")], sp).unwrap();
        assert_eq!(cps2.to_string(), "[233]");
        // from_codepoints is the inverse.
        let arr = Value::array_cell(crate::value::ArrayCell::new(vec![
            Value::int(72),
            Value::int(105),
        ]));
        assert_eq!(call("from_codepoints", &[arr], sp).unwrap(), s("Hi"));
        // astral plane (emoji U+1F600).
        let astral = Value::array_cell(crate::value::ArrayCell::new(vec![Value::int(0x1F600)]));
        assert_eq!(call("from_codepoints", &[astral], sp).unwrap(), s("😀"));
        // integral floats are accepted as code points.
        let fl = Value::array_cell(crate::value::ArrayCell::new(vec![Value::float(65.0)]));
        assert_eq!(call("from_codepoints", &[fl], sp).unwrap(), s("A"));
    }

    #[test]
    fn from_codepoints_rejects_invalid() {
        let sp = sp();
        // Surrogate (U+D800) is not a scalar value.
        let surr = Value::array_cell(crate::value::ArrayCell::new(vec![Value::int(0xD800)]));
        assert!(matches!(
            call("from_codepoints", &[surr], sp),
            Err(Control::Panic(_))
        ));
        // Beyond U+10FFFF.
        let over = Value::array_cell(crate::value::ArrayCell::new(vec![Value::int(0x110000)]));
        assert!(matches!(
            call("from_codepoints", &[over], sp),
            Err(Control::Panic(_))
        ));
        // Negative.
        let neg = Value::array_cell(crate::value::ArrayCell::new(vec![Value::int(-1)]));
        assert!(matches!(
            call("from_codepoints", &[neg], sp),
            Err(Control::Panic(_))
        ));
        // Non-int element.
        let bad = Value::array_cell(crate::value::ArrayCell::new(vec![s("x")]));
        assert!(matches!(
            call("from_codepoints", &[bad], sp),
            Err(Control::Panic(_))
        ));
        // Non-integral float.
        let frac = Value::array_cell(crate::value::ArrayCell::new(vec![Value::float(65.5)]));
        assert!(matches!(
            call("from_codepoints", &[frac], sp),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn code_at_basic_and_bounds() {
        let sp = sp();
        assert_eq!(
            call("code_at", &[s("ABC"), Value::int(0)], sp).unwrap(),
            Value::int(65)
        );
        assert_eq!(
            call("code_at", &[s("ABC"), Value::int(2)], sp).unwrap(),
            Value::int(67)
        );
        // out of range → panic.
        assert!(matches!(
            call("code_at", &[s("ABC"), Value::int(3)], sp),
            Err(Control::Panic(_))
        ));
        // negative → panic.
        assert!(matches!(
            call("code_at", &[s("ABC"), Value::int(-1)], sp),
            Err(Control::Panic(_))
        ));
        // non-integral float index → panic.
        assert!(matches!(
            call("code_at", &[s("ABC"), Value::float(1.5)], sp),
            Err(Control::Panic(_))
        ));
    }

    // ── repeat: alloc-size guards (regression for the `Inf as usize` host abort) ─
    #[test]
    fn repeat_count_guards_are_tier2() {
        let sp = sp();
        // A finite, in-range count still works.
        assert_eq!(call("repeat", &[s("ab"), Value::int(3)], sp).unwrap(), s("ababab"));
        assert_eq!(call("repeat", &[s("x"), Value::int(0)], sp).unwrap(), s(""));
        // Infinity (`1/0`) must be a CLEAN Tier-2 panic, NOT a `usize::MAX` abort.
        assert!(matches!(
            call("repeat", &[s("x"), Value::float(f64::INFINITY)], sp),
            Err(Control::Panic(_))
        ));
        // NaN → panic.
        assert!(matches!(
            call("repeat", &[s("x"), Value::float(f64::NAN)], sp),
            Err(Control::Panic(_))
        ));
        // Huge finite count (10^18) → OOM-class allocation → panic, not abort.
        assert!(matches!(
            call("repeat", &[s("x"), Value::float(1e18)], sp),
            Err(Control::Panic(_))
        ));
        // Negative → panic.
        assert!(matches!(
            call("repeat", &[s("x"), Value::float(-1.0)], sp),
            Err(Control::Panic(_))
        ));
        // In-range count but the PRODUCT overflows the bound → panic (no `repeat` panic).
        assert!(matches!(
            call("repeat", &[s("0123456789"), Value::float(u32::MAX as f64)], sp),
            Err(Control::Panic(_))
        ));
    }

    // ── padStart/padEnd: width guards (alloc via `take(need)`) ─────────────────
    #[test]
    fn pad_width_guards_are_tier2() {
        let sp = sp();
        assert_eq!(
            call("padStart", &[s("7"), Value::int(3), s("0")], sp).unwrap(),
            s("007")
        );
        // Infinite width → clean Tier-2 panic, not a `usize::MAX` cycle/take abort.
        assert!(matches!(
            call("padStart", &[s("x"), Value::float(f64::INFINITY), s("-")], sp),
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            call("padEnd", &[s("x"), Value::float(1e18), s("-")], sp),
            Err(Control::Panic(_))
        ));
    }
}
