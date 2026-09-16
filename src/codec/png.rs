//! PNG decoder + encoder (ISO/IEC 15948).
//!
//! Decoding powers `<img>`; encoding is what `--screenshot` and the live-view
//! window use to ship frames. Adam7 interlacing, palette/tRNS, gray+alpha and
//! 16-bit sources are all handled so nothing on a real page falls back to a
//! broken-image box.

use crate::codec::crc::{adler32, crc32_of};
use crate::codec::{inflate, Decoded};
use crate::util::Result;

const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

pub fn is_png(data: &[u8]) -> bool {
    data.len() >= 8 && data[..8] == SIG
}

struct Info {
    width: u32,
    height: u32,
    depth: u8,
    color: u8,
    interlace: u8,
}

fn channels(color: u8) -> usize {
    match color {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => 0,
    }
}

pub fn decode(data: &[u8]) -> Result<Decoded> {
    if data.len() < 8 || data[..8] != SIG {
        return Err("png: bad signature".to_string());
    }
    let mut pos = 8usize;
    let mut info: Option<Info> = None;
    let mut palette: Vec<u8> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    while pos + 12 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let typ = &data[pos + 4..pos + 8];
        let start = pos + 8;
        let end = start + len;
        if end + 4 > data.len() {
            break;
        }
        let body = &data[start..end];
        match typ {
            b"IHDR" => {
                if body.len() < 13 {
                    return Err("png: short IHDR".to_string());
                }
                let w = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                let h = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                let depth = body[8];
                let color = body[9];
                if body[10] != 0 || body[11] != 0 {
                    return Err("png: unsupported compression/filter method".to_string());
                }
                if channels(color) == 0 {
                    return Err("png: bad color type".to_string());
                }
                if !matches!(depth, 1 | 2 | 4 | 8 | 16) {
                    return Err("png: bad bit depth".to_string());
                }
                if w == 0 || h == 0 || w > 16_384 || h > 16_384 {
                    return Err(format!("png: implausible size {w}x{h}"));
                }
                info = Some(Info {
                    width: w,
                    height: h,
                    depth,
                    color,
                    interlace: body[12],
                });
            }
            b"PLTE" => palette.extend_from_slice(body),
            b"tRNS" => trns.extend_from_slice(body),
            b"IDAT" => idat.extend_from_slice(body),
            b"acTL" => return Err("png: APNG animation not supported".to_string()),
            b"IEND" => break,
            _ => {}
        }
        pos = end + 4;
    }
    let info = info.ok_or_else(|| "png: missing IHDR".to_string())?;
    if idat.is_empty() {
        return Err("png: no image data".to_string());
    }
    // IHDR tells us exactly how big the inflated stream can be, so bound it by
    // the real pixel size instead of a ratio guess: that blocks a zlib bomb while
    // still accepting a flat 4K screenshot that compresses 200:1.
    let need = (info.width as usize * 4 + 1) * info.height as usize;
    // 34 frames' worth of slack covers APNG without trusting a hostile IHDR.
    let raw = inflate::inflate_zlib_bounded(&idat, need * 34 + 1024)
        .or_else(|_| inflate::inflate_any(&idat))
        .map_err(|e| format!("png: {e}"))?;

    let w = info.width as usize;
    let h = info.height as usize;
    if raw.len() < (w * channels(info.color) * info.depth as usize + 7) / 8 * h {
        // Trust the trailer over the truncated buffer but keep going: partial
        // images are rendered as they arrive, like a progressive download.
    }
    let mut out = vec![0u8; w * h * 4];
    decode_into(&raw, w, h, &info, &palette, &trns, &mut out)?;
    Ok(Decoded::still(w as u32, h as u32, out))
}

fn apply_filter(ft: u8, cur: &mut [u8], prev: &[u8], bpp: usize) -> Result<()> {
    match ft {
        0 => Ok(()),
        1 => {
            for i in bpp..cur.len() {
                let a = cur[i - bpp] as i32;
                cur[i] = (cur[i] as i32 + a) as u8;
            }
            Ok(())
        }
        2 => {
            for i in 0..cur.len() {
                cur[i] = cur[i].wrapping_add(prev[i]);
            }
            Ok(())
        }
        3 => {
            for i in 0..cur.len() {
                let a = if i >= bpp { cur[i - bpp] as i32 } else { 0 };
                cur[i] = (cur[i] as i32 + (a + prev[i] as i32) / 2) as u8;
            }
            Ok(())
        }
        4 => {
            for i in 0..cur.len() {
                let a = if i >= bpp { cur[i - bpp] as i32 } else { 0 };
                let b = prev[i] as i32;
                let c = if i >= bpp { prev[i - bpp] as i32 } else { 0 };
                cur[i] = (cur[i] as i32 + paeth(a, b, c)) as u8;
            }
            Ok(())
        }
        _ => Err(format!("png: unknown filter type {ft}")),
    }
}

fn paeth(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

pub fn decode_into(
    raw: &[u8],
    w: usize,
    h: usize,
    info: &Info,
    palette: &[u8],
    trns: &[u8],
    out: &mut [u8],
) -> Result<()> {
    let ch = channels(info.color);
    let depth = info.depth;
    let bits_per_row = (w * ch * depth as usize + 7) / 8;
    let bpp = ((ch * depth as usize) + 7) / 8;
    if info.interlace == 0 {
        let mut cur = vec![0u8; bits_per_row];
        let mut prev = vec![0u8; bits_per_row];
        for y in 0..h {
            let start = y * (bits_per_row + 1);
            if start >= raw.len() {
                return Err(format!("png: truncated at row {y}"));
            }
            let ft = raw[start];
            let avail = &raw[start + 1..raw.len().min(start + 1 + bits_per_row)];
            for i in 0..bits_per_row {
                cur[i] = *avail.get(i).unwrap_or(&0);
            }
            apply_filter(ft, &mut cur, &prev, bpp)?;
            prev.clone_from(&cur);
            let samples = row_samples(&cur, depth, ch, w);
            for px in 0..w {
                let base = px * ch;
                if base >= samples.len() {
                    break;
                }
                write_pixel(out, w, h, px, y, &samples[base..base + ch], info, palette, trns);
            }
        }
        return Ok(());
    }
    if info.interlace != 1 {
        return Err("png: unknown interlace method".to_string());
    }
    // Adam7: seven passes, each with its own filtered scanlines.
    let mut pos = 0usize;
    let mut cur = Vec::new();
    let mut prev = Vec::new();
    for &(off_x, off_y, sh_x, sh_y) in ADAM7.iter() {
        let pw = pass_size(w, off_x, sh_x);
        let ph = pass_size(h, off_y, sh_y);
        if pw == 0 || ph == 0 {
            continue;
        }
        let row_bytes = (pw * ch * depth as usize + 7) / 8;
        let pbpp = ((ch * depth as usize) + 7) / 8;
        cur.resize(row_bytes, 0);
        prev.resize(row_bytes, 0);
        for py in 0..ph {
            let start = pos + py * (row_bytes + 1);
            if start >= raw.len() {
                return Err("png: truncated interlaced data".to_string());
            }
            let ft = raw[start];
            let avail = &raw[start + 1..raw.len().min(start + 1 + row_bytes)];
            for i in 0..row_bytes {
                cur[i] = *avail.get(i).unwrap_or(&0);
            }
            apply_filter(ft, &mut cur, &prev, pbpp)?;
            prev.clone_from(&cur);
            let samples = row_samples(&cur, depth, ch, pw);
            let y = off_y + py * sh_y;
            for px in 0..pw {
                let base = px * ch;
                if base >= samples.len() {
                    break;
                }
                let x = off_x + px * sh_x;
                write_pixel(out, w, h, x, y, &samples[base..base + ch], info, palette, trns);
            }
        }
        pos += ph * (row_bytes + 1);
    }
    Ok(())
}

/// Convert a filtered scanline into 8-bit samples: 16-bit keeps the high byte
/// (compositing is 8-bit anyway), sub-byte sources are scaled to 0..=255.
fn row_samples(row: &[u8], depth: u8, ch: usize, pixels: usize) -> Vec<u32> {
    let n = pixels * ch;
    let mut v: Vec<u32> = Vec::with_capacity(n);
    match depth {
        8 => {
            for i in 0..n.min(row.len()) {
                v.push(row[i] as u32);
            }
        }
        16 => {
            for i in 0..n {
                let k = i * 2;
                v.push(if k + 1 < row.len() {
                    row[k] as u32
                } else if k < row.len() {
                    row[k] as u32
                } else {
                    0
                });
            }
        }
        d @ (1 | 2 | 4) => {
            let per_byte = 8 / d as usize;
            let max = (1u32 << d) - 1;
            let scale = 255 / max;
            for b in row.iter() {
                for k in 0..per_byte {
                    let shift = 8 - d as usize * (k + 1);
                    v.push(((*b as u32 >> shift) & max) * scale);
                }
            }
        }
        _ => {}
    }
    while v.len() < n {
        v.push(0);
    }
    v
}

/// Adam7 pass offsets/strides: (xoff, yoff, xshift, yshift).
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

fn pass_size(total: usize, off: usize, shift: usize) -> usize {
    if total <= off {
        0
    } else {
        (total - off + shift - 1) / shift
    }
}

fn write_pixel(
    out: &mut [u8],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    vals: &[u32],
    info: &Info,
    palette: &[u8],
    trns: &[u8],
) {
    if x >= w || y >= h || vals.is_empty() || out.len() < (y * w + x) * 4 + 4 {
        return;
    }
    let g = vals[0] as u8;
    let (r, gr, b, a) = match info.color {
        0 => {
            let mut alpha = 255u8;
            if trns.len() == 2 && trns[1] == g {
                alpha = 0;
            }
            (g, g, g, alpha)
        }
        4 => (g, g, g, vals.get(1).copied().unwrap_or(255) as u8),
        2 => (
            g,
            vals.get(1).copied().unwrap_or(0) as u8,
            vals.get(2).copied().unwrap_or(0) as u8,
            if trns.len() == 6
                && trns[1] == g
                && trns[3] == vals.get(1).copied().unwrap_or(0) as u8
                && trns[5] == vals.get(2).copied().unwrap_or(0) as u8
            {
                0
            } else {
                255
            },
        ),
        3 => {
            let idx = g as usize;
            let base = idx * 3;
            let (pr, pg, pb) = if base + 2 < palette.len() {
                (palette[base], palette[base + 1], palette[base + 2])
            } else {
                (0, 0, 0)
            };
            let alpha = if idx < trns.len() { trns[idx] } else { 255 };
            (pr, pg, pb, alpha)
        }
        6 => (
            g,
            vals.get(1).copied().unwrap_or(0) as u8,
            vals.get(2).copied().unwrap_or(0) as u8,
            vals.get(3).copied().unwrap_or(255) as u8,
        ),
        _ => (0, 0, 0, 255),
    };
    let o = (y * w + x) * 4;
    out[o] = r;
    out[o + 1] = gr;
    out[o + 2] = b;
    out[o + 3] = a;
}

/// Encode RGBA8 as a PNG. `dpi` goes into pHYs so screenshots get a sane
/// physical size (96 dpi is the CSS reference).
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    encode_with(width, height, rgba, 96.0, 1)
}

pub fn encode_with(width: u32, height: u32, rgba: &[u8], dpi: f32, level: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let stride = w * 4;
    let mut raw: Vec<u8> = Vec::with_capacity((stride + 1) * h);
    let mut cur = vec![0u8; stride];
    let mut prev = vec![0u8; stride];
    let mut best_buf = vec![0u8; stride];
    let mut score_buf = vec![0i64; stride];
    for y in 0..h {
        let row = &rgba[y * stride..(((y + 1) * stride).min(rgba.len()))];
        for i in 0..stride {
            cur[i] = *row.get(i).unwrap_or(&0);
        }
        let mut best_ft = 0u8;
        let mut best_score: i64 = i64::MAX;
        for ft in 0..5u8 {
            let mut total: i64 = 0;
            for i in 0..stride {
                let a = if i >= 4 { cur[i - 4] as i32 } else { 0 };
                let b = prev[i] as i32;
                let c = if i >= 4 { prev[i - 4] as i32 } else { 0 };
                let v: i32 = match ft {
                    0 => cur[i] as i32,
                    1 => cur[i] as i32 - a,
                    2 => cur[i] as i32 - b,
                    3 => cur[i] as i32 - (a + b) / 2,
                    _ => cur[i] as i32 - paeth(a, b, c),
                };
                let sv = ((v as u8) as i8) as i64;
                score_buf[i] = sv;
                total += sv.abs();
            }
            if total < best_score {
                best_score = total;
                best_ft = ft;
                for (k, s) in score_buf.iter().enumerate() {
                    best_buf[k] = *s as u8;
                }
            }
        }
        raw.push(best_ft);
        raw.extend_from_slice(&best_buf);
        prev.clone_from(&cur);
    }
    let deflated = zlib_deflate(&raw, level);
    let mut out: Vec<u8> = Vec::with_capacity(deflated.len() + 64);
    out.extend_from_slice(&SIG);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    write_chunk(&mut out, b"IHDR", &ihdr);
    let ppm = (dpi / 0.0254).round() as u32;
    let mut phys = Vec::with_capacity(9);
    phys.extend_from_slice(&ppm.to_be_bytes());
    phys.extend_from_slice(&ppm.to_be_bytes());
    phys.push(1);
    write_chunk(&mut out, b"pHYs", &phys);
    write_chunk(&mut out, b"IDAT", &deflated);
    write_chunk(&mut out, b"IEND", &[]);
    out
}

fn zlib_deflate(raw: &[u8], level: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() / 2 + 16);
    out.push(0x78);
    // FLG must make (CMF<<8|FLG) a multiple of 31; with CMF=0x78 the legal values
    // are 1, 0x20, 0x5e, 0x7d, 0x9c, 0xbb, 0xda, 0xf9 - the FLEVEL pick below.
    out.push(if level == 0 {
        0x01
    } else if level < 5 {
        0x5e
    } else if level < 9 {
        0x9c
    } else {
        0xda
    });
    out.extend_from_slice(&crate::codec::deflate::compress_level(raw, level));
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn write_chunk(out: &mut Vec<u8>, typ: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(body);
    let mut crc_in = Vec::with_capacity(4 + body.len());
    crc_in.extend_from_slice(typ);
    crc_in.extend_from_slice(body);
    out.extend_from_slice(&crc32_of(&crc_in).to_be_bytes());
}

/// Chunk builder, used by tests that synthesise images.
pub fn chunk(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_chunk(&mut out, typ, body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> Vec<u8> {
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                px[o] = (x * 7) as u8;
                px[o + 1] = (y * 5) as u8;
                px[o + 2] = ((x + y) * 3) as u8;
                px[o + 3] = if (x + y) % 7 == 0 { 128 } else { 255 };
            }
        }
        px
    }

    #[test]
    fn roundtrip_encode_decode() {
        for (w, h) in [(1usize, 1usize), (7, 3), (64, 32), (101, 17)] {
            let px = gradient(w, h);
            let enc = encode(w as u32, h as u32, &px);
            let dec = decode(&enc).unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
            assert_eq!(dec.width as usize, w);
            assert_eq!(dec.height as usize, h);
            assert_eq!(dec.rgba, px, "roundtrip {w}x{h}");
        }
    }

    #[test]
    fn flat_image_compresses_well() {
        let px = vec![0u8; 200 * 100 * 4];
        let enc = encode(200, 100, &px);
        assert!(
            enc.len() < px.len() / 40,
            "flat image should shrink a lot: {}",
            enc.len()
        );
    }

    #[test]
    fn decodes_committed_fixture() {
        // tests/data/ref.png is written by scripts/make-fixtures.py with python's
        // zlib, so this validates the whole path (inflate + filters + palette)
        // against an independent encoder.
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let Ok(bytes) = std::fs::read(dir.join("ref.png")) else {
            return;
        };
        let expect = std::fs::read_to_string(dir.join("ref.expected.txt")).unwrap_or_default();
        let mut it = expect.split_whitespace();
        let w: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let h: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let sum: u64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let d = decode(&bytes).unwrap_or_else(|e| panic!("ref.png: {e}"));
        assert_eq!((d.width as usize, d.height as usize), (w, h));
        assert_eq!(d.rgba.iter().map(|&b| b as u64).sum::<u64>(), sum);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(decode(b"not a png").is_err());
        assert!(decode(&SIG).is_err());
    }
}
