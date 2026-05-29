//! `std/net/http` — modern HTTP client (feature `net`), spec §11.5.
//!
//! Verbs `get/post/put/patch/delete/head/options(url, opts?)` plus `request(opts)`
//! (where `opts.method` selects the verb). Every call is async and returns the
//! Tier-1 pair `[resp, err]`:
//!
//! - a connect / TLS / DNS / timeout failure → `[nil, err]`;
//! - otherwise `[resp, nil]` where `resp` is a `Value::Native(HttpResponse)` whose
//!   `fields` carry `status` (number), `ok` (200-299), `version` ("1.1"|"2"|...),
//!   `url` (final string), `headers` (object, lowercased keys) and `cookies` (an
//!   object of name→value parsed from `Set-Cookie`).
//!
//! A non-2xx response is NOT an error — it is a normal `resp` with `ok == false`.
//!
//! The response body is read lazily via async methods on the handle:
//! `await resp.text() → [string, err]`, `await resp.bytes() → [bytes, err]`,
//! `await resp.json() → [value, err]`. `reqwest::Response::{text,bytes,json}`
//! consume the response by value, so each accessor `take_resource`s it; a second
//! body accessor on the same handle is a Tier-2 panic "response body already
//! consumed". The metadata fields above are read at response time and need no
//! consumption.
//!
//! Request body shapes (`opts.body`): a string · bytes · `{json: value}` (serialized
//! via the shared std/json converter → `application/json`) · `{form: object}`
//! (urlencoded → `application/x-www-form-urlencoded`) · `{multipart: [...]}`
//! (`reqwest::multipart::Form`; each part `{name, value}` for a text field or
//! `{name, data, filename?, contentType?}` for a file/bytes part).
//!
//! Deferred to later M14 tasks: timeouts/redirects/retries/tls/cookies-jar/proxy/
//! httpVersion (Task 3), streaming response + request bodies (Task 4), `sse` (Task 5).

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    /// A process-wide default `reqwest::Client` (connection pool + cookie store off
    /// for the core verbs). The interp is single-threaded, so a thread-local cache
    /// is sufficient; per-request configuration (timeouts/redirects/tls) arrives in
    /// Task 3 and will build dedicated clients as needed.
    static DEFAULT_CLIENT: RefCell<Option<reqwest::Client>> = const { RefCell::new(None) };
}

fn default_client() -> reqwest::Client {
    DEFAULT_CLIENT.with(|c| {
        c.borrow_mut()
            .get_or_insert_with(|| {
                reqwest::Client::builder()
                    .build()
                    .expect("default reqwest client should build")
            })
            .clone()
    })
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("get", bi("net_http.get")),
        ("post", bi("net_http.post")),
        ("put", bi("net_http.put")),
        ("patch", bi("net_http.patch")),
        ("delete", bi("net_http.delete")),
        ("head", bi("net_http.head")),
        ("options", bi("net_http.options")),
        ("request", bi("net_http.request")),
    ]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

fn bytes_value(b: Vec<u8>) -> Value {
    Value::Bytes(Rc::new(RefCell::new(b)))
}

fn obj(map: IndexMap<String, Value>) -> Value {
    Value::Object(Rc::new(RefCell::new(map)))
}

/// Pull `opts.<key>` (an object) when present and non-nil.
fn opt_field(opts: &Value, key: &str) -> Option<Value> {
    match opts {
        Value::Object(o) => match o.borrow().get(key) {
            Some(Value::Nil) | None => None,
            Some(v) => Some(v.clone()),
        },
        _ => None,
    }
}

/// Map an AScript value to URL-query / form string pairs. Each value is rendered
/// with its scalar string form; arrays expand to repeated keys (`k=a&k=b`).
fn value_to_query_pairs(v: &Value, span: Span, ctx: &str) -> Result<Vec<(String, String)>, Control> {
    let o = match v {
        Value::Object(o) => o,
        other => {
            return Err(AsError::at(
                format!("{} expects an object, got {}", ctx, crate::interp::type_name(other)),
                span,
            )
            .into())
        }
    };
    let mut pairs = Vec::new();
    for (k, val) in o.borrow().iter() {
        match val {
            Value::Array(a) => {
                for item in a.borrow().iter() {
                    pairs.push((k.clone(), scalar_to_string(item, span, ctx)?));
                }
            }
            _ => pairs.push((k.clone(), scalar_to_string(val, span, ctx)?)),
        }
    }
    Ok(pairs)
}

/// Render a scalar (string/number/bool/nil) into its query/form string form.
fn scalar_to_string(v: &Value, span: Span, ctx: &str) -> Result<String, Control> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        Value::Number(_) | Value::Bool(_) => Ok(v.to_string()),
        Value::Nil => Ok(String::new()),
        other => Err(AsError::at(
            format!("{} value must be a string/number/bool, got {}", ctx, crate::interp::type_name(other)),
            span,
        )
        .into()),
    }
}

impl Interp {
    /// Module-level dispatch for `std/net/http` (the verbs + `request`).
    pub(crate) async fn call_http(
        &mut self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "get" | "post" | "put" | "patch" | "delete" | "head" | "options" => {
                let method = func.to_ascii_uppercase();
                let url = want_string(&arg(args, 0), span, &format!("net/http.{}", func))?;
                let opts = arg(args, 1);
                self.call_http_send(&method, url.to_string(), &opts, span).await
            }
            "request" => {
                let opts = arg(args, 0);
                let method = match opt_field(&opts, "method") {
                    Some(m) => want_string(&m, span, "net/http.request method")?.to_ascii_uppercase(),
                    None => "GET".to_string(),
                };
                let url = match opt_field(&opts, "url") {
                    Some(u) => want_string(&u, span, "net/http.request url")?.to_string(),
                    None => {
                        return Err(AsError::at("net/http.request requires opts.url", span).into())
                    }
                };
                self.call_http_send(&method, url, &opts, span).await
            }
            _ => Err(AsError::at(format!("std/net/http has no function '{}'", func), span).into()),
        }
    }

    /// Build + send one request, returning the Tier-1 `[resp, err]` pair.
    async fn call_http_send(
        &mut self,
        method: &str,
        url: String,
        opts: &Value,
        span: Span,
    ) -> Result<Value, Control> {
        let m = match reqwest::Method::from_bytes(method.as_bytes()) {
            Ok(m) => m,
            Err(_) => return Err(AsError::at(format!("net/http: invalid method '{}'", method), span).into()),
        };
        let client = default_client();
        let mut rb = client.request(m, &url);

        // query: object → query pairs (merged onto the URL).
        if let Some(q) = opt_field(opts, "query") {
            let pairs = value_to_query_pairs(&q, span, "net/http query")?;
            rb = rb.query(&pairs);
        }

        // headers: object of string→string. `auth:` is a sibling helper key.
        if let Some(h) = opt_field(opts, "headers") {
            let map = match &h {
                Value::Object(o) => o,
                other => {
                    return Err(AsError::at(
                        format!("net/http headers expects an object, got {}", crate::interp::type_name(other)),
                        span,
                    )
                    .into())
                }
            };
            for (k, v) in map.borrow().iter() {
                let vs = scalar_to_string(v, span, "net/http header")?;
                rb = rb.header(k.as_str(), vs);
            }
        }

        // auth: {bearer: tok} → Authorization: Bearer; {basic: [user, pass]} → basic.
        if let Some(a) = opt_field(opts, "auth") {
            rb = self.apply_auth(rb, &a, span)?;
        }

        // body: string · bytes · {json} · {form} · {multipart}.
        if let Some(b) = opt_field(opts, "body") {
            rb = self.apply_body(rb, &b, span)?;
        }

        match rb.send().await {
            Ok(resp) => Ok(make_pair(self.http_response_value(resp), Value::Nil)),
            Err(e) => Ok(err_pair(format!("net/http {} {} failed: {}", method, url, e))),
        }
    }

    fn apply_auth(
        &self,
        rb: reqwest::RequestBuilder,
        auth: &Value,
        span: Span,
    ) -> Result<reqwest::RequestBuilder, Control> {
        let o = match auth {
            Value::Object(o) => o,
            other => {
                return Err(AsError::at(
                    format!("net/http auth expects an object, got {}", crate::interp::type_name(other)),
                    span,
                )
                .into())
            }
        };
        let o = o.borrow();
        if let Some(tok) = o.get("bearer") {
            let tok = want_string(tok, span, "net/http auth.bearer")?;
            return Ok(rb.bearer_auth(tok.to_string()));
        }
        if let Some(basic) = o.get("basic") {
            let arr = super::want_array(basic, span, "net/http auth.basic")?;
            let arr = arr.borrow();
            let user = want_string(arr.first().unwrap_or(&Value::Nil), span, "net/http auth.basic[0]")?;
            let pass = arr.get(1).cloned();
            let pass = match pass {
                Some(Value::Nil) | None => None,
                Some(p) => Some(want_string(&p, span, "net/http auth.basic[1]")?.to_string()),
            };
            return Ok(rb.basic_auth(user.to_string(), pass));
        }
        Err(AsError::at("net/http auth expects {bearer} or {basic:[user,pass]}", span).into())
    }

    fn apply_body(
        &self,
        rb: reqwest::RequestBuilder,
        body: &Value,
        span: Span,
    ) -> Result<reqwest::RequestBuilder, Control> {
        match body {
            Value::Str(s) => Ok(rb.body(s.to_string())),
            Value::Bytes(b) => Ok(rb.body(b.borrow().clone())),
            Value::Object(o) => {
                let o = o.borrow();
                if let Some(jv) = o.get("json") {
                    let json = crate::stdlib::json::from_ascript(jv, &mut Vec::new())
                        .map_err(|m| Control::from(AsError::at(format!("net/http body.json: {}", m), span)))?;
                    let bytes = serde_json::to_vec(&json)
                        .map_err(|e| Control::from(AsError::at(format!("net/http body.json: {}", e), span)))?;
                    return Ok(rb
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(bytes));
                }
                if let Some(form) = o.get("form") {
                    let pairs = value_to_query_pairs(form, span, "net/http body.form")?;
                    // `.form(&pairs)` urlencodes + sets application/x-www-form-urlencoded.
                    return Ok(rb.form(&pairs));
                }
                if let Some(mp) = o.get("multipart") {
                    let form = build_multipart(mp, span)?;
                    return Ok(rb.multipart(form));
                }
                Err(AsError::at(
                    "net/http body object must be {json}, {form}, or {multipart}",
                    span,
                )
                .into())
            }
            other => Err(AsError::at(
                format!(
                    "net/http body must be a string, bytes, or an object, got {}",
                    crate::interp::type_name(other)
                ),
                span,
            )
            .into()),
        }
    }

    /// Read the response metadata into `fields` and register the live response (for
    /// the body accessors) behind a `Value::Native(HttpResponse)` handle.
    fn http_response_value(&mut self, resp: reqwest::Response) -> Value {
        let status = resp.status();
        let mut fields = IndexMap::new();
        fields.insert("status".to_string(), Value::Number(status.as_u16() as f64));
        fields.insert("ok".to_string(), Value::Bool(status.is_success()));
        fields.insert("version".to_string(), Value::Str(http_version_str(resp.version()).into()));
        fields.insert("url".to_string(), Value::Str(resp.url().as_str().into()));

        // headers: object of lowercased name → value (last value wins on repeats,
        // except Set-Cookie which we fold into `cookies` below).
        let mut headers = IndexMap::new();
        let mut cookies = IndexMap::new();
        for (name, value) in resp.headers().iter() {
            let key = name.as_str().to_ascii_lowercase();
            let val = value.to_str().unwrap_or("").to_string();
            if key == "set-cookie" {
                if let Some((k, v)) = parse_set_cookie(&val) {
                    cookies.insert(k, Value::Str(v.into()));
                }
            }
            headers.insert(key, Value::Str(val.into()));
        }
        fields.insert("headers".to_string(), obj(headers));
        fields.insert("cookies".to_string(), obj(cookies));

        self.register_resource(NativeKind::HttpResponse, fields, ResourceState::HttpResponse(resp))
    }

    /// Dispatch a body accessor on an HTTP response handle: `text`/`bytes`/`json`.
    /// Each consumes the response (`take_http_response`); a second body accessor on
    /// the same handle is a Tier-2 panic.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_http_response_method(
        &mut self,
        m: &Rc<NativeMethod>,
        _args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        let method = m.method.as_str();
        match method {
            "text" | "bytes" | "json" => {
                let resp = match self.take_http_response(id) {
                    Some(r) => r,
                    None => {
                        return Err(AsError::at("response body already consumed", span).into())
                    }
                };
                match method {
                    "text" => match resp.text().await {
                        Ok(s) => Ok(make_pair(Value::Str(s.into()), Value::Nil)),
                        Err(e) => Ok(err_pair(format!("response.text failed: {}", e))),
                    },
                    "bytes" => match resp.bytes().await {
                        Ok(b) => Ok(make_pair(bytes_value(b.to_vec()), Value::Nil)),
                        Err(e) => Ok(err_pair(format!("response.bytes failed: {}", e))),
                    },
                    "json" => match resp.bytes().await {
                        Ok(b) => match serde_json::from_slice::<serde_json::Value>(&b) {
                            Ok(jv) => Ok(make_pair(crate::stdlib::json::to_ascript(&jv), Value::Nil)),
                            Err(e) => Ok(err_pair(format!("response.json failed: {}", e))),
                        },
                        Err(e) => Ok(err_pair(format!("response.json failed: {}", e))),
                    },
                    _ => unreachable!(),
                }
            }
            other => Err(AsError::at(format!("httpResponse has no method '{}'", other), span).into()),
        }
    }
}

/// reqwest's HTTP `Version` → the spec's short string ("1.1" | "2" | "3" | ...).
fn http_version_str(v: reqwest::Version) -> &'static str {
    match v {
        reqwest::Version::HTTP_09 => "0.9",
        reqwest::Version::HTTP_10 => "1.0",
        reqwest::Version::HTTP_11 => "1.1",
        reqwest::Version::HTTP_2 => "2",
        reqwest::Version::HTTP_3 => "3",
        _ => "1.1",
    }
}

/// Parse the `name=value` prefix of a single `Set-Cookie` header (attributes after
/// the first `;` are ignored — a deliberately simple name→value model).
fn parse_set_cookie(header: &str) -> Option<(String, String)> {
    let first = header.split(';').next()?.trim();
    let (name, value) = first.split_once('=')?;
    Some((name.trim().to_string(), value.trim().to_string()))
}

/// Build a `reqwest::multipart::Form` from a `{multipart:[...]}` array. Each entry is
/// `{name, value}` (a text field) or `{name, data, filename?, contentType?}` (a file
/// / bytes part, where `data` is a string or bytes).
fn build_multipart(mp: &Value, span: Span) -> Result<reqwest::multipart::Form, Control> {
    let arr = super::want_array(mp, span, "net/http body.multipart")?;
    let mut form = reqwest::multipart::Form::new();
    for entry in arr.borrow().iter() {
        let o = match entry {
            Value::Object(o) => o,
            other => {
                return Err(AsError::at(
                    format!("net/http multipart part must be an object, got {}", crate::interp::type_name(other)),
                    span,
                )
                .into())
            }
        };
        let o = o.borrow();
        let name = match o.get("name") {
            Some(n) => want_string(n, span, "net/http multipart part.name")?.to_string(),
            None => return Err(AsError::at("net/http multipart part requires a name", span).into()),
        };
        if let Some(data) = o.get("data") {
            let bytes = match data {
                Value::Str(s) => s.as_bytes().to_vec(),
                Value::Bytes(b) => b.borrow().clone(),
                other => {
                    return Err(AsError::at(
                        format!("net/http multipart data must be string/bytes, got {}", crate::interp::type_name(other)),
                        span,
                    )
                    .into())
                }
            };
            let mut part = reqwest::multipart::Part::bytes(bytes);
            if let Some(fname) = o.get("filename") {
                let fname = want_string(fname, span, "net/http multipart part.filename")?;
                part = part.file_name(fname.to_string());
            }
            if let Some(ct) = o.get("contentType") {
                let ct = want_string(ct, span, "net/http multipart part.contentType")?;
                part = part
                    .mime_str(&ct)
                    .map_err(|e| Control::from(AsError::at(format!("net/http multipart contentType: {}", e), span)))?;
            }
            form = form.part(name, part);
        } else if let Some(value) = o.get("value") {
            let value = scalar_to_string(value, span, "net/http multipart part.value")?;
            form = form.text(name, value);
        } else {
            return Err(AsError::at("net/http multipart part requires `value` or `data`", span).into());
        }
    }
    Ok(form)
}

#[cfg(test)]
mod tests {
    use crate::interp::Interp;

    /// Run an AScript program and return its captured output.
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    /// Run on a caller-held interp (so resource state can be inspected after).
    async fn run_on(interp: &mut Interp, src: &str) -> Result<(), crate::interp::Control> {
        let tokens = crate::lexer::lex(src).expect("lex");
        let program = crate::parser::parse(&tokens).expect("parse");
        let env = crate::interp::global_env().child();
        interp.exec(&program, &env).await.map(|_| ())
    }

    // ---- in-process HTTP/1 test fixture (hyper 1.x) -------------------------
    //
    // Starts a hyper HTTP/1 server on 127.0.0.1:0 in a spawned tokio task, returns
    // the base URL `http://127.0.0.1:{port}`. Dispatches on path:
    //   /text          → 200 "hello"
    //   /json          → 200 {"x":1,"items":[1,2,3]} (application/json)
    //   /echo          → 200 JSON {method, headers:{...}, body:"..."} reflecting the request
    //   /status/404    → 404
    //   /redirect      → 302 Location: /text
    // Reused by Tasks 3-5.
    mod fixture {
        use http_body_util::{BodyExt, Full};
        use hyper::body::{Bytes, Incoming};
        use hyper::service::service_fn;
        use hyper::{Request, Response, StatusCode};
        use hyper_util::rt::TokioIo;
        use std::convert::Infallible;
        use tokio::net::TcpListener;

        async fn handle(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
            let method = req.method().to_string();
            let path = req.uri().path().to_string();
            // Collect headers before consuming the body.
            let mut headers = serde_json::Map::new();
            for (name, value) in req.headers().iter() {
                headers.insert(
                    name.as_str().to_ascii_lowercase(),
                    serde_json::Value::String(value.to_str().unwrap_or("").to_string()),
                );
            }
            let body_bytes = req.into_body().collect().await.map(|c| c.to_bytes()).unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            let resp = match path.as_str() {
                "/text" => Response::new(Full::new(Bytes::from_static(b"hello"))),
                "/json" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(b"{\"x\":1,\"items\":[1,2,3]}")));
                    r.headers_mut()
                        .insert(hyper::header::CONTENT_TYPE, "application/json".parse().unwrap());
                    r
                }
                "/echo" => {
                    let echo = serde_json::json!({
                        "method": method,
                        "headers": serde_json::Value::Object(headers),
                        "body": body_str,
                    });
                    let mut r = Response::new(Full::new(Bytes::from(echo.to_string())));
                    r.headers_mut()
                        .insert(hyper::header::CONTENT_TYPE, "application/json".parse().unwrap());
                    r
                }
                "/status/404" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(b"not found")));
                    *r.status_mut() = StatusCode::NOT_FOUND;
                    r
                }
                "/redirect" => {
                    let mut r = Response::new(Full::new(Bytes::new()));
                    *r.status_mut() = StatusCode::FOUND;
                    r.headers_mut().insert(hyper::header::LOCATION, "/text".parse().unwrap());
                    r
                }
                _ => {
                    let mut r = Response::new(Full::new(Bytes::from_static(b"nope")));
                    *r.status_mut() = StatusCode::NOT_FOUND;
                    r
                }
            };
            Ok(resp)
        }

        /// Start the fixture; returns `http://127.0.0.1:{port}`.
        pub async fn start() -> String {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                loop {
                    let (stream, _) = match listener.accept().await {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let io = TokioIo::new(stream);
                    tokio::spawn(async move {
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, service_fn(handle))
                            .await;
                    });
                }
            });
            format!("http://127.0.0.1:{}", addr.port())
        }
    }

    #[tokio::test]
    async fn get_text_ok_status_and_body() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/text")
print(err)
print(resp.ok)
print(resp.status)
let [body, berr] = await resp.text()
print(berr)
print(body)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\ntrue\n200\nnil\nhello\n");
    }

    #[tokio::test]
    async fn get_json_parses_to_object() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/json")
let [data, jerr] = await resp.json()
print(jerr)
print(data.x)
print(data.items[2])
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\n1\n3\n");
    }

    #[tokio::test]
    async fn non_2xx_is_not_an_error() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/status/404")
print(err)
print(resp.ok)
print(resp.status)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\nfalse\n404\n");
    }

    #[tokio::test]
    async fn post_json_body_reflected_with_content_type() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ post }} from "std/net/http"
let [resp, _e] = await post("{base}/echo", {{ body: {{ json: {{ a: 1 }} }} }})
let [data, _je] = await resp.json()
print(data.method)
print(data.body)
print(data.headers["content-type"])
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "POST\n{\"a\":1}\napplication/json\n");
    }

    #[tokio::test]
    async fn post_form_body_urlencoded() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ post }} from "std/net/http"
let [resp, _e] = await post("{base}/echo", {{ body: {{ form: {{ k: "v" }} }} }})
let [data, _je] = await resp.json()
print(data.body)
print(data.headers["content-type"])
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "k=v\napplication/x-www-form-urlencoded\n");
    }

    #[tokio::test]
    async fn headers_and_bearer_auth_reflected() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/echo", {{ headers: {{ "x-test": "yes" }}, auth: {{ bearer: "tok" }} }})
let [data, _je] = await resp.json()
print(data.headers["x-test"])
print(data.headers["authorization"])
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "yes\nBearer tok\n");
    }

    #[tokio::test]
    async fn query_object_merged_into_url() {
        let base = fixture::start().await;
        // /echo reflects the request; assert the final URL carried the query string.
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
import {{ find }} from "std/string"
let [resp, _e] = await get("{base}/echo", {{ query: {{ a: "1", b: "two" }} }})
print(find(resp.url, "a=1") >= 0)
print(find(resp.url, "b=two") >= 0)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "true\ntrue\n");
    }

    #[tokio::test]
    async fn connect_failure_is_tier1_err_no_panic() {
        // Port 1 has nothing listening → a connect error, surfaced as a Tier-1 err.
        let out = run(
            r#"
import { get } from "std/net/http"
let [resp, err] = await get("http://127.0.0.1:1/")
print(resp)
print(err != nil)
"#,
        )
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn double_body_consume_is_tier2_panic() {
        let base = fixture::start().await;
        let mut interp = Interp::new();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/text")
let [_t, _te] = await resp.text()
let [_b, _be] = await resp.bytes()
"#
        );
        let res = run_on(&mut interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(msg.contains("already consumed"), "got: {}", msg);
            }
            other => panic!("expected a Tier-2 panic, got ok={:?}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn interp_e2e_get_json_destructured() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
fn fetch() {{
  let [resp, err] = await get("{base}/json")
  if (err != nil) {{ return -1 }}
  let [data, jerr] = await resp.json()
  if (jerr != nil) {{ return -2 }}
  return data.x + data.items[0] + data.items[2]
}}
print(fetch())
"#
        );
        let out = run(&src).await;
        // x=1, items[0]=1, items[2]=3 → 5
        assert_eq!(out, "5\n");
    }
}
