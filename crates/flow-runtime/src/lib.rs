//! Asynchronous execution of validated Flow plans.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::{env, fmt};

use flow_capability::{Capability, Object, Span, Value};
use flow_compiler::{
    Constant, ContextPlan, ExecutionPlan, FlowPlan, Instruction, PlanExpression,
    PlanExpressionKind, PlanField, StringPart,
};

const MAX_CALL_DEPTH: usize = 1_024;

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
}

impl Runtime {
    #[must_use]
    pub fn new(capabilities: Vec<Arc<dyn Capability>>) -> Self {
        Self { capabilities }
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
            flow_stack: Vec::new(),
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

struct Executor<'a> {
    plan: &'a ExecutionPlan,
    capabilities: &'a [Arc<dyn Capability>],
    flow_stack: Vec<String>,
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
                PlanExpressionKind::Environment(name) => {
                    env::var(name).map(Value::String).map_err(|_| {
                        self.error(
                            format!("required environment variable `{name}` is not set"),
                            expression.span,
                        )
                    })
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
            }
        })
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
        RuntimeError {
            message: message.into(),
            span,
            flow_stack: self.flow_stack.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    use flow_compiler::compile;
    use flow_syntax::parse;

    use super::{Runtime, Value};

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
}
