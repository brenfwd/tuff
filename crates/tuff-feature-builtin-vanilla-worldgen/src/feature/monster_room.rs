//! Monster rooms: a cobblestone box with a spawner and up to two loot
//! chests. The spawner's mob and the chests' loot table are stored as block
//! entity data; the mobs themselves spawn later.

use std::sync::Arc;

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::world::{DUNGEON_LOOT_TABLE, DUNGEON_MOBS};
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::step;
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::obj;
use tuff_item::random::RandomSource;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:monster_room", |_, v| {
        obj(v, &[], "monster_room")?;
        Ok(Arc::new(MonsterRoom))
    });
}

struct MonsterRoom;

const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

fn replaceable(state: BlockState) -> bool {
    !crate::tags::features_cannot_replace().contains_state(state)
}

fn safe_set(level: &mut Region, pos: BlockPos, state: BlockState) {
    if replaceable(level.block(pos)) {
        level.set_block(pos, state);
    }
}

fn below(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y - 1, pos.z)
}

/// Turns a chest to face away from its only solid neighbour, or else away
/// from whatever blocks its default facing.
pub fn reorient(level: &Region, pos: BlockPos, state: BlockState) -> BlockState {
    let mut solid = None;
    for direction in HORIZONTAL {
        let neighbour = level.block(step(pos, direction));
        if neighbour.block() == Block::Chest {
            return state;
        }
        if neighbour.is_solid_render() {
            if solid.is_none() {
                solid = Some(direction);
            } else {
                solid = None;
                break;
            }
        }
    }
    let facing = match solid {
        Some(direction) => direction.opposite(),
        None => {
            let mut facing = state.facing().unwrap_or(Facing::North);
            if level.block(step(pos, facing)).is_solid_render() {
                facing = facing.opposite();
            }
            if level.block(step(pos, facing)).is_solid_render() {
                facing = facing.clockwise();
            }
            if level.block(step(pos, facing)).is_solid_render() {
                facing = facing.opposite();
            }
            facing
        }
    };
    state.with("facing", facing.name()).unwrap_or(state)
}

/// The room's half extents inside its walls.
struct Room {
    origin: BlockPos,
    half_x: i32,
    half_z: i32,
}

impl Room {
    fn cell(&self, dx: i32, dy: i32, dz: i32) -> BlockPos {
        BlockPos::new(self.origin.x + dx, self.origin.y + dy, self.origin.z + dz)
    }

    /// Wall offsets on each axis, one past the half extent.
    fn reach(&self) -> (i32, i32) {
        (self.half_x + 1, self.half_z + 1)
    }

    fn columns(&self) -> impl Iterator<Item = (i32, i32, bool)> + use<> {
        let (rx, rz) = self.reach();
        (-rx..=rx).flat_map(move |dx| {
            (-rz..=rz).map(move |dz| (dx, dz, dx.abs() == rx || dz.abs() == rz))
        })
    }

    /// A room needs a solid floor and ceiling and one to five two-high
    /// openings in the ring of its walls.
    fn fits(&self, level: &Region) -> bool {
        let mut openings = 0;
        for (dx, dz, wall) in self.columns() {
            if !level.block(self.cell(dx, -1, dz)).is_solid()
                || !level.block(self.cell(dx, 4, dz)).is_solid()
            {
                return false;
            }
            if wall
                && is_air(level.block(self.cell(dx, 0, dz)))
                && is_air(level.block(self.cell(dx, 1, dz)))
            {
                openings += 1;
            }
        }
        (1..=5).contains(&openings)
    }

    /// Hollows the inside and lines the walls and floor, a column at a time
    /// from the top down.
    fn carve(&self, ctx: &mut PlaceContext) {
        let cave_air = Block::CaveAir.default_state();
        for (dx, dz, wall) in self.columns() {
            for dy in (-1..=3).rev() {
                let cell = self.cell(dx, dy, dz);
                let state = ctx.level.block(cell);
                if !(wall || dy == -1) {
                    if !matches!(state.block(), Block::Chest | Block::Spawner) {
                        safe_set(ctx.level, cell, cave_air);
                    }
                    continue;
                }
                let unsupported =
                    cell.y >= ctx.level.min_y() && !ctx.level.block(below(cell)).is_solid();
                if unsupported {
                    ctx.level.set_block(cell, cave_air);
                } else if state.is_solid() && state.block() != Block::Chest {
                    let stone = if dy == -1 && ctx.random.next_int_bounded(4) != 0 {
                        Block::MossyCobblestone
                    } else {
                        Block::Cobblestone
                    };
                    safe_set(ctx.level, cell, stone.default_state());
                }
            }
        }
    }

    /// Up to three tries at a floor spot with exactly one solid side.
    fn place_chest(&self, ctx: &mut PlaceContext) {
        for _ in 0..3 {
            let mut spread = |half: i32| ctx.random.next_int_bounded(half * 2 + 1) - half;
            let (dx, dz) = (spread(self.half_x), spread(self.half_z));
            let spot = self.cell(dx, 0, dz);
            if !is_air(ctx.level.block(spot)) {
                continue;
            }
            let sides = HORIZONTAL
                .into_iter()
                .filter(|&d| ctx.level.block(step(spot, d)).is_solid())
                .count();
            if sides != 1 {
                continue;
            }
            let chest = reorient(ctx.level, spot, Block::Chest.default_state());
            safe_set(ctx.level, spot, chest);
            if ctx.level.block(spot).block() == Block::Chest {
                let seed = ctx.random.next_long();
                ctx.level.set_block_entity(
                    spot,
                    Compound::new()
                        .with("id", "minecraft:chest")
                        .with("LootTable", DUNGEON_LOOT_TABLE)
                        .with("LootTableSeed", seed),
                );
            }
            return;
        }
    }

    fn place_spawner(&self, ctx: &mut PlaceContext) {
        safe_set(ctx.level, self.origin, Block::Spawner.default_state());
        if ctx.level.block(self.origin).block() != Block::Spawner {
            return;
        }
        let mob = DUNGEON_MOBS[ctx.random.next_int_bounded(DUNGEON_MOBS.len() as i32) as usize];
        ctx.level.set_block_entity(
            self.origin,
            Compound::new()
                .with("id", "minecraft:mob_spawner")
                .with(
                    "SpawnData",
                    Compound::new().with("entity", Compound::new().with("id", mob)),
                )
                .with("SpawnPotentials", Tag::List(Vec::new())),
        );
    }
}

impl Feature for MonsterRoom {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let half_x = 2 + ctx.random.next_int_bounded(2);
        let half_z = 2 + ctx.random.next_int_bounded(2);
        let room = Room {
            origin,
            half_x,
            half_z,
        };
        if !room.fits(ctx.level) {
            return false;
        }
        room.carve(ctx);
        room.place_chest(ctx);
        room.place_chest(ctx);
        room.place_spawner(ctx);
        true
    }
}
