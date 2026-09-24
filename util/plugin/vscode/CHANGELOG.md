# Change Log

## 0.16.0

- Add an `echo()` snippet for flow-scoped diagnostic messages.
- Show logical parallel-branch and retry-attempt labels in the CLI report.

## 0.15.0

- Highlight `if`, `else`, `and`, `or`, and `not`; add a conditional snippet.
- Stop highlighting unsupported `data`, `race`, and `repeat` as language constructs.
- Document explicit statement and field separators, indexing, and short-circuit conditions.

## 0.14.0

- Add an assertion-with-message snippet for the new test failure reporting syntax.

## 0.13.0

- Add a play button above each test declaration to run that test alone.
- Support selecting one test by name or source line with `mettle test`.

## 0.12.0

- Show parser and compiler diagnostics in VS Code as files change, including
  unsaved project files, and clear stale diagnostics after fixes or closing files.

## 0.11.0

- Discover `.env` profiles for standalone files and projects in a status-bar picker.
- Pass the selected profile to Run Flow, Run All, and Run Tests tasks.
- Add a file-level Run Tests command.
- Make profile selection available from the editor title bar and label the
  status-bar selection explicitly.
- Add snippets for file tests, anonymous file contexts, and `senv()`.

## 0.10.0

- Add a snippet for POST, PUT, PATCH, and DELETE requests with text bodies.
- Recognize quoted key access used for HTTP headers and external JSON fields.

## 0.9.0

- Show structured flow reports in the Run Flow task terminal.
- Display live rate and concurrency progress when the CLI is attached to a terminal.
- Keep raw and JSON output modes available for automation.

## 0.8.0

- Highlight and provide snippets for `rate` and fixed `concurrency` workloads.
- Support the local load engine and scoped workload result syntax.

## 0.7.0

- Rename the extension, language mode, CLI configuration, and VSIX package to Mettle.
- Continue to use `flow` as the language keyword for reusable workflows.

## 0.6.1

- Keep anonymous Run Flow actions valid when their request text changes.
- Refresh Run Flow CodeLens actions after saving a `.mettle` file.
- Avoid guessing when structural edits make an anonymous declaration ambiguous.

## 0.6.0

- Highlight and provide snippets for `within`, `retry`, and bounded `parallel` expressions.

## 0.5.0

- Add a compiler-backed language server using LSP over standard input/output.
- Add Ctrl+Click, Go to Definition, and Peek Definition for flows, contexts,
  parameters, and local bindings.
- Resolve declarations across project files and synchronize unsaved documents.
- Keep the extension free of npm runtime dependencies.

## 0.4.0

- Discover runnable flows through project-aware CLI compilation.
- Add namespace and assertion snippets for the Milestone 3 language.
- Keep CodeLens actions scoped to declarations in the open file.

## 0.3.0

- Add compiler-backed Run Flow CodeLens actions for named and anonymous flows.
- Prompt for named flow arguments and execute them in a dedicated task terminal.
- Render Run Flow results with the CLI's concise HTTP summary.
- Add highlighting and snippets for expression-bodied and anonymous flows.

## 0.2.0

- Add context, HTTP GET, and JSON POST snippets.
- Highlight the implemented context, HTTP, JSON, duration, and member-access syntax.

## 0.1.0

- Recognize `.mettle` files.
- Add TextMate syntax highlighting.
- Configure comments, brackets, automatic closing, indentation, and folding.
- Add `flow` and `flowmain` snippets.
