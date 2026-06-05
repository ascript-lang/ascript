# AScript for Zed

AScript language support for [Zed](https://zed.dev): tree-sitter highlighting and the `ascript`
language server.

## Requirements

Install `ascript` and ensure it is on your `PATH`. To use a custom path, add to your Zed settings:

```json
{
  "lsp": {
    "ascript": {
      "binary": { "path": "/absolute/path/to/ascript" }
    }
  }
}
```

## Install

Until this extension is in the Zed registry, install it as a dev extension:
**zed: install dev extension** → select `editors/zed`.
