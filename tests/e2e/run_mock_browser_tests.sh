#!/usr/bin/env bash
# Puppeteer against astra + the protocol level mock engine.
#
# No browser binary needed, runs in about ten seconds, and it covers the CDP
# surface client libraries actually depend on: discovery and auto attach,
# session translation, navigation and lifecycle events, the Astra domain on a
# page session, screenshots, multiple pages.  Everything that needs a real
# renderer (DOM, video, image decoding, network savings) is skipped here and
# covered by tests/e2e/run_real_browser_tests.sh in CI.
#
#   bash tests/e2e/run_mock_browser_tests.sh      # or: make puppeteer
set -u
cd "$(dirname "$0")/../.."

PORT_MOCK="${PORT_MOCK:-19222}"
PORT="${PORT:-19233}"
OUT="${OUT:-/tmp/astra-mock-e2e}"
STEP_TIMEOUT="${STEP_TIMEOUT:-120}"

mkdir -p "$OUT"
[ -x ./build/astra ] || make

python3 tests/e2e/mock_engine.py --port "$PORT_MOCK" >"$OUT/mock.log" 2>&1 &
MOCK_PID=$!
./build/astra serve --engine-url "ws://127.0.0.1:$PORT_MOCK/devtools/browser/mock" \
  --port "$PORT" --log-level 3 --cache-dir "$OUT/cache" >"$OUT/astra.log" 2>&1 &
ASTRA_PID=$!
cleanup() {
  kill "$ASTRA_PID" 2>/dev/null
  kill "$MOCK_PID" 2>/dev/null
  wait "$ASTRA_PID" 2>/dev/null
  wait "$MOCK_PID" 2>/dev/null
}
trap cleanup EXIT

WS=""
for _ in $(seq 60); do
  WS="$(curl -fsS "http://127.0.0.1:$PORT/json/version" 2>/dev/null |
    sed -n 's/.*"webSocketDebuggerUrl": *"\([^"]*\)".*/\1/p')"
  [ -n "$WS" ] && break
  sleep 0.25
done
if [ -z "$WS" ]; then
  echo "astra did not start:" >&2
  cat "$OUT/astra.log" >&2
  exit 1
fi

# node cannot resolve puppeteer-core before the dependencies are installed
( cd tests/e2e/puppeteer && npm install --silent --no-fund --no-audit )

rc=0
( cd tests/e2e/puppeteer && ASTRA_WS="$WS" ASTRA_MOCK=1 TEST_URL="http://example.test/" \
    OUT_DIR="$OUT" timeout "$STEP_TIMEOUT" node test.mjs ) || rc=1
exit "$rc"
