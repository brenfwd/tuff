//! Area broadcasts and queries across region borders.
//!
//! A region only holds what lies inside it (plus read-only halo blocks), so
//! anything with a radius that crosses a border reaches the other regions
//! through these two primitives instead of a message variant per feature:
//!
//! - [`broadcast`] runs a task on every other loaded region an area reaches.
//! - [`query`] runs a read on every loaded region an area reaches, this one
//!   included, and hands the answers back to this region.
//!
//! Timing is fixed so the outcome never depends on which region ran a tick
//! first: work sent on game tick `N` runs at the start of each target's tick
//! `N + 1` (after neighbours' block updates, before blocks and entities
//! tick), ordered by sending tick, sender and send order. A query's reads
//! therefore all see the world as of the end of tick `N`, and its answer
//! runs on the asking region at the start of tick `N + 2`. While the game is
//! frozen, queued work runs on every frozen tick.
//!
//! Only loaded neighbours are reached: a region that is not running holds
//! no entities or players, and an area wider than a neighbour is clipped to
//! the eight regions around the sender. A query whose target region closes
//! before answering still completes with the answers it got.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::world::messages::RegionHandle;
use crate::world::region_api::{RegionTask, RegionWorld};
use tuff_world_model::pos::{ChunkPos, RegionPos};

/// An axis-aligned box of world coordinates: min x, y, z, then max x, y, z.
pub type Area = [f64; 6];

type Run = dyn Fn(&mut dyn RegionWorld) + Send + Sync;

/// Work run on each region an area reaches.
#[derive(Clone)]
pub struct AreaTask(Arc<Run>);

impl AreaTask {
    pub fn new(run: impl Fn(&mut dyn RegionWorld) + Send + Sync + 'static) -> Self {
        Self(Arc::new(run))
    }

    pub fn run(&self, world: &mut dyn RegionWorld) {
        (self.0)(world);
    }

    pub(crate) fn once(self) -> RegionTask {
        RegionTask::new(move |world| self.run(world))
    }
}

impl std::fmt::Debug for AreaTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AreaTask")
    }
}

/// A box reaching `radius` blocks from `center` along each axis.
pub fn around(center: [f64; 3], radius: f64) -> Area {
    [
        center[0] - radius,
        center[1] - radius,
        center[2] - radius,
        center[0] + radius,
        center[1] + radius,
        center[2] + radius,
    ]
}

/// The regions `area` overlaps among `region` and the eight around it, in
/// x then z order.
pub fn regions_in(area: Area, region: RegionPos) -> impl Iterator<Item = RegionPos> {
    let min = ChunkPos::of_position([area[0], 0.0, area[2]]).region();
    let max = ChunkPos::of_position([area[3], 0.0, area[5]]).region();
    let (x0, x1) = (min.x.max(region.x - 1), max.x.min(region.x + 1));
    let (z0, z1) = (min.z.max(region.z - 1), max.z.min(region.z + 1));
    (x0..=x1).flat_map(move |x| (z0..=z1).map(move |z| RegionPos::new(x, z)))
}

/// Runs `task` on every other loaded region `area` reaches, at the start of
/// its next tick. The caller handles its own region itself.
pub fn broadcast(
    world: &mut dyn RegionWorld,
    area: Area,
    task: impl Fn(&mut dyn RegionWorld) + Send + Sync + 'static,
) {
    world.area_run(area, false, AreaTask::new(task));
}

/// Runs `read` on every loaded region `area` reaches, this one included, at
/// the start of their next tick, then `then` on this region one tick later
/// with every answer (this region's first, then the others in region order).
pub fn query<T: Send + 'static>(
    world: &mut dyn RegionWorld,
    area: Area,
    read: impl Fn(&mut dyn RegionWorld) -> T + Send + Sync + 'static,
    then: impl FnOnce(&mut dyn RegionWorld, Vec<T>) + Send + 'static,
) {
    let gather = Arc::new(Gather {
        origin: world.region_pos(),
        handle: world.handle(),
        seq: AtomicU64::new(0),
        answers: Mutex::new(Answers {
            tick: None,
            list: Vec::new(),
        }),
        then: Mutex::new(Some(Box::new(then))),
    });
    let part = gather.clone();
    let seq = world.area_run(
        area,
        true,
        AreaTask::new(move |world| {
            let answer = read(world);
            let tick = world.tick();
            let mut answers = part.answers.lock().unwrap_or_else(PoisonError::into_inner);
            answers.tick = Some(answers.tick.map_or(tick, |t| t.max(tick)));
            answers.list.push((world.region_pos(), answer));
        }),
    );
    gather.seq.store(seq, Ordering::Relaxed);
}

type Then<T> = Box<dyn FnOnce(&mut dyn RegionWorld, Vec<T>) + Send>;

struct Answers<T> {
    /// The latest tick an answer was read on.
    tick: Option<u64>,
    list: Vec<(RegionPos, T)>,
}

/// The answers to one [`query`]; once the last region holding a part lets
/// go of it (answered, or dropped it while closing), the answers go back to
/// the asking region, to run the tick after the last was read.
struct Gather<T: Send + 'static> {
    origin: RegionPos,
    handle: RegionHandle,
    seq: AtomicU64,
    answers: Mutex<Answers<T>>,
    then: Mutex<Option<Then<T>>>,
}

impl<T: Send + 'static> Drop for Gather<T> {
    fn drop(&mut self) {
        let Some(then) = self
            .then
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let Answers { tick, mut list } = std::mem::replace(
            self.answers
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner),
            Answers {
                tick: None,
                list: Vec::new(),
            },
        );
        let origin = self.origin;
        list.sort_by_key(|(region, _)| (*region != origin, *region));
        let answers: Vec<T> = list.into_iter().map(|(_, answer)| answer).collect();
        self.handle
            .internal(crate::world::messages::RegionInternal::Area(Deferred {
                tick,
                from: origin,
                seq: *self.seq.get_mut(),
                task: RegionTask::new(move |world| then(world, answers)),
            }));
    }
}

/// Work waiting for the tick phase that runs it on a region.
#[derive(Debug)]
pub struct Deferred {
    /// The game tick it was sent on; it runs on a later one (`None`: on the
    /// next tick, whatever it is).
    pub tick: Option<u64>,
    pub from: RegionPos,
    /// The sender's count of deferred work, for a stable order.
    pub seq: u64,
    pub task: RegionTask,
}

impl Deferred {
    fn key(&self) -> (u64, RegionPos, u64) {
        (self.tick.unwrap_or(u64::MAX), self.from, self.seq)
    }
}

/// A region's queue of deferred area work.
#[derive(Debug, Default)]
pub struct Queue {
    waiting: Vec<Deferred>,
    sent: u64,
}

impl Queue {
    /// The next send order number.
    pub fn next_seq(&mut self) -> u64 {
        self.sent += 1;
        self.sent
    }

    pub fn push(&mut self, deferred: Deferred) {
        self.waiting.push(deferred);
    }

    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    /// The work due on tick `now` (all of it when `now` is `None`), in run
    /// order.
    pub fn due(&mut self, now: Option<u64>) -> Vec<Deferred> {
        let (mut due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|deferred| match (now, deferred.tick) {
                (None, _) | (_, None) => true,
                (Some(now), Some(tick)) => tick < now,
            });
        self.waiting = waiting;
        due.sort_by_key(Deferred::key);
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deferred(tick: Option<u64>, from: (i32, i32), seq: u64) -> Deferred {
        Deferred {
            tick,
            from: RegionPos::new(from.0, from.1),
            seq,
            task: RegionTask::new(|_| {}),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn work_waits_for_the_tick_after_it_was_sent_and_runs_in_a_stable_order() {
        let mut queue = Queue::default();
        queue.push(deferred(Some(5), (1, 0), 2));
        queue.push(deferred(None, (0, 0), 0));
        queue.push(deferred(Some(4), (1, 0), 1));
        queue.push(deferred(Some(5), (0, 0), 7));
        queue.push(deferred(Some(5), (1, 0), 1));
        let order = |due: Vec<Deferred>| {
            due.into_iter()
                .map(|d| (d.tick, d.from.x, d.seq))
                .collect::<Vec<_>>()
        };
        assert_eq!(order(queue.due(Some(5))), [(Some(4), 1, 1), (None, 0, 0)]);
        assert_eq!(order(queue.due(Some(5))), []);
        assert_eq!(
            order(queue.due(Some(6))),
            [(Some(5), 0, 7), (Some(5), 1, 1), (Some(5), 1, 2)]
        );
        assert!(queue.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_area_reaches_every_region_it_overlaps() {
        let home = RegionPos::new(0, 0);
        let regions: Vec<_> = regions_in(around([511.5, 64.0, 0.0], 2.0), home).collect();
        assert_eq!(
            regions,
            [
                RegionPos::new(0, -1),
                RegionPos::new(0, 0),
                RegionPos::new(1, -1),
                RegionPos::new(1, 0)
            ]
        );
        assert_eq!(
            regions_in(around([100.0, 0.0, 100.0], 8.0), home).collect::<Vec<_>>(),
            [home]
        );
        assert_eq!(
            regions_in(around([0.0, 0.0, 0.0], 1e9), home).count(),
            9,
            "an area is clipped to the regions around the sender"
        );
    }
}
