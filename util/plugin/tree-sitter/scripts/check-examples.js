const { readdirSync } = require("node:fs");
const { resolve, join } = require("node:path");
const { spawnSync } = require("node:child_process");

const grammar = resolve(__dirname, "..");
const root = resolve(grammar, "../../..");
function sources(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap(entry => {
    const path = join(dir, entry.name);
    return entry.isDirectory() ? sources(path) : path.endsWith(".mettle") ? [path] : [];
  });
}
// Some fixtures fail semantic validation; all are syntactically valid Mettle.
const files = [...sources(join(root, "examples")), ...sources(join(root, "tests"))];
const result = spawnSync("tree-sitter", ["parse", "--quiet", ...files], {
  cwd: grammar, encoding: "utf8", maxBuffer: 8 * 1024 * 1024,
});
if (result.error) throw result.error;
if (result.status !== 0) {
  process.stderr.write(result.stdout + result.stderr);
  process.exit(result.status || 1);
}
console.log(`Parsed all ${files.length} repository examples and fixtures without errors.`);
