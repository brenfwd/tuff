//! End features: the obsidian spikes around the main island, small
//! floating islands, gateways, chorus plants and the arrival platform.

use std::sync::Arc;

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::step;
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::{self, obj};
use tuff_item::random::RandomSource;
use tuff_nbt::{Compound, Tag};
pub use tuff_vanilla_common::end_spikes::{
    EndSpike, SpikeBlocks, island_blocks, podium_blocks, spike_blocks, spike_crystal,
    spikes_for_seed,
};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:end_spike", |_, v| {
        let o = obj(
            v,
            &["spikes", "crystal_invulnerable", "crystal_beam_target"],
            "end_spike",
        )?;
        let mut spikes = Vec::new();
        for spike in o.list("spikes")? {
            let s = obj(
                spike,
                &["centerX", "centerZ", "radius", "height", "guarded"],
                "end spike",
            )?;
            spikes.push(EndSpike {
                center_x: s.i32_or("centerX", 0)?,
                center_z: s.i32_or("centerZ", 0)?,
                radius: s.i32_or("radius", 0)?,
                height: s.i32_or("height", 0)?,
                guarded: s.bool_or("guarded", false)?,
            });
        }
        let crystal_invulnerable = o.bool_or("crystal_invulnerable", false)?;
        let crystal_beam_target = o.get("crystal_beam_target").map(json::vec3i).transpose()?;
        Ok(Arc::new(EndSpikes {
            spikes,
            crystal_invulnerable,
            crystal_beam_target,
        }))
    });
    registry.register("minecraft:end_island", |_, v| {
        obj(v, &[], "end_island")?;
        Ok(Arc::new(EndIsland))
    });
    registry.register("minecraft:end_gateway", |_, v| {
        let o = obj(v, &["exit", "exact"], "end_gateway")?;
        Ok(Arc::new(EndGateway {
            exit: o.get("exit").map(json::vec3i).transpose()?,
            exact: o.bool("exact")?,
        }))
    });
    registry.register("minecraft:end_podium", |_, v| {
        let o = obj(v, &["active"], "end_podium")?;
        Ok(Arc::new(EndPodium {
            active: o.bool_or("active", false)?,
        }))
    });
    registry.register("minecraft:chorus_plant", |_, v| {
        obj(v, &[], "chorus_plant")?;
        Ok(Arc::new(ChorusPlant))
    });
    registry.register("minecraft:end_platform", |_, v| {
        obj(v, &[], "end_platform")?;
        Ok(Arc::new(EndPlatform))
    });
}

struct EndSpikes {
    spikes: Vec<EndSpike>,
    crystal_invulnerable: bool,
    crystal_beam_target: Option<[i32; 3]>,
}

impl EndSpikes {
    fn place_spike(&self, ctx: &mut PlaceContext, spike: &EndSpike) {
        let (blocks, fire) = spike_blocks(spike, ctx.level.min_y());
        for (pos, state) in blocks {
            ctx.level.set_block(pos, state);
        }
        let yaw = ctx.random.next_float() * 360.0;
        let crystal = spike.crystal();
        ctx.level.add_entity(
            crystal,
            spike_crystal(
                spike,
                yaw,
                self.crystal_invulnerable,
                self.crystal_beam_target,
            ),
        );
        for (pos, state) in fire {
            ctx.level.set_block(pos, state);
        }
    }
}

impl Feature for EndSpikes {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let spikes = if self.spikes.is_empty() {
            spikes_for_seed(ctx.level.seed())
        } else {
            self.spikes.clone()
        };
        for spike in &spikes {
            if spike.center_x >> 4 == origin.x >> 4 && spike.center_z >> 4 == origin.z >> 4 {
                self.place_spike(ctx, spike);
            }
        }
        true
    }
}

struct EndIsland;

impl Feature for EndIsland {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let end_stone = Block::EndStone.default_state();
        for pos in island_blocks(ctx.random, origin) {
            ctx.level.set_block(pos, end_stone);
        }
        true
    }
}

struct EndGateway {
    exit: Option<[i32; 3]>,
    exact: bool,
}

impl Feature for EndGateway {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let air = Block::Air.default_state();
        let bedrock = Block::Bedrock.default_state();
        for z in origin.z - 1..=origin.z + 1 {
            for y in origin.y - 2..=origin.y + 2 {
                for x in origin.x - 1..=origin.x + 1 {
                    let pos = BlockPos::new(x, y, z);
                    let same_x = x == origin.x;
                    let same_y = y == origin.y;
                    let same_z = z == origin.z;
                    let end = (y - origin.y).abs() == 2;
                    let state = if same_x && same_y && same_z {
                        Block::EndGateway.default_state()
                    } else if same_y {
                        air
                    } else if end && same_x && same_z {
                        bedrock
                    } else if (!same_x && !same_z) || end {
                        air
                    } else {
                        bedrock
                    };
                    ctx.level.set_block(pos, state);
                    if state.block() == Block::EndGateway {
                        ctx.level.set_block_entity(pos, self.block_entity());
                    }
                }
            }
        }
        true
    }
}

impl EndGateway {
    fn block_entity(&self) -> Compound {
        let mut data = Compound::new();
        data.insert("id", Tag::String("minecraft:end_gateway".into()));
        data.insert("Age", Tag::Long(0));
        if let Some([x, y, z]) = self.exit {
            data.insert("exit_portal", Tag::IntArray(vec![x, y, z]));
            if self.exact {
                data.insert("ExactTeleport", Tag::Byte(1));
            }
        }
        data
    }
}

struct ChorusPlant;

fn is_chorus(state: BlockState) -> bool {
    matches!(state.block(), Block::ChorusPlant | Block::ChorusFlower)
}

fn supports_chorus(state: BlockState) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::block::key::SUPPORTS_CHORUS_PLANT,
        state.block(),
    )
}

const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

fn chorus_plant_state(level: &Region, pos: BlockPos) -> BlockState {
    let flag = |b: bool| if b { "true" } else { "false" };
    let mut state = Block::ChorusPlant.default_state();
    let down = level.block(step(pos, Facing::Down));
    state = state
        .with("down", flag(is_chorus(down) || supports_chorus(down)))
        .unwrap_or(state);
    for direction in [
        Facing::Up,
        Facing::North,
        Facing::East,
        Facing::South,
        Facing::West,
    ] {
        let neighbour = level.block(step(pos, direction));
        state = state
            .with(direction.name(), flag(is_chorus(neighbour)))
            .unwrap_or(state);
    }
    state
}

fn set_chorus_plant(level: &mut Region, pos: BlockPos) {
    let state = chorus_plant_state(level, pos);
    level.set_block(pos, state);
}

fn all_neighbours_empty(level: &Region, pos: BlockPos, ignore: Option<Facing>) -> bool {
    HORIZONTAL
        .iter()
        .all(|d| Some(*d) == ignore || is_air(level.block(step(pos, *d))))
}

struct ChorusBranch {
    top: BlockPos,
    depth: i32,
    sprouts_left: i32,
    sprouted: bool,
}

const CHORUS_MAX_DEPTH: i32 = 4;

fn raise_chorus_column(ctx: &mut PlaceContext, base: BlockPos, depth: i32) -> Option<ChorusBranch> {
    let length = ctx.random.next_int_bounded(4) + 1 + i32::from(depth == 0);
    for rise in 1..=length {
        let pos = BlockPos::new(base.x, base.y + rise, base.z);
        if !all_neighbours_empty(ctx.level, pos, None) {
            return None;
        }
        set_chorus_plant(ctx.level, pos);
        set_chorus_plant(ctx.level, step(pos, Facing::Down));
    }
    let sprouts_left = if depth < CHORUS_MAX_DEPTH {
        ctx.random.next_int_bounded(4) + i32::from(depth == 0)
    } else {
        0
    };
    Some(ChorusBranch {
        top: BlockPos::new(base.x, base.y + length, base.z),
        depth,
        sprouts_left,
        sprouted: false,
    })
}

fn grow_chorus(ctx: &mut PlaceContext, root: BlockPos, max_spread: i32) {
    let mut open: Vec<ChorusBranch> = Vec::with_capacity(CHORUS_MAX_DEPTH as usize + 1);
    open.extend(raise_chorus_column(ctx, root, 0));
    while let Some(branch) = open.last_mut() {
        if branch.sprouts_left == 0 {
            let done = open.pop().expect("non-empty");
            if !done.sprouted {
                let flower = Block::ChorusFlower.default_state();
                ctx.level
                    .set_block(done.top, flower.with("age", "5").unwrap_or(flower));
            }
            continue;
        }
        branch.sprouts_left -= 1;
        let (top, depth) = (branch.top, branch.depth);
        let toward = HORIZONTAL[ctx.random.next_int_bounded(4) as usize];
        let back = toward.opposite();
        let shoot = step(top, toward);
        let fits = (shoot.x - root.x).abs() < max_spread
            && (shoot.z - root.z).abs() < max_spread
            && is_air(ctx.level.block(shoot))
            && is_air(ctx.level.block(step(shoot, Facing::Down)))
            && all_neighbours_empty(ctx.level, shoot, Some(back));
        if !fits {
            continue;
        }
        branch.sprouted = true;
        set_chorus_plant(ctx.level, shoot);
        set_chorus_plant(ctx.level, step(shoot, back));
        open.extend(raise_chorus_column(ctx, shoot, depth + 1));
    }
}

impl Feature for ChorusPlant {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if !is_air(ctx.level.block(origin))
            || !supports_chorus(ctx.level.block(step(origin, Facing::Down)))
        {
            return false;
        }
        set_chorus_plant(ctx.level, origin);
        grow_chorus(ctx, origin, 8);
        true
    }
}

struct EndPodium {
    active: bool,
}

impl Feature for EndPodium {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        for (pos, state, _) in podium_blocks(origin, self.active) {
            ctx.level.set_block(pos, state);
        }
        true
    }
}

struct EndPlatform;

impl Feature for EndPlatform {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        for dz in -2..=2 {
            for dx in -2..=2 {
                for dy in -1..3 {
                    let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                    let block = if dy == -1 {
                        Block::Obsidian
                    } else {
                        Block::Air
                    };
                    if ctx.level.block(pos).block() != block {
                        ctx.level.set_block(pos, block.default_state());
                    }
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn end_podium_is_a_registered_feature_with_an_active_flag() {
        let mut registry = Registry::builtin();
        for active in [true, false] {
            let value = serde_json::json!({
                "type": "minecraft:end_podium",
                "active": active,
            });
            registry.feature(&value).unwrap();
        }
        let bad = serde_json::json!({
            "type": "minecraft:end_podium",
            "active": true,
            "other": 1,
        });
        assert!(registry.feature(&bad).is_err());
        assert!(!registry.missing.contains("minecraft:end_podium"));
    }
}
