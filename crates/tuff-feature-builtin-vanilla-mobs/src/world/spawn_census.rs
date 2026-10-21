//! Natural spawning's dimension-wide mob counts.
//!
//! The game counts every loaded mob of a dimension against one cap scaled
//! by the chunks near its players, and each player's mobs against a
//! per-player cap. Regions spawn in parallel, so no region may wait on
//! another: each region reports what it holds (mobs per category, its
//! spawnable chunks, and per nearby player the mobs it counts for that
//! player) whenever that changes and at least every [`REFRESH_TICKS`]. One
//! actor sums the reports of each dimension and publishes the sums on a
//! `watch` once per tick. A region spawning reads the latest sums, takes
//! its own last report back out (the per-region budget is the sums of
//! every other region) and adds its live counts, so the caps hold
//! dimension-wide with no round trip per spawn, at most a tick or two
//! stale. Regions spawning in the same tick can overshoot by what each
//! spawns that tick, which the next tick's caps absorb.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot, watch};
use tuff_core::world::Shared;
use tuff_core::world::dimension::{DimensionId, Dimensions};
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_vanilla_data::entity::MobCategory;
use tuff_world_model::pos::RegionPos;

type CensusStart = Option<(mpsc::Receiver<Message>, watch::Sender<Arc<Census>>)>;

/// Mobs per category, by category id.
pub type Counts = [i32; MobCategory::COUNT];

/// A region resends an unchanged report this often, so the actor can tell
/// a closed region's stale one apart.
pub const REFRESH_TICKS: u64 = 100;
/// A report not refreshed for this long belongs to a region that is gone.
const EXPIRY_TICKS: u64 = 1200;
/// Bounded: regions only `try_send`, and a report that does not fit is
/// sent again on the region's next tick.
const INBOX: usize = 4096;

/// What one region holds for natural spawning.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Its own chunks near a player, which scale the caps.
    pub chunks: i32,
    pub counts: Counts,
    /// For each player (by UUID) near any of its mobs, the mobs counted
    /// for that player's own cap.
    pub near: Vec<(u128, Counts)>,
}

/// The sums of one dimension's reports.
#[derive(Debug, Default)]
pub struct Totals {
    pub chunks: i32,
    pub counts: Counts,
    pub near: HashMap<u128, Counts>,
    regions: HashMap<RegionPos, Arc<Report>>,
}

impl Totals {
    fn sum(regions: HashMap<RegionPos, Arc<Report>>) -> Self {
        let mut totals = Self::default();
        for report in regions.values() {
            totals.chunks += report.chunks;
            add(&mut totals.counts, &report.counts);
            for (player, counts) in &report.near {
                add(totals.near.entry(*player).or_default(), counts);
            }
        }
        totals.regions = regions;
        totals
    }

    /// What every region but `region` holds: its spawn budget.
    pub fn others(&self, region: RegionPos) -> Others {
        let own = self.regions.get(&region);
        let mut counts = self.counts;
        let mut chunks = self.chunks;
        if let Some(own) = own {
            chunks -= own.chunks;
            for (total, mine) in counts.iter_mut().zip(own.counts) {
                *total -= mine;
            }
        }
        Others {
            chunks,
            counts,
            near: self
                .near
                .iter()
                .filter_map(|(player, total)| {
                    let mut counts = *total;
                    if let Some((_, mine)) =
                        own.and_then(|own| own.near.iter().find(|(p, _)| p == player))
                    {
                        for (total, mine) in counts.iter_mut().zip(mine) {
                            *total -= mine;
                        }
                    }
                    counts.iter().any(|c| *c != 0).then_some((*player, counts))
                })
                .collect(),
        }
    }
}

/// What the other regions of a dimension hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Others {
    pub chunks: i32,
    pub counts: Counts,
    pub near: HashMap<u128, Counts>,
}

impl Others {
    pub fn near(&self, player: u128) -> Counts {
        self.near.get(&player).copied().unwrap_or_default()
    }
}

fn add(into: &mut Counts, counts: &Counts) {
    for (total, count) in into.iter_mut().zip(counts) {
        *total += count;
    }
}

/// Every dimension's sums, as last published.
pub type Census = HashMap<DimensionId, Totals>;

enum Message {
    #[cfg(test)]
    Panic,
    Report {
        dimension: DimensionId,
        region: RegionPos,
        tick: u64,
        report: Arc<Report>,
    },
    Settled(oneshot::Sender<()>),
}

#[derive(Default)]
struct Dimension {
    regions: HashMap<RegionPos, (u64, Arc<Report>)>,
    dirty: bool,
}

struct Actor {
    dimensions: HashMap<DimensionId, Dimension>,
    view: watch::Sender<Arc<Census>>,
}

impl Actor {
    fn handle(&mut self, message: Message) {
        match message {
            #[cfg(test)]
            Message::Panic => panic!("test panic"),
            Message::Report {
                dimension,
                region,
                tick,
                report,
            } => {
                let dimension = self.dimensions.entry(dimension).or_default();
                dimension.regions.insert(region, (tick, report));
                dimension.dirty = true;
            }
            Message::Settled(reply) => {
                self.publish();
                let _ = reply.send(());
            }
        }
    }

    fn expire(&mut self, now: u64) {
        for dimension in self.dimensions.values_mut() {
            let before = dimension.regions.len();
            dimension
                .regions
                .retain(|_, (tick, _)| now.saturating_sub(*tick) <= EXPIRY_TICKS);
            dimension.dirty |= dimension.regions.len() != before;
        }
    }

    fn publish(&mut self) {
        if !self.dimensions.values().any(|d| d.dirty) {
            return;
        }
        let census: Census = self
            .dimensions
            .iter_mut()
            .map(|(id, dimension)| {
                dimension.dirty = false;
                let regions = dimension
                    .regions
                    .iter()
                    .map(|(pos, (_, report))| (*pos, report.clone()))
                    .collect();
                (*id, Totals::sum(regions))
            })
            .collect();
        self.view.send_replace(Arc::new(census));
    }

    async fn run(&mut self, shared: &Arc<Shared>, messages: &mut mpsc::Receiver<Message>) {
        let mut clock = shared.clock.clone();
        loop {
            tokio::select! {
                message = messages.recv() => {
                    let Some(message) = message else { break };
                    self.handle(message);
                }
                changed = clock.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let now = *clock.borrow_and_update();
                    self.expire(now);
                    self.publish();
                }
            }
        }
    }
}

/// The world's spawn census, reached by regions through `try_send` and a
/// `watch` read.
pub struct SpawnCensus {
    inbox: mpsc::Sender<Message>,
    view: watch::Receiver<Arc<Census>>,
    start: Mutex<CensusStart>,
}

impl SpawnCensus {
    /// Hands over `region`'s report as of `tick`; false when the actor is
    /// busy (report again next tick).
    pub fn report(
        &self,
        dimension: DimensionId,
        region: RegionPos,
        tick: u64,
        report: Arc<Report>,
    ) -> bool {
        self.inbox
            .try_send(Message::Report {
                dimension,
                region,
                tick,
                report,
            })
            .is_ok()
    }

    /// The other regions' part of `dimension`'s counts, as last published.
    pub fn others(&self, dimension: DimensionId, region: RegionPos) -> Others {
        self.view
            .borrow()
            .get(&dimension)
            .map(|totals| totals.others(region))
            .unwrap_or_default()
    }
}

impl WorldService for SpawnCensus {
    fn start(&self, shared: &Arc<Shared>) {
        let Some((messages, view)) = self.start.lock().expect("spawn census lock").take() else {
            return;
        };
        let actor = Actor {
            dimensions: HashMap::new(),
            view,
        };
        tuff_supervise::spawn(
            "the spawn census",
            tuff_supervise::restarting(
                "the spawn census",
                (actor, shared.clone(), messages),
                |(actor, shared, messages)| Box::pin(actor.run(shared, messages)),
            ),
        );
    }

    fn settled(&self, _tick: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if self.start.lock().expect("spawn census lock").is_some() {
                return;
            }
            let (reply, done) = oneshot::channel();
            if self.inbox.send(Message::Settled(reply)).await.is_ok() {
                let _ = done.await;
            }
        })
    }
}

pub struct SpawnCensusFactory;

impl ServiceFactory for SpawnCensusFactory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        let (inbox, messages) = mpsc::channel(INBOX);
        let (view, watched) = watch::channel(Arc::new(Census::new()));
        Arc::new(SpawnCensus {
            inbox,
            view: watched,
            start: Mutex::new(Some((messages, view))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_census_keeping_its_reports() {
        let (_clock, clock_rx) = watch::channel(0u64);
        let mut shared = tuff_test_support::bare_shared();
        shared.clock = clock_rx;
        let shared = Arc::new(shared);
        let (inbox, messages) = mpsc::channel(8);
        let (view_tx, view) = watch::channel(Arc::new(Census::default()));
        let actor = Actor {
            dimensions: HashMap::new(),
            view: view_tx,
        };
        tokio::spawn(tuff_supervise::restarting(
            "the spawn census",
            (actor, shared, messages),
            |(actor, shared, messages)| Box::pin(actor.run(shared, messages)),
        ));
        let dimension = DimensionId::for_tests(0);
        let region = RegionPos::new(0, 0);
        let report = Arc::new(Report {
            chunks: 4,
            counts: Counts::default(),
            near: Vec::new(),
        });
        let tick = 5;
        inbox
            .send(Message::Report {
                dimension,
                region,
                tick,
                report,
            })
            .await
            .unwrap();
        inbox.send(Message::Panic).await.unwrap();
        let (reply, settled) = oneshot::channel();
        inbox.send(Message::Settled(reply)).await.unwrap();
        settled.await.unwrap();
        assert!(view.borrow().contains_key(&dimension));
    }

    fn counts(monsters: i32, creatures: i32) -> Counts {
        let mut counts = Counts::default();
        counts[MobCategory::Monster.id() as usize] = monsters;
        counts[MobCategory::Creature.id() as usize] = creatures;
        counts
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_region_budget_is_every_other_region_summed() {
        let (a, b, c) = (
            RegionPos::new(0, 0),
            RegionPos::new(1, 0),
            RegionPos::new(0, 1),
        );
        let regions = HashMap::from([
            (
                a,
                Arc::new(Report {
                    chunks: 40,
                    counts: counts(30, 2),
                    near: vec![(7, counts(30, 2))],
                }),
            ),
            (
                b,
                Arc::new(Report {
                    chunks: 41,
                    counts: counts(5, 0),
                    near: vec![(7, counts(5, 0)), (9, counts(1, 0))],
                }),
            ),
        ]);
        let totals = Totals::sum(regions);
        assert_eq!((totals.chunks, totals.counts), (81, counts(35, 2)));
        let for_b = totals.others(b);
        assert_eq!(for_b.chunks, 40);
        assert_eq!(for_b.counts, counts(30, 2));
        assert_eq!(for_b.near(7), counts(30, 2));
        assert_eq!(for_b.near(9), Counts::default());
        let for_a = totals.others(a);
        assert_eq!((for_a.chunks, for_a.near(9)), (41, counts(1, 0)));
        let unknown = totals.others(c);
        assert_eq!((unknown.chunks, unknown.counts), (81, counts(35, 2)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn stale_reports_expire() {
        let (view, _watched) = watch::channel(Arc::new(Census::new()));
        let mut actor = Actor {
            dimensions: HashMap::new(),
            view,
        };
        let report = Arc::new(Report {
            chunks: 1,
            ..Default::default()
        });
        for (region, tick) in [(RegionPos::new(0, 0), 10), (RegionPos::new(1, 0), 500)] {
            actor.handle(Message::Report {
                dimension: DimensionId::default(),
                region,
                tick,
                report: report.clone(),
            });
        }
        actor.expire(10 + EXPIRY_TICKS + 1);
        actor.publish();
        let census = actor.view.borrow().clone();
        assert_eq!(census[&DimensionId::default()].chunks, 1);
    }
}
