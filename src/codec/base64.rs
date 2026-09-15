//! Base64 (RFC 4648) with padding-tolerant decode: needed for the WebSocket
//! handshake, `Authorization` headers and `data:` URLs.

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn decode(text: &str) -> crate::util::Result<Vec<u8>> {
    let mut rev = [255u8; 256];
    for (i, c) in ALPHABET.iter().enumerate() {
        rev[*c as usize] = i as u8;
    }
    rev['-' as usize] = 62; // url-safe alphabet
    rev['_' as usize] = 63;
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out: Vec<u8> = Vec::with_capacity(text.len() / 4 * 3 + 3);
    for &b in text.as_bytes() {
        if b == b'=' || b == b'\n' || b == b'\r' || b == b' ' || b == b'\t' {
            continue;
        }
        let v = rev[b as usize];
        if v == 255 {
            return Err(format!("base64: invalid byte {b}"));
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    if bits > 4 {
        return Err("base64: truncated input".to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        for s in ["", "x", "xy", "xyz", "wxyz", "\u{e9}\u{443}\u{3053}"] {
            let enc = encode(s.as_bytes());
            assert_eq!(decode(&enc).unwrap(), s.as_bytes(), "roundtrip {s:?}");
        }
        // Unpadded (common in data: URLs) and url-safe variants decode fine.
        assert_eq!(decode("Zm9vYmE").unwrap(), b"fooba");
        assert_eq!(decode("Zm9vYmE=").unwrap(), b"fooba");
        assert!(decode("!!!!").is_err());
    }
}
