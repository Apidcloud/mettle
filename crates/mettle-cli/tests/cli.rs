use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMPORARY_PATH_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temporary_path_suffix() -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let counter = TEMPORARY_PATH_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{unique}-{counter}", std::process::id())
}

fn source_file(contents: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "mettle-cli-test-{}.mettle",
        temporary_path_suffix()
    ));
    fs::write(&path, contents).expect("test source should be writable");
    path
}

fn project_directory() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "mettle-cli-project-test-{}",
        temporary_path_suffix()
    ));
    fs::create_dir_all(&path).expect("project directory should be creatable");
    path
}

fn file_uri(path: &std::path::Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let mut uri = if path.as_bytes().get(1) == Some(&b':') {
        String::from("file:///")
    } else {
        String::from("file://")
    };
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            uri.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(uri, "%{byte:02X}").expect("writing to a string cannot fail");
        }
    }
    uri
}

fn send_lsp(stdin: &mut ChildStdin, message: &serde_json::Value) {
    let body = serde_json::to_vec(message).expect("LSP message should serialize");
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("LSP header should be writable");
    stdin.write_all(&body).expect("LSP body should be writable");
    stdin.flush().expect("LSP message should flush");
}

fn receive_lsp(stdout: &mut BufReader<ChildStdout>) -> serde_json::Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        stdout
            .read_line(&mut line)
            .expect("LSP header should be readable");
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.trim().strip_prefix("Content-Length:") {
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .expect("content length should be numeric"),
            );
        }
    }
    let mut body = vec![0; length.expect("response should include a content length")];
    stdout
        .read_exact(&mut body)
        .expect("LSP body should be readable");
    serde_json::from_slice(&body).expect("LSP body should be JSON")
}

fn receive_lsp_response(stdout: &mut BufReader<ChildStdout>, id: u64) -> serde_json::Value {
    loop {
        let message = receive_lsp(stdout);
        if message["id"] == id {
            return message;
        }
        assert_eq!(
            message["method"], "textDocument/publishDiagnostics",
            "unexpected LSP message: {message}"
        );
    }
}

#[test]
fn check_validates_a_source_file() {
    let path = source_file("flow main() { return \"valid\" }");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("check")
        .arg(&path)
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Checked"));
}

#[test]
fn discovers_and_executes_a_multi_file_project() {
    let directory = project_directory();
    fs::write(directory.join("mettle.toml"), "name = \"test\"\n")
        .expect("manifest should be writable");
    fs::write(
        directory.join("first.mettle"),
        "namespace tools\nflow first() = \"project\"\n",
    )
    .expect("first source should be writable");
    fs::write(
        directory.join("second.mettle"),
        "namespace tools\nflow second() = first()\n",
    )
    .expect("second source should be writable");
    let entry = directory.join("main.mettle");
    fs::write(&entry, "use namespace tools\nflow main() = second()\n")
        .expect("entry source should be writable");

    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&entry)
        .output()
        .expect("flow should start");
    fs::remove_dir_all(directory).expect("project directory should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("main"));
    assert!(stdout.contains("project"));
    assert!(stdout.contains("Completed"));
}

#[test]
fn run_prints_the_main_flow_result() {
    let path =
        source_file("flow identity(value) { return value } flow main() { return identity(42) }");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("main"));
    assert!(stdout.contains("42"));
    assert!(stdout.contains("Completed"));
}

#[test]
fn verbose_prints_nested_results_for_humans() {
    let path = source_file(
        "flow main() { return { active: true, user: { name: \"Ada\", roles: [\"tester\"] } } }",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--verbose")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("main"));
    assert!(stdout.contains("\"active\": true"));
    assert!(stdout.contains("\"name\": \"Ada\""));
    assert!(stdout.contains("\"roles\""));
    assert!(stdout.contains("Completed"));
}

#[test]
fn ordinary_objects_are_not_mistaken_for_capability_results() {
    let path = source_file(
        "flow main() { return { body: \"ignored\", headers: { server: \"test\" }, json: { active: true }, method: \"GET\", status: 200, url: \"https://example.test/users\" } }",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"method\": \"GET\""));
    assert!(stdout.contains("\"url\": \"https://example.test/users\""));
    assert!(stdout.contains("\"status\": 200"));
    assert!(stdout.contains("\"active\": true"));
    assert!(stdout.contains("headers"));
    assert!(stdout.contains("ignored"));
}

#[test]
fn file_contexts_apply_to_every_flow_in_their_own_source_file() {
    let directory = project_directory();
    fs::write(directory.join("mettle.toml"), "name = \"contexts\"\n")
        .expect("manifest should be writable");
    let entry = directory.join("main.mettle");
    fs::write(
        &entry,
        "context entry { marker: \"entry\" }\nflow main() = marker\nuse context entry\n",
    )
    .expect("entry source should be writable");
    fs::write(
        directory.join("other.mettle"),
        "context otherContext { marker: \"other\" }\nflow other() = marker\nuse context otherContext\n",
    )
    .expect("other source should be writable");

    let main = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .args(["run"])
        .arg(&entry)
        .output()
        .expect("main flow should run");
    let other = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .args(["run"])
        .arg(&entry)
        .arg("other")
        .output()
        .expect("other flow should run");
    fs::remove_dir_all(directory).expect("project directory should be removable");

    assert!(main.status.success(), "{main:?}");
    assert!(other.status.success(), "{other:?}");
    assert!(String::from_utf8_lossy(&main.stdout).contains("entry"));
    assert!(!String::from_utf8_lossy(&main.stdout).contains("other"));
    assert!(String::from_utf8_lossy(&other.stdout).contains("other"));
}

#[test]
fn combined_context_declaration_applies_to_flows_and_tests() {
    let path = source_file(
        "use context credentials { token: senv(\"METTLE_TEST_SECRET\") }\nflow main() = token\ntest(\"has token\") { assert(token == senv(\"METTLE_TEST_SECRET\")) }\n",
    );
    let flow_output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--output")
        .arg("json")
        .env("METTLE_TEST_SECRET", "never-print-this")
        .output()
        .expect("flow should start");
    let test_output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .arg("--output")
        .arg("json")
        .env("METTLE_TEST_SECRET", "never-print-this")
        .output()
        .expect("test should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(flow_output.status.success(), "{flow_output:?}");
    let flow_stdout = String::from_utf8_lossy(&flow_output.stdout);
    assert!(flow_stdout.contains("[REDACTED]"), "{flow_stdout}");
    assert!(!flow_stdout.contains("never-print-this"), "{flow_stdout}");
    assert!(test_output.status.success(), "{test_output:?}");
    let test_stdout = String::from_utf8_lossy(&test_output.stdout);
    assert!(test_stdout.contains("\"passed\":1"), "{test_stdout}");
}

#[test]
fn anonymous_file_contexts_compose_and_do_not_leak_between_files() {
    let directory = project_directory();
    fs::write(
        directory.join("mettle.toml"),
        "name = \"anonymous-contexts\"\n",
    )
    .expect("manifest should be writable");
    let entry = directory.join("main.mettle");
    fs::write(
        &entry,
        "context base { marker: \"entry\" }\nflow main() = marker\ntest(\"sees entry\") { assert(marker == \"entry\") }\nuse context { use context base }\n",
    )
    .expect("entry source should be writable");
    fs::write(
        directory.join("other.mettle"),
        "use context { marker: \"other\" }\nflow other() = marker\n",
    )
    .expect("other source should be writable");
    let main = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .args(["run"])
        .arg(&entry)
        .arg("--output")
        .arg("json")
        .output()
        .expect("main flow should run");
    let other = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .args(["run"])
        .arg(&entry)
        .arg("other")
        .arg("--output")
        .arg("json")
        .output()
        .expect("other flow should run");
    let tests = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&entry)
        .arg("--output")
        .arg("json")
        .output()
        .expect("test should run");
    fs::remove_dir_all(directory).expect("project should be removable");

    assert!(main.status.success(), "{main:?}");
    assert!(other.status.success(), "{other:?}");
    assert!(tests.status.success(), "{tests:?}");
    assert!(String::from_utf8_lossy(&main.stdout).contains("\"result\":\"entry\""));
    assert!(String::from_utf8_lossy(&other.stdout).contains("\"result\":\"other\""));
    assert!(String::from_utf8_lossy(&tests.stdout).contains("\"passed\":1"));
}

#[test]
fn secret_values_are_redacted_after_interpolation_and_in_json() {
    let path = source_file(
        "flow main() { credentials = secret({ token: env(\"METTLE_TEST_SECRET\") })\n return \"Bearer ${credentials.token}\" }",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--output")
        .arg("json")
        .env("METTLE_TEST_SECRET", "never-print-this")
        .output()
        .expect("flow should run");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[REDACTED]"));
    assert!(!stdout.contains("never-print-this"));
}

#[test]
fn echo_respects_human_quiet_raw_and_json_output() {
    let path = source_file("flow main() { echo(\"starting\")\n return \"done\" }");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mettle"))
            .arg("run")
            .arg(&path)
            .args(args)
            .output()
            .expect("flow should start")
    };
    let human = run(&[]);
    let verbose = run(&["--verbose"]);
    let quiet = run(&["--quiet"]);
    let raw = run(&["--raw"]);
    let json = run(&["--output", "json"]);
    fs::remove_file(path).expect("test source should be removable");

    for output in [&human, &verbose, &quiet, &raw, &json] {
        assert!(output.status.success(), "{output:?}");
    }
    assert!(String::from_utf8_lossy(&human.stdout).contains("echo starting"));
    assert!(String::from_utf8_lossy(&verbose.stdout).contains("echo starting"));
    assert!(!String::from_utf8_lossy(&quiet.stdout).contains("starting"));
    assert_eq!(String::from_utf8_lossy(&raw.stdout).trim(), "done");
    let record: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON report");
    assert_eq!(record["events"][0]["type"], "echo");
    assert_eq!(record["events"][0]["value"], "starting");
}

#[test]
fn echo_in_parallel_has_branch_paths_and_keeps_batch_records_atomic() {
    let path = source_file(
        "flow message(name) { echo(name)\n return name }\nflow main() = parallel(limit: 2) { message(\"Ada\"), message(\"Lin\") }",
    );
    let human = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("main")
        .output()
        .expect("flow should start");
    let json = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("main")
        .args(["--output", "json"])
        .output()
        .expect("flow should start");
    let batch = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .args(["--all", "--output", "json"])
        .output()
        .expect("batch should start");
    fs::remove_file(path).expect("test source should be removable");
    assert!(human.status.success(), "{human:?}");
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("[p1:b1/2] echo Ada"), "{stdout}");
    assert!(stdout.contains("[p1:b2/2] echo Lin"), "{stdout}");
    let record: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON report");
    assert_eq!(record["events"][0]["scope"][0]["kind"], "parallel");
    assert_eq!(record["events"][0]["scope"][0]["branch"], 1);
    assert_eq!(record["events"][1]["scope"][0]["branch"], 2);
    assert!(batch.status.success(), "{batch:?}");
    let lines = String::from_utf8_lossy(&batch.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON line"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1]["type"], "result");
    assert_eq!(lines[1]["events"].as_array().expect("events").len(), 2);
}

#[test]
fn echo_is_retained_on_failure_and_redacts_secrets() {
    let path = source_file(
        "flow main() { echo(senv(\"METTLE_ECHO_SECRET\"))\n assert(false)\n return true }",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .env("METTLE_ECHO_SECRET", "do-not-print-me")
        .output()
        .expect("flow should start");
    let json = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .args(["--output", "json"])
        .env("METTLE_ECHO_SECRET", "do-not-print-me")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("echo [REDACTED]"), "{stderr}");
    assert!(!stderr.contains("do-not-print-me"));
    let record: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON failure");
    assert_eq!(record["events"][0]["value"], "[REDACTED]");
    assert!(!String::from_utf8_lossy(&json.stdout).contains("do-not-print-me"));
}

#[test]
fn senv_reads_environment_and_redacts_derived_values() {
    let path = source_file(
        "context credentials { token: senv(\"METTLE_TEST_SECRET\") }\nuse context credentials\nflow main() = \"Bearer ${token}\"\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--output")
        .arg("json")
        .env("METTLE_TEST_SECRET", "never-print-this")
        .output()
        .expect("flow should run");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[REDACTED]"), "{stdout}");
    assert!(!stdout.contains("never-print-this"), "{stdout}");
}

#[test]
fn test_declarations_require_unique_names_and_cannot_return() {
    let duplicate = source_file("test(\"same\") {}\ntest(\"same\") {}\n");
    let duplicate_output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("check")
        .arg(&duplicate)
        .output()
        .expect("check should start");
    fs::remove_file(duplicate).expect("test source should be removable");
    assert!(!duplicate_output.status.success());
    assert!(String::from_utf8_lossy(&duplicate_output.stderr).contains("declared more than once"));

    let returning = source_file("test(\"returns\") { return true }\n");
    let returning_output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("check")
        .arg(&returning)
        .output()
        .expect("check should start");
    fs::remove_file(returning).expect("test source should be removable");
    assert!(!returning_output.status.success());
    assert!(String::from_utf8_lossy(&returning_output.stderr).contains("not allowed in a test"));
}

#[test]
fn invalid_source_has_a_location_and_nonzero_exit() {
    let path = source_file("flow main() { return missing }");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("check")
        .arg(&path)
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("name `missing` is not defined"));
    assert!(error.contains(":1:22"));
    assert!(error.contains('^'));
}

#[test]
fn runs_a_selected_parameterized_flow_with_typed_arguments() {
    let path = source_file(
        "flow describe(name, active, timeout) { return { name: name, active: active, timeout: timeout } }",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("describe")
        .arg("--arg")
        .arg("name=Ada Lovelace")
        .arg("--arg")
        .arg("active=true")
        .arg("--arg")
        .arg("timeout=2s")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("describe"));
    assert!(stdout.contains("\"active\": true"));
    assert!(stdout.contains("\"name\": \"Ada Lovelace\""));
    assert!(stdout.contains("\"timeout\": \"2000000000ns\""));
}

#[test]
fn runs_anonymous_top_level_calls_by_compiler_id() {
    let path =
        source_file("flow identity(value) = value\nidentity(\"first\")\nidentity(\"second\")\n");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--flow-id")
        .arg("2")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("identity"));
    assert!(stdout.contains("second"));
}

#[test]
fn requires_selection_when_multiple_flows_have_no_main() {
    let path = source_file("flow first() = 1\nflow second() = 2\n");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("no default flow was found"));
    assert!(error.contains("first"));
    assert!(error.contains("second"));
}

#[test]
fn all_runs_zero_argument_flows_and_skips_parameterized_flows() {
    let path = source_file(
        "flow first() = \"one\"\nflow needsArgument(value) = value\nflow second() = \"two\"\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--all")
        .output()
        .expect("flows should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("first"));
    assert!(stdout.contains("one"));
    assert!(stdout.contains("second"));
    assert!(stdout.contains("two"));
    assert!(!stdout.contains("needsArgument"));
    assert!(stdout.contains("Flow 1/2 · first"));
    assert!(stdout.contains("Flow 2/2 · second"));
    assert!(stdout.contains("Batch summary"));
    assert!(stdout.contains("Passed: 2   Failed: 0   Skipped: 1"));
}

#[test]
fn tests_are_discovered_separately_and_continue_after_failure() {
    let path = source_file(
        "flow helper() = 42\ntest(\"first passes\") { assert(helper() == 42) }\ntest(\"fails\") { assert(false) }\ntest(\"last passes\") { assert(true) }\n",
    );
    let list = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("list")
        .arg(&path)
        .arg("--json")
        .output()
        .expect("list should start");
    let run_all = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--all")
        .output()
        .expect("run should start");
    let tests = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .arg("--output")
        .arg("json")
        .output()
        .expect("test should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(list.status.success(), "{list:?}");
    let discovered: serde_json::Value =
        serde_json::from_slice(&list.stdout).expect("discovery should be JSON");
    assert_eq!(discovered["flows"].as_array().expect("flows").len(), 1);
    assert_eq!(discovered["tests"].as_array().expect("tests").len(), 3);
    assert!(run_all.status.success(), "{run_all:?}");
    let flow_output = String::from_utf8_lossy(&run_all.stdout);
    assert!(flow_output.contains("Passed: 1   Failed: 0   Skipped: 0"));
    assert!(!flow_output.contains("first passes"));
    assert!(!tests.status.success(), "{tests:?}");
    let records = String::from_utf8_lossy(&tests.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON record"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 5);
    assert_eq!(records[0]["kind"], "test");
    assert_eq!(records[1]["test"], "first passes");
    assert_eq!(records[1]["status"], "passed");
    assert_eq!(records[2]["test"], "fails");
    assert_eq!(records[2]["status"], "failed");
    assert_eq!(records[2]["error"]["message"], "assertion failed");
    assert_eq!(records[3]["test"], "last passes");
    assert_eq!(records[4]["passed"], 2);
    assert_eq!(records[4]["failed"], 1);
}

#[test]
fn test_reports_every_assertion_message_in_human_and_json_output() {
    let path = source_file(
        "test(\"checks\") {\n    assert(false, \"status should be 200\")\n    assert(true)\n    assert(false, \"body should contain an ID\")\n}\n",
    );
    let human = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .output()
        .expect("test should start");
    let json = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .args(["--output", "json"])
        .output()
        .expect("test should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(!human.status.success(), "{human:?}");
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert!(stderr.contains("2 assertions failed:"), "{stderr}");
    assert!(stderr.contains("status should be 200"), "{stderr}");
    assert!(stderr.contains("body should contain an ID"), "{stderr}");
    assert!(stderr.contains(":2:"), "{stderr}");
    assert!(stderr.contains(":4:"), "{stderr}");

    assert!(!json.status.success(), "{json:?}");
    let records = String::from_utf8_lossy(&json.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON record"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[1]["status"], "failed");
    assert_eq!(records[1]["error"]["message"], "2 assertions failed");
    assert_eq!(
        records[1]["error"]["assertions"][0]["message"],
        "status should be 200"
    );
    assert_eq!(records[1]["error"]["assertions"][0]["line"], 2);
    assert_eq!(
        records[1]["error"]["assertions"][1]["message"],
        "body should contain an ID"
    );
    assert_eq!(records[1]["error"]["assertions"][1]["line"], 4);
    assert_eq!(records[2]["failed"], 1);
}

#[test]
fn assertion_messages_redact_secret_interpolation() {
    let path = source_file(
        "use context { token: senv(\"METTLE_ASSERTION_MESSAGE_SECRET\") }\ntest(\"secret message\") { assert(false, \"token ${token}\") }\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .args(["--output", "json"])
        .env("METTLE_ASSERTION_MESSAGE_SECRET", "never-print-this")
        .output()
        .expect("test should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(!output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("never-print-this"), "{stdout}");
    let records = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON record"))
        .collect::<Vec<_>>();
    assert_eq!(
        records[1]["error"]["assertions"][0]["message"],
        "[REDACTED]"
    );
}

#[test]
fn test_command_only_executes_tests_in_selected_file() {
    let directory = project_directory();
    fs::write(directory.join("mettle.toml"), "name = \"tests\"\n")
        .expect("manifest should be writable");
    let entry = directory.join("main.mettle");
    fs::write(&entry, "test(\"entry\") { assert(true) }\n").expect("entry should be writable");
    fs::write(
        directory.join("other.mettle"),
        "test(\"other\") { assert(false) }\n",
    )
    .expect("other should be writable");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&entry)
        .output()
        .expect("test should start");
    fs::remove_dir_all(directory).expect("project should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("entry"));
    assert!(!stdout.contains("other"));
    assert!(stdout.contains("Passed: 1   Failed: 0"));
}

#[test]
fn test_command_can_select_one_test_by_name_or_line() {
    let path = source_file(
        "test(\"first passes\") { assert(true) }\ntest(\"second fails\") { assert(false) }\n",
    );
    let named = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .arg("first passes")
        .args(["--output", "json"])
        .output()
        .expect("selected test should start");
    let lined = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .args(["--line", "1"])
        .output()
        .expect("selected test should start");
    let missing = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .arg("missing")
        .output()
        .expect("missing test should report an error");
    fs::remove_file(path).expect("test source should be removable");

    assert!(named.status.success(), "{named:?}");
    let records = String::from_utf8_lossy(&named.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON record"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["eligible"], 1);
    assert_eq!(records[1]["test"], "first passes");
    assert_eq!(records[2]["passed"], 1);
    assert!(lined.status.success(), "{lined:?}");
    let stdout = String::from_utf8_lossy(&lined.stdout);
    assert!(stdout.contains("first passes"), "{stdout}");
    assert!(!stdout.contains("second fails"), "{stdout}");
    assert!(!missing.status.success(), "{missing:?}");
    assert!(String::from_utf8_lossy(&missing.stderr).contains("was not found"));
}

#[test]
fn test_line_selector_rejects_ambiguous_declarations() {
    let path = source_file("test(\"first\") {} test(\"second\") {}\n");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .args(["--line", "1"])
        .output()
        .expect("test command should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("more than one test starts on line 1")
    );
}

#[test]
fn test_command_fails_when_selected_file_has_no_tests() {
    let path = source_file("flow main() = 1\n");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&path)
        .args(["--output", "json"])
        .output()
        .expect("test command should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("no tests are declared"));
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn duplicate_file_context_is_reported_once_per_file() {
    let path = source_file(
        "use context { marker: 1 }\nuse context { marker: 2 }\nflow first() = marker\nflow second() = marker\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("check")
        .arg(&path)
        .output()
        .expect("check should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr
            .matches("a file may apply only one default context")
            .count(),
        1
    );
}

#[test]
fn secret_http_urls_are_redacted_in_verbose_and_json_reports() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("local server should bind");
    let address = listener
        .local_addr()
        .expect("local address should be available");
    let server = std::thread::spawn(move || {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().expect("request should arrive");
            let mut request = [0_u8; 1024];
            let bytes_read = stream
                .read(&mut request)
                .expect("request should be readable");
            assert!(bytes_read > 0, "request should not be empty");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}")
                .expect("response should be writable");
        }
    });
    let path = source_file("flow main() = http.get(secret(env(\"METTLE_SECRET_URL\")))");
    let url = format!("http://{address}/health?token=never-print-this");
    for output_options in [vec!["--verbose"], vec!["--output", "json"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
            .arg("run")
            .arg(&path)
            .args(output_options)
            .env("METTLE_SECRET_URL", &url)
            .output()
            .expect("flow should start");
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("[REDACTED]"), "{stdout}");
        assert!(!stdout.contains("never-print-this"), "{stdout}");
    }
    let context_path = source_file(
        "context api { defaults http { baseUrl: senv(\"METTLE_SECRET_URL\") } }\nuse context api\nflow main() = http.get(\"/health\")\n",
    );
    let context_output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&context_path)
        .args(["--output", "json"])
        .env(
            "METTLE_SECRET_URL",
            format!("http://{address}/never-print-this"),
        )
        .output()
        .expect("flow should start");
    assert!(context_output.status.success(), "{context_output:?}");
    let context_stdout = String::from_utf8_lossy(&context_output.stdout);
    assert!(context_stdout.contains("[REDACTED]"), "{context_stdout}");
    assert!(
        !context_stdout.contains("never-print-this"),
        "{context_stdout}"
    );
    fs::remove_file(path).expect("test source should be removable");
    fs::remove_file(context_path).expect("test source should be removable");
    server.join().expect("local server should complete");
}

#[test]
fn all_json_output_is_atomic_json_lines_with_batch_records() {
    let path = source_file(
        "flow first() = \"one\"\nflow needsArgument(value) = value\nflow second() = \"two\"\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--all")
        .arg("--output")
        .arg("json")
        .output()
        .expect("flows should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let records = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("record is JSON"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 4);
    assert_eq!(records[0]["type"], "start");
    assert_eq!(records[0]["eligible"], 2);
    assert_eq!(records[0]["skipped"], 1);
    assert_eq!(records[1]["type"], "result");
    assert_eq!(records[1]["flow"], "first");
    assert_eq!(records[1]["result"], "one");
    assert_eq!(records[2]["type"], "result");
    assert_eq!(records[2]["flow"], "second");
    assert_eq!(records[3]["type"], "summary");
    assert_eq!(records[3]["passed"], 2);
    assert_eq!(records[3]["failed"], 0);
    assert_eq!(records[3]["skipped"], 1);
}

#[test]
fn all_only_runs_zero_argument_flows_in_the_requested_file() {
    let directory = project_directory();
    fs::write(directory.join("mettle.toml"), "name = \"all\"\n")
        .expect("manifest should be writable");
    fs::write(
        directory.join("shared.mettle"),
        "flow helper() = \"outside\"\n",
    )
    .expect("shared source should be writable");
    let entry = directory.join("main.mettle");
    fs::write(&entry, "flow main() = \"inside\"\n").expect("entry source should be writable");

    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&entry)
        .arg("--all")
        .output()
        .expect("flows should start");
    fs::remove_dir_all(directory).expect("test project should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("main"));
    assert!(stdout.contains("inside"));
    assert!(!stdout.contains("helper"));
    assert!(!stdout.contains("outside"));
}

#[test]
fn all_continues_after_a_failed_flow_and_returns_failure() {
    let path =
        source_file("flow broken() { assert(false)\n return true }\nflow healthy() = \"ok\"\n");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--all")
        .output()
        .expect("flows should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("healthy"));
    assert!(stdout.contains("ok"));
    assert!(stdout.contains("Batch summary"));
    assert!(stdout.contains("Passed: 1   Failed: 1   Skipped: 0"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("broken"));
    assert!(stderr.contains("assertion failed"));
}

#[test]
fn interpolation_falls_back_to_the_environment() {
    let path = source_file("flow endpoint() = \"${METTLE_TEST_URL}/health\"");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .env("METTLE_TEST_URL", "http://localhost:4020")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("endpoint"));
    assert!(stdout.contains("http://localhost:4020/health"));
}

#[test]
fn standalone_files_load_default_and_selected_profile_beside_the_entry() {
    let directory = project_directory();
    let entry = directory.join("main.mettle");
    fs::write(
        &entry,
        "flow main() = \"${METTLE_PROFILE_TEST_VALUE}\"\nflow secretValue() = senv(\"METTLE_PROFILE_SECRET\")\ntest(\"qa profile\") { assert(env(\"METTLE_PROFILE_TEST_VALUE\") == \"qa\") }\n",
    )
        .expect("source should be writable");
    fs::write(
        directory.join(".env"),
        "METTLE_PROFILE_TEST_VALUE=default\n",
    )
    .expect("base env should be writable");
    fs::write(
        directory.join(".env.qa"),
        "METTLE_PROFILE_TEST_VALUE=qa\nMETTLE_PROFILE_SECRET=never-print-this\n",
    )
    .expect("profile env should be writable");

    let default = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&entry)
        .arg("--output")
        .arg("json")
        .env_remove("METTLE_PROFILE_TEST_VALUE")
        .output()
        .expect("default run should start");
    let qa = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&entry)
        .args(["--profile", "qa", "--output", "json"])
        .env_remove("METTLE_PROFILE_TEST_VALUE")
        .output()
        .expect("profile run should start");
    let qa_tests = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("test")
        .arg(&entry)
        .args(["--profile", "qa", "--output", "json"])
        .env_remove("METTLE_PROFILE_TEST_VALUE")
        .output()
        .expect("profile test should start");
    let secret = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&entry)
        .arg("secretValue")
        .args(["--profile", "qa", "--output", "json"])
        .env_remove("METTLE_PROFILE_SECRET")
        .output()
        .expect("secret flow should start");
    let missing = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&entry)
        .args(["--profile", "missing"])
        .output()
        .expect("missing profile run should start");
    fs::remove_dir_all(directory).expect("test directory should be removable");

    assert!(default.status.success(), "{default:?}");
    assert!(qa.status.success(), "{qa:?}");
    assert!(qa_tests.status.success(), "{qa_tests:?}");
    assert!(secret.status.success(), "{secret:?}");
    assert!(String::from_utf8_lossy(&default.stdout).contains("\"result\":\"default\""));
    assert!(String::from_utf8_lossy(&qa.stdout).contains("\"result\":\"qa\""));
    assert!(String::from_utf8_lossy(&qa_tests.stdout).contains("\"passed\":1"));
    let secret_stdout = String::from_utf8_lossy(&secret.stdout);
    assert!(secret_stdout.contains("[REDACTED]"));
    assert!(!secret_stdout.contains("never-print-this"));
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no `.env.missing`"));
}

#[test]
fn project_profiles_overlay_root_and_entry_files_but_not_process_values() {
    let directory = project_directory();
    let entry_directory = directory.join("checks");
    fs::create_dir_all(&entry_directory).expect("entry directory should be creatable");
    fs::write(directory.join("mettle.toml"), "name = \"profiles\"\n")
        .expect("manifest should be writable");
    fs::write(
        directory.join(".env"),
        "METTLE_PROFILE_TEST_VALUE=root-default\n",
    )
    .expect("project base env should be writable");
    fs::write(
        entry_directory.join(".env"),
        "METTLE_PROFILE_TEST_VALUE=entry-default\n",
    )
    .expect("entry base env should be writable");
    fs::write(
        directory.join(".env.qa"),
        "METTLE_PROFILE_TEST_VALUE=root-qa\n",
    )
    .expect("project profile should be writable");
    fs::write(
        entry_directory.join(".env.qa"),
        "METTLE_PROFILE_TEST_VALUE=entry-qa\n",
    )
    .expect("entry profile should be writable");
    let entry = entry_directory.join("main.mettle");
    fs::write(&entry, "flow main() = env(\"METTLE_PROFILE_TEST_VALUE\")\n")
        .expect("entry source should be writable");
    let qa = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&entry)
        .args(["--profile", "qa", "--output", "json"])
        .env_remove("METTLE_PROFILE_TEST_VALUE")
        .output()
        .expect("profile run should start");
    let overridden = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&entry)
        .args(["--profile", "qa", "--output", "json"])
        .env("METTLE_PROFILE_TEST_VALUE", "process")
        .output()
        .expect("process override run should start");
    fs::remove_dir_all(directory).expect("project should be removable");

    assert!(qa.status.success(), "{qa:?}");
    assert!(overridden.status.success(), "{overridden:?}");
    assert!(String::from_utf8_lossy(&qa.stdout).contains("\"result\":\"entry-qa\""));
    assert!(String::from_utf8_lossy(&overridden.stdout).contains("\"result\":\"process\""));
}

#[test]
fn json_output_is_a_stable_execution_envelope() {
    let path = source_file("flow health() = { status: \"ok\" }");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--output")
        .arg("json")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("run output should be JSON");
    assert_eq!(result["flow"], "health");
    assert_eq!(result["result"]["status"], "ok");
    assert!(result["durationNanos"].is_number());
}

#[test]
fn json_output_reports_failures_without_human_text() {
    let path = source_file("flow health() { assert(false)\n return true }");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .arg("--output")
        .arg("json")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("failure output should be JSON");
    assert_eq!(result["flow"], "health");
    assert_eq!(result["error"]["message"], "assertion failed");
    assert_eq!(result["error"]["line"], 1);
}

#[test]
fn lists_compiler_discovered_flows_as_json() {
    let path = source_file("http.get(\"${API_URL}/health\")\nflow getUser(id) = id\n");
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("list")
        .arg(&path)
        .arg("--json")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("list output should be JSON");
    assert_eq!(result["flows"][0]["displayName"], "GET ${API_URL}/health");
    assert_eq!(result["flows"][0]["line"], 1);
    assert_eq!(result["flows"][1]["name"], "getUser");
    assert_eq!(result["flows"][1]["parameters"][0], "id");
}

#[test]
fn lsp_navigates_from_an_unsaved_document_to_another_file() {
    let directory = project_directory();
    fs::write(directory.join("mettle.toml"), "name = \"lsp\"\n")
        .expect("manifest should be writable");
    let declaration = directory.join("shared.mettle");
    fs::write(&declaration, "namespace shared\nflow helper() = true\n")
        .expect("declaration should be writable");
    let entry = directory.join("main.mettle");
    fs::write(&entry, "use namespace shared\nflow main() = false\n")
        .expect("entry should be writable");
    let entry_uri = file_uri(&entry);

    let mut child = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("language server should start");
    let mut stdin = child
        .stdin
        .take()
        .expect("language server should have stdin");
    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .expect("language server should have stdout"),
    );

    send_lsp(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
    );
    assert_eq!(receive_lsp_response(&mut stdout, 1)["id"], 1);
    send_lsp(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": entry_uri,
                    "languageId": "mettle",
                    "version": 2,
                    "text": "use namespace shared\nflow main() = helper()\n"
                }
            }
        }),
    );
    send_lsp(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": file_uri(&entry) },
                "position": { "line": 1, "character": 16 }
            }
        }),
    );
    let definition = receive_lsp_response(&mut stdout, 2);
    assert_eq!(definition["id"], 2);
    let definition_uri = definition["result"]["uri"]
        .as_str()
        .expect("definition URI should be a string");
    assert!(definition_uri.starts_with("file://"));
    assert!(definition_uri.ends_with("/shared.mettle"));
    assert_eq!(
        definition["result"]["range"]["start"],
        serde_json::json!({ "line": 1, "character": 5 })
    );

    send_lsp(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }),
    );
    assert_eq!(receive_lsp_response(&mut stdout, 3)["id"], 3);
    send_lsp(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );
    drop(stdin);
    assert!(child.wait().expect("language server should exit").success());
    fs::remove_dir_all(directory).expect("project directory should be removable");
}
