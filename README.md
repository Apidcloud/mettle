# Flow

Flow is an experimental language and native runtime for I/O-oriented workflows. It currently supports concise request collections, reusable parameterized flows, immutable data, contexts, environment configuration, HTTP/1.1 and HTTPS operations, JSON payloads and responses, and compiler-validated HTTP options.

The implementation compiles source into a resolved execution plan and interprets that plan on an asynchronous Rust runtime. HTTP clients and their connection pools are reused across operations.

The evolving language direction is in [`docs/language-proposal.md`](docs/language-proposal.md). The implementation and delivery strategy is in [`docs/flow-technical.md`](docs/flow-technical.md).

## Requirements

- Linux
- Rust 1.98.1 through [rustup](https://rustup.rs/)
- Python 3 for the local HTTP acceptance fixture

The repository pins its Rust toolchain in `rust-toolchain.toml`. Cargo selects it automatically when rustup is installed.

On Omarchy:

```bash
omarchy install dev-env rust
```

## Build and verify

```bash
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
./scripts/check-licenses.py
./scripts/acceptance-http.sh
```

The HTTP acceptance command starts isolated HTTP and HTTPS fixtures on random local ports. It verifies named and anonymous entry flows, CLI arguments, request chaining, JSON, environment configuration, connection reuse, whole-exchange timeouts (including response streaming), compile-time schema errors, secure certificate rejection, and the explicit certificate-verification override.

Build the optimized binary with:

```bash
cargo build --release
./target/release/flow --version
```

## Run a small flow

```flow
flow identity(value) {
    return value
}

flow main() {
    result = identity("Hello from Flow")
    return result
}
```

Validate and execute the included example:

```bash
cargo run -- check examples/hello.flow
cargo run -- run examples/hello.flow
```

`main` is the default when no flow is selected. It is optional: a source with exactly one flow runs that flow automatically, while a source with multiple flows requires a name or source line.

## Use Flow as a request collection

A top-level call is an independently runnable anonymous flow:

```flow
http.get("https://postman-echo.com/get?demo=bare")

flow inspectRequest(baseUrl, requestId) =
    http.get("${baseUrl}/get?requestId=${requestId}")
```

The included request collection uses the public Postman Echo HTTPS service. It needs an internet connection, but no account, credentials, environment variables, or manually started server.

Validate and inspect the included collection:

```bash
cargo run -- check examples/request-collection.flow
cargo run -- list examples/request-collection.flow
```

Run a named flow with typed arguments:

```bash
cargo run -- run examples/request-collection.flow inspectRequest \
  --arg baseUrl=https://postman-echo.com \
  --arg requestId=42
```

Run an anonymous flow using the line reported by `flow list`:

```bash
cargo run -- run examples/request-collection.flow --line 3
```

The default output for a direct HTTP operation is concise: method, URL, and status. Use `--verbose` to inspect headers, response data, body metadata, and the complete response envelope:

```bash
cargo run -- run examples/request-collection.flow --line 10 --verbose
```

Use `--raw` for the original one-line complete value when a script needs it. Verbose output summarizes binary response bodies, avoids printing a duplicate body string when parsed JSON is available, and clearly marks very long string previews as truncated.

The repository's automated acceptance suite does not depend on Postman Echo. `./scripts/acceptance-http.sh` starts private HTTP and HTTPS fixtures on random loopback ports, runs the checks, and stops the fixtures automatically. This makes tests repeatable and removes any manual server setup.

Argument values accept Flow literals such as `42`, `true`, `2s`, arrays, and objects. Other command-line values are strings. Missing, duplicate, and unknown arguments fail before execution.

## Run the HTTP example

The complete HTTP example also uses Postman Echo, so it runs without local setup or environment variables:

```bash
cargo run -- run examples/http.flow
```

The example reads seed data with `GET`, uses that response to construct a `POST` JSON payload, and returns the echoed payload:

```json
{
  "active": true,
  "name": "Ada",
  "roles": ["tester"],
  "sourceId": "seed-42"
}
```

## Language currently available

### Values and flows

Flow supports null, booleans, non-negative 64-bit integer literals, floating-point numbers, strings, durations, arrays, and objects:

```flow
flow describeUser(user) {
    result = {
        id: user.id
        active: true
        roles: ["tester", "developer"]
        timeout: 500ms
    }

    return result
}
```

Bindings are immutable. Named flows can use a block or a concise expression body. Flow calls are validated before execution, including name resolution and argument counts. Recursive calls are currently rejected.

```flow
flow health(baseUrl) = http.get("${baseUrl}/health")

flow getUser(baseUrl, userId) =
    http.get("${baseUrl}/users/${userId}")
```

`flow main()` remains the conventional default entry point but is not required. The CLI can execute any named flow, or an anonymous flow identified by its source line.

Strings support interpolation of names and member paths:

```flow
path = "/users/${user.id}"
```

For a simple interpolation such as `${API_URL}`, resolution first checks flow parameters and locals, then active context fields, and finally the process environment. A missing fallback environment variable fails before its operation begins. `env("API_URL")` remains available when explicit environment access is clearer.

### Contexts and environment variables

A flow can apply one context in its preamble:

```flow
context api {
    apiToken: env("API_TOKEN")

    defaults http {
        baseUrl: env("API_URL")
        timeout: 5s

        headers: {
            "Accept": "application/json"
            "Authorization": "Bearer ${apiToken}"
        }
    }
}

flow main() {
    use context api
    response = http.get("/health")
    return response.status
}
```

`env()` requires the named process environment variable. A missing variable fails before the first operation in that flow. Context values are immutable and scoped to the flow activation. Child flows inherit capability defaults from their caller; a child that declares its own context overlays those defaults.

A file-level directive provides a default context for every flow in that file, including bare anonymous calls:

```flow
context api {
    defaults http {
        baseUrl: "${API_URL}"
    }
}

use context api

http.get("/health")
flow ready() = http.get("/ready")
```

A context declared inside a named or anonymous block flow replaces the file default for that flow.

Context composition is not available yet, so a flow may declare one `use context` directive. It must appear before executable statements.

### HTTP

Available operations:

```flow
response = http.get("/users/42")

created = http.post("/users") {
    json: {
        name: "Ada"
        active: true
        roles: ["tester"]
    }
}
```

An absolute `http://` or `https://` URL can be passed directly. A relative URL requires `baseUrl` in the active HTTP defaults or operation block.

HTTP defaults and operation options are compiler-validated:

| Option | Type | Behavior |
| --- | --- | --- |
| `baseUrl` | String | Prefix for relative request URLs |
| `timeout` | Duration | Whole-request deadline; defaults to 30 seconds |
| `headers` | Object of strings | Request headers |
| `maxResponseBytes` | Positive integer | Bounded response body; defaults to 10 MiB |
| `tls.verifyCertificates` | Boolean | Certificate and hostname verification; defaults to `true` |
| `json` | JSON value | `POST` request body; sets `Content-Type` when absent |

`json` is accepted by `http.post` and rejected on `http.get`. Unknown fields and statically incorrect types fail during `flow check`.

An HTTP response is an object with:

| Member | Type |
| --- | --- |
| `status` | Integer HTTP status code |
| `headers` | Object containing response header strings |
| `body` | Response body decoded as text |
| `bodyBytes` | Response body as raw bytes |
| `json` | Parsed JSON value, or `null` when the body is not valid JSON |
| `method` | Request method string |
| `url` | Effective URL string |

HTTP status codes remain response values. Transport errors, timeouts, invalid configuration, and oversized bodies fail the flow.

HTTPS uses Rustls and Mozilla WebPKI roots. Certificate and hostname validation is enabled by default. Local systems with intentionally untrusted certificates can opt out explicitly:

```flow
defaults http {
    tls: {
        verifyCertificates: false
    }
}
```

This disables server authentication and should only be used for controlled test systems.

## Diagnostics

Syntax, compiler, and runtime errors return a nonzero status and point to the relevant source:

```text
error: unknown option `banana`
 --> tests/fixtures/invalid-http-option.flow:3:9
  |
3 |         banana: true
  |         ^^^^^^
```

Runtime errors include the Flow call stack. The CLI writes the returned value to standard output and diagnostics to standard error. HTTP results default to a concise summary; `--verbose` shows the complete formatted envelope and `--raw` retains the single-line machine-oriented representation.

## Commands

```text
flow check <file>  Parse, resolve, and schema-check a Flow source file
flow list <file>   List runnable flows and their source lines
flow run <file> [flow-name] [--line <line>] [--arg <name=value>]... [--verbose | --raw]
flow --help        Show command help
flow --version     Show the binary version
```

## VS Code support

The included VS Code extension provides `.flow` file recognition, syntax highlighting, comments, brackets, indentation, folding, snippets, and compiler-backed **Run Flow** CodeLens actions above named and anonymous flows. It prompts for declared parameters and runs the selected flow with concise status output in a dedicated task terminal.

```bash
cargo install --path crates/flow-cli --locked
cd util/plugin/vscode
npm run package
code --install-extension dist/flow-language-0.3.0.vsix --force
```

See [`util/plugin/vscode/README.md`](util/plugin/vscode/README.md).

## Repository layout

```text
crates/flow-syntax       Lexer, parser, AST, and source spans
crates/flow-capability   Capability schemas, values, and runtime interface
crates/flow-compiler     Resolution, validation, and execution-plan lowering
crates/flow-runtime      Async execution-plan interpreter and context scopes
crates/flow-http         HTTP schema, pooled client, JSON, timeouts, and TLS
crates/flow-cli          Native command-line interface and diagnostics
examples/                Runnable Flow programs
tests/fixtures/          Deterministic HTTP programs, invalid programs, and local TLS material
util/plugin/vscode/      Installable VS Code language extension
util/test-server/        Local HTTP/HTTPS acceptance fixture
docs/                    Language, runtime, and dependency documentation
```

Third-party Rust dependencies and their licences are documented in [`docs/dependencies.md`](docs/dependencies.md). The full locked graph is checked in [`docs/third-party-licenses.md`](docs/third-party-licenses.md).

## Current limits

- HTTP/1.1 `GET` and `POST` only
- no redirects or proxy discovery
- one directly applied context per flow
- no multi-file project discovery, context composition, or namespaces
- no assertions, retries, deadlines scopes, parallel execution, or load generation
- no custom CA bundles, client certificates, or mutual TLS
- Linux is the tested release platform

These limits are explicit so examples and documentation describe the executable language as it exists now.
