//! Shared compile-time schemas and runtime interfaces for Mettle capabilities.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub use mettle_syntax::Span;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaType {
    Boolean,
    Body,
    Bytes,
    Duration,
    Integer,
    Json,
    Object(&'static [FieldSchema]),
    String,
    StringMap,
}

impl SchemaType {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Body => "HTTP body value",
            Self::Bytes => "bytes",
            Self::Duration => "duration",
            Self::Integer => "integer",
            Self::Json => "JSON value",
            Self::Object(_) => "object",
            Self::String => "string",
            Self::StringMap => "object containing string values",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldSchema {
    pub name: &'static str,
    pub value_type: SchemaType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationSchema {
    pub name: &'static str,
    pub parameters: &'static [SchemaType],
    pub parameter_names: &'static [&'static str],
    pub options: &'static [FieldSchema],
    pub mutually_exclusive: &'static [&'static [&'static str]],
    pub result: SchemaType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityDescriptor {
    pub name: &'static str,
    pub defaults: &'static [FieldSchema],
    pub operations: &'static [OperationSchema],
}

pub type Object = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Bytes(Arc<[u8]>),
    Duration(Duration),
    Array(Vec<Self>),
    Object(Object),
    Sensitive(Box<Self>),
}

impl Value {
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Sensitive(value) => value.type_name(),
            Self::Null => "null",
            Self::Boolean(_) => "boolean",
            Self::Integer(_) => "integer",
            Self::Float(_) => "number",
            Self::String(_) => "string",
            Self::Bytes(_) => "bytes",
            Self::Duration(_) => "duration",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    #[must_use]
    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Self::Sensitive(value) => value.as_object(),
            Self::Object(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn sensitive(self) -> Self {
        match self {
            Self::Sensitive(_) => self,
            value => Self::Sensitive(Box::new(value)),
        }
    }

    #[must_use]
    pub const fn revealed(&self) -> &Self {
        match self {
            Self::Sensitive(value) => value.revealed(),
            value => value,
        }
    }

    #[must_use]
    pub const fn is_sensitive(&self) -> bool {
        matches!(self, Self::Sensitive(_))
    }

    #[must_use]
    pub fn contains_sensitive(&self) -> bool {
        match self {
            Self::Sensitive(_) => true,
            Self::Array(values) => values.iter().any(Self::contains_sensitive),
            Self::Object(fields) => fields.values().any(Self::contains_sensitive),
            _ => false,
        }
    }

    #[must_use]
    pub fn exposed_string(&self) -> String {
        match self.revealed() {
            Self::String(value) => value.clone(),
            Self::Array(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(exposed_json_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Object(fields) => format!(
                "{{{}}}",
                fields
                    .iter()
                    .map(|(name, value)| format!(
                        "\"{}\": {}",
                        escape_json(name),
                        exposed_json_value(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            value => value.to_string(),
        }
    }

    pub fn visit_sensitive_strings(&self, visitor: &mut impl FnMut(&str)) {
        match self {
            Self::Sensitive(value) => visit_strings(value, visitor),
            Self::Array(values) => {
                for value in values {
                    value.visit_sensitive_strings(visitor);
                }
            }
            Self::Object(fields) => {
                for value in fields.values() {
                    value.visit_sensitive_strings(visitor);
                }
            }
            _ => {}
        }
    }
}

fn exposed_json_value(value: &Value) -> String {
    match value.revealed() {
        Value::String(value) => format!("\"{}\"", escape_json(value)),
        value => value.exposed_string(),
    }
}

fn visit_strings(value: &Value, visitor: &mut impl FnMut(&str)) {
    match value.revealed() {
        Value::String(value) => visitor(value),
        Value::Array(values) => {
            for value in values {
                visit_strings(value, visitor);
            }
        }
        Value::Object(fields) => {
            for value in fields.values() {
                visit_strings(value, visitor);
            }
        }
        _ => {}
    }
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sensitive(_) => formatter.write_str("[REDACTED]"),
            Self::Null => formatter.write_str("null"),
            Self::Boolean(value) => value.fmt(formatter),
            Self::Integer(value) => value.fmt(formatter),
            Self::Float(value) => value.fmt(formatter),
            Self::String(value) => formatter.write_str(value),
            Self::Bytes(value) => {
                formatter.write_str("[")?;
                for (index, byte) in value.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{byte}")?;
                }
                formatter.write_str("]")
            }
            Self::Duration(value) => write!(formatter, "{}ns", value.as_nanos()),
            Self::Array(values) => {
                formatter.write_str("[")?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write_json_value(formatter, value)?;
                }
                formatter.write_str("]")
            }
            Self::Object(value) => {
                formatter.write_str("{")?;
                for (index, (key, value)) in value.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "\"{}\": ", escape_json(key))?;
                    write_json_value(formatter, value)?;
                }
                formatter.write_str("}")
            }
        }
    }
}

fn write_json_value(formatter: &mut fmt::Formatter<'_>, value: &Value) -> fmt::Result {
    match value {
        Value::String(value) => write!(formatter, "\"{}\"", escape_json(value)),
        _ => fmt::Display::fmt(value, formatter),
    }
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character <= '\u{1f}' => {
                write!(escaped, "\\u{:04x}", u32::from(character))
                    .expect("writing to a string cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityError {
    pub message: String,
    pub span: Span,
}

impl CapabilityError {
    #[must_use]
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CapabilityError {}

pub type CapabilityFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, CapabilityError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportOutcome {
    Success,
    Warning,
    Failure,
    Neutral,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReportSection {
    Fields {
        title: String,
        fields: Vec<(String, String)>,
    },
    Value {
        title: String,
        value: Value,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct OperationReport {
    pub summary: String,
    pub outcome: String,
    pub outcome_kind: ReportOutcome,
    pub sections: Vec<ReportSection>,
    pub payload: Option<Value>,
}

pub trait Capability: Send + Sync {
    fn name(&self) -> &'static str;

    fn operation_name(&self, _operation: usize) -> &'static str {
        "operation"
    }

    fn merge_options(&self, defaults: &mut Object, overrides: Object) {
        merge_objects(defaults, overrides);
    }

    fn report(&self, _operation: usize, _result: &Value) -> Option<OperationReport> {
        None
    }

    fn invoke(
        &self,
        operation: usize,
        arguments: Vec<Value>,
        options: Object,
        span: Span,
    ) -> CapabilityFuture<'_>;
}

pub fn merge_objects(target: &mut Object, source: Object) {
    for (name, value) in source {
        if let (Some(Value::Object(target)), Value::Object(source)) =
            (target.get_mut(&name), &value)
        {
            merge_objects(target, source.clone());
        } else {
            target.insert(name, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::Value;

    #[test]
    fn structured_values_have_stable_json_like_output() {
        let value = Value::Object(BTreeMap::from([
            ("active".to_owned(), Value::Boolean(true)),
            (
                "name".to_owned(),
                Value::String("João \"Mettle\"\n\u{0001}".to_owned()),
            ),
        ]));
        assert_eq!(
            value.to_string(),
            "{\"active\": true, \"name\": \"João \\\"Mettle\\\"\\n\\u0001\"}"
        );
    }
}
