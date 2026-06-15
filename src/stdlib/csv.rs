//! `std/csv` — CSV parse/stringify (backed by the `csv` crate).

use super::{arg, bi, want_array, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::{Value, ValueKind};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("parse", bi("csv.parse")),
        ("stringify", bi("csv.stringify")),
    ]
}

fn arr(v: Vec<Value>) -> Value {
    Value::array(v)
}
fn str_v(s: &str) -> Value {
    Value::str(s)
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("csv.{}", f);
    match func {
        "parse" => {
            let text = want_string(&arg(args, 0), span, &ctx("parse"))?;
            let header = matches!(args.get(1).map(|v| v.kind()), Some(ValueKind::Object(o)) if matches!(o.get("header").as_ref().map(|v| v.kind()), Some(ValueKind::Bool(true))));
            // parse is lenient — irregular quoting/ragged rows are coerced rather
            // than rejected; only genuine reader errors (I/O or UTF-8) reach the
            // Tier-1 err branch below. This matches the csv crate's permissive
            // default and real-world CSV.
            let mut rdr = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .from_reader(text.as_bytes());
            let mut records: Vec<Vec<String>> = Vec::new();
            for rec in rdr.records() {
                match rec {
                    Ok(r) => records.push(r.iter().map(|s| s.to_string()).collect()),
                    Err(e) => {
                        return Ok(make_pair(
                            Value::nil(),
                            make_error(str_v(&format!("invalid CSV: {}", e))),
                        ))
                    }
                }
            }
            let rows: Vec<Value> = if header {
                if records.is_empty() {
                    Vec::new()
                } else {
                    let head = records[0].clone();
                    records[1..]
                        .iter()
                        .map(|row| {
                            let mut o = indexmap::IndexMap::new();
                            for (i, key) in head.iter().enumerate() {
                                o.insert(
                                    key.clone(),
                                    str_v(row.get(i).map(|s| s.as_str()).unwrap_or("")),
                                );
                            }
                            Value::object(o)
                        })
                        .collect()
                }
            } else {
                records
                    .into_iter()
                    .map(|row| arr(row.iter().map(|s| str_v(s)).collect()))
                    .collect()
            };
            Ok(make_pair(arr(rows), Value::nil()))
        }
        "stringify" => {
            let rows = want_array(&arg(args, 0), span, &ctx("stringify"))?;
            let rows = rows.borrow();
            // The `csv` crate's WriterBuilder defaults to `\r\n` terminators; we
            // force `\n` for predictable cross-platform output (plan DECISION).
            let mut wtr = csv::WriterBuilder::new()
                .terminator(csv::Terminator::Any(b'\n'))
                .from_writer(vec![]);
            // Detect array-of-objects vs array-of-arrays from the first row.
            let as_objects = matches!(rows.first().map(|v| v.kind()), Some(ValueKind::Object(_)));
            if as_objects {
                // header = keys of the first object (insertion order)
                let header: Vec<String> = match rows.first().map(|v| v.kind()) {
                    Some(ValueKind::Object(o)) => o.keys_snapshot(),
                    _ => Vec::new(),
                };
                if wtr.write_record(&header).is_err() {
                    return Ok(make_pair(Value::nil(), make_error(str_v("CSV write error"))));
                }
                // Writing to an in-memory Vec is infallible, so data-row write
                // results are intentionally dropped (`let _ =`); any flush error
                // surfaces via `into_inner()` below.
                for row in rows.iter() {
                    let o = match row.kind() {
                        ValueKind::Object(o) => o,
                        _ => {
                            return Ok(make_pair(
                                Value::nil(),
                                make_error(str_v(
                                    "csv.stringify: mixed row kinds (expected all objects)",
                                )),
                            ))
                        }
                    };
                    let fields: Vec<String> = header
                        .iter()
                        .map(|k| o.get(k).map(|v| v.to_string()).unwrap_or_default())
                        .collect();
                    let _ = wtr.write_record(&fields);
                }
            } else {
                for row in rows.iter() {
                    let r = match row.kind() {
                        ValueKind::Array(a) => a,
                        _ => return Ok(make_pair(
                            Value::nil(),
                            make_error(str_v(
                                "csv.stringify expects an array of arrays or an array of objects",
                            )),
                        )),
                    };
                    let fields: Vec<String> = r.borrow().iter().map(|v| v.to_string()).collect();
                    // Infallible in-memory write; flush errors surface via into_inner().
                    let _ = wtr.write_record(&fields);
                }
            }
            match wtr.into_inner() {
                Ok(bytes) => Ok(make_pair(
                    str_v(&String::from_utf8_lossy(&bytes)),
                    Value::nil(),
                )),
                Err(e) => Ok(make_pair(
                    Value::nil(),
                    make_error(str_v(&format!("CSV write error: {}", e))),
                )),
            }
        }
        _ => Err(AsError::at(format!("std/csv has no function '{}'", func), span).into()),
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
    fn parse_rows_and_header() {
        let rows = call("parse", &[s("a,b\n1,2\n3,4")], sp()).unwrap();
        assert!(rows
            .to_string()
            .starts_with("[[[\"a\", \"b\"], [\"1\", \"2\"], [\"3\", \"4\"]], nil]"));
        let mut opt = indexmap::IndexMap::new();
        opt.insert("header".to_string(), Value::bool_(true));
        let withhdr = call(
            "parse",
            &[
                s("name,age\nAda,36"),
                Value::object(opt),
            ],
            sp(),
        )
        .unwrap();
        assert!(withhdr
            .to_string()
            .starts_with("[[{name: \"Ada\", age: \"36\"}], nil]"));
    }

    #[test]
    fn stringify_arrays_and_objects() {
        let data = arr(vec![
            arr(vec![s("x"), s("y")]),
            arr(vec![Value::float(1.0), Value::float(2.0)]),
        ]);
        let out = call("stringify", std::slice::from_ref(&data), sp()).unwrap();
        assert_eq!(out.to_string(), "[\"x,y\\n1.0,2.0\\n\", nil]");
    }
}
