use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

use super::budget::{BudgetLimits, PacketBudget, TickClock};
use super::{Check, Facts, Fix, Flag};

const FACE_SLACK: f64 = 1.0;
const SWING_WINDOW: u64 = 40;
/// Distinct blocks remembered within one client tick; more all count.
const TICK_MEMORY: usize = 8;
const STALE_TICKS: u64 = 20;
/// With the ladder's limit and decay, a second wrong finish within ten
/// seconds kicks while an isolated one only sets back.
pub const WRONG_BLOCK_WEIGHT: f64 = 8.0;

/// The block-aware half of mining: breaking air or liquid, a face the
/// eyes cannot see, finishing a block other than the one started, no arm
/// swing while mining, and starting blocks faster than the game's client
/// can. That client starts at most one distinct block per client tick
/// (instant breaks included, in every game mode) and ticks at most 20
/// times a second, so a second block before the client's tick end is a
/// nuker and new blocks beyond a real-time budget are `break_rate`.
pub struct BlockBreak {
    started: Option<(BlockPos, u64)>,
    swung: bool,
    this_tick: Vec<BlockPos>,
    beyond: usize,
    tick_ends: bool,
    opened_at: u64,
    budget: PacketBudget,
    clock: TickClock,
    rate_flagged_at: Option<u64>,
}

impl Default for BlockBreak {
    fn default() -> Self {
        Self {
            started: None,
            swung: false,
            this_tick: Vec::new(),
            beyond: 0,
            tick_ends: false,
            opened_at: 0,
            budget: PacketBudget::new(BudgetLimits::BREAKS),
            clock: TickClock::default(),
            rate_flagged_at: None,
        }
    }
}

fn air_like(state: BlockState) -> bool {
    matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir) || state.is_liquid()
}

/// Whether a ray from `eye` could hit `face` of the block at `pos`, with
/// slack for the eye moving between the client's raycast and this packet.
fn face_visible(eye: [f64; 3], pos: BlockPos, face: i8) -> bool {
    let min = [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)];
    let (axis, positive) = match face {
        0 => (1, false),
        1 => (1, true),
        2 => (2, false),
        3 => (2, true),
        4 => (0, false),
        5 => (0, true),
        _ => return false,
    };
    if positive {
        eye[axis] > min[axis] - FACE_SLACK
    } else {
        eye[axis] < min[axis] + 1.0 + FACE_SLACK
    }
}

/// Undo the client's prediction: the session resends the block and
/// acknowledges the sequence.
fn undo(position: BlockPos, sequence: i32, check: &'static str, weight: f64) -> Option<Flag> {
    Some(Flag {
        check,
        weight,
        fix: Fix::Correct(Box::new(Serverbound::PlayerAction {
            status: PlayerActionStatus::AbortDestroyBlock,
            position,
            face: 0,
            sequence,
        })),
    })
}

impl BlockBreak {
    fn end_tick(&mut self, tick: u64) {
        self.this_tick.clear();
        self.beyond = 0;
        self.opened_at = tick;
    }

    /// Counts a start against the client tick and the real-time budget.
    /// A client that never sent a tick end has its ticks bounded by the
    /// session's instead, and a tick end this check did not see (an
    /// exempt stretch) is assumed after a second.
    fn rate(&mut self, facts: &Facts, position: BlockPos) -> Option<(&'static str, f64)> {
        let since = facts.tick.saturating_sub(self.opened_at);
        if since > STALE_TICKS || (!self.tick_ends && since > 0) {
            self.end_tick(facts.tick);
        }
        if self.this_tick.contains(&position) {
            return None;
        }
        if self.this_tick.len() < TICK_MEMORY {
            self.this_tick.push(position);
        } else {
            self.beyond += 1;
        }
        if self.this_tick.len() + self.beyond > 1 {
            return Some(("nuker", 2.0));
        }
        let now = self.clock.now(facts.now, facts.tick);
        self.budget.record(now)?;
        let first = self.rate_flagged_at != Some(facts.tick);
        self.rate_flagged_at = Some(facts.tick);
        Some(("break_rate", if first { 1.0 } else { 0.0 }))
    }

    fn start(&mut self, facts: &Facts, position: BlockPos, face: i8, seq: i32) -> Option<Flag> {
        let unswung = matches!(self.started, Some((_, at)) if !self.swung
            && facts.tick.saturating_sub(at) <= SWING_WINDOW);
        self.started = Some((position, facts.tick));
        self.swung = false;
        let rate = self.rate(facts, position);
        let shape = if facts.target.is_some_and(air_like) {
            Some(("air_break", 2.0))
        } else if !face_visible(facts.eye, position, face) {
            Some(("break_face", 2.0))
        } else if unswung {
            Some(("no_swing", 1.0))
        } else {
            None
        };
        let (check, weight) = match (shape, rate) {
            (Some(s), Some(r)) if r.1 > s.1 => r,
            (Some(s), _) => s,
            (None, r) => r?,
        };
        undo(position, seq, check, weight)
    }

    fn stop(&mut self, facts: &Facts, position: BlockPos, seq: i32) -> Option<Flag> {
        if facts.instabuild {
            return None;
        }
        let started = self.started.take();
        let swung = std::mem::replace(&mut self.swung, true);
        match started {
            Some((start, _)) if start != position => {
                undo(position, seq, "wrong_block", WRONG_BLOCK_WEIGHT)
            }
            Some(_) if !swung => undo(position, seq, "no_swing", 1.0),
            _ => None,
        }
    }
}

impl Check for BlockBreak {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        match packet {
            Serverbound::Punch => {
                self.swung = true;
                None
            }
            Serverbound::ClientTickEnd => {
                self.tick_ends = true;
                self.end_tick(facts.tick);
                None
            }
            Serverbound::PlayerAction {
                status,
                position,
                face,
                sequence,
            } => match status {
                PlayerActionStatus::StartDestroyBlock => {
                    self.start(facts, *position, *face, *sequence)
                }
                PlayerActionStatus::StopDestroyBlock => self.stop(facts, *position, *sequence),
                PlayerActionStatus::AbortDestroyBlock => {
                    if self.started.is_some_and(|(p, _)| p == *position) {
                        self.started = None;
                        self.swung = true;
                    }
                    None
                }
                _ => None,
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::{defaults, facts, info_of};
    use super::*;
    use crate::violations::{Ladder, Limits, Step};

    fn action(status: PlayerActionStatus, x: i32, face: i8) -> Serverbound {
        Serverbound::PlayerAction {
            status,
            position: BlockPos::new(x, 63, 0),
            face,
            sequence: 7,
        }
    }

    fn start(x: i32) -> Serverbound {
        action(PlayerActionStatus::StartDestroyBlock, x, 1)
    }

    fn stop(x: i32) -> Serverbound {
        action(PlayerActionStatus::StopDestroyBlock, x, 1)
    }

    fn abort(x: i32) -> Serverbound {
        action(PlayerActionStatus::AbortDestroyBlock, x, 1)
    }

    fn stone() -> Facts {
        let mut f = facts();
        f.eye = [0.5, 65.62, 0.5];
        f.target = Some(Block::Stone.default_state());
        f
    }

    fn grass() -> Facts {
        let mut f = stone();
        f.target = Some(Block::ShortGrass.default_state());
        f
    }

    fn name(flag: Option<Flag>) -> Option<&'static str> {
        flag.map(|f| f.check)
    }

    /// Every default check plus the violation ladders, as the interceptor
    /// runs them: the heaviest flag of a packet climbs its ladder.
    struct Session {
        checks: Vec<Box<dyn Check>>,
        ladders: Vec<(&'static str, Ladder)>,
        facts: Facts,
        start: Instant,
        flags: Vec<Flag>,
        kicked: Option<&'static str>,
    }

    impl Session {
        fn new(facts: Facts) -> Self {
            Self {
                checks: defaults(),
                ladders: Vec::new(),
                start: facts.now,
                facts,
                flags: Vec::new(),
                kicked: None,
            }
        }

        /// Moves the arrival clock to `ms` after the start.
        fn at(&mut self, ms: u64) {
            self.facts.now = self.start + Duration::from_millis(ms);
            self.facts.tick = ms / 50;
        }

        fn send(&mut self, packet: &Serverbound) -> Option<Flag> {
            let mut worst: Option<Flag> = None;
            for check in &mut self.checks {
                if let Some(flag) = check.examine(&self.facts, packet)
                    && worst.as_ref().is_none_or(|w| flag.weight > w.weight)
                {
                    worst = Some(flag);
                }
            }
            let flag = worst?;
            let info = info_of(flag.check).expect("known check");
            let limits = Limits {
                decay: info.decay,
                kick_above: info.kick_above,
            };
            let index = match self.ladders.iter().position(|(n, _)| *n == flag.check) {
                Some(index) => index,
                None => {
                    self.ladders.push((flag.check, Ladder::default()));
                    self.ladders.len() - 1
                }
            };
            if self.ladders[index]
                .1
                .add(flag.weight, self.facts.tick, limits)
                == Step::Kick
            {
                self.kicked.get_or_insert(flag.check);
            }
            self.flags.push(flag.clone());
            Some(flag)
        }

        /// One client tick that starts `x` with the swing and tick end
        /// the game's client sends after it.
        fn instant(&mut self, x: i32) {
            self.send(&start(x));
            self.send(&Serverbound::Punch);
            self.send(&Serverbound::ClientTickEnd);
        }

        fn names(&self) -> Vec<&'static str> {
            self.flags.iter().map(|f| f.check).collect()
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn sweeping_instant_blocks_at_the_client_maximum_never_flags() {
        let mut s = Session::new(grass());
        for n in 0..2400u64 {
            s.at(n * 50);
            s.instant(i32::try_from(n % 64).unwrap());
        }
        assert!(s.flags.is_empty(), "{:?}", s.names());
    }

    #[tuff_core::watchdog]
    #[test]
    fn lag_bunched_and_jittered_sweeps_never_flag() {
        struct Sweep {
            s: Session,
            client: u64,
        }
        impl Sweep {
            fn ticks(&mut self, ticks: u64, delay: impl Fn(u64) -> u64) {
                for _ in 0..ticks {
                    let sent = self.client * 50;
                    self.s.at(sent + delay(self.client));
                    self.s.instant(i32::try_from(self.client % 64).unwrap());
                    self.client += 1;
                }
            }
        }
        let mut w = Sweep {
            s: Session::new(grass()),
            client: 0,
        };
        w.ticks(200, |_| 40);
        for stall in [200u64, 500, 1000, 1500, 2000] {
            let resume = w.client * 50 + stall;
            w.ticks(stall / 50, |c| resume.saturating_sub(c * 50) + 40);
            w.ticks(200, |c| 40 + (c * 37) % 90);
        }
        assert!(w.s.flags.is_empty(), "{:?}", w.s.names());
    }

    #[tuff_core::watchdog]
    #[test]
    fn creative_clicking_and_holding_never_flag() {
        let mut f = grass();
        f.instabuild = true;
        let mut s = Session::new(f);
        for n in 0..600u64 {
            s.at(n * 50);
            let x = i32::try_from(n % 32).unwrap();
            if n < 300 || n % 6 == 0 {
                s.instant(x);
            } else {
                s.send(&Serverbound::Punch);
                s.send(&Serverbound::ClientTickEnd);
            }
        }
        for n in 600..700u64 {
            s.at(n * 50);
            for _ in 0..3 {
                s.send(&start(0));
                s.send(&Serverbound::Punch);
            }
            s.send(&Serverbound::ClientTickEnd);
        }
        assert!(s.flags.is_empty(), "{:?}", s.names());
    }

    #[tuff_core::watchdog]
    #[test]
    fn haste_instant_mining_of_stone_never_flags() {
        let mut f = stone();
        f.mining = Some(super::super::Mining {
            lenient: 1.2,
            actual: 1.2,
            factor: 1.0,
            strict: false,
        });
        let mut s = Session::new(f);
        for n in 0..1200u64 {
            s.at(n * 50);
            s.instant(i32::try_from(n % 9).unwrap());
        }
        assert!(s.flags.is_empty(), "{:?}", s.names());
    }

    #[tuff_core::watchdog]
    #[test]
    fn efficiency_five_haste_two_instant_mining_of_every_instant_class_never_flags() {
        let blocks = [
            Block::ShortGrass,
            Block::Torch,
            Block::Dandelion,
            Block::Stone,
            Block::Dirt,
            Block::OakLeaves,
        ];
        for block in blocks {
            let mut f = grass();
            f.target = Some(block.default_state());
            f.mining = Some(super::super::Mining {
                lenient: 40.0,
                actual: 40.0,
                factor: 1.0,
                strict: false,
            });
            let mut s = Session::new(f);
            for n in 0..1200u64 {
                s.at(n * 50);
                s.instant(i32::try_from(n % 64).unwrap());
            }
            assert!(s.flags.is_empty(), "{block:?}: {:?}", s.names());
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_long_stall_flushed_at_one_instant_is_rate_limited_but_never_a_nuker() {
        let mut s = Session::new(grass());
        for n in 0..200u64 {
            s.at(n * 50 + 40);
            s.instant(i32::try_from(n % 64).unwrap());
        }
        let resume = 200 * 50 + 3000;
        for n in 200..260u64 {
            s.at(resume);
            s.instant(i32::try_from(n % 64).unwrap());
        }
        assert!(!s.names().contains(&"nuker"), "{:?}", s.names());
        assert!(s.kicked.is_none(), "{:?}", s.kicked);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_swing_after_the_stop_or_in_a_later_tick_never_flags() {
        let mut s = Session::new(stone());
        s.at(1000);
        s.send(&start(0));
        s.send(&Serverbound::ClientTickEnd);
        for n in 1..30u64 {
            s.at(1000 + n * 50);
            s.send(&Serverbound::Punch);
            s.send(&Serverbound::ClientTickEnd);
        }
        s.send(&stop(0));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
        s.at(1000 + 31 * 50);
        s.send(&start(1));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
        assert!(s.flags.is_empty(), "{:?}", s.names());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_hold_without_swings_past_the_window_is_flagged_on_the_next_start() {
        let mut check = BlockBreak::default();
        let mut f = stone();
        assert_eq!(name(check.examine(&f, &start(0))), None);
        f.tick += super::SWING_WINDOW;
        assert_eq!(name(check.examine(&f, &start(1))), Some("no_swing"));
        let mut check = BlockBreak::default();
        let mut f = stone();
        check.examine(&f, &start(0));
        f.tick += super::SWING_WINDOW + 1;
        assert_eq!(name(check.examine(&f, &start(1))), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_nuker_breaking_many_blocks_in_one_tick_is_set_back_and_kicked() {
        let mut s = Session::new(grass());
        s.at(1000);
        for x in 0..6 {
            s.send(&start(x));
            s.send(&Serverbound::Punch);
        }
        s.send(&Serverbound::ClientTickEnd);
        assert_eq!(s.names(), vec!["nuker"; 5]);
        assert!(
            s.flags
                .iter()
                .all(|f| matches!(&f.fix, Fix::Correct(p) if matches!(**p,
                    Serverbound::PlayerAction { status: PlayerActionStatus::AbortDestroyBlock, .. })))
        );
        assert_eq!(s.kicked, None);
        s.at(1050);
        for x in 0..4 {
            s.send(&start(x));
            s.send(&Serverbound::Punch);
        }
        assert_eq!(s.kicked, Some("nuker"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_nuker_faking_tick_ends_is_held_to_real_time_and_kicked() {
        let mut s = Session::new(grass());
        let mut x = 0;
        for n in 0..60u64 {
            s.at(10_000 + n * 50);
            for _ in 0..5 {
                s.instant(x);
                x += 1;
            }
            if s.kicked.is_some() {
                break;
            }
        }
        assert_eq!(s.kicked, Some("break_rate"));
        let allowed = x - i32::try_from(s.flags.len()).unwrap();
        assert!(allowed < 80, "{allowed} of {x} breaks passed");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_nuker_one_tick_faster_than_the_client_is_kicked() {
        let mut s = Session::new(grass());
        for n in 0..1200u64 {
            s.at(n * 40);
            s.instant(i32::try_from(n % 64).unwrap());
            if s.kicked.is_some() {
                break;
            }
        }
        assert_eq!(s.kicked, Some("break_rate"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn without_tick_ends_session_ticks_bound_the_client_tick() {
        let mut check = BlockBreak::default();
        let mut f = grass();
        for x in 0..40 {
            f.tick = u64::try_from(x).unwrap();
            assert_eq!(check.examine(&f, &start(x)), None);
            check.examine(&f, &Serverbound::Punch);
        }
        check.examine(&f, &start(100));
        check.examine(&f, &Serverbound::Punch);
        assert_eq!(name(check.examine(&f, &start(101))), Some("nuker"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_unseen_tick_end_does_not_merge_client_ticks() {
        let mut check = BlockBreak::default();
        let mut f = grass();
        check.examine(&f, &Serverbound::ClientTickEnd);
        check.examine(&f, &start(0));
        check.examine(&f, &Serverbound::Punch);
        f.tick = 400;
        assert_eq!(check.examine(&f, &start(1)), None);
        check.examine(&f, &Serverbound::Punch);
        assert_eq!(name(check.examine(&f, &start(2))), Some("nuker"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn legit_mining_with_swings_is_never_flagged() {
        let mut check = BlockBreak::default();
        let mut f = stone();
        for x in 0..40 {
            f.tick = u64::try_from(x).unwrap() * 8;
            assert_eq!(check.examine(&f, &start(x % 3)), None);
            assert_eq!(check.examine(&f, &Serverbound::Punch), None);
            f.tick += 6;
            assert_eq!(check.examine(&f, &Serverbound::Punch), None);
            assert_eq!(check.examine(&f, &stop(x % 3)), None);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn air_and_liquids_cannot_be_broken() {
        let mut check = BlockBreak::default();
        let mut f = stone();
        f.target = Some(Block::Air.default_state());
        assert_eq!(name(check.examine(&f, &start(0))), Some("air_break"));
        check.examine(&f, &Serverbound::Punch);
        f.target = Some(Block::Water.default_state());
        assert_eq!(name(check.examine(&f, &start(0))), Some("air_break"));
        check.examine(&f, &Serverbound::Punch);
        f.target = None;
        assert_eq!(check.examine(&f, &start(0)), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn hidden_faces_are_flagged() {
        let mut check = BlockBreak::default();
        let mut f = stone();
        let below = action(PlayerActionStatus::StartDestroyBlock, 0, 0);
        assert_eq!(name(check.examine(&f, &below)), Some("break_face"));
        check.examine(&f, &Serverbound::Punch);
        let bogus = action(PlayerActionStatus::StartDestroyBlock, 0, 9);
        assert_eq!(name(check.examine(&f, &bogus)), Some("break_face"));
        check.examine(&f, &Serverbound::Punch);
        f.tick = 1;
        let east = action(PlayerActionStatus::StartDestroyBlock, -1, 5);
        assert_eq!(check.examine(&f, &east), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn mining_without_swinging_is_flagged() {
        let mut check = BlockBreak::default();
        let mut f = stone();
        check.examine(&f, &start(0));
        f.tick = 10;
        assert_eq!(name(check.examine(&f, &stop(0))), Some("no_swing"));
        check.examine(&f, &start(1));
        f.tick = 11;
        assert_eq!(name(check.examine(&f, &start(2))), Some("no_swing"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn finishing_another_block_is_wrong_block() {
        let mut check = BlockBreak::default();
        let f = stone();
        check.examine(&f, &start(0));
        check.examine(&f, &Serverbound::Punch);
        let flag = check.examine(&f, &stop(1)).unwrap();
        assert_eq!(flag.check, "wrong_block");
        assert!(matches!(flag.fix, Fix::Correct(_)));
    }

    /// Mines `x` from start to finish over `ticks` client ticks.
    fn mine(s: &mut Session, from: u64, ticks: u64, x: i32) -> u64 {
        s.at(from * 50);
        s.instant(x);
        for t in 1..ticks {
            s.at((from + t) * 50);
            s.send(&Serverbound::Punch);
            s.send(&Serverbound::ClientTickEnd);
        }
        s.at((from + ticks) * 50);
        s.send(&stop(x));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
        from + ticks + 1
    }

    fn wrong_finish(s: &mut Session, at: u64) {
        s.at(at * 50);
        s.instant(0);
        s.at(at * 50 + 300);
        s.send(&stop(1));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_second_wrong_finish_soon_after_the_first_kicks() {
        for gap in [1u64, 20, 60, 150] {
            let mut s = Session::new(stone());
            wrong_finish(&mut s, 1000);
            assert_eq!(s.names(), vec!["wrong_block"]);
            assert_eq!(s.kicked, None, "one wrong finish only sets back");
            wrong_finish(&mut s, 1000 + gap);
            assert_eq!(s.kicked, Some("wrong_block"), "gap {gap}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn isolated_wrong_finishes_only_set_back() {
        let mut s = Session::new(stone());
        for n in 0..20 {
            wrong_finish(&mut s, 1000 + n * 600);
        }
        assert_eq!(s.names(), vec!["wrong_block"; 20]);
        assert_eq!(s.kicked, None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_clients_abort_start_and_finish_sequences_never_flag() {
        let mut s = Session::new(stone());
        let mut tick = mine(&mut s, 100, 8, 0);
        tick = mine(&mut s, tick + 5, 8, 1);
        s.at(tick * 50);
        s.instant(2);
        s.at((tick + 3) * 50);
        s.send(&abort(2));
        s.send(&start(3));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
        s.at((tick + 4) * 50);
        s.send(&abort(3));
        s.send(&start(3));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
        s.at((tick + 6) * 50);
        s.send(&action(PlayerActionStatus::ChangeDestroyDirection, 3, 4));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
        s.at((tick + 12) * 50);
        s.send(&stop(3));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
        tick += 18;
        for x in 10..20 {
            s.at(tick * 50);
            s.instant(x);
            tick += 1;
        }
        tick = mine(&mut s, tick, 6, 20);
        s.at(tick * 50);
        s.send(&action(PlayerActionStatus::StartDestroyBlock, 21, 0));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
        s.at((tick + 8) * 50);
        s.send(&stop(21));
        assert_eq!(s.names(), vec!["break_face"], "only the hidden face");
        assert_eq!(s.kicked, None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn retargeting_sends_abort_then_start_in_one_tick_never_flags() {
        let mut s = Session::new(stone());
        let mut tick = mine(&mut s, 100, 10, 0);
        s.at(tick * 50);
        s.send(&start(1));
        s.send(&Serverbound::Punch);
        s.send(&Serverbound::ClientTickEnd);
        for x in 2..30 {
            tick += 1;
            s.at(tick * 50);
            s.send(&abort(x - 1));
            s.send(&start(x));
            s.send(&Serverbound::Punch);
            s.send(&Serverbound::ClientTickEnd);
        }
        assert!(s.flags.is_empty(), "{:?}", s.names());
    }

    #[tuff_core::watchdog]
    #[test]
    fn held_cadences_of_six_ticks_never_flag() {
        let mut creative = grass();
        creative.instabuild = true;
        let mut s = Session::new(creative);
        for n in 0..300u64 {
            s.at(n * 50);
            if n % 6 == 0 {
                s.instant(i32::try_from(n / 6).unwrap());
            } else {
                s.send(&Serverbound::Punch);
                s.send(&Serverbound::ClientTickEnd);
            }
        }
        assert!(s.flags.is_empty(), "creative: {:?}", s.names());

        let mut s = Session::new(stone());
        let mut tick = 100;
        for x in 0..60 {
            tick = mine(&mut s, tick, 1, x) + 5;
        }
        assert!(s.flags.is_empty(), "survival: {:?}", s.names());
    }

    #[tuff_core::watchdog]
    #[test]
    fn queued_clicks_in_one_tick_at_one_position_never_flag() {
        let mut s = Session::new(grass());
        for n in 0..200u64 {
            s.at(n * 50);
            let x = i32::try_from(n % 8).unwrap();
            for _ in 0..4 {
                s.send(&start(x));
                s.send(&Serverbound::Punch);
            }
            s.send(&Serverbound::ClientTickEnd);
        }
        let breaks = s.names().into_iter().filter(|n| *n != "swing_rate");
        assert_eq!(breaks.count(), 0, "{:?}", s.names());
    }
}
