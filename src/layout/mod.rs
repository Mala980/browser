//! Layout: styled DOM in, positioned boxes out.
//!
//! The rules implemented here are CSS 2.1 §8-11 (box model, block flow, line
//! boxes, floats) plus a single-line-per-flex-container subset of CSS Flexbox 1,
//! and `position: relative/absolute/sticky/fixed`. Deliberately out: table
//! layout (tables are laid out as stacked blocks), bidirectional text,
//! `writing-mode`, fragmentation of `column-count`, and `shape-outside`.
//!
//! Everything is one pass top-down (widths) and one pass bottom-up (heights),
//! which is possible because `auto` widths come from the containing block and
//! `auto` heights come from the children.

pub mod block;
pub mod boxes;
pub mod flex;
pub mod inline;

pub use boxes::{
    edge, is_replaced, ANON, Atom, BoxKind, Item, LayoutBox, LayoutTree, Line, Marker, Replaced,
    Run,
};

use crate::css::style::{Styled, Viewport};
use crate::css::value::{Display, Length, Style};
use crate::dom::{Dom, Kind};
use crate::font::{FontDB, Request};
use crate::util::geom::Rect;

/// Replaced-element sizes, answered by whoever loads and decodes them (the page
/// layer owns the image cache; layout only ever asks "how big is it").
pub trait Images {
    /// Natural size of `url`, if it is known yet.
    fn size(&self, url: &str) -> Option<(f32, f32)>;

    /// Decoding was attempted and failed: draw the alt/broken placeholder.
    fn failed(&self, _url: &str) -> bool {
        false
    }

    /// Is the resource still in flight? Used to reserve space for lazy images.
    fn pending(&self, _url: &str) -> bool {
        false
    }
}

/// Used by tests and by pages with no images at all.
pub struct NoImages;

impl Images for NoImages {
    fn size(&self, _url: &str) -> Option<(f32, f32)> {
        None
    }
}

/// Left and right float frontiers inside one block container, so inline layout
/// can shorten line boxes the way CSS says it should.
#[derive(Clone, Debug, Default)]
pub struct Floats {
    pub rects: Vec<Rect>,
    /// x of the left frontier (content moves right as floats stack up).
    pub left: f32,
    /// Right edge of the right frontier.
    pub right: f32,
}

impl Floats {
    pub fn new() -> Floats {
        Floats::default()
    }

    /// How much room a line of height `h` starting at `y` has inside `w`.
    pub fn line_width(&self, y: f32, h: f32, x: f32, w: f32) -> (f32, f32) {
        let mut left = x + self.left;
        let mut right = x + w - self.right;
        for r in &self.rects {
            if r.bottom() <= y || r.top() >= y + h {
                continue;
            }
            if r.left() <= x + 1.0 {
                left = left.max(r.right());
            } else {
                right = right.min(r.left());
            }
        }
        if right < left {
            right = left;
        }
        (left - x, right - left)
    }

    /// y at which a box of height `h` placed at `x`..`x+w` no longer touches a
    /// float: used to push stacked floats beside/under each other.
    pub fn clear_y(&self, x: f32, w: f32, from: f32, h: f32, right_side: bool) -> f32 {
        let mut y = from;
        for _ in 0..64 {
            let mut moved = false;
            for r in &self.rects {
                let overlaps_x = if right_side {
                    r.right() >= x + w - 0.5
                } else {
                    r.left() <= x + 0.5
                };
                if overlaps_x && r.bottom() > y && r.top() < y + h {
                    y = r.bottom();
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        y
    }

    pub fn add(&mut self, side: u8, rect: Rect, base_x: f32, w: f32) {
        self.rects.push(rect);
        match side {
            0 => self.left = self.left.max(rect.right() - base_x),
            _ => self.right = self.right.max(base_x + w - rect.left()),
        }
    }

    /// y below every float we know about (used for `clear`).
    pub fn bottom(&self) -> f32 {
        self.rects
            .iter()
            .map(|r| r.bottom())
            .fold(0.0f32, f32::max)
    }
}

/// The read-only world layout lays out against.
pub struct Ctx<'a> {
    pub dom: &'a Dom,
    pub styled: &'a Styled,
    pub fonts: &'a FontDB,
    pub images: &'a dyn Images,
    pub vp: Viewport,
    pub root_font: f32,
    /// Stop the walk after this many boxes: deep pages stay responsive.
    pub max_boxes: usize,
    /// `width`/`height` attribute sizes of `<img>`/`<canvas>`, resolved by the
    /// caller when it built the DOM (`parse_sizes` hook).
    pub shrink_to_viewport: bool,
}

impl<'a> Ctx<'a> {
    pub fn new(
        dom: &'a Dom,
        styled: &'a Styled,
        fonts: &'a FontDB,
        images: &'a dyn Images,
        vp: Viewport,
    ) -> Ctx<'a> {
        Ctx {
            dom,
            styled,
            fonts,
            images,
            vp,
            root_font: 16.0,
            max_boxes: 200_000,
            shrink_to_viewport: false,
        }
    }

    pub fn style(&self, id: usize) -> Style {
        self.styled.get(id).clone()
    }

    /// `Length` for a property that takes no `auto` (padding, borders, offsets).
    pub fn len(&self, l: &Length, containing: f32, st: &Style) -> f32 {
        match l {
            Length::Auto => 0.0,
            other => other.resolve(
                containing,
                st.font_size,
                self.root_font,
                self.vp.width,
                self.vp.height,
            ),
        }
    }

    /// `Length` where `auto` has meaning (width/height/margin): None = auto.
    pub fn opt_len(&self, l: &Length, containing: f32, st: &Style) -> Option<f32> {
        match l {
            Length::Auto => None,
            other => Some(other.resolve(
                containing,
                st.font_size,
                self.root_font,
                self.vp.width,
                self.vp.height,
            )),
        }
    }

    /// Resolve the face for a style: `font-family` list + weight + italic.
    pub fn face(&self, st: &Style) -> Option<usize> {
        let req = Request::new(&st.font_family, st.font_weight, st.font_italic);
        self.fonts.resolve(&req)
    }

    pub fn font_request(st: &Style) -> Request {
        Request::new(&st.font_family, st.font_weight, st.font_italic)
    }

    /// Line box height for `line-height: normal`, using the face metrics when we
    /// have a face and the fallback ratios when we don't.
    pub fn line_metrics(&self, st: &Style, face: Option<usize>) -> (f32, f32) {
        let px = st.font_size;
        if !st.line_height_normal {
            // A specified line-height splits into 0.5 * (lh - (a+d)) leading.
            let a = self.fonts.ascent(face, px);
            let d = self.fonts.descent(face, px);
            let extra = (st.line_height - (a + d)) * 0.5;
            return ((a + extra).max(0.0), (d + extra).max(0.0));
        }
        match face.and_then(|i| self.fonts.face(i)) {
            Some(f) => {
                let gap = f.line_gap_px(px);
                let a = f.ascent(px);
                let d = f.descent(px);
                // Chromium-ish normal: metrics, half the gap on each side.
                (a + gap * 0.5, d + gap * 0.5)
            }
            None => (crate::font::fallback_ascent(px), crate::font::fallback_descent(px)),
        }
    }

    pub fn display(&self, id: usize) -> Display {
        self.styled.get(id).display
    }

    pub fn is_text(&self, id: usize) -> bool {
        matches!(
            self.dom.node(id).map(|n| n.kind),
            Some(Kind::Text)
        )
    }

    pub fn text(&self, id: usize) -> String {
        self.dom
            .node(id)
            .map(|n| n.data.clone())
            .unwrap_or_default()
    }

    pub fn tag(&self, id: usize) -> String {
        self.dom.node(id).map(|n| n.tag.clone()).unwrap_or_default()
    }
}

/// The element is a block container when its children take part in block flow
/// (`display: block`/`list-item`/`flow-root`) rather than being inline.
pub fn is_block_container(kind: BoxKind) -> bool {
    matches!(
        kind,
        BoxKind::Block | BoxKind::ListItem | BoxKind::Flex | BoxKind::Tableish
    )
}

/// Lay out `dom` against `vp`. `styled` must have come from the same DOM
/// revision; callers re-run this when `Dom::revision` changes.
pub fn layout_document(
    dom: &Dom,
    styled: &Styled,
    fonts: &FontDB,
    images: &dyn Images,
    vp: Viewport,
) -> LayoutTree {
    let ctx = Ctx::new(dom, styled, fonts, images, vp);
    let mut tree = LayoutTree::default();
    let root = match dom.html_root() {
        Some(r) => r,
        None => {
            // An empty document still gets a box, so painting and CDP have
            // something to report.
            let b = LayoutBox::anon(
                BoxKind::Block,
                Style {
                    width: Length::Px(vp.width),
                    ..Default::default()
                },
            );
            let idx = tree.push(b);
            tree.root = idx;
            let rect = Rect::new(0.0, 0.0, vp.width, 0.0);
            block::layout_box(&ctx, &mut tree, idx, &rect);
            tree.content_width = vp.width;
            return tree;
        }
    };
    let idx = tree.push(LayoutBox::new(root, "html", ctx.style(root)));
    tree.root = idx;
    let avail = Rect::new(0.0, 0.0, vp.width, 0.0);
    block::layout_box(&ctx, &mut tree, idx, &avail);
    let h = tree
        .boxes
        .get(idx)
        .map(|b| b.border_box.h.max(b.content_box.bottom()))
        .unwrap_or(0.0);
    tree.content_height = h.max(vp.height);
    tree.content_width = tree
        .boxes
        .iter()
        .map(|b| b.margin_box.right())
        .fold(vp.width, f32::max);
    // `position: fixed` boxes are viewport-anchored, so their y offset has to be
    // corrected for the final page height before painting.
    block::place_fixed(&ctx, &mut tree);
    tree
}

/// Resolve `<img width height>` attributes and CSS-free intrinsic sizes for a
/// node, shared by layout and `Page.getLayoutMetrics`.
pub fn replaced_size(ctx: &Ctx, id: usize, st: &Style, available: f32) -> (f32, f32, Option<(f32, f32)>) {
    let tag = ctx.tag(id);
    let url = replaced_url(ctx, id);
    let natural = if url.is_empty() {
        None
    } else {
        ctx.images.size(&url)
    };
    let attr = |n: &str| -> Option<f32> {
        ctx.dom
            .attr(id, n)
            .and_then(|v| v.trim().trim_end_matches('%').parse::<f32>().ok())
            .filter(|v| *v > 0.0)
    };
    let w_css = ctx.opt_len(&st.width, available, st);
    let h_css = ctx.opt_len(&st.height, available, st);
    let aw = attr("width");
    let ah = attr("height");
    let (mut w, mut h) = (
        w_css.or(aw).or(natural.map(|(w, _)| w)).unwrap_or(available),
        h_css.or(ah).or(natural.map(|(_, h)| h)).unwrap_or(0.0),
    );
    if let Some((nw, nh)) = natural {
        if nw > 0.0 && nh > 0.0 {
            if w_css.is_none() && h_css.is_some() {
                w = h * nw / nh;
            } else if h_css.is_none() && w_css.is_some() {
                h = w * nh / nw;
            }
        }
    }
    if tag == "hr" {
        h = h.max(2.0 + st.border_width[edge::TOP] + st.border_width[edge::BOTTOM]);
    }
    if tag == "input" || tag == "select" || tag == "button" || tag == "textarea" {
        // Form controls get a height from the font metrics so they look right at
        // any font-size, and a sensible default width.
        let (a, d) = ctx.line_metrics(st, ctx.face(st));
        h = h.max(a + d + 2.0 * (st.padding[edge::TOP] + st.padding[edge::BOTTOM]));
        if tag == "textarea" {
            h = h.max(2.0 * (a + d));
        }
        if w_css.is_none() && aw.is_none() && tag != "textarea" {
            w = (available * 0.9).min(180.0 * st.font_size / 16.0).max(60.0);
        }
    }
    if tag == "canvas" && natural.is_none() {
        let cw = attr("width").unwrap_or(300.0);
        let chh = attr("height").unwrap_or(150.0);
        if w_css.is_none() {
            w = cw;
        }
        if h_css.is_none() {
            h = chh;
        }
    }
    (w.max(0.0), h.max(0.0), natural)
}

/// The URL a replaced element points at (images, video posters).
pub fn replaced_url(ctx: &Ctx, id: usize) -> String {
    for name in ["src", "poster", "data-src"] {
        if let Some(v) = ctx.dom.attr(id, name) {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    String::new()
}

/// Convenience for tools and tests: parse, cascade and lay out a document.
pub fn layout_html_string(html: &str, css: &str, vp: Viewport, fonts: &FontDB) -> LayoutTree {
    let dom = crate::html::parse_html(html);
    let sheet = crate::css::parse_sheet(css);
    // `style_tree` merges the UA stylesheet itself; passing only the author sheet
    // here is what a page load does too.
    let styled = crate::css::style_tree(&dom, &sheet, vp);
    layout_document(&dom, &styled, fonts, &NoImages, vp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{parse_sheet, style_tree, Viewport};

    fn laid_out(html: &str, css: &str, w: f32) -> (LayoutTree, Dom) {
        let dom = crate::html::parse_html(html);
        let sheet = parse_sheet(css);
        let vp = Viewport {
            width: w,
            height: 600.0,
            ..Default::default()
        };
        let styled = style_tree(&dom, &sheet, vp);
        let fonts = FontDB::new();
        let tree = layout_document(&dom, &styled, &fonts, &NoImages, vp);
        (tree, dom)
    }

    fn first_box_of_tag(tree: &LayoutTree, dom: &Dom, tag: &str) -> Option<LayoutBox> {
        for id in dom.elements() {
            if dom.tag(id) == tag {
                if let Some(i) = tree.box_for_node(id) {
                    return tree.boxes.get(i).cloned();
                }
            }
        }
        None
    }

    #[test]
    fn box_model_and_margin_collapse() {
        let html = "<body style='margin:0'>\
                      <div id=a style='width:100px;height:50px'></div>\
                      <p>x</p>\
                    </body>";
        let (t, d) = laid_out(html, "", 1280.0);
        let a = first_box_of_tag(&t, &d, "div").expect("div box");
        assert_eq!(a.border_box.w, 100.0);
        assert_eq!(a.border_box.h, 50.0);
        assert_eq!(a.border_box.x, 0.0);
        assert_eq!(a.border_box.y, 0.0);
        let p = first_box_of_tag(&t, &d, "p").expect("p box");
        // UA p margin is 1em top and bottom; with no font, normal line height is
        // exactly the font size, so the line box is 16px.
        assert_eq!(p.margin[edge::TOP], 16.0);
        assert_eq!(p.border_box.y, 66.0, "collapsed 16px gap after the div");
        assert_eq!(p.border_box.w, 1280.0);
        assert_eq!(p.border_box.h, 16.0);
        // content_box = border box minus padding/border (all zero here).
        assert_eq!(p.content_box.w, 1280.0);
        // The body grows to contain both.
        let body = first_box_of_tag(&t, &d, "body").expect("body box");
        assert_eq!(body.border_box.h, 50.0 + 16.0 + 16.0);
    }

    #[test]
    fn padding_and_border_are_outside_the_content_width() {
        let html = "<body style='margin:0'><div style='width:100px;padding:10px;border:2px solid'></div></body>";
        let (t, d) = laid_out(html, "", 400.0);
        let a = first_box_of_tag(&t, &d, "div").expect("box");
        assert_eq!(a.border_box.w, 100.0 + 20.0 + 4.0);
        assert_eq!(a.content_box.w, 100.0);
        assert_eq!(a.padding[edge::LEFT], 10.0);
        assert_eq!(a.border[edge::LEFT], 2.0);
    }

    #[test]
    fn auto_margins_centre_a_fixed_width_box() {
        let html = "<body style='margin:0'><div style='width:200px;margin:0 auto;height:10px'></div></body>";
        let (t, d) = laid_out(html, "", 1000.0);
        let a = first_box_of_tag(&t, &d, "div").expect("box");
        assert_eq!(a.margin[edge::LEFT], 400.0);
        assert_eq!(a.border_box.x, 400.0);
    }

    #[test]
    fn inline_text_wraps_inside_the_content_width() {
        let filler = "alpha beta gamma delta epsilon zeta eta theta iota kappa ";
        let html = format!("<body style='margin:0'><p>{f}</p></body>");
        let (t, d) = laid_out(&html, "", 120.0);
        let p = first_box_of_tag(&t, &d, "p").expect("p box");
        assert!(p.lines.len() > 3, "expected wrapping, got {:?}", p.lines.len());
        for l in &p.lines {
            assert!(l.rect.w <= 120.5, "line wider than the box: {:?}", l.rect);
            let right = l.runs.iter().map(|r| r.x + r.w).fold(0.0f32, f32::max);
            assert!(right <= 120.5 + 1.0, "run overflows: {right}");
        }
        assert_eq!(p.border_box.h, p.lines.iter().map(|l| l.rect.h).sum::<f32>());
        let total_text: String = p.lines.iter().flat_map(|l| l.runs.iter().map(|r| r.text.clone())).collect();
        assert!(total_text.replace(' ', "").starts_with("alphabetagamma"));
    }

    #[test]
    fn nowrap_overflows_instead_of_wrapping() {
        let html = "<body style='margin:0'><p style='white-space:nowrap'>one two three four five six seven eight nine ten</p></body>";
        let (t, d) = laid_out(html, "", 60.0);
        let p = first_box_of_tag(&t, &d, "p").expect("p");
        assert_eq!(p.lines.len(), 1, "nowrap must not wrap");
    }

    #[test]
    fn flex_row_splits_free_space_by_grow() {
        let html = "<body style='margin:0'><div style='display:flex'>\
                      <span style='width:100px'>a</span>\
                      <span style='flex:1'>b</span>\
                    </div></body>";
        let (t, d) = laid_out(html, "", 300.0);
        let row = first_box_of_tag(&t, &d, "div").expect("flex row");
        assert_eq!(row.kind, BoxKind::Flex);
        let kids: Vec<LayoutBox> = row
            .children
            .iter()
            .map(|&i| t.boxes[i].clone())
            .collect();
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0].border_box.w, 100.0);
        assert!(
            (kids[1].border_box.w - 200.0).abs() < 1.0,
            "flexible child should get the rest, got {}",
            kids[1].border_box.w
        );
        assert_eq!(kids[1].border_box.x, 100.0, "placed after the first item");
    }

    #[test]
    fn absolute_positioning_uses_the_offsets() {
        let html = "<body style='margin:0;position:relative'>\
                      <div style='position:absolute;top:20px;right:10px;width:50px;height:30px'></div>\
                    </body>";
        let (t, d) = laid_out(html, "", 200.0);
        let a = first_box_of_tag(&t, &d, "div").expect("absolute box");
        assert_eq!(a.border_box.y, 20.0);
        assert_eq!(a.border_box.x, 200.0 - 10.0 - 50.0);
        assert_eq!(a.layer, 2);
    }

    #[test]
    fn list_items_get_markers() {
        let html = "<body style='margin:0'><ul><li>one</li><li>two</li></ul></body>";
        let (t, d) = laid_out(html, "", 400.0);
        let li = first_box_of_tag(&t, &d, "li").expect("li");
        assert_eq!(li.kind, BoxKind::ListItem);
        let m = li.marker.clone().expect("marker");
        assert_eq!(m.text, "\u{2022}");
        assert!(m.rect.x < li.content_box.x, "bullet sits left of the content");
    }

    #[test]
    fn float_shortens_the_lines_next_to_it() {
        let html = "<body style='margin:0'>\
                      <div style='float:left;width:100px;height:40px'></div>\
                      <p>word word word word word word word word word word word word</p>\
                    </body>";
        let (t, d) = laid_out(html, "", 200.0);
        let p = first_box_of_tag(&t, &d, "p").expect("p");
        assert!(p.lines.len() >= 2, "text should wrap into the narrowed column");
        assert!(
            p.lines[0].rect.w <= 101.0,
            "first line should be shortened by the float: {:?}",
            p.lines[0].rect
        );
    }

    #[test]
    fn hit_testing_and_box_model_json() {
        let html = "<body style='margin:0'><div id=x style='width:40px;height:20px;margin:10px'></div></body>";
        let (t, d) = laid_out(html, "", 300.0);
        let id = d.get_element_by_id("x").expect("node");
        let bi = t.box_for_node(id).expect("box");
        assert_eq!(t.hit_test(15.0, 15.0), Some(bi).filter(|_| true).or(Some(bi)));
        let j = t.box_model(bi);
        let content = j.get("content").expect("content quad");
        assert_eq!(content.as_arr().map(|v| v.len()), Some(8));
        let dump = t.dump();
        assert!(dump.contains("<div"));
    }

    #[test]
    fn empty_and_styled_less_documents_survive() {
        let (t, _d) = laid_out("", "", 800.0);
        assert!(t.boxes.len() <= 2, "no root: nothing to lay out");
        let (t2, _d2) = laid_out("<p>hi</p>", "p { color: red }", 800.0);
        assert!(t2.content_height > 0.0);
        // A viewport narrower than any word still produces boxes, not a panic.
        let (t3, _d3) = laid_out("<p>supercalifragilisticexpialidocious</p>", "", 12.0);
        assert!(t3.total_lines() >= 1);
    }
}
