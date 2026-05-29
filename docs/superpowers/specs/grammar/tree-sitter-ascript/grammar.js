/**
 * Tree-sitter grammar for AScript (.as)
 *
 * Source of truth for AScript *syntax*. Mirrors the grammar sketch in
 * docs/superpowers/specs/2026-05-29-ascript-design.md (§3) and the lexical
 * rules in §2. This grammar powers editor syntax highlighting, structural
 * selection, and the LSP — it is error-tolerant and incremental, and is
 * intentionally separate from the interpreter's recursive-descent parser.
 *
 * Notes / known follow-ups:
 *  - Automatic semicolon insertion (ASI-lite, §2) is approximated here by
 *    making the statement terminator optional. Faithful newline-sensitive ASI
 *    needs an external scanner (scanner.c) — tracked as a follow-up.
 *  - A handful of conflicts (arrow-fn vs parenthesized expr, object literal vs
 *    block) are resolved with `conflicts`/precedence and may need tuning when
 *    the grammar is generated.
 */

// Precedence ladder, low (loosest) to high (tightest binding).
const PREC = {
  assign: 1,
  coalesce: 2,
  or: 3,
  and: 4,
  equality: 5,
  compare: 6,
  range: 7,
  add: 8,
  mul: 9,
  exp: 10,    // right-associative
  unary: 11,
  postfix: 12, // call, member, index, optional-member, ? propagation
  primary: 13,
};

module.exports = grammar({
  name: 'ascript',

  word: $ => $.identifier,

  extras: $ => [
    /\s/,
    $.line_comment,
    $.block_comment,
  ],

  // Optional statement terminators + a couple of intentional ambiguities.
  conflicts: $ => [
    [$.object_literal, $.block],
    [$.arrow_function, $.parenthesized_expression],
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
        seq('{', commaSep($.import_specifier), '}'),
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
      $.expression_statement,
      $.block,
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
    array_pattern: $ => seq('[', commaSep($.identifier), ']'),

    function_declaration: $ => seq(
      optional('async'),
      'fn',
      field('name', $.identifier),
      field('parameters', $.parameter_list),
      optional(seq(':', field('return_type', $._type))),
      field('body', $.block),
    ),

    parameter_list: $ => seq('(', commaSep($.parameter), ')'),
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
      optional(seq('=', field('value', choice($.number, $.string)))),
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

    // for (x of iterable)  and  for (i in 0..n)
    for_statement: $ => seq(
      'for', '(',
      field('binding', $.identifier),
      field('kind', choice('of', 'in')),
      field('iterable', $._expression),
      ')',
      field('body', $.block),
    ),

    return_statement: $ => seq('return', optional($._expression), optional(';')),

    expression_statement: $ => seq($._expression, optional(';')),

    // ----- Expressions (§3 precedence) ------------------------------------
    _expression: $ => choice(
      $.assignment_expression,
      $.binary_expression,
      $.unary_expression,
      $.await_expression,
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

    // match c { Pattern => expr, _ => expr, }  (§3, §8.2)
    match_expression: $ => seq(
      'match',
      field('subject', $._expression),
      '{',
      commaSep($.match_arm),
      optional(','),
      '}',
    ),
    match_arm: $ => seq(
      field('pattern', $._match_pattern),
      '=>',
      field('value', $._expression),
    ),
    _match_pattern: $ => choice(
      $.wildcard_pattern,
      $.or_pattern,
      $._expression, // literal / enum-variant / identifier pattern
    ),
    wildcard_pattern: _ => '_',
    or_pattern: $ => prec.left(seq($._expression, repeat1(seq('|', $._expression)))),

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
      $.propagate_expression,
      $._primary_expression,
    ),

    call_expression: $ => prec(PREC.postfix, seq(
      field('function', $._postfix_expression),
      field('arguments', $.argument_list),
    )),
    argument_list: $ => seq('(', commaSep($._expression), ')'),

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

    // expr?  — Result early-return propagation (§6)
    propagate_expression: $ => prec(PREC.postfix, seq(
      field('operand', $._postfix_expression),
      '?',
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

    parenthesized_expression: $ => seq('(', $._expression, ')'),

    array_literal: $ => seq('[', commaSep($._expression), optional(','), ']'),

    object_literal: $ => seq('{', commaSep($.object_entry), optional(','), '}'),
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
      $.primitive_type,
      $.array_type,
      $.map_type,
      $.result_type,
      $.tuple_type,
      $.identifier, // class / enum name
    ),
    primitive_type: _ => choice(
      'number', 'string', 'bool', 'nil', 'any', 'fn', 'object', 'error',
    ),
    array_type: $ => seq('array', '<', $._type, '>'),
    map_type: $ => seq('map', '<', $._type, ',', $._type, '>'),
    result_type: $ => seq('Result', '<', $._type, '>'),
    tuple_type: $ => seq('[', commaSep1($._type), ']'),

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
