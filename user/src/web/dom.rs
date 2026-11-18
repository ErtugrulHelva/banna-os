//! Belge ağacı (DOM): düğümler bir dizide tutulur, birbirine numarayla bağlanır.
//! Silinen düğümler ağaçtan çıkarılır ama dizide kalır (numaralar kararlı kalır).

use alloc::string::String;
use alloc::vec::Vec;

pub type NodeId = usize;

#[derive(Clone, Debug)]
pub enum Kind {
    Document,
    Element { tag: String, attrs: Vec<(String, String)> },
    Text(String),
    Comment,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

pub struct Document {
    pub nodes: Vec<Node>,
    /// `:hover` için fare altındaki öğe.
    pub hover: Option<NodeId>,
    /// Klavye odağındaki form denetimi.
    pub focus: Option<NodeId>,
    /// Stil ya da yapı değişti: yeniden yerleşim gerekir.
    pub dirty: bool,
}

pub const ROOT: NodeId = 0;

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        Self {
            nodes: alloc::vec![Node {
                kind: Kind::Document,
                parent: None,
                children: Vec::new(),
            }],
            hover: None,
            focus: None,
            dirty: true,
        }
    }

    pub fn create(&mut self, kind: Kind) -> NodeId {
        self.nodes.push(Node {
            kind,
            parent: None,
            children: Vec::new(),
        });
        self.nodes.len() - 1
    }

    pub fn element(&mut self, tag: &str) -> NodeId {
        self.create(Kind::Element {
            tag: String::from(tag),
            attrs: Vec::new(),
        })
    }

    pub fn text(&mut self, text: &str) -> NodeId {
        self.create(Kind::Text(String::from(text)))
    }

    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
        self.dirty = true;
    }

    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) {
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        let at = before
            .and_then(|b| self.nodes[parent].children.iter().position(|&c| c == b))
            .unwrap_or(self.nodes[parent].children.len());
        self.nodes[parent].children.insert(at, child);
        self.dirty = true;
    }

    pub fn detach(&mut self, child: NodeId) {
        if let Some(parent) = self.nodes[child].parent.take() {
            self.nodes[parent].children.retain(|&c| c != child);
            self.dirty = true;
        }
    }

    pub fn clear_children(&mut self, node: NodeId) {
        for child in core::mem::take(&mut self.nodes[node].children) {
            self.nodes[child].parent = None;
        }
        self.dirty = true;
    }

    pub fn tag(&self, node: NodeId) -> Option<&str> {
        match &self.nodes.get(node)?.kind {
            Kind::Element { tag, .. } => Some(tag),
            _ => None,
        }
    }

    pub fn is(&self, node: NodeId, tag: &str) -> bool {
        self.tag(node) == Some(tag)
    }

    pub fn attr(&self, node: NodeId, name: &str) -> Option<&str> {
        match &self.nodes.get(node)?.kind {
            Kind::Element { attrs, .. } => attrs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str()),
            _ => None,
        }
    }

    pub fn set_attr(&mut self, node: NodeId, name: &str, value: &str) {
        if let Some(Node {
            kind: Kind::Element { attrs, .. },
            ..
        }) = self.nodes.get_mut(node)
        {
            match attrs.iter_mut().find(|(k, _)| k == name) {
                Some(entry) => entry.1 = String::from(value),
                None => attrs.push((String::from(name), String::from(value))),
            }
            self.dirty = true;
        }
    }

    pub fn remove_attr(&mut self, node: NodeId, name: &str) {
        if let Some(Node {
            kind: Kind::Element { attrs, .. },
            ..
        }) = self.nodes.get_mut(node)
        {
            attrs.retain(|(k, _)| k != name);
            self.dirty = true;
        }
    }

    pub fn classes(&self, node: NodeId) -> impl Iterator<Item = &str> {
        self.attr(node, "class").unwrap_or("").split_ascii_whitespace()
    }

    pub fn has_class(&self, node: NodeId, class: &str) -> bool {
        self.classes(node).any(|c| c == class)
    }

    /// Alt ağaçtaki metin (textContent).
    pub fn text_content(&self, node: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(node, &mut out);
        out
    }

    /// Kullanıcının göreceği metin: `<style>`, `<script>`, `<template>` ve `<noscript>` atlanır
    /// (ör. düğme etiketi; Google düğmelerin içine `<style>` koyar).
    pub fn visible_text(&self, node: NodeId) -> String {
        let mut out = String::new();
        self.collect_visible(node, &mut out);
        out
    }

    fn collect_visible(&self, node: NodeId, out: &mut String) {
        match &self.nodes[node].kind {
            Kind::Text(t) => out.push_str(t),
            _ if matches!(self.tag(node), Some("style" | "script" | "template" | "noscript")) => {}
            _ => {
                for &c in &self.nodes[node].children {
                    self.collect_visible(c, out);
                }
            }
        }
    }

    fn collect_text(&self, node: NodeId, out: &mut String) {
        match &self.nodes[node].kind {
            Kind::Text(t) => out.push_str(t),
            _ => {
                for &c in &self.nodes[node].children {
                    self.collect_text(c, out);
                }
            }
        }
    }

    pub fn set_text_content(&mut self, node: NodeId, text: &str) {
        if let Kind::Text(t) = &mut self.nodes[node].kind {
            *t = String::from(text);
            self.dirty = true;
            return;
        }
        self.clear_children(node);
        if !text.is_empty() {
            let t = self.text(text);
            self.append(node, t);
        }
    }

    /// Ön sıralı gezinti (düğümün kendisi dahil).
    pub fn descendants(&self, node: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = alloc::vec![node];
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n].children.iter().rev());
        }
        out
    }

    pub fn find_tag(&self, tag: &str) -> Option<NodeId> {
        self.descendants(ROOT).into_iter().find(|&n| self.is(n, tag))
    }

    pub fn by_id(&self, id: &str) -> Option<NodeId> {
        self.descendants(ROOT)
            .into_iter()
            .find(|&n| self.attr(n, "id") == Some(id))
    }

    pub fn body(&self) -> Option<NodeId> {
        self.find_tag("body")
    }

    pub fn title(&self) -> String {
        self.find_tag("title")
            .map(|t| collapse(&self.text_content(t)))
            .unwrap_or_default()
    }

    pub fn element_children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node]
            .children
            .iter()
            .copied()
            .filter(|&c| matches!(self.nodes[c].kind, Kind::Element { .. }))
            .collect()
    }

    /// Bağlı olduğu ağaçta mı (kopmuş düğümler çizilmez).
    pub fn connected(&self, mut node: NodeId) -> bool {
        loop {
            if node == ROOT {
                return true;
            }
            match self.nodes[node].parent {
                Some(p) => node = p,
                None => return false,
            }
        }
    }

    /// Öğenin HTML'i (innerHTML için çocuklar, outerHTML için kendisi).
    pub fn serialize(&self, node: NodeId, include_self: bool, out: &mut String) {
        match &self.nodes[node].kind {
            Kind::Text(t) => out.push_str(&escape(t)),
            Kind::Comment => {}
            Kind::Document => {
                for &c in &self.nodes[node].children {
                    self.serialize(c, true, out);
                }
            }
            Kind::Element { tag, attrs } => {
                if include_self {
                    out.push('<');
                    out.push_str(tag);
                    for (k, v) in attrs {
                        out.push(' ');
                        out.push_str(k);
                        out.push_str("=\"");
                        out.push_str(&escape(v));
                        out.push('"');
                    }
                    out.push('>');
                }
                if super::html::is_void(tag) {
                    return;
                }
                for &c in &self.nodes[node].children {
                    self.serialize(c, true, out);
                }
                if include_self {
                    out.push_str("</");
                    out.push_str(tag);
                    out.push('>');
                }
            }
        }
    }
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Boşlukları teke indirir ve kırpar.
pub fn collapse(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}
