//! `std/diff` — a hand-rolled Myers O(ND) line/char diff plus a unified-diff
//! renderer (BATT T2-4, spec §14). No dependency (`diff = []`).
//!
//! ## Tiering (load-bearing)
//!
//! - Wrong-TYPE arguments (a non-string to `lines`/`unified`/`chars`, a non-object
//!   `opts`) are **Tier-2** programmer misuse (a panic) — the diff fns assume the
//!   caller hands them text.
//! - An input that exceeds the size budget is a **Tier-1** `[nil, err]` (a clean
//!   "inputs too large" error, never an OOM/hang) — the inputs may be large external
//!   data and the caller should be able to recover.
//!
//! ## Myers O(ND) (hand-rolled, §3)
//!
//! The core is the classic Myers greedy O(ND) shortest-edit-script: line slices are
//! **interned to indices first** (so the inner comparison is integer-equality, not a
//! string compare), the D-path is traced forward over the `V` end-point array, and the
//! edit script is reconstructed by replaying the V-snapshot stack backward. The D-loop
//! is **budgeted** (`d` may not exceed `a.len() + b.len()`, itself capped by
//! [`MAX_DIFF_LINES`]); beyond the cap → Tier-1.
//!
//! `diff.chars` runs the SAME core over a `char` vector instead of a line vector
//! (intra-line, small-input).
//!
//! ## Line splitting + the trailing-newline flag (correctness pins)
//!
//! [`split_lines`] splits on `\n`, keeping a per-input **`final_newline`** flag so the
//! `\ No newline at end of file` marker is rendered correctly. **CRLF**: a `\r\n` line
//! ending is NOT special-cased — only `\n` terminates a line, so the `\r` stays as the
//! last char of the line content (a `\r\n` file and a `\n` file with otherwise-identical
//! text therefore differ, which matches GNU `diff` byte-for-byte without `--strip-trailing-cr`).
//! **Empty vs single newline:** an empty string `""` splits to ZERO lines (`final_newline`
//! is irrelevant); a single `"\n"` splits to ONE empty line WITH `final_newline=true`; a
//! single `"x"` (no newline) splits to ONE line `"x"` WITH `final_newline=false`. So an
//! empty file (`""`) is distinct from a one-blank-line file (`"\n"`).
//!
//! ## Unified renderer (§14)
//!
//! The edit script is grouped into HUNKS: a run of changes plus up to `context` (default
//! 3) equal lines of leading/trailing context, and two change runs whose context windows
//! OVERLAP are merged into one hunk (the standard `diff -u` behavior). Each hunk header is
//! `@@ -aStart,aCount +bStart,bCount @@` (1-based starts; a zero-length side prints its
//! start as the line-before, count 0). Output byte-matches GNU `diff -u --label A --label B`.

#![cfg(feature = "diff")]

use crate::error::AsError;
use crate::interp::{make_error, make_pair, type_name, Control};
use crate::span::Span;
use crate::value::{Value, ValueKind};

/// Total combined line/char budget for a single diff. A breach is a clean Tier-1
/// "inputs too large" error (never an OOM/hang). The Myers D-loop is O(ND) in the
/// worst case, so this also bounds the work.
pub(crate) const MAX_DIFF_LINES: usize = 200_000;

fn bi(qualified: &str) -> Value {
    Value::builtin(qualified)
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("lines", bi("diff.lines")),
        ("unified", bi("diff.unified")),
        ("chars", bi("diff.chars")),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// Line splitting + the trailing-newline flag.
// ─────────────────────────────────────────────────────────────────────────────

/// The result of splitting an input into lines (`\n`-terminated), plus whether the
/// input ended WITH a trailing newline. See the module doc for the empty-vs-newline
/// and CRLF pins.
struct Lined {
    lines: Vec<String>,
    final_newline: bool,
}

fn split_lines(s: &str) -> Lined {
    if s.is_empty() {
        return Lined {
            lines: Vec::new(),
            final_newline: true,
        };
    }
    let final_newline = s.ends_with('\n');
    let body = if final_newline { &s[..s.len() - 1] } else { s };
    // `split('\n')` on a body that has no trailing '\n' yields each line; an empty
    // body (the `"\n"` case, body == "") yields a single "" line — exactly one blank
    // line, which is correct.
    let lines: Vec<String> = body.split('\n').map(|l| l.to_string()).collect();
    Lined {
        lines,
        final_newline,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Myers O(ND) shortest edit script.
// ─────────────────────────────────────────────────────────────────────────────

/// A single edit-script operation over interned indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Equal,
    Delete,
    Insert,
}

/// Run Myers O(ND) over two index slices, returning the edit script as a flat list of
/// per-element ops (in input order). `Err` on budget breach.
///
/// The two inputs are pre-interned to `usize` ids so element comparison is integer
/// equality. The algorithm traces the greedy D-path forward, snapshotting the `V`
/// array at each `d`, then walks the snapshots backward to recover the script.
fn myers(a: &[usize], b: &[usize], budget: usize) -> Result<Vec<Edit>, ()> {
    let n = a.len();
    let m = b.len();
    if n + m > budget {
        return Err(());
    }
    // Trivial shortcuts (and they keep the V-array indexing simple).
    if n == 0 {
        return Ok(vec![Edit::Insert; m]);
    }
    if m == 0 {
        return Ok(vec![Edit::Delete; n]);
    }

    let max = n + m;
    let offset = max; // V is indexed by k in [-max, max] -> [0, 2*max].
    let vsize = 2 * max + 1;
    let mut v = vec![0isize; vsize];
    // Snapshot of V after each d, for the backward trace.
    let mut trace: Vec<Vec<isize>> = Vec::new();

    let mut found_d = None;
    'outer: for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            // Choose to go down (insert) or right (delete).
            let idx = (k + offset as isize) as usize;
            let mut x = if k == -d
                || (k != d && v[(k - 1 + offset as isize) as usize] < v[(k + 1 + offset as isize) as usize])
            {
                // down: take the point above (k+1)
                v[(k + 1 + offset as isize) as usize]
            } else {
                // right: take the point to the left (k-1), +1
                v[(k - 1 + offset as isize) as usize] + 1
            };
            let mut y = x - k;
            // Follow the snake (matching elements).
            while (x as usize) < n && (y as usize) < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x as usize >= n && y as usize >= m {
                found_d = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }

    let d_final = found_d.ok_or(())?;

    // Backward trace: reconstruct the script from the V snapshots.
    let mut script_rev: Vec<Edit> = Vec::new();
    let mut x = n as isize;
    let mut y = m as isize;
    for d in (0..=d_final).rev() {
        let vd = &trace[d as usize];
        let k = x - y;
        let down = k == -d
            || (k != d && vd[(k - 1 + offset as isize) as usize] < vd[(k + 1 + offset as isize) as usize]);
        let prev_k = if down { k + 1 } else { k - 1 };
        let prev_x = vd[(prev_k + offset as isize) as usize];
        let prev_y = prev_x - prev_k;

        // Emit the diagonal (equal) snake from (prev's move target) back to (x,y).
        while x > prev_x && y > prev_y {
            script_rev.push(Edit::Equal);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if down {
                script_rev.push(Edit::Insert);
            } else {
                script_rev.push(Edit::Delete);
            }
        }
        x = prev_x;
        y = prev_y;
    }
    script_rev.reverse();
    Ok(script_rev)
}

/// Intern a sequence of items (lines or chars) into ids over a shared dictionary, so
/// `myers` can compare with integer equality. Both inputs share the dictionary so an
/// equal item gets an equal id across the two.
fn intern<'a, T: std::hash::Hash + Eq + 'a>(
    a: impl Iterator<Item = &'a T>,
    b: impl Iterator<Item = &'a T>,
) -> (Vec<usize>, Vec<usize>) {
    use std::collections::HashMap;
    let mut dict: HashMap<&T, usize> = HashMap::new();
    let mut id_of = |t: &'a T| -> usize {
        let next = dict.len();
        *dict.entry(t).or_insert(next)
    };
    let ai: Vec<usize> = a.map(&mut id_of).collect();
    let bi: Vec<usize> = b.map(&mut id_of).collect();
    (ai, bi)
}

// ─────────────────────────────────────────────────────────────────────────────
// Hunk grouping (the `diff.lines` shape).
// ─────────────────────────────────────────────────────────────────────────────

/// A grouped hunk over the edit script: a maximal run that is uniformly equal,
/// deleted, or inserted, with the line slices it covers and the 0-based start/end
/// indices into the respective input.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Hunk {
    tag: Edit,
    a_start: usize,
    a_end: usize,
    b_start: usize,
    b_end: usize,
    lines: Vec<String>,
}

/// Group the flat edit script into maximal same-tag runs, tracking the a/b cursors and
/// collecting the covered line text.
fn group_hunks(script: &[Edit], a_lines: &[String], b_lines: &[String]) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    let mut ai = 0usize;
    let mut bi = 0usize;
    let mut i = 0usize;
    while i < script.len() {
        let tag = script[i];
        let a_start = ai;
        let b_start = bi;
        let mut lines: Vec<String> = Vec::new();
        while i < script.len() && script[i] == tag {
            match tag {
                Edit::Equal => {
                    lines.push(a_lines[ai].clone());
                    ai += 1;
                    bi += 1;
                }
                Edit::Delete => {
                    lines.push(a_lines[ai].clone());
                    ai += 1;
                }
                Edit::Insert => {
                    lines.push(b_lines[bi].clone());
                    bi += 1;
                }
            }
            i += 1;
        }
        out.push(Hunk {
            tag,
            a_start,
            a_end: ai,
            b_start,
            b_end: bi,
            lines,
        });
    }
    out
}

fn tag_str(t: Edit) -> &'static str {
    match t {
        Edit::Equal => "equal",
        Edit::Delete => "delete",
        Edit::Insert => "insert",
    }
}

/// Build the `diff.lines`/`diff.chars` return value: an array of hunk objects
/// `{tag, aStart, aEnd, bStart, bEnd, lines}`.
fn hunks_to_value(hunks: &[Hunk]) -> Value {
    let arr: Vec<Value> = hunks
        .iter()
        .map(|h| {
            let mut o = indexmap::IndexMap::new();
            o.insert("tag".to_string(), Value::str(tag_str(h.tag)));
            o.insert("aStart".to_string(), Value::int(h.a_start as i64));
            o.insert("aEnd".to_string(), Value::int(h.a_end as i64));
            o.insert("bStart".to_string(), Value::int(h.b_start as i64));
            o.insert("bEnd".to_string(), Value::int(h.b_end as i64));
            let lines: Vec<Value> = h.lines.iter().map(|l| Value::str(l.as_str())).collect();
            o.insert("lines".to_string(), Value::array(lines));
            Value::object(o)
        })
        .collect();
    Value::array(arr)
}

// ─────────────────────────────────────────────────────────────────────────────
// Unified renderer.
// ─────────────────────────────────────────────────────────────────────────────

/// Render a unified diff that byte-matches `diff -u --label from --label to`.
///
/// `context` is the number of equal context lines kept around each change run; change
/// runs whose context windows overlap are merged into a single hunk.
fn render_unified(
    script: &[Edit],
    a: &Lined,
    b: &Lined,
    context: usize,
    from_file: &str,
    to_file: &str,
) -> String {
    // No changes → empty string (matches `diff -u` producing no output).
    if script.iter().all(|e| *e == Edit::Equal) {
        return String::new();
    }

    // Walk the script into per-element records carrying the source line + a/b indices.
    // We need, for each script op, the line text and whether it's the LAST line of its
    // side (for the no-newline marker).
    struct Rec {
        op: Edit,
        text: String,
        // 0-based index into a (for Equal/Delete) — only meaningful when op != Insert
        a_idx: Option<usize>,
        // 0-based index into b (for Equal/Insert) — only meaningful when op != Delete
        b_idx: Option<usize>,
    }
    let mut recs: Vec<Rec> = Vec::with_capacity(script.len());
    let mut ai = 0usize;
    let mut bi = 0usize;
    for &op in script {
        match op {
            Edit::Equal => {
                recs.push(Rec {
                    op,
                    text: a.lines[ai].clone(),
                    a_idx: Some(ai),
                    b_idx: Some(bi),
                });
                ai += 1;
                bi += 1;
            }
            Edit::Delete => {
                recs.push(Rec {
                    op,
                    text: a.lines[ai].clone(),
                    a_idx: Some(ai),
                    b_idx: None,
                });
                ai += 1;
            }
            Edit::Insert => {
                recs.push(Rec {
                    op,
                    text: b.lines[bi].clone(),
                    a_idx: None,
                    b_idx: Some(bi),
                });
                bi += 1;
            }
        }
    }

    // Determine which records are part of a hunk: any change, plus `context` equals on
    // either side. Then split into maximal hunk-segments (a run of in-hunk records),
    // where two change groups separated by <= 2*context equals get merged (because each
    // keeps `context` equals and they touch/overlap).
    let n = recs.len();
    let mut in_hunk = vec![false; n];
    for (i, r) in recs.iter().enumerate() {
        if r.op != Edit::Equal {
            let lo = i.saturating_sub(context);
            let hi = (i + context + 1).min(n);
            for item in in_hunk.iter_mut().take(hi).skip(lo) {
                *item = true;
            }
        }
    }

    let mut out = String::new();
    out.push_str(&format!("--- {from_file}\n"));
    out.push_str(&format!("+++ {to_file}\n"));

    let mut i = 0usize;
    while i < n {
        if !in_hunk[i] {
            i += 1;
            continue;
        }
        // Hunk spans [i, j).
        let mut j = i;
        while j < n && in_hunk[j] {
            j += 1;
        }
        // Compute the a/b 1-based starts and counts over [i, j).
        let mut a_count = 0usize;
        let mut b_count = 0usize;
        let mut a_start_idx: Option<usize> = None;
        let mut b_start_idx: Option<usize> = None;
        for r in &recs[i..j] {
            if r.op != Edit::Insert {
                if a_start_idx.is_none() {
                    a_start_idx = r.a_idx;
                }
                a_count += 1;
            }
            if r.op != Edit::Delete {
                if b_start_idx.is_none() {
                    b_start_idx = r.b_idx;
                }
                b_count += 1;
            }
        }
        // A zero-length side prints its start as the index of the line BEFORE the hunk
        // (i.e. start = first-covered-index, but when count==0 GNU prints start-1+1
        // collapsing to the preceding line number; with count 0 the start is the line
        // number before insertion). Reconstruct 1-based starts.
        let a_start = match a_start_idx {
            Some(s) => s + 1,
            None => {
                // No a-lines in this hunk (pure insert at this position): the start is
                // the a-line index just before the hunk. Find it from the first record.
                recs[i].a_idx.map(|x| x + 1).unwrap_or(0)
            }
        };
        let b_start = match b_start_idx {
            Some(s) => s + 1,
            None => recs[i].b_idx.map(|x| x + 1).unwrap_or(0),
        };
        out.push_str(&hunk_header(a_start, a_count, b_start, b_count));

        for r in &recs[i..j] {
            let prefix = match r.op {
                Edit::Equal => ' ',
                Edit::Delete => '-',
                Edit::Insert => '+',
            };
            out.push(prefix);
            out.push_str(&r.text);
            out.push('\n');
            // No-newline marker: emit `\ No newline at end of file` after a rendered line
            // iff it is the LAST line of its source side AND that side lacks a final
            // newline. For an Equal record (shared text on both sides), the marker follows
            // when either side it represents is the unterminated last line. Because the
            // last-line newline state is folded into interning, an Equal last line implies
            // BOTH sides share the same newline state.
            let last_a = !a.lines.is_empty() && r.a_idx == Some(a.lines.len() - 1);
            let last_b = !b.lines.is_empty() && r.b_idx == Some(b.lines.len() - 1);
            let no_nl = match r.op {
                Edit::Delete => last_a && !a.final_newline,
                Edit::Insert => last_b && !b.final_newline,
                Edit::Equal => (last_a && !a.final_newline) || (last_b && !b.final_newline),
            };
            if no_nl {
                out.push_str("\\ No newline at end of file\n");
            }
        }

        i = j;
    }

    out
}

/// One unified hunk header. A side with count != 1 prints `start,count`; a side with
/// count == 1 prints just `start`; a side with count == 0 prints `start,0` where start
/// is the preceding line number (GNU convention).
fn hunk_header(a_start: usize, a_count: usize, b_start: usize, b_count: usize) -> String {
    let a_field = range_field(a_start, a_count);
    let b_field = range_field(b_start, b_count);
    format!("@@ -{a_field} +{b_field} @@\n")
}

fn range_field(start: usize, count: usize) -> String {
    if count == 1 {
        format!("{start}")
    } else if count == 0 {
        // GNU prints the line number BEFORE the empty region with count 0.
        format!("{},0", start.saturating_sub(1))
    } else {
        format!("{start},{count}")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The pure entry points (also used by the snapshot wiring).
// ─────────────────────────────────────────────────────────────────────────────

/// Line-level unified diff. `Err(())` on budget breach (Tier-1 at the call boundary).
pub(crate) fn unified_lines(
    a: &str,
    b: &str,
    context: usize,
    from_file: &str,
    to_file: &str,
) -> Result<String, ()> {
    let la = split_lines(a);
    let lb = split_lines(b);
    // Fold the trailing-newline state into the interned token of each side's LAST line:
    // a final line WITHOUT a newline gets a sentinel suffix so it cannot intern-equal an
    // otherwise-identical line that DOES end in a newline. This makes a pure
    // newline-at-EOF difference register as a change (GNU `diff -u` behavior — the
    // `\ No newline at end of file` line). The sentinel only affects equality, never the
    // rendered text (`render_unified` reads from `la.lines`/`lb.lines`, the unmarked text).
    let toks_a = newline_aware_tokens(&la);
    let toks_b = newline_aware_tokens(&lb);
    let (ai, bi) = intern(toks_a.iter(), toks_b.iter());
    let script = myers(&ai, &bi, MAX_DIFF_LINES)?;
    Ok(render_unified(&script, &la, &lb, context, from_file, to_file))
}

/// Build interning tokens that fold a missing trailing newline into the LAST line, so a
/// pure newline-at-EOF change is detected. Uses a `\0`-prefixed sentinel that real line
/// content can never collide with for the no-final-newline last line.
fn newline_aware_tokens(l: &Lined) -> Vec<String> {
    let mut toks: Vec<String> = l.lines.clone();
    if !l.final_newline {
        if let Some(last) = toks.last_mut() {
            last.insert_str(0, "\0\0NONL\0\0");
        }
    }
    toks
}

/// Line-level hunk list. `Err(())` on budget breach.
fn diff_lines(a: &str, b: &str) -> Result<Vec<Hunk>, ()> {
    let la = split_lines(a);
    let lb = split_lines(b);
    let (ai, bi) = intern(la.lines.iter(), lb.lines.iter());
    let script = myers(&ai, &bi, MAX_DIFF_LINES)?;
    Ok(group_hunks(&script, &la.lines, &lb.lines))
}

/// Char-level hunk list. `Err(())` on budget breach.
fn diff_chars(a: &str, b: &str) -> Result<Vec<Hunk>, ()> {
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let (ai, bi) = intern(ca.iter(), cb.iter());
    let script = myers(&ai, &bi, MAX_DIFF_LINES)?;
    // Reuse group_hunks via single-char "line" strings.
    let a_strs: Vec<String> = ca.iter().map(|c| c.to_string()).collect();
    let b_strs: Vec<String> = cb.iter().map(|c| c.to_string()).collect();
    Ok(group_hunks(&script, &a_strs, &b_strs))
}

// ─────────────────────────────────────────────────────────────────────────────
// Dispatch.
// ─────────────────────────────────────────────────────────────────────────────

/// A string argument that MUST already be a string (a non-string is Tier-2 misuse).
fn want_string(v: &Value, span: Span, ctx: &str) -> Result<String, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(s.to_string()),
        _ => Err(AsError::at(
            format!("{ctx} expects a string, got {}", type_name(v)),
            span,
        )
        .into()),
    }
}

/// The Tier-1 "inputs too large" error pair.
fn too_large_pair() -> Value {
    make_pair(
        Value::nil(),
        make_error(Value::str("diff: inputs too large")),
    )
}

impl crate::interp::Interp {
    pub(crate) fn call_diff(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        use crate::stdlib::arg;
        match func {
            // ── lines → array<hunk> ; budget breach Tier-1 ────────────────────
            "lines" => {
                let a = want_string(&arg(args, 0), span, "diff.lines")?;
                let b = want_string(&arg(args, 1), span, "diff.lines")?;
                match diff_lines(&a, &b) {
                    Ok(h) => Ok(hunks_to_value(&h)),
                    Err(()) => Ok(too_large_pair()),
                }
            }
            // ── chars → array<hunk> ; budget breach Tier-1 ────────────────────
            "chars" => {
                let a = want_string(&arg(args, 0), span, "diff.chars")?;
                let b = want_string(&arg(args, 1), span, "diff.chars")?;
                match diff_chars(&a, &b) {
                    Ok(h) => Ok(hunks_to_value(&h)),
                    Err(()) => Ok(too_large_pair()),
                }
            }
            // ── unified → string ; budget breach Tier-1 ───────────────────────
            "unified" => {
                let a = want_string(&arg(args, 0), span, "diff.unified")?;
                let b = want_string(&arg(args, 1), span, "diff.unified")?;
                let (context, from_file, to_file) = read_unified_opts(&arg(args, 2), span)?;
                match unified_lines(&a, &b, context, &from_file, &to_file) {
                    Ok(s) => Ok(Value::str(s)),
                    Err(()) => Ok(too_large_pair()),
                }
            }
            _ => Err(AsError::at(format!("std/diff has no function '{func}'"), span).into()),
        }
    }
}

/// Read the optional `{context?, fromFile?, toFile?}` opts object. Slab-safe: every
/// field is read through the [`crate::value::ObjectCell::get`] accessor (returns an
/// owned `Value`, Rc-bump), NEVER `borrow()` — a VM source-literal opts object is in
/// slab storage and `borrow()` would panic (SHAPE shim contract). Missing/`nil` opts →
/// `(3, "a", "b")`.
fn read_unified_opts(v: &Value, span: Span) -> Result<(usize, String, String), Control> {
    let mut context = 3usize;
    let mut from_file = "a".to_string();
    let mut to_file = "b".to_string();
    match v.kind() {
        ValueKind::Nil => {}
        ValueKind::Object(o) => {
            if let Some(c) = o.get("context") {
                if !matches!(c.kind(), ValueKind::Nil) {
                    let n = c.as_f64().ok_or_else(|| {
                        Control::from(AsError::at(
                            format!(
                                "diff.unified: opts.context must be a number, got {}",
                                type_name(&c)
                            ),
                            span,
                        ))
                    })?;
                    if n < 0.0 || !n.is_finite() {
                        return Err(AsError::at(
                            "diff.unified: opts.context must be a non-negative integer",
                            span,
                        )
                        .into());
                    }
                    context = n as usize;
                }
            }
            if let Some(f) = o.get("fromFile") {
                if let ValueKind::Str(s) = f.kind() {
                    from_file = s.to_string();
                }
            }
            if let Some(t) = o.get("toFile") {
                if let ValueKind::Str(s) = t.kind() {
                    to_file = s.to_string();
                }
            }
        }
        _ => {
            return Err(AsError::at(
                format!("diff.unified: opts must be an object, got {}", type_name(v)),
                span,
            )
            .into())
        }
    }
    Ok((context, from_file, to_file))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── (a) diff.lines hunk pins ─────────────────────────────────────────────

    #[test]
    fn lines_identical_one_equal_hunk() {
        let h = diff_lines("a\nb\nc\n", "a\nb\nc\n").unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].tag, Edit::Equal);
        assert_eq!(h[0].lines, vec!["a", "b", "c"]);
        assert_eq!((h[0].a_start, h[0].a_end), (0, 3));
        assert_eq!((h[0].b_start, h[0].b_end), (0, 3));
    }

    #[test]
    fn lines_pure_insert() {
        // a:[x,y]  b:[x,NEW,y]  -> equal[x], insert[NEW], equal[y]
        let h = diff_lines("x\ny\n", "x\nNEW\ny\n").unwrap();
        let tags: Vec<Edit> = h.iter().map(|x| x.tag).collect();
        assert_eq!(tags, vec![Edit::Equal, Edit::Insert, Edit::Equal]);
        let ins = h.iter().find(|x| x.tag == Edit::Insert).unwrap();
        assert_eq!(ins.lines, vec!["NEW"]);
        assert_eq!((ins.b_start, ins.b_end), (1, 2));
        // insert consumes no a-lines
        assert_eq!((ins.a_start, ins.a_end), (1, 1));
    }

    #[test]
    fn lines_pure_delete() {
        let h = diff_lines("x\nGONE\ny\n", "x\ny\n").unwrap();
        let tags: Vec<Edit> = h.iter().map(|x| x.tag).collect();
        assert_eq!(tags, vec![Edit::Equal, Edit::Delete, Edit::Equal]);
        let del = h.iter().find(|x| x.tag == Edit::Delete).unwrap();
        assert_eq!(del.lines, vec!["GONE"]);
        assert_eq!((del.a_start, del.a_end), (1, 2));
    }

    #[test]
    fn lines_interleaved() {
        // a change is a delete+insert pair.
        let h = diff_lines("a\nb\nc\n", "a\nB\nc\n").unwrap();
        let tags: Vec<Edit> = h.iter().map(|x| x.tag).collect();
        // Myers yields: equal[a], delete[b], insert[B], equal[c]
        assert_eq!(
            tags,
            vec![Edit::Equal, Edit::Delete, Edit::Insert, Edit::Equal]
        );
    }

    // ── (b) diff.unified BYTE-pins against GNU `diff -u --label a --label b` ──

    #[test]
    fn unified_simple_change() {
        // Generated by: diff -u --label a1.txt --label b1.txt
        let a = "line1\nline2\nline3\nline4\nline5\n";
        let b = "line1\nline2\nCHANGED\nline4\nline5\n";
        let expected = "\
--- a1.txt
+++ b1.txt
@@ -1,5 +1,5 @@
 line1
 line2
-line3
+CHANGED
 line4
 line5
";
        let got = unified_lines(a, b, 3, "a1.txt", "b1.txt").unwrap();
        assert_eq!(got, expected, "\n--- GOT ---\n{got}\n--- WANT ---\n{expected}");
    }

    #[test]
    fn unified_pure_insert() {
        let a = "apple\nbanana\ncherry\n";
        let b = "apple\nbanana\nblueberry\nboysenberry\ncherry\n";
        let expected = "\
--- a2.txt
+++ b2.txt
@@ -1,3 +1,5 @@
 apple
 banana
+blueberry
+boysenberry
 cherry
";
        let got = unified_lines(a, b, 3, "a2.txt", "b2.txt").unwrap();
        assert_eq!(got, expected, "\n--- GOT ---\n{got}\n--- WANT ---\n{expected}");
    }

    #[test]
    fn unified_no_trailing_newline_on_new() {
        let a = "one\ntwo\nthree\n";
        let b = "one\ntwo\nthree"; // no trailing newline
        let expected = "\
--- a3.txt
+++ b3.txt
@@ -1,3 +1,3 @@
 one
 two
-three
+three
\\ No newline at end of file
";
        let got = unified_lines(a, b, 3, "a3.txt", "b3.txt").unwrap();
        assert_eq!(got, expected, "\n--- GOT ---\n{got}\n--- WANT ---\n{expected}");
    }

    #[test]
    fn unified_two_hunks() {
        // Two changes far apart -> two separate @@ hunks (context windows don't overlap).
        let a = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\n";
        let b = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\nm\n";
        let expected = "\
--- a4.txt
+++ b4.txt
@@ -1,5 +1,5 @@
 a
-b
+B
 c
 d
 e
@@ -9,5 +9,5 @@
 i
 j
 k
-l
+L
 m
";
        let got = unified_lines(a, b, 3, "a4.txt", "b4.txt").unwrap();
        assert_eq!(got, expected, "\n--- GOT ---\n{got}\n--- WANT ---\n{expected}");
    }

    #[test]
    fn unified_merged_hunk() {
        // Two close changes -> one merged hunk (context windows overlap).
        let a = "a\nb\nc\nd\ne\nf\ng\n";
        let b = "a\nX\nc\nd\ne\nY\ng\n";
        let expected = "\
--- a5.txt
+++ b5.txt
@@ -1,7 +1,7 @@
 a
-b
+X
 c
 d
 e
-f
+Y
 g
";
        let got = unified_lines(a, b, 3, "a5.txt", "b5.txt").unwrap();
        assert_eq!(got, expected, "\n--- GOT ---\n{got}\n--- WANT ---\n{expected}");
    }

    #[test]
    fn unified_no_trailing_newline_on_old() {
        // diff -u --label a --label b b3.txt a3.txt  (old lacks newline)
        let a = "one\ntwo\nthree"; // no trailing newline
        let b = "one\ntwo\nthree\n";
        let expected = "\
--- a
+++ b
@@ -1,3 +1,3 @@
 one
 two
-three
\\ No newline at end of file
+three
";
        let got = unified_lines(a, b, 3, "a", "b").unwrap();
        assert_eq!(got, expected, "\n--- GOT ---\n{got}\n--- WANT ---\n{expected}");
    }

    #[test]
    fn unified_identical_is_empty() {
        let s = "a\nb\nc\n";
        assert_eq!(unified_lines(s, s, 3, "a", "b").unwrap(), "");
    }

    #[test]
    fn unified_empty_vs_content() {
        // empty file ("") vs a file with one line.
        let got = unified_lines("", "hello\n", 3, "a", "b").unwrap();
        let expected = "\
--- a
+++ b
@@ -0,0 +1 @@
+hello
";
        assert_eq!(got, expected, "\n--- GOT ---\n{got}\n--- WANT ---\n{expected}");
    }

    // ── empty-vs-newline distinction (correctness pin) ───────────────────────

    #[test]
    fn empty_string_vs_single_newline_distinct() {
        // "" is ZERO lines; "\n" is ONE blank line. They must diff as different.
        let got = unified_lines("", "\n", 3, "a", "b").unwrap();
        assert!(!got.is_empty(), "empty vs single newline must differ: {got:?}");
        // The reverse splits: "" has 0 lines, "\n" has 1 (blank) line -> one insert.
        let h = diff_lines("", "\n").unwrap();
        let inserts: usize = h.iter().filter(|x| x.tag == Edit::Insert).map(|x| x.lines.len()).sum();
        assert_eq!(inserts, 1, "single newline is one blank inserted line");
    }

    // ── CRLF split rule pin ──────────────────────────────────────────────────

    #[test]
    fn crlf_not_special_cased() {
        // "x\r\n" vs "x\n": only \n terminates, so the \r stays on the line content
        // -> the two lines "x\r" and "x" differ.
        let h = diff_lines("x\r\n", "x\n").unwrap();
        // One delete ("x\r") + one insert ("x"), no equal.
        assert!(h.iter().any(|x| x.tag == Edit::Delete && x.lines == vec!["x\r"]));
        assert!(h.iter().any(|x| x.tag == Edit::Insert && x.lines == vec!["x"]));
    }

    // ── (c) determinism pin ──────────────────────────────────────────────────

    #[test]
    fn determinism() {
        let a = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
        let b = "alpha\nBETA\ngamma\ndelta\nEPSILON\n";
        let r1 = unified_lines(a, b, 3, "a", "b").unwrap();
        let r2 = unified_lines(a, b, 3, "a", "b").unwrap();
        assert_eq!(r1, r2);
        let h1 = diff_lines(a, b).unwrap();
        let h2 = diff_lines(a, b).unwrap();
        assert_eq!(h1, h2);
    }

    // ── (d) budget breach → Err(()) (Tier-1 at the boundary) ─────────────────

    #[test]
    fn budget_breach_is_err() {
        // myers with a tiny budget over inputs exceeding it returns Err.
        let a: Vec<usize> = (0..100).collect();
        let b: Vec<usize> = (0..100).rev().collect();
        assert!(myers(&a, &b, 50).is_err());
        // And within budget it succeeds.
        assert!(myers(&a, &b, 1000).is_ok());
    }

    // ── (e) diff.chars ───────────────────────────────────────────────────────

    #[test]
    fn chars_basic() {
        // "cat" -> "cart": insert 'r' after 'a'.
        let h = diff_chars("cat", "cart").unwrap();
        let tags: Vec<Edit> = h.iter().map(|x| x.tag).collect();
        // equal[c,a], insert[r], equal[t]
        assert_eq!(tags, vec![Edit::Equal, Edit::Insert, Edit::Equal]);
        let ins = h.iter().find(|x| x.tag == Edit::Insert).unwrap();
        assert_eq!(ins.lines, vec!["r"]);
    }

    #[test]
    fn chars_unicode() {
        let h = diff_chars("héllo", "héllo!").unwrap();
        // one insert of "!" at the end, rest equal
        assert_eq!(h.last().unwrap().tag, Edit::Insert);
        assert_eq!(h.last().unwrap().lines, vec!["!"]);
    }

    // ── 10k-line performance sanity (must finish well under budget) ──────────

    #[test]
    fn large_input_sane() {
        let a: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        let mut b = a.clone();
        // change one line in the middle
        b = b.replace("line 2500\n", "CHANGED 2500\n");
        let got = unified_lines(&a, &b, 3, "a", "b").unwrap();
        assert!(got.contains("@@"));
        assert!(got.contains("+CHANGED 2500"));
    }
}
