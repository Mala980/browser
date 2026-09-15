//! CRC-32 (IEEE, used by gzip/PNG/zip) and Adler-32 (zlib).

static CRC_TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();

fn table() -> &'static [u32; 256] {
    CRC_TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        let mut i = 0usize;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    })
}

pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

/// `crc` must be the *pre-inverted* running value, i.e. call as
/// `!crc32_update(!0, part1)` then `!crc32_update(prev, part2)`.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut c = crc;
    let t = table();
    for b in data {
        c = t[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    c
}

pub fn crc32_finish(crc: u32) -> u32 {
    !crc
}

/// Standard one-shot helper.
pub fn crc32_of(data: &[u8]) -> u32 {
    crc32_finish(crc32_update(!0, data))
}

pub fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in data.chunks(5552) {
        for byte in chunk {
            a = (a + *byte as u32) % 65_521;
            b = (b + a) % 65_521;
        }
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // The canonical "check" value for CRC-32/ISO-HDLC over "123456789".
        assert_eq!(crc32_of(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32_of(b""), 0);
        assert_eq!(crc32_of(b"a"), 0xE8B7_BE43);
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b"abc"), 0x024D_0127);
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let mut c = !0u32;
        for p in data.chunks(7) {
            c = crc32_update(c, p);
        }
        assert_eq!(crc32_finish(c), crc32_of(data));
    }
}
