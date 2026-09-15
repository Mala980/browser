#!/usr/bin/env bash
# Cross-check Kilat's hand-written codecs against independent implementations.
#
# This exists because the engine reimplements DEFLATE, PNG, JPEG and the zlib
# framing that PNG needs: matching ourselves against zlib/libjpeg/ImageMagick is
# the only way to know the decoders are *correct* rather than self-consistent.
#
# usage: scripts/ci-codec-crosscheck.sh ./target/release/kilat
set -uo pipefail

KILAT="${1:-./target/release/kilat}"
DATA="${DATA:-tests/data}"
fail=0
ok() { echo "  ok   $*"; }
bad() { echo "  FAIL $*"; fail=$((fail + 1)); }

have() { command -v "$1" >/dev/null 2>&1; }

echo "== inflate: our DEFLATE vs system gzip"
for f in "$DATA"/*.gz; do
  base="${f%.gz}"
  [ -f "$base.bin" ] || continue
  if "$KILAT" dev inflate < "$f" | cmp -s - "$base.bin"; then
    ok "gunzip $(basename "$f")"
  else
    bad "gunzip $(basename "$f")"
  fi
done

echo "== deflate: our compressor must be readable by gunzip"
for f in "$DATA"/*.bin; do
  if "$KILAT" dev gzip < "$f" 2>/dev/null | gunzip -c 2>/dev/null | cmp -s - "$f"; then
    ok "gzip roundtrip $(basename "$f")"
  else
    bad "gzip roundtrip $(basename "$f")"
  fi
done
if "$KILAT" dev deflate < "$DATA/html10k.bin" | python3 -c '
import sys, zlib
data = sys.stdin.buffer.read()
raw = open(sys.argv[1], "rb").read()
out = zlib.decompress(data, -15)
sys.exit(0 if out == raw else 1)
' "$DATA/html10k.bin"; then
  ok "raw deflate readable by python zlib"
else
  bad "raw deflate readable by python zlib"
fi

echo "== PNG: encode/decode roundtrip + ImageMagick agreement"
if "$KILAT" dev png-decode < "$DATA/ref.png" > /tmp/kilat.raw; then
  if convert "$DATA/ref.png" -depth 8 RGBA:/tmp/im.png.raw 2>/dev/null; then
    if python3 - <<'PY'
import sys
def body(p, header):
    d = open(p, 'rb').read()
    if header:
        nl = d.index(b'\n') + 1
        d = d[nl:]
    return d
a = body('/tmp/kilat.raw', True)
b = body('/tmp/im.png.raw', False)
sys.exit(0 if a == b else 1)
PY
    then
      ok "png decode matches ImageMagick byte for byte"
    else
      bad "png decode differs from ImageMagick"
    fi
  else
    bad "convert failed"
  fi
  if "$KILAT" dev png-encode < /tmp/kilat.raw > /tmp/kilat.png &&
     "$KILAT" dev png-decode < /tmp/kilat.png > /tmp/kilat2.raw; then
    if cmp -s /tmp/kilat.raw /tmp/kilat2.raw; then
      ok "png encode/decode is lossless"
    else
      bad "png encode/decode lost data"
    fi
  else
    bad "png encode failed"
  fi
else
  bad "kilat could not decode $DATA/ref.png"
fi

echo "== JPEG: our IDCT/colour path vs libjpeg (djpeg)"
if have djpeg; then
  for j in baseline yuv444; do
    [ -f "$DATA/$j.jpg" ] || continue
    if "$KILAT" dev jpeg-decode < "$DATA/$j.jpg" > /tmp/k.jpg.raw &&
       djpeg -ppm -nosmooth "$DATA/$j.jpg" > /tmp/d.jpg.ppm 2>/dev/null; then
      if python3 - <<'PY'
# Compare Kilat's RGBA against djpeg's P6 PPM with a tolerance for IDCT and
# chroma-upsampling differences (both are correct, they just round differently).
k = open('/tmp/k.jpg.raw', 'rb').read()
nl = k.index(b'\n')
dims = k[:nl].decode().split('x')
w, h = int(dims[0]), int(dims[1])
kr = k[nl + 1:]
p = open('/tmp/d.jpg.ppm', 'rb').read()
# P6\n<w> <h>\n255\n
parts = p.split(b'\n', 2)
hdr = parts[0] + parts[1]
rest = p[len(hdr) + 1:]
maxv = int(parts[1].split()[-1]) if len(parts) > 1 else 255
px = rest[:len(rest) - (len(rest) % 3)]
scale = 255 / maxv if maxv else 1
n = min(len(kr) // 4, len(px) // 3, w * h)
acc = 0
for i in range(n):
    for c in range(3):
        acc += abs(kr[i * 4 + c] - int(px[i * 3 + c] * scale))
mean = acc / (n * 3)
print(f"    mean abs diff per channel: {mean:.3f} over {n} pixels")
raise SystemExit(0 if mean < 6.0 else 1)
PY
      then
        ok "jpeg $j matches libjpeg within tolerance"
      else
        bad "jpeg $j differs from libjpeg"
      fi
    else
      bad "jpeg $j decode failed"
    fi
  done
else
  echo "  skip (no djpeg)"
fi

echo "== progressive jpeg must be reported, not misdecoded"
if [ -f "$DATA/progressive.jpg" ]; then
  if "$KILAT" dev jpeg-decode < "$DATA/progressive.jpg" 2>/dev/null | head -c 1 | grep -q .; then
    : # we accept it only if an external decoder is wired; check the error text
  fi
  if "$KILAT" dev jpeg-decode < "$DATA/progressive.jpg" 2>&1 | grep -qi "progressive\|external"; then
    ok "progressive reported clearly"
  else
    bad "progressive handling unclear"
  fi
fi

echo
if [ "$fail" = 0 ]; then
  echo "all codec cross-checks passed"
else
  echo "$fail codec cross-check(s) failed"
  exit 1
fi
