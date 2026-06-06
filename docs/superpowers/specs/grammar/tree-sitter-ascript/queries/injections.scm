; Template `${ … }` substitutions are AScript expressions — re-inject them so the
; full highlight/locals machinery applies inside interpolations (including nested
; templates).
((template_substitution) @injection.content
  (#set! injection.language "ascript")
  (#set! injection.include-children))

; Comments carry no sub-language by default; a `// tree-sitter: <lang>` style
; directive could be added here later.

; ----- Future stubs (no stdlib surface yet) ---------------------------------
; When tagged-template / SQL / HTML / CSS string DSLs land, register them here by
; matching the tag identifier of the call and injecting into the template string,
; e.g.:
;   ((call_expression
;      function: (identifier) @_tag (#eq? @_tag "sql")
;      arguments: (arguments (template_string) @injection.content))
;     (#set! injection.language "sql"))
; (Left commented: `sql`/`html`/`css` tagged templates are not in the language yet.)
