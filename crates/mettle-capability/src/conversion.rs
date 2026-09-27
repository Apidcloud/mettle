//! Explicit built-in conversions, separate from representation codecs.

use mettle_syntax::{ExpressionKind, ValueKind, parse_value};

use crate::{CapabilityError, Span, Value};

#[must_use]
pub fn matches_kind(value: &Value, kind: ValueKind) -> bool {
    matches!(
        (value.revealed(), kind),
        (Value::Null, ValueKind::Null)
            | (Value::Boolean(_), ValueKind::Boolean)
            | (Value::Integer(_), ValueKind::Integer | ValueKind::Number)
            | (Value::Float(_), ValueKind::Number)
            | (Value::String(_), ValueKind::String)
            | (Value::Bytes(_), ValueKind::Bytes)
            | (Value::Duration(_), ValueKind::Duration)
            | (Value::Array(_), ValueKind::Array)
            | (Value::Object(_), ValueKind::Object)
            | (Value::Source(_), ValueKind::Source)
    )
}

/// Convert a value without implicit truthiness, truncation, or representation decoding.
/// # Errors
/// Returns a source-aware error for unsupported or invalid conversions, without input values.
pub fn cast_value(value: &Value, kind: ValueKind, span: Span) -> Result<Value, CapabilityError> {
    let invalid = || {
        CapabilityError::new(
            format!(
                "cannot convert {} to {}; use an explicit codec for content",
                value.type_name(),
                kind.name()
            ),
            span,
        )
    };
    let result = match (value.revealed(), kind) {
        (Value::Float(number), ValueKind::Number | ValueKind::String) if !number.is_finite() => {
            return Err(invalid());
        }
        (_, _) if matches_kind(value, kind) => value.clone(),
        (
            Value::String(text),
            ValueKind::Number | ValueKind::Integer | ValueKind::Boolean | ValueKind::Duration,
        ) => {
            if text.len() > 4096 || text.contains("//") {
                return Err(invalid());
            }
            let expression = parse_value(text.trim()).map_err(|_| invalid())?;
            let parsed = match expression.kind {
                ExpressionKind::Integer(value) => Value::Integer(value),
                ExpressionKind::Float(value) => Value::Float(value),
                ExpressionKind::Boolean(value) => Value::Boolean(value),
                ExpressionKind::DurationNanos(value) => {
                    Value::Duration(std::time::Duration::from_nanos(value))
                }
                _ => return Err(invalid()),
            };
            if matches_kind(&parsed, kind) {
                parsed
            } else {
                return Err(invalid());
            }
        }
        (Value::Float(number), ValueKind::Integer) => {
            // Upper bound is exclusive: i64::MAX rounds up when represented as f64.
            if !number.is_finite()
                || number.fract() != 0.0
                || *number < -9_223_372_036_854_775_808.0
                || *number >= 9_223_372_036_854_775_808.0
            {
                return Err(invalid());
            }
            #[allow(clippy::cast_possible_truncation)]
            let integer = *number as i64;
            Value::Integer(integer)
        }
        (
            Value::Null
            | Value::Boolean(_)
            | Value::Integer(_)
            | Value::Float(_)
            | Value::Duration(_),
            ValueKind::String,
        ) => Value::String(value.revealed().exposed_string()),
        _ => return Err(invalid()),
    };
    Ok(if value.contains_sensitive() {
        result.sensitive()
    } else {
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_are_explicit_lossless_and_sensitive() {
        let span = Span::new(4, 9);
        for (input, expected) in [
            ("-0x2a", Value::Integer(-42)),
            ("1_000", Value::Integer(1000)),
            ("1.25e2", Value::Float(125.0)),
        ] {
            assert_eq!(
                cast_value(&Value::String(input.into()), ValueKind::Number, span).unwrap(),
                expected
            );
        }
        assert_eq!(
            cast_value(&Value::String(" true ".into()), ValueKind::Boolean, span).unwrap(),
            Value::Boolean(true)
        );
        assert!(cast_value(&Value::Float(1.5), ValueKind::Integer, span).is_err());
        assert_eq!(
            cast_value(
                &Value::String("9007199254740993".into()),
                ValueKind::Integer,
                span
            )
            .unwrap(),
            Value::Integer(9_007_199_254_740_993)
        );
        assert!(
            cast_value(
                &Value::String("9007199254740993.0".into()),
                ValueKind::Integer,
                span
            )
            .is_err()
        );
        assert_eq!(
            cast_value(
                &Value::Float(-9_223_372_036_854_775_808.0),
                ValueKind::Integer,
                span
            )
            .unwrap(),
            Value::Integer(i64::MIN)
        );
        assert!(
            cast_value(
                &Value::Float(9_223_372_036_854_775_808.0),
                ValueKind::Integer,
                span
            )
            .is_err()
        );
        assert!(cast_value(&Value::Integer(1), ValueKind::Boolean, span).is_err());
        assert!(cast_value(&Value::String("NaN".into()), ValueKind::Number, span).is_err());
        assert!(matches_kind(&Value::Integer(1), ValueKind::Number));
        assert!(!matches_kind(&Value::Float(1.0), ValueKind::Integer));
        let secret = Value::String("private-invalid-number".into()).sensitive();
        let error = cast_value(&secret, ValueKind::Number, span).unwrap_err();
        assert_eq!(error.span, span);
        assert!(!error.message.contains("private"));
        assert!(
            cast_value(
                &Value::String("23".into()).sensitive(),
                ValueKind::Number,
                span
            )
            .unwrap()
            .is_sensitive()
        );
    }
}
