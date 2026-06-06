# AScript for Zed

AScript language support for [Zed](https://zed.dev): tree-sitter highlighting and the `ascript`
language server.

## Requirements

Install `ascript` and ensure it is on your shell `PATH` (e.g. `~/.local/bin`). Zed loads your
login-shell environment, so the extension then launches `ascript lsp` automatically — nothing to
configure.

If you must point Zed at an absolute binary, set **both** `path` and `arguments` — overriding
`binary.path` makes Zed launch the binary directly with `arguments` (which defaults to empty), so
you must pass `["lsp"]` yourself:

```json
{
  "lsp": {
    "ascript": {
      "binary": { "path": "/absolute/path/to/ascript", "arguments": ["lsp"] }
    }
  }
}
```

> ⚠️ `binary.path` **without** `arguments: ["lsp"]` launches bare `ascript`, which prints CLI
> help instead of speaking LSP — Zed reports "Server reset the connection" (you get highlighting
> but no diagnostics/hover/navigation). The `arguments` field is required with `binary.path`.

## Install

Until this extension is in the Zed registry, install it as a dev extension:
**zed: install dev extension** → select `editors/zed`.
