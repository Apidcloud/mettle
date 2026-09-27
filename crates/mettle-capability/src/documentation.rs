//! Version-matched reference rendering from the same schemas used by compilation.

use std::fmt::Write as _;
use std::time::Duration;

use crate::{CapabilityDescriptor, FieldSchema, OperationSchema, SchemaType};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefaultValue {
    Boolean(bool),
    Bytes(u64),
    Duration(Duration),
    PayloadBytes { buffered: u64, streamed: u64 },
}

impl std::fmt::Display for DefaultValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Boolean(value) => write!(formatter, "{value}"),
            Self::Bytes(value) => write!(formatter, "{value} bytes"),
            Self::Duration(value) => write!(formatter, "{value:?}"),
            Self::PayloadBytes { buffered, streamed } => write!(
                formatter,
                "{buffered} bytes buffered; {streamed} bytes streamed"
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OperationDocumentation {
    pub summary: &'static str,
    pub notes: &'static [&'static str],
    pub parameters: &'static [&'static str],
    pub details: &'static str,
    pub example: &'static str,
}

impl OperationDocumentation {
    pub const EMPTY: Self = Self {
        summary: "",
        notes: &[],
        parameters: &[],
        details: "",
        example: "",
    };
}

#[must_use]
pub fn signature(capability: &CapabilityDescriptor, operation: &OperationSchema) -> String {
    format_signature(capability, operation, SignatureStyle::Inline)
}

#[derive(Clone, Copy)]
enum SignatureStyle {
    Inline,
    Expanded,
    Compact,
}

fn format_signature(
    capability: &CapabilityDescriptor,
    operation: &OperationSchema,
    style: SignatureStyle,
) -> String {
    let mut parameters = operation
        .parameter_names
        .iter()
        .zip(operation.parameters)
        .map(|(name, kind)| format!("{name}: {}", kind.name()))
        .chain(
            operation
                .options
                .iter()
                .filter(|_| !matches!(style, SignatureStyle::Compact))
                .map(|field| format!("{}?: {}", field.name, field.value_type.name())),
        )
        .collect::<Vec<_>>();
    if matches!(style, SignatureStyle::Compact) && !operation.options.is_empty() {
        parameters.push("…".into());
    }
    let expanded = matches!(style, SignatureStyle::Expanded);
    let parameters = parameters.join(if expanded { ",\n    " } else { ", " });
    let parameters = if expanded {
        format!("\n    {parameters}\n")
    } else {
        parameters
    };
    format!(
        "{}.{}({parameters}) -> {}",
        capability.name,
        operation.name,
        operation.result.name()
    )
}

fn overview(operation: &OperationSchema) -> String {
    std::iter::once(operation.documentation.summary)
        .chain(operation.documentation.notes.iter().copied())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Compact operation help; exhaustive options remain in signatures/references.
#[must_use]
pub fn operation_hover(capability: &CapabilityDescriptor, operation: &OperationSchema) -> String {
    let mut output = format!(
        "```text\n{}\n```\n\n{}",
        format_signature(capability, operation, SignatureStyle::Compact),
        overview(operation)
    );
    if !operation.documentation.example.is_empty() {
        let _ = write!(
            output,
            "\n\n```mettle\n{}\n```",
            operation.documentation.example
        );
    }
    output
}

#[must_use]
pub fn field_reference(field: &FieldSchema) -> String {
    let mut output = format!(
        "**{}** — {}\n\n{}",
        field.name,
        field.value_type.name(),
        field.description
    );
    if let Some(default) = field.default {
        let _ = write!(output, "\n\nDefault: `{default}`.");
    }
    output
}

#[must_use]
pub fn operation_reference(
    capability: &CapabilityDescriptor,
    operation: &OperationSchema,
) -> String {
    let mut output = format!(
        "# {}.{}\n\n```text\n{}\n```\n\n{}\n",
        capability.name,
        operation.name,
        format_signature(
            capability,
            operation,
            if operation.parameters.len() + operation.options.len() > 3 {
                SignatureStyle::Expanded
            } else {
                SignatureStyle::Inline
            }
        ),
        overview(operation)
    );
    if !operation.parameter_names.is_empty() {
        output.push_str("\n## Arguments\n");
        for (index, (name, kind)) in operation
            .parameter_names
            .iter()
            .zip(operation.parameters)
            .enumerate()
        {
            let description = operation
                .documentation
                .parameters
                .get(index)
                .copied()
                .unwrap_or("");
            let _ = write!(output, "\n### {name}\n\n{}\n\n{description}\n", kind.name());
        }
    }
    if !operation.options.is_empty() {
        output.push_str("\n## Options\n");
        for field in operation.options {
            let _ = write!(
                output,
                "\n### {}\n\n{}\n",
                field.name,
                field_reference(field)
            );
            append_fields(&mut output, field.value_type, &format!("{}.", field.name));
        }
    }
    if !operation.mutually_exclusive.is_empty() {
        output.push_str("\n## Conflicting options\n");
        for group in operation.mutually_exclusive {
            let _ = writeln!(output, "\n{} cannot be combined.", group.join(" / "));
        }
    }
    output.push_str("\n## Result\n");
    let _ = writeln!(output, "\n{}", operation.result.name());
    append_fields(&mut output, operation.result, "");
    if !capability.removed_result_fields.is_empty() {
        output.push_str("\n## Migration\n");
        for field in capability.removed_result_fields {
            let _ = writeln!(output, "\n{}.", field.message);
        }
    }
    if !operation.documentation.details.is_empty() {
        let _ = write!(
            output,
            "\n## Behavior\n\n{}\n",
            operation.documentation.details
        );
    }
    if !operation.documentation.example.is_empty() {
        let _ = write!(
            output,
            "\n## Example\n\n```mettle\n{}\n```\n",
            operation.documentation.example
        );
    }
    output
}

fn append_fields(output: &mut String, kind: SchemaType, prefix: &str) {
    if let SchemaType::Object(fields) = kind {
        for field in fields {
            let _ = write!(
                output,
                "\n- `{prefix}{}` ({}): {}",
                field.name,
                field.value_type.name(),
                field.description
            );
            if let Some(default) = field.default {
                let _ = write!(output, " Default: `{default}`.");
            }
            output.push('\n');
            append_fields(
                output,
                field.value_type,
                &format!("{prefix}{}.", field.name),
            );
        }
    }
}

/// Render an operation, constant, or capability without external files or requests.
#[must_use]
pub fn reference(capabilities: &[CapabilityDescriptor], name: &str) -> Option<String> {
    let (root, member) = name
        .split_once('.')
        .map_or((name, None), |(root, member)| (root, Some(member)));
    let capability = capabilities
        .iter()
        .find(|capability| capability.name == root)?;
    if let Some(member) = member {
        if let Some(operation) = capability
            .operations
            .iter()
            .find(|operation| operation.name == member)
        {
            return Some(operation_reference(capability, operation));
        }
        let constant = capability
            .constants
            .iter()
            .find(|constant| constant.name == member)?;
        return Some(format!(
            "# {name}\n\n{}\n\nValue: `{}`\n",
            constant.description, constant.value
        ));
    }
    let mut output = format!("# {root}\n\n{}\n\n## Operations\n", capability.description);
    for operation in capability.operations {
        let _ = writeln!(
            output,
            "\n- `{root}.{}`: {}",
            operation.name, operation.documentation.summary
        );
    }
    for constant in capability.constants {
        let _ = writeln!(
            output,
            "\n- `{root}.{}`: {}",
            constant.name, constant.description
        );
    }
    Some(output)
}
