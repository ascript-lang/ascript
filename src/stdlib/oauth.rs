//! `std/oauth` — OAuth2 + PKCE (feature `auth`, BATT §5.6).
//!
//! A small, focused OAuth2 client over the SAME pooled reqwest client as
//! `std/net/http` (`net_http::shared_client()`) — there is NO second HTTP stack
//! (the ai/telemetry ungated-egress lesson). The whole module is `Net`-gated at
//! the dispatch chokepoint (`required_cap("oauth", _) == Net`), so `--deny net` /
//! `--sandbox` / `run_in_worker({deny net})` blocks every token call.
//!
//! ## Surface
//!
//! - `oauth.pkce()` → `{verifier, challenge, method:"S256"}`. The verifier is a
//!   high-entropy base64url string (RFC 7636 §4.1 — 32 random bytes → 43 chars);
//!   the challenge is `base64url(sha256(verifier))` (S256). The RNG routes through
//!   the SP9 determinism seam (`fill_seeded_bytes`) so it is reproducible under
//!   Record/Replay, exactly like `crypto.randomBytes`/`uuid.v4`.
//! - `oauth.exchangeCode({tokenUrl, code, codeVerifier, clientId, clientSecret?,
//!   redirectUri?})` → `[tokens, err]`. POSTs the
//!   `grant_type=authorization_code` form; a `clientSecret` selects HTTP Basic
//!   client authentication (RFC 6749 §2.3.1), else the `client_id` rides in the
//!   form (public client).
//! - `oauth.clientCredentials({tokenUrl, clientId, clientSecret, scope?})` →
//!   `[tokens, err]` (`grant_type=client_credentials`, Basic auth).
//! - `oauth.refresh({tokenUrl, refreshToken, clientId, clientSecret?})` →
//!   `[tokens, err]` (`grant_type=refresh_token`).
//! - `oauth.discover(issuer)` → `[metadata, err]`. GETs
//!   `<issuer>/.well-known/openid-configuration` and parses the JSON metadata.
//!
//! A non-2xx token response is a Tier-1 `[nil, err]` whose error carries the
//! response BODY (so the caller sees the provider's `{error, error_description}`).
//! A wrong argument TYPE (a non-object opts, a non-string field) is a Tier-2 panic
//! (a programming error), mirroring the rest of the stdlib.

use super::arg;
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp};
use crate::span::Span;
use crate::value::{Value, ValueKind};
use base64::Engine;
use indexmap::IndexMap;
use sha2::{Digest, Sha256};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("pkce", super::bi("oauth.pkce")),
        ("exchangeCode", super::bi("oauth.exchangeCode")),
        ("clientCredentials", super::bi("oauth.clientCredentials")),
        ("refresh", super::bi("oauth.refresh")),
        ("discover", super::bi("oauth.discover")),
    ]
}

fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// Build a Tier-1 `[nil, {message}]` error pair.
fn tier1(msg: impl Into<String>) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg.into())))
}

/// Read a required string field from an opts object. A missing/empty/non-string
/// field is a Tier-2 panic (a programming error) naming the field.
fn req_field(opts: &Value, field: &str, who: &str, span: Span) -> Result<String, Control> {
    let ValueKind::Object(o) = opts.kind() else {
        return Err(AsError::at(format!("{who}: expected an options object"), span).into());
    };
    match o.get(field).as_ref().map(|v| v.kind()) {
        Some(ValueKind::Str(s)) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(AsError::at(
            format!("{who}: '{field}' is required and must be a non-empty string"),
            span,
        )
        .into()),
    }
}

/// Read an optional string field (None for absent / nil / non-string).
fn opt_field(opts: &Value, field: &str) -> Option<String> {
    match opts.kind() {
        ValueKind::Object(o) => match o.get(field).as_ref().map(|v| v.kind()) {
            Some(ValueKind::Str(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

impl Interp {
    pub(crate) async fn call_oauth(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "pkce" => Ok(self.oauth_pkce()),
            "exchangeCode" => self.oauth_exchange_code(args, span).await,
            "clientCredentials" => self.oauth_client_credentials(args, span).await,
            "refresh" => self.oauth_refresh(args, span).await,
            "discover" => self.oauth_discover(args, span).await,
            _ => Err(AsError::at(format!("std/oauth has no function '{func}'"), span).into()),
        }
    }

    /// `oauth.pkce()` → `{verifier, challenge, method:"S256"}` (RFC 7636).
    fn oauth_pkce(&self) -> Value {
        // 32 random bytes → 43-char base64url verifier (RFC 7636 §4.1 recommends
        // 32 octets). Route through the determinism seam so a workflow/replay run
        // reproduces it (like crypto.randomBytes / uuid.v4).
        let mut buf = [0u8; 32];
        if !self.fill_seeded_bytes(&mut buf) {
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut buf);
        }
        let verifier = b64url(&buf);
        let challenge = b64url(Sha256::digest(verifier.as_bytes()).as_slice());
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("verifier".to_string(), Value::str(verifier));
        m.insert("challenge".to_string(), Value::str(challenge));
        m.insert("method".to_string(), Value::str("S256"));
        Value::object(m)
    }

    async fn oauth_exchange_code(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let opts = arg(args, 0);
        let who = "oauth.exchangeCode";
        let token_url = req_field(&opts, "tokenUrl", who, span)?;
        let code = req_field(&opts, "code", who, span)?;
        let code_verifier = req_field(&opts, "codeVerifier", who, span)?;
        let client_id = req_field(&opts, "clientId", who, span)?;
        let client_secret = opt_field(&opts, "clientSecret");
        let redirect_uri = opt_field(&opts, "redirectUri");

        let mut form: Vec<(String, String)> = vec![
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".to_string(), code),
            ("code_verifier".to_string(), code_verifier),
        ];
        if let Some(uri) = redirect_uri {
            form.push(("redirect_uri".to_string(), uri));
        }
        // A confidential client uses Basic auth; a public client carries client_id
        // in the form (RFC 6749 §2.3.1 / §4.1.3).
        if client_secret.is_none() {
            form.push(("client_id".to_string(), client_id.clone()));
        }
        self.oauth_token_request(&token_url, form, &client_id, client_secret.as_deref(), span)
            .await
    }

    async fn oauth_client_credentials(
        &self,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let opts = arg(args, 0);
        let who = "oauth.clientCredentials";
        let token_url = req_field(&opts, "tokenUrl", who, span)?;
        let client_id = req_field(&opts, "clientId", who, span)?;
        let client_secret = req_field(&opts, "clientSecret", who, span)?;
        let scope = opt_field(&opts, "scope");

        let mut form: Vec<(String, String)> =
            vec![("grant_type".to_string(), "client_credentials".to_string())];
        if let Some(scope) = scope {
            form.push(("scope".to_string(), scope));
        }
        self.oauth_token_request(&token_url, form, &client_id, Some(&client_secret), span)
            .await
    }

    async fn oauth_refresh(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let opts = arg(args, 0);
        let who = "oauth.refresh";
        let token_url = req_field(&opts, "tokenUrl", who, span)?;
        let refresh_token = req_field(&opts, "refreshToken", who, span)?;
        let client_id = req_field(&opts, "clientId", who, span)?;
        let client_secret = opt_field(&opts, "clientSecret");

        let mut form: Vec<(String, String)> = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), refresh_token),
        ];
        if client_secret.is_none() {
            form.push(("client_id".to_string(), client_id.clone()));
        }
        self.oauth_token_request(&token_url, form, &client_id, client_secret.as_deref(), span)
            .await
    }

    /// POST a form-encoded token request over the SHARED pooled client. A
    /// `client_secret` → HTTP Basic client auth. Non-2xx → Tier-1 carrying the
    /// response body. A 2xx → the parsed JSON token object as `[tokens, nil]`.
    async fn oauth_token_request(
        &self,
        token_url: &str,
        form: Vec<(String, String)>,
        client_id: &str,
        client_secret: Option<&str>,
        _span: Span,
    ) -> Result<Value, Control> {
        let client = crate::stdlib::net_http::shared_client();
        let mut rb = client
            .post(token_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&form);
        if let Some(secret) = client_secret {
            rb = rb.basic_auth(client_id, Some(secret));
        }
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => return Ok(tier1(format!("oauth token request failed: {e}"))),
        };
        let status = resp.status();
        let body = match resp.text().await {
            Ok(t) => t,
            Err(e) => return Ok(tier1(format!("oauth token response read failed: {e}"))),
        };
        if !status.is_success() {
            // Carry the provider's error BODY so the caller sees
            // {error, error_description}.
            return Ok(tier1(format!(
                "oauth token endpoint returned {}: {}",
                status.as_u16(),
                body
            )));
        }
        match parse_json(&body) {
            Ok(v) => Ok(make_pair(v, Value::nil())),
            Err(e) => Ok(tier1(format!("oauth token response is not valid JSON: {e}"))),
        }
    }

    /// `oauth.discover(issuer)` → GET `<issuer>/.well-known/openid-configuration`.
    async fn oauth_discover(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let issuer = match arg(args, 0).kind() {
            ValueKind::Str(s) if !s.is_empty() => s.to_string(),
            _ => {
                return Err(AsError::at(
                    "oauth.discover: issuer must be a non-empty string",
                    span,
                )
                .into())
            }
        };
        // Append the well-known path, tolerating a trailing slash on the issuer.
        let base = issuer.trim_end_matches('/');
        let url = format!("{base}/.well-known/openid-configuration");
        let client = crate::stdlib::net_http::shared_client();
        let resp = match client
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(tier1(format!("oauth discovery failed: {e}"))),
        };
        let status = resp.status();
        let body = match resp.text().await {
            Ok(t) => t,
            Err(e) => return Ok(tier1(format!("oauth discovery read failed: {e}"))),
        };
        if !status.is_success() {
            return Ok(tier1(format!(
                "oauth discovery returned {}: {}",
                status.as_u16(),
                body
            )));
        }
        match parse_json(&body) {
            Ok(v) => Ok(make_pair(v, Value::nil())),
            Err(e) => Ok(tier1(format!("oauth discovery metadata is not valid JSON: {e}"))),
        }
    }
}

/// Parse a JSON string into an AScript Value.
fn parse_json(text: &str) -> Result<Value, String> {
    let jv: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid json: {e}"))?;
    Ok(crate::stdlib::json::to_ascript(&jv))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "auth"))]
mod tests {
    use super::*;

    fn ip() -> Interp {
        Interp::new()
    }

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn get_field(v: &Value, k: &str) -> Option<String> {
        match v.kind() {
            ValueKind::Object(o) => match o.get(k).as_ref().map(|x| x.kind()) {
                Some(ValueKind::Str(s)) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn pair(v: &Value) -> (Value, Value) {
        match v.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                (b[0].clone(), b[1].clone())
            }
            _ => panic!("expected a [value, err] pair"),
        }
    }

    fn err_msg(v: &Value) -> String {
        let (_, err) = pair(v);
        match err.kind() {
            ValueKind::Object(o) => match o.get("message").as_ref().map(|m| m.kind()) {
                Some(ValueKind::Str(s)) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    // (g) PKCE — the RFC 7636 Appendix B S256 vector. We can't pin the random
    // verifier, but we CAN pin that challenge = base64url(sha256(verifier)) for the
    // RFC's fixed verifier, plus the structural invariants of a fresh pkce().
    #[test]
    fn pkce_rfc7636_appendix_b_vector() {
        // RFC 7636 Appendix B: verifier → challenge (S256).
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = b64url(Sha256::digest(verifier.as_bytes()).as_slice());
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn pkce_shape_and_self_consistency() {
        let interp = ip();
        let p = interp.oauth_pkce();
        assert_eq!(get_field(&p, "method").as_deref(), Some("S256"));
        let verifier = get_field(&p, "verifier").expect("verifier");
        let challenge = get_field(&p, "challenge").expect("challenge");
        // 32 random bytes → 43-char base64url (no padding).
        assert_eq!(verifier.len(), 43, "verifier should be 43 base64url chars");
        assert!(!verifier.contains('='), "no padding");
        assert!(!verifier.contains('+') && !verifier.contains('/'), "url-safe alphabet");
        // The challenge MUST be base64url(sha256(verifier)) — the S256 transform.
        let expect = b64url(Sha256::digest(verifier.as_bytes()).as_slice());
        assert_eq!(challenge, expect, "challenge must be S256(verifier)");
        // Two calls produce DIFFERENT verifiers (entropy).
        let p2 = interp.oauth_pkce();
        assert_ne!(
            get_field(&p2, "verifier"),
            Some(verifier),
            "successive pkce() must differ"
        );
    }

    // Wrong arg types → Tier-2 (programming error), not a Tier-1 result.
    #[tokio::test]
    async fn token_calls_wrong_args_are_tier2() {
        let interp = ip();
        // missing tokenUrl
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("code".to_string(), Value::str("c"));
        let opts = Value::object(m);
        assert!(interp
            .oauth_exchange_code(&[opts], sp())
            .await
            .is_err());
        // non-object opts
        assert!(interp
            .oauth_client_credentials(&[Value::int(1)], sp())
            .await
            .is_err());
        // discover with a non-string issuer
        assert!(interp.oauth_discover(&[Value::int(1)], sp()).await.is_err());
    }

    // (f) exchangeCode/clientCredentials/refresh/discover over the in-process stub.
    // The stub asserts the form body + Authorization header for exchangeCode, and a
    // non-200 returns a Tier-1 carrying the error body. discover parses the metadata.
    #[tokio::test]
    async fn oauth_end_to_end_against_stub() {
        let base = fixture::start().await;
        let interp = ip();

        // exchangeCode (confidential client → Basic auth + form body assertion).
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("tokenUrl".to_string(), Value::str(format!("{base}/token")));
        m.insert("code".to_string(), Value::str("the-auth-code"));
        m.insert("codeVerifier".to_string(), Value::str("the-verifier"));
        m.insert("clientId".to_string(), Value::str("client-123"));
        m.insert("clientSecret".to_string(), Value::str("s3cr3t"));
        m.insert("redirectUri".to_string(), Value::str("https://app/cb"));
        let r = interp.oauth_exchange_code(&[Value::object(m)], sp()).await.unwrap();
        let (tokens, err) = pair(&r);
        assert!(matches!(err.kind(), ValueKind::Nil), "exchangeCode err: {}", err_msg(&r));
        assert_eq!(get_field(&tokens, "access_token").as_deref(), Some("AT-OK"));

        // clientCredentials.
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("tokenUrl".to_string(), Value::str(format!("{base}/token")));
        m.insert("clientId".to_string(), Value::str("client-123"));
        m.insert("clientSecret".to_string(), Value::str("s3cr3t"));
        m.insert("scope".to_string(), Value::str("read write"));
        let r = interp.oauth_client_credentials(&[Value::object(m)], sp()).await.unwrap();
        let (tokens, err) = pair(&r);
        assert!(matches!(err.kind(), ValueKind::Nil));
        assert_eq!(get_field(&tokens, "access_token").as_deref(), Some("AT-OK"));

        // refresh.
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("tokenUrl".to_string(), Value::str(format!("{base}/token")));
        m.insert("refreshToken".to_string(), Value::str("RT-1"));
        m.insert("clientId".to_string(), Value::str("client-123"));
        let r = interp.oauth_refresh(&[Value::object(m)], sp()).await.unwrap();
        let (_, err) = pair(&r);
        assert!(matches!(err.kind(), ValueKind::Nil));

        // non-200 → Tier-1 carrying the error body.
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("tokenUrl".to_string(), Value::str(format!("{base}/token-error")));
        m.insert("code".to_string(), Value::str("bad"));
        m.insert("codeVerifier".to_string(), Value::str("v"));
        m.insert("clientId".to_string(), Value::str("client-123"));
        let r = interp.oauth_exchange_code(&[Value::object(m)], sp()).await.unwrap();
        let (val, _) = pair(&r);
        assert!(matches!(val.kind(), ValueKind::Nil), "non-200 must be Tier-1 nil");
        assert!(
            err_msg(&r).contains("invalid_grant"),
            "error must carry the provider body, got: {}",
            err_msg(&r)
        );

        // discover.
        let r = interp
            .oauth_discover(&[Value::str(base.clone())], sp())
            .await
            .unwrap();
        let (md, err) = pair(&r);
        assert!(matches!(err.kind(), ValueKind::Nil), "discover err: {}", err_msg(&r));
        assert_eq!(
            get_field(&md, "token_endpoint").as_deref(),
            Some("https://issuer/token")
        );
    }

    // ---- in-process OAuth token/discovery stub (hyper 1.x) ------------------
    mod fixture {
        use http_body_util::combinators::BoxBody;
        use http_body_util::{BodyExt, Full};
        use hyper::body::{Bytes, Incoming};
        use hyper::service::service_fn;
        use hyper::{Request, Response, StatusCode};
        use hyper_util::rt::TokioIo;
        use std::convert::Infallible;
        use tokio::net::TcpListener;

        async fn handle(
            req: Request<Incoming>,
        ) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
            let path = req.uri().path().to_string();
            let auth = req
                .headers()
                .get(hyper::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body = req
                .into_body()
                .collect()
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body).to_string();

            let resp = match path.as_str() {
                "/token" => {
                    // Assert the form body + Basic auth (when present).
                    assert!(
                        body_str.contains("grant_type="),
                        "token request must be form-encoded, got: {body_str}"
                    );
                    // exchangeCode body shape.
                    if body_str.contains("grant_type=authorization_code") {
                        assert!(body_str.contains("code=the-auth-code"), "code: {body_str}");
                        assert!(
                            body_str.contains("code_verifier=the-verifier"),
                            "code_verifier: {body_str}"
                        );
                        assert!(
                            auth.starts_with("Basic "),
                            "confidential client must send Basic auth, got: {auth:?}"
                        );
                    }
                    let mut r = Response::new(Full::new(Bytes::from_static(
                        b"{\"access_token\":\"AT-OK\",\"token_type\":\"Bearer\",\"expires_in\":3600}",
                    )));
                    r.headers_mut().insert(
                        hyper::header::CONTENT_TYPE,
                        "application/json".parse().unwrap(),
                    );
                    r
                }
                "/token-error" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(
                        b"{\"error\":\"invalid_grant\",\"error_description\":\"bad code\"}",
                    )));
                    *r.status_mut() = StatusCode::BAD_REQUEST;
                    r
                }
                "/.well-known/openid-configuration" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(
                        b"{\"issuer\":\"https://issuer\",\"token_endpoint\":\"https://issuer/token\",\"authorization_endpoint\":\"https://issuer/authorize\"}",
                    )));
                    r.headers_mut().insert(
                        hyper::header::CONTENT_TYPE,
                        "application/json".parse().unwrap(),
                    );
                    r
                }
                _ => {
                    let mut r = Response::new(Full::new(Bytes::from_static(b"nope")));
                    *r.status_mut() = StatusCode::NOT_FOUND;
                    r
                }
            };
            let (parts, body) = resp.into_parts();
            Ok(Response::from_parts(parts, BoxBody::new(body)))
        }

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
                        let svc = service_fn(handle);
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, svc)
                            .await;
                    });
                }
            });
            format!("http://127.0.0.1:{}", addr.port())
        }
    }
}
