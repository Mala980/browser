#!/usr/bin/env python3
"""Build the font fixtures the Rust tests parse (no font tools required).

  tests/data/kilat-mini.ttf   4-glyph TrueType: .notdef box, space, A, V, with
                              head/hhea/maxp/cmap(fmt4)/hmtx/loca/glyf/name/
                              OS2(v2)/post(kern fmt0)
  tests/data/kilat-mini.woff  the same tables in an uncompressed WOFF directory

Metrics the Rust tests assert (they live here so both sides agree):

  unitsPerEm      1000
  ascender/desc/lineGap  800 / -200 / 0
  advances        gid0 600, gid1 250, gid2 720, gid3 720
  cmap            0x20->1  0x2E->0  0x41->2  0x56->3  0x61->2
  kern            (A,V) = -80
  name            family "Kilat Mini", subfamily "Regular"
  OS/2            usWeightClass 400, fsSelection bit6 (REGULAR),
                  sxHeight 500, sCapHeight 700

Tables are assembled by byte offset (not one giant struct.pack) so the fixture
stays readable; `--check` re-parses the file with an independent mini-reader.
"""
import os
import struct
import sys

UNITS = 1000
GLYPHS = [
    # (contour points [(x, y, on_curve)], advance, lsb)
    ([(50, 0, 1), (550, 0, 1), (550, 700, 1), (50, 700, 1)], 600, 50),
    ([], 250, 0),
    ([(0, 0, 1), (100, 0, 1), (360, 520, 1), (620, 0, 1), (720, 0, 1),
      (360, 700, 1)], 720, 0),
    ([(0, 700, 1), (100, 700, 1), (360, 180, 1), (620, 700, 1), (720, 700, 1),
      (400, 0, 1)], 720, 0),
]
CMAP = {0x20: 1, 0x2E: 0, 0x41: 2, 0x56: 3, 0x61: 2}
KERN = [(2, 3, -80)]


def u16(v):
    return struct.pack(">H", v & 0xFFFF)


def i16(v):
    return struct.pack(">h", v)


def u32(v):
    return struct.pack(">I", v & 0xFFFFFFFF)


def pad4(b):
    return b + b"\x00" * ((-len(b)) % 4)


def checksum(data):
    d = pad4(data)
    total = 0
    for i in range(0, len(d), 4):
        total = (total + struct.unpack(">I", d[i:i + 4])[0]) & 0xFFFFFFFF
    return total


def tbl_head():
    b = bytearray(54)
    b[0:4] = u32(0x00010000)      # version
    b[4:8] = u32(0x00010000)      # fontRevision
    b[8:12] = u32(0)              # checkSumAdjustment (not validated by Kilat)
    b[12:16] = u32(0x5F0F3CF5)    # magicNumber
    b[16:18] = u16(0x000B)        # flags
    b[18:20] = u16(UNITS)         # unitsPerEm
    b[20:28] = struct.pack(">q", 0)   # created
    b[28:36] = struct.pack(">q", 0)   # modified
    b[36:38] = i16(0)             # xMin
    b[38:40] = i16(-200)          # yMin
    b[40:42] = i16(720)           # xMax
    b[42:44] = i16(700)          # yMax
    b[44:46] = u16(0)             # macStyle
    b[46:48] = u16(8)            # lowestRecPPEM
    b[48:50] = i16(2)            # fontDirectionHint
    b[50:52] = i16(0)            # indexToLocFormat: short
    b[52:54] = i16(0)            # glyphDataFormat
    return bytes(b)


def tbl_hhea():
    b = bytearray(36)
    b[0:4] = u32(0x00010000)
    b[4:6] = i16(800)             # ascender
    b[6:8] = i16(-200)            # descender
    b[8:10] = i16(0)              # lineGap
    b[10:12] = u16(720)           # advanceWidthMax
    b[12:14] = i16(0)             # minLeftSideBearing
    b[14:16] = i16(50)            # minRightSideBearing
    b[16:18] = i16(720)           # xMaxExtent
    b[18:20] = i16(1)             # caretSlopeRise
    b[20:22] = i16(0)             # caretSlopeRun
    b[22:24] = i16(0)             # caretOffset
    b[34:36] = u16(len(GLYPHS))   # numberOfHMetrics
    return bytes(b)


def tbl_maxp():
    b = bytearray(32)
    b[0:4] = u32(0x00010000)
    b[4:6] = u16(len(GLYPHS))
    b[6:8] = u16(6)               # maxPoints
    b[8:10] = u16(1)              # maxContours
    b[10:12] = u16(0)             # maxCompositePoints
    b[12:14] = u16(0)             # maxCompositeContours
    b[14:16] = u16(1)             # maxZones
    return bytes(b)


def tbl_cmap():
    segs = sorted(CMAP.items()) + [(0xFFFF, 0)]
    seg_count = len(segs)
    end = b"".join(u16(cp) for cp, _ in segs)
    start = b"".join(u16(cp) for cp, _ in segs)
    # format 4 maps cp -> cp + idDelta (mod 65536); the sentinel segment uses
    # delta 1 so every unassigned codepoint lands on .notdef.
    delta = b"".join(u16(gid - cp) for cp, gid in segs)
    rng = b"\x00\x00" * seg_count
    sub = bytearray()
    sub += u16(4)                                     # format
    length = 16 + 8 * seg_count                       # +2 reservedPad inside
    sub += u16(length)
    sub += u16(0)                                     # language
    sub += u16(2 * seg_count)                         # segCountX2
    sr = 2 * (1 << ((seg_count // 2).bit_length() - 1)) if seg_count >= 2 else 2
    es = (seg_count // 2).bit_length() - 1 if seg_count >= 2 else 0
    sub += u16(sr)                                    # searchRange
    sub += u16(es)                                    # entrySelector
    sub += u16(2 * seg_count - sr)                    # rangeShift
    sub += end + u16(0) + start + delta + rng
    # header: version(2) numTables(2) then one record: platform(2) encoding(2) offset(4)
    table = u16(0) + u16(1) + u16(3) + u16(1) + u32(12) + bytes(sub)
    return pad4(table)


def tbl_hmtx():
    return b"".join(u16(adv) + i16(lsb) for _, adv, lsb in GLYPHS)


def tbl_glyf_and_loca():
    glyf = b""
    offsets = []
    for pts, _adv, _lsb in GLYPHS:
        offsets.append(len(glyf))
        if not pts:
            continue
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        g = bytearray()
        g += i16(1)                        # numberOfContours
        g += i16(min(xs)) + i16(min(ys)) + i16(max(xs)) + i16(max(ys))
        g += u16(len(pts) - 1)             # endPtsOfContours
        g += u16(0)                        # instructionLength (no hinting here)
        g += bytes(0x01 if on else 0x00 for (_x, _y, on) in pts)
        prev = 0
        for x, _y, _on in pts:
            g += i16(x - prev)
            prev = x
        prev = 0
        for _x, y, _on in pts:
            g += i16(y - prev)
            prev = y
        glyf += pad4(bytes(g))
    offsets.append(len(glyf))
    loca = b"".join(u16(o // 2) for o in offsets)   # indexToLocFormat = 0
    return glyf, loca


def tbl_name():
    strings = {
        1: "Kilat Mini",
        2: "Regular",
        3: "KilatMini-Regular-2026",
        4: "Kilat Mini Regular",
        6: "KilatMini-Regular",
    }
    count = len(strings)
    storage = 6 + 12 * count
    records = b""
    data = b""
    for name_id, text in sorted(strings.items()):
        raw = text.encode("utf-16-be")
        # offsets in the records are relative to the storage area, per the spec
        records += u16(3) + u16(1) + u16(0x409) + u16(name_id) + u16(len(raw)) + u16(len(data))
        data += raw
    header = u16(0) + u16(count) + u16(storage)
    return pad4(header + records + data)


def tbl_os2():
    b = bytearray(96)
    b[0:2] = u16(2)               # version 2 (needed for sxHeight/sCapHeight)
    b[2:4] = u16(600)             # xAvgCharWidth
    b[4:6] = u16(400)            # usWeightClass
    b[6:8] = u16(5)              # usWidthClass (normal)
    b[8:10] = u16(0x0004)        # fsType (installable)
    b[30:32] = i16(0)            # sFamilyType
    b[32:42] = bytes([2, 2, 6, 3, 5, 4, 5, 2, 3, 5])  # panose
    b[42:46] = u32(0x00000041)   # ulUnicodeRange1: Basic Latin + Latin-1
    b[58:62] = b"KMNI"           # achVendID
    b[62:64] = u16(0x0040)       # fsSelection: bit6 REGULAR
    b[64:66] = u16(0x20)         # usFirstCharIndex
    b[66:68] = u16(0x7A)         # usLastCharIndex
    b[68:70] = i16(800)           # sTypoAscender
    b[70:72] = i16(-200)          # sTypoDescender
    b[72:74] = i16(0)             # sTypoLineGap
    b[74:76] = u16(700)           # usWinAscent
    b[76:78] = u16(200)           # usWinDescent
    b[86:88] = i16(500)           # sxHeight
    b[88:90] = i16(700)           # sCapHeight
    b[90:92] = u16(0)             # usDefaultChar
    b[92:94] = u16(0x20)         # usBreakChar
    b[94:96] = u16(0)            # maxContext
    return bytes(b)


def tbl_post():
    b = bytearray(32)
    b[0:4] = u32(0x00030000)      # format 3.0 (no glyph names)
    b[4:8] = struct.pack(">i", 0)  # italicAngle as 16.16 fixed
    b[8:10] = i16(0)              # underlinePosition
    b[10:12] = i16(50)            # underlineThickness
    b[12:16] = u32(0)             # isFixedPitch
    return bytes(b)


def tbl_kern():
    pairs = b"".join(u16(l) + u16(r) + i16(v) for l, r, v in KERN)
    n = len(KERN)
    sub = u16(0) + u16(14 + 6 * n) + u16(1) + u16(n) + u16(6 * n) + u16(
        n.bit_length() - 1 if n else 0) + u16(0)
    sub += pairs
    return pad4(u16(0) + u16(1) + sub)


def build_sfnt():
    tables = {
        "OS/2": tbl_os2(),
        "cmap": tbl_cmap(),
        "glyf": tbl_glyf_and_loca()[0],
        "head": tbl_head(),
        "hhea": tbl_hhea(),
        "hmtx": tbl_hmtx(),
        "kern": tbl_kern(),
        "loca": tbl_glyf_and_loca()[1],
        "maxp": tbl_maxp(),
        "name": tbl_name(),
        "post": tbl_post(),
    }
    tags = sorted(tables)
    num = len(tags)
    header_len = 12 + 16 * num
    off = header_len
    directory = b""
    body = b""
    for t in tags:
        d = tables[t]
        directory += t.encode("latin-1") + u32(checksum(d)) + u32(off) + u32(len(d))
        body += pad4(d)
        off += len(pad4(d))
    sr = 16 * (1 << (num.bit_length() - 1))
    es = num.bit_length() - 1
    out = u32(0x00010000) + u16(num) + u16(sr) + u16(es) + u16(num * 16 - sr)
    return out + directory + body


def build_woff(sfnt):
    tables = {}
    num = struct.unpack(">H", sfnt[4:6])[0]
    for i in range(num):
        base = 12 + 16 * i
        tag = sfnt[base:base + 4].decode("latin-1")
        _cs, off, length = struct.unpack(">III", sfnt[base + 4:base + 16])
        tables[tag] = sfnt[off:off + length]
    tags = sorted(tables)
    n = len(tags)
    dir_len = 20 * n
    data_off = 44 + dir_len
    directory = b""
    body = b""
    total_sfnt = 12 + 16 * n
    for t in tags:
        d = tables[t]
        od = pad4(d)
        # entry: tag, offset, compLength, origLength, origChecksum (20 bytes)
        directory += t.encode("latin-1") + u32(data_off + len(body)) + u32(
            len(d)) + u32(len(d)) + u32(checksum(d))
        body += od
        total_sfnt += len(od)
    # 44-byte header: sig, flavor, length, numTables, reserved, totalSfntSize,
    # majorVersion, minorVersion, meta{Offset,Length,CompLength,OrigLength}
    header = b"wOFF" + u32(0x00010000) + u32(44 + dir_len + len(body)) + u16(n) + u16(
        0) + u32(total_sfnt) + u32(1) + u32(0) + u32(0) + u32(0) + u32(0) + u32(0)
    assert len(header) == 44, len(header)
    return header + directory + body


def check(path):
    """Independent re-parse, so the fixture cannot silently lie."""
    data = open(path, "rb").read()
    num = struct.unpack(">H", data[4:6])[0]
    tables = {}
    for i in range(num):
        base = 12 + 16 * i
        tag = data[base:base + 4].decode("latin-1")
        _cs, off, length = struct.unpack(">III", data[base + 4:base + 16])
        tables[tag] = data[off:off + length]
    head = tables["head"]
    assert struct.unpack(">H", head[18:20])[0] == UNITS
    hhea = tables["hhea"]
    assert struct.unpack(">h", hhea[4:6])[0] == 800
    assert struct.unpack(">h", hhea[6:8])[0] == -200
    assert struct.unpack(">H", hhea[34:36])[0] == len(GLYPHS)
    cmap = tables["cmap"]
    assert struct.unpack(">H", cmap[2:4])[0] == 1, "one encoding record"
    assert struct.unpack(">I", cmap[8:12])[0] == 12, "subtable offset"
    fmt = struct.unpack(">H", cmap[12:14])[0]
    assert fmt == 4, fmt
    seg_x2 = struct.unpack(">H", cmap[18:20])[0]
    seg = seg_x2 // 2
    end_off = 26
    ends = [struct.unpack(">H", cmap[end_off + 2 * i:end_off + 2 * i + 2])[0] for i in range(seg)]
    start_off = end_off + 2 * seg + 2
    starts = [struct.unpack(">H", cmap[start_off + 2 * i:start_off + 2 * i + 2])[0] for i in range(seg)]
    delta_off = start_off + 2 * seg
    deltas = [struct.unpack(">h", cmap[delta_off + 2 * i:delta_off + 2 * i + 2])[0] for i in range(seg)]
    got = {}
    for i in range(seg):
        if ends[i] == 0xFFFF and starts[i] == 0xFFFF:
            continue
        for cp in range(starts[i], ends[i] + 1):
            got[cp] = (cp + deltas[i]) & 0xFFFF
    assert got == CMAP, (got, CMAP)
    hmtx = tables["hmtx"]
    advs = [struct.unpack(">H", hmtx[4 * i:4 * i + 2])[0] for i in range(len(GLYPHS))]
    assert advs == [600, 250, 720, 720], advs
    loca = tables["loca"]
    offs = [struct.unpack(">H", loca[2 * i:2 * i + 2])[0] * 2 for i in range(len(GLYPHS) + 1)]
    glyf = tables["glyf"]
    for gid, (pts, _a, _l) in enumerate(GLYPHS):
        s, e = offs[gid], offs[gid + 1]
        raw = glyf[s:e]
        if not pts:
            assert s == e, "empty glyph must have zero length"
            continue
        nc = struct.unpack(">h", raw[0:2])[0]
        assert nc == 1, nc
        npts = struct.unpack(">H", raw[10:12])[0] + 1
        assert npts == len(pts), (npts, len(pts))
        flags = raw[14:14 + npts]          # after the uint16 instructionLength
        assert all(f & 1 for f in flags), "all fixture points are on-curve"
        xo = 14 + npts
        xs = [struct.unpack(">h", raw[xo + 2 * i:xo + 2 * i + 2])[0] for i in range(npts)]
        xs2 = [sum(xs[:i + 1]) for i in range(npts)]
        assert xs2 == [p[0] for p in pts], (xs2, pts)
    name = tables["name"]
    cnt = struct.unpack(">H", name[2:4])[0]
    fam = None
    for i in range(cnt):
        base = 6 + 12 * i
        plat, enc, lang, nid, ln, off = struct.unpack(">HHHHHH", name[base:base + 12])
        if nid == 1 and plat == 3:
            storage = 6 + 12 * cnt
            fam = name[storage + off:storage + off + ln].decode("utf-16-be")
    assert fam == "Kilat Mini", fam
    os2 = tables["OS/2"]
    assert struct.unpack(">H", os2[0:2])[0] == 2
    assert struct.unpack(">H", os2[4:6])[0] == 400
    assert struct.unpack(">H", os2[62:64])[0] & 0x40
    assert struct.unpack(">h", os2[86:88])[0] == 500
    assert struct.unpack(">h", os2[88:90])[0] == 700
    kern = tables["kern"]
    # header: version(0) nTables(2); subtable: version(4) length(6) coverage(8)
    # nPairs(10) searchRange entrySelector rangeShift; pairs from 18.
    assert struct.unpack(">H", kern[0:2])[0] == 0
    assert struct.unpack(">H", kern[2:4])[0] == 1, "one subtable"
    assert struct.unpack(">H", kern[4:6])[0] == 0, "subtable version 0"
    assert struct.unpack(">H", kern[8:10])[0] & 1, "horizontal coverage bit"
    n_pairs = struct.unpack(">H", kern[10:12])[0]
    assert n_pairs == 1
    assert struct.unpack(">H", kern[6:8])[0] == 14 + 6 * n_pairs, "subtable length"
    l, r, v = struct.unpack(">HHh", kern[18:24])
    assert (l, r, v) == (2, 3, -80), (l, r, v)
    print("check ok:", path, "(%d bytes)" % len(data))


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "tests/data"
    os.makedirs(out, exist_ok=True)
    sfnt = build_sfnt()
    ttf = os.path.join(out, "kilat-mini.ttf")
    open(ttf, "wb").write(sfnt)
    woff = os.path.join(out, "kilat-mini.woff")
    open(woff, "wb").write(build_woff(sfnt))
    check(ttf)
    data = open(woff, "rb").read()
    assert data[:4] == b"wOFF", data[:4]
    n = struct.unpack(">H", data[12:14])[0]
    assert n == 11, n
    print("check ok: %s (%d bytes, %d tables)" % (woff, len(data), n))
    print("wrote %s (%d bytes)" % (ttf, len(sfnt)))


if __name__ == "__main__":
    if sys.argv[1:2] == ["--check"]:
        check("tests/data/kilat-mini.ttf")
    else:
        main()
