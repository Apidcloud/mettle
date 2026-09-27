# Mettle Language for VS Code

Basic language support for `.mettle` source files.

## Available editor features

- `.mettle` file recognition;
- TextMate syntax highlighting for supported Mettle constructs, including loops, conditional and boolean operators, numeric bases, exponents, and fractional durations;
- `//` comment toggling;
- matching and automatic closing of braces, brackets, parentheses, and strings;
- indentation and region folding;
- snippets for named flows, anonymous flows, tests, namespaces, assertions, terminal `fail()`,
  reusable and anonymous file contexts, `senv()`, `echo()`, conditionals, assertion messages, loops, named parallel branches, structured and load execution
  policies, and HTTP requests using named `body`;
- snippets for complete filesystem reads, incremental copies, and streamed HTTP uploads;
- compiler-backed **Run Flow** CodeLens actions above every named and anonymous flow;
- **Run Test** play buttons above each test declaration; each runs only that test;
- parser and compiler diagnostics in the editor, including unsaved project files;
- a file-level **Run All Eligible Flows** command for zero-argument flows;
- a file-level **Run Tests in File** command;
- a status-bar profile picker for `.env` and `.env.<name>` files;
- prompts for named flow parameters and execution in a dedicated task terminal.
- Ctrl+Click, **Go to Definition**, and **Peek Definition** for flow calls,
  context uses, parameters, and local bindings, including declarations in other
  project files and references in unsaved editor text.

The extension contains a small JavaScript entry point using only VS Code and Node built-in APIs. It has no npm runtime dependencies. Mettle discovery comes from project-aware `mettle list <file> --json`. Navigation and diagnostics use the Language Server Protocol through `mettle lsp`; they reuse the Rust parser, compiler, project discovery, source spans, and namespace resolver, so the editor does not maintain a second language implementation. Diagnostics refresh as open files change, when files are saved, and when project files change on disk. Completion, hover information, references, rename, formatting, and semantic highlighting are planned but are not available yet.

## Navigate source

Hold Ctrl and click a flow call, a name in `use context`, or a local name to
open its declaration. The usual **Go to Definition** (`F12`) and **Peek
Definition** (`Alt+F12`) commands work as well. Cross-file lookup follows the
same implicit-global, current-namespace, and `use namespace` rules as the
compiler. Ambiguous and unresolved names deliberately have no destination.

**Go to Implementation** opens the declaration of a named flow call or local
binding reference. Parameters and named contexts use Go to Definition.

The extension starts `mettle lsp` in the background and synchronizes complete
in-memory documents, so a file does not need to be saved before navigation.

## Run flows

The Mettle CLI must be available on `PATH`. From the repository root:

```bash
cargo install --path crates/mettle-cli --locked
```

Set **Mettle: Executable Path** when the binary lives elsewhere.

Open a `.mettle` file and use the action shown above any declaration:

```text
▶ Run GET https://jsonplaceholder.typicode.com/posts/1
http.get("https://jsonplaceholder.typicode.com/posts/1")

▶ Run inspectRequest
flow inspectRequest(baseUrl, requestId) =
    http.get("${baseUrl}/posts/${requestId}")
```

The extension asks for `baseUrl` and `requestId` before launching `inspectRequest`. Use `https://jsonplaceholder.typicode.com` as the base URL for the included demo. Anonymous flows are selected by their compiler-reported identity; named flows are selected by name. Dirty files are saved before execution. The dedicated task terminal shows the same structured flow report and live workload dashboard as the CLI. Use `mettle run --verbose` to expand HTTP response headers and decoded bodies.

To run every zero-argument flow in the active file, use **Mettle: Run All Eligible Flows in File** from the Command Palette or the editor title bar. Parameterized flows are skipped; the terminal ends with a batch summary. Project batches use `[run].jobs` from the nearest `mettle.toml`, defaulting to one job when omitted.

To run tests declared in the active file, use **Mettle: Run Tests in File**.
This action honors `[test].jobs` in `mettle.toml`; selecting an individual test
still runs only that test. For example, `[test]` followed by `jobs = 4` enables
up to four concurrent tests in the file without extra editor configuration.
With a `.mettle` file open, click **Mettle profile: Default** (or the current
profile name) in the bottom status bar, click the gear icon in the editor title
bar, or run **Mettle: Select Profile** from the Command Palette to choose
`qa`, `prod`, or another profile discovered beside the file or at its
project root. With `.env` and `.env.qa`, the picker shows **Default** and **qa**.
**Default** uses `.env` without passing `--profile`. The selection is remembered
per project (or per standalone-file directory) and
is passed to Run Flow, Run All, and Run Tests. The picker refreshes when the
active file changes, the window gains focus, or env files are created, deleted,
or renamed. If a selected profile disappears, the status bar warns rather than
silently switching environments.

Filesystem paths in standalone executions start at the active file's containing
directory, which is the task's CLI invocation directory. If the file belongs to
a Mettle project, the CLI uses the project root or its top-level `workingDir`
setting instead. Helper flows use the same directory. This does not change the
profile picker's environment discovery. File reads/writes and `body: fs.stream`
uploads use the ordinary flow actions; no exchange UI is required.

## Package

From this directory:

```bash
npm run package
```

Packaging uses the pinned official Microsoft `@vscode/vsce` 4.0.0 tool. It is downloaded by `npx` and is not included in the installed extension.

The resulting package is:

```text
dist/mettle-language-1.0.0-alpha.1.vsix
```

## Install

Install or update from the command line:

```bash
code --install-extension dist/mettle-language-1.0.0-alpha.1.vsix --force
```

Alternatively, open the Extensions view, choose **Install from VSIX…**, and select the package from `dist/`.

Open any `.mettle` file after installation. VS Code should show `Mettle` as the language mode in the status bar. Reload the editor window if an already-open file does not update immediately.

## Development

Open this extension directory in VS Code and press `F5` to launch an Extension Development Host. The extension is plain JavaScript and needs no compilation step.

Inspect highlighting scopes with **Developer: Inspect Editor Tokens and Scopes** from the Command Palette.

The current snippets prefer `flow main { ... }`, `test "name" { ... }`, and
`http.post("/path", body: { ... })`. Existing parenthesized declarations and
legacy HTTP option blocks remain accepted by the CLI.
