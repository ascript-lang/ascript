//! RT §5.1 — the signed, versioned release manifest + its four-check verifier.
//!
//! ```json
//! { "schema": 1, "ascript": "0.6.0", "created": "…",
//!   "stubs": [ { "target": "x86_64-unknown-linux-musl", "tier": "rt-net",
//!                "features": ["shared","bundle-zstd","data",…],
//!                "sha256": "<hex>", "size": 12345678,
//!                "filename": "ascript-rt-0.6.0-x86_64-unknown-linux-musl-rt-net" } … ] }
//! ```
//!
//! **The manifest bytes are UNTRUSTED.** The parser ([`parse_manifest`]) is a
//! hand-rolled, allocation-bounded JSON reader (no `serde` dependency, so the
//! `--no-default-features` build parses manifests too) with NO reachable panic or
//! unwrap on hostile input — a malformed manifest is a clean `Err(String)`.
//!
//! [`verify_manifest`] (gated on `rt-fetch`) runs ALL FOUR §5.1 checks; any failure
//! refuses, naming the reason. The signature is checked over the EXACT manifest bytes
//! against a compiled-in ed25519 public key — there is no insecure escape hatch.

/// RT §5.1: production release pubkey — rotated only by a toolchain release (Task 11 sets the real key).
///
/// A 32-byte ed25519 verifying key compiled into the toolchain. This placeholder is
/// all-zero until the real release key is minted; a real signed manifest will not
/// verify against it (fail-closed by construction), and the dev fallbacks (§5.4
/// `--stub`/sibling) are the offline path — never an insecure env knob.
pub const PRODUCTION_PUBKEY: [u8; 32] = [0u8; 32];

/// The current manifest schema version this toolchain understands.
pub const SCHEMA_VERSION: u64 = 1;

/// One stub entry in the release manifest (RT §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubEntry {
    /// The Rust target triple this stub was built for (e.g. `x86_64-unknown-linux-musl`).
    pub target: String,
    /// The tier name (`rt-core`/`rt-local`/`rt-net`/`rt-full`).
    pub tier: String,
    /// The cumulative Cargo feature set the stub was built with.
    pub features: Vec<String>,
    /// Lowercase-hex sha256 of the stub blob.
    pub sha256: String,
    /// The stub blob's exact size in bytes (the byte pin, with `sha256`).
    pub size: u64,
    /// The published blob filename under `{base}/v{version}/`.
    pub filename: String,
}

/// A parsed release manifest (RT §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtManifest {
    /// Manifest schema version (must be [`SCHEMA_VERSION`]).
    pub schema: u64,
    /// The toolchain version this release was cut for (the §5.1 check-2 version lock).
    pub ascript: String,
    /// An ISO-8601 creation timestamp (informational; not security-bearing).
    pub created: String,
    /// The published stubs.
    pub stubs: Vec<StubEntry>,
}

impl RtManifest {
    /// Find the entry for `(target, tier)` — RT §5.1 check 3. Returns `None` if no
    /// entry matches (the caller refuses).
    pub fn entry_for(&self, target: &str, tier: &str) -> Option<&StubEntry> {
        self.stubs.iter().find(|e| e.target == target && e.tier == tier)
    }
}

// ---------------------------------------------------------------------------
// Hand-rolled, bounds-checked JSON parsing (no serde; parses under --no-default).
// ---------------------------------------------------------------------------

/// Parse a release manifest from UNTRUSTED bytes. Fail-closed: any structural or
/// type error is a clean `Err`, never a panic. Rejects non-UTF-8, a wrong/missing
/// `schema`, missing required fields, or a non-integer/overflowing `size`.
pub fn parse_manifest(bytes: &[u8]) -> Result<RtManifest, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "manifest is not valid UTF-8".to_string())?;
    // Hard upper bound on input size to keep parsing costs bounded (a release manifest
    // is small; a megabyte of stubs is already absurd).
    if text.len() > 1 << 20 {
        return Err("manifest is implausibly large (>1 MiB) — refusing".to_string());
    }
    let mut p = Parser::new(text);
    let value = p.parse_value()?;
    p.skip_ws();
    if !p.at_end() {
        return Err("manifest has trailing content after the JSON value".to_string());
    }
    let obj = value.as_object().ok_or("manifest root is not a JSON object")?;

    let schema = obj
        .field("schema")
        .and_then(Json::as_u64)
        .ok_or("manifest is missing an integer 'schema'")?;
    if schema != SCHEMA_VERSION {
        return Err(format!(
            "manifest schema {schema} is unsupported (this toolchain understands schema {SCHEMA_VERSION})"
        ));
    }
    let ascript = obj
        .field("ascript")
        .and_then(Json::as_str)
        .ok_or("manifest is missing a string 'ascript' version")?
        .to_string();
    let created = obj
        .field("created")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();

    let stubs_arr = obj
        .field("stubs")
        .and_then(Json::as_array)
        .ok_or("manifest is missing a 'stubs' array")?;
    let mut stubs = Vec::with_capacity(stubs_arr.len().min(1024));
    for (i, entry) in stubs_arr.iter().enumerate() {
        let e = entry
            .as_object()
            .ok_or_else(|| format!("manifest stub entry #{i} is not an object"))?;
        let field_str = |name: &str| -> Result<String, String> {
            e.field(name)
                .and_then(Json::as_str)
                .map(|s| s.to_string())
                .ok_or_else(|| format!("manifest stub entry #{i} is missing string '{name}'"))
        };
        let target = field_str("target")?;
        let tier = field_str("tier")?;
        let sha256 = field_str("sha256")?;
        let filename = field_str("filename")?;
        let size = e
            .field("size")
            .and_then(Json::as_u64)
            .ok_or_else(|| format!("manifest stub entry #{i} is missing an integer 'size'"))?;
        let features = match e.field("features") {
            Some(Json::Array(items)) => {
                let mut feats = Vec::with_capacity(items.len());
                for f in items {
                    feats.push(
                        f.as_str()
                            .ok_or_else(|| {
                                format!("manifest stub entry #{i} has a non-string feature")
                            })?
                            .to_string(),
                    );
                }
                feats
            }
            Some(_) => {
                return Err(format!("manifest stub entry #{i} 'features' is not an array"))
            }
            None => Vec::new(),
        };
        stubs.push(StubEntry { target, tier, features, sha256, size, filename });
    }

    Ok(RtManifest { schema, ascript, created, stubs })
}

// A minimal JSON value model — just enough for the manifest shape. `Null`/`Bool` are
// parsed (so a valid JSON document containing them is accepted) but never inspected by
// the manifest reader, hence the allow.
#[allow(dead_code)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Int(u64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(o) => Some(o),
            _ => None,
        }
    }
    fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Int(n) => Some(*n),
            // Accept an integral float (e.g. `size: 1.0`) defensively.
            Json::Num(f) if f.fract() == 0.0 && *f >= 0.0 && *f <= u64::MAX as f64 => {
                Some(*f as u64)
            }
            _ => None,
        }
    }
}

// A `&[(String, Json)]` "object" lookup helper. Named `field` (not `get`) to avoid
// colliding with the slice's inherent `get(usize)`.
trait ObjLookup {
    fn field(&self, key: &str) -> Option<&Json>;
}
impl ObjLookup for &[(String, Json)] {
    fn field(&self, key: &str) -> Option<&Json> {
        self.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    // A recursion-depth guard so a deeply-nested hostile manifest cannot overflow the
    // stack (fail-closed: too-deep is an Err, never a crash).
    depth: usize,
}

const MAX_DEPTH: usize = 64;

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Parser { bytes: text.as_bytes(), pos: 0, depth: 0 }
    }
    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    fn bump(&mut self) -> Option<u8> {
        let b = self.bytes.get(self.pos).copied();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }
    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }
    fn parse_value(&mut self) -> Result<Json, String> {
        if self.depth >= MAX_DEPTH {
            return Err("manifest JSON nests too deeply — refusing".to_string());
        }
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(Json::Str(self.parse_string()?)),
            Some(b't') | Some(b'f') => self.parse_bool(),
            Some(b'n') => self.parse_null(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!("unexpected character '{}' in manifest", c as char)),
            None => Err("unexpected end of manifest".to_string()),
        }
    }
    fn parse_object(&mut self) -> Result<Json, String> {
        self.depth += 1;
        self.expect(b'{')?;
        let mut out: Vec<(String, Json)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Json::Object(out));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err("expected a string key in manifest object".to_string());
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            let val = self.parse_value()?;
            out.push((key, val));
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b'}') => break,
                _ => return Err("expected ',' or '}' in manifest object".to_string()),
            }
        }
        self.depth -= 1;
        Ok(Json::Object(out))
    }
    fn parse_array(&mut self) -> Result<Json, String> {
        self.depth += 1;
        self.expect(b'[')?;
        let mut out: Vec<Json> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Json::Array(out));
        }
        loop {
            let val = self.parse_value()?;
            out.push(val);
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => break,
                _ => return Err("expected ',' or ']' in manifest array".to_string()),
            }
        }
        self.depth -= 1;
        Ok(Json::Array(out))
    }
    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err("unterminated string in manifest".to_string()),
                Some(b'"') => break,
                Some(b'\\') => match self.bump() {
                    Some(b'"') => s.push('"'),
                    Some(b'\\') => s.push('\\'),
                    Some(b'/') => s.push('/'),
                    Some(b'n') => s.push('\n'),
                    Some(b't') => s.push('\t'),
                    Some(b'r') => s.push('\r'),
                    Some(b'b') => s.push('\u{0008}'),
                    Some(b'f') => s.push('\u{000C}'),
                    Some(b'u') => {
                        let cp = self.parse_unicode_escape()?;
                        s.push(cp);
                    }
                    _ => return Err("invalid escape in manifest string".to_string()),
                },
                Some(b) if b < 0x80 => s.push(b as char),
                // Multi-byte UTF-8: re-decode from the original slice to keep chars intact.
                Some(_) => {
                    let start = self.pos - 1;
                    // Find the char boundary by decoding from `start`.
                    let rest = std::str::from_utf8(&self.bytes[start..])
                        .map_err(|_| "invalid UTF-8 in manifest string".to_string())?;
                    let ch = rest
                        .chars()
                        .next()
                        .ok_or("invalid UTF-8 in manifest string".to_string())?;
                    s.push(ch);
                    self.pos = start + ch.len_utf8();
                }
            }
        }
        Ok(s)
    }
    fn parse_unicode_escape(&mut self) -> Result<char, String> {
        let mut code: u32 = 0;
        for _ in 0..4 {
            let b = self.bump().ok_or("truncated \\u escape in manifest")?;
            let nib = match b {
                b'0'..=b'9' => (b - b'0') as u32,
                b'a'..=b'f' => (b - b'a' + 10) as u32,
                b'A'..=b'F' => (b - b'A' + 10) as u32,
                _ => return Err("invalid hex in \\u escape in manifest".to_string()),
            };
            code = code * 16 + nib;
        }
        char::from_u32(code).ok_or_else(|| "invalid code point in manifest \\u escape".to_string())
    }
    fn parse_bool(&mut self) -> Result<Json, String> {
        if self.bytes[self.pos..].starts_with(b"true") {
            self.pos += 4;
            Ok(Json::Bool(true))
        } else if self.bytes[self.pos..].starts_with(b"false") {
            self.pos += 5;
            Ok(Json::Bool(false))
        } else {
            Err("invalid literal in manifest".to_string())
        }
    }
    fn parse_null(&mut self) -> Result<Json, String> {
        if self.bytes[self.pos..].starts_with(b"null") {
            self.pos += 4;
            Ok(Json::Null)
        } else {
            Err("invalid literal in manifest".to_string())
        }
    }
    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        let mut is_float = false;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while let Some(b) = self.peek() {
            match b {
                b'0'..=b'9' => self.pos += 1,
                b'.' | b'e' | b'E' | b'+' | b'-' => {
                    is_float = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| "invalid number in manifest".to_string())?;
        if !is_float {
            if let Ok(n) = text.parse::<u64>() {
                return Ok(Json::Int(n));
            }
        }
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("invalid number '{text}' in manifest"))
    }
    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' in manifest", c as char))
        }
    }
}

// ---------------------------------------------------------------------------
// RT §5.1 — the four-check verifier (network-side; gated on `rt-fetch`).
// ---------------------------------------------------------------------------

/// Verify a release manifest per RT §5.1, running ALL FOUR checks. Returns the parsed
/// manifest on success, or `Err(reason)` naming the FIRST failing check. Fail-closed:
/// any check failing refuses the manifest.
///
/// - **Check 1 (signature):** `signature` over the EXACT `manifest_bytes` against
///   `pubkey` (`VerifyingKey::verify_strict`). An empty/short/garbage signature, or a
///   signature by another key, is refused.
/// - **Check 2 (version lock):** `manifest.ascript == CARGO_PKG_VERSION` — the
///   downgrade/replay defense and the `ASO_FORMAT_VERSION` correctness lock.
///
/// Checks 3 (entry lookup) and 4 (byte pin) are performed by the caller against the
/// fetched blob ([`RtManifest::entry_for`] + the sha256/size comparison) so this fn
/// stays I/O-free.
#[cfg(feature = "rt-fetch")]
pub fn verify_manifest(
    manifest_bytes: &[u8],
    signature: &[u8],
    pubkey: &[u8; 32],
) -> Result<RtManifest, String> {
    use ed25519_dalek::{Signature, VerifyingKey};

    // Check 1: signature over the exact bytes.
    let vk = VerifyingKey::from_bytes(pubkey)
        .map_err(|e| format!("invalid release public key: {e}"))?;
    // An ed25519 signature is exactly 64 bytes; a short/empty (unsigned) sig is refused
    // here with a stable "signature" substring before we even touch the crypto.
    let sig_bytes: [u8; 64] = signature
        .try_into()
        .map_err(|_| "manifest signature is missing or malformed (expected 64 bytes)".to_string())?;
    let sig = Signature::from_bytes(&sig_bytes);
    vk.verify_strict(manifest_bytes, &sig)
        .map_err(|_| "manifest signature verification failed".to_string())?;

    // Parse only AFTER the signature is trusted (the bytes are now authentic).
    let manifest = parse_manifest(manifest_bytes)?;

    // Check 2: version lock.
    let expected = env!("CARGO_PKG_VERSION");
    if manifest.ascript != expected {
        return Err(format!(
            "manifest version mismatch: manifest is for ascript '{}' but this toolchain is '{}' \
             — refusing (a stub must match the toolchain's ASO_FORMAT_VERSION)",
            manifest.ascript, expected
        ));
    }

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    // These run under BOTH feature configs (no `rt-fetch` needed): the parse half.
    const TARGET: &str = "x86_64-unknown-linux-musl";

    fn good_json(version: &str) -> String {
        format!(
            r#"{{"schema":1,"ascript":"{version}","created":"1970-01-01T00:00:00Z",
"stubs":[{{"target":"{TARGET}","tier":"rt-net",
"features":["shared","bundle-zstd","data"],
"sha256":"abc123","size":4242,"filename":"ascript-rt-x"}}]}}"#
        )
    }

    #[test]
    fn parse_happy() {
        let m = parse_manifest(good_json("9.9.9").as_bytes()).expect("parse");
        assert_eq!(m.schema, 1);
        assert_eq!(m.ascript, "9.9.9");
        assert_eq!(m.stubs.len(), 1);
        let e = &m.stubs[0];
        assert_eq!(e.target, TARGET);
        assert_eq!(e.tier, "rt-net");
        assert_eq!(e.size, 4242);
        assert_eq!(e.sha256, "abc123");
        assert!(e.features.contains(&"shared".to_string()));
        assert_eq!(m.entry_for(TARGET, "rt-net"), Some(e));
        assert!(m.entry_for(TARGET, "rt-core").is_none());
    }

    #[test]
    fn parse_rejects_non_utf8() {
        let bad = [0xff, 0xfe, 0x00];
        assert!(parse_manifest(&bad).is_err());
    }

    #[test]
    fn parse_rejects_wrong_schema() {
        let j = r#"{"schema":2,"ascript":"1.0.0","created":"","stubs":[]}"#;
        let err = parse_manifest(j.as_bytes()).unwrap_err();
        assert!(err.contains("schema"), "{err}");
    }

    #[test]
    fn parse_rejects_missing_fields() {
        // Missing 'ascript'.
        let j = r#"{"schema":1,"created":"","stubs":[]}"#;
        assert!(parse_manifest(j.as_bytes()).is_err());
        // Missing 'stubs'.
        let j2 = r#"{"schema":1,"ascript":"1.0.0","created":""}"#;
        assert!(parse_manifest(j2.as_bytes()).is_err());
        // Stub entry missing 'sha256'.
        let j3 = r#"{"schema":1,"ascript":"1.0.0","created":"","stubs":[{"target":"t","tier":"rt-core","size":1,"filename":"f"}]}"#;
        assert!(parse_manifest(j3.as_bytes()).is_err());
        // Stub entry with a non-integer 'size'.
        let j4 = r#"{"schema":1,"ascript":"1.0.0","created":"","stubs":[{"target":"t","tier":"rt-core","sha256":"x","size":"big","filename":"f"}]}"#;
        assert!(parse_manifest(j4.as_bytes()).is_err());
    }

    #[test]
    fn parse_rejects_malformed_json() {
        assert!(parse_manifest(b"{").is_err());
        assert!(parse_manifest(b"not json").is_err());
        assert!(parse_manifest(b"").is_err());
        assert!(parse_manifest(b"[]").is_err()); // root not an object
        assert!(parse_manifest(b"{\"schema\":1} trailing").is_err());
    }

    #[test]
    fn parse_rejects_deep_nesting() {
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert!(parse_manifest(deep.as_bytes()).is_err());
    }
}
