import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";
import { resolveServerPath, checkServerVersion } from "./serverPath";

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand("ascript.restartServer", restartServer),
    vscode.commands.registerCommand("ascript.showServerVersion", showServerVersion),
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
