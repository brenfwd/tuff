// SPDX-License-Identifier: MIT
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};

use tuff_bots_api::control::Goal;

use super::{Edge, MoveKind, Params, Pos, heuristic, reached};
use crate::world::Blocks;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Nodes expanded per [`Search::step`] call.
    pub nodes_per_step: usize,
    /// Nodes expanded over the whole search before it settles for the best
    /// partial path. The search's time limit, in a form that is deterministic.
    pub total_nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            nodes_per_step: 2000,
            total_nodes: 20_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// From the start to the goal, or to the node closest to it.
    pub path: Vec<Pos>,
    /// Whether the last node satisfies the goal.
    pub complete: bool,
    pub cost: f64,
    /// Throwaway blocks placed on the way to each node of `path`.
    pub placed: Vec<u32>,
}

#[derive(Debug, PartialEq)]
pub enum Status {
    Running,
    Done(Plan),
}

struct Entry {
    f: f64,
    h: f64,
    seq: u64,
    g: f64,
    pos: Pos,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Entry {}
impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.f
            .total_cmp(&other.f)
            .then(self.h.total_cmp(&other.h))
            .then(self.seq.cmp(&other.seq))
    }
}

struct Node {
    g: f64,
    parent: Option<Pos>,
    /// Throwaway blocks the path to here places.
    placed: u32,
}

/// An A* that can be paused after any number of nodes and resumed with the
/// same result as an unbounded run.
pub struct Search {
    goal: Goal,
    params: Params,
    start: Pos,
    open: BinaryHeap<Reverse<Entry>>,
    nodes: HashMap<Pos, Node>,
    best: (f64, f64, Pos),
    /// The best node reached without placing: where a search that ran out
    /// of nodes to try ends, so no block is spent on a way that goes nowhere.
    best_dry: (f64, f64, Pos),
    seq: u64,
    expanded: usize,
    edges: Vec<Edge>,
}

impl Search {
    pub fn new(start: Pos, goal: Goal, params: Params) -> Self {
        let h = heuristic(&goal, start);
        let mut nodes = HashMap::new();
        nodes.insert(
            start,
            Node {
                g: 0.0,
                parent: None,
                placed: 0,
            },
        );
        let mut open = BinaryHeap::new();
        open.push(Reverse(Entry {
            f: h,
            h,
            seq: 0,
            g: 0.0,
            pos: start,
        }));
        Self {
            goal,
            params,
            start,
            open,
            nodes,
            best: (h, 0.0, start),
            best_dry: (h, 0.0, start),
            seq: 1,
            expanded: 0,
            edges: Vec::new(),
        }
    }

    pub fn start(&self) -> Pos {
        self.start
    }

    pub fn expanded(&self) -> usize {
        self.expanded
    }

    fn path_to(&self, end: Pos) -> (Vec<Pos>, Vec<u32>) {
        let mut path = vec![end];
        let mut placed = vec![self.nodes[&end].placed];
        let mut at = end;
        while let Some(parent) = self.nodes[&at].parent {
            path.push(parent);
            placed.push(self.nodes[&parent].placed);
            at = parent;
        }
        path.reverse();
        placed.reverse();
        (path, placed)
    }

    fn finish(&self, end: Pos, complete: bool) -> Status {
        let (path, placed) = self.path_to(end);
        Status::Done(Plan {
            path,
            complete,
            cost: self.nodes[&end].g,
            placed,
        })
    }

    /// Expands up to `limits.nodes_per_step` nodes. A node whose blocks are
    /// not readable yet is retried on the next call.
    pub fn step(
        &mut self,
        world: &mut dyn Blocks,
        moves: &[Box<dyn MoveKind>],
        limits: Limits,
    ) -> Status {
        let mut done = 0;
        while done < limits.nodes_per_step {
            if self.expanded >= limits.total_nodes {
                return self.finish(self.best.2, false);
            }
            let Some(Reverse(entry)) = self.open.pop() else {
                return self.finish(self.best_dry.2, false);
            };
            if self.nodes[&entry.pos].g < entry.g {
                continue;
            }
            if reached(&self.goal, entry.pos) {
                return self.finish(entry.pos, true);
            }
            self.edges.clear();
            let mut starved = false;
            for kind in moves {
                if kind
                    .expand(world, &self.params, entry.pos, &mut self.edges)
                    .is_err()
                {
                    starved = true;
                    break;
                }
            }
            if starved {
                self.open.push(Reverse(entry));
                return Status::Running;
            }
            self.expanded += 1;
            done += 1;
            let edges = std::mem::take(&mut self.edges);
            let placed_here = self.nodes[&entry.pos].placed;
            for edge in &edges {
                let placed = placed_here + edge.place;
                if placed > self.params.throwaway {
                    continue;
                }
                let g = entry.g + edge.cost;
                let better = self.nodes.get(&edge.to).is_none_or(|n| g < n.g);
                if !better {
                    continue;
                }
                self.nodes.insert(
                    edge.to,
                    Node {
                        g,
                        parent: Some(entry.pos),
                        placed,
                    },
                );
                let h = heuristic(&self.goal, edge.to);
                if (h, g) < (self.best.0, self.best.1) {
                    self.best = (h, g, edge.to);
                }
                if placed == 0 && (h, g) < (self.best_dry.0, self.best_dry.1) {
                    self.best_dry = (h, g, edge.to);
                }
                self.seq += 1;
                self.open.push(Reverse(Entry {
                    f: g + h,
                    h,
                    seq: self.seq,
                    g,
                    pos: edge.to,
                }));
            }
            self.edges = edges;
        }
        Status::Running
    }
}
