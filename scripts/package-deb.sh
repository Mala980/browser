#!/usr/bin/env bash
# Package the astra binary as a Termux .deb (and a plain tar.xz).
#
#   ./scripts/package-deb.sh 0.1.0 aarch64
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
cd "$HERE"
VERSION="${1:-0.1.0}"
ARCH="${2:-aarch64}"
BIN="${3:-dist/astra}"
[[ -f "$BIN" ]] || { echo "$BIN not found" >&2; exit 1; }

STAGE="build/deb/astra_${VERSION}_${ARCH}"
rm -rf "build/deb"
mkdir -p "$STAGE/DEBIAN" "$STAGE/data/data/com.termux/files/usr/bin" \
         "$STAGE/data/data/com.termux/files/usr/share/doc/astra"
install -m 0755 "$BIN" "$STAGE/data/data/com.termux/files/usr/bin/astra"
cp README.md "$STAGE/data/data/com.termux/files/usr/share/doc/astra/README.md" 2>/dev/null || true

cat > "$STAGE/DEBIAN/control" <<EOF
Package: astra
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: Astra contributors
Depends: chromium
Section: net
Priority: optional
Homepage: https://github.com/Mala980/browser
Description: lightweight CDP browser control plane with a bandwidth saving proxy
 Astra is a small native binary (single file, no runtime) that drives a
 Chromium-family rendering engine over the DevTools Protocol and optimises
 every request on the way: ad/tracker blocking, image re-encoding and
 downscaling, HTML/CSS minification, lazy loading and an on-disk cache.
 It speaks CDP, so Puppeteer and go-rod can connect to it directly.
EOF

cat > "$STAGE/DEBIAN/postinst" <<'EOF'
#!/bin/bash
echo "astra installed. Usage:"
echo "  astra serve --port 9222            # headless control plane"
echo "  astra open https://example.com     # one shot load"
echo "  astra bench https://example.com    # measure real byte savings"
echo "It needs a chromium-family engine: pkg install chromium"
EOF
chmod 0755 "$STAGE/DEBIAN/postinst"

mkdir -p dist
if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb --build --root-owner-group "$STAGE" "dist/astra_${VERSION}_${ARCH}.deb"
  echo "built dist/astra_${VERSION}_${ARCH}.deb"
fi
tar -cJf "dist/astra-${VERSION}-termux-${ARCH}.tar.xz" -C "$(dirname "$BIN")" "$(basename "$BIN")"
echo "built dist/astra-${VERSION}-termux-${ARCH}.tar.xz"
ls -lh dist
