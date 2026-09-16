//! Scanline rasterizer for TrueType outlines.
//!
//! Outlines are flattened to polylines in device space and filled with the
//! nonzero winding rule (what TrueType specifies), with each pixel row sampled at
//! `SUB` sub-rows and spans added analytically across x. That is 4x the cost of a
//! hard-edged fill and about as smooth as small text gets without a hinting
//! engine, which suits a browser that has to look right on a phone at 12-16px.
//!
//! Masks are cached by the painter, so this runs once per (glyph, size, style).

use super::tt::{Contour, Pt};

/// Vertical sub-samples per pixel row.
const SUB: usize = 4;

/// One glyph's coverage bitmap, in the coordinate system the painter wants:
/// origin at the pen position, y down, `w x h` bytes of alpha.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub cov: Vec<u8>,
    /// Pen advance for this glyph at this size, in pixels.
    pub advance: f32,
    /// Distance from the pen origin to the left edge of the bitmap (can be
    /// negative for overhanging glyphs like italics).
    pub left: f32,
    /// Distance from the baseline up to the top of the bitmap.
    pub top: f32,
}

impl Mask {
    pub fn at(&self, x: usize, y: usize) -> u8 {
        *self.cov.get(y * self.w + x).unwrap_or(&0)
    }
}

/// Flatten quadratics into closed polylines in device space.
///
/// `scale` maps font units to pixels; the pen sits at `(ox, oy)` (baseline left)
/// and font y is up, so device y is `oy - fy * scale`.
fn flatten(contours: &[Contour], scale: f32, ox: f32, oy: f32, out: &mut Vec<Vec<(f32, f32)>>) {
    let dev = |p: &Pt| (ox + p.x * scale, oy - p.y * scale);
    for c in contours {
        let n = c.pts.len();
        if n < 3 {
            continue;
        }
        // Implied on-curve points: a contour of only control points (a circle)
        // starts each segment at the midpoint of the pair.
        let mut pts: Vec<Pt> = Vec::with_capacity(n * 2);
        if c.pts.iter().all(|p| !p.on) {
            for i in 0..n {
                let a = &c.pts[i];
                let b = &c.pts[(i + 1) % n];
                pts.push(Pt {
                    x: (a.x + b.x) * 0.5,
                    y: (a.y + b.y) * 0.5,
                    on: true,
                });
                pts.push(*b);
            }
        } else {
            pts.extend_from_slice(&c.pts);
        }
        // Rotate so the walk begins on an on-curve point.
        let m = pts.len();
        let start = (0..m).find(|&i| pts[i].on).unwrap_or(0);
        let mut p2: Vec<Pt> = Vec::with_capacity(m);
        p2.extend_from_slice(&pts[start..]);
        p2.extend_from_slice(&pts[..start]);
        // Trailing off-curve points belong to the segment that wraps to the start.
        while p2.last().map(|p| !p.on).unwrap_or(false) && p2.len() > 1 {
            let tail = p2.pop().expect("checked");
            p2.insert(0, tail);
        }
        let m = p2.len();
        if m < 3 {
            continue;
        }
        let mut poly: Vec<(f32, f32)> = Vec::with_capacity(m * 4);
        poly.push(dev(&p2[0]));
        let mut i = 1usize;
        while i < m {
            if p2[i].on {
                poly.push(dev(&p2[i]));
                i += 1;
                continue;
            }
            let ctrl = p2[i];
            let (end, next) = if i + 1 >= m {
                (p2[0], m) // closes back onto the start point
            } else if p2[i + 1].on {
                (p2[i + 1], i + 1)
            } else {
                // Two control points in a row: the on-curve point between them is
                // implied at their midpoint.
                (
                    Pt {
                        x: (ctrl.x + p2[i + 1].x) * 0.5,
                        y: (ctrl.y + p2[i + 1].y) * 0.5,
                        on: true,
                    },
                    i + 1,
                )
            };
            curve(poly, *poly.last().unwrap_or(&(0.0, 0.0)), dev(&ctrl), dev(&end), 0.25, 0);
            if next >= m {
                break;
            }
            i = next + 1;
        }
        if poly.len() >= 3 {
            out.push(poly);
        }
    }
}

/// Adaptive quadratic subdivision: split while the curve midpoint sits further
/// than `tol` from the chord.
fn curve(poly: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), tol: f32, depth: u32) {
    if depth < 8 {
        let lerp = |a: (f32, f32), b: (f32, f32), t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let q0 = lerp(p0, p1, 0.5);
        let q1 = lerp(p1, p2, 0.5);
        let mid = lerp(q0, q1, 0.5);
        let cx = (p0.0 + p2.0) * 0.5;
        let cy = (p0.1 + p2.1) * 0.5;
        if (mid.0 - cx).abs().max((mid.1 - cy).abs()) > tol {
            curve(poly, p0, q0, mid, tol, depth + 1);
            curve(poly, mid, q1, p2, tol, depth + 1);
            return;
        }
    }
    poly.push(p2);
}

/// Rasterise `contours` into a coverage bitmap.
///
/// `bounds` is the device-space box to fill (usually the glyph bounding box plus
/// one pixel of margin for the antialiasing).
pub fn fill_span(
    contours: &[Contour],
    scale: f32,
    ox: f32,
    oy: f32,
    x0: f32,
    y0: f32,
    w: usize,
    h: usize,
) -> Vec<u8> {
    let mut polys: Vec<Vec<(f32, f32)>> = Vec::new();
    flatten(contours, scale, ox, oy, &mut polys);
    let mut cov = vec![0u16; w * h];
    for row in 0..h {
        // Accumulate this row's crossings over all sub-rows, then convert.
        let mut events: Vec<(f32, i32)> = Vec::new();
        for s in 0..SUB {
            let y = y0 + row as f32 + (s as f32 + 0.5) / SUB as f32;
            events.clear();
            for poly in &polys {
                let n = poly.len();
                if n < 2 {
                    continue;
                }
                for i in 0..n {
                    let a = poly[i];
                    let b = poly[(i + 1) % n];
                    if (a.1 <= y && b.1 <= y) || (a.1 > y && b.1 > y) {
                        continue;
                    }
                    let dir = if b.1 > a.1 { 1 } else { -1 };
                    let t = (y - a.1) / (b.1 - a.1);
                    let x = a.0 + t * (b.0 - a.0);
                    events.push((x, dir));
                }
            }
            if events.is_empty() {
                continue;
            }
            events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            // Nonzero winding: ink between the crossings where the winding number
            // is not zero. Each sub-row contributes 256/SUB of the pixel.
            let weight = (256 / SUB) as u16;
            let mut winding = 0i32;
            for i in 0..events.len() {
                winding += events[i].1;
                if i + 1 >= events.len() || winding == 0 {
                    continue;
                }
                let a = events[i].0 - x0;
                let b = events[i + 1].0 - x0;
                if b <= a {
                    continue;
                }
                let pa = a.floor() as i64;
                let pb = b.floor() as i64;
                if pa == pb {
                    if pa >= 0 && (pa as usize) < w {
                        cov[pa as usize + row * w] += ((b - a) * weight as f32).round() as u16;
                    }
                    continue;
                }
                if pa >= 0 && (pa as usize) < w {
                    cov[pa as usize + row * w] += ((pa as f32 + 1.0 - a) * weight as f32).round() as u16;
                }
                for k in (pa + 1).max(0)..pb.min(w as i64) {
                    cov[k as usize + row * w] += weight;
                }
                if pb >= 0 && (pb as usize) < w {
                    cov[pb as usize + row * w] += ((b - pb as f32) * weight as f32).round() as u16;
                }
            }
        }
    }
    cov.into_iter().map(|v| v.min(255) as u8).collect()
}

/// Rasterise one glyph at the given scale.
///
/// `bbox` is the `glyf` bounding box, which is already relative to the pen
/// position (that is what TrueType guarantees, and why `lsb` is not applied
/// here); pass the tight box or None for an empty glyph.
pub fn glyph_mask(
    contour_list: &[Contour],
    advance_units: f32,
    bbox: Option<(f32, f32, f32, f32)>,
    scale: f32,
) -> Mask {
    let advance = advance_units * scale;
    let (xmin, ymin, xmax, ymax) = match bbox {
        Some(b) => b,
        None => {
            return Mask {
                w: 0,
                h: 0,
                cov: Vec::new(),
                advance,
                left: 0.0,
                top: 0.0,
            }
        }
    };
    // Device box: x from lsb + xmin*scale, and y up from the baseline by
    // ymax*scale (device y is negative above the baseline).
    let dev_x0 = xmin * scale;
    let dev_x1 = xmax * scale;
    let dev_top = ymax * scale;
    let dev_bot = ymin * scale;
    let left = dev_x0.floor() - 1.0;
    let right = dev_x1.ceil() + 1.0;
    let top = dev_top.ceil() + 1.0;
    let bottom = (-dev_bot).ceil() + 1.0;
    let w = ((right - left).ceil() as usize).max(0).min(1024);
    let h = ((top + bottom) as usize).max(0).min(1024);
    if w == 0 || h == 0 || contour_list.is_empty() {
        return Mask {
            w: 0,
            h: 0,
            cov: Vec::new(),
            advance,
            left,
            top,
        };
    }
    let x0 = left;
    let y0 = -top;
    let cov = fill_span(contour_list, scale, 0.0, 0.0, x0, y0, w, h);
    Mask {
        w,
        h,
        cov,
        advance,
        left,
        top,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Contour> {
        vec![Contour {
            pts: vec![
                Pt { x: x0, y: y0, on: true },
                Pt { x: x1, y: y0, on: true },
                Pt { x: x1, y: y1, on: true },
                Pt { x: x0, y: y1, on: true },
            ],
        }]
    }

    #[test]
    fn filled_rectangle_has_hard_edges_and_correct_size() {
        // Font units 0..1000 scaled to 20px: a box from 50..550 x 0..700 becomes
        // 1..11 wide and 14 tall above the baseline, plus a 1px margin all round.
        let m = glyph_mask(&rect(50.0, 0.0, 550.0, 700.0), 600.0, Some((50.0, 0.0, 550.0, 700.0)), 0.02);
        assert!((m.advance - 12.0).abs() < 0.01);
        assert!(m.w >= 10 && m.w <= 14, "w={}", m.w);
        assert!(m.h >= 13 && m.h <= 17, "h={}", m.h);
        // Interior must be solid, outside must be empty.
        let mid = (m.w / 2, m.h / 2);
        assert_eq!(m.at(mid.0, mid.1), 255, "interior should be full coverage");
        assert_eq!(m.at(0, 0), 0, "corner above the box must be empty");
        let mut rows_with_ink = 0usize;
        for y in 0..m.h {
            if (0..m.w).any(|x| m.at(x, y) > 0) {
                rows_with_ink += 1;
            }
        }
        assert!(rows_with_ink >= 12, "expected ~14 inked rows, got {rows_with_ink}");
    }

    #[test]
    fn winding_rule_punches_a_hole() {
        // Outer clockwise + inner same-direction: nonzero winding fills both,
        // opposite directions make a hole. TrueType marks counter-clockwise
        // contours as holes, so test that: reverse the inner contour.
        let mut outer = rect(0.0, 0.0, 100.0, 100.0);
        let mut inner = rect(30.0, 30.0, 70.0, 70.0);
        inner[0].pts.reverse();
        outer.extend(inner);
        let m = glyph_mask(&outer, 100.0, Some((0.0, 0.0, 100.0, 100.0)), 0.1);
        let centre = (m.w / 2, m.h / 2);
        assert_eq!(m.at(centre.0, centre.1), 0, "the hole must be empty");
        let edge = (1, centre.1);
        assert!(m.at(edge.0, edge.1) > 200, "the ring must be inked");
    }

    #[test]
    fn quadratic_is_rounded_not_boxy() {
        // A circle-ish contour made of four off-curve control points.
        let c = vec![Contour {
            pts: (0..4)
                .map(|i| {
                    let a = std::f32::consts::PI * 2.0 * (i as f32 + 0.5) / 4.0;
                    Pt {
                        x: 50.0 + 80.0 * a.cos(),
                        y: 50.0 + 80.0 * a.sin(),
                        on: false,
                    }
                })
                .collect(),
        }];
        let m = glyph_mask(&c, 100.0, Some((0.0, 0.0, 100.0, 100.0)), 0.2);
        assert!(m.w > 20 && m.h > 20);
        // Corners empty, centre inked: that only holds if the curve was flattened.
        assert_eq!(m.at(1, 1), 0);
        assert!(m.at(m.w / 2, m.h / 2) > 200);
    }

    #[test]
    fn empty_glyph_is_a_zero_mask_with_an_advance() {
        let m = glyph_mask(&[], 250.0, 0.0, None, 0.02);
        assert_eq!(m.w, 0);
        assert_eq!(m.h, 0);
        assert!((m.advance - 5.0).abs() < 0.01);
    }
}
