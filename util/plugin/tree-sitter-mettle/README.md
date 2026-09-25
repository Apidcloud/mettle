# Tree-sitter Mettle

Editor parser for the syntax implemented by `crates/mettle-syntax`, with
highlighting, folding, and Neovim indentation queries. The grammar covers flows,
tests, contexts, namespaces, conditionals, assertions, calls and option blocks,
structured and load policies, `for` expressions, `fail()`, named arguments and
parallel branches, values, member/index access, and interpolation. It supports
optional declaration parentheses, trailing commas in arrays/calls/policy options,
signed and base-prefixed numbers, exponents, and fractional durations.
The compiler remains responsible for name resolution, valid call targets,
numeric limits, required/duplicate policy options, and semantic validation. VS Code continues using its TextMate
grammar and the Mettle language server.

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

Optional Neovim integration check (macOS/Linux, from this directory):

```sh
cc -shared -fPIC -Isrc src/parser.c src/scanner.c -o /tmp/mettle-test.so
METTLE_TS_PARSER=/tmp/mettle-test.so \
  nvim --headless -u NONE -i NONE -l scripts/check-neovim.lua
```

This checks highlighting captures and compares incremental parses with fresh
parses after valid and incomplete edits. It does not load or change user config.

## Neovim 0.11 with nvim-treesitter master

These instructions target the `master` branch of `nvim-treesitter`, whose API
differs from `main`. Add the parser registration inside the plugin's `config`
function, before its existing `require("nvim-treesitter.configs").setup(...)`:

```lua
local parsers = require("nvim-treesitter.parsers").get_parser_configs()
parsers.mettle = {
  install_info = {
    url = vim.fn.expand("~/path/to/mettle/util/plugin/tree-sitter-mettle"),
    files = { "src/parser.c", "src/scanner.c" },
    generate_requires_npm = false,
    requires_generate_from_grammar = false,
  },
  filetype = "mettle",
}
```

Enable `highlight = { enable = true }` in that setup, and register the filetype
once if it is not already registered by your Mettle LSP setup:

```lua
vim.filetype.add({ extension = { mettle = "mettle" } })
```

The parser installer does not install queries. Link the query directory into
your config, replacing the checkout path below. If you already have custom
Mettle queries, merge them deliberately rather than replacing them:

```sh
mkdir -p ~/.config/nvim/queries
ln -s /absolute/path/to/mettle/util/plugin/tree-sitter-mettle/queries \
  ~/.config/nvim/queries/mettle
```

Restart Neovim, then run `:TSInstall mettle` and open a `.mettle` file.
`:Inspect` shows the highlight captures under the cursor; `:InspectTree` shows
the syntax tree. LSP diagnostics and navigation work alongside Tree-sitter.

After changing the grammar or scanner, run `npm run generate`, then
`:TSInstall! mettle` and restart Neovim to load the rebuilt parser. Query-only
changes need no compilation; reopening Neovim reloads them from the symlink.
No tmux, project, or Mettle CLI restart is required.

## Licence

The Mettle grammar follows the repository's current `UNLICENSED` status.
