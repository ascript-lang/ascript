# AScript for VS Code

Language support for [AScript](https://github.com/ascript-lang/ascript): diagnostics, completion,
hover, go-to-definition, references, rename, formatting, and semantic highlighting — powered by the
`ascript lsp` language server.

## Requirements

Install the `ascript` binary and ensure it is on your `PATH`, or set `ascript.server.path`.

## Settings

- `ascript.server.path` — absolute path to the `ascript` binary (default: discover on `PATH`).
- `ascript.server.autoDownload` — download a checksum-verified prebuilt server if none is found.
- `ascript.trace.server` — `off` | `messages` | `verbose`.

## Commands

- **AScript: Restart Language Server**
- **AScript: Show Language Server Version**
