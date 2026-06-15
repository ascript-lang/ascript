//! `std/template` — minimal `{{name}}` string templating (core, NOT feature-gated).
//!
//! `template.render(tmpl, data) -> [string, err]` substitutes `{{path}}`
//! placeholders in `tmpl` with values resolved from `data` (an Object / Instance /
//! Map). Paths are dotted (`{{a.b.c}}`). This is a pure function — no resource,
//! no awaits.
//!
//! ## Decisions (per the SP5 spec recommendations)
//! - **Syntax:** `{{path}}` (Mustache-ish), distinct from AScript's own `${…}`
//!   string interpolation so there's no confusion.
//! - **Missing key:** a Tier-1 error (strict) — `render` returns `[nil, err]` whose
//!   message names the unresolved path. (No silent empty substitution.)
//! - **Escaping:** raw by default (output is not assumed to be HTML).
//! - **No loops/conditionals** — that would be a templating language; out of scope.
//!
//! A literal `{{` with no closing `}}` is a Tier-1 error ("unterminated
//! placeholder"). Whitespace inside the braces is trimmed (`{{ name }}` == `{{name}}`).

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::{MapKey, Value, ValueKind};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("render", bi("template.render"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg)))
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "render" => {
            let tmpl = want_string(&arg(args, 0), span, "template.render")?;
            let data = arg(args, 1);
            match render(&tmpl, &data) {
                Ok(s) => Ok(make_pair(Value::str(s), Value::nil())),
                Err(msg) => Ok(err_pair(msg)),
            }
        }
        _ => Err(AsError::at(format!("std/template has no function '{}'", func), span).into()),
    }
}

/// Render `tmpl`, substituting `{{path}}` placeholders against `data`. Returns the
/// rendered string, or an error message (missing key / unterminated placeholder).
fn render(tmpl: &str, data: &Value) -> Result<String, String> {
    let mut out = String::with_capacity(tmpl.len());
    let bytes = tmpl.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for the next "{{".
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Find the closing "}}".
            let rest = &tmpl[i + 2..];
            let close = match rest.find("}}") {
                Some(pos) => pos,
                None => return Err("template: unterminated placeholder '{{'".to_string()),
            };
            let path = rest[..close].trim();
            if path.is_empty() {
                return Err("template: empty placeholder '{{}}'".to_string());
            }
            let value = resolve_path(data, path)?;
            out.push_str(&stringify(&value));
            i += 2 + close + 2; // skip "{{" + path + "}}"
        } else {
            // Copy the current char (handle UTF-8 by char boundary).
            let ch = tmpl[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    Ok(out)
}

/// Resolve a dotted `path` against `data`, descending through Object/Instance/Map
/// at each segment. A missing segment is a Tier-1 error.
fn resolve_path(data: &Value, path: &str) -> Result<Value, String> {
    let mut current = data.clone();
    for (depth, segment) in path.split('.').enumerate() {
        let next = lookup(&current, segment);
        match next {
            Some(v) => current = v,
            None => {
                let so_far = path
                    .split('.')
                    .take(depth + 1)
                    .collect::<Vec<_>>()
                    .join(".");
                return Err(format!("template: missing key '{}'", so_far));
            }
        }
    }
    Ok(current)
}

/// Look up a single key on an Object, Instance, or Map; `None` if absent.
fn lookup(v: &Value, key: &str) -> Option<Value> {
    match v.kind() {
        ValueKind::Object(o) => o.get(key),
        ValueKind::Instance(inst) => inst.borrow().get(key),
        ValueKind::Map(m) => {
            let mk = MapKey::from_value(&Value::str(key))?;
            m.borrow().get(&mk).cloned()
        }
        _ => None,
    }
}

/// Stringify a substituted value. Strings pass through verbatim; everything else
/// uses the canonical Display (numbers, bools, nil, etc.).
fn stringify(v: &Value) -> String {
    match v.kind() {
        ValueKind::Str(s) => s.to_string(),
        _ => format!("{}", v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn obj(pairs: &[(&str, Value)]) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        Value::object(m)
    }
    fn ok(v: Value) -> String {
        match v.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                assert_eq!(b[1], Value::nil(), "expected nil err, got {:?}", b[1]);
                match b[0].kind() {
                    ValueKind::Str(s) => s.to_string(),
                    _ => panic!("expected string, got {:?}", b[0]),
                }
            }
            _ => panic!("expected pair"),
        }
    }
    fn is_err(v: &Value) -> bool {
        matches!(v.kind(), ValueKind::Array(a) if { let b = a.borrow(); b[0] == Value::nil() && b[1] != Value::nil() })
    }

    #[test]
    fn simple_substitution() {
        let data = obj(&[("name", Value::str("Ada"))]);
        let out = call("render", &[Value::str("Hi {{name}}!"), data], sp()).unwrap();
        assert_eq!(ok(out), "Hi Ada!");
    }

    #[test]
    fn dotted_path_and_number() {
        let inner = obj(&[("b", Value::float(1.0))]);
        let data = obj(&[("a", inner)]);
        let out = call(
            "render",
            &[Value::str("v={{a.b}}"), data],
            sp(),
        )
        .unwrap();
        assert_eq!(ok(out), "v=1.0");
    }

    #[test]
    fn whitespace_trimmed() {
        let data = obj(&[("x", Value::str("y"))]);
        let out = call("render", &[Value::str("{{ x }}"), data], sp()).unwrap();
        assert_eq!(ok(out), "y");
    }

    #[test]
    fn literal_text_passthrough() {
        let out = call(
            "render",
            &[Value::str("no placeholders here"), Value::nil()],
            sp(),
        )
        .unwrap();
        assert_eq!(ok(out), "no placeholders here");
    }

    #[test]
    fn missing_key_is_tier1_err() {
        let data = obj(&[("name", Value::str("Ada"))]);
        let out = call("render", &[Value::str("{{nope}}"), data], sp()).unwrap();
        assert!(is_err(&out), "expected Tier-1 err, got {:?}", out);
    }

    #[test]
    fn missing_nested_key_names_the_path() {
        let data = obj(&[("a", obj(&[("b", Value::float(1.0))]))]);
        let out = call("render", &[Value::str("{{a.c}}"), data], sp()).unwrap();
        match out.kind() {
            ValueKind::Array(arr) => {
                let b = arr.borrow();
                match b[1].kind() {
                    ValueKind::Object(o) => match o.get("message") {
                        Some(v) => match v.kind() {
                            ValueKind::Str(s) => assert!(s.contains("a.c"), "msg: {}", s),
                            _ => panic!("no message"),
                        },
                        _ => panic!("no message"),
                    },
                    _ => panic!("no err object"),
                }
            }
            _ => panic!("expected pair"),
        }
    }

    #[test]
    fn unterminated_placeholder_is_err() {
        let out = call(
            "render",
            &[Value::str("oops {{name"), obj(&[])],
            sp(),
        )
        .unwrap();
        assert!(is_err(&out), "expected Tier-1 err, got {:?}", out);
    }
}
