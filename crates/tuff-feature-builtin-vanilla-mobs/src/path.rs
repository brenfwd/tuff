//! A* pathfinding over the game's node model: every block a mob could
//! stand (or swim) in is a node typed by what is in and under it (open,
//! walkable, water, fence, fire nearby, ...), each type carrying a cost
//! malus the mob may override, negative meaning impassable. Walkers step
//! across the ground, swimmers move through water in all six directions,
//! and amphibious mobs do both.
//!
//! A search reads blocks only through the lookup it is given, where unknown
//! blocks count as blocked, and visits a bounded number of nodes.

use std::collections::HashMap;

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::entity::PathType;

use super::physics::{self, Aabb, BlockLookup};
use tuff_core::protocol::play::DebugNode;
use tuff_world_model::BlockPos;

const DEFAULT_JUMP_HEIGHT: f64 = 1.125;
const FUDGING: f32 = 1.5;
const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];
const ALL_DIRECTIONS: [Facing; 6] = [
    Facing::Down,
    Facing::Up,
    Facing::North,
    Facing::South,
    Facing::West,
    Facing::East,
];
const AMPHIBIOUS_WALKABLE_MALUS: f32 = 6.0;
const AMPHIBIOUS_WATER_BORDER_MALUS: f32 = 4.0;
const BREACH_MALUS: f32 = 8.0;

/// Which node model a search uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathMode {
    #[default]
    Walk,
    /// Through water only; `breaching` also allows the air just above
    /// the surface (dolphins leaping).
    Swim { breaching: bool },
    /// Walks on land and swims up and down through water.
    Amphibious,
    /// Through any open space in three dimensions (bees, parrots, allays).
    Fly,
}

/// What the search needs to know about the mob.
#[derive(Debug, Clone)]
pub struct Walker<'a> {
    pub position: [f64; 3],
    pub bounds: Aabb,
    pub width: f32,
    pub height: f32,
    pub on_ground: bool,
    pub max_up_step: f32,
    pub max_fall_distance: i32,
    pub can_float: bool,
    pub can_open_doors: bool,
    pub can_pass_doors: bool,
    pub can_walk_over_fences: bool,
    /// Whether the mob is in water it floats in.
    pub in_floatable_fluid: bool,
    /// Which fluid, if any, `can_float` treats as a surface to stand on:
    /// water for swimming mobs, water and lava for a strider.
    pub floats_on: fn(Option<BlockState>) -> bool,
    /// Which fluid it stands on regardless of `can_float` (lava for a strider).
    pub stands_on_fluid: fn(Option<BlockState>) -> bool,
    /// Path type costs that differ from the defaults.
    pub malus: &'a [(PathType, f32)],
    pub min_y: i32,
    pub mode: PathMode,
    /// Whether it is in water (an amphibious search then starts from its
    /// box rather than the ground under it).
    pub in_water: bool,
    /// Blocks every position farther than `radius_sq` (squared) from the
    /// center unless the mob is already at least as far out.
    pub bound: Option<(BlockPos, f64)>,
    /// Keep the search's sets on the path for the debug feed.
    pub capture_debug: bool,
    /// The frog's amphibious node model: deep water costs extra, blocks
    /// frogs like to jump to read as open and a search from water starts at
    /// the bottom of its box.
    pub frog_nodes: bool,
    pub sea_level: i32,
}

impl Walker<'_> {
    pub fn malus(&self, kind: PathType) -> f32 {
        if self.mode == PathMode::Amphibious {
            match kind {
                PathType::Water => return 0.0,
                PathType::Walkable => return AMPHIBIOUS_WALKABLE_MALUS,
                PathType::WaterBorder => return AMPHIBIOUS_WATER_BORDER_MALUS,
                _ => {}
            }
        }
        self.malus
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or_else(|| kind.malus(), |(_, m)| *m)
    }

    fn block_pos(&self) -> BlockPos {
        physics::block_pos(self.position)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathNode {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub kind: PathType,
}

impl PathNode {
    pub fn pos(&self) -> BlockPos {
        BlockPos::new(self.x, self.y, self.z)
    }
}

/// A found path, followed node by node.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub nodes: Vec<PathNode>,
    pub next: usize,
    pub target: BlockPos,
    pub reached: bool,
    pub distance_to_target: f32,
    /// What the search saw, kept only while the entity paths debug feed
    /// has subscribers.
    pub debug: Option<Box<PathDebug>>,
}

/// A path's search as the entity paths debug feed shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct PathDebug {
    /// The path's own nodes with their search values.
    pub nodes: Vec<DebugNode>,
    pub open_set: Vec<DebugNode>,
    pub closed_set: Vec<DebugNode>,
    pub targets: Vec<DebugNode>,
}

impl Path {
    fn new(nodes: Vec<PathNode>, target: BlockPos, reached: bool) -> Self {
        let distance_to_target = nodes.last().map_or(f32::MAX, |last| {
            ((last.x - target.x).abs() + (last.y - target.y).abs() + (last.z - target.z).abs())
                as f32
        });
        Self {
            nodes,
            next: 0,
            target,
            reached,
            distance_to_target,
            debug: None,
        }
    }

    pub fn is_done(&self) -> bool {
        self.next >= self.nodes.len()
    }

    pub fn advance(&mut self) {
        self.next += 1;
    }

    pub fn end(&self) -> Option<&PathNode> {
        self.nodes.last()
    }

    pub fn next_node(&self) -> &PathNode {
        &self.nodes[self.next]
    }

    /// Where an entity `width` wide stands at node `index`.
    pub fn entity_position(&self, index: usize, width: f32) -> [f64; 3] {
        let node = &self.nodes[index];
        let half = (width + 1.0) as i32 as f64 * 0.5;
        [node.x as f64 + half, node.y as f64, node.z as f64 + half]
    }

    pub fn same_nodes(&self, other: &Path) -> bool {
        self.nodes.len() == other.nodes.len()
            && self
                .nodes
                .iter()
                .zip(&other.nodes)
                .all(|(a, b)| (a.x, a.y, a.z) == (b.x, b.y, b.z))
    }
}

#[derive(Debug, Clone)]
struct Node {
    x: i32,
    y: i32,
    z: i32,
    g: f32,
    h: f32,
    f: f32,
    came_from: Option<usize>,
    closed: bool,
    walked: f32,
    malus: f32,
    heap_index: Option<usize>,
    kind: PathType,
}

impl Node {
    fn coords(&self) -> [i32; 3] {
        [self.x, self.y, self.z]
    }

    fn path_node(&self) -> PathNode {
        PathNode {
            x: self.x,
            y: self.y,
            z: self.z,
            kind: self.kind,
        }
    }

    fn distance(&self, other: &Node) -> f32 {
        distance(self, other.x, other.y, other.z)
    }
}

fn debug_node(n: &Node) -> DebugNode {
    DebugNode {
        pos: [n.x, n.y, n.z],
        walked_distance: n.walked,
        cost_malus: n.malus,
        closed: n.closed,
        path_type: n.kind.id() as i32,
        f: n.f,
    }
}

fn distance(node: &Node, x: i32, y: i32, z: i32) -> f32 {
    let dx = (x - node.x) as f32;
    let dy = (y - node.y) as f32;
    let dz = (z - node.z) as f32;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Open nodes ordered by `f`. Each node remembers its slot so its priority
/// can be lowered or raised in place.
#[derive(Default)]
struct Frontier {
    slots: Vec<usize>,
}

impl Frontier {
    fn place(&mut self, nodes: &mut [Node], slot: usize, node: usize) {
        self.slots[slot] = node;
        nodes[node].heap_index = Some(slot);
    }

    fn push(&mut self, nodes: &mut [Node], node: usize) {
        self.slots.push(node);
        self.rise(nodes, self.slots.len() - 1);
    }

    fn take_min(&mut self, nodes: &mut [Node]) -> Option<usize> {
        let tail = self.slots.pop()?;
        let top = match self.slots.first() {
            Some(&top) => {
                self.place(nodes, 0, tail);
                self.sink(nodes, 0);
                top
            }
            None => tail,
        };
        nodes[top].heap_index = None;
        Some(top)
    }

    fn reprioritize(&mut self, nodes: &mut [Node], node: usize, f: f32) {
        let lowered = f < nodes[node].f;
        nodes[node].f = f;
        let slot = nodes[node].heap_index.expect("node is open");
        if lowered {
            self.rise(nodes, slot);
        } else {
            self.sink(nodes, slot);
        }
    }

    fn rise(&mut self, nodes: &mut [Node], mut slot: usize) {
        let moving = self.slots[slot];
        let f = nodes[moving].f;
        while slot > 0 {
            let above = (slot - 1) / 2;
            let parent = self.slots[above];
            if nodes[parent].f <= f {
                break;
            }
            self.place(nodes, slot, parent);
            slot = above;
        }
        self.place(nodes, slot, moving);
    }

    fn sink(&mut self, nodes: &mut [Node], mut slot: usize) {
        let moving = self.slots[slot];
        let f = nodes[moving].f;
        let len = self.slots.len();
        loop {
            let left = 2 * slot + 1;
            if left >= len {
                break;
            }
            let right = left + 1;
            let pick = if right < len && nodes[self.slots[right]].f <= nodes[self.slots[left]].f {
                right
            } else {
                left
            };
            let child = self.slots[pick];
            if nodes[child].f >= f {
                break;
            }
            self.place(nodes, slot, child);
            slot = pick;
        }
        self.place(nodes, slot, moving);
    }
}

/// The walk node evaluator: which neighbours a node has and what they cost.
struct Evaluator<'a, 'b> {
    mob: &'a Walker<'b>,
    block: BlockLookup<'a>,
    nodes: Vec<Node>,
    index: HashMap<(i32, i32, i32), usize>,
    types: HashMap<(i32, i32, i32), PathType>,
    collisions: HashMap<[u64; 6], bool>,
    entity_width: i32,
    entity_height: i32,
}

/// How the block at `pos` looks to pathfinding on its own.
pub fn state_path_type(state: Option<BlockState>) -> PathType {
    state.map_or(PathType::Blocked, BlockState::path_type)
}

/// The path type of `pos` with what is under and around it, the same for
/// every mob.
pub fn static_path_type(pos: BlockPos, block: BlockLookup, min_y: i32) -> PathType {
    let own = state_path_type(block(pos));
    if own != PathType::Open || pos.y < min_y + 1 {
        return own;
    }
    match state_path_type(block(BlockPos::new(pos.x, pos.y - 1, pos.z))) {
        PathType::Open | PathType::Water | PathType::Lava | PathType::Walkable => PathType::Open,
        PathType::Fire => PathType::Fire,
        PathType::Damaging => PathType::Damaging,
        PathType::StickyHoney => PathType::StickyHoney,
        PathType::PowderSnow => PathType::OnTopOfPowderSnow,
        PathType::DamageCautious => PathType::DamageCautious,
        PathType::Trapdoor => PathType::OnTopOfTrapdoor,
        _ => neighbour_path_type(pos, block, PathType::Walkable),
    }
}

fn neighbour_path_type(pos: BlockPos, block: BlockLookup, fallback: PathType) -> PathType {
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                match state_path_type(block(BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz))) {
                    PathType::Damaging => return PathType::DamagingInNeighbor,
                    PathType::Fire | PathType::Lava => return PathType::FireInNeighbor,
                    PathType::Water => return PathType::WaterBorder,
                    PathType::DamageCautious => return PathType::DamageCautious,
                    _ => {}
                }
            }
        }
    }
    fallback
}

/// The top of the collision shape of the block under `pos`.
pub fn floor_level(pos: BlockPos, block: BlockLookup) -> f64 {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let top = match block(below) {
        Some(state) => state
            .collision_boxes()
            .iter()
            .map(|b| b[4] as f64)
            .fold(None, |max: Option<f64>, y| {
                Some(max.map_or(y, |m| m.max(y)))
            }),
        None => Some(1.0),
    };
    below.y as f64 + top.unwrap_or(0.0)
}

fn has_partial_collision(kind: PathType) -> bool {
    matches!(
        kind,
        PathType::Fence | PathType::DoorWoodClosed | PathType::DoorIronClosed
    )
}

/// The default `Walker::floats_on`: water only.
pub fn is_floatable(state: Option<BlockState>) -> bool {
    state.is_some_and(|s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::fluid::key::ENTITY_FLOATABLE,
            s.fluid_state().fluid(),
        )
    })
}

/// The default `Walker::stands_on_fluid`: no fluid.
pub fn stands_on_nothing(_: Option<BlockState>) -> bool {
    false
}

/// A strider's `Walker::stands_on_fluid`: lava.
pub fn stands_on_lava(state: Option<BlockState>) -> bool {
    state.is_some_and(|s| {
        !s.fluid_state().is_empty()
            && tuff_datapack::tags::is(
                tuff_vanilla_data::tag::fluid::key::LAVA,
                s.fluid_state().fluid(),
            )
    })
}

/// A strider's `Walker::floats_on`: water or lava.
pub fn floats_on_lava(state: Option<BlockState>) -> bool {
    is_floatable(state)
        || state.is_some_and(|s| {
            !s.fluid_state().is_empty()
                && tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::fluid::key::LAVA,
                    s.fluid_state().fluid(),
                )
        })
}

impl<'a, 'b> Evaluator<'a, 'b> {
    fn new(mob: &'a Walker<'b>, block: BlockLookup<'a>) -> Self {
        Self {
            mob,
            block,
            nodes: Vec::new(),
            index: HashMap::new(),
            types: HashMap::new(),
            collisions: HashMap::new(),
            entity_width: (mob.width + 1.0).floor() as i32,
            entity_height: (mob.height + 1.0).floor() as i32,
        }
    }

    fn node(&mut self, x: i32, y: i32, z: i32) -> usize {
        if let Some(index) = self.index.get(&(x, y, z)) {
            return *index;
        }
        self.nodes.push(Node {
            x,
            y,
            z,
            g: 0.0,
            h: 0.0,
            f: 0.0,
            came_from: None,
            closed: false,
            walked: 0.0,
            malus: 0.0,
            heap_index: None,
            kind: PathType::Blocked,
        });
        let index = self.nodes.len() - 1;
        self.index.insert((x, y, z), index);
        index
    }

    fn state(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        (self.block)(BlockPos::new(x, y, z))
    }

    fn outside_bound(&self, x: i32, y: i32, z: i32) -> bool {
        let Some((center, radius_sq)) = self.mob.bound else {
            return false;
        };
        let dist = |p: BlockPos| {
            let d = [p.x - center.x, p.y - center.y, p.z - center.z].map(f64::from);
            d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
        };
        let away = dist(BlockPos::new(x, y, z));
        away > radius_sq && away >= dist(self.mob.block_pos())
    }

    fn cached_type(&mut self, x: i32, y: i32, z: i32) -> PathType {
        if let Some(kind) = self.types.get(&(x, y, z)) {
            return *kind;
        }
        let kind = if self.outside_bound(x, y, z) {
            PathType::Blocked
        } else {
            self.mob_type(x, y, z)
        };
        self.types.insert((x, y, z), kind);
        kind
    }

    /// One block's type as this mode sees it: amphibious mobs tell water
    /// touching something solid apart from open water.
    fn static_type(&self, x: i32, y: i32, z: i32) -> PathType {
        let pos = BlockPos::new(x, y, z);
        if self.mob.frog_nodes
            && (self.block)(BlockPos::new(x, y - 1, z)).is_some_and(|s| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::FROG_PREFER_JUMP_TO,
                    s.block(),
                )
            })
        {
            return PathType::Open;
        }
        if self.mob.mode == PathMode::Amphibious
            && state_path_type((self.block)(pos)) == PathType::Water
        {
            let touches_blocked = ALL_DIRECTIONS.iter().any(|direction| {
                let (dx, dy, dz) = direction.step();
                state_path_type(self.state(x + dx, y + dy, z + dz)) == PathType::Blocked
            });
            return if touches_blocked {
                PathType::WaterBorder
            } else {
                PathType::Water
            };
        }
        if self.mob.mode == PathMode::Fly {
            return self.fly_cell_type(pos);
        }
        static_path_type(pos, self.block, self.mob.min_y)
    }

    /// A flyer's view of one block: open air over a fence, cocoa, fire or
    /// something damaging takes on that danger, anything else solid below
    /// reads as walkable, and the neighbours can still make either
    /// air or walkable dangerous.
    fn fly_cell_type(&self, pos: BlockPos) -> PathType {
        let mut kind = state_path_type((self.block)(pos));
        if kind == PathType::Open && pos.y > self.mob.min_y {
            let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
            kind = match state_path_type((self.block)(below)) {
                PathType::Fire | PathType::Lava => PathType::Fire,
                PathType::Damaging => PathType::Damaging,
                PathType::Cocoa => PathType::Cocoa,
                PathType::Fence if below != self.mob.block_pos() => PathType::Fence,
                PathType::Fence => PathType::Open,
                PathType::Walkable | PathType::Open | PathType::Water => PathType::Open,
                _ => PathType::Walkable,
            };
        }
        if matches!(kind, PathType::Walkable | PathType::Open) {
            kind = neighbour_path_type(pos, self.block, kind);
        }
        kind
    }

    fn mob_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if let PathMode::Swim { .. } = self.mob.mode {
            return self.swim_type(x, y, z);
        }
        let mob_pos = self.mob.block_pos();
        let mut types: Vec<PathType> = Vec::new();
        for dx in 0..self.entity_width {
            for dy in 0..self.entity_height {
                for dz in 0..self.entity_width {
                    let mut kind = self.static_type(x + dx, y + dy, z + dz);
                    if kind == PathType::DoorWoodClosed
                        && self.mob.can_open_doors
                        && self.mob.can_pass_doors
                    {
                        kind = PathType::WalkableDoor;
                    }
                    if kind == PathType::DoorOpen && !self.mob.can_pass_doors {
                        kind = PathType::Blocked;
                    }
                    if kind == PathType::Rail
                        && self.static_type(mob_pos.x, mob_pos.y, mob_pos.z) != PathType::Rail
                        && self.static_type(mob_pos.x, mob_pos.y - 1, mob_pos.z) != PathType::Rail
                    {
                        kind = PathType::UnpassableRail;
                    }
                    if !types.contains(&kind) {
                        types.push(kind);
                    }
                }
            }
        }
        types.sort_by_key(|t| t.id());
        if types.len() == 1 {
            return types[0];
        }
        if types.contains(&PathType::Fence) {
            return PathType::Fence;
        }
        if types.contains(&PathType::UnpassableRail) {
            return PathType::UnpassableRail;
        }
        let mut highest = PathType::Blocked;
        let mut highest_malus = self.mob.malus(highest);
        for kind in &types {
            let malus = self.mob.malus(*kind);
            if malus < 0.0 {
                return *kind;
            }
            if malus >= highest_malus {
                highest_malus = malus;
                highest = *kind;
            }
        }
        let own = self.static_type(x, y, z);
        if self.entity_width > 1 {
            let cheaper = self.mob.malus(own) < highest_malus;
            if cheaper && self.mob.malus(PathType::BigMobsCloseToDanger) < highest_malus {
                return PathType::BigMobsCloseToDanger;
            }
            return highest;
        }
        if own == PathType::Open && highest != PathType::Open && highest_malus == 0.0 {
            return PathType::Open;
        }
        highest
    }

    fn can_start_at(&mut self, x: i32, y: i32, z: i32) -> bool {
        let kind = self.cached_type(x, y, z);
        kind != PathType::Open && self.mob.malus(kind) >= 0.0
    }

    fn start(&mut self) -> Option<usize> {
        let b = self.mob.bounds;
        let (box_x, box_y, box_z) = (
            b[0].floor() as i32,
            (b[1] + 0.5).floor() as i32,
            b[2].floor() as i32,
        );
        match self.mob.mode {
            PathMode::Swim { .. } => return Some(self.node(box_x, box_y, box_z)),
            PathMode::Fly => return Some(self.fly_start()),
            PathMode::Amphibious if self.mob.in_water => {
                let box_y = if self.mob.frog_nodes {
                    b[1].floor() as i32
                } else {
                    box_y
                };
                return Some(self.start_node(box_x, box_y, box_z));
            }
            _ => {}
        }
        let [px, py, pz] = self.mob.position;
        let (bx, bz) = (px.floor() as i32, pz.floor() as i32);
        let mut start_y = py.floor() as i32;
        if (self.mob.stands_on_fluid)(self.state(bx, start_y, bz)) {
            while (self.mob.stands_on_fluid)(self.state(bx, start_y, bz)) {
                start_y += 1;
            }
            start_y -= 1;
        } else if self.mob.can_float && self.mob.in_floatable_fluid {
            while is_floatable(self.state(bx, start_y, bz)) {
                start_y += 1;
            }
            start_y -= 1;
        } else if self.mob.on_ground {
            start_y = (py + 0.5).floor() as i32;
        } else {
            let mut y = (py + 1.0).floor() as i32;
            while y > self.mob.min_y {
                start_y = y;
                y -= 1;
                let below = self.state(bx, y, bz);
                let passable = below.is_some_and(|s| {
                    tuff_world_model::chunk::is_air(s) || s.is_land_pathfindable()
                });
                if !passable {
                    break;
                }
            }
        }
        if !self.can_start_at(bx, start_y, bz) {
            let b = self.mob.bounds;
            for (x, z) in [(b[0], b[2]), (b[0], b[5]), (b[3], b[2]), (b[3], b[5])] {
                let (x, z) = (x.floor() as i32, z.floor() as i32);
                if self.can_start_at(x, start_y, z) {
                    return Some(self.start_node(x, start_y, z));
                }
            }
        }
        Some(self.start_node(bx, start_y, bz))
    }

    fn fly_start(&mut self) -> usize {
        let [px, py, pz] = self.mob.position;
        let (bx, bz) = (px.floor() as i32, pz.floor() as i32);
        let start_y = if self.mob.can_float && self.mob.in_water {
            let mut y = py.floor() as i32;
            while is_floatable(self.state(bx, y, bz)) {
                y += 1;
            }
            y
        } else {
            (py + 0.5).floor() as i32
        };
        if !self.fly_can_start_at(bx, start_y, bz) {
            for (x, y, z) in self.fly_start_candidates() {
                if self.fly_can_start_at(x, y, z) {
                    return self.start_node(x, y, z);
                }
            }
        }
        self.start_node(bx, start_y, bz)
    }

    fn fly_can_start_at(&mut self, x: i32, y: i32, z: i32) -> bool {
        let kind = self.cached_type(x, y, z);
        self.mob.malus(kind) >= 0.0
    }

    /// Where else a flyer may start: the corners of its box, or for a small
    /// one the cells of its box grown to a block, as far as ten of them.
    fn fly_start_candidates(&self) -> Vec<(i32, i32, i32)> {
        let b = self.mob.bounds;
        let size = [b[3] - b[0], b[4] - b[1], b[5] - b[2]];
        let block_y = self.mob.position[1].floor() as i32;
        let floor = |v: f64| v.floor() as i32;
        if (size[0] + size[1] + size[2]) / 3.0 >= 1.0 {
            return vec![
                (floor(b[0]), block_y, floor(b[2])),
                (floor(b[0]), block_y, floor(b[5])),
                (floor(b[3]), block_y, floor(b[2])),
                (floor(b[3]), block_y, floor(b[5])),
            ];
        }
        let pad = size.map(|s| (1.100_000_023_841_858 - s).max(0.0));
        let mut cells = Vec::new();
        for x in floor(b[0] - pad[0])..=floor(b[3] + pad[0]) {
            for y in floor(b[1] - pad[1])..=floor(b[4] + pad[1]) {
                for z in floor(b[2] - pad[2])..=floor(b[5] + pad[2]) {
                    cells.push((x, y, z));
                }
            }
        }
        cells.truncate(10);
        cells
    }

    fn start_node(&mut self, x: i32, y: i32, z: i32) -> usize {
        let node = self.node(x, y, z);
        let kind = self.cached_type(x, y, z);
        self.nodes[node].kind = kind;
        self.nodes[node].malus = self.mob.malus(kind);
        node
    }

    fn floor(&self, x: i32, y: i32, z: i32) -> f64 {
        let pos = BlockPos::new(x, y, z);
        if self.mob.can_float && (self.mob.floats_on)(self.state(x, y, z))
            || self.mob.mode == PathMode::Amphibious && is_floatable(self.state(x, y, z))
        {
            return y as f64 + 0.5;
        }
        floor_level(pos, self.block)
    }

    fn jump_height(&self) -> f64 {
        DEFAULT_JUMP_HEIGHT.max(self.mob.max_up_step as f64)
    }

    fn neighbors(&mut self, current: usize, out: &mut Vec<usize>) {
        out.clear();
        match self.mob.mode {
            PathMode::Swim { breaching } => self.swim_neighbors(current, breaching, out),
            PathMode::Walk => self.walk_neighbors(current, out),
            PathMode::Fly => self.fly_neighbors(current, out),
            PathMode::Amphibious => {
                self.walk_neighbors(current, out);
                self.vertical_neighbors(current, out);
                if self.mob.frog_nodes {
                    for node in out.iter() {
                        let n = &mut self.nodes[*node];
                        if n.kind == PathType::Water && n.y < self.mob.sea_level - 10 {
                            n.malus += 1.0;
                        }
                    }
                }
            }
        }
    }

    fn walk_neighbors(&mut self, current: usize, out: &mut Vec<usize>) {
        let (x, y, z) = {
            let n = &self.nodes[current];
            (n.x, n.y, n.z)
        };
        let above = self.cached_type(x, y + 1, z);
        let here = self.cached_type(x, y, z);
        let jump = if self.mob.malus(above) >= 0.0 && here != PathType::StickyHoney {
            self.mob.max_up_step.max(1.0).floor() as i32
        } else {
            0
        };
        let height = self.floor(x, y, z);
        let mut straight: [Option<usize>; 4] = [None; 4];
        for (i, direction) in HORIZONTAL.iter().enumerate() {
            let (dx, _, dz) = direction.step();
            let node = self.accepted(x + dx, y, z + dz, jump, height, *direction, here);
            straight[i] = node;
            if let Some(node) = node
                && self.neighbor_valid(node, current)
            {
                out.push(node);
            }
        }
        for i in 0..4 {
            let second = (i + 1) % 4;
            if !self.diagonal_valid(current, straight[i], straight[second]) {
                continue;
            }
            let (ax, _, az) = HORIZONTAL[i].step();
            let (bx, _, bz) = HORIZONTAL[second].step();
            let node = self.accepted(
                x + ax + bx,
                y,
                z + az + bz,
                jump,
                height,
                HORIZONTAL[i],
                here,
            );
            if let Some(node) = node {
                let n = &self.nodes[node];
                if !n.closed && n.kind != PathType::WalkableDoor && n.malus >= 0.0 {
                    out.push(node);
                }
            }
        }
    }

    /// An amphibious mob's extra neighbours: straight up and down through
    /// water.
    fn vertical_neighbors(&mut self, current: usize, out: &mut Vec<usize>) {
        let (x, y, z) = {
            let n = &self.nodes[current];
            (n.x, n.y, n.z)
        };
        let above = self.cached_type(x, y + 1, z);
        let here = self.cached_type(x, y, z);
        let jump = if self.mob.malus(above) >= 0.0 && here != PathType::StickyHoney {
            self.mob.max_up_step.max(1.0).floor() as i32
        } else {
            0
        };
        let height = self.floor(x, y, z);
        let up = self.accepted(x, y + 1, z, (jump - 1).max(0), height, Facing::Up, here);
        let down = self.accepted(x, y - 1, z, jump, height, Facing::Down, here);
        if let Some(up) = up.filter(|n| self.vertical_valid(*n, current)) {
            out.push(up);
        }
        if let Some(down) = down.filter(|n| self.vertical_valid(*n, current))
            && here != PathType::Trapdoor
        {
            out.push(down);
        }
    }

    fn vertical_valid(&self, node: usize, current: usize) -> bool {
        self.neighbor_valid(node, current) && self.nodes[node].kind == PathType::Water
    }

    fn swim_neighbors(&mut self, current: usize, breaching: bool, out: &mut Vec<usize>) {
        let (x, y, z) = {
            let n = &self.nodes[current];
            (n.x, n.y, n.z)
        };
        let mut around: [Option<usize>; 6] = [None; 6];
        for (i, direction) in ALL_DIRECTIONS.iter().enumerate() {
            let (dx, dy, dz) = direction.step();
            let node = self.swim_accepted(x + dx, y + dy, z + dz, breaching);
            around[i] = node;
            if let Some(node) = node.filter(|n| !self.nodes[*n].closed) {
                out.push(node);
            }
        }
        let at = |facing: Facing| {
            around[ALL_DIRECTIONS
                .iter()
                .position(|d| *d == facing)
                .expect("every direction")]
        };
        for direction in HORIZONTAL {
            let second = direction.clockwise();
            let has_malus = |node: Option<usize>| node.is_some_and(|n| self.nodes[n].malus >= 0.0);
            if !(has_malus(at(direction)) && has_malus(at(second))) {
                continue;
            }
            let (ax, _, az) = direction.step();
            let (bx, _, bz) = second.step();
            let node = self.swim_accepted(x + ax + bx, y, z + az + bz, breaching);
            if let Some(node) = node.filter(|n| !self.nodes[*n].closed) {
                out.push(node);
            }
        }
    }

    fn fly_accepted(&mut self, x: i32, y: i32, z: i32) -> Option<usize> {
        let kind = self.cached_type(x, y, z);
        let cost = self.mob.malus(kind);
        if cost < 0.0 {
            return None;
        }
        let node = self.with_cost(x, y, z, kind, cost);
        if kind == PathType::Walkable {
            self.nodes[node].malus += 1.0;
        }
        Some(node)
    }

    /// The 26 surrounding cells in the game's order: the four sides, up
    /// and down, then the two-axis and three-axis diagonals, each only when
    /// every cell it is made of is passable.
    fn fly_neighbors(&mut self, current: usize, out: &mut Vec<usize>) {
        const CELLS: [[i32; 3]; 26] = [
            [0, 0, 1],
            [-1, 0, 0],
            [1, 0, 0],
            [0, 0, -1],
            [0, 1, 0],
            [0, -1, 0],
            [0, 1, 1],
            [-1, 1, 0],
            [1, 1, 0],
            [0, 1, -1],
            [0, -1, 1],
            [-1, -1, 0],
            [1, -1, 0],
            [0, -1, -1],
            [1, 0, -1],
            [1, 0, 1],
            [-1, 0, -1],
            [-1, 0, 1],
            [1, 1, -1],
            [1, 1, 1],
            [-1, 1, -1],
            [-1, 1, 1],
            [1, -1, -1],
            [1, -1, 1],
            [-1, -1, -1],
            [-1, -1, 1],
        ];
        const NEEDS: [&[usize]; 26] = [
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[0, 4],
            &[1, 4],
            &[2, 4],
            &[3, 4],
            &[0, 5],
            &[1, 5],
            &[2, 5],
            &[3, 5],
            &[3, 2],
            &[0, 2],
            &[3, 1],
            &[0, 1],
            &[14, 3, 2, 4, 9, 8],
            &[15, 0, 2, 4, 6, 8],
            &[16, 3, 1, 4, 9, 7],
            &[17, 0, 1, 4, 6, 7],
            &[14, 3, 2, 5, 13, 12],
            &[15, 0, 2, 5, 10, 12],
            &[16, 3, 1, 5, 13, 11],
            &[17, 0, 1, 5, 10, 11],
        ];
        let (x, y, z) = {
            let n = &self.nodes[current];
            (n.x, n.y, n.z)
        };
        let mut found: [Option<usize>; 26] = [None; 26];
        for (i, [dx, dy, dz]) in CELLS.iter().enumerate() {
            let node = self.fly_accepted(x + dx, y + dy, z + dz);
            found[i] = node;
            let open = node.is_some_and(|n| !self.nodes[n].closed);
            let supported = NEEDS[i]
                .iter()
                .all(|r| found[*r].is_some_and(|n| self.nodes[n].malus >= 0.0));
            if open && supported {
                out.extend(node);
            }
        }
    }

    fn swim_accepted(&mut self, x: i32, y: i32, z: i32, breaching: bool) -> Option<usize> {
        let kind = self.cached_type(x, y, z);
        if !(breaching && kind == PathType::Breach || kind == PathType::Water) {
            return None;
        }
        let cost = self.mob.malus(kind);
        if cost < 0.0 {
            return None;
        }
        let node = self.with_cost(x, y, z, kind, cost);
        let dry = self
            .state(x, y, z)
            .is_none_or(|s| s.fluid_state().is_empty());
        if dry {
            self.nodes[node].malus += BREACH_MALUS;
        }
        Some(node)
    }

    /// How a swimmer sees its box placed at a node: all water, water with
    /// air just above it to breach into, or blocked. The final check reads
    /// the box's far corner, as the game does.
    fn swim_type(&self, x: i32, y: i32, z: i32) -> PathType {
        let water = |s: BlockState| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::fluid::key::WATER,
                s.fluid_state().fluid(),
            )
        };
        let mut last = BlockPos::new(x, y, z);
        for xx in x..x + self.entity_width {
            for yy in y..y + self.entity_height {
                for zz in z..z + self.entity_width {
                    last = BlockPos::new(xx, yy, zz);
                    let Some(state) = self.state(xx, yy, zz) else {
                        return PathType::Blocked;
                    };
                    let below = self.state(xx, yy - 1, zz);
                    if state.fluid_state().is_empty()
                        && below.is_some_and(BlockState::is_water_pathfindable)
                        && tuff_world_model::chunk::is_air(state)
                    {
                        return PathType::Breach;
                    }
                    if !water(state) {
                        return PathType::Blocked;
                    }
                }
            }
        }
        if (self.block)(last).is_some_and(BlockState::is_water_pathfindable) {
            PathType::Water
        } else {
            PathType::Blocked
        }
    }

    fn neighbor_valid(&self, node: usize, current: usize) -> bool {
        let n = &self.nodes[node];
        !(n.closed || (n.malus < 0.0 && self.nodes[current].malus >= 0.0))
    }

    fn diagonal_valid(&self, pos: usize, ew: Option<usize>, ns: Option<usize>) -> bool {
        let (Some(ew), Some(ns)) = (ew, ns) else {
            return false;
        };
        let centre = &self.nodes[pos];
        let sides = [&self.nodes[ew], &self.nodes[ns]];
        let raised_or_door = |side: &Node| side.y > centre.y || side.kind == PathType::WalkableDoor;
        if sides.iter().any(|s| raised_or_door(s)) {
            return false;
        }
        let wide = self.mob.width > 1.0;
        if wide && sides.iter().any(|s| s.malus > 0.0) {
            return false;
        }
        let squeeze_between_posts =
            (self.mob.width as f64) < 0.5 && sides.iter().all(|s| s.kind == PathType::Fence);
        squeeze_between_posts || sides.iter().all(|s| s.y < centre.y || s.malus >= 0.0)
    }

    fn collides(&mut self, area: Aabb) -> bool {
        let key = area.map(f64::to_bits);
        if let Some(hit) = self.collisions.get(&key) {
            return *hit;
        }
        let hit = !physics::no_collision(area, self.block);
        self.collisions.insert(key, hit);
        hit
    }

    fn reaches_without_collision(&mut self, node: usize) -> bool {
        let mut bounds = self.mob.bounds;
        let n = &self.nodes[node];
        let size = [
            bounds[3] - bounds[0],
            bounds[4] - bounds[1],
            bounds[5] - bounds[2],
        ];
        let delta = [
            n.x as f64 - self.mob.position[0] + size[0] / 2.0,
            n.y as f64 - self.mob.position[1] + size[1] / 2.0,
            n.z as f64 - self.mob.position[2] + size[2] / 2.0,
        ];
        let length = delta.iter().map(|v| v * v).sum::<f64>().sqrt();
        let average = (size[0] + size[1] + size[2]) / 3.0;
        let steps = (length / average).ceil() as i32;
        let step = delta.map(|v| v * (1.0 / steps as f64));
        for _ in 1..=steps {
            bounds = physics::offset(bounds, step);
            if self.collides(bounds) {
                return false;
            }
        }
        true
    }

    fn with_cost(&mut self, x: i32, y: i32, z: i32, kind: PathType, cost: f32) -> usize {
        let node = self.node(x, y, z);
        let n = &mut self.nodes[node];
        n.kind = kind;
        n.malus = n.malus.max(cost);
        node
    }

    fn blocked(&mut self, x: i32, y: i32, z: i32) -> usize {
        let node = self.node(x, y, z);
        let n = &mut self.nodes[node];
        n.kind = PathType::Blocked;
        n.malus = -1.0;
        node
    }

    fn closed_node(&mut self, x: i32, y: i32, z: i32, kind: PathType) -> usize {
        let node = self.node(x, y, z);
        let n = &mut self.nodes[node];
        n.closed = true;
        n.kind = kind;
        n.malus = kind.malus();
        node
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn accepted(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        jump: i32,
        height: f64,
        direction: Facing,
        here: PathType,
    ) -> Option<usize> {
        let floor = self.floor(x, y, z);
        if floor - height > self.jump_height() {
            return None;
        }
        let kind = self.cached_type(x, y, z);
        let cost = self.mob.malus(kind);
        let mut best = (cost >= 0.0).then(|| self.with_cost(x, y, z, kind, cost));
        if has_partial_collision(here)
            && let Some(node) = best
            && self.nodes[node].malus >= 0.0
            && !self.reaches_without_collision(node)
        {
            best = None;
        }
        if kind == PathType::Walkable
            || self.mob.mode == PathMode::Amphibious && kind == PathType::Water
        {
            return best;
        }
        let unusable = best.is_none_or(|node| self.nodes[node].malus < 0.0);
        if unusable
            && jump > 0
            && (kind != PathType::Fence || self.mob.can_walk_over_fences)
            && kind != PathType::UnpassableRail
            && kind != PathType::Trapdoor
            && kind != PathType::PowderSnow
        {
            return self.jump_on(x, y, z, jump, height, direction, here);
        }
        if kind == PathType::Water && !self.mob.can_float && self.mob.mode != PathMode::Amphibious {
            return self.first_non_water_below(x, y, z, best);
        }
        if kind == PathType::Open {
            return Some(self.first_ground_below(x, y, z));
        }
        if has_partial_collision(kind) && best.is_none() {
            return Some(self.closed_node(x, y, z, kind));
        }
        best
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn jump_on(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        jump: i32,
        height: f64,
        direction: Facing,
        here: PathType,
    ) -> Option<usize> {
        let above = self.accepted(x, y + 1, z, jump - 1, height, direction, here)?;
        if self.mob.width >= 1.0 {
            return Some(above);
        }
        let kind = self.nodes[above].kind;
        if kind != PathType::Open && kind != PathType::Walkable {
            return Some(above);
        }
        let (dx, _, dz) = direction.step();
        let center = [(x - dx) as f64 + 0.5, (z - dz) as f64 + 0.5];
        let half = self.mob.width as f64 / 2.0;
        let (ax, ay, az) = {
            let n = &self.nodes[above];
            (n.x, n.y, n.z)
        };
        let area = [
            center[0] - half,
            floor_level(
                BlockPos::new(center[0].floor() as i32, y + 1, center[1].floor() as i32),
                self.block,
            ) + 0.001,
            center[1] - half,
            center[0] + half,
            self.mob.height as f64 + floor_level(BlockPos::new(ax, ay, az), self.block) - 0.002,
            center[1] + half,
        ];
        if self.collides(area) {
            return None;
        }
        Some(above)
    }

    fn first_non_water_below(
        &mut self,
        x: i32,
        mut y: i32,
        z: i32,
        mut best: Option<usize>,
    ) -> Option<usize> {
        loop {
            y -= 1;
            if y <= self.mob.min_y {
                return best;
            }
            let kind = self.cached_type(x, y, z);
            if kind != PathType::Water {
                return best;
            }
            let cost = self.mob.malus(kind);
            best = Some(self.with_cost(x, y, z, kind, cost));
        }
    }

    fn first_ground_below(&mut self, x: i32, y: i32, z: i32) -> usize {
        let mut current = y - 1;
        while current >= self.mob.min_y {
            if y - current > self.mob.max_fall_distance {
                return self.blocked(x, current, z);
            }
            let kind = self.cached_type(x, current, z);
            let cost = self.mob.malus(kind);
            if kind != PathType::Open {
                if cost >= 0.0 {
                    return self.with_cost(x, current, z, kind, cost);
                }
                return self.blocked(x, current, z);
            }
            current -= 1;
        }
        self.blocked(x, y, z)
    }
}

/// One requested destination and the closest node the search has seen to it.
struct Goal {
    node: usize,
    pos: BlockPos,
    closest_h: f32,
    closest: Option<usize>,
    within_reach: bool,
}

/// Lowest straight-line distance from `node` to any goal, noting on each
/// goal whether `node` is the closest seen so far.
fn estimate(nodes: &[Node], goals: &mut [Goal], node: usize) -> f32 {
    goals.iter_mut().fold(f32::MAX, |lowest, goal| {
        let h = nodes[node].distance(&nodes[goal.node]);
        if h < goal.closest_h {
            goal.closest_h = h;
            goal.closest = Some(node);
        }
        lowest.min(h)
    })
}

fn mark_reached(nodes: &[Node], goals: &mut [Goal], node: usize, reach: i32) -> bool {
    let at = &nodes[node];
    let mut any = false;
    for goal in goals.iter_mut() {
        let to = &nodes[goal.node];
        let manhattan = (at.x - to.x).abs() + (at.y - to.y).abs() + (at.z - to.z).abs();
        if manhattan as f32 <= reach as f32 {
            goal.within_reach = true;
            any = true;
        }
    }
    any
}

fn trace_back(nodes: &[Node], end: usize) -> Vec<usize> {
    let mut chain: Vec<usize> = std::iter::successors(Some(end), |&i| nodes[i].came_from).collect();
    chain.reverse();
    chain
}

/// Searches for a path from the mob to any of `targets`, stopping within
/// `reach` blocks (Manhattan) of one, following at most `max_length`
/// blocks and visiting at most `max_visited` nodes. `budget` is the
/// region's node allowance for this tick; the visits are taken from it.
pub fn find_path(
    mob: &Walker,
    targets: &[BlockPos],
    max_length: f32,
    reach: i32,
    max_visited: u32,
    block: BlockLookup,
    budget: &mut u32,
) -> Option<Path> {
    let mut ev = Evaluator::new(mob, block);
    let origin = ev.start()?;
    let mut goals: Vec<Goal> = targets
        .iter()
        .map(|&pos| Goal {
            node: ev.node(pos.x, pos.y, pos.z),
            pos,
            closest_h: f32::MAX,
            closest: None,
            within_reach: false,
        })
        .collect();
    let h0 = estimate(&ev.nodes, &mut goals, origin);
    let first = &mut ev.nodes[origin];
    (first.g, first.h, first.f) = (0.0, h0, h0);
    let mut frontier = Frontier::default();
    frontier.push(&mut ev.nodes, origin);
    let cap = max_visited.min(*budget);
    let mut spent = 0u32;
    let mut arrived = false;
    let mut expanded = Vec::new();
    let mut around = Vec::with_capacity(8);
    while let Some(here) = frontier.take_min(&mut ev.nodes) {
        spent += 1;
        if spent >= cap {
            break;
        }
        ev.nodes[here].closed = true;
        if mark_reached(&ev.nodes, &mut goals, here, reach) {
            arrived = true;
            break;
        }
        if mob.capture_debug {
            expanded.push(here);
        }
        if ev.nodes[here].distance(&ev.nodes[origin]) >= max_length {
            continue;
        }
        ev.neighbors(here, &mut around);
        let (here_g, here_walked) = (ev.nodes[here].g, ev.nodes[here].walked);
        for &next in &around {
            let step = ev.nodes[here].distance(&ev.nodes[next]);
            let walked = here_walked + step;
            let candidate = &mut ev.nodes[next];
            candidate.walked = walked;
            let g = here_g + step + candidate.malus;
            let open = candidate.heap_index.is_some();
            if walked >= max_length || (open && g >= candidate.g) {
                continue;
            }
            let h = estimate(&ev.nodes, &mut goals, next) * FUDGING;
            let candidate = &mut ev.nodes[next];
            candidate.came_from = Some(here);
            candidate.g = g;
            candidate.h = h;
            let f = g + h;
            if open {
                frontier.reprioritize(&mut ev.nodes, next, f);
            } else {
                ev.nodes[next].f = f;
                frontier.push(&mut ev.nodes, next);
            }
        }
    }
    *budget = budget.saturating_sub(spent);
    let nodes = &ev.nodes;
    let mut chosen: Option<(Path, Vec<usize>)> = None;
    for goal in goals.iter().filter(|g| !arrived || g.within_reach) {
        let Some(end) = goal.closest else { continue };
        let chain = trace_back(nodes, end);
        let steps = chain.iter().map(|&i| nodes[i].path_node()).collect();
        let path = Path::new(steps, goal.pos, goal.within_reach);
        let better = chosen.as_ref().is_none_or(|(best, _)| {
            let shorter = path.nodes.len() < best.nodes.len();
            if arrived {
                shorter
            } else {
                match path.distance_to_target.total_cmp(&best.distance_to_target) {
                    std::cmp::Ordering::Less => true,
                    std::cmp::Ordering::Equal => shorter,
                    std::cmp::Ordering::Greater => false,
                }
            }
        });
        if better {
            chosen = Some((path, chain));
        }
    }
    let (mut path, chain) = chosen?;
    if mob.capture_debug {
        let snapshot = |list: &[usize]| list.iter().map(|&i| debug_node(&nodes[i])).collect();
        path.debug = Some(Box::new(PathDebug {
            nodes: snapshot(&chain),
            open_set: snapshot(&frontier.slots),
            closed_set: snapshot(&expanded),
            targets: goals
                .iter()
                .map(|goal| DebugNode {
                    pos: nodes[goal.node].coords(),
                    walked_distance: 0.0,
                    cost_malus: 0.0,
                    closed: false,
                    path_type: 0,
                    f: 0.0,
                })
                .collect(),
        }));
    }
    Some(path)
}

#[cfg(test)]
mod tests;
