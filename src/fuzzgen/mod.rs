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
//!     `set.from` size, and `for…of` iteration.
//!   - **Classes / enums / ADT / rich `match`** (see `class_decl`/`enum_decl`/`class_expr`/
//!     `enum_expr`): typed fields/defaults/`init`/methods, `instanceof`, inheritance/`super`;
//!     unit + positional + named ADT variants, constructors, exhaustive `match` over each with
//!     positional/named/unit + wildcard patterns + Option-C fresh-binds.
//!   - **Closures + capture:** arrow closures, capture-by-value vs by-reference, per-iteration
//!     loop-var freshness, nested/curried closures.
//!   - **String templates `${…}`** (incl. nested) and **`?`/`!` propagate/unwrap** over the
//!     `[value, err]` tier-1 model.
//!
//! STILL NOT EMITTED (the next breadth follow-up; the differential cannot fuzz what it never
//! generates): interfaces + structural-`instanceof`; destructuring/spread/rest in let/params;
//! async/await/spawn/workers (deferred — nondeterministic scheduling, see spec §6); try/recover
//! (the `recover(fn(){…})` carry-forward bug); generators `fn*`/`yield`.
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
        self.fns.push(FnSig { name, arity });
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
        let pick = self.choice(10);
        match pick {
            0 | 1 => self.let_stmt(depth),
            2 => self.const_stmt(depth),
            3 if !self.mut_vars().is_empty() => self.assign_stmt(depth),
            4 => self.if_stmt(depth),
            5 => self.while_stmt(depth),
            6 => self.for_stmt(depth),
            7 => self.print_stmt(depth),
            8 => self.match_stmt(depth),
            9 if self.in_fn => self.print_stmt(depth), // keep returns rare/at fn tail only
            _ => self.print_stmt(depth),
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
        match self.choice(16) {
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
            _ => self.int_expr(depth),
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
