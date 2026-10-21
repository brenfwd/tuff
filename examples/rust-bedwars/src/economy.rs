// SPDX-License-Identifier: MIT
//! Resources, generator rates and the game's timeline, in server ticks.

use tuff_plugin_sdk::text::Color;

pub const SECOND: u64 = 20;
pub const MINUTE: u64 = 60 * SECOND;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Resource {
    Iron,
    Gold,
    Diamond,
    Emerald,
}

impl Resource {
    pub const ALL: [Resource; 4] = [
        Resource::Iron,
        Resource::Gold,
        Resource::Diamond,
        Resource::Emerald,
    ];

    pub fn item(self) -> &'static str {
        match self {
            Resource::Iron => "minecraft:iron_ingot",
            Resource::Gold => "minecraft:gold_ingot",
            Resource::Diamond => "minecraft:diamond",
            Resource::Emerald => "minecraft:emerald",
        }
    }

    pub fn of_item(item: &str) -> Option<Resource> {
        Resource::ALL.into_iter().find(|r| r.item() == item)
    }

    pub fn name(self) -> &'static str {
        match self {
            Resource::Iron => "Iron",
            Resource::Gold => "Gold",
            Resource::Diamond => "Diamond",
            Resource::Emerald => "Emerald",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Resource::Iron => Color::White,
            Resource::Gold => Color::Gold,
            Resource::Diamond => Color::Aqua,
            Resource::Emerald => Color::DarkGreen,
        }
    }

    /// How many may lie at one generator before it pauses.
    pub fn cap(self) -> u32 {
        match self {
            Resource::Iron => 48,
            Resource::Gold => 12,
            Resource::Diamond => 4,
            Resource::Emerald => 2,
        }
    }
}

const IRON: u64 = 25;
const GOLD: u64 = 150;
const FORGE_EMERALD: u64 = 75 * SECOND;
const DIAMOND: [u64; 3] = [30 * SECOND, 23 * SECOND, 12 * SECOND];
const EMERALD: [u64; 3] = [65 * SECOND, 50 * SECOND, 35 * SECOND];

/// Forge upgrade tiers: none, Iron Forge (+50%), Golden Forge (+100%),
/// Emerald Forge (+100% and emeralds), Molten Forge (+200%).
pub const FORGE_TIERS: [&str; 5] = [
    "Forge",
    "Iron Forge",
    "Golden Forge",
    "Emerald Forge",
    "Molten Forge",
];
const FORGE_BONUS: [u64; 5] = [0, 50, 100, 100, 200];

/// Ticks between drops of a team forge at upgrade `tier`; none when that
/// tier makes none of `resource`.
pub fn forge_interval(resource: Resource, tier: usize) -> Option<u64> {
    let tier = tier.min(FORGE_TIERS.len() - 1);
    let base = match resource {
        Resource::Iron => IRON,
        Resource::Gold => GOLD,
        Resource::Emerald if tier >= 3 => return Some(FORGE_EMERALD),
        _ => return None,
    };
    Some(base * 100 / (100 + FORGE_BONUS[tier]))
}

/// Ticks between drops of a diamond or emerald generator at `tier` (0 to
/// 2).
pub fn generator_interval(resource: Resource, tier: usize) -> u64 {
    let tier = tier.min(2);
    match resource {
        Resource::Emerald => EMERALD[tier],
        _ => DIAMOND[tier],
    }
}

/// What happens as a game goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    DiamondII,
    EmeraldII,
    DiamondIII,
    EmeraldIII,
    BedGone,
    SuddenDeath,
    GameEnd,
}

pub const TIMELINE: [(u64, Stage); 7] = [
    (6 * MINUTE, Stage::DiamondII),
    (12 * MINUTE, Stage::EmeraldII),
    (18 * MINUTE, Stage::DiamondIII),
    (24 * MINUTE, Stage::EmeraldIII),
    (30 * MINUTE, Stage::BedGone),
    (40 * MINUTE, Stage::SuddenDeath),
    (50 * MINUTE, Stage::GameEnd),
];

impl Stage {
    pub fn title(self) -> &'static str {
        match self {
            Stage::DiamondII => "Diamond II",
            Stage::EmeraldII => "Emerald II",
            Stage::DiamondIII => "Diamond III",
            Stage::EmeraldIII => "Emerald III",
            Stage::BedGone => "Bed Gone",
            Stage::SuddenDeath => "Sudden Death",
            Stage::GameEnd => "Game End",
        }
    }
}

/// The diamond or emerald generator tier once the first `passed` stages
/// of the timeline happened.
pub fn tier(resource: Resource, passed: usize) -> usize {
    let (two, three) = match resource {
        Resource::Emerald => (Stage::EmeraldII, Stage::EmeraldIII),
        _ => (Stage::DiamondII, Stage::DiamondIII),
    };
    TIMELINE[..passed.min(TIMELINE.len())]
        .iter()
        .filter(|(_, s)| *s == two || *s == three)
        .count()
}

/// `m:ss` of a tick count.
pub fn clock(ticks: u64) -> String {
    let seconds = ticks / SECOND;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_upgrades_speed_up_iron_and_gold_and_add_emeralds() {
        assert_eq!(forge_interval(Resource::Iron, 0), Some(25));
        assert_eq!(forge_interval(Resource::Iron, 1), Some(16));
        assert_eq!(forge_interval(Resource::Gold, 2), Some(75));
        assert_eq!(forge_interval(Resource::Gold, 4), Some(50));
        assert_eq!(forge_interval(Resource::Emerald, 2), None);
        assert_eq!(forge_interval(Resource::Emerald, 3), Some(1500));
        assert_eq!(forge_interval(Resource::Diamond, 4), None);
        assert_eq!(forge_interval(Resource::Iron, 99), Some(8));
    }

    #[test]
    fn generator_tiers_follow_the_timeline() {
        assert_eq!(tier(Resource::Diamond, 0), 0);
        assert_eq!(tier(Resource::Diamond, 1), 1);
        assert_eq!(tier(Resource::Emerald, 1), 0);
        assert_eq!(tier(Resource::Emerald, 2), 1);
        assert_eq!(tier(Resource::Diamond, 3), 2);
        assert_eq!(tier(Resource::Emerald, 7), 2);
        assert_eq!(generator_interval(Resource::Diamond, 2), 240);
        assert_eq!(generator_interval(Resource::Emerald, 0), 1300);
        assert!(TIMELINE.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn clocks_and_items() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(6 * MINUTE + 5 * SECOND + 19), "6:05");
        assert_eq!(
            Resource::of_item("minecraft:gold_ingot"),
            Some(Resource::Gold)
        );
        assert_eq!(Resource::of_item("minecraft:stone"), None);
    }
}
