# Change Log

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

- Recognize `.flow` files.
- Add TextMate syntax highlighting.
- Configure comments, brackets, automatic closing, indentation, and folding.
- Add `flow` and `flowmain` snippets.
