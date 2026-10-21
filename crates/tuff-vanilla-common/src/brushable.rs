//! Suspicious sand and gravel: brushing uncovers the item they hold, their
//! `dusted` stage follows the strokes and falls back once brushing stops,
//! and they fall like sand but shatter where they land.

use std::collections::HashMap;

use rand::SeedableRng;
use rand::rngs::StdRng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState, Item};

use tuff_core::protocol::play;
use tuff_core::world::block_entity::raw::{LOOT_TABLE, LOOT_TABLE_SEED};
use tuff_core::world::messages::{AdvancementEvent, PlayerEvent, PlayerHandle};
use tuff_core::world::region_api::{LEVEL_EVENT_RANGE, RegionWorld};
use tuff_core::world::{framed, packets, redstone};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

/// Ticks between a brushable block's checks for falling and decay.
pub const TICK_DELAY: u32 = 2;
/// The use duration of a brush.
pub const BRUSH_USE_TICKS: u32 = 200;
/// A brush stroke lands every this many ticks of use.
pub const STROKE_INTERVAL: u32 = 10;
const COOLDOWN_TICKS: u64 = 10;
const RESET_TICKS: u64 = 40;
const DECAY_INTERVAL: u64 = 4;
const BRUSHES_TO_COMPLETE: u32 = 10;
const HIT_DIRECTION: &str = "hit_direction";
const ITEM: &str = "item";

pub fn is_brushable(block: Block) -> bool {
    matches!(block, Block::SuspiciousSand | Block::SuspiciousGravel)
}

/// What a fully brushed block becomes.
pub fn turns_into(block: Block) -> Block {
    match block {
        Block::SuspiciousSand => Block::Sand,
        Block::SuspiciousGravel => Block::Gravel,
        _ => Block::Air,
    }
}

/// The sound of a brush stroke on `block`.
pub fn brush_sound(block: Block) -> SoundEvent {
    match block {
        Block::SuspiciousSand => SoundEvent::ItemBrushBrushingSand,
        Block::SuspiciousGravel => SoundEvent::ItemBrushBrushingGravel,
        _ => SoundEvent::ItemBrushBrushingGeneric,
    }
}

/// Whether the brush use tick with `remaining` of `total` ticks left is a
/// stroke (the last tick before the backswing).
pub fn is_stroke(total: u32, remaining: u32) -> bool {
    (total - remaining + 1) % STROKE_INTERVAL == STROKE_INTERVAL / 2
}

fn legacy_direction(face: Facing) -> i8 {
    match face {
        Facing::Down => 0,
        Facing::Up => 1,
        Facing::North => 2,
        Facing::South => 3,
        Facing::West => 4,
        Facing::East => 5,
    }
}

fn from_legacy_direction(id: i8) -> Facing {
    match id {
        0 => Facing::Down,
        2 => Facing::North,
        3 => Facing::South,
        4 => Facing::West,
        5 => Facing::East,
        _ => Facing::Up,
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Progress {
    count: u32,
    resets_at: u64,
    cooldown_ends: u64,
}

impl Progress {
    fn stage(self) -> u8 {
        match self.count {
            0 => 0,
            1..3 => 1,
            3..6 => 2,
            _ => 3,
        }
    }
}

/// The runtime brushing progress of a region's brushable blocks (never
/// saved, as in the game).
#[derive(Default)]
pub struct Brushing(HashMap<BlockPos, Progress>);

fn progress(world: &mut dyn RegionWorld, pos: BlockPos) -> Progress {
    world
        .state()
        .get::<Brushing>()
        .0
        .get(&pos)
        .copied()
        .unwrap_or_default()
}

fn store(world: &mut dyn RegionWorld, pos: BlockPos, progress: Progress) {
    let map = &mut world.state().get::<Brushing>().0;
    if progress == Progress::default() {
        map.remove(&pos);
    } else {
        map.insert(pos, progress);
    }
}

/// Forgets the brushing progress at `pos` (the block left).
pub fn forget(world: &mut dyn RegionWorld, pos: BlockPos) {
    world.state().get::<Brushing>().0.remove(&pos);
}

/// The brushing stage `dusted` of `state` set to `stage`, then the block
/// entity resent so viewers see the uncovered item.
fn set_stage(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState, stage: u8) {
    let Some(new) = state.with("dusted", &stage.to_string()) else {
        return;
    };
    world.set_block_flags(pos, new, redstone::UPDATE_ALL);
    world.block_entity_changed(pos);
}

/// Rolls a pending archaeology loot table into the block's item.
fn unpack_loot(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: BlockState,
    player: &PlayerHandle,
) {
    let Some((table, seed)) = world.raw_block_entity(pos).and_then(|nbt| {
        let table = nbt.get_str(LOOT_TABLE)?.to_owned();
        Some((table, nbt.get_long(LOOT_TABLE_SEED).unwrap_or(0)))
    }) else {
        return;
    };
    let loot = world.shared().block_loot();
    let rolled = if seed != 0 {
        loot.table(
            &table,
            state,
            Some(Item::Brush),
            &mut StdRng::seed_from_u64(seed as u64),
        )
    } else {
        loot.table(&table, state, Some(Item::Brush), world.rng())
    };
    let Some(nbt) = world.raw_block_entity_mut(pos) else {
        return;
    };
    nbt.remove(LOOT_TABLE);
    nbt.remove(LOOT_TABLE_SEED);
    match rolled.into_iter().next() {
        Some(stack) => {
            nbt.insert(ITEM, stack.to_nbt());
        }
        None => {
            nbt.remove(ITEM);
        }
    }
    world.mark_blocks_dirty(pos);
    player.send(PlayerEvent::Advancement(Box::new(
        AdvancementEvent::GeneratedLoot(table),
    )));
}

/// One brush stroke by `player` on the block at `pos` through `face`;
/// true when it finished uncovering the block (the brush wears).
pub fn brush(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    face: Facing,
    player: &PlayerHandle,
) -> bool {
    let Some(state) = world.block(pos) else {
        return false;
    };
    world.play_sound(brush_sound(state.block()), pos, 1.0, 1.0, Some(player.id));
    if !is_brushable(state.block()) || !world.owns(pos) || world.raw_block_entity(pos).is_none() {
        return false;
    }
    if let Some(nbt) = world.raw_block_entity_mut(pos)
        && !nbt.contains_key(HIT_DIRECTION)
    {
        nbt.insert(HIT_DIRECTION, legacy_direction(face));
    }
    let now = world.tick();
    let mut progress = progress(world, pos);
    progress.resets_at = now + RESET_TICKS;
    if now < progress.cooldown_ends {
        store(world, pos, progress);
        return false;
    }
    progress.cooldown_ends = now + COOLDOWN_TICKS;
    unpack_loot(world, pos, state, player);
    let before = progress.stage();
    progress.count += 1;
    if progress.count >= BRUSHES_TO_COMPLETE {
        forget(world, pos);
        complete(world, pos, state, player);
        return true;
    }
    store(world, pos, progress);
    world.schedule_tick(pos, state.block(), TICK_DELAY);
    let after = progress.stage();
    if before != after {
        set_stage(world, pos, state, after);
    }
    false
}

/// The last stroke: the item pops out of the brushed face and the block
/// turns into plain sand or gravel.
fn complete(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState, player: &PlayerHandle) {
    unpack_loot(world, pos, state, player);
    let (item, face) = match world.raw_block_entity_mut(pos) {
        Some(nbt) => (
            nbt.remove(ITEM)
                .and_then(|tag| tag.as_compound().and_then(ItemStack::from_nbt)),
            nbt.get_byte(HIT_DIRECTION).map(from_legacy_direction),
        ),
        None => (None, None),
    };
    if let Some(mut stack) = item {
        let take: u8 = rand::Rng::gen_range(world.rng(), 10..=30);
        let (dx, dy, dz) = face.unwrap_or(Facing::Up).step();
        let drop = BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz);
        stack.count = stack.count.min(take);
        let position = [
            f64::from(drop.x) + 0.5,
            f64::from(drop.y) + 0.625,
            f64::from(drop.z) + 0.5,
        ];
        world.spawn_item_at(stack, position, [0.0; 3], 0);
    }
    let packet = framed(&play::LevelEvent {
        event: play::level_event::PARTICLES_AND_SOUND_BRUSH_BLOCK_COMPLETE,
        position: pos,
        data: state.id() as i32,
        global: false,
    });
    world.send_near(pos.center(), LEVEL_EVENT_RANGE, None, packets([packet]));
    world.set_block_flags(
        pos,
        turns_into(state.block()).default_state(),
        redstone::UPDATE_ALL,
    );
}

/// The brushable block's scheduled tick: brushing that stopped for two
/// seconds loses two strokes every four ticks until it is undone.
pub fn check_reset(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
    let mut progress = progress(world, pos);
    let now = world.tick();
    if progress.count != 0 && now >= progress.resets_at {
        let before = progress.stage();
        progress.count = progress.count.saturating_sub(2);
        let after = progress.stage();
        if before != after {
            set_stage(world, pos, state, after);
        }
        progress.resets_at = now + DECAY_INTERVAL;
    }
    if progress.count == 0 {
        forget(world, pos);
        if let Some(nbt) = world.raw_block_entity_mut(pos)
            && nbt.remove(HIT_DIRECTION).is_some()
        {
            world.mark_blocks_dirty(pos);
        }
        return;
    }
    store(world, pos, progress);
    world.schedule_tick(pos, state.block(), TICK_DELAY);
}

/// The item a brushable block entity holds.
pub fn held_item(nbt: &Compound) -> Option<ItemStack> {
    nbt.get(ITEM)
        .and_then(|tag| tag.as_compound())
        .and_then(ItemStack::from_nbt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn strokes_land_on_the_fifth_tick_of_every_ten() {
        let strokes: Vec<u32> = (1..=BRUSH_USE_TICKS)
            .rev()
            .filter(|remaining| is_stroke(BRUSH_USE_TICKS, *remaining))
            .map(|remaining| BRUSH_USE_TICKS - remaining + 1)
            .take(3)
            .collect();
        assert_eq!(strokes, [5, 15, 25]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn stages_follow_the_stroke_count() {
        let stages: Vec<u8> = (0..10)
            .map(|count| {
                Progress {
                    count,
                    ..Progress::default()
                }
                .stage()
            })
            .collect();
        assert_eq!(stages, [0, 1, 1, 2, 2, 2, 3, 3, 3, 3]);
    }
}
