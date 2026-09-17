#!/usr/bin/env bash
# Build Astra *inside* Termux (native clang, no cross compilation).
#
#   pkg install clang make
#   ./scripts/build-termux.sh [--package]
#
# Produces build/astra and, with --package, dist/astra_<version>_<arch>.deb
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
cd "$HERE"

: "${CC:=clang}"
: "${CFLAGS:=-O2 -std=gnu11 -Wall -Wextra -Wno-unused-parameter -Wno-unused-function -Wno-format-truncation}"
VERSION="$(grep -m1 'ASTRA_VERSION' src/config.h | sed -E 's/.*"([0-9.]+)".*/\1/')"
ARCH="$(uname -m)"
case "$ARCH" in
  arm64) ARCH=aarch64 ;;
  amd64) ARCH=x86_64 ;;
  armv7l | armv8l) ARCH=arm ;;
esac
echo "building astra $VERSION for $ARCH with $CC"

mkdir -p build
# shellcheck disable=SC2086
$CC $CFLAGS -Isrc src/*.c -o build/astra -lm   # bionic: no separate -lpthread
echo "built: build/astra ($(du -h build/astra | cut -f1))"

if [[ "${1:-}" == "--package" ]]; then
  # package-deb.sh stages dist/astra, so publish the freshly built binary there
  mkdir -p dist
  install -m 0755 build/astra dist/astra
  bash "$(dirname "$0")/package-deb.sh" "$VERSION" "$ARCH"
fi
