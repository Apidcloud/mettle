//! Declaration navigation over syntax trees.

use super::{
    ContextMember, DeclarationKind, Expression, ExpressionKind, FileContextUse, HashMap,
    MettleDecl, NameResolution, Program, Span, Statement, qualified_name, resolve_visible_name,
};

/// Find the declaration referenced at a source position.
///
/// Uses the same namespace visibility rules as compilation and returns no target
/// for ambiguous or unresolved names.
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
            .filter_map(|(id, context)| {
                context
                    .name
                    .as_ref()
                    .map(|name| (qualified_name(&context.namespace, &name.value), id))
            })
            .collect();
        let flow_ids = program
            .flows
            .iter()
            .enumerate()
            .filter_map(|(id, flow)| {
                flow.name
                    .as_ref()
                    .filter(|_| flow.kind == DeclarationKind::Flow)
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
            if let Some(name) = &context.name
                && self.at(name.span)
            {
                return Some(name.span);
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

        for file_context in &self.program.file_contexts {
            if let FileContextUse::Named(name) = file_context
                && self.at(name.span)
            {
                let owner = self
                    .program
                    .flows
                    .iter()
                    .find(|flow| flow.span.source == name.span.source)?;
                return self.resolve_context(
                    name.value.as_str(),
                    &owner.namespace,
                    &owner.namespace_uses,
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

    fn find_in_flow(&self, flow: &MettleDecl) -> Option<Span> {
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
        self.find_in_statements(&flow.body, &mut locals, flow)
    }

    fn find_in_statements(
        &self,
        statements: &[Statement],
        locals: &mut HashMap<String, Span>,
        flow: &MettleDecl,
    ) -> Option<Span> {
        for statement in statements {
            match statement {
                Statement::If {
                    branches,
                    else_body,
                    ..
                } => {
                    for branch in branches {
                        if let Some(target) =
                            self.find_in_expression(&branch.condition, locals, Some(flow))
                        {
                            return Some(target);
                        }
                        if let Some(target) =
                            self.find_in_statements(&branch.body, &mut locals.clone(), flow)
                        {
                            return Some(target);
                        }
                    }
                    if let Some(body) = else_body
                        && let Some(target) =
                            self.find_in_statements(body, &mut locals.clone(), flow)
                    {
                        return Some(target);
                    }
                }
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
                    if let Some(target) = self.find_in_expression(expression, locals, Some(flow)) {
                        return Some(target);
                    }
                    locals.insert(name.value.clone(), name.span);
                }
                Statement::Return { expression, .. } | Statement::Expression(expression) => {
                    if let Some(target) = self.find_in_expression(expression, locals, Some(flow)) {
                        return Some(target);
                    }
                }
                Statement::Assert {
                    expression,
                    message,
                    ..
                } => {
                    if let Some(target) = self.find_in_expression(expression, locals, Some(flow)) {
                        return Some(target);
                    }
                    if let Some(message) = message
                        && let Some(target) = self.find_in_expression(message, locals, Some(flow))
                    {
                        return Some(target);
                    }
                }
            }
        }
        None
    }

    #[allow(clippy::too_many_lines)]
    fn find_in_expression(
        &self,
        expression: &Expression,
        locals: &HashMap<String, Span>,
        flow: Option<&MettleDecl>,
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
            ExpressionKind::Block(statements) => {
                if let Some(flow) = flow {
                    return self.find_in_statements(statements, &mut locals.clone(), flow);
                }
            }
            ExpressionKind::For {
                key,
                value,
                iterable,
                body,
            } => {
                if let Some(target) = self.find_in_expression(iterable, locals, flow) {
                    return Some(target);
                }
                if let Some(flow) = flow {
                    let mut scope = locals.clone();
                    if let Some(key) = key {
                        scope.insert(key.value.clone(), key.span);
                    }
                    scope.insert(value.value.clone(), value.span);
                    return self.find_in_statements(body, &mut scope, flow);
                }
            }
            ExpressionKind::Call {
                callee,
                arguments,
                named_arguments,
                options,
            } => {
                if self.at(callee.span)
                    && !callee.value.contains('.')
                    && !matches!(callee.value.as_str(), "env" | "senv" | "secret" | "echo")
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
                for argument in named_arguments {
                    if let Some(target) =
                        self.find_in_expression(&argument.expression, locals, flow)
                    {
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
            ExpressionKind::Member { value, .. }
            | ExpressionKind::Fail(value)
            | ExpressionKind::Not(value)
            | ExpressionKind::Negate(value) => {
                return self.find_in_expression(value, locals, flow);
            }
            ExpressionKind::Index { value, index } => {
                return self
                    .find_in_expression(value, locals, flow)
                    .or_else(|| self.find_in_expression(index, locals, flow));
            }
            ExpressionKind::Binary { left, right, .. } => {
                return self
                    .find_in_expression(left, locals, flow)
                    .or_else(|| self.find_in_expression(right, locals, flow));
            }
            ExpressionKind::Within { timeout, body } => {
                return self
                    .find_in_expression(timeout, locals, flow)
                    .or_else(|| self.find_in_expression(body, locals, flow));
            }
            ExpressionKind::Retry {
                attempts,
                delay,
                body,
            } => {
                return self
                    .find_in_expression(attempts, locals, flow)
                    .or_else(|| {
                        delay
                            .as_deref()
                            .and_then(|delay| self.find_in_expression(delay, locals, flow))
                    })
                    .or_else(|| self.find_in_expression(body, locals, flow));
            }
            ExpressionKind::Parallel { limit, branches } => {
                if let Some(target) = limit
                    .as_deref()
                    .and_then(|limit| self.find_in_expression(limit, locals, flow))
                {
                    return Some(target);
                }
                for branch in branches {
                    if let Some(target) = self.find_in_expression(&branch.expression, locals, flow)
                    {
                        return Some(target);
                    }
                }
            }
            ExpressionKind::Rate {
                target,
                period,
                duration,
                limit,
                body,
            } => {
                for value in [target.as_ref(), period.as_ref(), duration.as_ref()] {
                    if let Some(target) = self.find_in_expression(value, locals, flow) {
                        return Some(target);
                    }
                }
                if let Some(target) = limit
                    .as_deref()
                    .and_then(|limit| self.find_in_expression(limit, locals, flow))
                {
                    return Some(target);
                }
                return self.find_in_expression(body, locals, flow);
            }
            ExpressionKind::Concurrency {
                limit,
                duration,
                body,
            } => {
                return self
                    .find_in_expression(limit, locals, flow)
                    .or_else(|| self.find_in_expression(duration, locals, flow))
                    .or_else(|| self.find_in_expression(body, locals, flow));
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
        uses: &[mettle_syntax::Spanned<String>],
    ) -> Option<Span> {
        let NameResolution::Found(id) =
            resolve_visible_name(&self.context_ids, name, namespace, uses)
        else {
            return None;
        };
        self.program.contexts[id]
            .name
            .as_ref()
            .map(|name| name.span)
    }

    fn at(&self, span: Span) -> bool {
        span.source == self.source && self.byte >= span.start && self.byte <= span.end
    }
}
