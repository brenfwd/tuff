use std::sync::LazyLock;

use super::get_number;
use tuff_nbt::{Compound, Tag};

const LOOT: &str = "loot_tables_to_eject: [{data: \"minecraft:spawners/ominous/trial_chamber/key\", weight: 3}, {data: \"minecraft:spawners/ominous/trial_chamber/consumables\", weight: 7}]";
const MELEE: &str =
    "equipment: {loot_table: \"minecraft:equipment/trial_chamber_melee\", slot_drop_chances: 0.0f}";
const RANGED: &str = "equipment: {loot_table: \"minecraft:equipment/trial_chamber_ranged\", slot_drop_chances: 0.0f}";
const BUMP: &str = "simultaneous_mobs: 4.0f, total_mobs: 12.0f";
const BABY: &str = "{IsBaby: 1b, id: \"minecraft:zombie\"}";

struct Entry {
    name: &'static str,
    normal: Compound,
    ominous: [Compound; 3],
}

fn parse(text: &str) -> Compound {
    match Tag::from_snbt(text) {
        Ok(Tag::Compound(compound)) => compound,
        _ => unreachable!("invalid built-in trial spawner config"),
    }
}

fn normal(mobs: f32, per_player: f32, entity: &str, ticks: i32, total: bool) -> String {
    let total = if total {
        ", total_mobs: 2.0f, total_mobs_added_per_player: 1.0f"
    } else {
        ""
    };
    format!(
        "{{simultaneous_mobs: {mobs:?}f, simultaneous_mobs_added_per_player: {per_player:?}f, spawn_potentials: [{{data: {{entity: {entity}}}, weight: 1}}], ticks_between_spawn: {ticks}{total}}}"
    )
}

fn with_equipment(entity: &str, equipment: &str) -> String {
    format!(
        "{{{LOOT}, spawn_potentials: [{{data: {{entity: {entity}, {equipment}}}, weight: 1}}]}}"
    )
}

fn mob(id: &str) -> String {
    format!("{{id: \"minecraft:{id}\"}}")
}

fn trim(mut tag: Compound) -> Compound {
    let defaults: [(&str, f64); 6] = [
        ("spawn_range", 4.0),
        ("total_mobs", 6.0),
        ("simultaneous_mobs", 2.0),
        ("total_mobs_added_per_player", 2.0),
        ("simultaneous_mobs_added_per_player", 1.0),
        ("ticks_between_spawn", 40.0),
    ];
    for (key, default) in defaults {
        if get_number(&tag, key) == Some(default) {
            tag.remove(key);
        }
    }
    tag
}

fn entry(name: &'static str, normal: &str, ominous: &str) -> Entry {
    let normal = parse(normal);
    let ominous = parse(ominous);
    let mut merged = normal.clone();
    for (key, value) in ominous.iter() {
        merged.insert(key, value.clone());
    }
    let trimmed = trim(merged.clone());
    Entry {
        name,
        normal,
        ominous: [ominous, merged, trimmed],
    }
}

static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(|| {
    let plain = format!("{{{LOOT}, {BUMP}}}");
    let slime = |size: i32| format!("{{Size: {size}, id: \"minecraft:slime\"}}");
    let slime_normal = format!(
        "{{simultaneous_mobs: 3.0f, simultaneous_mobs_added_per_player: 0.5f, spawn_potentials: [{{data: {{entity: {}}}, weight: 3}}, {{data: {{entity: {}}}, weight: 1}}], ticks_between_spawn: 20}}",
        slime(1),
        slime(2)
    );
    let breeze_ominous = format!("{{{LOOT}, simultaneous_mobs: 2.0f, total_mobs: 4.0f}}");
    let melee = |id: &str| with_equipment(&mob(id), MELEE);
    let ranged = |id: &str| with_equipment(&mob(id), RANGED);
    vec![
        entry(
            "trial_chamber/breeze",
            &normal(1.0, 0.5, &mob("breeze"), 20, true),
            &breeze_ominous,
        ),
        entry(
            "trial_chamber/melee/husk",
            &normal(3.0, 0.5, &mob("husk"), 20, false),
            &melee("husk"),
        ),
        entry(
            "trial_chamber/melee/spider",
            &normal(3.0, 0.5, &mob("spider"), 20, false),
            &plain,
        ),
        entry(
            "trial_chamber/melee/zombie",
            &normal(3.0, 0.5, &mob("zombie"), 20, false),
            &melee("zombie"),
        ),
        entry(
            "trial_chamber/ranged/poison_skeleton",
            &normal(3.0, 0.5, &mob("bogged"), 20, false),
            &ranged("bogged"),
        ),
        entry(
            "trial_chamber/ranged/skeleton",
            &normal(3.0, 0.5, &mob("skeleton"), 20, false),
            &ranged("skeleton"),
        ),
        entry(
            "trial_chamber/ranged/stray",
            &normal(3.0, 0.5, &mob("stray"), 20, false),
            &ranged("stray"),
        ),
        entry(
            "trial_chamber/slow_ranged/poison_skeleton",
            &normal(4.0, 2.0, &mob("bogged"), 160, false),
            &ranged("bogged"),
        ),
        entry(
            "trial_chamber/slow_ranged/skeleton",
            &normal(4.0, 2.0, &mob("skeleton"), 160, false),
            &ranged("skeleton"),
        ),
        entry(
            "trial_chamber/slow_ranged/stray",
            &normal(4.0, 2.0, &mob("stray"), 160, false),
            &ranged("stray"),
        ),
        entry(
            "trial_chamber/small_melee/baby_zombie",
            &normal(2.0, 0.5, BABY, 20, false),
            &with_equipment(BABY, MELEE),
        ),
        entry(
            "trial_chamber/small_melee/cave_spider",
            &normal(3.0, 0.5, &mob("cave_spider"), 20, false),
            &plain,
        ),
        entry(
            "trial_chamber/small_melee/silverfish",
            &normal(3.0, 0.5, &mob("silverfish"), 20, false),
            &plain,
        ),
        entry("trial_chamber/small_melee/slime", &slime_normal, &plain),
    ]
});

pub fn upgrade(be: &mut Compound) {
    let (Some(Tag::Compound(normal)), Some(Tag::Compound(ominous))) =
        (be.get("normal_config"), be.get("ominous_config"))
    else {
        return;
    };
    let Some(entry) = ENTRIES
        .iter()
        .find(|entry| entry.normal == *normal && entry.ominous.iter().any(|o| o == ominous))
    else {
        return;
    };
    let name = entry.name;
    be.insert("normal_config", format!("minecraft:{name}/normal"));
    be.insert("ominous_config", format!("minecraft:{name}/ominous"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn registry_configs_become_references() {
        let mut be = Compound::new();
        be.insert("id", "minecraft:trial_spawner");
        be.insert("normal_config", ENTRIES[3].normal.clone());
        be.insert("ominous_config", ENTRIES[3].ominous[2].clone());
        upgrade(&mut be);
        assert_eq!(
            be.get("normal_config"),
            Some(&Tag::String(
                "minecraft:trial_chamber/melee/zombie/normal".into()
            ))
        );
        assert_eq!(
            be.get("ominous_config"),
            Some(&Tag::String(
                "minecraft:trial_chamber/melee/zombie/ominous".into()
            ))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn custom_configs_are_kept() {
        let mut be = Compound::new();
        be.insert("normal_config", ENTRIES[0].normal.clone());
        be.insert("ominous_config", parse("{simultaneous_mobs: 9.0f}"));
        let before = be.clone();
        upgrade(&mut be);
        assert_eq!(be, before);
    }
}
