//! Jukeboxes, kept as saved NBT: a record plays for its song's length,
//! announcing itself every second to listeners and powering redstone
//! around it, and hoppers and droppers can load and unload it.

use std::collections::HashSet;

use rand::Rng;
use tuff_core::protocol::play;
use tuff_core::world::block_entity::analog::song_id;
use tuff_core::world::game_event::{Context, GameEvent};
use tuff_core::world::region_api::{PARTICLE_RANGE, RegionWorld};
use tuff_core::world::{framed, packets, redstone};
use tuff_item::item::ItemStack;
use tuff_nbt::Tag;
use tuff_vanilla_common::jukebox::{RECORD, TICKS, is_playing, plays, record};
use tuff_vanilla_data::registry::ParticleType;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

const PLAY_EVENT_INTERVAL: i64 = 20;
/// A song counts as finished this long after its last note.
const TAIL_TICKS: i64 = 20;
const EJECT_PICKUP_DELAY: u32 = 10;

/// Jukeboxes this region knows to be playing, ticked in position order.
#[derive(Default)]
pub struct Playing(HashSet<BlockPos>);

fn song_ticks(record: &ItemStack) -> Option<i64> {
    let name = tuff_vanilla_data::item::jukebox_song_name(song_id(record)?)?;
    tuff_vanilla_data::item::jukebox_song(name).map(|(_, ticks)| i64::from(ticks))
}

pub fn is_jukebox<W: RegionWorld + ?Sized>(world: &W, pos: BlockPos) -> bool {
    world
        .block(pos)
        .is_some_and(|s| s.block() == Block::Jukebox)
}

fn jukebox_at<W: RegionWorld + ?Sized>(world: &W, pos: BlockPos) -> Option<BlockState> {
    world
        .block(pos)
        .filter(|state| state.block() == Block::Jukebox && world.owns(pos))
}

/// Whether the jukebox at `pos` is empty and takes `stack`.
pub fn accepts<W: RegionWorld + ?Sized>(world: &W, pos: BlockPos, stack: &ItemStack) -> bool {
    jukebox_at(world, pos).is_some()
        && plays(stack)
        && world
            .raw_block_entity(pos)
            .is_some_and(|nbt| !nbt.contains_key(RECORD))
}

fn song_changed<W: RegionWorld + ?Sized>(world: &mut W, pos: BlockPos) {
    world.update_neighbors_at(pos, Block::Jukebox);
    world.mark_blocks_dirty(pos);
    world.update_output_signal(pos, Block::Jukebox);
}

fn set_has_record<W: RegionWorld + ?Sized>(
    world: &mut W,
    pos: BlockPos,
    state: BlockState,
    has: bool,
) {
    let Some(new) = state.with("has_record", if has { "true" } else { "false" }) else {
        return;
    };
    world.set_block_flags(pos, new, redstone::UPDATE_CLIENTS);
    world.game_event(GameEvent::BlockChange, pos.center(), Context::block(new));
}

/// Puts `record` (one item) into the empty jukebox at `pos` and starts
/// its song. False when there is no empty jukebox there.
pub fn insert<W: RegionWorld + ?Sized>(world: &mut W, pos: BlockPos, record: ItemStack) -> bool {
    let Some(state) = jukebox_at(world, pos) else {
        return false;
    };
    let Some(nbt) = world.raw_block_entity_mut(pos) else {
        return false;
    };
    if nbt.contains_key(RECORD) {
        return false;
    }
    nbt.insert(RECORD, record.to_nbt());
    let song = song_id(&record);
    if song.is_some() {
        nbt.insert(TICKS, Tag::Long(0));
    }
    set_has_record(world, pos, state, true);
    if let Some(song) = song {
        world.level_event(play::level_event::SOUND_PLAY_JUKEBOX_SONG, pos, song as i32);
        world.state().get::<Playing>().0.insert(pos);
        song_changed(world, pos);
    } else {
        world.mark_blocks_dirty(pos);
    }
    true
}

/// Swaps the jukebox's item for `record` (any item, as `/item` does): the
/// old song stops and a playable new one starts from the beginning.
pub fn replace<W: RegionWorld + ?Sized>(
    world: &mut W,
    pos: BlockPos,
    record: Option<ItemStack>,
) -> bool {
    let Some(state) = jukebox_at(world, pos) else {
        return false;
    };
    let Some(nbt) = world.raw_block_entity_mut(pos) else {
        return false;
    };
    nbt.remove(RECORD);
    let was_playing = nbt.remove(TICKS).is_some();
    let song = record.as_ref().and_then(song_id);
    if let Some(record) = &record {
        nbt.insert(RECORD, record.to_nbt());
    }
    if song.is_some() {
        nbt.insert(TICKS, Tag::Long(0));
    }
    set_has_record(world, pos, state, record.is_some());
    match song {
        Some(song) => {
            world.level_event(play::level_event::SOUND_PLAY_JUKEBOX_SONG, pos, song as i32);
            world.state().get::<Playing>().0.insert(pos);
        }
        None => {
            world.state().get::<Playing>().0.remove(&pos);
            if was_playing {
                announce_stop(world, pos, state);
            }
        }
    }
    song_changed(world, pos);
    true
}

/// Takes the record out of the jukebox at `pos`, stopping its song.
pub fn take<W: RegionWorld + ?Sized>(world: &mut W, pos: BlockPos) -> Option<ItemStack> {
    let state = jukebox_at(world, pos)?;
    let nbt = world.raw_block_entity_mut(pos)?;
    let record = nbt.remove(RECORD)?;
    let record = record.as_compound().and_then(ItemStack::from_storage);
    let was_playing = nbt.remove(TICKS).is_some();
    set_has_record(world, pos, state, false);
    if was_playing {
        announce_stop(world, pos, state);
    }
    song_changed(world, pos);
    record
}

/// A click on a loaded jukebox: the record pops out on top.
pub fn eject<W: RegionWorld + ?Sized>(world: &mut W, pos: BlockPos) -> bool {
    let Some(record) = take(world, pos) else {
        return false;
    };
    let rng = world.rng();
    let mut offset = || f64::from(rng.r#gen::<f32>() * 0.7 - 0.35);
    let (dx, dz) = (offset(), offset());
    let position = [
        f64::from(pos.x) + 0.5 + dx,
        f64::from(pos.y) + 1.01,
        f64::from(pos.z) + 0.5 + dz,
    ];
    let rng = world.rng();
    let velocity = [
        rng.r#gen::<f64>() * 0.2 - 0.1,
        0.2,
        rng.r#gen::<f64>() * 0.2 - 0.1,
    ];
    world.spawn_item_at(record, position, velocity, EJECT_PICKUP_DELAY);
    true
}

fn announce_stop<W: RegionWorld + ?Sized>(world: &mut W, pos: BlockPos, state: BlockState) {
    world.game_event(
        GameEvent::JukeboxStopPlay,
        pos.center(),
        Context::block(state),
    );
    world.level_event(play::level_event::SOUND_STOP_JUKEBOX_SONG, pos, 0);
}

/// Picks up playing jukeboxes this region did not start itself (loaded
/// chunks).
pub fn rescan<W: RegionWorld + ?Sized>(world: &mut W) {
    let found = world.raw_block_entities_where(&|nbt| {
        nbt.get_str("id") == Some("minecraft:jukebox") && is_playing(nbt)
    });
    world.state().get::<Playing>().0.extend(found);
}

/// One tick of every playing jukebox: the song ends after its length
/// plus a second; while it plays, every second tells listeners and shows
/// a note.
pub fn tick<W: RegionWorld + ?Sized>(world: &mut W) {
    let mut playing: Vec<BlockPos> = world.state().get::<Playing>().0.iter().copied().collect();
    playing.sort_unstable_by_key(|p| (p.y, p.z, p.x));
    for pos in playing {
        let Some((state, ticks, length)) = jukebox_at(world, pos).and_then(|state| {
            let nbt = world.raw_block_entity(pos)?;
            let ticks = nbt.get_long(TICKS)?;
            Some((state, ticks, song_ticks(&record(nbt)?)?))
        }) else {
            world.state().get::<Playing>().0.remove(&pos);
            continue;
        };
        if ticks >= length + TAIL_TICKS {
            world.state().get::<Playing>().0.remove(&pos);
            if let Some(nbt) = world.raw_block_entity_mut(pos) {
                nbt.remove(TICKS);
            }
            announce_stop(world, pos, state);
            song_changed(world, pos);
            continue;
        }
        if ticks % PLAY_EVENT_INTERVAL == 0 {
            world.game_event(GameEvent::JukeboxPlay, pos.center(), Context::block(state));
            let color = world.rng().gen_range(0..4) as f32 / 24.0;
            let note = framed(&play::Particles {
                particle: ParticleType::Note.id(),
                position: [
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + 1.2000000476837158,
                    f64::from(pos.z) + 0.5,
                ],
                spread: [color, 0.0, 0.0],
                speed: 1.0,
                count: 0,
                alternative: false,
            });
            world.send_near(pos.center(), PARTICLE_RANGE, None, packets([note]));
        }
        if let Some(nbt) = world.raw_block_entity_mut(pos) {
            nbt.insert(TICKS, Tag::Long(ticks + 1));
        }
    }
}

pub struct JukeboxPort;

impl tuff_core::world::item_port::ItemPort for JukeboxPort {
    fn offer(
        &self,
        world: &mut dyn RegionWorld,
        at: &tuff_core::world::item_port::PortAccess,
        stack: &ItemStack,
    ) -> bool {
        accepts(world, at.pos, stack) && insert(world, at.pos, stack.dup_unchecked())
    }

    fn replace(
        &self,
        world: &mut dyn RegionWorld,
        at: &tuff_core::world::item_port::PortAccess,
        stack: Option<ItemStack>,
    ) -> bool {
        replace(world, at.pos, stack)
    }

    fn peek(
        &self,
        world: &dyn RegionWorld,
        at: &tuff_core::world::item_port::PortAccess,
    ) -> Option<ItemStack> {
        if !world.owns(at.pos) {
            return None;
        }
        world.raw_block_entity(at.pos).and_then(record)
    }

    fn take(
        &self,
        world: &mut dyn RegionWorld,
        at: &tuff_core::world::item_port::PortAccess,
    ) -> Option<ItemStack> {
        take(world, at.pos)
    }
}
