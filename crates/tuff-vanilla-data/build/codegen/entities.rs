//! Entity type data from `build/EntityProperties.java`: tracking and
//! pacing, sizes, mob categories, spawn placement, the synched data layout
//! of every type, and the block data mobs walk, path and spawn over.
//! Validated strictly: every registered entity type and block state must
//! appear exactly once, and every line kind and name must be known.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::array_body;
use super::enums::IdEnum;
use crate::input::Input;
use crate::names::str_literal;

const PLACEMENTS: [&str; 4] = ["in_lava", "in_water", "no_restrictions", "on_ground"];
const HEIGHTMAPS: [&str; 6] = [
    "world_surface_wg",
    "world_surface",
    "ocean_floor_wg",
    "ocean_floor",
    "motion_blocking",
    "motion_blocking_no_leaves",
];
const CLIMATES: [&str; 3] = ["temperate", "warm", "cold"];

struct Type {
    range: u32,
    interval: Option<u32>,
    deltas: bool,
    fire_immune: bool,
    width: f32,
    height: f32,
    eye_height: f32,
    category: String,
    summonable: bool,
    spawns_far: bool,
    peaceful: bool,
    spawn_scale: f32,
    loot_table: Option<String>,
    baby: Option<(f32, f32, f32)>,
    air_methods: Option<String>,
    vehicle_attachment: Option<[f64; 3]>,
    passenger_attachments: Vec<[f64; 3]>,
    placement: Option<(String, String)>,
    data: Option<Vec<u8>>,
}

struct Category {
    max: i32,
    friendly: bool,
    persistent: bool,
    despawn: u32,
    no_despawn: u32,
}

fn flag(name: &str, value: &str) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        other => bail!("entity properties: bad {name} {other:?}"),
    }
}

fn finite(value: &str, what: &str) -> Result<f32> {
    let parsed: f32 = value
        .parse()
        .with_context(|| format!("entity properties: bad {what} {value:?}"))?;
    ensure!(
        parsed.is_finite(),
        "entity properties: {what} {value} is not finite"
    );
    Ok(parsed)
}

fn enum_variant<'a>(name: &'a str, entries: &[&'a str], entry: &str) -> Result<String> {
    let variants = IdEnum {
        doc: String::new(),
        name,
        id_type: "u8",
        entries: entries.to_vec(),
        default: None,
    }
    .variants()?;
    let index = entries
        .iter()
        .position(|e| *e == entry)
        .with_context(|| format!("entity properties: unknown {name} {entry:?}"))?;
    Ok(variants[index].clone())
}

/// Parses a sheep color tree: a color name or `(weight:tree ...)`.
fn color_tree(text: &str, colors: &[&str]) -> Result<String> {
    fn parse<'a>(text: &'a str, colors: &[&str]) -> Result<(String, &'a str)> {
        if let Some(mut rest) = text.strip_prefix('(') {
            let mut entries = Vec::new();
            loop {
                rest = rest.trim_start();
                if let Some(after) = rest.strip_prefix(')') {
                    ensure!(!entries.is_empty(), "sheep colors: empty weighted list");
                    return Ok((
                        format!("ColorTree::Weighted(&[{}])", entries.join(", ")),
                        after,
                    ));
                }
                let (weight, after) = rest
                    .split_once(':')
                    .context("sheep colors: entry without a weight")?;
                let weight: u32 = weight.parse()?;
                ensure!(weight > 0, "sheep colors: zero weight");
                let (tree, after) = parse(after, colors)?;
                entries.push(format!("({weight}, {tree})"));
                rest = after;
            }
        }
        let end = text.find([' ', ')']).unwrap_or(text.len());
        let name = &text[..end];
        ensure!(
            colors.contains(&name),
            "sheep colors: unknown dye color {name:?}"
        );
        Ok((
            format!("ColorTree::Color({})", str_literal(name)),
            &text[end..],
        ))
    }
    let (tree, rest) = parse(text, colors)?;
    ensure!(rest.is_empty(), "sheep colors: trailing {rest:?}");
    Ok(tree)
}

fn spawn_ids(list: &str, type_count: usize) -> Result<String> {
    let ids: Vec<u16> = list
        .split('.')
        .map(|i| i.parse::<u16>())
        .collect::<Result<_, _>>()?;
    ensure!(
        ids.iter().all(|i| usize::from(*i) < type_count),
        "entity properties: spawnable type id out of range"
    );
    Ok(ids
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", "))
}

pub fn generate(input: &Input) -> Result<String> {
    let mut position_tolerance = None;
    let mut forced_position_period = None;
    let mut creature_spawn_probability = None;
    let mut categories: Vec<(&str, Category)> = Vec::new();
    let mut types: BTreeMap<&str, Type> = BTreeMap::new();
    let mut path_types: Vec<(&str, f32)> = Vec::new();
    let mut serializers: BTreeMap<u8, &str> = BTreeMap::new();
    let mut movement: BTreeMap<&str, (f32, f32, f32, f32, f32)> = BTreeMap::new();
    let mut paths: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    let mut spawnable: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut dangerous: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut redstone: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    let mut sheep_colors: BTreeMap<&str, &str> = BTreeMap::new();
    let mut spawn_armor: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut air_supply = None;
    let mut poses: Vec<&str> = Vec::new();
    let mut player_poses: BTreeMap<&str, (f32, f32, f32)> = BTreeMap::new();

    for line in input.entity_properties.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        match parts.as_slice() {
            ["tracker", "position_tolerance", value] => {
                position_tolerance = Some(value.parse::<f64>()?);
            }
            ["tracker", "forced_position_period", value] => {
                forced_position_period = Some(value.parse::<u32>()?);
            }
            ["creature_spawn_probability", value] => {
                creature_spawn_probability = Some(finite(value, "spawn probability")?);
            }
            [
                "category",
                name,
                max,
                friendly,
                persistent,
                despawn,
                no_despawn,
            ] => {
                ensure!(
                    categories.iter().all(|(n, _)| n != name),
                    "entity properties: category {name} listed twice"
                );
                categories.push((
                    name,
                    Category {
                        max: max.parse()?,
                        friendly: flag("friendly", friendly)?,
                        persistent: flag("persistent", persistent)?,
                        despawn: despawn.parse()?,
                        no_despawn: no_despawn.parse()?,
                    },
                ));
            }
            [
                "entity",
                id,
                range,
                interval,
                deltas,
                fire_immune,
                width,
                height,
                eye_height,
                category,
                summonable,
                spawns_far,
                peaceful,
                spawn_scale,
                loot_table,
            ] => {
                let entry = Type {
                    range: range.parse()?,
                    interval: match *interval {
                        "never" => None,
                        ticks => Some(ticks.parse()?),
                    },
                    deltas: flag("track deltas", deltas)?,
                    fire_immune: flag("fire immunity", fire_immune)?,
                    width: finite(width, "width")?,
                    height: finite(height, "height")?,
                    eye_height: finite(eye_height, "eye height")?,
                    category: category.to_string(),
                    summonable: flag("summonable", summonable)?,
                    spawns_far: flag("spawns far", spawns_far)?,
                    peaceful: flag("peaceful", peaceful)?,
                    spawn_scale: finite(spawn_scale, "spawn scale")?,
                    loot_table: (*loot_table != "none").then(|| loot_table.to_string()),
                    baby: None,
                    air_methods: None,
                    vehicle_attachment: None,
                    passenger_attachments: Vec::new(),
                    placement: None,
                    data: None,
                };
                ensure!(
                    entry.width >= 0.0 && entry.height >= 0.0,
                    "entity properties: {id} has a bad size"
                );
                ensure!(
                    entry.interval != Some(0),
                    "entity properties: {id} has a zero update interval"
                );
                ensure!(
                    types.insert(id, entry).is_none(),
                    "entity properties: {id} listed twice"
                );
            }
            ["vehicle_attachment", id, x, y, z] => {
                let entry = types
                    .get_mut(id)
                    .with_context(|| format!("entity properties: attachment before {id}"))?;
                ensure!(
                    entry.vehicle_attachment.is_none(),
                    "entity properties: {id} attachment twice"
                );
                entry.vehicle_attachment = Some([
                    f64::from(finite(x, "attachment")?),
                    f64::from(finite(y, "attachment")?),
                    f64::from(finite(z, "attachment")?),
                ]);
            }
            ["passenger_attachment", id, x, y, z] => {
                let entry = types
                    .get_mut(id)
                    .with_context(|| format!("entity properties: seat before {id}"))?;
                entry.passenger_attachments.push([
                    f64::from(finite(x, "seat")?),
                    f64::from(finite(y, "seat")?),
                    f64::from(finite(z, "seat")?),
                ]);
            }
            ["baby", id, width, height, eye] => {
                let entry = types
                    .get_mut(id)
                    .with_context(|| format!("entity properties: baby size before {id}"))?;
                ensure!(entry.baby.is_none(), "entity properties: {id} baby twice");
                entry.baby = Some((
                    finite(width, "baby width")?,
                    finite(height, "baby height")?,
                    finite(eye, "baby eye height")?,
                ));
            }
            ["air", id, methods] => {
                let entry = types
                    .get_mut(id)
                    .with_context(|| format!("entity properties: air before {id}"))?;
                ensure!(
                    entry.air_methods.is_none() && !methods.is_empty(),
                    "entity properties: bad air methods for {id}"
                );
                entry.air_methods = Some(methods.to_string());
            }
            ["air_supply", ticks] => {
                ensure!(air_supply.is_none(), "entity properties: air supply twice");
                air_supply = Some(ticks.parse::<i32>()?);
            }
            ["pose", name, id] => {
                ensure!(
                    id.parse::<usize>()? == poses.len() && !poses.contains(name),
                    "entity properties: pose {name} is out of order"
                );
                poses.push(name);
            }
            ["player_pose", pose, width, height, eye] => {
                ensure!(
                    poses.contains(pose),
                    "entity properties: player pose {pose} is not a pose"
                );
                ensure!(
                    player_poses
                        .insert(
                            pose,
                            (
                                finite(width, "pose width")?,
                                finite(height, "pose height")?,
                                finite(eye, "pose eye height")?,
                            ),
                        )
                        .is_none(),
                    "entity properties: player pose {pose} twice"
                );
            }
            ["placement", id, placement, heightmap] => {
                let entry = types
                    .get_mut(id)
                    .with_context(|| format!("entity properties: placement before {id}"))?;
                entry.placement = Some((placement.to_string(), heightmap.to_ascii_lowercase()));
            }
            ["serializer", id, name] => {
                ensure!(
                    !name.is_empty()
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                    "entity properties: bad serializer name {name:?}"
                );
                ensure!(
                    serializers.values().all(|n| n != name),
                    "entity properties: serializer {name} listed twice"
                );
                ensure!(
                    serializers.insert(id.parse()?, name).is_none(),
                    "entity properties: serializer id {id} listed twice"
                );
            }
            ["data", id, serializers @ ..] => {
                let entry = types
                    .get_mut(id)
                    .with_context(|| format!("entity properties: data before {id}"))?;
                entry.data = Some(
                    serializers
                        .iter()
                        .map(|s| s.parse::<u8>())
                        .collect::<Result<_, _>>()?,
                );
            }
            ["pathtype", name, malus] => {
                path_types.push((name, finite(malus, "path malus")?));
            }
            ["movement", block, friction, speed, jump, reduction, bounce] => {
                let values = (
                    finite(friction, "friction")?,
                    finite(speed, "speed factor")?,
                    finite(jump, "jump factor")?,
                    finite(reduction, "fall distance reduction")?,
                    finite(bounce, "bounce restitution")?,
                );
                ensure!(
                    movement.insert(block, values).is_none(),
                    "entity properties: movement of {block} twice"
                );
            }
            ["path", block, values @ ..] => {
                let values: Vec<u8> = values
                    .iter()
                    .map(|v| v.parse::<u8>())
                    .collect::<Result<_, _>>()?;
                ensure!(
                    paths.insert(block, values).is_none(),
                    "entity properties: path of {block} twice"
                );
            }
            ["spawnable", block, values @ ..] => {
                ensure!(
                    spawnable.insert(block, values.to_vec()).is_none(),
                    "entity properties: spawnable of {block} twice"
                );
            }
            ["dangerous", block, values @ ..] => {
                ensure!(
                    dangerous.insert(block, values.to_vec()).is_none(),
                    "entity properties: dangerous of {block} twice"
                );
            }
            ["redstone", block, values @ ..] => {
                let values: Vec<u8> = values
                    .iter()
                    .map(|v| v.parse::<u8>())
                    .collect::<Result<_, _>>()?;
                ensure!(
                    values.iter().all(|v| *v < 4),
                    "entity properties: bad redstone flags for {block}"
                );
                ensure!(
                    redstone.insert(block, values).is_none(),
                    "entity properties: redstone of {block} twice"
                );
            }
            ["sheep_colors", climate, ..] => {
                ensure!(
                    CLIMATES.contains(climate),
                    "entity properties: unknown sheep climate {climate}"
                );
                let tree = line.splitn(3, ' ').nth(2).unwrap_or_default();
                ensure!(
                    sheep_colors.insert(climate, tree).is_none(),
                    "entity properties: sheep colors for {climate} twice"
                );
            }
            ["spawn_armor", slot, tiers @ ..] => {
                ensure!(
                    !tiers.is_empty(),
                    "entity properties: no spawn armor for {slot}"
                );
                spawn_armor.push((slot, tiers.to_vec()));
            }
            _ => bail!("entity properties: unknown line {line:?}"),
        }
    }

    let position_tolerance =
        position_tolerance.context("entity properties: no position tolerance")?;
    let forced_position_period =
        forced_position_period.context("entity properties: no forced position period")?;
    ensure!(
        forced_position_period > 0,
        "entity properties: zero forced period"
    );
    let creature_spawn_probability =
        creature_spawn_probability.context("entity properties: no creature spawn probability")?;
    let air_supply = air_supply.context("entity properties: no air supply")?;
    ensure!(air_supply > 0, "entity properties: air supply {air_supply}");
    ensure!(
        !poses.is_empty() && player_poses.len() == poses.len(),
        "entity properties: the player has {} of {} poses",
        player_poses.len(),
        poses.len()
    );
    ensure!(!categories.is_empty(), "entity properties: no categories");
    ensure!(
        !serializers.is_empty() && serializers.keys().copied().eq(0..serializers.len() as u8),
        "entity properties: serializer ids are not dense from 0"
    );
    ensure!(
        !path_types.is_empty() && path_types.len() < 32,
        "entity properties: {} path types do not fit beside the pathfindable bit",
        path_types.len()
    );
    ensure!(
        sheep_colors.len() == CLIMATES.len(),
        "entity properties: sheep colors for {} climates",
        sheep_colors.len()
    );

    let registry = &input.registries["minecraft:entity_type"];
    let entries = super::registries::sorted_entries(registry)?;
    ensure!(
        entries.len() == types.len(),
        "entity properties: {} types dumped, {} registered",
        types.len(),
        entries.len()
    );

    ensure!(
        spawn_armor.len() == 4
            && spawn_armor
                .iter()
                .all(|(_, t)| t.len() == spawn_armor[0].1.len()),
        "entity properties: spawn armor is not four slots of equal tiers"
    );
    let items = super::registries::variants(&input.registries["minecraft:item"])?;
    let mut out = String::new();
    writeln!(
        out,
        "/// The armor mobs may spawn in, by slot in the order it is put on, \
         then by tier."
    )?;
    write!(
        out,
        "pub static SPAWN_ARMOR: [(crate::item::EquipmentSlot, &[crate::Item]); 4] = ["
    )?;
    for (slot, tiers) in &spawn_armor {
        let tiers: Vec<String> = tiers
            .iter()
            .map(|item| {
                items
                    .get(*item)
                    .map(|v| format!("crate::Item::{v}"))
                    .with_context(|| format!("entity properties: unknown item {item}"))
            })
            .collect::<Result<_>>()?;
        write!(
            out,
            "(crate::item::EquipmentSlot::{}, &[{}]), ",
            crate::names::pascal(slot),
            tiers.join(", ")
        )?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "pub const POSITION_TOLERANCE: f64 = {position_tolerance:?};"
    )?;
    writeln!(
        out,
        "pub const FORCED_POSITION_PERIOD: u32 = {forced_position_period};"
    )?;
    writeln!(
        out,
        "pub const CREATURE_SPAWN_PROBABILITY: f32 = {creature_spawn_probability:?};"
    )?;
    writeln!(
        out,
        "/// The air supply entities start with and refill to, in ticks."
    )?;
    writeln!(out, "pub const MAX_AIR_SUPPLY: i32 = {air_supply};")?;
    IdEnum {
        doc: "An entity pose; the id is its network id.".into(),
        name: "Pose",
        id_type: "u8",
        entries: poses.clone(),
        default: None,
    }
    .emit(&mut out)?;
    write!(out, "static PLAYER_POSES: [Size; {}] = [", poses.len())?;
    array_body(
        &mut out,
        poses.iter().map(|pose| {
            let (width, height, eye) = player_poses[pose];
            format!(
                "Size {{ width: {width:?}, height: {height:?}, eye_height: {eye:?}, fire_immune: false }}"
            )
        }),
    );
    writeln!(out, "];")?;

    let category_names: Vec<&str> = categories.iter().map(|(n, _)| *n).collect();
    IdEnum {
        doc: "A mob category: its natural spawning cap and despawn rules.".into(),
        name: "MobCategory",
        id_type: "u8",
        entries: category_names.clone(),
        default: None,
    }
    .emit(&mut out)?;
    write!(
        out,
        "static CATEGORIES: [CategoryInfo; {}] = [",
        categories.len()
    )?;
    array_body(
        &mut out,
        categories.iter().map(|(_, c)| {
            format!(
                "CategoryInfo {{ max_per_chunk: {}, friendly: {}, persistent: {}, despawn_distance: {}, no_despawn_distance: {} }}",
                c.max, c.friendly, c.persistent, c.despawn, c.no_despawn
            )
        }),
    );
    writeln!(out, "];")?;

    let path_names: Vec<&str> = path_types.iter().map(|(n, _)| *n).collect();
    IdEnum {
        doc: "How a mob's pathfinding sees a block.".into(),
        name: "PathType",
        id_type: "u8",
        entries: path_names,
        default: None,
    }
    .emit(&mut out)?;
    write!(out, "static PATH_MALUS: [f32; {}] = [", path_types.len())?;
    array_body(&mut out, path_types.iter().map(|(_, m)| format!("{m:?}")));
    writeln!(out, "];")?;

    IdEnum {
        doc: "Where natural spawning puts a type.".into(),
        name: "SpawnPlacement",
        id_type: "u8",
        entries: PLACEMENTS.to_vec(),
        default: None,
    }
    .emit(&mut out)?;
    IdEnum {
        doc: "A heightmap kind.".into(),
        name: "Heightmap",
        id_type: "u8",
        entries: HEIGHTMAPS.to_vec(),
        default: None,
    }
    .emit(&mut out)?;

    writeln!(out, "static TRACKING: [Tracking; {}] = [", entries.len())?;
    for name in &entries {
        let entry = types
            .get(*name)
            .with_context(|| format!("entity properties: {name} is not dumped"))?;
        let interval = match entry.interval {
            Some(ticks) => format!("Some({ticks})"),
            None => "None".to_string(),
        };
        writeln!(
            out,
            "    Tracking {{ range_chunks: {}, update_interval: {interval}, track_deltas: {} }},",
            entry.range, entry.deltas
        )?;
    }
    writeln!(out, "];")?;
    writeln!(out, "static SIZES: [Size; {}] = [", entries.len())?;
    for name in &entries {
        let entry = &types[*name];
        writeln!(
            out,
            "    Size {{ width: {:?}, height: {:?}, eye_height: {:?}, fire_immune: {} }},",
            entry.width, entry.height, entry.eye_height, entry.fire_immune
        )?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "static VEHICLE_ATTACHMENTS: [[f64; 3]; {}] = [",
        entries.len()
    )?;
    for name in &entries {
        let seat = types[*name].vehicle_attachment.unwrap_or([0.0; 3]);
        writeln!(out, "    [{:?}, {:?}, {:?}],", seat[0], seat[1], seat[2])?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "static PASSENGER_ATTACHMENTS: [&[[f64; 3]]; {}] = [",
        entries.len()
    )?;
    for name in &entries {
        let seats = &types[*name].passenger_attachments;
        ensure!(!seats.is_empty(), "entity properties: {name} has no seat");
        let points: Vec<String> = seats
            .iter()
            .map(|p| format!("[{:?}, {:?}, {:?}]", p[0], p[1], p[2]))
            .collect();
        writeln!(out, "    &[{}],", points.join(", "))?;
    }
    writeln!(out, "];")?;
    writeln!(out, "static MOBS: [MobInfo; {}] = [", entries.len())?;
    for name in &entries {
        let entry = &types[*name];
        let (placement, heightmap) = entry
            .placement
            .as_ref()
            .with_context(|| format!("entity properties: {name} has no placement"))?;
        let baby = match entry.baby {
            Some((w, h, e)) => format!(
                "Some(Size {{ width: {w:?}, height: {h:?}, eye_height: {e:?}, fire_immune: {} }})",
                entry.fire_immune
            ),
            None => "None".into(),
        };
        let loot = match &entry.loot_table {
            Some(table) => format!("Some({})", str_literal(table)),
            None => "None".into(),
        };
        writeln!(
            out,
            "    MobInfo {{ category: MobCategory::{}, summonable: {}, spawns_far_from_players: {}, allowed_in_peaceful: {}, spawn_box_scale: {:?}, loot_table: {loot}, baby: {baby}, own_air_rules: {}, placement: SpawnPlacement::{}, heightmap: Heightmap::{} }},",
            enum_variant("MobCategory", &category_names, &entry.category)?,
            entry.summonable,
            entry.spawns_far,
            entry.peaceful,
            entry.spawn_scale,
            entry.air_methods.is_some(),
            enum_variant("SpawnPlacement", &PLACEMENTS, placement)?,
            enum_variant("Heightmap", &HEIGHTMAPS, heightmap)?,
        )?;
    }
    writeln!(out, "];")?;
    writeln!(out, "pub mod data_serializer {{")?;
    for (id, name) in &serializers {
        writeln!(
            out,
            "    pub const {}: u8 = {id};",
            name.to_ascii_uppercase()
        )?;
    }
    writeln!(out, "}}")?;
    writeln!(out, "static DATA_LAYOUTS: [&[u8]; {}] = [", entries.len())?;
    for name in &entries {
        let data = types[*name]
            .data
            .as_ref()
            .with_context(|| format!("entity properties: {name} has no data layout"))?;
        ensure!(
            data.iter().all(|id| serializers.contains_key(id)),
            "entity properties: {name} uses an unknown data serializer"
        );
        let list: Vec<String> = data.iter().map(u8::to_string).collect();
        writeln!(out, "    &[{}],", list.join(", "))?;
    }
    writeln!(out, "];")?;

    let block_registry = &input.registries["minecraft:block"];
    let blocks = super::registries::sorted_entries(block_registry)?;
    ensure!(
        movement.len() == blocks.len()
            && paths.len() == blocks.len()
            && spawnable.len() == blocks.len()
            && dangerous.len() == blocks.len()
            && redstone.len() == blocks.len(),
        "entity properties: block lines do not cover the {} blocks",
        blocks.len()
    );
    writeln!(
        out,
        "static MOVEMENT: [BlockMovement; {}] = [",
        blocks.len()
    )?;
    for block in &blocks {
        let (friction, speed, jump, reduction, bounce) = movement
            .get(block)
            .with_context(|| format!("entity properties: no movement for {block}"))?;
        writeln!(
            out,
            "    BlockMovement {{ friction: {friction:?}, speed_factor: {speed:?}, jump_factor: {jump:?}, fall_distance_reduction: {reduction:?}, bounce_restitution: {bounce:?} }},"
        )?;
    }
    writeln!(out, "];")?;

    let state_count: usize = input.blocks.values().map(|b| b.states.len()).sum();
    let mut state_paths: Vec<Option<u8>> = vec![None; state_count];
    let mut state_spawnable: Vec<Option<String>> = vec![None; state_count];
    let mut state_dangerous: Vec<Option<String>> = vec![None; state_count];
    let mut state_redstone: Vec<Option<u8>> = vec![None; state_count];
    let type_set = |spawn: &str| -> Result<String> {
        Ok(match spawn {
            "a" => "Spawnable::All".to_string(),
            "n" => "Spawnable::None".to_string(),
            other => match (other.strip_prefix('+'), other.strip_prefix('-')) {
                (Some(list), _) => {
                    format!("Spawnable::Only(&[{}])", spawn_ids(list, entries.len())?)
                }
                (_, Some(list)) => {
                    format!("Spawnable::Except(&[{}])", spawn_ids(list, entries.len())?)
                }
                _ => bail!("entity properties: bad entity type set {other:?}"),
            },
        })
    };
    for (name, report) in &input.blocks {
        let values = paths
            .get(name.as_str())
            .with_context(|| format!("entity properties: no path types for {name}"))?;
        let spawns = spawnable
            .get(name.as_str())
            .with_context(|| format!("entity properties: no spawn rules for {name}"))?;
        ensure!(
            values.len() == report.states.len() && spawns.len() == report.states.len(),
            "entity properties: {name} has {} states, dumped {}",
            report.states.len(),
            values.len()
        );
        let hurts = &dangerous[name.as_str()];
        let flags = &redstone[name.as_str()];
        ensure!(
            hurts.len() == report.states.len() && flags.len() == report.states.len(),
            "entity properties: {name} danger or redstone states do not match"
        );
        let mut ids: Vec<u32> = report.states.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        for (i, id) in ids.iter().enumerate() {
            state_dangerous[*id as usize] = Some(type_set(hurts[i])?);
            state_redstone[*id as usize] = Some(flags[i]);
        }
        for ((id, value), spawn) in ids.iter().zip(values).zip(spawns) {
            ensure!(
                usize::from(value & 31) < path_types.len() && value >> 7 == 0,
                "entity properties: bad path value {value} for {name}"
            );
            let slot = state_paths
                .get_mut(*id as usize)
                .context("entity properties: state id out of range")?;
            ensure!(slot.is_none(), "entity properties: state {id} twice");
            *slot = Some(*value);
            state_spawnable[*id as usize] = Some(type_set(spawn)?);
        }
    }
    write!(out, "static PATHS: [u8; {state_count}] = [")?;
    array_body(
        &mut out,
        state_paths
            .iter()
            .map(|v| v.context("entity properties: a state has no path type"))
            .collect::<Result<Vec<_>>>()?,
    );
    writeln!(out, "];")?;
    write!(out, "static SPAWNABLE: [Spawnable; {state_count}] = [")?;
    array_body(
        &mut out,
        state_spawnable
            .into_iter()
            .map(|v| v.context("entity properties: a state has no spawn rule"))
            .collect::<Result<Vec<_>>>()?,
    );
    writeln!(out, "];")?;

    write!(out, "static DANGEROUS: [Spawnable; {state_count}] = [")?;
    array_body(
        &mut out,
        state_dangerous
            .into_iter()
            .map(|v| v.context("entity properties: a state has no danger rule"))
            .collect::<Result<Vec<_>>>()?,
    );
    writeln!(out, "];")?;
    write!(out, "static REDSTONE: [u8; {state_count}] = [")?;
    array_body(
        &mut out,
        state_redstone
            .into_iter()
            .map(|v| v.context("entity properties: a state has no redstone flags"))
            .collect::<Result<Vec<_>>>()?,
    );
    writeln!(out, "];")?;

    let colors: Vec<&str> = input
        .item_properties
        .lines()
        .filter_map(|line| line.strip_prefix("dye "))
        .filter_map(|rest| rest.split(' ').next())
        .collect();
    ensure!(
        !colors.is_empty(),
        "entity properties: no dye colors in the item properties"
    );
    let mut trees = Vec::new();
    for climate in CLIMATES {
        trees.push(color_tree(sheep_colors[climate], &colors)?);
    }
    write!(out, "static SHEEP_COLORS: [ColorTree; 3] = [")?;
    array_body(&mut out, trees);
    writeln!(out, "];")?;
    Ok(out)
}
