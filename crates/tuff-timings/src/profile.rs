//! The aggregated call trees built from a recorder snapshot.

use std::time::{Duration, SystemTime};

use super::recorder::{RawNode, Snapshot, Stats};
use super::{FEATURE, SERVER_TICK, Tag};

/// A finished recording: each thread's call tree and all of them merged.
#[derive(Clone, Debug)]
pub struct Profile {
    pub started: SystemTime,
    pub duration: Duration,
    pub threads: Vec<ThreadProfile>,
    pub merged: Vec<Node>,
}

#[derive(Clone, Debug)]
pub struct ThreadProfile {
    pub index: usize,
    pub name: String,
    pub dropped: u64,
    pub roots: Vec<Node>,
}

/// One call path: every call of spans with this name and tag under the
/// same parent path.
#[derive(Clone, Debug)]
pub struct Node {
    pub name: &'static str,
    pub tag: Tag,
    pub asynchronous: bool,
    pub stats: Stats,
    pub children: Vec<Node>,
}

impl Node {
    /// Time not spent in recorded children. A timer's wall time only
    /// counts against an asynchronous parent: under a span it may have
    /// started before the span did.
    pub fn self_ns(&self) -> u64 {
        let children: u64 = self
            .children
            .iter()
            .filter(|c| self.asynchronous || !c.asynchronous)
            .map(|c| c.stats.total_ns)
            .sum();
        self.stats.total_ns.saturating_sub(children)
    }

    fn same_call(&self, other: &Node) -> bool {
        self.name == other.name && self.tag == other.tag && self.asynchronous == other.asynchronous
    }

    fn visit<'a>(&'a self, out: &mut impl FnMut(&'a Node)) {
        out(self);
        for child in &self.children {
            child.visit(out);
        }
    }
}

fn merge_into(target: &mut Vec<Node>, node: &Node) {
    match target.iter_mut().find(|t| t.same_call(node)) {
        Some(existing) => {
            existing.stats.merge(&node.stats);
            for child in &node.children {
                merge_into(&mut existing.children, child);
            }
        }
        None => target.push(node.clone()),
    }
}

fn sort(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| {
        b.stats
            .total_ns
            .cmp(&a.stats.total_ns)
            .then_with(|| a.name.cmp(b.name))
    });
    for node in nodes {
        sort(&mut node.children);
    }
}

fn thread_roots(nodes: &[RawNode]) -> Vec<Node> {
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut roots = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        match node.parent {
            Some(parent) if parent < index => children[parent].push(index),
            Some(_) => {}
            None => roots.push(index),
        }
    }
    fn build(index: usize, nodes: &[RawNode], children: &[Vec<usize>]) -> Node {
        let raw = &nodes[index];
        Node {
            name: raw.site.name,
            tag: raw.tag,
            asynchronous: raw.site.asynchronous,
            stats: raw.stats.clone(),
            children: children[index]
                .iter()
                .map(|&child| build(child, nodes, children))
                .collect(),
        }
    }
    let mut merged = Vec::new();
    for root in roots {
        merge_into(&mut merged, &build(root, nodes, &children));
    }
    merged.retain(|n| n.stats.count > 0 || !n.children.is_empty());
    sort(&mut merged);
    merged
}

impl Profile {
    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        let threads: Vec<ThreadProfile> = snapshot
            .threads
            .iter()
            .map(|thread| ThreadProfile {
                index: thread.index,
                name: thread.name.clone(),
                dropped: thread.dropped,
                roots: thread_roots(&thread.nodes),
            })
            .collect();
        let mut merged = Vec::new();
        for thread in &threads {
            for root in &thread.roots {
                merge_into(&mut merged, root);
            }
        }
        sort(&mut merged);
        Self {
            started: snapshot.started,
            duration: snapshot.duration,
            threads,
            merged,
        }
    }

    /// Every node of the merged tree, depth first.
    pub fn nodes(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        for root in &self.merged {
            root.visit(&mut |node| out.push(node));
        }
        out
    }

    /// The merged node reached by following span names from the roots.
    pub fn find(&self, path: &[&str]) -> Option<&Node> {
        let (first, rest) = path.split_first()?;
        let mut node = self.merged.iter().find(|n| n.name == *first)?;
        for name in rest {
            node = node.children.iter().find(|n| n.name == *name)?;
        }
        Some(node)
    }

    /// Spans lost to the depth and node limits, over all threads.
    pub fn dropped(&self) -> u64 {
        self.threads.iter().map(|t| t.dropped).sum()
    }

    /// Every server tick recorded, from [`SERVER_TICK`] wherever it is.
    pub fn ticks(&self) -> Option<Stats> {
        let mut total: Option<Stats> = None;
        for node in self.nodes().into_iter().filter(|n| n.name == SERVER_TICK) {
            total.get_or_insert_with(Stats::empty).merge(&node.stats);
        }
        total
    }

    /// Total time and calls per feature over every feature hook span, most
    /// expensive first. Nested hook spans count once, at the outermost.
    pub fn features(&self) -> Vec<(&'static str, u64, u64)> {
        fn walk(node: &Node, out: &mut Vec<(&'static str, u64, u64)>) {
            if node.name == FEATURE
                && let Tag::Name(id) = node.tag
            {
                match out.iter_mut().find(|(name, _, _)| *name == id) {
                    Some(entry) => {
                        entry.1 = entry.1.saturating_add(node.stats.total_ns);
                        entry.2 += node.stats.count;
                    }
                    None => out.push((id, node.stats.total_ns, node.stats.count)),
                }
                return;
            }
            for child in &node.children {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        for root in &self.merged {
            walk(root, &mut out);
        }
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        out
    }

    /// The `n` (name, tag) pairs with the most self time, summed wherever
    /// they appear in the merged tree.
    pub fn top_self(&self, n: usize) -> Vec<(&'static str, Tag, u64)> {
        let mut out: Vec<(&'static str, Tag, u64)> = Vec::new();
        for node in self.nodes().into_iter().filter(|n| !n.asynchronous) {
            let own = node.self_ns();
            match out
                .iter_mut()
                .find(|(name, tag, _)| *name == node.name && *tag == node.tag)
            {
                Some(entry) => entry.2 = entry.2.saturating_add(own),
                None => out.push((node.name, node.tag, own)),
            }
        }
        out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(b.0)));
        out.truncate(n);
        out
    }
}

/// Turns tags into display text: dimension names for region tags, short
/// type names for packet types.
#[derive(Clone, Debug, Default)]
pub struct Labels {
    pub dimensions: Vec<String>,
    pub server_name: String,
}

impl Labels {
    pub fn tag(&self, tag: Tag) -> Option<String> {
        match tag {
            Tag::None => None,
            Tag::Name(name) => Some(short_type_name(name)),
            Tag::Region { dimension, x, z } => {
                let dimension = self
                    .dimensions
                    .get(usize::from(dimension))
                    .cloned()
                    .unwrap_or_else(|| format!("dimension {dimension}"));
                Some(format!("{dimension} {x},{z}"))
            }
        }
    }
}

/// `a::b::Foo<c::Bar>` as `Foo<Bar>`; other text unchanged.
pub fn short_type_name(name: &str) -> String {
    if !name.contains("::") {
        return name.to_owned();
    }
    let mut out = String::with_capacity(name.len());
    let mut segment = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() || c == '_' || c == ':' {
            segment.push(c);
        } else {
            out.push_str(segment.rsplit("::").next().unwrap_or(""));
            segment.clear();
            out.push(c);
        }
    }
    out.push_str(segment.rsplit("::").next().unwrap_or(""));
    out
}
