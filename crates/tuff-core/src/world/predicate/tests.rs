use super::*;
use serde_json::json;
use tuff_item::item::Components;
use tuff_item::item::MintReason;

#[tuff_core::watchdog]
#[test]
fn entity_component_predicates_read_variants_of_any_mob() {
    let mut cat = EntityFacts {
        kind: Some(EntityType::Cat),
        ..EntityFacts::default()
    };
    cat.components
        .push(("minecraft:cat/variant".to_owned(), "tabby".to_owned()));
    let check = |facts: &EntityFacts, key: &str, value: Value| {
        entity(&json!({key: value}), facts, [0.0; 3])
    };
    let one = json!({"minecraft:cat/variant": "minecraft:tabby"});
    assert!(check(&cat, "minecraft:predicates", one));
    let other = json!({"minecraft:cat/variant": ["minecraft:black", "tabby"]});
    assert!(check(&cat, "minecraft:predicates", other));
    let wrong = json!({"minecraft:cat/variant": "minecraft:black"});
    assert!(!check(&cat, "minecraft:predicates", wrong));
    let exact = json!({"minecraft:cat/variant": "minecraft:tabby"});
    assert!(check(&cat, "minecraft:components", exact));
    let missing = json!({"minecraft:frog/variant": "minecraft:temperate"});
    assert!(!check(&cat, "minecraft:predicates", missing));
}

#[tuff_core::watchdog]
#[test]
fn damage_predicates_need_a_damageable_item() {
    use tuff_item::item::{ItemStack, MintReason};
    let check = |item, predicate: Value| {
        let stack = ItemStack::mint(item, 1, MintReason::Test);
        item_sub_predicate("minecraft:damage", &predicate, &stack)
    };
    assert!(!check(Item::Stick, json!({"damage": {"min": 0}})));
    assert!(!check(Item::Stick, json!({})));
    assert!(check(Item::IronPickaxe, json!({"damage": 0})));
    assert!(check(Item::IronPickaxe, json!({})));
}

#[tuff_core::watchdog]
#[test]
fn effect_predicates_check_ambient_and_visible() {
    let active = [(MobEffect::Speed, 1, 100, true, false)];
    let check = |v| effects(&json!({"minecraft:speed": v}), &active);
    assert!(check(json!({"ambient": true, "visible": false})));
    assert!(!check(json!({"ambient": false})));
    assert!(!check(json!({"visible": true})));
    assert!(check(json!({"amplifier": 1})));
}

#[tuff_core::watchdog]
#[test]
fn the_player_predicate_needs_a_player_and_looking_at_reads_full_facts() {
    let check = |facts: &EntityFacts, fields: Value| {
        entity(&json!({"type_specific/player": fields}), facts, [0.0; 3])
    };
    let mut zombie = EntityFacts {
        kind: Some(EntityType::Zombie),
        ..EntityFacts::default()
    };
    assert!(!check(&zombie, json!({})));
    let mut player = EntityFacts {
        player: Some(PlayerFacts {
            game_mode: GameMode::Survival,
            level: 0,
            advancements: None,
            stats: None,
            input: None,
            food: None,
        }),
        ..player_at([0.0; 3])
    };
    assert!(check(&player, json!({})));
    let wanted = json!({"looking_at": {"type": "minecraft:zombie", "flags": {"is_baby": true}}});
    assert!(!check(&player, wanted.clone()));
    player.looking_at = Some(Box::new(zombie.clone()));
    assert!(!check(&player, wanted.clone()));
    zombie.flags.baby = true;
    player.looking_at = Some(Box::new(zombie));
    assert!(check(&player, wanted));
}

fn player_at(position: [f64; 3]) -> EntityFacts {
    EntityFacts {
        kind: Some(EntityType::Player),
        position,
        dimension: "minecraft:overworld".into(),
        ..EntityFacts::default()
    }
}

#[tuff_core::watchdog]
#[test]
fn item_predicates_check_ids_tags_counts_and_enchantments() {
    let planks = ItemStack::mint(Item::OakPlanks, 5, MintReason::Test);
    assert!(item(&json!({"items": "minecraft:oak_planks"}), &planks));
    assert!(item(&json!({"items": "#minecraft:planks"}), &planks));
    assert!(item(
        &json!({"items": ["minecraft:stone", "minecraft:oak_planks"]}),
        &planks
    ));
    assert!(!item(&json!({"items": "minecraft:stone"}), &planks));
    assert!(item(&json!({"count": {"min": 5}}), &planks));
    assert!(!item(&json!({"count": {"min": 6}}), &planks));
    assert!(!item(&json!({"unknown": 1}), &planks));

    let mut pick = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
    let silk = json!({"predicates": {"minecraft:enchantments": [
        {"enchantments": "minecraft:silk_touch", "levels": {"min": 1}}
    ]}});
    assert!(!item(&silk, &pick));
    pick.components.enchantments =
        Some(Enchantments::new([("minecraft:silk_touch".to_owned(), 1)]));
    assert!(item(&silk, &pick));
}

#[tuff_core::watchdog]
#[test]
fn entity_predicates_check_type_distance_location_and_equipment() {
    let mut zombie = player_at([10.0, 64.0, 0.0]);
    zombie.kind = Some(EntityType::Zombie);
    zombie.equipment = vec![(
        EquipmentSlot::Head,
        ItemStack::mint(Item::IronHelmet, 1, MintReason::Test),
    )];
    let origin = [0.0, 64.0, 0.0];
    assert!(entity(
        &json!({"minecraft:entity_type": "minecraft:zombie"}),
        &zombie,
        origin
    ));
    assert!(entity(
        &json!({"minecraft:entity_type": "#minecraft:undead"}),
        &zombie,
        origin
    ));
    assert!(!entity(
        &json!({"minecraft:entity_type": "minecraft:skeleton"}),
        &zombie,
        origin
    ));
    assert!(entity(
        &json!({"minecraft:distance": {"horizontal": {"min": 9.5}}}),
        &zombie,
        origin
    ));
    assert!(!entity(
        &json!({"minecraft:distance": {"horizontal": {"min": 10.5}}}),
        &zombie,
        origin
    ));
    assert!(entity(
        &json!({"minecraft:location": {"dimension": "minecraft:overworld"}}),
        &zombie,
        origin
    ));
    assert!(entity(
        &json!({"minecraft:equipment": {"head": {"items": "minecraft:iron_helmet"}}}),
        &zombie,
        origin
    ));
    assert!(!entity(
        &json!({"minecraft:equipment": {"feet": {"items": "minecraft:iron_boots"}}}),
        &zombie,
        origin
    ));
}

#[tuff_core::watchdog]
#[test]
fn location_predicates_check_biomes_positions_and_blocks() {
    let at = LocationFacts {
        position: [0.0, -10.0, 0.0],
        dimension: "minecraft:overworld".into(),
        biome: Some("minecraft:badlands".into()),
        block: Some(Block::Wheat.default_state()),
        ..LocationFacts::default()
    };
    assert!(location(&json!({"biomes": "minecraft:badlands"}), &at));
    assert!(location(&json!({"biomes": "#minecraft:is_badlands"}), &at));
    assert!(!location(&json!({"biomes": "minecraft:desert"}), &at));
    assert!(location(&json!({"position": {"y": {"max": 0}}}), &at));
    assert!(location(
        &json!({"block": {"blocks": "minecraft:wheat", "state": {"age": "0"}}}),
        &at
    ));
    assert!(!location(
        &json!({"block": {"blocks": "minecraft:wheat", "state": {"age": "7"}}}),
        &at
    ));
}

#[tuff_core::watchdog]
#[test]
fn loot_conditions_combine_and_unknown_kinds_never_match() {
    let player = player_at([0.0, 70.0, 0.0]);
    let context = Context {
        this: Some(&player),
        target: None,
        origin: player.position,
        location: None,
        tool: None,
        enchantment_level: None,
    };
    let is_player = json!({"type": "minecraft:entity_properties", "entity": "this",
        "predicate": {"minecraft:entity_type": "minecraft:player"}});
    assert!(condition(&is_player, &context));
    assert!(!condition(
        &json!({"type": "minecraft:inverted", "term": is_player}),
        &context
    ));
    assert!(condition(
        &json!({"type": "minecraft:any_of", "terms": [{"type": "minecraft:weather_check"}, is_player]}),
        &context
    ));
    assert!(!condition(
        &json!({"type": "minecraft:weather_check"}),
        &context
    ));
    assert!(!condition(
        &json!("minecraft:some_predicate_file"),
        &context
    ));
}

/// A world of stone below y 64 and water at y 64, raining, with a
/// fixed random draw and one clock.
struct Mock {
    draw: f32,
}

impl PredicateWorld for Mock {
    fn location(&self, position: [f64; 3]) -> LocationFacts {
        let block = match position[1].floor() as i32 {
            y if y < 64 => Block::Stone,
            64 => Block::Water,
            _ => Block::Air,
        };
        LocationFacts {
            position,
            dimension: "minecraft:overworld".into(),
            biome: Some("minecraft:plains".into()),
            block: Some(block.default_state()),
            light: Some(if position[1] >= 64.0 { 15 } else { 0 }),
            can_see_sky: Some(position[1] >= 64.0),
            smokey: Some(false),
            block_entity: None,
            structures: Some(vec!["minecraft:village_plains".into()]),
        }
    }

    fn raining(&self) -> bool {
        true
    }

    fn thundering(&self) -> bool {
        false
    }

    fn clock(&self, id: &str) -> Option<i64> {
        (id == "minecraft:overworld").then_some(24_100)
    }

    fn score(&self, holder: &str, objective: &str) -> Option<i32> {
        (holder == "Steve" && objective == "coins").then_some(7)
    }

    fn team(&self, holder: &str) -> Option<&str> {
        (holder == "Steve").then_some("red")
    }

    fn random(&self) -> f32 {
        self.draw
    }

    fn random_int(&self, min: i64, _max: i64) -> i64 {
        min
    }
}

#[tuff_core::watchdog]
#[test]
fn world_conditions_read_weather_time_scores_and_blocks() {
    let mut steve = player_at([0.5, 65.0, 0.5]);
    steve.holder = Some("Steve".into());
    steve.tags = vec!["hero".into()];
    steve.nbt = Some(Compound::new().with("XpLevel", 3));
    steve.flags.on_ground = true;
    steve.flags.fall_flying = true;
    steve.player = Some(PlayerFacts {
        game_mode: GameMode::Creative,
        level: 3,
        advancements: None,
        stats: None,
        input: None,
        food: None,
    });
    let context = Context {
        this: Some(&steve),
        target: None,
        origin: steve.position,
        location: None,
        tool: None,
        enchantment_level: None,
    };
    let world = Mock { draw: 0.5 };
    let test = |condition: Value| condition_in(&condition, &context, Some(&world));
    assert!(test(
        json!({"type": "minecraft:weather_check", "raining": true})
    ));
    assert!(test(json!({"type": "minecraft:location_check",
        "predicate": {"structures": "#minecraft:village"}})));
    assert!(!test(json!({"type": "minecraft:location_check",
        "predicate": {"structures": ["minecraft:stronghold"]}})));
    assert!(!test(
        json!({"type": "minecraft:weather_check", "thundering": 1})
    ));
    assert!(test(
        json!({"type": "minecraft:time_check", "clock": "overworld",
        "period": 24000, "value": {"min": 0, "max": 200}})
    ));
    assert!(!test(
        json!({"type": "minecraft:time_check", "clock": "overworld",
        "value": 100})
    ));
    assert!(test(
        json!({"type": "minecraft:random_chance", "chance": 0.6})
    ));
    assert!(!test(json!({"type": "minecraft:random_chance",
        "chance": {"type": "minecraft:constant", "value": 0.4}})));
    assert!(!condition(
        &json!({"type": "minecraft:random_chance", "chance": 1.0}),
        &context
    ));
    assert!(test(
        json!({"type": "minecraft:entity_scores", "entity": "this",
        "scores": {"coins": {"min": 5, "max": 7}}})
    ));
    assert!(!test(
        json!({"type": "minecraft:entity_scores", "entity": "this",
        "scores": {"coins": 6}})
    ));
    assert!(!test(
        json!({"type": "minecraft:entity_scores", "entity": "this",
        "scores": {"gems": {"min": 0}}})
    ));
    assert!(test(json!({"type": "minecraft:int_value_check",
        "value": {"type": "minecraft:score", "target": "this", "score": "coins"},
        "test": {"min": {"type": "minecraft:score", "target": {"type": "fixed",
            "name": "Steve"}, "score": "coins"}}})));
    assert!(test(json!({"type": "minecraft:int_value_check",
        "value": {"type": "minecraft:score", "target": "this", "score": "gems",
            "fallback": 2}, "test": 2})));
    assert!(test(
        json!({"type": "minecraft:float_value_check", "value": 1.5,
        "test": {"max": 2.0}})
    ));
    assert!(test(
        json!({"type": "minecraft:location_check", "offsetY": -1,
        "predicate": {"fluid": {"fluids": "#minecraft:water"}, "can_see_sky": true}})
    ));
    assert!(test(
        json!({"type": "minecraft:location_check", "offsetY": -2,
        "predicate": {"block": {"blocks": "minecraft:stone"}, "light": {"light": 0}}})
    ));
    assert!(!test(json!({"type": "minecraft:location_check",
        "predicate": {"biomes": "minecraft:desert"}})));
    assert!(test(json!({"type": "minecraft:survives_explosion"})));
    assert!(!test(json!({"type": "minecraft:killed_by_player"})));
    assert!(test(
        json!({"type": "minecraft:entity_properties", "entity": "attacker"})
    ));

    let properties = |predicate: Value| {
        test(
            json!({"type": "minecraft:entity_properties", "entity": "this",
            "predicate": predicate}),
        )
    };
    assert!(properties(json!({"minecraft:nbt": "{XpLevel:3}"})));
    assert!(!properties(json!({"minecraft:nbt": "{XpLevel:4}"})));
    assert!(properties(
        json!({"minecraft:entity_tags": {"any_of": ["hero", "x"],
        "none_of": ["villain"]}})
    ));
    assert!(!properties(
        json!({"minecraft:entity_tags": {"all_of": ["hero", "x"]}})
    ));
    assert!(properties(json!({"minecraft:team": "red"})));
    assert!(!properties(json!({"minecraft:team": "blue"})));
    assert!(properties(json!({"minecraft:flags": {"is_flying": true,
        "is_fall_flying": 1, "is_sneaking": false}})));
    assert!(properties(json!({"minecraft:type_specific/player": {
        "gamemode": ["creative", "adventure"], "level": {"min": 3}}})));
    assert!(!properties(json!({"minecraft:type_specific/player": {
        "gamemode": ["survival"]}})));
    assert!(properties(
        json!({"minecraft:location": {"block": {"blocks": "minecraft:air"}}})
    ));
    assert!(properties(
        json!({"minecraft:stepping_on": {"block": {"blocks": "minecraft:water"}}})
    ));
    assert!(properties(json!({"minecraft:movement_affected_by": {
        "block": {"blocks": "minecraft:water"}}})));
}

#[tuff_core::watchdog]
#[test]
fn bounds_accept_numbers_and_ranges() {
    assert!(bounds(&json!(3), 3.0));
    assert!(bounds(&json!({"min": 1, "max": 3}), 2.0));
    assert!(!bounds(&json!({"min": 1, "max": 3}), 4.0));
    assert!(bounds_sqr(&json!({"min": 7000.0}), 7000.0 * 7000.0));
    assert!(!bounds_sqr(&json!({"min": 7000.0}), 6999.0 * 6999.0));
}

#[tuff_core::watchdog]
#[test]
fn exact_components_fall_back_to_item_defaults() {
    let stack = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
    assert!(item(
        &json!({"components": {"minecraft:max_stack_size": 1}}),
        &stack
    ));
    assert!(!item(
        &json!({"components": {"minecraft:max_stack_size": 64}}),
        &stack
    ));
    let planks = ItemStack::mint(Item::OakPlanks, 1, MintReason::Test);
    assert!(item(
        &json!({"components": {"minecraft:max_stack_size": 64}}),
        &planks
    ));
    assert!(!item(
        &json!({"components": {"minecraft:unknown_thing": 1}}),
        &planks
    ));
}

#[tuff_core::watchdog]
#[test]
fn state_ranges_compare_enum_and_boolean_properties_by_order() {
    let lever = Block::Lever
        .default_state()
        .with("face", "wall")
        .expect("face");
    assert!(state_properties(
        &json!({"face": {"min": "floor", "max": "wall"}}),
        lever
    ));
    assert!(!state_properties(
        &json!({"face": {"min": "ceiling"}}),
        lever
    ));
    assert!(state_properties(
        &json!({"powered": {"min": "false", "max": "true"}}),
        lever
    ));
    assert!(!state_properties(
        &json!({"powered": {"min": "true"}}),
        lever
    ));
}

fn stack_with(item: Item, key: &str, value: impl Into<Tag>) -> ItemStack {
    ItemStack::mint(item, 1, MintReason::Test)
        .with_components(Components::from_nbt(&Compound::new().with(key, value)))
}

#[tuff_core::watchdog]
#[test]
fn item_component_predicates_cover_fireworks_books_and_presence() {
    let rocket = stack_with(
        Item::FireworkRocket,
        "minecraft:fireworks",
        Compound::new().with("flight_duration", 2i8).with(
            "explosions",
            Tag::List(vec![
                Compound::new()
                    .with("shape", "star")
                    .with("has_trail", 1i8)
                    .into(),
            ]),
        ),
    );
    let fireworks = json!({"predicates": {"minecraft:fireworks": {
        "flight_duration": {"min": 2},
        "explosions": {"contains": [{"shape": "star", "has_trail": true}]}
    }}});
    assert!(item(&fireworks, &rocket));
    let wrong = json!({"predicates": {"minecraft:fireworks": {
        "explosions": {"contains": [{"shape": "burst"}]}
    }}});
    assert!(!item(&wrong, &rocket));
    assert!(!item(
        &fireworks,
        &ItemStack::mint(Item::FireworkRocket, 1, MintReason::Test)
    ));

    let book = stack_with(
        Item::WritableBook,
        "minecraft:writable_book_content",
        Compound::new().with("pages", Tag::List(vec![Tag::String("hello".into())])),
    );
    assert!(item(
        &json!({"predicates": {"minecraft:writable_book_content": {"pages": {"contains": ["hello"]}}}}),
        &book
    ));
    assert!(!item(
        &json!({"predicates": {"minecraft:writable_book_content": {"pages": {"contains": ["bye"]}}}}),
        &book
    ));

    let presence = json!({"predicates": {"minecraft:writable_book_content": {}}});
    assert!(item(&presence, &book));
    assert!(!item(
        &presence,
        &ItemStack::mint(Item::WritableBook, 1, MintReason::Test)
    ));
}

#[tuff_core::watchdog]
#[test]
fn adventure_predicates_match_blocks_tags_and_state() {
    use tuff_nbt::{Compound, Tag};
    let entry = |blocks: Tag, state: Option<(&str, &str)>| {
        let mut fields = Compound::new();
        fields.insert("blocks", blocks);
        if let Some((name, value)) = state {
            let mut properties = Compound::new();
            properties.insert(name, Tag::String(value.to_owned()));
            fields.insert("state", Tag::Compound(properties));
        }
        Tag::Compound(fields)
    };
    let text = |value: &str| Tag::String(value.to_owned());
    let lit = Block::RedstoneLamp
        .default_state()
        .with("lit", "true")
        .unwrap();
    assert!(adventure_allows(
        &entry(text("minecraft:redstone_lamp"), None),
        lit,
        None
    ));
    assert!(adventure_allows(
        &entry(text("minecraft:redstone_lamp"), Some(("lit", "true"))),
        lit,
        None
    ));
    assert!(!adventure_allows(
        &entry(text("minecraft:redstone_lamp"), Some(("lit", "false"))),
        lit,
        None
    ));
    assert!(!adventure_allows(
        &entry(text("minecraft:stone"), None),
        lit,
        None
    ));
    let list = Tag::List(vec![
        entry(text("minecraft:stone"), None),
        entry(Tag::List(vec![text("minecraft:redstone_lamp")]), None),
    ]);
    assert!(adventure_allows(&list, lit, None));
    let oak = Block::OakLog.default_state();
    assert!(adventure_allows(
        &entry(text("#minecraft:logs"), None),
        oak,
        None
    ));
}
