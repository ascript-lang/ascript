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
use crate::value::{MapKey, Value};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("render", bi("template.render"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "render" => {
            let tmpl = want_string(&arg(args, 0), span, "template.render")?;
            let data = arg(args, 1);
            match render(&tmpl, &data) {
                Ok(s) => Ok(make_pair(Value::Str(s.into()), Value::Nil)),
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
    match v {
        Value::Object(o) => o.borrow().get(key).cloned(),
        Value::Instance(inst) => inst.borrow().fields.get(key).cloned(),
        Value::Map(m) => {
            let mk = MapKey::from_value(&Value::Str(key.into()))?;
            m.borrow().get(&mk).cloned()
        }
        _ => None,
    }
}

/// Stringify a substituted value. Strings pass through verbatim; everything else
/// uses the canonical Display (numbers, bools, nil, etc.).
fn stringify(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        other => format!("{}", other),
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
        Value::Object(crate::value::ObjectCell::new(m))
    }
    fn ok(v: Value) -> String {
        match v {
            Value::Array(a) => {
                let b = a.borrow();
                assert_eq!(b[1], Value::Nil, "expected nil err, got {:?}", b[1]);
                match &b[0] {
                    Value::Str(s) => s.to_string(),
                    other => panic!("expected string, got {:?}", other),
                }
            }
            _ => panic!("expected pair"),
        }
    }
    fn is_err(v: &Value) -> bool {
        matches!(v, Value::Array(a) if { let b = a.borrow(); b[0] == Value::Nil && b[1] != Value::Nil })
    }

    #[test]
    fn simple_substitution() {
        let data = obj(&[("name", Value::Str("Ada".into()))]);
        let out = call("render", &[Value::Str("Hi {{name}}!".into()), data], sp()).unwrap();
        assert_eq!(ok(out), "Hi Ada!");
    }

    #[test]
    fn dotted_path_and_number() {
        let inner = obj(&[("b", Value::Number(1.0))]);
        let data = obj(&[("a", inner)]);
        let out = call(
            "render",
            &[Value::Str("v={{a.b}}".into()), data],
            sp(),
        )
        .unwrap();
        assert_eq!(ok(out), "v=1");
    }

    #[test]
    fn whitespace_trimmed() {
        let data = obj(&[("x", Value::Str("y".into()))]);
        let out = call("render", &[Value::Str("{{ x }}".into()), data], sp()).unwrap();
        assert_eq!(ok(out), "y");
    }

    #[test]
    fn literal_text_passthrough() {
        let out = call(
            "render",
            &[Value::Str("no placeholders here".into()), Value::Nil],
            sp(),
        )
        .unwrap();
        assert_eq!(ok(out), "no placeholders here");
    }

    #[test]
    fn missing_key_is_tier1_err() {
        let data = obj(&[("name", Value::Str("Ada".into()))]);
        let out = call("render", &[Value::Str("{{nope}}".into()), data], sp()).unwrap();
        assert!(is_err(&out), "expected Tier-1 err, got {:?}", out);
    }

    #[test]
    fn missing_nested_key_names_the_path() {
        let data = obj(&[("a", obj(&[("b", Value::Number(1.0))]))]);
        let out = call("render", &[Value::Str("{{a.c}}".into()), data], sp()).unwrap();
        match &out {
            Value::Array(arr) => {
                let b = arr.borrow();
                match &b[1] {
                    Value::Object(o) => match o.borrow().get("message") {
                        Some(Value::Str(s)) => assert!(s.contains("a.c"), "msg: {}", s),
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
            &[Value::Str("oops {{name".into()), obj(&[])],
            sp(),
        )
        .unwrap();
        assert!(is_err(&out), "expected Tier-1 err, got {:?}", out);
    }
}
