use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::future::Future as _;
use std::io::{self, IsTerminal as _, Read as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use mettle_capability::{CapabilityDescriptor, Object, Value};
use mettle_compiler::{
    CompileError, DeclarationKind, ExecutionPlan, MettlePlan, compile_with_capabilities,
};
use mettle_http::{DESCRIPTOR as HTTP_DESCRIPTOR, HttpCapability};
use mettle_runtime::{Runtime, RuntimeError};
use mettle_syntax::{Expression, ExpressionKind, Span, SyntaxError, parse, parse_value};

const HELP: &str = "\
Mettle language tools

Usage:
  mettle check <file>
  mettle list <file> [--json]
  mettle run <file> [flow-name] [--all | --line <line>] [--arg <name=value>]... [--profile <name>] [output options]
  mettle test <file> [--profile <name>] [--verbose | --quiet | --output json] [--no-progress] [--no-color]
  mettle lsp
  mettle --help
  mettle --version

Commands:
  check   Parse and validate a Mettle source file
  list    List compiler-discovered runnable flows
  run     Validate the source and execute a selected flow, or every zero-argument flow
  test    Execute tests in the selected file; fail if none are declared
  lsp     Start the Mettle language server over standard input/output

Run output options:
  --profile NAME  Overlay .env.NAME from the entry folder and project root
  --verbose       Show the complete result and operation details
  --quiet         Print only the final flow status
  --raw           Print only the returned Mettle value
  --output json   Print a structured execution report
  --no-progress   Disable the interactive workload display
  --no-color      Disable ANSI colors
";

mod env_file;
mod lsp;
mod report;

use report::{CliObserver, ExecutionReport, display_duration, failure_summary, raw_value};

const CAPABILITIES: &[CapabilityDescriptor] = &[HTTP_DESCRIPTOR];

#[derive(Debug)]
struct RunOptions {
    selector: Option<MettleSelector>,
    all: bool,
    arguments: Vec<(String, String)>,
    profile: Option<String>,
    output: OutputMode,
    progress: bool,
    color: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            selector: None,
            all: false,
            arguments: Vec::new(),
            profile: None,
            output: OutputMode::Human,
            progress: true,
            color: true,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum OutputMode {
    Human,
    Verbose,
    Quiet,
    Raw,
    Json,
}

#[derive(Debug)]
enum MettleSelector {
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
        Err(CliError::Interrupted) => ExitCode::from(130),
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
            println!("mettle {}", env!("CARGO_PKG_VERSION"));
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
        "test" if arguments.len() >= 2 => {
            let options = parse_run_options(&arguments[2..])?;
            run_tests(Path::new(&arguments[1]), &options)
        }
        "lsp" if arguments.len() == 1 => lsp::run(),
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
            "--all" => {
                if options.all {
                    return Err(CliError::Usage(
                        "`--all` was supplied more than once".to_owned(),
                    ));
                }
                if options.selector.is_some() {
                    return Err(CliError::Usage(
                        "`--all` cannot be combined with a flow name, line, or ID".to_owned(),
                    ));
                }
                options.all = true;
                cursor += 1;
            }
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
                set_selector(&mut options, MettleSelector::Line(line))?;
                cursor += 2;
            }
            "--arg" => {
                options
                    .arguments
                    .push(parse_named_argument(arguments.get(cursor + 1))?);
                cursor += 2;
            }
            "--profile" => {
                parse_profile_option(arguments.get(cursor + 1), &mut options)?;
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
                set_selector(&mut options, MettleSelector::Id(id))?;
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
            "--quiet" => {
                set_output_mode(&mut options, OutputMode::Quiet, argument)?;
                cursor += 1;
            }
            "--output" => {
                parse_output_format(arguments.get(cursor + 1), &mut options)?;
                cursor += 2;
            }
            "--no-progress" => {
                options.progress = false;
                cursor += 1;
            }
            "--no-color" => {
                options.color = false;
                cursor += 1;
            }
            value if value.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown run option `{value}`")));
            }
            name => {
                set_selector(&mut options, MettleSelector::Name(name.to_owned()))?;
                cursor += 1;
            }
        }
    }
    Ok(options)
}

fn parse_output_format(value: Option<&OsString>, options: &mut RunOptions) -> Result<(), CliError> {
    let value = value
        .and_then(|value| value.to_str())
        .ok_or_else(|| CliError::Usage("`--output` requires `json`".to_owned()))?;
    if value != "json" {
        return Err(CliError::Usage(format!(
            "unsupported output format `{value}`; expected `json`"
        )));
    }
    set_output_mode(options, OutputMode::Json, "--output json")
}

fn valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn parse_profile_option(
    value: Option<&OsString>,
    options: &mut RunOptions,
) -> Result<(), CliError> {
    let value = value
        .and_then(|value| value.to_str())
        .ok_or_else(|| CliError::Usage("`--profile` requires a name".to_owned()))?;
    if !valid_profile_name(value) {
        return Err(CliError::Usage(
            "profile names must use letters, digits, `_`, or `-`".to_owned(),
        ));
    }
    if options.profile.replace(value.to_owned()).is_some() {
        return Err(CliError::Usage(
            "`--profile` was supplied more than once".to_owned(),
        ));
    }
    Ok(())
}

fn parse_named_argument(value: Option<&OsString>) -> Result<(String, String), CliError> {
    let value = value
        .and_then(|value| value.to_str())
        .ok_or_else(|| CliError::Usage("`--arg` requires `name=value`".to_owned()))?;
    let (name, value) = value
        .split_once('=')
        .ok_or_else(|| CliError::Usage("`--arg` requires `name=value`".to_owned()))?;
    if name.is_empty() {
        return Err(CliError::Usage(
            "flow argument names cannot be empty".to_owned(),
        ));
    }
    Ok((name.to_owned(), value.to_owned()))
}

fn set_output_mode(
    options: &mut RunOptions,
    output: OutputMode,
    flag: &str,
) -> Result<(), CliError> {
    if options.output != OutputMode::Human {
        return Err(CliError::Usage(format!(
            "select only one output mode; `{flag}` conflicts with an earlier output option"
        )));
    }
    options.output = output;
    Ok(())
}

fn set_selector(options: &mut RunOptions, selector: MettleSelector) -> Result<(), CliError> {
    if options.all {
        return Err(CliError::Usage(
            "a flow name, line, or ID cannot be combined with `--all`".to_owned(),
        ));
    }
    if options.selector.is_some() {
        return Err(CliError::Usage(
            "select a flow by name or line, not both".to_owned(),
        ));
    }
    options.selector = Some(selector);
    Ok(())
}

fn check(path: &Path) -> Result<(), CliError> {
    let project = load_project(path)?;
    let plan = compile_project(&project)?;
    let flows = plan
        .flows
        .iter()
        .filter(|flow| flow.kind == DeclarationKind::Flow)
        .count();
    let tests = plan.flows.len() - flows;
    if tests == 0 {
        println!(
            "Checked {} ({flows} flow{}).",
            path.display(),
            if flows == 1 { "" } else { "s" }
        );
    } else {
        println!(
            "Checked {} ({flows} flow{}, {tests} test{}).",
            path.display(),
            if flows == 1 { "" } else { "s" },
            if tests == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

fn list_flows(path: &Path, json: bool) -> Result<(), CliError> {
    let project = load_project(path)?;
    let plan = compile_project(&project)?;
    if json {
        let flows = plan
            .flows
            .iter()
            .enumerate()
            .filter(|(_, flow)| flow.kind == DeclarationKind::Flow)
            .map(|(id, flow)| {
                let source = &project.sources[flow.span.source];
                let (line, column) = source_location(&source.text, flow.span.start);
                serde_json::json!({
                    "id": id,
                    "name": flow.name,
                    "displayName": flow.display_name,
                    "parameters": flow.parameters,
                    "line": line,
                    "column": column,
                    "path": source.path,
                })
            })
            .collect::<Vec<_>>();
        let tests = plan
            .flows
            .iter()
            .enumerate()
            .filter(|(_, flow)| flow.kind == DeclarationKind::Test)
            .map(|(id, test)| {
                let source = &project.sources[test.span.source];
                let (line, column) = source_location(&source.text, test.span.start);
                serde_json::json!({
                    "id": id,
                    "name": test.display_name,
                    "line": line,
                    "column": column,
                    "path": source.path,
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::json!({ "flows": flows, "tests": tests }));
    } else {
        print_flow_list(&plan, &project, false);
        print_test_list(&plan, &project);
    }
    Ok(())
}

fn run(path: &Path, options: &RunOptions) -> Result<(), CliError> {
    let project = load_project(path)?;
    let plan = compile_project(&project)?;
    let environment = execution_environment(&project, options.profile.as_deref())?;
    let flow_ids = if options.all {
        if !options.arguments.is_empty() {
            return Err(CliError::Usage(
                "`--arg` cannot be used with `--all`; parameterized flows are skipped".to_owned(),
            ));
        }
        plan.flows
            .iter()
            .enumerate()
            .filter_map(|(flow_id, flow)| {
                (flow.kind == DeclarationKind::Flow
                    && flow.span.source == project.entry_source
                    && flow.parameters.is_empty())
                .then_some(flow_id)
            })
            .collect()
    } else {
        vec![select_flow(&plan, &project, options.selector.as_ref())?]
    };

    let entry_flow_count = plan
        .flows
        .iter()
        .filter(|flow| flow.span.source == project.entry_source)
        .filter(|flow| flow.kind == DeclarationKind::Flow)
        .count();
    let skipped = entry_flow_count - flow_ids.len();
    if flow_ids.is_empty() {
        print_all_empty(options, skipped);
        return Ok(());
    }

    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            eprintln!("error: could not start the Mettle runtime: {error}");
            CliError::Failure
        })?;
    let batch_started = Instant::now();
    if options.all && options.output == OutputMode::Json {
        println!(
            "{}",
            serde_json::json!({
                "type": "start",
                "eligible": flow_ids.len(),
                "skipped": skipped,
            })
        );
    }
    let mut passed = 0;
    let mut failed = 0;
    let total = flow_ids.len();
    for (index, flow_id) in flow_ids.into_iter().enumerate() {
        if options.all {
            print_all_flow_separator(options, index + 1, total, &plan.flows[flow_id].display_name);
        }
        match run_flow(
            &project,
            &plan,
            flow_id,
            if options.all { &[] } else { &options.arguments },
            options,
            &async_runtime,
            &environment,
        ) {
            Ok(()) => passed += 1,
            Err(CliError::Failure) if options.all => failed += 1,
            Err(error) => return Err(error),
        }
    }
    if options.all {
        print_all_summary(options, passed, failed, skipped, batch_started.elapsed());
    }
    if failed > 0 {
        Err(CliError::Failure)
    } else {
        Ok(())
    }
}

fn run_tests(path: &Path, options: &RunOptions) -> Result<(), CliError> {
    if options.all
        || options.selector.is_some()
        || !options.arguments.is_empty()
        || options.output == OutputMode::Raw
    {
        return Err(CliError::Usage(
            "`mettle test` accepts output options only; tests are selected by file".to_owned(),
        ));
    }
    let project = load_project(path)?;
    let plan = compile_project(&project)?;
    let environment = execution_environment(&project, options.profile.as_deref())?;
    let test_ids = plan
        .flows
        .iter()
        .enumerate()
        .filter_map(|(id, test)| {
            (test.kind == DeclarationKind::Test && test.span.source == project.entry_source)
                .then_some(id)
        })
        .collect::<Vec<_>>();
    let total = test_ids.len();
    if total == 0 {
        eprintln!("error: no tests are declared in {}", path.display());
        return Err(CliError::Failure);
    }
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            eprintln!("error: could not start the Mettle runtime: {error}");
            CliError::Failure
        })?;
    let started = Instant::now();
    if options.output == OutputMode::Json {
        println!(
            "{}",
            serde_json::json!({ "type": "start", "kind": "test", "eligible": total })
        );
    }
    let mut passed = 0;
    let mut failed = 0;
    for (index, test_id) in test_ids.into_iter().enumerate() {
        if matches!(options.output, OutputMode::Human | OutputMode::Verbose) {
            println!(
                "\n------------------------------------------------------------------------\nTest {}/{total} · {}\n------------------------------------------------------------------------",
                index + 1,
                plan.flows[test_id].display_name
            );
        }
        match run_flow(
            &project,
            &plan,
            test_id,
            &[],
            options,
            &async_runtime,
            &environment,
        ) {
            Ok(()) => passed += 1,
            Err(CliError::Failure) => failed += 1,
            Err(error) => return Err(error),
        }
    }
    print_test_summary(options, passed, failed, started.elapsed());
    if failed > 0 {
        Err(CliError::Failure)
    } else {
        Ok(())
    }
}

fn print_test_summary(options: &RunOptions, passed: usize, failed: usize, duration: Duration) {
    if options.output == OutputMode::Json {
        println!(
            "{}",
            serde_json::json!({
                "type": "summary",
                "kind": "test",
                "eligible": passed + failed,
                "passed": passed,
                "failed": failed,
                "durationNanos": u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
            })
        );
    } else {
        println!(
            "\n========================================================================\nTest summary\n  Passed: {passed}   Failed: {failed}\n  Duration: {}\n========================================================================",
            display_duration(duration)
        );
    }
}

fn print_all_flow_separator(options: &RunOptions, index: usize, total: usize, display_name: &str) {
    if matches!(options.output, OutputMode::Human | OutputMode::Verbose) {
        println!(
            "\n------------------------------------------------------------------------\nFlow {index}/{total} · {display_name}\n------------------------------------------------------------------------"
        );
    }
}

fn run_flow(
    project: &LoadedProject,
    plan: &ExecutionPlan,
    flow_id: usize,
    supplied_arguments: &[(String, String)],
    options: &RunOptions,
    async_runtime: &tokio::runtime::Runtime,
    environment: &Arc<HashMap<String, String>>,
) -> Result<(), CliError> {
    let arguments = resolve_arguments(&plan.flows[flow_id], supplied_arguments)?;
    let observer = Arc::new(CliObserver::new(
        options.progress && matches!(options.output, OutputMode::Human | OutputMode::Verbose),
    ));
    let mettle_runtime = Runtime::new(vec![Arc::new(HttpCapability::new())])
        .with_observer(observer.clone())
        .with_environment(environment.clone());
    let started = Instant::now();
    let outcome = async_runtime.block_on(async {
        let mut execution = Box::pin(mettle_runtime.execute_selected(plan, flow_id, arguments));
        let mut interrupt = Box::pin(tokio::signal::ctrl_c());
        std::future::poll_fn(|task| {
            if let Poll::Ready(result) = execution.as_mut().poll(task) {
                return Poll::Ready(Some(result));
            }
            if interrupt.as_mut().poll(task).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
    });
    let Some(result) = outcome else {
        observer.clear_progress();
        eprintln!("execution cancelled");
        return Err(CliError::Interrupted);
    };
    observer.clear_progress();
    match result {
        Ok(value) => {
            print_success(plan, flow_id, options, started, &observer, &value);
            Ok(())
        }
        Err(error) => print_failure(project, plan, flow_id, options, started, &observer, &error),
    }
}

fn print_success(
    plan: &ExecutionPlan,
    flow_id: usize,
    options: &RunOptions,
    started: Instant,
    observer: &CliObserver,
    value: &Value,
) {
    let operations = observer.take_operations();
    let report = ExecutionReport {
        flow: &plan.flows[flow_id].display_name,
        duration: started.elapsed(),
        result: value,
        operations: &operations,
    };
    let color = options.color && io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    let is_test = plan.flows[flow_id].kind == DeclarationKind::Test;
    let output = match options.output {
        OutputMode::Human if is_test => report.test_human(false, color),
        OutputMode::Verbose if is_test => report.test_human(true, color),
        OutputMode::Quiet if is_test => report.test_quiet(color),
        OutputMode::Human => report.human(false, color),
        OutputMode::Verbose => report.human(true, color),
        OutputMode::Quiet => report.quiet(color),
        OutputMode::Raw => raw_value(value),
        OutputMode::Json if is_test => serde_json::json!({
            "type": "result",
            "kind": "test",
            "test": plan.flows[flow_id].display_name,
            "status": "passed",
            "durationNanos": u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        })
        .to_string(),
        OutputMode::Json if options.all => serde_json::json!({
            "type": "result",
            "flow": plan.flows[flow_id].display_name,
            "durationNanos": u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            "result": report::value_json(value),
        })
        .to_string(),
        OutputMode::Json => report.json(),
    };
    println!("{output}");
}

fn print_failure(
    project: &LoadedProject,
    plan: &ExecutionPlan,
    flow_id: usize,
    options: &RunOptions,
    started: Instant,
    observer: &CliObserver,
    error: &RuntimeError,
) -> Result<(), CliError> {
    if options.output == OutputMode::Json {
        let source = project
            .sources
            .get(error.span.source)
            .unwrap_or_else(|| &project.sources[project.entry_source]);
        let (line, column) = source_location(&source.text, error.span.start);
        let mut output = serde_json::json!({
            "flow": plan.flows[flow_id].display_name,
            "durationNanos": u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            "error": {
                "message": error.message,
                "path": source.path,
                "line": line,
                "column": column,
                "flowStack": error.flow_stack,
            }
        });
        if plan.flows[flow_id].kind == DeclarationKind::Test {
            let object = output
                .as_object_mut()
                .expect("execution report is an object");
            object.insert("type".to_owned(), serde_json::json!("failure"));
            object.insert("kind".to_owned(), serde_json::json!("test"));
            object.insert(
                "test".to_owned(),
                serde_json::json!(plan.flows[flow_id].display_name),
            );
            object.insert("status".to_owned(), serde_json::json!("failed"));
            object.remove("flow");
        } else if options.all {
            output
                .as_object_mut()
                .expect("execution report is an object")
                .insert("type".to_owned(), serde_json::json!("failure"));
        }
        println!("{output}");
        return Err(CliError::Failure);
    }
    let operations = observer.take_operations();
    let color = options.color && io::stderr().is_terminal() && env::var_os("NO_COLOR").is_none();
    eprintln!(
        "{}\n",
        failure_summary(
            &plan.flows[flow_id].display_name,
            &operations,
            color,
            plan.flows[flow_id].kind == DeclarationKind::Test
        )
    );
    eprintln!("{}", render_diagnostic(project, &error.message, error.span));
    if !error.flow_stack.is_empty() {
        eprintln!("flow stack: {}", error.flow_stack.join(" -> "));
    }
    Err(CliError::Failure)
}

fn print_all_empty(options: &RunOptions, skipped: usize) {
    if options.output == OutputMode::Json {
        println!(
            "{}",
            serde_json::json!({ "type": "start", "eligible": 0, "skipped": skipped })
        );
        println!(
            "{}",
            serde_json::json!({
                "type": "summary",
                "eligible": 0,
                "skipped": skipped,
                "passed": 0,
                "failed": 0,
                "durationNanos": 0,
            })
        );
    } else if options.output != OutputMode::Raw {
        println!(
            "\n========================================================================\nBatch summary\n  No zero-argument flows to run.\n  Passed: 0   Failed: 0   Skipped: {skipped}\n========================================================================"
        );
    }
}

fn print_all_summary(
    options: &RunOptions,
    passed: usize,
    failed: usize,
    skipped: usize,
    duration: Duration,
) {
    if options.output == OutputMode::Json {
        println!(
            "{}",
            serde_json::json!({
                "type": "summary",
                "eligible": passed + failed,
                "skipped": skipped,
                "passed": passed,
                "failed": failed,
                "durationNanos": u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
            })
        );
    } else if options.output != OutputMode::Raw {
        println!(
            "\n========================================================================\nBatch summary\n  Passed: {passed}   Failed: {failed}   Skipped: {skipped}\n  Duration: {}\n========================================================================",
            display_duration(duration)
        );
    }
}

fn select_flow(
    plan: &ExecutionPlan,
    project: &LoadedProject,
    selector: Option<&MettleSelector>,
) -> Result<usize, CliError> {
    let selected = match selector {
        Some(MettleSelector::Name(name)) => plan.flows.iter().position(|flow| {
            flow.kind == DeclarationKind::Flow && flow.name.as_deref() == Some(name)
        }),
        Some(MettleSelector::Line(line)) => {
            let matches = plan
                .flows
                .iter()
                .enumerate()
                .filter(|(_, flow)| {
                    flow.span.source == project.entry_source
                        && flow.kind == DeclarationKind::Flow
                        && source_location(&project.sources[flow.span.source].text, flow.span.start)
                            .0
                            == *line
                })
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                eprintln!("error: more than one flow starts on line {line}");
                return Err(CliError::Failure);
            }
            matches.first().copied()
        }
        Some(MettleSelector::Id(id)) => plan
            .flows
            .get(*id)
            .and_then(|flow| (flow.kind == DeclarationKind::Flow).then_some(*id)),
        None => plan.default_flow.or_else(|| {
            let mut flows = plan
                .flows
                .iter()
                .enumerate()
                .filter(|(_, flow)| flow.kind == DeclarationKind::Flow);
            let (id, _) = flows.next()?;
            flows.next().is_none().then_some(id)
        }),
    };
    if let Some(selected) = selected {
        return Ok(selected);
    }

    match selector {
        Some(MettleSelector::Name(name)) => eprintln!("error: flow `{name}` was not found"),
        Some(MettleSelector::Line(line)) => eprintln!("error: no flow starts on line {line}"),
        Some(MettleSelector::Id(id)) => eprintln!("error: no flow has compiler ID {id}"),
        None if !plan
            .flows
            .iter()
            .any(|flow| flow.kind == DeclarationKind::Flow) =>
        {
            eprintln!("error: this source contains no flows");
        }
        None => eprintln!("error: no default flow was found; select one by name or line"),
    }
    print_flow_list(plan, project, true);
    Err(CliError::Failure)
}

fn print_flow_list(plan: &ExecutionPlan, project: &LoadedProject, error_stream: bool) {
    let mut output = String::from("Available flows:\n");
    for flow in plan
        .flows
        .iter()
        .filter(|flow| flow.kind == DeclarationKind::Flow)
    {
        let source = &project.sources[flow.span.source];
        let (line, _) = source_location(&source.text, flow.span.start);
        let parameters = if flow.parameters.is_empty() {
            String::new()
        } else {
            format!(" ({})", flow.parameters.join(", "))
        };
        writeln!(
            output,
            "  {}:{line:<4}  {}{parameters}",
            source.path.display(),
            flow.display_name
        )
        .expect("writing to a string cannot fail");
    }
    if error_stream {
        eprint!("{output}");
    } else {
        print!("{output}");
    }
}

fn print_test_list(plan: &ExecutionPlan, project: &LoadedProject) {
    let tests = plan
        .flows
        .iter()
        .filter(|flow| flow.kind == DeclarationKind::Test)
        .collect::<Vec<_>>();
    if tests.is_empty() {
        return;
    }
    println!("Available tests:");
    for test in tests {
        let source = &project.sources[test.span.source];
        let (line, _) = source_location(&source.text, test.span.start);
        println!(
            "  {}:{line:<4}  {}",
            source.path.display(),
            test.display_name
        );
    }
}

fn resolve_arguments(
    flow: &MettlePlan,
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
        ExpressionKind::Call { .. }
        | ExpressionKind::Member { .. }
        | ExpressionKind::Binary { .. }
        | ExpressionKind::Within { .. }
        | ExpressionKind::Retry { .. }
        | ExpressionKind::Parallel { .. }
        | ExpressionKind::Rate { .. }
        | ExpressionKind::Concurrency { .. } => Err("flow arguments must be literal values"),
    }
}

struct SourceDocument {
    path: PathBuf,
    text: String,
}

struct LoadedProject {
    program: mettle_syntax::Program,
    sources: Vec<SourceDocument>,
    entry_source: usize,
}

fn load_project(path: &Path) -> Result<LoadedProject, CliError> {
    load_project_with_overlays(path, &HashMap::new())
}

fn execution_environment(
    project: &LoadedProject,
    profile: Option<&str>,
) -> Result<Arc<HashMap<String, String>>, CliError> {
    let entry = &project.sources[project.entry_source].path;
    let project_root = entry.parent().and_then(find_project_root);
    env_file::load_environment(entry, project_root.as_deref(), profile).map_err(|message| {
        eprintln!("error: {message}");
        CliError::Failure
    })
}

fn load_project_with_overlays(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
) -> Result<LoadedProject, CliError> {
    if path == Path::new("-") {
        let mut text = String::new();
        io::stdin().read_to_string(&mut text).map_err(|error| {
            eprintln!("error: could not read Mettle source from standard input: {error}");
            CliError::Failure
        })?;
        return load_sources(vec![(PathBuf::from("<stdin>"), text)], 0);
    }

    let entry = path
        .canonicalize()
        .or_else(|error| {
            if path.is_absolute() && overlays.contains_key(path) {
                Ok(path.to_path_buf())
            } else {
                Err(error)
            }
        })
        .map_err(|error| {
            eprintln!("error: could not open {}: {error}", path.display());
            CliError::Failure
        })?;
    let project_root = entry.parent().and_then(find_project_root);
    let mut paths = if let Some(root) = project_root {
        let mut paths = Vec::new();
        collect_flow_files(&root, &mut paths)?;
        paths.sort();
        paths
    } else {
        vec![entry.clone()]
    };
    if !paths.contains(&entry) {
        paths.push(entry.clone());
        paths.sort();
    }
    let entry_source = paths
        .iter()
        .position(|candidate| candidate == &entry)
        .expect("entry source was inserted");
    let sources = paths
        .into_iter()
        .map(|path| {
            overlays
                .get(&path)
                .cloned()
                .map_or_else(|| fs::read_to_string(&path), Ok)
                .map(|text| (path.clone(), text))
                .map_err(|error| {
                    eprintln!("error: could not read {}: {error}", path.display());
                    CliError::Failure
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    load_sources(sources, entry_source)
}

fn find_project_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|directory| directory.join("mettle.toml").is_file())
        .map(Path::to_path_buf)
}

fn collect_flow_files(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), CliError> {
    let entries = fs::read_dir(directory).map_err(|error| {
        eprintln!("error: could not inspect {}: {error}", directory.display());
        CliError::Failure
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            eprintln!("error: could not inspect {}: {error}", directory.display());
            CliError::Failure
        })?;
        let path = entry.path();
        if path.is_dir() {
            let hidden = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.') || name == "target");
            if !hidden {
                collect_flow_files(&path, paths)?;
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "mettle")
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn load_sources(
    sources: Vec<(PathBuf, String)>,
    entry_source: usize,
) -> Result<LoadedProject, CliError> {
    let documents = sources
        .into_iter()
        .map(|(path, text)| SourceDocument { path, text })
        .collect::<Vec<_>>();
    let mut parsed = Vec::with_capacity(documents.len());
    for (source_id, source) in documents.iter().enumerate() {
        let mut program = parse(&source.text).map_err(|error: SyntaxError| {
            eprintln!(
                "{}",
                render_source_diagnostic(&source.path, &source.text, &error.message, error.span)
            );
            CliError::Failure
        })?;
        program.set_source(source_id);
        if source_id != entry_source {
            program.flows.retain(|flow| flow.name.is_some());
        }
        parsed.push(program);
    }

    let entry = &parsed[entry_source];
    let mut program = mettle_syntax::Program {
        namespace: entry.namespace.clone(),
        namespace_uses: entry.namespace_uses.clone(),
        contexts: Vec::new(),
        file_contexts: Vec::new(),
        flows: Vec::new(),
    };
    for source in parsed {
        program.contexts.extend(source.contexts);
        program.file_contexts.extend(source.file_contexts);
        program.flows.extend(source.flows);
    }
    Ok(LoadedProject {
        program,
        sources: documents,
        entry_source,
    })
}

fn compile_project(project: &LoadedProject) -> Result<ExecutionPlan, CliError> {
    compile_with_capabilities(&project.program, CAPABILITIES).map_err(
        |errors: Vec<CompileError>| {
            for error in errors {
                eprintln!("{}", render_diagnostic(project, &error.message, error.span));
            }
            CliError::Failure
        },
    )
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

fn render_diagnostic(project: &LoadedProject, message: &str, span: Span) -> String {
    let source = project.sources.get(span.source).unwrap_or_else(|| {
        project
            .sources
            .get(project.entry_source)
            .expect("a loaded project always has an entry source")
    });
    render_source_diagnostic(&source.path, &source.text, message, span)
}

fn render_source_diagnostic(path: &Path, source: &str, message: &str, span: Span) -> String {
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
    Interrupted,
}
