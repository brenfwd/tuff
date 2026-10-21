use super::*;

fn chunk(version: i32, status: &str) -> Compound {
    Compound::new()
        .with("DataVersion", version)
        .with("xPos", 0)
        .with("zPos", 0)
        .with("Status", status)
        .with("blending_data", Compound::new().with("old_noise", true))
}

fn blending(root: &Compound) -> Option<(i32, i32)> {
    let data = root.get_compound("blending_data")?;
    Some((data.get_int("min_section")?, data.get_int("max_section")?))
}

#[tuff_core::watchdog]
#[test]
fn overworld_chunks_blend_over_the_height_they_were_generated_with() {
    let mut full = chunk(3100, "minecraft:full");
    chunk_in(&mut full, -4, true).unwrap();
    assert_eq!(blending(&full), Some((-4, 20)));
    assert!(
        !full
            .get_compound("blending_data")
            .unwrap()
            .contains_key("old_noise")
    );

    let mut retrogen = chunk(3100, "minecraft:biomes");
    retrogen.insert(
        "below_zero_retrogen",
        Compound::new().with("target_status", "minecraft:full"),
    );
    chunk_in(&mut retrogen, -4, true).unwrap();
    assert_eq!(blending(&retrogen), Some((0, 16)));

    let mut empty = chunk(3100, "minecraft:empty");
    chunk_in(&mut empty, -4, true).unwrap();
    assert!(!empty.contains_key("blending_data"));
}

#[tuff_core::watchdog]
#[test]
fn chunks_outside_the_overworld_lose_blending_data() {
    for version in [3100, 4500] {
        let mut nether = chunk(version, "minecraft:full");
        chunk_in(&mut nether, 0, false).unwrap();
        assert!(!nether.contains_key("blending_data"), "{version}");
    }
    let mut recent = chunk(4500, "minecraft:full");
    chunk_in(&mut recent, -4, true).unwrap();
    assert_eq!(blending(&recent), Some((-4, 20)));
}

#[tuff_core::watchdog]
#[test]
fn blending_heights_become_floats() {
    let mut root = chunk(5008, "minecraft:full");
    root.insert(
        "blending_data",
        Compound::new()
            .with("min_section", -4)
            .with("max_section", 20)
            .with(
                "heights",
                vec![Tag::Double(70.5), Tag::Double(f64::MAX), Tag::from("x")],
            ),
    );
    chunk_in(&mut root, -4, true).unwrap();
    let heights = root
        .get_compound("blending_data")
        .and_then(|data| data.get_list("heights"))
        .unwrap();
    assert_eq!(
        heights,
        &[Tag::Float(70.5), Tag::Float(f32::MAX), Tag::Float(f32::MAX)][..]
    );
    assert_eq!(blending(&root), Some((-4, 20)));
}

fn pot_item(version: i32, decorations: Vec<Tag>) -> Compound {
    let mut item = Compound::new()
        .with("id", "minecraft:decorated_pot")
        .with("count", 1)
        .with(
            "components",
            Compound::new().with("minecraft:pot_decorations", decorations),
        );
    item::upgrade(&mut item, Fixer::new(version));
    item
}

fn side(sides: &Compound, name: &str) -> String {
    sides
        .get_compound(name)
        .and_then(|side| side.get_str("id"))
        .unwrap_or_default()
        .to_owned()
}

#[tuff_core::watchdog]
#[test]
fn pot_decorations_become_one_template_per_side() {
    let item = pot_item(
        4995,
        vec![
            Tag::from("minecraft:angler_pottery_sherd"),
            Tag::from("minecraft:trial_explorer_map"),
        ],
    );
    let sides = components(&item)
        .get_compound("minecraft:pot_decorations")
        .unwrap();
    assert_eq!(side(sides, "back"), "minecraft:angler_pottery_sherd");
    assert_eq!(side(sides, "left"), "minecraft:buried_trial_chambers_map");
    assert_eq!(side(sides, "right"), "minecraft:brick");
    assert_eq!(side(sides, "front"), "minecraft:brick");

    let blank = pot_item(4995, vec![Tag::from(""), Tag::from("minecraft:brick")]);
    assert!(
        components(&blank)
            .get_list("minecraft:pot_decorations")
            .is_some()
    );
}

#[tuff_core::watchdog]
#[test]
fn pot_block_entity_sherds_are_unflattened_after_the_component() {
    for version in [4995, 4996] {
        let mut pot = Compound::new().with("id", "minecraft:decorated_pot").with(
            "sherds",
            vec![
                Tag::from("minecraft:brick"),
                Tag::from("minecraft:arms_up_pottery_sherd"),
                Tag::from("minecraft:brick"),
                Tag::from("minecraft:skull_pottery_sherd"),
            ],
        );
        block_entity::upgrade(&mut pot, Fixer::new(version));
        let sides = pot.get_compound("sherds").expect("{version}");
        assert_eq!(side(sides, "left"), "minecraft:arms_up_pottery_sherd");
        assert_eq!(side(sides, "front"), "minecraft:skull_pottery_sherd");
    }
}

#[tuff_core::watchdog]
#[test]
fn hover_items_get_item_fixes() {
    let mut sign = Compound::new().with("id", "minecraft:oak_sign").with(
        "front_text",
        Compound::new().with(
            "messages",
            vec![Tag::Compound(
                Compound::new().with("text", "map").with(
                    "hover_event",
                    Compound::new()
                        .with("action", "show_item")
                        .with("id", "minecraft:trial_explorer_map")
                        .with("count", 2),
                ),
            )],
        ),
    );
    block_entity::upgrade(&mut sign, Fixer::new(4300));
    let hover = sign
        .get_compound("front_text")
        .and_then(|side| side.get_list("messages"))
        .and_then(|messages| messages[0].as_compound())
        .and_then(|message| message.get_compound("hover_event"))
        .unwrap();
    assert_eq!(
        hover.get_str("id"),
        Some("minecraft:buried_trial_chambers_map")
    );
    assert_eq!(hover.get_int("count"), Some(2));

    let mut name = Tag::String(
        r#"{"text":"a","hoverEvent":{"action":"show_item","contents":{"id":"minecraft:woodland_explorer_map"}},"extra":[{"text":"b","hoverEvent":{"action":"show_entity","contents":{"type":"minecraft:pig","id":"x","name":{"text":"p","bold":"true"}}}}]}"#
            .into(),
    );
    text::upgrade(&mut name, Fixer::new(4189));
    let name = name.as_compound().unwrap();
    let hover = name.get_compound("hover_event").unwrap();
    assert_eq!(hover.get_str("id"), Some("minecraft:woodland_mansion_map"));
    let entity = name.get_list("extra").unwrap()[0]
        .as_compound()
        .and_then(|extra| extra.get_compound("hover_event"))
        .unwrap();
    assert_eq!(entity.get_str("id"), Some("minecraft:pig"));
    assert_eq!(
        entity
            .get_compound("name")
            .and_then(|name| name.get_bool("bold")),
        Some(true)
    );
}

#[tuff_core::watchdog]
#[test]
fn furnace_recipes_used_follow_recipe_renames() {
    let mut furnace = Compound::new().with("id", "minecraft:furnace").with(
        "RecipesUsed",
        Compound::new()
            .with("minecraft:quartz_slab_from_stonecutting", 2)
            .with("minecraft:iron_ingot", 3),
    );
    block_entity::upgrade(&mut furnace, Fixer::new(4700));
    let used = furnace.get_compound("RecipesUsed").unwrap();
    assert_eq!(
        used.get_int("minecraft:quartz_slab_from_quartz_block_stonecutting"),
        Some(2)
    );
    assert_eq!(used.get_int("minecraft:iron_ingot"), Some(3));
    assert_eq!(used.len(), 2);
}

#[tuff_core::watchdog]
#[test]
fn structure_templates_are_upgraded() {
    let state = |name: &str| Tag::Compound(Compound::new().with("Name", name));
    let mut root = Compound::new()
        .with("DataVersion", 3337)
        .with("palette", vec![state("minecraft:grass")])
        .with("palettes", vec![Tag::List(vec![state("minecraft:chain")])])
        .with(
            "blocks",
            vec![Tag::Compound(
                Compound::new().with(
                    "nbt",
                    Compound::new()
                        .with("id", "minecraft:sign")
                        .with("Text1", text::json_text("hi")),
                ),
            )],
        )
        .with(
            "entities",
            vec![Tag::Compound(
                Compound::new().with(
                    "nbt",
                    Compound::new()
                        .with("id", "minecraft:pig")
                        .with("CustomName", text::json_text("Bacon")),
                ),
            )],
        );
    structure(&mut root).unwrap();
    assert_eq!(root.get_int("DataVersion"), Some(DATA_VERSION));
    let palette = root.get_list("palette").unwrap();
    assert_eq!(
        palette[0].as_compound().and_then(|s| s.get_str("id")),
        Some("minecraft:short_grass")
    );
    let palettes = root.get_list("palettes").unwrap()[0].as_list().unwrap();
    assert_eq!(
        palettes[0].as_compound().and_then(|s| s.get_str("id")),
        Some("minecraft:iron_chain")
    );
    let block = root.get_list("blocks").unwrap()[0]
        .as_compound()
        .and_then(|b| b.get_compound("nbt"))
        .unwrap();
    assert!(block.contains_key("front_text"));
    let entity = root.get_list("entities").unwrap()[0]
        .as_compound()
        .and_then(|e| e.get_compound("nbt"))
        .unwrap();
    assert_eq!(
        entity
            .get_compound("CustomName")
            .and_then(|name| name.get_str("text")),
        Some("Bacon")
    );

    let mut current = Compound::new().with("palette", vec![state("minecraft:grass")]);
    structure(&mut current).unwrap();
    assert!(!current.contains_key("DataVersion"));
}

#[tuff_core::watchdog]
#[test]
fn overworld_shaped_custom_dimensions_lose_blending_data() {
    let mut custom = chunk(3100, "minecraft:full");
    chunk_in(&mut custom, -4, false).unwrap();
    assert!(!custom.contains_key("blending_data"));
}

#[tuff_core::watchdog]
#[test]
fn legacy_map_dimension_ids_become_names() {
    for (old, new) in [
        (-1, "minecraft:the_nether"),
        (0, "minecraft:overworld"),
        (1, "minecraft:the_end"),
        (7, "minecraft:overworld"),
    ] {
        let mut root = Compound::new()
            .with("DataVersion", 2500)
            .with("data", Compound::new().with("dimension", old));
        crate::datafix::level::saved_data("map_1", &mut root).unwrap();
        let data = root.get_compound("data").unwrap();
        assert_eq!(data.get_str("dimension"), Some(new));
    }
}

#[tuff_core::watchdog]
#[test]
fn pre_1911_chunk_statuses_are_renamed() {
    for (old, new) in [
        ("postprocessed", "full"),
        ("liquid_carved", "liquid_carvers"),
        ("mobs_spawned", "spawn"),
        ("biomes", "empty"),
        ("base", "surface"),
    ] {
        let mut root = Compound::new().with(
            "Level",
            Compound::new()
                .with("Status", old)
                .with("Sections", Vec::<Tag>::new()),
        );
        crate::datafix::legacy::upgrade(&mut root, Fixer::new(1600), -4);
        assert_eq!(root.get_str("Status"), Some(new), "{old}");
    }
}

#[tuff_core::watchdog]
#[test]
fn play_one_minute_becomes_play_time() {
    let mut stats = Compound::new().with("DataVersion", 2700).with(
        "stats",
        Compound::new().with(
            "minecraft:custom",
            Compound::new().with("minecraft:play_one_minute", 5),
        ),
    );
    level::stats(&mut stats).unwrap();
    let custom = stats
        .get_compound("stats")
        .and_then(|s| s.get_compound("minecraft:custom"))
        .unwrap();
    assert_eq!(custom.get_int("minecraft:play_time"), Some(5));
    assert!(!custom.contains_key("minecraft:play_one_minute"));

    let mut board = Compound::new().with("DataVersion", 2700).with(
        "data",
        Compound::new().with(
            "Objectives",
            vec![
                Compound::new().with(
                    "CriteriaType",
                    Compound::new()
                        .with("type", "minecraft:custom")
                        .with("id", "minecraft:play_one_minute"),
                ),
            ],
        ),
    );
    level::saved_data("minecraft:scoreboard", &mut board).unwrap();
    let id = board
        .get_compound("data")
        .unwrap()
        .get_list("Objectives")
        .unwrap()[0]
        .as_compound()
        .and_then(|o| o.get_compound("CriteriaType"))
        .and_then(|c| c.get_str("id"))
        .unwrap();
    assert_eq!(id, "minecraft:play_time");
}

#[tuff_core::watchdog]
#[test]
fn consumable_spear_animation_becomes_trident() {
    let mut item = Compound::new()
        .with("id", "minecraft:trident")
        .with("count", 1)
        .with(
            "components",
            Compound::new().with(
                "minecraft:consumable",
                Compound::new().with("animation", "spear"),
            ),
        );
    item::upgrade(&mut item, Fixer::new(4648));
    let animation = item
        .get_compound("components")
        .and_then(|c| c.get_compound("minecraft:consumable"))
        .and_then(|c| c.get_str("animation"));
    assert_eq!(animation, Some("trident"));
}

#[tuff_core::watchdog]
#[test]
fn goat_horn_sound_variant_becomes_instrument() {
    let horn = |variant: i32| {
        let mut item = Compound::new()
            .with("id", "minecraft:goat_horn")
            .with("Count", 1i8)
            .with("tag", Compound::new().with("SoundVariant", variant));
        item::upgrade(&mut item, Fixer::new(3093));
        item
    };
    let instrument = |item: &Compound| {
        components(item)
            .get_str("minecraft:instrument")
            .map(str::to_owned)
    };
    assert_eq!(
        instrument(&horn(2)).as_deref(),
        Some("minecraft:seek_goat_horn")
    );
    assert_eq!(
        instrument(&horn(99)).as_deref(),
        Some("minecraft:ponder_goat_horn")
    );
}

#[tuff_core::watchdog]
#[test]
fn command_block_last_output_and_thrown_trident_item_are_upgraded() {
    let mut be = Compound::new()
        .with("id", "minecraft:command_block")
        .with("LastOutput", text::json_text("done"));
    block_entity::upgrade(&mut be, Fixer::new(3000));
    assert_eq!(
        be.get_compound("LastOutput")
            .and_then(|t| t.get_str("text")),
        Some("done")
    );

    let mut trident = Compound::new().with("id", "minecraft:trident").with(
        "Trident",
        Compound::new()
            .with("id", "minecraft:grass")
            .with("Count", 1i8),
    );
    entity::upgrade(&mut trident, Fixer::new(3000));
    assert_eq!(
        trident
            .get_compound("Trident")
            .and_then(|t| t.get_str("id")),
        Some("minecraft:short_grass")
    );
}

#[tuff_core::watchdog]
#[test]
fn empty_second_trade_cost_is_removed() {
    let offer = |buy_b: Compound| {
        Compound::new()
            .with(
                "buy",
                Compound::new()
                    .with("id", "minecraft:emerald")
                    .with("count", 1),
            )
            .with("buyB", buy_b)
            .with(
                "sell",
                Compound::new()
                    .with("id", "minecraft:bread")
                    .with("count", 1),
            )
    };
    let mut villager = Compound::new().with("id", "minecraft:villager").with(
        "Offers",
        Compound::new().with(
            "Recipes",
            vec![
                offer(Compound::new().with("id", "minecraft:air").with("count", 1)),
                offer(
                    Compound::new()
                        .with("id", "minecraft:stick")
                        .with("count", 0),
                ),
                offer(
                    Compound::new()
                        .with("id", "minecraft:stick")
                        .with("count", 2),
                ),
            ],
        ),
    );
    entity::upgrade(&mut villager, Fixer::new(3827));
    let recipes = villager
        .get_compound("Offers")
        .unwrap()
        .get_list("Recipes")
        .unwrap();
    let has_buy_b = |i: usize| recipes[i].as_compound().unwrap().contains_key("buyB");
    assert!(!has_buy_b(0));
    assert!(!has_buy_b(1));
    assert!(has_buy_b(2));
}
