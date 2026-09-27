//! HTTP capability operation and result schemas.

use mettle_capability::documentation::{DefaultValue, OperationDocumentation};

use mettle_capability::{CapabilityDescriptor, FieldSchema, OperationSchema, SchemaType};

const URL_DESCRIPTION: &str = "Absolute HTTP/HTTPS URL, or a relative path using baseUrl.";

const FIELD_VERIFY_CERTIFICATES: FieldSchema =
    FieldSchema::new("verifyCertificates", SchemaType::Boolean)
        .documented("Verify server certificates and hostnames.")
        .with_default(DefaultValue::Boolean(crate::DEFAULT_VERIFY_CERTIFICATES));
const FIELD_BASE_URL: FieldSchema = FieldSchema::new("baseUrl", SchemaType::String)
    .documented("Base address for relative URLs; absolute URLs do not need it.");
const FIELD_TIMEOUT: FieldSchema = FieldSchema::new("timeout", SchemaType::Duration).documented("Positive deadline for opening the source, sending the request, and receiving the complete response.").with_default(DefaultValue::Duration(crate::DEFAULT_TIMEOUT));
const FIELD_HEADERS: FieldSchema = FieldSchema::new("headers", SchemaType::StringMap).documented("Request headers. Content-Type can select encoding; it must agree with mediaType when both are supplied.");
const FIELD_TLS: FieldSchema = FieldSchema::new("tls", SchemaType::Object(TLS_FIELDS)).documented("TLS certificate validation settings. Disabling validation is only suitable for controlled local fixtures.");
const FIELD_MAX_RESPONSE_BYTES: FieldSchema =
    FieldSchema::new("maxResponseBytes", SchemaType::Integer)
        .documented(
            "Positive response-body bound, enforced from Content-Length and during acquisition.",
        )
        .with_default(DefaultValue::Bytes(mettle_capability::DEFAULT_READ_BYTES));
const FIELD_STREAM: FieldSchema = FieldSchema::new("stream", SchemaType::Boolean).documented("Return at final response headers instead of buffering the body. response.chunks is a single-consumer byte source; reading body/bodyBytes acquires a bounded shared capture. Consume or close before entry exit.").with_default(DefaultValue::Boolean(false));
const FIELD_MAX_CAPTURE_BYTES: FieldSchema = FieldSchema::new("maxCaptureBytes", SchemaType::Integer).documented("Maximum complete capture through streamed response.body/bodyBytes, also capped by maxResponseBytes. Does not restrict streaming response.chunks.").with_default(DefaultValue::Bytes(mettle_capability::DEFAULT_READ_BYTES));
const FIELD_JSON: FieldSchema = FieldSchema::new("json", SchemaType::Json)
    .documented("Legacy JSON payload. Prefer body; json and body are mutually exclusive.");
const FIELD_BODY: FieldSchema = FieldSchema::new("body", SchemaType::Body).documented("Objects, arrays, numbers, booleans, and null are encoded as JSON; strings as UTF-8 text. Bytes and byte sources are sent unchanged. Use `mediaType` to select the encoding for ordinary values.");
const FIELD_MEDIA_TYPE: FieldSchema = FieldSchema::new("mediaType", SchemaType::String).documented("Optional representation selection, mapped to Content-Type. Codec mediaType constants and concrete strings both work. A name does not register a custom processor.");
const FIELD_MAX_BODY_BYTES: FieldSchema = FieldSchema::new("maxBodyBytes", SchemaType::Integer)
    .documented(
        "Positive total outgoing payload bound; streamed source bounds and deadlines also apply.",
    )
    .with_default(DefaultValue::PayloadBytes {
        buffered: mettle_capability::DEFAULT_READ_BYTES,
        streamed: mettle_capability::DEFAULT_TRANSFER_BYTES,
    });
mettle_capability::result_object! {
    pub(crate) struct ResponseValue {
        body => ("body", SchemaType::Value, "Decoded native response value. JSON/+json produces native values; text/* produces a UTF-8 string; missing/unknown content types produce bytes. HEAD/204/205/304 have null bodies. With stream: true, access acquires the complete bounded body and shares its capture with bodyBytes; cannot mix with chunks consumption. A media type does not prove application fields exist."),
        body_bytes => ("bodyBytes", SchemaType::Bytes, "Complete bounded response representation bytes. With stream: true, access acquires and caches the body; cannot mix with chunks consumption."),
        duration => ("duration", SchemaType::Duration, "Elapsed duration until the call returns: complete body normally, final response headers with stream: true."),
        headers => ("headers", SchemaType::StringMap, "Response headers; credential-bearing values retain sensitivity."),
        media_type => ("mediaType", SchemaType::NullableString, "Normalized Content-Type string, preserving explicit parameters, or null when absent. No content sniffing, inferred type, or implicit charset parameter is added. The original header remains in headers."),
        method => ("method", SchemaType::String, "HTTP method used for this request."),
        status => ("status", SchemaType::Integer, "Numeric HTTP response status; error statuses are returned rather than thrown automatically."),
        url => ("url", SchemaType::String, "Resolved request URL; sensitive input remains redacted."),
    }
}
mettle_capability::result_object! {
    pub(crate) struct StreamFields {
        chunks => ("chunks", SchemaType::Source, "Only present with stream: true. Single-consumer raw response bytes; aliases share the claim. Protocol/encoding processing is not applied. Consume with fs.write or explicitly capture body/bodyBytes instead."),
        close => ("close", SchemaType::Value, "Only present with stream: true. Call response.close() to abandon an unread or partial body. Idempotent; does not promise closure of the physical pooled connection. Entry exit also releases unread transfers."),
    }
}
const TLS_FIELDS: &[FieldSchema] = &[FIELD_VERIFY_CERTIFICATES];

const COMMON_OPTIONS: &[FieldSchema] = &[
    FIELD_BASE_URL,
    FIELD_TIMEOUT,
    FIELD_HEADERS,
    FIELD_TLS,
    FIELD_MAX_RESPONSE_BYTES,
    FIELD_STREAM,
    FIELD_MAX_CAPTURE_BYTES,
];

const NO_BODY_OPTIONS: &[FieldSchema] = COMMON_OPTIONS;

const BODY_OPTIONS: &[FieldSchema] = &[
    FIELD_BASE_URL,
    FIELD_TIMEOUT,
    FIELD_HEADERS,
    FIELD_TLS,
    FIELD_MAX_RESPONSE_BYTES,
    FIELD_STREAM,
    FIELD_MAX_CAPTURE_BYTES,
    FIELD_JSON,
    FIELD_BODY,
    FIELD_MEDIA_TYPE,
    FIELD_MAX_BODY_BYTES,
];

const BODY_CONFLICTS: &[&[&str]] = &[&["json", "body"]];
const NO_CONFLICTS: &[&[&str]] = &[];

const RESPONSE_FIELDS: &[FieldSchema] = &[
    ResponseValue::FIELDS[0],
    ResponseValue::FIELDS[1],
    ResponseValue::FIELDS[2],
    ResponseValue::FIELDS[3],
    ResponseValue::FIELDS[4],
    ResponseValue::FIELDS[5],
    ResponseValue::FIELDS[6],
    ResponseValue::FIELDS[7],
    StreamFields::FIELDS[0],
    StreamFields::FIELDS[1],
];

const RESPONSE_BEHAVIOR: &str = "HTTP error statuses are returned normally; use assertions to check success. Network, TLS, timeout, and body-limit errors fail the call.";
const RESPONSE_NOTES: &[&str] = &[RESPONSE_BEHAVIOR];
const PAYLOAD_NOTES: &[&str] = &[FIELD_BODY.description, RESPONSE_BEHAVIOR];

const OPERATIONS: &[OperationSchema] = &[
    OperationSchema {
        name: "get",
        documentation: OperationDocumentation {
            summary: "Fetch a resource with HTTP GET and return the complete response, including its status, headers, and body.",
            notes: RESPONSE_NOTES,
            parameters: &[URL_DESCRIPTION],
            details: include_str!("documentation.md"),
            example: "http.get(\"http://127.0.0.1:8080/method\")",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["url"],
        options: NO_BODY_OPTIONS,
        mutually_exclusive: NO_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "post",
        documentation: OperationDocumentation {
            summary: "Send an HTTP POST request and return the complete response, including its status, headers, and body.",
            notes: PAYLOAD_NOTES,
            parameters: &[URL_DESCRIPTION],
            details: include_str!("documentation.md"),
            example: "http.post(\"http://127.0.0.1:8080/method\", body: { name: \"Ana\" })",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["url"],
        options: BODY_OPTIONS,
        mutually_exclusive: BODY_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "put",
        documentation: OperationDocumentation {
            summary: "Send an HTTP PUT request, typically replacing a resource, and return the complete response.",
            notes: PAYLOAD_NOTES,
            parameters: &[URL_DESCRIPTION],
            details: include_str!("documentation.md"),
            example: "http.put(\"http://127.0.0.1:8080/method\", body: { name: \"Ana\" })",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["url"],
        options: BODY_OPTIONS,
        mutually_exclusive: BODY_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "patch",
        documentation: OperationDocumentation {
            summary: "Send an HTTP PATCH request, typically applying a partial update, and return the complete response.",
            notes: PAYLOAD_NOTES,
            parameters: &[URL_DESCRIPTION],
            details: include_str!("documentation.md"),
            example: "http.patch(\"http://127.0.0.1:8080/method\", body: { name: \"Ana\" })",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["url"],
        options: BODY_OPTIONS,
        mutually_exclusive: BODY_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "delete",
        documentation: OperationDocumentation {
            summary: "Send an HTTP DELETE request, optionally with a body, and return the complete response.",
            notes: PAYLOAD_NOTES,
            parameters: &[URL_DESCRIPTION],
            details: include_str!("documentation.md"),
            example: "http.delete(\"http://127.0.0.1:8080/method\", body: { name: \"Ana\" })",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["url"],
        options: BODY_OPTIONS,
        mutually_exclusive: BODY_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "head",
        documentation: OperationDocumentation {
            summary: "Fetch a resource’s response headers with HTTP HEAD without downloading its body.",
            notes: RESPONSE_NOTES,
            parameters: &[URL_DESCRIPTION],
            details: include_str!("documentation.md"),
            example: "http.head(\"http://127.0.0.1:8080/method\")",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["url"],
        options: NO_BODY_OPTIONS,
        mutually_exclusive: NO_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "options",
        documentation: OperationDocumentation {
            summary: "Ask a server which HTTP methods or request options it supports. Inspect the returned headers; allowed methods are not inferred automatically.",
            notes: RESPONSE_NOTES,
            parameters: &[URL_DESCRIPTION],
            details: include_str!("documentation.md"),
            example: "http.options(\"http://127.0.0.1:8080/method\")",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["url"],
        options: NO_BODY_OPTIONS,
        mutually_exclusive: NO_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
];

pub const DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    removed_result_fields: &[mettle_capability::RemovedResultField {
        name: "json",
        message: "HTTP response field `json` was removed; use `body` for decoded content, or `bodyBytes` for representation bytes",
    }],
    name: "http",
    description: "HTTP/1.1 client requests over HTTP or HTTPS with bounded bodies, shared connections, and TLS validation.",
    constants: &[],
    defaults: COMMON_OPTIONS,
    operations: OPERATIONS,
};
