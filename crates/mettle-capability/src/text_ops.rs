//! Bounded literal and regular-expression operations. CPU work stays off async workers.

use crate::documentation::OperationDocumentation;
use crate::{
    CapabilityError, FieldSchema, IoCancellation, IoContext, Object, OperationSchema, SchemaType,
    Span, Value,
};
use regex::{Captures, Regex, RegexBuilder};

const DETAILS: &str = "Literal selectors are exact and case-sensitive. Regex uses Rust regex syntax (Unicode, inline flags, no backreferences or lookaround). Positions count Unicode scalar values, with exclusive ends. Captures exclude the whole match; unmatched captures are null. Replacements are literal, never capture templates. Work is execution-owned and bounded: input/output maxBytes (default 1 MiB, hard maximum 10 MiB), pattern 4096 bytes, compiled regex 64 KiB, nesting 64, results at most 10,000, and a 32 MiB pattern-weighted search budget. Secret inputs, selectors, and options taint results; payloads are not captured by reports.";
const MAX: FieldSchema = FieldSchema::new("maxBytes", SchemaType::Integer)
    .documented("Positive input and output byte limit; default 1 MiB, maximum 10 MiB.");
const LIMIT: FieldSchema = FieldSchema::new("limit", SchemaType::Integer).documented("Positive result limit, default 1000, maximum 10,000. For split the final part contains the unsplit remainder; findAll/replace fail if more matches exist.");
const REGEX: FieldSchema = FieldSchema::new("regex", SchemaType::String).documented(
    "Regular-expression selector, at most 4096 bytes. Inline flags such as (?i) are supported.",
);
const TEXT: FieldSchema = FieldSchema::new("text", SchemaType::String)
    .documented("Nonempty literal selector. Exactly one of text and regex is required.");
const SEPARATOR: FieldSchema = FieldSchema::new("separator", SchemaType::String).documented("Nonempty literal separator; may also be the second positional argument. Exactly one of separator and regex is required.").positional();
const SPLIT_OPTIONS: &[FieldSchema] = &[SEPARATOR, REGEX, LIMIT, MAX];
const SEARCH_OPTIONS: &[FieldSchema] = &[TEXT, REGEX, LIMIT, MAX];
const FIND_OPTIONS: &[FieldSchema] = &[TEXT, REGEX, MAX];
const REPLACE_OPTIONS: &[FieldSchema] = &[
    TEXT,
    REGEX,
    FieldSchema::new("with", SchemaType::String).documented(
        "Literal replacement string; $1 and other capture-template spellings remain literal.",
    ),
    LIMIT,
    MAX,
];

const fn operation(
    name: &'static str,
    summary: &'static str,
    options: &'static [FieldSchema],
    conflicts: &'static [&'static [&'static str]],
    result: SchemaType,
    example: &'static str,
) -> OperationSchema {
    OperationSchema {
        name,
        parameters: &[SchemaType::String],
        parameter_names: &["value"],
        options,
        mutually_exclusive: conflicts,
        result,
        documentation: OperationDocumentation {
            summary,
            notes: &[],
            parameters: &[
                "Complete string to inspect. Encoded bytes must first be decoded explicitly.",
            ],
            details: DETAILS,
            example,
        },
    }
}
pub const SPLIT: OperationSchema = operation(
    "split",
    "Split a string, preserving leading, consecutive, and trailing empty parts. Regex separators that produce zero-width matches are rejected.",
    SPLIT_OPTIONS,
    &[&["separator", "regex"]],
    SchemaType::Array,
    "text.split(\"a,b,\", \",\")",
);
pub const MATCHES: OperationSchema = operation(
    "matches",
    "Test whether a regex matches anywhere in a string. Use ^ and $ for a whole-string check.",
    &[REGEX, MAX],
    &[],
    SchemaType::Boolean,
    "text.matches(\"HTTP 200\", regex: \"[0-9]+\")",
);
pub const FIND: OperationSchema = operation(
    "find",
    "Return the first match, or null. A match has text, start/end Unicode-scalar offsets, groups, and namedGroups.",
    FIND_OPTIONS,
    &[&["text", "regex"]],
    SchemaType::NullableObject(MatchValue::FIELDS),
    "text.find(\"id=42\", regex: \"(?P<id>[0-9]+)\")",
);
pub const FIND_ALL: OperationSchema = operation(
    "findAll",
    "Return a bounded array of non-overlapping match records, or an empty array. Exceeding limit fails instead of silently dropping matches.",
    SEARCH_OPTIONS,
    &[&["text", "regex"]],
    SchemaType::ObjectArray(MatchValue::FIELDS),
    "text.findAll(\"a1 b2\", regex: \"[0-9]+\")",
);
pub const REPLACE: OperationSchema = operation(
    "replace",
    "Replace all non-overlapping matches with a literal string. Exceeding the match/output bounds fails.",
    REPLACE_OPTIONS,
    &[&["text", "regex"]],
    SchemaType::String,
    "text.replace(\"a-b\", text: \"-\", with: \"_\")",
);

crate::result_object! {
    struct MatchValue {
        text => ("text", SchemaType::String, "Complete matched substring."),
        start => ("start", SchemaType::Integer, "Start offset in Unicode scalar values, not UTF-8 bytes."),
        end => ("end", SchemaType::Integer, "Exclusive end offset in Unicode scalar values."),
        groups => ("groups", SchemaType::Array, "Capturing groups in declaration order, excluding the whole match; unmatched groups are null."),
        named_groups => ("namedGroups", SchemaType::Object(&[]), "Named captures, with null for unmatched optional groups."),
    }
}

pub async fn invoke(
    operation: usize,
    arguments: Vec<Value>,
    options: Object,
    span: Span,
    context: &IoContext,
) -> Result<Value, CapabilityError> {
    context
        .blocking(span, move |cancel| {
            execute(operation, &arguments, &options, span, cancel)
        })
        .await
}

fn positive(
    options: &Object,
    name: &str,
    default: usize,
    maximum: usize,
    span: Span,
) -> Result<usize, CapabilityError> {
    match options.get(name).map(Value::revealed) {
        None => Ok(default),
        Some(Value::Integer(value)) if *value > 0 => usize::try_from(*value)
            .ok()
            .filter(|v| *v <= maximum)
            .ok_or_else(|| {
                CapabilityError::new(
                    format!("{name} exceeds the supported bound {maximum}"),
                    span,
                )
            }),
        _ => Err(CapabilityError::new(
            format!("{name} must be a positive integer"),
            span,
        )),
    }
}
fn string<'a>(
    value: Option<&'a Value>,
    name: &str,
    span: Span,
) -> Result<&'a str, CapabilityError> {
    match value.map(Value::revealed) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(CapabilityError::new(
            format!("{name} must be a string"),
            span,
        )),
    }
}
fn compile(pattern: &str, span: Span) -> Result<Regex, CapabilityError> {
    if pattern.len() > 4096 {
        return Err(CapabilityError::new(
            "regex exceeds the 4096 byte pattern bound",
            span,
        ));
    }
    RegexBuilder::new(pattern).size_limit(64 * 1024).dfa_size_limit(256 * 1024).nest_limit(64).build()
        .map_err(|_| CapabilityError::new("invalid or over-complex regular expression (lookaround and backreferences are not supported)", span))
}

#[allow(clippy::too_many_lines)]
fn execute(
    operation: usize,
    arguments: &[Value],
    options: &Object,
    span: Span,
    cancel: &IoCancellation,
) -> Result<Value, CapabilityError> {
    cancel.check()?;
    let input = string(arguments.first(), "value", span)?;
    let max_bytes = positive(options, "maxBytes", 1024 * 1024, 10 * 1024 * 1024, span)?;
    if input.len() > max_bytes {
        return Err(CapabilityError::new("text input exceeded maxBytes", span));
    }
    let limit = positive(options, "limit", 1000, 10_000, span)?;
    let literal_name = if operation == 2 { "separator" } else { "text" };
    let literal = options.get(literal_name);
    let pattern = options.get("regex");
    if usize::from(literal.is_some()) + usize::from(pattern.is_some()) != 1
        || (operation == 3 && pattern.is_none())
    {
        return Err(CapabilityError::new(
            format!("supply exactly one of {literal_name} and regex"),
            span,
        ));
    }
    let selector = string(pattern.or(literal), "selector", span)?;
    if literal.is_some() && selector.is_empty() {
        return Err(CapabilityError::new(
            "literal selectors cannot be empty",
            span,
        ));
    }
    if selector.len() > 4096 {
        return Err(CapabilityError::new(
            "text selector exceeded 4096 bytes",
            span,
        ));
    }
    let regex = if pattern.is_some() {
        Some(compile(selector, span)?)
    } else {
        None
    };
    if operation == 2 && regex.as_ref().is_some_and(|regex| regex.is_match("")) {
        return Err(CapabilityError::new(
            "regex split separators cannot match zero-width positions",
            span,
        ));
    }
    let weight = if regex.is_some() {
        selector.len().max(1)
    } else {
        1
    };
    // Searches resume after the previous non-overlapping match. Their scanned
    // segments partition the input, so charge the maximum aggregate once;
    // charging the full remaining suffix per match would reject useful findAll.
    if input.len().saturating_add(1).saturating_mul(weight) > 32 * 1024 * 1024 {
        return Err(CapabilityError::new(
            "text search work limit exceeded; narrow the input or simplify the pattern",
            span,
        ));
    }
    let sensitive = arguments.iter().any(Value::contains_sensitive)
        || options.values().any(Value::contains_sensitive);
    let protect = |v: Value| if sensitive { v.sensitive() } else { v };
    if operation == 3 {
        cancel.check()?;
        return Ok(protect(Value::Boolean(
            regex.as_ref().expect("regex selector").is_match(input),
        )));
    }
    let replacement = if operation == 6 {
        string(options.get("with"), "with", span)?
    } else {
        ""
    };
    let mut matches: Vec<(usize, usize, Option<Captures<'_>>)> = Vec::new();
    let mut from = 0;
    loop {
        if operation == 2 && matches.len() >= limit - 1 {
            break;
        }
        cancel.check()?;
        let found = if let Some(regex) = &regex {
            regex.captures_at(input, from).map(|captures| {
                let m = captures.get(0).expect("whole match");
                (m.start(), m.end(), Some(captures))
            })
        } else {
            input[from..]
                .find(selector)
                .map(|offset| (from + offset, from + offset + selector.len(), None))
        };
        let Some((start, end, captures)) = found else {
            break;
        };
        if operation == 2 && start == end {
            return Err(CapabilityError::new(
                "regex split separators cannot match zero-width positions",
                span,
            ));
        }
        if matches.len() >= limit {
            return Err(CapabilityError::new(
                "text match count exceeded limit",
                span,
            ));
        }
        let slots = captures.as_ref().map_or(1, Captures::len);
        if slots.saturating_mul(matches.len() + 1).saturating_mul(32) > max_bytes {
            return Err(CapabilityError::new(
                "text capture bookkeeping exceeded maxBytes",
                span,
            ));
        }
        matches.push((start, end, captures));
        if operation == 4 {
            break;
        }
        from = end;
        if start == end {
            let Some(next) = input[from..].chars().next() else {
                break;
            };
            from += next.len_utf8();
        }
    }
    let mut output_bytes = 0_usize;
    let mut account = |bytes: usize| -> Result<(), CapabilityError> {
        output_bytes = output_bytes.saturating_add(bytes);
        if output_bytes > max_bytes {
            return Err(CapabilityError::new("text output exceeded maxBytes", span));
        }
        Ok(())
    };
    let result = match operation {
        2 => {
            let mut parts = Vec::with_capacity(matches.len() + 1);
            let mut previous = 0;
            for (start, end, _) in matches {
                account(start - previous)?;
                parts.push(Value::String(input[previous..start].to_owned()));
                previous = end;
            }
            account(input.len() - previous)?;
            parts.push(Value::String(input[previous..].to_owned()));
            Value::Array(parts)
        }
        4 | 5 => {
            let mut records = Vec::with_capacity(matches.len());
            let mut byte_position = 0;
            let mut scalar_position = 0;
            for (start, end, captures) in matches {
                cancel.check()?;
                account(128)?;
                scalar_position += input[byte_position..start].chars().count();
                let length = input[start..end].chars().count();
                account(end - start)?;
                let mut groups = Vec::new();
                let mut named = Object::new();
                if let Some(captures) = captures {
                    for capture in captures.iter().skip(1) {
                        let value =
                            capture.map_or(Value::Null, |m| Value::String(m.as_str().to_owned()));
                        if let Value::String(value) = &value {
                            account(value.len())?;
                        }
                        groups.push(value);
                    }
                    for name in regex
                        .as_ref()
                        .expect("regex captures")
                        .capture_names()
                        .flatten()
                    {
                        let value = captures
                            .name(name)
                            .map_or(Value::Null, |m| Value::String(m.as_str().to_owned()));
                        if let Value::String(value) = &value {
                            account(value.len())?;
                        }
                        account(name.len())?;
                        named.insert(name.to_owned(), value);
                    }
                }
                records.push(
                    MatchValue {
                        text: Value::String(input[start..end].to_owned()),
                        start: Value::Integer(
                            i64::try_from(scalar_position).expect("bounded offset"),
                        ),
                        end: Value::Integer(
                            i64::try_from(scalar_position + length).expect("bounded offset"),
                        ),
                        groups: Value::Array(groups),
                        named_groups: Value::Object(named),
                    }
                    .into_value(),
                );
                byte_position = end;
                scalar_position += length;
            }
            if operation == 4 {
                records.into_iter().next().unwrap_or(Value::Null)
            } else {
                Value::Array(records)
            }
        }
        6 => {
            let mut result = String::new();
            let mut previous = 0;
            for (start, end, _) in matches {
                account(start - previous + replacement.len())?;
                result.push_str(&input[previous..start]);
                result.push_str(replacement);
                previous = end;
            }
            account(input.len() - previous)?;
            result.push_str(&input[previous..]);
            Value::String(result)
        }
        _ => return Err(CapabilityError::new("unknown text operation", span)),
    };
    Ok(protect(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn run(
        op: usize,
        input: &str,
        options: &[(&str, Value)],
    ) -> Result<Value, CapabilityError> {
        let context = IoContext::new(std::path::PathBuf::new());
        let result = invoke(
            op,
            vec![Value::String(input.to_owned())],
            options
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            Span::new(5, 10),
            &context,
        )
        .await;
        context.cleanup().await.unwrap();
        result
    }
    fn s(value: &str) -> Value {
        Value::String(value.to_owned())
    }
    #[tokio::test]
    async fn split_preserves_empty_parts_and_remainder() {
        assert_eq!(
            run(2, ",a,,", &[("separator", s(","))]).await.unwrap(),
            Value::Array(vec![s(""), s("a"), s(""), s("")])
        );
        assert_eq!(
            run(
                2,
                "a,b,c",
                &[("separator", s(",")), ("limit", Value::Integer(2))]
            )
            .await
            .unwrap(),
            Value::Array(vec![s("a"), s("b,c")])
        );
        assert!(run(2, "a", &[("regex", s("^"))]).await.is_err());
    }
    #[tokio::test]
    async fn unicode_positions_optional_captures_and_no_match() {
        let result = run(4, "é id=42", &[("regex", s("(?P<id>[0-9]+)(x)?"))])
            .await
            .unwrap();
        let fields = result.as_object().unwrap();
        assert_eq!(fields["start"], Value::Integer(5));
        assert_eq!(fields["end"], Value::Integer(7));
        assert_eq!(fields["groups"], Value::Array(vec![s("42"), Value::Null]));
        assert_eq!(
            run(4, "abc", &[("text", s("z"))]).await.unwrap(),
            Value::Null
        );
        let Value::Array(matches) = run(5, "é", &[("regex", s(""))]).await.unwrap() else {
            panic!("expected matches")
        };
        assert_eq!(matches.len(), 2);
    }
    #[tokio::test]
    async fn replacement_is_literal_and_limits_fail_loudly() {
        assert_eq!(
            run(6, "12", &[("regex", s("[0-9]")), ("with", s("$1"))])
                .await
                .unwrap(),
            s("$1$1")
        );
        assert!(
            run(5, "111", &[("text", s("1")), ("limit", Value::Integer(2))])
                .await
                .is_err()
        );
        assert!(
            run(3, "abc", &[("regex", s("(?=secret)"))])
                .await
                .unwrap_err()
                .message
                .find("secret")
                .is_none()
        );
        assert!(
            run(4, "abc", &[("regex", s("a").sensitive())])
                .await
                .unwrap()
                .contains_sensitive()
        );
    }
}
