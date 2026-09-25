-- Run from the grammar directory with METTLE_TS_PARSER pointing to a compiled
-- parser: nvim --headless -u NONE -i NONE -l scripts/check-neovim.lua
local parser_path = assert(vim.env.METTLE_TS_PARSER, "Set METTLE_TS_PARSER to the compiled parser")
vim.treesitter.language.add("mettle", { path = parser_path })
for _, name in ipairs({ "highlights", "folds", "indents" }) do
  vim.treesitter.query.set("mettle", name, table.concat(vim.fn.readfile("queries/" .. name .. ".scm"), "\n"))
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
print("Neovim: parser, highlight captures, query loading, and incremental edits passed.")
