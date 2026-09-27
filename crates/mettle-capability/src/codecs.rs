//! Built-in codec capabilities exposed through the same compiler interface as I/O.

use crate::content::BuiltinCodec;
use crate::{
    Capability, CapabilityConstant, CapabilityDescriptor, CapabilityError, CapabilityFuture,
    DEFAULT_READ_BYTES, FieldSchema, Object, OperationReport, OperationSchema, ReportOutcome,
    SchemaType, Span, Value,
};

const OPTIONS: &[FieldSchema] = &[FieldSchema {
    name: "maxBytes",
    value_type: SchemaType::Integer,
}];
const JSON_OPERATIONS: &[OperationSchema] = &[
    OperationSchema {
        name: "encode",
        parameters: &[SchemaType::Json],
        parameter_names: &["value"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
    OperationSchema {
        name: "decode",
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
        parameters: &[SchemaType::Json],
        parameter_names: &["value"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
    OperationSchema {
        name: "decode",
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
        parameters: &[SchemaType::Bytes],
        parameter_names: &["value"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
    OperationSchema {
        name: "decode",
        parameters: &[SchemaType::Bytes],
        parameter_names: &["content"],
        options: OPTIONS,
        mutually_exclusive: &[],
        result: SchemaType::Bytes,
    },
];

pub const JSON_DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "json",
    constants: &[CapabilityConstant {
        name: "mediaType",
        value: "application/json",
    }],
    defaults: OPTIONS,
    operations: JSON_OPERATIONS,
};
pub const TEXT_DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "text",
    constants: &[CapabilityConstant {
        name: "mediaType",
        value: "text/plain; charset=utf-8",
    }],
    defaults: OPTIONS,
    operations: TEXT_OPERATIONS,
};
pub const BYTES_DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "bytes",
    constants: &[CapabilityConstant {
        name: "mediaType",
        value: "application/octet-stream",
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
