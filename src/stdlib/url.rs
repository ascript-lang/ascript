//! `std/url` — URL parsing, building, and query-string utilities.
//!
//! All functions are pure and synchronous (no Interp/await needed).
//! Backed by the `url` crate for RFC-3986 conformant parsing and the
//! `percent-encoding` crate (already a `data` dependency) for
//! component encode/decode — identical behaviour to `encoding.urlEncode`
//! / `encoding.urlDecode`.

use super::{arg, bi, want_object, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use indexmap::IndexMap;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("parse", bi("url.parse")),
        ("parseQuery", bi("url.parseQuery")),
        ("buildQuery", bi("url.buildQuery")),
        ("build", bi("url.build")),
        ("encode", bi("url.encode")),
        ("decode", bi("url.decode")),
    ]
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Wrap a `&str` as `Value::Str`, or `Value::Nil` when the string is empty.
fn str_or_nil(s: &str) -> Value {
    if s.is_empty() {
        Value::Nil
    } else {
        Value::Str(s.into())
    }
}

/// Build a `Value::Object` from a list of `(&str, Value)` pairs.
fn make_obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m: IndexMap<String, Value> = IndexMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Object(crate::value::ObjectCell::new(m))
}

// ── public call entry ──────────────────────────────────────────────────────

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("url.{}", f);
    match func {
        // ── url.parse(s) -> [obj, err] ─────────────────────────────────────
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            match ::url::Url::parse(&s) {
                Ok(u) => {
                    // port: the url crate returns None when the port equals the
                    // scheme's default; we expose it as nil in that case too.
                    let port: Value = match u.port() {
                        Some(p) => Value::Number(f64::from(p)),
                        None => Value::Nil,
                    };
                    // username / password: url crate returns "" (not None) when absent
                    let username = str_or_nil(u.username());
                    let password = match u.password() {
                        Some(pw) => str_or_nil(pw),
                        None => Value::Nil,
                    };
                    // host: for file:// URLs there may be no host string
                    let host = match u.host_str() {
                        Some(h) => str_or_nil(h),
                        None => Value::Nil,
                    };
                    // query: raw query string (not decoded), or nil when absent
                    let query = match u.query() {
                        Some(q) => str_or_nil(q),
                        None => Value::Nil,
                    };
                    // fragment, or nil when absent
                    let fragment = match u.fragment() {
                        Some(f) => str_or_nil(f),
                        None => Value::Nil,
                    };
                    let obj = make_obj(vec![
                        ("scheme", Value::Str(u.scheme().into())),
                        ("host", host),
                        ("port", port),
                        ("path", Value::Str(u.path().into())),
                        ("query", query),
                        ("fragment", fragment),
                        ("username", username),
                        ("password", password),
                    ]);
                    Ok(make_pair(obj, Value::Nil))
                }
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("invalid URL: {}", e).into())),
                )),
            }
        }

        // ── url.parseQuery(s) -> object ────────────────────────────────────
        // Parses an `application/x-www-form-urlencoded` query string.
        // Repeated keys: last value wins.  Values are percent-decoded.
        "parseQuery" => {
            let s = want_string(&arg(args, 0), span, &ctx("parseQuery"))?;
            let mut m: IndexMap<String, Value> = IndexMap::new();
            // Use the url crate's built-in `form_urlencoded` parser so decoding
            // is consistent with how `url.parse` interprets query strings.
            for (k, v) in ::url::form_urlencoded::parse(s.as_bytes()) {
                m.insert(k.into_owned(), Value::Str(v.into_owned().into()));
            }
            Ok(Value::Object(crate::value::ObjectCell::new(m)))
        }

        // ── url.buildQuery(obj) -> string ──────────────────────────────────
        // Encodes an object's string values into `application/x-www-form-urlencoded`
        // format in insertion order.  Values are percent-encoded.
        "buildQuery" => {
            let o = want_object(&arg(args, 0), span, &ctx("buildQuery"))?;
            let mut ser = ::url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in o.borrow().iter() {
                let val = match v {
                    Value::Str(s) => s.to_string(),
                    Value::Number(n) => {
                        // integer-valued numbers without trailing ".0"
                        if n.fract() == 0.0 && n.is_finite() {
                            format!("{}", *n as i64)
                        } else {
                            format!("{}", n)
                        }
                    }
                    Value::Bool(b) => b.to_string(),
                    Value::Nil => String::new(),
                    other => {
                        return Err(AsError::at(
                            format!(
                                "url.buildQuery: object value must be a string, got {}",
                                crate::interp::type_name(other)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                ser.append_pair(k, &val);
            }
            Ok(Value::Str(ser.finish().into()))
        }

        // ── url.build(obj) -> [string, err] ───────────────────────────────
        // Assembles a URL from the component object (same shape as url.parse
        // output).  Tier-1 result — invalid components produce an error.
        "build" => {
            let o = want_object(&arg(args, 0), span, &ctx("build"))?;
            let borrow = o.borrow();
            let get_str = |key: &str| -> Option<String> {
                match borrow.get(key) {
                    Some(Value::Str(s)) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                }
            };
            let get_num = |key: &str| -> Option<f64> {
                match borrow.get(key) {
                    Some(Value::Number(n)) => Some(*n),
                    _ => None,
                }
            };

            let scheme = match get_str("scheme") {
                Some(s) => s,
                None => {
                    return Ok(make_pair(
                        Value::Nil,
                        make_error(Value::Str("url.build: 'scheme' is required".into())),
                    ))
                }
            };
            let host = get_str("host").unwrap_or_default();
            let path = get_str("path").unwrap_or_default();
            let port = get_num("port").map(|n| n as u16);
            let query = get_str("query");
            let fragment = get_str("fragment");
            let username = get_str("username");
            let password = get_str("password");

            // Build a base URL from scheme + host (required for url::Url::parse).
            let base = if host.is_empty() {
                format!("{}:", scheme)
            } else {
                format!("{}://{}", scheme, host)
            };
            match ::url::Url::parse(&base) {
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(
                        format!("url.build: invalid base '{}': {}", base, e).into(),
                    )),
                )),
                Ok(mut u) => {
                    // Set path
                    if !path.is_empty() {
                        u.set_path(&path);
                    }
                    // Port — ignore errors from invalid ports; url crate returns Err
                    // for ports that match the scheme default, which is fine.
                    if let Some(p) = port {
                        let _ = u.set_port(Some(p));
                    }
                    // Query
                    u.set_query(query.as_deref());
                    // Fragment
                    u.set_fragment(fragment.as_deref());
                    // Credentials
                    if let Some(user) = &username {
                        let _ = u.set_username(user);
                    }
                    if let Some(pass) = &password {
                        let _ = u.set_password(Some(pass));
                    }
                    Ok(make_pair(Value::Str(u.as_str().into()), Value::Nil))
                }
            }
        }

        // ── url.encode(s) -> string ────────────────────────────────────────
        // Percent-encodes a single URL component (same as encoding.urlEncode).
        "encode" => {
            let s = want_string(&arg(args, 0), span, &ctx("encode"))?;
            let encoded =
                percent_encoding::utf8_percent_encode(&s, percent_encoding::NON_ALPHANUMERIC)
                    .to_string();
            Ok(Value::Str(encoded.into()))
        }

        // ── url.decode(s) -> string ────────────────────────────────────────
        // Percent-decodes a single URL component.  Always returns a consistent
        // Tier-1 `[string, err]` pair: `[decoded, nil]` on success, `[nil, err]`
        // when the percent sequence is not valid UTF-8 — matching
        // encoding.urlDecode, url.parse, and url.build.
        "decode" => {
            let s = want_string(&arg(args, 0), span, &ctx("decode"))?;
            match percent_encoding::percent_decode_str(&s).decode_utf8() {
                Ok(decoded) => Ok(make_pair(
                    Value::Str(decoded.into_owned().into()),
                    Value::Nil,
                )),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(
                        format!("url.decode: invalid percent-encoding: {}", e).into(),
                    )),
                )),
            }
        }

        _ => Err(AsError::at(format!("std/url has no function '{}'", func), span).into()),
    }
}

// ── unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }
    /// Pull a named field from a `Value::Object`.
    fn field(obj: &Value, key: &str) -> Value {
        match obj {
            Value::Object(o) => o.borrow().get(key).cloned().unwrap_or(Value::Nil),
            _ => panic!("not an object: {:?}", obj),
        }
    }
    /// Extract index 0 from a `[val, err]` pair.
    fn ok_val(pair: &Value) -> Value {
        match pair {
            Value::Array(a) => a.borrow()[0].clone(),
            _ => panic!("not a pair"),
        }
    }
    /// Extract index 1 (the err slot) from a `[val, err]` pair.
    fn err_val(pair: &Value) -> Value {
        match pair {
            Value::Array(a) => a.borrow()[1].clone(),
            _ => panic!("not a pair"),
        }
    }

    // ── url.parse ─────────────────────────────────────────────────────────

    #[test]
    fn parse_full_url() {
        let pair = call(
            "parse",
            &[s("https://user:pass@host:8080/a/b?x=1&y=2#frag")],
            sp(),
        )
        .unwrap();
        let obj = ok_val(&pair);
        assert_eq!(err_val(&pair), Value::Nil);

        assert_eq!(field(&obj, "scheme"), s("https"));
        assert_eq!(field(&obj, "host"), s("host"));
        assert_eq!(field(&obj, "port"), Value::Number(8080.0));
        assert_eq!(field(&obj, "path"), s("/a/b"));
        assert_eq!(field(&obj, "query"), s("x=1&y=2"));
        assert_eq!(field(&obj, "fragment"), s("frag"));
        assert_eq!(field(&obj, "username"), s("user"));
        assert_eq!(field(&obj, "password"), s("pass"));
    }

    #[test]
    fn parse_minimal_url() {
        // A URL with no port, no query, no fragment, no credentials.
        let pair = call("parse", &[s("https://example.com/path")], sp()).unwrap();
        let obj = ok_val(&pair);
        assert_eq!(err_val(&pair), Value::Nil);

        assert_eq!(field(&obj, "scheme"), s("https"));
        assert_eq!(field(&obj, "host"), s("example.com"));
        assert_eq!(field(&obj, "port"), Value::Nil);
        assert_eq!(field(&obj, "path"), s("/path"));
        assert_eq!(field(&obj, "query"), Value::Nil);
        assert_eq!(field(&obj, "fragment"), Value::Nil);
        assert_eq!(field(&obj, "username"), Value::Nil);
        assert_eq!(field(&obj, "password"), Value::Nil);
    }

    #[test]
    fn parse_invalid_url() {
        let pair = call("parse", &[s("not a url")], sp()).unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        // err slot should be a {message:...} object
        assert!(pair.to_string().starts_with("[nil, {message:"));
    }

    // ── url.parseQuery ────────────────────────────────────────────────────

    #[test]
    fn parse_query_last_wins() {
        // "a=1&b=2&a=3" — second 'a' wins
        let obj = call("parseQuery", &[s("a=1&b=2&a=3")], sp()).unwrap();
        match &obj {
            Value::Object(o) => {
                let borrow = o.borrow();
                assert_eq!(borrow.get("a"), Some(&s("3")));
                assert_eq!(borrow.get("b"), Some(&s("2")));
            }
            _ => panic!("expected object, got {:?}", obj),
        }
    }

    #[test]
    fn parse_query_decodes_percent() {
        // "hello%20world" should decode to "hello world"
        let obj = call("parseQuery", &[s("k=hello%20world")], sp()).unwrap();
        assert_eq!(field(&obj, "k"), s("hello world"));
    }

    #[test]
    fn parse_query_empty() {
        let obj = call("parseQuery", &[s("")], sp()).unwrap();
        match &obj {
            Value::Object(o) => assert!(o.borrow().is_empty()),
            _ => panic!("expected object"),
        }
    }

    // ── url.buildQuery ────────────────────────────────────────────────────

    #[test]
    fn build_query_basic() {
        let mut m = IndexMap::new();
        m.insert("a".to_string(), s("1"));
        m.insert("b".to_string(), s("2"));
        let obj = Value::Object(crate::value::ObjectCell::new(m));
        let out = call("buildQuery", &[obj], sp()).unwrap();
        assert_eq!(out, s("a=1&b=2"));
    }

    #[test]
    fn build_query_encodes_special() {
        let mut m = IndexMap::new();
        m.insert("q".to_string(), s("hello world"));
        let obj = Value::Object(crate::value::ObjectCell::new(m));
        let out = call("buildQuery", &[obj], sp()).unwrap();
        // form_urlencoded uses '+' for spaces, not %20
        assert_eq!(out, s("q=hello+world"));
    }

    #[test]
    fn build_query_roundtrip() {
        // buildQuery then parseQuery should give back the same values.
        let mut m = IndexMap::new();
        m.insert("x".to_string(), s("foo bar"));
        m.insert("y".to_string(), s("a&b=c"));
        let obj = Value::Object(crate::value::ObjectCell::new(m));
        let qs = call("buildQuery", &[obj], sp()).unwrap();
        let parsed = call("parseQuery", std::slice::from_ref(&qs), sp()).unwrap();
        assert_eq!(field(&parsed, "x"), s("foo bar"));
        assert_eq!(field(&parsed, "y"), s("a&b=c"));
    }

    // ── url.encode / url.decode ───────────────────────────────────────────

    #[test]
    fn encode_decode_roundtrip() {
        let encoded = call("encode", &[s("a b&c")], sp()).unwrap();
        // NON_ALPHANUMERIC encodes space as %20 and & as %26
        assert_eq!(encoded, s("a%20b%26c"));
        // decode ALWAYS returns a Tier-1 [string, err] pair: [decoded, nil] on success
        let pair = call("decode", std::slice::from_ref(&encoded), sp()).unwrap();
        assert_eq!(ok_val(&pair), s("a b&c"));
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[test]
    fn decode_invalid_is_tier1_err() {
        // %FF is not valid UTF-8 → [nil, <err>]
        let pair = call("decode", &[s("%FF")], sp()).unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        assert!(pair.to_string().starts_with("[nil, {message:"));
    }

    #[test]
    fn encode_matches_encoding_module() {
        // url.encode must produce the same output as encoding.urlEncode for any
        // input — both use percent_encoding::NON_ALPHANUMERIC.
        use crate::stdlib::encoding;
        let input = s("foo/bar?baz=1");
        let via_url = call("encode", std::slice::from_ref(&input), sp()).unwrap();
        let via_enc = encoding::call("urlEncode", &[input], sp()).unwrap();
        assert_eq!(via_url, via_enc);
    }

    // ── url.build ─────────────────────────────────────────────────────────

    #[test]
    fn build_basic() {
        let mut m = IndexMap::new();
        m.insert("scheme".to_string(), s("https"));
        m.insert("host".to_string(), s("x"));
        m.insert("path".to_string(), s("/p"));
        let obj = Value::Object(crate::value::ObjectCell::new(m));
        let pair = call("build", &[obj], sp()).unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        let result = ok_val(&pair);
        assert_eq!(result, s("https://x/p"));
    }

    #[test]
    fn build_roundtrip_parse() {
        // Build a URL then parse it back; fields should match.
        let mut m = IndexMap::new();
        m.insert("scheme".to_string(), s("http"));
        m.insert("host".to_string(), s("example.com"));
        m.insert("port".to_string(), Value::Number(9090.0));
        m.insert("path".to_string(), s("/api/v1"));
        m.insert("query".to_string(), s("key=val"));
        let obj = Value::Object(crate::value::ObjectCell::new(m));
        let built = ok_val(&call("build", &[obj], sp()).unwrap());
        let parsed_pair = call("parse", std::slice::from_ref(&built), sp()).unwrap();
        let parsed = ok_val(&parsed_pair);
        assert_eq!(field(&parsed, "scheme"), s("http"));
        assert_eq!(field(&parsed, "host"), s("example.com"));
        assert_eq!(field(&parsed, "port"), Value::Number(9090.0));
        assert_eq!(field(&parsed, "path"), s("/api/v1"));
        assert_eq!(field(&parsed, "query"), s("key=val"));
    }

    #[test]
    fn build_missing_scheme_is_err() {
        let mut m = IndexMap::new();
        m.insert("host".to_string(), s("x"));
        let obj = Value::Object(crate::value::ObjectCell::new(m));
        let pair = call("build", &[obj], sp()).unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        assert!(pair.to_string().contains("scheme"));
    }
}
