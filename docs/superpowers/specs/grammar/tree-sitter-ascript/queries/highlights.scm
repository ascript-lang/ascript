; Tree-sitter syntax-highlighting queries for AScript.
; Capture names follow the standard tree-sitter highlight convention so they
; map onto editor themes (Neovim, Helix, Zed, the AScript LSP semantic tokens).

; ----- Comments --------------------------------------------------------------
(line_comment) @comment
(block_comment) @comment

; ----- Literals --------------------------------------------------------------
(number) @number
(string) @string
(template_string) @string
(template_substitution
  "${" @punctuation.special
  "}" @punctuation.special)
(boolean) @constant.builtin
(nil) @constant.builtin

; ----- Keywords --------------------------------------------------------------
[
  "let" "const" "fn" "return"
  "if" "else" "while" "for" "of" "in" "match"
  "async" "await"
  "class" "extends"
  "enum"
  "import" "export" "from" "as"
] @keyword

; ----- Operators -------------------------------------------------------------
[
  "+" "-" "*" "/" "%" "**"
  "==" "!=" "<" "<=" ">" ">="
  "&&" "||" "!" "??"
  "=" "+=" "-=" "*=" "/="
  "=>" ".." "?" "?."
] @operator

; ----- Punctuation -----------------------------------------------------------
[ "(" ")" "[" "]" "{" "}" ] @punctuation.bracket
[ "," "." ":" ";" ] @punctuation.delimiter

; ----- Types (§5) ------------------------------------------------------------
(primitive_type) @type.builtin
(array_type "array" @type.builtin)
(map_type "map" @type.builtin)
(result_type "Result" @type.builtin)
; Capitalized identifiers in type position read as class/enum names.
((identifier) @type
  (#match? @type "^[A-Z]"))

; ----- Declarations & names --------------------------------------------------
(function_declaration name: (identifier) @function)
(method_definition name: (identifier) @function.method)
(class_declaration name: (identifier) @type)
(enum_declaration name: (identifier) @type)
(enum_variant name: (identifier) @constant)

(parameter name: (identifier) @variable.parameter)

; ----- Calls & members -------------------------------------------------------
(call_expression
  function: (member_expression property: (identifier) @function.method))
(call_expression
  function: (identifier) @function)
(member_expression property: (identifier) @property)
(optional_member_expression property: (identifier) @property)
(object_entry key: (identifier) @property)

; `self` and `super` get builtin treatment wherever they appear.
((identifier) @variable.builtin
  (#any-of? @variable.builtin "self" "super"))

; Built-in globals (§11.1).
((identifier) @function.builtin
  (#any-of? @function.builtin
    "print" "len" "type" "assert" "range" "Ok" "Err" "recover"))

; ----- Fallback --------------------------------------------------------------
(identifier) @variable
