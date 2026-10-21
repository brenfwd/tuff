//! End cities: towers, bridges, fat towers and at most one ship, grown
//! from a house tower out of `end_city/` templates. A branch is kept only
//! when none of its pieces runs into the pieces grown before it.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::Structure;

use crate::json::{Result, obj};
use crate::structure::geometry::{BoundingBox, Rotation};
use crate::structure::piece::structure_loot;
use crate::structure::processor::{BlockIgnore, ProcessorList};
use crate::structure::ruined_portal::place_template_with_markers;
use crate::structure::template::{self, PlaceSettings, Template};
use crate::structure::{GenerationContext, GenerationStub, Piece, PieceContext, StructureKind};
use tuff_item::random::RandomSource;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const MAX_DEPTH: i32 = 8;

struct EndCity;

pub fn factory(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "end_city")?;
    Ok(Arc::new(EndCity))
}

/// Every template an end city uses, for checking they exist.
pub const TEMPLATES: [&str; 19] = [
    "base_floor",
    "base_roof",
    "bridge_end",
    "bridge_gentle_stairs",
    "bridge_piece",
    "bridge_steep_stairs",
    "fat_tower_base",
    "fat_tower_middle",
    "fat_tower_top",
    "second_floor_1",
    "second_floor_2",
    "second_roof",
    "ship",
    "third_floor_1",
    "third_floor_2",
    "third_roof",
    "tower_base",
    "tower_piece",
    "tower_top",
];

struct CityPiece {
    name: &'static str,
    template: Arc<Template>,
    settings: PlaceSettings,
    overwrite: bool,
    position: BlockPos,
    bounding_box: BoundingBox,
    gen_depth: Mutex<i32>,
}

impl CityPiece {
    fn new(name: &'static str, position: BlockPos, rotation: Rotation, overwrite: bool) -> Self {
        let template = template::get(&format!("minecraft:end_city/{name}"));
        let mut processors = ProcessorList::default();
        processors.push(if overwrite {
            BlockIgnore::structure_block()
        } else {
            BlockIgnore::structure_and_air()
        });
        let settings = PlaceSettings {
            rotation,
            processors,
            ignore_entities: true,
            ..PlaceSettings::default()
        };
        let bounding_box = template.settings_box(&settings, position);
        Self {
            name,
            template,
            settings,
            overwrite,
            position,
            bounding_box,
            gen_depth: Mutex::new(0),
        }
    }

    fn rotation(&self) -> Rotation {
        self.settings.rotation
    }

    fn depth(&self) -> i32 {
        *self.gen_depth.lock().unwrap()
    }

    fn set_depth(&self, depth: i32) {
        *self.gen_depth.lock().unwrap() = depth;
    }

    fn anchor(&self) -> Anchor {
        Anchor {
            position: self.position,
            rotation: self.rotation(),
            depth: self.depth(),
        }
    }

    /// A piece joined to this one at `offset` (in this piece's rotation).
    fn child(
        &self,
        offset: (i32, i32, i32),
        name: &'static str,
        rotation: Rotation,
        overwrite: bool,
    ) -> Self {
        self.anchor().child(offset, name, rotation, overwrite)
    }
}

/// What a child needs of its parent.
#[derive(Clone, Copy)]
struct Anchor {
    position: BlockPos,
    rotation: Rotation,
    depth: i32,
}

impl Anchor {
    fn child(
        &self,
        offset: (i32, i32, i32),
        name: &'static str,
        rotation: Rotation,
        overwrite: bool,
    ) -> CityPiece {
        let mut child = CityPiece::new(name, self.position, rotation, overwrite);
        let o = crate::structure::geometry::transform(
            BlockPos::new(offset.0, offset.1, offset.2),
            crate::structure::geometry::Mirror::None,
            self.rotation,
            BlockPos::new(0, 0, 0),
        );
        child.position = BlockPos::new(
            child.position.x + o.x,
            child.position.y + o.y,
            child.position.z + o.z,
        );
        child.bounding_box = child.bounding_box.moved(o.x, o.y, o.z);
        child
    }
}

type Pieces = Vec<CityPiece>;

#[derive(Clone, Copy)]
enum Section {
    HouseTower,
    Tower,
    TowerBridge,
    FatTower,
}

struct Builder {
    ship_created: bool,
}

/// Adds `piece` to `pieces` and returns its index.
fn add(pieces: &mut Pieces, piece: CityPiece) -> usize {
    pieces.push(piece);
    pieces.len() - 1
}

impl Builder {
    /// Grows `section` off `parent` into a list of its own, kept only if
    /// none of it collides with `pieces` (except with pieces of the
    /// parent's generation).
    fn children(
        &mut self,
        section: Section,
        depth: i32,
        parent: Anchor,
        offset: (i32, i32, i32),
        pieces: &mut Pieces,
        random: &mut dyn RandomSource,
    ) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        let mut children = Vec::new();
        if !self.generate(section, depth, parent, offset, &mut children, random) {
            return false;
        }
        let tag = random.next_int();
        let parent_depth = parent.depth;
        for child in &children {
            child.set_depth(tag);
            let collision = pieces
                .iter()
                .find(|p| p.bounding_box.intersects(&child.bounding_box));
            if collision.is_some_and(|p| p.depth() != parent_depth) {
                return false;
            }
        }
        pieces.extend(children);
        true
    }

    fn generate(
        &mut self,
        section: Section,
        depth: i32,
        parent: Anchor,
        offset: (i32, i32, i32),
        pieces: &mut Pieces,
        random: &mut dyn RandomSource,
    ) -> bool {
        let rotation = parent.rotation;
        match section {
            Section::HouseTower => {
                if depth > MAX_DEPTH {
                    return false;
                }
                let floor = add(pieces, parent.child(offset, "base_floor", rotation, true));
                match random.next_int_bounded(3) {
                    0 => {
                        let roof = pieces[floor].child((-1, 4, -1), "base_roof", rotation, true);
                        add(pieces, roof);
                    }
                    1 => {
                        let second =
                            pieces[floor].child((-1, 0, -1), "second_floor_2", rotation, false);
                        let second = add(pieces, second);
                        let roof =
                            pieces[second].child((-1, 8, -1), "second_roof", rotation, false);
                        let roof = add(pieces, roof);
                        let roof = pieces[roof].anchor();
                        self.children(Section::Tower, depth + 1, roof, (0, 0, 0), pieces, random);
                    }
                    _ => {
                        let second =
                            pieces[floor].child((-1, 0, -1), "second_floor_2", rotation, false);
                        let second = add(pieces, second);
                        let third =
                            pieces[second].child((-1, 4, -1), "third_floor_2", rotation, false);
                        let third = add(pieces, third);
                        let roof = pieces[third].child((-1, 8, -1), "third_roof", rotation, true);
                        let roof = add(pieces, roof);
                        let roof = pieces[roof].anchor();
                        self.children(Section::Tower, depth + 1, roof, (0, 0, 0), pieces, random);
                    }
                }
                true
            }
            Section::Tower => {
                let dx = 3 + random.next_int_bounded(2);
                let dz = 3 + random.next_int_bounded(2);
                let base = add(
                    pieces,
                    parent.child((dx, -3, dz), "tower_base", rotation, true),
                );
                let piece = pieces[base].child((0, 7, 0), "tower_piece", rotation, true);
                let mut last = add(pieces, piece);
                let mut bridge = (random.next_int_bounded(3) == 0).then_some(last);
                let height = 1 + random.next_int_bounded(3);
                for i in 0..height {
                    let piece = pieces[last].child((0, 4, 0), "tower_piece", rotation, true);
                    last = add(pieces, piece);
                    if i < height - 1 && random.next_bool() {
                        bridge = Some(last);
                    }
                }
                if let Some(bridge) = bridge {
                    for (turn, offset) in [
                        (Rotation::None, (1, -1, 0)),
                        (Rotation::Clockwise90, (6, -1, 1)),
                        (Rotation::CounterClockwise90, (0, -1, 5)),
                        (Rotation::Clockwise180, (5, -1, 6)),
                    ] {
                        if random.next_bool() {
                            let start = pieces[bridge].child(
                                offset,
                                "bridge_end",
                                rotation.then(turn),
                                true,
                            );
                            let start = add(pieces, start);
                            let start = pieces[start].anchor();
                            self.children(
                                Section::TowerBridge,
                                depth + 1,
                                start,
                                (0, 0, 0),
                                pieces,
                                random,
                            );
                        }
                    }
                    let top = pieces[last].child((-1, 4, -1), "tower_top", rotation, true);
                    add(pieces, top);
                    return true;
                }
                if depth == 7 {
                    let top = pieces[last].child((-1, 4, -1), "tower_top", rotation, true);
                    add(pieces, top);
                    return true;
                }
                let last = pieces[last].anchor();

                self.children(
                    Section::FatTower,
                    depth + 1,
                    last,
                    (0, 0, 0),
                    pieces,
                    random,
                )
            }
            Section::TowerBridge => {
                let length = random.next_int_bounded(4) + 1;
                let first = add(
                    pieces,
                    parent.child((0, 0, -4), "bridge_piece", rotation, true),
                );
                pieces[first].set_depth(-1);
                let mut last = first;
                let mut next_y = 0;
                for _ in 0..length {
                    let piece = if random.next_bool() {
                        let piece =
                            pieces[last].child((0, next_y, -4), "bridge_piece", rotation, true);
                        next_y = 0;
                        piece
                    } else {
                        let piece = if random.next_bool() {
                            pieces[last].child(
                                (0, next_y, -4),
                                "bridge_steep_stairs",
                                rotation,
                                true,
                            )
                        } else {
                            pieces[last].child(
                                (0, next_y, -8),
                                "bridge_gentle_stairs",
                                rotation,
                                true,
                            )
                        };
                        next_y = 4;
                        piece
                    };
                    last = add(pieces, piece);
                }
                if self.ship_created || random.next_int_bounded(10 - depth) != 0 {
                    let parent = pieces[last].anchor();
                    let done = self.children(
                        Section::HouseTower,
                        depth + 1,
                        parent,
                        (-3, next_y + 1, -11),
                        pieces,
                        random,
                    );
                    if !done {
                        return false;
                    }
                } else {
                    let dx = -8 + random.next_int_bounded(8);
                    let dz = -70 + random.next_int_bounded(10);
                    let ship = pieces[last].child((dx, next_y, dz), "ship", rotation, true);
                    add(pieces, ship);
                    self.ship_created = true;
                }
                let end = pieces[last].child(
                    (4, next_y, 0),
                    "bridge_end",
                    rotation.then(Rotation::Clockwise180),
                    true,
                );
                let end = add(pieces, end);
                pieces[end].set_depth(-1);
                true
            }
            Section::FatTower => {
                let base = add(
                    pieces,
                    parent.child((-3, 4, -3), "fat_tower_base", rotation, true),
                );
                let middle = pieces[base].child((0, 4, 0), "fat_tower_middle", rotation, true);
                let mut last = add(pieces, middle);
                let mut i = 0;
                while i < 2 && random.next_int_bounded(3) != 0 {
                    let middle = pieces[last].child((0, 8, 0), "fat_tower_middle", rotation, true);
                    last = add(pieces, middle);
                    for (turn, offset) in [
                        (Rotation::None, (4, -1, 0)),
                        (Rotation::Clockwise90, (12, -1, 4)),
                        (Rotation::CounterClockwise90, (0, -1, 8)),
                        (Rotation::Clockwise180, (8, -1, 12)),
                    ] {
                        if random.next_bool() {
                            let start =
                                pieces[last].child(offset, "bridge_end", rotation.then(turn), true);
                            let start = add(pieces, start);
                            let start = pieces[start].anchor();
                            self.children(
                                Section::TowerBridge,
                                depth + 1,
                                start,
                                (0, 0, 0),
                                pieces,
                                random,
                            );
                        }
                    }
                    i += 1;
                }
                let top = pieces[last].child((-2, 8, -2), "fat_tower_top", rotation, true);
                add(pieces, top);
                true
            }
        }
    }
}

impl StructureKind for EndCity {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let chunk = ctx.chunk;
        let (x, z) = ((chunk.x << 4) + 7, (chunk.z << 4) + 7);
        if !ctx.could_exist_in_terrain_column(x, z) {
            return None;
        }
        let rotation = Rotation::ALL[ctx.random.next_int_bounded(4) as usize];
        let (size_x, size_z) = match rotation {
            Rotation::Clockwise90 => (-5, 5),
            Rotation::Clockwise180 => (-5, -5),
            Rotation::CounterClockwise90 => (5, -5),
            Rotation::None => (5, 5),
        };
        let y = ctx.lowest_y(x, z, size_x, size_z);
        if y < 60 {
            return None;
        }
        let start = BlockPos::new(x, y, z);
        Some(GenerationStub {
            position: start,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let random = &mut ctx.random;
                let mut builder = Builder {
                    ship_created: false,
                };
                let mut pieces: Pieces = Vec::new();
                let floor = add(
                    &mut pieces,
                    CityPiece::new("base_floor", start, rotation, true),
                );
                let second = pieces[floor].child((-1, 0, -1), "second_floor_1", rotation, false);
                let second = add(&mut pieces, second);
                let third = pieces[second].child((-1, 4, -1), "third_floor_1", rotation, false);
                let third = add(&mut pieces, third);
                let roof = pieces[third].child((-1, 8, -1), "third_roof", rotation, true);
                let roof = add(&mut pieces, roof);
                let roof = pieces[roof].anchor();
                builder.children(Section::Tower, 1, roof, (0, 0, 0), &mut pieces, random);
                pieces
                    .into_iter()
                    .map(|p| Box::new(p) as Box<dyn Piece>)
                    .collect()
            }),
        })
    }
}

/// The saved data of an item frame holding an elytra, hung on the wall
/// behind `pos` facing `facing`.
fn elytra_frame(pos: BlockPos, facing: Facing) -> Compound {
    let (dx, _, dz) = facing.step();
    let center = [
        pos.x as f64 + 0.5 - dx as f64 * 0.46875,
        pos.y as f64 + 0.5,
        pos.z as f64 + 0.5 - dz as f64 * 0.46875,
    ];
    let yaw = crate::structure::geometry::data_value_2d(facing) as f32 * 90.0;
    let facing_id: i8 = match facing {
        Facing::Down => 0,
        Facing::Up => 1,
        Facing::North => 2,
        Facing::South => 3,
        Facing::West => 4,
        Facing::East => 5,
    };
    Compound::new()
        .with("id", "minecraft:item_frame")
        .with(
            "Pos",
            Tag::List(center.iter().map(|v| Tag::Double(*v)).collect()),
        )
        .with("Motion", Tag::List(vec![Tag::Double(0.0); 3]))
        .with(
            "Rotation",
            Tag::List(vec![Tag::Float(yaw), Tag::Float(0.0)]),
        )
        .with("block_pos", Tag::IntArray(vec![pos.x, pos.y, pos.z]))
        .with("Facing", facing_id)
        .with(
            "Item",
            Compound::new()
                .with("id", "minecraft:elytra")
                .with("count", 1),
        )
        .with("ItemRotation", 0i8)
        .with("ItemDropChance", 1.0f32)
        .with("Invisible", false)
        .with("Fixed", false)
}

impl Piece for CityPiece {
    fn kind(&self) -> PieceType {
        PieceType::Ecp
    }

    fn bounding_box(&self) -> BoundingBox {
        self.bounding_box
    }

    fn orientation(&self) -> Option<Facing> {
        Some(Facing::North)
    }

    fn gen_depth(&self) -> i32 {
        self.depth()
    }

    fn save_extra(&self, tag: &mut Compound) {
        tag.insert("TPX", self.position.x);
        tag.insert("TPY", self.position.y);
        tag.insert("TPZ", self.position.z);
        tag.insert("Template", self.name);
        tag.insert("Rot", self.rotation().name());
        tag.insert("OW", self.overwrite);
    }

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        reference: BlockPos,
    ) {
        let chunk = *chunk_box;
        let rotation = self.rotation();
        place_template_with_markers(
            ctx,
            &self.template,
            self.position,
            reference,
            &self.settings,
            chunk,
            |ctx, marker, pos| {
                if marker.starts_with("Chest") {
                    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
                    if chunk.contains(below) && ctx.level.block(below).block().has_loot_container()
                    {
                        let seed = ctx.random.next_long();
                        if let Some(data) = ctx.level.block_entity_mut(below) {
                            data.insert("LootTable", structure_loot("end_city"));
                            data.insert("LootTableSeed", seed);
                        }
                    }
                } else if chunk.contains(pos) && marker.starts_with("Sentry") {
                    let shulker = crate::structure::structure_mob("minecraft:shulker", pos, false);
                    ctx.level.add_entity(pos, shulker);
                } else if chunk.contains(pos) && marker.starts_with("Elytra") {
                    let facing = rotation.rotate_facing(Facing::South);
                    ctx.level.add_entity(pos, elytra_frame(pos, facing));
                }
            },
        );
    }
}
