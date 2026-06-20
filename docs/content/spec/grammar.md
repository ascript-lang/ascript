# Grammar (EBNF)

This chapter is the **normative grammar** of AScript, derived from the syntax
source of truth `tree-sitter-ascript/grammar.js`. Each production block is
followed by a `covers:` anchor naming the tree-sitter rules it formalizes; the
union of all `covers:` anchors equals the full rule inventory (the `spec_drift`
test enforces that every named rule appears here verbatim). Terminals are quoted;
the [lexical chapter](lexical) defines the token classes the terminals stand for.

> **Limitation — read this.** The drift test proves every tree-sitter rule is
> COVERED by a production here; it does NOT prove the two grammars generate the
> same language. The EBNF is the normative *form*; the tree-sitter rule names are
> the *anchor*. Language equivalence is pinned empirically by the conformance
> suite: both parsers (and the legacy front-end) accept the entire corpus
> (`tests/treesitter_conformance.rs`, `tests/frontend_conformance.rs`). A
> generated grammar is deliberately rejected in favor of this hand-written one.

## Notation

ISO-style EBNF is used:

| Form | Meaning |
| --- | --- |
| `=` | a production defines a rule |
| `\|` | alternation (one of) |
| `[ x ]` | optional — zero or one `x` |
| `{ x }` | repetition — zero or more `x` |
| `( … )` | grouping |
| `"…"` | a literal terminal |
| _italic_ / lowercase | a non-terminal |

Two helpers from the grammar are written inline:

```ebnf
commaSep(x)   = [ x { "," x } ]            (* zero or more, comma-separated *)
commaSep1(x)  = x { "," x }                (* one or more, comma-separated *)
```

## Program & items

A program is a sequence of items. An item is an import, an export, or a
statement.

```ebnf
source_file = { item } ;
item        = import_declaration | export_declaration | statement ;
```

`covers: source_file, _item`

Comments are lexical extras (see [lexical](lexical)) and produce no items.

```ebnf
line_comment  = "//" { any-char-but-newline } ;
block_comment = "/*" { any-char } "*/" ;
```

`covers: line_comment, block_comment`

## Modules

```ebnf
import_declaration = "import" ( "{" commaSep( import_specifier ) [ "," ] "}"
                              | "*" "as" identifier )
                     "from" string [ ";" ] ;
import_specifier   = identifier ;

export_declaration = "export" ( let_declaration | const_declaration
                              | function_declaration | class_declaration
                              | enum_declaration | interface_declaration ) ;
```

`covers: import_declaration, import_specifier, export_declaration`

There are no default exports; an `export` prefixes a single declaration. The
[modules](../language/modules-async) guide covers resolution semantics.

## Statements

```ebnf
statement = let_declaration | const_declaration | function_declaration
          | class_declaration | enum_declaration | interface_declaration
          | if_statement | while_statement | for_statement
          | return_statement | break_statement | continue_statement
          | defer_statement | block | expression_statement ;

block                = "{" { statement } "}" ;
expression_statement = expression [ ";" ] ;
```

`covers: _statement, block, expression_statement`

### Bindings

```ebnf
let_declaration   = "let" binding_target [ ":" type ] [ "=" expression ] [ ";" ] ;
const_declaration = "const" binding_target [ ":" type ] "=" expression [ ";" ] ;

binding_target = identifier | array_pattern | object_pattern ;
```

`covers: let_declaration, const_declaration, _binding_target`

A `const` requires an initializer; a `let` does not. The binding target MAY be a
destructuring pattern:

```ebnf
array_pattern        = "[" [ rest_element
                           | commaSep1( identifier ) [ "," rest_element ] [ "," ] ] "]" ;
object_pattern       = "{" [ rest_element
                           | commaSep1( object_pattern_entry ) [ "," rest_element ] [ "," ] ] "}" ;
object_pattern_entry = ( identifier | string ) [ "as" identifier ] ;
rest_element         = "..." identifier ;
```

`covers: array_pattern, object_pattern, object_pattern_entry, rest_element`

## Declarations

### Functions

```ebnf
function_declaration = [ worker_keyword ] [ "async" ] "fn" [ "*" ]
                       identifier [ type_parameters ]
                       parameter_list [ ":" type ] block ;

parameter_list = "(" [ rest_parameter
                     | commaSep1( parameter ) [ "," rest_parameter ] [ "," ] ] ")" ;
parameter      = identifier [ ":" type ] [ "=" expression ] ;
rest_parameter = "..." identifier [ ":" type ] ;
```

`covers: function_declaration, parameter_list, parameter, rest_parameter`

`fn*` (or `async fn*`) declares a generator. A defaulted parameter MUST NOT be
followed by a non-defaulted one; the rest parameter MUST be last.

### Generic type parameters

```ebnf
type_parameters = "<" commaSep1( type_parameter ) [ "," ] ">" ;
type_parameter  = identifier [ type_bound ] ;
type_bound      = ":" type ;
```

`covers: type_parameters, type_parameter, type_bound`

Type parameters are checked statically and erased at runtime (see the
*Types* chapter). A bound is interface-constrained by the checker; the grammar
accepts any type.

### Classes

```ebnf
class_declaration = [ worker_keyword ] "class" identifier [ type_parameters ]
                    [ "extends" identifier ] [ implements_clause ] class_body ;
implements_clause = "implements" commaSep1( identifier ) ;

class_body   = "{" { ";" } { class_member { ";" } } "}" ;
class_member = field_declaration | method_definition ;

field_declaration = identifier [ "?" ] ":" type [ "=" expression ] ;
method_definition = [ static_keyword ] [ worker_keyword ] [ "async" ] "fn" [ "*" ]
                    identifier parameter_list [ ":" type ] block ;
```

`covers: class_declaration, implements_clause, class_body, class_member, field_declaration, method_definition`

`worker class` declares a dedicated-isolate actor. A field declaration's optional
`?` marks the field nullable (`name?: T`, equivalent to `name: T?`). `static_keyword`
and `worker_keyword` are the contextual modifiers `static` and `worker`.

### Enums

```ebnf
enum_declaration = "enum" identifier [ type_parameters ]
                   "{" commaSep( enum_variant ) [ "," ] "}" ;
enum_variant     = identifier [ "=" expression
                              | "(" commaSep1( variant_field ) [ "," ] ")" ] ;
variant_field    = [ identifier ":" ] type ;
```

`covers: enum_declaration, enum_variant, variant_field`

A variant is unit, scalar-backed (`= value`), or payload-carrying (`(…)`); the
backing and payload forms are mutually exclusive (the hand front-ends enforce the
XOR). Payload fields are uniformly named (`Circle(radius: float)`) XOR positional
(`Pair(int, int)`).

### Interfaces

```ebnf
interface_declaration = "interface" identifier [ type_parameters ]
                        [ "extends" commaSep1( identifier ) ] interface_body ;
interface_body        = "{" { ";" } { method_requirement { ";" } } "}" ;
method_requirement    = "fn" identifier parameter_list [ ":" type ] ;
```

`covers: interface_declaration, interface_body, method_requirement`

An interface is a named set of method signatures with no bodies; `extends`
composes (unions) the requirements of other interfaces. Modifiers
(`static`/`worker`/`async`/`fn*`) on a requirement are rejected.

## Control flow

```ebnf
if_statement       = "if" "(" expression ")" block
                     [ "else" ( block | if_statement ) ] ;
while_statement    = "while" "(" expression ")" block ;
for_statement      = "for" [ "await" ] "(" identifier ( "of" | "in" ) expression ")" block ;
return_statement   = "return" [ expression ] [ ";" ] ;
break_statement    = "break" [ ";" ] ;
continue_statement = "continue" [ ";" ] ;
defer_statement    = "defer" [ "await" ] call_expression [ ";" ] ;
```

`covers: if_statement, while_statement, for_statement, return_statement, break_statement, continue_statement, defer_statement`

`for (x of iter)` iterates values; `for (i in a..b)` iterates a range;
`for await` drives an async stream. A `defer` operand MUST be a call expression
(call-only, enforced at parse time).

## Expressions

```ebnf
expression = assignment_expression | ternary_expression | binary_expression
           | range_expression | unary_expression | await_expression
           | yield_expression | match_expression | arrow_function
           | function_expression | postfix_expression ;
```

`covers: _expression`

### Precedence & associativity

The precedence ladder, from loosest (1) to tightest (16) binding, is transcribed
from the grammar's `PREC` table. A higher number binds tighter.

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
| (13) | — | _(free: postfix `?`/`!` are precedence-less)_ | — |
| 14 | unary | prefix `!` `-` `~`, `await` | right |
| 15 | postfix | call, member, index, optional-member | left |
| 16 | primary | literals, grouping, `match`, object/map literal | — |

The bitwise-OR tier sits between comparison and range (Go's binding), so
`a | b == c` parses as `(a | b) == c`. Shifts and bitwise-`&` sit at the
multiplicative tier.

```ebnf
assignment_expression = postfix_expression ( "=" | "+=" | "-=" | "*=" | "/=" ) expression ;

binary_expression = expression binary_operator expression ;
binary_operator   = "??" | "||" | "&&" | "==" | "!=" | "<" | "<=" | ">" | ">="
                  | "instanceof" | "|" | "^" | "+" | "-" | "+%" | "-%"
                  | "*" | "/" | "%" | "*%" | "<<" | ">>" | "&" | "**" ;
```

`covers: assignment_expression, binary_expression`

### Ranges

```ebnf
range_expression = expression ( ".." | "..=" ) expression [ step_keyword expression ] ;
step_keyword     = "step" ;
static_keyword   = "static" ;
worker_keyword   = "worker" ;
```

`covers: range_expression, step_keyword, static_keyword, worker_keyword`

`..` is exclusive, `..=` inclusive; a trailing `step k` is a signed stride. `step`
is contextual (recognized only here); `static` and `worker` are the contextual
member/declaration modifiers.

### Unary, await, yield

```ebnf
unary_expression = ( "!" | "-" | "~" ) expression ;
await_expression = "await" expression ;
yield_expression = "yield" [ expression ] ;
```

`covers: unary_expression, await_expression, yield_expression`

Prefix `!`/`-`/`~` and `await` bind at the unary tier; `yield` binds at the
assignment tier and takes an optional operand.

### Match

```ebnf
match_expression       = "match" match_subject "{" commaSep( match_arm ) [ "," ] "}" ;
match_arm              = match_pattern [ "if" expression ] "=>" expression ;
match_subject          = binary_expression | range_expression
                       | unary_expression | postfix_expression ;
```

`covers: match_expression, match_arm, _match_subject`

The subject excludes a bare object literal so the trailing `{` opens the arm
block. The optional `if` guard runs after a structural match. Pattern grammar is
in [Patterns](#patterns) below.

### Anonymous function & arrow expressions

```ebnf
function_expression = [ "async" ] "fn" parameter_list block ;

arrow_function = [ "async" ] parameter_list "=>" ( block | expression )
               | [ "async" ] identifier      "=>" ( block | expression ) ;
```

`covers: function_expression, arrow_function`

A `function_expression` is an anonymous `fn(params){body}` value — the named-less
sibling of `function_declaration`. The token AFTER `fn` decides: `fn (` is a
function expression, `fn name` is a declaration. A function expression carries NO
return-type slot (`fn(x): T {…}` is a syntax error). `fn*` stays
declaration-only. An arrow function MAY take a single bare-identifier parameter or
a parenthesized list, and its body is a block or a single expression.

### Postfix chain

```ebnf
postfix_expression = call_expression | member_expression
                   | optional_member_expression | index_expression
                   | unwrap_expression | propagate_expression
                   | primary_expression ;

call_expression            = postfix_expression arguments
                           | postfix_expression type_arguments arguments ;
arguments                  = "(" commaSep( named_argument | expression | spread_element ) [ "," ] ")" ;
named_argument             = identifier ":" expression ;
spread_element             = "..." expression ;
member_expression          = postfix_expression "." identifier ;
optional_member_expression = postfix_expression "?." identifier ;
index_expression           = postfix_expression "[" expression "]" ;
propagate_expression       = postfix_expression "?" ;
unwrap_expression          = postfix_expression "!" ;
type_arguments             = "<" commaSep1( type ) [ "," ] ">" ;
```

`covers: _postfix_expression, call_expression, arguments, named_argument, spread_element, member_expression, optional_member_expression, index_expression, propagate_expression, unwrap_expression, type_arguments`

A `named_argument` (`name: value`) is meaningful only for enum-variant
construction. `type_arguments` on a call (`Box<int>(5)`) are runtime-erased.

### Ternary

```ebnf
ternary_expression = expression "?" expression ":" expression ;
```

`covers: ternary_expression`

### Primary expressions & literals

```ebnf
primary_expression       = identifier | number | string | template_string
                         | boolean | nil | array_literal | object_literal
                         | map_literal | parenthesized_expression ;
parenthesized_expression = "(" expression ")" ;

array_literal  = "[" commaSep( expression | spread_element ) [ "," ] "]" ;
object_literal = "{" commaSep( object_entry | spread_element ) [ "," ] "}" ;
object_entry   = ( identifier | string ) ":" expression ;
map_literal    = "#{" commaSep( map_entry ) [ "," ] "}" ;
map_entry      = expression ":" expression ;
```

`covers: _primary_expression, parenthesized_expression, array_literal, object_literal, object_entry, map_literal, map_entry`

An `object_literal` key is an identifier or string and its value is an
expression; a `map_literal` (`#{ … }`) key is itself an expression (its value is
the key). Spread is permitted in array and object literals but not in a map
literal.

The literal token rules:

```ebnf
identifier       = /[A-Za-z_][A-Za-z0-9_]*/ ;
number           = hex | binary | octal | float | integer ;
string           = '"' { string-char } '"' | "'" { string-char } "'" ;
template_string  = "`" { template_chars | template_escape | template_substitution } "`" ;
template_chars   = /[^`$\\]+/ ;
template_escape  = /\\./ ;
template_substitution = "${" expression "}" ;
boolean          = "true" | "false" ;
nil              = "nil" ;
```

`covers: identifier, number, string, template_string, template_chars, template_escape, template_substitution, boolean, nil`

See [lexical](lexical) for the integer/float forms and escape rules.

## Patterns

A match arm's pattern is one or more `|`-separated single patterns.

```ebnf
match_pattern        = or_pattern | match_pattern_single ;
or_pattern           = match_pattern_single { "|" match_pattern_single } ;
match_pattern_single = wildcard_pattern | array_pattern_match | object_pattern_match
                     | variant_pattern | identifier_pattern | match_subject ;

wildcard_pattern     = "_" ;
identifier_pattern   = identifier ;
array_pattern_match  = "[" [ rest_element
                           | commaSep1( match_pattern_single ) [ "," rest_element ] [ "," ] ] "]" ;
object_pattern_match = "{" [ rest_element
                           | commaSep1( object_pattern_match_entry ) [ "," rest_element ] [ "," ] ] "}" ;
object_pattern_match_entry = ( identifier | string ) [ ":" match_pattern_single ] ;
variant_pattern       = ( identifier | member_expression ) "(" commaSep1( variant_pattern_field ) [ "," ] ")" ;
variant_pattern_field = identifier [ ":" match_pattern_single ] ;
```

`covers: _match_pattern, or_pattern, _match_pattern_single, wildcard_pattern, identifier_pattern, array_pattern_match, object_pattern_match, object_pattern_match_entry, variant_pattern, variant_pattern_field`

A literal, enum-variant, member, call, or **range** value pattern flows through
the `match_subject` branch (a range pattern reuses `range_expression`; a
positional variant pattern reuses `call_expression`). The named-variant form
`variant_pattern` (`Rect(w: ww)`) is the only payload-pattern that needs a
dedicated rule. The binding rule (compare-if-defined vs bind-if-new) is in the
*Patterns* chapter.

## Types

```ebnf
type      = union_type | type_atom ;
union_type = type_atom { "|" type_atom } ;
type_atom = optional_type | primitive_type | array_type | map_type
          | result_type | future_type | tuple_type | generic_type
          | function_type | identifier ;

primitive_type = "number" | "string" | "bool" | "nil" | "any" | "fn" | "object" | "error" ;
array_type     = "array" "<" type ">" ;
map_type       = "map" "<" type "," type ">" ;
result_type    = "Result" "<" type ">" ;
future_type    = "future" "<" type ">" ;
tuple_type     = "[" commaSep1( type ) [ "," ] "]" ;
generic_type   = identifier "<" commaSep1( type ) [ "," ] ">" ;
function_type  = "fn" "(" [ commaSep1( type ) [ "," ] ] ")" "->" type ;
optional_type  = ( primitive_type | array_type | map_type | result_type
                 | future_type | tuple_type | identifier ) "?" ;
```

`covers: _type, union_type, _type_atom, primitive_type, array_type, map_type, result_type, future_type, tuple_type, generic_type, function_type, optional_type`

`T?` is sugar for `T | nil`. A bare `identifier` in type position is a class,
enum, interface, or in-scope type-parameter name. A `generic_type`
(`Box<int>`) is a user generic application; `function_type` (`fn(A) -> B`) is a
parameterized function type whose `->` lexes as `-` `>`.

## Ambiguities resolved by GLR

The grammar uses GLR to keep genuinely-ambiguous parses alive until a later token
decides. The hand-written front-ends make the same decisions.

- **`?` propagation vs ternary.** `expr ?` is a ternary condition iff a `:`
  follows at bracket-depth 0 before the statement ends; otherwise it is
  Result-propagation. So `a ? -b : c` is ternary, but `f()? - 1` is
  propagate-then-subtract. The `propagate_expression` rule is precedence-less; a
  declared conflict keeps both readings alive until the `:` (or its absence)
  decides.

- **`!` unwrap precedence.** `propagate_expression` and `unwrap_expression` are
  deliberately precedence-less and bind LOOSER than `await` and prefix `!`/`-`:
  `await x!` parses as `(await x)!`. Position disambiguates postfix `!`
  (force-unwrap) from prefix `!` (logical not) and `!=` (one token).

- **`fn name` declaration vs `fn(` expression.** The token after `fn` decides:
  `fn (` opens a `function_expression`, `fn name` a `function_declaration`.
  Tree-sitter resolves this with static LR lookahead — no declared conflict.

- **Parameter list vs parenthesized expression.** `(x)` is held ambiguous between
  a one-parameter arrow `parameter` list and a `parenthesized_expression` until a
  following `=>` (arrow) or its absence (grouping) decides.

- **Match guard vs arrow.** A guard ending in a bare identifier right before the
  arm's `=>` (`n if n == lim => …`) is ambiguous between the arm separator and a
  single-param arrow; the precedence-less single-identifier `arrow_function` form
  makes this a dynamic conflict that the arm-completing parse wins.

- **`step` contextual keyword.** `step` is recognized only immediately after a
  range's end bound, so `let step = 1` and `fn step(n)` keep `step` as an
  identifier (settled by the declared range conflict).

- **Explicit type-arg call vs comparison.** `callee<T>(args)` is a generic call,
  but `a < b > (c)` is a comparison chain; the trailing `(arguments)` after the
  matching `>` selects the type-argument reading.

## Conformance

The grammar's coverage and the disambiguation rules are exercised by:

- `tests/treesitter_conformance.rs` — the tree-sitter grammar accepts the example
  corpus (and agrees with the hand-written parser on it).
- `tests/frontend_conformance.rs` — the legacy and CST front-ends parse the
  corpus identically.
- `tests/spec_drift.rs` — `grammar_rules_are_covered_by_spec` checks that every
  named `grammar.js` rule appears verbatim in this chapter.
- `tests/vm_differential.rs` — the ternary/propagate disambiguation is pinned by
  the short-circuit / propagate / ternary suites
  (`vm_propagate_then_compare_then_ternary_matches_treewalker`,
  `vm_propagate_inside_ternary_branch_matches_treewalker`,
  `vm_ternary_keyword_then_branch_matches_treewalker`), four-mode byte-identical.

Run `cargo test --test treesitter_conformance` and
`cargo test --test frontend_conformance` (both green); the EBNF coverage is
checked by `cargo test --test spec_drift grammar_rules_are_covered_by_spec`.
