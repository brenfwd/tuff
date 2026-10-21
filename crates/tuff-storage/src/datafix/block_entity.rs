//! Block entities, in chunks and in item and falling block data.

use super::{
    Fixer, dye_color_name, entity, fix_block_pos, for_each_compound, get_bool, get_number, id_of,
    item, rename, rename_with, text,
};
use tuff_nbt::{Compound, Tag};

const SIGN_FIELDS_TO_DROP: [&str; 10] = [
    "Text1",
    "Text2",
    "Text3",
    "Text4",
    "FilteredText1",
    "FilteredText2",
    "FilteredText3",
    "FilteredText4",
    "Color",
    "GlowingText",
];
const FILTERED_CORRECT: &str = "_filtered_correct";
const BANNER_PATTERNS: [(&str, &str); 41] = [
    ("b", "base"),
    ("bl", "square_bottom_left"),
    ("br", "square_bottom_right"),
    ("tl", "square_top_left"),
    ("tr", "square_top_right"),
    ("bs", "stripe_bottom"),
    ("ts", "stripe_top"),
    ("ls", "stripe_left"),
    ("rs", "stripe_right"),
    ("cs", "stripe_center"),
    ("ms", "stripe_middle"),
    ("drs", "stripe_downright"),
    ("dls", "stripe_downleft"),
    ("ss", "small_stripes"),
    ("cr", "cross"),
    ("sc", "straight_cross"),
    ("bt", "triangle_bottom"),
    ("tt", "triangle_top"),
    ("bts", "triangles_bottom"),
    ("tts", "triangles_top"),
    ("ld", "diagonal_left"),
    ("rd", "diagonal_up_right"),
    ("lud", "diagonal_up_left"),
    ("rud", "diagonal_right"),
    ("mc", "circle"),
    ("mr", "rhombus"),
    ("vh", "half_vertical"),
    ("hh", "half_horizontal"),
    ("vhr", "half_vertical_right"),
    ("hhb", "half_horizontal_bottom"),
    ("bo", "border"),
    ("cbo", "curly_border"),
    ("gra", "gradient"),
    ("gru", "gradient_up"),
    ("bri", "bricks"),
    ("glb", "globe"),
    ("cre", "creeper"),
    ("sku", "skull"),
    ("flo", "flower"),
    ("moj", "mojang"),
    ("pig", "piglin"),
];
const TRIAL_SPAWNER_CONFIG: [&str; 9] = [
    "spawn_range",
    "total_mobs",
    "simultaneous_mobs",
    "total_mobs_added_per_player",
    "simultaneous_mobs_added_per_player",
    "ticks_between_spawn",
    "spawn_potentials",
    "loot_tables_to_eject",
    "items_to_drop_when_ominous",
];
const FURNACES: [&str; 3] = [
    "minecraft:furnace",
    "minecraft:smoker",
    "minecraft:blast_furnace",
];

pub fn upgrade(block_entity: &mut Compound, fixer: Fixer) {
    if !fixer.pending() {
        return;
    }
    super::pre2860::block_entity(block_entity, fixer);
    upgrade_nested(block_entity, fixer);
    if fixer.due(3438, 0) && id_of(block_entity) == "minecraft:suspicious_sand" {
        block_entity.insert("id", "minecraft:brushable_block");
    }
    let id = id_of(block_entity);
    let be = block_entity;
    if fixer.due(3438, 0) && id == "minecraft:brushable_block" {
        rename(be, "loot_table", "LootTable");
        rename(be, "loot_table_seed", "LootTableSeed");
    }
    rename_sherds(be, fixer);
    if matches!(
        id.as_str(),
        "minecraft:sculk_sensor" | "minecraft:sculk_shrieker"
    ) {
        super::rename_vibration_game_event(be, fixer);
    }
    if fixer.due(3439, 0) && id == "minecraft:sign" {
        double_sided_sign(be);
    }
    if fixer.due(3439, 1) && id == "minecraft:hanging_sign" {
        double_sided_sign(be);
    }
    if (fixer.due(3564, 0) && id == "minecraft:sign")
        || (fixer.due(3564, 1) && id == "minecraft:hanging_sign")
    {
        drop_invalid_sign_data(be);
    }
    if fixer.due(3568, 0) && id == "minecraft:beacon" {
        entity::update_effect_id(be, "Primary", "primary_effect");
        entity::update_effect_id(be, "Secondary", "secondary_effect");
    }
    if fixer.due(3813, 0) {
        match id.as_str() {
            "minecraft:beehive" => {
                rename_with(be, "FlowerPos", "flower_pos", |p| Some(fix_block_pos(p)))
            }
            "minecraft:end_gateway" => {
                rename_with(be, "ExitPortal", "exit_portal", |p| Some(fix_block_pos(p)))
            }
            _ => {}
        }
    }
    if fixer.due(3818, 0) && id == "minecraft:beehive" {
        rename(be, "Bees", "bees");
        for_each_compound(be, "bees", |bee| {
            rename(bee, "EntityData", "entity_data");
            rename(bee, "TicksInHive", "ticks_in_hive");
            rename(bee, "MinOccupationTicks", "min_ticks_in_hive");
        });
    }
    if fixer.due(3818, 1) && id == "minecraft:banner" {
        rename_with(be, "Patterns", "patterns", |patterns| match patterns {
            Tag::List(layers) => Some(Tag::List(
                layers
                    .into_iter()
                    .map(|layer| match layer {
                        Tag::Compound(layer) => Tag::Compound(fix_banner_layer(layer)),
                        other => other,
                    })
                    .collect(),
            )),
            other => Some(other),
        });
    }
    if fixer.due(3820, 0) && id == "minecraft:skull" {
        let owner = be.remove("SkullOwner");
        let extra = be.remove("ExtraType");
        if let Some(owner) = owner.or(extra) {
            be.insert("profile", item::fix_profile(&owner));
        }
    }
    if fixer.due(3825, 0) {
        if id == "minecraft:banner"
            && be
                .get("CustomName")
                .and_then(text::json_translation_key)
                .is_some_and(|key| key == "block.minecraft.ominous_banner")
            && let Some(name) = be.remove("CustomName")
        {
            be.insert(
                "components",
                Compound::new()
                    .with("minecraft:item_name", name)
                    .with("minecraft:hide_additional_tooltip", Compound::new()),
            );
        }
        if id == "minecraft:trial_spawner" {
            let mut config = Compound::new();
            for key in TRIAL_SPAWNER_CONFIG {
                if let Some(value) = be.remove(key) {
                    config.insert(key, value);
                }
            }
            if !config.is_empty() {
                be.insert("normal_config", config);
            }
        }
    }
    if fixer.due(3833, 0)
        && id == "minecraft:brushable_block"
        && be.get_compound("item").is_some_and(is_empty_stack)
    {
        be.remove("item");
    }
    if fixer.due(3945, 0) && id == "minecraft:jukebox" {
        let ticks = get_number(be, "TickCount").map_or(0, |v| v as i64)
            - get_number(be, "RecordStartTick").map_or(0, |v| v as i64);
        for key in ["IsPlaying", "TickCount", "RecordStartTick"] {
            be.remove(key);
        }
        if ticks > 0 {
            be.insert("ticks_since_song_started", ticks);
        }
    }
    if fixer.due(4054, 0) && id == "minecraft:banner" {
        let ominous = be
            .get_compound("components")
            .and_then(|c| c.get("minecraft:item_name"))
            .and_then(text::json_translation_key)
            .is_some_and(|key| key == "block.minecraft.ominous_banner");
        if ominous && let Some(components) = be.get_compound_mut("components") {
            components.insert(
                "minecraft:item_name",
                text::json_translate("block.minecraft.ominous_banner"),
            );
            components.insert("minecraft:rarity", "uncommon");
        }
    }
    if fixer.due(4061, 0) && id == "minecraft:trial_spawner" {
        super::trial_registry::upgrade(be);
    }
    if fixer.due(4068, 0) {
        rename_with(be, "Lock", "lock", item::lock_predicate);
    }
    if fixer.due(4176, 0) && be.get("lock").is_some_and(item::is_broken_lock) {
        be.remove("lock");
    }
    if fixer.due(4181, 0) && FURNACES.contains(&id.as_str()) {
        rename(be, "CookTime", "cooking_time_spent");
        rename(be, "CookTimeTotal", "cooking_total_time");
        rename(be, "BurnTime", "lit_time_remaining");
        if let Some(remaining) = be.get("lit_time_remaining").cloned() {
            be.insert("lit_total_time", remaining);
        }
    }
    text::upgrade_field(be, "CustomName", fixer);
    text::upgrade_field(be, "LastOutput", fixer);
    for side in ["front_text", "back_text"] {
        if let Some(side) = be.get_compound_mut(side) {
            text::upgrade_list(side, "messages", fixer);
            text::upgrade_list(side, "filtered_messages", fixer);
        }
    }
    if let Some(components) = be.get_compound_mut("components") {
        text::upgrade_field(components, "minecraft:item_name", fixer);
        text::upgrade_field(components, "minecraft:custom_name", fixer);
    }
    if let Some(lock) = be.get_compound_mut("lock")
        && let Some(predicate) = lock.get_compound_mut("components")
    {
        text::upgrade_field(predicate, "minecraft:custom_name", fixer);
    }
    if fixer.due(4769, 0)
        && FURNACES.contains(&id.as_str())
        && let Some(Tag::Compound(used)) = be.remove("RecipesUsed")
    {
        let renamed = used
            .into_iter()
            .map(|(recipe, count)| {
                let recipe = super::level::renamed_stonecutter_recipe(&recipe).unwrap_or(recipe);
                (recipe, count)
            })
            .collect::<Compound>();
        be.insert("RecipesUsed", renamed);
    }
    if id == "minecraft:decorated_pot" {
        item::fix_pot_decorations(be, "sherds", fixer, fixer.due(4996, 1));
    }
    if fixer.due(5002, 0) && matches!(id.as_str(), "minecraft:sign" | "minecraft:hanging_sign") {
        be.insert("allow_op_features", true);
    }
}

fn upgrade_nested(be: &mut Compound, fixer: Fixer) {
    let Some(inner) = fixer.nested() else {
        return;
    };
    item::upgrade_list(be, "Items", inner);
    for key in ["item", "RecordItem", "Book"] {
        item::upgrade_field(be, key, inner);
    }
    for key in ["Bees", "bees"] {
        for_each_compound(be, key, |bee| {
            entity::upgrade_field(bee, "EntityData", inner);
            entity::upgrade_field(bee, "entity_data", inner);
        });
    }
    for key in ["SpawnData", "spawn_data"] {
        if let Some(data) = be.get_compound_mut(key) {
            entity::upgrade_field(data, "entity", inner);
        }
    }
    for_each_compound(be, "SpawnPotentials", |potential| {
        if let Some(data) = potential.get_compound_mut("data") {
            entity::upgrade_field(data, "entity", inner);
        }
    });
    for key in ["normal_config", "ominous_config"] {
        if let Some(config) = be.get_compound_mut(key) {
            for_each_compound(config, "spawn_potentials", |potential| {
                if let Some(data) = potential.get_compound_mut("data") {
                    entity::upgrade_field(data, "entity", inner);
                }
            });
        }
    }
    if let Some(shared) = be.get_compound_mut("shared_data") {
        item::upgrade_field(shared, "display_item", inner);
    }
    if let Some(server) = be.get_compound_mut("server_data") {
        item::upgrade_list(server, "items_to_eject", inner);
    }
    if let Some(config) = be.get_compound_mut("config") {
        item::upgrade_field(config, "key_item", inner);
    }
}

fn rename_sherds(be: &mut Compound, fixer: Fixer) {
    if let Some(sherds) = be.get_list_mut("sherds") {
        for sherd in sherds {
            if let Tag::String(id) = sherd {
                let mut stack = Compound::new().with("id", id.as_str());
                item::rename_ids_only(&mut stack, fixer);
                if let Some(new) = stack.get_str("id") {
                    *id = new.to_owned();
                }
            }
        }
    }
}

fn is_empty_stack(item: &Compound) -> bool {
    let id = super::namespaced(item.get_str("id").unwrap_or("minecraft:air"));
    id == "minecraft:air" || get_number(item, "count").map_or(0, |v| v as i32) == 0
}

fn fix_banner_layer(mut layer: Compound) -> Compound {
    rename_with(&mut layer, "Pattern", "pattern", |pattern| match pattern {
        Tag::String(code) => Some(Tag::String(
            BANNER_PATTERNS
                .iter()
                .find(|(short, _)| *short == code)
                .map_or(code, |(_, name)| format!("minecraft:{name}")),
        )),
        other => Some(other),
    });
    let color = get_number(&layer, "Color").map_or(0, |v| v as i32);
    layer.remove("Color");
    layer.insert("color", dye_color_name(color));
    layer
}

fn double_sided_sign(be: &mut Compound) {
    let empty = text::json_text("");
    let lines: Vec<Option<Tag>> = (1..=4)
        .map(|i| be.get(&format!("Text{i}")).cloned())
        .collect();
    let messages: Vec<Tag> = lines
        .iter()
        .map(|line| line.clone().unwrap_or_else(|| Tag::String(empty.clone())))
        .collect();
    let mut front = Compound::new()
        .with("messages", messages.clone())
        .with(
            "color",
            be.get("Color")
                .cloned()
                .unwrap_or_else(|| Tag::from("black")),
        )
        .with(
            "has_glowing_text",
            be.get("GlowingText").cloned().unwrap_or(Tag::from(false)),
        );
    let filtered: Vec<Option<Tag>> = (1..=4)
        .map(|i| be.get(&format!("FilteredText{i}")).cloned())
        .collect();
    if filtered.iter().any(Option::is_some) {
        front.insert(
            "filtered_messages",
            filtered
                .into_iter()
                .zip(&messages)
                .map(|(line, fallback)| line.unwrap_or_else(|| fallback.clone()))
                .collect::<Vec<_>>(),
        );
    }
    let back = Compound::new()
        .with("messages", vec![Tag::String(empty); 4])
        .with("color", "black")
        .with("has_glowing_text", false);
    be.insert("front_text", front);
    be.insert("back_text", back);
    be.insert("is_waxed", false);
    be.insert(FILTERED_CORRECT, true);
    for field in SIGN_FIELDS_TO_DROP {
        be.remove(field);
    }
}

fn drop_invalid_sign_data(be: &mut Compound) {
    if get_bool(be, FILTERED_CORRECT) == Some(true) {
        be.remove(FILTERED_CORRECT);
        return;
    }
    let empty = Tag::String(text::json_text(""));
    for side in ["front_text", "back_text"] {
        let Some(side) = be.get_compound_mut(side) else {
            continue;
        };
        let Some(filtered) = side.get_list("filtered_messages").map(<[Tag]>::to_vec) else {
            continue;
        };
        let messages = side
            .get_list("messages")
            .map(<[Tag]>::to_vec)
            .unwrap_or_default();
        let fixed: Vec<Tag> = filtered
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                if line == empty {
                    messages.get(i).cloned().unwrap_or_else(|| empty.clone())
                } else {
                    line
                }
            })
            .collect();
        if fixed == messages {
            side.remove("filtered_messages");
        } else {
            side.insert("filtered_messages", fixed);
        }
    }
    for field in SIGN_FIELDS_TO_DROP {
        be.remove(field);
    }
}
