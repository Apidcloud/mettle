local M = {}
local executable = "mettle"
local parameters = {}

function M.setup(options)
  executable = (options or {}).executable or "mettle"
  local runners = vim.g["test#custom_runners"] or {}
  runners.Mettle = runners.Mettle or {}
  if not vim.tbl_contains(runners.Mettle, "Mettle") then
    table.insert(runners.Mettle, "Mettle")
  end
  vim.g["test#custom_runners"] = runners
end

function M.executable()
  return executable
end

local function canonical(path)
  return vim.uv.fs_realpath(path) or vim.fn.fnamemodify(path, ":p")
end

local function require_saved(file)
  local root = vim.fs.root(file, "mettle.toml")
  for _, buffer in ipairs(vim.api.nvim_list_bufs()) do
    local name = canonical(vim.api.nvim_buf_get_name(buffer))
    if vim.bo[buffer].modified and name:match("%.mettle$")
      and (name == file or (root and vim.startswith(name, root .. "/"))) then
      error("Mettle: save changes before running: " .. name, 0)
    end
  end
end

function M.build_position(kind, position)
  parameters = {}
  if kind ~= "nearest" and kind ~= "file" then
    error("Mettle: project-wide suites are not supported; use :TestFile or :TestNearest", 0)
  end

  local file = canonical(position.file)
  require_saved(file)
  local result = vim.system({ executable, "list", file, "--json" }, { text = true }):wait()
  if result.code ~= 0 then
    error("Mettle discovery failed:\n" .. (result.stderr or result.stdout or ""), 0)
  end
  local ok, discovered = pcall(vim.json.decode, result.stdout)
  if not ok or type(discovered) ~= "table" then
    error("Mettle: invalid JSON from `mettle list`", 0)
  end

  local declarations, tests = {}, 0
  for _, group in ipairs({ { "flows", "run" }, { "tests", "test" } }) do
    for _, declaration in ipairs(discovered[group[1]] or {}) do
      if canonical(declaration.path) == file then
        declaration.command = group[2]
        table.insert(declarations, declaration)
        if group[2] == "test" then tests = tests + 1 end
      end
    end
  end

  local quoted_file = vim.fn.shellescape(file)
  if kind == "file" then
    if tests > 0 then return { "test", quoted_file } end
    return { "run", quoted_file, "--all" }
  end

  table.sort(declarations, function(a, b)
    return a.line < b.line or (a.line == b.line and a.column < b.column)
  end)
  -- Vim columns count bytes; the compiler reports Unicode character columns.
  local source_line = vim.fn.readfile(file, "", position.line)[position.line] or ""
  local column = vim.fn.strchars(source_line:sub(1, position.col - 1)) + 1
  local selected
  for _, declaration in ipairs(declarations) do
    if declaration.line < position.line
      or (declaration.line == position.line and declaration.column <= column) then
      selected = declaration
    end
  end
  -- Above the first declaration, run the first one.
  selected = selected or declarations[1]
  if not selected then error("Mettle: no runnable flows or tests in this file", 0) end

  parameters = selected.parameters or {}
  local same_line = 0
  for _, declaration in ipairs(declarations) do
    if declaration.command == selected.command and declaration.line == selected.line then
      same_line = same_line + 1
    end
  end
  if same_line > 1 then
    if selected.command == "test" then
      return { "test", quoted_file, vim.fn.shellescape(selected.name) }
    end
    return { "run", quoted_file, "--flow-id", tostring(selected.id) }
  end
  return { selected.command, quoted_file, "--line", tostring(selected.line) }
end

function M.build_args(args)
  -- vim-test prepends user/runner options; Mettle requires command and file first.
  local command_index
  for index = #args - 1, 1, -1 do
    if args[index] == "run" or args[index] == "test" then
      command_index = index
      break
    end
  end
  if not command_index then error("Mettle: missing run/test command", 0) end
  local command = table.remove(args, command_index)
  local file = table.remove(args, command_index)
  table.insert(args, 1, file)
  table.insert(args, 1, command)

  local supplied = {}
  for index, argument in ipairs(args) do
    if argument == "--arg" and args[index + 1] then
      local name = args[index + 1]:match("^([^=]+)=")
      if name then supplied[name] = true end
    end
  end
  local needed = parameters
  parameters = {}
  for _, parameter in ipairs(needed) do
    if not supplied[parameter] then
      local value = vim.fn.input({
        prompt = "Mettle " .. parameter .. " (CLI value; Esc cancels): ",
        cancelreturn = "\027",
      })
      if value == "\027" then error("Mettle: run cancelled", 0) end
      table.insert(args, "--arg")
      table.insert(args, vim.fn.shellescape(parameter .. "=" .. value))
    end
  end
  return args
end

return M
