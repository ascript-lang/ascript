; ----- Scopes ----------------------------------------------------------------
[
  (function_declaration)
  (method_definition)
  (arrow_function)
  (block)
  (for_statement)
  (while_statement)
  (if_statement)
  (match_arm)
] @local.scope

; ----- Definitions -----------------------------------------------------------
; let / const bindings (simple identifier target).
(let_declaration name: (identifier) @local.definition.var)
(const_declaration name: (identifier) @local.definition.var)

; Function / method / class / enum names.
(function_declaration name: (identifier) @local.definition.function)
(method_definition name: (identifier) @local.definition.method)
(class_declaration name: (identifier) @local.definition.type)
(enum_declaration name: (identifier) @local.definition.type)
(enum_variant name: (identifier) @local.definition.constant)

; Parameters (normal + rest) and the for-loop binding.
(parameter name: (identifier) @local.definition.parameter)
(rest_parameter name: (identifier) @local.definition.parameter)
(for_statement binding: (identifier) @local.definition.var)

; Import bindings.
(import_specifier (identifier) @local.definition.import)
(import_declaration namespace: (identifier) @local.definition.import)

; Destructuring rest collector.
(rest_element name: (identifier) @local.definition.var)

; ----- References ------------------------------------------------------------
; Any bare identifier use is a reference; the local resolver pairs it to the
; nearest in-scope definition above.
(identifier) @local.reference
