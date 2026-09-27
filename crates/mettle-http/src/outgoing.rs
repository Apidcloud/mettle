//! Select representation metadata and encoding before a request consumes its source.

use bytes::Bytes;
use mettle_capability::content::BuiltinCodec;
use mettle_capability::media_type::MediaType;
use mettle_capability::{
    ByteSource, CapabilityError, DEFAULT_READ_BYTES, DEFAULT_TRANSFER_BYTES, Object, Span, Value,
};
use std::sync::Arc;

pub(crate) struct PreparedBody {
    pub bytes: Bytes,
    pub source: Option<Arc<ByteSource>>,
    pub content_type: Option<String>,
    pub max_bytes: usize,
}

fn string(value: &Value, span: Span) -> Result<&str, CapabilityError> {
    match value.revealed() {
        Value::String(value) => Ok(value),
        _ => Err(CapabilityError::new(
            "representation metadata must be a string",
            span,
        )),
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn prepare(options: &Object, span: Span) -> Result<PreparedBody, CapabilityError> {
    let payload = options.get("body").or_else(|| options.get("json"));
    let explicit_codec = options.contains_key("json").then_some(BuiltinCodec::Json);
    let headers = options.get("headers").and_then(Value::as_object);
    let content_types = headers
        .map(|headers| {
            headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if content_types.len() > 1 {
        return Err(CapabilityError::new("duplicate Content-Type headers", span));
    }
    let header = content_types
        .first()
        .map(|(_, value)| string(value, span))
        .transpose()?;
    let media = options
        .get("mediaType")
        .map(|value| string(value, span))
        .transpose()?;
    let parsed_header = header
        .map(|value| MediaType::parse(value, span))
        .transpose()?;
    let parsed_media = media
        .map(|value| MediaType::parse(value, span))
        .transpose()?;
    if let (Some(header), Some(media)) = (&parsed_header, &parsed_media)
        && !header.equivalent(media)
    {
        return Err(CapabilityError::new(
            "mediaType conflicts with Content-Type header",
            span,
        ));
    }
    let representation = parsed_media.as_ref().or(parsed_header.as_ref());
    if explicit_codec == Some(BuiltinCodec::Json)
        && representation.is_some_and(|media| media.codec() != BuiltinCodec::Json)
    {
        return Err(CapabilityError::new(
            "JSON request body requires a JSON Content-Type",
            span,
        ));
    }
    let raw =
        payload.is_some_and(|value| matches!(value.revealed(), Value::Bytes(_) | Value::Source(_)));
    if raw && explicit_codec.is_some_and(|codec| codec != BuiltinCodec::Bytes) {
        return Err(CapabilityError::new(
            "bytes/sources already contain representation bytes and cannot be encoded again as JSON or text",
            span,
        ));
    }
    let inferred = payload.map(|value| match value.revealed() {
        Value::String(_) => BuiltinCodec::Text,
        Value::Bytes(_) | Value::Source(_) => BuiltinCodec::Bytes,
        _ => BuiltinCodec::Json,
    });
    let codec = explicit_codec
        .or_else(|| representation.map(MediaType::codec))
        .or(inferred);
    let max_bytes = match options.get("maxBodyBytes").map(Value::revealed) {
        None => usize::try_from(
            if payload.is_some_and(|value| matches!(value.revealed(), Value::Source(_))) {
                DEFAULT_TRANSFER_BYTES
            } else {
                DEFAULT_READ_BYTES
            },
        )
        .expect("default body bound"),
        Some(Value::Integer(value)) if *value > 0 => usize::try_from(*value)
            .map_err(|_| CapabilityError::new("maxBodyBytes is too large", span))?,
        _ => {
            return Err(CapabilityError::new(
                "maxBodyBytes must be a positive integer",
                span,
            ));
        }
    };
    let source = match payload.map(Value::revealed) {
        Some(Value::Source(source)) => {
            if headers.is_some_and(|headers| {
                headers.keys().any(|name| {
                    name.eq_ignore_ascii_case("content-length")
                        || name.eq_ignore_ascii_case("transfer-encoding")
                })
            }) {
                return Err(CapabilityError::new(
                    "Content-Length/Transfer-Encoding cannot be supplied for a streamed request; framing is automatic",
                    span,
                ));
            }
            Some(source.clone())
        }
        _ => None,
    };
    let bytes = if let (Some(payload), Some(codec)) = (payload, codec) {
        if source.is_some() {
            Bytes::new()
        } else {
            // Raw values are never re-encoded, even when their media type is JSON.
            let encoder = if raw { BuiltinCodec::Bytes } else { codec };
            if !raw && let Some(media) = representation {
                media.validate_encoding(span)?;
            }
            let output = encoder.encode(payload, max_bytes, span).map_err(|error| {
                CapabilityError::new(
                    error.message.replace("maxBytes", "maxBodyBytes"),
                    error.span,
                )
            })?;
            let Value::Bytes(bytes) = output.revealed() else {
                unreachable!("codecs encode bytes")
            };
            Bytes::copy_from_slice(bytes)
        }
    } else {
        Bytes::new()
    };
    Ok(PreparedBody {
        bytes,
        source,
        content_type: header
            .or(media)
            .map(str::to_owned)
            .or_else(|| codec.map(|codec| codec.media_type().to_owned())),
        max_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn representation_selection_distinguishes_null_absence_raw_bytes_and_conflicts() {
        let span = Span::default();
        let prepare = |body: Value, media: &str| {
            super::prepare(
                &Object::from([
                    ("body".into(), body),
                    ("mediaType".into(), Value::String(media.into())),
                ]),
                span,
            )
        };
        assert_eq!(
            prepare(Value::String("hello".into()), "application/json")
                .unwrap()
                .bytes,
            "\"hello\""
        );
        assert_eq!(
            prepare(
                Value::Bytes(Arc::from(b"{\"ok\":true}".as_slice())),
                "application/json"
            )
            .unwrap()
            .bytes,
            "{\"ok\":true}"
        );
        assert_eq!(
            prepare(Value::Integer(23), "text/plain").unwrap().bytes,
            "23"
        );
        assert!(prepare(Value::String("hello".into()), "application/unknown").is_err());
        assert!(prepare(Value::String("hello".into()), "text/plain;charset=latin1").is_err());
        let null = super::prepare(&Object::from([("body".into(), Value::Null)]), span).unwrap();
        assert_eq!(null.bytes, "null");
        assert_eq!(null.content_type.as_deref(), Some("application/json"));
        assert!(
            super::prepare(&Object::new(), span)
                .unwrap()
                .content_type
                .is_none()
        );
        let conflicting = Object::from([
            ("mediaType".into(), Value::String("application/json".into())),
            (
                "headers".into(),
                Value::Object(Object::from([(
                    "Content-Type".into(),
                    Value::String("text/plain".into()),
                )])),
            ),
        ]);
        assert!(super::prepare(&conflicting, span).is_err());
    }
}
