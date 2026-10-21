//! The spawn bonus chest: one chest with the bonus loot table on the spawn
//! chunk's first free column, ringed with torches where they can stand.

use std::sync::Arc;

use tuff_vanilla_data::Block;
use tuff_vanilla_data::block::Facing;

use crate::feature::predicate::step;
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::obj;
use crate::proto::HeightmapKind;
use tuff_item::random::RandomSource;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const LOOT_TABLE: &str = "minecraft:chests/spawn_bonus_chest";
const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:bonus_chest", |_, v| {
        obj(v, &[], "bonus_chest")?;
        Ok(Arc::new(BonusChest))
    });
}

struct BonusChest;

fn shuffled_range(start: i32, random: &mut impl RandomSource) -> Vec<i32> {
    let mut list: Vec<i32> = (start..start + 16).collect();
    for i in (2..=list.len()).rev() {
        let swap_to = random.next_int_bounded(i as i32) as usize;
        list.swap(i - 1, swap_to);
    }
    list
}

impl Feature for BonusChest {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let (min_x, min_z) = (origin.x & !15, origin.z & !15);
        let xs = shuffled_range(min_x, &mut *ctx.random);
        let zs = shuffled_range(min_z, &mut *ctx.random);
        for &x in &xs {
            for &z in &zs {
                let y = ctx
                    .level
                    .height(HeightmapKind::MotionBlockingNoLeaves, x, z);
                let pos = BlockPos::new(x, y, z);
                let state = ctx.level.block(pos);
                if !is_air(state) && !state.collision_boxes().is_empty() {
                    continue;
                }
                ctx.level.set_block(pos, Block::Chest.default_state());
                let seed = ctx.random.next_long();
                ctx.level.set_block_entity(
                    pos,
                    Compound::new()
                        .with("id", "minecraft:chest")
                        .with("LootTable", LOOT_TABLE)
                        .with("LootTableSeed", seed),
                );
                let torch = Block::Torch.default_state();
                for direction in HORIZONTAL {
                    let torch_pos = step(pos, direction);
                    if ctx.level.can_survive(torch, torch_pos) {
                        ctx.level.set_block(torch_pos, torch);
                    }
                }
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::level::Region;
    use crate::proto::ProtoChunk;
    use crate::surface::GenContext;
    use serde_json::json;
    use tuff_item::random::{Random, WorldgenRandom, XoroshiroRandom};
    use tuff_world_model::chunk::ChunkShape;
    use tuff_world_model::pos::ChunkPos;

    #[tuff_core::watchdog]
    #[test]
    fn a_chest_with_the_bonus_loot_and_torches_lands_in_the_spawn_chunk() {
        let shape = ChunkShape::OVERWORLD;
        let chunks: Vec<ProtoChunk> = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| ChunkPos::new(dx, dz)))
            .map(|pos| ProtoChunk::new(pos, shape))
            .collect();
        let span = GenContext {
            min_y: shape.min_y,
            height: shape.height,
            sea_level: 63,
        };
        let mut region = Region::live(ChunkPos::new(0, 0), chunks, span, 1, 1);
        let stone = Block::Stone.default_state();
        for x in -16..32 {
            for z in -16..32 {
                region.set_block(BlockPos::new(x, 64, z), stone);
            }
        }
        let feature = Registry::builtin()
            .feature(&json!({"type": "minecraft:bonus_chest"}))
            .unwrap();
        let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(7)));
        let mut ctx = PlaceContext {
            level: &mut region,
            random: &mut random,
        };
        assert!(feature.place(&mut ctx, BlockPos::new(5, 65, 5)));
        let chest = (0..16)
            .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, 65, z)))
            .find(|pos| region.block(*pos).block() == Block::Chest)
            .expect("a chest in the spawn chunk");
        let entity = region
            .block_entity_mut(chest)
            .expect("block entity")
            .clone();
        assert_eq!(entity.get_str("LootTable"), Some(LOOT_TABLE));
        let torches = HORIZONTAL
            .iter()
            .filter(|d| region.block(step(chest, **d)).block() == Block::Torch)
            .count();
        assert!(torches >= 1);
    }
}
