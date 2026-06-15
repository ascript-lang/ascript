//! Tree-shaker — reachability ANALYSIS over a module graph (Phase 2, Task 2.1).
//!
//! This module computes, for each compiled module in an archive, the set of
//! top-level binding NAMES that must be KEPT — the reachable closure. It does NOT
//! prune chunks and does NOT touch `compile_archive`/`build` (that is Task 2.3); the
//! output here is purely the per-module keep-set + a minimal report.
//!
//! ## What "reachable" means
//!
//! The ENTRY module (graph index 0) is the program: its whole top-level runs
//! top-to-bottom, so it is kept WHOLE — every one of its top-level `DefineGlobal`
//! names is a root and nothing in it is shaken. Reachability then flows OUTWARD
//! through `import` edges into LIBRARY modules, keeping only the exports actually
//! imported (transitively) plus whatever a library module's own top-level
//! SIDE-EFFECT statements reference (a library `print("loaded")` runs on import).
//!
//! ## Roots, per module
//!
//!   - **Side-effect roots** (every module): names referenced by top-level statements
//!     that are NOT `DefineGlobal`-producing (a bare `print(...)`, a top-level `for` /
//!     `while` / `if`). These run on import, so they + their transitive refs are kept.
//!   - **Entry roots** (index 0 only): ALL of the entry's top-level def names.
//!   - **Import roots** (library modules): the export names an importer pulls in via a
//!     `Named` edge; a `Namespace` edge pins the WHOLE target (Task 2.1 conservative
//!     rule — Task 2.2 will refine).
//!
//! Per module, `keep[i] = compute_closure(chunk, defs, roots[i])` — the transitive
//! closure of those roots over the module's own top-level defs, so a kept binding's
//! own references are kept (no dangling refs by construction).
//!
//! ## Cross-module fixpoint
//!
//! A kept binding in module B may itself `import` a name from module C — but that
//! edge's contribution to C's roots is unconditional in this graph model (an
//! `ImportEdge` is a static, already-resolved import statement, and a top-level
//! `import` ALWAYS runs when the module loads). So the import roots are stable from
//! the first pass and the only reason to iterate is defensive symmetry with later
//! tasks. We nonetheless run a worklist to a genuine fixpoint: recompute each
//! module's closure until no keep-set grows. This is correct and cheap (monotone
//! growth over a finite name set).
//!
//! The closure / def-discovery machinery is shared with the worker code-slice builder
//! (`crate::worker::dispatch`) — both reuse the neutral `pub(crate)` bytecode-analysis
//! helpers in `crate::vm::bcanalysis` rather than re-walking bytecode here.

use crate::span::Span;
use crate::vm::chunk::Chunk;
use crate::vm::opcode::Op;
use crate::vm::bcanalysis::{
    collect_range_refs, compute_closure, top_level_defs, top_level_statement_starts, TopDef,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

/// One node of the module graph the shaker analyzes: a compiled module plus its
/// resolved outgoing import edges. The caller (Task 2.3) builds these from
/// `compile_archive`'s walk, mapping each `ImportDesc.source` to the dedup'd target
/// module index; for THIS task the tests construct graphs directly.
pub struct ModuleNode {
    /// The archive logical key (for diagnostics / the report).
    pub key: String,
    /// The module's compiled chunk.
    pub chunk: Chunk,
    /// Resolved outgoing import edges (target = module index in the graph).
    pub edges: Vec<ImportEdge>,
}

/// A resolved import edge from one module to another module in the graph.
pub enum ImportEdge {
    /// `import { names } from <target>` — pull the named exports from `target`.
    Named {
        /// The target module's index in the graph.
        target: usize,
        /// The imported export names.
        names: Vec<Rc<str>>,
    },
    /// `import * as m from <target>` — bind the target's namespace under the local
    /// `alias`. Task 2.2 refines this: if the importer uses `alias` ONLY as a static
    /// `alias.literalName` member access, only the accessed exports are seeded as roots
    /// (per-binding shakeable); if `alias` is ever used dynamically (`alias[key]`) or
    /// ESCAPES as a value (returned / stored / passed as an arg), the WHOLE target is
    /// pinned (every top-level name becomes a root) and a [`PinReason`] is recorded. See
    /// [`classify_namespace_use`].
    Namespace {
        /// The target module's index in the graph.
        target: usize,
        /// The importer's LOCAL alias for the namespace (`import * as <alias>`). Every
        /// use of the namespace in the importer's chunk begins with `GET_GLOBAL <alias>`.
        alias: Rc<str>,
    },
}

/// The result of a reachability analysis over a module graph.
pub struct ReachResult {
    /// Per-module keep-set: module index → the set of top-level binding NAMES that
    /// must be retained. The entry (index 0) keeps all of its names.
    ///
    /// This is a membership LOOKUP table only (Task 2.3 queries it by name); the inner
    /// `HashSet` iteration order is NON-deterministic. It is NOT an ordered structure —
    /// Task 2.4's reproducible digest must be built from the sorted [`ShakeReport`]
    /// (`dropped`/`pins`), never by iterating `keep`.
    pub keep: HashMap<usize, HashSet<Rc<str>>>,
    /// A minimal report of what the analysis decided (Task 2.4 fleshes this out).
    pub report: ShakeReport,
}

/// The shake report: everything needed to (a) PRINT a human-readable tree-shaking
/// summary to stderr and (b) compute a REPRODUCIBLE 32-byte manifest digest (Task 2.4).
///
/// Every collection it carries is built deterministically so two builds of the SAME
/// source produce a byte-identical [`digest`](Self::digest): per-module dropped names are
/// sorted (a `BTreeSet`), and the digest re-orders modules + pins by their machine-
/// independent LOGICAL KEY (never the graph index, never an absolute path) before
/// hashing. The in-memory `dropped`/`pins` `Vec`s stay in graph index order (the printer
/// is index-friendly); the digest does its OWN key-sort so it is independent of graph
/// traversal order.
#[derive(Debug, Default)]
pub struct ShakeReport {
    /// Per-module dropped top-level names (index → sorted names), in the order the
    /// modules appear in the graph.
    pub dropped: Vec<ModuleDrops>,
    /// Reasons a module was pinned whole (e.g. a namespace import), in graph order.
    pub pins: Vec<PinReason>,
}

impl ShakeReport {
    /// The TOTAL number of dropped declarations across all modules.
    pub fn total_dropped(&self) -> usize {
        self.dropped.iter().map(|d| d.names.len()).sum()
    }

    /// The number of modules that actually dropped at least one declaration.
    pub fn modules_with_drops(&self) -> usize {
        self.dropped.iter().filter(|d| !d.names.is_empty()).count()
    }

    /// Compute the REPRODUCIBLE 32-byte sha256 digest of this report. The serialization
    /// is CANONICAL — building the same source twice yields byte-identical input here
    /// (and therefore an identical digest):
    ///
    ///   - a fixed `b"ascript-shake-v1\0"` domain tag (so an unrelated sha256 can never
    ///     collide with a shake digest, and a future format bump is unambiguous);
    ///   - the DROPS, sorted by the module's LOGICAL KEY (machine-independent — never the
    ///     graph index, never an absolute path); per module, the key then the dropped
    ///     names (already `BTreeSet`-sorted);
    ///   - the PINS, sorted by `(pinned key, importer key, alias, span.start, span.end)`;
    ///     per pin, those fields.
    ///
    /// Every string is LENGTH-PREFIXED with a `u32` (big-endian) so no concatenation
    /// ambiguity exists, every count is a `u32` (big-endian), and NOTHING is iterated
    /// from a `HashMap` (the keep-set table is never touched here). The result is stored
    /// in `ModuleArchive.shake_digest`.
    pub fn digest(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut h = Sha256::new();
        let put_bytes = |h: &mut Sha256, b: &[u8]| {
            h.update((b.len() as u32).to_be_bytes());
            h.update(b);
        };

        h.update(b"ascript-shake-v1\0");

        // ── Drops, sorted by logical key (machine-independent). ──────────────────
        let mut drops: Vec<&ModuleDrops> = self.dropped.iter().collect();
        drops.sort_by(|a, b| a.key.cmp(&b.key));
        h.update((drops.len() as u32).to_be_bytes());
        for d in &drops {
            put_bytes(&mut h, d.key.as_bytes());
            h.update((d.names.len() as u32).to_be_bytes());
            for name in &d.names {
                put_bytes(&mut h, name.as_bytes());
            }
        }

        // ── Pins, sorted by (pinned key, importer key, alias, span). ─────────────
        let mut pins: Vec<&PinReason> = self.pins.iter().collect();
        pins.sort_by(|a, b| {
            a.key
                .cmp(&b.key)
                .then_with(|| a.importer_key.cmp(&b.importer_key))
                .then_with(|| a.alias.cmp(&b.alias))
                .then_with(|| a.span.start.cmp(&b.span.start))
                .then_with(|| a.span.end.cmp(&b.span.end))
        });
        h.update((pins.len() as u32).to_be_bytes());
        for p in &pins {
            put_bytes(&mut h, p.key.as_bytes());
            put_bytes(&mut h, p.importer_key.as_bytes());
            put_bytes(&mut h, p.alias.as_bytes());
            put_bytes(&mut h, p.reason.as_bytes());
            h.update((p.span.start as u32).to_be_bytes());
            h.update((p.span.end as u32).to_be_bytes());
        }

        h.finalize().into()
    }
}

/// The names dropped from one module.
#[derive(Debug)]
pub struct ModuleDrops {
    /// The module's index in the graph.
    pub module: usize,
    /// The module's archive logical key.
    pub key: String,
    /// The top-level names dropped (sorted, deterministic).
    pub names: Vec<Rc<str>>,
}

/// A record that a module was pinned whole (no shaking applied to it).
#[derive(Debug)]
pub struct PinReason {
    /// The pinned module's index in the graph.
    pub module: usize,
    /// The pinned module's archive logical key.
    pub key: String,
    /// The IMPORTER module's index in the graph — the module whose namespace use forced
    /// the pin and whose source the [`span`](Self::span) is an offset into. The report
    /// printer renders the location against THIS module's source (Task 2.4).
    pub importer: usize,
    /// The IMPORTER module's archive logical key (machine-independent), used both for the
    /// rendered `<key>:line:col` location and as the digest's importer identity. NEVER an
    /// absolute path.
    pub importer_key: String,
    /// The importer's local alias for the namespace that triggered the pin
    /// (`import * as <alias>`), for the Task 2.4 report.
    pub alias: Rc<str>,
    /// A human-readable reason (e.g. "namespace `m` used dynamically or escapes").
    pub reason: String,
    /// The source span (CHAR offsets) of the offending `GET_GLOBAL <alias>` use in the
    /// IMPORTER's chunk (a zero span if the chunk records no instruction spans). Rendered
    /// to `line:col` against the importer's source at print time.
    pub span: Span,
    /// The PRE-RENDERED `<importer_key>:line:col` location string, filled by the caller
    /// (`compile_archive`) which holds the module sources — `compute_reachable` itself has
    /// no sources, so it leaves this `None`. Deterministic (line/col are derived from the
    /// machine-independent source + char span); deliberately EXCLUDED from
    /// [`ShakeReport::digest`] (it is redundant with `importer_key` + `span`, which the
    /// digest already covers).
    pub location: Option<String>,
}

/// Render a CHAR offset into `source` as a 1-based `(line, col)` pair. The line is the
/// count of `\n` at-or-before `offset` plus one; the column is the char count since the
/// last `\n` plus one. Robust to an out-of-range offset (clamped to the source length) so
/// a stale/zero span never panics. Used by [`render_pin_location`] for the build report.
pub fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in source.chars().enumerate() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Pre-render a pin's `<importer_key>:line:col` location string against the importer's
/// `source`. The caller (`compile_archive`) holds the sources; the printer then needs
/// none. Machine-independent (the key is logical, line/col are source-derived).
pub fn render_pin_location(importer_key: &str, source: &str, span: Span) -> String {
    let (line, col) = line_col(source, span.start);
    format!("{importer_key}:{line}:{col}")
}

/// Collect the NAMES referenced by a module's top-level SIDE-EFFECT statements — the
/// top-level statements that are NOT `DefineGlobal`-producing (a bare `print(...)`, a
/// top-level `for`/`while`/`if`, a block). Such statements run when the module is
/// imported, so their references (transitively) must be kept.
///
/// We walk the module's top-level statement leaders ([`top_level_statement_starts`]);
/// for each leader's instruction range `[start, next)`, if that range does NOT end in
/// a `DefineGlobal` (i.e. it is not a binding-producing statement), we collect its
/// `GET_GLOBAL` references via [`collect_range_refs`] (which also recurses into any
/// nested `CLOSURE`'d proto in the range).
fn side_effect_roots(chunk: &Chunk) -> Vec<Rc<str>> {
    let starts = top_level_statement_starts(chunk);
    let code_len = chunk.code.len();
    let mut out: Vec<Rc<str>> = Vec::new();

    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(code_len);
        if statement_is_definition(chunk, start, end) {
            // A binding-producing OR export-registration statement: its refs are pulled
            // by the closure when the bound NAME is reachable, not force-rooted here.
            continue;
        }
        collect_range_refs(chunk, start, end, &mut out);
    }
    out
}

/// Is the top-level statement spanning `[start, end)` a BINDING or EXPORT-registration
/// statement (as opposed to a real side-effect statement)?
///
/// A binding compiles to `<value-producing ops>; DEFINE_GLOBAL name`. The `export`
/// keyword adds a SEPARATE top-level statement `GET_GLOBAL name; DEFINE_EXPORT name`
/// that merely registers the already-bound global for importers — it is NOT a load-time
/// side effect, so its `GET_GLOBAL name` must NOT be force-rooted (otherwise EVERY
/// exported name would be unconditionally kept, defeating the shake). Such an export
/// statement's name is kept ONLY when an inbound import edge demands it; if the name is
/// dropped, the source-level `export <decl>` is pruned as a unit in pass 2, so no
/// dangling `GET_GLOBAL` survives. We find the LAST decodable instruction in the range
/// and test for either terminator.
fn statement_is_definition(chunk: &Chunk, start: usize, end: usize) -> bool {
    use crate::vm::opcode::Op;
    let mut ip = start;
    let mut last: Option<Op> = None;
    while ip < end {
        let Some(op) = Op::from_u8(chunk.code[ip]) else {
            break;
        };
        last = Some(op);
        ip += 1 + op.operand_width();
    }
    matches!(last, Some(Op::DefineGlobal | Op::DefineExport))
}

/// How a namespace alias is used throughout an importing module's chunk — the verdict
/// that decides whether the target module is per-binding shakeable or must be pinned
/// whole. See [`classify_namespace_use`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceUse {
    /// EVERY use of the alias is a STATIC member access (`alias.literalName`,
    /// `alias.method()`, `alias?.literalName`) — the set of accessed property names
    /// (possibly empty if the alias is imported but never used). The target is
    /// per-binding shakeable: only these names need be seeded as roots.
    Static(BTreeSet<Rc<str>>),
    /// At least one use of the alias is DYNAMIC (`alias[key]`) or ESCAPES (the
    /// namespace value flows into a call / return / store / arg / anything other than
    /// an immediately-following static-member op). The target CANNOT be shaken; pin it
    /// whole. `span` is the source span of the FIRST offending `GET_GLOBAL <alias>`.
    Dynamic {
        /// The source span of the first offending use (a zero span if the chunk has
        /// no instruction span table).
        span: Span,
    },
}

/// Should the forward receiver-simulation BAIL (classify the site as escape) if it
/// encounters `op` before resolving the namespace value's consumer? These are the ops
/// over which a straight-line stack simulation is NOT valid or NOT sound:
///
/// - jumps/branches ([`Op::Jump`]/`JumpIf*`/[`Op::Loop`]/`JumpIfArgSupplied`) — the
///   consumer might be reached via control flow, so the linear height tracking breaks
///   (e.g. `m.foo(cond ? a : b)` has a branch in its args);
/// - terminators ([`Op::Return`]/[`Op::Yield`]/[`Op::MatchNoArm`]/[`Op::ImmutableError`]) —
///   the path ends without a clean consumer;
/// - value-reshapers ([`Op::Dup`]/[`Op::Swap`]/[`Op::Rot3`]) — they DUPLICATE or REORDER
///   stack slots, so our single "height above `m`" counter can no longer prove WHERE the
///   namespace value sits.
///
/// Bailing here is always SOUND (under-shaking never drops live code).
fn sim_bails_on(op: Op) -> bool {
    matches!(
        op,
        Op::Jump
            | Op::JumpIfFalse
            | Op::JumpIfTrue
            | Op::JumpIfNotNil
            | Op::Loop
            | Op::JumpIfArgSupplied
            | Op::Return
            | Op::Yield
            | Op::MatchNoArm
            | Op::ImmutableError
            | Op::Dup
            | Op::Swap
            | Op::Rot3
    )
}

/// The verdict for ONE `GET_GLOBAL <alias>` use site, resolved by forward stack
/// simulation (see [`classify_site`]).
enum SiteVerdict {
    /// The site is a static `alias.<name>` member access / method call; `name` is the
    /// accessed property/method.
    Static(Rc<str>),
    /// The site escapes / is dynamically indexed — pin the whole target.
    Escape,
}

/// Read the property/method NAME const that rides a static-member op's FIRST `u16`
/// operand (`GET_PROP`/`GET_PROP_OPT`/`CALL_METHOD`/`CALL_METHOD_SPREAD`). Returns
/// `None` if the operand or const is missing/not a string (→ caller treats as escape).
fn member_name_at(chunk: &Chunk, op_ip: usize) -> Option<Rc<str>> {
    let name_ip = op_ip + 1;
    if name_ip + 1 >= chunk.code.len() {
        return None;
    }
    let idx = chunk.read_u16(name_ip) as usize;
    match chunk.consts.get(idx).map(|v| v.kind()) {
        // NANB Task 2.2: COLD (tree-shake analysis) — zero-cost clone under default repr.
        Some(crate::value::ValueKind::Str(name)) => Some(crate::value::astr_to_rc(name)),
        _ => None,
    }
}

/// Classify a single `GET_GLOBAL <alias>` use at `site_ip` (the opcode byte) by FORWARD
/// stack simulation, finding the op that CONSUMES the namespace value `m`.
///
/// We track `above` = the stack height STRICTLY ABOVE `m` (starts at 0, `m` just pushed).
/// Walking forward, for each op with stack effect `(pops, pushes)` (the authoritative
/// `verify::op_stack_pops_pushes` table):
///
/// - `pops <= above` → the op consumes only values above `m`; `m` survives. Update
///   `above = above - pops + pushes` and continue.
/// - `pops > above` → the op reaches DOWN to (and below) `m`'s slot — THIS is `m`'s
///   consumer. It is a STATIC member use iff it pops EXACTLY the args above `m` plus `m`
///   itself (`pops == above + 1`, so it never reaches BELOW `m`) AND it is either a
///   `GET_PROP`/`GET_PROP_OPT` with `above == 0` (`m` on top, a plain read) or a
///   `CALL_METHOD`/`CALL_METHOD_SPREAD` with `m` as the receiver at the bottom of its
///   `pops` (`above` args sit above the receiver `m`). The accessed name then rides the
///   op's first `u16` operand. Anything else (`pops > above + 1` reaching below `m`, a
///   `GET_INDEX`/`CALL`/`SET_*`/store, …) is an escape.
///
/// BAIL → escape if, before the consumer is found, we hit a [`sim_bails_on`] op, an op
/// we cannot decode, or the scan runs off the end of `code` (the consumer is unreachable
/// on this straight-line path). Bailing is sound (under-shaking).
fn classify_site(chunk: &Chunk, site_ip: usize) -> SiteVerdict {
    let code = &chunk.code;
    let len = code.len();
    // Step over the GET_GLOBAL itself; `m` is now on top, `above` = 0.
    let Some(site_op) = Op::from_u8(code[site_ip]) else {
        return SiteVerdict::Escape;
    };
    let mut ip = site_ip + 1 + site_op.operand_width();
    let mut above: usize = 0;
    while ip < len {
        let Some(op) = Op::from_u8(code[ip]) else {
            return SiteVerdict::Escape; // undecodable → bail.
        };
        if sim_bails_on(op) {
            return SiteVerdict::Escape; // control flow / reshaper / terminator → bail.
        }
        let (pops, pushes) = crate::vm::verify::op_stack_pops_pushes(chunk, op, ip + 1);
        if pops > above {
            // This op consumes `m`. Static only if it pops exactly the args above `m`
            // plus `m` (never below) AND it is a member-access op with `m` as receiver.
            if pops == above + 1 {
                match op {
                    // GetProp/GetPropOpt pop 1, so pops == above + 1 implies above == 0
                    // (the guard); `m` is on top — the only valid receiver position for a
                    // member read. The guard makes that invariant legible at the match site.
                    Op::GetProp | Op::GetPropOpt if above == 0 => {
                        return match member_name_at(chunk, ip) {
                            Some(name) => SiteVerdict::Static(name),
                            None => SiteVerdict::Escape,
                        };
                    }
                    Op::CallMethod | Op::CallMethodSpread => {
                        return match member_name_at(chunk, ip) {
                            Some(name) => SiteVerdict::Static(name),
                            None => SiteVerdict::Escape,
                        };
                    }
                    _ => return SiteVerdict::Escape,
                }
            }
            return SiteVerdict::Escape; // reaches below `m` → escape.
        }
        // `m` survives; update the height above it.
        above = above - pops + pushes;
        ip += 1 + op.operand_width();
    }
    SiteVerdict::Escape // ran off the end without a consumer → bail.
}

/// Classify how the namespace `alias` is used across `chunk` (recursing into every
/// nested proto). A namespace alias is a user-global, so every use begins with
/// `GET_GLOBAL <alias>`. EACH such site is classified INDEPENDENTLY by forward stack
/// simulation ([`classify_site`]): a static `alias.name` read or `alias.name(args)`
/// method call contributes `name` to the accessed set; any escaping / dynamically
/// indexed / uncertain site makes the WHOLE result [`NamespaceUse::Dynamic`].
///
/// Because each site is classified on its own, a mix of static and escaping sites
/// correctly pins whole (the escaping site forces it) — only when ALL sites are static
/// do we shake to the union of accessed names. The walk is byte-level and
/// decode-resilient and never `.unwrap()`/`panic!`s on chunk contents.
pub fn classify_namespace_use(chunk: &Chunk, alias: &str) -> NamespaceUse {
    let mut props: BTreeSet<Rc<str>> = BTreeSet::new();
    if let Some(span) = classify_namespace_use_rec(chunk, alias, &mut props) {
        NamespaceUse::Dynamic { span }
    } else {
        NamespaceUse::Static(props)
    }
}

/// The recursive worker for [`classify_namespace_use`]. Walks `chunk.code`, then every
/// nested proto's chunk. Returns `Some(span)` at the FIRST escaping/dynamic use of
/// `alias` (short-circuiting), or `None` if every use is a static member (with the
/// accessed names accumulated into `props`).
fn classify_namespace_use_rec(
    chunk: &Chunk,
    alias: &str,
    props: &mut BTreeSet<Rc<str>>,
) -> Option<Span> {
    let code = &chunk.code;
    let len = code.len();
    let mut ip = 0usize;
    while ip < len {
        let Some(op) = Op::from_u8(code[ip]) else {
            // Undecodable byte: stop scanning this chunk's linear stream (defensive —
            // never read past the code).
            break;
        };
        let width = op.operand_width();
        if op == Op::GetGlobal && ip + 2 < len {
            // The GET_GLOBAL's u16 operand names the global being read.
            let name_idx = chunk.read_u16(ip + 1) as usize;
            let is_alias = matches!(
                chunk.consts.get(name_idx).map(|v| v.kind()),
                Some(crate::value::ValueKind::Str(name)) if &**name == alias
            );
            if is_alias {
                match classify_site(chunk, ip) {
                    SiteVerdict::Static(name) => {
                        props.insert(name);
                    }
                    SiteVerdict::Escape => {
                        // Capture the span of THIS GET_GLOBAL site and short-circuit.
                        return Some(chunk.span_at(ip));
                    }
                }
            }
        }
        ip += 1 + width;
    }
    // Recurse into nested protos (a static/dynamic use can live inside a fn body).
    for proto in &chunk.protos {
        if let Some(span) = classify_namespace_use_rec(&proto.chunk, alias, props) {
            return Some(span);
        }
    }
    None
}

/// Compute, for each module in `graph`, the set of top-level binding NAMES that must
/// be KEPT (the reachable closure), plus a minimal report. See the module docs for
/// the semantics; `graph[0]` is the entry and is kept whole.
pub fn compute_reachable(graph: &[ModuleNode]) -> ReachResult {
    let n = graph.len();

    // Per-module: the resolved top-level defs and the side-effect roots (both stable
    // across the fixpoint — they depend only on the module's own chunk).
    let mut defs: Vec<HashMap<Rc<str>, TopDef>> = Vec::with_capacity(n);
    let mut base_roots: Vec<Vec<Rc<str>>> = Vec::with_capacity(n);
    for (i, node) in graph.iter().enumerate() {
        let d = top_level_defs(&node.chunk);
        // Every module's side-effect statements run on import → their refs are roots.
        let mut roots = side_effect_roots(&node.chunk);
        // A top-level binding whose initializer is COMPUTED (`let x = sideEffect()`,
        // not a bare literal/closure/class/enum/interface) runs its initializer
        // EAGERLY when the module is imported (a top-level `Op::Import` runs the whole
        // module body — spec §4.3). Such a binding is an always-kept module-load side
        // effect: force its NAME into the roots (so it is kept regardless of inbound
        // references) AND root its initializer's referenced names. This is deliberately
        // MORE conservative than the worker slice builder (which ships a ComputedConst
        // only if referenced — it runs in a fresh isolate with no module-load side
        // effects); the shaker must preserve eager side effects, so ALL top-level
        // ComputedConsts are kept. A literal `let x = 5` (`TopDef::Const`) / `Fn` /
        // `Class` / `Interface` stays droppable-if-unreferenced — those are inert value
        // producers with no load-time side effect.
        for (name, def) in &d {
            if matches!(def, TopDef::ComputedConst { .. }) {
                roots.push(name.clone());
            }
        }
        // The ENTRY module (index 0) is kept WHOLE: every top-level def name is a
        // root. Library modules keep only what is reached via edges + side-effects +
        // the computed-initializer roots above.
        if i == 0 {
            roots.extend(d.keys().cloned());
        }
        defs.push(d);
        base_roots.push(roots);
    }

    // Import roots + namespace classification. An edge's contribution is static (a
    // top-level import always runs), so these are computed once, up front.
    //
    // A `Namespace` edge is classified by [`classify_namespace_use`] against the
    // IMPORTER's chunk:
    //   - `Static(props)` → seed `target`'s roots with `props` (exactly like a `Named`
    //     edge) — the unaccessed exports become shakeable.
    //   - `Dynamic { span }` → pin the WHOLE `target` AND record a `PinReason`. ANY
    //     dynamic edge into a target pins it whole (over a `Static` edge from elsewhere).
    let mut import_roots: Vec<Vec<Rc<str>>> = vec![Vec::new(); n];
    let mut pinned_whole: Vec<bool> = vec![false; n];
    // Per-pinned-target: the (importer index, alias, span) of the FIRST dynamic namespace
    // use that pinned it (for the report). Graph-deterministic: edges are walked in node
    // order, so the FIRST dynamic importer is stable across builds.
    let mut pin_info: Vec<Option<(usize, Rc<str>, Span)>> = vec![None; n];
    for (importer, node) in graph.iter().enumerate() {
        for edge in &node.edges {
            match edge {
                ImportEdge::Named { target, names } => {
                    if let Some(slot) = import_roots.get_mut(*target) {
                        slot.extend(names.iter().cloned());
                    }
                }
                ImportEdge::Namespace { target, alias } => {
                    let t = *target;
                    match classify_namespace_use(&node.chunk, alias) {
                        NamespaceUse::Static(props) => {
                            // Per-binding shake: seed only the accessed exports.
                            if let Some(slot) = import_roots.get_mut(t) {
                                slot.extend(props.iter().cloned());
                            }
                        }
                        NamespaceUse::Dynamic { span } => {
                            if let Some(slot) = pinned_whole.get_mut(t) {
                                *slot = true;
                            }
                            if let Some(slot) = pin_info.get_mut(t) {
                                if slot.is_none() {
                                    *slot = Some((importer, alias.clone(), span));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // A namespace-pinned target keeps ALL of its top-level names: add every def name
    // to its roots.
    for (i, &pinned) in pinned_whole.iter().enumerate() {
        if pinned {
            import_roots[i].extend(defs[i].keys().cloned());
        }
    }

    // The fixpoint: recompute each module's closure until no keep-set grows. Roots
    // are stable, so a single pass already reaches the fixpoint; the worklist is the
    // defensive, obviously-correct formulation.
    let mut keep: Vec<HashSet<Rc<str>>> = vec![HashSet::new(); n];
    let mut changed = true;
    while changed {
        changed = false;
        for i in 0..n {
            // Seed = base roots (side-effects + entry-all) ∪ import roots (named +
            // namespace-pin) ∪ whatever is already kept (monotone growth).
            let mut roots: Vec<Rc<str>> = base_roots[i].clone();
            roots.extend(import_roots[i].iter().cloned());
            roots.extend(keep[i].iter().cloned());
            let closure = compute_closure(&graph[i].chunk, &defs[i], roots);
            // closure ⊇ keep[i] by construction (keep[i] is seeded into roots above);
            // size growth is the exact termination criterion.
            if closure.len() > keep[i].len() {
                keep[i] = closure;
                changed = true;
            }
        }
    }

    // Build the report: per-module dropped names (all top-level names minus keep) and
    // pin reasons, deterministically ordered.
    let mut dropped: Vec<ModuleDrops> = Vec::new();
    let mut pins: Vec<PinReason> = Vec::new();
    for (i, node) in graph.iter().enumerate() {
        let all: BTreeSet<Rc<str>> = defs[i].keys().cloned().collect();
        let drop_names: Vec<Rc<str>> = all
            .into_iter()
            .filter(|name| !keep[i].contains(name))
            .collect();
        dropped.push(ModuleDrops {
            module: i,
            key: node.key.clone(),
            names: drop_names,
        });
        if pinned_whole[i] {
            // `pin_info[i]` is set whenever `pinned_whole[i]` is (they are written
            // together in the namespace-classification loop); fall back to a placeholder
            // importer (self) + span + empty alias only if it were somehow absent (never
            // in practice).
            let (importer, alias, span) = pin_info[i]
                .clone()
                .unwrap_or_else(|| (i, Rc::from(""), Span::new(0, 0)));
            let importer_key = graph
                .get(importer)
                .map(|m| m.key.clone())
                .unwrap_or_default();
            pins.push(PinReason {
                module: i,
                key: node.key.clone(),
                importer,
                importer_key,
                reason: format!("namespace `{alias}` used dynamically or escapes"),
                alias,
                span,
                location: None, // filled by the caller, which holds the module sources
            });
        }
    }

    let mut keep_map: HashMap<usize, HashSet<Rc<str>>> = HashMap::with_capacity(n);
    for (i, set) in keep.into_iter().enumerate() {
        keep_map.insert(i, set);
    }

    ReachResult {
        keep: keep_map,
        report: ShakeReport { dropped, pins },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile_source;

    /// Compile a source to a chunk (panicking on a compile error — these are
    /// test-controlled fixtures).
    fn chunk(src: &str) -> Chunk {
        compile_source(src).expect("test fixture should compile")
    }

    fn rc(s: &str) -> Rc<str> {
        Rc::from(s)
    }

    /// Does `keep[i]` contain `name`?
    fn kept(res: &ReachResult, i: usize, name: &str) -> bool {
        res.keep.get(&i).is_some_and(|s| s.contains(&rc(name)))
    }

    #[test]
    fn headline_named_import_shakes_unused() {
        // Entry imports { used } from lib. lib defines used (which calls helper h),
        // unused, and h. keep[lib] ⊇ {used, h}, ∌ unused.
        let entry = chunk(r#"let x = 1"#);
        let lib = chunk(
            r#"
            fn h() { 42 }
            fn used() { h() }
            fn unused() { 99 }
        "#,
        );
        let graph = vec![
            ModuleNode {
                key: "entry".into(),
                chunk: entry,
                edges: vec![ImportEdge::Named {
                    target: 1,
                    names: vec![rc("used")],
                }],
            },
            ModuleNode {
                key: "lib".into(),
                chunk: lib,
                edges: vec![],
            },
        ];
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "used"), "used must be kept");
        assert!(kept(&res, 1, "h"), "h (transitive ref of used) must be kept");
        assert!(!kept(&res, 1, "unused"), "unused must be dropped");
        // The report records the drop.
        let drops = &res.report.dropped[1];
        assert!(drops.names.contains(&rc("unused")));
        assert!(!drops.names.contains(&rc("used")));
    }

    #[test]
    fn computed_initializer_binding_kept_with_refs() {
        // Regression: a top-level `let x = sideEffect()` runs EAGERLY on module import
        // (spec §4.3) — its initializer is a load-time side effect, so x, sideEffect,
        // and sideEffect's transitive helper must ALL be kept even though the entry
        // imports nothing from the library.
        let entry = chunk(r#"let z = 1"#);
        let lib = chunk(
            r#"
            fn helper() { return 5 }
            fn sideEffect() { return helper() }
            let x = sideEffect()
        "#,
        );
        let graph = vec![
            ModuleNode {
                key: "entry".into(),
                chunk: entry,
                edges: vec![],
            },
            ModuleNode {
                key: "lib".into(),
                chunk: lib,
                edges: vec![],
            },
        ];
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "x"), "computed-init binding x must be kept");
        assert!(
            kept(&res, 1, "sideEffect"),
            "x's initializer ref sideEffect must be kept"
        );
        assert!(
            kept(&res, 1, "helper"),
            "sideEffect's transitive ref helper must be kept"
        );
        // None of the three appear in the dropped report.
        let drops = &res.report.dropped[1];
        assert!(!drops.names.contains(&rc("x")));
        assert!(!drops.names.contains(&rc("sideEffect")));
        assert!(!drops.names.contains(&rc("helper")));
    }

    #[test]
    fn literal_let_still_droppable_if_unreferenced() {
        // Guard against over-correction: a literal `let x = 5` (TopDef::Const) is an
        // inert value producer with NO load-time side effect, so it stays
        // droppable-if-unreferenced. Here the lib's `k` is never imported / referenced.
        let entry = chunk(r#"let z = 1"#);
        let lib = chunk(
            r#"
            let k = 5
            fn used() { 1 }
        "#,
        );
        let graph = vec![
            ModuleNode {
                key: "entry".into(),
                chunk: entry,
                edges: vec![ImportEdge::Named {
                    target: 1,
                    names: vec![rc("used")],
                }],
            },
            ModuleNode {
                key: "lib".into(),
                chunk: lib,
                edges: vec![],
            },
        ];
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "used"), "imported used kept");
        assert!(
            !kept(&res, 1, "k"),
            "an unreferenced literal `let k = 5` must still be droppable"
        );
        assert!(res.report.dropped[1].names.contains(&rc("k")));
    }

    #[test]
    fn side_effect_retention() {
        // lib has a top-level print referencing g, plus an unused fn. The side-effect
        // pulls g but not unused.
        let entry = chunk(r#"let x = 1"#);
        let lib = chunk(
            r#"
            fn g() { 7 }
            fn unused() { 8 }
            print(g())
        "#,
        );
        let graph = vec![
            ModuleNode {
                key: "entry".into(),
                chunk: entry,
                edges: vec![],
            },
            ModuleNode {
                key: "lib".into(),
                chunk: lib,
                edges: vec![],
            },
        ];
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "g"), "g referenced by side-effect must be kept");
        assert!(
            !kept(&res, 1, "unused"),
            "unused not referenced by the side-effect must be dropped"
        );
    }

    #[test]
    fn side_effect_does_not_pull_unused_when_referencing_nothing() {
        // A side-effect referencing nothing top-level does not keep unused.
        let entry = chunk(r#"let x = 1"#);
        let lib = chunk(
            r#"
            fn unused() { 8 }
            print("loaded")
        "#,
        );
        let graph = vec![
            ModuleNode {
                key: "entry".into(),
                chunk: entry,
                edges: vec![],
            },
            ModuleNode {
                key: "lib".into(),
                chunk: lib,
                edges: vec![],
            },
        ];
        let res = compute_reachable(&graph);
        assert!(!kept(&res, 1, "unused"), "unused must be dropped");
    }

    #[test]
    fn transitive_class_and_superclass() {
        // used constructs C, which extends Base. C + Base kept; Other dropped.
        let entry = chunk(r#"let x = 1"#);
        let lib = chunk(
            r#"
            class Base { fn hi() { 1 } }
            class C extends Base { fn yo() { 2 } }
            class Other { fn no() { 3 } }
            fn used() { C() }
        "#,
        );
        let graph = vec![
            ModuleNode {
                key: "entry".into(),
                chunk: entry,
                edges: vec![ImportEdge::Named {
                    target: 1,
                    names: vec![rc("used")],
                }],
            },
            ModuleNode {
                key: "lib".into(),
                chunk: lib,
                edges: vec![],
            },
        ];
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "used"), "used kept");
        assert!(kept(&res, 1, "C"), "constructed class C kept");
        assert!(kept(&res, 1, "Base"), "superclass Base kept");
        assert!(!kept(&res, 1, "Other"), "unrelated class Other dropped");
    }

    /// Build a two-node graph: entry (index 0) `import * as m from "./lib"` with the
    /// given entry source body, and `lib` (index 1) with the given lib source. The
    /// namespace edge uses alias `m`.
    fn ns_graph(entry_src: &str, lib_src: &str) -> Vec<ModuleNode> {
        vec![
            ModuleNode {
                key: "entry".into(),
                chunk: chunk(entry_src),
                edges: vec![ImportEdge::Namespace {
                    target: 1,
                    alias: rc("m"),
                }],
            },
            ModuleNode {
                key: "lib".into(),
                chunk: chunk(lib_src),
                edges: vec![],
            },
        ]
    }

    const LIB_ABC_HELPER: &str = r#"
        fn helper() { return 0 }
        fn foo() { return helper() }
        fn bar() { return 2 }
        fn baz() { return 3 }
    "#;

    #[test]
    fn classify_static_member_reads() {
        // `m.foo` (bare read) + `m.bar` (bare read) → Static({foo, bar}).
        let c = chunk("import * as m from \"./lib\"\nlet a = m.foo\nlet b = m.bar\n");
        match classify_namespace_use(&c, "m") {
            NamespaceUse::Static(props) => {
                assert!(props.contains(&rc("foo")));
                assert!(props.contains(&rc("bar")));
                assert_eq!(props.len(), 2);
            }
            other => panic!("expected Static, got {other:?}"),
        }
    }

    #[test]
    fn classify_zero_arg_method_call_is_static() {
        // `m.foo()` lowers to GET_GLOBAL m; CALL_METHOD foo — still a static member use.
        let c = chunk("import * as m from \"./lib\"\nlet a = m.foo()\n");
        match classify_namespace_use(&c, "m") {
            NamespaceUse::Static(props) => assert!(props.contains(&rc("foo"))),
            other => panic!("expected Static, got {other:?}"),
        }
    }

    #[test]
    fn classify_optional_chain_is_static() {
        // `m?.foo` lowers to GET_GLOBAL m; GET_PROP_OPT foo — still a static member use.
        let c = chunk("import * as m from \"./lib\"\nlet a = m?.foo\n");
        match classify_namespace_use(&c, "m") {
            NamespaceUse::Static(props) => assert!(props.contains(&rc("foo"))),
            other => panic!("expected Static, got {other:?}"),
        }
    }

    #[test]
    fn classify_unused_alias_is_static_empty() {
        // Imported but never used → Static(∅): everything in the target is shakeable.
        let c = chunk("import * as m from \"./lib\"\nlet x = 1\n");
        assert_eq!(classify_namespace_use(&c, "m"), NamespaceUse::Static(BTreeSet::new()));
    }

    #[test]
    fn classify_dynamic_index_is_dynamic() {
        let c = chunk("import * as m from \"./lib\"\nlet k = \"foo\"\nlet r = m[k]\n");
        assert!(matches!(
            classify_namespace_use(&c, "m"),
            NamespaceUse::Dynamic { .. }
        ));
    }

    #[test]
    fn classify_escape_is_dynamic() {
        // `let g = m` — the namespace value flows into a store.
        let c = chunk("import * as m from \"./lib\"\nlet g = m\n");
        assert!(matches!(
            classify_namespace_use(&c, "m"),
            NamespaceUse::Dynamic { .. }
        ));
    }

    #[test]
    fn namespace_static_use_shakes_unused() {
        // Test 1: entry uses only m.foo + m.bar; lib has foo (calls helper), bar, baz.
        // keep[lib] ⊇ {foo, bar, helper}, ∌ baz.
        let graph = ns_graph(
            "import * as m from \"./lib\"\nlet a = m.foo()\nlet b = m.bar()\n",
            LIB_ABC_HELPER,
        );
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "foo"), "m.foo accessed → foo kept");
        assert!(kept(&res, 1, "bar"), "m.bar accessed → bar kept");
        assert!(kept(&res, 1, "helper"), "foo's transitive helper kept");
        assert!(!kept(&res, 1, "baz"), "unaccessed baz dropped");
        assert!(res.report.dropped[1].names.contains(&rc("baz")));
        // A statically-shaken namespace records NO pin.
        assert!(
            res.report.pins.is_empty(),
            "static namespace use must not pin"
        );
    }

    #[test]
    fn namespace_dynamic_index_pins_whole() {
        // Test 2: entry uses m[k] (computed) → keep[lib] = ALL lib exports; pin recorded.
        let graph = ns_graph(
            "import * as m from \"./lib\"\nlet k = \"foo\"\nlet r = m[k]\n",
            LIB_ABC_HELPER,
        );
        let res = compute_reachable(&graph);
        for name in ["foo", "bar", "baz", "helper"] {
            assert!(kept(&res, 1, name), "dynamic index pins whole: {name} kept");
        }
        assert!(res.report.dropped[1].names.is_empty(), "nothing dropped");
        let pin = res
            .report
            .pins
            .iter()
            .find(|p| p.module == 1)
            .expect("lib pinned");
        assert_eq!(&*pin.alias, "m");
        assert!(pin.reason.contains('m'));
    }

    #[test]
    fn namespace_escape_pins_whole() {
        // Test 3: entry does `let g = m` (escape) → whole-module pin + report.
        let graph = ns_graph(
            "import * as m from \"./lib\"\nlet g = m\n",
            LIB_ABC_HELPER,
        );
        let res = compute_reachable(&graph);
        for name in ["foo", "bar", "baz", "helper"] {
            assert!(kept(&res, 1, name), "escape pins whole: {name} kept");
        }
        assert!(res.report.pins.iter().any(|p| p.module == 1));
    }

    #[test]
    fn namespace_mixed_static_and_dynamic_pins_whole() {
        // Test 4: m.foo AND m[k] in the same module → ANY dynamic pins whole.
        let graph = ns_graph(
            "import * as m from \"./lib\"\nlet a = m.foo()\nlet k = \"bar\"\nlet r = m[k]\n",
            LIB_ABC_HELPER,
        );
        let res = compute_reachable(&graph);
        for name in ["foo", "bar", "baz", "helper"] {
            assert!(kept(&res, 1, name), "mixed → whole pin: {name} kept");
        }
        assert!(res.report.pins.iter().any(|p| p.module == 1));
    }

    #[test]
    fn namespace_static_use_inside_nested_fn() {
        // Test 5: `fn wrap() { return m.foo() }` — static use inside a proto (not
        // top-level). Proto recursion must still detect static `foo`; baz dropped.
        let graph = ns_graph(
            "import * as m from \"./lib\"\nfn wrap() { return m.foo() }\n",
            LIB_ABC_HELPER,
        );
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "foo"), "m.foo inside wrap → foo kept");
        assert!(kept(&res, 1, "helper"), "foo's helper kept");
        assert!(!kept(&res, 1, "baz"), "baz dropped (proto recursion works)");
        assert!(res.report.pins.is_empty());
    }

    // ---- 2.2 enhancement: namespace method-call receiver tracking ---------------

    #[test]
    fn classify_method_call_with_one_arg_is_static() {
        // `m.foo(arg)` lowers to GET_GLOBAL m; <arg ops>; CALL_METHOD foo 1 — the
        // forward receiver sim must see `m` as the receiver beneath its one arg.
        let c = chunk("import * as m from \"./lib\"\nlet arg = 1\nlet r = m.foo(arg)\n");
        match classify_namespace_use(&c, "m") {
            NamespaceUse::Static(props) => {
                assert!(props.contains(&rc("foo")));
                assert_eq!(props.len(), 1);
            }
            other => panic!("expected Static({{foo}}), got {other:?}"),
        }
    }

    #[test]
    fn classify_method_call_with_two_args_is_static() {
        // `m.foo(a, b)` → receiver beneath two args.
        let c = chunk("import * as m from \"./lib\"\nlet a = 1\nlet b = 2\nlet r = m.foo(a, b)\n");
        match classify_namespace_use(&c, "m") {
            NamespaceUse::Static(props) => assert!(props.contains(&rc("foo"))),
            other => panic!("expected Static, got {other:?}"),
        }
    }

    #[test]
    fn classify_nested_method_calls_both_static() {
        // `m.foo(m.bar())` → TWO independent sites, BOTH static: foo (outer receiver
        // beneath its one arg) and bar (inner receiver, zero args).
        let c = chunk("import * as m from \"./lib\"\nlet r = m.foo(m.bar())\n");
        match classify_namespace_use(&c, "m") {
            NamespaceUse::Static(props) => {
                assert!(props.contains(&rc("foo")), "outer foo static");
                assert!(props.contains(&rc("bar")), "inner bar static");
                assert_eq!(props.len(), 2);
            }
            other => panic!("expected Static({{foo, bar}}), got {other:?}"),
        }
    }

    #[test]
    fn classify_branch_in_args_pins_whole() {
        // `m.foo(cond ? a : b)` — a jump inside the args → the forward sim bails
        // conservatively (sound under-shake) → Dynamic.
        let c = chunk(
            "import * as m from \"./lib\"\nlet c = true\nlet a = 1\nlet b = 2\nlet r = m.foo(c ? a : b)\n",
        );
        assert!(
            matches!(classify_namespace_use(&c, "m"), NamespaceUse::Dynamic { .. }),
            "branch in args must conservatively pin whole"
        );
    }

    #[test]
    fn namespace_method_call_args_shakes_unused() {
        // Integration: entry calls only m.foo(arg); lib keeps foo (+helper), drops baz.
        let graph = ns_graph(
            "import * as m from \"./lib\"\nlet arg = 1\nlet r = m.foo(arg)\n",
            LIB_ABC_HELPER,
        );
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "foo"), "m.foo(arg) → foo kept");
        assert!(kept(&res, 1, "helper"), "foo's transitive helper kept");
        assert!(!kept(&res, 1, "baz"), "unaccessed baz dropped");
        assert!(
            res.report.pins.is_empty(),
            "static method-call use must not pin"
        );
    }

    #[test]
    fn namespace_branch_in_args_pins_whole_integration() {
        // Integration of test 4: m.foo(cond ? a : b) → whole-module pin + PinReason.
        let graph = ns_graph(
            "import * as m from \"./lib\"\nlet c = true\nlet a = 1\nlet b = 2\nlet r = m.foo(c ? a : b)\n",
            LIB_ABC_HELPER,
        );
        let res = compute_reachable(&graph);
        for name in ["foo", "bar", "baz", "helper"] {
            assert!(kept(&res, 1, name), "branch-in-args pins whole: {name} kept");
        }
        let pin = res
            .report
            .pins
            .iter()
            .find(|p| p.module == 1)
            .expect("lib pinned whole");
        assert_eq!(&*pin.alias, "m");
    }

    #[test]
    fn entry_module_kept_whole() {
        // The entry's own unreferenced fn is STILL kept (entry never shaken).
        let entry = chunk(
            r#"
            fn never_called() { 42 }
            let x = 1
        "#,
        );
        let graph = vec![ModuleNode {
            key: "entry".into(),
            chunk: entry,
            edges: vec![],
        }];
        let res = compute_reachable(&graph);
        assert!(
            kept(&res, 0, "never_called"),
            "entry's unreferenced fn is kept whole"
        );
        assert!(kept(&res, 0, "x"));
        assert!(
            res.report.dropped[0].names.is_empty(),
            "entry drops nothing"
        );
    }

    #[test]
    fn cross_module_fixpoint() {
        // A imports useB from B; B's useB imports useC from C (B's kept binding
        // references C's export); C also has unusedC.
        //
        // Note: these are bytecode-level graphs. We model the chain via edges. The
        // entry (A) imports useB from B; B has an edge pulling useC from C. So the
        // fixpoint must keep useC and drop unusedC.
        let entry_a = chunk(r#"let x = 1"#);
        let mod_b = chunk(
            r#"
            fn useB() { useC() }
            fn unusedB() { 0 }
        "#,
        );
        let mod_c = chunk(
            r#"
            fn useC() { 1 }
            fn unusedC() { 2 }
        "#,
        );
        let graph = vec![
            ModuleNode {
                key: "A".into(),
                chunk: entry_a,
                edges: vec![ImportEdge::Named {
                    target: 1,
                    names: vec![rc("useB")],
                }],
            },
            ModuleNode {
                key: "B".into(),
                chunk: mod_b,
                edges: vec![ImportEdge::Named {
                    target: 2,
                    names: vec![rc("useC")],
                }],
            },
            ModuleNode {
                key: "C".into(),
                chunk: mod_c,
                edges: vec![],
            },
        ];
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "useB"), "B.useB kept");
        assert!(!kept(&res, 1, "unusedB"), "B.unusedB dropped");
        assert!(kept(&res, 2, "useC"), "C.useC kept (transitive)");
        assert!(!kept(&res, 2, "unusedC"), "C.unusedC dropped");
    }

    /// Build the same shake graph twice; assert the canonical [`ShakeReport::digest`] is
    /// (a) byte-identical across the two builds and (b) non-zero (there ARE drops).
    fn headline_graph() -> Vec<ModuleNode> {
        let entry = chunk(r#"let x = 1"#);
        let lib = chunk(
            r#"
            fn h() { 42 }
            fn used() { h() }
            fn unused() { 99 }
        "#,
        );
        vec![
            ModuleNode {
                key: "entry.as".into(),
                chunk: entry,
                edges: vec![ImportEdge::Named {
                    target: 1,
                    names: vec![rc("used")],
                }],
            },
            ModuleNode {
                key: "lib.as".into(),
                chunk: lib,
                edges: vec![],
            },
        ]
    }

    #[test]
    fn digest_is_reproducible_and_nonzero() {
        let d1 = compute_reachable(&headline_graph()).report.digest();
        let d2 = compute_reachable(&headline_graph()).report.digest();
        assert_eq!(d1, d2, "same source → byte-identical digest");
        assert_ne!(d1, [0u8; 32], "a report with drops must hash to a non-zero digest");
    }

    #[test]
    fn digest_changes_when_drops_change() {
        // A graph that drops `unused` vs. an otherwise-identical graph that imports it
        // too (so `unused` is kept) must produce DIFFERENT digests — the digest reflects
        // the shake outcome, not just the module set.
        let d_with_drop = compute_reachable(&headline_graph()).report.digest();

        let entry = chunk(r#"let x = 1"#);
        let lib = chunk(
            r#"
            fn h() { 42 }
            fn used() { h() }
            fn unused() { 99 }
        "#,
        );
        let graph_no_drop = vec![
            ModuleNode {
                key: "entry.as".into(),
                chunk: entry,
                edges: vec![ImportEdge::Named {
                    target: 1,
                    names: vec![rc("used"), rc("unused")], // now both kept → no drops
                }],
            },
            ModuleNode {
                key: "lib.as".into(),
                chunk: lib,
                edges: vec![],
            },
        ];
        let d_no_drop = compute_reachable(&graph_no_drop).report.digest();
        assert_ne!(
            d_with_drop, d_no_drop,
            "a different drop set must change the digest"
        );
    }

    #[test]
    fn digest_independent_of_graph_order_by_key() {
        // The digest sorts by LOGICAL KEY, so it must not depend on the in-memory `pins`/
        // `dropped` Vec order. We build a report, then a permuted clone of its collections,
        // and assert the digest matches. (Construct ShakeReports directly to isolate the
        // sort from graph traversal.)
        let base = compute_reachable(&headline_graph()).report;
        // A reversed-order clone of the same logical content.
        let mut dropped_rev: Vec<ModuleDrops> = base
            .dropped
            .iter()
            .map(|d| ModuleDrops {
                module: d.module,
                key: d.key.clone(),
                names: d.names.clone(),
            })
            .collect();
        dropped_rev.reverse();
        let permuted = ShakeReport {
            dropped: dropped_rev,
            pins: Vec::new(),
        };
        assert_eq!(
            base.digest(),
            permuted.digest(),
            "digest must be independent of in-memory collection order"
        );
    }

    #[test]
    fn digest_independent_of_pin_order() {
        // The digest sorts PINS by (pinned key, importer key, alias, span). Build two
        // reports with the SAME two pins in OPPOSITE Vec orders and assert an identical
        // digest — guarding the pin sort the way `dropped` is guarded above. (Constructed
        // directly so the test isolates the sort from graph traversal.)
        let mk_pin = |key: &str, importer_key: &str, alias: &str, span: Span| PinReason {
            module: 0, // index is NOT part of the digest — only logical keys are
            key: key.into(),
            importer: 0,
            importer_key: importer_key.into(),
            alias: rc(alias),
            reason: format!("namespace `{alias}` used dynamically or escapes"),
            span,
            location: None, // excluded from the digest
        };
        let pins_a = vec![
            mk_pin("a_util.as", "app.as", "m", Span::new(10, 11)),
            mk_pin("z_util.as", "app.as", "n", Span::new(20, 21)),
        ];
        let pins_b = vec![
            mk_pin("z_util.as", "app.as", "n", Span::new(20, 21)),
            mk_pin("a_util.as", "app.as", "m", Span::new(10, 11)),
        ];
        let report_a = ShakeReport {
            dropped: Vec::new(),
            pins: pins_a,
        };
        let report_b = ShakeReport {
            dropped: Vec::new(),
            pins: pins_b,
        };
        assert_eq!(
            report_a.digest(),
            report_b.digest(),
            "digest must be independent of the in-memory pin order"
        );
        // Sanity: a report WITH pins is distinct from the empty report (the pins hash).
        let empty = ShakeReport::default();
        assert_ne!(
            report_a.digest(),
            empty.digest(),
            "pins must actually contribute to the digest"
        );
    }
}


