//! Composters and decorated pots as item ports. A composter takes
//! compostable items from above (hoppers, droppers), from players and from
//! farmers, adding layers by chance; a ripe one gives its bone meal to a
//! hopper or hopper minecart below, or pops it out when a player or mob
//! empties it. A decorated pot takes one more of the item it holds.

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, Item};

use tuff_core::world::game_event::{Context, GameEvent, Source};
use tuff_core::world::item_port::{ItemPort, PortAccess, Porter};
use tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY;
use tuff_core::world::redstone::UPDATE_ALL;
use tuff_core::world::region_api::RegionWorld;
use tuff_item::item::{ItemSnapshot, ItemStack, MintReason};
use tuff_nbt::Compound;
use tuff_vanilla_common::composter::{COMPOSTER_FILL, FULL, READY, compostable, level, with_level};
use tuff_world_model::BlockPos;

const POT_ITEM: &str = "item";

fn source(by: Porter) -> Option<Source> {
    match by {
        Porter::Mob(id) => Some(Source::entity(id)),
        Porter::Player(Some(id)) => Some(Source::player(id)),
        _ => None,
    }
}

/// Whether `at` reaches the composter through a face that lets `face`
/// through: blocks and carts only through it, players and mobs from
/// anywhere.
fn through(at: &PortAccess, face: Facing) -> bool {
    match at.by {
        Porter::Hopper(_) | Porter::Dropper(_) | Porter::Cart(_) => at.face == Some(face),
        Porter::Mob(_) | Porter::Player(_) => true,
    }
}

fn changed(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: tuff_vanilla_data::BlockState,
    by: Porter,
) {
    let context = Context {
        source: source(by),
        affected: Some(state),
    };
    world.game_event(GameEvent::BlockChange, pos.center(), context);
}

/// Puts one of `stack` into the composter at `pos`: `None` if it does not
/// take it (not compostable, or full), else whether a layer was added. The
/// item is used up either way.
fn compost(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    stack: &ItemStack,
    by: Porter,
) -> Option<bool> {
    let state = world.block(pos).filter(|_| world.owns(pos))?;
    let fill = level(state).filter(|fill| *fill < FULL)?;
    let provider = compostable(stack)?;
    let rng = world.rng();
    let layers = provider.get(&mut tuff_vanilla_data::provider::Context {
        block: Some(state),
        random: &mut |bound| rng.gen_range(0..bound.max(1)),
    });
    let raised = layers > 0;
    if raised {
        let next = with_level(state, (i32::from(fill) + layers).clamp(0, 7) as u8);
        world.set_block_flags(pos, next, UPDATE_ALL);
        changed(world, pos, next, by);
    }
    world.broadcast_level_event(COMPOSTER_FILL, pos, i32::from(raised));
    Some(raised)
}

/// Pops one bone meal out of the top of the composter at `pos`.
fn eject(world: &mut dyn RegionWorld, pos: BlockPos) {
    let rng = world.rng();
    let dx = f64::from((rng.r#gen::<f32>() - 0.5) * 0.7);
    let dz = f64::from((rng.r#gen::<f32>() - 0.5) * 0.7);
    let vx = rng.r#gen::<f64>() * 0.2 - 0.1;
    let vz = rng.r#gen::<f64>() * 0.2 - 0.1;
    let position = [
        f64::from(pos.x) + 0.5 + dx,
        f64::from(pos.y) + 1.01,
        f64::from(pos.z) + 0.5 + dz,
    ];
    world.spawn_item_at(
        ItemStack::mint(Item::BoneMeal, 1, MintReason::BlockDrop),
        position,
        [vx, 0.2, vz],
        BLOCK_DROP_PICKUP_DELAY,
    );
    world.play_sound(SoundEvent::BlockComposterEmpty, pos, 1.0, 1.0, None);
}

pub struct ComposterPort;

impl ItemPort for ComposterPort {
    fn offer(
        &self,
        world: &mut dyn RegionWorld,
        at: &PortAccess,
        stack: ItemStack,
    ) -> Result<(), ItemStack> {
        if !through(at, Facing::Up) || compost(world, at.pos, &stack, at.by).is_none() {
            return Err(stack);
        }
        stack.burn(tuff_item::item::BurnReason::Consumed);
        Ok(())
    }

    fn peek(&self, world: &dyn RegionWorld, at: &PortAccess) -> Option<ItemSnapshot> {
        ripe(world, at).then(|| ItemSnapshot::describe(Item::BoneMeal, 1, Default::default()))
    }

    fn take(&self, world: &mut dyn RegionWorld, at: &PortAccess) -> Option<ItemStack> {
        if !ripe(world, at) {
            return None;
        }
        let meal = ItemStack::mint(Item::BoneMeal, 1, MintReason::BlockDrop);
        let state = world.block(at.pos)?;
        let by_hand = matches!(at.by, Porter::Mob(_) | Porter::Player(_));
        if by_hand {
            eject(world, at.pos);
        }
        let empty = with_level(state, 0);
        world.set_block_flags(at.pos, empty, UPDATE_ALL);
        changed(world, at.pos, empty, at.by);
        if by_hand {
            meal.burn(tuff_item::item::BurnReason::Template);
            return None;
        }
        Some(meal)
    }
}

fn ripe(world: &dyn RegionWorld, at: &PortAccess) -> bool {
    world.owns(at.pos)
        && world.block(at.pos).and_then(level) == Some(READY)
        && through(at, Facing::Down)
}

pub struct PotPort;

impl ItemPort for PotPort {
    fn offer(
        &self,
        world: &mut dyn RegionWorld,
        at: &PortAccess,
        stack: ItemStack,
    ) -> Result<(), ItemStack> {
        if !world.owns(at.pos)
            || world.block(at.pos).map(|s| s.block()) != Some(Block::DecoratedPot)
        {
            return Err(stack);
        }
        let Some(nbt) = world.raw_block_entity_mut(at.pos) else {
            return Err(stack);
        };
        let merged = match nbt
            .get_compound(POT_ITEM)
            .and_then(ItemStack::picture_from_nbt)
        {
            None => stack,
            Some(mut held) => {
                if !held.stacks_with(&stack) || held.count() >= held.max_stack_size() {
                    return Err(stack);
                }
                held.join(stack)?;
                held
            }
        };
        let compound: Compound = merged.to_nbt();
        tuff_item::item::conservation::release(merged);
        nbt.insert(POT_ITEM, compound);
        world.mark_blocks_dirty(at.pos);
        Ok(())
    }

    fn peek(&self, _world: &dyn RegionWorld, _at: &PortAccess) -> Option<ItemSnapshot> {
        None
    }

    fn take(&self, _world: &mut dyn RegionWorld, _at: &PortAccess) -> Option<ItemStack> {
        None
    }
}
