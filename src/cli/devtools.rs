//! `kilat dev ...`: cross-check helpers used by CI to compare our hand-written
//! codecs against independent implementations (python zlib, libjpeg,
//! ImageMagick). Keeping them in the binary means CI needs no extra tooling and
//! the exact same decoder under test is the one users run.

use crate::codec::{gif, jpeg, png, Decoded};
use crate::util::Result;

pub fn image_info(path: &str) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let d = decode_any(&bytes)?;
    Ok(format!(
        "{}x{} frames={} bytes={}",
        d.width,
        d.height,
        d.frame_count(),
        d.rgba.len()
    ))
}

/// Format sniffing relies on magic bytes, so we never have to guess.
/// `paint::images` uses the same entry point for `<img>` decoding.
pub fn decode_any(bytes: &[u8]) -> Result<Decoded> {
    if png::is_png(bytes) {
        png::decode(bytes)
    } else if jpeg::is_jpeg(bytes) {
        jpeg::decode(bytes)
    } else if gif::is_gif(bytes) {
        gif::decode(bytes)
    } else {
        Err("unsupported image format".to_string())
    }
}
