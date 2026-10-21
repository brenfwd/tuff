//! Farm animal variants and sound variants from the data pack, sorted by
//! name, which is the order of their network ids.

use crate::registry::SoundEvent;

/// A cow, pig or chicken variant and where it spawns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarmVariant {
    pub name: &'static str,
    pub spawn_conditions: &'static [SpawnPriority],
}

/// A spawn condition: the variant is picked where every constraint set
/// holds (always when none is), preferring higher priorities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpawnPriority {
    pub priority: i32,
    pub biomes: Option<&'static [&'static str]>,
    /// The structures one of which the spawn must be inside.
    pub structures: Option<&'static [&'static str]>,
    /// The least moon brightness (0 new moon, 1 full moon) required.
    pub min_moon_brightness: Option<f32>,
}

/// Where a variant is being picked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpawnSite<'a> {
    pub biome: &'a str,
    /// The structure the spawn is part of, if any.
    pub structure: Option<&'a str>,
    pub moon_brightness: f32,
}

impl SpawnPriority {
    fn holds(&self, site: &SpawnSite) -> bool {
        self.biomes
            .is_none_or(|biomes| biomes.contains(&site.biome))
            && self.structures.is_none_or(|structures| {
                site.structure
                    .is_some_and(|structure| structures.contains(&structure))
            })
            && self
                .min_moon_brightness
                .is_none_or(|min| site.moon_brightness >= min)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundSet {
    pub ambient: SoundEvent,
    pub hurt: SoundEvent,
    pub death: SoundEvent,
    pub step: Option<SoundEvent>,
    pub eat: Option<SoundEvent>,
    /// Wolf-only: growls when hostile, pants after sprinting, whines when
    /// hurt underwater.
    pub growl: Option<SoundEvent>,
    pub pant: Option<SoundEvent>,
    pub whine: Option<SoundEvent>,
    /// Cat-only.
    pub beg_for_food: Option<SoundEvent>,
    pub hiss: Option<SoundEvent>,
    pub purr: Option<SoundEvent>,
    pub purreow: Option<SoundEvent>,
    pub stray_ambient: Option<SoundEvent>,
}

/// The sounds a variant makes; babies have their own set for some types.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundVariant {
    pub name: &'static str,
    pub adult: SoundSet,
    pub baby: Option<SoundSet>,
}

impl SoundVariant {
    pub fn sounds(&self, baby: bool) -> &SoundSet {
        match (&self.baby, baby) {
            (Some(sounds), true) => sounds,
            _ => &self.adult,
        }
    }
}

/// The variants that may spawn in `biome`: those whose best matching
/// condition has the highest priority of all. The game picks one of them
/// at random.
pub fn candidates(variants: &'static [FarmVariant], biome: &str) -> Vec<usize> {
    candidates_at(
        variants,
        &SpawnSite {
            biome,
            structure: None,
            moon_brightness: 0.0,
        },
    )
}

/// Like `candidates`, for a full spawn site.
pub fn candidates_at(variants: &'static [FarmVariant], site: &SpawnSite) -> Vec<usize> {
    let mut best: Option<i32> = None;
    let mut found = Vec::new();
    for (index, variant) in variants.iter().enumerate() {
        let priority = variant
            .spawn_conditions
            .iter()
            .filter(|c| c.holds(site))
            .map(|c| c.priority)
            .max();
        let Some(priority) = priority else {
            continue;
        };
        if best.is_none_or(|b| priority > b) {
            best = Some(priority);
            found.clear();
        }
        if best == Some(priority) {
            found.push(index);
        }
    }
    found
}

/// A variant's index by name.
pub fn index(variants: &'static [FarmVariant], name: &str) -> Option<usize> {
    variants.iter().position(|v| v.name == name)
}

include!(concat!(env!("OUT_DIR"), "/variants.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cats_turn_all_black_in_swamp_huts_and_under_a_full_moon() {
        let all_black = index(CAT, "minecraft:all_black").unwrap();
        let site = |structure, moon_brightness| SpawnSite {
            biome: "minecraft:plains",
            structure,
            moon_brightness,
        };
        assert_eq!(
            candidates_at(CAT, &site(Some("minecraft:swamp_hut"), 0.0)),
            [all_black]
        );
        assert!(candidates_at(CAT, &site(None, 1.0)).contains(&all_black));
        assert!(!candidates_at(CAT, &site(None, 0.75)).contains(&all_black));
        assert!(
            !candidates_at(CAT, &site(Some("minecraft:village_plains"), 0.0)).contains(&all_black)
        );
    }

    #[test]
    fn farm_variants_follow_biomes() {
        let names: Vec<&str> = COW.iter().map(|v| v.name).collect();
        assert_eq!(
            names,
            ["minecraft:cold", "minecraft:temperate", "minecraft:warm"]
        );
        assert_eq!(candidates(COW, "minecraft:plains"), [1]);
        assert_eq!(candidates(PIG, "minecraft:snowy_plains"), [0]);
        assert_eq!(candidates(CHICKEN, "minecraft:desert"), [2]);
        assert_eq!(index(COW, "minecraft:temperate"), Some(1));
        assert_eq!(COW_SOUNDS[0].adult.ambient, SoundEvent::EntityCowAmbient);
        assert!(PIG_SOUNDS[0].baby.is_some() && PIG_SOUNDS[0].adult.eat.is_some());
        assert_eq!(CHICKEN_SOUNDS.len(), 2);
    }
}
