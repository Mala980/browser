#!/usr/bin/env bash
# Generate the test assets (big PNG photos + a short WebM clip) used by the
# real-browser end-to-end tests.  Requires python3 (pillow) and ffmpeg.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$HERE/assets"

echo "generating png assets"
python3 - "$HERE/assets" <<'PY'
import os, sys, random, zlib, struct
out = sys.argv[1]
try:
    from PIL import Image, ImageDraw
    have_pil = True
except Exception:
    have_pil = False

def write_png(path, w, h, seed):
    if have_pil:
        import random as r
        r.seed(seed)
        img = Image.new('RGB', (w, h))
        px = img.load()
        for y in range(0, h, 2):
            for x in range(0, w, 2):
                c = (r.randrange(256), r.randrange(256), r.randrange(256))
                px[x, y] = c
                if x + 1 < w: px[x + 1, y] = c
                if y + 1 < h:
                    px[x, y + 1] = c
                    if x + 1 < w: px[x + 1, y + 1] = c
        d = ImageDraw.Draw(img)
        d.rectangle([10, 10, w - 10, h - 10], outline=(255, 255, 255), width=8)
        img.save(path, optimize=False, compress_level=1)
        return
    # fallback: pure python PNG (noise, uncompressed-ish)
    random.seed(seed)
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        for x in range(w):
            raw += bytes((random.randrange(256), random.randrange(256), random.randrange(256)))
    def chunk(t, d):
        return (struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff))
    ihdr = struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)
    with open(path, 'wb') as f:
        f.write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) +
                chunk(b'IDAT', zlib.compress(bytes(raw), 1)) + chunk(b'IEND', b''))

for i, (w, h) in enumerate([(2400, 1600), (2000, 1400), (1600, 1200)], start=1):
    p = os.path.join(out, 'photo-%d.png' % i)
    write_png(p, w, h, i * 17)
    print('  %s (%d bytes)' % (p, os.path.getsize(p)))
PY

echo "generating video asset"
if command -v ffmpeg >/dev/null 2>&1; then
  ffmpeg -y -loglevel error -f lavfi -i testsrc=size=640x360:rate=25:duration=3 \
      -c:v libvpx-vp9 -b:v 800k -pix_fmt yuv420p "$HERE/assets/clip.webm"
  ffmpeg -y -loglevel error -f lavfi -i testsrc=size=640x360:rate=25:duration=3 \
      -c:v libx264 -pix_fmt yuv420p -movflags +faststart "$HERE/assets/clip.mp4" || true
  ls -l "$HERE/assets"
else
  echo "ffmpeg not available - video tests will be skipped" >&2
fi
