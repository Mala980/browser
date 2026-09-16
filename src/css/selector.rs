//! Selector parsing + matching.
//!
//! Selectors are compiled to right-to-left compound chains (id/class/attribute/
//! pseudo tests then ancestor walking), which is how every real engine does it:
//! the leftmost part is the cheapest rejection, and `querySelectorAll` over a few
//! thousand nodes stays interactive.

use crate::css::value::split_top_sep;
use crate::dom::{Dom, Kind};
use crate::util::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    /// `A B`
    Descendant,
    /// `A > B`
    Child,
    /// `A + B`
    NextSibling,
    /// `A ~ B`
    LaterSibling,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttrSel {
    pub name: String,
    pub op: Option<char>,
    pub value: String,
    pub ci: bool,
}

impl AttrSel {
    fn test(&self, dom: &Dom, node: usize) -> bool {
        let have = match dom.attr(node, &self.name) {
            Some(v) => v,
            None => return false,
        };
        let op = match self.op {
            None => return true,
            Some(o) => o,
        };
        let (a, b) = if self.ci {
            (have.to_ascii_lowercase(), self.value.to_ascii_lowercase())
        } else {
            (have, self.value.clone())
        };
        match op {
            '<' => a != b,
            '=' => a == b,
            '~' => a.split_whitespace().any(|p| p == b),
            '|' => a == b || a.starts_with(&format!("{b}-")),
            '^' => !b.is_empty() && a.starts_with(&b),
            '$' => !b.is_empty() && a.ends_with(&b),
            '*' => !b.is_empty() && a.contains(&b),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pseudo {
    Root,
    Empty,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    NthChild(i32, i32),
    NthLastChild(i32, i32),
    NthOfType(i32, i32),
    Checked,
    Disabled,
    Enabled,
    Required,
    ReadOnly,
    Hover,
    Active,
    Focus,
    FocusVisible,
    Link,
    AnyLink,
    Visited,
    Is(Vec<Complex>),
    Where(Vec<Complex>),
    Not(Vec<Complex>),
    Has(Vec<Complex>),
    /// `:is(...)`-style with an unknown name: never matches, but parses.
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSel>,
    pub pseudos: Vec<Pseudo>,
    /// `::before`, `::after`, ... - remembered so the style step can attach the
    /// pseudo-element declarations to the right box.
    pub element: Option<String>,
    pub specificity: u32,
}

impl Compound {
    /// True when the compound asserts nothing about the element.
    pub fn matches_anything(&self) -> bool {
        self.tag.is_none()
            && self.id.is_none()
            && self.classes.is_empty()
            && self.attrs.is_empty()
            && self.pseudos.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Complex {
    /// Rightmost compound first; `combinators[i]` links `parts[i]` to `parts[i+1]`.
    pub parts: Vec<Compound>,
    pub combinators: Vec<Combinator>,
    pub specificity: u32,
}

impl Complex {
    pub fn has_pseudo_element(&self, name: &str) -> bool {
        self.parts
            .last()
            .map(|p| p.element.as_deref() == Some(name))
            .unwrap_or(false)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelectorSet {
    pub complexes: Vec<Complex>,
}

impl SelectorSet {
    pub fn parse(text: &str) -> Result<SelectorSet> {
        let mut set = SelectorSet::default();
        for chunk in split_top_sep(text, ',') {
            if chunk.trim().is_empty() {
                continue;
            }
            let mut p = SelParser {
                c: chunk.as_str(),
                i: 0,
            };
            let complex = p.parse_complex()?;
            set.complexes.push(complex);
        }
        if set.complexes.is_empty() {
            return Err(format!("css: empty selector {text:?}"));
        }
        Ok(set)
    }

    pub fn specificity(&self) -> u32 {
        self.complexes
            .iter()
            .map(|c| c.specificity)
            .max()
            .unwrap_or(0)
    }

    pub fn matches(&self, dom: &Dom, node: usize) -> bool {
        self.complexes.iter().any(|c| matches_complex(dom, node, c))
    }

    /// Selectors that only depend on the element itself (fast path used by the
    /// style cache to decide whether a state change invalidates anything).
    pub fn depends_on_ancestors(&self) -> bool {
        self.complexes.iter().any(|c| c.parts.len() > 1)
    }
}

fn spec(id: u32, cls: u32, ty: u32) -> u32 {
    id.min(255) * 65_536 + cls.min(255) * 256 + ty.min(255)
}

pub fn matches_complex(dom: &Dom, node: usize, c: &Complex) -> bool {
    if c.parts.is_empty() {
        return false;
    }
    if !matches_compound(dom, node, &c.parts[0]) {
        return false;
    }
    let mut cur = node;
    for (idx, comb) in c.combinators.iter().enumerate() {
        let want = &c.parts[idx + 1];
        match comb {
            Combinator::Descendant => {
                let mut p = dom.parent(cur);
                let mut found = false;
                while let Some(pp) = p {
                    if matches_compound(dom, pp, want) {
                        found = true;
                        cur = pp;
                        break;
                    }
                    p = dom.parent(pp);
                }
                if !found {
                    return false;
                }
            }
            Combinator::Child => {
                match dom.parent(cur) {
                    Some(p) if matches_compound(dom, p, want) => cur = p,
                    _ => return false,
                }
            }
            Combinator::NextSibling => {
                match dom.prev_element_sibling(cur) {
                    Some(s) if matches_compound(dom, s, want) => cur = s,
                    _ => return false,
                }
            }
            Combinator::LaterSibling => {
                let mut s = dom.prev_element_sibling(cur);
                let mut found = false;
                while let Some(cur_sib) = s {
                    if matches_compound(dom, cur_sib, want) {
                        found = true;
                        cur = cur_sib;
                        break;
                    }
                    s = dom.prev_element_sibling(cur_sib);
                }
                if !found {
                    return false;
                }
            }
        }
    }
    true
}

pub fn matches_compound(dom: &Dom, node: usize, c: &Compound) -> bool {
    let n = match dom.node(node) {
        Some(n) => n,
        None => return false,
    };
    // Selectors only ever match elements (querySelectorAll('*') excludes text).
    if n.kind != Kind::Element {
        return false;
    }
    if let Some(tag) = &c.tag {
        if tag != "*" {
            if n.kind != Kind::Element {
                return false;
            }
            if !n.tag.eq_ignore_ascii_case(tag) {
                return false;
            }
        }
    }
    if let Some(id) = &c.id {
        if dom.attr(node, "id").as_deref() != Some(id.as_str()) {
            return false;
        }
    }
    for cl in c.classes.iter() {
        if !dom.has_class(node, cl) {
            return false;
        }
    }
    for a in c.attrs.iter() {
        if !a.test(dom, node) {
            return false;
        }
    }
    for ps in c.pseudos.iter() {
        if !matches_pseudo(dom, node, ps) {
            return false;
        }
    }
    true
}

fn matches_pseudo(dom: &Dom, node: usize, ps: &Pseudo) -> bool {
    match ps {
        Pseudo::Root => {
            dom.node(node).map(|n| n.tag == "html").unwrap_or(false)
        }
        Pseudo::Empty => {
            let kids = dom.children(node);
            kids.is_empty()
                || kids.iter().all(|&k| match dom.node(k) {
                    Some(n) => n.kind == Kind::Comment || (n.kind == Kind::Text && n.data.trim().is_empty()),
                    None => true,
                })
        }
        Pseudo::FirstChild | Pseudo::LastChild | Pseudo::OnlyChild => {
            let p = match dom.parent(node) {
                Some(p) => p,
                None => return matches!(ps, Pseudo::FirstChild),
            };
            let kids = dom.element_children(p);
            let idx = kids.iter().position(|&k| k == node).unwrap_or(usize::MAX);
            match ps {
                Pseudo::FirstChild => idx == 0,
                Pseudo::LastChild => idx + 1 == kids.len(),
                _ => kids.len() == 1 && idx == 0,
            }
        }
        Pseudo::FirstOfType | Pseudo::LastOfType => {
            let p = match dom.parent(node) {
                Some(p) => p,
                None => return true,
            };
            let tag = dom.tag(node);
            let kids: Vec<usize> = dom
                .element_children(p)
                .into_iter()
                .filter(|&k| dom.tag(k) == tag)
                .collect();
            let idx = kids.iter().position(|&k| k == node).unwrap_or(usize::MAX);
            if matches!(ps, Pseudo::FirstOfType) {
                idx == 0
            } else {
                idx + 1 == kids.len()
            }
        }
        Pseudo::NthChild(a, b) => nth(dom, node, *a, *b, false, false),
        Pseudo::NthLastChild(a, b) => nth(dom, node, *a, *b, true, false),
        Pseudo::NthOfType(a, b) => nth(dom, node, *a, *b, false, true),
        Pseudo::Checked => {
            let c = dom.attr(node, "checked").is_some();
            let sel = dom.attr(node, "selected").is_some();
            let value = dom.attr(node, "value").unwrap_or_default();
            let type_ = dom.attr(node, "type").unwrap_or_default();
            if type_ == "radio" || type_ == "checkbox" {
                c
            } else if dom.tag(node) == "option" {
                sel
            } else if dom.tag(node) == "input" && type_ == "file" {
                !value.is_empty()
            } else {
                false
            }
        }
        Pseudo::Disabled => dom.attr(node, "disabled").is_some(),
        Pseudo::Enabled => {
            let t = dom.tag(node);
            (t == "input" || t == "button" || t == "select" || t == "textarea" || t == "option")
                && dom.attr(node, "disabled").is_none()
        }
        Pseudo::Required => dom.attr(node, "required").is_some(),
        Pseudo::ReadOnly => dom.attr(node, "readonly").is_some(),
        Pseudo::Hover => dom.state(node).hovered,
        Pseudo::Active => dom.state(node).pressed,
        Pseudo::Focus | Pseudo::FocusVisible => dom.state(node).focused,
        Pseudo::Link | Pseudo::AnyLink | Pseudo::Visited => {
            let t = dom.tag(node);
            (t == "a" || t == "area" || t == "link") && dom.attr(node, "href").is_some()
        }
        Pseudo::Is(list) | Pseudo::Where(list) | Pseudo::Not(list) | Pseudo::Has(list) => {
            let any = list.iter().any(|c| {
                if matches!(ps, Pseudo::Has(_)) {
                    dom.descendants(node).into_iter().any(|d| matches_complex(dom, d, c))
                } else {
                    matches_complex(dom, node, c)
                }
            });
            if matches!(ps, Pseudo::Not(_)) {
                !any
            } else {
                any
            }
        }
        Pseudo::Unknown => false,
    }
}

/// `:nth-child(an+b)`: an element at 1-based `index` matches when
/// `index = a*n + b` for some non-negative integer `n`.
fn nth(dom: &Dom, node: usize, a: i32, b: i32, from_end: bool, of_type: bool) -> bool {
    let p = match dom.parent(node) {
        Some(p) => p,
        None => return false,
    };
    let mut kids = dom.element_children(p);
    if of_type {
        let tag = dom.tag(node);
        kids.retain(|&k| dom.tag(k) == tag);
    }
    let idx = match kids.iter().position(|&k| k == node) {
        Some(i) => i,
        None => return false,
    };
    let pos = if from_end { kids.len() - idx } else { idx + 1 } as i32;
    if a == 0 {
        return pos == b;
    }
    let n = pos - b;
    // n must be an integer >= 0 with a*n divisible.
    if n == 0 {
        return true;
    }
    (n.signum() == a.signum()) && n % a == 0
}

struct SelParser<'a> {
    c: &'a str,
    i: usize,
}

impl<'a> SelParser<'a> {
    fn b(&self) -> &[u8] {
        self.c.as_bytes()
    }
    fn eof(&self) -> bool {
        self.i >= self.c.len()
    }
    fn peek(&self) -> u8 {
        if self.eof() {
            0
        } else {
            self.b()[self.i]
        }
    }
    fn skip_ws(&mut self) {
        while !self.eof() {
            match self.peek() {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => break,
            }
        }
    }

    fn parse_complex(&mut self) -> Result<Complex> {
        let mut parts: Vec<Compound> = Vec::new();
        let mut combinators: Vec<Combinator> = Vec::new();
        self.skip_ws();
        loop {
            let comp = self.parse_compound()?;
            parts.push(comp);
            self.skip_ws();
            if self.eof() {
                break;
            }
            let comb = match self.peek() {
                b'>' => {
                    self.i += 1;
                    self.skip_ws();
                    Combinator::Child
                }
                b'+' => {
                    self.i += 1;
                    self.skip_ws();
                    Combinator::NextSibling
                }
                b'~' => {
                    self.i += 1;
                    self.skip_ws();
                    Combinator::LaterSibling
                }
                _ => return Err(format!("css: unexpected {:?} in selector", self.c[self.i..].chars().next())),
            };
            combinators.push(comb);
        }
        let mut spec_id = 0u32;
        let mut spec_cls = 0u32;
        let mut spec_ty = 0u32;
        for p in parts.iter() {
            spec_id += if p.id.is_some() { 1 } else { 0 };
            spec_cls += (p.classes.len() + p.attrs.len()) as u32;
            spec_ty += if p.tag.is_some() && p.tag.as_deref() != Some("*") { 1 } else { 0 };
            spec_ty += if p.element.is_some() { 1 } else { 0 };
            for ps in p.pseudos.iter() {
                match ps {
                    Pseudo::Where(_) => {}
                    Pseudo::Is(list) | Pseudo::Not(list) | Pseudo::Has(list) => {
                        let inner = list
                            .iter()
                            .map(|c| c.specificity)
                            .max()
                            .unwrap_or(0);
                        spec_cls += inner / 256;
                        spec_ty += inner % 256;
                    }
                    _ => spec_cls += 1,
                }
            }
        }
        let specificity = spec(spec_id, spec_cls, spec_ty);
        Ok(Complex {
            parts,
            combinators,
            specificity,
        })
    }

    fn parse_compound(&mut self) -> Result<Compound> {
        let mut c = Compound::default();
        let mut first = true;
        loop {
            if self.eof() {
                break;
            }
            match self.peek() {
                b'*' => {
                    self.i += 1;
                    c.tag = Some("*".to_string());
                }
                b'#' => {
                    self.i += 1;
                    c.id = Some(self.parse_ident()?);
                }
                b'.' => {
                    self.i += 1;
                    c.classes.push(self.parse_ident()?);
                }
                b'[' => {
                    self.i += 1;
                    c.attrs.push(self.parse_attr()?);
                }
                b':' => {
                    self.i += 1;
                    let dbl = self.peek() == b':';
                    if dbl {
                        self.i += 1;
                        let name = self.parse_ident()?;
                        c.element = Some(name.to_ascii_lowercase());
                        break;
                    }
                    let name = self.parse_ident()?.to_ascii_lowercase();
                    let args = if self.peek() == b'(' {
                        self.i += 1;
                        let raw = self.parse_balanced()?;
                        Some(raw)
                    } else {
                        None
                    };
                    push_pseudo(&mut c, &name, args.as_deref())?;
                }
                b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'-' if first => {
                    let name = self.parse_ident()?;
                    c.tag = Some(name.to_ascii_lowercase());
                }
                _ => break,
            }
            first = false;
        }
        if c.tag.is_none() && c.matches_anything() {
            return Err("css: empty compound selector".to_string());
        }
        Ok(c)
    }

    fn parse_ident(&mut self) -> Result<String> {
        let start = self.i;
        while !self.eof() {
            let ch = self.peek();
            let ok = ch.is_ascii_alphanumeric()
                || ch == b'_'
                || ch == b'-'
                || ch >= 0x80
                || (ch == b'\\' && self.i + 1 < self.c.len());
            if !ok {
                break;
            }
            if ch == b'\\' {
                self.i += 2;
            } else {
                self.i += 1;
            }
        }
        if self.i == start {
            return Err(format!(
                "css: expected identifier at {:?}",
                &self.c[start..(start + 12).min(self.c.len())]
            ));
        }
        Ok(self.c[start..self.i].to_string())
    }

    fn parse_attr(&mut self) -> Result<AttrSel> {
        self.skip_ws();
        let mut name = self.parse_ident()?;
        // Namespace prefixes (`svg|rect`) are matched on the local name only.
        if let Some((_, rest)) = name.split_once('|') {
            name = rest.to_string();
        }
        let mut sel = AttrSel {
            name: name.to_ascii_lowercase(),
            op: None,
            value: String::new(),
            ci: false,
        };
        self.skip_ws();
        if !self.eof() && self.peek() != b']' {
            let op = self.peek() as char;
            if !"=~|^$*".contains(op) {
                return Err(format!("css: bad attribute operator {op:?}"));
            }
            self.i += 1;
            sel.op = Some(op);
            self.skip_ws();
            let q = self.peek();
            sel.value = if q == b'"' || q == b'\'' {
                self.i += 1;
                let s = self.i;
                while !self.eof() && self.peek() != q {
                    self.i += 1;
                }
                let v = self.c[s..self.i].to_string();
                if !self.eof() {
                    self.i += 1;
                }
                v
            } else {
                let s = self.i;
                while !self.eof() && self.peek() != b']' && self.peek() > b' ' {
                    self.i += 1;
                }
                self.c[s..self.i].trim().to_string()
            };
            self.skip_ws();
            if self.peek() == b'i' || self.peek() == b'I' {
                sel.ci = true;
                self.i += 1;
                self.skip_ws();
            }
        }
        if self.peek() == b']' {
            self.i += 1;
        }
        Ok(sel)
    }

    /// Consume up to the matching close paren, tracking nesting and quotes.
    fn parse_balanced(&mut self) -> Result<String> {
        let mut depth = 1i32;
        let start = self.i;
        let mut quote: Option<char> = None;
        while !self.eof() {
            let ch = self.c[self.i..].chars().next().unwrap_or('\0');
            self.i += ch.len_utf8();
            match quote {
                Some(q) => {
                    if ch == q {
                        quote = None;
                    }
                    continue;
                }
                None => {}
            }
            match ch {
                '"' | '\'' => quote = Some(ch),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(self.c[start..self.i - 1].to_string());
                    }
                }
                _ => {}
            }
        }
        Err("css: unbalanced parentheses in selector".to_string())
    }
}

fn push_pseudo(c: &mut Compound, name: &str, args: Option<&str>) -> Result<()> {
    let ps = match name {
        "root" => Pseudo::Root,
        "empty" => Pseudo::Empty,
        "first-child" => Pseudo::FirstChild,
        "last-child" => Pseudo::LastChild,
        "only-child" => Pseudo::OnlyChild,
        "first-of-type" => Pseudo::FirstOfType,
        "last-of-type" => Pseudo::LastOfType,
        "checked" => Pseudo::Checked,
        "disabled" => Pseudo::Disabled,
        "enabled" => Pseudo::Enabled,
        "required" => Pseudo::Required,
        "read-only" => Pseudo::ReadOnly,
        "hover" => Pseudo::Hover,
        "active" => Pseudo::Active,
        "focus" | "focus-visible" => Pseudo::Focus,
        "link" => Pseudo::Link,
        "any-link" => Pseudo::AnyLink,
        "visited" => Pseudo::Visited,
        "is" | "matches" | "where" => {
            let a = args.ok_or_else(|| format!("css: :{name}() needs a selector"))?;
            let set = parse_list(a)?;
            match name {
                "where" => Pseudo::Where(set),
                _ => Pseudo::Is(set),
            }
        }
        "not" => {
            let a = args.ok_or_else(|| "css: :not() needs a selector".to_string())?;
            Pseudo::Not(parse_list(a)?)
        }
        "has" => {
            let a = args.ok_or_else(|| "css: :has() needs a selector".to_string())?;
            // Relative selectors (`:has(> p)`) keep only their subject; the
            // combinator is dropped because matching tests every descendant.
            let trimmed = a.trim_start();
            let cleaned = trimmed
                .strip_prefix('>')
                .or_else(|| trimmed.strip_prefix('+'))
                .or_else(|| trimmed.strip_prefix('~'))
                .unwrap_or(trimmed);
            Pseudo::Has(parse_list(cleaned.trim())?)
        }
        "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
            let a = args.unwrap_or("");
            let (an, b) = parse_nth(a)?;
            match name {
                "nth-child" => Pseudo::NthChild(an, b),
                "nth-last-child" => Pseudo::NthLastChild(an, b),
                _ => Pseudo::NthOfType(an, b),
            }
        }
        // Anything else (`:before`, `:lang(en)`, `:dir`, vendor pseudo classes)
        // parses but never matches: pages keep rendering instead of dying.
        _ => Pseudo::Unknown,
    };
    c.pseudos.push(ps);
    Ok(())
}

fn parse_list(text: &str) -> Result<Vec<Complex>> {
    let mut out = Vec::new();
    for chunk in split_top_sep(text, ',') {
        if chunk.trim().is_empty() {
            continue;
        }
        let mut p = SelParser {
            c: chunk.as_str(),
            i: 0,
        };
        out.push(p.parse_complex()?);
    }
    if out.is_empty() {
        return Err("css: empty selector list".to_string());
    }
    Ok(out)
}

/// `An+B` (plus the `odd`/`even` keywords).
pub fn parse_nth(text: &str) -> Result<(i32, i32)> {
    let t = text.trim().to_ascii_lowercase().replace(' ', "");
    if t == "odd" {
        return Ok((2, 1));
    }
    if t == "even" {
        return Ok((2, 0));
    }
    if t.is_empty() {
        return Err("css: empty nth()".to_string());
    }
    // Forms: "3", "-n+2", "2n-1", "+3n", "n", "3n+1"
    if let Some(npos) = t.find('n') {
        let a_str = &t[..npos];
        let a: i32 = if a_str.is_empty() || a_str == "+" {
            1
        } else if a_str == "-" {
            -1
        } else {
            a_str.parse().map_err(|_| format!("css: bad nth multiplier {a_str:?}"))?
        };
        let rest = &t[npos + 1..];
        let b: i32 = if rest.is_empty() {
            0
        } else if let Some(v) = rest.strip_prefix('+') {
            if v.is_empty() {
                return Err("css: bad nth() sign".to_string());
            }
            v.parse().map_err(|_| "css: bad nth() offset".to_string())?
        } else if let Some(v) = rest.strip_prefix('-') {
            if v.is_empty() {
                return Err("css: bad nth() sign".to_string());
            }
            -v.parse::<i32>().map_err(|_| "css: bad nth() offset".to_string())?
        } else {
            return Err(format!("css: bad nth() expression {text:?}"));
        };
        Ok((a, b))
    } else {
        let v: i32 = t.parse().map_err(|_| format!("css: bad nth() {text:?}"))?;
        Ok((0, v))
    }
}

/// Public entry point: `dom.matches(node, sel)` goes through here, and so does
/// every selector coming from JS or CDP.
pub fn matches(dom: &Dom, node: usize, set: &SelectorSet) -> bool {
    set.matches(dom, node)
}

pub fn parse_selector(text: &str) -> Result<SelectorSet> {
    SelectorSet::parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Dom;

    fn doc() -> Dom {
        // <html><body><div id=main class="card wide"><p>one</p><p class="sel">two</p></div><ul><li>a</li><li>b</li></ul></body></html>
        let mut d = Dom::new();
        let html = d.create_element("html");
        let body = d.create_element("body");
        let div = d.create_element("div");
        let p1 = d.create_element("p");
        let p2 = d.create_element("p");
        let ul = d.create_element("ul");
        let li1 = d.create_element("li");
        let li2 = d.create_element("li");
        d.append(d.document, html);
        d.append(html, body);
        d.append(body, div);
        d.append(div, p1);
        d.append(div, p2);
        d.append(body, ul);
        d.append(ul, li1);
        d.append(ul, li2);
        let t1 = d.create_text("one");
        let t2 = d.create_text("two");
        d.append(p1, t1);
        d.append(p2, t2);
        d.set_attr(div, "id", "main");
        d.set_attr(div, "class", "card wide");
        d.set_attr(p2, "class", "sel");
        d
    }

    fn find(dom: &Dom, sel: &str) -> Vec<usize> {
        let set = SelectorSet::parse(sel).unwrap_or_else(|e| panic!("{sel}: {e}"));
        dom.elements()
            .into_iter()
            .filter(|&e| set.matches(dom, e))
            .collect()
    }

    #[test]
    fn basic_selectors() {
        let d = doc();
        assert_eq!(find(&d, "div").len(), 1);
        assert_eq!(find(&d, "#main").len(), 1);
        assert_eq!(find(&d, ".card").len(), 1);
        assert_eq!(find(&d, "p.sel").len(), 1);
        assert_eq!(find(&d, "div > p").len(), 2);
        assert_eq!(find(&d, "body p").len(), 2);
        assert_eq!(find(&d, "div p").len(), 2);
        assert_eq!(find(&d, "*").len(), 8);
        assert_eq!(find(&d, "html, li").len(), 3);
        assert_eq!(find(&d, "ul li:last-child").len(), 1);
        assert_eq!(find(&d, "li:first-child").len(), 1);
        assert_eq!(find(&d, "li:nth-child(2)").len(), 1);
        assert_eq!(find(&d, "li:nth-child(odd)").len(), 1);
        assert_eq!(find(&d, "p + p").len(), 1);
        assert_eq!(find(&d, "div ~ ul").len(), 1);
        assert!(SelectorSet::parse("p:not(.sel)").is_ok());
        assert_eq!(find(&d, "p:not(.sel)").len(), 1);
    }

    #[test]
    fn attribute_selectors() {
        let mut d = doc();
        let a = d.create_element("a");
        d.append(d.document, a);
        d.set_attr(a, "href", "https://example.com/x");
        d.set_attr(a, "data-x", "foo bar");
        d.set_attr(a, "lang", "EN-US");
        assert_eq!(find(&d, "a[href]").len(), 1);
        assert_eq!(find(&d, "a[href^=\"https\"]").len(), 1);
        assert_eq!(find(&d, "a[href$=\"/x\"]").len(), 1);
        assert_eq!(find(&d, "a[href*=\"ample\"]").len(), 1);
        assert_eq!(find(&d, "[data-x~=\"bar\"]").len(), 1);
        assert_eq!(find(&d, "[lang=\"en-us\"]").len(), 0);
        assert_eq!(find(&d, "[lang=\"en-us\" i]").len(), 1);
    }

    #[test]
    fn specificity_ordering() {
        let d = doc();
        let id = SelectorSet::parse("#main").unwrap();
        let cls = SelectorSet::parse("div.card.wide").unwrap();
        let ty = SelectorSet::parse("div").unwrap();
        assert!(id.specificity() > cls.specificity());
        assert!(cls.specificity() > ty.specificity());
        // :where() must not add specificity.
        let w = SelectorSet::parse("div:where(.card)").unwrap();
        assert_eq!(w.specificity(), ty.specificity());
    }

    #[test]
    fn nth_parsing() {
        assert_eq!(parse_nth("2n+1").unwrap(), (2, 1));
        assert_eq!(parse_nth("n").unwrap(), (1, 0));
        assert_eq!(parse_nth("-n+3").unwrap(), (-1, 3));
        assert_eq!(parse_nth("3").unwrap(), (0, 3));
        assert_eq!(parse_nth("odd").unwrap(), (2, 1));
        assert!(parse_nth("2x").is_err());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(SelectorSet::parse("").is_err());
        assert!(SelectorSet::parse("div >").is_err());
        assert!(SelectorSet::parse("[[broken").is_err());
        assert!(SelectorSet::parse(":not(").is_err());
    }

    #[test]
    fn functional_pseudos_and_has() {
        let d = doc();
        assert_eq!(find(&d, "div:has(> p)").len(), 1);
        assert_eq!(find(&d, "div:is(div, ul) p").len(), 2);
        assert_eq!(find(&d, ":root").len(), 1);
        assert_eq!(find(&d, "ul:empty").len(), 0);
    }
}
