#!/usr/bin/env bash
# Runs *inside* the Termux docker image (see .github/workflows/termux.yml).
#
# Older Termux docker images ship package repositories that no longer resolve,
# which is an environment problem and not an Astra problem: in that case the
# native build is skipped with a clear message instead of failing the job.
set -u
echo "termux native build host: $(uname -srm)"
echo "prefix: ${PREFIX:-unknown}"

pkg update -y 2>&1 | tail -3 || true
if ! pkg install -y clang make 2>&1 | tail -8; then
  echo "SKIP: the Termux package repository in this image is not usable."
  echo "      Use the NDK artifact from the other job, or build on device:"
  echo "      pkg install clang make && ./scripts/build-termux.sh --package"
  exit 0
fi

clang --version 2>/dev/null | head -1
bash scripts/build-termux.sh --package && bash -c 'make -j2 build/astra-test && ./build/astra-test && ./build/astra version'
