//! DX D2 Task 10 — test-name filtering for `ascript test --filter PATTERN` (spec §6.6).
//!
//! A `--filter` value is either a plain **substring** match (the default) or, when written
//! `/regex/`, a **regular expression** match against each registered test's NAME. It prunes
//! WHICH registered tests run (a non-matching test is SKIPPED — not counted pass or fail,
//! reported as "N filtered") and lets the file-loading path skip a file whose registrations
//! all filter out. The same parsed filter is applied INSIDE each parallel isolate, so the
//! result is identical regardless of `--parallel` (the §7 determinism contract): the filter
//! is a pure function of the test name, never of completion order.
//!
//! A regex filter needs the `regex` crate (pulled in by the `data` or `sys` features). Under
//! `--no-default-features` (no regex) a `/regex/` filter is a CLEAN error, never a panic;
//! substring filtering always works.

/// A parsed, ready-to-apply test-name filter. Cheap to clone (`Substring` is an `Rc`-free
/// owned `String`; `Regex` wraps the compiled automaton).
#[derive(Debug, Clone)]
pub enum TestFilter {
    /// Default: a test matches if its name CONTAINS this substring.
    Substring(String),
    /// `/regex/`: a test matches if the compiled regex finds a match in its name.
    #[cfg(any(feature = "data", feature = "sys"))]
    Regex(regex::Regex),
}

impl TestFilter {
    /// Parse a raw `--filter` value. `/.../ ` (slash-delimited, length ≥ 2) is a regex;
    /// anything else is a literal substring. A malformed regex (or a regex filter with no
    /// regex support compiled in) returns a clean human-readable `Err` — NEVER a panic.
    pub fn parse(raw: &str) -> Result<TestFilter, String> {
        if raw.len() >= 2 && raw.starts_with('/') && raw.ends_with('/') {
            let body = &raw[1..raw.len() - 1];
            #[cfg(any(feature = "data", feature = "sys"))]
            {
                return regex::Regex::new(body)
                    .map(TestFilter::Regex)
                    .map_err(|e| format!("invalid --filter regex /{body}/: {e}"));
            }
            #[cfg(not(any(feature = "data", feature = "sys")))]
            {
                return Err(format!(
                    "regex filter /{body}/ requires the 'data' or 'sys' feature; \
                     use a plain substring filter instead"
                ));
            }
        }
        Ok(TestFilter::Substring(raw.to_string()))
    }

    /// Does `name` match this filter? Substring → `name.contains`; regex → `is_match`.
    pub fn matches(&self, name: &str) -> bool {
        match self {
            TestFilter::Substring(s) => name.contains(s.as_str()),
            #[cfg(any(feature = "data", feature = "sys"))]
            TestFilter::Regex(re) => re.is_match(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_is_the_default() {
        let f = TestFilter::parse("add").unwrap();
        assert!(f.matches("adds two"));
        assert!(f.matches("readd")); // substring, not word-boundary
        assert!(!f.matches("subtract"));
    }

    #[test]
    fn empty_substring_matches_everything() {
        // An empty substring is "no filtering" semantics — every name contains "".
        let f = TestFilter::parse("").unwrap();
        assert!(f.matches("anything"));
        assert!(f.matches(""));
    }

    #[cfg(any(feature = "data", feature = "sys"))]
    #[test]
    fn slash_delimited_is_a_regex() {
        let f = TestFilter::parse("/^add/").unwrap();
        assert!(f.matches("adds"));
        assert!(!f.matches("readd")); // anchored — must START with add
        let f2 = TestFilter::parse("/foo|bar/").unwrap();
        assert!(f2.matches("the bar test"));
        assert!(f2.matches("a foo case"));
        assert!(!f2.matches("baz"));
    }

    #[cfg(any(feature = "data", feature = "sys"))]
    #[test]
    fn bad_regex_is_a_clean_error_not_a_panic() {
        let err = TestFilter::parse("/(unclosed/").unwrap_err();
        assert!(err.contains("invalid --filter regex"), "got: {err}");
    }

    #[test]
    fn a_lone_slash_is_a_substring() {
        // "/" alone (len 1) is NOT a regex delimiter — it's a literal substring.
        let f = TestFilter::parse("/").unwrap();
        match f {
            TestFilter::Substring(s) => assert_eq!(s, "/"),
            #[cfg(any(feature = "data", feature = "sys"))]
            TestFilter::Regex(_) => panic!("a lone '/' must be a substring, not a regex"),
        }
    }
}
