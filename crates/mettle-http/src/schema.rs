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
const FIELD_RESPONSE_BODY: FieldSchema = FieldSchema::new("body", SchemaType::String).documented("Complete response body as text, using replacement characters for invalid UTF-8. Incoming decoded-body normalization is not implemented yet.");
const FIELD_RESPONSE_BODY_BYTES: FieldSchema = FieldSchema::new("bodyBytes", SchemaType::Bytes)
    .documented("Complete bounded response representation bytes.");
const FIELD_RESPONSE_DURATION: FieldSchema = FieldSchema::new("duration", SchemaType::Duration)
    .documented("Elapsed duration of the complete request.");
const FIELD_RESPONSE_HEADERS: FieldSchema = FieldSchema::new("headers", SchemaType::StringMap)
    .documented("Response headers; credential-bearing values retain sensitivity.");
const FIELD_RESPONSE_JSON: FieldSchema = FieldSchema::new("json", SchemaType::Json).documented(
    "Parsed JSON value, or null when unavailable. Malformed nonempty declared JSON fails.",
);
const FIELD_RESPONSE_METHOD: FieldSchema =
    FieldSchema::new("method", SchemaType::String).documented("HTTP method used for this request.");
const FIELD_RESPONSE_STATUS: FieldSchema = FieldSchema::new("status", SchemaType::Integer).documented("Numeric HTTP response status; error statuses are returned rather than thrown automatically.");
const FIELD_RESPONSE_URL: FieldSchema = FieldSchema::new("url", SchemaType::String)
    .documented("Resolved request URL; sensitive input remains redacted.");

const TLS_FIELDS: &[FieldSchema] = &[FIELD_VERIFY_CERTIFICATES];

const COMMON_OPTIONS: &[FieldSchema] = &[
    FIELD_BASE_URL,
    FIELD_TIMEOUT,
    FIELD_HEADERS,
    FIELD_TLS,
    FIELD_MAX_RESPONSE_BYTES,
];

const NO_BODY_OPTIONS: &[FieldSchema] = COMMON_OPTIONS;

const BODY_OPTIONS: &[FieldSchema] = &[
    FIELD_BASE_URL,
    FIELD_TIMEOUT,
    FIELD_HEADERS,
    FIELD_TLS,
    FIELD_MAX_RESPONSE_BYTES,
    FIELD_JSON,
    FIELD_BODY,
    FIELD_MEDIA_TYPE,
    FIELD_MAX_BODY_BYTES,
];

const BODY_CONFLICTS: &[&[&str]] = &[&["json", "body"]];
const NO_CONFLICTS: &[&[&str]] = &[];

const RESPONSE_FIELDS: &[FieldSchema] = &[
    FIELD_RESPONSE_BODY,
    FIELD_RESPONSE_BODY_BYTES,
    FIELD_RESPONSE_DURATION,
    FIELD_RESPONSE_HEADERS,
    FIELD_RESPONSE_JSON,
    FIELD_RESPONSE_METHOD,
    FIELD_RESPONSE_STATUS,
    FIELD_RESPONSE_URL,
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
    name: "http",
    description: "HTTP/1.1 client requests over HTTP or HTTPS with bounded bodies, shared connections, and TLS validation.",
    constants: &[],
    defaults: COMMON_OPTIONS,
    operations: OPERATIONS,
};
