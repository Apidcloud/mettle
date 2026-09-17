const path = require("node:path");
const { spawn } = require("node:child_process");
const vscode = require("vscode");

const MAX_DISCOVERY_OUTPUT = 1024 * 1024;

function flowExecutable() {
  return vscode.workspace
    .getConfiguration("flow")
    .get("executablePath", "flow");
}

function discoverFlows(document, output, token) {
  return new Promise((resolve) => {
    const executable = flowExecutable();
    const child = spawn(executable, ["list", "-", "--json"], {
      cwd: path.dirname(document.uri.fsPath),
      stdio: ["pipe", "pipe", "pipe"],
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
    child.stdin.on("error", () => {
      // The process error/close handlers report startup and early-exit failures.
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
        output.appendLine("Flow discovery output exceeded 1 MiB.");
        resolve([]);
        return;
      }
      if (code !== 0) {
        output.appendLine(stderr.trim() || `Flow discovery exited with status ${code}.`);
        resolve([]);
        return;
      }
      try {
        const result = JSON.parse(stdout);
        resolve(Array.isArray(result.flows) ? result.flows : []);
      } catch (error) {
        output.appendLine(`Could not parse Flow discovery output: ${error.message}`);
        resolve([]);
      }
    });
    child.stdin.end(document.getText());
  });
}

class FlowCodeLensProvider {
  constructor(output) {
    this.output = output;
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
        command: "flow.runFlow",
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

async function runFlow(flow, output) {
  const uri = vscode.Uri.parse(flow.uri);
  const document = await vscode.workspace.openTextDocument(uri);
  if (document.isDirty && !(await document.save())) {
    void vscode.window.showErrorMessage("Save the Flow file before running it.");
    return;
  }

  const cancellationSource = new vscode.CancellationTokenSource();
  const currentFlows = await discoverFlows(
    document,
    output,
    cancellationSource.token,
  );
  cancellationSource.dispose();
  const current = flow.name
    ? currentFlows.find((candidate) => candidate.name === flow.name)
    : currentFlows.find(
        (candidate) =>
          candidate.name === null &&
          Number(candidate.line) === flow.line &&
          candidate.displayName === flow.displayName,
      );
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
      prompt: `Value for ${parameter} (Flow literal or text)`,
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
  const execution = new vscode.ProcessExecution(flowExecutable(), args, {
    cwd: path.dirname(uri.fsPath),
  });
  const task = new vscode.Task(
    { type: "flow", flow: current.displayName },
    scope,
    `Run ${current.displayName}`,
    "Flow",
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
  const output = vscode.window.createOutputChannel("Flow");
  const provider = new FlowCodeLensProvider(output);
  context.subscriptions.push(
    output,
    vscode.languages.registerCodeLensProvider({ language: "flow" }, provider),
    vscode.commands.registerCommand("flow.runFlow", (flow) => runFlow(flow, output)),
  );
}

function deactivate() {}

module.exports = { activate, deactivate };
