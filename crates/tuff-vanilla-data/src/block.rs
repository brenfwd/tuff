//! Block states. A [`BlockState`] is the global palette ID the protocol and
//! chunk format use. Its properties are decoded arithmetically: each block's
//! states are the cartesian product of its property values, with properties
//! ordered by name and the last one varying fastest.

use core::fmt;
use core::str::FromStr;

use crate::registry::{Block, Item, SoundEvent};

include!(concat!(env!("OUT_DIR"), "/blocks.rs"));
include!(concat!(env!("OUT_DIR"), "/block_properties.rs"));

/// The block an item places, and its wall variant with the standing
/// block's attachment direction.
type ItemBlock = (Block, Option<(Block, Facing)>);

struct Info {
    sturdy: u32,
    flags: u16,
    full_faces: u16,
    sound: u16,
    shape: u16,
    outline: u16,
}

/// The sounds a block makes and the volume and pitch they play at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundType {
    pub break_sound: SoundEvent,
    pub step_sound: SoundEvent,
    pub place_sound: SoundEvent,
    pub hit_sound: SoundEvent,
    pub fall_sound: SoundEvent,
    pub volume: f32,
    pub pitch: f32,
}

struct InstrumentInfo {
    sound: SoundEvent,
    tunable: bool,
    works_above_note_block: bool,
    custom_sound: bool,
}

/// A door, trapdoor or fence gate: its sounds and whether a hand opens it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Toggle {
    pub open_sound: SoundEvent,
    pub close_sound: SoundEvent,
    pub opens_by_hand: bool,
}

/// How far the game shifts a block's shapes by a hash of its position
/// (plants, bamboo, dripstone): at most `horizontal` along x and z, and
/// down by up to `vertical`, and which of its shapes follow the shift.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeOffset {
    pub horizontal: f32,
    pub vertical: f32,
    pub collision: bool,
    pub outline: bool,
}

impl ShapeOffset {
    /// The shift at block column `x`, `z`.
    pub fn at(self, x: i32, z: i32) -> [f64; 3] {
        let seed = i64::from(x.wrapping_mul(3_129_871)) ^ i64::from(z).wrapping_mul(116_129_781);
        let seed = seed
            .wrapping_mul(seed)
            .wrapping_mul(42_317_861)
            .wrapping_add(seed.wrapping_mul(11))
            >> 16;
        let nibble = |shift: u32| f64::from(((seed >> shift) & 15) as f32 / 15.0);
        let max = f64::from(self.horizontal);
        [
            ((nibble(0) - 0.5) * 0.5).clamp(-max, max),
            (nibble(4) - 1.0) * f64::from(self.vertical),
            ((nibble(8) - 0.5) * 0.5).clamp(-max, max),
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Button {
    /// Ticks a press lasts before the button releases.
    pub press_ticks: u32,
    pub press_sound: SoundEvent,
    pub release_sound: SoundEvent,
}

/// How a bucket takes from a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickupKind {
    /// A fluid block, only at its source level.
    Liquid,
    /// A block holding water in its `waterlogged` property.
    Waterlogged,
    /// The block is taken whole (powder snow, bubble columns).
    Whole,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BucketPickup {
    pub kind: PickupKind,
    /// The filled bucket the player gets.
    pub item: Item,
    pub sound: Option<SoundEvent>,
}

/// Which environment attribute rules a bed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BedRuleAttribute {
    /// `minecraft:gameplay/bed_rule`
    Bed,
    /// `minecraft:gameplay/straw_bed_rule`
    StrawBed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bed {
    pub rule: BedRuleAttribute,
    /// Whether the bed explodes where its rule destroys it on use, rather
    /// than simply going away.
    pub explodes: bool,
}

fn block_lookup<T: Copy>(table: &[(Block, T)], block: Block) -> Option<T> {
    table
        .binary_search_by_key(&(block as usize), |(b, _)| *b as usize)
        .ok()
        .map(|index| table[index].1)
}

impl Instrument {
    fn info(self) -> &'static InstrumentInfo {
        &INSTRUMENTS[self as usize]
    }

    pub fn sound(self) -> SoundEvent {
        self.info().sound
    }

    /// Whether the note changes its pitch.
    pub fn is_tunable(self) -> bool {
        self.info().tunable
    }

    /// Mob heads: set from the block above a note block and play even
    /// with a block there.
    pub fn works_above_note_block(self) -> bool {
        self.info().works_above_note_block
    }

    /// Player heads: the sound comes from the head's own data.
    pub fn has_custom_sound(self) -> bool {
        self.info().custom_sound
    }
}

const REPLACEABLE: u16 = 1;
const SOLID: u16 = 2;
const SOLID_RENDER: u16 = 4;
const FULL_COLLISION: u16 = 8;
const LIQUID: u16 = 16;
const COVERS_WALL_POST: u16 = 32;
const WATER_REPLACEABLE: u16 = 1024;
const LAVA_REPLACEABLE: u16 = 2048;
const DANGEROUS: u16 = 4096;
const SUFFOCATING: u16 = 8192;
const RESPAWNABLE: u16 = 16384;

/// The experience a block drops when mined with a harvesting tool that has
/// no silk touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockExperience {
    Constant(i32),
    /// Uniform in `min..=max`.
    Uniform {
        min: i32,
        max: i32,
    },
    /// `base` plus one uniform `0..bound` roll per bound (spawners).
    Sum {
        base: i32,
        bounds: &'static [i32],
    },
}

impl BlockExperience {
    /// Rolls an amount with `random(bound)` giving a uniform value in
    /// `0..bound`.
    pub fn sample(&self, mut random: impl FnMut(i32) -> i32) -> i32 {
        match *self {
            Self::Constant(value) => value,
            Self::Uniform { min, max } => min + random(max - min + 1),
            Self::Sum { base, bounds } => {
                bounds.iter().fold(base, |sum, bound| sum + random(*bound))
            }
        }
    }

    pub fn min(&self) -> i32 {
        match *self {
            Self::Constant(value) => value,
            Self::Uniform { min, .. } => min,
            Self::Sum { base, .. } => base,
        }
    }

    pub fn max(&self) -> i32 {
        match *self {
            Self::Constant(value) => value,
            Self::Uniform { max, .. } => max,
            Self::Sum { base, bounds } => bounds.iter().map(|b| b - 1).sum::<i32>() + base,
        }
    }
}

/// How much of a face must be solid for something to rest on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Full,
    /// A center post, like a torch or a hanging lantern needs.
    Center,
    /// Solid along the edges, like a rail or pressure plate needs.
    Rigid,
}

impl Facing {
    pub const HORIZONTAL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    pub const fn opposite(self) -> Self {
        match self {
            Self::North => Self::South,
            Self::South => Self::North,
            Self::East => Self::West,
            Self::West => Self::East,
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }

    /// Rotated a quarter turn clockwise seen from above; vertical directions
    /// are unchanged.
    pub const fn clockwise(self) -> Self {
        match self {
            Self::North => Self::East,
            Self::East => Self::South,
            Self::South => Self::West,
            Self::West => Self::North,
            other => other,
        }
    }

    pub const fn counter_clockwise(self) -> Self {
        self.clockwise().opposite()
    }

    pub const fn step(self) -> (i32, i32, i32) {
        match self {
            Self::North => (0, 0, -1),
            Self::South => (0, 0, 1),
            Self::East => (1, 0, 0),
            Self::West => (-1, 0, 0),
            Self::Up => (0, 1, 0),
            Self::Down => (0, -1, 0),
        }
    }

    pub const fn axis(self) -> Axis {
        match self {
            Self::East | Self::West => Axis::X,
            Self::Up | Self::Down => Axis::Y,
            Self::North | Self::South => Axis::Z,
        }
    }

    pub const fn is_horizontal(self) -> bool {
        !matches!(self, Self::Up | Self::Down)
    }

    /// Index in the protocol's direction order: down, up, north, south, west,
    /// east.
    pub const fn id(self) -> usize {
        match self {
            Self::Down => 0,
            Self::Up => 1,
            Self::North => 2,
            Self::South => 3,
            Self::West => 4,
            Self::East => 5,
        }
    }
}

struct PropertyDef {
    property: Property,
    name: &'static str,
    values: &'static [&'static str],
    /// `values` as the typed accessors see them: 0/1 for booleans, the number
    /// for integers, and the enum discriminant otherwise.
    raw: &'static [u8],
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockState(u16);

struct Slot {
    def: &'static PropertyDef,
    index: usize,
    stride: usize,
}

/// Which of a block's state properties its picked stack records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickState {
    pub always: bool,
    pub properties: &'static [&'static str],
    /// The `block_state` the item already carries by default.
    pub item_default: &'static [(&'static str, &'static str)],
}

const fn bytes_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn table_bit(set: &[u64], id: usize) -> bool {
    id / 64 < set.len() && set[id / 64] >> (id % 64) & 1 == 1
}

const FLAG_TICKS_RANDOMLY: u8 = 1;
const FLAG_NOT_AIR: u8 = 2;
const FLAG_FLUID: u8 = 4;
const FLAG_SURFACE_FLUID: u8 = 8;
const FLAG_HEIGHT_FLUID: u8 = 16;

/// Per vanilla state id, the `BlockState::FLAG_*` bits, evaluated once at
/// compile time from the generated tables.
static STATE_FLAGS: [u8; STATE_COUNT] = state_flags();

const fn state_flags() -> [u8; STATE_COUNT] {
    let mut flags = [0u8; STATE_COUNT];
    let mut block = 0;
    while block + 1 < BLOCK_FIRST_STATE.len() {
        let first = BLOCK_FIRST_STATE[block] as usize;
        let end = BLOCK_FIRST_STATE[block + 1] as usize;
        let kind = STATE_BLOCK[first];
        let air = matches!(kind, Block::Air | Block::CaveAir | Block::VoidAir);
        let liquid = matches!(kind, Block::Water | Block::Lava);
        let bubbles = matches!(kind, Block::BubbleColumn);

        let defs = BLOCK_PROPERTIES[block];
        let mut stride = 1;
        let mut length = 0;
        let mut true_index = 0;
        let mut p = defs.len();
        while p > 0 {
            p -= 1;
            let def = defs[p];
            if bytes_eq(def.name, "waterlogged") {
                length = def.values.len();
                let mut v = 0;
                while v < length {
                    if bytes_eq(def.values[v], "true") {
                        true_index = v;
                    }
                    v += 1;
                }
                break;
            }
            stride *= def.values.len();
        }

        let mut id = first;
        while id < end {
            let waterlogged = length != 0 && (id - first) / stride % length == true_index;
            let mut bits = 0;
            if table_bit(&crate::growth::RANDOM_TICKING, id)
                || table_bit(&crate::growth::FLUID_RANDOM_TICKING, id)
            {
                bits |= FLAG_TICKS_RANDOMLY;
            }
            if !air {
                bits |= FLAG_NOT_AIR;
            }
            if crate::fluid::STATE_FLUID[id] != 0 {
                bits |= FLAG_FLUID;
            }
            if liquid || waterlogged {
                bits |= FLAG_SURFACE_FLUID | FLAG_HEIGHT_FLUID;
            } else if bubbles {
                bits |= FLAG_HEIGHT_FLUID;
            }
            flags[id] = bits;
            id += 1;
        }
        block += 1;
    }
    flags
}

impl BlockState {
    pub const COUNT: usize = STATE_COUNT;

    /// Flag: a random tick at the block or its fluid does something.
    pub const FLAG_TICKS_RANDOMLY: u8 = FLAG_TICKS_RANDOMLY;
    /// Flag: the block is not air, cave air or void air.
    pub const FLAG_NOT_AIR: u8 = FLAG_NOT_AIR;
    /// Flag: the state has a non-empty fluid state.
    pub const FLAG_FLUID: u8 = FLAG_FLUID;
    /// Flag: water, lava or waterlogged, as the surface walk counts fluid.
    pub const FLAG_SURFACE_FLUID: u8 = FLAG_SURFACE_FLUID;
    /// Flag: water, lava, a bubble column or waterlogged, as the
    /// heightmaps count fluid.
    pub const FLAG_HEIGHT_FLUID: u8 = FLAG_HEIGHT_FLUID;

    /// The `FLAG_*` bits of this state, one table read.
    #[inline]
    pub fn flags(self) -> u8 {
        STATE_FLAGS[self.id() as usize]
    }

    /// The flags of every vanilla state id, for callers that look up many
    /// states and add bits of their own.
    pub fn flag_table() -> &'static [u8; STATE_COUNT] {
        &STATE_FLAGS
    }

    /// The item picking this state gives, if any: seeds for crops, the head
    /// block of a plant body, nothing for portals and frosted ice.
    pub fn pick_item(self) -> Option<Item> {
        let id = self.id() as u16;
        match PICK_OVERRIDES.binary_search_by_key(&id, |(id, _)| *id) {
            Ok(index) => PICK_OVERRIDES[index].1,
            Err(_) => self.block().pick_item(),
        }
    }

    pub const fn from_id(id: u32) -> Option<Self> {
        if (id as usize) < STATE_COUNT {
            Some(Self(id as u16))
        } else {
            None
        }
    }

    /// The raw palette value, as a `u16` for palette and table keys.
    #[inline]
    pub const fn key(self) -> u16 {
        self.0
    }

    /// The global palette id.
    #[inline]
    pub const fn id(self) -> u32 {
        self.0 as u32
    }

    /// For generated tables, whose IDs are checked at build time.
    pub(crate) const fn raw(id: u16) -> Self {
        Self(id)
    }

    #[inline]
    pub fn block(self) -> Block {
        STATE_BLOCK[self.id() as usize]
    }

    #[inline]
    pub fn is_default(self) -> bool {
        self.id() as u16 == BLOCK_DEFAULT_STATE[self.block() as usize]
    }

    /// Whether breaking this state drops nothing unless the held item's tool
    /// is correct for it (see [`crate::Tool::is_correct_for_drops`]).
    #[inline]
    pub fn requires_correct_tool_for_drops(self) -> bool {
        let id = self.id() as usize;
        STATE_REQUIRES_CORRECT_TOOL_FOR_DROPS[id / 64] >> (id % 64) & 1 == 1
    }

    /// The state's hardness, or `None` for unbreakable blocks such as bedrock.
    #[inline]
    pub fn destroy_speed(self) -> Option<f32> {
        let speed = DESTROY_SPEEDS[STATE_DESTROY_SPEED[self.id() as usize] as usize];
        if speed < 0.0 { None } else { Some(speed) }
    }

    #[inline]
    fn info(self) -> &'static Info {
        &INFOS[STATE_INFO[self.id() as usize] as usize]
    }

    /// Whether the given face offers `support` to a block resting on it.
    pub fn is_face_sturdy(self, face: Facing, support: Support) -> bool {
        self.info().sturdy >> (face.id() * 3 + support as usize) & 1 == 1
    }

    /// Whether the shape this state occludes neighbouring faces with covers
    /// the whole face toward `face`.
    pub fn is_occlusion_face_full(self, face: Facing) -> bool {
        self.info().full_faces >> face.id() & 1 == 1
    }

    /// Whether the collision shape covers the whole face toward `face`.
    pub fn is_collision_face_full(self, face: Facing) -> bool {
        self.info().full_faces >> (6 + face.id()) & 1 == 1
    }

    /// Whether the outline shape is exactly the full cube.
    pub fn is_shape_full_block(self) -> bool {
        self.info().full_faces >> 12 & 1 == 1
    }

    /// Whether structure templates file this state with the full blocks,
    /// which they place first: a static collision shape that is the full
    /// cube.
    pub fn is_template_full_block(self) -> bool {
        self.info().full_faces >> 13 & 1 == 1
    }

    fn transform(self, index: usize) -> Self {
        match STATE_TRANSFORMS.binary_search_by_key(&self.id(), |t| t[0]) {
            Ok(i) => Self(STATE_TRANSFORMS[i][index] as _),
            Err(_) => Self(self.id() as u16),
        }
    }

    /// This state turned a quarter clockwise, seen from above.
    pub fn rotated_clockwise(self) -> Self {
        self.transform(1)
    }

    /// This state mirrored across the x axis (north and south swap).
    pub fn mirrored_left_right(self) -> Self {
        self.transform(2)
    }

    /// This state mirrored across the z axis (east and west swap).
    pub fn mirrored_front_back(self) -> Self {
        self.transform(3)
    }

    /// Whether placing a block here simply overwrites this state, as with
    /// air, water or short grass.
    pub fn is_replaceable(self) -> bool {
        self.info().flags & REPLACEABLE != 0
    }

    /// The legacy "solid" flag some blocks check for support, e.g. signs.
    pub fn is_solid(self) -> bool {
        self.info().flags & SOLID != 0
    }

    pub fn is_solid_render(self) -> bool {
        self.info().flags & SOLID_RENDER != 0
    }

    /// Whether the collision shape is exactly the full cube.
    pub fn is_collision_full_block(self) -> bool {
        self.info().flags & FULL_COLLISION != 0
    }

    pub fn is_liquid(self) -> bool {
        self.info().flags & LIQUID != 0
    }

    /// Whether the bottom of this state's collision shape covers the post of
    /// a wall below it, or with `Some(side)` that side's wall section, which
    /// makes the wall below tall.
    pub fn covers_wall(self, side: Option<Facing>) -> bool {
        let bit = match side {
            None => 0,
            Some(Facing::North) => 1,
            Some(Facing::East) => 2,
            Some(Facing::South) => 3,
            Some(Facing::West) => 4,
            Some(Facing::Up | Facing::Down) => return false,
        };
        self.info().flags & COVERS_WALL_POST << bit != 0
    }

    /// Collision boxes as `[min_x, min_y, min_z, max_x, max_y, max_z]`
    /// relative to the block's corner.
    pub fn collision_boxes(self) -> &'static [[f32; 6]] {
        SHAPES[self.info().shape as usize]
    }

    /// Outline boxes (what the cursor targets and rays hit), laid out like
    /// [`Self::collision_boxes`].
    pub fn outline_boxes(self) -> &'static [[f32; 6]] {
        SHAPES[self.info().outline as usize]
    }

    pub fn sound_type(self) -> &'static SoundType {
        &SOUNDS[self.info().sound as usize]
    }

    /// The sound, volume and pitch of placing this state.
    pub fn place_sound(self) -> (SoundEvent, f32, f32) {
        let sound = self.sound_type();
        (sound.place_sound, sound.volume, sound.pitch)
    }

    /// Whether a fluid flowing or poured in simply replaces this state
    /// (`lava` for lava, water otherwise).
    pub fn is_replaceable_by_fluid(self, lava: bool) -> bool {
        let flag = if lava {
            LAVA_REPLACEABLE
        } else {
            WATER_REPLACEABLE
        };
        self.info().flags & flag != 0
    }

    /// Whether a player standing in or on this state gets hurt (fire,
    /// lava, magma, cactus, ...).
    pub fn is_dangerous(self) -> bool {
        self.info().flags & DANGEROUS != 0
    }

    /// Whether an entity whose head is inside this state suffocates, which
    /// also makes a bed under it obstructed.
    pub fn is_suffocating(self) -> bool {
        self.info().flags & SUFFOCATING != 0
    }

    /// Whether a player may respawn standing in this state at a forced
    /// spawn point.
    pub fn is_possible_to_respawn_in(self) -> bool {
        self.info().flags & RESPAWNABLE != 0
    }

    /// `(name, value)` pairs, ordered by name.
    pub fn properties(self) -> impl ExactSizeIterator<Item = (&'static str, &'static str)> {
        self.slots()
            .map(|slot| (slot.def.name, slot.def.values[slot.index]))
    }

    /// The value of the property called `name`, as it appears in commands and
    /// world files.
    pub fn get(self, name: &str) -> Option<&'static str> {
        self.slots()
            .find(|slot| slot.def.name == name)
            .map(|slot| slot.def.values[slot.index])
    }

    /// Every value of the property called `name`, in declaration order.
    pub fn property_values(self, name: &str) -> Option<&'static [&'static str]> {
        self.slots()
            .find(|slot| slot.def.name == name)
            .map(|slot| slot.def.values)
    }

    /// This state with the property called `name` set to `value`, or `None` if
    /// the block has no such property or value.
    pub fn with(self, name: &str, value: &str) -> Option<Self> {
        let slot = self.slots().find(|slot| slot.def.name == name)?;
        let index = slot.def.values.iter().position(|v| *v == value)?;
        Some(self.replace(&slot, index))
    }

    fn get_raw(self, property: Property) -> Option<u8> {
        self.slots()
            .find(|slot| slot.def.property == property)
            .map(|slot| slot.def.raw[slot.index])
    }

    fn with_raw(self, property: Property, raw: u8) -> Option<Self> {
        let slot = self.slots().find(|slot| slot.def.property == property)?;
        let index = slot.def.raw.iter().position(|r| *r == raw)?;
        Some(self.replace(&slot, index))
    }

    fn replace(self, slot: &Slot, index: usize) -> Self {
        let id = self.id() as usize - slot.index * slot.stride + index * slot.stride;
        Self(id as u16)
    }

    fn slots(self) -> impl ExactSizeIterator<Item = Slot> {
        let block = self.block() as usize;
        let defs = BLOCK_PROPERTIES[block];
        let offset = (self.id() as u16 - BLOCK_FIRST_STATE[block]) as usize;
        defs.iter().enumerate().map(move |(i, def)| {
            let stride: usize = defs[i + 1..].iter().map(|d| d.values.len()).product();
            Slot {
                def,
                index: offset / stride % def.values.len(),
                stride,
            }
        })
    }
}

impl Block {
    pub const fn kind(self) -> BlockKind {
        BLOCK_KIND[self as usize]
    }

    /// The item that places this block, if any.
    pub const fn item(self) -> Option<Item> {
        BLOCK_ITEMS[self as usize]
    }

    /// The item a player picks (middle-click) or that represents this block:
    /// its own item, or the standing item of a wall variant.
    pub fn pick_item(self) -> Option<Item> {
        self.item().or_else(|| {
            ITEM_BLOCKS
                .iter()
                .zip(Item::ALL)
                .find(|(blocks, _)| matches!(blocks, Some((_, Some((wall, _)))) if *wall == self))
                .map(|(_, item)| item)
        })
    }

    /// The state properties a picked stack of this block carries in its
    /// `block_state` component: always, or only when block data is copied.
    pub fn pick_state(self) -> Option<PickState> {
        block_lookup(&PICK_STATES, self)
    }

    /// The loot table this block drops from, if it drops anything. Wall
    /// variants share their standing block's table (`blocks/torch` for
    /// `wall_torch`).
    pub const fn loot_table(self) -> Option<&'static str> {
        BLOCK_LOOT_TABLE[self as usize]
    }

    /// How a piston moving the block treats it.
    pub const fn push_reaction(self) -> PushReaction {
        PUSH_REACTION[self as usize]
    }

    /// Whether the block falls when nothing holds it up.
    pub const fn is_falling(self) -> bool {
        FALLING[self as usize]
    }

    /// Ticks after placement or a support-losing update before a falling
    /// block checks whether it should start falling. Meaningless unless
    /// [`Self::is_falling`].
    pub const fn falling_delay(self) -> u8 {
        FALLING_DELAY[self as usize]
    }

    /// Whether this block's block entity is a container that can be filled
    /// from a loot table (chests, barrels, ...).
    pub const fn has_loot_container(self) -> bool {
        matches!(BLOCK_ENTITY[self as usize], Some((_, true)))
    }

    /// The experience the block drops when mined, if any.
    pub const fn experience(self) -> Option<BlockExperience> {
        BLOCK_EXPERIENCE[self as usize]
    }

    pub fn explosion_resistance(self) -> f32 {
        BLOCK_EXPLOSION_RESISTANCE[self as usize]
    }

    /// The note block instrument this block gives a note block above it
    /// (or, for mob heads, below it).
    pub const fn instrument(self) -> Instrument {
        BLOCK_INSTRUMENT[self as usize]
    }

    /// Doors, trapdoors and fence gates.
    pub fn toggle(self) -> Option<Toggle> {
        block_lookup(&TOGGLES, self)
    }

    pub fn button(self) -> Option<Button> {
        block_lookup(&BUTTONS, self)
    }

    /// How the game shifts this block's shapes by position, if it shifts
    /// any.
    pub fn shape_offset(self) -> Option<ShapeOffset> {
        block_lookup(&SHAPE_OFFSETS, self)
    }

    /// The damage a lit campfire deals to living things inside it.
    pub fn campfire_damage(self) -> Option<f32> {
        block_lookup(&CAMPFIRE_DAMAGE, self)
    }

    /// The block an infested block imitates (stone for infested stone).
    pub fn infested_host(self) -> Option<Block> {
        block_lookup(&INFESTED_HOSTS, self)
    }

    /// The block a dirt path or farmland turns back into when covered.
    pub fn base_block(self) -> Option<Block> {
        block_lookup(&BASE_BLOCKS, self)
    }

    /// The infested block that imitates this one, if silverfish can hide
    /// in it.
    pub fn infested_by_host(self) -> Option<Block> {
        INFESTED_HOSTS
            .iter()
            .find(|(_, host)| *host == self)
            .map(|(infested, _)| *infested)
    }

    /// What a bucket takes from this block.
    pub fn bucket_pickup(self) -> Option<BucketPickup> {
        block_lookup(&PICKUPS, self)
    }

    pub fn bed(self) -> Option<Bed> {
        block_lookup(&BEDS, self)
    }

    /// For plants: whether `ground` below can hold this block.
    pub fn may_place_on(self, ground: BlockState) -> bool {
        let set = BLOCK_GROUND[self as usize];
        let id = ground.id() as usize;
        set != u8::MAX && GROUNDS[set as usize][id / 64] >> (id % 64) & 1 == 1
    }

    pub const fn default_state(self) -> BlockState {
        BlockState(BLOCK_DEFAULT_STATE[self as usize])
    }

    pub fn states(self) -> impl DoubleEndedIterator<Item = BlockState> + ExactSizeIterator {
        let block = self as usize;
        (BLOCK_FIRST_STATE[block]..BLOCK_FIRST_STATE[block + 1]).map(BlockState)
    }

    /// The default state with each `(name, value)` property applied, or `None`
    /// if any of them is invalid for this block.
    pub fn state_with<'a>(
        self,
        properties: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Option<BlockState> {
        properties
            .into_iter()
            .try_fold(self.default_state(), |state, (name, value)| {
                state.with(name, value)
            })
    }
}

impl Item {
    /// The block this item places.
    pub const fn block(self) -> Option<Block> {
        match ITEM_BLOCKS[self as usize] {
            Some((block, _)) => Some(block),
            None => None,
        }
    }

    /// For items like torches and signs: the block placed against a wall, and
    /// the direction the standing block attaches in.
    pub const fn wall_block(self) -> Option<(Block, Facing)> {
        match ITEM_BLOCKS[self as usize] {
            Some((_, wall)) => wall,
            None => None,
        }
    }
}

impl Default for BlockState {
    fn default() -> Self {
        Block::default().default_state()
    }
}

impl From<Block> for BlockState {
    fn from(block: Block) -> Self {
        block.default_state()
    }
}

impl From<BlockState> for Block {
    fn from(state: BlockState) -> Self {
        state.block()
    }
}

/// Formats as `minecraft:oak_stairs[facing=east,half=bottom,...]`, the syntax
/// used by commands.
impl fmt::Display for BlockState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.block().name())?;
        for (i, (name, value)) in self.properties().enumerate() {
            f.write_str(if i == 0 { "[" } else { "," })?;
            write!(f, "{name}={value}")?;
        }
        if self.properties().len() > 0 {
            f.write_str("]")?;
        }
        Ok(())
    }
}

impl fmt::Debug for BlockState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlockState({}: {self})", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseBlockStateError(String);

impl fmt::Display for ParseBlockStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid block state `{}`", self.0)
    }
}

impl std::error::Error for ParseBlockStateError {}

/// Parses the [`Display`](fmt::Display) syntax. Unspecified properties keep
/// their default values.
impl FromStr for BlockState {
    type Err = ParseBlockStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let error = || ParseBlockStateError(s.to_owned());
        let (name, properties) = match s.split_once('[') {
            Some((name, rest)) => (name, rest.strip_suffix(']').ok_or_else(error)?),
            None => (s, ""),
        };
        let block = Block::from_name(name).ok_or_else(error)?;
        let pairs = properties
            .split(',')
            .filter(|pair| !pair.is_empty())
            .map(|pair| pair.split_once('=').ok_or_else(error))
            .collect::<Result<Vec<_>, _>>()?;
        block.state_with(pairs).ok_or_else(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Bucket;
    use crate::registry::SoundEvent;

    #[test]
    fn state_blocks_match_the_block_ranges() {
        for block in Block::ALL {
            let first = BLOCK_FIRST_STATE[block as usize] as usize;
            let end = BLOCK_FIRST_STATE[block as usize + 1] as usize;
            assert!(STATE_BLOCK[first..end].iter().all(|&owner| owner == block));
        }
    }

    #[test]
    fn state_flags_match_the_predicates_for_every_state() {
        for id in 0..BlockState::COUNT as u32 {
            let state = BlockState::from_id(id).unwrap();
            let flags = state.flags();
            let air = matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir);
            let waterlogged = state.get("waterlogged") == Some("true");
            let surface = matches!(state.block(), Block::Water | Block::Lava) || waterlogged;
            let height = surface || state.block() == Block::BubbleColumn;
            let ticks = state.is_randomly_ticking() || state.is_fluid_randomly_ticking();
            assert_eq!(flags & BlockState::FLAG_NOT_AIR != 0, !air, "{state:?}");
            assert_eq!(
                flags & BlockState::FLAG_SURFACE_FLUID != 0,
                surface,
                "{state:?}"
            );
            assert_eq!(
                flags & BlockState::FLAG_HEIGHT_FLUID != 0,
                height,
                "{state:?}"
            );
            assert_eq!(
                flags & BlockState::FLAG_FLUID != 0,
                !state.fluid_state().is_empty(),
                "{state:?}"
            );
            assert_eq!(
                flags & BlockState::FLAG_TICKS_RANDOMLY != 0,
                ticks,
                "{state:?}"
            );
            assert_eq!(flags & !31, 0);
        }
    }

    #[test]
    fn shape_offsets_match_the_game_and_shapes_are_stored_unshifted() {
        for (block, x, z, offset) in SHAPE_OFFSET_SAMPLES {
            assert_eq!(
                block.shape_offset().unwrap().at(x, z),
                offset,
                "{block:?} at {x},{z}"
            );
        }
        let bamboo = Block::Bamboo.shape_offset().unwrap();
        assert!(bamboo.collision && bamboo.outline);
        assert_eq!(bamboo.horizontal, 0.25);
        let stalk = Block::Bamboo.default_state().collision_boxes();
        assert_eq!(
            stalk,
            &[[6.5 / 16.0, 0.0, 6.5 / 16.0, 9.5 / 16.0, 1.0, 9.5 / 16.0]]
        );
        assert!(
            Block::BambooSapling
                .default_state()
                .collision_boxes()
                .is_empty()
        );
        assert_eq!(
            Block::PointedDripstone.shape_offset().unwrap().horizontal,
            0.125
        );
        assert_eq!(Block::Stone.shape_offset(), None);
    }

    #[test]
    fn blocks_carry_their_sound_types_and_interactions() {
        let stone = Block::Stone.default_state().sound_type();
        assert_eq!(stone.break_sound, SoundEvent::BlockStoneBreak);
        assert_eq!(stone.hit_sound, SoundEvent::BlockStoneHit);
        let door = Block::OakDoor.toggle().unwrap();
        assert_eq!(door.open_sound, SoundEvent::BlockWoodenDoorOpen);
        assert!(door.opens_by_hand);
        assert!(!Block::IronDoor.toggle().unwrap().opens_by_hand);
        assert_eq!(
            Block::OakFenceGate.toggle().unwrap().close_sound,
            SoundEvent::BlockFenceGateClose
        );
        assert_eq!(Block::OakButton.button().unwrap().press_ticks, 30);
        assert_eq!(Block::StoneButton.button().unwrap().press_ticks, 20);
        assert!(Block::Stone.toggle().is_none());
        assert_eq!(Block::Stone.instrument(), Instrument::Basedrum);
        assert_eq!(Block::GoldBlock.instrument(), Instrument::Bell);
        assert!(Instrument::Zombie.works_above_note_block());
        assert!(!Instrument::Zombie.is_tunable());
        assert_eq!(Block::Obsidian.explosion_resistance(), 1200.0);
        assert!(Block::Lava.default_state().is_dangerous());
        assert!(!Block::Stone.default_state().is_dangerous());
        assert!(Block::Stone.default_state().is_suffocating());
        assert!(!Block::Glass.default_state().is_suffocating());
        assert!(Block::Air.default_state().is_possible_to_respawn_in());
        assert!(
            Block::OakPressurePlate
                .default_state()
                .is_possible_to_respawn_in()
        );
        assert!(!Block::Water.default_state().is_possible_to_respawn_in());
    }

    #[test]
    fn falling_blocks_and_their_placement_delay_match_the_jar() {
        for block in [Block::Sand, Block::Gravel, Block::Anvil, Block::DragonEgg] {
            assert!(block.is_falling(), "{block:?} should fall");
        }
        assert_eq!(Block::Sand.falling_delay(), 2);
        assert_eq!(Block::Gravel.falling_delay(), 2);
        assert_eq!(Block::Anvil.falling_delay(), 2);
        assert_eq!(Block::DragonEgg.falling_delay(), 5);
        // Scaffolding and pointed dripstone fall through their own bespoke
        // logic rather than the generic falling-block behavior.
        assert!(!Block::Scaffolding.is_falling());
        assert!(!Block::PointedDripstone.is_falling());
    }

    #[test]
    fn loot_container_blocks_agree_with_block_entity_types() {
        for block in Block::ALL {
            assert_eq!(
                BLOCK_ENTITY[block as usize].map(|(kind, _)| kind),
                block.block_entity().map(|kind| kind.name()),
                "{block:?}"
            );
        }
        assert!(Block::Chest.has_loot_container());
        assert!(Block::Barrel.has_loot_container());
        assert!(!Block::Furnace.has_loot_container());
    }

    #[test]
    fn buckets_waxing_and_beds_come_from_game_code() {
        let water = Block::Water.bucket_pickup().unwrap();
        assert_eq!(
            (water.kind, water.item),
            (PickupKind::Liquid, Item::WaterBucket)
        );
        assert_eq!(water.sound, Some(SoundEvent::ItemBucketFill));
        assert_eq!(
            Block::OakStairs.bucket_pickup().unwrap().kind,
            PickupKind::Waterlogged
        );
        assert_eq!(
            Block::PowderSnow.bucket_pickup().unwrap().item,
            Item::PowderSnowBucket
        );
        assert_eq!(
            Item::Bucket.bucket(),
            Some(Bucket::Fluid {
                fluid: None,
                mob: None
            })
        );
        assert!(matches!(
            Item::PowderSnowBucket.bucket(),
            Some(Bucket::Solid {
                block: Block::PowderSnow,
                ..
            })
        ));
        assert_eq!(Item::MilkBucket.use_remainder(), Some(Item::Bucket));
        assert_eq!(Block::CopperBlock.waxed(), Some(Block::WaxedCopperBlock));
        assert_eq!(Block::WaxedCopperDoor.unwaxed(), Some(Block::CopperDoor));
        assert!(Block::RedBed.bed().unwrap().explodes);
        assert_eq!(
            Block::StrawBed.bed().unwrap().rule,
            BedRuleAttribute::StrawBed
        );
        let slab = Block::OakSlab.default_state();
        assert!(!slab.outline_boxes().is_empty());
        assert!(Block::Torch.default_state().collision_boxes().is_empty());
        assert!(!Block::Torch.default_state().outline_boxes().is_empty());
    }
}
