//! The clap CLI surface — the single source of truth for AScript's CLI.
//!
//! This module holds the derive types (`Cli`, `Command`, `CapFlags`) verbatim from
//! what used to live in `src/main.rs`, plus a `cli_command()` introspection seam.
//! Two consumers:
//!
//! 1. `src/main.rs` — imports these for actual CLI parsing (`Cli::parse()`).
//! 2. `tests/docs_drift.rs` — calls `cli_command()` to introspect the live clap
//!    tree and assert that `docs/content/cli.md` documents every subcommand and
//!    long flag (spec §4.1, tripwire 1).
//!
//! The types are `pub` so they cross the crate boundary into the integration test.
//! `cli_command()` is unconditional: clap is an unconditional dependency
//! (`Cargo.toml`), and the function is cheap (just `CommandFactory::command()`).

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ascript", about = "The AScript interpreter")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run a .as program (bytecode VM) or a compiled .aso program (VM)
    Run {
        /// Run a `.as` file on the legacy tree-walker engine instead of the
        /// bytecode VM (the differential oracle / debugging escape hatch). Must
        /// precede the file. Equivalent to `ASCRIPT_ENGINE=tree-walker`; the flag
        /// takes precedence over the env var. Ignored for `.aso` (always VM).
        #[arg(long = "tree-walker")]
        tree_walker: bool,
        /// ELIDE §5: enable contract elision — drop statically-PROVEN runtime
        /// type-contract checks (call-arg / annotated-let / declared-return) from the
        /// executed bytecode/AST. Default-OFF on `run` (the per-run collector cost is
        /// over the §5.1 startup budget; see `bench/ELIDE_RESULTS.md`). Behavior is
        /// byte-identical either way — elision is invisible. Equivalent to
        /// `ASCRIPT_ELIDE=1`. `ascript build` is the cost-free elide surface.
        #[arg(long = "elide", conflicts_with = "no_elide")]
        elide: bool,
        /// ELIDE §5.2: force contract elision OFF (the permanent kill switch; wins
        /// over `--elide`). Equivalent to `ASCRIPT_NO_ELIDE=1`. Redundant while the
        /// measured default is already off, but kept stable for when the default flips.
        #[arg(long = "no-elide")]
        no_elide: bool,
        /// Offline-deterministic: resolve dependencies EXACTLY from `ascript.lock`
        /// (no network), failing on any drift, missing lock, or integrity
        /// mismatch. For CI / sandboxes.
        #[arg(long = "locked")]
        locked: bool,
        #[command(flatten)]
        caps: CapFlags,
        /// DBG: run under the debugger — start a Debug Adapter Protocol (DAP) server
        /// over stdio for this program instead of running it normally. An editor's
        /// DAP client drives breakpoints/stepping/inspection. Requires the `dap`
        /// feature (default-on); a `--no-default-features` build reports a rebuild
        /// hint. The program stops at entry; output rides DAP `output` events.
        #[arg(long = "inspect")]
        inspect: bool,
        /// DBG: run under the CPU sampling profiler, writing a profile artifact on
        /// exit. v1 accepts only `cpu` (the arg is reserved for future modes). The
        /// program runs to completion normally — profiling is observation-only, so its
        /// stdout is byte-identical to a plain run. Requires the `profile` feature
        /// (default-on); a `--no-default-features` build reports a rebuild hint.
        #[arg(long = "profile", value_name = "MODE")]
        profile: Option<String>,
        /// DBG: the profile output path (default `profile.json`, or `profile.txt` for
        /// the collapsed format). Only meaningful with `--profile`.
        #[arg(long = "out", short = 'o', value_name = "FILE")]
        out: Option<String>,
        /// DBG: the profiler's wall-clock sample rate in samples/second (default 1000,
        /// i.e. ~1ms). Only meaningful with `--profile`.
        #[arg(long = "profile-hz", value_name = "N")]
        profile_hz: Option<u32>,
        /// DBG: the profile artifact format — `speedscope` (default, opens at
        /// speedscope.app) or `collapsed` (Brendan-Gregg folded stacks). Only
        /// meaningful with `--profile`. The special value `deterministic-speedscope` /
        /// `deterministic-collapsed` selects the deterministic (call-structure-driven,
        /// golden-stable) sample clock — used by goldens/tests, no wall-clock thread.
        #[arg(long = "profile-format", value_name = "FMT")]
        profile_format: Option<String>,
        /// WARM A: bypass the content-addressed compile cache for this run (always
        /// parse/resolve/compile from source). Equivalent to
        /// `ASCRIPT_NO_COMPILE_CACHE=1`. The cache only ever applies to the plain
        /// `.as`-on-the-VM path; `.aso`/`--tree-walker`/`--inspect`/`--profile` are
        /// never cached regardless of this flag.
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// REPLAY §4.1: record this run's non-deterministic effects (clock/RNG/uuid/fs/
        /// env/process.run/DNS/buffered-http/workflow.run) to <FILE> as a replayable
        /// trace. The run executes in deterministic mode (virtual clock, instant sleeps,
        /// seeded RNG); the trace is written even if the program panics or exits non-zero.
        /// Bypasses the compile cache. Composes with `--tree-walker` and `.aso`.
        #[arg(long = "record", value_name = "FILE", conflicts_with = "replay")]
        record: Option<String>,
        /// REPLAY §4.1: replay a previously recorded <FILE>, reproducing the run's exact
        /// effects with NO real I/O (strict divergence detection). Pass the same program
        /// file; a source change since recording is a clean error. Bypasses the cache.
        #[arg(long = "replay", value_name = "FILE")]
        replay: Option<String>,
        /// REPLAY §4.1: pin the RNG seed for `--record` (default: OS entropy). The same
        /// program + seed records an identical event stream.
        #[arg(long = "seed", value_name = "N", requires = "record")]
        seed: Option<u64>,
        file: String,
        /// Trailing arguments forwarded to the script as `env.args()`.
        /// Hyphen-prefixed values (e.g. `--flag`) are also captured.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Compile a .as program to bytecode (.aso)
    Build {
        file: String,
        /// Output path (defaults to `<file-stem>.aso`, or the bare `<file-stem>`
        /// executable with `--native`).
        #[arg(long, short)]
        out: Option<String>,
        /// ELIDE §5: enable contract elision in the compiled artifact — drop proven
        /// runtime type-contract checks from the `.aso`/native bytecode. The win is
        /// DURABLE (the `CallElided` opcode serializes; every later `run` of the
        /// artifact keeps it) and the one-shot collector cost is amortised, so
        /// `build --elide` is the recommended elide surface. Default-OFF (same as
        /// `run`, §5.1). Behavior is byte-identical. Equivalent to `ASCRIPT_ELIDE=1`.
        #[arg(long = "elide", conflicts_with = "no_elide")]
        elide: bool,
        /// ELIDE §5.2: force contract elision OFF (kill switch; wins over `--elide`).
        /// Equivalent to `ASCRIPT_NO_ELIDE=1`.
        #[arg(long = "no-elide")]
        no_elide: bool,
        /// Strip the optional DBG debug section (module source + per-proto
        /// line/variable info). Default: debug info is INCLUDED.
        #[arg(long = "strip")]
        strip: bool,
        /// BIN — produce a self-contained NATIVE executable (the whole runtime +
        /// the compiled program) instead of a `.aso`. Bundling, not AOT: the
        /// embedded VM still interprets. Host-only in v1.
        #[arg(long = "native")]
        native: bool,
        /// RT §6: target triple for a `--native` cross build (one of the 8 published
        /// triples — an unknown triple is rejected with the supported set). A platform-
        /// independent payload is appended onto a per-target stub resolved via the §5.4
        /// ladder; a cross target needs `--stub` or a fetched stub (no local fallback).
        /// `--target <host>` is equivalent to omitting it. Requires `--native` (or `--oci`,
        /// which implies `--native`).
        #[arg(long = "target", requires = "native")]
        target: Option<String>,
        /// RT §5.4 rung 1: an explicit local `ascript-rt` stub to append the payload onto
        /// (tests, air-gap, custom builds). Footer-checked (a pre-existing overlay is
        /// stripped) and feature-verified via `--rt-info` when host-executable. Requires
        /// `--native`.
        #[arg(long = "stub", requires = "native")]
        stub: Option<String>,
        /// RT §5.4 rung 3: skip the network fetch rung entirely (availability fall-through
        /// to the dev sibling / current_exe). Equivalent to `ASCRIPT_RT_NO_FETCH=1`.
        /// Requires `--native`.
        #[arg(long = "no-fetch", requires = "native")]
        no_fetch: bool,
        /// RT §7: zstd-compress the embedded payload of a `--native` bundle (smaller
        /// artifact; the stub decompresses it at startup). Requires `--native`. The
        /// footer is marked version 2 / `FLAG_ZSTD`; an uncompressed bundle stays
        /// bit-identical to a pre-RT build.
        #[arg(long = "compress", requires = "native")]
        compress: bool,
        /// RT §4.5: build the stub with EXACTLY the features the program requires via a
        /// local `cargo build` of `ascript-rt` (no tier slack). Requires `$ASCRIPT_SRC`
        /// to be set to a source checkout matching this toolchain's version; `cargo` must
        /// be on PATH. The result is content-addressed and cached — a second build with
        /// the same feature set skips the cargo invocation. `--exact --target
        /// *-apple-darwin` requires a macOS host. Mutually exclusive with `--tier` and
        /// `--stub`. Requires `--native`.
        #[arg(long = "exact", requires = "native", conflicts_with_all = ["tier", "stub"])]
        exact: bool,
        /// RT §4.4: force the stub tier (`rt-core`/`rt-local`/`rt-net`/`rt-full`) for a
        /// `--native` bundle instead of automatic nearest-superset selection. A tier
        /// below the program's requirements is rejected (the error lists the missing
        /// features and the modules that demand them). Requires `--native`.
        #[arg(long = "tier", requires = "native")]
        tier: Option<String>,
        /// RT §9.2: emit the canonical JSON build report for a `--native` bundle to
        /// `<PATH>` (or `-` for stdout) — the CI/reproducibility hook. The human report
        /// always prints to stderr regardless. Requires `--native`.
        #[arg(long = "report-json", requires = "native")]
        report_json: Option<String>,
        /// RT §8: produce an OCI Image Layout tarball loadable by `docker load`/`podman
        /// load` WITHOUT Docker at build time. Implies `--native`. The image has no base
        /// layers (scratch semantics), so the binary must be statically linked: requires
        /// a `*-unknown-linux-musl` target. With no `--target`, defaults to
        /// `<host-arch>-unknown-linux-musl`. A gnu/darwin/windows triple is rejected with
        /// an error naming the musl equivalent. Composes with `--compress`, `--stub`,
        /// `--target`, and `--tier`. Requires the `compress` Cargo feature (default-on).
        #[arg(long = "oci")]
        oci: bool,
        /// RT §8: the image reference tag written as the
        /// `org.opencontainers.image.ref.name` annotation in the OCI `index.json` (used
        /// by `docker load`/`podman load` to name the image). Defaults to
        /// `<file-stem>:latest`. Requires `--oci`.
        #[arg(long = "oci-tag", requires = "oci")]
        oci_tag: Option<String>,
        /// WARM B §3.1: run the program as a training workload, harvest the warmed
        /// inline caches and adaptive arithmetic state, and embed a PGO (profile-
        /// guided optimisation) section into the produced archive. The artifact is
        /// always an `ASCRIPTA` archive (even for a single-module program). The
        /// training run's stdout streams live. A panicking training run still
        /// produces a (possibly partial) PGO section — the build does not abort.
        #[arg(long = "pgo")]
        pgo: bool,
        #[command(flatten)]
        caps: CapFlags,
    },
    /// Start the interactive REPL
    Repl {
        /// Run the REPL on the legacy tree-walker engine instead of the bytecode
        /// VM (the differential oracle / debugging escape hatch). Equivalent to
        /// `ASCRIPT_ENGINE=tree-walker`; the flag takes precedence over the env
        /// var. Default → the bytecode VM (the production path post-cutover).
        #[arg(long = "tree-walker")]
        tree_walker: bool,
    },
    /// Format .as source files
    Fmt { files: Vec<String> },
    /// Statically check .as files (syntax + lints)
    Check {
        files: Vec<String>,
        /// Emit machine-readable JSON instead of human output.
        #[arg(long)]
        json: bool,
        /// Treat all warnings as errors (non-zero exit on any warning).
        #[arg(long)]
        deny_warnings: bool,
        /// Promote a lint rule to error severity (repeatable). E.g.
        /// `--deny unused-binding`. `syntax-error` is always an error already.
        #[arg(long = "deny", value_name = "RULE")]
        deny: Vec<String>,
        /// Force a lint rule to warning severity (repeatable).
        #[arg(long = "warn", value_name = "RULE")]
        warn: Vec<String>,
        /// Suppress a lint rule entirely (repeatable). `--allow syntax-error` is
        /// accepted but a no-op (syntax errors are always reported).
        #[arg(long = "allow", value_name = "RULE")]
        allow: Vec<String>,
        /// Apply safe autofixes in place (currently `unused-import` removal).
        /// Re-running is idempotent. Mutually exclusive with `--fix-dry-run`.
        #[arg(long)]
        fix: bool,
        /// Show the autofixes that `--fix` would apply (a unified diff, or the
        /// planned edits under `--json`) WITHOUT writing any file. Mutually
        /// exclusive with `--fix`.
        #[arg(long = "fix-dry-run")]
        fix_dry_run: bool,
    },
    /// Generate API documentation from `///` doc-comments (DX D1).
    #[cfg(feature = "doc")]
    Doc {
        /// Files or directories to document (default: the current directory,
        /// discovered like `check`). Resolves imports to document a whole project.
        paths: Vec<String>,
        /// Output directory (default `target/doc/`). Used for `--format html`.
        #[arg(long = "out", value_name = "DIR")]
        out: Option<String>,
        /// Output format: `html` (default; a self-contained `target/doc/` tree) or
        /// `md` (Markdown to stdout / `--out`).
        #[arg(long = "format", value_name = "FORMAT", default_value = "html")]
        format: String,
        /// Include non-exported declarations (default: public API only).
        #[arg(long = "private")]
        private: bool,
        /// Open the generated `index.html` after writing (best-effort, `sys`-gated).
        #[arg(long = "open")]
        open: bool,
        /// Doc-lint: write nothing; exit non-zero if a public declaration lacks a
        /// doc-comment (a CI gate), reporting the undocumented symbols.
        #[arg(long = "check")]
        check: bool,
    },
    /// Run .as test files
    Test {
        files: Vec<String>,
        /// ELIDE §5: enable contract elision for the (serial) test run. Default-OFF
        /// (§5.1). Behavior is identical with or without this flag. Equivalent to
        /// `ASCRIPT_ELIDE=1`. (The `--parallel` path runs each file in a worker
        /// isolate, which never elides — full checks there, §4.6.)
        #[arg(long = "elide", conflicts_with = "no_elide")]
        elide: bool,
        /// ELIDE §5.2: force contract elision OFF (kill switch; wins over `--elide`).
        /// Equivalent to `ASCRIPT_NO_ELIDE=1`.
        #[arg(long = "no-elide")]
        no_elide: bool,
        /// Offline-deterministic: resolve dependencies EXACTLY from `ascript.lock`
        /// (no network), failing on drift / missing lock / integrity mismatch.
        #[arg(long = "locked")]
        locked: bool,
        /// FFI §4.2: deny one or more capabilities for the test run (opt-out).
        /// Comma-separated / repeatable. Composes with the manifest.
        #[arg(long = "deny", value_name = "CAP", value_delimiter = ',')]
        deny: Vec<String>,
        /// FFI §4.2: deny ALL five dangerous capabilities for the test run.
        #[arg(long = "sandbox")]
        sandbox: bool,
        /// DX D2: run each test FILE in its own shared-nothing worker isolate, in
        /// parallel. `--parallel` (no value) uses `num_cpus` isolates; `--parallel=N`
        /// caps at N (further clamped by `$ASCRIPT_WORKERS`). Omitted = serial (the
        /// default). The aggregated result + exit code are deterministic regardless of
        /// completion order; a single file degrades to the serial path.
        #[arg(
            long = "parallel",
            value_name = "N",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "0"
        )]
        parallel: Option<usize>,
        /// DX D2 Task 8: re-baseline ALL snapshots this run (`jest -u`-style). A changed
        /// `assert.snapshot` value OVERWRITES the stored snapshot and PASSES (no source
        /// edit), and ORPHAN snapshot files (no matching assertion this run) are DELETED.
        /// Without the flag, a changed snapshot FAILS and orphans are only reported.
        #[arg(long = "update-snapshots")]
        update_snapshots: bool,
        /// DX D2 Task 10: run only tests whose NAME matches PATTERN — a substring by
        /// default, or a regex when written `/regex/`. Prunes both which registered tests
        /// run and (with no match in a file) which files contribute. A skipped test is
        /// reported as "filtered", never pass/fail. Composes with `--parallel`
        /// deterministically. A malformed regex is a clean error.
        #[arg(long = "filter", value_name = "PATTERN")]
        filter: Option<String>,
        /// DX D2 Task 10: re-run the affected tests on a file change, scoping by the
        /// workspace import graph (only files whose import closure touched the change
        /// re-run; falls back to all files if the graph is unavailable). Runs until
        /// interrupted (Ctrl-C). Requires the `sys` feature (file watching).
        #[arg(long = "watch")]
        watch: bool,
        /// DX D2 Task 6: record line COVERAGE for the test run on the bytecode VM and
        /// report it. `--coverage` (no value) → a text report; `--coverage=lcov` → LCOV;
        /// `--coverage=html` → a self-contained `target/coverage/` tree. The program
        /// output is byte-identical to a non-coverage run (observation-only). Coverage
        /// runs on the VM only (the tree-walker is the oracle, not instrumented).
        #[arg(
            long = "coverage",
            value_name = "FORMAT",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "text"
        )]
        coverage: Option<String>,
        /// BATT C1 (§10.1): run tests DETERMINISTICALLY with a fixed RNG seed. Each test
        /// body gets a fresh, identical `math.random`/`uuid.v4`/`crypto.randomBytes`
        /// stream. Independently usable with `--frozen-time`. When omitted, RNG is the
        /// normal thread-local stream (the inert default). `--seed` alone also freezes
        /// time at the seed-derived deterministic epoch.
        #[arg(long = "seed", value_name = "U64")]
        seed: Option<u64>,
        /// BATT C1 (§10.1): freeze the virtual clock for test bodies (`time.now`/
        /// `time.monotonic`/`date.now`; `time.sleep` returns instantly). Accepts an
        /// RFC3339 timestamp (e.g. `2026-01-02T03:04:05Z`, requires the `datetime`
        /// feature) OR a raw epoch-ms integer (accepted in every build). `--frozen-time`
        /// alone implies seed 0. Only TEST BODIES are frozen — module top-level load runs
        /// on the real clock (§10.2).
        #[arg(long = "frozen-time", value_name = "RFC3339|EPOCH_MS")]
        frozen_time: Option<String>,
    },
    /// Run the language server (LSP over stdio)
    #[cfg(feature = "lsp")]
    Lsp {
        /// Accepted for compatibility with LSP clients that pass `--stdio` (e.g. some
        /// VS Code client configs). stdio is the only transport, so this is a no-op.
        #[arg(long = "stdio")]
        stdio: bool,
    },
    /// Run the Debug Adapter Protocol (DAP) server over stdio. An editor's DAP client
    /// connects and drives launch/breakpoints/stepping. The program to debug comes
    /// from the `launch` request (or use `run --inspect <file>` to pre-set it).
    #[cfg(feature = "dap")]
    Dap {
        /// Accepted for compatibility with DAP clients that pass `--stdio`. stdio is
        /// the only transport, so this is a no-op.
        #[arg(long = "stdio")]
        stdio: bool,
    },
    /// Add a dependency to ascript.toml + lock (git/url/path spec).
    #[cfg(feature = "pkg")]
    Add {
        /// e.g. `github.com/owner/repo@v1.0.0`, `https://host/pkg-1.2.0.tar.gz`,
        /// or `../local-path`.
        spec: String,
    },
    /// Remove a dependency from ascript.toml + re-lock.
    #[cfg(feature = "pkg")]
    Remove { name: String },
    /// Resolve + fetch dependencies and write/verify ascript.lock.
    #[cfg(feature = "pkg")]
    Install {
        /// Install EXACTLY from ascript.lock (no network); fail on drift.
        #[arg(long = "locked")]
        locked: bool,
    },
    /// Raise pins + re-lock (optionally a single dependency).
    #[cfg(feature = "pkg")]
    Update { name: Option<String> },
    /// (Re)generate ascript.lock from the manifest.
    #[cfg(feature = "pkg")]
    Lock,
    /// Print the resolved dependency graph.
    #[cfg(feature = "pkg")]
    Tree,
    /// Re-hash the cache store against the lock integrity (fail-closed).
    #[cfg(feature = "pkg")]
    Verify,
    /// Scaffold a new project from a template (CNTR §9.3). The `server` template is
    /// a container-ready HTTP service: graceful SIGTERM drain, a /healthz probe, a
    /// resilient upstream call, plus a multi-stage Dockerfile, .dockerignore,
    /// ascript.toml, and README. Refuses to overwrite existing files unless `--force`.
    Init {
        /// The template to scaffold (currently only `server`).
        #[arg(long = "template", value_name = "NAME", default_value = "server")]
        template: String,
        /// Overwrite existing files instead of refusing with a conflict list.
        #[arg(long = "force")]
        force: bool,
        /// Target directory (created if needed; defaults to the current directory).
        #[arg(default_value = ".")]
        dir: String,
    },
    /// Manage the compile cache
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// RT §5.1 / Task 11 — generate + sign the release stub manifest (internal release
    /// tooling; driven by `scripts/release-rt-stubs.sh`). HIDDEN: not part of the public
    /// CLI surface. Gated on the default-OFF `rt-release` feature (the ed25519 SIGNING
    /// half), so it is absent from a normal toolchain build.
    #[cfg(feature = "rt-release")]
    #[command(name = "rt-manifest-gen", hide = true)]
    RtManifestGen {
        /// Mint a fresh ed25519 keypair, print `(private seed hex, public key hex)`, and
        /// exit. The maintainer compiles the public key into `PRODUCTION_PUBKEY` and
        /// stores the private seed in the CI secret `ASCRIPT_RT_SIGNING_KEY`. No manifest
        /// is written in this mode.
        #[arg(long = "genkey")]
        genkey: bool,
        /// The toolchain version the manifest is cut for (the §5.1 version lock). Defaults
        /// to this binary's own `CARGO_PKG_VERSION`.
        #[arg(long = "version", value_name = "VER")]
        version: Option<String>,
        /// The deterministic `created` timestamp embedded in the manifest (a fixed input,
        /// never `now()` — so a rebuild is byte-identical). Defaults to the epoch.
        #[arg(long = "created", value_name = "TS")]
        created: Option<String>,
        /// Path to a JSON file holding the stub entries array (each
        /// `{target,tier,features,sha256,size,filename}`). The release script assembles
        /// this after building + hashing every stub.
        #[arg(long = "entries-file", value_name = "PATH")]
        entries_file: Option<String>,
        /// Path to a file holding the 64-hex-char private signing seed (the CI secret
        /// `ASCRIPT_RT_SIGNING_KEY`). Required unless `--genkey`.
        #[arg(long = "key", value_name = "PATH")]
        key: Option<String>,
        /// Directory to write `rt-manifest.json` + `rt-manifest.json.sig` into. Defaults
        /// to the current directory.
        #[arg(long = "out-dir", value_name = "DIR")]
        out_dir: Option<String>,
    },
}

/// Subcommands for `ascript cache`.
#[derive(Subcommand)]
pub enum CacheAction {
    /// Remove the compiled/ namespace (compile cache entries only — the pkg store/ is unaffected).
    Clean,
    /// Print the cache root directory.
    Dir,
}

/// FFI §4.2/§4.4: the shared capability CLI flags, flattened into both the `run`
/// and `build` subcommands (DRY — identical surface + help text by construction).
/// `test` deliberately exposes only `--deny`/`--sandbox` (no granular carve-outs),
/// so it keeps its own inline pair rather than flattening this.
#[derive(clap::Args, Debug)]
pub struct CapFlags {
    /// FFI §4.2: deny capabilities for this run (comma-separated / repeatable:
    /// fs, net, process, ffi, env), composed with any `ascript.toml
    /// [capabilities]` (denial is monotone — the CLI cannot re-grant). For
    /// `build`/`build --native` the composed set is additionally EMBEDDED in the
    /// produced artifact and enforced at launch (further restrictable with
    /// `ASCRIPT_DENY`); see the bundles docs.
    #[arg(long = "deny", value_name = "CAP", value_delimiter = ',')]
    pub deny: Vec<String>,
    /// FFI §4.2: deny ALL five dangerous capabilities (fs, net, process, ffi,
    /// env). Sugar for `--deny fs,net,process,ffi,env`.
    #[arg(long = "sandbox")]
    pub sandbox: bool,
    /// FFI §4.4: a granular net carve-out: `--deny-net=external` (allow
    /// loopback/private, block public) or `--deny-net=all`.
    #[arg(long = "deny-net", value_name = "MODE")]
    pub deny_net: Option<String>,
    /// FFI §4.4: a granular fs carve-out: `--deny-fs=write` (reads allowed,
    /// writes denied) or `--deny-fs=all`.
    #[arg(long = "deny-fs", value_name = "MODE")]
    pub deny_fs: Option<String>,
}

/// The full clap command tree — the single source of truth for the CLI surface.
///
/// Called by `tests/docs_drift.rs` (tripwire 1) to introspect the live subcommand
/// and long-flag set against `docs/content/cli.md`. Also usable for smoke-checking
/// the structural validity of the clap definitions via `cmd.debug_assert()`.
pub fn cli_command() -> clap::Command {
    use clap::CommandFactory;
    Cli::command()
}

#[cfg(test)]
mod tests {
    /// clap's structural self-check: catches conflicting/invalid arg definitions
    /// that only surface at parse time. Also pins the surface seam the docs-drift
    /// tripwire introspects.
    #[test]
    fn cli_command_is_structurally_valid() {
        super::cli_command().debug_assert();
    }

    /// The seam exposes the real subcommand set (feature-dependent: doc/lsp/dap/pkg
    /// variants are cfg-gated — this test asserts only the unconditional core).
    #[test]
    fn cli_command_has_the_core_subcommands() {
        let cmd = super::cli_command();
        let names: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
        for core in ["run", "build", "repl", "fmt", "check", "test"] {
            assert!(names.contains(&core), "core subcommand '{core}' missing: {names:?}");
        }
    }
}
