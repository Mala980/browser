#!/usr/bin/env bash
# Install the dev-only tooling used by `npm run check:rust` and the E2E tests into
# a directory that survives a clean checkout (the repo itself never needs Node
# modules - the browser is pure Rust).
set -euo pipefail
DEST="${1:-/home/user/tools/tsdeps}"
TMP="$(mktemp -d)"
cd "$TMP"
npm init -y >/dev/null
npm i --silent web-tree-sitter@0.25 tree-sitter-wasms puppeteer-core@23 >/dev/null
mkdir -p "$DEST/web-tree-sitter"
cp node_modules/web-tree-sitter/tree-sitter.js "$DEST/web-tree-sitter/"
cp node_modules/web-tree-sitter/tree-sitter.wasm "$DEST/web-tree-sitter/"
cp node_modules/tree-sitter-wasms/out/tree-sitter-rust.wasm "$DEST/"
mkdir -p "$DEST/puppeteer-core"
cp -r node_modules/puppeteer-core "$DEST/" 2>/dev/null || true
rm -rf "$TMP"
echo "dev tools ready in $DEST"
