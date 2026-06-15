//! `std/regex` — compiled regular expressions (backed by the `regex` crate).

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::{RegexHandle, Value, ValueKind};
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

fn arr(v: Vec<Value>) -> Value {
    Value::array(v)
}

/// Resolve arg 0 to a compiled regex: a `Value::regex` is used directly; a
/// `Value::str` is compiled on the fly (a bad inline pattern is a Tier-2 panic —
/// use `compile` for the Tier-1 path on untrusted patterns).
fn want_regex(v: &Value, span: Span, ctx: &str) -> Result<Rc<RegexHandle>, Control> {
    match v.kind() {
        ValueKind::Regex(r) => Ok(r.clone()),
        ValueKind::Str(s) => match regex::Regex::new(s) {
            Ok(re) => Ok(Rc::new(RegexHandle {
                re,
                source: s.to_string(),
            })),
            Err(e) => {
                Err(AsError::at(format!("{}: invalid regex pattern: {}", ctx, e), span).into())
            }
        },
        _ => Err(AsError::at(
            format!(
                "{} expects a regex or pattern string, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

/// NUM §4: a match's character offset is an `int`.
fn char_index(haystack: &str, byte_idx: usize) -> i64 {
    haystack[..byte_idx].chars().count() as i64
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("regex.{}", f);
    match func {
        "compile" => {
            let s = want_string(&arg(args, 0), span, &ctx("compile"))?;
            match regex::Regex::new(&s) {
                Ok(re) => Ok(make_pair(
                    Value::regex(Rc::new(RegexHandle {
                        re,
                        source: s.to_string(),
                    })),
                    Value::nil(),
                )),
                Err(e) => Ok(make_pair(
                    Value::nil(),
                    make_error(Value::str(format!("invalid regex: {}", e))),
                )),
            }
        }
        "test" => {
            let re = want_regex(&arg(args, 0), span, &ctx("test"))?;
            let s = want_string(&arg(args, 1), span, &ctx("test"))?;
            Ok(Value::bool_(re.re.is_match(&s)))
        }
        "find" => {
            let re = want_regex(&arg(args, 0), span, &ctx("find"))?;
            let s = want_string(&arg(args, 1), span, &ctx("find"))?;
            match re.re.captures(&s) {
                Some(caps) => {
                    let whole = caps.get(0).unwrap();
                    let groups: Vec<Value> = caps
                        .iter()
                        .skip(1)
                        .map(|g| {
                            g.map(|m| Value::str(m.as_str()))
                                .unwrap_or(Value::nil())
                        })
                        .collect();
                    let mut obj = indexmap::IndexMap::new();
                    obj.insert("text".to_string(), Value::str(whole.as_str()));
                    obj.insert(
                        "index".to_string(),
                        Value::int(char_index(&s, whole.start())),
                    );
                    obj.insert("groups".to_string(), arr(groups));
                    Ok(Value::object(obj))
                }
                None => Ok(Value::nil()),
            }
        }
        "findAll" => {
            let re = want_regex(&arg(args, 0), span, &ctx("findAll"))?;
            let s = want_string(&arg(args, 1), span, &ctx("findAll"))?;
            let out: Vec<Value> = re
                .re
                .find_iter(&s)
                .map(|m| Value::str(m.as_str()))
                .collect();
            Ok(arr(out))
        }
        "replace" => {
            let re = want_regex(&arg(args, 0), span, &ctx("replace"))?;
            let s = want_string(&arg(args, 1), span, &ctx("replace"))?;
            let repl = want_string(&arg(args, 2), span, &ctx("replace"))?;
            Ok(Value::str(
                re.re.replace_all(&s, repl.as_ref()).into_owned(),
            ))
        }
        "split" => {
            let re = want_regex(&arg(args, 0), span, &ctx("split"))?;
            let s = want_string(&arg(args, 1), span, &ctx("split"))?;
            let out: Vec<Value> = re.re.split(&s).map(Value::str).collect();
            Ok(arr(out))
        }
        _ => Err(AsError::at(format!("std/regex has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn s(x: &str) -> Value {
        Value::str(x)
    }

    #[test]
    fn test_find_findall_replace_split() {
        assert_eq!(
            call("test", &[s("\\d+"), s("ab12")], sp()).unwrap(),
            Value::bool_(true)
        );
        let found = call("find", &[s("(\\d)(\\d)"), s("x42y")], sp()).unwrap();
        assert_eq!(
            found.to_string(),
            "{text: \"42\", index: 1, groups: [\"4\", \"2\"]}"
        );
        assert_eq!(
            call("findAll", &[s("\\d"), s("a1b2")], sp())
                .unwrap()
                .to_string(),
            "[\"1\", \"2\"]"
        );
        assert_eq!(
            call("replace", &[s("\\d"), s("a1b2"), s("#")], sp()).unwrap(),
            s("a#b#")
        );
        assert_eq!(
            call("split", &[s(",\\s*"), s("a, b,c")], sp())
                .unwrap()
                .to_string(),
            "[\"a\", \"b\", \"c\"]"
        );
    }

    #[test]
    fn compile_ok_and_err_and_reuse() {
        let compiled = call("compile", &[s("[a-z]+")], sp()).unwrap();
        assert!(compiled.to_string().starts_with("[<regex [a-z]+>, nil]"));
        let bad = call("compile", &[s("(")], sp()).unwrap();
        assert!(bad.to_string().starts_with("[nil, {message:"));
        // reuse: the compiled value works across multiple calls
        if let ValueKind::Array(a) = compiled.kind() {
            let re = a.borrow()[0].clone();
            assert_eq!(
                call("test", &[re.clone(), s("hello")], sp()).unwrap(),
                Value::bool_(true)
            );
            assert_eq!(
                call("test", &[re, s("123")], sp()).unwrap(),
                Value::bool_(false)
            );
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn bad_inline_pattern_panics() {
        assert!(matches!(
            call("test", &[s("("), s("x")], sp()),
            Err(Control::Panic(_))
        ));
    }
}
