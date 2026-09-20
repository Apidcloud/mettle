# An I/O Oriented Programming Language

Technical proposal for engineering discussion · September 2026

## Purpose and scope

We want to explore a small programming language for concurrent I/O workflows: making API requests, describing protocol interactions, running functional and performance tests, and eventually coordinating work across machines. The same flow should work for a manual check, a larger workflow, and a load test. Moving between these uses should primarily change the surrounding execution policy.

The central idea is that the language and runtime understand I/O, time, concurrency, cancellation, and measurement directly. A protocol operation such as `GET` is observable; a `flow` gives one or more operations a reusable name; a `rate` block is a scheduling policy; and a `within` block establishes a deadline. These constructs should compose without requiring users to assemble futures, executors, timers, and metric collectors themselves.

This is an evolving design, not a finished language specification. **Current direction** below means a design choice carried forward from the discussion. **Proposed semantics** identifies rules that make the examples precise but still require team agreement. **Future exploration** identifies features outside the initial implementation. `Mettle`, `.mettle`, and the `flow` command are working names only. Reusable and anonymous flows, direct CLI flow selection and arguments, contexts, environment interpolation, HTTP defaults, GET/POST operations, JSON payloads, response member access, durations, HTTPS, deadlines, retries, and bounded parallel execution are implemented; the root README is the authority for the exact executable subset. Other examples remain proposed syntax.

## Goals and design principles

- **Make small tasks small.** A single HTTP request should remain readable enough to serve the same everyday role as a `.http` file or an API-client collection.
- **Reuse flows across workloads.** A flow should not need rewriting when called sequentially, concurrently, or under load.
- **Make concurrency explicit and suspension implicit.** Ordinary statements execute in order. I/O suspends the current lightweight task; `parallel` explicitly introduces concurrent work. An I/O-oriented language need not add `async` and `await` to every request.
- **Represent execution policy structurally.** Rate, deadlines, retries, and task lifetime belong to scopes the runtime can inspect and enforce.
- **Separate data from language constructs.** `name: value` introduces a field. `use context`, `defaults http`, and `rate` introduce language behavior.
- **Keep configuration composable and bounded.** Contexts combine reusable values and capability defaults without becoming mutable process-wide state.
- **Make measurements explicit.** Assertions refer to returned results such as `load.latency.p95`, rather than whichever workload last updated an ambient metric.
- **Keep project organization lightweight.** Files organize declarations; namespaces control visibility without path-based import/export lists.

The initial target is a local runtime with useful HTTP workflows and trustworthy scheduling and measurement. Distributed execution, protocol extensions, streams, and machine orchestration remain part of the broader direction. General-purpose application development, infrastructure provisioning, and durable workflow recovery are not initial requirements.

## Component model

The language should have an explicit component model before it gains more syntax. This separates saved declarations from operators that control their execution and gives the compiler, runtime, test runner, and editor a shared vocabulary.

| Component | Purpose |
| --- | --- |
| **Project** | Root for source discovery, extensions, environment defaults, metadata, and tests |
| **Namespace** | Logical grouping and visibility of declarations across physical files |
| **Context** | Reusable values and capability-specific defaults |
| **Mettle** | Named reusable function containing one or more I/O operations or calls to other flows |
| **Test** | Instrumented verification scope with assertions, reporting, and pass/fail semantics |
| **Dataset** | Reusable or externally loaded iteration input |
| **Operation result** | Result of one primitive protocol operation |
| **Execution result** | Aggregate result returned by an execution primitive such as `rate` |
| **Capability** | Runtime extension that defines operations, schemas, result types, and protocol behavior |

Execution primitives such as `parallel`, `rate`, `retry`, `repeat`, `race`, and `within` are operators over work. They are not independently saved project components. Features such as `env()`, `json`, `assert`, `sleep`, `defaults`, `use context`, and `use namespace` are language forms used inside the components above.

The conceptual hierarchy is:

```text
project
├── global declarations
├── namespaces
│   ├── contexts
│   └── flows
│       ├── primitive I/O operations
│       ├── calls to other flows
│       └── execution primitives
├── tests
│   ├── flows and primitive operations
│   ├── execution primitives
│   └── assertions and lifecycle
└── datasets

orthogonal runtime concepts
├── capabilities: http, sip, tls, ...
├── execution: parallel, race, retry, rate, repeat, within, ...
└── values: primitives, arrays, objects, datasets, and results
```

There should be no `collection` keyword. The role of a saved API collection decomposes naturally into project, namespace, context, flow, test, and dataset. A directory of source code is already the collection boundary, while each language component has one clear responsibility.

## Syntax and compiler model

The surface language should reuse a small number of grammatical forms. Declarations introduce names, calls invoke functions or capability operations, trailing blocks provide executable work or structured options, and every field inside a data or configuration object uses `name: value`.

| Form | Meaning |
| --- | --- |
| `name: value` | A field in a data object or schema-defined structure |
| `auth: { token: value }` | An arbitrary nested data object |
| `context api { ... }` | A reusable context declaration |
| `use context api` | Compose a context or apply it to an execution scope |
| `defaults http { ... }` | Defaults validated against the HTTP capability schema |
| `namespace users` | Namespace containing this file's declarations |
| `use namespace users` | Make a namespace's declarations available here |
| `flow getUser(id) { ... }` | A reusable function containing one or more I/O operations |
| `flow health() = http.get("/health")` | Expression-bodied reusable flow |
| `http.get("/health")` at file level | Independently runnable anonymous flow |
| `test("user lookup") { ... }` | An instrumented verification scope |
| `data users = csv("users.csv")` | A dataset declaration, proposed syntax |
| `response = getUser(42)` | Bind a flow's result |
| `response = http.get("/users")` | Invoke a capability operation |
| `json: { ... }` | Assign a structured JSON payload field |
| `rate(target: 100, period: 1s, duration: 10s) { ... }` | Execute a trailing block under a rate policy |

Context fields do not use `let`. They describe structured data, so they use the same field syntax as payloads. Identifiers use `camelCase` by default. Executable bindings use `=` in the examples; mutability, reassignment, and the complete type system still need a specification.

At file level, a call expression is an anonymous entry flow rather than module initialization. The compiler wraps it in an implicit return and assigns it a source identity. Multiple top-level calls remain independently runnable. A multi-operation anonymous flow uses `flow { ... }`, while named flows remain callable from other flows. `main` is an optional conventional default rather than a required declaration.

The CLI may select named flows by name and anonymous flows by compiler-reported source line. If no selection is supplied, `main` wins; otherwise a source containing exactly one flow is unambiguous. Declared parameters are supplied as named CLI arguments and validated before execution.

The parser can be organized around a compact grammar resembling:

```text
declaration       := namespaceDecl | contextDecl | flowDecl | anonymousCall | dataDecl
flowDecl          := "flow" identifier parameterList (statementBlock | "=" expression)
                   | "flow" statementBlock
anonymousCall     := callExpression
contextMember     := useContext | defaultsBlock | objectField
defaultsBlock     := "defaults" qualifiedName objectLiteral
callExpression    := qualifiedName "(" arguments? ")" trailingBlock?
qualifiedName     := identifier ("." identifier)*
namedArgument     := identifier ":" expression
objectLiteral     := "{" objectField* "}"
objectField       := (identifier | string) ":" expression
trailingBlock     := "{" statement* "}"
```

Capability operations therefore use ordinary qualified calls such as `http.get(...)` and `sip.options(...)`; HTTP verbs and SIP methods do not require dedicated parser productions. Operation option blocks contain ordinary fields such as `timeout: 5s`, `headers: { ... }`, and `json: { ... }`. Capability-provided typed values use the same call and trailing-block form, such as `bearer() { ... }` and `sdp() { ... }` on the right-hand side of a field.

Duration literals such as `200ms` and `5s` are typed scalar values. Rate configuration deliberately avoids a special `10k/s` token: `target: 10_000` and `period: 1s` are separate named arguments. This is more explicit about iteration starts, avoids ambiguity with division, and keeps numeric suffixes out of the lexer.

### Compiler consequences

The parser does not need to know that `get` is an HTTP verb or `options` is a SIP method. It produces the same call-expression node for `http.get(...)`, `sip.options(...)`, `rate(...)`, and ordinary user functions. Later compiler phases provide the domain semantics:

1. Name resolution binds `http` or `sip` to a registered capability and resolves the operation name.
2. Signature checking validates positional and named arguments.
3. The trailing block is checked against the callable's expected block type. An HTTP operation expects schema-defined option fields; `rate` expects executable statements.
4. Constructor calls such as `sdp() { ... }` validate their trailing block against a nested schema and produce a typed value.
5. Lowering emits a capability invocation or execution-policy node in the intermediate representation while preserving source locations and field origins.

This keeps protocol extensibility outside the core parser. Adding `grpc.call(...)` or `kafka.publish(...)` requires a capability definition, type schemas, and runtime implementation rather than new syntax and lexer rules.

Blocks use line breaks as member or statement separators; call arguments and array elements use commas. This should be a deliberate lexical rule, not automatic recovery. A formatter can therefore produce one stable representation, and the parser never has to guess whether adjacent fields belong to one expression.

## Contexts and capability defaults

A context is a composable container for user-defined values and defaults for registered runtime capabilities. It is not a fixed schema with an optional HTTP property.

```text
context base {
    apiUrl: env("API_URL")
    apiToken: env("API_TOKEN")

    client: {
        name: "engineering-tools"
    }
}

context api {
    use context base

    defaults http {
        baseUrl: apiUrl
        timeout: 5s

        headers: {
            "Accept": "application/json"
            "Authorization": "Bearer ${apiToken}"
            "X-Client": client.name
        }
    }
}
```

`apiUrl`, `apiToken`, and `client` are ordinary context data. By contrast, fields inside `defaults http` are validated against the HTTP capability's schema. An unknown field such as `banana: 42` is valid user data in a context but should be rejected as an HTTP default. Capability schemas should prefer descriptive camelCase names such as `baseUrl`, `followRedirects`, and `maxConnections` rather than generic names such as `base`.

The keyword `defaults` expresses the override model: an operation may replace a default locally.

```text
flow slowReport() {
    use context api

    response = http.get("/reports") {
        timeout: 30s
    }

    return response
}
```

Here the operation uses the API base URL and headers, but its timeout is `30s`. Earlier names such as `configure`, and bare `http { ... }` context blocks, are superseded by this explicit form.

Capability names and user values occupy separate symbol spaces. Consequently, `http: "some data"` and `defaults http { ... }` can be grammatically unambiguous in the same context. Tooling may warn about confusing names without reserving every name that a future extension might introduce. Defaults for SIP or another capability follow the same language form, with their own schemas.

### Extension and composition

Use the existing composition mechanism rather than introducing a separate inheritance operator.

```text
context identified {
    defaults http {
        headers: {
            "X-Test-Suite": "api-regression"
        }
    }
}

context slowApi {
    use context api
    use context identified

    defaults http {
        timeout: 30s
    }
}
```

The intended ordering is: compose contexts in their listed order, then apply locally declared fields. Later composed contexts override earlier conflicting defaults, and local declarations override both. Composition must not mutate the original contexts. Cycles such as `a → b → a` should produce a diagnostic with the dependency chain.

**Proposed semantics:** merge capability fields by schema, retaining unrelated fields. Merge HTTP headers by case-insensitive name, so replacing `Authorization` does not discard `Accept`. For arbitrary user objects, a conservative first rule is replacement of the conflicting field as a whole; recursive object merging, collection merging, repeated headers, and explicit removal require separate decisions. Reject duplicate local declarations rather than silently depending on their textual order.

**Proposed evaluation model:** compose value definitions before evaluating expressions against the resulting immutable context. This lets a derived context override `apiUrl` and have an inherited `baseUrl: apiUrl` resolve consistently. Detect unresolved names and expression cycles. Do not accidentally freeze derived defaults at declaration time. Evaluation frequency must be explicit: a `uuid()` in a context could mean one value per activation, not one per operation. Put per-operation IDs in the operation until this contract is settled.

### Environment variables

`env("API_URL")` obtains a value from the runtime environment. The context name `base` is appropriate when the same variable names are supplied locally, in CI, and on remote workers. There is no need for a separate language-level environment declaration merely to read environment variables.

Inside string interpolation, `${name}` first resolves a flow parameter/local and then an active context field. When no value exists and the interpolation is a simple name rather than a member path, the runtime reads the process environment variable with that name. This supports concise request collections such as `http.get("${API_URL}/health")`; `env("API_URL")` remains the explicit form for ordinary expressions and context fields.

```dotenv
API_URL=http://localhost:8080
API_TOKEN=development-token
```

The runner may support `.env` files and explicit environment-file selection. A proposed precedence is base `.env`, then the selected environment file, then the actual process environment. Exact discovery and command-line syntax are runner decisions, not properties of `env()` itself. Missing required variables should fail clearly before work starts, and values should remain strings unless explicitly converted.

Environment selection is not required to live outside source code: named contexts can still represent intentional environment-specific settings. The common case should work with a shared `base` context and externally supplied values, without duplicating every request for local and staging environments.

Value retrieval and sensitivity are separate concerns. `env()` does not inherently promise redaction. A future sensitive-value wrapper could mark a value obtained from any source; it should not replace `env()` with a misleading secret-store lookup. The runtime should avoid recording authorization headers by default and define redaction before adding rich payload logging.

## Context lifetime and flow boundaries

The current direction is to apply contexts inside existing owners: context declarations, flows, tests, or supported execution blocks. Avoid persistent, free-floating `use context` at file level. Namespace directives remain valid at file level because they affect name resolution rather than runtime configuration.

```text
flow getUser(id) {
    use context api

    response = http.get("/users/${id}")
    return response
}

test("health checks") {
    use context api

    parallel() {
        http.get("/health")
        http.get("/ready")
    }
}
```

Contexts end with their owner. A flow's local context must not leak back into its caller or into sibling tasks. This removes the normal need for `clear context`, and avoids an extra `with` block used only to carry configuration.

**Proposed call semantics:** a child task or flow invocation receives a snapshot of the caller's active context. Contexts declared within the callee overlay that snapshot for the callee's lifetime; operation-local fields win last. Context directives should appear in a scope's preamble, before executable statements, so they configure the owner rather than changing an ambient state halfway through it.

This produces the following precedence:

```text
runtime defaults
    → caller context snapshot
    → callee context composition
    → operation-local fields
```

This rule needs an explicit team decision. A caller selecting a `30s` timeout would not override a callee that reapplies `api` with a `5s` timeout. Intrinsic flow contexts improve local readability, but ambient caller configuration must not become an undocumented override mechanism. Mettles needing variability should expose an argument or a documented context dependency. Tooling should show the effective configuration and the origin of each field.

Immutable context snapshots provide task isolation; they do not prohibit safe connection-pool reuse underneath the runtime. Resource ownership, connection reuse, cookies, authentication sessions, and SIP dialogs need their own lifetimes rather than being treated as mutable context fields.

## Projects and namespaces

A project is the boundary for source discovery, namespace indexing, extensions, default environment loading, test discovery, and optional metadata. A single `.mettle` file must still run without a manifest:

```text
http.get("https://example.com")
```

When a directory needs project-level behavior, it may add a small optional `mettle.toml`:

```toml
name = "payments-api"
version = "0.1"

extensions = ["http", "sip"]
```

The manifest format and fields are proposals. The important rule is that project machinery remains optional for the one-file case. If no manifest exists, the runner can treat the selected file as a standalone program or discover a project using conservative directory rules. A manifest establishes an unambiguous project root when namespace aggregation, extensions, environment files, or test discovery require one.

Directories organize physical files; namespaces organize language symbols. Files without a namespace declaration belong to an implicit global namespace. Small projects can place shared declarations there with no import boilerplate. Larger projects opt into named namespaces:

```text
namespace users
use namespace core
```

Multiple files may contribute to `users`. `use namespace users` makes those declarations available without naming files or listing exported symbols. Its dependencies are resolved transitively, but loading a namespace does not execute its files or activate its contexts.

Namespaces are not executable groups. They contain declarations only. A runnable grouping belongs in a flow or test; a directory is not made executable merely because its files share a namespace.

**Proposed resolution rules:** the compiler indexes declarations inside a defined project root, resolves global declarations and explicitly used namespaces, and reports ambiguous unqualified names. Imports in one namespace satisfy that namespace's dependencies; they do not silently re-export all dependency names to callers. Duplicate declarations within one namespace are errors. Qualification such as `users.get(...)` is a possible later escape hatch, not a requirement for the examples here.

The runner executes only the selected entry point. Other files contribute declarations; top-level executable statements in other entry scripts must not run during discovery. Test discovery should likewise be explicit. Namespace visibility must never imply runtime side effects.

### A complete HTTP project

```text
api-project/
├── mettle.toml
├── core.mettle
├── flows/
│   └── users.mettle
├── data/
│   └── user-ids.csv
├── main.mettle
├── tests/
│   └── users-load.mettle
└── .env
```

`core.mettle` defines configuration:

```text
namespace core

context base {
    apiUrl: env("API_URL")
    apiToken: env("API_TOKEN")
}

context api {
    use context base

    defaults http {
        baseUrl: apiUrl
        timeout: 5s
        headers: {
            "Accept": "application/json"
            "Authorization": "Bearer ${apiToken}"
        }
    }
}
```

`flows/users.mettle` defines reusable flows:

```text
namespace users
use namespace core

flow getUser(id) {
    use context api

    response = http.get("/users/${id}")
    return response
}

flow createUser(user) {
    use context api

    response = http.post("/users") {
        json: {
            name: user.name
            email: user.email
            active: true
            roles: ["tester"]
            address: {
                city: "Porto"
                country: "Portugal"
            }
        }
    }

    return response
}
```

`main.mettle` belongs to the implicit global namespace and runs a functional check:

```text
use namespace users

created = createUser({
    name: "João"
    email: "joao@example.com"
})
assert(created.status == 201)

user = getUser(created.json.id)
assert(user.status == 200)
assert(user.json.name == "João")
print(user.json)
```

`tests/users-load.mettle` reuses the same flow:

```text
use namespace users

data userIds = csv("data/user-ids.csv")

test("user lookup") {
    load = rate(target: 1_000, period: 1s, duration: 10s) {
        user = userIds.next()
        getUser(user.id)
    }

    assert(load.latency.p95 < 200ms)
    assert(load.errors < 1%)
}
```

This load example selects IDs from known test data. Randomly generating identifiers would change the error distribution being measured unless missing users are intentionally part of the workload.

Illustrative runner commands are `mettle run main.mettle` and `mettle run tests/users-load.mettle`. There are no `import` or `export` declarations, and loading `users` does not itself perform I/O.

## Mettles and tests

`flow` is the single reusable executable component. It may wrap one primitive protocol operation, contain a multi-step workflow, or call other flows. This deliberately avoids separate `request` and `scenario` declaration kinds:

```text
flow getUser(id) {
    use context api

    response = http.get("/users/${id}")
    return response
}
```

`http.get("/users/${id}")` is the atomic operation supplied by the HTTP capability. `getUser(id)` is a language-level flow with a stable name, arguments, result, context, and instrumentation identity. The same model applies to SIP and future capabilities.

Atomicity belongs to the primitive operation, not to the flow declaration. Tooling should display the runtime tree of nested flows and operations instead of trying to classify a flow permanently as a "request" or a "scenario." If a future feature needs to constrain a flow to one operation, that constraint can be introduced explicitly without splitting the core reusable abstraction.

A flow can grow without changing its kind:

```text
flow checkout(user) {
    session = login(user)
    products = listProducts()

    order = createOrder(
        session.userId,
        random(products.json.items)
    )

    return order
}

checkout(testUser)

load = rate(target: 1_000, period: 1s, duration: 30s) {
    checkout(users.next())
}
```

Calling a flow does not imply assertion collection, a CI exit status, or a test report. Those belong to `test`. Explicit `return` is preferred in the examples because it remains clear when a flow expands from one operation to several. The language may later permit an implicit final-expression return as shorthand.

### Driving a flow from an operation result

Operation results are normal values. A flow can bind the result of a `GET`, inspect it, select data from its decoded body, and use that data in a later operation:

```text
flow orderFirstAvailableProduct(userId) {
    use context api

    catalog = http.get("/products?availability=in-stock")

    if catalog.status != 200 {
        fail("could not load the product catalog")
    }

    if catalog.json.items.length == 0 {
        fail("no products are currently available")
    }

    product = catalog.json.items[0]

    order = http.post("/orders") {
        json: {
            userId: userId
            productId: product.id
            quantity: 1
        }
    }

    return order
}
```

The `GET` result drives both control flow and the payload of the following `POST`. `fail(...)`, indexing, and collection properties such as `length` are proposed core-value semantics; the important design point is that operation results are ordinary typed values rather than data trapped inside a protocol client.

### Tests

A test is an instrumented verification scope. It collects assertions and measurements, produces a named report entry, and contributes to the runner's exit status.

```text
test("create user") {
    response = createUser(testUser)

    assert(response.status == 201)
    assert(response.json.id != null)
}

test("user lookup load") {
    load = rate(target: 5_000, period: 1s, duration: 30s) {
        user = users.next()
        getUser(user.id)
    }

    assert(load.latency.p95 < 200ms)
    assert(load.errors < 1%)
}
```

Tests may call flows or primitive operations and may contain execution primitives. Whether assertions fail immediately or are collected until the test ends is a test-runner decision that must be consistent; collecting independent assertion failures usually produces more useful diagnostics, while a failed prerequisite may need an explicit fatal assertion.

## Mettles and first class JSON

A `flow` receives arguments, performs I/O or calls other flows, and returns a value the caller can inspect. A flow may contain one operation or many. Explicit returns, inferred return types, failure values, and flows with no result still need a precise specification.

HTTP results should expose status, headers, body access, and timing information. `response.json` denotes decoded JSON; malformed JSON or incompatible content must produce a defined error, not an unexplained null value. Transport failure, a received error status, and an assertion failure are distinct outcomes even if a workload policy counts all of them as failures.

`json: value` is a schema-defined HTTP operation field, not a separate statement form or a raw JSON string. An object literal supports nested objects, arrays, booleans, numbers, null, and references to program values. Source syntax may omit JSON's quoted identifier keys and commas; serialization still emits valid JSON and escapes strings correctly. Language identifiers default to camelCase, while payload keys may follow the external API's contract when that contract uses another convention. The HTTP capability should infer `Content-Type: application/json` and diagnose incompatible explicit settings.

```text
flow createOrder(userId, productId) {
    use context api

    response = http.post("/orders") {
        headers: {
            "X-Trace-ID": uuid()
        }
        json: {
            userId: userId
            product: {
                id: productId
                quantity: 1
            }
            metadata: {
                source: "integration-test"
                gift: false
            }
        }
    }

    return response
}
```

An existing value can also be supplied as the entire payload:

```text
http.post("/orders") {
    json: order
}
```

The capability accepts `json: value` whether the value is an inline object literal or an existing expression. It validates that the value is JSON-serializable and uses the same serializer in both cases.

Here `uuid()` is evaluated for the operation, making the trace identifier's lifetime clear. URL interpolation needs separate escaping rules from JSON serialization; the examples use numeric path IDs to avoid implying that arbitrary strings are safe path components.

## Datasets and test input

Load examples should not depend on undefined helpers such as `randomUser()`. Datasets are a project component that can supply deterministic functional-test input and repeated load-test input.

Proposed declarations for the first version are:

```text
data users = load(file: "users.csv", type: "csv")
data products = load(file: "products.json")
```

If not defined, `type` should be automatically inferred by the file extension.

They can be iterated normally:

```text
for user in users {
    createUser(user)
}
```

or consumed by a workload:

```text
load = rate(target: 1_000, period: 1s, duration: 30s) {
    createUser(users.next())
}
```

The declaration syntax is not yet final, but the behavioral contract must cover schema inference or declaration, CSV conversion rules, JSON shape, empty input, malformed rows, and path resolution relative to the project. `users.next()` also needs an explicit exhaustion policy: stop, fail, cycle, or partition. Cycling is convenient for sustained load but must be visible rather than implicit. Distributed execution will need deterministic partitioning so workers do not accidentally reuse the same rows unless requested.

Generated datasets may later support forms such as `data users = generate(count: 10_000) { ... }`. Fake-data providers are tooling or extension concerns and are not required for the first dataset implementation.

## Test lifecycle and scoped cleanup

Integration tests need setup and cleanup, but generic pre-request and post-response hooks would introduce behavior that is difficult to see at the call site. The language should first support test lifecycle and scoped resources.

One candidate syntax is:

```text
test("create order") {
    before() {
        user = createTestUser()
    }

    run() {
        response = createOrder(user.id, 42)
        assert(response.status == 201)
    }

    after() {
        deleteUser(user.id)
    }
}
```

This syntax remains open. Regardless of surface form, cleanup must run after success, assertion failure, timeout, or cancellation, with its own bounded deadline. Setup failure should skip the main body while still cleaning up resources that were successfully created. Values created during setup need a clear scope, and cleanup failures must be reported without hiding the original failure.

A later scoped-resource form may be a better general abstraction for temporary users, sockets, remote processes, and machines. Broad `before operation` or `after operation` interceptors should not be added initially. Simple per-operation behavior, such as generating a trace ID, remains visible in capability defaults or the operation itself.

## Authentication and sensitive values

Headers can express bearer authentication, but protocol-aware authentication affects challenges, retries, connection reuse, and redaction. It belongs in capability schemas rather than in a single cross-protocol global construct.

```text
context authenticatedApi {
    defaults http {
        auth: bearer() {
            token: secret(env("API_TOKEN"))
        }
    }
}

context authenticatedSip {
    defaults sip {
        auth: digest() {
            username: env("SIP_USERNAME")
            password: secret(env("SIP_PASSWORD"))
        }
    }
}
```

These forms are proposals. HTTP bearer, HTTP basic, and SIP digest do not share the same exchange and should be implemented by their capabilities. `env()` identifies where a value comes from; `secret(value)` marks any value as sensitive. Sensitive taint should propagate through interpolation and derived values and prevent disclosure in CLI output, traces, reports, errors, and recorded request data unless the user explicitly opts into an unsafe diagnostic mode.

## Documentation, examples, and test selection

Doc comments are sufficient initial metadata for declarations:

```text
/// Fetches a user by numeric identifier.
/// Returns 404 when the user does not exist.
flow getUser(id) {
    response = http.get("/users/${id}")
    return response
}
```

Editors and documentation generators can consume these comments later. Saved flow examples and fixtures may also become documentation or mock-server assets, but they do not need a runtime keyword until a concrete workflow justifies one.

Large projects need test discovery and filtering. Tags are therefore a product requirement even though their syntax is open. A possible form is:

```text
test("create user", tags: ["smoke", "users"]) {
    response = createUser(testUser)
    assert(response.status == 201)
}
```

This could support commands such as `flow test --tag smoke`. The project manifest should define test roots or discovery patterns when conventions are insufficient. Namespace membership and directory placement should not double as implicit test tags.

## Execution policies

### Parallel work and concurrency limits

`parallel` starts child work concurrently and joins it before the enclosing scope proceeds. The runtime retains the parent-child task relationship for deadlines, cancellation, traces, and metrics.

In the implemented grammar, `within` and `retry` each govern one child expression, while `parallel` accepts one or more expression branches. Policy expressions can be nested, bound, or returned. Branches do not declare shared mutable locals.

```text
test("service readiness") {
    use context api

    within(timeout: 3s) {
        parallel() {
            http.get("/health")
            http.get("/ready")
        }
    }
}
```

**Current failure policy:** a failed child cancels its active siblings, joins their cleanup, and reports the failure. Successful results are returned as an array in source order. An eventual collect-all policy could support independent checks, but should be explicit. A received HTTP status is still a value; transport failures and failed assertions are errors.

`parallel(limit: 20) { ... }` provides bounded fan-out. The implemented limit and branch count are positive compile-time integer literals, with at most 1,024 branches. Omitting `limit` admits all statically declared branches. A separate `concurrency(limit: 100) { ... }` workload would express a fixed number of active iterations or users, whereas `rate(target: 100, period: 1s) { ... }` expresses how frequently new iterations start. The exact duration control for sustained concurrency workloads remains open.

### Rate is an arrival policy

```text
load = rate(target: 100, period: 1s, duration: 30s) {
    getUser(42)
}
```

The proposed interpretation is 100 **iteration starts** per one-second period for 30 seconds. If each iteration issues two sequential HTTP operations, the target is still 100 iterations per second, not 100 HTTP requests per second. The target is requested scheduling behavior, not a throughput guarantee.

A rate workload requires sufficient concurrency to sustain its arrival target while I/O is pending. A fixed-concurrency workload instead slows its completion rate as response times grow. Rate and concurrency limits must therefore remain separate controls.

The runtime needs a bounded overload policy: report delayed or dropped starts rather than allowing unbounded queues or silently redefining the requested workload. Measure achieved rate and scheduling delay. At the end of the scheduling window, stop launching iterations and drain active work under a documented deadline. Report drain time separately from the arrival window. Exact saturation and draining defaults remain design decisions.

Ramping can remain inside the same call grammar, for example `rate(target: ramp(from: 100, to: 1_000, over: 30s), period: 1s, duration: 1m) { ... }`. The exact ramp value type is a future extension.

### Retries and deadlines

```text
flow resilientLookup(id) {
    use context api

    response = within(timeout: 2s) {
        retry(attempts: 3) {
            http.get("/users/${id}")
        }
    }

    return response
}
```

**Current semantics:** `retry(attempts: 3)` permits at most three total attempts. `delay: 50ms` optionally adds a fixed delay between failed attempts. Attempts and delay are positive compile-time literals. The first success is returned; exhaustion reports the final error with the attempt count. The outer deadline covers all attempts and delays. An inner operation timeout cannot extend the outer deadline, and expiry cancels and joins pending child work.

The current explicit retry block retries any runtime error produced by its single child expression. Predicate filtering, exponential backoff, and jitter remain to be specified. Retrying side-effecting operations may duplicate effects; the block does not claim exactly-once execution. Protocol retransmission, especially in SIP, must also be distinguished from replaying the whole application operation.

## Operation and execution results

Results are a formal part of the language model. There are two main categories.

An **operation result** describes one primitive protocol operation. Common fields may include status, headers, raw body access, decoded payload access, duration, and an error value. Capabilities add protocol-specific information such as HTTP redirects or SIP transaction and dialog identifiers. The common result interface must not pretend that every protocol has an HTTP status code.

An **execution result** describes work governed by an execution primitive. A `rate` result contains aggregate iteration, operation, timing, scheduling, and error measurements. Other primitives may return different result shapes; for example, `parallel` needs a defined way to retain each child result.

Execution blocks return explicit result objects. No test-global `latency` variable is implicitly overwritten by the latest workload.

```text
test("lookup comparison") {
    baseline = rate(target: 100, period: 1s, duration: 10s) {
        getUser(42)
    }

    load = rate(target: 1_000, period: 1s, duration: 10s) {
        getUser(42)
    }

    assert(baseline.latency.p95 < 100ms)
    assert(load.latency.p95 < 200ms)
    assert(load.errors < 1%)
}
```

`load` is a normal binding in the enclosing test. Its result is finalized when the workload, including its defined drain phase, completes. Subsequent workloads do not change it.

The following is a **proposed result contract**, extending the explicit-result direction:

| Field | Proposed meaning |
| --- | --- |
| `load.latency.p95` | 95th percentile of iteration elapsed time from actual start to terminal outcome |
| `load.errors` | Failed iteration count divided by completed iteration count; comparable to `1%` |
| `load.count` | Number of completed workload iterations |
| `load.attempts` | Number of protocol attempts, including application retry attempts |
| `load.success` | Number of successful iterations |
| `load.rate.target` | Requested iteration starts per configured period |
| `load.rate.period` | Period over which the target number of starts is scheduled |
| `load.rate.actual` | Actual iteration starts per configured period during the scheduling window |
| `load.duration` | Total workload elapsed time, including draining |

Multi-operation flows make the distinction important. Iteration latency includes the complete flow, including sequential operations, nested flow calls, parallel joins, and retry delays. It should not silently become a percentile over unrelated individual operations. Because flows have stable names, detailed measurements can be grouped by flow declaration:

```text
load.flows.login.latency.p95
load.flows.listProducts.latency.p95
load.flows.createOrder.errors
```

The exact field API remains proposed. Name aggregation must define what happens when the same flow is called more than once per iteration, from several parent flows, or from two namespaces with the same unqualified name. A stable fully qualified flow identity can back the metrics while reports display a short name when it is unambiguous. Primitive operations inside a flow also need source-derived identities or optional explicit labels so a multi-operation flow can be diagnosed below the flow level.

Record timeouts, cancellations, dropped starts, scheduling delay, and attempt counts separately. Define the default success policy explicitly—for example, how HTTP status codes and assertions affect `load.errors`. Empty samples should yield an unavailable percentile and a diagnostic assertion failure, not a misleading zero. Histogram accuracy and the treatment of failed samples must be documented. Distributed percentiles must eventually be computed from mergeable distributions, not by averaging worker percentiles.

## SIP as a second capability

SIP is a useful test of the abstraction because it adds transactions, provisional responses, dialogs, retransmission, and cleanup to the simpler request-response case. Contexts and defaults should remain general enough to support it without making SIP state look like HTTP state.

```text
context sipClient {
    domain: env("SIP_DOMAIN")
    callerUri: env("SIP_CALLER_URI")
    contactUri: env("SIP_CONTACT_URI")

    defaults sip {
        transport: udp
        timeout: 3s
        from: callerUri
        contact: contactUri
        headers: {
            "User-Agent": "flow-prototype/0.1"
        }
    }
}

flow probeSip() {
    use context sipClient

    response = sip.options("sip:${domain}") {
        to: "sip:${domain}"
    }

    return response
}

test("SIP availability") {
    response = probeSip()
    assert(response.status == 200)

    load = rate(target: 50, period: 1s, duration: 10s) {
        probeSip()
    }
    assert(load.latency.p95 < 500ms)
}
```

The SIP schema, transport enum, and operation result types are proposals. The adapter should own transaction identifiers and protocol bookkeeping, with explicit inspection or override facilities where protocol testing requires them.

### Future dialog and SDP syntax

The discussion also explored typed SDP and dialog handles:

```text
flow basicCall() {
    use context sipClient

    call = sip.invite("sip:bob@example.com") {
        body: sdp() {
            connection: "192.0.2.10"
            media: audio() {
                port: 49170
                codec: opus() {
                    payload: 111
                    clock: 48000
                    channels: 2
                }
            }
        }
    }

    within(timeout: 5s) {
        call.response(200)
    }
    sip.ack(call)
    sleep(5s)
    sip.bye(call)
}
```

This is a lifecycle sketch, not a complete SIP implementation contract. `call.response(200)` is shown as implicitly suspending, consistent with the core direction; earlier sketches used explicit `await`. The adapter must define provisional and final response delivery, rejection handling, transaction timers, cancellation before acceptance, and cleanup after acceptance. Exceptional exits must not leave calls behind. Typed SDP must supply or validate required session fields, and does not itself imply RTP/media generation. A raw-body escape hatch may remain useful for protocol tests.

## Runtime and implementation direction

Start with a parser and interpreter or compact intermediate representation rather than an optimizing native compiler:

```text
source files
    → project and namespace index
    → parser and syntax tree
    → name resolution and schema validation
    → execution representation
    → task scheduler and protocol adapters
    → I/O, timers, cancellation, and metrics
```

Protocol operations should remain visible to the runtime rather than becoming opaque user-library calls. The execution representation can carry source locations, effective context, deadlines, and measurement ownership. Adapters implement protocols; the scheduler enforces execution policy around them.

A JVM prototype using lightweight tasks or an asynchronous networking stack is one plausible direction; a native runtime is another. The first decision should follow semantic and measurement needs, not an assumption that native compilation is necessary. Whichever host is chosen, blocking adapter work must not stall unrelated tasks.

An incremental implementation could proceed as follows:

1. **Values, calls, and results.** Define strings, numbers, booleans, null, percentages, arrays, objects, durations, errors, and result types. Parse qualified calls, named arguments, typed objects, and trailing blocks, then execute a single `http.get(...)` operation.
2. **Mettles.** Add named flows, stable instrumentation identities, arguments, explicit return semantics, nested flow calls, and reusable multi-operation workflows.
3. **Project and configuration model.** Add optional project manifests, namespace indexing, global declarations, `env()`, contexts, capability schemas, composition, and useful diagnostics.
4. **Datasets and test lifecycle.** Add CSV and JSON inputs, deterministic iteration, test setup, bounded cleanup, assertion collection, discovery, and CI status.
5. **Structured execution.** Add task scopes, parallel joining, deadline propagation, cancellation, retries, and bounded resource cleanup.
6. **Measured workloads.** Add rate scheduling, concurrency bounds, execution results, per-flow and per-operation metric aggregation, histograms, and explicit threshold assertions. Validate target versus achieved load under saturation.
7. **Authentication and protocol breadth.** Add capability-aware authentication and sensitive-value handling, then implement SIP transactions before dialog orchestration and richer SDP support.

Distributed workers, remote command execution, synchronization barriers, streams with backpressure, periodic execution, races, and scoped resource acquisition can follow once the local contracts are stable. They should extend the same task and result model. Distribution introduces additional decisions about aggregate versus per-worker rates, clock coordination, failure recovery, credential delivery, and metric merging; none should be implied by a local `rate` block alone.

## Decisions to resolve before implementation

| Area | Question to settle |
| --- | --- |
| Project model | How is the project root found without a manifest, and which `mettle.toml` fields are part of the first version? |
| Context evaluation | When are expressions evaluated, and can a context intentionally depend on caller-provided values? |
| Context precedence | Do callee contexts always override caller defaults, and how does a caller intentionally customize a reusable flow? |
| Composition | How are nested objects, repeated protocol headers, removal, and collisions handled? |
| Namespaces | What defines the project root, entry points, visibility, and qualification rules? |
| Values and returns | Which bindings are mutable, how are types checked, and how do flows and policy blocks return values? |
| Datasets | How are schemas, conversions, exhaustion, cycling, shuffling, and distributed partitioning defined? |
| Test lifecycle | What setup and cleanup syntax guarantees bounded cleanup across failures, timeouts, and cancellation? |
| Task failures | Which outcomes cancel siblings, and how are multiple failures reported? |
| Load scheduling | What happens when arrival targets exceed concurrency or machine capacity? |
| Measurement | Which samples contribute to latency and error ratios, how are nested flow and operation identities aggregated, and how are empty or cancelled runs represented? |
| Authentication | Which auth schemes belong in each capability, and how does sensitive-value taint propagate? |
| Capability API | How does an extension register qualified operations, signatures, trailing-block schemas, result types, compiler lowering, and runtime handlers? |
| Test discovery | How are tests, tags, filters, and entry points selected without making namespaces executable? |
| SIP ownership | Which operations return transaction or dialog handles, and who performs cleanup? |

The immediate engineering objective is a small end-to-end prototype: run a standalone HTTP operation, place it in a reusable flow, compose that flow into a multi-operation flow and a test, feed it a small dataset, run it at a controlled rate, and inspect aggregate, per-flow, and per-operation results. A second thin slice should prove that setup resources are cleaned up after success, assertion failure, and timeout. Together these exercise the structural model while keeping distributed execution and deeper protocol features out of the first implementation.

## Elevator pitch

A small language and runtime for describing, composing and executing I/O workloads — from a single API request to distributed performance tests and multi-system orchestration.
