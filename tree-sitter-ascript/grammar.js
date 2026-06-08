/**
 * Tree-sitter grammar for AScript (.as)
 *
 * Source of truth for AScript *syntax*, reconciled with the implemented
 * interpreter (src/lexer.rs, src/parser.rs, src/ast.rs). It powers editor
 * highlighting, structural selection, and the LSP. It is intentionally
 * separate from the interpreter's recursive-descent parser, and is
 * conformance-tested against it over the example corpus (see
 * tests/treesitter_conformance.rs).
 *
 * Notes:
 *  - ASI-lite (§2) is approximated by making `;` an optional terminator; the
 *    interpreter treats newlines as soft separators and `;` is purely optional.
 *  - `self`, `super`, `extends`, `Ok`, `Err`, `recover`, and the primitive
 *    type names are plain identifiers / soft keywords in the interpreter, so
 *    they are NOT reserved here.
 */

// Precedence ladder, low (loosest) to high (tightest binding).
const PREC = {
  assign: 1,
  ternary: 2,  // cond ? then : else (right-associative)
  coalesce: 3,
  or: 4,
  and: 5,
  equality: 6,
  compare: 7,
  // Bitwise-OR tier (NUM §3.4, Go's binding): `| ^` bind TIGHTER than comparison
  // but LOOSER than `+ -`/range, so `a | b == c` is `(a|b)==c`.
  bitor: 8,
  range: 9,
  add: 10,   // `+ -` and wrapping `+% -%`
  mul: 11,   // `* / %`, wrapping `*%`, shifts `<< >>`, bitwise `&`
  exp: 12,   // right-associative
  // (13 intentionally free: postfix ?/! are precedence-less, GLR-resolved)
  unary: 14,
  postfix: 15, // call, member, index, optional-member
  primary: 16,
};

module.exports = grammar({
  name: 'ascript',

  word: $ => $.identifier,

  extras: $ => [
    /\s/,
    $.line_comment,
    $.block_comment,
  ],

  conflicts: $ => [
    // `(x)` could be a one-arg parameter list (arrow fn) or a parenthesized
    // identifier — needs a GLR split until the `=>` (or lack of it) is seen.
    [$.parameter, $._primary_expression],
    // At `<postfix-expr> ?` the parser cannot yet tell a propagation (`expr?`)
    // from a ternary condition (`expr ? then : else`): the postfix expression
    // could reduce to `_expression` (the ternary's condition) or extend into
    // `propagate_expression`. GLR keeps both alive until a following `:` (ternary)
    // or end-of-expression (propagation) decides.
    [$._expression, $.propagate_expression],
    // `<postfix-expr> !` has the same GLR ambiguity as propagation: the postfix
    // expression could reduce to `_expression` or extend into `unwrap_expression`.
    // Mirror propagate's precedence-less + declared-conflict treatment so the
    // ambiguity is resolved by GLR rather than a precedence the runtime may
    // interpret differently across tree-sitter versions.
    [$._expression, $.unwrap_expression],
    // ----- Match-arm pattern vs expression (Phase 8) ----------------------
    // A match-arm pattern shares its surface syntax with ordinary expressions,
    // so the parser cannot decide pattern-vs-expression until the arm's `=>`
    // (or a following `|`/`if`) is seen. These are settled by GLR, NOT by
    // precedence (a precedence here would distort the ternary `?`/`:`, the
    // `?`-propagation, and the range operator). Each entry keeps the two
    // interpretations alive until the lookahead disambiguates:
    //   - `[ ... ]` as an `array_pattern_match` vs an `array_literal` expr,
    [$.array_pattern_match, $.array_literal],
    //   - a value pattern (`_match_subject`) vs an `_expression` when both are
    //     reachable as an element of a `[…]` (array-pattern element vs
    //     array-literal element) or as an arm body operand.
    [$._expression, $._match_subject],
    //   - `...name` as a pattern `rest_element` vs a `spread_element` whose
    //     operand is an `identifier` (array/object-literal spread). Both share
    //     the `... identifier` prefix until the container is resolved as a
    //     pattern or a literal.
    [$.rest_element, $._primary_expression],
    // ----- Match-arm guard vs arrow (guard ends in a bare identifier) ------
    // A match guard is a full `_expression`, but a guard ENDING in a bare
    // identifier right before the arm's `=>` (e.g. `n if n == lim => "eq"`) is
    // ambiguous: the trailing `lim =>` could shift into a single-param
    // `arrow_function` (whose `=>` then steals the arm separator) OR `lim` could
    // reduce to an expression operand so the `=>` belongs to the `match_arm`.
    // GLR keeps both alive; only the latter yields a complete arm (`=> value`),
    // so it wins. Mirrors the hand-written parser, which suppresses a top-level
    // bare arrow inside the guard. The single-identifier `arrow_function` form is
    // precedence-LESS (see its rule) so this shift/reduce becomes a genuine
    // dynamic conflict: at `<ident> •  =>` reduce the identifier to a
    // `_primary_expression` (the `=>` is the arm separator) vs shift into a bare
    // `arrow_function` param.
    [$.arrow_function, $._primary_expression],
    // ----- ADT: variant pattern vs call expression -------------------------
    // A positional `Circle(r)` reduces to a `call_expression` (semantic recovery),
    // while a NAMED `Rect(w: ww)` reduces to a `variant_pattern`. Both share the
    // `<variant-ref> (` prefix until the first field's `key :` (named) or value
    // (positional) disambiguates; GLR keeps both alive.
    // A bare `Circle(` reduces to `_primary_expression` (the call callee, → a
    // positional pattern via semantic recovery) vs `variant_pattern` (named); a
    // qualified `Shape.Rect(` reduces to `_postfix_expression` (member-call callee)
    // vs `variant_pattern`. GLR keeps both alive until the first field disambiguates.
    [$.variant_pattern, $._primary_expression],
    [$.variant_pattern, $._postfix_expression],
  ],

  rules: {
    // ----- Program ---------------------------------------------------------
    source_file: $ => repeat($._item),

    _item: $ => choice(
      $.import_declaration,
      $.export_declaration,
      $._statement,
    ),

    // ----- Comments --------------------------------------------------------
    line_comment: _ => token(seq('//', /[^\n]*/)),
    block_comment: _ => token(seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/')),

    // ----- Modules (§9) ----------------------------------------------------
    import_declaration: $ => seq(
      'import',
      choice(
        seq('{', commaSep($.import_specifier), optional(','), '}'),
        seq('*', 'as', field('namespace', $.identifier)),
      ),
      'from',
      field('source', $.string),
      optional(';'),
    ),

    import_specifier: $ => $.identifier,

    export_declaration: $ => seq(
      'export',
      choice(
        $.let_declaration,
        $.const_declaration,
        $.function_declaration,
        $.class_declaration,
        $.enum_declaration,
      ),
    ),

    // ----- Statements (§3) -------------------------------------------------
    _statement: $ => choice(
      $.let_declaration,
      $.const_declaration,
      $.function_declaration,
      $.class_declaration,
      $.enum_declaration,
      $.if_statement,
      $.while_statement,
      $.for_statement,
      $.return_statement,
      $.break_statement,
      $.continue_statement,
      $.block,
      $.expression_statement,
    ),

    block: $ => seq('{', repeat($._statement), '}'),

    let_declaration: $ => seq(
      'let',
      field('name', $._binding_target),
      optional(seq(':', field('type', $._type))),
      optional(seq('=', field('value', $._expression))),
      optional(';'),
    ),

    const_declaration: $ => seq(
      'const',
      field('name', $._binding_target),
      optional(seq(':', field('type', $._type))),
      '=',
      field('value', $._expression),
      optional(';'),
    ),

    // `let [a, b] = ...` array destructuring (Result returns, §6) and
    // `let {a, b as local, "k" as v} = ...` object destructuring.
    _binding_target: $ => choice($.identifier, $.array_pattern, $.object_pattern),
    // `let [a, ...xs] = ...` — a trailing `...name` collects the remaining elements.
    array_pattern: $ => seq(
      '[',
      optional(choice(
        $.rest_element,
        seq(commaSep1($.identifier), optional(seq(',', $.rest_element)), optional(',')),
      )),
      ']',
    ),
    // `let {a, ...rest} = ...` — a trailing `...name` collects the leftover keys.
    object_pattern: $ => seq(
      '{',
      optional(choice(
        $.rest_element,
        seq(commaSep1($.object_pattern_entry), optional(seq(',', $.rest_element)), optional(',')),
      )),
      '}',
    ),
    object_pattern_entry: $ => seq(
      field('key', choice($.identifier, $.string)),
      optional(seq('as', field('binding', $.identifier))),
    ),
    // `...name` rest collector in array/object destructuring (must be last).
    rest_element: $ => seq('...', field('name', $.identifier)),

    function_declaration: $ => seq(
      optional($.worker_keyword),
      optional('async'),
      'fn',
      optional('*'),  // `fn*` / `async fn*` — a generator (§7, M17)
      field('name', $.identifier),
      field('parameters', $.parameter_list),
      optional(seq(':', field('return_type', $._type))),
      field('body', $.block),
    ),

    parameter_list: $ => seq(
      '(',
      optional(choice(
        $.rest_parameter,
        seq(commaSep1($.parameter), optional(seq(',', $.rest_parameter)), optional(',')),
      )),
      ')',
    ),
    parameter: $ => seq(
      field('name', $.identifier),
      optional(seq(':', field('type', $._type))),
      optional(seq('=', field('default', $._expression))),
    ),
    // `...name[: array<T>]` rest parameter — collects trailing args (must be last).
    rest_parameter: $ => seq(
      '...',
      field('name', $.identifier),
      optional(seq(':', field('type', $._type))),
    ),

    // ----- Classes & Enums (§8) -------------------------------------------
    class_declaration: $ => seq(
      optional($.worker_keyword),  // `worker class` — dedicated-isolate actor (Spec B)
      'class',
      field('name', $.identifier),
      optional(seq('extends', field('superclass', $.identifier))),
      field('body', $.class_body),
    ),
    // `;` is an optional run before the first member and after each member,
    // mirroring the hand-written parser's `skip_semicolons` in class bodies
    // (leading `;`, doubled `;;`, and only-`;` bodies are all accepted).
    class_body: $ => seq('{', repeat(';'), repeat(seq($.class_member, repeat(';'))), '}'),
    class_member: $ => choice($.field_declaration, $.method_definition),
    field_declaration: $ => seq(
      field('name', $.identifier),
      optional('?'),                    // `name?:` marker (lowers to T | nil)
      ':',
      field('type', $._type),           // also covers `name: T?`
      optional(seq('=', field('default', $._expression))),
    ),
    method_definition: $ => seq(
      optional($.static_keyword),  // `static fn` / `static async fn` / `static fn*` (SP1 §3)
      optional($.worker_keyword),
      optional('async'),
      'fn',
      optional('*'),  // `fn*` generator method (§7, M17)
      field('name', $.identifier),
      field('parameters', $.parameter_list),
      optional(seq(':', field('return_type', $._type))),
      field('body', $.block),
    ),

    enum_declaration: $ => seq(
      'enum',
      field('name', $.identifier),
      '{',
      commaSep($.enum_variant),
      optional(','),
      '}',
    ),
    enum_variant: $ => seq(
      field('name', $.identifier),
      // ADT: a `= scalar` backing OR a `(…)` payload, never both (the legacy/CST
      // parsers enforce the XOR; the grammar permits each independently).
      optional(choice(
        seq('=', field('value', $._expression)),
        seq('(', commaSep1($.variant_field), optional(','), ')'),
      )),
    ),
    // ADT: one declared payload field — named `id: T` or positional `T`.
    variant_field: $ => seq(
      optional(seq(field('field', $.identifier), ':')),
      field('type', $._type),
    ),

    // ----- Control flow ----------------------------------------------------
    if_statement: $ => prec.right(seq(
      'if', '(', field('condition', $._expression), ')',
      field('consequence', $.block),
      optional(seq('else', field('alternative', choice($.block, $.if_statement)))),
    )),

    while_statement: $ => seq(
      'while', '(', field('condition', $._expression), ')',
      field('body', $.block),
    ),

    // for (x of iterable), for (i in start..end), and for await (x in stream)
    for_statement: $ => seq(
      'for',
      optional('await'),  // `for await` — async iteration (§7, M17)
      '(',
      field('binding', $.identifier),
      field('kind', choice('of', 'in')),
      field('iterable', $._expression),
      ')',
      field('body', $.block),
    ),

    return_statement: $ => prec.right(seq(
      'return',
      optional($._expression),
      optional(';'),
    )),

    break_statement: $ => seq('break', optional(';')),
    continue_statement: $ => seq('continue', optional(';')),

    expression_statement: $ => seq($._expression, optional(';')),

    // ----- Expressions (§3 precedence) ------------------------------------
    _expression: $ => choice(
      $.assignment_expression,
      $.ternary_expression,
      $.binary_expression,
      $.range_expression,
      $.unary_expression,
      $.await_expression,
      $.yield_expression,
      $.match_expression,
      $.arrow_function,
      $._postfix_expression,
    ),

    assignment_expression: $ => prec.right(PREC.assign, seq(
      field('left', $._postfix_expression),
      field('operator', choice('=', '+=', '-=', '*=', '/=')),
      field('right', $._expression),
    )),

    binary_expression: $ => {
      const table = [
        ['??', PREC.coalesce],
        ['||', PREC.or],
        ['&&', PREC.and],
        ['==', PREC.equality], ['!=', PREC.equality],
        ['<', PREC.compare], ['<=', PREC.compare], ['>', PREC.compare], ['>=', PREC.compare],
        // `instanceof` is a reserved keyword at the comparison tier (SP2 §1). It is
        // automatically reserved against `identifier` via `word: $ => $.identifier`.
        ['instanceof', PREC.compare],
        // Bitwise-OR tier (NUM §3.4): `| ^` between compare and range. The `|`
        // here is bitwise-OR in EXPRESSION position; or-patterns (`or_pattern`)
        // and union types (`union_type`) are separate rules, so this never
        // collides with them.
        ['|', PREC.bitor], ['^', PREC.bitor],
        // NOTE: `..` / `..=` are NOT in this table — a range is its own
        // `range_expression` node (carries `inclusive` + an optional contextual
        // `step`), mirroring the hand-written parser's dedicated `ExprKind::Range`.
        // Additive: `+ -` and the wrapping `+% -%`.
        ['+', PREC.add], ['-', PREC.add], ['+%', PREC.add], ['-%', PREC.add],
        // Multiplicative: `* / %`, wrapping `*%`, shifts `<< >>`, bitwise `&`
        // (Go's binding — shifts/`&` at the multiplicative tier).
        ['*', PREC.mul], ['/', PREC.mul], ['%', PREC.mul], ['*%', PREC.mul],
        ['<<', PREC.mul], ['>>', PREC.mul], ['&', PREC.mul],
      ];
      const left = table.map(([op, p]) => prec.left(p, seq(
        field('left', $._expression),
        field('operator', op),
        field('right', $._expression),
      )));
      // ** is right-associative
      left.push(prec.right(PREC.exp, seq(
        field('left', $._expression),
        field('operator', '**'),
        field('right', $._expression),
      )));
      return choice(...left);
    },

    // `start .. end` / `start ..= end`, with an optional trailing contextual
    // `step <expr>`. A dedicated node (NOT a `binary_expression`) mirroring the
    // hand-written parser's `ExprKind::Range`, which carries `inclusive` and an
    // optional `step`. Left-associative at `PREC.range` so additive (`PREC.add`,
    // tighter) binds the bounds: `1+1..5*2 step k+1` is `(1+1)..(5*2) step (k+1)`.
    // `step` is NOT reserved (see `step_keyword`): it is recognized ONLY in this
    // immediate trailing position, so `let step = 1` / `fn step(n)` keep `step`
    // as an ordinary identifier. This rule is precedence-bounded but adds no new
    // reserved word; the `step`-vs-identifier choice is settled by GLR (declared
    // in `conflicts`).
    range_expression: $ => prec.left(PREC.range, seq(
      field('start', $._expression),
      field('operator', choice('..', '..=')),
      field('end', $._expression),
      optional(seq($.step_keyword, field('step', $._expression))),
    )),
    // The contextual `step` soft-keyword. Modeled as an aliased identifier-shaped
    // token (NOT a bare string literal) so tree-sitter's keyword extraction does
    // NOT promote `step` into the reserved word set — it stays a normal identifier
    // everywhere except immediately after a range's end bound.
    step_keyword: _ => 'step',
    // The contextual `static` soft-keyword (SP1 §3): the class-member modifier on
    // `fn` / `async fn` / `fn*`. `static` is NOT in the global reserved set, so it
    // stays an ordinary identifier in every NON-class-member position (`let static`,
    // `fn static`, `static(x)`, member access). The one tree-sitter-only limitation
    // (no external scanner): a class FIELD literally named `static` (`static: T`)
    // is not accepted by this grammar because at class-member start the lexer
    // commits to the `static` keyword token; the hand-written CST/legacy parsers
    // (which use a token of lookahead) do accept it. No corpus/example program
    // names a field `static`, so the three parsers agree on all real code.
    static_keyword: _ => 'static',
    // The contextual `worker` soft-keyword (Workers Spec A): the fn/method modifier
    // that dispatches the body to a pooled isolate and returns `future<T>`. `worker`
    // is NOT reserved — it is a plain identifier in every non-modifier position
    // (`let worker = 5`, `fn worker() {}`, `f(worker)`). Same contextual treatment as
    // `static_keyword`.
    worker_keyword: _ => 'worker',

    unary_expression: $ => prec.right(PREC.unary, seq(
      field('operator', choice('!', '-', '~')),
      field('operand', $._expression),
    )),

    await_expression: $ => prec.right(PREC.unary, seq('await', $._expression)),

    // `yield` / `yield <expr>` inside a generator body (`fn*`). Binds at the
    // assignment tier (lowest), like the hand-written parser. The operand is
    // optional (a bare `yield`).
    yield_expression: $ => prec.right(PREC.assign, seq(
      'yield',
      optional($._expression),
    )),

    // match subj { Pattern => expr, _ => expr, }  (§3, §8.2)
    // The subject is parsed at coalesce precedence so the trailing `{` opens
    // the arm block rather than being read as an object literal.
    match_expression: $ => prec(PREC.primary, seq(
      'match',
      field('subject', $._match_subject),
      '{',
      commaSep($.match_arm),
      optional(','),
      '}',
    )),
    // An arm: one or more `|`-separated patterns, an optional `if <cond>` guard
    // evaluated after a structural match, then `=> <expr>` (Phase 8). The guard
    // and body expressions are full `_expression`s.
    match_arm: $ => seq(
      field('pattern', $._match_pattern),
      optional(seq('if', field('guard', $._expression))),
      '=>',
      field('value', $._expression),
    ),
    _match_pattern: $ => choice(
      $.or_pattern,
      $._match_pattern_single,
    ),
    // `p0 | p1 | …` — alternatives; the arm fires when ANY matches.
    or_pattern: $ => prec.left(seq(
      $._match_pattern_single,
      repeat1(seq('|', $._match_pattern_single)),
    )),
    // A single (non-alternative) pattern. Order matters only for readability;
    // genuine ambiguities are resolved by the declared GLR `conflicts` above.
    _match_pattern_single: $ => choice(
      $.wildcard_pattern,
      $.array_pattern_match,
      $.object_pattern_match,
      $.variant_pattern, // ADT: NAMED variant destructure `Rect(w: ww)` (see note)
      $.identifier_pattern,
      $._match_subject, // literal / enum-variant / member / call / RANGE value pattern
    ),
    // ADT: a NAMED variant-destructuring pattern — `Rect(w: ww)` / `Circle(radius: 0.0)`
    // / `Shape.Rect(w: ww)`. POSITIONAL variant patterns (`Circle(r)`, `Pair(a, b)`)
    // are NOT a dedicated rule: they ride `call_expression` through the `_match_subject`
    // value-pattern branch (semantic recovery, exactly how the hand parser re-classifies
    // a parsed call into a variant pattern). Only the named form — whose `key: subpat`
    // entries are not valid call arguments — needs this node. A declared GLR conflict
    // with `call_expression` keeps the positional form a call and the named form a
    // variant_pattern.
    variant_pattern: $ => seq(
      field('variant', choice($.identifier, $.member_expression)),
      '(',
      commaSep1($.variant_pattern_field),
      optional(','),
      ')',
    ),
    variant_pattern_field: $ => seq(
      field('field', $.identifier),
      optional(seq(':', field('pattern', $._match_pattern_single))),
    ),
    wildcard_pattern: _ => '_',
    // A bare identifier pattern (Option C: compare-if-defined / bind-if-new,
    // resolved at match time by the interpreter). `name => body` in arm-pattern
    // position shares its surface with a single-param `arrow_function`; an arm
    // pattern must win that shift/reduce. The hand parser gets this for free by
    // parsing the pattern at coalesce precedence (no arrow). Here a precedence
    // ABOVE the arrow tier (`PREC.assign`) makes the `identifier → pattern`
    // reduction win over shifting into an arrow `=>`. This rule is reachable
    // ONLY in match-pattern position, so it does NOT affect arrow functions,
    // the ternary `?`/`:`, propagation, or ranges anywhere else.
    identifier_pattern: $ => prec(PREC.unary, $.identifier),
    // Range patterns `a..b` / `a..=b` (subject is a Number in the range) are NOT a
    // dedicated pattern rule: a `range_expression` is reachable through the
    // `_match_subject` value-pattern branch, exactly how the hand-written parser
    // recovers them — it parses a value expression and inspects it for an
    // `ExprKind::Range`. Both the exclusive (`..`) and inclusive (`..=`) forms,
    // plus an optional trailing `step`, flow through that single value path.
    // `[p0, p1, …, (...name | ...)?]` — array pattern with nested sub-patterns
    // and an optional trailing rest collector. Distinct from `array_pattern`
    // (destructuring), whose elements are plain identifiers; here each element
    // is itself a full `_match_pattern_single`.
    array_pattern_match: $ => seq(
      '[',
      optional(choice(
        $.rest_element,
        seq(commaSep1($._match_pattern_single), optional(seq(',', $.rest_element)), optional(',')),
      )),
      ']',
    ),
    // `{key, key2: subpat, …, (...name)?}` — object pattern. `{key}` is the
    // binding shorthand; `{key: subpat}` matches the field against a sub-pattern.
    object_pattern_match: $ => seq(
      '{',
      optional(choice(
        $.rest_element,
        seq(commaSep1($.object_pattern_match_entry), optional(seq(',', $.rest_element)), optional(',')),
      )),
      '}',
    ),
    object_pattern_match_entry: $ => seq(
      field('key', choice($.identifier, $.string)),
      optional(seq(':', field('pattern', $._match_pattern_single))),
    ),

    // The single-IDENTIFIER parameter form is precedence-LESS so the shift/reduce
    // at `<ident> =>` becomes a genuine GLR conflict (declared in `conflicts` as
    // `[$.match_arm, $.arrow_function]`): inside a match guard, `n if n == lim =>`
    // can then settle on the arm-completing parse (the `=>` is the arm separator)
    // rather than statically shifting `lim =>` into an arrow. The parenthesized
    // parameter form keeps `PREC.assign` (its `(` already disambiguates, and the
    // precedence preserves right-associativity of `(x) => (y) => z`).
    arrow_function: $ => choice(
      prec(PREC.assign, seq(
        optional('async'),
        field('parameters', $.parameter_list),
        '=>',
        field('body', choice($.block, $._expression)),
      )),
      seq(
        optional('async'),
        field('parameters', $.identifier),
        '=>',
        field('body', choice($.block, $._expression)),
      ),
    ),

    // Postfix chain: call, member, index, optional member, ? propagation.
    _postfix_expression: $ => choice(
      $.call_expression,
      $.member_expression,
      $.optional_member_expression,
      $.index_expression,
      $.unwrap_expression,
      $.propagate_expression,
      $._primary_expression,
    ),

    call_expression: $ => prec(PREC.postfix, seq(
      field('function', $._postfix_expression),
      field('arguments', $.arguments),
    )),
    arguments: $ => seq('(', commaSep(choice($.named_argument, $._expression, $.spread_element)), optional(','), ')'),

    // ADT §3.2: a named call argument `name: expr` (enum-variant construction —
    // `Shape.Rect(w: 3.0, h: 4.0)`). Only meaningful for a variant constructor;
    // the interpreter rejects a named arg on any other callee. The leading
    // `identifier ':'` is unambiguous at argument start (a bare `:` cannot
    // otherwise begin a call argument), so no GLR conflict is needed.
    named_argument: $ => seq(
      field('name', $.identifier),
      ':',
      field('value', $._expression),
    ),

    // ...expr — spread into an array literal, object literal, or call args
    // (typed-element AST in the interpreter; strict: spreading the wrong
    // container kind is a runtime panic).
    spread_element: $ => seq('...', $._expression),

    member_expression: $ => prec(PREC.postfix, seq(
      field('object', $._postfix_expression),
      '.',
      field('property', $.identifier),
    )),

    // obj?.field — safe access (§4)
    optional_member_expression: $ => prec(PREC.postfix, seq(
      field('object', $._postfix_expression),
      '?.',
      field('property', $.identifier),
    )),

    index_expression: $ => prec(PREC.postfix, seq(
      field('object', $._postfix_expression),
      '[', field('index', $._expression), ']',
    )),

    // expr?  — Result early-return propagation (§6). Intentionally left WITHOUT a
    // precedence: its `?` shares a prefix with the ternary `cond ? then : else`, so
    // the `shift ? (propagation)` vs `reduce postfix→expression (ternary condition)`
    // decision must stay an unresolved GLR conflict (declared above) rather than be
    // settled by precedence — only a following `:` (ternary) vs end-of-expression
    // (propagation) can decide it. The operand is already a `_postfix_expression`,
    // so propagation still binds tighter than any binary/ternary operator.
    propagate_expression: $ => seq(
      field('operand', $._postfix_expression),
      '?',
    ),
    // expr! — force-unwrap (dual of ?). Position-disambiguated from prefix `!`
    // (operand precedes it) and from `!=` (a single token).
    unwrap_expression: $ => seq(
      field('operand', $._postfix_expression),
      '!',
    ),

    // cond ? then : else — the conditional operator (§3). Right-associative,
    // binds just above assignment. Shares the `expr ?` prefix with
    // propagate_expression (resolved by the conflicts entry above).
    ternary_expression: $ => prec.right(PREC.ternary, seq(
      field('condition', $._expression),
      '?',
      field('consequence', $._expression),
      ':',
      field('alternative', $._expression),
    )),

    _primary_expression: $ => choice(
      $.identifier,
      $.number,
      $.string,
      $.template_string,
      $.boolean,
      $.nil,
      $.array_literal,
      $.object_literal,
      $.map_literal,
      $.parenthesized_expression,
    ),

    // Subject of a `match` / a match pattern: any expression EXCEPT an object
    // literal, so the `{` after the subject opens the match body. Mirrors the
    // interpreter, which parses the subject at coalesce precedence.
    _match_subject: $ => choice(
      $.binary_expression,
      $.range_expression,
      $.unary_expression,
      $._postfix_expression,
    ),

    parenthesized_expression: $ => seq('(', $._expression, ')'),

    array_literal: $ => seq('[', commaSep(choice($._expression, $.spread_element)), optional(','), ']'),

    object_literal: $ => prec(PREC.primary, seq(
      '{', commaSep(choice($.object_entry, $.spread_element)), optional(','), '}',
    )),
    object_entry: $ => seq(
      field('key', choice($.identifier, $.string)),
      ':',
      field('value', $._expression),
    ),

    // `#{ keyExpr: valueExpr, … }` map literal (SP2 §3). Unlike `object_entry`,
    // the key is an `_expression` — its VALUE is the map key. Spread inside `#{}`
    // is out of scope (D4): there is no spread alternative, so a `...` element is
    // a parse error.
    map_literal: $ => prec(PREC.primary, seq(
      '#{', commaSep($.map_entry), optional(','), '}',
    )),
    map_entry: $ => seq(
      field('key', $._expression),
      ':',
      field('value', $._expression),
    ),

    // ----- Types (§5) ------------------------------------------------------
    _type: $ => choice(
      $.union_type,
      $._type_atom,
    ),
    union_type: $ => prec.left(seq($._type_atom, repeat1(seq('|', $._type_atom)))),
    _type_atom: $ => choice(
      $.optional_type,
      $.primitive_type,
      $.array_type,
      $.map_type,
      $.result_type,
      $.future_type,
      $.tuple_type,
      $.identifier, // class / enum name
    ),
    primitive_type: _ => choice(
      'number', 'string', 'bool', 'nil', 'any', 'fn', 'object', 'error',
    ),
    array_type: $ => seq('array', '<', $._type, '>'),
    map_type: $ => seq('map', '<', $._type, ',', $._type, '>'),
    result_type: $ => seq('Result', '<', $._type, '>'),
    future_type: $ => seq('future', '<', $._type, '>'),
    tuple_type: $ => seq('[', commaSep1($._type), optional(','), ']'),
    // T? — nullable suffix (sugar for `T | nil`). Reachable only inside `_type`.
    // The inner `choice` is the non-recursive subset of `_type_atom` (avoids
    // left-recursion / `T??`); KEEP IN SYNC with `_type_atom` if a new type atom
    // is added there and should accept a `?` suffix.
    optional_type: $ => prec(PREC.postfix, seq(
      choice(
        $.primitive_type, $.array_type, $.map_type, $.result_type,
        $.future_type, $.tuple_type, $.identifier,
      ),
      '?',
    )),

    // ----- Literals (§2) ---------------------------------------------------
    identifier: _ => /[A-Za-z_][A-Za-z0-9_]*/,

    number: _ => token(choice(
      /0[xX][0-9a-fA-F_]+/,
      /0[bB][01_]+/,
      /0[oO][0-7_]+/,
      /(\d[\d_]*)?\.\d[\d_]*([eE][+-]?\d+)?/,
      /\d[\d_]*([eE][+-]?\d+)?/,
    )),

    string: _ => choice(
      seq('"', repeat(choice(/[^"\\]+/, /\\./)), '"'),
      seq("'", repeat(choice(/[^'\\]+/, /\\./)), "'"),
    ),

    template_string: $ => seq(
      '`',
      repeat(choice(
        $.template_chars,
        $.template_substitution,
      )),
      '`',
    ),
    template_chars: _ => token.immediate(prec(1, /[^`$\\]+/)),
    template_substitution: $ => seq('${', $._expression, '}'),

    boolean: _ => choice('true', 'false'),
    nil: _ => 'nil',
  },
});

function commaSep(rule) {
  return optional(commaSep1(rule));
}

function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)));
}
