//! CSS stylesheet parsing.
//!
//! Selector + declaration level only: no `@supports`, and `@keyframes` bodies
//! are skipped. `@media` keeps a small condition model that the style step
//! evaluates against the viewport, and shorthands are expanded into longhands
//! right here so that nothing downstream has to know `margin` existed.

use crate::css::selector::SelectorSet;
use crate::css::value::split_top_sep;

#[derive(Clone, Debug, PartialEq)]
pub enum Decl {
    /// `name: value` after shorthand expansion.
    Prop {
        name: String,
        value: String,
        important: bool,
    },
    /// `--custom: value`; resolved later through `var()`.
    Custom {
        name: String,
        value: String,
        important: bool,
    },
}

impl Decl {
    pub fn name(&self) -> &str {
        match self {
            Decl::Prop { name, .. } | Decl::Custom { name, .. } => name,
        }
    }
    pub fn value(&self) -> &str {
        match self {
            Decl::Prop { value, .. } | Decl::Custom { value, .. } => value,
        }
    }
    pub fn important(&self) -> bool {
        match self {
            Decl::Prop { important, .. } | Decl::Custom { important, .. } => *important,
        }
    }
}

/// The parts of `@media` we understand: viewport size, orientation, dark mode.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    pub min_width: Option<f32>,
    pub max_width: Option<f32>,
    pub min_height: Option<f32>,
    pub max_height: Option<f32>,
    pub orientation_landscape: Option<bool>,
    pub dark: Option<bool>,
    /// `@media print` and friends: only `screen`/`all` render.
    pub type_: Option<String>,
}

impl Media {
    pub fn matches(&self, vw: f32, vh: f32, dark: bool) -> bool {
        if let Some(t) = &self.type_ {
            if t != "all" && t != "screen" && t != "only screen" {
                return false;
            }
        }
        let ok = |a: Option<f32>, b: Option<f32>, le: bool| match (a, b) {
            (Some(x), Some(y)) => {
                if le {
                    x <= y
                } else {
                    x >= y
                }
            }
            _ => true,
        };
        ok(self.min_width, Some(vw), true)
            && ok(self.max_width, Some(vw), false)
            && ok(self.min_height, Some(vh), true)
            && ok(self.max_height, Some(vh), false)
            && match self.orientation_landscape {
                Some(l) => (vw >= vh) == l,
                None => true,
            }
            && match self.dark {
                Some(d) => d == dark,
                None => true,
            }
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub selectors: SelectorSet,
    pub declarations: Vec<Decl>,
    /// `::before { ... }` and `::after { ... }` declarations of this rule.
    pub pseudo: Vec<(String, Vec<Decl>)>,
    pub media: Option<Media>,
    /// Source order, the final cascade tiebreaker.
    pub order: u32,
}

#[derive(Clone, Debug, Default)]
pub struct FontFace {
    pub family: String,
    pub src: Vec<String>,
    pub weight: u16,
    pub style_italic: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub rules: Vec<Rule>,
    pub font_faces: Vec<FontFace>,
    /// `@import url(...)` in document order; the loader fetches these.
    pub imports: Vec<String>,
    /// Count of statements we could not understand (surfaced in `kilat css`).
    pub skipped: usize,
}

impl Sheet {
    pub fn new() -> Sheet {
        Sheet::default()
    }

    /// Parse an additional stylesheet, appending to this one (used for `@import`
    /// and for the per-page `<style>` elements).
    pub fn extend(&mut self, css: &str) {
        let other = parse_sheet(css);
        self.rules.extend(other.rules);
        self.font_faces.extend(other.font_faces);
        self.imports.extend(other.imports);
        self.skipped += other.skipped;
    }

    /// Rules whose selector list has an id or class, used by layout invalidation
    /// to decide whether an attribute change can matter at all.
    pub fn may_depend_on_attr(&self, name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        self.rules.iter().any(|r| {
            r.selectors.complexes.iter().any(|c| {
                c.parts.iter().any(|p| {
                    p.attrs
                        .iter()
                        .any(|a| a.name == n || (n == "class" && !p.classes.is_empty()))
                })
            })
        })
    }
}

/// Parse a whole stylesheet. Errors never abort: a bad declaration is dropped
/// and counted, exactly as the CSS error-handling rules prescribe.
pub fn parse_sheet(css: &str) -> Sheet {
    let mut sheet = Sheet::default();
    let b = css.as_bytes();
    let mut i = 0usize;
    let mut order = 0u32;
    let mut media_stack: Vec<Media> = Vec::new();
    while i < b.len() {
        skip_ws_and_comments(css, &mut i);
        if i >= b.len() {
            break;
        }
        if css.as_bytes()[i] == b'@' {
            let (name, block) = match at_rule(css, &mut i) {
                Some(v) => v,
                None => break,
            };
            match name.as_str() {
                "media" | "supports" | "layer" | "container" | "scope" => {
                    let cond = if name == "media" {
                        at_media_condition(block.0)
                    } else {
                        // @supports/@layer/@container: keep the inner rules,
                        // ignoring the condition (we apply everything we can).
                        Media::default()
                    };
                    media_stack.push(cond.clone());
                    let inner = &css[block.1..block.2];
                    let sub = parse_sheet(inner);
                    media_stack.pop();
                    let media = merge_media(&media_stack, &Some(cond));
                    for mut r in sub.rules {
                        r.media = media.clone();
                        r.order = order;
                        order += 1;
                        sheet.rules.push(r);
                    }
                    sheet.font_faces.extend(sub.font_faces);
                    sheet.imports.extend(sub.imports);
                    sheet.skipped += sub.skipped;
                }
                "import" => {
                    if let Some(u) = parse_import(block.0) {
                        sheet.imports.push(u);
                    }
                }
                "font-face" => {
                    let decls = parse_declarations(&css[block.1..block.2]);
                    let mut ff = FontFace::default();
                    for d in decls {
                        match d.name() {
                            "font-family" => {
                                ff.family = d.value().trim_matches('"').trim_matches('\'').to_string()
                            }
                            "src" => {
                                ff.src = split_top_sep(d.value(), ',')
                                    .into_iter()
                                    .filter_map(|s| {
                                        let s = s.trim();
                                        s.strip_prefix("url(").map(|u| {
                                            u.trim_end_matches(')')
                                                .trim_matches('"')
                                                .trim_matches('\'')
                                                .to_string()
                                        })
                                    })
                                    .collect()
                            }
                            "font-weight" => {
                                ff.weight = d
                                    .value()
                                    .split_whitespace()
                                    .next()
                                    .and_then(|w| w.parse().ok())
                                    .unwrap_or(400)
                            }
                            "font-style" => ff.style_italic = d.value().trim() == "italic",
                            _ => {}
                        }
                    }
                    if !ff.family.is_empty() {
                        sheet.font_faces.push(ff);
                    }
                }
                // @charset/@namespace are honoured elsewhere; @keyframes and
                // everything unknown are dropped whole.
                _ => {
                    sheet.skipped += 1;
                }
            }
            continue;
        }
        // Qualified rule: prelude up to '{', block up to the matching '}'.
        let prelude_start = i;
        let mut depth = 0i32;
        while i < b.len() {
            match b[i] {
                b'"' | b'\'' => {
                    let q = b[i];
                    i += 1;
                    while i < b.len() && b[i] != q {
                        i += 1;
                    }
                }
                b'{' if depth == 0 => break,
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        if i >= b.len() {
            // Truncated rule at EOF: parse what we have (browsers do the same).
            break;
        }
        let prelude = css[prelude_start..i].trim();
        i += 1;
        let body_start = i;
        let mut depth = 1i32;
        while i < b.len() && depth > 0 {
            match b[i] {
                b'"' | b'\'' => {
                    let q = b[i];
                    i += 1;
                    while i < b.len() && b[i] != q {
                        i += 1;
                    }
                }
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            if depth > 0 {
                i += 1;
            }
        }
        let body = &css[body_start..i.min(b.len())];
        if i < b.len() {
            i += 1; // consume '}'
        }
        let set = match SelectorSet::parse(prelude) {
            Ok(s) => s,
            Err(_) => {
                // An unknown selector invalidates the whole rule - but only that
                // rule, which is what keeps pages with `:has()` working.
                sheet.skipped += 1;
                continue;
            }
        };
        let decls = parse_declarations(body);
        if decls.is_empty() {
            continue;
        }
        let mut normal: Vec<Decl> = Vec::new();
        let mut pseudo: Vec<(String, Vec<Decl>)> = Vec::new();
        // Split off the pseudo-element branch of each selector.
        for (name, _list) in selector_pseudo_elements(&set) {
            let mut p = Vec::new();
            for d in decls.iter() {
                if let Decl::Prop { .. } = d {
                    p.push(d.clone());
                }
            }
            if !p.is_empty() {
                pseudo.push((name, p));
            }
        }
        for d in decls {
            normal.push(d);
        }
        order += 1;
        sheet.rules.push(Rule {
            selectors: set,
            declarations: normal,
            pseudo,
            media: merge_media(&media_stack, &None),
            order,
        });
    }
    sheet
}

fn merge_media(stack: &[Media], inner: &Option<Media>) -> Option<Media> {
    let mut out: Option<Media> = None;
    for m in stack.iter().chain(inner.iter()) {
        let o = out.get_or_insert_with(Media::default);
        o.min_width = max(o.min_width, m.min_width);
        o.max_width = min(o.max_width, m.max_width);
        o.min_height = max(o.min_height, m.min_height);
        o.max_height = min(o.max_height, m.max_height);
        o.orientation_landscape = m.orientation_landscape.or(o.orientation_landscape);
        o.dark = m.dark.or(o.dark);
        o.type_ = m.type_.clone().or_else(|| o.type_.clone());
    }
    out
}

fn max(a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    }
}
fn min(a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

fn skip_ws_and_comments(s: &str, i: &mut usize) {
    let b = s.as_bytes();
    while *i < b.len() {
        match b[*i] {
            c if (c as char).is_whitespace() => *i += 1,
            b'/' if *i + 1 < b.len() && b[*i + 1] == b'*' => {
                *i += 2;
                while *i + 1 < b.len() && !(b[*i] == b'*' && b[*i + 1] == b'/') {
                    *i += 1;
                }
                *i = (*i + 2).min(b.len());
            }
            _ => return,
        }
    }
}

/// `@name(prelude) {` or `@name prelude;` - returns (name, (prelude, block range)).
fn at_rule<'a>(css: &'a str, i: &mut usize) -> Option<(String, (&'a str, usize, usize))> {
    let b = css.as_bytes();
    let start = *i + 1;
    let mut j = start;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
        j += 1;
    }
    let name = css[start..j].to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let prelude_start = j;
    let mut depth = 0i32;
    while j < b.len() {
        match b[j] {
            b'"' | b'\'' => {
                let q = b[j];
                j += 1;
                while j < b.len() && b[j] != q {
                    j += 1;
                }
            }
            b'(' => depth += 1,
            b')' => depth -= 1,
            b';' if depth == 0 => {
                let prelude = &css[prelude_start..j];
                *i = j + 1;
                return Some((name, (prelude, j, j)));
            }
            b'{' if depth == 0 => break,
            _ => {}
        }
        j += 1;
    }
    if j >= b.len() {
        *i = b.len();
        return Some((name, (&css[prelude_start..], prelude_start, b.len())));
    }
    let prelude = &css[prelude_start..j];
    // Block body, brace matched.
    let body_start = j + 1;
    let mut depth = 1i32;
    let mut k = body_start;
    while k < b.len() && depth > 0 {
        match b[k] {
            b'"' | b'\'' => {
                let q = b[k];
                k += 1;
                while k < b.len() && b[k] != q {
                    k += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        if depth > 0 {
            k += 1;
        }
    }
    *i = (k + 1).min(b.len());
    Some((name, (prelude.trim(), body_start, k.min(b.len()))))
}

fn at_media_condition(prelude: &str) -> Media {
    let mut m = Media::default();
    let t = prelude.trim();
    let t = t
        .strip_prefix("media")
        .or_else(|| t.strip_prefix("supports"))
        .unwrap_or(t)
        .trim();
    for part in t.split(" and ") {
        let p = part.trim().trim_start_matches('(').trim_end_matches(')');
        let (k, v) = match p.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => {
                if p.eq_ignore_ascii_case("print") || p.eq_ignore_ascii_case("screen") {
                    m.type_ = Some(p.to_ascii_lowercase());
                } else if p.eq_ignore_ascii_case("all") {
                    m.type_ = Some("all".to_string());
                }
                continue;
            }
        };
        let num = |v: &str| -> Option<f32> {
            let v = v.trim();
            if let Some(px) = v.strip_suffix("px") {
                return px.trim().parse().ok();
            }
            if let Some(em) = v.strip_suffix("em") {
                return em.trim().parse::<f32>().ok().map(|x| x * 16.0);
            }
            if let Some(rems) = v.strip_suffix("rem") {
                return rems.trim().parse::<f32>().ok().map(|x| x * 16.0);
            }
            v.parse().ok()
        };
        match k.to_ascii_lowercase().as_str() {
            "min-width" => m.min_width = num(v),
            "max-width" => m.max_width = num(v),
            "min-height" => m.min_height = num(v),
            "max-height" => m.max_height = num(v),
            "orientation" => m.orientation_landscape = Some(v.eq_ignore_ascii_case("landscape")),
            "prefers-color-scheme" => {
                m.dark = Some(v.eq_ignore_ascii_case("dark"));
            }
            _ => {}
        }
    }
    m
}

fn parse_import(prelude: &str) -> Option<String> {
    let p = prelude.trim();
    let url = if let Some(rest) = p.strip_prefix("url(") {
        rest.trim_end_matches(')')
    } else if p.starts_with('"') || p.starts_with('\'') {
        return Some(p.trim_matches('"').trim_matches('\'').to_string());
    } else {
        return None;
    };
    let u = url.trim().trim_matches('"').trim_matches('\'');
    if u.is_empty() {
        None
    } else {
        Some(u.to_string())
    }
}

/// Which pseudo-elements a rule's declarations target.
fn selector_pseudo_elements(set: &SelectorSet) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (idx, c) in set.complexes.iter().enumerate() {
        if let Some(p) = c.parts.last() {
            if let Some(el) = &p.element {
                out.push((el.clone(), idx));
            }
        }
    }
    out
}

/// Declarations inside a block, with shorthands expanded.
fn parse_declarations(body: &str) -> Vec<Decl> {
    let mut out: Vec<Decl> = Vec::new();
    for chunk in split_declarations(body) {
        let (name, value) = match chunk.split_once(':') {
            Some(x) => x,
            None => continue,
        };
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let mut value = value.trim().to_string();
        let mut important = false;
        if let Some(pos) = value.rfind('!') {
            if value[pos..].replace(' ', "").eq_ignore_ascii_case("!important") {
                value = value[..pos].trim().to_string();
                important = true;
            }
        }
        value = value.trim().to_string();
        if value.eq_ignore_ascii_case("inherit") {
            out.push(Decl::Prop {
                name,
                value: "inherit".to_string(),
                important,
            });
            continue;
        }
        if value.is_empty() {
            continue;
        }
        if name.starts_with("--") {
            out.push(Decl::Custom {
                name,
                value,
                important,
            });
            continue;
        }
        expand_shorthand(&name, &value, important, &mut out);
    }
    out
}

/// Parse the contents of a `style="..."` attribute (declarations, no selector).
pub fn parse_style_attribute(text: &str) -> Vec<Decl> {
    parse_declarations(text)
}

/// Split on `;` at brace/paren depth 0 and outside strings (data URIs and
/// nested `url(...)` are common in real stylesheets).
fn split_declarations(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in body.chars() {
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
            ';' if depth == 0 => {
                if !cur.trim().is_empty() {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn prop(name: &str, value: &str, important: bool) -> Decl {
    Decl::Prop {
        name: name.to_string(),
        value: value.trim().to_string(),
        important,
    }
}

/// One entry point for all shorthand expansion.
pub fn expand_shorthand(name: &str, value: &str, important: bool, out: &mut Vec<Decl>) {
    let v = value.trim();
    let push = |out: &mut Vec<Decl>, n: &str, val: &str| out.push(prop(n, val, important));
    match name {
        "margin" | "padding" => {
            let parts = split_top_sep(v, ' ');
            let [t, r, b, l] = box_sides(&parts);
            for (side, val) in [
                ("top", t),
                ("right", r),
                ("bottom", b),
                ("left", l),
            ] {
                if val != "0" || name == "margin" {
                    push(
                        out,
                        &format!("{name}-{side}"),
                        if val == "0" { "0" } else { val },
                    );
                }
            }
        }
        "inset" => {
            let parts = split_top_sep(v, ' ');
            let [t, r, b, l] = box_sides(&parts);
            push(out, "top", t);
            push(out, "right", r);
            push(out, "bottom", b);
            push(out, "left", l);
        }
        "border" => {
            // `border: 1px solid red` in any order; also `border: none`.
            let parts = split_top_sep(v, ' ');
            let mut width = None;
            let mut style = None;
            let mut color = None;
            for p in parts {
                if p.parse::<f32>().is_ok() || crate::css::value::split_number_unit(&p).is_some() {
                    width = Some(p.clone());
                } else if matches!(
                    p.as_str(),
                    "none"
                        | "hidden"
                        | "dotted"
                        | "dashed"
                        | "solid"
                        | "double"
                        | "groove"
                        | "ridge"
                        | "inset"
                        | "outset"
                ) {
                    style = Some(p.clone());
                } else {
                    color = Some(p.clone());
                }
            }
            for side in ["top", "right", "bottom", "left"] {
                if let Some(w) = &width {
                    push(out, &format!("border-{side}-width"), w);
                }
                if let Some(s) = &style {
                    push(out, &format!("border-{side}-style"), s);
                }
                if let Some(c) = &color {
                    push(out, &format!("border-{side}-color"), c);
                }
            }
            if width.is_none() && style.is_none() && color.is_none() {
                push(out, "border-style", v);
            }
        }
        "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let side = name.strip_prefix("border-").unwrap_or("");
            for p in split_top_sep(v, ' ') {
                if p.parse::<f32>().is_ok() || crate::css::value::split_number_unit(&p).is_some() {
                    push(out, &format!("border-{side}-width"), &p);
                } else {
                    push(out, &format!("border-{side}-style"), &p);
                }
            }
        }
        "border-width" | "border-style" | "border-color" => {
            let suffix = name.strip_prefix("border-").unwrap_or("");
            let parts = split_top_sep(v, ' ');
            let [t, r, b, l] = box_sides(&parts);
            for (side, val) in [
                ("top", t),
                ("right", r),
                ("bottom", b),
                ("left", l),
            ] {
                push(out, &format!("border-{side}-{suffix}"), val);
            }
        }
        "border-radius" => {
            let parts = split_top_sep(v, '/');
            let h = split_top_sep(&parts[0], ' ');
            let [tl, tr, br, bl] = box_sides(&h);
            let vpart = parts.get(1).map(|s| split_top_sep(s, ' '));
            let vert = match &vpart {
                Some(list) => {
                    let [a, b, c, d] = box_sides(list);
                    [a, b, c, d]
                }
                None => [tl, tr, br, bl],
            };
            let horiz = [tl, tr, br, bl];
            let names = [
                "border-top-left-radius",
                "border-top-right-radius",
                "border-bottom-right-radius",
                "border-bottom-left-radius",
            ];
            for (i, n) in names.iter().enumerate() {
                push(out, n, &format!("{} {}", horiz[i], vert[i]));
            }
        }
        "background" => {
            let parts = split_top_commas(v);
            let mut color = None;
            let mut image = None;
            let mut size = None;
            let mut repeat = None;
            let mut position = None;
            let mut origin = None;
            for (idx, layer) in parts.iter().enumerate() {
                let mut pending_bg_size: Option<String> = None;
                for tok in split_top_sep(layer, '/') {
                    let t = tok.trim().to_string();
                    if t.starts_with("linear-gradient")
                        || t.starts_with("radial-gradient")
                        || t.starts_with("conic-gradient")
                        || t.starts_with("repeating-linear-gradient")
                        || t.starts_with("url(")
                        || t.eq_ignore_ascii_case("none")
                    {
                        if image.is_none() {
                            image = Some(t.clone());
                        }
                        continue;
                    }
                    if t.starts_with("no-repeat") || t.starts_with("repeat") {
                        if repeat.is_none() {
                            repeat = Some(t.clone());
                        }
                        continue;
                    }
                    if t.starts_with("center")
                        || t.starts_with("top")
                        || t.starts_with("bottom")
                        || t.starts_with("left")
                        || t.starts_with("right")
                        || crate::css::value::Length::parse(&t).is_some()
                    {
                        if position.is_none() {
                            position = Some(t.clone());
                        }
                        continue;
                    }
                    if t.starts_with("cover") || t.starts_with("contain") {
                        if size.is_none() {
                            size = Some(t.clone());
                        }
                        continue;
                    }
                    if matches!(
                        t.as_str(),
                        "padding-box" | "border-box" | "content-box" | "text"
                    ) {
                        if origin.is_none() {
                            origin = Some(t.clone());
                        }
                        continue;
                    }
                    if color.is_none() && idx == 0 {
                        color = Some(t.clone());
                    }
                }
                let _ = &mut pending_bg_size;
            }
            if let Some(c) = color {
                push(out, "background-color", &c);
            }
            if let Some(i) = image {
                push(out, "background-image", &i);
            }
            if let Some(r) = repeat {
                push(out, "background-repeat", &r);
            }
            if let Some(p) = position {
                push(out, "background-position", &p);
            }
            if let Some(s) = size {
                push(out, "background-size", &s);
            }
            if let Some(o) = origin {
                push(out, "background-clip", &o);
            }
        }
        "font" => {
            // `font: [style variant weight stretch] size[/line-height] family`
            let mut rest = v.to_string();
            let mut italic = None;
            let mut weight = None;
            let mut lh = None;
            let mut fs = None;
            let mut family = None;
            // Split size/family at the last space before the family list.
            if let Some(slash) = rest.find('/') {
                let (a, b) = rest.split_at(slash);
                fs = a
                    .split_whitespace()
                    .last()
                    .map(|s| s.trim().to_string());
                let after = &b[1..];
                let mut it = after.split_whitespace();
                lh = it.next().map(|s| s.to_string());
                family = Some(it.collect::<Vec<_>>().join(" "));
                rest = a.trim().to_string();
            } else if let Some(pos) = find_font_size(&rest) {
                fs = Some(rest[..pos].split_whitespace().last().unwrap_or("").to_string());
                family = Some(rest[pos..].to_string());
                rest = rest[..pos].to_string();
            }
            for tok in split_top_sep(rest.trim(), ' ') {
                match tok.as_str() {
                    "italic" | "oblique" => italic = Some("italic".to_string()),
                    "bold" => weight = Some("700".to_string()),
                    "bolder" => weight = Some("600".to_string()),
                    "lighter" => weight = Some("300".to_string()),
                    "normal" => {}
                    "small-caps" => {}
                    s if s.parse::<u16>().is_ok() => weight = Some(s.to_string()),
                    _ => {}
                }
            }
            if let Some(f) = fs {
                push(out, "font-size", &f);
            }
            if let Some(l) = lh {
                push(out, "line-height", &l);
            }
            if let Some(f) = family {
                push(out, "font-family", &f);
            }
            if let Some(i) = italic {
                push(out, "font-style", &i);
            }
            if let Some(w) = weight {
                push(out, "font-weight", &w);
            }
        }
        "flex" => {
            let parts = split_top_sep(v, ' ');
            match parts.first().map(|s| s.as_str()) {
                Some("none") => {
                    push(out, "flex-grow", "0");
                    push(out, "flex-shrink", "0");
                    push(out, "flex-basis", "auto");
                }
                Some("auto") => {
                    push(out, "flex-grow", "1");
                    push(out, "flex-shrink", "1");
                    push(out, "flex-basis", "auto");
                }
                _ => {
                    push(out, "flex-grow", parts.first().map(|s| s.as_str()).unwrap_or("0"));
                    if let Some(s) = parts.get(1) {
                        push(out, "flex-shrink", s);
                    } else {
                        push(out, "flex-shrink", "1");
                    }
                    if let Some(b) = parts.get(2) {
                        push(out, "flex-basis", b);
                    } else {
                        push(out, "flex-basis", if parts.len() == 1 { "0%" } else { "auto" });
                    }
                }
            }
        }
        "gap" | "grid-gap" => {
            let parts = split_top_sep(v, ' ');
            let [r, c, _, _] = box_sides(&parts);
            push(out, "row-gap", r);
            push(out, "column-gap", c);
        }
        "overflow" => {
            let parts = split_top_sep(v, ' ');
            match parts.len() {
                1 => {
                    push(out, "overflow-x", &parts[0]);
                    push(out, "overflow-y", &parts[0]);
                }
                _ => {
                    push(out, "overflow-x", &parts[0]);
                    push(out, "overflow-y", &parts.get(1).cloned().unwrap_or_else(|| "visible".to_string()));
                }
            }
        }
        "text-decoration" => {
            let parts = split_top_sep(v, ' ');
            for p in parts {
                match p.as_str() {
                    "underline" | "overline" | "line-through" | "none" => {
                        push(out, "text-decoration-line", &p)
                    }
                    "solid" | "double" | "dotted" | "dashed" | "wavy" => {
                        push(out, "text-decoration-style", &p)
                    }
                    other => {
                        if crate::util::geom::Color::parse(other).is_some() {
                            push(out, "text-decoration-color", other)
                        }
                    }
                }
            }
        }
        "outline" => {
            push(out, "outline-style", if v == "none" { "none" } else { "solid" });
            for p in split_top_sep(v, ' ') {
                if crate::css::value::split_number_unit(&p).is_some() {
                    push(out, "outline-width", &p);
                }
            }
        }
        "list-style" => {
            for p in split_top_sep(v, ' ') {
                if p == "none" || p == "disc" || p == "circle" || p == "square" || p == "decimal"
                {
                    push(out, "list-style-type", &p);
                } else if p == "inside" || p == "outside" {
                    push(out, "list-style-position", &p);
                }
            }
        }
        "visibility" | "cursor" | "transition" | "animation" | "filter" | "backdrop-filter"
        | "will-change" | "contain" | "content-visibility" => {
            // Kept verbatim: the painter reads `filter`/`visibility` directly and
            // the rest are accepted so unknown properties never drop a rule.
            push(out, name, v);
        }
        _ => push(out, name, v),
    }
}

/// `a`, `a b`, `a b c`, `a b c d` -> (top, right, bottom, left).
pub fn box_sides(parts: &[String]) -> [&str; 4] {
    match parts.len() {
        0 => ["0", "0", "0", "0"],
        1 => [parts[0].as_str(), parts[0].as_str(), parts[0].as_str(), parts[0].as_str()],
        2 => [
            parts[0].as_str(),
            parts[1].as_str(),
            parts[0].as_str(),
            parts[1].as_str(),
        ],
        3 => [
            parts[0].as_str(),
            parts[1].as_str(),
            parts[2].as_str(),
            parts[1].as_str(),
        ],
        _ => [
            parts[0].as_str(),
            parts[1].as_str(),
            parts[2].as_str(),
            parts[3].as_str(),
        ],
    }
}

/// Split on commas that are not inside parentheses (`background: a, b`).
fn split_top_commas(v: &str) -> Vec<String> {
    split_top_sep(v, ',')
}

/// `font: 12px/1.5 Arial` is handled separately; this finds the size boundary in
/// `font: bold 12px Arial` (first token with a length unit or a bare number that
/// is followed by a family name).
fn find_font_size(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut idx = 0usize;
    while idx < text.len() {
        while idx < text.len() && bytes[idx] == b' ' {
            idx += 1;
        }
        let start = idx;
        while idx < text.len() && bytes[idx] != b' ' {
            idx += 1;
        }
        let tok = &text[start..idx];
        let looks_like_size = tok.ends_with("px")
            || tok.ends_with("pt")
            || tok.ends_with("em")
            || tok.ends_with("rem")
            || tok.ends_with("%")
            || tok.parse::<f32>().is_ok();
        if looks_like_size && idx < text.len() {
            return Some(start);
        }
        if idx >= text.len() {
            break;
        }
    }
    None
}

/// Replace `var(--x, fallback)` occurrences. `lookup` supplies custom property
/// values; unresolved references collapse to their fallback or an empty string.
pub fn resolve_vars(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut cur = text.to_string();
    for _ in 0..8 {
        let start = match find_var(&cur) {
            Some(s) => s,
            None => return cur,
        };
        // cur[start..] begins with "var("
        let open = start + 3;
        let mut depth = 0i32;
        let mut end = open;
        while end < cur.len() {
            match cur.as_bytes()[end] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            end += 1;
        }
        if end >= cur.len() {
            return cur;
        }
        let inner = &cur[open + 1..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim().to_string())),
            None => (inner.trim(), None),
        };
        let replacement = lookup(name).or(fallback).unwrap_or_default();
        cur.replace_range(start..end + 1, &replacement);
    }
    cur
}

fn find_var(s: &str) -> Option<usize> {
    let mut from = 0usize;
    loop {
        let rel = s[from..].find("var(")?;
        let abs = from + rel;
        let before_ok = abs == 0 || !s.as_bytes()[abs - 1].is_ascii_alphanumeric();
        if before_ok {
            return Some(abs);
        }
        from = abs + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_and_comments() {
        let s = parse_sheet("/* hi */ a { color: red; /*x*/ text-decoration: underline solid blue }");
        assert_eq!(s.rules.len(), 1);
        let d = &s.rules[0].declarations;
        assert!(d.iter().any(|x| x.name() == "color" && x.value() == "red"));
        assert!(d
            .iter()
            .any(|x| x.name() == "text-decoration-line" && x.value() == "underline"));
        assert!(d
            .iter()
            .any(|x| x.name() == "text-decoration-color" && x.value() == "blue"));
    }

    #[test]
    fn important_and_inherit() {
        let s = parse_sheet("p { color: red !important; margin: 0px ! important; font: inherit }");
        let d = &s.rules[0].declarations;
        assert!(d[0].important());
        assert!(d[1..].iter().any(|x| x.important() && x.name() == "margin-left"));
        assert!(d.iter().any(|x| x.name() == "font-family" && x.value() == "inherit"));
    }

    #[test]
    fn shorthand_margin() {
        let s = parse_sheet("div { margin: 1px 2px 3px }");
        let d = &s.rules[0].declarations;
        let get = |n: &str| d.iter().find(|x| x.name() == n).map(|x| x.value().to_string());
        assert_eq!(get("margin-top").as_deref(), Some("1px"));
        assert_eq!(get("margin-right").as_deref(), Some("2px"));
        assert_eq!(get("margin-bottom").as_deref(), Some("1px"));
        assert_eq!(get("margin-left").as_deref(), Some("2px"));
    }

    #[test]
    fn shorthand_border_and_background() {
        let s = parse_sheet("div { border: 2px solid #333; background: url(a.png) no-repeat center / cover rgba(0,0,0,.5) }");
        let d = &s.rules[0].declarations;
        let names: Vec<&str> = d.iter().map(|x| x.name()).collect();
        assert!(names.contains(&"border-top-width"), "{names:?}");
        assert!(names.contains(&"border-left-style"));
        assert!(d.iter().any(|x| x.name() == "background-image" && x.value().starts_with("url(")));
        assert!(d.iter().any(|x| x.name() == "background-color" && x.value().starts_with("rgba")));
        assert!(d.iter().any(|x| x.name() == "background-size" && x.value() == "cover"));
    }

    #[test]
    fn shorthand_font() {
        let s = parse_sheet("p { font: italic bold 14px/1.5 'Helvetica Neue', sans-serif }");
        let d = &s.rules[0].declarations;
        let get = |n: &str| d.iter().find(|x| x.name() == n).map(|x| x.value().to_string());
        assert_eq!(get("font-size").as_deref(), Some("14px"));
        assert_eq!(get("line-height").as_deref(), Some("1.5"));
        assert_eq!(get("font-style").as_deref(), Some("italic"));
        assert_eq!(get("font-weight").as_deref(), Some("700"));
        assert!(get("font-family").unwrap().contains("Helvetica Neue"));
    }

    #[test]
    fn media_queries_filter_rules() {
        let s = parse_sheet("a{color:red}@media (max-width: 400px){a{color:blue}}");
        assert_eq!(s.rules.len(), 2);
        let narrow = s.rules[1].media.clone().unwrap();
        assert!(narrow.matches(300.0, 600.0, false));
        assert!(!narrow.matches(500.0, 600.0, false));
        assert!(s.rules[0].media.is_none());
    }

    #[test]
    fn font_face_and_import() {
        let s = parse_sheet("@font-face { font-family: 'MyFont'; src: url(a.woff2) format('woff2'), local(Foo) } @import url(b.css);");
        assert_eq!(s.font_faces.len(), 1);
        assert_eq!(s.font_faces[0].family, "MyFont");
        assert_eq!(s.font_faces[0].src, vec!["a.woff2".to_string()]);
        assert_eq!(s.imports, vec!["b.css".to_string()]);
    }

    #[test]
    fn pseudo_element_rules_are_split_out() {
        let s = parse_sheet("p::before { content: 'x'; color: red }");
        assert_eq!(s.rules[0].pseudo.len(), 1);
        assert_eq!(s.rules[0].pseudo[0].0, "before");
    }

    #[test]
    fn unbalanced_and_truncated() {
        let s = parse_sheet("a { color: red ; b { color: blue }");
        assert!(!s.rules.is_empty());
        let s = parse_sheet("a { color: rgb(");
        assert_eq!(s.rules.len(), 1);
        let s = parse_sheet("@media (");
        assert!(s.rules.is_empty());
    }

    #[test]
    fn var_resolution() {
        let s = parse_sheet(":root { --c: green; --w: 5px } p { color: var(--c, red); border-left-width: var(--w) }");
        let customs: Vec<(String, String)> = s
            .rules
            .iter()
            .flat_map(|r| r.declarations.iter())
            .filter_map(|d| match d {
                Decl::Custom { name, value, .. } => Some((name.clone(), value.clone())),
                _ => None,
            })
            .collect();
        let lookup = |n: &str| {
            customs
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.clone())
        };
        let color = s.rules[1]
            .declarations
            .iter()
            .find(|d| d.name() == "color")
            .unwrap();
        assert_eq!(resolve_vars(color.value(), &lookup), "green");
        let fallback = resolve_vars("var(--missing, 3px)", &lookup);
        assert_eq!(fallback, "3px");
        assert_eq!(resolve_vars("solid var(--c) red", &lookup), "solid green red");
    }

    #[test]
    fn selectors_survive_odd_syntax() {
        // Trailing commas, :is(), attribute selectors with strings containing ;
        let s = parse_sheet("a[href=\"x;y\"], li:is(ul, ol)::after { color: red }");
        assert_eq!(s.rules.len(), 1);
        assert_eq!(s.rules[0].selectors.complexes.len(), 2);
    }
}
