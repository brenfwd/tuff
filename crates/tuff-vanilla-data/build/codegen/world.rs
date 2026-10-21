//! Per-dimension gameplay rules from the dimension types' environment
//! attributes (bed rules, water evaporation, sky light), the day timelines'
//! sky light tracks, and the rules `build/WorldProperties.java` reads from
//! game code: attribute defaults, weather's pull on the sky light, and the
//! time sleeping skips to. Validated strictly.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::tags::resolved_registry_tags;
use crate::input::Input;
use crate::names::str_literal;

const BED_RULE: &str = "minecraft:gameplay/bed_rule";
const STRAW_BED_RULE: &str = "minecraft:gameplay/straw_bed_rule";
const WATER_EVAPORATES: &str = "minecraft:gameplay/water_evaporates";
const SKY_LIGHT_LEVEL: &str = "minecraft:gameplay/sky_light_level";
const MONSTERS_BURN: &str = "minecraft:gameplay/monsters_burn";
const SURFACE_SLIME_SPAWN_CHANCE: &str = "minecraft:gameplay/surface_slime_spawn_chance";
const TURTLE_EGG_HATCH_CHANCE: &str = "minecraft:gameplay/turtle_egg_hatch_chance";
const PIGLINS_ZOMBIFY: &str = "minecraft:gameplay/piglins_zombify";
const VILLAGER_ACTIVITY: &str = "minecraft:gameplay/villager_activity";
const BABY_VILLAGER_ACTIVITY: &str = "minecraft:gameplay/baby_villager_activity";
const SUN_ANGLE: &str = "minecraft:visual/sun_angle";
const NETHER_PORTAL_SPAWNS_PIGLIN: &str = "minecraft:gameplay/nether_portal_spawns_piglin";
const EYEBLOSSOM_OPEN: &str = "minecraft:gameplay/eyeblossom_open";
const INCREASED_FIRE_BURNOUT: &str = "minecraft:gameplay/increased_fire_burnout";
const CREAKING_ACTIVE: &str = "minecraft:gameplay/creaking_active";
const BEES_STAY_IN_HIVE: &str = "minecraft:gameplay/bees_stay_in_hive";
const RESPAWN_ANCHOR_WORKS: &str = "minecraft:gameplay/respawn_anchor_works";
const CAN_START_RAID: &str = "minecraft:gameplay/can_start_raid";
const BOOL_OPS: [(&str, &str); 7] = [
    ("override", "Override"),
    ("and", "And"),
    ("nand", "Nand"),
    ("or", "Or"),
    ("nor", "Nor"),
    ("xor", "Xor"),
    ("xnor", "Xnor"),
];
const BED_RULE_FIELDS: [&str; 5] = [
    "can_sleep",
    "can_set_spawn",
    "destroy_on_use",
    "destroy_on_leave",
    "error_message",
];
const FLOAT_OPS: [(&str, &str); 7] = [
    ("alpha_blend", "AlphaBlend"),
    ("override", "Override"),
    ("add", "Add"),
    ("subtract", "Subtract"),
    ("multiply", "Multiply"),
    ("minimum", "Minimum"),
    ("maximum", "Maximum"),
];

pub fn generate(input: &Input) -> Result<String> {
    let mut defaults: BTreeMap<String, Value> = BTreeMap::new();
    let mut weather: BTreeMap<String, (f32, f32)> = BTreeMap::new();
    let mut wake_up = None;
    let mut explosion_particles = Vec::new();
    let mut dungeon_mobs = Vec::new();
    let mut dungeon_loot = None;
    let mut decoration_steps = Vec::new();
    let mut ruined_portals = Vec::new();
    let mut giant_ruined_portals = Vec::new();
    let mut blackstone = Vec::new();
    let mut nether_fossils = Vec::new();
    let mut mineshafts = Vec::new();
    let mut mineshaft_loot = None;
    let mut shape_check = Vec::new();
    let mut structure_loot = Vec::new();
    let mut structure_templates = Vec::new();
    let mut structure_markers = Vec::new();
    let mut igloo_parts = Vec::new();
    let blocks = super::registries::variants(&input.registries["minecraft:block"])?;
    let entities = super::registries::variants(&input.registries["minecraft:entity_type"])?;
    let particles = super::registries::variants(&input.registries["minecraft:particle_type"])?;
    for line in input.world_properties.lines() {
        let mut parts = line.splitn(3, ' ');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("default"), Some(attribute), Some(json)) => {
                let value: Value = serde_json::from_str(json)
                    .with_context(|| format!("world properties: bad default in {line:?}"))?;
                defaults.insert(attribute.to_string(), value);
            }
            (Some("weather"), Some(kind), Some(rest)) => {
                let values: Vec<&str> = rest.split(' ').collect();
                let [attribute, value, alpha] = values.as_slice() else {
                    bail!("world properties: bad weather line {line:?}");
                };
                ensure!(
                    *attribute == SKY_LIGHT_LEVEL,
                    "world properties: weather changes unmodeled {attribute}"
                );
                let (value, alpha): (f32, f32) = (value.parse()?, alpha.parse()?);
                ensure!(
                    (0.0..=1.0).contains(&alpha) && value.is_finite(),
                    "world properties: bad weather blend in {line:?}"
                );
                weather.insert(kind.to_string(), (value, alpha));
            }
            (Some("wake_up"), Some(marker), None) => wake_up = Some(marker.to_string()),
            (Some("explosion_particle"), Some(particle), Some(rest)) => {
                let values: Vec<&str> = rest.split(' ').collect();
                let [scaling, speed, weight] = values.as_slice() else {
                    bail!("world properties: bad explosion particle {line:?}");
                };
                let variant = particles
                    .get(particle)
                    .with_context(|| format!("world properties: unknown particle {particle}"))?;
                let (scaling, speed): (f32, f32) = (scaling.parse()?, speed.parse()?);
                let weight: u32 = weight.parse()?;
                explosion_particles.push(format!(
                    "(crate::registry::ParticleType::{variant}, {scaling:?}, {speed:?}, {weight})"
                ));
            }
            (Some("dungeon_mob"), Some(entity), None) => {
                ensure!(
                    entities.contains_key(entity),
                    "world properties: unknown entity {entity}"
                );
                dungeon_mobs.push(str_literal(entity));
            }
            (Some("dungeon_loot"), Some(table), None) => {
                let (namespace, path) = table
                    .split_once(':')
                    .with_context(|| format!("world properties: bad loot table {table}"))?;
                ensure!(
                    input
                        .data_json
                        .contains_key(&format!("{namespace}/loot_table/{path}.json")),
                    "world properties: unknown loot table {table}"
                );
                dungeon_loot = Some(table.to_string());
            }
            (Some("decoration_step"), Some(step), None) => {
                decoration_steps.push(str_literal(step));
            }
            (Some("mineshaft"), Some(kind), Some(rest)) => {
                let names: Vec<&str> = rest.split(' ').collect();
                let [log, planks, fence] = names.as_slice() else {
                    bail!("world properties: bad mineshaft line {line:?}");
                };
                let variant = |name: &str| {
                    blocks
                        .get(name)
                        .with_context(|| format!("world properties: unknown block {name}"))
                };
                mineshafts.push(format!(
                    "({}, crate::Block::{}, crate::Block::{}, crate::Block::{})",
                    str_literal(kind),
                    variant(log)?,
                    variant(planks)?,
                    variant(fence)?
                ));
            }
            (Some("mineshaft_loot"), Some(table), None) => {
                let (namespace, path) = table
                    .split_once(':')
                    .with_context(|| format!("world properties: bad loot table {table}"))?;
                ensure!(
                    input
                        .data_json
                        .contains_key(&format!("{namespace}/loot_table/{path}.json")),
                    "world properties: unknown loot table {table}"
                );
                mineshaft_loot = Some(table.to_string());
            }
            (Some("shape_check"), Some(block), None) => {
                let variant = blocks
                    .get(block)
                    .with_context(|| format!("world properties: unknown block {block}"))?;
                shape_check.push(format!("crate::Block::{variant}"));
            }
            (Some("nether_fossil"), Some(template), None) => {
                let (namespace, path) = template
                    .split_once(':')
                    .with_context(|| format!("world properties: bad template {template}"))?;
                ensure!(
                    input
                        .structure_templates
                        .contains_key(&format!("{namespace}/structure/{path}.nbt")),
                    "world properties: unknown structure template {template}"
                );
                nether_fossils.push(str_literal(template));
            }
            (Some("ruined_portal"), Some(kind), Some(template)) => {
                let (namespace, path) = template
                    .split_once(':')
                    .with_context(|| format!("world properties: bad template {template}"))?;
                ensure!(
                    input
                        .structure_templates
                        .contains_key(&format!("{namespace}/structure/{path}.nbt")),
                    "world properties: unknown structure template {template}"
                );
                match kind {
                    "normal" => ruined_portals.push(str_literal(template)),
                    "giant" => giant_ruined_portals.push(str_literal(template)),
                    _ => bail!("world properties: bad ruined portal kind in {line:?}"),
                }
            }
            (Some("blackstone_replace"), Some(from), Some(to)) => {
                let variant = |name: &str| {
                    blocks
                        .get(name)
                        .with_context(|| format!("world properties: unknown block {name}"))
                };
                blackstone.push(format!(
                    "(crate::Block::{}, crate::Block::{})",
                    variant(from)?,
                    variant(to)?
                ));
            }
            (Some("structure_loot"), Some(use_), Some(table)) => {
                let (namespace, path) = table
                    .split_once(':')
                    .with_context(|| format!("world properties: bad loot table {table}"))?;
                ensure!(
                    input
                        .data_json
                        .contains_key(&format!("{namespace}/loot_table/{path}.json")),
                    "world properties: unknown loot table {table}"
                );
                structure_loot.push(format!("({}, {})", str_literal(use_), str_literal(table)));
            }
            (Some("structure_template"), Some(use_), Some(template)) => {
                check_template(input, template)?;
                structure_templates.push(format!(
                    "({}, {})",
                    str_literal(use_),
                    str_literal(template)
                ));
            }
            (Some("structure_marker"), Some(use_), Some(rest)) => {
                let Some((marker, table)) = rest.split_once(' ') else {
                    bail!("world properties: bad structure marker {line:?}");
                };
                check_loot_table(input, table)?;
                structure_markers.push(format!(
                    "({}, {}, {})",
                    str_literal(use_),
                    str_literal(marker),
                    str_literal(table)
                ));
            }
            (Some("igloo"), Some(template), Some(rest)) => {
                check_template(input, template)?;
                let numbers: Vec<i32> =
                    rest.split(' ')
                        .map(str::parse)
                        .collect::<Result<_, _>>()
                        .with_context(|| format!("world properties: bad igloo line {line:?}"))?;
                let [px, py, pz, ox, oy, oz] = numbers.as_slice() else {
                    bail!("world properties: bad igloo line {line:?}");
                };
                igloo_parts.push(format!(
                    "({}, [{px}, {py}, {pz}], [{ox}, {oy}, {oz}])",
                    str_literal(template)
                ));
            }
            _ => bail!("world properties: bad line {line:?}"),
        }
    }
    let wake_up = wake_up.context("world properties: no wake-up marker")?;
    let default = |key: &str| {
        defaults
            .get(key)
            .with_context(|| format!("world properties: no default for {key}"))
    };
    let timeline_tags = resolved_registry_tags(input, "minecraft:timeline")?;

    let mut out = String::new();
    let default_environment = Environment {
        bed_rule: bed_rule(default(BED_RULE)?)?,
        straw_bed_rule: bed_rule(default(STRAW_BED_RULE)?)?,
        water_evaporates: default(WATER_EVAPORATES)?
            .as_bool()
            .context("bad water_evaporates default")?,
        sky_light_level: sky_light(default(SKY_LIGHT_LEVEL)?)?,
        has_fixed_time: false,
        sky_light_tracks: "&[]".to_string(),
        monsters_burn: default(MONSTERS_BURN)?
            .as_bool()
            .context("bad monsters_burn default")?,
        monsters_burn_tracks: "&[]".to_string(),
        surface_slime_spawn_chance: default(SURFACE_SLIME_SPAWN_CHANCE)?
            .as_f64()
            .context("bad surface_slime_spawn_chance default")?
            as f32,
        surface_slime_tracks: "&[]".to_string(),
        turtle_egg_hatch_chance: default(TURTLE_EGG_HATCH_CHANCE)?
            .as_f64()
            .context("bad turtle_egg_hatch_chance default")?
            as f32,
        turtle_egg_tracks: "&[]".to_string(),
        piglins_zombify: default(PIGLINS_ZOMBIFY)?
            .as_bool()
            .context("bad piglins_zombify default")?,
        respawn_anchor_works: default(RESPAWN_ANCHOR_WORKS)?
            .as_bool()
            .context("bad respawn_anchor_works default")?,
        can_start_raid: default(CAN_START_RAID)?
            .as_bool()
            .context("bad can_start_raid default")?,
        villager_activity: activity(input, default(VILLAGER_ACTIVITY)?)?,
        villager_activity_tracks: "&[]".to_string(),
        baby_villager_activity: activity(input, default(BABY_VILLAGER_ACTIVITY)?)?,
        baby_villager_activity_tracks: "&[]".to_string(),
        sun_angle: default(SUN_ANGLE)?
            .as_f64()
            .context("bad sun_angle default")? as f32,
        sun_angle_tracks: "&[]".to_string(),
        nether_portal_spawns_piglin: false,
        eyeblossom_open_tracks: "&[]".to_string(),
        increased_fire_burnout: false,
        creaking_active: false,
        creaking_active_tracks: "&[]".to_string(),
        bees_stay_in_hive: false,
        bees_stay_in_hive_tracks: "&[]".to_string(),
        timelines: Vec::new(),
    };
    writeln!(
        out,
        "pub static DEFAULT_ENVIRONMENT: Environment = {};",
        default_environment.literal()
    )?;

    let mut dimensions = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("dimension_type/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let context = || format!("data/{path}");
        let attributes = json.get("attributes").and_then(Value::as_object);
        let attribute = |key: &str| attributes.and_then(|a| a.get(key));
        let mut environment = Environment {
            bed_rule: match attribute(BED_RULE) {
                Some(value) => bed_rule(value).with_context(context)?,
                None => default_environment.bed_rule.clone(),
            },
            straw_bed_rule: match attribute(STRAW_BED_RULE) {
                Some(value) => bed_rule(value).with_context(context)?,
                None => default_environment.straw_bed_rule.clone(),
            },
            water_evaporates: match attribute(WATER_EVAPORATES) {
                Some(value) => value.as_bool().with_context(context)?,
                None => default_environment.water_evaporates,
            },
            sky_light_level: match attribute(SKY_LIGHT_LEVEL) {
                Some(value) => sky_light(value).with_context(context)?,
                None => default_environment.sky_light_level,
            },
            has_fixed_time: json
                .get("has_fixed_time")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            sky_light_tracks: String::new(),
            monsters_burn: match attribute(MONSTERS_BURN) {
                Some(value) => value.as_bool().with_context(context)?,
                None => default_environment.monsters_burn,
            },
            monsters_burn_tracks: String::new(),
            surface_slime_spawn_chance: match attribute(SURFACE_SLIME_SPAWN_CHANCE) {
                Some(value) => value.as_f64().with_context(context)? as f32,
                None => default_environment.surface_slime_spawn_chance,
            },
            surface_slime_tracks: String::new(),
            turtle_egg_hatch_chance: match attribute(TURTLE_EGG_HATCH_CHANCE) {
                Some(value) => value.as_f64().with_context(context)? as f32,
                None => default_environment.turtle_egg_hatch_chance,
            },
            turtle_egg_tracks: String::new(),
            piglins_zombify: match attribute(PIGLINS_ZOMBIFY) {
                Some(value) => value.as_bool().with_context(context)?,
                None => default_environment.piglins_zombify,
            },
            respawn_anchor_works: match attribute(RESPAWN_ANCHOR_WORKS) {
                Some(value) => value.as_bool().with_context(context)?,
                None => default_environment.respawn_anchor_works,
            },
            can_start_raid: match attribute(CAN_START_RAID) {
                Some(value) => value.as_bool().with_context(context)?,
                None => default_environment.can_start_raid,
            },
            villager_activity: match attribute(VILLAGER_ACTIVITY) {
                Some(value) => activity(input, value).with_context(context)?,
                None => default_environment.villager_activity.clone(),
            },
            villager_activity_tracks: String::new(),
            baby_villager_activity: match attribute(BABY_VILLAGER_ACTIVITY) {
                Some(value) => activity(input, value).with_context(context)?,
                None => default_environment.baby_villager_activity.clone(),
            },
            baby_villager_activity_tracks: String::new(),
            sun_angle: match attribute(SUN_ANGLE) {
                Some(value) => value.as_f64().with_context(context)? as f32,
                None => default_environment.sun_angle,
            },
            sun_angle_tracks: String::new(),
            nether_portal_spawns_piglin: match attribute(NETHER_PORTAL_SPAWNS_PIGLIN) {
                Some(value) => value.as_bool().with_context(context)?,
                None => default_environment.nether_portal_spawns_piglin,
            },
            eyeblossom_open_tracks: String::new(),
            increased_fire_burnout: match attribute(INCREASED_FIRE_BURNOUT) {
                Some(value) => value.as_bool().with_context(context)?,
                None => default_environment.increased_fire_burnout,
            },
            creaking_active: match attribute(CREAKING_ACTIVE) {
                Some(value) => value.as_bool().with_context(context)?,
                None => default_environment.creaking_active,
            },
            creaking_active_tracks: String::new(),
            bees_stay_in_hive: match attribute(BEES_STAY_IN_HIVE) {
                Some(value) => value.as_bool().with_context(context)?,
                None => default_environment.bees_stay_in_hive,
            },
            bees_stay_in_hive_tracks: String::new(),
            timelines: Vec::new(),
        };
        let timelines: Vec<String> = match json.get("timelines") {
            None => Vec::new(),
            Some(Value::String(tag)) if tag.starts_with('#') => timeline_tags
                .get(&tag[1..])
                .with_context(|| format!("{}: unknown timeline tag {tag}", context()))?
                .clone(),
            Some(Value::String(name)) => vec![name.clone()],
            Some(Value::Array(names)) => names
                .iter()
                .map(|n| n.as_str().map(str::to_string).context("bad timeline"))
                .collect::<Result<_>>()?,
            Some(_) => bail!("{}: bad timelines", context()),
        };
        let mut tracks = Vec::new();
        let mut burn_tracks = Vec::new();
        let mut slime_tracks = Vec::new();
        let mut turtle_egg_tracks = Vec::new();
        let mut villager_tracks = Vec::new();
        let mut baby_tracks = Vec::new();
        let mut sun_tracks = Vec::new();
        let mut eyeblossom_tracks = Vec::new();
        let mut creaking_tracks = Vec::new();
        let mut bee_tracks = Vec::new();
        for timeline in &timelines {
            let (namespace, name) = timeline.split_once(':').unwrap_or(("minecraft", timeline));
            let file = input
                .data_json
                .get(&format!("{namespace}/timeline/{name}.json"))
                .with_context(|| format!("unknown timeline {timeline}"))?;
            let clock = || {
                file.get("clock")
                    .and_then(Value::as_str)
                    .with_context(|| format!("{timeline} has no clock"))
            };
            let period = file.get("period_ticks").and_then(Value::as_u64);
            if let Some(track) = file.get("tracks").and_then(|t| t.get(MONSTERS_BURN)) {
                burn_tracks
                    .push(flag_track(clock()?, period, track).with_context(|| timeline.clone())?);
            }
            if let Some(track) = file
                .get("tracks")
                .and_then(|t| t.get(SURFACE_SLIME_SPAWN_CHANCE))
            {
                slime_tracks.push(
                    float_track(clock()?, period, track, &["constant"])
                        .with_context(|| timeline.clone())?,
                );
            }
            if let Some(track) = file
                .get("tracks")
                .and_then(|t| t.get(TURTLE_EGG_HATCH_CHANCE))
            {
                turtle_egg_tracks.push(
                    float_track(clock()?, period, track, &["constant"])
                        .with_context(|| timeline.clone())?,
                );
            }
            if let Some(track) = file.get("tracks").and_then(|t| t.get(EYEBLOSSOM_OPEN)) {
                eyeblossom_tracks
                    .push(flag_track(clock()?, period, track).with_context(|| timeline.clone())?);
            }
            if let Some(track) = file.get("tracks").and_then(|t| t.get(CREAKING_ACTIVE)) {
                creaking_tracks
                    .push(flag_track(clock()?, period, track).with_context(|| timeline.clone())?);
            }
            if let Some(track) = file.get("tracks").and_then(|t| t.get(BEES_STAY_IN_HIVE)) {
                bee_tracks
                    .push(flag_track(clock()?, period, track).with_context(|| timeline.clone())?);
            }
            if let Some(track) = file.get("tracks").and_then(|t| t.get(SUN_ANGLE)) {
                sun_tracks.push(
                    float_track(
                        clock()?,
                        period,
                        track,
                        &["linear", "cubic_bezier", "named"],
                    )
                    .with_context(|| timeline.clone())?,
                );
            }
            for (key, list) in [
                (VILLAGER_ACTIVITY, &mut villager_tracks),
                (BABY_VILLAGER_ACTIVITY, &mut baby_tracks),
            ] {
                if let Some(track) = file.get("tracks").and_then(|t| t.get(key)) {
                    list.push(
                        activity_track(input, clock()?, period, track)
                            .with_context(|| timeline.clone())?,
                    );
                }
            }
            let Some(track) = file.get("tracks").and_then(|t| t.get(SKY_LIGHT_LEVEL)) else {
                continue;
            };
            tracks.push(
                float_track(clock()?, period, track, &["linear"])
                    .with_context(|| timeline.clone())?,
            );
        }
        environment.sky_light_tracks = format!("&[{}]", tracks.join(", "));
        environment.monsters_burn_tracks = format!("&[{}]", burn_tracks.join(", "));
        environment.surface_slime_tracks = format!("&[{}]", slime_tracks.join(", "));
        environment.turtle_egg_tracks = format!("&[{}]", turtle_egg_tracks.join(", "));
        environment.villager_activity_tracks = format!("&[{}]", villager_tracks.join(", "));
        environment.baby_villager_activity_tracks = format!("&[{}]", baby_tracks.join(", "));
        environment.sun_angle_tracks = format!("&[{}]", sun_tracks.join(", "));
        environment.eyeblossom_open_tracks = format!("&[{}]", eyeblossom_tracks.join(", "));
        environment.creaking_active_tracks = format!("&[{}]", creaking_tracks.join(", "));
        environment.bees_stay_in_hive_tracks = format!("&[{}]", bee_tracks.join(", "));
        let mut all_timelines: Vec<String> = input
            .data_json
            .keys()
            .filter_map(|path| {
                let (namespace, rest) = path.split_once('/')?;
                let name = rest.strip_prefix("timeline/")?.strip_suffix(".json")?;
                Some(format!("{namespace}:{name}"))
            })
            .collect();
        all_timelines.sort();
        environment.timelines = timelines
            .iter()
            .map(|t| {
                let name = match t.split_once(':') {
                    Some(_) => t.clone(),
                    None => format!("minecraft:{t}"),
                };
                let index = all_timelines
                    .iter()
                    .position(|n| *n == name)
                    .with_context(|| format!("unknown timeline {name}"))?;
                Ok(format!("crate::timeline::ALL[{index}].1"))
            })
            .collect::<Result<_>>()?;
        dimensions.push((format!("{namespace}:{name}"), environment));
    }
    ensure!(!dimensions.is_empty(), "no dimension types");
    dimensions.sort_by(|(a, _), (b, _)| a.cmp(b));
    writeln!(out, "pub static ENVIRONMENTS: &[(&str, Environment)] = &[")?;
    for (name, environment) in &dimensions {
        writeln!(
            out,
            "    ({}, {}),",
            str_literal(name),
            environment.literal()
        )?;
    }
    writeln!(out, "];")?;
    let blend = |kind: &str| -> Result<String> {
        let (value, alpha) = weather
            .get(kind)
            .with_context(|| format!("world properties: no {kind} blend"))?;
        Ok(format!(
            "WeatherBlend {{ value: {value:?}, alpha: {alpha:?} }}"
        ))
    };
    writeln!(
        out,
        "pub const RAIN_SKY_LIGHT: WeatherBlend = {};",
        blend("rain")?
    )?;
    writeln!(
        out,
        "pub const THUNDER_SKY_LIGHT: WeatherBlend = {};",
        blend("thunder")?
    )?;
    writeln!(
        out,
        "pub const WAKE_UP_MARKER: &str = {};",
        str_literal(&wake_up)
    )?;
    ensure!(
        !explosion_particles.is_empty(),
        "world properties: no explosion particles"
    );
    writeln!(
        out,
        "/// `(particle, scaling, speed, weight)` of the particles blocks give off\n\
         /// in an explosion.\n\
         pub const EXPLOSION_BLOCK_PARTICLES: &[(crate::registry::ParticleType, f32, f32, u32)] = &[{}];",
        explosion_particles.join(", ")
    )?;
    ensure!(
        !dungeon_mobs.is_empty(),
        "world properties: no dungeon mobs"
    );
    writeln!(
        out,
        "/// The entity types a monster room spawner picks from uniformly.\n\
         pub const DUNGEON_MOBS: &[&str] = &[{}];",
        dungeon_mobs.join(", ")
    )?;
    let dungeon_loot = dungeon_loot.context("world properties: no dungeon loot table")?;
    writeln!(
        out,
        "/// The loot table of monster room chests.\n\
         pub const DUNGEON_LOOT_TABLE: &str = {};",
        str_literal(&dungeon_loot)
    )?;
    ensure!(
        decoration_steps.len() >= 2,
        "world properties: no decoration steps"
    );
    writeln!(
        out,
        "/// The decoration steps, in the order chunks are decorated.\n\
         pub const DECORATION_STEPS: &[&str] = &[{}];",
        decoration_steps.join(", ")
    )?;
    ensure!(
        !ruined_portals.is_empty() && !giant_ruined_portals.is_empty(),
        "world properties: no ruined portal templates"
    );
    writeln!(
        out,
        "/// The templates of ordinary ruined portals, in the order they are\n\
         /// picked from.\n\
         pub const RUINED_PORTALS: &[&str] = &[{}];\n\
         /// The templates of giant ruined portals.\n\
         pub const GIANT_RUINED_PORTALS: &[&str] = &[{}];",
        ruined_portals.join(", "),
        giant_ruined_portals.join(", ")
    )?;
    ensure!(
        !mineshafts.is_empty(),
        "world properties: no mineshaft types"
    );
    writeln!(
        out,
        "/// `(type, log, planks, fence)` of each mineshaft type, in the game's\n\
         /// type order (which pieces save as `MST`).\n\
         pub const MINESHAFT_TYPES: &[(&str, crate::Block, crate::Block, crate::Block)] = &[{}];",
        mineshafts.join(", ")
    )?;
    ensure!(
        !structure_loot.is_empty(),
        "world properties: no structure loot tables"
    );
    writeln!(
        out,
        "/// `(use, loot table)`: the loot tables hand-built structure pieces\n\
         /// fill containers from (`fortress`, `stronghold_library`, ...).\n\
         pub const STRUCTURE_LOOT_TABLES: &[(&str, &str)] = &[{}];",
        structure_loot.join(", ")
    )?;
    ensure!(
        !structure_templates.is_empty() && !structure_markers.is_empty() && !igloo_parts.is_empty(),
        "world properties: no structure templates, markers or igloo parts"
    );
    writeln!(
        out,
        "/// `(use, template)`: the templates template-built structures pick\n\
         /// from, per use (`shipwreck_ocean`, `igloo_top`, ...), in order.\n\
         pub const STRUCTURE_TEMPLATES: &[(&str, &str)] = &[{}];\n\
         /// `(use, marker, loot table)`: the loot a data marker gives the\n\
         /// container below it.\n\
         pub const STRUCTURE_MARKERS: &[(&str, &str, &str)] = &[{}];\n\
         /// `(template, rotation pivot, offset from the start)` of each igloo\n\
         /// part.\n\
         pub const IGLOO_PARTS: &[(&str, [i32; 3], [i32; 3])] = &[{}];",
        structure_templates.join(", "),
        structure_markers.join(", "),
        igloo_parts.join(", ")
    )?;
    let mineshaft_loot = mineshaft_loot.context("world properties: no mineshaft loot table")?;
    writeln!(
        out,
        "/// The loot table of mineshaft chest minecarts.\n\
         pub const MINESHAFT_LOOT_TABLE: &str = {};",
        str_literal(&mineshaft_loot)
    )?;
    ensure!(
        !shape_check.is_empty(),
        "world properties: no shape check blocks"
    );
    writeln!(
        out,
        "/// Blocks structure pieces mark for a shape update after placing them.\n\
         pub const STRUCTURE_SHAPE_CHECK_BLOCKS: &[crate::Block] = &[{}];",
        shape_check.join(", ")
    )?;
    ensure!(
        !nether_fossils.is_empty(),
        "world properties: no nether fossil templates"
    );
    writeln!(
        out,
        "/// The templates of nether fossils, in the order they are picked from.\n\
         pub const NETHER_FOSSILS: &[&str] = &[{}];",
        nether_fossils.join(", ")
    )?;
    ensure!(
        !blackstone.is_empty(),
        "world properties: no blackstone replacements"
    );
    writeln!(
        out,
        "/// The blocks nether ruined portals turn into blackstone variants,\n\
         /// sorted by the block's name.\n\
         pub const BLACKSTONE_REPLACEMENTS: &[(crate::Block, crate::Block)] = &[{}];",
        blackstone.join(", ")
    )?;
    Ok(out)
}

struct Environment {
    bed_rule: String,
    straw_bed_rule: String,
    water_evaporates: bool,
    sky_light_level: f32,
    has_fixed_time: bool,
    sky_light_tracks: String,
    monsters_burn: bool,
    monsters_burn_tracks: String,
    surface_slime_spawn_chance: f32,
    surface_slime_tracks: String,
    turtle_egg_hatch_chance: f32,
    turtle_egg_tracks: String,
    piglins_zombify: bool,
    respawn_anchor_works: bool,
    can_start_raid: bool,
    villager_activity: String,
    villager_activity_tracks: String,
    baby_villager_activity: String,
    baby_villager_activity_tracks: String,
    sun_angle: f32,
    sun_angle_tracks: String,
    nether_portal_spawns_piglin: bool,
    eyeblossom_open_tracks: String,
    increased_fire_burnout: bool,
    creaking_active: bool,
    creaking_active_tracks: String,
    bees_stay_in_hive: bool,
    bees_stay_in_hive_tracks: String,
    timelines: Vec<String>,
}

impl Environment {
    fn literal(&self) -> String {
        format!(
            "Environment {{ bed_rule: {}, straw_bed_rule: {}, water_evaporates: {}, \
             sky_light_level: {:?}, has_fixed_time: {}, sky_light_tracks: {}, \
             monsters_burn: {}, monsters_burn_tracks: {}, \
             surface_slime_spawn_chance: {:?}, surface_slime_tracks: {}, \
             turtle_egg_hatch_chance: {:?}, turtle_egg_tracks: {}, \
             piglins_zombify: {}, respawn_anchor_works: {}, can_start_raid: {}, \
             villager_activity: {}, villager_activity_tracks: {}, \
             baby_villager_activity: {}, baby_villager_activity_tracks: {}, \
             sun_angle: {:?}, sun_angle_tracks: {}, \
             nether_portal_spawns_piglin: {}, eyeblossom_open_tracks: {}, \
             increased_fire_burnout: {}, creaking_active: {}, creaking_active_tracks: {}, \
             bees_stay_in_hive: {}, bees_stay_in_hive_tracks: {}, natural_spawns: None, \
             timelines: &[{}] }}",
            self.bed_rule,
            self.straw_bed_rule,
            self.water_evaporates,
            self.sky_light_level,
            self.has_fixed_time,
            if self.sky_light_tracks.is_empty() {
                "&[]"
            } else {
                &self.sky_light_tracks
            },
            self.monsters_burn,
            if self.monsters_burn_tracks.is_empty() {
                "&[]"
            } else {
                &self.monsters_burn_tracks
            },
            self.surface_slime_spawn_chance,
            if self.surface_slime_tracks.is_empty() {
                "&[]"
            } else {
                &self.surface_slime_tracks
            },
            self.turtle_egg_hatch_chance,
            or_empty(&self.turtle_egg_tracks),
            self.piglins_zombify,
            self.respawn_anchor_works,
            self.can_start_raid,
            self.villager_activity,
            or_empty(&self.villager_activity_tracks),
            self.baby_villager_activity,
            or_empty(&self.baby_villager_activity_tracks),
            self.sun_angle,
            or_empty(&self.sun_angle_tracks),
            self.nether_portal_spawns_piglin,
            or_empty(&self.eyeblossom_open_tracks),
            self.increased_fire_burnout,
            self.creaking_active,
            or_empty(&self.creaking_active_tracks),
            self.bees_stay_in_hive,
            or_empty(&self.bees_stay_in_hive_tracks),
            self.timelines.join(", "),
        )
    }
}

fn or_empty(list: &str) -> &str {
    if list.is_empty() { "&[]" } else { list }
}

/// An activity attribute value as its `Activity` variant.
fn activity(input: &Input, value: &Value) -> Result<String> {
    let name = value.as_str().context("bad activity")?;
    let variants = super::registries::variants(
        input
            .registries
            .get("minecraft:activity")
            .context("no activity registry")?,
    )?;
    let variant = variants
        .get(name)
        .with_context(|| format!("unknown activity {name}"))?;
    Ok(format!("crate::registry::Activity::{variant}"))
}

/// A timeline track on an activity attribute: each keyframe's value holds
/// until the next.
fn activity_track(
    input: &Input,
    clock: &str,
    period: Option<u64>,
    track: &Value,
) -> Result<String> {
    let map = track.as_object().context("bad track")?;
    for key in map.keys() {
        ensure!(key == "keyframes", "unknown activity track field {key}");
    }
    let mut keyframes = Vec::new();
    let mut last = i64::MIN;
    for keyframe in map
        .get("keyframes")
        .and_then(Value::as_array)
        .context("no keyframes")?
    {
        let ticks = keyframe
            .get("ticks")
            .and_then(Value::as_i64)
            .context("bad keyframe ticks")?;
        ensure!(ticks > last, "keyframes out of order");
        last = ticks;
        let value = activity(input, keyframe.get("value").context("no value")?)?;
        keyframes.push(format!("({ticks}, {value})"));
    }
    ensure!(!keyframes.is_empty(), "empty activity track");
    Ok(format!(
        "ActivityTrack {{ clock: {}, period_ticks: {}, keyframes: &[{}] }}",
        str_literal(clock),
        period.map_or("None".to_string(), |p| format!("Some({p})")),
        keyframes.join(", ")
    ))
}

fn condition(value: &Value) -> Result<&'static str> {
    Ok(match value.as_str().context("bad bed rule condition")? {
        "always" => "BedCondition::Always",
        "when_dark" => "BedCondition::WhenDark",
        "never" => "BedCondition::Never",
        other => bail!("unknown bed rule condition {other}"),
    })
}

fn bed_rule(value: &Value) -> Result<String> {
    let map = value.as_object().context("bad bed rule")?;
    for key in map.keys() {
        ensure!(
            BED_RULE_FIELDS.contains(&key.as_str()),
            "unknown bed rule field {key}"
        );
    }
    let flag = |key: &str| -> Result<bool> {
        match map.get(key) {
            None => Ok(false),
            Some(value) => value.as_bool().with_context(|| format!("bad {key}")),
        }
    };
    let message = match map.get("error_message") {
        None => "None".to_string(),
        Some(value) => format!("Some({})", str_literal(&value.to_string())),
    };
    Ok(format!(
        "BedRule {{ can_sleep: {}, can_set_spawn: {}, destroy_on_use: {}, destroy_on_leave: {}, \
         error_message: {message} }}",
        condition(map.get("can_sleep").context("no can_sleep")?)?,
        condition(map.get("can_set_spawn").context("no can_set_spawn")?)?,
        flag("destroy_on_use")?,
        flag("destroy_on_leave")?,
    ))
}

fn sky_light(value: &Value) -> Result<f32> {
    let level = value.as_f64().context("bad sky light level")? as f32;
    ensure!(
        (0.0..=15.0).contains(&level),
        "sky light level out of range"
    );
    Ok(level)
}

fn flag_track(clock: &str, period: Option<u64>, track: &Value) -> Result<String> {
    let map = track.as_object().context("bad track")?;
    for key in map.keys() {
        ensure!(
            ["keyframes", "modifier"].contains(&key.as_str()),
            "unknown flag track field {key}"
        );
    }
    let modifier = map
        .get("modifier")
        .map(|m| m.as_str().context("bad modifier"))
        .transpose()?
        .unwrap_or("override");
    let op = BOOL_OPS
        .iter()
        .find(|(name, _)| *name == modifier)
        .with_context(|| format!("unmodeled flag modifier {modifier}"))?
        .1;
    let mut keyframes = Vec::new();
    let mut last = i64::MIN;
    for keyframe in map
        .get("keyframes")
        .and_then(Value::as_array)
        .context("no keyframes")?
    {
        let ticks = keyframe
            .get("ticks")
            .and_then(Value::as_i64)
            .context("bad keyframe ticks")?;
        ensure!(ticks > last, "keyframes out of order");
        last = ticks;
        let value = keyframe
            .get("value")
            .and_then(Value::as_bool)
            .context("bad keyframe value")?;
        keyframes.push(format!("({ticks}, {value})"));
    }
    ensure!(!keyframes.is_empty(), "empty flag track");
    Ok(format!(
        "FlagTrack {{ clock: {}, period_ticks: {}, op: BoolOp::{op}, keyframes: &[{}] }}",
        str_literal(clock),
        period.map_or("None".to_string(), |p| format!("Some({p})")),
        keyframes.join(", ")
    ))
}

fn float_track(clock: &str, period: Option<u64>, track: &Value, eases: &[&str]) -> Result<String> {
    let map = track.as_object().context("bad track")?;
    for key in map.keys() {
        ensure!(
            ["keyframes", "modifier", "ease"].contains(&key.as_str()),
            "unknown track field {key}"
        );
    }
    let modifier = map
        .get("modifier")
        .map(|m| m.as_str().context("bad modifier"))
        .transpose()?
        .unwrap_or("override");
    let op = FLOAT_OPS
        .iter()
        .find(|(name, _)| *name == modifier)
        .with_context(|| format!("unmodeled float track modifier {modifier}"))?
        .1;
    let mut bezier = None;
    let ease = match map.get("ease") {
        Some(Value::Object(ease)) => {
            ensure!(
                ease.len() == 1 && ease.contains_key("cubic_bezier"),
                "unmodeled float track easing {ease:?}"
            );
            let controls: Vec<f32> = ease["cubic_bezier"]
                .as_array()
                .context("bad cubic_bezier")?
                .iter()
                .map(|v| v.as_f64().map(|v| v as f32).context("bad cubic_bezier"))
                .collect::<Result<_>>()?;
            ensure!(controls.len() == 4, "cubic_bezier needs four controls");
            bezier = Some(controls);
            "cubic_bezier"
        }
        Some(ease) => ease.as_str().context("bad easing")?,
        None => "linear",
    };
    let named = named_easing(ease);
    let ease = if named.is_some() { "named" } else { ease };
    ensure!(eases.contains(&ease), "unmodeled float track easing {ease}");
    let mut keyframes = Vec::new();
    let mut alphas = Vec::new();
    let mut last = i64::MIN;
    for keyframe in map
        .get("keyframes")
        .and_then(Value::as_array)
        .context("no keyframes")?
    {
        let ticks = keyframe
            .get("ticks")
            .and_then(Value::as_i64)
            .context("bad keyframe ticks")?;
        ensure!(ticks >= last, "keyframes out of order");
        last = ticks;
        let raw = keyframe.get("value").context("bad keyframe value")?;
        let (value, alpha) = match (op, raw) {
            ("AlphaBlend", Value::Object(blend)) => (
                blend.get("value").and_then(Value::as_f64),
                blend.get("alpha").map_or(Some(1.0), Value::as_f64),
            ),
            ("AlphaBlend", raw) => (raw.as_f64(), Some(1.0)),
            (_, raw) => (raw.as_f64(), None),
        };
        let value = value.context("bad keyframe value")?;
        if let Some(alpha) = alpha {
            alphas.push(format!("({ticks}, {:?})", alpha as f32));
        }
        keyframes.push(format!("({ticks}, {:?})", value as f32));
    }
    ensure!(!keyframes.is_empty(), "empty float track");
    Ok(format!(
        "FloatTrack {{ clock: {}, period_ticks: {}, op: FloatOp::{op}, constant: {}, \
         bezier: {}, easing: {}, keyframes: &[{}], alphas: &[{}] }}",
        str_literal(clock),
        period.map_or("None".to_string(), |p| format!("Some({p})")),
        ease == "constant",
        bezier.map_or("None".to_string(), |c| format!(
            "Some([{:?}, {:?}, {:?}, {:?}])",
            c[0], c[1], c[2], c[3]
        )),
        named.map_or("None".to_string(), |e| format!("Some({e})")),
        keyframes.join(", "),
        alphas.join(", ")
    ))
}

fn named_easing(name: &str) -> Option<String> {
    let (direction, curve) = if let Some(rest) = name.strip_prefix("in_out_") {
        ("InOut", rest)
    } else if let Some(rest) = name.strip_prefix("in_") {
        ("In", rest)
    } else {
        ("Out", name.strip_prefix("out_")?)
    };
    let curve = [
        "back", "bounce", "circ", "cubic", "elastic", "expo", "quad", "quart", "quint", "sine",
    ]
    .into_iter()
    .find(|c| *c == curve)?;
    let mut chars = curve.chars();
    let head = chars.next()?.to_ascii_uppercase();
    Some(format!(
        "crate::easing::Easing::new(crate::easing::Direction::{direction}, crate::easing::Curve::{head}{})",
        chars.as_str()
    ))
}

fn check_template(input: &Input, template: &str) -> Result<()> {
    let (namespace, path) = template
        .split_once(':')
        .with_context(|| format!("world properties: bad template {template}"))?;
    ensure!(
        input
            .structure_templates
            .contains_key(&format!("{namespace}/structure/{path}.nbt")),
        "world properties: unknown structure template {template}"
    );
    Ok(())
}

fn check_loot_table(input: &Input, table: &str) -> Result<()> {
    let (namespace, path) = table
        .split_once(':')
        .with_context(|| format!("world properties: bad loot table {table}"))?;
    ensure!(
        input
            .data_json
            .contains_key(&format!("{namespace}/loot_table/{path}.json")),
        "world properties: unknown loot table {table}"
    );
    Ok(())
}
