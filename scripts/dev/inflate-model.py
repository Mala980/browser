#!/usr/bin/env python3
"""Python model of kilat's inflate/deflate, for debugging the bit-exact format.

The Rust and Python paths are line-for-line equivalents of the same algorithm, so
a failure here reproduces the CI test failure without a Rust toolchain, and a fix
verified here can be transcribed back into src/codec/.

  python3 scripts/dev/inflate-model.py            # run all checks
"""
import sys
import os
import zlib

sys.setrecursionlimit(10000)

MAX_BITS = 15
LENGTH_BASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83,
               99, 115, 131, 163, 195, 227, 258]
LENGTH_EXTRA = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5,
                5, 0]
DIST_BASE = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025,
             1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577]
DIST_EXTRA = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11,
              12, 12, 13, 13]
CLEN_ORDER = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15]


class Error(Exception):
    pass


class Huff:
    def __init__(self, lens):
        self.count = [0] * (MAX_BITS + 1)
        for l in lens:
            if l > 0:
                self.count[l] += 1
        offs = [0] * (MAX_BITS + 1)
        for b in range(1, MAX_BITS):
            offs[b + 1] = offs[b] + self.count[b]
        total = offs[MAX_BITS] + self.count[MAX_BITS]
        self.symbol = [0] * total
        nxt = list(offs)
        for sym, l in enumerate(lens):
            if l > 0:
                self.symbol[nxt[l]] = sym
                nxt[l] += 1

    def is_empty(self):
        return not self.symbol


class BitReader:
    def __init__(self, src):
        self.src = src
        self.pos = 0
        self.acc = 0
        self.bits = 0

    def take(self, n):
        if n == 0:
            return 0
        while self.bits < n:
            if self.pos >= len(self.src):
                raise Error("inflate: unexpected end of input")
            self.acc |= self.src[self.pos] << self.bits
            self.bits += 8
            self.pos += 1
        mask = (1 << n) - 1
        v = self.acc & mask
        self.acc >>= n
        self.bits -= n
        return v

    def consumed(self):
        return self.pos - self.bits // 8

    def align(self):
        drop = self.bits % 8
        if self.bits >= drop:
            self.acc >>= drop
            self.bits -= drop

    def byte(self):
        return self.take(8)

    def u16le(self):
        lo = self.take(8)
        hi = self.take(8)
        return lo | (hi << 8)

    def decode(self, h):
        code = 0
        first = 0
        index = 0
        for length in range(1, MAX_BITS + 1):
            code |= self.take(1)
            cnt = h.count[length]
            if code - first < cnt:
                idx = index + (code - first)
                if idx >= len(h.symbol):
                    raise Error("inflate: bad code")
                return h.symbol[idx]
            index += cnt
            first = (first + cnt) << 1
            code <<= 1
        raise Error("inflate: invalid huffman code")


def fixed_tables():
    lit = [0] * 288
    for i in range(288):
        if i < 144:
            lit[i] = 8
        elif i < 256:
            lit[i] = 9
        elif i < 280:
            lit[i] = 7
        else:
            lit[i] = 8
    dist = [5] * 30
    return Huff(lit), Huff(dist)


def inflate_entropy(br, out, lit, dist, max_out):
    while True:
        sym = br.decode(lit)
        if sym < 256:
            if len(out) + 1 > max_out:
                raise Error("output exceeds max")
            out.append(sym)
            continue
        if sym == 256:
            return
        li = sym - 257
        if li >= 29:
            raise Error("bad length symbol")
        length = LENGTH_BASE[li] + br.take(LENGTH_EXTRA[li])
        ds = br.decode(dist)
        if ds >= 30:
            raise Error("bad distance symbol")
        d = DIST_BASE[ds] + br.take(DIST_EXTRA[ds])
        if d > len(out):
            raise Error("distance past output")
        if len(out) + length > max_out:
            raise Error("output exceeds max")
        start = len(out) - d
        for k in range(length):
            out.append(out[start + k])


def read_dynamic_header(br):
    hlit = br.take(5) + 257
    hdist = br.take(5) + 1
    hclen = br.take(4) + 4
    clen = [0] * 19
    for i in range(hclen):
        clen[CLEN_ORDER[i]] = br.take(3)
    clh = Huff(clen)
    total = hlit + hdist
    all_lens = []
    while len(all_lens) < total:
        sym = br.decode(clh)
        if sym <= 15:
            all_lens.append(sym)
        elif sym == 16:
            if not all_lens:
                raise Error("repeat with no previous length")
            prev = all_lens[-1]
            n = 3 + br.take(2)
            all_lens.extend([prev] * n)
        elif sym == 17:
            n = 3 + br.take(3)
            all_lens.extend([0] * n)
        elif sym == 18:
            n = 11 + br.take(7)
            all_lens.extend([0] * n)
        else:
            raise Error("bad code-length symbol")
    if len(all_lens) > total:
        raise Error("too many code lengths")
    return Huff(all_lens[:hlit]), Huff(all_lens[hlit:])


def inflate_blocks(br, out, max_out):
    while True:
        final = br.take(1) == 1
        btype = br.take(2)
        if btype == 0:
            br.align()
            length = br.u16le()
            nlen = br.u16le()
            if (length ^ 0xFFFF) != nlen:
                raise Error("stored block length mismatch")
            if len(out) + length > max_out:
                raise Error("output exceeds max")
            for _ in range(length):
                out.append(br.byte())
        elif btype == 1:
            lit, dist = fixed_tables()
            inflate_entropy(br, out, lit, dist, max_out)
        elif btype == 2:
            lit, dist = read_dynamic_header(br)
            if lit.is_empty():
                raise Error("empty literal table")
            inflate_entropy(br, out, lit, dist, max_out)
        else:
            raise Error("reserved block type")
        if final:
            return


def inflate_raw(src, max_out):
    br = BitReader(src)
    out = []
    inflate_blocks(br, out, max_out)
    return bytes(out)


# ---------------------------------------------------------------------------
# The encoder side, as written in src/codec/deflate.rs
# ---------------------------------------------------------------------------
class BitWriter:
    def __init__(self):
        self.out = bytearray()
        self.acc = 0
        self.bits = 0

    def push(self, value, n):
        self.acc |= (value & ((1 << n) - 1 if n < 32 else 0xFFFFFFFF)) << self.bits
        self.bits += n
        while self.bits >= 8:
            self.out.append(self.acc & 0xFF)
            self.acc >>= 8
            self.bits -= 8

    def code(self, code, n):
        k = n
        while k > 0:
            k -= 1
            self.acc |= ((code >> k) & 1) << self.bits
            self.bits += 1
            if self.bits == 8:
                self.out.append(self.acc & 0xFF)
                self.acc = 0
                self.bits = 0

    def finish(self):
        if self.bits > 0:
            self.out.append(self.acc & 0xFF)
        return bytes(self.out)


def write_lit(bw, sym):
    if sym < 144:
        bw.code(0x30 + sym, 8)
    elif sym < 256:
        bw.code(0x190 + (sym - 144), 9)
    elif sym < 280:
        bw.code(sym - 256, 7)
    else:
        bw.code(0xC0 + (sym - 280), 8)


def length_code(length):
    sym = 0
    for i in range(29):
        if LENGTH_BASE[i] <= length:
            sym = i
        else:
            break
    return 257 + sym, length - LENGTH_BASE[sym]


def dist_code(dist):
    sym = 0
    for i in range(30):
        if DIST_BASE[i] <= dist:
            sym = i
        else:
            break
    return sym, dist - DIST_BASE[sym]


HASH_BITS = 15
HASH_SIZE = 1 << HASH_BITS
WIN = 32768


def compress(data, level=1):
    if not data:
        bw = BitWriter()
        bw.push(1, 1)
        bw.push(1, 2)
        write_lit(bw, 256)
        return bw.finish()
    chain = {1: 1, 2: 2, 3: 4, 4: 8, 6: 32, 7: 64, 8: 128}.get(level, 16)
    bw = BitWriter()
    bw.push(1, 1)
    bw.push(1, 2)
    n = len(data)
    head = [0] * HASH_SIZE
    prev = [0] * WIN
    started = [False] * HASH_SIZE

    def hash_of(a, b, c):
        v = (a << 16) | (b << 8) | c
        h = (v * 0x9E3779B1) & 0xFFFFFFFF
        return (h >> (32 - HASH_BITS)) & (HASH_SIZE - 1)

    i = 0
    while i < n:
        best_len = 0
        best_dist = 0
        if i + 3 <= n:
            h = hash_of(data[i], data[i + 1], data[i + 2])
            if started[h]:
                cand = head[h]
                tries = 0
                while tries < chain and cand < i:
                    dist = i - cand
                    if dist > WIN:
                        break
                    mx = min(258, n - i)
                    if mx > best_len and data[cand + best_len] == data[i + best_len]:
                        l = 0
                        while l < mx and data[cand + l] == data[i + l]:
                            l += 1
                        if l > best_len:
                            best_len = l
                            best_dist = dist
                            if l >= mx:
                                break
                    p = prev[cand & (WIN - 1)]
                    if p >= cand:
                        break
                    cand = p
                    tries += 1
            prev[i & (WIN - 1)] = head[h]
            head[h] = i
            started[h] = True
        if best_len >= 3:
            lsym, lextra = length_code(best_len)
            dsym, dextra = dist_code(best_dist)
            write_lit(bw, lsym)
            bw.push(lextra, LENGTH_EXTRA[lsym - 257])
            bw.code(dsym, 5)
            bw.push(dextra, DIST_EXTRA[dsym])
            for k in range(1, best_len):
                j = i + k
                if j + 3 <= n:
                    h2 = hash_of(data[j], data[j + 1], data[j + 2])
                    prev[j & (WIN - 1)] = head[h2]
                    head[h2] = j
                    started[h2] = True
            i += best_len
        else:
            write_lit(bw, data[i])
            i += 1
    write_lit(bw, 256)
    return bw.finish()


def store(data):
    out = bytearray()
    chunks = [data[i:i + 65535] for i in range(0, len(data), 65535)] or [b""]
    for idx, chunk in enumerate(chunks):
        last = idx == len(chunks) - 1
        out.append(0x01 if last else 0x00)
        ln = len(chunk)
        nlen = (~ln) & 0xFFFF
        out += bytes([ln & 0xFF, ln >> 8, nlen & 0xFF, nlen >> 8])
        out += chunk
    return bytes(out)


def limit(n):
    """Mirror of the Rust `limit()` helper: a generous but bounded output cap."""
    return max(64, min(1 << 26, n * 64 + 4096))


def adler32(data):
    a, b = 1, 0
    for byte in data:
        a = (a + byte) % 65521
        b = (b + a) % 65521
    return ((b << 16) | a) & 0xFFFFFFFF


def compress_zlib(data):
    out = bytearray([0x78, 0x9C])
    out += compress(data)
    out += adler32(data).to_bytes(4, "big")
    return bytes(out)


def inflate_zlib(src, limit_fn=None):
    if len(src) < 6:
        raise Error("zlib: too short")
    cmf, flg = src[0], src[1]
    if cmf & 0x0F != 8:
        raise Error("zlib: unsupported method")
    if ((cmf << 8) | flg) % 31 != 0:
        raise Error("zlib: header check failed")
    if flg & 0x20:
        raise Error("zlib: preset dictionary unsupported")
    body = 2
    mx = limit_rust(len(src))
    out = inflate_raw(src[body:len(src) - 4], mx)
    want = int.from_bytes(src[-4:], "big")
    if adler32(out) != want:
        raise Error("zlib: adler32 mismatch")
    return out


def limit_rust(n):
    """src/codec/inflate.rs::limit"""
    return min(max(n * 64, 1 << 16), 64 * 1024 * 1024)


def main():
    fails = 0

    def check(name, cond, extra=""):
        nonlocal fails
        if cond:
            print("  ok   %s" % name)
        else:
            fails += 1
            print("  FAIL %s %s" % (name, extra))

    print("encoder/decoder self-consistency")
    samples = {
        "empty": b"",
        "one byte": b"x",
        "run of 40k": bytes([7]) * 40000,
        "html-ish": ("".join("div.row-%d { display:flex; padding:%dpx }\n" % (i % 7, i % 20)
                             for i in range(300))).encode(),
        "random 4k": os.urandom(4096),
        "zeros 64k": bytes(65536),
    }
    for name, data in samples.items():
        for lvl, enc in ((1, compress), (0, store)):
            c = enc(data)
            try:
                back = inflate_raw(c, len(data) + 64)
            except Error as e:
                check("%s lvl%d" % (name, lvl), False, "-> %s" % e)
                continue
            check("%s lvl%d" % (name, lvl), back == data,
                  "-> %d/%d bytes" % (len(back), len(data)))

    print("real decoder (python zlib) reading our encoder output")
    for name, data in samples.items():
        if not data:
            continue
        try:
            check("zlib-inflates %s" % name, zlib.decompress(bytes([0x78, 0x9c]) + compress(data)
                                                              + len(data).to_bytes(4, "big"))
                  == data)
        except Exception as e:
            check("zlib-inflates %s" % name, False, "-> %s" % e)

    print("our decoder reading python-zlib output (dynamic blocks)")
    for name, data in samples.items():
        c = zlib.compressobj(9, zlib.DEFLATED, -15)
        raw = c.compress(data) + c.flush()
        try:
            check("inflate-dyn %s" % name, inflate_raw(raw, limit_rust(len(raw))) == data,
                  "-> mismatch")
        except Error as e:
            check("inflate-dyn %s" % name, False, "-> %s" % e)
    print("zlib wrapper (as written in Rust)")
    for name, data in samples.items():
        z = compress_zlib(data)
        try:
            ok = inflate_zlib(z) == data
            check("zlib roundtrip %s" % name, ok)
        except Error as e:
            check("zlib roundtrip %s" % name, False, "-> %s" % e)
        # what python zlib says about our stream, and about our length limit
        try:
            check("zlib reads ours %s" % name, zlib.decompress(z) == data)
        except Exception as e:
            check("zlib reads ours %s" % name, False, "-> %s" % e)
    print("FAILURES: %d" % fails)
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
