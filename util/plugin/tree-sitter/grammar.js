// Keep these rules aligned with crates/mettle-syntax/src/{lib,parser}.rs.
const PREC = { or: 1, and: 2, compare: 3, unary: 4, member: 5, call: 6 };
const commaSep = rule => optional(seq(rule, repeat(seq(",", rule))));
const commaSepTrailing = rule => optional(seq(rule, repeat(seq(",", rule)), optional(",")));
const block = ($, rule, commas = false) => seq(
  "{",
  optional(seq(rule, repeat(seq(commas ? choice(",", $._newline) : $._newline, rule)),
    optional(commas ? choice(",", $._newline) : $._newline))),
  "}",
);

module.exports = grammar({
  name: "mettle",
  extras: $ => [/[ \t\r\n]/, $.comment],
  externals: $ => [
    $._newline, $._call_continuation, $._member_continuation, $._index_continuation,
    $._or_continuation, $._and_continuation,
    $._comparison_continuation, $._call_options_start, $._else_start, $._error_sentinel,
  ],
  word: $ => $.identifier,
  conflicts: $ => [[$._positional_arguments], [$._named_arguments], [$.call_expression]],
  reserved: {
    global: _ => [
      "flow", "test", "context", "namespace", "defaults", "use", "return", "assert",
      "if", "else", "and", "or", "not", "within", "retry", "parallel", "rate",
      "concurrency", "true", "false", "null", "for", "in", "fail",
    ],
  },

  rules: {
    source_file: $ => repeat(choice(
      $.namespace_declaration, $.namespace_use, $.context_declaration,
      $.file_context_use, $.flow_declaration, $.anonymous_flow,
      $.test_declaration, $.call_expression,
    )),
    namespace_declaration: $ => seq("namespace", field("name", $.identifier)),
    namespace_use: $ => seq("use", "namespace", field("name", $.identifier)),
    context_declaration: $ => seq("context", field("name", $.identifier), $.context_body),
    file_context_use: $ => prec.right(seq("use", "context", choice(
      seq(field("name", $.identifier), optional($.context_body)), $.context_body,
    ))),
    context_body: $ => block($, choice($.context_use, $.defaults_declaration, $.object_field), true),
    context_use: $ => seq("use", "context", field("name", $.identifier)),
    defaults_declaration: $ => seq("defaults", field("capability", $.identifier), $.object),
    flow_declaration: $ => seq("flow", field("name", $.identifier), optional($.parameter_list),
      choice(seq("=", field("body", $._expression)), field("body", $.block))),
    anonymous_flow: $ => seq("flow", field("body", $.block)),
    parameter_list: $ => seq("(", commaSep($.identifier), ")"),
    test_declaration: $ => seq("test", choice(field("name", $.string),
      seq("(", field("name", $.string), ")")), field("body", $.block)),
    block: $ => block($, $._statement),
    _statement: $ => choice(
      $.context_use, $.binding, $.return_statement, $.assert_statement,
      $.if_statement, $.expression_statement,
    ),
    binding: $ => seq(field("name", $.identifier), "=", field("value", $._expression)),
    return_statement: $ => seq("return", $._expression),
    assert_statement: $ => seq("assert", "(", $._expression, optional(seq(",", $.string)), ")"),
    if_statement: $ => prec.right(seq("if", "(", field("condition", $._expression), ")",
      field("consequence", $.block), optional(seq($._else_start, "else", field("alternative", choice($.block, $.if_statement)))))),
    expression_statement: $ => $._expression,

    _expression: $ => choice($._primary_expression, $.binary_expression, $.unary_expression),
    binary_expression: $ => choice(...[
      ["or", PREC.or], ["and", PREC.and],
      ["==", PREC.compare], ["!=", PREC.compare], ["<", PREC.compare],
      ["<=", PREC.compare], [">", PREC.compare], [">=", PREC.compare],
    ].map(([operator, precedence]) => prec.left(precedence, seq(
      field("left", precedence === PREC.compare ? $._comparison_operand : $._expression),
      operator === "or" ? $._or_continuation : operator === "and" ? $._and_continuation : $._comparison_continuation,
      field("operator", operator),
      field("right", precedence === PREC.compare ? $._comparison_operand : $._expression),
    )))),
    _comparison_operand: $ => choice($._primary_expression, $.unary_expression),
    unary_expression: $ => prec(PREC.unary, seq(choice("not", "-"), $._comparison_operand)),
    _primary_expression: $ => choice(
      $.identifier, $.string, $.integer, $.float, $.duration, $.boolean, $.null,
      $.array, $.object, $.parenthesized_expression, $.call_expression,
      $.member_expression, $.index_expression,
      $.within_expression, $.retry_expression, $.parallel_expression,
      $.rate_expression, $.concurrency_expression, $.for_expression, $.fail_expression,
    ),
    parenthesized_expression: $ => seq("(", $._expression, ")"),
    call_expression: $ => prec(PREC.call, seq(
      field("function", choice($.identifier, $.member_expression)),
      $._call_continuation, $.argument_list, optional(seq($._call_options_start, field("options", $.object))),
    )),
    argument_list: $ => seq("(", optional(choice(
      seq($._positional_arguments, optional(seq(",", $._named_arguments)), optional(",")),
      seq($._named_arguments, optional(",")),
    )), ")"),
    _positional_arguments: $ => seq($._expression, repeat(seq(",", $._expression))),
    _named_arguments: $ => seq($.named_argument, repeat(seq(",", $.named_argument))),
    named_argument: $ => seq(field("name", $.identifier), ":", field("value", $._expression)),
    member_expression: $ => prec.left(PREC.member, seq(
      field("object", $._primary_expression), $._member_continuation, ".", field("property", $.identifier),
    )),
    index_expression: $ => prec.left(PREC.member, seq(
      field("object", $._primary_expression), $._index_continuation, "[", field("index", $._expression), "]",
    )),
    array: $ => seq("[", commaSepTrailing($._expression), "]"),
    object: $ => block($, $.object_field, true),
    object_field: $ => seq(field("key", choice($.identifier, $.string)), ":", field("value", $._expression)),

    within_expression: $ => seq("within", "(", commaSepTrailing($.timeout_option), ")", $.expression_body),
    retry_expression: $ => seq("retry", "(", commaSepTrailing(choice($.attempts_option, $.delay_option)), ")", $.expression_body),
    parallel_expression: $ => seq("parallel", optional(seq("(", commaSepTrailing($.limit_option), ")")), $.parallel_body),
    rate_expression: $ => seq("rate", "(", commaSepTrailing(choice($.target_option, $.period_option, $.duration_option, $.limit_option)), ")", $.expression_body),
    concurrency_expression: $ => seq("concurrency", "(", commaSepTrailing(choice($.limit_option, $.duration_option)), ")", $.expression_body),
    timeout_option: $ => seq(field("name", alias("timeout", $.identifier)), ":", $._expression),
    attempts_option: $ => seq(field("name", alias("attempts", $.identifier)), ":", $._expression),
    delay_option: $ => seq(field("name", alias("delay", $.identifier)), ":", $._expression),
    limit_option: $ => seq(field("name", alias("limit", $.identifier)), ":", $._expression),
    target_option: $ => seq(field("name", alias("target", $.identifier)), ":", $._expression),
    period_option: $ => seq(field("name", alias("period", $.identifier)), ":", $._expression),
    duration_option: $ => seq(field("name", alias("duration", $.identifier)), ":", $._expression),
    expression_body: $ => block($, $._statement),
    parallel_body: $ => block($, choice($.named_branch, $._expression), true),
    named_branch: $ => seq(field("name", $.identifier), ":", field("value", $._expression)),
    fail_expression: $ => seq("fail", "(", $._expression, ")"),
    for_expression: $ => seq("for", optional(seq(field("key", $.identifier), ",")),
      field("value", $.identifier), "in", field("iterable", $._expression),
      optional($._call_options_start), field("body", $.block)),

    identifier: _ => /[A-Za-z_][A-Za-z0-9_]*/,
    integer: _ => /-?(0[xX][0-9a-fA-F](_?[0-9a-fA-F])*|0[bB][01](_?[01])*|[0-9](_?[0-9])*)/,
    float: _ => /-?[0-9](_?[0-9])*(\.[0-9](_?[0-9])*([eE][+-]?[0-9](_?[0-9])*)?|[eE][+-]?[0-9](_?[0-9])*)/,
    duration: _ => /[0-9](_?[0-9])*(\.[0-9](_?[0-9])*)?(ns|us|ms|s|m|h)/,
    boolean: _ => choice("true", "false"),
    null: _ => "null",
    string: $ => seq('"', repeat(choice($.string_content, $.escape_sequence, $.interpolation)), token.immediate('"')),
    string_content: _ => token.immediate(/[^"\\\r\n$]+|\$/),
    escape_sequence: _ => token.immediate(/\\["\\nrt]/),
    interpolation: $ => seq(token.immediate("${"), $.interpolation_path, token.immediate("}")),
    interpolation_path: _ => token.immediate(/[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)*/),
    comment: _ => token(seq("//", /[^\n]*/)),
  },
});
