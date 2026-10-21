//! Powder snow freezing, shared by players and mobs: frozen ticks build up
//! one a tick inside powder snow and thaw two a tick anywhere else; once
//! fully frozen a thing takes freeze damage every 40 ticks.

use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::tag;

use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

pub const TICKS_TO_FREEZE: i32 = 140;
pub const HURT_INTERVAL: u64 = 40;
pub const FREEZE_DAMAGE: f32 = 1.0;
/// How much damage from freezing multiplies for types it hurts extra.
pub const EXTRA_HURT_MULTIPLIER: f32 = 5.0;
/// Movement speed lost per fully frozen fraction, standing on something.
pub const SLOWDOWN: f64 = -0.05;
pub const SLOWDOWN_MODIFIER: &str = "minecraft:powder_snow";
/// Past this fall distance powder snow catches a falling thing a little
/// below its top, walkable or not.
pub const FALLING_CATCH_DISTANCE: f64 = 2.5;
pub const FALLING_COLLISION: [f32; 6] = [0.0, 0.0, 0.0, 1.0, 0.9, 1.0];
const FULL_BLOCK: [f32; 6] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
const ABOVE_EPSILON: f64 = 1.0e-5;

/// Whether a thing of `kind` wearing `armor` can freeze at all.
pub fn can_freeze<'a>(
    kind: EntityType,
    spectator: bool,
    armor: impl IntoIterator<Item = Option<&'a Item>>,
) -> bool {
    !spectator
        && !tuff_datapack::tags::is(tag::entity_type::key::FREEZE_IMMUNE_ENTITY_TYPES, kind)
        && !armor
            .into_iter()
            .flatten()
            .any(|item| tuff_datapack::tags::is(tag::item::key::FREEZE_IMMUNE_WEARABLES, *item))
}

/// Frozen ticks after one tick, from `ticks`.
pub fn step(ticks: i32, in_powder_snow: bool, can_freeze: bool) -> i32 {
    if in_powder_snow && can_freeze {
        (ticks + 1).min(TICKS_TO_FREEZE)
    } else {
        (ticks - 2).max(0)
    }
}

pub fn fully_frozen(ticks: i32) -> bool {
    ticks >= TICKS_TO_FREEZE
}

/// How far frozen, 0 to 1.
pub fn percent(ticks: i32) -> f64 {
    ticks.clamp(0, TICKS_TO_FREEZE) as f64 / TICKS_TO_FREEZE as f64
}

/// Whether the tick numbered `tick` hurts a thing frozen `ticks` long.
pub fn hurts(tick: u64, ticks: i32, can_freeze: bool) -> bool {
    tick.is_multiple_of(HURT_INTERVAL) && fully_frozen(ticks) && can_freeze
}

/// Freezing damage of `amount` to a `kind`: five times as much for types
/// it hurts extra.
pub fn scaled(kind: EntityType, amount: f32) -> f32 {
    if tuff_datapack::tags::is(tag::entity_type::key::FREEZE_HURTS_EXTRA_TYPES, kind) {
        amount * EXTRA_HURT_MULTIPLIER
    } else {
        amount
    }
}

/// Whether a thing of `kind` wearing `feet` can stand on powder snow.
pub fn walks_on_powder_snow(kind: EntityType, feet: Option<Item>) -> bool {
    tuff_datapack::tags::is(tag::entity_type::key::POWDER_SNOW_WALKABLE_MOBS, kind)
        || feet == Some(Item::LeatherBoots)
}

/// The collision boxes powder snow at `pos` has for a thing whose box
/// bottom is at `bottom`, that has fallen `fall_distance` and that can
/// (`walkable`) or cannot walk on it: full once such a walker is above it,
/// a catching floor for a long fall, else nothing.
pub fn powder_snow_boxes(
    pos: BlockPos,
    bottom: f64,
    fall_distance: f64,
    walkable: bool,
) -> &'static [[f32; 6]] {
    if fall_distance > FALLING_CATCH_DISTANCE {
        std::slice::from_ref(&FALLING_COLLISION)
    } else if walkable && bottom > pos.y as f64 + 1.0 - ABOVE_EPSILON {
        std::slice::from_ref(&FULL_BLOCK)
    } else {
        &[]
    }
}

/// What the blocks a box overlaps do to its frozen ticks: powder snow
/// freezes, fire and lava cauldrons thaw at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inside {
    pub powder_snow: bool,
    pub thaws: bool,
    /// A powder snow block it is inside, which puts out fire and melts
    /// under something burning.
    pub powder_snow_at: Option<BlockPos>,
}

impl Inside {
    pub fn of(state: BlockState) -> Self {
        let block = state.block();
        Self {
            powder_snow: block == Block::PowderSnow,
            thaws: block == Block::LavaCauldron
                || tuff_datapack::tags::is(tag::block::key::FIRE, block),
            powder_snow_at: None,
        }
    }

    /// What `state` at `pos` does.
    pub fn at(state: BlockState, pos: BlockPos) -> Self {
        let mut inside = Self::of(state);
        if inside.powder_snow {
            inside.powder_snow_at = Some(pos);
        }
        inside
    }

    pub fn merge(self, other: Self) -> Self {
        Self {
            powder_snow: self.powder_snow || other.powder_snow,
            thaws: self.thaws || other.thaws,
            powder_snow_at: self.powder_snow_at.or(other.powder_snow_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn frozen_ticks_build_up_in_powder_snow_and_thaw_twice_as_fast() {
        let mut ticks = 0;
        for _ in 0..200 {
            ticks = step(ticks, true, true);
        }
        assert_eq!(ticks, TICKS_TO_FREEZE);
        assert_eq!(step(ticks, false, true), TICKS_TO_FREEZE - 2);
        assert_eq!(step(ticks, true, false), TICKS_TO_FREEZE - 2);
        assert_eq!(step(1, false, true), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn leather_armor_and_immune_types_do_not_freeze() {
        let boots = Item::LeatherBoots;
        let iron = Item::IronChestplate;
        assert!(can_freeze(EntityType::Player, false, [None, Some(&iron)]));
        assert!(!can_freeze(EntityType::Player, false, [Some(&boots)]));
        assert!(!can_freeze(EntityType::Player, true, []));
        assert!(!can_freeze(EntityType::Stray, false, []));
    }

    #[tuff_core::watchdog]
    #[test]
    fn freeze_damage_hurts_extra_types_five_times_and_waits_for_the_interval() {
        assert_eq!(scaled(EntityType::Zombie, 1.0), 1.0);
        assert_eq!(scaled(EntityType::Blaze, 1.0), 5.0);
        assert!(hurts(40, TICKS_TO_FREEZE, true));
        assert!(!hurts(41, TICKS_TO_FREEZE, true));
        assert!(!hurts(40, TICKS_TO_FREEZE - 1, true));
        assert!(!hurts(40, TICKS_TO_FREEZE, false));
    }

    #[tuff_core::watchdog]
    #[test]
    fn powder_snow_holds_walkers_above_it_and_catches_long_falls() {
        let pos = BlockPos::new(0, 64, 0);
        assert_eq!(powder_snow_boxes(pos, 65.0, 0.0, true).len(), 1);
        assert!(powder_snow_boxes(pos, 64.5, 0.0, true).is_empty());
        assert!(powder_snow_boxes(pos, 65.0, 0.0, false).is_empty());
        assert_eq!(
            powder_snow_boxes(pos, 70.0, 3.0, false),
            &[FALLING_COLLISION]
        );
        assert!(walks_on_powder_snow(EntityType::Rabbit, None));
        assert!(walks_on_powder_snow(
            EntityType::Zombie,
            Some(Item::LeatherBoots)
        ));
        assert!(!walks_on_powder_snow(EntityType::Zombie, None));
    }
}
