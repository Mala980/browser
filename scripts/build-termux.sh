#!/usr/bin/env bash
# Build Astra *inside* Termux (native clang, no cross compilation).
#
#   pkg install clang make
#   ./scripts/build-termux.sh [--package]
#
# Produces build/astra and, with --package, dist/astra_<version>_aarch64.deb
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
cd "$HERE"

: "${CC:=clang}"
: "${CFLAGS:=-O2 -std=gnu11 -Wall -Wextra -Wno-unused-parameter -Wno-unused-function -Wno-format-truncation}"
VERSION="$(grep -m1 'ASTRA_VERSION' src/config.h | sed -E 's/.*"([0-9.]+)".*/\1/')"
echo "building astra $VERSION for $(uname -m) with $CC"

mkdir -p build
# shellcheck disable=SC2086
$CC $CFLAGS -Isrc src/*.c -o build/astra -lm -lpthread
echo "built: build/astra ($(du -h build/astra | cut -f1))"

if [[ "${1:-}" == "--package" ]]; then
  bash "$(dirname "$0")/package-deb.sh" "$VERSION" aarch64
fi
