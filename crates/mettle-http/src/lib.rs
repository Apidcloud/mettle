//! The built-in HTTP capability for Mettle.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderName, HeaderValue};
use hyper::{Method, Request, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use mettle_capability::{
    Capability, CapabilityError, CapabilityFuture, Object, OperationReport, ReportOutcome,
    ReportSection, Span, Value, merge_objects,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

mod schema;
pub use schema::DESCRIPTOR;

type HttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

#[derive(Clone, Copy, Eq, PartialEq)]
enum BodyFormat {
    Json,
    Text,
    Bytes,
}

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
        let secure_client = Client::builder(TokioExecutor::new()).build(secure_connector);

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
        let insecure_client = Client::builder(TokioExecutor::new()).build(insecure_connector);

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

        let timeout = option_duration(&options, "timeout", Duration::from_secs(30), span)?;
        let max_response_bytes =
            option_usize(&options, "maxResponseBytes", 10 * 1024 * 1024, span)?;
        let verify_certificates = options
            .get("tls")
            .and_then(Value::as_object)
            .and_then(|tls| tls.get("verifyCertificates"))
            .map_or(Ok(true), |value| match value {
                Value::Boolean(value) => Ok(*value),
                other => Err(type_error("tls.verifyCertificates", "boolean", other, span)),
            })?;

        if options.contains_key("json") && options.contains_key("body") {
            return Err(CapabilityError::new(
                "HTTP request options `json` and `body` cannot be used together",
                span,
            ));
        }
        if options.contains_key("json") && options.contains_key("bodyFormat") {
            return Err(CapabilityError::new(
                "legacy `json` cannot be combined with `bodyFormat`",
                span,
            ));
        }
        let explicit_format = options
            .get("bodyFormat")
            .map(|value| expect_string(Some(value), "bodyFormat", span))
            .transpose()?;
        if explicit_format.is_some() && !options.contains_key("body") {
            return Err(CapabilityError::new("`bodyFormat` requires `body`", span));
        }
        let payload = options.get("body").or_else(|| options.get("json"));
        let body_format = if options.contains_key("json") {
            Some(BodyFormat::Json)
        } else if let Some(payload) = payload {
            Some(match explicit_format {
                Some("json") => BodyFormat::Json,
                Some("text") => BodyFormat::Text,
                Some("bytes") => BodyFormat::Bytes,
                Some(other) => {
                    return Err(CapabilityError::new(
                        format!("unknown body format `{other}`; use `json`, `text`, or `bytes`"),
                        span,
                    ));
                }
                None => match payload.revealed() {
                    Value::Object(_) | Value::Array(_) => BodyFormat::Json,
                    Value::String(_) => BodyFormat::Text,
                    Value::Bytes(_) => BodyFormat::Bytes,
                    _ => {
                        return Err(CapabilityError::new(
                            "this body value requires an explicit `bodyFormat`",
                            span,
                        ));
                    }
                },
            })
        } else {
            None
        };
        let body = match (body_format, payload) {
            (Some(BodyFormat::Json), Some(value)) => {
                Bytes::from(serde_json::to_vec(&to_json(value, span)?).map_err(|error| {
                    CapabilityError::new(format!("could not encode JSON request: {error}"), span)
                })?)
            }
            (Some(BodyFormat::Text), Some(value)) => {
                Bytes::copy_from_slice(expect_string(Some(value), "body", span)?.as_bytes())
            }
            (Some(BodyFormat::Bytes), Some(value)) => match value.revealed() {
                Value::Bytes(bytes) => Bytes::copy_from_slice(bytes),
                other => return Err(type_error("body", "bytes", other, span)),
            },
            _ => Bytes::new(),
        };

        let mut request = Request::builder().method(method.clone()).uri(uri);
        if let Some(headers) = options.get("headers") {
            let headers = headers.as_object().ok_or_else(|| {
                type_error("headers", "object containing string values", headers, span)
            })?;
            for (name, value) in headers {
                let name = HeaderName::try_from(name.as_str()).map_err(|error| {
                    CapabilityError::new(
                        format!("invalid HTTP header name `{name}`: {error}"),
                        span,
                    )
                })?;
                let value = expect_string(Some(value), "HTTP header value", span)?;
                let value = HeaderValue::try_from(value).map_err(|error| {
                    CapabilityError::new(format!("invalid HTTP header value: {error}"), span)
                })?;
                request = request.header(name, value);
            }
        }
        let content_type = options
            .get("headers")
            .and_then(Value::as_object)
            .and_then(|headers| {
                headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type").then_some(value)
                })
            });
        if body_format == Some(BodyFormat::Json) {
            if let Some(content_type) = content_type {
                let content_type = expect_string(Some(content_type), "Content-Type header", span)?;
                if !is_json_content_type(content_type) {
                    return Err(CapabilityError::new(
                        format!(
                            "JSON request body requires a JSON Content-Type, found `{content_type}`"
                        ),
                        span,
                    ));
                }
            } else {
                request = request.header(CONTENT_TYPE, "application/json");
            }
        } else if content_type.is_none() {
            if body_format == Some(BodyFormat::Text) {
                request = request.header(CONTENT_TYPE, "text/plain; charset=utf-8");
            } else if body_format == Some(BodyFormat::Bytes) {
                request = request.header(CONTENT_TYPE, "application/octet-stream");
            }
        }
        let request = request.body(Full::new(body)).map_err(|error| {
            CapabilityError::new(format!("could not build HTTP request: {error}"), span)
        })?;

        let client = if verify_certificates {
            &self.secure_client
        } else {
            &self.insecure_client
        };
        let started = Instant::now();
        let exchange = async {
            let response = client.request(request).await.map_err(|error| {
                CapabilityError::new(
                    format!("HTTP request failed: {}", error_chain(&error)),
                    span,
                )
            })?;

            let status = response.status().as_u16();
            if method != Method::HEAD
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
            let declares_json = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(is_json_content_type);
            let headers = response
                .headers()
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
            let bytes = read_bounded_body(response.into_body(), max_response_bytes, span).await?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let json = match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(value) => match from_json(value, span) {
                    Ok(value) => value,
                    Err(_) if !declares_json => Value::Null,
                    Err(error) => return Err(error),
                },
                Err(_) if bytes.is_empty() || !declares_json => Value::Null,
                Err(error) => {
                    return Err(CapabilityError::new(
                        format!(
                            "HTTP response declared JSON but its body could not be decoded: {error}"
                        ),
                        span,
                    ));
                }
            };

            Ok(Value::Object(BTreeMap::from([
                ("body".to_owned(), Value::String(text)),
                ("bodyBytes".to_owned(), Value::Bytes(Arc::from(bytes))),
                ("duration".to_owned(), Value::Duration(started.elapsed())),
                ("headers".to_owned(), Value::Object(headers)),
                ("json".to_owned(), json),
                ("method".to_owned(), Value::String(method.to_string())),
                ("status".to_owned(), Value::Integer(i64::from(status))),
                (
                    "url".to_owned(),
                    if url_sensitive {
                        Value::String(url).sensitive()
                    } else {
                        Value::String(url)
                    },
                ),
            ])))
        };

        tokio::time::timeout(timeout, exchange).await.map_err(|_| {
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
                for (name, value) in incoming_headers {
                    if let Some(existing) = current_headers
                        .keys()
                        .find(|existing| existing.eq_ignore_ascii_case(&name))
                        .cloned()
                    {
                        current_headers.remove(&existing);
                    }
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
        let (payload_title, payload) = match fields.get("json") {
            Some(Value::Null) | None => ("Body", fields.get("body").cloned()),
            Some(json) => ("JSON body", Some(json.clone())),
        };
        let mut sections = vec![ReportSection::Fields {
            title: "Headers".to_owned(),
            fields: header_fields,
        }];
        if let Some(value) = &payload {
            sections.push(ReportSection::Value {
                title: payload_title.to_owned(),
                value: value.clone(),
            });
        }
        Some(OperationReport {
            summary: format!("{method:<6} {url}"),
            outcome: status.to_string(),
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
        Box::pin(self.execute(operation, arguments, options, span))
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

fn is_json_content_type(value: &str) -> bool {
    let media_type = value.split(';').next().unwrap_or(value).trim();
    media_type.eq_ignore_ascii_case("application/json")
        || media_type.to_ascii_lowercase().ends_with("+json")
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

fn to_json(value: &Value, span: Span) -> Result<serde_json::Value, CapabilityError> {
    match value.revealed() {
        Value::Null => Ok(serde_json::Value::Null),
        Value::Boolean(value) => Ok(serde_json::Value::Bool(*value)),
        Value::Integer(value) => Ok(serde_json::Value::Number((*value).into())),
        Value::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| CapabilityError::new("JSON number must be finite", span)),
        Value::String(value) => Ok(serde_json::Value::String(value.clone())),
        Value::Bytes(_) => Err(CapabilityError::new(
            "byte values cannot be encoded as JSON",
            span,
        )),
        Value::Duration(_) => Err(CapabilityError::new(
            "duration values cannot be encoded as JSON",
            span,
        )),
        Value::Array(values) => values
            .iter()
            .map(|value| to_json(value, span))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        Value::Object(fields) => fields
            .iter()
            .map(|(name, value)| Ok((name.clone(), to_json(value, span)?)))
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(serde_json::Value::Object),
        Value::Sensitive(_) => unreachable!("revealed values are not sensitive wrappers"),
    }
}

fn sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie" | "x-api-key" | "api-key"
    ) || name.to_ascii_lowercase().contains("token")
}

fn from_json(value: serde_json::Value, span: Span) -> Result<Value, CapabilityError> {
    Ok(match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Boolean(value),
        serde_json::Value::Number(value) => {
            let spelling = value.to_string();
            if spelling.contains(['.', 'e', 'E']) {
                let decimal = value
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| {
                        CapabilityError::new(
                            "JSON decimal is outside Mettle's finite number range",
                            span,
                        )
                    })?;
                Value::Float(decimal)
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
                .collect::<Result<Vec<_>, _>>()?,
        ),
        serde_json::Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(name, value)| Ok((name, from_json(value, span)?)))
                .collect::<Result<BTreeMap<_, _>, CapabilityError>>()?,
        ),
    })
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

    use super::{HttpCapability, from_json, is_json_content_type, resolve_url, to_json};

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
    fn json_conversion_round_trips_structured_values() {
        let value = Value::Object(BTreeMap::from([
            ("active".to_owned(), Value::Boolean(true)),
            ("count".to_owned(), Value::Integer(2)),
        ]));
        let encoded = to_json(&value, Span::default()).expect("value should encode");
        assert_eq!(
            from_json(encoded, Span::default()).expect("value should decode"),
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

    #[test]
    fn recognizes_json_media_types() {
        assert!(is_json_content_type("application/json"));
        assert!(is_json_content_type("Application/JSON; charset=utf-8"));
        assert!(is_json_content_type("application/merge-patch+json"));
        assert!(!is_json_content_type("text/plain"));
    }
}
