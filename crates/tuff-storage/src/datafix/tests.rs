use super::*;
use tuff_core::protocol::play::GameMode;
use tuff_core::storage::player_data::{
    ExperienceSave, FoodSave, PlayerRecord, PlayerState, RecipeBookSave,
};
use tuff_core::world::effects::ActiveEffects;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::ChunkPos;

mod legacy;
mod pre2860;
mod recent;
mod world;

/// 1.20.1.
const V1_20_1: i32 = 3465;
/// 1.20.4.
const V1_20_4: i32 = 3700;
/// 1.21.4.
const V1_21_4: i32 = 4189;

fn json(text: &str) -> Tag {
    Tag::String(text::json_text(text))
}

fn legacy_item(id: &str, count: i8, tag: Option<Compound>) -> Compound {
    let mut item = Compound::new().with("id", id).with("Count", count);
    if let Some(tag) = tag {
        item.insert("tag", tag);
    }
    item
}

fn floats(values: &[f32]) -> Tag {
    Tag::List(values.iter().map(|v| Tag::Float(*v)).collect())
}

fn components(item: &Compound) -> &Compound {
    item.get_compound("components").expect("components")
}

fn player_defaults() -> PlayerState {
    PlayerState {
        dimension: "minecraft:overworld".into(),
        position: [0.5, 70.0, 0.5],
        yaw: 0.0,
        pitch: 0.0,
        on_ground: true,
        fall_distance: 0.0,
        fire: 0,
        air: 300,
        ticks_frozen: 0,
        score: 0,
        portal_cooldown: 0,
        seen_credits: false,
        warden_tracker: Default::default(),
        health: 20.0,
        food: FoodSave {
            level: 20,
            saturation: 5.0,
            exhaustion: 0.0,
            timer: 0,
        },
        selected: 0,
        slots: Vec::new(),
        ender_items: Vec::new(),
        game_mode: GameMode::Survival,
        previous_game_mode: None,
        flying: false,
        recipe_book: RecipeBookSave::default(),
        experience: ExperienceSave::default(),
        respawn: None,
        last_death: None,
        entered_nether: None,
        post_effects: Vec::new(),
        shoulders: [None, None],
        effects: ActiveEffects::default(),
        attributes: Default::default(),
    }
}

#[tuff_core::watchdog]
#[test]
fn sub_versions_run_after_their_data_version() {
    let fixer = Fixer::new(3818);
    assert!(!fixer.due(3818, 0));
    assert!(fixer.due(3818, 1));
    assert!(fixer.due(3819, 0));
    assert!(!fixer.due(3700, 0));
    assert!(!Fixer::new(DATA_VERSION).pending());
}

#[tuff_core::watchdog]
#[test]
fn legacy_item_is_componentized() {
    let mut item = legacy_item(
        "minecraft:diamond_sword",
        1,
        Some(
            Compound::new()
                .with("Damage", 12)
                .with("RepairCost", 3)
                .with("Unbreakable", 1i8)
                .with("HideFlags", 1 | 4)
                .with(
                    "Enchantments",
                    vec![
                        Compound::new()
                            .with("id", "minecraft:sharpness")
                            .with("lvl", 5i16),
                        Compound::new()
                            .with("id", "minecraft:sweeping")
                            .with("lvl", 2i16),
                    ],
                )
                .with(
                    "display",
                    Compound::new()
                        .with("Name", Tag::String("{\"text\":\"Blade\"}".into()))
                        .with("Lore", vec![json("one"), Tag::from("not json {")]),
                )
                .with("CustomModelData", 7)
                .with("custom", "kept"),
        ),
    );
    item::upgrade(&mut item, Fixer::new(V1_20_4));
    assert_eq!(item.get_str("id"), Some("minecraft:diamond_sword"));
    assert_eq!(item.get_int("count"), Some(1));
    assert!(!item.contains_key("Count") && !item.contains_key("tag"));
    let c = components(&item);
    assert_eq!(c.get_int("minecraft:damage"), Some(12));
    assert_eq!(c.get_int("minecraft:repair_cost"), Some(3));
    let levels = c.get_compound("minecraft:enchantments").unwrap();
    assert_eq!(levels.get_int("minecraft:sharpness"), Some(5));
    assert_eq!(levels.get_int("minecraft:sweeping_edge"), Some(2));
    assert_eq!(
        c.get_compound("minecraft:custom_name")
            .unwrap()
            .get_str("text"),
        Some("Blade")
    );
    let lore = c.get_list("minecraft:lore").unwrap();
    assert_eq!(lore.len(), 1);
    assert_eq!(lore[0].as_compound().unwrap().get_str("text"), Some("one"));
    assert_eq!(
        c.get_compound("minecraft:custom_model_data")
            .unwrap()
            .get_list("floats"),
        Some(&[Tag::Float(7.0)][..])
    );
    assert_eq!(
        c.get_compound("minecraft:custom_data")
            .unwrap()
            .get_str("custom"),
        Some("kept")
    );
    assert!(c.get_compound("minecraft:unbreakable").unwrap().is_empty());
    let display = c.get_compound("minecraft:tooltip_display").unwrap();
    let hidden: Vec<&str> = display
        .get_list("hidden_components")
        .unwrap()
        .iter()
        .filter_map(Tag::as_str)
        .collect();
    assert!(hidden.contains(&"minecraft:unbreakable"));
    assert!(hidden.contains(&"minecraft:enchantments"));
    assert_eq!(display.get_bool("hide_tooltip"), Some(false));
}

#[tuff_core::watchdog]
#[test]
fn upgraded_item_parses_as_a_stack() {
    let mut item = legacy_item(
        "minecraft:iron_pickaxe",
        1,
        Some(Compound::new().with("Damage", 30)),
    );
    item::upgrade(&mut item, Fixer::new(V1_20_4));
    let stack = tuff_item::item::ItemStack::mint_from_nbt(&item, tuff_item::item::MintReason::Test)
        .unwrap();
    assert_eq!(stack.item, tuff_vanilla_data::Item::IronPickaxe);
    assert_eq!(stack.count(), 1);
    assert_eq!(stack.damage(), 30);
}

#[tuff_core::watchdog]
#[test]
fn nested_container_items_are_upgraded() {
    let inner = legacy_item(
        "minecraft:grass",
        16,
        Some(Compound::new().with("display", Compound::new().with("Name", json("Inner")))),
    );
    let mut shulker = legacy_item(
        "minecraft:shulker_box",
        1,
        Some(Compound::new().with(
            "BlockEntityTag",
            Compound::new().with("Items", vec![inner.with("Slot", 3i8)]),
        )),
    );
    item::upgrade(&mut shulker, Fixer::new(V1_20_1));
    let container = components(&shulker)
        .get_list("minecraft:container")
        .expect("container");
    let slot = container[0].as_compound().unwrap();
    assert_eq!(slot.get_int("slot"), Some(3));
    let inner = slot.get_compound("item").unwrap();
    assert_eq!(inner.get_str("id"), Some("minecraft:short_grass"));
    assert_eq!(inner.get_int("count"), Some(16));
    assert_eq!(
        components(inner)
            .get_compound("minecraft:custom_name")
            .unwrap()
            .get_str("text"),
        Some("Inner")
    );
}

#[tuff_core::watchdog]
#[test]
fn special_items_get_their_components() {
    let mut book = legacy_item(
        "minecraft:written_book",
        1,
        Some(
            Compound::new()
                .with("title", "Tale")
                .with("author", "Steve")
                .with("pages", vec![json("page one")]),
        ),
    );
    item::upgrade(&mut book, Fixer::new(V1_20_4));
    let content = components(&book)
        .get_compound("minecraft:written_book_content")
        .unwrap();
    assert_eq!(
        content.get_compound("title").unwrap().get_str("raw"),
        Some("Tale")
    );
    assert_eq!(content.get_str("author"), Some("Steve"));
    let page = content.get_list("pages").unwrap()[0].as_compound().unwrap();
    assert_eq!(
        page.get_compound("raw").unwrap().get_str("text"),
        Some("page one")
    );

    let mut potion = legacy_item(
        "minecraft:potion",
        1,
        Some(Compound::new().with("Potion", "minecraft:swiftness")),
    );
    item::upgrade(&mut potion, Fixer::new(V1_20_4));
    assert_eq!(
        components(&potion)
            .get_compound("minecraft:potion_contents")
            .unwrap()
            .get_str("potion"),
        Some("minecraft:swiftness")
    );

    let mut head = legacy_item(
        "minecraft:player_head",
        1,
        Some(Compound::new().with("SkullOwner", "Notch")),
    );
    item::upgrade(&mut head, Fixer::new(V1_20_4));
    assert_eq!(
        components(&head)
            .get_compound("minecraft:profile")
            .unwrap()
            .get_str("name"),
        Some("Notch")
    );

    let mut sherd = legacy_item("minecraft:pottery_shard_archer", 2, None);
    item::upgrade(&mut sherd, Fixer::new(3400));
    assert_eq!(sherd.get_str("id"), Some("minecraft:archer_pottery_sherd"));
}

#[tuff_core::watchdog]
#[test]
fn attribute_modifiers_get_ids_and_lose_prefixes() {
    let speed_uuid = Tag::uuid(0xfa233e1c_4180_4865_b01b_bcce9785aca3);
    let mut item = legacy_item(
        "minecraft:stick",
        1,
        Some(Compound::new().with(
            "AttributeModifiers",
            vec![
                Compound::new()
                    .with("AttributeName", "minecraft:generic.attack_speed")
                    .with("UUID", speed_uuid)
                    .with("Name", "x")
                    .with("Amount", 2.0f64)
                    .with("Operation", 0),
            ],
        )),
    );
    item::upgrade(&mut item, Fixer::new(V1_20_4));
    let modifiers = components(&item)
        .get_list("minecraft:attribute_modifiers")
        .unwrap();
    let modifier = modifiers[0].as_compound().unwrap();
    assert_eq!(modifier.get_str("type"), Some("minecraft:attack_speed"));
    assert_eq!(modifier.get_str("id"), Some("minecraft:base_attack_speed"));
    assert_eq!(modifier.get_str("operation"), Some("add_value"));
    assert!(!modifier.contains_key("uuid"));
}

#[tuff_core::watchdog]
#[test]
fn food_becomes_consumable() {
    let mut item = Compound::new()
        .with("id", "minecraft:apple")
        .with("count", 1)
        .with(
            "components",
            Compound::new().with(
                "minecraft:food",
                Compound::new()
                    .with("nutrition", 4)
                    .with("eat_seconds", 3.2f32)
                    .with(
                        "using_converts_to",
                        Compound::new()
                            .with("id", "minecraft:bowl")
                            .with("count", 1),
                    ),
            ),
        );
    item::upgrade(&mut item, Fixer::new(3955));
    let c = components(&item);
    assert_eq!(
        c.get_compound("minecraft:consumable")
            .unwrap()
            .get_float("consume_seconds"),
        Some(3.2)
    );
    assert_eq!(
        c.get_compound("minecraft:use_remainder")
            .unwrap()
            .get_str("id"),
        Some("minecraft:bowl")
    );
    let food = c.get_compound("minecraft:food").unwrap();
    assert_eq!(food.get_int("nutrition"), Some(4));
    assert!(!food.contains_key("eat_seconds"));
}

#[tuff_core::watchdog]
#[test]
fn malformed_items_do_not_panic() {
    let shapes = [
        Compound::new().with("id", 5).with("Count", "x"),
        Compound::new()
            .with("id", "minecraft:stone")
            .with("Count", 1i8)
            .with("tag", 4),
        legacy_item(
            "minecraft:enchanted_book",
            1,
            Some(
                Compound::new()
                    .with("StoredEnchantments", 3)
                    .with("display", "nope")
                    .with("HideFlags", "x")
                    .with("BlockEntityTag", Compound::new().with("Items", 7))
                    .with("AttributeModifiers", vec![Tag::Int(1)]),
            ),
        ),
        Compound::new().with("components", Compound::new().with("minecraft:lore", 9)),
    ];
    for mut item in shapes {
        item::upgrade(&mut item, Fixer::new(V1_20_4));
    }
}

#[tuff_core::watchdog]
#[test]
fn deeply_nested_items_stop_at_the_depth_limit() {
    let mut item = legacy_item("minecraft:stone", 1, None);
    for _ in 0..100 {
        item = legacy_item(
            "minecraft:bundle",
            1,
            Some(Compound::new().with("Items", vec![item])),
        );
    }
    item::upgrade(&mut item, Fixer::new(V1_20_4));
    let mut depth = 0;
    let mut current = &item;
    while let Some(inner) = current
        .get_compound("components")
        .and_then(|c| c.get_list("minecraft:bundle_contents"))
        .and_then(|l| l.first())
        .and_then(Tag::as_compound)
    {
        depth += 1;
        current = inner;
    }
    assert_eq!(depth, usize::from(MAX_DEPTH) + 1);
    assert!(current.contains_key("Count"));
}

#[tuff_core::watchdog]
#[test]
fn text_components_accept_lenient_json() {
    let tag = text::unflatten(
        "{text: 'hi there', bold = true; extra: [{text:\"x\"},], /* c */ color => red}",
    );
    let component = tag.as_compound().unwrap();
    assert_eq!(component.get_str("text"), Some("hi there"));
    assert_eq!(component.get_bool("bold"), Some(true));
    assert_eq!(component.get_str("color"), Some("red"));
    assert_eq!(component.get_list("extra").unwrap().len(), 1);

    assert_eq!(text::unflatten("hello").as_str(), Some("hello"));
    assert_eq!(text::unflatten("hello world").as_str(), Some("hello world"));
    assert_eq!(text::unflatten("{text: ").as_str(), Some("{text: "));
}

#[tuff_core::watchdog]
#[test]
fn text_components_unflatten_and_rename_events() {
    let mut tag = Tag::String(
        r#"{"text":"hi","bold":"true","clickEvent":{"action":"run_command","value":"/say hi"},"hoverEvent":{"action":"show_text","contents":"tip"},"extra":[{"text":"x","clickEvent":{"action":"open_url","value":"ftp://bad"}}]}"#
            .into(),
    );
    text::upgrade(&mut tag, Fixer::new(V1_21_4));
    let component = tag.as_compound().unwrap();
    assert_eq!(component.get_bool("bold"), Some(true));
    let click = component.get_compound("click_event").unwrap();
    assert_eq!(click.get_str("command"), Some("/say hi"));
    assert_eq!(
        component
            .get_compound("hover_event")
            .unwrap()
            .get_str("value"),
        Some("tip")
    );
    let extra = component.get_list("extra").unwrap()[0]
        .as_compound()
        .unwrap();
    assert!(!extra.contains_key("click_event") && !extra.contains_key("clickEvent"));

    let mut plain = Tag::String("not json at all".into());
    text::upgrade(&mut plain, Fixer::new(V1_21_4));
    assert_eq!(plain.as_str(), Some("not json at all"));

    let deep = format!("{}{}", "[".repeat(500), "]".repeat(500));
    let mut deep_tag = Tag::String(deep.clone());
    text::upgrade(&mut deep_tag, Fixer::new(V1_21_4));
    assert_eq!(deep_tag.as_str(), Some(deep.as_str()));

    let mut legacy = Tag::String(
        r#"{"text":"a","hoverEvent":{"action":"show_item","value":"{id:stone}"}}"#.into(),
    );
    text::upgrade(&mut legacy, Fixer::new(V1_21_4));
    let hover = legacy
        .as_compound()
        .unwrap()
        .get_compound("hover_event")
        .unwrap();
    assert_eq!(hover.get_str("action"), Some("show_text"));
    assert!(
        hover
            .get_compound("value")
            .unwrap()
            .get_str("text")
            .unwrap()
            .starts_with("Legacy hoverEvent: ")
    );
}

fn legacy_chunk() -> Compound {
    let sign = Compound::new()
        .with("id", "minecraft:sign")
        .with("x", 1)
        .with("y", 64)
        .with("z", 1)
        .with(
            "front_text",
            Compound::new()
                .with(
                    "messages",
                    vec![json("Hello"), json(""), json(""), json("")],
                )
                .with("color", "red")
                .with("has_glowing_text", 1i8),
        );
    let furnace = Compound::new()
        .with("id", "minecraft:furnace")
        .with("x", 2)
        .with("y", 64)
        .with("z", 2)
        .with("BurnTime", 40i16)
        .with("CookTime", 5i16)
        .with("CookTimeTotal", 200i16);
    let chest = Compound::new()
        .with("id", "minecraft:chest")
        .with("x", 3)
        .with("y", 64)
        .with("z", 3)
        .with(
            "Items",
            vec![legacy_item("minecraft:scute", 5, None).with("Slot", 0i8)],
        );
    let bed = Compound::new()
        .with("id", "minecraft:bed")
        .with("x", 4)
        .with("y", 64)
        .with("z", 4);
    let section = Compound::new()
        .with("Y", -4i8)
        .with(
            "block_states",
            Compound::new().with(
                "palette",
                vec![Compound::new().with("Name", "minecraft:grass")],
            ),
        )
        .with("SkyLight", vec![0i8; 2048]);
    Compound::new()
        .with("DataVersion", V1_20_1)
        .with("xPos", 0)
        .with("zPos", 0)
        .with("yPos", -4)
        .with("Status", "minecraft:full")
        .with("isLightOn", 1i8)
        .with("sections", vec![section])
        .with("block_entities", vec![sign, furnace, chest, bed])
        .with(
            "block_ticks",
            vec![Compound::new().with("i", "minecraft:grass").with("t", 1)],
        )
}

#[tuff_core::watchdog]
#[test]
fn legacy_chunk_is_upgraded() {
    let mut root = legacy_chunk();
    chunk_in(&mut root, 0, false).unwrap();
    assert_eq!(root.get_int("DataVersion"), Some(DATA_VERSION));
    assert!(!root.contains_key("isLightOn"));
    let section = root.get_list("sections").unwrap()[0].as_compound().unwrap();
    assert!(!section.contains_key("SkyLight"));
    let state = section
        .get_compound("block_states")
        .unwrap()
        .get_list("palette")
        .unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(state.get_str("id"), Some("minecraft:short_grass"));
    let tick = root.get_list("block_ticks").unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(tick.get_str("i"), Some("minecraft:short_grass"));
    let entities = root.get_list("block_entities").unwrap();
    assert_eq!(entities.len(), 3);
    let by_id = |id: &str| {
        entities
            .iter()
            .filter_map(Tag::as_compound)
            .find(|be| be.get_str("id") == Some(id))
            .unwrap()
    };
    let sign = by_id("minecraft:sign");
    let front = sign.get_compound("front_text").unwrap();
    assert_eq!(
        front.get_list("messages").unwrap()[0]
            .as_compound()
            .unwrap()
            .get_str("text"),
        Some("Hello")
    );
    assert_eq!(front.get_str("color"), Some("red"));
    assert_eq!(sign.get_bool("allow_op_features"), Some(true));
    let furnace = by_id("minecraft:furnace");
    assert_eq!(furnace.get_short("lit_time_remaining"), Some(40));
    assert_eq!(furnace.get_short("lit_total_time"), Some(40));
    assert_eq!(furnace.get_short("cooking_total_time"), Some(200));
    let chest = by_id("minecraft:chest");
    let item = chest.get_list("Items").unwrap()[0].as_compound().unwrap();
    assert_eq!(item.get_str("id"), Some("minecraft:turtle_scute"));
    assert_eq!(item.get_int("count"), Some(5));
    assert_eq!(item.get_byte("Slot"), Some(0));
}

#[tuff_core::watchdog]
#[test]
fn one_sided_sign_becomes_double_sided() {
    let mut sign = Compound::new()
        .with("id", "minecraft:sign")
        .with("Text1", json("Top"))
        .with("Text3", json("Low"))
        .with("FilteredText1", json("T*p"))
        .with("GlowingText", 1i8);
    block_entity::upgrade(&mut sign, Fixer::new(3337));
    for key in [
        "Text1",
        "Text3",
        "FilteredText1",
        "GlowingText",
        "_filtered_correct",
    ] {
        assert!(!sign.contains_key(key), "{key} was kept");
    }
    let front = sign.get_compound("front_text").unwrap();
    let lines: Vec<&str> = front
        .get_list("messages")
        .unwrap()
        .iter()
        .map(|line| line.as_compound().unwrap().get_str("text").unwrap())
        .collect();
    assert_eq!(lines, ["Top", "", "Low", ""]);
    assert_eq!(front.get_str("color"), Some("black"));
    assert_eq!(front.get_bool("has_glowing_text"), Some(true));
    let filtered = front.get_list("filtered_messages").unwrap();
    assert_eq!(
        filtered[0].as_compound().unwrap().get_str("text"),
        Some("T*p")
    );
    assert_eq!(
        filtered[2].as_compound().unwrap().get_str("text"),
        Some("Low")
    );
    let back = sign.get_compound("back_text").unwrap();
    assert_eq!(back.get_list("messages").unwrap().len(), 4);
    assert_eq!(sign.get_bool("is_waxed"), Some(false));
}

#[tuff_core::watchdog]
#[test]
fn upgraded_chunk_decodes() {
    let decoded = tuff_core::storage::chunk_nbt::decode(
        legacy_chunk(),
        ChunkPos::new(0, 0),
        ChunkShape::OVERWORLD,
    )
    .unwrap();
    let tuff_world_model::chunk_meta::Decoded::Chunk(chunk, meta) = decoded else {
        panic!("expected a full chunk");
    };
    assert_eq!(
        chunk.get(0, -64, 0).block(),
        tuff_vanilla_data::Block::ShortGrass
    );
    assert!(meta.relight);
}

#[tuff_core::watchdog]
#[test]
fn chunks_before_1_13_are_refused_and_unversioned_ones_kept() {
    const V1_12_2: i32 = 1343;
    for version in [V1_12_2, MIN_DATA_VERSION - 1] {
        let mut old = Compound::new().with("DataVersion", version).with(
            "Level",
            Compound::new().with("Sections", Vec::<Compound>::new()),
        );
        let before = old.clone();
        assert_eq!(
            chunk_in(&mut old, 0, false),
            Err(DataFixError::TooOld(version))
        );
        assert_eq!(old, before);
    }
    let mut floor = Compound::new().with("DataVersion", MIN_DATA_VERSION);
    chunk_in(&mut floor, 0, false).unwrap();
    assert_eq!(floor.get_int("DataVersion"), Some(DATA_VERSION));
    let mut unversioned = Compound::new().with("Status", "minecraft:noise");
    let before = unversioned.clone();
    chunk_in(&mut unversioned, 0, false).unwrap();
    assert_eq!(unversioned, before);
}

#[tuff_core::watchdog]
#[test]
fn proto_chunk_statuses_are_merged() {
    let mut root = Compound::new()
        .with("DataVersion", 3463)
        .with("Status", "minecraft:noise")
        .with("Heightmaps", Compound::new())
        .with(
            "sections",
            vec![
                Compound::new()
                    .with("Y", 0i8)
                    .with("block_states", Compound::new()),
            ],
        );
    chunk_in(&mut root, 0, false).unwrap();
    assert_eq!(root.get_str("Status"), Some("minecraft:biomes"));
    assert!(!root.contains_key("Heightmaps"));
    let section = root.get_list("sections").unwrap()[0].as_compound().unwrap();
    assert!(!section.contains_key("block_states"));

    let mut carvers = Compound::new()
        .with("DataVersion", 3400)
        .with("Status", "minecraft:liquid_carvers");
    chunk_in(&mut carvers, 0, false).unwrap();
    assert_eq!(carvers.get_str("Status"), Some("minecraft:terrain"));
}

#[tuff_core::watchdog]
#[test]
fn mob_equipment_attributes_and_names_are_upgraded() {
    let zombie = Compound::new()
        .with("id", "minecraft:zombie")
        .with("CustomName", json("Bob"))
        .with("FallDistance", 1.5f32)
        .with(
            "HandItems",
            vec![
                legacy_item("minecraft:iron_sword", 1, None),
                Compound::new(),
            ],
        )
        .with(
            "ArmorItems",
            vec![
                Compound::new(),
                Compound::new(),
                Compound::new(),
                legacy_item("minecraft:iron_helmet", 1, None),
            ],
        )
        .with("ArmorDropChances", floats(&[0.085, 0.085, 0.085, 1.0]))
        .with("HandDropChances", floats(&[0.085, 0.085]))
        .with(
            "Attributes",
            vec![
                Compound::new()
                    .with("Name", "minecraft:generic.max_health")
                    .with("Base", 20.0f64),
            ],
        )
        .with(
            "ActiveEffects",
            vec![
                Compound::new()
                    .with("Id", 1)
                    .with("Duration", 100)
                    .with("Amplifier", 0i8),
            ],
        );
    let mut root = Compound::new()
        .with("DataVersion", V1_20_1)
        .with("Entities", vec![zombie]);
    entity_chunk(&mut root).unwrap();
    let zombie = root.get_list("Entities").unwrap()[0].as_compound().unwrap();
    assert_eq!(
        zombie.get_compound("CustomName").unwrap().get_str("text"),
        Some("Bob")
    );
    assert_eq!(zombie.get_double("fall_distance"), Some(1.5));
    let equipment = zombie.get_compound("equipment").unwrap();
    assert_eq!(
        equipment.get_compound("mainhand").unwrap().get_str("id"),
        Some("minecraft:iron_sword")
    );
    assert_eq!(
        equipment.get_compound("head").unwrap().get_int("count"),
        Some(1)
    );
    assert!(!equipment.contains_key("offhand"));
    assert_eq!(
        zombie
            .get_compound("drop_chances")
            .unwrap()
            .get_float("head"),
        Some(1.0)
    );
    let attribute = zombie.get_list("attributes").unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(attribute.get_str("id"), Some("minecraft:max_health"));
    assert_eq!(attribute.get_double("base"), Some(20.0));
    let effect = zombie.get_list("active_effects").unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(effect.get_str("id"), Some("minecraft:speed"));
    assert_eq!(effect.get_int("duration"), Some(100));
}

#[tuff_core::watchdog]
#[test]
fn arrows_and_boats_are_split() {
    let arrow = Compound::new()
        .with("id", "minecraft:arrow")
        .with("Potion", "minecraft:poison");
    let boat = Compound::new()
        .with("id", "minecraft:chest_boat")
        .with("Type", "birch");
    let mut root = Compound::new()
        .with("DataVersion", 3463)
        .with("Entities", vec![arrow, boat]);
    entity_chunk(&mut root).unwrap();
    let entities = root.get_list("Entities").unwrap();
    let arrow = entities[0].as_compound().unwrap();
    let item = arrow.get_compound("item").unwrap();
    assert_eq!(item.get_str("id"), Some("minecraft:tipped_arrow"));
    assert_eq!(
        components(item)
            .get_compound("minecraft:potion_contents")
            .unwrap()
            .get_str("potion"),
        Some("minecraft:poison")
    );
    assert!(!arrow.contains_key("Potion"));
    let boat = entities[1].as_compound().unwrap();
    assert_eq!(boat.get_str("id"), Some("minecraft:birch_chest_boat"));
}

#[tuff_core::watchdog]
#[test]
fn numeric_player_dimension_becomes_a_name() {
    for (old, new) in [
        (-1, "minecraft:the_nether"),
        (0, "minecraft:overworld"),
        (1, "minecraft:the_end"),
        (7, "minecraft:overworld"),
    ] {
        let mut root = Compound::new()
            .with("DataVersion", 2500)
            .with("Dimension", old);
        player(&mut root).unwrap();
        assert_eq!(root.get_str("Dimension"), Some(new));
    }
}

#[tuff_core::watchdog]
#[test]
fn legacy_player_is_upgraded_and_decodes() {
    let mut root = Compound::new()
        .with("DataVersion", V1_20_4)
        .with(
            "Inventory",
            vec![
                legacy_item("minecraft:stone", 32, None).with("Slot", 0i8),
                legacy_item("minecraft:diamond_boots", 1, None).with("Slot", 100i8),
                legacy_item("minecraft:shield", 1, None).with("Slot", -106i8),
            ],
        )
        .with("SpawnX", 10)
        .with("SpawnY", 70)
        .with("SpawnZ", -3)
        .with("SpawnDimension", "minecraft:the_nether")
        .with("SpawnForced", 1i8)
        .with("FallDistance", 2.0f32);
    player(&mut root).unwrap();
    assert_eq!(root.get_int("DataVersion"), Some(DATA_VERSION));
    let inventory = root.get_list("Inventory").unwrap();
    assert_eq!(inventory.len(), 1);
    let equipment = root.get_compound("equipment").unwrap();
    assert_eq!(
        equipment.get_compound("feet").unwrap().get_str("id"),
        Some("minecraft:diamond_boots")
    );
    assert!(equipment.get_compound("offhand").is_some());
    let respawn = root.get_compound("respawn").unwrap();
    assert_eq!(respawn.get_int_array("pos"), Some(&[10, 70, -3][..]));
    assert_eq!(respawn.get_str("dimension"), Some("minecraft:the_nether"));
    assert_eq!(respawn.get_float("yaw"), Some(0.0));
    assert_eq!(root.get_double("fall_distance"), Some(2.0));

    let (_, state) = PlayerRecord::decode(root, &player_defaults());
    let respawn = state.respawn.unwrap();
    assert_eq!(respawn.pos, [10, 70, -3]);
    assert!(respawn.forced);
    assert_eq!(state.slots.len(), 3);
}

#[tuff_core::watchdog]
#[test]
fn saved_data_and_advancements_are_upgraded() {
    let mut tickets = Compound::new().with("DataVersion", V1_21_4).with(
        "data",
        Compound::new().with("Forced", vec![(7i64 << 32) | 3]),
    );
    level::saved_data("minecraft:chunk_tickets", &mut tickets).unwrap();
    let ticket = tickets
        .get_compound("data")
        .unwrap()
        .get_list("tickets")
        .unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(ticket.get_int_array("chunk_pos"), Some(&[3, 7][..]));
    assert_eq!(ticket.get_str("type"), Some("minecraft:forced"));

    let mut scoreboard = Compound::new().with("DataVersion", 3463).with(
        "data",
        Compound::new()
            .with("DisplaySlots", Compound::new().with("slot_1", "kills"))
            .with(
                "Objectives",
                vec![Compound::new().with("DisplayName", json("Kills"))],
            ),
    );
    level::saved_data("minecraft:scoreboard", &mut scoreboard).unwrap();
    let data = scoreboard.get_compound("data").unwrap();
    assert_eq!(
        data.get_compound("DisplaySlots")
            .unwrap()
            .get_str("sidebar"),
        Some("kills")
    );
    let objective = data.get_list("Objectives").unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(
        objective
            .get_compound("DisplayName")
            .unwrap()
            .get_str("text"),
        Some("Kills")
    );

    let mut advancements = Compound::new()
        .with("DataVersion", 3000)
        .with(
            "minecraft:nether/use_lodestone",
            Compound::new().with("done", 1i8),
        )
        .with(
            "minecraft:husbandry/complete_catalogue",
            Compound::new().with(
                "criteria",
                Compound::new().with("textures/entity/cat/british_shorthair.png", "2020"),
            ),
        );
    level::advancements(&mut advancements).unwrap();
    assert!(advancements.contains_key("minecraft:adventure/use_lodestone"));
    let criteria = advancements
        .get_compound("minecraft:husbandry/complete_catalogue")
        .unwrap()
        .get_compound("criteria")
        .unwrap();
    assert!(criteria.contains_key("minecraft:british_shorthair"));
}

#[tuff_core::watchdog]
#[test]
fn records_before_1_13_are_refused() {
    let old = || Compound::new().with("DataVersion", MIN_DATA_VERSION - 1);
    let too_old = Err(DataFixError::TooOld(MIN_DATA_VERSION - 1));
    assert_eq!(entity_chunk(&mut old()), too_old);
    assert_eq!(structure(&mut old()), too_old);
    assert_eq!(player(&mut old()), too_old);
    assert_eq!(
        level::saved_data("minecraft:scoreboard", &mut old()),
        too_old
    );
    assert_eq!(level::stats(&mut old()), too_old);
    assert_eq!(level::advancements(&mut old()), too_old);
    let mut level_dat = Compound::new().with("Data", old());
    let before = level_dat.clone();
    assert_eq!(level::level_dat(&mut level_dat), too_old);
    assert_eq!(level_dat, before);
    for document in [
        Document::Level,
        Document::SavedData("scoreboard"),
        Document::Stats(0),
        Document::Advancements(0),
        Document::Player(0),
    ] {
        let mut root = if matches!(document, Document::Level) {
            Compound::new().with("Data", old())
        } else {
            old()
        };
        assert_eq!(super::document(document, &mut root), too_old);
    }
}

#[tuff_core::watchdog]
#[test]
fn old_advancement_ids_are_renamed() {
    let mut advancements = Compound::new()
        .with("DataVersion", 2000)
        .with("minecraft:recipes/misc/sugar", Compound::new())
        .with("minecraft:recipes/misc/composter", Compound::new())
        .with(
            "minecraft:husbandry/play_jukebox_in_meadows",
            Compound::new(),
        )
        .with("minecraft:adventure/caves_and_cliff", Compound::new())
        .with(
            "minecraft:adventure/ride_strider_in_overworld_lava",
            Compound::new(),
        );
    level::advancements(&mut advancements).unwrap();
    for new in [
        "minecraft:recipes/misc/sugar_from_sugar_cane",
        "minecraft:recipes/decorations/composter",
        "minecraft:adventure/play_jukebox_in_meadows",
        "minecraft:adventure/fall_from_world_height",
        "minecraft:nether/ride_strider_in_overworld_lava",
    ] {
        assert!(advancements.contains_key(new), "{new}");
    }
    assert!(!advancements.contains_key("minecraft:recipes/misc/sugar"));
}

#[tuff_core::watchdog]
#[test]
fn a_pig_egg_carrying_an_entity_id_becomes_that_egg() {
    let egg = |entity: Option<&str>| {
        let mut tag = Compound::new();
        if let Some(entity) = entity {
            tag.insert("EntityTag", Compound::new().with("id", entity));
        }
        Compound::new()
            .with("id", "minecraft:pig_spawn_egg")
            .with("Count", 1i8)
            .with("tag", tag)
    };
    let id_after = |entity: Option<&str>| {
        let mut item = egg(entity);
        item::upgrade(&mut item, Fixer::new(3000));
        item.get_str("id").unwrap().to_owned()
    };
    assert_eq!(id_after(Some("minecraft:cow")), "minecraft:cow_spawn_egg");
    assert_eq!(
        id_after(Some("minecraft:unknown")),
        "minecraft:pig_spawn_egg"
    );
    assert_eq!(id_after(None), "minecraft:pig_spawn_egg");
}

#[tuff_core::watchdog]
#[test]
fn block_renames_reach_adventure_predicates() {
    let mut item = Compound::new()
        .with("id", "minecraft:stick")
        .with("count", 1)
        .with(
            "components",
            Compound::new().with(
                "minecraft:can_break",
                vec![Compound::new().with(
                    "blocks",
                    vec![Tag::from("minecraft:chain"), Tag::from("#minecraft:logs")],
                )],
            ),
        );
    item::upgrade(&mut item, Fixer::new(4400));
    let predicates = item
        .get_compound("components")
        .and_then(|c| c.get_list("minecraft:can_break"))
        .unwrap();
    let blocks = predicates[0]
        .as_compound()
        .and_then(|p| p.get_list("blocks"))
        .unwrap();
    assert_eq!(blocks[0].as_str(), Some("minecraft:iron_chain"));
    assert_eq!(blocks[1].as_str(), Some("#minecraft:logs"));
}

#[tuff_core::watchdog]
#[test]
fn renames_reach_items_and_block_states_inside_particles() {
    let mut cloud = Compound::new()
        .with("id", "minecraft:area_effect_cloud")
        .with(
            "custom_particle",
            Compound::new().with("type", "minecraft:block").with(
                "block_state",
                Compound::new().with("Name", "minecraft:chain"),
            ),
        );
    entity::upgrade(&mut cloud, Fixer::new(4500));
    let state = cloud
        .get_compound("custom_particle")
        .and_then(|p| p.get_compound("block_state"))
        .unwrap();
    assert_eq!(
        state.get_str("Name").or_else(|| state.get_str("id")),
        Some("minecraft:iron_chain")
    );
}

#[tuff_core::watchdog]
#[test]
fn vibration_game_events_are_renamed_at_3084() {
    let listener = |event: &str| {
        Compound::new().with(
            "listener",
            Compound::new().with("event", Compound::new().with("game_event", event)),
        )
    };
    let event_of = |c: &Compound| {
        c.get_compound("listener")
            .and_then(|l| l.get_compound("event"))
            .and_then(|e| e.get_str("game_event"))
            .map(str::to_owned)
    };
    let mut sensor = listener("minecraft:block_press").with("id", "minecraft:sculk_sensor");
    block_entity::upgrade(&mut sensor, Fixer::new(3083));
    assert_eq!(
        event_of(&sensor).as_deref(),
        Some("minecraft:block_activate")
    );

    let mut warden = listener("wolf_shaking").with("id", "minecraft:warden");
    entity::upgrade(&mut warden, Fixer::new(3083));
    assert_eq!(event_of(&warden).as_deref(), Some("minecraft:entity_shake"));

    let mut current = listener("minecraft:block_press").with("id", "minecraft:sculk_sensor");
    block_entity::upgrade(&mut current, Fixer::new(3084));
    assert_eq!(event_of(&current).as_deref(), Some("minecraft:block_press"));
}

#[tuff_core::watchdog]
#[test]
fn old_map_id_counter_is_wrapped() {
    let mut old = Compound::new().with("map", 4);
    level::saved_data("minecraft:maps/last_id", &mut old).unwrap();
    assert_eq!(old.get_compound("data").unwrap().get_int("map"), Some(4));
}

#[tuff_core::watchdog]
#[test]
fn structure_start_references_become_at_least_one() {
    let mut chunk = Compound::new().with(
        "structures",
        Compound::new().with(
            "starts",
            Compound::new()
                .with("Mansion", Compound::new().with("references", 0))
                .with("Igloo", Compound::new().with("references", 3)),
        ),
    );
    chunk::upgrade(&mut chunk, Fixer::new(2200), true);
    let starts = chunk
        .get_compound("structures")
        .unwrap()
        .get_compound("starts")
        .unwrap();
    assert_eq!(
        starts
            .get_compound("minecraft:mansion")
            .unwrap()
            .get_int("references"),
        Some(1)
    );
    assert_eq!(
        starts
            .get_compound("minecraft:igloo")
            .unwrap()
            .get_int("references"),
        Some(3)
    );
}
