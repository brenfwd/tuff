//! The anti-cheat's world service: a typed stream of violations other
//! features can subscribe to, and the board, an actor that keeps every
//! player's violation history for `/anticheat`, the staff who muted
//! alerts and the resets sessions apply to their own ladders.
//!
//! Sessions publish on a broadcast channel (never blocking, a lagging
//! reader loses the oldest events); the board is one such reader.
//! Commands talk to the board over a small buffered queue and wait for
//! its answer; the board never waits on anyone.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tuff_core::metrics::{Kind, Metrics, Series};
use tuff_core::world::Shared;
use tuff_core::world::dimension::Dimensions;
use tuff_core::world::services::{ServiceFactory, WorldService};

/// Events kept for a subscriber that falls behind.
const EVENTS: usize = 1024;
const TRACES: usize = 256;
const MAX_DEBUGGED: usize = 64;
const REQUESTS: usize = 32;
const MAX_PLAYERS: usize = 4096;
const KEPT_RESETS: usize = 64;
/// How long a chat message waits for the board before it counts no copies.
const ECHO_WAIT: Duration = Duration::from_millis(500);

/// Every counted violation, as a metrics counter shown on the dashboard.
pub const FLAGS_SERIES: &str = "anticheat.flags";
/// Every anti-cheat kick, as a metrics counter shown on the dashboard.
pub const KICKS_SERIES: &str = "anticheat.kicks";

/// What a violation did to the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The action was undone (cancelled, corrected or set back).
    SetBack,
    /// Only counted and reported: the check's action is `log`.
    Logged,
    Kicked,
}

/// One counted violation, as other features see it.
#[derive(Clone, Debug)]
pub struct Violation {
    pub player: u128,
    pub name: Arc<str>,
    pub check: &'static str,
    pub weight: f64,
    /// The check's violation level after this one.
    pub level: f64,
    pub outcome: Outcome,
}

/// Players whose checks are traced, by lowercase name: `None` traces
/// every check, `Some` only that one.
pub type Debugged = Arc<HashMap<String, Option<&'static str>>>;

/// One debug trace from a traced player's session.
#[derive(Clone, Debug)]
pub struct TraceEvent {
    pub player: u128,
    pub name: Arc<str>,
    pub trace: crate::trace::Trace,
}

/// A request to clear `player`'s levels of `check`, or of every check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reset {
    pub serial: u64,
    pub player: u128,
    pub check: Option<&'static str>,
}

/// One check's history for one player since the server started or the
/// last reset.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckStats {
    pub flags: u32,
    pub weight: f64,
    pub kicks: u32,
    pub level: f64,
    pub last: Instant,
    pub recent: Recent,
}

pub const HOUR: Duration = Duration::from_secs(3600);
pub const DAY: Duration = Duration::from_secs(86_400);
const BUCKET: Duration = Duration::from_secs(60);

/// Flag counts in one-minute buckets, kept for a day.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recent {
    buckets: std::collections::VecDeque<(Instant, u32)>,
}

impl Recent {
    pub fn add(&mut self, now: Instant) {
        while self
            .buckets
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= DAY)
        {
            self.buckets.pop_front();
        }
        match self.buckets.back_mut() {
            Some((at, n)) if now.saturating_duration_since(*at) < BUCKET => *n += 1,
            _ => self.buckets.push_back((now, 1)),
        }
    }

    /// Flags in the last `window`, to the minute.
    pub fn within(&self, now: Instant, window: Duration) -> u32 {
        self.buckets
            .iter()
            .filter(|(at, _)| now.saturating_duration_since(*at) < window)
            .map(|(_, n)| *n)
            .sum()
    }
}

/// One check's counts across every player.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckTotals {
    pub flags: u32,
    pub kicks: u32,
    pub hour: u32,
    pub day: u32,
}

/// Per-check counts summed over `players`.
pub fn totals(players: &[PlayerStats], now: Instant) -> BTreeMap<&'static str, CheckTotals> {
    let mut out: BTreeMap<&'static str, CheckTotals> = BTreeMap::new();
    for stats in players {
        for (check, c) in &stats.checks {
            let t = out.entry(check).or_default();
            t.flags = t.flags.saturating_add(c.flags);
            t.kicks = t.kicks.saturating_add(c.kicks);
            t.hour += c.recent.within(now, HOUR);
            t.day += c.recent.within(now, DAY);
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct PlayerStats {
    pub player: u128,
    pub name: Arc<str>,
    pub checks: BTreeMap<&'static str, CheckStats>,
}

impl PlayerStats {
    pub fn flags(&self) -> u32 {
        self.checks.values().map(|c| c.flags).sum()
    }

    pub fn kicks(&self) -> u32 {
        self.checks.values().map(|c| c.kicks).sum()
    }

    fn last(&self) -> Option<Instant> {
        self.checks.values().map(|c| c.last).max()
    }
}

enum Request {
    #[cfg(test)]
    Panic,
    Player {
        name: String,
        reply: oneshot::Sender<Option<PlayerStats>>,
    },
    List {
        reply: oneshot::Sender<Vec<PlayerStats>>,
    },
    Reset {
        name: String,
        check: Option<&'static str>,
        reply: oneshot::Sender<Option<Arc<str>>>,
    },
    Mute {
        player: u128,
        muted: Option<bool>,
        reply: oneshot::Sender<bool>,
    },
    Echo {
        key: u64,
        sender: u128,
        reply: oneshot::Sender<u32>,
    },
}

pub struct BoardFactory;

impl ServiceFactory for BoardFactory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(Board::new())
    }
}

/// The handle sessions and commands use; one per world.
pub struct Board {
    events: broadcast::Sender<Violation>,
    requests: mpsc::Sender<Request>,
    muted: watch::Receiver<Arc<HashSet<u128>>>,
    resets: watch::Receiver<Arc<Vec<Reset>>>,
    actor: Mutex<Option<Actor>>,
    counters: Counters,
    debugged: watch::Sender<Debugged>,
    traces: broadcast::Sender<TraceEvent>,
}

/// The board's metrics series: totals registered when the world starts,
/// and `anticheat.flags.<check>` and `anticheat.kicks.<check>` registered
/// the first time that check counts, so each handle is made once.
struct Counters {
    metrics: OnceLock<Metrics>,
    totals: OnceLock<[Series; 2]>,
    flags: Vec<OnceLock<Series>>,
    kicks: Vec<OnceLock<Series>>,
}

impl Counters {
    fn new() -> Self {
        let slots = || {
            (0..crate::checks::INFOS.len())
                .map(|_| OnceLock::new())
                .collect()
        };
        Self {
            metrics: OnceLock::new(),
            totals: OnceLock::new(),
            flags: slots(),
            kicks: slots(),
        }
    }

    fn start(&self, metrics: &Metrics) {
        let _ = self.metrics.set(metrics.clone());
        self.totals.get_or_init(|| {
            [
                metrics.series(FLAGS_SERIES, Kind::Counter),
                metrics.series(KICKS_SERIES, Kind::Counter),
            ]
        });
    }

    fn count(&self, check: &'static str, outcome: Outcome) {
        let Some(metrics) = self.metrics.get() else {
            return;
        };
        let Some(index) = crate::checks::INFOS.iter().position(|i| i.name == check) else {
            return;
        };
        let bump = |slots: &[OnceLock<Series>], total: usize, prefix: &str| {
            slots[index]
                .get_or_init(|| metrics.series(&format!("{prefix}.{check}"), Kind::Counter))
                .inc();
            if let Some(totals) = self.totals.get() {
                totals[total].inc();
            }
        };
        bump(&self.flags, 0, FLAGS_SERIES);
        if outcome == Outcome::Kicked {
            bump(&self.kicks, 1, KICKS_SERIES);
        }
    }
}

struct Actor {
    events: broadcast::Receiver<Violation>,
    requests: mpsc::Receiver<Request>,
    muted: watch::Sender<Arc<HashSet<u128>>>,
    resets: watch::Sender<Arc<Vec<Reset>>>,
    players: HashMap<u128, PlayerStats>,
    serial: u64,
    echoes: crate::checks::Echoes,
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

impl Board {
    pub fn new() -> Self {
        let (events, events_rx) = broadcast::channel(EVENTS);
        let (requests, requests_rx) = mpsc::channel(REQUESTS);
        let (muted_tx, muted) = watch::channel(Arc::default());
        let (resets_tx, resets) = watch::channel(Arc::default());
        Self {
            events,
            requests,
            muted,
            resets,
            actor: Mutex::new(Some(Actor {
                events: events_rx,
                requests: requests_rx,
                muted: muted_tx,
                resets: resets_tx,
                players: HashMap::new(),
                serial: 0,
                echoes: crate::checks::Echoes::default(),
            })),
            counters: Counters::new(),
            debugged: watch::channel(Arc::default()).0,
            traces: broadcast::channel(TRACES).0,
        }
    }

    /// Every counted violation from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Violation> {
        self.events.subscribe()
    }

    /// Counts `violation` in the metrics and tells every subscriber.
    pub fn publish(&self, violation: Violation) {
        self.counters.count(violation.check, violation.outcome);
        let _ = self.events.send(violation);
    }

    /// Traces `name`'s checks (`check` only, or all), or stops with
    /// `on` false; false when too many players are already traced.
    pub fn set_debug(&self, name: &str, check: Option<&'static str>, on: bool) -> bool {
        let name = name.to_ascii_lowercase();
        let mut done = true;
        self.debugged.send_modify(|map| {
            let map = Arc::make_mut(map);
            if !on {
                map.remove(&name);
            } else if map.len() < MAX_DEBUGGED || map.contains_key(&name) {
                map.insert(name, check);
            } else {
                done = false;
            }
        });
        done
    }

    /// The traced players, changed whenever one is added or removed.
    pub fn debugged(&self) -> watch::Receiver<Debugged> {
        self.debugged.subscribe()
    }

    /// Every debug trace from now on.
    pub fn traces(&self) -> broadcast::Receiver<TraceEvent> {
        self.traces.subscribe()
    }

    pub fn publish_trace(&self, event: TraceEvent) {
        let _ = self.traces.send(event);
    }

    /// Staff who turned their alerts off.
    pub fn muted(&self) -> Arc<HashSet<u128>> {
        self.muted.borrow().clone()
    }

    /// The recent resets, changed whenever one is added.
    pub fn resets(&self) -> watch::Receiver<Arc<Vec<Reset>>> {
        self.resets.clone()
    }

    /// Starts the actor; later calls do nothing.
    pub fn spawn(&self) {
        let actor = self.actor.lock().ok().and_then(|mut slot| slot.take());
        if let Some(actor) = actor {
            tuff_supervise::spawn(
                "the anticheat board",
                tuff_supervise::restarting("the anticheat board", actor, |actor| {
                    Box::pin(actor.run())
                }),
            );
        }
    }

    async fn ask<T>(&self, build: impl FnOnce(oneshot::Sender<T>) -> Request) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        self.requests.send(build(reply)).await.ok()?;
        answer.await.ok()
    }

    /// The history of the player called `name` (any case).
    pub async fn player(&self, name: &str) -> Option<PlayerStats> {
        let name = name.to_owned();
        self.ask(|reply| Request::Player { name, reply })
            .await
            .flatten()
    }

    /// Every player with a history, most recent first.
    pub async fn list(&self) -> Vec<PlayerStats> {
        self.ask(|reply| Request::List { reply })
            .await
            .unwrap_or_default()
    }

    /// Clears the history and levels of the player called `name`; their
    /// name as recorded, `None` when nobody by that name has a history.
    pub async fn reset(&self, name: &str, check: Option<&'static str>) -> Option<Arc<str>> {
        let name = name.to_owned();
        self.ask(|reply| Request::Reset { name, check, reply })
            .await
            .flatten()
    }

    /// How many other players sent this chat text in the last half
    /// minute; records this copy.
    pub async fn echoes(&self, text: &str, sender: u128) -> u32 {
        let Some(key) = crate::checks::echo_key(text) else {
            return 0;
        };
        let asked = self.ask(|reply| Request::Echo { key, sender, reply });
        tokio::time::timeout(ECHO_WAIT, asked)
            .await
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    /// Mutes or unmutes `player`'s alerts (`None` toggles); true when
    /// they now hear alerts.
    pub async fn set_alerts(&self, player: u128, on: Option<bool>) -> bool {
        self.ask(|reply| Request::Mute {
            player,
            muted: on.map(|on| !on),
            reply,
        })
        .await
        .unwrap_or(true)
    }
}

impl WorldService for Board {
    fn start(&self, shared: &Arc<Shared>) {
        self.counters.start(&shared.metrics);
        self.spawn();
    }
}

impl Actor {
    async fn run(&mut self) {
        loop {
            tokio::select! {
                biased;
                event = self.events.recv() => match event {
                    Ok(violation) => self.record(violation),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                request = self.requests.recv() => match request {
                    Some(request) => self.answer(request),
                    None => break,
                },
            }
        }
    }

    fn record(&mut self, violation: Violation) {
        if !self.players.contains_key(&violation.player) && self.players.len() >= MAX_PLAYERS {
            let oldest = self
                .players
                .values()
                .min_by_key(|p| p.last())
                .map(|p| p.player);
            if let Some(oldest) = oldest {
                self.players.remove(&oldest);
            }
        }
        let stats = self
            .players
            .entry(violation.player)
            .or_insert_with(|| PlayerStats {
                player: violation.player,
                name: violation.name.clone(),
                checks: BTreeMap::new(),
            });
        stats.name = violation.name;
        let check = stats
            .checks
            .entry(violation.check)
            .or_insert_with(|| CheckStats {
                flags: 0,
                weight: 0.0,
                kicks: 0,
                level: 0.0,
                last: Instant::now(),
                recent: Recent::default(),
            });
        check.recent.add(Instant::now());
        check.flags = check.flags.saturating_add(1);
        check.weight += violation.weight;
        check.level = violation.level;
        check.last = Instant::now();
        if violation.outcome == Outcome::Kicked {
            check.kicks = check.kicks.saturating_add(1);
        }
    }

    fn find(&self, name: &str) -> Option<&PlayerStats> {
        self.players
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    fn answer(&mut self, request: Request) {
        match request {
            #[cfg(test)]
            Request::Panic => panic!("test panic"),
            Request::Player { name, reply } => {
                let _ = reply.send(self.find(&name).cloned());
            }
            Request::List { reply } => {
                let mut all: Vec<PlayerStats> = self.players.values().cloned().collect();
                all.sort_by_key(|p| std::cmp::Reverse(p.last()));
                let _ = reply.send(all);
            }
            Request::Reset { name, check, reply } => {
                let found = self.find(&name).map(|p| (p.player, p.name.clone()));
                if let Some((player, _)) = found {
                    if let Some(stats) = self.players.get_mut(&player) {
                        match check {
                            Some(check) => {
                                stats.checks.remove(check);
                            }
                            None => stats.checks.clear(),
                        }
                        if stats.checks.is_empty() {
                            self.players.remove(&player);
                        }
                    }
                    self.serial += 1;
                    let reset = Reset {
                        serial: self.serial,
                        player,
                        check,
                    };
                    self.resets.send_modify(|resets| {
                        let list = Arc::make_mut(resets);
                        list.push(reset);
                        if list.len() > KEPT_RESETS {
                            list.remove(0);
                        }
                    });
                }
                let _ = reply.send(found.map(|(_, name)| name));
            }
            Request::Echo { key, sender, reply } => {
                let _ = reply.send(self.echoes.record(key, sender, Instant::now()));
            }
            Request::Mute {
                player,
                muted,
                reply,
            } => {
                let mut on = true;
                self.muted.send_modify(|set| {
                    let set = Arc::make_mut(set);
                    let mute = muted.unwrap_or(!set.contains(&player));
                    if mute {
                        set.insert(player);
                    } else {
                        set.remove(&player);
                    }
                    on = !mute;
                });
                let _ = reply.send(on);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violation(player: u128, name: &str, check: &'static str, outcome: Outcome) -> Violation {
        Violation {
            player,
            name: Arc::from(name),
            check,
            weight: 1.0,
            level: 2.0,
            outcome,
        }
    }

    async fn settle(board: &Board) {
        board.list().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn the_board_actor_counts_other_players_copies_of_chat() {
        let idle = Board::new();
        assert_eq!(
            idle.echoes("hello", 1).await,
            0,
            "an unstarted board answers nothing"
        );
        let board = Board::new();
        board.spawn();
        assert_eq!(board.echoes("Join my server!", 1).await, 0);
        assert_eq!(board.echoes("join my server", 1).await, 0);
        assert_eq!(board.echoes("JOIN MY SERVERRR", 2).await, 2);
        assert_eq!(board.echoes("join my server", 3).await, 3);
        assert_eq!(board.echoes("something else", 3).await, 0);
        assert_eq!(board.echoes("?!", 4).await, 0);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_board_keeping_history() {
        let board = Board::new();
        board.spawn();
        board.publish(violation(1, "Alex", "reach", Outcome::SetBack));
        settle(&board).await;
        board.requests.send(Request::Panic).await.unwrap();
        assert_eq!(board.player("alex").await.unwrap().flags(), 1);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn the_board_keeps_history_resets_and_mutes() {
        let board = Board::new();
        let mut subscriber = board.subscribe();
        board.spawn();
        board.publish(violation(1, "Alex", "reach", Outcome::SetBack));
        board.publish(violation(1, "Alex", "reach", Outcome::Kicked));
        board.publish(violation(2, "Steve", "fly", Outcome::Logged));
        assert_eq!(subscriber.recv().await.unwrap().check, "reach");
        settle(&board).await;
        let alex = board.player("alex").await.unwrap();
        assert_eq!(alex.flags(), 2);
        assert_eq!(alex.kicks(), 1);
        assert_eq!(board.list().await.len(), 2);

        let mut resets = board.resets();
        assert_eq!(board.reset("ALEX", None).await.as_deref(), Some("Alex"));
        assert!(resets.has_changed().unwrap());
        let list = resets.borrow_and_update().clone();
        assert_eq!(list[0].player, 1);
        assert_eq!(list[0].check, None);
        assert!(board.player("Alex").await.is_none());
        assert!(board.reset("Nobody", None).await.is_none());

        assert!(!board.set_alerts(9, None).await);
        assert!(board.muted().contains(&9));
        assert!(board.set_alerts(9, Some(true)).await);
        assert!(!board.muted().contains(&9));
    }

    #[tuff_core::watchdog]
    #[test]
    fn recent_counts_age_out_of_the_hour_and_day() {
        let start = Instant::now();
        let mut recent = Recent::default();
        recent.add(start);
        recent.add(start + Duration::from_secs(10));
        recent.add(start + Duration::from_secs(2 * 3600));
        let now = start + Duration::from_secs(2 * 3600 + 5);
        assert_eq!(recent.within(now, HOUR), 1);
        assert_eq!(recent.within(now, DAY), 3);
        recent.add(start + Duration::from_secs(25 * 3600));
        assert_eq!(
            recent.within(start + Duration::from_secs(25 * 3600), DAY),
            2
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn totals_sum_every_players_checks() {
        let board = Board::new();
        board.spawn();
        board.publish(violation(1, "Alex", "reach", Outcome::Logged));
        board.publish(violation(2, "Steve", "reach", Outcome::Kicked));
        board.publish(violation(2, "Steve", "fly", Outcome::Logged));
        settle(&board).await;
        let all = totals(&board.list().await, Instant::now());
        assert_eq!(
            all["reach"],
            CheckTotals {
                flags: 2,
                kicks: 1,
                hour: 2,
                day: 2
            }
        );
        assert_eq!(all["fly"].hour, 1);
    }
}
