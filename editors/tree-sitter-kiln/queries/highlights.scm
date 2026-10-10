; Tree-sitter highlight queries for Kiln.

(line_comment) @comment
(block_comment) @comment
((line_comment) @comment.documentation
  (#match? @comment.documentation "^///[^/]"))

; Literals
(string_literal) @string
(raw_string_literal) @string
(char_literal) @character
(escape_sequence) @string.escape
(integer_literal) @number
(float_literal) @number.float
(boolean_literal) @boolean
(interpolation
  "{" @punctuation.special
  "}" @punctuation.special) @embedded

; Types
(type_identifier) @type
(primitive_type) @type.builtin
(lifetime) @label
"Self" @type.builtin

((identifier) @type
  (#match? @type "^[A-Z]"))
((identifier) @constant
  (#match? @constant "^[A-Z][A-Z0-9_]+$"))
((identifier) @constant.builtin
  (#any-of? @constant.builtin "Some" "None" "Ok" "Err"))

; Definitions
(function_item name: (identifier) @function)
(struct_item name: (type_identifier) @type.definition)
(enum_item name: (type_identifier) @type.definition)
(trait_item name: (type_identifier) @type.definition)
(enum_variant name: (identifier) @constructor)
(field_declaration name: (field_identifier) @property)
(parameter pattern: (identifier) @variable.parameter)
(self_parameter) @variable.builtin
(self) @variable.builtin

; Calls and fields
(call_expression function: (identifier) @function.call)
(call_expression function: (scoped_identifier (identifier) @function.call .))
(method_call_expression method: (field_identifier) @function.method.call)
(field_expression field: (field_identifier) @property)
(field_initializer name: (field_identifier) @property)
(attribute (identifier) @attribute)

(scoped_identifier (identifier) @module . "::")

; Keywords
[
  "fn" "struct" "enum" "trait" "impl" "type" "mod" "where" "as" "dyn" "chan"
] @keyword
[ "import" ] @keyword.import
[ "pub" "mut" "const" "static" "let" "unsafe" "move" ] @keyword.modifier
[ "if" "else" "match" "select" ] @keyword.conditional
[ "for" "in" "while" "loop" "break" "continue" ] @keyword.repeat
[ "return" ] @keyword.return
[ "go" "defer" ] @keyword.coroutine
(mutable_specifier) @keyword.modifier

; Operators
[
  "<-" ":=" "=" "+=" "-=" "*=" "/=" "%=" "^=" "&=" "|=" "<<=" ">>="
  "+" "-" "*" "/" "%" "^" "!" "&" "|" "&&" "||" "<<" ">>"
  "==" "!=" "<" "<=" ">" ">=" ".." "..=" "?" "->" "=>"
] @operator

; Punctuation
[ "(" ")" "[" "]" "{" "}" ] @punctuation.bracket
[ "," ";" "." "::" ":" ] @punctuation.delimiter
"#" @punctuation.special
