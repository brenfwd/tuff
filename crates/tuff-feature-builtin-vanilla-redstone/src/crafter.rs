//! Crafters: a rising edge of power schedules a craft four ticks later.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState, Frozen};

use tuff_core::world::PLAYER_HALF_WIDTH;
use tuff_core::world::block_entity::{
    BlockEntityTicker, ContainerKind, State, TickOutcome, TickView,
};
use tuff_core::world::item_port::Porter;
use tuff_core::world::messages::{AdvancementEvent, PlayerEvent};
use tuff_core::world::region_api::{Insert, RegionWorld};
use tuff_item::item::ItemStack;
use tuff_vanilla_common::placement::step;

use crate::dispenser::drop_out;

use super::{
    BlockBehavior, Effect, Reader, RedstoneLevel, SignalBehavior, Signals, TickPriority,
    UPDATE_CLIENTS,
};
use tuff_world_model::BlockPos;

pub struct Crafter;

const CRAFT_DELAY: u32 = 4;

fn triggered(state: BlockState) -> bool {
    state.get("triggered") == Some("true")
}

impl BlockBehavior for Crafter {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        let signal = Signals::new(level).has_neighbor_signal(pos);
        let was = triggered(state);
        if signal && !was {
            level.schedule_tick(pos, Block::Crafter, CRAFT_DELAY, TickPriority::Normal);
            level.set_block(
                pos,
                state.with("triggered", "true").unwrap_or(state),
                UPDATE_CLIENTS,
            );
            level.effect(Effect::CrafterTriggered(pos, true));
        } else if !signal && was {
            let next = state
                .with("triggered", "false")
                .and_then(|s| s.with("crafting", "false"))
                .unwrap_or(state);
            level.set_block(pos, next, UPDATE_CLIENTS);
            level.effect(Effect::CrafterTriggered(pos, false));
        }
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, _state: BlockState) {
        level.effect(Effect::CrafterDispense(pos));
    }

    fn placement(&self, level: &dyn Reader, pos: BlockPos, state: BlockState) -> BlockState {
        let signal = Signals::new(level).has_neighbor_signal(pos);
        state
            .with("triggered", if signal { "true" } else { "false" })
            .unwrap_or(state)
    }

    fn placed(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if triggered(state) {
            level.schedule_tick(pos, Block::Crafter, CRAFT_DELAY, TickPriority::Normal);
        }
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for Crafter {}

const CRAFTING_TICKS: i32 = 6;
const LEVEL_EVENT_CRAFT: i32 = 1049;
const LEVEL_EVENT_FAIL: i32 = 1050;
const LEVEL_EVENT_SMOKE: i32 = 2010;
const REPORT_HALF: f64 = 8.5;

fn front(state: BlockState) -> Facing {
    let name = state
        .get("orientation")
        .and_then(|orientation| orientation.split('_').next())
        .unwrap_or("north");
    [
        Facing::Down,
        Facing::Up,
        Facing::North,
        Facing::South,
        Facing::West,
        Facing::East,
    ]
    .into_iter()
    .find(|facing| facing.name() == name)
    .unwrap_or(Facing::North)
}

/// Crafts from the grid once and pushes the result out of the front.
pub(crate) fn craft(world: &mut dyn RegionWorld, pos: BlockPos) {
    let Some(state) = world.block(pos).filter(|s| s.block() == Block::Crafter) else {
        return;
    };
    if world.container_kind(pos) != Some(ContainerKind::Crafter) {
        return;
    }
    let recipes = world.shared().recipes();
    let mut ingredients: Vec<tuff_item::item::ItemSnapshot> = Vec::new();
    let mut crafted = None;
    let edited = world.edit_container(pos, &mut |items| {
        ingredients = items
            .iter()
            .flatten()
            .map(tuff_item::item::ItemSnapshot::of)
            .collect();
        crafted = recipes.crafter_craft(items);
    });
    if !edited {
        return;
    }
    let Some(crafted) = crafted else {
        world.level_event(LEVEL_EVENT_FAIL, pos, 0);
        return;
    };
    if let Some(State::Crafter(crafter)) = world.block_entity_state_mut(pos) {
        crafter.crafting_ticks = CRAFTING_TICKS;
    }
    if let Some(next) = state.with("crafting", "true") {
        world.set_block_flags(pos, next, UPDATE_CLIENTS);
    }
    let direction = front(state);
    let made = (&crafted.recipe, ingredients.as_slice());
    dispense(world, pos, direction, crafted.result, made);
    for remainder in crafted.remainders {
        dispense(world, pos, direction, remainder, made);
    }
}

fn dispense(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    direction: Facing,
    stack: ItemStack,
    (recipe, ingredients): (&Frozen<str>, &[tuff_item::item::ItemSnapshot]),
) {
    let target = step(pos, direction);
    let face = Some(direction.opposite());
    let mut remaining = Some(stack);
    if let Some(kind) = world.container_kind(target) {
        let one_at_a_time = kind == ContainerKind::Crafter
            || remaining
                .as_ref()
                .is_some_and(|stack| stack.count() > stack.max_stack_size());
        let by = Porter::Dropper(pos);
        if one_at_a_time {
            while let Some(mut stack) = remaining.take() {
                let one = stack.take(1);
                match world.insert_item(target, face, one, by) {
                    Insert::Stored => {}
                    Insert::Nothing(back) | Insert::Refused(back) => {
                        if let Err(back) = stack.join(back) {
                            drop_out(world, pos, direction, back);
                        }
                        remaining = Some(stack);
                        break;
                    }
                }
                if !stack.is_empty() {
                    remaining = Some(stack);
                }
            }
        } else if let Some(stack) = remaining.take() {
            remaining = match world.insert_item(target, face, stack, by) {
                Insert::Stored => None,
                Insert::Nothing(rest) | Insert::Refused(rest) => Some(rest),
            };
        }
    }
    if let Some(rest) = remaining {
        drop_out(world, pos, direction, rest);
        report_crafted(world, pos, recipe, ingredients);
        world.level_event(LEVEL_EVENT_CRAFT, pos, 0);
        world.level_event(LEVEL_EVENT_SMOKE, pos, direction.id() as i32);
    }
}

/// Tells the players within a 17-block cube around the crafter what it
/// crafted.
fn report_crafted(
    world: &dyn RegionWorld,
    pos: BlockPos,
    recipe: &Frozen<str>,
    ingredients: &[tuff_item::item::ItemSnapshot],
) {
    let [cx, cy, cz] = pos.center();
    for player in world.present_players() {
        let [x, y, z] = player.position;
        let inside = x + PLAYER_HALF_WIDTH > cx - REPORT_HALF
            && x - PLAYER_HALF_WIDTH < cx + REPORT_HALF
            && z + PLAYER_HALF_WIDTH > cz - REPORT_HALF
            && z - PLAYER_HALF_WIDTH < cz + REPORT_HALF
            && y + player.height > cy - REPORT_HALF
            && y < cy + REPORT_HALF;
        if inside {
            player.handle.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::CrafterRecipeCrafted {
                    recipe: recipe.clone(),
                    ingredients: ingredients.to_vec(),
                },
            )));
        }
    }
}

/// Counts a crafter's crafting cooldown down and clears its `crafting`
/// state when it ends.
pub struct CrafterTicker;

impl BlockEntityTicker for CrafterTicker {
    fn tick(&self, view: &mut TickView) -> TickOutcome {
        let mut outcome = TickOutcome::default();
        let State::Crafter(crafter) = view.data else {
            return outcome;
        };
        if crafter.crafting_ticks <= 0 {
            return outcome;
        }
        crafter.crafting_ticks -= 1;
        if crafter.crafting_ticks == 0 {
            outcome.set_state = view.state.with("crafting", "false");
        }
        outcome
    }
}
