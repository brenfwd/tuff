//! Anti-xray: the chunk data players are sent hides valuable blocks no
//! player could see (every face against an opaque full block) behind
//! plausible ones, and the true blocks are resent once something near them
//! changes or a player starts mining next to them.
//!
//! The server keeps the true world; only what is encoded for clients
//! differs. A [`Plan`] per dimension says what to hide and how. Encoding
//! runs once per chunk version on the rayon pool, so the obfuscated chunk
//! (and its packet) is shared by every viewer; a per-chunk [`Cache`] keyed
//! by section identity skips sections whose blocks and neighbours have not
//! changed since the last encode.

use std::sync::{Arc, LazyLock};

use tuff_vanilla_data::{Block, BlockState};

use crate::config::{ConfigStore, Key, Value};
use crate::world::dimension::Dimension;
use tuff_world_model::chunk::{BlockMask, Chunk, MASK_WORDS, Section};
use tuff_world_model::pos::ChunkPos;

pub const ENABLED: &str = "tuff.antiXray.enabled";
pub const MODE: &str = "tuff.antiXray.mode";
pub const MAX_Y: &str = "tuff.antiXray.maxY";
pub const UPDATE_RADIUS: &str = "tuff.antiXray.updateRadius";
pub const MAX_UPDATE_RADIUS: i32 = 4;

const MODES: &[&str] = &["hide", "fill"];

/// The per-dimension keys, with their default hidden and replacement
/// blocks.
pub const DIMENSIONS: [(&str, &str, &str); 3] = [
    (
        "overworld",
        "minecraft:coal_ore,minecraft:deepslate_coal_ore,minecraft:copper_ore,\
         minecraft:deepslate_copper_ore,minecraft:raw_copper_block,minecraft:iron_ore,\
         minecraft:deepslate_iron_ore,minecraft:raw_iron_block,minecraft:gold_ore,\
         minecraft:deepslate_gold_ore,minecraft:redstone_ore,minecraft:deepslate_redstone_ore,\
         minecraft:lapis_ore,minecraft:deepslate_lapis_ore,minecraft:diamond_ore,\
         minecraft:deepslate_diamond_ore,minecraft:emerald_ore,minecraft:deepslate_emerald_ore,\
         minecraft:mossy_cobblestone,minecraft:obsidian,minecraft:clay,minecraft:chest,\
         minecraft:ender_chest",
        "minecraft:stone,minecraft:deepslate,minecraft:oak_planks",
    ),
    (
        "the_nether",
        "minecraft:ancient_debris,minecraft:nether_gold_ore,minecraft:nether_quartz_ore",
        "minecraft:netherrack,minecraft:basalt,minecraft:blackstone,minecraft:gravel,\
         minecraft:soul_sand,minecraft:soul_soil",
    ),
    ("the_end", "", "minecraft:end_stone"),
];

pub fn hidden_key(dimension: &str) -> String {
    format!("tuff.antiXray.{dimension}.hiddenBlocks")
}

pub fn replacement_key(dimension: &str) -> String {
    format!("tuff.antiXray.{dimension}.replacementBlocks")
}

/// Block names in a list setting: separated by commas or spaces, with or
/// without the `minecraft:` namespace.
pub fn parse_blocks(text: &str) -> Result<Vec<Block>, String> {
    let mut blocks = Vec::new();
    for name in text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|name| !name.is_empty())
    {
        if let Some(block) = Block::from_name(name) {
            blocks.push(block);
        } else {
            return Err(format!("unknown block {name}"));
        }
    }
    Ok(blocks)
}

fn block_list(value: &Value) -> Result<(), String> {
    match value {
        Value::String(text) => parse_blocks(text).map(|_| ()),
        _ => Err("must be a list of block names".to_owned()),
    }
}

pub fn config_keys() -> Vec<Key> {
    let mut keys = vec![
        Key::new(
            ENABLED,
            Value::Bool(false),
            "Hide ores and other valuable blocks from x-ray clients: chunks are sent with blocks nobody can see replaced, and the true blocks follow once they are exposed or mined towards",
        ),
        Key::new(
            MODE,
            Value::String("hide".to_owned()),
            "hide: hidden blocks are sent as a neighbouring stone-like block; fill: every enclosed hidden or replacement block is sent as a random hidden block (more traffic, x-ray shows noise)",
        )
        .hinted(MODES)
        .validated(|value| match value {
            Value::String(mode) if MODES.contains(&mode.as_str()) => Ok(()),
            _ => Err("must be hide or fill".to_owned()),
        }),
        Key::new(
            MAX_Y,
            Value::Int(64),
            "Highest y at which blocks are hidden",
        ),
        Key::new(
            UPDATE_RADIUS,
            Value::Int(2),
            "How far (in blocks, counted along the axes) around a changed or mined block the true blocks are resent",
        )
        .validated(|value| match value {
            Value::Int(n) if (1..=MAX_UPDATE_RADIUS as i64).contains(n) => Ok(()),
            _ => Err(format!("must be between 1 and {MAX_UPDATE_RADIUS}")),
        }),
    ];
    for (dimension, hidden, replacement) in DIMENSIONS {
        keys.push(
            Key::new(
                &hidden_key(dimension),
                Value::String(hidden.to_owned()),
                &format!("Blocks hidden in {dimension} (empty turns anti-xray off there)"),
            )
            .validated(block_list),
        );
        keys.push(
            Key::new(
                &replacement_key(dimension),
                Value::String(replacement.to_owned()),
                &format!(
                    "Blocks of {dimension} that stand in for hidden ones (the first is the fallback) and that fill mode also replaces"
                ),
            )
            .validated(block_list),
        );
    }
    keys
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Hidden blocks are sent as a neighbouring block.
    Hide,
    /// Enclosed hidden and replacement blocks are sent as random hidden
    /// blocks.
    Fill,
}

/// A set of block states, one bit per value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSet(Box<[u64]>);

impl StateSet {
    pub fn empty() -> Self {
        Self(vec![0; BlockState::COUNT.div_ceil(64)].into_boxed_slice())
    }

    pub fn of_blocks(blocks: impl IntoIterator<Item = Block>) -> Self {
        let mut set = Self::empty();
        for block in blocks {
            for state in block.states() {
                set.insert(state);
            }
        }
        set
    }

    pub fn insert(&mut self, state: BlockState) {
        let key = state.key() as usize;
        self.0[key >> 6] |= 1 << (key & 63);
    }

    #[inline]
    fn has(&self, key: usize) -> bool {
        self.0[key >> 6] >> (key & 63) & 1 != 0
    }

    #[inline]
    pub fn contains(&self, state: BlockState) -> bool {
        self.has(state.key() as usize)
    }

    pub fn union(&self, other: &StateSet) -> StateSet {
        Self(self.0.iter().zip(&other.0).map(|(a, b)| a | b).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|word| *word == 0)
    }
}

/// Opaque full blocks: a block whose every neighbour is one cannot be seen.
static OCCLUDING: LazyLock<StateSet> = LazyLock::new(|| {
    let mut set = StateSet::empty();
    for id in 0..BlockState::COUNT as u32 {
        if let Some(state) = BlockState::from_id(id)
            && state.is_solid_render()
        {
            set.insert(state);
        }
    }
    set
});

/// Whether `state` hides the faces of its neighbours from view.
#[inline]
pub fn occludes(state: BlockState) -> bool {
    OCCLUDING.contains(state)
}

/// What one dimension hides and how.
#[derive(Debug, PartialEq)]
pub struct Plan {
    pub mode: Mode,
    pub max_y: i32,
    pub radius: i32,
    hidden: StateSet,
    replacement: StateSet,
    /// Every state the plan may send differently.
    altered: StateSet,
    filler: BlockState,
    pool: Box<[BlockState]>,
    seed: u64,
}

impl Plan {
    /// `None` when nothing is hidden. `replacement`'s first block stands in
    /// when no neighbour fits; fill mode draws from `hidden`, leaving out
    /// blocks with block entities.
    pub fn new(
        mode: Mode,
        max_y: i32,
        radius: i32,
        hidden: &[Block],
        replacement: &[Block],
        seed: u64,
    ) -> Option<Plan> {
        let hidden_set = StateSet::of_blocks(hidden.iter().copied().filter(|b| *b != Block::Air));
        if hidden_set.is_empty() {
            return None;
        }
        let replacement_set = StateSet::of_blocks(replacement.iter().copied());
        let altered = match mode {
            Mode::Hide => hidden_set.clone(),
            Mode::Fill => hidden_set.union(&replacement_set),
        };
        let mut pool: Vec<BlockState> = hidden
            .iter()
            .filter(|block| !block.creates_block_entity() && **block != Block::Air)
            .map(|block| block.default_state())
            .collect();
        pool.dedup();
        Some(Plan {
            mode,
            max_y,
            radius: radius.clamp(1, MAX_UPDATE_RADIUS),
            hidden: hidden_set,
            replacement: replacement_set,
            altered,
            filler: replacement
                .first()
                .map_or(Block::Stone.default_state(), |block| block.default_state()),
            pool: pool.into_boxed_slice(),
            seed,
        })
    }

    pub fn hides(&self, state: BlockState) -> bool {
        self.hidden.contains(state)
    }

    /// Whether a block in `state` may have been sent as something else.
    #[inline]
    pub fn may_alter(&self, state: BlockState) -> bool {
        self.altered.contains(state)
    }

    fn stand_in(&self, sides: impl FnOnce() -> [Side; 6], x: i32, y: i32, z: i32) -> BlockState {
        match self.mode {
            Mode::Fill => {
                if self.pool.is_empty() {
                    return self.filler;
                }
                let hash = mix(self.seed
                    ^ (x as u32 as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
                    ^ (y as u32 as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
                    ^ (z as u32 as u64).wrapping_mul(0x1656_67b1_9e37_79f9));
                self.pool[((hash as u128 * self.pool.len() as u128) >> 64) as usize]
            }
            Mode::Hide => {
                let sides = sides();
                let states = sides.iter().filter_map(|side| match side {
                    Side::Block(state) => Some(*state),
                    _ => None,
                });
                states
                    .clone()
                    .find(|state| self.replacement.contains(*state))
                    .or_else(|| {
                        states.clone().find(|state| {
                            !self.hidden.contains(*state) && !state.block().creates_block_entity()
                        })
                    })
                    .unwrap_or(self.filler)
            }
        }
    }
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Builds each dimension's [`Plan`]. One feature may replace the default,
/// which reads the `tuff.antiXray.*` settings.
pub trait AntiXrayPolicy: Send + Sync {
    /// `seed` keys fill mode's random blocks. `None` sends chunks as they
    /// are.
    fn plan(&self, config: &ConfigStore, dimension: &Dimension, seed: u64) -> Option<Plan>;
}

/// The `tuff.antiXray.*` settings.
pub struct ConfigPolicy;

impl ConfigPolicy {
    /// The settings group a dimension reads: its own for the three
    /// vanilla dimensions, else by dimension type, else the overworld's.
    pub fn group(dimension: &Dimension) -> &'static str {
        let named = |name: &str| {
            DIMENSIONS
                .iter()
                .map(|(group, _, _)| *group)
                .find(|group| name.strip_prefix("minecraft:") == Some(*group))
        };
        named(&dimension.key)
            .or_else(|| named(dimension.kind.name))
            .unwrap_or("overworld")
    }
}

impl AntiXrayPolicy for ConfigPolicy {
    fn plan(&self, config: &ConfigStore, dimension: &Dimension, seed: u64) -> Option<Plan> {
        if !config.get_bool(ENABLED).unwrap_or(false) {
            return None;
        }
        let group = Self::group(dimension);
        let list = |key: String| {
            config
                .get_string(&key)
                .and_then(|text| parse_blocks(&text).ok())
                .unwrap_or_default()
        };
        let hidden = list(hidden_key(group));
        let mode = match config.get_string(MODE).as_deref() {
            Some("fill") => Mode::Fill,
            _ => Mode::Hide,
        };
        Plan::new(
            mode,
            config.get_int(MAX_Y).unwrap_or(64).clamp(-4096, 4096) as i32,
            config.get_int(UPDATE_RADIUS).unwrap_or(2) as i32,
            &hidden,
            &list(replacement_key(group)),
            seed,
        )
    }
}

/// The horizontal neighbours an encode reads, in this order.
pub const AROUND: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

/// What lies past a face of a block.
#[derive(Clone, Copy, Debug)]
enum Side {
    Block(BlockState),
    /// Below the world: nothing to see from there.
    Floor,
    /// Unknown or open: counts as exposed.
    Open,
    /// Not loaded yet, held shut until it is: counts as enclosed.
    Closed,
}

/// A section with the six it touches, `None` where unknown.
struct View<'a> {
    this: &'a Section,
    below: Option<&'a Section>,
    above: Option<&'a Section>,
    sides: [Option<&'a Section>; 4],
    closed: [bool; 4],
    bottom: bool,
}

impl View<'_> {
    #[inline]
    fn side(&self, i: usize, face: usize) -> Side {
        let at = |section: Option<&Section>, index: usize, closed: bool| {
            section.map_or(if closed { Side::Closed } else { Side::Open }, |s| {
                Side::Block(s.get(index))
            })
        };
        let (x, z, y) = (i & 15, (i >> 4) & 15, i >> 8);
        match face {
            0 if y > 0 => Side::Block(self.this.get(i - 256)),
            0 if self.bottom => Side::Floor,
            0 => at(self.below, i + 15 * 256, false),
            1 if y < 15 => Side::Block(self.this.get(i + 256)),
            1 => at(self.above, i - 15 * 256, false),
            2 if x > 0 => Side::Block(self.this.get(i - 1)),
            2 => at(self.sides[0], i + 15, self.closed[0]),
            3 if x < 15 => Side::Block(self.this.get(i + 1)),
            3 => at(self.sides[1], i - 15, self.closed[1]),
            4 if z > 0 => Side::Block(self.this.get(i - 16)),
            4 => at(self.sides[2], i + 15 * 16, self.closed[2]),
            _ if z < 15 => Side::Block(self.this.get(i + 16)),
            _ => at(self.sides[3], i - 15 * 16, self.closed[3]),
        }
    }
}

/// What the last encode of one chunk computed, by section.
#[derive(Default)]
pub struct Cache {
    plan: Option<Arc<Plan>>,
    entries: Vec<Option<Entry>>,
}

/// Holding the `Arc`s keeps their count above one, so a chunk edit copies
/// the section instead of mutating it in place under the same pointer.
struct Entry {
    source: Arc<Section>,
    /// The sections around it, when the result depended on them.
    around: Option<[Option<Arc<Section>>; 6]>,
    closed: [bool; 4],
    output: Arc<Section>,
}

/// How much of an encode had to be worked out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Sections scanned.
    pub computed: u32,
    /// Sections taken from the cache.
    pub reused: u32,
    /// Blocks sent as something else.
    pub replaced: u32,
}

fn same(a: &Option<Arc<Section>>, b: &Option<Arc<Section>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// The chunk as clients are to see it. `around` holds the neighbours in
/// [`AROUND`] order; a missing one leaves the blocks along that edge
/// exposed. Untouched sections stay shared with `chunk`.
pub fn obfuscate(
    chunk: &Chunk,
    pos: ChunkPos,
    around: &[Option<Chunk>; 4],
    plan: &Arc<Plan>,
    cache: &mut Cache,
) -> (Chunk, Stats) {
    obfuscate_closed(chunk, pos, around, [false; 4], plan, cache)
}

/// As [`obfuscate`], but a missing neighbour whose `closed` flag is set
/// counts as solid: its blocks are not to be learned before it arrives.
pub fn obfuscate_closed(
    chunk: &Chunk,
    pos: ChunkPos,
    around: &[Option<Chunk>; 4],
    closed: [bool; 4],
    plan: &Arc<Plan>,
    cache: &mut Cache,
) -> (Chunk, Stats) {
    let shape = chunk.shape();
    let count = shape.sections();
    if !cache.plan.as_ref().is_some_and(|p| Arc::ptr_eq(p, plan)) || cache.entries.len() != count {
        cache.plan = Some(plan.clone());
        cache.entries = (0..count).map(|_| None).collect();
    }
    let around: [Option<&Chunk>; 4] =
        std::array::from_fn(|k| around[k].as_ref().filter(|c| c.shape() == shape));
    let mut stats = Stats::default();
    let mut changed = false;
    let mut sections = Vec::with_capacity(count);
    for (index, slot) in cache.entries.iter_mut().enumerate() {
        let source = chunk.shared_section(index);
        let base = shape.section_base(index);
        if base > plan.max_y {
            sections.push(source.clone());
            continue;
        }
        let neighbours: [Option<Arc<Section>>; 6] = [
            index
                .checked_sub(1)
                .map(|i| chunk.shared_section(i).clone()),
            (index + 1 < count).then(|| chunk.shared_section(index + 1).clone()),
            around[0].map(|c| c.shared_section(index).clone()),
            around[1].map(|c| c.shared_section(index).clone()),
            around[2].map(|c| c.shared_section(index).clone()),
            around[3].map(|c| c.shared_section(index).clone()),
        ];
        let hit = slot.as_ref().filter(|entry| {
            Arc::ptr_eq(&entry.source, source)
                && entry.closed == closed
                && entry
                    .around
                    .as_ref()
                    .is_none_or(|old| old.iter().zip(&neighbours).all(|(a, b)| same(a, b)))
        });
        let output = match hit {
            Some(entry) => {
                stats.reused += 1;
                entry.output.clone()
            }
            None => {
                stats.computed += 1;
                let view = View {
                    this: source,
                    below: neighbours[0].as_deref(),
                    above: neighbours[1].as_deref(),
                    sides: std::array::from_fn(|k| neighbours[2 + k].as_deref()),
                    closed,
                    bottom: index == 0,
                };
                let origin = (pos.x * 16, base, pos.z * 16);
                let (output, depends) = match obfuscate_section(&view, plan, origin, &mut stats) {
                    Outcome::Changed(changes) => (Arc::new(source.with_changes(&changes)), true),
                    Outcome::Same { depends } => (source.clone(), depends),
                };
                *slot = Some(Entry {
                    source: source.clone(),
                    around: depends.then(|| neighbours.clone()),
                    closed,
                    output: output.clone(),
                });
                output
            }
        };
        changed |= !Arc::ptr_eq(&output, source);
        sections.push(output);
    }
    let sent = if changed {
        chunk.with_shared_sections(sections)
    } else {
        chunk.clone()
    };
    (sent, stats)
}

enum Outcome {
    Changed(Vec<(u16, BlockState)>),
    /// Sent as it is; `depends` when that could change with its neighbours.
    Same {
        depends: bool,
    },
}

/// Up to this many candidates per section are checked one by one; more
/// are checked a word of blocks at a time.
const SPARSE: u32 = 384;

/// The bits of one word's blocks with `x == 0` and with `x == 15`.
const X_LOW: u64 = 0x0001_0001_0001_0001;
const X_HIGH: u64 = 0x8000_8000_8000_8000;

/// For each block of the section, whether the block past `face` of it
/// occludes, given the section's own occlusion.
fn behind(view: &View, own: &BlockMask, face: usize) -> BlockMask {
    let edge = edge(view, face);
    std::array::from_fn(|w| match face {
        0 if w >= 4 => own[w - 4],
        0 => edge[w],
        1 if w < MASK_WORDS - 4 => own[w + 4],
        1 => edge[w],
        2 => (own[w] << 1) & !X_LOW | edge[w],
        3 => (own[w] >> 1) & !X_HIGH | edge[w],
        4 if w & 3 != 0 => own[w] << 16 | own[w - 1] >> 48,
        4 => own[w] << 16 | edge[w],
        _ if w & 3 != 3 => own[w] >> 16 | own[w + 1] << 48,
        _ => own[w] >> 16 | edge[w],
    })
}

/// The occlusion of the blocks across `face` from the section's blocks on
/// that face, set at those blocks' bits.
fn edge(view: &View, face: usize) -> BlockMask {
    let mut out = [0u64; MASK_WORDS];
    let (section, offset) = match face {
        0 if view.bottom => {
            out[..4].fill(!0);
            return out;
        }
        0 => (view.below, 15 * 256),
        1 => (view.above, -15 * 256),
        2 => (view.sides[0], 15),
        3 => (view.sides[1], -15),
        4 => (view.sides[2], 15 * 16),
        _ => (view.sides[3], -15 * 16),
    };
    let Some(section) = section else {
        if face >= 2 && view.closed[face - 2] {
            for a in 0..16 {
                for b in 0..16 {
                    let i = match face {
                        2 => a << 8 | b << 4,
                        3 => a << 8 | b << 4 | 15,
                        4 => a << 8 | b,
                        _ => a << 8 | 15 << 4 | b,
                    };
                    out[i >> 6] |= 1 << (i & 63);
                }
            }
        }
        return out;
    };
    if let Some(state) = section.uniform_state()
        && !occludes(state)
    {
        return out;
    }
    for a in 0..16 {
        for b in 0..16 {
            let i = match face {
                0 => a << 4 | b,
                1 => 15 << 8 | a << 4 | b,
                2 => a << 8 | b << 4,
                3 => a << 8 | b << 4 | 15,
                4 => a << 8 | b,
                _ => a << 8 | 15 << 4 | b,
            };
            let across = (i as isize + offset) as usize;
            out[i >> 6] |= u64::from(occludes(section.get(across))) << (i & 63);
        }
    }
    out
}

/// What the section is to be sent as.
fn obfuscate_section(
    view: &View,
    plan: &Plan,
    (x0, y0, z0): (i32, i32, i32),
    stats: &mut Stats,
) -> Outcome {
    let layers = (plan.max_y - y0 + 1).clamp(0, 16) as usize;
    if !view.this.palette().iter().any(|s| plan.may_alter(*s)) {
        return Outcome::Same { depends: false };
    }
    let mut candidates = view.this.select(|state| plan.may_alter(state));
    candidates[layers * 4..].fill(0);
    if candidates.iter().all(|word| *word == 0) {
        return Outcome::Same { depends: false };
    }
    let count: u32 = candidates.iter().map(|word| word.count_ones()).sum();
    if count <= SPARSE {
        for (w, word) in candidates.iter_mut().enumerate() {
            let mut bits = *word;
            while bits != 0 {
                let bit = bits.trailing_zeros();
                bits &= bits - 1;
                let i = w << 6 | bit as usize;
                let enclosed = (0..6).all(|face| match view.side(i, face) {
                    Side::Block(state) => occludes(state),
                    Side::Floor | Side::Closed => true,
                    Side::Open => false,
                });
                if !enclosed {
                    *word &= !(1 << bit);
                }
            }
        }
    } else {
        let own = view.this.select(occludes);
        for face in 0..6 {
            let behind = behind(view, &own, face);
            for (word, open) in candidates.iter_mut().zip(behind) {
                *word &= open;
            }
        }
    }
    let mut changes = Vec::new();
    for (w, word) in candidates.iter().enumerate() {
        let mut word = *word;
        while word != 0 {
            let i = w << 6 | word.trailing_zeros() as usize;
            word &= word - 1;
            let state = view.this.get(i);
            let (x, y, z) = (
                x0 + (i & 15) as i32,
                y0 + (i >> 8) as i32,
                z0 + ((i >> 4) & 15) as i32,
            );
            let stand_in =
                plan.stand_in(|| std::array::from_fn(|face| view.side(i, face)), x, y, z);
            if stand_in != state {
                changes.push((i as u16, stand_in));
                stats.replaced += 1;
            }
        }
    }
    if changes.is_empty() {
        Outcome::Same { depends: true }
    } else {
        Outcome::Changed(changes)
    }
}

/// Offsets within `radius` blocks counted along the axes.
pub fn offsets(radius: i32) -> impl Iterator<Item = (i32, i32, i32)> {
    (-radius..=radius).flat_map(move |dx| {
        (-radius..=radius).flat_map(move |dy| {
            (-radius..=radius)
                .map(move |dz| (dx, dy, dz))
                .filter(move |(dx, dy, dz)| dx.abs() + dy.abs() + dz.abs() <= radius)
        })
    })
}

#[cfg(test)]
mod tests;
