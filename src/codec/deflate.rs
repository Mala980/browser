//! DEFLATE compressor used for PNG output (screenshots, live-view frames) and
//! zlib streams in tests.
//!
//! Deliberately simple: fixed Huffman codes + greedy LZ77 matches found through
//! 3-byte hash chains. Fixed tables mean no dynamic-Huffman block header, so
//! output is a few percent larger than `-9`, but the encoder is small enough to
//! audit and roughly 10x faster, which matters more for a "smooth frames" path.

use crate::codec::crc::{adler32, crc32_of};
use crate::util::Result;

const HASH_BITS: usize = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;
const WIN_SIZE: usize = 32_768;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    bits: u32,
}

impl BitWriter {
    fn new(cap: usize) -> BitWriter {
        BitWriter {
            out: Vec::with_capacity(cap),
            acc: 0,
            bits: 0,
        }
    }

    /// LSB-first, used for header fields and extra bits.
    fn push(&mut self, value: u32, n: u32) {
        self.acc |= (value & mask(n)) << self.bits;
        self.bits += n;
        while self.bits >= 8 {
            self.out.push((self.acc & 0xff) as u8);
            self.acc >>= 8;
            self.bits -= 8;
        }
    }

    /// MSB-first, used for Huffman codes (DEFLATE stores Huffman codes with the
    /// most significant bit first, unlike every other field in the stream).
    fn code(&mut self, code: u32, n: u32) {
        let mut k = n;
        while k > 0 {
            k -= 1;
            let bit = (code >> k) & 1;
            self.acc |= bit << self.bits;
            self.bits += 1;
            if self.bits == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.bits = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

fn mask(n: u32) -> u32 {
    if n >= 32 {
        0xffff_ffff
    } else {
        (1u32 << n) - 1
    }
}

/// Fixed literal/length code table (RFC 1951 3.2.6).
fn write_lit(bw: &mut BitWriter, sym: u16) {
    if sym < 144 {
        bw.code(0x30 + sym as u32, 8);
    } else if sym < 256 {
        bw.code(0x190 + (sym as u32 - 144), 9);
    } else if sym < 280 {
        bw.code(sym as u32 - 256, 7);
    } else {
        bw.code(0xc0 + (sym as u32 - 280), 8);
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

fn length_code(len: usize) -> (u16, u32) {
    let mut sym = 0usize;
    for i in 0..29 {
        if (LENGTH_BASE[i] as usize) <= len {
            sym = i;
        } else {
            break;
        }
    }
    let extra_val = (len - LENGTH_BASE[sym] as usize) as u32;
    (257 + sym as u16, extra_val)
}

fn dist_code(dist: usize) -> (u16, u32) {
    let mut sym = 0usize;
    for i in 0..30 {
        if (DIST_BASE[i] as usize) <= dist {
            sym = i;
        } else {
            break;
        }
    }
    let extra_val = (dist - DIST_BASE[sym] as usize) as u32;
    (sym as u16, extra_val)
}

/// Raw DEFLATE stream (one fixed-Huffman block, `BFINAL` set).
pub fn compress(data: &[u8]) -> Vec<u8> {
    compress_level(data, 1)
}

/// `level` 0 => stored blocks (no entropy coding), 1..=9 => hash chain depth.
pub fn compress_level(data: &[u8], level: u32) -> Vec<u8> {
    if data.is_empty() {
        // A single empty fixed block: end-of-block code, then byte padding.
        let mut bw = BitWriter::new(1);
        bw.push(1, 1); // BFINAL
        bw.push(1, 2); // BTYPE = fixed
        write_lit(&mut bw, 256);
        return bw.finish();
    }
    if level == 0 {
        return store(data);
    }
    let chain = match level {
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        6 => 32,
        7 => 64,
        8 => 128,
        _ => 16,
    };
    let mut bw = BitWriter::new(data.len() / 2 + 16);
    bw.push(1, 1); // BFINAL
    bw.push(1, 2); // fixed Huffman

    let n = data.len();
    let mut head = vec![0u32; HASH_SIZE];
    let mut prev = vec![0u32; WIN_SIZE];
    let mut started = vec![false; HASH_SIZE];
    let hash_of = |a: u8, b: u8, c: u8| -> usize {
        let v = ((a as usize) << 16) | ((b as usize) << 8) | c as usize;
        // Multiplicative hash, then fold into HASH_BITS.
        let h = v.wrapping_mul(0x9E37_79B1);
        (h >> (32 - HASH_BITS)) & (HASH_SIZE - 1)
    };

    let mut i = 0usize;
    while i < n {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if i + MIN_MATCH <= n {
            let h = hash_of(data[i], data[i + 1], data[i + 2]);
            if started[h] {
                let mut cand = head[h] as usize;
                let mut tries = 0usize;
                while tries < chain && cand < i {
                    let dist = i - cand;
                    if dist > WIN_SIZE {
                        break;
                    }
                    let max = MAX_MATCH.min(n - i);
                    if max > best_len && data[cand + best_len] == data[i + best_len] {
                        let mut l = 0usize;
                        while l < max && data[cand + l] == data[i + l] {
                            l += 1;
                        }
                        if l > best_len {
                            best_len = l;
                            best_dist = dist;
                            if l >= max {
                                break;
                            }
                        }
                    }
                    let p = prev[cand & (WIN_SIZE - 1)] as usize;
                    if p >= cand {
                        break;
                    }
                    cand = p;
                    tries += 1;
                }
            }
            // Register this position in the chain.
            prev[i & (WIN_SIZE - 1)] = head[h];
            head[h] = i as u32;
            started[h] = true;
        }
        if best_len >= MIN_MATCH {
            let (lsym, lextra) = length_code(best_len);
            let (dsym, dextra) = dist_code(best_dist);
            write_lit(&mut bw, lsym);
            let li = lsym as usize - 257;
            bw.push(lextra, LENGTH_EXTRA[li] as u32);
            bw.code(dsym as u32, 5);
            bw.push(dextra, DIST_EXTRA[dsym as usize] as u32);
            // Index the skipped positions so later lookups still find matches.
            for k in 1..best_len {
                let j = i + k;
                if j + MIN_MATCH <= n {
                    let h = hash_of(data[j], data[j + 1], data[j + 2]);
                    prev[j & (WIN_SIZE - 1)] = head[h];
                    head[h] = j as u32;
                    started[h] = true;
                }
            }
            i += best_len;
        } else {
            write_lit(&mut bw, data[i] as u16);
            i += 1;
        }
    }
    write_lit(&mut bw, 256);
    bw.finish()
}

fn store(data: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(data.len() + data.len() / 65_535 * 5 + 8);
    let mut chunks = data.chunks(65_535).peekable();
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        out.push(if last { 0x01 } else { 0x00 });
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// zlib wrapper (used by tests and by the `--dev deflate` cross-check tool).
pub fn compress_zlib(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 12);
    out.push(0x78);
    out.push(0x9c);
    out.extend_from_slice(&compress(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Gzip wrapper: what `kilat dev gzip` writes, and what the HTTP layer produces
/// when a test server wants a compressed body.
pub fn compress_gzip(data: &[u8], level: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 24);
    out.extend_from_slice(&[0x1f, 0x8b, 8, 0]);
    out.extend_from_slice(&[0, 0, 0, 0]); // mtime
    out.push(0); // XFL
    out.push(3); // OS = unix
    out.extend_from_slice(&compress_level(data, level));
    out.extend_from_slice(&crc32_of(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// Decode back to raw bytes; used in tests and by `kilat dev gunzip`.
pub fn roundtrip_check(src: &[u8]) -> Result<bool> {
    let compressed = compress(src);
    let back = crate::codec::inflate::inflate_raw(&compressed, src.len() + 64)?;
    Ok(back == src)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut s = String::new();
        for i in 0..300 {
            s.push_str(&format!(
                "div.row-{} {{ display:flex; align-items:center; padding:{}px }}\n",
                i % 7,
                i % 20
            ));
        }
        s.into_bytes()
    }

    #[test]
    fn compresses_and_roundtrips() {
        let d = sample();
        for lvl in 0..=9 {
            let c = compress_level(&d, lvl);
            let back = crate::codec::inflate::inflate_raw(&c, d.len() + 64)
                .unwrap_or_else(|e| panic!("level {lvl}: {e}"));
            assert_eq!(back, d, "level {lvl}");
            // Level 0 is stored blocks: it cannot shrink anything, it only has to
            // stay within input + block headers.
            if lvl == 0 {
                assert!(
                    c.len() <= d.len() + (d.len() / 65_535 + 1) * 5 + 5,
                    "stored level grew too much: {}",
                    c.len()
                );
            } else {
                assert!(c.len() < d.len() / 2, "level {lvl} ratio {}", c.len());
            }
        }
        assert!(roundtrip_check(&d).unwrap());
    }

    #[test]
    fn edge_cases() {
        assert!(compress(&[]).len() <= 2);
        let one = b"x".to_vec();
        assert_eq!(
            crate::codec::inflate::inflate_raw(&compress(&one), 10).unwrap(),
            one
        );
        let long = vec![7u8; 40_000];
        let c = compress(&long);
        assert!(c.len() < 500, "runs should compress hard: {}", c.len());
        assert_eq!(
            crate::codec::inflate::inflate_raw(&c, long.len() + 8).unwrap(),
            long
        );
        let z = compress_zlib(&long);
        assert_eq!(crate::codec::inflate::inflate_zlib(&z).unwrap(), long);
        let g = compress_gzip(&long, 5);
        assert_eq!(crate::codec::inflate::gunzip(&g).unwrap(), long);
    }

    #[test]
    fn length_and_dist_codes() {
        assert_eq!(length_code(3), (257, 0));
        assert_eq!(length_code(10), (264, 0));
        assert_eq!(length_code(11), (265, 0));
        assert_eq!(length_code(12), (265, 1));
        assert_eq!(length_code(258), (285, 0));
        assert_eq!(dist_code(1), (0, 0));
        assert_eq!(dist_code(4), (3, 0));
        assert_eq!(dist_code(5), (4, 0));
        assert_eq!(dist_code(6), (4, 1));
        // Symbol 29 starts at 24577 with 13 extra bits, so 32768 is base+8191.
        assert_eq!(dist_code(32768), (29, 8_191));
    }
}
