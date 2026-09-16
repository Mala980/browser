//! CSS values: lengths, keywords, and the `Style` struct that layout/paint read.
//!
//! `Style` is one flat struct with cheap types (enums + f32 + Color) rather than a
//! property map: resolving once per element and copying the struct is what makes a
//! restyle after a click or a scroll cheap, which is the difference between feeling
//! like Chromium and feeling like a script toy.

use crate::util::{clamp_f32, Color};

/// A CSS length before it is resolved against containing block / font size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Auto,
    Px(f32),
    /// Percentage of the relevant axis (resolved by the caller).
    Pct(f32),
    Em(f32),
    Rem(f32),
    Vw(f32),
    Vh(f32),
    /// `min()`/`max()`/`clamp()` flattened at parse time into one of the above
    /// when it can be evaluated, otherwise kept as a viewport-ish fallback.
    Ch(f32),
    Zero,
}

impl Default for Length {
    fn default() -> Length {
        Length::Auto
    }
}

impl Length {
    pub fn px(v: f32) -> Length {
        if v == 0.0 {
            Length::Zero
        } else {
            Length::Px(v)
        }
    }

    pub fn is_auto(&self) -> bool {
        matches!(self, Length::Auto)
    }

    pub fn is_zero(&self) -> bool {
        matches!(self, Length::Zero)
    }

    /// Percentage of `containing` (0.0 means "no percentage base known").
    pub fn resolve(&self, containing: f32, font_size: f32, root_font: f32, vw: f32, vh: f32) -> f32 {
        match *self {
            Length::Auto => 0.0,
            Length::Zero => 0.0,
            Length::Px(v) => v,
            Length::Pct(p) => containing * p / 100.0,
            Length::Em(v) => font_size * v,
            Length::Rem(v) => root_font * v,
            Length::Vw(v) => vw * v / 100.0,
            Length::Vh(v) => vh * v / 100.0,
            Length::Ch(v) => font_size * 0.5 * v,
        }
    }

    /// Like `resolve` but `Auto` maps to `auto` sentinel (negative infinity) so
    /// layout can tell them apart from 0.
    pub fn resolve_or(&self, containing: f32, font_size: f32, root_font: f32, vw: f32, vh: f32, auto: f32) -> f32 {
        if self.is_auto() {
            auto
        } else {
            self.resolve(containing, font_size, root_font, vw, vh)
        }
    }

    pub fn parse(text: &str) -> Option<Length> {
        let t = text.trim();
        if t.is_empty() {
            return None;
        }
        if t.eq_ignore_ascii_case("auto") {
            return Some(Length::Auto);
        }
        if t.eq_ignore_ascii_case("none") {
            return Some(Length::Auto);
        }
        let (num, unit) = split_number_unit(t)?;
        if num == 0.0 {
            return Some(Length::Zero);
        }
        match unit.as_str() {
            "" | "px" => Some(Length::Px(num)),
            "pt" => Some(Length::Px(num * 96.0 / 72.0)),
            "%" => Some(Length::Pct(num)),
            "em" => Some(Length::Em(num)),
            "rem" => Some(Length::Rem(num)),
            "vw" => Some(Length::Vw(num)),
            "vh" => Some(Length::Vh(num)),
            "vmin" => Some(Length::Vw(num)),
            "vmax" => Some(Length::Vw(num)),
            "ch" => Some(Length::Ch(num)),
            "ex" | "cm" | "mm" | "in" | "pc" | "q" => {
                // Absolute units convert to px at 96dpi.
                let f = match unit.as_str() {
                    "cm" => 96.0 / 2.54,
                    "mm" => 96.0 / 25.4,
                    "in" => 96.0,
                    "pc" => 16.0,
                    "q" => 96.0 / 101.6,
                    _ => 0.5,
                };
                Some(Length::Px(num * f))
            }
            _ => None,
        }
    }

    pub fn to_css(&self) -> String {
        match *self {
            Length::Auto => "auto".to_string(),
            Length::Zero => "0px".to_string(),
            Length::Px(v) => format!("{}px", trim_num(v)),
            Length::Pct(v) => format!("{}%", trim_num(v)),
            Length::Em(v) => format!("{}em", trim_num(v)),
            Length::Rem(v) => format!("{}rem", trim_num(v)),
            Length::Vw(v) => format!("{}vw", trim_num(v)),
            Length::Vh(v) => format!("{}vh", trim_num(v)),
            Length::Ch(v) => format!("{}ch", trim_num(v)),
        }
    }
}

fn trim_num(v: f32) -> String {
    if (v - v.round()).abs() < 0.005 {
        format!("{}", v.round() as i32)
    } else {
        format!("{:.3}", v)
    }
}

/// `12.5px` -> (12.5, "px"). Also handles `+3`, `.5em`, `1e2px`.
/// Split `12px`/`1.5em` into (number, unit); the unit is lowercased.
pub fn split_number_unit(t: &str) -> Option<(f32, String)> {
    let b = t.as_bytes();
    let mut i = 0usize;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let start = i;
    let mut dots = 0usize;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_digit() {
            i += 1;
        } else if c == b'.' && dots == 0 {
            dots += 1;
            i += 1;
        } else if (c == b'e' || c == b'E') && i > start {
            // scientific notation: 1e3
            i += 1;
            if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                i += 1;
            }
        } else {
            break;
        }
    }
    if i == start {
        return None;
    }
    let num: f32 = t[start..i].trim().parse().ok()?;
    if !num.is_finite() {
        return None;
    }
    let unit = t[i..].trim().to_ascii_lowercase();
    Some((num, unit))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    None,
    Inline,
    Block,
    InlineBlock,
    Flex,
    InlineFlex,
    ListItem,
    Table,
    TableRow,
    TableCell,
    TableGroup,
    Contents,
}

impl Default for Display {
    fn default() -> Display {
        Display::Inline
    }
}

impl Display {
    pub fn is_block_level(&self) -> bool {
        matches!(
            self,
            Display::Block
                | Display::Flex
                | Display::ListItem
                | Display::Table
                | Display::TableRow
                | Display::TableCell
                | Display::TableGroup
        )
    }
    pub fn is_flex(&self) -> bool {
        matches!(self, Display::Flex | Display::InlineFlex)
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Display::None => "none",
            Display::Inline => "inline",
            Display::Block => "block",
            Display::InlineBlock => "inline-block",
            Display::Flex => "flex",
            Display::InlineFlex => "inline-flex",
            Display::ListItem => "list-item",
            Display::Table => "table",
            Display::TableRow => "table-row",
            Display::TableCell => "table-cell",
            Display::TableGroup => "table-row-group",
            Display::Contents => "contents",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Auto,
    Scroll,
}

impl Overflow {
    pub fn is_scrollable(&self) -> bool {
        matches!(self, Overflow::Auto | Overflow::Scroll)
    }
    pub fn clips(&self) -> bool {
        !matches!(self, Overflow::Visible)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Right,
    Center,
    Justify,
    Start,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    Nowrap,
    Pre,
    PreWrap,
    BreakSpaces,
    PreLine,
}

impl WhiteSpace {
    pub fn collapses(&self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::BreakSpaces)
    }
    pub fn wraps(&self) -> bool {
        matches!(
            self,
            WhiteSpace::Normal | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces | WhiteSpace::PreLine
        )
    }
    pub fn preserves(&self) -> bool {
        matches!(
            self,
            WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces | WhiteSpace::PreLine
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexDir {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

impl FlexDir {
    pub fn is_column(&self) -> bool {
        matches!(self, FlexDir::Column | FlexDir::ColumnReverse)
    }
    pub fn is_reverse(&self) -> bool {
        matches!(self, FlexDir::RowReverse | FlexDir::ColumnReverse)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Stretch,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Hidden,
    Solid,
    Dashed,
    Dotted,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

impl BorderStyle {
    pub fn paints(&self) -> bool {
        !matches!(self, BorderStyle::None | BorderStyle::Hidden)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BackgroundSize {
    Auto,
    Cover,
    Contain,
    Cols(Length, Length),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundRepeat {
    Repeat,
    RepeatX,
    RepeatY,
    NoRepeat,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImageValue {
    None,
    Url(String),
    Linear {
        /// Angle in degrees, 0deg points up (CSS convention).
        angle: f32,
        stops: Vec<(f32, Color)>,
    },
    Radial {
        stops: Vec<(f32, Color)>,
        circle: bool,
    },
    /// A solid colour expressed as a gradient image (`linear-gradient(red,red)`).
    Solid(Color),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub inset: bool,
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
}

/// 2x3 affine matrix (a b c d e f) in CSS order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Default for Matrix {
    fn default() -> Matrix {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
}

impl Matrix {
    pub fn identity() -> Matrix {
        Matrix::default()
    }
    pub fn is_identity(&self) -> bool {
        *self == Matrix::default()
    }
    pub fn translate(x: f32, y: f32) -> Matrix {
        Matrix {
            e: x,
            f: y,
            ..Matrix::default()
        }
    }
    pub fn mul(&self, o: &Matrix) -> Matrix {
        Matrix {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }
    pub fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
    pub fn determinant(&self) -> f32 {
        self.a * self.d - self.b * self.c
    }
}

/// The computed style of one element. Field order mirrors the CSS box model so
/// reading layout code stays legible.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub display: Display,
    pub position: Position,
    pub float: Float,
    pub clear: bool,
    pub box_sizing_border: bool,
    // box
    pub width: Length,
    pub height: Length,
    pub min_width: Length,
    pub min_height: Length,
    pub max_width: Length,
    pub max_height: Length,
    pub margin: [Length; 4],
    pub padding: [Length; 4],
    pub inset: [Length; 4],
    pub border_width: [f32; 4],
    pub border_style: [BorderStyle; 4],
    pub border_color: [Color; 4],
    pub radius: [f32; 4],
    // text
    pub color: Color,
    pub font_size: f32,
    pub line_height: f32,
    pub line_height_normal: bool,
    pub font_weight: u16,
    pub font_italic: bool,
    pub font_family: String,
    pub font_monospace: bool,
    pub text_align: TextAlign,
    pub text_indent: f32,
    pub white_space: WhiteSpace,
    pub word_break: bool,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub text_decoration_underline: bool,
    pub text_decoration_line_through: bool,
    pub text_transform_upper: bool,
    pub text_transform_lower: bool,
    pub text_transform_capitalize: bool,
    // background & effects
    pub background_color: Color,
    pub background_image: ImageValue,
    pub background_size: BackgroundSize,
    pub background_repeat: BackgroundRepeat,
    pub background_pos_x: f32,
    pub background_pos_y: f32,
    pub opacity: f32,
    pub visibility: bool,
    pub shadow: Vec<Shadow>,
    pub transform: Matrix,
    pub transform_origin_x: f32,
    pub transform_origin_y: f32,
    pub outline_width: f32,
    pub outline_color: Color,
    pub filter_blur: f32,
    pub filter_brightness: f32,
    pub filter_invert: bool,
    // flow
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub z_index: Option<i32>,
    pub list_style: bool,
    pub cursor_pointer: bool,
    // flex
    pub flex_dir: FlexDir,
    pub flex_wrap: bool,
    pub justify: Justify,
    pub align_items: Align,
    pub align_self: Align,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Length,
    pub row_gap: f32,
    pub col_gap: f32,
    // misc
    pub object_fit_cover: bool,
    pub user_select_none: bool,
    pub pointer_events_none: bool,
    pub appearance_none: bool,
}

impl Default for Style {
    fn default() -> Style {
        Style {
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: false,
            box_sizing_border: false,
            width: Length::Auto,
            height: Length::Auto,
            min_width: Length::Auto,
            min_height: Length::Auto,
            max_width: Length::Auto,
            max_height: Length::Auto,
            margin: [Length::Zero, Length::Zero, Length::Zero, Length::Zero],
            padding: [Length::Zero, Length::Zero, Length::Zero, Length::Zero],
            inset: [Length::Auto; 4],
            border_width: [0.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [Color::BLACK; 4],
            radius: [0.0; 4],
            color: Color::BLACK,
            font_size: 16.0,
            line_height: 18.4,
            line_height_normal: true,
            font_weight: 400,
            font_italic: false,
            font_family: String::new(),
            font_monospace: false,
            text_align: TextAlign::Start,
            text_indent: 0.0,
            white_space: WhiteSpace::Normal,
            word_break: false,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            text_decoration_underline: false,
            text_decoration_line_through: false,
            text_transform_upper: false,
            text_transform_lower: false,
            text_transform_capitalize: false,
            background_color: Color::TRANSPARENT,
            background_image: ImageValue::None,
            background_size: BackgroundSize::Auto,
            background_repeat: BackgroundRepeat::Repeat,
            background_pos_x: 0.0,
            background_pos_y: 0.0,
            opacity: 1.0,
            visibility: true,
            shadow: Vec::new(),
            transform: Matrix::identity(),
            transform_origin_x: 0.5,
            transform_origin_y: 0.5,
            outline_width: 0.0,
            outline_color: Color::BLACK,
            filter_blur: 0.0,
            filter_brightness: 1.0,
            filter_invert: false,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            z_index: None,
            list_style: false,
            cursor_pointer: false,
            flex_dir: FlexDir::Row,
            flex_wrap: false,
            justify: Justify::FlexStart,
            align_items: Align::Stretch,
            align_self: Align::Auto,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Length::Auto,
            row_gap: 0.0,
            col_gap: 0.0,
            object_fit_cover: false,
            user_select_none: false,
            pointer_events_none: false,
            appearance_none: false,
        }
    }
}

impl Style {
    /// Properties that inherit down the tree; everything else resets.
    pub fn inherit_from(parent: &Style) -> Style {
        let mut s = Style::default();
        s.color = parent.color;
        s.font_size = parent.font_size;
        s.line_height = parent.line_height;
        s.line_height_normal = parent.line_height_normal;
        s.font_weight = parent.font_weight;
        s.font_italic = parent.font_italic;
        s.font_family = parent.font_family.clone();
        s.font_monospace = parent.font_monospace;
        s.text_align = parent.text_align;
        s.text_indent = parent.text_indent;
        s.white_space = parent.white_space;
        s.word_break = parent.word_break;
        s.letter_spacing = parent.letter_spacing;
        s.word_spacing = parent.word_spacing;
        s.text_decoration_underline = parent.text_decoration_underline;
        s.text_decoration_line_through = parent.text_decoration_line_through;
        s.text_transform_upper = parent.text_transform_upper;
        s.text_transform_lower = parent.text_transform_lower;
        s.text_transform_capitalize = parent.text_transform_capitalize;
        s.visibility = parent.visibility;
        s.cursor_pointer = parent.cursor_pointer;
        s.list_style = parent.list_style;
        s.border_color = parent.border_color;
        s.outline_color = parent.outline_color;
        s
    }

    pub fn is_inline(&self) -> bool {
        matches!(
            self.display,
            Display::Inline | Display::InlineBlock | Display::InlineFlex
        )
    }

    pub fn has_border(&self) -> bool {
        self.border_width.iter().any(|w| *w > 0.0)
            || self.border_style.iter().any(|s| s.paints())
    }

    pub fn border_edge(&self, i: usize) -> (f32, Color) {
        if !self.border_style[i].paints() {
            (0.0, self.border_color[i])
        } else {
            (self.border_width[i], self.border_color[i])
        }
    }

    pub fn max_dim(&self, v: f32, max: &Length, min: &Length, fs: f32, vw: f32, vh: f32) -> f32 {
        let mut out = v;
        if !max.is_auto() {
            out = clamp_f32(out, 0.0, max.resolve(out, fs, fs, vw, vh).max(0.0));
        }
        if !min.is_auto() {
            out = out.max(min.resolve(out, fs, fs, vw, vh));
        }
        if out.is_nan() || out < 0.0 {
            0.0
        } else {
            out
        }
    }
}

/// Angle parsing for gradients/transforms; `turn`/`rad`/`grad` supported.
pub fn parse_angle(text: &str) -> Option<f32> {
    let (num, unit) = split_number_unit(text.trim())?;
    match unit.as_str() {
        "" | "deg" => Some(num),
        "rad" => Some(num * 180.0 / std::f32::consts::PI),
        "turn" => Some(num * 360.0),
        "grad" => Some(num * 0.9),
        _ => None,
    }
}

pub fn parse_color(text: &str) -> Option<Color> {
    Color::parse(text)
}

pub fn parse_float(text: &str) -> Option<f32> {
    let v: f32 = text.trim().parse().ok()?;
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

/// Split a value on top-level spaces (ignoring spaces inside `url(...)`,
/// `calc(...)` and quotes), which every shorthand parser needs.
pub fn split_top(value: &str) -> Vec<String> {
    split_top_sep(value, ' ')
}

/// Split on a character at nesting depth 0 (`:` for `a:b`, `,` for gradients).
/// Quoted strings and `(...)` groups are opaque, so `url(a b.png)` and
/// `linear-gradient(red, blue)` survive intact.
pub fn split_top_sep(value: &str, sep: char) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in value.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                cur.push(c);
            }
            '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' => {
                depth -= 1;
                cur.push(c);
            }
            c if depth == 0 && c == sep => {
                let t = cur.trim().to_string();
                if !t.is_empty() {
                    out.push(t);
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    let t = cur.trim().to_string();
    if !t.is_empty() {
        out.push(t);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_units() {
        assert_eq!(Length::parse("12px"), Some(Length::Px(12.0)));
        assert_eq!(Length::parse("0"), Some(Length::Zero));
        assert_eq!(Length::parse("50%"), Some(Length::Pct(50.0)));
        assert_eq!(Length::parse("1.5em"), Some(Length::Em(1.5)));
        assert_eq!(Length::parse("2rem"), Some(Length::Rem(2.0)));
        assert_eq!(Length::parse("1in"), Some(Length::Px(96.0)));
        assert_eq!(Length::parse("auto"), Some(Length::Auto));
        assert_eq!(Length::parse("bogus"), None);
        assert_eq!(Length::parse("1e2px"), Some(Length::Px(100.0)));
        let s = Length::Pct(25.0).resolve(200.0, 16.0, 16.0, 1000.0, 800.0);
        assert_eq!(s, 50.0);
        assert_eq!(
            Length::Rem(2.0).resolve(0.0, 20.0, 13.0, 0.0, 0.0),
            26.0
        );
    }

    #[test]
    fn split_respects_nesting() {
        assert_eq!(split_top("1px solid red"), vec!["1px", "solid", "red"]);
        assert_eq!(
            split_top("url(a b.png) no-repeat"),
            vec!["url(a b.png)", "no-repeat"]
        );
        assert_eq!(
            split_top_sep("linear-gradient(0deg, red, blue)", ','),
            vec!["linear-gradient(0deg", "red", "blue)"]
        );
        assert_eq!(
            split_top_sep("no-repeat repeat", ' ')
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["no-repeat", "repeat"]
        );
    }

    #[test]
    fn matrix_math() {
        let t = Matrix::translate(10.0, -4.0);
        let s = Matrix {
            a: 2.0,
            d: 3.0,
            ..Matrix::default()
        };
        let m = s.mul(&t);
        assert_eq!(m.map(0.0, 0.0), (20.0, -12.0));
        assert_eq!(m.a, 2.0);
        assert!(!m.is_identity());
        assert!(Matrix::identity().is_identity());
    }

    #[test]
    fn inheritance_subset() {
        let mut parent = Style::default();
        parent.color = Color::rgb(1, 2, 3);
        parent.font_size = 13.0;
        parent.width = Length::Px(40.0);
        let child = Style::inherit_from(&parent);
        assert_eq!(child.color, Color::rgb(1, 2, 3));
        assert_eq!(child.font_size, 13.0);
        assert_eq!(child.width, Length::Auto, "width must not inherit");
        assert!(Display::Block.is_block_level());
        assert!(!Display::Inline.is_block_level());
        assert!(WhiteSpace::Pre.preserves() && !WhiteSpace::Pre.collapses());
    }
}
