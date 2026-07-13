; Minimal Kotlin highlights for the tree-sitter-kotlin-ng grammar, which
; ships no query files. Uses only nodes/tokens the grammar actually
; exposes (verified against node-types.json).

[
  (line_comment)
  (block_comment)
] @comment

[
  (string_literal)
  (multiline_string_literal)
  (character_literal)
] @string

(string_content) @string

[
  (number_literal)
  (float_literal)
] @number

[
  "fun" "val" "var" "class" "object" "interface" "enum" "sealed" "data"
  "if" "else" "when" "for" "while" "do" "return" "throw" "try" "catch"
  "finally" "import" "package" "in" "is" "as" "by" "typealias"
  "companion" "init" "constructor" "this" "super" "where"
  "public" "private" "protected" "internal" "abstract" "final" "open"
  "override" "suspend" "inline" "lateinit" "const" "vararg" "out"
  "operator" "infix" "inner" "actual" "expect" "external" "tailrec"
] @keyword

(user_type) @type

(function_declaration (identifier) @function)
(call_expression (identifier) @function)

(annotation) @attribute

(identifier) @variable
