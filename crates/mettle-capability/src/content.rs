//! Protocol-independent bounded JSON, UTF-8 text, and identity byte codecs.

use std::io::{self, Write};
use std::sync::Arc;

use serde::Serialize;
use serde::ser::{Error as _, SerializeMap, SerializeSeq};

use crate::{CapabilityError, Span, Value};

// serde_json's default recursion budget accepts at most 127 nested containers.
// Encoding must not produce content that our own decoder cannot read back.
const MAX_JSON_CONTAINERS: usize = 127;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinCodec {
    Json,
    Text,
    Bytes,
}

impl BuiltinCodec {
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Text => "text/plain; charset=utf-8",
            Self::Bytes => "application/octet-stream",
        }
    }

    /// # Errors
    /// Rejects unsupported values, non-finite numbers, excessive depth or output size.
    pub fn encode(
        self,
        value: &Value,
        max_bytes: usize,
        span: Span,
    ) -> Result<Value, CapabilityError> {
        let bytes = match (self, value.revealed()) {
            (Self::Bytes, Value::Bytes(bytes)) => {
                check_limit(bytes.len(), max_bytes, span)?;
                return Ok(value.clone());
            }
            (Self::Bytes, _) => {
                return Err(CapabilityError::new(
                    "bytes codec requires already encoded bytes",
                    span,
                ));
            }
            (Self::Text, Value::String(text)) => {
                check_limit(text.len(), max_bytes, span)?;
                text.as_bytes().to_vec()
            }
            _ => {
                let mut output = LimitedOutput {
                    bytes: Vec::new(),
                    max_bytes,
                };
                serde_json::to_writer(&mut output, &JsonValue { value, depth: 0 }).map_err(
                    |error| CapabilityError::new(format!("content encoding failed: {error}"), span),
                )?;
                output.bytes
            }
        };
        let result = Value::Bytes(Arc::from(bytes));
        Ok(if value.contains_sensitive() {
            result.sensitive()
        } else {
            result
        })
    }

    /// # Errors
    /// Rejects invalid JSON/UTF-8, unsupported values, numeric overflow or excessive input.
    pub fn decode(
        self,
        value: &Value,
        max_bytes: usize,
        span: Span,
    ) -> Result<Value, CapabilityError> {
        let bytes: &[u8] = match value.revealed() {
            Value::Bytes(bytes) => bytes,
            Value::String(text) if self != Self::Bytes => text.as_bytes(),
            _ => {
                return Err(CapabilityError::new(
                    "decoding requires complete bytes (JSON/text also accept strings); collect sources explicitly",
                    span,
                ));
            }
        };
        check_limit(bytes.len(), max_bytes, span)?;
        let result = match self {
            Self::Json => {
                let json = serde_json::from_slice(bytes).map_err(|error| {
                    CapabilityError::new(format!("invalid JSON content: {error}"), span)
                })?;
                from_json(json, span)?
            }
            Self::Text => Value::String(
                std::str::from_utf8(bytes)
                    .map_err(|_| CapabilityError::new("text content is not valid UTF-8", span))?
                    .to_owned(),
            ),
            Self::Bytes => return Ok(value.clone()),
        };
        Ok(if value.contains_sensitive() {
            result.sensitive()
        } else {
            result
        })
    }
}

fn check_limit(length: usize, max_bytes: usize, span: Span) -> Result<(), CapabilityError> {
    if length > max_bytes {
        Err(CapabilityError::new(
            format!("content exceeds maxBytes ({max_bytes})"),
            span,
        ))
    } else {
        Ok(())
    }
}

struct LimitedOutput {
    bytes: Vec<u8>,
    max_bytes: usize,
}
impl Write for LimitedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.max_bytes.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("content exceeds maxBytes"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct JsonValue<'a> {
    value: &'a Value,
    depth: usize,
}
impl Serialize for JsonValue<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.depth >= MAX_JSON_CONTAINERS
            && matches!(self.value.revealed(), Value::Array(_) | Value::Object(_))
        {
            return Err(S::Error::custom(
                "content nesting exceeds 127 container levels",
            ));
        }
        match self.value.revealed() {
            Value::Null => serializer.serialize_unit(),
            Value::Boolean(value) => serializer.serialize_bool(*value),
            Value::Integer(value) => serializer.serialize_i64(*value),
            Value::Float(value) if value.is_finite() => serializer.serialize_f64(*value),
            Value::Float(_) => Err(S::Error::custom("JSON number must be finite")),
            Value::String(value) => serializer.serialize_str(value),
            Value::Array(values) => {
                let mut array = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    array.serialize_element(&JsonValue {
                        value,
                        depth: self.depth + 1,
                    })?;
                }
                array.end()
            }
            Value::Object(fields) => {
                let mut object = serializer.serialize_map(Some(fields.len()))?;
                for (name, value) in fields {
                    object.serialize_entry(
                        name,
                        &JsonValue {
                            value,
                            depth: self.depth + 1,
                        },
                    )?;
                }
                object.end()
            }
            _ => Err(S::Error::custom(
                "JSON/text encoding requires JSON-compatible values; bytes, sources, and durations need an explicit conversion",
            )),
        }
    }
}

/// Shared numeric and JSON-value policy for codecs and existing HTTP responses.
/// # Errors
/// Rejects integers outside i64 and decimals outside finite f64.
pub fn from_json(value: serde_json::Value, span: Span) -> Result<Value, CapabilityError> {
    Ok(match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Boolean(value),
        serde_json::Value::Number(value) => {
            let spelling = value.to_string();
            if spelling.contains(['.', 'e', 'E']) {
                Value::Float(
                    value
                        .as_f64()
                        .filter(|number| number.is_finite())
                        .ok_or_else(|| {
                            CapabilityError::new(
                                "JSON decimal is outside Mettle's finite number range",
                                span,
                            )
                        })?,
                )
            } else {
                Value::Integer(value.as_i64().ok_or_else(|| {
                    CapabilityError::new(
                        "JSON integer is outside Mettle's supported 64-bit range",
                        span,
                    )
                })?)
            }
        }
        serde_json::Value::String(value) => Value::String(value),
        serde_json::Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| from_json(value, span))
                .collect::<Result<_, _>>()?,
        ),
        serde_json::Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(name, value)| Ok((name, from_json(value, span)?)))
                .collect::<Result<_, CapabilityError>>()?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_nesting_limit_matches_encoding_and_decoding() {
        let span = Span::default();
        let mut value = Value::Integer(1);
        for _ in 0..127 {
            value = Value::Array(vec![value]);
        }
        let encoded = BuiltinCodec::Json.encode(&value, 1000, span).unwrap();
        assert_eq!(
            BuiltinCodec::Json.decode(&encoded, 1000, span).unwrap(),
            value
        );
        value = Value::Array(vec![value]);
        assert!(BuiltinCodec::Json.encode(&value, 1000, span).is_err());
        let input = Value::String(format!("{}1{}", "[".repeat(128), "]".repeat(128)));
        assert!(BuiltinCodec::Json.decode(&input, 1000, span).is_err());
    }

    #[test]
    fn codecs_are_bounded_strict_reusable_and_sensitive() {
        let span = Span::new(2, 8);
        for value in [
            Value::Null,
            Value::Boolean(true),
            Value::Integer(i64::MAX),
            Value::Float(1.5),
            Value::String("hello".into()),
            Value::Array(vec![Value::Integer(3)]),
        ] {
            let bytes = BuiltinCodec::Json.encode(&value, 1000, span).unwrap();
            assert_eq!(
                BuiltinCodec::Json.decode(&bytes, 1000, span).unwrap(),
                value
            );
        }
        assert!(
            BuiltinCodec::Json
                .decode(&Value::String("9223372036854775808".into()), 100, span)
                .is_err()
        );
        assert!(
            BuiltinCodec::Json
                .decode(&Value::String("1e999".into()), 100, span)
                .is_err()
        );
        assert!(
            BuiltinCodec::Json
                .encode(&Value::String("more".into()), 2, span)
                .is_err()
        );
        // Bound escaped output, not only the unencoded input size.
        assert!(
            BuiltinCodec::Json
                .encode(&Value::String("\n\n".into()), 5, span)
                .is_err()
        );
        let mut nested = Value::Integer(1);
        for _ in 0..130 {
            nested = Value::Array(vec![nested]);
        }
        assert!(
            BuiltinCodec::Json
                .encode(&nested, 1000, span)
                .unwrap_err()
                .message
                .contains("nesting")
        );
        let deep_json = Value::String(format!("{}0{}", "[".repeat(130), "]".repeat(130)));
        assert!(BuiltinCodec::Json.decode(&deep_json, 1000, span).is_err());
        assert!(
            BuiltinCodec::Text
                .decode(&Value::Bytes(Arc::from([255])), 10, span)
                .is_err()
        );
        assert!(
            BuiltinCodec::Json
                .encode(&Value::Float(f64::NAN), 100, span)
                .is_err()
        );
        let bytes = BuiltinCodec::Text
            .encode(&Value::Integer(23).sensitive(), 100, span)
            .unwrap();
        assert!(bytes.is_sensitive());
        assert!(
            BuiltinCodec::Text
                .decode(&bytes, 100, span)
                .unwrap()
                .is_sensitive()
        );
        assert_eq!(
            BuiltinCodec::Text
                .decode(
                    &BuiltinCodec::Text
                        .encode(&Value::String("plain".into()), 100, span)
                        .unwrap(),
                    100,
                    span
                )
                .unwrap(),
            Value::String("plain".into())
        );
    }
}
