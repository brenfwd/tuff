//! The dragon's phases and the graph of flight nodes it paths over. Phase
//! ids and the node links come from the game's code
//! (`tuff_vanilla_data::dragon`); each phase's state lives on as long as the
//! dragon, as the game keeps one instance per phase.

use tuff_vanilla_data::dragon::{NODE_ADJACENCY, PHASES};

use tuff_core::mth;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseKind {
    HoldingPattern,
    StrafePlayer,
    LandingApproach,
    Landing,
    Takeoff,
    SittingFlaming,
    SittingScanning,
    SittingAttacking,
    ChargingPlayer,
    Dying,
    Hover,
}

impl PhaseKind {
    const ALL: [PhaseKind; 11] = [
        Self::HoldingPattern,
        Self::StrafePlayer,
        Self::LandingApproach,
        Self::Landing,
        Self::Takeoff,
        Self::SittingFlaming,
        Self::SittingScanning,
        Self::SittingAttacking,
        Self::ChargingPlayer,
        Self::Dying,
        Self::Hover,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::HoldingPattern => "HoldingPattern",
            Self::StrafePlayer => "StrafePlayer",
            Self::LandingApproach => "LandingApproach",
            Self::Landing => "Landing",
            Self::Takeoff => "Takeoff",
            Self::SittingFlaming => "SittingFlaming",
            Self::SittingScanning => "SittingScanning",
            Self::SittingAttacking => "SittingAttacking",
            Self::ChargingPlayer => "ChargingPlayer",
            Self::Dying => "Dying",
            Self::Hover => "Hover",
        }
    }

    /// The id saved and sent in the dragon's data.
    pub fn id(self) -> i32 {
        PHASES
            .iter()
            .position(|name| *name == self.name())
            .expect("every phase is in the dumped list") as i32
    }

    /// The phase with `id`; unknown ids hold the pattern, as the game does.
    pub fn from_id(id: i32) -> Self {
        usize::try_from(id)
            .ok()
            .and_then(|i| PHASES.get(i))
            .and_then(|name| Self::ALL.into_iter().find(|p| p.name() == *name))
            .unwrap_or(Self::HoldingPattern)
    }

    pub fn is_sitting(self) -> bool {
        matches!(
            self,
            Self::SittingFlaming | Self::SittingScanning | Self::SittingAttacking | Self::Hover
        )
    }

    pub fn fly_speed(self) -> f32 {
        match self {
            Self::Landing => 1.5,
            Self::ChargingPlayer | Self::Dying => 3.0,
            Self::Hover => 1.0,
            _ => 0.6,
        }
    }
}

/// A node of the flight graph, in block coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Node {
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    fn distance_sq(self, other: Node) -> f32 {
        let (dx, dy, dz) = (
            (other.x - self.x) as f32,
            (other.y - self.y) as f32,
            (other.z - self.z) as f32,
        );
        dx * dx + dy * dy + dz * dz
    }

    fn distance(self, other: Node) -> f32 {
        self.distance_sq(other).sqrt()
    }
}

/// A route over flight nodes, followed one node at a time.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    nodes: Vec<Node>,
    next: usize,
}

impl Path {
    pub fn is_done(&self) -> bool {
        self.next >= self.nodes.len()
    }

    pub fn advance(&mut self) {
        self.next += 1;
    }

    pub fn next_node(&self) -> Node {
        self.nodes[self.next]
    }

    pub fn finish(&mut self) {
        self.next = self.nodes.len();
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
}

pub const NODE_COUNT: usize = 24;
/// Nodes never float lower than this.
const MIN_NODE_Y: i32 = 73;
/// The outer ring's nodes are only used while crystals remain.
pub const OUTER_NODES: usize = 12;

/// Where flight node `index` stands horizontally, and how far above the
/// ground it floats.
pub fn node_column(index: usize) -> (i32, i32, i32) {
    let (radius, step, offset, lift) = if index < 12 {
        (60.0f32, 0.2617994f32, 0, 5)
    } else if index < 20 {
        (40.0, std::f32::consts::FRAC_PI_8, 12, 15)
    } else {
        (20.0, std::f32::consts::FRAC_PI_4, 20, 5)
    };
    let angle = 2.0 * (-std::f32::consts::PI + step * (index - offset) as f32);
    let x = (radius * mth::cos(f64::from(angle))).floor() as i32;
    let z = (radius * mth::sin(f64::from(angle))).floor() as i32;
    (x, z, lift)
}

/// The flight graph over the island, from the ground height under each
/// node (one above its highest motion-blocking block).
#[derive(Debug, Clone)]
pub struct FlightGraph {
    pub nodes: [Node; NODE_COUNT],
}

impl FlightGraph {
    pub fn new(ground: impl Fn(i32, i32) -> i32) -> Self {
        let nodes = std::array::from_fn(|i| {
            let (x, z, lift) = node_column(i);
            Node::new(x, (ground(x, z) + lift).max(MIN_NODE_Y), z)
        });
        Self { nodes }
    }

    /// The node closest to a position, among the inner nodes only when no
    /// crystal is left.
    pub fn closest(&self, position: [f64; 3], crystals: bool) -> usize {
        let at = Node::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        );
        let start = if crystals { 0 } else { OUTER_NODES };
        let mut closest = 10000.0f32;
        let mut index = 0;
        for (i, node) in self.nodes.iter().enumerate().skip(start) {
            let distance = node.distance_sq(at);
            if distance < closest {
                closest = distance;
                index = i;
            }
        }
        index
    }

    /// An A* route from node `from` to node `to`, ending at `last` if
    /// given; the closest reachable node stands in for an unreachable
    /// target.
    pub fn find_path(
        &self,
        from: usize,
        to: usize,
        last: Option<Node>,
        crystals: bool,
    ) -> Option<Path> {
        let minimum = if crystals { 0 } else { OUTER_NODES };
        let mut g = [0.0f32; NODE_COUNT];
        let mut f = [0.0f32; NODE_COUNT];
        let mut came_from: [Option<usize>; NODE_COUNT] = [None; NODE_COUNT];
        let mut closed = [false; NODE_COUNT];
        let mut open: Vec<usize> = vec![from];
        let mut in_open = [false; NODE_COUNT];
        in_open[from] = true;
        let target = self.nodes[to];
        f[from] = self.nodes[from].distance(target);
        let mut closest = from;
        while !open.is_empty() {
            let best = (0..open.len())
                .min_by(|a, b| f[open[*a]].total_cmp(&f[open[*b]]))
                .expect("open is not empty");
            let current = open.remove(best);
            in_open[current] = false;
            if current == to {
                return Some(self.route(from, to, &came_from, last));
            }
            if self.nodes[current].distance(target) < self.nodes[closest].distance(target) {
                closest = current;
            }
            closed[current] = true;
            for next in minimum..NODE_COUNT {
                if NODE_ADJACENCY[current] & (1 << next) == 0 || closed[next] {
                    continue;
                }
                let tentative = g[current] + self.nodes[current].distance(self.nodes[next]);
                if !in_open[next] || tentative < g[next] {
                    came_from[next] = Some(current);
                    g[next] = tentative;
                    f[next] = tentative + self.nodes[next].distance(target);
                    if !in_open[next] {
                        in_open[next] = true;
                        open.push(next);
                    }
                }
            }
        }
        if closest == from {
            return None;
        }
        Some(self.route(from, closest, &came_from, last))
    }

    fn route(
        &self,
        from: usize,
        to: usize,
        came_from: &[Option<usize>; NODE_COUNT],
        last: Option<Node>,
    ) -> Path {
        let mut nodes = vec![self.nodes[to]];
        let mut at = to;
        while at != from {
            let Some(previous) = came_from[at] else {
                break;
            };
            nodes.push(self.nodes[previous]);
            at = previous;
        }
        nodes.reverse();
        if let Some(last) = last {
            nodes.push(last);
        }
        Path { nodes, next: 0 }
    }
}

/// The last 64 ticks of the dragon's height and heading, which shape its
/// neck and tail.
#[derive(Debug, Clone)]
pub struct FlightHistory {
    samples: [(f64, f32); 64],
    head: i32,
}

impl Default for FlightHistory {
    fn default() -> Self {
        Self {
            samples: [(0.0, 0.0); 64],
            head: -1,
        }
    }
}

impl FlightHistory {
    pub fn record(&mut self, y: f64, yaw: f32) {
        if self.head < 0 {
            self.samples = [(y, yaw); 64];
        }
        self.head = (self.head + 1) % 64;
        self.samples[self.head as usize] = (y, yaw);
    }

    /// The sample from `delay` ticks ago.
    pub fn get(&self, delay: i32) -> (f64, f32) {
        self.samples[((self.head - delay) & 63) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn phase_ids_follow_the_game() {
        for phase in PhaseKind::ALL {
            assert_eq!(PhaseKind::from_id(phase.id()), phase);
        }
        assert_eq!(PhaseKind::HoldingPattern.id(), 0);
        assert_eq!(PhaseKind::Dying.id(), 9);
        assert_eq!(PhaseKind::from_id(99), PhaseKind::HoldingPattern);
    }

    #[tuff_core::watchdog]
    #[test]
    fn nodes_form_three_rings_linked_both_ways() {
        let graph = FlightGraph::new(|_, _| 60);
        for i in 0..NODE_COUNT {
            let node = graph.nodes[i];
            let radius = f64::from(node.x * node.x + node.z * node.z).sqrt();
            let expected = if i < 12 {
                60.0
            } else if i < 20 {
                40.0
            } else {
                20.0
            };
            assert!((radius - expected).abs() < 1.5, "node {i} at {node:?}");
            for (j, adjacency) in NODE_ADJACENCY.iter().enumerate().take(NODE_COUNT) {
                let linked = NODE_ADJACENCY[i] & (1 << j) != 0;
                assert_eq!(linked, adjacency & (1 << i) != 0, "{i} {j}");
            }
        }
        assert_eq!(graph.nodes[0].y, 73);
        assert_eq!(graph.nodes[12].y, 75);
    }

    #[tuff_core::watchdog]
    #[test]
    fn paths_follow_links_and_skip_the_outer_ring_without_crystals() {
        let graph = FlightGraph::new(|_, _| 60);
        let path = graph.find_path(0, 6, None, true).unwrap();
        let index = |node: &Node| graph.nodes.iter().position(|n| n == node).unwrap();
        for pair in path.nodes().windows(2) {
            let (a, b) = (index(&pair[0]), index(&pair[1]));
            assert!(NODE_ADJACENCY[a] & (1 << b) != 0, "{a} -> {b} not linked");
        }
        assert_eq!(*path.nodes().last().unwrap(), graph.nodes[6]);
        let inner = graph.find_path(12, 16, None, false).unwrap();
        assert!(
            inner
                .nodes()
                .iter()
                .all(|n| graph.nodes[OUTER_NODES..].contains(n))
        );
        let with_end = graph
            .find_path(12, 20, Some(Node::new(0, 70, 0)), false)
            .unwrap();
        assert_eq!(*with_end.nodes().last().unwrap(), Node::new(0, 70, 0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn history_reads_back_by_delay() {
        let mut history = FlightHistory::default();
        history.record(10.0, 5.0);
        assert_eq!(history.get(20), (10.0, 5.0));
        history.record(11.0, 6.0);
        assert_eq!(history.get(0), (11.0, 6.0));
        assert_eq!(history.get(1), (10.0, 5.0));
    }
}
