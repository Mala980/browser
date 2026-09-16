//! GIF decoder (GIF87a/GIF89a) with animation and disposal handling.
//!
//! Animated GIFs are the one image format where every frame has to be composited
//! against a canvas, so the decoder keeps a running RGBA buffer and applies the
//! Graphics Control Extension disposal methods like a browser does.

use crate::codec::{Decoded, Frame};
use crate::util::Result;

const MAX_FRAMES: usize = 64;

pub fn is_gif(data: &[u8]) -> bool {
    data.len() > 5 && &data[..3] == b"GIF"
}

pub fn decode(data: &[u8]) -> Result<Decoded> {
    if data.len() < 13 || &data[..3] != b"GIF" {
        return Err("gif: bad header".to_string());
    }
    let version = &data[3..6];
    if version != b"87a" && version != b"89a" {
        return Err("gif: unknown version".to_string());
    }
    let w = u16::from_le_bytes([data[6], data[7]]) as usize;
    let h = u16::from_le_bytes([data[8], data[9]]) as usize;
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err(format!("gif: implausible size {w}x{h}"));
    }
    let packed = data[10];
    let gct = (packed & 0x80) != 0;
    let gct_size = 2usize << (packed & 7);
    let mut pos = 13usize;
    let mut palette: Vec<u8> = Vec::new();
    if gct {
        let end = (pos + gct_size * 3).min(data.len());
        palette = data[pos..end].to_vec();
        pos = end;
    }
    let background = vec![0u8; w * h * 4];
    let mut canvas: Vec<u8> = vec![0u8; w * h * 4];
    // Fully opaque by default; the palette's alpha comes from the GCE.
    for px in canvas.chunks_mut(4) {
        px[3] = 255;
    }
    let mut frames: Vec<Frame> = Vec::new();
    let mut loop_count: Option<u16> = None;
    let mut delay = 0u32;
    let mut transparent_index: Option<u8> = None;
    let mut disposal = 0u8;

    while pos < data.len() {
        match data[pos] {
            0x21 => {
                // Extension
                if pos + 1 >= data.len() {
                    break;
                }
                let kind = data[pos + 1];
                let mut p = pos + 2;
                if kind == 0xf9 && p + 4 < data.len() {
                    let bs = data[p];
                    disposal = (bs >> 2) & 7;
                    let has_t = (bs & 1) != 0;
                    delay = u16::from_le_bytes([data[p + 1], data[p + 2]]) as u32 * 10;
                    transparent_index = if has_t { Some(data[p + 3]) } else { None };
                }
                // Skip sub-blocks (also picks up the NETSCAPE loop count).
                let mut payload: Vec<u8> = Vec::new();
                p += if kind == 0xf9 { 5 } else { 0 };
                while p < data.len() && data[p] != 0 {
                    let n = data[p] as usize;
                    p += 1;
                    if p + n > data.len() {
                        break;
                    }
                    payload.extend_from_slice(&data[p..p + n]);
                    p += n;
                }
                p += 1;
                if kind == 0xff && payload.len() >= 17 {
                    if &payload[3..11] == b"NETSCAPE" && payload[11] == 3 && payload[12] == 1 {
                        let n = u16::from_le_bytes([payload[13], payload[14]]);
                        loop_count = Some(n);
                    }
                }
                pos = p;
            }
            0x2c => {
                // Image descriptor
                // 0x2C then left, top, width, height (u16 LE) and a flags byte:
                // ten bytes, so the fields start at pos+1, not pos+3.
                if pos + 11 > data.len() {
                    return Err("gif: truncated image descriptor".to_string());
                }
                let lx = u16::from_le_bytes([data[pos + 1], data[pos + 2]]) as usize;
                let ly = u16::from_le_bytes([data[pos + 3], data[pos + 4]]) as usize;
                let lw = u16::from_le_bytes([data[pos + 5], data[pos + 6]]) as usize;
                let lh = u16::from_le_bytes([data[pos + 7], data[pos + 8]]) as usize;
                let flags = data[pos + 9];
                let has_local = (flags & 0x80) != 0;
                let interlaced = (flags & 0x40) != 0;
                let local_bits = 2usize << (flags & 7);
                let mut p = pos + 10;
                let mut pal = palette.clone();
                if has_local {
                    let end = (p + local_bits * 3).min(data.len());
                    pal = data[p..end].to_vec();
                    p = end;
                }
                if p >= data.len() {
                    return Err("gif: truncated image data".to_string());
                }
                let min_code = data[p];
                p += 1;
                let mut blocks: Vec<u8> = Vec::new();
                while p < data.len() && data[p] != 0 {
                    let n = data[p] as usize;
                    p += 1;
                    if p + n > data.len() {
                        break;
                    }
                    blocks.extend_from_slice(&data[p..p + n]);
                    p += n;
                }
                p += 1;
                let index = decode_lzw(&blocks, min_code, lw * lh)?;
                let prev = canvas.clone();
                let mut wrote = false;
                paint_frame(
                    &mut canvas,
                    w,
                    h,
                    lx,
                    ly,
                    lw,
                    lh,
                    &index,
                    &pal,
                    transparent_index,
                    interlaced,
                    &mut wrote,
                );
                if frames.len() < MAX_FRAMES {
                    frames.push(Frame {
                        rgba: canvas.clone(),
                        duration_ms: if delay == 0 { 100 } else { delay },
                    });
                }
                // Disposal for the next frame.
                match disposal {
                    2 => {
                        for (i, px) in canvas.chunks_mut(4).enumerate() {
                            if i * 4 + 3 < background.len() {
                                px.copy_from_slice(&background[i * 4..i * 4 + 4]);
                                px[3] = 0;
                            }
                        }
                    }
                    3 => {
                        canvas = prev;
                    }
                    _ => {}
                }
                let _ = wrote;
                transparent_index = None;
                disposal = 0;
                delay = 0;
                pos = p;
            }
            0x3b => break,
            _ => {
                pos += 1;
            }
        }
    }
    if frames.is_empty() {
        return Err("gif: no image blocks".to_string());
    }
    Ok(Decoded::animated(w as u32, h as u32, frames, loop_count))
}

#[allow(clippy::too_many_arguments)]
fn paint_frame(
    canvas: &mut [u8],
    w: usize,
    h: usize,
    lx: usize,
    ly: usize,
    lw: usize,
    lh: usize,
    index: &[u8],
    palette: &[u8],
    transparent: Option<u8>,
    interlaced: bool,
    wrote: &mut bool,
) {
    for sy in 0..lh {
        let row = ly + if interlaced { deinterlace_row(sy, lh) } else { sy };
        if row >= h {
            continue;
        }
        for sx in 0..lw {
            let dx = lx + sx;
            if dx >= w {
                continue;
            }
            let si = sy * lw + sx;
            let idx = match index.get(si) {
                Some(v) => *v,
                None => continue,
            };
            if Some(idx) == transparent {
                continue;
            }
            let pi = idx as usize * 3;
            if pi + 2 >= palette.len() {
                continue;
            }
            let o = (row * w + dx) * 4;
            if o + 3 < canvas.len() {
                canvas[o] = palette[pi];
                canvas[o + 1] = palette[pi + 1];
                canvas[o + 2] = palette[pi + 2];
                canvas[o + 3] = 255;
                *wrote = true;
            }
        }
    }
}

/// GIF interlacing walks the sub-image in four passes: rows 0,8,16..., then
/// 4,12,..., then 2,6,..., then 1,3,....
fn deinterlace_row(src: usize, lh: usize) -> usize {
    let passes: [(usize, usize); 4] = [
        (0, 8),
        (4, 8),
        (2, 4),
        (1, 2),
    ];
    let mut s = src;
    for (start, stride) in passes.iter() {
        let rows = if lh > *start {
            (lh - *start + stride - 1) / stride
        } else {
            0
        };
        if s < rows {
            return start + s * stride;
        }
        s -= rows;
    }
    src.min(lh.saturating_sub(1))
}

/// GIF's variable-width LZW (4096 entry table, clear + end codes).
fn decode_lzw(data: &[u8], min_code_size: u8, expect: usize) -> Result<Vec<u8>> {
    if min_code_size < 2 || min_code_size > 11 {
        return Err("gif: bad LZW minimum code size".to_string());
    }
    let clear = 1u16 << min_code_size;
    let end = clear + 1;
    let mut width = min_code_size as u32 + 1;
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut next = end + 1;
    let mut out: Vec<u8> = Vec::with_capacity(expect + 16);
    let mut bitbuf: u32 = 0;
    let mut bitcnt = 0u32;
    let mut pos = 0usize;
    let mut old: Option<u16> = None;
    let mut first: u8 = 0;
    let mut stack = vec![0u8; 4096];
    loop {
        while bitcnt < width {
            if pos >= data.len() {
                return Ok(out);
            }
            bitbuf |= (data[pos] as u32) << bitcnt;
            bitcnt += 8;
            pos += 1;
        }
        let code = (bitbuf & ((1u32 << width) - 1)) as u16;
        bitbuf >>= width;
        bitcnt -= width;
        if code == clear {
            width = min_code_size as u32 + 1;
            next = end + 1;
            old = None;
            continue;
        }
        if code == end {
            return Ok(out);
        }
        let mut sp = 0usize;
        let mut c = code;
        if c >= next {
            // Reference to the entry currently being built ("KIKIK" case).
            match old {
                Some(o) => {
                    stack[sp] = first;
                    sp += 1;
                    c = o;
                }
                None => return Err("gif: invalid LZW code".to_string()),
            }
        }
        let mut guard = 0usize;
        while c > end {
            if guard >= 4095 || sp >= stack.len() {
                return Err("gif: LZW chain too long".to_string());
            }
            stack[sp] = suffix[c as usize];
            sp += 1;
            c = prefix[c as usize];
            guard += 1;
        }
        if c < clear {
            stack[sp] = c as u8;
            sp += 1;
        }
        if sp == 0 {
            return Err("gif: empty LZW entry".to_string());
        }
        first = stack[sp - 1];
        while sp > 0 {
            sp -= 1;
            out.push(stack[sp]);
        }
        if old.is_some() {
            if (next as usize) < 4096 {
                prefix[next as usize] = old.unwrap();
                suffix[next as usize] = first;
                next += 1;
                if next == (1u16 << width) && width < 12 {
                    width += 1;
                }
            } else {
                // Table full without a clear code: some encoders do this.
                next = end + 1;
                width = min_code_size as u32 + 1;
            }
        }
        old = Some(code);
        if out.len() > expect * 4 + 4096 {
            return Err("gif: LZW output too large".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-assembled 2x2 GIF87a, palette [black, white, red, blue], all indices 0.
    fn tiny_gif() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"GIF87a");
        d.extend_from_slice(&2u16.to_le_bytes());
        d.extend_from_slice(&2u16.to_le_bytes());
        d.push(0x80 | 0x01); // GCT present, 1 << (1+1) = 4 entries
        d.push(0);
        d.push(0);
        d.extend_from_slice(&[0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0, 255]);
        d.push(0x2c);
        d.extend_from_slice(&[0, 0, 0, 0, 2, 0, 2, 0, 0]);
        d.push(0x02); // LZW min code size
        // Codes: clear(4), 0,1,2,3 then end(5) at width 3 -> packed:
        // 100 000 001 010 011 101 -> bytes: 0x84 0x18 0x20 0x05
        d.push(4);
        d.extend_from_slice(&[0x84, 0x18, 0x20, 0x05]);
        d.push(0);
        d.push(0x3b);
        d
    }

    #[test]
    fn decodes_tiny_gif() {
        let d = decode(&tiny_gif()).unwrap();
        assert_eq!((d.width, d.height), (2, 2));
        assert_eq!(d.frames.len(), 1);
        // Four pixels of palette index 0 => black, opaque.
        assert_eq!(&d.rgba[..4], &[0, 0, 0, 255]);
        assert_eq!(&d.rgba[12..16], &[0, 0, 0, 255]);
    }

    #[test]
    fn decodes_committed_fixture() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let Ok(bytes) = std::fs::read(dir.join("ref.gif")) else {
            return;
        };
        let d = decode(&bytes).unwrap_or_else(|e| panic!("ref.gif: {e}"));
        assert_eq!((d.width, d.height), (2, 2));
        assert_eq!(d.rgba.len(), 16);
    }

    #[test]
    fn rejects_junk() {
        assert!(decode(b"PNG\r\n").is_err());
        assert!(is_gif(b"GIF89a.."));
        assert!(!is_gif(b"GIF"));
    }
}
