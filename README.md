# Mettle

> [!WARNING]
> **Mettle is under active development and is not ready for production use.** The language, command-line interface, capability APIs, and project format may change between revisions. Use it to experiment, test the current direction, and contribute feedback.

Mettle is an I/O-oriented programming language and native runtime for protocol workflows, functional checks, and high-performance load tests.

The same flow can begin as a quick manual probe, grow into a multi-step integration workflow, and later run under concurrency or rate policies without rewriting its protocol logic. Mettle gives the runtime direct knowledge of I/O, time, cancellation, retries, parallel work, and measurement, so these concerns compose as language features.

Protocols are capabilities rather than syntax baked into the language. HTTP is the first implemented capability. SIP is planned next, and the same model is intended to support capabilities such as gRPC, WebSocket, Kafka, and user-provided protocols. Each capability owns its operations, configuration schema, result types, runtime behavior, metrics, and redaction rules.

This HTTP flow is executable today:

```mettle
flow createUser() {
    seed = http.get("https://jsonplaceholder.typicode.com/users/1")

    created = http.post("https://jsonplaceholder.typicode.com/posts") {
        json: {
            name: seed.json.name
            active: true
            roles: ["tester"]
        }
    }

    assert(created.status == 201)
    return created.json
}
```

```bash
mettle run users.mettle createUser
```

Mettle compiles source into a validated execution plan before the runtime performs any I/O. Names, arguments, capability options, and bounded execution policies are checked up front. The native Rust runtime then executes that plan asynchronously and reuses resources such as HTTP connection pools.

The project is experimental. Its compiler, runtime, and capability boundary are being built as production foundations, even while the available protocol surface remains intentionally small. The architecture targets Linux, macOS, and Windows, with native checks for each operating system.

## One language, several jobs

A Mettle project is made from a few general concepts:

- **flows** name reusable sequences of operations;
- **tests** run assertions against flows and capability results;
- **capabilities** provide protocol operations such as `http.get()` and the planned `sip.options()`;
- **contexts** compose environment values and capability defaults;
- **execution policies** control deadlines, retries, parallelism, concurrency, and rate;
- **scoped results** make assertions and performance measurements explicit.

I/O suspends lightweight runtime work automatically. Source code does not need `async` and `await` around every operation. Concurrency appears where it matters through structured forms such as `parallel`, and all child work remains owned by an enclosing scope for cancellation and cleanup.

## Use Mettle

Mettle is intended to be a single native executable. Once release packages are available, install the `mettle` binary for your platform and place it on `PATH`.

Until then, build it from a checkout with Rust:

```bash
cargo install --path crates/mettle-cli --locked
mettle --version
```

### VS Code extension

The repository includes the Mettle Language extension for syntax highlighting,
parser/compiler diagnostics, flow and test CodeLens actions, and go-to-definition. Build and install its VSIX from
the repository checkout:

```bash
cd util/plugin/vscode
npm run package
code --install-extension dist/mettle-language-0.13.0.vsix --force
```

If the `code` launcher is unavailable, in VS Code open the Extensions view,
choose **Install from VSIX…**, and select the generated package. The extension
requires the `mettle` CLI on `PATH`; configure **Mettle: Executable Path** when
the binary is elsewhere. See [`util/plugin/vscode/README.md`](util/plugin/vscode/README.md)
for editor features and development details.

The included request collection uses the public JSONPlaceholder test API. It needs an internet connection but no account, credentials, environment variables, or local server.

### Platform support

The Mettle compiler, runtime, HTTP capability, CLI, and VS Code extension support Linux, Windows, and macOS. Native CI builds and tests Linux x64, Windows x64, Apple Silicon macOS, and Intel macOS. The portable acceptance suite executes real HTTP workflows and a local load test on each platform.

The repository does not publish prebuilt executables yet, so the current installation path requires Rustup and Cargo on every platform. Release archives and package-manager installation are part of release readiness work.

Most contributor acceptance scripts use Bash because Linux remains the primary development environment. `python scripts/acceptance-portable.py` provides the operating-system-neutral runtime smoke test used by CI.

```bash
mettle check examples/request-collection.mettle
mettle list examples/request-collection.mettle
mettle run examples/request-collection.mettle inspectRequest \
  --arg baseUrl=https://jsonplaceholder.typicode.com \
  --arg requestId=1
```

## Start with one operation

A top-level capability call is a runnable anonymous flow. With the current HTTP capability, a file can be as small as one request:

```mettle
http.get("https://jsonplaceholder.typicode.com/posts/1")
```

Give the work a name only when it needs inputs or more than one step.

```mettle
flow getPost(baseUrl, postId) = http.get("${baseUrl}/posts/${postId}")

flow inspectRequest(baseUrl, requestId) {
    response = http.get("${baseUrl}/posts/${requestId}")
    assert(response.status == 200)
    return response.json.args
}
```

Run a named flow directly:

```bash
mettle run requests.mettle getPost \
  --arg baseUrl=https://jsonplaceholder.typicode.com \
  --arg postId=1
```

`main` is the conventional default flow, but it is optional. If a file has exactly one runnable flow, Mettle runs it without a name. If there are several, select one by name or by a source line from `mettle list`.

```bash
mettle run examples/request-collection.mettle --line 3
```

Run every zero-argument flow in source order with `--all`. Parameterized flows
are deliberately skipped, so this is useful for a collection of self-contained
checks. Each flow still performs its real I/O; Mettle continues after a failed
flow and returns a nonzero status if any executed flow fails. Human output ends
with a batch summary. `--output json` emits JSON Lines: a `start` record, one
atomic `result` or `failure` record per executed flow, and a final `summary`
record.

```bash
mettle run checks.mettle --all
```

## Write executable tests

Declare checks with `test("name") { ... }`. Tests have no parameters or return
value, and a failed `assert(...)` fails that test without stopping the rest of
the file. `mettle test <file>` runs tests declared in that file in source order;
`mettle test <file> "test name"` or `mettle test <file> --line <line>` runs one test;
`mettle run <file> --all` still runs only zero-argument flows. The test command
exits nonzero when any test fails or the file has no tests, and supports `--verbose`, `--quiet`, and
`--output json` (JSON Lines) for CI.

```mettle
flow getPost(id) = http.get("https://jsonplaceholder.typicode.com/posts/${id}")

test("post 1 is available") {
    response = getPost(1)
    assert(response.status == 200)
    assert(response.json.id == 1)
}
```

Run the full example with `mettle test examples/http-tests.mettle`, or run one
test by name with `mettle test examples/http-tests.mettle "post 1 is available"`.

## Build a workflow from operation results

Capability operations return values. Bind one to a name, use its result to construct the next operation, then return what matters. Bindings are immutable, which keeps the data path easy to follow. The current HTTP capability exposes parsed JSON directly:

```mettle
context publicApi {
    defaults http {
        baseUrl: "https://jsonplaceholder.typicode.com"
        timeout: 10s
        headers: {
            "Accept": "application/json"
        }
    }
}

flow createUserFromSeed() {
    use context publicApi

    seed = http.get("/users/1")

    created = http.post("/posts") {
        json: {
            sourceId: seed.json.id
            name: seed.json.name
            active: true
            roles: ["tester"]
        }
    }

    assert(created.status == 201)
    return created.json
}
```

This is the complete shape used by [`examples/http.mettle`](examples/http.mettle):

```bash
mettle run examples/http.mettle
```

An HTTP response exposes `status`, `headers`, `body`, `bodyBytes`, `json`, `method`, `url`, and `duration`. A non-JSON response has `json: null`. HTTP status codes are ordinary values, so assertions make the expected condition obvious. Header names containing punctuation use string-key access, such as `response.headers["content-type"]`.

## Put shared setup in contexts

Contexts hold immutable values and capability defaults. A flow applies one context with `use context`; child flows inherit its defaults. A file-level `use context` applies a default to every flow and test in that source file, regardless of where the directive appears; placing it near the top is the recommended convention. For one-file setup, use an anonymous `use context { ... }`. Name it with `use context name { ... }` only when it should also be reusable. Plain `context name { ... }` remains reusable without applying itself. A flow-level context overrides the file default. Contexts can compose, so base URLs, authentication, and service-specific settings can live separately.

```mettle
context baseApi {
    defaults http {
        baseUrl: env("API_URL")
        timeout: 5s
        headers: {
            "Accept": "application/json"
        }
    }
}

use context {
    use context baseApi
    apiToken: senv("API_TOKEN")

    defaults http {
        headers: {
            "Authorization": "Bearer ${apiToken}"
        }
    }
}

flow currentUser() {
    return http.get("/me")
}
```

`env("API_URL")` requires an environment variable. Within a string, `${API_URL}` first resolves a flow local, parameter, or context value, then falls back to the process environment. That keeps a one-off file pleasant to use:

```mettle
flow health() = http.get("${API_URL}/health")
```

`env()` does not make a value secret by itself. Use `senv("API_TOKEN")` as shorthand
for `secret(env("API_TOKEN"))`, or wrap a value from another source with
`secret(...)`. Sensitivity propagates through interpolation and structured
values, and runtime CLI, JSON, and capability report output replaces
them with `[REDACTED]`. Syntax and compile diagnostics can print source lines,
so never put literal credentials in `.mettle` files; load them with `senv()`.
HTTP also redacts credential-bearing headers such as
`Authorization`, `Cookie`, and `Set-Cookie`. See
[`examples/secrets.mettle`](examples/secrets.mettle) for both forms; set
`API_TOKEN` before running it.

### Environment files and profiles

`mettle run` and `mettle test` load `.env` automatically beside the selected
entry file. In a project, they load the project-root `.env` first and then the
entry file's directory `.env` if it differs. Choose an overlay with
`--profile qa`, which loads `.env.qa` from the same locations; `--profile prod`
similarly loads `.env.prod`. A requested profile must exist. Process environment
variables override file values, and neither `check`, `list`, nor the language
server needs an env file. `env()` and `${NAME}` see the same resolved values;
use `senv()` for values that must be redacted.

```bash
mettle run examples/profile-standalone/main.mettle
mettle run examples/profile-standalone/main.mettle --profile qa
mettle run examples/project/main.mettle --profile qa
```

The [standalone profile example](examples/profile-standalone/main.mettle) and
[project example](examples/project/main.mettle) include safe demo `.env`,
`.env.qa`, and `.env.prod` files. Dotenv files support `NAME=value`, optional
`export`, comments, and quoted values; they do not execute shell code or expand
variables. Outside these allowlisted examples, `.env` files are Git-ignored.

## Control how work executes

Execution policies are independent of the protocol being exercised. They can be nested, assigned, returned, and combined with capability calls. Mettle currently implements `within`, `retry`, bounded `parallel`, `rate`, and fixed `concurrency`:

```mettle
flow probe(path) {
    response = retry(attempts: 3, delay: 100ms) {
        http.get("https://jsonplaceholder.typicode.com${path}")
    }
    return response.status
}

flow readiness() {
    return within(timeout: 5s) {
        parallel(limit: 2) {
            probe("/posts/1")
            probe("/users/1")
            probe("/todos/1")
        }
    }
}
```

`parallel` returns results in source order and never starts more branches than `limit`. If a branch fails, active siblings are cancelled and joined. `retry` counts the first execution as an attempt and returns the first successful result. `within` covers all nested work, including retry delays. Ctrl+C cancels the root execution and exits with status 130.

This gives every operation an owner, a lifetime, and a cleanup path. A flow that works as a functional check can run inside a load test without duplicating its operations.

Rate-driven workloads use an arrival target. `limit` bounds active iterations; when that limit is full, Mettle records a dropped start instead of building an unbounded queue. The finalized result remains scoped to its binding:

```mettle
load = rate(target: 1_000, period: 1s, duration: 30s, limit: 200) {
    readiness()
}

assert(load.errors < 0.001)
assert(load.latency.p95 < 200ms)
assert(load.dropped == 0)
```

`concurrency(limit: 100, duration: 30s) { ... }` keeps a fixed number of iterations active during its scheduling window. Both policies stop admitting work when the window closes, drain owned iterations for up to 30 seconds, and expose whether that drain timed out.

Workload results include `count`, `started`, `success`, `failed`, `errors`, `dropped`, `saturated`, total `duration`, and bounded-memory distributions for `latency` and `schedulingDelay`. Distributions expose `min`, `mean`, `max`, `p50`, `p90`, `p95`, and `p99`. Rate results also report `scheduled`, `rate.target`, `rate.period`, `rate.actual`, and `rate.limit`.

Run the included public example with:

```bash
mettle run examples/load-test.mettle
```

## Organize a project without import boilerplate

A `mettle.toml` file marks a project root. Running an entry file below it discovers every `.mettle` file in that project. Files contribute declarations directly, so there are no import or export lists to maintain.

```text
service-checks/
├── mettle.toml
├── core.mettle
├── users.mettle
└── main.mettle
```

Files without a namespace are in the implicit global namespace. Use a namespace when the project needs a clear boundary, then make it visible explicitly.

```mettle
namespace users
use namespace core

flow getUser(id) {
    use context api
    return http.get("/users/${id}")
}
```

```bash
mettle run examples/project/main.mettle
```

## Protocol capabilities

The core parser understands calls, values, flows, contexts, and execution policies. It does not need a special grammar rule for each protocol verb. The compiler resolves a qualified call such as `http.get()`, `sip.options()`, `grpc.call()`, or `kafka.publish()` through a registered capability.

A capability contributes:

- named operations and their signatures;
- schemas for defaults, options, payloads, and results;
- compile-time validation;
- runtime execution and resource management;
- protocol metrics and sensitive-data redaction.

SIP is the next important test of this design because it introduces transactions, retransmission, provisional responses, dialogs, and cleanup. The planned source form uses the same language concepts as HTTP:

```mettle
context sipClient {
    domain: env("SIP_DOMAIN")

    defaults sip {
        transport: udp
        timeout: 3s
        from: env("SIP_CALLER_URI")
    }
}

flow probeSip() {
    use context sipClient
    response = sip.options("sip:${domain}")
    assert(response.status == 200)
    return response
}
```

The SIP capability and this exact schema are planned work. HTTP is the only protocol capability included in the executable today. The capability contract already lives outside the parser and HTTP client implementation, so adding a protocol does not require turning its methods into language keywords.

## HTTP support today

Mettle currently supports HTTP/1.1 `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, and `OPTIONS` over HTTP or HTTPS. Absolute URLs work anywhere. Relative URLs use `baseUrl` from the active HTTP defaults or the operation itself.

```mettle
response = http.post("/users") {
    timeout: 2s
    headers: {
        "X-Request-Source": "smoke-test"
    }
    json: {
        name: "Ada"
        active: true
    }
}
```

Mettle validates options during `mettle check`, before it opens a connection.

| Option | Type | Meaning |
| --- | --- | --- |
| `baseUrl` | String | Prefix for a relative URL |
| `timeout` | Duration | Deadline for the complete request; default: 30 seconds |
| `headers` | Object of strings | Request headers |
| `maxResponseBytes` | Positive integer | Response body limit; default: 10 MiB |
| `tls.verifyCertificates` | Boolean | Certificate and hostname validation; default: `true` |
| `json` | JSON value | Body for `post`, `put`, `patch`, or `delete`; sets `Content-Type` when absent |
| `body` | String | Text body for `post`, `put`, `patch`, or `delete`; defaults to UTF-8 `text/plain` |

`json` and `body` are mutually exclusive and Mettle rejects the combination during compilation. An explicit `Content-Type` used with `json` must be `application/json` or a media type ending in `+json`. A response that declares one of those media types but contains malformed JSON fails with a clear protocol error. The response size limit is enforced from `Content-Length` when available and while streaming the body.

External data does not have to use Mettle identifier names. Use a quoted key after brackets for HTTP headers or JSON properties containing punctuation:

```mettle
requestId = response.headers["x-request-id"]
displayName = response.json["display-name"]
```

HTTPS certificate and hostname validation is enabled by default. A controlled test system with an intentionally untrusted certificate can opt out explicitly:

```mettle
defaults http {
    tls: {
        verifyCertificates: false
    }
}
```

## CLI output and diagnostics

Mettle presents a flow as one execution rather than dumping its internal value. A normal HTTP workflow shows each operation, its status and timing, the useful response payload, and the total duration:

```text
createUser

  ✓ GET    https://api.example.com/users/seed
    200 · 48ms

  ✓ POST   https://api.example.com/users
    201 · 91ms

  Response
    {
      "id": "created-seed-42",
      "active": true
    }

✓ Completed in 141ms
```

Large payloads are formatted and capped in the default view. `--verbose` expands
every HTTP operation with readable response headers and one decoded body, without
dumping duplicate raw body bytes. `--quiet` prints only the final flow status,
`--raw` prints only the returned value, and `--output json` produces a stable
execution envelope for automation. `--no-color` disables ANSI colors.

```bash
mettle run examples/request-collection.mettle --line 10 --verbose
mettle run examples/request-collection.mettle --line 10 --output json
```

Rate and concurrency workloads use an in-place dashboard when stderr is attached to a terminal. It updates the execution phase, active iterations, achieved rate, outcomes, dropped starts, and latency percentiles while the workload is running. Redirected output and machine-readable modes remain deterministic. Use `--no-progress` to disable the dashboard explicitly.

```text
Mettle · rate 1,000/1s for 30s
RUNNING   12.4s / 30s   active 87 / 200   achieved 998.2/s
started 12,400   completed 12,313   ok 12,302   failed 11   dropped 0
latency p50 38.2ms   p95 71.6ms   p99 104.8ms
```

Syntax, validation, and runtime failures return a nonzero status with source context. Runtime failures include the Mettle flow stack.

```text
error: unknown option `banana`
 --> tests/fixtures/invalid-http-option.mettle:3:9
  |
3 |         banana: true
  |         ^^^^^^
```

## VS Code extension

The included extension provides `.mettle` recognition, syntax highlighting, snippets, folding, parser/compiler diagnostics for unsaved edits, CodeLens play buttons to run individual flows and tests, and Ctrl+Click navigation for flows, contexts, parameters, and local bindings. Navigation and diagnostics are backed by `mettle lsp`, so they follow the same project and namespace rules as the CLI.

```bash
cd util/plugin/vscode
npm run package
code --install-extension dist/mettle-language-0.13.0.vsix --force
```

The extension looks for `mettle` on `PATH`. Set **Mettle: Executable Path** if the binary lives elsewhere. With a `.mettle` file open, click **Mettle profile: Default** (or the current profile) in the bottom status bar, use the gear icon in the editor title bar, or run **Mettle: Select Profile** from the Command Palette. The picker discovers `.env` and `.env.<name>` files for the active file; **Default** uses `.env` and no `--profile` flag. The selection is remembered per project or standalone-file directory and is passed to Run Flow, Run All, and Run Tests actions. Read [`util/plugin/vscode/README.md`](util/plugin/vscode/README.md) for installation details.

## For contributors

The checked-in toolchain is Rust 1.98.1. Cargo picks it automatically when Rustup is installed. Python 3 is only required for the repository's local HTTP and HTTPS acceptance fixture.

```bash
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
./scripts/check-licenses.py
./scripts/acceptance-http.sh
./scripts/acceptance-project.sh
./scripts/acceptance-execution.sh
./scripts/acceptance-load.sh
```

The acceptance scripts use private HTTP and HTTPS fixtures on random loopback ports. They cover request chaining, JSON, environment configuration, connection reuse, timeouts, TLS, project resolution, context composition, assertions, retries, concurrency bounds, cancellation, and redaction without depending on public services.

Build an optimized executable with:

```bash
cargo build --release
./target/release/mettle --version
```

Run the reproducible local load benchmark with:

```bash
./scripts/benchmark-load.sh
```

It builds the release binary, starts an isolated fixture, executes 5,000 scheduled iterations, and writes the workload result, peak resident memory, CPU time, elapsed time, OS, CPU, Rust version, Git revision, fixture configuration, and exact command under `target/benchmarks/`. Allocation profiling can be layered onto the same command with a system profiler without adding instrumentation to the runtime hot path. See [docs/load-benchmarks.md](docs/load-benchmarks.md) for the baseline and comparison method.

## Repository map

```text
crates/mettle-syntax       Lexer, parser, AST, and source spans
crates/mettle-capability   Capability schemas, values, and runtime interface
crates/mettle-compiler     Resolution, validation, and execution-plan lowering
crates/mettle-runtime      Async execution-plan interpreter and context scopes
crates/mettle-http         HTTP schema, pooled client, JSON, timeouts, and TLS
crates/mettle-cli          Native command-line interface and diagnostics
examples/                  Runnable Mettle programs
tests/fixtures/            Deterministic HTTP programs and local TLS material
tests/projects/            Multi-file project fixtures
util/plugin/vscode/        Installable VS Code extension
util/test-server/          Local HTTP and HTTPS acceptance fixture
docs/                      Language, runtime, and dependency documentation
```

The [language proposal](docs/language-proposal.md) describes the language direction. The [technical strategy](docs/mettle-technical.md) explains the runtime and compiler approach. Third-party Rust dependencies and licences are documented in [docs/dependencies.md](docs/dependencies.md) and [docs/third-party-licenses.md](docs/third-party-licenses.md).

The larger Rust crates keep their public API in `lib.rs` and separate parsing,
declaration navigation, semantic lowering, execution, and HTTP schemas into
focused source modules.

## Current limits

- no redirects or proxy discovery
- request bodies currently support JSON values and UTF-8 text; multipart forms and streaming bodies are not implemented
- no workload ramping, distributed workers, or per-operation metric breakdowns yet
- the SIP capability and external capability distribution model are still planned work
- no custom CA bundles, client certificates, or mutual TLS
- no prebuilt Windows, macOS, or Linux release archives yet
