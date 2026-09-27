# Agent instructions for Mettle

These instructions apply throughout this repository. Read
[CONTRIBUTING.md](CONTRIBUTING.md) for the shared contribution workflow and commit
conventions.

## Project context and sources of truth

Mettle is an experimental I/O-oriented language and native Rust runtime for
protocol workflows, functional checks, and load tests. HTTP is the implemented
protocol capability. SIP, distributed workers, native-code generation, and
external binary plugins are future work; do not assume they exist.

- Use the implementation, tests, and checked-in configuration to establish current
  behavior. [README.md](README.md) is the current user-facing overview; verify it
  against the code when changing behavior.
- This file and [CONTRIBUTING.md](CONTRIBUTING.md) record working conventions.
- Files under `docs/` are temporary supporting material, including design proposals
  and implementation notes. They may be stale; consult them for context rather
  than treating them as a live specification or mandatory reading.
- Use Cargo manifests, `Cargo.lock`, and `scripts/check-licenses.py` for the current
  dependency graph and licence checks.
- Read the relevant editor README before changing a plugin under `util/plugin/`.

Maintained documentation on GitHub, potentially a wiki, is planned for beta.
Until it exists, keep the README and examples aligned with behavior without
requiring every historical design note to be maintained.

## Repository boundaries

| Location | Responsibility |
| --- | --- |
| `crates/mettle-syntax` | Lexer, parser, AST, and source spans |
| `crates/mettle-capability` | Capability descriptors, schemas, shared values, and execution interfaces |
| `crates/mettle-compiler` | Resolution, semantic validation, and execution-plan lowering |
| `crates/mettle-runtime` | Validated-plan interpretation, scheduling, cancellation, and metrics |
| `crates/mettle-http` | HTTP schemas, pooling, request/response handling, and TLS |
| `crates/mettle-fs` | Complete file I/O, file sources, and transactional publication |
| `crates/mettle-cli` | Commands, project discovery, environment profiles, reporting, and LSP |
| `util/plugin/` | VS Code, Neovim, and Tree-sitter integrations |
| `tests/fixtures`, `tests/projects`, `util/test-server` | Local acceptance programs, project fixtures, and HTTP/HTTPS server |
| `examples/`, `docs/`, `scripts/` | User examples, temporary supporting notes, and verification tools |

Keep protocol behavior behind capability interfaces. The parser handles general
language syntax; the compiler validates capability descriptors without depending
on the HTTP client. The runtime executes resolved plans rather than reparsing or
resolving source during execution. Editor intelligence reuses the compiler and LSP.
Keep larger crates organized into focused modules rather than growing `lib.rs`
indefinitely.

## Implementation rules

- Use the Rust toolchain pinned in `rust-toolchain.toml` (currently 1.98.1), Rust
  edition 2024, and workspace lint settings. `unsafe` code is forbidden.
- Support Linux x64, Windows x64, and Apple Silicon macOS. Intel macOS is not a
  supported target. Keep platform-specific code isolated with portable behavior.
- Preserve structured concurrency: work has an owner, cancellation propagates,
  cleanup is bounded, and queues, concurrency, capture, and metrics remain bounded.
  Do not introduce detached runtime work or blocking I/O on async execution paths.
- Preserve sensitive-value metadata and redaction through transformations,
  diagnostics, reports, and editor output. Never commit real credentials; checked-in
  environment examples and TLS material are local fixtures only.
- Preserve source spans and useful diagnostics across parsing, compilation,
  execution, and LSP. Keep JSON output parseable and stdout/stderr behavior deliberate.
- Prefer existing infrastructure and explicit dependency features. Update
  `Cargo.lock`, dependency documentation, and the generated licence report when
  dependency changes require them. Do not bypass the licence allowlist to pass CI.
- Keep language changes aligned across the Rust parser/compiler/runtime, TextMate
  grammar and snippets, Tree-sitter grammar/queries, examples, and documentation as
  applicable. Generate Tree-sitter outputs with CLI 0.25.8; do not hand-edit them.
- Keep changes focused. Preserve unrelated local work and avoid speculative
  abstractions, unrelated refactors, or changing semantics merely to silence tests.

## Verification and delivery

For code changes, run the relevant tests first, then the applicable CI checks
listed in [CONTRIBUTING.md](CONTRIBUTING.md#validation). Add regression coverage for
changed behavior or important invariants. Use local fixtures, fake capabilities,
injectable clocks, and synchronization instead of public services or fragile
sleep-based ordering assertions.

For documentation-only changes, verify accuracy, links, and `git diff --check`;
runtime tests are unnecessary unless executable behavior also changed. Report
what changed, checks actually run, and any checks that could not be completed.
Do not claim a CI run passed based only on local checks.

All new commits must use Conventional Commits:
`<type>[optional scope][!]: <description>`. This includes commits created by
agents. Use a concise imperative description, a relevant type such as `feat`,
`fix`, `docs`, `refactor`, `test`, `perf`, `build`, `ci`, `chore`, or `revert`, and
`!` or a `BREAKING CHANGE:` footer for breaking changes. Examples:

```text
feat(runtime): add bounded batch execution
fix(cli): redact sensitive assertion messages
ci: remove macOS Intel coverage
docs: document contribution and agent conventions
```

Follow the user's requested Git workflow; do not rewrite existing history to
convert older commit messages to this convention.
