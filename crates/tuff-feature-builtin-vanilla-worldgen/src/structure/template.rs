//! Structure templates: the `.nbt` files structures and some features are
//! built from, read from the jar data the build embeds, and placed with a
//! rotation, a mirror and a list of processors.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::{Fluid, fluid};
use crate::feature::tree::{Voxels, update_edges};
use crate::structure::geometry::{BoundingBox, Mirror, Rotation, transform};
use crate::structure::processor::ProcessorList;
use tuff_core::world::features::CapturedArea;
use tuff_item::random::{LegacyRandom, RandomSource, position_seed};
use tuff_nbt::{Compound, NamedTag, NbtLimits, Tag};
use tuff_vanilla_common::placement::update_from_neighbours;
use tuff_world_model::BlockPos;

/// Decompressed templates, and the memory their tags take, may not exceed
/// this. The largest vanilla template is about 1 MiB of NBT, well within
/// it.
pub const MAX_TEMPLATE_BYTES: usize = 32 << 20;
/// Templates are flat; nothing legitimate nests deeper.
pub const MAX_TEMPLATE_DEPTH: usize = 64;

/// One block of a template, or of a template being placed.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockInfo {
    pub pos: BlockPos,
    pub state: BlockState,
    /// Boxed: few blocks carry data, and templates hold millions of blocks.
    pub nbt: Option<Box<Compound>>,
}

#[derive(Debug, Default)]
pub struct Template {
    pub size: [i32; 3],
    /// Each palette's blocks: full blocks, then the rest, then block
    /// entities, each sorted by y, x, z.
    palettes: Vec<Vec<BlockInfo>>,
    /// Each palette's jigsaw blocks, in palette order.
    jigsaws: Vec<Vec<JigsawBlock>>,
    pub has_entities: bool,
    /// The entities it places (villagers, bastion piglins).
    entities: Vec<TemplateEntity>,
}

/// An entity a template places: where it stands, the block it stands in,
/// and its saved data.
#[derive(Debug, Clone)]
struct TemplateEntity {
    pos: [f64; 3],
    block_pos: BlockPos,
    nbt: Compound,
}

pub use tuff_core::world::entity::FINALIZE_SPAWN;

/// A jigsaw block: where a piece connects to the next.
#[derive(Debug, Clone)]
pub struct JigsawBlock {
    pub pos: BlockPos,
    pub state: BlockState,
    pub rollable: bool,
    /// `None` only for the implicit jigsaw of a feature element, which
    /// accepts any target.
    pub name: Option<Arc<str>>,
    pub pool: Arc<str>,
    pub target: Arc<str>,
    pub placement_priority: i32,
    pub selection_priority: i32,
}

impl JigsawBlock {
    fn parse(info: &BlockInfo) -> Self {
        let empty = || Arc::<str>::from("minecraft:empty");
        let id = |key: &str| {
            info.nbt
                .as_ref()
                .and_then(|n| n.get_str(key))
                .map_or_else(empty, |s| Arc::from(crate::json::qualify(s)))
        };
        let int = |key: &str| info.nbt.as_ref().and_then(|n| n.get_int(key)).unwrap_or(0);
        let joint = info.nbt.as_ref().and_then(|n| n.get_str("joint"));
        let rollable = match joint {
            Some("rollable") => true,
            Some("aligned") => false,
            _ => !matches!(
                orientation(info.state).0,
                Facing::North | Facing::South | Facing::East | Facing::West
            ),
        };
        Self {
            pos: info.pos,
            state: info.state,
            rollable,
            name: Some(id("name")),
            pool: id("pool"),
            target: id("target"),
            placement_priority: int("placement_priority"),
            selection_priority: int("selection_priority"),
        }
    }

    pub fn front(&self) -> Facing {
        orientation(self.state).0
    }

    pub fn top(&self) -> Facing {
        orientation(self.state).1
    }

    /// Whether `target` can join this jigsaw.
    pub fn can_attach(&self, target: &JigsawBlock) -> bool {
        let (front, top) = orientation(self.state);
        let (target_front, target_top) = orientation(target.state);
        front == target_front.opposite()
            && (self.rollable || top == target_top)
            && target.name.as_ref().is_none_or(|name| *name == self.target)
    }
}

/// A jigsaw state's front and top directions.
pub fn orientation(state: BlockState) -> (Facing, Facing) {
    let value = state.get("orientation").unwrap_or("north_up");
    let (front, top) = value.split_once('_').unwrap_or(("north", "up"));
    (facing(front), facing(top))
}

fn facing(name: &str) -> Facing {
    match name {
        "down" => Facing::Down,
        "up" => Facing::Up,
        "south" => Facing::South,
        "west" => Facing::West,
        "east" => Facing::East,
        _ => Facing::North,
    }
}

/// Where a template's random draws come from. Loot seeds always come from
/// the placer's random; the palette choice and processors draw from it too
/// when it is shared (features), else from a random of their own per
/// position (structure pieces).
pub struct PlaceRandom<'a> {
    shared: bool,
    random: &'a mut dyn RandomSource,
}

impl<'a> PlaceRandom<'a> {
    pub fn positional(random: &'a mut dyn RandomSource) -> Self {
        Self {
            shared: false,
            random,
        }
    }

    pub fn shared(random: &'a mut dyn RandomSource) -> Self {
        Self {
            shared: true,
            random,
        }
    }

    pub fn with<R>(&mut self, pos: BlockPos, f: impl FnOnce(&mut dyn RandomSource) -> R) -> R {
        if self.shared {
            f(&mut *self.random)
        } else {
            let mut random = LegacyRandom::new(position_seed(pos.x, pos.y, pos.z));
            f(&mut random)
        }
    }

    pub fn loot(&mut self) -> &mut dyn RandomSource {
        &mut *self.random
    }
}

#[derive(Clone)]
pub struct PlaceSettings {
    pub mirror: Mirror,
    pub rotation: Rotation,
    pub pivot: BlockPos,
    /// Only blocks inside are placed (the chunk being decorated).
    pub bounding_box: Option<BoundingBox>,
    pub processors: ProcessorList,
    pub known_shape: bool,
    pub apply_waterlogging: bool,
    /// Whether placed blocks are marked for the chunk's post-processing
    /// (jigsaw pieces place without it).
    pub post_process: bool,
    /// Leaves the template's entities out (mansions, end cities).
    pub ignore_entities: bool,
    /// Sets placed mobs up as fresh structure spawns (jigsaw pieces).
    pub finalize_entities: bool,
}

impl Default for PlaceSettings {
    fn default() -> Self {
        Self {
            mirror: Mirror::None,
            rotation: Rotation::None,
            pivot: BlockPos::new(0, 0, 0),
            bounding_box: None,
            processors: ProcessorList::default(),
            known_shape: false,
            apply_waterlogging: true,
            post_process: true,
            ignore_entities: false,
            finalize_entities: false,
        }
    }
}

impl PlaceSettings {
    pub fn relative(&self, pos: BlockPos) -> BlockPos {
        transform(pos, self.mirror, self.rotation, self.pivot)
    }
}

fn int_list(tag: Option<&Tag>) -> [i32; 3] {
    let mut out = [0; 3];
    if let Some(Tag::List(list)) = tag {
        for (o, v) in out.iter_mut().zip(list) {
            *o = v.as_int().unwrap_or(0);
        }
    }
    out
}

fn palette_entry(state: BlockState) -> Tag {
    let mut entry = Compound::new().with("Name", state.block().name());
    if state.properties().len() > 0 {
        entry.insert("Properties", state.properties().collect::<Compound>());
    }
    Tag::Compound(entry)
}

fn sort_key(info: &BlockInfo) -> (i32, i32, i32) {
    (info.pos.y, info.pos.x, info.pos.z)
}

impl Template {
    /// Parses gzip-compressed template NBT, refusing anything that
    /// decompresses past [`MAX_TEMPLATE_BYTES`] or nests past
    /// [`MAX_TEMPLATE_DEPTH`].
    pub fn parse(data: &[u8]) -> Result<Template, String> {
        let limits = NbtLimits {
            max_depth: MAX_TEMPLATE_DEPTH,
            max_bytes: MAX_TEMPLATE_BYTES,
        };
        let (named, _) = NamedTag::from_bytes(data, limits).map_err(|e| e.to_string())?;
        let Tag::Compound(root) = named.tag else {
            return Err("template root is not a compound".into());
        };
        Self::from_nbt(&root)
    }

    pub fn from_nbt(root: &Compound) -> Result<Template, String> {
        let size = int_list(root.get("size"));
        let blocks = root.get_list("blocks").unwrap_or(&[]);
        let mut palettes = Vec::new();
        match root.get_list("palettes") {
            Some(lists) => {
                for list in lists {
                    palettes.push(Self::palette(list.as_list().unwrap_or(&[]), blocks)?);
                }
            }
            None => palettes.push(Self::palette(
                root.get_list("palette").unwrap_or(&[]),
                blocks,
            )?),
        }
        let entities: Vec<TemplateEntity> = root
            .get_list("entities")
            .unwrap_or(&[])
            .iter()
            .filter_map(Tag::as_compound)
            .filter_map(|entity| {
                let nbt = entity.get("nbt")?.as_compound()?.clone();
                let pos = match entity.get("pos") {
                    Some(Tag::List(list)) if list.len() == 3 => {
                        let mut pos = [0.0; 3];
                        for (p, v) in pos.iter_mut().zip(list) {
                            *p = match *v {
                                Tag::Double(v) => v,
                                Tag::Float(v) => v.into(),
                                _ => return None,
                            };
                        }
                        pos
                    }
                    _ => return None,
                };
                let [x, y, z] = int_list(entity.get("blockPos"));
                Some(TemplateEntity {
                    pos,
                    block_pos: BlockPos::new(x, y, z),
                    nbt,
                })
            })
            .collect();
        let has_entities = !entities.is_empty();
        let jigsaws = palettes
            .iter()
            .map(|palette| {
                palette
                    .iter()
                    .filter(|info| info.state.block() == Block::Jigsaw && info.nbt.is_some())
                    .map(JigsawBlock::parse)
                    .collect()
            })
            .collect();
        Ok(Template {
            size,
            palettes,
            jigsaws,
            has_entities,
            entities,
        })
    }

    fn palette(entries: &[Tag], blocks: &[Tag]) -> Result<Vec<BlockInfo>, String> {
        let states: Vec<BlockState> = entries
            .iter()
            .map(|tag| {
                let name = tag
                    .as_compound()
                    .and_then(|c| c.get_str("id").or_else(|| c.get_str("Name")))
                    .unwrap_or("");
                if Block::from_name(name).is_none() {
                    return Err(format!("unknown block {name:?} in template palette"));
                }
                Ok(tuff_core::storage::chunk_nbt::decode_block_state(tag))
            })
            .collect::<Result<_, _>>()?;
        let (mut full, mut other, mut entities) = (Vec::new(), Vec::new(), Vec::new());
        for block in blocks {
            let Some(block) = block.as_compound() else {
                continue;
            };
            let [x, y, z] = int_list(block.get("pos"));
            let index = block.get_int("state").unwrap_or(0);
            let state = usize::try_from(index)
                .ok()
                .and_then(|i| states.get(i))
                .copied()
                .unwrap_or(Block::Air.default_state());
            let info = BlockInfo {
                pos: BlockPos::new(x, y, z),
                state,
                nbt: block.get_compound("nbt").cloned().map(Box::new),
            };
            if info.nbt.is_some() {
                entities.push(info);
            } else if state.is_template_full_block() {
                full.push(info);
            } else {
                other.push(info);
            }
        }
        full.sort_by_key(sort_key);
        other.sort_by_key(sort_key);
        entities.sort_by_key(sort_key);
        full.extend(other);
        full.extend(entities);
        Ok(full)
    }

    /// The saved form of the blocks of an area `size` large, positions
    /// relative to its corner: the palette of first-seen states, then the
    /// blocks in the game's order (full blocks, the rest, block entities).
    /// Structure voids are left out.
    pub fn capture(area: CapturedArea) -> Compound {
        let CapturedArea {
            size,
            blocks,
            entities: captured_entities,
        } = area;
        let (mut full, mut other, mut entities) = (Vec::new(), Vec::new(), Vec::new());
        for (pos, state, nbt) in blocks {
            if state.block() == Block::StructureVoid {
                continue;
            }
            let nbt = nbt.map(|mut data| {
                for key in ["x", "y", "z"] {
                    data.remove(key);
                }
                Box::new(data)
            });
            let info = BlockInfo { pos, state, nbt };
            if info.nbt.is_some() {
                entities.push(info);
            } else if state.is_template_full_block() {
                full.push(info);
            } else {
                other.push(info);
            }
        }
        full.sort_by_key(sort_key);
        other.sort_by_key(sort_key);
        entities.sort_by_key(sort_key);
        let mut palette: Vec<BlockState> = Vec::new();
        let mut blocks_tag = Vec::new();
        for info in full.into_iter().chain(other).chain(entities) {
            let index = match palette.iter().position(|s| *s == info.state) {
                Some(index) => index,
                None => {
                    palette.push(info.state);
                    palette.len() - 1
                }
            };
            let mut block = Compound::new()
                .with(
                    "pos",
                    Tag::List(vec![
                        Tag::Int(info.pos.x),
                        Tag::Int(info.pos.y),
                        Tag::Int(info.pos.z),
                    ]),
                )
                .with("state", index as i32);
            if let Some(nbt) = info.nbt {
                block.insert("nbt", Tag::Compound(*nbt));
            }
            blocks_tag.push(Tag::Compound(block));
        }
        Compound::new()
            .with("size", Tag::List(size.map(Tag::Int).to_vec()))
            .with(
                "palette",
                Tag::List(palette.into_iter().map(palette_entry).collect()),
            )
            .with("blocks", Tag::List(blocks_tag))
            .with(
                "entities",
                Tag::List(
                    captured_entities
                        .into_iter()
                        .map(|(pos, block_pos, nbt)| {
                            Tag::Compound(
                                Compound::new()
                                    .with("pos", Tag::List(pos.map(Tag::Double).to_vec()))
                                    .with(
                                        "blockPos",
                                        Tag::List(vec![
                                            Tag::Int(block_pos.x),
                                            Tag::Int(block_pos.y),
                                            Tag::Int(block_pos.z),
                                        ]),
                                    )
                                    .with("nbt", Tag::Compound(nbt)),
                            )
                        })
                        .collect(),
                ),
            )
            .with("DataVersion", tuff_vanilla_data::version::DATA_VERSION)
    }

    pub fn size(&self, rotation: Rotation) -> [i32; 3] {
        let [x, y, z] = self.size;
        match rotation {
            Rotation::Clockwise90 | Rotation::CounterClockwise90 => [z, y, x],
            _ => [x, y, z],
        }
    }

    pub fn bounding_box(
        &self,
        position: BlockPos,
        rotation: Rotation,
        pivot: BlockPos,
        mirror: Mirror,
    ) -> BoundingBox {
        let [x, y, z] = self.size;
        let a = transform(BlockPos::new(0, 0, 0), mirror, rotation, pivot);
        let b = transform(BlockPos::new(x - 1, y - 1, z - 1), mirror, rotation, pivot);
        BoundingBox::from_corners(a, b).moved(position.x, position.y, position.z)
    }

    pub fn settings_box(&self, settings: &PlaceSettings, position: BlockPos) -> BoundingBox {
        self.bounding_box(position, settings.rotation, settings.pivot, settings.mirror)
    }

    /// The jigsaw blocks of the palette `position` picks, rotated and
    /// moved there.
    pub fn jigsaws(&self, position: BlockPos, rotation: Rotation) -> Vec<JigsawBlock> {
        if self.jigsaws.is_empty() {
            return Vec::new();
        }
        let mut random = LegacyRandom::new(position_seed(position.x, position.y, position.z));
        let index = random.next_int_bounded(self.jigsaws.len() as i32) as usize;
        let origin = BlockPos::new(0, 0, 0);
        self.jigsaws[index]
            .iter()
            .map(|jigsaw| {
                let pos = transform(jigsaw.pos, Mirror::None, rotation, origin);
                JigsawBlock {
                    pos: BlockPos::new(pos.x + position.x, pos.y + position.y, pos.z + position.z),
                    state: rotation.rotate_state(jigsaw.state),
                    ..jigsaw.clone()
                }
            })
            .collect()
    }

    fn palette_for(&self, random: &mut PlaceRandom, pos: BlockPos) -> &[BlockInfo] {
        let count = self.palettes.len() as i32;
        let index = random.with(pos, |r| r.next_int_bounded(count));
        &self.palettes[index as usize]
    }

    /// The blocks of one kind, placed and rotated but not processed, as
    /// the game finds data markers and jigsaws after placing.
    pub fn filter_blocks(
        &self,
        position: BlockPos,
        settings: &PlaceSettings,
        random: &mut PlaceRandom,
        block: Block,
    ) -> Vec<BlockInfo> {
        if self.palettes.is_empty() {
            return Vec::new();
        }
        self.palette_for(random, position)
            .iter()
            .filter(|info| info.state.block() == block)
            .filter_map(|info| {
                let pos = settings.relative(info.pos);
                let pos = BlockPos::new(pos.x + position.x, pos.y + position.y, pos.z + position.z);
                settings
                    .bounding_box
                    .is_none_or(|b| b.contains(pos))
                    .then(|| BlockInfo {
                        pos,
                        state: settings.rotation.rotate_state(info.state),
                        nbt: info.nbt.clone(),
                    })
            })
            .collect()
    }

    /// Places the template at `position`. False if there was nothing to
    /// place.
    pub fn place(
        &self,
        level: &mut Region,
        position: BlockPos,
        reference: BlockPos,
        settings: &PlaceSettings,
        place_random: &mut PlaceRandom,
    ) -> bool {
        if self.palettes.is_empty() {
            return false;
        }
        let blocks = self.palette_for(place_random, position);
        if (blocks.is_empty() && !self.has_entities)
            || self.size[0] < 1
            || self.size[1] < 1
            || self.size[2] < 1
        {
            return false;
        }
        let processed = process_blocks(level, position, reference, settings, blocks, place_random);
        let waterlog = settings.apply_waterlogging;
        let mut to_fill = Vec::new();
        let mut locked = Vec::new();
        let mut placed: Vec<(BlockPos, bool)> = Vec::with_capacity(processed.len());
        let (mut min, mut max) = ([i32::MAX; 3], [i32::MIN; 3]);
        for info in processed {
            let pos = info.pos;
            if settings.bounding_box.is_some_and(|b| !b.contains(pos)) {
                continue;
            }
            let previous = waterlog.then(|| fluid(level.block(pos)));
            let state = settings
                .rotation
                .rotate_state(settings.mirror.mirror_state(info.state));
            if info.nbt.is_some() {
                level.remove_block_entity(pos);
            }
            if !level.set_block_flags(pos, state, settings.post_process) {
                continue;
            }
            for (i, v) in [pos.x, pos.y, pos.z].into_iter().enumerate() {
                min[i] = min[i].min(v);
                max[i] = max[i].max(v);
            }
            placed.push((pos, info.nbt.is_some()));
            if let Some(mut nbt) = info.nbt
                && state.block().block_entity().is_some()
            {
                if state.block().has_loot_container() {
                    nbt.insert("LootTableSeed", place_random.loot().next_long());
                }
                store_block_entity(level, pos, state, *nbt);
            }
            if let Some(previous) = previous {
                if is_source(fluid(state)) {
                    locked.push(pos);
                } else if is_liquid_container(state) {
                    place_liquid(level, pos, state, previous);
                    if !is_source(previous) {
                        to_fill.push(pos);
                    }
                }
            }
        }
        fill_liquids(level, &mut to_fill, &locked);
        if min[0] <= max[0] && !settings.known_shape {
            let size = [
                max[0] - min[0] + 1,
                max[1] - min[1] + 1,
                max[2] - min[2] + 1,
            ];
            let mut shape = Voxels::with_size(size);
            for (pos, _) in &placed {
                shape.fill([pos.x - min[0], pos.y - min[1], pos.z - min[2]]);
            }
            update_edges(level, &shape, BlockPos::new(min[0], min[1], min[2]));
            for (pos, _) in &placed {
                let state = level.block(*pos);
                let updated = update_from_neighbours(state, *pos, &|p| level.block(p));
                if updated != state {
                    level.set_block_flags(*pos, updated, false);
                }
            }
        }
        if !settings.ignore_entities {
            self.place_entities(level, position, settings);
        }
        true
    }

    /// Adds the template's entities inside the settings' box: placed,
    /// turned and mirrored with the template, with fresh UUIDs.
    fn place_entities(&self, level: &mut Region, position: BlockPos, settings: &PlaceSettings) {
        for entity in &self.entities {
            let relative = settings.relative(entity.block_pos);
            let block_pos = BlockPos::new(
                relative.x + position.x,
                relative.y + position.y,
                relative.z + position.z,
            );
            if settings
                .bounding_box
                .is_some_and(|b| !b.contains(block_pos))
            {
                continue;
            }
            let [x, y, z] = transform_vec(entity.pos, settings);
            let pos = [
                x + f64::from(position.x),
                y + f64::from(position.y),
                z + f64::from(position.z),
            ];
            let mut nbt = entity.nbt.clone();
            nbt.insert("Pos", Tag::List(pos.map(Tag::Double).to_vec()));
            nbt.remove("UUID");
            if nbt.get("block_pos").is_some() {
                nbt.insert(
                    "block_pos",
                    Tag::IntArray(vec![block_pos.x, block_pos.y, block_pos.z]),
                );
            }
            let (yaw, pitch) = match nbt.get("Rotation") {
                Some(Tag::List(list)) if list.len() == 2 => (
                    list[0].as_float().unwrap_or(0.0),
                    list[1].as_float().unwrap_or(0.0),
                ),
                _ => (0.0, 0.0),
            };
            let yaw = rotated_yaw(yaw, settings);
            nbt.insert(
                "Rotation",
                Tag::List(vec![Tag::Float(yaw), Tag::Float(pitch)]),
            );
            if settings.finalize_entities {
                nbt.insert(FINALIZE_SPAWN, true);
            }
            level.add_entity(block_pos, nbt);
        }
    }
}

fn wrap_degrees(degrees: f32) -> f32 {
    let mut wrapped = degrees % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    wrapped
}

/// An entity's yaw once the template is turned and mirrored.
fn rotated_yaw(yaw: f32, settings: &PlaceSettings) -> f32 {
    let angle = wrap_degrees(yaw);
    let rotated = match settings.rotation {
        Rotation::Clockwise180 => angle + 180.0,
        Rotation::CounterClockwise90 => angle + 270.0,
        Rotation::Clockwise90 => angle + 90.0,
        Rotation::None => angle,
    };
    let mirrored = match settings.mirror {
        Mirror::FrontBack => -angle,
        Mirror::LeftRight => 180.0 - angle,
        Mirror::None => angle,
    };
    rotated + (mirrored - yaw)
}

/// A template-relative entity position mirrored, then turned about the
/// pivot, as blocks are but about the block's far corner.
fn transform_vec(pos: [f64; 3], settings: &PlaceSettings) -> [f64; 3] {
    let [mut x, y, mut z] = pos;
    match settings.mirror {
        Mirror::LeftRight => z = 1.0 - z,
        Mirror::FrontBack => x = 1.0 - x,
        Mirror::None => {}
    }
    let (px, pz) = (settings.pivot.x, settings.pivot.z);
    match settings.rotation {
        Rotation::CounterClockwise90 => [f64::from(px - pz) + z, y, f64::from(px + pz + 1) - x],
        Rotation::Clockwise90 => [f64::from(px + pz + 1) - z, y, f64::from(pz - px) + x],
        Rotation::Clockwise180 => [f64::from(px + px + 1) - x, y, f64::from(pz + pz + 1) - z],
        Rotation::None => [x, y, z],
    }
}

fn is_source(fluid: Fluid) -> bool {
    matches!(fluid, Fluid::Water | Fluid::Lava)
}

/// Blocks that can take in a fluid in place: every waterloggable block,
/// plus the underwater plants that always hold water (which never change).
fn is_liquid_container(state: BlockState) -> bool {
    state.get("waterlogged").is_some()
        || matches!(
            state.block(),
            Block::Kelp | Block::KelpPlant | Block::Seagrass | Block::TallSeagrass
        )
}

fn place_liquid(level: &mut Region, pos: BlockPos, state: BlockState, fluid: Fluid) {
    let double_slab = state.get("type") == Some("double")
        && tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::SLABS, state.block());
    if fluid != Fluid::Water || double_slab || state.get("waterlogged") != Some("false") {
        return;
    }
    let Some(mut wet) = state.with("waterlogged", "true") else {
        return;
    };
    if let Some(unlit) = wet.with("lit", "false") {
        wet = unlit;
    }
    level.set_block_flags(pos, wet, true);
}

/// Waterloggable blocks placed next to water sources take them in, as long
/// as that keeps finding new ones.
fn fill_liquids(level: &mut Region, to_fill: &mut Vec<BlockPos>, locked: &[BlockPos]) {
    use tuff_vanilla_data::block::Facing;
    const DIRECTIONS: [Facing; 5] = [
        Facing::Up,
        Facing::North,
        Facing::East,
        Facing::South,
        Facing::West,
    ];
    let mut filled = true;
    while filled && !to_fill.is_empty() {
        filled = false;
        let mut i = 0;
        while i < to_fill.len() {
            let pos = to_fill[i];
            let mut found = fluid(level.block(pos));
            for direction in DIRECTIONS {
                if is_source(found) {
                    break;
                }
                let (dx, dy, dz) = direction.step();
                let neighbour = BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz);
                let neighbour_fluid = fluid(level.block(neighbour));
                if is_source(neighbour_fluid) && !locked.contains(&neighbour) {
                    found = neighbour_fluid;
                }
            }
            if is_source(found) {
                let state = level.block(pos);
                if is_liquid_container(state) {
                    place_liquid(level, pos, state, found);
                    filled = true;
                    to_fill.remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }
}

/// Stores template block entity data the way the game saves it after
/// loading: the template's compound with the position, and for loot
/// containers only what a container without items keeps.
fn store_block_entity(level: &mut Region, pos: BlockPos, state: BlockState, mut nbt: Compound) {
    if let Some(kind) = state.block().block_entity() {
        nbt.insert("id", kind.name());
    }
    level.set_block_entity(pos, nbt);
}

/// Runs every processor over the blocks in `blocks` that the settings'
/// box admits (all of them if a processor looks at the whole piece), then
/// each processor's finishing pass.
pub fn process_blocks(
    level: &mut Region,
    position: BlockPos,
    reference: BlockPos,
    settings: &PlaceSettings,
    blocks: &[BlockInfo],
    random: &mut PlaceRandom,
) -> Vec<BlockInfo> {
    let whole_piece = settings
        .processors
        .iter()
        .any(|p| p.evaluates_entire_piece());
    let mut original = Vec::new();
    let mut processed = Vec::new();
    for info in blocks {
        let pos = settings.relative(info.pos);
        let pos = BlockPos::new(pos.x + position.x, pos.y + position.y, pos.z + position.z);
        if !whole_piece && settings.bounding_box.is_some_and(|b| !b.contains(pos)) {
            continue;
        }
        let mut current = Some(BlockInfo {
            pos,
            state: info.state,
            nbt: info.nbt.clone(),
        });
        for processor in settings.processors.iter() {
            let Some(block) = current else {
                break;
            };
            current = processor.process(
                level, position, reference, info.pos, block, settings, random,
            );
        }
        if let Some(block) = current {
            processed.push(block);
            original.push(info.clone());
        }
    }
    for processor in settings.processors.iter() {
        processed = processor.finalize(
            level,
            (position, reference),
            &original,
            processed,
            settings,
            random,
        );
    }
    processed
}

struct Cache {
    names: Vec<&'static str>,
    templates: Vec<Mutex<Option<Arc<Template>>>>,
}

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| {
        let names: Vec<&str> = tuff_vanilla_data::structure::template_names().collect();
        Cache {
            templates: names.iter().map(|_| Mutex::new(None)).collect(),
            names,
        }
    })
}

/// Where templates beyond the jar's come from: the world's `generated`
/// folder first, then what data packs provide, as the game searches them.
#[derive(Default)]
struct Sources {
    generated: Option<PathBuf>,
    packs: Arc<BTreeMap<String, Arc<[u8]>>>,
    worlds: Vec<WorldSources>,
    loaded: HashMap<String, Option<Arc<Template>>>,
}

/// One running world's sources; several worlds share a process in tests,
/// and each must keep seeing its own packs when another starts.
struct WorldSources {
    owner: Weak<tuff_core::world::Shared>,
    generated: Option<PathBuf>,
    packs: Arc<BTreeMap<String, Arc<[u8]>>>,
}

impl Sources {
    fn live_worlds(&self) -> impl Iterator<Item = &WorldSources> {
        self.worlds
            .iter()
            .rev()
            .filter(|w| w.owner.strong_count() > 0)
    }

    fn generated_dirs(&self) -> impl Iterator<Item = &Path> {
        self.generated
            .as_deref()
            .into_iter()
            .chain(self.live_worlds().filter_map(|w| w.generated.as_deref()))
    }

    fn pack_template(&self, qualified: &str) -> Option<&Arc<[u8]>> {
        self.packs
            .get(qualified)
            .or_else(|| self.live_worlds().find_map(|w| w.packs.get(qualified)))
    }
}

fn sources() -> &'static Mutex<Sources> {
    static SOURCES: OnceLock<Mutex<Sources>> = OnceLock::new();
    SOURCES.get_or_init(Mutex::default)
}

static HAS_SOURCES: AtomicBool = AtomicBool::new(false);

fn refresh_flag(sources: &Sources) {
    HAS_SOURCES.store(
        sources.generated.is_some()
            || !sources.packs.is_empty()
            || sources.live_worlds().next().is_some(),
        Ordering::Release,
    );
}

/// Sets a running world's `generated` folder and data pack templates,
/// searched after those set process-wide.
pub fn set_world_sources(
    owner: &Arc<tuff_core::world::Shared>,
    generated: Option<PathBuf>,
    packs: Arc<BTreeMap<String, Arc<[u8]>>>,
) {
    let mut sources = sources().lock().unwrap_or_else(PoisonError::into_inner);
    let owner = Arc::downgrade(owner);
    sources
        .worlds
        .retain(|w| w.owner.strong_count() > 0 && !w.owner.ptr_eq(&owner));
    sources.worlds.push(WorldSources {
        owner,
        generated,
        packs,
    });
    sources.loaded.clear();
    refresh_flag(&sources);
}

/// Sets the world folder whose `<namespace>/structure/<id>.nbt` files
/// are the first place templates are looked up, and where saves go.
pub fn set_generated_dir(dir: Option<PathBuf>) {
    let mut sources = sources().lock().unwrap_or_else(PoisonError::into_inner);
    sources.generated = dir;
    sources.loaded.clear();
    refresh_flag(&sources);
}

/// Sets the templates data packs provide (gzip NBT by qualified id).
pub fn set_pack_templates(packs: Arc<BTreeMap<String, Arc<[u8]>>>) {
    let mut sources = sources().lock().unwrap_or_else(PoisonError::into_inner);
    sources.packs = packs;
    sources.loaded.clear();
    refresh_flag(&sources);
}

fn valid_template_id(qualified: &str) -> bool {
    let Some((namespace, path)) = qualified.split_once(':') else {
        return false;
    };
    let plain = |c: char| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | '.');
    !namespace.is_empty()
        && namespace.chars().all(plain)
        && !path.is_empty()
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && part.chars().all(plain))
}

fn generated_path(dir: &Path, qualified: &str) -> Option<PathBuf> {
    if !valid_template_id(qualified) {
        return None;
    }
    let (namespace, path) = qualified.split_once(':')?;
    Some(
        dir.join(namespace)
            .join("structure")
            .join(format!("{path}.nbt")),
    )
}

fn external(qualified: &str) -> Option<Arc<Template>> {
    let mut sources = sources().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(found) = sources.loaded.get(qualified) {
        return found.clone();
    }
    let from_disk = sources
        .generated_dirs()
        .filter_map(|dir| generated_path(dir, qualified))
        .find_map(|path| std::fs::read(path).ok())
        .and_then(|data| parse_external(qualified, &data));
    let found = from_disk.or_else(|| {
        let data = sources.pack_template(qualified)?;
        parse_external(qualified, data)
    });
    sources.loaded.insert(qualified.to_owned(), found.clone());
    found
}

fn parse_external(qualified: &str, data: &[u8]) -> Option<Arc<Template>> {
    match Template::parse(data) {
        Ok(template) => Some(Arc::new(template)),
        Err(error) => {
            log::warn!("Failed to load structure template {qualified}: {error}");
            None
        }
    }
}

/// A template by name, parsed on first use; a name nothing provides is an
/// empty template, as in the game.
pub fn get(name: &str) -> Arc<Template> {
    static EMPTY: OnceLock<Arc<Template>> = OnceLock::new();
    find(name).unwrap_or_else(|| EMPTY.get_or_init(|| Arc::new(Template::default())).clone())
}

/// A template by name, `None` where nothing provides it.
pub fn find(name: &str) -> Option<Arc<Template>> {
    let qualified = crate::json::qualify(name);
    if HAS_SOURCES.load(Ordering::Acquire)
        && let Some(template) = external(&qualified)
    {
        return Some(template);
    }
    let cache = cache();
    let Ok(index) = cache.names.binary_search(&qualified.as_str()) else {
        return None;
    };
    let mut slot = cache.templates[index]
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let template = slot.get_or_insert_with(|| {
        let data = tuff_vanilla_data::structure::template(&qualified).expect("listed template");
        Arc::new(Template::parse(data).unwrap_or_else(|e| panic!("template {qualified}: {e}")))
    });
    Some(template.clone())
}

/// Writes `root` as the gzip NBT template `name` in the generated folder,
/// and makes later lookups see it.
pub fn save(name: &str, root: Compound) -> std::io::Result<()> {
    let qualified = crate::json::qualify(name);
    let mut sources = sources().lock().unwrap_or_else(PoisonError::into_inner);
    let path = sources
        .generated_dirs()
        .next()
        .and_then(|dir| generated_path(dir, &qualified))
        .ok_or_else(|| std::io::Error::other("no generated folder or invalid template name"))?;
    let bytes = NamedTag::new("", root)
        .to_bytes(tuff_nbt::Compression::Gzip)
        .map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension("nbt.tmp");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, &path)?;
    sources.loaded.remove(&qualified);
    Ok(())
}

/// Makes `root` the template `name` in memory only, ahead of every source,
/// until [`forget`] or a source change drops it.
pub fn keep(name: &str, root: &Compound) -> bool {
    let qualified = crate::json::qualify(name);
    if !valid_template_id(&qualified) {
        return false;
    }
    let Ok(template) = Template::from_nbt(root) else {
        return false;
    };
    let mut sources = sources().lock().unwrap_or_else(PoisonError::into_inner);
    sources.loaded.insert(qualified, Some(Arc::new(template)));
    HAS_SOURCES.store(true, Ordering::Release);
    true
}

/// Drops the in-memory copy of `name`; the next lookup reads its source.
pub fn forget(name: &str) {
    let qualified = crate::json::qualify(name);
    let mut sources = sources().lock().unwrap_or_else(PoisonError::into_inner);
    sources.loaded.remove(&qualified);
}

/// Drops every parsed template; each is parsed again when next used.
pub fn clear_cache() {
    for slot in &cache().templates {
        let dropped = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        drop(dropped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_nbt::Compression;

    fn gzip(root: Compound) -> Vec<u8> {
        NamedTag::new("", root).to_bytes(Compression::Gzip).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn cleared_templates_are_parsed_again() {
        let name = "minecraft:igloo/top";
        let first = get(name);
        assert!(Arc::ptr_eq(&first, &get(name)));
        clear_cache();
        let again = get(name);
        assert!(!Arc::ptr_eq(&first, &again));
        assert_eq!(first.size, again.size);
        assert_eq!(first.palettes, again.palettes);
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_areas_load_back_and_packs_fill_the_gaps() {
        let dir = std::env::temp_dir().join(format!("tuff-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_generated_dir(Some(dir.clone()));
        let stone = Block::Stone.default_state();
        let chest = Block::Chest.default_state();
        let chest_data = Compound::new().with("id", "minecraft:chest").with("x", 9);
        let root = Template::capture(CapturedArea {
            size: [2, 1, 1],
            blocks: vec![
                (BlockPos::new(0, 0, 0), stone, None),
                (BlockPos::new(1, 0, 0), chest, Some(chest_data)),
                (
                    BlockPos::new(1, 0, 0),
                    Block::StructureVoid.default_state(),
                    None,
                ),
            ],
            entities: vec![(
                [0.5, 0.0, 0.5],
                BlockPos::new(0, 0, 0),
                Compound::new().with("id", "minecraft:pig"),
            )],
        });
        save("tuff:test/saved", root.clone()).unwrap();
        assert!(dir.join("tuff/structure/test/saved.nbt").exists());
        let loaded = get("tuff:test/saved");
        assert_eq!(loaded.size, [2, 1, 1]);
        assert_eq!(loaded.palettes[0].len(), 2);
        let with_data = loaded.palettes[0].iter().find(|b| b.nbt.is_some()).unwrap();
        assert_eq!(with_data.state, chest);
        assert!(with_data.nbt.as_ref().unwrap().get("x").is_none());
        assert!(save("tuff:../escape", root.clone()).is_err());
        assert!(loaded.has_entities);

        let only_stone = Template::capture(CapturedArea {
            size: [3, 1, 1],
            blocks: vec![(BlockPos::new(0, 0, 0), stone, None)],
            entities: Vec::new(),
        });
        assert!(keep("tuff:test/saved", &only_stone));
        assert_eq!(get("tuff:test/saved").size, [3, 1, 1]);
        assert!(!dir.join("tuff/structure/test/saved.nbt.tmp").exists());
        forget("tuff:test/saved");
        assert_eq!(get("tuff:test/saved").size, [2, 1, 1]);

        let packed = NamedTag::new(
            "",
            Template::capture(CapturedArea {
                size: [1, 1, 1],
                blocks: vec![(BlockPos::new(0, 0, 0), stone, None)],
                entities: Vec::new(),
            }),
        )
        .to_bytes(Compression::Gzip)
        .unwrap();
        let mut packs = BTreeMap::new();
        packs.insert("tuff:test/from_pack".to_owned(), Arc::<[u8]>::from(packed));
        set_pack_templates(Arc::new(packs));
        assert_eq!(get("tuff:test/from_pack").size, [1, 1, 1]);
        assert_eq!(get("tuff:test/saved").size, [2, 1, 1]);
        set_pack_templates(Arc::default());
        set_generated_dir(None);
        assert_eq!(get("tuff:test/saved").size, [0, 0, 0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tuff_core::watchdog]
    #[test]
    fn template_blocks_stay_small() {
        assert!(std::mem::size_of::<BlockInfo>() <= 24);
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_jar_template_parses() {
        for name in tuff_vanilla_data::structure::template_names() {
            let data = tuff_vanilla_data::structure::template(name).unwrap();
            let template = Template::parse(data).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                template.size.iter().all(|&s| s >= 0),
                "{name} has a negative size"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn templates_sort_full_blocks_first_and_block_entities_last() {
        let template = get("minecraft:ruined_portal/portal_1");
        let blocks = &template.palettes[0];
        let first_other = blocks
            .iter()
            .position(|b| !b.state.is_template_full_block())
            .unwrap();
        let first_entity = blocks.iter().position(|b| b.nbt.is_some()).unwrap();
        assert!(first_other < first_entity);
        assert!(
            blocks[..first_other]
                .windows(2)
                .all(|w| sort_key(&w[0]) <= sort_key(&w[1]))
        );
        assert!(blocks[first_entity..].iter().all(|b| b.nbt.is_some()));
        assert_eq!(blocks[first_entity].state.block(), Block::Jigsaw);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bastions_and_villages_bring_their_mobs() {
        let mobs_of = |kind: &str| {
            tuff_vanilla_data::structure::template_names()
                .filter(|name| {
                    get(name)
                        .entities
                        .iter()
                        .any(|e| e.nbt.get_str("id") == Some(kind))
                })
                .count()
        };
        assert!(mobs_of("minecraft:piglin") > 0, "bastion piglins");
        assert!(mobs_of("minecraft:hoglin") > 0, "bastion hoglins");
        assert!(mobs_of("minecraft:villager") > 0, "villagers");
    }

    #[tuff_core::watchdog]
    #[test]
    fn entity_positions_and_yaws_turn_with_the_template() {
        let settings = |rotation, mirror| PlaceSettings {
            rotation,
            mirror,
            pivot: BlockPos::new(2, 0, 3),
            ..PlaceSettings::default()
        };
        let pos = [1.25, 4.0, 0.5];
        let plain = settings(Rotation::None, Mirror::None);
        assert_eq!(transform_vec(pos, &plain), pos);
        assert_eq!(rotated_yaw(30.0, &plain), 30.0);
        let turned = settings(Rotation::Clockwise90, Mirror::None);
        assert_eq!(transform_vec(pos, &turned), [5.5, 4.0, 2.25]);
        assert_eq!(rotated_yaw(30.0, &turned), 120.0);
        let mirrored = settings(Rotation::None, Mirror::FrontBack);
        assert_eq!(transform_vec(pos, &mirrored), [-0.25, 4.0, 0.5]);
        assert_eq!(rotated_yaw(30.0, &mirrored), -30.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unknown_templates_are_empty() {
        let template = get("minecraft:ancient_city/walls/intact_horizontal_wall_stairs_5");
        assert_eq!(template.size, [0, 0, 0]);
        assert!(template.palettes.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn oversized_templates_are_refused() {
        let mut root = Compound::new();
        root.insert("filler", Tag::ByteArray(vec![0; MAX_TEMPLATE_BYTES + 1]));
        let data = gzip(root);
        assert!(data.len() < MAX_TEMPLATE_BYTES / 64, "a decompression bomb");
        assert!(Template::parse(&data).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn truncated_and_malformed_templates_are_refused() {
        let data =
            tuff_vanilla_data::structure::template("minecraft:ruined_portal/portal_1").unwrap();
        assert!(Template::parse(&data[..data.len() / 2]).is_err());
        assert!(Template::parse(&[0x1F, 0x8B, 0x08, 0x00, 0xFF, 0xFF]).is_err());
        let mut garbage = data.to_vec();
        for byte in garbage.iter_mut().skip(20).step_by(7) {
            *byte ^= 0x5A;
        }
        assert!(Template::parse(&garbage).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn deeply_nested_templates_are_refused() {
        let mut tag = Tag::Compound(Compound::new());
        for _ in 0..MAX_TEMPLATE_DEPTH + 8 {
            tag = Tag::Compound(Compound::new().with("n", tag));
        }
        let data = gzip(Compound::new().with("nested", tag));
        assert!(Template::parse(&data).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn unknown_palette_blocks_are_refused() {
        let root = Compound::new()
            .with(
                "size",
                Tag::List(vec![Tag::Int(1), Tag::Int(1), Tag::Int(1)]),
            )
            .with(
                "palette",
                Tag::List(vec![Tag::Compound(
                    Compound::new().with("Name", "minecraft:not_a_block"),
                )]),
            )
            .with("blocks", Tag::List(Vec::new()));
        assert!(Template::parse(&gzip(root)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn bounding_boxes_follow_rotation_and_mirror() {
        let template = get("minecraft:ruined_portal/portal_1");
        assert_eq!(template.size, [6, 10, 6]);
        let pivot = BlockPos::new(3, 0, 3);
        let origin = BlockPos::new(304, 69, 288);
        let b = template.bounding_box(origin, Rotation::Clockwise90, pivot, Mirror::None);
        assert_eq!(
            (b.min_x, b.min_y, b.min_z, b.max_x, b.max_y, b.max_z),
            (305, 69, 288, 310, 78, 293)
        );
        let b = template.bounding_box(origin, Rotation::None, pivot, Mirror::FrontBack);
        assert_eq!((b.min_x, b.max_x, b.min_z, b.max_z), (299, 304, 288, 293));
    }
}
