//! Structures made of whole templates placed where the terrain is:
//! shipwrecks, igloos and ocean ruins.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::Structure;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::{Fluid, fluid};
use crate::json::{Result, obj};
use crate::proto::HeightmapKind;
use crate::structure::geometry::{BoundingBox, Mirror, Rotation, transform};
use crate::structure::piece::structure_loot;
use crate::structure::processor::{BlockIgnore, ProcessorList, parse_processor};
use crate::structure::ruined_portal::place_template_with_markers;
use crate::structure::template::{self, PlaceSettings, Template};
use crate::structure::{GenerationContext, GenerationStub, Piece, PieceContext, StructureKind};
use tuff_item::random::RandomSource;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

/// The templates dumped for a structure use, in order.
fn templates(use_: &str) -> Vec<&'static str> {
    let list: Vec<&'static str> = tuff_vanilla_data::world::STRUCTURE_TEMPLATES
        .iter()
        .filter(|(u, _)| *u == use_)
        .map(|(_, t)| *t)
        .collect();
    assert!(!list.is_empty(), "no structure templates for {use_}");
    list
}

fn random_rotation(random: &mut dyn RandomSource) -> Rotation {
    Rotation::ALL[random.next_int_bounded(4) as usize]
}

fn offset(pos: BlockPos, dx: i32, dy: i32, dz: i32) -> BlockPos {
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

/// Sets the loot table of the container at `pos`, if it holds one.
fn set_container_loot(
    level: &mut Region,
    random: &mut dyn RandomSource,
    pos: BlockPos,
    table: &str,
) {
    if !level.block(pos).block().has_loot_container() {
        return;
    }
    let seed = random.next_long();
    if let Some(data) = level.block_entity_mut(pos) {
        data.insert("LootTable", table);
        data.insert("LootTableSeed", seed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Variant {
    Shipwreck {
        beached: bool,
    },
    Igloo {
        top: bool,
        offset: [i32; 3],
    },
    OceanRuin {
        integrity: f32,
        warm: bool,
        large: bool,
    },
}

/// Where the template is and the box it fills, both moved by placement.
struct Placement {
    position: BlockPos,
    bounding_box: BoundingBox,
    height_adjusted: bool,
}

struct TemplatePiece {
    variant: Variant,
    name: &'static str,
    template: Arc<Template>,
    settings: PlaceSettings,
    placement: Mutex<Placement>,
}

impl TemplatePiece {
    fn new(
        variant: Variant,
        name: &'static str,
        settings: PlaceSettings,
        position: BlockPos,
    ) -> Self {
        let template = template::get(name);
        let bounding_box = template.settings_box(&settings, position);
        Self {
            variant,
            name,
            template,
            settings,
            placement: Mutex::new(Placement {
                position,
                bounding_box,
                height_adjusted: false,
            }),
        }
    }

    fn too_big(&self) -> bool {
        self.template.size[0] > 32 || self.template.size[1] > 32
    }

    fn beached_height(&self, min_y: i32, random: &mut dyn RandomSource) -> i32 {
        min_y - self.template.size[1] / 2 - random.next_int_bounded(3)
    }

    /// Places the template at `position`, then settles the box there.
    fn place_at(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &BoundingBox,
        position: BlockPos,
        reference: BlockPos,
    ) {
        self.placement.lock().unwrap().bounding_box =
            self.template.settings_box(&self.settings, position);
        let variant = self.variant;
        place_template_with_markers(
            ctx,
            &self.template,
            position,
            reference,
            &self.settings,
            *chunk_box,
            |ctx, marker, pos| handle_marker(variant, ctx, marker, pos),
        );
    }

    /// The ocean ruin's floor: the lowest solid block below its corners'
    /// span, used when enough of the footprint hangs over a drop.
    fn ruin_height(level: &Region, pos: BlockPos, corner: BlockPos) -> i32 {
        let mut new_y = pos.y;
        let mut min_y = 512;
        let top_y = new_y - 1;
        let mut area = 0;
        for x in pos.x.min(corner.x)..=pos.x.max(corner.x) {
            for z in pos.z.min(corner.z)..=pos.z.max(corner.z) {
                let mut floor_y = pos.y - 1;
                loop {
                    let state = level.block(BlockPos::new(x, floor_y, z));
                    let open = is_air(state)
                        || matches!(fluid(state), Fluid::Water | Fluid::FlowingWater)
                        || tuff_datapack::tags::is(
                            tuff_vanilla_data::tag::block::key::ICE,
                            state.block(),
                        );
                    if !open || floor_y <= level.min_y() + 1 {
                        break;
                    }
                    floor_y -= 1;
                }
                min_y = min_y.min(floor_y);
                if floor_y < top_y - 2 {
                    area += 1;
                }
            }
        }
        let width = (pos.x - corner.x).abs();
        if top_y - min_y > 2 && area > width - 2 {
            new_y = min_y + 1;
        }
        new_y
    }
}

fn handle_marker(variant: Variant, ctx: &mut PieceContext, marker: &str, pos: BlockPos) {
    let below = offset(pos, 0, -1, 0);
    match variant {
        Variant::Shipwreck { .. } => {
            if let Some((_, _, table)) = tuff_vanilla_data::world::STRUCTURE_MARKERS
                .iter()
                .find(|(u, m, _)| *u == "shipwreck" && *m == marker)
            {
                set_container_loot(ctx.level, &mut *ctx.random, below, table);
            }
        }
        Variant::Igloo { .. } => {
            if marker == "chest" {
                ctx.level.set_block(pos, Block::Air.default_state());
                if ctx.level.block(below).block() == Block::Chest {
                    set_container_loot(ctx.level, &mut *ctx.random, below, structure_loot("igloo"));
                }
            }
        }
        Variant::OceanRuin { large, .. } => match marker {
            "chest" => {
                let water = matches!(
                    fluid(ctx.level.block(pos)),
                    Fluid::Water | Fluid::FlowingWater
                );
                let chest = Block::Chest.default_state();
                let chest = chest
                    .with("waterlogged", if water { "true" } else { "false" })
                    .unwrap_or(chest);
                ctx.level.set_block(pos, chest);
                let table = structure_loot(if large {
                    "underwater_ruin_big"
                } else {
                    "underwater_ruin_small"
                });
                ctx.level.set_block_entity(
                    pos,
                    Compound::new()
                        .with("id", "minecraft:chest")
                        .with("LootTable", table)
                        .with("LootTableSeed", ctx.random.next_long()),
                );
            }
            "drowned" => {
                ctx.level
                    .add_entity(pos, super::structure_mob("minecraft:drowned", pos, true));
                let state = if pos.y > ctx.level.sea_level() {
                    Block::Air.default_state()
                } else {
                    Block::Water.default_state()
                };
                ctx.level.set_block(pos, state);
            }
            _ => {}
        },
    }
}

impl Piece for TemplatePiece {
    fn kind(&self) -> PieceType {
        match self.variant {
            Variant::Shipwreck { .. } => PieceType::Shipwreck,
            Variant::Igloo { .. } => PieceType::Iglu,
            Variant::OceanRuin { .. } => PieceType::Orp,
        }
    }

    fn bounding_box(&self) -> BoundingBox {
        self.placement.lock().unwrap().bounding_box
    }

    fn orientation(&self) -> Option<Facing> {
        Some(Facing::North)
    }

    fn load_progress(&self, tag: &Compound) {
        if !matches!(self.variant, Variant::Shipwreck { .. }) {
            return;
        }
        let mut placement = self.placement.lock().unwrap();
        if !placement.height_adjusted
            && tag.get_bool("height_adjusted") == Some(true)
            && let Some(y) = tag.get_int("TPY")
        {
            placement.height_adjusted = true;
            placement.position.y = y;
        }
    }

    fn save_extra(&self, tag: &mut Compound) {
        let placement = self.placement.lock().unwrap();
        tag.insert("TPX", placement.position.x);
        tag.insert("TPY", placement.position.y);
        tag.insert("TPZ", placement.position.z);
        tag.insert("Template", self.name);
        match self.variant {
            Variant::Shipwreck { beached } => {
                tag.insert("isBeached", beached);
                tag.insert("Rot", self.settings.rotation.name());
                tag.insert("height_adjusted", placement.height_adjusted);
            }
            Variant::Igloo { .. } => {
                tag.insert("Rot", self.settings.rotation.name());
            }
            Variant::OceanRuin {
                integrity,
                warm,
                large,
            } => {
                tag.insert("Rot", self.settings.rotation.name());
                tag.insert("Integrity", integrity);
                tag.insert("BiomeType", if warm { "WARM" } else { "COLD" });
                tag.insert("IsLarge", large);
            }
        }
    }

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        reference: BlockPos,
    ) {
        let chunk_box = *chunk_box;
        match self.variant {
            Variant::Shipwreck { beached } => {
                let position = {
                    let mut placement = self.placement.lock().unwrap();
                    if !placement.height_adjusted && !self.too_big() {
                        let kind = if beached {
                            HeightmapKind::WorldSurfaceWg
                        } else {
                            HeightmapKind::OceanFloorWg
                        };
                        let [sx, _, sz] = self.template.size;
                        let origin = placement.position;
                        let height = if sx * sz == 0 {
                            ctx.level.height(kind, origin.x, origin.z)
                        } else {
                            let (mut total, mut min_y) = (0, ctx.level.max_y() + 1);
                            for x in origin.x..origin.x + sx {
                                for z in origin.z..origin.z + sz {
                                    let h = ctx.level.height(kind, x, z);
                                    total += h;
                                    min_y = min_y.min(h);
                                }
                            }
                            if beached {
                                self.beached_height(min_y, &mut *ctx.random)
                            } else {
                                total / (sx * sz)
                            }
                        };
                        placement.height_adjusted = true;
                        placement.position.y = height;
                    }
                    placement.position
                };
                self.place_at(ctx, &chunk_box, position, reference);
            }
            Variant::Igloo { top, offset: o } => {
                let origin = self.placement.lock().unwrap().position;
                let entrance = self.settings.relative(BlockPos::new(3 - o[0], 0, -o[2]));
                let height = ctx.level.height(
                    HeightmapKind::WorldSurfaceWg,
                    origin.x + entrance.x,
                    origin.z + entrance.z,
                );
                let position = offset(origin, 0, height - 90 - 1, 0);
                self.place_at(ctx, &chunk_box, position, reference);
                if top {
                    let trapdoor = self.settings.relative(BlockPos::new(3, 0, 5));
                    let trapdoor = offset(position, trapdoor.x, trapdoor.y, trapdoor.z);
                    let below = ctx.level.block(offset(trapdoor, 0, -1, 0));
                    if !is_air(below) && below.block() != Block::Ladder {
                        ctx.level
                            .set_block(trapdoor, Block::SnowBlock.default_state());
                    }
                }
            }
            Variant::OceanRuin { .. } => {
                let origin = self.placement.lock().unwrap().position;
                let height = ctx
                    .level
                    .height(HeightmapKind::OceanFloorWg, origin.x, origin.z);
                let mut position = BlockPos::new(origin.x, height, origin.z);
                let [sx, _, sz] = self.template.size;
                let corner = transform(
                    BlockPos::new(sx - 1, 0, sz - 1),
                    Mirror::None,
                    self.settings.rotation,
                    BlockPos::new(0, 0, 0),
                );
                let corner = offset(position, corner.x, corner.y, corner.z);
                position.y = Self::ruin_height(ctx.level, position, corner);
                self.placement.lock().unwrap().position = position;
                self.place_at(ctx, &chunk_box, position, reference);
            }
        }
    }
}

struct Shipwreck {
    beached: bool,
}

pub fn shipwreck(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    let o = obj(config, &["is_beached"], "shipwreck")?;
    Ok(Arc::new(Shipwreck {
        beached: o.bool("is_beached")?,
    }))
}

impl StructureKind for Shipwreck {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let beached = self.beached;
        let kind = if beached {
            HeightmapKind::WorldSurfaceWg
        } else {
            HeightmapKind::OceanFloorWg
        };
        let position = ctx.on_top_of_chunk_center(kind)?;
        let chunk = ctx.chunk;
        Some(GenerationStub {
            position,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let rotation = random_rotation(&mut ctx.random);
                let origin = BlockPos::new(chunk.x << 4, 90, chunk.z << 4);
                let list = templates(if beached {
                    "shipwreck_beached"
                } else {
                    "shipwreck_ocean"
                });
                let name = list[ctx.random.next_int_bounded(list.len() as i32) as usize];
                let mut processors = ProcessorList::default();
                processors.push(BlockIgnore::structure_and_air());
                let settings = PlaceSettings {
                    rotation,
                    pivot: BlockPos::new(4, 0, 15),
                    processors,
                    ..PlaceSettings::default()
                };
                let piece =
                    TemplatePiece::new(Variant::Shipwreck { beached }, name, settings, origin);
                if piece.too_big() {
                    let b = piece.bounding_box();
                    let height = if beached {
                        let min_y = ctx.lowest_y(b.min_x, b.min_z, b.x_span(), b.z_span());
                        piece.beached_height(min_y, &mut ctx.random)
                    } else {
                        ctx.mean_first_occupied_height(b.min_x, b.min_z, b.x_span(), b.z_span())
                    };
                    let mut placement = piece.placement.lock().unwrap();
                    placement.height_adjusted = true;
                    placement.position.y = height;
                }
                vec![Box::new(piece) as Box<dyn Piece>]
            }),
        })
    }
}

struct Igloo;

pub fn igloo(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "igloo")?;
    Ok(Arc::new(Igloo))
}

fn igloo_piece(use_: &str, origin: BlockPos, rotation: Rotation, depth: i32) -> Box<dyn Piece> {
    let name = templates(use_)[0];
    let (_, pivot, o) = tuff_vanilla_data::world::IGLOO_PARTS
        .iter()
        .find(|(t, _, _)| *t == name)
        .copied()
        .expect("every igloo part has a pivot");
    let mut processors = ProcessorList::default();
    processors.push(BlockIgnore::structure_block());
    let settings = PlaceSettings {
        rotation,
        pivot: BlockPos::new(pivot[0], pivot[1], pivot[2]),
        processors,
        apply_waterlogging: false,
        ..PlaceSettings::default()
    };
    let position = offset(origin, o[0], o[1] - depth, o[2]);
    let variant = Variant::Igloo {
        top: use_ == "igloo_top",
        offset: o,
    };
    Box::new(TemplatePiece::new(variant, name, settings, position))
}

impl StructureKind for Igloo {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let position = ctx.on_top_of_chunk_center(HeightmapKind::WorldSurfaceWg)?;
        let chunk = ctx.chunk;
        Some(GenerationStub {
            position,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let origin = BlockPos::new(chunk.x << 4, 90, chunk.z << 4);
                let rotation = random_rotation(&mut ctx.random);
                let random = &mut ctx.random;
                let mut pieces = Vec::new();
                if random.next_double() < 0.5 {
                    let depth = random.next_int_bounded(8) + 4;
                    pieces.push(igloo_piece("igloo_bottom", origin, rotation, depth * 3));
                    for i in 0..depth - 1 {
                        pieces.push(igloo_piece("igloo_middle", origin, rotation, i * 3));
                    }
                }
                pieces.push(igloo_piece("igloo_top", origin, rotation, 0));
                pieces
            }),
        })
    }
}

struct OceanRuin {
    warm: bool,
    large_probability: f32,
    cluster_probability: f32,
}

pub fn ocean_ruin(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    let o = obj(
        config,
        &["biome_temp", "cluster_probability", "large_probability"],
        "ocean_ruin",
    )?;
    let warm = match o.str("biome_temp")? {
        "warm" => true,
        "cold" => false,
        other => return Err(format!("unknown ocean ruin type {other}")),
    };
    Ok(Arc::new(OceanRuin {
        warm,
        large_probability: o.f32("large_probability")?,
        cluster_probability: o.f32("cluster_probability")?,
    }))
}

fn mth_next_int(random: &mut dyn RandomSource, min: i32, max: i32) -> i32 {
    if min >= max {
        min
    } else {
        random.next_int_bounded(max - min + 1) + min
    }
}

impl OceanRuin {
    fn settings(&self, rotation: Rotation, integrity: f32) -> PlaceSettings {
        let (block, suspicious, table) = if self.warm {
            (
                "minecraft:sand",
                "minecraft:suspicious_sand",
                "ocean_ruin_warm_archaeology",
            )
        } else {
            (
                "minecraft:gravel",
                "minecraft:suspicious_gravel",
                "ocean_ruin_cold_archaeology",
            )
        };
        let archaeology = json!({
            "processor_type": "minecraft:capped",
            "limit": 5,
            "delegate": {
                "processor_type": "minecraft:rule",
                "rules": [{
                    "input_predicate": {"predicate_type": "minecraft:block_match", "block": block},
                    "location_predicate": {"predicate_type": "minecraft:always_true"},
                    "position_predicate": {"predicate_type": "minecraft:always_true"},
                    "output_state": {"Name": suspicious},
                    "block_entity_modifier": {
                        "type": "minecraft:append_loot",
                        "loot_table": structure_loot(table),
                    },
                }],
            },
        });
        let rot = json!({"processor_type": "minecraft:block_rot", "integrity": integrity});
        let mut processors = ProcessorList::default();
        processors
            .0
            .push(parse_processor(&rot).expect("block rot processor parses"));
        processors.push(BlockIgnore::structure_and_air());
        processors
            .0
            .push(parse_processor(&archaeology).expect("archaeology processor parses"));
        PlaceSettings {
            rotation,
            processors,
            ..PlaceSettings::default()
        }
    }

    fn add_piece(
        &self,
        pieces: &mut Vec<Box<dyn Piece>>,
        random: &mut dyn RandomSource,
        position: BlockPos,
        rotation: Rotation,
        large: bool,
        integrity: f32,
    ) {
        let mut push = |name: &'static str, integrity: f32| {
            let variant = Variant::OceanRuin {
                integrity,
                warm: self.warm,
                large,
            };
            let settings = self.settings(rotation, integrity);
            pieces.push(Box::new(TemplatePiece::new(
                variant, name, settings, position,
            )));
        };
        if self.warm {
            let list = templates(if large {
                "ocean_ruin_big_warm"
            } else {
                "ocean_ruin_warm"
            });
            push(
                list[random.next_int_bounded(list.len() as i32) as usize],
                integrity,
            );
        } else {
            let prefix = if large {
                "ocean_ruin_big_"
            } else {
                "ocean_ruin_"
            };
            let bricks = templates(&format!("{prefix}brick"));
            let cracked = templates(&format!("{prefix}cracked"));
            let mossy = templates(&format!("{prefix}mossy"));
            let index = random.next_int_bounded(bricks.len() as i32) as usize;
            push(bricks[index], integrity);
            push(cracked[index], 0.7);
            push(mossy[index], 0.5);
        }
    }

    fn add_cluster(
        &self,
        pieces: &mut Vec<Box<dyn Piece>>,
        random: &mut dyn RandomSource,
        rotation: Rotation,
        origin: BlockPos,
    ) {
        let zero = BlockPos::new(0, 0, 0);
        let parent = BlockPos::new(origin.x, 90, origin.z);
        let corner = transform(BlockPos::new(15, 0, 15), Mirror::None, rotation, zero);
        let corner = offset(parent, corner.x, corner.y, corner.z);
        let parent_box = BoundingBox::from_corners(parent, corner);
        let o = BlockPos::new(parent.x.min(corner.x), parent.y, parent.z.min(corner.z));
        let at = |random: &mut dyn RandomSource, x: (i32, i32, i32), z: (i32, i32, i32)| {
            let dx = x.0 + mth_next_int(random, x.1, x.2);
            let dz = z.0 + mth_next_int(random, z.1, z.2);
            offset(o, dx, 0, dz)
        };
        let mut positions = vec![
            at(random, (-16, 1, 8), (16, 1, 7)),
            at(random, (-16, 1, 8), (0, 1, 7)),
            at(random, (-16, 1, 8), (-16, 4, 8)),
            at(random, (0, 1, 7), (16, 1, 7)),
            at(random, (0, 1, 7), (-16, 4, 6)),
            at(random, (16, 1, 7), (16, 3, 8)),
            at(random, (16, 1, 7), (0, 1, 7)),
            at(random, (16, 1, 7), (-16, 4, 8)),
        ];
        let ruins = mth_next_int(random, 4, 8);
        for _ in 0..ruins {
            if positions.is_empty() {
                continue;
            }
            let index = random.next_int_bounded(positions.len() as i32) as usize;
            let pos = positions.remove(index);
            let rotation = random_rotation(random);
            let corner = transform(BlockPos::new(5, 0, 6), Mirror::None, rotation, zero);
            let corner = offset(pos, corner.x, corner.y, corner.z);
            if !BoundingBox::from_corners(pos, corner).intersects(&parent_box) {
                self.add_piece(pieces, random, pos, rotation, false, 0.8);
            }
        }
    }
}

impl StructureKind for OceanRuin {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let position = ctx.on_top_of_chunk_center(HeightmapKind::OceanFloorWg)?;
        let chunk = ctx.chunk;
        let this = OceanRuin {
            warm: self.warm,
            large_probability: self.large_probability,
            cluster_probability: self.cluster_probability,
        };
        Some(GenerationStub {
            position,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let origin = BlockPos::new(chunk.x << 4, 90, chunk.z << 4);
                let random = &mut ctx.random;
                let rotation = random_rotation(random);
                let large = random.next_float() <= this.large_probability;
                let integrity = if large { 0.9 } else { 0.8 };
                let mut pieces = Vec::new();
                this.add_piece(&mut pieces, random, origin, rotation, large, integrity);
                if large && random.next_float() <= this.cluster_probability {
                    this.add_cluster(&mut pieces, random, rotation, origin);
                }
                pieces
            }),
        })
    }
}

/// Buried treasure: one chest dug into the first stone or sandstone below
/// the ocean floor.
struct BuriedTreasure;

pub fn buried_treasure(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "buried_treasure")?;
    Ok(Arc::new(BuriedTreasure))
}

impl StructureKind for BuriedTreasure {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let position = ctx.on_top_of_chunk_center(HeightmapKind::OceanFloorWg)?;
        let chunk = ctx.chunk;
        Some(GenerationStub {
            position,
            pieces: Box::new(move |_: &mut GenerationContext| {
                let at = BlockPos::new((chunk.x << 4) + 9, 90, (chunk.z << 4) + 9);
                vec![Box::new(TreasurePiece {
                    bounding_box: Mutex::new(BoundingBox::at(at)),
                }) as Box<dyn Piece>]
            }),
        })
    }
}

struct TreasurePiece {
    bounding_box: Mutex<BoundingBox>,
}

fn is_liquid_block(state: BlockState) -> bool {
    matches!(state.block(), Block::Water | Block::Lava)
}

impl Piece for TreasurePiece {
    fn kind(&self) -> PieceType {
        PieceType::Btp
    }

    fn bounding_box(&self) -> BoundingBox {
        *self.bounding_box.lock().unwrap()
    }

    fn save_extra(&self, _: &mut Compound) {}

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        _reference: BlockPos,
    ) {
        let b = self.bounding_box();
        let level = &mut *ctx.level;
        let mut pos = BlockPos::new(
            b.min_x,
            level.height(HeightmapKind::OceanFloorWg, b.min_x, b.min_z),
            b.min_z,
        );
        while pos.y > level.min_y() {
            let current = level.block(pos);
            let below = level.block(offset(pos, 0, -1, 0));
            if matches!(
                below.block(),
                Block::Sandstone | Block::Stone | Block::Andesite | Block::Granite | Block::Diorite
            ) {
                let soft = if is_air(current) || is_liquid_block(current) {
                    Block::Sand.default_state()
                } else {
                    current
                };
                for direction in [
                    Facing::Down,
                    Facing::Up,
                    Facing::North,
                    Facing::South,
                    Facing::West,
                    Facing::East,
                ] {
                    let (dx, dy, dz) = direction.step();
                    let relative = offset(pos, dx, dy, dz);
                    let state = level.block(relative);
                    if is_air(state) || is_liquid_block(state) {
                        let under = level.block(offset(relative, 0, -1, 0));
                        if (is_air(under) || is_liquid_block(under)) && direction != Facing::Up {
                            level.set_block(relative, below);
                        } else {
                            level.set_block(relative, soft);
                        }
                    }
                }
                *self.bounding_box.lock().unwrap() = BoundingBox::at(pos);
                let chunk = *chunk_box;
                if chunk.contains(pos) && level.block(pos).block() != Block::Chest {
                    let state = crate::feature::monster_room::reorient(
                        level,
                        pos,
                        Block::Chest.default_state(),
                    );
                    level.set_block(pos, state);
                    level.set_block_entity(
                        pos,
                        Compound::new()
                            .with("id", "minecraft:chest")
                            .with("LootTable", structure_loot("buried_treasure"))
                            .with("LootTableSeed", ctx.random.next_long()),
                    );
                }
                return;
            }
            pos = offset(pos, 0, -1, 0);
        }
    }
}
