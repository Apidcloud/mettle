# Mettle Language for VS Code

Basic language support for `.mettle` source files.

## Available editor features

- `.mettle` file recognition;
- TextMate syntax highlighting for supported Mettle constructs, including conditionals and boolean operators;
- `//` comment toggling;
- matching and automatic closing of braces, brackets, parentheses, and strings;
- indentation and region folding;
- snippets for named flows, anonymous flows, tests, namespaces, assertions,
  reusable and anonymous file contexts, `senv()`, conditionals, assertion messages, structured and load execution
  policies, and HTTP requests;
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

To run every zero-argument flow in the active file, use **Mettle: Run All Eligible Flows in File** from the Command Palette or the editor title bar. Parameterized flows are skipped; all eligible flows run sequentially and the terminal ends with a batch summary.

To run tests declared in the active file, use **Mettle: Run Tests in File**.
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

## Package

From this directory:

```bash
npm run package
```

Packaging uses the pinned official Microsoft `@vscode/vsce` 4.0.0 tool. It is downloaded by `npx` and is not included in the installed extension.

The resulting package is:

```text
dist/mettle-language-0.15.0.vsix
```

## Install

Install or update from the command line:

```bash
code --install-extension dist/mettle-language-0.15.0.vsix --force
```

Alternatively, open the Extensions view, choose **Install from VSIX…**, and select the package from `dist/`.

Open any `.mettle` file after installation. VS Code should show `Mettle` as the language mode in the status bar. Reload the editor window if an already-open file does not update immediately.

## Development

Open this extension directory in VS Code and press `F5` to launch an Extension Development Host. The extension is plain JavaScript and needs no compilation step.

Inspect highlighting scopes with **Developer: Inspect Editor Tokens and Scopes** from the Command Palette.
