//! Semantic validation and lowering from Mettle syntax into an execution plan.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use mettle_capability::{CapabilityDescriptor, FieldSchema, SchemaType};
pub use mettle_syntax::BinaryOperator;
pub use mettle_syntax::DeclarationKind;
use mettle_syntax::{
    ContextMember, Expression, ExpressionKind, FileContextUse, MettleDecl, ObjectField, Program,
    Span, Statement,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionPlan {
    pub capability_names: Vec<String>,
    pub contexts: Vec<ContextPlan>,
    pub flows: Vec<MettlePlan>,
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
pub struct MettlePlan {
    pub kind: DeclarationKind,
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
    Echo {
        value: PlanExpression,
        span: Span,
    },
    If {
        branches: Vec<ConditionalBranch>,
        else_body: Option<Vec<Instruction>>,
    },
    Bind {
        slot: usize,
        expression: PlanExpression,
    },
    Evaluate(PlanExpression),
    Assert {
        condition: PlanExpression,
        message: Option<PlanExpression>,
    },
    Return(PlanExpression),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConditionalBranch {
    pub condition: PlanExpression,
    pub instructions: Vec<Instruction>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanParallelBranch {
    pub name: Option<String>,
    pub expression: PlanExpression,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallEvaluation {
    Parameter(usize),
    Option(usize),
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
    Sensitive(Box<PlanExpression>),
    Array(Vec<PlanExpression>),
    Object(Vec<PlanField>),
    Block {
        instructions: Vec<Instruction>,
        local_count: usize,
    },
    For {
        iterable: Box<PlanExpression>,
        key_slot: Option<usize>,
        value_slot: usize,
        instructions: Vec<Instruction>,
        local_count: usize,
        produces_value: bool,
    },
    Member {
        value: Box<PlanExpression>,
        member: String,
    },
    Index {
        value: Box<PlanExpression>,
        index: Box<PlanExpression>,
    },
    Not(Box<PlanExpression>),
    Negate(Box<PlanExpression>),
    Binary {
        left: Box<PlanExpression>,
        operator: BinaryOperator,
        right: Box<PlanExpression>,
    },
    InterpolatedString(Vec<StringPart>),
    Fail(Box<PlanExpression>),
    MettleCall {
        flow: usize,
        arguments: Vec<PlanExpression>,
        evaluation_order: Vec<usize>,
    },
    CapabilityCall {
        capability: usize,
        operation: usize,
        arguments: Vec<PlanExpression>,
        options: Vec<PlanField>,
        evaluation_order: Vec<CallEvaluation>,
    },
    Within {
        timeout: Duration,
        body: Box<PlanExpression>,
    },
    Retry {
        attempts: usize,
        delay: Duration,
        body: Box<PlanExpression>,
    },
    Parallel {
        limit: usize,
        branches: Vec<PlanParallelBranch>,
    },
    Rate {
        target: usize,
        period: Duration,
        duration: Duration,
        limit: usize,
        body: Box<PlanExpression>,
    },
    Concurrency {
        limit: usize,
        duration: Duration,
        body: Box<PlanExpression>,
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
    Bytes,
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
            Self::Bytes => "bytes",
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

const fn schema_value_type(schema: SchemaType) -> ValueType {
    match schema {
        SchemaType::Boolean => ValueType::Boolean,
        SchemaType::Body | SchemaType::Json => ValueType::Inferred,
        SchemaType::Bytes => ValueType::Bytes,
        SchemaType::Duration => ValueType::Duration,
        SchemaType::Integer => ValueType::Integer,
        SchemaType::Object(_) | SchemaType::StringMap => ValueType::Object,
        SchemaType::String => ValueType::String,
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

mod lowering;
pub use lowering::{compile, compile_with_capabilities};

mod definition;
pub use definition::{find_definition, find_implementation};

fn flow_display_name(flow: &MettleDecl, flow_id: usize) -> String {
    if let Some(name) = &flow.name {
        if flow.kind == DeclarationKind::Test {
            return name.value.clone();
        }
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
    uses: &[mettle_syntax::Spanned<String>],
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
    defaults: Vec<(mettle_syntax::Spanned<String>, Vec<ObjectField>)>,
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

fn with_name_suggestion<'a>(
    message: String,
    name: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> String {
    let candidate = candidates
        .into_iter()
        .map(|candidate| (edit_distance(name, candidate), candidate))
        .min_by_key(|(distance, _)| *distance);
    match candidate {
        Some((distance, candidate)) if distance <= 3 && distance * 3 <= name.len().max(3) => {
            format!("{message}; did you mean `{candidate}`?")
        }
        _ => message,
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_byte) in left.bytes().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_byte) in right.bytes().enumerate() {
            current[right_index + 1] = if left_byte == right_byte {
                previous[right_index]
            } else {
                1 + previous[right_index]
                    .min(previous[right_index + 1])
                    .min(current[right_index])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum VisitState {
    Unvisited,
    Visiting,
    Complete,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mettle_capability::{CapabilityDescriptor, FieldSchema, OperationSchema, SchemaType};
    use mettle_syntax::{ExpressionKind, Statement, parse};

    use super::{
        Constant, Instruction, PlanExpressionKind, compile, compile_with_capabilities,
        find_definition, find_implementation,
    };

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
    const BODY_OPTIONS: &[FieldSchema] = &[
        FieldSchema {
            name: "json",
            value_type: SchemaType::Json,
        },
        FieldSchema {
            name: "body",
            value_type: SchemaType::String,
        },
    ];
    const HTTP_OPERATIONS: &[OperationSchema] = &[
        OperationSchema {
            name: "get",
            parameters: &[SchemaType::String],
            parameter_names: &["url"],
            options: HTTP_OPTIONS,
            mutually_exclusive: &[],
            result: SchemaType::Json,
        },
        OperationSchema {
            name: "post",
            parameters: &[SchemaType::String],
            parameter_names: &["url"],
            options: BODY_OPTIONS,
            mutually_exclusive: &[&["json", "body"]],
            result: SchemaType::Json,
        },
    ];
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

    fn http_errors(source: &str) -> Vec<String> {
        let program = parse(source).expect("test source should parse");
        compile_with_capabilities(&program, &[HTTP])
            .expect_err("test program should not compile")
            .into_iter()
            .map(|error| error.message)
            .collect()
    }

    #[test]
    fn suggests_capability_operations_and_options() {
        let operation = http_errors(r#"flow main() = http.gett("/")"#);
        assert!(operation.iter().any(|message| {
            message == "capability `http` has no operation `gett`; did you mean `get`?"
        }));

        let option = http_errors(r#"flow main() = http.get("/") { tiemout: 1s }"#);
        assert!(
            option
                .iter()
                .any(|message| { message == "unknown option `tiemout`; did you mean `timeout`?" })
        );
    }

    #[test]
    fn rejects_mutually_exclusive_operation_options() {
        let messages =
            http_errors(r#"flow main() = http.post("/") { json: { ok: true }, body: "no" }"#);
        assert!(
            messages
                .iter()
                .any(|message| { message == "options `json` and `body` cannot be used together" })
        );
    }

    #[test]
    fn compiles_contexts_and_capability_calls() {
        let program = parse(
            r#"
            context api {
                apiUrl: env("API_URL")
                defaults http { baseUrl: apiUrl, timeout: 1s }
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
    fn conditional_returns_are_exhaustive_and_branch_bindings_stay_local() {
        let program = parse("flow main() { if (true) { return 1 } else { return 2 } }")
            .expect("source should parse");
        compile(&program).expect("both branches return");
        let messages = errors("flow main() { if (true) { return 1 } }");
        assert!(
            messages
                .iter()
                .any(|message| message.contains("must end with a return"))
        );
        let messages = errors("flow main() { if (true) { value = 1 }\n return value }");
        assert!(
            messages
                .iter()
                .any(|message| message.contains("value") && message.contains("defined"))
        );
        let messages = errors("flow main() { if (1) { return 1 } else { return 2 } }");
        assert!(
            messages
                .iter()
                .any(|message| message == "if condition must be boolean")
        );
    }

    #[test]
    fn echo_is_a_standalone_diagnostic_statement() {
        let plan = compile(
            &parse("flow main() { echo({ value: 1 })\n return true }")
                .expect("source should parse"),
        )
        .expect("echo should compile");
        assert!(matches!(
            plan.flows[0].instructions[0],
            super::Instruction::Echo { .. }
        ));
        let messages = errors("flow main() { return echo(1) }");
        assert!(
            messages
                .iter()
                .any(|message| message.contains("standalone statement"))
        );
        let messages = errors("flow main() { echo()\n return true }");
        assert!(
            messages
                .iter()
                .any(|message| message.contains("exactly one argument"))
        );
        let messages = errors("flow echo(value) = value");
        assert!(messages.iter().any(|message| message.contains("reserved")));
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
        let context_span = declarations.contexts[0]
            .name
            .as_ref()
            .expect("api is named")
            .span;
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
        let (call, argument) = match &entry.flows[0].body[1] {
            Statement::Bind { expression, .. } => match &expression.kind {
                ExpressionKind::Call {
                    callee, arguments, ..
                } => (callee.span, arguments[0].span),
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
        assert_eq!(
            find_implementation(&program, 1, call.start),
            Some(flow_span)
        );
        assert_eq!(find_implementation(&program, 1, argument.start), None);
        assert_eq!(find_implementation(&program, 1, local.start), Some(binding));
        assert_eq!(find_implementation(&program, 1, use_context.start), None);
        assert_eq!(find_implementation(&program, 1, binding.start), None);
    }

    #[test]
    fn lowers_bounded_structured_execution_policies() {
        let program = parse(
            r"
            flow first() = 1
            flow second() = 2
            flow main() = within(timeout: 2s) {
                retry(attempts: 3, delay: 10ms) {
                    parallel(limit: 1) { first(), second() }
                }
            }
            ",
        )
        .expect("source should parse");
        let plan = compile(&program).expect("source should compile");
        let Instruction::Return(expression) = &plan.flows[2].instructions[0] else {
            panic!("expected return");
        };
        let PlanExpressionKind::Within { timeout, body } = &expression.kind else {
            panic!("expected deadline plan");
        };
        assert_eq!(*timeout, Duration::from_secs(2));
        let PlanExpressionKind::Block { instructions, .. } = &body.kind else {
            panic!("expected value-producing block");
        };
        let Some(Instruction::Return(body)) = instructions.last() else {
            panic!("expected final block value");
        };
        let PlanExpressionKind::Retry {
            attempts,
            delay,
            body,
        } = &body.kind
        else {
            panic!("expected retry plan");
        };
        assert_eq!(*attempts, 3);
        assert_eq!(*delay, Duration::from_millis(10));
        let PlanExpressionKind::Block { instructions, .. } = &body.kind else {
            panic!("expected value-producing block");
        };
        let Some(Instruction::Return(body)) = instructions.last() else {
            panic!("expected final block value");
        };
        let PlanExpressionKind::Parallel { limit, branches } = &body.kind else {
            panic!("expected parallel plan");
        };
        assert_eq!(*limit, 1);
        assert_eq!(branches.len(), 2);
    }

    #[test]
    fn rejects_unbounded_or_invalid_policy_configuration() {
        let messages = errors("flow main() = parallel(limit: 0) { within(timeout: 0s) { true } }");
        assert!(
            messages
                .iter()
                .any(|message| message.contains("greater than zero"))
        );
    }

    #[test]
    fn rejects_ambiguous_return_inside_value_blocks() {
        let messages = errors("flow main = retry(attempts: 2) { return 1 }");
        assert!(
            messages
                .iter()
                .any(|message| message.contains("cannot exit a flow"))
        );

        let messages =
            errors("flow main = for item in [1] { if (true) { return item } else { item } }");
        assert!(
            messages
                .iter()
                .any(|message| message.contains("cannot exit a flow"))
        );
    }

    #[test]
    fn named_flow_arguments_bind_by_name_but_evaluate_in_source_order() {
        let source =
            parse("flow choose(first, second) = second\nflow main = choose(second: 2, first: 1)")
                .expect("source should parse");
        let plan = compile(&source).expect("source should compile");
        let Instruction::Return(expression) = &plan.flows[1].instructions[0] else {
            panic!("expected return");
        };
        let PlanExpressionKind::MettleCall {
            arguments,
            evaluation_order,
            ..
        } = &expression.kind
        else {
            panic!("expected flow call");
        };
        assert_eq!(evaluation_order, &vec![1, 0]);
        assert!(matches!(
            arguments[0].kind,
            PlanExpressionKind::Constant(Constant::Integer(1))
        ));
        assert!(matches!(
            arguments[1].kind,
            PlanExpressionKind::Constant(Constant::Integer(2))
        ));

        let messages = errors(
            "flow choose(first, second) = second\nflow main = choose(1, first: 2, second: 3)",
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("supplied more than once"))
        );
        let messages = errors("flow choose(first, second) = second\nflow main = choose(third: 3)");
        assert!(
            messages
                .iter()
                .any(|message| message.contains("no parameter `third`"))
        );
    }

    #[test]
    fn lowers_bounded_load_policies() {
        let program = parse(
            r"
            flow probe() = true
            flow main() {
                load = rate(target: 100, period: 1s, duration: 2s, limit: 10) { probe() }
                return concurrency(limit: 4, duration: 1s) { probe() }
            }
            ",
        )
        .expect("source should parse");
        let plan = compile(&program).expect("source should compile");
        let Instruction::Bind { expression, .. } = &plan.flows[1].instructions[0] else {
            panic!("expected rate binding");
        };
        let PlanExpressionKind::Rate {
            target,
            period,
            duration,
            limit,
            ..
        } = expression.kind
        else {
            panic!("expected rate plan");
        };
        assert_eq!(target, 100);
        assert_eq!(period, Duration::from_secs(1));
        assert_eq!(duration, Duration::from_secs(2));
        assert_eq!(limit, 10);
        let Instruction::Return(expression) = &plan.flows[1].instructions[1] else {
            panic!("expected concurrency return");
        };
        assert!(matches!(
            expression.kind,
            PlanExpressionKind::Concurrency { limit: 4, .. }
        ));
    }

    #[test]
    fn rejects_invalid_load_policy_bounds() {
        let messages =
            errors("flow main() = rate(target: 1, period: 1s, duration: 1ms, limit: 0) { true }");
        assert!(
            messages
                .iter()
                .any(|message| message.contains("greater than zero"))
        );

        let messages = errors("flow main() = rate(target: 1, period: 1s, duration: 1ms) { true }");
        assert!(messages.iter().any(|message| message.contains("between 1")));
    }
}
