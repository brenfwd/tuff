//! Violation levels with decay, and the rate buckets checks share.
//!
//! Movement ladders follow a stricter policy: a repeat inside a short
//! window adds a bonus, decay waits for a run of clean moves after each
//! violation, and it stops while the player is in the air without a
//! clean descent, so a hovering cheat cannot wait its level away.

use tuff_core::protocol::play::GameMode;

use crate::checks::Facts;

/// Ladders that follow the movement policy.
pub const MOVEMENT: &[&str] = &["move_speed", "predict", "fly", "phase", "ground", "no_slow"];
/// Clean moves after a movement violation before its level decays.
pub const CLEAN_MOVES: u32 = 10;
/// A movement violation this soon after the last one adds the bonus.
pub const REPEAT_WINDOW: u64 = 20;
pub const REPEAT_BONUS: f64 = 0.5;
/// Airborne moves without a clean descent before the hover weight lands
/// on the `fly` ladder.
pub const HOVER_MOVES: u32 = 20;
pub const HOVER_WEIGHT: f64 = 2.0;
const DESCENT_STEP: f64 = 0.01;
const TERMINAL_FALL: f64 = -3.0;

pub fn is_movement(check: &str) -> bool {
    MOVEMENT.contains(&check)
}

/// What a check's level allows before the player is kicked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// Level lost per session tick.
    pub decay: f64,
    /// A level above this kicks.
    pub kick_above: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Setback,
    Kick,
}

/// One check's violation level for one player.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ladder {
    level: f64,
    at: u64,
    last: Option<u64>,
    hold: u32,
}

impl Ladder {
    pub fn level(&self, tick: u64, decay: f64) -> f64 {
        (self.level - tick.saturating_sub(self.at) as f64 * decay).max(0.0)
    }

    /// Adds `weight` at `tick`; a kick resets the level.
    pub fn add(&mut self, weight: f64, tick: u64, limits: Limits) -> Step {
        self.level = self.level(tick, limits.decay) + weight.clamp(0.0, 1.0e6);
        self.at = tick;
        if self.level > limits.kick_above {
            self.level = 0.0;
            Step::Kick
        } else {
            Step::Setback
        }
    }

    /// Level after multiplicative relaxation: it shrinks by `factor` each
    /// tick, so a high level falls fast and a low one lingers.
    pub fn level_relaxed(&self, tick: u64, factor: f64) -> f64 {
        let ticks = tick.saturating_sub(self.at).min(i32::MAX as u64) as i32;
        self.level * factor.clamp(0.0, 1.0).powi(ticks)
    }

    /// [`Ladder::add`] with multiplicative relaxation instead of linear
    /// decay; a kick resets the level.
    pub fn add_relaxing(&mut self, weight: f64, tick: u64, factor: f64, kick_above: f64) -> Step {
        self.level = self.level_relaxed(tick, factor) + weight.clamp(0.0, 1.0e6);
        self.at = tick;
        if self.level > kick_above {
            self.level = 0.0;
            Step::Kick
        } else {
            Step::Setback
        }
    }

    /// [`Ladder::add`] under the movement policy: a repeat within
    /// [`REPEAT_WINDOW`] adds [`REPEAT_BONUS`], and decay waits for
    /// [`CLEAN_MOVES`] clean moves.
    pub fn add_movement(&mut self, weight: f64, tick: u64, limits: Limits) -> Step {
        if weight <= 0.0 {
            return self.add(weight, tick, limits);
        }
        let repeat = self
            .last
            .is_some_and(|at| tick.saturating_sub(at) <= REPEAT_WINDOW);
        self.last = Some(tick);
        self.hold = CLEAN_MOVES;
        let bonus = if repeat { REPEAT_BONUS } else { 0.0 };
        self.add(weight + bonus, tick, limits)
    }

    /// One move under the movement policy: the level does not decay over
    /// this move's tick when it is frozen or clean moves are still owed;
    /// idle time between moves decays as usual.
    pub fn on_move(&mut self, tick: u64, frozen: bool, decay: f64) {
        if (frozen || self.hold > 0) && tick > self.at {
            let start = self.at.max(tick - 1);
            self.level = self.level(start, decay);
            self.at = tick;
        }
        if !frozen {
            self.hold = self.hold.saturating_sub(1);
        }
    }
}

/// What one move says about the movement policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Airtime {
    /// In the air without a clean descent: movement levels do not decay.
    pub frozen: bool,
    /// Long enough without contact that the hover weight applies.
    pub hover: bool,
}

/// Tracks airborne moves for the movement policy. Only moves the client
/// itself says are airborne count towards the hover weight, since the
/// block view cannot see entities a player may stand on (a false ground
/// claim is the ground check's business). A descent is clean
/// when it falls faster than the move before (gravity at work) or at
/// terminal speed; rising after a jump slows the same way. Hovering,
/// gliding at a constant speed and climbing are not.
#[derive(Debug, Default)]
pub struct AirClock {
    last_dy: Option<f64>,
    unclean: u32,
}

impl AirClock {
    pub fn observe(&mut self, facts: &Facts, to: [f64; 3], claims_ground: bool) -> Airtime {
        let terrain = facts.terrain.unwrap_or_default();
        let dy = to[1] - facts.position[1];
        let last_dy = self.last_dy.replace(dy);
        let airborne = terrain.known
            && !terrain.supported
            && !terrain.soft
            && !terrain.medium
            && !facts.teleport_pending
            && !facts.confirming
            && !facts.riding
            && !facts.flying
            && !facts.may_fly
            && !facts.instabuild
            && !facts.velocity
            && !facts.levitation
            && !facts.slow_falling
            && facts.game_mode != GameMode::Spectator;
        if !airborne {
            self.unclean = 0;
            return Airtime::default();
        }
        let clean = match last_dy {
            Some(last) => dy < last - DESCENT_STEP || dy <= TERMINAL_FALL,
            None => dy < 0.0,
        };
        if clean {
            self.unclean = 0;
            return Airtime::default();
        }
        let still = dy.abs() < 1.0e-9
            && (to[0] - facts.position[0]).abs() < 1.0e-9
            && (to[2] - facts.position[2]).abs() < 1.0e-9;
        if !still && !claims_ground {
            self.unclean += 1;
        }
        let hover = self.unclean >= HOVER_MOVES;
        if hover {
            self.unclean = 0;
        }
        Airtime {
            frozen: true,
            hover,
        }
    }
}

/// A leaky bucket: each event adds its cost, time drains it, and going
/// over `cap` means the rate is too high. The level never exceeds `cap`,
/// so a flood cannot build up unbounded debt.
#[derive(Clone, Copy, Debug)]
pub struct Bucket {
    level: f32,
    at: u64,
    cap: f32,
    drain: f32,
}

impl Bucket {
    pub const fn new(cap: f32, drain: f32) -> Bucket {
        Bucket {
            level: 0.0,
            at: 0,
            cap,
            drain,
        }
    }

    /// Fraction of the cap currently filled.
    pub fn fill(&self) -> f64 {
        f64::from(self.level / self.cap)
    }

    /// Whether the event stays within the rate; an excess event is not
    /// counted.
    pub fn allow(&mut self, cost: f32, tick: u64) -> bool {
        let elapsed = tick.saturating_sub(self.at) as f32;
        self.level = (self.level - elapsed * self.drain).max(0.0);
        self.at = self.at.max(tick);
        if self.level + cost > self.cap {
            false
        } else {
            self.level += cost;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        decay: 0.5,
        kick_above: 10.0,
    };

    #[tuff_core::watchdog]
    #[test]
    fn levels_decay_with_time_and_kick_past_the_limit() {
        let mut ladder = Ladder::default();
        assert_eq!(ladder.add(4.0, 0, LIMITS), Step::Setback);
        assert_eq!(ladder.add(4.0, 2, LIMITS), Step::Setback);
        assert!((ladder.level(2, LIMITS.decay) - 7.0).abs() < 1e-9);
        assert_eq!(ladder.add(4.0, 3, LIMITS), Step::Kick);
        assert_eq!(ladder.level(3, LIMITS.decay), 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn relaxing_ladder_shrinks_multiplicatively() {
        let mut ladder = Ladder::default();
        assert_eq!(ladder.add_relaxing(4.0, 0, 0.9, 10.0), Step::Setback);
        let after = ladder.level_relaxed(10, 0.9);
        assert!((after - 4.0 * 0.9f64.powi(10)).abs() < 1e-9);
        assert_eq!(ladder.add_relaxing(8.0, 0, 0.9, 10.0), Step::Kick);
        assert_eq!(ladder.level_relaxed(0, 0.9), 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn spaced_violations_never_accumulate() {
        let mut ladder = Ladder::default();
        for n in 0..100u64 {
            assert_eq!(ladder.add(6.0, n * 20, LIMITS), Step::Setback);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_huge_weight_kicks_at_once() {
        let mut ladder = Ladder::default();
        assert_eq!(ladder.add(f64::INFINITY, 0, LIMITS), Step::Kick);
    }

    const MOVE_LIMITS: Limits = Limits {
        decay: 0.05,
        kick_above: 25.0,
    };

    fn air() -> Facts {
        let mut f = crate::checks::facts();
        f.terrain = Some(crate::checks::Terrain {
            known: true,
            ..Default::default()
        });
        f
    }

    fn ground() -> Facts {
        let mut f = crate::checks::facts();
        f.terrain = Some(crate::checks::Terrain {
            known: true,
            supported: true,
            ..Default::default()
        });
        f
    }

    /// Steps a player through moves of `dy`, `in_air` telling which are
    /// airborne, applying the movement policy to `ladder`; the hover
    /// weights it would add are added too.
    fn fly(ladder: &mut Ladder, clock: &mut AirClock, tick: &mut u64, moves: &[(f64, bool)]) {
        let mut y = 64.0;
        for (dy, in_air) in moves {
            *tick += 1;
            let mut f = if *in_air { air() } else { ground() };
            f.tick = *tick;
            f.position = [0.0, y, 0.0];
            let to = [0.1, y + dy, 0.0];
            let airtime = clock.observe(&f, to, !*in_air);
            ladder.on_move(*tick, airtime.frozen, MOVE_LIMITS.decay);
            if airtime.hover {
                ladder.add_movement(HOVER_WEIGHT, *tick, MOVE_LIMITS);
            }
            y += dy;
        }
    }

    fn jump() -> Vec<(f64, bool)> {
        let mut moves = Vec::new();
        let mut vy: f64 = 0.42;
        let mut height = 0.0;
        loop {
            height += vy;
            if height <= 0.0 {
                moves.push((vy - height, false));
                break;
            }
            moves.push((vy, true));
            vy = (vy - 0.08) * 0.98;
        }
        moves
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_repeat_inside_the_window_adds_the_bonus() {
        let mut ladder = Ladder::default();
        ladder.add_movement(1.0, 0, MOVE_LIMITS);
        ladder.add_movement(1.0, 5, MOVE_LIMITS);
        assert!((ladder.level(5, MOVE_LIMITS.decay) - 2.25).abs() < 1e-9);
        let mut spaced = Ladder::default();
        spaced.add_movement(1.0, 0, MOVE_LIMITS);
        spaced.add_movement(1.0, 100, MOVE_LIMITS);
        assert!((spaced.level(100, MOVE_LIMITS.decay) - 1.0).abs() < 1e-9);
        let mut corrected = Ladder::default();
        corrected.add_movement(0.0, 0, MOVE_LIMITS);
        corrected.add_movement(0.0, 1, MOVE_LIMITS);
        assert_eq!(corrected.level(1, MOVE_LIMITS.decay), 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn decay_waits_for_clean_moves() {
        let mut ladder = Ladder::default();
        ladder.add_movement(2.0, 0, MOVE_LIMITS);
        for tick in 1..u64::from(CLEAN_MOVES) {
            ladder.on_move(tick, false, MOVE_LIMITS.decay);
        }
        let held = ladder.level(u64::from(CLEAN_MOVES) - 1, MOVE_LIMITS.decay);
        assert!((held - 2.0).abs() < 1e-9);
        for tick in u64::from(CLEAN_MOVES)..100 {
            ladder.on_move(tick, false, MOVE_LIMITS.decay);
        }
        assert!(ladder.level(100, MOVE_LIMITS.decay) < 2.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn idle_time_before_a_frozen_move_still_decays() {
        let mut ladder = Ladder::default();
        ladder.add_movement(2.0, 0, MOVE_LIMITS);
        for tick in 1..=u64::from(CLEAN_MOVES) {
            ladder.on_move(tick, false, MOVE_LIMITS.decay);
        }
        ladder.on_move(30, true, MOVE_LIMITS.decay);
        let level = ladder.level(30, MOVE_LIMITS.decay);
        assert!(
            (level - (2.0 - 19.0 * MOVE_LIMITS.decay)).abs() < 1e-9,
            "{level}"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn bunnyhopping_decays_to_zero() {
        let mut ladder = Ladder::default();
        let mut clock = AirClock::default();
        let mut tick = 0;
        ladder.add_movement(3.0, tick, MOVE_LIMITS);
        let hops: Vec<(f64, bool)> = (0..20).flat_map(|_| jump()).collect();
        fly(&mut ladder, &mut clock, &mut tick, &hops);
        assert!(tick > 150);
        assert_eq!(ladder.level(tick, MOVE_LIMITS.decay), 0.0);
        let mut clock = AirClock::default();
        let f = air();
        let frozen = jump()
            .iter()
            .filter(|(_, in_air)| *in_air)
            .filter(|(dy, _)| clock.observe(&f, [0.0, 64.0 + dy, 0.0], false).frozen)
            .count();
        assert!(frozen <= 1, "only the take-off is unclean, saw {frozen}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_hovering_flyer_keeps_accumulating() {
        let mut ladder = Ladder::default();
        let mut clock = AirClock::default();
        let mut tick = 0;
        ladder.add_movement(3.0, tick, MOVE_LIMITS);
        let mut last = ladder.level(tick, MOVE_LIMITS.decay);
        for _ in 0..4 {
            fly(&mut ladder, &mut clock, &mut tick, &[(0.0, true); 40]);
            let level = ladder.level(tick, MOVE_LIMITS.decay);
            assert!(level > last, "{level} after {last}");
            last = level;
        }
        let mut glider = Ladder::default();
        let mut clock = AirClock::default();
        let mut tick = 0;
        glider.add_movement(3.0, tick, MOVE_LIMITS);
        fly(&mut glider, &mut clock, &mut tick, &[(-0.04, true); 200]);
        assert!(glider.level(tick, MOVE_LIMITS.decay) >= 3.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn standing_on_something_unseen_is_frozen_but_never_weighs() {
        let mut ladder = Ladder::default();
        let mut clock = AirClock::default();
        let f = air();
        for tick in 0..500 {
            let airtime = clock.observe(&f, [0.1, 64.0, 0.0], true);
            assert!(airtime.frozen && !airtime.hover);
            ladder.on_move(tick, airtime.frozen, MOVE_LIMITS.decay);
        }
        assert_eq!(ladder.level(500, MOVE_LIMITS.decay), 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn buckets_allow_a_burst_then_the_drain_rate() {
        let mut bucket = Bucket::new(4.0, 0.5);
        assert!((0..4).all(|_| bucket.allow(1.0, 0)));
        assert!(!bucket.allow(1.0, 0));
        assert!(bucket.allow(1.0, 2));
        assert!(!bucket.allow(1.0, 2));
        assert!(bucket.allow(1.0, 1000));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_flood_does_not_build_debt() {
        let mut bucket = Bucket::new(4.0, 1.0);
        for _ in 0..10_000 {
            bucket.allow(1.0, 0);
        }
        assert!(bucket.allow(1.0, 1));
    }
}
