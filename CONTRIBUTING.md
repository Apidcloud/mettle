# Contributing to Mettle

Mettle is an experimental I/O-oriented language and native runtime for protocol
workflows, functional checks, and load tests. The language and interfaces are still
evolving. Contributions should keep the compiler, runtime, examples, and editor
tooling consistent and protect correctness under load.

Start with [README.md](README.md) and the executable examples. Establish current
behavior from the implementation, tests, and checked-in configuration. This guide
records contribution conventions; agent-specific instructions live in
[AGENTS.md](AGENTS.md).

Files under `docs/` are temporary supporting material, such as proposals and
implementation notes. They serve a limited purpose and may be stale; they are
optional context rather than a live specification. Maintained documentation on
GitHub, potentially a wiki, is planned for beta. For now, update the README,
relevant editor READMEs, and examples when behavior changes; historical design
notes do not need to track every implementation change.

## Development setup

The supported targets are Linux x64, Windows x64, and Apple Silicon macOS. Linux
and macOS are the primary development environments. Intel macOS is not supported.

Install Rust through Rustup. Cargo automatically selects the toolchain pinned in
`rust-toolchain.toml`, currently Rust 1.98.1 with Rustfmt and Clippy. The workspace
uses Rust edition 2024. You also need Python 3 for acceptance fixtures and licence
checks, Node.js and npm for editor checks, and Bash plus ripgrep (`rg`) for the
shell acceptance scripts. Tree-sitter development requires a C compiler and
Tree-sitter CLI 0.25.8. See the plugin READMEs for Neovim-specific prerequisites.

From the repository root:

```sh
cargo build --workspace --locked
cargo run --locked -- check examples/language/basics.mettle
cargo run --locked -- run examples/language/basics.mettle
```

The runtime uses Rustls and does not require a system OpenSSL installation.

## Code and architecture conventions

The Rust crates have distinct responsibilities:

| Crate | Responsibility |
| --- | --- |
| `mettle-syntax` | Lexer, parser, AST, source spans |
| `mettle-capability` | Shared schemas, values, and capability interfaces |
| `mettle-compiler` | Resolution, validation, and execution-plan lowering |
| `mettle-runtime` | Interpretation, scopes, scheduling, cancellation, metrics |
| `mettle-http` | HTTP schema, pooled client, response handling, TLS |
| `mettle-fs` | Bounded file reads/writes and incremental file sources |
| `mettle-cli` | Commands, discovery, profiles, reporting, LSP |

- Keep protocol-specific behavior in capability implementations. The language
  parser should not grow a grammar production for each protocol operation.
- Compile and validate before execution. Runtime hot paths use resolved plans;
  they should not parse source or repeatedly resolve names.
- Keep public APIs in `lib.rs` and substantial implementation in focused modules.
  Format Rust with Rustfmt and follow the workspace's Clippy `all` and `pedantic`
  lint settings. `unsafe` code is forbidden across the workspace.
- Preserve bounded queues, concurrency, memory capture, and metrics. Concurrent
  work must have an owner, cancellation path, and bounded cleanup. Avoid detached
  runtime tasks and blocking operations on async execution paths.
- Preserve secret sensitivity and redaction in derived values, diagnostics, and
  human and machine-readable reports. Use dummy credentials and local TLS fixtures
  in tests; never commit real secrets.
- Keep diagnostics source-aware and machine-readable output stable and parseable.
  LSP behavior should reuse the compiler's resolution and validation rules.
- Keep platform-specific optimizations behind internal interfaces with portable
  fallback behavior. Measure performance changes with reproducible workloads and
  document the hardware, revision, fixture, and command used.

Language changes may require updates to the Rust implementation, CLI/LSP, VS Code
TextMate grammar and snippets, Tree-sitter grammar and queries, examples, and
documentation. Add meaningful coverage at the layers affected by the change.
Keep examples executable and document implemented behavior separately from plans.

## Validation

Start with focused checks for the affected crate or plugin. Before submitting
code changes, run the applicable checks below. The authoritative CI configuration
is [.github/workflows/portable.yml](.github/workflows/portable.yml).

### Rust and portable runtime

Run from the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 scripts/check-licenses.py
python3 scripts/acceptance-portable.py
python3 scripts/acceptance-filesystem.py
python3 scripts/acceptance-content.py
python3 scripts/acceptance-streaming.py
```

GitHub Actions runs workspace tests, Clippy, and portable HTTP/load acceptance on
all three supported platforms. The Python acceptance script builds the CLI and
starts its own loopback fixture; it does not require public services.

For changes involving HTTP, projects, execution policies, or load behavior, also
run the relevant deeper acceptance scripts in a Bash environment:

```sh
./scripts/acceptance-http.sh
./scripts/acceptance-project.sh
./scripts/acceptance-execution.sh
./scripts/acceptance-load.sh
```

Prefer deterministic tests with local fixtures, fake capabilities, injectable
clocks, or explicit synchronization. Avoid asserting completion order from short
wall-clock sleeps. Cover externally meaningful behavior and important invariants,
including errors, cancellation, limits, and redaction where relevant.

### Editor tooling

Run VS Code checks from `util/plugin/vscode`:

```sh
npm test
```

For grammar changes, install the pinned generator from the repository root:

```sh
cargo install tree-sitter-cli --version 0.25.8 --locked
```

Then run from `util/plugin/tree-sitter`:

```sh
npm run generate
npm test
```

Commit generated parser files and headers alongside grammar changes. Do not
hand-edit generated outputs. Add corpus cases with reviewed expected trees;
the tests also check repository examples, malformed syntax, and queries. CI
regenerates the parser and requires no resulting diff under `src/`.

For editor-specific changes, follow the packaging or integration checks in the
[VS Code README](util/plugin/vscode/README.md),
[Tree-sitter README](util/plugin/tree-sitter/README.md), and
[Neovim README](util/plugin/neovim/README.md).

Documentation-only changes need accurate content, working relative links, and
`git diff --check`; they do not require running runtime suites. State which checks
you ran and identify any checks you could not run.

## Dependency changes

Prefer mature dependencies for difficult infrastructure rather than convenience
in runtime hot paths. Use workspace dependencies where appropriate and enable
only necessary features. Keep the compiler foundation free of third-party parser
or compiler frameworks and the VS Code extension free of runtime npm dependencies.

Commit `Cargo.lock` changes and update dependency documentation when the dependency
set or feature policy changes. Regenerate and verify the licence report:

```sh
python3 scripts/check-licenses.py --write
python3 scripts/check-licenses.py
```

`docs/third-party-licenses.md` is generated; do not edit it manually. Unknown or
new licence expressions require explicit review before changing the allowlist.
The repository's distribution licence remains undecided and package metadata is
currently `UNLICENSED`.

## Commits and pull requests

All new commit subjects must follow Conventional Commits:

```text
<type>[optional scope][!]: <description>
```

Use `feat` for features, `fix` for fixes, `docs` for documentation, `refactor` for
behavior-preserving restructuring, `test` for tests, `perf` for performance,
`build` for build/dependency tooling, `ci` for pipelines, `chore` for maintenance,
and `revert` for reversions. Scopes are optional and should identify the affected
component, such as `syntax`, `compiler`, `runtime`, `http`, `cli`, or `vscode`.
Write a concise imperative description. Mark breaking changes with `!` in the
subject or a `BREAKING CHANGE:` footer explaining the impact and migration.

```text
feat(runtime): add bounded batch execution
fix(cli): preserve redaction in JSON reports
test(http): cover malformed response bodies
ci: remove macOS Intel coverage
docs: add contributor and agent guidance
feat(cli)!: change the JSON result envelope
```

This convention applies to contributor and agent commits, including squash commit
messages. Existing history does not need to be rewritten.

Keep each contribution focused and avoid unrelated formatting or refactors. A
pull request should explain the concrete problem, resulting behavior, relevant
design tradeoffs, and validation performed. Include updated examples and docs
when user-facing behavior changes. Do not claim that local checks establish a
successful GitHub CI run.
