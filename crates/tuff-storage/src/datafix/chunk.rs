//! Region chunks (1.18 layout and later) and the block states inside them.

use super::{Fixer, block_entity, entity, for_each_compound, get_bool, id_of, item, rename};
use tuff_nbt::{Compound, Tag};

const STATUS_REMAPS_3450: [(&str, &str); 2] = [
    ("minecraft:liquid_carvers", "minecraft:carvers"),
    ("minecraft:heightmaps", "minecraft:spawn"),
];
const BIOME_RENAMES_2838: [(&str, &str); 38] = [
    ("minecraft:badlands_plateau", "minecraft:badlands"),
    ("minecraft:bamboo_jungle_hills", "minecraft:bamboo_jungle"),
    ("minecraft:birch_forest_hills", "minecraft:birch_forest"),
    ("minecraft:dark_forest_hills", "minecraft:dark_forest"),
    ("minecraft:desert_hills", "minecraft:desert"),
    ("minecraft:desert_lakes", "minecraft:desert"),
    (
        "minecraft:giant_spruce_taiga_hills",
        "minecraft:old_growth_spruce_taiga",
    ),
    (
        "minecraft:giant_spruce_taiga",
        "minecraft:old_growth_spruce_taiga",
    ),
    (
        "minecraft:giant_tree_taiga_hills",
        "minecraft:old_growth_pine_taiga",
    ),
    (
        "minecraft:giant_tree_taiga",
        "minecraft:old_growth_pine_taiga",
    ),
    (
        "minecraft:gravelly_mountains",
        "minecraft:windswept_gravelly_hills",
    ),
    ("minecraft:jungle_edge", "minecraft:sparse_jungle"),
    ("minecraft:jungle_hills", "minecraft:jungle"),
    ("minecraft:modified_badlands_plateau", "minecraft:badlands"),
    (
        "minecraft:modified_gravelly_mountains",
        "minecraft:windswept_gravelly_hills",
    ),
    ("minecraft:modified_jungle_edge", "minecraft:sparse_jungle"),
    ("minecraft:modified_jungle", "minecraft:jungle"),
    (
        "minecraft:modified_wooded_badlands_plateau",
        "minecraft:wooded_badlands",
    ),
    ("minecraft:mountain_edge", "minecraft:windswept_hills"),
    ("minecraft:mountains", "minecraft:windswept_hills"),
    (
        "minecraft:mushroom_field_shore",
        "minecraft:mushroom_fields",
    ),
    ("minecraft:shattered_savanna", "minecraft:windswept_savanna"),
    (
        "minecraft:shattered_savanna_plateau",
        "minecraft:windswept_savanna",
    ),
    ("minecraft:snowy_mountains", "minecraft:snowy_plains"),
    ("minecraft:snowy_taiga_hills", "minecraft:snowy_taiga"),
    ("minecraft:snowy_taiga_mountains", "minecraft:snowy_taiga"),
    ("minecraft:snowy_tundra", "minecraft:snowy_plains"),
    ("minecraft:stone_shore", "minecraft:stony_shore"),
    ("minecraft:swamp_hills", "minecraft:swamp"),
    ("minecraft:taiga_hills", "minecraft:taiga"),
    ("minecraft:taiga_mountains", "minecraft:taiga"),
    (
        "minecraft:tall_birch_forest",
        "minecraft:old_growth_birch_forest",
    ),
    (
        "minecraft:tall_birch_hills",
        "minecraft:old_growth_birch_forest",
    ),
    (
        "minecraft:wooded_badlands_plateau",
        "minecraft:wooded_badlands",
    ),
    ("minecraft:wooded_hills", "minecraft:forest"),
    ("minecraft:wooded_mountains", "minecraft:windswept_forest"),
    ("minecraft:lofty_peaks", "minecraft:jagged_peaks"),
    ("minecraft:snowcapped_peaks", "minecraft:frozen_peaks"),
];
const OLD_INTERMEDIATE_TERRAIN: [&str; 2] = ["minecraft:noise", "minecraft:surface"];

fn structure_references_at_least_one(root: &mut Compound) {
    let Some(starts) = root
        .get_compound_mut("structures")
        .and_then(|s| s.get_compound_mut("starts"))
    else {
        return;
    };
    for (_, start) in starts.iter_mut() {
        if let Some(start) = start.as_compound_mut()
            && let Some(count) = start.get("references").and_then(super::number)
        {
            start.insert("references", (count as i32).max(1));
        }
    }
}

/// Upgrades a chunk of the 1.18 layout or later. Only overworld chunks keep
/// blending data, which marks them as old terrain to blend new terrain into.
pub fn upgrade(root: &mut Compound, fixer: Fixer, overworld: bool) {
    for_each_compound(root, "sections", |section| {
        if let Some(states) = section.get_compound_mut("block_states")
            && let Some(palette) = states.get_list_mut("palette")
        {
            for state in palette {
                fix_block_state(state, fixer);
            }
        }
    });
    for key in ["block_ticks", "fluid_ticks"] {
        for_each_compound(root, key, |tick| {
            if let Some(Tag::String(name)) = tick.get_mut("i") {
                item::rename_block_name(name, fixer);
            }
        });
    }
    if let Some(inner) = fixer.nested() {
        for_each_compound(root, "block_entities", |be| {
            block_entity::upgrade(be, inner)
        });
        for_each_compound(root, "entities", |e| entity::upgrade(e, inner));
    }
    if fixer.due(2838, 0) {
        rename_biomes(root, |name| {
            BIOME_RENAMES_2838
                .iter()
                .find(|(old, _)| *old == name)
                .map(|(_, new)| *new)
        });
    }
    if fixer.due(2843, 0) {
        rename_biomes(root, |name| {
            (name == "minecraft:deep_warm_ocean").then_some("minecraft:warm_ocean")
        });
    }
    if fixer.due(2211, 0) {
        structure_references_at_least_one(root);
    }
    if fixer.due(2970, 0) {
        super::structures::upgrade(root);
    }
    if fixer.due(3077, 0) && !get_bool(root, "isLightOn").unwrap_or(false) {
        delete_section_light(root);
    }
    if fixer.due(3108, 0) && !overworld {
        root.remove("blending_data");
    }
    if fixer.due(3450, 0) {
        remap_statuses(root, |status| {
            STATUS_REMAPS_3450
                .iter()
                .find(|(old, _)| *old == status)
                .map(|(_, new)| (*new).to_owned())
        });
    }
    if fixer.due(3451, 0) || fixer.due(4537, 0) {
        root.remove("isLightOn");
        delete_section_light(root);
    }
    if fixer.due(4885, 0)
        && let Some(list) = root.get_list_mut("block_entities")
    {
        list.retain(|be| {
            be.as_compound()
                .is_none_or(|be| id_of(be) != "minecraft:bed")
        });
    }
    if fixer.due(4997, 0) {
        reset_blending_data(root, overworld);
    }
    if fixer.due(5009, 0)
        && let Some(blending) = root.get_compound_mut("blending_data")
        && let Some(heights) = blending.get_list_mut("heights")
    {
        for height in heights {
            let value = super::number(height).unwrap_or(f64::MAX);
            *height = Tag::Float(if value == f64::MAX {
                f32::MAX
            } else {
                value as f32
            });
        }
    }
    if fixer.due(5013, 0) {
        merge_terrain_statuses(root);
    }
}

const STATUSES_WITHOUT_BLENDING: [&str; 4] = [
    "minecraft:empty",
    "minecraft:structure_starts",
    "minecraft:structure_references",
    "minecraft:biomes",
];

/// Old overworld terrain is blended with new terrain across the full height
/// of the world, or only the pre-1.18 height when just its below-zero part
/// still has to generate.
fn rename_biomes(root: &mut Compound, renamed: impl Fn(&str) -> Option<&'static str>) {
    for_each_compound(root, "sections", |section| {
        let Some(palette) = section
            .get_compound_mut("biomes")
            .and_then(|b| b.get_list_mut("palette"))
        else {
            return;
        };
        for biome in palette {
            if let Tag::String(name) = biome
                && let Some(new) = renamed(name)
            {
                *name = new.to_owned();
            }
        }
    });
}

fn reset_blending_data(root: &mut Compound, overworld: bool) {
    root.remove("blending_data");
    if !overworld || !root.contains_key("Status") {
        return;
    }
    let blends =
        |status: Option<&Tag>| !STATUSES_WITHOUT_BLENDING.contains(&status_of(status).as_str());
    let (min_y, height) = if blends(root.get("Status")) {
        (-64, 384)
    } else if let Some(retrogen) = root.get_compound("below_zero_retrogen")
        && blends(retrogen.get("target_status"))
    {
        (0, 256)
    } else {
        return;
    };
    root.insert(
        "blending_data",
        Compound::new()
            .with("min_section", min_y >> 4)
            .with("max_section", (min_y + height) >> 4),
    );
}

fn status_of(tag: Option<&Tag>) -> String {
    super::namespaced(tag.and_then(Tag::as_str).unwrap_or("minecraft:empty"))
}

fn remap_statuses(root: &mut Compound, remap: impl Fn(&str) -> Option<String>) {
    if let Some(Tag::String(status)) = root.get_mut("Status")
        && let Some(new) = remap(&super::namespaced(status))
    {
        *status = new;
    }
    if let Some(retrogen) = root.get_compound_mut("below_zero_retrogen")
        && let Some(Tag::String(status)) = retrogen.get_mut("target_status")
        && let Some(new) = remap(&super::namespaced(status))
    {
        *status = new;
    }
}

fn merged_status(status: &str) -> String {
    if status == "minecraft:carvers" {
        "minecraft:terrain".into()
    } else if OLD_INTERMEDIATE_TERRAIN.contains(&status) {
        "minecraft:biomes".into()
    } else {
        status.into()
    }
}

fn merge_terrain_statuses(root: &mut Compound) {
    let old = status_of(root.get("Status"));
    root.insert("Status", merged_status(&old));
    let mut old_target = None;
    if let Some(retrogen) = root.get_compound_mut("below_zero_retrogen") {
        let target = status_of(retrogen.get("target_status"));
        retrogen.insert("target_status", merged_status(&target));
        old_target = Some(target);
    }
    let intermediate = OLD_INTERMEDIATE_TERRAIN.contains(&old.as_str());
    let target_intermediate = old_target
        .as_deref()
        .is_some_and(|t| OLD_INTERMEDIATE_TERRAIN.contains(&t));
    if intermediate || target_intermediate {
        let only_below_zero = old_target.is_some() && !target_intermediate;
        root.remove("Heightmaps");
        root.remove("blending_data");
        for_each_compound(root, "sections", |section| {
            let y = section.get_byte("Y").unwrap_or(0);
            if !(y >= 0 && only_below_zero) {
                section.remove("block_states");
            }
        });
    }
}

fn delete_section_light(root: &mut Compound) {
    for_each_compound(root, "sections", |section| {
        section.remove("BlockLight");
        section.remove("SkyLight");
    });
}

/// Upgrades one block state: a palette entry, a flat id, or a saved
/// `{Name, Properties}` compound anywhere else.
pub(crate) fn fix_block_state(tag: &mut Tag, fixer: Fixer) {
    match tag {
        Tag::String(name) => item::rename_block_name(name, fixer),
        Tag::Compound(state) => {
            for key in ["Name", "id"] {
                if let Some(Tag::String(name)) = state.get_mut(key) {
                    item::rename_block_name(name, fixer);
                }
            }
            let name = state
                .get_str("Name")
                .or_else(|| state.get_str("id"))
                .map(super::namespaced)
                .unwrap_or_default();
            if let Some(properties) = state.get_compound_mut("Properties") {
                fix_properties(&name, properties, fixer);
            }
            if fixer.due(5006, 0) {
                rename(state, "Name", "id");
                rename(state, "Properties", "properties");
            }
        }
        _ => {}
    }
}

fn fix_properties(block: &str, properties: &mut Compound, fixer: Fixer) {
    if fixer.due(4294, 0)
        && block == "minecraft:creaking_heart"
        && let Some(active) = properties.remove("active")
    {
        let state = if active.as_str() == Some("true") {
            "awake"
        } else {
            "uprooted"
        };
        properties.insert("creaking_heart_state", state);
    }
    if fixer.due(4305, 0) && block == "minecraft:test_block" {
        rename(properties, "test_block_mode", "mode");
    }
}
