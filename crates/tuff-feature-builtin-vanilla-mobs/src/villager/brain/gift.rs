//! Thanks for a won raid: a villager that sees a Hero of the Village walks
//! up and tosses them something from its profession's gift table, then
//! waits half a minute to five and a half before the next.

use rand::Rng;
use tuff_vanilla_data::registry::{MobEffect, VillagerProfession};

use super::{Villager, distance_sq};
use crate::MobCore;
use crate::physics::block_pos;
use tuff_core::world::entity::{Effect, NearbyPlayer, TickContext};

const FIRST_GIFT_DELAY: i32 = 600;
const MIN_GIFT_DELAY: i32 = 600;
const GIFT_DELAY_SPREAD: i32 = 6000;
const TIMEOUT: u64 = 100;
const HEAD_TURN_TICKS: u64 = 20;
const THROW_DISTANCE: f64 = 5.0;
const WALK_SPEED: f64 = 0.5;
const VISIBLE_RANGE: f64 = 16.0;
const THROW_SPEED: f64 = 0.3;
const THROW_HAND_BELOW_EYE: f64 = 0.3;
const THROW_PICKUP_DELAY: u32 = 40;

const PROFESSIONS_WITH_GIFTS: &[VillagerProfession] = &[
    VillagerProfession::Armorer,
    VillagerProfession::Butcher,
    VillagerProfession::Cartographer,
    VillagerProfession::Cleric,
    VillagerProfession::Farmer,
    VillagerProfession::Fisherman,
    VillagerProfession::Fletcher,
    VillagerProfession::Leatherworker,
    VillagerProfession::Librarian,
    VillagerProfession::Mason,
    VillagerProfession::Shepherd,
    VillagerProfession::Toolsmith,
    VillagerProfession::Weaponsmith,
];

#[derive(Debug)]
pub struct State {
    wait: i32,
    started: Option<u64>,
    given: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            wait: FIRST_GIFT_DELAY,
            started: None,
            given: false,
        }
    }
}

fn is_hero(player: &NearbyPlayer) -> bool {
    player.effects & 1u64 << MobEffect::HeroOfTheVillage.id() != 0
}

/// The gift table a villager like this one throws from.
pub fn gift_table(profession: VillagerProfession, baby: bool) -> String {
    let name = if baby {
        "baby"
    } else if PROFESSIONS_WITH_GIFTS.contains(&profession) {
        profession
            .name()
            .strip_prefix("minecraft:")
            .unwrap_or(profession.name())
    } else {
        "unemployed"
    };
    format!("minecraft:gameplay/hero_of_the_village/{name}_gift")
}

impl Villager {
    fn nearest_hero<'a>(
        &self,
        core: &MobCore,
        cx: &'a TickContext,
    ) -> Option<&'a NearbyPlayer<'a>> {
        cx.players
            .iter()
            .filter(|p| distance_sq(p.position, core.base.position) <= VISIBLE_RANGE.powi(2))
            .min_by(|a, b| {
                distance_sq(a.position, core.base.position)
                    .total_cmp(&distance_sq(b.position, core.base.position))
            })
            .filter(|p| is_hero(p))
            .filter(|p| {
                core.can_see_point(
                    [p.position[0], p.position[1] + p.eye_height, p.position[2]],
                    cx,
                )
            })
    }

    /// One step of gift giving; true while it is busy with a gift.
    pub(super) fn gift_step(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        let hero = self
            .nearest_hero(core, cx)
            .map(|p| (p.position, p.eye_height));
        let state = &mut self.brain.gift;
        let Some(started) = state.started else {
            if hero.is_none() {
                return false;
            }
            if state.wait > 0 {
                state.wait -= 1;
                return false;
            }
            state.started = Some(cx.tick);
            state.given = false;
            return true;
        };
        let Some((position, eye_height)) =
            hero.filter(|_| !state.given && cx.tick.saturating_sub(started) <= TIMEOUT)
        else {
            state.started = None;
            state.wait = MIN_GIFT_DELAY + cx.rng.gen_range(0..=GIFT_DELAY_SPREAD);
            core.navigation.stop();
            return false;
        };
        let eye = [position[0], position[1] + eye_height, position[2]];
        core.look.set_look_at(eye, 30.0, 30.0);
        let (from, to) = (block_pos(core.base.position), block_pos(position));
        let blocks =
            f64::from((from.x - to.x).pow(2) + (from.y - to.y).pow(2) + (from.z - to.z).pow(2));
        if blocks >= THROW_DISTANCE * THROW_DISTANCE {
            core.move_to_entity(position, WALK_SPEED, cx);
            return true;
        }
        if cx.tick.saturating_sub(started) > HEAD_TURN_TICKS {
            let table = gift_table(self.data.profession, core.is_baby());
            for stack in cx.shared.block_loot().roll(&table, cx.rng) {
                throw_toward(core, stack, position, cx);
            }
            self.brain.gift.given = true;
        }
        true
    }
}

fn throw_toward(
    core: &MobCore,
    stack: tuff_item::item::ItemStack,
    target: [f64; 3],
    cx: &mut TickContext,
) {
    let from = [
        core.base.position[0],
        core.base.position[1] + core.size.eye_height as f64 - THROW_HAND_BELOW_EYE,
        core.base.position[2],
    ];
    let toward = [
        target[0] - core.base.position[0],
        target[1] - core.base.position[1],
        target[2] - core.base.position[2],
    ];
    let length = toward.iter().map(|c| c * c).sum::<f64>().sqrt().max(1e-6);
    cx.effects.push(Effect::DropItem {
        stack,
        position: from,
        velocity: toward.map(|c| c / length * THROW_SPEED),
        pickup_delay: THROW_PICKUP_DELAY,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn every_gift_table_exists() {
        for profession in PROFESSIONS_WITH_GIFTS {
            let table = gift_table(*profession, false);
            assert!(
                tuff_vanilla_data::loot_table::LootTable::get(&table).is_some(),
                "{table}"
            );
        }
        assert!(
            tuff_vanilla_data::loot_table::LootTable::get(&gift_table(
                VillagerProfession::Nitwit,
                false
            ))
            .is_some()
        );
        assert!(
            tuff_vanilla_data::loot_table::LootTable::get(&gift_table(
                VillagerProfession::Farmer,
                true
            ))
            .is_some()
        );
    }
}
