//! Explosions as pure functions: which blocks an explosion breaks (rays
//! from the center losing power to block resistance), how much of an
//! entity it sees, and the damage and push it deals. The region applies
//! the results (`region/explosions.rs`); what sets explosions off (TNT,
//! creepers, fireballs, ...) belongs to features.

use std::collections::HashSet;

use rand::Rng;
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState};

use crate::world::messages::Igniter;
use crate::world::raycast;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const RAY_STEP: f64 = 0.30000001192092896;
const RAY_STEP_LOSS: f32 = 0.22500001;
/// Explosions this small (or ones that leave blocks alone) use the small
/// particle.
pub const LARGE_EXPLOSION_RADIUS: f32 = 2.0;

/// The resistance a block puts up against an explosion ray: nothing for
/// air, otherwise the larger of the block's and its fluid's.
pub fn resistance(state: BlockState) -> Option<f32> {
    let fluid = raycast::fluid(state).map(|(lava, _, _)| {
        if lava {
            Block::Lava.explosion_resistance()
        } else {
            Block::Water.explosion_resistance()
        }
    });
    if is_air(state) && fluid.is_none() {
        return None;
    }
    Some(
        state
            .block()
            .explosion_resistance()
            .max(fluid.unwrap_or(0.0)),
    )
}

/// The positions an explosion of `radius` at `center` breaks. `block`
/// answers `None` where the world is not known (an unloaded chunk), which
/// stops that ray, as the game stops at the edge of the world. Positions
/// come in the order they are first reached.
pub fn exploded_positions(
    center: [f64; 3],
    radius: f32,
    rng: &mut impl Rng,
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> Vec<BlockPos> {
    exploded_positions_with(center, radius, rng, block, |_, state| resistance(state))
}

/// [`exploded_positions`] with each block's resistance from `resist`.
pub fn exploded_positions_with(
    center: [f64; 3],
    radius: f32,
    rng: &mut impl Rng,
    block: impl Fn(BlockPos) -> Option<BlockState>,
    resist: impl Fn(BlockPos, BlockState) -> Option<f32>,
) -> Vec<BlockPos> {
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    for x in 0..16 {
        for y in 0..16 {
            for z in 0..16 {
                if !(x == 0 || x == 15 || y == 0 || y == 15 || z == 0 || z == 15) {
                    continue;
                }
                let direction = [x, y, z].map(|c| (c as f32 / 15.0 * 2.0 - 1.0) as f64);
                let length = direction.iter().map(|d| d * d).sum::<f64>().sqrt();
                let step = direction.map(|d| d / length * RAY_STEP);
                let mut power = radius * (0.7 + rng.r#gen::<f32>() * 0.6);
                let mut at = center;
                while power > 0.0 {
                    let pos = BlockPos::new(
                        at[0].floor() as i32,
                        at[1].floor() as i32,
                        at[2].floor() as i32,
                    );
                    let Some(state) = block(pos) else {
                        break;
                    };
                    if let Some(resistance) = resist(pos, state) {
                        power -= (resistance + 0.3) * 0.3;
                    }
                    if power > 0.0 && seen.insert(pos) {
                        order.push(pos);
                    }
                    for axis in 0..3 {
                        at[axis] += step[axis];
                    }
                    power -= RAY_STEP_LOSS;
                }
            }
        }
    }
    order
}

/// The share of sample points spread over `bounds` (`[min_x, min_y, min_z,
/// max_x, max_y, max_z]`) that see `center` past block collision shapes.
pub fn seen_percent(
    center: [f64; 3],
    bounds: [f64; 6],
    level: crate::world::placement::Level,
) -> f32 {
    let size = [
        bounds[3] - bounds[0],
        bounds[4] - bounds[1],
        bounds[5] - bounds[2],
    ];
    let steps = size.map(|s| 1.0 / (s * 2.0 + 1.0));
    let x_offset = (1.0 - (1.0 / steps[0]).floor() * steps[0]) / 2.0;
    let z_offset = (1.0 - (1.0 / steps[2]).floor() * steps[2]) / 2.0;
    let mut hits = 0;
    let mut count = 0;
    let mut xx = 0.0;
    while xx <= 1.0 {
        let mut yy = 0.0;
        while yy <= 1.0 {
            let mut zz = 0.0;
            while zz <= 1.0 {
                let from = [
                    bounds[0] + xx * size[0] + x_offset,
                    bounds[1] + yy * size[1],
                    bounds[2] + zz * size[2] + z_offset,
                ];
                if raycast::cast_colliders(from, center, level).is_none() {
                    hits += 1;
                }
                count += 1;
                zz += steps[2];
            }
            yy += steps[1];
        }
        xx += steps[0];
    }
    if count == 0 {
        0.0
    } else {
        hits as f32 / count as f32
    }
}

/// What an explosion does to an entity: its damage and push, or `None` if
/// the entity is out of reach. `origin` is where the push aims from (a
/// living entity's eyes).
pub fn impact(
    center: [f64; 3],
    radius: f32,
    position: [f64; 3],
    origin: [f64; 3],
    exposure: f32,
) -> Option<(f32, [f64; 3])> {
    let reach = radius as f64 * 2.0;
    let distance = crate::world::distance_sq(position, center).sqrt() / reach;
    if distance > 1.0 {
        return None;
    }
    let power = (1.0 - distance) * exposure as f64;
    let damage = ((power * power + power) / 2.0 * 7.0 * reach + 1.0) as f32;
    let direction = [0, 1, 2].map(|i| origin[i] - center[i]);
    let length = direction.iter().map(|d| d * d).sum::<f64>().sqrt();
    let push = if length < 1.0e-4 {
        [0.0; 3]
    } else {
        direction.map(|d| d / length * power)
    };
    Some((damage, push))
}

/// What sets a wind burst apart from a blast: it hurts nobody, its rays
/// pass through every block but those tagged to stop it, and its push is
/// scaled by `knockback`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindBurst {
    pub knockback: f32,
    pub sound: SoundEvent,
    /// The wind charge type that burst.
    pub source: tuff_vanilla_data::registry::EntityType,
    /// A player's wind charge: players it launches take fall damage only
    /// for the drop below where it hit them.
    pub spares_fall: bool,
}

/// What an explosion does to blocks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlockInteraction {
    Keep,
    /// A wind burst: breaks nothing but flips the levers and presses the
    /// buttons it reaches.
    Trigger(WindBurst),
    Destroy,
    /// Drops thin out with the explosion's radius.
    DestroyWithDecay,
    /// Destroys as the others do, but rails and the blocks under them put
    /// up no resistance and stay (a primed TNT minecart's blast).
    SpareRails {
        decay: bool,
    },
}

impl BlockInteraction {
    /// The wind burst this is, if any.
    pub fn wind(self) -> Option<WindBurst> {
        match self {
            Self::Trigger(wind) => Some(wind),
            _ => None,
        }
    }

    /// Whether drops thin out with the explosion's radius.
    pub fn decays(self) -> bool {
        matches!(
            self,
            Self::DestroyWithDecay | Self::SpareRails { decay: true }
        )
    }
}

/// An explosion for a region to set off.
#[derive(Debug, Clone)]
pub struct Explosion {
    pub center: [f64; 3],
    pub radius: f32,
    pub fire: bool,
    pub blocks: BlockInteraction,
    pub damage_type: &'static DamageType,
    pub igniter: Option<Igniter>,
    /// The mob to blame when no player lit it (a creeper, a ghast).
    pub culprit: Option<Box<crate::world::entity::Culprit>>,
    /// A dangerous wither skull: blocks the wither can destroy resist at
    /// most 0.8.
    pub dangerous_wither_skull: bool,
    /// TNT that went through a portal: nether portal blocks neither resist
    /// nor break.
    pub spares_nether_portals: bool,
}

/// What blew up a block an explosion destroys, as features see it.
#[derive(Debug, Clone)]
pub struct ExplosionCause {
    pub center: [f64; 3],
    pub damage_type: &'static DamageType,
    pub igniter: Option<Igniter>,
    pub culprit: Option<crate::world::entity::Culprit>,
}

impl Explosion {
    pub fn cause(&self) -> ExplosionCause {
        ExplosionCause {
            center: self.center,
            damage_type: self.damage_type,
            igniter: self.igniter.clone(),
            culprit: self.culprit.as_deref().cloned(),
        }
    }
}

/// Lights primed TNT for the region: when redstone, fire or an explosion
/// sets TNT off, core asks the feature that owns the TNT entity to make one.
/// With no feature, TNT that would light simply disappears.
pub trait TntPrimer: Send + Sync {
    /// Primed TNT at `pos` with `fuse` ticks, or the feature's usual fuse
    /// when `None`.
    fn prime(
        &self,
        id: i32,
        uuid: u128,
        pos: BlockPos,
        fuse: Option<i32>,
        igniter: Option<Igniter>,
        rng: &mut rand::rngs::StdRng,
    ) -> Box<dyn crate::world::entity::RegionEntity>;

    /// The fuse of TNT caught in another explosion.
    fn chain_fuse(&self, rng: &mut rand::rngs::StdRng) -> i32;
}

/// The block rules of an explosion: what its rays meet, what it leaves
/// standing, what fire it lights and what a wind burst does to the blocks
/// it reaches. Core runs the rays and applies the results; filled by
/// `vanilla-combat`. With no feature rays meet each block's own
/// resistance, nothing is spared, nothing burns and wind moves no block.
pub trait ExplosionRules: Send + Sync {
    /// The resistance a ray of `explosion` meets at `pos`, `None` where it
    /// passes freely. `block` reads the world around.
    fn ray_resistance(
        &self,
        _explosion: &Explosion,
        _pos: BlockPos,
        state: BlockState,
        _block: &dyn Fn(BlockPos) -> Option<BlockState>,
    ) -> Option<f32> {
        resistance(state)
    }

    /// Whether `explosion` leaves the block at `pos` standing and puts up
    /// no resistance there.
    fn spares(
        &self,
        _explosion: &Explosion,
        _pos: BlockPos,
        _block: &dyn Fn(BlockPos) -> Option<BlockState>,
    ) -> bool {
        false
    }

    /// The fire an incendiary explosion lights on `ground`.
    fn fire(&self, _ground: BlockState) -> Option<BlockState> {
        None
    }

    /// A wind burst reached `state` at `pos`; true when the rules handled
    /// it, otherwise the block's own behaviour reacts.
    fn wind_reached(
        &self,
        _world: &mut dyn crate::world::region_api::RegionWorld,
        _pos: BlockPos,
        _state: BlockState,
    ) -> bool {
        false
    }
}

/// The rules an inert core uses.
pub struct NoExplosionRules;

impl ExplosionRules for NoExplosionRules {}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;

    fn stone_below(pos: BlockPos) -> Option<BlockState> {
        Some(if pos.y < 64 {
            Block::Stone.default_state()
        } else if pos.y < 66 && pos.x == 3 {
            Block::Obsidian.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn explosions_carve_stone_but_not_obsidian() {
        let mut rng = StdRng::seed_from_u64(1);
        let blown = exploded_positions([0.5, 64.0, 0.5], 4.0, &mut rng, stone_below);
        let stone: Vec<&BlockPos> = blown.iter().filter(|p| p.y < 64).collect();
        assert!(!stone.is_empty(), "the floor is blown open");
        assert!(stone.iter().all(|p| p.y >= 60), "stone stops the rays");
        assert!(!blown.iter().any(|p| p.x == 3 && p.y >= 64 && p.y < 66));
        let unknown = |pos: BlockPos| (pos.x < 2).then(|| Block::Air.default_state());
        let mut rng = StdRng::seed_from_u64(1);
        let blown = exploded_positions([0.5, 64.0, 0.5], 4.0, &mut rng, unknown);
        assert!(blown.iter().all(|p| p.x < 2), "rays stop at unknown blocks");
        assert_eq!(resistance(Block::Water.default_state()), Some(100.0));
        assert_eq!(resistance(Block::Air.default_state()), None);
        let waterlogged = Block::OakSlab
            .default_state()
            .with_waterlogged(true)
            .unwrap();
        assert_eq!(resistance(waterlogged), Some(100.0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_blast_centred_on_the_ground_is_not_hidden_by_that_ground() {
        let floor = |p: BlockPos| {
            if p.y < 64 {
                Block::GrassBlock.default_state()
            } else {
                Block::Air.default_state()
            }
        };
        let center = [0.5, 64.0, 0.5];
        let bounds = [2.2, 64.0, 0.2, 2.8, 65.8, 0.8];
        assert_eq!(seen_percent(center, bounds, &floor), 1.0);
        let (damage, _) = impact(center, 3.0, [2.5, 64.0, 0.5], [2.5, 65.62, 0.5], 1.0).unwrap();
        assert!(damage > 20.0, "a creeper two blocks away does {damage}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn exposure_damage_and_push_fall_off_with_distance_and_cover() {
        let open = |_: BlockPos| Block::Air.default_state();
        let bounds = [1.7, 64.0, 0.2, 2.3, 65.8, 0.8];
        assert_eq!(seen_percent([0.5, 64.5, 0.5], bounds, &open), 1.0);
        let wall = |p: BlockPos| {
            if p.x == 1 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            }
        };
        assert_eq!(seen_percent([0.5, 64.5, 0.5], bounds, &wall), 0.0);
        let (close, push) = impact(
            [0.5, 64.0, 0.5],
            4.0,
            [2.0, 64.0, 0.5],
            [2.0, 65.62, 0.5],
            1.0,
        )
        .unwrap();
        let (far, _) = impact(
            [0.5, 64.0, 0.5],
            4.0,
            [6.0, 64.0, 0.5],
            [6.0, 65.62, 0.5],
            1.0,
        )
        .unwrap();
        assert!(close > far && far >= 1.0);
        assert!(push[0] > 0.0 && push[1] > 0.0);
        assert!(
            impact(
                [0.5, 64.0, 0.5],
                4.0,
                [9.0, 64.0, 0.5],
                [9.0, 65.0, 0.5],
                1.0
            )
            .is_none()
        );
        let (shielded, _) = impact(
            [0.5, 64.0, 0.5],
            4.0,
            [2.0, 64.0, 0.5],
            [2.0, 65.62, 0.5],
            0.0,
        )
        .unwrap();
        assert_eq!(shielded, 1.0);
    }
}
