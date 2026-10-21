//! Rock features: magma under water and mossy boulders.

use std::sync::Arc;

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::{BlockPredicate, step};
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::{self, obj};
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:block_blob", |_, v| {
        let o = obj(v, &["state", "can_place_on"], "block_blob")?;
        Ok(Arc::new(BlockBlob {
            state: json::block_state(o.req("state")?)?,
            can_place_on: BlockPredicate::parse(o.req("can_place_on")?)?,
        }))
    });
    registry.register("minecraft:underwater_magma", |_, v| {
        let o = obj(
            v,
            &[
                "floor_search_range",
                "placement_radius_around_floor",
                "placement_probability_per_valid_position",
            ],
            "underwater_magma",
        )?;
        Ok(Arc::new(UnderwaterMagma {
            floor_search_range: o.i32("floor_search_range")?,
            radius: o.i32("placement_radius_around_floor")?,
            probability: o.f32("placement_probability_per_valid_position")?,
        }))
    });
}

/// Scans a column of blocks matching `inside` from `origin` toward
/// `direction`, at most `range - 1` steps, and returns the y of the edge
/// it stops on if that block is a valid edge.
pub fn scan_column(
    level: &Region,
    origin: BlockPos,
    range: i32,
    inside: impl Fn(BlockState) -> bool,
    edge: impl Fn(BlockState) -> bool,
    direction: Facing,
) -> Option<i32> {
    if !inside(level.block(origin)) {
        return None;
    }
    let mut pos = origin;
    let mut i = 1;
    while i < range && inside(level.block(pos)) {
        pos = step(pos, direction);
        i += 1;
    }
    edge(level.block(pos)).then_some(pos.y)
}

/// Whether the face of the block at `pos` toward `covered` leaves any gap.
pub fn is_visible_from_outside(level: &Region, pos: BlockPos, covered: Facing) -> bool {
    !level.block(pos).is_occlusion_face_full(covered)
}

struct UnderwaterMagma {
    floor_search_range: i32,
    radius: i32,
    probability: f32,
}

impl UnderwaterMagma {
    fn is_valid(level: &Region, pos: BlockPos) -> bool {
        let state = level.block(pos);
        if state.block() == Block::Water
            || is_air(state)
            || is_visible_from_outside(level, BlockPos::new(pos.x, pos.y - 1, pos.z), Facing::Up)
        {
            return false;
        }
        [Facing::North, Facing::East, Facing::South, Facing::West]
            .into_iter()
            .all(|d| !is_visible_from_outside(level, step(pos, d), d.opposite()))
    }
}

impl Feature for UnderwaterMagma {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let is_water = |s: BlockState| s.block() == Block::Water;
        let Some(floor) = scan_column(
            ctx.level,
            origin,
            self.floor_search_range,
            is_water,
            |s| !is_water(s),
            Facing::Down,
        ) else {
            return false;
        };
        let r = self.radius;
        let mut placed = false;
        for z in origin.z - r..=origin.z + r {
            for y in floor - r..=floor + r {
                for x in origin.x - r..=origin.x + r {
                    let pos = BlockPos::new(x, y, z);
                    if ctx.random.next_float() < self.probability && Self::is_valid(ctx.level, pos)
                    {
                        ctx.level.set_block(pos, Block::MagmaBlock.default_state());
                        placed = true;
                    }
                }
            }
        }
        placed
    }
}

struct BlockBlob {
    state: BlockState,
    can_place_on: BlockPredicate,
}

impl Feature for BlockBlob {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let floor = ctx.level.min_y() + 3;
        let mut origin = origin;
        while origin.y > floor
            && !self
                .can_place_on
                .test(ctx.level, BlockPos::new(origin.x, origin.y - 1, origin.z))
        {
            origin.y -= 1;
        }
        if origin.y <= floor {
            return false;
        }
        for _ in 0..3 {
            let xr = ctx.random.next_int_bounded(2);
            let yr = ctx.random.next_int_bounded(2);
            let zr = ctx.random.next_int_bounded(2);
            let radius = (xr + yr + zr) as f32 * 0.333 + 0.5;
            let limit = (radius * radius) as f64;
            for z in origin.z - zr..=origin.z + zr {
                for y in origin.y - yr..=origin.y + yr {
                    for x in origin.x - xr..=origin.x + xr {
                        let (dx, dy, dz) = (x - origin.x, y - origin.y, z - origin.z);
                        if ((dx * dx + dy * dy + dz * dz) as f64) <= limit {
                            ctx.level.set_block(BlockPos::new(x, y, z), self.state);
                        }
                    }
                }
            }
            let dx = -1 + ctx.random.next_int_bounded(2);
            let dy = -ctx.random.next_int_bounded(2);
            let dz = -1 + ctx.random.next_int_bounded(2);
            origin = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
        }
        true
    }
}
