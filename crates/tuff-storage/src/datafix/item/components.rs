use super::*;

pub(super) fn move_tag(tag: &mut Compound, components: &mut Compound, key: &str, component: &str) {
    if let Some(value) = tag.remove(key) {
        components.insert(component, value);
    }
}

pub(super) fn move_unless(
    tag: &mut Compound,
    components: &mut Compound,
    key: &str,
    component: &str,
    default: &Tag,
) {
    if let Some(value) = tag.remove(key)
        && value != *default
    {
        components.insert(component, value);
    }
}

pub(super) fn fix_block_state_tag(state: Tag) -> Tag {
    let Tag::Compound(state) = state else {
        return state;
    };
    let mut out = Compound::with_capacity(state.len());
    for (key, value) in state {
        let fixed = match super::super::number(&value) {
            Some(n) if BOOLEAN_STATE_PROPERTIES.contains(&key.as_str()) => {
                Tag::String((n as i64 as i8 != 0).to_string())
            }
            Some(_) => Tag::String(number_string(&value)),
            None => value,
        };
        out.insert(key, fixed);
    }
    Tag::Compound(out)
}

pub(super) fn number_string(tag: &Tag) -> String {
    match *tag {
        Tag::Byte(v) => v.to_string(),
        Tag::Short(v) => v.to_string(),
        Tag::Int(v) => v.to_string(),
        Tag::Long(v) => v.to_string(),
        Tag::Float(v) => format!("{v:?}"),
        Tag::Double(v) => format!("{v:?}"),
        _ => String::new(),
    }
}

pub(super) fn fix_display(components: &mut Compound, display: &mut Compound, hide: i32, id: &str) {
    if let Some(name) = display.remove("Name")
        && strictly_json(&name)
    {
        components.insert("minecraft:custom_name", name);
    }
    if let Some(Tag::List(lore)) = display.remove("Lore") {
        components.insert(
            "minecraft:lore",
            lore.into_iter().filter(strictly_json).collect::<Vec<_>>(),
        );
    }
    let color = display
        .remove("color")
        .as_ref()
        .and_then(super::super::number)
        .map(|v| v as i32);
    let hide_dye = hide & 64 != 0;
    if color.is_some() || hide_dye {
        let mut dyed = Compound::new().with("rgb", color.unwrap_or(10_511_680));
        if hide_dye {
            dyed.insert("show_in_tooltip", false);
        }
        components.insert("minecraft:dyed_color", dyed);
    }
    if let Some(Tag::String(key)) = display.remove("LocName") {
        components.insert("minecraft:item_name", text::json_translate(&key));
    }
    if id == "minecraft:filled_map"
        && let Some(color) = display.remove("MapColor")
    {
        components.insert("minecraft:map_color", color);
    }
}

pub(super) fn strictly_json(tag: &Tag) -> bool {
    tag.as_str()
        .is_some_and(|text| serde_json::from_str::<serde_json::Value>(text).is_ok())
}

pub(super) fn fix_block_entity_tag(
    components: &mut Compound,
    block_entity: &mut Compound,
    kind: &str,
) {
    if let Some(lock) = block_entity.remove("Lock") {
        components.insert("minecraft:lock", lock);
    }
    if let Some(table) = block_entity.remove("LootTable") {
        let mut loot = Compound::new().with("loot_table", table);
        let seed = block_entity
            .remove("LootTableSeed")
            .as_ref()
            .and_then(super::super::number)
            .map_or(0, |v| v as i64);
        if seed != 0 {
            loot.insert("seed", seed);
        }
        components.insert("minecraft:container_loot", loot);
    }
    match kind {
        "minecraft:skull" => {
            if let Some(sound) = block_entity.remove("note_block_sound") {
                components.insert("minecraft:note_block_sound", sound);
            }
        }
        "minecraft:decorated_pot" => {
            if let Some(sherds) = block_entity.remove("sherds") {
                components.insert("minecraft:pot_decorations", sherds);
            }
            if let Some(item) = block_entity.remove("item") {
                components.insert(
                    "minecraft:container",
                    vec![Compound::new().with("slot", 0).with("item", item)],
                );
            }
        }
        "minecraft:banner" => {
            if let Some(patterns) = block_entity.remove("patterns") {
                components.insert("minecraft:banner_patterns", patterns);
            }
            if let Some(base) = block_entity
                .remove("Base")
                .as_ref()
                .and_then(super::super::number)
            {
                components.insert("minecraft:base_color", dye_color_name(base as i32));
            }
        }
        "minecraft:shulker_box"
        | "minecraft:chest"
        | "minecraft:trapped_chest"
        | "minecraft:furnace"
        | "minecraft:ender_chest"
        | "minecraft:dispenser"
        | "minecraft:dropper"
        | "minecraft:brewing_stand"
        | "minecraft:hopper"
        | "minecraft:barrel"
        | "minecraft:smoker"
        | "minecraft:blast_furnace"
        | "minecraft:campfire"
        | "minecraft:chiseled_bookshelf"
        | "minecraft:crafter" => {
            if let Some(Tag::List(items)) = block_entity.remove("Items") {
                let slots: Vec<Compound> = items
                    .into_iter()
                    .filter_map(|tag| match tag {
                        Tag::Compound(mut item) => {
                            let slot = item
                                .remove("Slot")
                                .as_ref()
                                .and_then(super::super::number)
                                .map_or(0, |v| i32::from(v as i8 as u8));
                            Some(Compound::new().with("slot", slot).with("item", item))
                        }
                        _ => None,
                    })
                    .collect();
                if !slots.is_empty() {
                    components.insert("minecraft:container", slots);
                }
            }
        }
        "minecraft:beehive" => {
            if let Some(bees) = block_entity.remove("bees") {
                components.insert("minecraft:bees", bees);
            }
        }
        _ => {}
    }
}

pub(super) fn fix_enchantments(
    tag: &mut Compound,
    components: &mut Compound,
    key: &str,
    component: &str,
    hidden: bool,
) {
    let raw = tag.remove(key);
    let mut levels = Compound::new();
    if let Some(Tag::List(list)) = &raw {
        for entry in list {
            let Some(entry) = entry.as_compound() else {
                continue;
            };
            let (Some(id), Some(level)) = (entry.get_str("id"), get_number(entry, "lvl")) else {
                continue;
            };
            let level = (level as i64).clamp(0, 255) as i32;
            if level > 0 {
                levels.insert(id, level);
            }
        }
    }
    let empty = levels.is_empty();
    if !empty || hidden {
        let mut enchantments = Compound::new().with("levels", levels);
        if hidden {
            enchantments.insert("show_in_tooltip", false);
        }
        components.insert(component, enchantments);
    }
    if raw.is_some() && empty {
        components.insert("minecraft:enchantment_glint_override", true);
    }
}

pub(super) fn fix_predicates(
    tag: &mut Compound,
    components: &mut Compound,
    key: &str,
    component: &str,
    hidden: bool,
) {
    let Some(old) = tag.remove(key) else {
        return;
    };
    let predicates: Vec<Tag> = match old {
        Tag::List(list) => list
            .into_iter()
            .map(|value| match value {
                Tag::String(text) => Tag::Compound(block_predicate(&text)),
                other => other,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut out = Compound::new().with("predicates", predicates);
    if hidden {
        out.insert("show_in_tooltip", false);
    }
    components.insert(component, out);
}

pub(super) fn block_predicate(text: &str) -> Compound {
    let start_properties = text.find('[');
    let start_nbt = text.find('{');
    let mut end = text.len();
    if let Some(i) = start_properties {
        end = i;
    }
    if let Some(i) = start_nbt {
        end = end.min(i);
    }
    let mut predicate = Compound::new().with("blocks", text[..end].trim());
    if let (Some(start), Some(stop)) = (start_properties, text.find(']'))
        && stop > start
    {
        let mut properties = Compound::new();
        for property in text[start + 1..stop].split(',') {
            if let Some((key, value)) = property.split_once('=') {
                properties.insert(key.trim(), value.trim());
            }
        }
        predicate.insert("state", properties);
    }
    if let (Some(start), Some(stop)) = (start_nbt, text.find('}'))
        && stop >= start
    {
        predicate.insert("nbt", &text[start..=stop]);
    }
    predicate
}

pub(super) fn fix_attribute_modifiers(tag: &mut Compound, components: &mut Compound, hidden: bool) {
    let Some(raw) = tag.remove("AttributeModifiers") else {
        return;
    };
    let modifiers: Vec<Tag> = match raw {
        Tag::List(list) => list
            .iter()
            .filter_map(Tag::as_compound)
            .map(|modifier| Tag::Compound(fix_attribute_modifier(modifier)))
            .collect(),
        _ => Vec::new(),
    };
    let mut out = Compound::new().with("modifiers", modifiers);
    if hidden {
        out.insert("show_in_tooltip", false);
    }
    components.insert("minecraft:attribute_modifiers", out);
}

pub(super) fn fix_attribute_modifier(input: &Compound) -> Compound {
    let mut out = Compound::new()
        .with("name", "")
        .with("amount", 0.0f64)
        .with("operation", "add_value");
    for (from, to) in [
        ("AttributeName", "type"),
        ("Slot", "slot"),
        ("UUID", "uuid"),
        ("Name", "name"),
        ("Amount", "amount"),
    ] {
        if let Some(value) = input.get(from) {
            out.insert(to, value.clone());
        }
    }
    if let Some(operation) = input.get("Operation") {
        let name = match super::super::number(operation).map(|v| v as i32) {
            Some(1) => "add_multiplied_base",
            Some(2) => "add_multiplied_total",
            _ => "add_value",
        };
        out.insert("operation", name);
    }
    out
}

pub(super) fn fix_map_decoration(decoration: &Compound) -> (String, Compound) {
    let key = match decoration.get("id") {
        Some(Tag::String(id)) => id.clone(),
        _ => String::new(),
    };
    let kind = map_decoration_type(get_i32(decoration, "type").unwrap_or(0));
    let value = Compound::new()
        .with("type", kind)
        .with("x", get_number(decoration, "x").unwrap_or(0.0))
        .with("z", get_number(decoration, "z").unwrap_or(0.0))
        .with(
            "rotation",
            get_number(decoration, "rot").unwrap_or(0.0) as f32,
        );
    (key, value)
}

pub(super) fn map_decoration_type(id: i32) -> &'static str {
    pub(super) const TYPES: [&str; 34] = [
        "player",
        "frame",
        "red_marker",
        "blue_marker",
        "target_x",
        "target_point",
        "player_off_map",
        "player_off_limits",
        "mansion",
        "monument",
        "banner_white",
        "banner_orange",
        "banner_magenta",
        "banner_light_blue",
        "banner_yellow",
        "banner_lime",
        "banner_pink",
        "banner_gray",
        "banner_light_gray",
        "banner_cyan",
        "banner_purple",
        "banner_blue",
        "banner_brown",
        "banner_green",
        "banner_red",
        "banner_black",
        "red_x",
        "village_desert",
        "village_plains",
        "village_savanna",
        "village_snowy",
        "village_taiga",
        "jungle_temple",
        "swamp_hut",
    ];
    usize::try_from(id)
        .ok()
        .and_then(|i| TYPES.get(i))
        .copied()
        .unwrap_or("player")
}

pub(super) fn fix_potion_contents(tag: &mut Compound, components: &mut Compound) {
    let mut contents = Compound::new();
    if let Some(Tag::String(potion)) = tag.remove("Potion")
        && potion != "minecraft:empty"
    {
        contents.insert("potion", potion);
    }
    if let Some(color) = tag.remove("CustomPotionColor") {
        contents.insert("custom_color", color);
    }
    if let Some(effects) = tag.remove("custom_potion_effects") {
        contents.insert("custom_effects", effects);
    }
    if !contents.is_empty() {
        components.insert("minecraft:potion_contents", contents);
    }
}

pub(super) fn fix_book_pages(tag: &mut Compound) -> Option<Vec<Tag>> {
    let pages: Vec<String> = match tag.remove("pages") {
        Some(Tag::List(pages)) => pages
            .into_iter()
            .map(|page| match page {
                Tag::String(text) => text,
                _ => String::new(),
            })
            .collect(),
        _ => Vec::new(),
    };
    let filtered = match tag.remove("filtered_pages") {
        Some(Tag::Compound(filtered)) => filtered,
        _ => Compound::new(),
    };
    if pages.is_empty() {
        return None;
    }
    Some(
        pages
            .into_iter()
            .enumerate()
            .map(|(i, page)| {
                let filtered = filtered.get_str(&i.to_string()).map(str::to_owned);
                Tag::Compound(filtered_text(page, filtered))
            })
            .collect(),
    )
}

pub(super) fn filtered_text(raw: String, filtered: Option<String>) -> Compound {
    let mut out = Compound::new().with("raw", raw);
    if let Some(filtered) = filtered {
        out.insert("filtered", filtered);
    }
    out
}

pub(super) fn fix_lodestone_tracker(tag: &mut Compound, components: &mut Compound) {
    let pos = tag.remove("LodestonePos");
    let dimension = tag.remove("LodestoneDimension");
    if pos.is_none() && dimension.is_none() {
        return;
    }
    let tracked = tag
        .remove("LodestoneTracked")
        .as_ref()
        .and_then(super::super::number)
        .is_none_or(|v| v != 0.0);
    let mut tracker = Compound::new();
    if let (Some(pos), Some(dimension)) = (pos, dimension) {
        tracker.insert(
            "target",
            Compound::new()
                .with("pos", pos)
                .with("dimension", dimension),
        );
    }
    if !tracked {
        tracker.insert("tracked", false);
    }
    components.insert("minecraft:lodestone_tracker", tracker);
}

pub(super) fn fix_firework_explosion(mut explosion: Compound) -> Compound {
    let shape = match explosion
        .remove("Type")
        .as_ref()
        .and_then(super::super::number)
    {
        Some(v) if v as i32 == 1 => "large_ball",
        Some(v) if v as i32 == 2 => "star",
        Some(v) if v as i32 == 3 => "creeper",
        Some(v) if v as i32 == 4 => "burst",
        _ => "small_ball",
    };
    explosion.insert("shape", shape);
    rename(&mut explosion, "Colors", "colors");
    rename(&mut explosion, "FadeColors", "fade_colors");
    rename(&mut explosion, "Trail", "has_trail");
    rename(&mut explosion, "Flicker", "has_twinkle");
    explosion
}

/// A legacy skull owner (a name, or `{Name, Id, Properties}`) as a profile.
pub(crate) fn fix_profile(owner: &Tag) -> Tag {
    match owner {
        Tag::String(name) => {
            let mut profile = Compound::new();
            if valid_player_name(name) {
                profile.insert("name", name.as_str());
            }
            Tag::Compound(profile)
        }
        Tag::Compound(owner) => {
            let mut profile = Compound::new();
            let name = owner.get_str("Name").unwrap_or("");
            if valid_player_name(name) {
                profile.insert("name", name);
            }
            if let Some(id) = owner.get("Id") {
                profile.insert("id", id.clone());
            }
            if let Some(properties) = owner.get_compound("Properties") {
                let mut list = Vec::new();
                for (name, values) in properties.iter() {
                    for value in values.as_list().unwrap_or(&[]) {
                        let Some(value) = value.as_compound() else {
                            continue;
                        };
                        let mut property = Compound::new()
                            .with("name", name)
                            .with("value", value.get_str("Value").unwrap_or(""));
                        if let Some(signature) = value.get_str("Signature") {
                            property.insert("signature", signature);
                        }
                        list.push(Tag::Compound(property));
                    }
                }
                if !list.is_empty() {
                    profile.insert("properties", list);
                }
            }
            Tag::Compound(profile)
        }
        other => other.clone(),
    }
}

pub(super) fn valid_player_name(name: &str) -> bool {
    name.chars().count() <= 16 && !name.chars().any(|c| c <= ' ' || c >= '\u{7f}')
}

/// Every component fix after componentization, in version order.
pub(super) fn fix_components(components: &mut Compound, id: &str, fixer: Fixer) {
    if fixer.due(3820, 0)
        && let Some(Tag::Compound(mut target)) = components.remove("minecraft:lodestone_target")
    {
        let pos = target.remove("pos");
        let dimension = target.remove("dimension");
        if let (Some(pos), Some(dimension)) = (pos, dimension) {
            target.insert(
                "target",
                Compound::new()
                    .with("pos", pos)
                    .with("dimension", dimension),
            );
        }
        components.insert("minecraft:lodestone_tracker", target);
    }
    if fixer.due(3825, 0) {
        let expected = match id {
            "minecraft:white_banner" => Some(&["block.minecraft.ominous_banner"][..]),
            "minecraft:filled_map" => Some(&MAP_NAMES[..]),
            _ => None,
        };
        if let Some(expected) = expected
            && let Some(name) = components.get("minecraft:custom_name")
            && text::json_translation_key(name).is_some_and(|key| expected.contains(&key.as_str()))
        {
            rename(components, "minecraft:custom_name", "minecraft:item_name");
        }
    }
    if fixer.due(3945, 0)
        && let Some(modifiers) = components
            .get_compound_mut("minecraft:attribute_modifiers")
            .and_then(|c| c.get_list_mut("modifiers"))
    {
        *modifiers = entity::fix_modifier_ids(std::mem::take(modifiers));
    }
    if fixer.due(4054, 0) && id == "minecraft:white_banner" {
        fix_ominous_banner_rarity(components);
    }
    if fixer.due(4055, 0)
        && let Some(modifiers) = components
            .get_compound_mut("minecraft:attribute_modifiers")
            .and_then(|c| c.get_list_mut("modifiers"))
    {
        rename_modifier_types(modifiers, entity::strip_attribute_prefix);
    }
    if fixer.due(4059, 0) {
        fix_food(components);
    }
    if fixer.due(4064, 0) && components.remove("minecraft:fire_resistant").is_some() {
        components.insert(
            "minecraft:damage_resistant",
            Compound::new().with("types", "#minecraft:is_fire"),
        );
    }
    if fixer.due(4068, 0)
        && let Some(lock) = components.remove("minecraft:lock")
        && let Some(lock) = lock_predicate(lock)
    {
        components.insert("minecraft:lock", lock);
    }
    if fixer.due(4175, 0) {
        if let Some(equippable) = components.get_compound_mut("minecraft:equippable") {
            rename(equippable, "model", "asset_id");
        }
        if let Some(value) = components
            .get("minecraft:custom_model_data")
            .and_then(super::super::number)
        {
            components.insert(
                "minecraft:custom_model_data",
                Compound::new().with("floats", vec![Tag::Float(value as f32)]),
            );
        }
    }
    if fixer.due(4176, 0) && components.get("minecraft:lock").is_some_and(is_broken_lock) {
        components.remove("minecraft:lock");
    }
    text::upgrade_field(components, "minecraft:custom_name", fixer);
    text::upgrade_field(components, "minecraft:item_name", fixer);
    text::upgrade_list(components, "minecraft:lore", fixer);
    if let Some(book) = components.get_compound_mut("minecraft:written_book_content")
        && let Some(pages) = book.get_list_mut("pages")
    {
        for page in pages {
            match page {
                Tag::Compound(filtered) => {
                    text::upgrade_field(filtered, "raw", fixer);
                    text::upgrade_field(filtered, "filtered", fixer);
                }
                other => text::upgrade(other, fixer),
            }
        }
    }
    if let Some(lock) = components.get_compound_mut("minecraft:lock")
        && let Some(predicate) = lock.get_compound_mut(COMPONENTS)
    {
        text::upgrade_field(predicate, "minecraft:custom_name", fixer);
    }
    if fixer.due(4299, 0) {
        fix_bucket_variants(components, id);
    }
    if fixer.due(4307, 0) {
        fix_tooltip_display(components);
    }
    if fixer.due(4888, 0)
        && let Some(modifiers) = components.get_list_mut("minecraft:attribute_modifiers")
    {
        rename_modifier_types(modifiers, |name| {
            (name == "minecraft:nameplate_distance").then(|| "minecraft:name_tag_distance".into())
        });
    }
    fix_pot_decorations(
        components,
        "minecraft:pot_decorations",
        fixer,
        fixer.due(4996, 0),
    );
    if fixer.due(4649, 0)
        && let Some(consumable) = components.get_compound_mut("minecraft:consumable")
        && consumable.get_str("animation") == Some("spear")
    {
        consumable.insert("animation", "trident");
    }
    if fixer.due(5007, 0)
        && let Some(animation) = components.remove("minecraft:swing_animation")
    {
        components.insert("minecraft:attack_animation", animation.clone());
        components.insert("minecraft:interact_animation", animation);
    }
    if fixer.due(5008, 0) {
        components.remove("minecraft:map_color");
    }
    if fixer.due(5014, 0) {
        components.remove("minecraft:block_transformer");
    }
    for key in ["minecraft:can_break", "minecraft:can_place_on"] {
        rename_predicate_blocks(components, key, fixer);
    }
}

pub(super) fn rename_predicate_blocks(components: &mut Compound, key: &str, fixer: Fixer) {
    let rename_blocks = |predicate: &mut Compound| match predicate.get_mut("blocks") {
        Some(Tag::String(name)) => rename_block_name(name, fixer),
        Some(Tag::List(names)) => {
            for name in names.iter_mut() {
                if let Tag::String(name) = name {
                    rename_block_name(name, fixer);
                }
            }
        }
        _ => {}
    };
    match components.get_mut(key) {
        Some(Tag::Compound(predicate)) => rename_blocks(predicate),
        Some(Tag::List(predicates)) => {
            for predicate in predicates.iter_mut() {
                if let Tag::Compound(predicate) = predicate {
                    rename_blocks(predicate);
                }
            }
        }
        _ => {}
    }
}

pub(super) const POT_SIDES: [&str; 4] = ["back", "left", "right", "front"];

/// Pot decorations at `key`: a list of up to four item ids became one item
/// template per side in 4996 (4996.1 for the block entity), missing sides
/// filled with bricks. The sides are items from then on.
pub(crate) fn fix_pot_decorations(holder: &mut Compound, key: &str, fixer: Fixer, unpack: bool) {
    if unpack && let Some(Tag::List(ids)) = holder.get(key) {
        let mut sides = Compound::with_capacity(POT_SIDES.len());
        for (index, side) in POT_SIDES.iter().enumerate() {
            let id = ids.get(index).map_or("minecraft:brick", |id| {
                id.as_str().unwrap_or("minecraft:brick")
            });
            if id.is_empty() {
                return;
            }
            sides.insert(*side, Compound::new().with("id", id));
        }
        holder.insert(key, sides);
    }
    if let Some(inner) = fixer.nested()
        && let Some(sides) = holder.get_compound_mut(key)
    {
        for side in POT_SIDES {
            if let Some(Tag::Compound(item)) = sides.get_mut(side) {
                upgrade(item, inner.at_least(4996));
            }
        }
    }
}

pub(super) fn rename_modifier_types(
    modifiers: &mut [Tag],
    rename: impl Fn(&str) -> Option<String>,
) {
    for modifier in modifiers {
        if let Some(modifier) = modifier.as_compound_mut()
            && let Some(Tag::String(kind)) = modifier.get_mut("type")
            && let Some(new) = rename(kind)
        {
            *kind = new;
        }
    }
}

pub(super) fn fix_ominous_banner_rarity(components: &mut Compound) {
    let ominous = components
        .get("minecraft:item_name")
        .and_then(text::json_translation_key)
        .is_some_and(|key| key == "block.minecraft.ominous_banner");
    if ominous {
        components.insert(
            "minecraft:item_name",
            text::json_translate("block.minecraft.ominous_banner"),
        );
        components.insert("minecraft:rarity", "uncommon");
    }
}

pub(super) fn fix_food(components: &mut Compound) {
    let Some(Tag::Compound(mut food)) = components.remove("minecraft:food") else {
        return;
    };
    let seconds = food
        .remove("eat_seconds")
        .as_ref()
        .and_then(super::super::number)
        .map_or(1.6, |v| v as f32);
    let effects: Vec<Tag> = match food.remove("effects") {
        Some(Tag::List(effects)) => effects
            .iter()
            .filter_map(Tag::as_compound)
            .map(|effect| {
                Tag::Compound(
                    Compound::new()
                        .with("type", "minecraft:apply_effects")
                        .with(
                            "effects",
                            effect
                                .get("effect")
                                .cloned()
                                .into_iter()
                                .collect::<Vec<_>>(),
                        )
                        .with(
                            "probability",
                            get_number(effect, "probability").map_or(1.0, |v| v as f32),
                        ),
                )
            })
            .collect(),
        _ => Vec::new(),
    };
    if let Some(remainder) = food.remove("using_converts_to") {
        components.insert("minecraft:use_remainder", remainder);
    }
    components.insert("minecraft:food", food);
    components.insert(
        "minecraft:consumable",
        Compound::new()
            .with("consume_seconds", seconds)
            .with("on_consume_effects", effects),
    );
}

/// A legacy lock name as an item predicate on the key's custom name.
pub(crate) fn lock_predicate(lock: Tag) -> Option<Tag> {
    let Tag::String(name) = lock else {
        return None;
    };
    if name.is_empty() {
        return None;
    }
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    Some(Tag::Compound(Compound::new().with(
        COMPONENTS,
        Compound::new().with("minecraft:custom_name", format!("\"{escaped}\"")),
    )))
}

pub(crate) fn is_broken_lock(lock: &Tag) -> bool {
    let Some(lock) = lock.as_compound() else {
        return false;
    };
    lock.len() == 1
        && lock.get_compound(COMPONENTS).is_some_and(|components| {
            components.len() == 1 && components.get_str("minecraft:custom_name") == Some("\"\"")
        })
}

pub(super) fn fix_bucket_variants(components: &mut Compound, id: &str) {
    match id {
        "minecraft:salmon_bucket" => {
            if let Some(kind) = bucket_data(components).and_then(|data| data.remove("type")) {
                components.insert("minecraft:salmon/size", kind);
            }
        }
        "minecraft:axolotl_bucket" => {
            let variant = bucket_data(components)
                .and_then(|data| data.remove("Variant"))
                .as_ref()
                .and_then(super::super::number);
            if let Some(variant) = variant {
                let name = match variant as i32 {
                    1 => "wild",
                    2 => "gold",
                    3 => "cyan",
                    4 => "blue",
                    _ => "lucy",
                };
                components.insert("minecraft:axolotl/variant", name);
            }
        }
        "minecraft:tropical_fish_bucket" => {
            let packed = bucket_data(components)
                .and_then(|data| data.remove("BucketVariantTag"))
                .as_ref()
                .and_then(super::super::number);
            if let Some(packed) = packed {
                let packed = packed as i32;
                components.insert(
                    "minecraft:tropical_fish/pattern",
                    entity::tropical_pattern(packed),
                );
                components.insert(
                    "minecraft:tropical_fish/base_color",
                    dye_color_name((packed >> 16) & 255),
                );
                components.insert(
                    "minecraft:tropical_fish/pattern_color",
                    dye_color_name((packed >> 24) & 255),
                );
            }
        }
        "minecraft:painting" => {
            let Some(Tag::Compound(mut data)) = components.remove("minecraft:entity_data") else {
                return;
            };
            if id_of(&data) != "minecraft:painting" {
                components.insert("minecraft:entity_data", data);
                return;
            }
            let variant = data.remove("variant");
            let mut rest = data.clone();
            rest.remove("id");
            if !rest.is_empty() {
                components.insert("minecraft:entity_data", data);
            }
            if let Some(variant) = variant {
                components.insert("minecraft:painting/variant", variant);
            }
        }
        _ => {}
    }
}

pub(super) fn bucket_data(components: &mut Compound) -> Option<&mut Compound> {
    components.get_compound_mut("minecraft:bucket_entity_data")
}

pub(super) fn fix_tooltip_display(components: &mut Compound) {
    let mut hidden: Vec<&'static str> = Vec::new();
    for key in ["minecraft:can_place_on", "minecraft:can_break"] {
        if let Some(Tag::Compound(mut component)) = components.remove(key) {
            match component.remove("predicates") {
                Some(predicates) => {
                    if get_bool(&component, "show_in_tooltip") == Some(false) {
                        hidden.push(key);
                    }
                    components.insert(key, predicates);
                }
                None => {
                    components.insert(key, component);
                }
            }
        }
    }
    for (key, unwrap) in [
        ("minecraft:trim", None),
        ("minecraft:unbreakable", None),
        ("minecraft:dyed_color", Some("rgb")),
        ("minecraft:attribute_modifiers", Some("modifiers")),
        ("minecraft:enchantments", Some("levels")),
        ("minecraft:stored_enchantments", Some("levels")),
        ("minecraft:jukebox_playable", Some("song")),
    ] {
        let Some(Tag::Compound(mut component)) = components.remove(key) else {
            continue;
        };
        if get_bool(&component, "show_in_tooltip") == Some(false) {
            hidden.push(key);
        }
        component.remove("show_in_tooltip");
        let value = match unwrap.and_then(|field| component.get(field)) {
            Some(inner) => inner.clone(),
            None => Tag::Compound(component),
        };
        components.insert(key, value);
    }
    let hide_tooltip = components.remove("minecraft:hide_tooltip").is_some();
    if components
        .remove("minecraft:hide_additional_tooltip")
        .is_some()
    {
        for key in ADDITIONAL_TOOLTIP_COMPONENTS {
            if components.contains_key(key) && !hidden.contains(&key) {
                hidden.push(key);
            }
        }
    }
    if hidden.is_empty() && !hide_tooltip {
        return;
    }
    components.insert(
        "minecraft:tooltip_display",
        Compound::new().with("hide_tooltip", hide_tooltip).with(
            "hidden_components",
            hidden.into_iter().map(Tag::from).collect::<Vec<_>>(),
        ),
    );
}

pub(super) fn fix_explorer_map(item: &mut Compound) {
    if id_of(item) != "minecraft:filled_map" {
        return;
    }
    let Some(components) = item.get_compound(COMPONENTS) else {
        return;
    };
    let Some(kind) = components
        .get_compound("minecraft:map_decorations")
        .and_then(|d| d.get_compound("+"))
        .and_then(|d| d.get_str("type"))
    else {
        return;
    };
    let kind = namespaced(kind);
    let Some(&(_, new_id, legacy_name)) = EXPLORER_MAPS.iter().find(|(t, _, _)| *t == kind) else {
        return;
    };
    item.insert("id", new_id);
    if let Some(components) = item.get_compound_mut(COMPONENTS)
        && components
            .get("minecraft:item_name")
            .and_then(text::plain_translation_key)
            == Some(legacy_name)
    {
        components.remove("minecraft:item_name");
    }
}

/// Renames a block id (a block name, or a flat block state string such as
/// `name[props]{nbt}`) through the block renames due for `fixer`.
pub(crate) fn rename_block_name(name: &mut String, fixer: Fixer) {
    let end = name
        .find(['[', '{'])
        .filter(|&i| i > 0)
        .unwrap_or(name.len());
    let (block, rest) = name.split_at(end);
    let mut block = block.to_owned();
    for (version, table) in LEGACY_BLOCK_RENAMES {
        if fixer.due(*version, 0)
            && let Some(new) = renamed(&block, table)
        {
            block = new.to_owned();
        }
    }
    if fixer.due(3692, 0)
        && let Some(new) = renamed(&block, GRASS_RENAMES)
    {
        block = new.to_owned();
    }
    if fixer.due(4541, 0)
        && let Some(new) = renamed(&block, CHAIN_RENAMES)
    {
        block = new.to_owned();
    }
    if block != name[..end] {
        *name = format!("{block}{rest}");
    }
}
