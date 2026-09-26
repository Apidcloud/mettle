local M = {}

local source = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p")
local plugin_root = vim.fs.dirname(vim.fs.dirname(source))
local grammar_root = vim.fs.normalize(vim.fs.joinpath(plugin_root, "..", "tree-sitter"))

-- Call from nvim-treesitter's config callback, before configs.setup(). Keeping
-- queries on runtimepath also lets Neovim load them after parser installation.
function M.setup()
  local ok, parsers = pcall(require, "nvim-treesitter.parsers")
  if not ok or type(parsers.get_parser_configs) ~= "function" then
    error("Mettle Tree-sitter setup requires nvim-treesitter's master branch")
  end
  if vim.fn.filereadable(vim.fs.joinpath(grammar_root, "src", "parser.c")) ~= 1 then
    error("Mettle Tree-sitter grammar not found at " .. grammar_root)
  end

  parsers.get_parser_configs().mettle = {
    install_info = {
      url = grammar_root,
      files = { "src/parser.c", "src/scanner.c" },
      generate_requires_npm = false,
      requires_generate_from_grammar = false,
    },
    filetype = "mettle",
  }
  vim.filetype.add({ extension = { mettle = "mettle" } })
  if not vim.tbl_contains(vim.opt.runtimepath:get(), grammar_root) then
    vim.opt.runtimepath:append(grammar_root)
  end
end

return M
