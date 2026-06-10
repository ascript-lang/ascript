//! "Did you mean" suggestions — a tiny shared Levenshtein helper used by the
//! checker rules (unresolved name / unknown member / unknown `std/*` export) and
//! the CLI/LSP renderers (DX D4 §5.2).
//!
//! [`closest`] returns the nearest candidate to a typo'd `name` ONLY when it is
//! within a small edit-distance bound, so a genuinely-different name never gets a
//! nonsense suggestion. The bound is the LARGER of `2` and `⌈len(name)/3⌉`:
//! - very short names (`≤6` chars) allow up to 2 edits (`len`→`lne`, `fn`→`fb`);
//! - longer names scale (`somethingLongish` of length 16 → `⌈16/3⌉ = 6`),
//!
//! so a 1–2 character typo in a long identifier is still caught while keeping
//! short-name suggestions tight. The choice is documented + pinned by the
//! within/beyond boundary tests below.
//!
//! Pure over `&str` — no interpreter, no allocation beyond the DP row, `Send`-able.

/// The maximum edit distance at which a candidate is considered a plausible typo
/// of `name`. `max(2, ⌈len/3⌉)` — see the module doc.
fn distance_bound(name_len: usize) -> usize {
    2.max(name_len.div_ceil(3))
}

/// Levenshtein (edit) distance between `a` and `b` over Unicode scalar values, with
/// an `bound` early-out: once every cell of the current row exceeds `bound` the
/// true distance can only grow, so we stop and report a sentinel `bound + 1`
/// ("beyond the bound"). Two-row DP, O(len(a)·len(b)) time, O(len(b)) space.
fn bounded_levenshtein(a: &str, b: &str, bound: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // A length gap alone already exceeds the bound → no point computing.
    if a.len().abs_diff(b.len()) > bound {
        return bound + 1;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        let mut row_min = cur[0];
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j] + cost) // substitute / match
                .min(prev[j + 1] + 1) // delete from a
                .min(cur[j] + 1); // insert into a
            row_min = row_min.min(cur[j + 1]);
        }
        if row_min > bound {
            return bound + 1;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The candidate closest to `name` by edit distance, or `None` when the nearest
/// candidate is beyond the bound (`max(2, ⌈len/3⌉)`), the candidate set is empty,
/// or every candidate is too far. An EXACT match (distance 0) is never suggested
/// (a correctly-spelled name is not a typo). Ties break deterministically: the
/// SMALLEST distance wins; among equal distances the candidate seen FIRST in
/// iteration order wins (so the caller controls tie order).
pub fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let bound = distance_bound(name.chars().count());
    let mut best: Option<(&'a str, usize)> = None;
    for cand in candidates {
        // Never suggest the name itself (distance 0) — it is not a typo.
        if cand == name {
            continue;
        }
        let d = bounded_levenshtein(name, cand, bound);
        if d > bound {
            continue;
        }
        match best {
            // STRICT `<` keeps the FIRST candidate at a given distance (stable ties).
            Some((_, bd)) if d < bd => best = Some((cand, d)),
            None => best = Some((cand, d)),
            _ => {}
        }
    }
    best.map(|(c, _)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_distance_suggests() {
        assert_eq!(closest("lenght", ["length", "type", "print"]), Some("length"));
        assert_eq!(closest("lne", ["len", "type"]), Some("len"));
        // single transposition / single edit
        assert_eq!(closest("prnt", ["print", "len"]), Some("print"));
    }

    #[test]
    fn beyond_distance_returns_none() {
        // `xyz` vs `length` is far beyond the bound — no nonsense suggestion.
        assert_eq!(closest("xyz", ["length", "print"]), None);
        // A 3-edit typo on a short name (bound = 2) is beyond → None.
        // "len" -> "abcd": distance 4 (> bound 2).
        assert_eq!(closest("len", ["abcd"]), None);
    }

    #[test]
    fn boundary_is_pinned() {
        // name length 6 → bound = max(2, ceil(6/3)=2) = 2.
        // "lenght" -> "length": one transposition = 2 edits (gh<->hg), AT the bound.
        assert_eq!(closest("lenght", ["length"]), Some("length"));
        // name length 3 ("foo") → bound 2. A candidate exactly 2 away is IN.
        assert_eq!(closest("foo", ["fox"]), Some("fox")); // 1 edit
        assert_eq!(closest("foo", ["bar"]), None); // 3 edits, beyond
    }

    #[test]
    fn long_name_scales_bound() {
        // length 15 → bound = max(2, ceil(15/3)=5) = 5. A 3-edit typo is caught.
        assert_eq!(
            closest("configuratoin", ["configuration", "x"]),
            Some("configuration")
        );
    }

    #[test]
    fn empty_candidates_and_exact_match_are_none() {
        assert_eq!(closest("length", std::iter::empty()), None);
        // exact match is not a typo
        assert_eq!(closest("length", ["length"]), None);
    }

    #[test]
    fn ties_break_to_first_seen() {
        // "ab" is distance 1 from both "abc" and "abd"; the FIRST in order wins.
        assert_eq!(closest("ab", ["abc", "abd"]), Some("abc"));
        assert_eq!(closest("ab", ["abd", "abc"]), Some("abd"));
    }

    #[test]
    fn unicode_and_long_names_do_not_panic() {
        // Multibyte candidate / name — must not panic on byte vs char boundaries.
        // `café`→`cafe` (é→e) and `café`→`cafè` (é→è) are both distance 1; the
        // FIRST in order wins (stable ties), so `cafe`.
        assert_eq!(closest("café", ["cafe", "cafè"]), Some("cafe"));
        let long = "x".repeat(10_000);
        assert_eq!(closest(&long, ["y"]), None);
        // empty name
        assert_eq!(closest("", ["a", "bb"]), Some("a"));
    }
}
