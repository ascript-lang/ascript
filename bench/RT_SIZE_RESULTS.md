# RT Phase-0 Size Matrix

**Task:** RT Task 0 — measurement + pinning only; no production code changes.

**Date:** 2026-06-17  
**Machine:** Darwin Mahmouds-Mini.lan 25.5.0 arm64 (Apple Silicon)  
**Rust:** rustc 1.96.0 (ac68faa20 2026-05-25)  
**Profile:** `--release`  

---

## (a) Full-binary baseline (all default features)

```
Full binary size: 45,400,368 bytes  (43.29 MB)
```

This is `cargo build --release` with all default features, the binary whose stub RT
will replace. The `.text` section is 24.5 MB of that 45 MB.

---

## (b) Per-feature size delta table

Floor = `--no-default-features --features shared` (the VM + GC + core language,
shared-heap support, but none of the optional stdlib tiers):

```
Floor (shared only):  19,105,184 bytes  (18.22 MB)
```

| Feature | Binary (bytes) | Binary (MB) | Delta vs floor |
|---------|---------------|-------------|----------------|
| shared (floor) | 19,105,184 | 18.22 MB | +0 |
| datetime | 19,122,048 | 18.23 MB | +16,864 |
| sysinfo | 19,104,848 | 18.21 MB | −336 (rounding) |
| ffi | 19,154,448 | 18.26 MB | +49,264 |
| crypto | 19,218,048 | 18.32 MB | +112,864 |
| tui | 19,286,592 | 18.39 MB | +181,408 |
| redis | 19,483,728 | 18.58 MB | +378,544 |
| data | 19,773,376 | 18.85 MB | +668,192 |
| log | 19,806,720 | 18.88 MB | +701,536 |
| binary | 19,858,016 | 18.93 MB | +752,832 |
| sys | 19,905,568 | 18.98 MB | +800,384 |
| workflow | 19,912,864 | 18.99 MB | +807,680 |
| intl | 20,539,808 | 19.58 MB | +1,434,624 |
| sql | 20,895,808 | 19.92 MB | +1,790,624 |
| compress | 21,281,440 | 20.29 MB | +2,176,256 |
| telemetry | 22,212,384 | 21.18 MB | +3,107,200 |
| ai | 33,052,832 | 31.52 MB | +13,947,648 |
| net | *(build failed)* | — | depends on `data` |
| postgres | *(build failed)* | — | depends on `data` + `net` |

**Notes:**
- `net` and `postgres` failed to compile in isolation: both require `data`
  (`serde_json`/`json` module) which is a separate feature. Their sizes are not
  independently measurable with this method; they are included in the full-binary
  baseline and in any build with `data` enabled.
- `sysinfo` shows −336 bytes vs floor (measurement noise / dead-code elimination
  — effectively 0 marginal cost).
- `ai` is the largest single optional feature (+13.3 MB), pulling in `genai`,
  AWS SDK, multi-TLS backends, and HTTP clients.
- `compress` (+2.1 MB) pulls in brotli, zstd, lz4, xz, bzip2.

---

## (c) Toolchain-share estimate

From `cargo bloat --release -n 40` (installed automatically during the run):

```
 File  .text     Size      Crate Name
 0.2%   0.4% 107.9KiB    ascript ascript::vm::run::Vm::run_loop::{{closure}}
 ...
 0.1%   0.2%  57.4KiB    ascript <ascript::cli_surface::Command as clap_builder::derive::Subcommand>::augment_subcommands
 0.1%   0.2%  47.4KiB    ascript ascript::syntax::format::Printer::stmt  ← formatter (toolchain-only)
 0.1%   0.2%  44.4KiB    ascript brotli::enc::...                         ← compress crate
 0.1%   0.2%  38.8KiB  tower_lsp tower_lsp::generated::register_lsp_methods  ← LSP
 0.1%   0.1%  36.3KiB    ascript ascript::compile::Compiler::compile_stmt ← compiler (toolchain-only)
 0.1%   0.1%  35.7KiB    ascript ascript::syntax::format::Printer::expr   ← formatter
 ...
 50.1%  92.0%  22.5MiB            And 54,583 smaller methods.
 54.4% 100.0%  24.5MiB            .text section size, the file size is 45.0 MiB
```

Identified toolchain-only symbols (not needed in an `ascript-rt` stub):

| Component | Visible in bloat top-40 | Rough .text share |
|-----------|------------------------|-------------------|
| `clap` CLI surface (`augment_subcommands`) | 57.4 KiB | ~57 KiB |
| `ascript::syntax::format` (formatter, `fmt` subcommand) | 47.4 KiB + 35.7 KiB | ~83 KiB |
| `ascript::compile` (bytecode compiler) | 36.3 KiB + others | ~200+ KiB |
| `tower_lsp` (language server protocol) | 38.8 KiB top symbol | ~1 MB+ |
| `cstree` / `ascript::syntax` (CST front-end) | many small entries | ~500+ KiB |
| `ascript::check` (static checker) | not in top-40 | ~500+ KiB |
| `rustyline` (REPL readline) | not in top-40 | ~200+ KiB |
| `tree_sitter` (grammar) | not in top-40 | ~200+ KiB |
| `apple_codesign` (macOS bundle signer) | 25.7 KiB top | ~200+ KiB |
| `dap` debugger server | 21.2 KiB top | ~300+ KiB |

**Estimate:** The gap between the full 43.3 MB binary and the `shared`-floor 18.2 MB
is **25.1 MB** of optional stdlib tiers + default features. The toolchain-only
front-end components (LSP, CST/cstree, compiler, formatter, checker, REPL,
tree-sitter, clap, codesign, DAP) account for a further **≈ 3–4 MB** within the
floor's 18.2 MB that the runtime stub does not need. Conservatively:

> **≈ 3–4 MB of the 18.2 MB shared-floor binary is toolchain-only (gated out of a
> stub); the achievable ascript-rt floor is ≈ 14–15 MB before any additional
> feature gating is applied.**

This is a rough estimate from bloat top-40. A precise number requires building
with `lsp`, `syntax::format`, `dap`, `check`, and `repl` gated behind a `toolchain`
Cargo feature — that gate is Task 2's deliverable.

---

## Per-tier ascript-rt sizes (appended by Task 2)

[PLACEHOLDER — Task 2 will define the `ascript-rt` Cargo feature set and measure
 the resulting stub binary sizes per tier. Expected tiers: `core` (VM + GC only),
 `runtime-minimal` (+ sys + data + net), `runtime-full` (+ all non-toolchain
 features). Task 2 will fill in this section with real numbers and a comparison
 to the full `ascript` binary.]

---

## Key headline numbers

- **Full toolchain binary:** 43.3 MB (45,400,368 bytes)
- **Shared-floor binary (core language, no optional stdlib):** 18.2 MB (19,105,184 bytes)
- **Toolchain-share within floor (LSP + compiler + formatter + checker + REPL etc.):** ≈ 3–4 MB
- **Achievable rt-stub floor:** ≈ 14–15 MB (estimate; Task 2 measures it precisely)
- **Largest single feature:** `ai` (+13.3 MB over floor = AWS SDK + genai)
- **Smallest meaningful features:** `datetime` (+16 KB), `sysinfo` (≈ 0), `ffi` (+49 KB)
