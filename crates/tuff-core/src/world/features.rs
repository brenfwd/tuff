//! World generation features grown in a live world (saplings, bone meal,
//! multiface spread, sculk). Core only names what block behaviours need;
//! whoever owns the generators fills in the [`FeatureHost`].

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use tuff_item::random::WorldgenRandom;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::ChunkPos;

/// A growing sculk charge.
#[derive(Clone, Debug, PartialEq)]
pub struct SculkCursor {
    pub pos: BlockPos,
    pub charge: i32,
    pub update_delay: i32,
    pub decay_delay: i32,
    pub facings: Option<Vec<Facing>>,
}

impl SculkCursor {
    pub fn new(pos: BlockPos, charge: i32) -> Self {
        Self {
            pos,
            charge,
            update_delay: 0,
            decay_delay: 1,
            facings: None,
        }
    }

    pub fn is_unreasonable(&self, origin: BlockPos) -> bool {
        let d = (self.pos.x - origin.x)
            .abs()
            .max((self.pos.y - origin.y).abs())
            .max((self.pos.z - origin.z).abs());
        d > 1024
    }
}

/// What a cursor did that a live world should also hear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SculkEvent {
    Spread(BlockPos),
    Grew(BlockPos, Block),
}

/// The blocks of a feature window, as features read and write them.
pub trait FeatureRegion {
    fn block(&self, pos: BlockPos) -> BlockState;
    fn set_block(&mut self, pos: BlockPos, state: BlockState) -> bool;
    fn biome(&mut self, pos: BlockPos) -> u16;
    fn is_outside_build_height(&self, y: i32) -> bool;
    /// Places a configured feature by name.
    fn place_configured(&mut self, name: &str, random: &mut WorldgenRandom, pos: BlockPos) -> bool;
    /// Places a placed feature by name, through its placement modifiers.
    fn place_placed(&mut self, name: &str, random: &mut WorldgenRandom, pos: BlockPos) -> bool;
    /// Whether the multiface block `state` at `pos` can spread in any
    /// direction from `face`.
    fn multiface_can_spread(&self, state: BlockState, pos: BlockPos, face: Facing) -> bool;
    /// Spreads the multiface block `state` at `pos` from a random face.
    fn multiface_spread(&mut self, random: &mut WorldgenRandom, state: BlockState, pos: BlockPos);
    /// Updates sculk cursors, appending what a live world should hear.
    fn sculk_update(
        &mut self,
        cursors: &mut [SculkCursor],
        origin: BlockPos,
        random: &mut WorldgenRandom,
        events: &mut Vec<SculkEvent>,
    );
}

/// What a feature run changed, in chunk-window order.
#[derive(Default)]
pub struct FeatureChanges {
    pub blocks: Vec<(BlockPos, BlockState)>,
    pub block_entities: Vec<(BlockPos, tuff_nbt::Compound)>,
    /// Entities the run added, as saved data with `id` and `Pos`.
    pub entities: Vec<tuff_nbt::Compound>,
}

/// The shape and seeds of the world a feature window was cut from.
#[derive(Clone, Copy)]
pub struct FeatureSpan {
    pub min_y: i32,
    pub height: i32,
    pub sea_level: i32,
    pub seed: i64,
}

pub trait FeatureHost: Send + Sync {
    /// Runs `grow` on a copy of the chunk window around `center`; if it
    /// returns true, reports what it changed against the originals.
    fn with_window(
        &self,
        center: ChunkPos,
        window: &[(ChunkPos, Chunk)],
        span: FeatureSpan,
        random: &mut WorldgenRandom,
        grow: &mut dyn FnMut(&mut dyn FeatureRegion, &mut WorldgenRandom) -> bool,
    ) -> Option<FeatureChanges>;

    /// Runs a `/place` request on a copy of the window around `center`,
    /// reporting what it changed; `None` if nothing was placed.
    fn place(
        &self,
        request: &PlaceRequest,
        center: ChunkPos,
        window: &[(ChunkPos, Chunk)],
        span: FeatureSpan,
        random: &mut WorldgenRandom,
    ) -> Option<FeatureChanges>;

    /// The chunks the template of `request` reaches; `None` if the world
    /// has no such template.
    fn template_extent(&self, request: &TemplateRequest) -> Option<TemplateExtent>;

    /// The size of the template `name`, `None` where nothing provides it.
    fn template_size(&self, _name: &str) -> Option<[i32; 3]> {
        None
    }

    /// Makes `area` the template `name`, written to the world's generated
    /// folder when `to_disk`, otherwise kept in memory until forgotten.
    fn save_template(&self, _name: &str, _area: CapturedArea, _to_disk: bool) -> bool {
        false
    }

    /// Drops the in-memory copy of the template `name`; the next lookup
    /// reads it from its source again.
    fn forget_template(&self, _name: &str) {}

    /// The biome zoom seed derived from a world seed.
    fn obfuscate_seed(&self, seed: i64) -> i64;

    /// The quart the game's biome jitter picks for a block, given the
    /// obfuscated seed.
    fn zoom(&self, obfuscated_seed: i64, x: i32, y: i32, z: i32) -> [i32; 3];

    /// The temperature at a block of a biome with the given base.
    fn temperature(&self, base: f32, frozen: bool, x: i32, y: i32, z: i32, sea_level: i32) -> f32;
}

/// What `/place` asks a feature window to place.
#[derive(Clone, Debug)]
pub enum PlaceRequest {
    /// A configured feature: a registered name or an inline definition.
    Feature {
        feature: FeatureRef,
        pos: BlockPos,
    },
    Template(TemplateRequest),
}

#[derive(Clone, Debug)]
pub enum FeatureRef {
    Named(String),
    Inline(serde_json::Value),
}

/// A structure template placed the way `/place template` places it.
#[derive(Clone, Debug)]
pub struct TemplateRequest {
    pub name: String,
    pub pos: BlockPos,
    /// Quarter turns clockwise.
    pub rotation: u8,
    /// 0 none, 1 left-right, 2 front-back.
    pub mirror: u8,
    pub integrity: f32,
    /// 0 picks a fresh random seed.
    pub seed: i64,
    pub strict: bool,
    pub ignore_entities: bool,
}

/// An area read for a template: its size, its blocks (positions relative
/// to its corner, block entity data beside them) and its entities (exact
/// and block positions relative to the corner, saved data).
#[derive(Clone, Debug, Default)]
pub struct CapturedArea {
    pub size: [i32; 3],
    pub blocks: Vec<(BlockPos, BlockState, Option<tuff_nbt::Compound>)>,
    pub entities: Vec<([f64; 3], BlockPos, tuff_nbt::Compound)>,
}

/// The chunks a template reaches: the ones vanilla requires loaded
/// (from the position to the position plus the template's size) and the
/// ones its turned and mirrored box covers.
#[derive(Clone, Copy, Debug)]
pub struct TemplateExtent {
    pub declared: (ChunkPos, ChunkPos),
    pub placed: (ChunkPos, ChunkPos),
}

/// A structure start or jigsaw assembly ready to be placed chunk by chunk.
pub trait PlannedStructure: Send + Sync {
    /// The chunks its bounding box covers, both corners inclusive.
    fn chunks(&self) -> (ChunkPos, ChunkPos);

    /// Places the part inside the centre chunk of `window`, reporting what
    /// changed against the originals.
    fn place_in_window(
        &self,
        center: ChunkPos,
        window: &[(ChunkPos, Chunk)],
        span: FeatureSpan,
        random: &mut WorldgenRandom,
    ) -> Option<FeatureChanges>;
}
