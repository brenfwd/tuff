//! The building blocks of hand-built structure pieces (mineshafts,
//! fortresses, strongholds, ...): piece-local coordinates turned by the
//! piece's orientation, and boxes, spheres and single blocks placed within
//! the chunk being decorated.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::proto::HeightmapKind;
use crate::structure::geometry::{BoundingBox, Mirror, Rotation};
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

/// Where a piece is and which way it faces.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub bounding_box: BoundingBox,
    pub orientation: Option<Facing>,
    pub gen_depth: i32,
}

impl Frame {
    pub fn new(gen_depth: i32, bounding_box: BoundingBox, orientation: Option<Facing>) -> Self {
        Self {
            bounding_box,
            orientation,
            gen_depth,
        }
    }

    pub fn mirror(&self) -> Mirror {
        match self.orientation {
            Some(Facing::South | Facing::West) => Mirror::LeftRight,
            _ => Mirror::None,
        }
    }

    pub fn rotation(&self) -> Rotation {
        match self.orientation {
            Some(Facing::West | Facing::East) => Rotation::Clockwise90,
            _ => Rotation::None,
        }
    }

    pub fn world_x(&self, x: i32, z: i32) -> i32 {
        let b = &self.bounding_box;
        match self.orientation {
            Some(Facing::North | Facing::South) => b.min_x + x,
            Some(Facing::West) => b.max_x - z,
            Some(Facing::East) => b.min_x + z,
            _ => x,
        }
    }

    pub fn world_y(&self, y: i32) -> i32 {
        match self.orientation {
            None => y,
            Some(_) => y + self.bounding_box.min_y,
        }
    }

    pub fn world_z(&self, x: i32, z: i32) -> i32 {
        let b = &self.bounding_box;
        match self.orientation {
            Some(Facing::North) => b.max_z - z,
            Some(Facing::South) => b.min_z + z,
            Some(Facing::West | Facing::East) => b.min_z + x,
            _ => z,
        }
    }

    pub fn world_pos(&self, x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(self.world_x(x, z), self.world_y(y), self.world_z(x, z))
    }
}

/// The loot table dumped for a structure use (`fortress`, ...).
pub fn structure_loot(name: &str) -> &'static str {
    tuff_vanilla_data::world::STRUCTURE_LOOT_TABLES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, table)| *table)
        .unwrap_or_else(|| panic!("no structure loot table for {name}"))
}

pub fn is_replaceable_by_structures(state: BlockState) -> bool {
    is_air(state)
        || state.is_liquid()
        || matches!(
            state.block(),
            Block::GlowLichen | Block::Seagrass | Block::TallSeagrass
        )
}

/// A piece built from boxes and blocks in its own coordinates.
pub trait Framed {
    fn frame(&self) -> &Frame;

    /// Whether the piece may overwrite the block at piece coordinates.
    fn can_be_replaced(
        &self,
        _level: &Region,
        _x: i32,
        _y: i32,
        _z: i32,
        _chunk: &BoundingBox,
    ) -> bool {
        true
    }

    fn place_block(
        &self,
        level: &mut Region,
        state: BlockState,
        x: i32,
        y: i32,
        z: i32,
        chunk: &BoundingBox,
    ) {
        let frame = self.frame();
        let pos = frame.world_pos(x, y, z);
        if !chunk.contains(pos) || !self.can_be_replaced(level, x, y, z, chunk) {
            return;
        }
        let state = frame
            .rotation()
            .rotate_state(frame.mirror().mirror_state(state));
        level.set_block(pos, state);
        level.schedule_own_fluid_tick(pos, state);
        if tuff_vanilla_data::world::STRUCTURE_SHAPE_CHECK_BLOCKS.contains(&state.block()) {
            level.mark_post_processing(pos);
        }
    }

    fn get_block(&self, level: &Region, x: i32, y: i32, z: i32, chunk: &BoundingBox) -> BlockState {
        let pos = self.frame().world_pos(x, y, z);
        if !chunk.contains(pos) {
            return Block::Air.default_state();
        }
        level.block(pos)
    }

    fn is_interior(&self, level: &mut Region, x: i32, y: i32, z: i32, chunk: &BoundingBox) -> bool {
        let pos = self.frame().world_pos(x, y + 1, z);
        chunk.contains(pos) && pos.y < level.height(HeightmapKind::OceanFloorWg, pos.x, pos.z)
    }

    /// Fills from piece coordinates downward while the blocks are air,
    /// fluid or thin water plants.
    fn fill_column_down(
        &self,
        level: &mut Region,
        state: BlockState,
        x: i32,
        y: i32,
        z: i32,
        chunk: &BoundingBox,
    ) {
        let mut pos = self.frame().world_pos(x, y, z);
        if !chunk.contains(pos) {
            return;
        }
        while is_replaceable_by_structures(level.block(pos)) && pos.y > level.min_y() + 1 {
            level.set_block(pos, state);
            pos = BlockPos::new(pos.x, pos.y - 1, pos.z);
        }
    }

    /// Places a loot chest turned away from its walls, unless one is
    /// already there.
    #[allow(clippy::too_many_arguments)]
    fn create_chest(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        random: &mut dyn RandomSource,
        x: i32,
        y: i32,
        z: i32,
        loot_table: &str,
    ) -> bool {
        let pos = self.frame().world_pos(x, y, z);
        if !chunk.contains(pos) || level.block(pos).block() == Block::Chest {
            return false;
        }
        let state =
            crate::feature::monster_room::reorient(level, pos, Block::Chest.default_state());
        level.set_block(pos, state);
        level.set_block_entity(
            pos,
            tuff_nbt::Compound::new()
                .with("id", "minecraft:chest")
                .with("LootTable", loot_table)
                .with("LootTableSeed", random.next_long()),
        );
        true
    }

    /// A box whose blocks `select` picks, told whether each is on the edge.
    #[allow(clippy::too_many_arguments)]
    fn generate_box_with(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        random: &mut dyn RandomSource,
        (x0, y0, z0): (i32, i32, i32),
        (x1, y1, z1): (i32, i32, i32),
        skip_air: bool,
        mut select: impl FnMut(&mut dyn RandomSource, bool) -> BlockState,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if skip_air && is_air(self.get_block(level, x, y, z, chunk)) {
                        continue;
                    }
                    let edge = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    let state = select(random, edge);
                    self.place_block(level, state, x, y, z, chunk);
                }
            }
        }
    }

    fn generate_air_box(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        (x0, y0, z0): (i32, i32, i32),
        (x1, y1, z1): (i32, i32, i32),
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    self.place_block(level, Block::Air.default_state(), x, y, z, chunk);
                }
            }
        }
    }

    /// Places a loot dispenser facing `facing` (in piece coordinates),
    /// unless one is already there.
    #[allow(clippy::too_many_arguments)]
    fn create_dispenser(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        random: &mut dyn RandomSource,
        (x, y, z): (i32, i32, i32),
        facing: &str,
        loot_table: &str,
    ) -> bool {
        let pos = self.frame().world_pos(x, y, z);
        if !chunk.contains(pos) || level.block(pos).block() == Block::Dispenser {
            return false;
        }
        let state = Block::Dispenser.default_state();
        let state = state.with("facing", facing).unwrap_or(state);
        self.place_block(level, state, x, y, z, chunk);
        if level.block(pos).block() == Block::Dispenser {
            level.set_block_entity(
                pos,
                tuff_nbt::Compound::new()
                    .with("id", "minecraft:dispenser")
                    .with("LootTable", loot_table)
                    .with("LootTableSeed", random.next_long()),
            );
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn generate_box(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        (x0, y0, z0): (i32, i32, i32),
        (x1, y1, z1): (i32, i32, i32),
        edge: BlockState,
        fill: BlockState,
        skip_air: bool,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if skip_air && is_air(self.get_block(level, x, y, z, chunk)) {
                        continue;
                    }
                    let on_edge = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    let state = if on_edge { edge } else { fill };
                    self.place_block(level, state, x, y, z, chunk);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn generate_maybe_box(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        random: &mut dyn RandomSource,
        probability: f32,
        (x0, y0, z0): (i32, i32, i32),
        (x1, y1, z1): (i32, i32, i32),
        edge: BlockState,
        fill: BlockState,
        skip_air: bool,
        inside_only: bool,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if random.next_float() > probability {
                        continue;
                    }
                    if skip_air && is_air(self.get_block(level, x, y, z, chunk)) {
                        continue;
                    }
                    if inside_only && !self.is_interior(level, x, y, z, chunk) {
                        continue;
                    }
                    let on_edge = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    let state = if on_edge { edge } else { fill };
                    self.place_block(level, state, x, y, z, chunk);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn maybe_generate_block(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        random: &mut dyn RandomSource,
        probability: f32,
        x: i32,
        y: i32,
        z: i32,
        state: BlockState,
    ) {
        if random.next_float() < probability {
            self.place_block(level, state, x, y, z, chunk);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn generate_upper_half_sphere(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        (x0, y0, z0): (i32, i32, i32),
        (x1, y1, z1): (i32, i32, i32),
        fill: BlockState,
        skip_air: bool,
    ) {
        let diag_x = (x1 - x0 + 1) as f32;
        let diag_y = (y1 - y0 + 1) as f32;
        let diag_z = (z1 - z0 + 1) as f32;
        let cx = x0 as f32 + diag_x / 2.0;
        let cz = z0 as f32 + diag_z / 2.0;
        for y in y0..=y1 {
            let ny = (y - y0) as f32 / diag_y;
            for x in x0..=x1 {
                let nx = (x as f32 - cx) / (diag_x * 0.5);
                for z in z0..=z1 {
                    let nz = (z as f32 - cz) / (diag_z * 0.5);
                    if skip_air && is_air(self.get_block(level, x, y, z, chunk)) {
                        continue;
                    }
                    if nx * nx + ny * ny + nz * nz <= 1.05 {
                        self.place_block(level, fill, x, y, z, chunk);
                    }
                }
            }
        }
    }
}
