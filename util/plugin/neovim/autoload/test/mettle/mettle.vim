function! test#mettle#mettle#test_file(file) abort
  return a:file =~# '\.mettle$'
endfunction

function! test#mettle#mettle#build_position(type, position) abort
  return luaeval("require('mettle_vim_test').build_position(_A[1], _A[2])", [a:type, a:position])
endfunction

function! test#mettle#mettle#build_args(args) abort
  return luaeval("require('mettle_vim_test').build_args(_A)", a:args)
endfunction

function! test#mettle#mettle#executable() abort
  return luaeval("vim.fn.shellescape(require('mettle_vim_test').executable())")
endfunction
