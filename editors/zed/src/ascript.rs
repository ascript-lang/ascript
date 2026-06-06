//! Zed extension shim for AScript. Registers the `ascript lsp` language server.
//! Built to `wasm32-wasip2` (verify the target triple against current Zed docs;
//! fall back to `wasm32-wasip1` if the registry build requires it).

use zed_extension_api::{self as zed, LanguageServerId, Result};

/// Minimum AScript server version this extension targets. Keep in lockstep with
/// editors/README.md and the VS Code / Neovim integrations.
const MIN_SERVER_VERSION: &str = "0.6.0";

struct AScriptExtension {
    /// Cached resolved path to the `ascript` binary (PATH lookup is not free).
    cached_path: Option<String>,
}

impl AScriptExtension {
    /// Resolve the `ascript` binary: a user `binary.path` setting first, else PATH.
    fn server_binary_path(
        &mut self,
        id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        // 1) Honor an explicit setting: { "ascript": { "binary": { "path": "..." } } }.
        if let Ok(settings) = zed::settings::LspSettings::for_worktree("ascript", worktree) {
            if let Some(binary) = settings.binary {
                if let Some(path) = binary.path {
                    return Ok(path);
                }
            }
        }

        // 2) Cached PATH result.
        if let Some(path) = &self.cached_path {
            return Ok(path.clone());
        }

        // 3) Discover `ascript` on PATH via the worktree.
        zed::set_language_server_installation_status(
            id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );
        let path = worktree
            .which("ascript")
            .ok_or_else(|| {
                // A GUI-launched Zed may not inherit your shell PATH, so a binary in
                // ~/.local/bin can be invisible to `which`. The extension runs in a WASM
                // sandbox and cannot probe arbitrary paths, so point at the explicit setting.
                format!(
                    "could not find `ascript` on PATH. Install AScript (>= {MIN_SERVER_VERSION}), \
                     or set `lsp.ascript.binary.path` to its absolute path in your Zed settings. \
                     (A GUI-launched editor may not see your shell PATH; an absolute path avoids this.)"
                )
            })?;
        self.cached_path = Some(path.clone());
        Ok(path)
    }
}

impl zed::Extension for AScriptExtension {
    fn new() -> Self {
        AScriptExtension { cached_path: None }
    }

    fn language_server_command(
        &mut self,
        id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let command = self.server_binary_path(id, worktree)?;
        Ok(zed::Command {
            command,
            args: vec!["lsp".to_string()],
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(AScriptExtension);
