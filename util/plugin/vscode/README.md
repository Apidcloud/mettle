# Flow Language for VS Code

Basic language support for `.flow` source files.

## Available editor features

- `.flow` file recognition;
- TextMate syntax highlighting for current and proposed Flow constructs;
- `//` comment toggling;
- matching and automatic closing of braces, brackets, parentheses, and strings;
- indentation and region folding;
- snippets for named flows, anonymous flows, contexts, HTTP GET, and HTTP POST;
- compiler-backed **Run Flow** CodeLens actions above every named and anonymous flow;
- prompts for named flow parameters and execution in a dedicated task terminal.

The extension contains a small JavaScript entry point using only VS Code and Node built-in APIs. It has no npm runtime dependencies. Flow discovery comes from the Rust compiler through `flow list - --json`, so the editor does not maintain a second parser. Compiler diagnostics, completion, hover information, navigation, rename, formatting, and semantic highlighting require the planned Flow language server and are not available yet.

## Run flows

The Flow CLI must be available on `PATH`. From the repository root:

```bash
cargo install --path crates/flow-cli --locked
```

Set **Flow: Executable Path** when the binary lives elsewhere.

Open a `.flow` file and use the action shown above any declaration:

```text
▶ Run GET https://postman-echo.com/get?demo=bare
http.get("https://postman-echo.com/get?demo=bare")

▶ Run inspectRequest
flow inspectRequest(baseUrl, requestId) =
    http.get("${baseUrl}/get?requestId=${requestId}")
```

The extension asks for `baseUrl` and `requestId` before launching `inspectRequest`. Use `https://postman-echo.com` as the base URL for the included demo. Anonymous flows are selected by their compiler-reported identity; named flows are selected by name. Dirty files are saved before execution. Results default to method, URL, and status. Use `flow run --verbose` in a terminal when response data, headers, and the full response envelope are needed.

## Package

From this directory:

```bash
npm run package
```

Packaging uses the pinned official Microsoft `@vscode/vsce` 4.0.0 tool. It is downloaded by `npx` and is not included in the installed extension.

The resulting package is:

```text
dist/flow-language-0.3.0.vsix
```

## Install

Install or update from the command line:

```bash
code --install-extension dist/flow-language-0.3.0.vsix --force
```

Alternatively, open the Extensions view, choose **Install from VSIX…**, and select the package from `dist/`.

Open any `.flow` file after installation. VS Code should show `Flow` as the language mode in the status bar. Reload the editor window if an already-open file does not update immediately.

## Development

Open this extension directory in VS Code and press `F5` to launch an Extension Development Host. The extension is plain JavaScript and needs no compilation step.

Inspect highlighting scopes with **Developer: Inspect Editor Tokens and Scopes** from the Command Palette.
