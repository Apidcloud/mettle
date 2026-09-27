//! Bounded filesystem operations, using the execution's working directory.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mettle_capability::{
    ByteSource, Capability, CapabilityError, CapabilityFuture, DEFAULT_READ_BYTES,
    DEFAULT_TRANSFER_BYTES, IoContext, Object, OperationReport, ReportOutcome, Span, Value,
};

mod schema;
mod source;
#[cfg(test)]
mod tests;
mod write;
pub use schema::DESCRIPTOR;

#[derive(Debug, Default)]
pub struct FsCapability;

impl FsCapability {
    async fn execute(
        &self,
        operation: usize,
        arguments: Vec<Value>,
        options: Object,
        span: Span,
        context: &IoContext,
    ) -> Result<Value, CapabilityError> {
        let Some(Value::String(path)) = arguments.first().map(Value::revealed) else {
            return Err(CapabilityError::new(
                "filesystem path must be a string",
                span,
            ));
        };
        let path = context.resolve(Path::new(path));
        let sensitive = arguments.iter().any(Value::contains_sensitive);
        let max_bytes = match options.get("maxBytes").map(Value::revealed) {
            None => {
                if operation < 2 {
                    DEFAULT_READ_BYTES
                } else {
                    DEFAULT_TRANSFER_BYTES
                }
            }
            Some(Value::Integer(value)) if *value > 0 => {
                u64::try_from(*value).expect("positive integer")
            }
            _ => {
                return Err(CapabilityError::new(
                    "maxBytes must be a positive integer",
                    span,
                ));
            }
        };
        let timeout = match options.get("timeout").map(Value::revealed) {
            None => Duration::from_secs(30),
            Some(Value::Duration(value)) if !value.is_zero() => *value,
            _ => {
                return Err(CapabilityError::new(
                    "timeout must be a positive duration",
                    span,
                ));
            }
        };
        if operation == 2 {
            let source = source::FileSource {
                path,
                max_bytes,
                timeout,
                span,
                sensitive,
            };
            let value = Value::Source(Arc::new(ByteSource::new(Box::new(source), span, context)));
            return Ok(if sensitive { value.sensitive() } else { value });
        }
        let action = async {
            let value = match operation {
                0 | 1 => {
                    let source = ByteSource::new(
                        Box::new(source::FileSource {
                            path,
                            max_bytes,
                            timeout,
                            span,
                            sensitive,
                        }),
                        span,
                        context,
                    );
                    let mut reader = source.open(context).await?;
                    let mut bytes = Vec::new();
                    while let Some(chunk) = reader.next_chunk().await? {
                        bytes.extend_from_slice(&chunk);
                    }
                    if operation == 1 {
                        Value::String(
                            String::from_utf8(bytes).map_err(|_| {
                                CapabilityError::new("file is not valid UTF-8", span)
                            })?,
                        )
                    } else {
                        Value::Bytes(Arc::from(bytes))
                    }
                }
                3 => {
                    let payload = arguments
                        .get(1)
                        .ok_or_else(|| CapabilityError::new("fs.write requires content", span))?;
                    let overwrite = match options.get("overwrite").map(Value::revealed) {
                        None => false,
                        Some(Value::Boolean(value)) => *value,
                        _ => return Err(CapabilityError::new("overwrite must be boolean", span)),
                    };
                    write::write(
                        context, path, payload, overwrite, max_bytes, span, sensitive,
                    )
                    .await?
                }
                _ => return Err(CapabilityError::new("unknown filesystem operation", span)),
            };
            Ok(if sensitive { value.sensitive() } else { value })
        };
        tokio::time::timeout(timeout, action)
            .await
            .map_err(|_| CapabilityError::new("filesystem operation timed out", span))?
    }
}

pub(crate) fn file_error(
    path: &Path,
    sensitive: bool,
    action: &str,
    error: &std::io::Error,
    span: Span,
) -> CapabilityError {
    let path = if sensitive {
        "[REDACTED]".to_owned()
    } else {
        path.display().to_string()
    };
    CapabilityError::new(format!("could not {action} `{path}`: {error}"), span)
}

impl Capability for FsCapability {
    fn name(&self) -> &'static str {
        "fs"
    }
    fn operation_name(&self, operation: usize) -> &'static str {
        DESCRIPTOR
            .operations
            .get(operation)
            .map_or("unknown", |o| o.name)
    }
    fn observed_result(&self, operation: usize, result: &Value) -> Value {
        let snapshot = match (operation, result.revealed()) {
            (0, Value::Bytes(bytes)) => Value::Object(Object::from([(
                "bytesRead".into(),
                Value::Integer(i64::try_from(bytes.len()).unwrap_or(i64::MAX)),
            )])),
            (1, Value::String(text)) => Value::Object(Object::from([(
                "bytesRead".into(),
                Value::Integer(i64::try_from(text.len()).unwrap_or(i64::MAX)),
            )])),
            (2, _) => Value::String("lazy byte source".into()),
            _ => return result.clone(),
        };
        if result.contains_sensitive() {
            snapshot.sensitive()
        } else {
            snapshot
        }
    }

    fn report(&self, operation: usize, result: &Value) -> Option<OperationReport> {
        Some(OperationReport {
            summary: format!("fs.{}", self.operation_name(operation)),
            outcome: self.observed_result(operation, result).to_string(),
            outcome_kind: ReportOutcome::Success,
            sections: Vec::new(),
            payload: None,
        })
    }

    fn invoke(
        &self,
        _operation: usize,
        _arguments: Vec<Value>,
        _options: Object,
        span: Span,
    ) -> CapabilityFuture<'_> {
        Box::pin(async move {
            Err(CapabilityError::new(
                "filesystem operations require an execution context",
                span,
            ))
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
