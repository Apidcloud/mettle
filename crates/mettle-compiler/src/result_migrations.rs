//! Diagnose removed capability-result members only when provenance is proven.
//! This is not payload inference: arbitrary objects and runtime kinds stay unknown.

use super::{
    CompileError, Constant, Instruction, MettlePlan, PlanExpression, PlanExpressionKind, StringPart,
};
use mettle_capability::CapabilityDescriptor;
use std::collections::HashMap;

type Locals = HashMap<usize, usize>;

pub(super) fn check(
    flows: &[MettlePlan],
    contexts: &[super::ContextPlan],
    capabilities: &[CapabilityDescriptor],
) -> Vec<CompileError> {
    if capabilities
        .iter()
        .all(|capability| capability.removed_result_fields.is_empty())
    {
        return Vec::new();
    }
    let mut origins = Origins {
        flows,
        memo: vec![FlowOrigin::Unvisited; flows.len()],
    };
    for id in 0..flows.len() {
        origins.flow(id);
    }
    let mut errors = Vec::new();
    for context in contexts {
        for field in &context.fields {
            expression(
                &field.expression,
                &Locals::new(),
                &mut origins,
                capabilities,
                &mut errors,
            );
        }
        for defaults in &context.defaults {
            for field in &defaults.fields {
                expression(
                    &field.expression,
                    &Locals::new(),
                    &mut origins,
                    capabilities,
                    &mut errors,
                );
            }
        }
    }
    for flow in flows {
        instructions(
            &flow.instructions,
            &mut Locals::new(),
            &mut origins,
            capabilities,
            &mut errors,
        );
    }
    errors
}

struct Origins<'a> {
    flows: &'a [MettlePlan],
    memo: Vec<FlowOrigin>,
}

#[derive(Clone, Copy)]
enum FlowOrigin {
    Unvisited,
    Resolved(Option<usize>),
}

impl Origins<'_> {
    fn flow(&mut self, id: usize) -> Option<usize> {
        if let FlowOrigin::Resolved(origin) = self.memo[id] {
            return origin;
        }
        self.memo[id] = FlowOrigin::Resolved(None);
        let origin = self.block(&self.flows[id].instructions, &Locals::new());
        self.memo[id] = FlowOrigin::Resolved(origin);
        origin
    }

    fn block(&mut self, body: &[Instruction], outer: &Locals) -> Option<usize> {
        let mut locals = outer.clone();
        for instruction in body {
            match instruction {
                Instruction::Bind { slot, expression } => {
                    let origin = self.expression(expression, &locals);
                    bind(&mut locals, *slot, origin);
                }
                Instruction::Return(value) => return self.expression(value, &locals),
                // Earlier branches may return an unrelated ordinary object.
                Instruction::If { .. } => return None,
                _ => {}
            }
        }
        None
    }

    fn expression(&mut self, value: &PlanExpression, locals: &Locals) -> Option<usize> {
        match &value.kind {
            PlanExpressionKind::CapabilityCall { capability, .. } => Some(*capability),
            PlanExpressionKind::Local(slot) => locals.get(slot).copied(),
            PlanExpressionKind::MettleCall { flow, .. } => self.flow(*flow),
            PlanExpressionKind::Sensitive(value)
            | PlanExpressionKind::Within { body: value, .. }
            | PlanExpressionKind::Retry { body: value, .. } => self.expression(value, locals),
            PlanExpressionKind::Block { instructions, .. } => self.block(instructions, locals),
            _ => None,
        }
    }
}

fn bind(locals: &mut Locals, slot: usize, origin: Option<usize>) {
    if let Some(origin) = origin {
        locals.insert(slot, origin);
    } else {
        locals.remove(&slot);
    }
}

fn instructions(
    body: &[Instruction],
    locals: &mut Locals,
    origins: &mut Origins<'_>,
    capabilities: &[CapabilityDescriptor],
    errors: &mut Vec<CompileError>,
) {
    for instruction in body {
        match instruction {
            Instruction::Break(_) => {}
            Instruction::Yield(value) => expression(value, locals, origins, capabilities, errors),
            Instruction::Bind {
                slot,
                expression: value,
            } => {
                expression(value, locals, origins, capabilities, errors);
                let origin = origins.expression(value, locals);
                bind(locals, *slot, origin);
            }
            Instruction::Echo { value, .. }
            | Instruction::Evaluate(value)
            | Instruction::Return(value) => {
                expression(value, locals, origins, capabilities, errors);
            }
            Instruction::Assert { condition, message } => {
                expression(condition, locals, origins, capabilities, errors);
                if let Some(message) = message {
                    expression(message, locals, origins, capabilities, errors);
                }
            }
            Instruction::If {
                branches,
                else_body,
            } => {
                for branch in branches {
                    expression(&branch.condition, locals, origins, capabilities, errors);
                    instructions(
                        &branch.instructions,
                        &mut locals.clone(),
                        origins,
                        capabilities,
                        errors,
                    );
                }
                if let Some(body) = else_body {
                    instructions(body, &mut locals.clone(), origins, capabilities, errors);
                }
            }
        }
    }
}

fn expression(
    value: &PlanExpression,
    locals: &Locals,
    origins: &mut Origins<'_>,
    capabilities: &[CapabilityDescriptor],
    errors: &mut Vec<CompileError>,
) {
    let mut visit =
        |value: &PlanExpression| expression(value, locals, origins, capabilities, errors);
    match &value.kind {
        PlanExpressionKind::ResourceCall { receiver, .. } => visit(receiver),
        PlanExpressionKind::Member {
            value: object,
            member,
        } => {
            visit(object);
            removed(object, member, value, locals, origins, capabilities, errors);
        }
        PlanExpressionKind::Index {
            value: object,
            index,
        } => {
            visit(object);
            visit(index);
            if let PlanExpressionKind::Constant(Constant::String(key)) = &index.kind {
                removed(object, key, value, locals, origins, capabilities, errors);
            }
        }
        PlanExpressionKind::Sensitive(value)
        | PlanExpressionKind::Not(value)
        | PlanExpressionKind::Negate(value)
        | PlanExpressionKind::Fail(value)
        | PlanExpressionKind::TypeOperation { value, .. }
        | PlanExpressionKind::Within { body: value, .. }
        | PlanExpressionKind::Retry { body: value, .. }
        | PlanExpressionKind::Rate { body: value, .. }
        | PlanExpressionKind::Concurrency { body: value, .. } => visit(value),
        PlanExpressionKind::Array(values)
        | PlanExpressionKind::MettleCall {
            arguments: values, ..
        } => {
            for value in values {
                visit(value);
            }
        }
        PlanExpressionKind::Object(fields) => {
            for field in fields {
                visit(&field.expression);
            }
        }
        PlanExpressionKind::CapabilityCall {
            arguments, options, ..
        } => {
            for argument in arguments {
                visit(argument);
            }
            for option in options {
                visit(&option.expression);
            }
        }
        PlanExpressionKind::Binary { left, right, .. } => {
            visit(left);
            visit(right);
        }
        PlanExpressionKind::Parallel { branches, .. } => {
            for branch in branches {
                visit(&branch.expression);
            }
        }
        PlanExpressionKind::InterpolatedString(parts) => {
            for part in parts {
                if let StringPart::Value(value) = part {
                    visit(value);
                }
            }
        }
        PlanExpressionKind::Source {
            instructions: body, ..
        }
        | PlanExpressionKind::Block {
            instructions: body, ..
        } => instructions(body, &mut locals.clone(), origins, capabilities, errors),
        PlanExpressionKind::For {
            iterable,
            key_slot,
            value_slot,
            instructions: body,
            ..
        } => {
            visit(iterable);
            let mut inner = locals.clone();
            inner.remove(value_slot);
            if let Some(key) = key_slot {
                inner.remove(key);
            }
            instructions(body, &mut inner, origins, capabilities, errors);
        }
        PlanExpressionKind::Constant(_)
        | PlanExpressionKind::Local(_)
        | PlanExpressionKind::Context(_)
        | PlanExpressionKind::Environment(_) => {}
    }
}

fn removed(
    object: &PlanExpression,
    member: &str,
    at: &PlanExpression,
    locals: &Locals,
    origins: &mut Origins<'_>,
    capabilities: &[CapabilityDescriptor],
    errors: &mut Vec<CompileError>,
) {
    if let Some(id) = origins.expression(object, locals)
        && let Some(field) = capabilities[id]
            .removed_result_fields
            .iter()
            .find(|field| field.name == member)
    {
        errors.push(CompileError::new(field.message, at.span));
    }
}
