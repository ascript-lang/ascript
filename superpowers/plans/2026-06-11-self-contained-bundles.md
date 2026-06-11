# Self-Contained Bundles — Module Archive, Tree-Shaking & Capability Embedding — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task is executed by a **fresh implementer subagent**, then verified by **two independent reviewer subagents** (code-quality + spec-&-plan-adherence) before acceptance. At the end of each phase, a **holistic per-phase review subagent** reviews the phase's combined changes before the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Make `ascript build` and `ascript build --native` produce self-contained artifacts that embed their whole reachable (tree-shaken) module graph and their build-time capability set, and fix every outstanding bug found in review along the way.

**Architecture:** A new version-tagged **module archive** container (`manifest + per-module verified chunks`) is produced by both `build` and `--native` for any multi-module program. A resolver-level **tree-shaker** embeds only reachable top-level declarations (conservative on dynamic access). At runtime an **in-memory module map** is consulted before disk. Capabilities serialize into the archive manifest and are read at startup. `std/*` stays native and is never archived.

**Tech Stack:** Rust (single binary `ascript`); the bytecode VM (`src/vm/`), the CST resolver (`src/syntax/resolve/`), the `.aso` codec (`src/vm/aso.rs`), the bundle footer (`src/bundle.rs`), the capability model (`src/stdlib/caps.rs`). Tests via `cargo test` (both feature configs) and the four-mode differential.

**Binding execution standards:** see spec §9 (`superpowers/specs/2026-06-11-self-contained-bundles-design.md`). Every task carries the §9.1 per-change deliverables (unit test, `.as` example where surface-visible, docs, blast-radius pass). Nothing is deferred; any bug found en route is fixed here with a regression test.

---

## File Structure

**New files:**
- `src/vm/archive.rs` — the `ModuleArchive` container: encode/decode, manifest (entry id, `CapSet`, shake digest), per-module verified-chunk table. Bounds-checked decode.
- `src/compile/shake.rs` — the tree-shaker: reachability worklist over the resolved module graph, returns the per-module keep-set + the build report.
- `examples/bundle_multimodule.as` + a sibling `examples/bundle_util.as` — a runnable multi-module program for the corpus.
- `examples/advanced/bundle_caps.as` — a production-shaped multi-module program built with `--deny`.
- `tests/archive.rs` — archive round-trip, multi-module four-mode differential, shaken-vs-unshaken differential, deserialization-bounds tests.
- `docs/content/language/bundles.md` — user docs for self-contained bundles + tree-shaking + embedded caps (added to the `NAV` array).
- `fuzz/fuzz_targets/archive_roundtrip.rs` — archive decode fuzzing.

**Modified files:**
- `src/vm/run.rs` — `load_file_module` consults the in-memory archive before disk; worker code-shipping ships the archive.
- `src/lib.rs` — `build_native`/`compile_verified_aso_bytes` emit archives; `run_embedded_aso`/`run_verified_aso` thread the embedded `CapSet`.
- `src/main.rs` — `try_run_embedded` reports post-confirmation I/O errors; `ASCRIPT_DENY` monotone subtract.
- `src/bundle.rs` — `validate_footer` consumers updated for archive payloads (codec itself unchanged).
- `src/stdlib/caps.rs` — `CapSet` serialize/deserialize helpers.
- Plus the Phase 0 bug-fix sites enumerated below.

---

## Phase 0 — Bug fixes (independent; ships first)

> Each task: write the failing regression test → run it (fails) → apply the fix → run (passes) → §9.1 deliverables → commit. `.as` examples are required only where the bug is observable from script; pure-internal fixes use a Rust unit test. Run the four-mode differential (`cargo test --test vm_differential`) on any fix touching an engine path.

### Task 0.1: i64/float boundary in equality, key-folding, and exact conversion

**Files:**
- Modify: `src/value.rs:218`, `src/value.rs:1255`, `src/value.rs:1330`
- Test: inline `#[test]` in `src/value.rs`

- [x] **Step 1: Write the failing test**

```rust
#[test]
fn float_two_pow_63_is_not_i64_max() {
    // i64::MAX as f64 rounds UP to 2^63; the old `<= i64::MAX as f64` admitted it.
    let two63 = 9223372036854775808.0_f64; // 2^63, NOT representable as i64
    assert_eq!(int_eq_float(i64::MAX, two63), false);
    assert_eq!(Value::Float(two63).as_int_exact(), None);
    assert_ne!(
        MapKey::from_value(&Value::Float(two63)),
        MapKey::from_value(&Value::Int(i64::MAX))
    );
}
```

- [x] **Step 2: Run it — expect FAIL** — `cargo test float_two_pow_63_is_not_i64_max`
- [x] **Step 3: Apply the fix** — at all three sites replace the upper bound `… <= i64::MAX as f64` with the strict exclusive bound already used at `value.rs:1359`:

```rust
// before: && *f <= i64::MAX as f64      (and the *n / f variants at 218 / 1330)
&& *f < -(i64::MIN as f64)   // 2^63 exactly; no i64 is >= 2^63
```

- [x] **Step 4: Run it — expect PASS**; then `cargo test --test vm_differential` (both configs) — expect PASS.
- [x] **Step 5: §9.1** — add `examples/num_int_float_edges.as` exercising `9223372036854775808.0 == 9223372036854775807` (→ `false`) and a map keyed by both; docs: note in `docs/content/language/values-types.md` numeric-equality section; blast-radius: grep every `i64::MAX as f64` / `i64::MIN as f64` use and confirm none remain lossy.
- [x] **Step 6: Commit** — `git commit -m "fix(num): strict i64/float upper bound in eq, MapKey, as_int_exact"`

### Task 0.2: negative integer enum backing value (VM compile path)

**Files:**
- Modify: `src/compile/mod.rs:2447`
- Test: `tests/cli.rs` (or inline compile test) + `examples`

- [x] **Step 1: Write the failing test** — a program `enum E { A = -1, B = 2 } print(E.A.value)` run on the VM expects `-1`.
- [x] **Step 2: Run it — expect FAIL** (`enum variant backing value must be a number or string literal`).
- [x] **Step 3: Apply the fix** — add the `Int` arm to the unary-minus match:

```rust
match self.const_eval_enum_backing(&operand)? {
    Value::Int(n) => Ok(Value::Int(-n)),     // NEW (NUM split: int literals are Value::Int)
    Value::Float(n) => Ok(Value::Float(-n)),
    _ => Err(CompileError::new(
        "enum variant backing value must be a number or string literal",
        node_span(un),
    )),
}
```

- [x] **Step 4: Run it — expect PASS**; `cargo test --test vm_differential` (tree-walker already accepts it) — expect byte-identical.
- [x] **Step 5: §9.1** — `examples/enums_negative_backing.as`; docs: `classes-enums.md` enum-backing note; blast-radius: scan `const_eval_*` / literal-fold helpers for other `Value::Float`-only arms missing `Value::Int` (NUM-split stragglers) and fix any found (log them as discovered bugs per §9.4).
- [x] **Step 6: Commit** — `git commit -m "fix(compile): negative integer enum backing after NUM split"`

### Task 0.3: `.aso` range-`step` round-trip loss in field defaults

**Files:**
- Modify: `src/vm/aso.rs` (`EX_RANGE` write ~1365 + read ~1604)
- Test: `tests/archive.rs` (new) or `tests/cli.rs` round-trip

- [ ] **Step 1: Write the failing test** — compile `class C { xs: array<number> = 0..10 step 2 }` to `.aso`, load, `C.from({})`, assert `xs == [0,2,4,6,8]` (len 5). Currently loads as `0..10` (len 11).
- [ ] **Step 2: Run it — expect FAIL** (len 11).
- [ ] **Step 3: Apply the fix** — serialize the optional step. In the `EX_RANGE` write arm, after writing `start`/`end`, emit a presence byte then the step expr; mirror in the reader:

```rust
// write (replace the `step: _` wildcard):
ExprKind::Range { start, end, inclusive, step } => {
    w.u8(EX_RANGE);
    w.u8(u8::from(*inclusive));
    write_expr(w, start)?;
    write_expr(w, end)?;
    match step {
        Some(s) => { w.u8(1); write_expr(w, s)?; }
        None => { w.u8(0); }
    }
}
// read (EX_RANGE arm): read inclusive, start, end, then:
let step = if r.u8()? == 1 { Some(Box::new(read_expr(r)?)) } else { None };
ExprKind::Range { start, end, inclusive, step }
```

Remove the now-false comment claiming "step rejected upstream"; bump `ASO_FORMAT_VERSION` and update `verify.rs` if the expr-tag stream is length-validated.

- [ ] **Step 4: Run it — expect PASS**; `cargo test --test vm_differential`.
- [ ] **Step 5: §9.1** — `examples/range_step_default.as`; docs: ranges section already documents `step` — add the field-default note; blast-radius: confirm `cst_default_expr` (compile/mod.rs:492) and the value-position range writer agree; the `ASO_FORMAT_VERSION` bump ripples to any golden `.aso`.
- [ ] **Step 6: Commit** — `git commit -m "fix(aso): preserve range step in field-default round-trip; bump ASO version"`

### Task 0.4: or-pattern bindings dropped by the resolver

**Files:**
- Modify: `src/syntax/resolve/mod.rs` (`resolve_pattern`, ~1082) and `declare_pattern_names` (~748)
- Test: `tests/cli.rs` + example

- [ ] **Step 1: Write the failing test** — `match Shape.Circle(2) { Shape.Circle(r) | Shape.Square(r) => print(r) }` expects `2`; currently `undefined variable: r`.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Apply the fix** — add an `OrPat` arm to both `resolve_pattern` and `declare_pattern_names` that recurses into each alternative sub-pattern:

```rust
OrPat => {
    for sub in pat.children().filter(|c| is_pattern(c.kind())) {
        self.resolve_pattern(sub);   // (declare_pattern_names in that fn)
    }
}
```

- [ ] **Step 4: Run it — expect PASS** in both engines; `cargo test --test vm_differential`.
- [ ] **Step 5: §9.1** — `examples/match_or_patterns.as`; docs: match section note that alternatives must bind the same names; LSP: confirm go-to-def/rename on an or-pattern binding resolves; blast-radius: the legacy `parser.rs`/tree-walker path — verify it already binds (oracle parity) and add a frontend-conformance snippet.
- [ ] **Step 6: Commit** — `git commit -m "fix(resolve): bind names inside or-patterns"`

### Task 0.5: legacy formatter drops parameter defaults

**Files:**
- Modify: `src/fmt.rs` (`write_params`, ~44)
- Test: inline `#[test]` in `src/fmt.rs`

- [ ] **Step 1: Write the failing test** — formatting `fn f(x = 5) {}` yields `fn f(x = 5) {}` (idempotent), not `fn f(x) {}`.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Apply the fix** — render the default after the type annotation:

```rust
if let Some(ty) = &p.ty {
    out.push_str(": ");
    out.push_str(&render_type(ty));
}
if let Some(def) = &p.default {            // NEW
    out.push_str(" = ");
    write_expr(out, def, 0);
}
```

- [ ] **Step 4: Run it — expect PASS**; run an idempotence check over `examples/**`.
- [ ] **Step 5: §9.1** — covered by examples already containing default params; docs: none; blast-radius: confirm the CST formatter (`syntax/format/`) already renders defaults (it does) so both formatters agree; add a fmt round-trip assertion.
- [ ] **Step 6: Commit** — `git commit -m "fix(fmt): render parameter default values"`

### Task 0.6: `SetGlobal` verifier stack-depth precondition

**Files:**
- Modify: `src/vm/verify.rs:345`
- Test: inline `#[test]` in `src/vm/verify.rs`

- [ ] **Step 1: Write the failing test** — a hand-built chunk with `SetGlobal` where the abstract stack depth is 0 must be REJECTED by `verify`, not accepted.
- [ ] **Step 2: Run it — expect FAIL** (currently accepted; would `expect`-panic at `fiber.peek(0)` at run time).
- [ ] **Step 3: Apply the fix** — `SetGlobal => Effect::new(1, 1)` (consume 1 for the min-depth check, push it back — net zero, matching the "leaves TOS" semantics and aligning with `SetLocal`).
- [ ] **Step 4: Run it — expect PASS**; full `cargo test` to confirm no valid chunk regressed.
- [ ] **Step 5: §9.1** — Rust unit test only (internal); blast-radius: re-audit `stack_effect` for any other op whose `pops` is 0 but whose run.rs arm `peek`s/`pop`s.
- [ ] **Step 6: Commit** — `git commit -m "fix(verify): SetGlobal requires stack depth >= 1"`

### Task 0.7: verifier bounds for `VariantElem` / `MatchVariantArity`

**Files:**
- Modify: `src/vm/verify.rs:509`
- Test: inline `#[test]`

- [ ] **Step 1: Write the failing test** — a chunk with `VariantElem(0xFFFF)` on a 2-field variant is REJECTED by `verify`.
- [ ] **Step 2: Run it — expect FAIL** (currently pass-through).
- [ ] **Step 3: Apply the fix** — cap the operands at a conservative practical maximum (the payload-field ceiling, 255) so an out-of-range index is a clean `VerifyError`, not a runtime panic:

```rust
VariantElem(n) | MatchVariantArity(n) => {
    if *n > 255 { return Err(VerifyError::operand("variant operand out of range")); }
}
```

- [ ] **Step 4: Run it — expect PASS**; `cargo test`.
- [ ] **Step 5: §9.1** — Rust unit test; blast-radius: confirm no legitimate program emits >255 payload fields (the parser already bounds named-payload arity).
- [ ] **Step 6: Commit** — `git commit -m "fix(verify): bound VariantElem/MatchVariantArity operands"`

### Task 0.8: HTTP response header CRLF injection

**Files:**
- Modify: `src/stdlib/http_server.rs` (`serialize_response` ~859, `value_to_response` ~778)
- Test: inline `#[test]` + `examples/advanced`

- [ ] **Step 1: Write the failing test** — a handler returning a header value `"a\r\nX-Injected: 1"` must NOT produce two headers; the value is rejected (Tier-2 panic) or the CRLF stripped.
- [ ] **Step 2: Run it — expect FAIL** (currently splits).
- [ ] **Step 3: Apply the fix** — validate header name + value when building the response; reject names with non-token chars and values containing `\r`/`\n` (recoverable Tier-2 panic with a field-path message), in `value_to_response` before they reach `serialize_response`:

```rust
fn sanitize_header(name: &str, val: &str) -> Result<(), Control> {
    if name.bytes().any(|b| b == b':' || b == b'\r' || b == b'\n' || b == b' ')
        || val.bytes().any(|b| b == b'\r' || b == b'\n') {
        return Err(AsError::at(
            format!("invalid header '{name}': names and values may not contain CR/LF"), span).into());
    }
    Ok(())
}
```

- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — `examples/advanced/http_header_safety.as`; docs: `stdlib/*` http server page note; blast-radius: check every site that writes user values into the response head (status reason, trailers).
- [ ] **Step 6: Commit** — `git commit -m "fix(http): reject CRLF in response header names/values"`

### Task 0.9: git argument injection in the package fetcher

**Files:**
- Modify: `src/pkg/fetch.rs:233` (clone) and `:246/:254` (rev-parse)
- Test: `tests/pkg.rs`

- [ ] **Step 1: Write the failing test** — a dependency `url = "--upload-pack=touch /tmp/x"` (or a refspec starting with `-`) must not be treated as a git flag; assert the args contain a `--` separator before untrusted input.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Apply the fix** — insert `--` before untrusted positional args:

```rust
run_git(None, &["clone", "--bare", "--quiet", "--", url, &bare.to_string_lossy()])?;
// rev-parse:
git_output(&["--git-dir", &bare.to_string_lossy(), "rev-parse", "--", &format!("{refspec}^{{commit}}")])
```

Additionally validate `url` begins with a known scheme (`https://`/`git@`/`ssh://`/`file://`) before use.

- [ ] **Step 4: Run it — expect PASS**; `cargo test --test pkg`.
- [ ] **Step 5: §9.1** — pkg test only (hermetic); docs: pkg page security note; blast-radius: audit every `run_git`/`git_output` call for the same omission.
- [ ] **Step 6: Commit** — `git commit -m "fix(pkg): add -- separator + scheme check to git invocations"`

### Task 0.10: non-finite count guards (`string.repeat`, `reader.read`)

**Files:**
- Modify: `src/stdlib/string.rs:153`, `src/stdlib/process.rs:664`, `src/stdlib/net_http.rs:1553`
- Test: inline `#[test]`s

- [ ] **Step 1: Write the failing test** — `string.repeat("x", 1/0)` and `string.repeat("x", 1e18)` return a recoverable Tier-2 panic, not a process abort.
- [ ] **Step 2: Run it — expect FAIL** (allocator abort).
- [ ] **Step 3: Apply the fix** — a shared guard mirroring `bytes.rs`'s `want_index`: reject `!n.is_finite()` and `n` above a sane cap before `as usize`:

```rust
if !n.is_finite() || n < 0.0 || n > (u32::MAX as f64) {
    return Err(AsError::at("string.repeat count must be a finite, in-range non-negative number", span).into());
}
```

Apply the same finite+cap guard at the `reader.read` sites.

- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — `examples/advanced/string_repeat_guard.as`; docs: string/process pages; blast-radius: grep `as usize`/`as u64` on `want_number` results across stdlib and guard any unguarded site (log discoveries per §9.4).
- [ ] **Step 6: Commit** — `git commit -m "fix(stdlib): finite/in-range guards on repeat and read counts"`

### Task 0.11: workflow log atomic write

**Files:**
- Modify: `src/stdlib/workflow.rs:730`
- Test: `tests/` (workflow durability)

- [ ] **Step 1: Write the failing test** — simulate a crash between truncate and write by asserting `write_log` never leaves a zero-byte/partial file visible at `path` (write goes to a temp sibling then renames).
- [ ] **Step 2: Run it — expect FAIL** (current `File::create` truncates in place).
- [ ] **Step 3: Apply the fix** — write-to-temp + fsync + atomic rename:

```rust
fn write_log(path: &str, contents: &str, fsync: bool, span: Span) -> Result<(), Control> {
    use std::io::Write;
    let tmp = format!("{path}.tmp");
    let mut f = std::fs::File::create(&tmp)
        .map_err(|e| AsError::at(format!("workflow: cannot write log '{}': {}", tmp, e), span))?;
    f.write_all(contents.as_bytes())
        .map_err(|e| AsError::at(format!("workflow: log write failed: {}", e), span))?;
    if fsync { let _ = f.sync_all(); }
    drop(f);
    std::fs::rename(&tmp, path)
        .map_err(|e| AsError::at(format!("workflow: log commit failed: {}", e), span))?;
    Ok(())
}
```

- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — Rust test; docs: workflow durability note; blast-radius: the workflow append model rewrites the whole log each event — confirm rename semantics hold for the replay reader; no concurrent same-path runs (document the single-writer assumption).
- [ ] **Step 6: Commit** — `git commit -m "fix(workflow): atomic log write via temp+rename"`

### Task 0.12: `clock_monotonic_ms` replay-mismatch handling

**Files:**
- Modify: `src/det.rs:308`
- Test: inline `#[test]` in `src/det.rs`

- [ ] **Step 1: Write the failing test** — in Replay, a `clock_monotonic_ms` call facing a non-`MonotonicRead` event calls `replay_mismatch_recover` (surfaces divergence), matching `clock_now_ms`.
- [ ] **Step 2: Run it — expect FAIL** (currently `ClockRead => value` silently, `_ => live clock`).
- [ ] **Step 3: Apply the fix** — align with `clock_now_ms`:

```rust
Mode::Replay => match self.next_event() {
    Some(DetEvent::MonotonicRead { value }) => { self.clock.monotonic_ms = value; value }
    Some(other) => self.replay_mismatch_recover(other),
    None => {
        self.mode = Mode::Record;
        let v = self.clock.monotonic_ms();
        self.events.push(DetEvent::MonotonicRead { value: v });
        v
    }
},
```

- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — Rust test; blast-radius: audit every other Replay reader for the same silent cross-consumption pattern.
- [ ] **Step 6: Commit** — `git commit -m "fix(det): clock_monotonic_ms surfaces replay mismatch"`

### Task 0.13: `crypto.hashPassword` seeded salt under replay

**Files:**
- Modify: `src/stdlib/crypto.rs` (`hashPassword` ~118)
- Test: inline `#[test]`

- [ ] **Step 1: Write the failing test** — under deterministic mode two `hashPassword` calls with the same input + seed produce the same hash (reproducible salt).
- [ ] **Step 2: Run it — expect FAIL** (`OsRng` salt differs each run).
- [ ] **Step 3: Apply the fix** — draw the salt bytes through `interp.fill_seeded_bytes` when in deterministic mode (mirroring `randomBytes`), else `OsRng`:

```rust
let mut salt_bytes = [0u8; 16];
if !interp.fill_seeded_bytes(&mut salt_bytes) {
    OsRng.fill_bytes(&mut salt_bytes);
}
let salt = SaltString::encode_b64(&salt_bytes).map_err(...)?;
```

- [ ] **Step 4: Run it — expect PASS**; confirm non-deterministic mode is byte-identical to before.
- [ ] **Step 5: §9.1** — Rust test; docs: note `hashPassword` is replay-safe; `ffi`/workflow lint: confirm `crypto` in a workflow body is covered by determinism guidance; blast-radius: audit other `OsRng`/`thread_rng` uses in crypto for replay-safety.
- [ ] **Step 6: Commit** — `git commit -m "fix(crypto): seeded salt for hashPassword under deterministic mode"`

### Task 0.14: `synth_array` double-synthesis (duplicate diagnostics)

**Files:**
- Modify: `src/check/infer/pass.rs:1489` (`synth_array`)
- Test: `tests/check.rs`

- [ ] **Step 1: Write the failing test** — `let x: int? = nil; let a = [x + 1]` emits the `possibly-nil` diagnostic exactly once.
- [ ] **Step 2: Run it — expect FAIL** (emitted twice).
- [ ] **Step 3: Apply the fix** — remove the first (discarded) `for e in &elems { self.synth(e, env); }` pass; keep the single pass that both folds element types and emits diagnostics.
- [ ] **Step 4: Run it — expect PASS**; run the `corpus::` gate (examples emit 0 type diagnostics).
- [ ] **Step 5: §9.1** — check-test; blast-radius: scan other `synth_*` for accidental double-synthesis.
- [ ] **Step 6: Commit** — `git commit -m "fix(check): remove duplicate synthesis in synth_array"`

### Task 0.15: LSP `did_rename_files` stale index

**Files:**
- Modify: `src/lsp/server.rs:1568` (and the `:1587` rename-path sibling if applicable)
- Test: `tests/lsp.rs`

- [ ] **Step 1: Write the failing test** — after a `workspace/didRenameFiles`, a go-to-def / workspace-symbol for a symbol from the old path returns NO stale entry pointing at the old path.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Apply the fix** — remove the old path via the full unindex, not just the `files` map:

```rust
idx.remove_file_from_maps(&workspace::canon(&old));
idx.files.remove(&workspace::canon(&old));
idx.reindex_file(&new, &text);
```

- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — lsp test; blast-radius: audit `didDeleteFiles` and any other path that removes from `files` directly without `remove_file_from_maps`.
- [ ] **Step 6: Commit** — `git commit -m "fix(lsp): fully unindex renamed files (defs + import edges)"`

### Task 0.16: CST `return;` spurious error node

**Files:**
- Modify: `src/syntax/parser.rs` (`return_stmt`)
- Test: `tests/frontend_conformance.rs`

- [ ] **Step 1: Write the failing test** — parsing `fn f() { return; }` produces a clean `ReturnStmt` with no `Error` child.
- [ ] **Step 2: Run it — expect FAIL** (error node from `expr(p)` on `;`).
- [ ] **Step 3: Apply the fix** — guard the optional expression on `;`:

```rust
if !p.at(RBrace) && !p.at_end() && !p.at(Semicolon) {
    expr(p);
}
```

- [ ] **Step 4: Run it — expect PASS**; tree-sitter conformance unaffected.
- [ ] **Step 5: §9.1** — frontend-conformance snippet; blast-radius: check other statement parsers (e.g. `break;`/`continue;`) for the same missing `Semicolon` guard.
- [ ] **Step 6: Commit** — `git commit -m "fix(cst): no spurious error node for bare return;"`

### Task 0.17: DAP unbounded Content-Length

**Files:**
- Modify: `src/dap/proto.rs` (~49)
- Test: inline `#[test]`

- [ ] **Step 1: Write the failing test** — a frame `Content-Length: 999999999` returns `Ok(None)` (or a clean error), not a multi-hundred-MB allocation/hang.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Apply the fix** — cap before allocating:

```rust
const MAX_DAP_MESSAGE: usize = 64 * 1024 * 1024;
let len = match content_length {
    Some(len) if len <= MAX_DAP_MESSAGE => len,
    Some(_) => return Ok(None),   // oversize → treat as malformed
    None => return Ok(None),
};
```

- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — Rust test; blast-radius: confirm `write_message`'s `expect("DAP message serializes")` cannot be reached from untrusted input (it serializes our own values — OK).
- [ ] **Step 6: Commit** — `git commit -m "fix(dap): cap Content-Length before allocation"`

### Task 0.18: DAP `scopes` frame_id overflow + double-launch

**Files:**
- Modify: `src/dap/server.rs` (~470 scopes; the `launch` arm ~322)
- Test: inline `#[test]`s

- [ ] **Step 1: Write the failing test** — a `scopes` request with `frameId: i64::MAX` does not panic; a second `launch` resets session state (no stale frames served).
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Apply the fix** — `let var_ref = frame_id.saturating_add(1);`; in the `launch` arm, if a session is already live, send `Continue` to the old VM, join/detach the old pump+debuggee handles, and reset the session-scoped `AdapterState` fields before starting the new session.
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — Rust tests; blast-radius: audit all `as_i64().unwrap_or(...)` + arithmetic in DAP handlers for overflow; confirm no other handler mutates shared state without a session guard.
- [ ] **Step 6: Commit** — `git commit -m "fix(dap): saturating frame_id; reset state on re-launch"`

### Task 0.19: BIN startup payload-read error reporting + double-bundle + TOCTOU

**Files:**
- Modify: `src/main.rs` (`try_run_embedded` ~558), `src/lib.rs` (`build_native` stub read ~1007, output write ~1033)
- Test: `tests/native.rs`

- [ ] **Step 1: Write the failing test** — (a) a valid footer whose payload read fails reports a clear error (not clap's "missing subcommand"); (b) building with an already-bundled `ascript` strips the old overlay (output not double-sized); (c) the output is written via temp+rename.
- [ ] **Step 2: Run them — expect FAIL.**
- [ ] **Step 3: Apply the fixes** —
  - `try_run_embedded`: after `validate_footer` returns `Some`, switch the payload `seek`/`read_exact` from `.ok()?` to explicit error reporting that returns `Some(ExitCode::from(1))` with `eprintln!("error: failed to read embedded program: {e}")`.
  - `build_native`: strip an existing overlay before using the stub —
    ```rust
    let raw = std::fs::read(&exe)?;
    let stub = match crate::bundle::read_bundle_footer(&raw) {
        Some((offset, _)) => raw[..offset].to_vec(),
        None => raw,
    };
    ```
  - output: write to `out_path.with_extension("tmp")`, chmod, sign, append payload+footer, then atomic `rename` to `out_path`.
- [ ] **Step 4: Run them — expect PASS** (incl. existing `native_*` tests).
- [ ] **Step 5: §9.1** — native tests; docs: native-build page note; blast-radius: confirm worker re-exec / `current_exe` paths still resolve after the rename.
- [ ] **Step 6: Commit** — `git commit -m "fix(bin): report embedded payload errors; strip double-bundle; atomic output"`

### Task 0.20: Phase 0 holistic review

- [ ] **Step 1:** Dispatch a holistic-review subagent over the **combined** Phase 0 diff: cross-fix consistency, no regressions to the four-mode differential, clippy clean in BOTH feature configs, every fix has a regression test + the `.as`/docs deliverables where surface-visible, and the NUM-split / replay-reader / `as usize` blast-radius audits actually landed their discovered fixes.
- [ ] **Step 2:** Any holistic finding becomes a tracked task in this phase and is fixed before Phase 0 closes.
- [ ] **Step 3:** Tick this box only when the holistic review passes and `cargo test` + `cargo test --no-default-features` + both clippy configs are green.

---

## Phase 1 — Module archive format + in-memory loader (self-containment, no shaking yet)

> This phase makes multi-module programs self-contained by embedding whole reachable modules; Phase 2 adds shaking. After Phase 1, a `--native` binary and a `build` archive run with NO source tree present.

### Task 1.1: `CapSet` serialization helpers

**Files:**
- Modify: `src/stdlib/caps.rs`
- Test: inline `#[test]`

- [ ] **Step 1: Write the failing test** — a `CapSet` with `bits`, an `fs_scope` (write-deny + allowed prefixes), and a `net_scope` (external-deny + allowed hosts) round-trips through `to_bytes`/`from_bytes`.
- [ ] **Step 2: Run it — expect FAIL** (helpers don't exist).
- [ ] **Step 3: Implement** `pub fn to_bytes(&self) -> Vec<u8>` and `pub fn from_bytes(b: &[u8]) -> Result<(CapSet, usize), CapsDecodeError>`: write `bits:u8`, then `fs_scope` as `0u8` (none) or `1u8 + mode + len-prefixed prefix list`, then `net_scope` likewise. `from_bytes` bounds-checks every length against `b.len()` and returns the bytes consumed.
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — Rust test only; blast-radius: ensure no existing `CapSet` field is omitted (compile-time exhaustiveness — destructure the struct in `to_bytes` so a new field breaks the build).
- [ ] **Step 6: Commit** — `git commit -m "feat(caps): CapSet to_bytes/from_bytes for archive embedding"`

### Task 1.2: the `ModuleArchive` container codec

**Files:**
- Create: `src/vm/archive.rs`
- Modify: `src/vm/mod.rs` (`pub mod archive;`)
- Test: inline `#[test]` + `tests/archive.rs`

- [ ] **Step 1: Write the failing test** — build a `ModuleArchive` with entry id 0, a `CapSet`, and two `(key, chunk_bytes)` modules; `encode` then `decode` yields an equal archive; a truncated buffer and an over-large `count` both return `ArchiveError`, never panic.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Implement**:

```rust
pub const ARCHIVE_MAGIC: [u8; 8] = *b"ASCRIPTA";
pub const ARCHIVE_VERSION: u16 = 1;

pub struct ModuleArchive {
    pub entry: u32,
    pub caps: crate::stdlib::caps::CapSet,
    pub shake_digest: [u8; 32],
    pub modules: Vec<(String, Vec<u8>)>, // (logical key, verified .aso chunk bytes)
}

impl ModuleArchive {
    pub fn encode(&self) -> Vec<u8> { /* magic, version, entry, caps.to_bytes(), digest,
        count:u32, then per module: key (len-prefixed utf8) + chunk (len-prefixed) */ }
    pub fn decode(b: &[u8]) -> Result<ModuleArchive, ArchiveError> { /* every length
        bounds-checked against remaining input; count.min(remaining) reserve; CapSet via
        from_bytes; module chunks NOT verified here — verified lazily on load */ }
    pub fn get(&self, key: &str) -> Option<&[u8]> { /* linear/hashmap lookup */ }
}
```

- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — Rust + `tests/archive.rs`; blast-radius: confirm `ARCHIVE_MAGIC` is distinct from `bundle::BUNDLE_MAGIC` (`ASCRIPTB`) and `aso::ASO_MAGIC` (`ASO\0`) with an assertion test.
- [ ] **Step 6: Commit** — `git commit -m "feat(vm): ModuleArchive container codec with bounds-checked decode"`

### Task 1.3: build the archive by walking the import graph (whole modules)

**Files:**
- Modify: `src/lib.rs` (new `compile_archive(entry: &Path) -> Result<ModuleArchive, AsError>`)
- Test: `tests/archive.rs`

- [ ] **Step 1: Write the failing test** — `compile_archive` on `examples/bundle_multimodule.as` (which imports `./bundle_util.as`) produces an archive whose `modules` contains both logical keys and whose entry chunk verifies.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Implement** — a worklist starting at the entry: compile the entry to a verified chunk, scan its `imports` table for `Relative`/`Package` specifiers, resolve each to its logical key + file path (reuse the resolution logic factored out of `load_file_module`), compile each transitively, dedup by logical key. `std/*` specifiers are skipped (native). Entry id is the entry's index. `caps`/`shake_digest` are placeholders here (filled in Phases 3/2).
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — `examples/bundle_multimodule.as` + `examples/bundle_util.as`; docs: stub the bundles page; blast-radius: circular imports (A imports B imports A) must terminate (dedup by key before recursing) — add that test.
- [ ] **Step 6: Commit** — `git commit -m "feat(build): compile_archive walks the import graph"`

### Task 1.4: in-memory module loader on the Interp

**Files:**
- Modify: `src/vm/run.rs` (`load_file_module`), the `Interp`/`Vm` setup in `src/lib.rs`
- Test: `tests/archive.rs`

- [ ] **Step 1: Write the failing test** — running an archive whose modules exist ONLY in-memory (no files on disk) executes correctly and produces the same output as the on-disk run.
- [ ] **Step 2: Run it — expect FAIL** (loader hits disk, file absent).
- [ ] **Step 3: Implement** — add `module_archive: Rc<RefCell<Option<Rc<ModuleArchive>>>>` to the runtime; at the top of `load_file_module`, after computing the logical `canon`/key, consult the archive: a hit returns the embedded verified chunk (run it through `from_bytes_verified` exactly as the disk path does, then proceed identically); a miss falls through to today's disk path unchanged.
- [ ] **Step 4: Run it — expect PASS**; `cargo test --test vm_differential` over a multi-module example.
- [ ] **Step 5: §9.1** — archive test; blast-radius: the module cache key, circular-import in-progress marker, and once-only side-effect semantics must be identical whether loaded from archive or disk (assert with a side-effect-counting example).
- [ ] **Step 6: Commit** — `git commit -m "feat(vm): consult in-memory module archive before disk in load_file_module"`

### Task 1.5: emit/run archives from `build` and `--native`

**Files:**
- Modify: `src/lib.rs` (`compile_verified_aso_bytes`, `build_native`, `run_embedded_aso`, `run_verified_aso`/`run_aso_file`), `src/main.rs`
- Test: `tests/native.rs`, `tests/cli.rs`

- [ ] **Step 1: Write the failing test** — (a) `ascript build multimodule.as -o out.aso` then `ascript run out.aso` from a directory WITHOUT the sources works; (b) `ascript build --native multimodule.as -o app` then `./app` from an empty dir works.
- [ ] **Step 2: Run them — expect FAIL.**
- [ ] **Step 3: Implement** — `build`/`build --native` call `compile_archive`; when the graph has >1 module, serialize the `ModuleArchive` (magic `ASCRIPTA`) as the `.aso`/payload; a single-module graph still emits a bare chunk (`ASO\0`) for compat. The loader/`run_aso_file`/`run_embedded_aso` dispatch on the leading magic: `ASO\0` → today's single-chunk path; `ASCRIPTA` → decode the archive, install it into `module_archive`, run the entry chunk.
- [ ] **Step 4: Run them — expect PASS.**
- [ ] **Step 5: §9.1** — native + cli tests; docs: bundles page "what's embedded"; blast-radius: `bundle.rs` `validate_footer` is unchanged (payload is opaque), but `try_run_embedded` must hand the archive bytes to the archive-aware runner; verify `.aso` golden tests dispatch correctly.
- [ ] **Step 6: Commit** — `git commit -m "feat(build): emit and run module archives from build and --native"`

### Task 1.6: worker parity for embedded archives

**Files:**
- Modify: `src/vm/run.rs` / `src/worker/` (the code-shipping path that stashes `worker_aso_bytes`)
- Test: `tests/native.rs` (extend `native_worker_bundle_parity`)

- [ ] **Step 1: Write the failing test** — a bundled multi-module app whose `worker fn` calls into an imported module runs correctly from an empty dir.
- [ ] **Step 2: Run it — expect FAIL** (worker isolate can't find the imported module on disk).
- [ ] **Step 3: Implement** — ship the whole `ModuleArchive` (not just the entry `.aso`) across the worker airlock; the worker isolate installs it into its own `module_archive` at bootstrap so `load_file_module` resolves embedded modules.
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — native test; blast-radius: archive bytes crossing the airlock must be plain `Send` bytes (they are); confirm no `Rc`/handle leaks across the boundary.
- [ ] **Step 6: Commit** — `git commit -m "feat(worker): ship module archive to worker isolates"`

### Task 1.7: Phase 1 holistic review

- [ ] **Step 1:** Holistic-review subagent over the combined Phase 1 diff: archive↔disk semantic equivalence (cache, cycles, side-effect ordering), magic dispatch correctness across all run paths (`run .aso`, `run .as`, `--native`, REPL, worker), bounds-checked decode, clippy both configs.
- [ ] **Step 2:** Findings become tracked tasks fixed before phase close.
- [ ] **Step 3:** Tick only when the four-mode differential over multi-module programs is green in both feature configs.

---

## Phase 2 — Tree-shaker + build report

### Task 2.1: reachability worklist over the resolved module graph

**Files:**
- Create: `src/compile/shake.rs`
- Modify: `src/compile/mod.rs` (`mod shake;`)
- Test: inline `#[test]` + `tests/archive.rs`

- [ ] **Step 1: Write the failing test** — given a graph where the entry uses `import { used } from "./m"` and `m` also defines an unreferenced `fn unused()`, `compute_reachable` returns a keep-set for `m` containing `used` (and its transitive refs) but NOT `unused`.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Implement** `compute_reachable(graph) -> ReachResult { keep: Map<ModuleId, Set<BindingId>>, report: ShakeReport }`: roots = all entry top-level statements; worklist marks referenced top-level bindings, follows import edges (named → specific export; transitively into definitions), keeps all side-effectful top-level statements unconditionally. Uses the resolver's existing reference info.
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — Rust + archive test; blast-radius: classes kept whole (superclass chain, interfaces, enum variants reachable); enums/interfaces handled.
- [ ] **Step 6: Commit** — `git commit -m "feat(shake): reachability worklist over module graph"`

### Task 2.2: dynamic-access & escape detection → pin whole module

**Files:**
- Modify: `src/compile/shake.rs`
- Test: inline `#[test]`

- [ ] **Step 1: Write the failing test** — a module namespace-imported as `import * as m` and then indexed `m[key]` (dynamic) keeps ALL of `m`'s exports; a namespace used only as `m.literal` shakes the rest.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Implement** — when resolving a `Namespace` import, scan its uses: any dynamic `GetIndex`/computed member on `m`, or `m` escaping (returned/stored/passed), pins every export of the target module (mark all reachable + record the reason+span in the report). Only all-static-`.literal` access permits per-binding shaking.
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — Rust test; blast-radius: re-exports (`export { x } from "./y"`) treated as a reference to `x` in `y`; a value-returning function whose result escapes does not under-shake.
- [ ] **Step 6: Commit** — `git commit -m "feat(shake): pin whole module on dynamic namespace access/escape"`

### Task 2.3: compile only kept declarations into each archived module

**Files:**
- Modify: `src/lib.rs` (`compile_archive` uses the keep-set), `src/compile/mod.rs` (compile-with-keep-set entry)
- Test: `tests/archive.rs`

- [ ] **Step 1: Write the failing test** — the archive chunk for `m` does NOT contain `unused`'s code (assert by size/disasm or by a runtime probe that `m.unused` is absent when shaken), while `used` works.
- [ ] **Step 2: Run it — expect FAIL** (whole module still compiled).
- [ ] **Step 3: Implement** — thread the per-module keep-set into compilation so unreferenced inert top-level declarations are not emitted; side-effectful statements and kept decls are emitted in source order (preserving side-effect semantics).
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — archive test; blast-radius: a dropped binding must not leave a dangling reference (the keep-set is closed under references by construction — assert no `undefined` at runtime).
- [ ] **Step 6: Commit** — `git commit -m "feat(build): emit only reachable declarations per archived module"`

### Task 2.4: the build report + manifest digest

**Files:**
- Modify: `src/compile/shake.rs` (`ShakeReport`), `src/lib.rs` (emit to stderr; digest into manifest)
- Test: `tests/cli.rs`

- [ ] **Step 1: Write the failing test** — building a program with a dynamically-indexed namespace prints a report line naming the kept module + reason + span; a fully-shakeable build prints what was dropped.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Implement** — `ShakeReport` records per-module dropped names and kept-because-unprovable entries (reason + span); `build`/`--native` print it to stderr; a 32-byte digest of the canonicalized report goes into the archive manifest.
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — cli test; docs: bundles page "reading the shake report"; blast-radius: report must be deterministic (stable ordering) so the digest is reproducible.
- [ ] **Step 6: Commit** — `git commit -m "feat(shake): build report + reproducible manifest digest"`

### Task 2.5: the shaken-vs-unshaken differential (the load-bearing tripwire)

**Files:**
- Modify: `tests/archive.rs`
- Test: this IS the test

- [ ] **Step 1: Write the test** — for each multi-module example, run it (a) unshaken from disk and (b) as a shaken archive; assert byte-identical stdout across both, and across all engine modes.
- [ ] **Step 2: Run it — expect PASS** (if it fails, shaking dropped live code — a real bug to fix before proceeding).
- [ ] **Step 3:** Add adversarial fixtures: namespace + dynamic index, re-exports, escaping function values, circular imports, side-effectful top-level.
- [ ] **Step 4: Run them — expect PASS.**
- [ ] **Step 5: §9.1** — this is the test; blast-radius: this gate guards the whole feature's correctness.
- [ ] **Step 6: Commit** — `git commit -m "test(shake): shaken-vs-unshaken byte-identical differential"`

### Task 2.6: Phase 2 holistic review

- [ ] **Step 1:** Holistic-review subagent: soundness of the conservatism rules (no false drops), determinism of the report/digest, the differential corpus genuinely exercises the fallback paths, clippy both configs.
- [ ] **Step 2:** Findings → tracked tasks, fixed before phase close.
- [ ] **Step 3:** Tick only when the shaken-vs-unshaken differential + four-mode differential are green in both configs.

---

## Phase 3 — Capability embedding (closes N4)

### Task 3.1: embed the build-time CapSet into the manifest

**Files:**
- Modify: `src/lib.rs` (`build_native`, `compile_archive`/`build`), `src/main.rs` (pass composed caps into the builder)
- Test: `tests/native.rs`

- [ ] **Step 1: Write the failing test** — `ascript build --native --deny net app.as -o app` produces an archive whose manifest `CapSet` has `net` denied.
- [ ] **Step 2: Run it — expect FAIL** (manifest caps are all-granted placeholder).
- [ ] **Step 3: Implement** — the builder receives the composed `CapSet` (from `compose_caps`: CLI `--deny`/`--sandbox`/carve-outs + `ascript.toml [capabilities]`) and stores it in the manifest via `CapSet::to_bytes`.
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — native test; docs: bundles page "embedded capabilities"; blast-radius: a plain `build` (non-native) embeds caps too — confirm consistent.
- [ ] **Step 6: Commit** — `git commit -m "feat(build): embed composed CapSet into the archive manifest"`

### Task 3.2: enforce embedded caps at runtime (the N4 fix)

**Files:**
- Modify: `src/lib.rs` (`run_embedded_aso`, archive runner), `src/main.rs`
- Test: `tests/native.rs`

- [ ] **Step 1: Write the failing test** — `./app` (built with `--deny net`) attempting `net.*` gets a capability-denied Tier-2 error.
- [ ] **Step 2: Run it — expect FAIL** (currently all-granted via `caps: None`).
- [ ] **Step 3: Implement** — the archive runner reads the manifest `CapSet` and calls `interp.set_caps(...)`, replacing the `caps: None` at `lib.rs:1106`.
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — native test (run a denied + a granted call); docs: confirm the page documents the enforcement; blast-radius: a single-module bundle (bare chunk, no manifest) — decide + implement its caps source (embed a minimal caps header in the single-chunk native payload, or always emit an archive for `--native` so caps always have a home; choose the latter for uniformity and document).
- [ ] **Step 6: Commit** — `git commit -m "fix(bin): enforce embedded capabilities at runtime (closes N4)"`

### Task 3.3: `ASCRIPT_DENY` monotone launch-time subtraction

**Files:**
- Modify: `src/main.rs` / the archive runner
- Test: `tests/native.rs`

- [ ] **Step 1: Write the failing test** — `ASCRIPT_DENY=fs ./app` denies `fs` even if the embedded set granted it; it can never RE-grant a denied cap.
- [ ] **Step 2: Run it — expect FAIL.**
- [ ] **Step 3: Implement** — after installing the embedded `CapSet`, parse `ASCRIPT_DENY` (comma-separated cap names, same grammar as `--deny`) and subtract those caps (intersection-only; never add). Argv is untouched (still forwarded to the program).
- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: §9.1** — native test; docs: bundles page "restricting a bundle at launch"; blast-radius: invalid cap names in `ASCRIPT_DENY` → a clear startup error, not a silent ignore.
- [ ] **Step 6: Commit** — `git commit -m "feat(bin): ASCRIPT_DENY monotone subtracts from embedded caps"`

### Task 3.4: Phase 3 holistic review

- [ ] **Step 1:** Holistic-review subagent: caps are sourced identically by `build` and `--native`, enforcement matches a normal `--deny` run, `ASCRIPT_DENY` is strictly monotone, the macOS overlay-signing caveat (spec §10) is documented, clippy both configs.
- [ ] **Step 2:** Findings → tracked tasks, fixed before phase close.
- [ ] **Step 3:** Tick only when capability-enforcement tests pass in both configs.

---

## Phase 4 — Wiring, docs, examples, fuzzing, full matrix

### Task 4.1: user docs page

**Files:**
- Create: `docs/content/language/bundles.md`
- Modify: `docs/assets/app.js` (`NAV` array), `README.md`
- Test: manual + the docs-build check

- [ ] **Step 1:** Write `bundles.md`: what `build`/`--native` embed, the module archive, tree-shaking + reading the report, embedded capabilities + `ASCRIPT_DENY`, the std-stays-native and macOS-signing notes.
- [ ] **Step 2:** Add the `bundles` slug to the `NAV` array (sidebar + cmd-K), and a README mention.
- [ ] **Step 3:** Verify in-content links resolve relative to the page dir.
- [ ] **Step 4: §9.1** — docs is the deliverable; blast-radius: a new page absent from `NAV` is unreachable — assert the slug is present.
- [ ] **Step 5: Commit** — `git commit -m "docs(bundles): self-contained bundles, tree-shaking, embedded caps"`

### Task 4.2: archive deserialization fuzz target

**Files:**
- Create: `fuzz/fuzz_targets/archive_roundtrip.rs`
- Modify: `fuzz/Cargo.toml`
- Test: `cargo fuzz run archive_roundtrip -- -runs=100000` (CI smoke)

- [ ] **Step 1:** Write a fuzz target that feeds arbitrary bytes to `ModuleArchive::decode` and asserts it never panics (always `Ok`/`Err`), and that `encode∘decode` round-trips a structured input.
- [ ] **Step 2:** Register the target; add a seed corpus from real archives.
- [ ] **Step 3:** Run the smoke campaign — expect zero crashes.
- [ ] **Step 4: §9.1** — the fuzz target is the deliverable; blast-radius: wire into the nightly fuzz CI alongside `aso_roundtrip`.
- [ ] **Step 5: Commit** — `git commit -m "test(fuzz): archive decode fuzzing target"`

### Task 4.3: corpus examples + advanced

**Files:**
- Create: `examples/bundle_multimodule.as`, `examples/bundle_util.as`, `examples/advanced/bundle_caps.as`
- Test: the conformance corpus

- [ ] **Step 1:** Ensure the examples are runnable (`target/release/ascript run`), error-handled (advanced), and exercise named + namespace imports, a shaken unused export, and (advanced) a `--deny`-built bundle.
- [ ] **Step 2:** Add them to the corpus/skip lists appropriately (a port-binding server-style example would be skipped; these run to completion so they are NOT skipped).
- [ ] **Step 3: §9.1** — the examples are the deliverable; blast-radius: they participate in the four-mode differential.
- [ ] **Step 4: Commit** — `git commit -m "examples(bundle): multi-module + caps examples"`

### Task 4.4: full matrix + Definition of Done

**Files:** none (verification)

- [ ] **Step 1:** `cargo test` (default) green.
- [ ] **Step 2:** `cargo test --no-default-features` green.
- [ ] **Step 3:** `cargo clippy --all-targets` clean.
- [ ] **Step 4:** `cargo clippy --no-default-features --all-targets` clean.
- [ ] **Step 5:** `cargo test --test vm_differential` (both configs) green over the multi-module corpus.
- [ ] **Step 6:** shaken-vs-unshaken differential green.
- [ ] **Step 7:** LSP / tree-sitter / formatter checks green for any surface touched.
- [ ] **Step 8:** docs/examples present; `NAV` updated; `ASO_FORMAT_VERSION`/`ARCHIVE_VERSION` bumped where layout changed.

### Task 4.5: Phase 4 + whole-effort holistic review (Definition of Done)

- [ ] **Step 1:** Final holistic-review subagent over the ENTIRE diff (Phases 0–4): spec §1–§8 coverage, §9 execution-standard adherence, zero open `TODO`/deferral, zero known unfixed bugs, every discovered bug fixed with a regression test.
- [ ] **Step 2:** Confirm every checkbox in this plan is ticked.
- [ ] **Step 3:** Tick this box — **nothing left to do.**

---

## Self-Review (author pass)

- **Spec coverage:** §1 motivation → Phase 0 (bugs) + Phases 1–3 (self-containment, shaking, caps); §3 archive → Tasks 1.2/1.5; §4 shaking → Phase 2; §5 caps → Phase 3; §6 loader → Task 1.4; §7 testing → Tasks 2.5, 4.2, 4.4; §9 standards → embedded in every task + holistic reviews; §10 risks → covered by the differential corpus (Task 2.5), portable-key test (Task 1.3/1.5), signing caveat doc (Task 3.4/4.1).
- **No placeholders:** every code-changing step shows the code or the exact transform; new-file tasks define the concrete types/signatures used by later tasks (`ModuleArchive`, `CapSet::to_bytes/from_bytes`, `compute_reachable`, `compile_archive`).
- **Type consistency:** `ModuleArchive { entry, caps, shake_digest, modules }` and `module_archive` runtime field are referenced consistently across Tasks 1.2 → 1.4 → 1.6 → 3.2; `CapSet::to_bytes/from_bytes` defined in 1.1 and used in 1.2/3.1/3.2.

