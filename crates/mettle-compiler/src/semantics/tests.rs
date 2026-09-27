use super::*;
use mettle_capability::documentation::OperationDocumentation;
use mettle_capability::{FieldSchema, OperationSchema};
use std::fmt::Write as _;

const REMOTE: CapabilityDescriptor = CapabilityDescriptor {
    name: "remote",
    description: "Fake capability, never executed.",
    constants: &[],
    defaults: &[],
    removed_result_fields: &[],
    operations: &[OperationSchema {
        name: "get",
        parameters: &[],
        parameter_names: &[],
        options: &[],
        mutually_exclusive: &[],
        documentation: OperationDocumentation {
            summary: "Return an envelope.",
            notes: &[],
            parameters: &[],
            details: "",
            example: "remote.get()",
        },
        result: SchemaType::Object(&[
            FieldSchema::new("status", SchemaType::Integer).documented("Response status."),
            FieldSchema::new("body", SchemaType::Value).documented("Decoded native value."),
            FieldSchema::new("mediaType", SchemaType::NullableString),
        ]),
    }],
};

fn model(text: &str) -> SemanticModel {
    analyze(
        &mettle_syntax::parse(text).unwrap(),
        &[REMOTE, mettle_capability::codecs::JSON_DESCRIPTOR],
        &[text],
    )
}

fn at<'a>(model: &'a SemanticModel, text: &str, needle: &str) -> &'a SymbolInfo {
    model
        .at(0, text.find(needle).unwrap())
        .unwrap_or_else(|| panic!("missing {needle}"))
}

#[test]
fn capability_envelopes_aliases_helpers_and_unknown_payloads() {
    let text = "flow get = retry(attempts: 2) { remote.get() }\nflow main {\n response = get()\n alias = response\n status = alias.status\n payload = response.body\n field = payload.user\n mime = response.mediaType\n literal = { json: true, nested: { count: 3 } }\n selected = literal.nested.count\n { status: status, field: field }\n}";
    let model = model(text);
    let response = &at(&model, text, "response =").value;
    assert_eq!(response.kind, ValueType::Object);
    assert_eq!(response.operation.as_deref(), Some("remote.get"));
    assert_eq!(response.fields.len(), 3);
    assert_eq!(
        at(&model, text, "alias =").value.operation,
        response.operation
    );
    assert_eq!(at(&model, text, "status =").value.kind, ValueType::Integer);
    let field = &at(&model, text, "status\n payload").value;
    assert_eq!(field.description, "Response status.");
    assert_eq!(field.operation.as_deref(), Some("remote.get"));
    assert_eq!(
        at(&model, text, "payload =").value.kind,
        ValueType::Inferred
    );
    assert_eq!(at(&model, text, "field =").value.kind, ValueType::Inferred);
    assert_eq!(
        at(&model, text, "mime =").value.kind_name(),
        "string or null"
    );
    assert_eq!(
        at(&model, text, "selected =").value.kind,
        ValueType::Integer
    );
    assert_eq!(
        at(&model, text, "literal.nested").value.kind,
        ValueType::Object
    );
    assert_eq!(
        at(&model, text, "nested.count").value.kind,
        ValueType::Object
    );
    assert_eq!(
        at(&model, text, "count\n { status").value.kind,
        ValueType::Integer
    );
}

#[test]
fn contexts_shadowing_secrets_and_caller_dependent_parameters() {
    let text = "context base { token: senv(\"EDITOR_MUST_NOT_READ\"), options: { tries: 3 } }\ncontext applied { use context base\n options: { tries: 4, enabled: true } }\nuse context applied\nflow helper(input) = input\nflow main {\n credential = token\n attempts = options.tries\n json = { mediaType: \"private-local\" }\n localMime = json.mediaType\n record = { token: secret(\"NEVER_PRINT_THIS\"), count: 1 }\n safe = record.count\n protected = secret(record)\n unsafe = protected.count\n uncertain = helper(42)\n record\n}";
    let model = model(text);
    assert_eq!(
        at(&model, text, "credential =").value.sensitivity,
        Sensitivity::Sensitive
    );
    assert_eq!(
        at(&model, text, "attempts =").value.kind,
        ValueType::Integer
    );
    assert_eq!(
        at(&model, text, "localMime =").value.kind,
        ValueType::String
    );
    assert_eq!(
        at(&model, text, "safe =").value.sensitivity,
        Sensitivity::Public
    );
    assert_eq!(
        at(&model, text, "unsafe =").value.sensitivity,
        Sensitivity::Sensitive
    );
    assert_eq!(
        at(&model, text, "uncertain =").value.kind,
        ValueType::Inferred
    );
    assert_eq!(at(&model, text, "input)").role, SymbolRole::Parameter);
    assert!(!format!("{:?}", model.symbols).contains("NEVER_PRINT_THIS"));
    assert!(!format!("{:?}", model.symbols).contains("private-local"));
}

#[test]
fn branches_loops_parallel_and_precise_tokens_do_not_leak_scope() {
    let text = "flow choose(flag) { if (flag) { return { common: 1, onlyLeft: true } } else { return { common: 2 } } }\nflow main {\n result = choose(true)\n common = result.common\n absent = result.onlyLeft\n tasks = parallel { first: remote.get(), second: remote.get() }\n status = tasks.first.status\n items = [{ count: 1 }, { count: 2 }]\n counts = for index, item in items { item.count }\n object = { field: true }\n spaced = ((object  .  field))\n counts\n}";
    let model = model(text);
    assert_eq!(at(&model, text, "common =").value.kind, ValueType::Integer);
    assert_eq!(at(&model, text, "absent =").value.kind, ValueType::Inferred);
    assert_eq!(at(&model, text, "status =").value.kind, ValueType::Integer);
    assert_eq!(at(&model, text, "index,").value.kind, ValueType::Integer);
    assert_eq!(at(&model, text, "item.count").value.kind, ValueType::Object);
    assert_eq!(at(&model, text, "spaced =").value.kind, ValueType::Boolean);
    let field = at(&model, text, "field))");
    assert_eq!(&text[field.span.start..field.span.end], "field");
    assert_eq!(field.value.kind, ValueType::Boolean);
    let invalid = text.replace("counts\n}", "item\n}");
    let invalid_model = model_from_recovered(&invalid);
    assert!(
        invalid_model
            .at(0, invalid.rfind("item\n}").unwrap())
            .is_none()
    );
}

fn model_from_recovered(text: &str) -> SemanticModel {
    analyze(
        &mettle_syntax::documentation::recover(text),
        &[REMOTE],
        &[text],
    )
}

#[test]
fn iteration_and_conversions_preserve_wrapped_sensitivity_and_native_kinds() {
    let text = "flow main {\n items = secret([{ count: 1 }])\n counts = for item in items { item.count }\n tasks = parallel { first: remote.get(), second: remote.get() }\n statuses = for key, response in tasks { response.status }\n record = { token: secret(\"dummy\"), count: 1 }\n converted = record as object\n protectedCount = converted.count\n index = secret(0)\n selected = [1][index]\n negative = -selected\n number = \"42\" as number\n return { counts: counts, statuses: statuses }\n}";
    let model = model(text);
    assert_eq!(
        at(&model, text, "item.count").value.sensitivity,
        Sensitivity::Sensitive
    );
    assert_eq!(
        at(&model, text, "response.status").value.kind,
        ValueType::Object
    );
    assert_eq!(at(&model, text, "status }").value.kind, ValueType::Integer);
    assert_eq!(
        at(&model, text, "protectedCount =").value.sensitivity,
        Sensitivity::Sensitive
    );
    assert_eq!(
        at(&model, text, "selected =").value.sensitivity,
        Sensitivity::Sensitive
    );
    assert_eq!(
        at(&model, text, "negative =").value.kind,
        ValueType::Integer
    );
    assert_eq!(at(&model, text, "number =").value.kind_name(), "number");
}

#[test]
fn incomplete_invalid_recursive_and_large_shapes_remain_bounded() {
    let text = "flow main { valid = { count: 1 }\n copy = valid\n bad = missing\n unfinished = remote.get(";
    let model = model_from_recovered(text);
    assert_eq!(at(&model, text, "valid =").value.kind, ValueType::Object);
    assert_eq!(at(&model, text, "bad =").value.kind, ValueType::Inferred);
    let text = "flow recursive = recursive()\nflow main { value = recursive()\n value }";
    assert_eq!(
        at(&model_from_recovered(text), text, "value =").value.kind,
        ValueType::Inferred
    );
    let fields = (0..100)
        .map(|index| format!("field{index}: {index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!("flow main {{ object = {{ {fields} }}\n object }}");
    let model = model_from_recovered(&text);
    let object = &at(&model, &text, "object =").value;
    assert_eq!(object.fields.len(), MAX_FIELDS);
    assert!(object.fields_truncated);
    // Shared aliases form a DAG, not an exponentially traversed shape tree.
    let mut text = "flow helper(flag) { value0 = { count: 1 }\n".to_owned();
    for index in 1..35 {
        writeln!(
            text,
            "value{index} = {{ left: value{}, right: value{} }}",
            index - 1,
            index - 1
        )
        .unwrap();
    }
    text.push_str("if (flag) { return value34 } else { return value34 }\n}\nflow main { result = helper(true)\n result }");
    assert_eq!(
        at(&model_from_recovered(&text), &text, "result =")
            .value
            .kind,
        ValueType::Object
    );
}
