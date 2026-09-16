//! The box tree: what layout produces and what paint/CDP consume.
//!
//! Every box keeps three nested rectangles, because that is how CSS is written
//! and how DevTools reports geometry:
//!
//! ```text
//! +-------------------------------------+  margin_box
//! | +---------------------------------+ |  border_box
//! | | +-----------------------------+ | |  padding_box (= border_box minus
//! | | |   content_box               | | |    border widths)
//! ```
//!
//! Coordinates are absolute in *page* space (not viewport space): painting
//! subtracts the scroll offset, `DOM.getBoxModel` and hit testing use them as
//! they are. Boxes that overflow a clipping ancestor are still laid out; the
//! painter clips them.

use crate::css::value::Style;
use crate::util::geom::Rect;
use crate::util::{Color, Json};

/// Anonymous boxes (a text run wrapped in a block, a block's inline wrapper)
/// have no DOM node.
pub const ANON: usize = usize::MAX;

/// Indexes for the `[4]` arrays in `Style` and here. CSS writes box shorthands
/// clockwise from the top, and `css::parse::box_sides` keeps that order, so the
/// arrays are `[top, right, bottom, left]` - not the `[l,t,r,b]` geometry order
/// the rect helpers use.
pub mod edge {
    pub const TOP: usize = 0;
    pub const RIGHT: usize = 1;
    pub const BOTTOM: usize = 2;
    pub const LEFT: usize = 3;

    /// Horizontal pair, for the usual "left + right" sum.
    pub const H: [usize; 2] = [LEFT, RIGHT];
    pub const V: [usize; 2] = [TOP, BOTTOM];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxKind {
    /// Block container with block children.
    Block,
    /// Block container whose children are inline: owns the line boxes.
    Inline,
    /// Generated wrapper (anonymous block around bare text, or around inline
    /// content inside a block container).
    Anonymous,
    InlineBlock,
    Flex,
    ListItem,
    /// Replaced content: <img>, <video>, <canvas>, form controls.
    Replaced,
    /// display:table / row / cell, laid out as a simple stacked block for now.
    Tableish,
}

impl Default for BoxKind {
    fn default() -> BoxKind {
        BoxKind::Block
    }
}

/// One source of inline content inside a block container: a DOM node, or a
/// generated pseudo box whose text lives only in the style.
#[derive(Clone, Debug)]
pub enum Item {
    Node(usize),
    Pseudo {
        text: String,
        style: Box<Style>,
    },
}

impl Item {
    pub fn node(&self) -> usize {
        match self {
            Item::Node(i) => *i,
            Item::Pseudo { .. } => ANON,
        }
    }
}

/// One styled piece of text inside a line box, already measured.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// DOM text node this run came from.
    pub node: usize,
    /// Byte range inside that node's data, so CDP and selection can map back.
    pub start: usize,
    pub end: usize,
    /// x within the line box, the advance width, and the width without the
    /// trailing spaces that alignment ignores.
    pub x: f32,
    pub w: f32,
    pub w_trim: f32,
    /// y of the run's text top, relative to the top of its line box.
    pub dy: f32,
    /// The exact text to draw: a copy, so painting never has to touch the DOM.
    pub text: String,
    /// Index into `FontDB::faces`; None means "no face, use fallback metrics".
    pub face: Option<usize>,
    pub size: f32,
    pub color: Color,
    pub underline: bool,
    pub line_through: bool,
    /// `background-color` on an inline element paints behind its own runs only.
    pub bg: Option<crate::util::Color>,
    /// Set when the style asked for bold/italic but no such face was found, so
    /// paint can synthesise it.
    pub synth_bold: bool,
    pub synth_italic: bool,
    /// Trailing-space width, so justification can stretch spaces only.
    pub space: f32,
}

impl Default for Run {
    fn default() -> Run {
        Run {
            node: ANON,
            start: 0,
            end: 0,
            x: 0.0,
            w: 0.0,
            w_trim: 0.0,
            dy: 0.0,
            text: String::new(),
            face: None,
            size: 16.0,
            color: Color::BLACK,
            underline: false,
            line_through: false,
            bg: None,
            synth_bold: false,
            synth_italic: false,
            space: 0.0,
        }
    }
}

/// A line box: the vertical unit of inline layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    /// Line box in page coordinates. `w` is the containing block's content width.
    pub rect: Rect,
    /// Baseline measured from `rect.y`.
    pub baseline: f32,
    pub ascent: f32,
    pub descent: f32,
    pub runs: Vec<Run>,
    /// Atomic inlines on this line (images, inline-blocks), in page coords.
    pub atoms: Vec<Atom>,
    /// True when the line was broken because it was too long, not because of a
    /// hard newline: tells paint not to draw the trailing space.
    pub soft_break: bool,
}

/// Intrinsic data for a replaced element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Replaced {
    pub url: String,
    /// Decoded size, when it is known (None = still loading or not decodable).
    pub natural: Option<(f32, f32)>,
    pub alt: String,
    /// Video/canvas/placeholder height floor so the page does not jump.
    pub reserved: Option<(f32, f32)>,
}

/// An atomic inline placed on a line box: its border box in page coordinates,
/// plus which box in the tree it is (paint recurses into it for backgrounds,
/// borders and its own text).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Atom {
    pub box_idx: usize,
    pub rect: Rect,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marker {
    pub text: String,
    pub rect: Rect,
}

#[derive(Clone, Debug)]
pub struct LayoutBox {
    pub id: usize,
    pub anon: bool,
    /// Lowercase tag name (`"p"`), empty for anonymous boxes. Kept because
    /// input handling and CDP both want to say what was clicked.
    pub tag: String,
    pub kind: BoxKind,
    pub style: Style,
    pub margin: [f32; 4],
    pub border: [f32; 4],
    pub padding: [f32; 4],
    pub margin_box: Rect,
    pub border_box: Rect,
    pub content_box: Rect,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
    pub lines: Vec<Line>,
    pub replaced: Option<Replaced>,
    pub marker: Option<Marker>,
    pub z_index: i32,
    /// Out-of-flow boxes are laid out after in-flow ones and painted in this
    /// order: 0 = in flow, 1 = float, 2 = positioned (relative/absolute/fixed).
    pub layer: u8,
    /// `overflow` actually clipped something.
    pub clipped: bool,
    /// A scroll container with content taller than its box.
    pub scrollable: bool,
}

impl Default for LayoutBox {
    fn default() -> LayoutBox {
        LayoutBox {
            id: ANON,
            anon: true,
            tag: String::new(),
            kind: BoxKind::Block,
            style: Style::default(),
            margin: [0.0; 4],
            border: [0.0; 4],
            padding: [0.0; 4],
            margin_box: Rect::zero(),
            border_box: Rect::zero(),
            content_box: Rect::zero(),
            children: Vec::new(),
            parent: None,
            lines: Vec::new(),
            replaced: None,
            marker: None,
            z_index: 0,
            layer: 0,
            clipped: false,
            scrollable: false,
        }
    }
}

impl LayoutBox {
    pub fn new(id: usize, tag: impl Into<String>, style: Style) -> LayoutBox {
        let tag = tag.into();
        LayoutBox {
            id,
            anon: false,
            kind: kind_of(&style, &tag),
            style,
            ..Default::default()
        }
    }

    pub fn anon(kind: BoxKind, style: Style) -> LayoutBox {
        LayoutBox {
            id: ANON,
            anon: true,
            tag: String::new(),
            kind,
            style,
            ..Default::default()
        }
    }

    pub fn padding_box(&self) -> Rect {
        let b = &self.border;
        Rect::from_ltrb(
            self.border_box.left() + b[edge::LEFT],
            self.border_box.top() + b[edge::TOP],
            self.border_box.right() - b[edge::RIGHT],
            self.border_box.bottom() - b[edge::BOTTOM],
        )
    }

    pub fn set_border_box(&mut self, r: Rect) {
        self.border_box = r;
        self.content_box = self.padding_box();
        let m = &self.margin;
        self.margin_box = Rect::from_ltrb(
            r.left() - m[edge::LEFT],
            r.top() - m[edge::TOP],
            r.right() + m[edge::RIGHT],
            r.bottom() + m[edge::BOTTOM],
        );
    }

    /// The box a child should be positioned against.
    pub fn child_containing(&self) -> Rect {
        self.content_box
    }

    pub fn has_lines(&self) -> bool {
        !self.lines.is_empty()
    }

    pub fn visible(&self) -> bool {
        self.style.visibility && self.style.opacity > 0.001 && !self.border_box.is_empty()
    }

    /// Horizontal padding + border, i.e. what `width:auto` has to leave room for.
    pub fn frame_width(&self) -> f32 {
        self.padding[edge::LEFT] + self.padding[edge::RIGHT]
            + self.border[edge::LEFT] + self.border[edge::RIGHT]
    }

    /// Vertical padding + border.
    pub fn frame_height(&self) -> f32 {
        self.padding[edge::TOP] + self.padding[edge::BOTTOM]
            + self.border[edge::TOP] + self.border[edge::BOTTOM]
    }

    pub fn text_width(&self) -> f32 {
        self.lines
            .iter()
            .map(|l| {
                l.runs
                    .iter()
                    .map(|r| r.x + r.w)
                    .fold(0.0f32, f32::max)
            })
            .fold(0.0f32, f32::max)
    }

    pub fn covers(&self, x: f32, y: f32) -> bool {
        self.margin_box.contains(x, y)
    }
}

fn kind_of(style: &Style, tag: &str) -> BoxKind {
    use crate::css::value::Display as D;
    match style.display {
        D::Flex | D::InlineFlex => BoxKind::Flex,
        D::InlineBlock => BoxKind::InlineBlock,
        D::ListItem => BoxKind::ListItem,
        D::Table | D::TableRow | D::TableCell | D::TableGroup => BoxKind::Tableish,
        D::Inline => BoxKind::Inline,
        _ if is_replaced(tag) => BoxKind::Replaced,
        _ => BoxKind::Block,
    }
}

pub fn is_replaced(tag: &str) -> bool {
    matches!(
        tag,
        "img" | "video" | "canvas" | "iframe" | "embed" | "object" | "input" | "select" | "textarea" | "progress"
            | "meter" | "audio" | "button"
    )
}

/// The whole laid-out page.
#[derive(Clone, Debug, Default)]
pub struct LayoutTree {
    pub boxes: Vec<LayoutBox>,
    pub root: usize,
    /// Widest margin box edge and lowest one: the scrollable content size.
    pub content_width: f32,
    pub content_height: f32,
    pub laid_out: usize,
    /// Set when `max_boxes` cut the walk short (very deep pages).
    pub truncated: bool,
}

impl LayoutTree {
    pub fn push(&mut self, mut b: LayoutBox) -> usize {
        b.parent = None;
        self.boxes.push(b);
        self.boxes.len() - 1
    }

    pub fn get(&self, i: usize) -> Option<&LayoutBox> {
        self.boxes.get(i)
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut LayoutBox> {
        self.boxes.get_mut(i)
    }

    pub fn link(&mut self, parent: usize, child: usize) {
        if let Some(c) = self.boxes.get_mut(child) {
            c.parent = Some(parent);
        }
        if let Some(p) = self.boxes.get_mut(parent) {
            p.children.push(child);
        }
    }

    pub fn box_for_node(&self, id: usize) -> Option<usize> {
        if id == ANON {
            return None;
        }
        self.boxes.iter().position(|b| b.id == id)
    }

    pub fn boxes_for_node(&self, id: usize) -> Vec<usize> {
        self.boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.id == id)
            .map(|(i, _)| i)
            .collect()
    }

    /// Deepest visible box whose margin box contains the point, ignoring boxes
    /// with `pointer-events: none`.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        let mut best: Option<(usize, u64)> = None;
        for (i, b) in self.boxes.iter().enumerate() {
            if !b.visible() || b.style.pointer_events_none || !b.covers(x, y) {
                continue;
            }
            // Deeper (smaller) boxes win; area breaks ties between siblings.
            let area = (b.margin_box.w.max(1.0) * b.margin_box.h.max(1.0)) as u64;
            match best {
                Some((_, a)) if a <= area => {}
                _ => best = Some((i, area)),
            }
        }
        best.map(|(i, _)| i)
    }

    /// (text node, byte offset) for a caret position: used by input handling and
    /// `Input.dispatchMouseEvent` -> selection.
    pub fn caret_at(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        let hit = self.hit_test(x, y)?;
        let mut box_chain = vec![hit];
        let mut p = self.boxes[hit].parent;
        while let Some(i) = p {
            box_chain.push(i);
            p = self.boxes[i].parent;
        }
        for &bi in box_chain.iter() {
            for line in &self.boxes[bi].lines {
                let r = line.rect;
                if y < r.top() || y > r.bottom() {
                    continue;
                }
                let mut prev: Option<&Run> = None;
                for run in &line.runs {
                    let rx = r.left() + run.x;
                    if x >= rx && x <= rx + run.w {
                        let frac = if run.w > 0.0 {
                            ((x - rx) / run.w).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let n = (run.end - run.start) as f32;
                        let off = run.start + (frac * n).round() as usize;
                        return Some((run.node, off.min(run.end)));
                    }
                    prev = Some(run);
                }
                if let Some(run) = prev {
                    let rx = r.left() + run.x;
                    if x > rx {
                        return Some((run.node, run.end));
                    }
                }
            }
        }
        None
    }

    /// `DOM.getBoxModel` wants four quads; we hand back the same three boxes CSS
    /// exposes plus the content quad.
    pub fn box_model(&self, i: usize) -> Json {
        let b = match self.boxes.get(i) {
            Some(b) => b,
            None => return Json::Null,
        };
        let quad = |r: &Rect| {
            Json::arr(vec![
                Json::Num(r.left() as f64),
                Json::Num(r.top() as f64),
                Json::Num(r.right() as f64),
                Json::Num(r.top() as f64),
                Json::Num(r.right() as f64),
                Json::Num(r.bottom() as f64),
                Json::Num(r.left() as f64),
                Json::Num(r.bottom() as f64),
            ])
        };
        let content = b.content_box;
        let pad = b.padding_box();
        Json::object(vec![
            ("content", quad(&content)),
            ("padding", quad(&pad)),
            ("border", quad(&b.border_box)),
            ("margin", quad(&b.margin_box)),
            ("width", Json::Num(b.border_box.w as f64)),
            ("height", Json::Num(b.border_box.h as f64)),
        ])
    }

    /// DevTools' `getBoxModel` also reports the node's own bounds in viewport
    /// space; this is the scroll-adjusted version.
    pub fn viewport_rect(&self, i: usize, scroll_x: f32, scroll_y: f32) -> Rect {
        match self.boxes.get(i) {
            Some(b) => b.border_box.offset(-scroll_x, -scroll_y),
            None => Rect::zero(),
        }
    }

    /// Debug dump, one line per box; `kilat dev layout` prints this.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for (i, b) in self.boxes.iter().enumerate() {
            let name = if b.anon {
                format!("#{i} anon {}", format!("{:?}", b.kind))
            } else {
                format!("#{i} <{}>", b.tag)
            };
            out.push_str(&format!(
                "{name:<24} m=({},{}) b=({},{}) {}x{} lines={} layer={}\n",
                b.margin_box.x,
                b.margin_box.y,
                b.border_box.x,
                b.border_box.y,
                b.border_box.w,
                b.border_box.h,
                b.lines.len(),
                b.layer
            ));
        }
        out
    }

    pub fn total_lines(&self) -> usize {
        self.boxes.iter().map(|b| b.lines.len()).sum()
    }

    pub fn total_runs(&self) -> usize {
        self.boxes
            .iter()
            .map(|b| b.lines.iter().map(|l| l.runs.len()).sum::<usize>())
            .sum()
    }
}
