/**
 * Tree-sitter grammar for Kiln (see docs/SPEC.md).
 *
 * Statements are terminated by `;` or a newline. Like tree-sitter-go, newline
 * is a terminator token in statement/list contexts and whitespace elsewhere.
 */

const PREC = {
  range: 1,
  or: 2,
  and: 3,
  compare: 4,
  bitor: 5,
  bitxor: 6,
  bitand: 7,
  shift: 8,
  additive: 9,
  multiplicative: 10,
  cast: 11,
  unary: 12,
  postfix: 13,
  call: 14,
  field: 15,
  path: 16,
};

const terminator = choice('\n', ';', '\0');

// `a sep a sep ...` with optional trailing separator.
const terminated = (rule) => seq(rule, repeat(seq(terminator, rule)), optional(terminator));

const itemPrefix = ($) => [repeat($.attribute), optional($.visibility_modifier)];

const sep1 = (rule, s) => seq(rule, repeat(seq(s, rule)));
const commaSep = (rule) => optional(seq(sep1(rule, ','), optional(',')));
// Brace lists: elements separated by `,`, `;`, or newlines.
const braceList = (rule) => seq('{', optional(seq(rule, repeat(seq(choice(',', terminator), rule)), optional(choice(',', terminator)))), '}');

module.exports = grammar({
  name: 'kiln',

  extras: ($) => [/\s/, $.line_comment, $.block_comment],

  word: ($) => $.identifier,

  conflicts: ($) => [
    [$.short_name, $._expression],
    [$._expression, $._type_identifier],
  ],

  supertypes: ($) => [$._expression, $._type, $._pattern, $._item],

  rules: {
    source_file: ($) => optional(terminated($._item)),

    // ----- comments --------------------------------------------------------

    line_comment: (_) => token(seq('//', /[^\n]*/)),
    block_comment: (_) => token(seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/')),

    // ----- items -----------------------------------------------------------

    _item: ($) =>
      choice(
        $.function_item,
        $.struct_item,
        $.enum_item,
        $.trait_item,
        $.impl_item,
        $.import_declaration,
        $.const_item,
        $.static_item,
        $.type_item,
        $.mod_item,
      ),

    attribute: ($) => seq('#', '[', $.identifier, optional($.token_tree), ']'),
    token_tree: ($) => seq('(', repeat(choice($.token_tree, /[^()]+/)), ')'),

    visibility_modifier: (_) => prec.right(seq('pub', optional(seq('(', 'pkg', ')')))),

    function_item: ($) =>
      prec.right(seq(
        ...itemPrefix($),
        optional('unsafe'),
        'fn',
        field('name', $.identifier),
        optional(field('type_parameters', $.type_parameters)),
        field('parameters', $.parameters),
        optional(seq('->', field('return_type', $._type))),
        optional($.where_clause),
        optional(field('body', $.block)),
      )),

    parameters: ($) =>
      seq('(', commaSep(seq(choice($.self_parameter, $.parameter))), ')'),
    self_parameter: ($) => seq(optional(seq('&', optional($.lifetime))), optional('mut'), 'self'),
    parameter: ($) => seq(field('pattern', $._pattern), ':', field('type', $._type)),

    type_parameters: ($) =>
      seq('<', commaSep(choice($.lifetime_parameter, $.type_parameter, $.const_parameter)), '>'),
    lifetime_parameter: ($) => seq($.lifetime, optional(seq(':', sep1($.lifetime, '+')))),
    type_parameter: ($) =>
      seq(field('name', $._type_identifier), optional(seq(':', $.trait_bounds)), optional(seq('=', $._type))),
    const_parameter: ($) => seq('const', $.identifier, ':', $._type),
    trait_bounds: ($) => prec.right(sep1(choice($.lifetime, seq(optional('?'), $._type_path)), '+')),
    where_clause: ($) => prec.right(seq('where', sep1($.where_predicate, ','), optional(','))),
    where_predicate: ($) => seq($._type, ':', $.trait_bounds),

    struct_item: ($) =>
      prec.right(seq(
        ...itemPrefix($),
        'struct',
        field('name', $._type_identifier),
        optional($.type_parameters),
        optional(choice(seq($.ordered_field_list, optional($.where_clause)), seq(optional($.where_clause), $.field_list))),
      )),
    field_list: ($) => braceList($.field_declaration),
    field_declaration: ($) => seq(optional($.visibility_modifier), field('name', $._field_identifier), ':', field('type', $._type)),
    ordered_field_list: ($) => seq('(', commaSep(seq(optional($.visibility_modifier), $._type)), ')'),

    enum_item: ($) =>
      prec.right(seq(...itemPrefix($), 'enum', field('name', $._type_identifier), optional($.type_parameters), optional($.where_clause), $.variant_list)),
    variant_list: ($) => braceList($.enum_variant),
    enum_variant: ($) =>
      seq(field('name', $.identifier), optional(choice($.field_list, $.ordered_field_list)), optional(seq('=', $._expression))),

    trait_item: ($) =>
      prec.right(seq(
        ...itemPrefix($),
        'trait',
        field('name', $._type_identifier),
        optional($.type_parameters),
        optional(seq(':', $.trait_bounds)),
        optional($.where_clause),
        $.declaration_list,
      )),

    impl_item: ($) =>
      prec.right(seq(
        ...itemPrefix($),
        'impl',
        optional($.type_parameters),
        optional(seq(field('trait', $._type_path), 'for')),
        field('type', $._type),
        optional($.where_clause),
        $.declaration_list,
      )),

    declaration_list: ($) => seq('{', optional(terminated($._item)), '}'),

    import_declaration: ($) => prec.right(seq(...itemPrefix($), 'import', $.import_tree)),
    import_tree: ($) =>
      prec.right(seq($._path_segment, repeat(seq('::', choice($._path_segment, '*', $.import_group))), optional(seq('as', $.identifier)))),
    import_group: ($) => seq('{', commaSep($.import_tree), '}'),

    const_item: ($) => prec.right(seq(...itemPrefix($), 'const', $.identifier, ':', $._type, optional(seq('=', $._expression)))),
    static_item: ($) => prec.right(seq(...itemPrefix($), 'static', optional('mut'), $.identifier, ':', $._type, '=', $._expression)),
    type_item: ($) => prec.right(seq(...itemPrefix($), 'type', $._type_identifier, optional($.type_parameters), optional(seq('=', $._type)))),
    mod_item: ($) => prec.right(seq(...itemPrefix($), 'mod', $.identifier, optional($.declaration_list))),

    // ----- types -----------------------------------------------------------

    _type: ($) =>
      choice(
        $._type_path,
        $.reference_type,
        $.pointer_type,
        $.array_type,
        $.slice_type,
        $.tuple_type,
        $.unit_type,
        $.function_type,
        $.dyn_type,
        $.impl_type,
        $.send_channel_type,
        $.receive_channel_type,
        $.never_type,
        '_',
      ),
    _type_path: ($) => choice($._type_identifier, $.primitive_type, $.scoped_type, $.generic_type, 'Self'),
    primitive_type: (_) =>
      choice('i8', 'i16', 'i32', 'i64', 'i128', 'isize', 'u8', 'u16', 'u32', 'u64', 'u128', 'usize', 'f32', 'f64', 'bool', 'char', 'str'),
    scoped_type: ($) => prec(PREC.path, seq(choice($.identifier, $.scoped_type, 'self', 'Self'), '::', $._type_identifier)),
    generic_type: ($) => prec(1, seq(choice($._type_identifier, $.scoped_type), $.type_arguments)),
    type_arguments: ($) => seq(token(prec(1, '<')), commaSep(choice($._type, $.lifetime, $.type_binding)), '>'),
    type_binding: ($) => seq($._type_identifier, '=', $._type),
    reference_type: ($) => seq('&', optional($.lifetime), optional('mut'), $._type),
    pointer_type: ($) => seq('*', choice('const', 'mut'), $._type),
    array_type: ($) => seq('[', $._type, ';', $._expression, ']'),
    slice_type: ($) => seq('[', $._type, ']'),
    tuple_type: ($) => seq('(', $._type, ',', commaSep($._type), ')'),
    unit_type: (_) => seq('(', ')'),
    function_type: ($) => prec.right(seq('fn', '(', commaSep($._type), ')', optional(seq('->', $._type)))),
    dyn_type: ($) => prec.right(seq('dyn', $.trait_bounds)),
    impl_type: ($) => prec.right(seq('impl', $.trait_bounds)),
    send_channel_type: ($) => seq('chan', '<-', $._type),
    receive_channel_type: ($) => seq('<-', 'chan', $._type),
    never_type: (_) => '!',

    // ----- statements ------------------------------------------------------

    block: ($) => seq('{', optional(terminated($._statement)), '}'),

    _statement: ($) =>
      choice(
        $.let_declaration,
        $.short_var_declaration,
        $.assignment_statement,
        $.send_statement,
        $.go_statement,
        $.defer_statement,
        $.expression_statement,
        $._item,
      ),

    let_declaration: ($) =>
      prec.right(seq('let', field('pattern', $._pattern), optional(seq(':', field('type', $._type))), optional(seq('=', field('value', $._expression))))),

    short_var_declaration: ($) =>
      prec.right(seq(field('left', choice($.short_name, seq('(', sep1($.short_name, ','), optional(','), ')'))), ':=', field('right', $._expression))),
    short_name: ($) => seq(choice($.identifier, '_'), optional($.mutable_specifier)),
    mutable_specifier: (_) => 'mut',

    assignment_statement: ($) =>
      prec(1, seq(field('left', $._expression), field('operator', choice('=', '+=', '-=', '*=', '/=', '%=', '^=', '&=', '|=', '<<=', '>>=')), field('right', $._expression))),

    send_statement: ($) => prec(1, seq(field('channel', $._expression), '<-', field('value', $._expression))),

    go_statement: ($) => prec.right(seq('go', choice(seq(optional('move'), $.block), $.call_expression))),

    defer_statement: ($) => prec.right(seq('defer', $._expression)),

    expression_statement: ($) => $._expression,

    // ----- expressions -----------------------------------------------------

    _expression: ($) =>
      choice(
        $._literal,
        $.identifier,
        $.self,
        $.scoped_identifier,
        $.generic_function,
        $.unary_expression,
        $.receive_expression,
        $.reference_expression,
        $.binary_expression,
        $.cast_expression,
        $.range_expression,
        $.call_expression,
        $.method_call_expression,
        $.field_expression,
        $.index_expression,
        $.try_expression,
        $.parenthesized_expression,
        $.tuple_expression,
        $.unit_expression,
        $.array_expression,
        $.struct_expression,
        $.closure_expression,
        $.block,
        $.unsafe_block,
        $.if_expression,
        $.match_expression,
        $.select_expression,
        $.for_expression,
        $.while_expression,
        $.loop_expression,
        $.return_expression,
        $.break_expression,
        $.continue_expression,
      ),

    self: (_) => 'self',

    scoped_identifier: ($) =>
      prec(PREC.path, seq(optional(choice($.identifier, $.scoped_identifier, $.generic_function, 'self', 'Self', 'chan')), '::', $.identifier)),
    generic_function: ($) =>
      prec(PREC.path, seq(choice($.identifier, $.scoped_identifier, 'chan'), '::', $.type_arguments)),

    unary_expression: ($) => prec(PREC.unary, seq(choice('-', '!', '*'), $._expression)),
    receive_expression: ($) => prec(PREC.unary, seq('<-', $._expression)),
    reference_expression: ($) => prec(PREC.unary, seq('&', optional('mut'), $._expression)),

    binary_expression: ($) => {
      const table = [
        [PREC.or, '||'],
        [PREC.and, '&&'],
        [PREC.compare, choice('==', '!=', '<', '<=', '>', '>=')],
        [PREC.bitor, '|'],
        [PREC.bitxor, '^'],
        [PREC.bitand, '&'],
        [PREC.shift, choice('<<', '>>')],
        [PREC.additive, choice('+', '-')],
        [PREC.multiplicative, choice('*', '/', '%')],
      ];
      return choice(
        ...table.map(([p, op]) =>
          prec.left(p, seq(field('left', $._expression), field('operator', op), field('right', $._expression))),
        ),
      );
    },

    cast_expression: ($) => prec.left(PREC.cast, seq($._expression, 'as', $._type)),

    range_expression: ($) =>
      prec.left(PREC.range, choice(seq($._expression, choice('..', '..='), $._expression), seq($._expression, '..'), seq('..', $._expression), '..')),

    call_expression: ($) => prec(PREC.call, seq(field('function', $._expression), field('arguments', $.arguments))),
    arguments: ($) => seq('(', commaSep($._expression), ')'),

    method_call_expression: ($) =>
      prec(PREC.field + 1, seq(field('receiver', $._expression), '.', field('method', $._field_identifier), optional(seq('::', $.type_arguments)), field('arguments', $.arguments))),
    field_expression: ($) =>
      prec(PREC.field, seq(field('value', $._expression), '.', field('field', choice($._field_identifier, $.integer_literal)))),
    index_expression: ($) => prec(PREC.postfix, seq($._expression, '[', $._expression, ']')),
    try_expression: ($) => prec(PREC.postfix, seq($._expression, '?')),

    parenthesized_expression: ($) => seq('(', $._expression, ')'),
    tuple_expression: ($) => seq('(', $._expression, ',', commaSep($._expression), ')'),
    unit_expression: (_) => seq('(', ')'),
    array_expression: ($) =>
      seq('[', choice(seq($._expression, ';', $._expression), commaSep($._expression)), ']'),

    struct_expression: ($) => prec.dynamic(-1, prec(PREC.call, seq(field('name', choice($._type_identifier, $.scoped_type_identifier)), $.field_initializer_list))),
    scoped_type_identifier: ($) => prec(PREC.path, seq(choice($.identifier, $.scoped_identifier, 'Self'), '::', $._type_identifier)),
    field_initializer_list: ($) => braceList(choice($.shorthand_field_initializer, $.field_initializer, $.base_field_initializer)),
    shorthand_field_initializer: ($) => $.identifier,
    field_initializer: ($) => seq(field('name', $._field_identifier), ':', field('value', $._expression)),
    base_field_initializer: ($) => seq('..', $._expression),

    closure_expression: ($) =>
      prec.right(seq(optional('move'), $.closure_parameters, choice(seq('->', $._type, $.block), $._expression))),
    closure_parameters: ($) => choice('||', seq('|', commaSep(seq(choice($.identifier, $.mut_pattern, $.tuple_pattern, '_'), optional(seq(':', $._type)))), '|')),

    unsafe_block: ($) => seq('unsafe', $.block),

    _condition: ($) => choice($._expression, $.let_condition),
    let_condition: ($) => seq('let', $._pattern, '=', $._expression),

    if_expression: ($) =>
      prec.right(seq('if', field('condition', $._condition), field('consequence', $.block), optional(seq('else', field('alternative', choice($.block, $.if_expression)))))),

    match_expression: ($) => seq('match', field('value', $._expression), $.match_block),
    match_block: ($) => braceList($.match_arm),
    match_arm: ($) => seq(field('pattern', $._pattern), optional(seq('if', $._expression)), '=>', field('value', $._expression)),

    select_expression: ($) => seq('select', braceList($.select_arm)),
    select_arm: ($) =>
      seq(
        choice(
          seq(optional(seq($.short_name, ':=')), '<-', $._expression),
          seq($._expression, '<-', $._expression),
          '_',
        ),
        '=>',
        $._expression,
      ),

    label: ($) => seq($.lifetime, ':'),
    for_expression: ($) => seq(optional($.label), 'for', field('pattern', $._pattern), 'in', field('value', $._expression), field('body', $.block)),
    while_expression: ($) => seq(optional($.label), 'while', field('condition', $._condition), field('body', $.block)),
    loop_expression: ($) => seq(optional($.label), 'loop', field('body', $.block)),

    return_expression: ($) => prec.right(seq('return', optional($._expression))),
    break_expression: ($) => prec.right(seq('break', optional($.lifetime), optional($._expression))),
    continue_expression: ($) => prec.right(seq('continue', optional($.lifetime))),

    // ----- patterns --------------------------------------------------------

    _pattern: ($) =>
      choice(
        $._literal_pattern,
        $.identifier,
        $.scoped_identifier,
        $.tuple_pattern,
        $.tuple_struct_pattern,
        $.struct_pattern,
        $.slice_pattern,
        $.ref_pattern,
        $.mut_pattern,
        $.range_pattern,
        $.or_pattern,
        $.captured_pattern,
        $.remaining_field_pattern,
        '_',
      ),
    _literal_pattern: ($) => choice($._literal, $.negative_literal),
    negative_literal: ($) => seq('-', choice($.integer_literal, $.float_literal)),
    tuple_pattern: ($) => seq('(', commaSep($._pattern), ')'),
    tuple_struct_pattern: ($) => seq(choice($.identifier, $.scoped_identifier), '(', commaSep($._pattern), ')'),
    struct_pattern: ($) => seq(choice($._type_identifier, $.scoped_type_identifier), braceList(choice($.field_pattern, $.remaining_field_pattern))),
    field_pattern: ($) => choice(seq(optional('mut'), $._field_identifier), seq($._field_identifier, ':', $._pattern)),
    remaining_field_pattern: (_) => '..',
    slice_pattern: ($) => seq('[', commaSep($._pattern), ']'),
    ref_pattern: ($) => prec(1, seq('&', $._pattern)),
    mut_pattern: ($) => seq('mut', $.identifier),
    range_pattern: ($) => seq($._literal_pattern, choice('..', '..='), $._literal_pattern),
    or_pattern: ($) => prec.left(seq($._pattern, '|', $._pattern)),
    captured_pattern: ($) => prec(1, seq($.identifier, '@', $._pattern)),

    // ----- literals and names ----------------------------------------------

    _literal: ($) => choice($.string_literal, $.raw_string_literal, $.byte_string_literal, $.byte_literal, $.char_literal, $.boolean_literal, $.integer_literal, $.float_literal),

    integer_literal: (_) =>
      token(seq(choice(/[0-9][0-9_]*/, /0x[0-9a-fA-F_]+/, /0b[01_]+/, /0o[0-7_]+/), optional(choice('i8', 'i16', 'i32', 'i64', 'i128', 'isize', 'u8', 'u16', 'u32', 'u64', 'u128', 'usize')))),
    float_literal: (_) =>
      token(choice(
        seq(/[0-9][0-9_]*\.[0-9][0-9_]*/, optional(/[eE][+-]?[0-9_]+/), optional(choice('f32', 'f64'))),
        seq(/[0-9][0-9_]*/, /[eE][+-]?[0-9_]+/, optional(choice('f32', 'f64'))),
        seq(/[0-9][0-9_]*/, choice('f32', 'f64')),
      )),
    boolean_literal: (_) => choice('true', 'false'),
    char_literal: (_) => token(seq("'", choice(/[^'\\]/, /\\[nrt0\\"'{}]/, /\\x[0-7][0-9a-fA-F]/, /\\u\{[0-9a-fA-F_]+\}/), "'")),
    byte_string_literal: (_) =>
      token(choice(seq('b"', repeat(choice(/[^"\\]/, /\\[nrt0\\"']/, /\\x[0-9a-fA-F]{2}/)), '"'), seq('br', choice(seq('"', /[^"]*/, '"'), seq('#"', /([^"]|"[^#])*/, '"#'))))),
    byte_literal: (_) => token(seq("b'", choice(/[^'\\]/, /\\[nrt0\\"']/, /\\x[0-9a-fA-F]{2}/), "'")),
    raw_string_literal: (_) => token(seq('r', choice(seq('"', /[^"]*/, '"'), seq('#"', /([^"]|"[^#])*/, '"#')))),

    string_literal: ($) => seq('"', repeat(choice($.string_content, $.escape_sequence, $.interpolation)), token.immediate('"')),
    string_content: (_) => token.immediate(prec(1, /[^"\\{}]+/)),
    escape_sequence: (_) => token.immediate(choice(/\\[nrt0\\"'{}]/, /\\x[0-7][0-9a-fA-F]/, /\\u\{[0-9a-fA-F_]+\}/, '{{', '}}')),
    interpolation: ($) => seq(token.immediate('{'), $._expression, '}'),

    lifetime: ($) => seq("'", token.immediate(/[A-Za-z_][A-Za-z0-9_]*/)),

    _path_segment: ($) => choice($.identifier, 'self', 'Self'),
    identifier: (_) => /[\p{L}_][\p{L}\p{Nd}_]*/u,
    _type_identifier: ($) => alias($.identifier, $.type_identifier),
    _field_identifier: ($) => alias($.identifier, $.field_identifier),
  },
});
