//! Structure processors: per-block rewrites applied while a template is
//! placed (decay, moss, protected blocks, ...), a registry keyed by
//! resource id like feature kinds.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::OwnedIntProvider;
use crate::feature::level::Region;
use crate::feature::predicate::RuleTest;
use crate::json::{self, Result, obj};
use crate::proto::HeightmapKind;
use crate::structure::template::{BlockInfo, PlaceRandom, PlaceSettings};
use crate::tags::BlockSet;
use tuff_item::random::{LegacyRandom, Random, RandomSource, position_seed};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

pub trait Processor: Send + Sync {
    /// The block `info` (already at its world position) as it will be
    /// placed, or `None` to skip it. `template_pos` is where it sits in the
    /// template.
    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn process(
        &self,
        level: &mut Region,
        position: BlockPos,
        reference: BlockPos,
        template_pos: BlockPos,
        info: BlockInfo,
        settings: &PlaceSettings,
        random: &mut PlaceRandom,
    ) -> Option<BlockInfo>;

    /// A pass over every processed block once all are processed.
    fn finalize(
        &self,
        _level: &mut Region,
        _anchors: (BlockPos, BlockPos),
        _original: &[BlockInfo],
        processed: Vec<BlockInfo>,
        _settings: &PlaceSettings,
        _random: &mut PlaceRandom,
    ) -> Vec<BlockInfo> {
        processed
    }

    /// Whether the processor needs the blocks outside the chunk being
    /// placed as well.
    fn evaluates_entire_piece(&self) -> bool {
        false
    }
}

#[derive(Clone, Default)]
pub struct ProcessorList(pub Vec<Arc<dyn Processor>>);

impl ProcessorList {
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Processor>> {
        self.0.iter()
    }

    pub fn push(&mut self, processor: impl Processor + 'static) {
        self.0.push(Arc::new(processor));
    }

    /// A processor list: a name, a bare list, or `{processors: [...]}`.
    pub fn parse(value: &Value) -> Result<Self> {
        match value {
            Value::String(name) => named(name),
            Value::Array(list) => Ok(Self(
                list.iter().map(parse_processor).collect::<Result<_>>()?,
            )),
            other => {
                let o = obj(other, &["processors"], "processor list")?;
                Ok(Self(
                    o.list("processors")?
                        .iter()
                        .map(parse_processor)
                        .collect::<Result<_>>()?,
                ))
            }
        }
    }
}

/// A named processor list from the data pack, parsed once.
pub fn named(name: &str) -> Result<ProcessorList> {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<String, ProcessorList>>> = OnceLock::new();
    let name = json::qualify(name);
    if let Some(list) = crate::loaded::structures::processors(&name) {
        return list;
    }
    let cache = CACHE.get_or_init(Default::default);
    if let Some(list) = cache.lock().unwrap().get(&name) {
        return Ok(list.clone());
    }
    let text = tuff_vanilla_data::structure::processor_list(&name)
        .ok_or_else(|| format!("unknown processor list {name}"))?;
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let list = ProcessorList::parse(&value)?;
    cache.lock().unwrap().insert(name, list.clone());
    Ok(list)
}

type Factory = fn(&Value) -> Result<Arc<dyn Processor>>;

/// Processor kinds by resource id.
pub fn registry() -> &'static HashMap<&'static str, Factory> {
    static REGISTRY: OnceLock<HashMap<&'static str, Factory>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut map: HashMap<&'static str, Factory> = HashMap::new();
        map.insert("minecraft:rule", |v| Ok(Arc::new(RuleProcessor::parse(v)?)));
        map.insert("minecraft:block_rot", |v| Ok(Arc::new(BlockRot::parse(v)?)));
        map.insert("minecraft:block_ignore", |v| {
            let o = obj(v, &["processor_type", "blocks"], "block ignore")?;
            let blocks = o
                .list("blocks")?
                .iter()
                .map(|b| json::block_state(b).map(|s| s.block()))
                .collect::<Result<_>>()?;
            Ok(Arc::new(BlockIgnore(blocks)))
        });
        map.insert("minecraft:protected_blocks", |v| {
            let o = obj(v, &["processor_type", "value"], "protected blocks")?;
            Ok(Arc::new(Protected(json::block_set(o.req("value")?)?)))
        });
        map.insert("minecraft:capped", |v| {
            let o = obj(v, &["processor_type", "delegate", "limit"], "capped")?;
            Ok(Arc::new(Capped {
                delegate: parse_processor(o.req("delegate")?)?,
                limit: OwnedIntProvider::parse(o.req("limit")?)?,
            }))
        });
        map.insert("minecraft:gravity", |v| {
            let o = obj(v, &["processor_type", "heightmap", "offset"], "gravity")?;
            let heightmap = match o.get("heightmap") {
                Some(name) => HeightmapKind::from_name(name.as_str().unwrap_or(""))
                    .ok_or("unknown heightmap")?,
                None => HeightmapKind::WorldSurfaceWg,
            };
            Ok(Arc::new(Gravity {
                heightmap,
                offset: o.i32_or("offset", 0)?,
            }))
        });
        map.insert("minecraft:block_age", |v| {
            let o = obj(v, &["processor_type", "mossiness"], "block age")?;
            Ok(Arc::new(BlockAge {
                mossiness: o.f32("mossiness")?,
            }))
        });
        map.insert("minecraft:blackstone_replace", |v| {
            obj(v, &["processor_type"], "blackstone replace")?;
            Ok(Arc::new(BlackstoneReplace))
        });
        map.insert("minecraft:lava_submerged_block", |v| {
            obj(v, &["processor_type"], "lava submerged block")?;
            Ok(Arc::new(LavaSubmerged))
        });
        map.insert("minecraft:jigsaw_replacement", |v| {
            obj(v, &["processor_type"], "jigsaw replacement")?;
            Ok(Arc::new(JigsawReplacement))
        });
        map.insert("minecraft:nop", |v| {
            obj(v, &["processor_type"], "nop")?;
            Ok(Arc::new(Nop))
        });
        map
    })
}

pub fn parse_processor(value: &Value) -> Result<Arc<dyn Processor>> {
    let kind = value
        .get("processor_type")
        .and_then(Value::as_str)
        .ok_or("processors have a processor_type")?;
    let kind = json::qualify(kind);
    let factory = registry()
        .get(kind.as_str())
        .ok_or_else(|| format!("unmodeled processor {kind}"))?;
    factory(value)
}

pub struct Nop;

impl Processor for Nop {
    fn process(
        &self,
        _: &mut Region,
        _: BlockPos,
        _: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        _: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        Some(info)
    }
}

/// Skips the listed blocks.
pub struct BlockIgnore(pub Vec<Block>);

impl BlockIgnore {
    pub fn structure_block() -> Self {
        Self(vec![Block::StructureBlock])
    }

    pub fn structure_and_air() -> Self {
        Self(vec![Block::Air, Block::StructureBlock])
    }
}

impl Processor for BlockIgnore {
    fn process(
        &self,
        _: &mut Region,
        _: BlockPos,
        _: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        _: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        (!self.0.contains(&info.state.block())).then_some(info)
    }
}

/// Skips blocks whose world position holds a protected block.
pub struct Protected(pub BlockSet);

impl Processor for Protected {
    fn process(
        &self,
        level: &mut Region,
        _: BlockPos,
        _: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        _: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        (!self.0.contains_state(level.block(info.pos))).then_some(info)
    }
}

/// Drops blocks at random (only the listed ones, if any).
pub struct BlockRot {
    rottable: Option<BlockSet>,
    integrity: f32,
}

impl BlockRot {
    pub fn new(integrity: f32) -> Self {
        Self {
            rottable: None,
            integrity,
        }
    }

    fn parse(value: &Value) -> Result<Self> {
        let o = obj(
            value,
            &["processor_type", "integrity", "rottable_blocks"],
            "block rot",
        )?;
        Ok(Self {
            rottable: o.get("rottable_blocks").map(json::block_set).transpose()?,
            integrity: o.f32("integrity")?,
        })
    }
}

impl Processor for BlockRot {
    fn process(
        &self,
        _: &mut Region,
        _: BlockPos,
        _: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        random: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        if self
            .rottable
            .as_ref()
            .is_some_and(|set| !set.contains_state(info.state))
        {
            return Some(info);
        }
        let keep = random.with(info.pos, |r| r.next_float() <= self.integrity);
        keep.then_some(info)
    }
}

enum PosRuleTest {
    AlwaysTrue,
    Linear {
        min_chance: f32,
        max_chance: f32,
        min_dist: i32,
        max_dist: i32,
        axis: Option<Facing>,
    },
}

impl PosRuleTest {
    fn parse(value: &Value) -> Result<Self> {
        let kind = json::qualify(
            value
                .get("predicate_type")
                .and_then(Value::as_str)
                .ok_or("position rule tests have a predicate_type")?,
        );
        let linear = |o: json::Obj, axis| -> Result<Self> {
            Ok(Self::Linear {
                min_chance: o.f32_or("min_chance", 0.0)?,
                max_chance: o.f32_or("max_chance", 0.0)?,
                min_dist: o.i32_or("min_dist", 0)?,
                max_dist: o.i32_or("max_dist", 0)?,
                axis,
            })
        };
        const LINEAR: [&str; 5] = [
            "predicate_type",
            "min_chance",
            "max_chance",
            "min_dist",
            "max_dist",
        ];
        match kind.as_str() {
            "minecraft:always_true" => {
                obj(value, &["predicate_type"], "position rule test")?;
                Ok(Self::AlwaysTrue)
            }
            "minecraft:linear_pos" => linear(obj(value, &LINEAR, "linear pos")?, None),
            "minecraft:axis_aligned_linear_pos" => {
                let mut fields = LINEAR.to_vec();
                fields.push("axis");
                let o = obj(value, &fields, "axis aligned linear pos")?;
                let axis = match o.get("axis").and_then(Value::as_str).unwrap_or("y") {
                    "x" => Facing::East,
                    "y" => Facing::Up,
                    "z" => Facing::South,
                    other => return Err(format!("unknown axis {other}")),
                };
                linear(o, Some(axis))
            }
            other => Err(format!("unmodeled position rule test {other}")),
        }
    }

    fn test(&self, world: BlockPos, reference: BlockPos, random: &mut dyn RandomSource) -> bool {
        let Self::Linear {
            min_chance,
            max_chance,
            min_dist,
            max_dist,
            axis,
        } = self
        else {
            return true;
        };
        let dist = match axis {
            None => {
                (world.x - reference.x).abs()
                    + (world.y - reference.y).abs()
                    + (world.z - reference.z).abs()
            }
            Some(axis) => {
                let (sx, sy, sz) = axis.step();
                let xd = ((world.x - reference.x) * sx).abs() as f32;
                let yd = ((world.y - reference.y) * sy).abs() as f32;
                let zd = ((world.z - reference.z) * sz).abs() as f32;
                (xd + yd + zd) as i32
            }
        };
        let rnd = random.next_float();
        let factor = (dist as f32 - *min_dist as f32) / (*max_dist as f32 - *min_dist as f32);
        let chance = if factor < 0.0 {
            *min_chance
        } else if factor > 1.0 {
            *max_chance
        } else {
            min_chance + factor * (max_chance - min_chance)
        };
        rnd <= chance
    }
}

enum BlockEntityModifier {
    Passthrough,
    Clear,
    AppendLoot(String),
    AppendStatic(Compound),
}

fn json_to_tag(value: &Value) -> Tag {
    match value {
        Value::Null => Tag::Compound(Compound::new()),
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match n.as_i64().and_then(|i| i32::try_from(i).ok()) {
            Some(i) => Tag::Int(i),
            None => Tag::Double(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(items) => Tag::List(items.iter().map(json_to_tag).collect()),
        Value::Object(map) => Tag::Compound(
            map.iter()
                .map(|(key, value)| (key.clone(), json_to_tag(value)))
                .collect(),
        ),
    }
}

fn merge_compound(target: &mut Compound, source: &Compound) {
    for (key, value) in source.iter() {
        match (target.get_mut(key), value) {
            (Some(Tag::Compound(existing)), Tag::Compound(incoming)) => {
                merge_compound(existing, incoming)
            }
            _ => {
                target.insert(key, value.clone());
            }
        }
    }
}

impl BlockEntityModifier {
    fn parse(value: &Value) -> Result<Self> {
        let kind = json::qualify(json::kind(value)?);
        Ok(match kind.as_str() {
            "minecraft:passthrough" => {
                obj(value, &["type"], "passthrough")?;
                Self::Passthrough
            }
            "minecraft:clear" => {
                obj(value, &["type"], "clear")?;
                Self::Clear
            }
            "minecraft:append_loot" => {
                let o = obj(value, &["type", "loot_table"], "append loot")?;
                Self::AppendLoot(json::qualify(o.str("loot_table")?))
            }
            "minecraft:append_static" => {
                let o = obj(value, &["type", "data"], "append static")?;
                let data = match o.req("data")? {
                    Value::String(snbt) => {
                        Tag::from_snbt(snbt).map_err(|e| format!("append static data: {e}"))?
                    }
                    other => json_to_tag(other),
                };
                match data {
                    Tag::Compound(c) => Self::AppendStatic(c),
                    _ => return Err("append static data must be a compound".to_string()),
                }
            }
            other => return Err(format!("unmodeled block entity modifier {other}")),
        })
    }

    fn apply(&self, random: &mut dyn RandomSource, existing: Option<Compound>) -> Option<Compound> {
        match self {
            Self::Passthrough => existing,
            Self::Clear => Some(Compound::new()),
            Self::AppendLoot(table) => {
                let mut out = existing.unwrap_or_default();
                out.insert("LootTable", table.as_str());
                out.insert("LootTableSeed", random.next_long());
                Some(out)
            }
            Self::AppendStatic(data) => Some(match existing {
                None => data.clone(),
                Some(mut tag) => {
                    merge_compound(&mut tag, data);
                    tag
                }
            }),
        }
    }
}

pub struct ProcessorRule {
    input: RuleTest,
    location: RuleTest,
    position: PosRuleTest,
    output: BlockState,
    modifier: BlockEntityModifier,
}

impl ProcessorRule {
    /// A rule replacing `block` (with probability `chance`, if given) by
    /// `output`'s default state wherever it is placed.
    pub fn replace(block: Block, chance: Option<f32>, output: Block) -> Self {
        Self {
            input: match chance {
                Some(p) => RuleTest::RandomBlock(block, p),
                None => RuleTest::Block(block),
            },
            location: RuleTest::AlwaysTrue,
            position: PosRuleTest::AlwaysTrue,
            output: output.default_state(),
            modifier: BlockEntityModifier::Passthrough,
        }
    }

    fn parse(value: &Value) -> Result<Self> {
        let o = obj(
            value,
            &[
                "input_predicate",
                "location_predicate",
                "position_predicate",
                "output_state",
                "block_entity_modifier",
            ],
            "processor rule",
        )?;
        Ok(Self {
            input: RuleTest::parse(o.req("input_predicate")?)?,
            location: RuleTest::parse(o.req("location_predicate")?)?,
            position: o
                .get("position_predicate")
                .map(PosRuleTest::parse)
                .transpose()?
                .unwrap_or(PosRuleTest::AlwaysTrue),
            output: json::block_state(o.req("output_state")?)?,
            modifier: o
                .get("block_entity_modifier")
                .map(BlockEntityModifier::parse)
                .transpose()?
                .unwrap_or(BlockEntityModifier::Passthrough),
        })
    }
}

/// The first rule whose tests pass replaces the block.
pub struct RuleProcessor(pub Vec<ProcessorRule>);

impl RuleProcessor {
    fn parse(value: &Value) -> Result<Self> {
        let o = obj(value, &["processor_type", "rules"], "rule processor")?;
        Ok(Self(
            o.list("rules")?
                .iter()
                .map(ProcessorRule::parse)
                .collect::<Result<_>>()?,
        ))
    }
}

impl Processor for RuleProcessor {
    fn process(
        &self,
        level: &mut Region,
        _: BlockPos,
        reference: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        _: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        let pos = info.pos;
        let mut random = LegacyRandom::new(position_seed(pos.x, pos.y, pos.z));
        let world = level.block(pos);
        for rule in &self.0 {
            if rule.input.test(info.state, pos, &mut random)
                && rule.location.test(world, pos, &mut random)
                && rule.position.test(pos, reference, &mut random)
            {
                return Some(BlockInfo {
                    pos,
                    state: rule.output,
                    nbt: rule
                        .modifier
                        .apply(&mut random, info.nbt.map(|nbt| *nbt))
                        .map(Box::new),
                });
            }
        }
        Some(info)
    }
}

/// Applies its delegate to at most `limit` random blocks of the whole
/// piece.
pub struct Capped {
    delegate: Arc<dyn Processor>,
    limit: OwnedIntProvider,
}

impl Processor for Capped {
    fn process(
        &self,
        _: &mut Region,
        _: BlockPos,
        _: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        _: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        Some(info)
    }

    fn evaluates_entire_piece(&self) -> bool {
        true
    }

    fn finalize(
        &self,
        level: &mut Region,
        (position, reference): (BlockPos, BlockPos),
        original: &[BlockInfo],
        mut processed: Vec<BlockInfo>,
        settings: &PlaceSettings,
        place_random: &mut PlaceRandom,
    ) -> Vec<BlockInfo> {
        if self.limit.max_inclusive() == 0
            || processed.is_empty()
            || original.len() != processed.len()
        {
            return processed;
        }
        let seed = level.seed();
        let mut random = Random::Legacy(LegacyRandom::new(seed))
            .fork_positional()
            .at(position.x, position.y, position.z);
        let max = self.limit.sample(&mut random).min(processed.len() as i32);
        if max < 1 {
            return processed;
        }
        let mut indices: Vec<usize> = (0..processed.len()).collect();
        for i in (2..=indices.len()).rev() {
            let swap = random.next_int_bounded(i as i32) as usize;
            indices.swap(i - 1, swap);
        }
        let mut replaced = 0;
        for index in indices {
            if replaced >= max {
                break;
            }
            let current = processed[index].clone();
            let altered = self.delegate.process(
                level,
                position,
                reference,
                original[index].pos,
                current.clone(),
                settings,
                place_random,
            );
            if let Some(altered) = altered
                && altered != current
            {
                replaced += 1;
                processed[index] = altered;
            }
        }
        processed
    }
}

/// Drops blocks onto the terrain, keeping their height within the
/// template.
pub struct Gravity {
    heightmap: HeightmapKind,
    offset: i32,
}

impl Gravity {
    pub fn new(heightmap: HeightmapKind, offset: i32) -> Self {
        Self { heightmap, offset }
    }
}

impl Processor for Gravity {
    fn process(
        &self,
        level: &mut Region,
        _: BlockPos,
        _: BlockPos,
        template_pos: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        _: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        let pos = info.pos;
        let height = level.height(self.heightmap, pos.x, pos.z) + self.offset;
        Some(BlockInfo {
            pos: BlockPos::new(pos.x, height + template_pos.y, pos.z),
            ..info
        })
    }
}

/// Weathers ruined portal stone: cracks, moss, crying obsidian.
pub struct BlockAge {
    pub mossiness: f32,
}

impl BlockAge {
    fn random_stairs(random: &mut dyn RandomSource, block: Block) -> BlockState {
        let facing = Facing::HORIZONTAL[random.next_int_bounded(4) as usize];
        let half = ["top", "bottom"][random.next_int_bounded(2) as usize];
        block
            .default_state()
            .with("facing", facing_name(facing))
            .and_then(|s| s.with("half", half))
            .expect("stairs have a facing and a half")
    }

    fn pick(
        &self,
        random: &mut dyn RandomSource,
        plain: &[BlockState],
        mossy: &[BlockState],
    ) -> BlockState {
        let list = if random.next_float() < self.mossiness {
            mossy
        } else {
            plain
        };
        list[random.next_int_bounded(list.len() as i32) as usize]
    }
}

pub fn facing_name(facing: Facing) -> &'static str {
    match facing {
        Facing::North => "north",
        Facing::South => "south",
        Facing::East => "east",
        Facing::West => "west",
        Facing::Up => "up",
        Facing::Down => "down",
    }
}

/// `block`'s default state with every property `from` shares copied over.
pub fn with_properties_of(block: Block, from: BlockState) -> BlockState {
    let mut state = block.default_state();
    for (key, value) in from.properties() {
        if let Some(next) = state.with(key, value) {
            state = next;
        }
    }
    state
}

impl Processor for BlockAge {
    fn process(
        &self,
        _: &mut Region,
        _: BlockPos,
        _: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        random: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        use tuff_datapack::tags::is;
        use tuff_vanilla_data::tag::block::key::{SLABS, STAIRS, WALLS};
        let state = info.state;
        let block = state.block();
        let replaced = random.with(info.pos, |random| -> Option<BlockState> {
            if matches!(
                block,
                Block::StoneBricks | Block::Stone | Block::ChiseledStoneBricks
            ) {
                if random.next_float() >= 0.5 {
                    return None;
                }
                let plain = [
                    Block::CrackedStoneBricks.default_state(),
                    Self::random_stairs(random, Block::StoneBrickStairs),
                ];
                let mossy = [
                    Block::MossyStoneBricks.default_state(),
                    Self::random_stairs(random, Block::MossyStoneBrickStairs),
                ];
                Some(self.pick(random, &plain, &mossy))
            } else if is(STAIRS, block) {
                if random.next_float() >= 0.5 {
                    return None;
                }
                let plain = [
                    Block::StoneSlab.default_state(),
                    Block::StoneBrickSlab.default_state(),
                ];
                let mossy = [
                    with_properties_of(Block::MossyStoneBrickStairs, state),
                    Block::MossyStoneBrickSlab.default_state(),
                ];
                Some(self.pick(random, &plain, &mossy))
            } else if is(SLABS, block) {
                (random.next_float() < self.mossiness)
                    .then(|| with_properties_of(Block::MossyStoneBrickSlab, state))
            } else if is(WALLS, block) {
                (random.next_float() < self.mossiness)
                    .then(|| with_properties_of(Block::MossyStoneBrickWall, state))
            } else if block == Block::Obsidian {
                (random.next_float() < 0.15).then(|| Block::CryingObsidian.default_state())
            } else {
                None
            }
        });
        Some(match replaced {
            Some(state) => BlockInfo { state, ..info },
            None => info,
        })
    }
}

/// Swaps stone for blackstone variants, keeping stair and slab shape.
pub struct BlackstoneReplace;

impl Processor for BlackstoneReplace {
    fn process(
        &self,
        _: &mut Region,
        _: BlockPos,
        _: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        _: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        let old = info.state;
        let Some((_, new)) = tuff_vanilla_data::world::BLACKSTONE_REPLACEMENTS
            .iter()
            .find(|(from, _)| *from == old.block())
        else {
            return Some(info);
        };
        let mut state = new.default_state();
        for key in ["facing", "half", "type"] {
            if let Some(value) = old.get(key)
                && let Some(next) = state.with(key, value)
            {
                state = next;
            }
        }
        Some(BlockInfo { state, ..info })
    }
}

/// Blocks that do not fill their space stay lava where lava was.
pub struct LavaSubmerged;

impl Processor for LavaSubmerged {
    fn process(
        &self,
        level: &mut Region,
        _: BlockPos,
        _: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        _: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        if level.block(info.pos).block() == Block::Lava && !info.state.is_shape_full_block() {
            return Some(BlockInfo {
                state: Block::Lava.default_state(),
                ..info
            });
        }
        Some(info)
    }
}

/// Jigsaw blocks become the state they name.
pub struct JigsawReplacement;

impl Processor for JigsawReplacement {
    fn process(
        &self,
        _: &mut Region,
        _: BlockPos,
        _: BlockPos,
        _: BlockPos,
        info: BlockInfo,
        _: &PlaceSettings,
        _: &mut PlaceRandom,
    ) -> Option<BlockInfo> {
        if info.state.block() != Block::Jigsaw {
            return Some(info);
        }
        let Some(nbt) = &info.nbt else {
            return Some(info);
        };
        let state = parse_state(nbt.get_str("final_state").unwrap_or("minecraft:air"))?;
        if state.block() == Block::StructureVoid {
            return None;
        }
        Some(BlockInfo {
            pos: info.pos,
            state,
            nbt: None,
        })
    }
}

/// Parses `name[key=value,...]` the way jigsaw blocks store their final
/// state; properties left out keep their defaults.
pub fn parse_state(text: &str) -> Option<BlockState> {
    let (name, properties) = match text.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']')?),
        None => (text, ""),
    };
    let mut state = Block::from_name(&json::qualify(name.trim()))?.default_state();
    for pair in properties.split(',').filter(|p| !p.trim().is_empty()) {
        let (key, value) = pair.split_once('=')?;
        state = state.with(key.trim(), value.trim())?;
    }
    Some(state)
}

#[cfg(test)]
mod modifier_tests {
    use super::*;
    use serde_json::json;
    use tuff_item::random::XoroshiroRandom;

    #[tuff_core::watchdog]
    #[test]
    fn append_static_copies_or_merges_its_data() {
        let modifier = BlockEntityModifier::parse(&json!({
            "type": "minecraft:append_static",
            "data": {"a": 1, "inner": {"x": "new"}}
        }))
        .unwrap();
        let mut random = XoroshiroRandom::new(1);
        let fresh = modifier.apply(&mut random, None).unwrap();
        assert_eq!(fresh.get_int("a"), Some(1));
        let existing = Compound::new()
            .with("keep", 5)
            .with("inner", Compound::new().with("y", "old"));
        let merged = modifier.apply(&mut random, Some(existing)).unwrap();
        assert_eq!(merged.get_int("keep"), Some(5));
        assert_eq!(merged.get_int("a"), Some(1));
        let inner = merged.get_compound("inner").unwrap();
        assert_eq!(inner.get_str("x"), Some("new"));
        assert_eq!(inner.get_str("y"), Some("old"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn append_static_takes_snbt_and_rejects_non_compounds() {
        let ok = BlockEntityModifier::parse(&json!({
            "type": "minecraft:append_static",
            "data": "{CustomName:'x'}"
        }));
        assert!(ok.is_ok());
        let bad = BlockEntityModifier::parse(&json!({
            "type": "minecraft:append_static",
            "data": 3
        }));
        assert!(bad.is_err());
    }
}
