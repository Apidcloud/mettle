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

The project is experimental. Its compiler, runtime, and capability boundary are being built as production foundations, even while the available protocol surface remains intentionally small. The architecture targets Linux, macOS, and Windows; Linux is the platform exercised by the repository today.

## One language, several jobs

A Mettle project is made from a few general concepts:

- **flows** name reusable sequences of operations;
- **capabilities** provide protocol operations such as `http.get()` and the planned `sip.options()`;
- **contexts** compose environment values and capability defaults;
- **execution policies** control deadlines, retries, parallelism, concurrency, and rate;
- **tests and results** make assertions and performance measurements explicit.

I/O suspends lightweight runtime work automatically. Source code does not need `async` and `await` around every operation. Concurrency appears where it matters through structured forms such as `parallel`, and all child work remains owned by an enclosing scope for cancellation and cleanup.

## Use Mettle

Mettle is intended to be a single native executable. Once release packages are available, install the `mettle` binary for your platform and place it on `PATH`.

Until then, build it from a checkout with Rust:

```bash
cargo install --path crates/mettle-cli --locked
mettle --version
```

The included request collection uses the public JSONPlaceholder test API. It needs an internet connection but no account, credentials, environment variables, or local server.

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

An HTTP response exposes `status`, `headers`, `body`, `bodyBytes`, `json`, `method`, and `url`. A non-JSON response has `json: null`. HTTP status codes are ordinary values, so assertions make the expected condition obvious.

## Put shared setup in contexts

Contexts hold immutable values and capability defaults. A flow applies one context with `use context`; child flows inherit its defaults. Contexts can compose, so base URLs, authentication, and service-specific settings can live separately.

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

context authenticatedApi {
    use context baseApi
    apiToken: env("API_TOKEN")

    defaults http {
        headers: {
            "Authorization": "Bearer ${apiToken}"
        }
    }
}

flow currentUser() {
    use context authenticatedApi
    return http.get("/me")
}
```

`env("API_URL")` requires an environment variable. Within a string, `${API_URL}` first resolves a flow local, parameter, or context value, then falls back to the process environment. That keeps a one-off file pleasant to use:

```mettle
flow health() = http.get("${API_URL}/health")
```

## Control how work executes

Execution policies are independent of the protocol being exercised. They can be nested, assigned, returned, and combined with capability calls. The currently implemented policies are `within`, `retry`, and bounded `parallel`:

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

This gives every operation an owner, a lifetime, and a cleanup path. Rate-driven execution and scoped performance results build on the same model. A flow that works as a functional check should be reusable inside a load test without duplicating its operations.

The proposed load-test form keeps the result scoped and named:

```mettle
load = rate(target: 1_000, period: 1s, duration: 30s) {
    readiness()
}

assert(load.successRate > 0.999)
assert(load.latency.p95 < 200ms)
```

`rate` and performance result aggregation are part of the language direction and are not implemented yet.

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

Mettle currently supports HTTP/1.1 `GET` and `POST` over HTTP or HTTPS. Absolute URLs work anywhere. Relative URLs use `baseUrl` from the active HTTP defaults or the operation itself.

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
| `json` | JSON value | Body for `http.post`; sets `Content-Type` when absent |

HTTPS certificate and hostname validation is enabled by default. A controlled test system with an intentionally untrusted certificate can opt out explicitly:

```mettle
defaults http {
    tls: {
        verifyCertificates: false
    }
}
```

## CLI output and diagnostics

Normal HTTP output is compact:

```text
GET https://jsonplaceholder.typicode.com/posts/1 200
```

Use `--verbose` to inspect the formatted response envelope, headers, response data, and body metadata. Use `--raw` for a one-line complete value in scripts.

```bash
mettle run examples/request-collection.mettle --line 10 --verbose
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

The included extension provides `.mettle` recognition, syntax highlighting, snippets, folding, CodeLens actions to run flows, and Ctrl+Click navigation for flows, contexts, parameters, and local bindings. Navigation is backed by `mettle lsp`, so it follows the same project and namespace rules as the CLI.

```bash
cd util/plugin/vscode
npm run package
code --install-extension dist/mettle-language-0.7.0.vsix --force
```

The extension looks for `mettle` on `PATH`. Set **Mettle: Executable Path** if the binary lives elsewhere. Read [`util/plugin/vscode/README.md`](util/plugin/vscode/README.md) for installation details.

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
```

The acceptance scripts use private HTTP and HTTPS fixtures on random loopback ports. They cover request chaining, JSON, environment configuration, connection reuse, timeouts, TLS, project resolution, context composition, assertions, retries, concurrency bounds, cancellation, and redaction without depending on public services.

Build an optimized executable with:

```bash
cargo build --release
./target/release/mettle --version
```

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

## Current limits

- HTTP/1.1 `GET` and `POST` are the only protocol operations implemented today
- no redirects or proxy discovery
- no rate-driven workload controller or load-test reporting yet
- the SIP capability and external capability distribution model are still planned work
- no custom CA bundles, client certificates, or mutual TLS
- Linux is the tested release platform
