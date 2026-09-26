# Tree-sitter Mettle

Editor parser for the syntax implemented by `crates/mettle-syntax`, with
highlighting, folding, and Neovim indentation queries. The grammar covers flows,
tests, contexts, namespaces, conditionals, assertions, calls and option blocks,
structured and load policies, `for` expressions, `fail()`, named arguments and
parallel branches, values, member/index access, and interpolation. It supports
optional declaration parentheses, trailing commas in arrays/calls/policy options,
signed and base-prefixed numbers, exponents, and fractional durations.
The compiler remains responsible for name resolution, valid call targets,
numeric limits, required/duplicate policy options, and semantic validation.
VS Code continues using its TextMate grammar and the Mettle language server.

## Develop and test

Use Node.js, a C compiler, and **Tree-sitter CLI 0.25.8**. No npm dependencies or
Rust workspace dependencies are added. Install the pinned CLI if needed:

```sh
cargo install tree-sitter-cli --version 0.25.8 --locked
```

From this directory:

```sh
npm run generate
npm test
```

Commit `src/parser.c`, `src/grammar.json`, `src/node-types.json`, and the generated
headers alongside grammar changes. Consumers can compile the checked-in C files
without Node.js or the generator. The generated parser uses ABI 15, supported by
Neovim 0.11 and newer. Keep syntax changes aligned with the Rust parser and add
corpus cases with reviewed expected trees. Tests also parse every repository
example/fixture, reject malformed syntax, and check recovery and query validity.

`src/scanner.c` handles newline separators without preventing multiline calls,
operators, option blocks, or `else` branches. Its lookahead tokens consume no
source text, so comments remain available for highlighting. Expressions can span
lines; statements require newlines, while object/context fields and parallel
branches accept commas or newlines. Arrays and call arguments require commas.

## Editor integration

The grammar package stays independent of any editor. Shared query files live in
`queries/mettle/`; `tree-sitter.json` points the CLI at the highlighting query.
This layout also lets Neovim discover the queries directly on its runtime path.

For Neovim, use the sibling [Neovim plugin's Tree-sitter setup](../neovim/README.md#tree-sitter).
It registers the local parser and its queries without copying files or requiring
personal symlinks. Its integration test covers setup, query discovery,
highlighting, and incremental edits.

After changing the grammar or scanner, run `npm run generate`, then
`:TSInstall! mettle` and restart Neovim to load the rebuilt parser. Query-only
changes need no compilation; restarting Neovim reloads them from the checkout.
No tmux, project, or Mettle CLI restart is required.

## Licence

The Mettle grammar follows the repository's current `UNLICENSED` status.
