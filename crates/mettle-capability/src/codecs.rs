//! Built-in codec capabilities exposed through the same compiler interface as I/O.

use crate::content::BuiltinCodec;
use crate::documentation::{DefaultValue, OperationDocumentation};

use crate::{
    Capability, CapabilityConstant, CapabilityDescriptor, CapabilityError, CapabilityFuture,
    DEFAULT_READ_BYTES, FieldSchema, Object, OperationReport, OperationSchema, ReportOutcome,
    SchemaType, Span, Value,
};

const DETAILS: &str = "Complete values are reusable and bounded. JSON/text processing rejects invalid JSON, UTF-8, numeric ranges, or nesting where applicable, with a source location. Sensitivity is preserved through encoding/decoding and reports do not capture payload contents.";
const MEDIA_TYPE_DESCRIPTION: &str = "The codec canonical media type as an ordinary string.";

const OPTIONS: &[FieldSchema] = &[FieldSchema::new("maxBytes", SchemaType::Integer)
    .documented("Positive byte bound on complete encoded output or input.")
    .with_default(DefaultValue::Bytes(crate::DEFAULT_READ_BYTES))];
const JSON_OPERATIONS: &[OperationSchema] = &[
    OperationSchema {
        name: "encode",
        documentation: OperationDocumentation {
            summary: "Encode an object, array, string, number, boolean, or null into reusable JSON bytes. Sending these bytes does not encode them a second time.",
            notes: &[],
            parameters: &[
                "Value to encode. Bytes, live sources, and durations require explicit conversion for JSON/text.",
            ],
            details: DETAILS,
            example: "json.encode({ ok: true })",
        },
        parameters: &[SchemaType::Json],
        parameter_names: &["value"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
    OperationSchema {
        name: "decode",
        documentation: OperationDocumentation {
            summary: "Parse a JSON string or complete bytes into a language value. Invalid JSON or unsupported numeric ranges fail; live sources are not consumed implicitly.",
            notes: &[],
            parameters: &[
                "Complete encoded bytes; JSON/text also accept strings. Live sources are never collected implicitly.",
            ],
            details: DETAILS,
            example: "json.decode(\"{\\\"ok\\\":true}\")",
        },
        parameters: &[SchemaType::Encoded],
        parameter_names: &["content"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Json,
    },
];
const TEXT_OPERATIONS: &[OperationSchema] = &[
    OperationSchema {
        name: "encode",
        documentation: OperationDocumentation {
            summary: "Encode a string as reusable UTF-8 bytes. Other JSON-compatible values use compact JSON spelling: objects are not converted with an arbitrary display format.",
            notes: &[],
            parameters: &[
                "Value to encode. Bytes, live sources, and durations require explicit conversion for JSON/text.",
            ],
            details: DETAILS,
            example: "text.encode(\"Hello!\")",
        },
        parameters: &[SchemaType::Json],
        parameter_names: &["value"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
    OperationSchema {
        name: "decode",
        documentation: OperationDocumentation {
            summary: "Decode complete bytes into a UTF-8 string, or validate an existing string. Invalid UTF-8 fails rather than inserting replacement characters.",
            notes: &[],
            parameters: &[
                "Complete encoded bytes; JSON/text also accept strings. Live sources are never collected implicitly.",
            ],
            details: DETAILS,
            example: "text.decode(text.encode(\"Hello!\"))",
        },
        parameters: &[SchemaType::Encoded],
        parameter_names: &["content"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::String,
    },
];
const BYTE_OPERATIONS: &[OperationSchema] = &[
    OperationSchema {
        name: "encode",
        documentation: OperationDocumentation {
            summary: "Return reusable bytes unchanged after checking the byte bound. This identity codec does not parse text, serialize objects, or consume live sources.",
            notes: &[],
            parameters: &[
                "Complete reusable bytes. No conversion or source collection is performed.",
            ],
            details: DETAILS,
            example: "bytes.encode(text.encode(\"Hello!\"))",
        },
        parameters: &[SchemaType::Bytes],
        parameter_names: &["value"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
    OperationSchema {
        name: "decode",
        documentation: OperationDocumentation {
            summary: "Return reusable bytes unchanged after checking the byte bound. This identity codec does not parse text, serialize objects, or consume live sources.",
            notes: &[],
            parameters: &[
                "Complete reusable bytes. No conversion or source collection is performed.",
            ],
            details: DETAILS,
            example: "bytes.decode(text.encode(\"Hello!\"))",
        },
        parameters: &[SchemaType::Bytes],
        parameter_names: &["content"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
];

pub const JSON_DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "json",
    description: "Protocol-independent JSON encoding and decoding for complete values.",
    constants: &[CapabilityConstant {
        name: "mediaType",
        description: MEDIA_TYPE_DESCRIPTION,
        value: BuiltinCodec::Json.media_type(),
    }],
    defaults: OPTIONS,
    operations: JSON_OPERATIONS,
};
pub const TEXT_DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "text",
    description: "Protocol-independent UTF-8 text encoding and strict decoding.",
    constants: &[CapabilityConstant {
        name: "mediaType",
        description: MEDIA_TYPE_DESCRIPTION,
        value: BuiltinCodec::Text.media_type(),
    }],
    defaults: OPTIONS,
    operations: TEXT_OPERATIONS,
};
pub const BYTES_DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "bytes",
    description: "Identity codec for complete representation bytes; no implicit conversion.",
    constants: &[CapabilityConstant {
        name: "mediaType",
        description: MEDIA_TYPE_DESCRIPTION,
        value: BuiltinCodec::Bytes.media_type(),
    }],
    defaults: OPTIONS,
    operations: BYTE_OPERATIONS,
};

#[derive(Debug)]
pub struct CodecCapability(pub BuiltinCodec);
impl Capability for CodecCapability {
    fn name(&self) -> &'static str {
        match self.0 {
            BuiltinCodec::Json => "json",
            BuiltinCodec::Text => "text",
            BuiltinCodec::Bytes => "bytes",
        }
    }
    fn operation_name(&self, operation: usize) -> &'static str {
        match operation {
            0 => "encode",
            1 => "decode",
            _ => "unknown",
        }
    }
    fn invoke(
        &self,
        operation: usize,
        arguments: Vec<Value>,
        options: Object,
        span: Span,
    ) -> CapabilityFuture<'_> {
        Box::pin(async move {
            let value = arguments
                .first()
                .ok_or_else(|| CapabilityError::new("codec requires an input value", span))?;
            let max_bytes = match options.get("maxBytes").map(Value::revealed) {
                None => usize::try_from(DEFAULT_READ_BYTES).expect("default content bound"),
                Some(Value::Integer(value)) if *value > 0 => usize::try_from(*value)
                    .map_err(|_| CapabilityError::new("maxBytes is too large", span))?,
                _ => {
                    return Err(CapabilityError::new(
                        "maxBytes must be a positive integer",
                        span,
                    ));
                }
            };
            match operation {
                0 => self.0.encode(value, max_bytes, span),
                1 => self.0.decode(value, max_bytes, span),
                _ => Err(CapabilityError::new("unknown codec operation", span)),
            }
        })
    }
    fn observed_result(&self, _operation: usize, result: &Value) -> Value {
        let value = Value::String(format!("{} value", result.type_name()));
        if result.contains_sensitive() {
            value.sensitive()
        } else {
            value
        }
    }
    fn report(&self, operation: usize, result: &Value) -> Option<OperationReport> {
        Some(OperationReport {
            summary: format!("{}.{}", self.name(), self.operation_name(operation)),
            outcome: self.observed_result(operation, result).to_string(),
            outcome_kind: ReportOutcome::Success,
            sections: Vec::new(),
            payload: None,
        })
    }
}
