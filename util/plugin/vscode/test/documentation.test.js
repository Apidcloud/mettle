const assert = require("node:assert/strict");
const test = require("node:test");
const documentation = require("../mettle-documentation");

class MarkdownString {
  constructor(value) { this.value = value; }
  appendMarkdown(value) { this.value += value; }
}
class Entry { constructor(label, documentation) { this.label = label; this.documentation = documentation; } }
const vscode = { MarkdownString, Hover: class { constructor(contents, range) { this.contents = contents; this.range = range; } }, Range: class {}, SignatureHelp: class {}, SignatureInformation: Entry, ParameterInformation: Entry };

test("only versioned built-in reference URIs can enable the documentation command", () => {
  for (const bad of ["file:///secret", "command:other", "mettle-doc:/1/../x.md", "mettle-doc:/1/http/post.md?x=y", "mettle-doc:/1/http/post.md#x"]) assert.equal(documentation.isDocumentationUri(bad), false);
  const plain = documentation.hover(vscode, { contents: { value: "[unsafe](command:other)" } });
  assert.equal(plain.contents.isTrusted, false);
  const builtin = documentation.hover(vscode, { contents: { value: "POST" }, mettleReference: "mettle-doc:/1.0.0-alpha.1/http/post.md" });
  assert.deepEqual(builtin.contents.isTrusted, { enabledCommands: ["mettle.openDocumentation"] });
  assert.match(builtin.contents.value, /Open full reference/);
  assert.equal(builtin.contents.supportHtml, false);
});

test("signature help retains the active named parameter and its description", () => {
  const help = documentation.signatureHelp(vscode, { signatures: [{ label: "post(url, body)", activeParameter: 1, parameters: [{ label: "url", documentation: { value: "Address" } }, { label: "body", documentation: { value: "Payload" } }] }] });
  assert.equal(help.activeParameter, 1);
  assert.equal(help.signatures[0].parameters[1].documentation.value, "Payload");
  assert.equal(help.signatures[0].parameters[1].documentation.isTrusted, false);
  assert.equal(documentation.signatureHelp(vscode, null), undefined);
});

test("language keyword references use the same safe hover adapter", () => {
  for (const name of ["assert", "use", "parallel", "env"]) {
    const uri = `mettle-doc:/1.0.0-alpha.1/language/${name}.md`;
    assert.equal(documentation.isDocumentationUri(uri), true);
    const hover = documentation.hover(vscode, { contents: { value: name }, mettleReference: uri });
    assert.deepEqual(hover.contents.isTrusted, { enabledCommands: ["mettle.openDocumentation"] });
    assert.match(hover.contents.value, /Open full reference/);
  }
});
