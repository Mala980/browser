//! Small self-contained helpers: JSON, geometry, logging, monotonic time.
//!
//! Everything here is written against `std` only - Kilat deliberately has no
//! third-party Rust crates so that `cargo build --offline` works on a phone.

pub mod geom;
pub mod json;
pub mod log;
pub mod time;

pub use geom::{Color, Rect, Vec2};
pub use json::Json;

/// Error plumbing across the engine: an error almost always just means "skip
/// this resource and keep rendering", so a displayable string is enough.
pub type Error = String;
pub type Result<T> = core::result::Result<T, String>;

/// Cheap stable hash for cache keys (FNV-1a 64 bit). Not for security.
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Split a comma separated option list, trimming and lowercasing entries.
pub fn split_list(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let p = part.trim().to_ascii_lowercase();
        if !p.is_empty() {
            out.push(p);
        }
    }
    out
}

/// Parse `k=v;k=v` or `k=v,k=v` option strings (used by `--opt=...`).
pub fn parse_kv(spec: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in spec.split(|c| c == ';' || c == ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            Some((k, v)) => out.push((k.trim().to_ascii_lowercase(), v.trim().to_string())),
            None => out.push((part.to_ascii_lowercase(), String::new())),
        }
    }
    out
}

pub fn clamp_f32(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

pub fn clamp_i32(v: i32, lo: i32, hi: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

pub fn clamp_usize(v: usize, lo: usize, hi: usize) -> usize {
    v.max(lo).min(hi)
}

/// Round to device pixels. Layout works in CSS pixels, painting in physical
/// pixels; Chromium's crispness comes largely from snapping borders and baselines
/// onto whole device pixels, so we do the same.
pub fn snap(v: f32, scale: f32) -> f32 {
    (v * scale).round() / scale
}

/// Parse `500kb`, `12MB`, `2mib`, `900` into a byte count.
pub fn parse_size_human(s: &str) -> Option<u64> {
    let t = s.trim().to_ascii_lowercase();
    let suffixes: [(&str, u64); 6] = [
        ("kib", 1_024),
        ("mib", 1_048_576),
        ("gib", 1_073_741_824),
        ("kb", 1_000),
        ("mb", 1_000_000),
        ("gb", 1_000_000_000),
    ];
    let (num, mul) = match suffixes.iter().find(|(sfx, _)| t.ends_with(sfx)) {
        Some((sfx, mul)) => (t[..t.len() - sfx.len()].to_string(), *mul),
        None => (t.trim_end_matches('b').to_string(), 1u64),
    };
    let v: f64 = num.trim().parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    Some((v * mul as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size_human("500kb"), Some(500_000));
        assert_eq!(parse_size_human("2 MiB"), Some(2_097_152));
        assert_eq!(parse_size_human("900"), Some(900));
        assert_eq!(parse_size_human("1.5mb"), Some(1_500_000));
        assert_eq!(parse_size_human("-4mb"), None);
        assert_eq!(parse_size_human("abc"), None);
    }

    #[test]
    fn kv_and_lists() {
        assert_eq!(
            parse_kv("quality=balanced; budget=8mb"),
            vec![
                ("quality".to_string(), "balanced".to_string()),
                ("budget".to_string(), "8mb".to_string())
            ]
        );
        assert_eq!(split_list("a, B ,,c"), vec!["a", "b", "c"]);
        assert_eq!(fnv64(b"abc"), fnv64(b"abc"));
        assert_ne!(fnv64(b"abc"), fnv64(b"abd"));
    }
}
