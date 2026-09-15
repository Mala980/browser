#!/usr/bin/env bash
# Turn a cargo build/test log into GitHub annotations.
#
# Raw action logs are not always retrievable from automation, and iterating on a
# Rust codebase without seeing `rustc` output is hopeless, so CI pipes every build
# through this: each `error[Exxxx]: ...` block becomes an annotation on the step.
#
# usage: scripts/ci-report.sh <logfile> [--max 12]
set -uo pipefail
log="${1:-/dev/stdin}"
max=12
if [ "${2:-}" = "--max" ]; then max="${3:-12}"; fi

if [ ! -s "$log" ]; then
  echo "::warning::no build log captured"
  exit 0
fi

python3 - "$log" "$max" <<'PY'
import re, sys
path, maxn = sys.argv[1], int(sys.argv[2])
try:
    text = open(path, encoding="utf-8", errors="replace").read()
except OSError as e:
    print(f"::warning::cannot read log: {e}")
    sys.exit(0)

def esc(s):
    return (s.replace("%", "%25").replace("\r", "").replace("\n", "%0A")
             .replace("]", "%5D"))

blocks = re.split(r"(?m)^(?=(?:error|warning: unused)[^\n]*\n)", text)
SKIP = ("error: aborting due to", "error: could not compile", "error: could not find")
errors = [b for b in blocks if b.startswith("error") and not b.startswith(SKIP)]
if not errors:
    tail = text[-3500:]
    print("::error::build failed without a recognised rustc error block%0A%0A" + esc(tail))
    sys.exit(0)

# rustc prints each diagnostic as "error[E0308]: message\n  --> file:line:col\n..."
for b in errors[:maxn]:
    lines = [l.rstrip() for l in b.splitlines() if l.strip()]
    head = lines[0][:300]
    loc = next((l.strip() for l in lines if l.strip().startswith("-->")), "")
    ctx = " | ".join(l.strip() for l in lines[1:6])[:1200]
    title = re.sub(r"^error(\[[^\]]+\])?:\s*", "", head)
    print(f"::error title=rustc error::{esc(loc)}%0A{esc(head)}%0A{esc(ctx)}")
extra = len(errors) - maxn
if extra > 0:
    print(f"::warning title=more errors::{extra} further rustc error(s) in the log")
sys.exit(1)
PY
