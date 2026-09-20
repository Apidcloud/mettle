const path = require("node:path");
const { spawn } = require("node:child_process");
const vscode = require("vscode");
const { selectCurrentFlow } = require("./mettle-selection");

const MAX_DISCOVERY_OUTPUT = 1024 * 1024;
const MAX_LSP_MESSAGE = 8 * 1024 * 1024;

function mettleExecutable() {
  return vscode.workspace
    .getConfiguration("mettle")
    .get("executablePath", "mettle");
}

function discoverFlows(document, output, token) {
  return new Promise((resolve) => {
    const executable = mettleExecutable();
    const child = spawn(executable, ["list", document.uri.fsPath, "--json"], {
      cwd: path.dirname(document.uri.fsPath),
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    let oversized = false;

    const cancellation = token.onCancellationRequested(() => child.kill());
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      if (stdout.length > MAX_DISCOVERY_OUTPUT) {
        oversized = true;
        child.kill();
      }
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", (error) => {
      cancellation.dispose();
      output.appendLine(`Could not start ${executable}: ${error.message}`);
      resolve([]);
    });
    child.on("close", (code) => {
      cancellation.dispose();
      if (token.isCancellationRequested) {
        resolve([]);
        return;
      }
      if (oversized) {
        output.appendLine("Mettle discovery output exceeded 1 MiB.");
        resolve([]);
        return;
      }
      if (code !== 0) {
        output.appendLine(stderr.trim() || `Mettle discovery exited with status ${code}.`);
        resolve([]);
        return;
      }
      try {
        const result = JSON.parse(stdout);
        const flows = Array.isArray(result.flows) ? result.flows : [];
        resolve(
          flows.filter(
            (flow) =>
              !flow.path ||
              path.resolve(flow.path) === path.resolve(document.uri.fsPath),
          ),
        );
      } catch (error) {
        output.appendLine(`Could not parse Mettle discovery output: ${error.message}`);
        resolve([]);
      }
    });
  });
}

class FlowCodeLensProvider {
  constructor(output) {
    this.output = output;
    this.changeEmitter = new vscode.EventEmitter();
    this.onDidChangeCodeLenses = this.changeEmitter.event;
  }

  refresh() {
    this.changeEmitter.fire();
  }

  dispose() {
    this.changeEmitter.dispose();
  }

  async provideCodeLenses(document, token) {
    const flows = await discoverFlows(document, this.output, token);
    return flows.map((flow) => {
      const line = Math.max(0, Number(flow.line) - 1);
      const range = new vscode.Range(line, 0, line, 0);
      const label = flow.name
        ? `$(play) Run ${flow.name}`
        : `$(play) Run ${flow.displayName}`;
      return new vscode.CodeLens(range, {
        title: label,
        command: "mettle.runFlow",
        arguments: [
          {
            uri: document.uri.toString(),
            id: Number(flow.id),
            line: Number(flow.line),
            name: flow.name,
            displayName: flow.displayName,
            parameters: Array.isArray(flow.parameters) ? flow.parameters : [],
          },
        ],
      });
    });
  }
}

class MettleLanguageServer {
  constructor(output) {
    this.output = output;
    this.child = undefined;
    this.starting = undefined;
    this.buffer = Buffer.alloc(0);
    this.nextId = 1;
    this.pending = new Map();
  }

  async start() {
    if (this.child) {
      return;
    }
    if (this.starting) {
      return this.starting;
    }
    this.starting = this.startProcess();
    try {
      await this.starting;
    } finally {
      this.starting = undefined;
    }
  }

  async startProcess() {
    const executable = mettleExecutable();
    const folder = vscode.workspace.workspaceFolders?.[0];
    const child = spawn(executable, ["lsp"], {
      cwd: folder?.uri.fsPath,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    this.child = child;
    this.buffer = Buffer.alloc(0);
    child.stdout.on("data", (chunk) => this.receive(chunk));
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => this.output.append(chunk));
    child.on("error", (error) => {
      this.output.appendLine(`Could not start ${executable} lsp: ${error.message}`);
      this.failPending(error);
    });
    child.on("close", (code) => {
      if (this.child === child) {
        this.child = undefined;
      }
      this.failPending(new Error(`Mettle language server exited with status ${code}.`));
    });

    const folders = (vscode.workspace.workspaceFolders || []).map((workspace) => ({
      uri: workspace.uri.toString(),
      name: workspace.name,
    }));
    await this.requestRaw("initialize", {
      processId: process.pid,
      rootUri: folder?.uri.toString() || null,
      workspaceFolders: folders,
      capabilities: {},
      clientInfo: { name: "Mettle VS Code", version: "0.9.0" },
    });
    this.notify("initialized", {});
    for (const document of vscode.workspace.textDocuments) {
      if (document.languageId === "mettle") {
        this.open(document);
      }
    }
  }

  receive(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (true) {
      const headerEnd = this.buffer.indexOf("\r\n\r\n");
      if (headerEnd < 0) {
        return;
      }
      const header = this.buffer.subarray(0, headerEnd).toString("ascii");
      const match = /(?:^|\r\n)Content-Length:\s*(\d+)/i.exec(header);
      if (!match) {
        this.output.appendLine("Mettle language server sent a response without Content-Length.");
        this.buffer = Buffer.alloc(0);
        return;
      }
      const length = Number(match[1]);
      if (length > MAX_LSP_MESSAGE) {
        this.output.appendLine("Mettle language server response exceeded 8 MiB.");
        this.child?.kill();
        return;
      }
      const messageStart = headerEnd + 4;
      if (this.buffer.length < messageStart + length) {
        return;
      }
      const body = this.buffer.subarray(messageStart, messageStart + length);
      this.buffer = this.buffer.subarray(messageStart + length);
      try {
        this.handleMessage(JSON.parse(body.toString("utf8")));
      } catch (error) {
        this.output.appendLine(`Could not parse Mettle language server response: ${error.message}`);
      }
    }
  }

  handleMessage(message) {
    if (message.id === undefined) {
      return;
    }
    const pending = this.pending.get(String(message.id));
    if (!pending) {
      return;
    }
    this.pending.delete(String(message.id));
    if (message.error) {
      pending.reject(new Error(message.error.message || "Mettle language server request failed."));
    } else {
      pending.resolve(message.result);
    }
  }

  send(message) {
    if (!this.child?.stdin.writable) {
      throw new Error("Mettle language server is not running.");
    }
    const body = Buffer.from(JSON.stringify(message), "utf8");
    this.child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    this.child.stdin.write(body);
  }

  requestRaw(method, params) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(String(id), { resolve, reject });
      try {
        this.send({ jsonrpc: "2.0", id, method, params });
      } catch (error) {
        this.pending.delete(String(id));
        reject(error);
      }
    });
  }

  async request(method, params, token) {
    await this.start();
    const id = this.nextId++;
    const promise = new Promise((resolve, reject) => {
      this.pending.set(String(id), { resolve, reject });
      this.send({ jsonrpc: "2.0", id, method, params });
    });
    const cancellation = token?.onCancellationRequested(() => {
      this.notify("$/cancelRequest", { id });
    });
    try {
      return await promise;
    } finally {
      cancellation?.dispose();
    }
  }

  notify(method, params) {
    if (this.child?.stdin.writable) {
      this.send({ jsonrpc: "2.0", method, params });
    }
  }

  open(document) {
    this.notify("textDocument/didOpen", {
      textDocument: {
        uri: document.uri.toString(),
        languageId: "mettle",
        version: document.version,
        text: document.getText(),
      },
    });
  }

  change(event) {
    this.notify("textDocument/didChange", {
      textDocument: {
        uri: event.document.uri.toString(),
        version: event.document.version,
      },
      contentChanges: [{ text: event.document.getText() }],
    });
  }

  close(document) {
    this.notify("textDocument/didClose", {
      textDocument: { uri: document.uri.toString() },
    });
  }

  async definition(document, position, token) {
    try {
      const result = await this.request(
        "textDocument/definition",
        {
          textDocument: { uri: document.uri.toString() },
          position: { line: position.line, character: position.character },
        },
        token,
      );
      if (!result) {
        return undefined;
      }
      return new vscode.Location(
        vscode.Uri.parse(result.uri),
        new vscode.Range(
          result.range.start.line,
          result.range.start.character,
          result.range.end.line,
          result.range.end.character,
        ),
      );
    } catch (error) {
      this.output.appendLine(`Mettle definition lookup failed: ${error.message}`);
      return undefined;
    }
  }

  failPending(error) {
    for (const pending of this.pending.values()) {
      pending.reject(error);
    }
    this.pending.clear();
  }

  async stop() {
    if (!this.child) {
      return;
    }
    try {
      await this.requestRaw("shutdown", null);
      this.notify("exit", null);
    } catch (error) {
      this.output.appendLine(`Could not stop Mettle language server cleanly: ${error.message}`);
    } finally {
      this.child?.kill();
      this.child = undefined;
    }
  }
}

async function runFlow(flow, output) {
  const uri = vscode.Uri.parse(flow.uri);
  const document = await vscode.workspace.openTextDocument(uri);
  if (document.isDirty && !(await document.save())) {
    void vscode.window.showErrorMessage("Save the Mettle file before running it.");
    return;
  }

  const cancellationSource = new vscode.CancellationTokenSource();
  const currentFlows = await discoverFlows(
    document,
    output,
    cancellationSource.token,
  );
  cancellationSource.dispose();
  const current = selectCurrentFlow(flow, currentFlows);
  if (!current) {
    void vscode.window.showErrorMessage(
      "The selected flow changed. Use the refreshed Run Flow action.",
    );
    return;
  }

  const argumentsByName = [];
  for (const parameter of current.parameters) {
    const value = await vscode.window.showInputBox({
      title: `Run ${current.displayName}`,
      prompt: `Value for ${parameter} (Mettle literal or text)`,
      ignoreFocusOut: true,
    });
    if (value === undefined) {
      return;
    }
    argumentsByName.push([parameter, value]);
  }

  const args = ["run", uri.fsPath];
  if (current.name) {
    args.push(current.name);
  } else {
    args.push("--flow-id", String(current.id));
  }
  for (const [name, value] of argumentsByName) {
    args.push("--arg", `${name}=${value}`);
  }

  const workspaceFolder = vscode.workspace.getWorkspaceFolder(uri);
  const scope = workspaceFolder || vscode.TaskScope.Workspace;
  const execution = new vscode.ProcessExecution(mettleExecutable(), args, {
    cwd: path.dirname(uri.fsPath),
  });
  const task = new vscode.Task(
    { type: "mettle", flow: current.displayName },
    scope,
    `Run ${current.displayName}`,
    "Mettle",
    execution,
  );
  task.presentationOptions = {
    reveal: vscode.TaskRevealKind.Always,
    panel: vscode.TaskPanelKind.Dedicated,
    clear: true,
  };
  await vscode.tasks.executeTask(task);
}

function activate(context) {
  const output = vscode.window.createOutputChannel("Mettle");
  const provider = new FlowCodeLensProvider(output);
  const languageServer = new MettleLanguageServer(output);
  context.subscriptions.push(
    output,
    provider,
    vscode.languages.registerCodeLensProvider({ language: "mettle" }, provider),
    vscode.languages.registerDefinitionProvider(
      { language: "mettle", scheme: "file" },
      { provideDefinition: (document, position, token) => languageServer.definition(document, position, token) },
    ),
    vscode.workspace.onDidOpenTextDocument((document) => {
      if (document.languageId === "mettle") {
        void languageServer.start().then(() => languageServer.open(document));
      }
    }),
    vscode.workspace.onDidChangeTextDocument((event) => {
      if (event.document.languageId === "mettle") {
        languageServer.change(event);
      }
    }),
    vscode.workspace.onDidSaveTextDocument((document) => {
      if (document.languageId === "mettle") {
        provider.refresh();
      }
    }),
    vscode.workspace.onDidCloseTextDocument((document) => {
      if (document.languageId === "mettle") {
        languageServer.close(document);
      }
    }),
    vscode.commands.registerCommand("mettle.runFlow", (flow) => runFlow(flow, output)),
    { dispose: () => void languageServer.stop() },
  );
  if (vscode.workspace.textDocuments.some((document) => document.languageId === "mettle")) {
    void languageServer.start();
  }
}

function deactivate() {}

module.exports = { activate, deactivate };
