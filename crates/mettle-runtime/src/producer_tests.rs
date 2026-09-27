use crate::Runtime;
use mettle_capability::documentation::OperationDocumentation;
use mettle_capability::{
    Capability, CapabilityDescriptor, CapabilityError, CapabilityFuture, IoContext, Object,
    OperationSchema, SchemaType, Span, Value,
};
use mettle_compiler::compile_with_capabilities;
use mettle_syntax::parse;
use std::sync::Arc;

const fn operation(name: &'static str) -> OperationSchema {
    OperationSchema {
        name,
        documentation: OperationDocumentation::EMPTY,
        parameters: &[SchemaType::Source],
        parameter_names: &["content"],
        options: &[],
        mutually_exclusive: &[],
        result: SchemaType::String,
    }
}
const DESCRIPTOR: CapabilityDescriptor = CapabilityDescriptor {
    name: "consume",
    description: "Test consumer",
    removed_result_fields: &[],
    constants: &[],
    defaults: &[],
    operations: &[operation("all"), operation("first")],
};
struct Consumer;
impl Capability for Consumer {
    fn name(&self) -> &'static str {
        "consume"
    }
    fn invoke(&self, _: usize, _: Vec<Value>, _: Object, span: Span) -> CapabilityFuture<'_> {
        Box::pin(async move {
            Err(CapabilityError::new(
                "consumer needs an execution context",
                span,
            ))
        })
    }
    fn invoke_with_context<'a>(
        &'a self,
        operation: usize,
        arguments: Vec<Value>,
        _: Object,
        span: Span,
        context: &'a IoContext,
    ) -> CapabilityFuture<'a> {
        Box::pin(async move {
            let Value::Source(source) = arguments[0].revealed() else {
                return Err(CapabilityError::new("source required", span));
            };
            let mut reader = source.open(context).await?;
            let mut bytes = Vec::new();
            while let Some(chunk) = reader.next_chunk().await? {
                bytes.extend_from_slice(&chunk);
                if operation == 1 {
                    break;
                }
            }
            let value = Value::String(String::from_utf8(bytes).unwrap());
            Ok(if arguments[0].contains_sensitive() {
                value.sensitive()
            } else {
                value
            })
        })
    }
}
fn run(source: &str) -> Result<Value, crate::RuntimeError> {
    let plan = compile_with_capabilities(&parse(source).unwrap(), &[DESCRIPTOR]).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(Runtime::new(vec![Arc::new(Consumer)]).execute(&plan))
}

#[test]
fn source_is_lazy_ordered_and_can_be_returned_from_helpers() {
    let result = run(r#"
        flow make = source {
            yield "a"
            for value in ["b", "c"] { yield value }
        }
        flow main {
            unused = source { fail("must not execute") }
            consume.all(make())
        }
    "#)
    .unwrap();
    assert_eq!(result.revealed(), &Value::String("abc".to_owned()));
    assert!(result.contains_sensitive());
}
#[test]
fn backpressure_drop_does_not_execute_the_rest_of_the_producer() {
    let result = run(r#"flow main = consume.first(source {
        yield "first"
        fail("consumer already stopped")
    })"#)
    .unwrap();
    assert_eq!(result.revealed(), &Value::String("first".to_owned()));
}
#[test]
fn source_aliases_cannot_be_consumed_twice_and_fail_bypasses_retry() {
    let error = run(r#"flow main {
        content = source { yield "hi" }
        consume.all(content)
        consume.all(content)
    }"#)
    .unwrap_err();
    assert!(error.message.contains("already consumed"));
    let error = run(r#"flow main = retry(attempts: 3) {
        consume.all(source { fail("terminal producer error") })
    }"#)
    .unwrap_err();
    assert!(error.terminal);
    assert!(error.message.contains("terminal producer error"));
}
#[test]
fn break_exits_nearest_loop_and_mapping_keeps_only_completed_iterations() {
    assert_eq!(
        run(r"flow main {
        values = for value in [1, 2, 3] {
            if (value == 2) { break }
            value
        }
        values
    }")
        .unwrap(),
        Value::Array(vec![Value::Integer(1)])
    );
    let value = run(r#"flow main = consume.all(source {
        for part in ["a", "b", "c"] {
            if (part == "b") { break }
            for nested in [1, 2] {
                yield part
                break
            }
        }
        yield "done"
    })"#)
    .unwrap();
    assert_eq!(value.revealed(), &Value::String("adone".to_owned()));
}
#[test]
fn invalid_source_control_flow_is_rejected_before_execution() {
    for source in [
        "flow main { break }",
        "flow main { yield \"x\" }",
        "flow main = source { yield 42 }",
        "flow main = source { return \"x\" }",
        "flow main = for x in [1] { retry(attempts: 2) { break } }",
        "flow main = for x in [1] { source { break } }",
        "flow main = for x in [1] { break\n echo(\"unreachable\") }",
    ] {
        assert!(
            compile_with_capabilities(&parse(source).unwrap(), &[DESCRIPTOR]).is_err(),
            "{source}"
        );
    }
}
