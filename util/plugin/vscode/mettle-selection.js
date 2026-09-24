function selectCurrentFlow(selected, currentFlows) {
  if (selected.name) {
    return currentFlows.find((candidate) => candidate.name === selected.name);
  }

  const anonymous = currentFlows.filter((candidate) => candidate.name === null);
  const sameLine = anonymous.find(
    (candidate) => Number(candidate.line) === Number(selected.line),
  );
  if (sameLine) {
    return sameLine;
  }

  const sameIdentity = anonymous.find(
    (candidate) =>
      Number(candidate.id) === Number(selected.id) &&
      candidate.displayName === selected.displayName,
  );
  if (sameIdentity) {
    return sameIdentity;
  }

  return anonymous.length === 1 ? anonymous[0] : undefined;
}

function selectCurrentTest(selected, currentTests) {
  return currentTests.find((candidate) => candidate.name === selected.name);
}

module.exports = { selectCurrentFlow, selectCurrentTest };
