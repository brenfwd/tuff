//! Scheduled block and fluid ticks, kept per chunk as the game keeps them
//! and saved in the chunk's `block_ticks` and `fluid_ticks` lists.
//!
//! A tick is unique per position and kind: scheduling one that is already
//! pending does nothing. Due ticks run in order of due time, priority and
//! the order they were scheduled in, and at most [`MAX_PER_TICK`] of each
//! list run per game tick; the rest wait for the next one.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet};
use std::hash::Hash;

use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

type TickKey = (u64, i32, i64);

/// The most ticks of one kind a region runs per game tick, the game's
/// per-level bound.
pub const MAX_PER_TICK: usize = 65536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheduled<T> {
    pub pos: BlockPos,
    pub kind: T,
    pub due: u64,
    pub priority: i32,
    /// When it was scheduled relative to the others; ticks loaded from a
    /// chunk come first.
    pub order: i64,
}

impl<T> Scheduled<T> {
    fn key(&self) -> (u64, i32, i64) {
        (self.due, self.priority, self.order)
    }
}

struct Entry<T>(Scheduled<T>);

impl<T> PartialEq for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.key() == other.0.key()
    }
}

impl<T> Eq for Entry<T> {}

impl<T> PartialOrd for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.key().cmp(&other.0.key())
    }
}

/// The pending ticks of one chunk.
pub struct ChunkTicks<T> {
    queue: BinaryHeap<Reverse<Entry<T>>>,
    pending: HashSet<(BlockPos, T)>,
}

impl<T> Default for ChunkTicks<T> {
    fn default() -> Self {
        Self {
            queue: BinaryHeap::new(),
            pending: HashSet::new(),
        }
    }
}

impl<T: Copy + Eq + Hash> ChunkTicks<T> {
    /// Adds the tick unless one of its kind is already pending at its
    /// position.
    pub fn schedule(&mut self, tick: Scheduled<T>) -> bool {
        if !self.pending.insert((tick.pos, tick.kind)) {
            return false;
        }
        self.queue.push(Reverse(Entry(tick)));
        true
    }

    pub fn is_pending(&self, pos: BlockPos, kind: T) -> bool {
        self.pending.contains(&(pos, kind))
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn peek(&self) -> Option<&Scheduled<T>> {
        self.queue.peek().map(|Reverse(Entry(tick))| tick)
    }

    fn pop(&mut self) -> Option<Scheduled<T>> {
        let Reverse(Entry(tick)) = self.queue.pop()?;
        self.pending.remove(&(tick.pos, tick.kind));
        Some(tick)
    }

    /// The pending ticks in the order they were scheduled, as they are
    /// saved.
    pub fn in_order(&self) -> Vec<Scheduled<T>> {
        let mut ticks: Vec<Scheduled<T>> = self
            .queue
            .iter()
            .map(|Reverse(Entry(tick))| *tick)
            .collect();
        ticks.sort_by_key(|tick| tick.order);
        ticks
    }
}

/// Takes the ticks due by `now` out of `chunks`, in the order they run, at
/// most `limit`.
pub fn drain<T: Copy + Eq + Hash>(
    chunks: &mut [&mut ChunkTicks<T>],
    now: u64,
    limit: usize,
) -> Vec<Scheduled<T>> {
    let mut tops: BinaryHeap<Reverse<(TickKey, usize)>> = chunks
        .iter()
        .enumerate()
        .filter_map(|(i, chunk)| {
            let top = chunk.peek()?;
            (top.due <= now).then_some(Reverse((top.key(), i)))
        })
        .collect();
    let mut due = Vec::new();
    while due.len() < limit
        && let Some(Reverse((_, i))) = tops.pop()
    {
        let Some(tick) = chunks[i].pop() else {
            continue;
        };
        due.push(tick);
        if let Some(next) = chunks[i].peek()
            && next.due <= now
        {
            tops.push(Reverse((next.key(), i)));
        }
    }
    due
}

/// Takes the entries of `list` that `parse` recognises out of a loaded
/// chunk's data as pending ticks; the rest stay stored as they were.
pub fn take<T: Copy + Eq + Hash>(
    extra: &mut Compound,
    list: &str,
    now: u64,
    parse: impl Fn(&str) -> Option<T>,
) -> ChunkTicks<T> {
    let mut ticks = ChunkTicks::default();
    let Some(Tag::List(entries)) = extra.get(list).cloned() else {
        return ticks;
    };
    let mut kept = Vec::new();
    let mut taken = Vec::new();
    for entry in entries {
        let Tag::Compound(nbt) = &entry else {
            kept.push(entry);
            continue;
        };
        let (Some(kind), Some(x), Some(y), Some(z)) = (
            nbt.get_str("i").and_then(&parse),
            nbt.get_int("x"),
            nbt.get_int("y"),
            nbt.get_int("z"),
        ) else {
            kept.push(entry);
            continue;
        };
        let delay = nbt.get_int("t").unwrap_or(0).max(0) as u64;
        let priority = nbt.get_int("p").unwrap_or(0);
        taken.push((BlockPos::new(x, y, z), kind, now + delay, priority));
    }
    let count = taken.len() as i64;
    for (index, (pos, kind, due, priority)) in taken.into_iter().enumerate() {
        ticks.schedule(Scheduled {
            pos,
            kind,
            due,
            priority,
            order: index as i64 - count,
        });
    }
    extra.insert(list, Tag::List(kept));
    ticks
}

/// Adds pending ticks to a chunk's stored `list`, with their delays from
/// `now`.
pub fn store<T: Copy + Eq + Hash>(
    extra: &mut Compound,
    list: &str,
    ticks: &ChunkTicks<T>,
    now: u64,
    name: impl Fn(T) -> &'static str,
) {
    if ticks.is_empty() {
        return;
    }
    let mut entries = match extra.get(list) {
        Some(Tag::List(entries)) => entries.clone(),
        _ => Vec::new(),
    };
    for tick in ticks.in_order() {
        entries.push(Tag::Compound(
            Compound::new()
                .with("i", name(tick.kind))
                .with("x", tick.pos.x)
                .with("y", tick.pos.y)
                .with("z", tick.pos.z)
                .with(
                    "t",
                    tick.due.saturating_sub(now).min(i32::MAX as u64) as i32,
                )
                .with("p", tick.priority),
        ));
    }
    extra.insert(list, Tag::List(entries));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(x: i32, kind: u8, due: u64, order: i64) -> Scheduled<u8> {
        Scheduled {
            pos: BlockPos::new(x, 64, 0),
            kind,
            due,
            priority: 0,
            order,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_pending_tick_is_not_scheduled_twice() {
        let mut ticks = ChunkTicks::default();
        assert!(ticks.schedule(tick(0, 1, 10, 0)));
        assert!(!ticks.schedule(tick(0, 1, 3, 1)));
        assert!(ticks.schedule(tick(0, 2, 3, 2)));
        assert_eq!(ticks.len(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn due_ticks_run_in_order_across_chunks_up_to_the_limit() {
        let mut a = ChunkTicks::default();
        let mut b = ChunkTicks::default();
        a.schedule(tick(0, 0, 5, 3));
        a.schedule(tick(1, 0, 4, 4));
        b.schedule(tick(2, 0, 5, 1));
        b.schedule(tick(3, 0, 9, 2));
        let due = drain(&mut [&mut a, &mut b], 5, 2);
        let xs: Vec<i32> = due.iter().map(|t| t.pos.x).collect();
        assert_eq!(xs, vec![1, 2]);
        let rest = drain(&mut [&mut a, &mut b], 5, 10);
        assert_eq!(rest.iter().map(|t| t.pos.x).collect::<Vec<_>>(), vec![0]);
        assert!(!a.is_pending(BlockPos::new(0, 64, 0), 0));
        assert!(b.is_pending(BlockPos::new(3, 64, 0), 0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn ticks_round_trip_through_stored_chunk_data() {
        let mut extra = Compound::new().with(
            "fluid_ticks",
            Tag::List(vec![Tag::Compound(
                Compound::new()
                    .with("i", "modded:goo")
                    .with("x", 1)
                    .with("y", 2)
                    .with("z", 3)
                    .with("t", 5)
                    .with("p", 0),
            )]),
        );
        let mut ticks = ChunkTicks::default();
        ticks.schedule(tick(7, 1, 130, 9));
        ticks.schedule(tick(8, 1, 100, 4));
        store(
            &mut extra,
            "fluid_ticks",
            &ticks,
            100,
            |_| "minecraft:water",
        );
        let parse = |name: &str| (name == "minecraft:water").then_some(1u8);
        let mut taken = take(&mut extra, "fluid_ticks", 1000, parse);
        let order: Vec<(i32, u64)> = taken.in_order().iter().map(|t| (t.pos.x, t.due)).collect();
        assert_eq!(order, vec![(8, 1000), (7, 1030)]);
        assert!(taken.in_order().iter().all(|t| t.order < 0));
        let due = drain(&mut [&mut taken], 1000, 10);
        assert_eq!(due.len(), 1);
        let Some(Tag::List(left)) = extra.get("fluid_ticks") else {
            panic!("fluid ticks kept");
        };
        assert_eq!(left.len(), 1, "unknown ticks stay stored");
    }
}
