# Pattern matching & exhaustiveness

This chapter specifies the `match` expression, the pattern forms it accepts, the
**bind-vs-compare** rule for identifier patterns, and **static exhaustiveness**
checking. The syntactic forms are in the [grammar chapter](grammar); enum variants
matched here are declared per the [classes chapter](classes).

## `match`

A `match` is an **expression**. It tests its subject against arms top-to-bottom and
evaluates the first arm whose pattern matches (and whose guard, if any, holds) —
**first-arm-wins**. Arms are comma-delimited. Each arm has the form
`pattern [| pattern …] [if guard] => body`: one or more **or-patterns** and an
optional boolean **guard**.

```as
let r = match 5 {
  1 | 2 => "small",
  n if n > 3 => "big",
  _ => "other",
}
print(r)                    // big
```

Internally an arm is `{ patterns, guard }`: a vector of alternative patterns and an
optional guard expression. An arm matches if **any** of its patterns matches the
subject; the guard is then evaluated with the pattern's bindings in scope, and the
arm is taken only if the guard is truthy.

## Pattern forms

A pattern is one of:

- **Wildcard** `_` — matches anything, binds nothing.
- **Identifier** `name` — see *The binding rule* below.
- **Value** — a literal (number, string, `true`/`false`/`nil`) compared with `==`.
- **Range** `start..end` / `start..=end` — membership in the range (exclusive or
  inclusive); a `step` clause makes it **strided membership** (the subject must be
  one of the strided points). Range direction follows the bounds.
- **Array** `[p0, p1, …]` with an optional trailing rest — matches an array of the
  right shape, binding elementwise; the rest collects the tail.
- **Object** `{p0, p1, …}` with an optional trailing rest — matches by key. The
  **shorthand** `{key}` always **binds** the value at `key` (it is never a
  compare). The rest collects the leftover keys.
- **Variant** — an enum variant pattern, qualified (`Shape.Circle(...)`) or bare
  (`Circle(...)`), with **positional** fields (`Pair(a, b)`) or **named** fields
  (`Circle(radius: r)`, or shorthand `Circle(radius)`).

```as
let r = match [1, 2, 3] {
  [first, ...tail] => first,
  _ => 0,
}
print(r)                    // 1
```

## The binding rule

AScript uses **Option C** for identifier patterns:

- A bare identifier that **already names an in-scope binding** is a **comparison**
  (`==` against that binding's value).
- A bare identifier with **no in-scope binding** **binds** the subject for the
  arm's body and guard.
- Object-shorthand `{key}` is **always a bind**, never a compare.

```as
let x = 5
print(match 5  { x => "compared-equal", _ => "other" })   // compared-equal
print(match 99 { x => "compared-equal", _ => "other" })   // other
print(match 7  { n => n * 2 })                            // 14  (n binds)
```

A consequence: a **unit enum variant written unqualified** is just a bare
identifier, so it **shadow-binds** (matches everything) instead of comparing
against the variant. In exhaustiveness-relevant matches, write unit variants
**qualified** (`Shape.Point`). The checker warns on the unqualified form with
`enum-variant-binding-shadow`.

## Exhaustiveness

Exhaustiveness over an **enum-typed subject** is checked **statically**. When the
checker can prove the subject is a particular enum type, a `match` that does not
cover every variant emits `non-exhaustive-match` — **default severity Error** — and
the diagnostic names the missing variant(s).

When the checker **cannot prove** the subject's enum type, the exhaustiveness check
is **gradually silent** (no diagnostic) — consistent with AScript's gradual model.

At **runtime**, a subject that matches no arm is a **Tier-2 panic**
(`no matching arm in match expression`, the `MatchNoArm` backstop) — identical on
every engine. A wildcard `_` arm or a bare-binding catch-all arm makes a match
exhaustive both statically and at runtime.

```as
enum Color { Red, Green }
fn f(c: Color) {
  match c {            // checker: non-exhaustive-match (Error) — does not cover: Green
    Color.Red => 1,
  }
}
```

## Range & strided patterns

A range pattern tests membership. With an explicit `step`, membership is
**strided**: only the strided points in the range match. Step validation
(step 0, non-finite, direction mismatch) flows through the same
`resolve_step` logic as value-position and for-loop ranges, so an invalid step is
the identical Tier-2 panic in all three positions (see the
[expressions chapter](expressions)).

## Conformance

The pattern forms and rules in this chapter are exercised by:

- `examples/pattern_matching.as` — wildcard, value, range, array, and object
  patterns with bindings.
- `examples/match_or_patterns.as` — or-patterns and guards.
- `examples/enums_adt.as` — variant patterns (positional and named) and the
  bind-vs-compare rule for qualified vs unqualified variants.
- `examples/advanced/state_machine.as` — `match`-driven state transitions over an
  enum.
- `tests/check.rs` — the static exhaustiveness pins
  (`non_exhaustive_missing_variant_is_error_naming_it`,
  `exhaustive_all_variants_is_clean`, `wildcard_catch_all_is_exhaustive`,
  `bare_binding_catch_all_is_exhaustive`).

Run each example with `target/release/ascript run examples/pattern_matching.as`
(and likewise); each matches its recorded golden. The `non-exhaustive-match` Error
above is reproduced by `target/release/ascript check` on a two-variant enum match
that omits one arm; the runtime `MatchNoArm` panic is byte-identical on the
tree-walker and the VM.
