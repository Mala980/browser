//! DEFLATE / RFC 1951 decoder, plus the zlib (1950) and gzip (1952) wrappers.
//!
//! Servers send the bulk of the modern web gzip-compressed; without this a page
//! costs 3-4x more bytes, so inflate is a first-class bandwidth feature rather
//! than a nicety. The structure follows the classic `puff` canonical-Huffman
//! decoder: small, branchy but easy to audit, and fast enough for a browser that
//! decodes maybe a few hundred KB per navigation.

use crate::codec::crc::{adler32, crc32_of};
use crate::util::Result;

const MAX_BITS: usize = 15;

/// Canonical Huffman decoding table built from a list of code lengths.
pub struct Huff {
    count: [u16; MAX_BITS + 1],
    symbol: Vec<u16>,
}

impl Huff {
    pub fn new(lens: &[u8]) -> Huff {
        let mut count = [0u16; MAX_BITS + 1];
        for &l in lens {
            if l > 0 {
                count[l as usize] += 1;
            }
        }
        let mut offs = [0u16; MAX_BITS + 1];
        for b in 1..MAX_BITS {
            offs[b + 1] = offs[b] + count[b];
        }
        let total = offs[MAX_BITS] + count[MAX_BITS];
        let mut symbol = vec![0u16; total as usize];
        let mut next = offs;
        for (sym, &l) in lens.iter().enumerate() {
            if l > 0 {
                symbol[next[l as usize] as usize] = sym as u16;
                next[l as usize] += 1;
            }
        }
        Huff { count, symbol }
    }

    fn is_empty(&self) -> bool {
        self.symbol.is_empty()
    }
}

struct BitReader<'a> {
    src: &'a [u8],
    pos: usize,
    acc: u32,
    bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(src: &'a [u8]) -> BitReader<'a> {
        BitReader {
            src,
            pos: 0,
            acc: 0,
            bits: 0,
        }
    }

    fn take(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        while self.bits < n {
            if self.pos >= self.src.len() {
                return Err("inflate: unexpected end of input".to_string());
            }
            self.acc |= (self.src[self.pos] as u32) << self.bits;
            self.bits += 8;
            self.pos += 1;
        }
        let mask = if n >= 32 { 0xffff_ffff } else { (1u32 << n) - 1 };
        let v = self.acc & mask;
        self.acc >>= n;
        self.bits -= n;
        Ok(v)
    }

    /// Number of whole bytes of the source consumed by the decoder so far; used
    /// to find the gzip trailer right after the final deflate block.
    fn consumed(&self) -> usize {
        self.pos - (self.bits / 8) as usize
    }

    fn align(&mut self) {
        let drop = self.bits % 8;
        // Discard the remaining partial byte, matching DEFLATE's byte-aligned
        // block boundaries.
        if self.bits >= drop {
            self.acc >>= drop;
            self.bits -= drop;
        }
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(8)? as u8)
    }

    fn u16le(&mut self) -> Result<u32> {
        let lo = self.take(8)?;
        let hi = self.take(8)?;
        Ok(lo | (hi << 8))
    }

    fn decode(&mut self, h: &Huff) -> Result<u16> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=MAX_BITS {
            code |= self.take(1)? as i32;
            let cnt = h.count[len] as i32;
            if code - first < cnt {
                let idx = (index + (code - first)) as usize;
                return h.symbol.get(idx).copied().ok_or_else(|| "inflate: bad code".to_string());
            }
            index += cnt;
            first = (first + cnt) << 1;
            code <<= 1;
        }
        Err("inflate: invalid huffman code".to_string())
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
    131, 163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
    13, 13,
];
/// Order in which code lengths for the code-length alphabet are stored.
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn fixed_tables() -> (Huff, Huff) {
    let mut lit = [0u8; 288];
    for (i, v) in lit.iter_mut().enumerate() {
        *v = if i < 144 {
            8
        } else if i < 256 {
            9
        } else if i < 280 {
            7
        } else {
            8
        };
    }
    let dist = [5u8; 30];
    (Huff::new(&lit), Huff::new(&dist))
}

/// Decode one or more DEFLATE blocks into `out`, stopping at the final block.
fn inflate_blocks(br: &mut BitReader, out: &mut Vec<u8>, max_out: usize) -> Result<()> {
    let mut fixed_lit: Option<Huff> = None;
    let mut fixed_dist: Option<Huff> = None;
    let mut dyn_lit: Vec<u8> = Vec::new();
    let mut dyn_dist: Vec<u8> = Vec::new();
    loop {
        let final_block = br.take(1)? == 1;
        let btype = br.take(2)?;
        match btype {
            0 => {
                br.align();
                let len = br.u16le()? as usize;
                let nlen = br.u16le()?;
                if (len ^ 0xffff) != nlen as usize {
                    return Err("inflate: stored block length mismatch".to_string());
                }
                if out.len() + len > max_out {
                    return Err(format!("inflate: output exceeds {max_out} bytes"));
                }
                for _ in 0..len {
                    let b = br.byte()?;
                    out.push(b);
                }
            }
            1 => {
                if fixed_lit.is_none() {
                    let (a, b) = fixed_tables();
                    fixed_lit = Some(a);
                    fixed_dist = Some(b);
                }
                let l = fixed_lit.as_ref().unwrap();
                let d = fixed_dist.as_ref().unwrap();
                inflate_entropy(br, out, l, d, max_out)?;
            }
            2 => {
                let (hlit, hdist) = read_dynamic_header(br, &mut dyn_lit, &mut dyn_dist)?;
                let lit = Huff::new(&dyn_lit[..hlit]);
                let dist = Huff::new(&dyn_dist[..hdist]);
                if lit.is_empty() {
                    return Err("inflate: empty literal table".to_string());
                }
                inflate_entropy(br, out, &lit, &dist, max_out)?;
            }
            _ => return Err("inflate: reserved block type".to_string()),
        }
        if final_block {
            return Ok(());
        }
    }
}

fn read_dynamic_header(
    br: &mut BitReader,
    lit_lens: &mut Vec<u8>,
    dist_lens: &mut Vec<u8>,
) -> Result<(usize, usize)> {
    let hlit = br.take(5)? as usize + 257;
    let hdist = br.take(5)? as usize + 1;
    let hclen = br.take(4)? as usize + 4;
    let mut clen = [0u8; 19];
    for i in 0..hclen {
        clen[CLEN_ORDER[i]] = br.take(3)? as u8;
    }
    let clh = Huff::new(&clen);
    let total = hlit + hdist;
    lit_lens.clear();
    dist_lens.clear();
    let mut all: Vec<u8> = Vec::with_capacity(total);
    while all.len() < total {
        let sym = br.decode(&clh)?;
        match sym {
            0..=15 => all.push(sym as u8),
            16 => {
                if all.is_empty() {
                    return Err("inflate: repeat with no previous length".to_string());
                }
                let prev = *all.last().unwrap();
                let n = 3 + br.take(2)? as usize;
                for _ in 0..n {
                    all.push(prev);
                }
            }
            17 => {
                let n = 3 + br.take(3)? as usize;
                for _ in 0..n {
                    all.push(0);
                }
            }
            18 => {
                let n = 11 + br.take(7)? as usize;
                for _ in 0..n {
                    all.push(0);
                }
            }
            _ => return Err("inflate: bad code-length symbol".to_string()),
        }
    }
    if all.len() > total {
        return Err("inflate: too many code lengths".to_string());
    }
    lit_lens.extend_from_slice(&all[..hlit]);
    dist_lens.extend_from_slice(&all[hlit..]);
    Ok((hlit, hdist))
}

fn inflate_entropy(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    lit: &Huff,
    dist: &Huff,
    max_out: usize,
) -> Result<()> {
    loop {
        let sym = br.decode(lit)?;
        if sym < 256 {
            if out.len() + 1 > max_out {
                return Err(format!("inflate: output exceeds {max_out} bytes"));
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        let li = (sym - 257) as usize;
        if li >= 29 {
            return Err("inflate: bad length symbol".to_string());
        }
        let len = (LENGTH_BASE[li] as usize) + br.take(LENGTH_EXTRA[li] as u32)? as usize;
        let ds = br.decode(dist)? as usize;
        if ds >= 30 {
            return Err("inflate: bad distance symbol".to_string());
        }
        let d = (DIST_BASE[ds] as usize) + br.take(DIST_EXTRA[ds] as u32)? as usize;
        if d > out.len() {
            return Err(format!("inflate: distance {d} past output"));
        }
        if out.len() + len > max_out {
            return Err(format!("inflate: output exceeds {max_out} bytes"));
        }
        // Overlapping copies are legal and the whole point of LZ77: copy byte
        // by byte so a run of length 258 at distance 1 replicates correctly.
        let start = out.len() - d;
        for k in 0..len {
            let b = out[start + k];
            out.push(b);
        }
    }
}

/// Raw DEFLATE stream.
pub fn inflate_raw(src: &[u8], max_out: usize) -> Result<Vec<u8>> {
    let mut br = BitReader::new(src);
    let mut out: Vec<u8> = Vec::with_capacity((src.len() * 3).min(max_out).max(64));
    inflate_blocks(&mut br, &mut out, max_out)?;
    Ok(out)
}

/// zlib stream (RFC 1950).
pub fn inflate_zlib(src: &[u8]) -> Result<Vec<u8>> {
    if src.len() < 6 {
        return Err("zlib: too short".to_string());
    }
    let cmf = src[0];
    let flg = src[1];
    if cmf & 0x0f != 8 {
        return Err("zlib: unsupported compression method".to_string());
    }
    if ((cmf as u16) << 8 | flg as u16) % 31 != 0 {
        return Err("zlib: header check failed".to_string());
    }
    // FDICT is bit 5 of FLG (RFC 1950 has no flag at 0x04 - that bit belongs to
    // the 5-bit FCHECK); a preset dictionary would need a 4-byte DICTID here.
    if flg & 0x20 != 0 {
        return Err("zlib: preset dictionary unsupported".to_string());
    }
    let body_start = 2;
    let max = limit(src.len());
    let out = inflate_raw(&src[body_start..src.len() - 4], max)?;
    let want = u32::from_be_bytes([
        src[src.len() - 4],
        src[src.len() - 3],
        src[src.len() - 2],
        src[src.len() - 1],
    ]);
    if adler32(&out) != want {
        return Err("zlib: adler32 mismatch".to_string());
    }
    Ok(out)
}

/// gzip stream (RFC 1952), including concatenated members.
pub fn gunzip(src: &[u8]) -> Result<Vec<u8>> {
    let mut pos = 0usize;
    let mut out: Vec<u8> = Vec::new();
    let max = limit(src.len());
    loop {
        if src.len() - pos < 18 {
            return Err("gzip: truncated".to_string());
        }
        if src[pos] != 0x1f || src[pos + 1] != 0x8b || src[pos + 2] != 8 {
            return Err("gzip: bad magic".to_string());
        }
        let flg = src[pos + 3];
        let mut p = pos + 10;
        if flg & 0x04 != 0 {
            if p + 2 > src.len() {
                return Err("gzip: bad extra field".to_string());
            }
            let xlen = u16::from_le_bytes([src[p], src[p + 1]]) as usize;
            p += 2 + xlen;
        }
        if flg & 0x08 != 0 {
            while p < src.len() && src[p] != 0 {
                p += 1;
            }
            p += 1;
        }
        if flg & 0x10 != 0 {
            while p < src.len() && src[p] != 0 {
                p += 1;
            }
            p += 1;
        }
        if flg & 0x02 != 0 {
            p += 2;
        }
        if p >= src.len() {
            return Err("gzip: no deflate data".to_string());
        }
        let mut br = BitReader::new(&src[p..]);
        let before = out.len();
        inflate_blocks(&mut br, &mut out, max)?;
        if out.len() - before > max {
            return Err("gzip: output too large".to_string());
        }
        // The trailer starts at the next byte boundary after the deflate stream.
        let q = p + br.consumed();
        if q + 8 > src.len() {
            return Err("gzip: truncated trailer".to_string());
        }
        let crc = u32::from_le_bytes([src[q], src[q + 1], src[q + 2], src[q + 3]]);
        let isize_v =
            u32::from_le_bytes([src[q + 4], src[q + 5], src[q + 6], src[q + 7]]);
        if crc32_of(&out[before..]) != crc || isize_v as usize != out.len() - before {
            return Err("gzip: trailer mismatch".to_string());
        }
        pos = q + 8;
        if pos >= src.len() {
            return Ok(out);
        }
        // Another member may follow; otherwise trailing junk is ignored.
        if !(src[pos] == 0x1f && pos + 1 < src.len() && src[pos + 1] == 0x8b) {
            return Ok(out);
        }
    }
}

/// Sniff gzip / zlib / raw DEFLATE and decode. `Content-Encoding` handling uses
/// this so a server that ignores `Accept-Encoding` still works.
pub fn inflate_any(src: &[u8]) -> Result<Vec<u8>> {
    if src.len() >= 2 && src[0] == 0x1f && src[1] == 0x8b {
        return gunzip(src);
    }
    if src.len() >= 2 && src[0] == 0x78 && ((src[0] as u16) << 8 | src[1] as u16) % 31 == 0 {
        return inflate_zlib(src);
    }
    inflate_raw(src, limit(src.len()))
}

/// Bomb guard: never more than 64 MiB out of one response, and at most 64x the
/// compressed size.
fn limit(src_len: usize) -> usize {
    let by_ratio = src_len.saturating_mul(64).max(1 << 16);
    by_ratio.min(64 * 1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Option<Vec<u8>> {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/data");
        p.push(name);
        std::fs::read(p).ok()
    }

    #[test]
    fn roundtrip_against_own_deflate() {
        let mut body = String::new();
        for i in 0..400 {
            body.push_str(&format!("<p id=\"p{i}\">the quick brown fox jumps over the lazy dog {i}</p>\n"));
        }
        let raw = crate::codec::deflate::compress(body.as_bytes());
        let back = inflate_raw(&raw, 1 << 24).unwrap();
        assert_eq!(back, body.as_bytes());

        let z = crate::codec::deflate::compress_zlib(body.as_bytes());
        let back2 = inflate_zlib(&z).unwrap();
        assert_eq!(back2, body.as_bytes());
        assert_eq!(inflate_any(&z).unwrap(), body.as_bytes());
    }

    #[test]
    fn empty_and_small() {
        // A single stored block: BFINAL=1 BTYPE=00, pad to byte, LEN=0, NLEN=!LEN.
        let raw = [0x01u8, 0x00, 0x00, 0xff, 0xff];
        assert_eq!(inflate_raw(&raw, 1024).unwrap().len(), 0);
    }

    #[test]
    fn external_fixtures() {
        // tests/data is generated by scripts/make-codec-fixtures.mjs (see CI);
        // missing fixtures are skipped so the test still passes on fresh clones.
        for base in ["tiny", "html10k", "zeros64k", "rand4k", "mixed90k"] {
            let Some(gz) = fixture(&format!("{base}.gz")) else {
                continue;
            };
            let Some(raw) = fixture(&format!("{base}.bin")) else {
                continue;
            };
            assert_eq!(
                gunzip(&gz).unwrap_or_else(|e| panic!("{base}.gz: {e}")),
                raw,
                "gzip fixture {base}"
            );
            let Some(z) = fixture(&format!("{base}.zz")) else {
                continue;
            };
            assert_eq!(
                inflate_zlib(&z).unwrap_or_else(|e| panic!("{base}.zz: {e}")),
                raw,
                "zlib fixture {base}"
            );
            let Some(st) = fixture(&format!("{base}.stored.zz")) else {
                continue;
            };
            assert_eq!(inflate_zlib(&st).unwrap(), raw, "stored-block fixture {base}");
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(gunzip(b"not gzip at all").is_err());
        assert!(inflate_raw(b"", 100).is_err());
        assert!(inflate_zlib(&[0x78, 0x9c]).is_err());
    }
}
