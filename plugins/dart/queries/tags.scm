; Dart tags.scm — functions, classes, imports, type references
; Note: Dart's tree-sitter grammar (UserNobody14) has non-standard node types.
; call_expression does NOT exist. Calls are detected via selector patterns.

; ── Definitions ──

(declaration
  (function_signature
    name: (identifier) @name)) @definition.function

(class_definition
  name: (identifier) @name) @definition.class

(enum_declaration
  name: (identifier) @name) @definition.class

; mixin_declaration has identifier as positional child
(mixin_declaration
  (identifier) @name) @definition.class

; ── Imports ──

(import_or_export
  (library_import
    (import_specification
      (configurable_uri) @import.module))) @import

; ── Calls ──

; Dart uses selector patterns, not call_expression
; Constructor: new ClassName(args)
(new_expression
  (type_identifier) @call.name) @call

; ── Type references ──

(type_identifier) @reference.type
