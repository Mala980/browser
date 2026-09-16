//! Inline layout: turning text (and atomic inlines) into line boxes.
//!
//! Greedy breaking, which is what CSS describes and what browsers do for normal
//! text: walk fragments in order, keep them on the line while they fit, break
//! before the fragment that no longer fits. Implemented here: `white-space`
//! (normal/nowrap/pre/pre-wrap/pre-line/break-spaces), whitespace collapsing,
//! `text-indent`, `text-align` including justify, `letter-spacing`,
//! `word-spacing`, mid-word splitting for unbreakably long words, floats
//! shortening lines, per-run inline `background-color`, decoration inherited
//! through nested inline elements, `::before`/`::after` text, and atomic inlines
//! (`inline-block`, images) aligned with their bottom margin edge on the
//! baseline.
//!
//! Not here: bidirectional reordering, hyphenation, `vertical-align` other than
//! baseline, and optimal (Knuth-Plass) justification.

use super::block;
use super::boxes::{edge, ANON, Atom, BoxKind, Item, LayoutBox, LayoutTree, Line, Run};
use super::{Ctx, Floats};
use crate::css::style::{Content, ContentRun};
use crate::css::value::{Display, Style, TextAlign, WhiteSpace};
use crate::dom::Kind;
use crate::util::geom::Rect;
use crate::util::Color;

/// Marks a forced line break inside the fragment stream.
const HARD_BREAK: char = '\u{1}';

/// Everything about a style that inline layout needs, resolved once per style.
#[derive(Clone, Debug)]
struct FStyle {
    size: f32,
    color: Color,
    face: Option<usize>,
    asc: f32,
    desc: f32,
    /// Explicit `line-height`, or 0 for `normal`.
    line_h: f32,
    ls: f32,
    underline: bool,
    through: bool,
    bg: Option<Color>,
    synth_bold: bool,
    synth_italic: bool,
}

impl FStyle {
    /// Half-leading: what a specified line-height adds above and below.
    fn leading(&self) -> f32 {
        if self.line_h > 0.0 {
            ((self.line_h - (self.asc + self.desc)) * 0.5).max(0.0)
        } else {
            0.0
        }
    }

    fn height(&self) -> f32 {
        (self.asc + self.desc + 2.0 * self.leading()).max(0.0)
    }
}

/// One breakable unit: a word with the spaces that follow it attached, a forced
/// break, or an atomic inline that has already been laid out.
#[derive(Clone, Debug)]
struct Frag {
    text: String,
    /// Advance including trailing spaces.
    w: f32,
    /// Advance without them: what fitting and alignment measure.
    w_trim: f32,
    /// Trailing spaces, which justification stretches.
    space: f32,
    node: usize,
    start: usize,
    end: usize,
    si: usize,
    hard_break: bool,
    atom: Option<AtomInfo>,
}

/// An atomic inline: laid out at the origin, shifted onto its line later.
#[derive(Clone, Copy, Debug)]
struct AtomInfo {
    idx: usize,
    /// Border box size as `block::layout_box` produced it.
    w: f32,
    h: f32,
    cur_x: f32,
    cur_y: f32,
    margin_left: f32,
    margin_bottom: f32,
    /// Distance from the border box top to the atom's baseline: the bottom edge
    /// for images, the first line's ascent for inline-blocks.
    baseline: f32,
}

/// Shrink-to-fit widths, computed from the DOM so measuring never allocates boxes:
/// `max` is the no-break width, `min` the longest unbreakable piece.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Widths {
    pub min: f32,
    pub max: f32,
}

/// Preferred (max-content) and minimum (min-content) widths of `id`'s content,
/// used for shrink-to-fit on floats, inline-blocks, absolutes and tables.
pub fn measure(ctx: &Ctx, id: usize, st: &Style) -> Widths {
    let mut out = Widths::default();
    let kids: Vec<usize> = if id == ANON {
        Vec::new()
    } else {
        ctx.dom.children(id)
    };
    for k in kids {
        match ctx.dom.node(k).map(|n| n.kind) {
            Some(Kind::Text) => {
                let data = ctx.text(k);
                let face = ctx.face(st);
                for w in words(&data, st.white_space) {
                    let trimmed = w.trim_end();
                    let ww = ctx.fonts.measure(face, trimmed, st.font_size, true)
                        + st.letter_spacing * trimmed.chars().count() as f32;
                    let sp = ctx.fonts.measure(face, " ", st.font_size, false) + st.word_spacing;
                    out.max += ww + sp;
                    out.min = out.min.max(ww);
                }
            }
            Some(Kind::Element) => {
                let cst = ctx.styled.get(k).clone();
                if cst.display == Display::None || !cst.visibility || cst.opacity <= 0.0 {
                    continue;
                }
                if let Some(w) = ctx.opt_len(&cst.width, 1.0, &cst) {
                    // A fixed width is what it contributes, exactly.
                    let extra = cst.padding[edge::LEFT]
                        + cst.padding[edge::RIGHT]
                        + cst.border_width[edge::LEFT]
                        + cst.border_width[edge::RIGHT];
                    out.max += w + extra;
                    out.min = out.min.max(w + extra);
                    continue;
                }
                let inner = measure(ctx, k, &cst);
                let extra = cst.padding[edge::LEFT]
                    + cst.padding[edge::RIGHT]
                    + ctx.len(&cst.margin[edge::LEFT], 1.0, &cst)
                    + ctx.len(&cst.margin[edge::RIGHT], 1.0, &cst);
                if cst.display == Display::Inline || cst.display == Display::Contents {
                    out.max += inner.max + extra;
                    out.min = out.min.max(inner.min + extra);
                } else {
                    // Block children stack: the widest one wins.
                    out.max = out.max.max(inner.max + extra);
                    out.min = out.min.max(inner.min + extra);
                }
            }
            _ => {}
        }
    }
    // A replaced child contributes its intrinsic width.
    if super::is_replaced(&ctx.tag(id)) {
        let (w, _h, _n) = super::replaced_size(ctx, id, st, 1.0);
        out.max = out.max.max(w);
        out.min = out.min.max(w.min(out.max.max(1.0)));
    }
    if st.font_size > 0.0 && out.max == 0.0 {
        out.max = st.font_size;
        out.min = st.font_size * 0.5;
    }
    out
}

/// Split text into breakable words under a `white-space` mode. Each returned
/// word keeps one collapsed space at its end (the break opportunity), and a lone
/// `HARD_BREAK` marks a newline that must break the line. Tabs measure as a
/// single space; that is close enough for pre-formatted code in a 1280px window
/// and avoids a tab-stop model.
pub fn words(text: &str, mode: WhiteSpace) -> Vec<String> {
    let collapse = matches!(mode, WhiteSpace::Normal | WhiteSpace::Nowrap);
    let hard_newlines = !matches!(mode, WhiteSpace::Normal | WhiteSpace::Nowrap);
    let mut norm = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => {
                if hard_newlines {
                    norm.push(HARD_BREAK);
                } else {
                    norm.push(' ');
                }
            }
            ' ' | '\t' | '\r' => {
                if collapse {
                    if !norm.ends_with(' ') && !norm.ends_with(HARD_BREAK) && !norm.is_empty() {
                        norm.push(' ');
                    }
                } else {
                    norm.push(' ');
                }
            }
            _ => norm.push(c),
        }
    }
    while norm.ends_with(' ') && mode != WhiteSpace::BreakSpaces {
        norm.pop();
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut fresh = true;
    for c in norm.chars() {
        if c == HARD_BREAK {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(HARD_BREAK.to_string());
            fresh = true;
            continue;
        }
        if c == ' ' && !cur.is_empty() {
            cur.push(' ');
            out.push(std::mem::take(&mut cur));
            fresh = true;
            continue;
        }
        if c == ' ' && (cur.is_empty() && !fresh) {
            // Two spaces in a row in a preserving mode: keep the second one as
            // its own zero-width-ish fragment so `pre` columns line up.
            cur.push(' ');
            out.push(std::mem::take(&mut cur));
            fresh = true;
            continue;
        }
        if c == ' ' && cur.is_empty() && fresh && collapse {
            // Leading space after a break: removed by CSS in collapsing modes.
            continue;
        }
        cur.push(c);
        fresh = false;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ---- line breaking ---------------------------------------------------------

/// Lay out `items` as the inline content of box `owner`. Lines are appended to
/// `owner.lines` starting at `rect.y`; the return value is the height consumed.
pub fn layout_run(
    ctx: &Ctx,
    tree: &mut LayoutTree,
    owner: usize,
    items: &[Item],
    rect: &Rect,
    floats: &Floats,
) -> f32 {
    let own = tree.boxes[owner].style.clone();
    let mut frags: Vec<Frag> = Vec::new();
    let mut styles: Vec<FStyle> = Vec::new();
    for it in items {
        collect(ctx, tree, owner, it, &own, rect.w, &mut frags, &mut styles, 0);
    }
    if frags.is_empty() {
        return 0.0;
    }
    let avail = rect.w.max(1.0);
    let nowrap = own.white_space == WhiteSpace::Nowrap;
    let mut height = 0.0f32;
    let mut i = 0usize;
    let mut first = true;
    let mut guard = 0usize;
    while i < frags.len() {
        guard += 1;
        if guard > 200_000 {
            tree.truncated = true;
            break;
        }
        let est = frag_height(&styles[frags[i].si], own.font_size);
        let (ind, line_w) = floats.line_width(rect.y + height, est, rect.x, avail);
        let indent = if first { own.text_indent } else { 0.0 };
        let usable = (line_w - indent).max(1.0);
        let mut line = Line {
            rect: Rect::new(rect.x + ind, rect.y + height, line_w, est),
            ..Default::default()
        };
        let mut used = 0.0f32;
        let mut asc = 0.0f32;
        let mut desc = 0.0f32;
        let mut n = 0usize;
        let mut hard = false;
        let mut line_atoms: Vec<(AtomInfo, f32)> = Vec::new();
        while i < frags.len() {
            let f = frags[i].clone();
            let si = &styles[f.si];
            if f.hard_break {
                i += 1;
                hard = true;
                if n == 0 {
                    asc = asc.max(si.asc);
                    desc = desc.max(si.desc);
                }
                break;
            }
            let add = if n == 0 { f.w_trim } else { f.w };
            if used + add > usable + 0.6 && n > 0 && !nowrap {
                break;
            }
            if used + add > usable + 0.6 && n == 0 && f.atom.is_none() && !nowrap {
                // A single word wider than the line: cut it at the fit point
                // (CSS `overflow-wrap: break-word` / `word-break: break-all`).
                let cut = long_word_cut(ctx, si, &f.text, usable - used);
                if cut > 0 {
                    let head_w = measure_str(ctx, si, &f.text[..cut]);
                    line.runs.push(Run {
                        text: f.text[..cut].to_string(),
                        x: used + indent,
                        w: head_w,
                        w_trim: head_w,
                        space: 0.0,
                        dy: -(si.asc + si.leading()),
                        node: f.node,
                        start: f.start,
                        end: f.start + cut,
                        ..run_from(si)
                    });
                    asc = asc.max(si.asc + si.leading());
                    desc = desc.max(si.desc + si.leading());
                    used += head_w;
                    let rest_w = (f.w - head_w).max(0.0);
                    let rest_trim = (f.w_trim - head_w).max(0.0);
                    frags[i] = Frag {
                        text: f.text[cut..].to_string(),
                        w: rest_w,
                        w_trim: rest_trim,
                        space: f.space,
                        node: f.node,
                        start: f.start + cut,
                        end: f.end,
                        si: f.si,
                        hard_break: false,
                        atom: None,
                    };
                    if used >= usable {
                        n += 1;
                        break;
                    }
                    continue;
                }
            }
            let lead = si.leading();
            asc = asc.max(si.asc + lead);
            desc = desc.max(si.desc + lead);
            if let Some(a) = f.atom {
                asc = asc.max(a.baseline);
                desc = desc.max((a.h - a.baseline).max(0.0) + a.margin_bottom);
                line_atoms.push((a, used));
                used += f.w;
                n += 1;
                i += 1;
                continue;
            }
            line.runs.push(Run {
                text: f.text.clone(),
                x: used + indent,
                w: f.w,
                w_trim: f.w_trim,
                space: f.space,
                dy: -(si.asc + lead),
                node: f.node,
                start: f.start,
                end: f.end,
                ..run_from(si)
            });
            used += add;
            n += 1;
            i += 1;
        }
        if n == 0 && !hard && i < frags.len() {
            // Defensive: never spin on a fragment that cannot be placed.
            i += 1;
        }
        if asc + desc < 0.5 {
            asc = own.font_size * 0.8;
            desc = own.font_size * 0.2;
        }
        line.ascent = asc;
        line.descent = desc;
        line.baseline = asc;
        line.rect.h = asc + desc;
        for r in &mut line.runs {
            r.dy += asc;
        }
        let free = (line_w - used).max(0.0);
        let mut extra_x = 0.0f32;
        if free > 0.5 {
            match own.text_align {
                TextAlign::Right | TextAlign::End => extra_x = free,
                TextAlign::Center => extra_x = free * 0.5,
                TextAlign::Justify if !hard && i < frags.len() => {
                    justify(&mut line.runs, free);
                }
                _ => {}
            }
        }
        if extra_x != 0.0 {
            for r in &mut line.runs {
                r.x += extra_x;
            }
            for a in &mut line_atoms {
                a.1 += extra_x;
            }
        }
        for (a, ax) in line_atoms {
            let target_x = line.rect.x + ax + a.margin_left;
            let target_y = rect.y + height + asc - a.baseline;
            block::shift(tree, a.idx, target_x - a.cur_x, target_y - a.cur_y);
            line.atoms.push(Atom {
                box_idx: a.idx,
                rect: Rect::new(target_x, target_y, a.w, a.h),
            });
        }
        line.soft_break = !hard;
        if let Some(b) = tree.get_mut(owner) {
            b.lines.push(line);
        }
        height += asc + desc;
        first = false;
    }
    height
}

/// Height to assume while asking the floats how much room a line has.
fn frag_height(s: &FStyle, font_size: f32) -> f32 {
    let h = s.height();
    if h > 0.5 {
        h
    } else {
        font_size * 1.2
    }
}

/// Fields every run copies out of the resolved style.
fn run_from(s: &FStyle) -> Run {
    Run {
        node: ANON,
        start: 0,
        end: 0,
        x: 0.0,
        w: 0.0,
        w_trim: 0.0,
        space: 0.0,
        dy: 0.0,
        text: String::new(),
        face: s.face,
        size: s.size,
        color: s.color,
        underline: s.underline,
        line_through: s.through,
        bg: s.bg,
        synth_bold: s.synth_bold,
        synth_italic: s.synth_italic,
    }
}

fn measure_str(ctx: &Ctx, s: &FStyle, text: &str) -> f32 {
    ctx.fonts.measure(s.face, text, s.size, true) + s.ls * text.chars().count() as f32
}

/// Byte index at which `text` no longer fits in `usable`. Characters are measured
/// one by one up to a bounded prefix, which is the cost of a pathological word
/// (a 500-char token) and nothing else.
fn long_word_cut(ctx: &Ctx, s: &FStyle, text: &str, usable: f32) -> usize {
    if usable <= 0.5 {
        return 0;
    }
    let mut w = 0.0f32;
    let mut prev_byte = 0usize;
    for (k, ch) in text.char_indices() {
        if k > 4096 {
            break;
        }
        let cw = ctx.fonts.measure(s.face, &ch.to_string(), s.size, false) + s.ls;
        if w + cw > usable {
            // Keep at least one character so the line always makes progress.
            return if k == 0 { first_char_end(text) } else { prev_byte };
        }
        w += cw;
        prev_byte = k + ch.len_utf8();
    }
    0
}

fn first_char_end(text: &str) -> usize {
    match text.chars().next() {
        Some(c) => c.len_utf8(),
        None => 0,
    }
}

/// Spread `free` over the inter-word spaces of a justified line.
fn justify(runs: &mut [Run], free: f32) {
    let spaces: Vec<usize> = runs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.space > 0.0)
        .map(|(k, _)| k)
        .collect();
    if spaces.is_empty() {
        return;
    }
    let per = free / spaces.len() as f32;
    let mut extra = 0.0f32;
    for (k, r) in runs.iter_mut().enumerate() {
        r.x += extra;
        if spaces.contains(&k) {
            r.w += per;
            extra += per;
        }
    }
}

// ---- fragment collection ---------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn collect(
    ctx: &Ctx,
    tree: &mut LayoutTree,
    owner: usize,
    item: &Item,
    inherited: &Style,
    avail: f32,
    frags: &mut Vec<Frag>,
    styles: &mut Vec<FStyle>,
    depth: u32,
) {
    if depth > 96 {
        return;
    }
    match item {
        Item::Pseudo { text, style } => {
            let si = push_style(ctx, styles, style, false, false, None);
            push_words(ctx, text, style, si, ANON, frags);
        }
        Item::Node(id) => match ctx.dom.node(*id).map(|n| n.kind) {
            Some(Kind::Text) => {
                let data = ctx.text(*id);
                let si = push_style(ctx, styles, inherited, false, false, None);
                push_words(ctx, &data, inherited, si, *id, frags);
            }
            Some(Kind::Element) => {
                let st = ctx.styled.get(*id).clone();
                if st.display == Display::None || !st.visibility || st.opacity <= 0.0 {
                    return;
                }
                let under = st.text_decoration_underline;
                let thr = st.text_decoration_line_through;
                let bg = if st.background_color.is_transparent() {
                    None
                } else {
                    Some(st.background_color)
                };
                let tag = ctx.tag(*id);
                let atomic = st.display != Display::Inline
                    && st.display != Display::Contents
                    && !matches!(st.display, Display::None);
                if !atomic {
                    if let Some(t) = pseudo_text(ctx, *id, "before") {
                        let si = push_style(ctx, styles, &st, under, thr, bg);
                        push_words(ctx, &t, &st, si, ANON, frags);
                    }
                    for k in ctx.dom.children(*id) {
                        collect(
                            ctx, tree, owner, &Item::Node(k), &st, avail, frags, styles,
                            depth + 1,
                        );
                    }
                    if let Some(t) = pseudo_text(ctx, *id, "after") {
                        let si = push_style(ctx, styles, &st, under, thr, bg);
                        push_words(ctx, &t, &st, si, ANON, frags);
                    }
                    return;
                }
                // Atomic inline (image, inline-block, flex, ...): lay the box out
                // at the origin to size it; the line placer shifts it into place.
                let kind = if super::is_replaced(&tag) {
                    BoxKind::Replaced
                } else {
                    BoxKind::InlineBlock
                };
                let mut b = LayoutBox::new(*id, tag.clone(), st.clone());
                b.kind = kind;
                let bi = tree.push(b);
                tree.link(owner, bi);
                let p = block::layout_box(ctx, tree, bi, &Rect::new(0.0, 0.0, avail.max(1.0), 0.0));
                let bb = tree.boxes[bi].border_box;
                let m = tree.boxes[bi].margin;
                let baseline = if kind == BoxKind::Replaced {
                    bb.h
                } else {
                    let (a, _d) = ctx.line_metrics(&st, ctx.face(&st));
                    a.min(bb.h)
                };
                let si = push_style(ctx, styles, &st, under, thr, bg);
                frags.push(Frag {
                    text: String::new(),
                    w: bb.w + m[edge::LEFT] + m[edge::RIGHT],
                    w_trim: bb.w + m[edge::LEFT] + m[edge::RIGHT],
                    space: 0.0,
                    node: *id,
                    start: 0,
                    end: 0,
                    si,
                    hard_break: false,
                    atom: Some(AtomInfo {
                        idx: bi,
                        w: bb.w,
                        h: p.border_h.max(bb.h),
                        cur_x: bb.x,
                        cur_y: bb.y,
                        margin_left: m[edge::LEFT],
                        margin_bottom: m[edge::BOTTOM],
                        baseline,
                    }),
                });
            }
            _ => {}
        },
    }
}

fn push_style(
    ctx: &Ctx,
    styles: &mut Vec<FStyle>,
    st: &Style,
    underline: bool,
    through: bool,
    bg: Option<Color>,
) -> usize {
    let face = ctx.face(st);
    let (asc, desc) = ctx.line_metrics(st, face);
    let bold_face = face.map(|i| ctx.fonts.face(i).map(|f| f.bold).unwrap_or(false)) == Some(true);
    let italic_face =
        face.map(|i| ctx.fonts.face(i).map(|f| f.italic).unwrap_or(false)) == Some(true);
    styles.push(FStyle {
        size: st.font_size,
        color: st.color,
        face,
        asc,
        desc,
        line_h: if st.line_height_normal {
            0.0
        } else {
            st.line_height
        },
        ls: st.letter_spacing,
        underline: underline || st.text_decoration_underline,
        through: through || st.text_decoration_line_through,
        bg,
        synth_bold: st.font_weight >= 600 && !bold_face,
        synth_italic: st.font_italic && !italic_face,
    });
    styles.len() - 1
}

fn push_words(ctx: &Ctx, text: &str, st: &Style, si: usize, node: usize, frags: &mut Vec<Frag>) {
    let face = ctx.face(st);
    let mut off = 0usize;
    for w in words(text, st.white_space) {
        let start = off;
        off += w.len();
        if w.len() == 1 && w.starts_with(HARD_BREAK) {
            frags.push(Frag {
                text: String::new(),
                w: 0.0,
                w_trim: 0.0,
                space: 0.0,
                node,
                start,
                end: start,
                si,
                hard_break: true,
                atom: None,
            });
            continue;
        }
        let trimmed = w.trim_end();
        let space_n = w.chars().count() - trimmed.chars().count();
        let ww = ctx.fonts.measure(face, trimmed, st.font_size, true)
            + st.letter_spacing * trimmed.chars().count() as f32;
        let sp = if space_n > 0 {
            (ctx.fonts.measure(face, " ", st.font_size, false) + st.word_spacing) * space_n as f32
        } else {
            0.0
        };
        frags.push(Frag {
            text: w,
            w: ww + sp,
            w_trim: ww,
            space: sp,
            node,
            start,
            end: start + trimmed.len(),
            si,
            hard_break: false,
            atom: None,
        });
    }
}

/// `content: "..."` / `attr()` / `counter()` for pseudo boxes, flattened to text.
pub fn pseudo_text(ctx: &Ctx, id: usize, which: &str) -> Option<String> {
    let p = ctx.styled.pseudo(id, which)?;
    let mut s = String::new();
    match &p.content {
        Content::None => return None,
        Content::Runs(runs) => {
            for r in runs {
                match r {
                    ContentRun::Text(t) => s.push_str(t),
                    ContentRun::Attr(a) => s.push_str(&ctx.dom.attr(id, a).unwrap_or_default()),
                    ContentRun::Url(_) => {}
                    ContentRun::Counter(_) => s.push('1'),
                }
            }
        }
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_splitting_respects_white_space() {
        let w = words("  hello   world  ", WhiteSpace::Normal);
        assert_eq!(w, vec!["hello ", "world"]);
        let w = words("a\nb", WhiteSpace::Normal);
        assert_eq!(w, vec!["a b"]);
        let w = words("a\nb", WhiteSpace::Pre);
        assert_eq!(w, vec!["a", "\u{1}", "b"]);
        let w = words("x  y", WhiteSpace::Pre);
        assert_eq!(w, vec!["x", " ", " y"]);
        let w = words("a\nb", WhiteSpace::Nowrap);
        assert_eq!(w, vec!["a b"]);
    }
}
