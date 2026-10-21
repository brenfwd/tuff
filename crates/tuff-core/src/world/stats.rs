//! Player statistics: counters keyed by a statistic type and an entry of
//! the registry that type counts (blocks mined, items crafted, mobs
//! killed, and the custom counters such as play time and distances).
//! Saved per player as `{stats: {type: {entry: count}}, DataVersion}`
//! and sent with Award Statistics, only the changed ones each time.

use std::collections::{HashMap, HashSet};

use tuff_vanilla_data::item::StatRegistry;
use tuff_vanilla_data::registry::{CustomStat, EntityType, StatType};
use tuff_vanilla_data::{Block, Item};

use tuff_nbt::{Compound, Tag};

/// One statistic: its type and the registry id of the entry it counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Stat {
    pub kind: StatType,
    pub id: u32,
}

impl Stat {
    pub fn custom(stat: CustomStat) -> Self {
        Self {
            kind: StatType::Custom,
            id: stat.id(),
        }
    }

    pub fn block(kind: StatType, block: Block) -> Self {
        debug_assert_eq!(kind.registry(), StatRegistry::Block);
        Self {
            kind,
            id: block.id(),
        }
    }

    pub fn item(kind: StatType, item: Item) -> Self {
        debug_assert_eq!(kind.registry(), StatRegistry::Item);
        Self {
            kind,
            id: item.id(),
        }
    }

    pub fn entity(kind: StatType, entity: EntityType) -> Self {
        debug_assert_eq!(kind.registry(), StatRegistry::EntityType);
        Self {
            kind,
            id: entity.id(),
        }
    }

    /// The saved name of the entry it counts.
    fn entry_name(self) -> Option<&'static str> {
        Some(match self.kind.registry() {
            StatRegistry::Block => Block::from_id(self.id)?.name(),
            StatRegistry::Item => Item::from_id(self.id)?.name(),
            StatRegistry::EntityType => EntityType::from_id(self.id)?.name(),
            StatRegistry::CustomStat => CustomStat::from_id(self.id)?.name(),
        })
    }

    /// The scoreboard criterion that counts this statistic, e.g.
    /// `minecraft.mined:minecraft.stone`.
    pub fn criterion(self) -> Option<String> {
        let entry = self.entry_name()?;
        Some(format!(
            "{}:{}",
            self.kind.name().replace(':', "."),
            entry.replace(':', ".")
        ))
    }

    /// The statistic a scoreboard criterion names, in either the full or
    /// the namespace-less form.
    pub fn from_criterion(name: &str) -> Option<Self> {
        let (kind, entry) = name.split_once(':')?;
        let id = |part: &str| match part.split_once('.') {
            Some((namespace, path)) => format!("{namespace}:{path}"),
            None => format!("minecraft:{part}"),
        };
        Self::from_names(StatType::from_name(&id(kind))?, &id(entry))
    }

    /// The statistic of type `kind` counting `entry`, by name.
    pub fn from_names(kind: StatType, entry: &str) -> Option<Self> {
        let id = match kind.registry() {
            StatRegistry::Block => Block::from_name(entry)?.id(),
            StatRegistry::Item => Item::from_name(entry)?.id(),
            StatRegistry::EntityType => EntityType::from_name(entry)?.id(),
            StatRegistry::CustomStat => CustomStat::from_name(entry)?.id(),
        };
        Some(Self { kind, id })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    values: HashMap<Stat, i32>,
    dirty: HashSet<Stat>,
    /// Entries of the saved file this server does not know, kept as they
    /// were (by type name, then entry name).
    unknown: Compound,
    /// While the scoreboard counts statistics: each award (`Some`) or reset
    /// (`None`) since last taken.
    pub scored: Option<Vec<(Stat, Option<i32>)>>,
}

impl Stats {
    pub fn get(&self, stat: Stat) -> i32 {
        self.values.get(&stat).copied().unwrap_or(0)
    }

    /// Adds `amount`, saturating at the int range the game keeps.
    pub fn award(&mut self, stat: Stat, amount: i32) {
        if amount == 0 {
            return;
        }
        let value = self.get(stat).saturating_add(amount).max(0);
        self.set(stat, value);
        if let Some(scored) = &mut self.scored {
            scored.push((stat, Some(amount)));
        }
    }

    pub fn set(&mut self, stat: Stat, value: i32) {
        self.values.insert(stat, value);
        self.dirty.insert(stat);
    }

    pub fn award_custom(&mut self, stat: CustomStat, amount: i32) {
        self.award(Stat::custom(stat), amount);
    }

    /// Sets a custom counter back to zero (time since death, rest).
    pub fn reset_custom(&mut self, stat: CustomStat) {
        self.set(Stat::custom(stat), 0);
        if let Some(scored) = &mut self.scored {
            scored.push((Stat::custom(stat), None));
        }
    }

    /// Everything counts as changed, so the next request sends it all.
    pub fn mark_all_dirty(&mut self) {
        self.dirty.extend(self.values.keys().copied());
    }

    /// The statistics changed since last asked, with their values, in a
    /// stable order.
    pub fn take_dirty(&mut self) -> Vec<(Stat, i32)> {
        let mut changed: Vec<(Stat, i32)> = self
            .dirty
            .drain()
            .map(|stat| (stat, self.values.get(&stat).copied().unwrap_or(0)))
            .collect();
        changed.sort_unstable();
        changed
    }

    pub fn from_nbt(root: &Compound) -> Self {
        let mut stats = Self::default();
        let Some(groups) = root.get_compound("stats") else {
            return stats;
        };
        for (type_name, entries) in groups.iter() {
            let Some(entries) = entries.as_compound() else {
                continue;
            };
            let kind = StatType::from_name(type_name);
            for (entry, value) in entries.iter() {
                let count = match value {
                    Tag::Int(v) => Some(*v),
                    Tag::Long(v) => i32::try_from(*v).ok(),
                    Tag::Short(v) => Some(i32::from(*v)),
                    Tag::Byte(v) => Some(i32::from(*v)),
                    _ => None,
                };
                match (kind.and_then(|kind| Stat::from_names(kind, entry)), count) {
                    (Some(stat), Some(count)) => {
                        stats.values.insert(stat, count);
                    }
                    _ => {
                        let mut group = stats
                            .unknown
                            .get_compound(type_name)
                            .cloned()
                            .unwrap_or_default();
                        group.insert(entry, value.clone());
                        stats.unknown.insert(type_name, group);
                    }
                }
            }
        }
        stats
    }

    pub fn to_nbt(&self) -> Compound {
        let mut groups = self.unknown.clone();
        let mut sorted: Vec<(&Stat, &i32)> = self.values.iter().collect();
        sorted.sort_unstable();
        for (stat, value) in sorted {
            let Some(entry) = stat.entry_name() else {
                continue;
            };
            let type_name = stat.kind.name();
            let mut group = groups.get_compound(type_name).cloned().unwrap_or_default();
            group.insert(entry, *value);
            groups.insert(type_name, group);
        }
        Compound::new()
            .with("stats", groups)
            .with("DataVersion", tuff_vanilla_data::version::DATA_VERSION)
    }
}

/// What a player was doing while it moved, for the distance counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct Moving {
    pub swimming: bool,
    /// The eyes are under water.
    pub under_water: bool,
    pub in_water: bool,
    pub climbing: bool,
    pub on_ground: bool,
    pub sprinting: bool,
    pub crouching: bool,
    pub fall_flying: bool,
}

fn centimeters(distance: f64) -> i32 {
    ((distance as f32) * 100.0).round() as i32
}

/// The distance counter a move of `delta` adds to, and by how much, as the
/// game picks it: swimming, under water, in water, climbing, on the ground by gait, or
/// flying (moves of more than 25 cm only).
pub fn movement(state: Moving, delta: [f64; 3]) -> Option<(CustomStat, i32)> {
    let [dx, dy, dz] = delta;
    if dx == 0.0 && dy == 0.0 && dz == 0.0 {
        return None;
    }
    let full = centimeters((dx * dx + dy * dy + dz * dz).sqrt());
    let flat = centimeters((dx * dx + dz * dz).sqrt());
    let (stat, distance) = if state.swimming {
        (CustomStat::SwimOneCm, full)
    } else if state.under_water {
        (CustomStat::WalkUnderWaterOneCm, full)
    } else if state.in_water {
        (CustomStat::WalkOnWaterOneCm, flat)
    } else if state.climbing {
        if dy <= 0.0 {
            return None;
        }
        (CustomStat::ClimbOneCm, (dy * 100.0).round() as i32)
    } else if !state.on_ground {
        if state.fall_flying {
            (CustomStat::AviateOneCm, full)
        } else if flat > 25 {
            (CustomStat::FlyOneCm, flat)
        } else {
            return None;
        }
    } else if state.sprinting {
        (CustomStat::SprintOneCm, flat)
    } else if state.crouching {
        (CustomStat::CrouchOneCm, flat)
    } else {
        (CustomStat::WalkOneCm, flat)
    };
    (distance > 0).then_some((stat, distance))
}

/// The distance counter riding a `vehicle` adds to.
pub fn riding(vehicle: EntityType, delta: [f64; 3]) -> Option<(CustomStat, i32)> {
    let [dx, dy, dz] = delta;
    if dx == 0.0 && dy == 0.0 && dz == 0.0 {
        return None;
    }
    let distance = centimeters((dx * dx + dy * dy + dz * dz).sqrt());
    if tuff_vanilla_data::Item::of_boat(vehicle).is_some() {
        return Some((CustomStat::BoatOneCm, distance));
    }
    let stat = match vehicle {
        EntityType::Minecart
        | EntityType::ChestMinecart
        | EntityType::FurnaceMinecart
        | EntityType::HopperMinecart
        | EntityType::TntMinecart
        | EntityType::SpawnerMinecart
        | EntityType::CommandBlockMinecart => CustomStat::MinecartOneCm,
        EntityType::Horse
        | EntityType::Donkey
        | EntityType::Mule
        | EntityType::Llama
        | EntityType::TraderLlama
        | EntityType::SkeletonHorse
        | EntityType::ZombieHorse
        | EntityType::Camel
        | EntityType::CamelHusk => CustomStat::HorseOneCm,
        EntityType::Nautilus | EntityType::ZombieNautilus => CustomStat::NautilusOneCm,
        EntityType::Pig => CustomStat::PigOneCm,
        EntityType::Strider => CustomStat::StriderOneCm,
        EntityType::HappyGhast => CustomStat::HappyGhastOneCm,
        _ => return None,
    };
    Some((stat, distance))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn gliding_and_riding_use_their_own_counters() {
        let gliding = Moving {
            fall_flying: true,
            ..Moving::default()
        };
        assert_eq!(
            movement(gliding, [0.0, -0.3, 0.4]),
            Some((CustomStat::AviateOneCm, 50))
        );
        for (kind, stat) in [
            (EntityType::Minecart, CustomStat::MinecartOneCm),
            (EntityType::Donkey, CustomStat::HorseOneCm),
            (EntityType::Nautilus, CustomStat::NautilusOneCm),
        ] {
            assert_eq!(riding(kind, [0.3, 0.0, 0.4]), Some((stat, 50)));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn moves_count_towards_the_counter_of_what_the_player_was_doing() {
        let walking = Moving {
            on_ground: true,
            ..Moving::default()
        };
        assert_eq!(
            movement(walking, [0.3, 0.0, 0.4]),
            Some((CustomStat::WalkOneCm, 50))
        );
        let sprinting = Moving {
            sprinting: true,
            ..walking
        };
        assert_eq!(
            movement(sprinting, [0.3, 0.0, 0.4]),
            Some((CustomStat::SprintOneCm, 50))
        );
        let falling = Moving::default();
        assert_eq!(movement(falling, [0.1, -0.5, 0.1]), None, "short air moves");
        assert_eq!(
            movement(falling, [0.3, 0.0, 0.0]),
            Some((CustomStat::FlyOneCm, 30))
        );
        let climbing = Moving {
            climbing: true,
            ..Moving::default()
        };
        assert_eq!(
            movement(climbing, [0.0, 0.2, 0.0]),
            Some((CustomStat::ClimbOneCm, 20))
        );
        assert_eq!(movement(climbing, [0.0, -0.2, 0.0]), None);
        let wading = Moving {
            in_water: true,
            under_water: true,
            ..walking
        };
        assert_eq!(
            movement(wading, [0.3, 0.4, 0.0]),
            Some((CustomStat::WalkUnderWaterOneCm, 50))
        );
        assert_eq!(
            riding(EntityType::OakBoat, [3.0, 0.0, 4.0]),
            Some((CustomStat::BoatOneCm, 500))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn stats_count_saturate_and_round_trip_with_unknown_entries() {
        let mut stats = Stats::default();
        stats.award_custom(CustomStat::PlayTime, 20);
        stats.award_custom(CustomStat::PlayTime, 5);
        stats.award(Stat::block(StatType::Mined, Block::Stone), 3);
        stats.award(Stat::item(StatType::Crafted, Item::Stick), 4);
        stats.award(Stat::entity(StatType::Killed, EntityType::Zombie), 1);
        stats.award_custom(CustomStat::Jump, i32::MAX);
        stats.award_custom(CustomStat::Jump, 10);
        assert_eq!(stats.get(Stat::custom(CustomStat::Jump)), i32::MAX);
        assert_eq!(stats.get(Stat::custom(CustomStat::PlayTime)), 25);
        let dirty = stats.take_dirty();
        assert_eq!(dirty.len(), 5);
        assert!(stats.take_dirty().is_empty());

        let mut nbt = stats.to_nbt();
        let groups = nbt.get_compound("stats").unwrap();
        assert_eq!(
            groups
                .get_compound("minecraft:custom")
                .unwrap()
                .get_int("minecraft:play_time"),
            Some(25)
        );
        assert_eq!(
            groups
                .get_compound("minecraft:mined")
                .unwrap()
                .get_int("minecraft:stone"),
            Some(3)
        );
        let mut modded = groups.clone();
        modded.insert(
            "minecraft:custom",
            groups
                .get_compound("minecraft:custom")
                .unwrap()
                .clone()
                .with("modded:thing", 7),
        );
        nbt.insert("stats", modded);
        let loaded = Stats::from_nbt(&nbt);
        assert_eq!(loaded.get(Stat::custom(CustomStat::PlayTime)), 25);
        assert_eq!(
            loaded.get(Stat::entity(StatType::Killed, EntityType::Zombie)),
            1
        );
        let saved = loaded.to_nbt();
        assert_eq!(
            saved
                .get_compound("stats")
                .unwrap()
                .get_compound("minecraft:custom")
                .unwrap()
                .get_int("modded:thing"),
            Some(7),
            "unknown entries are kept"
        );
    }
}
