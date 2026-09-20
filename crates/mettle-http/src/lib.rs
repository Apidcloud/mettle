//! The built-in HTTP capability for Mettle.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use hyper::{Method, Request, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use mettle_capability::{
    Capability, CapabilityDescriptor, CapabilityError, CapabilityFuture, FieldSchema, Object,
    OperationSchema, SchemaType, Span, Value, merge_objects,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

const TLS_FIELDS: &[FieldSchema] = &[FieldSchema {
    name: "verifyCertificates",
    value_type: SchemaType::Boolean,
}];

const COMMON_OPTIONS: &[FieldSchema] = &[
    FieldSchema {
        name: "baseUrl",
        value_type: SchemaType::String,
    },
    FieldSchema {
        name: "timeout",
        value_type: SchemaType::Duration,
    },
    FieldSchema {
        name: "headers",
        value_type: SchemaType::StringMap,
    },
    FieldSchema {
        name: "tls",
        value_type: SchemaType::Object(TLS_FIELDS),
    },
    FieldSchema {
        name: "maxResponseBytes",
        value_type: SchemaType::Integer,
    },
];

const GET_OPTIONS: &[FieldSchema] = COMMON_OPTIONS;

const POST_OPTIONS: &[FieldSchema] = &[
    FieldSchema {
        name: "baseUrl",
        value_type: SchemaType::String,
    },
    FieldSchema {
        name: "timeout",
        value_type: SchemaType::Duration,
    },
    FieldSchema {
        name: "headers",
        value_type: SchemaType::StringMap,
    },
    FieldSchema {
        name: "tls",
        value_type: SchemaType::Object(TLS_FIELDS),
    },
    FieldSchema {
        name: "maxResponseBytes",
        value_type: SchemaType::Integer,
    },
    FieldSchema {
        name: "json",
        value_type: SchemaType::Json,
    },
];

const OPERATIONS: &[OperationSchema] = &[
    OperationSchema {
        name: "get",
        parameters: &[SchemaType::String],
        options: GET_OPTIONS,
    },
    OperationSchema {
        name: "post",
        parameters: &[SchemaType::String],
        options: POST_OPTIONS,
    },
];

pub const DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "http",
    defaults: COMMON_OPTIONS,
    operations: OPERATIONS,
};

type HttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

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
        let (method, operation_name) = match operation {
            0 => (Method::GET, "get"),
            1 => (Method::POST, "post"),
            _ => {
                return Err(CapabilityError::new(
                    "HTTP execution plan references an unknown operation",
                    span,
                ));
            }
        };
        let path = expect_string(arguments.first(), "HTTP URL", span)?;
        let url = resolve_url(path, &options, span)?;
        let uri = url.parse::<Uri>().map_err(|error| {
            CapabilityError::new(format!("invalid HTTP URL `{url}`: {error}"), span)
        })?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) {
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

        let body = if operation_name == "post" {
            if let Some(json) = options.get("json") {
                Bytes::from(serde_json::to_vec(&to_json(json, span)?).map_err(|error| {
                    CapabilityError::new(format!("could not encode JSON request: {error}"), span)
                })?)
            } else {
                Bytes::new()
            }
        } else {
            Bytes::new()
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
        if operation_name == "post" && options.contains_key("json") {
            let has_content_type = options
                .get("headers")
                .and_then(Value::as_object)
                .is_some_and(|headers| {
                    headers
                        .keys()
                        .any(|name| name.eq_ignore_ascii_case("content-type"))
                });
            if !has_content_type {
                request = request.header(CONTENT_TYPE, "application/json");
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
        let exchange = async {
            let response = client.request(request).await.map_err(|error| {
                CapabilityError::new(
                    format!("HTTP request failed: {}", error_chain(&error)),
                    span,
                )
            })?;

            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        Value::String(String::from_utf8_lossy(value.as_bytes()).into_owned()),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let bytes = read_bounded_body(response.into_body(), max_response_bytes, span).await?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let json = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .map_or(Value::Null, from_json);

            Ok(Value::Object(BTreeMap::from([
                ("body".to_owned(), Value::String(text)),
                ("bodyBytes".to_owned(), Value::Bytes(Arc::from(bytes))),
                ("headers".to_owned(), Value::Object(headers)),
                ("json".to_owned(), json),
                ("method".to_owned(), Value::String(method.to_string())),
                ("status".to_owned(), Value::Integer(i64::from(status))),
                ("url".to_owned(), Value::String(url)),
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
    if path.starts_with("http://") || path.starts_with("https://") {
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

fn option_duration(
    options: &Object,
    name: &str,
    default: Duration,
    span: Span,
) -> Result<Duration, CapabilityError> {
    options.get(name).map_or(Ok(default), |value| match value {
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
    options.get(name).map_or(Ok(default), |value| match value {
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
    match value {
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
    match value {
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
    }
}

fn from_json(value: serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Boolean(value),
        serde_json::Value::Number(value) => value.as_i64().map_or_else(
            || Value::Float(value.as_f64().expect("JSON number is representable as f64")),
            Value::Integer,
        ),
        serde_json::Value::String(value) => Value::String(value),
        serde_json::Value::Array(values) => {
            Value::Array(values.into_iter().map(from_json).collect())
        }
        serde_json::Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(name, value)| (name, from_json(value)))
                .collect(),
        ),
    }
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

    use super::{HttpCapability, from_json, resolve_url, to_json};

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
        assert_eq!(from_json(encoded), value);
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
