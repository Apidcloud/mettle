//! Shared-schema editor documentation; no protocol calls or extension-side catalogue.

use std::fmt::Write as _;

use mettle_capability::documentation::{field_reference, operation_hover, reference, signature};
use mettle_capability::{CapabilityDescriptor, FieldSchema, OperationSchema, SchemaType};
use mettle_compiler::{find_definition, resolve_documentation_flow};
use mettle_syntax::documentation::{
    CallSite, Documentation, call_site, declaration, recover, symbol,
};
use mettle_syntax::{MettleDecl, Program, Span};
use serde_json::{Value, json};

use super::{Server, byte_offset, file_uri_to_path, normalize_path, span_range};
use crate::{
    CAPABILITIES, SourceDocument, combine_parsed_sources, load_project_documents_with_overlays,
};

struct Snapshot {
    sources: Vec<SourceDocument>,
    program: Program,
    source: usize,
    byte: usize,
    namespace: String,
    namespace_uses: Vec<mettle_syntax::Spanned<String>>,
}

fn operation(name: &str) -> Option<(&'static CapabilityDescriptor, &'static OperationSchema)> {
    let (root, name) = name.split_once('.')?;
    let capability = CAPABILITIES
        .iter()
        .find(|capability| capability.name == root)?;
    Some((
        capability,
        capability
            .operations
            .iter()
            .find(|operation| operation.name == name)?,
    ))
}

fn core_item(name: &str) -> Option<&'static mettle_syntax::language::Item> {
    mettle_compiler::documentation::item(name).or_else(|| mettle_syntax::language::item(name))
}

fn uri(name: &str) -> String {
    format!(
        "mettle-doc:/{}/{}.md",
        env!("CARGO_PKG_VERSION"),
        name.replace('.', "/")
    )
}

pub(super) fn reference_query(params: &Value) -> Option<Value> {
    let name = if let Some(name) = params.get("name").and_then(Value::as_str) {
        name.to_owned()
    } else {
        let value = params.get("uri")?.as_str()?;
        value
            .strip_prefix(&format!("mettle-doc:/{}/", env!("CARGO_PKG_VERSION")))?
            .strip_suffix(".md")?
            .replace('/', ".")
    };
    let content = reference(CAPABILITIES, &name)
        .or_else(|| mettle_compiler::documentation::reference(&name))?;
    Some(json!({ "uri": uri(&name), "languageId": "markdown", "content": content }))
}

fn doc_for_flow(snapshot: &Snapshot, flow: &MettleDecl) -> Documentation {
    let parameters = flow
        .parameters
        .iter()
        .map(|parameter| parameter.value.as_str())
        .collect::<Vec<_>>();
    declaration(
        &snapshot.sources[flow.span.source].text,
        flow.span.start,
        &parameters,
    )
}

fn flow_signature(flow: &MettleDecl) -> String {
    format!(
        "{}({})",
        flow.name
            .as_ref()
            .map_or("anonymous", |name| name.value.as_str()),
        flow.parameters
            .iter()
            .map(|parameter| parameter.value.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn parameter_description(doc: &Documentation, name: &str) -> String {
    doc.parameters
        .iter()
        .find(|(parameter, _)| parameter == name)
        .map_or_else(String::new, |(_, description)| description.clone())
}

fn flow_markdown(flow: &MettleDecl, doc: &Documentation) -> String {
    let mut output = format!(
        "```mettle\nflow {}\n```\n\n{}",
        flow_signature(flow),
        doc.description
    );
    for (name, description) in &doc.parameters {
        let _ = write!(output, "\n\n**{name}** — {description}");
    }
    if !doc.returns.is_empty() {
        let _ = write!(output, "\n\n**Returns** — {}", doc.returns);
    }
    output
}

fn call_field<'a>(
    operation: &'a OperationSchema,
    call: &CallSite,
    name: &str,
) -> Option<&'a FieldSchema> {
    let mut fields = operation.options;
    for part in &call.path {
        let field = fields.iter().find(|field| field.name == part)?;
        let SchemaType::Object(nested) = field.value_type else {
            return None;
        };
        fields = nested;
    }
    fields.iter().find(|field| field.name == name)
}

pub(super) fn warnings(text: &str, program: &Program) -> Vec<(Span, String)> {
    let mut warnings = Vec::new();
    for flow in &program.flows {
        let parameters = flow
            .parameters
            .iter()
            .map(|parameter| parameter.value.as_str())
            .collect::<Vec<_>>();
        warnings.extend(declaration(text, flow.span.start, &parameters).warnings);
    }
    for context in &program.contexts {
        warnings.extend(declaration(text, context.span.start, &[]).warnings);
    }
    warnings
}

impl Server {
    fn documentation_snapshot(&self, params: &Value) -> Option<Snapshot> {
        let path = normalize_path(file_uri_to_path(
            params.pointer("/textDocument/uri")?.as_str()?,
        )?);
        let line = usize::try_from(params.pointer("/position/line")?.as_u64()?).ok()?;
        let character = usize::try_from(params.pointer("/position/character")?.as_u64()?).ok()?;
        let (sources, entry) = load_project_documents_with_overlays(&path, &self.documents).ok()?;
        let source = sources
            .iter()
            .position(|source| normalize_path(source.path.clone()) == path)?;
        let byte = byte_offset(&sources[source].text, line, character)?;
        let parsed: Vec<_> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let mut program = recover(&source.text);
                program.set_source(index);
                program
            })
            .collect();
        let namespace = parsed[source]
            .namespace
            .as_ref()
            .map_or_else(String::new, |namespace| namespace.value.clone());
        let namespace_uses = parsed[source].namespace_uses.clone();
        let project = combine_parsed_sources(sources, parsed, entry);
        Some(Snapshot {
            sources: project.sources,
            program: project.program,
            source,
            byte,
            namespace,
            namespace_uses,
        })
    }

    pub(super) fn documentation_query(&self, params: &Value, method: &str) -> Option<Value> {
        let snapshot = self.documentation_snapshot(params)?;
        if method == "textDocument/signatureHelp" {
            return signature_help(&snapshot);
        }
        let (markdown, range, reference) = hover(&snapshot)?;
        let mut result = json!({ "contents": { "kind": "markdown", "value": markdown }, "range": span_range(&snapshot.sources[snapshot.source].text, range) });
        if let Some(reference) = reference {
            result["mettleReference"] = json!(reference);
        }
        Some(result)
    }

    pub(super) fn builtin_documentation_target(&self, params: &Value) -> Option<Value> {
        let snapshot = self.documentation_snapshot(params)?;
        let (_, hovered_span, reference) = hover(&snapshot)?;
        let reference = reference?;
        let content = reference_query(&json!({ "uri": reference }))?;
        let text = &snapshot.sources[snapshot.source].text;
        let (name, span) =
            symbol(text, snapshot.byte).unwrap_or_else(|| (String::new(), hovered_span));
        let nested = call_site(text, span.start)
            .filter(|call| !call.path.is_empty())
            .map(|call| format!("- `{}.{name}`", call.path.join(".")));
        let line = content["content"]
            .as_str()?
            .lines()
            .position(|line| {
                line == format!("### {name}")
                    || nested
                        .as_ref()
                        .is_some_and(|prefix| line.starts_with(prefix))
            })
            .unwrap_or(0);
        Some(
            json!({ "uri": reference, "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 0 } } }),
        )
    }
}

fn hover(snapshot: &Snapshot) -> Option<(String, Span, Option<String>)> {
    let text = &snapshot.sources[snapshot.source].text;
    if let Some((name, span)) = mettle_syntax::language::at(text, snapshot.byte) {
        let content = core_item(name)
            .map(|item| item.hover())
            .or_else(|| mettle_syntax::language::reference(name))?;
        return Some((content, span, Some(uri(&format!("language.{name}")))));
    }
    let (name, range) = symbol(text, snapshot.byte)?;
    let definition = find_definition(&snapshot.program, snapshot.source, snapshot.byte);
    // Locals/context bindings shadow constants, just as in compilation. Operation
    // call targets are always capability-qualified, regardless of such bindings.
    let is_call = mettle_syntax::documentation::is_call_target(text, range);
    let sources = snapshot
        .sources
        .iter()
        .map(|source| source.text.as_str())
        .collect::<Vec<_>>();
    let semantics = (!is_call)
        .then(|| mettle_compiler::semantics::analyze(&snapshot.program, CAPABILITIES, &sources));
    let semantic = semantics
        .as_ref()
        .and_then(|model| model.at(snapshot.source, snapshot.byte));
    if is_call && let Some(item) = mettle_compiler::documentation::item(&name) {
        return Some((item.hover(), range, Some(uri(&format!("language.{name}")))));
    }
    if (definition.is_none() && semantic.is_none()) || is_call {
        if let Some((capability, operation)) = operation(&name) {
            return Some((
                operation_hover(capability, operation),
                range,
                Some(uri(&name)),
            ));
        }
        if let Some(content) = reference(CAPABILITIES, &name) {
            return Some((content, range, Some(uri(&name))));
        }
    }
    // Field documentation belongs to the innermost operation, including nested
    // schema fields such as tls.verifyCertificates, never arbitrary body keys.
    if text.get(range.end..)?.trim_start().starts_with(':')
        && let Some(call) = call_site(text, range.start)
    {
        if let Some((_, operation)) = operation(&call.name) {
            if let Some(field) = call_field(operation, &call, &name) {
                return Some((field_reference(field), range, Some(uri(&call.name))));
            }
            if let Some(index) = operation
                .parameter_names
                .iter()
                .position(|parameter| *parameter == name)
            {
                return Some((
                    format!(
                        "**{name}** — {}\n\n{}",
                        operation.parameters[index].name(),
                        operation
                            .documentation
                            .parameters
                            .get(index)
                            .copied()
                            .unwrap_or("")
                    ),
                    range,
                    Some(uri(&call.name)),
                ));
            }
        } else if let Some(item) = core_item(&call.name) {
            let (_, description) = item
                .parameters
                .iter()
                .find(|(parameter, _)| *parameter == name)?;
            return Some((
                format!("**{name}**\n\n{description}"),
                range,
                Some(uri(&format!("language.{}", item.name))),
            ));
        } else if let Some(id) = resolve_documentation_flow(
            &snapshot.program,
            &call.name,
            &snapshot.namespace,
            &snapshot.namespace_uses,
        ) {
            let flow = &snapshot.program.flows[id];
            if flow
                .parameters
                .iter()
                .any(|parameter| parameter.value == name)
            {
                return Some((
                    format!(
                        "**{name}**\n\n{}",
                        parameter_description(&doc_for_flow(snapshot, flow), &name)
                    ),
                    range,
                    None,
                ));
            }
        }
    }
    if let Some(info) = semantic {
        let content = local_hover(snapshot, definition, info);
        return Some((content, info.span, info.value.operation.as_deref().map(uri)));
    }
    definition.and_then(|target| declaration_hover(snapshot, target, &name, range))
}

fn local_hover(
    snapshot: &Snapshot,
    definition: Option<Span>,
    info: &mettle_compiler::semantics::SymbolInfo,
) -> String {
    let text = &snapshot.sources[snapshot.source].text;
    let label = &text[info.span.start..info.span.end];
    let mut content = semantic_hover(label, info);
    if let Some(target) = definition {
        for flow in &snapshot.program.flows {
            if let Some(parameter) = flow
                .parameters
                .iter()
                .find(|parameter| parameter.span == target)
            {
                let description =
                    parameter_description(&doc_for_flow(snapshot, flow), &parameter.value);
                if !description.is_empty() {
                    let _ = write!(content, "\n\n{description}");
                }
            }
        }
    }
    content
}

fn semantic_hover(name: &str, symbol: &mettle_compiler::semantics::SymbolInfo) -> String {
    use mettle_compiler::semantics::{Sensitivity, SymbolRole};
    let value = &symbol.value;
    let mut content = format!("```mettle\n{name}: {}\n```", value.kind_name());
    if symbol.role == SymbolRole::Parameter {
        content.push_str("\n\nFlow parameter; its kind depends on the caller.");
    }
    if let Some(operation) = &value.operation {
        let _ = write!(content, "\n\nResult information from `{operation}`.");
    }
    if !value.description.is_empty() {
        let _ = write!(content, "\n\n{}", value.description);
    }
    if !value.fields.is_empty() {
        content.push_str("\n\nKnown fields:\n");
        for field in value.fields.iter() {
            let _ = write!(content, "\n- `{}: {}`", field.name, field.value.kind_name());
            if !field.value.description.is_empty() {
                let _ = write!(content, " — {}", field.value.description);
            }
        }
        content.push('\n');
    }
    if value.fields_truncated {
        content.push_str("\n\nAdditional fields omitted from this preview.");
    }
    match value.sensitivity {
        Sensitivity::Sensitive => content.push_str(
            "\n\nSensitive value (or contains sensitive fields); contents are never shown here.",
        ),
        Sensitivity::Unknown => content.push_str(
            "\n\nSensitivity is determined at runtime; contents are never inspected here.",
        ),
        Sensitivity::Public => {}
    }
    content
}

fn declaration_hover(
    snapshot: &Snapshot,
    target: Span,
    name: &str,
    range: Span,
) -> Option<(String, Span, Option<String>)> {
    for flow in &snapshot.program.flows {
        let doc = doc_for_flow(snapshot, flow);
        if flow.name.as_ref().is_some_and(|name| name.span == target) {
            return Some((flow_markdown(flow, &doc), range, None));
        }
        if let Some(parameter) = flow
            .parameters
            .iter()
            .find(|parameter| parameter.span == target)
        {
            return Some((
                format!(
                    "**{}**\n\n{}",
                    parameter.value,
                    parameter_description(&doc, &parameter.value)
                ),
                range,
                None,
            ));
        }
    }
    if let Some(context) = snapshot.program.contexts.iter().find(|context| {
        context
            .name
            .as_ref()
            .is_some_and(|name| name.span == target)
    }) {
        let doc = declaration(
            &snapshot.sources[context.span.source].text,
            context.span.start,
            &[],
        );
        return Some((
            format!("**context {name}**\n\n{}", doc.description),
            range,
            None,
        ));
    }
    None
}

fn signature_help(snapshot: &Snapshot) -> Option<Value> {
    let text = &snapshot.sources[snapshot.source].text;
    let call = call_site(text, snapshot.byte)?;
    let (label, description, names, parameters) = if let Some((capability, operation)) =
        operation(&call.name)
    {
        let names = operation
            .parameter_names
            .iter()
            .map(|name| (*name).to_owned())
            .chain(operation.options.iter().map(|field| field.name.to_owned()))
            .collect::<Vec<_>>();
        let parameters = operation.parameter_names.iter().zip(operation.parameters).enumerate().map(|(index, (name, kind))| json!({"label": format!("{name}: {}", kind.name()), "documentation": { "kind": "markdown", "value": operation.documentation.parameters.get(index).copied().unwrap_or("") }}))
            .chain(operation.options.iter().map(|field| json!({ "label": format!("{}?: {}", field.name, field.value_type.name()), "documentation": { "kind": "markdown", "value": field_reference(field) } }))).collect::<Vec<_>>();
        (
            signature(capability, operation),
            operation.documentation.summary.to_owned(),
            names,
            parameters,
        )
    } else if let Some(item) = core_item(&call.name).filter(|item| !item.parameters.is_empty()) {
        let names = item
            .parameters
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        let parameters = item.parameters.iter().map(|(name, description)| json!({"label": name, "documentation": {"kind": "markdown", "value": description}})).collect();
        (
            item.form.to_owned(),
            item.description.to_owned(),
            names,
            parameters,
        )
    } else {
        let id = resolve_documentation_flow(
            &snapshot.program,
            &call.name,
            &snapshot.namespace,
            &snapshot.namespace_uses,
        )?;
        let flow = &snapshot.program.flows[id];
        let doc = doc_for_flow(snapshot, flow);
        let names = flow
            .parameters
            .iter()
            .map(|parameter| parameter.value.clone())
            .collect::<Vec<_>>();
        let parameters = names.iter().map(|name| json!({ "label": name, "documentation": { "kind": "markdown", "value": parameter_description(&doc, name) } })).collect();
        (flow_signature(flow), doc.description, names, parameters)
    };
    let active = if let Some(name) = &call.named {
        names.iter().position(|parameter| parameter == name)
    } else {
        (call.argument < names.len()).then_some(call.argument)
    };
    let mut signature = json!({ "label": label, "documentation": { "kind": "markdown", "value": description }, "parameters": parameters });
    if let Some(active) = active {
        signature["activeParameter"] = json!(active);
    }
    Some(json!({ "signatures": [signature], "activeSignature": 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture {
        server: Server,
        path: PathBuf,
        directory: PathBuf,
    }
    impl Fixture {
        fn new(text: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "mettle-docs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            let path = directory.join("main.mettle");
            let mut server = Server::default();
            server.documents.insert(path.clone(), text.to_owned());
            Self {
                server,
                path,
                directory,
            }
        }
        fn params(&self, needle: &str) -> Value {
            let text = &self.server.documents[&self.path];
            let byte = if needle.is_empty() {
                text.len()
            } else {
                text.find(needle).unwrap()
            };
            json!({ "textDocument": { "uri": super::super::path_to_file_uri(&self.path) }, "position": span_range(text, Span::new(byte, byte))["start"] })
        }
        fn query(&self, method: &str, needle: &str) -> Value {
            self.server
                .documentation_query(&self.params(needle), method)
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    #[test]
    fn variable_and_field_hovers_use_normalized_native_shapes_without_execution() {
        let text = "// 😀 Unsaved, no server or environment needed.\ncontext settings { token: senv(\"EDITOR_UNSET_TOKEN\"), json: { mediaType: \"not-the-codec\" } }\nuse context settings\n/// @param input Caller-owned input.\nflow identity(input) = input\nflow request(url) = http.post(url, body: { name: \"Ada\" })\nflow main {\n created = request(\"http://127.0.0.1:1/unreachable\")\n alias = created\n status = alias.status\n payload = alias.body\n uncertain = payload.name\n credential = token\n local = json.mediaType\n unknown = identity(42)\n created\n}";
        let fixture = Fixture::new(text);
        let hover = fixture.query("textDocument/hover", "created =");
        let markdown = hover["contents"]["value"].as_str().unwrap();
        assert!(markdown.contains("created: object"));
        assert!(markdown.contains("status: integer"));
        assert!(markdown.contains("mediaType: string or null"));
        assert!(markdown.contains("body: value (kind determined at runtime)"));
        assert!(!markdown.contains("json:"));
        assert_eq!(hover["mettleReference"], uri("http.post"));
        let hover = fixture.query("textDocument/hover", "status\n payload");
        assert_eq!(
            hover["range"],
            span_range(
                text,
                Span::new(
                    text.find("status\n payload").unwrap(),
                    text.find("status\n payload").unwrap() + 6
                )
            )
        );
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("Numeric HTTP response status")
        );
        assert_eq!(hover["mettleReference"], uri("http.post"));
        for needle in ["payload =", "uncertain =", "unknown ="] {
            assert!(
                fixture.query("textDocument/hover", needle)["contents"]["value"]
                    .as_str()
                    .unwrap()
                    .contains("kind determined at runtime")
            );
        }
        let secret = fixture.query("textDocument/hover", "credential =");
        assert!(
            secret["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("Sensitive value")
        );
        assert!(!secret.to_string().contains("EDITOR_UNSET_TOKEN"));
        let local = fixture.query("textDocument/hover", "json.mediaType");
        assert!(
            local["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("json: object")
        );
        assert!(local.get("mettleReference").is_none());
        let parameter = fixture.query("textDocument/hover", "input\nflow request");
        assert!(
            parameter["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("Caller-owned input.")
        );
    }

    #[test]
    fn incomplete_variable_hovers_preserve_scope_and_project_overlays() {
        let mut fixture = Fixture::new(
            "use namespace remote\nflow main { response = fetch()\n alias = response\n unfinished = http.get(",
        );
        std::fs::write(
            fixture.directory.join("mettle.toml"),
            "name = \"hover-project\"\n",
        )
        .unwrap();
        let helper = fixture.directory.join("remote.mettle");
        std::fs::write(
            &helper,
            "namespace remote\nflow fetch = { diskOnly: true }\n",
        )
        .unwrap();
        fixture.server.documents.insert(
            helper.clone(),
            "namespace remote\nflow fetch = http.get(\"http://127.0.0.1:1\")\n".into(),
        );
        let hover = fixture.query("textDocument/hover", "alias =");
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("status: integer")
        );
        assert!(!hover.to_string().contains("diskOnly"));
        fixture.server.documents.insert(
            helper,
            "namespace remote\nflow fetch = { updated: true }\n".into(),
        );
        let hover = fixture.query("textDocument/hover", "alias =");
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("updated: boolean")
        );
        assert!(hover.get("mettleReference").is_none());
        fixture.server.documents.insert(
            fixture.path.clone(),
            "flow main { if (true) { scoped = 1\n echo(scoped) }\n scoped }".into(),
        );
        let text = &fixture.server.documents[&fixture.path];
        let byte = text.rfind("scoped }").unwrap();
        let params = json!({"textDocument": {"uri": super::super::path_to_file_uri(&fixture.path)}, "position": span_range(text, Span::new(byte, byte))["start"]});
        assert!(
            fixture
                .server
                .documentation_query(&params, "textDocument/hover")
                .is_none()
        );
    }

    #[test]
    fn all_reserved_words_have_hover_and_virtual_reference_coverage() {
        for item in mettle_syntax::language::KEYWORDS {
            let mut fixture = Fixture::new(item.example);
            let program = mettle_syntax::parse(item.example).unwrap();
            mettle_compiler::compile_with_capabilities(&program, CAPABILITIES)
                .unwrap_or_else(|errors| panic!("{}: {errors:?}", item.name));
            let byte = item
                .example
                .char_indices()
                .map(|(byte, _)| byte)
                .find(|byte| {
                    mettle_syntax::language::at(item.example, *byte)
                        .is_some_and(|(name, _)| name == item.name)
                })
                .unwrap();
            let params = json!({ "textDocument": {"uri": super::super::path_to_file_uri(&fixture.path)}, "position": span_range(item.example, Span::new(byte, byte))["start"] });
            let hover = fixture
                .server
                .documentation_query(&params, "textDocument/hover")
                .unwrap();
            assert!(
                hover["contents"]["value"]
                    .as_str()
                    .unwrap()
                    .contains(item.description)
            );
            assert_eq!(
                hover["mettleReference"],
                uri(&format!("language.{}", item.name))
            );
            fixture.server.virtual_documentation = true;
            let target = fixture
                .server
                .navigation(&params, "textDocument/definition")
                .unwrap();
            let reference = reference_query(&json!({"uri":target["uri"]})).unwrap();
            assert!(
                reference["content"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("# {}\n", item.name))
            );
        }
    }

    #[test]
    fn core_helpers_signatures_kinds_and_non_code_positions_are_documented_correctly() {
        for item in mettle_compiler::documentation::BUILTINS {
            let fixture = Fixture::new(item.example);
            let hover = fixture.query("textDocument/hover", &format!("{}(", item.name));
            assert!(
                hover["contents"]["value"]
                    .as_str()
                    .unwrap()
                    .contains(item.description)
            );
        }
        for text in [
            "flow main { assert(true, ",
            "flow main { fail(",
            "flow main = retry(attempts: 3, delay: ",
            "flow main = env(",
        ] {
            let fixture = Fixture::new(text);
            let signature = fixture.query("textDocument/signatureHelp", "");
            assert!(
                !signature["signatures"][0]["parameters"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            if text.contains("assert") || text.contains("retry") {
                assert_eq!(signature["signatures"][0]["activeParameter"], 1);
            }
        }
        let fixture = Fixture::new("flow main = retry(attempts: 3, delay: 1ms) { 42 }");
        let hover = fixture.query("textDocument/hover", "attempts:");
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("including the first run")
        );
        let fixture = Fixture::new("flow main = 42 is number");
        assert!(
            fixture.query("textDocument/hover", "number")["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("integer or a finite")
        );
        let fixture = Fixture::new("flow main = (secret)(\"demo-token\")");
        assert_eq!(
            fixture.query("textDocument/hover", "secret")["mettleReference"],
            uri("language.secret")
        );
        let fixture = Fixture::new("flow main { echo = \"local\"\n echo }");
        let hover = fixture.query("textDocument/hover", "echo }");
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("echo: string")
        );
        assert!(hover.get("mettleReference").is_none());
        for text in [
            "// assert(true)\nflow main = 1",
            "flow main = \"use context\"",
            "flow main = \"echo(hello)\"",
        ] {
            let fixture = Fixture::new(text);
            let needle = if text.contains("assert") {
                "assert"
            } else if text.contains("use") {
                "use"
            } else {
                "echo"
            };
            assert!(
                fixture
                    .server
                    .documentation_query(&fixture.params(needle), "textDocument/hover")
                    .is_none()
            );
        }
        assert!(reference_query(&json!({"name":"language.then"})).is_none());
    }

    #[test]
    fn builtin_hover_options_constants_and_reference_use_registered_schemas() {
        let fixture = Fixture::new(
            "// 😀 UTF-16 positions\nflow main = http.post(\"/users\", body: { ok: true }, mediaType: json.mediaType, tls: { verifyCertificates: true })",
        );
        let hover = fixture.query("textDocument/hover", "post(");
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("HTTP POST")
        );
        assert_eq!(hover["mettleReference"], uri("http.post"));
        let markdown = hover["contents"]["value"].as_str().unwrap();
        assert!(markdown.starts_with("```text\nhttp.post(url: string, …) -> object\n```"));
        assert!(!markdown.contains("maxResponseBytes?:"));
        assert!(!markdown.contains("maxBodyBytes?:"));
        assert!(markdown.contains("encoded as JSON"));
        assert!(markdown.contains("error statuses are returned normally"));
        assert!(markdown.contains("```mettle\nhttp.post("));
        let body = fixture.query("textDocument/hover", "body:");
        assert!(
            body["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("sent unchanged")
        );
        let tls = fixture.query("textDocument/hover", "verifyCertificates:");
        assert!(
            tls["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("Default: `true`")
        );
        let target = fixture
            .server
            .builtin_documentation_target(&fixture.params("post("))
            .unwrap();
        let reference = reference_query(&json!({ "uri": target["uri"] })).unwrap();
        let content = reference["content"].as_str().unwrap();
        assert!(content.contains("maxBodyBytes?: integer"));
        assert!(content.contains("## Result") && content.contains("maxBodyBytes"));
        assert!(content.contains("10485760 bytes buffered"));
        assert!(reference_query(&json!({ "uri": "file:///private" })).is_none());
        assert!(reference_query(&json!({ "name": "http.unknown" })).is_none());
        let constant = fixture.query("textDocument/hover", "json.mediaType");
        assert!(
            constant["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("application/json")
        );
    }

    #[test]
    fn user_docs_parameter_hover_and_unfinished_signature_help() {
        let text = "/// Fetch a user.\n/// @param id User identifier.\n/// @returns A response.\nflow getUser(id) = id\nflow main = getUser(id: 42)";
        let mut fixture = Fixture::new(text);
        let hover = fixture.query("textDocument/hover", "getUser(id: 42)");
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("Fetch a user")
        );
        let argument = fixture.query("textDocument/hover", "id: 42");
        assert!(
            argument["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("User identifier")
        );
        fixture
            .server
            .documents
            .insert(fixture.path.clone(), text.replace("id: 42)", "id: "));
        let signature = fixture.query("textDocument/signatureHelp", "");
        assert_eq!(signature["signatures"][0]["activeParameter"], 0);
        assert!(
            signature["signatures"][0]["parameters"][0]["documentation"]["value"]
                .as_str()
                .unwrap()
                .contains("User identifier")
        );
        fixture.server.documents.insert(
            fixture.path.clone(),
            "flow main = http.post(\"/users\", body: { x: [1, 2] }, mediaType: ".into(),
        );
        let signature = fixture.query("textDocument/signatureHelp", "");
        let active = signature["signatures"][0]["activeParameter"]
            .as_u64()
            .unwrap();
        assert!(
            signature["signatures"][0]["parameters"][usize::try_from(active).unwrap()]["label"]
                .as_str()
                .unwrap()
                .starts_with("mediaType")
        );
    }

    #[test]
    fn project_docs_use_unsaved_sources_and_namespace_rules_even_in_incomplete_calls() {
        let mut fixture =
            Fixture::new("namespace client\nuse namespace users\nflow main = getUser(id: ");
        std::fs::write(
            fixture.directory.join("mettle.toml"),
            "name = \"docs-fixture\"\n",
        )
        .unwrap();
        let helper = fixture.directory.join("users.mettle");
        std::fs::write(
            &helper,
            "namespace users\n/// Old documentation.\nflow getUser(id) = id\n",
        )
        .unwrap();
        fixture.server.documents.insert(helper.clone(), "namespace users\n/// Unsaved user lookup.\n/// @param id Identifier from the caller.\nflow getUser(id) = id\n".into());
        let signature = fixture.query("textDocument/signatureHelp", "");
        assert_eq!(
            signature["signatures"][0]["documentation"]["value"],
            "Unsaved user lookup."
        );
        assert_eq!(
            signature["signatures"][0]["parameters"][0]["documentation"]["value"],
            "Identifier from the caller."
        );
        fixture.server.documents.insert(
            fixture.path.clone(),
            "namespace client\nuse namespace users\nflow main = getUser(id: 1)\n".into(),
        );
        let hover = fixture.query("textDocument/hover", "getUser(");
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("Unsaved user lookup.")
        );
        let target = fixture
            .server
            .navigation(&fixture.params("getUser("), "textDocument/definition")
            .unwrap();
        assert_eq!(target["uri"], super::super::path_to_file_uri(&helper));
        fixture.server.documents.insert(
            fixture.directory.join("other.mettle"),
            "namespace other\nflow getUser(id) = id\n".into(),
        );
        fixture.server.documents.insert(
            fixture.path.clone(),
            "use namespace users\nuse namespace other\nflow main = getUser(id: ".into(),
        );
        assert!(
            fixture
                .server
                .documentation_query(&fixture.params(""), "textDocument/signatureHelp")
                .is_none()
        );
    }

    #[test]
    fn documentation_warnings_are_nonblocking_and_clear_after_correction() {
        let mut fixture = Fixture::new(
            "/// @param typo Incorrect.\nflow helper(id) = id\nflow main = helper(1)\n",
        );
        let diagnostics = fixture.server.project_diagnostics(&fixture.path).unwrap();
        let warnings = &diagnostics[&fixture.path];
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0]["severity"], 2);
        assert_eq!(warnings[0]["code"], "documentation");
        let text = fixture.server.documents[&fixture.path].replace("@param typo", "@param id");
        fixture.server.documents.insert(fixture.path.clone(), text);
        assert!(
            fixture.server.project_diagnostics(&fixture.path).unwrap()[&fixture.path].is_empty()
        );
    }

    #[test]
    fn virtual_definitions_are_opt_in_but_hover_is_portable() {
        let mut fixture = Fixture::new("flow main = http.get(\"/health\")");
        let params = fixture.params("get(");
        assert!(
            fixture
                .server
                .navigation(&params, "textDocument/definition")
                .is_none()
        );
        assert!(
            fixture
                .server
                .documentation_query(&params, "textDocument/hover")
                .is_some()
        );
        let mut output = Vec::new();
        fixture.server.handle(&json!({"id": 1, "method": "initialize", "params": {"capabilities": {"experimental": {"mettleDocumentation": true}}}}), &mut output).unwrap();
        let target = fixture
            .server
            .navigation(&params, "textDocument/definition")
            .unwrap();
        assert_eq!(target["uri"], uri("http.get"));
        assert!(
            fixture
                .server
                .navigation(&params, "textDocument/implementation")
                .is_none()
        );
    }

    #[test]
    fn shadowing_comments_doc_warnings_and_examples_are_safe() {
        let fixture =
            Fixture::new("flow main { json = { mediaType: \"local\" }\n json.mediaType\n}");
        let hover = fixture.query("textDocument/hover", "json.mediaType");
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("json: object")
        );
        assert!(hover.get("mettleReference").is_none());
        let text = "/// @param wrong Typo.\nflow f(id) = id";
        let program = mettle_syntax::parse(text).unwrap();
        assert_eq!(warnings(text, &program).len(), 1);
        for capability in CAPABILITIES {
            assert!(!capability.description.is_empty());
            for operation in capability.operations {
                let hover = operation_hover(capability, operation);
                let reference = reference(
                    CAPABILITIES,
                    &format!("{}.{}", capability.name, operation.name),
                )
                .unwrap();
                let compact = hover.split("```").nth(1).unwrap();
                for option in operation.options {
                    let label = format!("{}?: {}", option.name, option.value_type.name());
                    assert!(!compact.contains(&label));
                    assert!(signature(capability, operation).contains(&label));
                    assert!(reference.contains(&label));
                }
                for note in operation.documentation.notes {
                    assert!(hover.contains(note));
                    assert!(reference.contains(note));
                }
                assert!(hover.contains(&format!(
                    "```mettle\n{}\n```",
                    operation.documentation.example
                )));
                assert_eq!(
                    operation.parameters.len(),
                    operation.documentation.parameters.len()
                );
                assert!(
                    operation
                        .options
                        .iter()
                        .all(|field| !field.description.is_empty())
                );
                let program = mettle_syntax::parse(&format!(
                    "flow main = {}",
                    operation.documentation.example
                ))
                .unwrap();
                mettle_compiler::compile_with_capabilities(&program, CAPABILITIES).unwrap();
            }
        }
    }
}
