# Expressions & operators

This chapter specifies the **evaluation** of AScript expressions: the operators,
their precedence and associativity, and the semantics of each form. The
[grammar chapter](grammar) gives the syntactic precedence ladder; this chapter
restates it and adds runtime meaning. Value semantics for the operands are in the
[values chapter](values).

## Evaluation order

Expression evaluation is **left-to-right**. In a binary expression the left
operand evaluates before the right; in a call the callee evaluates before its
arguments, and arguments evaluate left-to-right.

The logical operators **short-circuit**: `a && b` does not evaluate `b` when `a`
is falsy, `a || b` does not evaluate `b` when `a` is truthy, and `a ?? b` does
not evaluate `b` when `a` is non-nil. An optional member access `a?.m` whose
receiver is `nil` yields `nil` without evaluating the member.

## The precedence ladder

The operators bind in the order below, loosest (tier 1) to tightest. This table
mirrors the [grammar chapter](grammar)'s `PREC` transcription; a higher tier
binds tighter and is evaluated first when unparenthesized.

| Tier | Level | Operators | Assoc. |
| --- | --- | --- | --- |
| 1 | assign | `=` `+=` `-=` `*=` `/=` | right |
| 2 | ternary | `cond ? a : b` | right |
| 3 | coalesce | `??` | left |
| 4 | or | `\|\|` | left |
| 5 | and | `&&` | left |
| 6 | equality | `==` `!=` | left |
| 7 | compare | `<` `<=` `>` `>=` `instanceof` | left |
| 8 | bitor | `\|` `^` | left |
| 9 | range | `..` `..=` (`step`) | left |
| 10 | add | `+` `-` `+%` `-%` | left |
| 11 | mul | `*` `/` `%` `*%` `<<` `>>` `&` | left |
| 12 | exp | `**` | right |
| — | postfix `?`/`!` | _precedence-less (GLR)_ | — |
| 14 | unary | prefix `!` `-` `~`, `await` | right |
| 15 | postfix | call, `.`, `?.`, `[]` | left |
| 16 | primary | literals, grouping, `match` | — |

## Arithmetic

The arithmetic operators `+ - * / % **` and unary `-` operate on numbers (and `+`
also concatenates strings). Their numeric behavior — type-directed division,
overflow trapping, the wrapping operators `+% -% *%`, and int-only bitwise/shift
— is specified in the [values chapter](values). Division or modulo by zero is a
Tier-2 panic.

## Comparison & `instanceof`

The relational operators `< <= > >=` compare numbers (and strings
lexicographically); `==`/`!=` follow the equality rules in the
[values chapter](values).

`x instanceof RHS` requires a class, an interface, or a reserved type name
(`int`, `float`, `number`, `string`, `bool`) on the right:

- a **class** → a nominal chain walk;
- an **interface** → a structural conformance check (name + arity);
- a **reserved type name** → a runtime type guard on the value's kind.

Any other right-hand side is a Tier-2 panic. A left-hand operand that is not an
instance never panics — it simply yields `false` for a class/interface RHS.

## Result propagation `?`

A postfix `expr?` is **Result-propagation**: when `expr` is a `[value, err]`
pair whose error is non-nil, `?` early-returns `[nil, err]` from the enclosing
function; otherwise it evaluates to the `value`. Using `?` where the enclosing
function cannot return a pair is a compile-time error (see the *Errors* chapter).

`?` is disambiguated from the ternary by a following `:`: an `expr ?` is a
ternary condition iff a `:` follows at bracket-depth 0 before the statement ends;
otherwise it is propagation.

```as
fn g(x) { return [x, nil] }
async fn m() {
  let r = g(5)? - 1     // propagate-then-subtract: g(5) yields 5, then 5 - 1
  print(r)              // 4
}
await m()
```

## Force-unwrap `!`

A postfix `expr!` **force-unwraps** a `[value, err]` pair: it evaluates to
`value` when the error is nil, and otherwise raises a *recoverable* Tier-2 panic
carrying the original error message.

```as
fn f() { return [7, nil] }
print(f()!)            // 7
```

Both `?` and `!` occupy a single precedence-less tier between `**` (tier 12) and
the unary tier, so they bind **looser** than `await` and prefix `!`/`-`. Thus
`await x!` parses as `(await x)!`:

```as
async fn f() { return [7, nil] }
async fn m() { print(await f()!) }   // (await f())!  →  7
await m()
```

## Ternary

`cond ? a : b` evaluates `cond`; if truthy it evaluates and yields `a`, otherwise
`b` (only the chosen branch evaluates). The condition extends only to the `:` at
bracket-depth 0, so a leading unary in a branch is unambiguous:

```as
let c = true
let b = 3
print(c ? -b : 9)      // -3
```

## Ranges

`a..b` is an **exclusive** range and `a..=b` an **inclusive** one. The direction
follows the bounds, so `10..1` counts down. A trailing `step k` is a signed
stride. A step of `0`, a non-finite step, or a step whose sign contradicts the
range direction is a Tier-2 panic. The same range rules apply in a `for` range,
in value position (where a range materializes to `array<number>`), and in a match
pattern (where it is a strided membership test).

```as
print(1..4)            // [1, 2, 3]
print(1..=4)           // [1, 2, 3, 4]
print(10..1 step -3)   // [10, 7, 4]  (counts down)
```

## Spread `...`

The spread operator expands a container into an array literal, an object literal,
or a call's argument list. Spreading the wrong container kind is a Tier-2 panic.
Object spread is **later-value-wins** while preserving each key's first-seen
position.

```as
let a = [1, 2]
print([...a, 3])                     // [1, 2, 3]
print({ ...{x: 1}, x: 2 })           // {x: 2}   (later value wins)
```

## Member access & calls

`a.m` reads a member; `a?.m` reads it only when `a` is non-nil, else yields `nil`
(skipping the member entirely); `a[k]` indexes by an arbitrary key. A call
`f(args)` may include spread arguments (`f(...xs)`) and named arguments
(`name: value`, meaningful for enum-variant construction). Explicit type
arguments on a call (`Box<int>(5)`) are runtime-erased.

## Safe access & nil-coalescing

`a ?? b` evaluates to `a` when `a` is non-nil, otherwise to `b` (short-circuit).
Combined with `?.`, this gives nil-safe navigation:

```as
let user = { name: nil }
print(user?.name ?? "anonymous")     // anonymous
```

## Anonymous function expressions & arrows

A closure value may be written two ways:

- an **arrow** `(params) => body`, where `body` is a single expression or a
  block, with a single bare parameter permitted (`x => x * x`);
- an **anonymous function expression** `fn(params) { body }`, the named-less
  sibling of a function declaration.

```as
let add = fn(a, b) { return a + b }
print(add(2, 3))                     // 5
let sq = (x) => x * x
print(sq(4))                         // 16
```

An anonymous function expression desugars to the same closure as a block-bodied
arrow. Unlike a `fn` *declaration*, it **cannot declare a return type**: `fn(x):
T { … }` is a syntax error (`anonymous function expressions cannot declare a
return type`). Use a named `fn` declaration when an enforced return contract is
needed. A function expression carries a `fn(){}` body and is usable directly as a
call argument:

```as
let r = recover(fn() { assert(false, "boom") })   // a recoverable panic
print(r[1].message)                                // boom
```

## `await`

`await expr` drives a future to completion and yields its result. `await` on a
non-future value is the **identity** (it yields the value unchanged). It binds at
the unary tier; concurrency semantics are in the *Concurrency* chapter.

## Template strings

A template string `` `…${expr}…` `` evaluates each interpolation `${expr}` and
concatenates the textual pieces. Interpolations nest (an interpolation may
contain string and template literals). The lexical form is in the
[lexical chapter](lexical).

## Conformance

The operator semantics and disambiguation rules in this chapter are exercised by:

- `examples/all_features.as` — a broad sweep of operators, ternary, ranges, and
  spread.
- `examples/ranges.as`, `examples/range_step_default.as` — range and `step`
  semantics in value, `for`, and match positions.
- `examples/instanceof.as` — `instanceof` as a class/interface/type-name guard.
- `examples/force_unwrap.as` — the `!` force-unwrap of a `[value, err]` pair.
- `examples/spread.as` — spread into arrays, objects, and call arguments.
- `tests/vm_differential.rs` — the `?`/ternary disambiguation and short-circuit
  evaluation are pinned four-mode byte-identically by
  `vm_propagate_then_compare_then_ternary_matches_treewalker`,
  `vm_ternary_keyword_then_branch_matches_treewalker`, and
  `vm_short_circuit_does_not_evaluate_rhs`.

Run the examples with `target/release/ascript run examples/force_unwrap.as` (and
likewise); each matches its recorded golden.
