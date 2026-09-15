//! HTML tokenizer + tree builder in one pass.
//!
//! This implements the tree-construction rules that actually matter on the live
//! web: implied `html`/`head`/`body`, void elements, raw-text elements, the
//! auto-closing paragraph/list/table rules, character references, duplicate
//! attribute dropping, and a hard nesting limit so a pathological
//! `<div>((((((((…` cannot overflow the stack.
//!
//! Not implemented (documented in docs/STATUS.md): foster parenting of text
//! inside tables, `<template>` content, the frameset phases, and `<select>`
//! popup rules. Those constructs still yield a sane tree, just not a
//! byte-identical one to a spec-complete parser.

use crate::dom::Dom;
use crate::html::entities;

/// Elements whose start tag closes an open `<p>` (a subset of "special" tags).
const CLOSES_P: &[&str] = &[
    "address", "article", "aside", "blockquote", "center", "details", "dialog", "dir", "div",
    "dl", "dt", "dd", "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3",
    "h4", "h5", "h6", "header", "hgroup", "hr", "main", "menu", "nav", "ol", "p", "pre",
    "search", "section", "table", "ul", "li",
];

/// Elements that belong to `<head>`; anything else forces `<body>`.
const HEAD_ONLY: &[&str] = &[
    "title", "meta", "link", "style", "base", "basefont", "bgsound", "script", "noscript",
    "template",
];

/// Content model: text verbatim, no character references.
const RAWTEXT: &[&str] = &["script", "style", "xmp", "iframe", "noembed", "noframes", "plaintext"];
/// Text with character references decoded.
const RCDATA: &[&str] = &["textarea", "title"];
/// Like RAWTEXT but also ends only on its own close tag; nested same-name
/// start tags are ignored (`<nobr>` behaves that way in browsers).
const SPECIAL: &[&str] = &["nobr", "select", "optgroup", "option"];

const MAX_DEPTH: usize = 360;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    BeforeHtml,
    InHead,
    AfterHead,
    InBody,
    AfterBody,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Total source bytes.
    pub bytes: usize,
    /// Elements + text nodes created.
    pub nodes: usize,
    /// Tags we recovered from (unbalanced end tags, depth overflow, …).
    pub recovered: usize,
}

#[derive(Clone, Debug)]
struct Tag<'a> {
    name: &'a str,
    self_closing: bool,
    attrs: Vec<(String, String)>,
}

pub struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    i: usize,
    phase: Phase,
    stats: Stats,
    html: Option<usize>,
    head: Option<usize>,
    body: Option<usize>,
    stack: Vec<usize>,
}

/// Parse a complete document. The root `<html>` element is appended to
/// `dom.document`.
pub fn parse_document(dom: &mut Dom, src: &str) -> Stats {
    let mut p = Parser::new(src);
    p.run(dom, None)
}

/// Parse `src` as the inner HTML of `parent` (`innerHTML`, CDP
/// `DOM.setOuterHTML`). No implied html/head/body is created.
pub fn parse_fragment(dom: &mut Dom, parent: usize, src: &str) -> Stats {
    let mut p = Parser::new(src);
    p.phase = Phase::InBody;
    p.body = Some(parent);
    p.run(dom, Some(parent))
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Parser<'a> {
        Parser {
            src,
            b: src.as_bytes(),
            i: 0,
            phase: Phase::BeforeHtml,
            stats: Stats::default(),
            html: None,
            head: None,
            body: None,
            stack: Vec::new(),
        }
    }

    fn run(&mut self, dom: &mut Dom, fragment: Option<usize>) -> Stats {
        self.stats.bytes = self.src.len();
        match fragment {
            Some(parent) => {
                self.stack.push(parent);
                dom.clear_children(parent);
            }
            None => {
                // A doctype/comment before <html> still lands in the document.
            }
        }
        while self.i < self.b.len() {
            if self.b[self.i] == b'<' {
                self.tag(dom);
            } else {
                let start = self.i;
                while self.i < self.b.len() && self.b[self.i] != b'<' {
                    self.i += 1;
                }
                let raw = &self.src[start..self.i];
                self.text(dom, raw);
            }
        }
        self.stats
    }

    // ---- tree helpers -----------------------------------------------------

    fn ensure_html(&mut self, dom: &mut Dom, t: Option<&Tag<'_>>) {
        if self.html.is_some() {
            return;
        }
        let el = match t {
            Some(t) if t.name == "html" => {
                let e = dom.create_element("html");
                for (k, v) in &t.attrs {
                    dom.set_attr(e, k, v);
                }
                e
            }
            _ => dom.create_element("html"),
        };
        dom.append(dom.document, el);
        self.html = Some(el);
        self.stack = vec![el];
        self.stats.nodes += 1;
        if self.head.is_none() {
            self.open_head(dom);
        }
    }

    fn open_head(&mut self, dom: &mut Dom) {
        let html = match self.html {
            Some(h) => h,
            None => return,
        };
        let head = dom.create_element("head");
        dom.append(html, head);
        self.head = Some(head);
    }

    fn ensure_body(&mut self, dom: &mut Dom, t: Option<&Tag<'_>>) -> usize {
        if let Some(b) = self.body {
            return b;
        }
        let html = match self.html {
            Some(h) => h,
            None => {
                self.ensure_html(dom, None);
                self.html.unwrap()
            }
        };
        if let Some(h) = self.head {
            if self.phase == Phase::InHead {
                self.phase = Phase::InBody;
                self.stack.retain(|&n| n != h);
            }
        }
        let body = match t {
            Some(t) if t.name == "body" => {
                let e = dom.create_element("body");
                for (k, v) in &t.attrs {
                    dom.set_attr(e, k, v);
                }
                e
            }
            _ => dom.create_element("body"),
        };
        dom.append(html, body);
        self.body = Some(body);
        self.stack = vec![html, body];
        self.stats.nodes += 1;
        self.phase = Phase::InBody;
        body
    }

    /// Where the next node goes: the deepest open element that is not the
    /// synthetic head placeholder.
    fn insertion_point(&mut self, dom: &mut Dom) -> usize {
        loop {
            let top = match self.stack.last() {
                Some(t) => *t,
                None => return self.ensure_body(dom, None),
            };
            let tag = dom.tag(top);
            if tag.is_empty() {
                return self.ensure_body(dom, None);
            }
            if tag == "head" && self.phase != Phase::InHead {
                // Content after </head>: reparent into <body>.
                self.ensure_body(dom, None);
                continue;
            }
            return top;
        }
    }

    fn text(&mut self, dom: &mut Dom, raw: &str) {
        if raw.is_empty() {
            return;
        }
        if self.html.is_none() {
            if raw.trim().is_empty() {
                return;
            }
            self.ensure_html(dom, None);
        }
        let parent = self.insertion_point(dom);
        let ptag = dom.tag(parent);
        if raw.trim().is_empty() && (ptag == "head" || ptag == "html") {
            return;
        }
        let decoded = entities::decode(raw, false);
        if decoded.is_empty() {
            return;
        }
        self.stats.nodes += 1;
        let last = dom.children(parent).pop();
        let merge = match last {
            Some(id) => dom
                .node(id)
                .map(|n| n.kind == crate::dom::Kind::Text)
                .unwrap_or(false),
            None => false,
        };
        match last {
            Some(id) if merge => dom.append_text(id, &decoded),
            _ => {
                let t = dom.create_text(&decoded);
                dom.append(parent, t);
            }
        }
    }

    // ---- tokenizer --------------------------------------------------------

    fn tag(&mut self, dom: &mut Dom) {
        let save = self.i;
        self.i += 1; // '<'
        if self.i >= self.b.len() {
            self.i = save;
            self.text(dom, "<");
            self.i = save + 1;
            return;
        }
        match self.b[self.i] {
            b'!' => {
                if self.src[self.i..].starts_with("!--") {
                    self.comment(dom);
                } else if self.src[self.i..].to_ascii_uppercase().starts_with("!DOCTYPE")
                    || self.src[self.i..].starts_with("!doctype")
                {
                    self.doctype(dom);
                } else if self.src[self.i..].starts_with("![CDATA[") {
                    self.i += 8;
                    let start = self.i;
                    while self.i < self.b.len() && !self.src[self.i..].starts_with("]]>") {
                        self.i += 1;
                    }
                    let t = self.src[start..self.i].to_string();
                    self.i = (self.i + 3).min(self.b.len());
                    let parent = self.insertion_point(dom);
                    let n = dom.create_text(&t);
                    dom.append(parent, n);
                } else {
                    // Bogus comment.
                    let start = self.i;
                    while self.i < self.b.len() && self.b[self.i] != b'>' {
                        self.i += 1;
                    }
                    let t = dom.create_comment(&self.src[start..self.i]);
                    self.i = (self.i + 1).min(self.b.len());
                    let parent = self.insertion_point(dom);
                    dom.append(parent, t);
                }
            }
            b'/' => {
                self.i += 1;
                let name = self.tag_name();
                self.skip_to_gt();
                if !name.is_empty() {
                    self.end_tag(dom, &name);
                }
            }
            b'?' => {
                // Processing instruction: bogus comment.
                let start = self.i;
                while self.i < self.b.len() && self.b[self.i] != b'>' {
                    self.i += 1;
                }
                let t = dom.create_comment(&self.src[start..self.i]);
                self.i = (self.i + 1).min(self.b.len());
                let parent = self.insertion_point(dom);
                dom.append(parent, t);
            }
            c if c.is_ascii_alphabetic() => {
                let t = self.start_tag();
                self.start_tag_handle(dom, &t);
            }
            _ => {
                // A '<' that starts nothing: literal text.
                self.i = save;
                self.text(dom, "<");
                self.i = save + 1;
            }
        }
    }

    fn tag_name(&mut self) -> String {
        let start = self.i;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c.is_ascii_alphanumeric() || c == b'-' || c == b':' || c == b'_' {
                self.i += 1;
            } else {
                break;
            }
        }
        self.src[start..self.i].to_ascii_lowercase()
    }

    fn skip_to_gt(&mut self) {
        while self.i < self.b.len() && self.b[self.i] != b'>' {
            self.i += 1;
        }
        if self.i < self.b.len() {
            self.i += 1;
        }
    }

    fn start_tag(&mut self) -> Tag<'a> {
        let src: &'a str = self.src;
        let name_start = self.i;
        let name_end = {
            let mut j = self.i;
            while j < self.b.len() {
                let c = self.b[j];
                if c.is_ascii_alphanumeric() || c == b'-' || c == b':' || c == b'_' {
                    j += 1;
                } else {
                    break;
                }
            }
            j
        };
        let name = &src[name_start..name_end];
        self.i = name_end;
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_closing = false;
        loop {
            while self.i < self.b.len() && (self.b[self.i] as char).is_whitespace() {
                self.i += 1;
            }
            if self.i >= self.b.len() {
                break;
            }
            match self.b[self.i] {
                b'>' => {
                    self.i += 1;
                    break;
                }
                b'/' => {
                    self_closing = true;
                    self.i += 1;
                    continue;
                }
                b'<' => {
                    // `<b <i>` - treat the '<' as starting a new tag.
                    break;
                }
                _ => {}
            }
            let ns = self.i;
            while self.i < self.b.len() {
                let c = self.b[self.i];
                if c == b'=' || c == b'>' || c == b'/' || (c as char).is_whitespace() {
                    break;
                }
                self.i += 1;
            }
            if self.i == ns {
                self.i += 1;
                continue;
            }
            let key = src[ns..self.i].to_ascii_lowercase();
            while self.i < self.b.len() && (self.b[self.i] as char).is_whitespace() {
                self.i += 1;
            }
            let mut value = String::new();
            if self.i < self.b.len() && self.b[self.i] == b'=' {
                self.i += 1;
                while self.i < self.b.len() && (self.b[self.i] as char).is_whitespace() {
                    self.i += 1;
                }
                if self.i < self.b.len() && (self.b[self.i] == b'"' || self.b[self.i] == b'\'') {
                    let q = self.b[self.i];
                    self.i += 1;
                    let vs = self.i;
                    while self.i < self.b.len() && self.b[self.i] != q {
                        self.i += 1;
                    }
                    value = src[vs..self.i].to_string();
                    self.i = (self.i + 1).min(self.b.len());
                } else {
                    let vs = self.i;
                    while self.i < self.b.len() {
                        let c = self.b[self.i];
                        if c == b'>' || c == b'/' || (c as char).is_whitespace() {
                            break;
                        }
                        self.i += 1;
                    }
                    value = src[vs..self.i].to_string();
                }
            }
            let value = entities::decode(&value, true);
            // Duplicate attributes: the first occurrence wins.
            if !attrs.iter().any(|(k, _)| *k == key) {
                attrs.push((key, value));
            }
        }
        Tag {
            name,
            self_closing,
            attrs,
        }
    }

    fn comment(&mut self, dom: &mut Dom) {
        self.i += 3; // "<!--"
        let start = self.i;
        let text = match self.src[start..].find("-->") {
            Some(k) => {
                let t = self.src[start..start + k].to_string();
                self.i = start + k + 3;
                t
            }
            None => {
                let t = self.src[start..].to_string();
                self.i = self.b.len();
                t
            }
        };
        let parent = if self.html.is_none() {
            dom.document
        } else if self.phase == Phase::InHead {
            self.head.unwrap_or(dom.document)
        } else {
            self.insertion_point(dom)
        };
        let node = dom.create_comment(&text);
        dom.append(parent, node);
    }

    fn doctype(&mut self, dom: &mut Dom) {
        let start = self.i;
        while self.i < self.b.len() && self.b[self.i] != b'>' {
            self.i += 1;
        }
        let raw = self.src[start + 8..self.i].trim().to_string();
        if self.i < self.b.len() {
            self.i += 1;
        }
        if let Some(name) = raw.split_whitespace().next() {
            let n = dom.create_doctype(name);
            dom.append(dom.document, n);
        }
        self.phase = Phase::BeforeHtml;
    }

    // ---- tree construction ------------------------------------------------

    fn start_tag_handle(&mut self, dom: &mut Dom, t: &Tag<'a>) {
        let name = t.name;
        match self.phase {
            Phase::BeforeHtml => {
                if name == "html" {
                    self.ensure_html(dom, Some(t));
                    self.phase = Phase::InHead;
                    return;
                }
                self.ensure_html(dom, None);
                self.phase = Phase::InHead;
            }
            Phase::InHead | Phase::AfterHead => {
                if HEAD_ONLY.contains(&name) {
                    if self.head.is_none() {
                        self.open_head(dom);
                    }
                    if self.phase != Phase::InHead {
                        self.phase = Phase::InHead;
                        self.stack.retain(|&n| Some(n) != self.body);
                    }
                    let parent = self.head.unwrap_or_else(|| self.insertion_point(dom));
                    let el = dom.create_element(name);
                    for (k, v) in &t.attrs {
                        dom.set_attr(el, k, v);
                    }
                    dom.append(parent, el);
                    self.stats.nodes += 1;
                    if name == "template" {
                        // `<template>` contents are kept as real children (our
                        // engine has no template instantiation).
                    }
                    if !t.self_closing && !crate::dom::VOID.contains(&name) {
                        self.stack = vec![self.html.unwrap_or(parent), el];
                        if RAWTEXT.contains(&name) || RCDATA.contains(&name) {
                            self.consume_rawtext(dom, el, name, RCDATA.contains(&name));
                            self.stack.pop();
                        }
                    }
                    return;
                }
                if name == "head" {
                    return;
                }
                if name == "body" {
                    self.ensure_body(dom, Some(t));
                    return;
                }
                self.ensure_body(dom, None);
            }
            Phase::InBody | Phase::AfterBody => {}
        }
        if self.phase == Phase::BeforeHtml || self.html.is_none() {
            self.ensure_html(dom, None);
        }
        if self.phase == Phase::InHead || self.phase == Phase::AfterHead {
            self.ensure_body(dom, None);
        }
        match name {
            "html" => {
                if let Some(h) = self.html {
                    for (k, v) in &t.attrs {
                        dom.set_attr(h, k, v);
                    }
                }
                return;
            }
            "head" | "body" if self.body.is_some() => return,
            _ => {}
        }
        if CLOSES_P.contains(&name) {
            self.close_if_in(dom, &["p"]);
        }
        match name {
            "li" => self.close_if_in(dom, &["li"]),
            "dd" | "dt" => self.close_if_in(dom, &["dd", "dt"]),
            "option" => self.close_if_in(dom, &["option"]),
            "optgroup" => {
                self.close_if_in(dom, &["option"]);
                self.close_if_in(dom, &["optgroup"]);
            }
            "td" | "th" => self.close_if_in(dom, &["td", "th", "caption", "col", "colgroup"]),
            "tr" => self.close_if_in(dom, &["td", "th", "tr", "caption"]),
            "thead" | "tbody" | "tfoot" => self.close_if_in(
                dom,
                &["td", "th", "tr", "thead", "tbody", "tfoot", "caption", "colgroup", "col"],
            ),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_if_in(dom, &["h1", "h2", "h3", "h4", "h5", "h6"])
            }
            "nobr" => self.close_if_in(dom, &["nobr"]),
            "p" if self.top_is(dom, "p") => self.close_if_in(dom, &["p"]),
            _ => {}
        }
        let parent = self.insertion_point(dom);
        let el = dom.create_element(name);
        for (k, v) in &t.attrs {
            dom.set_attr(el, k, v);
        }
        dom.append(parent, el);
        self.stats.nodes += 1;
        if t.self_closing || crate::dom::VOID.contains(&name) {
            return;
        }
        if self.stack.len() >= MAX_DEPTH {
            self.stats.recovered += 1;
            return;
        }
        self.stack.push(el);
        if RAWTEXT.contains(&name) || RCDATA.contains(&name) || SPECIAL.contains(&name) {
            let decoded = !RAWTEXT.contains(&name);
            self.consume_rawtext(dom, el, name, decoded);
        }
    }

    /// `<script>`, `<style>`, `<textarea>`, `<title>` and friends: everything up
    /// to the matching end tag becomes a single text node.
    fn consume_rawtext(&mut self, dom: &mut Dom, el: usize, name: &str, decode: bool) {
        let lower = self.src[self.i..].to_ascii_lowercase();
        let close = format!("</{name}");
        let rel = match lower.find(&close) {
            Some(k) => k,
            None => self.src.len() - self.i,
        };
        let abs = self.i + rel;
        let content = &self.src[self.i..abs];
        if !content.is_empty() {
            let text = if decode {
                entities::decode(content, false)
            } else {
                content.to_string()
            };
            let t = dom.create_text(&text);
            dom.append(el, t);
            self.stats.nodes += 1;
        }
        self.i = abs;
        self.skip_to_gt();
        // The element is already on the stack; pop it here.
        if self.stack.last() == Some(&el) {
            self.stack.pop();
        }
    }

    fn end_tag(&mut self, dom: &mut Dom, name: &str) {
        match name {
            "head" => {
                self.phase = Phase::AfterHead;
                if let Some(h) = self.head {
                    self.stack.retain(|&n| n != h);
                }
                return;
            }
            "body" => {
                self.phase = Phase::AfterBody;
                return;
            }
            "html" => {
                self.phase = Phase::AfterBody;
                return;
            }
            _ => {}
        }
        if crate::dom::VOID.contains(&name) {
            return;
        }
        if self.top_is(dom, name) {
            self.stack.pop();
            return;
        }
        if let Some(pos) = self.stack.iter().rposition(|&n| dom.tag(n) == name) {
            if pos == 0 {
                return;
            }
            self.stack.truncate(pos);
            self.stats.recovered += 1;
            return;
        }
        if name == "p" {
            let t = Tag {
                name: "p",
                self_closing: false,
                attrs: Vec::new(),
            };
            self.start_tag_handle(dom, &t);
            self.stack.pop();
            return;
        }
        self.stats.recovered += 1;
    }

    fn top_is(&self, dom: &Dom, name: &str) -> bool {
        match self.stack.last() {
            Some(&n) => dom.tag(n) == name,
            None => false,
        }
    }

    fn close_if_in(&mut self, dom: &Dom, set: &[&str]) {
        loop {
            if self.stack.len() <= 1 {
                return;
            }
            let top = *self.stack.last().unwrap();
            if set.contains(&dom.tag(top).as_str()) {
                self.stack.pop();
            } else {
                return;
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Dom {
        let mut d = Dom::new();
        parse_document(&mut d, html);
        d
    }

    #[test]
    fn implied_structures() {
        let d = parse("<title>T</title><p>hi");
        assert_eq!(d.serialize(d.document).replace("\n", ""), "<p>hi</p>");
        let html = d.child(d.document, 1).unwrap();
        assert_eq!(d.tag(html), "html");
        assert_eq!(d.child(html, 0).map(|n| d.tag(n)), Some("head".to_string()));
        assert_eq!(d.child(html, 1).map(|n| d.tag(n)), Some("body".to_string()));
        let head = d.child(html, 0).unwrap();
        assert_eq!(d.text_content(head), "T");
        let body = d.child(html, 1).unwrap();
        assert_eq!(d.text_content(body), "hi");
    }

    #[test]
    fn doctype_and_comments() {
        let d = parse("<!DOCTYPE html><!-- hi --><html><body>x</body></html>");
        let dt = d.child(d.document, 0).unwrap();
        assert_eq!(d.node(dt).unwrap().kind, crate::dom::Kind::DocType);
        assert_eq!(d.node(dt).unwrap().data, "html");
        let html = d.html_root().unwrap();
        let comment = d
            .descendants(html)
            .into_iter()
            .find(|&n| {
                d.node(n)
                    .map(|x| x.kind == crate::dom::Kind::Comment)
                    .unwrap_or(false)
            })
            .expect("comment survives parsing");
        assert_eq!(d.node(comment).unwrap().data, " hi ");
        assert_eq!(d.text_content(d.body().unwrap()), "x");
    }

    fn paragraph_and_list_auto_close() {
        let d = parse("<body><p>a<p>b<ul><li>x<li>y</ul>");
        let body = d.body().unwrap();
        let kids = d.children(body);
        assert_eq!(kids.len(), 2); // p, p, ul -> three
        assert_eq!(d.tag(kids[0]), "p");
        let ul = kids[1];
        assert_eq!(d.children(ul).len(), 2);
        assert_eq!(d.text_content(d.children(ul)[0]), "x");
        assert_eq!(d.text_content(d.children(ul)[1]), "y");
    }

    #[test]
    fn table_cells_auto_close() {
        let d = parse("<table><tr><td>1<td>2<tr><td>3</table>");
        let table = d
            .descendants(d.body().unwrap())
            .into_iter()
            .find(|&n| d.tag(n) == "table")
            .unwrap();
        let rows: Vec<usize> = d
            .children(table)
            .into_iter()
            .filter(|&n| d.tag(n) == "tr")
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(d.children(rows[0]).len(), 2);
        assert_eq!(d.text_content(d.children(rows[0])[1]), "2");
    }

    #[test]
    fn attributes_and_entities() {
        let d = parse(r#"<a href=/x?p=1&q=2 title="a > b" data-X=yes disabled>r&amp;s</a>"#);
        let body = d.body().unwrap();
        let a = d.children(body)[0];
        assert_eq!(d.attr(a, "href").unwrap(), "/x?p=1&q=2");
        assert_eq!(d.attr(a, "title").unwrap(), "a > b");
        assert_eq!(d.attr(a, "data-x").unwrap(), "yes");
        assert_eq!(d.attr(a, "disabled").unwrap(), "");
        assert_eq!(d.text_content(a), "r&s");
    }

    #[test]
    fn duplicate_attributes_keep_first() {
        let d = parse(r#"<div class="one" CLASS="two">x</div>"#);
        let div = d.children(d.body().unwrap())[0];
        assert_eq!(d.attr(div, "class").unwrap(), "one");
    }

    #[test]
    fn raw_text_elements() {
        let d = parse("<script>if (a<b && c>0) { x = '</span>'; }</script><style>a{color:red}</style><p>after");
        let head_text = d.text_content(d.child(d.html_root().unwrap(), 0).unwrap());
        assert!(head_text.contains("a<b"), "{head_text}");
        assert!(head_text.contains("'</span>'"), "script must not end early");
        assert!(head_text.contains("color:red"));
        let body = d.body().unwrap();
        assert_eq!(d.text_content(d.children(body)[0]), "after");
    }

    #[test]
    fn textarea_decodes_entities() {
        let d = parse("<textarea>&lt;b&gt;</textarea>");
        let ta = d.descendants(d.body().unwrap()).into_iter().find(|&n| d.tag(n) == "textarea").unwrap();
        assert_eq!(d.text_content(ta), "<b>");
    }

    #[test]
    fn stray_end_tags_recovered() {
        let mut d = Dom::new();
        let st = parse_document(&mut d, "<div><span>x</p></div></body></html></nope>");
        assert!(st.recovered > 0, "{:?}", st);
        assert_eq!(d.text_content(d.body().unwrap()), "x");
    }

    #[test]
    fn unmatched_close_p_creates_one() {
        let d = parse("<body>text</p>more");
        let body = d.body().unwrap();
        let tags: Vec<String> = d.children(body).iter().map(|&n| d.tag(n)).collect();
        assert!(tags.contains(&"p".to_string()), "{tags:?}");
    }

    #[test]
    fn deep_nesting_is_capped_not_fatal() {
        let src = "<b>".repeat(2000) + "x" + &"</b>".repeat(2000);
        let mut d = Dom::new();
        let st = parse_document(&mut d, &src);
        assert!(st.nodes > 0);
        assert_eq!(d.text_content(d.body().unwrap()), "x");
    }

    #[test]
    fn self_closing_div_is_not_self_closing() {
        // HTML (unlike XML) ignores the slash on non-void elements.
        let d = parse("<div/>after");
        let body = d.body().unwrap();
        assert_eq!(d.text_content(body), "after");
        let div = d.children(body)[0];
        assert_eq!(d.tag(div), "div");
        assert_eq!(d.text_content(div), "after");
    }

    #[test]
    fn fragments() {
        let mut d = Dom::new();
        let host = d.create_element("div");
        d.append(d.document, host);
        parse_fragment(&mut d, host, "<p>one</p><p>two");
        assert_eq!(d.children(host).len(), 2);
        assert_eq!(d.text_content(d.children(host)[1]), "two");
        // innerHTML replacement clears previous content
        parse_fragment(&mut d, host, "<span>x</span>");
        assert_eq!(d.children(host).len(), 1);
        assert_eq!(d.tag(d.children(host)[0]), "span");
    }

    #[test]
    fn nested_same_name_inside_rawtext_is_ignored() {
        let d = parse("<div><p>a<div>b<p>c</div></div>");
        let body = d.body().unwrap();
        // The first div must close at </div>, leaving no extra wrappers.
        assert_eq!(d.tag(d.children(body)[0]), "div");
        assert_eq!(d.text_content(body).replace(' ', ""), "abc");
    }
}
