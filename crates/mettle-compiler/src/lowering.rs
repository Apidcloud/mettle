//! Semantic lowering and validation.

use super::{
    BTreeMap, CapabilityDefaultsPlan, CapabilityDescriptor, CompileError, Constant, ContextMember,
    ContextPlan, DeclarationKind, Duration, ExecutionPlan, Expression, ExpressionKind, FieldSchema,
    FileContextUse, FlattenedContext, HashMap, HashSet, Instruction, MettleDecl, MettlePlan,
    NameResolution, ObjectField, PlanExpression, PlanExpressionKind, PlanField, Program,
    SchemaType, Span, Statement, StringPart, ValueType, VisitState, flow_display_name,
    is_valid_identifier, merge_context_field, qualified_name, resolve_visible_name,
    schema_value_type, with_name_suggestion,
};

/// Compile without external capabilities. Useful for pure Mettle programs.
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
        self.validate_file_contexts();

        let default_flow = self.flow_ids.get("main").copied();
        let contexts = self.compile_contexts();
        let mut flows = Vec::with_capacity(self.program.flows.len());
        for (flow_id, flow) in self.program.flows.iter().enumerate() {
            let file_context = self.resolve_file_context(flow);
            flows.push(self.compile_flow(flow_id, flow, file_context));
        }
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
            let Some(name) = &context.name else {
                continue;
            };
            let qualified = qualified_name(&context.namespace, &name.value);
            if self.context_ids.insert(qualified.clone(), index).is_some() {
                self.errors.push(CompileError::new(
                    format!("context `{qualified}` is declared more than once"),
                    name.span,
                ));
            }
        }
    }

    fn collect_flow_names(&mut self) {
        let mut test_names = HashMap::new();
        for (index, flow) in self.program.flows.iter().enumerate() {
            let Some(name) = &flow.name else {
                continue;
            };
            if flow.kind == DeclarationKind::Test {
                if test_names
                    .insert((flow.span.source, name.value.as_str()), name.span)
                    .is_some()
                {
                    self.errors.push(CompileError::new(
                        format!(
                            "test `{}` is declared more than once in this file",
                            name.value
                        ),
                        name.span,
                    ));
                }
                continue;
            }
            let qualified = qualified_name(&flow.namespace, &name.value);
            if self.flow_ids.insert(qualified.clone(), index).is_some() {
                self.errors.push(CompileError::new(
                    format!("flow `{qualified}` is declared more than once"),
                    name.span,
                ));
            }
        }
    }

    fn validate_file_contexts(&mut self) {
        let mut sources = HashSet::new();
        for context in &self.program.file_contexts {
            if !sources.insert(context.span().source) {
                self.errors.push(CompileError::new(
                    "a file may apply only one default context until context composition is available",
                    context.span(),
                ));
            }
        }
    }

    fn resolve_context_name(
        &mut self,
        name: &str,
        namespace: &str,
        uses: &[mettle_syntax::Spanned<String>],
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

    fn resolve_flow_name(&mut self, name: &str, flow: &MettleDecl, span: Span) -> Option<usize> {
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

    fn resolve_file_context(&mut self, flow: &MettleDecl) -> Option<usize> {
        let context = self
            .program
            .file_contexts
            .iter()
            .find(|context| context.span().source == flow.span.source)?;
        match context {
            FileContextUse::Inline(span) => self
                .program
                .contexts
                .iter()
                .position(|context| context.name.is_none() && context.span == *span),
            FileContextUse::Named(name) => {
                let error_count = self.errors.len();
                self.resolve_context_name(
                    &name.value,
                    &flow.namespace,
                    &flow.namespace_uses,
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
        }
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
                ContextPlan {
                    name: self.context_qualified_name(context_id),
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
                self.program.contexts[context_id].span,
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
        context.name.as_ref().map_or_else(
            || {
                format!(
                    "<anonymous context at {}:{}>",
                    context.span.source, context.span.start
                )
            },
            |name| qualified_name(&context.namespace, &name.value),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn compile_flow(
        &mut self,
        flow_id: usize,
        flow: &MettleDecl,
        file_context: Option<usize>,
    ) -> MettlePlan {
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

        let mut next_slot = locals.len();
        let (instructions, returned) = self.compile_statements(
            flow_id,
            flow,
            &flow.body,
            &mut locals,
            &mut next_slot,
            active_context,
            true,
        );

        if !returned && flow.kind == DeclarationKind::Flow {
            let display_name = flow_display_name(flow, flow_id);
            self.errors.push(CompileError::new(
                format!("flow `{display_name}` must end with a return value"),
                flow.span,
            ));
        }

        MettlePlan {
            kind: flow.kind,
            name: flow
                .name
                .as_ref()
                .filter(|_| flow.kind == DeclarationKind::Flow)
                .map(|name| qualified_name(&flow.namespace, &name.value)),
            display_name: flow_display_name(flow, flow_id),
            parameters: flow
                .parameters
                .iter()
                .map(|parameter| parameter.value.clone())
                .collect(),
            span: flow.span,
            local_count: next_slot,
            contexts,
            instructions,
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn compile_statements(
        &mut self,
        flow_id: usize,
        flow: &MettleDecl,
        statements: &[Statement],
        locals: &mut HashMap<String, usize>,
        next_slot: &mut usize,
        active_context: Option<usize>,
        top_level: bool,
    ) -> (Vec<Instruction>, bool) {
        let mut instructions = Vec::new();
        let mut returned = false;
        for statement in statements {
            if returned {
                self.errors.push(CompileError::new(
                    "statement is unreachable because the flow already returned",
                    statement.span(),
                ));
                continue;
            }
            match statement {
                Statement::UseContext { span, .. } => {
                    if !top_level {
                        self.errors.push(CompileError::new(
                            "`use context` is only allowed at flow entry",
                            *span,
                        ));
                    }
                }
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
                        self.compile_expression(Some(flow_id), expression, locals, active_context);
                    let slot = *next_slot;
                    *next_slot += 1;
                    locals.insert(name.value.clone(), slot);
                    if let Some(expression) = expression {
                        instructions.push(Instruction::Bind { slot, expression });
                    }
                }
                Statement::Expression(expression) => {
                    if let Some(expression) =
                        self.compile_expression(Some(flow_id), expression, locals, active_context)
                    {
                        instructions.push(Instruction::Evaluate(expression));
                    }
                }
                Statement::Return { expression, .. } => {
                    if flow.kind == DeclarationKind::Test {
                        self.errors.push(CompileError::new(
                            "`return` is not allowed in a test",
                            statement.span(),
                        ));
                        continue;
                    }
                    if let Some(expression) =
                        self.compile_expression(Some(flow_id), expression, locals, active_context)
                    {
                        instructions.push(Instruction::Return(expression));
                    }
                    returned = true;
                }
                Statement::Assert {
                    expression,
                    message,
                    ..
                } => {
                    if let Some(expression) =
                        self.compile_expression(Some(flow_id), expression, locals, active_context)
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
                        let message = message.as_ref().and_then(|message| {
                            self.compile_expression(Some(flow_id), message, locals, active_context)
                        });
                        if let Some(message) = &message
                            && !matches!(
                                message.value_type,
                                ValueType::String | ValueType::Inferred
                            )
                        {
                            self.errors.push(CompileError::new(
                                "assertion message must be a string",
                                message.span,
                            ));
                        }
                        instructions.push(Instruction::Assert {
                            condition: expression,
                            message,
                        });
                    }
                }
                Statement::If {
                    branches,
                    else_body,
                    ..
                } => {
                    let mut lowered = Vec::new();
                    let mut all_returned = true;
                    for branch in branches {
                        let condition = self.compile_expression(
                            Some(flow_id),
                            &branch.condition,
                            locals,
                            active_context,
                        );
                        if let Some(condition) = &condition
                            && !matches!(
                                condition.value_type,
                                ValueType::Boolean | ValueType::Inferred
                            )
                        {
                            self.errors.push(CompileError::new(
                                "if condition must be boolean",
                                condition.span,
                            ));
                        }
                        let (body, branch_returned) = self.compile_statements(
                            flow_id,
                            flow,
                            &branch.body,
                            &mut locals.clone(),
                            next_slot,
                            active_context,
                            false,
                        );
                        all_returned &= branch_returned;
                        if let Some(condition) = condition {
                            lowered.push(super::ConditionalBranch {
                                condition,
                                instructions: body,
                            });
                        }
                    }
                    let else_instructions = else_body.as_ref().map(|body| {
                        let (instructions, branch_returned) = self.compile_statements(
                            flow_id,
                            flow,
                            body,
                            &mut locals.clone(),
                            next_slot,
                            active_context,
                            false,
                        );
                        all_returned &= branch_returned;
                        instructions
                    });
                    returned = all_returned && else_instructions.is_some();
                    instructions.push(Instruction::If {
                        branches: lowered,
                        else_body: else_instructions,
                    });
                }
            }
        }
        (instructions, returned)
    }

    #[allow(clippy::too_many_lines)]
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
            ExpressionKind::Index { value, index } => {
                let value = self.compile_expression(current_flow, value, locals, context)?;
                let index = self.compile_expression(current_flow, index, locals, context)?;
                if !matches!(
                    index.value_type,
                    ValueType::String | ValueType::Integer | ValueType::Inferred
                ) {
                    self.errors.push(CompileError::new(
                        "index must be a string key or integer position",
                        index.span,
                    ));
                }
                (
                    PlanExpressionKind::Index {
                        value: Box::new(value),
                        index: Box::new(index),
                    },
                    ValueType::Inferred,
                )
            }
            ExpressionKind::Not(value) => {
                let value = self.compile_expression(current_flow, value, locals, context)?;
                if !matches!(value.value_type, ValueType::Boolean | ValueType::Inferred) {
                    self.errors
                        .push(CompileError::new("`not` requires a boolean", value.span));
                }
                (PlanExpressionKind::Not(Box::new(value)), ValueType::Boolean)
            }
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                let left = self.compile_expression(current_flow, left, locals, context)?;
                let right = self.compile_expression(current_flow, right, locals, context)?;
                if matches!(
                    operator,
                    super::BinaryOperator::And | super::BinaryOperator::Or
                ) {
                    for value in [&left, &right] {
                        if !matches!(value.value_type, ValueType::Boolean | ValueType::Inferred) {
                            self.errors.push(CompileError::new(
                                "logical operators require booleans",
                                value.span,
                            ));
                        }
                    }
                }
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
            ExpressionKind::Within { .. }
            | ExpressionKind::Retry { .. }
            | ExpressionKind::Parallel { .. }
            | ExpressionKind::Rate { .. }
            | ExpressionKind::Concurrency { .. } => {
                return self.compile_policy_expression(current_flow, expression, locals, context);
            }
        };
        Some(PlanExpression {
            kind,
            value_type,
            span: expression.span,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn compile_policy_expression(
        &mut self,
        current_flow: Option<usize>,
        expression: &Expression,
        locals: &HashMap<String, usize>,
        context: Option<usize>,
    ) -> Option<PlanExpression> {
        let (kind, value_type) = match &expression.kind {
            ExpressionKind::Within { timeout, body } => {
                let timeout = self.duration_literal(timeout, "`within` timeout")?;
                let body = self.compile_expression(current_flow, body, locals, context)?;
                let value_type = body.value_type;
                (
                    PlanExpressionKind::Within {
                        timeout,
                        body: Box::new(body),
                    },
                    value_type,
                )
            }
            ExpressionKind::Retry {
                attempts,
                delay,
                body,
            } => {
                let attempts = self.positive_integer_literal(attempts, "`retry` attempts")?;
                let delay = match delay.as_deref() {
                    Some(delay) => self.duration_literal(delay, "`retry` delay")?,
                    None => Duration::default(),
                };
                let body = self.compile_expression(current_flow, body, locals, context)?;
                let value_type = body.value_type;
                (
                    PlanExpressionKind::Retry {
                        attempts,
                        delay,
                        body: Box::new(body),
                    },
                    value_type,
                )
            }
            ExpressionKind::Parallel { limit, branches } => {
                if branches.is_empty() || branches.len() > 1_024 {
                    let message = if branches.is_empty() {
                        "`parallel` requires at least one branch"
                    } else {
                        "`parallel` supports at most 1024 branches"
                    };
                    self.errors
                        .push(CompileError::new(message, expression.span));
                    return None;
                }
                let limit = match limit.as_deref() {
                    Some(limit) => self.positive_integer_literal(limit, "`parallel` limit")?,
                    None => branches.len(),
                }
                .min(branches.len());
                let branches = branches
                    .iter()
                    .filter_map(|branch| {
                        self.compile_expression(current_flow, branch, locals, context)
                    })
                    .collect();
                (
                    PlanExpressionKind::Parallel { limit, branches },
                    ValueType::Array,
                )
            }
            ExpressionKind::Rate {
                target,
                period,
                duration,
                limit,
                body,
            } => {
                let target = self.positive_integer_literal(target, "`rate` target")?;
                let period = self.duration_literal(period, "`rate` period")?;
                let duration = self.duration_literal(duration, "`rate` duration")?;
                let limit = match limit.as_deref() {
                    Some(limit) => self.positive_integer_literal(limit, "`rate` limit")?,
                    None => 1_024,
                };
                if target > 1_000_000 {
                    self.errors.push(CompileError::new(
                        "`rate` target supports at most 1,000,000 starts per period",
                        expression.span,
                    ));
                    return None;
                }
                if limit > 100_000 {
                    self.errors.push(CompileError::new(
                        "`rate` limit supports at most 100,000 active iterations",
                        expression.span,
                    ));
                    return None;
                }
                let Some(total) = duration
                    .as_nanos()
                    .checked_mul(target as u128)
                    .map(|value| value / period.as_nanos())
                else {
                    self.errors.push(CompileError::new(
                        "`rate` configuration exceeds the supported workload size",
                        expression.span,
                    ));
                    return None;
                };
                if total == 0 || total > 100_000_000 {
                    self.errors.push(CompileError::new(
                        "`rate` must schedule between 1 and 100,000,000 iterations",
                        expression.span,
                    ));
                    return None;
                }
                let body = self.compile_expression(current_flow, body, locals, context)?;
                (
                    PlanExpressionKind::Rate {
                        target,
                        period,
                        duration,
                        limit,
                        body: Box::new(body),
                    },
                    ValueType::Object,
                )
            }
            ExpressionKind::Concurrency {
                limit,
                duration,
                body,
            } => {
                let limit = self.positive_integer_literal(limit, "`concurrency` limit")?;
                let duration = self.duration_literal(duration, "`concurrency` duration")?;
                if limit > 100_000 {
                    self.errors.push(CompileError::new(
                        "`concurrency` supports at most 100,000 active iterations",
                        expression.span,
                    ));
                    return None;
                }
                let body = self.compile_expression(current_flow, body, locals, context)?;
                (
                    PlanExpressionKind::Concurrency {
                        limit,
                        duration,
                        body: Box::new(body),
                    },
                    ValueType::Object,
                )
            }
            _ => unreachable!("only policy expressions are delegated"),
        };
        Some(PlanExpression {
            kind,
            value_type,
            span: expression.span,
        })
    }

    fn positive_integer_literal(&mut self, expression: &Expression, label: &str) -> Option<usize> {
        let ExpressionKind::Integer(value) = expression.kind else {
            self.errors.push(CompileError::new(
                format!("{label} must be a positive integer literal"),
                expression.span,
            ));
            return None;
        };
        usize::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .or_else(|| {
                self.errors.push(CompileError::new(
                    format!("{label} must be greater than zero"),
                    expression.span,
                ));
                None
            })
    }

    fn duration_literal(&mut self, expression: &Expression, label: &str) -> Option<Duration> {
        let ExpressionKind::DurationNanos(value) = expression.kind else {
            self.errors.push(CompileError::new(
                format!("{label} must be a duration literal"),
                expression.span,
            ));
            return None;
        };
        if value == 0 {
            self.errors.push(CompileError::new(
                format!("{label} must be greater than zero"),
                expression.span,
            ));
            return None;
        }
        Some(Duration::from_nanos(value))
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    fn compile_call(
        &mut self,
        current_flow: Option<usize>,
        callee: &mettle_syntax::Spanned<String>,
        arguments: &[Expression],
        options: &[ObjectField],
        span: Span,
        locals: &HashMap<String, usize>,
        context: Option<usize>,
    ) -> Option<PlanExpression> {
        if matches!(callee.value.as_str(), "env" | "senv") {
            if !options.is_empty() || arguments.len() != 1 {
                self.errors.push(CompileError::new(
                    format!(
                        "`{}` expects exactly one string argument and no option block",
                        callee.value
                    ),
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
            let environment = PlanExpression {
                kind: PlanExpressionKind::Environment(name.clone()),
                value_type: ValueType::String,
                span,
            };
            return Some(if callee.value == "senv" {
                PlanExpression {
                    kind: PlanExpressionKind::Sensitive(Box::new(environment)),
                    value_type: ValueType::String,
                    span,
                }
            } else {
                environment
            });
        }

        if callee.value == "secret" {
            if !options.is_empty() || arguments.len() != 1 {
                self.errors.push(CompileError::new(
                    "`secret` expects exactly one argument and no option block",
                    span,
                ));
                return None;
            }
            let value = self.compile_expression(current_flow, &arguments[0], locals, context)?;
            let value_type = value.value_type;
            return Some(PlanExpression {
                kind: PlanExpressionKind::Sensitive(Box::new(value)),
                value_type,
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
                let message = with_name_suggestion(
                    format!("capability `{capability_name}` has no operation `{operation_name}`"),
                    operation_name,
                    capability.operations.iter().map(|operation| operation.name),
                );
                self.errors.push(CompileError::new(message, callee.span));
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
            for group in operation.mutually_exclusive {
                let present = options
                    .iter()
                    .filter(|option| group.contains(&option.name.value.as_str()))
                    .collect::<Vec<_>>();
                if present.len() > 1 {
                    self.errors.push(CompileError::new(
                        format!(
                            "options {} cannot be used together",
                            present
                                .iter()
                                .map(|option| format!("`{}`", option.name.value))
                                .collect::<Vec<_>>()
                                .join(" and ")
                        ),
                        present[1].name.span,
                    ));
                }
            }
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
                value_type: schema_value_type(operation.result),
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
            kind: PlanExpressionKind::MettleCall {
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
                    let message = with_name_suggestion(
                        format!("unknown option `{}`", field.name.value),
                        &field.name.value,
                        schema.into_iter().flatten().map(|candidate| candidate.name),
                    );
                    self.errors
                        .push(CompileError::new(message, field.name.span));
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
            SchemaType::Bytes => value.value_type == ValueType::Bytes,
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
