//! Where players appear: the column search that places a new world's
//! spawn and the randomized search within the respawn radius that places a
//! player around it, as pure functions of block and heightmap lookups.

use tuff_vanilla_data::BlockState;

use tuff_core::world::heightmap_kind::HeightmapKind;
use tuff_core::world::physics;
use tuff_world_model::BlockPos;

/// The most candidates the respawn radius search tries.
const MAX_CANDIDATES: i64 = 1024;
const PLAYER_WIDTH: f64 = 0.6;
const PLAYER_HEIGHT: f64 = 1.8;

/// A heightmap's value (the lowest y above every block it counts) for the
/// column at `x`, `z`, scanning down from `max_y`.
pub fn column_height(
    kind: HeightmapKind,
    x: i32,
    z: i32,
    min_y: i32,
    max_y: i32,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> i32 {
    (min_y..max_y)
        .rev()
        .find(|&y| block(BlockPos::new(x, y, z)).is_some_and(|state| kind.is_opaque(state)))
        .map_or(min_y, |y| y + 1)
}

/// Whether a collision shape covers the whole top of its block.
fn full_top(state: BlockState) -> bool {
    state
        .collision_boxes()
        .iter()
        .any(|b| b[0] <= 0.0 && b[2] <= 0.0 && b[3] >= 1.0 && b[5] >= 1.0 && b[4] >= 1.0)
}

/// Where a player may stand in the column at `x`, `z`: on the highest
/// block with a full top below the motion-blocking surface, unless the
/// column is under water. `spawn_height` replaces the surface in worlds
/// with a ceiling.
pub fn respawn_pos(
    x: i32,
    z: i32,
    min_y: i32,
    spawn_height: Option<i32>,
    heights: &dyn Fn(HeightmapKind, i32, i32) -> i32,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Option<BlockPos> {
    let top = spawn_height.unwrap_or_else(|| heights(HeightmapKind::MotionBlocking, x, z));
    if top < min_y {
        return None;
    }
    let surface = heights(HeightmapKind::WorldSurface, x, z);
    if surface <= top && surface > heights(HeightmapKind::OceanFloor, x, z) {
        return None;
    }
    for y in (min_y..=top + 1).rev() {
        let pos = BlockPos::new(x, y, z);
        let state = block(pos)?;
        if !state.fluid_state().is_empty() {
            return None;
        }
        if full_top(state) {
            return Some(BlockPos::new(x, y + 1, z));
        }
    }
    None
}

/// The first column of a chunk, in x then z order, a player may stand in.
pub fn spawn_in_chunk(
    min_x: i32,
    min_z: i32,
    min_y: i32,
    heights: &dyn Fn(HeightmapKind, i32, i32) -> i32,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Option<BlockPos> {
    for x in min_x..min_x + 16 {
        for z in min_z..min_z + 16 {
            if let Some(pos) = respawn_pos(x, z, min_y, None, heights, block) {
                return Some(pos);
            }
        }
    }
    None
}

/// Chunk offsets around a new world's spawn chunk, spiralling out over
/// 11 by 11 chunks.
pub fn spiral() -> Vec<(i32, i32)> {
    let (mut x, mut z, mut dx, mut dz) = (0, 0, 0, -1);
    let mut out = Vec::with_capacity(121);
    for _ in 0..121 {
        if (-5..=5).contains(&x) && (-5..=5).contains(&z) {
            out.push((x, z));
        }
        if x == z || (x < 0 && x == -z) || (x > 0 && x == 1 - z) {
            (dx, dz) = (-dz, dx);
        }
        x += dx;
        z += dz;
    }
    out
}

/// The columns the respawn radius search tries around `center`, in order:
/// a coprime stride through the square from a random `offset`.
pub fn candidates(center: BlockPos, radius: i32, offset: u32) -> Vec<(i32, i32)> {
    let side = i64::from(radius) * 2 + 1;
    let count = (side * side).min(MAX_CANDIDATES) as i32;
    let coprime = if count <= 16 { count - 1 } else { 17 };
    let offset = offset as i32 % count.max(1);
    (0..count)
        .map(|index| {
            let value = (offset + coprime * index) % count;
            let width = radius * 2 + 1;
            (
                center.x + value % width - radius,
                center.z + value / width - radius,
            )
        })
        .collect()
}

/// Whether a player standing at the bottom centre of `pos` collides with
/// no block and touches no liquid.
pub fn fits(pos: BlockPos, block: &dyn Fn(BlockPos) -> Option<BlockState>) -> bool {
    let (x, z) = (f64::from(pos.x) + 0.5, f64::from(pos.z) + 0.5);
    let half = PLAYER_WIDTH / 2.0;
    let area = [
        x - half,
        f64::from(pos.y),
        z - half,
        x + half,
        f64::from(pos.y) + PLAYER_HEIGHT,
        z + half,
    ];
    if !physics::no_collision(area, block) {
        return false;
    }
    for bx in area[0].floor() as i32..=area[3].floor() as i32 {
        for by in area[1].floor() as i32..=area[4].floor() as i32 {
            for bz in area[2].floor() as i32..=area[5].floor() as i32 {
                let Some(state) = block(BlockPos::new(bx, by, bz)) else {
                    return false;
                };
                if !state.fluid_state().is_empty() {
                    return false;
                }
            }
        }
    }
    true
}

/// `pos` moved up out of anything it is stuck in, then down onto the
/// ground: the spawn when no candidate worked.
pub fn fixup_height(
    pos: BlockPos,
    min_y: i32,
    max_y: i32,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> BlockPos {
    let mut y = pos.y;
    while !fits(BlockPos::new(pos.x, y, pos.z), block) && y < max_y {
        y += 1;
    }
    y -= 1;
    while fits(BlockPos::new(pos.x, y, pos.z), block) && y > min_y {
        y -= 1;
    }
    BlockPos::new(pos.x, y + 1, pos.z)
}

/// The respawn radius search around the world spawn `center`, over the
/// blocks `world` sees.
pub fn find_spawn(
    world: &dyn tuff_core::world::region_api::RegionWorld,
    center: BlockPos,
    radius: i32,
    offset: u32,
) -> [f64; 3] {
    let dimension = world.dimension_info();
    let shape = dimension.shape;
    let (min_y, max_y) = (shape.min_y, shape.min_y + shape.height);
    let ceiling = dimension.kind.has_ceiling.then_some(64);
    let block = |pos: BlockPos| world.block(pos);
    let heights = |kind, x, z| column_height(kind, x, z, min_y, max_y, &block);
    for (x, z) in candidates(center, radius, offset) {
        if let Some(pos) = respawn_pos(x, z, min_y, ceiling, &heights, &block)
            && fits(pos, &block)
        {
            return [
                f64::from(pos.x) + 0.5,
                f64::from(pos.y),
                f64::from(pos.z) + 0.5,
            ];
        }
    }
    let pos = fixup_height(center, min_y, max_y, &block);
    [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    #[tuff_core::watchdog]
    #[test]
    fn the_spiral_covers_eleven_chunks_square_from_the_centre() {
        let spiral = spiral();
        assert_eq!(spiral.len(), 121);
        assert_eq!(&spiral[..3], &[(0, 0), (1, 0), (1, 1)]);
        let mut sorted = spiral.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 121);
    }

    #[tuff_core::watchdog]
    #[test]
    fn candidates_visit_every_column_once() {
        let center = BlockPos::new(100, 64, -40);
        let list = candidates(center, 10, 12345);
        assert_eq!(list.len(), 441);
        let mut sorted = list.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 441);
        assert!(
            list.iter()
                .all(|&(x, z)| (x - 100).abs() <= 10 && (z + 40).abs() <= 10)
        );
        assert_eq!(candidates(center, 0, 7), vec![(100, -40)]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn players_stand_on_solid_ground_and_never_in_water() {
        let block = |pos: BlockPos| {
            Some(match (pos.x, pos.y) {
                (_, y) if y < 60 => Block::Stone.default_state(),
                (0, 60) => Block::Water.default_state(),
                (1, 60) => Block::GrassBlock.default_state(),
                _ => Block::Air.default_state(),
            })
        };
        let heights = |kind, x, z| column_height(kind, x, z, 0, 100, &block);
        assert_eq!(heights(HeightmapKind::MotionBlocking, 1, 0), 61);
        assert_eq!(
            respawn_pos(1, 0, 0, None, &heights, &block),
            Some(BlockPos::new(1, 61, 0))
        );
        assert_eq!(respawn_pos(0, 0, 0, None, &heights, &block), None);
        assert!(fits(BlockPos::new(1, 61, 0), &block));
        assert!(!fits(BlockPos::new(0, 60, 0), &block));
        assert_eq!(
            fixup_height(BlockPos::new(1, 50, 0), 0, 100, &block),
            BlockPos::new(1, 61, 0)
        );
    }
}
