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
always allowed but never required.

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

## Operators

```text
+  -  *  /  %  **        arithmetic ( ** is exponentiation )
== != <  <= >  >=        comparison
&& || !                  logical (short-circuit)
?? ?.                    nil-coalescing / optional chaining
=  += -= *= /=           assignment (and compound)
?                        postfix Result propagation (see Errors)
..                       range
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
