//! Flexbox, the subset that carries real pages: row/column with reverse, wrap,
//! `flex-basis`/`grow`/`shrink` with min/max clamping, `justify-content`,
//! `align-items`/`align-self` (stretch, start, end, center), `gap`, and auto
//! margins swallowing free space (what makes `margin-left:auto` push a nav item
//! to the right).
//!
//! The order follows the spec: measure each item's hypothetical main size, resolve
//! flexible lengths against the free space, re-layout the items that changed, then
//! align on the cross axis and place along the main axis. Not implemented: `order`,
//! `align-content`, `baseline` alignment beyond treating it as start, and
//! `min-content`/`max-content` keywords as a basis.

use super::block::{self, Frame};
use super::boxes::{BoxKind, LayoutBox, LayoutTree};
use super::Ctx;
use crate::css::value::{Align, Display, FlexDir, Justify, Length, Position, Style};
use crate::util::geom::Rect;
use crate::dom::Kind;

/// Frame extents: horizontal for `main = true` in a row container.
fn frame(main_is_x: bool, fr: &Frame) -> f32 {
    if main_is_x {
        fr.frame_w()
    } else {
        fr.frame_h()
    }
}

/// One flex item, mid-solve.
struct Item {
    /// Index of the box in `LayoutTree::boxes`.
    bi: usize,
    st: Style,
    fr: Frame,
    /// Hypothetical main size, clamped by min/max.
    main: f32,
    basis: f32,
    grows: f32,
    shrinks: f32,
    auto_start: bool,
    auto_end: bool,
}

impl Item {
    fn border_main(&self, tree: &LayoutTree, main_is_x: bool) -> f32 {
        let b = &tree.boxes[self.bi].border_box;
        if main_is_x {
            b.w
        } else {
            b.h
        }
    }

    fn border_cross(&self, tree: &LayoutTree, main_is_x: bool) -> f32 {
        let b = &tree.boxes[self.bi].border_box;
        if main_is_x {
            b.h
        } else {
            b.w
        }
    }
}

/// Lay out the flex items of box `idx` inside `inner` (its content box origin and
/// width, plus its height when the container has a definite one). Returns the
/// content height the container needs.
pub fn layout_container(ctx: &Ctx, tree: &mut LayoutTree, idx: usize, inner: &Rect) -> f32 {
    let st = tree.boxes[idx].style.clone();
    let id = tree.boxes[idx].id;
    let row = matches!(st.flex_dir, FlexDir::Row | FlexDir::RowReverse);
    let reverse = matches!(st.flex_dir, FlexDir::RowReverse | FlexDir::ColumnReverse);
    let main_is_x = row;
    let avail_main = if main_is_x { inner.w } else { inner.h.max(0.0) };
    let avail_cross = if main_is_x {
        inner.h.max(0.0)
    } else {
        inner.w
    };
    let gap = if row { st.col_gap } else { st.row_gap };

    let mut items: Vec<Item> = Vec::new();
    for k in ctx.dom.children(id) {
        let cst = match ctx.dom.node(k) {
            Some(n) if n.kind == Kind::Element => ctx.styled.get(k).clone(),
            _ => continue,
        };
        if cst.display == Display::None
            || cst.display == Display::Contents
            || matches!(cst.position, Position::Absolute | Position::Fixed)
        {
            continue;
        }
        let mut fr = Frame::compute(ctx, &cst, inner.w, true);
        let auto_start = if main_is_x {
            matches!(cst.margin[edge::LEFT], Length::Auto)
        } else {
            matches!(cst.margin[edge::TOP], Length::Auto)
        };
        let auto_end = if main_is_x {
            matches!(cst.margin[edge::RIGHT], Length::Auto)
        } else {
            matches!(cst.margin[edge::BOTTOM], Length::Auto)
        };
        fr.zero_auto();
        // Flex items are blockified.
        let bstyle = Style {
            display: match cst.display {
                Display::Inline | Display::InlineBlock => Display::Block,
                Display::InlineFlex => Display::Flex,
                other => other,
            },
            ..cst
        };
        let mut b = LayoutBox::new(k, ctx.tag(k), bstyle.clone());
        if bstyle.display == Display::Flex {
            b.kind = BoxKind::Flex;
        }
        let bi = tree.push(b);
        tree.link(idx, bi);
        let basis = match cst_flex_basis(&bstyle) {
            Length::Auto => {
                if main_is_x {
                    ctx.opt_len(&bstyle.width, inner.w, &bstyle)
                } else {
                    ctx.opt_len(&bstyle.height, inner.w, &bstyle)
                }
            }
            other => Some(ctx.len(&other, inner.w, &bstyle)),
        };
        // First pass: lay the item out with the main axis free, which is what an
        // auto-basis item wants, and read back the natural size.
        let probe_main = basis.unwrap_or((avail_main - frame(main_is_x, &fr)).max(1.0));
        block::layout_box(
            ctx,
            tree,
            bi,
            &Rect::new(inner.x, inner.y, probe_main, 0.0),
        );
        let natural = {
            let with_frame = if main_is_x {
                tree.boxes[bi].border_box.w + fr.margin[edge::LEFT] + fr.margin[edge::RIGHT]
            } else {
                tree.boxes[bi].border_box.h + fr.margin[edge::TOP] + fr.margin[edge::BOTTOM]
            };
            basis.unwrap_or(with_frame)
        };
        let min_main = main_limit(ctx, &bstyle, true, inner.w, main_is_x);
        let max_main = main_limit(ctx, &bstyle, false, inner.w, main_is_x);
        let mut main = natural;
        if let Some(v) = min_main {
            main = main.max(v);
        }
        if let Some(v) = max_main {
            main = main.min(v);
        }
        let grows = bstyle.flex_grow.max(0.0);
        let shrinks = bstyle.flex_shrink.max(0.0);
        items.push(Item {
            bi,
            st: bstyle,
            fr,
            main: main.max(0.0),
            basis: basis.unwrap_or(0.0),
            grows,
            shrinks,
            auto_start,
            auto_end,
        });
    }
    if items.is_empty() {
        return 0.0;
    }

    // ---- wrap into lines ---------------------------------------------------
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut used = 0.0f32;
    for i in 0..items.len() {
        let add = items[i].main + frame(main_is_x, &items[i].fr);
        let sep = if cur.is_empty() { 0.0 } else { gap };
        if st.flex_wrap && !cur.is_empty() && avail_main > 0.5 && used + sep + add > avail_main + 0.5
        {
            lines.push(std::mem::take(&mut cur));
            used = 0.0;
        }
        used += (if cur.is_empty() { 0.0 } else { gap }) + add;
        cur.push(i);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    // ---- solve and place each line ----------------------------------------
    let mut cross_total = 0.0f32;
    let mut flow = inner.y;
    for line in lines {
        let mut sum = 0.0f32;
        let mut grow_sum = 0.0f32;
        let mut weighted = 0.0f32;
        let mut auto_count = 0usize;
        for &i in line.iter() {
            sum += items[i].main + frame(main_is_x, &items[i].fr);
            grow_sum += items[i].grows;
            weighted += items[i].shrinks * items[i].main;
            if items[i].auto_start || items[i].auto_end {
                auto_count += 1;
            }
        }
        sum += gap * line.len().saturating_sub(1) as f32;
        let free = (avail_main - sum).max(0.0);
        let shortfall = (sum - avail_main).max(0.0);
        if free > 0.5 {
            if auto_count > 0 {
                // Auto margins take the space before flex-grow gets any of it.
                let per = free / auto_count as f32;
                for &i in line.iter() {
                    let (s, e) = if main_is_x {
                        (edge::LEFT, edge::RIGHT)
                    } else {
                        (edge::TOP, edge::BOTTOM)
                    };
                    let it = &items[i];
                    if it.auto_start && it.auto_end {
                        items[i].fr.margin[s] = per * 0.5;
                        items[i].fr.margin[e] = per * 0.5;
                    } else if it.auto_start {
                        items[i].fr.margin[s] = per;
                    } else if it.auto_end {
                        items[i].fr.margin[e] = per;
                    }
                }
            } else if grow_sum > 0.0 {
                for &i in line.iter() {
                    if items[i].grows > 0.0 {
                        let add = free * items[i].grows / grow_sum;
                        let min = main_limit(ctx, &items[i].st, true, inner.w, main_is_x);
                        let max = main_limit(ctx, &items[i].st, false, inner.w, main_is_x);
                        let mut want = items[i].main + add;
                        if let Some(v) = min {
                            want = want.max(v);
                        }
                        if let Some(v) = max {
                            want = want.min(v);
                        }
                        if want != items[i].main {
                            items[i].main = want;
                            relayout(ctx, tree, &mut items[i], main_is_x, inner);
                        }
                    }
                }
            }
        } else if shortfall > 0.5 && weighted > 0.0 {
            for &i in line.iter() {
                if items[i].shrinks <= 0.0 || items[i].main <= 0.0 {
                    continue;
                }
                let take = shortfall * (items[i].shrinks * items[i].main) / weighted;
                let min = main_limit(ctx, &items[i].st, true, inner.w, main_is_x).unwrap_or(0.0);
                let want = (items[i].main - take).max(min);
                if want != items[i].main {
                    items[i].main = want;
                    relayout(ctx, tree, &mut items[i], main_is_x, inner);
                }
            }
        }
        let mut line_main = 0.0f32;
        let mut line_cross = 0.0f32;
        for &i in line.iter() {
            line_main += items[i].main + frame(main_is_x, &items[i].fr);
            let cross = items[i].border_cross(tree, main_is_x)
                + if main_is_x {
                    items[i].fr.margin[edge::TOP] + items[i].fr.margin[edge::BOTTOM]
                } else {
                    items[i].fr.margin[edge::LEFT] + items[i].fr.margin[edge::RIGHT]
                };
            line_cross = line_cross.max(cross);
        }
        line_main += gap * line.len().saturating_sub(1) as f32;
        // Stretch first (it changes cross sizes), then measure the line again.
        if avail_cross > 0.5 && st.align_items == Align::Stretch && line_cross < avail_cross {
            line_cross = avail_cross;
            for &i in line.iter() {
                let it = &items[i];
                let cross_auto = if main_is_x {
                    it.st.height == Length::Auto
                } else {
                    it.st.width == Length::Auto
                };
                let align_self_start = it.st.align_self != Align::Auto && it.st.align_self != Align::Stretch;
                if !cross_auto || align_self_start {
                    continue;
                }
                let target = (avail_cross
                    - if main_is_x {
                        it.fr.margin[edge::TOP] + it.fr.margin[edge::BOTTOM] + it.fr.border[edge::TOP] + it.fr.border[edge::BOTTOM] + it.fr.padding[edge::TOP] + it.fr.padding[edge::BOTTOM]
                    } else {
                        it.fr.margin[edge::LEFT] + it.fr.margin[edge::RIGHT] + it.fr.border[edge::LEFT] + it.fr.border[edge::RIGHT] + it.fr.padding[edge::LEFT] + it.fr.padding[edge::RIGHT]
                    })
                .max(0.0);
                if target > 1.0 {
                    if let Some(b) = tree.get_mut(it.bi) {
                        if main_is_x {
                            b.style.height = Length::Px(target);
                        } else {
                            b.style.width = Length::Px(target);
                        }
                    }
                    block::layout_box(
                        ctx,
                        tree,
                        it.bi,
                        &Rect::new(
                            inner.x,
                            inner.y,
                            if main_is_x { it.main } else { target }.max(1.0),
                            0.0,
                        ),
                    );
                }
            }
        }
        let extra = (avail_main - line_main).max(0.0);
        let (lead, between) = match st.justify {
            Justify::FlexEnd => (extra, 0.0),
            Justify::Center => (extra * 0.5, 0.0),
            Justify::SpaceBetween if line.len() > 1 => (0.0, extra / (line.len() - 1) as f32),
            Justify::SpaceAround if !line.is_empty() => {
                let b = extra / line.len() as f32;
                (b * 0.5, b)
            }
            Justify::SpaceEvenly if !line.is_empty() => {
                let b = extra / (line.len() + 1) as f32;
                (b, b)
            }
            _ => (0.0, 0.0),
        };
        let order: Vec<usize> = if reverse {
            line.iter().rev().copied().collect()
        } else {
            line.clone()
        };
        let mut pos = if main_is_x { inner.x } else { inner.x } + lead;
        let mut along = if main_is_x { inner.x + lead } else { flow + lead };
        let _ = pos;
        for &i in order.iter() {
            let it = &items[i];
            let outer_main = it.main + frame(main_is_x, &it.fr);
            let cross = it.border_cross(tree, main_is_x)
                + if main_is_x {
                    it.fr.margin[edge::TOP] + it.fr.margin[edge::BOTTOM]
                } else {
                    it.fr.margin[edge::LEFT] + it.fr.margin[edge::RIGHT]
                };
            let align = if it.st.align_self != Align::Auto {
                it.st.align_self
            } else {
                st.align_items
            };
            let off = match align {
                Align::FlexEnd => (line_cross - cross).max(0.0),
                Align::Center => ((line_cross - cross) * 0.5).max(0.0),
                _ => 0.0,
            };
            let bb = tree.boxes[it.bi].border_box;
            let (cx, cy) = if main_is_x {
                (
                    along + it.fr.margin[edge::LEFT],
                    flow + off + it.fr.margin[edge::TOP],
                )
            } else {
                (
                    inner.x + off + it.fr.margin[edge::LEFT],
                    along + it.fr.margin[edge::TOP],
                )
            };
            block::shift(tree, it.bi, cx - bb.x, cy - bb.y);
            along += outer_main + between + gap;
        }
        if main_is_x {
            cross_total += line_cross;
            flow += line_cross + gap;
        } else {
            flow += line_main + gap;
            cross_total = cross_total.max(line_cross);
        }
    }
    if main_is_x {
        cross_total
    } else {
        (flow - inner.y).max(0.0)
    }
}

fn cst_flex_basis(st: &Style) -> Length {
    st.flex_basis.clone()
}

/// Re-run an item's box layout now that its main size is decided.
fn relayout(ctx: &Ctx, tree: &mut LayoutTree, item: &mut Item, main_is_x: bool, inner: &Rect) {
    if let Some(b) = tree.get_mut(item.bi) {
        if main_is_x {
            b.style.width = Length::Px(item.main);
        } else {
            b.style.height = Length::Px(item.main);
        }
    }
    let main = item.main.max(1.0);
    block::layout_box(
        ctx,
        tree,
        item.bi,
        &Rect::new(
            inner.x,
            inner.y,
            if main_is_x { main } else { inner.w.max(1.0) },
            0.0,
        ),
    );
    if let Some(b) = tree.get_mut(item.bi) {
        b.margin = item.fr.margin;
        b.padding = item.fr.padding;
        b.border = item.fr.border;
    }
}

/// min/max on the main axis.
fn main_limit(ctx: &Ctx, st: &Style, is_min: bool, containing: f32, main_is_x: bool) -> Option<f32> {
    let l = if main_is_x {
        if is_min {
            &st.min_width
        } else {
            &st.max_width
        }
    } else if is_min {
        &st.min_height
    } else {
        &st.max_height
    };
    if matches!(l, Length::Auto) {
        return None;
    }
    let v = ctx.len(l, containing, st);
    if is_min {
        Some(v.max(0.0))
    } else if v <= 0.0 {
        None
    } else {
        Some(v)
    }
}

use super::boxes::edge;
