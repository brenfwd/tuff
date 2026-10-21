use super::fixtures::*;
use crate::proto::ProtoChunk;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::pos::ChunkPos;

#[tuff_core::watchdog]
#[test]
fn vanilla_features_parse() {
    for dimension in ["overworld", "the_nether", "the_end"] {
        let registry = generator(dimension).decorator().registry.lock().unwrap();
        assert!(registry.errors.is_empty(), "{:#?}", registry.errors);
        eprintln!(
            "{dimension} feature kinds not implemented: {:?}",
            registry.missing
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn vanilla_structures_parse() {
    for dimension in ["overworld", "the_nether", "the_end"] {
        let structures = generator(dimension).structures();
        assert!(structures.errors.is_empty(), "{:#?}", structures.errors);
        assert!(
            structures.missing.is_empty(),
            "{dimension} structure types not implemented: {:?}",
            structures.missing
        );
    }
}

type Order = [(i32, i32); 9];

/// Every full fixture chunk with the order its 3x3 was decorated in.
/// Vanilla's order is scheduling dependent; where the row order does not
/// reproduce it, the order is one `decoration_order_search` found to
/// reproduce every block.
const DECORATED_EXACT: [(&str, [i32; 2], &Order); 72] = [
    ("overworld", [-200, 300], &ROW_ORDER),
    ("overworld", [-563, -125], &ROW_ORDER),
    ("overworld", [360, -552], &ROW_ORDER),
    ("overworld", [-96, 56], &ROW_ORDER),
    ("overworld", [0, 1], &ROW_ORDER),
    ("overworld", [-56, 112], &ROW_ORDER),
    ("overworld", [24, -96], &ROW_ORDER),
    ("overworld", [-96, -184], &ROW_ORDER),
    ("overworld", [16, 560], &ROW_ORDER),
    ("overworld", [16, -80], &ROW_ORDER),
    ("overworld", [40, 640], &ROW_ORDER),
    (
        "overworld",
        [-12, -90],
        &[
            (0, -1),
            (-1, 1),
            (-1, 0),
            (0, 0),
            (1, -1),
            (-1, -1),
            (0, 1),
            (1, 1),
            (1, 0),
        ],
    ),
    (
        "overworld",
        [-156, -186],
        &[
            (-1, -1),
            (0, -1),
            (1, 0),
            (-1, 0),
            (0, 0),
            (1, -1),
            (-1, 1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [0, 0],
        &[
            (-1, 0),
            (0, -1),
            (1, -1),
            (-1, -1),
            (0, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [-48, -176],
        &[
            (-1, -1),
            (0, -1),
            (1, 1),
            (-1, 0),
            (0, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, -1),
        ],
    ),
    (
        "overworld",
        [0, -1],
        &[
            (-1, 0),
            (-1, 1),
            (1, -1),
            (-1, -1),
            (0, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [1, 1],
        &[
            (-1, -1),
            (0, -1),
            (-1, 1),
            (-1, 0),
            (0, 0),
            (1, 0),
            (1, -1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [-1, -1],
        &[
            (0, 0),
            (1, 1),
            (-1, -1),
            (0, 1),
            (1, 0),
            (0, -1),
            (-1, 0),
            (-1, 1),
            (1, -1),
        ],
    ),
    (
        "overworld",
        [1, -1],
        &[
            (-1, -1),
            (-1, 1),
            (1, 1),
            (-1, 0),
            (0, 0),
            (0, 1),
            (0, -1),
            (1, 0),
            (1, -1),
        ],
    ),
    (
        "overworld",
        [1, 0],
        &[
            (-1, -1),
            (0, -1),
            (-1, 1),
            (-1, 0),
            (0, 0),
            (1, 0),
            (1, -1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [100, -50],
        &[
            (-1, -1),
            (-1, 0),
            (-1, 1),
            (0, -1),
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
            (1, -1),
        ],
    ),
    ("the_nether", [0, 0], &ROW_ORDER),
    ("the_nether", [30, -20], &ROW_ORDER),
    ("the_nether", [0, 12], &ROW_ORDER),
    (
        "the_nether",
        [12, 18],
        &[
            (-1, 1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (0, 0),
            (1, 0),
            (-1, -1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "the_nether",
        [-18, 6],
        &[
            (-1, -1),
            (-1, 0),
            (1, -1),
            (0, -1),
            (0, 0),
            (0, 1),
            (-1, 1),
            (1, 0),
            (1, 1),
        ],
    ),
    ("the_end", [0, 0], &ROW_ORDER),
    ("the_end", [100, 10], &ROW_ORDER),
    ("the_end", [-3, -1], &ROW_ORDER),
    ("the_end", [-12, -72], &ROW_ORDER),
    ("the_end", [-66, 0], &ROW_ORDER),
    ("the_end", [6, 0], &ROW_ORDER),
    ("the_end", [89, 4], &ROW_ORDER),
    ("overworld", [-1, 0], &ROW_ORDER),
    (
        "overworld",
        [-1, 1],
        &[
            (0, -1),
            (-1, -1),
            (1, -1),
            (-1, 0),
            (0, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [19, 18],
        &[
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (-1, 1),
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [-111, -33],
        &[
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (0, 0),
            (0, 1),
            (-1, 1),
            (1, 0),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [-345, 0],
        &[
            (-1, -1),
            (0, -1),
            (1, 1),
            (-1, 0),
            (0, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, -1),
        ],
    ),
    ("overworld", [21, 569], &ROW_ORDER),
    (
        "the_nether",
        [15, -40],
        &[
            (-1, -1),
            (-1, 0),
            (0, -1),
            (0, 0),
            (1, -1),
            (-1, 1),
            (0, 1),
            (1, 0),
            (1, 1),
        ],
    ),
    (
        "the_nether",
        [13, -40],
        &[
            (1, -1),
            (1, 0),
            (1, 1),
            (0, -1),
            (0, 0),
            (-1, 0),
            (-1, -1),
            (0, 1),
            (-1, 1),
        ],
    ),
    (
        "overworld",
        [-25, 15],
        &[
            (-1, -1),
            (0, -1),
            (1, 0),
            (-1, 0),
            (0, 0),
            (1, -1),
            (-1, 1),
            (0, 1),
            (1, 1),
        ],
    ),
    ("overworld", [70, 136], &ROW_ORDER),
    ("overworld", [-16, -58], &ROW_ORDER),
    (
        "overworld",
        [39, 51],
        &[
            (-1, 0),
            (0, -1),
            (1, -1),
            (-1, -1),
            (0, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ],
    ),
    ("the_nether", [13, 1], &ROW_ORDER),
    ("the_nether", [31, -10], &ROW_ORDER),
    ("the_nether", [-16, -29], &ROW_ORDER),
    ("the_nether", [15, -60], &ROW_ORDER),
    ("the_nether", [22, -10], &ROW_ORDER),
    ("overworld", [125, -188], &ROW_ORDER),
    ("overworld", [1, 61], &ROW_ORDER),
    ("overworld", [17, 597], &ROW_ORDER),
    ("overworld", [-13, -20], &ROW_ORDER),
    ("overworld", [28, 10], &ROW_ORDER),
    ("overworld", [-13, 1], &ROW_ORDER),
    ("the_end", [6, 98], &ROW_ORDER),
    (
        "overworld",
        [76, -76],
        &[
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (-1, 1),
            (0, 0),
            (1, 0),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [118, 15],
        &[
            (-1, -1),
            (-1, 0),
            (-1, 1),
            (0, -1),
            (0, 0),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [22, 580],
        &[
            (-1, 1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (0, 0),
            (1, 0),
            (-1, -1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [27, -41],
        &[
            (-1, -1),
            (0, -1),
            (-1, 1),
            (-1, 0),
            (0, 0),
            (1, 0),
            (1, -1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [-18, 48],
        &[
            (-1, 0),
            (0, -1),
            (1, -1),
            (-1, -1),
            (0, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [109, 43],
        &[
            (-1, -1),
            (-1, 0),
            (-1, 1),
            (0, -1),
            (0, 0),
            (1, -1),
            (1, 0),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "the_nether",
        [-6, 6],
        &[
            (-1, -1),
            (-1, 0),
            (0, -1),
            (-1, 1),
            (0, 0),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ],
    ),
    ("overworld", [-48, -82], &ROW_ORDER),
    ("overworld", [101, -88], &ROW_ORDER),
    (
        "overworld",
        [-255, -86],
        &[
            (-1, 0),
            (0, -1),
            (1, -1),
            (-1, -1),
            (0, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [-147, 8],
        &[
            (-1, -1),
            (-1, 0),
            (1, -1),
            (0, -1),
            (0, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [-10, -113],
        &[
            (-1, -1),
            (0, -1),
            (-1, 1),
            (-1, 0),
            (0, 0),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [-151, 18],
        &[
            (-1, -1),
            (-1, 0),
            (0, -1),
            (-1, 1),
            (1, -1),
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [-9, -59],
        &[
            (-1, -1),
            (0, -1),
            (-1, 1),
            (-1, 0),
            (0, 0),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ],
    ),
    (
        "overworld",
        [192, -152],
        &[
            (-1, -1),
            (-1, 0),
            (0, -1),
            (-1, 1),
            (0, 0),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ],
    ),
];

/// A fixture chunk decorated in its known order (row order if none).
pub(super) fn decorated_exact(dimension: &str, pos: ChunkPos) -> ProtoChunk {
    let order = DECORATED_EXACT
        .iter()
        .find(|(d, [x, z], _)| *d == dimension && ChunkPos::new(*x, *z) == pos)
        .map_or(&ROW_ORDER, |(_, _, order)| *order);
    decorate_in_order(dimension, pos, terrain(dimension, pos), order)
}

/// Every full fixture chunk matches vanilla block for block once its 3x3
/// is decorated in the chunk's known order.
#[tuff_core::watchdog]
#[test]
fn full_chunks_match_vanilla() {
    let _watchdog = tuff_core::watchdog::arm(tuff_core::watchdog::DEFAULT);
    let kept = kept("full");
    for (dimension, pos) in &kept {
        assert!(
            DECORATED_EXACT
                .iter()
                .any(|(d, [x, z], _)| d == dimension && ChunkPos::new(*x, *z) == *pos),
            "{dimension} {pos:?} has no decoration order"
        );
    }
    let failures: Vec<String> = map_on_threads(&DECORATED_EXACT, |&(dimension, [x, z], order)| {
        let pos = ChunkPos::new(x, z);
        let (theirs, _) = load("full", dimension, pos);
        let ours = decorate_in_order(dimension, pos, terrain(dimension, pos), order);
        let (blocks, biomes, lines) = diff(&ours, &theirs, 12);
        (blocks + biomes > 0).then(|| {
            format!(
                "{dimension} {pos:?}: {blocks} blocks and {biomes} biomes differ\n  {}",
                lines.join("\n  ")
            )
        })
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(failures.is_empty(), "full:\n{}", failures.join("\n"));
}

/// The block entity data features and structures leave matches vanilla's:
/// a monster room's spawner mob and chest loot seed (drawn from the
/// feature's random stream), an end gateway's exit, and the chest loot
/// seeds (drawn from the structure's random) of ruined portals, a
/// fortress (with its blaze spawner), bastions of every kind (with the
/// treasure room's spawner), a pillager outpost, and a trial chamber's
/// trial spawner and vaults (their configs).
#[tuff_core::watchdog]
#[test]
fn feature_block_entities_match_vanilla() {
    let _watchdog = tuff_core::watchdog::arm(tuff_core::watchdog::DEFAULT);
    const KEYS: [&str; 13] = [
        "id",
        "x",
        "y",
        "z",
        "LootTable",
        "LootTableSeed",
        "SpawnData",
        "Age",
        "exit_portal",
        "ExactTeleport",
        "normal_config",
        "ominous_config",
        "config",
    ];
    // Vanilla saves a vault's config without the default reward table the
    // template spells out.
    let pick = |c: &Compound| -> Vec<(String, Tag)> {
        KEYS.iter()
            .filter_map(|k| c.get(k).map(|v| (k.to_string(), v.clone())))
            .map(|(k, v)| match v {
                Tag::Compound(mut config) if k == "config" => {
                    if config.get_str("loot_table")
                        == Some("minecraft:chests/trial_chambers/reward")
                    {
                        config.remove("loot_table");
                    }
                    (k, Tag::Compound(config))
                }
                v => (k, v),
            })
            .collect()
    };
    // Ruined portal chests are the only block entities of their chunks this
    // server places (the jungle one also holds a sculk shrieker, which gets
    // no block entity here yet).
    let chunks = [
        ("overworld", [192, -152], None, 2),
        ("the_end", [89, 4], None, 1),
        ("overworld", [19, 18], Some("minecraft:chest"), 1),
        ("overworld", [1, 61], Some("minecraft:chest"), 1),
        ("overworld", [-25, 15], Some("minecraft:chest"), 1),
        ("overworld", [-111, -33], Some("minecraft:chest"), 1),
        ("overworld", [-147, 8], Some("minecraft:chest"), 1),
        ("overworld", [-345, 0], Some("minecraft:chest"), 1),
        ("overworld", [21, 569], Some("minecraft:chest"), 1),
        ("the_nether", [15, -40], Some("minecraft:chest"), 1),
        ("the_nether", [22, -10], None, 2),
        ("the_nether", [13, 1], None, 2),
        ("the_nether", [31, -10], None, 5),
        ("the_nether", [-16, -29], None, 2),
        ("the_nether", [15, -60], None, 1),
        ("overworld", [70, 136], Some("minecraft:chest"), 1),
        ("overworld", [17, 597], None, 7),
        ("overworld", [-13, -20], None, 1),
        ("overworld", [28, 10], None, 1),
        ("overworld", [27, -41], None, 1),
        ("overworld", [-18, 48], None, 6),
        ("overworld", [-13, 1], None, 15),
        ("overworld", [109, 43], Some("minecraft:mob_spawner"), 1),
        ("overworld", [22, 580], Some("minecraft:chest"), 1),
        ("the_end", [6, 98], None, 2),
        ("overworld", [39, 51], Some("minecraft:trial_spawner"), 1),
        ("overworld", [39, 51], Some("minecraft:vault"), 3),
    ];
    map_on_threads(&chunks, |&(dimension, [x, z], only, count)| {
        let pos = ChunkPos::new(x, z);
        let (_, root) = load("full", dimension, pos);
        let ours = decorated_exact(dimension, pos);
        let mut theirs: Vec<_> = root
            .get_list("block_entities")
            .expect("vanilla block entities")
            .iter()
            .map(|t| t.as_compound().expect("compound"))
            .filter(|c| only.is_none_or(|id| c.get_str("id") == Some(id)))
            .map(pick)
            .collect();
        let mut ours: Vec<_> = ours
            .block_entities
            .values()
            .filter(|c| only.is_none_or(|id| c.get_str("id") == Some(id)))
            .map(pick)
            .collect();
        theirs.sort_by_key(|e| format!("{e:?}"));
        ours.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(theirs.len(), count, "{dimension} {pos:?}");
        assert_eq!(ours, theirs, "{dimension} {pos:?}");
    });
}

#[tuff_core::watchdog]
#[test]
#[ignore = "diagnostic summary of full-chunk differences"]
fn full_chunk_difference_summary() {
    use std::collections::HashMap;
    for (dimension, pos) in kept("full") {
        let (theirs, _) = load("full", dimension, pos);
        let ours = decorated_exact(dimension, pos);
        let mut pairs: HashMap<(String, String), usize> = HashMap::new();
        let shape = ours.shape;
        for y in shape.min_y..shape.min_y + shape.height {
            for z in 0..16 {
                for x in 0..16 {
                    let (a, b) = (ours.get(x, y, z), theirs.get(x, y, z));
                    if a != b {
                        *pairs
                            .entry((format!("{:?}", a.block()), format!("{:?}", b.block())))
                            .or_default() += 1;
                    }
                }
            }
        }
        let mut pairs: Vec<_> = pairs.into_iter().collect();
        pairs.sort_by_key(|a| std::cmp::Reverse(a.1));
        pairs.truncate(12);
        eprintln!("{dimension} {pos:?}: {pairs:?}");
    }
}

/// Lists the differing blocks of one full fixture chunk, named by
/// `MC_WORLDGEN_CHUNK=<dimension>,<x>,<z>`.
#[tuff_core::watchdog]
#[test]
#[ignore = "diagnostic listing of one chunk's differences"]
fn full_chunk_differences() {
    let spec = std::env::var("MC_WORLDGEN_CHUNK").expect("MC_WORLDGEN_CHUNK=<dimension>,<x>,<z>");
    let parts: Vec<&str> = spec.split(',').collect();
    let dimension = match parts[0] {
        "overworld" => "overworld",
        "the_nether" => "the_nether",
        _ => "the_end",
    };
    let pos = ChunkPos::new(parts[1].parse().unwrap(), parts[2].parse().unwrap());
    let (theirs, root) = load("full", dimension, pos);
    let ours = decorated_exact(dimension, pos);
    let (blocks, biomes, lines) = diff(&ours, &theirs, 400);
    eprintln!("{blocks} blocks and {biomes} biomes differ");
    for line in lines {
        eprintln!("{line}");
    }
    if let Ok(column) = std::env::var("MC_WORLDGEN_COLUMN") {
        let [x, z]: [i32; 2] = {
            let v: Vec<i32> = column.split(',').map(|v| v.parse().unwrap()).collect();
            [v[0], v[1]]
        };
        let shape = ours.shape;
        for y in (shape.min_y..shape.min_y + shape.height).rev() {
            let (a, b) = (ours.get(x, y, z), theirs.get(x, y, z));
            if !tuff_world_model::chunk::is_air(a) || !tuff_world_model::chunk::is_air(b) {
                eprintln!("{y}: ours {a:?} vanilla {b:?}");
            }
        }
    }
    eprintln!("vanilla block entities: {:?}", root.get("block_entities"));
    eprintln!("our block entities: {:?}", ours.block_entities);
}

/// Searches for the decoration order of the 3x3 around one fixture chunk
/// (`MC_WORLDGEN_CHUNK=<dimension>,<x>,<z>`) that comes closest to vanilla,
/// by hill climbing over pairwise swaps from the row order. Reaching zero
/// differences shows a chunk's remaining mismatch is only the order.
#[tuff_core::watchdog]
#[test]
#[ignore = "diagnostic search over decoration orders"]
fn decoration_order_search() {
    use rayon::prelude::*;
    let spec = std::env::var("MC_WORLDGEN_CHUNK").expect("MC_WORLDGEN_CHUNK=<dimension>,<x>,<z>");
    let parts: Vec<&str> = spec.split(',').collect();
    let dimension = match parts[0] {
        "overworld" => "overworld",
        "the_nether" => "the_nether",
        _ => "the_end",
    };
    let pos = ChunkPos::new(parts[1].parse().unwrap(), parts[2].parse().unwrap());
    let (theirs, _) = load("full", dimension, pos);
    let base = terrain(dimension, pos);
    let score = |order: &[(i32, i32)]| {
        let ours = decorate_in_order(dimension, pos, base.clone(), order);
        diff(&ours, &theirs, 0).0
    };
    let climb = |mut order: Vec<(i32, i32)>| {
        let mut best = score(&order);
        while best > 0 {
            let swaps: Vec<(usize, usize)> = (0..9)
                .flat_map(|i| (i + 1..9).map(move |j| (i, j)))
                .collect();
            let (count, next) = swaps
                .par_iter()
                .map(|&(i, j)| {
                    let mut o = order.clone();
                    o.swap(i, j);
                    (score(&o), o)
                })
                .min_by_key(|c| c.0)
                .unwrap();
            if count >= best {
                break;
            }
            best = count;
            order = next;
        }
        (best, order)
    };
    eprintln!("row order: {} blocks differ", score(&ROW_ORDER));
    let (mut best, mut order) = climb(ROW_ORDER.to_vec());
    let mut state = 0x9e37_79b9_u32;
    for restart in 0..12 {
        if best == 0 {
            break;
        }
        let mut start = ROW_ORDER.to_vec();
        for i in (1..9).rev() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            start.swap(i, state as usize % (i + 1));
        }
        let (count, found) = climb(start);
        eprintln!("restart {restart}: {count} blocks differ");
        if count < best {
            best = count;
            order = found;
        }
    }
    eprintln!("best: {best} blocks differ with order {order:?}");
    let ours = decorate_in_order(dimension, pos, base, &order);
    for line in diff(&ours, &theirs, 40).2 {
        eprintln!("{line}");
    }
}

/// A decorated chunk saved the way the game saves chunks in generation
/// reads back as the same chunk.
#[tuff_core::watchdog]
#[test]
fn chunks_in_progress_round_trip_through_storage() {
    use crate::feature::level::Region;
    use crate::proto::HeightmapKind;
    use crate::saved::{self, Saved};
    use tuff_core::storage::codec::ChunkCodec;
    use tuff_core::storage::codec::NbtChunkCodec;

    let pos = ChunkPos::new(-18, 48);
    let mut chunks = terrain("overworld", pos);
    let window = super::fixtures::ROW_ORDER
        .iter()
        .map(|(dx, dz)| {
            chunks
                .remove(&ChunkPos::new(pos.x + dx, pos.z + dz))
                .unwrap()
        })
        .collect();
    let generator = generator("overworld");
    let mut region = Region::new(generator, pos, window);
    generator.decorate(&mut region);
    let mut proto = region
        .into_chunks()
        .into_iter()
        .find(|c| c.pos == pos)
        .unwrap();
    proto.height(HeightmapKind::MotionBlocking, 0, 0);
    assert!(!proto.entities.is_empty() && !proto.block_entities.is_empty());
    assert!(!proto.post_processing.is_empty());
    let codec = NbtChunkCodec::default();
    let blob = codec.encode_root(saved::to_nbt(&proto, true)).unwrap();
    let root = codec.decode_root(&blob, proto.shape).unwrap();
    assert_eq!(root.get_str("Status"), Some(saved::STATUS_FEATURES));
    let Saved::Proto(back, decorated) = saved::from_nbt(&root, pos, proto.shape) else {
        panic!("not read back as a chunk in progress");
    };
    assert!(decorated);
    assert_eq!(back.biomes(), proto.biomes());
    for kind in [
        HeightmapKind::WorldSurfaceWg,
        HeightmapKind::OceanFloorWg,
        HeightmapKind::MotionBlocking,
        HeightmapKind::OceanFloor,
    ] {
        assert_eq!(back.heightmap(kind), proto.heightmap(kind), "{kind:?}");
    }
    let sorted = |p: &ProtoChunk| {
        let mut marks = p.post_processing.clone();
        marks.sort();
        marks
    };
    assert_eq!(sorted(&back), sorted(&proto));
    assert_eq!(back.fluid_ticks, proto.fluid_ticks);
    assert_eq!(back.block_ticks, proto.block_ticks);
    assert_eq!(back.block_entities, proto.block_entities);
    assert_eq!(back.entities, proto.entities);
    assert!(back.clone().into_chunk() == proto.clone().into_chunk());
    assert_eq!(
        back.highest_filled_section(),
        proto.highest_filled_section()
    );
}

/// The ceiling shortcut of the ore footprint scan never rejects a
/// footprint that the column scan would accept.
#[tuff_core::watchdog]
#[test]
fn ore_footprint_ceiling_agrees_with_the_column_scan() {
    use crate::feature::level::Region;
    use crate::feature::ore::reaches_ground;
    use crate::proto::HeightmapKind;

    let pos = ChunkPos::new(-18, 48);
    let mut chunks = terrain("overworld", pos);
    let window = super::fixtures::ROW_ORDER
        .iter()
        .map(|(dx, dz)| {
            chunks
                .remove(&ChunkPos::new(pos.x + dx, pos.z + dz))
                .unwrap()
        })
        .collect();
    let mut region = Region::new(generator("overworld"), pos, window);
    let ceiling = region.ocean_floor_ceiling().expect("settled heightmaps");
    let (min_y, max_y) = (region.min_y(), region.max_y());
    let mut state = 12345u64;
    let mut next = |n: i32| {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) % n as u64) as i32
    };
    let mut accepted = 0;
    for _ in 0..4000 {
        let size_xz = 2 + next(25);
        let x = pos.x * 16 - 40 + next(130);
        let z = pos.z * 16 - 40 + next(130);
        let y = min_y - 8 + next(max_y - min_y + 16);
        let scan = (x..=x + size_xz).any(|x| {
            (z..=z + size_xz).any(|z| y <= region.height(HeightmapKind::OceanFloorWg, x, z))
        });
        assert_eq!(reaches_ground(&mut region, [x, y, z], size_xz), scan);
        if y > ceiling {
            assert!(!scan);
        }
        accepted += scan as i32;
    }
    assert!(accepted > 0 && accepted < 4000);
}

/// Chunks the warm shortcut skips are chunks where the full column scan
/// would change nothing, and the shortcut does not skip chunks it changes.
#[tuff_core::watchdog]
#[test]
fn freeze_shortcut_only_skips_chunks_the_scan_leaves_alone() {
    use crate::feature::PlaceContext;
    use crate::feature::kinds::{chunk_stays_warm, freeze_columns};
    use crate::feature::level::Region;
    use tuff_item::random::{Random, WorldgenRandom, XoroshiroRandom};
    use tuff_world_model::BlockPos;

    let generator = generator("overworld");
    let (mut warm, mut cold) = (0, 0);
    for (cx, cz) in [
        (-18, 48),
        (0, 0),
        (40, -30),
        (200, 200),
        (-300, 120),
        (1000, -1000),
    ] {
        let pos = ChunkPos::new(cx, cz);
        let mut chunks = terrain("overworld", pos);
        let window = super::fixtures::ROW_ORDER
            .iter()
            .map(|(dx, dz)| {
                chunks
                    .remove(&ChunkPos::new(pos.x + dx, pos.z + dz))
                    .unwrap()
            })
            .collect();
        let mut region = Region::new(generator, pos, window);
        let origin = BlockPos::new(pos.x << 4, region.min_y(), pos.z << 4);
        let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(0)));
        let mut ctx = PlaceContext {
            level: &mut region,
            random: &mut random,
        };
        let skipped = chunk_stays_warm(&ctx, origin);
        let changed = freeze_columns(&mut ctx, origin);
        if skipped {
            assert_eq!(changed, 0, "chunk {cx} {cz}");
            warm += 1;
        } else {
            cold += 1;
        }
    }
    assert!(warm > 0, "{warm} {cold}");
}

/// The freeze check compares against the default water state, which is
/// the level 0 water it used to look for through the property.
#[tuff_core::watchdog]
#[test]
fn default_water_is_the_level_zero_state() {
    use tuff_vanilla_data::Block;
    let water = Block::Water.default_state();
    assert_eq!(water.get("level"), Some("0"));
    let level_one = water.with("level", "1").unwrap();
    assert_ne!(level_one, water);
    assert_eq!(level_one.with("level", "0"), Some(water));
}
