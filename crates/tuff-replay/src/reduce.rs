//! Seek reduction: which events of a range a seek can skip because what they
//! create is gone again before the range ends. A chunk that is loaded and
//! then forgotten (or loaded again), an entity that is added and then
//! removed (or added again), and the block and entity updates in between
//! never reach the viewer. Skipping them leaves the transformer's tracked
//! chunks and entities exactly as playing every event would.

use std::collections::{BTreeSet, HashMap};
use std::hash::Hash;

use bytes::Bytes;
use tuff_vanilla_data::packet::play::Clientbound as C;
use tuff_wire::PacketReader;

use crate::script::{Event, Timed};

/// What an event does to a chunk or entity, keyed by the chunk or entity.
enum Effect {
    LoadChunk((i32, i32)),
    ForgetChunk((i32, i32)),
    TouchChunk((i32, i32)),
    AddEntity(i32),
    RemoveEntities(Vec<i32>),
    TouchEntity(i32),
    MoveEntity(i32),
}

#[derive(Default)]
struct Life {
    /// The viewer holds it from before the range and no kept event dropped it.
    held: bool,
    /// The event in the range that created the current one.
    created: Option<usize>,
    /// Events since then that only change it.
    touched: Vec<usize>,
    /// Relative moves since then; folded into one sync if it lives on.
    moves: Vec<usize>,
}

impl Life {
    fn supersede(&mut self, at: usize, skip: &mut [bool]) {
        self.drop_range(skip);
        self.created = Some(at);
    }

    fn drop_range(&mut self, skip: &mut [bool]) {
        if let Some(created) = self.created.take() {
            skip[created] = true;
        }
        for touched in self.touched.drain(..).chain(self.moves.drain(..)) {
            skip[touched] = true;
        }
    }

    /// Ends it; returns whether the viewer would still hold it without
    /// the ending event.
    fn end(&mut self, skip: &mut [bool]) -> bool {
        self.drop_range(skip);
        std::mem::take(&mut self.held)
    }
}

struct Lives<'a, K> {
    held: &'a BTreeSet<K>,
    lives: HashMap<K, Life>,
}

impl<'a, K: Copy + Ord + Hash> Lives<'a, K> {
    fn new(held: &'a BTreeSet<K>) -> Self {
        Self {
            held,
            lives: HashMap::new(),
        }
    }

    fn get(&mut self, key: K) -> &mut Life {
        let held = self.held;
        self.lives.entry(key).or_insert_with(|| Life {
            held: held.contains(&key),
            ..Life::default()
        })
    }
}

/// For each event of `events`, whether a seek over them can skip it, given
/// the chunks and entities the viewer holds before the first one.
pub fn skippable(
    events: &[Timed],
    chunks: &BTreeSet<(i32, i32)>,
    entities: &BTreeSet<i32>,
) -> Skips {
    let mut skip = vec![false; events.len()];
    let mut chunks = Lives::new(chunks);
    let mut entities = Lives::new(entities);
    for (at, timed) in events.iter().enumerate() {
        let Event::Packet(payload) = &timed.event else {
            continue;
        };
        let Some(effect) = effect(payload) else {
            continue;
        };
        match effect {
            Effect::LoadChunk(pos) => chunks.get(pos).supersede(at, &mut skip),
            Effect::ForgetChunk(pos) => skip[at] = !chunks.get(pos).end(&mut skip),
            Effect::TouchChunk(pos) => chunks.get(pos).touched.push(at),
            Effect::AddEntity(id) => entities.get(id).supersede(at, &mut skip),
            Effect::RemoveEntities(ids) => {
                let mut needed = false;
                for id in ids {
                    needed |= entities.get(id).end(&mut skip);
                }
                skip[at] = !needed;
            }
            Effect::TouchEntity(id) => entities.get(id).touched.push(at),
            Effect::MoveEntity(id) => entities.get(id).moves.push(at),
        }
    }
    let mut collapse = vec![false; events.len()];
    for life in entities.lives.values() {
        for &at in &life.moves {
            collapse[at] = true;
        }
    }
    Skips { skip, collapse }
}

/// What a seek does with each event of a range.
pub struct Skips {
    /// Dropped: what it does is gone again before the end.
    pub skip: Vec<bool>,
    /// A relative move of an entity that lives on: tracked, not sent; the
    /// entity gets one position sync at the end instead.
    pub collapse: Vec<bool>,
}

const MOVES: [C; 5] = [
    C::MoveEntityPos,
    C::MoveEntityPosRot,
    C::MoveEntityRot,
    C::RotateHead,
    C::SetEntityMotion,
];

const ENTITY_UPDATES: [C; 14] = [
    C::MoveMinecartAlongTrack,
    C::SetEntityData,
    C::SetEquipment,
    C::UpdateAttributes,
    C::TeleportEntity,
    C::EntityPositionSync,
    C::Animate,
    C::HurtAnimation,
    C::DamageEvent,
    C::UpdateMobEffect,
    C::RemoveMobEffect,
    C::TakeItemEntity,
    C::SetPassengers,
    C::ProjectilePower,
];

const MAX_REMOVED: i32 = 1 << 16;

fn effect(payload: &Bytes) -> Option<Effect> {
    let mut r = PacketReader::new(payload.clone());
    let id = r.read_var_int().ok()?;
    let is = |kind: C| kind.id() == id;
    if is(C::LevelChunkWithLight) {
        Some(Effect::LoadChunk((r.read_int().ok()?, r.read_int().ok()?)))
    } else if is(C::ForgetLevelChunk) {
        let z = r.read_int().ok()?;
        let x = r.read_int().ok()?;
        Some(Effect::ForgetChunk((x, z)))
    } else if is(C::BlockUpdate) || is(C::BlockEntityData) || is(C::BlockEvent) {
        let packed = r.read_long().ok()?;
        let x = (packed >> 38) as i32;
        let z = ((packed << 26) >> 38) as i32;
        Some(Effect::TouchChunk((x >> 4, z >> 4)))
    } else if is(C::SectionBlocksUpdate) {
        let packed = r.read_long().ok()?;
        let x = (packed >> 42) as i32;
        let z = ((packed << 22) >> 42) as i32;
        Some(Effect::TouchChunk((x, z)))
    } else if is(C::LightUpdate) {
        Some(Effect::TouchChunk((
            r.read_var_int().ok()?,
            r.read_var_int().ok()?,
        )))
    } else if is(C::AddEntity) {
        Some(Effect::AddEntity(r.read_var_int().ok()?))
    } else if is(C::RemoveEntities) {
        let count = r.read_var_int().ok()?;
        if !(0..=MAX_REMOVED).contains(&count) {
            return None;
        }
        let ids = (0..count)
            .map(|_| r.read_var_int().ok())
            .collect::<Option<Vec<_>>>()?;
        Some(Effect::RemoveEntities(ids))
    } else if MOVES.iter().any(|kind| kind.id() == id) {
        Some(Effect::MoveEntity(r.read_var_int().ok()?))
    } else if ENTITY_UPDATES.iter().any(|kind| kind.id() == id) {
        Some(Effect::TouchEntity(r.read_var_int().ok()?))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use tuff_core::protocol::play::{ForgetLevelChunk, RemoveEntities};
    use tuff_wire::PacketWriter;

    use super::*;
    use crate::transform::encode;

    fn raw(kind: C, body: impl FnOnce(&mut PacketWriter)) -> Timed {
        let mut w = PacketWriter::new();
        w.write_var_int(kind.id());
        body(&mut w);
        Timed {
            ns: 0,
            event: Event::Packet(w.into_bytes()),
        }
    }

    fn timed(payload: Bytes) -> Timed {
        Timed {
            ns: 0,
            event: Event::Packet(payload),
        }
    }

    fn load(x: i32, z: i32) -> Timed {
        raw(C::LevelChunkWithLight, |w| {
            w.write_int(x);
            w.write_int(z);
        })
    }

    fn forget(x: i32, z: i32) -> Timed {
        timed(encode(&ForgetLevelChunk { x, z }))
    }

    fn block(x: i32, y: i32, z: i32) -> Timed {
        raw(C::BlockUpdate, |w| {
            w.write_long(
                (i64::from(x) & 0x3FF_FFFF) << 38
                    | (i64::from(z) & 0x3FF_FFFF) << 12
                    | (i64::from(y) & 0xFFF),
            );
            w.write_var_int(1);
        })
    }

    fn add(id: i32) -> Timed {
        raw(C::AddEntity, |w| w.write_var_int(id))
    }

    fn touch(id: i32) -> Timed {
        raw(C::SetEntityData, |w| w.write_var_int(id))
    }

    fn remove(ids: &[i32]) -> Timed {
        timed(encode(&RemoveEntities(ids)))
    }

    fn skipped(events: &[Timed], chunks: &[(i32, i32)], entities: &[i32]) -> Vec<usize> {
        let chunks = chunks.iter().copied().collect();
        let entities = entities.iter().copied().collect();
        skippable(events, &chunks, &entities)
            .skip
            .into_iter()
            .enumerate()
            .filter_map(|(at, skip)| skip.then_some(at))
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn chunks_gone_by_the_end_are_skipped_with_their_updates() {
        let events = [
            load(0, 0),
            block(3, 70, -5),
            block(-20, -60, 40),
            forget(0, -1),
            load(-2, 2),
            load(-2, 2),
            forget(0, 0),
            forget(5, 5),
            load(7, 7),
        ];
        assert_eq!(skipped(&events, &[(0, -1)], &[]), [0, 1, 2, 4, 6, 7]);
        assert_eq!(skipped(&events, &[(0, 0)], &[]), [0, 1, 2, 3, 4, 7]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn entities_removed_by_the_end_are_skipped_with_their_updates() {
        let events = [
            add(1),
            touch(1),
            touch(2),
            add(3),
            touch(3),
            remove(&[1, 2]),
            add(3),
            remove(&[4]),
            touch(5),
        ];
        assert_eq!(skipped(&events, &[], &[2]), [0, 1, 2, 3, 4, 7]);
        assert_eq!(skipped(&events, &[], &[]), [0, 1, 2, 3, 4, 5, 7]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn moves_of_entities_that_live_on_are_collapsed() {
        let mv = |id| raw(C::MoveEntityPos, |w| w.write_var_int(id));
        let events = [add(1), mv(1), mv(2), touch(1), mv(3), remove(&[3]), mv(1)];
        let skips = skippable(&events, &BTreeSet::new(), &BTreeSet::from([2]));
        let at = |v: Vec<bool>| {
            v.into_iter()
                .enumerate()
                .filter_map(|(i, b)| b.then_some(i))
                .collect::<Vec<_>>()
        };
        assert_eq!(at(skips.collapse), [1, 2, 6]);
        assert_eq!(at(skips.skip), [4, 5]);
    }
}
