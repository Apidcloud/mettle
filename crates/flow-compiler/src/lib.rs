//! Semantic validation and lowering from Flow syntax into an execution plan.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use flow_capability::{CapabilityDescriptor, FieldSchema, SchemaType};
use flow_syntax::{
    ContextMember, Expression, ExpressionKind, FlowDecl, ObjectField, Program, Span, Statement,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionPlan {
    pub capability_names: Vec<String>,
    pub contexts: Vec<ContextPlan>,
    pub flows: Vec<FlowPlan>,
    pub default_flow: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextPlan {
    pub name: String,
    pub fields: Vec<PlanField>,
    pub defaults: Vec<CapabilityDefaultsPlan>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CapabilityDefaultsPlan {
    pub capability: usize,
    pub fields: Vec<PlanField>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanField {
    pub name: String,
    pub expression: PlanExpression,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlowPlan {
    pub name: Option<String>,
    pub display_name: String,
    pub parameters: Vec<String>,
    pub span: Span,
    pub local_count: usize,
    pub contexts: Vec<usize>,
    pub instructions: Vec<Instruction>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Instruction {
    Bind {
        slot: usize,
        expression: PlanExpression,
    },
    Evaluate(PlanExpression),
    Return(PlanExpression),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanExpression {
    pub kind: PlanExpressionKind,
    pub value_type: ValueType,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlanExpressionKind {
    Constant(Constant),
    Local(usize),
    Context(usize),
    Environment(String),
    Array(Vec<PlanExpression>),
    Object(Vec<PlanField>),
    Member {
        value: Box<PlanExpression>,
        member: String,
    },
    InterpolatedString(Vec<StringPart>),
    FlowCall {
        flow: usize,
        arguments: Vec<PlanExpression>,
    },
    CapabilityCall {
        capability: usize,
        operation: usize,
        arguments: Vec<PlanExpression>,
        options: Vec<PlanField>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum StringPart {
    Text(String),
    Value(PlanExpression),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    DurationNanos(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueType {
    Null,
    Boolean,
    Integer,
    Float,
    String,
    Duration,
    Array,
    Object,
    Inferred,
}

impl ValueType {
    const fn name(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Float => "number",
            Self::String => "string",
            Self::Duration => "duration",
            Self::Array => "array",
            Self::Object => "object",
            Self::Inferred => "inferred value",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileError {
    pub message: String,
    pub span: Span,
}

impl CompileError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CompileError {}

/// Compile without external capabilities. Useful for pure Flow programs.
///
/// # Errors
///
/// Returns all independent semantic errors found during compilation.
pub fn compile(program: &Program) -> Result<ExecutionPlan, Vec<CompileError>> {
    compile_with_capabilities(program, &[])
}

/// Compile using the supplied capability schemas.
///
/// # Errors
///
/// Returns all independent semantic or capability-schema errors found during compilation.
pub fn compile_with_capabilities(
    program: &Program,
    capabilities: &[CapabilityDescriptor],
) -> Result<ExecutionPlan, Vec<CompileError>> {
    Compiler::new(program, capabilities).compile()
}

struct Compiler<'a> {
    program: &'a Program,
    capabilities: &'a [CapabilityDescriptor],
    capability_ids: HashMap<&'a str, usize>,
    context_ids: HashMap<&'a str, usize>,
    context_fields: Vec<HashMap<String, usize>>,
    flow_ids: HashMap<&'a str, usize>,
    errors: Vec<CompileError>,
    call_edges: Vec<Vec<(usize, Span)>>,
}

impl<'a> Compiler<'a> {
    fn new(program: &'a Program, capabilities: &'a [CapabilityDescriptor]) -> Self {
        Self {
            program,
            capabilities,
            capability_ids: HashMap::new(),
            context_ids: HashMap::new(),
            context_fields: vec![HashMap::new(); program.contexts.len()],
            flow_ids: HashMap::new(),
            errors: Vec::new(),
            call_edges: vec![Vec::new(); program.flows.len()],
        }
    }

    fn compile(mut self) -> Result<ExecutionPlan, Vec<CompileError>> {
        self.collect_capability_names();
        self.collect_context_names();
        self.collect_flow_names();

        let default_flow = self.flow_ids.get("main").copied();
        let file_context = self.resolve_file_context();

        let contexts = self.compile_contexts();
        let flows = self
            .program
            .flows
            .iter()
            .enumerate()
            .map(|(flow_id, flow)| self.compile_flow(flow_id, flow, file_context))
            .collect();
        self.detect_recursion();

        if self.errors.is_empty() {
            Ok(ExecutionPlan {
                capability_names: self
                    .capabilities
                    .iter()
                    .map(|capability| capability.name.to_owned())
                    .collect(),
                contexts,
                flows,
                default_flow,
            })
        } else {
            Err(self.errors)
        }
    }

    fn collect_capability_names(&mut self) {
        for (index, capability) in self.capabilities.iter().enumerate() {
            if self.capability_ids.insert(capability.name, index).is_some() {
                self.errors.push(CompileError::new(
                    format!(
                        "capability `{}` is registered more than once",
                        capability.name
                    ),
                    Span::default(),
                ));
            }
        }
    }

    fn collect_context_names(&mut self) {
        for (index, context) in self.program.contexts.iter().enumerate() {
            if self
                .context_ids
                .insert(&context.name.value, index)
                .is_some()
            {
                self.errors.push(CompileError::new(
                    format!(
                        "context `{}` is declared more than once",
                        context.name.value
                    ),
                    context.name.span,
                ));
            }
        }
    }

    fn collect_flow_names(&mut self) {
        for (index, flow) in self.program.flows.iter().enumerate() {
            let Some(name) = &flow.name else {
                continue;
            };
            if self.flow_ids.insert(&name.value, index).is_some() {
                self.errors.push(CompileError::new(
                    format!("flow `{}` is declared more than once", name.value),
                    name.span,
                ));
            }
        }
    }

    fn resolve_file_context(&mut self) -> Option<usize> {
        let name = self.program.file_contexts.first()?;
        for duplicate in self.program.file_contexts.iter().skip(1) {
            self.errors.push(CompileError::new(
                "a file may apply only one default context until context composition is available",
                duplicate.span,
            ));
        }
        self.context_ids
            .get(name.value.as_str())
            .copied()
            .or_else(|| {
                self.errors.push(CompileError::new(
                    format!("context `{}` is not defined", name.value),
                    name.span,
                ));
                None
            })
    }

    fn compile_contexts(&mut self) -> Vec<ContextPlan> {
        self.program
            .contexts
            .iter()
            .enumerate()
            .map(|(context_id, context)| {
                let mut fields = Vec::new();
                let mut defaults = Vec::new();
                let mut default_capabilities = HashMap::new();

                for member in &context.members {
                    match member {
                        ContextMember::Field(field) => {
                            if self.context_fields[context_id].contains_key(&field.name.value) {
                                self.errors.push(CompileError::new(
                                    format!(
                                        "context field `{}` is declared more than once",
                                        field.name.value
                                    ),
                                    field.name.span,
                                ));
                                continue;
                            }
                            let expression = self.compile_expression(
                                None,
                                &field.expression,
                                &HashMap::new(),
                                Some(context_id),
                            );
                            let slot = self.context_fields[context_id].len();
                            self.context_fields[context_id]
                                .insert(field.name.value.clone(), slot);
                            if let Some(expression) = expression {
                                fields.push(PlanField {
                                    name: field.name.value.clone(),
                                    expression,
                                });
                            }
                        }
                        ContextMember::Defaults {
                            capability,
                            fields: source_fields,
                            ..
                        } => {
                            let Some(capability_id) =
                                self.capability_ids.get(capability.value.as_str()).copied()
                            else {
                                self.errors.push(CompileError::new(
                                    format!(
                                        "capability `{}` is not registered",
                                        capability.value
                                    ),
                                    capability.span,
                                ));
                                continue;
                            };
                            if default_capabilities
                                .insert(capability_id, capability.span)
                                .is_some()
                            {
                                self.errors.push(CompileError::new(
                                    format!(
                                        "defaults for `{}` are declared more than once in this context",
                                        capability.value
                                    ),
                                    capability.span,
                                ));
                                continue;
                            }
                            let schema = self.capabilities[capability_id].defaults;
                            let compiled = self.compile_fields(
                                None,
                                source_fields,
                                &HashMap::new(),
                                Some(context_id),
                                Some(schema),
                            );
                            defaults.push(CapabilityDefaultsPlan {
                                capability: capability_id,
                                fields: compiled,
                            });
                        }
                    }
                }

                ContextPlan {
                    name: context.name.value.clone(),
                    fields,
                    defaults,
                }
            })
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    fn compile_flow(
        &mut self,
        flow_id: usize,
        flow: &FlowDecl,
        file_context: Option<usize>,
    ) -> FlowPlan {
        let mut locals: HashMap<String, usize> = HashMap::new();
        for parameter in &flow.parameters {
            let next_slot = locals.len();
            if locals.insert(parameter.value.clone(), next_slot).is_some() {
                self.errors.push(CompileError::new(
                    format!("parameter `{}` is declared more than once", parameter.value),
                    parameter.span,
                ));
            }
        }

        let mut contexts = file_context.into_iter().collect::<Vec<_>>();
        let mut local_context_seen = false;
        let mut executable_seen = false;
        for statement in &flow.body {
            match statement {
                Statement::UseContext { name, span } if !executable_seen => {
                    let Some(context) = self.context_ids.get(name.value.as_str()).copied() else {
                        self.errors.push(CompileError::new(
                            format!("context `{}` is not defined", name.value),
                            name.span,
                        ));
                        continue;
                    };
                    if local_context_seen {
                        self.errors.push(CompileError::new(
                            "multiple contexts in one flow are reserved for the composition milestone",
                            *span,
                        ));
                    } else {
                        contexts.clear();
                        contexts.push(context);
                        local_context_seen = true;
                    }
                }
                Statement::UseContext { span, .. } => self.errors.push(CompileError::new(
                    "`use context` must appear before executable statements",
                    *span,
                )),
                _ => executable_seen = true,
            }
        }
        let active_context = contexts.first().copied();

        let mut instructions = Vec::new();
        let mut returned = false;
        for statement in &flow.body {
            if matches!(statement, Statement::UseContext { .. }) {
                continue;
            }
            if returned {
                self.errors.push(CompileError::new(
                    "statement is unreachable because the flow already returned",
                    statement.span(),
                ));
                continue;
            }

            match statement {
                Statement::Bind {
                    name, expression, ..
                } => {
                    if locals.contains_key(&name.value) {
                        self.errors.push(CompileError::new(
                            format!("name `{}` is already defined in this flow", name.value),
                            name.span,
                        ));
                        continue;
                    }
                    let expression =
                        self.compile_expression(Some(flow_id), expression, &locals, active_context);
                    let slot = locals.len();
                    locals.insert(name.value.clone(), slot);
                    if let Some(expression) = expression {
                        instructions.push(Instruction::Bind { slot, expression });
                    }
                }
                Statement::Expression(expression) => {
                    if let Some(expression) =
                        self.compile_expression(Some(flow_id), expression, &locals, active_context)
                    {
                        instructions.push(Instruction::Evaluate(expression));
                    }
                }
                Statement::Return { expression, .. } => {
                    if let Some(expression) =
                        self.compile_expression(Some(flow_id), expression, &locals, active_context)
                    {
                        instructions.push(Instruction::Return(expression));
                    }
                    returned = true;
                }
                Statement::UseContext { .. } => unreachable!("context statements were skipped"),
            }
        }

        if !returned {
            let display_name = flow_display_name(flow, flow_id);
            self.errors.push(CompileError::new(
                format!("flow `{display_name}` must end with a return value"),
                flow.span,
            ));
        }

        FlowPlan {
            name: flow.name.as_ref().map(|name| name.value.clone()),
            display_name: flow_display_name(flow, flow_id),
            parameters: flow
                .parameters
                .iter()
                .map(|parameter| parameter.value.clone())
                .collect(),
            span: flow.span,
            local_count: locals.len(),
            contexts,
            instructions,
        }
    }

    fn compile_expression(
        &mut self,
        current_flow: Option<usize>,
        expression: &Expression,
        locals: &HashMap<String, usize>,
        context: Option<usize>,
    ) -> Option<PlanExpression> {
        let (kind, value_type) = match &expression.kind {
            ExpressionKind::Null => (
                PlanExpressionKind::Constant(Constant::Null),
                ValueType::Null,
            ),
            ExpressionKind::Boolean(value) => (
                PlanExpressionKind::Constant(Constant::Boolean(*value)),
                ValueType::Boolean,
            ),
            ExpressionKind::Integer(value) => (
                PlanExpressionKind::Constant(Constant::Integer(*value)),
                ValueType::Integer,
            ),
            ExpressionKind::Float(value) => (
                PlanExpressionKind::Constant(Constant::Float(*value)),
                ValueType::Float,
            ),
            ExpressionKind::DurationNanos(value) => (
                PlanExpressionKind::Constant(Constant::DurationNanos(*value)),
                ValueType::Duration,
            ),
            ExpressionKind::String(value) => {
                return self.compile_string(value, expression.span, locals, context);
            }
            ExpressionKind::Name(name) => {
                return self.resolve_name(name, expression.span, locals, context);
            }
            ExpressionKind::Array(values) => {
                let values = values
                    .iter()
                    .filter_map(|value| {
                        self.compile_expression(current_flow, value, locals, context)
                    })
                    .collect();
                (PlanExpressionKind::Array(values), ValueType::Array)
            }
            ExpressionKind::Object(fields) => {
                let fields = self.compile_fields(current_flow, fields, locals, context, None);
                (PlanExpressionKind::Object(fields), ValueType::Object)
            }
            ExpressionKind::Member { value, member } => {
                let value = self.compile_expression(current_flow, value, locals, context)?;
                (
                    PlanExpressionKind::Member {
                        value: Box::new(value),
                        member: member.value.clone(),
                    },
                    ValueType::Inferred,
                )
            }
            ExpressionKind::Call {
                callee,
                arguments,
                options,
            } => {
                return self.compile_call(
                    current_flow,
                    callee,
                    arguments,
                    options,
                    expression.span,
                    locals,
                    context,
                );
            }
        };
        Some(PlanExpression {
            kind,
            value_type,
            span: expression.span,
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    fn compile_call(
        &mut self,
        current_flow: Option<usize>,
        callee: &flow_syntax::Spanned<String>,
        arguments: &[Expression],
        options: &[ObjectField],
        span: Span,
        locals: &HashMap<String, usize>,
        context: Option<usize>,
    ) -> Option<PlanExpression> {
        if callee.value == "env" {
            if !options.is_empty() || arguments.len() != 1 {
                self.errors.push(CompileError::new(
                    "`env` expects exactly one string argument and no option block",
                    span,
                ));
                return None;
            }
            let ExpressionKind::String(name) = &arguments[0].kind else {
                self.errors.push(CompileError::new(
                    "environment variable name must be a string literal",
                    arguments[0].span,
                ));
                return None;
            };
            return Some(PlanExpression {
                kind: PlanExpressionKind::Environment(name.clone()),
                value_type: ValueType::String,
                span,
            });
        }

        if let Some((capability_name, operation_name)) = callee.value.split_once('.') {
            let Some(capability_id) = self.capability_ids.get(capability_name).copied() else {
                self.errors.push(CompileError::new(
                    format!("capability `{capability_name}` is not registered"),
                    callee.span,
                ));
                return None;
            };
            let capability = self.capabilities[capability_id];
            let Some((operation_id, operation)) = capability
                .operations
                .iter()
                .enumerate()
                .find(|(_, operation)| operation.name == operation_name)
            else {
                self.errors.push(CompileError::new(
                    format!("capability `{capability_name}` has no operation `{operation_name}`"),
                    callee.span,
                ));
                return None;
            };
            if arguments.len() != operation.parameters.len() {
                self.errors.push(CompileError::new(
                    format!(
                        "operation `{}` expects {} argument(s), but {} were provided",
                        callee.value,
                        operation.parameters.len(),
                        arguments.len()
                    ),
                    span,
                ));
            }
            let compiled_arguments = arguments
                .iter()
                .enumerate()
                .filter_map(|(index, argument)| {
                    let value = self.compile_expression(current_flow, argument, locals, context)?;
                    if let Some(schema) = operation.parameters.get(index) {
                        self.validate_type(&value, *schema, argument.span);
                    }
                    Some(value)
                })
                .collect();
            let options = self.compile_fields(
                current_flow,
                options,
                locals,
                context,
                Some(operation.options),
            );
            return Some(PlanExpression {
                kind: PlanExpressionKind::CapabilityCall {
                    capability: capability_id,
                    operation: operation_id,
                    arguments: compiled_arguments,
                    options,
                },
                value_type: ValueType::Object,
                span,
            });
        }

        let Some(target) = self.flow_ids.get(callee.value.as_str()).copied() else {
            self.errors.push(CompileError::new(
                format!("flow `{}` is not defined", callee.value),
                callee.span,
            ));
            return None;
        };
        if !options.is_empty() {
            self.errors.push(CompileError::new(
                "option blocks are only supported by capability operations",
                span,
            ));
        }
        let expected = self.program.flows[target].parameters.len();
        if arguments.len() != expected {
            self.errors.push(CompileError::new(
                format!(
                    "flow `{}` expects {expected} argument(s), but {} were provided",
                    callee.value,
                    arguments.len()
                ),
                span,
            ));
        }
        let compiled_arguments = arguments
            .iter()
            .filter_map(|argument| self.compile_expression(current_flow, argument, locals, context))
            .collect();
        if let Some(current_flow) = current_flow {
            self.call_edges[current_flow].push((target, callee.span));
        }
        Some(PlanExpression {
            kind: PlanExpressionKind::FlowCall {
                flow: target,
                arguments: compiled_arguments,
            },
            value_type: ValueType::Inferred,
            span,
        })
    }

    fn compile_fields(
        &mut self,
        current_flow: Option<usize>,
        fields: &[ObjectField],
        locals: &HashMap<String, usize>,
        context: Option<usize>,
        schema: Option<&[FieldSchema]>,
    ) -> Vec<PlanField> {
        let mut names = BTreeMap::new();
        fields
            .iter()
            .filter_map(|field| {
                if names.insert(&field.name.value, field.name.span).is_some() {
                    self.errors.push(CompileError::new(
                        format!("field `{}` is declared more than once", field.name.value),
                        field.name.span,
                    ));
                    return None;
                }
                let field_schema = schema.and_then(|schema| {
                    schema
                        .iter()
                        .find(|candidate| candidate.name == field.name.value)
                });
                if schema.is_some() && field_schema.is_none() {
                    self.errors.push(CompileError::new(
                        format!("unknown option `{}`", field.name.value),
                        field.name.span,
                    ));
                }
                let expression = if let Some(FieldSchema {
                    value_type: SchemaType::Object(nested),
                    ..
                }) = field_schema
                    && let ExpressionKind::Object(source_fields) = &field.expression.kind
                {
                    PlanExpression {
                        kind: PlanExpressionKind::Object(self.compile_fields(
                            current_flow,
                            source_fields,
                            locals,
                            context,
                            Some(nested),
                        )),
                        value_type: ValueType::Object,
                        span: field.expression.span,
                    }
                } else {
                    self.compile_expression(current_flow, &field.expression, locals, context)?
                };
                if let Some(field_schema) = field_schema {
                    self.validate_type(&expression, field_schema.value_type, field.expression.span);
                }
                Some(PlanField {
                    name: field.name.value.clone(),
                    expression,
                })
            })
            .collect()
    }

    fn validate_type(&mut self, value: &PlanExpression, expected: SchemaType, span: Span) {
        if value.value_type == ValueType::Inferred {
            return;
        }
        let valid = match expected {
            SchemaType::Boolean => value.value_type == ValueType::Boolean,
            SchemaType::Duration => value.value_type == ValueType::Duration,
            SchemaType::Integer => value.value_type == ValueType::Integer,
            SchemaType::String => value.value_type == ValueType::String,
            SchemaType::Object(_) | SchemaType::StringMap => value.value_type == ValueType::Object,
            SchemaType::Json => value.value_type != ValueType::Duration,
        };
        if !valid {
            self.errors.push(CompileError::new(
                format!(
                    "expected {}, found {}",
                    expected.name(),
                    value.value_type.name()
                ),
                span,
            ));
            return;
        }
        if expected == SchemaType::StringMap
            && let PlanExpressionKind::Object(fields) = &value.kind
        {
            for field in fields {
                if !matches!(
                    field.expression.value_type,
                    ValueType::String | ValueType::Inferred
                ) {
                    self.errors.push(CompileError::new(
                        "header values must be strings",
                        field.expression.span,
                    ));
                }
            }
        }
    }

    fn compile_string(
        &mut self,
        value: &str,
        span: Span,
        locals: &HashMap<String, usize>,
        context: Option<usize>,
    ) -> Option<PlanExpression> {
        if !value.contains("${") {
            return Some(PlanExpression {
                kind: PlanExpressionKind::Constant(Constant::String(value.to_owned())),
                value_type: ValueType::String,
                span,
            });
        }

        let mut remaining = value;
        let mut parts = Vec::new();
        while let Some(start) = remaining.find("${") {
            if start > 0 {
                parts.push(StringPart::Text(remaining[..start].to_owned()));
            }
            let after_start = &remaining[start + 2..];
            let Some(end) = after_start.find('}') else {
                self.errors
                    .push(CompileError::new("unterminated string interpolation", span));
                return None;
            };
            let name = &after_start[..end];
            if name.is_empty() || !name.split('.').all(is_valid_identifier) {
                self.errors.push(CompileError::new(
                    "string interpolation must contain a name or member path",
                    span,
                ));
                return None;
            }
            let expression = self.resolve_interpolation_name(name, span, locals, context)?;
            parts.push(StringPart::Value(expression));
            remaining = &after_start[end + 1..];
        }
        if !remaining.is_empty() {
            parts.push(StringPart::Text(remaining.to_owned()));
        }
        Some(PlanExpression {
            kind: PlanExpressionKind::InterpolatedString(parts),
            value_type: ValueType::String,
            span,
        })
    }

    fn resolve_name(
        &mut self,
        name: &str,
        span: Span,
        locals: &HashMap<String, usize>,
        context: Option<usize>,
    ) -> Option<PlanExpression> {
        if let Some(expression) = self.try_resolve_name(name, span, locals, context) {
            return Some(expression);
        }
        let root = name
            .split('.')
            .next()
            .expect("split always returns one part");
        self.errors.push(CompileError::new(
            format!("name `{root}` is not defined"),
            span,
        ));
        None
    }

    fn resolve_interpolation_name(
        &mut self,
        name: &str,
        span: Span,
        locals: &HashMap<String, usize>,
        context: Option<usize>,
    ) -> Option<PlanExpression> {
        if let Some(expression) = self.try_resolve_name(name, span, locals, context) {
            return Some(expression);
        }
        if !name.contains('.') {
            return Some(PlanExpression {
                kind: PlanExpressionKind::Environment(name.to_owned()),
                value_type: ValueType::String,
                span,
            });
        }
        self.errors.push(CompileError::new(
            format!("name `{name}` is not defined"),
            span,
        ));
        None
    }

    fn try_resolve_name(
        &self,
        name: &str,
        span: Span,
        locals: &HashMap<String, usize>,
        context: Option<usize>,
    ) -> Option<PlanExpression> {
        let mut parts = name.split('.');
        let root = parts.next().expect("split always returns one part");
        let mut expression = if let Some(slot) = locals.get(root).copied() {
            PlanExpression {
                kind: PlanExpressionKind::Local(slot),
                value_type: ValueType::Inferred,
                span,
            }
        } else if let Some(context) = context
            && let Some(slot) = self.context_fields[context].get(root).copied()
        {
            PlanExpression {
                kind: PlanExpressionKind::Context(slot),
                value_type: ValueType::Inferred,
                span,
            }
        } else {
            return None;
        };
        for member in parts {
            expression = PlanExpression {
                kind: PlanExpressionKind::Member {
                    value: Box::new(expression),
                    member: member.to_owned(),
                },
                value_type: ValueType::Inferred,
                span,
            };
        }
        Some(expression)
    }

    fn detect_recursion(&mut self) {
        let mut states = vec![VisitState::Unvisited; self.program.flows.len()];
        for flow in 0..self.program.flows.len() {
            self.visit_flow(flow, &mut states);
        }
    }

    fn visit_flow(&mut self, flow: usize, states: &mut [VisitState]) {
        match states[flow] {
            VisitState::Complete | VisitState::Visiting => return,
            VisitState::Unvisited => states[flow] = VisitState::Visiting,
        }
        for (target, span) in self.call_edges[flow].clone() {
            if states[target] == VisitState::Visiting {
                self.errors.push(CompileError::new(
                    format!(
                        "recursive flow calls are not supported: `{}` calls `{}`",
                        flow_display_name(&self.program.flows[flow], flow),
                        flow_display_name(&self.program.flows[target], target)
                    ),
                    span,
                ));
            } else {
                self.visit_flow(target, states);
            }
        }
        states[flow] = VisitState::Complete;
    }
}

fn flow_display_name(flow: &FlowDecl, flow_id: usize) -> String {
    if let Some(name) = &flow.name {
        return name.value.clone();
    }
    if let [Statement::Return { expression, .. }] = flow.body.as_slice()
        && let ExpressionKind::Call {
            callee, arguments, ..
        } = &expression.kind
    {
        if let Some((capability, operation)) = callee.value.split_once('.')
            && capability == "http"
            && let Some(Expression {
                kind: ExpressionKind::String(url),
                ..
            }) = arguments.first()
        {
            return format!("{} {url}", operation.to_ascii_uppercase());
        }
        return callee.value.clone();
    }
    format!("anonymous flow {}", flow_id + 1)
}

fn is_valid_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum VisitState {
    Unvisited,
    Visiting,
    Complete,
}

#[cfg(test)]
mod tests {
    use flow_capability::{CapabilityDescriptor, FieldSchema, OperationSchema, SchemaType};
    use flow_syntax::parse;

    use super::{PlanExpressionKind, compile, compile_with_capabilities};

    const TLS_OPTIONS: &[FieldSchema] = &[FieldSchema {
        name: "verifyCertificates",
        value_type: SchemaType::Boolean,
    }];
    const HTTP_OPTIONS: &[FieldSchema] = &[
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
            value_type: SchemaType::Object(TLS_OPTIONS),
        },
    ];
    const HTTP_OPERATIONS: &[OperationSchema] = &[OperationSchema {
        name: "get",
        parameters: &[SchemaType::String],
        options: HTTP_OPTIONS,
    }];
    const HTTP: CapabilityDescriptor = CapabilityDescriptor {
        name: "http",
        defaults: HTTP_OPTIONS,
        operations: HTTP_OPERATIONS,
    };

    fn errors(source: &str) -> Vec<String> {
        let program = parse(source).expect("test source should parse");
        compile(&program)
            .expect_err("test program should not compile")
            .into_iter()
            .map(|error| error.message)
            .collect()
    }

    #[test]
    fn compiles_contexts_and_capability_calls() {
        let program = parse(
            r#"
            context api {
                apiUrl: env("API_URL")
                defaults http { baseUrl: apiUrl timeout: 1s }
            }
            flow main() {
                use context api
                response = http.get("/users")
                return response.status
            }
            "#,
        )
        .expect("source should parse");
        let plan = compile_with_capabilities(&program, &[HTTP]).expect("program should compile");

        assert_eq!(plan.contexts.len(), 1);
        assert_eq!(plan.flows[0].contexts, vec![0]);
        let super::Instruction::Bind { expression, .. } = &plan.flows[0].instructions[0] else {
            panic!("expected binding");
        };
        assert!(matches!(
            expression.kind,
            PlanExpressionKind::CapabilityCall { .. }
        ));
    }

    #[test]
    fn rejects_unknown_capability_options() {
        let program = parse("flow main() { return http.get(\"/\") { banana: true } }")
            .expect("source should parse");
        let messages = compile_with_capabilities(&program, &[HTTP])
            .expect_err("program should fail")
            .into_iter()
            .map(|error| error.message)
            .collect::<Vec<_>>();
        assert!(
            messages
                .iter()
                .any(|message| message == "unknown option `banana`")
        );
    }

    #[test]
    fn rejects_unknown_nested_capability_options() {
        let program =
            parse("flow main() { return http.get(\"/\") { tls: { trustEverything: true } } }")
                .expect("source should parse");
        let messages = compile_with_capabilities(&program, &[HTTP])
            .expect_err("program should fail")
            .into_iter()
            .map(|error| error.message)
            .collect::<Vec<_>>();
        assert!(
            messages
                .iter()
                .any(|message| message == "unknown option `trustEverything`")
        );
    }

    #[test]
    fn rejects_wrong_schema_types() {
        let program = parse("flow main() { return http.get(42) }").expect("source should parse");
        let messages = compile_with_capabilities(&program, &[HTTP])
            .expect_err("program should fail")
            .into_iter()
            .map(|error| error.message)
            .collect::<Vec<_>>();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("expected string"))
        );
    }

    #[test]
    fn compiles_a_valid_pure_program() {
        let program =
            parse("flow identity(value) { return value } flow main() { return identity(\"ok\") }")
                .expect("source should parse");
        let plan = compile(&program).expect("program should compile");
        assert_eq!(plan.default_flow, Some(1));
    }

    #[test]
    fn rejects_recursive_calls() {
        let messages = errors("flow main() { return main() }");
        assert!(messages[0].contains("recursive flow calls"));
    }

    #[test]
    fn accepts_programs_without_main_and_parameterized_main() {
        let helper = compile(&parse("flow helper() = null").expect("source should parse"))
            .expect("helper-only source should compile");
        assert_eq!(helper.default_flow, None);

        let main = compile(&parse("flow main(value) = value").expect("source should parse"))
            .expect("parameterized main should compile");
        assert_eq!(main.default_flow, Some(0));
        assert_eq!(main.flows[0].parameters, ["value"]);
    }

    #[test]
    fn interpolation_prefers_values_then_falls_back_to_environment() {
        let program = parse(
            r#"
            flow target(API_URL) = "${API_URL}/local"
            flow environment() = "${API_URL}/environment"
            "#,
        )
        .expect("source should parse");
        let plan = compile(&program).expect("source should compile");

        let super::Instruction::Return(local) = &plan.flows[0].instructions[0] else {
            panic!("expected return");
        };
        let PlanExpressionKind::InterpolatedString(local_parts) = &local.kind else {
            panic!("expected interpolated string");
        };
        assert!(matches!(
            local_parts[0],
            super::StringPart::Value(super::PlanExpression {
                kind: PlanExpressionKind::Local(0),
                ..
            })
        ));

        let super::Instruction::Return(environment) = &plan.flows[1].instructions[0] else {
            panic!("expected return");
        };
        let PlanExpressionKind::InterpolatedString(environment_parts) = &environment.kind else {
            panic!("expected interpolated string");
        };
        assert!(matches!(
            &environment_parts[0],
            super::StringPart::Value(super::PlanExpression {
                kind: PlanExpressionKind::Environment(name),
                ..
            }) if name == "API_URL"
        ));
    }
}
