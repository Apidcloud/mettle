(identifier) @variable
(comment) @comment
(string) @string
(escape_sequence) @string.escape
(interpolation_path) @variable
(interpolation ["${" "}"] @punctuation.special)
(integer) @number
(float) @number.float
(duration) @number
(boolean) @boolean
(null) @constant.builtin

["flow" "context" "namespace" "defaults" "test"] @keyword
"use" @keyword.import
"return" @keyword.return
["if" "else"] @keyword.conditional
["for" "in"] @keyword.repeat
["within" "retry" "parallel" "rate" "concurrency"] @keyword
["assert" "fail"] @function.builtin
["and" "or" "not"] @keyword.operator
["=" "==" "!=" "<" "<=" ">" ">=" "-"] @operator
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," ":" "."] @punctuation.delimiter

(flow_declaration name: (identifier) @function)
(parameter_list (identifier) @variable.parameter)
(member_expression property: (identifier) @variable.member)
(call_expression function: (identifier) @function.call)
(call_expression function: (member_expression property: (identifier) @function.call))
(object_field key: (identifier) @property)
(object_field key: (string) @string.special)
(named_argument name: (identifier) @property)
(named_branch name: (identifier) @property)
(for_expression key: (identifier) @variable)
(for_expression value: (identifier) @variable)
(namespace_declaration name: (identifier) @module)
(namespace_use name: (identifier) @module)
(context_declaration name: (identifier) @type)
(file_context_use name: (identifier) @type)
(context_use name: (identifier) @type)
(defaults_declaration capability: (identifier) @module)
[
  (timeout_option name: (identifier) @property)
  (attempts_option name: (identifier) @property)
  (delay_option name: (identifier) @property)
  (limit_option name: (identifier) @property)
  (target_option name: (identifier) @property)
  (period_option name: (identifier) @property)
  (duration_option name: (identifier) @property)
]

((call_expression function: (identifier) @function.builtin)
 (#any-of? @function.builtin "env" "senv" "echo"))
