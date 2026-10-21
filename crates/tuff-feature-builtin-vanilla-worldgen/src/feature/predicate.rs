//! Block predicates (tests against the world at an offset) and rule tests
//! (tests against one block state, as ore targets and structure processors
//! use them).

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState, Support};

use crate::feature::level::Region;
use crate::json::{self, Result, obj};
use crate::tags::{BiomeSet, BlockSet};
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{biome_id, is_air};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fluid {
    Empty,
    Water,
    FlowingWater,
    Lava,
    FlowingLava,
}

impl Fluid {
    /// The fluid's id, `None` for no fluid.
    pub fn id(self) -> Option<&'static str> {
        match self {
            Fluid::Empty => None,
            Fluid::Water => Some("minecraft:water"),
            Fluid::FlowingWater => Some("minecraft:flowing_water"),
            Fluid::Lava => Some("minecraft:lava"),
            Fluid::FlowingLava => Some("minecraft:flowing_lava"),
        }
    }
}

pub fn fluid(state: BlockState) -> Fluid {
    match state.block() {
        Block::Water if state.get("level") == Some("0") => Fluid::Water,
        Block::Water => Fluid::FlowingWater,
        Block::Lava if state.get("level") == Some("0") => Fluid::Lava,
        Block::Lava => Fluid::FlowingLava,
        Block::BubbleColumn
        | Block::Seagrass
        | Block::TallSeagrass
        | Block::Kelp
        | Block::KelpPlant => Fluid::Water,
        _ if state.get("waterlogged") == Some("true") => Fluid::Water,
        _ => Fluid::Empty,
    }
}

fn parse_fluid(name: &str) -> Result<Fluid> {
    Ok(match json::qualify(name).as_str() {
        "minecraft:empty" => Fluid::Empty,
        "minecraft:water" => Fluid::Water,
        "minecraft:flowing_water" => Fluid::FlowingWater,
        "minecraft:lava" => Fluid::Lava,
        "minecraft:flowing_lava" => Fluid::FlowingLava,
        other => return Err(format!("unknown fluid {other}")),
    })
}

/// Every direction, in the game's order.
pub const DIRECTIONS: [Facing; 6] = [
    Facing::Down,
    Facing::Up,
    Facing::North,
    Facing::South,
    Facing::West,
    Facing::East,
];

pub fn parse_direction(name: &str) -> Result<Facing> {
    Ok(match name {
        "down" => Facing::Down,
        "up" => Facing::Up,
        "north" => Facing::North,
        "south" => Facing::South,
        "west" => Facing::West,
        "east" => Facing::East,
        other => return Err(format!("unknown direction {other}")),
    })
}

pub fn offset(pos: BlockPos, [x, y, z]: [i32; 3]) -> BlockPos {
    BlockPos::new(pos.x + x, pos.y + y, pos.z + z)
}

pub fn step(pos: BlockPos, direction: Facing) -> BlockPos {
    let (x, y, z) = direction.step();
    BlockPos::new(pos.x + x, pos.y + y, pos.z + z)
}

pub enum BlockPredicate {
    MatchingBlocks([i32; 3], BlockSet),
    MatchingBlockTag([i32; 3], BlockSet),
    MatchingFluids([i32; 3], Vec<Fluid>),
    MatchingBiomes(BiomeSet),
    HasSturdyFace([i32; 3], Facing),
    Solid([i32; 3]),
    Replaceable([i32; 3]),
    WouldSurvive([i32; 3], BlockState),
    InsideWorldBounds([i32; 3]),
    AnyOf(Vec<BlockPredicate>),
    AllOf(Vec<BlockPredicate>),
    Not(Box<BlockPredicate>),
    True,
    Unobstructed,
    HeightRange(
        tuff_vanilla_data::worldgen::VerticalAnchor,
        tuff_vanilla_data::worldgen::VerticalAnchor,
    ),
    VolumeMatch([i32; 3], [i32; 3], Box<BlockPredicate>),
}

fn offset_field(o: &json::Obj) -> Result<[i32; 3]> {
    match o.get("offset") {
        None => Ok([0, 0, 0]),
        Some(v) => json::vec3i(v),
    }
}

impl BlockPredicate {
    pub fn parse(value: &Value) -> Result<Self> {
        let kind = json::qualify(json::kind(value)?);
        let what = "block predicate";
        Ok(match kind.as_str() {
            "minecraft:matching_blocks" => {
                let o = obj(value, &["offset", "blocks"], what)?;
                Self::MatchingBlocks(offset_field(&o)?, json::block_set(o.req("blocks")?)?)
            }
            "minecraft:matching_block_tag" => {
                let o = obj(value, &["offset", "tag"], what)?;
                Self::MatchingBlockTag(offset_field(&o)?, json::tag_blocks(o.str("tag")?)?)
            }
            "minecraft:matching_fluids" => {
                let o = obj(value, &["offset", "fluids"], what)?;
                let fluids = match o.req("fluids")? {
                    Value::String(name) => vec![parse_fluid(name)?],
                    Value::Array(list) => list
                        .iter()
                        .map(|v| parse_fluid(v.as_str().ok_or("fluid names")?))
                        .collect::<Result<_>>()?,
                    other => return Err(format!("bad fluid list {other}")),
                };
                Self::MatchingFluids(offset_field(&o)?, fluids)
            }
            "minecraft:matching_biomes" => {
                let o = obj(value, &["biomes"], what)?;
                let names: Vec<String> = match o.req("biomes")? {
                    Value::String(name) if name.starts_with('#') => {
                        let tag = json::qualify(&name[1..]);
                        return BiomeSet::tag(&tag)
                            .map(Self::MatchingBiomes)
                            .ok_or_else(|| format!("unknown biome tag {tag}"));
                    }
                    Value::String(name) => vec![json::qualify(name)],
                    Value::Array(list) => list
                        .iter()
                        .map(|v| v.as_str().map(json::qualify).ok_or("biome names"))
                        .collect::<std::result::Result<_, _>>()?,
                    other => return Err(format!("bad biome list {other}")),
                };
                Self::MatchingBiomes(BiomeSet::Fixed(
                    names.iter().filter_map(|n| biome_id(n)).collect(),
                ))
            }
            "minecraft:has_sturdy_face" => {
                let o = obj(value, &["offset", "direction"], what)?;
                Self::HasSturdyFace(offset_field(&o)?, parse_direction(o.str("direction")?)?)
            }
            "minecraft:solid" => Self::Solid(offset_field(&obj(value, &["offset"], what)?)?),
            "minecraft:replaceable" => {
                Self::Replaceable(offset_field(&obj(value, &["offset"], what)?)?)
            }
            "minecraft:would_survive" => {
                let o = obj(value, &["offset", "state"], what)?;
                Self::WouldSurvive(offset_field(&o)?, json::block_state(o.req("state")?)?)
            }
            "minecraft:inside_world_bounds" => {
                Self::InsideWorldBounds(offset_field(&obj(value, &["offset"], what)?)?)
            }
            "minecraft:any_of" | "minecraft:all_of" => {
                let o = obj(value, &["predicates"], what)?;
                let list = o
                    .list("predicates")?
                    .iter()
                    .map(Self::parse)
                    .collect::<Result<Vec<_>>>()?;
                if kind == "minecraft:any_of" {
                    Self::AnyOf(list)
                } else {
                    Self::AllOf(list)
                }
            }
            "minecraft:not" => {
                let o = obj(value, &["predicate"], what)?;
                Self::Not(Box::new(Self::parse(o.req("predicate")?)?))
            }
            "minecraft:true" => {
                obj(value, &[], what)?;
                Self::True
            }
            "minecraft:unobstructed" => {
                obj(value, &["offset"], what)?;
                Self::Unobstructed
            }
            "minecraft:height_range" => {
                let o = obj(value, &["min_inclusive", "max_inclusive"], what)?;
                Self::HeightRange(
                    json::anchor(o.req("min_inclusive")?)?,
                    json::anchor(o.req("max_inclusive")?)?,
                )
            }
            "minecraft:volume_match" => {
                let o = obj(value, &["min", "max", "match"], what)?;
                Self::VolumeMatch(
                    json::vec3i(o.req("min")?)?,
                    json::vec3i(o.req("max")?)?,
                    Box::new(Self::parse(o.req("match")?)?),
                )
            }
            other => return Err(format!("unmodeled block predicate {other}")),
        })
    }

    pub fn test(&self, level: &mut Region, pos: BlockPos) -> bool {
        match self {
            Self::MatchingBlocks(o, set) | Self::MatchingBlockTag(o, set) => {
                set.contains_state(level.block(offset(pos, *o)))
            }
            Self::MatchingFluids(o, fluids) => {
                fluids.contains(&fluid(level.block(offset(pos, *o))))
            }
            Self::MatchingBiomes(biomes) => biomes.contains(level.biome(pos)),
            Self::HasSturdyFace(o, direction) => level
                .block(offset(pos, *o))
                .is_face_sturdy(*direction, Support::Full),
            Self::Solid(o) => level.block(offset(pos, *o)).is_solid(),
            Self::Replaceable(o) => level.block(offset(pos, *o)).is_replaceable(),
            Self::WouldSurvive(o, state) => {
                let target = offset(pos, *o);
                level.can_survive(*state, target)
            }
            Self::InsideWorldBounds(o) => !level.is_outside_build_height(offset(pos, *o).y),
            Self::AnyOf(list) => list.iter().any(|p| p.test(level, pos)),
            Self::AllOf(list) => list.iter().all(|p| p.test(level, pos)),
            Self::Not(inner) => !inner.test(level, pos),
            Self::True | Self::Unobstructed => true,
            Self::HeightRange(min, max) => {
                let (min, max) = (level.resolve(*min), level.resolve(*max));
                pos.y >= min && pos.y <= max
            }
            Self::VolumeMatch(min, max, inner) => {
                for ox in min[0]..=max[0] {
                    for oz in min[2]..=max[2] {
                        for oy in min[1]..=max[1] {
                            if !inner.test(level, offset(pos, [ox, oy, oz])) {
                                return false;
                            }
                        }
                    }
                }
                true
            }
        }
    }
}

/// Tests of a single block state, with the position and a random source.
pub enum RuleTest {
    AlwaysTrue,
    Block(Block),
    BlockState(BlockState),
    Tag(BlockSet),
    RandomBlock(Block, f32),
    RandomBlockState(BlockState, f32),
    Height(i32, i32),
    AllOf(Vec<RuleTest>),
    AnyOf(Vec<RuleTest>),
    Not(Box<RuleTest>),
}

impl RuleTest {
    pub fn parse(value: &Value) -> Result<Self> {
        let kind = value
            .get("predicate_type")
            .and_then(Value::as_str)
            .ok_or("rule tests have a predicate_type")?;
        let what = "rule test";
        let with = |fields: &[&str]| -> Result<json::Obj> {
            let mut all = fields.to_vec();
            all.push("predicate_type");
            obj(value, &all, what)
        };
        Ok(match json::qualify(kind).as_str() {
            "minecraft:always_true" => {
                with(&[])?;
                Self::AlwaysTrue
            }
            "minecraft:block_match" => Self::Block(json::block(with(&["block"])?.req("block")?)?),
            "minecraft:blockstate_match" => Self::BlockState(json::block_state(
                with(&["block_state"])?.req("block_state")?,
            )?),
            "minecraft:tag_match" => Self::Tag(json::tag_blocks(with(&["tag"])?.str("tag")?)?),
            "minecraft:random_block_match" => {
                let o = with(&["block", "probability"])?;
                Self::RandomBlock(json::block(o.req("block")?)?, o.f32("probability")?)
            }
            "minecraft:random_blockstate_match" => {
                let o = with(&["block_state", "probability"])?;
                Self::RandomBlockState(
                    json::block_state(o.req("block_state")?)?,
                    o.f32("probability")?,
                )
            }
            "minecraft:height_match" => {
                let o = with(&["min_inclusive", "max_inclusive"])?;
                Self::Height(o.i32("min_inclusive")?, o.i32("max_inclusive")?)
            }
            "minecraft:all_of" | "minecraft:any_of" => {
                let o = with(&["rules"])?;
                let list = o
                    .list("rules")?
                    .iter()
                    .map(Self::parse)
                    .collect::<Result<Vec<_>>>()?;
                if kind.ends_with("all_of") {
                    Self::AllOf(list)
                } else {
                    Self::AnyOf(list)
                }
            }
            "minecraft:not" => Self::Not(Box::new(Self::parse(with(&["rule"])?.req("rule")?)?)),
            other => return Err(format!("unmodeled rule test {other}")),
        })
    }

    pub fn test(
        &self,
        state: BlockState,
        pos: BlockPos,
        random: &mut (impl RandomSource + ?Sized),
    ) -> bool {
        match self {
            Self::AlwaysTrue => true,
            Self::Block(block) => state.block() == *block,
            Self::BlockState(expected) => state == *expected,
            Self::Tag(set) => set.contains_state(state),
            Self::RandomBlock(block, p) => state.block() == *block && random.next_float() < *p,
            Self::RandomBlockState(expected, p) => state == *expected && random.next_float() < *p,
            Self::Height(min, max) => *min <= pos.y && pos.y <= *max,
            Self::AllOf(list) => list.iter().all(|t| t.test(state, pos, random)),
            Self::AnyOf(list) => list.iter().any(|t| t.test(state, pos, random)),
            Self::Not(inner) => !inner.test(state, pos, random),
        }
    }
}

pub fn is_air_state(state: BlockState) -> bool {
    is_air(state)
}
