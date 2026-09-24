//! HTTP capability operation and result schemas.

use mettle_capability::{CapabilityDescriptor, FieldSchema, OperationSchema, SchemaType};

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

const NO_BODY_OPTIONS: &[FieldSchema] = COMMON_OPTIONS;

const BODY_OPTIONS: &[FieldSchema] = &[
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
    FieldSchema {
        name: "body",
        value_type: SchemaType::String,
    },
];

const BODY_CONFLICTS: &[&[&str]] = &[&["json", "body"]];
const NO_CONFLICTS: &[&[&str]] = &[];

const RESPONSE_FIELDS: &[FieldSchema] = &[
    FieldSchema {
        name: "body",
        value_type: SchemaType::String,
    },
    FieldSchema {
        name: "bodyBytes",
        value_type: SchemaType::Bytes,
    },
    FieldSchema {
        name: "duration",
        value_type: SchemaType::Duration,
    },
    FieldSchema {
        name: "headers",
        value_type: SchemaType::StringMap,
    },
    FieldSchema {
        name: "json",
        value_type: SchemaType::Json,
    },
    FieldSchema {
        name: "method",
        value_type: SchemaType::String,
    },
    FieldSchema {
        name: "status",
        value_type: SchemaType::Integer,
    },
    FieldSchema {
        name: "url",
        value_type: SchemaType::String,
    },
];

const OPERATIONS: &[OperationSchema] = &[
    OperationSchema {
        name: "get",
        parameters: &[SchemaType::String],
        options: NO_BODY_OPTIONS,
        mutually_exclusive: NO_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "post",
        parameters: &[SchemaType::String],
        options: BODY_OPTIONS,
        mutually_exclusive: BODY_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "put",
        parameters: &[SchemaType::String],
        options: BODY_OPTIONS,
        mutually_exclusive: BODY_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "patch",
        parameters: &[SchemaType::String],
        options: BODY_OPTIONS,
        mutually_exclusive: BODY_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "delete",
        parameters: &[SchemaType::String],
        options: BODY_OPTIONS,
        mutually_exclusive: BODY_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "head",
        parameters: &[SchemaType::String],
        options: NO_BODY_OPTIONS,
        mutually_exclusive: NO_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
    OperationSchema {
        name: "options",
        parameters: &[SchemaType::String],
        options: NO_BODY_OPTIONS,
        mutually_exclusive: NO_CONFLICTS,
        result: SchemaType::Object(RESPONSE_FIELDS),
    },
];

pub const DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "http",
    defaults: COMMON_OPTIONS,
    operations: OPERATIONS,
};
