import * as vscode from "vscode";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as https from "https";
import * as crypto from "crypto";
import { execFile } from "child_process";
import { promisify } from "util";

const execFileAsync = promisify(execFile);

export interface ServerValidation {
  ok: boolean;
  version?: string;
  reason?: string;
}

/** Pre-flight the resolved binary BEFORE starting the language client, so a binary
 *  that cannot serve LSP produces a clear, actionable error instead of a silent
 *  spawn → exit → restart loop (which manifests as the status bar "blinking" with no
 *  diagnostics or navigation). The common cause is a build without the `lsp` Cargo
 *  feature, whose `ascript lsp` answers `error: unrecognized subcommand 'lsp'`. */
export async function validateServer(command: string): Promise<ServerValidation> {
  let help: string;
  try {
    const { stdout } = await execFileAsync(command, ["--help"], { timeout: 5000 });
    help = stdout;
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, reason: `\`${command} --help\` failed to run: ${msg}` };
  }
  // The `lsp` subcommand only exists when AScript is built with the `lsp` feature
  // (on by default). Match it as a subcommand line in `--help` output.
  if (!/^\s*lsp\b/m.test(help)) {
    return {
      ok: false,
      reason:
        `the \`ascript\` binary at '${command}' has no \`lsp\` subcommand — it was built without ` +
        `the \`lsp\` feature. Reinstall AScript with default features ` +
        `(e.g. \`cargo install --path .\` or \`cargo build --release\`), then restart the server.`,
    };
  }
  let version: string | undefined;
  try {
    const { stdout } = await execFileAsync(command, ["--version"], { timeout: 5000 });
    version = stdout.trim().split(/\s+/).pop();
  } catch {
    // version is best-effort; a working --help is enough to proceed
  }
  return { ok: true, version };
}

/** The minimum AScript server version this extension supports. Keep in lockstep
 *  with editors/README.md and the other two integrations. */
export const MIN_SERVER_VERSION = "0.6.0";

/** Resolve the path to the `ascript` server binary.
 *  Order: (1) `ascript.server.path` setting, (2) `ascript` on PATH,
 *  (3) optional checksum-verified download if `ascript.server.autoDownload`. */
export async function resolveServerPath(
  context: vscode.ExtensionContext,
): Promise<string | undefined> {
  const cfg = vscode.workspace.getConfiguration("ascript");

  const explicit = cfg.get<string>("server.path", "").trim();
  if (explicit) {
    if (isExecutable(explicit)) {
      return explicit;
    }
    vscode.window.showErrorMessage(
      `ascript.server.path points at '${explicit}', which is not an executable file.`,
    );
    return undefined;
  }

  const onPath = findOnPath("ascript");
  if (onPath) {
    return onPath;
  }

  if (cfg.get<boolean>("server.autoDownload", false)) {
    return await downloadServer(context);
  }

  vscode.window.showErrorMessage(
    "Could not find the `ascript` language server. Set `ascript.server.path` to its absolute path, " +
      "add its directory (e.g. `~/.local/bin`) to your PATH, or enable `ascript.server.autoDownload`. " +
      "Tip: on macOS an editor launched from the Dock/Finder may not inherit your shell's PATH.",
  );
  return undefined;
}

/** Common install locations a GUI-launched editor's PATH often omits — the classic
 *  macOS "VS Code started from the Dock has a stripped PATH" problem. Searching these
 *  lets the extension still find a user-installed `ascript` (e.g. in `~/.local/bin` or
 *  `~/.cargo/bin`) without requiring an explicit `ascript.server.path`. */
function commonBinDirs(): string[] {
  const home = os.homedir();
  const dirs = [
    path.join(home, ".local", "bin"),
    path.join(home, ".cargo", "bin"),
    path.join(home, "bin"),
  ];
  if (process.platform !== "win32") {
    dirs.push("/usr/local/bin", "/opt/homebrew/bin", "/usr/bin");
  }
  return dirs;
}

function isExecutable(p: string): boolean {
  try {
    fs.accessSync(p, fs.constants.X_OK);
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}

/** Search each PATH entry for an executable `name` (handles `.exe` on Windows). */
function findOnPath(name: string): string | undefined {
  const exeNames = process.platform === "win32" ? [`${name}.exe`, name] : [name];
  // Search the inherited PATH first, then common user/system bin dirs the GUI PATH
  // frequently misses (de-duplicated, order-preserving).
  const seen = new Set<string>();
  const dirs = [...(process.env.PATH ?? "").split(path.delimiter), ...commonBinDirs()];
  for (const dir of dirs) {
    if (!dir || seen.has(dir)) {
      continue;
    }
    seen.add(dir);
    for (const exe of exeNames) {
      const candidate = path.join(dir, exe);
      if (isExecutable(candidate)) {
        return candidate;
      }
    }
  }
  return undefined;
}

/** Compare semver-ish "major.minor.patch" strings. Returns <0, 0, or >0. */
export function compareVersions(a: string, b: string): number {
  const pa = a.split(".").map((n) => parseInt(n, 10) || 0);
  const pb = b.split(".").map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < 3; i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d !== 0) {
      return d;
    }
  }
  return 0;
}

/** Warn if the connected server is older than the pinned minimum. */
export function checkServerVersion(reported: string | undefined): void {
  if (!reported) {
    return;
  }
  if (compareVersions(reported, MIN_SERVER_VERSION) < 0) {
    vscode.window.showWarningMessage(
      `The AScript language server is version ${reported}, but this extension expects ` +
        `at least ${MIN_SERVER_VERSION}. Please upgrade AScript.`,
    );
  }
}

/** Per-platform prebuilt release asset name. */
function assetName(): string | undefined {
  const platform = process.platform;
  const arch = process.arch;
  const triple =
    platform === "darwin"
      ? arch === "arm64"
        ? "aarch64-apple-darwin"
        : "x86_64-apple-darwin"
      : platform === "linux"
        ? arch === "arm64"
          ? "aarch64-unknown-linux-gnu"
          : "x86_64-unknown-linux-gnu"
        : platform === "win32"
          ? "x86_64-pc-windows-msvc"
          : undefined;
  if (!triple) {
    return undefined;
  }
  return platform === "win32" ? `ascript-${triple}.exe` : `ascript-${triple}`;
}

/** Optional: download a checksum-verified prebuilt server into global storage. */
async function downloadServer(
  context: vscode.ExtensionContext,
): Promise<string | undefined> {
  const asset = assetName();
  if (!asset) {
    vscode.window.showErrorMessage(
      `No prebuilt AScript server is available for ${process.platform}/${process.arch}.`,
    );
    return undefined;
  }
  const base = `https://github.com/ascript-lang/ascript/releases/download/v${MIN_SERVER_VERSION}`;
  const url = `${base}/${asset}`;
  const sumUrl = `${url}.sha256`;

  const dir = context.globalStorageUri.fsPath;
  fs.mkdirSync(dir, { recursive: true });
  const dest = path.join(dir, process.platform === "win32" ? "ascript.exe" : "ascript");

  try {
    const expected = (await httpGetText(sumUrl)).trim().split(/\s+/)[0];
    const data = await httpGetBuffer(url);
    const actual = crypto.createHash("sha256").update(data).digest("hex");
    if (actual !== expected) {
      throw new Error(`checksum mismatch (expected ${expected}, got ${actual})`);
    }
    fs.writeFileSync(dest, data, { mode: 0o755 });
    return dest;
  } catch (e) {
    vscode.window.showErrorMessage(`Failed to download the AScript server: ${e}`);
    return undefined;
  }
}

function httpGetBuffer(url: string): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    https
      .get(url, (res) => {
        if (res.statusCode && res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          httpGetBuffer(res.headers.location).then(resolve, reject);
          return;
        }
        if (res.statusCode !== 200) {
          reject(new Error(`HTTP ${res.statusCode} for ${url}`));
          return;
        }
        const chunks: Buffer[] = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => resolve(Buffer.concat(chunks)));
      })
      .on("error", reject);
  });
}

async function httpGetText(url: string): Promise<string> {
  return (await httpGetBuffer(url)).toString("utf8");
}
