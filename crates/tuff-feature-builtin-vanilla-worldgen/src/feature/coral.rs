//! Coral shapes: claws and trees built by repeating a placed feature (the
//! coral block and its decoration) along branches.

use std::sync::Arc;

use tuff_vanilla_data::block::Facing;

use crate::feature::predicate::step;
use crate::feature::tree::shuffled;
use crate::feature::{Feature, PlaceContext, PlacedFeature, Registry};
use crate::json::obj;
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;

const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:coral_claw", |r, v| {
        let o = obj(v, &["feature"], "coral_claw")?;
        Ok(Arc::new(CoralClaw(r.placed(o.req("feature")?)?)))
    });
    registry.register("minecraft:coral_tree", |r, v| {
        let o = obj(v, &["feature"], "coral_tree")?;
        Ok(Arc::new(CoralTree(r.placed(o.req("feature")?)?)))
    });
}

struct CoralClaw(Arc<PlacedFeature>);

impl CoralClaw {
    fn run(
        &self,
        ctx: &mut PlaceContext,
        mut pos: BlockPos,
        toward: Facing,
        limit: i32,
    ) -> BlockPos {
        for _ in 0..limit {
            if !self.0.place(ctx, pos, None) {
                break;
            }
            pos = step(pos, toward);
        }
        pos
    }

    fn reach(&self, ctx: &mut PlaceContext, mut pos: BlockPos, toward: Facing, limit: i32) {
        for _ in 0..limit {
            pos = step(pos, toward);
            if !self.0.place(ctx, pos, None) {
                return;
            }
            if ctx.random.next_float() < 0.25 {
                pos = step(pos, Facing::Up);
            }
        }
    }
}

impl Feature for CoralClaw {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if !self.0.place(ctx, origin, None) {
            return false;
        }
        let forward = HORIZONTAL[ctx.random.next_int_bounded(4) as usize];
        let arm_count = ctx.random.next_int_bounded(2) as usize + 2;
        let arms = shuffled(
            &[forward, forward.clockwise(), forward.counter_clockwise()],
            ctx.random,
        );
        for &arm in &arms[..arm_count] {
            let base_len = ctx.random.next_int_bounded(2) + 1;
            let (start, run_dir, reach_len) = if arm == forward {
                (step(origin, arm), arm, ctx.random.next_int_bounded(3) + 2)
            } else {
                let run_dir = if ctx.random.next_int_bounded(2) == 0 {
                    arm
                } else {
                    Facing::Up
                };
                (
                    step(step(origin, arm), Facing::Up),
                    run_dir,
                    ctx.random.next_int_bounded(3) + 3,
                )
            };
            let end = self.run(ctx, start, run_dir, base_len);
            let elbow = step(step(end, run_dir.opposite()), Facing::Up);
            self.reach(ctx, elbow, forward, reach_len);
        }
        true
    }
}

struct CoralTree(Arc<PlacedFeature>);

impl Feature for CoralTree {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut pos = origin;
        let trunk = ctx.random.next_int_bounded(3) + 1;
        for _ in 0..trunk {
            if !self.0.place(ctx, pos, None) {
                return true;
            }
            pos = step(pos, Facing::Up);
        }
        let top = pos;
        let branches = ctx.random.next_int_bounded(3) + 2;
        let directions = shuffled(&HORIZONTAL, ctx.random);
        for &branch in &directions[..branches as usize] {
            let mut pos = step(top, branch);
            let height = ctx.random.next_int_bounded(5) + 2;
            let mut segment = 0;
            let mut j = 0;
            while j < height && self.0.place(ctx, pos, None) {
                segment += 1;
                pos = step(pos, Facing::Up);
                if j == 0 || (segment >= 2 && ctx.random.next_float() < 0.25) {
                    pos = step(pos, branch);
                    segment = 0;
                }
                j += 1;
            }
        }
        true
    }
}
