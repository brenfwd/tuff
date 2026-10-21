//! Background upkeep any process can enable through policy: eating when
//! hungry. Free of host calls so the decisions are plain functions.

use crate::hands::{Carried, HOTBAR};
use crate::world::name_of;

/// Ticks the held use lasts before the item is consumed, plus slack.
const EAT_TICKS: u32 = 35;
const FULL: i32 = 20;

/// Food points restored by the foods a bot will eat.
const FOODS: &[(&str, i32)] = &[
    ("cooked_beef", 8),
    ("cooked_porkchop", 8),
    ("pumpkin_pie", 8),
    ("cooked_salmon", 6),
    ("cooked_mutton", 6),
    ("cooked_chicken", 6),
    ("cooked_cod", 5),
    ("bread", 5),
    ("baked_potato", 5),
    ("beetroot_soup", 6),
    ("mushroom_stew", 6),
    ("rabbit_stew", 10),
    ("golden_carrot", 6),
    ("carrot", 3),
    ("apple", 4),
    ("melon_slice", 2),
    ("sweet_berries", 2),
    ("cookie", 2),
    ("dried_kelp", 1),
    ("beef", 3),
    ("porkchop", 3),
    ("mutton", 2),
];

pub fn food_value(item: &str) -> Option<i32> {
    let name = name_of(item);
    FOODS.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

/// The hotbar slot (0 to 8) of the best food that restores fully without
/// waste at this food level.
pub fn pick_food(carried: &[Carried], food: i32) -> Option<u8> {
    carried
        .iter()
        .filter(|(slot, _, count)| HOTBAR.contains(slot) && *count > 0)
        .filter_map(|(slot, item, _)| food_value(item).map(|v| (*slot, v)))
        .filter(|(_, v)| food + v <= FULL)
        .max_by_key(|(_, v)| *v)
        .map(|(slot, _)| slot - HOTBAR.start())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Eat {
    /// Select this hotbar slot and start using the item.
    Start(u8),
    /// Let go of the use key.
    Stop,
}

#[derive(Debug, Default)]
pub struct Eater {
    left: u32,
}

impl Eater {
    pub fn eating(&self) -> bool {
        self.left > 0
    }

    pub fn tick(&mut self, enabled: bool, food: i32, carried: &[Carried]) -> Option<Eat> {
        if self.left > 0 {
            self.left -= 1;
            return (self.left == 0).then_some(Eat::Stop);
        }
        if !enabled {
            return None;
        }
        let slot = pick_food(carried, food)?;
        self.left = EAT_TICKS;
        Some(Eat::Start(slot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hotbar(items: &[(u8, &str)]) -> Vec<Carried> {
        items
            .iter()
            .map(|(i, n)| (36 + i, format!("minecraft:{n}"), 3))
            .collect()
    }

    #[test]
    fn a_hungry_bot_picks_the_best_food_that_does_not_overshoot() {
        let kit = hotbar(&[(0, "stone"), (1, "bread"), (2, "cooked_beef")]);
        assert_eq!(pick_food(&kit, 10), Some(2));
        assert_eq!(pick_food(&kit, 14), Some(1));
        assert_eq!(pick_food(&kit, 18), None);
        assert_eq!(pick_food(&hotbar(&[(0, "stone")]), 2), None);
    }

    #[test]
    fn food_outside_the_hotbar_is_ignored() {
        let kit = vec![(10, "minecraft:bread".to_string(), 1)];
        assert_eq!(pick_food(&kit, 5), None);
    }

    #[test]
    fn eating_starts_holds_and_releases_once() {
        let kit = hotbar(&[(4, "bread")]);
        let mut eater = Eater::default();
        assert_eq!(eater.tick(false, 5, &kit), None);
        assert_eq!(eater.tick(true, 5, &kit), Some(Eat::Start(4)));
        for _ in 0..EAT_TICKS - 1 {
            assert_eq!(eater.tick(true, 5, &kit), None);
            assert!(eater.eating());
        }
        assert_eq!(eater.tick(true, 5, &kit), Some(Eat::Stop));
        assert!(!eater.eating());
    }
}
