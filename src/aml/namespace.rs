//! ACPI ad alanı: dört karakterli adlardan oluşan ağaç.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::value::{Object, Value};

pub type NodeId = usize;
pub type NameSeg = [u8; 4];

pub const ROOT: NodeId = 0;

pub struct Node {
    pub name: NameSeg,
    pub parent: NodeId,
    pub children: BTreeMap<NameSeg, NodeId>,
    pub object: Object,
    pub alive: bool,
}

/// Ayrıştırılmış ad: kök/üst önekleri ve segmentler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamePath {
    pub absolute: bool,
    pub parents: usize,
    pub segments: Vec<NameSeg>,
}

impl NamePath {
    pub fn parse(text: &str) -> Option<Self> {
        let mut rest = text;
        let absolute = rest.starts_with('\\');
        if absolute {
            rest = &rest[1..];
        }
        let mut parents = 0;
        while let Some(stripped) = rest.strip_prefix('^') {
            parents += 1;
            rest = stripped;
        }
        let mut segments = Vec::new();
        for part in rest.split('.').filter(|p| !p.is_empty()) {
            if part.len() > 4 {
                return None;
            }
            let mut seg = *b"____";
            seg[..part.len()].copy_from_slice(part.as_bytes());
            segments.push(seg);
        }
        Some(Self {
            absolute,
            parents,
            segments,
        })
    }

    pub fn is_single(&self) -> bool {
        !self.absolute && self.parents == 0 && self.segments.len() == 1
    }
}

pub struct Namespace {
    nodes: Vec<Node>,
}

impl Namespace {
    pub fn new() -> Self {
        let mut ns = Self { nodes: Vec::new() };
        ns.nodes.push(Node {
            name: *b"\\___",
            parent: ROOT,
            children: BTreeMap::new(),
            object: Object::Scope,
            alive: true,
        });
        for scope in [b"_GPE", b"_PR_", b"_SB_", b"_SI_", b"_TZ_"] {
            ns.add(ROOT, *scope, Object::Scope);
        }
        ns.add(ROOT, *b"_GL_", Object::Mutex);
        ns.add(ROOT, *b"_REV", Object::Name(Value::Integer(2)));
        ns.add(
            ROOT,
            *b"_OS_",
            Object::Name(Value::String(String::from("Microsoft Windows NT"))),
        );
        ns
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id]
    }

    pub fn object(&self, id: NodeId) -> &Object {
        &self.nodes[id].object
    }

    pub fn len(&self) -> usize {
        self.nodes.iter().filter(|n| n.alive).count()
    }

    /// Aynı adlı canlı bir çocuk varsa nesnesini değiştirmeden onu döndürür.
    pub fn add(&mut self, parent: NodeId, name: NameSeg, object: Object) -> NodeId {
        if let Some(&existing) = self.nodes[parent].children.get(&name) {
            let node = &mut self.nodes[existing];
            if matches!(node.object, Object::Scope | Object::External(_)) || !node.alive {
                node.object = object;
                node.alive = true;
            }
            return existing;
        }
        let id = self.nodes.len();
        self.nodes.push(Node {
            name,
            parent,
            children: BTreeMap::new(),
            object,
            alive: true,
        });
        self.nodes[parent].children.insert(name, id);
        id
    }

    pub fn remove(&mut self, id: NodeId) {
        let parent = self.nodes[id].parent;
        let name = self.nodes[id].name;
        if self.nodes[parent].children.get(&name) == Some(&id) {
            self.nodes[parent].children.remove(&name);
        }
        let children: Vec<NodeId> = self.nodes[id].children.values().copied().collect();
        for child in children {
            self.remove(child);
        }
        self.nodes[id].alive = false;
    }

    pub fn child(&self, parent: NodeId, name: NameSeg) -> Option<NodeId> {
        self.nodes[parent].children.get(&name).copied()
    }

    /// `path`'in son segmenti hariç kısmını çözer (yeni nesne oluştururken kullanılır).
    pub fn resolve_parent(&self, scope: NodeId, path: &NamePath) -> Option<(NodeId, NameSeg)> {
        let (last, prefix) = path.segments.split_last()?;
        let mut node = if path.absolute { ROOT } else { scope };
        for _ in 0..path.parents {
            node = self.nodes[node].parent;
        }
        for seg in prefix {
            node = self.child(node, *seg)?;
        }
        Some((node, *last))
    }

    /// ACPI ad arama kuralları: tek segmentli göreli adlar üst kapsamlarda da aranır.
    pub fn resolve(&self, scope: NodeId, path: &NamePath) -> Option<NodeId> {
        if path.segments.is_empty() {
            let mut node = if path.absolute { ROOT } else { scope };
            for _ in 0..path.parents {
                node = self.nodes[node].parent;
            }
            return Some(node);
        }
        if path.is_single() {
            let mut node = scope;
            loop {
                if let Some(found) = self.child(node, path.segments[0]) {
                    return Some(self.follow_alias(found));
                }
                if node == ROOT {
                    return None;
                }
                node = self.nodes[node].parent;
            }
        }
        let (parent, last) = self.resolve_parent(scope, path)?;
        self.child(parent, last).map(|id| self.follow_alias(id))
    }

    pub fn follow_alias(&self, mut id: NodeId) -> NodeId {
        for _ in 0..8 {
            match self.nodes[id].object {
                Object::Alias(target) => id = target,
                _ => break,
            }
        }
        id
    }

    pub fn path_of(&self, mut id: NodeId) -> String {
        let mut segments = Vec::new();
        while id != ROOT {
            segments.push(self.nodes[id].name);
            id = self.nodes[id].parent;
        }
        let mut out = String::from("\\");
        for (i, seg) in segments.iter().rev().enumerate() {
            if i > 0 {
                out.push('.');
            }
            out.push_str(core::str::from_utf8(seg).unwrap_or("????"));
        }
        out
    }

    pub fn walk(&self, mut visit: impl FnMut(NodeId, &Node)) {
        let mut stack = alloc::vec![ROOT];
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            if !node.alive {
                continue;
            }
            visit(id, node);
            stack.extend(node.children.values().rev());
        }
    }
}
