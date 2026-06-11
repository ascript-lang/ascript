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
//! (`crate::worker::dispatch`) — we reuse its `pub(crate)` helpers rather than
//! re-walking bytecode here.

use crate::vm::chunk::Chunk;
use crate::worker::dispatch::{
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
    /// `import * as m from <target>` — Task 2.1 conservative rule: pin the WHOLE
    /// target module (every top-level name becomes a root). Task 2.2 will refine
    /// namespace imports to allow static-only shaking.
    Namespace {
        /// The target module's index in the graph.
        target: usize,
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

/// A minimal shake report (Task 2.1 skeleton). Task 2.4 will add the digest +
/// stderr printing; for now it records, per module, the names DROPPED (all top-level
/// names minus the keep-set) and any "pinned whole" reasons — deterministically
/// ordered for stable diagnostics.
#[derive(Debug, Default)]
pub struct ShakeReport {
    /// Per-module dropped top-level names (index → sorted names), in the order the
    /// modules appear in the graph.
    pub dropped: Vec<ModuleDrops>,
    /// Reasons a module was pinned whole (e.g. a namespace import), in graph order.
    pub pins: Vec<PinReason>,
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
    /// A human-readable reason (e.g. "namespace import").
    pub reason: String,
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
            // A binding-producing statement: its refs are pulled by the closure when
            // the bound NAME is reachable, not here.
            continue;
        }
        collect_range_refs(chunk, start, end, &mut out);
    }
    out
}

/// Does the top-level statement spanning `[start, end)` end in a `DefineGlobal`? Such
/// a statement is a binding (its run is `<value-producing ops>; DEFINE_GLOBAL name`);
/// anything else at top level is a side-effect statement. We find the LAST decodable
/// instruction in the range and test its opcode.
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
    matches!(last, Some(Op::DefineGlobal))
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

    // Import roots + namespace pins. An edge's contribution is static (a top-level
    // import always runs), so these are computed once, up front.
    let mut import_roots: Vec<Vec<Rc<str>>> = vec![Vec::new(); n];
    let mut pinned_whole: Vec<bool> = vec![false; n];
    for node in graph {
        for edge in &node.edges {
            match edge {
                ImportEdge::Named { target, names } => {
                    if let Some(slot) = import_roots.get_mut(*target) {
                        slot.extend(names.iter().cloned());
                    }
                }
                ImportEdge::Namespace { target } => {
                    if let Some(slot) = pinned_whole.get_mut(*target) {
                        *slot = true;
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
            pins.push(PinReason {
                module: i,
                key: node.key.clone(),
                reason: "namespace import".to_string(),
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

    #[test]
    fn namespace_import_pins_whole_module() {
        // import * as m from lib → ALL of lib kept, report records the pin.
        let entry = chunk(r#"let x = 1"#);
        let lib = chunk(
            r#"
            fn a() { 1 }
            fn b() { 2 }
            fn c() { 3 }
        "#,
        );
        let graph = vec![
            ModuleNode {
                key: "entry".into(),
                chunk: entry,
                edges: vec![ImportEdge::Namespace { target: 1 }],
            },
            ModuleNode {
                key: "lib".into(),
                chunk: lib,
                edges: vec![],
            },
        ];
        let res = compute_reachable(&graph);
        assert!(kept(&res, 1, "a"));
        assert!(kept(&res, 1, "b"));
        assert!(kept(&res, 1, "c"));
        // Nothing dropped from a pinned module.
        assert!(res.report.dropped[1].names.is_empty());
        // The pin is recorded.
        assert!(res.report.pins.iter().any(|p| p.module == 1));
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
}
