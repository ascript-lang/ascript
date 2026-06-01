:::eyebrow Language

# Syntax & control flow

AScript's grammar is small enough to fit on one page. If you read JavaScript, almost everything here
is already familiar.

## Lexical structure

Source files are UTF-8 and use the `.as` extension.

```ascript
// line comment
/* block comment */
```

Statements are newline-terminated with lightweight automatic semicolon insertion. An explicit `;` is
always allowed but never required. The same optional `;` may separate **class members**
(`class P { x: number; y: number }`); newlines are the canonical form and the formatter normalizes
`;` back to newlines. Note that `enum` variants are **comma**-separated, not `;`-separated.

**Keywords:** `let const fn return if else while for of in match async await class extends super self
enum import export nil true false`.

## Bindings

```ascript
let count = 0          // mutable
const name = "Ada"     // immutable — rebinding is an error

let x: number = 3      // optional type annotation (see Type contracts)
let later              // declared without an initializer; value is nil
```

`const` forbids rebinding the name; it does not deep-freeze the value (a `const` array can still have
elements pushed).

### Object destructuring

A `let` (or `const`) binding can pull several fields out of an object or class instance at once, by
key name:

```ascript
let user = { name: "Ada", role: "admin", "login count": 42 }

let {name, role} = user            // shorthand: binds `name` and `role`
let {role as r} = user             // rename: binds `r` from key `role`
let {"login count" as logins} = user  // quoted key for non-identifier names

let {missing} = user               // key not present → binds nil
```

Each entry is `key` or `key as local`; the key is `Ident | Str` (quote any key that is not a bare
identifier), and `as` renames it to a local. Missing keys bind `nil` rather than erroring.
Destructuring works on plain `Object` values and on class instances alike:

```ascript
class Point { x: number; y: number }
let p = Point.from({x: 3, y: 4})
let {x, y} = p                     // x = 3, y = 4
```

### Spread

`...expr` expands a collection inline in three contexts — array literals, object literals, and call
arguments:

```ascript
let base = [1, 2, 3]
let more = [0, ...base, 4]         // [0, 1, 2, 3, 4]

let defaults = {host: "local", port: 80}
let config = {...defaults, port: 443}  // {host: "local", port: 443}

fn sum3(a, b, c) { return a + b + c }
let nums = [10, 20, 30]
print(sum3(...nums))               // 60
```

Spread is **strict**: an array spread (`[...x]` or `f(...x)`) requires `x` to be an array, and an
object spread (`{...x}`) requires `x` to be an object — there is no array↔object coercion, and
spreading the wrong container kind is a runtime panic.

In an object literal, spread is **later-value-wins**: a key written after a `...` overrides the
spread-in value, and a `...` after an explicit entry overrides that. A key keeps its **first-seen
position** in the result (so `{...a, k: v}` updates `k` in place if `a` already had it, rather than
moving it to the end).

### Rest in destructuring

A destructuring pattern may end with a `...name` rest collector that gathers whatever the named
entries did not bind. In an array pattern it collects the trailing elements into a new array; in an
object pattern it collects the leftover keys into a new object:

```ascript
let [head, ...tail] = [10, 20, 30]  // head = 10, tail = [20, 30]
let [only, ...rest] = [42]          // only = 42, rest = []  (empty when nothing remains)

let {id, ...meta} = {id: 7, role: "admin", active: true}
// id = 7, meta = {role: "admin", active: true}
```

The rest collector must be **last** in the pattern. Array-rest takes the elements past the named
positions (an empty array `[]` when there are none). Object-rest takes every source key **not already
bound** by an earlier entry — including keys reached through `as` renames — preserving their original
order, and is `{}` when nothing is left over.

## Operators

```text
+  -  *  /  %  **        arithmetic ( ** is exponentiation )
== != <  <= >  >=        comparison
&& || !                  logical (short-circuit)
?? ?.                    nil-coalescing / optional chaining
?  :                     conditional (ternary)  — cond ? then : else
=  += -= *= /=           assignment (and compound)
?                        postfix Result propagation (see Errors)
..                       range (exclusive)
..=                      inclusive range — used in match patterns and for loops
```

**Precedence**, highest to lowest:

```text
()  []  .  ?            grouping · call · index · member · postfix-?
!  -                    unary
**                      exponentiation
*  /  %                 multiplicative
+  -                    additive
<  <= >  >=             relational
== !=                   equality
&&                      logical and
||                      logical or
??                      nil-coalescing
=  += -= *= /=          assignment
```

`&&`, `||`, and `??` short-circuit — the right operand is only evaluated when needed.

## Control flow

```ascript
if (count > 0) {
  print("positive")
} else if (count == 0) {
  print("zero")
} else {
  print("negative")
}

while (count < 3) {
  count += 1
}
```

There are two `for` forms — `in` walks a numeric range, `of` walks the values of an iterable:

```ascript
for (i in 0..10) {            // half-open range: 0,1,…,9
  print(i)
}

for (item of [10, 20, 30]) {  // values of an array (or characters of a string)
  print(item)
}
```

`break` and `continue` work inside both loop forms. The range operator `..` is half-open and
produces an iterable; `range(...)` (a builtin) produces an actual array when you need one.

## Functions

```ascript
fn add(a, b) {
  return a + b
}

const double = (x) => x * 2                  // arrow, expression body
const greet  = (who) => { return `hi ${who}` } // arrow, block body
```

Functions are first-class **closures** — they capture the environment where they are defined:

```ascript
fn counter() {
  let n = 0
  return () => {
    n += 1
    return n
  }
}

let next = counter()
print(next())   // 1
print(next())   // 2
```

A function may be `async` (see [Modules & async](modules-async)). Arrow functions may be async too:
`async (x) => x + 1`.

### Rest parameters

A function's final parameter may be a `...name` **rest parameter** that collects any trailing
arguments into an array:

```ascript
fn tagged(label, ...rest) {
  print(label)   // "nums"
  print(rest)    // [1, 2]
}
tagged("nums", 1, 2)
```

When no extra arguments are passed, the rest parameter binds an empty array `[]`. A rest parameter
must be **last** — nothing may follow it. It may carry a type annotation, which is always written as
`array<T>` and is **checked per element** against `T`:

```ascript
fn sum(...nums: array<number>) {
  let total = 0
  for (n in nums) { total = total + n }
  return total
}
print(sum(1, 2, 3, 4))   // 10
print(sum())             // 0
```

Rest pairs naturally with [spread](#spread) for argument forwarding — `fn wrap(...args) { return
sum(...args) }` collects then re-expands. For `async` functions and generators (`fn*`), arity and
contract errors on a rest parameter surface **lazily**, when the returned future or generator is
driven, rather than at the call site.

## Template strings

Backtick strings interpolate `${expr}`:

```ascript
let user = { name: "Ada", role: "admin" }
print(`${user.name} (${user.role})`)   // Ada (admin)
```

## Safe access: `?.` and `??`

Reading a field of `nil` is a panic. Optional chaining and nil-coalescing are the opt-in safe forms:

```ascript
let cfg = { db: { port: 5432 } }

cfg?.db?.port           // 5432
cfg?.cache?.ttl         // nil — the chain short-circuits, no panic
cfg?.cache?.ttl ?? 60   // 60 — fall back to a default
```

> [!NOTE] Reading a *missing* key of an existing object returns `nil` (not a panic) — e.g.
> `({a: 1}).b` is `nil`. Only a `nil` *receiver* and an out-of-bounds `arr[i]` index panic. Use
> `?.` when the receiver itself might be `nil`.

## Conditional expressions (the ternary)

`cond ? then : else` is an **expression** that evaluates `cond` and yields one of the two branches —
only the selected branch runs (like `&&`/`||`, it short-circuits).

```ascript
let label = score >= 90 ? "A" : "B"
let access = user == nil ? "guest" : user.role

// Use it inline, e.g. inside a template:
print(`status: ${ok ? "up" : "down"}`)
```

It binds looser than every other operator except assignment, and is **right-associative**, so it
chains cleanly as an `if`/`else if`/`else` ladder:

```ascript
let sign = n < 0 ? "negative"
         : n == 0 ? "zero"
         : "positive"
```

> [!NOTE] `?` is overloaded: as a ternary here, and as the postfix
> [Result-propagation](errors#the-propagation-operator) operator (`expr?`). They never collide —
> a `?` is a ternary only when a `:` follows it; otherwise it propagates. So `a ? -b : c` is a
> ternary, while `f()? - 1` propagates the result of `f()` and then subtracts. For the verbose case
> with many branches, [`match`](classes-enums#match) is often clearer.
