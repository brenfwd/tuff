//! The velocity ledger: every velocity the server sends a player waits
//! for the move that shows the client applied it. That move, and the few
//! after it while the push dies down, may go as far as the push carries;
//! a push never applied grants nothing once it expires, and one the
//! client could have applied but ignored (anti-knockback) is flagged.

use tuff_core::world::session::VelocitySent;

use super::{Facts, Fix, Flag, Terrain};

const BASE_WINDOW: u64 = 10;
const MAX_ROUND_TRIP: u64 = 20;
const MIN_COMPONENT: f64 = 0.1;
const HORIZONTAL_TOLERANCE: f64 = 0.25;
const VERTICAL_TOLERANCE: f64 = 0.05;
const ADDED_SINK: f64 = 0.08;
const AIR_DRAG: f64 = 0.91;
const SLACK: f64 = 0.1;
const MAX_USES: u8 = 20;
const SPENT: f64 = 0.05;
const KEPT: usize = 8;
const UNPLACED_LIMIT: u8 = 1;

#[derive(Clone, Copy, Debug)]
struct Entry {
    seq: u64,
    added: bool,
    velocity: [f64; 3],
    deadline: u64,
    applied: bool,
    excused: bool,
    residual: f64,
    uses: u8,
    unplaced: u8,
}

/// The extra motion one move may carry because of the server.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Push {
    /// Horizontal distance on top of the move's own limit.
    pub horizontal: f64,
    /// The upward speed a push just gave, which restarts the air bound.
    pub launch: Option<f64>,
}

/// One velocity the server gave the client, as the client applies it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Impulse {
    /// Counts every velocity ever given, in order.
    pub seq: u64,
    pub velocity: [f64; 3],
    /// Added to the client's velocity (a blast) rather than replacing it.
    pub added: bool,
}

#[derive(Debug, Default)]
pub struct VelocityLedger {
    entries: Vec<Entry>,
    seen: u64,
}

fn horizontal(v: [f64; 3]) -> f64 {
    v[0].hypot(v[2])
}

/// Whether a move of `d` shows velocity `v` applied: along the push at
/// least its length less what the player's own input could take off,
/// and up at least as fast as it was launched.
fn reflects(d: [f64; 3], v: [f64; 3], added: bool) -> bool {
    let h = horizontal(v);
    let along = h < MIN_COMPONENT || (d[0] * v[0] + d[2] * v[2]) / h >= h - HORIZONTAL_TOLERANCE;
    let sink = if added { ADDED_SINK } else { 0.0 };
    let up = v[1] < MIN_COMPONENT || d[1] >= v[1] - VERTICAL_TOLERANCE - sink;
    along && up
}

impl VelocityLedger {
    /// Queues velocities sent since the last call; `sent` holds the
    /// session tick of each and `total` counts every one ever sent.
    pub fn record(&mut self, sent: &VelocitySent, latency_ms: i32) {
        if sent.total == self.seen {
            return;
        }
        let first = self.seen.max(sent.total - sent.recent.len() as u64);
        let fresh = (first..).zip(sent.since(self.seen));
        self.seen = sent.total;
        let round_trip = (u64::try_from(latency_ms.max(0)).unwrap_or(0) * 2).div_ceil(50);
        for (seq, given) in fresh {
            let (tick, velocity) = (given.tick, given.velocity);
            if self.entries.len() == KEPT {
                self.entries.remove(0);
            }
            self.entries.push(Entry {
                seq,
                added: given.added,
                velocity,
                deadline: tick + BASE_WINDOW + round_trip.min(MAX_ROUND_TRIP),
                applied: false,
                excused: false,
                residual: 0.0,
                uses: 0,
                unplaced: 0,
            });
        }
    }

    /// Every velocity still held, oldest first.
    pub fn impulses(&self) -> Vec<Impulse> {
        self.entries
            .iter()
            .map(|entry| Impulse {
                seq: entry.seq,
                velocity: entry.velocity,
                added: entry.added,
            })
            .collect()
    }

    /// Whether a push is waiting to show up or still carrying the player.
    pub fn active(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Settles one position move from `facts.position` to `to`: what extra
    /// motion it may carry, and a flag for each push the client ignored
    /// though nothing blocked it.
    pub fn settle(&mut self, facts: &Facts, to: [f64; 3], free: bool) -> (Push, Option<Flag>) {
        let d = [
            to[0] - facts.position[0],
            to[1] - facts.position[1],
            to[2] - facts.position[2],
        ];
        let terrain = facts.terrain.unwrap_or_default();
        let blocked = !free || blocking(terrain);
        let mut push = Push::default();
        let mut matched = false;
        for entry in &mut self.entries {
            entry.excused |= blocked;
            if entry.applied {
                entry.residual *= AIR_DRAG;
                entry.uses += 1;
                push.horizontal = push.horizontal.max(entry.residual);
            } else if !matched && reflects(d, entry.velocity, entry.added) {
                matched = true;
                entry.applied = true;
                entry.residual = horizontal(entry.velocity);
                push.horizontal = push.horizontal.max(entry.residual);
                if entry.velocity[1] > 0.0 {
                    push.launch = Some(push.launch.unwrap_or(0.0).max(entry.velocity[1]));
                }
            }
        }
        if push.horizontal > 0.0 {
            push.horizontal += SLACK;
        }
        let mut ignored = 0;
        self.entries.retain(|entry| {
            if entry.applied {
                let live = entry.uses < MAX_USES && entry.residual > SPENT;
                if !live {
                    ignored += usize::from(!entry.excused && entry.unplaced >= UNPLACED_LIMIT);
                }
                return live;
            }
            if facts.tick <= entry.deadline {
                return true;
            }
            ignored += usize::from(!entry.excused);
            false
        });
        let flag = (ignored > 0).then(|| Flag {
            check: "velocity",
            weight: ignored as f64,
            fix: Fix::Pass,
        });
        (push, flag)
    }

    /// A move under pending pushes that no replay placed: a push the client
    /// only partly applied counts once it dies down.
    pub fn unplaced(&mut self) {
        for entry in &mut self.entries {
            entry.unplaced = entry.unplaced.saturating_add(1);
        }
    }

    /// The player was moved by the server (a teleport, a respawn): pending
    /// pushes can no longer show up as sent.
    pub fn excuse_all(&mut self) {
        for entry in &mut self.entries {
            entry.excused = true;
        }
    }
}

fn blocking(terrain: Terrain) -> bool {
    !terrain.known || terrain.soft || terrain.cramped || terrain.phased
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    fn open() -> Facts {
        let mut f = facts();
        f.terrain = Some(Terrain {
            known: true,
            supported: true,
            ..Terrain::default()
        });
        f
    }

    fn knock(ledger: &mut VelocityLedger, f: &Facts, v: [f64; 3]) {
        let mut sent = VelocitySent {
            total: ledger.seen,
            ..VelocitySent::default()
        };
        sent.push(f.tick, v);
        ledger.record(&sent, f.latency_ms);
    }

    fn walk(ledger: &mut VelocityLedger, f: &mut Facts, d: [f64; 3]) -> (Push, Option<Flag>) {
        let to = [
            f.position[0] + d[0],
            f.position[1] + d[1],
            f.position[2] + d[2],
        ];
        let out = ledger.settle(f, to, true);
        f.position = to;
        f.tick += 1;
        out
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_blast_added_to_the_ground_sink_still_counts_as_applied() {
        let mut ledger = VelocityLedger::default();
        let mut f = open();
        let mut sent = VelocitySent::default();
        sent.push_added(f.tick, [0.0, 0.46, -0.59]);
        ledger.record(&sent, f.latency_ms);
        let (push, flag) = walk(&mut ledger, &mut f, [0.0, 0.38, -0.59]);
        assert_eq!(flag, None);
        assert!(push.horizontal > 0.5);
        for _ in 0..30 {
            assert_eq!(walk(&mut ledger, &mut f, [0.0, -0.1, -0.05]).1, None);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn applied_knockback_carries_the_player_and_never_flags() {
        let mut ledger = VelocityLedger::default();
        let mut f = open();
        f.latency_ms = 100;
        for _ in 0..50 {
            knock(&mut ledger, &f, [0.4, 0.4, 0.0]);
            for _ in 0..3 {
                assert_eq!(walk(&mut ledger, &mut f, [0.0; 3]).1, None);
            }
            let (push, flag) = walk(&mut ledger, &mut f, [0.32, 0.4, 0.0]);
            let received = tuff_core::protocol::codec::lp_vec3_received([0.4, 0.4, 0.0]);
            assert_eq!(flag, None);
            assert!(push.horizontal >= received[0]);
            assert_eq!(push.launch, Some(received[1]));
            let mut v = received[0];
            for _ in 0..6 {
                v *= AIR_DRAG;
                let (push, flag) = walk(&mut ledger, &mut f, [v, 0.0, 0.0]);
                assert_eq!(flag, None);
                assert!(push.horizontal >= v);
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn ignoring_knockback_is_flagged_every_time() {
        let mut ledger = VelocityLedger::default();
        let mut f = open();
        let mut flags = 0;
        for _ in 0..10 {
            knock(&mut ledger, &f, [0.4, 0.4, 0.0]);
            for _ in 0..15 {
                flags += usize::from(walk(&mut ledger, &mut f, [-0.2, 0.0, 0.0]).1.is_some());
            }
        }
        assert_eq!(flags, 10);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_blocked_push_is_excused() {
        let mut ledger = VelocityLedger::default();
        let mut f = open();
        f.terrain = Some(Terrain {
            cramped: true,
            ..f.terrain.unwrap()
        });
        knock(&mut ledger, &f, [0.4, 0.4, 0.0]);
        for _ in 0..30 {
            assert_eq!(walk(&mut ledger, &mut f, [0.0; 3]).1, None);
        }
        assert!(!ledger.active());
    }

    #[tuff_core::watchdog]
    #[test]
    fn stale_velocity_grants_nothing_later() {
        let mut ledger = VelocityLedger::default();
        let mut f = open();
        f.terrain = Some(Terrain::default());
        knock(&mut ledger, &f, [2.0, 0.0, 0.0]);
        for _ in 0..30 {
            walk(&mut ledger, &mut f, [0.0; 3]);
        }
        f.terrain = open().terrain;
        let (push, _) = walk(&mut ledger, &mut f, [2.0, 0.0, 0.0]);
        assert_eq!(push, Push::default());
        assert!(!ledger.active());
    }

    #[tuff_core::watchdog]
    #[test]
    fn one_push_is_used_up_after_it_dies_down() {
        let mut ledger = VelocityLedger::default();
        let mut f = open();
        knock(&mut ledger, &f, [1.0, 0.0, 0.0]);
        let mut used = 0;
        while walk(&mut ledger, &mut f, [1.0, 0.0, 0.0]).0.horizontal > 0.0 {
            used += 1;
            assert!(used <= usize::from(MAX_USES) + 1);
        }
        assert!(!ledger.active());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_push_the_engine_could_not_place_is_flagged_once_it_dies_down() {
        let mut ledger = VelocityLedger::default();
        let mut f = open();
        knock(&mut ledger, &f, [0.4, 0.0, 0.0]);
        let mut flags = 0;
        for i in 0..40 {
            let (_, flag) = walk(&mut ledger, &mut f, [0.35 * 0.91f64.powi(i), 0.0, 0.0]);
            flags += usize::from(flag.is_some());
            if i == 0 {
                ledger.unplaced();
            }
        }
        assert_eq!(flags, 1);
    }
}
