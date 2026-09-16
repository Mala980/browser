#!/usr/bin/env bash
# Cross compile Astra for Termux (Android aarch64, bionic libc) using the
# Android NDK.  Termux binaries are ordinary bionic binaries, so a plain
# aarch64-linux-android target works.
#
#   ANDROID_NDK_HOME=/opt/ndk ./scripts/build-termux-ndk.sh
#
# Output: dist/astra  (static when possible), dist/astra_<version>_aarch64.deb
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
cd "$HERE"

NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-${1:-}}}"
if [[ -z "$NDK" || ! -d "$NDK" ]]; then
  echo "set ANDROID_NDK_HOME to your NDK path" >&2
  exit 1
fi
API="${API:-24}"
TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/$(uname -s | tr 'A-Z' 'a-z')-x86_64"
CC="$TOOLCHAIN/bin/aarch64-linux-android${API}-clang"
STRIP="$TOOLCHAIN/bin/llvm-strip"
[[ -x "$CC" ]] || { echo "clang not found: $CC" >&2; exit 1; }

VERSION="$(grep -m1 'ASTRA_VERSION' src/config.h | sed -E 's/.*"([0-9.]+)".*/\1/')"
echo "cross compiling astra $VERSION (aarch64-linux-android$API) with the NDK"

mkdir -p build dist
CFLAGS="-O2 -std=gnu11 -Wall -Wextra -Wno-unused-parameter -Wno-unused-function -Wno-format-truncation -Isrc"

# Prefer a fully static binary (no runtime surprises on device), fall back to
# dynamic linking against Android's bionic.
if $CC $CFLAGS -static src/*.c -o build/astra-android -lm -lpthread 2>/dev/null; then
  echo "linked statically"
else
  echo "static link failed, falling back to dynamic"
  $CC $CFLAGS src/*.c -o build/astra-android -lm -lpthread
fi
"$STRIP" build/astra-android 2>/dev/null || true
cp build/astra-android dist/astra
chmod +x dist/astra
ls -lh dist/astra
file dist/astra || true

bash "$(dirname "$0")/package-deb.sh" "$VERSION" aarch64
