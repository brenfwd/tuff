//! Chunk structure ids before 1.18.2: one id per structure kind, replaced by
//! configured structures chosen from the biomes of the chunk.

use std::collections::HashMap;

use tuff_nbt::{Compound, Tag};

type Conversion = (
    &'static str,
    &'static [(&'static [&'static str], &'static str)],
    &'static str,
);

const RUINED_PORTAL_MOUNTAIN: &[&str] = &[
    "minecraft:badlands",
    "minecraft:eroded_badlands",
    "minecraft:wooded_badlands",
    "minecraft:windswept_hills",
    "minecraft:windswept_forest",
    "minecraft:windswept_gravelly_hills",
    "minecraft:savanna_plateau",
    "minecraft:windswept_savanna",
    "minecraft:stony_shore",
    "minecraft:meadow",
    "minecraft:frozen_peaks",
    "minecraft:jagged_peaks",
    "minecraft:stony_peaks",
    "minecraft:snowy_slopes",
];
const RUINED_PORTAL_OCEAN: &[&str] = &[
    "minecraft:deep_frozen_ocean",
    "minecraft:deep_cold_ocean",
    "minecraft:deep_ocean",
    "minecraft:deep_lukewarm_ocean",
    "minecraft:frozen_ocean",
    "minecraft:ocean",
    "minecraft:cold_ocean",
    "minecraft:lukewarm_ocean",
    "minecraft:warm_ocean",
];

const CONVERSIONS: &[Conversion] = &[
    (
        "mineshaft",
        &[(
            &[
                "minecraft:badlands",
                "minecraft:eroded_badlands",
                "minecraft:wooded_badlands",
            ],
            "minecraft:mineshaft_mesa",
        )],
        "minecraft:mineshaft",
    ),
    (
        "shipwreck",
        &[(
            &["minecraft:beach", "minecraft:snowy_beach"],
            "minecraft:shipwreck_beached",
        )],
        "minecraft:shipwreck",
    ),
    (
        "ocean_ruin",
        &[(
            &[
                "minecraft:warm_ocean",
                "minecraft:lukewarm_ocean",
                "minecraft:deep_lukewarm_ocean",
            ],
            "minecraft:ocean_ruin_warm",
        )],
        "minecraft:ocean_ruin_cold",
    ),
    (
        "village",
        &[
            (&["minecraft:desert"], "minecraft:village_desert"),
            (&["minecraft:savanna"], "minecraft:village_savanna"),
            (&["minecraft:snowy_plains"], "minecraft:village_snowy"),
            (&["minecraft:taiga"], "minecraft:village_taiga"),
        ],
        "minecraft:village_plains",
    ),
    (
        "ruined_portal",
        &[
            (&["minecraft:desert"], "minecraft:ruined_portal_desert"),
            (RUINED_PORTAL_MOUNTAIN, "minecraft:ruined_portal_mountain"),
            (
                &[
                    "minecraft:bamboo_jungle",
                    "minecraft:jungle",
                    "minecraft:sparse_jungle",
                ],
                "minecraft:ruined_portal_jungle",
            ),
            (RUINED_PORTAL_OCEAN, "minecraft:ruined_portal_ocean"),
        ],
        "minecraft:ruined_portal",
    ),
    ("pillager_outpost", &[], "minecraft:pillager_outpost"),
    ("mansion", &[], "minecraft:mansion"),
    ("jungle_pyramid", &[], "minecraft:jungle_pyramid"),
    ("desert_pyramid", &[], "minecraft:desert_pyramid"),
    ("igloo", &[], "minecraft:igloo"),
    ("swamp_hut", &[], "minecraft:swamp_hut"),
    ("stronghold", &[], "minecraft:stronghold"),
    ("monument", &[], "minecraft:monument"),
    ("fortress", &[], "minecraft:fortress"),
    ("endcity", &[], "minecraft:end_city"),
    ("buried_treasure", &[], "minecraft:buried_treasure"),
    ("nether_fossil", &[], "minecraft:nether_fossil"),
    ("bastion_remnant", &[], "minecraft:bastion_remnant"),
];

pub fn upgrade(root: &mut Compound) {
    let biomes = biome_counts(root);
    let Some(structures) = root.get_compound_mut("structures") else {
        return;
    };
    if let Some(Tag::Compound(starts)) = structures.get_mut("starts") {
        let mut out = Compound::new();
        for (key, start) in std::mem::take(starts) {
            let valid = start
                .as_compound()
                .and_then(|s| s.get_str("id"))
                .is_none_or(|id| id != "INVALID");
            let Some(id) = valid.then(|| converted(&key, &biomes)).flatten() else {
                continue;
            };
            if out.contains_key(&id) {
                continue;
            }
            let mut start = start;
            if let Tag::Compound(compound) = &mut start {
                compound.insert("id", id.clone());
            }
            out.insert(id, start);
        }
        *starts = out;
    }
    if let Some(Tag::Compound(references)) = structures.get_mut("References") {
        let mut merged: Vec<(String, Vec<i64>)> = Vec::new();
        for (key, refs) in std::mem::take(references) {
            let Tag::LongArray(refs) = refs else {
                continue;
            };
            let Some(id) = (!refs.is_empty())
                .then(|| converted(&key, &biomes))
                .flatten()
            else {
                continue;
            };
            match merged.iter_mut().find(|(known, _)| *known == id) {
                Some((_, all)) => all.extend(refs),
                None => merged.push((id, refs)),
            }
        }
        *references = merged.into_iter().collect();
    }
}

fn biome_counts(root: &Compound) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for section in root.get_list("sections").unwrap_or_default() {
        let palette = section
            .as_compound()
            .and_then(|s| s.get_compound("biomes"))
            .and_then(|b| b.get_list("palette"));
        for biome in palette.unwrap_or_default() {
            if let Some(name) = biome.as_str() {
                *counts.entry(name.to_owned()).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn converted(key: &str, biomes: &HashMap<String, usize>) -> Option<String> {
    let key = key.to_lowercase();
    let (_, mapping, fallback) = CONVERSIONS.iter().find(|(name, ..)| *name == key)?;
    let mut votes: Vec<(&str, usize)> = Vec::new();
    for (names, target) in *mapping {
        for name in *names {
            if let Some(count) = biomes.get(*name) {
                match votes.iter_mut().find(|(known, _)| known == target) {
                    Some((_, total)) => *total += count,
                    None => votes.push((target, *count)),
                }
            }
        }
    }
    let best = votes
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(t, _)| *t);
    Some(best.unwrap_or(fallback).to_owned())
}
