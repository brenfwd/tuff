use super::fixtures::*;
use std::sync::Arc;

use crate::proto::ProtoChunk;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::pos::ChunkPos;

use super::decoration::decorated_exact;

#[tuff_core::watchdog]
#[test]
fn a_dimension_origin_placement_starts_only_in_chunk_zero() {
    use crate::structure::placement::Set;
    use tuff_vanilla_data::structure::{
        FrequencyReduction, Spread, StructurePlacement, StructureSet,
    };

    let generator = generator("overworld");
    let structures = generator.structures();
    let data: &'static StructureSet = Box::leak(Box::new(StructureSet {
        name: "test:origin",
        structures: &[("minecraft:pillager_outpost", 1)],
        placement: StructurePlacement::DimensionOrigin {
            spread: Spread {
                locate_offset: [0, 0, 0],
                frequency_reduction: FrequencyReduction::Default,
                frequency: 1.0,
                salt: 0,
                exclusion_zone: None,
            },
        },
    }));
    let set = Set::new(data, &structures.entries);
    assert!(set.is_placement(structures, generator, ChunkPos::new(0, 0)));
    assert!(!set.is_placement(structures, generator, ChunkPos::new(1, 0)));
    assert!(!set.is_placement(structures, generator, ChunkPos::new(0, -1)));
}

/// Lists the structure starts of a dimension in a square of chunks
/// (`MC_WORLDGEN_SCAN=<dim>,<min x>,<min z>,<max x>,<max z>[,<name filter>]`),
/// to pick fixture centers; `MC_WORLDGEN_SCAN_ALL` prints every piece.
#[tuff_core::watchdog]
#[test]
#[ignore = "diagnostic"]
fn structure_start_scan() {
    let spec = std::env::var("MC_WORLDGEN_SCAN").expect("MC_WORLDGEN_SCAN");
    let parts: Vec<&str> = spec.split(',').collect();
    let dimension = parts[0];
    let [x0, z0, x1, z1]: [i32; 4] = std::array::from_fn(|i| parts[i + 1].parse().unwrap());
    let filter = parts.get(5).copied().unwrap_or("");
    let generator = generator(dimension);
    for z in z0..=z1 {
        for x in x0..=x1 {
            for start in generator.structure_starts(ChunkPos::new(x, z), shape(dimension)) {
                if !start.structure.name.contains(filter) {
                    continue;
                }
                let shown = if std::env::var_os("MC_WORLDGEN_SCAN_ALL").is_some() {
                    usize::MAX
                } else {
                    3
                };
                let pieces: Vec<String> = start
                    .pieces
                    .iter()
                    .take(shown)
                    .map(|p| format!("{}", p.to_nbt()))
                    .collect();
                eprintln!(
                    "{} chunk {} {} box {:?} {}",
                    start.structure.name,
                    x,
                    z,
                    start.bounding_box(),
                    pieces.join(" | ")
                );
            }
        }
    }
}

/// The structure references (and, for kept start chunks, the starts) of
/// every full fixture chunk match vanilla's, for every structure type this
/// server implements.
#[tuff_core::watchdog]
#[test]
fn structure_references_match_vanilla() {
    use crate::structure::chunk_key;

    let chunks = variant_chunks("full");
    let mut failures = Vec::new();
    for (dimension, pos) in kept("full") {
        let root = chunks.get_compound(&key(dimension, pos)).unwrap();
        let structures = root.get_compound("structures").cloned().unwrap_or_default();
        let generator = generator(dimension);
        let implemented = |name: &str| {
            generator
                .structures()
                .entries
                .iter()
                .any(|e| e.data.name == name && e.kind.is_some())
        };
        let ours = structured_terrain(dimension, pos);
        let mut ours_refs: Vec<(String, Vec<i64>)> = ours
            .structure_references
            .iter()
            .map(|r| {
                let mut keys: Vec<i64> = r.starts.iter().map(|s| chunk_key(s.chunk)).collect();
                keys.sort();
                (
                    tuff_vanilla_data::structure::STRUCTURES[r.structure]
                        .0
                        .to_string(),
                    keys,
                )
            })
            .collect();
        ours_refs.sort();
        let mut theirs_refs: Vec<(String, Vec<i64>)> = structures
            .get_compound("References")
            .map(|refs| {
                refs.iter()
                    .filter(|(name, _)| implemented(name))
                    .map(|(name, keys)| {
                        let mut keys = keys.as_long_array().unwrap_or(&[]).to_vec();
                        keys.sort();
                        (name.to_string(), keys)
                    })
                    .collect()
            })
            .unwrap_or_default();
        theirs_refs.sort();
        if ours_refs != theirs_refs {
            failures.push(format!(
                "{dimension} {pos:?} references: ours {ours_refs:?} vanilla {theirs_refs:?}"
            ));
        }
        compare_starts(
            dimension,
            pos,
            &structures,
            &ours.structure_starts,
            &mut failures,
        );
    }
    for (dimension, [x, z]) in STARTS_KEPT {
        let pos = ChunkPos::new(x, z);
        let Some(root) = chunks.get_compound(&format!("starts {}", key(dimension, pos))) else {
            failures.push(format!("{dimension} {pos:?}: not in the fixture"));
            continue;
        };
        let structures = root.get_compound("structures").cloned().unwrap_or_default();
        compare_starts(
            dimension,
            pos,
            &structures,
            &starts(dimension, pos),
            &mut failures,
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Where two saved structure starts differ: the first differing piece,
/// or the fields beside the pieces.
fn start_difference(
    dimension: &str,
    pos: ChunkPos,
    name: &str,
    ours: Option<&Tag>,
    theirs: &Tag,
) -> String {
    let (Some(Tag::Compound(ours)), Tag::Compound(theirs)) = (ours, theirs) else {
        return format!("{dimension} {pos:?} start {name}: ours {ours:?} vanilla {theirs}");
    };
    let children = |c: &Compound| {
        c.get_list("Children")
            .map(<[Tag]>::to_vec)
            .unwrap_or_default()
    };
    let (a, b) = (children(ours), children(theirs));
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        if x != y {
            return format!(
                "{dimension} {pos:?} start {name}: piece {i} of {}/{} differs\n  ours    {x}\n  vanilla {y}",
                a.len(),
                b.len()
            );
        }
    }
    let strip = |c: &Compound| {
        let mut c = c.clone();
        c.remove("Children");
        c
    };
    format!(
        "{dimension} {pos:?} start {name}: {} pieces vs vanilla {}; ours {} vanilla {}",
        a.len(),
        b.len(),
        strip(ours),
        strip(theirs)
    )
}

fn compare_starts(
    dimension: &str,
    pos: ChunkPos,
    structures: &Compound,
    ours: &[Arc<crate::structure::StructureStart>],
    failures: &mut Vec<String>,
) {
    let generator = generator(dimension);
    let implemented = |name: &str| {
        generator
            .structures()
            .entries
            .iter()
            .any(|e| e.data.name == name && e.kind.is_some())
    };
    {
        let theirs_starts = structures
            .get_compound("starts")
            .cloned()
            .unwrap_or_default();
        for (name, start) in theirs_starts.iter() {
            if !implemented(name) {
                continue;
            }
            let ours_start = ours
                .iter()
                .find(|s| s.structure.name == name)
                .map(|s| Tag::Compound(s.to_nbt()));
            // Whether a spider corridor or fortress throne placed its
            // spawner, or a fortress turn its chest, depends on which chunks
            // were decorated before the start chunk was saved.
            let without_progress = |tag: &Tag| {
                let mut tag = tag.clone();
                if let Some(children) = tag
                    .as_compound_mut()
                    .and_then(|c| c.get_list_mut("Children"))
                {
                    for child in children {
                        if let Some(child) = child.as_compound_mut() {
                            for progress in ["hps", "Mob", "Chest"] {
                                child.remove(progress);
                            }
                            // Pieces that settle on the terrain the first
                            // time they are placed move their box then.
                            let settles = matches!(
                                child.get_str("id"),
                                Some(
                                    "minecraft:tedp"
                                        | "minecraft:tejp"
                                        | "minecraft:tesh"
                                        | "minecraft:iglu"
                                        | "minecraft:shipwreck"
                                        | "minecraft:btp"
                                        | "minecraft:orp"
                                )
                            );
                            if settles {
                                for progress in [
                                    "BB",
                                    "HPos",
                                    "TPY",
                                    "height_adjusted",
                                    "hasPlacedChest0",
                                    "hasPlacedChest1",
                                    "hasPlacedChest2",
                                    "hasPlacedChest3",
                                    "placedMainChest",
                                    "placedHiddenChest",
                                    "placedTrap1",
                                    "placedTrap2",
                                    "Witch",
                                    "Cat",
                                ] {
                                    child.remove(progress);
                                }
                            }
                        }
                    }
                }
                tag
            };
            let ours_start = ours_start.as_ref().map(without_progress);
            let start = &without_progress(start);
            if ours_start.as_ref() != Some(start) {
                failures.push(start_difference(
                    dimension,
                    pos,
                    name,
                    ours_start.as_ref(),
                    start,
                ));
            }
        }
        for start in ours {
            if !theirs_starts.contains_key(start.structure.name) {
                failures.push(format!(
                    "{dimension} {pos:?} start {} is not in vanilla",
                    start.structure.name
                ));
            }
        }
    }
}

/// Decorates every chunk of a square
/// (`MC_WORLDGEN_SCAN=<dim>,<min x>,<min z>,<max x>,<max z>`) in row order
/// and lists the block entities each ends with, to pick fixture centers
/// that hold structure chests and spawners.
#[tuff_core::watchdog]
#[test]
#[ignore = "diagnostic"]
fn block_entity_scan() {
    let spec = std::env::var("MC_WORLDGEN_SCAN").expect("MC_WORLDGEN_SCAN");
    let parts: Vec<&str> = spec.split(',').collect();
    let dimension = match parts[0] {
        "overworld" => "overworld",
        "the_nether" => "the_nether",
        _ => "the_end",
    };
    let [x0, z0, x1, z1]: [i32; 4] = std::array::from_fn(|i| parts[i + 1].parse().unwrap());
    let chunks: Vec<ChunkPos> = (z0..=z1)
        .flat_map(|z| (x0..=x1).map(move |x| ChunkPos::new(x, z)))
        .collect();
    let lines: Vec<String> = map_on_threads(&chunks, |pos| {
        let chunk = decorated(dimension, *pos);
        let mut ids: Vec<String> = chunk
            .block_entities
            .values()
            .map(|c| {
                format!(
                    "{}{}",
                    c.get_str("id").unwrap_or("?"),
                    c.get_str("LootTable")
                        .map(|t| format!("({t})"))
                        .unwrap_or_default()
                )
            })
            .collect();
        ids.sort();
        format!("{pos:?}: {}", ids.join(" "))
    });
    for line in lines {
        eprintln!("{line}");
    }
}

/// Lists the starts of one random spread structure set within `radius`
/// grid cells of the origin: `MC_WORLDGEN_SCAN_SET=<dim>,<set>,<radius>`.
#[tuff_core::watchdog]
#[test]
#[ignore = "diagnostic"]
fn structure_set_scan() {
    use crate::structure::placement::potential_chunk;
    use tuff_vanilla_data::structure::StructurePlacement;

    let spec = std::env::var("MC_WORLDGEN_SCAN_SET").expect("MC_WORLDGEN_SCAN_SET");
    let parts: Vec<&str> = spec.split(',').collect();
    let (dimension, name, radius) = (parts[0], parts[1], parts[2].parse::<i32>().unwrap());
    let generator = generator(dimension);
    let set = tuff_vanilla_data::structure::STRUCTURE_SETS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| s)
        .expect("structure set");
    let StructurePlacement::RandomSpread {
        spacing,
        separation,
        spread_type,
        spread,
    } = &set.placement
    else {
        panic!("{name} is not a random spread");
    };
    let names: Vec<&str> = set.structures.iter().map(|(n, _)| *n).collect();
    for gz in -radius..=radius {
        for gx in -radius..=radius {
            let cell = ChunkPos::new(gx * spacing, gz * spacing);
            let pos = potential_chunk(1, cell, *spacing, *separation, *spread_type, spread.salt);
            for start in generator.structure_starts(pos, shape(dimension)) {
                if names.contains(&start.structure.name) {
                    eprintln!(
                        "{} chunk {} {} pieces {} box {:?}",
                        start.structure.name,
                        pos.x,
                        pos.z,
                        start.pieces.len(),
                        start.bounding_box()
                    );
                }
            }
        }
    }
}

/// Every template an end city or a woodland mansion names exists in the
/// jar (their names are built in code).
#[tuff_core::watchdog]
#[test]
fn code_named_templates_exist() {
    let names: std::collections::HashSet<&str> =
        tuff_vanilla_data::structure::template_names().collect();
    for name in crate::structure::end_city::TEMPLATES {
        assert!(
            names.contains(format!("minecraft:end_city/{name}").as_str()),
            "{name}"
        );
    }
    let generator = generator("overworld");
    let starts = generator.structure_starts(ChunkPos::new(118, 15), shape("overworld"));
    let mansion = starts
        .iter()
        .find(|s| s.structure.name == "minecraft:mansion")
        .expect("a mansion starts in 118, 15");
    for piece in &mansion.pieces {
        let nbt = piece.to_nbt();
        let name = nbt.get_str("Template").unwrap();
        assert!(
            names.contains(format!("minecraft:woodland_mansion/{name}").as_str()),
            "{name}"
        );
    }
}

/// The end city ship's elytra hangs in an item frame, handed to the chunk
/// as entity data.
#[tuff_core::watchdog]
#[test]
fn end_city_ship_hangs_an_elytra() {
    let chunks: Vec<ProtoChunk> = [(6, 98), (6, 97), (7, 98), (7, 97), (6, 99), (7, 99)]
        .into_iter()
        .map(|(x, z)| decorated_exact("the_end", ChunkPos::new(x, z)))
        .collect();
    let frames: Vec<_> = chunks
        .iter()
        .flat_map(|c| c.entities.iter())
        .filter(|e| e.get_str("id") == Some("minecraft:item_frame"))
        .collect();
    assert_eq!(frames.len(), 1, "{frames:?}");
    let item = frames[0].get_compound("Item").unwrap();
    assert_eq!(item.get_str("id"), Some("minecraft:elytra"));
}

#[tuff_core::watchdog]
#[test]
fn swamp_huts_house_a_witch_and_a_cat() {
    let mut mobs: Vec<String> = (-256..=-254)
        .flat_map(|x| (-87..=-85).map(move |z| (x, z)))
        .flat_map(|(x, z)| decorated("overworld", ChunkPos::new(x, z)).entities)
        .filter_map(|e| e.get_str("id").map(str::to_owned))
        .collect();
    mobs.sort();
    assert_eq!(mobs, ["minecraft:cat", "minecraft:witch"]);
}

#[tuff_core::watchdog]
#[test]
fn monument_elders_wait_in_their_chunks() {
    let mut elders: Vec<Vec<f64>> = [(76, -77), (76, -78), (76, -75)]
        .into_iter()
        .flat_map(|(x, z)| decorated("overworld", ChunkPos::new(x, z)).entities)
        .filter(|e| e.get_str("id") == Some("minecraft:elder_guardian"))
        .map(|e| {
            e.get_list("Pos")
                .unwrap()
                .iter()
                .map(|v| match v {
                    Tag::Double(d) => *d,
                    other => panic!("{other:?}"),
                })
                .collect()
        })
        .collect();
    elders.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(
        elders,
        [
            vec![1216.5, 53.0, -1216.5],
            vec![1227.5, 42.0, -1232.5],
            vec![1230.5, 45.0, -1199.5],
        ]
    );
}

/// `/locate` finds what vanilla finds from the same spots
/// (`scripts/locate-reference.sh`): the same place, name and distance.
#[tuff_core::watchdog]
#[test]
fn locate_answers_like_vanilla() {
    use tuff_feature_builtin_vanilla_commands::locate::{Target, distance, members};
    use tuff_world_model::BlockPos;

    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/worldgen/locate.txt"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let mut failures = Vec::new();
    for line in text.lines() {
        let (query, answer) = line.split_once('|').unwrap();
        let q: Vec<&str> = query.split_whitespace().collect();
        let [dimension, x, y, z, kind, argument] = q[..] else {
            panic!("bad query {query}");
        };
        let from = BlockPos::new(x.parse().unwrap(), y.parse().unwrap(), z.parse().unwrap());
        let target = if kind == "structure" {
            Target::Structure
        } else {
            Target::Biome
        };
        let (tag, name) = match argument.strip_prefix('#') {
            Some(name) => (true, name),
            None => (false, argument),
        };
        let ids = members(target, name, tag, &tuff_datapack::tags::TagSet::vanilla()).unwrap();
        let generator = generator(dimension);
        let found = match target {
            Target::Structure => crate::locate::structure(generator, &ids, from, shape(dimension)),
            Target::Biome => crate::locate::biome(generator, &ids, from, shape(dimension)),
        };
        let ours = match found {
            Some((pos, id)) => {
                let shown = if tag {
                    format!("{argument} ({id})")
                } else {
                    argument.to_owned()
                };
                let y = match target {
                    Target::Structure => "~".to_owned(),
                    Target::Biome => pos.y.to_string(),
                };
                format!(
                    "The nearest {shown} is at [{}, {y}, {}] ({} blocks away)",
                    pos.x,
                    pos.z,
                    distance(target, from, pos)
                )
            }
            None => "none".to_owned(),
        };
        let theirs = if answer.starts_with("The nearest") {
            answer
        } else {
            "none"
        };
        if ours != theirs {
            failures.push(format!("{query}: ours {ours} vanilla {theirs}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `/locate structure minecraft:trial_chambers` finds the trial chamber
/// starts vanilla's chunks reference, from spots near each.
#[tuff_core::watchdog]
#[test]
fn locate_finds_vanilla_trial_chambers() {
    use tuff_feature_builtin_vanilla_commands::locate::{Target, members};
    use tuff_world_model::BlockPos;

    let ids = members(
        Target::Structure,
        "minecraft:trial_chambers",
        false,
        &tuff_datapack::tags::TagSet::vanilla(),
    )
    .unwrap();
    let generator = generator("overworld");
    for (from, start) in [
        ([650, -20, 790], [40, 48]),
        ([300, -20, 230], [18, 14]),
        ([-340, -20, 190], [-22, 11]),
        ([280, -20, 9350], [17, 585]),
    ] {
        let from = BlockPos::new(from[0], from[1], from[2]);
        let (pos, name) = crate::locate::structure(generator, &ids, from, shape("overworld"))
            .unwrap_or_else(|| panic!("no trial chambers found from {from:?}"));
        assert_eq!(name, "minecraft:trial_chambers");
        assert_eq!([pos.x >> 4, pos.z >> 4], start, "from {from:?}");
    }
}

/// The ids of the entities the start of `structure` in a chunk within
/// eight of `near` leaves across every chunk it covers.
fn structure_entities(dimension: &str, near: ChunkPos, structure: &str) -> Vec<String> {
    let start = (-8..=8)
        .flat_map(|dz| (-8..=8).map(move |dx| ChunkPos::new(near.x + dx, near.z + dz)))
        .flat_map(|pos| starts(dimension, pos))
        .find(|s| s.structure.name == structure)
        .unwrap_or_else(|| panic!("no {structure} near {near:?}"));
    let b = start.bounding_box();
    let chunks: Vec<ChunkPos> = (b.min_z >> 4..=b.max_z >> 4)
        .flat_map(|z| (b.min_x >> 4..=b.max_x >> 4).map(move |x| ChunkPos::new(x, z)))
        .collect();
    let mut ids: Vec<String> = map_on_threads(&chunks, |pos| decorated(dimension, *pos).entities)
        .into_iter()
        .flatten()
        .filter_map(|e| e.get_str("id").map(str::to_owned))
        .collect();
    ids.sort();
    ids
}

/// Ocean ruins, igloo basements, mansions and end cities leave their mobs
/// in the chunks they cover: data markers become drowned, evokers,
/// vindicators, allays and shulkers; the igloo's template holds a villager
/// and a zombie villager.
#[tuff_core::watchdog]
#[test]
fn structures_leave_their_mobs() {
    let count = |ids: &[String], id: &str| ids.iter().filter(|i| *i == id).count();
    let warm = structure_entities(
        "overworld",
        ChunkPos::new(-18, 48),
        "minecraft:ocean_ruin_warm",
    );
    assert_eq!(count(&warm, "minecraft:drowned"), 6, "{warm:?}");
    let cold = structure_entities(
        "overworld",
        ChunkPos::new(-13, 1),
        "minecraft:ocean_ruin_cold",
    );
    assert_eq!(count(&cold, "minecraft:drowned"), 17, "{cold:?}");
    let igloo = structure_entities("overworld", ChunkPos::new(-9, -59), "minecraft:igloo");
    assert_eq!(igloo, ["minecraft:villager", "minecraft:zombie_villager"]);
    let mansion = structure_entities("overworld", ChunkPos::new(118, 15), "minecraft:mansion");
    assert_eq!(
        [
            "minecraft:evoker",
            "minecraft:vindicator",
            "minecraft:allay"
        ]
        .map(|id| count(&mansion, id)),
        [1, 16, 7],
        "{mansion:?}"
    );
    let city = structure_entities("the_end", ChunkPos::new(6, 98), "minecraft:end_city");
    assert_eq!(count(&city, "minecraft:shulker"), 32, "{city:?}");
}

/// A start recomputed after a restart takes back the placed-mob flags its
/// saved copy carries.
#[tuff_core::watchdog]
#[test]
fn saved_start_progress_is_read_back() {
    let pos = ChunkPos::new(-255, -86);
    let placed: Vec<Compound> = (-256..=-254)
        .flat_map(|x| (-87..=-85).map(move |z| ChunkPos::new(x, z)))
        .flat_map(|p| decorated("overworld", p).structure_starts)
        .filter(|s| s.structure.name == "minecraft:swamp_hut")
        .map(|s| s.to_nbt())
        .collect();
    let saved = placed.first().expect("a swamp hut start");
    let fresh = generator("overworld").structure_starts(pos, shape("overworld"));
    let start = fresh
        .iter()
        .find(|s| s.structure.name == "minecraft:swamp_hut")
        .expect("fresh swamp hut start");
    assert_ne!(start.to_nbt(), *saved);
    start.load_progress(saved);
    assert_eq!(start.to_nbt(), *saved);
}
