//! Asynchronous execution of validated Flow plans.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use std::{env, fmt};

use flow_capability::{Capability, Object, Span, Value};
use flow_compiler::{
    BinaryOperator, Constant, ContextPlan, ExecutionPlan, FlowPlan, Instruction, PlanExpression,
    PlanExpressionKind, PlanField, StringPart,
};

const MAX_CALL_DEPTH: usize = 1_024;

pub type ClockFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

pub trait Clock: Send + Sync {
    fn sleep(&self, duration: Duration) -> ClockFuture;
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

#[derive(Clone, Default)]
struct ActiveContext {
    values: Vec<Value>,
    defaults: Vec<Object>,
}

pub struct Runtime {
    capabilities: Vec<Arc<dyn Capability>>,
    clock: Arc<dyn Clock>,
}

impl Runtime {
    #[must_use]
    pub fn new(capabilities: Vec<Arc<dyn Capability>>) -> Self {
        Self {
            capabilities,
            clock: Arc::new(TokioClock),
        }
    }

    #[must_use]
    pub fn with_clock(capabilities: Vec<Arc<dyn Capability>>, clock: Arc<dyn Clock>) -> Self {
        Self {
            capabilities,
            clock,
        }
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
        flow: &FlowPlan,
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
        flow: &FlowPlan,
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
                PlanExpressionKind::FlowCall { flow, arguments } => {
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
                    capability
                        .invoke(*operation, values, effective, expression.span)
                        .await
                        .map_err(|error| self.error(error.message, error.span))
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use flow_capability::{
        Capability, CapabilityDescriptor, CapabilityFuture, Object, OperationSchema, Span,
    };
    use flow_compiler::{compile, compile_with_capabilities};
    use flow_syntax::parse;

    use super::{Clock, ClockFuture, Runtime, Value};

    const PROBE_OPERATIONS: &[OperationSchema] = &[OperationSchema {
        name: "wait",
        parameters: &[],
        options: &[],
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
        type Output = Result<Value, flow_capability::CapabilityError>;

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
        type Output = Result<Value, flow_capability::CapabilityError>;

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
                greeting = forward("Hello from Flow")
                return greeting
            }
            "#,
        )
        .expect("source should parse");
        let plan = compile(&syntax).expect("source should compile");

        assert_eq!(
            block_on(Runtime::default().execute(&plan)).expect("program should run"),
            Value::String("Hello from Flow".to_owned())
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
                user = { name: "Flow", roles: ["tester"] }
                return user.name
            }
            "#,
        )
        .expect("source should parse");
        let plan = compile(&syntax).expect("source should compile");

        assert_eq!(
            block_on(Runtime::default().execute(&plan)).expect("program should run"),
            Value::String("Flow".to_owned())
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
        let variable = format!("FLOW_RETRY_MISSING_{}", std::process::id());
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
        let variable = format!("FLOW_PARALLEL_MISSING_{}", std::process::id());
        let syntax = parse(&format!(
            "flow main() = parallel() {{ probe.wait() env(\"{variable}\") }}"
        ))
        .expect("source should parse");
        let plan = compile_with_capabilities(&syntax, &[PROBE]).expect("source should compile");
        let dropped = Arc::new(AtomicBool::new(false));
        let runtime = Runtime::new(vec![Arc::new(PendingCapability {
            dropped: Arc::clone(&dropped),
        })]);
        let error = block_on(runtime.execute(&plan)).expect_err("one branch should fail");
        assert!(error.message.contains("required environment variable"));
        assert!(dropped.load(Ordering::SeqCst));
    }
}
