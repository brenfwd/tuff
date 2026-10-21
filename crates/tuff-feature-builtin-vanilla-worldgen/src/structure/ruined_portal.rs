//! Ruined portals: one template piece, sunk to a height the setup picks,
//! with netherrack spread around it and optional vines and leaves.

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::Structure;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::json::{Result, obj};
use crate::proto::HeightmapKind;
use crate::structure::geometry::{BoundingBox, Mirror, Rotation};
use crate::structure::processor::{
    BlackstoneReplace, BlockAge, BlockIgnore, LavaSubmerged, ProcessorList, ProcessorRule,
    Protected, RuleProcessor, facing_name, parse_state,
};
use crate::structure::template::{self, PlaceRandom, PlaceSettings, Template};
use crate::structure::{GenerationContext, GenerationStub, Piece, PieceContext, StructureKind};
use crate::tags::BlockSet;
use tuff_item::random::RandomSource;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalPlacement {
    OnLandSurface,
    PartlyBuried,
    OnOceanFloor,
    InMountain,
    Underground,
    InNether,
}

impl VerticalPlacement {
    fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "on_land_surface" => Self::OnLandSurface,
            "partly_buried" => Self::PartlyBuried,
            "on_ocean_floor" => Self::OnOceanFloor,
            "in_mountain" => Self::InMountain,
            "underground" => Self::Underground,
            "in_nether" => Self::InNether,
            other => return Err(format!("unknown ruined portal placement {other}")),
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::OnLandSurface => "on_land_surface",
            Self::PartlyBuried => "partly_buried",
            Self::OnOceanFloor => "on_ocean_floor",
            Self::InMountain => "in_mountain",
            Self::Underground => "underground",
            Self::InNether => "in_nether",
        }
    }

    fn heightmap(self) -> HeightmapKind {
        if self == Self::OnOceanFloor {
            HeightmapKind::OceanFloorWg
        } else {
            HeightmapKind::WorldSurfaceWg
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Setup {
    placement: VerticalPlacement,
    air_pocket_probability: f32,
    mossiness: f32,
    overgrown: bool,
    vines: bool,
    can_be_cold: bool,
    replace_with_blackstone: bool,
    weight: f32,
}

/// What the piece keeps of its setup.
#[derive(Debug, Clone, Copy)]
pub struct Properties {
    pub cold: bool,
    pub mossiness: f32,
    pub air_pocket: bool,
    pub overgrown: bool,
    pub vines: bool,
    pub replace_with_blackstone: bool,
}

struct RuinedPortal {
    setups: Vec<Setup>,
}

pub fn factory(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    let o = obj(config, &["setups"], "ruined portal")?;
    let setups = o
        .list("setups")?
        .iter()
        .map(|setup| {
            let s = obj(
                setup,
                &[
                    "placement",
                    "air_pocket_probability",
                    "mossiness",
                    "overgrown",
                    "vines",
                    "can_be_cold",
                    "replace_with_blackstone",
                    "weight",
                ],
                "ruined portal setup",
            )?;
            Ok(Setup {
                placement: VerticalPlacement::parse(s.str("placement")?)?,
                air_pocket_probability: s.f32("air_pocket_probability")?,
                mossiness: s.f32("mossiness")?,
                overgrown: s.bool("overgrown")?,
                vines: s.bool("vines")?,
                can_be_cold: s.bool("can_be_cold")?,
                replace_with_blackstone: s.bool("replace_with_blackstone")?,
                weight: s.f32("weight")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if setups.is_empty() {
        return Err("ruined portals need a setup".into());
    }
    Ok(Arc::new(RuinedPortal { setups }))
}

fn sample(random: &mut impl RandomSource, limit: f32) -> bool {
    if limit == 0.0 {
        return false;
    }
    limit == 1.0 || random.next_float() < limit
}

fn random_within(random: &mut impl RandomSource, min_preferred: i32, max: i32) -> i32 {
    if min_preferred < max {
        random.next_int_between_inclusive(min_preferred, max)
    } else {
        max
    }
}

impl StructureKind for RuinedPortal {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let random = &mut ctx.random;
        let setup = if self.setups.len() > 1 {
            let total: f32 = self.setups.iter().fold(0.0, |sum, s| sum + s.weight);
            let mut pick = random.next_float();
            let mut chosen = None;
            for s in &self.setups {
                pick -= s.weight / total;
                if pick < 0.0 {
                    chosen = Some(*s);
                    break;
                }
            }
            chosen?
        } else {
            self.setups[0]
        };
        let air_pocket = sample(random, setup.air_pocket_probability);
        let name = if random.next_float() < 0.05 {
            let list = tuff_vanilla_data::world::GIANT_RUINED_PORTALS;
            list[random.next_int_bounded(list.len() as i32) as usize]
        } else {
            let list = tuff_vanilla_data::world::RUINED_PORTALS;
            list[random.next_int_bounded(list.len() as i32) as usize]
        };
        let template = template::get(name);
        let rotation = Rotation::ALL[random.next_int_bounded(4) as usize];
        let mirror = if random.next_float() < 0.5 {
            Mirror::None
        } else {
            Mirror::FrontBack
        };
        let pivot = BlockPos::new(template.size[0] / 2, 0, template.size[2] / 2);
        let base = BlockPos::new(ctx.chunk.x << 4, 0, ctx.chunk.z << 4);
        let bounding_box = template.bounding_box(base, rotation, pivot, mirror);
        let center = bounding_box.center();
        let surface_y = ctx.base_height(center.x, center.z, setup.placement.heightmap()) - 1;
        let projected_y =
            find_suitable_y(ctx, setup.placement, air_pocket, surface_y, &bounding_box);
        let origin = BlockPos::new(base.x, projected_y, base.z);
        Some(GenerationStub {
            position: origin,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let cold = setup.can_be_cold && {
                    let biome = ctx.biome(origin.x >> 2, origin.y >> 2, origin.z >> 2);
                    crate::temperature::cold_enough_to_snow(
                        biome,
                        origin.x,
                        origin.y,
                        origin.z,
                        ctx.sea_level(),
                    )
                };
                let properties = Properties {
                    cold,
                    mossiness: setup.mossiness,
                    air_pocket,
                    overgrown: setup.overgrown,
                    vines: setup.vines,
                    replace_with_blackstone: setup.replace_with_blackstone,
                };
                vec![Box::new(RuinedPortalPiece::new(
                    origin,
                    setup.placement,
                    properties,
                    name,
                    template,
                    rotation,
                    mirror,
                    pivot,
                )) as Box<dyn Piece>]
            }),
        })
    }
}

fn find_suitable_y(
    ctx: &mut GenerationContext,
    placement: VerticalPlacement,
    air_pocket: bool,
    surface_y: i32,
    bounding_box: &BoundingBox,
) -> i32 {
    let min_y = ctx.shape.min_y + 15;
    let y_span = bounding_box.y_span();
    let random = &mut ctx.random;
    let new_y = match placement {
        VerticalPlacement::InNether => {
            if air_pocket {
                random.next_int_between_inclusive(32, 100)
            } else if random.next_float() < 0.5 {
                random.next_int_between_inclusive(27, 29)
            } else {
                random.next_int_between_inclusive(29, 100)
            }
        }
        VerticalPlacement::InMountain => random_within(random, 70, surface_y - y_span),
        VerticalPlacement::Underground => random_within(random, min_y, surface_y - y_span),
        VerticalPlacement::PartlyBuried => {
            surface_y - y_span + random.next_int_between_inclusive(2, 8)
        }
        VerticalPlacement::OnLandSurface | VerticalPlacement::OnOceanFloor => surface_y,
    };
    let corners = [
        (bounding_box.min_x, bounding_box.min_z),
        (bounding_box.max_x, bounding_box.min_z),
        (bounding_box.min_x, bounding_box.max_z),
        (bounding_box.max_x, bounding_box.max_z),
    ];
    let columns: Vec<(i32, Vec<BlockState>)> = corners
        .iter()
        .map(|&(x, z)| ctx.base_column(x, z))
        .collect();
    let heightmap = if placement == VerticalPlacement::OnOceanFloor {
        HeightmapKind::OceanFloorWg
    } else {
        HeightmapKind::WorldSurfaceWg
    };
    let mut y = new_y;
    'search: while y > min_y {
        let mut solid = 0;
        for (bottom, column) in &columns {
            let state = usize::try_from(y - bottom)
                .ok()
                .and_then(|i| column.get(i))
                .copied()
                .unwrap_or(Block::Air.default_state());
            if heightmap.is_opaque(state) {
                solid += 1;
                if solid == 3 {
                    break 'search;
                }
            }
        }
        y -= 1;
    }
    y
}

const MAGMA_CHANCE: f32 = 0.07;
const TRICKLE_MAX_DEPTH: i32 = 8;
const TRICKLE_CONTINUE: f32 = 0.5;
const CRUST_FALLOFF: [f32; 14] = [
    1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.9, 0.9, 0.8, 0.7, 0.6, 0.4, 0.2,
];

pub struct RuinedPortalPiece {
    template_name: &'static str,
    template: Arc<Template>,
    position: BlockPos,
    settings: PlaceSettings,
    placement: VerticalPlacement,
    properties: Properties,
    bounding_box: BoundingBox,
}

impl RuinedPortalPiece {
    #[allow(clippy::too_many_arguments)]
    fn new(
        position: BlockPos,
        placement: VerticalPlacement,
        properties: Properties,
        template_name: &'static str,
        template: Arc<Template>,
        rotation: Rotation,
        mirror: Mirror,
        pivot: BlockPos,
    ) -> Self {
        let settings = settings(mirror, rotation, placement, pivot, &properties);
        let bounding_box = template.settings_box(&settings, position);
        Self {
            template_name,
            template,
            position,
            settings,
            placement,
            properties,
            bounding_box,
        }
    }

    fn features_cannot_replace() -> BlockSet {
        BlockSet::Tag(tuff_vanilla_data::tag::block::key::FEATURES_CANNOT_REPLACE)
    }

    fn can_be_replaced(&self, state: BlockState) -> bool {
        !(state.block() == Block::Air
            || state.block() == Block::Obsidian
            || crate::tags::features_cannot_replace().contains_state(state)
            || (self.placement != VerticalPlacement::InNether && state.block() == Block::Lava))
    }

    fn lay_crust(&self, random: &mut impl RandomSource, level: &mut Region, pos: BlockPos) {
        if !self.can_be_replaced(level.block(pos)) {
            return;
        }
        let magma = !self.properties.cold && random.next_float() < MAGMA_CHANCE;
        let block = if magma {
            Block::MagmaBlock
        } else {
            Block::Netherrack
        };
        level.set_block(pos, block.default_state());
    }

    fn trickle_down(&self, random: &mut impl RandomSource, level: &mut Region, top: BlockPos) {
        for depth in 0..=TRICKLE_MAX_DEPTH {
            self.lay_crust(random, level, BlockPos::new(top.x, top.y - depth, top.z));
            if depth == TRICKLE_MAX_DEPTH || random.next_float() >= TRICKLE_CONTINUE {
                break;
            }
        }
    }

    fn trickle_under_floor(&self, random: &mut impl RandomSource, level: &mut Region) {
        let bounds = self.bounding_box;
        let floor = bounds.min_y;
        for x in bounds.min_x + 1..bounds.max_x {
            for z in bounds.min_z + 1..bounds.max_z {
                if level.block(BlockPos::new(x, floor, z)).block() == Block::Netherrack {
                    self.trickle_down(random, level, BlockPos::new(x, floor - 1, z));
                }
            }
        }
    }

    fn scatter_crust(&self, random: &mut impl RandomSource, level: &mut Region) {
        let reach = CRUST_FALLOFF.len() as i32;
        let bounds = self.bounding_box;
        let mid = bounds.center();
        let half_width = (bounds.x_span() + bounds.z_span()) / 4;
        let shift = random.next_int_bounded((8 - half_width).max(1));
        let hugs_surface = matches!(
            self.placement,
            VerticalPlacement::OnLandSurface | VerticalPlacement::OnOceanFloor
        );
        let heightmap = self.placement.heightmap();
        for dx in -reach..=reach {
            for dz in -reach..=reach {
                let ring = (dx.abs() + dz.abs() + shift).max(0) as usize;
                let Some(&chance) = CRUST_FALLOFF.get(ring) else {
                    continue;
                };
                if random.next_double() >= f64::from(chance) {
                    continue;
                }
                let (x, z) = (mid.x + dx, mid.z + dz);
                let ground = level.height(heightmap, x, z) - 1;
                let y = if hugs_surface {
                    ground
                } else {
                    ground.min(bounds.min_y)
                };
                if (y - bounds.min_y).abs() > 3 {
                    continue;
                }
                let spot = BlockPos::new(x, y, z);
                if !self.can_be_replaced(level.block(spot)) {
                    continue;
                }
                self.lay_crust(random, level, spot);
                if self.properties.overgrown {
                    maybe_add_leaves_above(random, level, spot);
                }
                self.trickle_down(random, level, BlockPos::new(x, y - 1, z));
            }
        }
    }
}

fn maybe_add_vines(random: &mut impl RandomSource, level: &mut Region, pos: BlockPos) {
    let state = level.block(pos);
    if is_air(state) || state.block() == Block::Vine {
        return;
    }
    let direction = Facing::HORIZONTAL[random.next_int_bounded(4) as usize];
    let (dx, dy, dz) = direction.step();
    let neighbour = BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz);
    if !is_air(level.block(neighbour)) || !state.is_collision_face_full(direction) {
        return;
    }
    let vine = Block::Vine
        .default_state()
        .with(facing_name(direction.opposite()), "true")
        .expect("vines face every horizontal side");
    level.set_block(neighbour, vine);
}

fn maybe_add_leaves_above(random: &mut impl RandomSource, level: &mut Region, pos: BlockPos) {
    let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    if random.next_float() < 0.5
        && level.block(pos).block() == Block::Netherrack
        && is_air(level.block(above))
    {
        let leaves = Block::JungleLeaves
            .default_state()
            .with("persistent", "true")
            .expect("leaves can persist");
        level.set_block(above, leaves);
    }
}

fn settings(
    mirror: Mirror,
    rotation: Rotation,
    placement: VerticalPlacement,
    pivot: BlockPos,
    properties: &Properties,
) -> PlaceSettings {
    let mut processors = ProcessorList::default();
    processors.push(if properties.air_pocket {
        BlockIgnore::structure_block()
    } else {
        BlockIgnore::structure_and_air()
    });
    let mut rules = vec![ProcessorRule::replace(
        Block::GoldBlock,
        Some(0.3),
        Block::Air,
    )];
    rules.push(if placement == VerticalPlacement::OnOceanFloor {
        ProcessorRule::replace(Block::Lava, None, Block::MagmaBlock)
    } else if properties.cold {
        ProcessorRule::replace(Block::Lava, None, Block::Netherrack)
    } else {
        ProcessorRule::replace(Block::Lava, Some(0.2), Block::MagmaBlock)
    });
    if !properties.cold {
        rules.push(ProcessorRule::replace(
            Block::Netherrack,
            Some(0.07),
            Block::MagmaBlock,
        ));
    }
    processors.push(RuleProcessor(rules));
    processors.push(BlockAge {
        mossiness: properties.mossiness,
    });
    processors.push(Protected(RuinedPortalPiece::features_cannot_replace()));
    processors.push(LavaSubmerged);
    if properties.replace_with_blackstone {
        processors.push(BlackstoneReplace);
    }
    PlaceSettings {
        mirror,
        rotation,
        pivot,
        processors,
        ..PlaceSettings::default()
    }
}

impl Piece for RuinedPortalPiece {
    fn kind(&self) -> PieceType {
        PieceType::Rupo
    }

    fn bounding_box(&self) -> BoundingBox {
        self.bounding_box
    }

    fn orientation(&self) -> Option<Facing> {
        Some(Facing::North)
    }

    fn save_extra(&self, tag: &mut Compound) {
        tag.insert("TPX", self.position.x);
        tag.insert("TPY", self.position.y);
        tag.insert("TPZ", self.position.z);
        tag.insert("Template", self.template_name);
        tag.insert("Rotation", self.settings.rotation.name());
        tag.insert("Mirror", self.settings.mirror.name());
        tag.insert("VerticalPlacement", self.placement.name());
        let p = self.properties;
        tag.insert(
            "Properties",
            Compound::new()
                .with("cold", p.cold)
                .with("mossiness", p.mossiness)
                .with("air_pocket", p.air_pocket)
                .with("overgrown", p.overgrown)
                .with("vines", p.vines)
                .with("replace_with_blackstone", p.replace_with_blackstone),
        );
    }

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        reference: BlockPos,
    ) {
        let bounding_box = self.bounding_box;
        if !chunk_box.contains(bounding_box.center()) {
            return;
        }
        chunk_box.encapsulate(&bounding_box);
        place_template(
            ctx,
            &self.template,
            self.position,
            reference,
            &self.settings,
            *chunk_box,
        );
        let random = &mut *ctx.random;
        let level = &mut *ctx.level;
        self.scatter_crust(random, level);
        self.trickle_under_floor(random, level);
        if self.properties.vines || self.properties.overgrown {
            for pos in bounding_box.positions() {
                if self.properties.vines {
                    maybe_add_vines(random, level, pos);
                }
                if self.properties.overgrown {
                    maybe_add_leaves_above(random, level, pos);
                }
            }
        }
    }
}

/// A template piece's placement: the template, then its jigsaw blocks
/// turned into their final states.
pub fn place_template(
    ctx: &mut PieceContext,
    template: &Template,
    position: BlockPos,
    reference: BlockPos,
    settings: &PlaceSettings,
    chunk_box: BoundingBox,
) {
    place_template_with_markers(
        ctx,
        template,
        position,
        reference,
        settings,
        chunk_box,
        |_, _, _| {},
    );
}

/// [`place_template`], handing each data marker (a structure block in data
/// mode, by its metadata and position) to `marker` before the jigsaws.
pub fn place_template_with_markers(
    ctx: &mut PieceContext,
    template: &Template,
    position: BlockPos,
    reference: BlockPos,
    settings: &PlaceSettings,
    chunk_box: BoundingBox,
    mut marker: impl FnMut(&mut PieceContext, &str, BlockPos),
) {
    let mut settings = settings.clone();
    settings.bounding_box = Some(chunk_box);
    let mut random = PlaceRandom::positional(&mut *ctx.random);
    if !template.place(ctx.level, position, reference, &settings, &mut random) {
        return;
    }
    let markers = template.filter_blocks(position, &settings, &mut random, Block::StructureBlock);
    for data in markers {
        let Some(nbt) = &data.nbt else {
            continue;
        };
        if nbt.get_str("mode") == Some("DATA") {
            let metadata = nbt.get_str("metadata").unwrap_or("").to_string();
            marker(ctx, &metadata, data.pos);
        }
    }
    let mut random = PlaceRandom::positional(&mut *ctx.random);
    for jigsaw in template.filter_blocks(position, &settings, &mut random, Block::Jigsaw) {
        let Some(nbt) = &jigsaw.nbt else {
            continue;
        };
        let state = nbt
            .get_str("final_state")
            .map_or(Some(Block::Air.default_state()), parse_state)
            .unwrap_or(Block::Air.default_state());
        ctx.level.set_block(jigsaw.pos, state);
    }
}
