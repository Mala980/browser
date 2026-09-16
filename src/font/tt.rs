//! TrueType / OpenType-with-glyf / WOFF reader, written to the spec.
//!
//! `tests/data/kilat-mini.ttf` is produced by `scripts/make-font-fixture.py` and
//! cross-checked against fontTools, so the offsets below are real sfnt offsets.
//!
//! Tables read: `head` (unitsPerEm, indexToLocFormat), `hhea` (ascender,
//! descender, lineGap, numberOfHMetrics), `maxp` (numGlyphs), `cmap` (formats 4,
//! 6, 12), `hmtx` (advances + lsb), `name` (family/subfamily/full/postscript in
//! UTF-16BE or Mac Roman), `OS/2` (weight, fsSelection, sxHeight, sCapHeight),
//! `post` (italicAngle, isFixedPitch), `kern` (format 0 horizontal pairs) and
//! `loca` + `glyf` (simple and composite outlines).
//!
//! CFF-only faces (most `.otf`) are detected: metrics and advance widths stay
//! usable, and drawing falls through to the next face in the chain.

use crate::util::Result;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
    /// On-curve points are true; false marks a quadratic control point.
    pub on: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contour {
    pub pts: Vec<Pt>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bbox {
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
}

/// Big-endian cursor that never panics: reads past the end yield 0, so a
/// truncated font degrades to "no such glyph" instead of a crash.
pub struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Rd<'a> {
    pub fn at(b: &'a [u8], i: usize) -> Rd<'a> {
        Rd { b, i }
    }
    pub fn u8(&mut self) -> u8 {
        let v = self.b.get(self.i).copied().unwrap_or(0);
        self.i += 1;
        v
    }
    pub fn u16(&mut self) -> u16 {
        ((self.u8() as u16) << 8) | self.u8() as u16
    }
    pub fn i16(&mut self) -> i16 {
        self.u16() as i16
    }
    pub fn u32(&mut self) -> u32 {
        ((self.u16() as u32) << 16) | self.u16() as u32
    }
    pub fn skip(&mut self, n: usize) {
        self.i += n;
    }
}

fn u16at(b: &[u8], i: usize) -> u16 {
    if i + 2 > b.len() {
        return 0;
    }
    u16::from_be_bytes([b[i], b[i + 1]])
}

fn i16at(b: &[u8], i: usize) -> i16 {
    u16at(b, i) as i16
}

fn u32at(b: &[u8], i: usize) -> u32 {
    if i + 4 > b.len() {
        return 0;
    }
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn i32at(b: &[u8], i: usize) -> i32 {
    u32at(b, i) as i32
}

fn tag4(s: &str) -> [u8; 4] {
    let b = s.as_bytes();
    [b[0], b[1], b[2], b[3]]
}

/// A parsed face. Owns its bytes so the table directory stays valid.
pub struct Face {
    pub data: Vec<u8>,
    /// Table directory: tag -> (offset, length).
    pub tables: HashMap<[u8; 4], (usize, usize)>,
    pub family: String,
    pub subfamily: String,
    pub full_name: String,
    pub postscript_name: String,
    pub units_per_em: u16,
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    pub cap_height: i16,
    pub x_height: i16,
    pub weight: u16,
    pub italic: bool,
    pub bold: bool,
    pub fixed_pitch: bool,
    pub num_glyphs: u16,
    pub index_to_loc_format: i16,
    pub is_cff: bool,
    pub has_outlines: bool,
    cmap: HashMap<u32, u16>,
    advances: Vec<u16>,
    lsb: Vec<i16>,
    kern: HashMap<u32, i16>,
}

impl std::fmt::Debug for Face {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Face({} {} upm={} glyphs={} cmap={})",
            self.family,
            self.subfamily,
            self.units_per_em,
            self.num_glyphs,
            self.cmap.len()
        )
    }
}

impl Face {
    /// Sniff the container (`wOFF`, sfnt, `ttcf`) and parse it.
    pub fn parse(bytes: Vec<u8>) -> Result<Face> {
        if bytes.len() < 16 {
            return Err("font: too small to be a font".to_string());
        }
        if &bytes[0..4] == b"wOFF" {
            let data = unpack_woff(&bytes)?;
            return Face::parse_sfnt(data);
        }
        if &bytes[0..4] == b"ttcf" {
            // Font collection: take the first face.
            let off = u32at(&bytes, 12) as usize;
            if off >= bytes.len() {
                return Err("font: bad ttcf offset".to_string());
            }
            return Face::parse_sfnt(bytes[off..].to_vec());
        }
        Face::parse_sfnt(bytes)
    }

    pub fn parse_file(path: &str) -> Result<Face> {
        let bytes = std::fs::read(path).map_err(|e| format!("font {path}: {e}"))?;
        Face::parse(bytes)
    }

    fn parse_sfnt(data: Vec<u8>) -> Result<Face> {
        let num = u16at(&data, 4) as usize;
        if num == 0 || 12 + 16 * num > data.len() {
            return Err("font: bad sfnt directory".to_string());
        }
        let mut tables: HashMap<[u8; 4], (usize, usize)> = HashMap::new();
        for i in 0..num {
            let base = 12 + 16 * i;
            let t = [data[base], data[base + 1], data[base + 2], data[base + 3]];
            let off = u32at(&data, base + 8) as usize;
            let len = u32at(&data, base + 12) as usize;
            if off.checked_add(len).map(|e| e > data.len()).unwrap_or(true) {
                // A table claiming to run past EOF: skip it, keep the rest.
                continue;
            }
            tables.insert(t, (off, len));
        }
        // Slices over `data`; everything derived here is copied out before `data`
        // moves into the Face, so no borrow has to survive the move.
        let slice = |t: &str| -> Option<&[u8]> {
            tables.get(&tag4(t)).map(|(o, l)| &data[*o..*o + *l])
        };
        let head = slice("head").ok_or_else(|| "font: no head table".to_string())?;
        let units_per_em = u16at(head, 18).max(1);
        let index_to_loc_format = i16at(head, 50);
        let hhea = slice("hhea").ok_or_else(|| "font: no hhea table".to_string())?;
        let mut ascender = i16at(hhea, 4);
        let mut descender = i16at(hhea, 6);
        let mut line_gap = i16at(hhea, 8);
        let num_hmetrics = u16at(hhea, 34) as usize;
        let maxp = slice("maxp").ok_or_else(|| "font: no maxp table".to_string())?;
        let num_glyphs = u16at(maxp, 4).max(1);
        let is_cff = slice("CFF ").is_some() && slice("glyf").is_none();
        let has_outlines = slice("glyf").is_some();

        let mut weight = 400u16;
        let mut italic = false;
        let mut bold = false;
        let mut cap_height = 0i16;
        let mut x_height = 0i16;
        if let Some(os2) = slice("OS/2") {
            let ver = u16at(os2, 0);
            weight = u16at(os2, 4).max(1);
            // fsSelection: bit0 ITALIC, bit5 BOLD, bit6 REGULAR.
            let fsel = u16at(os2, 62);
            if fsel != 0 {
                italic = fsel & 0x01 != 0;
                bold = fsel & 0x20 != 0;
            }
            bold |= weight >= 700;
            if ver >= 1 {
                let ta = i16at(os2, 68);
                let td = i16at(os2, 70);
                if ta != 0 {
                    ascender = ta;
                }
                if td != 0 {
                    descender = td;
                }
                line_gap = i16at(os2, 72);
            }
            if ver >= 2 && os2.len() >= 90 {
                x_height = i16at(os2, 86);
                cap_height = i16at(os2, 88);
            }
        }
        let mut fixed_pitch = false;
        if let Some(post) = slice("post") {
            if post.len() >= 16 {
                // italicAngle is 16.16 fixed point; non-zero means oblique/italic.
                italic |= i32at(post, 4) != 0;
                fixed_pitch = u32at(post, 12) != 0;
            }
        }
        if ascender == 0 {
            ascender = (units_per_em as f32 * 0.8) as i16;
        }
        if descender == 0 {
            descender = -((units_per_em as f32 * 0.2) as i16);
        }
        let names = names_of(slice("name"));
        let cmap = cmap_of(slice("cmap"));
        if cmap.is_empty() {
            return Err(format!(
                "font: no usable cmap subtable in {:?}",
                if names.full_name.is_empty() {
                    "unnamed font"
                } else {
                    &names.full_name
                }
            ));
        }
        let (advances, lsb) = hmtx_of(slice("hmtx"), num_hmetrics, num_glyphs as usize);
        let kern = kern_of(slice("kern"));
        Ok(Face {
            data,
            tables,
            family: names.family,
            subfamily: names.subfamily,
            full_name: names.full_name,
            postscript_name: names.postscript_name,
            units_per_em,
            ascender,
            descender,
            line_gap,
            cap_height,
            x_height,
            weight,
            italic,
            bold,
            fixed_pitch,
            num_glyphs,
            index_to_loc_format,
            is_cff,
            has_outlines,
            cmap,
            advances,
            lsb,
            kern,
        })
    }

    // ---- queries ----------------------------------------------------------

    pub fn glyph_for(&self, ch: char) -> Option<u16> {
        self.cmap.get(&(ch as u32)).copied().filter(|&g| g != 0)
    }

    /// Advance width in font units.
    pub fn advance_units(&self, gid: u16) -> u16 {
        self.advances.get(gid as usize).copied().unwrap_or(0)
    }

    pub fn lsb_units(&self, gid: u16) -> i16 {
        self.lsb.get(gid as usize).copied().unwrap_or(0)
    }

    pub fn kern_units(&self, left: u16, right: u16) -> i16 {
        self.kern
            .get(&((u32::from(left) << 16) | u32::from(right)))
            .copied()
            .unwrap_or(0)
    }

    /// Font units -> CSS pixels for a given em size.
    pub fn scale(&self, px: f32) -> f32 {
        px / self.units_per_em as f32
    }

    pub fn ascent(&self, px: f32) -> f32 {
        self.ascender as f32 * self.scale(px)
    }

    pub fn descent(&self, px: f32) -> f32 {
        (self.descender as f32).abs() * self.scale(px)
    }

    pub fn line_gap_px(&self, px: f32) -> f32 {
        self.line_gap as f32 * self.scale(px)
    }

    pub fn cap_height_px(&self, px: f32) -> f32 {
        if self.cap_height > 0 {
            self.cap_height as f32 * self.scale(px)
        } else {
            self.ascender as f32 * 0.72 * self.scale(px)
        }
    }

    pub fn x_height_px(&self, px: f32) -> f32 {
        if self.x_height > 0 {
            self.x_height as f32 * self.scale(px)
        } else {
            self.ascender as f32 * 0.52 * self.scale(px)
        }
    }

    /// Text width in pixels; `kerning` applies the `kern` table when present.
    pub fn measure(&self, text: &str, px: f32, kerning: bool) -> f32 {
        let s = self.scale(px);
        let mut total = 0.0f32;
        let mut prev: Option<u16> = None;
        for ch in text.chars() {
            let gid = match self.glyph_for(ch) {
                Some(g) => g,
                None => continue,
            };
            if let Some(p) = prev {
                if kerning {
                    total += self.kern_units(p, gid) as f32 * s;
                }
            }
            total += self.advance_units(gid) as f32 * s;
            prev = Some(gid);
        }
        total
    }

    pub fn coverage(&self, ch: char) -> bool {
        self.glyph_for(ch).is_some()
    }

    pub fn glyph_count(&self) -> usize {
        self.cmap.len()
    }

    /// `size`-pixel underline position/thickness, from `post` when sane.
    pub fn underline(&self, px: f32) -> (f32, f32) {
        let s = self.scale(px);
        match self.tables.get(&tag4("post")).map(|(o, _)| *o) {
            Some(off) => (
                i16at(&self.data, off + 8) as f32 * s,
                i16at(&self.data, off + 10) as f32 * s,
            ),
            None => (-0.1 * px, 0.08 * px),
        }
    }

    fn loca_entry(&self, gid: usize) -> usize {
        let (off, len) = match self.tables.get(&tag4("loca")) {
            Some(v) => *v,
            None => return 0,
        };
        let _ = len;
        if self.index_to_loc_format == 0 {
            u16at(&self.data, off + 2 * gid) as usize * 2
        } else {
            u32at(&self.data, off + 4 * gid) as usize
        }
    }

    fn glyph_bytes(&self, gid: u16) -> Option<&[u8]> {
        let (toff, tlen) = *self.tables.get(&tag4("glyf"))?;
        let start = self.loca_entry(gid as usize).min(tlen);
        let end = self.loca_entry(gid as usize + 1).min(tlen);
        if end <= start {
            return None;
        }
        let a = toff + start;
        let b = (toff + end).min(toff + tlen);
        self.data.get(a..b)
    }

    /// Glyph outline in font units (y up), composites flattened.
    pub fn outline(&self, gid: u16) -> Result<Vec<Contour>> {
        if !self.has_outlines {
            return Err("font: face has no glyf table (CFF outline)".to_string());
        }
        let mut out = Vec::new();
        match self.glyph_bytes(gid) {
            Some(g) => read_glyph(self, g, Xform::default(), 0, &mut out)?,
            None => return Ok(out),
        }
        Ok(out)
    }

    /// Tight bounding box in font units, or None for empty glyphs.
    /// Rasterise `gid` at `px` size into an alpha mask.
    pub fn mask(&self, gid: u16, px: f32) -> Option<crate::font::raster::Mask> {
        let contours = self.outline(gid).ok()?;
        let bbox = self.bbox(gid)?;
        Some(crate::font::raster::glyph_mask(
            &contours,
            self.advance_units(gid) as f32,
            Some((bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max)),
            self.scale(px),
        ))
    }

    pub fn bbox(&self, gid: u16) -> Option<Bbox> {
        let g = self.glyph_bytes(gid)?;
        let ncont = i16at(g, 0);
        if ncont >= 0 {
            return Some(Bbox {
                x_min: i16at(g, 2) as f32,
                y_min: i16at(g, 4) as f32,
                x_max: i16at(g, 6) as f32,
                y_max: i16at(g, 8) as f32,
            });
        }
        // Composite: union the decoded outline instead of re-reading component
        // headers, so transforms are handled exactly once.
        let contours = self.outline(gid).ok()?;
        let mut bb = Bbox::default();
        let mut first = true;
        for c in contours {
            for p in c.pts {
                if first {
                    bb = Bbox {
                        x_min: p.x,
                        y_min: p.y,
                        x_max: p.x,
                        y_max: p.y,
                    };
                    first = false;
                } else {
                    bb.x_min = bb.x_min.min(p.x);
                    bb.y_min = bb.y_min.min(p.y);
                    bb.x_max = bb.x_max.max(p.x);
                    bb.y_max = bb.y_max.max(p.y);
                }
            }
        }
        if first {
            None
        } else {
            Some(bb)
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Names {
    family: String,
    subfamily: String,
    full_name: String,
    postscript_name: String,
}

fn names_of(n: Option<&[u8]>) -> Names {
    let mut out = Names::default();
    let n = match n {
        Some(n) if n.len() >= 6 => n,
        _ => return out,
    };
    let count = u16at(n, 2) as usize;
    let storage = u16at(n, 4) as usize;
    // Prefer Windows/Mac Unicode records; Mac Roman is a fallback only.
    let mut best: HashMap<u16, (u8, String)> = HashMap::new();
    for i in 0..count {
        let base = 6 + 12 * i;
        if base + 12 > n.len() {
            break;
        }
        let plat = u16at(n, base);
        let id = u16at(n, base + 6);
        let len = u16at(n, base + 8) as usize;
        let off = u16at(n, base + 10) as usize;
        let raw = match n.get(storage + off..storage + off + len) {
            Some(s) => s,
            None => continue,
        };
        let (rank, text) = match plat {
            3 | 0 => (2u8, decode_utf16be(raw)),
            1 => (1u8, decode_mac_roman(raw)),
            _ => continue,
        };
        match best.get(&id) {
            Some((r, _)) if *r >= rank => {}
            _ => {
                best.insert(id, (rank, text));
            }
        }
    }
    let mut take = |id: u16| best.remove(&id).map(|(_, t)| t).unwrap_or_default();
    out.family = take(1);
    out.subfamily = take(2);
    out.full_name = take(4);
    out.postscript_name = take(6);
    // Typographic family (16) is what CSS family matching should use.
    let typo = take(16);
    if !typo.is_empty() {
        out.family = typo;
    }
    if out.full_name.is_empty() {
        out.full_name = out.family.clone();
    }
    out
}

fn cmap_of(c: Option<&[u8]>) -> HashMap<u32, u16> {
    let mut map = HashMap::new();
    let c = match c {
        Some(c) if c.len() >= 4 => c,
        _ => return map,
    };
    let num = u16at(c, 2) as usize;
    let mut chosen: Option<(u8, usize)> = None;
    for i in 0..num {
        let base = 4 + 8 * i;
        if base + 8 > c.len() {
            break;
        }
        let plat = u16at(c, base);
        let enc = u16at(c, base + 2);
        let off = u32at(c, base + 4) as usize;
        let rank: u8 = match (plat, enc) {
            (3, 10) => 6,
            (0, 4) | (0, 6) => 5,
            (3, 1) => 4,
            (0, 3) => 3,
            (1, 0) | (1, 25) => 2,
            (3, 0) => 1,
            _ => 0,
        };
        if rank > 0 && off < c.len() {
            match chosen {
                Some((r, _)) if r >= rank => {}
                _ => chosen = Some((rank, off)),
            }
        }
    }
    let off = match chosen {
        Some((_, o)) => o,
        // Any readable subtable beats none.
        None => (0..num)
            .map(|i| u32at(c, 4 + 8 * i) as usize)
            .find(|&o| o + 2 <= c.len())
            .unwrap_or(0),
    };
    match u16at(c, off) {
        4 => read_cmap4(c, off, &mut map),
        6 => {
            let first = u16at(c, off + 6);
            let cnt = u16at(c, off + 8) as usize;
            for k in 0..cnt.min(0x10000) {
                let gid = u16at(c, off + 10 + 2 * k);
                if gid != 0 {
                    map.insert(u32::from(first) + k as u32, gid);
                }
            }
        }
        12 => {
            let groups = u32at(c, off + 12) as usize;
            for g in 0..groups.min(200_000) {
                let base = off + 16 + 12 * g;
                if base + 12 > c.len() {
                    break;
                }
                let start = u32at(c, base);
                let end = u32at(c, base + 4);
                let start_gid = u32at(c, base + 8);
                if end < start || end - start > 0x10_0000 {
                    continue;
                }
                for cp in start..=end {
                    let gid = start_gid + (cp - start);
                    if gid != 0 && gid <= 0xFFFF {
                        map.insert(cp, gid as u16);
                    }
                }
            }
        }
        _ => {}
    }
    map
}

fn read_cmap4(c: &[u8], off: usize, map: &mut HashMap<u32, u16>) {
    let seg_x2 = u16at(c, off + 6) as usize;
    let seg = seg_x2 / 2;
    if seg == 0 || seg > 0x8000 {
        return;
    }
    let ends = off + 14;
    let starts = ends + seg_x2 + 2;
    let deltas = starts + seg_x2;
    let ranges = deltas + seg_x2;
    for i in 0..seg {
        let end = u16at(c, ends + 2 * i);
        let start = u16at(c, starts + 2 * i);
        let delta = u16at(c, deltas + 2 * i);
        let ro = u16at(c, ranges + 2 * i);
        if start == 0xFFFF || end < start || end - start > 0x8000 {
            continue;
        }
        for cp in start..=end {
            let gid = if ro == 0 {
                cp.wrapping_add(delta)
            } else {
                let addr = ranges + 2 * i + ro as usize + 2 * (cp as usize - start as usize);
                let g = u16at(c, addr);
                if g == 0 {
                    0
                } else {
                    g.wrapping_add(delta)
                }
            };
            if gid != 0 {
                map.insert(u32::from(cp), gid);
            }
        }
    }
}

fn hmtx_of(h: Option<&[u8]>, num_hmetrics: usize, n: usize) -> (Vec<u16>, Vec<i16>) {
    let mut adv = vec![0u16; n];
    let mut lsb = vec![0i16; n];
    let h = match h {
        Some(h) => h,
        None => return (adv, lsb),
    };
    let mut r = Rd::at(h, 0);
    let nhm = num_hmetrics.clamp(1, n);
    // Glyphs past numberOfHMetrics repeat the last advance width.
    let mut last = 0u16;
    for i in 0..n {
        if i < nhm {
            last = r.u16();
        }
        adv[i] = last;
        lsb[i] = r.i16();
    }
    (adv, lsb)
}

fn kern_of(k: Option<&[u8]>) -> HashMap<u32, i16> {
    let mut map = HashMap::new();
    let k = match k {
        Some(k) if k.len() >= 4 => k,
        _ => return map,
    };
    let ntables = u16at(k, 2) as usize;
    let mut off = 4usize;
    for _ in 0..ntables {
        if off + 14 > k.len() {
            break;
        }
        let version = u16at(k, off);
        let len = u16at(k, off + 2) as usize;
        let coverage = u16at(k, off + 4);
        if len < 14 {
            break;
        }
        // The classic cross-format 0 horizontal subtable only.
        if version == 0 && coverage & 0x000F == 0x0001 {
            let npairs = u16at(k, off + 6) as usize;
            for p in 0..npairs.min(65535) {
                let base = off + 14 + 6 * p;
                if base + 6 > k.len() {
                    break;
                }
                let l = u16at(k, base);
                let r = u16at(k, base + 2);
                let v = i16at(k, base + 4);
                map.insert((u32::from(l) << 16) | u32::from(r), v);
            }
        }
        off += len;
    }
    map
}

/// Affine map applied to component outlines: x' = a*x + c*y + e, y' = b*x + d*y + f.
#[derive(Clone, Copy, Debug)]
struct Xform {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Default for Xform {
    fn default() -> Xform {
        Xform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 }
    }
}

impl Xform {
    /// `self` composed after `o` (i.e. apply `o`, then `self`'s parent chain).
    fn then(&self, o: &Xform) -> Xform {
        Xform {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }
    fn apply(&self, p: Pt) -> Pt {
        Pt {
            x: self.a * p.x + self.c * p.y + self.e,
            y: self.b * p.x + self.d * p.y + self.f,
            on: p.on,
        }
    }
}

const F26_6: f32 = 64.0;

fn fixed16_16(v: i16) -> f32 {
    f32::from(v) / F26_6
}

/// Decode one `glyf` record. Simple glyphs have their own points; composite
/// glyphs recurse over components with the accumulated transform.
fn read_glyph(
    face: &Face,
    g: &[u8],
    x: Xform,
    depth: u32,
    out: &mut Vec<Contour>,
) -> Result<()> {
    if depth > 8 {
        return Err("font: composite glyph nesting too deep".to_string());
    }
    if g.len() < 10 {
        return Ok(());
    }
    let ncont = i16at(g, 0);
    if ncont >= 0 {
        return read_simple(g, ncont as usize, &x, out);
    }
    // Composite.
    let mut i = 10usize;
    for _ in 0..256 {
        if i + 4 > g.len() {
            break;
        }
        let flags = u16at(g, i);
        let gid = u16at(g, i + 2);
        i += 4;
        let words = flags & 0x0001 != 0;
        let xy_values = flags & 0x0002 != 0;
        let (e, f): (f32, f32) = if words {
            let a = i16at(g, i);
            let b = i16at(g, i + 2);
            i += 4;
            (f32::from(a), f32::from(b))
        } else {
            let a = i8::from_be_bytes([g.get(i).copied().unwrap_or(0)]) as f32;
            let b = i8::from_be_bytes([g.get(i + 1).copied().unwrap_or(0)]) as f32;
            i += 2;
            (a, b)
        };
        // Point-matched components (args are glyph point indices) would need the
        // component's own outlines first; the offsets we just read are a
        // good-enough approximation for accented Latin, which is all Kilat draws.
        let (mut a, mut b, mut c, mut d) = (1.0f32, 0.0f32, 0.0f32, 1.0f32);
        if flags & 0x0008 != 0 {
            let s = fixed16_16(i16at(g, i));
            i += 2;
            a = s;
            d = s;
        } else if flags & 0x0040 != 0 {
            a = fixed16_16(i16at(g, i));
            d = fixed16_16(i16at(g, i + 2));
            i += 4;
        } else if flags & 0x0080 != 0 {
            a = fixed16_16(i16at(g, i));
            b = fixed16_16(i16at(g, i + 2));
            c = fixed16_16(i16at(g, i + 4));
            d = fixed16_16(i16at(g, i + 6));
            i += 8;
        }
        let comp = Xform { a, b, c, d, e, f }.then(&x);
        let _ = xy_values;
        if let Some(sub) = face.glyph_bytes(gid) {
            read_glyph(face, sub, comp, depth + 1, out)?;
        }
        if flags & 0x0020 == 0 {
            break;
        }
    }
    Ok(())
}

fn read_simple(g: &[u8], ncont: usize, x: &Xform, out: &mut Vec<Contour>) -> Result<()> {
    if ncont == 0 || ncont > 0x4000 {
        return Ok(());
    }
    let mut i = 10usize;
    let mut ends = Vec::with_capacity(ncont);
    for _ in 0..ncont {
        ends.push(u16at(g, i) as usize);
        i += 2;
    }
    let npts = match ends.last() {
        Some(&e) => e + 1,
        None => return Ok(()),
    };
    if npts == 0 || npts > 0x10000 {
        return Ok(());
    }
    let ilen = u16at(g, i) as usize;
    i += 2 + ilen;
    let mut flags: Vec<u8> = Vec::with_capacity(npts);
    while flags.len() < npts {
        let f = g.get(i).copied().unwrap_or(0);
        i += 1;
        flags.push(f);
        if f & 0x08 != 0 {
            let rep = g.get(i).copied().unwrap_or(0);
            i += 1;
            for _ in 0..rep {
                if flags.len() >= npts {
                    break;
                }
                flags.push(f);
            }
        }
    }
    let mut xs = Vec::with_capacity(npts);
    let mut px = 0f32;
    for &f in flags.iter() {
        if f & 0x02 != 0 {
            let v = g.get(i).copied().unwrap_or(0);
            i += 1;
            px += if f & 0x10 != 0 { v as f32 } else { -(v as f32) };
        } else {
            px += i16at(g, i) as f32;
            i += 2;
        }
        xs.push(px);
    }
    let mut ys = Vec::with_capacity(npts);
    let mut py = 0f32;
    for (k, &f) in flags.iter().enumerate() {
        if f & 0x04 != 0 {
            let v = g.get(i + k.min(usize::MAX)).copied().unwrap_or(0);
            i += 1;
            py += if f & 0x20 != 0 { v as f32 } else { -(v as f32) };
        } else {
            py += i16at(g, i) as f32;
            i += 2;
        }
        ys.push(py);
    }
    let mut start = 0usize;
    for &end in ends.iter() {
        let end = end.min(npts.saturating_sub(1));
        let mut c = Contour::default();
        for k in start..=end {
            let p = Pt { x: xs[k], y: ys[k], on: flags[k] & 1 != 0 };
            c.pts.push(x.apply(p));
        }
        start = end + 1;
        if !c.pts.is_empty() {
            out.push(c);
        }
    }
    Ok(())
}

pub fn decode_utf16be(raw: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(raw.len() / 2);
    let mut i = 0usize;
    while i + 1 < raw.len() {
        units.push(u16::from_be_bytes([raw[i], raw[i + 1]]));
        i += 2;
    }
    String::from_utf16_lossy(&units)
}

/// Mac Roman for the `name` table: the printable high bytes that actually show up
/// in font names; anything else falls back to Latin-1 so names stay readable.
fn decode_mac_roman(raw: &[u8]) -> String {
    const HI: &[(u8, char)] = &[
        (0x80, '\u{00c4}'), (0x81, '\u{00c5}'), (0x82, '\u{00c7}'), (0x83, '\u{00c9}'),
        (0x84, '\u{00d1}'), (0x85, '\u{00d6}'), (0x86, '\u{00dc}'), (0x87, '\u{00e1}'),
        (0x88, '\u{00e0}'), (0x89, '\u{00e2}'), (0x8a, '\u{00e4}'), (0x8b, '\u{00e3}'),
        (0x8c, '\u{00e5}'), (0x8d, '\u{00e7}'), (0x8e, '\u{00e9}'), (0x8f, '\u{00e8}'),
        (0x90, '\u{00ea}'), (0x91, '\u{00eb}'), (0x92, '\u{00ec}'), (0x93, '\u{00ed}'),
        (0x94, '\u{00ee}'), (0x95, '\u{00ef}'), (0x96, '\u{00f1}'), (0x97, '\u{00f2}'),
        (0x98, '\u{00f3}'), (0x99, '\u{00f4}'), (0x9a, '\u{00f6}'), (0x9b, '\u{00f5}'),
        (0x9c, '\u{00fa}'), (0x9d, '\u{00fb}'), (0x9e, '\u{00fc}'), (0x9f, '\u{00bd}'),
        (0xa0, '\u{2018}'), (0xa1, '\u{2019}'), (0xa2, '\u{201c}'), (0xa3, '\u{201d}'),
        (0xa4, '\u{2022}'), (0xa5, '\u{2013}'), (0xa6, '\u{2014}'), (0xa8, '\u{00a9}'),
        (0xa9, '\u{2122}'), (0xaa, '\u{00ae}'), (0xc0, '\u{00c0}'), (0xc1, '\u{00c3}'),
        (0xc2, '\u{00d5}'), (0xc3, '\u{0152}'), (0xc4, '\u{0153}'), (0xc7, '\u{00a1}'),
        (0xc8, '\u{00bf}'), (0xca, '\u{00a4}'), (0xcb, '\u{00a4}'), (0xcc, '\u{00ac}'),
        (0xcd, '\u{00bd}'), (0xd0, '\u{201c}'), (0xd1, '\u{201d}'), (0xd8, '\u{00a8}'),
        (0xd9, '\u{00a8}'), (0xda, '\u{00d7}'), (0xdb, '\u{00d7}'), (0xdc, '\u{00f7}'),
        (0xdd, '\u{00f7}'), (0xde, '\u{00a1}'), (0xdf, '\u{2020}'), (0xe0, '\u{00b0}'),
        (0xe1, '\u{00a2}'), (0xe2, '\u{00a3}'), (0xe5, '\u{00b6}'), (0xe7, '\u{00ab}'),
        (0xe8, '\u{00bb}'), (0xf8, '\u{00f7}'), (0xfb, '\u{00a7}'), (0xfc, '\u{00f1}'),
    ];
    raw.iter()
        .map(|&b| {
            if b < 0x80 {
                return b as char;
            }
            match HI.iter().find(|(k, _)| *k == b) {
                Some((_, c)) => *c,
                None => b as char,
            }
        })
        .collect()
}

/// `wOFF` 1.0 container: rebuild an sfnt image from the (optionally zlib'd) tables.
fn unpack_woff(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 44 {
        return Err("woff: truncated header".to_string());
    }
    let num = u16at(bytes, 12) as usize;
    if num == 0 || 44 + 20 * num > bytes.len() {
        return Err("woff: truncated directory".to_string());
    }
    struct E {
        t: [u8; 4],
        off: usize,
        comp: usize,
        orig: usize,
        cs: u32,
    }
    let mut entries = Vec::with_capacity(num);
    for i in 0..num {
        let base = 44 + 20 * i;
        let e = E {
            t: [
                bytes[base],
                bytes[base + 1],
                bytes[base + 2],
                bytes[base + 3],
            ],
            off: u32at(bytes, base + 4) as usize,
            comp: u32at(bytes, base + 8) as usize,
            orig: u32at(bytes, base + 12) as usize,
            cs: u32at(bytes, base + 16),
        };
        if e.off.checked_add(e.comp).map(|x| x > bytes.len()).unwrap_or(true) {
            return Err("woff: table beyond end of file".to_string());
        }
        entries.push(e);
    }
    let search_range = 16 * (1 << (num.max(1) as f64).log2() as u32);
    let entry_sel = (num.max(1) as f64).log2() as u32;
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 16 * num);
    out.extend_from_slice(&u32::to_be_bytes(0x0001_0000));
    out.extend_from_slice(&u16::to_be_bytes(num as u16));
    out.extend_from_slice(&u16::to_be_bytes(search_range.min(0xFFFF) as u16));
    out.extend_from_slice(&u16::to_be_bytes(entry_sel.min(0xFFFF) as u16));
    out.extend_from_slice(&u16::to_be_bytes(
        (num * 16).saturating_sub(search_range as usize) as u16,
    ));
    let mut data_off = 12 + 16 * num;
    for e in entries.iter() {
        let padded = (e.orig + 3) & !3;
        out.extend_from_slice(&e.t);
        out.extend_from_slice(&u32::to_be_bytes(e.cs));
        out.extend_from_slice(&u32::to_be_bytes(data_off as u32));
        out.extend_from_slice(&u32::to_be_bytes(e.orig as u32));
        data_off += padded;
    }
    for e in entries.iter() {
        let raw = &bytes[e.off..e.off + e.comp];
        let table = if e.comp == e.orig {
            raw.to_vec()
        } else {
            crate::codec::inflate::inflate_zlib(raw).map_err(|e| format!("woff inflate: {e}"))?
        };
        out.extend_from_slice(&table);
        let pad = ((table.len() + 3) & !3) - table.len();
        out.resize(out.len() + pad, 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Option<Vec<u8>> {
        let mut p = String::from(env!("CARGO_MANIFEST_DIR"));
        p.push_str("/tests/data/");
        p.push_str(name);
        std::fs::read(&p).ok()
    }

    #[test]
    fn parses_mini_ttf_metrics_and_cmap() {
        let bytes = match fixture("kilat-mini.ttf") {
            Some(b) => b,
            None => return,
        };
        let f = Face::parse(bytes).expect("parse mini ttf");
        assert_eq!(f.units_per_em, 1000);
        assert_eq!(f.ascender, 800);
        assert_eq!(f.descender, -200);
        assert_eq!(f.line_gap, 0);
        assert_eq!(f.weight, 400);
        assert_eq!(f.x_height, 500);
        assert_eq!(f.cap_height, 700);
        assert_eq!(f.family, "Kilat Mini");
        assert_eq!(f.subfamily, "Regular");
        assert_eq!(f.num_glyphs, 4);
        assert!(!f.italic);
        assert!(!f.fixed_pitch);
        assert!(!f.is_cff);
        assert!(f.has_outlines);
        assert_eq!(f.glyph_for('A'), Some(2));
        assert_eq!(f.glyph_for('a'), Some(2));
        assert_eq!(f.glyph_for('V'), Some(3));
        assert_eq!(f.glyph_for(' '), Some(1));
        assert_eq!(f.glyph_for('.'), None, "mapped to .notdef, treated as absent");
        assert_eq!(f.advance_units(2), 720);
        assert_eq!(f.advance_units(1), 250);
        assert_eq!(f.kern_units(2, 3), -80);
        assert_eq!(f.scale(20.0), 0.02);
        assert_eq!(f.ascent(20.0), 16.0);
        assert_eq!(f.descent(20.0), 4.0);
        assert_eq!(f.cap_height_px(20.0), 14.0);
        assert_eq!(f.measure("A", 20.0, false), 14.4);
        assert_eq!(f.measure("AV", 20.0, false), 28.8);
        assert_eq!(f.measure("AV", 20.0, true), 27.2);
    }

    #[test]
    fn reads_outlines_and_bbox() {
        let bytes = match fixture("kilat-mini.ttf") {
            Some(b) => b,
            None => return,
        };
        let f = Face::parse(bytes).expect("parse mini ttf");
        let c = f.outline(0).expect("notdef outline");
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0].pts,
            vec![
                Pt { x: 50.0, y: 0.0, on: true },
                Pt { x: 550.0, y: 0.0, on: true },
                Pt { x: 550.0, y: 700.0, on: true },
                Pt { x: 50.0, y: 700.0, on: true },
            ]
        );
        let a = f.outline(2).expect("A outline");
        assert_eq!(a[0].pts.len(), 6);
        assert_eq!(f.bbox(2), Some(Bbox { x_min: 0.0, y_min: 0.0, x_max: 720.0, y_max: 700.0 }));
        assert_eq!(f.bbox(1), None, "space has no outline");
        assert_eq!(f.outline(1).map(|v| v.len()), Ok(0));
    }

    #[test]
    fn reads_woff_wrapper() {
        let bytes = match fixture("kilat-mini.woff") {
            Some(b) => b,
            None => return,
        };
        let f = Face::parse(bytes).expect("parse mini woff");
        assert_eq!(f.family, "Kilat Mini");
        assert_eq!(f.units_per_em, 1000);
        assert_eq!(f.advance_units(2), 720);
        assert_eq!(f.glyph_for('A'), Some(2));
        assert_eq!(f.kern_units(2, 3), -80);
        assert_eq!(f.outline(2).map(|c| c[0].pts.len()), Ok(6));
    }

    #[test]
    fn truncated_fonts_never_panic() {
        let bytes = match fixture("kilat-mini.ttf") {
            Some(b) => b,
            None => return,
        };
        for cut in [8usize, 40, 90, 200, 400, 700] {
            let mut v = bytes.clone();
            v.truncate(cut);
            let r = Face::parse(v);
            if let Ok(f) = r {
                // Whatever survived must still answer queries without panicking.
                let _ = f.measure("AV", 16.0, true);
                let _ = f.outline(2);
                let _ = f.bbox(3);
            }
        }
        assert!(Face::parse(b"junk".to_vec()).is_err());
    }

    #[test]
    fn utf16_and_mac_roman_names() {
        assert_eq!(decode_utf16be(&[0x00, b'A', 0x00, b'B']), "AB");
        assert_eq!(decode_mac_roman(b"DejaVu Sans"), "DejaVu Sans");
        assert_eq!(decode_mac_roman(&[0x8e]), "\u{00e9}");
    }
}
