//! Huge brown and red mushrooms.

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::BlockState;

use crate::feature::predicate::BlockPredicate;
use crate::feature::provider::StateProvider;
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::{Result, obj};
use crate::tags::{leaves, replaceable_by_mushrooms};
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:huge_brown_mushroom", |r, v| {
        Ok(Arc::new(HugeMushroom::parse(r, v, false)?))
    });
    registry.register("minecraft:huge_red_mushroom", |r, v| {
        Ok(Arc::new(HugeMushroom::parse(r, v, true)?))
    });
}

fn faces_rim(d: i32, toward: i32, crossing_rim: bool) -> bool {
    d == toward || (crossing_rim && d == toward - toward.signum())
}

struct HugeMushroom {
    red: bool,
    cap: Arc<StateProvider>,
    stem: Arc<StateProvider>,
    radius: i32,
    can_place_on: BlockPredicate,
}

impl HugeMushroom {
    fn parse(registry: &mut Registry, value: &Value, red: bool) -> Result<Self> {
        let o = obj(
            value,
            &[
                "type",
                "cap_provider",
                "stem_provider",
                "foliage_radius",
                "can_place_on",
            ],
            "huge mushroom",
        )?;
        Ok(Self {
            red,
            cap: StateProvider::parse(registry, o.req("cap_provider")?)?,
            stem: StateProvider::parse(registry, o.req("stem_provider")?)?,
            radius: o.i32_or("foliage_radius", 2)?,
            can_place_on: BlockPredicate::parse(o.req("can_place_on")?)?,
        })
    }

    fn radius_at(&self, height: i32, yo: i32) -> i32 {
        let in_cap = if self.red {
            yo == height || (height - 3..height).contains(&yo)
        } else {
            yo > 3
        };
        if in_cap { self.radius } else { 0 }
    }

    fn put(ctx: &mut PlaceContext, pos: BlockPos, state: BlockState) {
        let current = ctx.level.block(pos);
        if is_air(current) || replaceable_by_mushrooms().contains_state(current) {
            ctx.level.set_block(pos, state);
        }
    }

    fn valid(&self, ctx: &mut PlaceContext, origin: BlockPos, height: i32) -> bool {
        let y = origin.y;
        let below = BlockPos::new(origin.x, y - 1, origin.z);
        if y < ctx.level.min_y() + 1
            || y + height + 1 > ctx.level.max_y()
            || !self.can_place_on.test(ctx.level, below)
        {
            return false;
        }
        for dy in 0..=height {
            let radius = self.radius_at(-1, dy);
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    let state =
                        ctx.level
                            .block(BlockPos::new(origin.x + dx, y + dy, origin.z + dz));
                    if !is_air(state) && !leaves().contains_state(state) {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn with_sides(state: BlockState, sides: &[(&str, bool)]) -> BlockState {
        if sides.iter().any(|&(name, _)| state.get(name).is_none()) {
            return state;
        }
        let mut out = state;
        for &(name, on) in sides {
            out = out
                .with(name, if on { "true" } else { "false" })
                .unwrap_or(out);
        }
        out
    }

    fn cap(&self, ctx: &mut PlaceContext, origin: BlockPos, height: i32) {
        if self.red {
            self.dome_cap(ctx, origin, height);
        } else {
            self.flat_cap(ctx, origin, height);
        }
    }

    fn dome_cap(&self, ctx: &mut PlaceContext, origin: BlockPos, height: i32) {
        let inner = self.radius - 2;
        for layer in height - 3..=height {
            let top = layer == height;
            let half = self.radius - i32::from(top);
            for dx in -half..=half {
                let x_rim = dx.abs() == half;
                for dz in -half..=half {
                    if !top && x_rim == (dz.abs() == half) {
                        continue;
                    }
                    let state = self.cap.state(ctx.level, ctx.random, origin);
                    let state = Self::with_sides(
                        state,
                        &[
                            ("up", layer + 1 >= height),
                            ("west", dx < -inner),
                            ("east", dx > inner),
                            ("north", dz < -inner),
                            ("south", dz > inner),
                        ],
                    );
                    Self::put(
                        ctx,
                        BlockPos::new(origin.x + dx, origin.y + layer, origin.z + dz),
                        state,
                    );
                }
            }
        }
    }

    fn flat_cap(&self, ctx: &mut PlaceContext, origin: BlockPos, height: i32) {
        let r = self.radius;
        for dx in -r..=r {
            let x_rim = dx.abs() == r;
            for dz in -r..=r {
                let z_rim = dz.abs() == r;
                if x_rim && z_rim {
                    continue;
                }
                let state = self.cap.state(ctx.level, ctx.random, origin);
                let state = Self::with_sides(
                    state,
                    &[
                        ("west", faces_rim(dx, -r, z_rim)),
                        ("east", faces_rim(dx, r, z_rim)),
                        ("north", faces_rim(dz, -r, x_rim)),
                        ("south", faces_rim(dz, r, x_rim)),
                    ],
                );
                Self::put(
                    ctx,
                    BlockPos::new(origin.x + dx, origin.y + height, origin.z + dz),
                    state,
                );
            }
        }
    }
}

impl Feature for HugeMushroom {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut height = ctx.random.next_int_bounded(3) + 4;
        if ctx.random.next_int_bounded(12) == 0 {
            height *= 2;
        }
        if !self.valid(ctx, origin, height) {
            return false;
        }
        self.cap(ctx, origin, height);
        for dy in 0..height {
            let pos = BlockPos::new(origin.x, origin.y + dy, origin.z);
            let state = self.stem.state(ctx.level, ctx.random, origin);
            Self::put(ctx, pos, state);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::faces_rim;

    #[tuff_core::watchdog]
    #[test]
    fn flat_cap_faces_mark_rims_and_cells_beside_corners() {
        for r in 0..6 {
            for dx in -r..=r {
                for dz in -r..=r {
                    let (min_x, max_x, min_z, max_z) = (dx == -r, dx == r, dz == -r, dz == r);
                    let (x_edge, z_edge) = (min_x || max_x, min_z || max_z);
                    assert_eq!(faces_rim(dx, -r, z_edge), min_x || (z_edge && dx == 1 - r));
                    assert_eq!(faces_rim(dx, r, z_edge), max_x || (z_edge && dx == r - 1));
                    assert_eq!(faces_rim(dz, -r, x_edge), min_z || (x_edge && dz == 1 - r));
                    assert_eq!(faces_rim(dz, r, x_edge), max_z || (x_edge && dz == r - 1));
                }
            }
        }
    }
}
