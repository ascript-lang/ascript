//! `std/jwt` — JSON Web Tokens (feature `auth`). Typed keys kill alg-confusion
//! structurally (spec §5.3): a key is a tagged Object {__jwtkey: kind, ...} and
//! verify intersects the header alg with the key kind's algorithm set; alg:"none"
//! is rejected before dispatch. Verification failure is Tier-1 [nil, err] ALWAYS
//! (auth failures are control flow); a non-key where a key is due is Tier-2.
//!
//! A5 wires the `hmac` key kind (HS256/HS384/HS512 over `hmac` + `sha2`); A6 fills
//! the `rsa-*` (RS256, `rsa` crate) and `ec-*` (ES256, `p256` crate) arms — purely
//! additive, never re-touching the alg-intersection logic.
//!
//! ## Asymmetric keys (A6)
//!
//! `jwt.rsaPublicKey`/`rsaPrivateKey`/`ecPublicKey`/`ecPrivateKey` each take a PEM
//! string, VALIDATE it at construction (a bad/wrong-kind PEM is a Tier-1 error),
//! and STORE the PEM TEXT in the tagged Object — the key is re-parsed per
//! sign/verify op. Keys aren't a hot path; storing the PEM (not an opaque native
//! handle) keeps a key SENDABLE across the worker airlock and PRINTABLE-safe: the
//! `__jwtkey` tag shows the kind while the key material stays an ordinary string
//! field. (Treat the PEM string as you would any secret — it is the key material.)
//!
//! ## ES256 JOSE encoding (the security pin)
//!
//! The ECDSA signature is the FIXED-WIDTH 64-byte `r||s` concatenation (JOSE / RFC
//! 7518 §3.4), NOT ASN.1/DER. Sign uses `Signature::to_bytes()`; verify uses
//! `Signature::from_slice` (which is fixed-width and rejects a DER `0x30…` blob by
//! construction). The `from_der` path is NEVER used — a DER sig must fail verify.
//!
//! ## Regenerating the test fixtures (`testdata/jwt_{rsa,ec}_{priv,pub}.pem`)
//!
//! ```sh
//! openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
//!     -out src/stdlib/testdata/jwt_rsa_priv.pem
//! openssl pkey -in src/stdlib/testdata/jwt_rsa_priv.pem -pubout \
//!     -out src/stdlib/testdata/jwt_rsa_pub.pem
//! openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 \
//!     -out src/stdlib/testdata/jwt_ec_priv.pem
//! openssl pkey -in src/stdlib/testdata/jwt_ec_priv.pem -pubout \
//!     -out src/stdlib/testdata/jwt_ec_pub.pem
//! ```

use super::{arg, bi, want_object};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp};
use crate::span::Span;
use crate::value::{Value, ValueKind};
use base64::Engine;
use hmac::{Hmac, Mac};
use indexmap::IndexMap;
use sha2::{Sha256, Sha384, Sha512};

/// The tag field that marks an Object as a typed JWT key (§5.3).
const KEY_TAG: &str = "__jwtkey";

/// Per-segment base64url size cap (alloc bound). A compact JWT's three segments
/// are each base64url; a hostile token can carry an arbitrarily long segment to
/// force a huge allocation on decode. We reject `s.len() > MAX_SEGMENT` BEFORE
/// decoding. 1 MiB per segment is far beyond any legitimate JWT (claims rarely
/// exceed a few KiB) yet bounds the worst case.
const MAX_SEGMENT: usize = 1024 * 1024;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("hmacKey", bi("jwt.hmacKey")),
        ("rsaPublicKey", bi("jwt.rsaPublicKey")),
        ("rsaPrivateKey", bi("jwt.rsaPrivateKey")),
        ("ecPublicKey", bi("jwt.ecPublicKey")),
        ("ecPrivateKey", bi("jwt.ecPrivateKey")),
        ("sign", bi("jwt.sign")),
        ("verify", bi("jwt.verify")),
        ("decode", bi("jwt.decode")),
        ("jwks", bi("jwt.jwks")),
    ]
}

// ── base64url helpers ────────────────────────────────────────────────────────

fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// Decode a base64url segment, rejecting an over-long input BEFORE allocating the
/// decode buffer. Returns the decode error as a string (caller maps to Tier-1).
fn b64url_decode(s: &str, max: usize) -> Result<Vec<u8>, String> {
    if s.len() > max {
        return Err(format!("segment too large ({} > {} bytes)", s.len(), max));
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.as_bytes())
        .map_err(|e| format!("invalid base64url: {e}"))
}

// ── typed keys (§5.3) ────────────────────────────────────────────────────────

/// The algorithm set a key KIND can ever produce/verify. The intersection of
/// THIS set with the token's header `alg` (and the caller's `algs` allowlist) is
/// what makes alg-confusion unrepresentable: an HMAC key can only HS-verify, and
/// (A6) an RSA/EC public key can never HMAC-verify.
///
/// Only `hmac` is wired in A5; the `rsa-*`/`ec-*` arms are present so A6 is
/// additive (it fills the verify dispatch, not this table).
fn algs_for_key_kind(kind: &str) -> &'static [&'static str] {
    match kind {
        "hmac" => &["HS256", "HS384", "HS512"],
        "rsa-public" | "rsa-private" => &["RS256"],
        "ec-public" | "ec-private" => &["ES256"],
        _ => &[],
    }
}

/// Extract the `__jwtkey` kind tag from a candidate key value, or `None` if it is
/// not a tagged key Object.
fn key_kind(v: &Value) -> Option<String> {
    match v.kind() {
        ValueKind::Object(o) => match o.get(KEY_TAG).as_ref().map(|t| t.kind()) {
            Some(ValueKind::Str(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Read the `secret` field of an hmac key as raw bytes (string → UTF-8, bytes →
/// raw). Returns `None` for a malformed key (no/ wrong-typed secret).
fn hmac_secret(key: &Value) -> Option<Vec<u8>> {
    let ValueKind::Object(o) = key.kind() else {
        return None;
    };
    match o.get("secret").as_ref().map(|s| s.kind()) {
        Some(ValueKind::Str(s)) => Some(s.as_bytes().to_vec()),
        Some(ValueKind::Bytes(b)) => Some(b.borrow().clone()),
        _ => None,
    }
}

// ── HMAC over the signing input ──────────────────────────────────────────────

/// Compute the raw HMAC tag for `alg` over `signing_input` with `secret`.
/// `None` for a non-HS algorithm (caller has already intersected, so this is a
/// defensive guard).
fn hmac_sign(alg: &str, secret: &[u8], signing_input: &[u8]) -> Option<Vec<u8>> {
    match alg {
        "HS256" => {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac any key len");
            mac.update(signing_input);
            Some(mac.finalize().into_bytes().to_vec())
        }
        "HS384" => {
            let mut mac = Hmac::<Sha384>::new_from_slice(secret).expect("hmac any key len");
            mac.update(signing_input);
            Some(mac.finalize().into_bytes().to_vec())
        }
        "HS512" => {
            let mut mac = Hmac::<Sha512>::new_from_slice(secret).expect("hmac any key len");
            mac.update(signing_input);
            Some(mac.finalize().into_bytes().to_vec())
        }
        _ => None,
    }
}

/// Constant-time verify of `sig` against the HMAC of `signing_input`. Uses the
/// crate's `Mac::verify_slice` (constant-time) — NEVER `==` on raw sig bytes.
/// Returns `Ok(())` on a valid tag, `Err(())` on a bad tag or non-HS alg.
fn hmac_verify(alg: &str, secret: &[u8], signing_input: &[u8], sig: &[u8]) -> Result<(), ()> {
    match alg {
        "HS256" => {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac any key len");
            mac.update(signing_input);
            mac.verify_slice(sig).map_err(|_| ())
        }
        "HS384" => {
            let mut mac = Hmac::<Sha384>::new_from_slice(secret).expect("hmac any key len");
            mac.update(signing_input);
            mac.verify_slice(sig).map_err(|_| ())
        }
        "HS512" => {
            let mut mac = Hmac::<Sha512>::new_from_slice(secret).expect("hmac any key len");
            mac.update(signing_input);
            mac.verify_slice(sig).map_err(|_| ())
        }
        _ => Err(()),
    }
}

// ── JSON serialization (byte-exact, insertion-order) ─────────────────────────

/// Serialize an AScript Object/value to a compact JSON string (insertion order;
/// `serde_json` is built with `preserve_order`). Errors → a string.
fn json_compact(v: &Value) -> Result<String, String> {
    let jv = crate::stdlib::json::from_ascript(v, &mut Vec::new())?;
    serde_json::to_string(&jv).map_err(|e| format!("cannot serialize: {e}"))
}

/// Parse a JSON byte slice into an AScript Value. Errors → a string.
fn json_parse(bytes: &[u8]) -> Result<Value, String> {
    let jv: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid json: {e}"))?;
    Ok(crate::stdlib::json::to_ascript(&jv))
}

/// `alg: "none"` (any casing) is rejected unconditionally, BEFORE key dispatch.
fn is_none_alg(alg: &str) -> bool {
    alg.eq_ignore_ascii_case("none")
}

// ── dispatch ─────────────────────────────────────────────────────────────────

impl Interp {
    /// Sync jwt dispatch (the pure-crypto funcs). `jwt.jwks` is async (network) and
    /// routed separately via [`Interp::call_jwt_async`].
    pub(crate) fn call_jwt(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "hmacKey" => jwt_hmac_key(args, span),
            "rsaPublicKey" => jwt_rsa_public_key(args, span),
            "rsaPrivateKey" => jwt_rsa_private_key(args, span),
            "ecPublicKey" => jwt_ec_public_key(args, span),
            "ecPrivateKey" => jwt_ec_private_key(args, span),
            "sign" => self.jwt_sign(args, span),
            "verify" => self.jwt_verify(args, span),
            "decode" => jwt_decode(args, span),
            _ => Err(AsError::at(format!("std/jwt has no function '{func}'"), span).into()),
        }
    }

    /// Async jwt dispatch: `jwt.jwks` (the ONLY network-touching jwt func, Net-gated
    /// at the dispatch chokepoint) routes here; everything else delegates to the sync
    /// [`Interp::call_jwt`].
    pub(crate) async fn call_jwt_async(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "jwks" => self.jwt_jwks(args, span).await,
            _ => self.call_jwt(func, args, span),
        }
    }

    /// `jwt.sign(claims, key, opts?) -> [token, err]`.
    fn jwt_sign(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        // Wrong arg TYPE is Tier-2 (a programming error), not an auth failure.
        let claims = want_object(&arg(args, 0), span, "jwt.sign claims")?;
        let key = arg(args, 1);
        let Some(kind) = key_kind(&key) else {
            return Err(AsError::at(
                "jwt.sign: key must be a jwt key (use jwt.hmacKey), got a plain value",
                span,
            )
            .into());
        };
        let opts = arg(args, 2);

        // Algorithm: opts.alg or a kind-appropriate default; MUST be in the key
        // kind's set. The default is HS256 for hmac, else the kind's sole alg —
        // an asymmetric private key signs with its only algorithm by default.
        let alg = opt_str(&opts, "alg").unwrap_or_else(|| match kind.as_str() {
            "hmac" => "HS256".to_string(),
            _ => algs_for_key_kind(&kind)
                .first()
                .map(|a| a.to_string())
                .unwrap_or_default(),
        });
        if !algs_for_key_kind(&kind).contains(&alg.as_str()) {
            return Ok(tier1(format!(
                "alg '{alg}' is not valid for a '{kind}' key"
            )));
        }

        // A public key cannot sign — only the hmac secret and the *private*
        // asymmetric kinds reach the signing path.
        if kind == "rsa-public" || kind == "ec-public" {
            return Ok(tier1(format!(
                "jwt.sign: a '{kind}' key cannot sign — use the matching private key"
            )));
        }

        // Header: {alg, typ:"JWT"} merged with opts.headers (caller headers do
        // NOT override alg/typ — the protected alg is authoritative).
        let mut header: IndexMap<String, Value> = IndexMap::new();
        header.insert("alg".to_string(), Value::str(alg.clone()));
        header.insert("typ".to_string(), Value::str("JWT"));
        if let ValueKind::Object(h) = opts.kind() {
            if let Some(extra) = h.get("headers") {
                if let ValueKind::Object(eo) = extra.kind() {
                    eo.for_each(|k, v| {
                        if k != "alg" && k != "typ" {
                            header.insert(k.to_string(), v.clone());
                        }
                    });
                }
            }
        }

        // Claims: copy, then apply opts.expiresIn → exp.
        let mut claims_map: IndexMap<String, Value> = IndexMap::new();
        claims.for_each(|k, v| {
            claims_map.insert(k.to_string(), v.clone());
        });
        if let Some(secs) = opt_num(&opts, "expiresIn") {
            let now_s = (self.clock_now_ms() / 1000.0).floor();
            claims_map.insert("exp".to_string(), Value::int((now_s + secs) as i64));
        }

        let header_v = Value::object(header);
        let claims_v = Value::object(claims_map);
        let header_json = match json_compact(&header_v) {
            Ok(s) => s,
            Err(e) => return Ok(tier1(format!("cannot serialize header: {e}"))),
        };
        let claims_json = match json_compact(&claims_v) {
            Ok(s) => s,
            Err(e) => return Ok(tier1(format!("cannot serialize claims: {e}"))),
        };

        let signing_input = format!(
            "{}.{}",
            b64url(header_json.as_bytes()),
            b64url(claims_json.as_bytes())
        );

        // Dispatch the signature by key kind. The alg-intersection above already
        // proved `alg` ∈ algs_for_key_kind(kind), so each arm sees only its algs.
        let sig: Vec<u8> = match kind.as_str() {
            "hmac" => {
                let Some(secret) = hmac_secret(&key) else {
                    return Err(AsError::at(
                        "jwt.sign: hmac key is missing a valid 'secret' field",
                        span,
                    )
                    .into());
                };
                match hmac_sign(&alg, &secret, signing_input.as_bytes()) {
                    Some(s) => s,
                    None => return Ok(tier1(format!("alg '{alg}' is not an hmac algorithm"))),
                }
            }
            "rsa-private" => {
                let Some(pem) = key_pem(&key) else {
                    return Err(AsError::at(
                        "jwt.sign: rsa key is missing a valid 'pem' field",
                        span,
                    )
                    .into());
                };
                match rs256_sign(&pem, signing_input.as_bytes()) {
                    Ok(s) => s,
                    Err(e) => return Ok(tier1(format!("rsa sign failed: {e}"))),
                }
            }
            "ec-private" => {
                let Some(pem) = key_pem(&key) else {
                    return Err(AsError::at(
                        "jwt.sign: ec key is missing a valid 'pem' field",
                        span,
                    )
                    .into());
                };
                match es256_sign(&pem, signing_input.as_bytes()) {
                    Ok(s) => s,
                    Err(e) => return Ok(tier1(format!("ec sign failed: {e}"))),
                }
            }
            other => {
                return Ok(tier1(format!("jwt.sign: unsupported key kind '{other}'")));
            }
        };

        let token = format!("{}.{}", signing_input, b64url(&sig));
        Ok(make_pair(Value::str(token), Value::nil()))
    }

    /// `jwt.verify(token, key, opts?) -> [claims, err]`. Auth failure is ALWAYS
    /// Tier-1 [nil, err]. Signature authenticity is checked BEFORE any claim
    /// (exp/nbf/iss/aud) so a failing claim never leaks before the token is
    /// proven authentic.
    fn jwt_verify(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let token = match arg(args, 0).kind() {
            ValueKind::Str(s) => s.to_string(),
            other => {
                return Err(AsError::at(
                    format!(
                        "jwt.verify: token must be a string, got {}",
                        kind_name(&other)
                    ),
                    span,
                )
                .into())
            }
        };
        let key = arg(args, 1);
        let Some(kind) = key_kind(&key) else {
            return Err(AsError::at(
                "jwt.verify: key must be a jwt key (use jwt.hmacKey), got a plain value",
                span,
            )
            .into());
        };
        let opts = arg(args, 2);

        // 1. Split: exactly three non-empty-delimited parts.
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Ok(tier1("malformed token: expected 3 dot-separated segments"));
        }
        let (h_b64, p_b64, s_b64) = (parts[0], parts[1], parts[2]);

        // 2. Parse the header (alloc-bounded decode).
        let header_bytes = match b64url_decode(h_b64, MAX_SEGMENT) {
            Ok(b) => b,
            Err(e) => return Ok(tier1(format!("malformed header: {e}"))),
        };
        let header = match json_parse(&header_bytes) {
            Ok(v) => v,
            Err(e) => return Ok(tier1(format!("malformed header: {e}"))),
        };
        let alg = match header_field_str(&header, "alg") {
            Some(a) => a,
            None => return Ok(tier1("header is missing 'alg'")),
        };

        // 3. Reject alg:"none" (any casing) BEFORE key dispatch — the canonical
        //    alg-confusion / signature-stripping bypass.
        if is_none_alg(&alg) {
            return Ok(tier1("alg 'none' is rejected"));
        }

        // 4. Intersection (the structural alg-confusion defense): the alg MUST be
        //    in the key kind's algorithm set AND in the caller's allowlist (if
        //    one is given). `jku`/`jwk`/`kid` header fields are IGNORED — the key
        //    comes ONLY from the caller's `key` argument.
        let kind_algs = algs_for_key_kind(&kind);
        if !kind_algs.contains(&alg.as_str()) {
            return Ok(tier1(format!(
                "alg '{alg}' is not allowed for a '{kind}' key"
            )));
        }
        if let Some(allow) = opt_str_array(&opts, "algs") {
            if !allow.iter().any(|a| a == &alg) {
                return Ok(tier1(format!("alg '{alg}' is not in the allowlist")));
            }
        }

        // 5. Decode the signature (alloc-bounded).
        let sig = match b64url_decode(s_b64, MAX_SEGMENT) {
            Ok(b) => b,
            Err(e) => return Ok(tier1(format!("malformed signature: {e}"))),
        };

        // 6. Recompute + verify over the EXACT header.payload bytes. Dispatch by
        //    key kind; the intersection above guarantees alg ∈ kind's algs, so the
        //    hmac path is UNREACHABLE for an rsa/ec key (the structural kill).
        let signing_input = format!("{h_b64}.{p_b64}");
        let sig_ok = match kind.as_str() {
            "hmac" => {
                // CONSTANT-TIME (the MAC's verify_slice) — never `==` on raw bytes.
                let Some(secret) = hmac_secret(&key) else {
                    return Err(AsError::at(
                        "jwt.verify: hmac key is missing a valid 'secret' field",
                        span,
                    )
                    .into());
                };
                hmac_verify(&alg, &secret, signing_input.as_bytes(), &sig).is_ok()
            }
            "rsa-public" | "rsa-private" => {
                let Some(pem) = key_pem(&key) else {
                    return Err(AsError::at(
                        "jwt.verify: rsa key is missing a valid 'pem' field",
                        span,
                    )
                    .into());
                };
                rs256_verify(&pem, signing_input.as_bytes(), &sig)
            }
            "ec-public" | "ec-private" => {
                let Some(pem) = key_pem(&key) else {
                    return Err(AsError::at(
                        "jwt.verify: ec key is missing a valid 'pem' field",
                        span,
                    )
                    .into());
                };
                es256_verify(&pem, signing_input.as_bytes(), &sig)
            }
            // Any other tagged kind has an empty alg set, so it was already
            // rejected by the intersection; this is a defensive Tier-1.
            _ => false,
        };
        if !sig_ok {
            return Ok(tier1("signature invalid"));
        }

        // 7. ONLY AFTER authenticity: parse claims + validate exp/nbf/iat/iss/aud.
        let claims_bytes = match b64url_decode(p_b64, MAX_SEGMENT) {
            Ok(b) => b,
            Err(e) => return Ok(tier1(format!("malformed payload: {e}"))),
        };
        let claims = match json_parse(&claims_bytes) {
            Ok(v) => v,
            Err(e) => return Ok(tier1(format!("malformed payload: {e}"))),
        };

        let now_s = match opt_num(&opts, "clock") {
            Some(ms) => (ms / 1000.0).floor(),
            None => (self.clock_now_ms() / 1000.0).floor(),
        };
        let leeway = opt_num(&opts, "leeway").unwrap_or(0.0);

        if let Some(exp) = claim_num(&claims, "exp") {
            if now_s > exp + leeway {
                return Ok(tier1("token has expired"));
            }
        }
        if let Some(nbf) = claim_num(&claims, "nbf") {
            if now_s + leeway < nbf {
                return Ok(tier1("token is not yet valid (nbf)"));
            }
        }
        if let Some(iat) = claim_num(&claims, "iat") {
            // A token issued in the future (beyond leeway) is suspect.
            if iat > now_s + leeway {
                return Ok(tier1("token issued in the future (iat)"));
            }
        }
        if let Some(want_iss) = opt_str(&opts, "iss") {
            match claim_str(&claims, "iss") {
                Some(got) if got == want_iss => {}
                _ => return Ok(tier1("issuer (iss) mismatch")),
            }
        }
        if let Some(want_aud) = opt_str(&opts, "aud") {
            if !aud_matches(&claims, &want_aud) {
                return Ok(tier1("audience (aud) mismatch"));
            }
        }

        Ok(make_pair(claims, Value::nil()))
    }
}

// ── JWKS fetch + cache (BATT §5.6) ────────────────────────────────────────────
//
// `jwt.jwks(url)` fetches a JWK Set over the SHARED pooled client and returns a
// `JwksCache` native handle. The handle's `.verify(token, opts?)` resolves the
// token's `kid` header to the matching cached public key, then verifies through
// the EXACT A5/A6 `jwt.verify` path (so the alg-intersection / alg-confusion
// defenses are reused, not re-implemented — a JWKS RSA key cannot verify an HS256
// token, structurally). On an unknown kid the cache REFETCHES exactly once; on a
// TTL miss the next verify refetches; a verify within TTL never refetches.

/// The default JWKS cache TTL: 5 minutes. A verify within this window reuses the
/// cached keys; past it, the next verify refetches before resolving the kid.
const DEFAULT_JWKS_TTL_MS: u64 = 5 * 60 * 1000;

/// A self-contained TTL cache of `kid -> {__jwtkey}` public keys fetched from a
/// JWKS endpoint. Lives in `Interp.resources` behind a `Value::native(JwksCache)`
/// handle (BATT §5.6). NOT `std/lru` — it is a tiny per-URL TTL snapshot.
pub struct JwksCacheState {
    /// The JWKS endpoint URL (re-fetched on a TTL/unknown-kid miss).
    pub url: String,
    /// Cache lifetime in milliseconds.
    pub ttl_ms: u64,
    /// kid → the converted `{__jwtkey}` public key Object (rsa-public / ec-public).
    pub keys: std::collections::HashMap<String, Value>,
    /// The wall-clock (det-routed) ms at which `keys` was last populated.
    pub fetched_at_ms: f64,
}

impl Interp {
    /// `jwt.jwks(url, opts?) -> [jwksCache, err]`. Fetches the JWK Set once at
    /// construction (a fetch/parse failure is a Tier-1 err) and returns the cache
    /// handle. `opts.ttlMs` overrides the default TTL.
    async fn jwt_jwks(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let url = match arg(args, 0).kind() {
            ValueKind::Str(s) if !s.is_empty() => s.to_string(),
            _ => {
                return Err(AsError::at(
                    "jwt.jwks: url must be a non-empty string",
                    span,
                )
                .into())
            }
        };
        let opts = arg(args, 1);
        let ttl_ms = opt_num(&opts, "ttlMs")
            .filter(|n| *n > 0.0)
            .map(|n| n as u64)
            .unwrap_or(DEFAULT_JWKS_TTL_MS);

        // Initial fetch (over the SHARED pooled client). A network/parse failure is
        // a Tier-1 err — JWKS unavailability is recoverable control flow.
        let keys = match fetch_jwks_keys(&url).await {
            Ok(k) => k,
            Err(e) => return Ok(tier1(format!("jwt.jwks: {e}"))),
        };
        let state = JwksCacheState {
            url,
            ttl_ms,
            keys,
            fetched_at_ms: self.clock_now_ms(),
        };
        let handle = self.register_resource(
            crate::value::NativeKind::JwksCache,
            IndexMap::new(),
            crate::interp::ResourceState::JwksCache(Box::new(state)),
        );
        Ok(make_pair(handle, Value::nil()))
    }

    /// Dispatch a method on a `JwksCache` handle: `verify` / `keys` / `close`.
    pub(crate) async fn call_jwks_method(
        &self,
        m: &crate::value::NativeMethod,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match m.method.as_str() {
            "verify" => self.jwks_verify(m.receiver.id, &args, span).await,
            "keys" => {
                // Snapshot the current cached kids as an array of strings.
                let Some(state) = self.take_resource(m.receiver.id) else {
                    return Err(AsError::at("jwksCache: handle is closed", span).into());
                };
                let crate::interp::ResourceState::JwksCache(b) = state else {
                    self.return_resource(m.receiver.id, state);
                    return Err(AsError::at("jwksCache: wrong resource kind", span).into());
                };
                let kids: Vec<Value> = b.keys.keys().map(|k| Value::str(k.clone())).collect();
                self.return_resource(
                    m.receiver.id,
                    crate::interp::ResourceState::JwksCache(b),
                );
                Ok(Value::array(kids))
            }
            "close" => {
                self.take_resource(m.receiver.id);
                Ok(Value::nil())
            }
            other => Err(AsError::at(
                format!("jwksCache handle has no method '{other}'"),
                span,
            )
            .into()),
        }
    }

    /// `cache.verify(token, opts?)` — resolve the token's `kid`, refetch on a
    /// TTL/unknown-kid miss (EXACTLY ONE refetch per verify), then delegate to the
    /// A5/A6 `jwt.verify` path. Auth failure is Tier-1 throughout.
    async fn jwks_verify(
        &self,
        id: u64,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let token = match arg(args, 0).kind() {
            ValueKind::Str(s) => s.to_string(),
            other => {
                return Err(AsError::at(
                    format!(
                        "jwksCache.verify: token must be a string, got {}",
                        kind_name(&other)
                    ),
                    span,
                )
                .into())
            }
        };
        let opts = arg(args, 1);

        // Parse the header to read the `kid` (alloc-bounded). A malformed token is
        // Tier-1 (mirrors jwt.verify), never a refetch trigger.
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Ok(tier1("malformed token: expected 3 dot-separated segments"));
        }
        let header_bytes = match b64url_decode(parts[0], MAX_SEGMENT) {
            Ok(b) => b,
            Err(e) => return Ok(tier1(format!("malformed header: {e}"))),
        };
        let header = match json_parse(&header_bytes) {
            Ok(v) => v,
            Err(e) => return Ok(tier1(format!("malformed header: {e}"))),
        };
        let kid = match header_field_str(&header, "kid") {
            Some(k) => k,
            None => return Ok(tier1("token header is missing 'kid' (required for JWKS)")),
        };

        // Read the cache snapshot: the URL/ttl, whether the TTL has expired, and the
        // candidate key for this kid. We take the box OUT (no borrow held across the
        // refetch await) and put it straight back; the refetch (if any) re-takes it.
        let (url, ttl_expired, mut key) = {
            let Some(state) = self.take_jwks(id) else {
                return Err(AsError::at("jwksCache: handle is closed", span).into());
            };
            let now = self.clock_now_ms();
            let expired = now - state.fetched_at_ms >= state.ttl_ms as f64;
            let url = state.url.clone();
            let key = state.keys.get(&kid).cloned();
            self.return_resource(id, crate::interp::ResourceState::JwksCache(state));
            (url, expired, key)
        };

        // Refetch ONCE if the TTL expired OR the kid is unknown. The refetch uses the
        // take-out-across-await pattern: we do NOT hold the `resources` borrow across
        // the HTTP `.await`. (The first miss is the only refetch — a still-missing kid
        // after the refetch is a Tier-1 error, no loop.)
        if ttl_expired || key.is_none() {
            match fetch_jwks_keys(&url).await {
                Ok(fresh) => {
                    key = fresh.get(&kid).cloned();
                    // Install the fresh snapshot back into the handle.
                    if let Some(mut state) = self.take_jwks(id) {
                        state.keys = fresh;
                        state.fetched_at_ms = self.clock_now_ms();
                        self.return_resource(
                            id,
                            crate::interp::ResourceState::JwksCache(state),
                        );
                    }
                }
                Err(e) => {
                    // A refetch failure on an otherwise-cached key still lets a
                    // within-TTL hit succeed; only surface the error if we have no key.
                    if key.is_none() {
                        return Ok(tier1(format!("jwks refetch failed: {e}")));
                    }
                }
            }
        }

        let Some(key) = key else {
            return Ok(tier1(format!("no key in JWKS for kid '{kid}'")));
        };

        // Delegate to the EXACT A5/A6 verify path — the alg-intersection (and thus the
        // alg-confusion kill: an RSA JWKS key cannot HS256-verify) is reused verbatim.
        self.jwt_verify(&[Value::str(token), key, opts], span)
    }

    /// Take the boxed JWKS state out of the table (for a refetch install). Returns
    /// `None` if the handle was closed or is the wrong kind.
    fn take_jwks(&self, id: u64) -> Option<Box<JwksCacheState>> {
        match self.take_resource(id) {
            Some(crate::interp::ResourceState::JwksCache(b)) => Some(b),
            Some(other) => {
                self.return_resource(id, other);
                None
            }
            None => None,
        }
    }
}

/// Fetch + parse a JWK Set from `url` over the SHARED pooled reqwest client,
/// converting each supported (RSA / EC P-256) key into a `{__jwtkey}` public key
/// Object indexed by `kid`. Unsupported `kty`/`crv` and keys without a usable `kid`
/// are SKIPPED (a JWKS may mix in keys this verifier can't use). Returns a string
/// error for a network failure / malformed JSON (caller maps to Tier-1).
async fn fetch_jwks_keys(url: &str) -> Result<std::collections::HashMap<String, Value>, String> {
    let client = crate::stdlib::net_http::shared_client();
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("JWKS endpoint returned status {}", status.as_u16()));
    }
    let doc: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("malformed JWKS json: {e}"))?;
    let arr = doc
        .get("keys")
        .and_then(|k| k.as_array())
        .ok_or_else(|| "JWKS json has no 'keys' array".to_string())?;

    let mut out: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for jwk in arr {
        // A key with no kid can't be resolved by kid — skip it.
        let Some(kid) = jwk.get("kid").and_then(|k| k.as_str()) else {
            continue;
        };
        if let Some(key) = jwk_to_jwtkey(jwk) {
            out.insert(kid.to_string(), key);
        }
        // else: unsupported kty/crv or malformed material → skipped (not an error).
    }
    Ok(out)
}

/// Convert ONE JWK (serde_json) into a `{__jwtkey}` public key Object, or `None`
/// for an unsupported / malformed key (skipped by the caller). Supports RSA (RS256)
/// and EC P-256 (ES256), the same two asymmetric kinds A6 wired.
fn jwk_to_jwtkey(jwk: &serde_json::Value) -> Option<Value> {
    let kty = jwk.get("kty").and_then(|k| k.as_str())?;
    match kty {
        "RSA" => {
            let n = jwk.get("n").and_then(|v| v.as_str())?;
            let e = jwk.get("e").and_then(|v| v.as_str())?;
            let pem = rsa_jwk_to_pem(n, e).ok()?;
            Some(make_pem_key("rsa-public", pem))
        }
        "EC" => {
            // Only P-256 (ES256) is supported (matches A6).
            let crv = jwk.get("crv").and_then(|v| v.as_str())?;
            if crv != "P-256" {
                return None;
            }
            let x = jwk.get("x").and_then(|v| v.as_str())?;
            let y = jwk.get("y").and_then(|v| v.as_str())?;
            let pem = ec_jwk_to_pem(x, y).ok()?;
            Some(make_pem_key("ec-public", pem))
        }
        _ => None,
    }
}

/// Build an RSA public key SPKI PEM from the JWK base64url `n`/`e` parameters, so a
/// JWKS RSA key becomes a normal A6 `rsa-public` `{__jwtkey}` (re-parsed per verify).
fn rsa_jwk_to_pem(n_b64: &str, e_b64: &str) -> Result<String, String> {
    use rsa::pkcs8::EncodePublicKey;
    use rsa::BigUint;
    let n = b64url_decode(n_b64, MAX_SEGMENT).map_err(|e| format!("bad 'n': {e}"))?;
    let e = b64url_decode(e_b64, MAX_SEGMENT).map_err(|e| format!("bad 'e': {e}"))?;
    let key = rsa::RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e))
        .map_err(|e| format!("invalid RSA params: {e}"))?;
    key.to_public_key_pem(rsa::pkcs8::LineEnding::LF)
        .map_err(|e| format!("pem encode: {e}"))
}

/// Build a P-256 public key SPKI PEM from the JWK base64url `x`/`y` coordinates, so
/// a JWKS EC key becomes a normal A6 `ec-public` `{__jwtkey}`.
fn ec_jwk_to_pem(x_b64: &str, y_b64: &str) -> Result<String, String> {
    use p256::pkcs8::EncodePublicKey;
    let x = b64url_decode(x_b64, MAX_SEGMENT).map_err(|e| format!("bad 'x': {e}"))?;
    let y = b64url_decode(y_b64, MAX_SEGMENT).map_err(|e| format!("bad 'y': {e}"))?;
    if x.len() != 32 || y.len() != 32 {
        return Err("P-256 coordinates must be 32 bytes each".to_string());
    }
    let point = p256::EncodedPoint::from_affine_coordinates(
        p256::FieldBytes::from_slice(&x),
        p256::FieldBytes::from_slice(&y),
        false,
    );
    let vk = p256::ecdsa::VerifyingKey::from_encoded_point(&point)
        .map_err(|e| format!("invalid EC point: {e}"))?;
    vk.to_public_key_pem(p256::pkcs8::LineEnding::LF)
        .map_err(|e| format!("pem encode: {e}"))
}

/// `jwt.hmacKey(secret) -> {__jwtkey:"hmac", secret}`. A string|bytes secret.
fn jwt_hmac_key(args: &[Value], span: Span) -> Result<Value, Control> {
    let secret = arg(args, 0);
    let stored = match secret.kind() {
        ValueKind::Str(_) | ValueKind::Bytes(_) => secret.clone(),
        other => {
            return Err(AsError::at(
                format!(
                    "jwt.hmacKey: secret must be a string or bytes, got {}",
                    kind_name(&other)
                ),
                span,
            )
            .into())
        }
    };
    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert(KEY_TAG.to_string(), Value::str("hmac"));
    m.insert("secret".to_string(), stored);
    Ok(Value::object(m))
}

// ── asymmetric key constructors (A6) ─────────────────────────────────────────
//
// Each takes a PEM string, VALIDATES it at construction (a malformed or
// wrong-kind PEM is a Tier-1 [nil, err], naming the expected kind), and STORES
// the PEM TEXT (re-parsed per op). A non-string argument is a Tier-2 panic (a
// programming error), mirroring `jwt.hmacKey`.

/// Read the PEM string out of a candidate key value. The arg MUST be a string,
/// else Tier-2 (programming error).
fn require_pem_arg(args: &[Value], span: Span, who: &str) -> Result<String, Control> {
    match arg(args, 0).kind() {
        ValueKind::Str(s) => Ok(s.to_string()),
        other => Err(AsError::at(
            format!("{who}: pem must be a string, got {}", kind_name(&other)),
            span,
        )
        .into()),
    }
}

/// Build a tagged asymmetric key Object `{__jwtkey: kind, pem}`.
fn make_pem_key(kind: &str, pem: String) -> Value {
    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert(KEY_TAG.to_string(), Value::str(kind));
    m.insert("pem".to_string(), Value::str(pem));
    Value::object(m)
}

/// Read the `pem` field of an asymmetric key. `None` for a malformed key.
fn key_pem(key: &Value) -> Option<String> {
    let ValueKind::Object(o) = key.kind() else {
        return None;
    };
    match o.get("pem").as_ref().map(|p| p.kind()) {
        Some(ValueKind::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// `jwt.rsaPublicKey(pem)` → `{__jwtkey:"rsa-public", pem}` (SPKI/PKCS#1 PEM).
fn jwt_rsa_public_key(args: &[Value], span: Span) -> Result<Value, Control> {
    let pem = require_pem_arg(args, span, "jwt.rsaPublicKey")?;
    match rsa_public_from_pem(&pem) {
        Ok(_) => Ok(make_pem_key("rsa-public", pem)),
        Err(e) => Ok(tier1(format!("jwt.rsaPublicKey: not a valid RSA public key PEM: {e}"))),
    }
}

/// `jwt.rsaPrivateKey(pem)` → `{__jwtkey:"rsa-private", pem}` (PKCS#8/PKCS#1 PEM).
fn jwt_rsa_private_key(args: &[Value], span: Span) -> Result<Value, Control> {
    let pem = require_pem_arg(args, span, "jwt.rsaPrivateKey")?;
    match rsa_private_from_pem(&pem) {
        Ok(_) => Ok(make_pem_key("rsa-private", pem)),
        Err(e) => Ok(tier1(format!("jwt.rsaPrivateKey: not a valid RSA private key PEM: {e}"))),
    }
}

/// `jwt.ecPublicKey(pem)` → `{__jwtkey:"ec-public", pem}` (P-256 SPKI PEM).
fn jwt_ec_public_key(args: &[Value], span: Span) -> Result<Value, Control> {
    let pem = require_pem_arg(args, span, "jwt.ecPublicKey")?;
    match ec_public_from_pem(&pem) {
        Ok(_) => Ok(make_pem_key("ec-public", pem)),
        Err(e) => Ok(tier1(format!(
            "jwt.ecPublicKey: not a valid EC (P-256) public key PEM: {e}"
        ))),
    }
}

/// `jwt.ecPrivateKey(pem)` → `{__jwtkey:"ec-private", pem}` (P-256 PKCS#8/SEC1 PEM).
fn jwt_ec_private_key(args: &[Value], span: Span) -> Result<Value, Control> {
    let pem = require_pem_arg(args, span, "jwt.ecPrivateKey")?;
    match ec_private_from_pem(&pem) {
        Ok(_) => Ok(make_pem_key("ec-private", pem)),
        Err(e) => Ok(tier1(format!(
            "jwt.ecPrivateKey: not a valid EC (P-256) private key PEM: {e}"
        ))),
    }
}

// ── RS256 (RSASSA-PKCS1-v1_5 over SHA-256) ───────────────────────────────────

/// Parse an RSA public key from a PEM (SPKI first, PKCS#1 fallback).
fn rsa_public_from_pem(pem: &str) -> Result<rsa::RsaPublicKey, String> {
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::pkcs8::DecodePublicKey;
    rsa::RsaPublicKey::from_public_key_pem(pem)
        .or_else(|_| rsa::RsaPublicKey::from_pkcs1_pem(pem))
        .map_err(|e| e.to_string())
}

/// Parse an RSA private key from a PEM (PKCS#8 first, PKCS#1 fallback).
fn rsa_private_from_pem(pem: &str) -> Result<rsa::RsaPrivateKey, String> {
    use rsa::pkcs1::DecodeRsaPrivateKey;
    use rsa::pkcs8::DecodePrivateKey;
    rsa::RsaPrivateKey::from_pkcs8_pem(pem)
        .or_else(|_| rsa::RsaPrivateKey::from_pkcs1_pem(pem))
        .map_err(|e| e.to_string())
}

/// RS256-sign the signing input, returning the raw PKCS#1-v1.5 signature bytes.
fn rs256_sign(pem: &str, signing_input: &[u8]) -> Result<Vec<u8>, String> {
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding, Signer};
    let sk = rsa_private_from_pem(pem)?;
    let signing_key = SigningKey::<Sha256>::new(sk);
    let sig = signing_key
        .try_sign(signing_input)
        .map_err(|e| e.to_string())?;
    Ok(sig.to_vec())
}

/// RS256-verify. Returns `true` iff the signature is valid (any parse/format
/// error → `false`, mapped to a Tier-1 "signature invalid" by the caller).
fn rs256_verify(pem: &str, signing_input: &[u8], sig: &[u8]) -> bool {
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier;
    let Ok(pk) = rsa_public_from_pem(pem) else {
        return false;
    };
    let vk: VerifyingKey<Sha256> = VerifyingKey::new(pk);
    let Ok(signature) = Signature::try_from(sig) else {
        return false;
    };
    vk.verify(signing_input, &signature).is_ok()
}

// ── ES256 (ECDSA P-256 over SHA-256, FIXED-WIDTH r||s JOSE encoding) ──────────

/// Parse a P-256 public key from a PEM (SPKI).
fn ec_public_from_pem(pem: &str) -> Result<p256::ecdsa::VerifyingKey, String> {
    use p256::pkcs8::DecodePublicKey;
    p256::ecdsa::VerifyingKey::from_public_key_pem(pem).map_err(|e| e.to_string())
}

/// Parse a P-256 private signing key from a PEM (PKCS#8 first, SEC1 fallback).
fn ec_private_from_pem(pem: &str) -> Result<p256::ecdsa::SigningKey, String> {
    use p256::pkcs8::DecodePrivateKey;
    p256::ecdsa::SigningKey::from_pkcs8_pem(pem)
        .or_else(|_| {
            // SEC1 `EC PRIVATE KEY` fallback.
            p256::SecretKey::from_sec1_pem(pem).map(p256::ecdsa::SigningKey::from)
        })
        .map_err(|e| e.to_string())
}

/// ES256-sign, returning the FIXED-WIDTH 64-byte `r||s` JOSE signature (NOT DER).
fn es256_sign(pem: &str, signing_input: &[u8]) -> Result<Vec<u8>, String> {
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::Signature;
    let sk = ec_private_from_pem(pem)?;
    let sig: Signature = sk.sign(signing_input);
    // `to_bytes()` is the fixed-width 64-byte r||s — the JOSE encoding.
    Ok(sig.to_bytes().to_vec())
}

/// ES256-verify. The signature MUST be the fixed-width 64-byte `r||s` form;
/// `Signature::from_slice` rejects a DER blob (or any non-64-byte input) by
/// construction — THE JOSE PIN. Any error → `false`.
fn es256_verify(pem: &str, signing_input: &[u8], sig: &[u8]) -> bool {
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::Signature;
    let Ok(vk) = ec_public_from_pem(pem) else {
        return false;
    };
    // FIXED-WIDTH ONLY — never Signature::from_der. A DER `0x30…` blob fails here.
    let Ok(signature) = Signature::from_slice(sig) else {
        return false;
    };
    vk.verify(signing_input, &signature).is_ok()
}

/// `jwt.decode(token) -> [{header, claims, signature, verified:false}, err]`.
/// PURE inspection — no key, no verification. Tier-1 only on a malformed compact
/// form. The result's `verified:false` testifies that nothing was checked (§5.4).
fn jwt_decode(args: &[Value], span: Span) -> Result<Value, Control> {
    let token = match arg(args, 0).kind() {
        ValueKind::Str(s) => s.to_string(),
        other => {
            return Err(AsError::at(
                format!(
                    "jwt.decode: token must be a string, got {}",
                    kind_name(&other)
                ),
                span,
            )
            .into())
        }
    };
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Ok(tier1("malformed token: expected 3 dot-separated segments"));
    }
    let header_bytes = match b64url_decode(parts[0], MAX_SEGMENT) {
        Ok(b) => b,
        Err(e) => return Ok(tier1(format!("malformed header: {e}"))),
    };
    let header = match json_parse(&header_bytes) {
        Ok(v) => v,
        Err(e) => return Ok(tier1(format!("malformed header: {e}"))),
    };
    let claims_bytes = match b64url_decode(parts[1], MAX_SEGMENT) {
        Ok(b) => b,
        Err(e) => return Ok(tier1(format!("malformed payload: {e}"))),
    };
    let claims = match json_parse(&claims_bytes) {
        Ok(v) => v,
        Err(e) => return Ok(tier1(format!("malformed payload: {e}"))),
    };
    let mut out: IndexMap<String, Value> = IndexMap::new();
    out.insert("header".to_string(), header);
    out.insert("claims".to_string(), claims);
    out.insert("signature".to_string(), Value::str(parts[2]));
    out.insert("verified".to_string(), Value::bool_(false));
    Ok(make_pair(Value::object(out), Value::nil()))
}

// ── small helpers ────────────────────────────────────────────────────────────

/// Build a Tier-1 `[nil, {message}]` error pair.
fn tier1(msg: impl Into<String>) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg.into())))
}

fn kind_name(k: &ValueKind) -> &'static str {
    match k {
        ValueKind::Nil => "nil",
        ValueKind::Bool(_) => "bool",
        ValueKind::Int(_) => "int",
        ValueKind::Float(_) => "float",
        ValueKind::Decimal(_) => "decimal",
        ValueKind::Str(_) => "string",
        ValueKind::Array(_) => "array",
        ValueKind::Object(_) => "object",
        ValueKind::Map(_) => "map",
        ValueKind::Set(_) => "set",
        ValueKind::Bytes(_) => "bytes",
        _ => "value",
    }
}

fn opt_str(opts: &Value, field: &str) -> Option<String> {
    match opts.kind() {
        ValueKind::Object(o) => match o.get(field).as_ref().map(|v| v.kind()) {
            Some(ValueKind::Str(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn opt_num(opts: &Value, field: &str) -> Option<f64> {
    match opts.kind() {
        ValueKind::Object(o) => o.get(field).and_then(|v| v.as_f64()),
        _ => None,
    }
}

fn opt_str_array(opts: &Value, field: &str) -> Option<Vec<String>> {
    let ValueKind::Object(o) = opts.kind() else {
        return None;
    };
    let v = o.get(field)?;
    let ValueKind::Array(a) = v.kind() else {
        return None;
    };
    let mut out = Vec::new();
    for e in a.borrow().iter() {
        if let ValueKind::Str(s) = e.kind() {
            out.push(s.to_string());
        }
    }
    Some(out)
}

fn header_field_str(header: &Value, field: &str) -> Option<String> {
    match header.kind() {
        ValueKind::Object(o) => match o.get(field).as_ref().map(|v| v.kind()) {
            Some(ValueKind::Str(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn claim_num(claims: &Value, name: &str) -> Option<f64> {
    match claims.kind() {
        ValueKind::Object(o) => o.get(name).and_then(|v| v.as_f64()),
        _ => None,
    }
}

fn claim_str(claims: &Value, name: &str) -> Option<String> {
    match claims.kind() {
        ValueKind::Object(o) => match o.get(name).as_ref().map(|v| v.kind()) {
            Some(ValueKind::Str(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// `aud` may be a string OR an array of strings (RFC 7519 §4.1.3).
fn aud_matches(claims: &Value, want: &str) -> bool {
    let ValueKind::Object(o) = claims.kind() else {
        return false;
    };
    let Some(aud) = o.get("aud") else {
        return false;
    };
    match aud.kind() {
        ValueKind::Str(s) => s.as_ref() == want,
        ValueKind::Array(a) => a
            .borrow()
            .iter()
            .any(|e| matches!(e.kind(), ValueKind::Str(s) if s.as_ref() == want)),
        _ => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "auth"))]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    /// A fresh non-deterministic interp.
    fn ip() -> Interp {
        Interp::new()
    }

    /// An interp whose virtual clock is pinned at `now_ms` (Record mode does not
    /// advance the clock, so every `clock_now_ms()` returns `now_ms`).
    fn ip_at(now_ms: f64) -> Interp {
        let interp = Interp::new();
        interp.restore_determinism(Some(crate::det::DeterminismContext::record(1, now_ms)));
        interp
    }

    fn s(x: &str) -> Value {
        Value::str(x)
    }

    fn obj(pairs: &[(&str, Value)]) -> Value {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v.clone());
        }
        Value::object(m)
    }

    /// Unwrap a `[value, err]` pair into (value, err).
    fn pair(v: &Value) -> (Value, Value) {
        match v.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                (b[0].clone(), b[1].clone())
            }
            _ => panic!("expected a [value, err] pair"),
        }
    }

    fn token_str(v: &Value) -> String {
        let (val, err) = pair(v);
        assert!(matches!(err.kind(), ValueKind::Nil), "expected ok, got err");
        match val.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!("expected token string"),
        }
    }

    fn is_err(v: &Value) -> bool {
        let (val, err) = pair(v);
        matches!(val.kind(), ValueKind::Nil) && !matches!(err.kind(), ValueKind::Nil)
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

    // (a) RFC 7515 A.1 HS256 vector. The header is serialized in our canonical
    // compact form {"alg":"HS256","typ":"JWT"}; the signature is the correct
    // HMAC-SHA256 over the RFC A.1 key + payload (computed independently).
    #[test]
    fn rfc7515_a1_hs256_vector() {
        let interp = ip();
        // RFC 7515 A.1 key: the JWK `k` octet sequence (base64url-decoded).
        let key_bytes: Vec<u8> = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(
                "AyM1SysPpbyDfgZld3umj1qzKObwVMkoqQ-EstJQLr_T-1qS0gZH75aKtMN3Yj0iPS4hcgUuTwjAzZr1Z9CAow",
            )
            .unwrap();
        let key = jwt_hmac_key(
            &[Value::bytes_rc(std::rc::Rc::new(std::cell::RefCell::new(
                key_bytes,
            )))],
            sp(),
        )
        .unwrap();
        // RFC A.1 payload, in its document order.
        let claims = obj(&[
            ("iss", s("joe")),
            ("exp", Value::int(1300819380)),
            ("http://example.com/is_root", Value::bool_(true)),
        ]);
        let signed = interp.jwt_sign(&[claims, key], sp()).unwrap();
        let tok = token_str(&signed);
        assert_eq!(
            tok,
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.\
             eyJpc3MiOiJqb2UiLCJleHAiOjEzMDA4MTkzODAsImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ.\
             d6nMDXnJZfNNj-1o1e75s6d0six0lkLp5hSrGaz4o9A"
        );
    }

    // (b) sign↔verify roundtrip for HS256/384/512.
    #[test]
    fn sign_verify_roundtrip_all_hs() {
        for alg in ["HS256", "HS384", "HS512"] {
            let interp = ip();
            let key = jwt_hmac_key(&[s("my-test-secret-key")], sp()).unwrap();
            let claims = obj(&[("sub", s("alice")), ("role", s("admin"))]);
            let signed = interp
                .jwt_sign(&[claims, key.clone(), obj(&[("alg", s(alg))])], sp())
                .unwrap();
            let tok = token_str(&signed);
            let verified = interp.jwt_verify(&[s(&tok), key], sp()).unwrap();
            let (val, err) = pair(&verified);
            assert!(matches!(err.kind(), ValueKind::Nil), "{alg}: verify failed");
            assert_eq!(claim_str(&val, "sub"), Some("alice".to_string()), "{alg}");
            assert_eq!(claim_str(&val, "role"), Some("admin".to_string()), "{alg}");
        }
    }

    // (c) claim validation: exp / nbf / leeway / iss / aud.
    #[test]
    fn claim_validation() {
        let key = jwt_hmac_key(&[s("secret")], sp()).unwrap();

        // exp in the past → "expired".
        let signer = ip_at(2_000_000.0); // 2000s
        let expired = signer
            .jwt_sign(
                &[obj(&[("exp", Value::int(1000))]), key.clone()],
                sp(),
            )
            .unwrap();
        let tok = token_str(&expired);
        let v = signer.jwt_verify(&[s(&tok), key.clone()], sp()).unwrap();
        assert!(is_err(&v));
        assert!(err_msg(&v).contains("expired"), "got: {}", err_msg(&v));
        // leeway rescues it (leeway covers the 1999s gap).
        let v2 = signer
            .jwt_verify(
                &[s(&tok), key.clone(), obj(&[("leeway", Value::int(2_000_000))])],
                sp(),
            )
            .unwrap();
        assert!(!is_err(&v2), "leeway should rescue exp");

        // nbf in the future → not yet valid.
        let signer2 = ip_at(1000.0); // 1s
        let nbf_tok = token_str(
            &signer2
                .jwt_sign(&[obj(&[("nbf", Value::int(999_999))]), key.clone()], sp())
                .unwrap(),
        );
        let v3 = signer2.jwt_verify(&[s(&nbf_tok), key.clone()], sp()).unwrap();
        assert!(is_err(&v3));
        assert!(err_msg(&v3).contains("not yet valid"), "got: {}", err_msg(&v3));
        // leeway rescues nbf.
        let v4 = signer2
            .jwt_verify(
                &[
                    s(&nbf_tok),
                    key.clone(),
                    obj(&[("leeway", Value::int(1_000_000))]),
                ],
                sp(),
            )
            .unwrap();
        assert!(!is_err(&v4), "leeway should rescue nbf");

        // iss mismatch.
        let interp = ip();
        let iss_tok = token_str(
            &interp
                .jwt_sign(&[obj(&[("iss", s("good"))]), key.clone()], sp())
                .unwrap(),
        );
        let bad_iss = interp
            .jwt_verify(&[s(&iss_tok), key.clone(), obj(&[("iss", s("expected"))])], sp())
            .unwrap();
        assert!(is_err(&bad_iss));
        assert!(err_msg(&bad_iss).contains("iss"));
        let ok_iss = interp
            .jwt_verify(&[s(&iss_tok), key.clone(), obj(&[("iss", s("good"))])], sp())
            .unwrap();
        assert!(!is_err(&ok_iss));

        // aud mismatch (and array form).
        let aud_tok = token_str(
            &interp
                .jwt_sign(
                    &[obj(&[("aud", Value::array(vec![s("a"), s("b")]))]), key.clone()],
                    sp(),
                )
                .unwrap(),
        );
        let bad_aud = interp
            .jwt_verify(&[s(&aud_tok), key.clone(), obj(&[("aud", s("c"))])], sp())
            .unwrap();
        assert!(is_err(&bad_aud));
        let ok_aud = interp
            .jwt_verify(&[s(&aud_tok), key.clone(), obj(&[("aud", s("b"))])], sp())
            .unwrap();
        assert!(!is_err(&ok_aud));
    }

    // (d) THE security battery — alg-confusion defenses.
    #[test]
    fn jwt_alg_confusion_battery() {
        let interp = ip();
        let key = jwt_hmac_key(&[s("secret")], sp()).unwrap();
        let claims = obj(&[("sub", s("alice"))]);
        let tok = token_str(&interp.jwt_sign(&[claims, key.clone()], sp()).unwrap());
        let h_b64 = tok.split('.').next().unwrap();
        let p_b64 = tok.split('.').nth(1).unwrap();
        let s_b64 = tok.split('.').nth(2).unwrap();

        // alg:"none" / "None" / "NONE" — ALL rejected, NEVER verify.
        for none_alg in ["none", "None", "NONE"] {
            let hdr = obj(&[("alg", s(none_alg)), ("typ", s("JWT"))]);
            let hj = json_compact(&hdr).unwrap();
            // A "none" token has an empty signature segment (the classic form).
            let none_tok = format!("{}.{}.", b64url(hj.as_bytes()), p_b64);
            let v = interp.jwt_verify(&[s(&none_tok), key.clone()], sp()).unwrap();
            assert!(is_err(&v), "alg:{none_alg} must be rejected");
            assert!(
                err_msg(&v).contains("none"),
                "alg:{none_alg} rejection should mention none, got: {}",
                err_msg(&v)
            );
        }

        // allowlist intersection: HS256 token verified with algs:["HS384"] → err.
        let v = interp
            .jwt_verify(
                &[
                    s(&tok),
                    key.clone(),
                    obj(&[("algs", Value::array(vec![s("HS384")]))]),
                ],
                sp(),
            )
            .unwrap();
        assert!(is_err(&v), "alg not in allowlist must fail");
        assert!(err_msg(&v).contains("allowlist"));
        // and HS256 IS allowed when in the allowlist.
        let ok = interp
            .jwt_verify(
                &[
                    s(&tok),
                    key.clone(),
                    obj(&[("algs", Value::array(vec![s("HS256"), s("HS384")]))]),
                ],
                sp(),
            )
            .unwrap();
        assert!(!is_err(&ok));

        // tampered header → err.
        let tampered_hdr = {
            let bad = obj(&[("alg", s("HS256")), ("typ", s("JWT")), ("x", s("evil"))]);
            format!("{}.{}.{}", b64url(json_compact(&bad).unwrap().as_bytes()), p_b64, s_b64)
        };
        assert!(is_err(&interp.jwt_verify(&[s(&tampered_hdr), key.clone()], sp()).unwrap()));

        // tampered payload → err.
        let tampered_pl = {
            let bad = obj(&[("sub", s("attacker"))]);
            format!("{}.{}.{}", h_b64, b64url(json_compact(&bad).unwrap().as_bytes()), s_b64)
        };
        assert!(is_err(&interp.jwt_verify(&[s(&tampered_pl), key.clone()], sp()).unwrap()));

        // tampered signature → err.
        let mut sig_bytes = b64url_decode(s_b64, MAX_SEGMENT).unwrap();
        sig_bytes[0] ^= 0xFF;
        let tampered_sig = format!("{}.{}.{}", h_b64, p_b64, b64url(&sig_bytes));
        let vs = interp.jwt_verify(&[s(&tampered_sig), key.clone()], sp()).unwrap();
        assert!(is_err(&vs));
        assert!(err_msg(&vs).contains("signature invalid"));

        // jku / jwk / kid headers are IGNORED (verified purely by the provided key).
        let hdr_with_jose = obj(&[
            ("alg", s("HS256")),
            ("typ", s("JWT")),
            ("kid", s("attacker-key-1")),
            ("jku", s("https://evil.example/keys")),
            ("jwk", obj(&[("kty", s("oct"))])),
        ]);
        let hj = json_compact(&hdr_with_jose).unwrap();
        let hb = b64url(hj.as_bytes());
        // Re-sign over the NEW header with the SAME secret (a legitimate token
        // that merely carries jku/jwk/kid). It must verify by the provided key,
        // and those headers must be neither fetched nor trusted.
        let si = format!("{hb}.{p_b64}");
        let sig = hmac_sign("HS256", b"secret", si.as_bytes()).unwrap();
        let jose_tok = format!("{hb}.{p_b64}.{}", b64url(&sig));
        let v = interp.jwt_verify(&[s(&jose_tok), key.clone()], sp()).unwrap();
        assert!(!is_err(&v), "a token with jku/jwk/kid must verify by the provided key");
    }

    // (e) malformed compact forms — Tier-1, alloc-bounded, NEVER a Rust panic.
    #[test]
    fn malformed_tokens_never_panic() {
        let interp = ip();
        let key = jwt_hmac_key(&[s("secret")], sp()).unwrap();
        let huge = "A".repeat(MAX_SEGMENT + 10);
        let bad_tokens = vec![
            "".to_string(),
            "abc".to_string(),               // 0 dots
            "abc.def".to_string(),           // 1 dot
            "a.b.c.d".to_string(),           // 3 dots
            "!!!.!!!.!!!".to_string(),       // bad base64url
            format!("{huge}.{huge}.{huge}"), // huge segments (over the cap)
            "....".to_string(),
            ".".to_string(),
        ];
        for t in bad_tokens {
            let v = interp.jwt_verify(&[s(&t), key.clone()], sp()).unwrap();
            assert!(is_err(&v), "token {t:?} should be a Tier-1 err");
            let d = jwt_decode(&[s(&t)], sp()).unwrap();
            assert!(is_err(&d), "decode of {t:?} should be a Tier-1 err");
        }
    }

    // (f) jwt.decode → pure inspection, verified:false, no key, no verification.
    #[test]
    fn decode_is_unverified_inspection() {
        let interp = ip();
        let key = jwt_hmac_key(&[s("secret")], sp()).unwrap();
        let claims = obj(&[("sub", s("alice")), ("role", s("admin"))]);
        let tok = token_str(&interp.jwt_sign(&[claims, key], sp()).unwrap());
        let (val, err) = pair(&jwt_decode(&[s(&tok)], sp()).unwrap());
        assert!(matches!(err.kind(), ValueKind::Nil));
        // verified must be false.
        match val.kind() {
            ValueKind::Object(o) => {
                let verified = o.get("verified").unwrap();
                assert!(matches!(verified.kind(), ValueKind::Bool(false)));
                let header = o.get("header").unwrap();
                assert_eq!(header_field_str(&header, "alg"), Some("HS256".to_string()));
                let c = o.get("claims").unwrap();
                assert_eq!(claim_str(&c, "sub"), Some("alice".to_string()));
                let signature = o.get("signature").unwrap();
                assert!(matches!(signature.kind(), ValueKind::Str(_)));
            }
            _ => panic!("decode result must be an object"),
        }
    }

    // sign with a non-key value is Tier-2 (a programming error).
    #[test]
    fn sign_with_non_key_is_tier2() {
        let interp = ip();
        let claims = obj(&[("sub", s("a"))]);
        assert!(interp.jwt_sign(&[claims, s("just-a-string")], sp()).is_err());
    }

    // algs_for_key_kind: hmac wired; rsa/ec placeholders present for A6.
    #[test]
    fn alg_sets_per_kind() {
        assert_eq!(algs_for_key_kind("hmac"), &["HS256", "HS384", "HS512"]);
        assert_eq!(algs_for_key_kind("rsa-public"), &["RS256"]);
        assert_eq!(algs_for_key_kind("ec-private"), &["ES256"]);
        assert!(algs_for_key_kind("bogus").is_empty());
    }

    // ── A6 asymmetric fixtures (see the regen commands in the module header) ──────

    const RSA_PRIV: &str = include_str!("testdata/jwt_rsa_priv.pem");
    const RSA_PUB: &str = include_str!("testdata/jwt_rsa_pub.pem");
    const EC_PRIV: &str = include_str!("testdata/jwt_ec_priv.pem");
    const EC_PUB: &str = include_str!("testdata/jwt_ec_pub.pem");

    // ── A6 (a): RS256 sign↔verify roundtrip + an EXTERNAL byte-level cross-check ──
    //
    // The cross-check proves wire-compat with the ecosystem: AScript signs, and the
    // `rsa` crate's OWN verify (a path independent of jwt_verify) accepts the
    // resulting signature over the EXACT signing input against the public key.
    #[test]
    fn rs256_sign_verify_roundtrip() {
        let interp = ip();
        let priv_key = interp
            .call_jwt("rsaPrivateKey", &[s(RSA_PRIV)], sp())
            .unwrap();
        let pub_key = interp.call_jwt("rsaPublicKey", &[s(RSA_PUB)], sp()).unwrap();
        let claims = obj(&[("sub", s("alice")), ("role", s("admin"))]);
        let signed = interp
            .jwt_sign(&[claims, priv_key, obj(&[("alg", s("RS256"))])], sp())
            .unwrap();
        let tok = token_str(&signed);

        // AScript verify accepts it.
        let verified = interp
            .jwt_verify(&[s(&tok), pub_key.clone()], sp())
            .unwrap();
        let (val, err) = pair(&verified);
        assert!(matches!(err.kind(), ValueKind::Nil), "RS256 verify failed");
        assert_eq!(claim_str(&val, "sub"), Some("alice".to_string()));

        // INDEPENDENT byte-level cross-check via the rsa crate's own verify over
        // the exact signing input (header.payload).
        use rsa::pkcs1v15::{Signature, VerifyingKey};
        use rsa::pkcs8::DecodePublicKey;
        use rsa::signature::Verifier;
        use rsa::RsaPublicKey;
        let parts: Vec<&str> = tok.split('.').collect();
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = b64url_decode(parts[2], MAX_SEGMENT).unwrap();
        let rsa_pub = RsaPublicKey::from_public_key_pem(RSA_PUB).unwrap();
        let vk: VerifyingKey<Sha256> = VerifyingKey::new(rsa_pub);
        let sig = Signature::try_from(sig_bytes.as_slice()).unwrap();
        vk.verify(signing_input.as_bytes(), &sig)
            .expect("rsa crate must accept AScript's RS256 signature (wire-compat)");
    }

    // ── A6 (b): ES256 roundtrip + THE JOSE-ENCODING PIN ──────────────────────────
    //
    // The signature segment, b64url-decoded, MUST be EXACTLY 64 bytes (r||s,
    // fixed-width). A DER (ASN.1, 0x30...) signature of the SAME signature fed as
    // the token sig MUST FAIL verification (Signature::from_slice is fixed-width).
    #[test]
    fn es256_roundtrip_and_jose_pin() {
        let interp = ip();
        let priv_key = interp.call_jwt("ecPrivateKey", &[s(EC_PRIV)], sp()).unwrap();
        let pub_key = interp.call_jwt("ecPublicKey", &[s(EC_PUB)], sp()).unwrap();
        let claims = obj(&[("sub", s("bob"))]);
        let signed = interp
            .jwt_sign(&[claims, priv_key, obj(&[("alg", s("ES256"))])], sp())
            .unwrap();
        let tok = token_str(&signed);
        let parts: Vec<&str> = tok.split('.').collect();

        // THE PIN: the decoded signature is EXACTLY 64 bytes (fixed-width r||s).
        let sig_bytes = b64url_decode(parts[2], MAX_SEGMENT).unwrap();
        assert_eq!(
            sig_bytes.len(),
            64,
            "ES256 JOSE signature must be exactly 64 bytes (r||s), got {}",
            sig_bytes.len()
        );

        // Roundtrip verify accepts the fixed-width sig.
        let verified = interp.jwt_verify(&[s(&tok), pub_key.clone()], sp()).unwrap();
        assert!(!is_err(&verified), "ES256 verify failed: {}", err_msg(&verified));

        // DER REJECTION: re-encode the SAME (r,s) signature as ASN.1 DER and splice
        // it into the token's sig segment. Verify MUST fail — a DER-encoded ECDSA
        // signature (variable length, leading 0x30) is NOT valid JOSE.
        use p256::ecdsa::Signature;
        let fixed = Signature::from_slice(&sig_bytes).unwrap();
        let der = fixed.to_der();
        let der_bytes = der.as_bytes();
        assert_eq!(der_bytes[0], 0x30, "DER must begin with the SEQUENCE tag");
        assert_ne!(der_bytes.len(), 64, "DER form must differ in length from r||s");
        let der_tok = format!("{}.{}.{}", parts[0], parts[1], b64url(der_bytes));
        let der_verify = interp.jwt_verify(&[s(&der_tok), pub_key], sp()).unwrap();
        assert!(
            is_err(&der_verify),
            "a DER-encoded ECDSA signature MUST be rejected (JOSE is fixed-width only)"
        );
    }

    // ── A6 (c): hostile / mismatched PEM at construction → Tier-1 ────────────────
    #[test]
    fn asymmetric_key_constructors_reject_bad_pem() {
        let interp = ip();
        let garbage = ["", "not a pem", "-----BEGIN PRIVATE KEY-----\nZZZZ\n-----END PRIVATE KEY-----"];
        for kind in ["rsaPublicKey", "rsaPrivateKey", "ecPublicKey", "ecPrivateKey"] {
            for bad in garbage {
                let r = interp.call_jwt(kind, &[s(bad)], sp()).unwrap();
                assert!(is_err(&r), "{kind}({bad:?}) must be Tier-1 err");
            }
        }
        // An EC PEM fed to rsaPublicKey → Tier-1 naming the mismatch (rsa expected).
        let ec_to_rsa = interp.call_jwt("rsaPublicKey", &[s(EC_PUB)], sp()).unwrap();
        assert!(is_err(&ec_to_rsa));
        assert!(
            err_msg(&ec_to_rsa).to_lowercase().contains("rsa"),
            "EC→rsaPublicKey should name rsa, got: {}",
            err_msg(&ec_to_rsa)
        );
        // An RSA PEM fed to ecPublicKey → Tier-1 naming the mismatch (ec expected).
        let rsa_to_ec = interp.call_jwt("ecPublicKey", &[s(RSA_PUB)], sp()).unwrap();
        assert!(is_err(&rsa_to_ec));
        assert!(
            err_msg(&rsa_to_ec).to_lowercase().contains("ec")
                || err_msg(&rsa_to_ec).to_lowercase().contains("p-256"),
            "RSA→ecPublicKey should name ec/p-256, got: {}",
            err_msg(&rsa_to_ec)
        );
        // A non-string key material → Tier-2 (programming error).
        assert!(interp.call_jwt("rsaPublicKey", &[Value::int(1)], sp()).is_err());
    }

    // ── A6 (d): THE STRUCTURAL KILL — an RSA public key + alg:HS256 → err ─────────
    //
    // The hmac path must NEVER run with an rsa key: algs_for_key_kind("rsa-public")
    // does not include HS256, so the alg-intersection rejects it before any verify.
    #[test]
    fn rsa_key_cannot_hmac_verify() {
        let interp = ip();
        let pub_key = interp.call_jwt("rsaPublicKey", &[s(RSA_PUB)], sp()).unwrap();
        // Forge an HS256 token (anyone can compute an HMAC over a chosen secret —
        // the attack is to pass it the RSA *public* key as if it were the secret).
        let hmac_key = jwt_hmac_key(&[s("anything")], sp()).unwrap();
        let claims = obj(&[("sub", s("attacker"))]);
        let hs_tok = token_str(
            &interp
                .jwt_sign(&[claims, hmac_key, obj(&[("alg", s("HS256"))])], sp())
                .unwrap(),
        );
        // Verifying that HS256 token with the RSA public key MUST fail — the hmac
        // path is unreachable for an rsa-public kind.
        let v = interp.jwt_verify(&[s(&hs_tok), pub_key], sp()).unwrap();
        assert!(is_err(&v), "RSA pubkey + HS256 must be rejected (structural kill)");
        assert!(
            err_msg(&v).contains("not allowed") || err_msg(&v).contains("HS256"),
            "rejection should be the intersection check, got: {}",
            err_msg(&v)
        );

        // Symmetrically: an EC public key + HS256 → err.
        let ec_pub = interp.call_jwt("ecPublicKey", &[s(EC_PUB)], sp()).unwrap();
        let hmac_key2 = jwt_hmac_key(&[s("anything")], sp()).unwrap();
        let hs_tok2 = token_str(
            &interp
                .jwt_sign(&[obj(&[("sub", s("x"))]), hmac_key2, obj(&[("alg", s("HS256"))])], sp())
                .unwrap(),
        );
        assert!(is_err(&interp.jwt_verify(&[s(&hs_tok2), ec_pub], sp()).unwrap()));
    }

    // ── A6: cross-confusion — RS256 token must NOT verify with an EC key & v.v. ───
    #[test]
    fn rs256_es256_keys_do_not_cross() {
        let interp = ip();
        let rsa_priv = interp.call_jwt("rsaPrivateKey", &[s(RSA_PRIV)], sp()).unwrap();
        let ec_pub = interp.call_jwt("ecPublicKey", &[s(EC_PUB)], sp()).unwrap();
        let rs_tok = token_str(
            &interp
                .jwt_sign(&[obj(&[("sub", s("a"))]), rsa_priv, obj(&[("alg", s("RS256"))])], sp())
                .unwrap(),
        );
        // RS256 token verified with an EC public key → err (alg RS256 ∉ ec algs).
        assert!(is_err(&interp.jwt_verify(&[s(&rs_tok), ec_pub], sp()).unwrap()));

        let ec_priv = interp.call_jwt("ecPrivateKey", &[s(EC_PRIV)], sp()).unwrap();
        let rsa_pub = interp.call_jwt("rsaPublicKey", &[s(RSA_PUB)], sp()).unwrap();
        let es_tok = token_str(
            &interp
                .jwt_sign(&[obj(&[("sub", s("a"))]), ec_priv, obj(&[("alg", s("ES256"))])], sp())
                .unwrap(),
        );
        assert!(is_err(&interp.jwt_verify(&[s(&es_tok), rsa_pub], sp()).unwrap()));
    }

    // ── A7: JWKS fetch + cache (BATT §5.6) ───────────────────────────────────────

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Build the RSA JWK ({kty,kid,n,e}) for the test RSA public PEM.
    fn rsa_jwk_json(kid: &str) -> serde_json::Value {
        use rsa::pkcs8::DecodePublicKey;
        use rsa::traits::PublicKeyParts;
        let key = rsa::RsaPublicKey::from_public_key_pem(RSA_PUB).unwrap();
        let n = b64url(&key.n().to_bytes_be());
        let e = b64url(&key.e().to_bytes_be());
        serde_json::json!({"kty":"RSA","kid":kid,"alg":"RS256","use":"sig","n":n,"e":e})
    }

    /// Build the EC P-256 JWK ({kty,crv,kid,x,y}) for the test EC public PEM.
    fn ec_jwk_json(kid: &str) -> serde_json::Value {
        use p256::pkcs8::DecodePublicKey;
        let vk = p256::ecdsa::VerifyingKey::from_public_key_pem(EC_PUB).unwrap();
        let pt = vk.to_encoded_point(false);
        let x = b64url(pt.x().unwrap().as_slice());
        let y = b64url(pt.y().unwrap().as_slice());
        serde_json::json!({"kty":"EC","crv":"P-256","kid":kid,"alg":"ES256","use":"sig","x":x,"y":y})
    }

    /// An in-process JWKS endpoint serving `jwks_json`, counting hits in `counter`.
    mod jwks_fixture {
        use http_body_util::combinators::BoxBody;
        use http_body_util::Full;
        use hyper::body::{Bytes, Incoming};
        use hyper::service::service_fn;
        use hyper::{Request, Response};
        use hyper_util::rt::TokioIo;
        use std::convert::Infallible;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::net::TcpListener;

        async fn handle(
            _req: Request<Incoming>,
            body: Arc<String>,
            counter: Arc<AtomicUsize>,
        ) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut r = Response::new(Full::new(Bytes::from((*body).clone())));
            r.headers_mut().insert(
                hyper::header::CONTENT_TYPE,
                "application/json".parse().unwrap(),
            );
            let (parts, body) = r.into_parts();
            Ok(Response::from_parts(parts, BoxBody::new(body)))
        }

        /// Start the JWKS server; returns the `/jwks` URL. `counter` increments once
        /// per request so a test can assert refetch behavior.
        pub async fn start(jwks_json: String, counter: Arc<AtomicUsize>) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().unwrap();
            let body = Arc::new(jwks_json);
            tokio::spawn(async move {
                loop {
                    let (stream, _) = match listener.accept().await {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let io = TokioIo::new(stream);
                    let body = body.clone();
                    let counter = counter.clone();
                    tokio::spawn(async move {
                        let svc =
                            service_fn(move |req| handle(req, body.clone(), counter.clone()));
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, svc)
                            .await;
                    });
                }
            });
            format!("http://127.0.0.1:{}/jwks", addr.port())
        }
    }

    /// Sign a token with `priv_key` carrying a `kid` header.
    fn sign_with_kid(interp: &Interp, priv_key: &Value, kid: &str, alg: &str) -> String {
        let claims = obj(&[("sub", s("alice"))]);
        let opts = obj(&[("alg", s(alg)), ("headers", obj(&[("kid", s(kid))]))]);
        token_str(&interp.jwt_sign(&[claims, priv_key.clone(), opts], sp()).unwrap())
    }

    // (a) jwks(url) → cache; cache.verify resolves kid → RSA key → RS256 verifies.
    #[tokio::test]
    async fn jwks_fetch_and_verify_rsa() {
        let interp = ip();
        let jwks = serde_json::json!({"keys":[rsa_jwk_json("rsa-1")]}).to_string();
        let counter = Arc::new(AtomicUsize::new(0));
        let url = jwks_fixture::start(jwks, counter.clone()).await;

        let priv_key = interp.call_jwt("rsaPrivateKey", &[s(RSA_PRIV)], sp()).unwrap();
        let tok = sign_with_kid(&interp, &priv_key, "rsa-1", "RS256");

        let cache = {
            let (c, e) = pair(&interp.call_jwt_async("jwks", &[s(&url)], sp()).await.unwrap());
            assert!(matches!(e.kind(), ValueKind::Nil), "jwks fetch failed");
            c
        };
        assert_eq!(counter.load(Ordering::SeqCst), 1, "one fetch at construction");
        let v = jwks_verify_via_handle(&interp, &cache, &tok).await;
        let (claims, err) = pair(&v);
        assert!(matches!(err.kind(), ValueKind::Nil), "RS256 jwks verify failed: {}", err_msg(&v));
        assert_eq!(claim_str(&claims, "sub"), Some("alice".to_string()));
        // verify within TTL did NOT refetch.
        assert_eq!(counter.load(Ordering::SeqCst), 1, "verify within TTL must not refetch");
    }

    // (a') EC P-256 / ES256 over JWKS.
    #[tokio::test]
    async fn jwks_fetch_and_verify_ec() {
        let interp = ip();
        let jwks = serde_json::json!({"keys":[ec_jwk_json("ec-1")]}).to_string();
        let counter = Arc::new(AtomicUsize::new(0));
        let url = jwks_fixture::start(jwks, counter.clone()).await;

        let priv_key = interp.call_jwt("ecPrivateKey", &[s(EC_PRIV)], sp()).unwrap();
        let tok = sign_with_kid(&interp, &priv_key, "ec-1", "ES256");
        let (cache, _) = pair(&interp.call_jwt_async("jwks", &[s(&url)], sp()).await.unwrap());
        let v = jwks_verify_via_handle(&interp, &cache, &tok).await;
        assert!(!is_err(&v), "ES256 jwks verify failed: {}", err_msg(&v));
    }

    // (b) unknown kid → exactly ONE refetch; still missing → Tier-1.
    #[tokio::test]
    async fn jwks_unknown_kid_refetches_once_then_tier1() {
        let interp = ip();
        let jwks = serde_json::json!({"keys":[rsa_jwk_json("rsa-1")]}).to_string();
        let counter = Arc::new(AtomicUsize::new(0));
        let url = jwks_fixture::start(jwks, counter.clone()).await;

        let priv_key = interp.call_jwt("rsaPrivateKey", &[s(RSA_PRIV)], sp()).unwrap();
        // Sign with a kid the JWKS does NOT contain.
        let tok = sign_with_kid(&interp, &priv_key, "missing-kid", "RS256");
        let (cache, _) = pair(&interp.call_jwt_async("jwks", &[s(&url)], sp()).await.unwrap());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        let v = jwks_verify_via_handle(&interp, &cache, &tok).await;
        assert!(is_err(&v), "unknown kid must be Tier-1");
        assert!(err_msg(&v).contains("missing-kid"), "got: {}", err_msg(&v));
        // EXACTLY one refetch on the unknown kid (1 construction + 1 refetch = 2).
        assert_eq!(counter.load(Ordering::SeqCst), 2, "unknown kid → exactly one refetch");
    }

    // (c) TTL expiry refetches; a verify within TTL does NOT.
    #[tokio::test]
    async fn jwks_ttl_expiry_refetches() {
        let interp = ip();
        let jwks = serde_json::json!({"keys":[rsa_jwk_json("rsa-1")]}).to_string();
        let counter = Arc::new(AtomicUsize::new(0));
        let url = jwks_fixture::start(jwks, counter.clone()).await;
        let priv_key = interp.call_jwt("rsaPrivateKey", &[s(RSA_PRIV)], sp()).unwrap();
        let tok = sign_with_kid(&interp, &priv_key, "rsa-1", "RS256");
        // ttlMs = 0 forces the TTL to be considered expired on every verify (>= cmp).
        let opts = obj(&[("ttlMs", Value::int(1))]);
        let (cache, _) = pair(&interp.call_jwt_async("jwks", &[s(&url), opts], sp()).await.unwrap());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        // Sleep so the 1ms TTL is surely past.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let v = jwks_verify_via_handle(&interp, &cache, &tok).await;
        assert!(!is_err(&v), "verify failed: {}", err_msg(&v));
        assert!(counter.load(Ordering::SeqCst) >= 2, "TTL expiry must refetch");
    }

    // (d) malformed JWKS → Tier-1; mixed JWKS (oct skipped) still serves RSA/EC.
    #[tokio::test]
    async fn jwks_malformed_and_mixed() {
        // malformed JSON → Tier-1 at construction.
        let interp = ip();
        let counter = Arc::new(AtomicUsize::new(0));
        let url = jwks_fixture::start("{not json".to_string(), counter.clone()).await;
        let (c, e) = pair(&interp.call_jwt_async("jwks", &[s(&url)], sp()).await.unwrap());
        assert!(matches!(c.kind(), ValueKind::Nil) && !matches!(e.kind(), ValueKind::Nil),
            "malformed JWKS must be Tier-1");

        // mixed: an unsupported `oct` key + a real RSA key. The oct is skipped, the
        // RSA one is usable.
        let interp = ip();
        let mixed = serde_json::json!({"keys":[
            {"kty":"oct","kid":"sym-1","k":"AAAA"},
            {"kty":"RSA","kid":"unsupported-no-kid-test"}, // missing n/e → skipped
            rsa_jwk_json("rsa-1"),
        ]}).to_string();
        let counter = Arc::new(AtomicUsize::new(0));
        let url = jwks_fixture::start(mixed, counter.clone()).await;
        let priv_key = interp.call_jwt("rsaPrivateKey", &[s(RSA_PRIV)], sp()).unwrap();
        let tok = sign_with_kid(&interp, &priv_key, "rsa-1", "RS256");
        let (cache, _) = pair(&interp.call_jwt_async("jwks", &[s(&url)], sp()).await.unwrap());
        let v = jwks_verify_via_handle(&interp, &cache, &tok).await;
        assert!(!is_err(&v), "RSA key in a mixed JWKS must be usable: {}", err_msg(&v));
    }

    // THE alg-confusion kill carries over JWKS: an RSA JWKS key cannot HS256-verify.
    #[tokio::test]
    async fn jwks_rsa_key_cannot_hmac_verify() {
        let interp = ip();
        let jwks = serde_json::json!({"keys":[rsa_jwk_json("rsa-1")]}).to_string();
        let counter = Arc::new(AtomicUsize::new(0));
        let url = jwks_fixture::start(jwks, counter.clone()).await;
        // Forge an HS256 token whose kid points at the RSA JWKS key.
        let hmac_key = jwt_hmac_key(&[s("anything")], sp()).unwrap();
        let claims = obj(&[("sub", s("attacker"))]);
        let opts = obj(&[("alg", s("HS256")), ("headers", obj(&[("kid", s("rsa-1"))]))]);
        let hs_tok = token_str(&interp.jwt_sign(&[claims, hmac_key, opts], sp()).unwrap());
        let (cache, _) = pair(&interp.call_jwt_async("jwks", &[s(&url)], sp()).await.unwrap());
        let v = jwks_verify_via_handle(&interp, &cache, &hs_tok).await;
        assert!(is_err(&v), "RSA JWKS key + HS256 must be rejected (alg-confusion kill)");
    }

    /// Drive `cache.verify(token)` through the native-method dispatch.
    async fn jwks_verify_via_handle(interp: &Interp, cache: &Value, token: &str) -> Value {
        let id = match cache.kind() {
            ValueKind::Native(n) => n.id,
            _ => panic!("jwks() should return a native handle"),
        };
        let _ = id;
        let m = std::rc::Rc::new(crate::value::NativeMethod {
            receiver: match cache.kind() {
                ValueKind::Native(n) => n.clone(),
                _ => unreachable!(),
            },
            method: "verify".to_string(),
        });
        interp.call_jwks_method(&m, vec![Value::str(token)], sp()).await.unwrap()
    }
}
