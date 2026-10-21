//! The void-world spawn platform: stone around the origin column's chunk
//! neighbourhood with one cobblestone block at the exact centre.

use std::sync::Arc;

use tuff_vanilla_data::Block;

use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::obj;
use tuff_world_model::BlockPos;

const PLATFORM_X: i32 = 8;
const PLATFORM_Z: i32 = 8;
const PLATFORM_DY: i32 = 3;
const RADIUS: i32 = 16;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:void_start_platform", |_, v| {
        obj(v, &[], "void_start_platform")?;
        Ok(Arc::new(VoidStartPlatform))
    });
}

struct VoidStartPlatform;

fn chebyshev(xa: i32, za: i32, xb: i32, zb: i32) -> i32 {
    (xa - xb).abs().max((za - zb).abs())
}

impl Feature for VoidStartPlatform {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let (cx, cz) = (origin.x >> 4, origin.z >> 4);
        if chebyshev(cx, cz, 0, 0) > 1 {
            return true;
        }
        let y = origin.y + PLATFORM_DY;
        let stone = Block::Stone.default_state();
        let cobble = Block::Cobblestone.default_state();
        for z in (cz << 4)..=((cz << 4) + 15) {
            for x in (cx << 4)..=((cx << 4) + 15) {
                if chebyshev(PLATFORM_X, PLATFORM_Z, x, z) <= RADIUS {
                    let centre = x == PLATFORM_X && z == PLATFORM_Z;
                    ctx.level
                        .set_block(BlockPos::new(x, y, z), if centre { cobble } else { stone });
                }
            }
        }
        true
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

    fn region() -> Region<'static> {
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
        Region::live(ChunkPos::new(0, 0), chunks, span, 1, 1)
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_platform_is_stone_with_a_cobblestone_centre() {
        let mut region = region();
        let feature = Registry::builtin()
            .feature(&json!({"type": "minecraft:void_start_platform"}))
            .unwrap();
        let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(1)));
        for (cx, cz) in [(0, 0), (-1, 0), (0, -1)] {
            let mut ctx = PlaceContext {
                level: &mut region,
                random: &mut random,
            };
            assert!(feature.place(&mut ctx, BlockPos::new(cx * 16, 60, cz * 16)));
        }
        assert_eq!(
            region.block(BlockPos::new(8, 63, 8)).block(),
            Block::Cobblestone
        );
        assert_eq!(region.block(BlockPos::new(0, 63, 0)).block(), Block::Stone);
        assert_eq!(region.block(BlockPos::new(-8, 63, 8)).block(), Block::Stone);
        assert_eq!(region.block(BlockPos::new(8, 62, 8)).block(), Block::Air);
    }
}
