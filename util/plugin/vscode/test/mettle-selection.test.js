const assert = require("node:assert/strict");
const test = require("node:test");

const { selectCurrentFlow } = require("../mettle-selection");

test("keeps an anonymous flow selected when its request text changes", () => {
  const selected = {
    id: 0,
    line: 3,
    name: null,
    displayName: "GET /before",
  };
  const current = [
    { id: 0, line: 3, name: null, displayName: "GET /after" },
  ];

  assert.equal(selectCurrentFlow(selected, current), current[0]);
});

test("tracks a named flow by its compiler-resolved name", () => {
  const selected = { id: 2, line: 8, name: "users.health", displayName: "users.health" };
  const current = [
    { id: 4, line: 12, name: "users.health", displayName: "users.health" },
  ];

  assert.equal(selectCurrentFlow(selected, current), current[0]);
});

test("tracks an anonymous flow moved by edits above it when its identity is unchanged", () => {
  const selected = { id: 3, line: 8, name: null, displayName: "GET /health" };
  const current = [
    { id: 3, line: 12, name: null, displayName: "GET /health" },
    { id: 4, line: 16, name: null, displayName: "GET /ready" },
  ];

  assert.equal(selectCurrentFlow(selected, current), current[0]);
});

test("does not guess between multiple anonymous flows after structural edits", () => {
  const selected = { id: 3, line: 8, name: null, displayName: "GET /old" };
  const current = [
    { id: 3, line: 4, name: null, displayName: "GET /newly-inserted" },
    { id: 4, line: 12, name: null, displayName: "GET /changed" },
  ];

  assert.equal(selectCurrentFlow(selected, current), undefined);
});
