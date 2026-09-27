//! Bounded, value-free editor information over the compiler's resolved plans.
//!
//! This deliberately does not evaluate constants, environment variables, or
//! capabilities. It describes successful execution, not a runtime payload schema.

use std::collections::HashMap;
use std::sync::Arc;

use mettle_capability::{CapabilityDescriptor, SchemaType};
use mettle_syntax::{Program, Span};

use crate::{
    Constant, ExecutionPlan, Instruction, PlanExpression, PlanExpressionKind, StringPart, ValueType,
};

const MAX_DEPTH: usize = 48;
const MAX_FIELDS: usize = 64;
const MAX_STEPS: usize = 20_000;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sensitivity {
    Public,
    Sensitive,
    Unknown,
}

/// Native kind, known shape, and provenance; never the bound value itself.
#[derive(Clone, Debug, PartialEq)]
pub struct ValueInfo {
    pub kind: ValueType,
    pub nullable: bool,
    pub fields: Arc<Vec<KnownField>>,
    pub fields_truncated: bool,
    pub sensitivity: Sensitivity,
    pub description: &'static str,
    pub operation: Option<Arc<str>>,
    element: Option<Arc<Self>>,
    wrapped_sensitive: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KnownField {
    pub name: String,
    pub value: ValueInfo,
}

impl ValueInfo {
    fn unknown() -> Self {
        Self::new(ValueType::Inferred, Sensitivity::Unknown)
    }

    fn new(kind: ValueType, sensitivity: Sensitivity) -> Self {
        Self {
            kind,
            nullable: false,
            fields: Arc::default(),
            fields_truncated: false,
            sensitivity,
            description: "",
            operation: None,
            element: None,
            wrapped_sensitive: false,
        }
    }

    /// Native-kind spelling suitable for editor output, not an encoding name.
    #[must_use]
    pub fn kind_name(&self) -> String {
        let kind = match self.kind {
            ValueType::Inferred => "value (kind determined at runtime)",
            ValueType::Source => "byte source",
            _ => self.kind.name(),
        };
        if self.nullable {
            format!("{kind} or null")
        } else {
            kind.to_owned()
        }
    }

    fn member(&self, name: &str) -> Self {
        let field = self.fields.iter().find(|field| field.name == name);
        let mut info = field.map_or_else(
            || {
                self.element
                    .as_ref()
                    .filter(|_| self.kind == ValueType::Object)
                    .map_or_else(Self::unknown, |element| element.as_ref().clone())
            },
            |field| field.value.clone(),
        );
        if field.is_some() && info.operation.is_none() {
            info.operation.clone_from(&self.operation);
        }
        if self.wrapped_sensitive {
            info.sensitivity = Sensitivity::Sensitive;
            info.wrapped_sensitive = true;
        }
        info
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolRole {
    Binding,
    Parameter,
    ContextField,
    Field,
}

#[derive(Clone, Debug)]
pub struct SymbolInfo {
    pub span: Span,
    pub role: SymbolRole,
    pub value: ValueInfo,
}

/// A snapshot shared by LSP consumers. Querying it cannot perform runtime I/O.
pub struct SemanticModel {
    symbols: Vec<SymbolInfo>,
}

impl SemanticModel {
    #[must_use]
    pub fn at(&self, source: usize, byte: usize) -> Option<&SymbolInfo> {
        self.symbols
            .iter()
            .filter(|symbol| {
                symbol.span.source == source && symbol.span.start <= byte && byte < symbol.span.end
            })
            .min_by_key(|symbol| symbol.span.end - symbol.span.start)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum BindingOwner {
    Flow(usize, usize),
    Loop(usize, usize),
    Context(usize, usize),
}

pub(super) struct BindingSource {
    pub declaration: Span,
    pub initializer: Option<Span>,
    pub owner: BindingOwner,
}

/// Analyze even independently valid parts of an incomplete/invalid program.
/// Name resolution, scope, and capability lookup belong to normal lowering.
#[must_use]
pub fn analyze(
    program: &Program,
    capabilities: &[CapabilityDescriptor],
    sources: &[&str],
) -> SemanticModel {
    let (plan, errors, bindings) = crate::lowering::editor_plan(program, capabilities);
    let mut analyzer = Analyzer {
        plan: &plan,
        capabilities,
        bindings: &bindings,
        symbols: Vec::new(),
        flow_results: vec![None; plan.flows.len()],
        active_flows: vec![false; plan.flows.len()],
        context_results: HashMap::new(),
        steps: MAX_STEPS,
        invalid: errors.iter().map(|error| error.span).collect(),
        identifiers: sources
            .iter()
            .enumerate()
            .map(|(source, text)| {
                mettle_syntax::documentation::identifier_spans(text)
                    .into_iter()
                    .map(|span| span.with_source(source))
                    .collect()
            })
            .collect(),
        symbol_indices: HashMap::new(),
    };
    for context in 0..plan.contexts.len() {
        for binding in &bindings {
            if let BindingOwner::Context(id, slot) = binding.owner
                && context == id
            {
                analyzer.context(id, slot, 0);
            }
        }
    }
    for flow in 0..plan.flows.len() {
        analyzer.flow(flow, 0);
    }
    for binding in &bindings {
        if !analyzer.symbol_indices.contains_key(&(
            binding.declaration.source,
            binding.declaration.start,
            binding.declaration.end,
        )) {
            let role = match binding.owner {
                BindingOwner::Context(_, _) => SymbolRole::ContextField,
                BindingOwner::Flow(_, _) if binding.initializer.is_none() => SymbolRole::Parameter,
                BindingOwner::Flow(_, _) | BindingOwner::Loop(_, _) => SymbolRole::Binding,
            };
            analyzer.record(binding.declaration, role, &ValueInfo::unknown());
        }
    }
    SemanticModel {
        symbols: analyzer.symbols,
    }
}

struct Analyzer<'a> {
    plan: &'a ExecutionPlan,
    capabilities: &'a [CapabilityDescriptor],
    bindings: &'a [BindingSource],
    symbols: Vec<SymbolInfo>,
    flow_results: Vec<Option<ValueInfo>>,
    active_flows: Vec<bool>,
    context_results: HashMap<(usize, usize), ValueInfo>,
    steps: usize,
    invalid: Vec<Span>,
    identifiers: Vec<Vec<Span>>,
    symbol_indices: HashMap<(usize, usize, usize), usize>,
}

type Locals = HashMap<usize, ValueInfo>;

impl Analyzer<'_> {
    fn available(&mut self, depth: usize) -> bool {
        if depth > MAX_DEPTH || self.steps == 0 {
            return false;
        }
        self.steps -= 1;
        true
    }

    fn record(&mut self, span: Span, role: SymbolRole, value: &ValueInfo) {
        let key = (span.source, span.start, span.end);
        if let Some(index) = self.symbol_indices.get(&key).copied() {
            self.symbols[index].value = join(&self.symbols[index].value, value);
            return;
        }
        if self.symbols.len() < MAX_STEPS {
            self.symbol_indices.insert(key, self.symbols.len());
            self.symbols.push(SymbolInfo {
                span,
                role,
                value: value.clone(),
            });
        }
    }

    fn reference_span(&self, expression: &PlanExpression, member: bool) -> Option<Span> {
        let identifiers = self.identifiers.get(expression.span.source)?;
        let mut tokens = identifiers
            .iter()
            .filter(|span| span.start >= expression.span.start && span.end <= expression.span.end);
        if !member {
            return tokens.next().copied();
        }
        if let PlanExpressionKind::Member { value, .. } = &expression.kind
            && value.span == expression.span
        {
            let mut base = value.as_ref();
            let mut index = 1;
            while let PlanExpressionKind::Member { value, .. } = &base.kind {
                if value.span != expression.span {
                    break;
                }
                index += 1;
                base = value;
            }
            if matches!(base.kind, PlanExpressionKind::Constant(_)) {
                index += 1;
            }
            return tokens.nth(index).copied();
        }
        tokens.next_back().copied()
    }

    fn binding(
        &mut self,
        owner: BindingOwner,
        initializer: Option<Span>,
        role: SymbolRole,
        value: &ValueInfo,
    ) {
        if let Some(binding) = self
            .bindings
            .iter()
            .find(|binding| binding.owner == owner && binding.initializer == initializer)
        {
            self.record(binding.declaration, role, value);
        }
    }

    fn context(&mut self, id: usize, slot: usize, depth: usize) -> ValueInfo {
        if let Some(info) = self.context_results.get(&(id, slot)) {
            return info.clone();
        }
        if !self.available(depth) {
            return ValueInfo::unknown();
        }
        self.context_results
            .insert((id, slot), ValueInfo::unknown());
        let binding = self.bindings.iter().find(|binding|
            matches!(binding.owner, BindingOwner::Context(context, index) if context == id && index == slot));
        let info = binding
            .and_then(|binding| binding.initializer)
            .and_then(|span| {
                self.plan.contexts[id]
                    .fields
                    .iter()
                    .find(|field| field.expression.span == span)
            })
            .map_or_else(ValueInfo::unknown, |field| {
                self.expression(&field.expression, &Locals::new(), None, Some(id), depth + 1)
            });
        if let Some(binding) = binding {
            self.record(binding.declaration, SymbolRole::ContextField, &info);
        }
        self.context_results.insert((id, slot), info.clone());
        info
    }

    fn flow(&mut self, id: usize, depth: usize) -> ValueInfo {
        if let Some(info) = &self.flow_results[id] {
            return info.clone();
        }
        if self.active_flows[id] || !self.available(depth) {
            return ValueInfo::unknown();
        }
        self.active_flows[id] = true;
        let flow = &self.plan.flows[id];
        let mut locals = Locals::new();
        for slot in 0..flow.parameters.len() {
            let info = ValueInfo::unknown();
            self.binding(
                BindingOwner::Flow(id, slot),
                None,
                SymbolRole::Parameter,
                &info,
            );
            locals.insert(slot, info);
        }
        let mut info = self.block(
            &flow.instructions,
            &mut locals,
            Some(id),
            flow.contexts.first().copied(),
            depth + 1,
        );
        if self.invalid.iter().any(|span| {
            span.source == flow.span.source
                && span.start >= flow.span.start
                && span.end <= flow.span.end
        }) {
            info = ValueInfo::unknown();
        }
        self.active_flows[id] = false;
        self.flow_results[id] = Some(info.clone());
        info
    }

    fn block(
        &mut self,
        body: &[Instruction],
        locals: &mut Locals,
        flow: Option<usize>,
        context: Option<usize>,
        depth: usize,
    ) -> ValueInfo {
        if !self.available(depth) {
            return ValueInfo::unknown();
        }
        let mut returns = Vec::new();
        for instruction in body {
            match instruction {
                Instruction::Bind { slot, expression } => {
                    let info = self.expression(expression, locals, flow, context, depth + 1);
                    if let Some(flow) = flow {
                        self.binding(
                            BindingOwner::Flow(flow, *slot),
                            Some(expression.span),
                            SymbolRole::Binding,
                            &info,
                        );
                    }
                    locals.insert(*slot, info);
                }
                Instruction::Echo { value, .. } | Instruction::Evaluate(value) => {
                    self.expression(value, locals, flow, context, depth + 1);
                }
                Instruction::Return(value) => {
                    returns.push(self.expression(value, locals, flow, context, depth + 1));
                }
                Instruction::Assert { condition, message } => {
                    self.expression(condition, locals, flow, context, depth + 1);
                    if let Some(message) = message {
                        self.expression(message, locals, flow, context, depth + 1);
                    }
                }
                Instruction::If {
                    branches,
                    else_body,
                } => {
                    for branch in branches {
                        self.expression(&branch.condition, locals, flow, context, depth + 1);
                        let info = self.block(
                            &branch.instructions,
                            &mut locals.clone(),
                            flow,
                            context,
                            depth + 1,
                        );
                        if has_return(&branch.instructions) {
                            returns.push(info);
                        }
                    }
                    if let Some(body) = else_body {
                        let info = self.block(body, &mut locals.clone(), flow, context, depth + 1);
                        if has_return(body) {
                            returns.push(info);
                        }
                    }
                }
            }
        }
        returns
            .into_iter()
            .reduce(|left, right| join(&left, &right))
            .unwrap_or_else(ValueInfo::unknown)
    }

    #[allow(clippy::too_many_lines)]
    fn expression(
        &mut self,
        expression: &PlanExpression,
        locals: &Locals,
        flow: Option<usize>,
        context: Option<usize>,
        depth: usize,
    ) -> ValueInfo {
        if !self.available(depth) {
            return ValueInfo::unknown();
        }
        let mut info = match &expression.kind {
            PlanExpressionKind::Constant(_) => {
                ValueInfo::new(expression.value_type, Sensitivity::Public)
            }
            PlanExpressionKind::Environment(_) => {
                ValueInfo::new(ValueType::String, Sensitivity::Unknown)
            }
            PlanExpressionKind::Sensitive(value) => {
                let mut info = self.expression(value, locals, flow, context, depth + 1);
                info.sensitivity = Sensitivity::Sensitive;
                info.wrapped_sensitive = true;
                info
            }
            PlanExpressionKind::Local(slot) => {
                let info = locals.get(slot).cloned().unwrap_or_else(ValueInfo::unknown);
                if let Some(span) = self.reference_span(expression, false) {
                    let role =
                        if flow.is_some_and(|id| *slot < self.plan.flows[id].parameters.len()) {
                            SymbolRole::Parameter
                        } else {
                            SymbolRole::Binding
                        };
                    self.record(span, role, &info);
                }
                info
            }
            PlanExpressionKind::Context(slot) => {
                let info = context
                    .map_or_else(ValueInfo::unknown, |id| self.context(id, *slot, depth + 1));
                if let Some(span) = self.reference_span(expression, false) {
                    self.record(span, SymbolRole::ContextField, &info);
                }
                info
            }
            PlanExpressionKind::Object(fields) => {
                let mut info = ValueInfo::new(ValueType::Object, Sensitivity::Public);
                let mut shape = Vec::new();
                for field in fields.iter().take(MAX_FIELDS) {
                    let value =
                        self.expression(&field.expression, locals, flow, context, depth + 1);
                    info.sensitivity = composite(info.sensitivity, value.sensitivity);
                    shape.push(KnownField {
                        name: field.name.clone(),
                        value,
                    });
                }
                info.fields_truncated = fields.len() > MAX_FIELDS;
                if info.fields_truncated {
                    info.sensitivity = composite(info.sensitivity, Sensitivity::Unknown);
                }
                info.fields = Arc::new(shape);
                info
            }
            PlanExpressionKind::Array(values) => {
                let mut info = ValueInfo::new(ValueType::Array, Sensitivity::Public);
                let mut element = None;
                for value in values.iter().take(MAX_FIELDS) {
                    let value = self.expression(value, locals, flow, context, depth + 1);
                    info.sensitivity = composite(info.sensitivity, value.sensitivity);
                    element = Some(match element {
                        Some(previous) => join(&previous, &value),
                        None => value,
                    });
                }
                if values.len() > MAX_FIELDS {
                    info.sensitivity = composite(info.sensitivity, Sensitivity::Unknown);
                    element = None;
                }
                info.element = element.map(Arc::new);
                info
            }
            PlanExpressionKind::Member { value, member } => {
                let value = self.expression(value, locals, flow, context, depth + 1);
                let info = value.member(member);
                if let Some(span) = self.reference_span(expression, true) {
                    self.record(span, SymbolRole::Field, &info);
                }
                info
            }
            PlanExpressionKind::Index { value, index } => {
                let value = self.expression(value, locals, flow, context, depth + 1);
                let index_info = self.expression(index, locals, flow, context, depth + 1);
                let mut info =
                    if let PlanExpressionKind::Constant(Constant::String(key)) = &index.kind {
                        value.member(key)
                    } else {
                        value
                            .element
                            .as_deref()
                            .cloned()
                            .unwrap_or_else(ValueInfo::unknown)
                    };
                if value.wrapped_sensitive || index_info.wrapped_sensitive {
                    info.sensitivity = Sensitivity::Sensitive;
                    info.wrapped_sensitive = true;
                }
                info
            }
            PlanExpressionKind::CapabilityCall {
                capability,
                operation,
                arguments,
                options,
                ..
            } => {
                for value in arguments {
                    self.expression(value, locals, flow, context, depth + 1);
                }
                for field in options {
                    self.expression(&field.expression, locals, flow, context, depth + 1);
                }
                let descriptor = &self.capabilities[*capability];
                let operation = &descriptor.operations[*operation];
                let mut info = schema(operation.result, depth + 1, &mut self.steps);
                info.operation = Some(format!("{}.{}", descriptor.name, operation.name).into());
                info
            }
            PlanExpressionKind::MettleCall {
                flow: called,
                arguments,
                ..
            } => {
                for value in arguments {
                    self.expression(value, locals, flow, context, depth + 1);
                }
                // A reusable flow is inferred with unknown parameters, never
                // specialized using one caller's values or credentials.
                self.flow(*called, depth + 1)
            }
            PlanExpressionKind::Block { instructions, .. } => {
                self.block(instructions, &mut locals.clone(), flow, context, depth + 1)
            }
            PlanExpressionKind::Within { body, .. } | PlanExpressionKind::Retry { body, .. } => {
                self.expression(body, locals, flow, context, depth + 1)
            }
            PlanExpressionKind::For {
                iterable,
                key_slot,
                value_slot,
                instructions,
                produces_value,
                ..
            } => {
                let iterable = self.expression(iterable, locals, flow, context, depth + 1);
                let mut scope = locals.clone();
                let mut value = iterable
                    .element
                    .as_deref()
                    .cloned()
                    .or_else(|| {
                        (iterable.kind == ValueType::Object && !iterable.fields_truncated).then(
                            || {
                                iterable
                                    .fields
                                    .iter()
                                    .map(|field| field.value.clone())
                                    .reduce(|left, right| join(&left, &right))
                                    .unwrap_or_else(ValueInfo::unknown)
                            },
                        )
                    })
                    .unwrap_or_else(ValueInfo::unknown);
                if iterable.wrapped_sensitive {
                    value.sensitivity = Sensitivity::Sensitive;
                    value.wrapped_sensitive = true;
                }
                scope.insert(*value_slot, value.clone());
                if let Some(id) = flow {
                    self.binding(
                        BindingOwner::Loop(id, *value_slot),
                        Some(expression.span),
                        SymbolRole::Binding,
                        &value,
                    );
                }
                if let Some(slot) = key_slot {
                    let kind = match iterable.kind {
                        ValueType::Array => ValueType::Integer,
                        ValueType::Object => ValueType::String,
                        _ => ValueType::Inferred,
                    };
                    let mut key = ValueInfo::new(kind, Sensitivity::Unknown);
                    if iterable.kind == ValueType::Object && iterable.wrapped_sensitive {
                        key.sensitivity = Sensitivity::Sensitive;
                        key.wrapped_sensitive = true;
                    }
                    scope.insert(*slot, key.clone());
                    if let Some(id) = flow {
                        self.binding(
                            BindingOwner::Loop(id, *slot),
                            Some(expression.span),
                            SymbolRole::Binding,
                            &key,
                        );
                    }
                }
                let element = self.block(instructions, &mut scope, flow, context, depth + 1);
                let mut info = ValueInfo::new(
                    if *produces_value {
                        match iterable.kind {
                            ValueType::Array | ValueType::Object => iterable.kind,
                            _ => ValueType::Inferred,
                        }
                    } else {
                        ValueType::Null
                    },
                    Sensitivity::Unknown,
                );
                if *produces_value && iterable.kind == ValueType::Array {
                    info.element = Some(Arc::new(element));
                }
                info
            }
            PlanExpressionKind::Parallel { branches, .. } => {
                let named = branches.iter().all(|branch| branch.name.is_some());
                let mut info = ValueInfo::new(
                    if named {
                        ValueType::Object
                    } else {
                        ValueType::Array
                    },
                    Sensitivity::Public,
                );
                let mut fields = Vec::new();
                let mut element = None;
                for branch in branches.iter().take(MAX_FIELDS) {
                    let value =
                        self.expression(&branch.expression, locals, flow, context, depth + 1);
                    info.sensitivity = composite(info.sensitivity, value.sensitivity);
                    if let Some(name) = &branch.name {
                        fields.push(KnownField {
                            name: name.clone(),
                            value,
                        });
                    } else {
                        element = Some(match element {
                            Some(previous) => join(&previous, &value),
                            None => value,
                        });
                    }
                }
                info.fields_truncated = branches.len() > MAX_FIELDS;
                if info.fields_truncated {
                    element = None;
                    info.sensitivity = Sensitivity::Unknown;
                }
                info.fields = Arc::new(fields);
                info.element = element.map(Arc::new);
                info
            }
            PlanExpressionKind::Not(value) | PlanExpressionKind::Negate(value) => {
                let value = self.expression(value, locals, flow, context, depth + 1);
                let kind = if matches!(expression.kind, PlanExpressionKind::Negate(_)) {
                    match value.kind {
                        ValueType::Integer | ValueType::Float => value.kind,
                        _ => ValueType::Inferred,
                    }
                } else {
                    expression.value_type
                };
                let mut info = ValueInfo::new(kind, value.sensitivity);
                info.wrapped_sensitive = value.sensitivity == Sensitivity::Sensitive;
                info
            }
            PlanExpressionKind::TypeOperation {
                value,
                target,
                cast,
            } => {
                let value = self.expression(value, locals, flow, context, depth + 1);
                let kind = if *cast && *target == mettle_syntax::ValueKind::Number {
                    ValueType::Float
                } else {
                    expression.value_type
                };
                let mut info = if *cast && kind == value.kind {
                    value.clone()
                } else {
                    ValueInfo::new(kind, value.sensitivity)
                };
                info.wrapped_sensitive = value.sensitivity == Sensitivity::Sensitive;
                info
            }
            PlanExpressionKind::Binary { left, right, .. } => {
                let left = self.expression(left, locals, flow, context, depth + 1);
                let right = self.expression(right, locals, flow, context, depth + 1);
                ValueInfo::new(
                    expression.value_type,
                    composite(left.sensitivity, right.sensitivity),
                )
            }
            PlanExpressionKind::InterpolatedString(parts) => {
                let mut sensitivity = Sensitivity::Public;
                for part in parts {
                    if let StringPart::Value(value) = part {
                        sensitivity = composite(
                            sensitivity,
                            self.expression(value, locals, flow, context, depth + 1)
                                .sensitivity,
                        );
                    }
                }
                ValueInfo::new(ValueType::String, sensitivity)
            }
            PlanExpressionKind::Fail(value)
            | PlanExpressionKind::Rate { body: value, .. }
            | PlanExpressionKind::Concurrency { body: value, .. } => {
                self.expression(value, locals, flow, context, depth + 1);
                ValueInfo::unknown()
            }
        };
        if self.invalid.iter().any(|span| {
            span.source == expression.span.source
                && span.start >= expression.span.start
                && span.end <= expression.span.end
        }) {
            info = ValueInfo::unknown();
        }
        info
    }
}

fn has_return(body: &[Instruction]) -> bool {
    body.iter().any(|instruction| match instruction {
        Instruction::Return(_) => true,
        Instruction::If {
            branches,
            else_body,
        } => {
            branches
                .iter()
                .any(|branch| has_return(&branch.instructions))
                || else_body.as_ref().is_some_and(|body| has_return(body))
        }
        _ => false,
    })
}

fn schema(kind: SchemaType, depth: usize, steps: &mut usize) -> ValueInfo {
    if depth > MAX_DEPTH || *steps == 0 {
        return ValueInfo::unknown();
    }
    *steps -= 1;
    let mut info = ValueInfo::new(crate::schema_value_type(kind), Sensitivity::Unknown);
    info.nullable = kind == SchemaType::NullableString;
    if info.nullable {
        info.kind = ValueType::String;
    }
    if let SchemaType::Object(fields) = kind {
        info.fields = Arc::new(
            fields
                .iter()
                .take(MAX_FIELDS)
                .map(|field| {
                    let mut value = schema(field.value_type, depth + 1, steps);
                    value.description = field.description;
                    KnownField {
                        name: field.name.to_owned(),
                        value,
                    }
                })
                .collect(),
        );
        info.fields_truncated = fields.len() > MAX_FIELDS;
    } else if kind == SchemaType::StringMap {
        info.element = Some(Arc::new(ValueInfo::new(
            ValueType::String,
            Sensitivity::Unknown,
        )));
    }
    info
}

fn composite(left: Sensitivity, right: Sensitivity) -> Sensitivity {
    match (left, right) {
        (Sensitivity::Sensitive, _) | (_, Sensitivity::Sensitive) => Sensitivity::Sensitive,
        (Sensitivity::Public, Sensitivity::Public) => Sensitivity::Public,
        _ => Sensitivity::Unknown,
    }
}

fn join(left: &ValueInfo, right: &ValueInfo) -> ValueInfo {
    merge(left, right, &mut 512)
}

fn merge(left: &ValueInfo, right: &ValueInfo, steps: &mut usize) -> ValueInfo {
    if *steps == 0 {
        return ValueInfo::unknown();
    }
    *steps -= 1;
    if left.kind != right.kind || left.nullable != right.nullable {
        return ValueInfo::unknown();
    }
    let same_element = match (&left.element, &right.element) {
        (None, None) => true,
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        _ => false,
    };
    if Arc::ptr_eq(&left.fields, &right.fields)
        && same_element
        && left.sensitivity == right.sensitivity
        && left.wrapped_sensitive == right.wrapped_sensitive
        && left.operation == right.operation
        && left.description == right.description
    {
        return left.clone();
    }
    let mut info = ValueInfo::new(
        left.kind,
        if left.sensitivity == right.sensitivity {
            left.sensitivity
        } else {
            Sensitivity::Unknown
        },
    );
    info.nullable = left.nullable;
    info.fields = Arc::new(
        left.fields
            .iter()
            .filter_map(|field| {
                right
                    .fields
                    .iter()
                    .find(|other| other.name == field.name)
                    .map(|other| KnownField {
                        name: field.name.clone(),
                        value: merge(&field.value, &other.value, steps),
                    })
            })
            .collect(),
    );
    info.fields_truncated = left.fields_truncated || right.fields_truncated;
    info.wrapped_sensitive = left.wrapped_sensitive && right.wrapped_sensitive;
    if left.operation == right.operation {
        info.operation.clone_from(&left.operation);
    }
    if left.description == right.description {
        info.description = left.description;
    }
    if let (Some(left), Some(right)) = (&left.element, &right.element) {
        info.element = Some(Arc::new(merge(left, right, steps)));
    }
    info
}
