//! Per-entity-type data from game code: how far away each type is tracked,
//! how often the server resends its movement to the players tracking it,
//! its size, mob category and spawn placement, its synched data layout,
//! and the per-block data mobs move, path and spawn over.

use crate::registry::EntityType;
use crate::{Block, BlockState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tracking {
    /// How far away players see the entity, in chunks.
    pub range_chunks: u32,
    /// Movement is resent on ticks that are multiples of this since the
    /// entity started being tracked; `None` means only when forced.
    pub update_interval: Option<u32>,
    /// Whether velocity is sent along with position.
    pub track_deltas: bool,
}

impl Tracking {
    /// Whether movement is due on the `age`th tick of tracking.
    pub fn is_due(self, age: u64) -> bool {
        self.update_interval
            .is_some_and(|interval| age.is_multiple_of(u64::from(interval)))
    }
}

/// A type's default bounding box and whether fire hurts it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
    pub eye_height: f32,
    pub fire_immune: bool,
}

/// A mob category's natural spawning cap and despawn distances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CategoryInfo {
    /// Mobs per 17x17 chunks around players; negative for categories that
    /// never spawn naturally.
    pub max_per_chunk: i32,
    pub friendly: bool,
    pub persistent: bool,
    pub despawn_distance: u32,
    pub no_despawn_distance: u32,
}

impl MobCategory {
    pub fn info(self) -> CategoryInfo {
        CATEGORIES[self.id() as usize]
    }
}

/// What game code says about a type as a mob.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MobInfo {
    pub category: MobCategory,
    pub summonable: bool,
    pub spawns_far_from_players: bool,
    pub allowed_in_peaceful: bool,
    pub spawn_box_scale: f32,
    pub loot_table: Option<&'static str>,
    /// The baby's own size, where it is not half the adult's.
    pub baby: Option<Size>,
    /// Whether game code gives the type its own breathing: another air
    /// supply, losing air out of water, or never losing it.
    pub own_air_rules: bool,
    pub placement: SpawnPlacement,
    pub heightmap: Heightmap,
}

impl EntityType {
    pub fn tracking(self) -> Tracking {
        TRACKING[self.id() as usize]
    }

    pub fn size(self) -> Size {
        SIZES[self.id() as usize]
    }

    /// Where the type sits on a vehicle, from its feet.
    pub fn vehicle_attachment(self) -> [f64; 3] {
        VEHICLE_ATTACHMENTS[self.id() as usize]
    }

    /// Where its passengers sit, from its feet, unturned, by seat; never
    /// empty.
    pub fn passenger_attachments(self) -> &'static [[f64; 3]] {
        PASSENGER_ATTACHMENTS[self.id() as usize]
    }

    pub fn mob(self) -> &'static MobInfo {
        &MOBS[self.id() as usize]
    }

    pub fn category(self) -> MobCategory {
        self.mob().category
    }

    /// Whether its spawn only needs no other entity in the way: liquid in
    /// its box is fine (swimmers, and striders in lava).
    pub fn spawns_in_liquid(self) -> bool {
        matches!(
            self,
            Self::Cod
                | Self::Salmon
                | Self::Pufferfish
                | Self::TropicalFish
                | Self::Squid
                | Self::GlowSquid
                | Self::Dolphin
                | Self::Axolotl
                | Self::Guardian
                | Self::ElderGuardian
                | Self::Drowned
                | Self::Nautilus
                | Self::ZombieNautilus
                | Self::Strider
        )
    }

    /// The box it needs free to spawn with its feet at `position`.
    pub fn spawn_box(self, position: [f64; 3]) -> [f64; 6] {
        let size = self.size();
        let scale = self.mob().spawn_box_scale;
        let half = (scale * size.width) as f64 / 2.0;
        let height = (scale * size.height) as f64;
        let [x, y, z] = position;
        [x - half, y, z - half, x + half, y + height, z + half]
    }

    /// The size of a baby of this type.
    pub fn baby_size(self) -> Size {
        self.mob().baby.unwrap_or_else(|| {
            let size = self.size();
            Size {
                width: size.width * 0.5,
                height: size.height * 0.5,
                eye_height: size.eye_height * 0.5,
                fire_immune: size.fire_immune,
            }
        })
    }

    /// The synched data serializer id of every data index, in order.
    pub fn data_layout(self) -> &'static [u8] {
        DATA_LAYOUTS[self.id() as usize]
    }

    /// The serializer id of data index `index`.
    pub fn data_serializer(self, index: u8) -> Option<u8> {
        self.data_layout().get(usize::from(index)).copied()
    }
}

impl Pose {
    /// The player's box and eye height in this pose.
    pub fn player_size(self) -> Size {
        PLAYER_POSES[self.id() as usize]
    }
}

/// How blocks change the movement of what walks on or in them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockMovement {
    pub friction: f32,
    pub speed_factor: f32,
    pub jump_factor: f32,
    /// Fraction of the fall distance a landing on this block forgives.
    pub fall_distance_reduction: f32,
    /// How much of a landing's vertical speed a non-sneaking mob keeps,
    /// reversed (slime blocks, beds).
    pub bounce_restitution: f32,
}

impl Block {
    pub fn movement(self) -> BlockMovement {
        MOVEMENT[self.id() as usize]
    }
}

impl PathType {
    /// The cost mobs pay for a node of this type unless they override it;
    /// negative means impassable.
    pub fn malus(self) -> f32 {
        PATH_MALUS[self.id() as usize]
    }
}

const PATHFINDABLE: u8 = 32;
const WATER_PATHFINDABLE: u8 = 64;

/// The entity types allowed to spawn on top of a block state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spawnable {
    All,
    None,
    /// Only these types, by id.
    Only(&'static [u16]),
    /// Every type but these, by id.
    Except(&'static [u16]),
}

impl Spawnable {
    pub fn allows(self, kind: EntityType) -> bool {
        let id = kind.id() as u16;
        match self {
            Self::All => true,
            Self::None => false,
            Self::Only(ids) => ids.contains(&id),
            Self::Except(ids) => !ids.contains(&id),
        }
    }
}

impl BlockState {
    /// How pathfinding sees this state on its own, before neighbours are
    /// looked at.
    pub fn path_type(self) -> PathType {
        PathType::from_id(PATHS[self.id() as usize] & (PATHFINDABLE - 1))
            .expect("generated path types are valid")
    }

    /// Whether land mobs may path through this state.
    pub fn is_land_pathfindable(self) -> bool {
        PATHS[self.id() as usize] & PATHFINDABLE != 0
    }

    /// Whether water mobs may path through this state.
    pub fn is_water_pathfindable(self) -> bool {
        PATHS[self.id() as usize] & WATER_PATHFINDABLE != 0
    }

    /// Whether `kind` may spawn standing on top of this state.
    pub fn is_valid_spawn(self, kind: EntityType) -> bool {
        SPAWNABLE[self.id() as usize].allows(kind)
    }

    /// Whether standing in this state hurts `kind`, so it never spawns in
    /// it.
    pub fn is_dangerous_to(self, kind: EntityType) -> bool {
        DANGEROUS[self.id() as usize].allows(kind)
    }

    /// Whether this state conducts redstone, as full solid blocks do.
    pub fn is_redstone_conductor(self) -> bool {
        REDSTONE[self.id() as usize] & 1 != 0
    }

    /// Whether this state emits a redstone signal.
    pub fn is_signal_source(self) -> bool {
        REDSTONE[self.id() as usize] & 2 != 0
    }
}

/// A weighted choice of dye color, by color name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorTree {
    Color(&'static str),
    Weighted(&'static [(u32, ColorTree)]),
}

impl ColorTree {
    /// Picks a color, drawing each weighted choice with
    /// `random(total_weight)`.
    pub fn pick(&self, random: &mut impl FnMut(u32) -> u32) -> &'static str {
        match *self {
            Self::Color(name) => name,
            Self::Weighted(entries) => {
                let total: u32 = entries.iter().map(|(w, _)| w).sum();
                let mut roll = random(total);
                for (weight, tree) in entries {
                    if roll < *weight {
                        return tree.pick(random);
                    }
                    roll -= weight;
                }
                unreachable!("roll below the total weight")
            }
        }
    }
}

/// The climate a sheep spawns in, which decides its likely colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Climate {
    Temperate,
    Warm,
    Cold,
}

/// How naturally spawned sheep pick their color in a climate.
pub fn sheep_colors(climate: Climate) -> &'static ColorTree {
    &SHEEP_COLORS[climate as usize]
}

include!(concat!(env!("OUT_DIR"), "/entities.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falling_and_dropped_entities_are_paced() {
        assert_eq!(
            EntityType::Tnt.tracking(),
            Tracking {
                range_chunks: 10,
                update_interval: Some(10),
                track_deltas: true
            }
        );
        assert_eq!(EntityType::Item.tracking().update_interval, Some(20));
        assert_eq!(
            EntityType::ExperienceOrb.tracking().update_interval,
            Some(20)
        );
        assert!(EntityType::Tnt.tracking().is_due(0));
        assert!(!EntityType::Tnt.tracking().is_due(5));
        assert!(EntityType::Tnt.tracking().is_due(20));
        assert_eq!(FORCED_POSITION_PERIOD, 60);
    }

    #[test]
    fn mob_data_comes_from_the_types() {
        let cow = EntityType::Cow;
        assert_eq!(cow.category(), MobCategory::Creature);
        assert!(MobCategory::Creature.info().persistent);
        assert_eq!(MobCategory::Monster.info().max_per_chunk, 70);
        assert_eq!(cow.mob().loot_table, Some("minecraft:entities/cow"));
        assert_eq!(cow.mob().placement, SpawnPlacement::OnGround);
        assert_eq!(cow.mob().heightmap, Heightmap::MotionBlockingNoLeaves);
        assert_eq!(cow.baby_size().width, 0.45);
        assert_eq!(EntityType::Zombie.size().eye_height, 1.74);
        assert_eq!(cow.data_layout().len(), 20);
        assert_eq!(PathType::Water.malus(), 8.0);
        assert_eq!(PathType::Fence.malus(), -1.0);
        assert_eq!(Block::Ice.movement().friction, 0.98);
        assert_eq!(Block::SoulSand.movement().speed_factor, 0.4);
        assert_eq!(Block::OakFence.default_state().path_type(), PathType::Fence);
        assert!(Block::Air.default_state().is_land_pathfindable());
        assert!(!Block::Stone.default_state().is_land_pathfindable());
        assert!(Block::Water.default_state().is_water_pathfindable());
        assert!(!Block::Air.default_state().is_water_pathfindable());
        assert!(!Block::Stone.default_state().is_water_pathfindable());
        assert!(Block::GrassBlock.default_state().is_valid_spawn(cow));
        assert!(!Block::Glass.default_state().is_valid_spawn(cow));
        assert!(Block::Cactus.default_state().is_dangerous_to(cow));
        assert!(!Block::ShortGrass.default_state().is_dangerous_to(cow));
        assert!(Block::Stone.default_state().is_redstone_conductor());
        assert!(Block::Lever.default_state().is_signal_source());
        let mut rolls = [0u32, 400].into_iter();
        let color = sheep_colors(Climate::Temperate).pick(&mut |_| rolls.next().unwrap());
        assert_eq!(color, "black");
    }

    #[test]
    fn player_poses_and_breathing_come_from_game_code() {
        assert_eq!(MAX_AIR_SUPPLY, 300);
        assert_eq!(Pose::Swimming.id(), 3);
        assert_eq!(Pose::Crouching.id(), 5);
        let standing = Pose::Standing.player_size();
        assert_eq!((standing.height, standing.eye_height), (1.8, 1.62));
        assert_eq!(Pose::Crouching.player_size().eye_height, 1.27);
        assert_eq!(Pose::Swimming.player_size().eye_height, 0.4);
        assert_eq!(Pose::Sleeping.player_size().eye_height, 0.2);
        assert_eq!(Pose::Croaking.player_size(), standing);
        assert!(EntityType::Dolphin.mob().own_air_rules);
        assert!(EntityType::IronGolem.mob().own_air_rules);
        assert!(EntityType::Cod.mob().own_air_rules);
        assert!(!EntityType::Zombie.mob().own_air_rules);
        assert!(!EntityType::Cow.mob().own_air_rules);
    }

    #[test]
    fn sizes_and_fire_immunity_come_from_the_types() {
        let tnt = EntityType::Tnt.size();
        assert_eq!((tnt.width, tnt.height), (0.98, 0.98));
        let item = EntityType::Item.size();
        assert_eq!((item.width, item.height), (0.25, 0.25));
        assert!(!item.fire_immune);
        assert!(EntityType::Blaze.size().fire_immune);
    }
}
