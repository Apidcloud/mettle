use mettle_capability::{CapabilityDescriptor, FieldSchema, OperationSchema, SchemaType};

const OPTIONS: &[FieldSchema] = &[
    FieldSchema {
        name: "maxBytes",
        value_type: SchemaType::Integer,
    },
    FieldSchema {
        name: "timeout",
        value_type: SchemaType::Duration,
    },
];
const WRITE_OPTIONS: &[FieldSchema] = &[
    FieldSchema {
        name: "maxBytes",
        value_type: SchemaType::Integer,
    },
    FieldSchema {
        name: "timeout",
        value_type: SchemaType::Duration,
    },
    FieldSchema {
        name: "overwrite",
        value_type: SchemaType::Boolean,
    },
];
const WRITE_RESULT: &[FieldSchema] = &[FieldSchema {
    name: "bytesWritten",
    value_type: SchemaType::Integer,
}];
const OPERATIONS: &[OperationSchema] = &[
    OperationSchema {
        name: "read",
        parameters: &[SchemaType::String],
        parameter_names: &["path"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
    OperationSchema {
        name: "readText",
        parameters: &[SchemaType::String],
        parameter_names: &["path"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::String,
    },
    OperationSchema {
        name: "stream",
        parameters: &[SchemaType::String],
        parameter_names: &["path"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Source,
    },
    OperationSchema {
        name: "write",
        parameters: &[SchemaType::String, SchemaType::Writable],
        parameter_names: &["path", "content"],
        options: WRITE_OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Object(WRITE_RESULT),
    },
];

pub const DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "fs",
    defaults: OPTIONS,
    operations: OPERATIONS,
};
