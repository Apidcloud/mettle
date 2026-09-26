const assert = require("node:assert/strict");
const { mkdtempSync, writeFileSync, rmSync, readFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join, resolve } = require("node:path");
const { spawnSync } = require("node:child_process");

const grammar = resolve(__dirname, "..");
const temporary = mkdtempSync(join(tmpdir(), "mettle-tree-sitter-"));
const invalid = [
  'flow main() { value = 1 return value }',
  'flow main() = { a: 1 b: 2 }',
  'flow main() = parallel() { first() second() }',
  'flow main() { first(), second() }',
  'flow main() = [1,,]',
  'flow main(a,) = a',
  'flow main() = fetch(named: 1, 2)',
  'flow main() = 1 < 2 < 3',
  'flow main() = 1e2s',
  'flow main() = 5days',
  'flow main() = 0b102',
  'flow main() = 1__000',
  'flow main() = "bad\\q"',
  'flow main() = "line\nbreak"',
  'flow main() = "${user name}"',
  'flow main() = within(duration: 1s) { fetch() }',
  'flow main() = retry(attempts: 2) { first() second() }',
  'flow main() { assert(true, 123) }',
  'flow main() { assert(true, ((123))) }',
  'flow main() { assert(true, (message)) }',
  'flow main = (1)()',
  'flow main = for x in f(g() {}) {}',
  'flow main = for x in [f() {}] {}',
  'flow main(return) = return',
];

function parse(source, options = []) {
  const file = join(temporary, "case.mettle");
  writeFileSync(file, source);
  const result = spawnSync("tree-sitter", ["parse", ...options, file], {
    cwd: grammar, encoding: "utf8", maxBuffer: 8 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  assert.notEqual(result.status, null, result.stderr);
  return result;
}

try {
  for (const source of invalid) {
    const result = parse(source);
    assert.notEqual(result.status, 0, `Expected a syntax error: ${source}\n${result.stdout}`);
    assert.match(result.stdout, /ERROR|MISSING/, result.stderr);
  }
  const crlf = 'flow main() {\r\n  a = 1 // comment\r\n  return a\r\n}\r\n';
  assert.equal(parse(crlf).status, 0, "CRLF statement separators must parse");
  assert.equal(parse('flow fields() = { a: 1\n, b: 2 }').status, 0, "A comma may follow a newline");
  assert.equal(parse('flow branches() = parallel() { foo\nbar, baz }').status, 0, "A bare name can start a branch");

  // Incomplete expressions should not swallow the following declaration.
  for (const broken of ['flow broken() = fetch(', 'flow broken() = "unfinished']) {
    const result = parse(`${broken}\nflow intact() = 42\n`);
    assert.notEqual(result.status, 0);
    assert.match(result.stdout, /flow_declaration \[1, 0\]/, result.stdout);
  }

  // Validate all query files against the generated node types.
  const example = resolve(grammar, "../../../examples/language/conditionals.mettle");
  for (const name of ["highlights", "folds", "indents"]) {
    const result = spawnSync("tree-sitter", ["query", "--quiet", `queries/mettle/${name}.scm`, example], {
      cwd: grammar, encoding: "utf8",
    });
    assert.equal(result.status, 0, result.stdout + result.stderr);
  }
  const metadata = JSON.parse(readFileSync(join(grammar, "tree-sitter.json"), "utf8"));
  assert.deepEqual(metadata.grammars[0]["file-types"], ["mettle"]);
  console.log(`Checked ${invalid.length} invalid programs, CRLF, error recovery, and all editor queries.`);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
