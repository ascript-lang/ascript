//! `std/toml` — TOML parse/stringify, bridged through serde_json::Value
//! (reuses the std/json converter).
//!
//! NOTE: this module is `crate::stdlib::toml`, which shadows the external
//! `toml` crate name within this file. We use the leading-`::` extern-crate
//! path (`::toml::from_str` / `::toml::to_string`) so the calls always resolve
//! to the crate, never to this module.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::stdlib::json::{from_ascript, to_ascript};
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("parse", bi("toml.parse")),
        ("stringify", bi("toml.stringify")),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("toml.{}", f);
    match func {
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            match ::toml::from_str::<serde_json::Value>(&s) {
                Ok(jv) => Ok(make_pair(to_ascript(&jv), Value::nil())),
                Err(e) => Ok(make_pair(
                    Value::nil(),
                    make_error(Value::str(format!("invalid TOML: {}", e))),
                )),
            }
        }
        "stringify" => {
            let v = arg(args, 0);
            match from_ascript(&v, &mut Vec::new()) {
                Ok(jv) => match ::toml::to_string(&jv) {
                    Ok(text) => Ok(make_pair(Value::str(text), Value::nil())),
                    Err(e) => Ok(make_pair(
                        Value::nil(),
                        make_error(Value::str(
                            format!("cannot serialize to TOML: {}", e),
                        )),
                    )),
                },
                Err(msg) => Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
            }
        }
        _ => Err(AsError::at(format!("std/toml has no function '{}'", func), span).into()),
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
    fn parse_basic() {
        // Keys come out in source order (serde_json's preserve_order feature
        // backs the bridge map with an IndexMap). The integer `36` renders as
        // `36` (not `36.0`) thanks to the json converter's integer handling.
        let parsed = call("parse", &[s("name = \"Ada\"\nage = 36")], sp()).unwrap();
        assert_eq!(parsed.to_string(), "[{name: \"Ada\", age: 36}, nil]");
    }

    #[test]
    fn parse_invalid_is_err() {
        assert!(call("parse", &[s("= bad")], sp())
            .unwrap()
            .to_string()
            .starts_with("[nil, {message:"));
    }

    #[test]
    fn stringify_table() {
        // TOML top level must be a table → an object.
        let mut m = indexmap::IndexMap::new();
        m.insert("k".to_string(), Value::str("v"));
        let obj = Value::object(m);
        let out = call("stringify", std::slice::from_ref(&obj), sp()).unwrap();
        assert_eq!(out.to_string(), "[\"k = \\\"v\\\"\\n\", nil]");
    }

    #[test]
    fn stringify_integer_not_float() {
        // NUM §4: an `int` renders as a TOML integer (`k = 1`); a `float` renders
        // as a TOML float (`k = 1.0`) — the subtype is preserved through
        // `from_ascript` (JSON), so the two are now genuinely distinguishable.
        let mut m = indexmap::IndexMap::new();
        m.insert("k".to_string(), Value::int(1));
        let obj = Value::object(m);
        let out = call("stringify", std::slice::from_ref(&obj), sp()).unwrap();
        assert_eq!(out.to_string(), "[\"k = 1\\n\", nil]");

        let mut m = indexmap::IndexMap::new();
        m.insert("k".to_string(), Value::float(1.0));
        let obj = Value::object(m);
        let out = call("stringify", std::slice::from_ref(&obj), sp()).unwrap();
        assert_eq!(out.to_string(), "[\"k = 1.0\\n\", nil]");
    }

    #[test]
    fn stringify_non_table_is_err() {
        // A bare number cannot be represented at the TOML top level → Tier-1 err.
        let out = call("stringify", &[Value::float(5.0)], sp()).unwrap();
        assert!(out.to_string().starts_with("[nil, {message:"));
    }
}
