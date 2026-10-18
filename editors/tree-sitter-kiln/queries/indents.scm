; Indentation (nvim-treesitter conventions).
[
  (block)
  (declaration_list)
  (field_list)
  (ordered_field_list)
  (variant_list)
  (match_block)
  (field_initializer_list)
  (arguments)
  (parameters)
  (array_expression)
  (tuple_expression)
  (closure_parameters)
  (type_arguments)
  (type_parameters)
  (where_clause)
  (select_expression)
] @indent.begin

[
  "}"
  ")"
  "]"
] @indent.end @indent.branch

[
  (line_comment)
  (block_comment)
  (string_literal)
  (raw_string_literal)
] @indent.ignore
