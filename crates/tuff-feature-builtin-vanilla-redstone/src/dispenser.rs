//! Dispensers and droppers: firing one item from a random filled slot.

use rand::Rng;
use tuff_item::item::BurnReason;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::redstone::Dispense;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::world::item_port::Porter;
use tuff_core::world::messages::RegionInternal;
use tuff_core::world::physics::triangle;
use tuff_core::world::region_api::{
    BlockCause, DispenseOutcome, Dispensers, Fired, Insert, RegionTask, RegionWorld,
};
use tuff_item::item::ItemStack;
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const DISPENSE_ACCURACY: f64 = 6.0;
const LEVEL_EVENT_DISPENSE: i32 = 1000;
const LEVEL_EVENT_FAIL: i32 = 1001;
const LEVEL_EVENT_LAUNCH: i32 = 1002;
const LEVEL_EVENT_SMOKE: i32 = 2000;

pub struct DispenserRuntime;

pub(crate) fn origin(pos: BlockPos, facing: Facing) -> [f64; 3] {
    let [x, y, z] = pos.center();
    let (sx, sy, sz) = facing.step();
    [
        x + 0.7 * f64::from(sx),
        y + 0.7 * f64::from(sy),
        z + 0.7 * f64::from(sz),
    ]
}

fn facing_of(state: BlockState) -> Facing {
    state.facing().unwrap_or(Facing::North)
}

fn slot_stack(
    world: &dyn RegionWorld,
    pos: BlockPos,
    slot: usize,
) -> Option<tuff_item::item::ItemSnapshot> {
    world
        .container_items(pos)?
        .get(slot)?
        .as_ref()
        .map(tuff_item::item::ItemSnapshot::of)
}

impl Dispensers for DispenserRuntime {
    fn fire(&self, world: &mut dyn RegionWorld, pos: BlockPos) {
        if world
            .block(pos)
            .is_some_and(|s| s.block() == Block::Crafter)
        {
            crate::crafter::craft(world, pos);
            return;
        }
        fire(world, pos);
    }
}

fn fire(world: &mut dyn RegionWorld, pos: BlockPos) {
    let Some(state) = world.block(pos) else {
        return;
    };
    let facing = facing_of(state);
    let dropper = state.block() == Block::Dropper;
    let Some(items) = world.container_items(pos) else {
        return;
    };
    let filled: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, stack)| stack.is_some())
        .map(|(i, _)| i)
        .collect();
    let mut slot = None;
    for (odds, i) in (1..).zip(filled) {
        if world.rng().gen_range(0..odds) == 0 {
            slot = Some(i);
        }
    }
    let Some(slot) = slot else {
        world.level_event(LEVEL_EVENT_FAIL, pos, 0);
        return;
    };
    let Some(stack) = slot_stack(world, pos, slot) else {
        return;
    };
    let target = step(pos, facing);
    if dropper && drop_into_target(world, pos, slot, facing, target, state, &stack) {
        return;
    }
    let behaviour = if dropper {
        Some(Dispense::Default)
    } else {
        stack.item.dispense()
    };
    if behaviour == Some(Dispense::Default) {
        complete(world, pos, slot, &stack, None, DispenseOutcome::Dropped);
        return;
    }
    let one = take_one(world, pos, slot);
    let fired = Fired::new(stack, one);
    let outcome = run_hooks(world, pos, facing, &fired);
    let kept = fired.taken();
    let (stack, one) = fired.into_parts();
    let one = (!kept).then_some(one).flatten();
    let outcome = match outcome {
        Some(outcome) => outcome,
        None if behaviour.is_none() => DispenseOutcome::Dropped,
        None => {
            put_back(world, pos, facing, slot, one);
            return;
        }
    };
    if outcome == DispenseOutcome::Remote {
        forward(world, pos, facing, slot, stack, one);
        return;
    }
    let kept = kept_or(one, kept, world, pos, facing, slot);
    complete(world, pos, slot, &stack, kept, outcome);
}

/// `Some(())` when a hook kept the fired item; otherwise puts the item
/// back into its slot first.
fn kept_or(
    one: Option<ItemStack>,
    kept: bool,
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    facing: Facing,
    slot: usize,
) -> Option<()> {
    if kept {
        return Some(());
    }
    put_back(world, pos, facing, slot, one);
    None
}

fn put_back(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    facing: Facing,
    slot: usize,
    one: Option<ItemStack>,
) {
    let Some(one) = one else {
        return;
    };
    let mut back = Some(one);
    world.edit_container_slot(pos, slot, &mut |held| {
        let fits = match (held.as_ref(), back.as_ref()) {
            (None, _) => true,
            (Some(held), Some(back)) => {
                held.stacks_with(back) && held.count() < held.item.max_stack_size()
            }
            (Some(_), None) => false,
        };
        if fits && let Some(back) = back.take() {
            ItemStack::put_back(held, back);
        }
    });
    if let Some(back) = back {
        drop_out(world, pos, facing, back);
    }
}

fn run_hooks(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    facing: Facing,
    stack: &Fired,
) -> Option<DispenseOutcome> {
    let at = origin(pos, facing);
    let hooks = world.shared().region_hooks.clone();
    hooks
        .iter()
        .find_map(|hook| hook.dispense(world, pos, facing, at, stack))
}

fn drop_into_target(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    slot: usize,
    facing: Facing,
    target: BlockPos,
    state: BlockState,
    stack: &ItemStack,
) -> bool {
    let mut one = None;
    world.edit_container_slot(pos, slot, &mut |held| {
        if held.as_ref().is_some_and(|held| held.stacks_with(stack)) {
            one = ItemStack::take_from(held, 1);
        }
    });
    let Some(one) = one else {
        return false;
    };
    let block = state.block();
    let inserted = world.caused(BlockCause::Block(block), |world| {
        world.insert_item(target, Some(facing.opposite()), one, Porter::Dropper(pos))
    });
    let (back, stored) = match inserted {
        Insert::Nothing(back) => (Some(back), false),
        Insert::Stored => (None, true),
        Insert::Refused(back) => (Some(back), true),
    };
    if let Some(back) = back {
        let mut back = Some(back);
        world.edit_container_slot(pos, slot, &mut |held| {
            if let Some(back) = back.take() {
                ItemStack::put_back(held, back);
            }
        });
        if let Some(back) = back {
            drop_out(world, pos, facing, back);
        }
    }
    stored
}

fn forward(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    facing: Facing,
    slot: usize,
    stack: tuff_item::item::ItemSnapshot,
    one: Option<ItemStack>,
) {
    let target = step(pos, facing);
    let Some(neighbour) = world.neighbor(ChunkPos::of_block(target).region()) else {
        put_back(world, pos, facing, slot, one);
        complete(world, pos, slot, &stack, None, DispenseOutcome::Dropped);
        return;
    };
    let reply = world.handle();
    let carried = std::sync::Arc::new(std::sync::Mutex::new(one));
    let sent = carried.clone();
    let task = RegionTask::new(move |other| {
        let one = sent.lock().ok().and_then(|mut one| one.take());
        let fired = Fired::new(stack, one);
        let outcome = run_hooks(other, pos, facing, &fired)
            .filter(|outcome| *outcome != DispenseOutcome::Remote)
            .unwrap_or(DispenseOutcome::Dropped);
        let kept = fired.taken();
        let (stack, one) = fired.into_parts();
        reply.internal(RegionInternal::Run(RegionTask::new(move |world| {
            let facing = world.block(pos).map_or(Facing::North, facing_of);
            let kept = kept_or(one, kept, world, pos, facing, slot);
            complete(world, pos, slot, &stack, kept, outcome);
        })));
    });
    if !neighbour.internal(RegionInternal::Run(task)) {
        let one = carried.lock().ok().and_then(|mut one| one.take());
        put_back(world, pos, facing, slot, one);
    }
}

/// Finishes a dispense once its hooks ran; `kept` when a hook kept the
/// fired item, which then is not taken from the slot again.
fn complete(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    slot: usize,
    stack: &ItemStack,
    kept: Option<()>,
    outcome: DispenseOutcome,
) {
    let Some(state) = world.block(pos) else {
        return;
    };
    let facing = facing_of(state);
    if kept.is_none() && !slot_stack(world, pos, slot).is_some_and(|held| held.item == stack.item)
    {
        return;
    }
    let sound = match outcome {
        DispenseOutcome::Dropped | DispenseOutcome::DroppedFailing => {
            if let Some(one) = take_one(world, pos, slot) {
                drop_out(world, pos, facing, one);
            }
            if outcome == DispenseOutcome::Dropped {
                LEVEL_EVENT_DISPENSE
            } else {
                LEVEL_EVENT_FAIL
            }
        }
        DispenseOutcome::Dispensed => {
            if kept.is_none() {
                take(world, pos, slot);
            }
            LEVEL_EVENT_DISPENSE
        }
        DispenseOutcome::Launched { sound } => {
            if kept.is_none() {
                take(world, pos, slot);
            }
            sound.unwrap_or(LEVEL_EVENT_LAUNCH)
        }
        DispenseOutcome::Kept => LEVEL_EVENT_DISPENSE,
        DispenseOutcome::Failed | DispenseOutcome::Remote => LEVEL_EVENT_FAIL,
        DispenseOutcome::Worn(amount) => {
            wear(world, pos, slot, amount);
            LEVEL_EVENT_DISPENSE
        }
        DispenseOutcome::Exchanged(remainder) => {
            if stack.count() == 1 {
                let mut remainder = Some(remainder);
                world.edit_container_slot(pos, slot, &mut |held| {
                    if let Some(old) = std::mem::replace(held, remainder.take()) {
                        old.burn(BurnReason::Consumed);
                    }
                });
            } else {
                take(world, pos, slot);
                give_back(world, pos, facing, state, remainder);
            }
            LEVEL_EVENT_DISPENSE
        }
    };
    world.level_event(sound, pos, 0);
    world.level_event(LEVEL_EVENT_SMOKE, pos, facing.id() as i32);
}

fn wear(world: &mut dyn RegionWorld, pos: BlockPos, slot: usize, amount: u32) {
    let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(world.rng().r#gen());
    world.edit_container_slot(pos, slot, &mut |held| {
        if let Some(stack) = held {
            stack.hurt(amount, &mut rng);
        }
        if held.as_ref().is_some_and(|stack| stack.count() == 0)
            && let Some(broken) = held.take()
        {
            broken.burn(BurnReason::Broken);
        }
    });
}

fn take(world: &mut dyn RegionWorld, pos: BlockPos, slot: usize) {
    if let Some(one) = take_one(world, pos, slot) {
        one.burn(BurnReason::Consumed);
    }
}

fn take_one(world: &mut dyn RegionWorld, pos: BlockPos, slot: usize) -> Option<ItemStack> {
    let mut one = None;
    world.edit_container_slot(pos, slot, &mut |held| one = ItemStack::take_from(held, 1));
    one
}

fn give_back(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    facing: Facing,
    state: BlockState,
    stack: ItemStack,
) {
    let inserted = world.caused(BlockCause::Block(state.block()), |world| {
        world.insert_item(pos, None, stack, Porter::Player(None))
    });
    let rest = match inserted {
        Insert::Stored => return,
        Insert::Nothing(rest) | Insert::Refused(rest) => rest,
    };
    drop_out(world, pos, facing, rest);
    world.level_event(LEVEL_EVENT_DISPENSE, pos, 0);
    world.level_event(LEVEL_EVENT_SMOKE, pos, facing.id() as i32);
}

pub(crate) fn drop_out(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    facing: Facing,
    stack: ItemStack,
) {
    let [x, y, z] = origin(pos, facing);
    let y = if facing.is_horizontal() {
        y - 0.15625
    } else {
        y - 0.125
    };
    let speed = world.rng().r#gen::<f64>() * 0.1 + 0.2;
    let spread = 0.017_227_5 * DISPENSE_ACCURACY;
    let (sx, _, sz) = facing.step();
    let rng = world.rng();
    let velocity = [
        triangle(rng, f64::from(sx) * speed, spread),
        triangle(rng, 0.2, spread),
        triangle(rng, f64::from(sz) * speed, spread),
    ];
    world.spawn_item_at(stack, [x, y, z], velocity, 0);
}
