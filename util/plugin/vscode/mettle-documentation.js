// Convert shared LSP documentation without maintaining an editor API catalogue.
function isDocumentationUri(uri) {
  return typeof uri === "string" && /^mettle-doc:\/[A-Za-z0-9.-]+\/[A-Za-z_][A-Za-z0-9_]*(?:\/[A-Za-z_][A-Za-z0-9_]*)?\.md$/.test(uri);
}

function markdown(vscode, value) {
  const text = typeof value === "string" ? value : value?.value || "";
  const result = new vscode.MarkdownString(text);
  result.isTrusted = false;
  result.supportHtml = false;
  return result;
}

function hover(vscode, result) {
  if (!result?.contents) return undefined;
  const content = markdown(vscode, result.contents);
  if (isDocumentationUri(result.mettleReference)) {
    const commandArguments = encodeURIComponent(JSON.stringify([result.mettleReference]));
    content.appendMarkdown(`\n\n[Open full reference](command:mettle.openDocumentation?${commandArguments})`);
    content.isTrusted = { enabledCommands: ["mettle.openDocumentation"] };
  }
  const range = result.range && new vscode.Range(
    result.range.start.line, result.range.start.character,
    result.range.end.line, result.range.end.character,
  );
  return new vscode.Hover(content, range);
}

function signatureHelp(vscode, result) {
  if (!result?.signatures?.length) return undefined;
  const help = new vscode.SignatureHelp();
  help.signatures = result.signatures.map((entry) => {
    const signature = new vscode.SignatureInformation(entry.label, markdown(vscode, entry.documentation));
    signature.parameters = (entry.parameters || []).map((parameter) =>
      new vscode.ParameterInformation(parameter.label, markdown(vscode, parameter.documentation)));
    signature.activeParameter = entry.activeParameter;
    return signature;
  });
  help.activeSignature = result.activeSignature || 0;
  help.activeParameter = result.activeParameter ?? help.signatures[help.activeSignature]?.activeParameter ?? 0;
  return help;
}

module.exports = { isDocumentationUri, hover, signatureHelp };
