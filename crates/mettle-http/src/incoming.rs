//! Bounded response representation selection and native-value decoding.

use flate2::read::GzDecoder;
use hyper::header::{CONTENT_ENCODING, CONTENT_TYPE};
use hyper::{HeaderMap, Method};
use mettle_capability::content::BuiltinCodec;
use mettle_capability::media_type::MediaType;
use mettle_capability::{CapabilityError, Span, Value};
use std::io::{self, Read};
use std::sync::Arc;

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

    let mut encodings = Vec::new();
    for encoding in headers.get_all(CONTENT_ENCODING) {
        let encoding = encoding
            .to_str()
            .map_err(|_| CapabilityError::new("invalid HTTP response Content-Encoding", span))?;
        for part in encoding.split(',') {
            let literal = part.trim().to_ascii_lowercase();
            if literal.is_empty() {
                continue;
            }
            encodings.push(literal);
        }
    }

    let is_sensitive = bytes.contains_sensitive();
    let mut buf: Arc<[u8]> = match bytes.revealed() {
        Value::Bytes(b) => b.clone(),
        _ => {
            return Err(CapabilityError::new(
                "HTTP response needs to be bytes before decoding",
                span,
            ));
        }
    };

    for encoding in encodings.iter().rev() {
        match encoding.as_str() {
            "identity" => {
                // no-op
            }
            "gzip" | "x-gzip" => {
                buf = decode_gzip(&buf, max_bytes, span)?;
            }
            other => {
                return Err(CapabilityError::new(
                    format!("Unsupported HTTP response Content-Encoding: {other}"),
                    span,
                ));
            }
        }
    }

    let bytes = if is_sensitive {
        Value::Bytes(buf).sensitive()
    } else {
        Value::Bytes(buf)
    };
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
        .decode(&bytes, max_bytes, span)
        .map_err(|error| {
            CapabilityError::new(
                format!("HTTP response body could not be decoded: {}", error.message),
                span,
            )
        })
}

fn decode_gzip(
    bytes: &Arc<[u8]>,
    max_bytes: usize,
    span: Span,
) -> Result<Arc<[u8]>, CapabilityError> {
    let mut decoder = GzDecoder::new(&bytes[..]);
    let mut result = Vec::with_capacity(bytes.len().min(max_bytes));
    loop {
        if result.len() > max_bytes {
            return Err(CapabilityError::new(
                format!("HTTP gzip decoded response length exceeded max_bytes {max_bytes}"),
                span,
            ));
        }

        let mut chunk = [0u8; 8 * 1024];
        let n = decoder.read(&mut chunk).map_err(|e| {
            let kind = e.kind();
            let msg = match kind {
                io::ErrorKind::UnexpectedEof => "gzip stream ended unexpectedly",
                io::ErrorKind::InvalidData => "invalid gzip data",
                _ => "gzip decompression failed",
            };
            CapabilityError::new(format!("{msg}: {e}"), span)
        })?;

        if n == 0 {
            // EOF
            break;
        }

        if result.len() + n > max_bytes {
            return Err(CapabilityError::new(
                format!("HTTP gzip decoded response length exceeded max_bytes {max_bytes}"),
                span,
            ));
        }

        result.extend_from_slice(&chunk[..n]);
    }

    Ok(result.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use hyper::header::HeaderValue;
    use std::io::Write;
    use std::sync::Arc;

    fn gzip(input: &[u8]) -> Arc<[u8]> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(input).unwrap();
        encoder.finish().unwrap().into()
    }

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

    fn decoded_content_encoding(
        content_encoding: Option<&str>,
        input: &[u8],
    ) -> Result<Value, CapabilityError> {
        let mut headers = HeaderMap::new();
        if let Some(content_encoding) = content_encoding {
            headers.insert(
                CONTENT_ENCODING,
                HeaderValue::from_str(content_encoding).unwrap(),
            );
        }
        let span = Span::new(10, 20);
        decode(
            &Value::Bytes(Arc::from(input)),
            representation(&headers, span)?.as_ref(),
            &headers,
            false,
            1024,
            span,
        )
    }

    #[test]
    fn content_encoding_verification() {
        let hello = Value::Bytes(Arc::from(b"hello".as_slice()));
        let gzipped = gzip(b"hello");
        assert!(decoded_content_encoding(None, b"hello").is_ok());
        assert!(decoded_content_encoding(Some("identity"), b"hello").is_ok());
        assert!(decoded_content_encoding(Some("Identity"), b"hello").is_ok());
        assert!(decoded_content_encoding(Some("identity, identity"), b"hello").is_ok());
        assert_eq!(
            decoded_content_encoding(Some("gzip"), &gzipped).unwrap(),
            hello
        );
        assert_eq!(
            decoded_content_encoding(Some("identity, gzip"), &gzipped).unwrap(),
            hello
        );
        assert!(decoded_content_encoding(Some("gzip"), b"hello").is_err());
        assert!(decoded_content_encoding(Some("br"), b"hello").is_err());
        assert!(decoded_content_encoding(Some("gzip, br"), &gzipped).is_err());
    }

    #[test]
    fn bodyless_responses_ignore_content_encoding() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("br"));
        assert_eq!(
            decode(
                &Value::Bytes(Arc::from(b"".as_slice())),
                None,
                &headers,
                true,
                1024,
                Span::default(),
            )
            .unwrap(),
            Value::Null
        );
    }

    #[test]
    fn gzip_content_exceeds_max_bytes_limit() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        let input = gzip(br#"{"name":"Ada"}"#);
        let span = Span::new(10, 20);

        assert!(
            decode(
                &Value::Bytes(input),
                representation(&headers, span).unwrap().as_ref(),
                &headers,
                false,
                5,
                span,
            )
            .unwrap_err()
            .message
            .contains("decoded response length exceeded")
        );
    }

    #[test]
    fn gzip_content_is_decoded_before_its_media_type() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        let input = gzip(br#"{"name":"Ada"}"#);
        let span = Span::new(10, 20);

        assert_eq!(
            decode(
                &Value::Bytes(input),
                representation(&headers, span).unwrap().as_ref(),
                &headers,
                false,
                1024,
                span,
            )
            .unwrap(),
            Value::Object(
                [("name".into(), Value::String("Ada".into()))]
                    .into_iter()
                    .collect()
            )
        );
    }

    #[test]
    fn gzip_content_preserves_sensitivity_and_decoded_bounds() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("x-gzip"));
        let input = gzip(b"private response");
        let span = Span::new(10, 20);

        assert_eq!(
            decode(
                &Value::Bytes(input.clone()).sensitive(),
                representation(&headers, span).unwrap().as_ref(),
                &headers,
                false,
                1024,
                span,
            )
            .unwrap(),
            Value::String("private response".into()).sensitive()
        );
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
                .contains("gzip")
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
