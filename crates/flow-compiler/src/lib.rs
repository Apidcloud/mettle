//! Semantic validation and lowering from Flow syntax into an execution plan.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use flow_capability::{CapabilityDescriptor, FieldSchema, SchemaType};
pub use flow_syntax::BinaryOperator;
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
    Assert(PlanExpression),
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
    Binary {
        left: Box<PlanExpression>,
        operator: BinaryOperator,
        right: Box<PlanExpression>,
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

/// Find the declaration referenced at a source position.
///
/// This uses the same namespace visibility rules as compilation. It intentionally
/// returns no target for ambiguous or unresolved names.
#[must_use]
pub fn find_definition(program: &Program, source: usize, byte: usize) -> Option<Span> {
    DefinitionFinder::new(program, source, byte).find()
}

struct DefinitionFinder<'a> {
    program: &'a Program,
    source: usize,
    byte: usize,
    context_ids: HashMap<String, usize>,
    flow_ids: HashMap<String, usize>,
}

impl<'a> DefinitionFinder<'a> {
    fn new(program: &'a Program, source: usize, byte: usize) -> Self {
        let context_ids = program
            .contexts
            .iter()
            .enumerate()
            .map(|(id, context)| (qualified_name(&context.namespace, &context.name.value), id))
            .collect();
        let flow_ids = program
            .flows
            .iter()
            .enumerate()
            .filter_map(|(id, flow)| {
                flow.name
                    .as_ref()
                    .map(|name| (qualified_name(&flow.namespace, &name.value), id))
            })
            .collect();
        Self {
            program,
            source,
            byte,
            context_ids,
            flow_ids,
        }
    }

    fn find(&self) -> Option<Span> {
        for context in &self.program.contexts {
            if self.at(context.name.span) {
                return Some(context.name.span);
            }
            for member in &context.members {
                match member {
                    ContextMember::UseContext { name, .. } if self.at(name.span) => {
                        return self.resolve_context(
                            &name.value,
                            &context.namespace,
                            &context.namespace_uses,
                        );
                    }
                    ContextMember::Field(field) => {
                        if let Some(target) =
                            self.find_in_expression(&field.expression, &HashMap::new(), None)
                        {
                            return Some(target);
                        }
                    }
                    ContextMember::Defaults { fields, .. } => {
                        for field in fields {
                            if let Some(target) =
                                self.find_in_expression(&field.expression, &HashMap::new(), None)
                            {
                                return Some(target);
                            }
                        }
                    }
                    ContextMember::UseContext { .. } => {}
                }
            }
        }

        for name in &self.program.file_contexts {
            if self.at(name.span) {
                let namespace = self
                    .program
                    .namespace
                    .as_ref()
                    .map_or("", |value| value.value.as_str());
                return self.resolve_context(
                    name.value.as_str(),
                    namespace,
                    &self.program.namespace_uses,
                );
            }
        }

        for flow in &self.program.flows {
            if let Some(name) = &flow.name
                && self.at(name.span)
            {
                return Some(name.span);
            }
            if let Some(target) = self.find_in_flow(flow) {
                return Some(target);
            }
        }
        None
    }

    fn find_in_flow(&self, flow: &FlowDecl) -> Option<Span> {
        let mut locals = flow
            .parameters
            .iter()
            .map(|parameter| (parameter.value.clone(), parameter.span))
            .collect::<HashMap<_, _>>();
        for parameter in &flow.parameters {
            if self.at(parameter.span) {
                return Some(parameter.span);
            }
        }
        for statement in &flow.body {
            match statement {
                Statement::UseContext { name, .. } => {
                    if self.at(name.span) {
                        return self.resolve_context(
                            &name.value,
                            &flow.namespace,
                            &flow.namespace_uses,
                        );
                    }
                }
                Statement::Bind {
                    name, expression, ..
                } => {
                    if self.at(name.span) {
                        return Some(name.span);
                    }
                    if let Some(target) = self.find_in_expression(expression, &locals, Some(flow)) {
                        return Some(target);
                    }
                    locals.insert(name.value.clone(), name.span);
                }
                Statement::Return { expression, .. }
                | Statement::Assert { expression, .. }
                | Statement::Expression(expression) => {
                    if let Some(target) = self.find_in_expression(expression, &locals, Some(flow)) {
                        return Some(target);
                    }
                }
            }
        }
        None
    }

    fn find_in_expression(
        &self,
        expression: &Expression,
        locals: &HashMap<String, Span>,
        flow: Option<&FlowDecl>,
    ) -> Option<Span> {
        match &expression.kind {
            ExpressionKind::Name(name) => {
                if self.at(expression.span) {
                    let root = name.split('.').next().unwrap_or(name);
                    return locals.get(root).copied();
                }
            }
            ExpressionKind::Array(values) => {
                for value in values {
                    if let Some(target) = self.find_in_expression(value, locals, flow) {
                        return Some(target);
                    }
                }
            }
            ExpressionKind::Object(fields) => {
                for field in fields {
                    if let Some(target) = self.find_in_expression(&field.expression, locals, flow) {
                        return Some(target);
                    }
                }
            }
            ExpressionKind::Call {
                callee,
                arguments,
                options,
            } => {
                if self.at(callee.span)
                    && !callee.value.contains('.')
                    && callee.value != "env"
                    && let Some(flow) = flow
                    && let NameResolution::Found(id) = resolve_visible_name(
                        &self.flow_ids,
                        &callee.value,
                        &flow.namespace,
                        &flow.namespace_uses,
                    )
                {
                    return self.program.flows[id].name.as_ref().map(|name| name.span);
                }
                for argument in arguments {
                    if let Some(target) = self.find_in_expression(argument, locals, flow) {
                        return Some(target);
                    }
                }
                for option in options {
                    if let Some(target) = self.find_in_expression(&option.expression, locals, flow)
                    {
                        return Some(target);
                    }
                }
            }
            ExpressionKind::Member { value, .. } => {
                return self.find_in_expression(value, locals, flow);
            }
            ExpressionKind::Binary { left, right, .. } => {
                return self
                    .find_in_expression(left, locals, flow)
                    .or_else(|| self.find_in_expression(right, locals, flow));
            }
            ExpressionKind::Null
            | ExpressionKind::Boolean(_)
            | ExpressionKind::Integer(_)
            | ExpressionKind::Float(_)
            | ExpressionKind::String(_)
            | ExpressionKind::DurationNanos(_) => {}
        }
        None
    }

    fn resolve_context(
        &self,
        name: &str,
        namespace: &str,
        uses: &[flow_syntax::Spanned<String>],
    ) -> Option<Span> {
        let NameResolution::Found(id) =
            resolve_visible_name(&self.context_ids, name, namespace, uses)
        else {
            return None;
        };
        Some(self.program.contexts[id].name.span)
    }

    fn at(&self, span: Span) -> bool {
        span.source == self.source && self.byte >= span.start && self.byte <= span.end
    }
}

struct Compiler<'a> {
    program: &'a Program,
    capabilities: &'a [CapabilityDescriptor],
    capability_ids: HashMap<&'a str, usize>,
    context_ids: HashMap<String, usize>,
    context_fields: Vec<HashMap<String, usize>>,
    flow_ids: HashMap<String, usize>,
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
            let qualified = qualified_name(&context.namespace, &context.name.value);
            if self.context_ids.insert(qualified.clone(), index).is_some() {
                self.errors.push(CompileError::new(
                    format!("context `{qualified}` is declared more than once"),
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
            let qualified = qualified_name(&flow.namespace, &name.value);
            if self.flow_ids.insert(qualified.clone(), index).is_some() {
                self.errors.push(CompileError::new(
                    format!("flow `{qualified}` is declared more than once"),
                    name.span,
                ));
            }
        }
    }

    fn resolve_context_name(
        &mut self,
        name: &str,
        namespace: &str,
        uses: &[flow_syntax::Spanned<String>],
        span: Span,
    ) -> Option<usize> {
        match resolve_visible_name(&self.context_ids, name, namespace, uses) {
            NameResolution::Found(id) => Some(id),
            NameResolution::Missing => None,
            NameResolution::Ambiguous(candidates) => {
                self.errors.push(CompileError::new(
                    format!(
                        "context `{name}` is ambiguous; it is provided by {}",
                        candidates.join(", ")
                    ),
                    span,
                ));
                None
            }
        }
    }

    fn resolve_flow_name(&mut self, name: &str, flow: &FlowDecl, span: Span) -> Option<usize> {
        match resolve_visible_name(&self.flow_ids, name, &flow.namespace, &flow.namespace_uses) {
            NameResolution::Found(id) => Some(id),
            NameResolution::Missing => None,
            NameResolution::Ambiguous(candidates) => {
                self.errors.push(CompileError::new(
                    format!(
                        "flow `{name}` is ambiguous; it is provided by {}",
                        candidates.join(", ")
                    ),
                    span,
                ));
                None
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
        let error_count = self.errors.len();
        self.resolve_context_name(
            &name.value,
            self.program
                .namespace
                .as_ref()
                .map_or("", |namespace| namespace.value.as_str()),
            &self.program.namespace_uses,
            name.span,
        )
        .or_else(|| {
            if self.errors.len() == error_count {
                self.errors.push(CompileError::new(
                    format!("context `{}` is not defined", name.value),
                    name.span,
                ));
            }
            None
        })
    }

    fn compile_contexts(&mut self) -> Vec<ContextPlan> {
        let mut states = vec![VisitState::Unvisited; self.program.contexts.len()];
        let mut cache = vec![None; self.program.contexts.len()];
        let mut stack = Vec::new();
        for context in 0..self.program.contexts.len() {
            self.flatten_context(context, &mut states, &mut cache, &mut stack);
        }

        for (context_id, flattened) in cache.iter().enumerate() {
            if let Some(flattened) = flattened {
                for (slot, field) in flattened.fields.iter().enumerate() {
                    self.context_fields[context_id].insert(field.name.value.clone(), slot);
                }
            }
        }

        cache
            .into_iter()
            .enumerate()
            .map(|(context_id, flattened)| {
                let flattened = flattened.unwrap_or_default();
                let fields = flattened
                    .fields
                    .iter()
                    .filter_map(|field| {
                        self.compile_expression(
                            None,
                            &field.expression,
                            &HashMap::new(),
                            Some(context_id),
                        )
                        .map(|expression| PlanField {
                            name: field.name.value.clone(),
                            expression,
                        })
                    })
                    .collect();
                let mut defaults = Vec::new();
                for (capability, source_fields) in &flattened.defaults {
                    let Some(capability_id) =
                        self.capability_ids.get(capability.value.as_str()).copied()
                    else {
                        self.errors.push(CompileError::new(
                            format!("capability `{}` is not registered", capability.value),
                            capability.span,
                        ));
                        continue;
                    };
                    defaults.push(CapabilityDefaultsPlan {
                        capability: capability_id,
                        fields: self.compile_fields(
                            None,
                            source_fields,
                            &HashMap::new(),
                            Some(context_id),
                            Some(self.capabilities[capability_id].defaults),
                        ),
                    });
                }
                let context = &self.program.contexts[context_id];
                ContextPlan {
                    name: qualified_name(&context.namespace, &context.name.value),
                    fields,
                    defaults,
                }
            })
            .collect()
    }

    fn flatten_context(
        &mut self,
        context_id: usize,
        states: &mut [VisitState],
        cache: &mut [Option<FlattenedContext>],
        stack: &mut Vec<usize>,
    ) -> FlattenedContext {
        if states[context_id] == VisitState::Complete {
            return cache[context_id].clone().unwrap_or_default();
        }
        if states[context_id] == VisitState::Visiting {
            let start = stack.iter().position(|id| *id == context_id).unwrap_or(0);
            let mut chain = stack[start..]
                .iter()
                .map(|id| self.context_qualified_name(*id))
                .collect::<Vec<_>>();
            chain.push(self.context_qualified_name(context_id));
            self.errors.push(CompileError::new(
                format!("context composition cycle: {}", chain.join(" -> ")),
                self.program.contexts[context_id].name.span,
            ));
            return FlattenedContext::default();
        }

        states[context_id] = VisitState::Visiting;
        stack.push(context_id);
        let context = self.program.contexts[context_id].clone();
        let mut flattened = FlattenedContext::default();
        for member in &context.members {
            if let ContextMember::UseContext { name, .. } = member {
                let error_count = self.errors.len();
                if let Some(parent) = self.resolve_context_name(
                    &name.value,
                    &context.namespace,
                    &context.namespace_uses,
                    name.span,
                ) {
                    let parent = self.flatten_context(parent, states, cache, stack);
                    for field in parent.fields {
                        merge_context_field(&mut flattened.fields, field);
                    }
                    flattened.defaults.extend(parent.defaults);
                } else if self.errors.len() == error_count {
                    self.errors.push(CompileError::new(
                        format!("context `{}` is not defined", name.value),
                        name.span,
                    ));
                }
            }
        }

        let mut local_fields = HashMap::new();
        let mut local_defaults = HashMap::new();
        for member in &context.members {
            match member {
                ContextMember::Field(field) => {
                    if local_fields
                        .insert(&field.name.value, field.name.span)
                        .is_some()
                    {
                        self.errors.push(CompileError::new(
                            format!(
                                "context field `{}` is declared more than once",
                                field.name.value
                            ),
                            field.name.span,
                        ));
                    } else {
                        merge_context_field(&mut flattened.fields, field.clone());
                    }
                }
                ContextMember::Defaults {
                    capability, fields, ..
                } => {
                    if local_defaults
                        .insert(&capability.value, capability.span)
                        .is_some()
                    {
                        self.errors.push(CompileError::new(
                            format!(
                                "defaults for `{}` are declared more than once in this context",
                                capability.value
                            ),
                            capability.span,
                        ));
                    } else {
                        flattened
                            .defaults
                            .push((capability.clone(), fields.clone()));
                    }
                }
                ContextMember::UseContext { .. } => {}
            }
        }
        stack.pop();
        states[context_id] = VisitState::Complete;
        cache[context_id] = Some(flattened.clone());
        flattened
    }

    fn context_qualified_name(&self, context: usize) -> String {
        let context = &self.program.contexts[context];
        qualified_name(&context.namespace, &context.name.value)
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
                    let error_count = self.errors.len();
                    let Some(context) = self.resolve_context_name(
                        &name.value,
                        &flow.namespace,
                        &flow.namespace_uses,
                        name.span,
                    ) else {
                        if self.errors.len() == error_count {
                            self.errors.push(CompileError::new(
                                format!("context `{}` is not defined", name.value),
                                name.span,
                            ));
                        }
                        continue;
                    };
                    if local_context_seen {
                        self.errors.push(CompileError::new(
                            "a flow may apply one context; compose reusable contexts in a context declaration",
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
                Statement::Assert { expression, .. } => {
                    if let Some(expression) =
                        self.compile_expression(Some(flow_id), expression, &locals, active_context)
                    {
                        if !matches!(
                            expression.value_type,
                            ValueType::Boolean | ValueType::Inferred
                        ) {
                            self.errors.push(CompileError::new(
                                "assertion expression must be boolean",
                                expression.span,
                            ));
                        }
                        instructions.push(Instruction::Assert(expression));
                    }
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
            name: flow
                .name
                .as_ref()
                .map(|name| qualified_name(&flow.namespace, &name.value)),
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
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                let left = self.compile_expression(current_flow, left, locals, context)?;
                let right = self.compile_expression(current_flow, right, locals, context)?;
                (
                    PlanExpressionKind::Binary {
                        left: Box::new(left),
                        operator: *operator,
                        right: Box::new(right),
                    },
                    ValueType::Boolean,
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

        let Some(current_flow_id) = current_flow else {
            self.errors.push(CompileError::new(
                "flow calls are not available while evaluating a context",
                callee.span,
            ));
            return None;
        };
        let current_flow_decl = self.program.flows[current_flow_id].clone();
        let error_count = self.errors.len();
        let Some(target) = self.resolve_flow_name(&callee.value, &current_flow_decl, callee.span)
        else {
            if self.errors.len() == error_count {
                self.errors.push(CompileError::new(
                    format!("flow `{}` is not defined", callee.value),
                    callee.span,
                ));
            }
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
        self.call_edges[current_flow_id].push((target, callee.span));
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
        return qualified_name(&flow.namespace, &name.value);
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

fn qualified_name(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_owned()
    } else {
        format!("{namespace}.{name}")
    }
}

enum NameResolution {
    Found(usize),
    Missing,
    Ambiguous(Vec<String>),
}

fn resolve_visible_name(
    names: &HashMap<String, usize>,
    name: &str,
    namespace: &str,
    uses: &[flow_syntax::Spanned<String>],
) -> NameResolution {
    let local = qualified_name(namespace, name);
    if let Some(id) = names.get(&local).copied() {
        return NameResolution::Found(id);
    }

    let mut candidates = Vec::new();
    if !namespace.is_empty() && names.contains_key(name) {
        candidates.push(name.to_owned());
    }
    for used in uses {
        let candidate = qualified_name(&used.value, name);
        if names.contains_key(&candidate) && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    match candidates.as_slice() {
        [] => NameResolution::Missing,
        [candidate] => NameResolution::Found(names[candidate]),
        _ => NameResolution::Ambiguous(candidates),
    }
}

#[derive(Clone, Default)]
struct FlattenedContext {
    fields: Vec<ObjectField>,
    defaults: Vec<(flow_syntax::Spanned<String>, Vec<ObjectField>)>,
}

fn merge_context_field(fields: &mut Vec<ObjectField>, field: ObjectField) {
    if let Some(existing) = fields
        .iter_mut()
        .find(|existing| existing.name.value == field.name.value)
    {
        *existing = field;
    } else {
        fields.push(field);
    }
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
    use flow_syntax::{ExpressionKind, Statement, parse};

    use super::{PlanExpressionKind, compile, compile_with_capabilities, find_definition};

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

    #[test]
    fn finds_cross_file_flow_and_context_definitions() {
        let mut declarations =
            parse("namespace shared\ncontext api {}\nflow helper(value) = value\n")
                .expect("declarations should parse");
        declarations.set_source(0);
        let context_span = declarations.contexts[0].name.span;
        let flow_span = declarations.flows[0]
            .name
            .as_ref()
            .expect("helper is named")
            .span;

        let mut entry = parse(
            "use namespace shared\nflow main(input) {\n use context api\n result = helper(input)\n return result\n}\n",
        )
        .expect("entry should parse");
        entry.set_source(1);
        let use_context = match &entry.flows[0].body[0] {
            Statement::UseContext { name, .. } => name.span,
            _ => panic!("expected context use"),
        };
        let call = match &entry.flows[0].body[1] {
            Statement::Bind { expression, .. } => match &expression.kind {
                ExpressionKind::Call { callee, .. } => callee.span,
                _ => panic!("expected call"),
            },
            _ => panic!("expected binding"),
        };
        let local = match &entry.flows[0].body[2] {
            Statement::Return { expression, .. } => expression.span,
            _ => panic!("expected return"),
        };
        let binding = match &entry.flows[0].body[1] {
            Statement::Bind { name, .. } => name.span,
            _ => panic!("expected binding"),
        };

        let mut program = entry;
        program.contexts.extend(declarations.contexts);
        program.flows.extend(declarations.flows);

        assert_eq!(
            find_definition(&program, 1, use_context.start),
            Some(context_span)
        );
        assert_eq!(find_definition(&program, 1, call.start), Some(flow_span));
        assert_eq!(find_definition(&program, 1, local.start), Some(binding));
    }
}
