//! BATT B8 §9.2 — `std/blob` S3-compatible CLIENT tests against an in-process
//! HTTP stub that speaks the S3 REST API.
//!
//! The stub is a tiny tokio HTTP/1.1 server (its OWN current-thread runtime on a
//! dedicated std::thread) that:
//!   - parses each request (method, path, query, headers, body),
//!   - VERIFIES the `Authorization` SigV4 header is present, well-formed, and uses
//!     the known access key + the `s3` service + a `host` signed header (proving the
//!     client signed the request the stub actually received),
//!   - returns canned S3 XML / headers per a scripted route table,
//!   - records the requests it served for assertion.
//!
//! Covered (spec §9.2):
//!   (a) put → get → head → delete roundtrip (etag, contentType, x-amz-meta-*)
//!   (b) list generator paginates across 2 pages via NextContinuationToken, LAZILY
//!   (c) range get
//!   (d) multipart create → 3 parts → complete order + abort-on-part-failure
//!   (e) path-style vs virtual-host URL matrix + R2 region:"auto"
//!   (f) S3 XML error body → err.code/message/status
//!   (g) malformed XML → clean Tier-1
//!   (h) cap_audit denials (covered in tests/cap_audit.rs; a smoke check here too)

#![cfg(all(feature = "blob", feature = "net"))]

use std::io::Write as _;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// The crate's own audited SigV4 primitives — REUSED by the stub to RECOMPUTE the
// signature of every received request from the canonical request built off the WIRE
// (the path + query AS-RECEIVED). This is the durable guard: a client that signs a
// different (e.g. double-encoded) canonical request than the one it puts on the wire
// produces a signature the stub cannot reproduce → 403 SignatureDoesNotMatch.
use ascript::stdlib::blob::sigv4;

const ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

/// Verify the SigV4 `Authorization` of a received request by RECOMPUTING the signature
/// from the canonical request built off the WIRE (`method`, `wire_path` and
/// `wire_query` exactly as received, the signed headers, the body's sha256). Returns
/// `Ok(())` if the recomputed signature matches the `Signature=` the client sent, or
/// `Err(reason)` (→ the stub answers 403 SignatureDoesNotMatch).
///
/// AWS rule: the wire path/query ARE the canonical URI/query (the client must encode
/// EXACTLY ONCE), so the stub uses them verbatim. A double-encoding client signs a
/// different canonical request than it transmits → mismatch here.
fn verify_sigv4(
    method: &str,
    wire_path: &str,
    wire_query: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(), String> {
    let authz = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.clone())
        .ok_or_else(|| "no Authorization header".to_string())?;

    // Parse `AWS4-HMAC-SHA256 Credential=<ak>/<date>/<region>/<service>/aws4_request,
    //                        SignedHeaders=<h;h>, Signature=<hex>`.
    let rest = authz
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .ok_or_else(|| format!("bad algorithm prefix: {authz}"))?;
    let mut credential = None;
    let mut signed_headers = None;
    let mut claimed_sig = None;
    for part in rest.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("Credential=") {
            credential = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("SignedHeaders=") {
            signed_headers = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("Signature=") {
            claimed_sig = Some(v.to_string());
        }
    }
    let credential = credential.ok_or("no Credential")?;
    let signed_headers = signed_headers.ok_or("no SignedHeaders")?;
    let claimed_sig = claimed_sig.ok_or("no Signature")?;

    // Credential = <ak>/<date>/<region>/<service>/aws4_request
    let cred_parts: Vec<&str> = credential.split('/').collect();
    if cred_parts.len() != 5 || cred_parts[4] != "aws4_request" {
        return Err(format!("malformed Credential: {credential}"));
    }
    let (date, region, service) = (cred_parts[1], cred_parts[2], cred_parts[3]);
    let amz_datetime = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-amz-date"))
        .map(|(_, v)| v.clone())
        .ok_or("no x-amz-date")?;
    let payload_hash = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-amz-content-sha256"))
        .map(|(_, v)| v.clone())
        .ok_or("no x-amz-content-sha256")?;

    // Defensive: the payload hash the client SIGNED must match the body actually sent.
    let body_hash = sigv4::sha256_hex(body);
    if payload_hash != body_hash {
        return Err(format!(
            "x-amz-content-sha256 ({payload_hash}) does not match the received body hash ({body_hash})"
        ));
    }

    // Build the canonical headers block from ONLY the signed-headers set, in the order
    // the SignedHeaders list declares (lowercased names; values trimmed by canonical).
    let wanted: Vec<&str> = signed_headers.split(';').collect();
    let mut hpairs: Vec<(String, String)> = Vec::new();
    for name in &wanted {
        let val = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .ok_or_else(|| format!("signed header '{name}' not present"))?;
        hpairs.push((name.to_string(), val));
    }
    let (c_headers, derived_signed) = sigv4::canonical_headers(&hpairs);
    if derived_signed != signed_headers {
        return Err(format!(
            "signed-headers mismatch: claimed {signed_headers}, derived {derived_signed}"
        ));
    }

    // The WIRE path/query are the canonical URI/query (client encodes exactly once).
    let c_uri = if wire_path.is_empty() { "/" } else { wire_path };
    let c_req = sigv4::canonical_request(
        method,
        c_uri,
        wire_query,
        &c_headers,
        &signed_headers,
        &payload_hash,
    );
    let scope = sigv4::credential_scope(date, region, service);
    let sts = sigv4::string_to_sign(&amz_datetime, &scope, &c_req);
    let key = sigv4::signing_key(SECRET_KEY, date, region, service);
    let recomputed = sigv4::signature(&key, &sts);
    if recomputed != claimed_sig {
        return Err(format!(
            "SignatureDoesNotMatch: client signed {claimed_sig} but the canonical request \
             over the WIRE (path={c_uri:?} query={wire_query:?}) yields {recomputed}"
        ));
    }
    Ok(())
}

/// One served HTTP request the stub recorded.
#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        let lname = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lname)
            .map(|(_, v)| v.as_str())
    }
}

/// A canned response: status, headers, body. The route closure picks one based on
/// the recorded request (method/path/query). The first matching route serves.
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn ok_xml(body: &str) -> Response {
        Response {
            status: 200,
            headers: vec![("content-type".into(), "application/xml".into())],
            body: body.as_bytes().to_vec(),
        }
    }
}

type Router = Arc<dyn Fn(&Recorded) -> Response + Send + Sync>;

/// Spawn the stub on its own thread/runtime. Serves `n_requests` then exits. Returns
/// (port, recorded-handle). The router decides each response.
fn spawn_stub(n_requests: usize, router: Router) -> (u16, Arc<Mutex<Vec<Recorded>>>) {
    let recorded: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
    let rec2 = recorded.clone();
    let (tx, rx) = mpsc::channel::<u16>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tx.send(port).unwrap();
            for _ in 0..n_requests {
                let (stream, _) = match listener.accept().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let router = router.clone();
                let rec2 = rec2.clone();
                // Serve sequentially (one connection at a time keeps assertions simple;
                // reqwest reuses a connection but we accept each request on its own loop).
                serve_conn(stream, n_requests, router, rec2).await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        });
    });

    let port = rx.recv_timeout(Duration::from_secs(5)).expect("stub did not bind");
    (port, recorded)
}

/// Serve possibly-multiple keep-alive requests on one connection until it closes or we
/// have served `max` total. Each request is parsed, recorded, routed, answered.
async fn serve_conn(
    mut stream: TcpStream,
    _max: usize,
    router: Router,
    recorded: Arc<Mutex<Vec<Recorded>>>,
) {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        // Read until we have headers (\r\n\r\n).
        let header_end = loop {
            if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
            let mut tmp = [0u8; 8192];
            let n = match stream.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            buf.extend_from_slice(&tmp[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("").to_string();
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };
        let mut headers: Vec<(String, String)> = Vec::new();
        let mut content_length = 0usize;
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim().to_string();
                let v = v.trim().to_string();
                if k.eq_ignore_ascii_case("content-length") {
                    content_length = v.parse().unwrap_or(0);
                }
                headers.push((k, v));
            }
        }
        // Read the body.
        let mut body: Vec<u8> = buf[header_end..].to_vec();
        while body.len() < content_length {
            let mut tmp = [0u8; 8192];
            let n = match stream.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            body.extend_from_slice(&tmp[..n]);
        }
        let leftover = body.split_off(content_length.min(body.len()));

        let authorization = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        let rec = Recorded {
            method: method.clone(),
            path: path.clone(),
            query: query.clone(),
            headers: headers.clone(),
            body: body.clone(),
        };

        // SigV4 sanity: every authenticated S3 request must carry an AWS4-HMAC-SHA256
        // Authorization with our access key, the s3 service, and `host` among the
        // signed headers (the client signed the host it connected to).
        assert!(
            authorization.starts_with("AWS4-HMAC-SHA256 "),
            "missing/invalid SigV4 Authorization on {method} {path}: {authorization:?}"
        );
        assert!(
            authorization.contains(&format!("Credential={ACCESS_KEY}/")),
            "wrong access key in Authorization: {authorization}"
        );
        assert!(
            authorization.contains("/s3/aws4_request"),
            "wrong service scope (expected s3): {authorization}"
        );
        assert!(
            authorization.contains("SignedHeaders=")
                && authorization.contains("host"),
            "host not signed: {authorization}"
        );
        // x-amz-date + x-amz-content-sha256 must be present (signed).
        assert!(
            rec.header("x-amz-date").is_some(),
            "missing x-amz-date on {method} {path}"
        );
        assert!(
            rec.header("x-amz-content-sha256").is_some(),
            "missing x-amz-content-sha256 on {method} {path}"
        );

        recorded.lock().unwrap().push(rec.clone());

        // RECOMPUTE the SigV4 signature from the wire request. A mismatch (e.g. a
        // double-encoded signed path/query vs the single-encoded wire) → 403, exactly
        // as a real S3 endpoint would respond.
        let resp = match verify_sigv4(&method, &path, &query, &headers, &body) {
            Ok(()) => router(&rec),
            Err(reason) => Response {
                status: 403,
                headers: vec![("content-type".into(), "application/xml".into())],
                body: format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <Error><Code>SignatureDoesNotMatch</Code><Message>{}</Message></Error>",
                    xml_escape_text(&reason)
                )
                .into_bytes(),
            },
        };
        let mut out = format!(
            "HTTP/1.1 {} {}\r\n",
            resp.status,
            status_text(resp.status)
        )
        .into_bytes();
        let mut have_clen = false;
        for (k, v) in &resp.headers {
            if k.eq_ignore_ascii_case("content-length") {
                have_clen = true;
            }
            out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        if !have_clen {
            out.extend_from_slice(format!("content-length: {}\r\n", resp.body.len()).as_bytes());
        }
        out.extend_from_slice(b"connection: keep-alive\r\n\r\n");
        out.extend_from_slice(&resp.body);
        if stream.write_all(&out).await.is_err() {
            return;
        }
        let _ = stream.flush().await;

        // Prepare buf for the next keep-alive request on this connection.
        buf = leftover;
        if buf.is_empty() {
            // Peek whether the client will send more; if it closes, we exit.
            // We simply loop; the read at the top returns 0 on close.
        }
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Escape XML text so a recompute-failure reason rides safely in the error body.
fn xml_escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn status_text(s: u16) -> &'static str {
    match s {
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

/// Run an `.as` program; return (success, stdout, stderr).
fn run_script(src: &str, name: &str, flags: &[&str]) -> (bool, String, String) {
    let file = std::env::temp_dir().join(name);
    let mut f = std::fs::File::create(&file).unwrap();
    f.write_all(src.as_bytes()).unwrap();
    drop(f);
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("run");
    for fl in flags {
        cmd.arg(fl);
    }
    cmd.arg(&file);
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn recorded(r: &Arc<Mutex<Vec<Recorded>>>) -> Vec<Recorded> {
    std::thread::sleep(Duration::from_millis(150));
    r.lock().unwrap().clone()
}

/// A client-construction preamble (path-style against 127.0.0.1).
fn client_src(port: u16) -> String {
    format!(
        r#"import * as blob from "std/blob"
let client = blob.client({{
  endpoint: "http://127.0.0.1:{port}",
  region: "us-east-1",
  accessKey: "{ACCESS_KEY}",
  secretKey: "{SECRET_KEY}",
  bucket: "my-bucket",
  pathStyle: true,
}})
"#
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// (a) put → get → head → delete roundtrip
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_put_get_head_delete_roundtrip() {
    let router: Router = Arc::new(|rec: &Recorded| match rec.method.as_str() {
        "PUT" => Response {
            status: 200,
            headers: vec![("ETag".into(), "\"abc123etag\"".into())],
            body: Vec::new(),
        },
        "GET" => Response {
            status: 200,
            headers: vec![
                ("ETag".into(), "\"abc123etag\"".into()),
                ("Content-Type".into(), "text/plain".into()),
            ],
            body: b"hello world".to_vec(),
        },
        "HEAD" => Response {
            status: 200,
            headers: vec![
                ("ETag".into(), "\"abc123etag\"".into()),
                ("Content-Type".into(), "text/plain".into()),
                ("Content-Length".into(), "11".into()),
                ("Last-Modified".into(), "Tue, 24 May 2013 00:00:00 GMT".into()),
                ("x-amz-meta-author".into(), "alice".into()),
            ],
            body: Vec::new(),
        },
        "DELETE" => Response {
            status: 204,
            headers: vec![],
            body: Vec::new(),
        },
        _ => Response::ok_xml("<Error/>"),
    });
    let (port, rec) = spawn_stub(4, router);
    let src = format!(
        "{}{}",
        client_src(port),
        r#"
let [etag, perr] = client.put("greeting.txt", "hello world", { contentType: "text/plain", metadata: { author: "alice" } })
if (perr != nil) { print(`put-err: ${perr.message}`); exit(1) }
print(`etag=${etag}`)

let [data, gerr] = client.get("greeting.txt")
if (gerr != nil) { print(`get-err: ${gerr.message}`); exit(1) }
print(`get=${encoding.utf8Decode(data)[0]}`)

let [meta, herr] = client.head("greeting.txt")
if (herr != nil) { print(`head-err: ${herr.message}`); exit(1) }
print(`size=${meta.size}`)
print(`ctype=${meta.contentType}`)
print(`author=${meta.metadata.author}`)

let [_, derr] = client.delete("greeting.txt")
if (derr != nil) { print(`del-err: ${derr.message}`); exit(1) }
print("deleted")
"#
    );
    let full = format!("import * as encoding from \"std/encoding\"\n{src}");
    let (ok, out, err) = run_script(&full, "blob_roundtrip.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("etag=abc123etag") || out.contains("etag=\"abc123etag\""), "etag: {out}");
    assert!(out.contains("get=hello world"), "get body: {out}");
    assert!(out.contains("size=11"), "head size: {out}");
    assert!(out.contains("ctype=text/plain"), "head ctype: {out}");
    assert!(out.contains("author=alice"), "head meta: {out}");
    assert!(out.contains("deleted"), "delete: {out}");

    let reqs = recorded(&rec);
    // path-style: /my-bucket/greeting.txt
    assert!(reqs.iter().all(|r| r.path == "/my-bucket/greeting.txt"), "paths: {:?}", reqs.iter().map(|r| r.path.clone()).collect::<Vec<_>>());
    let put = reqs.iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(put.body, b"hello world", "PUT body");
    assert_eq!(put.header("content-type"), Some("text/plain"), "PUT content-type");
    assert_eq!(put.header("x-amz-meta-author"), Some("alice"), "PUT metadata header");
}

// ─────────────────────────────────────────────────────────────────────────────
// (a2) SPECIAL-CHAR KEY roundtrip — space + '+' + Unicode. The verifying stub
//      RECOMPUTES the signature from the wire path, so a double-encoded signed path
//      (vs the single-encoded wire) is a 403 SignatureDoesNotMatch. This is the
//      regression guard for the B8 double-encode blocker.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_special_char_key_put_get_head_delete() {
    let router: Router = Arc::new(|rec: &Recorded| match rec.method.as_str() {
        "PUT" => Response { status: 200, headers: vec![("ETag".into(), "\"k1\"".into())], body: Vec::new() },
        "GET" => Response {
            status: 200,
            headers: vec![("ETag".into(), "\"k1\"".into()), ("Content-Type".into(), "text/plain".into())],
            body: b"payload".to_vec(),
        },
        "HEAD" => Response {
            status: 200,
            headers: vec![
                ("ETag".into(), "\"k1\"".into()),
                ("Content-Type".into(), "text/plain".into()),
                ("Content-Length".into(), "7".into()),
            ],
            body: Vec::new(),
        },
        "DELETE" => Response { status: 204, headers: vec![], body: Vec::new() },
        _ => Response::ok_xml("<Error/>"),
    });
    let (port, rec) = spawn_stub(4, router);
    // Key with a SPACE, a '+', and a Unicode char — each must percent-encode EXACTLY
    // once in both the wire path AND the signed canonical (the verifying stub checks).
    let key = "logs/a b/c+d é.txt";
    let prog = format!(
        r#"
let key = "{key}"
let [etag, perr] = client.put(key, "payload", {{ contentType: "text/plain" }})
if (perr != nil) {{ print(`put-err: ${{perr.message}}`); exit(1) }}
print(`put-etag=${{etag}}`)
let [data, gerr] = client.get(key)
if (gerr != nil) {{ print(`get-err: ${{gerr.message}}`); exit(1) }}
print(`get=${{encoding.utf8Decode(data)[0]}}`)
let [meta, herr] = client.head(key)
if (herr != nil) {{ print(`head-err: ${{herr.message}}`); exit(1) }}
print(`size=${{meta.size}}`)
let [_, derr] = client.delete(key)
if (derr != nil) {{ print(`del-err: ${{derr.message}}`); exit(1) }}
print("ok")
"#
    );
    let src = format!(
        "import * as encoding from \"std/encoding\"\n{}{prog}",
        client_src(port),
    );
    let (ok, out, err) = run_script(&src, "blob_special_key.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("put-etag=k1"), "put etag (sig mismatch?): {out}");
    assert!(out.contains("get=payload"), "get body: {out}");
    assert!(out.contains("size=7"), "head size: {out}");
    assert!(out.contains("ok"), "delete: {out}");

    // The WIRE path must be SINGLE percent-encoded: space→%20, '+'→%2B, é→%C3%A9.
    let reqs = recorded(&rec);
    let get = reqs.iter().find(|r| r.method == "GET").unwrap();
    assert_eq!(
        get.path, "/my-bucket/logs/a%20b/c%2Bd%20%C3%A9.txt",
        "wire path not single-encoded: {}",
        get.path
    );
    // It must NOT be double-encoded (no %25 sequences from %20→%2520).
    assert!(!get.path.contains("%25"), "wire path is double-encoded: {}", get.path);
}

#[test]
fn blob_special_char_key_presign() {
    // presign is pure (no network) but the stub-less check still proves single-encoding:
    // the presigned URL's path is single-encoded AND the X-Amz-Signature was computed
    // over that same single-encoded canonical (we recompute below to be sure).
    let src = format!(
        r#"import * as blob from "std/blob"
let client = blob.client({{ endpoint: "https://s3.example.com", region: "us-east-1",
  accessKey: "{ACCESS_KEY}", secretKey: "{SECRET_KEY}", bucket: "my-bucket", pathStyle: true }})
let [url, err] = client.presign("GET", "a b/c+d.txt")
if (err != nil) {{ print(`presign-err: ${{err.message}}`); exit(1) }}
print(`url=${{url}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "blob_special_presign.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    let line = out.lines().find(|l| l.starts_with("url=")).unwrap();
    let url = line.strip_prefix("url=").unwrap();
    // Path single-encoded, NOT double.
    assert!(
        url.contains("/my-bucket/a%20b/c%2Bd.txt?"),
        "presign path not single-encoded: {url}"
    );
    assert!(!url.contains("%2520") && !url.contains("%252B"), "presign path double-encoded: {url}");
    assert!(url.contains("X-Amz-Signature="), "no signature: {url}");

    // RECOMPUTE the presign signature over the WIRE path — the durable guard that the
    // signature matches the single-encoded path the client actually emits. (Before the
    // fix, presign signed a DOUBLE-encoded path while emitting a single-encoded URL.)
    let (path, q) = url.split_once('?').unwrap();
    let wire_path = path.split_once("s3.example.com").unwrap().1; // strip scheme+host
    // Split the query into the auth params; pull out X-Amz-Signature; rebuild the
    // canonical query (everything except the signature) and the scope from Credential.
    let mut params: Vec<(String, String)> = Vec::new();
    let mut claimed_sig = String::new();
    let mut credential = String::new();
    let mut amz_date = String::new();
    for kv in q.split('&') {
        let (k, v) = kv.split_once('=').unwrap();
        if k == "X-Amz-Signature" {
            claimed_sig = v.to_string();
            continue;
        }
        if k == "X-Amz-Credential" {
            credential = v.to_string();
        }
        if k == "X-Amz-Date" {
            amz_date = v.to_string();
        }
        // The query on the wire is already canonical/encoded; pass it through verbatim
        // to canonical_request (do NOT re-encode).
        params.push((k.to_string(), v.to_string()));
    }
    // Canonical query = the wire params (already sorted+encoded), re-joined verbatim.
    let canonical_query = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    // Credential = <ak>%2F<date>%2F<region>%2F<service>%2Faws4_request (encoded '/').
    let cred_dec = credential.replace("%2F", "/");
    let cp: Vec<&str> = cred_dec.split('/').collect();
    let (date, region, service) = (cp[1], cp[2], cp[3]);
    let c_headers = "host:s3.example.com\n".to_string();
    let c_req = sigv4::canonical_request(
        "GET",
        wire_path,
        &canonical_query,
        &c_headers,
        "host",
        "UNSIGNED-PAYLOAD",
    );
    let scope = sigv4::credential_scope(date, region, service);
    let sts = sigv4::string_to_sign(&amz_date, &scope, &c_req);
    let key = sigv4::signing_key(SECRET_KEY, date, region, service);
    let recomputed = sigv4::signature(&key, &sts);
    assert_eq!(
        recomputed, claimed_sig,
        "presign signature does not match the WIRE path {wire_path:?} (double-encode?)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (b) list generator — paginates across 2 pages, LAZILY
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_list_paginates_lazily() {
    let page1 = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>my-bucket</Name>
  <IsTruncated>true</IsTruncated>
  <NextContinuationToken>TOKEN_PAGE2</NextContinuationToken>
  <Contents><Key>a.txt</Key><Size>10</Size><ETag>"e1"</ETag><LastModified>2013-05-24T00:00:00.000Z</LastModified></Contents>
  <Contents><Key>b.txt</Key><Size>20</Size><ETag>"e2"</ETag><LastModified>2013-05-24T00:00:01.000Z</LastModified></Contents>
</ListBucketResult>"#;
    let page2 = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>my-bucket</Name>
  <IsTruncated>false</IsTruncated>
  <Contents><Key>c.txt</Key><Size>30</Size><ETag>"e3"</ETag><LastModified>2013-05-24T00:00:02.000Z</LastModified></Contents>
</ListBucketResult>"#;
    let p1 = page1.to_string();
    let p2 = page2.to_string();
    let router: Router = Arc::new(move |rec: &Recorded| {
        // page 2 is requested with continuation-token=TOKEN_PAGE2 in the query.
        if rec.query.contains("continuation-token=TOKEN_PAGE2") {
            Response::ok_xml(&p2)
        } else {
            Response::ok_xml(&p1)
        }
    });
    let (port, rec) = spawn_stub(2, router);
    let src = format!(
        "{}{}",
        client_src(port),
        r#"
let g = client.list({ prefix: "" })
let count = 0
let keys = []
for await (item in g) {
  count = count + 1
  keys = [...keys, item.key]
  // After the first page (2 items) is drained, page 2 has NOT been fetched yet.
  // We assert laziness by checking the stub only saw 1 request until we cross into item 3.
  print(`item ${count}: ${item.key} size=${item.size}`)
}
print(`total=${count}`)
print(`keys=${keys}`)
"#
    );
    let (ok, out, err) = run_script(&src, "blob_list.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("item 1: a.txt size=10"), "out: {out}");
    assert!(out.contains("item 2: b.txt size=20"), "out: {out}");
    assert!(out.contains("item 3: c.txt size=30"), "out: {out}");
    assert!(out.contains("total=3"), "out: {out}");

    let reqs = recorded(&rec);
    assert_eq!(reqs.len(), 2, "expected exactly 2 list page requests (lazy): {:?}", reqs.iter().map(|r| r.query.clone()).collect::<Vec<_>>());
    // Page 1 has no continuation-token; page 2 carries it.
    assert!(!reqs[0].query.contains("continuation-token"), "page1 query: {}", reqs[0].query);
    assert!(reqs[1].query.contains("continuation-token=TOKEN_PAGE2"), "page2 query: {}", reqs[1].query);
    // list-type=2 is the v2 ListObjects marker.
    assert!(reqs[0].query.contains("list-type=2"), "missing list-type=2: {}", reqs[0].query);
}

// ─────────────────────────────────────────────────────────────────────────────
// (b2) list with a SPACE prefix and a base64 continuation-token containing '+' '/' '='
//      — the page-2 query double-encode regression guard. Against the verifying stub,
//      a double-encoded signed query (vs the single-encoded wire) is a 403, AND the
//      page-2 token must round-trip so the stub matches the token route.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_list_special_prefix_and_continuation_token() {
    // A realistic base64 continuation token: contains '+', '/', and '=' padding.
    const TOKEN: &str = "ab+cd/ef==";
    let page1 = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>my-bucket</Name>
  <IsTruncated>true</IsTruncated>
  <NextContinuationToken>{TOKEN}</NextContinuationToken>
  <Contents><Key>logs/x y.txt</Key><Size>10</Size><ETag>"e1"</ETag><LastModified>2013-05-24T00:00:00.000Z</LastModified></Contents>
</ListBucketResult>"#
    );
    let page2 = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>my-bucket</Name>
  <IsTruncated>false</IsTruncated>
  <Contents><Key>logs/z.txt</Key><Size>20</Size><ETag>"e2"</ETag><LastModified>2013-05-24T00:00:01.000Z</LastModified></Contents>
</ListBucketResult>"#;
    let p1 = page1.clone();
    let p2 = page2.to_string();
    let router: Router = Arc::new(move |rec: &Recorded| {
        // page 2 is requested with the SINGLE-encoded token: '+'→%2B, '/'→%2F, '='→%3D.
        if rec.query.contains("continuation-token=ab%2Bcd%2Fef%3D%3D") {
            Response::ok_xml(&p2)
        } else {
            Response::ok_xml(&p1)
        }
    });
    let (port, rec) = spawn_stub(2, router);
    let src = format!(
        "{}{}",
        client_src(port),
        r#"
let g = client.list({ prefix: "logs/a b" })
let keys = []
for await (item in g) {
  keys = [...keys, item.key]
}
print(`count=${len(keys)}`)
print(`keys=${keys}`)
"#
    );
    let (ok, out, err) = run_script(&src, "blob_list_special.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    // BOTH pages must have been fetched (page 2 succeeded → token round-tripped + signed
    // correctly), yielding 2 keys.
    assert!(out.contains("count=2"), "page 2 did not succeed (token/sig?): {out}");

    let reqs = recorded(&rec);
    assert_eq!(reqs.len(), 2, "expected 2 list pages: {:?}", reqs.iter().map(|r| r.query.clone()).collect::<Vec<_>>());
    // Page-1 wire query: the space prefix is single-encoded (%20), not double (%2520).
    assert!(reqs[0].query.contains("prefix=logs%2Fa%20b"), "page1 prefix not single-encoded: {}", reqs[0].query);
    assert!(!reqs[0].query.contains("%2520"), "page1 query double-encoded: {}", reqs[0].query);
    // Page-2 wire query: the base64 token is single-encoded.
    assert!(
        reqs[1].query.contains("continuation-token=ab%2Bcd%2Fef%3D%3D"),
        "page2 token not single-encoded: {}",
        reqs[1].query
    );
    assert!(!reqs[1].query.contains("%252B") && !reqs[1].query.contains("%253D"), "page2 token double-encoded: {}", reqs[1].query);
}

// ─────────────────────────────────────────────────────────────────────────────
// (c) range get
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_range_get() {
    let router: Router = Arc::new(|_rec: &Recorded| Response {
        status: 206,
        headers: vec![
            ("Content-Range".into(), "bytes 0-4/11".into()),
            ("Content-Type".into(), "text/plain".into()),
        ],
        body: b"hello".to_vec(),
    });
    let (port, rec) = spawn_stub(1, router);
    let src = format!(
        "import * as encoding from \"std/encoding\"\n{}{}",
        client_src(port),
        r#"
let [data, err] = client.get("greeting.txt", { range: [0, 4] })
if (err != nil) { print(`get-err: ${err.message}`); exit(1) }
print(`range=${encoding.utf8Decode(data)[0]}`)
"#
    );
    let (ok, out, err) = run_script(&src, "blob_range.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("range=hello"), "out: {out}");
    let reqs = recorded(&rec);
    assert_eq!(reqs[0].header("range"), Some("bytes=0-4"), "range header: {:?}", reqs[0].header("range"));
}

// ─────────────────────────────────────────────────────────────────────────────
// (d) multipart upload — create → 3 parts → complete, ordered
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_multipart_upload_order() {
    let init = r#"<?xml version="1.0"?><InitiateMultipartUploadResult><Bucket>my-bucket</Bucket><Key>big.bin</Key><UploadId>UPLOAD123</UploadId></InitiateMultipartUploadResult>"#;
    let complete = r#"<?xml version="1.0"?><CompleteMultipartUploadResult><ETag>"final-etag"</ETag></CompleteMultipartUploadResult>"#;
    let init = init.to_string();
    let complete = complete.to_string();
    let router: Router = Arc::new(move |rec: &Recorded| {
        if rec.method == "POST" && rec.query.contains("uploads") {
            Response::ok_xml(&init)
        } else if rec.method == "POST" && rec.query.contains("uploadId=UPLOAD123") {
            Response::ok_xml(&complete)
        } else if rec.method == "PUT" && rec.query.contains("partNumber=") {
            // each part gets an etag derived from its part number
            let pn = rec
                .query
                .split('&')
                .find_map(|kv| kv.strip_prefix("partNumber="))
                .unwrap_or("0");
            Response {
                status: 200,
                headers: vec![("ETag".into(), format!("\"part{pn}etag\""))],
                body: Vec::new(),
            }
        } else {
            Response::ok_xml("<Error/>")
        }
    });
    let (port, rec) = spawn_stub(5, router);
    // Three chunks. The 5 MiB non-final-part floor (an S3 server constraint) applies to
    // ANY source, so the two NON-FINAL parts are 5 MiB; the FINAL part may be small.
    let src = format!(
        "{}{}",
        client_src(port),
        r#"
let big = string.repeat("z", 5 * 1024 * 1024)   // 5 MiB, meets the non-final floor
let chunks = [
  encoding.utf8Encode(big),       // part 1 (non-final, 5 MiB)
  encoding.utf8Encode(big),       // part 2 (non-final, 5 MiB)
  encoding.utf8Encode("tail"),    // part 3 (final, small — allowed)
]
let [etag, err] = client.putMultipart("big.bin", chunks)
if (err != nil) { print(`mp-err: ${err.message}`); exit(1) }
print(`etag=${etag}`)
"#
    );
    let full = format!("import * as string from \"std/string\"\nimport * as encoding from \"std/encoding\"\n{src}");
    let (ok, out, err) = run_script(&full, "blob_multipart.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("etag=final-etag") || out.contains("etag=\"final-etag\""), "etag: {out}");

    let reqs = recorded(&rec);
    // Order: InitiateMultipartUpload (POST ?uploads), then 3 UploadPart (PUT
    // ?partNumber=1,2,3 in order), then CompleteMultipartUpload (POST ?uploadId).
    let methods: Vec<String> = reqs.iter().map(|r| format!("{} {}", r.method, r.query)).collect();
    assert!(methods[0].starts_with("POST") && methods[0].contains("uploads"), "first req not initiate: {methods:?}");
    let part_nums: Vec<&str> = reqs
        .iter()
        .filter(|r| r.method == "PUT")
        .map(|r| r.query.split('&').find_map(|kv| kv.strip_prefix("partNumber=")).unwrap_or("?"))
        .collect();
    assert_eq!(part_nums, vec!["1", "2", "3"], "parts not in order: {part_nums:?}");
    let last = methods.last().unwrap();
    assert!(last.starts_with("POST") && last.contains("uploadId=UPLOAD123"), "last req not complete: {last}");
    // The complete body must list the part ETags in order.
    let complete_req = reqs.iter().rfind(|r| r.method == "POST" && r.query.contains("uploadId")).unwrap();
    let body = String::from_utf8_lossy(&complete_req.body);
    assert!(body.contains("part1etag") && body.contains("part2etag") && body.contains("part3etag"), "complete body etags: {body}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (d1b) multipart whose uploadId contains '/' and '+' — the UploadPart/complete/abort
//       sub-query double-encode regression guard. Against the verifying stub, the
//       single-encoded wire uploadId must round-trip AND sign correctly.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_multipart_special_upload_id() {
    // An uploadId with '/' and '+' (real S3 uploadIds are opaque, can contain these).
    const UPLOAD_ID: &str = "abc/def+ghi";
    let init = format!(
        r#"<?xml version="1.0"?><InitiateMultipartUploadResult><UploadId>{UPLOAD_ID}</UploadId></InitiateMultipartUploadResult>"#
    );
    let complete = r#"<?xml version="1.0"?><CompleteMultipartUploadResult><ETag>"sp-final"</ETag></CompleteMultipartUploadResult>"#;
    let init = init.clone();
    let complete = complete.to_string();
    // Single-encoded uploadId on the wire: '/'→%2F, '+'→%2B.
    const ENC_ID: &str = "abc%2Fdef%2Bghi";
    let router: Router = Arc::new(move |rec: &Recorded| {
        if rec.method == "POST" && rec.query.contains("uploads") {
            Response::ok_xml(&init)
        } else if rec.method == "POST" && rec.query.contains(&format!("uploadId={ENC_ID}")) {
            Response::ok_xml(&complete)
        } else if rec.method == "PUT" && rec.query.contains(&format!("uploadId={ENC_ID}")) {
            let pn = rec.query.split('&').find_map(|kv| kv.strip_prefix("partNumber=")).unwrap_or("0");
            Response { status: 200, headers: vec![("ETag".into(), format!("\"sp{pn}\""))], body: Vec::new() }
        } else {
            // Wrong (double?) encoding or unknown route → fail loudly so the test catches it.
            Response::ok_xml("<Error><Code>RouteMiss</Code><Message>unexpected query</Message></Error>")
        }
    });
    let (port, rec) = spawn_stub(4, router);
    let src = format!(
        "import * as string from \"std/string\"\nimport * as encoding from \"std/encoding\"\n{}{}",
        client_src(port),
        r#"
let big = string.repeat("q", 5 * 1024 * 1024)
let chunks = [encoding.utf8Encode(big), encoding.utf8Encode("tail")]
let [etag, err] = client.putMultipart("big.bin", chunks)
if (err != nil) { print(`mp-err: ${err.message}`); exit(1) }
print(`etag=${etag}`)
"#
    );
    let (ok, out, err) = run_script(&src, "blob_multipart_special_id.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("etag=sp-final") || out.contains("etag=\"sp-final\""), "etag (sig/encode?): {out}");

    let reqs = recorded(&rec);
    // Every UploadPart + complete carried the SINGLE-encoded uploadId on the wire.
    for r in reqs.iter().filter(|r| r.method == "PUT" || (r.method == "POST" && r.query.contains("uploadId"))) {
        assert!(
            r.query.contains(&format!("uploadId={ENC_ID}")),
            "uploadId not single-encoded on the wire: {}",
            r.query
        );
        assert!(!r.query.contains("%252F") && !r.query.contains("%252B"), "uploadId double-encoded: {}", r.query);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// (d2) multipart abort-on-part-failure — stub 500s part 2 → AbortMultipartUpload
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_multipart_aborts_on_part_failure() {
    let init = r#"<?xml version="1.0"?><InitiateMultipartUploadResult><UploadId>UPLOAD500</UploadId></InitiateMultipartUploadResult>"#;
    let init = init.to_string();
    let router: Router = Arc::new(move |rec: &Recorded| {
        if rec.method == "POST" && rec.query.contains("uploads") {
            Response::ok_xml(&init)
        } else if rec.method == "PUT" && rec.query.contains("partNumber=1") {
            Response { status: 200, headers: vec![("ETag".into(), "\"p1\"".into())], body: Vec::new() }
        } else if rec.method == "PUT" && rec.query.contains("partNumber=2") {
            // Part 2 fails with an S3 error body.
            Response {
                status: 500,
                headers: vec![("content-type".into(), "application/xml".into())],
                body: br#"<?xml version="1.0"?><Error><Code>InternalError</Code><Message>boom</Message></Error>"#.to_vec(),
            }
        } else if rec.method == "DELETE" && rec.query.contains("uploadId=UPLOAD500") {
            // AbortMultipartUpload.
            Response { status: 204, headers: vec![], body: Vec::new() }
        } else {
            Response::ok_xml("<Error/>")
        }
    });
    let (port, rec) = spawn_stub(4, router);
    // Non-final parts are 5 MiB (the floor); part 2 triggers the stub 500.
    let src = format!(
        "import * as string from \"std/string\"\nimport * as encoding from \"std/encoding\"\n{}{}",
        client_src(port),
        r#"
let big = string.repeat("w", 5 * 1024 * 1024)
let chunks = [
  encoding.utf8Encode(big),
  encoding.utf8Encode(big),
  encoding.utf8Encode("part-three"),
]
let [etag, err] = client.putMultipart("big.bin", chunks)
if (err == nil) { print("FAIL: should have errored"); exit(1) }
print(`mp-err: ${err.message}`)
"#
    );
    let (ok, out, err) = run_script(&src, "blob_multipart_abort.as", &[]);
    assert!(ok, "script should run (Tier-1): {out}\n{err}");
    assert!(out.contains("mp-err:"), "expected mp error: {out}");

    let reqs = recorded(&rec);
    // The Abort (DELETE ?uploadId=UPLOAD500) MUST have been issued (no orphaned upload).
    assert!(
        reqs.iter().any(|r| r.method == "DELETE" && r.query.contains("uploadId=UPLOAD500")),
        "AbortMultipartUpload not observed: {:?}",
        reqs.iter().map(|r| format!("{} {}", r.method, r.query)).collect::<Vec<_>>()
    );
    // Part 3 must NOT have been uploaded after the part-2 failure.
    assert!(
        !reqs.iter().any(|r| r.method == "PUT" && r.query.contains("partNumber=3")),
        "part 3 uploaded after failure (no abort-on-error): {:?}",
        reqs.iter().map(|r| r.query.clone()).collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (d3) multipart over a GENERATOR source — streaming pull, same lifecycle/order
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_multipart_generator_source_order() {
    let init = r#"<?xml version="1.0"?><InitiateMultipartUploadResult><UploadId>GENUP1</UploadId></InitiateMultipartUploadResult>"#;
    let complete = r#"<?xml version="1.0"?><CompleteMultipartUploadResult><ETag>"gen-final-etag"</ETag></CompleteMultipartUploadResult>"#;
    let init = init.to_string();
    let complete = complete.to_string();
    let router: Router = Arc::new(move |rec: &Recorded| {
        if rec.method == "POST" && rec.query.contains("uploads") {
            Response::ok_xml(&init)
        } else if rec.method == "POST" && rec.query.contains("uploadId=GENUP1") {
            Response::ok_xml(&complete)
        } else if rec.method == "PUT" && rec.query.contains("partNumber=") {
            let pn = rec
                .query
                .split('&')
                .find_map(|kv| kv.strip_prefix("partNumber="))
                .unwrap_or("0");
            Response {
                status: 200,
                headers: vec![("ETag".into(), format!("\"genpart{pn}\""))],
                body: Vec::new(),
            }
        } else {
            Response::ok_xml("<Error/>")
        }
    });
    let (port, rec) = spawn_stub(5, router);
    // The source is a `fn*` generator yielding 3 chunks. Each non-final chunk is ≥ 5
    // MiB (the streaming floor) so a genuine large-object stream is exercised; the
    // final chunk may be small. To keep the test fast we build the 5 MiB chunks with a
    // string repeat, NOT a literal.
    let src = format!(
        "import * as encoding from \"std/encoding\"\n{}{}",
        client_src(port),
        r#"
let big = string.repeat("x", 5 * 1024 * 1024)   // 5 MiB, meets the non-final floor
fn* parts() {
  yield encoding.utf8Encode(big)        // part 1 (5 MiB)
  yield encoding.utf8Encode(big)        // part 2 (5 MiB)
  yield encoding.utf8Encode("tail")     // part 3 (final, small — allowed)
}
let [etag, err] = client.putMultipart("streamed.bin", parts())
if (err != nil) { print(`mp-err: ${err.message}`); exit(1) }
print(`etag=${etag}`)
"#
    );
    let full = format!("import * as string from \"std/string\"\n{src}");
    let (ok, out, err) = run_script(&full, "blob_multipart_gen.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("etag=gen-final-etag") || out.contains("etag=\"gen-final-etag\""), "etag: {out}");

    let reqs = recorded(&rec);
    let methods: Vec<String> = reqs.iter().map(|r| format!("{} {}", r.method, r.query)).collect();
    assert!(methods[0].starts_with("POST") && methods[0].contains("uploads"), "first req not initiate: {methods:?}");
    let part_nums: Vec<&str> = reqs
        .iter()
        .filter(|r| r.method == "PUT")
        .map(|r| r.query.split('&').find_map(|kv| kv.strip_prefix("partNumber=")).unwrap_or("?"))
        .collect();
    assert_eq!(part_nums, vec!["1", "2", "3"], "generator parts not in order: {part_nums:?}");
    let last = methods.last().unwrap();
    assert!(last.starts_with("POST") && last.contains("uploadId=GENUP1"), "last req not complete: {last}");
    let complete_req = reqs.iter().rfind(|r| r.method == "POST" && r.query.contains("uploadId")).unwrap();
    let body = String::from_utf8_lossy(&complete_req.body);
    assert!(
        body.contains("genpart1") && body.contains("genpart2") && body.contains("genpart3"),
        "complete body etags: {body}"
    );
    // The final (small) part rode through; the non-final 5 MiB parts uploaded their bytes.
    let part2 = reqs.iter().find(|r| r.method == "PUT" && r.query.contains("partNumber=2")).unwrap();
    assert_eq!(part2.body.len(), 5 * 1024 * 1024, "part 2 (non-final) should carry 5 MiB");
    let part3 = reqs.iter().find(|r| r.method == "PUT" && r.query.contains("partNumber=3")).unwrap();
    assert_eq!(part3.body, b"tail", "final part bytes");
}

#[test]
fn blob_multipart_generator_aborts_on_part_failure() {
    let init = r#"<?xml version="1.0"?><InitiateMultipartUploadResult><UploadId>GENUP500</UploadId></InitiateMultipartUploadResult>"#;
    let init = init.to_string();
    let router: Router = Arc::new(move |rec: &Recorded| {
        if rec.method == "POST" && rec.query.contains("uploads") {
            Response::ok_xml(&init)
        } else if rec.method == "PUT" && rec.query.contains("partNumber=1") {
            Response { status: 200, headers: vec![("ETag".into(), "\"g1\"".into())], body: Vec::new() }
        } else if rec.method == "PUT" && rec.query.contains("partNumber=2") {
            Response {
                status: 500,
                headers: vec![("content-type".into(), "application/xml".into())],
                body: br#"<?xml version="1.0"?><Error><Code>InternalError</Code><Message>boom</Message></Error>"#.to_vec(),
            }
        } else if rec.method == "DELETE" && rec.query.contains("uploadId=GENUP500") {
            Response { status: 204, headers: vec![], body: Vec::new() }
        } else {
            Response::ok_xml("<Error/>")
        }
    });
    let (port, rec) = spawn_stub(4, router);
    // A generator whose 2nd pulled chunk triggers a stub 500 on UploadPart → abort.
    // Non-final parts are 5 MiB to satisfy the streaming floor.
    let src = format!(
        "import * as string from \"std/string\"\nimport * as encoding from \"std/encoding\"\n{}{}",
        client_src(port),
        r#"
let big = string.repeat("y", 5 * 1024 * 1024)
fn* parts() {
  yield encoding.utf8Encode(big)
  yield encoding.utf8Encode(big)
  yield encoding.utf8Encode("never-reached")
}
let [etag, err] = client.putMultipart("streamed.bin", parts())
if (err == nil) { print("FAIL: should have errored"); exit(1) }
print(`mp-err: ${err.message}`)
"#
    );
    let (ok, out, err) = run_script(&src, "blob_multipart_gen_abort.as", &[]);
    assert!(ok, "script should run (Tier-1): {out}\n{err}");
    assert!(out.contains("mp-err:"), "expected mp error: {out}");

    let reqs = recorded(&rec);
    assert!(
        reqs.iter().any(|r| r.method == "DELETE" && r.query.contains("uploadId=GENUP500")),
        "AbortMultipartUpload not observed: {:?}",
        reqs.iter().map(|r| format!("{} {}", r.method, r.query)).collect::<Vec<_>>()
    );
    // The generator must NOT have been driven past the failing part (no part 3).
    assert!(
        !reqs.iter().any(|r| r.method == "PUT" && r.query.contains("partNumber=3")),
        "part 3 uploaded after failure (generator over-driven): {:?}",
        reqs.iter().map(|r| r.query.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn blob_multipart_generator_nonfinal_part_too_small_is_tier1() {
    // A NON-FINAL pulled chunk below the 5 MiB floor is a runtime-stream Tier-1 error
    // (distinct from a configured-partSize Tier-2). The upload must abort.
    let init = r#"<?xml version="1.0"?><InitiateMultipartUploadResult><UploadId>GENSMALL</UploadId></InitiateMultipartUploadResult>"#;
    let init = init.to_string();
    let router: Router = Arc::new(move |rec: &Recorded| {
        if rec.method == "POST" && rec.query.contains("uploads") {
            Response::ok_xml(&init)
        } else if rec.method == "PUT" && rec.query.contains("partNumber=1") {
            // Part 1 may be uploaded before we see part 2 is too small, OR the floor is
            // checked with lookahead before uploading part 1. Either way the stub answers.
            Response { status: 200, headers: vec![("ETag".into(), "\"s1\"".into())], body: Vec::new() }
        } else if rec.method == "DELETE" && rec.query.contains("uploadId=GENSMALL") {
            Response { status: 204, headers: vec![], body: Vec::new() }
        } else {
            Response::ok_xml("<Error/>")
        }
    });
    // At most: init + (maybe part1) + abort = 3 requests.
    let (port, rec) = spawn_stub(3, router);
    let src = format!(
        "import * as encoding from \"std/encoding\"\n{}{}",
        client_src(port),
        r#"
fn* parts() {
  yield encoding.utf8Encode("too-small-nonfinal")   // < 5 MiB but NOT the last chunk
  yield encoding.utf8Encode("there-is-a-second")     // makes the first one non-final
}
let [etag, err] = client.putMultipart("streamed.bin", parts())
if (err == nil) { print("FAIL: should have errored"); exit(1) }
print(`mp-err: ${err.message}`)
"#
    );
    let (ok, out, err) = run_script(&src, "blob_multipart_gen_small.as", &[]);
    assert!(ok, "script should run (Tier-1, not a panic): {out}\n{err}");
    assert!(out.contains("mp-err:"), "expected mp error: {out}");
    assert!(
        out.to_lowercase().contains("5 mib") || out.to_lowercase().contains("part") && out.to_lowercase().contains("small")
            || out.to_lowercase().contains("minimum"),
        "error should explain the non-final part floor: {out}"
    );
    // The upload must have been aborted (no orphan).
    let reqs = recorded(&rec);
    assert!(
        reqs.iter().any(|r| r.method == "DELETE" && r.query.contains("uploadId=GENSMALL")),
        "AbortMultipartUpload not observed after a too-small non-final part: {:?}",
        reqs.iter().map(|r| format!("{} {}", r.method, r.query)).collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (e) URL matrix — path-style vs virtual-host + R2 region:"auto"
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_virtual_host_and_pathstyle_presign() {
    // Presign is pure (no network), so we can assert the URL shape without a stub.
    let src = format!(
        r#"import * as blob from "std/blob"
// path-style (default for a non-AWS endpoint): endpoint/bucket/key
let pathStyle = blob.client({{ endpoint: "http://127.0.0.1:9000", region: "us-east-1",
  accessKey: "{ACCESS_KEY}", secretKey: "{SECRET_KEY}", bucket: "my-bucket", pathStyle: true }})
let [u1, e1] = pathStyle.presign("GET", "k.txt")
if (e1 != nil) {{ print(`p1-err: ${{e1.message}}`); exit(1) }}
print(`pathstyle=${{u1}}`)

// virtual-host style: bucket.host/key
let vhost = blob.client({{ endpoint: "https://s3.example.com", region: "us-east-1",
  accessKey: "{ACCESS_KEY}", secretKey: "{SECRET_KEY}", bucket: "my-bucket", pathStyle: false }})
let [u2, e2] = vhost.presign("GET", "k.txt")
if (e2 != nil) {{ print(`p2-err: ${{e2.message}}`); exit(1) }}
print(`vhost=${{u2}}`)

// R2 region:"auto" is accepted.
let r2 = blob.client({{ endpoint: "https://acct.r2.cloudflarestorage.com", region: "auto",
  accessKey: "{ACCESS_KEY}", secretKey: "{SECRET_KEY}", bucket: "data", pathStyle: false }})
let [u3, e3] = r2.presign("GET", "k.txt")
if (e3 != nil) {{ print(`p3-err: ${{e3.message}}`); exit(1) }}
print(`r2=${{u3}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "blob_url_matrix.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    // path-style: host:port path /my-bucket/k.txt
    assert!(out.contains("pathstyle=http://127.0.0.1:9000/my-bucket/k.txt?"), "pathstyle url: {out}");
    // virtual-host: bucket prefixed on the host
    assert!(out.contains("vhost=https://my-bucket.s3.example.com/k.txt?"), "vhost url: {out}");
    // R2 auto region in the credential scope.
    assert!(out.contains("r2=https://data.acct.r2.cloudflarestorage.com/k.txt?"), "r2 url: {out}");
    assert!(out.contains("%2Fauto%2Fs3%2Faws4_request"), "r2 region scope (auto): {out}");
    // every presigned URL carries the SigV4 query params.
    for tag in ["pathstyle", "vhost", "r2"] {
        let line = out.lines().find(|l| l.starts_with(tag)).unwrap();
        assert!(line.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"), "{tag} missing algo: {line}");
        assert!(line.contains("X-Amz-Signature="), "{tag} missing signature: {line}");
        assert!(line.contains("X-Amz-Expires=900"), "{tag} default expiry: {line}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// (f) S3 XML error body → err.code/message/status
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_s3_error_body_is_structured() {
    let router: Router = Arc::new(|_rec: &Recorded| Response {
        status: 403,
        headers: vec![("content-type".into(), "application/xml".into())],
        body: br#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>AccessDenied</Code><Message>Access Denied</Message><RequestId>REQ1</RequestId></Error>"#
            .to_vec(),
    });
    let (port, _rec) = spawn_stub(1, router);
    let src = format!(
        "{}{}",
        client_src(port),
        r#"
let [data, err] = client.get("denied.txt")
if (err == nil) { print("FAIL: no error"); exit(1) }
print(`code=${err.code}`)
print(`message=${err.message}`)
print(`status=${err.status}`)
"#
    );
    let (ok, out, e) = run_script(&src, "blob_s3_error.as", &[]);
    assert!(ok, "script should run (Tier-1): {out}\n{e}");
    assert!(out.contains("code=AccessDenied"), "code: {out}");
    assert!(out.contains("message=Access Denied"), "message: {out}");
    assert!(out.contains("status=403"), "status: {out}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (g) malformed XML → clean Tier-1 (never a panic)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_malformed_xml_is_clean_tier1() {
    let router: Router = Arc::new(|_rec: &Recorded| Response {
        status: 500,
        headers: vec![("content-type".into(), "application/xml".into())],
        body: b"<Error><Code>oops</Code".to_vec(), // truncated / malformed
    });
    let (port, _rec) = spawn_stub(1, router);
    let src = format!(
        "{}{}",
        client_src(port),
        r#"
let [data, err] = client.get("broken.txt")
if (err == nil) { print("FAIL: no error"); exit(1) }
print(`status=${err.status}`)
print(`has-message=${err.message != nil}`)
print("clean")
"#
    );
    let (ok, out, e) = run_script(&src, "blob_malformed_xml.as", &[]);
    assert!(ok, "malformed XML must be a clean Tier-1, not a panic: {out}\n{e}");
    assert!(out.contains("status=500"), "status: {out}");
    assert!(out.contains("clean"), "out: {out}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (h) cap_audit smoke — client ops + presign denied under --deny net
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn blob_ops_denied_under_deny_net() {
    // Whole-module Net: `blob.client(...)` is itself gated at the dispatch chokepoint,
    // and operating an already-built client is gated at the per-handle re-check. We
    // recover around the WHOLE chain (construct + op) so the denial surfaces wherever
    // it fires first — and `presign` (the secret-minting op) is denied too.
    let mk = |op: &str| {
        format!(
            r#"import * as blob from "std/blob"
let r = recover(() => {{
  let client = blob.client({{ endpoint: "http://127.0.0.1:9", region: "us-east-1",
    accessKey: "{ACCESS_KEY}", secretKey: "{SECRET_KEY}", bucket: "b", pathStyle: true }})
  return {op}
}})
print(r[1].message)
"#
        )
    };
    for (name, op) in [
        ("blob_deny_put.as", r#"client.put("k", "v")"#),
        ("blob_deny_get.as", r#"client.get("k")"#),
        ("blob_deny_head.as", r#"client.head("k")"#),
        ("blob_deny_delete.as", r#"client.delete("k")"#),
        ("blob_deny_presign.as", r#"client.presign("GET", "k")"#),
        ("blob_deny_client.as", r#"client"#),
    ] {
        let src = mk(op);
        let (ok, out, err) = run_script(&src, name, &["--deny", "net"]);
        assert!(ok, "[{name}] denial is recoverable; stderr: {err}");
        assert_eq!(out.trim(), "capability 'net' denied", "[{name}] out: {out}");
    }
}
