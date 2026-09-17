#!/usr/bin/env bash
set -euo pipefail

plugin_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$plugin_dir"

mkdir -p dist
exec npx --yes @vscode/vsce@4.0.0 package \
  --allow-missing-repository \
  --out dist/flow-language-0.4.0.vsix
