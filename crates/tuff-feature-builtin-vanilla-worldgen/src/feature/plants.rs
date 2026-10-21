//! Single vines and bamboo stalks.

use std::sync::Arc;

use tuff_vanilla_data::Block;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;

use crate::feature::multiface::can_attach_to;
use crate::feature::predicate::{DIRECTIONS, step};
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::obj;
use crate::proto::HeightmapKind;
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:vines", |_, v| {
        obj(v, &[], "vines")?;
        Ok(Arc::new(Vines))
    });
    registry.register("minecraft:bamboo", |_, v| {
        let o = obj(v, &["probability"], "bamboo")?;
        Ok(Arc::new(Bamboo {
            probability: o.f32("probability")?,
        }))
    });
}

struct Vines;

impl Feature for Vines {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if !is_air(ctx.level.block(origin)) {
            return false;
        }
        for direction in DIRECTIONS {
            if direction != Facing::Down
                && can_attach_to(ctx.level.block(step(origin, direction)), direction)
            {
                let vine = Block::Vine.default_state();
                let vine = vine.with(direction.name(), "true").unwrap_or(vine);
                ctx.level.set_block(origin, vine);
                return true;
            }
        }
        false
    }
}

struct Bamboo {
    probability: f32,
}

fn bamboo(leaves: &str, stage: &str) -> BlockState {
    let state = Block::Bamboo.default_state();
    state
        .with("age", "1")
        .and_then(|s| s.with("leaves", leaves))
        .and_then(|s| s.with("stage", stage))
        .unwrap_or(state)
}

impl Feature for Bamboo {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if !is_air(ctx.level.block(origin)) {
            return false;
        }
        let level = &*ctx.level;
        if !level.can_survive(Block::Bamboo.default_state(), origin) {
            return true;
        }
        let height = ctx.random.next_int_bounded(12) + 5;
        if ctx.random.next_float() < self.probability {
            let r = ctx.random.next_int_bounded(4) + 1;
            for x in origin.x - r..=origin.x + r {
                for z in origin.z - r..=origin.z + r {
                    let (dx, dz) = (x - origin.x, z - origin.z);
                    if dx * dx + dz * dz > r * r {
                        continue;
                    }
                    let y = ctx.level.height(HeightmapKind::WorldSurface, x, z) - 1;
                    let pos = BlockPos::new(x, y, z);
                    let replaceable = tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::block::key::BENEATH_BAMBOO_PODZOL_REPLACEABLE,
                        ctx.level.block(pos).block(),
                    );
                    if replaceable {
                        ctx.level.set_block(pos, Block::Podzol.default_state());
                    }
                }
            }
        }
        let trunk = bamboo("none", "0");
        let mut pos = origin;
        let mut i = 0;
        while i < height && is_air(ctx.level.block(pos)) {
            ctx.level.set_block(pos, trunk);
            pos.y += 1;
            i += 1;
        }
        if pos.y - origin.y >= 3 {
            ctx.level.set_block(pos, bamboo("large", "1"));
            pos.y -= 1;
            ctx.level.set_block(pos, bamboo("large", "0"));
            pos.y -= 1;
            ctx.level.set_block(pos, bamboo("small", "0"));
        }
        true
    }
}
