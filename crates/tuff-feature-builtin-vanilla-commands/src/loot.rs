//! `/loot <destination> <source>`: rolls a loot table, a block, an entity or
//! a fishing catch and hands the drops to inventories, containers or the
//! world.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::world::drops::EntityFacts;
use tuff_core::world::messages::{Directive, RegionRequest, SlotFill};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_vanilla_data::Item;
use tuff_world_model::pos::ChunkPos;

use super::data::{find_entity, player_data};
use super::item::{Holder, container, has_slot, read_slot, write_slot};
use super::players::item_name;
use super::targets::{self, Target};
use super::world::{copy_blocks, paste_blocks};
use super::{Context, Executor, executor_future, text};

pub struct Loot;

struct Drops {
    items: Vec<ItemStack>,
    table: Option<String>,
}

fn fail_args() -> CommandError {
    CommandError::simple("command.unknown.argument")
}

fn table_name(ctx: &Context<'_>) -> Result<String, CommandError> {
    let Some(Value::Identifier(name)) = ctx.arg("loot_table") else {
        return Err(fail_args());
    };
    let name = if name.contains(':') {
        name.clone()
    } else {
        format!("minecraft:{name}")
    };
    if ctx.shared().loot_tables().get(&name).is_none() {
        return Err(CommandError::dynamic("argument.id.unknown", [text(&name)]));
    }
    Ok(name)
}

async fn held_item(ctx: &mut Context<'_>, slot: &str) -> Result<ItemStack, CommandError> {
    let ids = tuff_core::world::predicate::slot_range(slot).unwrap_or_default();
    let no_items = |ctx: &Context<'_>| {
        CommandError::dynamic("commands.drop.no_held_items", [ctx.source.display_name()])
    };
    let (data, holder) = match targets::source_player(ctx).await {
        Ok(player) => (player_data(ctx, &player), Holder::Player),
        Err(_) => {
            let Some(id) = ctx.source.entity else {
                return Err(CommandError::simple("permissions.requires.entity"));
            };
            let Some((_, data)) = find_entity(ctx, id).await else {
                return Err(no_items(ctx));
            };
            (data, Holder::Mob)
        }
    };
    ids.iter()
        .find_map(|id| read_slot(&data, *id, holder))
        .map(|stack| stack.unwrap_or_else(|| ItemStack::new(Item::Air, 0)))
        .ok_or_else(|| no_items(ctx))
}

async fn tool(ctx: &mut Context<'_>) -> Result<ItemStack, CommandError> {
    let literals = ctx.literals();
    match () {
        () if literals.contains(&"mainhand") => held_item(ctx, "weapon.mainhand").await,
        () if literals.contains(&"offhand") => held_item(ctx, "weapon.offhand").await,
        () => match ctx.arg("tool") {
            Some(Value::ItemStack(input)) => Ok(input.stack(1)),
            _ => Ok(ItemStack::new(Item::Air, 0)),
        },
    }
}

async fn drops(ctx: &mut Context<'_>, source: &str) -> Result<Drops, CommandError> {
    let mut rng = StdRng::from_entropy();
    let loot = ctx.shared().block_loot();
    match source {
        "loot" => {
            let name = table_name(ctx)?;
            Ok(Drops {
                items: loot.roll(&name, &mut rng),
                table: None,
            })
        }
        "fish" => {
            let name = table_name(ctx)?;
            let tool = tool(ctx).await?;
            let items = if name == "minecraft:gameplay/fishing" {
                loot.fishing(0.0, false, &tool, &mut rng)
            } else {
                loot.roll(&name, &mut rng)
            };
            Ok(Drops { items, table: None })
        }
        "kill" => {
            let Some(Value::Selector(selector)) = ctx.arg("target").cloned() else {
                return Err(CommandError::simple("argument.entity.notfound.entity"));
            };
            let target = targets::select_entity(ctx, &selector).await?;
            let kind = match &target {
                Target::Entity(entity) => entity.kind,
                Target::Player(_) => tuff_vanilla_data::registry::EntityType::Player,
            };
            let Some(table) = kind.mob().loot_table else {
                return Err(CommandError::dynamic(
                    "commands.drop.no_loot_table.entity",
                    [target.display_name()],
                ));
            };
            let killer = targets::source_player(ctx).await.ok();
            let facts = EntityFacts {
                kind: Some(kind),
                killed_by_player: killer.is_some(),
                player_name: killer.as_ref().map(|p| p.name.clone()),
                damage_kind: Some(&tuff_vanilla_data::damage_type::MAGIC),
                damage_direct: true,
                tables: Some(ctx.shared().loot_tables()),
                ..EntityFacts::default()
            };
            Ok(Drops {
                items: loot.entity_drops(table, &facts, &mut rng),
                table: Some(table.to_owned()),
            })
        }
        "mine" => {
            let Some(Value::Coordinates(coordinates)) = ctx.arg("pos").cloned() else {
                return Err(fail_args());
            };
            let pos = coordinates.block(&ctx.source.pose());
            let tool = tool(ctx).await?;
            let dimension = ctx.source.dimension;
            let copied = copy_blocks(ctx, dimension, &[pos]).await?;
            let Some((state, data)) = copied.into_iter().next().flatten() else {
                return Err(CommandError::simple("argument.pos.unloaded"));
            };
            let block = state.block();
            let Some(table) = block.loot_table() else {
                let name = block.name();
                let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
                return Err(CommandError::dynamic(
                    "commands.drop.no_loot_table.block",
                    [TextComponent::translate(
                        format!("block.{namespace}.{path}"),
                        [],
                    )],
                ));
            };
            let items = if tool.count == 0 {
                loot.destroyed(state, data.as_ref(), &mut rng)
            } else {
                let mut items = loot.drops(state, Some(&tool), data.as_ref(), &mut rng);
                if items.is_empty() && !loot.can_harvest(state, Some(&tool)) {
                    items = loot.table(table, state, Some(tool.item), &mut rng);
                }
                items
            };
            Ok(Drops {
                items,
                table: Some(table.to_owned()),
            })
        }
        _ => Err(fail_args()),
    }
}

fn empty() -> ItemStack {
    ItemStack::new(Item::Air, 0)
}

async fn replace_entities(
    ctx: &mut Context<'_>,
    items: &[ItemStack],
    tracked: &mut Vec<ItemStack>,
) -> Result<(), CommandError> {
    let Some(Value::Selector(selector)) = ctx.arg("entities").cloned() else {
        return Err(CommandError::simple("argument.entity.notfound.entity"));
    };
    let Some(Value::Slots { name, ids }) = ctx.arg("slot").cloned() else {
        return Err(fail_args());
    };
    let count = match ctx.arg("count") {
        Some(Value::Int(count)) => *count as usize,
        _ => items.len(),
    };
    let start = ids.first().copied().unwrap_or(0);
    let found = targets::select_entities(ctx, &selector).await?;
    for target in &found {
        for index in 0..count {
            let stack = items.get(index).cloned().unwrap_or_else(empty);
            let slot = start + index as i32;
            match target {
                Target::Player(player) => {
                    let replies = targets::dispatch_wait(ctx, std::slice::from_ref(player), |_| {
                        Directive::SetSlots {
                            ids: vec![slot],
                            name: name.clone(),
                            stack: stack.clone(),
                            mode: SlotFill::Replace,
                        }
                    })
                    .await;
                    if replies.iter().any(|reply| reply.affected) {
                        tracked.push(stack);
                    }
                }
                Target::Entity(entity) => {
                    let Some((region, mut data)) = find_entity(ctx, entity.id).await else {
                        continue;
                    };
                    if !has_slot(slot, Holder::Mob, &data) {
                        continue;
                    }
                    write_slot(
                        &mut data,
                        slot,
                        Holder::Mob,
                        (stack.count > 0).then(|| stack.clone()),
                    );
                    let id = entity.id;
                    let _ = region
                        .ask(move |world| world.replace_entity(id, data))
                        .await;
                    tracked.push(stack);
                }
            }
        }
    }
    Ok(())
}

fn block_pos(ctx: &Context<'_>) -> Result<[i32; 3], CommandError> {
    match ctx.arg("targetPos") {
        Some(Value::Coordinates(coordinates)) => Ok(coordinates.block(&ctx.source.pose())),
        _ => Err(fail_args()),
    }
}

async fn replace_block(
    ctx: &mut Context<'_>,
    items: &[ItemStack],
    tracked: &mut Vec<ItemStack>,
) -> Result<(), CommandError> {
    let pos = block_pos(ctx)?;
    let Some(Value::Slots { name, ids }) = ctx.arg("slot").cloned() else {
        return Err(fail_args());
    };
    let count = match ctx.arg("count") {
        Some(Value::Int(count)) => *count as usize,
        _ => items.len(),
    };
    let (state, mut data, holder) =
        container(ctx, pos, "commands.item.target.not_a_container").await?;
    let start = ids.first().copied().unwrap_or(0);
    if !has_slot(start, holder, &data) {
        return Err(CommandError::dynamic(
            "commands.item.target.no_such_slot",
            [text(&name)],
        ));
    }
    for index in 0..count {
        let slot = start + index as i32;
        if !has_slot(slot, holder, &data) {
            continue;
        }
        let stack = items.get(index).cloned().unwrap_or_else(empty);
        write_slot(
            &mut data,
            slot,
            holder,
            (stack.count > 0).then(|| stack.clone()),
        );
        tracked.push(stack);
    }
    let dimension = ctx.source.dimension;
    paste_blocks(ctx, dimension, &[(pos, state, Some(data))], true).await?;
    Ok(())
}

fn distribute(data: &mut Compound, holder: Holder, size: i32, mut stack: ItemStack) -> bool {
    let mut changed = false;
    for slot in 0..size {
        if stack.count == 0 {
            break;
        }
        let Some(current) = read_slot(data, slot, holder) else {
            continue;
        };
        match current {
            None => {
                write_slot(data, slot, holder, Some(stack.clone()));
                return true;
            }
            Some(mut current) if current.stacks_with(&stack) => {
                let space = current.max_stack_size().saturating_sub(current.count);
                let moved = stack.count.min(space);
                if moved > 0 {
                    stack.count -= moved;
                    current.count += moved;
                    write_slot(data, slot, holder, Some(current));
                    changed = true;
                }
            }
            Some(_) => {}
        }
    }
    changed
}

async fn insert_block(
    ctx: &mut Context<'_>,
    items: &[ItemStack],
    tracked: &mut Vec<ItemStack>,
) -> Result<(), CommandError> {
    let pos = block_pos(ctx)?;
    let (state, mut data, holder) =
        container(ctx, pos, "commands.item.target.not_a_container").await?;
    let size = match holder {
        Holder::Container(size, _) => size,
        _ => 0,
    };
    for stack in items {
        if distribute(&mut data, holder, size, stack.clone()) {
            tracked.push(stack.clone());
        }
    }
    let dimension = ctx.source.dimension;
    paste_blocks(ctx, dimension, &[(pos, state, Some(data))], true).await?;
    Ok(())
}

async fn give_players(
    ctx: &mut Context<'_>,
    items: &[ItemStack],
    tracked: &mut Vec<ItemStack>,
) -> Result<(), CommandError> {
    let Some(Value::Selector(selector)) = ctx.arg("players").cloned() else {
        return Err(CommandError::simple("argument.entity.notfound.player"));
    };
    let players = targets::select_players(ctx, &selector).await?;
    for stack in items {
        let count = i32::from(stack.count);
        let one = stack.clone().with_count(1);
        let replies = targets::dispatch_wait(ctx, &players, |_| Directive::Give {
            stack: one.clone(),
            count,
        })
        .await;
        for reply in replies {
            if reply.affected {
                tracked.push(stack.clone());
            }
        }
    }
    Ok(())
}

async fn spawn_items(
    ctx: &mut Context<'_>,
    items: &[ItemStack],
    tracked: &mut Vec<ItemStack>,
) -> Result<(), CommandError> {
    let position = match ctx.arg("targetPos") {
        Some(Value::Coordinates(coordinates)) => coordinates.position(&ctx.source.pose()),
        _ => return Err(fail_args()),
    };
    let dimension = ctx.source.dimension;
    let Some(region) = ctx
        .actor
        .region(dimension, ChunkPos::of_position(position).region())
        .await
    else {
        return Err(CommandError::simple("commands.summon.failed"));
    };
    let mut rng = StdRng::from_entropy();
    for stack in items {
        let velocity = [rng.gen_range(-0.1..0.1), 0.2, rng.gen_range(-0.1..0.1)];
        region
            .request(RegionRequest::SpawnItem {
                stack: stack.clone(),
                position,
                velocity,
                pickup_delay: 10,
            })
            .await;
        tracked.push(stack.clone());
    }
    Ok(())
}

impl Executor for Loot {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            let destination = literals.get(1).copied().unwrap_or_default();
            let source = literals
                .iter()
                .skip(1)
                .copied()
                .find(|literal| matches!(*literal, "fish" | "loot" | "kill" | "mine"))
                .unwrap_or_default();
            let Drops { items, table } = drops(ctx, source).await?;
            let mut tracked = Vec::new();
            match (destination, literals.contains(&"entity")) {
                ("replace", true) => replace_entities(ctx, &items, &mut tracked).await?,
                ("replace", _) => replace_block(ctx, &items, &mut tracked).await?,
                ("insert", _) => insert_block(ctx, &items, &mut tracked).await?,
                ("give", _) => give_players(ctx, &items, &mut tracked).await?,
                ("spawn", _) => spawn_items(ctx, &items, &mut tracked).await?,
                _ => return Err(fail_args()),
            }
            let message = match (tracked.as_slice(), table) {
                ([only], Some(table)) => TextComponent::translate(
                    "commands.drop.success.single_with_table",
                    [
                        text(only.count),
                        item_name(only.item, &only.components),
                        text(table),
                    ],
                ),
                (many, Some(table)) => TextComponent::translate(
                    "commands.drop.success.multiple_with_table",
                    [text(many.len()), text(table)],
                ),
                ([only], None) => TextComponent::translate(
                    "commands.drop.success.single",
                    [text(only.count), item_name(only.item, &only.components)],
                ),
                (many, None) => {
                    TextComponent::translate("commands.drop.success.multiple", [text(many.len())])
                }
            };
            ctx.success(message, false).await;
            Ok(tracked.len() as i32)
        })
    }
}
