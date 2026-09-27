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

use mettle_capability::IoContext;
use mettle_capability::codecs::{
    BYTES_DESCRIPTOR, CodecCapability, JSON_DESCRIPTOR, TEXT_DESCRIPTOR,
};
use mettle_capability::content::BuiltinCodec;
use mettle_capability::{CapabilityDescriptor, Object, Value};
use mettle_compiler::{
    CompileError, DeclarationKind, ExecutionPlan, MettlePlan, compile_with_capabilities,
};
use mettle_fs::{DESCRIPTOR as FS_DESCRIPTOR, FsCapability};
use mettle_http::{DESCRIPTOR as HTTP_DESCRIPTOR, HttpCapability};
use mettle_runtime::{Runtime, RuntimeError};
use mettle_syntax::{Expression, ExpressionKind, Span, SyntaxError, parse, parse_value};

const HELP: &str = "\
Mettle language tools

Usage:
  mettle check <file>
  mettle list <file> [--json]
  mettle run <file> [flow-name] [--all | --line <line>] [--jobs <count>] [--arg <name=value>]... [--profile <name>] [output options]
  mettle test <file> [test-name | --line <line>] [--jobs <count>] [--profile <name>] [--verbose | --quiet | --output json] [--no-progress] [--no-color]
  mettle lsp
  mettle docs <symbol>
  mettle --help
  mettle --version

Commands:
  check   Parse and validate a Mettle source file
  list    List compiler-discovered runnable flows
  run     Validate the source and execute a selected flow, or every zero-argument flow
  test    Execute tests in the selected file, or one selected test
  lsp     Start the Mettle language server over standard input/output
  docs    Print the installed version's language or capability reference as Markdown

Run output options:
  --profile NAME  Overlay .env.NAME from the entry folder and project root
  --jobs COUNT    Override the project's job limit for a file-level batch (default: 1)
  --verbose       Show the complete result and operation details
  --quiet         Print only the final flow status
  --raw           Print only the returned Mettle value
  --output json   Print a structured execution report
  --no-progress   Disable the interactive workload display
  --no-color      Disable ANSI colors
";

mod env_file;
mod lsp;
mod project_config;
mod report;

use report::{
    CapturedEvents, CliObserver, ExecutionReport, display_duration, failure_summary, raw_value,
};

const CAPABILITIES: &[CapabilityDescriptor] = &[
    HTTP_DESCRIPTOR,
    FS_DESCRIPTOR,
    JSON_DESCRIPTOR,
    TEXT_DESCRIPTOR,
    BYTES_DESCRIPTOR,
];

#[derive(Debug)]
struct RunOptions {
    selector: Option<MettleSelector>,
    all: bool,
    arguments: Vec<(String, String)>,
    jobs: Option<usize>,
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
            jobs: None,
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

#[derive(Clone, Copy, Debug)]
enum BatchKind {
    Flows,
    Tests,
}

#[derive(Debug)]
struct EntrySpec {
    flow_id: usize,
    source_index: usize,
    arguments: Vec<Value>,
}

struct EntryOutcome {
    flow_id: usize,
    source_index: usize,
    duration: Duration,
    captured: CapturedEvents,
    result: Result<Value, RuntimeError>,
}

struct ActiveEntry {
    flow_id: usize,
    source_index: usize,
    started: Instant,
    observer: Arc<CliObserver>,
    io: IoContext,
}

enum BatchEvent {
    Interrupted,
    Joined(Box<Option<Result<(tokio::task::Id, EntryOutcome), tokio::task::JoinError>>>),
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
        "docs" if arguments.len() == 2 => {
            let name = arguments[1]
                .to_str()
                .ok_or_else(|| CliError::Usage("documentation name must be UTF-8".into()))?;
            let content = mettle_capability::documentation::reference(CAPABILITIES, name)
                .or_else(|| mettle_compiler::documentation::reference(name))
                .ok_or_else(|| CliError::Usage(format!("unknown documentation symbol `{name}`")))?;
            print!("{content}");
            Ok(())
        }
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
            "--jobs" => {
                parse_jobs_option(arguments.get(cursor + 1), &mut options)?;
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

fn parse_jobs_option(value: Option<&OsString>, options: &mut RunOptions) -> Result<(), CliError> {
    let value = value
        .and_then(|value| value.to_str())
        .ok_or_else(|| CliError::Usage("`--jobs` requires a count".to_owned()))?;
    let jobs = value
        .parse::<usize>()
        .map_err(|_| CliError::Usage("`--jobs` requires a positive integer".to_owned()))?;
    if jobs == 0 {
        return Err(CliError::Usage(
            "`--jobs` requires a positive integer".to_owned(),
        ));
    }
    if options.jobs.replace(jobs).is_some() {
        return Err(CliError::Usage(
            "`--jobs` was supplied more than once".to_owned(),
        ));
    }
    Ok(())
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
    if options.jobs.is_some() && !options.all {
        return Err(CliError::Usage(
            "`--jobs` is only valid with `mettle run <file> --all`".to_owned(),
        ));
    }
    let project = load_project(path)?;
    let jobs = if options.all {
        options.jobs.unwrap_or_else(|| project.config.run.jobs())
    } else {
        1
    };
    if jobs > 1 && options.output == OutputMode::Raw {
        return Err(CliError::Usage(
            "`--raw` requires one job; use `--jobs 1` or choose human or JSON output".to_owned(),
        ));
    }
    let plan = Arc::new(compile_project(&project)?);
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
                "jobs": jobs.min(flow_ids.len()),
            })
        );
    }
    let total = flow_ids.len();
    let entries = flow_ids
        .into_iter()
        .enumerate()
        .map(|(index, flow_id)| {
            Ok(EntrySpec {
                flow_id,
                source_index: index + 1,
                arguments: resolve_arguments(
                    &plan.flows[flow_id],
                    if options.all { &[] } else { &options.arguments },
                )?,
            })
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    let (passed, failed) = async_runtime.block_on(execute_entries(
        &project,
        plan,
        entries,
        options,
        environment,
        options.all.then_some(BatchKind::Flows),
        total,
        jobs,
    ))?;
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
    validate_test_options(options)?;
    let project = load_project(path)?;
    let jobs = if options.selector.is_none() {
        options.jobs.unwrap_or_else(|| project.config.test.jobs())
    } else {
        1
    };
    let plan = Arc::new(compile_project(&project)?);
    let environment = execution_environment(&project, options.profile.as_deref())?;
    let available_test_ids = plan
        .flows
        .iter()
        .enumerate()
        .filter_map(|(id, test)| {
            (test.kind == DeclarationKind::Test && test.span.source == project.entry_source)
                .then_some(id)
        })
        .collect::<Vec<_>>();
    let test_ids = available_test_ids
        .iter()
        .copied()
        .filter(|&id| match options.selector.as_ref() {
            Some(MettleSelector::Name(name)) => plan.flows[id].display_name == *name,
            Some(MettleSelector::Line(line)) => {
                source_location(
                    &project.sources[project.entry_source].text,
                    plan.flows[id].span.start,
                )
                .0 == *line
            }
            _ => true,
        })
        .collect::<Vec<_>>();
    if let Some(MettleSelector::Line(line)) = options.selector.as_ref()
        && test_ids.len() > 1
    {
        eprintln!("error: more than one test starts on line {line}; select by name");
        return Err(CliError::Failure);
    }
    let total = test_ids.len();
    if total == 0 {
        match options.selector.as_ref() {
            Some(MettleSelector::Name(name)) => {
                eprintln!("error: test `{name}` was not found in {}", path.display());
            }
            Some(MettleSelector::Line(line)) => {
                eprintln!("error: no test starts on line {line} in {}", path.display());
            }
            _ => eprintln!("error: no tests are declared in {}", path.display()),
        }
        return Err(CliError::Failure);
    }
    run_selected_tests(&project, plan, environment, test_ids, options, jobs)
}

fn validate_test_options(options: &RunOptions) -> Result<(), CliError> {
    if options.all
        || matches!(options.selector, Some(MettleSelector::Id(_)))
        || !options.arguments.is_empty()
        || options.output == OutputMode::Raw
    {
        return Err(CliError::Usage(
            "`mettle test` accepts a test name or `--line` selector, plus output options"
                .to_owned(),
        ));
    }
    if options.jobs.is_some() && options.selector.is_some() {
        return Err(CliError::Usage(
            "`--jobs` is only valid when running every test in a file".to_owned(),
        ));
    }
    Ok(())
}

fn run_selected_tests(
    project: &LoadedProject,
    plan: Arc<ExecutionPlan>,
    environment: Arc<HashMap<String, String>>,
    test_ids: Vec<usize>,
    options: &RunOptions,
    jobs: usize,
) -> Result<(), CliError> {
    let total = test_ids.len();
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
            serde_json::json!({
                "type": "start",
                "kind": "test",
                "eligible": total,
                "jobs": jobs.min(total),
            })
        );
    }
    let entries = test_ids
        .into_iter()
        .enumerate()
        .map(|(index, flow_id)| EntrySpec {
            flow_id,
            source_index: index + 1,
            arguments: Vec::new(),
        })
        .collect();
    let batch_kind = options.selector.is_none().then_some(BatchKind::Tests);
    let (passed, failed) = async_runtime.block_on(execute_entries(
        project,
        plan,
        entries,
        options,
        environment,
        batch_kind,
        total,
        jobs,
    ))?;
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

fn print_entry_separator(
    options: &RunOptions,
    kind: BatchKind,
    index: usize,
    total: usize,
    display_name: &str,
    error_stream: bool,
) {
    if matches!(options.output, OutputMode::Human | OutputMode::Verbose) {
        let label = match kind {
            BatchKind::Flows => "Flow",
            BatchKind::Tests => "Test",
        };
        let header = format!(
            "\n------------------------------------------------------------------------\n{label} {index}/{total} · {display_name}\n------------------------------------------------------------------------"
        );
        if error_stream {
            eprintln!("{header}");
        } else {
            println!("{header}");
        }
    }
}

fn spawn_entry(
    join_set: &mut tokio::task::JoinSet<EntryOutcome>,
    plan: Arc<ExecutionPlan>,
    spec: EntrySpec,
    environment: Arc<HashMap<String, String>>,
    sources: Arc<[String]>,
    progress: bool,
    directory: PathBuf,
) -> (tokio::task::Id, ActiveEntry) {
    let observer = Arc::new(CliObserver::new(progress).with_sources(sources));
    let started = Instant::now();
    let io = IoContext::new(directory);
    let active = ActiveEntry {
        flow_id: spec.flow_id,
        source_index: spec.source_index,
        started,
        observer: observer.clone(),
        io: io.clone(),
    };
    let handle = join_set.spawn(async move {
        let mettle_runtime = Runtime::new(vec![
            Arc::new(HttpCapability::new()),
            Arc::new(FsCapability),
            Arc::new(CodecCapability(BuiltinCodec::Json)),
            Arc::new(CodecCapability(BuiltinCodec::Text)),
            Arc::new(CodecCapability(BuiltinCodec::Bytes)),
        ])
        .with_observer(observer.clone())
        .with_environment(environment)
        .with_io_context(io);
        let result = mettle_runtime
            .execute_selected(&plan, spec.flow_id, spec.arguments)
            .await;
        observer.clear_progress();
        EntryOutcome {
            flow_id: spec.flow_id,
            source_index: spec.source_index,
            duration: started.elapsed(),
            captured: observer.take_events(),
            result,
        }
    });
    (handle.id(), active)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn execute_entries(
    project: &LoadedProject,
    plan: Arc<ExecutionPlan>,
    entries: Vec<EntrySpec>,
    options: &RunOptions,
    environment: Arc<HashMap<String, String>>,
    batch_kind: Option<BatchKind>,
    total: usize,
    jobs: usize,
) -> Result<(usize, usize), CliError> {
    let mut pending = std::collections::VecDeque::from(entries);
    let directory = execution_directory(project)?;
    let sources: Arc<[String]> = Arc::from(
        project
            .sources
            .iter()
            .map(|source| source.text.clone())
            .collect::<Vec<_>>(),
    );
    let progress = jobs == 1
        && options.progress
        && matches!(options.output, OutputMode::Human | OutputMode::Verbose);
    let batch_started = Instant::now();
    if jobs > 1
        && batch_kind.is_some()
        && matches!(options.output, OutputMode::Human | OutputMode::Verbose)
    {
        println!(
            "Running {total} entries with up to {} concurrent jobs…",
            jobs.min(total)
        );
    }
    let mut join_set = tokio::task::JoinSet::new();
    let mut active = HashMap::new();
    while active.len() < jobs {
        let Some(spec) = pending.pop_front() else {
            break;
        };
        let (task_id, entry) = spawn_entry(
            &mut join_set,
            plan.clone(),
            spec,
            environment.clone(),
            sources.clone(),
            progress,
            directory.clone(),
        );
        active.insert(task_id, entry);
    }

    let mut interrupt = Box::pin(tokio::signal::ctrl_c());
    let mut passed = 0;
    let mut failed = 0;
    while !active.is_empty() {
        let event = std::future::poll_fn(|task| {
            if interrupt.as_mut().poll(task).is_ready() {
                return Poll::Ready(BatchEvent::Interrupted);
            }
            match join_set.poll_join_next_with_id(task) {
                Poll::Ready(result) => Poll::Ready(BatchEvent::Joined(Box::new(result))),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        match event {
            BatchEvent::Interrupted => {
                for entry in active.values() {
                    entry.io.cancel();
                }
                join_set.abort_all();
                while let Some(result) = join_set.join_next_with_id().await {
                    if let Ok((task_id, outcome)) = result {
                        active.remove(&task_id);
                        if outcome.result.is_ok() {
                            passed += 1;
                        } else {
                            failed += 1;
                        }
                        print_entry_outcome(project, &plan, options, batch_kind, total, outcome);
                    }
                }
                let mut cancelled = active.into_values().collect::<Vec<_>>();
                cancelled.sort_by_key(|entry| entry.source_index);
                for entry in &cancelled {
                    if let Err(error) = entry.io.cleanup().await {
                        eprintln!("error: {error}");
                    }
                    print_cancellation(&plan, options, batch_kind, total, entry);
                }
                if let Some(kind) = batch_kind {
                    let skipped = if matches!(kind, BatchKind::Flows) {
                        plan.flows
                            .iter()
                            .filter(|flow| {
                                flow.kind == DeclarationKind::Flow
                                    && flow.span.source == project.entry_source
                                    && !flow.parameters.is_empty()
                            })
                            .count()
                    } else {
                        0
                    };
                    print_interrupted_summary(
                        options,
                        kind,
                        [passed, failed, cancelled.len(), pending.len(), skipped],
                        batch_started.elapsed(),
                    );
                }
                return Err(CliError::Interrupted);
            }
            BatchEvent::Joined(result) => match *result {
                Some(Ok((task_id, outcome))) => {
                    active.remove(&task_id);
                    let succeeded = outcome.result.is_ok();
                    print_entry_outcome(project, &plan, options, batch_kind, total, outcome);
                    if succeeded {
                        passed += 1;
                    } else {
                        failed += 1;
                    }
                    if let Some(spec) = pending.pop_front() {
                        let (task_id, entry) = spawn_entry(
                            &mut join_set,
                            plan.clone(),
                            spec,
                            environment.clone(),
                            sources.clone(),
                            progress,
                            directory.clone(),
                        );
                        active.insert(task_id, entry);
                    }
                }
                Some(Err(error)) => {
                    for entry in active.values() {
                        entry.io.cancel();
                    }
                    join_set.abort_all();
                    while join_set.join_next().await.is_some() {}
                    for entry in active.values() {
                        let _ = entry.io.cleanup().await;
                    }
                    eprintln!("error: an execution job failed internally: {error}");
                    return Err(CliError::Failure);
                }
                None => break,
            },
        }
    }
    Ok((passed, failed))
}

fn print_interrupted_summary(
    options: &RunOptions,
    kind: BatchKind,
    counts: [usize; 5],
    duration: Duration,
) {
    let [passed, failed, cancelled, not_started, skipped] = counts;
    if options.output == OutputMode::Json {
        let mut summary = serde_json::json!({
            "type": "summary",
            "status": "interrupted",
            "eligible": passed + failed + cancelled + not_started,
            "passed": passed,
            "failed": failed,
            "cancelled": cancelled,
            "notStarted": not_started,
            "durationNanos": u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
        });
        if matches!(kind, BatchKind::Tests) {
            summary["kind"] = serde_json::json!("test");
        } else {
            summary["skipped"] = serde_json::json!(skipped);
        }
        println!("{summary}");
    } else if options.output != OutputMode::Raw {
        let label = match kind {
            BatchKind::Flows => "Batch",
            BatchKind::Tests => "Test",
        };
        let skipped_line = if matches!(kind, BatchKind::Flows) {
            format!("\n  Skipped: {skipped}")
        } else {
            String::new()
        };
        println!(
            "\n========================================================================\n{label} summary · Interrupted\n  Passed: {passed}   Failed: {failed}   Cancelled: {cancelled}   Not started: {not_started}{skipped_line}\n  Duration: {}\n========================================================================",
            display_duration(duration)
        );
    }
}

fn print_cancellation(
    plan: &ExecutionPlan,
    options: &RunOptions,
    batch_kind: Option<BatchKind>,
    total: usize,
    entry: &ActiveEntry,
) {
    entry.observer.clear_progress();
    let captured = entry.observer.take_events();
    let flow = &plan.flows[entry.flow_id];
    if let Some(kind) = batch_kind {
        print_entry_separator(
            options,
            kind,
            entry.source_index,
            total,
            &flow.display_name,
            true,
        );
    } else if flow.kind == DeclarationKind::Test {
        print_entry_separator(
            options,
            BatchKind::Tests,
            1,
            total,
            &flow.display_name,
            true,
        );
    }
    if options.output == OutputMode::Json {
        let mut output = if flow.kind == DeclarationKind::Test {
            serde_json::json!({
                "type": "cancelled",
                "kind": "test",
                "test": flow.display_name,
                "durationNanos": u64::try_from(entry.started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                "events": report::events_json(&captured.events, captured.omitted_workload_echoes),
                "workloads": report::workloads_json(&captured.workloads),
                "omittedWorkloads": captured.omitted_workloads,
            })
        } else {
            serde_json::json!({
                "type": "cancelled",
                "flow": flow.display_name,
                "durationNanos": u64::try_from(entry.started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                "events": report::events_json(&captured.events, captured.omitted_workload_echoes),
                "workloads": report::workloads_json(&captured.workloads),
                "omittedWorkloads": captured.omitted_workloads,
            })
        };
        if batch_kind.is_some() {
            output
                .as_object_mut()
                .expect("cancellation report is an object")
                .insert(
                    "sourceIndex".to_owned(),
                    serde_json::json!(entry.source_index),
                );
        }
        println!("{output}");
    } else if matches!(options.output, OutputMode::Human | OutputMode::Verbose) {
        let color =
            options.color && io::stderr().is_terminal() && env::var_os("NO_COLOR").is_none();
        eprintln!(
            "{}",
            report::cancellation_summary(
                &flow.display_name,
                &captured.events,
                captured.omitted_workload_echoes,
                color
            )
        );
        if !captured.workloads.is_empty() {
            eprintln!(
                "{}",
                report::workloads_result(&captured.workloads, captured.omitted_workloads, color)
            );
        }
    } else {
        eprintln!("execution cancelled");
    }
}

fn print_entry_outcome(
    project: &LoadedProject,
    plan: &ExecutionPlan,
    options: &RunOptions,
    batch_kind: Option<BatchKind>,
    total: usize,
    outcome: EntryOutcome,
) {
    if let Some(kind) = batch_kind {
        print_entry_separator(
            options,
            kind,
            outcome.source_index,
            total,
            &plan.flows[outcome.flow_id].display_name,
            outcome.result.is_err(),
        );
    } else if plan.flows[outcome.flow_id].kind == DeclarationKind::Test {
        print_entry_separator(
            options,
            BatchKind::Tests,
            1,
            total,
            &plan.flows[outcome.flow_id].display_name,
            outcome.result.is_err(),
        );
    }
    let source_index = batch_kind.map(|_| outcome.source_index);
    match outcome.result {
        Ok(value) => print_success(
            plan,
            outcome.flow_id,
            options,
            outcome.duration,
            &outcome.captured,
            &value,
            source_index,
        ),
        Err(error) => print_failure(
            project,
            plan,
            outcome.flow_id,
            options,
            outcome.duration,
            &outcome.captured,
            &error,
            source_index,
        ),
    }
}

fn print_success(
    plan: &ExecutionPlan,
    flow_id: usize,
    options: &RunOptions,
    duration: Duration,
    captured: &CapturedEvents,
    value: &Value,
    source_index: Option<usize>,
) {
    let report = ExecutionReport {
        flow: &plan.flows[flow_id].display_name,
        duration,
        result: value,
        events: &captured.events,
        omitted_workload_echoes: captured.omitted_workload_echoes,
        workloads: &captured.workloads,
        omitted_workloads: captured.omitted_workloads,
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
        OutputMode::Json if is_test => json_with_source_index(
            serde_json::json!({
                "type": "result",
                "kind": "test",
                "test": plan.flows[flow_id].display_name,
                "status": "passed",
                "durationNanos": u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
                "events": report::events_json(&captured.events, captured.omitted_workload_echoes),
                "workloads": report::workloads_json(&captured.workloads),
                "omittedWorkloads": captured.omitted_workloads,
            }),
            source_index,
        ),
        OutputMode::Json if options.all => json_with_source_index(
            serde_json::json!({
                "type": "result",
                "flow": plan.flows[flow_id].display_name,
                "durationNanos": u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
                "result": report::value_json(value),
                "events": report::events_json(&captured.events, captured.omitted_workload_echoes),
                "workloads": report::workloads_json(&captured.workloads),
                "omittedWorkloads": captured.omitted_workloads,
            }),
            source_index,
        ),
        OutputMode::Json => report.json(),
    };
    println!("{output}");
}

fn json_with_source_index(mut output: serde_json::Value, source_index: Option<usize>) -> String {
    if let Some(source_index) = source_index {
        output
            .as_object_mut()
            .expect("execution report is an object")
            .insert("sourceIndex".to_owned(), serde_json::json!(source_index));
    }
    output.to_string()
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn print_failure(
    project: &LoadedProject,
    plan: &ExecutionPlan,
    flow_id: usize,
    options: &RunOptions,
    duration: Duration,
    captured: &CapturedEvents,
    error: &RuntimeError,
    source_index: Option<usize>,
) {
    if options.output == OutputMode::Json {
        let source = project
            .sources
            .get(error.span.source)
            .unwrap_or_else(|| &project.sources[project.entry_source]);
        let (line, column) = source_location(&source.text, error.span.start);
        let mut output = serde_json::json!({
            "flow": plan.flows[flow_id].display_name,
            "durationNanos": u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
            "events": report::events_json(&captured.events, captured.omitted_workload_echoes),
            "workloads": report::workloads_json(&captured.workloads),
            "omittedWorkloads": captured.omitted_workloads,
            "error": {
                "message": error.message,
                "terminal": error.terminal,
                "path": source.path,
                "line": line,
                "column": column,
                "flowStack": error.flow_stack,
                "scope": report::scope_json(&error.scope_path),
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
            if !error.assertions.is_empty() {
                let assertions = error
                    .assertions
                    .iter()
                    .map(|assertion| {
                        let source = project
                            .sources
                            .get(assertion.span.source)
                            .unwrap_or_else(|| &project.sources[project.entry_source]);
                        let (line, column) = source_location(&source.text, assertion.span.start);
                        serde_json::json!({
                            "message": assertion.message,
                            "path": source.path,
                            "line": line,
                            "column": column,
                        })
                    })
                    .collect::<Vec<_>>();
                output["error"]["assertions"] = serde_json::json!(assertions);
            }
        } else if options.all {
            output
                .as_object_mut()
                .expect("execution report is an object")
                .insert("type".to_owned(), serde_json::json!("failure"));
        }
        if let Some(source_index) = source_index {
            output
                .as_object_mut()
                .expect("execution report is an object")
                .insert("sourceIndex".to_owned(), serde_json::json!(source_index));
        }
        println!("{output}");
        return;
    }
    let color = options.color && io::stderr().is_terminal() && env::var_os("NO_COLOR").is_none();
    eprintln!(
        "{}\n",
        failure_summary(
            &plan.flows[flow_id].display_name,
            if matches!(options.output, OutputMode::Quiet | OutputMode::Raw) {
                &[]
            } else {
                &captured.events
            },
            if matches!(options.output, OutputMode::Quiet | OutputMode::Raw) {
                0
            } else {
                captured.omitted_workload_echoes
            },
            color,
            plan.flows[flow_id].kind == DeclarationKind::Test
        )
    );
    if !matches!(options.output, OutputMode::Quiet | OutputMode::Raw)
        && !captured.workloads.is_empty()
    {
        eprintln!(
            "{}\n",
            report::workloads_result(&captured.workloads, captured.omitted_workloads, color)
        );
    }
    if !error.assertion_only {
        eprintln!("{}", render_diagnostic(project, &error.message, error.span));
    }
    if !error.assertions.is_empty() {
        eprintln!(
            "{} assertion{} failed:",
            error.assertions.len(),
            if error.assertions.len() == 1 { "" } else { "s" }
        );
        for assertion in &error.assertions {
            eprintln!(
                "{}",
                render_diagnostic(project, &assertion.message, assertion.span)
            );
        }
    }
    if !error.flow_stack.is_empty() && !error.assertion_only {
        eprintln!("flow stack: {}", error.flow_stack.join(" -> "));
    }
    if !error.scope_path.is_empty() && !error.assertion_only {
        eprintln!(
            "execution path: {}",
            report::scope_label(&error.scope_path).trim_end()
        );
    }
}

fn print_all_empty(options: &RunOptions, skipped: usize) {
    if options.output == OutputMode::Json {
        println!(
            "{}",
            serde_json::json!({ "type": "start", "eligible": 0, "skipped": skipped, "jobs": 0 })
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
        || (source.starts_with('-') && source.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
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
        ExpressionKind::Negate(value) => match value_from_expression(value)? {
            Value::Integer(number) => number
                .checked_neg()
                .map(Value::Integer)
                .ok_or("integer negation overflowed"),
            Value::Float(number) => Ok(Value::Float(-number)),
            _ => Err("unary `-` requires a number"),
        },
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
        | ExpressionKind::Fail(_)
        | ExpressionKind::Block(_)
        | ExpressionKind::Source(_)
        | ExpressionKind::For { .. }
        | ExpressionKind::Member { .. }
        | ExpressionKind::Index { .. }
        | ExpressionKind::Not(_)
        | ExpressionKind::TypeOperation { .. }
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
    config: project_config::ProjectConfig,
}

fn execution_directory(project: &LoadedProject) -> Result<PathBuf, CliError> {
    let entry = &project.sources[project.entry_source].path;
    let root = if entry == Path::new("<stdin>") {
        None
    } else {
        entry.parent().and_then(find_project_root)
    };
    let directory = if let Some(root) = root {
        project
            .config
            .working_dir
            .as_ref()
            .map_or(root.clone(), |path| root.join(path))
    } else {
        std::env::current_dir().map_err(|error| {
            eprintln!("error: could not determine invocation directory: {error}");
            CliError::Failure
        })?
    };
    if !directory.is_dir() {
        eprintln!(
            "error: workingDir is not an existing directory: {}",
            directory.display()
        );
        return Err(CliError::Failure);
    }
    Ok(directory)
}

fn load_project(path: &Path) -> Result<LoadedProject, CliError> {
    let mut project = load_project_with_overlays(path, &HashMap::new())?;
    let entry = &project.sources[project.entry_source].path;
    let root = if entry == Path::new("<stdin>") {
        None
    } else {
        entry.parent().and_then(find_project_root)
    };
    project.config = project_config::load(root.as_deref()).map_err(|message| {
        eprintln!("error: {message}");
        CliError::Failure
    })?;
    Ok(project)
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
    let (sources, entry_source) = load_project_documents_with_overlays(path, overlays)?;
    load_sources(sources, entry_source)
}

fn load_project_documents_with_overlays(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
) -> Result<(Vec<SourceDocument>, usize), CliError> {
    if path == Path::new("-") {
        let mut text = String::new();
        io::stdin().read_to_string(&mut text).map_err(|error| {
            eprintln!("error: could not read Mettle source from standard input: {error}");
            CliError::Failure
        })?;
        return Ok((
            vec![SourceDocument {
                path: PathBuf::from("<stdin>"),
                text,
            }],
            0,
        ));
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
    let mut paths = if let Some(root) = &project_root {
        let mut paths = Vec::new();
        collect_flow_files(root, &mut paths)?;
        paths
    } else {
        vec![entry.clone()]
    };
    if let Some(root) = &project_root {
        paths.extend(
            overlays
                .keys()
                .filter(|overlay| {
                    overlay.starts_with(root)
                        && overlay
                            .extension()
                            .is_some_and(|extension| extension == "mettle")
                })
                .cloned(),
        );
    }
    paths.push(entry.clone());
    paths.sort();
    paths.dedup();
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
                .map(|text| SourceDocument {
                    path: path.clone(),
                    text,
                })
                .map_err(|error| {
                    eprintln!("error: could not read {}: {error}", path.display());
                    CliError::Failure
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((sources, entry_source))
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
    documents: Vec<SourceDocument>,
    entry_source: usize,
) -> Result<LoadedProject, CliError> {
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

    Ok(combine_parsed_sources(documents, parsed, entry_source))
}

fn combine_parsed_sources(
    documents: Vec<SourceDocument>,
    parsed: Vec<mettle_syntax::Program>,
    entry_source: usize,
) -> LoadedProject {
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
    LoadedProject {
        program,
        sources: documents,
        entry_source,
        config: project_config::ProjectConfig::default(),
    }
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
