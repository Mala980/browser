#!/usr/bin/env bash
# Turn a cargo build/test log into GitHub annotations.
#
# Raw action logs are not always retrievable from automation (this project is
# developed in a sandbox where CI is the compiler), and rustc output is the one
# thing that must survive. So CI pipes every build through here: diagnostics are
# stripped of ANSI colours, compacted to `file:line:col message` and emitted as a
# couple of annotations that the API can read back in full.
#
# usage: scripts/ci-report.sh <logfile>
set -uo pipefail
log="${1:-/dev/stdin}"

if [ ! -s "$log" ]; then
  echo "::warning::no build log captured"
  exit 0
fi

python3 - "$log" <<'PY'
import re, sys
path = sys.argv[1]
try:
    raw = open(path, encoding="utf-8", errors="replace").read()
except OSError as e:
    print(f"::warning::cannot read log: {e}")
    sys.exit(0)

ansi = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
text = ansi.sub("", raw)


def esc(s, limit=58000):
    s = s[:limit]
    return s.replace("%", "%25").replace("\r", "").replace("\n", "%0A").replace("]", "%5D")


# One line per diagnostic: "error[E0308]: msg" followed by its first "-->" location.
items = []
lines = text.splitlines()
i = 0
while i < len(lines):
    l = lines[i].strip()
    m = re.match(r"^(error(?:\[[EW]\d+\])?!?): (.*?)(?:\[E\d+\])?$", l)
    if m and not l.startswith(("error: aborting", "error: could not")):
        kind, msg = m.group(1), l[len(m.group(1)) + 2:].strip()
        loc = ""
        for j in range(i + 1, min(i + 8, len(lines))):
            s = lines[j].strip()
            if s.startswith("-->"):
                loc = s[3:].strip()
                break
            if s.startswith(("error", "warning")):
                break
        items.append((kind, loc, msg))
    elif l.startswith("warning: ") and "generated" not in l and i > 0:
        pass
    i += 1

if not items:
    print("::error title=build failure::" + esc(text[-6000:]))
    sys.exit(1)

compact = []
for kind, loc, msg in items[:120]:
    compact.append(f"{kind} {loc}: {msg}" if loc else f"{kind}: {msg}")
print(f"::error title=rustc diagnostics ({len(items)})::" + esc("\n".join(compact)))

# One full block (the first error, with its snippet) for context.
first = re.search(r"(?m)^error(?:\[[EW]?\d*\])?: .*$", text)
if first:
    block = text[first.start():first.start() + 2400]
    print("::error title=first error (annotated)::" + esc(block))

# Test failures are reported as "failures:" with a name list.
tf = re.search(r"(?m)^failures:$", text)
if tf:
    print("::error title=test failures::" + esc(text[tf.start():tf.start() + 3000]))

# ...but the interesting part is each failing test's stdout block, which appears
# before the summary, and the panic lines with the two lines under them (that is
# where the assertion message lives).
blocks = re.findall(r"(?m)^---- (.+?) stdout ----\n(.*?)(?=^---- |^failures:|\Z)", text, re.S)
if blocks:
    parts = []
    for name, body in blocks[:10]:
        body = body.strip().replace("test panicked: ", "")
        parts.append(f"---- {name} ----\n{body[:700]}")
    print("::error title=test panic details::" + esc("\n".join(parts)))

lines = text.splitlines()
pans = []
for k, l in enumerate(lines):
    if "panicked at" in l:
        chunk = [x.strip() for x in lines[k:k + 3]]
        pans.append(" ".join(chunk)[:300])
if pans:
    print("::error title=panic lines::" + esc("\n".join(pans[:30])))

# Test binaries that died before running (link/compile of the test harness).
for sig in ("error: test failed", "error: process didn't exit successfully", "signal:"):
    idx = text.find(sig)
    if idx >= 0:
        print(f"::error title=harness note::" + esc(text[idx:idx + 400]))
        break
sys.exit(1)
PY
