//! HTML parsing: tokenizer + tree builder + character references.
//!
//! `kilat` never rejects a document: everything a real browser would error
//! recover from is recovered from here, and the resulting tree is always
//! renderable. See [`parse`] for the list of spec features we do not model.

pub mod entities;
pub mod parse;

pub use entities::decode as decode_entities;
pub use parse::{parse_document, parse_fragment, Parser, Stats};

use crate::dom::Dom;

/// Convenience for tests and `kilat html`: parse and return the DOM.
pub fn parse_html(src: &str) -> Dom {
    let mut dom = Dom::new();
    parse_document(&mut dom, src);
    dom
}

/// Byte-level charset sniffing: the `<meta charset>` / `content-type` declaration
/// if present, else UTF-8 with a latin-1 fallback for stray bytes.
pub fn decode_bytes(bytes: &[u8], declared: Option<&str>) -> String {
    let label = declared
        .map(|s| s.to_ascii_lowercase())
        .or_else(|| sniff(bytes))
        .unwrap_or_else(|| "utf-8".to_string());
    match label.as_str() {
        u if u.starts_with("utf-16le") => decode_utf16(bytes, true),
        u if u.starts_with("utf-16be") => decode_utf16(bytes, false),
        u if u == "latin-1" || u == "iso-8859-1" || u == "windows-1252" => {
            bytes.iter().map(|&b| cp1252(b)).collect()
        }
        _ => {
            let body = bytes.strip_prefix(&[0xef, 0xbb, 0xbf][..]).unwrap_or(bytes);
            match std::str::from_utf8(body) {
                Ok(s) => s.to_string(),
                // Lossy is what browsers do (replacement chars, parse continues).
                Err(_) => String::from_utf8_lossy(body).into_owned(),
            }
        }
    }
}

fn sniff(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let at = |key: &str| {
        text.find(key)
            .map(|k| k + key.len())
            .and_then(|s| text[s..].find(['"', '\'', '>', ';', ' ']).map(|e| text[s..s + e].to_string()))
    };
    if let Some(c) = at("charset=") {
        return Some(c);
    }
    if let Some(c) = at("charset =") {
        return Some(c);
    }
    None
}

fn decode_utf16(bytes: &[u8], le: bool) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        units.push(if le {
            u16::from_le_bytes([bytes[i], bytes[i + 1]])
        } else {
            u16::from_be_bytes([bytes[i], bytes[i + 1]])
        });
        i += 2;
    }
    String::from_utf16_lossy(&units)
}

/// windows-1252 high half, which is what `charset=iso-8859-1` means in practice.
fn cp1252(b: u8) -> char {
    const HI: [char; 32] = [
        '\u{20ac}', '\u{fffd}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{fffd}',
        '\u{17d}', '\u{fffd}', '\u{fffd}', '\u{17e}', '\u{178}', '\u{161}', '\u{203a}', '\u{153}',
        '\u{fffd}', '\u{fffd}', '\u{2dc}', '\u{732}', '\u{2dd}', '\u{fffd}', '\u{fffd}', '\u{fffd}',
        '\u{fffd}', '\u{fffd}',
    ];
    if b < 0x80 {
        b as char
    } else if b < 0xa0 {
        HI[(b - 0x80) as usize]
    } else {
        char::from_u32(b as u32).unwrap_or('\u{fffd}')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charset_sniffing_and_fallback() {
        assert_eq!(decode_bytes(b"<p>\xc3\xa9</p>", None), "<p>é</p>");
        assert_eq!(
            decode_bytes(b"<p>\xe9</p>", Some("windows-1252")),
            "<p>\u{e9}</p>"
        );
        assert_eq!(
            decode_bytes(b"\xef\xbb\xbf<p>ok</p>", None),
            "<p>ok</p>"
        );
        let declared = b"<meta charset=shift_jis>";
        assert_eq!(sniff(declared).as_deref(), Some("shift_jis"));
        // Invalid UTF-8 still yields a string.
        let s = decode_bytes(b"<p>\xff\xfe bad</p>", None);
        assert!(s.contains("\u{fffd}"));
    }

    #[test]
    fn utf16_roundtrip() {
        let mut bytes = Vec::new();
        for u in "héllo".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_bytes(&bytes, Some("utf-16le")), "héllo");
    }
}
