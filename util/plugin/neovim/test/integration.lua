local root = vim.fn.getcwd()
local vim_test = assert(vim.env.METTLE_VIM_TEST_PATH, "Set METTLE_VIM_TEST_PATH to a vim-test checkout")
vim.opt.runtimepath:append(vim_test)
vim.opt.runtimepath:append(root .. "/util/plugin/neovim")
local adapter = require("mettle_vim_test")
adapter.setup({ executable = root .. "/target/debug/mettle" })
adapter.setup({ executable = root .. "/target/debug/mettle" })
assert(#vim.g["test#custom_runners"].Mettle == 1, "setup must be idempotent")
vim.cmd("runtime plugin/test.vim")
vim.cmd([[
function! CaptureMettleCommand(command) abort
  let g:mettle_captured_command = a:command
endfunction
let g:test#custom_strategies = {'capture': function('CaptureMettleCommand')}
let g:test#strategy = 'capture'
]])

local directory = vim.fn.tempname()
vim.fn.mkdir(directory, "p")
local checks = 0
local function check(condition, message)
  assert(condition, message)
  checks = checks + 1
end
local function fixture(name, lines)
  local path = directory .. "/" .. name
  vim.fn.writefile(lines, path)
  return path
end
local function open(path, line, column)
  vim.cmd.edit(vim.fn.fnameescape(path))
  vim.api.nvim_win_set_cursor(0, { line or 1, column or 0 })
end
local function run(command)
  vim.g.mettle_captured_command = nil
  vim.cmd(command)
  return assert(vim.g.mettle_captured_command, "strategy was not called")
end
local function execute(command)
  local output = vim.fn.system(command)
  check(vim.v.shell_error == 0, output)
  return output
end
local function rejected(command, expected)
  vim.g.mettle_captured_command = nil
  local ok, error = pcall(vim.cmd, command)
  check(not ok and tostring(error):find(expected, 1, true), tostring(error))
  check(vim.g.mettle_captured_command == nil, "rejected command must not execute")
end

local original_input = vim.fn.input
local ok, failure = xpcall(function()
  local flows = fixture("flows 'quoted' $fixture.mettle", {
    '// flow decoy() { echo("comment") }',
    'flow first() { return "first" }',
    '',
    'flow {',
    '  return "anonymous"',
    '}',
    'flow parameterized(value) { return value }',
  })
  check(vim.fn["test#determine_runner"](flows) == "mettle#mettle", "runner detection")
  check(vim.fn["test#mettle#mettle#test_file"]("other.rs") == 0, "other extensions")
  open(flows, 5)
  local nearest = run("TestNearest")
  check(nearest:find("--line 4", 1, true), "body cursor must resolve to declaration line")
  check(execute(nearest):find("anonymous", 1, true), "anonymous flow output")
  open(flows, 1)
  check(run("TestNearest"):find("--line 2", 1, true), "comment must not be selected")
  local all = execute(run("TestFile --no-progress"))
  check(all:find("Skipped: 1", 1, true), "--all skips parameterized flows")

  open(flows, 7)
  vim.fn.input = function() return "literal 'quote' $(printf INJECTED)" end
  local parameterized = run("TestNearest --verbose")
  check(execute(parameterized):find("$(printf INJECTED)", 1, true), "prompt values must be shell escaped")
  vim.fn.input = function() error("should not prompt for supplied parameters") end
  check(execute(run("TestNearest --arg value=123")):find("123", 1, true), "explicit parameter")
  vim.fn.input = function() return "\027" end
  rejected("TestNearest", "run cancelled")
  vim.fn.input = original_input

  local mixed = fixture("mixed.mettle", {
    'flow helper() { return 42 }',
    'test("first check") {',
    '  assert(helper() == 42)',
    '}',
    'test("second check") { assert(true) }',
  })
  open(mixed, 3)
  local test_command = run("TestNearest")
  check(test_command:find(" test ", 1, true) and test_command:find("--line 2", 1, true), "nearest test")
  execute(test_command)
  local tests = run("TestFile --verbose")
  check(tests:find(" test ", 1, true) and not tests:find("--all", 1, true), "mixed file runs tests")
  local output = execute(tests)
  check(output:find("first check", 1, true) and output:find("second check", 1, true), "all tests execute")

  vim.cmd.enew()
  vim.cmd.cd(vim.fn.fnameescape(directory))
  check(run("TestLast") == tests, "last command retained outside source buffer and original cwd")
  execute(vim.g.mettle_captured_command)
  vim.cmd("TestVisit")
  check(vim.uv.fs_realpath(vim.api.nvim_buf_get_name(0)) == vim.uv.fs_realpath(mixed)
    and vim.fn.line(".") == 3, "visit restores location: " .. vim.inspect({
      actual = vim.api.nvim_buf_get_name(0), expected = mixed, line = vim.fn.line("."),
    }))
  vim.cmd.cd(vim.fn.fnameescape(root))
  rejected("TestSuite", "project-wide suites")
  vim.api.nvim_buf_set_lines(0, 0, 0, false, { "// unsaved" })
  rejected("TestNearest", "save changes before running")
  vim.cmd("edit!")

  local same_line = fixture("same-line.mettle", {
    'flow one() { return "one" } flow two() { return "two" }',
    'test("one") { assert(true) } test("two") { assert(true) }',
  })
  open(same_line, 1, 35)
  check(execute(run("TestNearest")):find("two", 1, true), "same-line flow selection")
  open(same_line, 2, 35)
  local selected_test = execute(run("TestNearest"))
  check(selected_test:find("two", 1, true) and not selected_test:find("Test 2", 1, true), "same-line test selection")

  local unicode_line = 'flow unicode() { return "éééééééééé" }     flow later() { return 2 }'
  local unicode = fixture("unicode.mettle", { unicode_line })
  open(unicode, 1, unicode_line:find("}", 1, true) - 1)
  check(run("TestNearest"):find("--flow-id 0", 1, true), "Unicode columns must not select the next flow")

  vim.cmd([[
  function! VimuxRunCommand(command) abort
    let g:mettle_vimux_command = a:command
  endfunction
  ]])
  vim.cmd("TestNearest -strategy=vimux")
  check(vim.g.mettle_vimux_command:find("--flow-id 0", 1, true), "Vimux receives Mettle command")

  -- Project discovery includes other files; they must not influence file/nearest selection.
  fixture("mettle.toml", {})
  local dependency = fixture("dependency.mettle", { 'test("dependency only") { assert(true) }' })
  open(flows, 5)
  check(run("TestFile"):find("--all", 1, true), "dependency tests must not switch file mode")
  check(run("TestNearest"):find("--line 4", 1, true), "dependency declarations excluded")
  local dependency_buffer = vim.fn.bufadd(dependency)
  vim.fn.bufload(dependency_buffer)
  vim.api.nvim_buf_set_lines(dependency_buffer, 0, 0, false, { "// unsaved dependency" })
  rejected("TestFile", "save changes before running")
  vim.bo[dependency_buffer].modified = false
  local invalid = fixture("invalid.mettle", { 'flow broken( {' })
  open(invalid)
  rejected("TestNearest", "discovery failed")
end, debug.traceback)

vim.fn.input = original_input
vim.cmd.cd(vim.fn.fnameescape(root))
vim.fn.delete(directory, "rf")
if not ok then
  io.stderr:write(failure .. "\n")
  vim.cmd("cquit 1")
end
print(string.format("Mettle vim-test integration: %d checks passed", checks))
vim.cmd("qa!")
