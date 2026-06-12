//! FUZZ — grammar-aware AScript source generator (the core asset).
//!
//! This module emits an unbounded stream of **valid, deterministic, run-to-completion**
//! AScript source programs. It is the engine behind the three-way differential property
//! test (`tests/property.rs`): `tree-walker == specialized-VM == generic-VM` over generated
//! programs. ANY disagreement is a guaranteed bug.
//!
//! Design (spec §3.1 / plan Task 4):
//! - **`arbitrary::Unstructured`-driven, recursion-budgeted.** Generation consumes an
//!   unstructured byte source so the SAME generator serves both `proptest` (in-tree,
//!   shrinking via a `Vec<u8>` strategy) and a future `cargo-fuzz` libFuzzer target
//!   (coverage-guided). A depth/size budget decremented per recursion guarantees
//!   termination and keeps programs runnable.
//! - **Scope-correct by construction.** A symbol environment threads through generation so
//!   every emitted identifier is in scope, every `const` is never reassigned, every call
//!   targets an existing function with matching arity, and `break`/`continue`/`return`
//!   appear only where legal. This maximizes the fraction that compiles + runs.
//! - **Deterministic-by-construction.** No clock/RNG/wall-time/unsorted-iteration/race
//!   output is ever emitted — the program's stdout is a pure function of its text, or the
//!   differential is meaningless (spec §6). Loops are bound-capped so programs always halt.
//! - **Edge-biased.** Weighted toward known divergence-prone regions: numeric boundaries
//!   (`i64::MIN/MAX`, `2^53±1`, `0`, mixed int/float, `/`/`%`/wrapping ops, bitwise `& | ^`),
//!   deep nesting, empty collections, shadowing/reassignment, and `match` over int
//!   literals/ranges/`_`.
//!
//! **Coverage (broadened by FUZZ Unit 2 — the differential only fuzzes the surface it
//! reaches).** Unit 1 covered the pre-NUM *core* surface (above). Unit 2 ADDED, each behind
//! a hard three-way stress run (see `tests/property.rs::stress_differential_many_seeds`):
//!   - **Arithmetic completeness:** `**` exponent (overflow-trap), bitwise/shift/complement
//!     `& | ^ << >> ~` (int-only, bounded shift amount), and exact **decimals** (`decimal.from`
//!     arithmetic, mixed with int).
//!   - **Composite literals + ops:** object literals `{k: v}` (member/index read, `len`), map
//!     literals `#{k: v}` (`map.get`/`len`, numeric-key MapKey canonicalization), array indexing,
//!     and `set.from` size. (Loops are numeric `for (i in 0..k)` ranges — deterministic, ordered;
//!     `for…of` over a collection is NOT emitted, to avoid any iteration-order dependence.)
//!   - **Classes / enums / ADT / rich `match`** (see `class_decl`/`enum_decl`/`class_expr`/
//!     `enum_expr`): typed fields/defaults/`init`/methods, `instanceof`, inheritance/`super`;
//!     unit + positional + named ADT variants, constructors, exhaustive `match` over each with
//!     positional/named/unit + wildcard patterns + Option-C fresh-binds.
//!   - **Closures + capture** (see `closure_capture_stmt`/`loop_closure_stmt`/`closure_expr`):
//!     arrow closures, BY-VALUE vs BY-REFERENCE capture (an immutable read vs a mutated
//!     accumulator observed from outside), PER-ITERATION loop-var freshness (a closure-per-
//!     iteration bag), and nested/curried/IIFE arrows.
//!   - **String templates `${…}`** (incl. nested, `template_expr`).
//!   - **`?`/`!` propagate/unwrap** over the tier-1 `[value, err]` model (always-ok `rok`/
//!     `rerr` helpers so the happy path runs to completion; see `unwrap_expr`/`propagate_stmt`).
//!
//! BUGS FOUND + FIXED in-branch via this broadened differential (Gate 0), each with a permanent
//! four-mode regression guard in `tests/vm_differential.rs`: (1) a compiler `loop_refresh_slots`
//! frame-leak — a closure in an INNER loop capturing a mutated OUTER-loop variable read `nil`
//! on the VM (the inner loop fresh-celled an unrelated slot shared with a nested closure param);
//! (2) the tree-walker `match` value pattern used Rust structural `PartialEq` instead of the
//! `==`-operator equality, so a `Decimal` subject did NOT match an int/float literal pattern
//! (the VM, compiling to `Op::Eq`, did); (3) the CST `ternary_ahead` scanner misparsed a
//! postfix propagate `?` as a ternary when a `:` followed at apparent depth 0 — inside a `#{…}`
//! map literal (`HashLBrace` was uncounted) or in a LATER statement's real ternary (the scan
//! didn't stop at a statement keyword).
//!
//! STILL NOT EMITTED (the next breadth follow-up; the differential cannot fuzz what it never
//! generates): interfaces + structural-`instanceof`; destructuring/spread/rest in let/params;
//! async/await/spawn/workers (deferred — nondeterministic scheduling, see spec §6); try/recover
//! (the `recover(fn(){…})` carry-forward bug); generators `fn*`/`yield`.
//!
//! Known BLIND SPOT (tracked): class/enum/`fn` declarations are emitted TOP-LEVEL only — the
//! generator cannot nest a declaration INSIDE a loop body. That pattern hid a real closure
//! frame-leak (a loop-nested METHOD param colliding with a captured outer-loop cell — Unit-2
//! bug #1's `MethodDecl` sibling, caught by REVIEW not the fuzzer; fixed + guarded by
//! `vm_loop_nested_method_closure_capture_matches_treewalker`). Letting `stmt()` nest a
//! declaration in a loop body would let the fuzzer reach this class of bug automatically.
//!
//! Crate-gated `#[cfg(any(test, fuzzing))]` (in `lib.rs`) so it compiles into `ascript`
//! ONLY for `cargo test` (and a `--cfg fuzzing` libFuzzer build) — never in a normal or
//! `--no-default-features` build, and `arbitrary` never enters the production graph.

use arbitrary::Unstructured;
use std::fmt::Write as _;

/// A generated AScript program, ready to feed the three engines.
#[derive(Debug, Clone)]
pub struct GenProgram {
    pub source: String,
}

/// Generate one valid, deterministic, run-to-completion AScript program from an
/// unstructured byte source. Never fails: when the byte source is exhausted, the
/// generator falls back to a fixed default at every choice point, so it always
/// returns a complete program (an exhausted `Unstructured` yields trivial-but-valid
/// programs rather than an error).
pub fn gen_program(u: &mut Unstructured) -> GenProgram {
    let mut g = Gen::new(u);
    g.program();
    GenProgram { source: g.out }
}

/// Convenience: generate a program from a raw seed byte slice (the proptest entry point).
pub fn gen_program_from_bytes(bytes: &[u8]) -> GenProgram {
    let mut u = Unstructured::new(bytes);
    gen_program(&mut u)
}

/// Generate a single deterministic EXPRESSION program: `print(<expr>)` where `<expr>`
/// is a pure, side-effect-free expression over fresh literals. Feeds the
/// expression-granularity differential (`assert_vm_matches_treewalker`-style checks).
pub fn gen_expr_program(u: &mut Unstructured) -> GenProgram {
    let mut g = Gen::new(u);
    g.out.push_str("print(");
    let e = g.expr(0);
    g.out.push_str(&e);
    g.out.push_str(")\n");
    GenProgram { source: g.out }
}

/// The maximum statement-nesting / expression-nesting budget. Kept well under the
/// runtime guards (`MAX_CALL_DEPTH`/`EXPR_NEST_LIMIT`) so a generated program never
/// trips the recursion guard non-deterministically — deep-nesting bias is bounded.
const MAX_DEPTH: u32 = 6;
/// Hard ceiling on emitted top-level statements (keeps programs small + fast).
const MAX_TOP_STMTS: u32 = 14;
/// Hard ceiling on a generated loop's iteration count (determinism + termination).
const MAX_LOOP_ITERS: i64 = 6;

/// A scope of in-scope bindings (name + mutability), pushed/popped as blocks open/close.
#[derive(Clone)]
struct Scope {
    /// `(name, mutable)` — `const`/loop-vars/params are immutable; `let` is mutable.
    vars: Vec<(String, bool)>,
}

/// A declared function: name + arity (so calls match).
#[derive(Clone)]
struct FnSig {
    name: String,
    arity: usize,
    /// True if the fn prints its first argument — used as a `defer` target so the
    /// differential bites on LIFO drain ORDER (a wrong drain order → divergent stdout).
    prints_arg: bool,
}

/// A declared class: its name, the number of positional `init` params (so construction
/// matches), and an int method name. `init` always assigns the required field from its single
/// param; the other field carries a default, so every field is readable after construction.
#[derive(Clone)]
struct ClassSig {
    name: String,
    /// `init` arity (0 or 1 — we keep construction trivial + deterministic).
    init_arity: usize,
    /// A method name returning an int (callable on an instance).
    method: String,
}

/// A declared ADT enum + its variants (so constructions + exhaustive matches are valid).
#[derive(Clone)]
struct EnumSig {
    name: String,
    variants: Vec<VariantSig>,
}

/// One enum variant: unit, positional-payload, or named-payload.
#[derive(Clone)]
enum VariantSig {
    /// `Name` — a payload-less unit variant.
    Unit(String),
    /// `Name(int, int)` — positional `int` payload of the given arity (1 or 2).
    Positional(String, usize),
    /// `Name(a: int, b: int)` — named `int` payload with these field names.
    Named(String, Vec<String>),
}

struct Gen<'a, 'b> {
    u: &'a mut Unstructured<'b>,
    out: String,
    /// Lexical scopes, innermost last.
    scopes: Vec<Scope>,
    /// Top-level functions declared so far (callable from anywhere below).
    fns: Vec<FnSig>,
    /// Top-level classes declared so far (constructible / `instanceof`-checkable below).
    classes: Vec<ClassSig>,
    /// Top-level enums declared so far (constructible + matchable below).
    enums: Vec<EnumSig>,
    /// Monotonic counter for fresh identifier names (guarantees no accidental clash).
    counter: u32,
    /// True while emitting inside a loop body (so `break`/`continue` are legal).
    in_loop: bool,
    /// True while emitting inside a function body (so `return` is legal).
    in_fn: bool,
    /// PROTECTED loop-counter names (the `while (cN < K)` counters). They are declared
    /// `mutable` so the generator's own guaranteed-progress `cN = cN + 1` is legal, but they
    /// are NEVER offered to `assign_stmt` as a target — otherwise a body statement could
    /// reassign the counter to a huge value (`w1 = 1000000 -% 2^53`), making the loop run
    /// ~quadrillions of iterations (a generator-induced near-hang, NOT an engine bug). This
    /// is the loop-termination invariant (spec §6 / Task 4: bound loop counts).
    protected: Vec<String>,
}

impl<'a, 'b> Gen<'a, 'b> {
    fn new(u: &'a mut Unstructured<'b>) -> Self {
        Gen {
            u,
            out: String::new(),
            scopes: vec![Scope { vars: Vec::new() }],
            fns: Vec::new(),
            classes: Vec::new(),
            enums: Vec::new(),
            counter: 0,
            in_loop: false,
            in_fn: false,
            protected: Vec::new(),
        }
    }

    // ---- unstructured-source helpers (all infallible: fall back on exhaustion) ----

    /// A byte in `0..n` (n>0). On exhaustion returns 0 (deterministic fallback).
    fn choice(&mut self, n: u32) -> u32 {
        if n <= 1 {
            return 0;
        }
        self.u.int_in_range(0..=n - 1).unwrap_or(0)
    }

    /// A bool (false on exhaustion).
    fn flag(&mut self) -> bool {
        self.u.arbitrary().unwrap_or(false)
    }

    /// A fresh, guaranteed-unique identifier.
    fn fresh(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{}", self.counter)
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope { vars: Vec::new() });
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
    fn declare(&mut self, name: &str, mutable: bool) {
        self.scopes
            .last_mut()
            .unwrap()
            .vars
            .push((name.to_string(), mutable));
    }

    /// All in-scope variable names (any mutability).
    fn all_vars(&self) -> Vec<String> {
        self.scopes
            .iter()
            .flat_map(|s| s.vars.iter().map(|(n, _)| n.clone()))
            .collect()
    }
    /// In-scope MUTABLE variable names that are legal ASSIGNMENT targets — excludes the
    /// protected loop counters (see `protected`), so a generated loop ALWAYS terminates.
    fn mut_vars(&self) -> Vec<String> {
        self.scopes
            .iter()
            .flat_map(|s| s.vars.iter().filter(|(_, m)| *m).map(|(n, _)| n.clone()))
            .filter(|n| !self.protected.contains(n))
            .collect()
    }

    // ---- program / statements ----

    fn program(&mut self) {
        // Stdlib imports the broadened generator relies on (decimal arithmetic, array/map/
        // set construction + access). Always emitted so any later production can reference
        // them; an unused import is a lint Warning, never a runtime error, so this keeps the
        // generated source valid + deterministic regardless of which productions fire.
        for (alias, module) in [
            ("decimal", "std/decimal"),
            ("array", "std/array"),
            ("map", "std/map"),
            ("set", "std/set"),
        ] {
            let _ = writeln!(self.out, "import * as {alias} from \"{module}\"");
            let _ = alias;
        }
        // FUZZ Unit 2 — propagate/unwrap helpers. Two top-level result-returning fns (the
        // tier-1 `[value, err]` model): `rok(x)` is ALWAYS-ok (`[x, nil]`) and `rerr(x)`
        // returns ok only when `x >= 0` (`[x, nil]` else `[nil, "neg"]`). The generator uses
        // `rok(...)!` (always succeeds → deterministic int) and `rok(...)?`/`rerr(...)?`
        // inside generated fn bodies (the `?` early-returns the `[nil, err]` pair). Both
        // engines share the `?`/`!` lowering, so any divergence is a real propagate/unwrap bug.
        self.out.push_str("fn rok(x) {\n    return [x, nil]\n}\n");
        self.out
            .push_str("fn rerr(x) {\n    if (x >= 0) {\n        return [x, nil]\n    }\n    return [nil, \"neg\"]\n}\n");
        // A handful of top-level classes + enums (so later code can construct/match them).
        // Declared BEFORE the fns/statements so every construction + match is in scope.
        let n_classes = self.choice(3); // 0..2
        for _ in 0..n_classes {
            self.class_decl();
        }
        let n_enums = self.choice(3); // 0..2
        for _ in 0..n_enums {
            self.enum_decl();
        }
        // FUZZ DEFER — emit 1..2 "print-fn" helpers that print their single int arg.
        // These serve as `defer` targets where the differential bites on LIFO drain ORDER
        // (a wrong drain order → divergent stdout). Declared before the general fns so they
        // are in scope for every subsequent defer emission.
        let n_print_fns = 1 + self.choice(2); // 1..2 print fns
        for _ in 0..n_print_fns {
            self.print_fn_decl();
        }
        // A handful of top-level functions first (so later code can call them).
        let n_fns = self.choice(3); // 0..3
        for _ in 0..n_fns {
            self.fn_decl();
        }
        let n = 1 + self.choice(MAX_TOP_STMTS);
        for _ in 0..n {
            self.stmt(0);
        }
        // Always end with a deterministic print of a fresh expression so EVERY program
        // produces observable output (an empty program would make the differential vacuous).
        let e = self.expr(0);
        let _ = writeln!(self.out, "print({e})");
    }

    /// Emit a top-level function declaration `fn name(p0, p1) { ... return e }`.
    fn fn_decl(&mut self) {
        let name = self.fresh("f");
        let arity = self.choice(3) as usize; // 0..2 params
        let params: Vec<String> = (0..arity).map(|_| self.fresh("p")).collect();
        let _ = write!(self.out, "fn {name}(");
        self.out.push_str(&params.join(", "));
        self.out.push_str(") {\n");

        self.push_scope();
        let prev_fn = self.in_fn;
        let prev_loop = self.in_loop;
        self.in_fn = true;
        self.in_loop = false;
        for p in &params {
            self.declare(p, false); // params are immutable bindings here
        }
        // A couple of body statements.
        let body_n = self.choice(3);
        for _ in 0..body_n {
            self.stmt(1);
        }
        // Always a return so the fn yields a value usable in arithmetic.
        let e = self.expr(1);
        let _ = writeln!(self.out, "    return {e}");
        self.in_fn = prev_fn;
        self.in_loop = prev_loop;
        self.pop_scope();
        self.out.push_str("}\n");

        // Register AFTER the body so a fn cannot (yet) call itself — keeps recursion
        // bounded by construction (no unbounded self-recursion in generated programs).
        self.fns.push(FnSig { name, arity, prints_arg: false });
    }

    /// FUZZ Unit 2 — emit a top-level class declaration. The class has one required int
    /// field (assigned in `init` from the single param), one defaulted int field, and one
    /// method returning an int over `self`. Roughly half the time it `extends` an
    /// already-declared class with `super.init`/`super.<method>` so inheritance + `super` +
    /// the method-resolution-order are exercised. All field types are `number` and every
    /// observable read is an int, so output stays deterministic.
    fn class_decl(&mut self) {
        let name = self.fresh("C");
        let field_a = self.fresh("fa");
        let field_b = self.fresh("fb");
        let method = self.fresh("m");
        let def_b = self.int_literal();

        // Optionally inherit from a previously-declared class.
        let parent = if !self.classes.is_empty() && self.flag() {
            let idx = self.choice(self.classes.len() as u32) as usize;
            Some(self.classes[idx].clone())
        } else {
            None
        };

        match &parent {
            Some(p) => {
                let _ = writeln!(self.out, "class {name} extends {} {{", p.name);
            }
            None => {
                let _ = writeln!(self.out, "class {name} {{");
            }
        }
        let _ = writeln!(self.out, "    {field_a}: number");
        let _ = writeln!(self.out, "    {field_b}: number = {def_b}");
        // init: assign the required field; chain to super.init when inheriting.
        self.out.push_str("    fn init(x) {\n");
        if let Some(p) = &parent {
            if p.init_arity == 1 {
                self.out.push_str("        super.init(x)\n");
            } else {
                self.out.push_str("        super.init()\n");
            }
        }
        let _ = writeln!(self.out, "        self.{field_a} = x");
        self.out.push_str("    }\n");
        // method: an int over self (and super's method when inheriting).
        let _ = writeln!(self.out, "    fn {method}() {{");
        match &parent {
            Some(p) => {
                let _ = writeln!(
                    self.out,
                    "        return self.{field_a} + self.{field_b} + super.{}()",
                    p.method
                );
            }
            None => {
                let _ = writeln!(self.out, "        return self.{field_a} + self.{field_b}");
            }
        }
        self.out.push_str("    }\n");
        self.out.push_str("}\n");

        self.classes.push(ClassSig {
            name,
            init_arity: 1,
            method,
        });
    }

    /// FUZZ Unit 2 — emit a top-level ADT enum declaration with a mix of unit, positional-
    /// payload, and named-payload variants (every payload field is `int`). Registered so the
    /// generator can construct each variant + write an EXHAUSTIVE `match` over it.
    fn enum_decl(&mut self) {
        let name = self.fresh("E");
        let n_variants = 2 + self.choice(3); // 2..4 variants
        let mut variants = Vec::new();
        let _ = writeln!(self.out, "enum {name} {{");
        for _ in 0..n_variants {
            let vname = self.fresh("V");
            let v = match self.choice(3) {
                0 => VariantSig::Unit(vname.clone()),
                1 => {
                    let arity = 1 + self.choice(2) as usize; // 1..2 positional fields
                    VariantSig::Positional(vname.clone(), arity)
                }
                _ => {
                    let n = 1 + self.choice(2) as usize; // 1..2 named fields
                    let fields: Vec<String> = (0..n).map(|_| self.fresh("nf")).collect();
                    VariantSig::Named(vname.clone(), fields)
                }
            };
            match &v {
                VariantSig::Unit(n) => {
                    let _ = writeln!(self.out, "    {n},");
                }
                VariantSig::Positional(n, arity) => {
                    let tys = vec!["int"; *arity].join(", ");
                    let _ = writeln!(self.out, "    {n}({tys}),");
                }
                VariantSig::Named(n, fields) => {
                    let decls: Vec<String> =
                        fields.iter().map(|f| format!("{f}: int")).collect();
                    let _ = writeln!(self.out, "    {n}({}),", decls.join(", "));
                }
            }
            variants.push(v);
        }
        self.out.push_str("}\n");
        self.enums.push(EnumSig { name, variants });
    }

    /// Emit one statement at the given nesting `depth`.
    fn stmt(&mut self, depth: u32) {
        // At/over budget, only emit a trivial leaf statement (a print).
        if depth >= MAX_DEPTH {
            let e = self.expr(depth);
            let _ = writeln!(self.out, "{:indent$}print({e})", "", indent = (depth * 4) as usize);
            return;
        }
        // Weighted choice over statement kinds. Assignment/break/continue/return are
        // gated on legality; we re-roll to a safe default when illegal.
        // Choices 13..15 are the DEFER axis (Gate 15): a modest weight (~3/16) so other
        // statements still dominate and the generator explores a broad surface.
        let pick = self.choice(16);
        match pick {
            0 | 1 => self.let_stmt(depth),
            2 => self.const_stmt(depth),
            3 if !self.mut_vars().is_empty() => self.assign_stmt(depth),
            4 => self.if_stmt(depth),
            5 => self.while_stmt(depth),
            6 => self.for_stmt(depth),
            7 => self.print_stmt(depth),
            8 => self.match_stmt(depth),
            9 if self.in_fn => self.propagate_stmt(depth), // `?` inside a fn body
            10 => self.closure_capture_stmt(depth),
            11 => self.loop_closure_stmt(depth),
            // DEFER axis — three shapes covering §3.3's matrix:
            //   13: plain `defer <print_fn>(arg)` — drain ORDER is observable
            //   14: defer arrow-IIFE touching a mutable local (capture-by-value)
            //   15: `defer await <print_fn>(arg)` — the §3.4 await form (await on a
            //       sync fn is identity, so this stays deterministic)
            13 => self.defer_stmt(depth),
            14 => self.defer_iife_stmt(depth),
            15 => self.defer_await_stmt(depth),
            _ => self.print_stmt(depth),
        }
    }

    /// FUZZ Unit 2 — a propagate `?` statement, legal ONLY inside a fn body (the `?` early-
    /// returns the `[nil, err]` pair). Uses the ALWAYS-ok `rok(...)` helper so the propagate
    /// branch is never actually taken (the fn's normal control flow + return are unchanged) —
    /// this exercises the `ExprKind::Try` lowering on the happy path deterministically. Binds
    /// the unwrapped int to a fresh local usable downstream.
    fn propagate_stmt(&mut self, depth: u32) {
        // Three shapes, all happy-path deterministic (`rok(..)?` never early-returns):
        //   0: the plain `let v = rok(x)?` binding.
        //   1: propagate-then-infix-then-ternary — `rok(x)? > k ? a : b` (repro A). The
        //      `?` MUST stay a postfix propagate (the next token `>` cannot begin an
        //      expression); the later `:` belongs to the trailing ternary. A naive
        //      `ternary_ahead` token-scan once fused these and rejected the program.
        //   2: propagate INSIDE a ternary then-branch — `c ? rok(x)? : k` (repro B). The
        //      inner `?` is propagate (the next token is the OUTER ternary's `:`).
        // Both 1 and 2 are the FUZZ Unit-2 disambiguation class; emitting them keeps the
        // four-mode differential standing guard over it.
        let name = self.fresh("pv");
        let x = self.int_atom();
        match self.choice(3) {
            1 => {
                let k = self.int_literal();
                let a = self.int_literal();
                let b = self.int_literal();
                self.indent(depth);
                let _ = writeln!(self.out, "let {name} = rok({x})? > {k} ? {a} : {b}");
            }
            2 => {
                // `c` is a deterministic int condition (truthy unless 0); both branches
                // are ints so the result type is stable.
                let c = self.int_literal();
                let k = self.int_literal();
                self.indent(depth);
                let _ = writeln!(self.out, "let {name} = {c} ? rok({x})? : {k}");
            }
            _ => {
                self.indent(depth);
                let _ = writeln!(self.out, "let {name} = rok({x})?");
            }
        }
        self.declare(&name, true);
    }

    /// FUZZ Unit 2 — closures + capture (KNOWN high-divergence: the resolver splits
    /// `captured && mutated` → a shared by-reference cell vs `captured && !mutated` → a
    /// by-value copy; `Op::Closure` copies the by-value slots into a fresh private cell).
    /// This emits BOTH shapes deterministically:
    ///   - a BY-VALUE capture of an immutable outer binding (read-only use), and
    ///   - a BY-REFERENCE capture: a mutable accumulator the closure increments, called
    ///     twice, with the accumulator ALSO read afterward (so the shared-cell write is
    ///     observable from the outside — the exact thing a wrong by-value/by-ref split would
    ///     break). All observables are ints → deterministic.
    fn closure_capture_stmt(&mut self, depth: u32) {
        let cap = self.fresh("cap"); // immutable outer (by-value capture)
        let acc = self.fresh("acc"); // mutable outer (by-reference capture)
        let f = self.fresh("clo");
        let g = self.fresh("clo");
        let cv = self.int_literal();
        self.indent(depth);
        let _ = writeln!(self.out, "const {cap} = {cv}");
        self.declare(&cap, false);
        self.indent(depth);
        let _ = writeln!(self.out, "let {acc} = 0");
        self.declare(&acc, true);
        // by-value capture closure: reads cap.
        self.indent(depth);
        let _ = writeln!(self.out, "let {f} = (x) => x + {cap}");
        self.declare(&f, true);
        // by-reference capture closure: mutates acc and returns it.
        self.indent(depth);
        let _ = writeln!(
            self.out,
            "let {g} = (n) => {{ {acc} = {acc} + n; return {acc} }}"
        );
        self.declare(&g, true);
        // Observe both: the by-value application and the shared-cell mutation (twice + read).
        let a0 = self.int_atom();
        self.indent(depth);
        let _ = writeln!(self.out, "print({f}({a0}))");
        let n0 = self.int_atom();
        let n1 = self.int_atom();
        self.indent(depth);
        let _ = writeln!(self.out, "print({g}({n0}))");
        self.indent(depth);
        let _ = writeln!(self.out, "print({g}({n1}))");
        self.indent(depth);
        let _ = writeln!(self.out, "print({acc})");
    }

    /// FUZZ Unit 2 — PER-ITERATION loop-var freshness (the subtlest capture case): collect a
    /// closure per loop iteration that captures the immutable loop variable, then call each.
    /// Because the loop var is captured-and-NOT-mutated, each closure must see its OWN
    /// iteration's value (a by-value copy into a fresh private cell per iteration). A wrong
    /// capture would make all closures observe the final value. The loop bound is small +
    /// fixed and each closure returns `loopvar * k` (an int) → deterministic, ordered output.
    fn loop_closure_stmt(&mut self, depth: u32) {
        let bag = self.fresh("bag");
        let iv = self.fresh("i");
        let k = 2 + self.choice(3); // 2..4 iterations
        let mult = 1 + self.choice(5);
        self.indent(depth);
        let _ = writeln!(self.out, "let {bag} = []");
        self.declare(&bag, true);
        self.indent(depth);
        let _ = writeln!(self.out, "for ({iv} in 0..{k}) {{");
        self.indent(depth + 1);
        let _ = writeln!(self.out, "array.push({bag}, () => {iv} * {mult})");
        self.indent(depth);
        self.out.push_str("}\n");
        // Call each captured closure in order → must reflect the per-iteration loop value.
        for idx in 0..k {
            self.indent(depth);
            let _ = writeln!(self.out, "print({bag}[{idx}]())");
        }
    }

    // ---- DEFER axis (Gate 15, FUZZ §8.3) ----------------------------------------
    //
    // Three emission shapes covering §3.3's frame-exit matrix:
    //   1. `defer <print_fn>(arg)` — normal return path; LIFO drain order is
    //      observable in stdout → the differential catches any wrong-order drain.
    //   2. `defer (() => { let t = mut_local; print(t) })()` — arrow-IIFE that
    //      reads a mutable local (capture-by-value interplay, §3.1 subtlety).
    //   3. `defer await <print_fn>(arg)` — the §3.4 `await` form applied to a SYNC
    //      fn; `await` on a non-future is identity, so output stays deterministic.
    //
    // Defers in loop bodies, nested fn bodies, and propagating bodies arise
    // NATURALLY: `defer_stmt` / `defer_iife_stmt` / `defer_await_stmt` are called
    // from `stmt()`, which is already called inside `for_stmt`, `while_stmt`,
    // `fn_decl`, and `propagate_stmt`'s host fn — no extra loop/fn wrapping needed.
    //
    // All three helpers emit valid AScript regardless of whether `print_fns()` is
    // empty (they fall back to a plain `print(int_atom())` defer, which still
    // pushes/drains one entry and increments both counters).

    /// Return the subset of declared fns that print their single arg.
    fn print_fns(&self) -> Vec<FnSig> {
        self.fns
            .iter()
            .filter(|f| f.prints_arg)
            .cloned()
            .collect()
    }

    /// Emit a top-level 1-arg fn that prints its arg and returns it. Registered with
    /// `prints_arg: true` so `defer_stmt` / `defer_await_stmt` can target it — the
    /// observable `print` makes the differential catch a wrong LIFO drain order.
    fn print_fn_decl(&mut self) {
        let name = self.fresh("pf");
        let param = self.fresh("x");
        let _ = writeln!(self.out, "fn {name}({param}) {{");
        let _ = writeln!(self.out, "    print({param})");
        let _ = writeln!(self.out, "    return {param}");
        self.out.push_str("}\n");
        self.fns.push(FnSig {
            name,
            arity: 1,
            prints_arg: true,
        });
    }

    /// DEFER shape 1 — `defer <print_fn>(arg)`: a plain deferred call to a print fn.
    /// Emits 1..3 defers so the LIFO drain order (last-registered runs first) produces
    /// a visible stdout sequence that the differential can catch if wrong.
    fn defer_stmt(&mut self, depth: u32) {
        let pfs = self.print_fns();
        let n = 1 + self.choice(3); // 1..3 defers
        for i in 0..n {
            self.indent(depth);
            if let Some(pf) = pfs.first() {
                // Use the first print fn; vary the arg by iteration so each print line
                // is distinct (LIFO order has observable content, not just count).
                let arg = self.int_atom();
                // Mix in the iteration index to make args distinct across the n defers
                // even when int_atom returns the same literal.
                if i == 0 {
                    let _ = writeln!(self.out, "defer {}({arg})", pf.name);
                } else {
                    let _ = writeln!(self.out, "defer {}({i} + {arg})", pf.name);
                }
            } else {
                // No print fn yet (very early in program) — emit a plain print defer.
                let e = self.int_atom();
                let _ = writeln!(self.out, "defer print({e})");
            }
        }
    }

    /// DEFER shape 2 — `defer (() => { let t = <local>; print(t) })()`: an arrow-IIFE
    /// that reads a mutable local through the capture (exercises capture-by-value
    /// inside a deferred IIFE body, §3.1 capture timing). The local is read at IIFE
    /// construction time (the arrow captures the binding value then), so the value
    /// observed at drain time reflects the local's value at defer-statement time — not
    /// at drain time. This is the by-value capture subtlety the generator already
    /// exercises for closures; `defer` adds the "runs at frame exit" dimension.
    fn defer_iife_stmt(&mut self, depth: u32) {
        self.indent(depth);
        let e = self.int_atom();
        // The IIFE captures `e` by value (e is an expression, not a binding, so the
        // IIFE body uses a fresh local `t` to hold it — making the capture explicit
        // and exercising the let-in-arrow-body path).
        let _ = writeln!(self.out, "defer (() => {{ let t = {e}; print(t) }})()");
    }

    /// DEFER shape 3 — `defer await <print_fn>(arg)`: the §3.4 first-class `await`
    /// form. Targets a sync print fn; `await` on a non-future is identity (the
    /// language-wide rule), so the program stays deterministic. This exercises the
    /// `awaited` flag in the defer entry and the `DeferKind::Call { awaited: true }`
    /// drain path without requiring a real async fn (which would introduce
    /// nondeterministic scheduling, violating spec §6).
    fn defer_await_stmt(&mut self, depth: u32) {
        self.indent(depth);
        let pfs = self.print_fns();
        if let Some(pf) = pfs.first() {
            let arg = self.int_atom();
            let _ = writeln!(self.out, "defer await {}({arg})", pf.name);
        } else {
            // Fallback: `defer await print(e)` — `print` is a builtin, non-async;
            // `await print(v)` is identity. Still exercises the `await` defer path.
            let e = self.int_atom();
            let _ = writeln!(self.out, "defer await print({e})");
        }
    }

    fn indent(&mut self, depth: u32) {
        for _ in 0..depth {
            self.out.push_str("    ");
        }
    }

    fn let_stmt(&mut self, depth: u32) {
        let name = self.fresh("v");
        let e = self.expr(depth);
        self.indent(depth);
        let _ = writeln!(self.out, "let {name} = {e}");
        self.declare(&name, true);
    }

    fn const_stmt(&mut self, depth: u32) {
        let name = self.fresh("k");
        let e = self.expr(depth);
        self.indent(depth);
        let _ = writeln!(self.out, "const {name} = {e}");
        self.declare(&name, false);
    }

    fn assign_stmt(&mut self, depth: u32) {
        let muts = self.mut_vars();
        let idx = self.choice(muts.len() as u32) as usize;
        let target = muts[idx].clone();
        let e = self.expr(depth);
        self.indent(depth);
        let _ = writeln!(self.out, "{target} = {e}");
    }

    fn print_stmt(&mut self, depth: u32) {
        let e = self.expr(depth);
        self.indent(depth);
        let _ = writeln!(self.out, "print({e})");
    }

    fn if_stmt(&mut self, depth: u32) {
        let cond = self.bool_expr(depth);
        self.indent(depth);
        let _ = writeln!(self.out, "if ({cond}) {{");
        self.block(depth + 1);
        self.indent(depth);
        if self.flag() {
            self.out.push_str("} else {\n");
            self.block(depth + 1);
            self.indent(depth);
        }
        self.out.push_str("}\n");
    }

    fn while_stmt(&mut self, depth: u32) {
        // A bounded counting loop: `let cN = 0; while (cN < K) { ...; cN = cN + 1 }`.
        // The counter increment is ALWAYS emitted last so the loop is guaranteed to halt.
        let ctr = self.fresh("w");
        let iters = 1 + self.choice(MAX_LOOP_ITERS as u32);
        self.indent(depth);
        let _ = writeln!(self.out, "let {ctr} = 0");
        self.declare(&ctr, true);
        self.indent(depth);
        let _ = writeln!(self.out, "while ({ctr} < {iters}) {{");
        let prev = self.in_loop;
        self.in_loop = true;
        // Protect the counter from being reassigned anywhere in the body (the loop-
        // termination invariant): a body `{ctr} = <huge>` would otherwise blow the iteration
        // count to quadrillions. The generator's own `{ctr} = {ctr} + 1` below is emitted
        // directly (not via `assign_stmt`), so it is unaffected by the protection.
        self.protected.push(ctr.clone());
        self.block_no_close(depth + 1);
        self.protected.pop();
        self.in_loop = prev;
        // Guaranteed-progress increment.
        self.indent(depth + 1);
        let _ = writeln!(self.out, "{ctr} = {ctr} + 1");
        self.indent(depth);
        self.out.push_str("}\n");
    }

    fn for_stmt(&mut self, depth: u32) {
        // `for (vN in 0..K) { ... }` — a lazy bounded range, always terminates.
        let var = self.fresh("i");
        let hi = 1 + self.choice(MAX_LOOP_ITERS as u32);
        self.indent(depth);
        let _ = writeln!(self.out, "for ({var} in 0..{hi}) {{");
        self.push_scope();
        self.declare(&var, false); // loop var is immutable
        let prev = self.in_loop;
        self.in_loop = true;
        let n = 1 + self.choice(2);
        for _ in 0..n {
            self.stmt(depth + 1);
        }
        // Optionally a legal break/continue.
        if self.flag() {
            self.indent(depth + 1);
            let kw = if self.flag() { "break\n" } else { "continue\n" };
            self.out.push_str(kw);
        }
        self.in_loop = prev;
        self.pop_scope();
        self.indent(depth);
        self.out.push_str("}\n");
    }

    fn match_stmt(&mut self, depth: u32) {
        // `match (<int expr>) { 0 => ..., 1..3 => ..., _ => ... }`. Always exhaustive
        // via the trailing wildcard arm.
        let subj = self.int_expr(depth);
        self.indent(depth);
        let _ = writeln!(self.out, "match ({subj}) {{");
        // a value arm
        self.indent(depth + 1);
        let v0 = self.int_literal();
        let e0 = self.int_expr(depth + 1);
        let _ = writeln!(self.out, "{v0} => print({e0}),");
        // a range arm
        self.indent(depth + 1);
        let e1 = self.int_expr(depth + 1);
        let _ = writeln!(self.out, "10..20 => print({e1}),");
        // exhaustive wildcard
        self.indent(depth + 1);
        let e2 = self.int_expr(depth + 1);
        let _ = writeln!(self.out, "_ => print({e2}),");
        self.indent(depth);
        self.out.push_str("}\n");
    }

    /// A `{`-less block body (caller already emitted the `{`; caller emits the `}`).
    fn block_no_close(&mut self, depth: u32) {
        self.push_scope();
        let n = 1 + self.choice(3);
        for _ in 0..n {
            self.stmt(depth);
        }
        self.pop_scope();
    }

    /// A full block: statements only (caller wraps in `{ }`).
    fn block(&mut self, depth: u32) {
        self.push_scope();
        let n = 1 + self.choice(3);
        for _ in 0..n {
            self.stmt(depth);
        }
        self.pop_scope();
    }

    // ---- expressions (all return a String; never emit a statement) ----

    /// A general expression at nesting `depth`. Biased toward numeric edges.
    fn expr(&mut self, depth: u32) -> String {
        if depth >= MAX_DEPTH {
            return self.leaf_expr();
        }
        match self.choice(19) {
            0..=2 => self.int_expr(depth),
            3 | 4 => self.float_expr(depth),
            5 => self.bool_expr(depth),
            6 => self.string_expr(depth),
            7 => self.ternary_expr(depth),
            8 => self.call_expr(depth),
            9 => self.array_expr(depth),
            10 => self.leaf_expr(),
            11 => self.decimal_expr(depth),
            12 => self.composite_expr(depth),
            13 if !self.classes.is_empty() => self.class_expr(depth),
            14 if !self.enums.is_empty() => self.enum_expr(depth),
            15 => self.closure_expr(depth),
            16 => self.template_expr(depth),
            17 => self.unwrap_expr(depth),
            _ => self.int_expr(depth),
        }
    }

    /// FUZZ Unit 2 — an int-valued IIFE-style closure expression: an immediately-applied
    /// arrow, optionally CURRIED (an arrow returning an arrow), optionally capturing an
    /// in-scope int var (by-value, since the captured var is only read). Exercises arrow
    /// closure compilation, upvalue capture-by-value, and call dispatch inline at expression
    /// position. Always reduces to an int → deterministic.
    fn closure_expr(&mut self, _depth: u32) -> String {
        let a = self.int_atom();
        let b = self.int_atom();
        // Optional captured operand (an in-scope var or a literal).
        let cap = self.leaf_or_int();
        match self.choice(3) {
            // simple applied arrow
            0 => format!("((x) => x + {cap})({a})"),
            // curried arrow (a => b => a + b), applied twice
            1 => format!("((p) => (q) => p + q)({a})({b})"),
            // arrow with a block body that does a local let + return
            _ => format!("((x) => {{ let t = x * 2; return t + {cap} }})({a})"),
        }
    }

    /// An in-scope int-ish variable, or an int literal (used as a captured closure operand).
    fn leaf_or_int(&mut self) -> String {
        let vars = self.all_vars();
        if !vars.is_empty() && self.flag() {
            let idx = self.choice(vars.len() as u32) as usize;
            vars[idx].clone()
        } else {
            self.int_literal()
        }
    }

    /// FUZZ Unit 2 — a string template `${…}` expression (incl. NESTED templates). Exercises
    /// the template-interpolation path (the `template_interpolation_*` tests' surface) +
    /// nested-template parsing. Interpolates only int/bool/string sub-expressions (no map/set/
    /// float — those print deterministically too, but ints keep the observable simple). The
    /// result is a string; `len(...)` of it is NOT used (the string itself prints
    /// deterministically since every interpolated value prints deterministically).
    fn template_expr(&mut self, depth: u32) -> String {
        let a = self.int_atom();
        let b = self.int_atom();
        if depth < MAX_DEPTH && self.flag() {
            // Nested template: `${`inner ${a}`}`.
            format!("`out ${{`in ${{{a}}}`}} {b}`")
        } else {
            format!("`v=${{{a}}} w=${{{b}}} s=${{{a} + {b}}}`")
        }
    }

    /// FUZZ Unit 2 — an int-valued force-unwrap `!` expression over the tier-1 `[value, err]`
    /// model. Uses the ALWAYS-ok `rok(x)` helper so the unwrap NEVER panics (keeps the
    /// program run-to-completion); occasionally `rerr(x)` with a guaranteed-non-negative `x`
    /// so it is also always-ok. Both engines share the `Unwrap` lowering, so a divergence is
    /// a real propagate/unwrap bug. Deterministic int.
    fn unwrap_expr(&mut self, _depth: u32) -> String {
        let x = self.int_atom();
        if self.flag() {
            format!("rok({x})!")
        } else {
            // `rerr` with a known non-negative arg (a literal in 0..50) stays ok.
            let n = self.choice(50);
            format!("rerr({n})!")
        }
    }

    /// FUZZ Unit 2 — an int-valued expression over a declared class: construct an instance
    /// and either call its int method, read an int field, or `instanceof`-test it (→ a `0`/`1`
    /// int via a ternary). Construction + method dispatch exercise the field/method inline
    /// caches + shapes; `instanceof` exercises the nominal `is_instance_of` path. Deterministic.
    fn class_expr(&mut self, depth: u32) -> String {
        let idx = self.choice(self.classes.len() as u32) as usize;
        let c = self.classes[idx].clone();
        let arg = self.int_atom();
        let ctor = if c.init_arity == 1 {
            format!("{}({arg})", c.name)
        } else {
            format!("{}()", c.name)
        };
        match self.choice(3) {
            // call the int method
            0 => format!("({ctor}).{}()", c.method),
            // instanceof → 0/1 (ternary keeps it int-typed + deterministic)
            1 => format!("(({ctor}) instanceof {} ? 1 : 0)", c.name),
            // bind then read the method (a deeper member-cache warm via a let in a block-expr
            // is not available as an expr; just call the method again with a different arg)
            _ => {
                let arg2 = self.int_atom();
                let ctor2 = if c.init_arity == 1 {
                    format!("{}({arg2})", c.name)
                } else {
                    format!("{}()", c.name)
                };
                let _ = depth;
                format!("(({ctor}).{}() + ({ctor2}).{}())", c.method, c.method)
            }
        }
    }

    /// FUZZ Unit 2 — an int-valued expression over a declared enum: construct a variant and
    /// reduce it to an int via an EXHAUSTIVE `match` (value/positional/named/unit patterns +
    /// a wildcard). Construction validates payload arity/types (`validate_into`); the match
    /// exercises structural `==` + payload binding. Deterministic int observable.
    fn enum_expr(&mut self, _depth: u32) -> String {
        let idx = self.choice(self.enums.len() as u32) as usize;
        let e = self.enums[idx].clone();
        let ctor = self.enum_variant_ctor(&e);
        // Build an exhaustive match over the enum that maps each variant to an int.
        let mut arms = String::new();
        for v in &e.variants {
            match v {
                VariantSig::Unit(n) => {
                    let lit = self.int_literal();
                    arms.push_str(&format!("{}.{n} => {lit}, ", e.name));
                }
                VariantSig::Positional(n, arity) => {
                    // Fresh bind names guarantee Option-C BINDS (never accidentally compares
                    // against an in-scope var) and never trips the bind-shadow warning.
                    let binds: Vec<String> = (0..*arity).map(|_| self.fresh("g")).collect();
                    let body = binds.join(" + ");
                    arms.push_str(&format!("{n}({}) => ({body}), ", binds.join(", ")));
                }
                VariantSig::Named(n, fields) => {
                    // Bind each named field (renamed to a fresh local); sum them.
                    let renamed: Vec<String> = fields.iter().map(|_| self.fresh("g")).collect();
                    let binds: Vec<String> = fields
                        .iter()
                        .zip(&renamed)
                        .map(|(f, r)| format!("{f}: {r}"))
                        .collect();
                    arms.push_str(&format!(
                        "{n}({}) => ({}), ",
                        binds.join(", "),
                        renamed.join(" + ")
                    ));
                }
            }
        }
        // Trailing wildcard keeps it exhaustive even if the constructed variant set narrows.
        arms.push_str("_ => 0");
        format!("(match ({ctor}) {{ {arms} }})")
    }

    /// Construct a random variant of `e`, returning the constructor expression. Positional
    /// payloads pass positional int args; named payloads pass `field: int` named args.
    fn enum_variant_ctor(&mut self, e: &EnumSig) -> String {
        let vi = self.choice(e.variants.len() as u32) as usize;
        match &e.variants[vi] {
            VariantSig::Unit(n) => format!("{}.{n}", e.name),
            VariantSig::Positional(n, arity) => {
                let args: Vec<String> = (0..*arity).map(|_| self.int_atom()).collect();
                format!("{}.{n}({})", e.name, args.join(", "))
            }
            VariantSig::Named(n, fields) => {
                let args: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{f}: {}", self.int_atom()))
                    .collect();
                format!("{}.{n}({})", e.name, args.join(", "))
            }
        }
    }

    /// A decimal-typed expression: exact arithmetic over `decimal.from("…")` constructors
    /// (NUM: `Value::Decimal`, exact, opt-in). Mixes with `int` operands (decimal op int is
    /// legal and stays decimal). Avoids `/ 0` by biasing the divisor non-zero. Decimals
    /// print deterministically (exact textual scale), so output stays a pure function of
    /// source. Exercises the `Op::Add`/`Sub`/`Mul`/`Div` decimal arms + MapKey::Decimal.
    fn decimal_expr(&mut self, depth: u32) -> String {
        let a = self.decimal_atom();
        if depth >= MAX_DEPTH || self.flag() {
            return a;
        }
        let op = match self.choice(4) {
            0 => "+",
            1 => "-",
            2 => "*",
            _ => "/",
        };
        if op == "/" {
            // Non-zero decimal divisor by construction.
            let nz = 1 + self.choice(8);
            format!("({a} / decimal.from(\"{nz}\"))")
        } else if self.flag() {
            // Mix with an int operand (decimal op int → decimal).
            let n = self.choice(20);
            format!("({a} {op} {n})")
        } else {
            let b = self.decimal_atom();
            format!("({a} {op} {b})")
        }
    }

    /// A decimal atom: `decimal.from("<literal>")` over a small fixed set of exact values.
    fn decimal_atom(&mut self) -> String {
        let lit = match self.choice(8) {
            0 => "0",
            1 => "1",
            2 => "-1",
            3 => "2.5",
            4 => "0.1",
            5 => "100",
            6 => "3.14",
            _ => "10",
        };
        format!("decimal.from(\"{lit}\")")
    }

    /// A leaf expression: an in-scope variable or a fresh literal.
    fn leaf_expr(&mut self) -> String {
        let vars = self.all_vars();
        if !vars.is_empty() && self.flag() {
            let idx = self.choice(vars.len() as u32) as usize;
            vars[idx].clone()
        } else {
            match self.choice(4) {
                0 => self.int_literal(),
                1 => self.float_literal(),
                2 => self.string_literal(),
                _ => if self.flag() { "true".to_string() } else { "false".to_string() },
            }
        }
    }

    /// An integer-typed expression (variable, literal, or arithmetic over int operands).
    /// Wrapping operators (`+% -% *%`) are preferred for the unbounded-magnitude cases so
    /// the program does not panic on overflow (a panic is FINE for the differential — it
    /// compares panic messages — but wrapping keeps more programs run-to-completion).
    ///
    /// FUZZ Unit 2 — arithmetic completeness: this now also emits `**` exponent (NUM:
    /// traps on i64 overflow; right-associative), the int-only bitwise/shift family
    /// (`& | ^ << >> ~`, Go precedence), exercising the adaptive-arithmetic opcodes plus
    /// the `Op::Pow`/`Op::Shl`/`Op::Shr`/`Op::BitNot` paths. Shift amounts are bounded to a
    /// small valid window and exponents to a small base/power so most programs run to
    /// completion (an out-of-range shift / overflow is still a clean Tier-2 panic the
    /// differential compares, just rarer).
    fn int_expr(&mut self, depth: u32) -> String {
        if depth >= MAX_DEPTH || self.flag() {
            return self.int_atom();
        }
        // Unary complement `~x` (int-only) — a leaf-ish prefix form.
        if self.choice(12) == 0 {
            let a = self.int_atom();
            return format!("(~{a})");
        }
        // Exponent `a ** b` — keep the base small and the power in 0..6 so the result
        // rarely overflows i64 (overflow is a clean trap, just kept rare for completion%).
        if self.choice(12) == 0 {
            let base = match self.choice(4) {
                0 => "2",
                1 => "3",
                2 => "(-2)",
                _ => "1",
            };
            let pow = self.choice(7); // 0..6
            return format!("({base} ** {pow})");
        }
        let a = self.int_atom();
        let b = self.int_atom();
        // Avoid `/ 0` and `% 0` panics being the ONLY thing tested: bias divisor away from
        // a literal zero by adding 1 inside a paren when the op is `/` or `%`.
        let op = match self.choice(13) {
            0 => "+%",
            1 => "-%",
            2 => "*%",
            3 => "+",
            4 => "-",
            5 => "&",
            6 => "|",
            7 => "^",
            8 => "<<",
            9 => ">>",
            10 => "*",
            11 => "/",
            _ => "%",
        };
        if op == "/" || op == "%" {
            // `(b - b + small)` is non-zero by construction → no spurious div-by-zero.
            let nz = 1 + self.choice(7);
            format!("({a} {op} {nz})")
        } else if op == "<<" || op == ">>" {
            // Bound the shift amount to a valid `0..=15` so the shift never trips the
            // `shift amount out of range` trap on most programs (still occasionally a
            // legal small value). The shift COUNT must be a small non-negative literal.
            let amt = self.choice(16); // 0..15
            format!("({a} {op} {amt})")
        } else {
            format!("({a} {op} {b})")
        }
    }

    /// An integer atom: an in-scope int-ish var, an edge literal, or a small literal.
    fn int_atom(&mut self) -> String {
        let vars = self.all_vars();
        if !vars.is_empty() && self.choice(3) == 0 {
            let idx = self.choice(vars.len() as u32) as usize;
            vars[idx].clone()
        } else {
            self.int_literal()
        }
    }

    /// An integer literal, biased toward NUM edge cases. Note: `i64::MIN` cannot be a
    /// bare literal (the lexer parses the magnitude `9223372036854775808` as out-of-range
    /// for i64), so we render it as the equivalent `(-9223372036854775807 - 1)` — exactly
    /// the boundary the engines must agree on.
    fn int_literal(&mut self) -> String {
        match self.choice(14) {
            0 => "0".to_string(),
            1 => "1".to_string(),
            2 => "(-1)".to_string(),
            3 => "9223372036854775807".to_string(),         // i64::MAX
            4 => "(-9223372036854775807 - 1)".to_string(),  // i64::MIN
            5 => "9007199254740992".to_string(),            // 2^53
            6 => "9007199254740993".to_string(),            // 2^53 + 1
            7 => "(-9007199254740992)".to_string(),         // -2^53
            8 => "255".to_string(),
            9 => "0xFF".to_string(),
            10 => "0b1010".to_string(),
            11 => "0o17".to_string(),
            12 => "1_000_000".to_string(),
            _ => {
                let n: i64 = self.u.int_in_range(-1000..=1000).unwrap_or(0);
                if n < 0 {
                    format!("({n})")
                } else {
                    n.to_string()
                }
            }
        }
    }

    /// A float-typed expression.
    fn float_expr(&mut self, depth: u32) -> String {
        if depth >= MAX_DEPTH || self.flag() {
            return self.float_literal();
        }
        let a = self.float_literal();
        let b = self.float_literal();
        let op = match self.choice(4) {
            0 => "+",
            1 => "-",
            2 => "*",
            _ => "/",
        };
        if op == "/" {
            // Non-zero float divisor literal.
            let d = 1 + self.choice(9);
            format!("({a} {op} {d}.0)")
        } else {
            format!("({a} {op} {b})")
        }
    }

    /// A float literal (always with a decimal point so the `float` subtype is explicit).
    fn float_literal(&mut self) -> String {
        match self.choice(8) {
            0 => "0.0".to_string(),
            1 => "1.0".to_string(),
            2 => "(-1.0)".to_string(),
            3 => "3.14".to_string(),
            4 => "0.5".to_string(),
            5 => "100.0".to_string(),
            6 => "(-0.5)".to_string(),
            _ => {
                let n: i64 = self.u.int_in_range(-1000..=1000).unwrap_or(0);
                if n < 0 {
                    format!("({n}.0)")
                } else {
                    format!("{n}.0")
                }
            }
        }
    }

    /// A boolean-typed expression (comparison / logical / literal).
    fn bool_expr(&mut self, depth: u32) -> String {
        if depth >= MAX_DEPTH || self.flag() {
            return if self.flag() { "true".to_string() } else { "false".to_string() };
        }
        match self.choice(4) {
            0 => {
                let a = self.int_atom();
                let b = self.int_atom();
                let op = match self.choice(6) {
                    0 => "<",
                    1 => ">",
                    2 => "<=",
                    3 => ">=",
                    4 => "==",
                    _ => "!=",
                };
                format!("({a} {op} {b})")
            }
            1 => {
                let a = self.bool_expr(depth + 1);
                let b = self.bool_expr(depth + 1);
                let op = if self.flag() { "&&" } else { "||" };
                format!("({a} {op} {b})")
            }
            2 => {
                let a = self.bool_expr(depth + 1);
                format!("(!{a})")
            }
            _ => if self.flag() { "true".to_string() } else { "false".to_string() },
        }
    }

    /// A string-typed expression (literal or concatenation).
    fn string_expr(&mut self, depth: u32) -> String {
        if depth >= MAX_DEPTH || self.flag() {
            return self.string_literal();
        }
        let a = self.string_literal();
        let b = self.string_literal();
        format!("({a} + {b})")
    }

    fn string_literal(&mut self) -> String {
        // A small fixed alphabet of safe, escape-free strings (no `${}`/`"`/`\` so the
        // emitted source is always well-formed and deterministic).
        match self.choice(6) {
            0 => "\"\"".to_string(),
            1 => "\"a\"".to_string(),
            2 => "\"hello\"".to_string(),
            3 => "\"xyz\"".to_string(),
            4 => "\"123\"".to_string(),
            _ => "\" \"".to_string(),
        }
    }

    /// A ternary `cond ? a : b`.
    fn ternary_expr(&mut self, depth: u32) -> String {
        let c = self.bool_expr(depth + 1);
        let a = self.int_expr(depth + 1);
        let b = self.int_expr(depth + 1);
        format!("({c} ? {a} : {b})")
    }

    /// A call to a declared function with matching arity (or a leaf if none declared).
    fn call_expr(&mut self, depth: u32) -> String {
        if self.fns.is_empty() {
            return self.int_expr(depth);
        }
        let idx = self.choice(self.fns.len() as u32) as usize;
        let sig = self.fns[idx].clone();
        let args: Vec<String> = (0..sig.arity).map(|_| self.int_expr(depth + 1)).collect();
        format!("{}({})", sig.name, args.join(", "))
    }

    /// An array literal (sometimes empty — an empty-collection edge case). `len(...)` of
    /// it is the observable so output stays deterministic (no element-formatting reliance).
    fn array_expr(&mut self, _depth: u32) -> String {
        let n = self.choice(4); // 0..3 elements (0 = empty-collection edge)
        let elems: Vec<String> = (0..n).map(|_| self.int_atom()).collect();
        format!("len([{}])", elems.join(", "))
    }

    /// FUZZ Unit 2 — composite literals + ops. An int-valued expression read OUT of an
    /// object / map / array / set: object-literal `{k: v}` member + index read, map-literal
    /// `#{k: v}` `map.get`, array indexing, and `set.from([...])` size. Exercises the shape
    /// inline caches (object member/index reads), `MapKey` canonicalization (−0.0/NaN/
    /// integral-float-folds-to-int via numeric keys), and the `len`/`set.size`/`map.get`
    /// native paths. The observable is always a deterministic scalar `int` so output stays
    /// a pure function of source (never an unordered map/set print).
    fn composite_expr(&mut self, _depth: u32) -> String {
        match self.choice(7) {
            // Object literal: member read of a known key.
            0 => {
                let a = self.int_atom();
                let b = self.int_atom();
                format!("({{a: {a}, b: {b}}}).a")
            }
            // Object literal: index read with a string key.
            1 => {
                let a = self.int_atom();
                let b = self.int_atom();
                format!("({{a: {a}, b: {b}}})[\"b\"]")
            }
            // Object literal: `len` (key count).
            2 => {
                let a = self.int_atom();
                let b = self.int_atom();
                format!("len({{a: {a}, b: {b}}})")
            }
            // Map literal: `map.get` of a present key. Numeric keys exercise MapKey
            // canonicalization (the integral-float-folds-to-int + −0.0/NaN unification).
            3 => {
                let v = self.int_atom();
                let key = match self.choice(4) {
                    0 => "1",
                    1 => "0",
                    2 => "true",
                    _ => "\"k\"",
                };
                format!("map.get(#{{{key}: {v}}}, {key})")
            }
            // Map literal: `len`.
            4 => {
                let a = self.int_atom();
                let b = self.int_atom();
                format!("len(#{{1: {a}, 2: {b}}})")
            }
            // Array indexing of a non-empty array literal.
            5 => {
                let a = self.int_atom();
                let b = self.int_atom();
                let c = self.int_atom();
                let idx = self.choice(3); // 0..2, always in bounds
                format!("[{a}, {b}, {c}][{idx}]")
            }
            // Set construction + size (dedup edge: `set.from` over a literal array).
            _ => {
                let a = self.int_atom();
                let b = self.int_atom();
                format!("set.size(set.from([{a}, {b}, {a}]))")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke: every generator output PARSES and RUNS to completion (or panics cleanly)
    /// on all three engines without the GENERATOR itself panicking. We drive a spread of
    /// fixed byte seeds (deterministic, reproducible) and just assert the generator
    /// produces non-empty, parseable source. The full three-engine agreement is the
    /// `tests/property.rs` differential — here we only prove the generator is well-formed.
    #[test]
    fn generator_emits_parseable_programs() {
        for seed in 0u64..64 {
            let bytes = seed_bytes(seed);
            let prog = gen_program_from_bytes(&bytes);
            assert!(!prog.source.is_empty(), "seed {seed}: empty program");
            // The legacy lexer+parser must accept it (the generator is grammar-aware).
            let tokens = crate::lexer::lex(&prog.source).unwrap_or_else(|e| {
                panic!("seed {seed}: lex failed: {}\n--- src ---\n{}", e.message, prog.source)
            });
            crate::parser::parse(&tokens).unwrap_or_else(|e| {
                panic!("seed {seed}: parse failed: {}\n--- src ---\n{}", e.message, prog.source)
            });
            // The CST front-end must also accept it (no error nodes from a fatal parse).
            let parse = crate::syntax::parser::parse(&prog.source);
            assert!(
                parse.errors.is_empty() && parse.lex_errors.is_empty(),
                "seed {seed}: CST parse errors {:?} / lex {:?}\n--- src ---\n{}",
                parse.errors,
                parse.lex_errors,
                prog.source
            );
        }
    }

    /// The expression generator likewise produces a parseable `print(<expr>)` program.
    #[test]
    fn expr_generator_emits_parseable_programs() {
        for seed in 0u64..32 {
            let bytes = seed_bytes(seed.wrapping_mul(2654435761));
            let mut u = Unstructured::new(&bytes);
            let prog = gen_expr_program(&mut u);
            let tokens = crate::lexer::lex(&prog.source)
                .unwrap_or_else(|e| panic!("expr seed {seed}: lex: {}\n{}", e.message, prog.source));
            crate::parser::parse(&tokens)
                .unwrap_or_else(|e| panic!("expr seed {seed}: parse: {}\n{}", e.message, prog.source));
        }
    }

    /// Deterministic-by-construction: the same seed produces byte-identical source.
    #[test]
    fn generator_is_deterministic() {
        let bytes = seed_bytes(0xDEADBEEF);
        let a = gen_program_from_bytes(&bytes);
        let b = gen_program_from_bytes(&bytes);
        assert_eq!(a.source, b.source, "same seed must yield identical source");
    }

    /// LOOP-TERMINATION INVARIANT (regression guard, FUZZ self-found bug): a `while` loop
    /// counter is PROTECTED inside its OWN BODY — it must never appear as an assignment
    /// target there except the generator's own `cN = cN + 1` progress step. A body
    /// reassignment like `w1 = (1000000 -% 9007199254740992)` would set the counter to
    /// ~-9e15 and the loop would run ~quadrillions of iterations (a generator-induced
    /// near-hang the three-way differential flagged as a multi-minute non-termination).
    ///
    /// We brace-scan each `while (cN < K) {` to its matching close and assert no
    /// `cN = <other>` occurs INSIDE that span (a reassignment AFTER the loop ends is
    /// harmless and allowed). The increment `cN = cN + 1` is the only permitted form.
    #[test]
    fn while_counters_are_never_reassigned_inside_their_body() {
        for seed in 0u64..300 {
            let prog = gen_program_from_bytes(&seed_bytes(seed));
            let lines: Vec<&str> = prog.source.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let t = line.trim();
                let Some(rest) = t.strip_prefix("while (") else {
                    continue;
                };
                let ctr = rest.split(' ').next().unwrap_or("");
                if ctr.is_empty() {
                    continue;
                }
                let legal = format!("{ctr} = {ctr} + 1");
                let illegal_prefix = format!("{ctr} = ");
                // Brace-scan from the `while (...) {` line to its matching close.
                let mut depth = 0i32;
                let mut started = false;
                for l in &lines[i..] {
                    depth += l.matches('{').count() as i32;
                    depth -= l.matches('}').count() as i32;
                    started |= l.contains('{');
                    let lt = l.trim();
                    if lt.starts_with(&illegal_prefix) && lt != legal {
                        panic!(
                            "seed {seed}: while-counter `{ctr}` reassigned INSIDE its body \
                             (non-terminating risk): `{lt}`\n--- src ---\n{}",
                            prog.source
                        );
                    }
                    if started && depth <= 0 {
                        break; // matched the closing brace of this while
                    }
                }
            }
        }
    }

    /// Expand a u64 seed into a longer, varied byte buffer so the generator has enough
    /// entropy to reach deeper productions (a bare 8-byte seed exhausts fast → trivial
    /// programs only). A simple xorshift fill — deterministic and dependency-free.
    fn seed_bytes(seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        let mut out = Vec::with_capacity(512);
        for _ in 0..512 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.push((x & 0xFF) as u8);
        }
        out
    }
}
