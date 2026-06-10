//! DBG Task 7 — the CPU sampling profiler's aggregation + output.
//!
//! The VM publishes a frame-name stack at each frame push/pop (see
//! [`crate::vm::run::Vm::publish_profile_frames`]); a sampler (a wall-clock thread, or
//! the inline deterministic recorder) collects those snapshots into a
//! `Vec<Vec<String>>` — one root→leaf path per sample. This module turns that raw
//! sample set into a profile artifact:
//!
//! - [`format_speedscope`] — a [speedscope](https://www.speedscope.app/) JSON document
//!   in the `sampled` profile shape (a flat frame table + one stack-of-frame-indices
//!   per sample). speedscope reconstructs the call tree from the sample stacks, so this
//!   is function-level (per-line is a documented follow-up).
//! - [`format_collapsed`] — Brendan-Gregg folded stacks (`a;b;c <count>` per line),
//!   the input format for flamegraph.pl and many viewers.
//!
//! # Determinism
//!
//! Under the deterministic sample clock (a sample per frame push, no wall-clock), the
//! sample set is a pure function of the program's call structure, so BOTH outputs are
//! byte-stable for a golden. The collapsed lines are emitted in first-seen path order;
//! the speedscope frame table is in first-seen frame order — neither depends on hashing
//! iteration order.

/// One output format for the aggregated profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfileFormat {
    /// speedscope JSON (the default; opens directly at speedscope.app).
    Speedscope,
    /// Brendan-Gregg collapsed/folded stacks (`a;b;c <count>`).
    Collapsed,
}

impl ProfileFormat {
    /// Parse a `--profile-format` value. `None` for an unknown name (the caller
    /// reports a clean error).
    pub fn parse(s: &str) -> Option<ProfileFormat> {
        match s {
            "speedscope" => Some(ProfileFormat::Speedscope),
            "collapsed" => Some(ProfileFormat::Collapsed),
            _ => None,
        }
    }
}

/// Render the samples in the requested format (the single entry point the CLI uses).
pub fn format_samples(samples: &[Vec<String>], format: ProfileFormat, name: &str) -> String {
    match format {
        ProfileFormat::Speedscope => format_speedscope(samples, name),
        ProfileFormat::Collapsed => format_collapsed(samples),
    }
}

/// Aggregate the raw samples into ordered `(path, count)` pairs, where `path` is the
/// joined `;`-separated frame names. Order is FIRST-SEEN (deterministic — never hash
/// order), so the collapsed golden is byte-stable. An empty path (a sample with no
/// frames) is skipped.
fn folded_counts(samples: &[Vec<String>]) -> Vec<(String, usize)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for stack in samples {
        if stack.is_empty() {
            continue;
        }
        let key = stack.join(";");
        match counts.get_mut(&key) {
            Some(c) => *c += 1,
            None => {
                counts.insert(key.clone(), 1);
                order.push(key);
            }
        }
    }
    order
        .into_iter()
        .map(|k| {
            let c = counts[&k];
            (k, c)
        })
        .collect()
}

/// Brendan-Gregg folded stacks: one `frame;frame;frame <count>` line per distinct
/// root→leaf path, in first-seen order. Stable for a golden.
pub fn format_collapsed(samples: &[Vec<String>]) -> String {
    let folded = folded_counts(samples);
    let mut out = String::new();
    for (path, count) in folded {
        out.push_str(&path);
        out.push(' ');
        out.push_str(&count.to_string());
        out.push('\n');
    }
    out
}

/// Emit a speedscope JSON document (`$schema` = speedscope's file format) in the
/// `sampled` profile shape: a flat `shared.frames` table (`{name}` per frame, in
/// first-seen order) plus one `sampled` profile whose `samples` are arrays of frame
/// indices (root→leaf) and whose `weights` are all `1` (one weight unit per sample).
///
/// Function-level only (v1); the frame table carries names, not source lines (a
/// documented follow-up). Built with `serde_json` so the JSON is always well-formed
/// and escaping is correct.
pub fn format_speedscope(samples: &[Vec<String>], name: &str) -> String {
    // First-seen frame table: name -> index, preserving discovery order.
    let mut frame_index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut frame_order: Vec<&str> = Vec::new();
    for stack in samples {
        for f in stack {
            if !frame_index.contains_key(f.as_str()) {
                frame_index.insert(f.as_str(), frame_order.len());
                frame_order.push(f.as_str());
            }
        }
    }

    let frames: Vec<serde_json::Value> = frame_order
        .iter()
        .map(|n| serde_json::json!({ "name": n }))
        .collect();

    // One sample = one array of frame indices (root → leaf). Skip empty samples.
    let mut sample_stacks: Vec<serde_json::Value> = Vec::new();
    let mut weights: Vec<serde_json::Value> = Vec::new();
    for stack in samples {
        if stack.is_empty() {
            continue;
        }
        let idxs: Vec<serde_json::Value> = stack
            .iter()
            .map(|f| serde_json::json!(frame_index[f.as_str()]))
            .collect();
        sample_stacks.push(serde_json::Value::Array(idxs));
        weights.push(serde_json::json!(1));
    }

    let total: usize = sample_stacks.len();
    let doc = serde_json::json!({
        "$schema": "https://www.speedscope.app/file-format-schema.json",
        "shared": { "frames": frames },
        "profiles": [
            {
                "type": "sampled",
                "name": name,
                "unit": "none",
                "startValue": 0,
                "endValue": total,
                "samples": sample_stacks,
                "weights": weights,
            }
        ],
        "name": name,
        "exporter": "ascript",
        "activeProfileIndex": 0,
    });
    // Pretty-print so the golden is human-diffable; serde_json preserves the insertion
    // order of the json! macro (the `preserve_order` feature is on for `data`/`dap`).
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn collapsed_counts_and_order_are_stable() {
        let samples = vec![
            s(&["<script>", "a"]),
            s(&["<script>", "a", "b"]),
            s(&["<script>", "a"]),
            s(&["<script>", "a", "b"]),
            s(&["<script>", "a", "b", "c"]),
        ];
        let out = format_collapsed(&samples);
        // First-seen order: a (2), a;b (2), a;b;c (1).
        assert_eq!(
            out,
            "<script>;a 2\n<script>;a;b 2\n<script>;a;b;c 1\n"
        );
    }

    #[test]
    fn empty_samples_yield_empty_collapsed() {
        assert_eq!(format_collapsed(&[]), "");
        // A single empty stack is skipped.
        assert_eq!(format_collapsed(&[Vec::new()]), "");
    }

    #[test]
    fn speedscope_is_valid_json_with_frame_table() {
        let samples = vec![s(&["<script>", "a"]), s(&["<script>", "a", "b"])];
        let json = format_speedscope(&samples, "prog");
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["profiles"][0]["type"], "sampled");
        // Frame table in first-seen order: <script>, a, b.
        assert_eq!(v["shared"]["frames"][0]["name"], "<script>");
        assert_eq!(v["shared"]["frames"][1]["name"], "a");
        assert_eq!(v["shared"]["frames"][2]["name"], "b");
        // Two samples, each weight 1.
        assert_eq!(v["profiles"][0]["samples"].as_array().unwrap().len(), 2);
        assert_eq!(v["profiles"][0]["samples"][0], serde_json::json!([0, 1]));
        assert_eq!(v["profiles"][0]["samples"][1], serde_json::json!([0, 1, 2]));
        assert_eq!(v["profiles"][0]["endValue"], 2);
    }

    #[test]
    fn empty_profile_is_valid_json() {
        let json = format_speedscope(&[], "empty");
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["profiles"][0]["samples"].as_array().unwrap().len(), 0);
        assert_eq!(v["shared"]["frames"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn format_parse_roundtrip() {
        assert_eq!(ProfileFormat::parse("speedscope"), Some(ProfileFormat::Speedscope));
        assert_eq!(ProfileFormat::parse("collapsed"), Some(ProfileFormat::Collapsed));
        assert_eq!(ProfileFormat::parse("nope"), None);
    }
}
