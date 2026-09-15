//! Baseline JPEG decoder (ITU-T T.81, SOF0/SOF1, single- or multi-scan,
//! non-interleaved included).
//!
//! `<img>` on the real web is mostly JPEG, and a Termux build has no libjpeg to
//! lean on, so huffman decoding, dequantisation, IDCT, YCbCr->RGB and EXIF
//! orientation all live here. Progressive (SOF2) and arithmetic (SOF9/10) files
//! are detected and reported; the image loader routes them to the optional
//! external decoder (`ffmpeg`), the same fallback used for WebP/AVIF/JPEG-XL.

use crate::codec::Decoded;
use crate::util::{clamp_i32, Result};

pub fn is_jpeg(data: &[u8]) -> bool {
    data.len() > 4 && data[0] == 0xff && data[1] == 0xd8
}

/// Standard zigzag -> natural order table (matches libjpeg's `natural_order`).
const NATURAL: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

#[derive(Clone)]
struct Huff {
    count: [u16; 17],
    first_code: [u32; 17],
    first_index: [u32; 17],
    val: Vec<u8>,
}

impl Huff {
    fn empty() -> Huff {
        Huff {
            count: [0; 17],
            first_code: [0; 17],
            first_index: [0; 17],
            val: Vec::new(),
        }
    }

    fn build(bits: &[u8], symbols: &[u8]) -> Huff {
        let mut h = Huff::empty();
        let mut code = 0u32;
        let mut idx = 0u32;
        for l in 1..=16usize {
            let c = *bits.get(l - 1).unwrap_or(&0) as u16;
            h.count[l] = c;
            h.first_code[l] = code;
            h.first_index[l] = idx;
            idx += c as u32;
            code = (code + c as u32) << 1;
        }
        h.val = symbols.to_vec();
        h
    }

    fn usable(&self) -> bool {
        !self.val.is_empty()
    }
}

struct BitReader<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u32,
    cnt: u32,
    /// Marker byte that interrupted the entropy stream, if any (0 = none).
    marker: u8,
}

impl<'a> BitReader<'a> {
    fn new(d: &'a [u8], pos: usize) -> BitReader<'a> {
        BitReader {
            d,
            pos,
            acc: 0,
            cnt: 0,
            marker: 0,
        }
    }

    fn bits(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        if n > 25 {
            return Err("jpeg: too many bits at once".to_string());
        }
        while self.cnt < n {
            let b = match self.d.get(self.pos) {
                Some(v) => *v,
                None => return Err("jpeg: out of data".to_string()),
            };
            self.pos += 1;
            if b == 0xff {
                match self.d.get(self.pos) {
                    Some(0) => {
                        self.pos += 1;
                    }
                    Some(m) => {
                        self.marker = *m;
                        return Err("jpeg: marker inside entropy data".to_string());
                    }
                    None => return Err("jpeg: truncated marker".to_string()),
                }
            }
            self.acc = (self.acc << 8) | b as u32;
            self.cnt += 8;
        }
        self.cnt -= n;
        Ok((self.acc >> self.cnt) & ((1u32 << n) - 1))
    }

    fn symbol(&mut self, h: &Huff) -> Result<u8> {
        let mut code = self.bits(1)?;
        for len in 1..=16usize {
            if h.count[len] > 0 && code >= h.first_code[len] && code < h.first_code[len] + h.count[len] as u32 {
                let i = (h.first_index[len] + (code - h.first_code[len])) as usize;
                return h
                    .val
                    .get(i)
                    .copied()
                    .ok_or_else(|| "jpeg: huffman index out of range".to_string());
            }
            let b = self.bits(1)?;
            code = (code << 1) | b;
        }
        Err("jpeg: invalid huffman code".to_string())
    }

    /// "receive and extend": the JPEG magnitude decoding rule.
    fn receive(&mut self, size: u32) -> Result<i32> {
        if size == 0 {
            return Ok(0);
        }
        let v = self.bits(size)? as i32;
        if v < (1 << (size - 1)) {
            Ok(v - (1 << size) + 1)
        } else {
            Ok(v)
        }
    }
}

struct Comp {
    id: u8,
    h: u8,
    v: u8,
    tq: usize,
    /// Blocks per row/column in the scan grid (shared across components).
    bw: usize,
    bh: usize,
    dc_pred: i32,
    dct: usize,
    act: usize,
    /// Raw (not yet dequantised) coefficients, 64 per block, natural order.
    coefs: Vec<i32>,
}

impl Comp {
    fn new() -> Comp {
        Comp {
            id: 0,
            h: 1,
            v: 1,
            tq: 0,
            bw: 0,
            bh: 0,
            dc_pred: 0,
            dct: 0,
            act: 0,
            coefs: Vec::new(),
        }
    }
}

pub struct Header {
    pub width: u32,
    pub height: u32,
    pub comps: usize,
    pub progressive: bool,
    pub arithmetic: bool,
    pub orientation: u32,
}

struct Jpg<'a> {
    d: &'a [u8],
    pos: usize,
    width: usize,
    height: usize,
    precision: u8,
    qt: [[i32; 64]; 4],
    dc: [Huff; 4],
    ac: [Huff; 4],
    comps: Vec<Comp>,
    max_h: usize,
    max_v: usize,
    mcu_w: usize,
    mcu_h: usize,
    restart: usize,
    progressive: bool,
    orientation: u32,
    /// Component/table indices of the scan currently being decoded.
    scan_comp: [usize; 4],
    scan_dct: [usize; 4],
    scan_act: [usize; 4],
}

impl<'a> Jpg<'a> {
    fn new(d: &'a [u8]) -> Jpg<'a> {
        let q = [16i32; 64];
        Jpg {
            d,
            pos: 2,
            width: 0,
            height: 0,
            precision: 8,
            qt: [q, q, q, q],
            dc: [Huff::empty(), Huff::empty(), Huff::empty(), Huff::empty()],
            ac: [Huff::empty(), Huff::empty(), Huff::empty(), Huff::empty()],
            comps: Vec::new(),
            max_h: 1,
            max_v: 1,
            mcu_w: 0,
            mcu_h: 0,
            restart: 0,
            progressive: false,
            orientation: 1,
            scan_comp: [0; 4],
            scan_dct: [0; 4],
            scan_act: [0; 4],
        }
    }

    fn u16(&self, at: usize) -> u16 {
        u16::from_be_bytes([self.d[at], self.d[at + 1]])
    }

    /// Parse everything before the first scan.
    fn headers(&mut self) -> Result<Header> {
        if !is_jpeg(self.d) {
            return Err("jpeg: missing SOI marker".to_string());
        }
        let mut saw_sof = false;
        let mut sof_marker = 0u8;
        while self.pos + 4 <= self.d.len() {
            if self.d[self.pos] != 0xff {
                self.pos += 1;
                continue;
            }
            while self.pos < self.d.len() && self.d[self.pos] == 0xff {
                self.pos += 1;
            }
            if self.pos >= self.d.len() {
                break;
            }
            let m = self.d[self.pos];
            self.pos += 1;
            if m == 0x00 || m == 0x01 || (0xd0..=0xd7).contains(&m) {
                continue;
            }
            if m == 0xd9 {
                break;
            }
            if m == 0xda {
                // Start of scan: headers are complete. Step back over the marker
                // so `read_sos` (which owns scan headers) can parse it.
                self.pos = self.pos.saturating_sub(2);
                break;
            }
            let len = self.u16(self.pos) as usize;
            if len < 2 || self.pos + len > self.d.len() {
                return Err("jpeg: truncated segment".to_string());
            }
            let body = self.pos + 2;
            match m {
                0xc4 => self.read_dht(body, self.pos + len)?,
                0xdb => self.read_dqt(body, self.pos + len)?,
                0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce
                | 0xcf => {
                    sof_marker = m;
                    self.read_sof(body)?;
                    saw_sof = true;
                }
                0xdd => {
                    self.restart = self.u16(body) as usize;
                }
                0xe1 => {
                    let end = (self.pos + len).min(self.d.len());
                    if body < end {
                        self.orientation = exif_orientation(&self.d[body..end]);
                    }
                }
                _ => {}
            }
            self.pos += len;
        }
        if !saw_sof {
            return Err("jpeg: no start-of-frame segment".to_string());
        }
        self.progressive = matches!(sof_marker, 0xc2 | 0xc6 | 0xca | 0xce);
        if self.precision > 8 {
            return Err("jpeg: 12-bit precision not supported".to_string());
        }
        if self.width == 0 || self.height == 0 {
            return Err("jpeg: zero size".to_string());
        }
        if self.width > 16_384 || self.height > 16_384 {
            return Err(format!("jpeg: implausible size {}x{}", self.width, self.height));
        }
        Ok(Header {
            width: self.width as u32,
            height: self.height as u32,
            comps: self.comps.len(),
            progressive: self.progressive,
            arithmetic: matches!(sof_marker, 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf),
            orientation: self.orientation,
        })
    }

    fn read_dqt(&mut self, mut p: usize, end: usize) -> Result<()> {
        while p < end {
            let pq = self.d[p] >> 4;
            let tq = (self.d[p] & 15) as usize;
            p += 1;
            if pq > 1 {
                return Err("jpeg: bad DQT precision".to_string());
            }
            let need = if pq == 0 { 64 } else { 128 };
            if p + need > self.d.len() {
                return Err("jpeg: truncated DQT".to_string());
            }
            // Quant values arrive in zigzag order; store them in natural order.
            let mut q = [1i32; 64];
            for nat in 0..64 {
                q[nat] = if pq == 0 {
                    self.d[p + NATURAL[nat]] as i32
                } else {
                    let i = p + NATURAL[nat] * 2;
                    u16::from_be_bytes([self.d[i], self.d[i + 1]]) as i32
                };
            }
            self.qt[tq.min(3)] = q;
            p += need;
        }
        Ok(())
    }

    fn read_dht(&mut self, mut p: usize, end: usize) -> Result<()> {
        while p < end {
            let class = (self.d[p] >> 4) as usize;
            let id = (self.d[p] & 15) as usize;
            p += 1;
            if p + 16 > end {
                return Err("jpeg: truncated DHT".to_string());
            }
            let mut counts = [0u8; 16];
            for c in counts.iter_mut() {
                *c = self.d[p];
                p += 1;
            }
            let total: usize = counts.iter().map(|c| *c as usize).sum();
            if p + total > self.d.len() {
                return Err("jpeg: truncated DHT values".to_string());
            }
            let table = Huff::build(&counts, &self.d[p..p + total]);
            p += total;
            if class == 0 {
                self.dc[id.min(3)] = table;
            } else {
                self.ac[id.min(3)] = table;
            }
        }
        Ok(())
    }

    fn read_sof(&mut self, p: usize) -> Result<()> {
        self.precision = self.d[p];
        self.height = self.u16(p + 1) as usize;
        self.width = self.u16(p + 3) as usize;
        let n = self.d[p + 5] as usize;
        if n == 0 || n > 4 {
            return Err("jpeg: bad component count".to_string());
        }
        let mut specs = Vec::with_capacity(n);
        for i in 0..n {
            let o = p + 6 + i * 3;
            let id = self.d[o];
            let hv = self.d[o + 1];
            let tq = (self.d[o + 2] & 15) as usize;
            specs.push((id, hv >> 4, hv & 15, tq));
        }
        self.max_h = specs.iter().map(|s| s.1.max(1) as usize).max().unwrap_or(1);
        self.max_v = specs.iter().map(|s| s.2.max(1) as usize).max().unwrap_or(1);
        if self.max_h > 4 || self.max_v > 4 {
            return Err("jpeg: bad sampling factors".to_string());
        }
        let mcos_x = (self.width + 8 * self.max_h - 1) / (8 * self.max_h);
        let mcos_y = (self.height + 8 * self.max_v - 1) / (8 * self.max_v);
        self.mcu_w = mcos_x;
        self.mcu_h = mcos_y;
        self.comps.clear();
        for (id, h, v, tq) in specs {
            let mut c = Comp::new();
            c.id = id;
            c.h = h.max(1);
            c.v = v.max(1);
            c.tq = tq;
            c.bw = mcos_x * c.h as usize;
            c.bh = mcos_y * c.v as usize;
            c.coefs = vec![0i32; c.bw * c.bh * 64];
            self.comps.push(c);
        }
        Ok(())
    }

    /// Decode every scan in the file. Baseline scans only (Ss=0, Se=63, Ah=Al=0).
    fn scans(&mut self) -> Result<()> {
        loop {
            // Find the next SOS.
            let (ns, ss, se, ah, al) = self.read_sos()?;
            if ns == 0 {
                break;
            }
            if ss != 0 || se != 63 || ah != 0 || al != 0 || self.progressive {
                return Err("jpeg: progressive/multi-scan AC unsupported".to_string());
            }
            // Copy the scan's tables out of `self` so entropy decoding can mutate
            // coefficient storage while reading them (borrow-splitting by field).
            let dc_tab: Vec<Huff> = (0..4).map(|i| self.dc[i].clone()).collect();
            let ac_tab: Vec<Huff> = (0..4).map(|i| self.ac[i].clone()).collect();
            let mut br = BitReader::new(self.d, self.pos);
            let mut mcu_seen = 0usize;
            'mcus: for my in 0..self.mcu_h {
                for mx in 0..self.mcu_w {
                    for i in 0..ns {
                        let ci = self.scan_comp[i];
                        let dct = self.scan_dct[i];
                        let act = self.scan_act[i];
                        let (hb, vb, blocks_per_row) = {
                            let c = &self.comps[ci];
                            (c.h as usize, c.v as usize, c.bw)
                        };
                        for by in 0..vb {
                            for bx in 0..hb {
                                let blk = (my * vb + by) * blocks_per_row + (mx * hb + bx);
                                if let Err(e) =
                                    self.decode_block(&mut br, ci, blk, &dc_tab[dct], &ac_tab[act])
                                {
                                    if br.marker != 0 {
                                        // Entropy data ended at a marker (normal for
                                        // the last MCU); finish the scan politely.
                                        break 'mcus;
                                    }
                                    return Err(e);
                                }
                            }
                        }
                    }
                    mcu_seen += 1;
                    if self.restart > 0 && mcu_seen % self.restart == 0 {
                        skip_restart(&mut br);
                        for c in self.comps.iter_mut() {
                            c.dc_pred = 0;
                        }
                    }
                }
            }
            self.pos = br.pos;
        }
        Ok(())
    }

    /// Read one SOS header; returns (Ns, Ss, Se, Ah, Al) and leaves `pos` at the
    /// first entropy byte. `Ns == 0` means no more scans.
    fn read_sos(&mut self) -> Result<(usize, usize, usize, usize, usize)> {
        loop {
            if self.pos + 2 > self.d.len() {
                return Ok((0, 0, 0, 0, 0));
            }
            if self.d[self.pos] != 0xff {
                self.pos += 1;
                continue;
            }
            while self.pos < self.d.len() && self.d[self.pos] == 0xff {
                self.pos += 1;
            }
            if self.pos >= self.d.len() {
                return Ok((0, 0, 0, 0, 0));
            }
            let m = self.d[self.pos];
            self.pos += 1;
            if m == 0xda {
                break;
            }
            if m == 0xd9 {
                return Ok((0, 0, 0, 0, 0));
            }
            let len = self.u16(self.pos) as usize;
            self.pos += len.max(2);
        }
        let start = self.pos;
        let len = self.u16(start) as usize;
        let ns = self.d[start + 2] as usize;
        if ns == 0 || ns > 4 || start + 2 + ns * 3 + 3 > self.d.len() {
            return Err("jpeg: bad SOS".to_string());
        }
        for i in 0..ns {
            let o = start + 3 + i * 2;
            let cid = self.d[o];
            let tbl = self.d[o + 1];
            let ci = self
                .comps
                .iter()
                .position(|c| c.id == cid)
                .ok_or_else(|| "jpeg: SOS names unknown component".to_string())?;
            self.scan_comp[i] = ci;
            self.scan_dct[i] = ((tbl >> 4) & 15) as usize;
            self.scan_act[i] = (tbl & 15) as usize;
        }
        let p = start + 3 + ns * 2;
        let ss = self.d[p] as usize;
        let se = self.d[p + 1] as usize;
        let ap = self.d[p + 2];
        self.pos = start + len;
        Ok((ns, ss, se, (ap >> 4) as usize, (ap & 15) as usize))
    }

    fn decode_block(
        &mut self,
        br: &mut BitReader,
        ci: usize,
        blk: usize,
        dc: &Huff,
        ac: &Huff,
    ) -> Result<()> {
        let base = blk * 64;
        if base + 64 > self.comps[ci].coefs.len() {
            return Err("jpeg: block index out of range".to_string());
        }
        if !dc.usable() {
            return Err("jpeg: missing DC table".to_string());
        }
        let rs = br.symbol(dc)?;
        let diff = br.receive(rs as u32)?;
        self.comps[ci].dc_pred += diff;
        self.comps[ci].coefs[base] = self.comps[ci].dc_pred;
        if self.comps[ci].coefs[base] > 2047 || self.comps[ci].coefs[base] < -2047 {
            return Err("jpeg: DC out of range".to_string());
        }
        if !ac.usable() {
            return Err("jpeg: missing AC table".to_string());
        }
        let mut k = 1usize;
        while k < 64 {
            let rs = br.symbol(ac)?;
            let r = (rs >> 4) as usize;
            let s = (rs & 15) as usize;
            if s == 0 {
                if r != 15 {
                    break;
                }
                k += 16;
                continue;
            }
            k += r;
            if k > 63 {
                return Err("jpeg: AC run past end of block".to_string());
            }
            let v = br.receive(s as u32)?;
            self.comps[ci].coefs[base + NATURAL[k]] = v;
            k += 1;
        }
        Ok(())
    }

    /// Dequantise + IDCT every block into 8-bit component planes, then colour
    /// convert to RGBA at the display size.
    fn render(&self) -> Result<Vec<u8>> {
        let mut cos = [[0f32; 8]; 8];
        for u in 0..8 {
            for x in 0..8 {
                cos[u][x] = (((2 * x + 1) as f32) * u as f32 * std::f32::consts::PI / 16.0).cos();
            }
        }
        let mut planes: Vec<(Vec<u8>, usize, usize)> = Vec::new();
        for c in self.comps.iter() {
            let pw = c.bw * 8;
            let ph = c.bh * 8;
            let mut plane = vec![128u8; pw * ph];
            let q = &self.qt[c.tq.min(3)];
            for by in 0..c.bh {
                for bx in 0..c.bw {
                    let base = (by * c.bw + bx) * 64;
                    let mut blk = [0f32; 64];
                    for i in 0..64 {
                        blk[i] = c.coefs[base + i] as f32 * q[i] as f32;
                    }
                    let mut out = [0f32; 64];
                    idct(&blk, &mut out, &cos);
                    for y in 0..8 {
                        for x in 0..8 {
                            let v = clamp_i32(out[y * 8 + x] as i32 + 128, 0, 255) as u8;
                            plane[(by * 8 + y) * pw + bx * 8 + x] = v;
                        }
                    }
                }
            }
            planes.push((plane, pw, ph));
        }
        let w = self.width;
        let h = self.height;
        let mut rgba = vec![255u8; w * h * 4];
        if planes.len() == 1 {
            let (p, pw, _) = &planes[0];
            for y in 0..h {
                for x in 0..w {
                    let v = p[y * *pw + x];
                    let o = (y * w + x) * 4;
                    rgba[o] = v;
                    rgba[o + 1] = v;
                    rgba[o + 2] = v;
                }
            }
        } else if planes.len() >= 3 {
            let (yp, ypw, _) = &planes[0];
            let (cbp, cpw, cph) = &planes[1];
            let (crp, _, _) = &planes[2];
            let sx = (self.max_h / self.comps[1].h as usize).max(1);
            let sy = (self.max_v / self.comps[1].v as usize).max(1);
            let rgb_planes = self.ids_are_rgb();
            for y in 0..h {
                for x in 0..w {
                    let yy = yp[y * *ypw + x] as i32;
                    let cx = (x / sx).min(cpw.saturating_sub(1));
                    let cy = (y / sy).min(cph.saturating_sub(1));
                    let ci = cy * *cpw + cx;
                    let (r, g, b) = if rgb_planes {
                        (
                            yy as u8,
                            cbp[ci.min(cbp.len() - 1)],
                            crp[ci.min(crp.len() - 1)],
                        )
                    } else {
                        let cb = cbp[ci.min(cbp.len() - 1)] as i32 - 128;
                        let cr = crp[ci.min(crp.len() - 1)] as i32 - 128;
                        // ITU-R BT.601 fixed point (same rounding as libjpeg's default).
                        (
                            clamp_i32(yy + ((91881 * cr) >> 16), 0, 255) as u8,
                            clamp_i32(yy - ((22554 * cb + 46802 * cr) >> 16), 0, 255) as u8,
                            clamp_i32(yy + ((116130 * cb) >> 16), 0, 255) as u8,
                        )
                    };
                    let o = (y * w + x) * 4;
                    rgba[o] = r;
                    rgba[o + 1] = g;
                    rgba[o + 2] = b;
                }
            }
        } else {
            return Err("jpeg: 2-component files are not supported".to_string());
        }
        Ok(rgba)
    }

    fn ids_are_rgb(&self) -> bool {
        self.comps.len() == 3
            && self.comps[0].id == b'R'
            && self.comps[1].id == b'G'
            && self.comps[2].id == b'B'
    }
}

fn skip_restart(br: &mut BitReader) {
    // Byte align, then consume the next RSTn marker if present.
    br.cnt -= br.cnt % 8;
    while br.pos < br.d.len() && br.d[br.pos] != 0xff {
        br.pos += 1;
    }
    if br.pos < br.d.len() {
        br.pos += 1;
        while br.pos < br.d.len() && br.d[br.pos] == 0xff {
            br.pos += 1;
        }
        if br.pos < br.d.len() {
            let m = br.d[br.pos];
            if (0xd0..=0xd7).contains(&m) {
                br.pos += 1;
            }
        }
    }
    br.acc = 0;
    br.cnt = 0;
    br.marker = 0;
}

fn idct(src: &[f32; 64], dst: &mut [f32; 64], cos: &[[f32; 8]; 8]) {
    let s0 = std::f32::consts::FRAC_1_SQRT_2;
    let mut tmp = [0f32; 64];
    for x in 0..8 {
        for y in 0..8 {
            let mut acc = 0f32;
            for u in 0..8 {
                let du = src[u * 8 + x] * if u == 0 { s0 } else { 1.0 };
                if du != 0.0 {
                    acc += du * cos[u][y];
                }
            }
            tmp[y * 8 + x] = acc;
        }
    }
    for y in 0..8 {
        for x in 0..8 {
            let mut acc = 0f32;
            for v in 0..8 {
                let tv = tmp[y * 8 + v] * if v == 0 { s0 } else { 1.0 };
                if tv != 0.0 {
                    acc += tv * cos[v][x];
                }
            }
            dst[y * 8 + x] = acc * 0.125;
        }
    }
}

fn exif_orientation(app1: &[u8]) -> u32 {
    if app1.len() < 14 || &app1[..4] != b"Exif" {
        return 1;
    }
    let t = &app1[6..];
    let le = t[0] == b'I';
    let g16 = |i: usize| -> Option<u32> {
        Some(if le {
            u16::from_le_bytes(*t.get(i..i + 2)?) as u32
        } else {
            u16::from_be_bytes(*t.get(i..i + 2)?) as u32
        })
    };
    let g32 = |i: usize| -> Option<u32> {
        Some(if le {
            u32::from_le_bytes(*t.get(i..i + 4)?)
        } else {
            u32::from_be_bytes(*t.get(i..i + 4)?)
        })
    };
    let ifd = g32(4)? as usize;
    let n = g16(ifd)? as usize;
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        if e + 12 > t.len() {
            break;
        }
        if g16(e)? == 0x0112 {
            return g16(e + 8)?.clamp(1, 8);
        }
    }
    1
}

pub fn header_of(data: &[u8]) -> Option<Header> {
    let mut j = Jpg::new(data);
    j.headers().ok()
}

pub fn decode(data: &[u8]) -> Result<Decoded> {
    let mut j = Jpg::new(data);
    let hdr = j.headers()?;
    if hdr.progressive || hdr.arithmetic {
        return Err("jpeg: needs external decoder (progressive or arithmetic)".to_string());
    }
    for c in j.comps.iter() {
        if !j.dc[c.dct.min(3)].usable() {
            return Err("jpeg: component has no DC table".to_string());
        }
        if !j.ac[c.act.min(3)].usable() {
            return Err("jpeg: component has no AC table".to_string());
        }
    }
    j.scans()?;
    let px = j.render()?;
    let (w, h, rgba) = apply_orientation(px, j.width, j.height, j.orientation);
    Ok(Decoded::still(w as u32, h as u32, rgba))
}

/// Rotate/flip pixels for EXIF orientation 1..=8.
pub fn apply_orientation(
    rgba: Vec<u8>,
    w: usize,
    h: usize,
    o: u32,
) -> (usize, usize, Vec<u8>) {
    if o <= 1 {
        return (w, h, rgba);
    }
    let swap = o >= 5;
    let (nw, nh) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                _ => (y, w - 1 - x),
            };
            let i = (y * w + x) * 4;
            let j = (dy * nw + dx) * 4;
            if j + 3 < out.len() && i + 3 < rgba.len() {
                out[j..j + 4].copy_from_slice(&rgba[i..i + 4]);
            }
        }
    }
    (nw, nh, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"not a jpeg").is_err());
        assert!(is_jpeg(b"\xff\xd8\xff\xe0abc"));
        assert!(!is_jpeg(b"PNG"));
    }

    #[test]
    fn natural_table_is_a_permutation() {
        let mut seen = [false; 64];
        for v in NATURAL.iter() {
            assert!(*v < 64);
            assert!(!seen[*v], "duplicate in zigzag table");
            seen[*v] = true;
        }
        assert!(seen.iter().all(|s| *s));
        assert_eq!(NATURAL[1], 8);
        assert_eq!(NATURAL[63], 63);
    }

    #[test]
    fn huffman_table_matches_counts() {
        // The all-8-bit toy alphabet: 256 symbols, one code each of length 8.
        let mut counts = [0u8; 16];
        counts[7] = 256;
        let vals: Vec<u8> = (0..256u16).map(|v| v as u8).collect();
        let h = Huff::build(&counts, &vals);
        assert_eq!(h.first_code[8], 0);
        assert_eq!(h.first_code[9], 512);
        assert!(h.usable());
    }

    fn fixture_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
    }

    /// Mean absolute per-channel difference against a reference RGBA8 dump.
    fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
        let n = a.len().min(b.len());
        if n == 0 {
            return 999.0;
        }
        let mut acc = 0u64;
        for i in 0..n {
            acc += (a[i] as i32 - b[i] as i32).unsigned_abs() as u64;
        }
        acc as f64 / n as f64
    }

    #[test]
    fn decodes_baseline_fixtures_like_libjpeg() {
        let dir = fixture_dir();
        let Ok(jpg) = std::fs::read(dir.join("baseline.jpg")) else {
            return; // fixtures are generated by scripts/make-fixtures.py
        };
        let d = decode(&jpg).unwrap_or_else(|e| panic!("baseline.jpg: {e}"));
        assert_eq!((d.width, d.height), (64, 48));
        let want = std::fs::read(dir.join("baseline.ref.rgba")).unwrap_or_default();
        assert_eq!(want.len(), 64 * 48 * 4);
        // Different IDCT/upsampling implementations differ by a few levels; more
        // than that means the decode is genuinely wrong.
        let diff = mean_abs_diff(&d.rgba, &want);
        assert!(diff < 6.0, "mean abs diff vs reference decode = {diff}");
        // A gradient must be monotonic-ish: left dark, right light.
        let left = d.rgba[0] as i32;
        let right = d.rgba[(48 * 64 - 1) * 4] as i32;
        assert!(right > left + 40, "gradient lost: {left} -> {right}");

        let gray = std::fs::read(dir.join("gray.jpg")).ok();
        if let Some(g) = gray {
            let d = decode(&g).unwrap_or_else(|e| panic!("gray.jpg: {e}"));
            assert_eq!((d.width, d.height), (64, 48));
            for px in d.rgba.chunks(4) {
                assert_eq!(px[0], px[1]);
                assert_eq!(px[1], px[2]);
            }
        }
        let yuv = std::fs::read(dir.join("yuv444.jpg")).ok();
        if let Some(u) = yuv {
            let d = decode(&u).unwrap_or_else(|e| panic!("yuv444.jpg: {e}"));
            assert_eq!((d.width, d.height), (64, 48));
            assert!(mean_abs_diff(&d.rgba, &want) < 6.0);
        }
    }

    #[test]
    fn progressive_is_detected_not_misdecoded() {
        let dir = fixture_dir();
        let Ok(prog) = std::fs::read(dir.join("progressive.jpg")) else {
            return;
        };
        let hdr = header_of(&prog).expect("progressive header parses");
        assert!(hdr.progressive, "must be flagged progressive");
        let err = decode(&prog).unwrap_err();
        assert!(err.contains("progressive") || err.contains("external"), "{err}");
    }

    #[test]
    fn decode_fixture_if_present() {
        // tests/data/baseline.jpg is generated by scripts/make-fixtures.mjs;
        // `expected.txt` holds "<w> <h> <sum-of-all-bytes>".
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let Ok(bytes) = std::fs::read(dir.join("baseline.jpg")) else {
            return;
        };
        let d = decode(&bytes).unwrap_or_else(|e| panic!("fixture decode failed: {e}"));
        let expect = std::fs::read_to_string(dir.join("baseline.expected.txt")).unwrap_or_default();
        let mut it = expect.split_whitespace();
        let w: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(d.width);
        let h: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(d.height);
        assert_eq!((d.width, d.height), (w, h));
    }
}
