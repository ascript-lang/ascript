; ----- Functions (declarations, methods, arrows) -----------------------------
(function_declaration body: (block) @function.inner) @function.outer
(method_definition body: (block) @function.inner) @function.outer
(arrow_function body: (block) @function.inner) @function.outer
(arrow_function body: (_) @function.inner) @function.outer

; ----- Classes ---------------------------------------------------------------
(class_declaration body: (class_body) @class.inner) @class.outer

; ----- Enums (treated as a class-like type body for navigation) --------------
(enum_declaration) @class.outer

; ----- Parameters ------------------------------------------------------------
(parameter name: (identifier) @parameter.inner) @parameter.outer
(rest_parameter name: (identifier) @parameter.inner) @parameter.outer

; ----- Call arguments (as "parameter"-class textobjects at call sites) --------
(arguments (_) @parameter.inner)

; ----- Comments --------------------------------------------------------------
(line_comment) @comment.outer
(block_comment) @comment.outer
