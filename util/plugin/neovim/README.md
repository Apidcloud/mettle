# Mettle for vim-test in Neovim

This adapter connects `.mettle` files to an existing
[vim-test](https://github.com/vim-test/vim-test) installation. Requires Neovim
0.11+ and the Mettle CLI. It adds no mappings and uses your configured vim-test
output strategy, including Vimux or Neovim terminal splits.

Add the adapter to the runtime path and register it in your Neovim configuration:

```lua
vim.opt.runtimepath:append("/path/to/mettle/util/plugin/neovim")
require("mettle_vim_test").setup({
  executable = "/path/to/mettle/target/debug/mettle", -- defaults to "mettle" on PATH
})
```

The executable is a path, not a shell command. Use the same binary as your Mettle
LSP configuration. No change to your existing vim-test mappings is needed.

With lazy.nvim, run this setup **after** `require("lazy").setup(...)`, or in a
`User LazyDone` autocmd if your Mettle configuration loads earlier. Lazy resets
the runtime path during startup; a path added only before that reset disappears,
causing `Unknown function: test#mettle#mettle#test_file` when running a test.

| Command | Behavior |
| --- | --- |
| `:TestNearest` | Run the nearest preceding flow, anonymous flow/request, or test; above the first declaration, run that declaration. |
| `:TestFile` | Run all tests declared in the file, or `mettle run <file> --all` if it has no tests. Parameterized flows are skipped by `--all`. |
| `:TestLast` | Repeat the last command, including previously entered parameter values. |
| `:TestVisit` | Return to the file and cursor position of the previous run. |
| `:TestSuite` | Report that project-wide suites are not supported. |

Nearest selection uses `mettle list <file> --json`, including compiler-reported
source locations for anonymous declarations. Project dependencies are filtered
out of the selection. Multiple declarations on one line use the cursor column
to choose a declaration. Selection and execution use saved source: save modified
Mettle buffers in the project before running. The adapter rejects unsaved source
for nearest/file runs; `:TestLast` uses vim-test's normal saved-file behavior.
Vim-test also supports its usual `autowrite`/`autowriteall` settings.

Nearest flows prompt for required parameters, using the CLI's value syntax
(plain strings, numbers, booleans, or Mettle literals). Escape cancels execution.
You can supply values directly, for example `:TestNearest --arg id=1`.
Normal options such as `:TestFile --verbose` or `:TestNearest --profile qa` work.
As with other vim-test runners, command-line arguments are shell arguments;
quote shell-sensitive values. Values entered through the prompt are escaped
automatically.

To display output in a reusable Neovim terminal instead of an external pane,
vim-test supports `vim.g["test#strategy"] = "neovim_sticky"`. This adapter leaves
that choice to your existing configuration.

## Verify the adapter

Build the CLI, then run the integration checks with a local vim-test checkout:

```sh
cargo build -p mettle-cli
METTLE_VIM_TEST_PATH=/path/to/vim-test nvim --headless -u NONE -i NONE \
  -l util/plugin/neovim/test/integration.lua
```

The checks use temporary local fixtures and make no network requests.
