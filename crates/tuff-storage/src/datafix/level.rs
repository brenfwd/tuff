//! `level.dat`, saved data, statistics and advancements. Level data older
//! than the 26.1 split is first brought to the form the split reads
//! ([`prepare`]); the split itself moves files and lives in
//! [`super::layout`].

use tuff_vanilla_data::version::DATA_VERSION;

use super::{
    DataFixError, Fixer, entity, fix_block_pos, for_each_compound, get_bool, get_number,
    inline_block_pos, rename, rename_with, text,
};
use tuff_nbt::{Compound, Tag};

/// The data version that split `level.dat` into saved data files and moved
/// every dimension under `dimensions/`.
pub const SPLIT_VERSION: i32 = 4772;

#[derive(Clone, Copy)]
enum Rule {
    Bool,
    Inverted,
    Int(i32, i32),
}

const RULE_RENAMES: &[(&str, &str, Rule)] = &[
    (
        "allowEnteringNetherUsingPortals",
        "minecraft:allow_entering_nether_using_portals",
        Rule::Bool,
    ),
    (
        "announceAdvancements",
        "minecraft:show_advancement_messages",
        Rule::Bool,
    ),
    (
        "blockExplosionDropDecay",
        "minecraft:block_explosion_drop_decay",
        Rule::Bool,
    ),
    (
        "commandBlockOutput",
        "minecraft:command_block_output",
        Rule::Bool,
    ),
    (
        "enableCommandBlocks",
        "minecraft:command_blocks_work",
        Rule::Bool,
    ),
    (
        "commandBlocksEnabled",
        "minecraft:command_blocks_work",
        Rule::Bool,
    ),
    (
        "commandModificationBlockLimit",
        "minecraft:max_block_modifications",
        Rule::Int(1, i32::MAX),
    ),
    (
        "disableElytraMovementCheck",
        "minecraft:elytra_movement_check",
        Rule::Inverted,
    ),
    (
        "disablePlayerMovementCheck",
        "minecraft:player_movement_check",
        Rule::Inverted,
    ),
    ("disableRaids", "minecraft:raids", Rule::Inverted),
    ("doDaylightCycle", "minecraft:advance_time", Rule::Bool),
    ("doEntityDrops", "minecraft:entity_drops", Rule::Bool),
    (
        "doImmediateRespawn",
        "minecraft:immediate_respawn",
        Rule::Bool,
    ),
    ("doInsomnia", "minecraft:spawn_phantoms", Rule::Bool),
    (
        "doLimitedCrafting",
        "minecraft:limited_crafting",
        Rule::Bool,
    ),
    ("doMobLoot", "minecraft:mob_drops", Rule::Bool),
    ("doMobSpawning", "minecraft:spawn_mobs", Rule::Bool),
    ("doPatrolSpawning", "minecraft:spawn_patrols", Rule::Bool),
    ("doTileDrops", "minecraft:block_drops", Rule::Bool),
    (
        "doTraderSpawning",
        "minecraft:spawn_wandering_traders",
        Rule::Bool,
    ),
    ("doVinesSpread", "minecraft:spread_vines", Rule::Bool),
    ("doWardenSpawning", "minecraft:spawn_wardens", Rule::Bool),
    ("doWeatherCycle", "minecraft:advance_weather", Rule::Bool),
    ("drowningDamage", "minecraft:drowning_damage", Rule::Bool),
    (
        "enderPearlsVanishOnDeath",
        "minecraft:ender_pearls_vanish_on_death",
        Rule::Bool,
    ),
    ("fallDamage", "minecraft:fall_damage", Rule::Bool),
    ("fireDamage", "minecraft:fire_damage", Rule::Bool),
    (
        "forgiveDeadPlayers",
        "minecraft:forgive_dead_players",
        Rule::Bool,
    ),
    ("freezeDamage", "minecraft:freeze_damage", Rule::Bool),
    (
        "globalSoundEvents",
        "minecraft:global_sound_events",
        Rule::Bool,
    ),
    ("keepInventory", "minecraft:keep_inventory", Rule::Bool),
    (
        "lavaSourceConversion",
        "minecraft:lava_source_conversion",
        Rule::Bool,
    ),
    ("locatorBar", "minecraft:locator_bar", Rule::Bool),
    (
        "logAdminCommands",
        "minecraft:log_admin_commands",
        Rule::Bool,
    ),
    (
        "maxCommandChainLength",
        "minecraft:max_command_sequence_length",
        Rule::Int(0, i32::MAX),
    ),
    (
        "maxCommandForkCount",
        "minecraft:max_command_forks",
        Rule::Int(0, i32::MAX),
    ),
    (
        "maxEntityCramming",
        "minecraft:max_entity_cramming",
        Rule::Int(0, i32::MAX),
    ),
    (
        "minecartMaxSpeed",
        "minecraft:max_minecart_speed",
        Rule::Int(i32::MIN, i32::MAX),
    ),
    (
        "mobExplosionDropDecay",
        "minecraft:mob_explosion_drop_decay",
        Rule::Bool,
    ),
    ("mobGriefing", "minecraft:mob_griefing", Rule::Bool),
    (
        "naturalRegeneration",
        "minecraft:natural_health_regeneration",
        Rule::Bool,
    ),
    (
        "playersNetherPortalCreativeDelay",
        "minecraft:players_nether_portal_creative_delay",
        Rule::Int(0, i32::MAX),
    ),
    (
        "playersNetherPortalDefaultDelay",
        "minecraft:players_nether_portal_default_delay",
        Rule::Int(0, i32::MAX),
    ),
    (
        "playersSleepingPercentage",
        "minecraft:players_sleeping_percentage",
        Rule::Int(0, i32::MAX),
    ),
    (
        "projectilesCanBreakBlocks",
        "minecraft:projectiles_can_break_blocks",
        Rule::Bool,
    ),
    ("pvp", "minecraft:pvp", Rule::Bool),
    (
        "randomTickSpeed",
        "minecraft:random_tick_speed",
        Rule::Int(0, i32::MAX),
    ),
    (
        "reducedDebugInfo",
        "minecraft:reduced_debug_info",
        Rule::Bool,
    ),
    (
        "sendCommandFeedback",
        "minecraft:send_command_feedback",
        Rule::Bool,
    ),
    (
        "showDeathMessages",
        "minecraft:show_death_messages",
        Rule::Bool,
    ),
    (
        "snowAccumulationHeight",
        "minecraft:max_snow_accumulation_height",
        Rule::Int(0, 8),
    ),
    ("spawnMonsters", "minecraft:spawn_monsters", Rule::Bool),
    (
        "spawnRadius",
        "minecraft:respawn_radius",
        Rule::Int(i32::MIN, i32::MAX),
    ),
    (
        "spawnerBlocksEnabled",
        "minecraft:spawner_blocks_work",
        Rule::Bool,
    ),
    (
        "spectatorsGenerateChunks",
        "minecraft:spectators_generate_chunks",
        Rule::Bool,
    ),
    ("tntExplodes", "minecraft:tnt_explodes", Rule::Bool),
    (
        "tntExplosionDropDecay",
        "minecraft:tnt_explosion_drop_decay",
        Rule::Bool,
    ),
    ("universalAnger", "minecraft:universal_anger", Rule::Bool),
    (
        "waterSourceConversion",
        "minecraft:water_source_conversion",
        Rule::Bool,
    ),
];
const STONECUTTER_RECIPES: &[(&str, &str)] = &[
    (
        "minecraft:chiseled_stone_bricks_stone_from_stonecutting",
        "minecraft:chiseled_stone_bricks_from_stone_stonecutting",
    ),
    (
        "minecraft:end_stone_brick_slab_from_end_stone_brick_stonecutting",
        "minecraft:end_stone_brick_slab_from_end_stone_bricks_stonecutting",
    ),
    (
        "minecraft:end_stone_brick_stairs_from_end_stone_brick_stonecutting",
        "minecraft:end_stone_brick_stairs_from_end_stone_bricks_stonecutting",
    ),
    (
        "minecraft:end_stone_brick_wall_from_end_stone_brick_stonecutting",
        "minecraft:end_stone_brick_wall_from_end_stone_bricks_stonecutting",
    ),
    (
        "minecraft:mossy_stone_brick_slab_from_mossy_stone_brick_stonecutting",
        "minecraft:mossy_stone_brick_slab_from_mossy_stone_bricks_stonecutting",
    ),
    (
        "minecraft:mossy_stone_brick_stairs_from_mossy_stone_brick_stonecutting",
        "minecraft:mossy_stone_brick_stairs_from_mossy_stone_bricks_stonecutting",
    ),
    (
        "minecraft:mossy_stone_brick_wall_from_mossy_stone_brick_stonecutting",
        "minecraft:mossy_stone_brick_wall_from_mossy_stone_bricks_stonecutting",
    ),
    (
        "minecraft:prismarine_brick_slab_from_prismarine_stonecutting",
        "minecraft:prismarine_brick_slab_from_prismarine_bricks_stonecutting",
    ),
    (
        "minecraft:prismarine_brick_stairs_from_prismarine_stonecutting",
        "minecraft:prismarine_brick_stairs_from_prismarine_bricks_stonecutting",
    ),
    (
        "minecraft:quartz_slab_from_stonecutting",
        "minecraft:quartz_slab_from_quartz_block_stonecutting",
    ),
    (
        "minecraft:stone_brick_walls_from_stone_stonecutting",
        "minecraft:stone_brick_wall_from_stone_stonecutting",
    ),
];
const STONECUTTER_ADVANCEMENTS: &[(&str, &str)] = &[
    (
        "minecraft:recipes/building_blocks/chiseled_stone_bricks_stone_from_stonecutting",
        "minecraft:recipes/building_blocks/chiseled_stone_bricks_from_stone_stonecutting",
    ),
    (
        "minecraft:recipes/building_blocks/end_stone_brick_slab_from_end_stone_brick_stonecutting",
        "minecraft:recipes/building_blocks/end_stone_brick_slab_from_end_stone_bricks_stonecutting",
    ),
    (
        "minecraft:recipes/building_blocks/end_stone_brick_stairs_from_end_stone_brick_stonecutting",
        "minecraft:recipes/building_blocks/end_stone_brick_stairs_from_end_stone_bricks_stonecutting",
    ),
    (
        "minecraft:recipes/decorations/end_stone_brick_wall_from_end_stone_brick_stonecutting",
        "minecraft:recipes/decorations/end_stone_brick_wall_from_end_stone_bricks_stonecutting",
    ),
    (
        "minecraft:recipes/building_blocks/mossy_stone_brick_slab_from_mossy_stone_brick_stonecutting",
        "minecraft:recipes/building_blocks/mossy_stone_brick_slab_from_mossy_stone_bricks_stonecutting",
    ),
    (
        "minecraft:recipes/building_blocks/mossy_stone_brick_stairs_from_mossy_stone_brick_stonecutting",
        "minecraft:recipes/building_blocks/mossy_stone_brick_stairs_from_mossy_stone_bricks_stonecutting",
    ),
    (
        "minecraft:recipes/building_blocks/mossy_stone_brick_wall_from_mossy_stone_brick_stonecutting",
        "minecraft:recipes/decorations/mossy_stone_brick_wall_from_mossy_stone_bricks_stonecutting",
    ),
    (
        "minecraft:recipes/building_blocks/prismarine_brick_slab_from_prismarine_stonecutting",
        "minecraft:recipes/building_blocks/prismarine_brick_slab_from_prismarine_bricks_stonecutting",
    ),
    (
        "minecraft:recipes/building_blocks/prismarine_brick_stairs_from_prismarine_stonecutting",
        "minecraft:recipes/building_blocks/prismarine_brick_stairs_from_prismarine_bricks_stonecutting",
    ),
    (
        "minecraft:recipes/building_blocks/quartz_slab_from_stonecutting",
        "minecraft:recipes/building_blocks/quartz_slab_from_quartz_block_stonecutting",
    ),
    (
        "minecraft:recipes/decorations/stone_brick_walls_from_stone_stonecutting",
        "minecraft:recipes/decorations/stone_brick_wall_from_stone_stonecutting",
    ),
];
const SCOREBOARD_SLOTS: [&str; 19] = [
    "list",
    "sidebar",
    "below_name",
    "sidebar.team.black",
    "sidebar.team.dark_blue",
    "sidebar.team.dark_green",
    "sidebar.team.dark_aqua",
    "sidebar.team.dark_red",
    "sidebar.team.dark_purple",
    "sidebar.team.gold",
    "sidebar.team.gray",
    "sidebar.team.dark_gray",
    "sidebar.team.blue",
    "sidebar.team.green",
    "sidebar.team.aqua",
    "sidebar.team.red",
    "sidebar.team.light_purple",
    "sidebar.team.yellow",
    "sidebar.team.white",
];
const RAID_RENAMES: [(&str, &str); 11] = [
    ("Id", "id"),
    ("Started", "started"),
    ("Active", "active"),
    ("TicksActive", "ticks_active"),
    ("BadOmenLevel", "raid_omen_level"),
    ("GroupsSpawned", "groups_spawned"),
    ("PreRaidTicks", "cooldown_ticks"),
    ("PostRaidTicks", "post_raid_ticks"),
    ("TotalHealth", "total_health"),
    ("NumGroups", "group_count"),
    ("HeroesOfTheVillage", "heroes_of_the_village"),
];
/// `(version, feature flags it retired)`.
const RETIRED_FEATURES: [(i32, &str); 5] = [
    (3440, "minecraft:update_1_20"),
    (3939, "minecraft:update_1_21"),
    (4067, "minecraft:bundle"),
    (4180, "minecraft:winter_drop"),
    (4424, "minecraft:locator_bar"),
];
const CAT_TEXTURES: [(&str, &str); 11] = [
    ("textures/entity/cat/tabby.png", "minecraft:tabby"),
    ("textures/entity/cat/black.png", "minecraft:black"),
    ("textures/entity/cat/red.png", "minecraft:red"),
    ("textures/entity/cat/siamese.png", "minecraft:siamese"),
    (
        "textures/entity/cat/british_shorthair.png",
        "minecraft:british",
    ),
    ("textures/entity/cat/calico.png", "minecraft:calico"),
    ("textures/entity/cat/persian.png", "minecraft:persian"),
    ("textures/entity/cat/ragdoll.png", "minecraft:ragdoll"),
    ("textures/entity/cat/white.png", "minecraft:white"),
    ("textures/entity/cat/jellie.png", "minecraft:jellie"),
    ("textures/entity/cat/all_black.png", "minecraft:all_black"),
];
const CAT_CATALOGUE: &str = "minecraft:husbandry/complete_catalogue";

fn fixer_for(root: &Compound) -> Result<Option<Fixer>, DataFixError> {
    super::fixer_for(root)
}

/// A level's `Data` compound saved before the split, brought to the form
/// the split reads (and the player inside it to the split version).
pub fn prepare(data: &mut Compound, fixer: Fixer) -> Result<(), DataFixError> {
    super::pre2860::level(data, fixer)?;
    if fixer.due(3440, 0)
        && let Some(dimensions) = data
            .get_compound_mut("WorldGenSettings")
            .and_then(|settings| settings.get_compound_mut("dimensions"))
    {
        for (_, dimension) in dimensions.iter_mut() {
            if let Some(source) = dimension
                .as_compound_mut()
                .and_then(|d| d.get_compound_mut("generator"))
                .and_then(|g| g.get_compound_mut("biome_source"))
                && source.get_str("preset") == Some("minecraft:overworld_update_1_20")
            {
                source.insert("preset", "minecraft:overworld");
            }
        }
    }
    for (version, feature) in RETIRED_FEATURES {
        if fixer.due(version, 0) {
            retire_feature(data, feature);
        }
    }
    if fixer.due(3459, 0) && !data.contains_key("DragonFight") {
        let mut fight = data
            .get_compound("DimensionData")
            .and_then(|d| d.get_compound("1"))
            .and_then(|d| d.get_compound("DragonFight"))
            .cloned()
            .unwrap_or_default();
        if let Some(exit) = fight.remove("ExitPortalLocation") {
            fight.insert("ExitPortalLocation", fix_block_pos(exit));
        }
        data.insert("DragonFight", fight);
    }
    if fixer.due(4290, 0)
        && let Some(events) = data.get_compound_mut("CustomBossEvents")
    {
        boss_event_names(events, fixer);
    }
    if fixer.due(4544, 0) {
        legacy_world_border(data);
    }
    if fixer.due(4548, 0) {
        let x = get_number(data, "SpawnX").map_or(0, |v| v as i32);
        let y = get_number(data, "SpawnY").map_or(0, |v| v as i32);
        let z = get_number(data, "SpawnZ").map_or(0, |v| v as i32);
        let angle = get_number(data, "SpawnAngle").unwrap_or(0.0) as f32;
        for key in ["SpawnX", "SpawnY", "SpawnZ", "SpawnAngle"] {
            data.remove(key);
        }
        data.insert(
            "spawn",
            Compound::new()
                .with("dimension", "minecraft:overworld")
                .with("pos", vec![x, y, z])
                .with("yaw", angle)
                .with("pitch", 0.0f32),
        );
    }
    if fixer.due(4658, 0)
        && let Some(Tag::Compound(rules)) = data.remove("GameRules")
    {
        data.insert("game_rules", game_rules(rules));
    }
    if fixer.due(4766, 0) {
        let time = get_number(data, "Time").map_or(0, |v| v as i64);
        let day_time = data
            .remove("DayTime")
            .as_ref()
            .and_then(super::number)
            .map_or(time, |v| v as i64);
        data.insert(
            "world_clocks",
            Compound::new().with(
                "minecraft:overworld",
                Compound::new()
                    .with("total_ticks", day_time)
                    .with("paused", false),
            ),
        );
    }
    if fixer.due(4771, 0) {
        let difficulty = match get_number(data, "Difficulty").map_or(2, |v| v as i32) {
            0 => "peaceful",
            1 => "easy",
            3 => "hard",
            _ => "normal",
        };
        let settings = Compound::new()
            .with("difficulty", difficulty)
            .with("hardcore", get_bool(data, "hardcore").unwrap_or(false))
            .with(
                "locked",
                get_bool(data, "DifficultyLocked").unwrap_or(false),
            );
        for key in ["Difficulty", "hardcore", "DifficultyLocked"] {
            data.remove(key);
        }
        data.insert("difficulty_settings", settings);
        prepare_split(data);
    }
    if let Some(player) = data.get_compound_mut("Player") {
        entity::upgrade_player(player, fixer);
        player.remove("DataVersion");
    }
    Ok(())
}

fn retire_feature(data: &mut Compound, feature: &str) {
    let Some(enabled) = data.get_list_mut("enabled_features") else {
        return;
    };
    let before = enabled.len();
    enabled.retain(|f| f.as_str() != Some(feature));
    if enabled.len() == before {
        return;
    }
    let mut removed = data
        .get_list("removed_features")
        .map(<[Tag]>::to_vec)
        .unwrap_or_default();
    removed.push(Tag::from(feature));
    data.insert("removed_features", removed);
}

fn boss_event_names(events: &mut Compound, fixer: Fixer) {
    for (_, event) in events.iter_mut() {
        if let Some(event) = event.as_compound_mut() {
            text::upgrade_field(event, "Name", fixer);
        }
    }
}

fn legacy_world_border(data: &mut Compound) {
    let double =
        |data: &Compound, key: &str, default: f64| get_number(data, key).unwrap_or(default);
    let border = Compound::new()
        .with("center_x", double(data, "BorderCenterX", 0.0))
        .with("center_z", double(data, "BorderCenterZ", 0.0))
        .with("size", double(data, "BorderSize", 59_999_968.0))
        .with(
            "lerp_time",
            get_number(data, "BorderSizeLerpTime").map_or(0, |v| v as i64),
        )
        .with("lerp_target", double(data, "BorderSizeLerpTarget", 0.0))
        .with("safe_zone", double(data, "BorderSafeZone", 5.0))
        .with(
            "damage_per_block",
            double(data, "BorderDamagePerBlock", 0.2),
        )
        .with(
            "warning_blocks",
            get_number(data, "BorderWarningBlocks").map_or(5, |v| v as i32),
        )
        .with(
            "warning_time",
            get_number(data, "BorderWarningTime").map_or(15, |v| v as i32),
        );
    for key in [
        "BorderCenterX",
        "BorderCenterZ",
        "BorderSize",
        "BorderSizeLerpTime",
        "BorderSizeLerpTarget",
        "BorderSafeZone",
        "BorderDamagePerBlock",
        "BorderWarningBlocks",
        "BorderWarningTime",
    ] {
        data.remove(key);
    }
    data.insert("world_border", border);
}

fn game_rules(mut rules: Compound) -> Compound {
    let flag = |rules: &Compound, key: &str, default: bool| {
        rules
            .get_str(key)
            .map_or(default, |v| v.eq_ignore_ascii_case("true"))
    };
    let fire_tick = flag(&rules, "doFireTick", true);
    let away = flag(&rules, "allowFireTicksAwayFromPlayer", false);
    let radius = match (fire_tick, away) {
        (false, _) => 0,
        (true, false) => 128,
        (true, true) => -1,
    };
    for key in [
        "spawnChunkRadius",
        "entitiesWithPassengersCanUsePortals",
        "gameLoopFunction",
        "doFireTick",
        "allowFireTicksAwayFromPlayer",
    ] {
        rules.remove(key);
    }
    if radius != 128 {
        rules.insert("minecraft:fire_spread_radius_around_player", radius);
    }
    for (old, new, rule) in RULE_RENAMES {
        let Some(value) = rules.remove(old) else {
            continue;
        };
        let text = value.as_str().unwrap_or("").to_owned();
        let converted = match rule {
            Rule::Bool => Tag::from(text.eq_ignore_ascii_case("true")),
            Rule::Inverted => Tag::from(!text.eq_ignore_ascii_case("true")),
            Rule::Int(min, max) => match text.parse::<i32>() {
                Ok(v) => Tag::Int(v.clamp(*min, *max)),
                Err(_) => value,
            },
        };
        rules.insert(*new, converted);
    }
    rules
}

fn prepare_split(data: &mut Compound) {
    rename_with(data, "DragonFight", "dragon_fight", |fight| {
        let Tag::Compound(mut fight) = fight else {
            return Some(fight);
        };
        for (from, to) in [
            ("NeedsStateScanning", "needs_state_scanning"),
            ("DragonKilled", "dragon_killed"),
            ("PreviouslyKilled", "previously_killed"),
            ("Dragon", "dragon_uuid"),
            ("ExitPortalLocation", "exit_portal_location"),
            ("Gateways", "gateways"),
        ] {
            rename(&mut fight, from, to);
        }
        if fight
            .remove("IsRespawning")
            .as_ref()
            .and_then(super::number)
            .unwrap_or(0.0)
            != 0.0
        {
            fight.insert("respawn_stage", "start");
            fight.insert("respawn_time", 0);
        }
        Some(Tag::Compound(fight))
    });
    let trader = Compound::new()
        .with(
            "spawn_delay",
            get_number(data, "WanderingTraderSpawnDelay").map_or(24000, |v| v as i32),
        )
        .with(
            "spawn_chance",
            get_number(data, "WanderingTraderSpawnChance").map_or(25, |v| v as i32),
        );
    for key in [
        "WanderingTraderSpawnDelay",
        "WanderingTraderSpawnChance",
        "WanderingTraderId",
    ] {
        data.remove(key);
    }
    data.insert("wandering_trader_migration_data", trader);
    let int = |data: &Compound, key: &str| get_number(data, key).map_or(0, |v| v as i32);
    let weather = Compound::new()
        .with("clear_weather_time", int(data, "clearWeatherTime"))
        .with("rain_time", int(data, "rainTime"))
        .with("thunder_time", int(data, "thunderTime"))
        .with("raining", get_bool(data, "raining").unwrap_or(false))
        .with("thundering", get_bool(data, "thundering").unwrap_or(false));
    for key in [
        "clearWeatherTime",
        "rainTime",
        "thunderTime",
        "raining",
        "thundering",
    ] {
        data.remove(key);
    }
    data.insert("weather_data", weather);
    let events: Vec<Tag> = match data.remove("ScheduledEvents") {
        Some(Tag::List(events)) => events
            .into_iter()
            .map(|event| match event {
                Tag::Compound(mut event) => {
                    rename(&mut event, "Name", "id");
                    rename(&mut event, "TriggerTime", "trigger_time");
                    rename_with(
                        &mut event,
                        "Callback",
                        "callback",
                        |callback| match callback {
                            Tag::Compound(mut callback) => {
                                rename(&mut callback, "Type", "type");
                                rename(&mut callback, "Name", "id");
                                Some(Tag::Compound(callback))
                            }
                            other => Some(other),
                        },
                    );
                    Tag::Compound(event)
                }
                other => other,
            })
            .collect(),
        _ => Vec::new(),
    };
    data.insert("scheduled_events", Compound::new().with("events", events));
    rename_with(
        data,
        "WorldGenSettings",
        "world_gen_settings",
        |settings| match settings {
            Tag::Compound(mut settings) => {
                rename(&mut settings, "generate_features", "generate_structures");
                Some(Tag::Compound(settings))
            }
            other => Some(other),
        },
    );
}

/// `level.dat`'s root. Data older than the split is brought to the split
/// form in place; a backend that keeps its files elsewhere has no split to
/// make.
pub fn level_dat(root: &mut Compound) -> Result<(), DataFixError> {
    let Some(data) = root.get_compound_mut("Data") else {
        return Ok(());
    };
    let Some(fixer) = fixer_for(data)? else {
        return Ok(());
    };
    let mut prepared = data.clone();
    prepare(&mut prepared, fixer)?;
    *data = prepared;
    data.insert("DataVersion", DATA_VERSION);
    Ok(())
}

/// A saved data file's root (`{data, DataVersion}`), by resource id.
pub fn saved_data(id: &str, root: &mut Compound) -> Result<(), DataFixError> {
    if id == "minecraft:maps/last_id" && !root.contains_key("data") && root.contains_key("map") {
        let version = root.remove("DataVersion");
        let data = std::mem::take(root);
        root.insert("data", data);
        root.insert("DataVersion", version.unwrap_or(Tag::Int(DATA_VERSION)));
    }
    let Some(fixer) = fixer_for(root)? else {
        return Ok(());
    };
    let id = super::namespaced(id);
    if let Some(data) = root.get_compound_mut("data") {
        fix_saved_data(&id, data, fixer);
    }
    root.insert("DataVersion", DATA_VERSION);
    Ok(())
}

fn fix_saved_data(id: &str, data: &mut Compound, fixer: Fixer) {
    match id {
        "minecraft:chunk_tickets" | "minecraft:chunks" => {
            if fixer.due(4297, 0)
                && let Some(Tag::LongArray(forced)) = data.remove("Forced")
            {
                let tickets: Vec<Compound> = forced
                    .into_iter()
                    .map(|pos| {
                        Compound::new()
                            .with("type", "minecraft:forced")
                            .with("level", 31)
                            .with("ticks_left", 0i64)
                            .with("chunk_pos", pos)
                    })
                    .collect();
                data.insert("tickets", tickets);
            }
            if fixer.due(4309, 0) {
                for_each_compound(data, "tickets", |ticket| {
                    if let Some(key) = ticket.get("chunk_pos").and_then(Tag::as_long) {
                        let x = key as i32;
                        let z = (key >> 32) as i32;
                        ticket.insert("chunk_pos", vec![x, z]);
                    }
                });
            }
        }
        "minecraft:raids" | "minecraft:raids_end" => {
            super::pre2860::raids(data, fixer);
            if fixer.due(4309, 0) {
                rename(data, "Raids", "raids");
                for_each_compound(data, "raids", |raid| {
                    inline_block_pos(raid, "CX", "CY", "CZ", "center");
                    for (from, to) in RAID_RENAMES {
                        rename(raid, from, to);
                    }
                });
                rename(data, "Tick", "tick");
                rename(data, "NextAvailableID", "next_id");
            }
        }
        "minecraft:scoreboard" => {
            if fixer.due(3566, 0)
                && let Some(Tag::Compound(slots)) = data.remove("DisplaySlots")
            {
                let mut renamed = Compound::new();
                for (key, value) in slots {
                    let new = key
                        .strip_prefix("slot_")
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| SCOREBOARD_SLOTS.get(i))
                        .map_or(key.clone(), |name| (*name).to_owned());
                    renamed.insert(new, value);
                }
                data.insert("DisplaySlots", renamed);
            }
            for_each_compound(data, "Objectives", |objective| {
                text::upgrade_field(objective, "DisplayName", fixer);
                if fixer.due(2710, 0)
                    && let Some(criteria) = objective.get_compound_mut("CriteriaType")
                    && criteria.get_str("type").map(super::namespaced).as_deref()
                        == Some("minecraft:custom")
                    && matches!(
                        criteria.get_str("id"),
                        Some("minecraft:play_one_minute" | "play_one_minute")
                    )
                {
                    criteria.insert("id", "minecraft:play_time");
                }
            });
            for_each_compound(data, "Teams", |team| {
                for key in ["MemberNamePrefix", "MemberNameSuffix", "DisplayName"] {
                    text::upgrade_field(team, key, fixer);
                }
            });
            for_each_compound(data, "PlayerScores", |score| {
                text::upgrade_field(score, "display", fixer)
            });
        }
        "minecraft:random_sequences" => {
            if fixer.due(3565, 0) {
                let sequences = std::mem::take(data);
                data.insert("sequences", sequences);
            }
        }
        "minecraft:world_border" => {
            if fixer.due(4657, 0) {
                let time = get_number(data, "warning_time").map_or(15, |v| v as i32);
                data.insert("warning_time", time.saturating_mul(20));
            }
        }
        "minecraft:custom_boss_events" => {
            if fixer.due(4290, 0) {
                boss_event_names(data, fixer);
            }
        }
        _ if id.starts_with("minecraft:maps/") || id.starts_with("minecraft:map_") => {
            if fixer.due(2537, 0) {
                super::pre2860::legacy_dimension_id(data, "dimension");
            }
            if fixer.due(3807, 1) {
                for_each_compound(data, "banners", |banner| {
                    if let Some(pos) = banner.remove("Pos") {
                        banner.insert("Pos", fix_block_pos(pos));
                    }
                });
            }
            if fixer.due(3813, 0) {
                for_each_compound(data, "frames", |frame| {
                    rename_with(frame, "Pos", "pos", |p| Some(fix_block_pos(p)));
                    rename(frame, "Rotation", "rotation");
                    rename(frame, "EntityId", "entity_id");
                });
                for_each_compound(data, "banners", |banner| {
                    rename(banner, "Pos", "pos");
                    rename(banner, "Color", "color");
                    rename(banner, "Name", "name");
                });
            }
        }
        _ => {}
    }
}

/// A player's statistics file (no fixes since 1.18).
pub fn stats(root: &mut Compound) -> Result<(), DataFixError> {
    let Some(fixer) = fixer_for(root)? else {
        return Ok(());
    };
    if fixer.due(2710, 0)
        && let Some(stats) = root.get_compound_mut("stats")
        && let Some(custom) = stats.get_compound_mut("minecraft:custom")
    {
        rename_play_time(custom);
    }
    root.insert("DataVersion", DATA_VERSION);
    Ok(())
}

fn rename_play_time(custom: &mut Compound) {
    let key = ["minecraft:play_one_minute", "play_one_minute"]
        .into_iter()
        .find(|key| custom.contains_key(key));
    if let Some(key) = key
        && let Some(value) = custom.remove(key)
    {
        custom.insert("minecraft:play_time", value);
    }
}

/// A player's advancement progress: `{advancement id: progress}`.
pub fn advancements(root: &mut Compound) -> Result<(), DataFixError> {
    let Some(fixer) = fixer_for(root)? else {
        return Ok(());
    };
    if let Some(progress) = root.get_compound_mut(CAT_CATALOGUE)
        && let Some(criteria) = progress.get_compound_mut("criteria")
    {
        let renames = |criteria: &mut Compound, rename: &dyn Fn(&str) -> Option<String>| {
            let keys: Vec<String> = criteria.keys().map(str::to_owned).collect();
            for key in keys {
                if let Some(new) = rename(&key)
                    && let Some(value) = criteria.remove(&key)
                {
                    criteria.insert(new, value);
                }
            }
        };
        if fixer.due(3086, 0) {
            renames(criteria, &|key| {
                CAT_TEXTURES
                    .iter()
                    .find(|(old, _)| *old == key)
                    .map(|(_, new)| (*new).to_owned())
            });
        }
        if fixer.due(3097, 0) {
            renames(criteria, &|key| {
                (key == "minecraft:british").then(|| "minecraft:british_shorthair".to_owned())
            });
        }
    }
    let mut rename_advancement = |table: &[(&str, &str)]| {
        for (old, new) in table {
            if let Some(value) = root.remove(old) {
                root.insert(*new, value);
            }
        }
    };
    if fixer.due(2100, 0) {
        rename_advancement(&[(
            "minecraft:recipes/misc/sugar",
            "minecraft:recipes/misc/sugar_from_sugar_cane",
        )]);
    }
    if fixer.due(2503, 0) {
        rename_advancement(&[(
            "minecraft:recipes/misc/composter",
            "minecraft:recipes/decorations/composter",
        )]);
    }
    if fixer.due(2846, 0) {
        rename_advancement(&[
            (
                "minecraft:husbandry/play_jukebox_in_meadows",
                "minecraft:adventure/play_jukebox_in_meadows",
            ),
            (
                "minecraft:adventure/caves_and_cliff",
                "minecraft:adventure/fall_from_world_height",
            ),
            (
                "minecraft:adventure/ride_strider_in_overworld_lava",
                "minecraft:nether/ride_strider_in_overworld_lava",
            ),
        ]);
    }
    if fixer.due(4311, 0) {
        rename_advancement(&[(
            "minecraft:nether/use_lodestone",
            "minecraft:adventure/use_lodestone",
        )]);
    }
    if fixer.due(4769, 0) {
        rename_advancement(STONECUTTER_ADVANCEMENTS);
    }
    root.insert("DataVersion", DATA_VERSION);
    Ok(())
}

pub(crate) fn renamed_stonecutter_recipe(name: &str) -> Option<String> {
    let full = super::namespaced(name);
    STONECUTTER_RECIPES
        .iter()
        .find(|(old, _)| *old == full)
        .map(|(_, new)| (*new).to_owned())
}
