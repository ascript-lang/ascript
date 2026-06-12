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
  "defer"
  "class" "extends"
  "enum"
  "interface" "implements"
  "import" "export" "from" "as"
] @keyword

(worker_keyword) @keyword
(static_keyword) @keyword

; ----- Operators -------------------------------------------------------------
[
  "+" "-" "*" "/" "%" "**"
  "==" "!=" "<" "<=" ">" ">="
  "&&" "||" "!" "??"
  "&" "|" "^" "~" "<<" ">>"
  "+%" "-%" "*%"
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
; A user generic application head (`Box<int>`).
(generic_type name: (identifier) @type)
; TYPE §6: generic type parameters (`<T, U: Bound>`) and a `T` reference.
(type_parameter name: (identifier) @type.parameter)
(type_bound (identifier) @type)
; Capitalized identifiers in type position read as class/enum names.
((identifier) @type
  (#match? @type "^[A-Z]"))

; ----- Declarations & names --------------------------------------------------
(function_declaration name: (identifier) @function)
(method_definition name: (identifier) @function.method)
(class_declaration name: (identifier) @type)
(enum_declaration name: (identifier) @type)
(interface_declaration name: (identifier) @type)
(method_requirement name: (identifier) @function.method)
(enum_variant name: (identifier) @constant)
; ADT: payload field names + a named variant-pattern field.
(variant_field field: (identifier) @variable.member)
(variant_pattern_field field: (identifier) @variable.member)
; ADT: a named call argument `name: expr` in variant construction.
(named_argument name: (identifier) @variable.member)

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
