//! `std/string` — string manipulation.

use super::{arg, bi, clamp_index, want_array, want_number, want_string};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

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
        ("format", bi("string.format")),
        ("padStart", bi("string.padStart")),
        ("padEnd", bi("string.padEnd")),
        ("repeat", bi("string.repeat")),
    ]
}

fn str_val(s: String) -> Value {
    Value::Str(s.into())
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
                s.split(sep.as_ref()).map(|p| str_val(p.to_string())).collect()
            };
            Ok(Value::Array(Rc::new(RefCell::new(parts))))
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
                None | Some(Value::Nil) => len,
                Some(v) => clamp_index(want_number(v, span, &ctx("slice"))?, len),
            };
            let slice: String = if start < end { chars[start..end].iter().collect() } else { String::new() };
            Ok(str_val(slice))
        }
        "trim" => Ok(str_val(want_string(&arg(args, 0), span, &ctx("trim"))?.trim().to_string())),
        "upper" => Ok(str_val(want_string(&arg(args, 0), span, &ctx("upper"))?.to_uppercase())),
        "lower" => Ok(str_val(want_string(&arg(args, 0), span, &ctx("lower"))?.to_lowercase())),
        "find" => {
            let s = want_string(&arg(args, 0), span, &ctx("find"))?;
            let sub = want_string(&arg(args, 1), span, &ctx("find"))?;
            match s.find(sub.as_ref()) {
                Some(byte_idx) => Ok(Value::Number(s[..byte_idx].chars().count() as f64)),
                None => Ok(Value::Number(-1.0)),
            }
        }
        "replace" => {
            let s = want_string(&arg(args, 0), span, &ctx("replace"))?;
            let from = want_string(&arg(args, 1), span, &ctx("replace"))?;
            let to = want_string(&arg(args, 2), span, &ctx("replace"))?;
            let result = if from.is_empty() { s.to_string() } else { s.replace(from.as_ref(), to.as_ref()) };
            Ok(str_val(result))
        }
        "format" => {
            let template = want_string(&arg(args, 0), span, &ctx("format"))?;
            Ok(str_val(format_template(&template, &args[1.min(args.len())..], span)?))
        }
        "padStart" | "padEnd" => {
            let s = want_string(&arg(args, 0), span, &ctx(func))?;
            let width = want_number(&arg(args, 1), span, &ctx(func))? as usize;
            let fill = match args.get(2) {
                None | Some(Value::Nil) => " ".to_string(),
                Some(v) => want_string(v, span, &ctx(func))?.to_string(),
            };
            let cur = s.chars().count();
            if cur >= width || fill.is_empty() {
                return Ok(str_val(s.to_string()));
            }
            let need = width - cur;
            let pad: String = fill.chars().cycle().take(need).collect();
            let result = if func == "padStart" { format!("{}{}", pad, s) } else { format!("{}{}", s, pad) };
            Ok(str_val(result))
        }
        "repeat" => {
            let s = want_string(&arg(args, 0), span, &ctx("repeat"))?;
            let n = want_number(&arg(args, 1), span, &ctx("repeat"))?;
            if n < 0.0 {
                return Err(AsError::at("string.repeat count must be non-negative", span).into());
            }
            Ok(str_val(s.repeat(n as usize)))
        }
        _ => Err(AsError::at(format!("std/string has no function '{}'", func), span).into()),
    }
}

/// `format("Hello {}, you are {}", name, age)`. `{}` consumes the next argument
/// in order; `{{` and `}}` are literal braces. Too few args → panic.
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
                    None => return Err(AsError::at("string.format: not enough arguments for placeholders", span).into()),
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
    fn s(x: &str) -> Value { Value::Str(x.into()) }
    fn sp() -> Span { Span::new(0, 0) }

    #[test]
    fn splits_and_joins() {
        let parts = call("split", &[s("a,b,c"), s(",")], sp()).unwrap();
        assert_eq!(parts.to_string(), "[\"a\", \"b\", \"c\"]");
        let joined = call("join", &[parts, s("-")], sp()).unwrap();
        assert_eq!(joined, s("a-b-c"));
    }

    #[test]
    fn slice_trim_case() {
        assert_eq!(call("slice", &[s("hello"), Value::Number(1.0), Value::Number(4.0)], sp()).unwrap(), s("ell"));
        assert_eq!(call("slice", &[s("hello"), Value::Number(-2.0)], sp()).unwrap(), s("lo"));
        assert_eq!(call("trim", &[s("  hi  ")], sp()).unwrap(), s("hi"));
        assert_eq!(call("upper", &[s("aB")], sp()).unwrap(), s("AB"));
        assert_eq!(call("lower", &[s("aB")], sp()).unwrap(), s("ab"));
    }

    #[test]
    fn find_replace_format_pad_repeat() {
        assert_eq!(call("find", &[s("hello"), s("ll")], sp()).unwrap(), Value::Number(2.0));
        assert_eq!(call("find", &[s("hello"), s("z")], sp()).unwrap(), Value::Number(-1.0));
        assert_eq!(call("replace", &[s("a.b.c"), s("."), s("-")], sp()).unwrap(), s("a-b-c"));
        assert_eq!(call("format", &[s("{} + {} = {}"), Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)], sp()).unwrap(), s("1 + 2 = 3"));
        assert_eq!(call("format", &[s("{{literal}}")], sp()).unwrap(), s("{literal}"));
        assert_eq!(call("padStart", &[s("7"), Value::Number(3.0), s("0")], sp()).unwrap(), s("007"));
        assert_eq!(call("padEnd", &[s("7"), Value::Number(3.0)], sp()).unwrap(), s("7  "));
        assert_eq!(call("repeat", &[s("ab"), Value::Number(3.0)], sp()).unwrap(), s("ababab"));
    }

    #[test]
    fn misuse_panics() {
        assert!(matches!(call("split", &[Value::Number(1.0), s(",")], sp()), Err(Control::Panic(_))));
        assert!(matches!(call("format", &[s("{}")], sp()), Err(Control::Panic(_))));
    }
}
