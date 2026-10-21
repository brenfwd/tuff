//! Item stacks: the legacy `{id, Count, tag}` form became `{id, count,
//! components}` in 1.20.5, and components kept changing after that. Items
//! nested inside an item (containers, bundles, projectiles, entity and block
//! entity data) are upgraded first, then the item itself.

use tuff_vanilla_data::Block;

use super::{
    Fixer, block_entity, dye_color_name, entity, for_each_compound, get_bool, get_i32, get_number,
    id_of, namespaced, rename, rename_id, renamed, text,
};
use tuff_nbt::{Compound, Tag};

const COMPONENTS: &str = "components";

pub(crate) const SHARD_RENAMES: &[(&str, &str)] = &[
    (
        "minecraft:pottery_shard_archer",
        "minecraft:archer_pottery_shard",
    ),
    (
        "minecraft:pottery_shard_prize",
        "minecraft:prize_pottery_shard",
    ),
    (
        "minecraft:pottery_shard_arms_up",
        "minecraft:arms_up_pottery_shard",
    ),
    (
        "minecraft:pottery_shard_skull",
        "minecraft:skull_pottery_shard",
    ),
];
const SHERD_NAMES: [&str; 20] = [
    "angler",
    "archer",
    "arms_up",
    "blade",
    "brewer",
    "burn",
    "danger",
    "explorer",
    "friend",
    "heart",
    "heartbreak",
    "howl",
    "miner",
    "mourner",
    "plenty",
    "prize",
    "sheaf",
    "shelter",
    "skull",
    "snort",
];
pub(crate) const GRASS_RENAMES: &[(&str, &str)] = &[("minecraft:grass", "minecraft:short_grass")];
const SCUTE_RENAMES: &[(&str, &str)] = &[("minecraft:scute", "minecraft:turtle_scute")];
const FUNGI: &[(&str, &str)] = &[
    ("minecraft:warped_fungi", "minecraft:warped_fungus"),
    ("minecraft:crimson_fungi", "minecraft:crimson_fungus"),
];
const SOUL_ITEMS: &[(&str, &str)] = &[
    ("minecraft:soul_fire_torch", "minecraft:soul_torch"),
    ("minecraft:soul_fire_lantern", "minecraft:soul_lantern"),
];
const SOUL_BLOCKS: &[(&str, &str)] = &[
    ("minecraft:soul_fire_torch", "minecraft:soul_torch"),
    (
        "minecraft:soul_fire_wall_torch",
        "minecraft:soul_wall_torch",
    ),
    ("minecraft:soul_fire_lantern", "minecraft:soul_lantern"),
];
const GRIMSTONE: &[(&str, &str)] = &[
    ("minecraft:grimstone", "minecraft:deepslate"),
    (
        "minecraft:grimstone_slab",
        "minecraft:cobbled_deepslate_slab",
    ),
    (
        "minecraft:grimstone_stairs",
        "minecraft:cobbled_deepslate_stairs",
    ),
    (
        "minecraft:grimstone_wall",
        "minecraft:cobbled_deepslate_wall",
    ),
    (
        "minecraft:polished_grimstone",
        "minecraft:polished_deepslate",
    ),
    (
        "minecraft:polished_grimstone_slab",
        "minecraft:polished_deepslate_slab",
    ),
    (
        "minecraft:polished_grimstone_stairs",
        "minecraft:polished_deepslate_stairs",
    ),
    (
        "minecraft:polished_grimstone_wall",
        "minecraft:polished_deepslate_wall",
    ),
    ("minecraft:grimstone_tiles", "minecraft:deepslate_tiles"),
    (
        "minecraft:grimstone_tile_slab",
        "minecraft:deepslate_tile_slab",
    ),
    (
        "minecraft:grimstone_tile_stairs",
        "minecraft:deepslate_tile_stairs",
    ),
    (
        "minecraft:grimstone_tile_wall",
        "minecraft:deepslate_tile_wall",
    ),
    ("minecraft:grimstone_bricks", "minecraft:deepslate_bricks"),
    (
        "minecraft:grimstone_brick_slab",
        "minecraft:deepslate_brick_slab",
    ),
    (
        "minecraft:grimstone_brick_stairs",
        "minecraft:deepslate_brick_stairs",
    ),
    (
        "minecraft:grimstone_brick_wall",
        "minecraft:deepslate_brick_wall",
    ),
    (
        "minecraft:chiseled_grimstone",
        "minecraft:chiseled_deepslate",
    ),
];
const AZALEA: &[(&str, &str)] = &[(
    "minecraft:azalea_leaves_flowers",
    "minecraft:flowering_azalea_leaves",
)];
const PIGMAN_EGG: &[(&str, &str)] = &[(
    "minecraft:zombie_pigman_spawn_egg",
    "minecraft:zombified_piglin_spawn_egg",
)];
const DYES: &[(&str, &str)] = &[
    ("minecraft:cactus_green", "minecraft:green_dye"),
    ("minecraft:rose_red", "minecraft:red_dye"),
    ("minecraft:dandelion_yellow", "minecraft:yellow_dye"),
];
const SIGNS_AND_SLABS: &[(&str, &str)] = &[
    ("minecraft:stone_slab", "minecraft:smooth_stone_slab"),
    ("minecraft:sign", "minecraft:oak_sign"),
];
const BEEHIVE: &[(&str, &str)] = &[("minecraft:bee_hive", "minecraft:beehive")];
const LEGACY_ITEM_RENAMES: &[(i32, &[(&str, &str)])] = &[
    (1800, DYES),
    (1802, SIGNS_AND_SLABS),
    (
        1928,
        &[(
            "minecraft:illager_beast_spawn_egg",
            "minecraft:ravager_spawn_egg",
        )],
    ),
    (2209, BEEHIVE),
    (2508, FUNGI),
    (2509, PIGMAN_EGG),
    (2528, SOUL_ITEMS),
    (2680, &[("minecraft:grass_path", "minecraft:dirt_path")]),
    (2690, COPPER_RENAMES_2690),
    (2691, COPPER_RENAMES_2691),
    (2696, GRIMSTONE),
    (2717, AZALEA),
];
const LEGACY_BLOCK_RENAMES: &[(i32, &[(&str, &str)])] = &[
    (
        1802,
        &[
            ("minecraft:stone_slab", "minecraft:smooth_stone_slab"),
            ("minecraft:sign", "minecraft:oak_sign"),
            ("minecraft:wall_sign", "minecraft:oak_wall_sign"),
        ],
    ),
    (2209, BEEHIVE),
    (2508, FUNGI),
    (2528, SOUL_BLOCKS),
    (2680, &[("minecraft:grass_path", "minecraft:dirt_path")]),
    (2690, COPPER_RENAMES_2690),
    (2691, COPPER_RENAMES_2691),
    (2696, GRIMSTONE),
    (
        2700,
        &[
            ("minecraft:cave_vines_head", "minecraft:cave_vines"),
            ("minecraft:cave_vines_body", "minecraft:cave_vines_plant"),
        ],
    ),
    (2717, AZALEA),
];

const COPPER_RENAMES_2690: &[(&str, &str)] = &[
    (
        "minecraft:weathered_copper_block",
        "minecraft:oxidized_copper_block",
    ),
    (
        "minecraft:semi_weathered_copper_block",
        "minecraft:weathered_copper_block",
    ),
    (
        "minecraft:lightly_weathered_copper_block",
        "minecraft:exposed_copper_block",
    ),
    (
        "minecraft:weathered_cut_copper",
        "minecraft:oxidized_cut_copper",
    ),
    (
        "minecraft:semi_weathered_cut_copper",
        "minecraft:weathered_cut_copper",
    ),
    (
        "minecraft:lightly_weathered_cut_copper",
        "minecraft:exposed_cut_copper",
    ),
    (
        "minecraft:weathered_cut_copper_stairs",
        "minecraft:oxidized_cut_copper_stairs",
    ),
    (
        "minecraft:semi_weathered_cut_copper_stairs",
        "minecraft:weathered_cut_copper_stairs",
    ),
    (
        "minecraft:lightly_weathered_cut_copper_stairs",
        "minecraft:exposed_cut_copper_stairs",
    ),
    (
        "minecraft:weathered_cut_copper_slab",
        "minecraft:oxidized_cut_copper_slab",
    ),
    (
        "minecraft:semi_weathered_cut_copper_slab",
        "minecraft:weathered_cut_copper_slab",
    ),
    (
        "minecraft:lightly_weathered_cut_copper_slab",
        "minecraft:exposed_cut_copper_slab",
    ),
    (
        "minecraft:waxed_semi_weathered_copper",
        "minecraft:waxed_weathered_copper",
    ),
    (
        "minecraft:waxed_lightly_weathered_copper",
        "minecraft:waxed_exposed_copper",
    ),
    (
        "minecraft:waxed_semi_weathered_cut_copper",
        "minecraft:waxed_weathered_cut_copper",
    ),
    (
        "minecraft:waxed_lightly_weathered_cut_copper",
        "minecraft:waxed_exposed_cut_copper",
    ),
    (
        "minecraft:waxed_semi_weathered_cut_copper_stairs",
        "minecraft:waxed_weathered_cut_copper_stairs",
    ),
    (
        "minecraft:waxed_lightly_weathered_cut_copper_stairs",
        "minecraft:waxed_exposed_cut_copper_stairs",
    ),
    (
        "minecraft:waxed_semi_weathered_cut_copper_slab",
        "minecraft:waxed_weathered_cut_copper_slab",
    ),
    (
        "minecraft:waxed_lightly_weathered_cut_copper_slab",
        "minecraft:waxed_exposed_cut_copper_slab",
    ),
];

const COPPER_RENAMES_2691: &[(&str, &str)] = &[
    ("minecraft:waxed_copper", "minecraft:waxed_copper_block"),
    (
        "minecraft:oxidized_copper_block",
        "minecraft:oxidized_copper",
    ),
    (
        "minecraft:weathered_copper_block",
        "minecraft:weathered_copper",
    ),
    ("minecraft:exposed_copper_block", "minecraft:exposed_copper"),
];

pub(crate) const CHAIN_RENAMES: &[(&str, &str)] = &[("minecraft:chain", "minecraft:iron_chain")];
const MAP_RENAMES: &[(&str, &str)] = &[
    (
        "minecraft:ocean_explorer_map",
        "minecraft:ocean_monument_map",
    ),
    ("minecraft:swamp_explorer_map", "minecraft:swamp_hut_map"),
    (
        "minecraft:trial_explorer_map",
        "minecraft:buried_trial_chambers_map",
    ),
    (
        "minecraft:woodland_explorer_map",
        "minecraft:woodland_mansion_map",
    ),
    (
        "minecraft:jungle_explorer_map",
        "minecraft:jungle_pyramid_map",
    ),
];
const ENCHANTMENT_RENAMES: &[(&str, &str)] = &[("minecraft:sweeping", "minecraft:sweeping_edge")];
pub(crate) const JUMP_STRENGTH_RENAMES: &[(&str, &str)] = &[(
    "minecraft:horse.jump_strength",
    "minecraft:generic.jump_strength",
)];
const POTION_HOLDERS: [&str; 4] = [
    "minecraft:potion",
    "minecraft:splash_potion",
    "minecraft:lingering_potion",
    "minecraft:tipped_arrow",
];
const BUCKETED_MOBS: [&str; 6] = [
    "minecraft:pufferfish_bucket",
    "minecraft:salmon_bucket",
    "minecraft:cod_bucket",
    "minecraft:tropical_fish_bucket",
    "minecraft:axolotl_bucket",
    "minecraft:tadpole_bucket",
];
const BUCKETED_MOB_TAGS: [&str; 10] = [
    "NoAI",
    "Silent",
    "NoGravity",
    "Glowing",
    "Invulnerable",
    "Health",
    "Age",
    "Variant",
    "HuntingCooldown",
    "BucketVariantTag",
];
const BOOLEAN_STATE_PROPERTIES: [&str; 47] = [
    "attached",
    "bottom",
    "conditional",
    "disarmed",
    "drag",
    "enabled",
    "extended",
    "eye",
    "falling",
    "hanging",
    "has_bottle_0",
    "has_bottle_1",
    "has_bottle_2",
    "has_record",
    "has_book",
    "inverted",
    "in_wall",
    "lit",
    "locked",
    "occupied",
    "open",
    "persistent",
    "powered",
    "short",
    "signal_fire",
    "snowy",
    "triggered",
    "unstable",
    "waterlogged",
    "berries",
    "bloom",
    "shrieking",
    "can_summon",
    "up",
    "down",
    "north",
    "east",
    "south",
    "west",
    "slot_0_occupied",
    "slot_1_occupied",
    "slot_2_occupied",
    "slot_3_occupied",
    "slot_4_occupied",
    "slot_5_occupied",
    "cracked",
    "crafting",
];
const MAP_NAMES: [&str; 11] = [
    "filled_map.buried_treasure",
    "filled_map.explorer_jungle",
    "filled_map.explorer_swamp",
    "filled_map.mansion",
    "filled_map.monument",
    "filled_map.trial_chambers",
    "filled_map.village_desert",
    "filled_map.village_plains",
    "filled_map.village_savanna",
    "filled_map.village_snowy",
    "filled_map.village_taiga",
];
const ADDITIONAL_TOOLTIP_COMPONENTS: [&str; 17] = [
    "minecraft:banner_patterns",
    "minecraft:bees",
    "minecraft:block_entity_data",
    "minecraft:block_state",
    "minecraft:bundle_contents",
    "minecraft:charged_projectiles",
    "minecraft:container",
    "minecraft:container_loot",
    "minecraft:firework_explosion",
    "minecraft:fireworks",
    "minecraft:instrument",
    "minecraft:map_id",
    "minecraft:painting/variant",
    "minecraft:pot_decorations",
    "minecraft:potion_contents",
    "minecraft:tropical_fish/pattern",
    "minecraft:written_book_content",
];
/// `(decoration type, item, legacy name key)` of explorer maps.
const EXPLORER_MAPS: [(&str, &str, &str); 11] = [
    (
        "minecraft:mansion",
        "minecraft:woodland_explorer_map",
        "filled_map.mansion",
    ),
    (
        "minecraft:monument",
        "minecraft:ocean_explorer_map",
        "filled_map.monument",
    ),
    (
        "minecraft:trial_chambers",
        "minecraft:trial_explorer_map",
        "filled_map.trial_chambers",
    ),
    (
        "minecraft:jungle_temple",
        "minecraft:jungle_explorer_map",
        "filled_map.explorer_jungle",
    ),
    (
        "minecraft:swamp_hut",
        "minecraft:swamp_explorer_map",
        "filled_map.explorer_swamp",
    ),
    (
        "minecraft:village_desert",
        "minecraft:desert_village_map",
        "filled_map.village_desert",
    ),
    (
        "minecraft:village_plains",
        "minecraft:plains_village_map",
        "filled_map.village_plains",
    ),
    (
        "minecraft:village_savanna",
        "minecraft:savanna_village_map",
        "filled_map.village_savanna",
    ),
    (
        "minecraft:village_snowy",
        "minecraft:snowy_village_map",
        "filled_map.village_snowy",
    ),
    (
        "minecraft:village_taiga",
        "minecraft:taiga_village_map",
        "filled_map.village_taiga",
    ),
    (
        "minecraft:red_x",
        "minecraft:buried_treasure_map",
        "filled_map.buried_treasure",
    ),
];

/// Upgrades the item at `key`, if present.
const GOAT_HORNS: [&str; 8] = [
    "minecraft:ponder_goat_horn",
    "minecraft:sing_goat_horn",
    "minecraft:seek_goat_horn",
    "minecraft:feel_goat_horn",
    "minecraft:admire_goat_horn",
    "minecraft:call_goat_horn",
    "minecraft:yearn_goat_horn",
    "minecraft:dream_goat_horn",
];

const PIG_EGG_ENTITIES: &[(&str, &str)] = &[
    ("minecraft:bat", "minecraft:bat_spawn_egg"),
    ("minecraft:blaze", "minecraft:blaze_spawn_egg"),
    ("minecraft:cave_spider", "minecraft:cave_spider_spawn_egg"),
    ("minecraft:chicken", "minecraft:chicken_spawn_egg"),
    ("minecraft:cow", "minecraft:cow_spawn_egg"),
    ("minecraft:creeper", "minecraft:creeper_spawn_egg"),
    ("minecraft:donkey", "minecraft:donkey_spawn_egg"),
    (
        "minecraft:elder_guardian",
        "minecraft:elder_guardian_spawn_egg",
    ),
    ("minecraft:ender_dragon", "minecraft:ender_dragon_spawn_egg"),
    ("minecraft:enderman", "minecraft:enderman_spawn_egg"),
    ("minecraft:endermite", "minecraft:endermite_spawn_egg"),
    (
        "minecraft:evocation_illager",
        "minecraft:evocation_illager_spawn_egg",
    ),
    ("minecraft:ghast", "minecraft:ghast_spawn_egg"),
    ("minecraft:guardian", "minecraft:guardian_spawn_egg"),
    ("minecraft:horse", "minecraft:horse_spawn_egg"),
    ("minecraft:husk", "minecraft:husk_spawn_egg"),
    ("minecraft:iron_golem", "minecraft:iron_golem_spawn_egg"),
    ("minecraft:llama", "minecraft:llama_spawn_egg"),
    ("minecraft:magma_cube", "minecraft:magma_cube_spawn_egg"),
    ("minecraft:mooshroom", "minecraft:mooshroom_spawn_egg"),
    ("minecraft:mule", "minecraft:mule_spawn_egg"),
    ("minecraft:ocelot", "minecraft:ocelot_spawn_egg"),
    ("minecraft:pufferfish", "minecraft:pufferfish_spawn_egg"),
    ("minecraft:parrot", "minecraft:parrot_spawn_egg"),
    ("minecraft:pig", "minecraft:pig_spawn_egg"),
    ("minecraft:polar_bear", "minecraft:polar_bear_spawn_egg"),
    ("minecraft:rabbit", "minecraft:rabbit_spawn_egg"),
    ("minecraft:sheep", "minecraft:sheep_spawn_egg"),
    ("minecraft:shulker", "minecraft:shulker_spawn_egg"),
    ("minecraft:silverfish", "minecraft:silverfish_spawn_egg"),
    ("minecraft:skeleton", "minecraft:skeleton_spawn_egg"),
    (
        "minecraft:skeleton_horse",
        "minecraft:skeleton_horse_spawn_egg",
    ),
    ("minecraft:slime", "minecraft:slime_spawn_egg"),
    ("minecraft:snow_golem", "minecraft:snow_golem_spawn_egg"),
    ("minecraft:spider", "minecraft:spider_spawn_egg"),
    ("minecraft:squid", "minecraft:squid_spawn_egg"),
    ("minecraft:stray", "minecraft:stray_spawn_egg"),
    ("minecraft:turtle", "minecraft:turtle_spawn_egg"),
    ("minecraft:vex", "minecraft:vex_spawn_egg"),
    ("minecraft:villager", "minecraft:villager_spawn_egg"),
    (
        "minecraft:vindication_illager",
        "minecraft:vindication_illager_spawn_egg",
    ),
    ("minecraft:witch", "minecraft:witch_spawn_egg"),
    ("minecraft:wither", "minecraft:wither_spawn_egg"),
    (
        "minecraft:wither_skeleton",
        "minecraft:wither_skeleton_spawn_egg",
    ),
    ("minecraft:wolf", "minecraft:wolf_spawn_egg"),
    ("minecraft:zombie", "minecraft:zombie_spawn_egg"),
    ("minecraft:zombie_horse", "minecraft:zombie_horse_spawn_egg"),
    (
        "minecraft:zombie_pigman",
        "minecraft:zombie_pigman_spawn_egg",
    ),
    (
        "minecraft:zombie_villager",
        "minecraft:zombie_villager_spawn_egg",
    ),
];

pub fn upgrade_field(compound: &mut Compound, key: &str, fixer: Fixer) {
    if let Some(Tag::Compound(item)) = compound.get_mut(key) {
        upgrade(item, fixer);
    }
}

/// Upgrades every item of the list at `key`.
pub fn upgrade_list(compound: &mut Compound, key: &str, fixer: Fixer) {
    for_each_compound(compound, key, |item| upgrade(item, fixer));
}

pub fn upgrade(item: &mut Compound, fixer: Fixer) {
    if !fixer.pending() {
        return;
    }
    upgrade_nested(item, fixer);
    rename_item(item, fixer);
    super::pre2860::item(item, fixer);
    if fixer.due(3094, 0)
        && id_of(item) == "minecraft:goat_horn"
        && let Some(tag) = item.get_compound_mut("tag")
    {
        let variant = tag
            .get("SoundVariant")
            .and_then(super::number)
            .unwrap_or(0.0) as i64;
        let name = usize::try_from(variant)
            .ok()
            .and_then(|v| GOAT_HORNS.get(v))
            .unwrap_or(&GOAT_HORNS[0]);
        tag.remove("SoundVariant");
        tag.insert("instrument", *name);
    }
    if fixer.due(3209, 0) && id_of(item) == "minecraft:pig_spawn_egg" {
        fix_pig_spawn_egg(item);
    }
    if fixer.due(3322, 0)
        && is_potion_holder(item)
        && let Some(tag) = item.get_compound_mut("tag")
    {
        for_each_compound(tag, "CustomPotionEffects", entity::fix_effect_duration);
    }
    if fixer.due(3568, 0) {
        fix_legacy_effect_ids(item);
    }
    if fixer.due(3803, 0)
        && let Some(tag) = item.get_compound_mut("tag")
    {
        for key in ["Enchantments", "StoredEnchantments"] {
            for_each_compound(tag, key, |enchantment| {
                rename_id(enchantment, "id", ENCHANTMENT_RENAMES)
            });
        }
    }
    if fixer.due(3813, 0)
        && id_of(item) == "minecraft:compass"
        && let Some(tag) = item.get_compound_mut("tag")
        && let Some(pos) = tag.remove("LodestonePos")
    {
        tag.insert("LodestonePos", super::fix_block_pos(pos));
    }
    if fixer.due(3814, 0)
        && let Some(tag) = item.get_compound_mut("tag")
    {
        for_each_compound(tag, "AttributeModifiers", |modifier| {
            rename_id(modifier, "AttributeName", JUMP_STRENGTH_RENAMES)
        });
    }
    if fixer.due(3818, 5) && item.contains_key("Count") {
        componentize(item);
    }
    let id = id_of(item);
    if let Some(components) = item.get_compound_mut(COMPONENTS) {
        fix_components(components, &id, fixer);
    }
    if fixer.due(4885, 0)
        && let Some(components) = item.get_compound_mut(COMPONENTS)
        && components
            .get_compound("minecraft:block_entity_data")
            .is_some_and(|data| id_of(data) == "minecraft:bed")
    {
        components.remove("minecraft:block_entity_data");
    }
    if fixer.due(5008, 0) {
        fix_explorer_map(item);
    }
    if fixer.due(5012, 0) {
        rename_id(item, "id", MAP_RENAMES);
    }
    if fixer.due(5016, 0)
        && id_of(item) == "minecraft:map"
        && let Some(components) = item.get_compound_mut(COMPONENTS)
        && let Some(Tag::Compound(name)) = components.get_mut("minecraft:item_name")
        && text::plain_translation_key(&Tag::Compound(name.clone()))
            == Some("filled_map.buried_treasure")
    {
        name.insert("translate", "item.minecraft.buried_treasure_map");
    }
}

fn fix_pig_spawn_egg(item: &mut Compound) {
    let Some(entity_id) = item
        .get_compound("tag")
        .and_then(|tag| tag.get_compound("EntityTag"))
        .and_then(|entity| entity.get_str("id"))
        .map(namespaced)
    else {
        return;
    };
    let egg = PIG_EGG_ENTITIES
        .iter()
        .find(|(entity, _)| *entity == entity_id)
        .map_or("minecraft:pig_spawn_egg", |(_, egg)| egg);
    item.insert("id", egg);
}

fn is_potion_holder(item: &Compound) -> bool {
    POTION_HOLDERS.contains(&id_of(item).as_str())
}

pub(crate) fn rename_ids_only(item: &mut Compound, fixer: Fixer) {
    rename_item(item, fixer);
}

fn rename_item(item: &mut Compound, fixer: Fixer) {
    for (version, table) in LEGACY_ITEM_RENAMES {
        if fixer.due(*version, 0) {
            rename_id(item, "id", table);
        }
    }
    if fixer.due(3438, 0) {
        rename_id(item, "id", SHARD_RENAMES);
    }
    if fixer.due(3447, 0)
        && let Some(Tag::String(id)) = item.get_mut("id")
    {
        let full = namespaced(id);
        if let Some(name) = full
            .strip_prefix("minecraft:")
            .and_then(|n| n.strip_suffix("_pottery_shard"))
            && SHERD_NAMES.contains(&name)
        {
            *id = format!("minecraft:{name}_pottery_sherd");
        }
    }
    if fixer.due(3692, 0) {
        rename_id(item, "id", GRASS_RENAMES);
    }
    if fixer.due(3800, 0) {
        rename_id(item, "id", SCUTE_RENAMES);
    }
    if fixer.due(4541, 0) {
        rename_id(item, "id", CHAIN_RENAMES);
    }
}

fn upgrade_nested(item: &mut Compound, fixer: Fixer) {
    let Some(inner) = fixer.nested() else {
        return;
    };
    let id = id_of(item);
    if let Some(tag) = item.get_compound_mut("tag") {
        if let Some(block_entity) = tag.get_compound_mut("BlockEntityTag") {
            if !block_entity.contains_key("id")
                && let Some(kind) = block_entity_for_item(&id)
            {
                block_entity.insert("id", kind);
            }
            block_entity::upgrade(block_entity, inner);
        }
        if let Some(entity) = tag.get_compound_mut("EntityTag") {
            if !entity.contains_key("id")
                && let Some(kind) = entity_for_item(&id)
            {
                entity.insert("id", kind);
            }
            entity::upgrade(entity, inner);
        }
        upgrade_list(tag, "Items", inner);
        upgrade_list(tag, "ChargedProjectiles", inner);
    }
    if let Some(components) = item.get_compound_mut(COMPONENTS) {
        upgrade_list(components, "minecraft:bundle_contents", inner);
        upgrade_list(components, "minecraft:charged_projectiles", inner);
        for_each_compound(components, "minecraft:container", |slot| {
            upgrade_field(slot, "item", inner)
        });
        upgrade_field(components, "minecraft:use_remainder", inner);
        upgrade_field(components, "minecraft:sulfur_cube_content", inner);
        if let Some(food) = components.get_compound_mut("minecraft:food") {
            upgrade_field(food, "using_converts_to", inner);
        }
        if let Some(Tag::Compound(data)) = components.get_mut("minecraft:entity_data") {
            entity::upgrade(data, inner);
        }
        if let Some(Tag::Compound(data)) = components.get_mut("minecraft:block_entity_data") {
            block_entity::upgrade(data, inner);
        }
        for_each_compound(components, "minecraft:bees", |bee| {
            entity::upgrade_field(bee, "entity_data", inner)
        });
    }
}

fn block_entity_for_item(item: &str) -> Option<String> {
    let block = Block::from_name(item)?;
    block.block_entity().map(|kind| kind.name().to_owned())
}

fn entity_for_item(item: &str) -> Option<String> {
    let name = item.strip_prefix("minecraft:")?;
    if let Some(mob) = name.strip_suffix("_spawn_egg") {
        return Some(format!("minecraft:{mob}"));
    }
    matches!(
        name,
        "armor_stand" | "item_frame" | "glow_item_frame" | "painting"
    )
    .then(|| item.to_owned())
}

fn fix_legacy_effect_ids(item: &mut Compound) {
    let id = id_of(item);
    let Some(tag) = item.get_compound_mut("tag") else {
        return;
    };
    if id == "minecraft:suspicious_stew" {
        if let Some(Tag::List(effects)) = tag.remove("Effects") {
            let effects: Vec<Tag> = effects
                .into_iter()
                .map(|effect| match effect {
                    Tag::Compound(mut entry) => {
                        entity::stew_effect_entry(&mut entry);
                        Tag::Compound(entry)
                    }
                    other => other,
                })
                .collect();
            tag.insert("effects", effects);
        }
    } else if POTION_HOLDERS.contains(&id.as_str()) {
        entity::update_effect_list(tag, "CustomPotionEffects", "custom_potion_effects");
    }
}

/// Item stack componentization (1.20.5).
fn componentize(item: &mut Compound) {
    let Some(Tag::String(raw_id)) = item.remove("id") else {
        return;
    };
    let Some(count) = item.remove("Count").as_ref().and_then(super::number) else {
        item.insert("id", raw_id);
        return;
    };
    let id = namespaced(&raw_id);
    let mut tag = match item.remove("tag") {
        Some(Tag::Compound(tag)) => tag,
        _ => Compound::new(),
    };
    let mut components = Compound::new();
    let hide = tag
        .remove("HideFlags")
        .as_ref()
        .and_then(super::number)
        .map_or(0, |v| v as i32);
    move_unless(
        &mut tag,
        &mut components,
        "Damage",
        "minecraft:damage",
        &Tag::Int(0),
    );
    move_unless(
        &mut tag,
        &mut components,
        "RepairCost",
        "minecraft:repair_cost",
        &Tag::Int(0),
    );
    move_tag(
        &mut tag,
        &mut components,
        "CustomModelData",
        "minecraft:custom_model_data",
    );
    if let Some(state) = tag.remove("BlockStateTag") {
        components.insert("minecraft:block_state", fix_block_state_tag(state));
    }
    move_tag(
        &mut tag,
        &mut components,
        "EntityTag",
        "minecraft:entity_data",
    );
    if let Some(Tag::Compound(mut block_entity)) = tag.remove("BlockEntityTag") {
        let kind = id_of(&block_entity);
        fix_block_entity_tag(&mut components, &mut block_entity, &kind);
        let mut without_id = block_entity.clone();
        without_id.remove("id");
        if !without_id.is_empty() {
            components.insert("minecraft:block_entity_data", block_entity);
        }
    }
    if tag
        .remove("Unbreakable")
        .as_ref()
        .and_then(super::number)
        .unwrap_or(0.0)
        != 0.0
    {
        let mut unbreakable = Compound::new();
        if hide & 4 != 0 {
            unbreakable.insert("show_in_tooltip", false);
        }
        components.insert("minecraft:unbreakable", unbreakable);
    }
    fix_enchantments(
        &mut tag,
        &mut components,
        "Enchantments",
        "minecraft:enchantments",
        hide & 1 != 0,
    );
    if id == "minecraft:enchanted_book" {
        fix_enchantments(
            &mut tag,
            &mut components,
            "StoredEnchantments",
            "minecraft:stored_enchantments",
            hide & 32 != 0,
        );
    }
    let mut display = match tag.remove("display") {
        Some(Tag::Compound(display)) => display,
        _ => Compound::new(),
    };
    fix_display(&mut components, &mut display, hide, &id);
    if !display.is_empty() {
        tag.insert("display", display);
    }
    fix_predicates(
        &mut tag,
        &mut components,
        "CanDestroy",
        "minecraft:can_break",
        hide & 8 != 0,
    );
    fix_predicates(
        &mut tag,
        &mut components,
        "CanPlaceOn",
        "minecraft:can_place_on",
        hide & 16 != 0,
    );
    fix_attribute_modifiers(&mut tag, &mut components, hide & 2 != 0);
    if let Some(mut trim) = tag.remove("Trim") {
        if hide & 128 != 0
            && let Tag::Compound(trim) = &mut trim
        {
            trim.insert("show_in_tooltip", false);
        }
        components.insert("minecraft:trim", trim);
    }
    if hide & 32 != 0 {
        components.insert("minecraft:hide_additional_tooltip", Compound::new());
    }
    match id.as_str() {
        "minecraft:crossbow" => {
            tag.remove("Charged");
            move_unless(
                &mut tag,
                &mut components,
                "ChargedProjectiles",
                "minecraft:charged_projectiles",
                &Tag::List(Vec::new()),
            );
        }
        "minecraft:bundle" => move_unless(
            &mut tag,
            &mut components,
            "Items",
            "minecraft:bundle_contents",
            &Tag::List(Vec::new()),
        ),
        "minecraft:filled_map" => {
            move_tag(&mut tag, &mut components, "map", "minecraft:map_id");
            let mut decorations = Compound::new();
            if let Some(Tag::List(list)) = tag.remove("Decorations") {
                for decoration in list {
                    if let Tag::Compound(decoration) = decoration {
                        let (key, value) = fix_map_decoration(&decoration);
                        if !decorations.contains_key(&key) {
                            decorations.insert(key, value);
                        }
                    }
                }
            }
            if !decorations.is_empty() {
                components.insert("minecraft:map_decorations", decorations);
            }
        }
        "minecraft:writable_book" => {
            if let Some(pages) = fix_book_pages(&mut tag) {
                components.insert(
                    "minecraft:writable_book_content",
                    Compound::new().with("pages", pages),
                );
            }
        }
        "minecraft:written_book" => {
            let pages = fix_book_pages(&mut tag);
            let title = match tag.remove("title") {
                Some(Tag::String(title)) => title,
                _ => String::new(),
            };
            let filtered_title = match tag.remove("filtered_title") {
                Some(Tag::String(title)) => Some(title),
                _ => None,
            };
            let mut content = Compound::new().with("title", filtered_text(title, filtered_title));
            for key in ["author", "resolved", "generation"] {
                if let Some(value) = tag.remove(key) {
                    content.insert(key, value);
                }
            }
            if let Some(pages) = pages {
                content.insert("pages", pages);
            }
            components.insert("minecraft:written_book_content", content);
        }
        "minecraft:suspicious_stew" => move_tag(
            &mut tag,
            &mut components,
            "effects",
            "minecraft:suspicious_stew_effects",
        ),
        "minecraft:debug_stick" => move_tag(
            &mut tag,
            &mut components,
            "DebugProperty",
            "minecraft:debug_stick_state",
        ),
        "minecraft:goat_horn" => move_tag(
            &mut tag,
            &mut components,
            "instrument",
            "minecraft:instrument",
        ),
        "minecraft:knowledge_book" => {
            move_tag(&mut tag, &mut components, "Recipes", "minecraft:recipes")
        }
        "minecraft:compass" => fix_lodestone_tracker(&mut tag, &mut components),
        "minecraft:firework_rocket" => {
            if let Some(Tag::Compound(mut fireworks)) = tag.remove("Fireworks") {
                let explosions: Vec<Tag> = match fireworks.remove("Explosions") {
                    Some(Tag::List(list)) => list
                        .into_iter()
                        .filter_map(|e| match e {
                            Tag::Compound(e) => Some(Tag::Compound(fix_firework_explosion(e))),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let flight = fireworks
                    .remove("Flight")
                    .as_ref()
                    .and_then(super::number)
                    .map_or(0, |v| v as i32);
                components.insert(
                    "minecraft:fireworks",
                    Compound::new()
                        .with("explosions", explosions)
                        .with("flight_duration", flight as i8),
                );
                if !fireworks.is_empty() {
                    tag.insert("Fireworks", fireworks);
                }
            }
        }
        "minecraft:firework_star" => {
            if let Some(Tag::Compound(mut explosion)) = tag.remove("Explosion") {
                components.insert(
                    "minecraft:firework_explosion",
                    fix_firework_explosion(explosion.clone()),
                );
                for key in ["Type", "Colors", "FadeColors", "Trail", "Flicker"] {
                    explosion.remove(key);
                }
                if !explosion.is_empty() {
                    tag.insert("Explosion", explosion);
                }
            }
        }
        "minecraft:player_head" => {
            if let Some(owner) = tag.remove("SkullOwner") {
                components.insert("minecraft:profile", fix_profile(&owner));
            }
        }
        _ => {}
    }
    if POTION_HOLDERS.contains(&id.as_str()) {
        fix_potion_contents(&mut tag, &mut components);
    }
    if BUCKETED_MOBS.contains(&id.as_str()) {
        let mut data = Compound::new();
        for key in BUCKETED_MOB_TAGS {
            if let Some(value) = tag.remove(key) {
                data.insert(key, value);
            }
        }
        if !data.is_empty() {
            components.insert("minecraft:bucket_entity_data", data);
        }
    }
    if !tag.is_empty() {
        components.insert("minecraft:custom_data", tag);
    }
    let remainder = std::mem::take(item);
    item.insert("id", id);
    item.insert("count", count as i32);
    if !components.is_empty() {
        item.insert(COMPONENTS, components);
    }
    for (key, value) in remainder {
        if !item.contains_key(&key) {
            item.insert(key, value);
        }
    }
}
mod components;
pub(crate) use components::*;
