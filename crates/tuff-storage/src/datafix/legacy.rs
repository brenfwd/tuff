//! Chunks saved between the flattening (1.13) and 1.18: everything lives in a
//! `Level` compound, sections carry a flat `Palette` plus `BlockStates`, and
//! biomes are one int array per column. These are brought to the 1.18 layout
//! before the current-layout fixes run.

use super::{Fixer, for_each_compound, get_i32, item, namespaced, rename};
use tuff_nbt::{Compound, Tag};

const AIR: &str = "minecraft:air";
const PLAINS: &str = "minecraft:plains";
const OLD_SECTIONS: usize = 16;
const NEW_SECTIONS: usize = 24;
const NEW_MIN_SECTION: i32 = -4;
const HEIGHTMAP_BITS: usize = 9;
const HEIGHTMAP_MASK: i64 = 511;
const HEIGHTMAP_TYPES: [&str; 7] = [
    "WORLD_SURFACE_WG",
    "WORLD_SURFACE",
    "WORLD_SURFACE_IGNORE_SNOW",
    "OCEAN_FLOOR_WG",
    "OCEAN_FLOOR",
    "MOTION_BLOCKING",
    "MOTION_BLOCKING_NO_LEAVES",
];
const WALLS: [&str; 14] = [
    "andesite_wall",
    "brick_wall",
    "cobblestone_wall",
    "diorite_wall",
    "end_stone_brick_wall",
    "granite_wall",
    "mossy_cobblestone_wall",
    "mossy_stone_brick_wall",
    "nether_brick_wall",
    "prismarine_wall",
    "red_nether_brick_wall",
    "red_sandstone_wall",
    "sandstone_wall",
    "stone_brick_wall",
];
const ALWAYS_WATERLOGGED: [&str; 5] = [
    "minecraft:bubble_column",
    "minecraft:kelp",
    "minecraft:kelp_plant",
    "minecraft:seagrass",
    "minecraft:tall_seagrass",
];
const STATUS_AFTER_SURFACE: [&str; 8] = [
    "surface",
    "carvers",
    "liquid_carvers",
    "features",
    "light",
    "spawn",
    "heightmaps",
    "full",
];
const BLOCKS_BEFORE_FEATURES: [&str; 34] = [
    "minecraft:air",
    "minecraft:basalt",
    "minecraft:bedrock",
    "minecraft:blackstone",
    "minecraft:calcite",
    "minecraft:cave_air",
    "minecraft:coarse_dirt",
    "minecraft:crimson_nylium",
    "minecraft:dirt",
    "minecraft:end_stone",
    "minecraft:grass_block",
    "minecraft:gravel",
    "minecraft:ice",
    "minecraft:lava",
    "minecraft:mycelium",
    "minecraft:nether_wart_block",
    "minecraft:netherrack",
    "minecraft:orange_terracotta",
    "minecraft:packed_ice",
    "minecraft:podzol",
    "minecraft:powder_snow",
    "minecraft:red_sand",
    "minecraft:red_sandstone",
    "minecraft:sand",
    "minecraft:sandstone",
    "minecraft:snow_block",
    "minecraft:soul_sand",
    "minecraft:soul_soil",
    "minecraft:stone",
    "minecraft:terracotta",
    "minecraft:warped_nylium",
    "minecraft:warped_wart_block",
    "minecraft:water",
    "minecraft:white_terracotta",
];
const STATUS_AFTER_NOISE: [&str; 9] = [
    "noise",
    "surface",
    "carvers",
    "liquid_carvers",
    "features",
    "light",
    "spawn",
    "heightmaps",
    "full",
];

/// Biome ids of the pre-1.18 int arrays.
const BIOMES: [(u8, &str); 87] = [
    (0, "ocean"),
    (1, "plains"),
    (2, "desert"),
    (3, "mountains"),
    (4, "forest"),
    (5, "taiga"),
    (6, "swamp"),
    (7, "river"),
    (8, "nether_wastes"),
    (9, "the_end"),
    (10, "frozen_ocean"),
    (11, "frozen_river"),
    (12, "snowy_tundra"),
    (13, "snowy_mountains"),
    (14, "mushroom_fields"),
    (15, "mushroom_field_shore"),
    (16, "beach"),
    (17, "desert_hills"),
    (18, "wooded_hills"),
    (19, "taiga_hills"),
    (20, "mountain_edge"),
    (21, "jungle"),
    (22, "jungle_hills"),
    (23, "jungle_edge"),
    (24, "deep_ocean"),
    (25, "stone_shore"),
    (26, "snowy_beach"),
    (27, "birch_forest"),
    (28, "birch_forest_hills"),
    (29, "dark_forest"),
    (30, "snowy_taiga"),
    (31, "snowy_taiga_hills"),
    (32, "giant_tree_taiga"),
    (33, "giant_tree_taiga_hills"),
    (34, "wooded_mountains"),
    (35, "savanna"),
    (36, "savanna_plateau"),
    (37, "badlands"),
    (38, "wooded_badlands_plateau"),
    (39, "badlands_plateau"),
    (40, "small_end_islands"),
    (41, "end_midlands"),
    (42, "end_highlands"),
    (43, "end_barrens"),
    (44, "warm_ocean"),
    (45, "lukewarm_ocean"),
    (46, "cold_ocean"),
    (47, "deep_warm_ocean"),
    (48, "deep_lukewarm_ocean"),
    (49, "deep_cold_ocean"),
    (50, "deep_frozen_ocean"),
    (127, "the_void"),
    (129, "sunflower_plains"),
    (130, "desert_lakes"),
    (131, "gravelly_mountains"),
    (132, "flower_forest"),
    (133, "taiga_mountains"),
    (134, "swamp_hills"),
    (140, "ice_spikes"),
    (149, "modified_jungle"),
    (151, "modified_jungle_edge"),
    (155, "tall_birch_forest"),
    (156, "tall_birch_hills"),
    (157, "dark_forest_hills"),
    (158, "snowy_taiga_mountains"),
    (160, "giant_spruce_taiga"),
    (161, "giant_spruce_taiga_hills"),
    (162, "modified_gravelly_mountains"),
    (163, "shattered_savanna"),
    (164, "shattered_savanna_plateau"),
    (165, "eroded_badlands"),
    (166, "modified_wooded_badlands_plateau"),
    (167, "modified_badlands_plateau"),
    (168, "bamboo_jungle"),
    (169, "bamboo_jungle_hills"),
    (170, "soul_sand_valley"),
    (171, "crimson_forest"),
    (172, "warped_forest"),
    (173, "basalt_deltas"),
    (174, "dripstone_caves"),
    (175, "lush_caves"),
    (177, "meadow"),
    (178, "grove"),
    (179, "snowy_slopes"),
    (180, "snowcapped_peaks"),
    (181, "lofty_peaks"),
    (182, "stony_peaks"),
];

fn biome_name(id: i32) -> String {
    let id = (id & 255) as u8;
    let name = BIOMES
        .iter()
        .find(|(known, _)| *known == id)
        .map_or("plains", |(_, name)| name);
    format!("minecraft:{name}")
}

/// Brings a pre-1.18 chunk root to the 1.18 layout. `min_section` is the
/// lowest section of the dimension the chunk belongs to (-4 for the
/// overworld).
pub fn upgrade(root: &mut Compound, fixer: Fixer, min_section: i32) {
    let Some(Tag::Compound(mut level)) = root.remove("Level") else {
        return;
    };
    if fixer.due(1624, 0) {
        trapped_chests(&mut level);
    }
    if fixer.due(1905, 0) && level.get_str("Status") == Some("postprocessed") {
        level.insert("Status", "fullchunk");
    }
    if fixer.due(1911, 0) {
        rename_status_1911(&mut level);
    }
    if fixer.due(1920, 0) {
        new_village(&mut level);
    }
    fix_palettes(&mut level, fixer);
    if fixer.due(2202, 0) {
        column_biomes_to_cells(&mut level);
    }
    if fixer.due(2527, 0) {
        align_bit_storage(&mut level);
    }
    if fixer.due(2701, 0) {
        feature_pool_elements(&mut level);
    }
    if fixer.due(2832, 0) {
        height_and_biome(&mut level, min_section < 0);
    }
    if fixer.due(2841, 0) {
        proto_ticks(&mut level);
    }
    if fixer.due(2842, 0) {
        rename(&mut level, "TileEntities", "block_entities");
        rename(&mut level, "TileTicks", "block_ticks");
        rename(&mut level, "Entities", "entities");
        rename(&mut level, "Sections", "sections");
        if let Some(Tag::Compound(structures)) = level.get_mut("Structures") {
            rename(structures, "Starts", "starts");
        }
        rename(&mut level, "Structures", "structures");
    }
    for (key, value) in level {
        root.insert(key, value);
    }
    if fixer.due(2843, 0) {
        overreaching_ticks(root);
    }
}

const POOL_PIECES: [&str; 5] = [
    "minecraft:jigsaw",
    "minecraft:nvi",
    "minecraft:pcp",
    "minecraft:bastionremnant",
    "minecraft:runtime",
];

/// Feature pool elements that embedded a whole feature definition get the id
/// of the equivalent placed feature instead.
fn feature_pool_elements(level: &mut Compound) {
    let Some(starts) = level
        .get_compound_mut("Structures")
        .and_then(|s| s.get_compound_mut("Starts"))
    else {
        return;
    };
    for (_, start) in starts.iter_mut() {
        let Some(children) = start
            .as_compound_mut()
            .and_then(|s| s.get_list_mut("Children"))
        else {
            continue;
        };
        for child in children {
            let Some(child) = child.as_compound_mut() else {
                continue;
            };
            if !child
                .get_str("id")
                .is_some_and(|id| POOL_PIECES.contains(&id))
            {
                continue;
            }
            let Some(element) = child.get_compound_mut("pool_element") else {
                continue;
            };
            if element.get_str("element_type") != Some("minecraft:feature_pool_element") {
                continue;
            }
            let replacement = element
                .get_compound("feature")
                .and_then(feature_replacement);
            if let Some(replacement) = replacement {
                element.insert("feature", replacement);
            }
        }
    }
}

fn feature_replacement(feature: &Compound) -> Option<&'static str> {
    let text = |path: &[&str]| -> &str {
        let (last, parents) = path.split_last().unwrap_or((&"", &[]));
        let mut at = feature;
        for key in parents {
            match at.get_compound(key) {
                Some(next) => at = next,
                None => return "",
            }
        }
        at.get_str(last).unwrap_or("")
    };
    let config = feature.get_compound("config");
    let first_entry = config
        .and_then(|c| c.get_compound("state_provider"))
        .and_then(|p| p.get_list("entries"))
        .and_then(|entries| entries.first())
        .and_then(Tag::as_compound)
        .and_then(|entry| entry.get_compound("data"))
        .and_then(|data| data.get_str("Name"))
        .unwrap_or("");
    let kind = text(&["type"]);
    let name = text(&["name"]);
    let provider = text(&["config", "state_provider", "type"]);
    let state = text(&["config", "state_provider", "state", "Name"]);
    let foliage = text(&["config", "foliage_placer", "type"]);
    let leaves = text(&["config", "leaves_provider", "state", "Name"]);
    let kind = if !kind.is_empty() {
        kind
    } else if name == "minecraft:normal_tree" {
        "minecraft:tree"
    } else if !name.is_empty() {
        name
    } else {
        return None;
    };
    match kind {
        "minecraft:random_patch" => match provider {
            "minecraft:simple_state_provider" => match state {
                "minecraft:sweet_berry_bush" => Some("minecraft:patch_berry_bush"),
                "minecraft:cactus" => Some("minecraft:patch_cactus"),
                _ => None,
            },
            "minecraft:weighted_state_provider"
                if matches!(first_entry, "minecraft:grass" | "minecraft:fern") =>
            {
                Some("minecraft:patch_taiga_grass")
            }
            _ => None,
        },
        "minecraft:block_pile" => match provider {
            "minecraft:simple_state_provider" | "minecraft:rotated_block_provider" => match state {
                "minecraft:hay_block" => Some("minecraft:pile_hay"),
                "minecraft:melon" => Some("minecraft:pile_melon"),
                "minecraft:snow" => Some("minecraft:pile_snow"),
                _ => None,
            },
            "minecraft:weighted_state_provider" => match first_entry {
                "minecraft:packed_ice" | "minecraft:blue_ice" => Some("minecraft:pile_ice"),
                "minecraft:jack_o_lantern" | "minecraft:pumpkin" => Some("minecraft:pile_pumpkin"),
                _ => None,
            },
            _ => None,
        },
        "minecraft:flower" => Some("minecraft:flower_plain"),
        "minecraft:tree" => match foliage {
            "minecraft:acacia_foliage_placer" => Some("minecraft:acacia"),
            "minecraft:blob_foliage_placer" if leaves == "minecraft:oak_leaves" => {
                Some("minecraft:oak")
            }
            "minecraft:pine_foliage_placer" => Some("minecraft:pine"),
            "minecraft:spruce_foliage_placer" => Some("minecraft:spruce"),
            _ => None,
        },
        _ => None,
    }
}

/// 1.14 drops the starts and references of the old village structure and
/// renames the new one into its place.
fn new_village(level: &mut Compound) {
    let Some(structures) = level.get_compound_mut("Structures") else {
        return;
    };
    if let Some(Tag::Compound(starts)) = structures.get_mut("Starts") {
        let mut out = Compound::new();
        for (key, mut start) in std::mem::take(starts) {
            if key == "Village" {
                continue;
            }
            if let Tag::Compound(start) = &mut start
                && start.get_str("id").map(super::namespaced).as_deref()
                    == Some("minecraft:new_village")
            {
                start.insert("id", "minecraft:village");
            }
            out.insert(
                if key == "New_Village" {
                    "Village".into()
                } else {
                    key
                },
                start,
            );
        }
        *starts = out;
    }
    if let Some(Tag::Compound(references)) = structures.get_mut("References") {
        references.remove("New_Village");
        references.remove("Village");
    }
}

/// 1.15 keeps one biome per 4x4x4 cell instead of per column: each cell
/// takes the column at its centre, the same at every height.
fn column_biomes_to_cells(level: &mut Compound) {
    let Some(old) = level.get_int_array("Biomes").filter(|b| b.len() == 256) else {
        return;
    };
    let mut layer = [0i32; 16];
    for (cell, biome) in layer.iter_mut().enumerate() {
        let (x, z) = (cell & 3, cell >> 2);
        *biome = old[((z << 2) + 2) << 4 | ((x << 2) + 2)];
    }
    let biomes: Vec<i32> = layer.iter().copied().cycle().take(1024).collect();
    level.insert("Biomes", biomes);
}

fn packed_value(data: &[i64], bits: usize, index: usize) -> usize {
    let mask = (1u64 << bits) - 1;
    let start = index * bits;
    let (cell, shift) = (start >> 6, start & 63);
    let low = data.get(cell).copied().unwrap_or(0) as u64 >> shift;
    let value = if shift + bits > 64 {
        low | (data.get(cell + 1).copied().unwrap_or(0) as u64) << (64 - shift)
    } else {
        low
    };
    (value & mask) as usize
}

/// Chests standing where a section has a trapped chest become trapped chest
/// block entities.
fn trapped_chests(level: &mut Compound) {
    let mut positions = std::collections::HashSet::new();
    for_each_compound(level, "Sections", |section| {
        let Some(palette) = section.get_list("Palette") else {
            return;
        };
        let trapped: Vec<usize> = palette
            .iter()
            .enumerate()
            .filter(|(_, state)| {
                state
                    .as_compound()
                    .and_then(|state| state.get_str("Name"))
                    .is_some_and(|name| name == "minecraft:trapped_chest")
            })
            .map(|(index, _)| index)
            .collect();
        let bits = ceil_log2(palette.len()).max(4);
        let Some(Tag::LongArray(data)) = section.get("BlockStates") else {
            return;
        };
        if trapped.is_empty() {
            return;
        }
        let y = get_i32(section, "Y").unwrap_or(0);
        for index in 0..4096 {
            if trapped.contains(&packed_value(data, bits, index)) {
                positions.insert((y << 12) | index as i32);
            }
        }
    });
    if positions.is_empty() {
        return;
    }
    let chunk_x = get_i32(level, "xPos").unwrap_or(0);
    let chunk_z = get_i32(level, "zPos").unwrap_or(0);
    for_each_compound(level, "TileEntities", |entity| {
        let x = get_i32(entity, "x").unwrap_or(0) - (chunk_x << 4);
        let y = get_i32(entity, "y").unwrap_or(0);
        let z = get_i32(entity, "z").unwrap_or(0) - (chunk_z << 4);
        if positions.contains(&((y >> 4) << 12 | (y & 15) << 8 | (z & 15) << 4 | (x & 15)))
            && entity.get_str("id").is_some()
        {
            entity.insert("id", "minecraft:trapped_chest");
        }
    });
}

fn fix_palettes(level: &mut Compound, fixer: Fixer) {
    for key in ["Sections", "sections"] {
        for_each_compound(level, key, |section| {
            if let Some(palette) = section.get_list_mut("Palette") {
                for state in palette {
                    if let Some(state) = state.as_compound_mut() {
                        fix_state(state, fixer);
                    }
                }
            }
        });
    }
    for key in ["TileTicks", "LiquidTicks"] {
        for_each_compound(level, key, |tick| {
            if let Some(Tag::String(name)) = tick.get_mut("i") {
                item::rename_block_name(name, fixer);
            }
        });
    }
}

/// The block state fixes of data versions 2503 to 2700 on one `{Name,
/// Properties}` compound.
pub(crate) fn fix_state(state: &mut Compound, fixer: Fixer) {
    if let Some(Tag::String(name)) = state.get_mut("Name") {
        item::rename_block_name(name, fixer);
    }
    let name = state.get_str("Name").map(namespaced).unwrap_or_default();
    if fixer.due(2503, 0)
        && let Some(short) = name.strip_prefix("minecraft:")
        && WALLS.contains(&short)
        && let Some(properties) = state.get_compound_mut("Properties")
    {
        for side in ["east", "west", "north", "south"] {
            if let Some(value) = properties.get_str(side) {
                let new = if value == "true" { "low" } else { "none" };
                properties.insert(side, new);
            }
        }
    }
    if fixer.due(2531, 0)
        && name == "minecraft:redstone_wire"
        && let Some(properties) = state.get_compound_mut("Properties")
    {
        redstone_connections(properties);
    }
    if fixer.due(2518, 0)
        && name == "minecraft:jigsaw"
        && let Some(properties) = state.get_compound_mut("Properties")
    {
        let facing = properties.get_str("facing").unwrap_or("north").to_owned();
        let orientation = match facing.as_str() {
            "down" => "down_south".to_owned(),
            "up" => "up_north".to_owned(),
            "north" | "south" | "west" | "east" => format!("{facing}_up"),
            _ => facing,
        };
        properties.remove("facing");
        properties.insert("orientation", orientation);
    }
    if fixer.due(2679, 0) && name == "minecraft:cauldron" {
        let empty = state
            .get_compound("Properties")
            .and_then(|p| p.get_str("level"))
            .is_none_or(|level| level == "0");
        if empty {
            state.remove("Properties");
        } else {
            state.insert("Name", "minecraft:water_cauldron");
        }
    }
}

fn redstone_connections(properties: &mut Compound) {
    let side =
        |properties: &Compound, key: &str| properties.get_str(key).unwrap_or("none").to_owned();
    let [east, west, north, south] =
        ["east", "west", "north", "south"].map(|k| side(properties, k));
    let connected = |value: &str| value != "none";
    let east_west = connected(&east) || connected(&west);
    let north_south = connected(&north) || connected(&south);
    let fixed = |own: &str, other_axis: bool| {
        if connected(own) || other_axis {
            own.to_owned()
        } else {
            "side".to_owned()
        }
    };
    properties.insert("east", fixed(&east, north_south));
    properties.insert("west", fixed(&west, north_south));
    properties.insert("north", fixed(&north, east_west));
    properties.insert("south", fixed(&south, east_west));
}

fn ceil_log2(value: usize) -> usize {
    if value <= 1 {
        0
    } else {
        (usize::BITS - (value - 1).leading_zeros()) as usize
    }
}

fn align_bit_storage(level: &mut Compound) {
    for_each_compound(level, "Sections", |section| {
        let palette_len = section.get_list("Palette").map_or(0, <[Tag]>::len);
        let bits = if palette_len == 0 {
            0
        } else {
            ceil_log2(palette_len).max(4)
        };
        if bits == 0 || bits.is_power_of_two() {
            return;
        }
        if let Some(Tag::LongArray(data)) = section.get_mut("BlockStates") {
            *data = add_padding(4096, bits, data);
        }
    });
    if let Some(heightmaps) = level.get_compound_mut("Heightmaps") {
        for (_, map) in heightmaps.iter_mut() {
            if let Tag::LongArray(data) = map {
                *data = add_padding(256, HEIGHTMAP_BITS, data);
            }
        }
    }
}

/// Repacks tightly packed entries so none straddles two longs.
fn add_padding(size: usize, bits: usize, data: &[i64]) -> Vec<i64> {
    if data.is_empty() {
        return Vec::new();
    }
    let mask = (1u64 << bits) - 1;
    let per_long = 64 / bits;
    let mut out = vec![0i64; size.div_ceil(per_long)];
    for index in 0..size {
        let start = index * bits;
        let (cell, shift) = (start >> 6, start & 63);
        let low = data.get(cell).copied().unwrap_or(0) as u64 >> shift;
        let value = if shift + bits > 64 {
            (low | (data.get(cell + 1).copied().unwrap_or(0) as u64) << (64 - shift)) & mask
        } else {
            low & mask
        };
        out[index / per_long] |= (value << ((index % per_long) * bits)) as i64;
    }
    out
}

fn container(palette: Vec<Tag>, data: Option<Vec<i64>>) -> Tag {
    let mut container = Compound::new().with("palette", palette);
    if let Some(data) = data {
        container.insert("data", data);
    }
    Tag::Compound(container)
}

fn air_state() -> Tag {
    Tag::Compound(Compound::new().with("Name", AIR))
}

fn air_container() -> Tag {
    container(vec![air_state()], None)
}

fn pack_aligned(indices: &[usize], bits: usize) -> Vec<i64> {
    let per_long = 64 / bits;
    let mut out = vec![0i64; indices.len().div_ceil(per_long)];
    for (i, &index) in indices.iter().enumerate() {
        out[i / per_long] |= (index as i64) << ((i % per_long) * bits);
    }
    out
}

fn biome_container(biome_at: impl Fn(usize) -> i32) -> Tag {
    let mut ids: Vec<i32> = Vec::new();
    let mut indices = Vec::with_capacity(64);
    for i in 0..64 {
        let id = biome_at(i);
        let slot = ids
            .iter()
            .position(|known| *known == id)
            .unwrap_or_else(|| {
                ids.push(id);
                ids.len() - 1
            });
        indices.push(slot);
    }
    let palette = ids.iter().map(|id| Tag::String(biome_name(*id))).collect();
    let bits = ceil_log2(ids.len());
    let data = (bits > 0).then(|| pack_aligned(&indices, bits));
    container(palette, data)
}

fn block_container(section: &mut Compound) -> Tag {
    let Some(Tag::List(mut palette)) = section.remove("Palette") else {
        section.remove("BlockStates");
        return air_container();
    };
    let Some(Tag::LongArray(data)) = section.remove("BlockStates") else {
        return air_container();
    };
    if palette.len() == 1 {
        return container(palette, None);
    }
    let estimated = data.len() * 64 / 4096;
    if estimated > ceil_log2(palette.len()) && estimated > 0 {
        let wanted = (1usize << (estimated - 1)) + 1;
        while palette.len() < wanted {
            palette.push(air_state());
        }
    }
    container(palette, Some(data))
}

fn height_and_biome(level: &mut Compound, overworld: bool) {
    let min_section = if overworld { NEW_MIN_SECTION } else { 0 };
    let count = if overworld {
        NEW_SECTIONS
    } else {
        OLD_SECTIONS
    };
    let old_biomes = level.get_int_array("Biomes").map(<[i32]>::to_vec);
    let mut was_increased = false;
    let mut biomes: Vec<Tag> = Vec::with_capacity(count);
    match old_biomes.as_deref() {
        Some(old) if old.len() == 1536 => {
            was_increased = true;
            biomes.extend((0..NEW_SECTIONS).map(|s| biome_container(|i| old[s * 64 + i])));
        }
        Some(old) if old.len() == 1024 => {
            let section = |s: usize| biome_container(|i| old[s * 64 + i]);
            if overworld {
                let below = biome_container(|i| old[i % 16]);
                let above = biome_container(|i| old[i % 16 + 1008]);
                biomes.extend(std::iter::repeat_n(below, 4));
                biomes.extend((0..OLD_SECTIONS).map(section));
                biomes.extend(std::iter::repeat_n(above, 4));
            } else {
                biomes.extend((0..OLD_SECTIONS).map(section));
            }
        }
        _ => biomes.extend(std::iter::repeat_n(
            container(vec![Tag::String(PLAINS.to_owned())], None),
            count,
        )),
    }
    let mut seen = std::collections::HashSet::new();
    let mut blocks_in_chunk = std::collections::HashSet::new();
    let mut bedrock_section: Option<Tag> = None;
    let sections = level.get_list_mut("Sections");
    let mut new_sections: Vec<Tag> = Vec::new();
    if let Some(sections) = sections {
        for tag in std::mem::take(sections) {
            let Tag::Compound(mut section) = tag else {
                continue;
            };
            let y = i32::from(section.get_byte("Y").unwrap_or(0));
            if let Some(palette) = section.get_list("Palette") {
                blocks_in_chunk.extend(palette.iter().map(|state| {
                    state
                        .as_compound()
                        .and_then(|c| c.get_str("Name"))
                        .unwrap_or(AIR)
                        .to_owned()
                }));
            }
            let has_y = section.contains_key("Y");
            let blocks = block_container(&mut section);
            if has_y && section.get_byte("Y") == Some(0) {
                bedrock_section = Some(blocks.clone());
            }
            let index = y - min_section;
            if let Some(biome) = usize::try_from(index).ok().and_then(|i| biomes.get(i)) {
                section.insert("biomes", biome.clone());
            }
            section.insert("block_states", blocks);
            seen.insert(y);
            new_sections.push(Tag::Compound(section));
        }
    }
    for (index, biome) in biomes.iter().enumerate() {
        let y = index as i32 + min_section;
        if seen.insert(y) {
            new_sections.push(Tag::Compound(
                Compound::new()
                    .with("Y", y as i8)
                    .with("block_states", air_container())
                    .with("biomes", biome.clone()),
            ));
        }
    }
    level.insert("Sections", new_sections);
    level.remove("Biomes");
    if overworld {
        predict_status_before_surface(level, blocks_in_chunk);
    }
    update_chunk_tag(level, overworld, was_increased, bedrock_section);
}

fn rename_status_1911(level: &mut Compound) {
    let status = level.get_str("Status").unwrap_or("empty");
    let renamed = match status {
        "structure_references" | "biomes" => "empty",
        "base" => "surface",
        "carved" => "carvers",
        "liquid_carved" => "liquid_carvers",
        "decorated" => "features",
        "lighted" => "light",
        "mobs_spawned" => "spawn",
        "finalized" => "heightmaps",
        "fullchunk" => "full",
        _ => "empty",
    };
    if renamed != status {
        level.insert("Status", renamed);
    }
}

fn predict_status_before_surface(
    level: &mut Compound,
    mut blocks: std::collections::HashSet<String>,
) {
    let status = level.get_str("Status").unwrap_or("empty").to_owned();
    if STATUS_AFTER_SURFACE.contains(&status.as_str()) {
        return;
    }
    blocks.remove(AIR);
    let has_non_air = !blocks.is_empty();
    blocks.retain(|name| !BLOCKS_BEFORE_FEATURES.contains(&name.as_str()));
    let predicted = if !blocks.is_empty() {
        "liquid_carvers"
    } else if status == "noise" || has_non_air {
        "noise"
    } else if status == "biomes" {
        "structure_references"
    } else {
        return;
    };
    level.insert("Status", predicted);
}

fn missing_bedrock_mask(container: &Tag) -> Option<(bool, [u64; 4])> {
    let container = container.as_compound()?;
    let palette = container.get_list("palette")?;
    let data = container.get_long_array("data").unwrap_or(&[]);
    let name_of = |index: usize| -> Option<&str> {
        palette
            .get(index)?
            .as_compound()
            .map(|c| c.get_str("Name").unwrap_or(""))
    };
    let bits = ceil_log2(palette.len()).max(4);
    let per_long = 64 / bits;
    let mask = (1u64 << bits) - 1;
    let mut missing = [0u64; 4];
    let mut any_bedrock = false;
    for z in 0..16usize {
        for x in 0..16usize {
            let name = match palette.len() {
                0 => None,
                1 => name_of(0),
                _ => {
                    let index = (z << 4) | x;
                    data.get(index / per_long).and_then(|cell| {
                        let shift = (index % per_long) * bits;
                        name_of(((*cell as u64 >> shift) & mask) as usize)
                    })
                }
            };
            if name == Some(AIR) {
                missing[z >> 2] |= 1 << (((z & 3) << 4) | x);
            }
            any_bedrock |= name == Some("minecraft:bedrock");
        }
    }
    Some((any_bedrock, missing))
}

fn below_zero_retrogen(level: &mut Compound, status: &str, bedrock_section: &Tag) {
    let Some((any_bedrock, missing)) = missing_bedrock_mask(bedrock_section) else {
        return;
    };
    let any_bedrock = any_bedrock || status == "noise";
    if any_bedrock && missing.iter().map(|w| w.count_ones()).sum::<u32>() != 256 {
        let last = missing.iter().rposition(|w| *w != 0).map_or(0, |i| i + 1);
        let target = if status == "full" {
            "heightmaps"
        } else {
            status
        };
        level.insert(
            "below_zero_retrogen",
            Compound::new().with("target_status", target).with(
                "missing_bedrock",
                missing[..last]
                    .iter()
                    .map(|w| *w as i64)
                    .collect::<Vec<_>>(),
            ),
        );
        level.insert("Status", "empty");
    }
    level.insert("isLightOn", false);
}

fn update_chunk_tag(
    level: &mut Compound,
    overworld: bool,
    was_increased: bool,
    bedrock_section: Option<Tag>,
) {
    if !overworld {
        carving_masks(level, OLD_SECTIONS, 0);
        return;
    }
    if was_increased {
        carving_masks(level, NEW_SECTIONS, 0);
        return;
    }
    if let Some(heightmaps) = level.get_compound_mut("Heightmaps") {
        for kind in HEIGHTMAP_TYPES {
            if let Some(Tag::LongArray(data)) = heightmaps.get_mut(kind) {
                for value in data.iter_mut() {
                    *value = raise_heightmap(*value);
                }
            }
        }
    }
    for key in ["LiquidsToBeTicked", "PostProcessing", "ToBeTicked"] {
        pad_entries(level, key);
    }
    carving_masks(level, NEW_SECTIONS, 4);
    if let Some(Tag::Compound(upgrade)) = level.get_mut("UpgradeData")
        && let Some(Tag::Compound(indices)) = upgrade.get_mut("Indices")
    {
        let shifted: Compound = std::mem::take(indices)
            .into_iter()
            .filter_map(|(key, value)| {
                let index: i32 = key.parse().ok()?;
                Some(((index - NEW_MIN_SECTION).to_string(), value))
            })
            .collect();
        *indices = shifted;
    }
    let status = level.get_str("Status").map(str::to_owned);
    if let Some(status) = status {
        let status = status.strip_prefix("minecraft:").unwrap_or(&status);
        if status != "empty" {
            let old_noise = STATUS_AFTER_NOISE.contains(&status);
            level.insert(
                "blending_data",
                Compound::new().with("old_noise", old_noise),
            );
            if let Some(section) = &bedrock_section {
                below_zero_retrogen(level, status, section);
            }
        }
    }
}

fn raise_heightmap(value: i64) -> i64 {
    let mut out = 0i64;
    let mut bit = 0;
    while bit + HEIGHTMAP_BITS <= 64 {
        let old = (value >> bit) & HEIGHTMAP_MASK;
        let new = if old == 0 {
            0
        } else {
            (old + 64).min(HEIGHTMAP_MASK)
        };
        out |= new << bit;
        bit += HEIGHTMAP_BITS;
    }
    out
}

fn pad_entries(level: &mut Compound, key: &str) {
    let mut list = match level.remove(key) {
        Some(Tag::List(list)) => list,
        _ => Vec::new(),
    };
    if list.len() != NEW_SECTIONS {
        for _ in 0..4 {
            list.insert(0, Tag::List(Vec::new()));
            list.push(Tag::List(Vec::new()));
        }
    }
    level.insert(key, list);
}

fn carving_masks(level: &mut Compound, sections: usize, added_below: usize) {
    let mut masks = level
        .get_compound("CarvingMasks")
        .cloned()
        .unwrap_or_default();
    for (_, mask) in masks.iter_mut() {
        let bytes: Vec<u8> = match mask {
            Tag::ByteArray(bytes) => bytes.iter().map(|b| *b as u8).collect(),
            _ => continue,
        };
        let mut words = vec![0i64; 64 * sections];
        for (i, chunk) in bytes.chunks(8).enumerate() {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            if let Some(slot) = words.get_mut(64 * added_below + i) {
                *slot = i64::from_le_bytes(word);
            }
        }
        *mask = Tag::LongArray(words);
    }
    level.insert("CarvingMasks", masks);
}

struct BlockView {
    palette: Vec<Compound>,
    data: Vec<i64>,
    bits: usize,
}

impl BlockView {
    fn of(section: &Compound) -> Option<Self> {
        let states = section.get_compound("block_states")?;
        let palette: Vec<Compound> = states
            .get_list("palette")?
            .iter()
            .filter_map(|s| s.as_compound().cloned())
            .collect();
        let data = states
            .get_long_array("data")
            .map(<[i64]>::to_vec)
            .unwrap_or_default();
        let bits = ceil_log2(palette.len()).max(4);
        Some(Self {
            palette,
            data,
            bits,
        })
    }

    fn at(&self, index: usize) -> Option<&Compound> {
        match self.palette.len() {
            0 => None,
            1 => self.palette.first(),
            len => {
                let per_long = 64 / self.bits;
                let cell = *self.data.get(index / per_long)?;
                let shift = (index % per_long) * self.bits;
                let slot = ((cell as u64 >> shift) & ((1u64 << self.bits) - 1)) as usize;
                (slot < len).then(|| &self.palette[slot])
            }
        }
    }
}

fn proto_ticks(level: &mut Compound) {
    if let Some(ticks) = level.remove("LiquidTicks") {
        level.insert("fluid_ticks", ticks);
    }
    let mut sections: std::collections::HashMap<i32, BlockView> = std::collections::HashMap::new();
    let mut lowest = 0i32;
    for tag in level.get_list("Sections").unwrap_or_default() {
        let Some(section) = tag.as_compound() else {
            continue;
        };
        let Some(y) = section.get_byte("Y").map(i32::from) else {
            continue;
        };
        if section.contains_key("biomes") {
            lowest = lowest.min(y);
        }
        if let Some(view) = BlockView::of(section) {
            sections.insert(y, view);
        }
    }
    level.insert("yPos", lowest);
    if level.contains_key("TileTicks") || level.contains_key("fluid_ticks") {
        return;
    }
    let (x, z) = (
        get_i32(level, "xPos").unwrap_or(0),
        get_i32(level, "zPos").unwrap_or(0),
    );
    let fluid = tick_list(
        level,
        "LiquidsToBeTicked",
        &sections,
        lowest,
        x,
        z,
        liquid_of,
    );
    let block = tick_list(level, "ToBeTicked", &sections, lowest, x, z, block_of);
    level.insert("TileTicks", block);
    level.insert("fluid_ticks", fluid);
    level.remove("ToBeTicked");
    level.remove("LiquidsToBeTicked");
}

fn block_of(state: Option<&Compound>) -> String {
    state
        .and_then(|s| s.get_str("Name"))
        .unwrap_or(AIR)
        .to_owned()
}

fn liquid_of(state: Option<&Compound>) -> String {
    let Some(state) = state else {
        return "minecraft:empty".into();
    };
    let name = state.get_str("Name").unwrap_or("");
    let properties = state.get_compound("Properties");
    let level = || {
        properties
            .and_then(|p| p.get_str("level"))
            .and_then(|l| l.parse::<i32>().ok())
            .unwrap_or(0)
    };
    match name {
        "minecraft:water" => if level() == 0 {
            "minecraft:water"
        } else {
            "minecraft:flowing_water"
        }
        .into(),
        "minecraft:lava" => if level() == 0 {
            "minecraft:lava"
        } else {
            "minecraft:flowing_lava"
        }
        .into(),
        _ if ALWAYS_WATERLOGGED.contains(&name)
            || properties.and_then(|p| p.get_str("waterlogged")) == Some("true") =>
        {
            "minecraft:water".into()
        }
        _ => "minecraft:empty".into(),
    }
}

fn tick_list(
    level: &Compound,
    key: &str,
    sections: &std::collections::HashMap<i32, BlockView>,
    lowest: i32,
    chunk_x: i32,
    chunk_z: i32,
    type_of: fn(Option<&Compound>) -> String,
) -> Tag {
    let mut out = Vec::new();
    for (slot, positions) in level.get_list(key).unwrap_or_default().iter().enumerate() {
        let section_y = slot as i32 + lowest;
        let view = sections.get(&section_y);
        let Tag::List(positions) = positions else {
            continue;
        };
        for pos in positions {
            let Some(pos) = pos.as_short().map(i32::from).filter(|p| *p > 0) else {
                continue;
            };
            let (rx, ry, rz) = (pos & 15, (pos >> 4) & 15, (pos >> 8) & 15);
            let state = view.and_then(|v| v.at(((ry << 4 | rz) << 4 | rx) as usize));
            out.push(Tag::Compound(
                Compound::new()
                    .with("i", type_of(state))
                    .with("x", chunk_x * 16 + rx)
                    .with("y", section_y * 16 + ry)
                    .with("z", chunk_z * 16 + rz)
                    .with("t", 0)
                    .with("p", 0),
            ));
        }
    }
    Tag::List(out)
}

fn overreaching_ticks(root: &mut Compound) {
    let (chunk_x, chunk_z) = (
        get_i32(root, "xPos").unwrap_or(0),
        get_i32(root, "zPos").unwrap_or(0),
    );
    for (key, target) in [
        ("block_ticks", "neighbor_block_ticks"),
        ("fluid_ticks", "neighbor_fluid_ticks"),
    ] {
        let overreaching: Vec<Tag> = root
            .get_list(key)
            .unwrap_or_default()
            .iter()
            .filter(|tick| {
                let Some(tick) = tick.as_compound() else {
                    return false;
                };
                let dx = (chunk_x - (get_i32(tick, "x").unwrap_or(0) >> 4)).abs();
                let dz = (chunk_z - (get_i32(tick, "z").unwrap_or(0) >> 4)).abs();
                !(dx == 0 && dz == 0) && dx <= 1 && dz <= 1
            })
            .cloned()
            .collect();
        if overreaching.is_empty() {
            continue;
        }
        if !root.contains_key("UpgradeData") {
            root.insert("UpgradeData", Compound::new());
        }
        if let Some(upgrade) = root.get_compound_mut("UpgradeData") {
            upgrade.insert(target, overreaching);
        }
    }
}
