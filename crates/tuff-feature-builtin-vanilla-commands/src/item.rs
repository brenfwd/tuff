//! `/item replace|fill|override block|entity <target> <slots>` with an item or
//! `from block|entity <source> <slots>`, on players, mobs and containers.
//! `modify` and `from ... <modifier>` run item modifiers: stored `item_modifier`
//! files or inline JSON-like functions.

use std::sync::Arc;
use tuff_item::item::ItemSnapshot;

use rand::SeedableRng;
use rand::rngs::StdRng;
use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::world::block_entity;
use tuff_core::world::messages::PlayerSnapshot;
use tuff_core::world::messages::{Directive, SlotFill};
use tuff_core::world::predicate::slot_range;
use tuff_datapack::slot_source::{SlotSource, SlotSourceRef, SlotSources};
use tuff_item::item::{ItemPicture, ItemStack, MintReason};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::loot_table::LootFunction;
use tuff_vanilla_data::registry::BlockEntityType;

use super::data::{Accessor, find_entity, player_data};
use super::feedback::Messages;
use super::players::item_name;
use super::targets::{self, Target};
use super::world::{copy_blocks, paste_blocks, replace_port_item};
use super::{Context, Executor, executor_future, text};

#[derive(Clone, Copy)]
pub(super) enum Holder {
    Container(i32, Option<&'static str>),
    Player,
    Mob,
}

/// The slot count of a block entity that holds items, and the key of its
/// lone item compound when it does not keep an `Items` list.
fn container_layout(nbt: &Compound) -> Option<(i32, Option<&'static str>)> {
    let kind = block_entity::raw::kind(nbt)?;
    let size = i32::try_from(kind.container_size()?).ok()?;
    let single = match kind {
        BlockEntityType::Jukebox => Some("RecordItem"),
        BlockEntityType::DecoratedPot => Some("item"),
        _ => None,
    };
    Some((size, single))
}

fn equipment_key(id: i32, holder: Holder) -> Option<&'static str> {
    Some(match (id, holder) {
        (98, Holder::Mob) => "mainhand",
        (99, _) => "offhand",
        (100, _) => "feet",
        (101, _) => "legs",
        (102, _) => "chest",
        (103, _) => "head",
        (105, Holder::Mob) => "body",
        (106, Holder::Mob) => "saddle",
        _ => return None,
    })
}

fn list_key(id: i32, holder: Holder, nbt: &Compound) -> Option<(&'static str, i8)> {
    match holder {
        Holder::Container(size, None) => (0..size).contains(&id).then_some(("Items", id as i8)),
        Holder::Container(_, Some(_)) => None,
        Holder::Player if (0..=35).contains(&id) => Some(("Inventory", id as i8)),
        Holder::Player if id == 98 => Some((
            "Inventory",
            nbt.get_int("SelectedItemSlot").unwrap_or(0) as i8,
        )),
        Holder::Player if (200..=226).contains(&id) => Some(("EnderItems", (id - 200) as i8)),
        Holder::Mob if (300..=307).contains(&id) => Some(("Inventory", (id - 300) as i8)),
        Holder::Mob if (500..=514).contains(&id) => Some(("Items", (id - 500) as i8)),
        _ => None,
    }
}

/// The stored form of slot `id`: `None` when the holder has no such
/// slot, `Some(None)` when it is empty.
fn slot_nbt(nbt: &Compound, id: i32, holder: Holder) -> Option<Option<&Compound>> {
    if let Holder::Container(_, Some(key)) = holder {
        return (id == 0).then(|| nbt.get_compound(key));
    }
    if let Some(key) = equipment_key(id, holder) {
        return Some(
            nbt.get_compound("equipment")
                .and_then(|equipment| equipment.get_compound(key)),
        );
    }
    let (list, slot) = list_key(id, holder, nbt)?;
    Some(
        nbt.get_list(list)
            .unwrap_or(&[])
            .iter()
            .filter_map(Tag::as_compound)
            .find(|entry| entry.get_byte("Slot") == Some(slot)),
    )
}

/// The stack in slot `id`, loaded to be changed and written back with
/// [`write_slot`] (or released). `None` when the holder has no such slot.
pub(super) fn read_slot(nbt: &Compound, id: i32, holder: Holder) -> Option<Option<ItemStack>> {
    Some(slot_nbt(nbt, id, holder)?.and_then(ItemStack::from_storage))
}

/// A picture of the stack in slot `id`, which stays stored. `None` when
/// the holder has no such slot.
pub(super) fn peek_slot(nbt: &Compound, id: i32, holder: Holder) -> Option<Option<ItemPicture>> {
    Some(slot_nbt(nbt, id, holder)?.and_then(ItemPicture::from_nbt))
}

pub(super) fn write_slot(nbt: &mut Compound, id: i32, holder: Holder, value: Option<ItemStack>) {
    tuff_item::item::conservation::releasing(|| write_slot_nbt(nbt, id, holder, value));
}

/// The stack is kept as the holder's saved NBT.
fn write_slot_nbt(nbt: &mut Compound, id: i32, holder: Holder, value: Option<ItemStack>) {
    if let Holder::Container(_, Some(key)) = holder {
        if id == 0 {
            match value {
                Some(stack) => nbt.insert(key, stack.to_storage()),
                None => nbt.remove(key),
            };
        }
        return;
    }
    if let Some(key) = equipment_key(id, holder) {
        let mut equipment = nbt.get_compound("equipment").cloned().unwrap_or_default();
        match value {
            Some(stack) => equipment.insert(key, stack.to_storage()),
            None => equipment.remove(key),
        };
        nbt.insert("equipment", equipment);
        return;
    }
    let Some((list, slot)) = list_key(id, holder, nbt) else {
        return;
    };
    let mut entries = nbt.get_list(list).map(<[Tag]>::to_vec).unwrap_or_default();
    entries.retain(|entry| entry.as_compound().and_then(|e| e.get_byte("Slot")) != Some(slot));
    if let Some(stack) = value {
        entries.push(Tag::Compound(
            stack.to_storage().with("Slot", Tag::Byte(slot)),
        ));
    }
    nbt.insert(list, Tag::List(entries));
}

pub(super) fn has_slot(id: i32, holder: Holder, nbt: &Compound) -> bool {
    matches!(holder, Holder::Container(_, Some(_))) && id == 0
        || equipment_key(id, holder).is_some()
        || list_key(id, holder, nbt).is_some()
}

pub(super) async fn container(
    ctx: &mut Context<'_>,
    pos: [i32; 3],
    error: &'static str,
) -> Result<(tuff_vanilla_data::BlockState, Compound, Holder), CommandError> {
    let fail = || CommandError::dynamic(error, [text(pos[0]), text(pos[1]), text(pos[2])]);
    let dimension = ctx.source.dimension;
    let copied = copy_blocks(ctx, dimension, &[pos]).await?;
    let Some((state, Some(data))) = copied.into_iter().next().flatten() else {
        return Err(fail());
    };
    let (size, single) = container_layout(&data).ok_or_else(fail)?;
    Ok((state, data, Holder::Container(size, single)))
}

pub struct ItemCommand;

const BOOLEAN_KEYS: [&str; 3] = ["add", "only_compatible", "expand"];

fn booleans(value: &mut serde_json::Value, depth: usize) {
    if depth > 32 {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if BOOLEAN_KEYS.contains(&key.as_str())
                    && let Some(flag) = entry.as_i64().filter(|n| (0..=1).contains(n))
                {
                    *entry = serde_json::Value::Bool(flag == 1);
                } else {
                    booleans(entry, depth + 1);
                }
            }
        }
        serde_json::Value::Array(list) => list.iter_mut().for_each(|e| booleans(e, depth + 1)),
        _ => {}
    }
}

fn modifier(ctx: &Context<'_>) -> Result<Arc<Vec<LootFunction>>, CommandError> {
    let contents = ctx.shared().datapacks.contents();
    match ctx.arg("modifier") {
        Some(Value::Identifier(id)) => contents.item_modifiers.get(id).ok_or_else(|| {
            CommandError::dynamic(
                "argument.resource_or_id.no_such_element",
                [id.clone(), "minecraft:item_modifier".to_owned()],
            )
        }),
        Some(Value::Nbt(inline)) => {
            let mut json = super::predicates::json_of(inline);
            booleans(&mut json, 0);
            tuff_datapack::loot::parse_item_modifier(&json, &contents.predicates)
                .map(Arc::new)
                .map_err(|err| {
                    CommandError::dynamic("argument.resource_or_id.failed_to_parse", [err])
                })
        }
        _ => Err(fail_args()),
    }
}

fn apply(
    ctx: &Context<'_>,
    functions: &[LootFunction],
    stack: ItemStack,
    rng: &mut StdRng,
) -> ItemStack {
    ctx.shared().block_loot().modify_item(functions, stack, rng)
}

async fn modify_items(ctx: &mut Context<'_>, is_block: bool) -> Result<i32, CommandError> {
    let slot = SlotArg::from_value(ctx, ctx.arg("slots"))?;
    let functions = modifier(ctx)?;
    let mut rng = StdRng::from_entropy();
    if is_block {
        let Some(Value::Coordinates(coordinates)) = ctx.arg("target").cloned() else {
            return Err(fail_args());
        };
        let pos = coordinates.block(&ctx.source.pose());
        let (state, mut data, holder) =
            container(ctx, pos, "commands.item.target.not_a_container").await?;
        let mut selected = 0;
        let valid: Vec<i32> = slot
            .ids(&data, holder)
            .iter()
            .copied()
            .filter(|id| has_slot(*id, holder, &data))
            .collect();
        for id in valid {
            let Some(stack) = read_slot(&data, id, holder) else {
                continue;
            };
            let stack = stack.unwrap_or_else(empty_stack);
            selected += 1;
            let result = apply(ctx, &functions, stack, &mut rng);
            write_slot(
                &mut data,
                id,
                holder,
                (result.count() > 0).then_some(result),
            );
        }
        if selected == 0 {
            return Err(slot.missing_error("target"));
        }
        store_block(ctx, pos, state, data, holder).await?;
        let [x, y, z] = pos_args(pos);
        ctx.success(
            TextComponent::translate(
                "commands.item.block.modify.success",
                [text(selected), x, y, z],
            ),
            true,
        )
        .await;
        return Ok(selected);
    }
    let Some(Value::Selector(selector)) = ctx.arg("target").cloned() else {
        return Err(CommandError::simple("argument.entity.notfound.entity"));
    };
    let found = targets::select_entities(ctx, &selector).await?;
    let mut players = Vec::new();
    let mut mob_slots = 0;
    let mut mob_count = 0;
    let mut last_mob = String::new();
    for target in &found {
        match target {
            Target::Player(player) => players.push(player.clone()),
            Target::Entity(entity) => {
                let Some((region, mut data)) = find_entity(ctx, entity.id).await else {
                    continue;
                };
                let mut changed = 0;
                let valid: Vec<i32> = slot
                    .ids(&data, Holder::Mob)
                    .iter()
                    .copied()
                    .filter(|id| has_slot(*id, Holder::Mob, &data))
                    .collect();
                for id in valid {
                    let Some(stack) = read_slot(&data, id, Holder::Mob) else {
                        continue;
                    };
                    let stack = stack.unwrap_or_else(empty_stack);
                    let result = apply(ctx, &functions, stack, &mut rng);
                    write_slot(
                        &mut data,
                        id,
                        Holder::Mob,
                        (result.count() > 0).then_some(result),
                    );
                    changed += 1;
                }
                if changed == 0 {
                    continue;
                }
                let id = entity.id;
                let _ = region
                    .ask(move |world| world.replace_entity(id, data))
                    .await;
                mob_slots += changed;
                mob_count += 1;
                last_mob = entity.name.clone();
            }
        }
    }
    if players.is_empty() {
        if mob_count == 0 {
            return Err(slot.missing_error("target"));
        }
        let message = if mob_count == 1 {
            TextComponent::translate(
                "commands.item.entity.modify.success.single",
                [text(mob_slots), text(last_mob)],
            )
        } else {
            TextComponent::translate(
                "commands.item.entity.modify.success.multiple",
                [text(mob_count)],
            )
        };
        ctx.success(message, true).await;
        return Ok(mob_count);
    }
    let missing = slot.missing("target");
    let player_ids = slot.player_ids(ctx, &players);
    let name = slot.label();
    let messages = Messages::new(
        |reply| {
            TextComponent::translate(
                "commands.item.entity.modify.success.single",
                [text(reply.value), text(&reply.name)],
            )
        },
        |players, _| {
            TextComponent::translate(
                "commands.item.entity.modify.success.multiple",
                [text(players)],
            )
        },
    )
    .on_zero(move |replies| {
        if replies.iter().any(|r| r.affected) {
            TextComponent::translate("commands.item.target.failed", [])
        } else {
            missing.clone()
        }
    });
    targets::dispatch(
        ctx,
        &players,
        |player| Directive::ModifySlots {
            ids: ids_for(&players, &player_ids, player),
            name: name.clone(),
            functions: functions.clone(),
        },
        messages,
    )
    .await;
    Ok(1)
}

/// Writes back a container block's edited data; a jukebox instead swaps
/// its record so the block state and song follow.
async fn store_block(
    ctx: &mut Context<'_>,
    pos: [i32; 3],
    state: tuff_vanilla_data::BlockState,
    data: Compound,
    holder: Holder,
) -> Result<(), CommandError> {
    let dimension = ctx.source.dimension;
    if state.block() == tuff_vanilla_data::Block::Jukebox {
        let record = read_slot(&data, 0, holder).flatten();
        if replace_port_item(ctx, dimension, pos, record).await? {
            return Ok(());
        }
    }
    paste_blocks(ctx, dimension, &[(pos, state, Some(data))], true).await?;
    Ok(())
}

fn empty_stack() -> ItemStack {
    ItemStack::empty()
}

fn fail_args() -> CommandError {
    CommandError::simple("command.unknown.argument")
}

fn pos_args(pos: [i32; 3]) -> [TextComponent; 3] {
    [text(pos[0]), text(pos[1]), text(pos[2])]
}

async fn source_stack(ctx: &mut Context<'_>, slot: &SlotArg) -> Result<ItemPicture, CommandError> {
    let accessor = Accessor::resolve(ctx, "source").await?;
    let (data, holder) = source_data(ctx, &accessor).await?;
    let mut found = None;
    for id in slot.ids(&data, holder) {
        if let Some(stack) = peek_slot(&data, id, holder) {
            found = Some(stack);
            break;
        }
    }
    found
        .map(|stack| stack.unwrap_or_else(|| ItemPicture::new(Item::Air, 0)))
        .ok_or_else(|| slot.missing_error("source"))
}

/// The contents of the existing slots among `ids` in `accessor`'s container
/// or entity; empty slots read as `None`.
pub(super) async fn existing_slots(
    ctx: &mut Context<'_>,
    accessor: &Accessor,
    slot: &SlotArg,
) -> Result<Vec<Option<ItemPicture>>, CommandError> {
    let (data, holder) = source_data(ctx, accessor).await?;
    Ok(slot
        .ids(&data, holder)
        .into_iter()
        .filter_map(|id| peek_slot(&data, id, holder))
        .collect())
}

async fn source_data(
    ctx: &mut Context<'_>,
    accessor: &Accessor,
) -> Result<(Compound, Holder), CommandError> {
    Ok(match accessor {
        Accessor::Block(pos) => {
            let (_, data, holder) =
                container(ctx, *pos, "commands.item.source.not_a_container").await?;
            (data, holder)
        }
        Accessor::Entity(target) => match &**target {
            Target::Player(player) => (player_data(ctx, player), Holder::Player),
            Target::Entity(_) => (accessor.read(ctx).await?, Holder::Mob),
        },
        Accessor::Storage(_) => return Err(fail_args()),
    })
}

fn spread<M: tuff_item::item::Mode>(
    mode: SlotFill,
    ids: &[i32],
    stack: &ItemStack<M>,
) -> Vec<(i32, Option<ItemStack>)> {
    let value = || {
        (stack.count() > 0).then(|| ItemStack::mint_like(stack, stack.count(), MintReason::Command))
    };
    ids.iter()
        .enumerate()
        .filter_map(|(index, id)| match mode {
            SlotFill::Fill => Some((*id, value())),
            SlotFill::Replace => (index == 0).then(|| (*id, value())),
            SlotFill::Override => Some((*id, if index == 0 { value() } else { None })),
        })
        .collect()
}

impl Executor for ItemCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            if literals.contains(&"modify") {
                return modify_items(ctx, literals.contains(&"block")).await;
            }
            let mode = if literals.contains(&"fill") {
                SlotFill::Fill
            } else if literals.contains(&"override") {
                SlotFill::Override
            } else if literals.contains(&"replace") {
                SlotFill::Replace
            } else {
                return Err(fail_args());
            };
            let slot = SlotArg::from_value(ctx, ctx.arg("slots"))?;
            let is_block = literals.contains(&"block");
            let stack: ItemSnapshot = if literals.contains(&"from") {
                let source_slot = SlotArg::from_value(ctx, ctx.arg("sourceSlots"))?;
                let source = source_stack(ctx, &source_slot).await?;
                if ctx.arg("modifier").is_some() {
                    let functions = modifier(ctx)?;
                    let working =
                        ItemStack::mint_like(&source, source.count(), MintReason::Command);
                    apply(ctx, &functions, working, &mut StdRng::from_entropy())
                        .into_picture()
                        .into()
                } else {
                    source.into()
                }
            } else {
                let Some(Value::ItemStack(input)) = ctx.arg("item").cloned() else {
                    return Err(fail_args());
                };
                let count = match ctx.arg("count") {
                    Some(Value::Int(count)) => *count as u8,
                    _ => 1,
                };
                input.template(count)
            };
            let item = item_name(stack.item(), &stack.components);
            if is_block {
                return set_block(ctx, mode, &slot, &stack, item).await;
            }
            let Some(Value::Selector(selector)) = ctx.arg("target").cloned() else {
                return Err(CommandError::simple("argument.entity.notfound.entity"));
            };
            let found = targets::select_entities(ctx, &selector).await?;
            let mut mobs = Vec::new();
            let mut players = Vec::new();
            for target in &found {
                match target {
                    Target::Player(player) => players.push(player.clone()),
                    Target::Entity(entity) => mobs.push(entity.clone()),
                }
            }
            let mut mob_count = 0;
            for entity in &mobs {
                let Some((region, mut data)) = find_entity(ctx, entity.id).await else {
                    continue;
                };
                let writes: Vec<_> = spread(
                    mode,
                    &slot
                        .ids(&data, Holder::Mob)
                        .iter()
                        .copied()
                        .filter(|id| has_slot(*id, Holder::Mob, &data))
                        .collect::<Vec<_>>(),
                    &stack,
                );
                if writes.is_empty() {
                    continue;
                }
                for (id, value) in writes {
                    write_slot(&mut data, id, Holder::Mob, value);
                }
                let id = entity.id;
                let _ = region
                    .ask(move |world| world.replace_entity(id, data))
                    .await;
                mob_count += 1;
            }
            if players.is_empty() {
                if mob_count == 0 {
                    return Err(slot.missing_error("target"));
                }
                let message = if mob_count == 1 {
                    let who = mobs.first().map(|e| e.name.clone()).unwrap_or_default();
                    TextComponent::translate(
                        "commands.item.entity.set.success.single",
                        [text(who), item],
                    )
                } else {
                    TextComponent::translate(
                        "commands.item.entity.set.success.multiple",
                        [text(mob_count), item],
                    )
                };
                ctx.success(message, true).await;
                return Ok(mob_count);
            }
            let missing = slot.missing("target");
            let player_ids = slot.player_ids(ctx, &players);
            let name = slot.label();
            let single_item = item.clone();
            let known = item_name(stack.item(), &stack.components);
            let messages = Messages::new(
                move |reply| {
                    TextComponent::translate(
                        "commands.item.entity.set.success.single",
                        [text(&reply.name), single_item.clone()],
                    )
                },
                move |players, _| {
                    TextComponent::translate(
                        "commands.item.entity.set.success.multiple",
                        [text(players), item.clone()],
                    )
                },
            )
            .on_zero(move |replies| {
                if replies.iter().any(|r| r.affected) {
                    TextComponent::translate(
                        "commands.item.target.failed.known_item",
                        [known.clone()],
                    )
                } else {
                    missing.clone()
                }
            });
            targets::dispatch(
                ctx,
                &players,
                |player| Directive::SetSlots {
                    ids: ids_for(&players, &player_ids, player),
                    name: name.clone(),
                    stack: stack.clone(),
                    mode,
                },
                messages,
            )
            .await;
            Ok(1)
        })
    }
}

async fn set_block<M: tuff_item::item::Mode>(
    ctx: &mut Context<'_>,
    mode: SlotFill,
    slot: &SlotArg,
    stack: &ItemStack<M>,
    item: TextComponent,
) -> Result<i32, CommandError> {
    let Some(Value::Coordinates(coordinates)) = ctx.arg("target").cloned() else {
        return Err(fail_args());
    };
    let pos = coordinates.block(&ctx.source.pose());
    let (state, mut data, holder) =
        container(ctx, pos, "commands.item.target.not_a_container").await?;
    let valid: Vec<i32> = slot
        .ids(&data, holder)
        .iter()
        .copied()
        .filter(|id| has_slot(*id, holder, &data))
        .collect();
    if valid.is_empty() {
        return Err(slot.missing_error("target"));
    }
    for (id, value) in spread(mode, &valid, stack) {
        write_slot(&mut data, id, holder, value);
    }
    store_block(ctx, pos, state, data, holder).await?;
    let [x, y, z] = pos_args(pos);
    ctx.success(
        TextComponent::translate("commands.item.block.set.success", [x, y, z, item]),
        true,
    )
    .await;
    Ok(1)
}

const MAX_SOURCE_DEPTH: usize = 16;

enum SlotKind {
    Ids(Vec<i32>),
    Source(SlotSourceRef),
}

/// A `slot_source` argument: a slot range name, a stored slot source id or
/// an inline source. `name` is what error messages show; inline sources
/// have none.
pub(super) struct SlotArg {
    pub(super) name: Option<String>,
    kind: SlotKind,
    sources: Arc<SlotSources>,
}

impl SlotArg {
    pub(super) fn from_value(
        ctx: &Context<'_>,
        value: Option<&Value>,
    ) -> Result<Self, CommandError> {
        let sources = ctx.shared().datapacks.contents().slot_sources.clone();
        let (name, kind) = match value {
            Some(Value::Slots { name, ids }) => (Some(name.clone()), SlotKind::Ids(ids.clone())),
            Some(Value::Identifier(id)) => {
                if sources.get(id).is_none() {
                    return Err(CommandError::dynamic(
                        "argument.resource_or_id.no_such_element",
                        [id.clone(), "minecraft:slot_source".to_owned()],
                    ));
                }
                (
                    Some(id.clone()),
                    SlotKind::Source(SlotSourceRef::Id(id.clone())),
                )
            }
            Some(Value::Nbt(inline)) => {
                let json = super::predicates::json_of(inline);
                let parsed = tuff_datapack::slot_source::parse(&json, 0, &|range| {
                    slot_range(range).is_some()
                })
                .map_err(|err| {
                    CommandError::dynamic("argument.resource_or_id.failed_to_parse", [err])
                })?;
                (None, SlotKind::Source(SlotSourceRef::Inline(parsed)))
            }
            _ => return Err(fail_args()),
        };
        Ok(Self {
            name,
            kind,
            sources,
        })
    }

    /// The slot ids this argument selects in a holder's data, in order.
    pub(super) fn ids(&self, data: &Compound, holder: Holder) -> Vec<i32> {
        match &self.kind {
            SlotKind::Ids(ids) => ids.clone(),
            SlotKind::Source(source) => resolve(source, &self.sources, data, holder, 0),
        }
    }

    fn fixed(&self) -> Option<&[i32]> {
        match &self.kind {
            SlotKind::Ids(ids) => Some(ids),
            SlotKind::Source(_) => None,
        }
    }

    /// The ids for each player, read from their data unless fixed.
    fn player_ids(&self, ctx: &Context<'_>, players: &[PlayerSnapshot]) -> Vec<Vec<i32>> {
        players
            .iter()
            .map(|player| match self.fixed() {
                Some(ids) => ids.to_vec(),
                None => self.ids(&player_data(ctx, player), Holder::Player),
            })
            .collect()
    }

    fn missing(&self, side: &str) -> TextComponent {
        match &self.name {
            Some(name) => {
                TextComponent::translate(format!("commands.item.{side}.no_such_slot"), [text(name)])
            }
            None => {
                TextComponent::translate(format!("commands.item.{side}.no_such_slot.unnamed"), [])
            }
        }
    }

    fn missing_error(&self, side: &str) -> CommandError {
        match &self.name {
            Some(name) => {
                CommandError::dynamic(&format!("commands.item.{side}.no_such_slot"), [text(name)])
            }
            None => CommandError::simple(&format!("commands.item.{side}.no_such_slot.unnamed")),
        }
    }

    fn label(&self) -> String {
        self.name.clone().unwrap_or_default()
    }
}

fn ids_for(players: &[PlayerSnapshot], ids: &[Vec<i32>], player: &PlayerSnapshot) -> Vec<i32> {
    players
        .iter()
        .position(|candidate| candidate.id == player.id)
        .and_then(|index| ids.get(index).cloned())
        .unwrap_or_default()
}

fn resolve(
    source: &SlotSourceRef,
    sources: &SlotSources,
    data: &Compound,
    holder: Holder,
    depth: usize,
) -> Vec<i32> {
    if depth > MAX_SOURCE_DEPTH {
        return Vec::new();
    }
    let stored;
    let source = match source {
        SlotSourceRef::Inline(source) => source,
        SlotSourceRef::Id(id) => match sources.get(id) {
            Some(found) => {
                stored = found;
                &*stored
            }
            None => return Vec::new(),
        },
    };
    let child = |inner: &SlotSourceRef| resolve(inner, sources, data, holder, depth + 1);
    match source {
        SlotSource::Empty | SlotSource::Contents { .. } => Vec::new(),
        SlotSource::Group(terms) => terms.iter().flat_map(child).collect(),
        SlotSource::Limit { source, limit } => {
            let mut ids = child(source);
            ids.truncate(*limit as usize);
            ids
        }
        SlotSource::Filtered {
            source,
            item_filter,
        } => child(source)
            .into_iter()
            .filter(|id| {
                let stack = peek_slot(data, *id, holder)
                    .flatten()
                    .unwrap_or_else(|| ItemPicture::new(Item::Air, 0));
                tuff_core::world::predicate::item(item_filter, &stack)
            })
            .collect(),
        SlotSource::Range { source, slots } => {
            if source != "container" {
                return Vec::new();
            }
            slot_range(slots)
                .unwrap_or_default()
                .into_iter()
                .filter(|id| has_slot(*id, holder, data))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn mob_inventory_and_horse_slots_map_to_their_lists() {
        let mut nbt = Compound::new();
        let stack = ItemStack::mint_from_nbt(
            &Compound::new()
                .with("id", Tag::String("minecraft:apple".into()))
                .with("count", Tag::Int(1)),
            MintReason::Test,
        );
        assert!(stack.is_some());
        let other = stack
            .as_ref()
            .map(|stack| ItemStack::mint_like(stack, stack.count(), MintReason::Test));
        write_slot(&mut nbt, 302, Holder::Mob, stack);
        write_slot(&mut nbt, 503, Holder::Mob, other);
        assert!(nbt.get_list("Inventory").is_some());
        assert!(nbt.get_list("Items").is_some());
        assert!(read_slot(&nbt, 302, Holder::Mob).flatten().is_some());
        assert!(read_slot(&nbt, 503, Holder::Mob).flatten().is_some());
        assert!(read_slot(&nbt, 303, Holder::Mob).flatten().is_none());
        assert!(has_slot(307, Holder::Mob, &nbt) && !has_slot(308, Holder::Mob, &nbt));
    }
}
