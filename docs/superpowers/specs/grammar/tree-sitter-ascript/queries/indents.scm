; Nodes whose body should be indented one level.
[
  (block)
  (class_body)
  (object_literal)
  (map_literal)
  (array_literal)
  (parameter_list)
  (arguments)
  (match_expression)
  (enum_declaration)
] @indent.begin

; The closing bracket of each of those dedents back.
[
  "}"
  "]"
  ")"
] @indent.end

; `else` continues the if-construct at the same level (branch, not a new indent).
(if_statement
  "else" @indent.branch)

; Keep a line that is only a closing bracket aligned with its opener.
[
  "}"
  "]"
  ")"
] @indent.align
