//! The cascade: match rules, resolve conflicts, compute values.
//!
//! Pipeline position: `Dom` + [`Sheet`] -> [`Styled`] (one [`Style`] per element,
//! plus `::before`/`::after` boxes). Layout and painting never re-read CSS text.
//!
//! Cascade order follows the spec's shape: `!important` flips origin priority
//! (UA wins over author), then specificity, then source order. The `style=""`
//! attribute participates as an author declaration with maximal specificity.

use crate::css::parse::{parse_style_attribute, resolve_vars, Decl, Media, Rule, Sheet};
use crate::css::selector::SelectorSet;
use crate::css::value::{
    parse_angle, parse_color, parse_float, split_top, split_top_sep, BackgroundRepeat,
    BackgroundSize, BorderStyle, Display, FlexDir, Float, ImageValue, Justify, Length, Matrix,
    Overflow, Position, Shadow, Style, TextAlign, WhiteSpace,
};
use crate::dom::{Dom, Kind};
use crate::util::geom::Color;
use std::collections::HashMap;

/// Everything a media query or viewport unit can look at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    pub dark: bool,
    /// Device pixel ratio: CSS pixels are unaffected, the raster multiplies.
    pub dpr: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            width: 980.0,
            height: 650.0,
            dark: false,
            dpr: 1.0,
        }
    }
}

/// One item of a `content:` list.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentRun {
    Text(String),
    Attr(String),
    Url(String),
    Counter(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// `normal`/`none`: no pseudo box.
    None,
    Runs(Vec<ContentRun>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PseudoBox {
    pub content: Content,
    pub style: Style,
}

/// Computed styles, indexed by DOM node id.
#[derive(Clone, Debug)]
pub struct Styled {
    pub styles: Vec<Style>,
    pub before: Vec<Option<PseudoBox>>,
    pub after: Vec<Option<PseudoBox>>,
    /// Custom properties per element, kept for `getComputedStyle` and `var()`.
    pub customs: Vec<HashMap<String, String>>,
    /// Style for non-element nodes and out-of-range ids.
    pub default_style: Style,
    pub revision: u64,
    /// Declarations that actually won a property (stats only).
    pub applied: usize,
}

impl Default for Styled {
    fn default() -> Self {
        Styled {
            styles: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            customs: Vec::new(),
            default_style: Style::default(),
            revision: 0,
            applied: 0,
        }
    }
}

impl Styled {
    pub fn get(&self, id: usize) -> &Style {
        self.styles.get(id).unwrap_or(&self.default_style)
    }
    pub fn is_display_none(&self, id: usize) -> bool {
        self.get(id).display == Display::None
    }
    pub fn pseudo(&self, id: usize, which: &str) -> Option<&PseudoBox> {
        let slot = if which == "before" {
            self.before.get(id)
        } else if which == "after" {
            self.after.get(id)
        } else {
            None
        };
        slot.and_then(|o| o.as_ref())
    }
    pub fn len(&self) -> usize {
        self.styles.len()
    }
    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }
}

// ---------------------------------------------------------------------------
// cascade rules
// ---------------------------------------------------------------------------

/// One rule after flattening: selector plus per-pseudo-element declaration sets.
struct CascadeRule<'a> {
    set: &'a SelectorSet,
    media: Option<&'a Media>,
    /// `None` = normal element declarations; `Some("before")` etc.
    slots: Vec<(Option<&'a str>, Vec<(String, &'a str, bool)>)>,
    specificity: u32,
    order: u32,
    origin: u8,
}

impl<'a> CascadeRule<'a> {
    fn new(rule: &'a Rule, origin: u8, order: u32) -> CascadeRule<'a> {
        let mut slots: Vec<(Option<&'a str>, Vec<(String, &'a str, bool)>)> = Vec::new();
        let mut normal: Vec<(String, &'a str, bool)> = Vec::new();
        for d in &rule.declarations {
            match d {
                Decl::Prop {
                    name,
                    value,
                    important,
                } => normal.push((name.clone(), value.as_str(), *important)),
                Decl::Custom {
                    name,
                    value,
                    important,
                } => normal.push((name.clone(), value.as_str(), *important)),
            }
        }
        slots.push((None, normal));
        for (which, decls) in &rule.pseudo {
            let mut v: Vec<(String, &'a str, bool)> = Vec::new();
            for d in decls {
                if let Decl::Prop {
                    name,
                    value,
                    important,
                } = d
                {
                    v.push((name.clone(), value.as_str(), *important));
                }
            }
            slots.push((Some(which.as_str()), v));
        }
        CascadeRule {
            set: &rule.selectors,
            media: rule.media.as_ref(),
            slots,
            specificity: rule.selectors.specificity(),
            order,
            origin,
        }
    }

    fn rank(&self, important: bool) -> u64 {
        // Bit layout: important | origin key | specificity | source order.
        let origin_key = if important {
            (3 - self.origin.min(3)) as u64
        } else {
            self.origin as u64
        };
        ((important as u64) << 63)
            | (origin_key << 55)
            | ((self.specificity as u64) << 31)
            | (self.order as u64)
    }

    fn applies(&self, vp: &Viewport) -> bool {
        match self.media {
            Some(m) => m.matches(vp.width, vp.height, vp.dark),
            None => true,
        }
    }

    fn has_decls(&self, which: Option<&str>) -> bool {
        self.slots
            .iter()
            .any(|(w, list)| *w == which && !list.is_empty())
    }
}

/// Rule index keyed on the rightmost compound's simplest requirement, so an
/// element only tests the rules that can possibly match it.
pub struct RuleIndex<'a> {
    rules: Vec<CascadeRule<'a>>,
    by_tag: HashMap<String, Vec<u32>>,
    by_id: HashMap<String, Vec<u32>>,
    by_class: HashMap<String, Vec<u32>>,
    any: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
enum Key {
    Tag(String),
    Id(String),
    Class(String),
    Any,
}

impl<'a> RuleIndex<'a> {
    fn build(rules: Vec<CascadeRule<'a>>) -> RuleIndex<'a> {
        let mut ix = RuleIndex {
            rules,
            by_tag: HashMap::new(),
            by_id: HashMap::new(),
            by_class: HashMap::new(),
            any: Vec::new(),
        };
        for (i, r) in ix.rules.iter().enumerate() {
            let key = r
                .set
                .complexes
                .iter()
                .filter_map(|c| c.parts.first())
                .find_map(|p| {
                    if let Some(id) = &p.id {
                        Some(Key::Id(id.clone()))
                    } else if let Some(cl) = p.classes.first() {
                        Some(Key::Class(cl.clone()))
                    } else if let Some(t) = &p.tag {
                        if t == "*" {
                            None
                        } else {
                            Some(Key::Tag(t.clone()))
                        }
                    } else {
                        None
                    }
                })
                .unwrap_or(Key::Any);
            let i = i as u32;
            match key {
                Key::Tag(t) => ix.by_tag.entry(t).or_default().push(i),
                Key::Id(id) => ix.by_id.entry(id).or_default().push(i),
                Key::Class(c) => ix.by_class.entry(c).or_default().push(i),
                Key::Any => ix.any.push(i),
            }
        }
        ix
    }

    fn candidates(&self, dom: &Dom, id: usize, out: &mut Vec<u32>) {
        out.clear();
        let tag = dom.tag(id);
        if let Some(v) = self.by_tag.get(&tag) {
            out.extend(v.iter().copied());
        }
        if let Some(i) = dom.attr(id, "id") {
            if let Some(v) = self.by_id.get(&i) {
                out.extend(v.iter().copied());
            }
        }
        for c in dom.class_list(id) {
            if let Some(v) = self.by_class.get(&c) {
                out.extend(v.iter().copied());
            }
        }
        out.extend(self.any.iter().copied());
        out.sort_unstable();
        out.dedup();
    }

    /// Number of rules (used by `kilat css --stats`).
    pub fn len(&self) -> usize {
        self.rules.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// The user-agent sheet plus the page's sheets, applied to the whole tree.
pub fn style_tree(dom: &Dom, page: &Sheet, vp: Viewport) -> Styled {
    let mut styled = Styled {
        styles: vec![Style::default(); dom.len()],
        before: vec![None; dom.len()],
        after: vec![None; dom.len()],
        customs: vec![HashMap::new(); dom.len()],
        default_style: Style::default(),
        revision: dom.revision,
        applied: 0,
    };
    let mut rules: Vec<CascadeRule> = Vec::new();
    let mut order = 0u32;
    for r in ua_sheet().rules.iter() {
        order += 1;
        rules.push(CascadeRule::new(r, 0, order));
    }
    for r in page.rules.iter() {
        order += 1;
        rules.push(CascadeRule::new(r, 1, order));
    }
    rules.retain(|r| r.applies(&vp));
    let index = RuleIndex::build(rules);
    let root_font = 16.0f32;

    // Parents before children: an explicit walk, not arena order.
    let mut cands: Vec<u32> = Vec::new();
    let mut winners: Vec<(String, String, u64)> = Vec::new();
    style_subtree(
        dom,
        &index,
        vp,
        root_font,
        dom.document,
        &mut cands,
        &mut winners,
        &mut styled,
    );
    styled
}

fn style_subtree(
    dom: &Dom,
    index: &RuleIndex<'_>,
    vp: Viewport,
    root_font: f32,
    node: usize,
    cands: &mut Vec<u32>,
    winners: &mut Vec<(String, String, u64)>,
    styled: &mut Styled,
) {
    let kind = dom.node(node).map(|n| n.kind).unwrap_or(Kind::Text);
    let parent = dom.parent(node);
    let parent_style = match parent {
        Some(p) => styled.get(p).clone(),
        None => Style::default(),
    };
    let parent_customs: HashMap<String, String> = match parent {
        Some(p) => styled.customs.get(p).cloned().unwrap_or_default(),
        None => HashMap::new(),
    };
    let mut customs = parent_customs.clone();
    if kind == Kind::Element {
        index.candidates(dom, node, cands);
        winners.clear();
        for &ri in cands.iter() {
            let rule = &index.rules[ri as usize];
            if !rule.set.matches(dom, node) {
                continue;
            }
            let rank = rule.rank(false);
            for (name, value, important) in rule.slots.iter().find(|(w, _)| w.is_none()).map(|x| &x.1).unwrap() {
                let r = if *important {
                    rule.rank(true)
                } else {
                    rank
                };
                winners.push((name.clone(), value.to_string(), r));
            }
        }
        // style="..." attribute.
        if let Some(text) = dom.attr(node, "style") {
            let decls = parse_style_attribute(&text);
            // Inline style: author origin with maximal specificity.
            let inline_rank = (1u64 << 55) | (0xFFFF_FFu64 << 31);
            for d in &decls {
                let important = d.important();
                let r = if important {
                    (1u64 << 63) | (2u64 << 55) | (0xFFFF_FFu64 << 31)
                } else {
                    inline_rank
                };
                match d {
                    Decl::Prop { name, value, .. } => {
                        winners.push((name.clone(), value.clone(), r))
                    }
                    Decl::Custom { name, value, .. } => {
                        customs.insert(name.clone(), value.clone());
                        styled.customs[node] = customs.clone();
                    }
                }
            }
        }
        // Reduce to one winner per property.
        let mut best: HashMap<&str, (u64, String)> = HashMap::new();
        for (name, value, rank) in winners.iter() {
            match best.get(name.as_str()) {
                Some((r, _)) if *r >= *rank => {}
                _ => {
                    best.insert(name.as_str(), (*rank, value.clone()));
                }
            }
        }
        let mut chosen: Vec<(String, String)> = Vec::with_capacity(best.len());
        for (name, (_, value)) in best {
            let value = if value.contains("var(") {
                resolve_vars(&value, &|n| customs.get(n).cloned())
            } else {
                value
            };
            if name.starts_with("--") {
                customs.insert(name.to_string(), value);
                continue;
            }
            chosen.push((name.to_string(), value));
        }
        styled.customs[node] = customs.clone();
        let style = compute(&chosen, &parent_style, root_font, vp, &mut styled.applied);
        styled.styles[node] = style.clone();
        // Pseudo elements inherit from their originating element.
        for which in ["before", "after"] {
            let mut pw: Vec<(String, String, u64)> = Vec::new();
            for &ri in cands.iter() {
                let rule = &index.rules[ri as usize];
                if !rule.has_decls(Some(which)) || !rule.set.matches(dom, node) {
                    continue;
                }
                if let Some((_, list)) = rule.slots.iter().find(|(w, _)| *w == Some(which)) {
                    for (name, value, important) in list {
                        let r = rule.rank(*important);
                        pw.push((name.clone(), value.to_string(), r));
                    }
                }
            }
            if pw.is_empty() {
                continue;
            }
            let mut best: HashMap<&str, (u64, String)> = HashMap::new();
            for (name, value, rank) in pw.iter() {
                match best.get(name.as_str()) {
                    Some((r, _)) if *r >= *rank => {}
                    _ => {
                        best.insert(name.as_str(), (*rank, value.clone()));
                    }
                }
            }
            let chosen: Vec<(String, String)> = best
                .into_iter()
                .map(|(n, (_, v))| {
                    (
                        n.to_string(),
                        if v.contains("var(") {
                            resolve_vars(&v, &|x| customs.get(x).cloned())
                        } else {
                            v
                        },
                    )
                })
                .collect();
            let content = chosen
                .iter()
                .find(|(n, _)| n == "content")
                .map(|(_, v)| parse_content(v))
                .unwrap_or(Content::None);
            let ps = compute(&chosen, &style, root_font, vp, &mut styled.applied);
            let slot = if which == "before" {
                &mut styled.before[node]
            } else {
                &mut styled.after[node]
            };
            *slot = Some(PseudoBox {
                content,
                style: ps,
            });
        }
    } else if kind == Kind::Text {
        styled.styles[node] = parent_style.clone();
        styled.customs[node] = customs;
    }
    // Children, in document order.
    for child in dom.children(node) {
        style_subtree(dom, index, vp, root_font, child, cands, winners, styled);
    }
}

// ---------------------------------------------------------------------------
// value computation
// ---------------------------------------------------------------------------

/// Turn winning longhand declarations into a computed style.
pub fn compute(
    decls: &[(String, String)],
    parent: &Style,
    root_font: f32,
    vp: Viewport,
    applied: &mut usize,
) -> Style {
    let mut s = Style::inherit_from(parent);
    let parent_fs = parent.font_size.max(1.0);

    // font-size first: every other em-relative value uses the new size.
    let mut line_height_raw: Option<String> = None;
    let mut deferred: Vec<(&str, &str)> = Vec::with_capacity(decls.len());
    for (name, value) in decls {
        match name.as_str() {
            "font-size" => {
                s.font_size = font_size(value, parent_fs, root_font, vp).max(1.0);
            }
            "font-family" => {
                s.font_family = first_family(value);
                s.font_monospace = s.font_family.contains("monospace");
            }
            "font-weight" => s.font_weight = parse_weight(value),
            "font-style" => s.font_italic = value.eq_ignore_ascii_case("italic"),
            "font-variant" | "font-variant-caps" => {
                s.text_transform_capitalize |= value.contains("small-caps")
            }
            "line-height" => line_height_raw = Some(value.clone()),
            _ => deferred.push((name.as_str(), value.as_str())),
        }
    }
    match line_height_raw.as_deref() {
        None => {
            s.line_height = s.font_size * 1.15;
            s.line_height_normal = true;
        }
        Some(v) => {
            s.line_height_normal = v.eq_ignore_ascii_case("normal");
            s.line_height = resolve_line_height(v, s.font_size, root_font, vp);
        }
    }
    let fs = s.font_size;
    for (name, value) in deferred {
        *applied += 1;
        apply(&mut s, name, value, fs, root_font, vp);
    }
    s
}

fn len_px(v: &str, fs: f32, root_font: f32, vp: Viewport) -> f32 {
    match Length::parse(v) {
        Some(l) => l.resolve(fs, fs, root_font, vp.width, vp.height),
        None => 0.0,
    }
}

fn keep_len(v: &str, fs: f32, root_font: f32, vp: Viewport) -> Length {
    match Length::parse(v) {
        // Resolve font-relative units eagerly; percentages and viewport units
        // stay symbolic so layout can apply the real containing block.
        Some(Length::Em(x)) => Length::Px(x * fs),
        Some(Length::Rem(x)) => Length::Px(x * root_font),
        Some(Length::Vw(x)) => Length::Px(x * vp.width / 100.0),
        Some(Length::Vh(x)) => Length::Px(x * vp.height / 100.0),
        Some(Length::Ch(x)) => Length::Px(x * fs * 0.5),
        Some(other) => other,
        None => Length::Auto,
    }
}

fn font_size(v: &str, parent_fs: f32, root_font: f32, vp: Viewport) -> f32 {
    let t = v.trim().to_ascii_lowercase();
    let kw = match t.as_str() {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 22.0,
        "xx-large" => 26.0,
        "xxx-large" => 32.0,
        "larger" => return parent_fs * 1.2,
        "smaller" => return parent_fs / 1.2,
        _ => 0.0,
    };
    if kw > 0.0 {
        return kw;
    }
    match Length::parse(&t) {
        Some(Length::Pct(p)) => parent_fs * p / 100.0,
        Some(Length::Em(e)) => parent_fs * e,
        Some(Length::Rem(r)) => root_font * r,
        Some(l) => l.resolve(parent_fs, parent_fs, root_font, vp.width, vp.height),
        None => parent_fs,
    }
}

fn resolve_line_height(v: &str, fs: f32, root_font: f32, vp: Viewport) -> f32 {
    let t = v.trim();
    if let Ok(num) = t.parse::<f32>() {
        return num * fs;
    }
    if t.ends_with('%') {
        return fs * parse_float(t).unwrap_or(100.0) / 100.0;
    }
    len_px(t, fs, root_font, vp).max(1.0)
}

fn first_family(v: &str) -> String {
    split_top_sep(v, ',')
        .into_iter()
        .next()
        .map(|f| f.trim_matches('"').trim_matches('\'').trim().to_string())
        .unwrap_or_default()
}

fn parse_weight(v: &str) -> u16 {
    let t = v.trim().to_ascii_lowercase();
    match t.as_str() {
        "bold" => 700,
        "bolder" => 700,
        "lighter" => 300,
        "normal" => 400,
        _ => t.parse().unwrap_or(400),
    }
}

fn parse_display(v: &str) -> Option<Display> {
    let t = v.trim().to_ascii_lowercase();
    let t = t.split_whitespace().next().unwrap_or("");
    match t {
        "none" => Some(Display::None),
        "inline" => Some(Display::Inline),
        "block" => Some(Display::Block),
        "inline-block" => Some(Display::InlineBlock),
        "flex" | "-webkit-flex" => Some(Display::Flex),
        "inline-flex" => Some(Display::InlineFlex),
        "list-item" => Some(Display::ListItem),
        "table" => Some(Display::Table),
        "table-row" => Some(Display::TableRow),
        "table-cell" => Some(Display::TableCell),
        "table-row-group" | "table-header-group" | "table-footer-group" => {
            Some(Display::TableGroup)
        }
        "contents" => Some(Display::Contents),
        _ => None,
    }
}

fn parse_image(v: &str) -> ImageValue {
    let t = v.trim();
    if t.eq_ignore_ascii_case("none") {
        return ImageValue::None;
    }
    if let Some(rest) = strip_fn(t, "url") {
        let u = rest.trim().trim_matches('"').trim_matches('\'');
        return ImageValue::Url(u.to_string());
    }
    if let Some(rest) = strip_fn(t, "linear-gradient") {
        return parse_linear(rest, false);
    }
    if let Some(rest) = strip_fn(t, "repeating-linear-gradient") {
        return parse_linear(rest, true);
    }
    if let Some(rest) = strip_fn(t, "radial-gradient") {
        let (_, stops) = match rest.split_once(',') {
            Some((a, b)) => (a, b),
            None => ("", rest),
        };
        return ImageValue::Radial {
            stops: parse_stops(stops),
            circle: t.contains("circle"),
        };
    }
    if let Some(rest) = strip_fn(t, "-webkit-linear-gradient") {
        return parse_linear(rest, false);
    }
    if let Some(c) = parse_color(t) {
        return ImageValue::Solid(c);
    }
    ImageValue::None
}

fn strip_fn<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let lower = text.to_ascii_lowercase();
    let prefix = format!("{name}(");
    let idx = lower.find(&prefix)?;
    // Only if it starts the string (otherwise it is an unrelated token).
    if idx != 0 {
        return None;
    }
    let end = lower.rfind(')')?;
    if end <= idx + prefix.len() {
        return None;
    }
    Some(&text[idx + prefix.len()..end])
}

fn parse_linear(inner: &str, _repeating: bool) -> ImageValue {
    let parts = split_top_sep(inner, ',');
    let mut angle = 180.0f32; // `to bottom`
    let mut i = 0usize;
    if let Some(first) = parts.first() {
        let f = first.trim();
        if f.starts_with("to ") || parse_angle(f).is_some() {
            i = 1;
            if let Some(a) = parse_angle(f) {
                angle = a;
            } else if let Some(dir) = f.strip_prefix("to ") {
                angle = match dir.trim() {
                    "top" => 0.0,
                    "right" => 90.0,
                    "bottom" => 180.0,
                    "left" => 270.0,
                    "top right" | "right top" => 45.0,
                    "bottom right" | "right bottom" => 135.0,
                    "bottom left" | "left bottom" => 225.0,
                    "top left" | "left top" => 315.0,
                    _ => 180.0,
                };
            }
        }
    }
    let stops: Vec<(f32, Color)> = parse_stops(&parts[i..].join(","));
    if stops.len() < 2 {
        if let Some((_, c)) = stops.first() {
            return ImageValue::Solid(*c);
        }
        return ImageValue::None;
    }
    ImageValue::Linear { angle, stops }
}

fn parse_stops(text: &str) -> Vec<(f32, Color)> {
    let mut out = Vec::new();
    let parts = split_top_sep(text, ',');
    for (i, p) in parts.iter().enumerate() {
        let p = p.trim();
        let toks = split_top_sep(p, ' ');
        let (color_part, pos) = if toks.len() >= 2 {
            let r = toks[1].trim();
            let pos = if r.ends_with('%') {
                Some(parse_float(r).unwrap_or(0.0) / 100.0)
            } else {
                None
            };
            (toks[0].as_str(), pos)
        } else {
            (p, None)
        };
        if let Some(c) = parse_color(color_part.trim()) {
            let n = parts.len().max(2) - 1;
            out.push((pos.unwrap_or(i as f32 / n as f32), c));
        }
    }
    out
}

fn parse_shadows(v: &str) -> Vec<Shadow> {
    let mut out = Vec::new();
    for layer in split_top_sep(v, ',') {
        let mut sh = Shadow {
            inset: false,
            x: 0.0,
            y: 0.0,
            blur: 0.0,
            spread: 0.0,
            color: Color::BLACK,
        };
        let mut nums: Vec<f32> = Vec::new();
        let mut color: Option<Color> = None;
        let mut inset = false;
        for tok in split_top_sep(&layer, ' ') {
            let t = tok.trim();
            if t.eq_ignore_ascii_case("inset") {
                inset = true;
                continue;
            }
            if let Some(c) = parse_color(t) {
                color = Some(c);
                continue;
            }
            if let Some(l) = Length::parse(t) {
                nums.push(match l {
                    Length::Px(p) => p,
                    Length::Zero => 0.0,
                    other => other.resolve(16.0, 16.0, 16.0, 0.0, 0.0),
                });
                continue;
            }
            if let Ok(n) = t.parse::<f32>() {
                nums.push(n);
            }
        }
        if nums.is_empty() {
            continue;
        }
        sh.x = nums[0];
        sh.y = nums.get(1).copied().unwrap_or(0.0);
        sh.blur = nums.get(2).copied().unwrap_or(0.0);
        sh.spread = nums.get(3).copied().unwrap_or(0.0);
        sh.color = color.unwrap_or_else(|| Color::rgba(0, 0, 0, 160));
        sh.inset = inset;
        out.push(sh);
    }
    out
}

fn parse_transform(v: &str) -> Option<Matrix> {
    let mut m = Matrix::identity();
    let mut any = false;
    for part in split_top_sep(v, ' ') {
        let p = part.trim();
        for (name, inner) in [
            "translate",
            "translatex",
            "translatey",
            "translate3d",
            "scale",
            "scalex",
            "scaley",
            "rotate",
            "skew",
            "matrix",
            "matrix3d",
        ]
        .iter()
        .filter_map(|n| strip_fn(p, n).map(|args| (*n, args)))
        {
            let args: Vec<&str> = split_top_sep(inner, ',').iter().map(|s| s.as_str()).collect();
            let num = |i: usize| -> f32 {
                args.get(i)
                    .and_then(|a| Length::parse(a))
                    .map(|l| l.resolve(16.0, 16.0, 16.0, 0.0, 0.0))
                    .unwrap_or(0.0)
            };
            let raw = |i: usize| -> f32 {
                args.get(i)
                    .and_then(|a| parse_float(a))
                    .unwrap_or(if i == 0 { 1.0 } else { 0.0 })
            };
            any = true;
            match name {
                "translate" => m = m.mul(&Matrix::translate(num(0), args.get(1).map(|_| num(1)).unwrap_or(0.0))),
                "translatex" => m = m.mul(&Matrix::translate(num(0), 0.0)),
                "translatey" => m = m.mul(&Matrix::translate(0.0, num(0))),
                "translate3d" => m = m.mul(&Matrix::translate(num(0), num(1))),
                "scale" => {
                    let sx = raw(0);
                    let sy = args.get(1).map(|_| raw(1)).unwrap_or(sx);
                    m = m.mul(&Matrix {
                        a: sx,
                        b: 0.0,
                        c: 0.0,
                        d: sy,
                        e: 0.0,
                        f: 0.0,
                    });
                }
                "scalex" => m = m.mul(&Matrix {
                    a: raw(0),
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: 0.0,
                    f: 0.0,
                }),
                "scaley" => m = m.mul(&Matrix {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: raw(0),
                    e: 0.0,
                    f: 0.0,
                }),
                "rotate" => {
                    let rad = parse_angle(args.first().copied().unwrap_or("0")).unwrap_or(0.0)
                        * std::f32::consts::PI
                        / 180.0;
                    m = m.mul(&Matrix {
                        a: rad.cos(),
                        b: rad.sin(),
                        c: -rad.sin(),
                        d: rad.cos(),
                        e: 0.0,
                        f: 0.0,
                    });
                }
                "skew" => {
                    let ax = parse_angle(args.first().copied().unwrap_or("0")).unwrap_or(0.0);
                    let ay = parse_angle(args.get(1).copied().unwrap_or("0")).unwrap_or(0.0);
                    m = m.mul(&Matrix {
                        a: 1.0,
                        b: (ay * std::f32::consts::PI / 180.0).tan(),
                        c: (ax * std::f32::consts::PI / 180.0).tan(),
                        d: 1.0,
                        e: 0.0,
                        f: 0.0,
                    });
                }
                "matrix" => {
                    if args.len() >= 6 {
                        m = m.mul(&Matrix {
                            a: parse_float(args[0]).unwrap_or(1.0),
                            b: parse_float(args[1]).unwrap_or(0.0),
                            c: parse_float(args[2]).unwrap_or(0.0),
                            d: parse_float(args[3]).unwrap_or(1.0),
                            e: num(4),
                            f: num(5),
                        });
                    }
                }
                _ => {
                    // matrix3d: take the 2D projection.
                    if args.len() >= 14 {
                        m = m.mul(&Matrix {
                            a: parse_float(args[0]).unwrap_or(1.0),
                            b: parse_float(args[1]).unwrap_or(0.0),
                            c: parse_float(args[4]).unwrap_or(0.0),
                            d: parse_float(args[5]).unwrap_or(1.0),
                            e: parse_float(args[12]).unwrap_or(0.0),
                            f: parse_float(args[13]).unwrap_or(0.0),
                        });
                    }
                }
            }
        }
    }
    if any {
        Some(m)
    } else {
        None
    }
}

fn parse_content(v: &str) -> Content {
    let t = v.trim();
    if t.eq_ignore_ascii_case("none") || t.eq_ignore_ascii_case("normal") || t.is_empty() {
        return Content::None;
    }
    let mut runs = Vec::new();
    let mut chars = t.chars().peekable();
    let mut pending = String::new();
    let mut flush = |pending: &mut String, runs: &mut Vec<ContentRun>| {
        let s = pending.trim();
        if !s.is_empty() {
            for tok in split_top_sep(s, ' ') {
                let tok = tok.trim();
                if tok.is_empty() {
                    continue;
                }
                if let Some(rest) = strip_fn(tok, "attr") {
                    runs.push(ContentRun::Attr(rest.trim().to_string()));
                } else if let Some(rest) = strip_fn(tok, "url") {
                    runs.push(ContentRun::Url(
                        rest.trim().trim_matches('"').trim_matches('\'').to_string(),
                    ));
                } else if let Some(rest) = strip_fn(tok, "counter") {
                    runs.push(ContentRun::Counter(rest.trim().to_string()));
                } else if tok.starts_with('"') || tok.starts_with('\'') {
                    runs.push(ContentRun::Text(
                        tok.trim_matches('"').trim_matches('\'').to_string(),
                    ));
                } else if tok.eq_ignore_ascii_case("open-quote") {
                    runs.push(ContentRun::Text("\u{201c}".to_string()));
                } else if tok.eq_ignore_ascii_case("close-quote") {
                    runs.push(ContentRun::Text("\u{201d}".to_string()));
                } else {
                    runs.push(ContentRun::Text(tok.to_string()));
                }
            }
        }
        pending.clear();
    };
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            // Copy quoted strings verbatim so the split below sees them as one run.
            pending.push(c);
            while let Some(&n) = chars.peek() {
                chars.next();
                pending.push(n);
                if n == c {
                    break;
                }
            }
        } else {
            pending.push(c);
        }
    }
    flush(&mut pending, &mut runs);
    if runs.is_empty() {
        Content::None
    } else {
        Content::Runs(runs)
    }
}

/// Apply one longhand to the style being computed.
fn apply(s: &mut Style, name: &str, value: &str, fs: f32, root_font: f32, vp: Viewport) {
    let v = value.trim();
    let l = |v: &str| len_px(v, fs, root_font, vp);
    let k = |v: &str| keep_len(v, fs, root_font, vp);
    match name {
        "display" => {
            if let Some(d) = parse_display(v) {
                s.display = d;
            }
        }
        "position" => {
            s.position = match v {
                "relative" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "sticky" => Position::Sticky,
                _ => Position::Static,
            };
        }
        "float" => {
            s.float = match v {
                "left" => Float::Left,
                "right" => Float::Right,
                _ => Float::None,
            };
        }
        "clear" => s.clear = v != "none",
        "box-sizing" => s.box_sizing_border = v == "border-box",
        "width" => s.width = k(v),
        "height" => s.height = k(v),
        "min-width" => s.min_width = k(v),
        "min-height" => s.min_height = k(v),
        "max-width" => s.max_width = k(v),
        "max-height" => s.max_height = k(v),
        "top" => s.inset[0] = k(v),
        "right" => s.inset[1] = k(v),
        "bottom" => s.inset[2] = k(v),
        "left" => s.inset[3] = k(v),
        "margin-top" => s.margin[0] = k(v),
        "margin-right" => s.margin[1] = k(v),
        "margin-bottom" => s.margin[2] = k(v),
        "margin-left" => s.margin[3] = k(v),
        "padding-top" => s.padding[0] = k(v),
        "padding-right" => s.padding[1] = k(v),
        "padding-bottom" => s.padding[2] = k(v),
        "padding-left" => s.padding[3] = k(v),
        "border-top-width" => s.border_width[0] = side_width(v, s.border_style[0], fs),
        "border-right-width" => s.border_width[1] = side_width(v, s.border_style[1], fs),
        "border-bottom-width" => s.border_width[2] = side_width(v, s.border_style[2], fs),
        "border-left-width" => s.border_width[3] = side_width(v, s.border_style[3], fs),
        "border-top-style" => s.border_style[0] = parse_border_style(v),
        "border-right-style" => s.border_style[1] = parse_border_style(v),
        "border-bottom-style" => s.border_style[2] = parse_border_style(v),
        "border-left-style" => s.border_style[3] = parse_border_style(v),
        "border-top-color" => s.border_color[0] = parse_color(v).unwrap_or(s.border_color[0]),
        "border-right-color" => s.border_color[1] = parse_color(v).unwrap_or(s.border_color[1]),
        "border-bottom-color" => s.border_color[2] = parse_color(v).unwrap_or(s.border_color[2]),
        "border-left-color" => s.border_color[3] = parse_color(v).unwrap_or(s.border_color[3]),
        "border-top-left-radius" => s.radius[0] = parse_radius(v, fs, root_font, vp, 0.0),
        "border-top-right-radius" => s.radius[1] = parse_radius(v, fs, root_font, vp, 0.0),
        "border-bottom-right-radius" => s.radius[2] = parse_radius(v, fs, root_font, vp, 0.0),
        "border-bottom-left-radius" => s.radius[3] = parse_radius(v, fs, root_font, vp, 0.0),
        "color" => s.color = parse_color(v).unwrap_or(s.color),
        "opacity" => s.opacity = parse_float(v).unwrap_or(1.0).clamp(0.0, 1.0),
        "visibility" => s.visibility = v == "visible",
        "background-color" => s.background_color = parse_color(v).unwrap_or(Color::TRANSPARENT),
        "background-image" => s.background_image = parse_image(v),
        "background-repeat" => {
            s.background_repeat = match v.split_whitespace().next().unwrap_or("") {
                "no-repeat" => BackgroundRepeat::NoRepeat,
                "repeat-x" => BackgroundRepeat::RepeatX,
                "repeat-y" => BackgroundRepeat::RepeatY,
                _ => BackgroundRepeat::Repeat,
            }
        }
        "background-size" => {
            s.background_size = match v {
                "cover" => BackgroundSize::Cover,
                "contain" => BackgroundSize::Contain,
                "auto" => BackgroundSize::Auto,
                _ => {
                    let parts = split_top_sep(v, ' ');
                    let a = parts
                        .first()
                        .map(|s| k(s))
                        .unwrap_or(Length::Auto);
                    let b = parts
                        .get(1)
                        .map(|s| k(s))
                        .unwrap_or(Length::Auto);
                    BackgroundSize::Cols(a, b)
                }
            }
        }
        "background-position" => {
            let parts = split_top_sep(v, ' ');
            let mut x = 0.0f32;
            let mut y = 0.0f32;
            let mut idx = 0;
            for p in parts.iter() {
                match p.as_str() {
                    "left" => {
                        x = 0.0;
                        idx += 1;
                    }
                    "right" => {
                        x = 100.0;
                        idx += 1;
                    }
                    "center" => {
                        if idx == 0 {
                            x = 50.0;
                        } else {
                            y = 50.0;
                        }
                        idx += 1;
                    }
                    "top" => {
                        y = 0.0;
                        idx += 1;
                    }
                    "bottom" => {
                        y = 100.0;
                        idx += 1;
                    }
                    other => {
                        let val = Length::parse(other)
                            .map(|ll| ll.resolve(100.0, fs, root_font, vp.width, vp.height))
                            .unwrap_or(0.0);
                        if idx == 0 {
                            x = val;
                        } else {
                            y = val;
                        }
                        idx += 1;
                    }
                }
            }
            s.background_pos_x = x;
            s.background_pos_y = y;
        }
        "text-align" => {
            s.text_align = match v {
                "left" => TextAlign::Left,
                "right" => TextAlign::Right,
                "center" => TextAlign::Center,
                "justify" => TextAlign::Justify,
                "end" => TextAlign::End,
                _ => TextAlign::Start,
            }
        }
        "text-indent" => s.text_indent = l(v),
        "white-space" => {
            s.white_space = match v {
                "nowrap" => WhiteSpace::Nowrap,
                "pre" => WhiteSpace::Pre,
                "pre-wrap" => WhiteSpace::PreWrap,
                "pre-line" => WhiteSpace::PreLine,
                "break-spaces" => WhiteSpace::BreakSpaces,
                _ => WhiteSpace::Normal,
            }
        }
        "word-break" => s.word_break = v == "break-all" || v == "break-word",
        "overflow-wrap" | "word-wrap" => s.word_break |= v == "anywhere" || v == "break-word",
        "letter-spacing" => {
            s.letter_spacing = if v == "normal" { 0.0 } else { l(v) }
        }
        "word-spacing" => s.word_spacing = if v == "normal" { 0.0 } else { l(v) },
        "text-decoration-line" => {
            s.text_decoration_underline = v.contains("underline");
            s.text_decoration_line_through = v.contains("line-through");
        }
        "text-decoration" => {
            s.text_decoration_underline = v.contains("underline");
            s.text_decoration_line_through = v.contains("line-through");
        }
        "text-transform" => {
            s.text_transform_upper = v == "uppercase";
            s.text_transform_lower = v == "lowercase";
            s.text_transform_capitalize = v == "capitalize";
        }
        "overflow" => {
            let o = parse_overflow(v);
            s.overflow_x = o;
            s.overflow_y = o;
        }
        "overflow-x" => s.overflow_x = parse_overflow(v),
        "overflow-y" => s.overflow_y = parse_overflow(v),
        "z-index" => {
            s.z_index = if v == "auto" {
                None
            } else {
                Some(l(v) as i32)
            }
        }
        "list-style-type" => s.list_style = v != "none",
        "list-style" => s.list_style = !v.contains("none"),
        "cursor" => s.cursor_pointer = v.starts_with("pointer") || v.starts_with("hand"),
        "user-select" | "-webkit-user-select" => s.user_select_none = v == "none",
        "pointer-events" | "-webkit-pointer-events" => s.pointer_events_none = v == "none",
        "appearance" | "-webkit-appearance" => s.appearance_none = v == "none",
        "object-fit" => s.object_fit_cover = v == "cover" || v == "contain",
        "outline-width" => s.outline_width = l(v),
        "outline-color" => s.outline_color = parse_color(v).unwrap_or(s.outline_color),
        "outline-style" => {
            s.outline_width = if v == "none" { 0.0 } else { s.outline_width.max(1.0) }
        }
        "box-shadow" => s.shadow = parse_shadows(v),
        "text-shadow" => s.shadow = parse_shadows(v),
        "filter" => {
            s.filter_blur = strip_fn(v, "blur")
                .and_then(|a| Length::parse(a.trim()))
                .map(|x| x.resolve(1.0, 1.0, 1.0, 0.0, 0.0))
                .unwrap_or(s.filter_blur);
            s.filter_brightness = strip_fn(v, "brightness")
                .and_then(parse_float)
                .unwrap_or(s.filter_brightness);
            s.filter_invert = v.contains("invert");
        }
        "transform" => {
            if v != "none" {
                if let Some(m) = parse_transform(v) {
                    s.transform = m;
                }
            }
        }
        "transform-origin" => {
            let parts = split_top_sep(v, ' ');
            let pct_or = |p: &str, default: f32| -> f32 {
                if p == "left" || p == "top" {
                    0.0
                } else if p == "right" || p == "bottom" {
                    1.0
                } else if p == "center" {
                    0.5
                } else if let Some(ll) = Length::parse(p) {
                    match ll {
                        Length::Pct(x) => x / 100.0,
                        other => other.resolve(100.0, fs, root_font, vp.width, vp.height) / 100.0,
                    }
                } else {
                    default
                }
            };
            s.transform_origin_x = parts
                .first()
                .map(|p| pct_or(p, 0.5))
                .unwrap_or(0.5);
            s.transform_origin_y = parts
                .get(1)
                .map(|p| pct_or(p, 0.5))
                .unwrap_or(0.5);
        }
        "flex-direction" => {
            s.flex_dir = match v {
                "row-reverse" => FlexDir::RowReverse,
                "column" => FlexDir::Column,
                "column-reverse" => FlexDir::ColumnReverse,
                _ => FlexDir::Row,
            }
        }
        "flex-wrap" => s.flex_wrap = v.starts_with("wrap"),
        "flex-grow" => s.flex_grow = parse_float(v).unwrap_or(0.0).max(0.0),
        "flex-shrink" => s.flex_shrink = parse_float(v).unwrap_or(1.0).max(0.0),
        "flex-basis" => s.flex_basis = k(v),
        "justify-content" => {
            s.justify = match v {
                "center" => Justify::Center,
                "flex-end" | "end" | "right" => Justify::FlexEnd,
                "space-between" => Justify::SpaceBetween,
                "space-around" => Justify::SpaceAround,
                "space-evenly" => Justify::SpaceEvenly,
                _ => Justify::FlexStart,
            }
        }
        "align-items" | "align-content" => {
            let a = match v {
                "center" => crate::css::value::Align::Center,
                "flex-end" | "end" | "self-end" => crate::css::value::Align::FlexEnd,
                "baseline" => crate::css::value::Align::Baseline,
                "stretch" => crate::css::value::Align::Stretch,
                _ => crate::css::value::Align::FlexStart,
            };
            if name == "align-items" {
                s.align_items = a;
            }
        }
        "align-self" => {
            s.align_self = match v {
                "auto" => crate::css::value::Align::Auto,
                "center" => crate::css::value::Align::Center,
                "flex-end" | "end" => crate::css::value::Align::FlexEnd,
                "stretch" => crate::css::value::Align::Stretch,
                _ => crate::css::value::Align::Baseline,
            }
        }
        "row-gap" => s.row_gap = l(v).max(0.0),
        "column-gap" => s.col_gap = l(v).max(0.0),
        "font-size-adjust" | "direction" | "unicode-bidi" | "transition" | "transition-property"
        | "animation" | "animation-name" | "will-change" | "backface-visibility"
        | "text-rendering" | "image-rendering" | "content" | "quotes" | "counter-increment"
        | "counter-reset" | "table-layout" | "border-collapse" | "border-spacing"
        | "-webkit-font-smoothing" | "touch-action" | "scroll-behavior" | "contain"
        | "content-visibility" | "isolation" | "mix-blend-mode" | "clip-path" | "clip"
        | "text-overflow" | "-webkit-line-clamp" | "line-clamp" | "vertical-align"
        | "caption-side" | "empty-cells" | "font-stretch" | "writing-mode" | "text-orientation"
        | "background" | "background-attachment" | "background-origin" | "background-clip"
        | "border" | "border-color" | "border-style" | "border-width" | "border-radius"
        | "margin" | "padding" | "inset" | "flex" | "flex-flow" | "gap" | "grid" | "grid-template"
        | "grid-template-columns" | "grid-template-rows" | "grid-auto-flow" | "grid-column"
        | "grid-row" | "outline" | "outline-offset" | "text-decoration-style"
        | "text-decoration-color" | "text-decoration-thickness" | "list-style-position"
        | "list-style-image" => {
            // Shorthands are expanded before we get here, and the rest are
            // accepted but not modelled; keeping them silent avoids noisy errors.
            let _ = (v, l, k);
        }
        _ => {
            let _ = (v, l, k);
        }
    }
}

fn side_width(v: &str, style: BorderStyle, fs: f32) -> f32 {
    if v.eq_ignore_ascii_case("thin") {
        1.0
    } else if v.eq_ignore_ascii_case("medium") {
        3.0
    } else if v.eq_ignore_ascii_case("thick") {
        5.0
    } else if style.paints() {
        Length::parse(v).map(|l| l.resolve(fs, fs, fs, 0.0, 0.0)).unwrap_or(0.0).max(0.0)
    } else {
        0.0
    }
}

fn parse_border_style(v: &str) -> BorderStyle {
    match v.trim() {
        "solid" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        "groove" => BorderStyle::Groove,
        "ridge" => BorderStyle::Ridge,
        "inset" => BorderStyle::Inset,
        "outset" => BorderStyle::Outset,
        "hidden" => BorderStyle::Hidden,
        _ => BorderStyle::None,
    }
}

fn parse_overflow(v: &str) -> Overflow {
    match v.trim() {
        "hidden" | "clip" => Overflow::Hidden,
        "auto" => Overflow::Auto,
        "scroll" => Overflow::Scroll,
        _ => Overflow::Visible,
    }
}

fn parse_radius(v: &str, fs: f32, root_font: f32, vp: Viewport, _default: f32) -> f32 {
    // `border-radius: 10px / 5px` keeps the horizontal component.
    let h = split_top_sep(v, '/').into_iter().next().unwrap_or_else(|| v.to_string());
    let first = split_top_sep(&h, ' ')
        .into_iter()
        .next()
        .unwrap_or_else(|| "0".to_string());
    let pct = first.ends_with('%');
    let px = Length::parse(&first)
        .map(|l| l.resolve(if pct { 100.0 } else { fs }, fs, root_font, vp.width, vp.height))
        .unwrap_or(0.0);
    if pct {
        px
    } else {
        px.max(0.0)
    }
}

// ---------------------------------------------------------------------------
// sheets
// ---------------------------------------------------------------------------

/// Parse `UA_CSS` once per thread and keep it alive: the cascade only borrows it.
fn ua_sheet() -> &'static Sheet {
    static SHEET: std::sync::OnceLock<Sheet> = std::sync::OnceLock::new();
    SHEET.get_or_init(|| crate::css::parse::parse_sheet(UA_CSS))
}

/// The UA stylesheet. Kept in sync with docs/STATUS.md.
pub const UA_CSS: &str = r#"
html { display: block; font-family: sans-serif; font-size: 16px; color: rgb(0,0,0);
       background-color: rgb(255,255,255); }
head, style, script, link, meta, title, base, template { display: none; }
body { display: block; margin: 8px; }
p, div, section, article, aside, main, header, footer, nav, address, blockquote, figure,
figcaption, details, dialog, form, fieldset, legend, dl, ul, ol, li, pre, hr, table,
h1, h2, h3, h4, h5, h6, center, form { display: block; }
li { display: list-item; }
h1 { font-size: 2em; margin: 0.67em 0; font-weight: bold; }
h2 { font-size: 1.5em; margin: 0.83em 0; font-weight: bold; }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold; }
h4 { margin: 1.33em 0; font-weight: bold; }
h5 { font-size: 0.83em; margin: 1.67em 0; font-weight: bold; }
h6 { font-size: 0.67em; margin: 2.33em 0; font-weight: bold; }
b, strong { font-weight: bold; }
i, em, cite, dfn, var { font-style: italic; }
u, ins { text-decoration: underline; }
s, strike, del { text-decoration: line-through; }
mark { background-color: rgb(255,255,0); }
small { font-size: 0.83em; }
big { font-size: 1.17em; }
code, kbd, samp, tt, pre { font-family: monospace; font-size: 13px; }
pre { white-space: pre; }
a { color: rgb(0,0,238); text-decoration: underline; cursor: pointer; }
a:visited { color: rgb(85,26,139); }
a:not([href]) { color: inherit; text-decoration: none; cursor: auto; }
blockquote { margin: 1em 40px; }
ul, menu, dir { margin: 1em 0; padding-left: 40px; list-style-type: disc; }
ol { margin: 1em 0; padding-left: 40px; list-style-type: decimal; }
ul ul, ol ul { list-style-type: circle; }
ul ul ul, ol ol ul { list-style-type: square; }
li ul, li ol { margin: 0; }
dd { margin-left: 40px; }
hr { border-style: inset; border-width: 1px; margin: 0.5em auto; }
br { display: inline; }
table { border-collapse: separate; }
th { font-weight: bold; text-align: center; }
td, th { padding: 1px; }
caption { text-align: center; }
thead, tbody, tfoot { display: table-row-group; }
tr { display: table-row; }
td, th { display: table-cell; }
img { display: inline-block; }
input, button, textarea, select { display: inline-block; }
textarea { white-space: pre-wrap; }
button { text-align: center; }
fieldset { border-style: groove; border-width: 2px; padding: 0.35em 0.625em 0.75em; }
iframe { border-style: inset; border-width: 2px; }
[hidden] { display: none; }
[align=center], center { text-align: center; }
[align=right] { text-align: right; }
[align=left] { text-align: left; }
marquee, video, canvas, svg { display: inline-block; }
sup { font-size: 0.7em; }
sub { font-size: 0.7em; }
"#;

/// `getComputedStyle` support: resolve a single property's used value.
pub fn computed_property(style: &Style, name: &str, containing: f32) -> String {
    let n = name.replace('_', "-").to_ascii_lowercase();
    let fs = style.font_size;
    let fmt = |l: Length| -> String {
        match l {
            Length::Auto => "auto".to_string(),
            other => format!("{}px", other.resolve(containing, fs, fs, 0.0, 0.0)),
        }
    };
    match n.as_str() {
        "display" => style.display.as_str().to_string(),
        "position" => format!("{:?}", style.position).to_ascii_lowercase(),
        "color" => style.color.to_css(),
        "background-color" => style.background_color.to_css(),
        "font-size" => format!("{}px", fs),
        "font-weight" => style.font_weight.to_string(),
        "font-family" => style.font_family.clone(),
        "line-height" => format!("{}px", style.line_height),
        "text-align" => format!("{:?}", style.text_align).to_ascii_lowercase(),
        "width" => fmt(style.width.clone()),
        "height" => fmt(style.height.clone()),
        "margin-top" => fmt(style.margin[0].clone()),
        "margin-right" => fmt(style.margin[1].clone()),
        "margin-bottom" => fmt(style.margin[2].clone()),
        "margin-left" => fmt(style.margin[3].clone()),
        "padding-top" => fmt(style.padding[0].clone()),
        "padding-right" => fmt(style.padding[1].clone()),
        "padding-bottom" => fmt(style.padding[2].clone()),
        "padding-left" => fmt(style.padding[3].clone()),
        "opacity" => style.opacity.to_string(),
        "visibility" => {
            if style.visibility {
                "visible".to_string()
            } else {
                "hidden".to_string()
            }
        }
        "overflow-x" => format!("{:?}", style.overflow_x).to_ascii_lowercase(),
        "overflow-y" => format!("{:?}", style.overflow_y).to_ascii_lowercase(),
        "z-index" => style
            .z_index
            .map(|z| z.to_string())
            .unwrap_or_else(|| "auto".to_string()),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Dom;

    fn styled(html: &str, css: &str) -> (Dom, Styled) {
        let mut d = Dom::new();
        crate::html::parse::parse_document(&mut d, html);
        let sheet = crate::css::parse::parse_sheet(css);
        let s = style_tree(&d, &sheet, Viewport::default());
        (d, s)
    }

    fn find(d: &Dom, sel: &str) -> usize {
        d.query_selector(sel).expect("selector matched nothing")
    }

    #[test]
    fn author_beats_ua_and_later_wins() {
        let (d, s) = styled("<p>x</p>", "p { color: rgb(1,2,3) } p { color: blue }");
        let p = find(&d, "p");
        assert_eq!(s.get(p).color, Color::parse("blue").unwrap());
    }

    #[test]
    fn specificity_and_important() {
        let (d, s) = styled(
            "<div id=a class=b><p>x</p></div>",
            "p { color: red } .b p { color: green } #a p { color: blue }",
        );
        let p = find(&d, "p");
        assert_eq!(s.get(p).color, Color::parse("blue").unwrap());
        let (d2, s2) = styled(
            "<div id=a><p>x</p></div>",
            "p { color: red !important } #a p { color: blue }",
        );
        assert_eq!(s2.get(find(&d2, "p")).color, Color::parse("red").unwrap());
    }

    #[test]
    fn inline_style_wins_over_rules_but_not_important() {
        let (d, s) = styled(r#"<p style="color: lime">x</p>"#, "p { color: red }");
        assert_eq!(s.get(find(&d, "p")).color, Color::parse("lime").unwrap());
        let (d2, s2) = styled(
            r#"<p style="color: lime">x</p>"#,
            "p { color: red !important }",
        );
        assert_eq!(s2.get(find(&d2, "p")).color, Color::parse("red").unwrap());
    }

    #[test]
    fn inheritance_and_reset() {
        let (d, s) = styled("<div style=\"color: red\"><span><b>x</b></span></div>", "");
        let b = find(&d, "b");
        assert_eq!(s.get(b).color, Color::parse("red").unwrap());
        // Non-inherited properties reset: div is block, b must be inline.
        assert_eq!(s.get(b).display, Display::Inline);
    }

    #[test]
    fn font_size_units() {
        let (d, s) = styled(
            "<div style=\"font-size:20px\"><p style=\"font-size:1.5em\">a</p><p style=\"font-size:2rem\">b</p></div>",
            "",
        );
        let ps = d.query_selector_all("p").unwrap();
        assert_eq!(s.get(ps[0]).font_size, 30.0);
        assert_eq!(s.get(ps[1]).font_size, 32.0);
    }

    #[test]
    fn ua_defaults_apply() {
        let (d, s) = styled("<h1>t</h1><ul><li>i</li></ul><table><tr><td>c</td></tr></table>", "");
        assert_eq!(s.get(find(&d, "h1")).font_size, 32.0);
        assert_eq!(s.get(find(&d, "h1")).font_weight, 700);
        let li = find(&d, "li");
        assert_eq!(s.get(li).display, Display::ListItem);
        assert!(s.get(li).list_style);
        assert_eq!(s.get(find(&d, "td")).display, Display::TableCell);
        assert_eq!(s.get(find(&d, "script")).display, Display::None);
    }

    #[test]
    fn hidden_attribute() {
        let (d, s) = styled("<div hidden>x</div>", "");
        assert_eq!(s.get(find(&d, "div")).display, Display::None);
    }

    #[test]
    fn pseudo_element_box() {
        let (d, s) = styled("<p>x</p>", "p::before { content: 'hi'; color: red }");
        let p = find(&d, "p");
        let b = s.pseudo(p, "before").expect("before box");
        assert_eq!(b.content, Content::Runs(vec![ContentRun::Text("hi".into())]));
        assert_eq!(b.style.color, Color::parse("red").unwrap());
    }

    #[test]
    fn media_query_switches_rules() {
        let mut d = Dom::new();
        crate::html::parse::parse_document(&mut d, "<p>x</p>");
        let sheet = crate::css::parse::parse_sheet("p{color:red}@media (max-width: 500px){p{color:blue}}");
        let wide = style_tree(&d, &sheet, Viewport { width: 900.0, ..Default::default() });
        assert_eq!(wide.get(find(&d, "p")).color, Color::parse("red").unwrap());
        let narrow = style_tree(&d, &sheet, Viewport { width: 400.0, ..Default::default() });
        assert_eq!(narrow.get(find(&d, "p")).color, Color::parse("blue").unwrap());
    }

    #[test]
    fn var_lookup_from_ancestor() {
        let (d, s) = styled(
            "<div><p>x</p></div>",
            "div { --c: green } p { color: var(--c) }",
        );
        assert_eq!(s.get(find(&d, "p")).color, Color::parse("green").unwrap());
    }

    #[test]
    fn shorthand_values_survive() {
        let (d, s) = styled(
            "<div style=\"margin:10px; padding:5px 1em; border:2px solid red; border-radius:4px; background:#eee url(a.png) no-repeat\">x</div>",
            "",
        );
        let div = find(&d, "div");
        let st = s.get(div);
        assert_eq!(st.margin[0], Length::Px(10.0));
        assert_eq!(st.padding[1], Length::Px(16.0));
        assert_eq!(st.border_width[0], 2.0);
        assert_eq!(st.border_style[0], BorderStyle::Solid);
        assert_eq!(st.radius[0], 4.0);
        assert_eq!(st.background_image, ImageValue::Url("a.png".into()));
        assert_eq!(st.background_repeat, BackgroundRepeat::NoRepeat);
    }

    #[test]
    fn gradient_and_shadow_parsing() {
        let img = parse_image("linear-gradient(45deg, red, blue 50%, lime)");
        match img {
            ImageValue::Linear { angle, stops } => {
                assert_eq!(angle, 45.0);
                assert_eq!(stops.len(), 3);
                assert_eq!(stops[1].0, 0.5);
            }
            other => panic!("{other:?}"),
        }
        let sh = parse_shadows("1px 2px 3px rgba(0,0,0,0.5), inset 0 0 4px blue");
        assert_eq!(sh.len(), 2);
        assert!(!sh[0].inset);
        assert!(sh[1].inset);
        assert_eq!(sh[0].y, 2.0);
        assert_eq!(sh[0].blur, 3.0);
    }

    #[test]
    fn transform_parsing() {
        let m = parse_transform("translate(10px, 20px) scale(2)").unwrap();
        assert_eq!(m.a, 2.0);
        assert_eq!(m.e, 10.0);
        assert_eq!(m.f, 20.0);
        assert!(parse_transform("none").is_none());
    }

    #[test]
    fn hover_state_flows_through_style() {
        let mut d = Dom::new();
        crate::html::parse::parse_document(&mut d, "<a href=\"#\">x</a>");
        let a = d.query_selector("a").unwrap();
        let sheet = crate::css::parse::parse_sheet("a:hover { color: rgb(9,9,9) }");
        assert_ne!(style_tree(&d, &sheet, Viewport::default()).get(a).color, Color::rgb(9, 9, 9));
        d.set_state(
            a,
            crate::dom::ElementState {
                hovered: true,
                ..Default::default()
            },
        );
        assert_eq!(style_tree(&d, &sheet, Viewport::default()).get(a).color, Color::rgb(9, 9, 9));
    }

    #[test]
    fn focus_and_checked_pseudo_classes() {
        let mut d = Dom::new();
        crate::html::parse::parse_document(
            &mut d,
            "<input type=text><input type=checkbox checked>",
        );
        let sheet =
            crate::css::parse::parse_sheet("input:focus { opacity: 0.5 } input:checked { opacity: 0.25 }");
        let inputs = d.query_selector_all("input").unwrap();
        let s = style_tree(&d, &sheet, Viewport::default());
        assert_eq!(s.get(inputs[1]).opacity, 0.25);
        assert_eq!(s.get(inputs[0]).opacity, 1.0);
        d.set_focus(Some(inputs[0]));
        let s2 = style_tree(&d, &sheet, Viewport::default());
        assert_eq!(s2.get(inputs[0]).opacity, 0.5);
    }
}
