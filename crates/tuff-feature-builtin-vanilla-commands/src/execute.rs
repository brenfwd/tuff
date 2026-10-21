//! `/execute`: modifies who and where a command runs as, tests
//! conditions, and runs the command after `run` once per resulting source.

use tuff_core::command::args::{Anchor, ScoreHolderInput, Value};
use tuff_core::command::nbt_path::NbtPath;
use tuff_core::command::selector::Selector;
use tuff_core::command::{CommandError, ParsedNode};
use tuff_core::world::heightmap_kind::HeightmapKind;
use tuff_item::text::TextComponent;
use tuff_nbt::Tag;
use tuff_vanilla_common::spawn_search::column_height;

use super::data::{Accessor, NumberKind, store_number};
use super::predicates;
use super::scoreboard::{objective_named, resolve_holders};
use super::targets::{self, Target};
use super::world::{BlockBox, read_blocks};
use super::{Context, Executor, Frame, Outcome, Source, executor_future, text};

const MAX_TEST_AREA: i64 = 32_768;

/// The nodes of one subcommand after `execute`, e.g. `as <targets>`.
struct Step<'a> {
    literal: &'a str,
    nodes: &'a [ParsedNode],
}

const SUBCOMMANDS: &[&str] = &[
    "align",
    "anchored",
    "as",
    "at",
    "facing",
    "if",
    "in",
    "positioned",
    "rotated",
    "run",
    "unless",
    "store",
    "summon",
    "on",
];

fn steps(nodes: &[ParsedNode]) -> Vec<Step<'_>> {
    let mut steps = Vec::new();
    let mut start = None;
    for (index, node) in nodes.iter().enumerate().skip(1) {
        let is_literal = matches!(
            node.node.kind,
            tuff_vanilla_data::command::CommandNodeKind::Literal
        );
        if is_literal && SUBCOMMANDS.contains(&node.node.name) && Some(index) != start {
            if let Some(from) = start {
                steps.push(Step {
                    literal: nodes[from].node.name,
                    nodes: &nodes[from..index],
                });
            }
            start = Some(index);
        }
    }
    if let Some(from) = start {
        steps.push(Step {
            literal: nodes[from].node.name,
            nodes: &nodes[from..],
        });
    }
    steps
}

fn value<'a>(step: &Step<'a>, name: &str) -> Option<&'a Value> {
    step.nodes
        .iter()
        .find(|n| n.node.name == name && n.value.is_some())
        .and_then(|n| n.value.as_ref())
}

fn literal_at<'a>(step: &Step<'a>, index: usize) -> Option<&'a str> {
    step.nodes.get(index).map(|n| n.node.name)
}

fn selector<'a>(step: &Step<'a>, name: &str) -> Result<&'a Selector, CommandError> {
    match value(step, name) {
        Some(Value::Selector(selector)) => Ok(selector),
        _ => Err(CommandError::simple("command.unknown.argument")),
    }
}

/// The entity or knot holding the lead on entity `id`, as a source.
async fn leasher(ctx: &mut Context<'_>, source: &Source, id: i32) -> Option<Source> {
    let (_, mut data) = super::data::find_entity(ctx, id).await?;
    let leash = tuff_core::world::leash::Leash::load(&mut data)?;
    let entities = targets::entities(ctx).await;
    let holder = match leash.anchor {
        tuff_core::world::leash::Anchor::Entity(uuid) => entities
            .into_iter()
            .find(|e| e.uuid == uuid)
            .map(Target::Entity)
            .or(targets::players(ctx)
                .await
                .into_iter()
                .find(|p| p.uuid == uuid)
                .map(Target::Player)),
        tuff_core::world::leash::Anchor::Knot(pos) => entities
            .into_iter()
            .find(|e| {
                e.kind == tuff_vanilla_data::registry::EntityType::LeashKnot
                    && e.position.map(|c| c.floor() as i32) == [pos.x, pos.y, pos.z]
            })
            .map(Target::Entity),
    }?;
    Some(with_target(source, &holder))
}

async fn by_uuid(ctx: &mut Context<'_>, uuid: u128) -> Option<Target> {
    if let Some(entity) = targets::entities(ctx)
        .await
        .into_iter()
        .find(|e| e.uuid == uuid)
    {
        return Some(Target::Entity(entity));
    }
    targets::players(ctx)
        .await
        .into_iter()
        .find(|p| p.uuid == uuid)
        .map(Target::Player)
}

/// The entities `on <relation>` reaches from entity `id`, read from its
/// saved form.
async fn related(ctx: &mut Context<'_>, id: i32, relation: &str) -> Vec<Target> {
    let mut uuids = Vec::new();
    if let Some((_, data)) = super::data::find_entity(ctx, id).await {
        let uuid_of = |key: &str| data.get(key).and_then(Tag::as_uuid);
        match relation {
            "passengers" | "controller" => {
                if let Some(Tag::List(list)) = data.get("Passengers") {
                    uuids.extend(list.iter().filter_map(|t| match t {
                        Tag::Compound(c) => c.get("UUID").and_then(Tag::as_uuid),
                        _ => None,
                    }));
                }
            }
            "owner" | "origin" => {
                uuids.extend(uuid_of("Owner").or_else(|| uuid_of("owner")));
                if relation == "origin" && uuids.is_empty() {
                    uuids.extend(uuid_of("Thrower"));
                }
            }
            "attacker" => {
                uuids.extend(uuid_of("last_hurt_by_mob").or_else(|| uuid_of("last_hurt_by_player")))
            }
            "target" => uuids.extend(uuid_of("angry_at").or_else(|| uuid_of("Target"))),
            _ => {}
        }
    }
    if relation == "controller" {
        uuids.truncate(1);
    }
    let mut out = Vec::new();
    for uuid in uuids {
        out.extend(by_uuid(ctx, uuid).await);
    }
    if matches!(relation, "passengers" | "controller") {
        out.extend(super::ride::player_passengers(ctx, id).await);
        if relation == "controller" {
            out.truncate(1);
        }
    }
    out
}

fn with_target(source: &Source, target: &Target) -> Source {
    let mut next = source.clone();
    next.entity = Some(target.id());
    next.name = target.display_name().plain();
    next
}

fn at_target(source: &Source, target: &Target) -> Source {
    let mut next = source.clone();
    next.dimension = target.dimension();
    next.position = target.position();
    next.rotation = target.rotation();
    next
}

fn facing(source: &Source, point: [f64; 3]) -> Source {
    let anchor = source.anchor_position();
    let dx = point[0] - anchor[0];
    let dy = point[1] - anchor[1];
    let dz = point[2] - anchor[2];
    let horizontal = (dx * dx + dz * dz).sqrt();
    let pitch =
        tuff_core::command::coords::wrap_degrees(-(dy.atan2(horizontal).to_degrees()) as f32);
    let yaw = tuff_core::command::coords::wrap_degrees((dz.atan2(dx).to_degrees() - 90.0) as f32);
    let mut next = source.clone();
    next.rotation = [yaw, pitch];
    next
}

pub struct Execute;

/// The outcome of a condition for one source: how many things matched
/// (one for a plain yes).
enum Condition {
    Pass(i32),
    Fail(i32),
}

async fn condition(
    ctx: &mut Context<'_>,
    source: &Source,
    step: &Step<'_>,
) -> Result<Condition, CommandError> {
    let original = std::mem::replace(&mut ctx.source, source.clone());
    let result = condition_inner(ctx, step).await;
    ctx.source = original;
    result
}

async fn condition_inner(
    ctx: &mut Context<'_>,
    step: &Step<'_>,
) -> Result<Condition, CommandError> {
    let kind = literal_at(step, 1).unwrap_or("");
    let pose = ctx.source.pose();
    let dimension = ctx.source.dimension;
    let pass = |ok: bool| {
        if ok {
            Condition::Pass(1)
        } else {
            Condition::Fail(0)
        }
    };
    Ok(match kind {
        "block" => {
            let (Some(Value::Coordinates(pos)), Some(Value::BlockPredicate(predicate))) =
                (value(step, "pos"), value(step, "block"))
            else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let predicate = predicate
                .clone()
                .resolved(&ctx.shared().datapacks.contents().tags)?;
            pass(super::world::test_block(ctx, dimension, pos.block(&pose), &predicate).await?)
        }
        "blocks" => {
            let (
                Some(Value::Coordinates(start)),
                Some(Value::Coordinates(end)),
                Some(Value::Coordinates(destination)),
            ) = (
                value(step, "start"),
                value(step, "end"),
                value(step, "destination"),
            )
            else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let masked = step.nodes.iter().any(|n| n.node.name == "masked");
            let from = BlockBox::from_corners(start.block(&pose), end.block(&pose));
            if from.volume() > MAX_TEST_AREA {
                return Err(CommandError::dynamic(
                    "commands.execute.blocks.toobig",
                    [text(MAX_TEST_AREA), text(from.volume())],
                ));
            }
            let target = destination.block(&pose);
            let offset = [
                target[0] - from.min[0],
                target[1] - from.min[1],
                target[2] - from.min[2],
            ];
            let sources: Vec<[i32; 3]> = from.positions().collect();
            let targets: Vec<[i32; 3]> = sources
                .iter()
                .map(|p| [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]])
                .collect();
            let source_states = read_blocks(ctx, dimension, &sources, true).await?;
            let target_states = read_blocks(ctx, dimension, &targets, true).await?;
            let mut count = 0;
            for (a, b) in source_states.iter().zip(&target_states) {
                if masked && a.is_some_and(|s| s.block() == tuff_vanilla_data::Block::Air) {
                    continue;
                }
                if a != b || a.is_none() {
                    return Ok(Condition::Fail(0));
                }
                count += 1;
            }
            Condition::Pass(count)
        }
        "entity" => {
            let selector = selector(step, "entities")?.clone();
            match targets::select_entities(ctx, &selector).await {
                Ok(found) => Condition::Pass(found.len() as i32),
                Err(_) => Condition::Fail(0),
            }
        }
        "score" => pass(score_condition(ctx, step).await?),
        "predicate" => {
            let argument = value(step, "predicate")
                .ok_or_else(|| CommandError::simple("command.unknown.argument"))?;
            let condition = predicates::resolve(ctx.shared(), argument)?;
            pass(predicates::test_source(ctx, condition).await)
        }
        "data" => {
            let accessor = Accessor::from_value(ctx, value(step, "source").cloned()).await?;
            let Some(Value::NbtPath(path)) = value(step, "path") else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let root = Tag::Compound(accessor.read(ctx).await?);
            let count = path.count_matching(&root) as i32;
            if count > 0 {
                Condition::Pass(count)
            } else {
                Condition::Fail(0)
            }
        }
        "items" | "slots" => {
            let accessor = Accessor::from_value(ctx, value(step, "source").cloned()).await?;
            let slot = super::item::SlotArg::from_value(ctx, value(step, "slots"))?;
            let slots = super::item::existing_slots(ctx, &accessor, &slot).await?;
            let count = if kind == "slots" {
                slots.len() as i32
            } else {
                let Some(Value::ItemPredicate(predicate)) = value(step, "item_predicate") else {
                    return Err(CommandError::simple("command.unknown.argument"));
                };
                let predicate = predicate
                    .clone()
                    .resolved(&ctx.shared().datapacks.contents().tags)?;
                slots
                    .iter()
                    .flatten()
                    .filter(|stack| predicate.test(stack))
                    .map(|stack| i32::from(stack.count()))
                    .sum()
            };
            if count > 0 {
                Condition::Pass(count)
            } else {
                Condition::Fail(0)
            }
        }
        "dimension" => {
            let Some(Value::Identifier(key)) = value(step, "dimension") else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            pass(ctx.dimension_key() == key)
        }
        "biome" => {
            let (Some(Value::Coordinates(pos)), Some(Value::ResourceOrTag { name, tag })) =
                (value(step, "pos"), value(step, "biome"))
            else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let biomes = ctx.shared().dimensions.biomes().clone();
            let tags = ctx.shared().datapacks.contents().tags.clone();
            let members = super::locate::biome_members(&biomes, name, *tag, &tags)
                .ok_or_else(|| super::locate::unknown_tag(name, "minecraft:worldgen/biome"))?;
            let found = super::world::read_biome(ctx, dimension, pos.block(&pose)).await?;
            pass(found.is_some_and(|biome| members.contains(&biome)))
        }
        "stopwatch" => {
            let (Some(Value::Identifier(id)), Some(Value::DoubleRange(range))) =
                (value(step, "id"), value(step, "range"))
            else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let elapsed = ctx
                .shared()
                .stopwatches
                .elapsed_seconds(id)
                .ok_or_else(|| {
                    CommandError::dynamic("commands.stopwatch.does_not_exist", [text(id)])
                })?;
            pass(range.contains(elapsed))
        }
        "loaded" => {
            let Some(Value::Coordinates(pos)) = value(step, "pos") else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let states = read_blocks(ctx, dimension, &[pos.block(&pose)], false).await?;
            pass(states[0].is_some())
        }
        _ => return Err(CommandError::simple("command.unknown.command")),
    })
}

fn holder_input(step: &Step<'_>, name: &str) -> Result<ScoreHolderInput, CommandError> {
    match value(step, name) {
        Some(Value::ScoreHolder(input)) => Ok(input.clone()),
        _ => Err(CommandError::simple("command.unknown.argument")),
    }
}

fn objective_name(step: &Step<'_>, name: &str) -> Result<String, CommandError> {
    match value(step, name) {
        Some(Value::Str(name)) => Ok(name.clone()),
        _ => Err(CommandError::simple("command.unknown.argument")),
    }
}

/// `if score`: both scores must exist; `matches` tests one against a range.
async fn score_condition(ctx: &mut Context<'_>, step: &Step<'_>) -> Result<bool, CommandError> {
    let target = resolve_holders(ctx, holder_input(step, "target")?, false)
        .await?
        .remove(0);
    let target_objective = objective_named(ctx, objective_name(step, "targetObjective")?, false)
        .await?
        .name;
    let comparison = literal_at(step, 4).unwrap_or_default();
    if comparison == "matches" {
        let Some(Value::IntRange(range)) = value(step, "range") else {
            return Err(CommandError::simple("command.unknown.argument"));
        };
        let range = *range;
        let score = ctx
            .shared()
            .scoreboard
            .read(move |board| {
                board
                    .score(&target.name, &target_objective)
                    .map(|s| s.value)
            })
            .await
            .flatten();
        return Ok(score.is_some_and(|value| range.contains(value)));
    }
    let source = resolve_holders(ctx, holder_input(step, "source")?, false)
        .await?
        .remove(0);
    let source_objective = objective_named(ctx, objective_name(step, "sourceObjective")?, false)
        .await?
        .name;
    let scores = ctx
        .shared()
        .scoreboard
        .read(move |board| {
            Some((
                board.score(&target.name, &target_objective)?.value,
                board.score(&source.name, &source_objective)?.value,
            ))
        })
        .await
        .flatten();
    let Some((a, b)) = scores else {
        return Ok(false);
    };
    Ok(match comparison {
        "<" => a < b,
        "<=" => a <= b,
        "=" => a == b,
        ">" => a > b,
        ">=" => a >= b,
        _ => return Err(CommandError::simple("command.unknown.argument")),
    })
}

/// Where `execute store` puts a command's result or success.
#[derive(Clone)]
struct Store {
    result: bool,
    target: StoreTarget,
}

#[derive(Clone)]
enum StoreTarget {
    Score {
        holders: Vec<tuff_core::world::scoreboard::Holder>,
        objective: String,
    },
    BossBar {
        bars: crate::bossbar::BossBars,
        id: String,
        value: bool,
    },
    Nbt {
        accessor: Accessor,
        path: NbtPath,
        kind: NumberKind,
        scale: f64,
    },
}

async fn store(ctx: &mut Context<'_>, step: &Step<'_>) -> Result<Store, CommandError> {
    let result = literal_at(step, 1) == Some("result");
    let target = match literal_at(step, 2) {
        Some("score") => {
            let holders = resolve_holders(ctx, holder_input(step, "targets")?, true).await?;
            let objective = objective_named(ctx, objective_name(step, "objective")?, false)
                .await?
                .name;
            StoreTarget::Score { holders, objective }
        }
        Some("bossbar") => {
            let id = match value(step, "id") {
                Some(Value::Identifier(id)) => id.clone(),
                _ => String::new(),
            };
            let bars = crate::bossbar::bars(ctx)?;
            let exists = bars
                .with({
                    let id = id.clone();
                    move |bars| bars.get(&id).is_some()
                })
                .await
                .unwrap_or(false);
            if !exists {
                return Err(crate::bossbar::unknown(&id));
            }
            StoreTarget::BossBar {
                bars,
                id,
                value: literal_at(step, 4) == Some("value"),
            }
        }
        Some("block" | "entity" | "storage") => {
            let accessor = Accessor::from_value(ctx, value(step, "target").cloned()).await?;
            let Some(Value::NbtPath(path)) = value(step, "path") else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let kind = step
                .nodes
                .iter()
                .find_map(|n| NumberKind::from_name(n.node.name))
                .ok_or_else(|| CommandError::simple("command.unknown.argument"))?;
            let scale = match value(step, "scale") {
                Some(Value::Double(scale)) => *scale,
                _ => 1.0,
            };
            StoreTarget::Nbt {
                accessor,
                path: path.clone(),
                kind,
                scale,
            }
        }
        _ => return Err(CommandError::simple("command.unknown.command")),
    };
    Ok(Store { result, target })
}

/// Hands a finished command's outcome to the source's stores.
async fn apply_stores(ctx: &mut Context<'_>, stores: &[Store], success: bool, result: i32) {
    for store in stores {
        let number = if store.result {
            result
        } else {
            i32::from(success)
        };
        match &store.target {
            StoreTarget::Score { holders, objective } => {
                let holders = holders.clone();
                let objective = objective.clone();
                ctx.shared()
                    .scoreboard
                    .apply(move |board| {
                        for holder in &holders {
                            board.set(holder, &objective, number);
                        }
                    })
                    .await;
            }
            StoreTarget::BossBar { bars, id, value } => {
                let (id, value) = (id.clone(), *value);
                bars.with(move |bars| {
                    if let Some(bar) = bars.get_mut(&id) {
                        if value {
                            bar.set_value(number);
                        } else {
                            bar.set_max(number);
                        }
                    }
                })
                .await;
            }
            StoreTarget::Nbt {
                accessor,
                path,
                kind,
                scale,
            } => store_number(ctx, accessor, path, *kind, *scale, number).await,
        }
    }
}

/// The sources one modifier or condition step leads `source` to.
async fn modify(
    ctx: &mut Context<'_>,
    step: &Step<'_>,
    source: &Source,
) -> Result<Vec<Source>, CommandError> {
    let mut out = Vec::new();
    match step.literal {
        "as" => {
            let selector = selector(step, "targets")?.clone();
            ctx.source = source.clone();
            for target in targets::select_entities(ctx, &selector).await? {
                out.push(with_target(source, &target));
            }
        }
        "at" => {
            let selector = selector(step, "targets")?.clone();
            ctx.source = source.clone();
            for target in targets::select_entities(ctx, &selector).await? {
                out.push(at_target(source, &target));
            }
        }
        "positioned" => {
            if let Some(Value::Coordinates(pos)) = value(step, "pos") {
                let mut moved = source.clone();
                moved.position = pos.position(&source.pose());
                moved.anchor = Anchor::Feet;
                out.push(moved);
            } else if literal_at(step, 1) == Some("as") {
                let selector = selector(step, "targets")?.clone();
                ctx.source = source.clone();
                for target in targets::select_entities(ctx, &selector).await? {
                    let mut moved = source.clone();
                    moved.position = target.position();
                    out.push(moved);
                }
            } else if let Some(Value::Heightmap(name)) = value(step, "heightmap") {
                let kind = HeightmapKind::from_name(&name.to_ascii_uppercase())
                    .ok_or_else(|| CommandError::simple("command.unknown.argument"))?;
                let dimension = source.dimension;
                let (min_y, max_y) = {
                    let shape = &ctx.shared().dimension(dimension).shape;
                    (shape.min_y, shape.max_y())
                };
                let [x, _, z] = source.position;
                let (bx, bz) = (x.floor() as i32, z.floor() as i32);
                let column: Vec<[i32; 3]> = (min_y..=max_y).rev().map(|y| [bx, y, bz]).collect();
                ctx.source = source.clone();
                let states = read_blocks(ctx, dimension, &column, false).await?;
                if states.iter().any(Option::is_none) {
                    return Err(CommandError::simple("argument.pos.unloaded"));
                }
                let height = column_height(kind, bx, bz, min_y, max_y + 1, &|pos| {
                    states[(max_y - pos.y) as usize]
                });
                let mut moved = source.clone();
                moved.position = [x, f64::from(height), z];
                out.push(moved);
            } else {
                return Err(CommandError::simple("command.unknown.command"));
            }
        }
        "rotated" => {
            if let Some(Value::Rotation(rotation)) = value(step, "rot") {
                let mut turned = source.clone();
                turned.rotation = rotation.get(source.rotation);
                out.push(turned);
            } else if literal_at(step, 1) == Some("as") {
                let selector = selector(step, "targets")?.clone();
                ctx.source = source.clone();
                for target in targets::select_entities(ctx, &selector).await? {
                    let mut turned = source.clone();
                    turned.rotation = target.rotation();
                    out.push(turned);
                }
            } else {
                return Err(CommandError::simple("command.unknown.command"));
            }
        }
        "facing" => {
            if let Some(Value::Coordinates(pos)) = value(step, "pos") {
                out.push(facing(source, pos.position(&source.pose())));
            } else {
                let selector = selector(step, "targets")?.clone();
                let anchor = match value(step, "anchor") {
                    Some(Value::Anchor(anchor)) => *anchor,
                    _ => Anchor::Feet,
                };
                ctx.source = source.clone();
                for target in targets::select_entities(ctx, &selector).await? {
                    let point = match anchor {
                        Anchor::Feet => target.position(),
                        Anchor::Eyes => targets::eye_of(&target),
                    };
                    out.push(facing(source, point));
                }
            }
        }
        "align" => {
            let Some(Value::Swizzle(axes)) = value(step, "axes") else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let mut aligned = source.clone();
            for axis in 0..3 {
                if axes[axis] {
                    aligned.position[axis] = aligned.position[axis].floor();
                }
            }
            out.push(aligned);
        }
        "on" => {
            let Some(relation) = step.nodes.get(1).map(|node| node.node.name) else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let Some(id) = source.entity else {
                return Ok(out);
            };
            match relation {
                "leasher" => out.extend(leasher(ctx, source, id).await),
                "vehicle" => {
                    if let Some(vehicle) = super::ride::vehicle_of_id(ctx, id).await {
                        out.push(with_target(source, &vehicle));
                    }
                }
                "passengers" | "controller" | "owner" | "origin" | "attacker" | "target" => {
                    for holder in related(ctx, id, relation).await {
                        out.push(with_target(source, &holder));
                    }
                }
                _ => return Err(CommandError::simple("command.unknown.argument")),
            }
        }
        "anchored" => {
            let Some(Value::Anchor(anchor)) = value(step, "anchor") else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let mut anchored = source.clone();
            anchored.anchor = *anchor;
            out.push(anchored);
        }
        "in" => {
            let Some(Value::Identifier(key)) = value(step, "dimension") else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let Some(dimension) = ctx.shared().dimensions.by_key(key) else {
                return Err(CommandError::dynamic(
                    "argument.dimension.invalid",
                    [key.clone()],
                ));
            };
            let mut moved = source.clone();
            moved.dimension = dimension.id;
            out.push(moved);
        }
        "if" | "unless" if literal_at(step, 1) == Some("function") => {
            ctx.source = source.clone();
            let expected = step.literal == "if";
            if super::function::condition(ctx, source, step.nodes, expected).await? {
                out.push(source.clone());
            }
        }
        "if" | "unless" => {
            let expected = step.literal == "if";
            let count = match condition(ctx, source, step).await? {
                Condition::Pass(count) => count,
                Condition::Fail(count) => count,
            };
            if (expected && count > 0) || (!expected && count == 0) {
                out.push(source.clone());
            }
        }
        _ => return Err(CommandError::simple("command.unknown.command")),
    }
    Ok(out)
}

/// The sources `execute`'s subcommands lead to when the chain ends in
/// `run` and stores nothing, so a caller can run what follows `run` itself.
/// `None` for any other chain.
pub async fn sources_before_run(
    ctx: &mut Context<'_>,
    nodes: &[ParsedNode],
) -> Result<Option<Vec<Source>>, CommandError> {
    let steps = steps(nodes);
    let Some((last, modifiers)) = steps.split_last() else {
        return Ok(None);
    };
    if last.literal != "run" || modifiers.iter().any(|step| step.literal == "store") {
        return Ok(None);
    }
    let fork_limit = ForkLimit::of(ctx);
    let mut sources = vec![ctx.source.clone()];
    for step in modifiers {
        let mut next = Vec::new();
        for source in &sources {
            let out = modify(ctx, step, source).await?;
            fork_limit.check(next.len() + out.len())?;
            next.extend(out);
        }
        sources = next;
        if sources.is_empty() {
            break;
        }
    }
    Ok(Some(sources))
}

/// Every step but the last for every source, breadth first, carrying the
/// stores each source has gathered.
#[derive(Clone, Copy)]
struct ForkLimit(i32);

impl ForkLimit {
    fn of(ctx: &Context<'_>) -> Self {
        Self(ctx.shared().rule_int("max_command_forks"))
    }

    fn check(self, sources: usize) -> Result<(), CommandError> {
        if i64::try_from(sources).unwrap_or(i64::MAX) >= i64::from(self.0) {
            return Err(CommandError::dynamic("command.forkLimit", [text(self.0)]));
        }
        Ok(())
    }
}

async fn fan_out(
    ctx: &mut Context<'_>,
    steps: &[Step<'_>],
) -> Result<Vec<(Source, Vec<Store>)>, CommandError> {
    let mut sources = vec![(ctx.source.clone(), Vec::<Store>::new())];
    let fork_limit = ForkLimit::of(ctx);
    for step in steps {
        let mut next: Vec<(Source, Vec<Store>)> = Vec::new();
        for (source, stores) in &sources {
            if step.literal == "store" {
                ctx.source = source.clone();
                let target = store(ctx, step).await?;
                let mut kept = stores.clone();
                kept.push(target);
                next.push((source.clone(), kept));
            } else {
                let out = modify(ctx, step, source).await?;
                fork_limit.check(next.len() + out.len())?;
                next.extend(out.into_iter().map(|s| (s, stores.clone())));
            }
        }
        sources = next;
        if sources.is_empty() {
            break;
        }
    }
    Ok(sources)
}

/// A condition ending the chain: its result goes to the stores and the
/// source hears whether it passed.
async fn final_condition(
    ctx: &mut Context<'_>,
    step: &Step<'_>,
    sources: &[(Source, Vec<Store>)],
) -> Result<i32, CommandError> {
    let mut result = 0;
    for (source, stores) in sources {
        let expected = step.literal == "if";
        let numeric = matches!(
            literal_at(step, 1),
            Some("entity" | "blocks" | "data" | "items" | "slots")
        );
        let count = match condition(ctx, source, step).await? {
            Condition::Pass(count) => count,
            Condition::Fail(count) => count,
        };
        let passed = if expected { count > 0 } else { count == 0 };
        ctx.source = source.clone();
        let value = if expected && numeric { count } else { 1 };
        if passed {
            apply_stores(ctx, stores, true, value).await;
            let message = if expected && numeric {
                TextComponent::translate("commands.execute.conditional.pass_count", [text(count)])
            } else {
                TextComponent::translate("commands.execute.conditional.pass", [])
            };
            ctx.success(message, false).await;
            result += count.max(1);
        } else {
            apply_stores(ctx, stores, false, 0).await;
            if !expected && numeric {
                ctx.fail(TextComponent::translate(
                    "commands.execute.conditional.fail_count",
                    [text(count)],
                ));
            } else {
                ctx.fail(TextComponent::translate(
                    "commands.execute.conditional.fail",
                    [],
                ));
            }
        }
    }
    Ok(result)
}

/// Stores what one `run` gave and shows its error, if any.
async fn finish_run(
    ctx: &mut Context<'_>,
    source: &Source,
    stores: &[Store],
    ran: Result<i32, CommandError>,
) -> i32 {
    match ran {
        Ok(value) => {
            apply_stores(ctx, stores, true, value).await;
            value
        }
        Err(err) => {
            apply_stores(ctx, stores, false, 0).await;
            let feedback = source.feedback_target();
            for line in err.display(ctx.input) {
                feedback.send(line);
            }
            0
        }
    }
}

/// Notes whether what `run` ran was a function call that never returned,
/// which leaves the stores untouched.
struct CallWatch {
    temporary: bool,
}

fn watch_calls(ctx: &mut Context<'_>, stores: &[Store]) -> Option<CallWatch> {
    if stores.is_empty() {
        return None;
    }
    let temporary = ctx.frame.is_none();
    if temporary {
        ctx.frame = Some(Frame::outside(super::function::budget(ctx.shared())));
    }
    if let Some(frame) = &ctx.frame {
        frame.take_call();
    }
    Some(CallWatch { temporary })
}

impl CallWatch {
    fn ended_without_result(self, ctx: &mut Context<'_>, ran: &Result<i32, CommandError>) -> bool {
        let last = ctx.frame.as_ref().and_then(Frame::take_call);
        if self.temporary {
            ctx.frame = None;
        } else if let (Some(frame), Some(last)) = (&ctx.frame, last) {
            frame.record_call(last);
        }
        matches!(ran, Ok(0)) && matches!(last, Some(None | Some(Outcome::Ended)))
    }
}

type Boxed<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
type Forked = Vec<(Source, Vec<Store>)>;

// The heavy parts of `execute` are boxed from functions that are never
// inlined, so their futures are never laid out in the frame that polls
// `run` (which nests as deep as functions call through `execute`).
#[inline(never)]
fn fan_out_boxed<'a>(
    ctx: &'a mut Context<'_>,
    steps: &'a [Step<'a>],
) -> Boxed<'a, Result<Forked, CommandError>> {
    Box::pin(fan_out(ctx, steps))
}

#[inline(never)]
fn final_condition_boxed<'a>(
    ctx: &'a mut Context<'_>,
    step: &'a Step<'a>,
    sources: &'a [(Source, Vec<Store>)],
) -> Boxed<'a, Result<i32, CommandError>> {
    Box::pin(final_condition(ctx, step, sources))
}

#[inline(never)]
fn finish_run_boxed<'a>(
    ctx: &'a mut Context<'_>,
    source: &'a Source,
    stores: &'a [Store],
    ran: Result<i32, CommandError>,
) -> Boxed<'a, i32> {
    Box::pin(finish_run(ctx, source, stores, ran))
}

/// [`sources_before_run`], boxed outside the caller's frame.
#[inline(never)]
pub fn sources_before_run_boxed<'a>(
    ctx: &'a mut Context<'_>,
    nodes: &'a [ParsedNode],
) -> Boxed<'a, Result<Option<Vec<Source>>, CommandError>> {
    Box::pin(sources_before_run(ctx, nodes))
}

impl Executor for Execute {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let nodes = ctx.nodes;
            let steps = steps(nodes);
            let Some((last, modifiers)) = steps.split_last() else {
                return Err(CommandError::simple("command.unknown.command"));
            };
            let sources = fan_out_boxed(ctx, modifiers).await?;
            match last.literal {
                "run" => {
                    let mut result = 0i32;
                    for (source, stores) in &sources {
                        let outer = ctx.needs_result;
                        ctx.needs_result |= !stores.is_empty();
                        let watch = watch_calls(ctx, stores);
                        let ran = ctx.run_rest(source.clone()).await;
                        ctx.needs_result = outer;
                        let value =
                            if watch.is_some_and(|watch| watch.ended_without_result(ctx, &ran)) {
                                0
                            } else {
                                finish_run_boxed(ctx, source, stores, ran).await
                            };
                        result = result.wrapping_add(value);
                        if ctx.returned() {
                            break;
                        }
                    }
                    Ok(result)
                }
                "if" | "unless" if literal_at(last, 1) != Some("function") => {
                    final_condition_boxed(ctx, last, &sources).await
                }
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }
}
