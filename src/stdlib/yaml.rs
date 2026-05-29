//! `std/yaml` — YAML parse/stringify, bridged through serde_json::Value
//! (reuses the std/json converter).

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
        // Mapping keys come out in source order (serde_json's preserve_order
        // feature backs the bridge map with an IndexMap). `36` renders as `36`
        // (not `36.0`).
        let parsed = call("parse", &[s("name: Ada\nage: 36\ntags:\n  - a\n  - b")], sp()).unwrap();
        assert_eq!(parsed.to_string(), "[{name: \"Ada\", age: 36, tags: [\"a\", \"b\"]}, nil]");
    }

    #[test]
    fn stringify_roundtrip() {
        // Stringify a value and parse it back; data is preserved.
        let mut m = indexmap::IndexMap::new();
        m.insert("x".to_string(), Value::Number(1.0));
        let obj = Value::Object(std::rc::Rc::new(std::cell::RefCell::new(m)));
        let out = call("stringify", std::slice::from_ref(&obj), sp()).unwrap();
        assert_eq!(out.to_string(), "[\"x: 1\\n\", nil]");
        // round-trip back through parse
        let text = s("x: 1\n");
        let reparsed = call("parse", std::slice::from_ref(&text), sp()).unwrap();
        assert_eq!(reparsed.to_string(), "[{x: 1}, nil]");
    }

    #[test]
    fn parse_invalid_is_err() {
        // A tab character used for indentation in a mapping is rejected by YAML.
        let out = call("parse", &[s("a:\n\t- x")], sp()).unwrap();
        assert!(out.to_string().starts_with("[nil, {message:"));
    }
}
