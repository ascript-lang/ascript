//! `std/semver` — SemVer 2.0.0 parse/compare/sort + a node-semver-SUBSET range
//! engine (`satisfies`/`maxSatisfying`). Hand-rolled, no dependency (BATT T2-2,
//! spec §12; the `semver` crate implements *Cargo* range semantics, not
//! node-semver's `^ ~ || x-range` forms, so it is REJECTED in favor of a hand
//! roll — §3 verdict).
//!
//! ## Tiering (load-bearing)
//!
//! - `semver.parse(v) -> [parsed, err]` and `semver.valid(v) -> bool` accept any
//!   string; a malformed version is a Tier-1 `[nil, err]` from `parse` (often
//!   user/config data).
//! - `semver.compare(a, b) -> -1|0|1` and `semver.sort(versions)` treat a
//!   malformed VERSION as **Tier-2** programmer misuse (a panic) — sorting/
//!   comparing assumes already-validated data.
//! - `semver.satisfies(version, range) -> [bool, err]` and
//!   `maxSatisfying`/`minSatisfying` treat a malformed RANGE as **Tier-1**
//!   `[nil, err]` (ranges are frequently external/config data); a malformed
//!   VERSION argument to them is Tier-2 (same posture as `compare`).
//!
//! ## SemVer 2.0.0 parsing (strict)
//!
//! `MAJOR.MINOR.PATCH[-prerelease][+build]`. The three core numbers are required
//! ASCII digit runs with **NO leading zeros** (`01.0.0` rejected). A leading `v`
//! (`v1.2.3`) is **REJECTED** — strict SemVer has no `v` prefix (pin §12). The
//! prerelease is a `.`-separated list of identifiers; a **numeric** identifier
//! (all digits) may not have a leading zero; an **alphanumeric** identifier is
//! `[0-9A-Za-z-]+`. Build metadata after `+` is `.`-separated `[0-9A-Za-z-]+`
//! and is IGNORED in precedence (`1.0.0+a` and `1.0.0+b` compare EQUAL — pin §12).
//!
//! ## Precedence (SemVer §11)
//!
//! Compare major, then minor, then patch numerically. A version WITH a
//! prerelease has LOWER precedence than the same core without one
//! (`1.0.0-alpha < 1.0.0`). Prerelease identifiers compare left-to-right:
//! numeric < alphanumeric; two numeric ids compare numerically; two alphanumeric
//! ids compare ASCII-lexically; if all preceding are equal the LONGER set of
//! fields wins (`1.0.0-alpha < 1.0.0-alpha.1`).
//!
//! ## Range subset (node-semver-compatible — documented VERBATIM)
//!
//! A range is `||`-separated comparator SETS (OR); within a set, space-separated
//! comparators are AND-ed. Each surface form desugars to a pair of primitive
//! `>= / < / > / <= / =` comparators:
//!
//! - **exact / comparators:** `=1.2.3`, `>1.2.3`, `>=1.2.3`, `<1.2.3`, `<=1.2.3`,
//!   bare `1.2.3` (= `=1.2.3`).
//! - **caret `^`** — "compatible, do not change the left-most non-zero":
//!   `^1.2.3` → `>=1.2.3 <2.0.0`; `^0.2.3` → `>=0.2.3 <0.3.0`;
//!   `^0.0.3` → `>=0.0.3 <0.0.4`. Partial/x carets:
//!   `^1.2.x`/`^1.2` → `>=1.2.0 <2.0.0`; `^0.0.x`/`^0.0` → `>=0.0.0 <0.1.0`;
//!   `^1.x`/`^1` → `>=1.0.0 <2.0.0`; `^0.x`/`^0` → `>=0.0.0 <1.0.0`.
//! - **tilde `~`** — "allow patch-level changes if a minor is specified":
//!   `~1.2.3` → `>=1.2.3 <1.3.0`; `~1.2` → `>=1.2.0 <1.3.0`;
//!   `~1` → `>=1.0.0 <2.0.0`.
//! - **x-ranges / partials:** `*` / `x` / empty → `>=0.0.0`;
//!   `1.x` / `1` → `>=1.0.0 <2.0.0`; `1.2.x` / `1.2` → `>=1.2.0 <1.3.0`.
//! - **hyphen ranges:** `1.2.3 - 2.3.4` → `>=1.2.3 <=2.3.4`. A partial LOW end
//!   floors to `.0` (`1.2 - …` → `>=1.2.0`). A partial HIGH end becomes an
//!   exclusive `<` of the next increment (`… - 2` → `<3.0.0`; `… - 2.3` →
//!   `<2.4.0`); a full HIGH end is inclusive `<=`.
//!
//! **The prerelease participation rule (node default, `includePrerelease:false`):**
//! a version that CARRIES a prerelease (`1.2.3-alpha`) can satisfy a comparator
//! ONLY if that comparator's tuple has the SAME `[major,minor,patch]` AND itself
//! carries a prerelease. So `1.2.3-alpha` satisfies `>=1.2.3-0` (same tuple,
//! comparator has a prerelease) but NOT `>=1.2.0` (no comparator in the set is a
//! prerelease at `1.2.3`). A non-prerelease version is unaffected by this rule.
//!
//! ## NOT supported (documented deferrals, spec §12 / §17)
//!
//! `workspace:` / `npm:` protocol specifiers; loose mode; node's
//! `includePrerelease:true` option (a recorded future). A range using any of
//! these is a Tier-1 malformed-range error.

#![cfg(feature = "semver")]

use super::{arg, bi};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, type_name, Control};
use crate::span::Span;
use crate::value::{Value, ValueKind};
use indexmap::IndexMap;
use std::cmp::Ordering;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("parse", bi("semver.parse")),
        ("valid", bi("semver.valid")),
        ("compare", bi("semver.compare")),
        ("sort", bi("semver.sort")),
        ("satisfies", bi("semver.satisfies")),
        ("maxSatisfying", bi("semver.maxSatisfying")),
        ("minSatisfying", bi("semver.minSatisfying")),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// The parsed version + prerelease identifiers.
// ─────────────────────────────────────────────────────────────────────────────

/// A single prerelease identifier: numeric (compares as a number) or
/// alphanumeric (compares ASCII-lexically). Numeric always sorts BELOW
/// alphanumeric (SemVer §11).
#[derive(Clone, Debug, PartialEq, Eq)]
enum PreId {
    Num(u64),
    Alpha(String),
}

impl PartialOrd for PreId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for PreId {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (PreId::Num(a), PreId::Num(b)) => a.cmp(b),
            (PreId::Alpha(a), PreId::Alpha(b)) => a.cmp(b),
            // numeric identifiers always have lower precedence than alphanumeric.
            (PreId::Num(_), PreId::Alpha(_)) => Ordering::Less,
            (PreId::Alpha(_), PreId::Num(_)) => Ordering::Greater,
        }
    }
}

/// A parsed SemVer 2.0.0 version.
#[derive(Clone, Debug)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// Empty = no prerelease (a release version).
    pre: Vec<PreId>,
    /// Build metadata, IGNORED in precedence — retained only for `parse` output.
    build: Vec<String>,
}

impl Version {
    fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
    /// `[major, minor, patch]` tuple (build/pre excluded).
    fn core(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
    /// SemVer §11 precedence — build metadata excluded.
    fn precedence(&self, other: &Version) -> Ordering {
        match self.core().cmp(&other.core()) {
            Ordering::Equal => {}
            ne => return ne,
        }
        // A release (empty pre) outranks any prerelease of the same core.
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            (false, false) => {}
        }
        // Field-by-field; a longer set of fields wins if all preceding equal.
        for (a, b) in self.pre.iter().zip(other.pre.iter()) {
            match a.cmp(b) {
                Ordering::Equal => continue,
                ne => return ne,
            }
        }
        self.pre.len().cmp(&other.pre.len())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Parsing.
// ─────────────────────────────────────────────────────────────────────────────

/// Is `s` a non-empty ASCII digit run with no leading zero (or exactly "0")?
fn is_numeric_id_strict(s: &str) -> bool {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    !(s.len() > 1 && s.starts_with('0'))
}

/// Parse a required numeric core field (major/minor/patch): digits, no leading
/// zero (unless the value is exactly `0`).
fn parse_core_num(s: &str) -> Result<u64, String> {
    if !is_numeric_id_strict(s) {
        return Err(format!("invalid numeric identifier '{s}'"));
    }
    s.parse::<u64>().map_err(|_| format!("numeric overflow in '{s}'"))
}

/// `[0-9A-Za-z-]+`
fn is_alphanum_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn parse_prerelease(s: &str) -> Result<Vec<PreId>, String> {
    let mut ids = Vec::new();
    for part in s.split('.') {
        if part.is_empty() {
            return Err("empty prerelease identifier".to_string());
        }
        let all_digits = part.bytes().all(|b| b.is_ascii_digit());
        if all_digits {
            if !is_numeric_id_strict(part) {
                return Err(format!(
                    "numeric prerelease identifier '{part}' must not have a leading zero"
                ));
            }
            let n = part
                .parse::<u64>()
                .map_err(|_| format!("numeric prerelease overflow in '{part}'"))?;
            ids.push(PreId::Num(n));
        } else {
            if !is_alphanum_id(part) {
                return Err(format!("invalid prerelease identifier '{part}'"));
            }
            ids.push(PreId::Alpha(part.to_string()));
        }
    }
    Ok(ids)
}

fn parse_build(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for part in s.split('.') {
        if !is_alphanum_id(part) {
            return Err(format!("invalid build identifier '{part}'"));
        }
        out.push(part.to_string());
    }
    Ok(out)
}

/// Parse a strict SemVer 2.0.0 version string. A leading `v` is REJECTED.
fn parse_version(input: &str) -> Result<Version, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("empty version string".to_string());
    }
    // No `v`/`V` prefix in strict SemVer.
    if s.starts_with('v') || s.starts_with('V') {
        return Err("leading 'v' is not valid in strict SemVer (use '1.2.3', not 'v1.2.3')".to_string());
    }

    // Split off build metadata first (`+`), then prerelease (`-`).
    let (core_pre, build) = match s.split_once('+') {
        Some((cp, b)) => (cp, Some(b)),
        None => (s, None),
    };
    let (core, pre) = match core_pre.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (core_pre, None),
    };

    let mut it = core.split('.');
    let major = parse_core_num(it.next().ok_or("missing major version")?)?;
    let minor = parse_core_num(it.next().ok_or("missing minor version")?)?;
    let patch = parse_core_num(it.next().ok_or("missing patch version")?)?;
    if it.next().is_some() {
        return Err(format!("too many version fields in '{core}'"));
    }

    let pre = match pre {
        Some(p) => parse_prerelease(p)?,
        None => Vec::new(),
    };
    let build = match build {
        Some(b) => parse_build(b)?,
        None => Vec::new(),
    };

    Ok(Version {
        major,
        minor,
        patch,
        pre,
        build,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Ranges — desugar to comparator sets (DNF: OR of AND-sets of comparators).
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
}

/// One primitive comparator: `<op> <bound>`.
#[derive(Clone, Debug)]
struct Comparator {
    op: Op,
    bound: Version,
}

impl Comparator {
    /// Does `v` satisfy this single comparator, with the node prerelease rule
    /// applied at the SET level (the per-comparator allowance is decided by the
    /// caller — here we do the raw numeric comparison only)?
    fn raw_matches(&self, v: &Version) -> bool {
        let ord = v.precedence(&self.bound);
        match self.op {
            Op::Lt => ord == Ordering::Less,
            Op::Lte => ord != Ordering::Greater,
            Op::Gt => ord == Ordering::Greater,
            Op::Gte => ord != Ordering::Less,
            Op::Eq => ord == Ordering::Equal,
        }
    }
}

/// An AND-set of comparators (one element of the OR'd DNF).
type CompSet = Vec<Comparator>;

/// A range = OR of comparator sets.
struct Range {
    sets: Vec<CompSet>,
}

/// A partially-specified version used while desugaring (x-ranges/partials).
struct Partial {
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: Vec<PreId>,
}

fn ver(major: u64, minor: u64, patch: u64) -> Version {
    Version {
        major,
        minor,
        patch,
        pre: Vec::new(),
        build: Vec::new(),
    }
}

/// Parse a (possibly partial / x-range) version token into a [`Partial`].
/// Returns None for the bare wildcards `*` / `x` / `X` / empty.
fn parse_partial(tok: &str) -> Result<Partial, String> {
    let tok = tok.trim();
    if tok.is_empty() || tok == "*" || tok == "x" || tok == "X" {
        return Ok(Partial {
            major: None,
            minor: None,
            patch: None,
            pre: Vec::new(),
        });
    }
    if tok.starts_with('v') || tok.starts_with('V') {
        return Err("leading 'v' is not valid in a range comparator".to_string());
    }
    // Strip build metadata (ignored), keep prerelease for the patch field.
    let (core_pre, _build) = match tok.split_once('+') {
        Some((cp, b)) => (cp, Some(b)),
        None => (tok, None),
    };
    let (core, pre_str) = match core_pre.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (core_pre, None),
    };

    let mut fields = core.split('.');
    let xfield = |f: Option<&str>| -> Result<Option<u64>, String> {
        match f {
            None => Ok(None),
            Some(s) if s == "x" || s == "X" || s == "*" => Ok(None),
            Some(s) => Ok(Some(parse_core_num(s)?)),
        }
    };
    let major = xfield(fields.next())?;
    let minor = xfield(fields.next())?;
    let patch = xfield(fields.next())?;
    if fields.next().is_some() {
        return Err(format!("too many fields in range token '{tok}'"));
    }
    // A wildcard before a concrete field is malformed (`1.x.3`).
    if major.is_none() && (minor.is_some() || patch.is_some()) {
        return Err(format!("malformed x-range '{tok}'"));
    }
    if minor.is_none() && patch.is_some() {
        return Err(format!("malformed x-range '{tok}'"));
    }
    let pre = match pre_str {
        Some(p) => parse_prerelease(p)?,
        None => Vec::new(),
    };
    Ok(Partial {
        major,
        minor,
        patch,
        pre,
    })
}

/// Build a concrete `Version` from a partial, flooring missing fields to 0 and
/// carrying any prerelease.
fn floor(p: &Partial) -> Version {
    Version {
        major: p.major.unwrap_or(0),
        minor: p.minor.unwrap_or(0),
        patch: p.patch.unwrap_or(0),
        pre: p.pre.clone(),
        build: Vec::new(),
    }
}

/// Desugar a plain (no-operator) x-range/partial token into a comparator set.
/// `*`/`x`/empty → `>=0.0.0`; `1` → `>=1.0.0 <2.0.0`; `1.2` → `>=1.2.0 <1.3.0`;
/// `1.2.3` → `=1.2.3`.
fn desugar_partial(p: &Partial) -> CompSet {
    match (p.major, p.minor, p.patch) {
        (None, _, _) => vec![Comparator {
            op: Op::Gte,
            bound: ver(0, 0, 0),
        }],
        (Some(maj), None, _) => vec![
            Comparator {
                op: Op::Gte,
                bound: ver(maj, 0, 0),
            },
            Comparator {
                op: Op::Lt,
                bound: ver(maj + 1, 0, 0),
            },
        ],
        (Some(maj), Some(min), None) => vec![
            Comparator {
                op: Op::Gte,
                bound: ver(maj, min, 0),
            },
            Comparator {
                op: Op::Lt,
                bound: ver(maj, min + 1, 0),
            },
        ],
        (Some(_), Some(_), Some(_)) => vec![Comparator {
            op: Op::Eq,
            bound: floor(p),
        }],
    }
}

/// Caret desugar — preserve the left-most non-zero element.
fn desugar_caret(p: &Partial) -> CompSet {
    let lower = floor(p);
    let maj = p.major.unwrap_or(0);
    let min = p.minor.unwrap_or(0);
    // Upper bound: increment the left-most non-zero field, considering missing
    // (x) fields per node semantics.
    let upper = if maj != 0 {
        ver(maj + 1, 0, 0)
    } else if min != 0 {
        ver(0, min + 1, 0)
    } else if let Some(pat) = p.patch.filter(|&x| x != 0) {
        // ^0.0.3 → <0.0.4
        ver(0, 0, pat + 1)
    } else if p.minor.is_some() && p.patch.is_some() {
        // ^0.0.0 → <0.0.1
        ver(0, 0, 1)
    } else if p.minor.is_some() {
        // ^0.0 / ^0.0.x → <0.1.0
        ver(0, 1, 0)
    } else {
        // ^0 / ^0.x → <1.0.0
        ver(1, 0, 0)
    };
    vec![
        Comparator {
            op: Op::Gte,
            bound: lower,
        },
        Comparator {
            op: Op::Lt,
            bound: upper,
        },
    ]
}

/// Tilde desugar — allow patch-level changes when a minor is specified, else
/// minor-level when only a major is specified.
fn desugar_tilde(p: &Partial) -> CompSet {
    let lower = floor(p);
    let maj = p.major.unwrap_or(0);
    let upper = match (p.minor, p.patch) {
        // ~1 → >=1.0.0 <2.0.0
        (None, _) => ver(maj + 1, 0, 0),
        // ~1.2 / ~1.2.3 → >=1.2.0 <1.3.0
        (Some(min), _) => ver(maj, min + 1, 0),
    };
    vec![
        Comparator {
            op: Op::Gte,
            bound: lower,
        },
        Comparator {
            op: Op::Lt,
            bound: upper,
        },
    ]
}

/// Hyphen-range high-end: a full triple is inclusive `<=`; a partial end is an
/// exclusive `<` of the next increment.
fn hyphen_high(p: &Partial) -> Comparator {
    match (p.major, p.minor, p.patch) {
        (Some(_), Some(_), Some(_)) => Comparator {
            op: Op::Lte,
            bound: floor(p),
        },
        (Some(maj), Some(min), None) => Comparator {
            op: Op::Lt,
            bound: ver(maj, min + 1, 0),
        },
        (Some(maj), None, _) => Comparator {
            op: Op::Lt,
            bound: ver(maj + 1, 0, 0),
        },
        (None, _, _) => Comparator {
            op: Op::Gte,
            bound: ver(0, 0, 0),
        },
    }
}

/// Parse one operator-prefixed comparator token (`>=1.2.3`, `^1.2`, `~1`, `*`,
/// `1.2.x`, …) into a comparator SET.
fn parse_comparator(tok: &str) -> Result<CompSet, String> {
    let tok = tok.trim();
    if tok.is_empty() {
        return Err("empty comparator".to_string());
    }
    if tok.starts_with("workspace:") || tok.starts_with("npm:") {
        return Err(format!("unsupported range protocol in '{tok}'"));
    }
    if let Some(rest) = tok.strip_prefix('^') {
        return Ok(desugar_caret(&parse_partial(rest)?));
    }
    if let Some(rest) = tok.strip_prefix('~') {
        // Tolerate `~>` (node maps it to `~`).
        let rest = rest.strip_prefix('>').unwrap_or(rest);
        return Ok(desugar_tilde(&parse_partial(rest)?));
    }
    // Two-char operators first.
    for (pre, op) in [(">=", Op::Gte), ("<=", Op::Lte)] {
        if let Some(rest) = tok.strip_prefix(pre) {
            return Ok(vec![Comparator {
                op,
                bound: floor(&parse_partial(rest)?),
            }]);
        }
    }
    for (pre, op) in [(">", Op::Gt), ("<", Op::Lt), ("=", Op::Eq)] {
        if let Some(rest) = tok.strip_prefix(pre) {
            return Ok(vec![Comparator {
                op,
                bound: floor(&parse_partial(rest)?),
            }]);
        }
    }
    // No operator → plain partial / x-range / exact.
    Ok(desugar_partial(&parse_partial(tok)?))
}

/// Parse a whole range string (`||` of space-AND comparator sets, incl. hyphen
/// ranges) into a [`Range`].
fn parse_range(input: &str) -> Result<Range, String> {
    let input = input.trim();
    let mut sets: Vec<CompSet> = Vec::new();
    // `*` / empty whole range → match-all.
    for or_part in input.split("||") {
        let part = or_part.trim();
        let set = parse_comparator_set(part)?;
        sets.push(set);
    }
    if sets.is_empty() {
        sets.push(vec![Comparator {
            op: Op::Gte,
            bound: ver(0, 0, 0),
        }]);
    }
    Ok(Range { sets })
}

/// Parse one OR-branch (a space-AND comparator set), handling hyphen ranges.
fn parse_comparator_set(part: &str) -> Result<CompSet, String> {
    let part = part.trim();
    if part.is_empty() {
        // Empty branch → match-all (`>=0.0.0`).
        return Ok(vec![Comparator {
            op: Op::Gte,
            bound: ver(0, 0, 0),
        }]);
    }
    let toks: Vec<&str> = part.split_whitespace().collect();

    // Hyphen range: `A - B` (exactly: tok, "-", tok).
    if let Some(hyphen_idx) = toks.iter().position(|t| *t == "-") {
        // Only support the canonical single-hyphen form.
        let low_toks = &toks[..hyphen_idx];
        let high_toks = &toks[hyphen_idx + 1..];
        if low_toks.len() != 1 || high_toks.len() != 1 {
            return Err(format!("malformed hyphen range '{part}'"));
        }
        let low = parse_partial(low_toks[0])?;
        let high = parse_partial(high_toks[0])?;
        return Ok(vec![
            Comparator {
                op: Op::Gte,
                bound: floor(&low),
            },
            hyphen_high(&high),
        ]);
    }

    let mut set: CompSet = Vec::new();
    for tok in toks {
        set.extend(parse_comparator(tok)?);
    }
    if set.is_empty() {
        set.push(Comparator {
            op: Op::Gte,
            bound: ver(0, 0, 0),
        });
    }
    Ok(set)
}

// ─────────────────────────────────────────────────────────────────────────────
// satisfies — OR over sets, AND within, with the node prerelease rule.
// ─────────────────────────────────────────────────────────────────────────────

fn set_matches(v: &Version, set: &CompSet) -> bool {
    // First, the raw AND of all comparators.
    if !set.iter().all(|c| c.raw_matches(v)) {
        return false;
    }
    // The prerelease participation rule: if the candidate carries a prerelease,
    // it may only satisfy the set if SOME comparator in this set is itself a
    // prerelease at the SAME [major,minor,patch] tuple.
    if v.is_prerelease() {
        let allowed = set
            .iter()
            .any(|c| c.bound.is_prerelease() && c.bound.core() == v.core());
        if !allowed {
            return false;
        }
    }
    true
}

fn satisfies(v: &Version, range: &Range) -> bool {
    range.sets.iter().any(|set| set_matches(v, set))
}

// ─────────────────────────────────────────────────────────────────────────────
// Value <-> parsed conversions.
// ─────────────────────────────────────────────────────────────────────────────

fn version_value(v: &Version) -> Value {
    let mut o: IndexMap<String, Value> = IndexMap::new();
    o.insert("major".to_string(), Value::int(v.major as i64));
    o.insert("minor".to_string(), Value::int(v.minor as i64));
    o.insert("patch".to_string(), Value::int(v.patch as i64));
    let pre: Vec<Value> = v
        .pre
        .iter()
        .map(|id| match id {
            PreId::Num(n) => Value::int(*n as i64),
            PreId::Alpha(s) => Value::str(s.as_str()),
        })
        .collect();
    o.insert("prerelease".to_string(), Value::array(pre));
    let build: Vec<Value> = v.build.iter().map(|s| Value::str(s.as_str())).collect();
    o.insert("build".to_string(), Value::array(build));
    Value::object(o)
}

// ─────────────────────────────────────────────────────────────────────────────
// Dispatch.
// ─────────────────────────────────────────────────────────────────────────────

impl crate::interp::Interp {
    pub(crate) async fn call_semver(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            // ── parse → Tier-1 [parsed, err] ───────────────────────────────
            "parse" => {
                let s = want_version_string(&arg(args, 0), span, "semver.parse")?;
                match parse_version(&s) {
                    Ok(v) => Ok(make_pair(version_value(&v), Value::nil())),
                    Err(e) => Ok(make_pair(
                        Value::nil(),
                        make_error(Value::str(format!("invalid version: {e}"))),
                    )),
                }
            }
            // ── valid → bool ───────────────────────────────────────────────
            "valid" => {
                let s = want_version_string(&arg(args, 0), span, "semver.valid")?;
                Ok(Value::bool_(parse_version(&s).is_ok()))
            }
            // ── compare → -1|0|1 ; malformed VERSION is Tier-2 ─────────────
            "compare" => {
                let a = parse_version_strict(&arg(args, 0), span, "semver.compare")?;
                let b = parse_version_strict(&arg(args, 1), span, "semver.compare")?;
                let ord = a.precedence(&b);
                Ok(Value::int(match ord {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                }))
            }
            // ── sort → array (ascending) ; malformed VERSION is Tier-2 ─────
            "sort" => {
                let raw = want_string_array(&arg(args, 0), span, "semver.sort")?;
                let mut parsed: Vec<(String, Version)> = Vec::with_capacity(raw.len());
                for s in &raw {
                    let v = parse_version(s).map_err(|e| {
                        Control::from(AsError::at(
                            format!("semver.sort: invalid version '{s}': {e}"),
                            span,
                        ))
                    })?;
                    parsed.push((s.clone(), v));
                }
                parsed.sort_by(|a, b| a.1.precedence(&b.1));
                Ok(Value::array(
                    parsed.into_iter().map(|(s, _)| Value::str(s)).collect(),
                ))
            }
            // ── satisfies → Tier-1 [bool, err] ; malformed RANGE is Tier-1 ──
            "satisfies" => {
                let v = parse_version_strict(&arg(args, 0), span, "semver.satisfies")?;
                let range_str = want_version_string(&arg(args, 1), span, "semver.satisfies")?;
                match parse_range(&range_str) {
                    Ok(r) => Ok(make_pair(Value::bool_(satisfies(&v, &r)), Value::nil())),
                    Err(e) => Ok(make_pair(
                        Value::nil(),
                        make_error(Value::str(format!("invalid range: {e}"))),
                    )),
                }
            }
            // ── maxSatisfying / minSatisfying → Tier-1 [string|nil, err] ───
            "maxSatisfying" | "minSatisfying" => {
                let want_max = func == "maxSatisfying";
                let raw = want_string_array(&arg(args, 0), span, &format!("semver.{func}"))?;
                let range_str = want_version_string(&arg(args, 1), span, &format!("semver.{func}"))?;
                let range = match parse_range(&range_str) {
                    Ok(r) => r,
                    Err(e) => {
                        return Ok(make_pair(
                            Value::nil(),
                            make_error(Value::str(format!("invalid range: {e}"))),
                        ))
                    }
                };
                // Malformed VERSION in the candidate list is Tier-2 (same posture
                // as compare — these are programmer-supplied).
                let mut best: Option<(String, Version)> = None;
                for s in &raw {
                    let v = parse_version(s).map_err(|e| {
                        Control::from(AsError::at(
                            format!("semver.{func}: invalid version '{s}': {e}"),
                            span,
                        ))
                    })?;
                    if !satisfies(&v, &range) {
                        continue;
                    }
                    best = Some(match best {
                        None => (s.clone(), v),
                        Some((bs, bv)) => {
                            let take = if want_max {
                                v.precedence(&bv) == Ordering::Greater
                            } else {
                                v.precedence(&bv) == Ordering::Less
                            };
                            if take {
                                (s.clone(), v)
                            } else {
                                (bs, bv)
                            }
                        }
                    });
                }
                let out = best.map(|(s, _)| Value::str(s)).unwrap_or_else(Value::nil);
                Ok(make_pair(out, Value::nil()))
            }
            _ => Err(AsError::at(format!("std/semver has no function '{func}'"), span).into()),
        }
    }
}

/// A version argument that MUST already be a string (a non-string is a Tier-2
/// type misuse).
fn want_version_string(v: &Value, span: Span, ctx: &str) -> Result<String, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(s.to_string()),
        _ => Err(AsError::at(
            format!("{ctx} expects a string, got {}", type_name(v)),
            span,
        )
        .into()),
    }
}

/// Parse a version argument STRICTLY: a non-string OR a malformed version is a
/// Tier-2 panic (`compare`/`satisfies`/`maxSatisfying` version posture).
fn parse_version_strict(v: &Value, span: Span, ctx: &str) -> Result<Version, Control> {
    let s = want_version_string(v, span, ctx)?;
    parse_version(&s)
        .map_err(|e| AsError::at(format!("{ctx}: invalid version '{s}': {e}"), span).into())
}

/// Read an array-of-strings argument slab-safely (arrays use `ArrayCell::borrow`,
/// which is sound for arrays — only Object/Instance use slab storage). A non-array
/// or a non-string element is a Tier-2 type misuse.
fn want_string_array(v: &Value, span: Span, ctx: &str) -> Result<Vec<String>, Control> {
    match v.kind() {
        ValueKind::Array(a) => {
            let mut out = Vec::new();
            for el in a.borrow().iter() {
                match el.kind() {
                    ValueKind::Str(s) => out.push(s.to_string()),
                    _ => {
                        return Err(AsError::at(
                            format!("{ctx} expects an array of strings, got element {}", type_name(el)),
                            span,
                        )
                        .into())
                    }
                }
            }
            Ok(out)
        }
        _ => Err(AsError::at(
            format!("{ctx} expects an array, got {}", type_name(v)),
            span,
        )
        .into()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::{Control, Interp};
    use crate::span::Span;
    use crate::value::Value;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    // ── (a) parse matrix ────────────────────────────────────────────────────

    #[test]
    fn parse_core() {
        let v = parse_version("1.2.3").unwrap();
        assert_eq!(v.core(), (1, 2, 3));
        assert!(v.pre.is_empty() && v.build.is_empty());
    }

    #[test]
    fn parse_prerelease_and_build() {
        let v = parse_version("1.0.0-alpha.1+build.7").unwrap();
        assert_eq!(v.core(), (1, 0, 0));
        assert_eq!(v.pre, vec![PreId::Alpha("alpha".into()), PreId::Num(1)]);
        assert_eq!(v.build, vec!["build".to_string(), "7".to_string()]);
    }

    #[test]
    fn parse_rejects_v_prefix() {
        assert!(parse_version("v1.2.3").is_err());
        assert!(parse_version("V1.2.3").is_err());
    }

    #[test]
    fn parse_rejects_leading_zeros() {
        assert!(parse_version("01.0.0").is_err());
        assert!(parse_version("1.02.0").is_err());
        assert!(parse_version("1.0.03").is_err());
        // numeric prerelease leading zero rejected
        assert!(parse_version("1.0.0-01").is_err());
        // but "0" alone is fine, and alphanumeric "0a" is fine
        assert!(parse_version("0.0.0").is_ok());
        assert!(parse_version("1.0.0-0a").is_ok());
        assert!(parse_version("1.0.0-alpha.0a").is_ok());
    }

    #[test]
    fn parse_numeric_vs_alpha_ids() {
        let v = parse_version("1.0.0-1.alpha").unwrap();
        assert_eq!(v.pre, vec![PreId::Num(1), PreId::Alpha("alpha".into())]);
    }

    #[test]
    fn parse_rejects_garbage() {
        for bad in ["", "1", "1.2", "1.2.3.4", "1.2.x", "a.b.c", "1.2.3-", "1.2.3+"] {
            assert!(parse_version(bad).is_err(), "expected '{bad}' to be rejected");
        }
    }

    // ── (b) the SemVer 2.0.0 §11 precedence ladder VERBATIM ─────────────────

    #[test]
    fn precedence_ladder() {
        let ladder = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        let parsed: Vec<Version> = ladder.iter().map(|s| parse_version(s).unwrap()).collect();
        for w in parsed.windows(2) {
            assert_eq!(
                w[0].precedence(&w[1]),
                Ordering::Less,
                "expected {:?} < {:?}",
                w[0].core(),
                w[1].core()
            );
            // strict antisymmetry
            assert_eq!(w[1].precedence(&w[0]), Ordering::Greater);
        }
    }

    #[test]
    fn build_metadata_ignored_in_precedence() {
        let a = parse_version("1.0.0+x").unwrap();
        let b = parse_version("1.0.0+y").unwrap();
        assert_eq!(a.precedence(&b), Ordering::Equal);
        let c = parse_version("1.0.0").unwrap();
        assert_eq!(a.precedence(&c), Ordering::Equal);
    }

    #[test]
    fn release_outranks_prerelease() {
        let pre = parse_version("1.0.0-rc.1").unwrap();
        let rel = parse_version("1.0.0").unwrap();
        assert_eq!(pre.precedence(&rel), Ordering::Less);
    }

    // ── (c) the satisfies table per spec §12 ────────────────────────────────

    fn s(version: &str, range: &str) -> bool {
        let v = parse_version(version).unwrap();
        let r = parse_range(range).unwrap();
        satisfies(&v, &r)
    }

    #[test]
    fn satisfies_caret() {
        // ^1.2.3 → >=1.2.3 <2.0.0
        assert!(s("1.2.3", "^1.2.3"));
        assert!(s("1.9.0", "^1.2.3"));
        assert!(!s("2.0.0", "^1.2.3"));
        assert!(!s("1.2.2", "^1.2.3"));
        // ^0.2.3 → >=0.2.3 <0.3.0
        assert!(s("0.2.3", "^0.2.3"));
        assert!(s("0.2.9", "^0.2.3"));
        assert!(!s("0.3.0", "^0.2.3"));
        // ^0.0.3 → >=0.0.3 <0.0.4
        assert!(s("0.0.3", "^0.0.3"));
        assert!(!s("0.0.4", "^0.0.3"));
        // ^1.x / ^1 → >=1.0.0 <2.0.0
        assert!(s("1.5.0", "^1.x"));
        assert!(s("1.5.0", "^1"));
        assert!(!s("2.0.0", "^1"));
        // ^0.0.x → >=0.0.0 <0.1.0
        assert!(s("0.0.9", "^0.0.x"));
        assert!(!s("0.1.0", "^0.0.x"));
    }

    #[test]
    fn satisfies_tilde() {
        // ~1.2.3 → >=1.2.3 <1.3.0
        assert!(s("1.2.3", "~1.2.3"));
        assert!(s("1.2.9", "~1.2.3"));
        assert!(!s("1.3.0", "~1.2.3"));
        // ~1.2 → >=1.2.0 <1.3.0
        assert!(s("1.2.0", "~1.2"));
        assert!(!s("1.3.0", "~1.2"));
        // ~1 → >=1.0.0 <2.0.0
        assert!(s("1.9.9", "~1"));
        assert!(!s("2.0.0", "~1"));
    }

    #[test]
    fn satisfies_xrange_partial() {
        assert!(s("1.2.99", "1.2.x"));
        assert!(!s("1.3.0", "1.2.x"));
        assert!(s("1.99.0", "1.x"));
        assert!(!s("2.0.0", "1.x"));
        assert!(s("1.2.3", "1.2")); // partial → >=1.2.0 <1.3.0
        assert!(s("1.9.9", "1")); // partial → >=1.0.0 <2.0.0
        assert!(s("9.9.9", "*"));
        assert!(s("0.0.0", "*"));
    }

    #[test]
    fn satisfies_hyphen() {
        assert!(s("1.5.0", "1.2.3 - 2.3.4"));
        assert!(s("1.2.3", "1.2.3 - 2.3.4"));
        assert!(s("2.3.4", "1.2.3 - 2.3.4"));
        assert!(!s("2.3.5", "1.2.3 - 2.3.4"));
        // partial high end: `1.2.3 - 2` → <3.0.0
        assert!(s("2.9.9", "1.2.3 - 2"));
        assert!(!s("3.0.0", "1.2.3 - 2"));
        // partial high end: `1.2.3 - 2.3` → <2.4.0
        assert!(s("2.3.9", "1.2.3 - 2.3"));
        assert!(!s("2.4.0", "1.2.3 - 2.3"));
    }

    #[test]
    fn satisfies_or_and_comparators() {
        // `||` OR
        assert!(s("1.0.0", "1.0.0 || 2.0.0"));
        assert!(s("2.0.0", "1.0.0 || 2.0.0"));
        assert!(!s("3.0.0", "1.0.0 || 2.0.0"));
        // space AND
        assert!(s("1.5.0", ">=1.2.3 <2.0.0"));
        assert!(!s("2.0.0", ">=1.2.3 <2.0.0"));
        // comparators
        assert!(s("1.2.4", ">1.2.3"));
        assert!(!s("1.2.3", ">1.2.3"));
        assert!(s("1.2.3", ">=1.2.3"));
        assert!(s("1.2.2", "<1.2.3"));
        assert!(s("1.2.3", "=1.2.3"));
        assert!(s("1.2.3", "1.2.3"));
    }

    #[test]
    fn satisfies_prerelease_participation_rule() {
        // A prerelease satisfies a comparator only if the comparator's tuple
        // has the SAME [major,minor,patch] AND a prerelease.
        assert!(s("1.2.3-alpha", ">=1.2.3-0"));
        assert!(!s("1.2.3-alpha", ">=1.2.0"));
        // Same-tuple prerelease range matches:
        assert!(s("1.2.3-beta", ">=1.2.3-alpha <1.2.4"));
        // A prerelease at a DIFFERENT tuple than the comparator's prerelease is excluded:
        assert!(!s("1.2.4-alpha", ">=1.2.3-0 <2.0.0"));
        // A non-prerelease version is unaffected:
        assert!(s("1.2.3", ">=1.2.0"));
    }

    // ── (d) maxSatisfying / minSatisfying ───────────────────────────────────

    #[tokio::test]
    async fn call_max_satisfying() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let versions = Value::array(vec![
                    Value::str("1.2.0"),
                    Value::str("1.2.5"),
                    Value::str("1.3.0"),
                    Value::str("2.0.0"),
                ]);
                let r = interp
                    .call_semver("maxSatisfying", &[versions, Value::str("^1.2.0")], sp())
                    .await
                    .unwrap();
                // [ "1.3.0", nil ]
                assert_eq!(r.to_string(), "[\"1.3.0\", nil]");
            })
            .await;
    }

    #[tokio::test]
    async fn call_min_satisfying() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let versions = Value::array(vec![
                    Value::str("1.2.0"),
                    Value::str("1.2.5"),
                    Value::str("1.3.0"),
                ]);
                let r = interp
                    .call_semver("minSatisfying", &[versions, Value::str("^1.2.0")], sp())
                    .await
                    .unwrap();
                assert_eq!(r.to_string(), "[\"1.2.0\", nil]");
            })
            .await;
    }

    #[tokio::test]
    async fn call_max_satisfying_none() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let versions = Value::array(vec![Value::str("3.0.0")]);
                let r = interp
                    .call_semver("maxSatisfying", &[versions, Value::str("^1.2.0")], sp())
                    .await
                    .unwrap();
                assert_eq!(r.to_string(), "[nil, nil]");
            })
            .await;
    }

    // ── (e) tiering ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn compare_malformed_version_is_tier2() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let r = interp
                    .call_semver("compare", &[Value::str("not-a-version"), Value::str("1.0.0")], sp())
                    .await;
                assert!(
                    matches!(r, Err(Control::Panic(_))),
                    "compare on a malformed version must be Tier-2, got {r:?}"
                );
            })
            .await;
    }

    #[tokio::test]
    async fn compare_returns_ordering() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let lt = interp
                    .call_semver("compare", &[Value::str("1.0.0"), Value::str("2.0.0")], sp())
                    .await
                    .unwrap();
                assert_eq!(lt.to_string(), "-1");
                let eq = interp
                    .call_semver("compare", &[Value::str("1.0.0+a"), Value::str("1.0.0+b")], sp())
                    .await
                    .unwrap();
                assert_eq!(eq.to_string(), "0");
                let gt = interp
                    .call_semver("compare", &[Value::str("2.0.0"), Value::str("1.0.0")], sp())
                    .await
                    .unwrap();
                assert_eq!(gt.to_string(), "1");
            })
            .await;
    }

    #[tokio::test]
    async fn satisfies_malformed_range_is_tier1() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let r = interp
                    .call_semver("satisfies", &[Value::str("1.0.0"), Value::str(">>>bogus")], sp())
                    .await
                    .expect("malformed range must be Tier-1, not a panic");
                let txt = r.to_string();
                assert!(
                    txt.starts_with("[nil, {message:"),
                    "expected Tier-1 [nil, err], got {txt}"
                );
            })
            .await;
    }

    #[tokio::test]
    async fn satisfies_ok_is_tier1_pair() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let r = interp
                    .call_semver("satisfies", &[Value::str("1.5.0"), Value::str("^1.2.3")], sp())
                    .await
                    .unwrap();
                assert_eq!(r.to_string(), "[true, nil]");
            })
            .await;
    }

    #[tokio::test]
    async fn call_parse_tier1_on_bad_version() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let r = interp
                    .call_semver("parse", &[Value::str("v1.2.3")], sp())
                    .await
                    .expect("parse must be Tier-1, not a panic");
                let txt = r.to_string();
                assert!(txt.starts_with("[nil, {message:"), "expected Tier-1 err, got {txt}");
            })
            .await;
    }

    #[tokio::test]
    async fn call_sort_ascending() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let versions = Value::array(vec![
                    Value::str("1.0.0"),
                    Value::str("1.0.0-alpha"),
                    Value::str("2.0.0"),
                    Value::str("1.0.0-beta"),
                ]);
                let r = interp.call_semver("sort", &[versions], sp()).await.unwrap();
                assert_eq!(r.to_string(), "[\"1.0.0-alpha\", \"1.0.0-beta\", \"1.0.0\", \"2.0.0\"]");
            })
            .await;
    }

    #[tokio::test]
    async fn call_sort_malformed_is_tier2() {
        let interp = Interp::new();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let versions = Value::array(vec![Value::str("1.0.0"), Value::str("oops")]);
                let r = interp.call_semver("sort", &[versions], sp()).await;
                assert!(matches!(r, Err(Control::Panic(_))));
            })
            .await;
    }
}
