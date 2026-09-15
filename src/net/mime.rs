//! MIME types: extension mapping, parameter extraction, and the small amount of
//! content sniffing the spec asks for when a server says nothing useful.

use crate::net::url::Url;

/// MIME type with its parameters (`charset=utf-8`, `boundary=...`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mime {
    pub ty: String,
    pub subtype: String,
    pub charset: Option<String>,
    pub boundary: Option<String>,
}

impl Mime {
    pub fn parse(value: &str) -> Mime {
        let mut parts = value.split(';');
        let full = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let (ty, subtype) = match full.split_once('/') {
            Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
            None => (full.clone(), String::new()),
        };
        let mut charset = None;
        let mut boundary = None;
        for p in parts {
            let p = p.trim();
            if let Some((k, v)) = p.split_once('=') {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                match k.trim().to_ascii_lowercase().as_str() {
                    "charset" => charset = Some(v.to_ascii_lowercase()),
                    "boundary" => boundary = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        Mime {
            ty,
            subtype,
            charset,
            boundary,
        }
    }

    pub fn is_html(&self) -> bool {
        self.subtype == "html" || self.subtype.ends_with("+html")
    }
    pub fn is_xml(&self) -> bool {
        self.subtype.ends_with("xml") || self.subtype.ends_with("+xml")
    }
    pub fn is_css(&self) -> bool {
        self.subtype == "css"
    }
    pub fn is_javascript(&self) -> bool {
        self.subtype.contains("javascript")
            || self.subtype.contains("ecmascript")
            || self.subtype == "node"
    }
    pub fn is_json(&self) -> bool {
        self.subtype == "json" || self.subtype.ends_with("+json")
    }
    pub fn is_text(&self) -> bool {
        self.ty == "text"
            || self.is_json()
            || self.is_xml()
            || self.is_javascript()
            || self.is_css()
            || self.subtype.ends_with("+json")
            || self.subtype.ends_with("+xml")
    }
    pub fn is_image(&self) -> bool {
        self.ty == "image"
    }
    pub fn is_video(&self) -> bool {
        self.ty == "video" || self.subtype.ends_with("mp4")
    }
    pub fn is_audio(&self) -> bool {
        self.ty == "audio"
    }
    pub fn is_font(&self) -> bool {
        self.subtype.contains("font")
            || self.subtype.contains("woff")
            || self.subtype.contains("opentype")
            || self.subtype.contains("truetype")
    }
    pub fn is_pdf(&self) -> bool {
        self.subtype == "pdf"
    }
    /// A body we can hand to the decoder instead of downloading blindly.
    pub fn is_decodable_image(&self) -> bool {
        matches!(
            self.subtype.as_str(),
            "png" | "gif" | "jpeg" | "jpg" | "apng" | "svg+xml"
        )
    }
    pub fn main(&self) -> String {
        format!("{}/{}", self.ty, self.subtype)
    }
}

impl std::fmt::Display for Mime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.ty, self.subtype)
    }
}

/// Extension -> MIME. Covers what a browser downloads on a typical page.
pub fn from_extension(ext: &str) -> Option<&'static str> {
    let e = ext.to_ascii_lowercase();
    Some(match e.as_str() {
        "html" | "htm" | "xhtml" | "shtml" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "json" | "map" => "application/json",
        "txt" | "text" | "log" | "md" => "text/plain",
        "xml" | "rss" | "atom" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" | "cur" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "heic" | "heif" => "image/heic",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "wasm" => "application/wasm",
        "vtt" => "text/vtt",
        "srt" => "application/x-subrip",
        "ics" => "text/calendar",
        "csv" => "text/csv",
        _ => return None,
    })
}

/// MIME -> extension, for `file:` saves and the image cache's temp names.
pub fn to_extension(mime: &str) -> &'static str {
    let m = Mime::parse(mime);
    match m.subtype.as_str() {
        "html" => "html",
        "css" => "css",
        "javascript" => "js",
        "json" => "json",
        "png" => "png",
        "jpeg" | "jpg" => "jpg",
        "gif" => "gif",
        "webp" => "webp",
        "avif" => "avif",
        "svg+xml" => "svg",
        "x-icon" | "vnd.microsoft.icon" => "ico",
        "mp4" => "mp4",
        "webm" => "webm",
        "mpeg" => "mp3",
        "woff2" => "woff2",
        "woff" => "woff",
        "ttf" => "ttf",
        "otf" => "otf",
        "pdf" => "pdf",
        "plain" => "txt",
        _ => "bin",
    }
}

/// Guess a type from the first bytes, used when the server says
/// `application/octet-stream` (common on object stores).
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let head = &bytes[..bytes.len().min(512)];
    let ascii = |s: &[u8]| head.len() >= s.len() && &head[..s.len()] == s;
    let starts = |s: &str| {
        let t = s.trim_start();
        head.len() >= t.len() && head[..t.len()].eq_ignore_ascii_case(t.as_bytes())
    };
    if ascii(b"\x89PNG") {
        Some("image/png")
    } else if ascii(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if ascii(b"GIF87a") || ascii(b"GIF89a") {
        Some("image/gif")
    } else if head.len() > 12 && ascii(b"RIFF") && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else if head.len() > 12 && ascii(b"ftyp") {
        Some("video/mp4")
    } else if ascii(b"\x1a\x45\xdf\xa3") {
        Some("video/webm")
    } else if ascii(b"OggS") {
        Some("audio/ogg")
    } else if ascii(b"wOF2") {
        Some("font/woff2")
    } else if ascii(b"wOFF") {
        Some("font/woff")
    } else if ascii(b"OTTO") {
        Some("font/otf")
    } else if ascii(b"\x00\x01\x00\x00") || ascii(b"true") || ascii(b"ttcf") {
        Some("font/ttf")
    } else if ascii(b"%PDF") {
        Some("application/pdf")
    } else if ascii(b"ID3") || ascii(b"\xff\xfb") || ascii(b"\xff\xf3") {
        Some("audio/mpeg")
    } else if starts("<!doctype html") || starts("<html") || starts("<head") || starts("<body")
        || starts("<!DOCTYPE HTML")
        || starts("<script")
        || starts("<iframe")
        || starts("<div")
        || starts("<p>")
    {
        Some("text/html")
    } else if starts("<?xml") {
        Some("application/xml")
    } else if starts("{") || starts("[") {
        Some("application/json")
    } else if head.iter().take(256).all(|b| *b == b'\t' || *b == b'\n' || *b == b'\r' || *b >= 0x20)
    {
        Some("text/plain")
    } else {
        None
    }
}

/// The type the *document loader* should use, applying the same fallback order
/// as browsers: Content-Type, then the URL extension, then content sniffing.
pub fn resolve(content_type: Option<&str>, url: &Url, body: &[u8]) -> Mime {
    if let Some(ct) = content_type {
        let m = Mime::parse(ct);
        let vague = m.ty.is_empty()
            || m.subtype == "octet-stream"
            || m.subtype == "binary"
            || m.subtype == "unknown"
            || m.subtype == "x-download";
        if !vague {
            return m;
        }
        if let Some(g) = sniff(body) {
            let mut m2 = Mime::parse(g);
            m2.charset = m.charset;
            return m2;
        }
        return m;
    }
    if let Some(g) = sniff(body) {
        return Mime::parse(g);
    }
    if let Some(e) = from_extension(&url.extension()) {
        return Mime::parse(e);
    }
    Mime::parse("text/plain")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::url::Url;

    #[test]
    fn parameters_and_predicates() {
        let m = Mime::parse("text/html; charset=UTF-8");
        assert!(m.is_html());
        assert!(m.is_text());
        assert_eq!(m.charset.as_deref(), Some("utf-8"));
        let j = Mime::parse("application/x-ndjson");
        assert!(!j.is_json());
        let css = Mime::parse("text/css");
        assert!(css.is_css());
        assert!(Mime::parse("image/svg+xml").is_image());
        assert!(!Mime::parse("image/webp").is_decodable_image());
        assert!(Mime::parse("font/woff2").is_font());
        let f = Mime::parse(r#"multipart/form-data; boundary="abc""#);
        assert_eq!(f.boundary.as_deref(), Some("abc"));
    }

    #[test]
    fn extension_roundtrip() {
        assert_eq!(from_extension("svg"), Some("image/svg+xml"));
        assert_eq!(from_extension("Woff2"), Some("font/woff2"));
        assert_eq!(from_extension("nope"), None);
        assert_eq!(to_extension("image/jpeg"), "jpg");
        assert_eq!(to_extension("text/html; charset=utf-8"), "html");
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff(b"\xff\xd8\xff\xe0abc"), Some("image/jpeg"));
        assert_eq!(sniff(b"<html><body>"), Some("text/html"));
        assert_eq!(sniff(b"  <!DOCTYPE html>"), Some("text/html"));
        assert_eq!(sniff(b"{\"a\":1}"), Some("application/json"));
        assert_eq!(sniff(b"hello world"), Some("text/plain"));
        assert_eq!(sniff(&[0u8, 1, 2, 3]), None);
    }

    #[test]
    fn resolution_order() {
        let u = Url::parse("http://x/y.png").unwrap();
        // Trust a specific Content-Type.
        assert_eq!(resolve(Some("image/gif"), &u, b"whatever").subtype, "gif");
        // Fall back to sniffing, then the extension.
        assert_eq!(
            resolve(Some("application/octet-stream"), &u, b"\x89PNG\r\n\x1a\nx")
                .subtype,
            "png"
        );
        let u2 = Url::parse("http://x/y.css").unwrap();
        assert_eq!(resolve(None, &u2, b"[b]{color:red}").subtype, "css");
        assert_eq!(resolve(None, &u, b"\x00\x01").subtype, "png");
    }
}
