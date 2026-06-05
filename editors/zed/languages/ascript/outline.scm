; Zed outline view. Zed's outline uses tree-sitter `tags`; this is derived from
; the promoted grammar's queries/tags.scm (LSP Phase 5), mapped onto Zed's
; outline capture convention (@context = keyword/name run, @name = symbol name).

(function_declaration
  "fn" @context
  name: (identifier) @name) @item

(method_definition
  name: (identifier) @name) @item

(class_declaration
  "class" @context
  name: (identifier) @name) @item

(enum_declaration
  "enum" @context
  name: (identifier) @name) @item

(enum_variant
  name: (identifier) @name) @item
