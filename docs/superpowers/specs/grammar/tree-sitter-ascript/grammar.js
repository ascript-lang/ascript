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
  range: 8,
  add: 9,
  mul: 10,
  exp: 11,    // right-associative
  unwrap: 12, // postfix ? and ! — looser than unary/await, tighter than binary
  unary: 13,
  postfix: 14, // call, member, index, optional-member
  primary: 15,
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

    // `let [a, b] = ...` array destructuring (Result returns, §6).
    _binding_target: $ => choice($.identifier, $.array_pattern),
    array_pattern: $ => seq('[', commaSep($.identifier), optional(','), ']'),

    function_declaration: $ => seq(
      optional('async'),
      'fn',
      optional('*'),  // `fn*` / `async fn*` — a generator (§7, M17)
      field('name', $.identifier),
      field('parameters', $.parameter_list),
      optional(seq(':', field('return_type', $._type))),
      field('body', $.block),
    ),

    parameter_list: $ => seq('(', commaSep($.parameter), optional(','), ')'),
    parameter: $ => seq(
      field('name', $.identifier),
      optional(seq(':', field('type', $._type))),
    ),

    // ----- Classes & Enums (§8) -------------------------------------------
    class_declaration: $ => seq(
      'class',
      field('name', $.identifier),
      optional(seq('extends', field('superclass', $.identifier))),
      field('body', $.class_body),
    ),
    class_body: $ => seq('{', repeat($.method_definition), '}'),
    method_definition: $ => seq(
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
      optional(seq('=', field('value', $._expression))),
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
        ['..', PREC.range],
        ['+', PREC.add], ['-', PREC.add],
        ['*', PREC.mul], ['/', PREC.mul], ['%', PREC.mul],
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

    unary_expression: $ => prec.right(PREC.unary, seq(
      field('operator', choice('!', '-')),
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
    match_arm: $ => seq(
      field('pattern', $._match_pattern),
      '=>',
      field('value', $._expression),
    ),
    _match_pattern: $ => choice(
      $.wildcard_pattern,
      $.or_pattern,
      $._match_subject, // literal / enum-variant / identifier pattern
    ),
    wildcard_pattern: _ => '_',
    or_pattern: $ => prec.left(seq(
      $._match_subject,
      repeat1(seq('|', $._match_subject)),
    )),

    arrow_function: $ => prec(PREC.assign, seq(
      optional('async'),
      field('parameters', choice($.parameter_list, $.identifier)),
      '=>',
      field('body', choice($.block, $._expression)),
    )),

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
    arguments: $ => seq('(', commaSep($._expression), optional(','), ')'),

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
    unwrap_expression: $ => prec(PREC.unwrap, seq(
      field('operand', $._postfix_expression),
      '!',
    )),

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
      $.parenthesized_expression,
    ),

    // Subject of a `match` / a match pattern: any expression EXCEPT an object
    // literal, so the `{` after the subject opens the match body. Mirrors the
    // interpreter, which parses the subject at coalesce precedence.
    _match_subject: $ => choice(
      $.binary_expression,
      $.unary_expression,
      $._postfix_expression,
    ),

    parenthesized_expression: $ => seq('(', $._expression, ')'),

    array_literal: $ => seq('[', commaSep($._expression), optional(','), ']'),

    object_literal: $ => prec(PREC.primary, seq(
      '{', commaSep($.object_entry), optional(','), '}',
    )),
    object_entry: $ => seq(
      field('key', choice($.identifier, $.string)),
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
