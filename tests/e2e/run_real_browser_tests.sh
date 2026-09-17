#!/usr/bin/env bash
# Real browser end-to-end suite.
#
# 1. serves tests/e2e/site over http
# 2. starts `astra serve` in headless mode and runs the Puppeteer + go-rod tests
# 3. runs `astra bench` to measure real byte savings (lite on vs off)
# 4. repeats the Puppeteer test against `astra serve --mode=full` under Xvfb
#
# Env: ASTRA_ENGINE (path to a chromium-family binary), CHROME_DIR (puppeteer cache)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

PORT="${PORT:-9222}"
FULL_PORT="${FULL_PORT:-9223}"
SITE_PORT="${SITE_PORT:-8123}"
TEST_URL="http://127.0.0.1:${SITE_PORT}/index.html"
OUT="${OUT:-/tmp/astra-e2e}"
mkdir -p "$OUT"
rc=0

log() { echo; echo "=== $* ==="; }

wait_http() { # url timeout
  local url="$1" t="${2:-30}" i
  for ((i = 0; i < t * 4; i++)); do
    if curl -fsS "$url" >/dev/null 2>&1; then return 0; fi
    sleep 0.25
  done
  return 1
}

find_engine() {
  if [[ -n "${ASTRA_ENGINE:-}" ]]; then echo "$ASTRA_ENGINE"; return; fi
  local c
  for c in "$HOME/.cache/puppeteer/chrome/"*/chrome-linux64/chrome \
           "$HOME/.cache/puppeteer/chrome-headless-shell/"*/chrome-headless-shell-linux64/chrome-headless-shell \
           /usr/bin/chromium /usr/bin/google-chrome; do
    [[ -x "$c" ]] && { echo "$c"; return; }
  done
  echo ""
}

ENGINE="$(find_engine)"
if [[ -z "$ENGINE" ]]; then
  echo "no chromium-family engine found (set ASTRA_ENGINE or install chrome)" >&2
  exit 1
fi
echo "engine: $ENGINE"

STEP_TIMEOUT="${STEP_TIMEOUT:-420}"

cleanup() {
  [[ -n "${ASTRA_PID:-}" ]] && kill "$ASTRA_PID" 2>/dev/null
  [[ -n "${SITE_PID:-}" ]] && kill "$SITE_PID" 2>/dev/null
  [[ -n "${XVFB_PID:-}" ]] && kill "$XVFB_PID" 2>/dev/null
  # engine processes we spawned (all carry --remote-debugging-port)
  pkill -f -- "--remote-debugging-port" 2>/dev/null
}
trap cleanup EXIT

log "generate test assets"
if [[ ! -f tests/e2e/site/assets/photo-1.png ]]; then
  bash tests/e2e/site/generate_assets.sh || echo "asset generation failed" >&2
fi
if [[ -f tests/e2e/site/assets/clip.webm ]]; then export HAS_VIDEO=1; fi

log "serve test site on :$SITE_PORT"
( cd tests/e2e/site && python3 -m http.server "$SITE_PORT" >"$OUT/site.log" 2>&1 ) &
SITE_PID=$!
wait_http "$TEST_URL" 20 || { echo "test site not reachable" >&2; exit 1; }

log "start astra (headless)"
./build/astra serve --engine "$ENGINE" --port "$PORT" --log-level 3 \
  --cache-dir "$OUT/cache" --profile "$OUT/profile-headless" >"$OUT/astra-headless.log" 2>&1 &
ASTRA_PID=$!
wait_http "http://127.0.0.1:$PORT/json/version" 40 || {
  echo "astra did not start:" >&2; cat "$OUT/astra-headless.log" >&2; exit 1; }

log "puppeteer (headless mode)"
export ASTRA_HTTP="http://127.0.0.1:$PORT" TEST_URL OUT_DIR="$OUT"
( cd tests/e2e/puppeteer && npm install --silent --no-fund --no-audit >/dev/null 2>&1; \
    timeout "$STEP_TIMEOUT" node test.mjs ) || rc=1

log "go-rod"
if command -v go >/dev/null 2>&1; then
  ( cd tests/e2e/gorod && go mod tidy >/dev/null 2>&1; \
    ASTRA_HTTP="http://127.0.0.1:$PORT" TEST_URL="$TEST_URL" \
    timeout "$STEP_TIMEOUT" go test -timeout 300s -v ./... ) || rc=1
else
  echo "go not installed - skipping go-rod tests"
fi

log "astra bench (real byte savings, lite on vs off)"
timeout "$STEP_TIMEOUT" ./build/astra bench "$TEST_URL" --engine "$ENGINE" --wait 1500 \
  --cache-dir "$OUT/cache-bench" --profile "$OUT/profile-bench" --log-level 2 | tee "$OUT/bench.txt"
if ! grep -q "lite mode moved" "$OUT/bench.txt"; then
  echo "bench did not produce a comparison" >&2
  rc=1
fi

kill "$ASTRA_PID" 2>/dev/null; ASTRA_PID=""; sleep 1

log "start astra (full / windowed mode under Xvfb)"
if command -v Xvfb >/dev/null 2>&1; then
  Xvfb :99 -screen 0 1600x1000x24 >"$OUT/xvfb.log" 2>&1 &
  XVFB_PID=$!
  sleep 2
  DISPLAY=:99 ./build/astra serve --engine "$ENGINE" --port "$FULL_PORT" --mode=full \
    --log-level 3 --cache-dir "$OUT/cache-full" --profile "$OUT/profile-full" \
    >"$OUT/astra-full.log" 2>&1 &
  ASTRA_PID=$!
  if wait_http "http://127.0.0.1:$FULL_PORT/json/version" 40; then
    ( cd tests/e2e/puppeteer && ASTRA_HTTP="http://127.0.0.1:$FULL_PORT" TEST_URL="$TEST_URL" \
      OUT_DIR="$OUT" timeout "$STEP_TIMEOUT" node test.mjs ) || rc=1
  else
    echo "astra --mode=full did not start:" >&2; cat "$OUT/astra-full.log" >&2; rc=1
  fi
else
  echo "Xvfb not installed - skipping windowed mode test"
fi

log "done"
echo "artifacts in $OUT"
exit $rc
