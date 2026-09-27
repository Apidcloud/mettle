//! Core-language references and the lexer keyword inventory, authored together.

use crate::{Span, TokenKind, ValueKind};

#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub name: &'static str,
    pub form: &'static str,
    pub description: &'static str,
    pub example: &'static str,
    pub parameters: &'static [(&'static str, &'static str)],
}

impl Item {
    #[must_use]
    pub fn hover(self) -> String {
        format!(
            "```text\n{}\n```\n\n{}\n\n```mettle\n{}\n```",
            self.form, self.description, self.example
        )
    }

    #[must_use]
    pub fn reference(self) -> String {
        let mut output = format!("# {}\n\n{}\n", self.name, self.hover());
        if !self.parameters.is_empty() {
            output.push_str("\n## Parameters\n");
            for (name, description) in self.parameters {
                use std::fmt::Write as _;
                let _ = write!(output, "\n### {name}\n\n{description}\n");
            }
        }
        output
    }
}

// Adding a reserved word requires its documentation in the same entry. The
// spelling-to-token mapping is generated here, not maintained in a second list.
macro_rules! keywords {
    ($($kind:ident => ($name:literal, $form:literal, $description:literal, $example:literal, $parameters:expr)),* $(,)?) => {
        pub const KEYWORDS: &[Item] = &[$(Item { name: $name, form: $form, description: $description, example: $example, parameters: $parameters }),*];
        pub(crate) fn keyword_token(name: &str) -> Option<TokenKind> {
            match name { $($name => Some(TokenKind::$kind),)* _ => None }
        }
        pub(crate) fn keyword_name(kind: &TokenKind) -> Option<&'static str> {
            match kind { $(TokenKind::$kind => Some($name),)* _ => None }
        }
    };
}

keywords! {
    If => ("if", "if (condition) { ... } else { ... }", "Execute only the selected statement branch. The condition must be boolean, and branch bindings stay local. Use return in branches for an early flow result; if is not a value expression.", "flow main { if (true) { return 1 } else { return 2 } }", &[]),
    For => ("for", "for value in collection { ... }", "Iterate an array or object sequentially. A value-producing loop maps into a collection of the same shape; a statement loop need not collect a result. Use two bindings (`for key, value in ...`) to access the array index or object key. This does not create parallel work or consume byte sources.", "flow main = for value in [1, 2] { value }", &[]),
    In => ("in", "for key, value in collection { ... }", "Separate loop bindings from an array or object to iterate. A single binding receives the value; two bindings receive index/key and value. Each iteration has its own local bindings.", "flow main = for index, value in [10, 20] { { index: index, value: value } }", &[]),
    Else => ("else", "if (condition) { ... } else { ... }", "Supply the branch evaluated when the preceding condition is false. Use `else if` for another condition. Unselected branches perform no I/O and do not evaluate their expressions.", "flow main { if (false) { return 1 } else if (true) { return 2 } else { return 3 } }", &[]),
    And => ("and", "left and right", "Boolean conjunction with short-circuiting: evaluate the right side only when the left side is true. Both evaluated operands must be boolean; false is not a substitute for an arbitrary value.", "flow main = true and (1 == 1)", &[]),
    Or => ("or", "left or right", "Boolean disjunction with short-circuiting: evaluate the right side only when the left side is false. It does not provide a fallback for null, empty strings, or other non-boolean values.", "flow main = false or true", &[]),
    Not => ("not", "not condition", "Negate a boolean expression. Non-boolean operands fail; combine with parentheses when negating a larger comparison or condition.", "flow main = not (1 == 2)", &[]),
    Is => ("is", "value is kind", "Check a primitive value kind without converting the value. `number` matches integers and floats. Sensitivity does not change the underlying kind; codecs and media-type names are not classes or kinds.", "flow main = 42 is number", &[]),
    As => ("as", "value as kind", "Explicitly convert a primitive value or fail if the conversion is unsupported, invalid, or lossy. Secret sensitivity is preserved. This does not invoke a codec, decode a media type, or collect a live source.", "flow main = \"42\" as integer", &[]),
    Mettle => ("flow", "flow name(parameters) { ... } / flow name = expression", "Declare a reusable workflow. Parameters accept positional or named arguments; parentheses may be omitted for argumentless declarations. The last expression is the result unless return is used. Top-level capability calls and anonymous flow blocks are also runnable entries.", "flow greet(name) = \"Hello, ${name}!\"\nflow main = greet(\"Mettle\")", &[]),
    Test => ("test", "test \"name\" { ... }", "Declare a named functional check, executed by `mettle test`, not ordinary run batches. Test-level assertion failures are collected so later checks can continue; execution-block assertions fail their block, and other runtime errors stop that test. Tests have no parameters and cannot use return. File-level tests are sequential unless jobs are configured.", "test \"values match\" { assert(1 == 1) }", &[]),
    Context => ("context", "context name { ... } / use context name", "Declare reusable values and capability defaults. A declaration alone does not apply the context: use `use context name`, or combine declaration and application with `use context name { ... }`. Anonymous `use context { ... }` also works. File-level uses apply to every flow/test in that file, even when written later.", "context settings { greeting: \"Hello\" }\nuse context settings\nflow main = greeting", &[]),
    Namespace => ("namespace", "namespace name / use namespace name", "Assign the file's declarations to a namespace, or make another namespace visible with `use namespace`. This changes name resolution, not file loading; projects discover source files through their manifest rules. Ambiguous unqualified names are rejected.", "namespace demo\nflow main = 42", &[]),
    Defaults => ("defaults", "defaults capability { option: value }", "Set supported capability defaults inside a context, such as HTTP baseUrl or timeout. Apply that context to use them; explicit call options override the defaults. The compiler validates default names and types against the capability descriptor.", "use context { defaults http { baseUrl: \"http://127.0.0.1:8080\" } }\nflow main = 42", &[]),
    Use => ("use", "use context name / use namespace name", "Apply a context's values/defaults or make namespace declarations visible. At file level, `use context` applies to all flows and tests in that file regardless of its position; inside a flow it applies to that flow. Named declaration-and-use and anonymous context blocks are supported. `use namespace` affects name lookup, not environment-file selection or file imports.", "use context credentials { token: \"demo-token\" }\nflow main = token", &[]),
    Return => ("return", "return expression", "Finish the current flow with an explicit value, skipping remaining statements. Most flows can simply end with their result expression. Return is not allowed in tests and cannot let a live source escape as a final execution result.", "flow main { return 42 }", &[]),
    Assert => ("assert", "assert(condition, message?)", "Check a boolean condition. In a flow, false fails the execution; at test level, it records an assertion failure and later checks continue. Inside an execution block such as retry, false fails the block. The optional message must be a string literal and may interpolate values; it is evaluated only on failure and sensitive values are redacted.", "test \"status check\" { assert(200 == 200, \"status should be 200\") }", &[("condition", "Boolean condition to check."), ("message", "Optional string literal explaining failure; interpolation runs only when the check fails.")]),
    Fail => ("fail", "fail(message)", "Stop the current execution with a failure and a string message. It is terminal: retry does not retry it, and owned parallel/load work is cancelled. It is not a process-wide exit; unrelated batch entries may continue. Sensitive message contents are redacted.", "flow main { fail(\"scenario cannot continue\") }", &[("message", "String explaining why this execution must stop.")]),
    Within => ("within", "within(timeout: duration) { ... }", "Run a block with an overall deadline, returning its final value if it finishes. Expiry fails the block and cancels its owned work. The timeout must be positive; this bounds the whole block, not each operation separately.", "flow main = within(timeout: 1s) { 42 }", &[("timeout", "Positive overall duration allowed for the block.")]),
    Retry => ("retry", "retry(attempts: count, delay: duration?) { ... }", "Re-evaluate a block after ordinary execution failures, returning the first successful result. Attempts includes the first run; delay is optional between failed attempts. Terminal fail and cancellation bypass retry. Retrying can duplicate remote side effects; create a fresh byte source inside each attempt.", "flow main = retry(attempts: 3, delay: 100ms) { 42 }", &[("attempts", "Positive total attempt count, including the first run."), ("delay", "Optional nonnegative wait between failed attempts.")]),
    Parallel => ("parallel", "parallel(limit: count?) { ... }", "Run owned branches concurrently with bounded admission. Named branches return an object; unnamed branches return an array in source order. Do not mix named and unnamed branches. A branch failure cancels and joins its siblings; this does not change file-level test scheduling.", "flow main = parallel(limit: 2) { left: 1, right: 2 }", &[("limit", "Optional positive maximum number of concurrently admitted branches.")]),
    Rate => ("rate", "rate(target: count, period: duration, duration: duration, limit: count?) { ... }", "Schedule a bounded workload at a target number of starts per period for a duration. When capacity is full, starts are dropped rather than queued without bound. The result contains workload metrics, not a collection of every iteration's value. Achieved rate may be below the target.", "flow main = rate(target: 2, period: 1s, duration: 1s, limit: 2) { 42 }", &[("target", "Positive target number of iteration starts per period."), ("period", "Positive period over which the target is measured."), ("duration", "Positive scheduling duration of the workload."), ("limit", "Optional positive maximum number of in-flight iterations.")]),
    Concurrency => ("concurrency", "concurrency(limit: count, duration: duration) { ... }", "Keep a bounded set of workload iterations active for a duration, starting replacements as iterations finish. Unlike rate, this targets active work rather than starts per period. Owned work is drained/cancelled within runtime bounds; the result contains aggregated workload metrics.", "flow main = concurrency(limit: 2, duration: 1s) { 42 }", &[("limit", "Positive maximum number of active iterations."), ("duration", "Positive workload duration.")]),
    True => ("true", "true", "The boolean true value. Use boolean values in conditions and assertions; Mettle does not apply truthiness to strings, numbers, or collections.", "flow main = true", &[]),
    False => ("false", "false", "The boolean false value. It short-circuits and expressions and selects the else branch of a conditional; it is distinct from null or zero.", "flow main = false", &[]),
    Null => ("null", "null / value is null", "The absence-of-value literal and primitive null kind. It is distinct from false, an empty string, and an omitted argument. An explicit null HTTP body is JSON null; omitting body sends no payload.", "flow main = null is null", &[]),
}

#[must_use]
pub fn item(name: &str) -> Option<&'static Item> {
    KEYWORDS.iter().find(|item| item.name == name)
}

#[must_use]
pub fn kind_description(kind: ValueKind) -> &'static str {
    match kind {
        ValueKind::Null => "The absence-of-value kind; distinct from false and empty values.",
        ValueKind::Boolean => "Boolean true or false, without truthiness coercion.",
        ValueKind::Integer => "A signed 64-bit integer; overflow is rejected rather than rounded.",
        ValueKind::Number => "A number: either an integer or a finite floating-point value.",
        ValueKind::String => "A complete UTF-8 string; text is a codec, not the string kind.",
        ValueKind::Bytes => {
            "Complete reusable representation bytes; decode explicitly with a codec."
        }
        ValueKind::Duration => {
            "A nonnegative duration, with units such as ms or s; not an ordinary number."
        }
        ValueKind::Array => "An ordered collection of values; for iteration returns an array.",
        ValueKind::Object => "A map of named fields; not a codec-defined class or schema.",
        ValueKind::Source => {
            "An owned, single-consumer byte source; aliases do not permit replay or implicit collection."
        }
    }
}

#[must_use]
pub fn reference(name: &str) -> Option<String> {
    let name = name.strip_prefix("language.").unwrap_or(name);
    if name == "language" {
        return Some(format!(
            "# Language\n\n{}\n",
            KEYWORDS
                .iter()
                .map(|item| format!("- `{}` — `{}`", item.name, item.form))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if let Some(item) = item(name) {
        return Some(item.reference());
    }
    let kind = ValueKind::parse(name)?;
    Some(format!(
        "# {name}\n\n{}\n\n```mettle\nvalue is {name}\nvalue as {name}\n```\n\nAn unsupported cast fails; checking a kind does not convert the value.\n",
        kind_description(kind)
    ))
}

/// Keyword or contextual primitive-kind token, excluding strings/comments.
#[must_use]
pub fn at(source: &str, byte: usize) -> Option<(&'static str, Span)> {
    let tokens = crate::documentation::tokens(source);
    let index = tokens
        .iter()
        .position(|token| token.span.start <= byte && byte < token.span.end)?;
    let token = &tokens[index];
    if index > 0 && matches!(tokens[index - 1].kind, TokenKind::Is | TokenKind::As) {
        let kind = ValueKind::parse(source.get(token.span.start..token.span.end)?)?;
        return Some((kind.name(), token.span));
    }
    Some((keyword_name(&token.kind)?, token.span))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyword_inventory_is_documented_and_examples_parse() {
        for item in KEYWORDS {
            let token = keyword_token(item.name).unwrap();
            assert_eq!(keyword_name(&token), Some(item.name));
            assert!(!item.description.is_empty());
            crate::parse(item.example).unwrap_or_else(|error| panic!("{}: {error}", item.name));
            assert_eq!(at(item.name, 0).unwrap().0, item.name);
        }
        assert!(at("// assert(true)", 3).is_none());
        assert!(at("\"use context\"", 1).is_none());
        assert!(at("assertion", 0).is_none());
        assert_eq!(at("value is bytes", 9).unwrap().0, "bytes");
    }
}
