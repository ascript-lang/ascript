# AScript Editor Integrations

First-class language support for [AScript](../README.md) in **VS Code**, **Zed**, and **Neovim**.
Each integration launches the AScript language server (`ascript lsp`, stdio) and adds syntax
highlighting and formatting (`ascript fmt`).

| Editor  | Path              | Syntax                         | LSP launch        | Format            |
|---------|-------------------|--------------------------------|-------------------|-------------------|
| VS Code | `vscode/`         | TextMate + server semanticTokens | `ascript lsp`     | LSP formatting    |
| Zed     | `zed/`            | tree-sitter (shared queries)   | `ascript lsp`     | LSP formatting    |
| Neovim  | `nvim/`           | nvim-treesitter (shared queries) | `ascript lsp`     | LSP / `conform.nvim` (`ascript fmt`) |

## Binary distribution (shared policy)

1. **Default:** discover `ascript` on the user's `PATH`.
2. **Override:** the `ascript.server.path` setting (an absolute path to the binary).
3. **Optional (enhancement):** a per-platform prebuilt download, checksum-verified, cached in
   the extension's global storage. Off by default; enabled by `ascript.server.autoDownload`.

## Minimum server version

All three extensions pin a **minimum server version of `0.6.0`**. The server reports its version
in the `initialize` response (`serverInfo.version`); a client that connects to an older server
warns the user to upgrade. Bump this constant in lockstep across `vscode/src/serverPath.ts`,
`zed/src/ascript.rs`, and `nvim/lua/ascript/init.lua` when the server's released version changes.

## Language identity

- Language id: `ascript`
- File extension: `.as`
- Root markers: `ascript.toml`, `.git`

## Tree-sitter grammar & queries

The grammar and the full query set (`highlights/injections/locals/folds/indents/textobjects/
tags/brackets`) come from the **promoted standalone `tree-sitter-ascript`** (LSP Phase 5). Zed and
Neovim copy the queries from that grammar's `queries/` directory; VS Code uses a TextMate grammar
plus the server's semantic tokens (VS Code has no public tree-sitter API for third-party languages).
