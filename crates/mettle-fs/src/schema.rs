use mettle_capability::documentation::{DefaultValue, OperationDocumentation};

use mettle_capability::{CapabilityDescriptor, FieldSchema, OperationSchema, SchemaType};

const PATH_DESCRIPTION: &str =
    "File path relative to the execution working directory, or an absolute path.";

const DETAILS: &str = "Standalone paths start at the CLI invocation directory; projects use their root or workingDir. Only regular-file sources are supported. Sources share consumption state across aliases, cannot escape as final execution results, and are never implicitly collected. Writes publish a temporary sibling only after completion; failure before publication retains the old destination. Parent directories must exist. Publication is not a durability guarantee.";

const OPTIONS: &[FieldSchema] = &[
    FieldSchema::new("maxBytes", SchemaType::Integer)
        .documented(
            "Positive byte bound; checked during production, not only against initial metadata.",
        )
        .with_default(DefaultValue::Bytes(mettle_capability::DEFAULT_READ_BYTES)),
    FieldSchema::new("timeout", SchemaType::Duration)
        .documented("Positive deadline for consuming or producing file contents.")
        .with_default(DefaultValue::Duration(
            mettle_capability::DEFAULT_IO_TIMEOUT,
        )),
];
const TRANSFER_OPTIONS: &[FieldSchema] = &[
    OPTIONS[0].with_default(DefaultValue::Bytes(
        mettle_capability::DEFAULT_TRANSFER_BYTES,
    )),
    OPTIONS[1],
];

const WRITE_OPTIONS: &[FieldSchema] = &[
    TRANSFER_OPTIONS[0],
    TRANSFER_OPTIONS[1],
    FieldSchema::new("overwrite", SchemaType::Boolean)
        .documented(
            "Set `overwrite: true` to permit replacing an existing destination; otherwise publication refuses it.",
        )
        .with_default(DefaultValue::Boolean(crate::DEFAULT_OVERWRITE)),
];
mettle_capability::result_object! {
    pub(crate) struct WriteResult {
        bytes_written => ("bytesWritten", SchemaType::Integer, "Number of representation bytes published to the destination."),
    }
}
const OPERATIONS: &[OperationSchema] = &[
    OperationSchema {
        name: "read",
        documentation: OperationDocumentation {
            summary: "Read a complete file into reusable bytes. The whole result is held in memory; use fs.stream for incremental transfers.",
            notes: &[],
            parameters: &[PATH_DESCRIPTION],
            details: DETAILS,
            example: "fs.read(\"input.txt\")",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["path"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
    OperationSchema {
        name: "readText",
        documentation: OperationDocumentation {
            summary: "Read a complete file into a UTF-8 string. Invalid UTF-8 fails rather than inserting replacement characters; use fs.read for binary content.",
            notes: &[],
            parameters: &[PATH_DESCRIPTION],
            details: DETAILS,
            example: "fs.readText(\"input.txt\")",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["path"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::String,
    },
    OperationSchema {
        name: "stream",
        documentation: OperationDocumentation {
            summary: "Create a lazy byte source: the file is opened when consumed, not when this call returns. One source can be consumed only once, even through an alias.",
            notes: &[
                "Pass the source as `body` to `http.post`, or as `content` to `fs.write`. It is never implicitly buffered as a complete value or replayed.",
            ],
            parameters: &[PATH_DESCRIPTION],
            details: DETAILS,
            example: "fs.stream(\"input.txt\")",
        },
        parameters: &[SchemaType::String],
        parameter_names: &["path"],
        options: TRANSFER_OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Source,
    },
    OperationSchema {
        name: "write",
        documentation: OperationDocumentation {
            summary: "Write a string, bytes, or a source to a file and return bytesWritten. The destination is published only after the complete write succeeds.",
            notes: &[WRITE_OPTIONS[2].description],
            parameters: &[
                PATH_DESCRIPTION,
                "Complete UTF-8 string, bytes, or single-consumer byte source to write.",
            ],
            details: DETAILS,
            example: "fs.write(\"copy.txt\", fs.stream(\"input.txt\"))",
        },
        parameters: &[SchemaType::String, SchemaType::Writable],
        parameter_names: &["path", "content"],
        options: WRITE_OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Object(WriteResult::FIELDS),
    },
];

pub const DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    removed_result_fields: &[],
    name: "fs",
    description: "Bounded complete file I/O and owned file sources, with deliberate working-directory and publication rules.",
    constants: &[],
    defaults: OPTIONS,
    operations: OPERATIONS,
};
