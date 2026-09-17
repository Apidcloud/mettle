use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read as _};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use flow_capability::{CapabilityDescriptor, Object, Value};
use flow_compiler::{CompileError, ExecutionPlan, FlowPlan, compile_with_capabilities};
use flow_http::{DESCRIPTOR as HTTP_DESCRIPTOR, HttpCapability};
use flow_runtime::Runtime;
use flow_syntax::{Expression, ExpressionKind, Span, SyntaxError, parse, parse_value};

const HELP: &str = "\
Flow language tools

Usage:
  flow check <file>
  flow list <file> [--json]
  flow run <file> [flow-name] [--line <line>] [--arg <name=value>]... [--verbose | --raw]
  flow --help
  flow --version

Commands:
  check   Parse and validate a Flow source file
  list    List compiler-discovered runnable flows
  run     Validate the source and execute a selected flow
";

const CAPABILITIES: &[CapabilityDescriptor] = &[HTTP_DESCRIPTOR];

#[derive(Debug, Default)]
struct RunOptions {
    selector: Option<FlowSelector>,
    arguments: Vec<(String, String)>,
    output: OutputMode,
}

#[derive(Debug, Default, Eq, PartialEq)]
enum OutputMode {
    #[default]
    Concise,
    Verbose,
    Raw,
}

#[derive(Debug)]
enum FlowSelector {
    Name(String),
    Line(usize),
    Id(usize),
}

fn main() -> ExitCode {
    match run_cli(env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Usage(message)) => {
            eprintln!("error: {message}\n\n{HELP}");
            ExitCode::from(2)
        }
        Err(CliError::Failure) => ExitCode::FAILURE,
    }
}

fn run_cli(arguments: impl IntoIterator<Item = OsString>) -> Result<(), CliError> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    let Some(command) = arguments.first().and_then(|value| value.to_str()) else {
        return Err(CliError::Usage("a command is required".to_owned()));
    };
    match command {
        "--help" | "-h" if arguments.len() == 1 => {
            print!("{HELP}");
            Ok(())
        }
        "--version" | "-V" if arguments.len() == 1 => {
            println!("flow {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "check" if arguments.len() == 2 => check(Path::new(&arguments[1])),
        "list" if arguments.len() == 2 => list_flows(Path::new(&arguments[1]), false),
        "list" if arguments.len() == 3 && arguments[2] == "--json" => {
            list_flows(Path::new(&arguments[1]), true)
        }
        "run" if arguments.len() >= 2 => {
            let options = parse_run_options(&arguments[2..])?;
            run(Path::new(&arguments[1]), &options)
        }
        _ => Err(CliError::Usage(format!(
            "unknown command or invalid arguments: `{command}`"
        ))),
    }
}

fn parse_run_options(arguments: &[OsString]) -> Result<RunOptions, CliError> {
    let mut options = RunOptions::default();
    let mut cursor = 0;
    while cursor < arguments.len() {
        let argument = arguments[cursor]
            .to_str()
            .ok_or_else(|| CliError::Usage("run options must be valid UTF-8".to_owned()))?;
        match argument {
            "--line" => {
                let value = arguments
                    .get(cursor + 1)
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| CliError::Usage("`--line` requires a line number".to_owned()))?;
                let line = value.parse::<usize>().map_err(|_| {
                    CliError::Usage("`--line` requires a positive line number".to_owned())
                })?;
                if line == 0 {
                    return Err(CliError::Usage(
                        "`--line` requires a positive line number".to_owned(),
                    ));
                }
                set_selector(&mut options, FlowSelector::Line(line))?;
                cursor += 2;
            }
            "--arg" => {
                let value = arguments
                    .get(cursor + 1)
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| CliError::Usage("`--arg` requires `name=value`".to_owned()))?;
                let Some((name, value)) = value.split_once('=') else {
                    return Err(CliError::Usage("`--arg` requires `name=value`".to_owned()));
                };
                if name.is_empty() {
                    return Err(CliError::Usage(
                        "flow argument names cannot be empty".to_owned(),
                    ));
                }
                options.arguments.push((name.to_owned(), value.to_owned()));
                cursor += 2;
            }
            "--flow-id" => {
                let value = arguments
                    .get(cursor + 1)
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| CliError::Usage("`--flow-id` requires an ID".to_owned()))?;
                let id = value.parse::<usize>().map_err(|_| {
                    CliError::Usage("`--flow-id` requires a non-negative integer".to_owned())
                })?;
                set_selector(&mut options, FlowSelector::Id(id))?;
                cursor += 2;
            }
            "--verbose" | "--pretty" => {
                set_output_mode(&mut options, OutputMode::Verbose, argument)?;
                cursor += 1;
            }
            "--raw" => {
                set_output_mode(&mut options, OutputMode::Raw, argument)?;
                cursor += 1;
            }
            value if value.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown run option `{value}`")));
            }
            name => {
                set_selector(&mut options, FlowSelector::Name(name.to_owned()))?;
                cursor += 1;
            }
        }
    }
    Ok(options)
}

fn set_output_mode(
    options: &mut RunOptions,
    output: OutputMode,
    flag: &str,
) -> Result<(), CliError> {
    if options.output != OutputMode::Concise {
        return Err(CliError::Usage(format!(
            "select only one output mode; `{flag}` conflicts with an earlier output option"
        )));
    }
    options.output = output;
    Ok(())
}

fn set_selector(options: &mut RunOptions, selector: FlowSelector) -> Result<(), CliError> {
    if options.selector.is_some() {
        return Err(CliError::Usage(
            "select a flow by name or line, not both".to_owned(),
        ));
    }
    options.selector = Some(selector);
    Ok(())
}

fn check(path: &Path) -> Result<(), CliError> {
    let source = read_source(path)?;
    let syntax = parse_source(path, &source)?;
    let plan = compile_source(path, &source, &syntax)?;
    println!(
        "Checked {} ({} flow{}).",
        path.display(),
        plan.flows.len(),
        if plan.flows.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

fn list_flows(path: &Path, json: bool) -> Result<(), CliError> {
    let source = read_source(path)?;
    let syntax = parse_source(path, &source)?;
    let plan = compile_source(path, &source, &syntax)?;
    if json {
        let flows = plan
            .flows
            .iter()
            .enumerate()
            .map(|(id, flow)| {
                let (line, column) = source_location(&source, flow.span.start);
                serde_json::json!({
                    "id": id,
                    "name": flow.name,
                    "displayName": flow.display_name,
                    "parameters": flow.parameters,
                    "line": line,
                    "column": column,
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::json!({ "flows": flows }));
    } else {
        print_flow_list(&plan, &source, false);
    }
    Ok(())
}

fn run(path: &Path, options: &RunOptions) -> Result<(), CliError> {
    let source = read_source(path)?;
    let syntax = parse_source(path, &source)?;
    let plan = compile_source(path, &source, &syntax)?;
    let flow_id = select_flow(&plan, &source, options.selector.as_ref())?;
    let arguments = resolve_arguments(&plan.flows[flow_id], &options.arguments)?;
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            eprintln!("error: could not start the Flow runtime: {error}");
            CliError::Failure
        })?;
    let flow_runtime = Runtime::new(vec![Arc::new(HttpCapability::new())]);
    match async_runtime.block_on(flow_runtime.execute_selected(&plan, flow_id, arguments)) {
        Ok(value) => {
            let output = match options.output {
                OutputMode::Concise => concise_result(&value),
                OutputMode::Verbose => pretty_result(&value),
                OutputMode::Raw => value.to_string(),
            };
            println!("{output}");
            Ok(())
        }
        Err(error) => {
            eprintln!(
                "{}",
                render_diagnostic(path, &source, &error.message, error.span)
            );
            if !error.flow_stack.is_empty() {
                eprintln!("flow stack: {}", error.flow_stack.join(" -> "));
            }
            Err(CliError::Failure)
        }
    }
}

fn concise_result(value: &Value) -> String {
    let Value::Object(fields) = value else {
        return pretty_result(value);
    };
    let (Some(Value::String(method)), Some(Value::String(url)), Some(Value::Integer(status))) = (
        fields.get("method"),
        fields.get("url"),
        fields.get("status"),
    ) else {
        return pretty_result(value);
    };

    format!("{method} {url} → {status}")
}

fn pretty_result(value: &Value) -> String {
    match value {
        Value::Array(_) | Value::Object(_) | Value::Bytes(_) => {
            serde_json::to_string_pretty(&human_readable_json(value, None, false))
                .expect("Flow values always convert to JSON")
        }
        _ => value.to_string(),
    }
}

fn human_readable_json(
    value: &Value,
    field_name: Option<&str>,
    parsed_json: bool,
) -> serde_json::Value {
    const STRING_PREVIEW_CHARS: usize = 4_096;

    match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(value) => serde_json::Value::Bool(*value),
        Value::Integer(value) => serde_json::Value::Number((*value).into()),
        Value::Float(value) => serde_json::Number::from_f64(*value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(value) => {
            if field_name == Some("body") && parsed_json {
                return serde_json::Value::String(format!(
                    "<{} UTF-8 bytes; parsed content is in `json`>",
                    value.len()
                ));
            }
            let mut characters = value.chars();
            let preview = characters
                .by_ref()
                .take(STRING_PREVIEW_CHARS)
                .collect::<String>();
            if characters.next().is_some() {
                serde_json::Value::String(format!(
                    "{preview}… <truncated; {} UTF-8 bytes total>",
                    value.len()
                ))
            } else {
                serde_json::Value::String(value.clone())
            }
        }
        Value::Bytes(value) => serde_json::Value::String(format!("<{} binary bytes>", value.len())),
        Value::Duration(value) => serde_json::Value::String(format!("{}ns", value.as_nanos())),
        Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| human_readable_json(value, None, false))
                .collect(),
        ),
        Value::Object(values) => {
            let has_parsed_json = values
                .get("json")
                .is_some_and(|value| !matches!(value, Value::Null));
            serde_json::Value::Object(
                values
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            human_readable_json(value, Some(name), has_parsed_json),
                        )
                    })
                    .collect(),
            )
        }
    }
}

fn select_flow(
    plan: &ExecutionPlan,
    source: &str,
    selector: Option<&FlowSelector>,
) -> Result<usize, CliError> {
    let selected = match selector {
        Some(FlowSelector::Name(name)) => plan
            .flows
            .iter()
            .position(|flow| flow.name.as_deref() == Some(name)),
        Some(FlowSelector::Line(line)) => {
            let matches = plan
                .flows
                .iter()
                .enumerate()
                .filter(|(_, flow)| source_location(source, flow.span.start).0 == *line)
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                eprintln!("error: more than one flow starts on line {line}");
                return Err(CliError::Failure);
            }
            matches.first().copied()
        }
        Some(FlowSelector::Id(id)) => (*id < plan.flows.len()).then_some(*id),
        None => plan
            .default_flow
            .or_else(|| (plan.flows.len() == 1).then_some(0)),
    };
    if let Some(selected) = selected {
        return Ok(selected);
    }

    match selector {
        Some(FlowSelector::Name(name)) => eprintln!("error: flow `{name}` was not found"),
        Some(FlowSelector::Line(line)) => eprintln!("error: no flow starts on line {line}"),
        Some(FlowSelector::Id(id)) => eprintln!("error: no flow has compiler ID {id}"),
        None if plan.flows.is_empty() => eprintln!("error: this source contains no flows"),
        None => eprintln!("error: no default flow was found; select one by name or line"),
    }
    print_flow_list(plan, source, true);
    Err(CliError::Failure)
}

fn print_flow_list(plan: &ExecutionPlan, source: &str, error_stream: bool) {
    let mut output = String::from("Available flows:\n");
    for flow in &plan.flows {
        let (line, _) = source_location(source, flow.span.start);
        let parameters = if flow.parameters.is_empty() {
            String::new()
        } else {
            format!(" ({})", flow.parameters.join(", "))
        };
        writeln!(output, "  {line:>4}  {}{parameters}", flow.display_name)
            .expect("writing to a string cannot fail");
    }
    if error_stream {
        eprint!("{output}");
    } else {
        print!("{output}");
    }
}

fn resolve_arguments(
    flow: &FlowPlan,
    supplied: &[(String, String)],
) -> Result<Vec<Value>, CliError> {
    let mut values = HashMap::new();
    for (name, value) in supplied {
        if values.insert(name.as_str(), value.as_str()).is_some() {
            eprintln!("error: flow argument `{name}` was supplied more than once");
            return Err(CliError::Failure);
        }
    }

    let mut arguments = Vec::with_capacity(flow.parameters.len());
    for parameter in &flow.parameters {
        let Some(value) = values.remove(parameter.as_str()) else {
            eprintln!(
                "error: flow `{}` requires argument `{parameter}`",
                flow.display_name
            );
            return Err(CliError::Failure);
        };
        arguments.push(parse_argument_value(parameter, value)?);
    }
    if let Some(unknown) = values.keys().next() {
        eprintln!(
            "error: flow `{}` has no argument named `{unknown}`",
            flow.display_name
        );
        return Err(CliError::Failure);
    }
    Ok(arguments)
}

fn parse_argument_value(name: &str, source: &str) -> Result<Value, CliError> {
    match parse_value(source) {
        Ok(expression) => value_from_expression(&expression).map_err(|message| {
            eprintln!("error: invalid value for argument `{name}`: {message}");
            CliError::Failure
        }),
        Err(_error) if !looks_like_explicit_literal(source) => Ok(Value::String(source.to_owned())),
        Err(error) => {
            eprintln!(
                "error: invalid value for argument `{name}`: {} at byte {}",
                error.message, error.span.start
            );
            Err(CliError::Failure)
        }
    }
}

fn looks_like_explicit_literal(source: &str) -> bool {
    source.starts_with(['"', '[', '{'])
        || source.as_bytes().first().is_some_and(u8::is_ascii_digit)
        || matches!(source, "true" | "false" | "null")
}

fn value_from_expression(expression: &Expression) -> Result<Value, &'static str> {
    match &expression.kind {
        ExpressionKind::Null => Ok(Value::Null),
        ExpressionKind::Boolean(value) => Ok(Value::Boolean(*value)),
        ExpressionKind::Integer(value) => Ok(Value::Integer(*value)),
        ExpressionKind::Float(value) => Ok(Value::Float(*value)),
        ExpressionKind::String(value) | ExpressionKind::Name(value) => {
            Ok(Value::String(value.clone()))
        }
        ExpressionKind::DurationNanos(value) => {
            Ok(Value::Duration(std::time::Duration::from_nanos(*value)))
        }
        ExpressionKind::Array(values) => values
            .iter()
            .map(value_from_expression)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        ExpressionKind::Object(fields) => fields
            .iter()
            .map(|field| {
                Ok((
                    field.name.value.clone(),
                    value_from_expression(&field.expression)?,
                ))
            })
            .collect::<Result<Object, _>>()
            .map(Value::Object),
        ExpressionKind::Call { .. } | ExpressionKind::Member { .. } => {
            Err("flow arguments must be literal values")
        }
    }
}

fn read_source(path: &Path) -> Result<String, CliError> {
    if path == Path::new("-") {
        let mut source = String::new();
        return io::stdin()
            .read_to_string(&mut source)
            .map(|_| source)
            .map_err(|error| {
                eprintln!("error: could not read Flow source from standard input: {error}");
                CliError::Failure
            });
    }
    fs::read_to_string(path).map_err(|error| {
        eprintln!("error: could not read {}: {error}", path.display());
        CliError::Failure
    })
}

fn parse_source(path: &Path, source: &str) -> Result<flow_syntax::Program, CliError> {
    parse(source).map_err(|error: SyntaxError| {
        eprintln!(
            "{}",
            render_diagnostic(path, source, &error.message, error.span)
        );
        CliError::Failure
    })
}

fn compile_source(
    path: &Path,
    source: &str,
    syntax: &flow_syntax::Program,
) -> Result<ExecutionPlan, CliError> {
    compile_with_capabilities(syntax, CAPABILITIES).map_err(|errors: Vec<CompileError>| {
        for error in errors {
            eprintln!(
                "{}",
                render_diagnostic(path, source, &error.message, error.span)
            );
        }
        CliError::Failure
    })
}

fn source_location(source: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(source.len());
    let line_start = source[..byte].rfind('\n').map_or(0, |index| index + 1);
    let line = source[..line_start]
        .bytes()
        .filter(|character| *character == b'\n')
        .count()
        + 1;
    let column = source[line_start..byte].chars().count() + 1;
    (line, column)
}

fn render_diagnostic(path: &Path, source: &str, message: &str, span: Span) -> String {
    let start = span.start.min(source.len());
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |offset| start + offset);
    let (line_number, column) = source_location(source, start);
    let line = &source[line_start..line_end];
    let marked_end = span
        .end
        .min(line_end)
        .max(start.saturating_add(1).min(line_end));
    let width = source[start..marked_end].chars().count().max(1);
    let gutter_width = line_number.to_string().len();
    let padding = " ".repeat(column.saturating_sub(1));
    let marker = "^".repeat(width);
    format!(
        "error: {message}\n --> {}:{line_number}:{column}\n{empty:>gutter_width$} |\n{line_number:>gutter_width$} | {line}\n{empty:>gutter_width$} | {padding}{marker}",
        path.display(),
        empty = "",
    )
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    Failure,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use flow_capability::{Object, Value};

    use super::pretty_result;

    #[test]
    fn pretty_http_result_summarizes_duplicate_and_binary_bodies() {
        let value = Value::Object(Object::from([
            (
                "body".to_owned(),
                Value::String("{\"active\":true}".to_owned()),
            ),
            (
                "bodyBytes".to_owned(),
                Value::Bytes(Arc::from([1_u8, 2, 3])),
            ),
            (
                "json".to_owned(),
                Value::Object(Object::from([("active".to_owned(), Value::Boolean(true))])),
            ),
        ]));

        let output = pretty_result(&value);
        assert!(output.contains("<15 UTF-8 bytes; parsed content is in `json`>"));
        assert!(output.contains("<3 binary bytes>"));
        assert!(output.contains("\"active\": true"));
        assert!(!output.contains("[\n    1,"));
    }
}
