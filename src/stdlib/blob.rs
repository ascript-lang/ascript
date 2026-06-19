#![cfg(feature = "blob")]

//! `std/blob` — S3-compatible object storage (BATT B8 §9).
//!
//! A small S3 REST client over the SHARED pooled reqwest client
//! (`net_http::shared_client()` — there is NO second HTTP stack) with AWS
//! Signature Version 4 request signing (the [`sigv4`] core, B7). Every operation
//! is **Tier-1** (`[value, err]`): an S3 error body decodes to `{code, message,
//! status}`; a network failure or malformed XML is a clean error, never a panic.
//! Misuse (a non-object client config, a non-array `list` opts, a too-small
//! configured multipart part size) is a **Tier-2** panic.
//!
//! The `blob.client(...)` handle is a `NativeKind::BlobClient` carrying CONFIG ONLY
//! (endpoint, region, credentials, default bucket, path-style flag) behind a
//! [`ResourceState::BlobClient`]; operating it requires the `net` capability (whole
//! module, INCLUDING `presign` — minting a capability-bearing URL from the secret
//! key is gated alongside the rest of the secret-handling surface).
//!
//! SigV4 is security-critical: a wrong canonicalization silently produces an
//! invalid signature or, worse, signs the wrong request. Every stage of the
//! [`sigv4`] core is pinned byte-exactly against published / independently-derived
//! AWS vectors in the `sigv4_vector_battery` below.

/// AWS Signature Version 4 core. Pure functions over request components — no I/O,
/// no clock, no environment. The caller supplies the `amz_datetime`
/// (`YYYYMMDDTHHMMSSZ`) and a precomputed payload hash.
///
/// `allow(dead_code)`: this is the COMPLETE SigV4 surface (the `EMPTY_PAYLOAD_SHA256`
/// constant + the `SignedRequest`/`PresignedQuery` accessor fields). The B8 client
/// reaches most of it; the remainder is exercised by the `sigv4_vector_battery` and is
/// part of the audited, vector-pinned public API — kept whole on purpose.
#[allow(dead_code)]
pub(crate) mod sigv4 {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    type HmacSha256 = Hmac<Sha256>;

    /// SHA-256 of the empty body — the canonical payload hash for a request with no body.
    pub const EMPTY_PAYLOAD_SHA256: &str =
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    /// The literal payload-hash placeholder used by presigned URLs (and chunked uploads).
    pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

    /// The SigV4 algorithm identifier.
    pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";

    /// Lowercase-hex SHA-256 of `data`. Use for the payload hash and the
    /// canonical-request digest.
    pub fn sha256_hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        hex::encode(h.finalize())
    }

    fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
        mac.update(msg);
        mac.finalize().into_bytes().to_vec()
    }

    /// Percent-encode a single component per RFC 3986, AWS-strict: the unreserved
    /// set `A-Za-z0-9-._~` passes through; EVERYTHING else (including `/`, space,
    /// `+`) becomes uppercase `%XX`. Space → `%20` (never `+`). Used for query
    /// keys/values and for individual path segments.
    pub fn uri_encode_component(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for &b in s.as_bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(b as char);
                }
                _ => {
                    out.push('%');
                    out.push_str(&format!("{:02X}", b));
                }
            }
        }
        out
    }

    /// Canonical URI path (Stage 1). Each `/`-delimited segment is single-encoded
    /// per RFC 3986 (the `/` separators are preserved, never encoded). An empty
    /// path normalizes to `/`. Does NOT double-encode (S3 rule).
    pub fn canonical_uri(path: &str) -> String {
        if path.is_empty() || path == "/" {
            return "/".to_string();
        }
        // Preserve leading/trailing slashes and empty segments exactly.
        let mut out = String::with_capacity(path.len());
        let mut first = true;
        for seg in path.split('/') {
            if !first {
                out.push('/');
            }
            first = false;
            out.push_str(&uri_encode_component(seg));
        }
        out
    }

    /// Canonical query string (Stage 2) from already-split `(key, value)` pairs.
    /// Each key and value is percent-encoded, the pairs are sorted by encoded key
    /// then encoded value, joined `key=value` with `&`. A value-less param yields
    /// `key=`.
    pub fn canonical_query_pairs(pairs: &[(String, String)]) -> String {
        let mut encoded: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (uri_encode_component(k), uri_encode_component(v)))
            .collect();
        encoded.sort();
        encoded
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&")
    }

    /// Canonical query string (Stage 2) from a raw `a=b&c=d` query string. A pair
    /// without `=` is treated as a value-less param (`key=`).
    pub fn canonical_query_string(query: &str) -> String {
        if query.is_empty() {
            return String::new();
        }
        let pairs: Vec<(String, String)> = query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (p.to_string(), String::new()),
            })
            .collect();
        canonical_query_pairs(&pairs)
    }

    /// Trim + collapse internal runs of whitespace in a header value to a single
    /// space (AWS canonicalization). Leading/trailing whitespace removed.
    fn canonical_header_value(v: &str) -> String {
        // AWS's wording is "convert sequential spaces to a single space"; we also
        // collapse tabs into the same run. This is an intentional superset that matches
        // every real AWS SDK's trimall and the test-suite vectors — do not "fix" it back
        // to spaces-only.
        let mut out = String::with_capacity(v.len());
        let mut prev_space = false;
        for ch in v.trim().chars() {
            if ch == ' ' || ch == '\t' {
                if !prev_space {
                    out.push(' ');
                }
                prev_space = true;
            } else {
                out.push(ch);
                prev_space = false;
            }
        }
        out
    }

    /// Canonical headers block (Stage 3) + the signed-headers list (Stage 4).
    /// Header names are lowercased, values canonicalized, sorted by lowercase name.
    /// Returns `(canonical_headers_with_trailing_newline, signed_headers)`.
    pub fn canonical_headers(headers: &[(String, String)]) -> (String, String) {
        let mut norm: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), canonical_header_value(v)))
            .collect();
        norm.sort_by(|a, b| a.0.cmp(&b.0));
        let mut ch = String::new();
        for (k, v) in &norm {
            ch.push_str(k);
            ch.push(':');
            ch.push_str(v);
            ch.push('\n');
        }
        let signed = norm
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");
        (ch, signed)
    }

    /// The credential scope: `YYYYMMDD/region/service/aws4_request`.
    pub fn credential_scope(date: &str, region: &str, service: &str) -> String {
        format!("{}/{}/{}/aws4_request", date, region, service)
    }

    /// Canonical request (Stage 6). `canonical_query` and `canonical_headers` must
    /// already be canonicalized; `signed_headers` is the Stage-4 list.
    pub fn canonical_request(
        method: &str,
        canonical_uri: &str,
        canonical_query: &str,
        canonical_headers: &str,
        signed_headers: &str,
        payload_hash: &str,
    ) -> String {
        format!(
            "{method}\n{uri}\n{query}\n{headers}\n{signed}\n{payload}",
            method = method,
            uri = canonical_uri,
            query = canonical_query,
            headers = canonical_headers,
            signed = signed_headers,
            payload = payload_hash,
        )
    }

    /// String-to-sign (Stage 7).
    pub fn string_to_sign(amz_datetime: &str, scope: &str, canonical_request: &str) -> String {
        format!(
            "{algo}\n{dt}\n{scope}\n{crh}",
            algo = ALGORITHM,
            dt = amz_datetime,
            scope = scope,
            crh = sha256_hex(canonical_request.as_bytes()),
        )
    }

    /// Derive the SigV4 signing key (Stage 8): the HMAC chain
    /// `kDate → kRegion → kService → kSigning`.
    pub fn signing_key(secret_key: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
        let k_date = hmac(format!("AWS4{}", secret_key).as_bytes(), date.as_bytes());
        let k_region = hmac(&k_date, region.as_bytes());
        let k_service = hmac(&k_region, service.as_bytes());
        hmac(&k_service, b"aws4_request")
    }

    /// The final signature (Stage 9): lowercase-hex `HMAC(signing_key, string_to_sign)`.
    pub fn signature(signing_key: &[u8], string_to_sign: &str) -> String {
        hex::encode(hmac(signing_key, string_to_sign.as_bytes()))
    }

    /// The `Authorization` header value (Stage 10) for the signed-header variant.
    pub fn authorization_header(
        access_key: &str,
        scope: &str,
        signed_headers: &str,
        signature: &str,
    ) -> String {
        format!(
            "{algo} Credential={ak}/{scope}, SignedHeaders={sh}, Signature={sig}",
            algo = ALGORITHM,
            ak = access_key,
            scope = scope,
            sh = signed_headers,
            sig = signature,
        )
    }

    /// A fully-signed request: the `Authorization` header value plus the
    /// signed-headers list. The caller already supplies all headers to sign
    /// (`host`, `x-amz-date`, `x-amz-content-sha256`, and `x-amz-security-token`
    /// when a session token is present) and the `payload_hash`.
    pub struct SignedRequest {
        pub authorization: String,
        pub signed_headers: String,
        pub signature: String,
        pub scope: String,
    }

    /// Sign a request for the `Authorization`-header variant (Stages 1–10).
    ///
    /// `amz_datetime` is `YYYYMMDDTHHMMSSZ`; `date` is its `YYYYMMDD` prefix.
    #[allow(clippy::too_many_arguments)]
    pub fn sign_request(
        method: &str,
        uri_path: &str,
        query: &str,
        headers: &[(String, String)],
        payload_hash: &str,
        region: &str,
        service: &str,
        amz_datetime: &str,
        date: &str,
        access_key: &str,
        secret_key: &str,
    ) -> SignedRequest {
        let c_uri = canonical_uri(uri_path);
        let c_query = canonical_query_string(query);
        let (c_headers, signed_headers) = canonical_headers(headers);
        let c_req = canonical_request(
            method,
            &c_uri,
            &c_query,
            &c_headers,
            &signed_headers,
            payload_hash,
        );
        let scope = credential_scope(date, region, service);
        let sts = string_to_sign(amz_datetime, &scope, &c_req);
        let key = signing_key(secret_key, date, region, service);
        let sig = signature(&key, &sts);
        let authorization = authorization_header(access_key, &scope, &signed_headers, &sig);
        SignedRequest {
            authorization,
            signed_headers,
            signature: sig,
            scope,
        }
    }

    /// Build the canonical query string for a presigned URL (Stage 11) — the auth
    /// parameters live in the query, payload hash is `UNSIGNED-PAYLOAD`. Returns the
    /// canonical (sorted, encoded) query string WITHOUT the trailing
    /// `X-Amz-Signature` (which is appended after signing).
    #[allow(clippy::too_many_arguments)]
    pub fn presign_canonical_query(
        access_key: &str,
        scope: &str,
        amz_datetime: &str,
        expires_secs: u64,
        signed_headers: &str,
        session_token: Option<&str>,
        extra: &[(String, String)],
    ) -> String {
        let mut pairs: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".into(), ALGORITHM.into()),
            (
                "X-Amz-Credential".into(),
                format!("{}/{}", access_key, scope),
            ),
            ("X-Amz-Date".into(), amz_datetime.into()),
            ("X-Amz-Expires".into(), expires_secs.to_string()),
            ("X-Amz-SignedHeaders".into(), signed_headers.into()),
        ];
        if let Some(tok) = session_token {
            pairs.push(("X-Amz-Security-Token".into(), tok.into()));
        }
        pairs.extend(extra.iter().cloned());
        canonical_query_pairs(&pairs)
    }

    /// A presigned-URL result: the full query string (auth params + the trailing
    /// `X-Amz-Signature`) and the signature.
    pub struct PresignedQuery {
        pub query: String,
        pub signature: String,
    }

    /// Compute a presigned-URL query string (Stage 11). The `host` header is the
    /// single signed header for v1 presign; the canonical headers block is
    /// `host:<host>\n` and `signed_headers` is `host`.
    #[allow(clippy::too_many_arguments)]
    pub fn presign(
        method: &str,
        uri_path: &str,
        host: &str,
        region: &str,
        service: &str,
        amz_datetime: &str,
        date: &str,
        expires_secs: u64,
        access_key: &str,
        secret_key: &str,
        session_token: Option<&str>,
        extra_query: &[(String, String)],
    ) -> PresignedQuery {
        let scope = credential_scope(date, region, service);
        let signed_headers = "host";
        let c_query = presign_canonical_query(
            access_key,
            &scope,
            amz_datetime,
            expires_secs,
            signed_headers,
            session_token,
            extra_query,
        );
        let c_uri = canonical_uri(uri_path);
        let c_headers = format!("host:{}\n", host);
        let c_req = canonical_request(
            method,
            &c_uri,
            &c_query,
            &c_headers,
            signed_headers,
            UNSIGNED_PAYLOAD,
        );
        let sts = string_to_sign(amz_datetime, &scope, &c_req);
        let key = signing_key(secret_key, date, region, service);
        let sig = signature(&key, &sts);
        let query = format!("{}&X-Amz-Signature={}", c_query, sig);
        PresignedQuery {
            query,
            signature: sig,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// The S3 client (BATT B8 §9.2)
// ═════════════════════════════════════════════════════════════════════════════

use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::stdlib::{arg, want_object, want_string};
use crate::value::{NativeKind, NativeMethod, Value, ValueKind};
use indexmap::IndexMap;
use std::rc::Rc;

/// The S3 service name (always `s3` for object storage).
const SERVICE: &str = "s3";
/// Default presign expiry (seconds).
const DEFAULT_PRESIGN_EXPIRES: u64 = 900;
/// S3's hard floor for a non-final multipart part (5 MiB).
const MIN_PART_SIZE: usize = 5 * 1024 * 1024;

/// The CONFIG-ONLY state behind a `NativeKind::BlobClient` handle. Holds no socket —
/// every operation makes a fresh request through the SHARED pooled reqwest client,
/// so there is nothing to reclaim on drop beyond the plain data. Boxed in the
/// `ResourceState` enum to keep it compact.
#[derive(Clone)]
pub(crate) struct BlobClientState {
    /// The endpoint base URL (scheme://host[:port]), no trailing slash.
    endpoint: String,
    region: String,
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    /// Default bucket (overridable per-call via `opts.bucket`).
    bucket: Option<String>,
    /// Path-style (`endpoint/bucket/key`) vs virtual-host (`bucket.host/key`).
    path_style: bool,
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("client", crate::stdlib::bi("blob.client"))]
}

/// Build the `[YYYYMMDDTHHMMSSZ, YYYYMMDD]` pair from epoch-ms (the det clock seam).
fn amz_dates(epoch_ms: f64) -> (String, String) {
    // Convert epoch ms → a UTC civil datetime WITHOUT a calendar dep: derive the
    // date from days-since-epoch and the time from the second-of-day. This matches
    // the `YYYYMMDDTHHMMSSZ` / `YYYYMMDD` SigV4 forms and is determinism-replayable
    // by construction (input is the det clock).
    let total_secs = (epoch_ms / 1000.0).floor() as i64;
    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400);
    let (h, mi, s) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
    let (y, mo, d) = civil_from_days(days);
    let datetime = format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z");
    let date = format!("{y:04}{mo:02}{d:02}");
    (datetime, date)
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse + type-check `blob.client(config)`. Misuse → Tier-2 panic.
fn parse_client_config(config: &Value, span: Span) -> Result<BlobClientState, Control> {
    let o = want_object(config, span, "blob.client")?;
    let req_str = |key: &str| -> Result<String, Control> {
        match o.get(key) {
            Some(v) => Ok(want_string(&v, span, &format!("blob.client '{key}'"))?.to_string()),
            None => Err(AsError::at(format!("blob.client: '{key}' is required"), span).into()),
        }
    };
    let opt_str = |key: &str| -> Result<Option<String>, Control> {
        match o.get(key) {
            None => Ok(None),
            Some(v) if matches!(v.kind(), ValueKind::Nil) => Ok(None),
            Some(v) => Ok(Some(want_string(&v, span, &format!("blob.client '{key}'"))?.to_string())),
        }
    };
    let endpoint = req_str("endpoint")?;
    let endpoint = endpoint.trim_end_matches('/').to_string();
    let region = req_str("region")?;
    let access_key = req_str("accessKey")?;
    let secret_key = req_str("secretKey")?;
    let session_token = opt_str("sessionToken")?;
    let bucket = opt_str("bucket")?;
    // path_style defaults true (non-AWS endpoints are usually path-style); an AWS
    // endpoint (`amazonaws.com`) defaults to virtual-host.
    let path_style = match o.get("pathStyle") {
        Some(v) if !matches!(v.kind(), ValueKind::Nil) => v.is_truthy(),
        _ => !endpoint.contains("amazonaws.com"),
    };
    if endpoint.is_empty() || !endpoint.contains("://") {
        return Err(AsError::at(
            format!("blob.client: 'endpoint' must be an absolute URL (scheme://host), got {endpoint:?}"),
            span,
        )
        .into());
    }
    Ok(BlobClientState {
        endpoint,
        region,
        access_key,
        secret_key,
        session_token,
        bucket,
        path_style,
    })
}

/// A built request target: the absolute URL, the host header to sign, and the
/// canonical URI path (already split so the host carries no path).
struct Target {
    url: String,
    host: String,
    /// The path component to sign (begins with `/`).
    path: String,
}

impl BlobClientState {
    /// Resolve the effective bucket (per-call override else default). A missing
    /// bucket is a Tier-2 misuse.
    fn bucket_for(&self, opts: Option<&Value>, span: Span) -> Result<String, Control> {
        if let Some(ov) = opts {
            if let ValueKind::Object(o) = ov.kind() {
                if let Some(b) = o.get("bucket") {
                    if !matches!(b.kind(), ValueKind::Nil) {
                        return Ok(want_string(&b, span, "blob opts.bucket")?.to_string());
                    }
                }
            }
        }
        self.bucket.clone().ok_or_else(|| {
            AsError::at(
                "blob: no bucket — set a default bucket on the client or pass opts.bucket",
                span,
            )
            .into()
        })
    }

    /// Build the URL + host + signing path for a (bucket, key) under the configured
    /// addressing style. `key` may be empty (bucket-level operations like `list`).
    fn target(&self, bucket: &str, key: &str) -> Target {
        // Split the endpoint into scheme + authority (host[:port]).
        let (scheme, authority) = match self.endpoint.split_once("://") {
            Some((s, rest)) => (s, rest),
            None => ("https", self.endpoint.as_str()),
        };
        // Encode each key path segment (S3 single-encode, preserving `/`).
        let key_path = if key.is_empty() {
            String::new()
        } else {
            sigv4::canonical_uri(&format!("/{}", key.trim_start_matches('/')))
        };
        if self.path_style {
            // endpoint/bucket/key — host is the endpoint authority unchanged.
            let path = if key.is_empty() {
                format!("/{bucket}")
            } else {
                format!("/{bucket}{key_path}")
            };
            Target {
                url: format!("{scheme}://{authority}{path}"),
                host: authority.to_string(),
                path,
            }
        } else {
            // virtual-host: bucket.host/key
            let vhost = format!("{bucket}.{authority}");
            let path = if key.is_empty() { "/".to_string() } else { key_path };
            Target {
                url: format!("{scheme}://{vhost}{path}"),
                host: vhost,
                path,
            }
        }
    }
}

/// Decode an S3 XML error body → `{code, message, status}`. A malformed body still
/// yields a clean error (carrying the status + the raw text as the message), never a
/// panic. `status` is always present.
fn s3_error_value(status: u16, body: &str) -> Value {
    let mut code = String::new();
    let mut message = String::new();
    if let Ok(doc) = crate::stdlib::xml::parse_document(body) {
        code = xml_child_text(&doc, "Code").unwrap_or_default();
        message = xml_child_text(&doc, "Message").unwrap_or_default();
    }
    let mut o: IndexMap<String, Value> = IndexMap::new();
    o.insert("code".to_string(), Value::str(if code.is_empty() { format!("HTTP{status}") } else { code }));
    o.insert(
        "message".to_string(),
        Value::str(if message.is_empty() {
            // Carry a trimmed snippet of the raw body so the user sees SOMETHING.
            let snippet: String = body.trim().chars().take(200).collect();
            if snippet.is_empty() {
                format!("S3 request failed with status {status}")
            } else {
                snippet
            }
        } else {
            message
        }),
    );
    o.insert("status".to_string(), Value::int(status as i64));
    Value::object(o)
}

/// An S3 XML element value is `{tag, attrs, children}`; `children` is an array of
/// either text strings or nested elements. Find the FIRST child element with `tag`
/// and return its concatenated text content.
fn xml_child_text(elem: &Value, tag: &str) -> Option<String> {
    let child = xml_find_child(elem, tag)?;
    Some(xml_text(&child))
}

/// Find the first direct child element of `elem` whose tag == `tag`.
fn xml_find_child(elem: &Value, tag: &str) -> Option<Value> {
    let ValueKind::Object(o) = elem.kind() else { return None };
    let children = o.get("children")?;
    let ValueKind::Array(arr) = children.kind() else { return None };
    for c in arr.borrow().iter() {
        if let ValueKind::Object(co) = c.kind() {
            if let Some(t) = co.get("tag") {
                if matches!(t.kind(), ValueKind::Str(s) if s.as_ref() == tag) {
                    return Some(c.clone());
                }
            }
        }
    }
    None
}

/// All direct child elements of `elem` whose tag == `tag`.
fn xml_find_children(elem: &Value, tag: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let ValueKind::Object(o) = elem.kind() else { return out };
    let Some(children) = o.get("children") else { return out };
    let ValueKind::Array(arr) = children.kind() else { return out };
    for c in arr.borrow().iter() {
        if let ValueKind::Object(co) = c.kind() {
            if let Some(t) = co.get("tag") {
                if matches!(t.kind(), ValueKind::Str(s) if s.as_ref() == tag) {
                    out.push(c.clone());
                }
            }
        }
    }
    out
}

/// Concatenated text content of an element (only the direct text children).
fn xml_text(elem: &Value) -> String {
    let ValueKind::Object(o) = elem.kind() else { return String::new() };
    let Some(children) = o.get("children") else { return String::new() };
    let ValueKind::Array(arr) = children.kind() else { return String::new() };
    let mut s = String::new();
    for c in arr.borrow().iter() {
        if let ValueKind::Str(t) = c.kind() {
            s.push_str(t);
        }
    }
    s
}

/// A signed request, ready to send: method, URL, host, the headers to attach
/// (including the SigV4 `Authorization`), and the body.
struct SignedHttp {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Interp {
    /// BATT B8 §9.2 — dispatch the single module function `blob.client(config)`. The
    /// cap gate already fired at `call_stdlib` (`required_cap("blob", _) == Net`).
    pub(crate) async fn call_blob(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "client" => {
                let cfg = parse_client_config(&arg(args, 0), span)?;
                let handle = self.register_resource(
                    NativeKind::BlobClient,
                    IndexMap::new(),
                    ResourceState::BlobClient(Box::new(cfg)),
                );
                Ok(handle)
            }
            _ => Err(AsError::at(format!("std/blob has no function '{func}'"), span).into()),
        }
    }

    /// Dispatch a method on a live `BlobClient` handle. The per-handle Net re-check
    /// fired in `call_native_method` before reaching here (so a `caps.drop("net")`
    /// holds for an already-built client — INCLUDING `presign`).
    pub(crate) async fn call_blob_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        // Clone the config out (take-out-across-await: never hold the resources borrow
        // across the network `.await`). The client is config-only, so we clone + return
        // it immediately (it is never mutated).
        let cfg = match self.take_resource(id) {
            Some(ResourceState::BlobClient(s)) => {
                let cfg = (*s).clone();
                self.return_resource(id, ResourceState::BlobClient(s));
                cfg
            }
            other => {
                if let Some(o) = other {
                    self.return_resource(id, o);
                }
                return Err(AsError::at("blob client is closed", span).into());
            }
        };
        match m.method.as_str() {
            "put" => self.blob_put(&cfg, &args, span).await,
            "get" => self.blob_get(&cfg, &args, span).await,
            "head" => self.blob_head(&cfg, &args, span).await,
            "delete" => self.blob_delete(&cfg, &args, span).await,
            "list" => self.blob_list(&cfg, &args, span),
            "presign" => self.blob_presign(&cfg, &args, span),
            "putMultipart" => self.blob_put_multipart(&cfg, &args, span).await,
            other => Err(AsError::at(format!("blobClient has no method '{other}'"), span).into()),
        }
    }

    /// Sign a request: build the canonical headers (host, x-amz-date,
    /// x-amz-content-sha256, optional x-amz-security-token), run SigV4, and return
    /// the ready-to-send request. Time from the determinism seam.
    fn sign_http(
        &self,
        cfg: &BlobClientState,
        method: &str,
        target: &Target,
        query: &str,
        mut extra_headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> SignedHttp {
        let (amz_datetime, date) = amz_dates(self.clock_now_ms());
        let payload_hash = sigv4::sha256_hex(&body);

        let mut headers: Vec<(String, String)> = vec![
            ("host".to_string(), target.host.clone()),
            ("x-amz-date".to_string(), amz_datetime.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
        ];
        if let Some(tok) = &cfg.session_token {
            headers.push(("x-amz-security-token".to_string(), tok.clone()));
        }
        headers.append(&mut extra_headers);

        let signed = sigv4::sign_request(
            method,
            &target.path,
            query,
            &headers,
            &payload_hash,
            &cfg.region,
            SERVICE,
            &amz_datetime,
            &date,
            &cfg.access_key,
            &cfg.secret_key,
        );

        let url = if query.is_empty() {
            target.url.clone()
        } else {
            format!("{}?{}", target.url, query)
        };
        // Attach the Authorization header to the wire headers.
        headers.push(("authorization".to_string(), signed.authorization));

        SignedHttp {
            method: method.to_string(),
            url,
            headers,
            body,
        }
    }

    /// Send a signed request through the shared pooled client. Returns the reqwest
    /// response or a Tier-1 error pair (network failure).
    async fn send_signed(&self, req: SignedHttp) -> Result<reqwest::Response, Value> {
        let client = crate::stdlib::net_http::shared_client();
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .unwrap_or(reqwest::Method::GET);
        let mut rb = client.request(method, &req.url);
        for (k, v) in &req.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if !req.body.is_empty() {
            rb = rb.body(req.body.clone());
        }
        match rb.send().await {
            Ok(r) => Ok(r),
            Err(e) => Err(make_error(Value::str(format!("blob request failed: {e}")))),
        }
    }

    // ── put ────────────────────────────────────────────────────────────────────

    async fn blob_put(&self, cfg: &BlobClientState, args: &[Value], span: Span) -> Result<Value, Control> {
        let key = want_string(&arg(args, 0), span, "blob.put key")?.to_string();
        let data = body_bytes(&arg(args, 1), span, "blob.put data")?;
        let opts = arg(args, 2);
        let opts_ref = (!matches!(opts.kind(), ValueKind::Nil)).then_some(&opts);
        let bucket = cfg.bucket_for(opts_ref, span)?;

        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(ov) = opts_ref {
            if let ValueKind::Object(o) = ov.kind() {
                if let Some(ct) = o.get("contentType") {
                    if let ValueKind::Str(s) = ct.kind() {
                        headers.push(("content-type".to_string(), s.to_string()));
                    }
                }
                if let Some(md) = o.get("metadata") {
                    if let ValueKind::Object(mo) = md.kind() {
                        for (k, v) in mo.entries() {
                            if let ValueKind::Str(s) = v.kind() {
                                headers.push((format!("x-amz-meta-{}", k.to_ascii_lowercase()), s.to_string()));
                            }
                        }
                    }
                }
            }
        }

        let target = cfg.target(&bucket, &key);
        let req = self.sign_http(cfg, "PUT", &target, "", headers, data);
        let resp = match self.send_signed(req).await {
            Ok(r) => r,
            Err(e) => return Ok(make_pair(Value::nil(), e)),
        };
        let status = resp.status().as_u16();
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_string());
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Ok(make_pair(Value::nil(), s3_error_value(status, &body)));
        }
        Ok(make_pair(Value::str(etag.unwrap_or_default()), Value::nil()))
    }

    // ── get ────────────────────────────────────────────────────────────────────

    async fn blob_get(&self, cfg: &BlobClientState, args: &[Value], span: Span) -> Result<Value, Control> {
        let key = want_string(&arg(args, 0), span, "blob.get key")?.to_string();
        let opts = arg(args, 1);
        let opts_ref = (!matches!(opts.kind(), ValueKind::Nil)).then_some(&opts);
        let bucket = cfg.bucket_for(opts_ref, span)?;

        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(ov) = opts_ref {
            if let ValueKind::Object(o) = ov.kind() {
                if let Some(r) = o.get("range") {
                    if let ValueKind::Array(a) = r.kind() {
                        let a = a.borrow();
                        if a.len() == 2 {
                            if let (ValueKind::Int(s), ValueKind::Int(e)) = (a[0].kind(), a[1].kind()) {
                                headers.push(("range".to_string(), format!("bytes={s}-{e}")));
                            }
                        }
                    }
                }
            }
        }

        let target = cfg.target(&bucket, &key);
        let req = self.sign_http(cfg, "GET", &target, "", headers, Vec::new());
        let resp = match self.send_signed(req).await {
            Ok(r) => r,
            Err(e) => return Ok(make_pair(Value::nil(), e)),
        };
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Ok(make_pair(Value::nil(), s3_error_value(status, &body)));
        }
        match resp.bytes().await {
            Ok(b) => Ok(make_pair(Value::bytes(b.to_vec()), Value::nil())),
            Err(e) => Ok(make_pair(Value::nil(), make_error(Value::str(format!("blob.get read failed: {e}"))))),
        }
    }

    // ── head ───────────────────────────────────────────────────────────────────

    async fn blob_head(&self, cfg: &BlobClientState, args: &[Value], span: Span) -> Result<Value, Control> {
        let key = want_string(&arg(args, 0), span, "blob.head key")?.to_string();
        let opts = arg(args, 1);
        let opts_ref = (!matches!(opts.kind(), ValueKind::Nil)).then_some(&opts);
        let bucket = cfg.bucket_for(opts_ref, span)?;

        let target = cfg.target(&bucket, &key);
        let req = self.sign_http(cfg, "HEAD", &target, "", Vec::new(), Vec::new());
        let resp = match self.send_signed(req).await {
            Ok(r) => r,
            Err(e) => return Ok(make_pair(Value::nil(), e)),
        };
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            // A HEAD has no body; surface the status.
            return Ok(make_pair(Value::nil(), s3_error_value(status, "")));
        }
        let h = resp.headers();
        let get = |name: &str| h.get(name).and_then(|v| v.to_str().ok()).map(|s| s.to_string());
        let mut out: IndexMap<String, Value> = IndexMap::new();
        let size = get("content-length").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        out.insert("size".to_string(), Value::int(size));
        out.insert(
            "etag".to_string(),
            Value::str(get("etag").map(|s| s.trim_matches('"').to_string()).unwrap_or_default()),
        );
        out.insert("contentType".to_string(), Value::str(get("content-type").unwrap_or_default()));
        out.insert("lastModified".to_string(), Value::str(get("last-modified").unwrap_or_default()));
        let mut meta: IndexMap<String, Value> = IndexMap::new();
        for (k, v) in h.iter() {
            let kn = k.as_str();
            if let Some(suffix) = kn.strip_prefix("x-amz-meta-") {
                if let Ok(vs) = v.to_str() {
                    meta.insert(suffix.to_string(), Value::str(vs.to_string()));
                }
            }
        }
        out.insert("metadata".to_string(), Value::object(meta));
        Ok(make_pair(Value::object(out), Value::nil()))
    }

    // ── delete ──────────────────────────────────────────────────────────────────

    async fn blob_delete(&self, cfg: &BlobClientState, args: &[Value], span: Span) -> Result<Value, Control> {
        let key = want_string(&arg(args, 0), span, "blob.delete key")?.to_string();
        let opts = arg(args, 1);
        let opts_ref = (!matches!(opts.kind(), ValueKind::Nil)).then_some(&opts);
        let bucket = cfg.bucket_for(opts_ref, span)?;

        let target = cfg.target(&bucket, &key);
        let req = self.sign_http(cfg, "DELETE", &target, "", Vec::new(), Vec::new());
        let resp = match self.send_signed(req).await {
            Ok(r) => r,
            Err(e) => return Ok(make_pair(Value::nil(), e)),
        };
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Ok(make_pair(Value::nil(), s3_error_value(status, &body)));
        }
        Ok(make_pair(Value::nil(), Value::nil()))
    }

    // ── list (lazy paginating generator) ─────────────────────────────────────────

    fn blob_list(&self, cfg: &BlobClientState, args: &[Value], span: Span) -> Result<Value, Control> {
        let opts = arg(args, 0);
        // A non-object opts (other than nil) is a Tier-2 misuse.
        if !matches!(opts.kind(), ValueKind::Nil | ValueKind::Object(_)) {
            return Err(AsError::at(
                format!("blob.list: opts must be an object, got {}", crate::interp::type_name(&opts)),
                span,
            )
            .into());
        }
        let opts_ref = (!matches!(opts.kind(), ValueKind::Nil)).then_some(&opts);
        let bucket = cfg.bucket_for(opts_ref, span)?;
        let get_str = |key: &str| -> Option<String> {
            opts_ref.and_then(|ov| {
                if let ValueKind::Object(o) = ov.kind() {
                    o.get(key).and_then(|v| match v.kind() {
                        ValueKind::Str(s) => Some(s.to_string()),
                        _ => None,
                    })
                } else {
                    None
                }
            })
        };
        let prefix = get_str("prefix");
        let delimiter = get_str("delimiter");
        let page_size: Option<i64> = opts_ref.and_then(|ov| {
            if let ValueKind::Object(o) = ov.kind() {
                o.get("pageSize").and_then(|v| match v.kind() {
                    ValueKind::Int(n) => Some(n),
                    _ => None,
                })
            } else {
                None
            }
        });

        let cfg = cfg.clone();
        let me = self.rc();
        // A native cursor generator: it makes its OWN signed HTTP calls per page,
        // yielding one entry per `next()`. Page N+1 is fetched ONLY when iteration
        // crosses past page N's entries (driven by NextContinuationToken/IsTruncated).
        let body: std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Control>>>> =
            Box::pin(async move {
                let mut continuation: Option<String> = None;
                loop {
                    // Build the list-objects-v2 query.
                    let mut pairs: Vec<(String, String)> = vec![("list-type".into(), "2".into())];
                    if let Some(p) = &prefix {
                        if !p.is_empty() {
                            pairs.push(("prefix".into(), p.clone()));
                        }
                    }
                    if let Some(d) = &delimiter {
                        if !d.is_empty() {
                            pairs.push(("delimiter".into(), d.clone()));
                        }
                    }
                    if let Some(ps) = page_size {
                        pairs.push(("max-keys".into(), ps.to_string()));
                    }
                    if let Some(tok) = &continuation {
                        pairs.push(("continuation-token".into(), tok.clone()));
                    }
                    let query = sigv4::canonical_query_pairs(&pairs);

                    let target = cfg.target(&bucket, "");
                    let req = me.sign_http(&cfg, "GET", &target, &query, Vec::new(), Vec::new());
                    let resp = match me.send_signed(req).await {
                        Ok(r) => r,
                        Err(e) => {
                            let g = crate::coro::current_generator().expect("inside a generator");
                            g.yield_(make_pair(Value::nil(), e)).await;
                            return Ok(Value::nil());
                        }
                    };
                    let status = resp.status().as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    if !(200..300).contains(&status) {
                        let g = crate::coro::current_generator().expect("inside a generator");
                        g.yield_(make_pair(Value::nil(), s3_error_value(status, &text))).await;
                        return Ok(Value::nil());
                    }
                    let doc = match crate::stdlib::xml::parse_document(&text) {
                        Ok(d) => d,
                        Err(e) => {
                            let g = crate::coro::current_generator().expect("inside a generator");
                            g.yield_(make_pair(
                                Value::nil(),
                                make_error(Value::str(format!("blob.list: malformed XML response: {e}"))),
                            ))
                            .await;
                            return Ok(Value::nil());
                        }
                    };
                    for c in xml_find_children(&doc, "Contents") {
                        let entry = list_entry_value(&c);
                        let g = crate::coro::current_generator().expect("inside a generator");
                        g.yield_(entry).await;
                    }
                    let truncated = xml_child_text(&doc, "IsTruncated")
                        .map(|s| s.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);
                    let next = xml_child_text(&doc, "NextContinuationToken").filter(|s| !s.is_empty());
                    match (truncated, next) {
                        (true, Some(tok)) => continuation = Some(tok),
                        _ => break,
                    }
                }
                Ok(Value::nil())
            });
        Ok(Value::generator(Rc::new(crate::coro::GeneratorHandle::new(body))))
    }

    // ── presign (pure; no network) ───────────────────────────────────────────────

    fn blob_presign(&self, cfg: &BlobClientState, args: &[Value], span: Span) -> Result<Value, Control> {
        let method = want_string(&arg(args, 0), span, "blob.presign method")?.to_string();
        let key = want_string(&arg(args, 1), span, "blob.presign key")?.to_string();
        let opts = arg(args, 2);
        let opts_ref = (!matches!(opts.kind(), ValueKind::Nil)).then_some(&opts);
        let bucket = cfg.bucket_for(opts_ref, span)?;

        let expires = opts_ref
            .and_then(|ov| {
                if let ValueKind::Object(o) = ov.kind() {
                    o.get("expires").and_then(|v| match v.kind() {
                        ValueKind::Int(n) if n > 0 => Some(n as u64),
                        _ => None,
                    })
                } else {
                    None
                }
            })
            .unwrap_or(DEFAULT_PRESIGN_EXPIRES);

        let target = cfg.target(&bucket, &key);
        let (amz_datetime, date) = amz_dates(self.clock_now_ms());
        let p = sigv4::presign(
            &method,
            &target.path,
            &target.host,
            &cfg.region,
            SERVICE,
            &amz_datetime,
            &date,
            expires,
            &cfg.access_key,
            &cfg.secret_key,
            cfg.session_token.as_deref(),
            &[],
        );
        let url = format!("{}?{}", target.url, p.query);
        Ok(make_pair(Value::str(url), Value::nil()))
    }

    // ── putMultipart (create → parts → complete; abort on error) ──────────────────

    async fn blob_put_multipart(
        &self,
        cfg: &BlobClientState,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let key = want_string(&arg(args, 0), span, "blob.putMultipart key")?.to_string();
        let source = arg(args, 1);
        let opts = arg(args, 2);
        let opts_ref = (!matches!(opts.kind(), ValueKind::Nil)).then_some(&opts);
        let bucket = cfg.bucket_for(opts_ref, span)?;

        // A CONFIGURED partSize below the 5 MiB floor is a Tier-2 misuse.
        if let Some(ov) = opts_ref {
            if let ValueKind::Object(o) = ov.kind() {
                if let Some(ps) = o.get("partSize") {
                    if let ValueKind::Int(n) = ps.kind() {
                        if (n as usize) < MIN_PART_SIZE {
                            return Err(AsError::at(
                                format!(
                                    "blob.putMultipart: partSize {n} is below the S3 minimum of {MIN_PART_SIZE} bytes (5 MiB) for non-final parts"
                                ),
                                span,
                            )
                            .into());
                        }
                    }
                }
            }
        }

        // Prepare a CHUNK SOURCE: either an eager array of chunks or a live generator
        // we pull ONE chunk at a time (true streaming — a large object is never fully
        // materialized; only the current part + a one-chunk lookahead are in memory).
        let mut chunk_source = ChunkSource::new(&source, span)?;

        let content_type = opts_ref.and_then(|ov| {
            if let ValueKind::Object(o) = ov.kind() {
                o.get("contentType").and_then(|v| match v.kind() {
                    ValueKind::Str(s) => Some(s.to_string()),
                    _ => None,
                })
            } else {
                None
            }
        });

        // 1) InitiateMultipartUpload — POST ?uploads
        let target = cfg.target(&bucket, &key);
        let mut init_headers: Vec<(String, String)> = Vec::new();
        if let Some(ct) = &content_type {
            init_headers.push(("content-type".to_string(), ct.clone()));
        }
        let req = self.sign_http(cfg, "POST", &target, "uploads=", init_headers, Vec::new());
        let resp = match self.send_signed(req).await {
            Ok(r) => r,
            Err(e) => return Ok(make_pair(Value::nil(), e)),
        };
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Ok(make_pair(Value::nil(), s3_error_value(status, &text)));
        }
        let upload_id = match crate::stdlib::xml::parse_document(&text)
            .ok()
            .and_then(|d| xml_child_text(&d, "UploadId"))
            .filter(|s| !s.is_empty())
        {
            Some(id) => id,
            None => {
                return Ok(make_pair(
                    Value::nil(),
                    make_error(Value::str("blob.putMultipart: no UploadId in InitiateMultipartUpload response")),
                ))
            }
        };

        // 2) UploadPart per chunk — PUT ?partNumber=N&uploadId=... STREAMING with a
        //    one-chunk LOOKAHEAD so we know whether the current part is the FINAL one
        //    (the 5 MiB floor applies only to non-final parts). We hold at most the
        //    current chunk + the next (lookahead) in memory at any time.
        let mut part_etags: Vec<(usize, String)> = Vec::new();
        let mut failure: Option<Value> = None;
        let mut part_number: usize = 0;
        // Prime the pipeline with the first chunk.
        let mut current = match chunk_source.next(span).await {
            Ok(c) => c,
            Err(e) => {
                // A generator that errors before producing anything → abort + Tier-1
                // (a propagation/panic from the producer is surfaced as a Tier-2 below).
                self.abort_multipart(cfg, &target, &upload_id).await;
                return Err(e);
            }
        };
        if current.is_none() {
            // An empty source (no chunks) — abort the just-created upload, Tier-1.
            self.abort_multipart(cfg, &target, &upload_id).await;
            return Ok(make_pair(
                Value::nil(),
                make_error(Value::str("blob.putMultipart: source produced no chunks")),
            ));
        }
        while let Some(chunk) = current.take() {
            // Pull the NEXT chunk to learn whether `chunk` is the final part.
            let next = match chunk_source.next(span).await {
                Ok(c) => c,
                Err(e) => {
                    // The producer errored mid-stream: abort + re-raise (Tier-2 propagate
                    // / panic from a user generator surfaces directly).
                    self.abort_multipart(cfg, &target, &upload_id).await;
                    return Err(e);
                }
            };
            let is_final = next.is_none();
            part_number += 1;

            // RUNTIME-STREAM floor (§9.2): a NON-FINAL pulled chunk below 5 MiB is a
            // Tier-1 error (distinct from the configured-partSize Tier-2 above).
            if !is_final && chunk.len() < MIN_PART_SIZE {
                failure = Some(make_error(Value::str(format!(
                    "blob.putMultipart: non-final part {part_number} is {} bytes, below the S3 \
                     minimum of {MIN_PART_SIZE} bytes (5 MiB) — buffer larger chunks from the source",
                    chunk.len()
                ))));
                break;
            }

            let canon =
                sigv4::canonical_query_string(&format!("partNumber={part_number}&uploadId={upload_id}"));
            let req = self.sign_http(cfg, "PUT", &target, &canon, Vec::new(), chunk);
            let resp = match self.send_signed(req).await {
                Ok(r) => r,
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            };
            let st = resp.status().as_u16();
            let etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            if !(200..300).contains(&st) {
                let b = resp.text().await.unwrap_or_default();
                failure = Some(s3_error_value(st, &b));
                break;
            }
            match etag {
                Some(e) => part_etags.push((part_number, e)),
                None => {
                    failure = Some(make_error(Value::str(format!(
                        "blob.putMultipart: part {part_number} response had no ETag"
                    ))));
                    break;
                }
            }
            // Advance: the lookahead becomes the current chunk for the next iteration.
            current = next;
        }

        // On ANY part error → AbortMultipartUpload (no orphaned upload), then return.
        if let Some(err) = failure {
            self.abort_multipart(cfg, &target, &upload_id).await;
            return Ok(make_pair(Value::nil(), err));
        }

        // 3) CompleteMultipartUpload — POST ?uploadId=... with the part list XML.
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (n, etag) in &part_etags {
            xml.push_str(&format!(
                "<Part><PartNumber>{n}</PartNumber><ETag>{}</ETag></Part>",
                xml_escape(etag)
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let complete_q = sigv4::canonical_query_string(&format!("uploadId={upload_id}"));
        let req = self.sign_http(
            cfg,
            "POST",
            &target,
            &complete_q,
            vec![("content-type".to_string(), "application/xml".to_string())],
            xml.into_bytes(),
        );
        let resp = match self.send_signed(req).await {
            Ok(r) => r,
            Err(e) => return Ok(make_pair(Value::nil(), e)),
        };
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Ok(make_pair(Value::nil(), s3_error_value(status, &text)));
        }
        // S3 can return a 200 with an Error body for CompleteMultipartUpload.
        if text.contains("<Error") {
            return Ok(make_pair(Value::nil(), s3_error_value(200, &text)));
        }
        let etag = crate::stdlib::xml::parse_document(&text)
            .ok()
            .and_then(|d| xml_child_text(&d, "ETag"))
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        Ok(make_pair(Value::str(etag), Value::nil()))
    }

    /// AbortMultipartUpload (best-effort) — DELETE ?uploadId=... so a failed/abandoned
    /// multipart leaves no orphaned upload on the server. Errors are ignored (the part
    /// failure is the one surfaced to the caller).
    async fn abort_multipart(&self, cfg: &BlobClientState, target: &Target, upload_id: &str) {
        let abort_q = sigv4::canonical_query_string(&format!("uploadId={upload_id}"));
        let req = self.sign_http(cfg, "DELETE", target, &abort_q, Vec::new(), Vec::new());
        let _ = self.send_signed(req).await;
    }
}

/// Build a `{key, size, etag, lastModified}` value from a `<Contents>` element.
fn list_entry_value(contents: &Value) -> Value {
    let mut o: IndexMap<String, Value> = IndexMap::new();
    o.insert("key".to_string(), Value::str(xml_child_text(contents, "Key").unwrap_or_default()));
    let size = xml_child_text(contents, "Size").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    o.insert("size".to_string(), Value::int(size));
    o.insert(
        "etag".to_string(),
        Value::str(xml_child_text(contents, "ETag").map(|s| s.trim_matches('"').to_string()).unwrap_or_default()),
    );
    o.insert(
        "lastModified".to_string(),
        Value::str(xml_child_text(contents, "LastModified").unwrap_or_default()),
    );
    Value::object(o)
}

/// Coerce a `put`/multipart body argument (string or bytes) into raw bytes.
fn body_bytes(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(s.as_bytes().to_vec()),
        ValueKind::Bytes(b) => Ok(b.borrow().to_vec()),
        _ => Err(AsError::at(
            format!("{ctx} must be a string or bytes, got {}", crate::interp::type_name(v)),
            span,
        )
        .into()),
    }
}

/// A multipart chunk source — the unifying abstraction over the two accepted
/// `source` shapes so the UploadPart loop has ONE streaming code path:
///
/// - `Array` — an eager `array<bytes | string>`; chunks are coerced lazily by index
///   (no up-front copy of the whole array's bytes beyond what the Value already holds).
/// - `Generator` — a live `fn*`/`async fn*` value; one chunk is PULLED per part via
///   `g.resume(...)`, so a large object is never fully materialized — only the current
///   part plus the loop's one-chunk lookahead are in memory at a time.
///
/// A `source` that is neither is a Tier-2 misuse, rejected at construction.
enum ChunkSource {
    Array { items: Value, idx: usize },
    Generator(Rc<crate::coro::GeneratorHandle>),
}

impl ChunkSource {
    /// Validate `source` and build the cursor. Tier-2 on a wrong-kind source.
    fn new(source: &Value, span: Span) -> Result<ChunkSource, Control> {
        match source.kind() {
            ValueKind::Array(_) => Ok(ChunkSource::Array { items: source.clone(), idx: 0 }),
            ValueKind::Generator(g) => Ok(ChunkSource::Generator(g.clone())),
            _ => Err(AsError::at(
                format!(
                    "blob.putMultipart: source must be an array of bytes/string chunks or a generator, got {}",
                    crate::interp::type_name(source)
                ),
                span,
            )
            .into()),
        }
    }

    /// Pull the next chunk's bytes, or `None` at end-of-source. A non-bytes/string
    /// yielded value is a Tier-2 misuse; a generator panic/propagation surfaces as the
    /// `Control` it raised (the caller aborts the upload before re-raising). The
    /// generator `.resume` await holds NO resources borrow (a consumer-driven generator
    /// uses none), satisfying the never-hold-a-borrow-across-await rule.
    async fn next(&mut self, span: Span) -> Result<Option<Vec<u8>>, Control> {
        match self {
            ChunkSource::Array { items, idx } => {
                let ValueKind::Array(a) = items.kind() else { return Ok(None) };
                let item = {
                    let b = a.borrow();
                    if *idx >= b.len() {
                        return Ok(None);
                    }
                    b[*idx].clone()
                };
                let i = *idx;
                *idx += 1;
                Ok(Some(body_bytes(&item, span, &format!("blob.putMultipart chunk[{i}]"))?))
            }
            ChunkSource::Generator(g) => match g.resume(Value::nil()).await? {
                Some(v) => Ok(Some(body_bytes(&v, span, "blob.putMultipart generator chunk")?)),
                None => Ok(None),
            },
        }
    }
}

/// Minimal XML text escaping for the CompleteMultipartUpload part list (ETags are
/// quoted hex, but escape defensively).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    //! `sigv4_vector_battery` — every expected intermediate string is sourced from
    //! either an AWS-published worked example or, where the published *final*
    //! signature could not be cited with certainty, INDEPENDENTLY DERIVED with
    //! Python `hashlib`/`hmac` (the trusted external oracle), NEVER from this
    //! implementation's own output. Each constant notes its provenance.
    use super::sigv4::*;

    // ---- Stage-by-stage encoding edges --------------------------------------

    #[test]
    fn uri_encode_edges() {
        // Independently derived with Python urllib.parse.quote(safe='-_.~').
        assert_eq!(uri_encode_component("key with spaces"), "key%20with%20spaces");
        assert_eq!(uri_encode_component("a+b"), "a%2Bb"); // '+' is NOT a space
        assert_eq!(uri_encode_component("~"), "~"); // tilde preserved
        assert_eq!(uri_encode_component("a/b"), "a%2Fb"); // '/' encoded in a query value
        assert_eq!(uri_encode_component("a=b&c"), "a%3Db%26c");
        assert_eq!(uri_encode_component("-._~AZaz09"), "-._~AZaz09"); // unreserved set
    }

    #[test]
    fn canonical_uri_preserves_path_slashes() {
        assert_eq!(canonical_uri(""), "/");
        assert_eq!(canonical_uri("/"), "/");
        assert_eq!(canonical_uri("/test.txt"), "/test.txt");
        // segment-encoded but '/' separators preserved (single-encode, S3 rule)
        assert_eq!(canonical_uri("/a b/c+d"), "/a%20b/c%2Bd");
        assert_eq!(canonical_uri("/a/b/c"), "/a/b/c");
    }

    #[test]
    fn canonical_query_sorts_and_encodes() {
        // sorted by encoded key, space->%20, value-less param -> key=
        assert_eq!(
            canonical_query_string("Param2=value2&Param1=value1"),
            "Param1=value1&Param2=value2"
        );
        assert_eq!(canonical_query_string("a"), "a=");
        assert_eq!(canonical_query_string("k=v with space"), "k=v%20with%20space");
        // duplicate keys sorted by encoded value
        assert_eq!(canonical_query_string("k=b&k=a"), "k=a&k=b");
    }

    #[test]
    fn canonical_headers_lowercase_trim_sort() {
        let (ch, signed) = canonical_headers(&[
            ("X-Amz-Date".into(), "20150830T123600Z".into()),
            ("Host".into(), "  example.amazonaws.com  ".into()),
        ]);
        assert_eq!(
            ch,
            "host:example.amazonaws.com\nx-amz-date:20150830T123600Z\n"
        );
        assert_eq!(signed, "host;x-amz-date");
    }

    #[test]
    fn empty_payload_constant() {
        // Independently derived: sha256("") via Python hashlib.
        assert_eq!(sha256_hex(b""), EMPTY_PAYLOAD_SHA256);
    }

    // ---- Vector 1: aws-sig-v4-test-suite get-vanilla ------------------------
    // Source: AWS docs "Create a canonical request" worked example + the
    // aws-sig-v4-test-suite get-vanilla case. The canonical request, string-to-sign
    // and final signature below are the PUBLISHED AWS values, cross-checked with
    // Python hashlib/hmac.

    #[test]
    fn vector_get_vanilla() {
        let headers = vec![
            ("Host".to_string(), "example.amazonaws.com".to_string()),
            ("X-Amz-Date".to_string(), "20150830T123600Z".to_string()),
        ];
        let (ch, signed) = canonical_headers(&headers);
        assert_eq!(signed, "host;x-amz-date");
        let c_req = canonical_request(
            "GET",
            &canonical_uri("/"),
            &canonical_query_string(""),
            &ch,
            &signed,
            EMPTY_PAYLOAD_SHA256,
        );
        // Published canonical request (AWS docs).
        assert_eq!(
            c_req,
            "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\n\
             host;x-amz-date\n\
             e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let scope = credential_scope("20150830", "us-east-1", "service");
        let sts = string_to_sign("20150830T123600Z", &scope, &c_req);
        // Published string-to-sign (AWS docs); the embedded crh is the published
        // canonical-request hash bb579772...
        assert_eq!(
            sts,
            "AWS4-HMAC-SHA256\n20150830T123600Z\n\
             20150830/us-east-1/service/aws4_request\n\
             bb579772317eb040ac9ed261061d46c1f17a8133879d6129b6e1c25292927e63"
        );
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "service",
        );
        let sig = signature(&key, &sts);
        // PUBLISHED final signature for get-vanilla (AWS docs / test-suite).
        assert_eq!(sig, "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31");

        let auth = authorization_header(
            "AKIDEXAMPLE",
            &scope,
            &signed,
            &sig,
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn vector_get_vanilla_via_sign_request() {
        let headers = vec![
            ("Host".to_string(), "example.amazonaws.com".to_string()),
            ("X-Amz-Date".to_string(), "20150830T123600Z".to_string()),
        ];
        let r = sign_request(
            "GET",
            "/",
            "",
            &headers,
            EMPTY_PAYLOAD_SHA256,
            "us-east-1",
            "service",
            "20150830T123600Z",
            "20150830",
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        );
        assert_eq!(
            r.signature,
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        assert_eq!(r.signed_headers, "host;x-amz-date");
    }

    // ---- Vector 2: get-vanilla-query-order-key-case -------------------------
    // Source: aws-sig-v4-test-suite. Exercises query sort/encode. Final signature
    // is the PUBLISHED test-suite value, cross-checked with Python.

    #[test]
    fn vector_query_order() {
        let headers = vec![
            ("Host".to_string(), "example.amazonaws.com".to_string()),
            ("X-Amz-Date".to_string(), "20150830T123600Z".to_string()),
        ];
        let r = sign_request(
            "GET",
            "/",
            // input order is reversed; canonicalization must sort to Param1<Param2
            "Param2=value2&Param1=value1",
            &headers,
            EMPTY_PAYLOAD_SHA256,
            "us-east-1",
            "service",
            "20150830T123600Z",
            "20150830",
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        );
        // PUBLISHED signature for get-vanilla-query-order-key-case.
        assert_eq!(
            r.signature,
            "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"
        );
    }

    // ---- Vector 3: S3 GET object (signed-header worked example) --------------
    // Source: AWS S3 docs "Authenticating Requests: Using the Authorization Header
    // (AWS Signature Version 4)" worked example. The canonical-request HASH
    // 7344ae5b... is the PUBLISHED intermediate; the final signature below is
    // INDEPENDENTLY DERIVED with Python hashlib/hmac from that exact canonical
    // request + the documented signing-key chain (the get-vanilla vector above
    // proves the chain against a published signature).

    #[test]
    fn vector_s3_get_signed_header() {
        let headers = vec![
            ("Host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
            ("Range".to_string(), "bytes=0-9".to_string()),
            ("x-amz-content-sha256".to_string(), EMPTY_PAYLOAD_SHA256.to_string()),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
        ];
        let (ch, signed) = canonical_headers(&headers);
        assert_eq!(signed, "host;range;x-amz-content-sha256;x-amz-date");
        let c_req = canonical_request(
            "GET",
            &canonical_uri("/test.txt"),
            "",
            &ch,
            &signed,
            EMPTY_PAYLOAD_SHA256,
        );
        // The PUBLISHED canonical-request hash (AWS S3 docs).
        assert_eq!(
            sha256_hex(c_req.as_bytes()),
            "7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972"
        );
        let r = sign_request(
            "GET",
            "/test.txt",
            "",
            &headers,
            EMPTY_PAYLOAD_SHA256,
            "us-east-1",
            "s3",
            "20130524T000000Z",
            "20130524",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        );
        // INDEPENDENTLY DERIVED with Python from the published canonical request.
        assert_eq!(
            r.signature,
            "67fe34c8530db585abddc51067328adfedb6e42487d2566dc7d927d6e2722900"
        );
    }

    // ---- Vector 4: S3 presigned URL -----------------------------------------
    // Source: AWS S3 docs "Authenticating Requests: Using Query Parameters (AWS
    // Signature Version 4)" worked example (presigned GET test.txt, Expires=86400).
    // The canonical-request HASH 3bfa2928... is the PUBLISHED intermediate; the
    // signature is INDEPENDENTLY DERIVED with Python from that canonical request.

    #[test]
    fn vector_s3_presign() {
        let p = presign(
            "GET",
            "/test.txt",
            "examplebucket.s3.amazonaws.com",
            "us-east-1",
            "s3",
            "20130524T000000Z",
            "20130524",
            86400,
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            None,
            &[],
        );
        // The full presigned query: auth params (sorted/encoded) + the trailing
        // X-Amz-Signature. Independently derived with Python.
        assert_eq!(
            p.query,
            "X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20130524T000000Z\
             &X-Amz-Expires=86400\
             &X-Amz-SignedHeaders=host\
             &X-Amz-Signature=3ed0be64024db54d5574a27da223529635c383f911f80e636f0ccc13890053d2"
        );
        assert_eq!(
            p.signature,
            "3ed0be64024db54d5574a27da223529635c383f911f80e636f0ccc13890053d2"
        );
    }

    // ---- Vector 5: presigned URL WITH a session token -----------------------
    // No published vector; fully INDEPENDENTLY DERIVED with Python. Proves the
    // session token is percent-encoded (+ -> %2B, / -> %2F, = -> %3D) and folded
    // into the signed canonical query as X-Amz-Security-Token.

    #[test]
    fn vector_presign_session_token() {
        let p = presign(
            "GET",
            "/",
            "example.amazonaws.com",
            "us-east-1",
            "service",
            "20150830T123600Z",
            "20150830",
            3600,
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            Some("AQoDYXdzEXAMPLE+TOKEN/value=="),
            &[],
        );
        assert_eq!(
            p.query,
            "X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIDEXAMPLE%2F20150830%2Fus-east-1%2Fservice%2Faws4_request\
             &X-Amz-Date=20150830T123600Z\
             &X-Amz-Expires=3600\
             &X-Amz-Security-Token=AQoDYXdzEXAMPLE%2BTOKEN%2Fvalue%3D%3D\
             &X-Amz-SignedHeaders=host\
             &X-Amz-Signature=e008a345d94af6faa2e3c34fb7ce3e10efab485656c61fcd6a176404409485de"
        );
    }

    // ---- Vector 6: signed-header request WITH a session token ---------------
    // No published vector; fully INDEPENDENTLY DERIVED with Python. The session
    // token rides in x-amz-security-token and is therefore a SIGNED header.

    #[test]
    fn vector_signed_header_session_token() {
        let headers = vec![
            ("Host".to_string(), "example.amazonaws.com".to_string()),
            ("x-amz-date".to_string(), "20150830T123600Z".to_string()),
            (
                "x-amz-security-token".to_string(),
                "AQoDYXdzEXAMPLE+TOKEN/value==".to_string(),
            ),
        ];
        let r = sign_request(
            "GET",
            "/",
            "",
            &headers,
            EMPTY_PAYLOAD_SHA256,
            "us-east-1",
            "service",
            "20150830T123600Z",
            "20150830",
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        );
        assert_eq!(r.signed_headers, "host;x-amz-date;x-amz-security-token");
        assert_eq!(
            r.signature,
            "53c0e5feb8c030f7c2c453c0a5a2bd20e9ffb9f5836ede560573547881b2cf93"
        );
        assert_eq!(
            r.authorization,
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date;x-amz-security-token, \
             Signature=53c0e5feb8c030f7c2c453c0a5a2bd20e9ffb9f5836ede560573547881b2cf93"
        );
    }
}
