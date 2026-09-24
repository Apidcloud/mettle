# Mettle Technical Strategy

Status: exploratory implementation proposal and delivery plan
Initial platform: Linux
Initial capability: HTTP

Current implementation: compiler foundation, VS Code editor integration with definition navigation, minimal HTTP vertical slice, request collections, and multi-file project composition are complete. The root README is the authority for executable behavior.

## 1. Purpose

Mettle should become a small language and high-performance runtime for functional I/O workflows and load tests. A user should be able to describe an HTTP interaction once, run it as an ordinary workflow, and then place the same flow under concurrency or rate policies without rewriting its operations.

This document defines how to build that system. The language direction is described separately in [`language-proposal.md`](language-proposal.md). Syntax remains open to refinement, but the runtime foundations must be reliable enough that later syntax does not hide unsafe scheduling, unbounded memory growth, or inaccurate measurements.

The implementation starts on Linux and must preserve a credible path to Windows and macOS. It initially supports HTTP only. SIP and user-provided protocol capabilities remain architectural goals rather than MVP features.

## 2. Main technical decisions

### 2.1 Rust

Rust is a good fit because Mettle needs predictable memory use, low overhead, native binaries, explicit ownership, and safe concurrency. Rust also supports Linux, Windows, and macOS from one codebase.

Rust does not make the runtime automatically fast. Performance still depends on allocation patterns, connection reuse, task scheduling, synchronization, metrics, and workload shape. Every performance claim must be backed by a reproducible benchmark on documented hardware.

The project pins its Rust toolchain and forbids `unsafe` code across the workspace. An exception may be proposed later only with a measured benefit, a documented safety argument, focused tests, and review. No current component needs one.

### 2.2 Compile to a validated execution plan, then interpret it

The initial toolchain is:

```text
.mettle source
    → lexer and parser
    → AST with source spans
    → name resolution and type/schema checking
    → validated execution plan
    → native Rust runtime interpreter
    → capability operations
```

This is an interpreter in deployment terms, but it does meaningful compilation before execution. The runtime does not repeatedly parse text or resolve names. It consumes a compact plan containing resolved flow and local-slot identifiers.

This approach gives quick iteration on language semantics, useful diagnostics, and deterministic validation before a load test starts. Native-code generation would increase complexity while most execution time is expected to be spent waiting for or processing I/O. Plan serialization or ahead-of-time packaging can be added later without replacing the front end. Plans contain every named and anonymous flow; entry selection is a CLI/runtime input rather than a compiler assumption that `main` always exists.

### 2.3 A small core with compiled capabilities

The parser owns general language forms: declarations, calls, values, bindings, blocks, and execution policies. It should not contain a grammar production for every HTTP verb or future protocol operation.

Capabilities provide:

- callable operation signatures;
- configuration and payload schemas;
- result types;
- compile-time validation hooks;
- runtime execution;
- metrics emitted by the capability;
- redaction rules for sensitive protocol data.

The first release compiles capabilities into the binary. A developer may add another protocol by implementing a Rust capability crate and rebuilding Mettle. Dynamic native plugins and a stable binary ABI are deferred. Keeping the internal capability contract independent from Rust dynamic loading leaves room for a future plugin process, WebAssembly boundary, or versioned protocol.

### 2.4 Structured concurrency

Every unit of concurrent work must have an owner and a bounded lifetime. The runtime will model cancellation as a tree rooted at a command invocation:

```text
run
└── test or flow scope
    ├── rate/concurrency controller
    │   ├── iteration
    │   └── iteration
    └── metrics collector
```

When a parent ends or is cancelled, it stops admitting new work, propagates cancellation to its children, waits for their cleanup within a bounded shutdown period, and finalizes metrics. Detached background tasks are prohibited in the language runtime.

Queues and concurrency limits must be bounded from their first introduction. Backpressure behavior is part of execution semantics; it cannot be replaced safely at the end after users have depended on an unbounded prototype.

### 2.5 Portability boundary

Mettle code, the compiler, scheduling policies, and capability interfaces remain platform-independent. Tokio is planned as the asynchronous runtime boundary. It maps its portable APIs to platform facilities such as `epoll` on Linux, `kqueue` on macOS/BSD, and Windows I/O mechanisms.

Linux is the primary development environment. Native CI verifies Linux x64, Windows x64, Apple Silicon macOS, and Intel macOS. Platform-specific optimizations must remain behind internal interfaces and include a portable fallback. Experimental facilities such as `io_uring` are not part of the initial architecture.

## 3. Workspace architecture

The workspace is divided by responsibility rather than by deployment process:

| Crate | Responsibility |
| --- | --- |
| `mettle-syntax` | Lexer, parser, syntax tree, source spans |
| `mettle-compiler` | Name resolution, type/schema checks, capability validation, lowering |
| `mettle-runtime` | Plan interpreter, scopes, scheduling, cancellation, resource ownership |
| `mettle-capability` | Stable internal capability contracts and shared result types |
| `mettle-http` | HTTP defaults, operations, pooling, TLS, response values, HTTP metrics |
| `mettle-cli` | Commands, project discovery, diagnostics, output and exit behavior |

All six components exist in the current implementation. Splitting the workspace does not create runtime processes or dynamic-link boundaries. Cargo statically links the required code into one native executable.

Dependencies must point inward:

```text
mettle-cli ───────────────┐
mettle-http → capability  │
             ↓          │
mettle-runtime → compiler │
                 ↓      │
              syntax ←──┘
```

The compiler can validate against capability descriptors without depending on an HTTP client implementation. The runtime invokes capabilities through internal interfaces without teaching the scheduler HTTP-specific behavior.

## 4. Runtime model

### 4.1 Values and contexts

Values should use compact, immutable representations. Primitive values can be stored inline; strings, arrays, objects, and JSON may use shared immutable storage where profiling shows a benefit. Context activation produces an immutable snapshot. Child flows receive a snapshot and may overlay it without mutating their caller.

Capability defaults are resolved before hot execution where possible. For example, header names can be normalized and static URLs parsed once. Operation-local options produce a derived configuration without changing the active context.

The CLI builds one immutable environment snapshot before `run` or `test`, layering project-root `.env`, entry-directory `.env`, selected `.env.<profile>` files, and finally process variables. `env()` and interpolation read that snapshot during execution. `senv("NAME")` is the sensitive shorthand for `secret(env("NAME"))`. Missing required values fail before their dependent work runs. Sensitive values need metadata separate from their source so diagnostics and request capture can redact credentials consistently.

### 4.2 Execution plans

The plan should eventually contain:

- resolved declaration and local-slot IDs;
- inferred value types;
- validated capability operation IDs;
- normalized constant values and durations;
- compiled context composition order;
- explicit policy nodes for `within`, `retry`, `parallel`, and `rate`;
- source spans for every user-visible failure;
- metric descriptors known before execution.

Plans are immutable and shareable across worker tasks. Runtime state, including connections and iteration values, lives outside the plan.

### 4.3 HTTP

The first HTTP capability supports a deliberately focused surface:

- HTTP and HTTPS URLs;
- `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, and `OPTIONS` operations;
- base URL, timeout, headers, JSON request bodies, and UTF-8 text request bodies;
- connection reuse;
- response status, headers, bounded body bytes/text, parsed JSON, and operation duration;
- an explicit certificate-verification setting for local test systems.

Proposed configuration:

```mettle
context local {
    defaults http {
        baseUrl: "https://localhost:8443"
        timeout: 5s

        tls: {
            verifyCertificates: false
        }
    }
}
```

Certificate and hostname verification remains enabled by default. Disabling it must be conspicuous in source and diagnostics. Custom CA bundles, mutual TLS, certificate pinning, client certificates, and fine-grained TLS version policy belong to the final refinement work.

Connection pools belong to an execution runtime or HTTP client scope, not to immutable context data. Pool sizes and pending acquisition queues must be bounded. Cancellation must release permits and response bodies correctly so one failed flow cannot slowly exhaust the pool.

### 4.4 Rate and concurrency scheduling

Rate and concurrency are different controls:

- **Rate** controls how quickly iterations begin.
- **Concurrency** bounds how many iterations may be active.

The scheduler must define what happens when the desired rate exceeds available concurrency. The planned default is bounded admission with an explicit missed-start or saturation measurement; it must not create an unlimited queue of late iterations.

The clock abstraction should use a monotonic clock. Tests need a controllable clock so timing behavior is deterministic without sleeping for wall-clock durations.

### 4.5 Retry and deadlines

`within` creates a deadline inherited by child work. Operation timeouts cannot extend beyond the active parent deadline. Retry consumes the same deadline unless the language explicitly creates a new scope.

Retries need explicit predicates and delay policy. HTTP methods are not assumed safe to retry merely because a transport failed. Retry attempts emit separate operation metrics while the enclosing flow retains one logical outcome.

### 4.6 Metrics

Measurement points must be designed before concurrency is added, although full reporting arrives later. Hot-path metric recording should avoid a global lock and unbounded event storage. Workers can accumulate local counters and latency distributions that are merged at controlled intervals or shutdown.

The runtime should distinguish at least:

- requested, admitted, started, completed, cancelled, and rejected iterations;
- operation transport errors, timeouts, protocol outcomes, and assertion failures;
- end-to-end flow latency and individual operation latency;
- active work, queue pressure, and connection-pool pressure;
- scheduler lateness and missed starts.

A statement such as `load = rate(...) { ... }` returns a scoped result. Assertions such as `assert load.latency.p95 < 200ms` read that result rather than ambient global metrics.

Latency and scheduling delay use an internal fixed-size logarithmic histogram with 64 sub-buckets per power-of-two range. Each workload owns a bounded 4,097-counter distribution regardless of iteration count, with exact minimum, maximum, count, and sum alongside approximate percentiles. This avoids a metrics dependency and unbounded sample retention. The approximation and bucket layout remain internal until the report contract is stabilized.

### 4.7 Diagnostics

The lexer records byte spans immediately and every compiler and plan node preserves a source origin. User-facing errors should include filename, line, column, relevant source text, and a concrete explanation. Runtime failures should add a Mettle call stack and operation identity while redacting sensitive data.

Errors intended for automation need stable categories and optional machine-readable output. Human wording may improve without forcing CI systems to parse prose.

### 4.8 Editor tooling

The repository contains a VS Code extension in `util/plugin/vscode`. It provides `.mettle` file recognition, TextMate syntax highlighting, editor configuration, snippets, Run Mettle actions, and definition navigation.

Editor intelligence is compiler-backed rather than an independent JavaScript implementation of the language. The `mettle lsp` process exposes the Language Server Protocol over standard input/output and reuses project discovery, syntax trees, namespace visibility, and source spans from the Rust implementation. The VS Code extension contains a small dependency-free client that starts the server and maps LSP locations into VS Code. Full-document synchronization lets definition lookup use unsaved editor text.

The remaining language-server sequence is:

1. Publish compiler diagnostics while a file is edited.
2. Complete keywords, declarations, context names, capability operations, and schema fields.
3. Provide hover information and effective capability configuration.
4. Add document symbols, references, and safe rename. Definition navigation is already available.
5. Add formatting, semantic tokens, inlay hints, and code actions where they have clear value.

TextMate highlighting remains useful as an immediate tokenizer and fallback even after semantic tokens exist. Grammar and snippets must be updated with each syntax milestone. Editor packages and the CLI must advertise compatible language versions once the language begins versioning.

## 5. Dependency and licence policy

Dependencies are accepted for difficult infrastructure where mature implementations improve correctness or security. Convenience alone is insufficient in performance-sensitive runtime paths.

The expected networking foundation is:

| Crate | Purpose | Upstream licence |
| --- | --- | --- |
| [Tokio](https://github.com/tokio-rs/tokio/blob/master/LICENSE) | Async tasks, timers, sockets, synchronization, OS event-loop abstraction | MIT |
| [Hyper](https://github.com/hyperium/hyper/blob/master/LICENSE) | HTTP protocol and client machinery | MIT |
| [Rustls](https://github.com/rustls/rustls#license) | TLS implementation for HTTPS | Apache-2.0 OR MIT OR ISC |

Small integration crates required by their supported APIs are reviewed with the same standard. The compiler foundation remains implemented without third-party parser or compiler libraries. The current locked graph and automated licence allowlist are documented in [`dependencies.md`](dependencies.md) and [`third-party-licenses.md`](third-party-licenses.md).

Project policy:

1. Minimize direct dependencies and disable unused default features.
2. Commit `Cargo.lock` for reproducible application builds.
3. Review the full transitive graph, not only direct crates.
4. Allow only explicitly approved licences; reject unknown, GPL, and AGPL packages unless separately reviewed and accepted.
5. Check advisories, source provenance, duplicate versions, and licence metadata in CI.
6. Produce third-party notices and a software bill of materials for releases.
7. Record the reason, enabled features, and removal conditions for each runtime dependency.
8. Measure binary-size and runtime effects when adding dependencies to hot paths.

The repository's own distribution licence remains undecided and is therefore marked `UNLICENSED` in package metadata. This must be resolved before public distribution.

## 6. Delivery milestones

Every milestone ends in a coherent native binary. Work is accepted only when a clean checkout builds, automated checks pass, examples run, error behavior is exercised, and the root README accurately describes the available product at that commit.

The README must read as current documentation. It must not call features “milestone 1” or document future syntax as available. Roadmap material belongs in this technical document.

### Milestone 1: compiler foundation

Status: complete.

Deliver:

- Rust workspace and pinned toolchain;
- lexer and parser with byte-accurate source spans;
- reusable flows, parameters, immutable bindings, primitive literals, calls, and returns;
- semantic checks for definitions, arity, reachability, entry point, and unsupported recursion;
- compact execution plan using resolved flow IDs and local slots;
- deterministic plan interpreter;
- `mettle check` and `mettle run`;
- source-aware CLI diagnostics;
- no third-party crate dependencies.

User acceptance:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run -- check examples/hello.mettle
cargo run -- run examples/hello.mettle
```

The final command must print `Hello from Mettle`. Editing the example to reference an unknown name must produce a nonzero exit status and a source caret.

### Editor tooling checkpoint

Status: complete. Version 0.4.0 is packaged and installable from `util/plugin/vscode`. It retains declarative language support and adds a small project-aware compiler-backed CodeLens entry point without npm runtime dependencies.

Before HTTP work, deliver:

- a dependency-free declarative VS Code language extension;
- `.mettle` file recognition and TextMate highlighting;
- comments, brackets, automatic closing, indentation, and folding;
- snippets limited to syntax implemented by the compiler;
- a reproducible VSIX package using a pinned official packaging tool;
- command-line installation instructions;
- a language-server direction that reuses the Rust compiler.

User acceptance packages the extension, installs the resulting VSIX, confirms it appears as `flow-lang.mettle-language`, and opens `examples/request-collection.mettle` with the `Mettle` language mode and Run Mettle actions.

### Milestone 2: minimal HTTP vertical slice

Status: complete.

Deliver:

- context declarations, `use context`, `defaults http`, and `env()`;
- HTTP `GET` and `POST` operations;
- binding an HTTP response and using its status or body later in the flow;
- JSON object and array values with `json: { ... }` request options;
- base URL, headers, and operation timeout;
- HTTPS with secure verification by default and explicit `verifyCertificates: false`;
- connection reuse for sequential requests;
- Tokio, Hyper, and Rustls introduced with pinned features and a recorded licence audit.

User acceptance uses repository-owned HTTP and HTTPS fixtures, so it does not depend on the public internet:

```bash
./scripts/acceptance-http.sh
```

The acceptance run demonstrates a response value driving a second request, connection reuse, request timeouts, secure certificate rejection, the explicit certificate-verification override, and an invalid HTTP option failing during compilation.

### Milestone 2.5: entry flows and request collections

Status: complete.

Deliver:

- optional `main` with direct execution of any named flow;
- named CLI arguments parsed as Mettle values;
- expression-bodied declarations such as `flow health() = http.get(...)`;
- bare top-level calls compiled as independently runnable anonymous flows;
- anonymous multi-operation `flow { ... }` blocks;
- source-line selection and machine-readable compiler discovery;
- `${name}` lookup through local/context values followed by environment fallback;
- an optional file-level `use context` default;
- structured human output by default, with `--verbose`, `--quiet`, `--raw`, and `--output json` for explicit levels of detail;
- compiler-backed VS Code Run Mettle CodeLens actions with parameter prompts and structured output.

User acceptance:

```bash
cargo run -- check examples/request-collection.mettle
cargo run -- list examples/request-collection.mettle
./scripts/acceptance-http.sh
```

The acceptance run executes a bare anonymous HTTP flow by source line and a named parameterized HTTP flow by name. Editor packaging verifies that the CodeLens implementation ships without runtime npm dependencies. Human output presents the flow, its operations, useful response data, and completion status. Raw and JSON output remain available for scripts.

### Milestone 3: projects, composition, and assertions

Status: complete.

Deliver:

- context extension/composition with deterministic precedence and cycle errors;
- namespaces, implicit global namespace, and `use namespace`;
- project source discovery without import/export lists;
- first-class assertions over response values and JSON members;
- multiple files contributing to a namespace;
- secrets redacted from normal diagnostics.

User acceptance compiles a multi-file project, composes contexts, executes a flow against the local fixture, and demonstrates both a passing and failing assertion. A context cycle and an ambiguous name must fail before execution.

Run `./scripts/acceptance-project.sh` to exercise these behaviors, including source-aware diagnostics across files and redaction of environment-derived values.

### Milestone 3.5: compiler-backed definition navigation

Status: complete.

Deliver:

- a dependency-free `mettle lsp` server using standard LSP framing over standard input/output;
- full-document synchronization for unsaved `.mettle` source;
- compiler-backed definition lookup for named flow calls and `use context`;
- local definition lookup for parameters and immutable bindings;
- cross-file lookup through project discovery and the same namespace visibility rules used by compilation;
- a small VS Code LSP client with Ctrl+Click, Go to Definition, and Peek Definition.

The server deliberately returns no destination for ambiguous, unresolved, environment, capability-operation, or dynamic member names. Diagnostics, completion, hover, references, and rename remain later editor milestones.

### Milestone 4: structured execution policies

Status: complete.

Deliver:

- `within`, retry, and `parallel` plan nodes;
- cancellation tree and deterministic child ownership;
- bounded task admission and operation queues;
- graceful Ctrl+C behavior;
- controllable clock and deterministic scheduler tests;
- explicit timeout and retry result semantics.

User acceptance runs parallel fixture requests, cancels an in-progress run, verifies prompt shutdown, and confirms that a deadline cancels its child operation. Stress tests must prove queues remain within configured bounds.

`parallel` uses a fixed-size active set and a statically bounded branch list; no
runtime task queue grows with completion latency. Results retain source order.
Failure drops active sibling futures before returning, which cancels HTTP work
without detached tasks. `within` races its complete child future against the
runtime clock, so nested operations and retry delays inherit the same deadline.
The clock is injectable for deterministic deadline and backoff tests. The CLI
races root execution against SIGINT and returns status 130 after dropping the
owned execution tree.

Run `./scripts/acceptance-execution.sh` to verify retry recovery, bounded HTTP
fan-out, deadline cancellation, and graceful Ctrl+C behavior against the local
fixture. Runtime tests additionally exercise 100 branches under a limit of seven
and verify sibling cleanup after a branch failure.

### Milestone 5: local load engine

Status: complete.

Deliver:

- rate and concurrency policies;
- scoped execution results such as `load = rate(...) { ... }`;
- low-overhead counters and latency distributions;
- assertions over results such as `load.latency.p95`;
- bounded overload behavior and saturation reporting;
- reproducible throughput, latency, memory, and allocation benchmarks;
- connection-pool and scheduler tuning based on profiles.

User acceptance runs a documented local load test against the fixture at several rates, verifies the number of admitted/completed iterations, checks a percentile assertion, and observes bounded memory when the requested rate exceeds capacity. Benchmark output records CPU, memory, OS, Mettle revision, fixture configuration, and command line.

`rate(target, period, duration, limit)` schedules iteration starts across a fixed arrival window. Its active set never exceeds `limit`; starts that arrive while the set is full are counted as dropped rather than queued. `concurrency(limit, duration)` maintains a fixed active set until its scheduling window closes. Both policies then drain owned iterations for at most 30 seconds and report a drain timeout as saturation.

The scoped result includes completed, successful, failed, started, and dropped counts; error ratio; total elapsed time; saturation state; latency and scheduling-delay distributions; and policy-specific configuration. Rate results additionally report planned starts and achieved start rate. Runtime errors inside a workload become failed iteration outcomes, so assertions inside a measured flow contribute to the workload error ratio without aborting metric finalization.

Run `./scripts/acceptance-load.sh` to exercise steady-rate scheduling, overload drops, percentile assertions, fixed concurrency, and server-observed concurrency bounds against the local fixture.

### Milestone 5.5: execution reporting

Status: complete.

The runtime exposes a protocol-neutral execution observer. It reports completed capability operations and workload snapshots without coupling execution to a terminal or output format. Workload snapshots are emitted at a bounded interval, and individual operations inside a load iteration are suppressed to protect throughput and memory.

The CLI consumes these events to provide:

- structured flow, operation, response, failure, and completion output;
- an in-place terminal dashboard for rate and concurrency workloads;
- default response previews with bounded large-payload rendering;
- complete human output through `--verbose`;
- final-status output through `--quiet`;
- returned-value output through `--raw`;
- a stable execution envelope through `--output json`;
- automatic plain output for pipes and explicit `--no-color` and `--no-progress` controls.

The VS Code Run Flow action launches the same CLI in a dedicated task terminal, so editor execution and direct terminal execution share one reporting implementation.

### HTTP MVP hardening checkpoint

Deliver:

- common HTTP/1.1 methods through capability operations rather than parser keywords;
- mutually exclusive JSON and UTF-8 text body options validated before execution;
- JSON media-type validation and automatic content types;
- explicit failure for responses that declare JSON but contain malformed JSON;
- response duration and quoted string-key access for HTTP headers and external JSON fields;
- bounded response bodies checked from declared length and while streaming;
- compiler suggestions for mistyped capability operations and option names;
- deterministic acceptance coverage for every method, body mode, size rejection, timeout, TLS verification, and connection reuse.

Redirects, proxy discovery, multipart requests, streaming bodies, custom trust stores, and client certificates remain separate HTTP slices because they introduce additional security or lifecycle policy.

### Milestone 6: MVP refinement and release readiness

Deliver:

- complete cleanup and cancellation edge-case audit;
- final human and machine-readable diagnostics;
- stable metrics definitions and report formats;
- custom CA bundles, mutual TLS/client certificates, and TLS policy controls;
- long-running soak tests and injected network failures;
- parser fuzzing and malformed-server-response coverage;
- cross-platform design audit while Linux remains the release target;
- dependency security, provenance, licence, notices, and SBOM checks;
- documented resource limits and performance envelope;
- packaging of a stripped Linux binary.

User acceptance executes a release checklist from a clean machine, runs the soak and failure suites, verifies graceful shutdown at load, exercises advanced TLS against local fixtures, and installs/runs the packaged binary without a Rust toolchain.

### Portability baseline

The production crates use Rust standard-library paths, Tokio networking and signals, Hyper, and Rustls without direct operating-system APIs. Windows drive paths receive explicit file-URI normalization in the language server. GitHub Actions runs workspace tests, Clippy, and the portable HTTP/load acceptance suite on Linux x64, Windows x64, Apple Silicon macOS, and Intel macOS.

Linux-specific `/proc` sampling remains isolated to the optional benchmark script. Bash acceptance scripts remain useful for the primary development environment, while `scripts/acceptance-portable.py` covers the runtime surface without shell-specific process control.

## 7. Testing strategy

Testing follows the layer boundaries:

- Lexer/parser unit tests cover valid forms, malformed input, Unicode boundaries, and stable spans.
- Compiler tests cover resolution, schema errors, context precedence, cycle detection, and plan shape.
- Runtime tests use deterministic clocks and fake capabilities for cancellation, deadlines, admission, and cleanup.
- HTTP integration tests use local servers with controlled delays, disconnects, malformed responses, and TLS certificates.
- CLI tests assert exit codes, stdout/stderr separation, diagnostics, and project discovery.
- Editor-extension checks validate the manifest, grammar, snippets, VSIX contents, and installation identity.
- Language-server tests will exercise protocol requests against the real compiler without launching an editor.
- Property and fuzz tests target parsers, plan validation, and protocol boundary handling.
- Load tests measure correctness under saturation before measuring peak throughput.
- Benchmarks record their environment and preserve comparable baselines in CI artifacts.

Tests should verify externally meaningful behavior or high-risk invariants. They should not merely duplicate implementation branches.

## 8. Performance method

Performance work proceeds from measurement:

1. Establish a repeatable local fixture and workload.
2. Record throughput, latency distribution, CPU, resident memory, allocations, connections, and errors.
3. Profile CPU and allocation hot spots.
4. Change one material factor at a time.
5. Re-run correctness, saturation, and benchmark suites.
6. Keep an optimization only when the improvement is repeatable and the complexity is justified.

Initial design choices that protect performance include immutable shared plans, local-slot resolution, bounded queues, connection reuse, worker-local metric accumulation, and avoiding per-request source parsing or name lookup.

Peak requests per second is not a sufficient measure. A fast engine that drops intended starts silently, skews percentile calculations, leaks response bodies, or grows memory without bound is incorrect.

## 9. Deferred work

The following remain outside the local HTTP MVP:

- SIP and other built-in protocols;
- stable third-party binary plugins;
- distributed load generation;
- remote workers and result aggregation;
- native-code generation;
- durable workflow recovery;
- browser-based authoring or dashboards;
- Windows and macOS release packages.

The architecture must avoid blocking these directions, but the MVP should not implement speculative abstractions that have no immediate test case.

## 10. Open decisions

Decisions should be captured as small records when implementation reaches them. The next material choices are:

- whether HTTP response and error values should gain explicit language types;
- assertion policy for HTTP status codes (status codes currently remain response values);
- JSON number representation;
- context merge and explicit removal rules;
- rate overload semantics and fairness;
- latency histogram accuracy and range;
- machine-readable result format;
- the project's public distribution licence.

None of these require weakening the compiler/runtime boundary established by the first milestone.
