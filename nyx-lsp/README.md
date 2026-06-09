# nyx-lsp

The Language Server for [Nyx](../README.md).

## Features

- **Inlay hints**: inferred types after `let` bindings, kept stable while you edit.
- **Semantic tokens**: syntactic highlighting that works even on a buffer that does not parse yet.
- **Document symbols**: the outline of functions, structs and enums.
- **Go-to-definition**: basic resolution (work in progress).
- **Load progress**: a `$/progress` spinner while the project (and std) loads.

## Build

```sh
cargo build --release -p nyx-lsp
# binary at: target/release/nyx-lsp
```

The server needs to find the Nyx standard library. It resolves it in this order:

1. the `NYX_STD_PATH` environment variable,
2. `<binary_dir>/std/`,
3. `std/` relative to the working directory.

Setting `NYX_STD_PATH` in your editor config is the reliable option.

## Neovim

Neovim 0.9+ (using [`nvim-lspconfig`](https://github.com/neovim/nvim-lspconfig)).
Point `cmd` at the built binary and `NYX_STD_PATH` at this repo's `std/`:

```lua
vim.filetype.add({ extension = { nyx = "nyx" } })

local lspconfig = require("lspconfig")
local configs = require("lspconfig.configs")

if not configs.nyx_lsp then
  configs.nyx_lsp = {
    default_config = {
      cmd = { "/path/to/nyx/target/release/nyx-lsp" },
      cmd_env = { NYX_STD_PATH = "/path/to/nyx/std" },
      filetypes = { "nyx" },
      root_dir = lspconfig.util.root_pattern(".git"),
      single_file_support = true,
    },
  }
end

lspconfig.nyx_lsp.setup({})
```

## Vim

Vim has no built-in LSP client, use a plugin such as
[`vim-lsp`](https://github.com/prabirshrestha/vim-lsp):

```vim
augroup nyx_lsp
  autocmd!
  autocmd BufRead,BufNewFile *.nyx setfiletype nyx
  autocmd User lsp_setup call lsp#register_server({
    \ 'name': 'nyx-lsp',
    \ 'cmd': {server_info->['/path/to/nyx/target/release/nyx-lsp']},
    \ 'allowlist': ['nyx'],
    \ })
augroup END
```

Export `NYX_STD_PATH` in your shell so the server finds the standard library
(`export NYX_STD_PATH=/path/to/nyx/std`).
