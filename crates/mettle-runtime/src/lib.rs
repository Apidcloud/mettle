//! Asynchronous execution of validated Mettle plans.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};
use std::{env, fmt};

use mettle_capability::{Capability, Object, Span, Value};
use mettle_compiler::{
    BinaryOperator, Constant, ContextPlan, ExecutionPlan, Instruction, MettlePlan, PlanExpression,
    PlanExpressionKind, PlanField, StringPart,
};

const MAX_CALL_DEPTH: usize = 1_024;
const WORKLOAD_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const HISTOGRAM_SUB_BUCKETS: usize = 64;
const HISTOGRAM_BUCKETS: usize = 1 + 64 * HISTOGRAM_SUB_BUCKETS;
const WORKLOAD_PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

pub type ClockFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

pub trait Clock: Send + Sync {
    fn sleep(&self, duration: Duration) -> ClockFuture;

    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Debug, Default)]
pub struct TokioClock;

impl Clock for TokioClock {
    fn sleep(&self, duration: Duration) -> ClockFuture {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeError {
    pub message: String,
    pub span: Span,
    pub flow_stack: Vec<String>,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Clone, Debug)]
pub struct OperationEvent {
    pub capability: String,
    pub operation: String,
    pub duration: Duration,
    pub span: Span,
    pub result: Result<Value, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkloadPhase {
    Starting,
    Running,
    Draining,
    Completed,
}

#[derive(Clone, Debug)]
pub enum WorkloadKind {
    Rate {
        target: usize,
        period: Duration,
        duration: Duration,
        limit: usize,
        planned: usize,
    },
    Concurrency {
        limit: usize,
        duration: Duration,
    },
}

#[derive(Clone, Debug)]
pub struct WorkloadSnapshot {
    pub kind: WorkloadKind,
    pub phase: WorkloadPhase,
    pub elapsed: Duration,
    pub active: usize,
    pub started: usize,
    pub completed: usize,
    pub success: usize,
    pub failed: usize,
    pub dropped: usize,
    pub latency_p50: Duration,
    pub latency_p95: Duration,
    pub latency_p99: Duration,
}

pub trait ExecutionObserver: Send + Sync {
    fn operation_completed(&self, _event: OperationEvent) {}

    fn workload_updated(&self, _snapshot: WorkloadSnapshot) {}
}

#[derive(Debug, Default)]
struct NoopObserver;

impl ExecutionObserver for NoopObserver {}

#[derive(Clone, Default)]
struct ActiveContext {
    values: Vec<Value>,
    defaults: Vec<Object>,
}

pub struct Runtime {
    capabilities: Vec<Arc<dyn Capability>>,
    clock: Arc<dyn Clock>,
    observer: Arc<dyn ExecutionObserver>,
}

impl Runtime {
    #[must_use]
    pub fn new(capabilities: Vec<Arc<dyn Capability>>) -> Self {
        Self {
            capabilities,
            clock: Arc::new(TokioClock),
            observer: Arc::new(NoopObserver),
        }
    }

    #[must_use]
    pub fn with_clock(capabilities: Vec<Arc<dyn Capability>>, clock: Arc<dyn Clock>) -> Self {
        Self {
            capabilities,
            clock,
            observer: Arc::new(NoopObserver),
        }
    }

    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn ExecutionObserver>) -> Self {
        self.observer = observer;
        self
    }

    /// Execute the program's default flow, or its only flow when unambiguous.
    ///
    /// # Errors
    ///
    /// Returns a source-aware runtime or capability error.
    pub async fn execute(&self, plan: &ExecutionPlan) -> Result<Value, RuntimeError> {
        let flow = plan
            .default_flow
            .or_else(|| (plan.flows.len() == 1).then_some(0));
        let Some(flow) = flow else {
            return Err(RuntimeError {
                message: "execution requires an explicitly selected flow".to_owned(),
                span: Span::default(),
                flow_stack: Vec::new(),
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
            inside_workload: false,
            flow_stack: Vec::new(),
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
    inside_workload: bool,
    flow_stack: Vec<String>,
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
        for (slot, argument) in arguments.into_iter().enumerate() {
            locals[slot] = Some(argument);
        }

        for instruction in &flow.instructions {
            match instruction {
                Instruction::Bind { slot, expression } => {
                    let value = self.evaluate(expression, &locals, &context).await?;
                    let Some(destination) = locals.get_mut(*slot) else {
                        return Err(self.error(
                            "execution plan references an invalid local slot",
                            expression.span,
                        ));
                    };
                    *destination = Some(value);
                }
                Instruction::Evaluate(expression) => {
                    self.evaluate(expression, &locals, &context).await?;
                }
                Instruction::Assert(expression) => {
                    let value = self.evaluate(expression, &locals, &context).await?;
                    match value {
                        Value::Boolean(true) => {}
                        Value::Boolean(false) => {
                            return Err(self.error("assertion failed", expression.span));
                        }
                        value => {
                            return Err(self.error(
                                format!(
                                    "assertion produced {}, expected boolean",
                                    value.type_name()
                                ),
                                expression.span,
                            ));
                        }
                    }
                }
                Instruction::Return(expression) => {
                    return self.evaluate(expression, &locals, &context).await;
                }
            }
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
                PlanExpressionKind::Environment(name) => env::var(name)
                    .map(|value| {
                        if !value.is_empty() && !self.secrets.contains(&value) {
                            self.secrets.push(value.clone());
                        }
                        Value::String(value)
                    })
                    .map_err(|_| {
                        self.error(
                            format!("required environment variable `{name}` is not set"),
                            expression.span,
                        )
                    }),
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
                    let Value::Object(fields) = value else {
                        return Err(self.error(
                            format!("cannot read member `{member}` from {}", value.type_name()),
                            expression.span,
                        ));
                    };
                    fields.get(member).cloned().ok_or_else(|| {
                        self.error(format!("object has no member `{member}`"), expression.span)
                    })
                }
                PlanExpressionKind::Binary {
                    left,
                    operator,
                    right,
                } => {
                    let left = self.evaluate(left, locals, context).await?;
                    let right = self.evaluate(right, locals, context).await?;
                    evaluate_binary(&left, *operator, &right)
                        .map(Value::Boolean)
                        .map_err(|message| self.error(message, expression.span))
                }
                PlanExpressionKind::InterpolatedString(parts) => {
                    let mut result = String::new();
                    for part in parts {
                        match part {
                            StringPart::Text(value) => result.push_str(value),
                            StringPart::Value(value) => {
                                result.push_str(
                                    &self.evaluate(value, locals, context).await?.to_string(),
                                );
                            }
                        }
                    }
                    Ok(Value::String(result))
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
                            if !self.inside_workload {
                                self.observer.operation_completed(OperationEvent {
                                    capability: capability_name.to_owned(),
                                    operation: operation_name.to_owned(),
                                    duration,
                                    span: expression.span,
                                    result: Ok(value.clone()),
                                });
                            }
                            Ok(value)
                        }
                        Err(error) => {
                            let error = self.error(error.message, error.span);
                            if !self.inside_workload {
                                self.observer.operation_completed(OperationEvent {
                                    capability: capability_name.to_owned(),
                                    operation: operation_name.to_owned(),
                                    duration,
                                    span: expression.span,
                                    result: Err(error.message.clone()),
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
                    let mut last_error = None;
                    for attempt in 0..*attempts {
                        match self.evaluate(body, locals, context).await {
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
        let mut results = vec![None; branches.len()];
        let mut futures = Vec::with_capacity(limit);
        let mut next = 0;
        while next < branches.len() || !futures.is_empty() {
            while next < branches.len() && futures.len() < limit {
                let index = next;
                let branch = &branches[index];
                let mut executor = self.clone();
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
            results[index] = Some(result?);
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
        }
    }
}

struct ActiveIteration<'a> {
    started: Instant,
    future: Pin<Box<dyn Future<Output = Result<Value, RuntimeError>> + Send + 'a>>,
}

enum WorkloadEvent {
    Completed(usize, Result<Value, RuntimeError>),
    Deadline,
}

#[derive(Clone, Copy)]
struct RateSettings {
    target: usize,
    period: Duration,
    duration: Duration,
    limit: usize,
}

#[derive(Clone, Copy)]
enum WorkloadPolicy {
    Rate {
        target: usize,
        period: Duration,
        limit: usize,
        window: Duration,
        planned: usize,
    },
    Concurrency {
        limit: usize,
        duration: Duration,
    },
}

struct WorkloadMetrics {
    started_at: Instant,
    started: usize,
    success: usize,
    failed: usize,
    dropped: usize,
    latency: DurationHistogram,
    scheduling_delay: DurationHistogram,
}

impl WorkloadMetrics {
    fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            started: 0,
            success: 0,
            failed: 0,
            dropped: 0,
            latency: DurationHistogram::default(),
            scheduling_delay: DurationHistogram::default(),
        }
    }

    fn record_completion(&mut self, started: Instant, completed: Instant, succeeded: bool) {
        self.latency.record(
            completed
                .checked_duration_since(started)
                .unwrap_or_default(),
        );
        if succeeded {
            self.success += 1;
        } else {
            self.failed += 1;
        }
    }

    fn snapshot(
        &self,
        policy: &WorkloadPolicy,
        phase: WorkloadPhase,
        active: usize,
        now: Instant,
    ) -> WorkloadSnapshot {
        let kind = match *policy {
            WorkloadPolicy::Rate {
                target,
                period,
                limit,
                window,
                planned,
            } => WorkloadKind::Rate {
                target,
                period,
                duration: window,
                limit,
                planned,
            },
            WorkloadPolicy::Concurrency { limit, duration } => {
                WorkloadKind::Concurrency { limit, duration }
            }
        };
        WorkloadSnapshot {
            kind,
            phase,
            elapsed: now
                .checked_duration_since(self.started_at)
                .unwrap_or_default(),
            active,
            started: self.started,
            completed: self.success + self.failed,
            success: self.success,
            failed: self.failed,
            dropped: self.dropped,
            latency_p50: self.latency.percentile(50),
            latency_p95: self.latency.percentile(95),
            latency_p99: self.latency.percentile(99),
        }
    }

    fn into_value(
        self,
        policy: WorkloadPolicy,
        finished_at: Instant,
        drain_timed_out: bool,
    ) -> Value {
        let completed = self.success + self.failed;
        let errors = if completed == 0 {
            0.0
        } else {
            count_f64(self.failed) / count_f64(completed)
        };
        let mut result = Object::from([
            ("count".to_owned(), count_value(completed)),
            ("started".to_owned(), count_value(self.started)),
            ("success".to_owned(), count_value(self.success)),
            ("failed".to_owned(), count_value(self.failed)),
            ("errors".to_owned(), Value::Float(errors)),
            ("dropped".to_owned(), count_value(self.dropped)),
            (
                "saturated".to_owned(),
                Value::Boolean(self.dropped > 0 || drain_timed_out),
            ),
            ("drainTimedOut".to_owned(), Value::Boolean(drain_timed_out)),
            ("latency".to_owned(), self.latency.into_value()),
            (
                "schedulingDelay".to_owned(),
                self.scheduling_delay.into_value(),
            ),
            (
                "duration".to_owned(),
                Value::Duration(
                    finished_at
                        .checked_duration_since(self.started_at)
                        .unwrap_or_default(),
                ),
            ),
        ]);

        match policy {
            WorkloadPolicy::Rate {
                target,
                period,
                limit,
                window,
                planned,
            } => {
                let actual = count_f64(self.started) * period.as_secs_f64() / window.as_secs_f64();
                result.insert("scheduled".to_owned(), count_value(planned));
                result.insert(
                    "rate".to_owned(),
                    Value::Object(Object::from([
                        ("target".to_owned(), count_value(target)),
                        ("period".to_owned(), Value::Duration(period)),
                        ("actual".to_owned(), Value::Float(actual)),
                        ("limit".to_owned(), count_value(limit)),
                    ])),
                );
            }
            WorkloadPolicy::Concurrency { limit, .. } => {
                result.insert(
                    "concurrency".to_owned(),
                    Value::Object(Object::from([("limit".to_owned(), count_value(limit))])),
                );
            }
        }
        Value::Object(result)
    }
}

struct DurationHistogram {
    buckets: Box<[u64]>,
    count: u64,
    sum_nanos: u128,
    min: Option<Duration>,
    max: Duration,
}

impl Default for DurationHistogram {
    fn default() -> Self {
        Self {
            buckets: vec![0; HISTOGRAM_BUCKETS].into_boxed_slice(),
            count: 0,
            sum_nanos: 0,
            min: None,
            max: Duration::ZERO,
        }
    }
}

impl DurationHistogram {
    fn record(&mut self, duration: Duration) {
        let nanos = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        self.buckets[histogram_index(nanos)] += 1;
        self.count += 1;
        self.sum_nanos += u128::from(nanos);
        self.min = Some(self.min.map_or(duration, |current| current.min(duration)));
        self.max = self.max.max(duration);
    }

    fn percentile(&self, percentile: u64) -> Duration {
        if self.count == 0 {
            return Duration::ZERO;
        }
        let rank = self.count.saturating_mul(percentile).div_ceil(100);
        let mut seen = 0;
        for (index, count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return Duration::from_nanos(histogram_upper_bound(index));
            }
        }
        self.max
    }

    fn into_value(self) -> Value {
        let mean = if self.count == 0 {
            Duration::ZERO
        } else {
            Duration::from_nanos(
                u64::try_from(self.sum_nanos / u128::from(self.count)).unwrap_or(u64::MAX),
            )
        };
        Value::Object(Object::from([
            (
                "min".to_owned(),
                Value::Duration(self.min.unwrap_or_default()),
            ),
            ("mean".to_owned(), Value::Duration(mean)),
            ("max".to_owned(), Value::Duration(self.max)),
            ("p50".to_owned(), Value::Duration(self.percentile(50))),
            ("p90".to_owned(), Value::Duration(self.percentile(90))),
            ("p95".to_owned(), Value::Duration(self.percentile(95))),
            ("p99".to_owned(), Value::Duration(self.percentile(99))),
        ]))
    }
}

fn histogram_index(nanos: u64) -> usize {
    if nanos == 0 {
        return 0;
    }
    let exponent = 63 - nanos.leading_zeros() as usize;
    let base = 1_u64 << exponent;
    let sub_bucket = usize::try_from(
        (u128::from(nanos - base) * HISTOGRAM_SUB_BUCKETS as u128) / u128::from(base),
    )
    .expect("histogram sub-bucket fits usize");
    1 + exponent * HISTOGRAM_SUB_BUCKETS + sub_bucket.min(HISTOGRAM_SUB_BUCKETS - 1)
}

fn histogram_upper_bound(index: usize) -> u64 {
    if index == 0 {
        return 0;
    }
    let index = index - 1;
    let exponent = index / HISTOGRAM_SUB_BUCKETS;
    let sub_bucket = index % HISTOGRAM_SUB_BUCKETS;
    let base = 1_u64 << exponent;
    let width = u128::from(base)
        .saturating_mul((sub_bucket + 1) as u128)
        .div_ceil(HISTOGRAM_SUB_BUCKETS as u128);
    u64::try_from(u128::from(base).saturating_add(width).saturating_sub(1)).unwrap_or(u64::MAX)
}

fn count_value(value: usize) -> Value {
    Value::Integer(i64::try_from(value).unwrap_or(i64::MAX))
}

fn count_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

async fn cooperative_yield() {
    struct YieldOnce(bool);

    impl Future for YieldOnce {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, task: &mut std::task::Context<'_>) -> Poll<Self::Output> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                task.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    YieldOnce(false).await;
}

fn format_duration(duration: Duration) -> String {
    if duration.as_nanos().is_multiple_of(1_000_000_000) {
        format!("{}s", duration.as_secs())
    } else if duration.as_nanos().is_multiple_of(1_000_000) {
        format!("{}ms", duration.as_millis())
    } else if duration.as_nanos().is_multiple_of(1_000) {
        format!("{}us", duration.as_micros())
    } else {
        format!("{}ns", duration.as_nanos())
    }
}

fn evaluate_binary(left: &Value, operator: BinaryOperator, right: &Value) -> Result<bool, String> {
    if matches!(operator, BinaryOperator::Equal | BinaryOperator::NotEqual) {
        let equal = left == right;
        return Ok(if operator == BinaryOperator::Equal {
            equal
        } else {
            !equal
        });
    }

    let ordering = match (left, right) {
        (Value::Integer(left), Value::Integer(right)) => left.partial_cmp(right),
        (Value::Float(left), Value::Float(right)) => left.partial_cmp(right),
        (Value::String(left), Value::String(right)) => left.partial_cmp(right),
        (Value::Duration(left), Value::Duration(right)) => left.partial_cmp(right),
        _ => {
            return Err(format!(
                "cannot compare {} and {}",
                left.type_name(),
                right.type_name()
            ));
        }
    }
    .ok_or_else(|| "comparison is undefined for these values".to_owned())?;
    Ok(match operator {
        BinaryOperator::Less => ordering.is_lt(),
        BinaryOperator::LessEqual => ordering.is_le(),
        BinaryOperator::Greater => ordering.is_gt(),
        BinaryOperator::GreaterEqual => ordering.is_ge(),
        BinaryOperator::Equal | BinaryOperator::NotEqual => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use mettle_capability::{
        Capability, CapabilityDescriptor, CapabilityFuture, Object, OperationSchema, Span,
    };
    use mettle_compiler::{compile, compile_with_capabilities};
    use mettle_syntax::parse;

    use super::{
        Clock, ClockFuture, ExecutionObserver, OperationEvent, Runtime, Value, WorkloadPhase,
        WorkloadSnapshot,
    };

    const PROBE_OPERATIONS: &[OperationSchema] = &[OperationSchema {
        name: "wait",
        parameters: &[],
        options: &[],
        mutually_exclusive: &[],
    }];
    const PROBE: CapabilityDescriptor = CapabilityDescriptor {
        name: "probe",
        defaults: &[],
        operations: PROBE_OPERATIONS,
    };

    #[derive(Default)]
    struct ImmediateClock {
        sleeps: AtomicUsize,
    }

    impl Clock for ImmediateClock {
        fn sleep(&self, _duration: Duration) -> ClockFuture {
            self.sleeps.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(()))
        }
    }

    struct PendingClock;

    impl Clock for PendingClock {
        fn sleep(&self, _duration: Duration) -> ClockFuture {
            Box::pin(std::future::pending())
        }
    }

    #[derive(Default)]
    struct RecordingObserver {
        operations: Mutex<Vec<OperationEvent>>,
        workloads: Mutex<Vec<WorkloadSnapshot>>,
    }

    impl ExecutionObserver for RecordingObserver {
        fn operation_completed(&self, event: OperationEvent) {
            self.operations.lock().expect("observer lock").push(event);
        }

        fn workload_updated(&self, snapshot: WorkloadSnapshot) {
            self.workloads.lock().expect("observer lock").push(snapshot);
        }
    }

    struct PendingCapability {
        dropped: Arc<AtomicBool>,
    }

    impl Capability for PendingCapability {
        fn name(&self) -> &'static str {
            "probe"
        }

        fn invoke(
            &self,
            _operation: usize,
            _arguments: Vec<Value>,
            _options: Object,
            _span: Span,
        ) -> CapabilityFuture<'_> {
            Box::pin(PendingOperation {
                dropped: Arc::clone(&self.dropped),
            })
        }
    }

    struct PendingOperation {
        dropped: Arc<AtomicBool>,
    }

    #[derive(Default)]
    struct ProbeCapability {
        active: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
    }

    impl Capability for ProbeCapability {
        fn name(&self) -> &'static str {
            "probe"
        }

        fn invoke(
            &self,
            _operation: usize,
            _arguments: Vec<Value>,
            _options: Object,
            _span: Span,
        ) -> CapabilityFuture<'_> {
            Box::pin(ProbeOperation {
                active: Arc::clone(&self.active),
                maximum: Arc::clone(&self.maximum),
                started: false,
            })
        }
    }

    struct ProbeOperation {
        active: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
        started: bool,
    }

    impl Future for ProbeOperation {
        type Output = Result<Value, mettle_capability::CapabilityError>;

        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
            if self.started {
                self.active.fetch_sub(1, Ordering::SeqCst);
                self.started = false;
                Poll::Ready(Ok(Value::Null))
            } else {
                self.started = true;
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.maximum.fetch_max(active, Ordering::SeqCst);
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    impl Drop for ProbeOperation {
        fn drop(&mut self) {
            if self.started {
                self.active.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    impl Future for PendingOperation {
        type Output = Result<Value, mettle_capability::CapabilityError>;

        fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    impl Drop for PendingOperation {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn executes_bindings_and_nested_flow_calls() {
        let syntax = parse(
            r#"
            flow identity(value) { return value }
            flow forward(value) { return identity(value) }
            flow main() {
                greeting = forward("Hello from Mettle")
                return greeting
            }
            "#,
        )
        .expect("source should parse");
        let plan = compile(&syntax).expect("source should compile");

        assert_eq!(
            block_on(Runtime::default().execute(&plan)).expect("program should run"),
            Value::String("Hello from Mettle".to_owned())
        );
    }

    #[test]
    fn evaluates_assertions_and_comparisons() {
        let passing = parse(
            r#"
            flow main() {
                assert(10 > 5)
                assert("flow" != "rust")
                return true
            }
            "#,
        )
        .expect("program should parse");
        let passing = compile(&passing).expect("program should compile");
        assert_eq!(
            block_on(Runtime::default().execute(&passing)).expect("assertions should pass"),
            Value::Boolean(true)
        );

        let failing =
            parse("flow main() { assert(false) return null }").expect("program should parse");
        let failing = compile(&failing).expect("program should compile");
        let error = block_on(Runtime::default().execute(&failing))
            .expect_err("false assertion should fail");
        assert_eq!(error.message, "assertion failed");
    }

    #[test]
    fn evaluates_objects_arrays_and_member_paths() {
        let syntax = parse(
            r#"
            flow main() {
                response = {
                    headers: { "content-type": "application/json" }
                    roles: ["tester"]
                }
                return response.headers["content-type"]
            }
            "#,
        )
        .expect("source should parse");
        let plan = compile(&syntax).expect("source should compile");

        assert_eq!(
            block_on(Runtime::default().execute(&plan)).expect("program should run"),
            Value::String("application/json".to_owned())
        );
    }

    #[test]
    fn parallel_preserves_source_order() {
        let syntax = parse(
            r"
            flow first() = 1
            flow second() = 2
            flow third() = 3
            flow main() = parallel(limit: 2) { third() first() second() }
            ",
        )
        .expect("source should parse");
        let plan = compile(&syntax).expect("source should compile");
        assert_eq!(
            block_on(Runtime::default().execute(&plan)).expect("parallel work should complete"),
            Value::Array(vec![
                Value::Integer(3),
                Value::Integer(1),
                Value::Integer(2)
            ])
        );
    }

    #[test]
    fn deadline_drops_pending_child_work() {
        let syntax = parse("flow main() = within(timeout: 1s) { probe.wait() }")
            .expect("source should parse");
        let plan = compile_with_capabilities(&syntax, &[PROBE]).expect("source should compile");
        let dropped = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(ImmediateClock::default());
        let runtime = Runtime::with_clock(
            vec![Arc::new(PendingCapability {
                dropped: Arc::clone(&dropped),
            })],
            clock,
        );
        let error = block_on(runtime.execute(&plan)).expect_err("deadline should expire");
        assert_eq!(error.message, "deadline exceeded after 1s");
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn retry_uses_the_injected_clock_and_reports_exhaustion() {
        let variable = format!("METTLE_RETRY_MISSING_{}", std::process::id());
        let syntax = parse(&format!(
            "flow main() = retry(attempts: 3, delay: 10ms) {{ env(\"{variable}\") }}"
        ))
        .expect("source should parse");
        let plan = compile(&syntax).expect("source should compile");
        let clock = Arc::new(ImmediateClock::default());
        let runtime = Runtime::with_clock(Vec::new(), clock.clone());
        let error = block_on(runtime.execute(&plan)).expect_err("retry should exhaust");
        assert!(
            error
                .message
                .starts_with("retry exhausted after 3 attempts:")
        );
        assert_eq!(clock.sleeps.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn parallel_admission_never_exceeds_its_limit() {
        let branches = std::iter::repeat_n("probe.wait()", 100)
            .collect::<Vec<_>>()
            .join(" ");
        let syntax = parse(&format!(
            "flow main() = parallel(limit: 7) {{ {branches} }}"
        ))
        .expect("source should parse");
        let plan = compile_with_capabilities(&syntax, &[PROBE]).expect("source should compile");
        let capability = Arc::new(ProbeCapability::default());
        let runtime = Runtime::new(vec![capability.clone()]);
        let result = block_on(runtime.execute(&plan)).expect("parallel work should complete");
        let Value::Array(values) = result else {
            panic!("parallel should return an array");
        };
        assert_eq!(values.len(), 100);
        assert_eq!(capability.maximum.load(Ordering::SeqCst), 7);
        assert_eq!(capability.active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_parallel_branch_cancels_and_joins_its_siblings() {
        let variable = format!("METTLE_PARALLEL_MISSING_{}", std::process::id());
        let syntax = parse(&format!(
            "flow main() = parallel() {{ probe.wait() env(\"{variable}\") }}"
        ))
        .expect("source should parse");
        let plan = compile_with_capabilities(&syntax, &[PROBE]).expect("source should compile");
        let dropped = Arc::new(AtomicBool::new(false));
        let runtime = Runtime::with_clock(
            vec![Arc::new(PendingCapability {
                dropped: Arc::clone(&dropped),
            })],
            Arc::new(PendingClock),
        );
        let error = block_on(runtime.execute(&plan)).expect_err("one branch should fail");
        assert!(error.message.contains("required environment variable"));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn rate_returns_scoped_metrics_and_supports_assertions() {
        let syntax = parse(
            r"
            flow main() {
                load = rate(target: 4, period: 1s, duration: 1s, limit: 2) { true }
                assert(load.count == 4)
                assert(load.success == 4)
                assert(load.failed == 0)
                assert(load.dropped == 0)
                assert(load.latency.p95 >= 0ms)
                return load
            }
            ",
        )
        .expect("source should parse");
        let plan = compile(&syntax).expect("source should compile");
        let clock = Arc::new(ImmediateClock::default());
        let result = block_on(Runtime::with_clock(Vec::new(), clock).execute(&plan))
            .expect("rate workload should complete");
        let Value::Object(result) = result else {
            panic!("rate should return an object");
        };
        assert_eq!(result.get("count"), Some(&Value::Integer(4)));
        assert!(matches!(result.get("latency"), Some(Value::Object(_))));
        assert!(matches!(result.get("rate"), Some(Value::Object(_))));
    }

    #[test]
    fn rate_drops_starts_instead_of_growing_an_overload_queue() {
        let syntax = parse(
            "flow main() = rate(target: 4, period: 1s, duration: 1s, limit: 2) { probe.wait() }",
        )
        .expect("source should parse");
        let plan = compile_with_capabilities(&syntax, &[PROBE]).expect("source should compile");
        let dropped = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(ImmediateClock::default());
        let result = block_on(
            Runtime::with_clock(
                vec![Arc::new(PendingCapability {
                    dropped: Arc::clone(&dropped),
                })],
                clock,
            )
            .execute(&plan),
        )
        .expect("saturated rate workload should return metrics");
        let Value::Object(result) = result else {
            panic!("rate should return an object");
        };
        assert_eq!(result.get("started"), Some(&Value::Integer(2)));
        assert_eq!(result.get("dropped"), Some(&Value::Integer(2)));
        assert_eq!(result.get("saturated"), Some(&Value::Boolean(true)));
        assert_eq!(result.get("drainTimedOut"), Some(&Value::Boolean(true)));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn cancelling_a_workload_drops_its_active_iterations() {
        let syntax = parse(
            "flow main() = rate(target: 1, period: 1s, duration: 1s, limit: 1) { probe.wait() }",
        )
        .expect("source should parse");
        let plan = compile_with_capabilities(&syntax, &[PROBE]).expect("source should compile");
        let dropped = Arc::new(AtomicBool::new(false));
        let runtime = Runtime::with_clock(
            vec![Arc::new(PendingCapability {
                dropped: Arc::clone(&dropped),
            })],
            Arc::new(PendingClock),
        );
        let mut execution = Box::pin(runtime.execute(&plan));
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(execution.as_mut().poll(&mut context).is_pending());
        assert!(execution.as_mut().poll(&mut context).is_pending());
        drop(execution);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn concurrency_owns_and_drains_its_fixed_active_set() {
        let syntax = parse("flow main() = concurrency(limit: 3, duration: 1s) { probe.wait() }")
            .expect("source should parse");
        let plan = compile_with_capabilities(&syntax, &[PROBE]).expect("source should compile");
        let capability = Arc::new(ProbeCapability::default());
        let clock = Arc::new(ImmediateClock::default());
        let result = block_on(Runtime::with_clock(vec![capability.clone()], clock).execute(&plan))
            .expect("concurrency workload should complete");
        let Value::Object(result) = result else {
            panic!("concurrency should return an object");
        };
        assert_eq!(result.get("count"), Some(&Value::Integer(3)));
        assert_eq!(capability.maximum.load(Ordering::SeqCst), 3);
        assert_eq!(capability.active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn observer_receives_operations_and_bounded_workload_snapshots() {
        let operation_source =
            parse("flow main() = probe.wait()").expect("operation source should parse");
        let operation_plan = compile_with_capabilities(&operation_source, &[PROBE])
            .expect("operation source should compile");
        let observer = Arc::new(RecordingObserver::default());
        block_on(
            Runtime::new(vec![Arc::new(ProbeCapability::default())])
                .with_observer(observer.clone())
                .execute(&operation_plan),
        )
        .expect("operation should complete");
        assert_eq!(observer.operations.lock().expect("observer lock").len(), 1);

        let workload_source =
            parse("flow main() = rate(target: 4, period: 1s, duration: 1s, limit: 2) { true }")
                .expect("workload source should parse");
        let workload_plan = compile(&workload_source).expect("workload source should compile");
        block_on(
            Runtime::with_clock(Vec::new(), Arc::new(ImmediateClock::default()))
                .with_observer(observer.clone())
                .execute(&workload_plan),
        )
        .expect("workload should complete");
        let workloads = observer.workloads.lock().expect("observer lock");
        assert_eq!(
            workloads.first().map(|value| value.phase),
            Some(WorkloadPhase::Starting)
        );
        assert_eq!(
            workloads.last().map(|value| value.phase),
            Some(WorkloadPhase::Completed)
        );
        assert!(workloads.len() <= 8, "snapshots should remain bounded");
    }
}
