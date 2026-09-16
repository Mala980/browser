//! Block layout: the block formatting context, margin collapsing, floats,
//! replaced elements, out-of-flow boxes and list markers.
//!
//! One entry point, `layout_box`, does everything: it places a box's border box at
//! the `(x, y)` it is handed (plus its own used left margin), sizes it, recurses,
//! and reports what it consumed. That split exists because margins belong to the
//! *relationship between* siblings: the caller decides the collapsed top margin,
//! the callee cannot know it.

use super::boxes::{edge, ANON, BoxKind, Item, LayoutBox, LayoutTree, Marker, Replaced};
use super::{flex, inline, is_replaced, Ctx, Floats};
use crate::css::value::{BorderStyle, Display, Float as CssFloat, Length, Overflow, Position, Style};
use crate::dom::Kind;
use crate::util::geom::Rect;

/// What a laid-out child reports back to its parent's flow cursor.
#[derive(Clone, Copy, Debug, Default)]
pub struct Placed {
    pub border_h: f32,
    pub margin_top: f32,
    pub margin_bottom: f32,
}

/// Collapse two adjacent margins: largest positive, smallest negative, added.
pub fn collapse_margins(a: f32, b: f32) -> f32 {
    a.max(0.0).max(b.max(0.0)) + a.min(0.0).min(b.min(0.0))
}

/// Used border widths: `none`/`hidden` remove the border, `double` needs twice
/// the space before it reads as two lines.
pub fn border_widths(st: &Style) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for i in 0..4 {
        out[i] = match st.border_style[i] {
            BorderStyle::None | BorderStyle::Hidden => 0.0,
            BorderStyle::Double => (st.border_width[i] * 2.0).max(3.0),
            _ => st.border_width[i].max(0.0),
        };
    }
    out
}

/// Resolved margin/padding/border of a box, before width and height are known.
#[derive(Clone, Copy, Debug, Default)]
pub struct Frame {
    pub margin: [f32; 4],
    pub padding: [f32; 4],
    pub border: [f32; 4],
}

impl Frame {
    /// `keep_auto` leaves `auto` margins as `NAN` so the caller can centre with
    /// them; otherwise they resolve to 0 (which is what vertical `auto` means).
    pub fn compute(ctx: &Ctx, st: &Style, containing_w: f32, keep_auto: bool) -> Frame {
        let mut f = Frame {
            border: border_widths(st),
            ..Default::default()
        };
        for i in 0..4 {
            f.padding[i] = ctx.len(&st.padding[i], containing_w, st).max(0.0);
            f.margin[i] = match st.margin[i] {
                Length::Auto if keep_auto => f32::NAN,
                Length::Auto => 0.0,
                other => ctx.len(&other, containing_w, st),
            };
        }
        f
    }

    pub fn zero_auto(&mut self) {
        for v in self.margin.iter_mut() {
            if v.is_nan() {
                *v = 0.0;
            }
        }
    }

    pub fn frame_w(&self) -> f32 {
        self.padding[edge::LEFT] + self.padding[edge::RIGHT] + self.border[edge::LEFT]
            + self.border[edge::RIGHT]
    }

    pub fn frame_h(&self) -> f32 {
        self.padding[edge::TOP] + self.padding[edge::BOTTOM] + self.border[edge::TOP]
            + self.border[edge::BOTTOM]
    }
}

/// Lay out box `idx`; see the module docs for the `containing` contract.
pub fn layout_box(ctx: &Ctx, tree: &mut LayoutTree, idx: usize, containing: &Rect) -> Placed {
    if tree.boxes.len() > ctx.max_boxes {
        tree.truncated = true;
        return Placed::default();
    }
    tree.laid_out += 1;
    let st = tree.boxes[idx].style.clone();
    let id = tree.boxes[idx].id;
    let mut kind = tree.boxes[idx].kind;
    if st.display == Display::None {
        return Placed::default();
    }
    if is_replaced(&tree.boxes[idx].tag) {
        kind = BoxKind::Replaced;
    }

    let mut fr = Frame::compute(ctx, &st, containing.w, true);
    let auto_l = fr.margin[edge::LEFT].is_nan();
    let auto_r = fr.margin[edge::RIGHT].is_nan();
    fr.zero_auto();
    let frame_w = fr.frame_w();
    let frame_h = fr.frame_h();

    // ---- used width ------------------------------------------------------
    let explicit_w = ctx.opt_len(&st.width, containing.w, &st).map(|w| {
        if st.box_sizing_border {
            (w - frame_w).max(0.0)
        } else {
            w
        }
    });
    let avail = (containing.w - fr.margin[edge::LEFT] - fr.margin[edge::RIGHT] - frame_w).max(0.0);
    let shrink = matches!(
        kind,
        BoxKind::InlineBlock | BoxKind::Replaced | BoxKind::Tableish
    ) || st.position == Position::Absolute
        || st.position == Position::Fixed
        || st.float != CssFloat::None;
    let mut width = match explicit_w {
        Some(w) => w,
        None if kind == BoxKind::Replaced => {
            let (w, _h, _n) = super::replaced_size(ctx, id, &st, containing.w);
            if st.box_sizing_border {
                (w - frame_w).max(0.0)
            } else {
                w
            }
        }
        None if shrink => {
            let m = inline::measure(ctx, id, &st);
            (m.max.min(avail)).max(m.min.min(avail))
        }
        None => avail,
    };
    if let Some(mw) = ctx.opt_len(&st.min_width, containing.w, &st) {
        width = width.max(if st.box_sizing_border { (mw - frame_w).max(0.0) } else { mw });
    }
    if let Some(mw) = ctx.opt_len(&st.max_width, containing.w, &st) {
        width = width.min(if st.box_sizing_border { (mw - frame_w).max(0.0) } else { mw });
    }
    if (auto_l || auto_r) && explicit_w.is_some() {
        let free = (containing.w - width - frame_w).max(0.0);
        if auto_l && auto_r {
            fr.margin[edge::LEFT] = free * 0.5;
            fr.margin[edge::RIGHT] = free * 0.5;
        } else if auto_l {
            fr.margin[edge::LEFT] = free;
        } else {
            fr.margin[edge::RIGHT] = free;
        }
    }
    width = width.max(0.0);
    let x = containing.x + fr.margin[edge::LEFT];
    let y = containing.y;
    let content_x = x + fr.padding[edge::LEFT] + fr.border[edge::LEFT];
    let content_y = y + fr.padding[edge::TOP] + fr.border[edge::TOP];

    // ---- replaced elements ----------------------------------------------
    if kind == BoxKind::Replaced {
        let (_raw_w, raw_h, natural) = super::replaced_size(ctx, id, &st, containing.w);
        let mut content_h = ctx
            .opt_len(&st.height, containing.w, &st)
            .map(|h| {
                if st.box_sizing_border {
                    (h - frame_h).max(0.0)
                } else {
                    h
                }
            })
            .unwrap_or_else(|| (raw_h - frame_h).max(0.0));
        let url = super::replaced_url(ctx, id);
        let mut res = Replaced {
            url: url.clone(),
            natural,
            alt: ctx.dom.attr(id, "alt").unwrap_or_default(),
            reserved: None,
        };
        if natural.is_none() {
            if ctx.images.pending(&url) {
                // Hold a 4:3 hole so the page does not jump when it lands.
                res.reserved = Some((width, width * 0.75));
                content_h = content_h.max(width * 0.75);
            } else if !url.is_empty() || ctx.images.failed(&url) {
                // Broken or missing: leave room for the alt text.
                let (a, d) = ctx.line_metrics(&st, ctx.face(&st));
                content_h = content_h.max((a + d) * 1.2);
            }
        }
        content_h = clamp_height(ctx, &st, content_h, width, frame_h);
        let box_w = width + frame_w;
        let box_h = content_h + frame_h;
        write_frame(tree, idx, &fr);
        set_border_box(tree, idx, Rect::new(x, y, box_w, box_h));
        if let Some(b) = tree.get_mut(idx) {
            b.replaced = Some(res);
            b.kind = BoxKind::Replaced;
        }
        return Placed {
            border_h: box_h,
            margin_top: fr.margin[edge::TOP],
            margin_bottom: fr.margin[edge::BOTTOM],
        };
    }

    let explicit_h = ctx.opt_len(&st.height, containing.w, &st).map(|h| {
        if st.box_sizing_border {
            (h - frame_h).max(0.0)
        } else {
            h
        }
    });

    // ---- flex containers: flex owns every child --------------------------
    if kind == BoxKind::Flex {
        let inner = Rect::new(
            content_x,
            content_y,
            width,
            explicit_h.unwrap_or(0.0),
        );
        let h = flex::layout_container(ctx, tree, idx, &inner);
        let content_h = clamp_height(ctx, &st, explicit_h.unwrap_or(h), width, frame_h);
        write_frame(tree, idx, &fr);
        set_border_box(tree, idx, Rect::new(x, y, width + frame_w, content_h + frame_h));
        return Placed {
            border_h: content_h + frame_h,
            margin_top: fr.margin[edge::TOP],
            margin_bottom: fr.margin[edge::BOTTOM],
        };
    }

    // ---- children --------------------------------------------------------
    let bfc = kind == BoxKind::Flex
        || st.overflow_x != Overflow::Visible
        || st.overflow_y != Overflow::Visible
        || st.float != CssFloat::None
        || st.display == Display::InlineBlock;
    let collapse_top = !bfc && fr.border[edge::TOP] == 0.0 && fr.padding[edge::TOP] == 0.0;
    let collapse_bottom =
        !bfc && fr.border[edge::BOTTOM] == 0.0 && fr.padding[edge::BOTTOM] == 0.0;

    let mut cursor = 0.0f32;
    let mut prev_bottom: Option<f32> = None;
    let mut floats = Floats::new();
    let mut out_of_flow: Vec<(usize, f32)> = Vec::new();
    let kids = child_list(ctx, id);
    let mut run: Vec<Item> = Vec::new();
    let mut last_bottom = 0.0f32;
    let mut k = 0usize;
    while k <= kids.len() {
        let next = kids.get(k).copied();
        let cls = next.and_then(|c| classify(ctx, c));
        let inlineish = matches!(cls, Some(Child::Text) | Some(Child::Inline));
        if inlineish {
            if let Some(c) = next {
                if run.is_empty() {
                    if let Some(it) = before_item(ctx, id) {
                        run.push(it);
                    }
                }
                run.push(Item::Node(c));
            }
            k += 1;
            continue;
        }
        if !run.is_empty() {
            if k >= kids.len() {
                if let Some(it) = after_item(ctx, id, true) {
                    run.push(it);
                }
            }
            let items = std::mem::take(&mut run);
            let h = inline::layout_run(
                ctx,
                tree,
                idx,
                &items,
                &Rect::new(content_x, content_y + cursor, width, 0.0),
                &floats,
            );
            cursor += h;
            prev_bottom = Some(0.0);
        }
        let Some(c) = next else { break };
        match cls {
            Some(Child::Block) => {
                let cst = ctx.style(c);
                let mt = match cst.margin[edge::TOP] {
                    Length::Auto => 0.0,
                    other => ctx.len(&other, width, &cst),
                };
                let used_top = match prev_bottom {
                    None if collapse_top => 0.0,
                    None => mt,
                    Some(pb) => collapse_margins(pb, mt),
                };
                let bi = tree.push(LayoutBox::new(c, ctx.tag(c), cst.clone()));
                tree.link(idx, bi);
                let p = layout_box(
                    ctx,
                    tree,
                    bi,
                    &Rect::new(content_x, content_y + cursor + used_top, width, 0.0),
                );
                last_bottom = match cst.margin[edge::BOTTOM] {
                    Length::Auto => 0.0,
                    other => ctx.len(&other, width, &cst),
                };
                cursor += used_top + p.border_h;
                prev_bottom = Some(last_bottom);
            }
            Some(Child::Floated(side)) => {
                let cst = ctx.style(c);
                let bi = tree.push(LayoutBox::new(c, ctx.tag(c), cst.clone()));
                tree.link(idx, bi);
                let mut outer = content_y + cursor;
                let fx = if side == 0 {
                    content_x + floats.left
                } else {
                    content_x + width - floats.right
                };
                let fw = (width - floats.left - floats.right).max(1.0);
                let mut probe = layout_box(ctx, tree, bi, &Rect::new(fx, outer, fw, 0.0));
                // Floats stack: move down until the frontier no longer collides.
                for _ in 0..16 {
                    let cleared = floats.clear_y(fx, probe.border_h, outer, probe.border_h, side != 0);
                    if cleared <= outer + 0.5 {
                        break;
                    }
                    outer = cleared;
                    probe = layout_box(ctx, tree, bi, &Rect::new(fx, outer, fw, 0.0));
                }
                let bb = tree.boxes[bi].border_box;
                let rect = Rect::new(bb.x, bb.y, bb.w + tree.boxes[bi].margin[edge::LEFT] + tree.boxes[bi].margin[edge::RIGHT], probe.border_h);
                floats.add(side, rect, content_x, width);
                if bfc {
                    cursor = cursor.max(rect.bottom() - content_y);
                }
                prev_bottom = Some(0.0);
            }
            Some(Child::Out(_fixed)) => {
                let cst = ctx.style(c);
                let bi = tree.push(LayoutBox::new(c, ctx.tag(c), cst.clone()));
                tree.link(idx, bi);
                if let Some(b) = tree.get_mut(bi) {
                    b.layer = if cst.position == Position::Fixed { 3 } else { 2 };
                }
                out_of_flow.push((bi, content_y + cursor));
            }
            _ => {}
        }
        k += 1;
    }

    // ---- height ----------------------------------------------------------
    let mut content_h = clamp_height(ctx, &st, explicit_h.unwrap_or(cursor.max(0.0)), width, frame_h);
    if bfc {
        content_h = content_h.max(floats.bottom() - content_y);
    }
    content_h = content_h.max(0.0);
    let box_w = width + frame_w;
    let box_h = content_h + frame_h;
    write_frame(tree, idx, &fr);
    set_border_box(tree, idx, Rect::new(x, y, box_w, box_h));
    if let Some(b) = tree.get_mut(idx) {
        b.clipped = st.overflow_x != Overflow::Visible || st.overflow_y != Overflow::Visible;
        b.scrollable = (st.overflow_y == Overflow::Scroll || st.overflow_y == Overflow::Auto)
            && content_h > cursor + 0.5;
    }
    if kind == BoxKind::ListItem {
        add_marker(ctx, tree, idx);
    }
    let mut margin_bottom = fr.margin[edge::BOTTOM];
    if collapse_bottom {
        // The last child's bottom margin escapes the parent, so the parent's own
        // bottom margin grows instead of the parent getting taller.
        margin_bottom = collapse_margins(margin_bottom, last_bottom);
        if let Some(b) = tree.get_mut(idx) {
            b.margin[edge::BOTTOM] = margin_bottom;
            b.margin_box = Rect::from_ltrb(
                b.border_box.left() - b.margin[edge::LEFT],
                b.border_box.top() - b.margin[edge::TOP],
                b.border_box.right() + b.margin[edge::RIGHT],
                b.border_box.bottom() + margin_bottom,
            );
        }
    }

    // ---- out-of-flow ------------------------------------------------------
    for (bi, static_y) in out_of_flow {
        let cst = tree.boxes[bi].style.clone();
        let anchor = if cst.position == Position::Fixed {
            Rect::new(0.0, 0.0, ctx.vp.width, ctx.vp.height)
        } else {
            positioned_ancestor(tree, idx)
                .map(|a| tree.boxes[a].padding_box())
                .unwrap_or_else(|| tree.boxes[idx].padding_box())
        };
        let mut cfr = Frame::compute(ctx, &cst, anchor.w, true);
        cfr.zero_auto();
        let left = ctx.opt_len(&cst.inset[edge::LEFT], anchor.w, &cst);
        let right = ctx.opt_len(&cst.inset[edge::RIGHT], anchor.w, &cst);
        let top = ctx.opt_len(&cst.inset[edge::TOP], anchor.h, &cst);
        let bottom = ctx.opt_len(&cst.inset[edge::BOTTOM], anchor.h, &cst);
        // A stretched box (both sides set, width auto) resolves its width from
        // the offsets before recursing; everything else uses shrink-to-fit.
        let stretch_w = match (left, right) {
            (Some(l), Some(r)) if cst.width == Length::Auto && cst.box_sizing_border == false => {
                Some((anchor.w - l - r - cfr.frame_w()).max(0.0))
            }
            _ => None,
        };
        if let Some(w) = stretch_w {
            if let Some(b) = tree.get_mut(bi) {
                b.style.width = Length::Px(w);
            }
        }
        let px = match (left, right) {
            (Some(l), _) => anchor.x + l + cfr.margin[edge::LEFT],
            (None, Some(r)) => {
                // Unknown width yet: place at the left edge, then slide left.
                anchor.x + anchor.w - r - cfr.margin[edge::RIGHT]
            }
            (None, None) => tree.boxes[idx].content_box.x + cfr.margin[edge::LEFT],
        };
        let py = match (top, bottom) {
            (Some(t), _) => anchor.y + t + cfr.margin[edge::TOP],
            (None, Some(b)) => anchor.y + anchor.h - b - cfr.margin[edge::BOTTOM],
            (None, None) => static_y,
        };
        let avail_w = (anchor.x + anchor.w - px).max(1.0);
        layout_box(ctx, tree, bi, &Rect::new(px, py, avail_w, 0.0));
        if let (None, Some(r)) = (left, right) {
            let w = tree.boxes[bi].border_box.w;
            let target = anchor.x + anchor.w - r - w - cfr.margin[edge::RIGHT];
            let dx = target - tree.boxes[bi].border_box.x;
            shift(tree, bi, dx, 0.0);
        }
        if let (None, Some(b)) = (top, bottom) {
            let h = tree.boxes[bi].border_box.h;
            let target = anchor.y + anchor.h - b - h - cfr.margin[edge::BOTTOM];
            let dy = target - tree.boxes[bi].border_box.y;
            shift(tree, bi, 0.0, dy);
        }
    }

    // ---- relative offsets ------------------------------------------------
    if st.position == Position::Relative {
        let dx = match (
            ctx.opt_len(&st.inset[edge::LEFT], containing.w, &st),
            ctx.opt_len(&st.inset[edge::RIGHT], containing.w, &st),
        ) {
            (Some(l), _) => l,
            (None, Some(r)) => -r,
            _ => 0.0,
        };
        let dy = match (
            ctx.opt_len(&st.inset[edge::TOP], containing.w, &st),
            ctx.opt_len(&st.inset[edge::BOTTOM], containing.w, &st),
        ) {
            (Some(t), _) => t,
            (None, Some(b)) => -b,
            _ => 0.0,
        };
        if dx != 0.0 || dy != 0.0 {
            shift(tree, idx, dx, dy);
            if let Some(b) = tree.get_mut(idx) {
                b.layer = 1;
            }
        }
    }

    Placed {
        border_h: box_h,
        margin_top: fr.margin[edge::TOP],
        margin_bottom,
    }
}

fn clamp_height(ctx: &Ctx, st: &Style, content_h: f32, width: f32, frame_h: f32) -> f32 {
    let mut h = content_h.max(0.0);
    let adjust = |v: f32| {
        if st.box_sizing_border {
            (v - frame_h).max(0.0)
        } else {
            v
        }
    };
    if let Some(mh) = ctx.opt_len(&st.min_height, width, st) {
        h = h.max(adjust(mh));
    }
    if let Some(mh) = ctx.opt_len(&st.max_height, width, st) {
        h = h.min(adjust(mh));
    }
    h
}

/// The nearest ancestor an `position: absolute` box anchors to.
fn positioned_ancestor(tree: &LayoutTree, mut idx: usize) -> Option<usize> {
    let mut guard = 0;
    while let Some(p) = tree.boxes[idx].parent {
        guard += 1;
        if guard > 512 {
            return None;
        }
        if tree.boxes[p].style.position != Position::Static {
            return Some(p);
        }
        idx = p;
    }
    None
}

/// Move a box and everything under it. Relative offsets and right/bottom-anchored
/// absolutes both need it, and doing it after the fact keeps the recursive layout
/// code free of offset parameters.
pub fn shift(tree: &mut LayoutTree, idx: usize, dx: f32, dy: f32) {
    let mut stack = vec![idx];
    let mut guard = 0usize;
    while let Some(i) = stack.pop() {
        guard += 1;
        if guard > 200_000 {
            break;
        }
        if let Some(b) = tree.get_mut(i) {
            b.margin_box = b.margin_box.offset(dx, dy);
            b.border_box = b.border_box.offset(dx, dy);
            b.content_box = b.content_box.offset(dx, dy);
            for l in &mut b.lines {
                l.rect = l.rect.offset(dx, dy);
                for a in &mut l.atoms {
                    a.rect = a.rect.offset(dx, dy);
                }
            }
            if let Some(m) = b.marker.as_mut() {
                m.rect = m.rect.offset(dx, dy);
            }
            let kids = b.children.clone();
            stack.extend(kids);
        }
    }
}

fn write_frame(tree: &mut LayoutTree, idx: usize, fr: &Frame) {
    if let Some(b) = tree.get_mut(idx) {
        b.margin = fr.margin;
        b.padding = fr.padding;
        b.border = fr.border;
    }
}

fn set_border_box(tree: &mut LayoutTree, idx: usize, border: Rect) {
    if let Some(b) = tree.get_mut(idx) {
        b.set_border_box(border);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Child {
    /// Text node: part of the surrounding inline run.
    Text,
    /// Inline-level element: also part of the run.
    Inline,
    /// Block-level in-flow element.
    Block,
    /// Float, 0 = left, 1 = right.
    Floated(u8),
    /// Absolutely positioned; the flag says "fixed".
    Out(bool),
    /// `display: contents`: contributes children, never a box.
    Contents,
}

fn classify(ctx: &Ctx, id: usize) -> Option<Child> {
    let n = ctx.dom.node(id)?;
    match n.kind {
        Kind::Text => {
            if n.data.is_empty() {
                return None;
            }
            Some(Child::Text)
        }
        Kind::Element => {
            let st = ctx.styled.get(id);
            if st.display == Display::None {
                return None;
            }
            if matches!(st.position, Position::Absolute | Position::Fixed) {
                return Some(Child::Out(st.position == Position::Fixed));
            }
            if st.float != CssFloat::None {
                return Some(Child::Floated(if st.float == CssFloat::Left { 0 } else { 1 }));
            }
            if st.display == Display::Contents {
                return Some(Child::Contents);
            }
            if is_replaced(&n.tag) {
                return Some(Child::Block);
            }
            match st.display {
                Display::Inline | Display::InlineBlock => Some(Child::Inline),
                _ => Some(Child::Block),
            }
        }
        _ => None,
    }
}

/// Children of `id`, with `display: contents` replaced by its own children so its
/// contents join the parent's flow.
fn child_list(ctx: &Ctx, id: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if id == ANON {
        return out;
    }
    for k in ctx.dom.children(id) {
        match classify(ctx, k) {
            Some(Child::Contents) => out.extend(child_list(ctx, k)),
            Some(_) => out.push(k),
            None => {}
        }
    }
    out
}

/// `::before` text, as the first item of the first inline run.
fn before_item(ctx: &Ctx, id: usize) -> Option<Item> {
    let p = ctx.styled.pseudo(id, "before")?;
    let text = inline::pseudo_text(ctx, id, "before")?;
    Some(Item::Pseudo {
        text,
        style: Box::new(p.style.clone()),
    })
}

/// `::after` text, appended when the run being flushed is the last one.
fn after_item(ctx: &Ctx, id: usize, last: bool) -> Option<Item> {
    if !last {
        return None;
    }
    let p = ctx.styled.pseudo(id, "after")?;
    let text = inline::pseudo_text(ctx, id, "after")?;
    Some(Item::Pseudo {
        text,
        style: Box::new(p.style.clone()),
    })
}

/// Bullets and numbers for `display: list-item`, in the marker box left of the
/// content box.
fn add_marker(ctx: &Ctx, tree: &mut LayoutTree, idx: usize) {
    let id = tree.boxes[idx].id;
    let parent = match ctx.dom.parent(id) {
        Some(p) => p,
        None => return,
    };
    let st = tree.boxes[idx].style.clone();
    let (a, d) = ctx.line_metrics(&st, ctx.face(&st));
    let ptag = ctx.tag(parent);
    let text = if ptag == "ol" {
        let mut n: i64 = ctx
            .dom
            .attr(id, "value")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or_else(|| {
                let start = ctx
                    .dom
                    .attr(parent, "start")
                    .and_then(|v| v.trim().parse::<i64>().ok())
                    .unwrap_or(1);
                let mut k = start;
                for &sib in ctx.dom.children(parent).iter() {
                    if sib == id {
                        break;
                    }
                    if ctx.styled.get(sib).display == Display::ListItem {
                        k += 1;
                    }
                }
                k
            });
        if ctx.dom.attr(id, "value").is_some() {
            // Remember the value so the next item continues from it.
            n = ctx.dom.attr(id, "value").and_then(|v| v.parse::<i64>().ok()).unwrap_or(n);
        }
        format!("{n}.")
    } else {
        "\u{2022}".to_string()
    };
    let face = ctx.face(&st);
    let mw = ctx.fonts.measure(face, &text, st.font_size, false).max(st.font_size * 0.5);
    let cb = tree.boxes[idx].content_box;
    let mx = (cb.x - mw - st.font_size * 0.4).max(0.0);
    if let Some(b) = tree.get_mut(idx) {
        b.marker = Some(Marker {
            text,
            rect: Rect::new(mx, cb.y, mw, a + d),
        });
    }
}
