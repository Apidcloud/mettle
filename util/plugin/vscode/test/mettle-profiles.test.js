const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const { listProfiles, profileLocations } = require("../mettle-profiles");

test("discovers standalone profiles afresh when files change", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "mettle-profiles-"));
  try {
    const entry = path.join(directory, "checks.mettle");
    fs.writeFileSync(entry, "flow main() = null\n");
    fs.writeFileSync(path.join(directory, ".env"), "VALUE=default\n");
    fs.writeFileSync(path.join(directory, ".env.qa"), "VALUE=qa\n");
    fs.writeFileSync(path.join(directory, ".env.example"), "VALUE=sample\n");
    assert.deepEqual(listProfiles(entry).profiles, ["qa"]);
    assert.equal(listProfiles(entry).hasDefault, true);
    fs.renameSync(path.join(directory, ".env.qa"), path.join(directory, ".env.prod"));
    assert.deepEqual(listProfiles(entry).profiles, ["prod"]);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test("finds profiles in project root and entry directory", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "mettle-profiles-"));
  try {
    const entryDirectory = path.join(directory, "checks");
    fs.mkdirSync(entryDirectory);
    const entry = path.join(entryDirectory, "main.mettle");
    fs.writeFileSync(entry, "flow main() = null\n");
    fs.writeFileSync(path.join(directory, "mettle.toml"), "name = \"profiles\"\n");
    fs.writeFileSync(path.join(directory, ".env.qa"), "VALUE=root\n");
    fs.writeFileSync(path.join(entryDirectory, ".env.prod"), "VALUE=entry\n");
    assert.equal(profileLocations(entry).scope, directory);
    assert.deepEqual(listProfiles(entry).profiles, ["prod", "qa"]);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});
