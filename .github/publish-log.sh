#!/usr/bin/env bash
# Publish a CI log to the `ci-diagnostics` branch.
#
# GitHub Actions stores logs in blob storage that is not reachable from every
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
git fetch origin "$BRANCH" >/dev/null 2>&1
git checkout -q -B "$BRANCH" 2>/dev/null || exit 0
mkdir -p logs
cp "$LOG" "logs/${NAME}-run${GITHUB_RUN_ID:-0}-$(date -u +%Y%m%dT%H%M%S).txt"
git add logs >/dev/null 2>&1
git commit -q -m "ci log: ${NAME} (run ${GITHUB_RUN_ID:-0})" >/dev/null 2>&1 || exit 0
git push -q origin "$BRANCH" >/dev/null 2>&1 || echo "could not push diagnostics"
