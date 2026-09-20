use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn source_file(contents: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "mettle-cli-test-{}-{unique}.mettle",
        std::process::id()
    ));
    fs::write(&path, contents).expect("test source should be writable");
    path
}

fn project_directory() -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "mettle-cli-project-test-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("project directory should be creatable");
    path
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
        "flow main() { return { active: true user: { name: \"Ada\" roles: [\"tester\"] } } }",
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
fn default_output_summarizes_http_responses() {
    let path = source_file(
        "flow main() { return { body: \"ignored\" headers: { server: \"test\" } json: { active: true } method: \"GET\" status: 200 url: \"https://example.test/users\" } }",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_mettle"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("GET"));
    assert!(stdout.contains("https://example.test/users"));
    assert!(stdout.contains("200"));
    assert!(stdout.contains("\"active\": true"));
    assert!(!stdout.contains("headers"));
    assert!(!stdout.contains("ignored"));
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
        "flow describe(name, active, timeout) { return { name: name active: active timeout: timeout } }",
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
    let path = source_file("flow health() { assert(false) return true }");
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
    let entry_uri = format!("file://{}", entry.display());

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
    assert_eq!(receive_lsp(&mut stdout)["id"], 1);
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
                "textDocument": { "uri": format!("file://{}", entry.display()) },
                "position": { "line": 1, "character": 16 }
            }
        }),
    );
    let definition = receive_lsp(&mut stdout);
    assert_eq!(definition["id"], 2);
    assert_eq!(
        definition["result"]["uri"],
        format!(
            "file://{}",
            declaration
                .canonicalize()
                .expect("declaration path should resolve")
                .display()
        )
    );
    assert_eq!(
        definition["result"]["range"]["start"],
        serde_json::json!({ "line": 1, "character": 5 })
    );

    send_lsp(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }),
    );
    assert_eq!(receive_lsp(&mut stdout)["id"], 3);
    send_lsp(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );
    drop(stdin);
    assert!(child.wait().expect("language server should exit").success());
    fs::remove_dir_all(directory).expect("project directory should be removable");
}
