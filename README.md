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
            name: seed.body.name
            active: true
            roles: ["tester"]
        })

    assert(created.status == 201)
    created.body
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

The server reports diagnostics for unsaved edits. Go to Implementation navigates from named flow calls and local binding references to their declarations. Go to Definition also resolves parameters and named contexts.

To run flows and tests with your existing vim-test shortcuts and output strategy,
load the optional [Neovim vim-test adapter](util/plugin/neovim/README.md). It
supports nearest execution, all tests or eligible flows in a file, and vim-test's
last-run and return-to-source commands.

For syntax highlighting, the repository also includes a
[Tree-sitter grammar](util/plugin/tree-sitter/README.md).
It provides highlighting, folding, and indentation queries and works alongside
the language server. The [Neovim plugin setup](util/plugin/neovim/README.md#tree-sitter)
registers the grammar and shared queries for Neovim 0.11 with the
`nvim-treesitter` plugin's `master` branch.

### VS Code extension

The repository includes the Mettle Language extension for syntax highlighting,
parser/compiler diagnostics, flow and test CodeLens actions, Go to Definition, and Go to Implementation for flow calls and local binding references. Build and install its VSIX from
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

The Mettle compiler, runtime, HTTP capability, CLI, and VS Code extension support Linux x64, Windows x64, and macOS on Apple Silicon. Native CI builds and tests these platforms. Intel macOS is not a supported target. The portable acceptance suite executes real HTTP workflows and a local load test on each supported platform.

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

Run every zero-argument flow with `--all`. Parameterized flows are deliberately
skipped, so this is useful for a collection of self-contained checks. The
default `--jobs 1` executes entries sequentially in source order unless the
project sets a different default in `mettle.toml`. Set a larger
job count to run independent entries concurrently:

```bash
mettle run checks.mettle --all
mettle run checks.mettle --all --jobs 4
```

At most `N` entries run at once. Each has isolated bindings, context, events,
and workload metrics. Mettle continues after a failed entry and returns a
nonzero status if any entry fails. With concurrent jobs, completed entries are
printed in completion order and their headers retain the original source index.
Human output remains atomic per entry and ends with a batch summary.
`--output json` emits JSON Lines: one `start` record, one atomic `result` or
`failure` record per completed entry, and one `summary`. Each entry record has
a one-based `sourceIndex`, so consumers can restore declaration order.
Concurrent human runs announce the entry count and job limit before execution.
Failure headers and diagnostics stay together on stderr; successful reports
are written to stdout. Ctrl+C retains completed results and ends a batch with
an interrupted summary separating cancelled entries from those not started.
`--raw` is intentionally rejected when `--jobs` is greater than one because
unlabelled, completion-ordered values would be ambiguous.

## Write executable tests

Declare checks with `test "name" { ... }`. The older `test("name")` spelling
also works. Tests have no parameters or return
value. Use `assert(condition, "message")` to explain a failure; the message is
optional and may interpolate values. A test collects false assertions and reports
each one with its source location, then continues to the next test. A runtime
error stops the current test but preserves any earlier assertion failures.
Assertions inside ordinary flows still fail immediately. `mettle test <file>`
runs tests declared in that file sequentially unless `[test].jobs` configures concurrency;
`mettle test <file> "test name"` or `mettle test <file> --line <line>` runs one test;
`mettle run <file> --all` still runs only zero-argument flows. Use
`mettle test <file> --jobs 4` to run independent file tests concurrently;
selected individual tests do not accept `--jobs`. The test command exits
nonzero when any test fails or the file has no tests, and supports `--verbose`,
`--quiet`, and `--output json` (JSON Lines) for CI. Keep the default
`--jobs 1` when tests depend on shared external state or intentional ordering.

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
    assert(response.body.id == 1, "post 1 should have ID 1")
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
            sourceId: seed.body.id
            name: seed.body.name
            active: true
            roles: ["tester"]
        })

    assert(created.status == 201)
    created.body
}
```

This is the complete shape used by the [HTTP workflow example](examples/http/workflow.mettle):

```bash
mettle run examples/http/workflow.mettle
```

An HTTP response exposes `status`, `headers`, `body`, `bodyBytes`, `mediaType`, `method`, `url`, and `duration`. `body` is the decoded native value, not necessarily an object. `mediaType` is the normalized Content-Type string with explicit parameters, or `null` when absent. HTTP status codes are ordinary values, so assertions make the expected condition obvious. Header names containing punctuation use string-key access, such as `response.headers["content-type"]`.

Incoming JSON and `+json` decode into native objects, arrays, strings, numbers,
booleans, or null. `text/*` decodes into a strict UTF-8 string. Missing or unknown
content types remain bytes, without content sniffing. `bodyBytes` always retains
the bounded representation bytes; the original Content-Type stays in `headers`.
JSON is a representation, not a language value kind or a guarantee that fields exist.

HEAD and statuses 204/205/304 have `body: null`. Other empty text/binary bodies
remain `""`/empty bytes; empty declared JSON fails. Malformed or duplicate
Content-Type, invalid JSON/UTF-8, unsupported JSON/text charsets, excessive
nesting/numbers, and response limits fail with useful source locations. Non-identity
Content-Encoding is rejected for responses with a body; compression is separate
future work. HTTP error statuses are returned when their content is valid.

**Breaking response migration:** `.json` has been removed; replace
`response.json.name` with `response.body.name`. To obtain text independently of
the declared type, use `text.decode(response.bodyBytes)`; to explicitly decode JSON,
use `json.decode(response.bodyBytes)`. Known HTTP-result `.json` accesses get a
compiler migration diagnostic, including aliases and simple helper-flow results.
This does not reserve `json` as an ordinary object field.

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

`parallel` with unnamed branches returns an array in source order; named branches return an object keyed by their labels. Without `limit`, all branches may start; with it, no more than `limit` start at once. If a branch fails, active siblings are cancelled and joined. `retry` counts the first execution as an attempt and reruns its entire block—including assertions—until it succeeds or exhausts its attempts; terminal `fail(...)` bypasses retry. `within` covers all nested work, including retry delays. Ctrl+C cancels and joins every active top-level job, stops admitting queued entries, and exits with status 130.

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

The manifest also supports separate default job counts for flow and test batches:

```toml
name = "service-checks"
version = "0.1.0"

[test]
jobs = 4

[run]
jobs = 2
```

`[test].jobs` applies to `mettle test <file>`; `[run].jobs` applies to
`mettle run <file> --all`. Explicit `--jobs N` overrides the project default,
and omitted settings default to `1`. Selecting a single flow or test ignores
the batch default and still runs only that entry. Defaults come from the nearest
project root, independently of the shell's working directory; nested projects
do not inherit settings from a parent project.

Job counts must be positive integers. Malformed TOML, unknown keys, and invalid
values are reported with the manifest path and source location. Supported root
keys are `name`, `version`, `run`, and `test`; each execution table currently
accepts only `jobs`. Profiles and output modes remain explicit invocation
options. In projects configured for concurrent flow batches, use
`--jobs 1 --raw` when you want sequential raw output.

The editor's **Run All** and **Run Tests in File** actions use these defaults
automatically. Try the network-free project checks:

```bash
mettle run examples/language/project/checks.mettle --all
mettle test examples/language/project/checks.mettle
mettle test examples/language/project/checks.mettle --jobs 1
```

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

The implemented capabilities include HTTP and filesystem I/O. Filesystem byte
sources compose with HTTP request bodies through the shared capability interface.

### Read, copy, and send files

```mettle
message = fs.readText("./message.txt")
payload = fs.read("./small.bin")
copied = fs.write("./copy.bin", fs.stream("./large.bin"))
response = http.post("http://127.0.0.1:8080/upload", body: fs.stream("./large.bin"))
```

| Operation | Result | Default `maxBytes` |
| --- | --- | --- |
| `fs.read(path)` | Complete, reusable bytes | 10 MiB |
| `fs.readText(path)` | Complete, reusable UTF-8 string; invalid UTF-8 fails | 10 MiB |
| `fs.stream(path)` | Lazy, single-use byte source | 1 GiB |
| `fs.write(path, content)` | `{ bytesWritten: ... }` after writing string, bytes, or source | 1 GiB |

Each accepts a positive integer `maxBytes` and positive duration `timeout`
(default 30 seconds). Stream lifetime starts when consumption begins and includes
time between reads. Complete reads fail when their bound is exceeded. Streams
are pulled in chunks of at most 64 KiB without collecting the entire file.
Each execution entry permits up to 64 open source readers and 64 filesystem
worker operations; waiting for capacity is included in operation deadlines.

Standalone paths start at the directory where you invoke `mettle`. When a
project is discovered, paths start at its root, including paths in helper flows.
Set a different directory using a top-level manifest field:

```toml
name = "upload-demo"
workingDir = "./data"
```

Relative `workingDir` values start at the project root; the directory must exist.
This does not change environment-profile discovery. VS Code launches standalone
files from their containing directory; project execution follows the same root
and configuration rules as the CLI.

`fs.write` refuses existing destinations unless `overwrite: true` is supplied.
It writes a temporary sibling and publishes only the complete output. Failure
or cancellation cleans incomplete output before the execution finishes, retaining
the previous destination. Publication is the commit point: cancellation after
publication does not undo a completed write. Parent directories must exist.
Writes are not a durability guarantee. File reads follow symlinks to regular
files; writes reject final destination symlinks. Directories and special-file
sources are rejected. These rules do not confine paths to the working directory.

Complete values can be reused. Aliases of a stream share its consumption state:
consuming the same source twice, including in parallel, fails. Create another
`fs.stream(path)` for another read. An unopened source can pass through helper
flows within its execution; it cannot be returned as the final execution result.
Fresh reads can observe changed files. Limits are checked during reads as well
as against initial file metadata.

HTTP `body: fs.stream(path)` sends representation bytes through ordinary calls,
using automatic HTTP/1.1 framing. Do not supply `Content-Length` or
`Transfer-Encoding` for a streamed body. Bytes/sources infer
`application/octet-stream`; an explicit `Content-Type` can describe an already
encoded file. Sources transmit existing bytes; selecting a media type does not
serialize them again as JSON or text.
If an endpoint replies early, production stops and the normal call returns its
actual bounded response. Source failures retain their original diagnostics.
Streams are not automatically replayed; explicit retry must construct a new
source inside each attempt. Request retries may duplicate external side effects.

File operation reports retain counts and source descriptions, rather than
capturing file contents. Returning or explicitly echoing a complete value still
outputs it with the normal sensitivity/redaction rules.

Try the [filesystem project](examples/language/filesystem/main.mettle) and
[local upload example](examples/http/file-upload.mettle). Progressive response
consumption and custom codecs remain future phases. Ordinary HTTP responses are
complete and bounded, with decoded `.body` values.

### Content codecs and value kinds

Codecs describe representation bytes across I/O capabilities, not HTTP-only
classes. Built-ins expose ordinary strings as `json.mediaType`
(`application/json`), `text.mediaType` (`text/plain; charset=utf-8`), and
`bytes.mediaType` (`application/octet-stream`). Media-type strings also work.

```mettle
encoded = json.encode({ celsius: 23 })
value = json.decode(encoded)
assert(value is object)
temperature = "23" as number
assert(temperature is number)
message = text.decode(text.encode(temperature))
response = http.post("/temperature", body: value, mediaType: json.mediaType)
```

`json.encode` returns bytes; `json.decode` accepts complete bytes or a string and
returns a native value (object, array, scalar, or null). `text.encode` converts a
string into UTF-8 bytes without quotes. Numbers, booleans, null, arrays, and
objects can also be explicitly formatted with `text.encode`: these use compact
JSON spelling. `text.decode` returns a strict UTF-8 string. `bytes.encode` and
`bytes.decode` are identity operations on complete bytes. Values are reusable;
these operations never consume a live source implicitly. Each accepts positive
`maxBytes` (default 10 MiB); serialization stops at the output bound rather than
first building an unlimited buffer. JSON encoding/decoding permits at most 127
nested containers. Excessive nesting, invalid UTF-8,
non-finite numbers, and integers outside the signed 64-bit range fail clearly.
`defaults json`, `defaults text`, and `defaults bytes` can set `maxBytes`.

`is` checks the actual built-in kind without conversion; `as` converts explicitly.
Kinds are `null`, `boolean`, `integer`, `number`, `string`, `bytes`, `duration`,
`array`, `object`, and `source`. Both integers and decimals are `number`; an
integral decimal is not automatically `integer`.

| Conversion | Rule |
| --- | --- |
| Same kind | Preserve the value; source aliases still share one consumer |
| String → number | Trim outer whitespace and parse Mettle's decimal, exponent, hex, or binary literal syntax |
| String → integer | Parse integer literal syntax directly; no floating-point rounding |
| Decimal → integer | Require an integral value in signed 64-bit range; never truncate |
| String → boolean | Only `true` or `false`, with optional outer whitespace |
| String → duration | Parse a duration literal, such as `"1.5s"` |
| Null, boolean, number, duration → string | Deterministic spelling; duration uses nanoseconds |
| Other cross-kind conversions | Error; use explicit codecs for JSON or UTF-8 content |

Conversions parse at most 4096 bytes. No truthiness, object-to-string cast,
byte-to-string cast, user classes, or overloaded encoders are introduced.
`as number` preserves an integer rather than rounding it to floating point.
Kind operations associate left-to-right and bind before unary `not`/`-`,
comparisons, `and`, and `or`; member/index access binds first. For example,
`not value is number` means `not (value is number)`. Parenthesize unary expressions
when converting their result: `(-value) as string`.
Checks, conversions, codecs, and subsequent boolean/numeric operations preserve
secret metadata. Conversion and codec error messages describe failures without
embedding the supplied value; normal source-location excerpts still appear.

Codec reports show value kinds instead of capturing payloads. Return or echo an
ordinary result explicitly when it should be displayed. Try the
[network-free content example](examples/language/content.mettle) and
[local HTTP representation example](examples/http/content.mettle). The
[incoming content example](examples/http/incoming-content.mettle) shows native
body kinds, metadata, bodyless responses, and explicit raw-byte decoding.

Incoming `.body` values use the built-in decoders today. User-defined codecs,
compression, scoped exchanges, and live response iteration are later work;
raw `.bodyBytes` remain explicitly available.

### Capability interfaces

The core parser understands calls, values, flows, contexts, and execution policies. It does not need a special grammar rule for each protocol verb. The compiler resolves a qualified call such as `http.get()`, `sip.options()`, `grpc.call()`, or `kafka.publish()` through a registered capability.

A capability contributes:

- named operations, constants, and their signatures;
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
| `body` | JSON-compatible value, string, bytes, or byte source | Request payload for `post`, `put`, `patch`, or `delete` |
| `mediaType` | String or codec media-type constant | Select representation encoding and generate `Content-Type` |
| `maxBodyBytes` | Positive integer | Total outgoing payload bound; default 10 MiB buffered, 1 GiB streamed |
| `json` | JSON-compatible native value | Legacy payload option; prefer `body` in new files |

The URL is HTTP's only positional parameter and may instead be written as `url: "/users"`. Named arguments can follow positional ones, in any order; after the first named argument, no positional argument is allowed. User-defined flows follow the same positional-then-named rule. Duplicate or unknown names fail during `mettle check`, and argument expressions run once in their written order.

Objects, arrays, numbers, booleans, and an explicitly supplied null in `body`
default to JSON; strings default to UTF-8 text; bytes/sources default to raw bytes.
An omitted body does not imply JSON null or a default content type.
To send a JSON string instead of plain text, write
`body: "Ada", mediaType: json.mediaType`. A header-only `Content-Type` now selects
the encoder too, when no legacy `json:` payload override is present.
JSON-compatible `+json` types reuse the JSON codec; `text/*` uses UTF-8 text.
An illustrative name such as `application/vnd.example.temperature+json` labels
generic JSON; it does not register a temperature codec or validate domain fields.
Unknown representations require pre-encoded bytes or a source, not a guessed
encoder. Built-in encoding supports UTF-8 only; send explicitly encoded bytes
for other charsets. There is no filename-based inference or automatic compression.

Bytes and sources are already encoded and are **never encoded again**, even when
`mediaType` is JSON. For example,
`http.post("/users", body: json.encode(user), mediaType: json.mediaType)` sends
the encoded JSON unchanged. Stream bounds are checked during production; file
source bounds and deadlines apply as well.

Supplying both `mediaType` and `Content-Type` requires equivalent parsed media
types. Type/subtype and parameter names are case-insensitive; quoting and
parameter order do not cause conflicts; UTF-8 is the built-in default charset.
Other parameter values retain their case. Duplicate parameters, wildcard types,
malformed metadata, duplicate `Content-Type` headers, or conflicting selectors
fail before a source is consumed. Metadata is limited to 8 KiB and 32 unique
parameters, with ASCII parameter syntax in this initial implementation.

`bodyFormat` has been removed: use `body` with optional `mediaType` instead.
For example, replace `body: "Hello!", bodyFormat: "json"` with
`body: "Hello!", mediaType: json.mediaType`. For another representation, encode
explicitly first and send the resulting bytes with their media type.
Legacy `json:` payloads remain supported and cannot be combined with `body`;
their declared media type must be JSON-compatible.
Compared with earlier versions, scalar bodies now infer JSON, header-only
content types can select encoding, and buffered outgoing payloads have a bound.
Check calls that previously used a deliberately mismatched header.
A response declaring malformed JSON still fails clearly. Its size limit is
enforced from `Content-Length` when available and during body acquisition.

External data does not have to use Mettle identifier names. Use a quoted or computed string key after brackets for HTTP headers or JSON properties containing punctuation:

```mettle
requestId = response.headers["x-request-id"]
displayName = response.body["display-name"]
firstRole = response.body.roles[0]
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
flow finishes; `--all --jobs N` therefore keeps each completed flow's output
together even when several entries execute concurrently. `--jobs` schedules
independent top-level entries; the language-level `parallel` expression owns
cooperating branches inside one entry. There is no
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

Binary bodies use a bounded 64-byte hexadecimal preview even with `--verbose`;
`--raw` and `--output json` retain the complete byte representation.

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

## Code documentation

Language and built-in capability references are available offline from the CLI:

```bash
mettle docs http.post
mettle docs fs.stream
mettle docs json.encode
mettle docs assert
mettle docs use
mettle docs language
```

Signatures, option types, conflicts, and result fields come from the same
capability schemas used by the compiler. Default values use shared runtime
constants; descriptions and examples are authored beside the implementation.
There is no separate editor API catalogue to maintain.
Capability result records own both object construction and documented field
schemas, so field names are not copied into a second result catalogue.
Operation hovers use a compact signature (`…` stands for optional named options),
behavior notes, and a small example. Signature help and the full reference keep
the exhaustive option list, defaults, and result fields.

Every reserved word also has hover documentation, including `assert`, `use`,
`flow`, and execution policies such as `parallel`. Core helpers (`env`, `senv`,
`secret`, `echo`) and parameterized keywords show individual argument descriptions
on hover as well as in signature help and the full reference; primitive kinds
have references when used with `is` or `as`. Keyword spellings/documentation share
the lexer inventory, and helper descriptions share compiler intrinsic resolution.
Use `language.<name>` to disambiguate a primitive from a capability, for example
`mettle docs language.bytes` versus `mettle docs bytes`.

Document your own flows or named contexts with contiguous `///` comments
immediately above the declaration:

```mettle
/// Build a greeting.
/// @param name Name to include in the greeting.
/// @returns A greeting string.
flow greet(name) = "Hello, ${name}!"
```

VS Code shows these descriptions on hover and during signature help, including
named arguments, cross-file references, and unsaved edits. Unknown or duplicate
`@param` names produce editor warnings, not runtime failures. Blank physical lines
or ordinary comments detach the documentation block; `///` can separate paragraphs.
See the [runnable documentation example](examples/language/documentation.mettle).

Binding and field hovers show compiler-inferred native kinds and known shapes,
including aliases, context values, reusable flow results, and HTTP response
envelopes. `response.status` is an integer; `response.body` is runtime-dependent,
not a JSON type or a guaranteed object. Parameter kinds remain caller-dependent.
No runtime I/O or environment values are inspected, and bound contents are never
shown. Statically sensitive values are marked; unknown sensitivity stays unknown.
Inference is bounded and conservative: branch results retain common fields,
large shapes get a limited preview, and complex/invalid expressions may remain
unknown. Finished statements can still have useful hovers during incomplete edits.
See the [offline variable example](examples/language/variable-intelligence.mettle).

## VS Code extension

The included extension provides `.mettle` recognition, syntax highlighting, snippets, folding, parser/compiler diagnostics for unsaved edits, CodeLens play buttons to run individual flows and tests, and Ctrl+Click navigation for flows, contexts, parameters, and local bindings. Hover and signature help describe language keywords/helpers, built-in operations/options, and documented user declarations. F12 or **Open full reference** on a built-in or keyword opens its read-only, version-matched reference. These features use `mettle lsp`, following the same project and namespace rules as the CLI.

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

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, validation, and commit conventions.
Instructions for coding agents are in [AGENTS.md](AGENTS.md).

```text
crates/mettle-syntax              Lexer, parser, AST, and source spans
crates/mettle-capability          Capability schemas, values, and runtime interface
crates/mettle-compiler            Resolution, validation, and execution-plan lowering
crates/mettle-runtime             Async execution-plan interpreter and context scopes
crates/mettle-http                HTTP schema, pooled client, JSON, timeouts, and TLS
crates/mettle-fs                  Complete file I/O, owned sources, and safe publication
crates/mettle-cli                 Native command-line interface and diagnostics
examples/                         Curated language and HTTP examples; start with examples/README.md
tests/fixtures/                   Deterministic HTTP programs and local TLS material
tests/projects/                   Multi-file project fixtures
util/plugin/vscode/               Installable VS Code extension
util/plugin/neovim/               Neovim Tree-sitter setup and vim-test adapter
util/plugin/tree-sitter/          Tree-sitter editor parser, queries, and grammar tests
util/test-server/                 Local HTTP and HTTPS acceptance fixture
docs/                             Temporary design notes and supporting reports
```

The files under `docs/`, including the [language proposal](docs/language-proposal.md)
and [technical strategy](docs/mettle-technical.md), are temporary supporting notes
and may be stale. Current behavior is reflected in the implementation, tests,
examples, and this README. Maintained documentation on GitHub, potentially a wiki,
is planned for beta. The generated [licence report](docs/third-party-licenses.md)
records the locked third-party Rust packages.

The larger Rust crates keep their public API in `lib.rs` and separate parsing,
declaration navigation, semantic lowering, execution, and HTTP schemas into
focused source modules.

## Current limits

- no redirects or proxy discovery
- request bodies support JSON, UTF-8 text, bytes, and filesystem byte sources; multipart forms and manual streaming exchanges are not implemented
- no workload ramping or distributed workers yet; operation metrics are local and source-site aggregated, not distributed traces
- the SIP capability and external capability distribution model are still planned work
- no custom CA bundles, client certificates, or mutual TLS
- no prebuilt Windows, macOS, or Linux release archives yet
