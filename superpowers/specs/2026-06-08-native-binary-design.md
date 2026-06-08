# AScript Native Single-Binary Distribution — Design (BIN)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** BIN (Serious Language campaign — see `goal.md`)
- **Depends on:** **FUZZ** (the `.aso` deserializer + verifier must be fuzz-hardened first, AND the
  standalone P0 reader allocation-clamp bugfix it owns must have landed — this is a hard gate, not a
  courtesy; §4). Independent of the NUM/VAL/ADT/IFACE/TYPE/FFI track. Builds cleanly on the shipped
  `src/worker/` isolate model (§7).
- **Depended on by:** nothing (a leaf deployment feature).
- **Engines:** the **bytecode VM only** — an embedded binary always runs the embedded `.aso`
  on the VM (`.aso` is "always VM", `main.rs:21`/`:229`). The tree-walker is unaffected (it
  remains reachable as `--tree-walker` over a `.as` *source* file, never over an embedded chunk).
- **Breaking:** **no.** Purely additive, CLI-side. No language change, no new `Value`/AST/opcode,
  **no `.aso` format change → no `ASO_FORMAT_VERSION` bump** (the verified `.aso` is *embedded*
  verbatim, not altered).

---

## 1. Summary & motivation (the deployment gap)

AScript today ships as a single Rust binary, `ascript`, and runs programs two ways: `ascript run
file.as` (compile to bytecode → VM) and `ascript run file.aso` (run pre-compiled bytecode → VM),
with `ascript build file.as` producing the verified `.aso` in between (`main.rs:283`,
`lib.rs:356` `build_file`, `lib.rs:389` `run_aso_file`). Both require **the `ascript` binary to be
installed on the target machine**. To distribute a tool written in AScript you must either tell
users to `cargo install ascript` (or fetch a release) and *then* hand them a `.as`/`.aso`, or
script an installer. There is no "here is one file, run it" deliverable.

Every serious modern scripting runtime closes this gap with a **compile-to-executable** command:
Deno `deno compile`, Bun `bun build --compile`, and (in spirit) PyInstaller / Node's SEA
(single-executable applications). The shared technique is not ahead-of-time machine-code
compilation — it is **bundling**: append the program's bytecode/script to a copy of the runtime
executable, and have the runtime detect-and-run the embedded payload on startup. The VM still
interprets; the binary is just self-contained.

**Campaign repositioning.** This is part of the "scripting → general-purpose language" arc (the
Serious Language campaign, `goal.md`): a single-file native deliverable is a *general-purpose*
capability, not a scripting nicety. A language you can only run via its own installed interpreter is a
scripting tool; a language whose programs ship as one runnable binary stands next to Go/Deno/Bun as a
distributable general-purpose runtime. BIN closes that deployment gap (NUM/VAL/ADT/IFACE/TYPE/FFI close
the language ones).

This spec adds that command to AScript:

```
ascript build --native app.as -o app     # → a self-contained ./app executable
./app                                     # runs with NO `ascript` on PATH; output == `ascript run app.as`
```

**Explicit non-goal — this is NOT AOT compilation.** `--native` does **not** emit machine code for
the AScript program. The embedded VM still interprets the embedded `.aso` exactly as `ascript run
app.aso` does. Native code generation is the deferred **JIT** spec (`goal.md`: "Baseline JIT
(Cranelift)… only after NUM + VAL"); BIN deliberately stays in the bundling tier so it can land
now, behind only the FUZZ gate. The motto: **`--native` changes *packaging*, never *semantics*.**

### What "self-contained" means here

The produced executable is the `ascript` runtime (the same binary, with the same statically- or
dynamically-linked native dependencies it was built with — rustls, the bundled SQLite, etc.) with
the program's `.aso` glued on. It needs no `ascript` on `PATH` and no separate `.aso`/`.as` file.
It is **not** statically linked beyond what the `ascript` build already is (a binary built against
the system libc still needs that libc; cross-compilation feasibility is §3). On disk it is "the
whole runtime + your bytecode" — tens of MB (§6).

## 2. The bundling mechanism

### 2.1 Layout: runtime ‖ payload ‖ footer

`--native` produces:

```text
┌─────────────────────────────┐
│  copy of the `ascript`      │   the runtime, byte-for-byte (current_exe by default,
│  executable (the "stub")    │   or the --target runtime artifact, §3)
├─────────────────────────────┤
│  embedded payload:          │   the verified `.aso` bytes produced by the SAME
│    <.aso bytes>             │   pipeline as `ascript build` (magic "ASO\0" + version
│                             │   + chunk; lib.rs:372 `chunk.to_bytes()`)
├─────────────────────────────┤
│  footer (fixed-size, last   │   appended AFTER the payload (the very end of the file)
│  N bytes of the file):      │
│    payload_offset : u64 LE  │   byte offset of the payload start within the file
│    payload_len    : u64 LE  │   length of the embedded .aso
│    aso_version    : u32 LE  │   ASO_FORMAT_VERSION at bundle time (defence-in-depth, §2.4)
│    bundle_version : u16 LE  │   BIN footer-format version (this spec = 1)
│    reserved       : u16     │   zero; room to grow without a new magic
│    magic          : [u8;8]  │   b"ASCRIPTB" — the bundle sentinel
└─────────────────────────────┘
```

**Why a trailing footer (append-at-end), not a header or a section:**

- **Appending to the end is the portable technique.** A native executable (ELF / Mach-O / PE) is
  read by the OS loader from its *header* and program/section tables; trailing bytes past the last
  mapped segment are ignored by the loader. So you can concatenate arbitrary data after a working
  executable and it still runs. This is exactly the "self-extracting archive" / "polyglot" trick
  Deno and Bun use. We do **not** edit ELF/Mach-O/PE section tables (that is platform-specific,
  fragile, and breaks code-signing differently per OS — §3, §10).
- **Footer at the very end, not header at the start,** because the start is the executable's own
  magic/entry the OS needs. The program reads its own file, seeks to `file_len - FOOTER_SIZE`,
  and checks the sentinel.
- **Fixed-size, self-describing footer** so detection is O(1): read the last `FOOTER_SIZE` bytes,
  compare the magic; if it matches, the offset/length tell us exactly where the `.aso` lives.

The footer is the **single source of truth** for "is this a bundle, and where is the payload." The
magic `ASCRIPTB` is distinct from the `.aso` magic `ASO\0` (`aso.rs:50`) so the two are never
confused.

### 2.2 Build path — `ascript build --native`

`--native` is a flag on the existing `build` subcommand (`main.rs:36`), keeping one verb:

```
ascript build --native app.as -o app
```

Build steps (a new `build_native(file, out, target)` in `lib.rs`, reusing `build_file`'s front
half):

1. **Compile + verify the `.aso`** exactly as `build_file` does today: `compile_source` →
   `verify::verify(&chunk)` (`lib.rs:366`) → `chunk.to_bytes()` (`lib.rs:372`). The payload is the
   *verified* `.aso` byte vector — identical to what `ascript build` writes. (Reuse, do not fork,
   this code path so the embedded `.aso` is provably the same artifact.)
2. **Obtain the stub runtime.** For same-host `--native` (the default, no `--target`): copy the
   currently-running executable, `std::env::current_exe()`. For `--target <triple>`: the fetched/
   cached per-target runtime artifact (§3). The copy preserves the exec bit (and on Unix we
   `chmod +x` the output defensively).
3. **Append `payload || footer`** to the stub copy. Compute `payload_offset` = stub length *before*
   appending; `payload_len` = `.aso` length; write the footer struct LE.
4. **Write to `-o` / default output** (default output name = the source stem with no extension, or
   `<stem>.exe` on Windows — *not* `.aso`). Print `bundled <file> -> <out> (<size>)`.
5. **On macOS, re-apply an ad-hoc code signature** (the equivalent of `codesign -s - <out>`, §10).
   Appending the payload invalidated the stub's signature, and **macOS arm64 will not exec an unsigned
   Mach-O** (it is `SIGKILL`ed). This step is mandatory on macOS-arm64 (harmless / skippable on x86_64
   macOS, Linux, Windows). It seals runnability, NOT authenticity (notarization stays the distributor's
   job, §10).

The whole step is **CLI-side / build-host-side**; nothing about it touches the language, the VM run
loop, or the `.aso` format.

### 2.3 Startup dispatch — `main.rs` footer detection

The runtime gains a startup check **before** clap parses argv. New, in `real_main()` (or a small
pre-clap shim in `main.rs`):

```text
on startup:
  let exe = current_exe()
  if let Some(payload) = read_bundle_footer(exe):     // O(1): read last FOOTER_SIZE bytes, match magic
      run the embedded chunk (§2.4); forward argv (minus argv[0]) as the script args; exit
  else:
      fall through to the normal clap CLI (run / build / repl / check / …)
```

Key design points:

- **Detection is the first thing main does**, gated only on reading the footer of `current_exe()`.
  A plain `ascript` binary has no footer → the read returns `None` → **byte-identical** to today's
  CLI *output*. (A robustness note: if `current_exe()` fails — rare, e.g. the binary was unlinked — we
  treat it as "no bundle" and continue to the normal CLI, never abort.)
- **Startup-cost honesty (Gate 12).** The footer detect is NOT free on the non-bundle path: it adds a
  `current_exe()` resolution plus an open + seek-to-end + read of the last `FOOTER_SIZE` bytes (~32 B)
  on **every** `ascript` launch, including a plain `ascript run`/`build`/`repl`/`check`. This is one
  `stat`/`open`/`pread`-class syscall sequence on a file already in the OS page cache (the executable
  we are running). **It must be benchmarked, not asserted "negligible."** The acceptance gate is a
  recorded measurement: the wall-clock delta between a `main` with the footer check and the current
  `main` for `ascript --version` (a do-nothing launch that isolates pure startup overhead), measured
  with `hyperfine` over ≥1000 runs on the CI reference host. The expectation is sub-millisecond (a
  warm-cache trailing read), but the **number is the deliverable** — if it exceeds a recorded budget
  (proposed: **≤ 1 ms / ≤ 2 % of bare `--version` startup**), the check is moved behind a cheaper guard
  (e.g. only when `argv[0]` is not the canonical `ascript`/`ascript.exe` name, or only when no
  subcommand parses) before BIN locks. This mirrors the SP1 perf-trade posture (a measured ~2.5× note),
  not a hand-wave. The benchmark lives in the BIN plan's acceptance criteria, run in both feature
  configs.
- **In bundle mode, argv is the *program's* args, not ascript subcommands.** `./app foo --bar`
  passes `["foo", "--bar"]` to the script as `env.args()` — the embedded program is the whole CLI.
  There is no `run`/`build`/`repl` surface on a bundled binary (it *is* the app). This mirrors Deno
  compile (the compiled binary takes the script's args, not `deno` flags).
- **A bundled binary still honors the runtime env vars** the embedded program legitimately reads
  (`ASCRIPT_WORKERS` for pool size, `ASCRIPT_LOG`, `ASCRIPT_CACHE`) — these are runtime knobs, not
  CLI parsing. `ASCRIPT_ENGINE=tree-walker` is **ignored in bundle mode** (there is no source to
  re-parse; the embedded payload is `.aso`, which is always VM — same rule as `run file.aso`,
  `main.rs:229`).

### 2.4 The embedded run entry — verified once at startup

The embedded-run path is **`run_aso_file` with the bytes sliced out of `current_exe()` instead of
read from a standalone `.aso` file** (`lib.rs:389`). A new `run_embedded_aso(payload, args)` in
`lib.rs` factors the shared body so the two share one implementation:

1. Slice `payload = exe_bytes[payload_offset .. payload_offset + payload_len]` per the footer.
2. **`Chunk::from_bytes_verified(&payload)`** — the SAME verified loader `run_aso_file` uses
   (`lib.rs:396`; the function itself is `Chunk::from_bytes_verified`, `verify.rs:782` — it composes
   the raw reader `Chunk::from_bytes` with `verify::verify`, returning `FromBytesVerifiedError`). This
   validates the magic (`ASO\0`, `aso.rs:456`), the **`ASO_FORMAT_VERSION`** (the version read from the
   header is compared against the build's constant, NOT hardcoded — `aso.rs:459`–`:460`), and runs the
   full structural **`verify::verify`** pass (decode integrity, operand ranges, jump targets,
   stack-depth balance, recursive proto verification — `verify.rs:1`). A version mismatch or a
   structurally-invalid chunk is a clean `AsError`, printed and a non-zero exit — **never UB, never a
   deep VM panic.** This is the security crux (§4): in a shipped binary the verifier is the only thing
   standing between a tampered payload and the VM.
   - The footer's redundant `aso_version` field lets the runtime emit a *clearer* mismatch message
     ("this binary was bundled with an older AScript; rebuild") **before** even slicing, but it is
     defence-in-depth — `from_bytes_verified` is still the authoritative gate. There is **no
     `--trust`/`--no-verify` bypass** in v1 (rejected, §11).
3. **Set up the run exactly like `run_aso_file`:** `Interp::new_live()` → `set_cli_args(args)` →
   **`set_worker_aso_bytes(payload)`** (so worker dispatch has the bytecode without source —
   `lib.rs:403`, §7) → `install_self` → `Vm::new` → run the top-level closure on a `LocalSet`,
   end-of-program `gc::collect`, map the `RunOutcome`/`Control` to an exit code (`lib.rs:437`).
   Relative-import resolution: a bundled binary has no meaningful "module dir," so embedded imports
   are resolved relative to **`current_exe().parent()`** (documented; in practice a bundled app
   should have inlined its modules at build time — multi-module bundling is noted as a follow-up in
   §3.3, v1 bundles a single compiled top-level chunk just like `build` does).

**Precisely where the verifier runs (don't over-claim).** The embedded payload is verified **exactly
once, at process startup**, via `from_bytes_verified` (step 2). It is **not** re-verified on every
internal re-decode. Specifically, when a worker isolate needs the top-level chunk to build its code
slice, `resolve_worker_top_chunk` (`dispatch.rs:1309`) re-parses the stored bytes with the **unverified**
`Chunk::from_bytes` (`dispatch.rs:1319`), NOT `from_bytes_verified`. This is sound and deliberate: the
bytes in `worker_aso_bytes` are the *same payload `from_bytes_verified` already accepted at startup* —
re-verifying identical, already-verified, in-memory bytes on each worker spawn would be redundant work
on the hot path. The honest claim is therefore: **the embedded chunk is verified once at startup;
worker slices are re-parsed (decode-only) from those already-verified bytes.** No unverified bytes from
an untrusted source ever reach the VM — the single trust boundary is the startup `from_bytes_verified`,
and everything downstream operates on bytes that crossed it.

Because the embedded payload is run through the identical `from_bytes_verified` → VM path as a
standalone `.aso`, **the four-mode byte-identity gate (`goal.md` Gate 1) extends for free**: a
bundled binary's output must equal `ascript run app.aso` which already equals tree-walker ==
specialized == generic (§8).

### 2.5 Why this is no-bugs-safe by construction

- The payload is the **verified** `.aso` (`build_file` already verifies before writing,
  `lib.rs:366`; `--native` reuses that). Bundling can only *append* — it cannot produce an invalid
  chunk from a valid one.
- The runtime **re-verifies at the startup load** (`from_bytes_verified`, the single trust boundary —
  §2.4), so even a *tampered* binary (someone edited the embedded bytes, or the footer offset is wrong)
  is rejected cleanly, not run. A test asserts this (§8). (Worker slices are decode-only re-parses of
  those already-verified bytes, not a second trust boundary — §2.4.)
- The footer is fixed-size and bounds-checked: `payload_offset + payload_len` must be `≤ exe_len -
  FOOTER_SIZE` and `payload_offset` must be `≥` the stub's expected size; otherwise "not a valid
  bundle" → fall through / clean error (never an out-of-bounds slice).

## 3. Cross-compilation (`--target`) — staged, honest feasibility

### 3.1 The hard part: the stub is a *native* binary

Same-host `--native` is easy because the stub is `current_exe()` — already a working executable for
*this* platform. `--target x86_64-unknown-linux-gnu` (building a Linux binary on a Mac, say) needs a
**runtime executable for that triple** to append to. We cannot synthesize one; appending a payload
does not cross-compile Rust.

So `--target` reduces to: **"where do we get an `ascript` executable for triple T?"** And here the
honest answer is that AScript's runtime is **not** a pure-Rust, dependency-free binary:

- **rustls / reqwest** (the `net` feature) — pulls in `ring`/`aws-lc` crypto with per-target
  assembly; cross-linking is doable with the right toolchain but not trivial.
- **bundled SQLite** (`sql` feature, `rusqlite` with the bundled C source) — a C dependency
  compiled per target; needs a cross C toolchain.
- **postgres / redis / compress (zstd/brotli C bits)** — more C.
- **FFI (the future `libffi`-style trampoline, the FFI spec)** — explicitly native, per-target.

These mean a faithful `--target` binary for triple T must be **built for T with a real cross
toolchain** (or fetched as a prebuilt release artifact for T). AScript cannot manufacture it from
the host binary by byte-appending.

### 3.2 Recommendation: ship same-host `--native` first; `--target` is a staged follow-up

**v1 (this spec): `--native` only (same host, same triple).** Resolve the stub from
`current_exe()`. Reject `--target <triple>` with a clear, owner-noted error (per `goal.md` Gate 6:
*no silent deferrals — every gap is a documented feature or a Tier-1 error*):

```
ascript build --target <T>: cross-compilation is not yet supported (BIN v1 bundles for the
host platform only). Build on a <T> host, or in a <T> CI runner / container. See docs/…/native.
```

(`--target` is *parsed* so the error is specific; it is not silently ignored.)

**Staged follow-up (own spec/plan, recorded here, not built now): `--target` via per-target runtime
artifacts.** The viable design is to make `--target` a **fetch-and-append**, leaning on
infrastructure that already exists:

- The release pipeline publishes a **prebuilt `ascript` runtime per supported triple** (CI already
  builds release binaries; this formalizes them as bundling stubs, versioned to match the host
  `ascript`'s `ASO_FORMAT_VERSION` so payload and stub agree).
- `--target T` **fetches the stub for T into the pkg content-addressed cache** and appends the
  locally-built payload to it. The pkg cache machinery is reusable almost verbatim: a cache root
  honoring `$ASCRIPT_CACHE` (`pkg/cache.rs:26`), a content-addressed `store/<hash>/` layout
  (`pkg/cache.rs:89`), atomic staging (`pkg/cache.rs:99`). A `runtimes/<triple>/<version>/ascript`
  sibling tree (or a reserved `store/` entry) caches fetched stubs offline-reusably.
- **The payload is platform-independent** (`.aso` is LE, host-`usize`-checked on read, `aso.rs:30`),
  so the *same* compiled payload appends to any same-version stub. The only per-target artifact is
  the stub executable — exactly what the cache fetches.

This keeps the staged `--target` honest: it works only for triples we publish a runtime for, and a
triple with no published runtime is a clear error (not a broken binary). FFI/`net`/`sql`-heavy
programs simply require that the published stub for T was built with those features — a build-matrix
concern, surfaced as a documented limitation, never a silent capability drop.

### 3.3 Out of scope for BIN (noted, not built)

- **Feature-trimmed stubs** (a bundle carrying only the features the program uses) — features are
  **compile-time** Cargo flags (`Cargo.toml`); you cannot trim them by post-processing a finished
  binary. A trimmed bundle would require building a bespoke runtime per program (a `cargo build`
  with a computed feature set), which is a different, heavier mechanism. Documented as a possible
  follow-up; v1 ships the full default-feature runtime (§6).
- **Multi-module bundling** (inlining a program's `import`ed local `.as` files into one payload) —
  v1 bundles the single compiled top-level chunk that `build` produces, matching today's `build`
  semantics. A bundler that traces and inlines the local import graph is a clean additive follow-up.
- **Compression of the payload / the stub** (Deno/Bun optionally compress) — additive; v1 appends
  raw verified `.aso` (it is already compact bytecode, and skipping compression keeps the verified
  bytes byte-identical to a `build` artifact).

## 4. Security & the FUZZ gating dependency (non-negotiable)

This is the spec's defining constraint and the reason `goal.md` orders **BIN after FUZZ**.

In `ascript run app.aso`, the `.aso` reader (`aso.rs` `from_bytes`) and the verifier (`verify.rs`)
already parse *untrusted* bytes — but the blast radius is one developer's machine running a file
they chose. **In a bundled binary, that same reader runs inside an executable that end users
download and run**, and the payload sits in a file an attacker can edit. The `.aso` deserializer +
verifier therefore become a **distribution-grade attack surface**: a malformed/adversarial payload
must be *rejected*, with **no out-of-bounds read, no integer-overflow UB, no panic-to-abort, no way
to smuggle unverified bytecode into the run loop.**

Therefore:

- **The standalone P0 reader allocation-clamp bugfix is a hard pre-req.** FUZZ owns an explicit,
  standalone-P0 deliverable: clamp **every** attacker-controlled-length allocation in the `.aso` reader
  (`reserve(n)`/`with_capacity(n)` in `aso.rs` `read_chunk`/`read_proto`/`read_value`/`read_type`) with
  `.min(r.remaining())`, the pattern the worker serializer already uses (`serialize.rs:564`,
  `with_capacity(len.min(r.remaining()))`). TODAY a crafted `.aso` with a huge `u32` length forces a
  multi-GB allocation and a SIGABRT(OOM) **before** `verify` ever runs — i.e. the trust boundary can be
  crashed pre-verification. BIN cannot ship over that: an end-user-downloaded binary whose embedded
  bytes were tampered must *reject*, never `abort()`. **This fix landing (TDD'd with a crafted
  huge-length `.aso` that yields a clean `AsoError`, not an abort) is a BIN lock pre-req, distinct from
  the fuzz-campaign gate below.**
- **BIN hard-depends on FUZZ having fuzzed the `.aso` deserializer (`aso.rs`) and the verifier
  (`verify.rs`).** `goal.md` already states this twice (FUZZ "`.aso` deserializer + verifier
  fuzzing (bug *and* security surface; gates BIN)"; BIN "Must land after FUZZ hardens the `.aso`
  reader"). This spec makes it a **lock condition**: BIN's plan may not be marked implementable
  until the FUZZ `.aso`/verifier fuzz target has **run a sustained nightly campaign clean** (FUZZ's
  own gating bar — `2026-06-08-fuzzing-infra-design.md:10`, `:238`–`:239`), not merely "exists + one
  green per-PR CI run." A single green CI run is a smoke re-run of known-interesting inputs; the
  *nightly* deep run is the real campaign and is what gates BIN, so an implementer cannot satisfy the
  lock with one green CI invocation. The reader/verifier must be fuzzed for: truncation at every field
  (the `Truncated`/`Overflow` paths, `aso.rs:115`), every bad tag (`BadConst`/`BadTag`, `aso.rs:117`),
  `u64→usize` narrowing on small hosts (`aso.rs:30`), the now-clamped unbounded-allocation vector
  (above), and the full verifier invariant set (`verify.rs:1` items 1–5, especially the stack-depth
  abstract interpretation — the one that prevents `fiber.pop()` UB).
- **The verifier always runs on the embedded chunk — verified once at startup.** No
  `--trust`/`--no-verify` flag in v1 (§11 rejects it). `from_bytes_verified` (`verify.rs:782`) is the
  *only* entry the embedded run uses at startup (§2.4); worker slices are decode-only re-parses of those
  already-verified bytes (§2.4), so no unverified-from-untrusted bytes reach the VM.
- **Defence in depth, not a substitute for verification:** the footer's `aso_version` gives a
  clearer pre-slice error, and the bounds-checked footer prevents an OOB slice — but the
  authoritative guarantee is the fuzz-hardened verifier. We do **not** add a payload checksum/HMAC
  as a security control (it would only detect *accidental* corruption; a real attacker re-computes
  it). Integrity against a *malicious* edit is exactly what the verifier provides: any tampered
  chunk that survives slicing is caught by `verify`, or it is a *valid* chunk (in which case it is
  no more dangerous than any program the user could have compiled themselves — bytecode the verifier
  accepts is, by the no-bugs pillar, safe for the VM to run). Code-signing for *authenticity* (this
  binary is from vendor X) is an OS-platform concern (§10), orthogonal to verification.

## 5. Implementation surface & cross-cutting checklist

Per `CLAUDE.md`. **No grammar / parser / tree-sitter / formatter / type-system / `.aso`-format
work** — BIN adds no surface syntax and does not change the `.aso` format. The surface is CLI +
startup + a small footer codec + docs/tests.

**CLI (`src/main.rs`):**
- Add `--native` (bool) and `--target <triple>` (`Option<String>`) to the `Build` subcommand
  (`main.rs:36`). Route `Build` to `build_native(...)` when `--native` is set, else the existing
  `build_file` (`main.rs:283`). `--target` without `--native` is a usage error.
- **Add the pre-clap startup footer check** at the top of `real_main()` (`main.rs:210`): if
  `current_exe()` has the bundle footer, run the embedded chunk via the new `lib.rs` entry and
  return its exit code **before** `Cli::parse()` ever runs (so argv is the program's, not ascript's).
- Map `build_native` errors (verify failure, `--target` unsupported, write/IO) to a clean message +
  non-zero exit, like `build_file` (`main.rs:290`).

**Library (`src/lib.rs`):**
- `build_native(file, out, target) -> Result<PathBuf, AsError>`: reuse `build_file`'s
  compile+verify+`to_bytes` front half (factor the shared part), resolve the stub
  (`current_exe()` for host; `--target` → the v1 unsupported error), append `payload || footer`,
  write the executable (+exec bit), and **on macOS apply the ad-hoc `codesign -s -` seal** (§10/§2.2,
  step 5) so the arm64 output is runnable. A `build_native` error includes the specific `--target`
  rejection (Gate 10), verify failure, write/IO, and (macOS) a failed ad-hoc sign.
- `run_embedded_aso(payload: &[u8], args) -> Result<i32, AsError>`: the §2.4 body — factor it out of
  `run_aso_file` (`lib.rs:389`) so the standalone-`.aso` path and the embedded path share ONE
  implementation (verified load, `set_worker_aso_bytes`, run, exit-code mapping). `run_aso_file`
  becomes "read file → `run_embedded_aso(bytes)`."
- A tiny footer codec (read/write the fixed struct, the magic check, the bounds check) — its own
  small module (`src/bundle.rs`) or a section of `lib.rs`; pure byte I/O, unit-tested in isolation.

**Reuse, unchanged:** `aso.rs` `to_bytes` (`aso.rs:437`) / `from_bytes` (`aso.rs:453`, the raw reader),
`Chunk::from_bytes_verified` (`verify.rs:782`, the composed reader+verifier), `verify::verify`
(`verify.rs`), the `Vm`/`Interp`/`LocalSet` run setup (`lib.rs:399`–`:443`), the worker subsystem
(`src/worker/`, §7), `ASO_FORMAT_VERSION` (`aso.rs:105`, read-and-compared on load, **not bumped**).

**Worker subsystem (`src/worker/`):** *no code change expected* — §7 is a confirmation that the
existing `set_worker_aso_bytes` + in-thread `bootstrap` path already works in bundle mode. Add a
worker-in-bundle integration test (§8) to lock it.

**Docs:**
- A "Native binaries" section in the language guide (`docs/content/language/modules-async.md` or a
  new page; **if a new page, add its slug to the `NAV` array in `docs/assets/app.js`** — the
  sidebar + cmd-K search derive from `NAV`, an unlisted page is unreachable): `ascript build
  --native`, what self-contained means, the size note (§6), the host-only `--target` limitation, and
  the explicit "this is bundling, not AOT — same VM" framing.
- `README.md`: add `build --native` to the CLI table; note the single-binary deliverable.
- `CLAUDE.md`: a line under the `build → .aso; run .aso` pipeline note that `build --native`
  appends the verified `.aso` to the runtime with a trailing footer, and that startup detects it.
- `roadmap.md`: BIN status.

**Unchanged:** the grammar, both parsers, tree-sitter + editor pins, the formatter, the type
systems, the LSP, the REPL, the `.aso` format + version, the GC, the determinism seams, all stdlib.

## 6. Binary size (documented, not hidden)

A bundled binary is **the entire `ascript` runtime + the payload** — with the default feature set
(net/sql/crypto/compress/postgres/redis/…) that is **tens of MB** (the `ascript` release binary's
size), the same order as a `deno compile` (~80–100 MB) or `bun build --compile` output. The `.aso`
payload itself is small (compact bytecode); the size is the runtime, not the program.

- **This is documented in the `--native` docs and the build output** (`bundled app.as -> app
  (NN MB)`), so the size is never a surprise.
- **Feature-trimming would shrink it but is a follow-up** (§3.3): features are compile-time, so a
  trimmed bundle means building a bespoke runtime, not post-processing. v1 ships the full runtime;
  this is an honest, recorded trade (a fat-but-zero-config binary), matching Deno/Bun's default.

## 7. Workers / multi-isolate in an embedded binary

**Confirmed: a bundled binary spawns worker isolates with no special handling.** Worker isolates do
**not** re-exec the binary or parse argv — they construct a **fresh `Interp`/`Vm` in-process on a
new OS thread** via the shared `bootstrap` (`src/worker/isolate.rs:190`: "build a `current_thread`
tokio runtime + `LocalSet` + a fresh shared-NOTHING `Interp`/`Vm`… constructed INSIDE the thread").
The pool (`src/worker/pool.rs`) is lazy/demand-grown and entirely in-process. So:

- **No argv dependence.** Because isolates never look at `argv[0]` or re-launch the executable, the
  bundle's "argv is the program's args" rule (§2.3) does not interfere with worker spawning at all.
- **Bytecode for the worker slice comes from `worker_aso_bytes`, which the embedded run sets** to
  the payload (§2.4) — the *exact same* mechanism `run_aso_file` uses for a standalone `.aso`
  (`lib.rs:403`; `interp.rs:558` `worker_aso_bytes`, consumed by `build_code_slice` when there is no
  source). The embedded run is, for worker purposes, indistinguishable from a `.aso` run: the
  isolate gets the slice bytes, re-parses the top-level chunk, and builds the worker code slice. The
  `worker_source` path (`interp.rs:838`, used by `run file.as`) is **not** taken in bundle mode
  (there is no source) — and that is correct, because the `.aso` path is already the proven one.
- **`ASCRIPT_WORKERS`** still controls the pool cap in a bundled binary (it is a runtime env read,
  not CLI parsing — §2.3).

This is locked by an integration test: a bundled program that does a `worker fn` parallel
`map`+`gather` must produce the same output as `ascript run` of the same source (§8). The worker
subsystem requires **no code change** for BIN.

## 8. Testing

All tests spawn the built binary (`env!("CARGO_BIN_EXE_ascript")`), matching the `tests/` posture;
a new `tests/native.rs`.

**Footer codec (unit, in `src/bundle.rs`):** round-trip write→read of the footer (offset/len/
versions/magic); a non-bundle byte blob → `None`; truncated/short file → `None`; an out-of-bounds
offset/len → rejected (no panic, no OOB slice).

**End-to-end equivalence (the headline test):**
- `ascript build --native examples/<x>.as -o <tmp>/app`; run `<tmp>/app` **with a scrubbed `PATH`
  that contains no `ascript`** (and from a CWD with no `.as`/`.aso`), assert **stdout, STDERR, and exit
  code all == `ascript run examples/<x>.as`**. Asserting **stderr is load-bearing**, not optional:
  `std/log` writes to stderr (Live) and panic/`recover`-miss diagnostics go to stderr too, so a
  stdout+exit-only assertion would silently miss a divergence in logging or error output. Pick at least
  one example that emits on stderr (a `log.info`/`log.error` line, or a deliberate Tier-2 panic) so the
  stderr channel is actually exercised, not vacuously equal. This proves "self-contained, no install
  needed" and the packaging-not-semantics invariant. Run for a few representative examples (a plain
  program, one reading `env.args()`, a stderr-emitting program, and a worker program).
  - *Scoping note:* the comparison is over the program's own stdout/stderr/exit. It does NOT assert on
    `ascript`'s own CLI chrome (e.g. the `bundled … -> …` build line), which the bundled binary never
    prints — only the *embedded program's* output is compared, on all three channels.
- Argv forwarding: `./app a b --c` → the embedded program sees `["a","b","--c"]` in `env.args()`.

**Verifier-rejects-tampered-embedded-chunk (the security test):** build a `--native` binary, then
**flip a byte inside the embedded `.aso` region** (locate it via the footer offset) and run the
binary; assert it **exits non-zero with a clean load/verify error** (not a panic/abort/SIGSEGV, not
silent execution). A second variant corrupts the footer's offset (points past EOF) and asserts the
clean "not a valid bundle"/bounds error. This is the test that the FUZZ-hardened verifier is the
real gate.

**`--target` parsed-but-rejected (Gate-10 unit test):** `ascript build --native --target
x86_64-unknown-linux-gnu app.as` must exit non-zero with the **specific** "cross-compilation is not yet
supported (BIN v1 …)" message (§3.2), NOT a generic clap error and NOT a silent ignore. This pins the
"every gap is a documented Tier-1 error, never a silent deferral" rule (`goal.md` Gate 6 / Gate 10) at
the unit level: assert the error string mentions the requested triple and points at the host-only
limitation. A companion check: `--target` *without* `--native` is a usage error (§5).

**Startup-overhead benchmark (Gate 12, §2.3):** record the non-bundle-path startup delta — the
footer-detect cost on a plain `ascript` — via `hyperfine 'ascript --version'` against a baseline `main`
with the check removed, ≥1000 runs on the CI reference host, in both feature configs. Asserted against
the recorded budget (≤ 1 ms / ≤ 2 %, §2.3). This is the "negligible needs a number" deliverable.

**Worker-in-bundle (§7):** a `--native` binary whose program does `worker fn` parallel map+gather →
output equals `ascript run` of the source; proves isolates spawn and get their slice from
`worker_aso_bytes` in embedded mode (decode-only re-parse of the already-verified payload, §2.4).

**Four-mode parity (Gate 1, free):** because the embedded run is `from_bytes_verified` → VM (the
*identical* path as `run file.aso`), the bundled program's output is, by construction, the `.aso`
mode of the existing differential harness — no new differential entry is required, but the e2e
equivalence test above pins it end-to-end. (`vm_differential.rs` is unchanged; BIN runs no new
engine path.)

**CI smoke test:** a tiny `examples/`-backed `build --native` + run on each CI OS the release matrix
covers, asserting a self-contained binary runs there. (When the staged `--target` lands, this
extends to cross-built targets.)

## 9. Determinism

**None new.** A bundled binary runs the *same* VM over the *same* verified bytecode as `ascript run
app.aso`; the SP9 determinism seams (`src/det.rs`), the per-`Interp` Record/Replay context, and the
durable-workflow log are all unchanged and equally available. Worker isolates each keep their own
per-`Interp` determinism context exactly as today (Workers Spec A §9). There is **no new source of
nondeterminism** — the only thing BIN changes is where the bytes come from (sliced out of the
executable vs read from a `.aso` file).

## 10. Risks & honest limitations (recorded)

- **macOS arm64 requires an ad-hoc signature to EXECUTE — `ascript` must apply it (REQUIRED build
  step).** This is sharper than "Gatekeeper warning." On Apple Silicon (arm64), the kernel **refuses to
  exec a Mach-O with no code signature at all** — an unsigned arm64 binary is `SIGKILL`ed at launch
  (`kill -9` / "killed: 9"), not merely warned about. Appending our payload bytes **invalidates the
  `current_exe()` stub's existing signature**, so the produced bundle is, post-append, effectively
  unsigned and would be unrunnable on the very host that built it. Therefore `build_native` **must
  re-apply an ad-hoc signature to the output on macOS** as a documented, non-optional post-write step:
  the equivalent of **`codesign -s - <out>`** (ad-hoc / "sign with identity `-`"), invoked after the
  footer is appended (shelling out to `codesign`, or the `apple-codesign`/`rcodesign` crate to avoid a
  hard tool dependency — implementation choice noted in the plan). This is the minimum to make the
  bundle *runnable*; it is NOT authenticity. (x86_64 macOS and Linux/Windows do not require a signature
  to execute — there, an unsigned bundle still runs; the ad-hoc step is macOS-arm64-mandatory, harmless
  elsewhere.) **An e2e test must build a `--native` binary and execute it on the macOS-arm64 CI runner**,
  catching a regression where the ad-hoc sign is dropped (without it, the e2e equivalence test on that
  runner fails with "killed: 9").
- **OS authenticity-signing / notarization (distributor's job, separate from the above).** The ad-hoc
  signature above makes the binary *run*; it does **not** make it *trusted*. On macOS an ad-hoc / not
  notarized binary still triggers **Gatekeeper** on download (the quarantine prompt); Windows SmartScreen
  flags unsigned PEs. This is the same constraint Deno/Bun document. v1 produces an **ad-hoc-signed (so
  it runs) but not authenticity-signed/notarized** binary, and documents that distributors must
  sign-with-a-real-identity / notarize the *final* bundle themselves (a post-build step, outside
  `ascript`). We deliberately do **not** edit signing sections beyond the ad-hoc seal, nor apply a real
  Developer-ID identity; authenticity signing is the distributor's job. (This is why §4 separates
  *verification* — our job, always on — from *authenticity/signing* — the OS/distributor's job; the
  ad-hoc exec-signature is a third, mechanical thing: runnability, not trust.)
- **Antivirus heuristics** sometimes flag "executable with appended data" (the self-extractor
  pattern). Documented; a real, shared limitation of the technique.
- **`current_exe()` reliability.** The startup path and the host stub both rely on
  `std::env::current_exe()`. It is reliable on the supported desktop OSes but can fail if the binary
  is unlinked mid-run; we treat a failure as "no bundle, normal CLI" at startup (§2.3) and a clear
  error at build time.
- **Cross-compilation is genuinely limited** by native deps (§3) — the headline honesty of this
  spec. Same-host first; `--target` staged behind published per-target runtimes; FFI/net/sql-heavy
  programs constrain which targets are buildable. None of this is hidden: unsupported `--target` is
  a Tier-1 error, not a broken binary.
- **The FUZZ gate is a real schedule dependency** (§4): BIN cannot lock/ship until (a) the standalone
  P0 reader allocation-clamp bugfix has landed AND (b) the `.aso`/verifier fuzz target has **run a
  sustained nightly campaign clean** (not just one green per-PR CI run — FUZZ's own bar). This is
  intended — it is the price of running the deserializer inside shipped binaries.

## 11. Scope & rejected alternatives

**In scope (v1):** `ascript build --native` (same-host); the trailing-footer bundle format; the macOS
**ad-hoc exec-signature** (`codesign -s -`, required so arm64 bundles run — §10); startup footer
detection + embedded run via the verified loader; argv forwarding; worker-in-bundle support
(confirmation, no new code); docs; the e2e/security/worker/`--target`-rejection/startup-benchmark tests.
`--target` is *parsed and clearly rejected* (staged follow-up).

**Out of scope / staged (documented, owner-noted — `goal.md` Gate 6):** `--target` cross-compilation
(needs per-target runtime artifacts via the pkg cache, §3.2); feature-trimmed stubs (compile-time,
§3.3); multi-module/import-graph bundling (§3.3); payload compression (§3.3); OS **authenticity**
code-signing / notarization (§10, distributor's job — distinct from the in-scope ad-hoc *exec* seal).

**Rejected:**
- **AOT native machine-code compilation of the AScript program.** That is the deferred **JIT** spec
  (`goal.md`), and a far larger effort. BIN is explicitly *bundling* — the embedded VM still
  interprets. Conflating the two would block a shippable deployment win on the hardest perf work.
- **Changing the `.aso` format / bumping `ASO_FORMAT_VERSION`.** Unnecessary and undesirable: the
  verified `.aso` is embedded **verbatim**. Bundling adds a *wrapper* (the footer), not a new
  bytecode layout. Keeping the payload byte-identical to a `build` artifact is what makes the
  four-mode parity gate and the "embedded == standalone `.aso`" equivalence free (§2.4, §8). A
  separate footer version (`bundle_version`, §2.1) covers future bundle-wrapper evolution without
  touching `.aso`.
- **Trusting the embedded chunk without verification (a `--trust`/`--no-verify` fast path).**
  Rejected outright: the embedded reader runs in shipped binaries over attacker-editable bytes
  (§4); skipping `verify` would turn a tampered payload into VM UB. The verifier always runs. (The
  startup cost of `verify` is a one-time per-launch pass over the program's own bytecode — paid once
  at process start, negligible against a real program's runtime, and exactly the CPython-`.pyc`
  "trusted-but-verified" posture `verify.rs:1` already documents.)
- **Embedding the payload as a real ELF/Mach-O/PE section** (instead of appended trailing bytes).
  Platform-specific, fragile across the three object formats, and interacts badly with signing per
  OS. Appending past the last segment (§2.1) is the portable, format-agnostic technique every peer
  runtime uses.
- **A separate `compile`/`bundle` subcommand.** `--native` on `build` keeps one verb ("produce a
  deployable artifact from this source"), the way `build` already produces a `.aso`. A new verb adds
  CLI surface for no benefit.
- **Re-exec for worker isolates in bundle mode.** Unnecessary and wrong: isolates build a fresh
  in-process `Interp`/`Vm` and read their bytecode from `worker_aso_bytes`, never from argv or a
  re-launch (§7). Re-exec would reintroduce argv-coupling the design specifically avoids.

## 12. Grounding (verified sources & code)

- **Self-extracting / appended-data executable technique** (append payload past the last loaded
  segment; the OS loader ignores trailing bytes; read your own file + a trailing footer to locate
  it): the long-standing self-extracting-archive / installer-stub pattern; used by SFX archives and
  Go's `embed`-adjacent tooling. The footer-at-EOF + sentinel is the standard "find my appended
  data" idiom.
- **Deno `deno compile`** — bundles the script + a copy of the Deno runtime into a standalone
  executable that still runs on the embedded V8 interpreter/JIT (not AOT-compiling the JS to native
  the way a C compiler would); produces tens-of-MB binaries; the compiled binary takes the script's
  args. Directly analogous to BIN's bundling-not-AOT framing and argv forwarding (§1, §2.3).
- **Bun `bun build --compile`** — same model: appends the bundled JS/TS + the Bun runtime; large
  binaries; documents code-signing as a post-step. Confirms §6 (size), §10 (signing).
- **Node.js Single Executable Applications (SEA)** & **PyInstaller** — the same "runtime +
  embedded program in one file, detect-and-run at startup" pattern from the Node/Python ecosystems.
- **CPython `.pyc` "trusted-but-verified" posture** — the model AScript's verifier already cites
  (`verify.rs:1`): bytecode loaded from disk is structurally validated before execution; BIN raises
  the stakes (the bytes are now in shipped binaries) and so hard-gates on FUZZ (§4).
- **macOS arm64 requires a code signature to execute.** Apple Silicon enforces mandatory code signing
  for *all* arm64 Mach-O executables (introduced with the arm64 transition): an unsigned arm64 binary is
  killed by the kernel at launch (`SIGKILL`), not merely Gatekeeper-warned. An **ad-hoc** signature
  (`codesign --sign -`, identity `-`) satisfies the *runnability* requirement without a Developer-ID
  identity or notarization (those are for trust/distribution). This is why §10 splits the ad-hoc
  exec-seal (BIN's build step) from authenticity signing (distributor's job).
- **Code (this repo):** `src/main.rs:36` (`Build` subcommand), `:210`/`:229` (`real_main`, the
  `.aso`-always-VM rule), `:139` (the `WORKER_STACK_SIZE` worker-thread main); `src/lib.rs:356`
  (`build_file` compile+verify+`to_bytes`), `:389` (`run_aso_file`, the verified-load run path),
  `:396` (calls `from_bytes_verified`), `:403` (`set_worker_aso_bytes`); `src/vm/aso.rs:50`
  (`ASO_MAGIC`), `:105` (`ASO_FORMAT_VERSION`, **not bumped**), `:456` (magic check), `:459`–`:460`
  (`from_bytes` reads the version and compares it to the build constant — read, not hardcoded);
  `src/vm/verify.rs:782` (`Chunk::from_bytes_verified` = `from_bytes` + `verify`, returns
  `FromBytesVerifiedError`), `verify.rs:1` (the verifier's five structural invariants);
  `src/worker/dispatch.rs:1309` (`resolve_worker_top_chunk`) / `:1319` (worker re-parse via the
  **unverified** `Chunk::from_bytes` over already-verified `worker_aso_bytes` — the R1 precision);
  `src/worker/isolate.rs:190` (in-thread fresh-`Vm` `bootstrap` — workers don't re-exec),
  `src/interp.rs:558` (`worker_aso_bytes`, the no-source worker-bytecode source) / `:838`
  (`set_worker_source`, the source path NOT taken in bundle mode); `src/worker/serialize.rs:564`
  (`with_capacity(len.min(r.remaining()))` — the allocation-clamp pattern FUZZ's P0 fix applies to the
  `.aso` reader); `src/pkg/cache.rs:26`/`:89`/`:99` (the content-addressed cache reusable for staged
  `--target` runtime artifacts).
</content>
</invoke>
