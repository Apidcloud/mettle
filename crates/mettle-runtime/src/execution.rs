//! Flow execution and runtime orchestration.

use super::{
    ActiveContext, ActiveIteration, Arc, AssertionFailure, Capability, Clock, Constant,
    ContextPlan, DeclarationKind, Duration, ExecutionEvent, ExecutionEventKind, ExecutionObserver,
    ExecutionPlan, ExecutionScope, Future, HashMap, Instant, Instruction, MAX_CALL_DEPTH,
    MettlePlan, NoopObserver, Object, OperationEvent, Pin, PlanExpression, PlanExpressionKind,
    PlanField, Poll, RateSettings, RuntimeError, Span, StringPart, TokioClock, Value,
    WORKLOAD_DRAIN_TIMEOUT, WORKLOAD_PROGRESS_INTERVAL, WorkloadEvent, WorkloadMetrics,
    WorkloadPhase, WorkloadPolicy, cooperative_yield, evaluate_binary, format_duration,
    process_environment,
};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Runtime {
    capabilities: Vec<Arc<dyn Capability>>,
    clock: Arc<dyn Clock>,
    observer: Arc<dyn ExecutionObserver>,
    environment: Arc<HashMap<String, String>>,
}

impl Runtime {
    #[must_use]
    pub fn new(capabilities: Vec<Arc<dyn Capability>>) -> Self {
        Self {
            capabilities,
            clock: Arc::new(TokioClock),
            observer: Arc::new(NoopObserver),
            environment: Arc::new(process_environment()),
        }
    }

    #[must_use]
    pub fn with_clock(capabilities: Vec<Arc<dyn Capability>>, clock: Arc<dyn Clock>) -> Self {
        Self {
            capabilities,
            clock,
            observer: Arc::new(NoopObserver),
            environment: Arc::new(process_environment()),
        }
    }

    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn ExecutionObserver>) -> Self {
        self.observer = observer;
        self
    }

    #[must_use]
    pub fn with_environment(mut self, environment: Arc<HashMap<String, String>>) -> Self {
        self.environment = environment;
        self
    }

    /// Execute the program's default flow, or its only flow when unambiguous.
    ///
    /// # Errors
    ///
    /// Returns a source-aware runtime or capability error.
    pub async fn execute(&self, plan: &ExecutionPlan) -> Result<Value, RuntimeError> {
        let flow = plan.default_flow.or_else(|| {
            let mut flows = plan
                .flows
                .iter()
                .enumerate()
                .filter(|(_, flow)| flow.kind == DeclarationKind::Flow);
            let (id, _) = flows.next()?;
            flows.next().is_none().then_some(id)
        });
        let Some(flow) = flow else {
            return Err(RuntimeError {
                message: "execution requires an explicitly selected flow".to_owned(),
                span: Span::default(),
                flow_stack: Vec::new(),
                scope_path: Vec::new().into_boxed_slice(),
                assertions: Vec::new(),
                assertion_only: false,
            });
        };
        self.execute_selected(plan, flow, Vec::new()).await
    }

    /// Execute one resolved flow with the supplied argument values.
    ///
    /// # Errors
    ///
    /// Returns a source-aware runtime or capability error.
    pub async fn execute_selected(
        &self,
        plan: &ExecutionPlan,
        flow: usize,
        arguments: Vec<Value>,
    ) -> Result<Value, RuntimeError> {
        self.validate_capabilities(plan)?;
        Executor {
            plan,
            capabilities: &self.capabilities,
            clock: self.clock.as_ref(),
            observer: self.observer.as_ref(),
            environment: &self.environment,
            inside_workload: false,
            flow_stack: Vec::new(),
            scope_path: Vec::new(),
            next_scope_id: Arc::new(AtomicU64::new(1)),
            secrets: Vec::new(),
        }
        .execute_flow(flow, arguments, ActiveContext::default())
        .await
    }

    fn validate_capabilities(&self, plan: &ExecutionPlan) -> Result<(), RuntimeError> {
        if plan.capability_names.len() != self.capabilities.len() {
            return Err(RuntimeError {
                message: "execution plan and runtime have different capability sets".to_owned(),
                span: Span::default(),
                flow_stack: Vec::new(),
                scope_path: Vec::new().into_boxed_slice(),
                assertions: Vec::new(),
                assertion_only: false,
            });
        }
        for (expected, actual) in plan.capability_names.iter().zip(&self.capabilities) {
            if expected != actual.name() {
                return Err(RuntimeError {
                    message: format!(
                        "execution plan expected capability `{expected}`, found `{}`",
                        actual.name()
                    ),
                    span: Span::default(),
                    flow_stack: Vec::new(),
                    scope_path: Vec::new().into_boxed_slice(),
                    assertions: Vec::new(),
                    assertion_only: false,
                });
            }
        }
        Ok(())
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

#[derive(Clone)]
struct Executor<'a> {
    plan: &'a ExecutionPlan,
    capabilities: &'a [Arc<dyn Capability>],
    clock: &'a dyn Clock,
    observer: &'a dyn ExecutionObserver,
    environment: &'a HashMap<String, String>,
    inside_workload: bool,
    flow_stack: Vec<String>,
    scope_path: Vec<ExecutionScope>,
    next_scope_id: Arc<AtomicU64>,
    secrets: Vec<String>,
}

impl Executor<'_> {
    fn execute_flow<'b>(
        &'b mut self,
        flow_id: usize,
        arguments: Vec<Value>,
        inherited_context: ActiveContext,
    ) -> Pin<Box<dyn Future<Output = Result<Value, RuntimeError>> + Send + 'b>> {
        Box::pin(async move {
            if self.flow_stack.len() >= MAX_CALL_DEPTH {
                return Err(self.error("maximum flow call depth exceeded", Span::default()));
            }
            let Some(flow) = self.plan.flows.get(flow_id) else {
                return Err(
                    self.error("execution plan references an unknown flow", Span::default())
                );
            };
            if arguments.len() != flow.parameters.len() {
                return Err(self.error(
                    format!(
                        "execution plan passed {} argument(s) to `{}`, which expects {}",
                        arguments.len(),
                        flow.display_name,
                        flow.parameters.len()
                    ),
                    Span::default(),
                ));
            }

            self.flow_stack.push(flow.display_name.clone());
            let result = async {
                let context = self.activate_context(flow, inherited_context).await?;
                self.execute_instructions(flow, arguments, context).await
            }
            .await;
            self.flow_stack.pop();
            result
        })
    }

    async fn activate_context(
        &mut self,
        flow: &MettlePlan,
        mut active: ActiveContext,
    ) -> Result<ActiveContext, RuntimeError> {
        for context_id in &flow.contexts {
            let Some(context) = self.plan.contexts.get(*context_id) else {
                return Err(self.error(
                    "execution plan references an unknown context",
                    Span::default(),
                ));
            };
            active = self.apply_context(context, active).await?;
        }
        Ok(active)
    }

    async fn apply_context(
        &mut self,
        context: &ContextPlan,
        inherited: ActiveContext,
    ) -> Result<ActiveContext, RuntimeError> {
        let mut active = ActiveContext {
            values: Vec::with_capacity(context.fields.len()),
            defaults: inherited.defaults,
        };
        active
            .defaults
            .resize_with(self.capabilities.len(), Object::new);

        for field in &context.fields {
            let value = self.evaluate(&field.expression, &[], &active).await?;
            active.values.push(value);
        }
        for defaults in &context.defaults {
            let values = self.evaluate_fields(&defaults.fields, &[], &active).await?;
            let Some(current) = active.defaults.get_mut(defaults.capability) else {
                return Err(self.error(
                    "execution plan references an unknown capability default",
                    Span::default(),
                ));
            };
            let Some(capability) = self.capabilities.get(defaults.capability) else {
                return Err(self.error(
                    "execution plan references an unknown capability",
                    Span::default(),
                ));
            };
            capability.merge_options(current, values);
        }
        Ok(active)
    }

    async fn execute_instructions(
        &mut self,
        flow: &MettlePlan,
        arguments: Vec<Value>,
        context: ActiveContext,
    ) -> Result<Value, RuntimeError> {
        let mut locals = vec![None; flow.local_count];
        let mut assertions = Vec::new();
        for (slot, argument) in arguments.into_iter().enumerate() {
            locals[slot] = Some(argument);
        }

        let returned = self
            .run_instructions(
                &flow.instructions,
                &mut locals,
                &context,
                &mut assertions,
                flow.kind == DeclarationKind::Test,
            )
            .await?;
        if let Some(value) = returned {
            return Ok(value);
        }

        if flow.kind == DeclarationKind::Test {
            if let Some(first) = assertions.first() {
                let message = if assertions.len() == 1 {
                    first.message.clone()
                } else {
                    format!("{} assertions failed", assertions.len())
                };
                let mut error = self.error(message, first.span);
                error.assertions = assertions;
                error.assertion_only = true;
                return Err(error);
            }
            return Ok(Value::Null);
        }
        Err(self.error(
            format!(
                "flow `{}` completed without returning a value",
                flow.display_name
            ),
            Span::default(),
        ))
    }

    #[allow(clippy::too_many_lines)]
    fn run_instructions<'b>(
        &'b mut self,
        instructions: &'b [Instruction],
        locals: &'b mut [Option<Value>],
        context: &'b ActiveContext,
        assertions: &'b mut Vec<AssertionFailure>,
        collect_assertions: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, RuntimeError>> + Send + 'b>> {
        Box::pin(async move {
            for instruction in instructions {
                match instruction {
                    Instruction::Echo { value, span } => {
                        let value = self
                            .evaluate(value, locals, context)
                            .await
                            .map_err(|error| error.with_assertions(assertions))?;
                        self.observer.execution_event(ExecutionEvent {
                            kind: ExecutionEventKind::Echo(value),
                            scope_path: self.scope_path.clone(),
                            span: *span,
                            inside_workload: self.inside_workload,
                        });
                    }
                    Instruction::If {
                        branches,
                        else_body,
                    } => {
                        let mut selected = None;
                        for branch in branches {
                            let value = self
                                .evaluate(&branch.condition, locals, context)
                                .await
                                .map_err(|error| error.with_assertions(assertions))?;
                            match value.revealed() {
                                Value::Boolean(true) => {
                                    selected = Some(branch.instructions.as_slice());
                                    break;
                                }
                                Value::Boolean(false) => {}
                                _ => {
                                    return Err(self
                                        .error(
                                            format!(
                                                "if condition produced {}, expected boolean",
                                                value.type_name()
                                            ),
                                            branch.condition.span,
                                        )
                                        .with_assertions(assertions));
                                }
                            }
                        }
                        if let Some(body) = selected.or(else_body.as_deref())
                            && let Some(value) = self
                                .run_instructions(
                                    body,
                                    locals,
                                    context,
                                    assertions,
                                    collect_assertions,
                                )
                                .await?
                        {
                            return Ok(Some(value));
                        }
                    }
                    Instruction::Bind { slot, expression } => {
                        let value = self
                            .evaluate(expression, locals, context)
                            .await
                            .map_err(|error| error.with_assertions(assertions))?;
                        let Some(destination) = locals.get_mut(*slot) else {
                            return Err(self
                                .error(
                                    "execution plan references an invalid local slot",
                                    expression.span,
                                )
                                .with_assertions(assertions));
                        };
                        *destination = Some(value);
                    }
                    Instruction::Evaluate(expression) => {
                        self.evaluate(expression, locals, context)
                            .await
                            .map_err(|error| error.with_assertions(assertions))?;
                    }
                    Instruction::Assert { condition, message } => {
                        if let Some(failure) = self
                            .evaluate_assertion(
                                condition,
                                message.as_ref(),
                                locals,
                                context,
                                collect_assertions,
                            )
                            .await
                            .map_err(|error| error.with_assertions(assertions))?
                        {
                            assertions.push(failure);
                        }
                    }
                    Instruction::Return(expression) => {
                        return self
                            .evaluate(expression, locals, context)
                            .await
                            .map(Some)
                            .map_err(|error| error.with_assertions(assertions));
                    }
                }
            }

            Ok(None)
        })
    }

    async fn evaluate_assertion(
        &mut self,
        condition: &PlanExpression,
        message: Option<&PlanExpression>,
        locals: &[Option<Value>],
        context: &ActiveContext,
        collect: bool,
    ) -> Result<Option<AssertionFailure>, RuntimeError> {
        let value = self.evaluate(condition, locals, context).await?;
        match value.revealed() {
            Value::Boolean(true) => Ok(None),
            Value::Boolean(false) => {
                let message = if let Some(message) = message {
                    let value = self.evaluate(message, locals, context).await?;
                    match value.revealed() {
                        Value::String(_) if value.is_sensitive() => "[REDACTED]".to_owned(),
                        Value::String(text) => text.clone(),
                        _ => {
                            return Err(self.error(
                                "assertion message produced a non-string value",
                                message.span,
                            ));
                        }
                    }
                } else {
                    "assertion failed".to_owned()
                };
                let error = self.error(message, condition.span);
                if collect {
                    Ok(Some(AssertionFailure {
                        message: error.message,
                        span: condition.span,
                    }))
                } else {
                    Err(error)
                }
            }
            _ => Err(self.error(
                format!("assertion produced {}, expected boolean", value.type_name()),
                condition.span,
            )),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn evaluate<'b>(
        &'b mut self,
        expression: &'b PlanExpression,
        locals: &'b [Option<Value>],
        context: &'b ActiveContext,
    ) -> Pin<Box<dyn Future<Output = Result<Value, RuntimeError>> + Send + 'b>> {
        Box::pin(async move {
            match &expression.kind {
                PlanExpressionKind::Constant(constant) => Ok(match constant {
                    Constant::Null => Value::Null,
                    Constant::Boolean(value) => Value::Boolean(*value),
                    Constant::Integer(value) => Value::Integer(*value),
                    Constant::Float(value) => Value::Float(*value),
                    Constant::String(value) => Value::String(value.clone()),
                    Constant::DurationNanos(value) => {
                        Value::Duration(std::time::Duration::from_nanos(*value))
                    }
                }),
                PlanExpressionKind::Local(slot) => {
                    locals.get(*slot).and_then(Clone::clone).ok_or_else(|| {
                        self.error(
                            "execution plan read an uninitialized local value",
                            expression.span,
                        )
                    })
                }
                PlanExpressionKind::Context(slot) => {
                    context.values.get(*slot).cloned().ok_or_else(|| {
                        self.error(
                            "execution plan read an uninitialized context value",
                            expression.span,
                        )
                    })
                }
                PlanExpressionKind::Environment(name) => self
                    .environment
                    .get(name)
                    .cloned()
                    .map(Value::String)
                    .ok_or_else(|| {
                        self.error(
                            format!("required environment variable `{name}` is not set"),
                            expression.span,
                        )
                    }),
                PlanExpressionKind::Sensitive(value) => {
                    Ok(self.evaluate(value, locals, context).await?.sensitive())
                }
                PlanExpressionKind::Array(values) => {
                    let mut result = Vec::with_capacity(values.len());
                    for value in values {
                        result.push(self.evaluate(value, locals, context).await?);
                    }
                    Ok(Value::Array(result))
                }
                PlanExpressionKind::Object(fields) => self
                    .evaluate_fields(fields, locals, context)
                    .await
                    .map(Value::Object),
                PlanExpressionKind::Member { value, member } => {
                    let value = self.evaluate(value, locals, context).await?;
                    let inherited_sensitivity = value.is_sensitive();
                    let Value::Object(fields) = value.revealed() else {
                        return Err(self.error(
                            format!("cannot read member `{member}` from {}", value.type_name()),
                            expression.span,
                        ));
                    };
                    fields
                        .get(member)
                        .cloned()
                        .map(|value| {
                            if inherited_sensitivity {
                                value.sensitive()
                            } else {
                                value
                            }
                        })
                        .ok_or_else(|| {
                            self.error(format!("object has no member `{member}`"), expression.span)
                        })
                }
                PlanExpressionKind::Index { value, index } => {
                    let value = self.evaluate(value, locals, context).await?;
                    let index = self.evaluate(index, locals, context).await?;
                    let inherited_sensitivity = value.is_sensitive() || index.is_sensitive();
                    let result = match (value.revealed(), index.revealed()) {
                        (Value::Object(fields), Value::String(key)) => {
                            fields.get(key).cloned().ok_or_else(|| {
                                self.error(format!("object has no member `{key}`"), expression.span)
                            })?
                        }
                        (Value::Array(values), Value::Integer(position)) if *position >= 0 => {
                            usize::try_from(*position)
                                .ok()
                                .and_then(|position| values.get(position))
                                .cloned()
                                .ok_or_else(|| {
                                    self.error(
                                        format!("array index {position} is out of range"),
                                        expression.span,
                                    )
                                })?
                        }
                        (Value::Array(_), Value::Integer(position)) => {
                            return Err(self.error(
                                format!("array index {position} must be non-negative"),
                                expression.span,
                            ));
                        }
                        (Value::Object(_), _) => {
                            return Err(self.error(
                                format!(
                                    "object index must be a string, found {}",
                                    index.type_name()
                                ),
                                expression.span,
                            ));
                        }
                        (Value::Array(_), _) => {
                            return Err(self.error(
                                format!(
                                    "array index must be an integer, found {}",
                                    index.type_name()
                                ),
                                expression.span,
                            ));
                        }
                        _ => {
                            return Err(self.error(
                                format!("cannot index {}", value.type_name()),
                                expression.span,
                            ));
                        }
                    };
                    Ok(if inherited_sensitivity {
                        result.sensitive()
                    } else {
                        result
                    })
                }
                PlanExpressionKind::Not(value) => {
                    let value = self.evaluate(value, locals, context).await?;
                    match value.revealed() {
                        Value::Boolean(value) => Ok(Value::Boolean(!value)),
                        _ => Err(self.error(
                            format!("`not` requires boolean, found {}", value.type_name()),
                            expression.span,
                        )),
                    }
                }
                PlanExpressionKind::Binary {
                    left,
                    operator,
                    right,
                } => {
                    let left = self.evaluate(left, locals, context).await?;
                    if matches!(
                        operator,
                        super::BinaryOperator::And | super::BinaryOperator::Or
                    ) {
                        let Value::Boolean(left_value) = left.revealed() else {
                            return Err(self.error(
                                format!(
                                    "logical operator requires boolean, found {}",
                                    left.type_name()
                                ),
                                expression.span,
                            ));
                        };
                        if (*operator == super::BinaryOperator::And && !left_value)
                            || (*operator == super::BinaryOperator::Or && *left_value)
                        {
                            return Ok(Value::Boolean(*left_value));
                        }
                        let right = self.evaluate(right, locals, context).await?;
                        let Value::Boolean(right_value) = right.revealed() else {
                            return Err(self.error(
                                format!(
                                    "logical operator requires boolean, found {}",
                                    right.type_name()
                                ),
                                expression.span,
                            ));
                        };
                        return Ok(Value::Boolean(*right_value));
                    }
                    let right = self.evaluate(right, locals, context).await?;
                    evaluate_binary(&left, *operator, &right)
                        .map(Value::Boolean)
                        .map_err(|message| self.error(message, expression.span))
                }
                PlanExpressionKind::InterpolatedString(parts) => {
                    let mut result = String::new();
                    let mut sensitive = false;
                    for part in parts {
                        match part {
                            StringPart::Text(value) => result.push_str(value),
                            StringPart::Value(value) => {
                                let value = self.evaluate(value, locals, context).await?;
                                sensitive |= value.contains_sensitive();
                                result.push_str(&value.exposed_string());
                            }
                        }
                    }
                    let result = Value::String(result);
                    Ok(if sensitive {
                        result.sensitive()
                    } else {
                        result
                    })
                }
                PlanExpressionKind::MettleCall { flow, arguments } => {
                    let mut values = Vec::with_capacity(arguments.len());
                    for argument in arguments {
                        values.push(self.evaluate(argument, locals, context).await?);
                    }
                    self.execute_flow(*flow, values, context.clone()).await
                }
                PlanExpressionKind::CapabilityCall {
                    capability,
                    operation,
                    arguments,
                    options,
                } => {
                    let mut values = Vec::with_capacity(arguments.len());
                    for argument in arguments {
                        values.push(self.evaluate(argument, locals, context).await?);
                    }
                    let mut effective = context
                        .defaults
                        .get(*capability)
                        .cloned()
                        .unwrap_or_default();
                    let local = self.evaluate_fields(options, locals, context).await?;
                    let Some(capability) = self.capabilities.get(*capability) else {
                        return Err(self.error(
                            "execution plan references an unknown capability",
                            expression.span,
                        ));
                    };
                    capability.merge_options(&mut effective, local);
                    for value in values.iter().chain(effective.values()) {
                        value.visit_sensitive_strings(&mut |secret| {
                            if !secret.is_empty() && !self.secrets.iter().any(|seen| seen == secret)
                            {
                                self.secrets.push(secret.to_owned());
                            }
                        });
                    }
                    let capability_name = capability.name();
                    let operation_name = capability.operation_name(*operation);
                    let started = self.clock.now();
                    let result = capability
                        .invoke(*operation, values, effective, expression.span)
                        .await;
                    let duration = self
                        .clock
                        .now()
                        .checked_duration_since(started)
                        .unwrap_or_default();
                    match result {
                        Ok(value) => {
                            let report = capability.report(*operation, &value);
                            if !self.inside_workload {
                                self.observer.execution_event(ExecutionEvent {
                                    kind: ExecutionEventKind::Operation(OperationEvent {
                                        capability: capability_name.to_owned(),
                                        operation: operation_name.to_owned(),
                                        duration,
                                        span: expression.span,
                                        result: Ok(value.clone()),
                                        report,
                                    }),
                                    scope_path: self.scope_path.clone(),
                                    span: expression.span,
                                    inside_workload: false,
                                });
                            }
                            Ok(value)
                        }
                        Err(error) => {
                            let error = self.error(error.message, error.span);
                            if !self.inside_workload {
                                self.observer.execution_event(ExecutionEvent {
                                    kind: ExecutionEventKind::Operation(OperationEvent {
                                        capability: capability_name.to_owned(),
                                        operation: operation_name.to_owned(),
                                        duration,
                                        span: expression.span,
                                        result: Err(error.message.clone()),
                                        report: None,
                                    }),
                                    scope_path: self.scope_path.clone(),
                                    span: expression.span,
                                    inside_workload: false,
                                });
                            }
                            Err(error)
                        }
                    }
                }
                PlanExpressionKind::Within { timeout, body } => {
                    let timeout_error = self.error(
                        format!("deadline exceeded after {}", format_duration(*timeout)),
                        expression.span,
                    );
                    let mut timer = self.clock.sleep(*timeout);
                    let mut work = self.evaluate(body, locals, context);
                    std::future::poll_fn(|task| {
                        if let Poll::Ready(result) = work.as_mut().poll(task) {
                            return Poll::Ready(result);
                        }
                        if timer.as_mut().poll(task).is_ready() {
                            return Poll::Ready(Err(timeout_error.clone()));
                        }
                        Poll::Pending
                    })
                    .await
                }
                PlanExpressionKind::Retry {
                    attempts,
                    delay,
                    body,
                } => {
                    let invocation = self.next_scope_id.fetch_add(1, Ordering::Relaxed);
                    let mut last_error = None;
                    for attempt in 0..*attempts {
                        let mut scoped = self.clone();
                        scoped.scope_path.push(ExecutionScope::Retry {
                            invocation,
                            attempt: attempt + 1,
                            total: *attempts,
                        });
                        let result = scoped.evaluate(body, locals, context).await;
                        self.secrets = scoped.secrets;
                        match result {
                            Ok(value) => return Ok(value),
                            Err(error) => last_error = Some(error),
                        }
                        if attempt + 1 < *attempts && !delay.is_zero() {
                            self.clock.sleep(*delay).await;
                        }
                    }
                    let mut error = last_error.expect("retry always has at least one attempt");
                    error.message = format!(
                        "retry exhausted after {attempts} attempts: {}",
                        error.message
                    );
                    Err(error)
                }
                PlanExpressionKind::Parallel { limit, branches } => {
                    self.evaluate_parallel(branches, *limit, locals, context)
                        .await
                }
                PlanExpressionKind::Rate {
                    target,
                    period,
                    duration,
                    limit,
                    body,
                } => {
                    self.evaluate_rate(
                        body,
                        RateSettings {
                            target: *target,
                            period: *period,
                            duration: *duration,
                            limit: *limit,
                        },
                        locals,
                        context,
                    )
                    .await
                }
                PlanExpressionKind::Concurrency {
                    limit,
                    duration,
                    body,
                } => {
                    self.evaluate_concurrency(body, *limit, *duration, locals, context)
                        .await
                }
            }
        })
    }

    async fn evaluate_parallel(
        &mut self,
        branches: &[PlanExpression],
        limit: usize,
        locals: &[Option<Value>],
        context: &ActiveContext,
    ) -> Result<Value, RuntimeError> {
        let invocation = self.next_scope_id.fetch_add(1, Ordering::Relaxed);
        let mut results = vec![None; branches.len()];
        let mut futures = Vec::with_capacity(limit);
        let mut next = 0;
        while next < branches.len() || !futures.is_empty() {
            while next < branches.len() && futures.len() < limit {
                let index = next;
                let branch = &branches[index];
                let mut executor = self.clone();
                executor.scope_path.push(ExecutionScope::Parallel {
                    invocation,
                    branch: index + 1,
                    total: branches.len(),
                });
                let locals = locals.to_vec();
                let context = context.clone();
                let future: Pin<Box<dyn Future<Output = Result<Value, RuntimeError>> + Send + '_>> =
                    Box::pin(async move { executor.evaluate(branch, &locals, &context).await });
                futures.push((index, future));
                next += 1;
            }
            let (completed, result) = std::future::poll_fn(|task| {
                for (position, (_, future)) in futures.iter_mut().enumerate() {
                    if let Poll::Ready(result) = future.as_mut().poll(task) {
                        return Poll::Ready((position, result));
                    }
                }
                Poll::Pending
            })
            .await;
            let (index, _) = futures.swap_remove(completed);
            match result {
                Ok(value) => results[index] = Some(value),
                Err(error) => {
                    if !self.inside_workload {
                        for (cancelled, _) in &futures {
                            let mut scope_path = self.scope_path.clone();
                            scope_path.push(ExecutionScope::Parallel {
                                invocation,
                                branch: cancelled + 1,
                                total: branches.len(),
                            });
                            self.observer.execution_event(ExecutionEvent {
                                kind: ExecutionEventKind::BranchCancelled,
                                scope_path,
                                span: branches[*cancelled].span,
                                inside_workload: false,
                            });
                        }
                    }
                    return Err(error);
                }
            }
        }
        Ok(Value::Array(
            results
                .into_iter()
                .map(|value| value.expect("every parallel branch completed"))
                .collect(),
        ))
    }

    async fn evaluate_rate(
        &mut self,
        body: &PlanExpression,
        settings: RateSettings,
        locals: &[Option<Value>],
        context: &ActiveContext,
    ) -> Result<Value, RuntimeError> {
        let planned = usize::try_from(
            settings.duration.as_nanos() * settings.target as u128 / settings.period.as_nanos(),
        )
        .expect("compiler bounds rate iteration count");
        let window_start = self.clock.now();
        let mut metrics = WorkloadMetrics::new(window_start);
        let mut active = Vec::with_capacity(settings.limit.min(planned));
        let policy = WorkloadPolicy::Rate {
            target: settings.target,
            period: settings.period,
            limit: settings.limit,
            window: settings.duration,
            planned,
        };
        let mut next_progress = window_start;
        self.report_workload(
            &metrics,
            &policy,
            active.len(),
            WorkloadPhase::Starting,
            &mut next_progress,
            true,
        );

        for index in 0..planned {
            let offset_nanos = settings.period.as_nanos() * index as u128 / settings.target as u128;
            let offset = Duration::from_nanos(
                u64::try_from(offset_nanos).expect("rate offset is within configured duration"),
            );
            let intended = window_start + offset;
            self.wait_until(
                intended,
                &mut active,
                &mut metrics,
                &policy,
                &mut next_progress,
            )
            .await;
            self.collect_ready(&mut active, &mut metrics).await;
            self.report_workload(
                &metrics,
                &policy,
                active.len(),
                WorkloadPhase::Running,
                &mut next_progress,
                false,
            );
            if index.is_multiple_of(1_024) {
                cooperative_yield().await;
            }

            if active.len() == settings.limit {
                metrics.dropped += 1;
                continue;
            }
            metrics.scheduling_delay.record(
                self.clock
                    .now()
                    .checked_duration_since(intended)
                    .unwrap_or_default(),
            );
            active.push(self.start_workload_iteration(body, locals, context));
            metrics.started += 1;
        }

        self.report_workload(
            &metrics,
            &policy,
            active.len(),
            WorkloadPhase::Draining,
            &mut next_progress,
            true,
        );
        let drain_timed_out = self
            .drain_workload(&mut active, &mut metrics, &policy, &mut next_progress)
            .await;
        self.report_workload(
            &metrics,
            &policy,
            active.len(),
            WorkloadPhase::Completed,
            &mut next_progress,
            true,
        );
        Ok(metrics.into_value(policy, self.clock.now(), drain_timed_out))
    }

    async fn evaluate_concurrency(
        &mut self,
        body: &PlanExpression,
        limit: usize,
        duration: Duration,
        locals: &[Option<Value>],
        context: &ActiveContext,
    ) -> Result<Value, RuntimeError> {
        let window_start = self.clock.now();
        let deadline = window_start + duration;
        let mut metrics = WorkloadMetrics::new(window_start);
        let mut active = Vec::with_capacity(limit);
        let policy = WorkloadPolicy::Concurrency { limit, duration };
        let mut next_progress = window_start;
        self.report_workload(
            &metrics,
            &policy,
            active.len(),
            WorkloadPhase::Starting,
            &mut next_progress,
            true,
        );
        for _ in 0..limit {
            active.push(self.start_workload_iteration(body, locals, context));
            metrics.started += 1;
        }

        loop {
            if self.clock.now() >= deadline {
                break;
            }
            let wake_at = deadline.min(next_progress);
            let completed = self
                .wait_until_or_complete(wake_at, &mut active, &mut metrics)
                .await;
            self.report_workload(
                &metrics,
                &policy,
                active.len(),
                WorkloadPhase::Running,
                &mut next_progress,
                false,
            );
            if self.clock.now() >= deadline {
                break;
            }
            if !completed {
                if self.clock.now() < wake_at {
                    break;
                }
                continue;
            }
            active.push(self.start_workload_iteration(body, locals, context));
            metrics.started += 1;
            self.report_workload(
                &metrics,
                &policy,
                active.len(),
                WorkloadPhase::Running,
                &mut next_progress,
                false,
            );
            if metrics.started.is_multiple_of(64) {
                cooperative_yield().await;
            }
        }

        self.report_workload(
            &metrics,
            &policy,
            active.len(),
            WorkloadPhase::Draining,
            &mut next_progress,
            true,
        );
        let drain_timed_out = self
            .drain_workload(&mut active, &mut metrics, &policy, &mut next_progress)
            .await;
        self.report_workload(
            &metrics,
            &policy,
            active.len(),
            WorkloadPhase::Completed,
            &mut next_progress,
            true,
        );
        Ok(metrics.into_value(policy, self.clock.now(), drain_timed_out))
    }

    fn start_workload_iteration<'b>(
        &'b self,
        body: &'b PlanExpression,
        locals: &'b [Option<Value>],
        context: &'b ActiveContext,
    ) -> ActiveIteration<'b> {
        let mut executor = self.clone();
        executor.inside_workload = true;
        let locals = locals.to_vec();
        let context = context.clone();
        let started = self.clock.now();
        ActiveIteration {
            started,
            future: Box::pin(async move { executor.evaluate(body, &locals, &context).await }),
        }
    }

    fn report_workload(
        &self,
        metrics: &WorkloadMetrics,
        policy: &WorkloadPolicy,
        active: usize,
        phase: WorkloadPhase,
        next_progress: &mut Instant,
        force: bool,
    ) {
        let now = self.clock.now();
        if !force && now < *next_progress {
            return;
        }
        *next_progress = now + WORKLOAD_PROGRESS_INTERVAL;
        self.observer
            .workload_updated(metrics.snapshot(policy, phase, active, now));
    }

    async fn wait_until_or_complete(
        &self,
        deadline: Instant,
        active: &mut Vec<ActiveIteration<'_>>,
        metrics: &mut WorkloadMetrics,
    ) -> bool {
        let now = self.clock.now();
        if now >= deadline {
            return false;
        }
        let mut timer = self.clock.sleep(deadline.duration_since(now));
        let event = std::future::poll_fn(|task| {
            for (position, iteration) in active.iter_mut().enumerate() {
                if let Poll::Ready(result) = iteration.future.as_mut().poll(task) {
                    return Poll::Ready(WorkloadEvent::Completed(position, result));
                }
            }
            if timer.as_mut().poll(task).is_ready() {
                return Poll::Ready(WorkloadEvent::Deadline);
            }
            Poll::Pending
        })
        .await;
        match event {
            WorkloadEvent::Completed(position, result) => {
                let iteration = active.swap_remove(position);
                metrics.record_completion(iteration.started, self.clock.now(), result.is_ok());
                true
            }
            WorkloadEvent::Deadline => false,
        }
    }

    async fn wait_until(
        &self,
        deadline: Instant,
        active: &mut Vec<ActiveIteration<'_>>,
        metrics: &mut WorkloadMetrics,
        policy: &WorkloadPolicy,
        next_progress: &mut Instant,
    ) {
        while self.clock.now() < deadline {
            let wake_at = deadline.min(*next_progress);
            let completed = self.wait_until_or_complete(wake_at, active, metrics).await;
            self.report_workload(
                metrics,
                policy,
                active.len(),
                WorkloadPhase::Running,
                next_progress,
                false,
            );
            if !completed && self.clock.now() < wake_at {
                break;
            }
        }
    }

    async fn collect_ready(
        &self,
        active: &mut Vec<ActiveIteration<'_>>,
        metrics: &mut WorkloadMetrics,
    ) {
        loop {
            let ready = std::future::poll_fn(|task| {
                for (position, iteration) in active.iter_mut().enumerate() {
                    if let Poll::Ready(result) = iteration.future.as_mut().poll(task) {
                        return Poll::Ready(Some((position, result)));
                    }
                }
                Poll::Ready(None)
            })
            .await;
            let Some((position, result)) = ready else {
                break;
            };
            let iteration = active.swap_remove(position);
            metrics.record_completion(iteration.started, self.clock.now(), result.is_ok());
        }
    }

    async fn drain_workload(
        &self,
        active: &mut Vec<ActiveIteration<'_>>,
        metrics: &mut WorkloadMetrics,
        policy: &WorkloadPolicy,
        next_progress: &mut Instant,
    ) -> bool {
        let deadline = self.clock.now() + WORKLOAD_DRAIN_TIMEOUT;
        while !active.is_empty() {
            let wake_at = deadline.min(*next_progress);
            let completed = self.wait_until_or_complete(wake_at, active, metrics).await;
            self.report_workload(
                metrics,
                policy,
                active.len(),
                WorkloadPhase::Draining,
                next_progress,
                false,
            );
            if !completed && (self.clock.now() >= deadline || self.clock.now() < wake_at) {
                let now = self.clock.now();
                for iteration in active.drain(..) {
                    metrics.record_completion(iteration.started, now, false);
                }
                return true;
            }
        }
        false
    }

    async fn evaluate_fields(
        &mut self,
        fields: &[PlanField],
        locals: &[Option<Value>],
        context: &ActiveContext,
    ) -> Result<Object, RuntimeError> {
        let mut values = Object::new();
        for field in fields {
            let value = self.evaluate(&field.expression, locals, context).await?;
            values.insert(field.name.clone(), value);
        }
        Ok(values)
    }

    fn error(&self, message: impl Into<String>, span: Span) -> RuntimeError {
        let mut message = message.into();
        for secret in &self.secrets {
            message = message.replace(secret, "[REDACTED]");
        }
        RuntimeError {
            message,
            span,
            flow_stack: self.flow_stack.clone(),
            scope_path: self.scope_path.clone().into_boxed_slice(),
            assertions: Vec::new(),
            assertion_only: false,
        }
    }
}

impl RuntimeError {
    fn with_assertions(mut self, assertions: &[AssertionFailure]) -> Self {
        self.assertions.extend_from_slice(assertions);
        self
    }
}
