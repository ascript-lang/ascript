; ----- Definitions -----------------------------------------------------------
(function_declaration
  name: (identifier) @name) @definition.function

(method_definition
  name: (identifier) @name) @definition.method

(class_declaration
  name: (identifier) @name) @definition.class
; Note: `worker class C {}` is intentionally a class for symbol tagging — the
; optional leading `worker_keyword` modifier does not affect this structural match.

(enum_declaration
  name: (identifier) @name) @definition.enum

(enum_variant
  name: (identifier) @name) @definition.constant

; ----- References ------------------------------------------------------------
; Direct call:  foo(...)
(call_expression
  function: (identifier) @name) @reference.call

; Method / namespaced call:  obj.foo(...)  /  math.abs(...)
(call_expression
  function: (member_expression
    property: (identifier) @name)) @reference.call

; Class instantiation / type mention:  C(...)  (Capitalized callee)
(call_expression
  function: (identifier) @name (#match? @name "^[A-Z]")) @reference.class
