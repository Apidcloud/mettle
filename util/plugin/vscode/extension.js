const path = require("node:path");
const { spawn } = require("node:child_process");
const vscode = require("vscode");
const { selectCurrentFlow, selectCurrentTest } = require("./mettle-selection");
const { listProfiles, profileLocations } = require("./mettle-profiles");

const MAX_DISCOVERY_OUTPUT = 1024 * 1024;
const MAX_LSP_MESSAGE = 8 * 1024 * 1024;

function mettleExecutable() {
  return vscode.workspace
    .getConfiguration("mettle")
    .get("executablePath", "mettle");
}

function discoverDeclarations(document, output, token) {
  return new Promise((resolve) => {
    const empty = () => ({ flows: [], tests: [] });
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
      resolve(empty());
    });
    child.on("close", (code) => {
      cancellation.dispose();
      if (token.isCancellationRequested) {
        resolve(empty());
        return;
      }
      if (oversized) {
        output.appendLine("Mettle discovery output exceeded 1 MiB.");
        resolve(empty());
        return;
      }
      if (code !== 0) {
        output.appendLine(stderr.trim() || `Mettle discovery exited with status ${code}.`);
        resolve(empty());
        return;
      }
      try {
        const result = JSON.parse(stdout);
        const inDocument = (item) =>
          !item.path || path.resolve(item.path) === path.resolve(document.uri.fsPath);
        resolve({
          flows: (Array.isArray(result.flows) ? result.flows : []).filter(inDocument),
          tests: (Array.isArray(result.tests) ? result.tests : []).filter(inDocument),
        });
      } catch (error) {
        output.appendLine(`Could not parse Mettle discovery output: ${error.message}`);
        resolve(empty());
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
    const { flows, tests } = await discoverDeclarations(document, this.output, token);
    const flowLenses = flows.map((flow) => {
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
    const testLenses = tests.map((test) => {
      const line = Math.max(0, Number(test.line) - 1);
      return new vscode.CodeLens(new vscode.Range(line, 0, line, 0), {
        title: `$(play) Run test: ${test.name}`,
        command: "mettle.runTest",
        arguments: [{ uri: document.uri.toString(), name: test.name }],
      });
    });
    return [...flowLenses, ...testLenses];
  }
}

class MettleLanguageServer {
  constructor(output, diagnostics) {
    this.output = output;
    this.diagnostics = diagnostics;
    this.child = undefined;
    this.starting = undefined;
    this.buffer = Buffer.alloc(0);
    this.nextId = 1;
    this.pending = new Map();
    this.openDocuments = new Set();
    this.initialized = false;
  }

  async start() {
    if (this.starting) {
      return this.starting;
    }
    if (this.child) {
      return;
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
    this.openDocuments.clear();
    this.initialized = false;
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
        this.openDocuments.clear();
        this.initialized = false;
        this.diagnostics.clear();
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
      clientInfo: { name: "Mettle VS Code", version: "0.13.0" },
    });
    this.notify("initialized", {});
    this.initialized = true;
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
    if (message.method === "textDocument/publishDiagnostics") {
      this.publishDiagnostics(message.params);
      return;
    }
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

  publishDiagnostics(params) {
    try {
      if (typeof params?.uri !== "string" || !Array.isArray(params.diagnostics)) {
        throw new Error("invalid diagnostic notification");
      }
      const uri = vscode.Uri.parse(params.uri);
      const severities = [
        undefined,
        vscode.DiagnosticSeverity.Error,
        vscode.DiagnosticSeverity.Warning,
        vscode.DiagnosticSeverity.Information,
        vscode.DiagnosticSeverity.Hint,
      ];
      const diagnostics = params.diagnostics.map((item) => {
        const range = new vscode.Range(
          item.range.start.line,
          item.range.start.character,
          item.range.end.line,
          item.range.end.character,
        );
        const diagnostic = new vscode.Diagnostic(
          range,
          item.message,
          severities[item.severity] ?? vscode.DiagnosticSeverity.Error,
        );
        diagnostic.source = item.source || "mettle";
        diagnostic.code = item.code;
        return diagnostic;
      });
      this.diagnostics.set(uri, diagnostics);
    } catch (error) {
      this.output.appendLine(`Could not display Mettle diagnostics: ${error.message}`);
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
    if (document.uri.scheme !== "file" || !this.initialized || !this.child?.stdin.writable) {
      return;
    }
    const uri = document.uri.toString();
    if (this.openDocuments.has(uri)) {
      return;
    }
    this.openDocuments.add(uri);
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
    if (!this.initialized) {
      return;
    }
    if (!this.openDocuments.has(event.document.uri.toString())) {
      this.open(event.document);
      return;
    }
    this.notify("textDocument/didChange", {
      textDocument: {
        uri: event.document.uri.toString(),
        version: event.document.version,
      },
      contentChanges: [{ text: event.document.getText() }],
    });
  }

  close(document) {
    if (!this.openDocuments.delete(document.uri.toString())) {
      return;
    }
    this.notify("textDocument/didClose", {
      textDocument: { uri: document.uri.toString() },
    });
  }

  save(document) {
    if (!this.initialized) {
      return;
    }
    this.notify("textDocument/didSave", {
      textDocument: { uri: document.uri.toString() },
    });
  }

  revalidate() {
    if (!this.initialized) {
      return;
    }
    this.notify("mettle/revalidate", {});
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

function profileKey(filePath) {
  return `mettle.profile:${profileLocations(filePath).scope}`;
}

function selectedProfile(context, filePath) {
  return context.workspaceState.get(profileKey(filePath));
}

function profileArguments(context, filePath) {
  const profile = selectedProfile(context, filePath);
  if (!profile) {
    return [];
  }
  if (!listProfiles(filePath).profiles.includes(profile)) {
    void vscode.window.showErrorMessage(
      `Mettle profile "${profile}" is no longer available beside this file or at its project root. Select another profile.`,
    );
    return undefined;
  }
  return ["--profile", profile];
}

function refreshProfileStatus(context, status, output) {
  const document = vscode.window.activeTextEditor?.document;
  if (!document || document.languageId !== "mettle" || document.uri.scheme !== "file") {
    status.hide();
    return;
  }
  try {
    const available = listProfiles(document.uri.fsPath);
    const profile = selectedProfile(context, document.uri.fsPath);
    const missing = profile && !available.profiles.includes(profile);
    status.text = missing
      ? `$(warning) Mettle profile: ${profile}`
      : `$(settings-gear) Mettle profile: ${profile || "Default"}`;
    status.tooltip = missing
      ? `The selected .env.${profile} file is missing. Click to choose a profile.`
      : "Select the Mettle environment profile for this file.";
    status.show();
  } catch (error) {
    output.appendLine(`Could not discover Mettle profiles: ${error.message}`);
    status.text = "$(warning) Mettle profile";
    status.tooltip = "Could not discover environment profiles. Click to retry.";
    status.show();
  }
}

async function selectProfile(context, status, output) {
  const document = vscode.window.activeTextEditor?.document;
  if (!document || document.languageId !== "mettle" || document.uri.scheme !== "file") {
    void vscode.window.showErrorMessage("Open a Mettle file to select a profile.");
    return;
  }
  try {
    const available = listProfiles(document.uri.fsPath);
    const selected = selectedProfile(context, document.uri.fsPath);
    const items = [
      {
        label: "Default",
        description: available.hasDefault ? ".env" : "process environment only",
        profile: undefined,
        picked: !selected,
      },
      ...available.profiles.map((profile) => ({
        label: profile,
        description: `.env.${profile}`,
        profile,
        picked: profile === selected,
      })),
    ];
    const choice = await vscode.window.showQuickPick(items, {
      title: "Mettle: Select Profile",
      placeHolder: "Choose the environment for this file or project",
    });
    if (choice) {
      await context.workspaceState.update(profileKey(document.uri.fsPath), choice.profile);
      refreshProfileStatus(context, status, output);
    }
  } catch (error) {
    void vscode.window.showErrorMessage(`Could not discover Mettle profiles: ${error.message}`);
  }
}

async function runFlow(flow, output, context) {
  const uri = vscode.Uri.parse(flow.uri);
  const document = await vscode.workspace.openTextDocument(uri);
  if (document.isDirty && !(await document.save())) {
    void vscode.window.showErrorMessage("Save the Mettle file before running it.");
    return;
  }

  const cancellationSource = new vscode.CancellationTokenSource();
  const { flows: currentFlows } = await discoverDeclarations(
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
  const environmentArgs = profileArguments(context, uri.fsPath);
  if (!environmentArgs) {
    return;
  }
  args.push(...environmentArgs);

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

async function runTest(test, output, context) {
  const uri = vscode.Uri.parse(test.uri);
  const document = await vscode.workspace.openTextDocument(uri);
  if (document.isDirty && !(await document.save())) {
    void vscode.window.showErrorMessage("Save the Mettle file before running it.");
    return;
  }

  const cancellationSource = new vscode.CancellationTokenSource();
  const { tests } = await discoverDeclarations(document, output, cancellationSource.token);
  cancellationSource.dispose();
  const current = selectCurrentTest(test, tests);
  if (!current) {
    void vscode.window.showErrorMessage(
      "The selected test changed. Use the refreshed Run Test action.",
    );
    return;
  }

  const environmentArgs = profileArguments(context, uri.fsPath);
  if (!environmentArgs) {
    return;
  }
  const workspaceFolder = vscode.workspace.getWorkspaceFolder(uri);
  const scope = workspaceFolder || vscode.TaskScope.Workspace;
  const execution = new vscode.ProcessExecution(
    mettleExecutable(),
    ["test", uri.fsPath, current.name, ...environmentArgs],
    { cwd: path.dirname(uri.fsPath) },
  );
  const task = new vscode.Task(
    { type: "mettle", test: current.name },
    scope,
    `Run test: ${current.name}`,
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

async function runFileTask(context, operation, label, extraArguments) {
  const document = vscode.window.activeTextEditor?.document;
  if (!document || document.languageId !== "mettle") {
    void vscode.window.showErrorMessage("Open a Mettle file before running it.");
    return;
  }
  if (document.isDirty && !(await document.save())) {
    void vscode.window.showErrorMessage("Save the Mettle file before running it.");
    return;
  }

  const environmentArgs = profileArguments(context, document.uri.fsPath);
  if (!environmentArgs) {
    return;
  }

  const workspaceFolder = vscode.workspace.getWorkspaceFolder(document.uri);
  const scope = workspaceFolder || vscode.TaskScope.Workspace;
  const execution = new vscode.ProcessExecution(
    mettleExecutable(),
    [operation, document.uri.fsPath, ...extraArguments, ...environmentArgs],
    { cwd: path.dirname(document.uri.fsPath) },
  );
  const task = new vscode.Task(
    { type: "mettle", file: document.uri.fsPath, operation },
    scope,
    label,
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
  const diagnostics = vscode.languages.createDiagnosticCollection("mettle");
  const provider = new FlowCodeLensProvider(output);
  const languageServer = new MettleLanguageServer(output, diagnostics);
  const profileStatus = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  profileStatus.command = "mettle.selectProfile";
  const profileWatcher = vscode.workspace.createFileSystemWatcher("**/.env*");
  const projectWatcher = vscode.workspace.createFileSystemWatcher("**/mettle.toml");
  const sourceWatcher = vscode.workspace.createFileSystemWatcher("**/*.mettle");
  context.subscriptions.push(
    output,
    diagnostics,
    provider,
    profileStatus,
    profileWatcher,
    projectWatcher,
    sourceWatcher,
    profileWatcher.onDidCreate(() => refreshProfileStatus(context, profileStatus, output)),
    profileWatcher.onDidDelete(() => refreshProfileStatus(context, profileStatus, output)),
    profileWatcher.onDidChange(() => refreshProfileStatus(context, profileStatus, output)),
    projectWatcher.onDidCreate(() => {
      refreshProfileStatus(context, profileStatus, output);
      languageServer.revalidate();
    }),
    projectWatcher.onDidDelete(() => {
      refreshProfileStatus(context, profileStatus, output);
      languageServer.revalidate();
    }),
    projectWatcher.onDidChange(() => languageServer.revalidate()),
    sourceWatcher.onDidCreate(() => languageServer.revalidate()),
    sourceWatcher.onDidChange(() => languageServer.revalidate()),
    sourceWatcher.onDidDelete(() => languageServer.revalidate()),
    vscode.window.onDidChangeActiveTextEditor(() => refreshProfileStatus(context, profileStatus, output)),
    vscode.window.onDidChangeWindowState((state) => {
      if (state.focused) {
        refreshProfileStatus(context, profileStatus, output);
      }
    }),
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
        languageServer.save(document);
      }
    }),
    vscode.workspace.onDidCloseTextDocument((document) => {
      if (document.languageId === "mettle") {
        languageServer.close(document);
      }
    }),
    vscode.commands.registerCommand("mettle.runFlow", (flow) => runFlow(flow, output, context)),
    vscode.commands.registerCommand("mettle.runTest", (test) => runTest(test, output, context)),
    vscode.commands.registerCommand("mettle.runAllFlows", () =>
      runFileTask(context, "run", "Run All Eligible Flows", ["--all"])),
    vscode.commands.registerCommand("mettle.runTests", () =>
      runFileTask(context, "test", "Run File Tests", [])),
    vscode.commands.registerCommand("mettle.selectProfile", () =>
      selectProfile(context, profileStatus, output)),
    { dispose: () => void languageServer.stop() },
  );
  if (vscode.workspace.textDocuments.some((document) => document.languageId === "mettle")) {
    void languageServer.start();
  }
  refreshProfileStatus(context, profileStatus, output);
}

function deactivate() {}

module.exports = { activate, deactivate };
