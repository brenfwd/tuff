//! Entity, player, item and block entity fixes of data versions 2501 to 2831,
//! which only matter for chunks, entity chunks and player files older than 1.18.

use super::{DataFixError, Fixer, for_each_compound, get_number, id_of, namespaced};
use std::collections::BTreeMap;

use tuff_nbt::{Compound, Tag};

const ABSTRACT_HORSES: &[&str] = &[
    "minecraft:donkey",
    "minecraft:horse",
    "minecraft:llama",
    "minecraft:mule",
    "minecraft:skeleton_horse",
    "minecraft:trader_llama",
    "minecraft:zombie_horse",
];
const TAMEABLE_ANIMALS: &[&str] = &["minecraft:cat", "minecraft:parrot", "minecraft:wolf"];
const ANIMALS: &[&str] = &[
    "minecraft:bee",
    "minecraft:chicken",
    "minecraft:cow",
    "minecraft:fox",
    "minecraft:mooshroom",
    "minecraft:ocelot",
    "minecraft:panda",
    "minecraft:pig",
    "minecraft:polar_bear",
    "minecraft:rabbit",
    "minecraft:sheep",
    "minecraft:turtle",
    "minecraft:hoglin",
];
const MOBS: &[&str] = &[
    "minecraft:bat",
    "minecraft:blaze",
    "minecraft:cave_spider",
    "minecraft:cod",
    "minecraft:creeper",
    "minecraft:dolphin",
    "minecraft:drowned",
    "minecraft:elder_guardian",
    "minecraft:ender_dragon",
    "minecraft:enderman",
    "minecraft:endermite",
    "minecraft:evoker",
    "minecraft:ghast",
    "minecraft:giant",
    "minecraft:guardian",
    "minecraft:husk",
    "minecraft:illusioner",
    "minecraft:magma_cube",
    "minecraft:pufferfish",
    "minecraft:zombified_piglin",
    "minecraft:salmon",
    "minecraft:shulker",
    "minecraft:silverfish",
    "minecraft:skeleton",
    "minecraft:slime",
    "minecraft:snow_golem",
    "minecraft:spider",
    "minecraft:squid",
    "minecraft:stray",
    "minecraft:tropical_fish",
    "minecraft:vex",
    "minecraft:villager",
    "minecraft:iron_golem",
    "minecraft:vindicator",
    "minecraft:pillager",
    "minecraft:wandering_trader",
    "minecraft:witch",
    "minecraft:wither",
    "minecraft:wither_skeleton",
    "minecraft:zombie",
    "minecraft:zombie_villager",
    "minecraft:phantom",
    "minecraft:ravager",
    "minecraft:piglin",
];
const PROJECTILES: &[&str] = &[
    "minecraft:arrow",
    "minecraft:dragon_fireball",
    "minecraft:firework_rocket",
    "minecraft:fireball",
    "minecraft:llama_spit",
    "minecraft:small_fireball",
    "minecraft:snowball",
    "minecraft:spectral_arrow",
    "minecraft:egg",
    "minecraft:ender_pearl",
    "minecraft:experience_bottle",
    "minecraft:potion",
    "minecraft:trident",
    "minecraft:wither_skull",
];
const THROWABLES: &[&str] = &[
    "minecraft:egg",
    "minecraft:ender_pearl",
    "minecraft:experience_bottle",
    "minecraft:snowball",
    "minecraft:potion",
];
const ARROWS: &[&str] = &[
    "minecraft:arrow",
    "minecraft:spectral_arrow",
    "minecraft:trident",
];
const ATTRIBUTE_RENAMES: [(&str, &str); 17] = [
    ("generic.maxHealth", "minecraft:generic.max_health"),
    ("Max Health", "minecraft:generic.max_health"),
    (
        "zombie.spawnReinforcements",
        "minecraft:zombie.spawn_reinforcements",
    ),
    (
        "Spawn Reinforcements Chance",
        "minecraft:zombie.spawn_reinforcements",
    ),
    ("horse.jumpStrength", "minecraft:horse.jump_strength"),
    ("Jump Strength", "minecraft:horse.jump_strength"),
    ("generic.followRange", "minecraft:generic.follow_range"),
    ("Follow Range", "minecraft:generic.follow_range"),
    (
        "generic.knockbackResistance",
        "minecraft:generic.knockback_resistance",
    ),
    (
        "Knockback Resistance",
        "minecraft:generic.knockback_resistance",
    ),
    ("generic.movementSpeed", "minecraft:generic.movement_speed"),
    ("Movement Speed", "minecraft:generic.movement_speed"),
    ("generic.flyingSpeed", "minecraft:generic.flying_speed"),
    ("Flying Speed", "minecraft:generic.flying_speed"),
    ("generic.attackDamage", "minecraft:generic.attack_damage"),
    (
        "generic.attackKnockback",
        "minecraft:generic.attack_knockback",
    ),
    ("generic.attackSpeed", "minecraft:generic.attack_speed"),
];

fn long_of(tag: &Tag) -> i64 {
    match *tag {
        Tag::Byte(v) => v.into(),
        Tag::Short(v) => v.into(),
        Tag::Int(v) => v.into(),
        Tag::Long(v) => v,
        Tag::Float(v) => v as i64,
        Tag::Double(v) => v as i64,
        _ => 0,
    }
}

fn uuid_tag(most: i64, least: i64) -> Tag {
    Tag::IntArray(vec![
        (most >> 32) as i32,
        most as i32,
        (least >> 32) as i32,
        least as i32,
    ])
}

fn from_longs(compound: &Compound, most: &str, least: &str) -> Option<Tag> {
    let most = compound.get(most).map_or(0, long_of);
    let least = compound.get(least).map_or(0, long_of);
    (most != 0 && least != 0).then(|| uuid_tag(most, least))
}

fn parse_uuid(text: &str) -> Option<Tag> {
    let parts: Vec<&str> = text.split('-').collect();
    let [a, b, c, d, e] = parts.as_slice() else {
        return None;
    };
    let hex = |part: &str| u64::from_str_radix(part, 16).ok();
    let (a, b, c, d, e) = (hex(a)?, hex(b)?, hex(c)?, hex(d)?, hex(e)?);
    let most = ((a & 0xffff_ffff) << 32) | ((b & 0xffff) << 16) | (c & 0xffff);
    let least = ((d & 0xffff) << 48) | (e & 0xffff_ffff_ffff);
    Some(uuid_tag(most as i64, least as i64))
}

fn replace_least_most(compound: &mut Compound, old: &str, new: &str) {
    let (most, least) = (format!("{old}Most"), format!("{old}Least"));
    if let Some(uuid) = from_longs(compound, &most, &least) {
        compound.remove(&most);
        compound.remove(&least);
        compound.insert(new, uuid);
    }
}

fn replace_string(compound: &mut Compound, old: &str, new: &str) {
    if let Some(uuid) = compound.get_str(old).and_then(parse_uuid) {
        compound.remove(old);
        compound.insert(new, uuid);
    }
}

fn replace_ml(compound: &mut Compound, old: &str, new: &str) {
    if let Some(uuid) = compound
        .get(old)
        .and_then(Tag::as_compound)
        .and_then(|ml| from_longs(ml, "M", "L"))
    {
        compound.remove(old);
        compound.insert(new, uuid);
    }
}

fn living_uuids(entity: &mut Compound) {
    for_each_compound(entity, "Attributes", |attribute| {
        for_each_compound(attribute, "Modifiers", |modifier| {
            replace_least_most(modifier, "UUID", "UUID")
        });
    });
}

fn mob_uuids(entity: &mut Compound) {
    living_uuids(entity);
    if let Some(leash) = entity.get_compound_mut("Leash") {
        replace_least_most(leash, "UUID", "UUID");
    }
}

fn animal_uuids(entity: &mut Compound) {
    mob_uuids(entity);
    replace_least_most(entity, "LoveCause", "LoveCause");
}

fn entity_uuids(entity: &mut Compound, id: &str) {
    replace_least_most(entity, "UUID", "UUID");
    if ABSTRACT_HORSES.contains(&id) || TAMEABLE_ANIMALS.contains(&id) {
        animal_uuids(entity);
        replace_string(entity, "OwnerUUID", "Owner");
    }
    if ANIMALS.contains(&id) {
        animal_uuids(entity);
    }
    if MOBS.contains(&id) {
        mob_uuids(entity);
    }
    if id == "minecraft:armor_stand" {
        living_uuids(entity);
    }
    if id == "minecraft:armor_stand" {
        living_uuids(entity);
    }
    if PROJECTILES.contains(&id) {
        super::rename(entity, "OwnerUUID", "Owner");
    }
    match id {
        "minecraft:bee" | "minecraft:zombified_piglin" => {
            replace_string(entity, "HurtBy", "HurtBy")
        }
        "minecraft:fox" => {
            if let Some(Tag::List(trusted)) = entity.remove("TrustedUUIDs") {
                let trusted = trusted
                    .into_iter()
                    .map(|tag| {
                        tag.as_compound()
                            .and_then(|ml| from_longs(ml, "M", "L"))
                            .unwrap_or(tag)
                    })
                    .collect();
                entity.insert("Trusted", Tag::List(trusted));
            }
        }
        "minecraft:item" => {
            replace_ml(entity, "Owner", "Owner");
            replace_ml(entity, "Thrower", "Thrower");
        }
        "minecraft:shulker_bullet" => {
            replace_ml(entity, "Owner", "Owner");
            replace_ml(entity, "Target", "Target");
        }
        "minecraft:area_effect_cloud" => replace_least_most(entity, "OwnerUUID", "Owner"),
        "minecraft:zombie_villager" => {
            replace_least_most(entity, "ConversionPlayer", "ConversionPlayer")
        }
        "minecraft:evoker_fangs" => replace_least_most(entity, "OwnerUUID", "Owner"),
        "minecraft:piglin" => {
            if let Some(angry) = entity
                .get_compound_mut("Brain")
                .and_then(|brain| brain.get_compound_mut("memories"))
                .and_then(|memories| memories.get_compound_mut("minecraft:angry_at"))
            {
                replace_string(angry, "value", "value");
            }
        }
        _ => {}
    }
}

fn projectile_owner(entity: &mut Compound, id: &str) {
    let (most, least) = if THROWABLES.contains(&id) {
        let owner = entity.remove("owner");
        let owner = owner.as_ref().and_then(Tag::as_compound);
        let get = |key: &str| owner.and_then(|o| o.get(key)).map_or(0, long_of);
        (get("M"), get("L"))
    } else if id == "minecraft:llama_spit" {
        let owner = entity.remove("Owner");
        let owner = owner.as_ref().and_then(Tag::as_compound);
        let get = |key: &str| owner.and_then(|o| o.get(key)).map_or(0, long_of);
        (get("OwnerUUIDMost"), get("OwnerUUIDLeast"))
    } else if ARROWS.contains(&id) {
        let get = |entity: &mut Compound, key: &str| entity.remove(key).as_ref().map_or(0, long_of);
        let most = get(entity, "OwnerUUIDMost");
        (most, get(entity, "OwnerUUIDLeast"))
    } else {
        return;
    };
    if most != 0 && least != 0 {
        entity.insert("OwnerUUID", uuid_tag(most, least));
    }
}

fn rename_attributes(attributes: Option<&mut Vec<Tag>>, key: &str) {
    for attribute in attributes.into_iter().flatten() {
        if let Some(attribute) = attribute.as_compound_mut()
            && let Some(Tag::String(name)) = attribute.get_mut(key)
            && let Some((_, new)) = ATTRIBUTE_RENAMES.iter().find(|(old, _)| old == name)
        {
            *name = (*new).to_owned();
        }
    }
}

fn wrap_memories(entity: &mut Compound) {
    let Some(memories) = entity
        .get_compound_mut("Brain")
        .and_then(|brain| brain.get_compound_mut("memories"))
    else {
        return;
    };
    let keys: Vec<String> = memories.keys().map(str::to_owned).collect();
    for key in keys {
        if let Some(value) = memories.remove(&key) {
            memories.insert(key, Compound::new().with("value", value));
        }
    }
}

fn rotate_shulker(entity: &mut Compound) {
    let Some(rotation) = entity.get_list_mut("Rotation") else {
        return;
    };
    let mut angles: Vec<f32> = rotation
        .iter()
        .map(|tag| super::number(tag).unwrap_or(180.0) as f32)
        .collect();
    let Some(yaw) = angles.first_mut() else {
        return;
    };
    *yaw -= 180.0;
    *rotation = angles.into_iter().map(Tag::Float).collect();
}

fn spawner_data(spawner: &mut Compound) {
    if let Some(Tag::Compound(entity)) = spawner.remove("SpawnData") {
        spawner.insert("SpawnData", Compound::new().with("entity", entity));
    }
    if let Some(Tag::List(potentials)) = spawner.remove("SpawnPotentials") {
        let wrapped = potentials
            .into_iter()
            .map(|tag| {
                let Tag::Compound(mut potential) = tag else {
                    return tag;
                };
                let weight = potential.remove("Weight").as_ref().map_or(1, long_of) as i32;
                let mut data = Compound::new();
                if let Some(entity) = potential.remove("Entity") {
                    data.insert("entity", entity);
                }
                let mut out = potential;
                out.insert("weight", weight);
                out.insert("data", data);
                Tag::Compound(out)
            })
            .collect();
        spawner.insert("SpawnPotentials", Tag::List(wrapped));
    }
}

const XP_THRESHOLDS: [i32; 5] = [0, 10, 50, 100, 150];

fn min_xp(level: i64) -> i32 {
    XP_THRESHOLDS[(level - 1).clamp(0, 4) as usize]
}

fn profession(profession: i64, career: i64) -> &'static str {
    match (profession, career) {
        (0, 2) => "minecraft:fisherman",
        (0, 3) => "minecraft:shepherd",
        (0, 4) => "minecraft:fletcher",
        (0, _) => "minecraft:farmer",
        (1, 2) => "minecraft:cartographer",
        (1, _) => "minecraft:librarian",
        (2, _) => "minecraft:cleric",
        (3, 2) => "minecraft:weaponsmith",
        (3, 3) => "minecraft:toolsmith",
        (3, _) => "minecraft:armorer",
        (4, 2) => "minecraft:leatherworker",
        (4, _) => "minecraft:butcher",
        (5, _) => "minecraft:nitwit",
        _ => "minecraft:none",
    }
}

fn villager_data(entity: &mut Compound) {
    let number = |entity: &Compound, key: &str| get_number(entity, key).map_or(0, |v| v as i64);
    let kind = profession(number(entity, "Profession"), number(entity, "Career"));
    let level = entity.remove("CareerLevel");
    entity.remove("Profession");
    entity.remove("Career");
    entity.insert(
        "VillagerData",
        Compound::new()
            .with("type", "minecraft:plains")
            .with("profession", kind)
            .with("level", level.unwrap_or(Tag::Int(1))),
    );
}

fn villager_level_and_xp(entity: &mut Compound) {
    let mut level = entity
        .get_compound("VillagerData")
        .and_then(|data| get_number(data, "level"))
        .map_or(0, |v| v as i64);
    if level <= 1 {
        let offers = entity
            .get_compound("Offers")
            .and_then(|offers| offers.get_list("Recipes"))
            .map_or(0, <[Tag]>::len) as i64;
        level = (offers / 2).clamp(1, 5);
        if level > 1
            && let Some(data) = entity.get_compound_mut("VillagerData")
        {
            data.insert("level", level as i32);
        }
    }
    if get_number(entity, "Xp").is_none() {
        entity.insert("Xp", min_xp(level));
    }
}

fn zombie_villager_xp(entity: &mut Compound) {
    if get_number(entity, "Xp").is_none() {
        let level = entity
            .get_compound("VillagerData")
            .and_then(|data| get_number(data, "level"))
            .map_or(1, |v| v as i64);
        entity.insert("Xp", min_xp(level));
    }
}

fn early_entity(entity: &mut Compound, fixer: Fixer) {
    let mut id = id_of(entity);
    if fixer.due(1904, 0) && id == "minecraft:ocelot" {
        let kind = get_number(entity, "CatType").map_or(0, |v| v as i32);
        if kind == 0 {
            let named = |key: &str| entity.get_str(key).is_some_and(|v| !v.is_empty());
            if named("Owner") || named("OwnerUUID") {
                entity.insert("Trusting", true);
            }
        } else if (1..4).contains(&kind) {
            let owner = entity.get_str("OwnerUUID").unwrap_or("").to_owned();
            entity.insert("id", "minecraft:cat");
            entity.insert("CatType", kind);
            entity.insert("OwnerUUID", owner);
            id = "minecraft:cat".into();
        }
    }
    if fixer.due(1917, 0) && id == "minecraft:cat" && get_number(entity, "CatType") == Some(9.0) {
        entity.insert("CatType", 10);
    }
    let villager = matches!(
        id.as_str(),
        "minecraft:villager" | "minecraft:zombie_villager"
    );
    if fixer.due(1918, 0) && villager {
        villager_data(entity);
    }
    if fixer.due(1928, 0) && id == "minecraft:illager_beast" {
        entity.insert("id", "minecraft:ravager");
    }
    if fixer.due(1955, 0) {
        match id.as_str() {
            "minecraft:villager" => villager_level_and_xp(entity),
            "minecraft:zombie_villager" => zombie_villager_xp(entity),
            _ => {}
        }
    }
    if fixer.due(1963, 0)
        && id == "minecraft:villager"
        && let Some(gossips) = entity.get_list_mut("Gossips")
    {
        gossips.retain(|gossip| {
            gossip
                .as_compound()
                .is_none_or(|g| g.get_str("Type") != Some("golem"))
        });
    }
}

/// `LevelUUIDFix`: the wandering trader, dragon and boss bar player ids.
pub fn level(data: &mut Compound, fixer: Fixer) -> Result<(), DataFixError> {
    legacy_world_gen_settings(data, fixer)?;
    if !fixer.due(2514, 0) {
        return Ok(());
    }
    replace_string(data, "WanderingTraderId", "WanderingTraderId");
    if let Some(dimensions) = data.get_compound_mut("DimensionData") {
        for (_, dimension) in dimensions.iter_mut() {
            if let Some(fight) = dimension
                .as_compound_mut()
                .and_then(|d| d.get_compound_mut("DragonFight"))
            {
                replace_least_most(fight, "DragonUUID", "Dragon");
            }
        }
    }
    if let Some(events) = data.get_compound_mut("CustomBossEvents") {
        for (_, event) in events.iter_mut() {
            if let Some(players) = event
                .as_compound_mut()
                .and_then(|e| e.get_list_mut("Players"))
            {
                for player in players {
                    if let Some(uuid) = player.as_compound().and_then(|p| from_longs(p, "M", "L")) {
                        *player = uuid;
                    }
                }
            }
        }
    }
    Ok(())
}

const OLD_WORLD_GEN_KEYS: [&str; 7] = [
    "RandomSeed",
    "generatorName",
    "generatorOptions",
    "generatorVersion",
    "legacy_custom_options",
    "MapFeatures",
    "BonusChest",
];

/// `LevelLegacyWorldGenSettingsFix` (2538) moves the loose generator keys
/// into `WorldGenSettings`; the seed, structure and bonus chest flags then
/// take their `WorldGenSettingsFix` (2550) names, so the seed survives.
fn legacy_world_gen_settings(data: &mut Compound, fixer: Fixer) -> Result<(), DataFixError> {
    let moving = fixer.due(2538, 0);
    let mut settings = match data.remove("WorldGenSettings") {
        Some(Tag::Compound(settings)) => settings,
        _ if moving => Compound::new(),
        _ => return Ok(()),
    };
    if moving {
        for key in OLD_WORLD_GEN_KEYS {
            if let Some(value) = data.remove(key) {
                settings.insert(key, value);
            }
        }
    }
    if fixer.due(2550, 0) {
        settings = build_world_gen_settings(settings);
    }
    for (version, value) in [(2693, false), (2707, true), (2825, false)] {
        if fixer.due(version, 0) && !settings.contains_key(WAS_INCREASED_KEY) {
            settings.insert(WAS_INCREASED_KEY, value);
        }
    }
    if fixer.due(2832, 0) {
        fix_height_and_biome(&mut settings);
    }
    for version in [2833, 2852] {
        if fixer.due(version, 0) {
            disallow_old_custom_worlds(&settings)?;
        }
    }
    if fixer.due(2967, 0) {
        flatten_structure_settings(&mut settings);
    }
    data.insert("WorldGenSettings", settings);
    Ok(())
}

/// `SavedDataUUIDFix`: the heroes of the village of every raid.
pub fn raids(data: &mut Compound, fixer: Fixer) {
    if !fixer.due(2514, 0) {
        return;
    }
    for_each_compound(data, "Raids", |raid| {
        if let Some(heroes) = raid.get_list_mut("HeroesOfTheVillage") {
            for hero in heroes {
                if let Some(uuid) = hero
                    .as_compound()
                    .and_then(|h| from_longs(h, "UUIDMost", "UUIDLeast"))
                {
                    *hero = uuid;
                }
            }
        }
    });
}

/// Entity fixes in version order. The entity id is returned renamed.
pub fn entity(entity: &mut Compound, fixer: Fixer) {
    if fixer.from() >= 2860 {
        return;
    }
    early_entity(entity, fixer);
    if fixer.due(2509, 0) && id_of(entity) == "minecraft:zombie_pigman" {
        entity.insert("id", "minecraft:zombified_piglin");
    }
    let id = id_of(entity);
    if fixer.due(2505, 0) && id == "minecraft:villager" {
        wrap_memories(entity);
    }
    if fixer.due(2511, 0) {
        projectile_owner(entity, &id);
    }
    if fixer.due(2514, 0) {
        entity_uuids(entity, &id);
    }
    if fixer.due(2516, 0)
        && matches!(
            id.as_str(),
            "minecraft:villager" | "minecraft:zombie_villager"
        )
    {
        for_each_compound(entity, "Gossips", |gossip| {
            replace_least_most(gossip, "Target", "Target")
        });
    }
    if fixer.due(2523, 0) {
        rename_attributes(entity.get_list_mut("Attributes"), "Name");
    }
    if fixer.due(2529, 0)
        && id == "minecraft:strider"
        && get_number(entity, "NoGravity").is_some_and(|v| v != 0.0)
    {
        entity.insert("NoGravity", 0i8);
    }
    if fixer.due(2533, 0) && id == "minecraft:villager" {
        for_each_compound(entity, "Attributes", |attribute| {
            if attribute.get_str("Name") == Some("generic.follow_range")
                && get_number(attribute, "Base") == Some(16.0)
            {
                attribute.insert("Base", 48.0f64);
            }
        });
    }
    if fixer.due(2535, 0) && id == "minecraft:shulker" {
        rotate_shulker(entity);
    }
    if fixer.due(2702, 0) && ARROWS.contains(&id.as_str()) && !entity.contains_key("pickup") {
        let from_player = get_number(entity, "player").is_none_or(|v| v != 0.0);
        entity.remove("player");
        entity.insert("pickup", i8::from(from_player));
    }
    if fixer.due(2831, 0) && id == "minecraft:spawner_minecart" {
        spawner_data(entity);
    }
}

pub fn player(player: &mut Compound, fixer: Fixer) {
    if fixer.from() >= 2860 {
        return;
    }
    if fixer.due(2100, 0)
        && let Some(book) = player.get_compound_mut("recipeBook")
    {
        for key in ["recipes", "toBeDisplayed"] {
            for recipe in book.get_list_mut(key).into_iter().flatten() {
                if let Tag::String(name) = recipe
                    && super::namespaced(name) == "minecraft:sugar"
                {
                    *name = "minecraft:sugar_from_sugar_cane".to_owned();
                }
            }
        }
    }
    if fixer.due(2514, 0) {
        replace_least_most(player, "UUID", "UUID");
        if let Some(vehicle) = player.get_compound_mut("RootVehicle") {
            replace_least_most(vehicle, "Attach", "Attach");
        }
        living_uuids(player);
    }
    if fixer.due(2523, 0) {
        rename_attributes(player.get_list_mut("Attributes"), "Name");
    }
    if fixer.due(2537, 0) {
        legacy_dimension_id(player, "Dimension");
    }
}

/// Numeric dimension ids become names: -1 the nether, 1 the end, any other
/// number the overworld.
pub(super) fn legacy_dimension_id(compound: &mut Compound, key: &str) {
    let Some(id) = compound.get(key).and_then(super::number) else {
        return;
    };
    let name = match id as i32 {
        -1 => "minecraft:the_nether",
        1 => "minecraft:the_end",
        _ => "minecraft:overworld",
    };
    compound.insert(key, name);
}

pub fn block_entity(be: &mut Compound, fixer: Fixer) {
    if fixer.from() >= 2860 {
        return;
    }
    let id = id_of(be);
    if fixer.due(1914, 0)
        && id == "minecraft:chest"
        && be.get_str("LootTable") == Some("minecraft:chests/village_blacksmith")
    {
        be.insert("LootTable", "minecraft:chests/village/village_weaponsmith");
    }
    if fixer.due(1953, 0) && id == "minecraft:banner" {
        ominous_banner_name(be, "CustomName");
    }
    if fixer.due(2501, 0)
        && matches!(
            id.as_str(),
            "minecraft:furnace" | "minecraft:blast_furnace" | "minecraft:smoker"
        )
    {
        furnace_recipes(be);
    }
    if fixer.due(2514, 0) {
        match id.as_str() {
            "minecraft:conduit" => replace_ml(be, "target_uuid", "Target"),
            "minecraft:skull" => {
                if let Some(Tag::Compound(mut owner)) = be.remove("Owner") {
                    replace_string(&mut owner, "Id", "Id");
                    be.insert("SkullOwner", owner);
                }
            }
            _ => {}
        }
    }
    if fixer.due(2518, 0) && id == "minecraft:jigsaw" {
        let name = be
            .get_str("attachement_type")
            .unwrap_or("minecraft:empty")
            .to_owned();
        let pool = be
            .get_str("target_pool")
            .unwrap_or("minecraft:empty")
            .to_owned();
        be.remove("attachement_type");
        be.remove("target_pool");
        be.insert("name", name.as_str());
        be.insert("target", name);
        be.insert("pool", pool);
    }
    if fixer.due(2831, 0) && id == "minecraft:mob_spawner" {
        spawner_data(be);
    }
}

fn furnace_recipes(be: &mut Compound) {
    let size = be.remove("RecipesUsedSize").as_ref().map_or(0, long_of);
    let mut used = Compound::new();
    for i in 0..size.clamp(0, 4096) {
        let location = be.remove(&format!("RecipeLocation{i}"));
        let amount = be
            .remove(&format!("RecipeAmount{i}"))
            .as_ref()
            .map_or(0, long_of);
        if amount > 0
            && let Some(Tag::String(location)) = location
        {
            used.insert(namespaced(&location), amount as i32);
        }
    }
    be.insert("RecipesUsed", used);
}

fn ominous_banner_name(compound: &mut Compound, key: &str) {
    if let Some(Tag::String(name)) = compound.get_mut(key) {
        *name = name.replace(
            "\"translate\":\"block.minecraft.illager_banner\"",
            "\"translate\":\"block.minecraft.ominous_banner\"",
        );
    }
}

/// Item fixes that act on the item's `tag`.
pub fn item(item: &mut Compound, fixer: Fixer) {
    if fixer.from() >= 2860 {
        return;
    }
    let id = id_of(item);
    let player_head = id == "minecraft:player_head";
    let Some(tag) = item.get_compound_mut("tag") else {
        return;
    };
    if fixer.due(1803, 0)
        && let Some(lore) = tag
            .get_compound_mut("display")
            .and_then(|display| display.get_list_mut("Lore"))
    {
        for line in lore {
            if let Tag::String(text) = line {
                *text = super::text::json_text(text);
            }
        }
    }
    if fixer.due(1948, 0)
        && id == "minecraft:white_banner"
        && let Some(display) = tag.get_compound_mut("display")
    {
        ominous_banner_name(display, "Name");
    }
    if fixer.due(2514, 0) {
        for_each_compound(tag, "AttributeModifiers", |modifier| {
            replace_least_most(modifier, "UUID", "UUID")
        });
        if player_head && let Some(owner) = tag.get_compound_mut("SkullOwner") {
            replace_string(owner, "Id", "Id");
        }
    }
    if fixer.due(2523, 0) {
        rename_attributes(tag.get_list_mut("AttributeModifiers"), "AttributeName");
    }
}

const WAS_INCREASED_KEY: &str = "has_increased_height_already";

fn long_of_tag(tag: Option<&Tag>) -> i64 {
    tag.and_then(Tag::as_long).unwrap_or(0)
}

fn flag_of(settings: &mut Compound, key: &str, default: bool) -> bool {
    settings
        .remove(key)
        .as_ref()
        .and_then(super::number)
        .map_or(default, |v| v != 0.0)
}

fn noise_generator(seed: i64, settings: &str, biome_source: Compound) -> Compound {
    Compound::new()
        .with("type", "minecraft:noise")
        .with("biome_source", biome_source)
        .with("seed", seed)
        .with("settings", settings)
}

fn vanilla_biome_source(seed: i64, legacy_init_layer: bool, large_biomes: bool) -> Compound {
    let mut source = Compound::new()
        .with("type", "minecraft:vanilla_layered")
        .with("seed", seed)
        .with("large_biomes", large_biomes);
    if legacy_init_layer {
        source.insert("legacy_biome_init_layer", true);
    }
    source
}

fn vanilla_levels(seed: i64, overworld: Compound, caves: bool) -> Compound {
    let overworld_type = if caves {
        "minecraft:overworld_caves"
    } else {
        "minecraft:overworld"
    };
    let nether = noise_generator(
        seed,
        "minecraft:nether",
        Compound::new()
            .with("type", "minecraft:multi_noise")
            .with("seed", seed)
            .with("preset", "minecraft:nether"),
    );
    let end = noise_generator(
        seed,
        "minecraft:end",
        Compound::new()
            .with("type", "minecraft:the_end")
            .with("seed", seed),
    );
    let dimension = |kind: &str, generator: Compound| {
        Compound::new()
            .with("type", kind)
            .with("generator", generator)
    };
    Compound::new()
        .with("minecraft:overworld", dimension(overworld_type, overworld))
        .with(
            "minecraft:the_nether",
            dimension("minecraft:the_nether", nether),
        )
        .with("minecraft:the_end", dimension("minecraft:the_end", end))
}

/// `WorldGenSettingsFix` (2550): the loose generator keys gathered by
/// 2538 become the seed, flags and the three vanilla dimensions.
fn build_world_gen_settings(mut old: Compound) -> Compound {
    let seed = long_of_tag(old.get("RandomSeed"));
    let name = old.get_str("generatorName").map(str::to_lowercase);
    let options = old.get("generatorOptions").cloned();
    let custom = match old.remove("legacy_custom_options") {
        Some(Tag::String(custom)) => Some(custom),
        _ => (name.as_deref() == Some("customized"))
            .then(|| options.as_ref().and_then(|o| o.as_str().map(str::to_owned)))
            .flatten(),
    };
    let default_overworld = || {
        noise_generator(
            seed,
            "minecraft:overworld",
            vanilla_biome_source(seed, false, false),
        )
    };
    let mut caves = false;
    let generator = match name.as_deref() {
        None | Some("customized") => default_overworld(),
        Some("flat") => flat_generator(options.as_ref()),
        Some("debug_all_block_states") => Compound::new().with("type", "minecraft:debug"),
        Some("buffet") => {
            let buffet = options.as_ref().and_then(Tag::as_compound);
            let kind = buffet
                .and_then(|b| b.get_compound("chunk_generator"))
                .and_then(|g| g.get_str("type"));
            let settings = match kind {
                Some("minecraft:caves") => {
                    caves = true;
                    "minecraft:caves"
                }
                Some("minecraft:floating_islands") => "minecraft:floating_islands",
                _ => "minecraft:overworld",
            };
            let mut source = buffet
                .and_then(|b| b.get_compound("biome_source"))
                .cloned()
                .unwrap_or_else(|| Compound::new().with("type", "minecraft:fixed"));
            if source.get_str("type") == Some("minecraft:fixed") {
                let biome = source
                    .get_compound("options")
                    .and_then(|o| o.get_list("biomes"))
                    .and_then(|biomes| biomes.first())
                    .and_then(Tag::as_str)
                    .unwrap_or("minecraft:ocean")
                    .to_owned();
                source.remove("options");
                source.insert("biome", biome);
            }
            noise_generator(seed, settings, source)
        }
        Some(other) => {
            let normal = other == "default";
            let version = old.get("generatorVersion").and_then(super::number);
            let legacy = other == "default_1_1" || (normal && version.unwrap_or(0.0) as i32 == 0);
            let settings = if other == "amplified" {
                "minecraft:amplified"
            } else {
                "minecraft:overworld"
            };
            noise_generator(
                seed,
                settings,
                vanilla_biome_source(seed, legacy, other == "largebiomes"),
            )
        }
    };
    let features = flag_of(&mut old, "MapFeatures", true);
    let bonus = flag_of(&mut old, "BonusChest", false);
    let mut settings = Compound::new()
        .with("seed", seed)
        .with("generate_features", features)
        .with("bonus_chest", bonus)
        .with("dimensions", vanilla_levels(seed, generator, caves));
    if let Some(custom) = custom {
        settings.insert("legacy_custom_options", custom);
    }
    settings
}

struct SpreadConfig {
    spacing: i32,
    separation: i32,
    salt: i32,
}

fn default_spread(structure: &str) -> Option<SpreadConfig> {
    let (spacing, separation, salt) = match structure {
        "minecraft:village" => (32, 8, 10387312),
        "minecraft:desert_pyramid" => (32, 8, 14357617),
        "minecraft:igloo" => (32, 8, 14357618),
        "minecraft:jungle_pyramid" => (32, 8, 14357619),
        "minecraft:swamp_hut" => (32, 8, 14357620),
        "minecraft:pillager_outpost" => (32, 8, 165745296),
        "minecraft:monument" => (32, 5, 10387313),
        "minecraft:endcity" => (20, 11, 10387313),
        "minecraft:mansion" => (80, 20, 10387319),
        _ => return None,
    };
    Some(SpreadConfig {
        spacing,
        separation,
        salt,
    })
}

fn flat_generator(options: Option<&Tag>) -> Compound {
    let flat = options.and_then(Tag::as_compound);
    let layers = flat
        .and_then(|f| f.get("layers"))
        .cloned()
        .unwrap_or_else(|| {
            let layer = |height: i32, block: &str| {
                Tag::Compound(Compound::new().with("height", height).with("block", block))
            };
            Tag::List(vec![
                layer(1, "minecraft:bedrock"),
                layer(2, "minecraft:dirt"),
                layer(1, "minecraft:grass_block"),
            ])
        });
    let biome = flat
        .and_then(|f| f.get_str("biome"))
        .unwrap_or("minecraft:plains")
        .to_owned();
    Compound::new().with("type", "minecraft:flat").with(
        "settings",
        Compound::new()
            .with("structures", flat_structures(options))
            .with("layers", layers)
            .with("biome", biome),
    )
}

fn flat_structures(options: Option<&Tag>) -> Compound {
    fn number_option(value: &str, default: i32, min: i32) -> i32 {
        value.parse::<i32>().unwrap_or(default).max(min)
    }
    fn set_spacing(
        configs: &mut BTreeMap<&'static str, SpreadConfig>,
        structure: &'static str,
        value: &str,
        min: i32,
    ) {
        let current = configs
            .remove(structure)
            .or_else(|| default_spread(structure))
            .expect("known structure");
        configs.insert(
            structure,
            SpreadConfig {
                spacing: number_option(value, current.spacing, min),
                ..current
            },
        );
    }
    let (mut distance, mut spread, mut count) = (32, 3, 128);
    let mut stronghold = false;
    let mut configs: BTreeMap<&'static str, SpreadConfig> = BTreeMap::new();
    if options.is_none() {
        stronghold = true;
        configs.insert(
            "minecraft:village",
            default_spread("minecraft:village").expect("village"),
        );
    }
    let structures = options
        .and_then(Tag::as_compound)
        .and_then(|o| o.get_compound("structures"));
    for (structure, settings) in structures.into_iter().flat_map(Compound::iter) {
        let Some(settings) = settings.as_compound() else {
            continue;
        };
        for (option, value) in settings.iter() {
            let value = value.as_str().unwrap_or("");
            if structure == "stronghold" {
                stronghold = true;
                match option {
                    "distance" => distance = number_option(value, distance, 1),
                    "spread" => spread = number_option(value, spread, 1),
                    "count" => count = number_option(value, count, 1),
                    _ => {}
                }
            }
            match (option, structure) {
                ("distance", "village") => set_spacing(&mut configs, "minecraft:village", value, 9),
                ("distance", "biome_1") => {
                    for name in [
                        "minecraft:desert_pyramid",
                        "minecraft:igloo",
                        "minecraft:jungle_pyramid",
                        "minecraft:swamp_hut",
                        "minecraft:pillager_outpost",
                    ] {
                        set_spacing(&mut configs, name, value, 9);
                    }
                }
                ("distance", "endcity") => set_spacing(&mut configs, "minecraft:endcity", value, 1),
                ("distance", "mansion") => set_spacing(&mut configs, "minecraft:mansion", value, 1),
                ("separation", "oceanmonument") => {
                    let current = configs
                        .remove("minecraft:monument")
                        .or_else(|| default_spread("minecraft:monument"))
                        .expect("monument");
                    configs.insert(
                        "minecraft:monument",
                        SpreadConfig {
                            spacing: number_option(value, current.separation, 1),
                            ..current
                        },
                    );
                }
                ("spacing", "oceanmonument") => {
                    set_spacing(&mut configs, "minecraft:monument", value, 1)
                }
                _ => {}
            }
        }
    }
    let mut configured = Compound::new();
    for (name, config) in configs {
        configured.insert(
            name,
            Compound::new()
                .with("spacing", config.spacing)
                .with("separation", config.separation)
                .with("salt", config.salt),
        );
    }
    let mut out = Compound::new().with("structures", configured);
    if stronghold {
        out.insert(
            "stronghold",
            Compound::new()
                .with("distance", distance)
                .with("spread", spread)
                .with("count", count),
        );
    }
    out
}

/// `WorldGenSettingsHeightAndBiomeFix` (2832).
fn fix_height_and_biome(settings: &mut Compound) {
    let flag = settings.remove(WAS_INCREASED_KEY);
    let was_experimental_snapshot = flag.is_none();
    let was_previously_increased = flag
        .as_ref()
        .and_then(super::number)
        .is_none_or(|v| v != 0.0);
    let Some(generator) = settings
        .get_compound_mut("dimensions")
        .and_then(|d| d.get_compound_mut("minecraft:overworld"))
        .and_then(|d| d.get_compound_mut("generator"))
    else {
        return;
    };
    match generator.get_str("type") {
        Some("minecraft:noise") => {
            let mut large_biomes = false;
            if let Some(source) = generator.get_compound_mut("biome_source") {
                let kind = source.get_str("type");
                if kind == Some("minecraft:vanilla_layered")
                    || (was_experimental_snapshot && kind == Some("minecraft:multi_noise"))
                {
                    large_biomes = source
                        .get("large_biomes")
                        .and_then(super::number)
                        .is_some_and(|v| v != 0.0);
                    *source = Compound::new()
                        .with("preset", "minecraft:overworld")
                        .with("type", "minecraft:multi_noise");
                }
            }
            if large_biomes && generator.get_str("settings") == Some("minecraft:overworld") {
                generator.insert("settings", "minecraft:large_biomes");
            }
        }
        Some("minecraft:flat") if !was_previously_increased => {
            if let Some(layers) = generator
                .get_compound_mut("settings")
                .and_then(|s| s.get_list_mut("layers"))
            {
                layers.insert(
                    0,
                    Tag::Compound(
                        Compound::new()
                            .with("height", 64)
                            .with("block", "minecraft:air"),
                    ),
                );
            }
        }
        _ => {}
    }
}

/// `WorldGenSettingsDisallowOldCustomWorldsFix` (2833, 2852): a dimension
/// without a type is an old custom world, which cannot be loaded.
fn disallow_old_custom_worlds(settings: &Compound) -> Result<(), DataFixError> {
    let Some(dimensions) = settings.get_compound("dimensions") else {
        return Ok(());
    };
    for (_, dimension) in dimensions.iter() {
        if let Some(dimension) = dimension.as_compound()
            && !matches!(dimension.get("type"), Some(Tag::String(_)))
        {
            return Err(DataFixError::OldCustomWorld);
        }
    }
    Ok(())
}

/// `StructureSettingsFlattenFix` (2967).
fn flatten_structure_settings(settings: &mut Compound) {
    let Some(dimensions) = settings.get_compound_mut("dimensions") else {
        return;
    };
    for (_, dimension) in dimensions.iter_mut() {
        let Some(structures) = dimension
            .as_compound_mut()
            .and_then(|d| d.get_compound_mut("generator"))
            .and_then(|g| g.get_compound_mut("settings"))
            .and_then(|s| s.get_compound_mut("structures"))
        else {
            continue;
        };
        let mut flat = match structures.remove("structures") {
            Some(Tag::Compound(inner)) => inner,
            _ => Compound::new(),
        };
        for (_, value) in flat.iter_mut() {
            if let Some(value) = value.as_compound_mut() {
                value.insert("type", "minecraft:random_spread");
            }
        }
        if let Some(Tag::Compound(mut stronghold)) = structures.remove("stronghold") {
            stronghold.insert("type", "minecraft:concentric_rings");
            flat.insert("minecraft:stronghold", stronghold);
        }
        *structures = flat;
    }
}
