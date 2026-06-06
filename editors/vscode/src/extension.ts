import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";
import { resolveServerPath, checkServerVersion, validateServer } from "./serverPath";

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand("ascript.restartServer", restartServer),
    vscode.commands.registerCommand("ascript.showServerVersion", showServerVersion),
    vscode.commands.registerCommand("ascript.run", (uri?: string) => runInTerminal(context, "run", uri)),
    vscode.commands.registerCommand("ascript.runTest", (uri?: string) =>
      runInTerminal(context, "test", uri),
    ),
  );
  await startClient(context);
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
    client = undefined;
  }
}

async function startClient(context: vscode.ExtensionContext): Promise<void> {
  const command = await resolveServerPath(context);
  if (!command) {
    return; // resolveServerPath already surfaced the error
  }

  // Pre-flight: a binary that can't serve LSP (e.g. built without the `lsp` feature)
  // would otherwise spawn → exit → restart-loop silently. Fail with a clear message.
  const validation = await validateServer(command);
  if (!validation.ok) {
    vscode.window.showErrorMessage(`AScript language server cannot start: ${validation.reason}`);
    return;
  }

  const serverOptions: ServerOptions = {
    run: { command, args: ["lsp"], transport: TransportKind.stdio },
    debug: { command, args: ["lsp"], transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "ascript" }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/{*.as,ascript.toml}"),
    },
    // Honors the `ascript.trace.server` setting automatically.
  };

  client = new LanguageClient("ascript", "AScript Language Server", serverOptions, clientOptions);
  await client.start();

  const info = client.initializeResult?.serverInfo;
  checkServerVersion(info?.version);
}

/** Resolve the target `.as` file: the CodeLens passes its URI as the first
 *  argument; fall back to the active editor when invoked from the palette. */
function resolveTargetFile(uri?: string): string | undefined {
  if (uri) {
    return vscode.Uri.parse(uri).fsPath;
  }
  const doc = vscode.window.activeTextEditor?.document;
  if (doc && doc.languageId === "ascript") {
    return doc.fileName;
  }
  vscode.window.showWarningMessage("AScript: no .as file to run (open one or use a CodeLens).");
  return undefined;
}

/** Run `ascript <subcommand> <file>` in an integrated terminal. `runTest` maps
 *  to `ascript test <file>`, which runs ALL of the file's `test(...)`
 *  registrations — the CLI has no per-test name filter, so a test-name argument
 *  from the CodeLens is informational only. */
async function runInTerminal(
  context: vscode.ExtensionContext,
  subcommand: "run" | "test",
  uri?: string,
): Promise<void> {
  const file = resolveTargetFile(uri);
  if (!file) {
    return;
  }
  const binary = await resolveServerPath(context);
  if (!binary) {
    return; // resolveServerPath already surfaced the error
  }

  const terminal =
    vscode.window.terminals.find((t) => t.name === "AScript") ??
    vscode.window.createTerminal("AScript");
  terminal.show();
  terminal.sendText(`${quoteArg(binary)} ${subcommand} ${quoteArg(file)}`);
}

/** Quote a shell argument for both POSIX shells and Windows (cmd/PowerShell). */
function quoteArg(arg: string): string {
  if (process.platform === "win32") {
    return /[\s"]/.test(arg) ? `"${arg.replace(/"/g, '""')}"` : arg;
  }
  return /[^A-Za-z0-9_/.:@%+=-]/.test(arg) ? `'${arg.replace(/'/g, `'\\''`)}'` : arg;
}

async function restartServer(): Promise<void> {
  if (!client) {
    vscode.window.showWarningMessage("The AScript language server is not running.");
    return;
  }
  await client.restart();
  vscode.window.showInformationMessage("AScript language server restarted.");
}

function showServerVersion(): void {
  const info = client?.initializeResult?.serverInfo;
  if (info) {
    vscode.window.showInformationMessage(
      `AScript language server: ${info.name} ${info.version ?? "(unknown version)"}`,
    );
  } else {
    vscode.window.showWarningMessage("The AScript language server is not running.");
  }
}
