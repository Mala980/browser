#!/usr/bin/env bash
# Publish a CI log to the `ci-diagnostics` branch.
#
# GitHub Actions keeps logs in blob storage that is not reachable from every
# environment (and `gh run view --log` needs it too).  Committing the log to a
# dedicated branch makes it readable through the plain repository API, which is
# how CI failures are investigated on this project.
set -uo pipefail
NAME="${1:-job}"
LOG="${2:-/tmp/ci.log}"
BRANCH="ci-diagnostics"
[[ -f "$LOG" ]] || { echo "no log at $LOG"; exit 0; }
git config user.email "ci@astra.local"
git config user.name "Astra CI"

STAMP="$(date -u +%Y%m%dT%H%M%S)"
for attempt in 1 2 3; do
  git fetch origin "$BRANCH" >/dev/null 2>&1
  if git rev-parse --verify "origin/$BRANCH" >/dev/null 2>&1; then
    git checkout -q -B "$BRANCH" "origin/$BRANCH" || exit 0
  else
    git checkout -q -B "$BRANCH" || exit 0
  fi
  mkdir -p logs
  cp "$LOG" "logs/${NAME}-run${GITHUB_RUN_ID:-0}-${STAMP}.txt"
  git add logs >/dev/null 2>&1
  if ! git commit -q -m "ci log: ${NAME} (run ${GITHUB_RUN_ID:-0})" >/dev/null 2>&1; then
    echo "nothing to commit"; exit 0
  fi
  if git push origin "$BRANCH" >/dev/null 2>&1; then
    echo "diagnostics published: logs/${NAME}-run${GITHUB_RUN_ID:-0}-${STAMP}.txt"
    exit 0
  fi
  echo "push attempt $attempt failed (concurrent job?), retrying"
  sleep $((attempt * 3))
done
echo "could not publish diagnostics"
