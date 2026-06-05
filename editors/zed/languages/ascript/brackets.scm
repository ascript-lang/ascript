; All structural brackets — matched as pairs by the editor's bracket engine.
[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

; Template interpolation delimiters `${ … }` are a bracket pair too.
(template_substitution
  "${" @punctuation.special
  "}" @punctuation.special)
