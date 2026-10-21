use super::super::pre2860;
use super::super::{DataFixError, Fixer, block_entity, entity, item};
use tuff_nbt::{Compound, Tag};

const FROM: i32 = 2500;

fn most_least(prefix: &str, most: i64, least: i64) -> Compound {
    Compound::new()
        .with(format!("{prefix}Most"), most)
        .with(format!("{prefix}Least"), least)
}

fn merge(mut a: Compound, b: Compound) -> Compound {
    for (key, value) in b.iter() {
        a.insert(key, value.clone());
    }
    a
}

#[tuff_core::watchdog]
#[test]
fn entity_uuid_and_owner_move_to_int_arrays() {
    let mut wolf = merge(
        Compound::new()
            .with("id", "minecraft:wolf")
            .with("OwnerUUID", "00000001-0002-0003-0004-000500000006"),
        most_least("UUID", 0x0000_0001_0000_0002, 0x0000_0003_0000_0004),
    );
    pre2860::entity(&mut wolf, Fixer::new(FROM));
    assert_eq!(wolf.get_int_array("UUID"), Some(&[1, 2, 3, 4][..]));
    assert!(wolf.get("UUIDMost").is_none());
    assert_eq!(
        wolf.get_int_array("Owner"),
        Some(&[1, 2 << 16 | 3, 4 << 16 | 5, 6][..])
    );
    assert!(wolf.get("OwnerUUID").is_none());
}

#[tuff_core::watchdog]
#[test]
fn throwable_owner_is_converted_then_renamed() {
    let mut egg = Compound::new().with("id", "minecraft:egg").with(
        "owner",
        Compound::new()
            .with("M", 0x0000_0001_0000_0002i64)
            .with("L", 0x0000_0003_0000_0004i64),
    );
    pre2860::entity(&mut egg, Fixer::new(FROM));
    assert_eq!(egg.get_int_array("Owner"), Some(&[1, 2, 3, 4][..]));
    assert!(egg.get("owner").is_none());
}

#[tuff_core::watchdog]
#[test]
fn zombie_pigman_is_renamed_and_arrow_pickup_added() {
    let mut pigman = Compound::new().with("id", "minecraft:zombie_pigman");
    entity::upgrade(&mut pigman, Fixer::new(FROM));
    assert_eq!(pigman.get_str("id"), Some("minecraft:zombified_piglin"));
    let mut arrow = Compound::new()
        .with("id", "minecraft:arrow")
        .with("player", 0i8);
    pre2860::entity(&mut arrow, Fixer::new(FROM));
    assert_eq!(arrow.get_byte("pickup"), Some(0));
    assert!(arrow.get("player").is_none());
}

#[tuff_core::watchdog]
#[test]
fn attributes_gossips_and_villager_range() {
    let mut villager = Compound::new()
        .with("id", "minecraft:villager")
        .with(
            "Attributes",
            vec![
                Compound::new()
                    .with("Name", "generic.followRange")
                    .with("Base", 16.0f64),
            ],
        )
        .with(
            "Gossips",
            vec![merge(
                Compound::new().with("Type", "major_positive"),
                most_least("Target", 5, 6),
            )],
        );
    pre2860::entity(&mut villager, Fixer::new(FROM));
    let attribute = villager.get_list("Attributes").unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(
        attribute.get_str("Name"),
        Some("minecraft:generic.follow_range")
    );
    let gossip = villager.get_list("Gossips").unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(gossip.get_int_array("Target"), Some(&[0, 5, 0, 6][..]));
}

#[tuff_core::watchdog]
#[test]
fn fixes_at_or_after_the_saved_version_are_skipped() {
    let mut arrow = Compound::new()
        .with("id", "minecraft:arrow")
        .with("player", 0i8);
    pre2860::entity(&mut arrow, Fixer::new(2702));
    assert!(arrow.get("pickup").is_none());
}

#[tuff_core::watchdog]
#[test]
fn shulker_rotation_and_strider_gravity() {
    let mut shulker = Compound::new()
        .with("id", "minecraft:shulker")
        .with("Rotation", vec![Tag::Float(270.0), Tag::Float(10.0)]);
    pre2860::entity(&mut shulker, Fixer::new(FROM));
    let rotation = shulker.get_list("Rotation").unwrap();
    assert_eq!(rotation[0], Tag::Float(90.0));
    let mut strider = Compound::new()
        .with("id", "minecraft:strider")
        .with("NoGravity", 1i8);
    pre2860::entity(&mut strider, Fixer::new(FROM));
    assert_eq!(strider.get_byte("NoGravity"), Some(0));
}

#[tuff_core::watchdog]
#[test]
fn spawner_data_is_wrapped() {
    let mut spawner = Compound::new()
        .with("id", "minecraft:mob_spawner")
        .with("SpawnData", Compound::new().with("id", "minecraft:pig"))
        .with(
            "SpawnPotentials",
            vec![
                Compound::new()
                    .with("Weight", 3)
                    .with("Entity", Compound::new().with("id", "minecraft:cow")),
            ],
        );
    pre2860::block_entity(&mut spawner, Fixer::new(FROM));
    let data = spawner.get_compound("SpawnData").unwrap();
    assert_eq!(
        data.get_compound("entity").unwrap().get_str("id"),
        Some("minecraft:pig")
    );
    let potential = spawner.get_list("SpawnPotentials").unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(potential.get_int("weight"), Some(3));
    assert_eq!(
        potential
            .get_compound("data")
            .unwrap()
            .get_compound("entity")
            .unwrap()
            .get_str("id"),
        Some("minecraft:cow")
    );
}

#[tuff_core::watchdog]
#[test]
fn furnace_recipes_jigsaw_and_skull_blocks() {
    let mut furnace = Compound::new()
        .with("id", "minecraft:furnace")
        .with("RecipesUsedSize", 1)
        .with("RecipeLocation0", "smelting_iron")
        .with("RecipeAmount0", 4);
    pre2860::block_entity(&mut furnace, Fixer::new(FROM));
    let used = furnace.get_compound("RecipesUsed").unwrap();
    assert_eq!(used.get_int("minecraft:smelting_iron"), Some(4));
    assert!(furnace.get("RecipeLocation0").is_none());

    let mut jigsaw = Compound::new()
        .with("id", "minecraft:jigsaw")
        .with("attachement_type", "minecraft:door")
        .with("target_pool", "minecraft:pool");
    pre2860::block_entity(&mut jigsaw, Fixer::new(FROM));
    assert_eq!(jigsaw.get_str("name"), Some("minecraft:door"));
    assert_eq!(jigsaw.get_str("target"), Some("minecraft:door"));
    assert_eq!(jigsaw.get_str("pool"), Some("minecraft:pool"));

    let mut skull = Compound::new().with("id", "minecraft:skull").with(
        "Owner",
        Compound::new().with("Id", "00000001-0002-0003-0004-000500000006"),
    );
    pre2860::block_entity(&mut skull, Fixer::new(FROM));
    assert!(
        skull
            .get_compound("SkullOwner")
            .unwrap()
            .get_int_array("Id")
            .is_some()
    );
    let _ = block_entity::upgrade;
}

#[tuff_core::watchdog]
#[test]
fn old_item_ids_are_renamed_and_modifier_uuids_converted() {
    let mut torch = Compound::new()
        .with("id", "minecraft:soul_fire_torch")
        .with("Count", 1i8);
    item::upgrade(&mut torch, Fixer::new(FROM));
    assert_eq!(torch.get_str("id"), Some("minecraft:soul_torch"));
    let mut sword = Compound::new().with("id", "minecraft:iron_sword").with(
        "tag",
        Compound::new().with(
            "AttributeModifiers",
            vec![merge(
                Compound::new().with("AttributeName", "generic.attackDamage"),
                most_least("UUID", 7, 8),
            )],
        ),
    );
    pre2860::item(&mut sword, Fixer::new(FROM));
    let modifier = sword
        .get_compound("tag")
        .unwrap()
        .get_list("AttributeModifiers")
        .unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(modifier.get_int_array("UUID"), Some(&[0, 7, 0, 8][..]));
    assert_eq!(
        modifier.get_str("AttributeName"),
        Some("minecraft:generic.attack_damage")
    );
}

#[tuff_core::watchdog]
#[test]
fn ocelot_splits_into_cat_and_trusting_ocelot() {
    let mut cat = Compound::new()
        .with("id", "minecraft:ocelot")
        .with("CatType", 2)
        .with("OwnerUUID", "");
    pre2860::entity(&mut cat, Fixer::new(1900));
    assert_eq!(cat.get_str("id"), Some("minecraft:cat"));
    let mut wild = Compound::new()
        .with("id", "minecraft:ocelot")
        .with("CatType", 0)
        .with("OwnerUUID", "00000001-0002-0003-0004-000500000006");
    pre2860::entity(&mut wild, Fixer::new(1900));
    assert_eq!(wild.get_str("id"), Some("minecraft:ocelot"));
    assert_eq!(wild.get_byte("Trusting"), Some(1));
}

#[tuff_core::watchdog]
#[test]
fn villager_profession_level_xp_and_gossip() {
    let mut villager = Compound::new()
        .with("id", "minecraft:villager")
        .with("Profession", 3)
        .with("Career", 2)
        .with("CareerLevel", 1)
        .with(
            "Offers",
            Compound::new().with("Recipes", vec![Compound::new(); 6]),
        )
        .with(
            "Gossips",
            vec![
                Compound::new().with("Type", "golem"),
                Compound::new().with("Type", "major_positive"),
            ],
        );
    pre2860::entity(&mut villager, Fixer::new(1900));
    let data = villager.get_compound("VillagerData").unwrap();
    assert_eq!(data.get_str("profession"), Some("minecraft:weaponsmith"));
    assert_eq!(data.get_int("level"), Some(3));
    assert_eq!(villager.get_int("Xp"), Some(50));
    assert_eq!(villager.get_list("Gossips").map(<[Tag]>::len), Some(1));
    assert!(villager.get("Profession").is_none());
}

#[tuff_core::watchdog]
#[test]
fn level_and_raid_uuids_become_int_arrays() {
    let mut data = Compound::new()
        .with("WanderingTraderId", "00000001-0002-0003-0004-000500000006")
        .with(
            "CustomBossEvents",
            Compound::new().with(
                "a:b",
                Compound::new().with(
                    "Players",
                    vec![
                        Compound::new()
                            .with("M", 0x0000_0001_0000_0002i64)
                            .with("L", 0x0000_0003_0000_0004i64),
                    ],
                ),
            ),
        );
    pre2860::level(&mut data, Fixer::new(FROM)).unwrap();
    assert_eq!(
        data.get_int_array("WanderingTraderId"),
        Some(&[1, 2 << 16 | 3, 4 << 16 | 5, 6][..])
    );
    let mut raids = Compound::new().with(
        "Raids",
        vec![Compound::new().with(
            "HeroesOfTheVillage",
            vec![most_least(
                "UUID",
                0x0000_0001_0000_0002,
                0x0000_0003_0000_0004,
            )],
        )],
    );
    pre2860::raids(&mut raids, Fixer::new(FROM));
    let raid = raids.get_list("Raids").unwrap()[0].as_compound().unwrap();
    assert!(matches!(
        raid.get_list("HeroesOfTheVillage").unwrap()[0],
        Tag::IntArray(_)
    ));
}

#[tuff_core::watchdog]
#[test]
fn old_item_lore_and_blacksmith_loot_table() {
    let mut item = Compound::new().with("id", "minecraft:cactus_green").with(
        "tag",
        Compound::new().with(
            "display",
            Compound::new().with("Lore", Tag::List(vec![Tag::String("hi".into())])),
        ),
    );
    item::upgrade(&mut item, Fixer::new(1700));
    assert_eq!(item.get_str("id"), Some("minecraft:green_dye"));
    let mut chest = Compound::new()
        .with("id", "minecraft:chest")
        .with("LootTable", "minecraft:chests/village_blacksmith");
    pre2860::block_entity(&mut chest, Fixer::new(1900));
    assert_eq!(
        chest.get_str("LootTable"),
        Some("minecraft:chests/village/village_weaponsmith")
    );
}

#[tuff_core::watchdog]
#[test]
fn legacy_level_generator_keys_become_world_gen_settings() {
    let mut data = Compound::new()
        .with("RandomSeed", -77i64)
        .with("generatorName", "Customized")
        .with("generatorOptions", "{custom}")
        .with("generatorVersion", 1)
        .with("MapFeatures", 0i8)
        .with("BonusChest", 1i8);
    pre2860::level(&mut data, Fixer::new(FROM)).unwrap();
    for key in ["RandomSeed", "generatorName", "MapFeatures", "BonusChest"] {
        assert!(data.get(key).is_none(), "{key}");
    }
    let settings = data.get_compound("WorldGenSettings").unwrap();
    assert_eq!(settings.get_long("seed"), Some(-77));
    assert_eq!(settings.get_byte("generate_features"), Some(0));
    assert_eq!(settings.get_byte("bonus_chest"), Some(1));
    assert_eq!(settings.get_str("legacy_custom_options"), Some("{custom}"));
    assert!(settings.get("generatorName").is_none());

    let mut current = Compound::new().with("RandomSeed", 5i64);
    pre2860::level(&mut current, Fixer::new(2538)).unwrap();
    assert_eq!(current.get_long("RandomSeed"), Some(5));
    assert!(current.get("WorldGenSettings").is_none());
}

fn converted(old: Compound, from: i32) -> Compound {
    let mut data = old;
    pre2860::level(&mut data, Fixer::new(from)).unwrap();
    data.get_compound("WorldGenSettings").unwrap().clone()
}

fn overworld_generator(settings: &Compound) -> &Compound {
    settings
        .get_compound("dimensions")
        .and_then(|d| d.get_compound("minecraft:overworld"))
        .and_then(|d| d.get_compound("generator"))
        .unwrap()
}

#[tuff_core::watchdog]
#[test]
fn a_legacy_default_world_gets_the_three_vanilla_dimensions() {
    let settings = converted(
        Compound::new()
            .with("RandomSeed", i64::MAX - 5)
            .with("generatorName", "largeBiomes")
            .with("MapFeatures", 1i8),
        1600,
    );
    assert_eq!(settings.get_long("seed"), Some(i64::MAX - 5));
    let dimensions = settings.get_compound("dimensions").unwrap();
    for key in [
        "minecraft:overworld",
        "minecraft:the_nether",
        "minecraft:the_end",
    ] {
        assert!(dimensions.get_compound(key).unwrap().contains_key("type"));
    }
    let generator = overworld_generator(&settings);
    assert_eq!(generator.get_str("type"), Some("minecraft:noise"));
    assert_eq!(
        generator.get_str("settings"),
        Some("minecraft:large_biomes")
    );
    let source = generator.get_compound("biome_source").unwrap();
    assert_eq!(source.get_str("type"), Some("minecraft:multi_noise"));
    assert_eq!(source.get_str("preset"), Some("minecraft:overworld"));
    assert!(!settings.contains_key("has_increased_height_already"));
}

#[tuff_core::watchdog]
#[test]
fn a_legacy_flat_world_keeps_its_layers_and_gains_the_air_layer() {
    let options = Compound::new()
        .with("biome", "minecraft:desert")
        .with(
            "layers",
            vec![
                Compound::new()
                    .with("height", 3)
                    .with("block", "minecraft:stone"),
            ],
        )
        .with(
            "structures",
            Compound::new()
                .with("village", Compound::new().with("distance", "20"))
                .with("stronghold", Compound::new().with("count", "9")),
        );
    let settings = converted(
        Compound::new()
            .with("generatorName", "flat")
            .with("generatorOptions", options),
        1600,
    );
    let generator = overworld_generator(&settings);
    assert_eq!(generator.get_str("type"), Some("minecraft:flat"));
    let flat = generator.get_compound("settings").unwrap();
    assert_eq!(flat.get_str("biome"), Some("minecraft:desert"));
    let layers = flat.get_list("layers").unwrap();
    assert_eq!(layers.len(), 2);
    assert_eq!(
        layers[0].as_compound().unwrap().get_str("block"),
        Some("minecraft:air")
    );
    let structures = flat.get_compound("structures").unwrap();
    let village = structures.get_compound("minecraft:village").unwrap();
    assert_eq!(village.get_int("spacing"), Some(20));
    assert_eq!(village.get_str("type"), Some("minecraft:random_spread"));
    let stronghold = structures.get_compound("minecraft:stronghold").unwrap();
    assert_eq!(stronghold.get_int("count"), Some(9));
    assert_eq!(
        stronghold.get_str("type"),
        Some("minecraft:concentric_rings")
    );
}

#[tuff_core::watchdog]
#[test]
fn a_legacy_caves_buffet_world_uses_the_caves_dimension_type() {
    let options = Compound::new()
        .with(
            "chunk_generator",
            Compound::new().with("type", "minecraft:caves"),
        )
        .with(
            "biome_source",
            Compound::new().with("type", "minecraft:fixed").with(
                "options",
                Compound::new().with("biomes", vec![Tag::from("minecraft:plains")]),
            ),
        );
    let settings = converted(
        Compound::new()
            .with("generatorName", "buffet")
            .with("generatorOptions", options),
        1600,
    );
    let overworld = settings
        .get_compound("dimensions")
        .and_then(|d| d.get_compound("minecraft:overworld"))
        .unwrap();
    assert_eq!(overworld.get_str("type"), Some("minecraft:overworld_caves"));
    let generator = overworld_generator(&settings);
    assert_eq!(generator.get_str("settings"), Some("minecraft:caves"));
    let source = generator.get_compound("biome_source").unwrap();
    assert_eq!(source.get_str("biome"), Some("minecraft:plains"));
    assert!(!source.contains_key("options"));
}

#[tuff_core::watchdog]
#[test]
fn old_custom_dimensions_without_a_type_are_refused() {
    let custom = Compound::new().with(
        "WorldGenSettings",
        Compound::new().with(
            "dimensions",
            Compound::new().with("a:b", Compound::new().with("generator", Compound::new())),
        ),
    );
    let mut data = custom;
    assert_eq!(
        pre2860::level(&mut data, Fixer::new(2700)),
        Err(DataFixError::OldCustomWorld)
    );
}

#[tuff_core::watchdog]
#[test]
fn sugar_recipe_is_renamed_in_the_recipe_book() {
    let mut player = Compound::new().with(
        "recipeBook",
        Compound::new()
            .with(
                "recipes",
                vec![Tag::from("minecraft:sugar"), Tag::from("minecraft:bread")],
            )
            .with("toBeDisplayed", vec![Tag::from("sugar")]),
    );
    pre2860::player(&mut player, Fixer::new(1900));
    let book = player.get_compound("recipeBook").unwrap();
    assert_eq!(
        book.get_list("recipes").unwrap()[0],
        Tag::from("minecraft:sugar_from_sugar_cane")
    );
    assert_eq!(
        book.get_list("recipes").unwrap()[1],
        Tag::from("minecraft:bread")
    );
    assert_eq!(
        book.get_list("toBeDisplayed").unwrap()[0],
        Tag::from("minecraft:sugar_from_sugar_cane")
    );
}
