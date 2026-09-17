use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn source_file(contents: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "flow-cli-test-{}-{unique}.flow",
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
        "flow-cli-project-test-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("project directory should be creatable");
    path
}

#[test]
fn check_validates_a_source_file() {
    let path = source_file("flow main() { return \"valid\" }");
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
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
    fs::write(directory.join("flow.toml"), "name = \"test\"\n")
        .expect("manifest should be writable");
    fs::write(
        directory.join("first.flow"),
        "namespace tools\nflow first() = \"project\"\n",
    )
    .expect("first source should be writable");
    fs::write(
        directory.join("second.flow"),
        "namespace tools\nflow second() = first()\n",
    )
    .expect("second source should be writable");
    let entry = directory.join("main.flow");
    fs::write(&entry, "use namespace tools\nflow main() = second()\n")
        .expect("entry source should be writable");

    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
        .arg("run")
        .arg(&entry)
        .output()
        .expect("flow should start");
    fs::remove_dir_all(directory).expect("project directory should be removable");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "project\n");
}

#[test]
fn run_prints_the_main_flow_result() {
    let path =
        source_file("flow identity(value) { return value } flow main() { return identity(42) }");
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
}

#[test]
fn verbose_prints_nested_results_for_humans() {
    let path = source_file(
        "flow main() { return { active: true user: { name: \"Ada\" roles: [\"tester\"] } } }",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
        .arg("run")
        .arg(&path)
        .arg("--verbose")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!(
            "{\n",
            "  \"active\": true,\n",
            "  \"user\": {\n",
            "    \"name\": \"Ada\",\n",
            "    \"roles\": [\n",
            "      \"tester\"\n",
            "    ]\n",
            "  }\n",
            "}\n"
        )
    );
}

#[test]
fn default_output_summarizes_http_responses() {
    let path = source_file(
        "flow main() { return { body: \"ignored\" headers: { server: \"test\" } json: { active: true } method: \"GET\" status: 200 url: \"https://example.test/users\" } }",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "GET https://example.test/users → 200\n");
    assert!(!stdout.contains("headers"));
    assert!(!stdout.contains("ignored"));
}

#[test]
fn invalid_source_has_a_location_and_nonzero_exit() {
    let path = source_file("flow main() { return missing }");
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
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
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
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
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!(
            "{\n",
            "  \"active\": true,\n",
            "  \"name\": \"Ada Lovelace\",\n",
            "  \"timeout\": \"2000000000ns\"\n",
            "}\n"
        )
    );
}

#[test]
fn runs_anonymous_top_level_calls_by_compiler_id() {
    let path =
        source_file("flow identity(value) = value\nidentity(\"first\")\nidentity(\"second\")\n");
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
        .arg("run")
        .arg(&path)
        .arg("--flow-id")
        .arg("2")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "second\n");
}

#[test]
fn requires_selection_when_multiple_flows_have_no_main() {
    let path = source_file("flow first() = 1\nflow second() = 2\n");
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
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
    let path = source_file("flow endpoint() = \"${FLOW_TEST_URL}/health\"");
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
        .arg("run")
        .arg(&path)
        .env("FLOW_TEST_URL", "http://localhost:4020")
        .output()
        .expect("flow should start");
    fs::remove_file(path).expect("test source should be removable");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "http://localhost:4020/health\n"
    );
}

#[test]
fn lists_compiler_discovered_flows_as_json() {
    let path = source_file("http.get(\"${API_URL}/health\")\nflow getUser(id) = id\n");
    let output = Command::new(env!("CARGO_BIN_EXE_flow"))
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
