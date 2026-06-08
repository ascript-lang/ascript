# P0 — `.aso` Reader Allocation Clamp (live bug fix) — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (or executing-plans). Steps use `- [ ]`.

**Goal:** Fix a live abort/OOM vector in the `.aso` deserializer: every `reserve(n)`/`with_capacity(n)`
in `src/vm/aso.rs` uses an attacker-controlled `u32` length with no clamp against the bytes actually
remaining, so a crafted `.aso` (or a truncated/corrupt one) forces a multi-GB allocation and SIGABRT
**before** `verify` runs. The worker serializer already shows the fix (`src/worker/serialize.rs:564`,
`len.min(r.remaining())`, with `remaining()` at `:306`). This is standalone, independent of the
campaign, and **gates BIN** (which runs this reader over bytes embedded in shipped binaries).

**Architecture:** add `Reader::remaining(&self) -> usize` to `aso.rs` and clamp every length-driven
allocation to it. A length larger than the remaining bytes is never a valid stream, so clamping turns
a pre-allocation bomb into a normal short-read that the existing per-element decode loop reports as a
clean `AsoError::Truncated`. No format change, no `ASO_FORMAT_VERSION` bump, no behavior change for
valid `.aso`.

**Tech stack:** Rust, the existing `aso.rs` `Reader` + `AsoError`.

## File structure
**Modified:** `src/vm/aso.rs` — add `Reader::remaining`; clamp `reserve`/`with_capacity` at the
length-driven sites (`read_chunk` ~571-610; `read_proto`/`read_value`/`read_type`/`read_class_proto`
~705/724/769/918/1201/1219/1240/1266/1296/1387+/1487/1585 — verify exact lines).
**Tests:** `src/vm/aso.rs` `#[cfg(test)]` (mirror `serialize.rs`'s `decode_huge_length_does_not_allocate`).

## Conventions
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean both configs.

## Task 1: `Reader::remaining` + failing test

**Files:** `src/vm/aso.rs`

- [ ] **Failing test** — a `.aso` whose header is valid but which declares a `u32::MAX` const-pool /
  proto / value length over a short buffer must return `Err(AsoError::…)`, NOT abort/OOM:
```rust
#[test]
fn reader_huge_length_does_not_allocate() {
    // a buffer that decodes a valid header then claims a gigantic count
    let mut w = Writer::new();
    w.u32(ASO_FORMAT_VERSION);            // valid header
    // ... minimal valid prefix up to a length field, then:
    w.u32(u32::MAX);                      // attacker-controlled count
    let bytes = w.finish();
    // must be a clean Err, and must not allocate u32::MAX elements
    assert!(matches!(Chunk::from_bytes(&bytes), Err(_)));
}
```
  (Model it on `serialize.rs` `decode_huge_length_does_not_allocate` @ ~:1018; pick the earliest
  length-driven site — the const pool — so the test is minimal.)
- [ ] **Add `remaining`:**
```rust
impl Reader<'_> {
    /// Bytes left unread — the hard ceiling on any length-driven pre-allocation.
    fn remaining(&self) -> usize { self.bytes.len().saturating_sub(self.pos) }
}
```
- [ ] Green.

## Task 2: Clamp every length-driven allocation

**Files:** `src/vm/aso.rs`

- [ ] For EACH `Vec::with_capacity(n)` / `IndexMap::with_capacity(n)` / `*.reserve(n)` where `n`
  derives from a `r.len()?`/`r.u32()?` stream value, change to `.with_capacity(n.min(r.remaining()))`
  (or `.reserve(n.min(r.remaining()))`). Grep to find them all; do not miss the recursive `read_*`.
  An element wider than 1 byte makes the clamp conservative (fine — it only caps the *pre-alloc*; the
  decode loop still reads exactly `n` and errors cleanly if the stream is short).
- [ ] Add a couple more targeted tests: a truncated buffer mid-vector (already covered by element
  decode → `Truncated`), and a huge count on a *nested* `read_proto`/`read_value`.
- [ ] `cargo test` + `--no-default-features` green; clippy clean both configs.
- [ ] **Independent review** (runs the tests, greps for any unclamped `with_capacity`/`reserve` left,
  confirms no `ASO_FORMAT_VERSION` change). Commit.

## Done when
Every length-driven allocation in `aso.rs` is clamped; the huge-length tests pass; no format/version
change; both feature configs green; an independent review confirms zero remaining unclamped sites.
This becomes a permanent regression guard and a seed in the FUZZ `.aso` corpus.
