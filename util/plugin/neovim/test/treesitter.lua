-- Run with a compiled parser and a local nvim-treesitter master checkout.
-- This check does not load or change user config, and works from any directory.
local source = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p")
local plugin_root = vim.fs.dirname(vim.fs.dirname(source))
local grammar_root = vim.fs.normalize(vim.fs.joinpath(plugin_root, "..", "tree-sitter"))
local dependency = assert(vim.env.METTLE_TREESITTER_PATH, "Set METTLE_TREESITTER_PATH to nvim-treesitter master")
local parser_path = assert(vim.env.METTLE_TS_PARSER, "Set METTLE_TS_PARSER to the compiled parser")

-- Isolate query discovery from personal config and run setup before loading
-- the parser, as on the first installation. Repeated setup must be harmless.
vim.opt.runtimepath:remove(vim.fn.stdpath("config"))
vim.opt.runtimepath:remove(vim.fn.stdpath("config") .. "/after")
vim.opt.runtimepath:prepend(dependency)
vim.opt.runtimepath:prepend(plugin_root)
local integration = require("mettle_treesitter")
local original_cwd = vim.fn.getcwd()
vim.cmd.cd(vim.fn.fnamemodify(vim.fn.tempname(), ":h"))
integration.setup()
integration.setup()
vim.cmd.cd(original_cwd)
local registration = require("nvim-treesitter.parsers").get_parser_configs().mettle
assert(registration.install_info.url == grammar_root, "Parser path must be relative to the plugin")
assert(vim.deep_equal(registration.install_info.files, { "src/parser.c", "src/scanner.c" }))
assert(vim.filetype.match({ filename = "example.mettle" }) == "mettle")
local count = 0
for _, path in ipairs(vim.opt.runtimepath:get()) do
  if path == grammar_root then count = count + 1 end
end
assert(count == 1, "Setup must not duplicate the grammar runtime path")

vim.treesitter.language.add("mettle", { path = parser_path })
for _, name in ipairs({ "highlights", "folds", "indents" }) do
  local files = vim.treesitter.query.get_files("mettle", name)
  assert(#files == 1 and files[1] == grammar_root .. "/queries/mettle/" .. name .. ".scm",
    "Queries must load directly from the shared grammar: " .. name)
  assert(vim.treesitter.query.get("mettle", name), "Missing query " .. name)
end

local buf = vim.api.nvim_create_buf(false, true)
vim.api.nvim_set_current_buf(buf)
local source = {
  "flow fetchUser(user) {",
  '  response = http.get("/${user.name}") { timeout: 5s }',
  "  // a comment",
  "  return response.status == 200 and not false",
  "}",
  "flow next() = true",
}
vim.api.nvim_buf_set_lines(buf, 0, -1, false, source)
local parser = vim.treesitter.get_parser(buf, "mettle")
assert(not parser:parse()[1]:root():has_error())
vim.treesitter.start(buf, "mettle")

local query = assert(vim.treesitter.query.get("mettle", "highlights"))
local captures = {}
for id, node in query:iter_captures(parser:parse()[1]:root(), buf, 0, -1) do
  local text = vim.treesitter.get_node_text(node, buf)
  captures[query.captures[id] .. ":" .. text] = true
end
for _, capture in ipairs({
  "keyword:flow", "function:fetchUser", "variable.parameter:user",
  "function.call:get", "variable:user.name", "property:timeout",
  "number:5s", "comment:// a comment", "variable.member:status",
  "keyword.operator:and", "keyword.operator:not", "boolean:false",
}) do
  assert(captures[capture], "Missing highlight capture " .. capture)
end

local function snapshot(node)
  local parts = { node:type(), tostring(node:has_error()), table.concat({ node:range() }, ",") }
  for child in node:iter_children() do
    parts[#parts + 1] = snapshot(child)
  end
  return table.concat(parts, "|")
end

local function check_edit(row, first, last, replacement)
  vim.api.nvim_buf_set_text(buf, row, first, row, last, replacement)
  local incremental = parser:parse()[1]:root()
  local text = table.concat(vim.api.nvim_buf_get_lines(buf, 0, -1, false), "\n") .. "\n"
  local fresh = vim.treesitter.get_string_parser(text, "mettle"):parse()[1]:root()
  assert(snapshot(incremental) == snapshot(fresh), "Incremental parse differs from fresh parse")
  return incremental
end

-- Change a token's length, break/repair interpolation, insert/delete a newline,
-- and remove/restore a block delimiter, reusing the same live buffer parser.
assert(not check_edit(1, 50, 52, { "100ms" }):has_error())
assert(check_edit(1, 35, 36, { "" }):has_error())
assert(not check_edit(1, 35, 35, { "}" }):has_error())
check_edit(3, 25, 25, { "", "    " })
vim.api.nvim_buf_set_text(buf, 3, 25, 4, 4, { "" })
check_edit(3, 0, 0, { "" })
assert(check_edit(4, 0, 1, { "" }):has_error())
assert(not check_edit(4, 0, 0, { "}" }):has_error())

-- The first braces belong to the loop, even with another object on the next
-- line. Verify ranges as well as fresh/incremental equivalence after edits.
vim.api.nvim_buf_set_lines(buf, 0, -1, false, {
  "flow f = [1]",
  "flow main {",
  "  for x in f() {}",
  "  {}",
  '  assert(true, ("yes"))',
  "}",
})
local function check_loop_boundaries(root)
  assert(not root:has_error())
  local body = root:named_child(1):field("body")[1]
  assert(body:named_child_count() == 3, "Loop must not consume the following object")
  local loop = body:named_child(0):named_child(0)
  assert(loop:type() == "for_expression")
  assert(#loop:field("iterable")[1]:field("options") == 0, "Iterable call must not take options")
  local first_row, _, last_row = loop:field("body")[1]:range()
  assert(first_row == 2 and last_row == 2, "Loop body must use the first braces")
  assert(body:named_child(1):named_child(0):type() == "object")
end
check_loop_boundaries(check_edit(2, 11, 12, { "f" }))
check_loop_boundaries(check_edit(2, 11, 12, { "(f)" }))
check_loop_boundaries(check_edit(2, 11, 14, { "((f))" }))
assert(check_edit(2, 11, 16, { "((f)" }):has_error())
check_loop_boundaries(check_edit(2, 11, 15, { "((f))" }))
check_loop_boundaries(check_edit(4, 15, 22, { '(("yes"))' }))
assert(check_edit(4, 17, 22, { "123" }):has_error())
check_loop_boundaries(check_edit(4, 17, 20, { '"yes"' }))
print("Neovim: setup, parser registration, shared queries, highlights, and incremental edits passed.")
