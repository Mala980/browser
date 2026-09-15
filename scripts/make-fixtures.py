#!/usr/bin/env python3
"""Generate the byte-level fixtures the Rust unit tests compare against.

The engine has no way to fetch a reference decompressor at test time, so the
ground truth is committed: raw payloads plus their gzip/zlib encodings (made by
zlib, i.e. a completely independent DEFLATE implementation) and a PNG made by
this script plus a pixel checksum.

    python3 scripts/make-fixtures.py [outdir]

Safe to re-run; only writes into tests/data.
"""
import os
import random
import shutil
import subprocess
import struct
import sys
import zlib


def write(outdir, name, data):
    if isinstance(data, str):
        data = data.encode()
    path = os.path.join(outdir, name)
    with open(path, "wb") as f:
        f.write(data)
    return len(data)


def png_rgba(w, h, pixels):
    """Minimal RGBA8 PNG writer (filter 0 on every row)."""
    raw = b"".join(b"\x00" + pixels[y * w * 4:(y + 1) * w * 4] for y in range(h))

    def chunk(tag, body):
        return (struct.pack(">I", len(body)) + tag + body
                + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


def main():
    outdir = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tests", "data")
    os.makedirs(outdir, exist_ok=True)
    total = 0

    payloads = {
        "tiny": b"hello",
        "html10k": (b"<div class=\"row\"><a href=\"/x\">link</a></div>\n" * 270)[:10_800],
        "zeros64k": bytes(64 * 1024),
        "rand4k": bytes(random.Random(1234).getrandbits(8) for _ in range(4096)),
        # A 66 KiB payload forces at least one dynamic+static block mix in zlib.
        "mixed90k": (b"The quick brown fox. " * 3000 + bytes(range(256)) * 100)[:92_160],
    }
    for name, raw in payloads.items():
        total += write(outdir, f"{name}.bin", raw)
        total += write(outdir, f"{name}.gz", zlib.compressobj(9, zlib.DEFLATED, 31).compress(raw)
                       + zlib.compressobj(9, zlib.DEFLATED, 31).flush())
        z = zlib.compressobj(9, zlib.DEFLATED, 15)
        total += write(outdir, f"{name}.zz", z.compress(raw) + z.flush())
        # Stored (uncompressed) blocks: exercises BTYPE=00 parsing.
        zs = zlib.compressobj(0, zlib.DEFLATED, 15)
        total += write(outdir, f"{name}.stored.zz", zs.compress(raw) + zs.flush())

    # 37x23 gradient PNG with some alpha, plus the checksum the Rust test asserts.
    w, h = 37, 23
    px = bytearray()
    for y in range(h):
        for x in range(w):
            px += bytes([(x * 255) // w, (y * 255) // (h - 1), ((x + y) * 7) & 0xFF,
                         0 if (x * y) % 11 == 0 else 255])
    total += write(outdir, "ref.png", png_rgba(w, h, bytes(px)))
    total += write(outdir, "ref.expected.txt", f"{w} {h} {sum(px)}\n")

    # A tiny hand-built GIF (2x2, no interlacing) for the GIF decoder tests.
    gif = (b"GIF89a" + struct.pack("<HH", 2, 2) + bytes([0x80, 0, 0])
           + bytes([0, 0, 0, 255, 255, 255])
           + b"\x2c" + struct.pack("<HHHH", 0, 0, 2, 2) + bytes([0])
           + bytes([2, 4, 0x84, 0x18, 0x20, 0x05, 0]) + b"\x3b")
    total += write(outdir, "ref.gif", gif)
    total += write(outdir, "ref.gif.expected.txt", f"2 2 {sum([0,0,0,255]*3 + [255,255,255,255])}\n")

    # JPEG fixtures need an encoder; use ImageMagick when it is installed.
    # `baseline.ref.rgba` is the decoded reference (RGBA8, straight) that the Rust
    # test compares its own IDCT/colour-conversion output against.
    if shutil.which("convert"):
        subprocess.check_call([
            "convert", "-size", "64x48", "gradient:#203040-#c0d0e0",
            "-colorspace", "sRGB", os.path.join(outdir, "gradient.png")])
        subprocess.check_call([
            "convert", os.path.join(outdir, "gradient.png"), "-quality", "85",
            "-sampling-factor", "2x2", "-interlace", "none",
            os.path.join(outdir, "baseline.jpg")])
        subprocess.check_call([
            "convert", os.path.join(outdir, "baseline.jpg"), "-depth", "8",
            "RGBA:" + os.path.join(outdir, "baseline.ref.rgba")])
        # Grayscale single-component file, and a 4:4:4 file (no chroma subsampling).
        subprocess.check_call([
            "convert", os.path.join(outdir, "gradient.png"), "-colorspace", "Gray",
            "-quality", "90", os.path.join(outdir, "gray.jpg")])
        subprocess.check_call([
            "convert", os.path.join(outdir, "gradient.png"), "-depth", "8",
            "RGBA:" + os.path.join(outdir, "gray.ref.rgba")])
        subprocess.check_call([
            "convert", os.path.join(outdir, "gradient.png"), "-quality", "92",
            "-sampling-factor", "1x1", "-interlace", "none",
            os.path.join(outdir, "yuv444.jpg")])
        # Progressive file: must be detected and reported, not misdecoded.
        subprocess.check_call([
            "convert", os.path.join(outdir, "gradient.png"), "-quality", "85",
            "-interlace", "JPEG", os.path.join(outdir, "progressive.jpg")])
        for nm in ["baseline.jpg", "gray.jpg", "yuv444.jpg", "progressive.jpg"]:
            fp = os.path.join(outdir, nm)
            total += os.path.getsize(fp)
            print(f"  {nm}: {os.path.getsize(fp)} bytes")
        w2, h2 = 64, 48
        with open(os.path.join(outdir, "baseline.expected.txt"), "w") as f:
            f.write(f"{w2} {h2}\n")

    print(f"{outdir}: wrote {total} bytes of fixtures")


if __name__ == "__main__":
    main()
