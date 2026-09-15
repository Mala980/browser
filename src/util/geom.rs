//! Geometry primitives shared by CSS, layout and painting.

use crate::util::{clamp_f32, Result};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Vec2 {
        Vec2 { x, y }
    }
}

/// A box in CSS pixel space. `w`/`h` are always >= 0 in practice; `is_empty`
/// guards the rasteriser.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn zero() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        }
    }

    pub fn from_ltrb(l: f32, t: f32, r: f32, b: f32) -> Rect {
        Rect {
            x: l,
            y: t,
            w: (r - l).max(0.0),
            h: (b - t).max(0.0),
        }
    }

    pub fn left(&self) -> f32 {
        self.x
    }
    pub fn top(&self) -> f32 {
        self.y
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.right() && py < self.bottom()
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && o.x < self.right()
            && o.right() > self.x
            && o.y < self.bottom()
            && o.bottom() > self.y
    }

    pub fn intersection(&self, o: &Rect) -> Option<Rect> {
        if !self.intersects(o) {
            return None;
        }
        let l = self.x.max(o.x);
        let t = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        if r <= l || b <= t {
            None
        } else {
            Some(Rect::from_ltrb(l, t, r, b))
        }
    }

    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Rect::from_ltrb(
            self.x.min(o.x),
            self.y.min(o.y),
            self.right().max(o.right()),
            self.bottom().max(o.bottom()),
        )
    }

    pub fn inflate(&self, dx: f32, dy: f32) -> Rect {
        Rect {
            x: self.x - dx,
            y: self.y - dy,
            w: self.w + dx * 2.0,
            h: self.h + dy * 2.0,
        }
    }

    pub fn offset(&self, dx: f32, dy: f32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            w: self.w,
            h: self.h,
        }
    }

    /// Snapped outward to whole device pixels - used for clipping so antialiased
    /// edges of neighbouring boxes never leave hairline seams.
    pub fn snapped_out(&self, scale: f32) -> Rect {
        let s = if scale <= 0.0 { 1.0 } else { scale };
        Rect::from_ltrb(
            (self.x * s).floor() / s,
            (self.y * s).floor() / s,
            (self.right() * s).ceil() / s,
            (self.bottom() * s).ceil() / s,
        )
    }

    /// Integer pixel bounds, clipped to a surface; `None` when fully outside.
    pub fn pixel_bounds(&self, surf_w: u32, surf_h: u32) -> Option<(u32, u32, u32, u32)> {
        if self.w <= 0.0 || self.h <= 0.0 {
            return None;
        }
        let l = self.x.max(0.0).floor() as i64;
        let t = self.y.max(0.0).floor() as i64;
        let r = self.right().min(surf_w as f32).ceil() as i64;
        let b = self.bottom().min(surf_h as f32).ceil() as i64;
        if r <= l || b <= t {
            return None;
        }
        Some((l as u32, t as u32, (r - l) as u32, (b - t) as u32))
    }

    pub fn json(&self) -> crate::util::Json {
        let mut o = Vec::<(&str, crate::util::Json)>::new();
        o.push(("x", crate::util::Json::Num(self.x as f64)));
        o.push(("y", crate::util::Json::Num(self.y as f64)));
        o.push(("width", crate::util::Json::Num(self.w as f64)));
        o.push(("height", crate::util::Json::Num(self.h as f64)));
        crate::util::Json::object(o)
    }
}

/// A CSS color, straight 8-bit RGBA. `a == 0` means fully transparent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }

    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    pub const BLACK: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    pub fn is_transparent(&self) -> bool {
        self.a == 0
    }

    pub fn with_alpha(&self, a: f32) -> Color {
        Color {
            r: self.r,
            g: self.g,
            b: self.b,
            a: (clamp_f32(a, 0.0, 1.0) * 255.0).round() as u8,
        }
    }

    /// Source-over compositing onto an existing framebuffer pixel.
    pub fn over(&self, dst: Color) -> Color {
        if self.a == 255 {
            return *self;
        }
        if self.a == 0 {
            return dst;
        }
        let sa = self.a as f32 / 255.0;
        let da = dst.a as f32 / 255.0;
        let oa = sa + da * (1.0 - sa);
        if oa <= 0.0 {
            return Color::TRANSPARENT;
        }
        let ch = |sc: u8, dc: u8| -> u8 {
            let v = (sc as f32 * sa + dc as f32 * da * (1.0 - sa)) / oa;
            clamp_f32(v, 0.0, 255.0).round() as u8
        };
        Color {
            r: ch(self.r, dst.r),
            g: ch(self.g, dst.g),
            b: ch(self.b, dst.b),
            a: (oa * 255.0).round() as u8,
        }
    }

    pub fn mix(&self, other: &Color, t: f32) -> Color {
        let t = clamp_f32(t, 0.0, 1.0);
        let c = |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * t).round() as u8 };
        Color {
            r: c(self.r, other.r),
            g: c(self.g, other.g),
            b: c(self.b, other.b),
            a: c(self.a, other.a),
        }
    }

    pub fn to_hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    pub fn to_css(&self) -> String {
        if self.a == 255 {
            self.to_hex()
        } else {
            format!(
                "rgba({}, {}, {}, {})",
                self.r,
                self.g,
                self.b,
                (self.a as f32 / 255.0 * 100.0).round() / 100.0
            )
        }
    }

    /// Parse a CSS color: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()/rgba()`,
    /// `hsl()/hsla()`, `transparent`, `currentColor` (-> black default) and the
    /// CSS color keywords we ship.
    pub fn parse(text: &str) -> Option<Color> {
        let t = text.trim();
        if t.is_empty() {
            return None;
        }
        if t.as_bytes()[0] == b'#' {
            return Color::parse_hex(&t[1..]);
        }
        let lower = t.to_ascii_lowercase();
        if lower == "transparent" {
            return Some(Color::TRANSPARENT);
        }
        if lower == "currentcolor" {
            return Some(Color::BLACK);
        }
        if let Some(c) = lookup_keyword(&lower) {
            return Some(c);
        }
        let (fname, args) = match lower.find('(') {
            Some(i) => (&lower[..i], lower[i + 1..lower.rfind(')')?].to_string()),
            None => return None,
        };
        let parts: Vec<&str> = if args.contains('/') && !args.contains(',') {
            // Modern space separated syntax: rgb(0 0 0 / 50%)
            let (cols, alpha) = match args.split_once('/') {
                Some((c, a)) => (c.trim(), Some(a.trim())),
                None => (args.trim(), None),
            };
            let mut v: Vec<&str> = cols.split_whitespace().collect();
            if let Some(a) = alpha {
                v.push(a);
            }
            v
        } else {
            args.split(',').map(|s| s.trim()).collect()
        };
        match fname.trim() {
            "rgb" | "rgba" => {
                if parts.len() < 3 {
                    return None;
                }
                let r = chan8(parts[0])?;
                let g = chan8(parts[1])?;
                let b = chan8(parts[2])?;
                let a = if parts.len() > 3 { chan_a(parts[3])? } else { 255 };
                Some(Color::rgba(r, g, b, a))
            }
            "hsl" | "hsla" => {
                if parts.len() < 3 {
                    return None;
                }
                let h = parse_angle(parts[0])?;
                let s = parse_percent(parts[1])?;
                let l = parse_percent(parts[2])?;
                let a = if parts.len() > 3 { chan_a(parts[3])? } else { 255 };
                Some(hsl_to_rgb(h, s, l, a))
            }
            _ => None,
        }
    }

    pub fn parse_hex(hex: &str) -> Option<Color> {
        let b = hex.as_bytes();
        let nib = |c: u8| -> Option<u8> {
            match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None,
            }
        };
        let pair = |i: usize| -> Option<u8> {
            Some(nib(b[i])? << 4 | nib(b[i + 1])?)
        };
        match b.len() {
            3 => Some(Color::rgb(
                nib(b[0])? << 4 | nib(b[0])?,
                nib(b[1])? << 4 | nib(b[1])?,
                nib(b[2])? << 4 | nib(b[2])?,
            )),
            4 => Some(Color::rgba(
                nib(b[0])? << 4 | nib(b[0])?,
                nib(b[1])? << 4 | nib(b[1])?,
                nib(b[2])? << 4 | nib(b[2])?,
                nib(b[3])? << 4 | nib(b[3])?,
            )),
            6 => Some(Color::rgb(pair(0)?, pair(2)?, pair(4)?)),
            8 => Some(Color::rgba(pair(0)?, pair(2)?, pair(4)?, pair(6)?)),
            _ => None,
        }
    }
}

fn chan8(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        let v: f32 = p.trim().parse().ok()?;
        return Some(clamp_f32(v / 100.0 * 255.0, 0.0, 255.0).round() as u8);
    }
    let v: f32 = s.parse().ok()?;
    Some(clamp_f32(v, 0.0, 255.0).round() as u8)
}

fn chan_a(s: &str) -> Option<u8> {
    let t = s.trim();
    if let Some(p) = t.strip_suffix('%') {
        let v: f32 = p.trim().parse().ok()?;
        return Some((clamp_f32(v, 0.0, 100.0) * 2.55).round() as u8);
    }
    let v: f32 = t.parse().ok()?;
    let a = if v > 1.0 {
        clamp_f32(v, 0.0, 255.0)
    } else {
        clamp_f32(v, 0.0, 1.0) * 255.0
    };
    Some(a.round() as u8)
}

fn parse_angle(s: &str) -> Option<f32> {
    let s = s.trim();
    let t = s
        .strip_suffix("deg")
        .or_else(|| s.strip_suffix("rad").map(|v| v))
        .unwrap_or(s);
    let mut v: f32 = t.trim().parse().ok()?;
    if s.ends_with("rad") {
        v = v * 180.0 / std::f32::consts::PI;
    }
    Some(v.rem_euclid(360.0))
}

fn parse_percent(s: &str) -> Option<f32> {
    let v: f32 = s.trim().trim_end_matches('%').trim().parse().ok()?;
    Some(clamp_f32(v, 0.0, 100.0))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32, a: u8) -> Color {
    let s = s / 100.0;
    let l = l / 100.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match (h / 60.0).floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let q = |v: f32| -> u8 { (clamp_f32(v + m, 0.0, 1.0) * 255.0).round() as u8 };
    Color::rgba(q(r1), q(g1), q(b1), a)
}

/// CSS keyword subset. Kept as a sorted table so lookup is a binary search.
fn lookup_keyword(name: &str) -> Option<Color> {
    // Linear scan: the table is small and this keeps us free of a fragile
    // "must stay alphabetically sorted" invariant.
    for (k, hex) in CSS_COLORS.iter() {
        if *k == name {
            return Color::parse_hex(hex);
        }
    }
    None
}

const CSS_COLORS: &[(&str, &str)] = &[
    ("aliceblue", "f0f8ff"),
    ("antiquewhite", "faebd7"),
    ("aqua", "00ffff"),
    ("aquamarine", "7fffd4"),
    ("azure", "f0ffff"),
    ("beige", "f5f5dc"),
    ("bisque", "ffe4c4"),
    ("black", "000000"),
    ("blanchedalmond", "ffebcd"),
    ("blue", "0000ff"),
    ("blueviolet", "8a2be2"),
    ("brown", "a52a2a"),
    ("burlywood", "deb887"),
    ("cadetblue", "5f9ea0"),
    ("chartreuse", "7fff00"),
    ("chocolate", "d2691e"),
    ("coral", "ff7f50"),
    ("cornflowerblue", "6495ed"),
    ("cornsilk", "fff8dc"),
    ("crimson", "dc143c"),
    ("cyan", "00ffff"),
    ("darkblue", "00008b"),
    ("darkcyan", "008b8b"),
    ("darkgoldenrod", "b8860b"),
    ("darkgray", "a9a9a9"),
    ("darkgreen", "006400"),
    ("darkgrey", "a9a9a9"),
    ("darkkhaki", "bdb76b"),
    ("darkmagenta", "8b008b"),
    ("darkolivegreen", "556b2f"),
    ("darkorange", "ff8c00"),
    ("darkorchid", "9932cc"),
    ("darkred", "8b0000"),
    ("darksalmon", "e9967a"),
    ("darkseagreen", "8fbc8f"),
    ("darkslateblue", "483d8b"),
    ("darkslategray", "2f4f4f"),
    ("darkturquoise", "00ced1"),
    ("darkviolet", "9400d3"),
    ("deeppink", "ff1493"),
    ("deepskyblue", "00bfff"),
    ("dimgray", "696969"),
    ("dimgrey", "696969"),
    ("dodgerblue", "1e90ff"),
    ("firebrick", "b22222"),
    ("floralwhite", "fffaf0"),
    ("forestgreen", "228b22"),
    ("fuchsia", "ff00ff"),
    ("gainsboro", "dcdcdc"),
    ("ghostwhite", "f8f8ff"),
    ("gold", "ffd700"),
    ("goldenrod", "daa520"),
    ("gray", "808080"),
    ("green", "008000"),
    ("greenyellow", "adff2f"),
    ("grey", "808080"),
    ("honeydew", "f0fff0"),
    ("hotpink", "ff69b4"),
    ("indianred", "cd5c5c"),
    ("indigo", "4b0082"),
    ("ivory", "fffff0"),
    ("khaki", "f0e68c"),
    ("lavender", "e6e6fa"),
    ("lawngreen", "7cfc00"),
    ("lemonchiffon", "fffacd"),
    ("lightblue", "add8e6"),
    ("lightcoral", "f08080"),
    ("lightcyan", "e0ffff"),
    ("lime", "00ff00"),
    ("magenta", "ff00ff"),
    ("maroon", "800000"),
    ("navy", "000080"),
    ("olive", "808000"),
    ("orange", "ffa500"),
    ("orangered", "ff4500"),
    ("orchid", "da70d6"),
    ("pink", "ffc0cb"),
    ("plum", "dda0dd"),
    ("purple", "800080"),
    ("red", "ff0000"),
    ("royalblue", "4169e1"),
    ("salmon", "fa8072"),
    ("silver", "c0c0c0"),
    ("teal", "008080"),
    ("white", "ffffff"),
    ("yellow", "ffff00"),
];

/// 4 edges in CSS order: top, right, bottom, left.
pub const EDGE_TOP: usize = 0;
pub const EDGE_RIGHT: usize = 1;
pub const EDGE_BOTTOM: usize = 2;
pub const EDGE_LEFT: usize = 3;

pub fn expand4(top: f32, right: f32, bottom: f32, left: f32) -> [f32; 4] {
    [top, right, bottom, left]
}

pub fn assert_rect(r: &Rect) -> Result<()> {
    if r.w.is_nan() || r.h.is_nan() {
        Err("NaN rect".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(Color::parse("#fff").unwrap(), Color::rgb(255, 255, 255));
        assert_eq!(Color::parse("#F00").unwrap(), Color::rgb(255, 0, 0));
        assert_eq!(
            Color::parse("rgba(0, 128, 255, 0.5)").unwrap(),
            Color::rgba(0, 128, 255, 128)
        );
        assert_eq!(Color::parse("rgb(0 128 255)").unwrap(), Color::rgb(0, 128, 255));
        assert_eq!(Color::parse("rebeccapurple"), None); // not in our subset
        assert_eq!(Color::parse("red").unwrap(), Color::rgb(255, 0, 0));
        assert_eq!(
            Color::parse("hsl(0, 100%, 50%)").unwrap(),
            Color::rgb(255, 0, 0)
        );
        assert_eq!(
            Color::parse("hsl(120, 100%, 50%)").unwrap(),
            Color::rgb(0, 255, 0)
        );
        assert_eq!(Color::parse("transparent").unwrap().a, 0);
        assert_eq!(Color::parse("#12345"), None);
        assert_eq!(Color::parse("#12"), None);
    }

    #[test]
    fn blend_and_mix() {
        let over = Color::rgba(0, 0, 0, 128).over(Color::rgb(255, 255, 255));
        assert!(over.r > 120 && over.r < 135);
        let m = Color::WHITE.mix(&Color::BLACK, 0.25);
        assert_eq!(m.r, 191);
    }

    #[test]
    fn rects() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.intersection(&b), Some(Rect::new(5.0, 5.0, 5.0, 5.0)));
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 15.0, 15.0));
        assert!(!a.intersects(&Rect::new(20.0, 20.0, 5.0, 5.0)));
        assert_eq!(a.pixel_bounds(8, 8), Some((0, 0, 8, 8)));
        assert_eq!(Rect::new(-4.0, -4.0, 2.0, 2.0).pixel_bounds(8, 8), None);
        assert!(a.contains(1.0, 1.0));
        assert!(!a.contains(10.0, 1.0));
    }
}
