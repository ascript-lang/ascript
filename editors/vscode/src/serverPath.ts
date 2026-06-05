import * as vscode from "vscode";
import * as fs from "fs";
import * as path from "path";
import * as os from "os";
import * as https from "https";
import * as crypto from "crypto";

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
    "Could not find the `ascript` language server on your PATH. " +
      "Install AScript, set `ascript.server.path`, or enable `ascript.server.autoDownload`.",
  );
  return undefined;
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
  const dirs = (process.env.PATH ?? "").split(path.delimiter);
  for (const dir of dirs) {
    if (!dir) {
      continue;
    }
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
