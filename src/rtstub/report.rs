//! RT §4.6 / §9.2 — the native build report (stderr human form + `--report-json`).
//!
//! [`BuildReport`] is the machine-and-human twin of the shake report. It carries the
//! §9.2 fields fillable at this stage (selection, payload, output, caps, module counts);
//! fetch/oci sub-objects arrive with later tasks (defaulted/omitted for now).
//!
//! **Determinism (§9.1):** the report contains NO timestamps — building the same source
//! twice yields byte-identical JSON. [`to_json`](BuildReport::to_json) is hand-rolled
//! (serde-free) so it builds under `--no-default-features` (no `serde_json`/`data`
//! feature), with insertion-ordered keys by construction.
//!
//! Compiled only in the TOOLCHAIN build (`#[cfg(not(ascript_rt))]`).

use super::select::{Selection, TierSource};
use super::tiers::Tier;

/// The payload framing of a built bundle (the §9.2 `payload` sub-object).
#[derive(Debug, Clone)]
pub struct PayloadInfo {
    /// `"aso"` (bare single-module chunk) or `"archive"` (`ASCRIPTA` graph).
    pub format: &'static str,
    /// Whether the payload region is zstd-compressed (`--compress`).
    pub compressed: bool,
    /// The on-disk payload region size (compressed size when `compressed`).
    pub size: u64,
    /// The pre-compression size; equals `size` when not compressed.
    pub uncompressed_size: u64,
    /// sha256 of the payload bytes as they appear in the artifact.
    pub sha256: String,
}

/// Where the stub the payload was appended to came from (the §9.2 `stub.origin`).
#[derive(Debug, Clone)]
pub struct StubInfo {
    /// A short origin tag: `"current_exe"` (today's ladder rung), later `"cache"`,
    /// `"fetch"`, `"--stub"`, `"--exact"`, `"sibling"`.
    pub origin: &'static str,
    /// sha256 of the (clean) stub bytes.
    pub sha256: String,
    /// The stub's byte size.
    pub size: u64,
}

/// The native build report (§4.6 stderr + §9.2 JSON).
#[derive(Debug, Clone)]
pub struct BuildReport {
    /// The source file path (as passed on the CLI).
    pub source: String,
    /// The output artifact path.
    pub output: String,
    /// sha256 of the FINAL artifact bytes (stub || payload || footer).
    pub output_sha256: String,
    /// The target triple (`None` ⇒ the host; Task 7 fills cross targets).
    pub target: Option<String>,
    /// The chosen tier.
    pub tier: Tier,
    /// How the tier was chosen.
    pub tier_source: TierSource,
    /// The required / stub / unused feature breakdown (from [`Selection`]).
    pub selection: Selection,
    /// The payload framing.
    pub payload: PayloadInfo,
    /// The stub origin + identity.
    pub stub: StubInfo,
    /// Number of modules embedded in the payload.
    pub module_count: usize,
    /// The reproducible shake digest (hex), or `None` for a bare single-module chunk.
    pub shake_digest: Option<String>,
    /// Whether the embedded caps are all-granted (`true`) or restricted (`false`).
    pub caps_all_granted: bool,
}

impl BuildReport {
    /// Render the §4.6 human report to a `String` (the caller writes it to stderr — the
    /// `bundled … -> …` line stays on stdout).
    pub fn render_stderr(&self) -> String {
        let mut s = String::new();
        s.push_str("native build report:\n");
        s.push_str(&format!(
            "  tier:    {} ({})\n",
            self.tier.name(),
            self.tier_source.as_str(),
        ));
        s.push_str(&format!(
            "  stub:    {} ({} bytes, sha256 {})\n",
            self.stub.origin,
            self.stub.size,
            &short(&self.stub.sha256),
        ));
        if let Some(t) = &self.target {
            s.push_str(&format!("  target:  {t}\n"));
        }
        s.push_str(&format!(
            "  payload: {} ({}{} bytes, sha256 {})\n",
            self.payload.format,
            if self.payload.compressed {
                format!("{} -> ", self.payload.uncompressed_size)
            } else {
                String::new()
            },
            self.payload.size,
            &short(&self.payload.sha256),
        ));
        s.push_str(&format!(
            "  features: required [{}]\n",
            self.selection.required.join(", "),
        ));
        if self.selection.unused.is_empty() {
            s.push_str("            unused   [] (tier fits exactly)\n");
        } else {
            s.push_str(&format!(
                "            unused   [{}] (rebuild with --exact to trim)\n",
                self.selection.unused.join(", "),
            ));
        }
        s.push_str(&format!(
            "  modules: {} (caps: {})\n",
            self.module_count,
            if self.caps_all_granted { "all-granted" } else { "restricted" },
        ));
        s.push_str(&format!(
            "  output:  {} (sha256 {})\n",
            self.output,
            &short(&self.output_sha256),
        ));
        s
    }

    /// Render the canonical §9.2 JSON document (`"schema": 1`). Hand-rolled, serde-free,
    /// insertion-ordered, with NO timestamp field (determinism §9.1).
    pub fn to_json(&self) -> String {
        let mut j = JsonObj::new();
        j.num("schema", 1);
        j.str("source", &self.source);
        j.str("output", &self.output);
        j.str("output_sha256", &self.output_sha256);
        match &self.target {
            Some(t) => j.str("target", t),
            None => j.null("target"),
        }
        j.str("tier", self.tier.name());
        j.str("tier_source", self.tier_source.as_str());
        j.arr("required", &self.selection.required);
        j.arr("stub_features", &self.selection.stub);
        j.arr("unused", &self.selection.unused);

        // stub sub-object
        let mut stub = JsonObj::new();
        stub.str("origin", self.stub.origin);
        stub.str("sha256", &self.stub.sha256);
        stub.num("size", self.stub.size as i64);
        j.obj("stub", stub);

        // payload sub-object
        let mut payload = JsonObj::new();
        payload.str("format", self.payload.format);
        payload.boolean("compressed", self.payload.compressed);
        payload.num("size", self.payload.size as i64);
        payload.num("uncompressed_size", self.payload.uncompressed_size as i64);
        payload.str("sha256", &self.payload.sha256);
        j.obj("payload", payload);

        j.num("module_count", self.module_count as i64);
        match &self.shake_digest {
            Some(d) => j.str("shake_digest", d),
            None => j.null("shake_digest"),
        }
        j.boolean("caps_all_granted", self.caps_all_granted);

        j.finish()
    }
}

/// Shorten a hex digest for the human report (first 12 chars).
fn short(hex: &str) -> String {
    if hex.len() > 12 {
        format!("{}…", &hex[..12])
    } else {
        hex.to_string()
    }
}

// ─── Tiny hand-rolled JSON object builder (serde-free, insertion-ordered) ─────────

/// A minimal insertion-ordered JSON object emitter. Keeps key order exactly as fields
/// are added (the §9.2 canonical order) and escapes strings per JSON. No floats, no
/// timestamps — every value is a string, integer, bool, null, array-of-strings, or a
/// nested object, which is all §9.2 needs.
struct JsonObj {
    parts: Vec<String>,
}

impl JsonObj {
    fn new() -> JsonObj {
        JsonObj { parts: Vec::new() }
    }

    fn key(k: &str) -> String {
        format!("\"{}\":", esc(k))
    }

    fn str(&mut self, k: &str, v: &str) {
        self.parts.push(format!("{}\"{}\"", Self::key(k), esc(v)));
    }

    fn num(&mut self, k: &str, v: i64) {
        self.parts.push(format!("{}{}", Self::key(k), v));
    }

    fn boolean(&mut self, k: &str, v: bool) {
        self.parts.push(format!("{}{}", Self::key(k), v));
    }

    fn null(&mut self, k: &str) {
        self.parts.push(format!("{}null", Self::key(k)));
    }

    fn arr(&mut self, k: &str, items: &[String]) {
        let body = items
            .iter()
            .map(|s| format!("\"{}\"", esc(s)))
            .collect::<Vec<_>>()
            .join(",");
        self.parts.push(format!("{}[{}]", Self::key(k), body));
    }

    fn obj(&mut self, k: &str, v: JsonObj) {
        self.parts.push(format!("{}{}", Self::key(k), v.finish()));
    }

    fn finish(self) -> String {
        format!("{{{}}}", self.parts.join(","))
    }
}

/// JSON string escaping (the subset needed for paths/hex/feature names — control chars
/// can appear in arbitrary file paths, so escape the mandatory ones).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
