# Lexical structure

This chapter specifies how AScript source text is decomposed into tokens. It is
grounded in the reference lexer (`src/lexer.rs`) and the tokens consumed by the
[grammar](grammar). Terms are as defined in the [notation chapter](intro).

## Source text

An AScript source file is a sequence of Unicode characters encoded as **UTF-8**.
Identifiers, string contents, and comments MAY contain non-ASCII characters; the
lexer scans by Unicode scalar value, and source positions in diagnostics are
**character** offsets (not byte offsets).

## Comments

There are two comment forms, both treated as whitespace (they separate tokens but
produce none):

- **Line comment** — `//` to the end of the line.
- **Block comment** — `/* … */`. Block comments do not nest.

```as
// a line comment
let x = 1 /* an inline block comment */ + 2
```

The grammar models these as the `line_comment` and `block_comment` extra tokens.

## Whitespace & statement separation

Whitespace (spaces, tabs, newlines) separates tokens and is otherwise
insignificant, with one exception: AScript uses **newline-terminated** statements
(an ASI-lite rule). A statement ends at a newline.

The semicolon `;` is an **optional** statement separator. It MAY appear at the end
of a statement, between statements in a block or at the top level, and between
class-body and interface-body members; leading, doubled (`;;`), and only-`;`
bodies are accepted. A `;` MUST NOT substitute for a comma `,`: enum variants,
match arms, parameters, and literal elements are comma-delimited and take no `;`.
The formatter canonicalizes statement separation to newlines.

```as
let a = 1; let b = 2   // `;` separates two statements on one line
let c = 3              // newline terminates — no `;` needed
```

## Identifiers

An identifier matches `[A-Za-z_][A-Za-z0-9_]*`: a letter or underscore, followed
by letters, digits, or underscores. Identifiers are case-sensitive.

## Keywords

Keywords are partitioned three ways.

### Reserved keywords

These words are reserved by the lexer and MUST NOT be used as identifiers. The
set is exactly:

```
true   false  nil    let    const  if     else   while
for    in     of     instanceof    return break  continue
fn     enum   match  class  interface     defer  import
export async  await  yield
```

Using a reserved word where an identifier is expected is a compile error:

```as
let interface = 1   // error: expected a name or destructuring pattern after let
```

### Contextual keywords

These words have keyword meaning **only in a specific syntactic position** and are
ordinary identifiers everywhere else:

- **`step`** — recognized only immediately after a range's end bound
  (`a..b step k`); `let step = 5` binds an ordinary variable.
- **`as`** — the rename binder in object destructuring (`{k as v}`) and the
  namespace binder in imports (`import * as m`).
- **`from`** — the source clause of an import.
- **`static`** — the class-member modifier on `fn` / `async fn` / `fn*`.
- **`worker`** — the function/method/class modifier that dispatches to an isolate.
- **`implements`** — the class conformance clause.
- **`extends`** — superclass (on a class) and interface composition (on an
  interface).

```as
let step = 5     // `step` is an ordinary identifier here
print(step)      // prints 5
```

### Soft / grammar-level identifiers

The tree-sitter grammar treats the following as plain identifiers; any semantic
restriction on them is enforced later by the resolver or interpreter, not the
lexer: `self`, `super`, `Ok`, `Err`, `recover`, and the primitive type names
(`number`, `string`, `bool`, `nil`, `any`, `object`, `error`). In type position
these names denote types; in value position `self`/`super` are bound only inside
methods.

## Literals

### Integer literals

An integer literal denotes an `int` (a 64-bit signed integer). The forms are:

- decimal — `42`, `1_000_000`
- hexadecimal — `0xFF`, `0xff`
- binary — `0b1010`
- octal — `0o17`

Underscores `_` MAY appear between digits as visual separators and are ignored.

```as
print(0xFF)        // 255
print(0b1010)      // 10
print(0o17)        // 15
print(1_000_000)   // 1000000
```

### Float literals

A numeric literal denotes a `float` (a 64-bit IEEE-754 double) when it contains a
fractional part (a `.` followed by a digit) or a decimal exponent (`e`/`E`). A `.`
not followed by a digit is NOT a fraction, so `0..5` is a range and `a.0` is a
member access.

```as
print(1.5)      // 1.5
print(1.5e3)    // 1500.0  (a float — note the trailing decimal)
print(2e-2)     // 0.02
```

A float always prints with a fractional digit and an int never does, so the two
numeric subtypes are visually distinguishable (see the *Values* chapter).

### Decimal values

AScript has an exact `decimal` value kind, but it has **no literal syntax**: there
is no `m`/`d` suffix. A `decimal` is constructed at runtime through the
`std/decimal` module (for example `decimal.parse("1.5")`, which returns a Tier-1
`[value, err]` pair). The lexer therefore recognizes no decimal token.

### String literals

A string literal is delimited by double quotes `"…"` or single quotes `'…'`. A
backslash introduces an escape (`\n`, `\t`, `\\`, `\"`, `\'`, and the other
standard escapes). The two quote forms are interchangeable; the choice only
affects which quote must be escaped inside.

```as
print("with \"quotes\" and\ttab")
print('it\'s fine')
```

### Template strings

A template string is delimited by backticks `` `…` `` and MAY contain
`${ expression }` interpolations. The interpolated expression is any expression.
Interpolations nest: a template substitution MAY itself contain string literals
and further template strings.

```as
let n = "a"
print(`outer ${`inner ${n}`} end`)   // outer inner a end
```

A backslash escape inside template text is recognized (`\n`, `\``, `\$`, …), so a
literal `${` is written `\${`:

```as
print(`esc \${n} literal`)            // esc ${n} literal
```

The grammar models template text as `template_chars`, escapes as
`template_escape`, and interpolations as `template_substitution`.

### Boolean and nil literals

The literals `true`, `false`, and `nil` denote the two booleans and the nil value.

## Operators & punctuation

The lexer recognizes the following operator and punctuation tokens.

- **Arithmetic** — `+`, `-`, `*`, `/`, `%`, `**` (exponentiation), and the
  wrapping forms `+%`, `-%`, `*%`.
- **Bitwise / shift** — `&`, `|`, `^`, `~`, `<<`, `>>`.
- **Comparison** — `==`, `!=`, `<`, `<=`, `>`, `>=`.
- **Logical** — `&&`, `||`, `!`.
- **Assignment** — `=`, `+=`, `-=`, `*=`, `/=`.
- **Ranges** — `..` (exclusive), `..=` (inclusive).
- **Spread / rest** — `...`.
- **Access & nullish** — `.`, `?.` (safe member access), `??` (nullish
  coalescing).
- **Postfix Result forms** — `?` (propagation / ternary), `!` (force-unwrap).
- **Arrow** — `=>`.
- **Grouping & punctuation** — `(` `)`, `[` `]`, `{` `}`, `#{` (map-literal
  opener), `,`, `:`, `;`, `` ` ``.

The `?` token is overloaded between ternary and Result-propagation, and `!` is
both prefix logical-not and postfix force-unwrap; these are disambiguated by the
[grammar](grammar), not the lexer.

## Conformance

The token forms in this chapter are exercised by:

- `tests/frontend_conformance.rs` — asserts the two front-ends tokenize and parse
  the example corpus identically.
- `examples/strings.as` — string and template-string literals, including escapes
  and nested interpolation.
- `examples/numbers.as` — integer literal forms (`0x`/`0b`/`0o`/underscores).
- `examples/integers.as` — integer literal forms and `int` semantics.

Run them with `target/release/ascript run examples/strings.as` (and likewise for
`numbers.as`, `integers.as`); each matches its recorded golden.
