//! The document tree: one flat arena of nodes, indices as handles.
//!
//! This is the same shape Servo uses (`ScriptThread` + indexable nodes) because it
//! sidesteps the borrow checker: layout, CSS matching, painting and the JS bridge
//! all want to read/mutate the tree at once, and `usize` handles make that trivial
//! and cheap. Node ids handed to CDP (`backendNodeId`) are these indices.

use crate::css::selector::{self, SelectorSet};
use crate::util::Result;
use std::collections::HashMap;

/// Interaction state, consulted by `:hover` / `:focus` / `:active`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ElementState {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Document,
    Element,
    Text,
    Comment,
    Doctype,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: usize,
    pub kind: Kind,
    /// Lowercase tag name for elements (SVG camelCase names are restored on read).
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    /// Text/Comment/Doctype payload.
    pub data: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// False once detached; stale handles then read as nothing.
    pub alive: bool,
    pub svg: bool,
}

impl Node {
    pub fn is_element(&self) -> bool {
        self.kind == Kind::Element
    }
}

#[derive(Clone, Debug)]
pub struct Dom {
    pub nodes: Vec<Node>,
    pub document: usize,
    /// Bumped by every mutation; style/cache invalidation keys off it.
    pub revision: u64,
    next_id: usize,
    pub states: HashMap<usize, ElementState>,
    pub focus: Option<usize>,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    pub fn new() -> Dom {
        let mut d = Dom {
            nodes: Vec::with_capacity(256),
            document: 0,
            revision: 0,
            next_id: 1,
            states: HashMap::new(),
            focus: None,
        };
        d.document = d.alloc(Kind::Document, "#document".to_string());
        d
    }

    fn alloc(&mut self, kind: Kind, tag: String) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            id,
            kind,
            tag,
            attrs: Vec::new(),
            data: String::new(),
            parent: None,
            children: Vec::new(),
            alive: true,
            svg: false,
        });
        self.next_id += 1;
        id
    }

    pub fn create_element(&mut self, tag: &str) -> usize {
        let lower = tag.to_ascii_lowercase();
        let svg = self.inside_svg_flag();
        let id = self.alloc(Kind::Element, lower);
        self.nodes[id].svg = svg;
        id
    }

    /// `<svg>` subtree elements keep camelCase attribute names.
    fn inside_svg_flag(&self) -> bool {
        false
    }

    pub fn create_text(&mut self, text: &str) -> usize {
        let id = self.alloc(Kind::Text, "#text".to_string());
        self.nodes[id].data = text.to_string();
        id
    }

    pub fn create_comment(&mut self, text: &str) -> usize {
        let id = self.alloc(Kind::Comment, "#comment".to_string());
        self.nodes[id].data = text.to_string();
        id
    }

    pub fn create_doctype(&mut self, text: &str) -> usize {
        let id = self.alloc(Kind::Doctype, "#doctype".to_string());
        self.nodes[id].data = text.to_string();
        id
    }

    pub fn node(&self, id: usize) -> Option<&Node> {
        self.nodes.get(id).filter(|n| n.alive)
    }

    pub fn node_mut(&mut self, id: usize) -> Option<&mut Node> {
        match self.nodes.get_mut(id) {
            Some(n) if n.alive => Some(n),
            _ => None,
        }
    }

    pub fn is_element(&self, id: usize) -> bool {
        matches!(self.node(id), Some(n) if n.kind == Kind::Element)
    }

    pub fn tag(&self, id: usize) -> String {
        self.node(id).map(|n| n.tag.clone()).unwrap_or_default()
    }

    /// Append text to an existing text node (the tokenizer merges runs).
    pub fn append_text(&mut self, id: usize, text: &str) {
        if let Some(n) = self.node_mut(id) {
            n.data.push_str(text);
        }
    }

    /// The root `<html>` element, if the document has been parsed.
    pub fn html_root(&self) -> Option<usize> {
        self.children(self.document)
            .into_iter()
            .find(|&n| self.tag(n) == "html")
    }

    pub fn head(&self) -> Option<usize> {
        let html = self.html_root()?;
        self.children(html)
            .into_iter()
            .find(|&n| self.tag(n) == "head")
    }

    pub fn body(&self) -> Option<usize> {
        let html = self.html_root()?;
        self.children(html)
            .into_iter()
            .find(|&n| self.tag(n) == "body")
    }

    /// Node count (arena size) - styles and layout index by node id.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn mark_dirty(&mut self) {
        self.revision += 1;
    }

    pub fn state(&self, id: usize) -> ElementState {
        self.states.get(&id).copied().unwrap_or_default()
    }

    pub fn set_state(&mut self, id: usize, st: ElementState) {
        if st == ElementState::default() {
            self.states.remove(&id);
        } else {
            self.states.insert(id, st);
        }
    }

    /// Move focus; the previously focused node loses `:focus`.
    pub fn set_focus(&mut self, id: Option<usize>) {
        if let Some(prev) = self.focus {
            let mut s = self.state(prev);
            s.focused = false;
            self.set_state(prev, s);
        }
        self.focus = id;
        if let Some(n) = id {
            let mut s = self.state(n);
            s.focused = true;
            self.set_state(n, s);
        }
        self.mark_dirty();
    }

    /// Every element below `id` in document order (used by `:has()`).
    pub fn descendants(&self, id: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let kids = self.children(id);
        for k in kids {
            if self.is_element(k) {
                out.push(k);
            }
            out.extend(self.descendants(k));
        }
        out
    }

    pub fn append(&mut self, parent: usize, child: usize) {
        self.detach(child);
        if let Some(c) = self.nodes.get_mut(child) {
            c.parent = Some(parent);
        }
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.push(child);
        }
        self.propagate_svg(child, parent);
        self.mark_dirty();
    }

    fn propagate_svg(&mut self, child: usize, parent: usize) {
        let svg = self
            .node(parent)
            .map(|p| p.svg || p.tag == "svg")
            .unwrap_or(false);
        if svg {
            self.set_svg_subtree(child, true);
        }
    }

    fn set_svg_subtree(&mut self, id: usize, svg: bool) {
        if let Some(n) = self.nodes.get_mut(id) {
            n.svg = svg;
            let kids = n.children.clone();
            for k in kids {
                self.set_svg_subtree(k, svg);
            }
        }
    }

    pub fn insert_before(&mut self, parent: usize, child: usize, reference: Option<usize>) {
        self.detach(child);
        if let Some(c) = self.nodes.get_mut(child) {
            c.parent = Some(parent);
        }
        if let Some(p) = self.nodes.get_mut(parent) {
            let at = match reference {
                Some(r) => p.children.iter().position(|&c| c == r).unwrap_or(p.children.len()),
                None => p.children.len(),
            };
            p.children.insert(at, child);
        }
        self.mark_dirty();
    }

    /// Remove from the parent's child list (the node stays usable for reinsertion).
    pub fn detach(&mut self, child: usize) {
        let parent = match self.node(child) {
            Some(n) => n.parent,
            None => None,
        };
        if let Some(p) = parent {
            if let Some(pn) = self.nodes.get_mut(p) {
                pn.children.retain(|&c| c != child);
            }
        }
        if let Some(cn) = self.nodes.get_mut(child) {
            cn.parent = None;
        }
        self.mark_dirty();
    }

    pub fn remove(&mut self, child: usize) {
        self.detach(child);
        self.kill(child);
    }

    fn kill(&mut self, id: usize) {
        let kids = match self.nodes.get(id) {
            Some(n) => n.children.clone(),
            None => return,
        };
        if let Some(n) = self.nodes.get_mut(id) {
            n.alive = false;
            n.children.clear();
        }
        for k in kids {
            self.kill(k);
        }
    }

    pub fn parent(&self, id: usize) -> Option<usize> {
        self.node(id).and_then(|n| n.parent)
    }

    pub fn attr(&self, id: usize, name: &str) -> Option<String> {
        let n = self.node(id)?;
        let lower = name.to_ascii_lowercase();
        n.attrs
            .iter()
            .find(|(k, _)| k == name || k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.clone())
    }

    pub fn has_attr(&self, id: usize, name: &str) -> bool {
        self.attr(id, name).is_some()
    }

    pub fn set_attr(&mut self, id: usize, name: &str, value: &str) {
        let lower = name.to_ascii_lowercase();
        if let Some(n) = self.nodes.get_mut(id) {
            match n.attrs.iter_mut().find(|(k, _)| *k == name || *k == lower) {
                Some(slot) => slot.1 = value.to_string(),
                None => n.attrs.push((lower, value.to_string())),
            }
        }
        self.mark_dirty();
    }

    pub fn remove_attr(&mut self, id: usize, name: &str) {
        let lower = name.to_ascii_lowercase();
        if let Some(n) = self.nodes.get_mut(id) {
            n.attrs.retain(|(k, _)| *k != name && *k != lower);
        }
        self.mark_dirty();
    }

    pub fn class_list(&self, id: usize) -> Vec<String> {
        match self.attr(id, "class") {
            Some(c) => c.split_whitespace().map(|s| s.to_string()).collect(),
            None => Vec::new(),
        }
    }

    pub fn has_class(&self, id: usize, name: &str) -> bool {
        match self.node(id) {
            Some(n) => n
                .attrs
                .iter()
                .any(|(k, v)| k == "class" && v.split_whitespace().any(|c| c == name)),
            None => false,
        }
    }

    pub fn get_element_by_id(&self, id: &str) -> Option<usize> {
        for n in self.nodes.iter().filter(|n| n.alive && n.kind == Kind::Element) {
            if n.attrs.iter().any(|(k, v)| k == "id" && v == id) {
                return Some(n.id);
            }
        }
        None
    }

    pub fn get_elements_by_class_name(&self, name: &str) -> Vec<usize> {
        self.elements()
            .into_iter()
            .filter(|&e| self.has_class(e, name))
            .collect()
    }

    pub fn get_elements_by_tag_name(&self, tag: &str) -> Vec<usize> {
        let t = tag.to_ascii_lowercase();
        self.elements()
            .into_iter()
            .filter(|&e| self.node(e).map(|n| n.tag == t).unwrap_or(false))
            .collect()
    }

    /// Pre-order list of every element in the document.
    pub fn elements(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![self.document];
        while let Some(id) = stack.pop() {
            if let Some(n) = self.node(id) {
                if n.kind == Kind::Element {
                    out.push(id);
                }
                let kids: Vec<usize> = n.children.iter().copied().collect();
                for k in kids.into_iter().rev() {
                    stack.push(k);
                }
            }
        }
        out
    }

    /// Every node (including text) in document order.
    pub fn all_nodes(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.walk(self.document, &mut out);
        out
    }

    fn walk(&self, id: usize, out: &mut Vec<usize>) {
        out.push(id);
        if let Some(n) = self.node(id) {
            for c in n.children.iter() {
                self.walk(*c, out);
            }
        }
    }

    /// The `index`-th child (`element.children[i]` in the DOM).
    pub fn child(&self, id: usize, index: usize) -> Option<usize> {
        self.children(id).get(index).copied()
    }

    pub fn children(&self, id: usize) -> Vec<usize> {
        self.node(id).map(|n| n.children.clone()).unwrap_or_default()
    }

    pub fn element_children(&self, id: usize) -> Vec<usize> {
        self.children(id)
            .into_iter()
            .filter(|&c| self.is_element(c))
            .collect()
    }

    pub fn first_element_child(&self, id: usize) -> Option<usize> {
        self.children(id).into_iter().find(|&c| self.is_element(c))
    }

    pub fn next_element_sibling(&self, id: usize) -> Option<usize> {
        let p = self.parent(id)?;
        let kids = self.children(p);
        let i = kids.iter().position(|&c| c == id)?;
        kids[i + 1..].iter().copied().find(|&c| self.is_element(c))
    }

    pub fn prev_element_sibling(&self, id: usize) -> Option<usize> {
        let p = self.parent(id)?;
        let kids = self.children(p);
        let i = kids.iter().position(|&c| c == id)?;
        let mut found = None;
        for &c in &kids[..i] {
            if self.is_element(c) {
                found = Some(c);
            }
        }
        found
    }

    /// Concatenated text of a subtree, as `textContent` does.
    pub fn text_content(&self, id: usize) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: usize, out: &mut String) {
        if let Some(n) = self.node(id) {
            match n.kind {
                Kind::Text | Kind::Comment => out.push_str(&n.data),
                _ => {
                    for c in n.children.iter() {
                        self.collect_text(*c, out);
                    }
                }
            }
        }
    }

    pub fn set_text_content(&mut self, id: usize, text: &str) {
        let kids = self.children(id);
        for k in kids {
            self.remove(k);
        }
        let t = self.create_text(text);
        self.append(id, t);
    }

    pub fn matches(&self, id: usize, sel: &str) -> bool {
        match SelectorSet::parse(sel) {
            Ok(s) => self.matches_set(id, &s),
            Err(_) => false,
        }
    }

    pub fn matches_set(&self, id: usize, sel: &SelectorSet) -> bool {
        selector::matches(self, id, sel)
    }

    pub fn query_selector(&self, sel: &str) -> Option<usize> {
        let set = SelectorSet::parse(sel).ok()?;
        self.elements().into_iter().find(|&e| selector::matches(self, e, &set))
    }

    pub fn query_selector_all(&self, sel: &str) -> Result<Vec<usize>> {
        let set = SelectorSet::parse(sel)?;
        Ok(self
            .elements()
            .into_iter()
            .filter(|&e| selector::matches(self, e, &set))
            .collect())
    }

    /// `<base href>` resolution needs this early during parsing.
    pub fn base_href(&self) -> Option<String> {
        self.get_elements_by_tag_name("base")
            .into_iter()
            .find_map(|e| self.attr(e, "href"))
    }

    pub fn title(&self) -> String {
        self.get_elements_by_tag_name("title")
            .first()
            .map(|&t| self.text_content(t).trim().to_string())
            .unwrap_or_default()
    }

    /// `document.charset` / `<meta charset>` / `<meta http-equiv=content-type>`.
    pub fn declared_charset(&self) -> Option<String> {
        for m in self.get_elements_by_tag_name("meta") {
            if let Some(c) = self.attr(m, "charset") {
                return Some(c.trim().to_ascii_lowercase());
            }
            let equiv = self.attr(m, "http-equiv").unwrap_or_default();
            if equiv.eq_ignore_ascii_case("content-type") {
                if let Some(c) = self.attr(m, "content") {
                    for part in c.split(';') {
                        if let Some((k, v)) = part.split_once('=') {
                            if k.trim().eq_ignore_ascii_case("charset") {
                                return Some(v.trim().to_ascii_lowercase());
                            }
                        }
                    }
                }
            }
        }
        None
    }

    // ------------------------------------------------------------ serialising

    pub fn outer_html(&self, id: usize) -> String {
        let mut out = String::new();
        self.serialize(id, &mut out);
        out
    }

    pub fn inner_html(&self, id: usize) -> String {
        let mut out = String::new();
        for c in self.children(id) {
            self.serialize(c, &mut out);
        }
        out
    }

    fn serialize(&self, id: usize, out: &mut String) {
        let n = match self.node(id) {
            Some(n) => n,
            None => return,
        };
        match n.kind {
            Kind::Document => {
                for c in n.children.iter() {
                    self.serialize(*c, out);
                }
            }
            Kind::Text => out.push_str(&escape_text(&n.data)),
            Kind::Comment => {
                out.push_str("<!--");
                out.push_str(&n.data);
                out.push_str("-->");
            }
            Kind::Doctype => {
                out.push_str("<!DOCTYPE ");
                out.push_str(&n.data);
                out.push('>');
            }
            Kind::Element => {
                let tag = display_tag(n);
                out.push('<');
                out.push_str(&tag);
                for (k, v) in n.attrs.iter() {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&escape_attr(v));
                    out.push('"');
                }
                out.push('>');
                if !VOID.contains(&tag.as_str()) {
                    if RAWTEXT.contains(&tag.as_str()) {
                        out.push_str(&self.text_content(id));
                    } else {
                        for c in n.children.iter() {
                            self.serialize(*c, out);
                        }
                    }
                    out.push_str("</");
                    out.push_str(&tag);
                    out.push('>');
                }
            }
        }
    }
}

fn display_tag(n: &Node) -> String {
    if n.svg {
        // Restore the handful of camelCase SVG names we care about.
        match n.tag.as_str() {
            "lineargradient" => return "linearGradient".to_string(),
            "radialgradient" => return "radialGradient".to_string(),
            "clippath" => return "clipPath".to_string(),
            "foreignobject" => return "foreignObject".to_string(),
            _ => {}
        }
    }
    n.tag.clone()
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            _ => out.push(c),
        }
    }
    out
}

pub const VOID: [&str; 16] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
    "source", "track", "wbr", "keygen", "basefont",
];

pub const RAWTEXT: [&str; 4] = ["script", "style", "textarea", "title"];

/// Elements whose end tag is optional and which auto-close on certain parents.
pub const AUTO_CLOSE: [&str; 12] = [
    "p", "li", "dt", "dd", "option", "thead", "tbody", "tfoot", "tr", "td", "th", "rt",
];

/// `NodeList`-ish wrapper the JS bridge hands back.
#[derive(Clone, Debug, Default)]
pub struct NodeList {
    pub ids: Vec<usize>,
}

impl NodeList {
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

pub fn void_element(tag: &str) -> bool {
    VOID.contains(&tag)
}

pub fn rawtext_element(tag: &str) -> bool {
    RAWTEXT.contains(&tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_query_tree() {
        let mut d = Dom::new();
        let html = d.create_element("html");
        let body = d.create_element("body");
        let div = d.create_element("div");
        let span = d.create_element("span");
        let txt = d.create_text("hello &amp; goodbye");
        d.append(d.document, html);
        d.append(html, body);
        d.append(body, div);
        d.append(div, span);
        d.append(span, txt);
        d.set_attr(div, "id", "main");
        d.set_attr(div, "class", "a b");
        assert_eq!(d.attr(div, "ID").unwrap(), "main");
        assert!(d.has_class(div, "b"));
        assert_eq!(d.get_element_by_id("main"), Some(div));
        assert_eq!(d.get_elements_by_tag_name("span"), vec![span]);
        assert_eq!(d.text_content(span), "hello &amp; goodbye");
        let kids = d.element_children(body);
        assert_eq!(kids, vec![div]);
        let inner = d.inner_html(body);
        assert!(inner.starts_with("<div id=\"main\" class=\"a b\""), "{inner}");
        assert!(inner.contains("<span>hello &amp;amp; goodbye</span>"), "{inner}");
    }

    #[test]
    fn detach_and_remove() {
        let mut d = Dom::new();
        let ul = d.create_element("ul");
        let li1 = d.create_element("li");
        let li2 = d.create_element("li");
        d.append(d.document, ul);
        d.append(ul, li1);
        d.append(ul, li2);
        assert_eq!(d.children(ul).len(), 2);
        d.remove(li1);
        assert_eq!(d.children(ul), vec![li2]);
        assert!(d.node(li1).is_none(), "removed nodes read as nothing");
        d.set_text_content(li2, "only");
        assert_eq!(d.text_content(li2), "only");
        assert_eq!(d.children(li2).len(), 1);
    }

    #[test]
    fn serialization_escapes_and_voids() {
        let mut d = Dom::new();
        let p = d.create_element("p");
        let img = d.create_element("img");
        let t = d.create_text("<b>x</b> & y");
        d.append(d.document, p);
        d.append(p, t);
        d.append(p, img);
        d.set_attr(img, "src", "a\"b&c");
        let html = d.outer_html(p);
        assert_eq!(
            html,
            "<p>&lt;b&gt;x&lt;/b&gt; &amp; y<img src=\"a&quot;b&amp;c\"></p>"
        );
        assert!(void_element("img"));
        assert!(!void_element("div"));
        assert!(rawtext_element("script"));
    }
}
