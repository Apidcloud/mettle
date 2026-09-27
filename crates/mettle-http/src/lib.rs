//! The built-in HTTP capability for Mettle.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Full};
use hyper::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HeaderName, HeaderValue};
use hyper::{Method, Request, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use mettle_capability::{
    Capability, CapabilityError, CapabilityFuture, IoContext, Object, OperationReport,
    ReportOutcome, ReportSection, Span, Value, merge_objects,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

mod incoming;
mod outgoing;
mod request_body;
mod response_stream;
mod schema;
#[cfg(test)]
mod streaming_tests;
use request_body::{RequestBody, UploadControl, UploadGuard};
pub use schema::DESCRIPTOR;

const DEFAULT_TIMEOUT: Duration = mettle_capability::DEFAULT_IO_TIMEOUT;
const DEFAULT_VERIFY_CERTIFICATES: bool = true;

type HttpClient = Client<HttpsConnector<HttpConnector>, RequestBody>;

#[derive(Clone)]
pub struct HttpCapability {
    secure_client: HttpClient,
    insecure_client: HttpClient,
}

impl fmt::Debug for HttpCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpCapability")
            .finish_non_exhaustive()
    }
}

impl HttpCapability {
    /// Build reusable secure and explicitly insecure connection pools.
    ///
    /// # Panics
    ///
    /// Panics only if rustls' built-in ring provider does not support its own
    /// safe default protocol versions, which indicates an invalid build.
    #[must_use]
    pub fn new() -> Self {
        let provider = rustls::crypto::ring::default_provider();
        let _ = provider.clone().install_default();

        let secure_connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        let secure_client = Client::builder(TokioExecutor::new())
            .retry_canceled_requests(false)
            .build(secure_connector);

        let insecure_tls = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .expect("ring supports rustls default protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
            .with_no_client_auth();
        let insecure_connector = HttpsConnectorBuilder::new()
            .with_tls_config(insecure_tls)
            .https_or_http()
            .enable_http1()
            .build();
        let insecure_client = Client::builder(TokioExecutor::new())
            .retry_canceled_requests(false)
            .build(insecure_connector);

        Self {
            secure_client,
            insecure_client,
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn execute(
        &self,
        operation: usize,
        arguments: Vec<Value>,
        options: Object,
        span: Span,
        context: &IoContext,
    ) -> Result<Value, CapabilityError> {
        let method = match operation {
            0 => Method::GET,
            1 => Method::POST,
            2 => Method::PUT,
            3 => Method::PATCH,
            4 => Method::DELETE,
            5 => Method::HEAD,
            6 => Method::OPTIONS,
            _ => {
                return Err(CapabilityError::new(
                    "HTTP execution plan references an unknown operation",
                    span,
                ));
            }
        };
        let url_sensitive = arguments.first().is_some_and(Value::contains_sensitive)
            || options
                .get("baseUrl")
                .is_some_and(Value::contains_sensitive);
        let path = expect_string(arguments.first(), "HTTP URL", span)?;
        let url = resolve_url(path, &options, span)?;
        let uri = url.parse::<Uri>().map_err(|error| {
            CapabilityError::new(format!("invalid HTTP URL `{url}`: {error}"), span)
        })?;
        if !uri.scheme_str().is_some_and(|scheme| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        }) {
            return Err(CapabilityError::new(
                "HTTP URL must use the `http` or `https` scheme",
                span,
            ));
        }

        let timeout = option_duration(&options, "timeout", DEFAULT_TIMEOUT, span)?;
        let started = Instant::now();
        let deadline = tokio::time::Instant::from_std(
            started
                .checked_add(timeout)
                .ok_or_else(|| CapabilityError::new("HTTP timeout is too large", span))?,
        );
        let max_response_bytes = option_usize(
            &options,
            "maxResponseBytes",
            usize::try_from(mettle_capability::DEFAULT_READ_BYTES).expect("default response bound"),
            span,
        )?;
        let stream_response = match options.get("stream").map(Value::revealed) {
            None => false,
            Some(Value::Boolean(value)) => *value,
            _ => return Err(CapabilityError::new("stream must be a boolean", span)),
        };
        let max_capture_bytes = option_usize(
            &options,
            "maxCaptureBytes",
            usize::try_from(mettle_capability::DEFAULT_READ_BYTES).expect("default capture bound"),
            span,
        )?;
        let verify_certificates = options
            .get("tls")
            .and_then(Value::as_object)
            .and_then(|tls| tls.get("verifyCertificates"))
            .map_or(Ok(DEFAULT_VERIFY_CERTIFICATES), |value| match value {
                Value::Boolean(value) => Ok(*value),
                other => Err(type_error("tls.verifyCertificates", "boolean", other, span)),
            })?;

        if options.contains_key("json") && options.contains_key("body") {
            return Err(CapabilityError::new(
                "HTTP request options `json` and `body` cannot be used together",
                span,
            ));
        }
        let prepared = outgoing::prepare(&options, span)?;
        // A server may echo or transform a sensitive request payload. Keep that
        // content protected in both complete and deferred response views.
        let payload_sensitive = options
            .get("body")
            .or_else(|| options.get("json"))
            .is_some_and(Value::contains_sensitive);
        let upload = Arc::new(UploadControl::default());
        let _upload_guard = UploadGuard(upload.clone());
        let mut request = Request::builder().method(method.clone()).uri(uri);
        if let Some(headers) = options.get("headers") {
            let headers = headers.as_object().ok_or_else(|| {
                type_error("headers", "object containing string values", headers, span)
            })?;
            for (name, value) in headers {
                let name = HeaderName::try_from(name.as_str())
                    .map_err(|_| CapabilityError::new("invalid HTTP header name", span))?;
                if name == CONTENT_TYPE {
                    continue;
                }
                let value = expect_string(Some(value), "HTTP header value", span)?;
                let value = HeaderValue::try_from(value)
                    .map_err(|_| CapabilityError::new("invalid HTTP header value", span))?;
                request = request.header(name, value);
            }
        }
        if let Some(content_type) = &prepared.content_type {
            let content_type = HeaderValue::try_from(content_type.as_str())
                .map_err(|_| CapabilityError::new("invalid Content-Type header", span))?;
            request = request.header(CONTENT_TYPE, content_type);
        }
        let stream = prepared.source;
        let body = prepared.bytes;
        let body = if let Some(source) = stream {
            let reader = tokio::time::timeout_at(deadline, source.open(context))
                .await
                .map_err(|_| {
                    CapabilityError::new("opening request source timed out", source.span)
                })??;
            RequestBody::source(reader, upload.clone(), prepared.max_bytes, source.span)
        } else {
            RequestBody::Full(Full::new(body))
        };
        let request = request.body(body).map_err(|error| {
            CapabilityError::new(format!("could not build HTTP request: {error}"), span)
        })?;

        let client = if verify_certificates {
            &self.secure_client
        } else {
            &self.insecure_client
        };
        let exchange = async {
            let response = client.request(request).await.map_err(|error| {
                upload
                    .error
                    .lock()
                    .expect("upload error lock")
                    .clone()
                    .unwrap_or_else(|| {
                        CapabilityError::new(
                            format!("HTTP request failed: {}", error_chain(&error)),
                            span,
                        )
                    })
            })?;

            upload.stop();
            if let Some(error) = upload.error.lock().expect("upload error lock").clone() {
                return Err(error);
            }

            let status = response.status().as_u16();
            let has_no_body = incoming::bodyless(&method, status);
            if !has_no_body
                && let Some(length) = response.headers().get(CONTENT_LENGTH)
                && let Ok(length) = length.to_str()
                && let Ok(length) = length.parse::<u64>()
                && length > max_response_bytes as u64
            {
                return Err(CapabilityError::new(
                    format!("HTTP response exceeded the {max_response_bytes} byte limit"),
                    span,
                ));
            }
            if !is_identity_content_encoding(response.headers().get(CONTENT_ENCODING)) {
                return Err(CapabilityError::new(
                    format!(
                        "HTTP response uses an unsupported Content-Encoding; only `identity` is supported"
                    ),
                    span,
                ));
            }
            let media = incoming::representation(response.headers(), span)?;
            let (parts, response_body) = response.into_parts();
            let headers = parts
                .headers
                .iter()
                .map(|(name, value)| {
                    let value =
                        Value::String(String::from_utf8_lossy(value.as_bytes()).into_owned());
                    let value = if sensitive_header(name.as_str()) {
                        value.sensitive()
                    } else {
                        value
                    };
                    (name.as_str().to_owned(), value)
                })
                .collect::<BTreeMap<_, _>>();
            if stream_response {
                let streamed = response_stream::ResponseStream::new(
                    response_body,
                    parts.headers,
                    response_stream::Settings {
                        media: media.clone(),
                        bodyless: has_no_body,
                        deadline,
                        max_transfer: max_response_bytes,
                        max_capture: max_capture_bytes.min(max_response_bytes),
                        sensitive: payload_sensitive,
                        span,
                    },
                    context,
                )?;
                let mut value = schema::ResponseValue {
                    body: streamed.field(response_stream::FieldKind::Body),
                    body_bytes: streamed.field(response_stream::FieldKind::Bytes),
                    duration: Value::Duration(started.elapsed()),
                    headers: Value::Object(headers),
                    media_type: media
                        .map_or(Value::Null, |media| Value::String(media.normalized())),
                    method: Value::String(method.to_string()),
                    status: Value::Integer(i64::from(status)),
                    url: if url_sensitive {
                        Value::String(url).sensitive()
                    } else {
                        Value::String(url)
                    },
                }
                .into_value();
                let Value::Object(fields) = &mut value else {
                    unreachable!()
                };
                let Value::Object(extra) = (schema::StreamFields {
                    chunks: streamed.chunks(context),
                    close: streamed.field(response_stream::FieldKind::Close),
                })
                .into_value() else {
                    unreachable!()
                };
                fields.extend(extra);
                return Ok(value);
            }
            let bytes = read_bounded_body(response_body, max_response_bytes, span).await?;
            let body_bytes = Value::Bytes(Arc::from(bytes));
            let body_bytes = if payload_sensitive {
                body_bytes.sensitive()
            } else {
                body_bytes
            };
            let decoded = incoming::decode(
                &body_bytes,
                media.as_ref(),
                &parts.headers,
                has_no_body,
                max_response_bytes,
                span,
            )?;

            Ok(schema::ResponseValue {
                body: decoded,
                body_bytes,
                duration: Value::Duration(started.elapsed()),
                headers: Value::Object(headers),
                media_type: media.map_or(Value::Null, |media| Value::String(media.normalized())),
                method: Value::String(method.to_string()),
                status: Value::Integer(i64::from(status)),
                url: if url_sensitive {
                    Value::String(url).sensitive()
                } else {
                    Value::String(url)
                },
            }
            .into_value())
        };

        tokio::time::timeout_at(deadline, exchange)
            .await
            .map_err(|_| {
                CapabilityError::new(
                    format!(
                        "HTTP request exceeded its {} ms timeout",
                        timeout.as_millis()
                    ),
                    span,
                )
            })?
    }
}

impl Default for HttpCapability {
    fn default() -> Self {
        Self::new()
    }
}

impl Capability for HttpCapability {
    fn name(&self) -> &'static str {
        DESCRIPTOR.name
    }

    fn operation_name(&self, operation: usize) -> &'static str {
        DESCRIPTOR
            .operations
            .get(operation)
            .map_or("request", |operation| operation.name)
    }

    fn merge_options(&self, defaults: &mut Object, mut overrides: Object) {
        if let Some(Value::Object(incoming_headers)) = overrides.remove("headers") {
            let current_headers = defaults
                .entry("headers".to_owned())
                .or_insert_with(|| Value::Object(Object::new()));
            if let Value::Object(current_headers) = current_headers {
                // Override previous layers case-insensitively, but retain distinct
                // spellings within this layer so duplicate Content-Type is rejected
                // by representation validation instead of silently picking one.
                current_headers.retain(|existing, _| {
                    !incoming_headers
                        .keys()
                        .any(|name| existing.eq_ignore_ascii_case(name))
                });
                for (name, value) in incoming_headers {
                    current_headers.insert(name, value);
                }
            }
        }
        merge_objects(defaults, overrides);
    }

    fn report(&self, _operation: usize, result: &Value) -> Option<OperationReport> {
        let fields = result.as_object()?;
        let method = expect_string(fields.get("method"), "method", Span::default()).ok()?;
        let url = fields.get("url")?.to_string();
        let Value::Integer(status) = fields.get("status")?.revealed() else {
            return None;
        };
        let header_fields = fields
            .get("headers")?
            .as_object()?
            .iter()
            .map(|(name, value)| (name.clone(), value.to_string()))
            .collect();
        let payload = fields
            .get("body")
            .filter(|value| !matches!(value.revealed(), Value::Deferred(_)))
            .cloned();
        let mut sections = vec![ReportSection::Fields {
            title: "Headers".to_owned(),
            fields: header_fields,
        }];
        if let Some(value) = &payload {
            sections.push(ReportSection::Value {
                title: "Body".to_owned(),
                value: value.clone(),
            });
        }
        Some(OperationReport {
            summary: format!("{method:<6} {url}"),
            outcome: if fields.contains_key("chunks") {
                format!("{status} (headers received; body not captured)")
            } else {
                status.to_string()
            },
            outcome_kind: match status {
                200..=399 => ReportOutcome::Success,
                400..=499 => ReportOutcome::Warning,
                _ => ReportOutcome::Failure,
            },
            sections,
            payload,
        })
    }

    fn invoke(
        &self,
        operation: usize,
        arguments: Vec<Value>,
        options: Object,
        span: Span,
    ) -> CapabilityFuture<'_> {
        Box::pin(async move {
            if options
                .get("stream")
                .is_some_and(|value| matches!(value.revealed(), Value::Boolean(true)))
            {
                return Err(CapabilityError::new(
                    "streamed HTTP responses require an execution-owned I/O context",
                    span,
                ));
            }
            let context = IoContext::new(std::env::current_dir().map_err(|error| {
                CapabilityError::new(
                    format!("could not determine execution directory: {error}"),
                    span,
                )
            })?);
            let result = self
                .execute(operation, arguments, options, span, &context)
                .await;
            context.cleanup().await?;
            result
        })
    }

    fn invoke_with_context<'a>(
        &'a self,
        operation: usize,
        arguments: Vec<Value>,
        options: Object,
        span: Span,
        context: &'a IoContext,
    ) -> CapabilityFuture<'a> {
        Box::pin(self.execute(operation, arguments, options, span, context))
    }
}

async fn read_bounded_body(
    mut body: hyper::body::Incoming,
    limit: usize,
    span: Span,
) -> Result<Vec<u8>, CapabilityError> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| {
            CapabilityError::new(format!("failed while reading HTTP response: {error}"), span)
        })?;
        if let Ok(data) = frame.into_data() {
            if bytes.len().saturating_add(data.len()) > limit {
                return Err(CapabilityError::new(
                    format!("HTTP response exceeded the {limit} byte limit"),
                    span,
                ));
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes)
}

fn resolve_url(path: &str, options: &Object, span: Span) -> Result<String, CapabilityError> {
    if has_uri_scheme(path) {
        return Ok(path.to_owned());
    }
    let base = options.get("baseUrl").ok_or_else(|| {
        CapabilityError::new(
            "relative HTTP URL requires `baseUrl` in HTTP defaults or operation options",
            span,
        )
    })?;
    let base = expect_string(Some(base), "baseUrl", span)?;
    Ok(format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    ))
}

fn has_uri_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

fn option_duration(
    options: &Object,
    name: &str,
    default: Duration,
    span: Span,
) -> Result<Duration, CapabilityError> {
    options
        .get(name)
        .map_or(Ok(default), |value| match value.revealed() {
            Value::Duration(value) => Ok(*value),
            other => Err(type_error(name, "duration", other, span)),
        })
}

fn option_usize(
    options: &Object,
    name: &str,
    default: usize,
    span: Span,
) -> Result<usize, CapabilityError> {
    options
        .get(name)
        .map_or(Ok(default), |value| match value.revealed() {
            Value::Integer(value) if *value > 0 => usize::try_from(*value).map_err(|_| {
                CapabilityError::new(format!("`{name}` is too large for this platform"), span)
            }),
            Value::Integer(_) => Err(CapabilityError::new(
                format!("`{name}` must be greater than zero"),
                span,
            )),
            other => Err(type_error(name, "positive integer", other, span)),
        })
}

fn expect_string<'a>(
    value: Option<&'a Value>,
    name: &str,
    span: Span,
) -> Result<&'a str, CapabilityError> {
    match value.map(Value::revealed) {
        Some(Value::String(value)) => Ok(value),
        Some(other) => Err(type_error(name, "string", other, span)),
        None => Err(CapabilityError::new(format!("missing {name}"), span)),
    }
}

fn type_error(name: &str, expected: &str, value: &Value, span: Span) -> CapabilityError {
    CapabilityError::new(
        format!("`{name}` must be {expected}, found {}", value.type_name()),
        span,
    )
}

fn is_identity_content_encoding(value: Option<&HeaderValue>) -> bool {
    value.is_none_or(|value| {
        value.to_str().is_ok_and(|value| {
            value
                .split(',')
                .all(|encoding| encoding.trim().eq_ignore_ascii_case("identity"))
        })
    })
}

fn error_chain(error: &dyn Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    message
}

fn sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie" | "x-api-key" | "api-key"
    ) || name.to_ascii_lowercase().contains("token")
}

#[derive(Debug)]
struct NoCertificateVerification;

impl ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use mettle_capability::{Capability, Object, Span, Value};

    use super::{HttpCapability, is_identity_content_encoding, resolve_url};
    use hyper::header::HeaderValue;
    use mettle_capability::content::BuiltinCodec;
    use mettle_capability::content::from_json;

    #[test]
    fn resolves_relative_urls_without_double_slashes() {
        let options = BTreeMap::from([(
            "baseUrl".to_owned(),
            Value::String("http://localhost:8000/".to_owned()),
        )]);
        assert_eq!(
            resolve_url("/users", &options, Span::default()).expect("URL should resolve"),
            "http://localhost:8000/users"
        );
    }

    #[test]
    fn content_encoding_verification() {
        assert!(is_identity_content_encoding(None));
        assert!(is_identity_content_encoding(Some(
            &HeaderValue::from_static("identity")
        )));
        assert!(is_identity_content_encoding(Some(
            &HeaderValue::from_static("Identity")
        )));
        assert!(!is_identity_content_encoding(Some(
            &HeaderValue::from_static("gzip")
        )));
        assert!(!is_identity_content_encoding(Some(
            &HeaderValue::from_static("br")
        )));
        assert!(!is_identity_content_encoding(Some(
            &HeaderValue::from_static("identity, gzip")
        )));
        assert!(is_identity_content_encoding(Some(
            &HeaderValue::from_static("identity, identity")
        )));
    }

    #[test]
    fn json_conversion_round_trips_structured_values() {
        let value = Value::Object(BTreeMap::from([
            ("active".to_owned(), Value::Boolean(true)),
            ("count".to_owned(), Value::Integer(2)),
        ]));
        let encoded = BuiltinCodec::Json
            .encode(&value, 1024, Span::default())
            .expect("value should encode");
        assert_eq!(
            BuiltinCodec::Json
                .decode(&encoded, 1024, Span::default())
                .expect("value should decode"),
            value
        );
    }

    #[test]
    fn rejects_oversized_json_integers_without_rounding() {
        let value =
            serde_json::from_str("18446744073709551615").expect("JSON integer should parse");
        let error = from_json(value, Span::default()).expect_err("integer should exceed range");
        assert!(error.message.contains("64-bit range"));
    }

    #[test]
    fn header_overrides_are_case_insensitive() {
        let capability = HttpCapability::new();
        let mut defaults = Object::from([(
            "headers".to_owned(),
            Value::Object(Object::from([
                (
                    "Accept".to_owned(),
                    Value::String("application/json".to_owned()),
                ),
                (
                    "Authorization".to_owned(),
                    Value::String("Bearer old".to_owned()),
                ),
            ])),
        )]);
        let overrides = Object::from([(
            "headers".to_owned(),
            Value::Object(Object::from([(
                "authorization".to_owned(),
                Value::String("Bearer new".to_owned()),
            )])),
        )]);

        capability.merge_options(&mut defaults, overrides);

        let headers = defaults
            .get("headers")
            .and_then(Value::as_object)
            .expect("merged headers should be an object");
        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers.get("Accept"),
            Some(&Value::String("application/json".to_owned()))
        );
        assert_eq!(
            headers.get("authorization"),
            Some(&Value::String("Bearer new".to_owned()))
        );
        assert!(!headers.contains_key("Authorization"));
    }
}
