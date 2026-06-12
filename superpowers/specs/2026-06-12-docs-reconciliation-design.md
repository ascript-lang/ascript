# Documentation Reconciliation + Permanent Drift Tripwires — Design (DOCS)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** DOCS (a developer-experience-track spec of the PERF campaign — joins SIG under
  `goal-perf.md` §"Developer-experience track")
- **Depends on:** nothing. Static docs + test-only Rust (one behavior-identical refactor of
  `src/main.rs`'s clap definitions into a library module, §4.1). NOTHING in the engine campaign
  is a dependency.
- **Depended on by:** nothing hard. SIG and DOCS are **mutually independent** (boundary in §3);
  whichever lands first, the other still applies cleanly.
- **Sequencing:** owner-decided. The owner wants DOCS as part of the current docs-currency push;
  there is **no technical dependency on the engine waves** — executable any time.
- **Engines:** N/A — docs + tests + a CLI-definition refactor. **No engine, VM, compiler,
  `.aso`, or grammar surface.** `ASO_FORMAT_VERSION` untouched; `tests/vm_differential.rs`
  untouched (run once at the gate as the no-engine-surface proof, the SIG posture).
- **Breaking:** no. `ascript --help` output and all CLI behavior are byte-identical (§4.1).

---

## 0. Read this first

A docs-vs-reality audit (2026-06-12, internal; its findings are **inlined below, each
re-verified mechanically against the current tree with file:line on both sides** — this
document is self-contained and normative on its own) found the documentation site largely
healthy but with one big hole (`docs/content/cli.md` documents a fraction of the real CLI
surface), one scattered surface (env vars have no central reference and one is documented
nowhere), and one piece of meta-drift (`CLAUDE.md` misdescribes how the stdlib reference is
organized). Two audit claims turned out **stale on re-verification** (§1.5) — which is itself
the strongest argument for this spec's second half: point-in-time audits rot; only **in-tree
tripwires** (the house tests-as-gates pattern, e.g. `tests/srv_negative_space.rs`) keep docs
and reality bound permanently.

DOCS ships two units:

- **Unit A — the reconciliation run** (one-time fix sweep): bring `cli.md` to full CLI parity,
  add a consolidated env-var reference, close the verified stdlib-member gaps, fix the
  `CLAUDE.md` wording, re-verify the tooling pages and README.
- **Unit B — permanent drift tripwires** (the durable value): `tests/docs_drift.rs`, six
  mechanical assertions binding the CLI surface, env vars, stdlib module coverage, NAV
  reachability, and in-content links to the docs — written FIRST, observed failing on the
  known gaps, made green by Unit A (TDD at the docs level). Plus a proposed campaign gate:
  **"docs drift tripwires stay green in CI."**

This serves `goal-perf.md` pillar 2 ("docs staleness is a campaign-blocking defect") and
`goal.md` Gate 13 ("Every spec updates its docs pages (+ `NAV`, served-site sanity) … the
stdlib reference tracks every API change. Docs staleness is a campaign-blocking defect").
Gate 13 states the policy; DOCS builds the mechanism that enforces it.

## 1. The audit findings, re-verified (every claim grounded on both sides)

### 1.1 `cli.md` is the big gap — CONFIRMED, quantified

The real CLI surface is the clap derive in `src/main.rs` (`Cli`, `src/main.rs:7-12`;
`Command`, `:14-261`; the flattened `CapFlags`, `:267-289`). What `docs/content/cli.md`
documents against it:

| Subcommand | Real flags (`src/main.rs`) | Documented in `cli.md` |
|---|---|---|
| `run` | `--tree-walker` (:22), `--locked` (:27), `--deny`/`--sandbox`/`--deny-net`/`--deny-fs` (CapFlags :275-288), `--inspect` (:36), `--profile` (:43), `--out`/`-o` (:47), `--profile-hz` (:51), `--profile-format` (:58), trailing script args (:63-64) | `cli.md:8-22` — **only `--tree-walker`**. 9 long flags + the `env.args()` forwarding undocumented. |
| `build` | `--out`/`-o` (:71), `--strip` (:75), `--native` (:80), `--target` (:84), the 4 CapFlags | `cli.md:23-31` — **only `-o`**. `--native`/`--target` have depth in `language/bundles.md` (`bundles.md:7,13,106`) but `cli.md` never mentions them. |
| `repl` | `--tree-walker` (:95) | documented (`cli.md:52`). ✓ |
| `fmt` | (no flags, :98-99) | documented. ✓ |
| `check` | `--json`, `--deny-warnings`, `--deny`/`--warn`/`--allow`, `--fix`, `--fix-dry-run` (:100-129) | **fully documented** (`cli.md:131-194`). ✓ |
| `doc` | `--out`, `--format`, `--private`, `--open`, `--check` (:130-153) | documented (`cli.md:250-264`; `--private`/`--check` named, `--open` only on the tooling page). Mostly ✓ — `--open` gap closed in Unit A. |
| `test` | `--locked` (:158), `--deny`/`--sandbox` (:163-167), `--parallel[=N]` (:173-180), `--update-snapshots` (:184), `--filter` (:192), `--watch` (:198), `--coverage[=fmt]` (:205-212) | `cli.md:79-129` — **zero flags documented**. |
| `lsp` | `--stdio` no-op compat (:219-220) | section exists (`cli.md:196-248`), flag absent. |
| `dap` | whole subcommand (:222-231) + `--stdio` | **entirely absent from `cli.md`** (depth exists at `tooling/debugging-profiling.md`). |
| `add`/`remove`/`install`/`update`/`lock`/`tree`/`verify` (pkg, :232-260) | `install --locked` (:246-247) | **absent from `cli.md`**; documented only in `packages.md:131-140`. |

Tally: **27 long flags + the `dap` subcommand + the 7 package subcommands are absent from
`cli.md`** — the audit's "~25+ flags/env-vars undocumented" confirmed. (Caveat from
re-verification: naive grep "hits" for `native`/`target`/`watch`/`add` in `cli.md` are prose
false positives — `cli.md:186,229,256` — the flags genuinely never appear.)

### 1.2 Env vars scattered, one documented nowhere — CONFIRMED

Every `ASCRIPT_*` env var read in `src/` (mechanical `grep -rno 'ASCRIPT_[A-Z_0-9]*' src/`),
with its read site and current docs home:

| Var | Read site | Documented today |
|---|---|---|
| `ASCRIPT_ENGINE` | `src/main.rs:666,828` (run/repl), `src/repl.rs:16` | `runtime.md`, `examples.md`, `README.md:152` — **not `cli.md`** |
| `ASCRIPT_WORKERS` | `src/worker/pool.rs:59`, `src/lib.rs:620`, `src/stdlib/http_server.rs:329` | `language/workers.md` (once) — **not `cli.md`** |
| `ASCRIPT_LOG` | `src/interp.rs:1001` | `stdlib/log.md` — **not `cli.md`** |
| `ASCRIPT_CACHE` | `src/pkg/cache.rs:27` | `packages.md`, `README.md:126` — **not `cli.md`** |
| `ASCRIPT_DENY` | `src/lib.rs:1714` (bundle launch-time monotone deny) | `language/bundles.md`, `README.md:161` — **not `cli.md`** |
| `ASCRIPT_NO_SPECIALIZE` | `src/lib.rs:2066` (generic-VM kill switch) | **NOWHERE** — zero hits in `docs/` + `README.md` |
| `ASCRIPT_UPDATE_SNAPSHOTS` | `src/stdlib/assert_mod.rs:759` | `stdlib/assert.md` — **not `cli.md`** |

Test-only fixtures (all read inside `#[cfg(test)]` modules; verified e.g.
`src/stdlib/redis.rs:268` / `src/stdlib/postgres.rs:460` sit under `mod tests` at
`redis.rs:204`): `ASCRIPT_TEST_REDIS_URL`, `ASCRIPT_TEST_POSTGRES_URL`,
`ASCRIPT_DOTENV_KEY_*`/`ASCRIPT_DOTENV_OTHER_*`, `ASCRIPT_TEST_ENV_*`, `ASCRIPT_E2E_ENV_*`
(`src/stdlib/env.rs`). These are the tripwire allowlist (§5.2).

So: **7 user-facing vars, 0 in `cli.md`, 1 (`ASCRIPT_NO_SPECIALIZE`) documented nowhere at
all** — confirmed, and the scattering is real (8 different pages + README).

### 1.3 The stdlib spot-check — STALE; the real residual gap is ONE member

The audit claimed ~10 undocumented `std/math` members (`floordiv`, `divmod`, `ceildiv`,
`popcount`, `leading_zeros`, `trailing_zeros`, `rotl`, `rotr`, `pi`, `e`). **Re-verified
FALSE on the current tree:** all 44 `std/math` exports (enumerated by namespace-import
introspection — `import * as m from "std/math"; object.keys(m)` against the built binary,
cross-checked against `src/stdlib/math.rs::exports`) appear in
`docs/content/stdlib/collections.md` (`math.pi`/`math.e` at `collections.md:997-998`,
`math.floordiv` at `:1465`, `math.ceildiv` at `:1478`, the bit helpers named at
`:1004-1005`, …). The NUM docs commit (`37ce523 docs(num): numeric model documentation`)
closed this gap after the audit's snapshot.

A **full mechanical sweep of all 57 `STD_MODULES` modules** (same introspection per module;
each exported name grepped across `docs/content/stdlib/*.md` in both documentation styles —
`module.name` prefix form AND the named-import/heading/table forms the SIG spec's §2.2
notation survey catalogues) finds the residual member-level gap is exactly **one**:

- **`task.pipe`** — exported by `std/task` (`src/stdlib/task_mod.rs`), discussed in
  `docs/content/language/workers.md:349` and `language/modules-async.md:234`, but **absent
  from the stdlib reference** (`docs/content/stdlib/async.md`, the `std/task` owning page).

Every other initially-flagged candidate is a documentation-style false positive, verified
individually: `sync.acquire`/`release`/`tryRecv`/`available` are in `async.md`'s method
tables (`async.md:136-179`); `workflow.activity`/`run`/`resume` use named-import style
(`workflow.md:29-55`); `server.create` likewise (`net.md:417-422`); `tui.init` is the
heading style (`tui.md:7-22`); `ffi.u16/u32/i16/i64` are in the grouped marshalling table
(`ffi.md:53-55`). **The audit's "assume other modules have similar tails" is disconfirmed**
— but the sweep methodology is now captured and the plan re-runs it at execution time
(Unit A fills anything that appeared since; §4.3).

### 1.4 `CLAUDE.md` meta-drift — CONFIRMED

`CLAUDE.md:31`: *"The stdlib reference pages mirror the source modules; if you change a
`std/*` API, update the matching `docs/content/stdlib/*.md` page."* But the pages are
**domain-grouped** — 22 pages under `docs/content/stdlib/` (e.g. `collections.md` owns
string + array + object + map + set + math + convert + bytes; `data.md` owns 11 modules)
covering 57 modules. There is no "matching page" per module; a contributor following the
instruction literally looks for `math.md` and finds nothing. (The SIG spec hit the same
wall: its §2.2 records "the brief's assumption of per-module pages is wrong on disk" as a
brief-vs-tree correction.) Unit A rewrites the instruction; Unit B's checked-in
module→page mapping (§5.3) becomes the durable lookup the instruction can point to.

### 1.5 Healthy surfaces — verified and recorded as the baseline

- **NAV ⇄ files bijection holds**: the `NAV` array (`docs/assets/app.js:11-63`) lists
  **40** slugs; `find docs/content -name '*.md'` yields the same 40; set-diff is empty.
  (Delta vs the audit: 40 pages, not 39.)
- **In-content links**: all **134** relative links across `docs/content/**/*.md` resolve
  under the documented relative-to-current-page rule (`app.js` `resolveDocHref`,
  `app.js:81-85`; leading `/` = content-root-absolute) — zero broken today.
- **Editor pins are mutually consistent**: `editors/zed/extension.toml:17` `rev =
  "7227fb7fa00fd6675b03883556906ba8aafed577"` == `editors/nvim/lua/ascript/treesitter.lua:18`
  `GRAMMAR_REV` (same SHA). `tooling/editor-setup.md` embeds no literal SHA (it describes
  the pinning, `editor-setup.md:109`) — so the page cannot go stale on a SHA bump; what CAN
  drift is zed-vs-nvim disagreement (in-repo testable, §5.6) and pin-vs-mirror currency
  (NOT in-repo testable, §5.6).
- **README + language guide accurate on spot-check**: `README.md:107-111` (`run --inspect`,
  `run --profile cpu -o`), `:126` (`$ASCRIPT_CACHE`), `:152` (`ASCRIPT_ENGINE`), `:161`
  (`ASCRIPT_DENY`) all match `src/main.rs`/`src/lib.rs` behavior.
- **Audit citation drift, recorded**: the audit cited `server_capabilities` at
  `src/lsp/server.rs:195-325`; it is at **`server.rs:224`** on the current tree.
  `tooling/lsp-capabilities.md:6` self-declares "The list mirrors `server_capabilities()` in
  `src/lsp/server.rs`, the single source of truth" — Unit A re-verifies the mirror (§4.5).

## 2. Goals / non-goals

**Goals**

1. `docs/content/cli.md` documents the **complete** CLI surface: every subcommand, every
   long flag, the trailing-args convention, plus a consolidated **environment-variable
   reference** — with prose quality (cross-linking depth pages, never duplicating them).
2. The stdlib reference covers every `STD_MODULES` export at member level (the one verified
   gap closed; the sweep re-run at execution time).
3. `CLAUDE.md`'s stdlib-docs instruction matches reality and points at a durable lookup.
4. Tooling pages + README re-verified against the tree.
5. **Six permanent drift tripwires** in `tests/docs_drift.rs` (§5) that fail in CI the
   moment the CLI/env-var/module/NAV/link surfaces drift from the docs — written first,
   red on today's gaps, green after Unit A.
6. A campaign gate addition: `goal-perf.md` gates gain **"docs drift tripwires stay green
   in CI"**; `CLAUDE.md`'s docs guidance names the tripwires.

**Non-goals / rejected (recorded so they aren't re-litigated)**

- **Auto-GENERATING `cli.md` from clap** — rejected. Prose quality matters (the page
  teaches; help text labels); generated prose is worse than curated prose, and the
  tripwire keeps parity without the coupling. Same reasoning as SIG's rejection of
  generating the signature table from docs (SIG §2.3: "the docs stay the social source of
  truth; … drift tests bind them").
- **Doc-comment-driven stdlib reference generation via `ascript doc`** — out of scope,
  recorded as future work. `ascript doc` (DX D1) documents **user** `.as` code from `///`
  comments; pointing it at the stdlib would require machine-readable native-fn signatures —
  exactly SIG's `std_sigs` table — plus a rendering pipeline. Revisit after SIG ships.
- **Screenshots / site redesign** — out.
- **Per-function signature validation of stdlib pages** — explicitly **SIG's territory**
  (§3), not duplicated here.
- **A network-checking editor-pin currency test** — not mechanically testable in-repo
  (§5.6); documented manual checklist instead of a silent gap.

## 3. The SIG/DOCS boundary (explicit, order-independent)

SIG (`superpowers/specs/2026-06-12-lsp-stdlib-signatures-design.md`) also tests docs: its
§2.3 drift test (b) parses `docs/content/stdlib/*.md` and validates **per-function facts**
(param names, optionality, variadic, return) against its curated `std_sigs` table, plus a
member-level docs-coverage assertion for Style-1 modules. The boundary:

| Concern | Owner |
|---|---|
| Per-function **signature** consistency on stdlib pages (params/optionality/return/doc line) | **SIG** (drift test b + the table) |
| Member-level docs coverage for Style-1 modules (curated fn with no docs entry = fail) | **SIG** (drift test b coverage assertion) |
| Module-level **existence/claiming** — every `STD_MODULES` module owned by exactly one stdlib page | **DOCS** (tripwire 3) |
| CLI subcommand/flag currency, env-var currency, feature-page currency | **DOCS** (tripwires 1–2 + Unit A) |
| NAV reachability, in-content link integrity, editor-pin consistency | **DOCS** (tripwires 4–6) |

**Order-independence, stated:** DOCS's tripwires read only `STD_MODULES`, the clap
`Command`, `docs/` files, and `app.js` — none of SIG's artifacts. SIG's docs parser is
style-tolerant by design, and DOCS's Unit-A member additions are written **in the owning
page's existing style** (§4.3), so they parse as one more fact, never a contradiction.
Whichever lands first: if SIG first, its table gains a `task.pipe` row when DOCS documents
it (SIG's coverage assertion would have flagged the missing docs entry for Style-1 modules
anyway — `async.md` is Style-2, so it would not have; the DOCS sweep is what catches it);
if DOCS first, SIG curates from richer docs. **DOCS fills EXISTENCE + one-line
descriptions; SIG validates SIGNATURES** — neither blocks, neither relaxes the other.

## 4. Unit A — the reconciliation run

### 4.1 The clap-surface seam (the one code change, behavior-identical)

Tripwire 1 (§5.1) needs programmatic access to the CLI surface. Decision matrix
(investigated per the brief):

- **(i) Parse `src/main.rs` textually** — rejected: brittle (attribute spans, cfg-gates,
  `#[command(flatten)]` indirection), and it re-implements what clap already knows.
- **(ii) Parse `ascript --help` / `ascript <sub> --help` output** — workable (the
  spawn-binary idiom, `tests/cli.rs:12` `env!("CARGO_BIN_EXE_ascript")`) but couples the
  test to clap's help **formatting** and needs one process spawn per subcommand.
- **(iii) clap introspection via a small exported builder fn** — **chosen.**
  `Command::get_subcommands()` / `get_arguments()` / `Arg::get_long()` are stable clap-4
  APIs (clap 4 is an unconditional `[dependencies]` entry, `Cargo.toml:52`, shared by lib
  and bin targets of the same package).

The seam: the derive types `Cli`, `Command`, `CapFlags` move **verbatim** from
`src/main.rs` into a new library module **`src/cli_surface.rs`** (named to avoid colliding
with `src/stdlib/cli.rs` / the `std/cli` module), which exports

```rust
/// The full clap command tree — the single source of truth for the CLI surface.
/// Consumed by `src/main.rs` (parsing) and `tests/docs_drift.rs` (introspection).
pub fn cli_command() -> clap::Command {
    use clap::CommandFactory;
    Cli::command()
}
```

`src/main.rs` keeps everything else (`compose_caps`, `run_profiled`, `real_main`, the
bundle shim) and does `use ascript::cli_surface::{Cli, Command, CapFlags};`. The pkg
subcommand handlers don't move (the enum's pkg variants carry only `String`s — `src/pkg/`
stays binary-side per SP6). Feature-gated variants (`doc`/`lsp`/`dap`/`pkg`,
`src/main.rs:131,215,225,233-260`) keep their `#[cfg]`s — lib and bin share one feature
set, so the introspected surface always equals the parsed surface **for the build being
tested** (under `--no-default-features` the tree is smaller; the tripwire's "every
introspected item is documented" direction remains valid as a subset check).

**Behavior identity is asserted, not assumed:** `cli_command().debug_assert()` (clap's
self-check) runs as a unit test, and the reviewer diffs `ascript --help` + every
`ascript <sub> --help` before/after the move (must be byte-identical).

### 4.2 `cli.md` to full parity (page structure decided)

Structure (one page, the existing eyebrow/title kept):

1. **Per-command sections, one per clap subcommand, in `Command`-enum order** — `run`,
   `build`, `repl`, `fmt`, `check`, `doc`, `test`, `lsp`, `dap`, then one **"Package
   management"** section listing `add`/`remove`/`install`/`update`/`lock`/`tree`/`verify`
   each in a sentence (depth stays in `packages.md`, cross-linked — the existing
   `packages.md:131-140` reference is not duplicated).
2. Every section gains a **flag table** (`flag` / `meaning`) covering ALL its long flags,
   each one-to-three lines of curated prose. **Cross-link, never duplicate depth:**
   `--native`/`--target`/`--strip` → `language/bundles.md`; `--deny`/`--sandbox`/
   `--deny-net`/`--deny-fs` → `stdlib/caps.md`; `--inspect`/`--profile*` →
   `tooling/debugging-profiling.md`; `--locked` → `packages.md`; `--parallel`/`--coverage`/
   `--watch`/`--filter`/`--update-snapshots` get their primary documentation HERE (no
   deeper page exists); `--tree-walker` → `runtime.md` (as today).
3. `run`'s section documents the **trailing-args convention** (`src/main.rs:61-64`:
   forwarded to `env.args()`, hyphen-values captured).
4. A new **"Environment variables"** section at the end of the page: one table, the 7
   user-facing vars of §1.2 — name, what it controls, which commands honor it, link to the
   deep page (`runtime`, `language/workers`, `stdlib/log`, `packages`, `language/bundles`,
   `stdlib/assert`). `ASCRIPT_NO_SPECIALIZE` gets its **first documentation anywhere**
   (the generic-VM kill switch, `src/lib.rs:2060-2066`, VAL Task 4 bench seam) — worded as
   a debugging/benchmarking knob, mirroring how `--tree-walker` is presented.
   The scattered per-page mentions **stay** (they are in-context and correct); the new
   section is the index, not a move.

### 4.3 The stdlib member sweep (all `STD_MODULES`, methodology fixed)

The sweep is re-run at execution time (the tree moves; §1.3 proved a point-in-time list
rots). Methodology, captured from the §1.3 re-verification:

1. For each of the 57 modules in `stdlib::STD_MODULES` (`src/stdlib/mod.rs:221-279`):
   enumerate exports by **namespace-import introspection** against the built binary
   (`import * as m from "std/<mod>"; print(object.keys(m))`) — exercises
   `std_module_exports` (`mod.rs:114`) exactly as a user does, no Rust parsing.
2. For each export, search the **owning page** (per the §5.3 mapping) for the member in
   ANY of the surveyed styles (SIG §2.2): `mod.name` prefix, backticked heading, named-
   import example, method/constant table row.
3. A miss is verified MANUALLY before it is called a gap (the §1.3 false-positive lesson),
   then filled **in the owning page's existing style** with existence + a one-line
   description (+ a cross-link where a guide page holds the depth). Signature precision
   beyond the page's own style is NOT added — that is SIG's table.

Known fill from the verified sweep: **`task.pipe(gen, bus)`** added to
`docs/content/stdlib/async.md`'s `std/task` section (one entry, Style-2 backticked heading
like its siblings; cross-link `language/workers.md` for the worker-stream bridging depth).

### 4.4 `CLAUDE.md` wording fix

`CLAUDE.md:31` becomes (final wording at implementation, content fixed here): the stdlib
reference is **domain-grouped** — 22 pages covering the 57 `STD_MODULES` modules; the
authoritative module→page mapping is the checked-in table in `tests/docs_drift.rs`
(tripwire 3 validates it both directions); if you change a `std/*` API, update the
module's **owning page** per that mapping, and adding a NEW std module requires a mapping
entry (the tripwire fails otherwise). The adjacent NAV-orphan guidance (`CLAUDE.md:32-35`)
is kept and gains "now enforced by `tests/docs_drift.rs`".

### 4.5 Tooling pages + README re-verification

- **`tooling/lsp-capabilities.md`** vs `server_capabilities()` (`src/lsp/server.rs:224`,
  through `:331+` workspace caps): walk the constructed `ServerCapabilities` fields against
  the page's method tables; fix any mismatch (none expected — the page was written from
  this fn; the audit found none, only the line-number drift §1.5).
- **`tooling/debugging-profiling.md`** limitations vs the real flags: `--profile` v1
  accepts only `cpu` (`main.rs:423-426`), `--profile-format` values incl. the
  `deterministic-*` clocks (`main.rs:431-441`), `.aso`/tree-walker refusal
  (`main.rs:696-700`), `dap` caps posture (`main.rs:1191-1196`) — confirm the page states
  each (spot-check §1.5 found `v1 limitations` and `--profile cpu` present,
  `debugging-profiling.md:66,88`).
- **`tooling/editor-setup.md`** vs the pins: confirm the page still describes (not embeds)
  the pin (`editor-setup.md:109`) and that both pins agree (now also tripwire 6).
- **README + getting-started**: re-run the §1.5 spot-checks; fix anything the `cli.md`
  rewrite reveals as stale.

## 5. Unit B — the permanent drift tripwires (`tests/docs_drift.rs`)

One new integration-test file, repo-rooted via `env!("CARGO_MANIFEST_DIR")` (the
`tests/srv_negative_space.rs` idiom, `srv_negative_space.rs:24-26`). Pure static checks —
no interpreter, no network; runs under both feature configs. Six tripwires:

### 5.1 Tripwire 1 — CLI surface ⊆ `cli.md`

Walk `ascript::cli_surface::cli_command()`: every subcommand name, and every non-positional
argument's long name (skipping clap's auto `help`), across all subcommands (one level —
the tree has no nested subcommands). Assert, against the `cli.md` text:

- each subcommand `s` appears as the literal `ascript <s>`;
- each long flag appears as the literal `--<long>` (anywhere on the page — section
  placement is prose judgment, presence is mechanical).

A **documented-exemption allowlist** exists but starts **EMPTY** (every current flag is
documentable); any future entry requires an owner-justified comment — the same posture as
the env-var allowlist below. **Fails TODAY** with exactly the §1.1 inventory (27 flags,
`dap`, 7 pkg subcommands) — the TDD-red proof the tripwire bites.

### 5.2 Tripwire 2 — env vars ⊆ `cli.md`'s env-var section

Walk `src/**/*.rs`, regex `ASCRIPT_[A-Z0-9_]+`; every match must appear in `cli.md` unless
allowlisted. The allowlist (each entry owner-justified in a comment):
`ASCRIPT_TEST_*`, `ASCRIPT_DOTENV_*`, `ASCRIPT_E2E_*` prefixes — test-fixture vars read
only inside `#[cfg(test)]` modules (§1.2); **the prefixes are hereby reserved for test
fixtures** — a user-facing var must not use them (the allowlist comment states this; a
violation hides from the tripwire, which is why the reservation is written down).
Grep-based rather than cfg-aware by design: simpler, and over-matching (a var named in a
doc comment) errs toward documenting more. **Fails TODAY** for all 7 user-facing vars.

### 5.3 Tripwire 3 — every `STD_MODULES` module claimed by exactly one stdlib page

Naive containment is too noisy in BOTH directions (verified: 20 modules are *mentioned* by
2-5 pages via legitimate cross-references; single-module pages like `schema.md` never
repeat `std/schema` in a heading). So the source of truth is a **checked-in mapping
table** in the test file, validated both directions:

```text
collections.md: string array object map set math convert bytes
data.md:        json csv toml yaml msgpack cbor encoding regex url uuid decimal
system.md:      fs process env os io crypto compress
net.md:         net net/tcp net/http http/server net/udp net/ws
db.md:          sqlite postgres redis
time.md:        time date intl        async.md: task sync
utilities.md:   lru events template   cli.md:   cli color
ai.md: ai   assert.md: assert   bench.md: bench   caps.md: caps   ffi.md: ffi
log.md: log  schema.md: schema  shared.md: shared stream.md: stream
telemetry.md: telemetry  tui.md: tui  workflow.md: workflow
```

(57 modules → 21 owning pages; derived mechanically from the current pages — every mapping
verified by content.) Assertions: (a) the mapping's module set == `STD_MODULES` exactly
(a NEW std module with no docs home fails here — the durable value); (b) every named page
exists under `docs/content/stdlib/`; (c) the owning page contains the literal `std/<mod>`
at least once (a mapping pointing at a page that never discusses the module fails); (d)
reverse: every `docs/content/stdlib/*.md` page except `overview.md` owns ≥1 module (no
orphan reference pages). **Green at birth** (the mapping above is complete) — its
can-it-fail proof is a deliberate-mutation self-test (§5.7).

### 5.4 Tripwire 4 — NAV ⇄ `docs/content` bijection

Automates the standing manual rule (`CLAUDE.md:32-35`; memory note "Docs nav orphan
gotcha"). Parse `docs/assets/app.js` with a tolerant extraction: take the text between
`const NAV = [` and its closing `];`, regex the `['slug', 'Title']` pairs
(`\[\s*'([^']+)'\s*,`). Assert slug-set == the `*.md` file set under `docs/content/`
(extension stripped), both directions, with a clear "add it to NAV in
docs/assets/app.js — the sidebar AND cmd-K search derive from it" failure message.
**Green at birth** (40 == 40, §1.5); mutation self-test proves it can fail.

### 5.5 Tripwire 5 — in-content relative link checker

For every `docs/content/**/*.md`: extract `](target)` links; skip `http(s):`/`mailto:`
and pure-`#anchor`; strip a trailing `#anchor`; resolve per the **documented** rule
(`CLAUDE.md:36-37`; implemented by `resolveDocHref`, `app.js:81-85`): leading `/` =
content-root-absolute, else relative to the current page's directory. The resolved target
must exist as `<target>.md` (or as a literal asset file when the path carries an
extension). Broken link = failure naming page + link. **Green at birth** (134/134, §1.5);
mutation self-test.

### 5.6 Tripwire 6 — editor pins: in-repo consistency test + documented manual item

Pin **currency** (does the pinned SHA match the `ascript-lang/tree-sitter-ascript` mirror
head?) is **not mechanically testable in-repo** — the referent lives in another repository
and checking it needs the network; a CI network check would flake and a stale-allowed pin
is sometimes intentional. Recorded as a **documented manual checklist item**, not a silent
gap: the grammar-publishing checklist (`CLAUDE.md` "Publishing the grammar" +
`CONTRIBUTING.md:46-49`) gains an explicit "verify both editor pins were bumped to the
new SHA" line. What IS in-repo testable, and is: **the two pins agree with each other** —
parse `rev = "…"` from `editors/zed/extension.toml:17` and `GRAMMAR_REV = "…"` from
`editors/nvim/lua/ascript/treesitter.lua:18`; assert equal and 40-hex. (A half-done pin
bump — the historically likely failure — fails this.) **Green at birth** (§1.5).

### 5.7 The anti-false-green rule (applies to all six)

Tripwires 1–2 are **observed red on today's tree** before Unit A (their failure output is
the §1.1/§1.2 inventory — recorded in the plan's task logs). Tripwires 3–6 are green at
birth (healthy baselines, §1.5), so each one's checking logic is factored into a pure
helper (`fn check_nav(nav_src, files) -> Vec<Violation>`-shaped) exercised by a
**deliberate-mutation self-test** — feed a synthetically broken input (a NAV missing one
slug; a mapping missing one module; a link to a nonexistent page; mismatched pins) and
assert the violation is reported. This is the JIT-spec/Gate-15 anti-false-green rule and
SIG's "deliberate-contradiction self-test" applied to docs.

## 6. The gate addition

`goal-perf.md` §"Gates" gains (numbered after the existing 15–18):

> 19. **Docs drift tripwires stay green in CI.** `tests/docs_drift.rs` (DOCS) binds the
>     CLI surface, env vars, stdlib module coverage, NAV, in-content links, and editor-pin
>     consistency to the docs. A spec that adds a flag/env-var/module/page makes the
>     corresponding docs change in the SAME PR — fix the docs, never the assertion
>     (allowlist additions require an owner-justified comment).

`goal-perf.md`'s DX-track section gains the DOCS entry (status table discipline, like
SIG's); `CLAUDE.md`'s docs section names the tripwires (§4.4); `roadmap.md` records the
milestone.

## 7. Test matrix

| # | Test | Today | After Unit A |
|---|---|---|---|
| 1 | CLI surface ⊆ cli.md (introspection walk) | **RED** — §1.1 inventory | green |
| 2 | env vars ⊆ cli.md env section (grep + allowlist) | **RED** — 7 vars | green |
| 3 | module→page mapping, 4 assertions | green + mutation self-test | green |
| 4 | NAV bijection | green + mutation self-test | green |
| 5 | link checker | green + mutation self-test | green |
| 6 | editor-pin consistency | green + mutation self-test | green |
| — | `cli_command().debug_assert()` + help-output identity (§4.1) | n/a | green |
| — | full suite + clippy, BOTH feature configs; `vm_differential` once (no-engine-surface proof) | green | green |

## 8. Performance

No engine surface; no runtime code beyond the §4.1 move (same code, different module —
the binary's parse path is unchanged). Gates 12/17 trivially held; `vm_differential` run
once as the proof. Test cost: pure file/string work, milliseconds.

## 9. Grounding (verified against the current tree, 2026-06-12)

- `src/main.rs`: `Cli:7-12`; `Command:14-261` (Run `:17-65`, Build `:66-88`, Repl
  `:89-97`, Fmt `:98-99`, Check `:100-129`, Doc `:130-153`, Test `:154-213`, Lsp
  `:214-221`, Dap `:222-231`, pkg `:232-260`); `CapFlags:267-289`; `ASCRIPT_ENGINE`
  reads `:666,828`; profile-mode/format validation `:423-441`; profile/.aso refusal
  `:696-700`; dap caps posture `:1191-1196`; trailing args `:61-64`.
- `Cargo.toml:52`: `clap = { version = "4", features = ["derive"] }` (unconditional dep).
- `docs/content/cli.md` (264 lines): run `:8-22`; build `:23-31`; repl `:36-66`; fmt
  `:68-77`; test `:79-129`; check `:131-194`; lsp `:196-248`; doc `:250-264`. Zero
  occurrences of the §1.1 missing flags (prose false positives at `:186,229,256`).
- Env reads: `src/interp.rs:1001` (`ASCRIPT_LOG`); `src/lib.rs:1714` (`ASCRIPT_DENY`),
  `:2060-2066` (`ASCRIPT_NO_SPECIALIZE`), `:616-620` (`ASCRIPT_WORKERS` ceiling);
  `src/worker/pool.rs:59`, `src/stdlib/http_server.rs:329` (`ASCRIPT_WORKERS`);
  `src/pkg/cache.rs:23-27` (`ASCRIPT_CACHE`); `src/stdlib/assert_mod.rs:756-759`
  (`ASCRIPT_UPDATE_SNAPSHOTS`); `src/repl.rs:16` (`ASCRIPT_ENGINE`). Test fixtures:
  `src/stdlib/redis.rs:268` / `postgres.rs:460` (inside `mod tests`, `redis.rs:204`);
  `src/stdlib/env.rs` (`ASCRIPT_DOTENV_*`, `ASCRIPT_TEST_ENV_*`, `ASCRIPT_E2E_ENV_*`).
- `src/stdlib/mod.rs`: `std_module_exports:114`; `STD_MODULES:221-279` (57 entries);
  `is_known_std_module:282-284`.
- `docs/assets/app.js`: `NAV:11-63` (40 slugs); `PAGE_TITLES`/`PAGE_ORDER` derive `:66`;
  `RENDER_DIR:72`; `resolveDocHref:81-85`.
- Docs verification: math members in `collections.md:997-998,1004-1005,1465,1478`;
  `task.pipe` in `language/workers.md:349` + `language/modules-async.md:234`, absent from
  `stdlib/async.md`; style false positives `async.md:136-179`, `workflow.md:29-55`,
  `net.md:417-422`, `tui.md:7-22`, `ffi.md:53-55`; pkg commands `packages.md:131-140`;
  bundles flags `language/bundles.md:7,13,106`; 134 relative links, 0 broken.
- `CLAUDE.md:31` (the meta-drift line), `:32-37` (NAV-orphan + relative-link rules).
- `editors/zed/extension.toml:17` / `editors/nvim/lua/ascript/treesitter.lua:18` — equal
  SHAs (`7227fb7f…`). `CONTRIBUTING.md:46-49` (sync-grammar + pin-bump steps).
- `src/lsp/server.rs:224` `server_capabilities()`; `tooling/lsp-capabilities.md:6` (the
  mirror declaration); `tooling/debugging-profiling.md:66,88`; `tooling/editor-setup.md:109`.
- House patterns: `tests/srv_negative_space.rs:24-26` (repo-root idiom; tests-as-gates);
  `tests/cli.rs:12` (`CARGO_BIN_EXE_ascript`); SIG spec §2.2/§2.3 (docs notation survey;
  curated-table + drift-test discipline; the per-module-pages brief correction).
- `goal.md` Gates item 13 (docs staleness campaign-blocking); `goal-perf.md` pillar 2 +
  Gates 15–18 (the numbering this spec's gate 19 follows).
