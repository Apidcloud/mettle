const fs = require("node:fs");
const path = require("node:path");

const PROFILE_FILE = /^\.env\.([A-Za-z0-9_-]+)$/;

function isFile(filePath) {
  try {
    return fs.statSync(filePath).isFile();
  } catch (error) {
    if (error.code === "ENOENT") {
      return false;
    }
    throw error;
  }
}

function profileLocations(filePath) {
  const entryDirectory = path.dirname(path.resolve(filePath));
  let projectRoot;
  for (let directory = entryDirectory; ; directory = path.dirname(directory)) {
    if (isFile(path.join(directory, "mettle.toml"))) {
      projectRoot = directory;
      break;
    }
    if (path.dirname(directory) === directory) {
      break;
    }
  }
  const directories = projectRoot && projectRoot !== entryDirectory
    ? [projectRoot, entryDirectory]
    : [entryDirectory];
  return { directories, scope: projectRoot || entryDirectory };
}

function listProfiles(filePath) {
  const locations = profileLocations(filePath);
  const profiles = new Set();
  let hasDefault = false;
  for (const directory of locations.directories) {
    let entries;
    try {
      entries = fs.readdirSync(directory, { withFileTypes: true });
    } catch (error) {
      if (error.code === "ENOENT") {
        continue;
      }
      throw error;
    }
    for (const entry of entries) {
      if (!isFile(path.join(directory, entry.name))) {
        continue;
      }
      if (entry.name === ".env") {
        hasDefault = true;
      } else {
        const match = PROFILE_FILE.exec(entry.name);
        if (match && match[1] !== "example") {
          profiles.add(match[1]);
        }
      }
    }
  }
  return {
    ...locations,
    hasDefault,
    profiles: [...profiles].sort(),
  };
}

module.exports = { listProfiles, profileLocations };
