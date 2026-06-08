# ADT — Algebraic Enums & Exhaustive Match — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; reviewer runs the commands and probes edges). Steps use `- [ ]`. Follows the NUM plan format
> (`superpowers/plans/2026-06-08-numeric-model.md`).

**Spec:** `superpowers/specs/2026-06-08-algebraic-types-design.md`. **Branch:** `feat/algebraic-types`
off `main`. **Depends on:** **NUM merged** (variant payload field types use `int`/`float`/`number`; rebase
onto `Value::Int`/`Value::Float` first). **Breaking** (enum surface redesign + a blocking
`non-exhaustive-match` Error) — the corpus is *migrated*, not deleted (Gate 7).

**Architecture:** extend `EnumVariant` (`src/value.rs:278`) with `payload: Option<Payload>` + `ctor: bool`;
keep `Value::EnumVariant(Rc<EnumVariant>)` UNCHANGED (`value.rs:660` — the wrapper is NOT re-typed to `Cc`;
only the payload `Vec`/`Cc<ObjectCell>` is cycle-collected, §5.3). `Payload::{Positional(Vec<Value>),
Named(Cc<ObjectCell>)}`; `VariantSchema` (arity + field names + `Type`s) on `EnumDef`. Unit/scalar variants
stay byte-identical (interned `Rc`, identity equality `value.rs:719-720`, `.name`/`.value`
`interp.rs:3578-3585`). Payload variants are a fresh value, **structural** equality. New
`Pattern::Variant` (`ast.rs:385`) + a `VariantPat` CST kind for named/nested; positional patterns ride
`call_expression` via **semantic recovery** (no new grammar node). Exhaustiveness is a **static** analysis
in `src/check/infer/pass.rs` emitting `non-exhaustive-match` (default **Error**) — the runtime
`MatchNoArm` backstop (`interp.rs:3137`, `Op::MatchNoArm` `compile/mod.rs:3553`) is UNCHANGED. Worker
far-side unit-variant **re-interning is NEW** (`serialize.rs:624` builds a fresh `Rc`). `.aso` version
bump = read `ASO_FORMAT_VERSION` (`aso.rs:105`, currently `18`, NUM bumped) **+ 1**. Both engines
byte-identical.

**Tech stack:** Rust; the two front-ends (`src/parser.rs`, `src/syntax/parser.rs`); tree-sitter
(`--abi 14`); `src/vm/{compile,opcode,run,disasm,aso,verify}`; `src/check/infer/{ty,table,env,pass}.rs`;
`src/check/{config,rules}`; `src/worker/serialize.rs` + `dispatch.rs`; `src/fmt.rs`; `src/lsp/`; `src/repl.rs`.

---

## Shared API Contract (pinned to current code)
**Existing (verified):** `Value::EnumVariant(Rc<EnumVariant>)` `value.rs:660`; `struct EnumVariant
{enum_name,name,value}` `value.rs:278`; `struct EnumDef {name, variants}` `value.rs:274`; `is_truthy`
`value.rs:687` (NUM-resolved falsy set); `EnumVariant` identity-eq `value.rs:719-720`; `Display`
`value.rs:885`; `Stmt::Enum` eval interns variants `interp.rs:2750`; `read_member` enum arm `interp.rs:3578`
(`.name` `:3579`, `.value` `:3580`); no-arm panic `interp.rs:3137`; `match_pattern` `interp.rs:3259`
(`Pattern::Ident` `env.get` bind/compare `:3269`); `type_name` `interp.rs:5410` (`EnumVariant(_) =>
"enum variant"`); `check_type` free fn `interp.rs:5704`; `enum Pattern` `ast.rs:385` (+ `Display` `:420`);
`struct EnumVariantDecl {name, value, name_span}` `ast.rs:497`; legacy `enum_decl` `parser.rs:281`,
`parse_pattern` `parser.rs:1341`; CST `enum_decl` `syntax/parser.rs:1449`, `pattern`
`syntax/parser.rs:1691`; `SyntaxKind::{MatchExpr,MatchArm,WildcardPat,LiteralPat,RangePat,ArrayPat,
ObjectPat}` `syntax/kind.rs:73-81`; `compile_enum` `compile/mod.rs:1949`, `compile_match`
`compile/mod.rs:3500` (`MatchNoArm` emit `:3553`), `compile_pattern_test(pat: &ResolvedNode, …)`
`compile/mod.rs:3596`; checker `CheckTy::{Enum(EnumId),EnumVariant(EnumId,name)}` `ty.rs:66-68` (widen
`:190`, eq `:375-382`); `struct EnumInfo` `table.rs:29`, `fn enum_variants(node)->Vec<String>`
`table.rs:251`; `synth_match` `pass.rs:952` (iterates `MatchExpr` children `:972`; first-arm-only nesting
doc `:949-951`); `walk_stmts` (reaches sibling arms) `pass.rs:136`; member-access variant synth
`pass.rs:1033-1036`; infer `emit` hardcodes `Severity::Warning` `pass.rs:118-128`; `analyze_with_config`
applies `config.effective(code, severity)` `analyze.rs:32,97`; `ALL_CODES` (incl.
`"unknown-enum-variant"`) `config.rs:31-50`; `unknown_enum_variant::check` `rules/unknown_enum_variant.rs`
(Warning `:75`, conservative single-enum receiver); worker `TAG_ENUM=10` `serialize.rs:90` (wire doc `:18`,
encode `:466`, decode fresh `Rc` `:620-630`, `unsendable_kind` lists `EnumVariant` `:121`, round-trip
fixture `:897`); `with_capacity(len.min(r.remaining()))` clamp precedent `serialize.rs:564`; worker
global-name fixpoint (`GET_GLOBAL`/`TopDef`) `dispatch.rs:17-46,82-98`; `ASO_FORMAT_VERSION=18`
`aso.rs:105`; GC doc-comment lists `EnumVariant` under "immutable/acyclic … stay on Rc" `gc.rs:34`;
`fmt::write_pattern` `fmt.rs:719`; REPL `is_incomplete` (token-depth) `repl.rs:48`; LSP providers
`src/lsp/providers/*`; examples with trailing-`_`-sibling catch-alls `examples/oop.as:29-32`,
`examples/all_features.as:149-152,161-166,179-182`.
**New names (do not rename):** `Payload::{Positional,Named}`; `struct VariantSchema {fields:
Vec<(Option<Rc<str>>, Type)>}`; `EnumDef.variant_schemas`; `Pattern::Variant {enum_name:
Option<Rc<str>>, variant: Rc<str>, fields: VariantPatFields}` with `VariantPatFields::{Positional(Vec<
Pattern>), Named(Vec<(Rc<str>, Option<Pattern>)>)}`; `SyntaxKind::VariantPat`; check codes
`non-exhaustive-match` (default **Error**) + `enum-variant-binding-shadow` (default **Warning**); optional
`unreachable-match-arm` (Warning). Wire `payload_tag` `0|1|2` (unit/positional/named).

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH configs.
- Four-mode byte-identity (tree-walker == specialized-VM == generic-VM == `.aso`, both feature configs,
  `vm_differential.rs`) — fix the engine, never the assertion. Never hold a `RefCell`/`resources` borrow
  across `.await`. Tree-sitter: after `grammar.js`, `tree-sitter generate --abi 14` then `cargo build`.
- Gradual gate (Gate 5): `examples/**` emits ZERO `non-exhaustive-match` / `enum-variant-binding-shadow` /
  `type-*` in BOTH configs — uncertain ⇒ silent, never relax the gate.

---

## Task 1 — Value layer: payload-carrying `EnumVariant`, `Trace`, structural eq, Display, MapKey
**Files:** `src/value.rs` (+ every exhaustive `match Value`, compiler-flushed). **Tests:** `value.rs`.
- [ ] Failing tests: a `payload: None, ctor: false` unit variant is byte-identical to today (`.name`,
  `.value==Nil`/scalar, identity `==`, `is_truthy==true`, `type_name=="enum variant"`); a constructed
  `Positional([Int(3),Int(4)])` and `Named({radius:Float(2.0)})` variant: structural `==`
  (`Circle(2.0)==Circle(2.0)` true, `!=Circle(3.0)`), truthy, `Display` renders `Shape.Circle(2.0)` /
  `Shape.Pair(3, 4)`; a payload variant as a `MapKey` is the identity-container Tier-2 panic (unit
  unchanged); a unit variant traces nothing, a `Json.Arr` self-referential payload is reachable via `Trace`.
- [ ] Extend `struct EnumVariant` (`value.rs:278`) with `payload: Option<Payload>` + `ctor: bool`; add
  `enum Payload {Positional(Vec<Value>), Named(Cc<ObjectCell>)}` and `struct VariantSchema {fields:
  Vec<(Option<Rc<str>>, Type)>}`; add `variant_schemas: IndexMap<String, VariantSchema>` to `EnumDef`
  (`value.rs:274`). **Keep `Value::EnumVariant(Rc<EnumVariant>)`** (`value.rs:660`) — edit construction
  sites in place, do NOT re-type to `Cc`. Add a real `Trace` impl (trace `value` + when `Some` the payload
  `Vec`/`ObjectCell`) and mirror `EnumVariant` into the `Value::trace` container set. Structural-eq branch
  in the `EnumVariant` `PartialEq` arm (`value.rs:719-720`, `payload.is_some()` → element/key-wise; else
  `Rc::ptr_eq`). `Display` for a constructed variant (`value.rs:885`). `MapKey::from` rejects payload
  variants (unit unchanged). Confirm-no-change asserts on `is_truthy` (`value.rs:687`) and `type_name`
  (`interp.rs:5410`) wildcard arms (payload is truthy; string unchanged).
- [ ] Green both configs; clippy. Independent review (greps for unit-variant `Rc` cheapness preserved; no
  `Cc` on the wrapper; `Trace` reaches a cyclic payload; structural-eq is element/key-wise). Commit.

## Task 2 — AST: payload variant decl + `Pattern::Variant` + Display
**Files:** `src/ast.rs` (+ `interp.rs`/`fmt.rs`/`ast.rs Display` exhaustive arms, compiler-flushed).
**Tests:** `ast.rs`.
- [ ] Extend `EnumVariantDecl` (`ast.rs:497`): a variant is `value: Option<Expr>` (scalar backing) XOR a
  `payload: Vec<VariantField>` where `VariantField {name: Option<Rc<str>>, ty: Type}` (named ⇔ `Some`,
  positional ⇔ `None`; uniformity is a parse-time concern, Tasks 3/4). Add `Pattern::Variant {enum_name:
  Option<Rc<str>>, variant: Rc<str>, fields: VariantPatFields}` to `enum Pattern` (`ast.rs:385`) +
  `VariantPatFields::{Positional(Vec<Pattern>), Named(Vec<(Rc<str>, Option<Pattern>)>)}`. The compiler
  flushes the missing exhaustive arms in `interp.rs` (eval), `fmt.rs` (`write_pattern` `:719`), and the
  `ast.rs` `Pattern` `Display` (`:420`) — wire all three (real arms, Task 8/10 refine fmt goldens).
- [ ] Green; review (every `match Pattern`/`match … variant decl` arm covered, no `todo!`); commit.

## Task 3 — Legacy parser: payload variant decl + variant patterns
**Files:** `src/parser.rs`. **Tests:** `parser.rs`.
- [ ] Failing tests: `enum Shape {Circle(radius: float), Rect(w: float, h: float), Pair(int, int),
  Point}` parses to the payload/backing fields; mixed `Pair(int, h: float)` → `enum variant fields must be
  all named or all positional`; a variant with both `= 2` and `(…)` → `a variant cannot have both a
  '= value' backing and a '(…)' payload`; `Circle(r)` / `Shape.Circle(r)` / `Pair(a, b)` /
  `Rect(w: ww, h: hh)` / `Circle(radius: 0.0)` in pattern position → `Pattern::Variant`; a bare unit
  `Point` stays `Pattern::Ident` (Option-C, unchanged).
- [ ] `enum_decl` (`parser.rs:281`): parse a `(…)` payload field list (named `id: T` / positional `T`,
  uniformity + backing-XOR-payload errors; field type **required**). `parse_pattern` (`parser.rs:1341`):
  after parsing a primary that is a variant-ref (`Name` / `Name.Variant`) **followed by `(`**, re-classify
  into `Pattern::Variant` — positional sub-patterns by index, named (`field` / `field: subpat`) mirroring
  object patterns. Bare/qualified unit (no parens) flows through the existing value/ident path UNCHANGED.
- [ ] Green; review; commit.

## Task 4 — CST parser + SyntaxKind + frontend conformance
**Files:** `src/syntax/parser.rs`, `src/syntax/kind.rs`. **Tests:** `tests/frontend_conformance.rs`.
- [ ] Add `SyntaxKind::VariantPat` (`kind.rs:73-81` cluster). `enum_decl` (`syntax/parser.rs:1449`) +
  `enum_variant`: payload field list, same uniformity/backing-XOR errors. `pattern`
  (`syntax/parser.rs:1691`): a variant-ref followed by `(` produces a `VariantPat` node (positional ride
  also accepted via the call-recovery path, §11 — but the CST emits a typed `VariantPat` so
  `compile_pattern_test` can match a kind). Frontend conformance proves both parsers agree on payload
  enums + all variant-pattern shapes incl. the two parse errors.
- [ ] Green; review (both front-ends produce identical errors + shapes); commit.

## Task 5 — Tree-sitter grammar + publish + editor pins
**Files:** `tree-sitter-ascript/grammar.js`, `queries/highlights.scm`, editors. **Tests:**
`tests/treesitter_conformance.rs`.
- [ ] Extend `enum_variant` (`grammar.js:273`) with an optional payload list (named/positional). Patterns:
  **positional `Circle(r)`/`Shape.Circle(r)` ride `call_expression`** via the existing
  `_match_subject`→`_postfix_expression` route (`grammar.js:605,521`) and the `[$._expression,
  $._match_subject]` conflict (`:77`) — **semantic recovery**, no new node, no new conflict (the
  `array_pattern_match`/`array_literal` precedent `:73` is a bracketed form, dropped per spec §11). Add a
  **minimal `variant_pattern` node ONLY for named/renamed/nested** (`Name`/`Name.Variant` + paren list of
  `field (':' _match_pattern_single)?`) with a single declared GLR conflict against `call_expression`.
  Regen `parser.c` (`tree-sitter generate --abi 14`); `cargo build`. Update `queries/highlights.scm`
  (variant-name + field). Run `./scripts/sync-grammar.sh`; bump `editors/zed/extension.toml` `commit` +
  `editors/nvim/lua/ascript/treesitter.lua` `revision`; update Zed/Neovim bundled `highlights.scm` +
  `editors/vscode/syntaxes/ascript.tmLanguage.json`.
- [ ] Conformance green (new grammar rules parse; no GLR regressions); review; commit.

## Task 6 — Tree-walker: construction, constructor value, `.value`/field sugar, match arm
**Files:** `src/interp.rs`. **Tests:** `interp.rs`.
- [ ] Failing tests: `Stmt::Enum` builds `variant_schemas` + constructor variants (`ctor:true` for payload
  variants); `Shape.Point` reads the interned unit (unchanged); `Shape.Circle` reads a constructor;
  `Shape.Pair(3,4)` / `Shape.Circle(2.0)` / `Shape.Rect(w:3.0,h:4.0)` construct (arity + field-type checked
  via `validate_into`); arity error (`Shape.Pair expects 2 fields, got 1`), field-type error
  (`Shape.Circle.radius: expected float, got string`), unit-called error (`Shape.Point ... takes no
  payload`), multi-field-named-positional error (`Shape.Rect requires named fields (w:, h:)`); first-class
  `let mk = Shape.Circle; mk(2.0)` and `array.map(radii, Shape.Circle)`; `.value` → Object (named) / stable
  Array handle (positional, `v.value is v.value`); `c.radius` field sugar; `match s {Circle(r)=>…,
  Pair(a,b)=>…, Point=>…}` binds correctly, non-matching falls through, no-arm panic still fires.
- [ ] `Stmt::Enum` eval (`interp.rs:2750`): build `variant_schemas`; intern unit variants (unchanged),
  build `ctor:true` payload-constructor variants. `read_member` (`interp.rs:3578`): return constructor for
  a payload variant; extend `.value` (`:3580`) to the payload-as-data + named-field sugar. `call_value`:
  validate a `ctor` variant call (arity + field types via the `validate_into` field-coercion path) →
  construct `payload: Some(…), ctor:false`. `match_pattern` (`interp.rs:3259`): `Pattern::Variant` arm
  (enum_name + variant tag-test, then destructure positional-by-index / named-by-field, sub-patterns
  recurse). Runtime is byte-identical on both engines; no subject-type lookup added to the hot path.
- [ ] Green; review (never holds a borrow across `.await`; constructor schema looked up on `EnumDef`, not
  cloned per-ctor); commit.

## Task 7 — VM: `compile_enum`, `compile_pattern_test` Variant, constructor call
**Files:** `src/compile/mod.rs`, `src/vm/{opcode,run,disasm}.rs`. **Tests:** `tests/vm_differential.rs`.
- [ ] `compile_enum` (`compile/mod.rs:1949`) builds `variant_schemas`; `compile_pattern_test`
  (`compile/mod.rs:3596`) lowers `VariantPat` byte-identically to the tree-walker `match_pattern` Variant
  arm (tag-test → payload sub-pattern fail-jumps; honor the `compile_match` BYTE-FOR-BYTE doc
  `compile/mod.rs:3500`). A variant constructor call routes through the SAME validation as Task 6. A fused
  `Op::MatchVariant` is OPTIONAL/additive — if added, wire `opcode.rs`+`run.rs`+`disasm.rs` and keep both
  specialize modes byte-identical (three-way differential). `MatchNoArm` (`:3553`) UNCHANGED.
- [ ] Three-way differential green (specialized == generic == tree-walker on construction/equality/
  destructure/`.value`/no-arm); review; commit.

## Task 8 — `.aso`: per-variant schema + constructed-payload constant + version bump
**Files:** `src/vm/aso.rs`, `src/vm/verify.rs`. **Tests:** `aso.rs`.
- [ ] Failing tests: write→read preserves payload-variant constants + `variant_schemas` (field names +
  types) + positional/named payload; **read `ASO_FORMAT_VERSION` (`aso.rs:105`) and bump by ONE** relative
  to whatever NUM left it — do NOT hardcode `19` (cross-cutting #5). Serialize/verify the new layout;
  update `verify.rs` bounds checks; **clamp every payload-length `reserve`/`with_capacity` with
  `.min(r.remaining())`** (cross-cutting #1, precedent `serialize.rs:564`).
- [ ] Round-trip green; review (no unclamped allocation in any new reader path); commit.

## Task 9 — Worker airlock: payload wire format + far-side unit-variant re-interning
**Files:** `src/worker/serialize.rs` (+ confirm `dispatch.rs` enum-shipping). **Tests:** `serialize.rs`.
- [ ] Failing tests: encode→decode round-trips a positional, a named, and a **cyclic recursive** payload
  variant (incl. as a nested object field and as a Map value); a payload holding a non-sendable kind
  (`Function`/`Native`/`Future`/`Generator`) is the recoverable path-error with the path extended through
  the payload (`arg[0].payload.items[2]`); **cross-isolate equality (NEW requirement):** a received unit
  variant `==` the far isolate's own `Shape.Point` literal (re-interning succeeded), a received payload
  variant `==` a fresh far-side `Shape.Circle(2.0)` (structural); the best-effort fallback (absent far-side
  `EnumDef` → fresh `Rc`) is pinned so a code-shipping regression is caught.
- [ ] Extend `TAG_ENUM=10` (`serialize.rs:90`, wire doc `:18`, encode `:466`) with `payload_tag` `0` (unit,
  old format) / `1` (positional: `len`+elements) / `2` (named: reuse the Object serializer); payload
  participates in the visited-reference table (cycles). **Decoder (`serialize.rs:620-630`):** for a unit
  variant, look the variant up on the **far-side `EnumDef`** (in scope via the `dispatch.rs:17-46`
  global-name fixpoint that ships enum consts) and return that isolate's interned constant; fall back to a
  fresh `Rc` only if absent. Payload variant → fresh constructed variant (structural). Extend
  `unsendable_kind` path threading through the payload; clamp any payload-length `with_capacity` with
  `.min(r.remaining())`. Update the encode fixture (`serialize.rs:897`).
- [ ] Green; review (re-interning correct + fallback pinned; cycles handled; sendability path correct);
  commit.

## Task 10 — Formatter: payload decls + variant patterns
**Files:** `src/fmt.rs`. **Tests:** fmt idempotence goldens.
- [ ] Render payload variant **declarations** (`Circle(radius: float)`, `Rect(w: float, h: float)`,
  `Pair(int, int)`) and `Pattern::Variant` in `write_pattern` (`fmt.rs:719`) — `Circle(r)`, `Pair(a, b)`,
  `Rect(w: ww, h: hh)`, `Circle(radius: 0.0)` — canonically; idempotence goldens; the `ast.rs` `Pattern`
  `Display` (`:420`) matches the formatter exactly.
- [ ] Green; review; commit.

## Task 11 — Checker: schema, construction synth, narrowing, `unknown-enum-variant` extension
**Files:** `src/check/infer/{table,ty,env,pass}.rs`, `src/check/rules/unknown_enum_variant.rs`,
`src/check/config.rs`. **Tests:** `tests/check.rs`.
- [x] `EnumInfo` (`table.rs`) gains per-variant payload `variant_fields` (name + `CheckTy`), lowered in a
  pass-2b step so a recursive `array<Json>` field forward-references resolve; `EnumInfo::fields_of`.
  Construction synth: `Shape.Circle(2.0)` synths `CheckTy::EnumVariant(Shape,"Circle")` (widens to
  `Enum(Shape)`) via `synth_variant_construction` in `synth_call` — the receiver enum is resolved by NAME
  (`enum_id_of_ref`, since the enum name used as a value is not env-typed) OR by an `Enum`-typed receiver; a
  **provably-wrong** positional payload arg vs the declared field type → `type-mismatch` (`Unknown`/named/
  spread args stay silent — Gate 5). Per-variant narrowing in a `Circle(r)=>…` arm: `synth_match` narrows the
  subject to `EnumVariant(Shape,"Circle")` and `bind_variant_payload` binds `r` (and named/renamed fields)
  to the declared field type in the arm overlay. **Extended** `unknown_enum_variant::check` to qualified
  variant patterns `Shape.Nope(r)` (payload-ctor calls already flow through the existing `MemberExpr` arm);
  bare patterns stay uncovered. Registered `non-exhaustive-match` (default **Error**) +
  `enum-variant-binding-shadow` (default **Warning**) in `RULE_CODES` (`config.rs`).
- [x] **Gate 5:** `examples/**` emits ZERO `type-*`/`enum-variant-binding-shadow` in BOTH configs (0/0
  verified on the full corpus). Green; reviewed.

## Task 12 — Exhaustiveness analysis: `non-exhaustive-match` (Error) + sibling-arm gather + shadow diag
**Files:** `src/check/infer/pass.rs`. **Tests:** `tests/check.rs`.
- [x] Tests (the correctness pillar) in `tests/check.rs::adt_exhaustiveness`: missing variant + no `_` →
  `non-exhaustive-match` **Error** naming the missing variants (Gate 9 — also exercised as a real
  `ascript check` non-zero exit); the passing twin (all variants OR `_`) → zero; full coverage / `_` /
  bare-binding catch-all → no diagnostic; a **guarded-only** arm does NOT cover (guarded+unguarded does);
  **unknown/untyped subject → silent**; a bare unit variant that would *bind* on a known-enum subject →
  `enum-variant-binding-shadow` (Warning) **and** counts as a catch-all; qualified `Shape.Point` → no
  warning, covers `Point`; a value-equality arm (`Circle(0.0)`) does not fully cover. **Explicit
  zero-diagnostic assertions on `examples/oop.as` and `examples/all_features.as`.**
- [x] New `check_exhaustiveness` wired into `synth_match` (runs only when the subject provably IS an enum).
  Added an **Error-severity** `emit_with` (the old `emit` now delegates at `Warning`); `config.effective`
  carries the Error default. **Arm gathering — FINDING:** an earlier ADT task (the CST parser rework) already
  nests EVERY arm directly under `MatchExpr` (empirically verified: `oop.as` `[3]`, `all_features.as`
  `[3,5,3,3]`, ZERO orphan sibling `MatchArm`). So `expr.children().filter(MatchArm)` already sees all arms;
  `gather_match_arms` uses that PLUS a defensive trailing-sibling sweep (a no-op today, kept as a guard
  against any CST regression). **Coverage** per spec: variant pattern / unit `V`/`E.V` / catch-all
  (`_`/would-bind-`Ident`/catch-all or-alt); guarded arms cover nothing on their own; a refutable payload
  sub-pattern (`Circle(0.0)`) does not fully cover. Would-bind vs would-compare is decided by whether the
  resolver created a `PatternBind` at the ident's range (empirically verified). `unreachable-match-arm` not
  shipped (optional).
- [x] **Gate 5/9:** ZERO false positives on `examples/**` (both configs, full-corpus count 0/0); the
  missing-variant fixture is a real `ascript check` Error (exit 1). Green; reviewed. (NOTE: migrated
  `examples/enums_adt.as` `Point =>` → `Shape.Point =>` — the spec-mandated qualified unit form — so the
  example is exhaustive AND shadow-clean; runtime output unchanged.)

## Task 13 — LSP
**Files:** `src/lsp/providers/*`, `src/lsp/workspace.rs`. **Tests:** `tests/lsp.rs`.
- [ ] Semantic tokens for variant fields (`semantic_tokens.rs`); `hover` shows a variant's payload
  signature + a constructed value's enum type (`hover.rs`); go-to-def / find-references / rename cover
  payload variants + their fields (`navigation.rs`/`rename.rs` + `workspace.rs` index); the
  `non-exhaustive-match`/`enum-variant-binding-shadow` diagnostics flow the existing `check::analyze` →
  `diagnostic.rs` path; `completion.rs` offers variant constructors with field placeholders.
- [ ] Green; review; commit.

## Task 14 — REPL
**Files:** `src/repl.rs`. **Tests:** `repl` regression (in `tests/` or inline).
- [ ] Payload variant decls + variant patterns use parens/braces → existing delimiter-depth
  `is_incomplete` buffering (`repl.rs:48`) handles multi-line entry unchanged. Regression test: declare a
  payload enum across lines, construct, `match`, observe `.value` — via the persistent session
  `Vm`/`Interp` (cross-line).
- [ ] Green; review; commit.

## Task 15 — Example corpus + four-mode differential + Gate-7 migration
**Files:** `examples/enums_adt.as`, `examples/advanced/{state_machine,json_adt,typed_errors}.as`; ALL
existing enum examples/goldens. **Tests:** conformance + `vm_differential.rs` + fmt idempotence.
- [ ] New `examples/enums_adt.as` (`Shape` Circle/Rect/Pair/Point: construction, `match` area, `.value`
  reflection, first-class ctor via `map`); `examples/advanced/state_machine.as` (`enum Event {KeyPress(
  code: int), Resize(w: int, h: int), Quit}` exhaustively matched — a clean zero-diagnostic program, Gate
  5); `examples/advanced/json_adt.as` (recursive `Json` enum: render + parse, exercises recursive payload
  + GC); optional `examples/advanced/typed_errors.as` (`DbError` through `[value, err]` with `?` + an
  exhaustive error `match`, §8). **Migration (Gate 7):** review `examples/oop.as`, `examples/all_features.as`
  + any `.name`/`.value` goldens against the §4 contract — unit enums need no change, but the corpus is
  verified, never trimmed. Wire all new examples into the four-mode differential (both feature configs):
  construction, equality, destructure, `.value`, runtime no-arm panic.
- [ ] All four modes byte-identical; **GC test:** a cyclic recursive payload is collected (no leak);
  review; commit.

## Task 16 — Docs
**Files:** `docs/content/language/{classes-enums,errors}.md`, `README.md`, the design spec, `CLAUDE.md`,
`roadmap.md`.
- [ ] Rewrite the enum section in `docs/content/language/classes-enums.md` (algebraic variants,
  construction, the `.value`-compat contract §4) + the `match` content (variant patterns + exhaustiveness);
  note the typed-error pattern in `docs/content/language/errors.md`; update `README.md`'s feature line; the
  main design spec's enum/match sections; `CLAUDE.md` (the "Values" `EnumVariant` paragraph **and the
  `gc.rs:34` doc-comment** — drop `EnumVariant` from "immutable/acyclic … stay on Rc", note the wrapper is
  `Rc` but a `Some(payload)` is traced; the "Match pattern extensions" note + an exhaustiveness bullet);
  `roadmap.md` entry. **NAV unchanged** (content appends to existing pages — re-verify the served site).
- [ ] Review; commit.

## Done when
Every task checked behind an independent review; four-mode byte-identity holds in both configs
(`vm_differential.rs`); Gate-5 ZERO `non-exhaustive-match`/`enum-variant-binding-shadow`/`type-*` on
`examples/**` (incl. the explicit `oop.as`/`all_features.as` sibling-gather assertions); the missing-variant
exhaustiveness case is an EXERCISED `check` Error (Gate 9); unit variants byte-identical to pre-ADT
(`Rc`-cheap, no `Cc` registration; the Gate-12 unit-variant-`match` micro-benchmark shows no regression);
worker payload round-trip + cross-isolate re-interning pass; `.aso` version bumped by reading +1 (never
hardcoded); the corpus is migrated; clippy + tests green both configs; tooling parity verified (both
parsers, tree-sitter regen+publish+pins, fmt, LSP, REPL). Merge `--no-ff` to `main` (rebased onto NUM's
`Int`/`Float`; one grammar publish in this merge wave).
