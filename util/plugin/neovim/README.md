# Mettle for vim-test in Neovim

This adapter connects `.mettle` files to an existing
[vim-test](https://github.com/vim-test/vim-test) installation. Requires Neovim
0.11+ and the Mettle CLI. It adds no mappings and uses your configured vim-test
output strategy, including Vimux or Neovim terminal splits.

Add this to your existing Mettle configuration module, or to `init.lua`. Replace
the paths with your checkout and executable locations:

```lua
local function setup_vim_test()
  vim.opt.runtimepath:append("/path/to/mettle/util/plugin/neovim")
  require("mettle_vim_test").setup({
    executable = "/path/to/mettle/target/debug/mettle", -- or "mettle" on PATH
  })

  -- Optional: hide the duplicate command echo for all vim-test runners.
  vim.g["test#echo_command"] = 0
end

-- lazy.nvim resets runtimepath during startup; restore the adapter afterwards.
vim.api.nvim_create_autocmd("User", {
  group = vim.api.nvim_create_augroup("MettleVimTest", { clear = true }),
  pattern = "LazyDone",
  once = true,
  callback = setup_vim_test,
})
setup_vim_test()
```

The executable is a path, not a shell command. Use the same binary as your Mettle
LSP configuration. No change to your existing vim-test mappings is needed.

The example works whether it loads before or after lazy.nvim. The `LazyDone`
callback restores the runtime path if lazy resets it, avoiding
`Unknown function: test#mettle#mettle#test_file`. If you do not use lazy.nvim,
you can omit the autocmd and keep the `setup_vim_test()` call.

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

To avoid vim-test printing a second copy of the shell command before the output,
set `vim.g["test#echo_command"] = 0` in your vim-test configuration. This setting
applies to all vim-test runners. With Vimux, the shell command may still be
visible in scrollback; the default screen clearing keeps the current results tidy.

## Verify the adapter

Build the CLI, then run the integration checks with a local vim-test checkout:

```sh
cargo build -p mettle-cli
METTLE_VIM_TEST_PATH=/path/to/vim-test nvim --headless -u NONE -i NONE \
  -l util/plugin/neovim/test/integration.lua
```

The checks use temporary local fixtures and make no network requests.
