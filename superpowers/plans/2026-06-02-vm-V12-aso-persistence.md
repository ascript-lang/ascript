# VM Plan V12 — `.aso` bytecode persistence + import resolution

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Serialize a `Chunk` to an on-disk `.aso` ("AScript Object") with a version-magic header; verify bytecode on load (jump targets, stack-depth balance, operand ranges); add `ascript build foo.as → foo.aso`; and make `import` resolve `.aso` modules (the `.pyc`/`.jar` model — NOT native FFI). Specialization is runtime-only and does NOT serialize; the `.aso` holds the GENERIC chunk so the format is stable across IC/spec evolution.

**Architecture:** A `Chunk` is serializable because its const pool holds only compile-time literals + nested `FnProto`s (never runtime `Value`s). Implement a compact binary (de)serializer (hand-rolled or `bincode`-style; the const pool's `Value`s are restricted to the literal subset — number/string/bool/nil/decimal — so serialization is total). The loader verifies + reconstructs the `Chunk`. `import` precedence: prefer `.aso` when no source is present or the `.aso` is up-to-date vs source (mtime, Python's rule), else compile source. **Depends on V11** (serialize the generic chunk; ICs are not serialized).

---

## Ground truth
- Const pool restricted to literals + protos (V1 design). If any runtime-only `Value` ever entered a const pool, serialization would be partial — assert/verify the pool is literals-only at build time.
- `import` today resolves `std/*` (stdlib) and `.as` files (`std_module_exports` + `load_module`). Extend the file-module path to also find/load `.aso`.
- Version header: `.aso` is tied to a specific opcode set + value layout. A mismatch RECOMPILES (or errors) — never runs stale bytecode.
- Trust model: loading `.aso` runs its bytecode → treat as trusted input (like `.pyc`) AND run a verifier on load.

---

## Tasks
- [ ] **T1 — Chunk (de)serialization.** Implement `Chunk::to_bytes() -> Vec<u8>` / `Chunk::from_bytes(&[u8]) -> Result<Chunk, AsoError>`: a magic header (`b"ASO\0"` + a `u32` format version derived from a hash of the opcode set + value layout), then code/consts/protos/spans/upvalues/slot_count. Serialize the const pool's literal subset (number f64 bits, string len+utf8, bool, nil tag, decimal). Recurse into protos. Do NOT serialize ICs/shapes (runtime-only) — `ic_count` is recorded but caches are empty on load. Round-trip tests: serialize→deserialize a non-trivial chunk → structurally equal (disasm equal). Version-mismatch → error/recompile signal. Commit `feat(aso): Chunk serialization + version header`.
- [ ] **T2 — bytecode verifier.** `verify(&Chunk) -> Result<(), AsoError>`: validate every jump target lands on an instruction boundary within bounds; operand indices (const/proto/slot/upvalue) in range; stack-depth balance (abstract-interpret the code, ensure no underflow and a consistent depth at each join — at minimum non-underflow + RETURN-balanced); recurse into protos. Tests: reject a chunk with an out-of-range const index, a jump into the middle of an instruction, a stack underflow. Commit.
- [ ] **T3 — `ascript build`.** Add `Command::Build { file, out: Option<String> }` to `src/main.rs`: compile `foo.as` → `Chunk` → `to_bytes` → write `foo.aso` (or `--out`). Refuse files with parse/resolve errors (report via the checker/diagnostics). Integration test (`tests/aso.rs`): `ascript build` a sample, assert the `.aso` exists, header valid, verifier passes, and running it (`ascript run foo.aso`) produces the same stdout as `ascript run foo.as`. Commit.
- [ ] **T4 — `ascript run foo.aso` + import resolution.** `run_file`/the run path: if the file is `.aso`, load+verify+run its top-level (no compile step). `import` resolution: when resolving a file module `foo`, look for `foo.aso` AND `foo.as`; prefer `.aso` when no source OR `.aso` mtime >= source mtime (Python rule), else compile source. Verify header on load; on mismatch, recompile from source (if present) or error. Transitive imports work. Bind exports identically to importing source. Tests: import an `.aso` module; stale `.aso` (older than source) recompiles; `.aso`-only (no source) runs; transitive `.aso` imports; behavior identical to importing source. Commit.
- [ ] **T5 — full suite + clippy both configs.** The differential gate still holds (running via `.aso` == running source == tree-walker). Commit.

## Done criteria (V12)
- [ ] `Chunk` (de)serializes with a version header; verifier rejects malformed bytecode; ICs/shapes not serialized (generic chunk only).
- [ ] `ascript build foo.as → foo.aso`; `ascript run foo.aso` and `import` of `.aso` work, with Python-style precedence; behavior identical to source.
- [ ] `cargo test` green; clippy clean both configs.

**Next:** V13 — the CLOSING phase: migrate cycle-capable `Value`s + upvalue cells + Fiber/Closure structures `Rc → Cc` (gcmodule), implement `Trace`, enable + tune the Bacon–Rajan cycle collector; soundness (cycle reclamation), soak (`http.serve`), and deterministic native-resource `Drop` gates.
