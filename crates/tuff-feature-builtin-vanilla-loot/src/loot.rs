//! Block drops from the vanilla `blocks/*` loot tables, evaluated for a
//! player mining without enchantments.

use rand::Rng;
use rand::seq::SliceRandom;
use serde_json::Value;
use tuff_item::item::BurnReason;
use tuff_vanilla_data::loot_table::{
    LootCondition, LootEntry, LootEntryKind, LootFunction, LootPool, LootTable, NumberProvider,
};
use tuff_vanilla_data::{Block, BlockState, Item};

use std::cell::RefCell;

use tuff_datapack::loot::LootTables;
use tuff_item::item::{AnyItem, Enchantments, ItemPicture, ItemStack, MintReason, Mode};
use tuff_vanilla_data::registry::Potion;

use tuff_core::world::block_entity::raw;
use tuff_core::world::crafting::CookingStation;
use tuff_core::world::predicate::{LocationFacts, PredicateWorld};
use tuff_item::item::enchanting;
use tuff_nbt::{Compound, Tag};

mod functions;
mod modify;
mod slots;
pub use modify::modify_item;

pub const SILK_TOUCH: &str = "minecraft:silk_touch";

pub fn can_harvest(state: BlockState, tool: Option<AnyItem<'_>>) -> bool {
    !state.requires_correct_tool_for_drops()
        || tool.is_some_and(|tool| {
            tuff_item::with_item!(tool, |stack| stack.is_correct_tool_for_drops(state.block()))
        })
}

pub use tuff_core::world::drops::EntityFacts;

/// What a loot table's conditions can see.
struct Context<'a> {
    tables: &'a LootTables,
    /// The tables being rolled, by address, so a table that reaches
    /// itself rolls nothing the second time.
    visiting: RefCell<Vec<usize>>,
    state: BlockState,
    entity: Option<&'a EntityFacts>,
    tool: Option<AnyItem<'a>>,
    /// The radius of the explosion that broke the block, when its drops
    /// decay.
    explosion_radius: Option<f32>,
    /// The broken block's block entity, as saved.
    block_entity: Option<&'a Compound>,
    /// Where the loot is generated, for `location_check`.
    location: Option<&'a LocationFacts>,
    /// The level, for weather, clock, score and attribute conditions.
    world: Option<&'a dyn PredicateWorld>,
}

/// Whether one item survives an explosion of `radius`.
fn survives(radius: Option<f32>, rng: &mut impl Rng) -> bool {
    radius.is_none_or(|radius| rng.r#gen::<f32>() <= 1.0 / radius)
}

/// Applies an `explosion_decay` function to `drops`, keeping each item
/// with the survival chance.
fn decay(drops: &mut [ItemStack], radius: Option<f32>, rng: &mut impl Rng) {
    if radius.is_none() {
        return;
    }
    for stack in drops {
        let kept = (0..stack.count()).filter(|_| survives(radius, rng)).count() as u8;
        stack.resize(kept, MintReason::Loot, BurnReason::Destroyed);
    }
}

fn applies_decay(functions: &[LootFunction]) -> bool {
    functions
        .iter()
        .any(|f| f.kind == "minecraft:explosion_decay")
}

pub fn block_drops(
    state: BlockState,
    tool: Option<AnyItem<'_>>,
    block_entity: Option<&Compound>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    block_drops_in(LootTables::builtin(), state, tool, block_entity, rng)
}

pub fn block_drops_in(
    tables: &LootTables,
    state: BlockState,
    tool: Option<AnyItem<'_>>,
    block_entity: Option<&Compound>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    if !can_harvest(state, tool) {
        return Vec::new();
    }
    rolled_block_drops(tables, state, tool, block_entity, None, None, rng)
}

/// [`block_drops_in`] for a block broken at `at`, which `location_check`
/// conditions test.
pub fn block_drops_located(
    tables: &LootTables,
    state: BlockState,
    tool: Option<AnyItem<'_>>,
    block_entity: Option<&Compound>,
    at: &LocationFacts,
    world: Option<&dyn PredicateWorld>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    if !can_harvest(state, tool) {
        return Vec::new();
    }
    rolled_block_drops(tables, state, tool, block_entity, Some(at), world, rng)
}

fn rolled_block_drops(
    tables: &LootTables,
    state: BlockState,
    tool: Option<AnyItem<'_>>,
    block_entity: Option<&Compound>,
    location: Option<&LocationFacts>,
    world: Option<&dyn PredicateWorld>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    let Some(table) = state.block().loot_table().and_then(|name| tables.get(name)) else {
        return Vec::new();
    };
    let context = Context {
        tables,
        visiting: RefCell::default(),
        state,
        entity: None,
        tool,
        explosion_radius: None,
        block_entity,
        location,
        world,
    };
    let mut drops = Vec::new();
    roll_table(table, &context, rng, &mut drops);
    drops
}

/// The drops of an entity loot table (death, shearing, egg laying) for
/// the entity `facts` describes.
pub fn entity_drops(name: &str, facts: &EntityFacts, rng: &mut impl Rng) -> Vec<ItemStack> {
    let tables = facts.tables.as_deref().unwrap_or(LootTables::builtin());
    let Some(table) = tables.get(name) else {
        return Vec::new();
    };
    let context = Context {
        tables,
        visiting: RefCell::default(),
        state: Block::Air.default_state(),
        entity: Some(facts),
        tool: facts.attacker_weapon.as_deref().map(ItemStack::any),
        explosion_radius: None,
        block_entity: None,
        location: facts.origin.as_ref(),
        world: None,
    };
    let mut drops = Vec::new();
    roll_table(table, &context, rng, &mut drops);
    drops.retain(|stack| stack.count() > 0);
    drops
}

/// What a bobber reels in: the fishing table rolled with the rod as the
/// tool and `luck` shifting the junk, treasure and fish pools.
pub fn fishing_drops(
    tables: &LootTables,
    luck: f32,
    open_water: bool,
    rod: AnyItem<'_>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    let facts = EntityFacts {
        kind: Some(tuff_vanilla_data::registry::EntityType::FishingBobber),
        open_water: Some(open_water),
        luck,
        ..EntityFacts::default()
    };
    let Some(table) = tables.get("minecraft:gameplay/fishing") else {
        return Vec::new();
    };
    let context = Context {
        tables,
        visiting: RefCell::default(),
        state: Block::Air.default_state(),
        entity: Some(&facts),
        tool: Some(rod),
        explosion_radius: None,
        block_entity: None,
        location: None,
        world: None,
    };
    let mut drops = Vec::new();
    roll_table(table, &context, rng, &mut drops);
    drops.retain(|stack| stack.count() > 0);
    drops
}

/// Drops of a block an explosion broke: no tool, and when the explosion
/// decays drops, `radius` thins them out.
pub fn exploded_drops(
    state: BlockState,
    radius: Option<f32>,
    block_entity: Option<&Compound>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    exploded_drops_in(LootTables::builtin(), state, radius, block_entity, rng)
}

pub fn exploded_drops_in(
    tables: &LootTables,
    state: BlockState,
    radius: Option<f32>,
    block_entity: Option<&Compound>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    let Some(table) = state.block().loot_table().and_then(|name| tables.get(name)) else {
        return Vec::new();
    };
    let context = Context {
        tables,
        visiting: RefCell::default(),
        state,
        entity: None,
        tool: None,
        explosion_radius: radius,
        block_entity,
        location: None,
        world: None,
    };
    let mut drops = Vec::new();
    roll_table(table, &context, rng, &mut drops);
    drops.retain(|stack| stack.count() > 0);
    drops
}

/// Drops of a named table rolled for an interaction with `state` (a hoe
/// tilling rooted dirt).
pub fn table_drops(
    name: &str,
    state: BlockState,
    tool: Option<Item>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    table_drops_in(LootTables::builtin(), name, state, tool, rng)
}

pub fn table_drops_in(
    tables: &LootTables,
    name: &str,
    state: BlockState,
    tool: Option<Item>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    let Some(table) = tables.get(name) else {
        return Vec::new();
    };
    let tool = tool.map(|item| ItemPicture::new(item, 1));
    let context = Context {
        tables,
        visiting: RefCell::default(),
        state,
        entity: None,
        tool: tool.as_ref().map(ItemStack::any),
        explosion_radius: None,
        block_entity: None,
        location: None,
        world: None,
    };
    let mut drops = Vec::new();
    roll_table(table, &context, rng, &mut drops);
    drops
}

/// A named loot table rolled with no block, tool or entity in view (a
/// trial spawner's or vault's rewards, a mob's equipment table).
pub fn roll(name: &str, rng: &mut impl Rng) -> Vec<ItemStack> {
    roll_in(LootTables::builtin(), name, rng)
}

/// [`roll`] against the loot tables in effect.
pub fn roll_in(tables: &LootTables, name: &str, rng: &mut impl Rng) -> Vec<ItemStack> {
    let Some(table) = tables.get(name) else {
        return Vec::new();
    };
    let context = Context {
        tables,
        visiting: RefCell::default(),
        state: Block::Air.default_state(),
        entity: None,
        tool: None,
        explosion_radius: None,
        block_entity: None,
        location: None,
        world: None,
    };
    let mut drops = Vec::new();
    roll_table(table, &context, rng, &mut drops);
    drops.retain(|stack| stack.count() > 0);
    drops
}

/// Drops of a block destroyed by the world, e.g. by losing its support.
pub fn destroyed_drops(
    state: BlockState,
    block_entity: Option<&Compound>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    rolled_block_drops(
        LootTables::builtin(),
        state,
        None,
        block_entity,
        None,
        None,
        rng,
    )
}

/// [`destroyed_drops`] against the loot tables in effect.
pub fn destroyed_drops_in(
    tables: &LootTables,
    state: BlockState,
    block_entity: Option<&Compound>,
    rng: &mut impl Rng,
) -> Vec<ItemStack> {
    rolled_block_drops(tables, state, None, block_entity, None, None, rng)
}

/// Rolls `name`'s loot table into a container's slots the way a chest's
/// first open does: the pools roll independently of any block or tool, then
/// stacks bigger than one are randomly split and everything is shuffled
/// into `slots` container slots (extra items past a full container are
/// dropped, as the game warns and does). `None` when the table is unknown.
pub fn container_loot(
    name: &str,
    slots: usize,
    rng: &mut impl Rng,
) -> Option<Vec<Option<ItemStack>>> {
    container_loot_in(LootTables::builtin(), name, slots, rng)
}

pub fn container_loot_in(
    tables: &LootTables,
    name: &str,
    slots: usize,
    rng: &mut impl Rng,
) -> Option<Vec<Option<ItemStack>>> {
    let table = tables.get(name)?;
    let context = Context {
        tables,
        visiting: RefCell::default(),
        state: Block::Air.default_state(),
        entity: None,
        tool: None,
        explosion_radius: None,
        block_entity: None,
        location: None,
        world: None,
    };
    let mut drops = Vec::new();
    roll_table(table, &context, rng, &mut drops);
    drops.retain(|stack| stack.count() > 0);
    Some(fill_container_slots(drops, slots, rng))
}

/// Splits stacks bigger than one and shuffles everything into `slots`
/// slots, as `LootTable.fill` does.
fn fill_container_slots(
    drops: Vec<ItemStack>,
    slots: usize,
    rng: &mut impl Rng,
) -> Vec<Option<ItemStack>> {
    let mut singles = Vec::new();
    let mut splittable = Vec::new();
    for stack in drops {
        if stack.count() > 1 {
            splittable.push(stack);
        } else {
            singles.push(stack);
        }
    }
    while slots.saturating_sub(singles.len() + splittable.len()) > 0 && !splittable.is_empty() {
        let index = rng.gen_range(0..splittable.len());
        let mut stack = splittable.remove(index);
        let half = (stack.count() / 2).max(1);
        let taken = rng.gen_range(1..=half);
        let piece = stack.take(taken);
        if stack.count() > 1 && rng.r#gen::<bool>() {
            splittable.push(stack);
        } else {
            singles.push(stack);
        }
        if piece.count() > 1 && rng.r#gen::<bool>() {
            splittable.push(piece);
        } else {
            singles.push(piece);
        }
    }
    singles.extend(splittable);
    singles.shuffle(rng);
    let mut available: Vec<usize> = (0..slots).collect();
    available.shuffle(rng);
    let mut result: Vec<Option<ItemStack>> = (0..slots).map(|_| None).collect();
    for stack in singles {
        let Some(slot) = available.pop() else {
            break;
        };
        result[slot] = Some(stack);
    }
    result
}

/// Whether a container's `lock` predicate (`items`/`count`/`components`;
/// `predicates` sub-matchers are not modeled) admits the item held to open
/// it.
pub fn lock_admits<M: Mode>(lock: &Compound, stack: Option<&ItemStack<M>>) -> bool {
    if let Some(items) = lock.get("items") {
        let Some(stack) = stack else {
            return false;
        };
        if !nbt_in_list(items, stack.item().name(), |tag| {
            in_tag("minecraft:item", tag, stack.item().name())
        }) {
            return false;
        }
    }
    if let Some(count) = lock.get("count")
        && !nbt_int_in_range(count, stack.map_or(0, |s| s.count() as i64))
    {
        return false;
    }
    if let Some(components) = lock.get_compound("components") {
        let patch = stack.map(|s| s.components.to_nbt()).unwrap_or_default();
        if !components
            .iter()
            .all(|(id, expected)| patch.get(id) == Some(expected))
        {
            return false;
        }
    }
    true
}

/// Like [`in_list`], for the NBT-encoded form of a holder set (a single
/// resource location, `#tag`, or a list of resource locations).
fn nbt_in_list(list: &Tag, name: &str, in_tag: impl Fn(&str) -> bool) -> bool {
    match list {
        Tag::String(entry) => match entry.strip_prefix('#') {
            Some(tag) => in_tag(tag),
            None => entry == name,
        },
        Tag::List(entries) => entries.iter().any(|e| e.as_str() == Some(name)),
        _ => false,
    }
}

/// Like [`int_in_range`], for the NBT-encoded form of `MinMaxBounds.Ints`
/// (an exact int, or `{min, max}`).
fn nbt_int_in_range(bound: &Tag, value: i64) -> bool {
    if let Some(exact) = bound.as_int() {
        return value == exact as i64;
    }
    let Some(range) = bound.as_compound() else {
        return false;
    };
    range.get_int("min").is_none_or(|min| value >= min as i64)
        && range.get_int("max").is_none_or(|max| value <= max as i64)
}

fn roll_table(
    table: &LootTable,
    context: &Context,
    rng: &mut impl Rng,
    drops: &mut Vec<ItemStack>,
) {
    let address = std::ptr::from_ref(table) as usize;
    if context.visiting.borrow().contains(&address) {
        return;
    }
    context.visiting.borrow_mut().push(address);
    let start = drops.len();
    for pool in table.pools.iter() {
        if !pool
            .condition
            .as_ref()
            .is_none_or(|c| condition(c, context, rng))
        {
            continue;
        }
        let pool_start = drops.len();
        let luck = context.entity.map_or(0.0, |facts| facts.luck);
        for _ in 0..pool_rolls(pool, luck, rng) {
            roll_pool_once(&pool.entries, context, rng, drops);
        }
        modify_stacks(&pool.functions, &mut drops[pool_start..], context, rng);
        drop_emptied(drops, pool_start);
        copy_components(&pool.functions, &mut drops[pool_start..], context, rng);
        if applies_decay(&pool.functions) {
            decay(&mut drops[pool_start..], context.explosion_radius, rng);
        }
    }
    modify_stacks(&table.functions, &mut drops[start..], context, rng);
    drop_emptied(drops, start);
    copy_components(&table.functions, &mut drops[start..], context, rng);
    if applies_decay(&table.functions) {
        decay(&mut drops[start..], context.explosion_radius, rng);
    }
    context.visiting.borrow_mut().pop();
}

/// Applies a loot function that changes a created stack rather than its
/// count: potions, damage, enchantments, components and ominous bottle
/// levels. Others leave it as it is.
fn modify_stack(
    mut stack: ItemStack,
    kind: &str,
    params: &Value,
    context: &Context,
    rng: &mut impl Rng,
) -> ItemStack {
    match kind {
        "minecraft:exploration_map" => {
            if let Some(spec) = crate::exploration::ExplorationSpec::from_json(params) {
                crate::exploration::mark(&mut stack, &spec);
            }
        }
        "minecraft:set_count" => {
            if let Some(value) = json_int(&params["count"], rng, context.scores().as_ref()) {
                let count = if params["add"].as_bool() == Some(true) {
                    i64::from(stack.count()) + value
                } else {
                    value
                };
                stack.resize(
                    count.clamp(0, 255) as u8,
                    MintReason::Loot,
                    BurnReason::Loot,
                );
            }
        }
        "minecraft:limit_count" => {
            let count = limit_count(&params["limit"], i64::from(stack.count()), rng);
            stack.resize(
                count.clamp(0, 255) as u8,
                MintReason::Loot,
                BurnReason::Loot,
            );
        }
        "minecraft:apply_bonus" => {
            let level = tool_enchantment_level(params, context.tool);
            let count = apply_bonus(params, i64::from(stack.count()), level, rng);
            stack.resize(
                count.clamp(0, 255) as u8,
                MintReason::Loot,
                BurnReason::Loot,
            );
        }
        "minecraft:set_item" => {
            if let Some(item) = params["item"].as_str().and_then(Item::from_name) {
                let count = stack.count();
                stack = stack.into_item(item, count, MintReason::Loot, BurnReason::Loot);
            }
        }
        "minecraft:furnace_smelt" => {
            let smelted = match context.entity.and_then(|e| e.recipes.as_deref()) {
                Some(recipes) => recipes
                    .cooking_recipe(CookingStation::Furnace, stack.item())
                    .map(|(_, recipe)| recipe.result.item),
                None => smelting_recipe(stack.item()).map(|recipe| recipe.result.item),
            };
            if let Some(item) = smelted {
                let count = stack.count();
                stack = stack.into_item(item, count, MintReason::Loot, BurnReason::Loot);
            }
        }
        "minecraft:set_name" => {
            if let Some(name) = params.get("name").and_then(json_to_nbt) {
                let target = format!(
                    "minecraft:{}",
                    params["target"].as_str().unwrap_or("custom_name")
                );
                let mut patch = stack.components.to_nbt();
                patch.insert(target, name);
                stack.set_components_nbt(&patch, MintReason::Loot, BurnReason::Loot);
            }
        }
        "minecraft:set_potion" => {
            if let Some(potion) = params["id"].as_str().and_then(Potion::from_name) {
                stack
                    .components
                    .potion_contents
                    .get_or_insert_with(Default::default)
                    .potion = Some(potion);
            }
        }
        "minecraft:set_random_potion" => {
            let options: Vec<Potion> = match params.get("options") {
                None => Potion::ALL.to_vec(),
                Some(value) => potion_options(value),
            };
            if !options.is_empty() {
                stack
                    .components
                    .potion_contents
                    .get_or_insert_with(Default::default)
                    .potion = Some(options[rng.gen_range(0..options.len())]);
            }
        }
        "minecraft:set_damage" => {
            if let Some(max) = stack.max_damage().filter(|max| *max > 0) {
                let base = if params["add"].as_bool() == Some(true) {
                    1.0 - (stack.damage() / max) as f32
                } else {
                    0.0
                };
                let damage = json_number(&params["damage"], rng, None).unwrap_or(0.0);
                let remaining = 1.0 - (damage + base).clamp(0.0, 1.0);
                stack.set_damage((remaining * max as f32).floor() as u32);
            }
        }
        "minecraft:set_enchantments" => {
            let add = params["add"].as_bool() == Some(true);
            if stack.item() == Item::Book {
                let count = stack.count();
                stack = stack.into_item(
                    Item::EnchantedBook,
                    count,
                    MintReason::Loot,
                    BurnReason::Loot,
                );
            }
            let target = if stack.item() == Item::EnchantedBook {
                &mut stack.components.stored_enchantments
            } else {
                &mut stack.components.enchantments
            };
            let mut entries: Vec<(String, i32)> = target
                .as_ref()
                .map(|e| e.entries().to_vec())
                .unwrap_or_default();
            for (id, level) in params["enchantments"].as_object().into_iter().flatten() {
                let Some(level) = json_int(level, rng, None) else {
                    continue;
                };
                let existing = entries.iter().position(|(e, _)| e == id);
                let current = existing.map_or(0, |i| entries[i].1);
                let level = if add { current as i64 + level } else { level }.clamp(0, 255) as i32;
                match (existing, level) {
                    (Some(i), 0) => {
                        entries.remove(i);
                    }
                    (Some(i), level) => entries[i].1 = level,
                    (None, 0) => {}
                    (None, level) => entries.push((id.clone(), level)),
                }
            }
            *target = (!entries.is_empty()).then(|| Enchantments::new(entries));
        }
        "minecraft:enchant_randomly" => {
            let only_compatible = params["only_compatible"].as_bool().unwrap_or(true);
            stack = enchanting::enchant_randomly(stack, &params["options"], only_compatible, rng).0;
        }
        "minecraft:enchant_with_levels" => {
            if let Some(levels) = json_int(&params["levels"], rng, None) {
                let registry = tuff_item::enchantment::current();
                let options = enchanting::enchantment_options(&registry, &params["options"]);
                let chosen = enchanting::select_enchantments(&stack, levels as i32, &options, rng);
                stack = enchanting::enchant(stack, &chosen);
            }
        }
        "minecraft:set_components" => {
            if let Some(components) = params["components"].as_object() {
                let mut patch = stack.components.to_nbt();
                for (id, value) in components {
                    match id.strip_prefix('!') {
                        Some(removed) => {
                            patch.remove(removed);
                            patch.insert(id, Compound::new());
                        }
                        None => {
                            if let Some(tag) = json_to_nbt(value) {
                                patch.remove(&format!("!{id}"));
                                patch.insert(id, tag);
                            }
                        }
                    }
                }
                stack.set_components_nbt(&patch, MintReason::Loot, BurnReason::Loot);
            }
        }
        "minecraft:set_stew_effect" => {
            let entries = params["effects"].as_array().filter(|e| !e.is_empty());
            if let (Item::SuspiciousStew, Some(entries)) = (stack.item(), entries) {
                let entry = &entries[rng.gen_range(0..entries.len())];
                let effect = entry["type"]
                    .as_str()
                    .and_then(tuff_vanilla_data::registry::MobEffect::from_name);
                let duration = json_number(&entry["duration"], rng, None);
                if let (Some(effect), Some(duration)) = (effect, duration) {
                    let ticks = if effect.is_instantaneous() {
                        duration
                    } else {
                        duration * 20.0
                    } as i32;
                    stack
                        .components
                        .suspicious_stew_effects
                        .get_or_insert_with(Vec::new)
                        .push(tuff_item::item::SuspiciousEffect {
                            effect,
                            duration: ticks,
                        });
                }
            }
        }
        "minecraft:set_random_dyes" => {
            let rolls = json_int(&params["number_of_dyes"], rng, None).unwrap_or(0);
            if rolls > 0 {
                let dyes: Vec<tuff_vanilla_data::item::DyeColor> = (0..rolls)
                    .map(|_| {
                        tuff_vanilla_data::item::DyeColor::ALL
                            [rng.gen_range(0..tuff_vanilla_data::item::DyeColor::ALL.len())]
                    })
                    .collect();
                let color = tuff_item::item::decor::apply_dyes(stack.components.dyed_color, &dyes);
                stack.resize(1, MintReason::Loot, BurnReason::Loot);
                stack.components.dyed_color = Some(color);
            }
        }
        "minecraft:set_ominous_bottle_amplifier" => {
            if let Some(amplifier) = json_int(&params["amplifier"], rng, None) {
                stack.set_ominous_bottle_amplifier(amplifier.clamp(0, 4) as u32);
            }
        }
        _ => stack = functions::apply(stack, kind, params, context, rng),
    }
    stack
}

fn potion_options(value: &serde_json::Value) -> Vec<Potion> {
    match value {
        serde_json::Value::String(s) => match s.strip_prefix('#') {
            Some(tag) => {
                tuff_datapack::tags::with(|tags| tags.typed::<Potion>(tag)).unwrap_or_default()
            }
            None => Potion::from_name(s).into_iter().collect(),
        },
        serde_json::Value::Array(list) => list
            .iter()
            .filter_map(|v| Potion::from_name(v.as_str()?))
            .collect(),
        _ => Vec::new(),
    }
}

/// Removes the stacks from `from` on that a function emptied.
fn drop_emptied(drops: &mut Vec<ItemStack>, from: usize) {
    let mut position = 0;
    drops.retain(|stack| {
        let keep = position < from || stack.count() > 0;
        position += 1;
        keep
    });
}

/// Applies a pool's or table's stack-changing functions to its drops.
fn modify_stacks(
    functions: &[LootFunction],
    drops: &mut [ItemStack],
    context: &Context,
    rng: &mut impl Rng,
) {
    for function in functions {
        let params = function
            .params
            .as_deref()
            .and_then(|p| serde_json::from_str::<Value>(p).ok())
            .unwrap_or_else(|| Value::Object(Default::default()));
        if !params
            .get("condition")
            .is_none_or(|c| json_condition(c, context, rng))
        {
            continue;
        }
        for stack in drops.iter_mut() {
            let taken = std::mem::replace(stack, ItemStack::empty());
            *stack = modify_stack(taken, &function.kind, &params, context, rng);
        }
    }
}

fn json_to_nbt(value: &Value) -> Option<Tag> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i32::try_from(i).map_or(Tag::Long(i), Tag::Int),
            (None, Some(f)) => Tag::Double(f),
            _ => return None,
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(values) => Tag::List(values.iter().map(json_to_nbt).collect::<Option<_>>()?),
        Value::Object(map) => Tag::Compound(
            map.iter()
                .map(|(k, v)| Some((k.clone(), json_to_nbt(v)?)))
                .collect::<Option<Compound>>()?,
        ),
    })
}

/// Applies `copy_components` functions that copy from the broken block's
/// block entity: the components it collects, filtered by `include` and
/// `exclude`, set on each drop.
fn copy_components(
    functions: &[LootFunction],
    drops: &mut [ItemStack],
    context: &Context,
    rng: &mut impl Rng,
) {
    for function in functions
        .iter()
        .filter(|f| f.kind == "minecraft:copy_components")
    {
        let Some(params) = function
            .params
            .as_deref()
            .and_then(|p| serde_json::from_str::<Value>(p).ok())
        else {
            continue;
        };
        if params["source"] != "block_entity"
            || !params
                .get("condition")
                .is_none_or(|c| json_condition(c, context, rng))
        {
            continue;
        }
        let Some(nbt) = context.block_entity else {
            continue;
        };
        let Some(kind) = raw::kind(nbt) else {
            continue;
        };
        let listed = |key: &str| {
            params[key]
                .as_array()
                .map(|ids| ids.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        };
        let (include, exclude) = (listed("include"), listed("exclude"));
        let collected = raw::collect_components(kind, nbt);
        for stack in drops.iter_mut() {
            let mut patch = stack.components.to_nbt();
            for (id, value) in collected.iter() {
                if include.as_ref().is_some_and(|ids| !ids.contains(&id))
                    || exclude.as_ref().is_some_and(|ids| ids.contains(&id))
                {
                    continue;
                }
                patch.remove(&format!("!{id}"));
                patch.insert(id, value.clone());
            }
            stack.set_components_nbt(&patch, MintReason::Loot, BurnReason::Loot);
        }
    }
}

/// One roll of a pool: expands `entries` into the weighted leaf candidates
/// it allows this roll (honoring `group`/`alternatives`/`sequence`
/// composition and every level's `condition`), then creates the item of a
/// single one chosen at random by weight -- matching a pool's own roll,
/// which produces at most one item per roll, not one per listed entry.
fn effective_weight(entry: &LootEntry, context: &Context) -> u64 {
    let luck = context.entity.map_or(0.0, |facts| facts.luck);
    let shifted = i64::try_from(entry.weight).unwrap_or(i64::MAX)
        + (entry.quality as f64 * f64::from(luck)).floor() as i64;
    shifted.max(0) as u64
}

type Candidate<'a> = (&'a LootEntry, Option<Item>);

fn roll_pool_once(
    entries: &[LootEntry],
    context: &Context,
    rng: &mut impl Rng,
    drops: &mut Vec<ItemStack>,
) {
    let mut candidates: Vec<Candidate> = Vec::new();
    for entry in entries {
        expand_pool_entry(entry, context, rng, &mut candidates);
    }
    let total_weight: u64 = candidates
        .iter()
        .map(|(entry, _)| effective_weight(entry, context))
        .sum();
    if total_weight == 0 {
        return;
    }
    let chosen = if candidates.len() == 1 {
        candidates[0]
    } else {
        let mut index = rng.gen_range(0..total_weight);
        candidates
            .iter()
            .find(|(entry, _)| {
                let weight = effective_weight(entry, context);
                if index < weight {
                    true
                } else {
                    index -= weight;
                    false
                }
            })
            .copied()
            .unwrap_or(candidates[candidates.len() - 1])
    };
    create_entry_item(chosen.0, chosen.1, context, rng, drops);
}

/// Expands an entry into the weighted leaf candidates it contributes to a
/// pool's roll, returning whether the entry "ran" (which is what
/// `alternatives` and `sequence` parents branch on). A composite entry
/// (`group`/`alternatives`/`sequence`) never becomes a candidate itself,
/// only the leaves it expands to; a leaf with weight `0` runs (so a
/// following `sequence` sibling still gets a chance) but is never a pick.
fn expand_pool_entry<'a>(
    entry: &'a LootEntry,
    context: &Context,
    rng: &mut impl Rng,
    candidates: &mut Vec<Candidate<'a>>,
) -> bool {
    if !entry
        .condition
        .as_ref()
        .is_none_or(|c| condition(c, context, rng))
    {
        return false;
    }
    match &entry.kind {
        LootEntryKind::Group(children) => {
            for child in children {
                expand_pool_entry(child, context, rng, candidates);
            }
            true
        }
        LootEntryKind::Alternatives(children) => {
            for child in children {
                if expand_pool_entry(child, context, rng, candidates) {
                    return true;
                }
            }
            false
        }
        LootEntryKind::Sequence(children) => {
            for child in children {
                if !expand_pool_entry(child, context, rng, candidates) {
                    return false;
                }
            }
            true
        }
        LootEntryKind::Tag {
            tag, expand: true, ..
        } => {
            if effective_weight(entry, context) > 0 {
                let items =
                    tuff_datapack::tags::with(|tags| tags.typed::<Item>(tag)).unwrap_or_default();
                candidates.extend(items.iter().map(|item| (entry, Some(*item))));
            }
            true
        }
        _ => {
            if effective_weight(entry, context) > 0 {
                candidates.push((entry, None));
            }
            true
        }
    }
}

/// Creates the item(s) of a single chosen leaf entry.
fn create_entry_item(
    entry: &LootEntry,
    forced: Option<Item>,
    context: &Context,
    rng: &mut impl Rng,
    drops: &mut Vec<ItemStack>,
) {
    if forced.is_none()
        && (matches!(entry.kind, LootEntryKind::Item(_)) || entry.functions.is_empty())
    {
        return create_entry_leaf(entry, context, rng, drops);
    }
    let from = drops.len();
    match forced {
        #[expect(
            clippy::disallowed_methods,
            reason = "loot table output (create_entry_item)"
        )]
        Some(item) => drops.push(ItemStack::mint(item, 1, MintReason::Loot)),
        None => create_entry_leaf(entry, context, rng, drops),
    }
    modify_stacks(&entry.functions, &mut drops[from..], context, rng);
    drop_emptied(drops, from);
}

fn create_entry_leaf(
    entry: &LootEntry,
    context: &Context,
    rng: &mut impl Rng,
    drops: &mut Vec<ItemStack>,
) {
    match &entry.kind {
        LootEntryKind::Item(item) => {
            let mut count: i64 = 1;
            let mut item = *item;
            let mut stack_functions = Vec::new();
            for function in entry.functions.iter() {
                let params = function
                    .params
                    .as_deref()
                    .and_then(|p| serde_json::from_str::<Value>(p).ok())
                    .unwrap_or_else(|| Value::Object(Default::default()));
                let applies = params
                    .get("condition")
                    .is_none_or(|c| json_condition(c, context, rng));
                if !applies {
                    continue;
                }
                match &*function.kind {
                    "minecraft:set_count" => {
                        if let Some(value) =
                            json_int(&params["count"], rng, context.scores().as_ref())
                        {
                            count = if params["add"].as_bool() == Some(true) {
                                count + value
                            } else {
                                value
                            };
                        }
                    }
                    "minecraft:limit_count" => {
                        count = limit_count(&params["limit"], count, rng);
                    }
                    "minecraft:furnace_smelt" => {
                        let smelted = match context.entity.and_then(|e| e.recipes.as_deref()) {
                            Some(recipes) => recipes
                                .cooking_recipe(CookingStation::Furnace, item)
                                .map(|(_, recipe)| (recipe.result.item, recipe.result.count)),
                            None => smelting_recipe(item)
                                .map(|recipe| (recipe.result.item, recipe.result.count)),
                        };
                        if let Some((smelted, result_count)) = smelted {
                            item = smelted;
                            let input = if params["use_input_count"].as_bool() == Some(false) {
                                1
                            } else {
                                count
                            };
                            count = input * result_count as i64;
                        }
                    }
                    "minecraft:enchanted_count_increase" => {
                        let Some(enchantment) = params["enchantment"].as_str() else {
                            continue;
                        };
                        let level = context
                            .entity
                            .and_then(|e| e.attacker_weapon.as_deref())
                            .map_or(0, |weapon| weapon.enchantment_level(enchantment));
                        if level > 0 {
                            let per_level =
                                json_number(&params["count"], rng, context.scores().as_ref())
                                    .unwrap_or(0.0);
                            count += (level as f32 * per_level).round() as i64;
                            if let Some(limit) = params["limit"].as_i64().filter(|l| *l > 0)
                                && count > limit
                            {
                                count = limit;
                            }
                        }
                    }
                    "minecraft:apply_bonus" => {
                        let level = tool_enchantment_level(&params, context.tool);
                        count = apply_bonus(&params, count, level, rng);
                    }
                    kind => stack_functions.push((kind, params)),
                }
            }
            let max = i64::from(item.max_stack_size().max(1));
            while count > 0 {
                let chunk = count.min(max);
                count -= chunk;
                #[expect(
                    clippy::disallowed_methods,
                    reason = "loot table output (create_entry_leaf)"
                )]
                let mut stack = ItemStack::mint(item, chunk as u8, MintReason::Loot);
                for (kind, params) in &stack_functions {
                    stack = modify_stack(stack, kind, params, context, rng);
                }
                let mut stack = [stack];
                if applies_decay(&entry.functions) {
                    decay(&mut stack, context.explosion_radius, rng);
                }
                copy_components(&entry.functions, &mut stack, context, rng);
                let [stack] = stack;
                if stack.count() > 0 {
                    drops.push(stack);
                }
            }
        }
        LootEntryKind::Inline(table) => roll_table(table, context, rng, drops),
        LootEntryKind::Dynamic(name) if &**name == "minecraft:contents" => {
            if let Some(items) = context
                .block_entity
                .and_then(|nbt| nbt.get("Items"))
                .and_then(Tag::as_list)
            {
                drops.extend(functions::item_stacks(items));
            }
        }
        LootEntryKind::Dynamic(name) if &**name == "minecraft:sherds" => {
            match context.block_entity.and_then(|nbt| nbt.get("sherds")) {
                Some(sherds) => {
                    if let Some([back, left, right, front]) =
                        tuff_item::item::pot_decorations_from_nbt(sherds)
                    {
                        #[expect(
                            clippy::disallowed_methods,
                            reason = "loot table output (create_entry_leaf)"
                        )]
                        drops.extend([left, back, front, right].into_iter().flatten().map(
                            |side| ItemStack::mint_like(&side, side.count(), MintReason::Loot),
                        ));
                    }
                }
                #[expect(
                    clippy::disallowed_methods,
                    reason = "loot table output (create_entry_leaf)"
                )]
                None => {
                    drops.extend((0..4).map(|_| ItemStack::mint(Item::Brick, 1, MintReason::Loot)))
                }
            }
        }
        LootEntryKind::Tag { tag, expand, .. } => {
            let items =
                tuff_datapack::tags::with(|tags| tags.typed::<Item>(tag)).unwrap_or_default();
            let chosen: Vec<Item> = if *expand {
                if items.is_empty() {
                    Vec::new()
                } else {
                    vec![items[rng.gen_range(0..items.len())]]
                }
            } else {
                items.to_vec()
            };
            #[expect(
                clippy::disallowed_methods,
                reason = "loot table output (create_entry_leaf)"
            )]
            drops.extend(
                chosen
                    .into_iter()
                    .map(|item| ItemStack::mint(item, 1, MintReason::Loot)),
            );
        }
        LootEntryKind::LootTable(name) => {
            if let Some(table) = context.tables.get(name) {
                roll_table(table, context, rng, drops);
            }
        }
        LootEntryKind::Unknown { kind, raw } if &**kind == "minecraft:slots" => {
            drops.extend(slots::provide(raw, context));
        }
        _ => {}
    }
}

mod conditions;
pub use conditions::*;

#[cfg(test)]
mod tests;
