#!/usr/bin/env bash
set -euo pipefail

plugin_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$plugin_dir"

mkdir -p dist
exec npx --yes @vscode/vsce@4.0.0 package \
  --allow-missing-repository \
  --out dist/mettle-language-0.13.0.vsix
