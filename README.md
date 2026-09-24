# Mettle

> [!WARNING]
> **Mettle is under active development and is not ready for production use.** The language, command-line interface, capability APIs, and project format may change between revisions. Use it to experiment, test the current direction, and contribute feedback.

Mettle is an I/O-oriented programming language and native runtime for protocol workflows, functional checks, and high-performance load tests.

The same flow can begin as a quick manual probe, grow into a multi-step integration workflow, and later run under concurrency or rate policies without rewriting its protocol logic. Mettle gives the runtime direct knowledge of I/O, time, cancellation, retries, parallel work, and measurement, so these concerns compose as language features.

Protocols are capabilities rather than syntax baked into the language. HTTP is the first implemented capability. SIP is planned next, and the same model is intended to support capabilities such as gRPC, WebSocket, Kafka, and user-provided protocols. Each capability owns its operations, configuration schema, result types, runtime behavior, metrics, and redaction rules.

This HTTP flow is executable today:

```mettle
flow createUser {
    seed = http.get("https://jsonplaceholder.typicode.com/users/1")

    created = http.post("https://jsonplaceholder.typicode.com/posts", body: {
            name: seed.json.name
            active: true
            roles: ["tester"]
        })

    assert(created.status == 201)
    created.json
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

### Neovim

Neovim 0.11+ can use its built-in LSP client for diagnostics and navigation;
no Neovim plugin is required. Build or install the `mettle` CLI, then add
this to your `init.lua` or a Lua module loaded by it:

```lua
vim.filetype.add({ extension = { mettle = "mettle" } })

vim.lsp.config("mettle", {
  cmd = { "mettle", "lsp" },
  filetypes = { "mettle" },
  root_markers = { "mettle.toml" },
  workspace_required = false,
})
vim.lsp.enable("mettle")
```

If you built the CLI with `cargo build -p mettle-cli` rather than installing
it on `PATH`, replace `"mettle"` in `cmd` with the absolute path to
`target/debug/mettle`. Open a `.mettle` file and run `:checkhealth vim.lsp`
to confirm that the server attached.

The server reports diagnostics for unsaved edits. For the time being, given Mettle doesn't have separate implementation declarations, go-to-implementation returns the same declaration as go-to-definition.

### VS Code extension

The repository includes the Mettle Language extension for syntax highlighting,
parser/compiler diagnostics, flow and test CodeLens actions, and go-to-definition (go-to-implementation resolves to the same). Build and install its VSIX from
the repository checkout:

```bash
cd util/plugin/vscode
npm run package
code --install-extension dist/mettle-language-1.0.0-alpha.1.vsix --force
```

If the `code` launcher is unavailable, in VS Code open the Extensions view,
choose **Install from VSIX…**, and select the generated package. The extension
requires the `mettle` CLI on `PATH`; configure **Mettle: Executable Path** when
the binary is elsewhere. See [`util/plugin/vscode/README.md`](util/plugin/vscode/README.md)
for editor features and development details.

Start with the [example index](examples/README.md): `language/` contains network-free
walkthroughs plus one multi-file project; `http/` contains public-API workflows.
The HTTP examples need an internet connection but no account or credentials.

### Platform support

The Mettle compiler, runtime, HTTP capability, CLI, and VS Code extension support Linux, Windows, and macOS. Native CI builds and tests Linux x64, Windows x64, Apple Silicon macOS, and Intel macOS. The portable acceptance suite executes real HTTP workflows and a local load test on each platform.

The repository does not publish prebuilt executables yet, so the current installation path requires Rustup and Cargo on every platform. Release archives and package-manager installation are part of release readiness work.

Most contributor acceptance scripts use Bash because Linux remains the primary development environment. `python scripts/acceptance-portable.py` provides the operating-system-neutral runtime smoke test used by CI.

```bash
mettle check examples/http/requests.mettle
mettle list examples/http/requests.mettle
mettle run examples/http/requests.mettle inspectRequest \
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
flow inspectRequest(baseUrl, requestId) =
    http.get("${baseUrl}/posts/${requestId}")
```

Run a named flow directly:

```bash
mettle run examples/http/requests.mettle inspectRequest \
  --arg baseUrl=https://jsonplaceholder.typicode.com \
  --arg requestId=1
```

`main` is the conventional default flow, but it is optional. If a file has exactly one runnable flow, Mettle runs it without a name. If there are several, select one by name or by a source line from `mettle list`.

```bash
mettle list examples/http/requests.mettle
mettle run examples/http/requests.mettle --line 4
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

Declare checks with `test "name" { ... }`. The older `test("name")` spelling
also works. Tests have no parameters or return
value. Use `assert(condition, "message")` to explain a failure; the message is
optional and may interpolate values. A test collects false assertions and reports
each one with its source location, then continues to the next test. A runtime
error stops the current test but preserves any earlier assertion failures.
Assertions inside ordinary flows still fail immediately. `mettle test <file>`
runs tests declared in that file in source order;
`mettle test <file> "test name"` or `mettle test <file> --line <line>` runs one test;
`mettle run <file> --all` still runs only zero-argument flows. The test command
exits nonzero when any test fails or the file has no tests, and supports `--verbose`, `--quiet`, and
`--output json` (JSON Lines) for CI.

Use `fail("reason")` when an entry cannot continue. Unlike a collected test
assertion, it stops the current flow or test immediately and keeps earlier
diagnostics. Its message must be a string; sensitive values are redacted.
`fail` is a never-returning expression: it can be the whole body of a flow or
a branch of `parallel`. `retry` does not repeat it, and an enclosing `parallel`,
`rate`, or `concurrency` scope cancels its in-flight work. The CLI reports a
nonzero status, but other entries in `mettle test <file>` or `mettle run --all`
still run. There is no language-level `exit(code)` that kills the whole process.

```mettle
flow getPost(id) = http.get("https://jsonplaceholder.typicode.com/posts/${id}")

test "post 1 is available" {
    response = getPost(1)
    assert(response.status == 200, "post 1 should return 200")
    assert(response.json.id == 1, "post 1 should have ID 1")
}
```

Run the full example with `mettle test examples/http/tests.mettle`, or run one
test by name with `mettle test examples/http/tests.mettle "post 1 is available"`.

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

flow createUserFromSeed {
    use context publicApi

    seed = http.get("/users/1")

    created = http.post("/posts", body: {
            sourceId: seed.json.id
            name: seed.json.name
            active: true
            roles: ["tester"]
        })

    assert(created.status == 201)
    created.json
}
```

This is the complete shape used by the [HTTP workflow example](examples/http/workflow.mettle):

```bash
mettle run examples/http/workflow.mettle
```

An HTTP response exposes `status`, `headers`, `body`, `bodyBytes`, `json`, `method`, `url`, and `duration`. A non-JSON response has `json: null`. HTTP status codes are ordinary values, so assertions make the expected condition obvious. Header names containing punctuation use string-key access, such as `response.headers["content-type"]`.

## Make decisions and select values

Use `if (condition) { ... }`, optional `else if (condition) { ... }`, and optional `else { ... }` in flows and tests. Conditions must be booleans. `not` binds before comparisons, comparisons before `and`, and `and` before `or`; parentheses group explicitly. `and` and `or` short-circuit, so a skipped side does no I/O or environment lookup. A flow may return from every branch instead of ending with a separate `return`. Bindings declared inside a branch are visible only in that branch.

```mettle
flow label(users, position) {
    user = users[position]
    if (user.active and not user.disabled) {
        return user.name
    } else if (user.active == false) {
        return "inactive"
    } else {
        return "disabled"
    }
}
```

`array[0]` is zero-based; `object["key"]` and `object[keyExpression]` select string keys. Array indexes must be non-negative integers. Invalid key types, missing keys, and out-of-range positions report runtime errors. Dot access remains convenient for identifier-style object keys. For an intentional early failure, use `fail("reason")`; `return` is the early-success path in a flow.

Separate statements with newlines. Object and context fields, and `parallel` branches, may use either newlines or commas; same-line entries need commas. Arrays and call arguments always use commas; multiline arrays and calls may end with a trailing comma. A block-bodied flow or execution policy yields its final expression, so multi-step policies no longer need a helper flow. `return` remains useful for an early flow exit. A zero-argument declaration can omit parentheses (`flow main { ... }`), but calling it still requires `main()`—a bare name never starts I/O. Try the network-free [collections and numbers example](examples/language/collections-and-numbers.mettle) with `mettle run examples/language/collections-and-numbers.mettle` and `mettle test examples/language/collections-and-numbers.mettle`.

### Iterate results

`for` iterates finite arrays and objects sequentially. One binding receives each value; two receive `index, value` for an array or `key, value` for an object. Objects iterate in sorted key order, including objects returned by named `parallel`. A loop with a final expression maps values into a new collection of the same shape. A loop used only as a statement need not build a result. Inside a test, the loop continues collecting failed assertions across iterations; in an ordinary flow or retry block, an assertion still fails immediately.

```mettle
responses = parallel {
    users: http.get("/users")
    posts: http.get("/posts")
}
statuses = for name, response in responses {
    assert(response.status == 200, "${name} failed")
    response.status
}
```

Named `parallel` branches produce an object and attach labels such as `[p1:users]` to diagnostic events. Unnamed branches still produce an array in source order. A single `parallel` expression cannot mix named and unnamed branches; each branch contains one expression. Inside a policy or loop block, use the final expression for its value; an explicit `return` there is rejected because it cannot exit the enclosing flow.

### Numbers and durations

Integers accept decimal (`1_000`), hexadecimal (`0xFF`), and binary (`0b1010`) forms; `-` works with integer and decimal values. Decimals also accept exponents such as `1.25e2`. Durations use `ns`, `us`, `ms`, `s`, `m`, or `h`, including exact fractional forms such as `1.5s` and `0.25ms`. Fractions smaller than one nanosecond, negative durations, non-finite decimals, malformed separators, and out-of-range integers are rejected. Integers remain signed 64-bit values for now; incoming HTTP JSON integers outside that range fail explicitly instead of rounding. Integer and decimal comparisons are numeric without rounding a large integer first. There are no percent, rate, or byte-size suffixes yet; `0xFF` is an integer, not raw bytes.

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
[secrets example](examples/language/secrets.mettle) for both forms; set
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
mettle run examples/language/profiles/main.mettle
mettle run examples/language/profiles/main.mettle --profile qa
mettle run examples/language/project/main.mettle --profile qa
```

The [standalone profile example](examples/language/profiles/main.mettle) and
[project example](examples/language/project/main.mettle) include safe demo `.env`,
`.env.qa`, and `.env.prod` files. Dotenv files support `NAME=value`, optional
`export`, comments, and quoted values; they do not execute shell code or expand
variables. Outside these allowlisted examples, `.env` files are Git-ignored.

## Control how work executes

Execution policies are independent of the protocol being exercised. They can be nested, assigned, returned, and combined with capability calls. Mettle currently implements `within`, `retry`, `parallel`, `rate`, and fixed `concurrency`:

```mettle
flow probe(path) = retry(delay: 100ms, attempts: 3) {
    response = http.get("https://jsonplaceholder.typicode.com${path}")
    assert(response.status == 200)
    response.status
}

flow readiness() {
    return within(timeout: 5s) {
        parallel(limit: 2) {
            posts: probe("/posts/1")
            users: probe("/users/1")
            todos: probe("/todos/1")
        }
    }
}
```

`parallel` with unnamed branches returns an array in source order; named branches return an object keyed by their labels. Without `limit`, all branches may start; with it, no more than `limit` start at once. If a branch fails, active siblings are cancelled and joined. `retry` counts the first execution as an attempt and reruns its entire block—including assertions—until it succeeds or exhausts its attempts; terminal `fail(...)` bypasses retry. `within` covers all nested work, including retry delays. Ctrl+C cancels the root execution and exits with status 130.

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

Workload results include `count`, `started`, `success`, `failed`, `errors`, `dropped`, `cancelled`, `saturated`, total `duration`, and bounded-memory distributions for `latency` and `schedulingDelay`. Distributions expose `min`, `mean`, `max`, `p50`, `p90`, `p95`, and `p99`. Rate results also report `scheduled`, `rate.target`, `rate.period`, `rate.actual`, and `rate.limit`. A terminal `fail(...)` inside a workload stops scheduling, cancels in-flight iterations, and reports partial metrics with phase `aborted`.

Run the included public example with:

```bash
mettle run examples/http/load.mettle
```

For two independent rate workloads running at the same time, see the
[parallel load example](examples/http/load-parallel.mettle). Its named
`parallel` branches return separate workload metrics. The live dashboard keeps
both branches visible, including per-call-site counts, HTTP status classes,
and latency; the final report retains the same workload grouping.

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
mettle run examples/language/project/main.mettle
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
response = http.post(
    "/users",
    timeout: 2s,
    headers: { "X-Request-Source": "smoke-test" },
    body: { name: "Ada", active: true },
)
```

Mettle validates options during `mettle check`, before it opens a connection.

| Option | Type | Meaning |
| --- | --- | --- |
| `baseUrl` | String | Prefix for a relative URL |
| `timeout` | Duration | Deadline for the complete request; default: 30 seconds |
| `headers` | Object of strings | Request headers |
| `maxResponseBytes` | Positive integer | Response body limit; default: 10 MiB |
| `tls.verifyCertificates` | Boolean | Certificate and hostname validation; default: `true` |
| `body` | Object, array, string, bytes, or explicitly formatted scalar | Request payload for `post`, `put`, `patch`, or `delete` |
| `bodyFormat` | `"json"`, `"text"`, or `"bytes"` | Overrides body-format inference when needed |
| `json` | JSON value | Legacy payload option; prefer `body` in new files |

The URL is HTTP's only positional parameter and may instead be written as `url: "/users"`. Named arguments can follow positional ones, in any order; after the first named argument, no positional argument is allowed. User-defined flows follow the same positional-then-named rule. Duplicate or unknown names fail during `mettle check`, and argument expressions run once in their written order.

Objects and arrays in `body` default to JSON; strings default to UTF-8 text; bytes values default to raw bytes. The default `Content-Type` follows that format (`application/json`, UTF-8 `text/plain`, or `application/octet-stream`). To send a JSON string rather than plain text, write `body: "Ada", bodyFormat: "json"`. Scalars such as numbers require an explicit `bodyFormat`. An explicitly supplied `Content-Type` describes the payload but does not change its serialization; JSON bodies require a JSON-compatible media type. An HTTP call can still use its legacy option-block form, including `json:`, while files migrate. `json` and `body` are mutually exclusive, as are `json` and `bodyFormat`. A response that declares a JSON media type but contains malformed JSON fails with a clear protocol error. The response size limit is enforced from `Content-Length` when available and while streaming the body.

External data does not have to use Mettle identifier names. Use a quoted or computed string key after brackets for HTTP headers or JSON properties containing punctuation:

```mettle
requestId = response.headers["x-request-id"]
displayName = response.json["display-name"]
firstRole = response.json.roles[0]
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

Use `echo(value)` as a standalone statement in a flow or test when a value should
appear in its execution report. It accepts one value, including strings with
interpolation or structured objects. It does not change the flow's return value.
Like other statements, its argument is evaluated in every output mode; for
example, `echo(http.get(...))` still makes the request with `--quiet` or `--raw`.

```mettle
flow greet(name) {
    echo("Greeting ${name}")
    return "Hello, ${name}!"
}

flow main = parallel(limit: 2) { greet("Ada"), greet("Lin") }
```

Messages from parallel branches carry logical labels such as `[p1:b2/2]` for
unnamed branches and `[p1:users]` for named ones. Nested branches retain their full path;
retry attempts similarly use labels such as `[r2:a1/3]`. These are execution
identifiers, not OS thread IDs. Branch numbers follow source order, while event
order reflects what actually completed or emitted first. Messages from a
cancelled branch remain in the report, and pending sibling branches are marked
cancelled when another branch fails.

`echo` appears in normal and verbose human reports, including failures. Quiet
and raw modes keep their existing minimal output. JSON reports carry ordered,
redacted `events` inside each atomic flow or test record. In workload iterations,
the CLI retains at most 50 messages per run and reports how many were omitted,
keeping load-test output bounded. Messages are collected until the top-level
flow finishes; `--all` therefore keeps each flow's output together. There is no
separate `debug()` or debug mode yet. Try the network-free
[echo example](examples/language/echo.mettle) with `mettle run examples/language/echo.mettle`.

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
mettle run examples/http/requests.mettle inspectRequest --arg baseUrl=https://jsonplaceholder.typicode.com --arg requestId=1 --verbose
mettle run examples/http/requests.mettle inspectRequest --arg baseUrl=https://jsonplaceholder.typicode.com --arg requestId=1 --output json
```

Rate and concurrency workloads use an in-place dashboard when stderr is attached to a terminal. It prioritizes up to three active workloads and counts any others hidden from the live view. Each shows its execution phase, active iterations, achieved rate, outcomes, dropped starts, and latency percentiles. HTTP calls are aggregated by source call site, with counts, status classes, and p95 latency. The final human and JSON reports retain up to 32 workloads and 32 action sites per workload, explicitly counting any excess. Only a few failure categories are retained, without URLs, bodies, or headers. Redirected output and machine-readable modes remain deterministic. Use `--no-progress` to disable the live dashboard explicitly.

```text
Mettle · 2 workloads
browsing · rate 2/1s for 2s · RUNNING
  1.00s/2s · active 1/4 · 3.0/s · ok 2 · fail 0 · drop 0
  iterations 3/2 · p50 47ms · p95 155ms · p99 155ms
    browseUser · GET L12 · 2 calls · p95 135ms
    browseUser · GET L15 · 2 calls · p95 26ms
posts · rate 3/1s for 4s · RUNNING
  1.00s/4s · active 1/6 · 4.0/s · ok 3 · fail 0 · drop 0
  iterations 4/3 · p50 24ms · p95 116ms · p99 116ms
    readPost · GET L21 · 3 calls · p95 118ms
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
code --install-extension dist/mettle-language-1.0.0-alpha.1.vsix --force
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
examples/                  Curated language and HTTP examples; start with examples/README.md
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
- request bodies support JSON, UTF-8 text, and runtime bytes values; multipart forms and streaming bodies are not implemented
- no workload ramping or distributed workers yet; operation metrics are local and source-site aggregated, not distributed traces
- the SIP capability and external capability distribution model are still planned work
- no custom CA bundles, client certificates, or mutual TLS
- no prebuilt Windows, macOS, or Linux release archives yet
