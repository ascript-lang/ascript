# Icon asset

The extension currently ships **without** an icon and uses VS Code's default file icon, so
`vsce package` succeeds with no icon present.

To re-enable icons later, add a 128×128 `ascript.png` here and restore both references in
`package.json`:

- the top-level `"icon": "icons/ascript.png"` (the Marketplace gallery icon), and
- the per-language `contributes.languages[].icon` block (`{ "light": "./icons/ascript.png",
  "dark": "./icons/ascript.png" }`, the file-explorer icon).
