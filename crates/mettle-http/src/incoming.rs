//! Bounded response representation selection and native-value decoding.

use hyper::header::{CONTENT_ENCODING, CONTENT_TYPE};
use hyper::{HeaderMap, Method};
use mettle_capability::content::BuiltinCodec;
use mettle_capability::media_type::MediaType;
use mettle_capability::{CapabilityError, Span, Value};

pub fn bodyless(method: &Method, status: u16) -> bool {
    *method == Method::HEAD || matches!(status, 204 | 205 | 304)
}

pub fn representation(
    headers: &HeaderMap,
    span: Span,
) -> Result<Option<MediaType>, CapabilityError> {
    let mut values = headers.get_all(CONTENT_TYPE).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(CapabilityError::new(
            "duplicate HTTP response Content-Type headers",
            span,
        ));
    }
    let value = value
        .to_str()
        .map_err(|_| CapabilityError::new("invalid HTTP response Content-Type", span))?;
    MediaType::parse(value, span)
        .map(Some)
        .map_err(|_| CapabilityError::new("invalid HTTP response Content-Type", span))
}

pub fn decode(
    bytes: &Value,
    media: Option<&MediaType>,
    headers: &HeaderMap,
    has_no_body: bool,
    max_bytes: usize,
    span: Span,
) -> Result<Value, CapabilityError> {
    if has_no_body {
        return Ok(if bytes.contains_sensitive() {
            Value::Null.sensitive()
        } else {
            Value::Null
        });
    }
    for encoding in headers.get_all(CONTENT_ENCODING) {
        let encoding = encoding
            .to_str()
            .map_err(|_| CapabilityError::new("invalid HTTP response Content-Encoding", span))?;
        if !encoding
            .split(',')
            .all(|part| part.trim().eq_ignore_ascii_case("identity"))
        {
            return Err(CapabilityError::new(
                "unsupported HTTP response Content-Encoding; automatic decompression is not implemented",
                span,
            ));
        }
    }
    if let Some(media) = media {
        media.validate_encoding(span).map_err(|_| {
            CapabilityError::new(
                "HTTP response JSON/text decoding only supports charset=utf-8",
                span,
            )
        })?;
    }
    media
        .map_or(BuiltinCodec::Bytes, MediaType::codec)
        .decode(bytes, max_bytes, span)
        .map_err(|error| {
            CapabilityError::new(
                format!("HTTP response body could not be decoded: {}", error.message),
                span,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;
    use std::sync::Arc;

    fn decoded(content_type: Option<&str>, input: &[u8]) -> Result<Value, CapabilityError> {
        let mut headers = HeaderMap::new();
        if let Some(content_type) = content_type {
            headers.insert(CONTENT_TYPE, HeaderValue::from_str(content_type).unwrap());
        }
        let span = Span::new(10, 20);
        let media = representation(&headers, span)?;
        decode(
            &Value::Bytes(Arc::from(input)),
            media.as_ref(),
            &headers,
            false,
            1024,
            span,
        )
    }

    #[test]
    fn decoding_uses_native_kinds_and_never_sniffs_unknown_content() {
        for (input, expected) in [
            (b"null".as_slice(), Value::Null),
            (b"42".as_slice(), Value::Integer(42)),
            (b"true".as_slice(), Value::Boolean(true)),
            (b"\"hello\"".as_slice(), Value::String("hello".into())),
            (b"[1]".as_slice(), Value::Array(vec![Value::Integer(1)])),
        ] {
            assert_eq!(
                decoded(
                    Some("Application/Vnd.Example+JSON; charset=\"UTF-8\""),
                    input
                )
                .unwrap(),
                expected
            );
        }
        assert!(matches!(
            decoded(Some("application/json"), b"{}").unwrap(),
            Value::Object(_)
        ));
        assert_eq!(
            decoded(Some("text/plain"), b"{}").unwrap(),
            Value::String("{}".into())
        );
        for media in [
            None,
            Some("application/octet-stream"),
            Some("application/x-unknown"),
        ] {
            assert_eq!(
                decoded(media, b"{}").unwrap(),
                Value::Bytes(Arc::from(b"{}".as_slice()))
            );
        }
    }

    #[test]
    fn malformed_declared_representations_fail_with_spans_and_bounds() {
        for (media, input) in [
            ("application/json", b"{".as_slice()),
            ("application/json", b"".as_slice()),
            ("text/plain", b"\xff".as_slice()),
            ("text/plain; charset=latin1", b"hello".as_slice()),
            ("text/plain; charset=\"unfinished", b"hello".as_slice()),
        ] {
            assert_eq!(
                decoded(Some(media), input).unwrap_err().span,
                Span::new(10, 20)
            );
        }
        let bytes = Value::Bytes(Arc::from(b"{}".as_slice()));
        let headers = HeaderMap::new();
        assert!(decode(&bytes, None, &headers, false, 1, Span::default()).is_err());
        let mut headers = HeaderMap::new();
        headers.append(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.append(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert!(
            representation(&headers, Span::default())
                .unwrap_err()
                .message
                .contains("duplicate")
        );
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        assert!(
            decode(&bytes, None, &headers, false, 10, Span::default())
                .unwrap_err()
                .message
                .contains("Content-Encoding")
        );
    }

    #[test]
    fn absent_content_and_empty_representations_are_distinct_and_sensitive() {
        let empty = Value::Bytes(Arc::from([]));
        let headers = HeaderMap::new();
        for status in [204, 205, 304] {
            assert!(bodyless(&Method::GET, status));
        }
        assert!(bodyless(&Method::HEAD, 200));
        assert!(!bodyless(&Method::GET, 200));
        let media = MediaType::parse("application/json", Span::default()).unwrap();
        assert_eq!(
            decode(
                &empty.clone().sensitive(),
                Some(&media),
                &headers,
                true,
                10,
                Span::default()
            )
            .unwrap(),
            Value::Null.sensitive()
        );
        assert_eq!(
            decode(&empty, Some(&media), &headers, true, 10, Span::default()).unwrap(),
            Value::Null
        );
        assert_eq!(
            decoded(Some("text/plain"), b"").unwrap(),
            Value::String(String::new())
        );
        assert_eq!(decoded(None, b"").unwrap(), empty);
        let bytes = Value::Bytes(Arc::from(b"42".as_slice())).sensitive();
        assert_eq!(
            decode(&bytes, Some(&media), &headers, false, 10, Span::default()).unwrap(),
            Value::Integer(42).sensitive()
        );
    }
}
