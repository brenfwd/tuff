//! Snap turns: a large turn in the tick of an attack that swings back
//! within two ticks (an aura aiming for one hit), and spinning faster
//! than any mouse over a second. Both only feed the improbable score.
//! Login, teleports and their grace reset the tracking, since the server
//! sets the view then.

use std::collections::VecDeque;

use tuff_core::protocol::play::Serverbound;

use super::{Check, Facts, Fix, Flag};

const CHECK: &str = "rotation";
/// Degrees an attack tick must turn to count as a snap.
const SNAP: f32 = 40.0;
/// Fraction of the snap the following ticks must turn back.
const RETURN: f32 = 0.6;
const RETURN_TICKS: u8 = 2;
const WINDOW: usize = 20;
/// Degrees of turning within the window no player reaches.
const SPIN: f32 = 2400.0;

fn wrap(degrees: f32) -> f32 {
    let d = degrees.rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

#[derive(Default)]
pub struct SnapTurn {
    last: Option<[f32; 2]>,
    turn: [f32; 2],
    attacked: bool,
    snap: Option<([f32; 2], [f32; 2], u8)>,
    window: VecDeque<f32>,
    total: f32,
    weak: f64,
}

fn flag() -> Option<Flag> {
    Some(Flag {
        check: CHECK,
        weight: 1.0,
        fix: Fix::Pass,
    })
}

impl SnapTurn {
    fn reset(&mut self) {
        *self = SnapTurn::default();
    }

    fn end_tick(&mut self, quiet: bool) -> Option<Flag> {
        let turn = std::mem::take(&mut self.turn);
        let attacked = std::mem::take(&mut self.attacked);
        let size = turn[0].hypot(turn[1]);
        if !quiet {
            self.weak =
                crate::improbable::rotation_rate_weight(f64::from(turn[0]), f64::from(turn[1]));
        }
        self.window.push_back(size);
        self.total += size;
        if self.window.len() > WINDOW {
            self.total -= self.window.pop_front().unwrap_or(0.0);
        }
        if quiet {
            self.snap = None;
            return None;
        }
        if let Some((snap, returned, left)) = self.snap.take() {
            let returned = [returned[0] + turn[0], returned[1] + turn[1]];
            let back = -(returned[0] * snap[0] + returned[1] * snap[1]);
            if back >= RETURN * (snap[0] * snap[0] + snap[1] * snap[1]) {
                return flag();
            }
            if left > 1 {
                self.snap = Some((snap, returned, left - 1));
            }
        }
        if attacked && size >= SNAP {
            self.snap = Some((turn, [0.0, 0.0], RETURN_TICKS));
        }
        if self.total > SPIN {
            self.window.clear();
            self.total = 0.0;
            return flag();
        }
        None
    }
}

impl Check for SnapTurn {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        if facts.teleport_pending || facts.confirming {
            self.reset();
            return None;
        }
        match packet {
            Serverbound::Move(movement) => {
                if let Some((yaw, pitch)) = movement.rotation {
                    if let Some([y, p]) = self.last {
                        self.turn[0] += wrap(yaw - y);
                        self.turn[1] += pitch - p;
                    }
                    self.last = Some([yaw, pitch]);
                }
                None
            }
            Serverbound::Attack(_) => {
                self.attacked = true;
                None
            }
            Serverbound::ClientTickEnd => self.end_tick(facts.grace),
            _ => None,
        }
    }

    fn weak(&mut self) -> f64 {
        std::mem::take(&mut self.weak)
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;
    use tuff_core::protocol::play::Movement;

    fn look(yaw: f32, pitch: f32) -> Serverbound {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: None,
            rotation: Some((yaw, pitch)),
            on_ground: true,
        })
    }

    /// Plays ticks of (yaw, pitch, attack) and counts flags.
    fn play(check: &mut SnapTurn, f: &Facts, ticks: &[(f32, f32, bool)]) -> usize {
        let mut flags = 0;
        for &(yaw, pitch, attack) in ticks {
            if attack {
                flags += usize::from(check.examine(f, &Serverbound::Attack(7)).is_some());
            }
            flags += usize::from(check.examine(f, &look(yaw, pitch)).is_some());
            flags += usize::from(check.examine(f, &Serverbound::ClientTickEnd).is_some());
        }
        flags
    }

    #[tuff_core::watchdog]
    #[test]
    fn flicking_to_a_target_and_fighting_is_not_flagged() {
        let f = facts();
        let mut check = SnapTurn::default();
        let mut ticks = vec![(0.0, 0.0, false)];
        ticks.extend([(90.0, 10.0, true), (95.0, 12.0, false), (100.0, 10.0, true)]);
        ticks.extend((0..40).map(|n| (100.0 + (n as f32 * 0.7).sin() * 30.0, 5.0, n % 10 == 0)));
        ticks.extend((0..20).map(|n| (n as f32 * 100.0, 0.0, false)));
        assert_eq!(play(&mut check, &f, &ticks), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn snapping_to_hit_and_back_is_flagged_but_only_passes() {
        let f = facts();
        let mut check = SnapTurn::default();
        let snaps: Vec<(f32, f32, bool)> = (0..5)
            .flat_map(|_| {
                [
                    (0.0, 0.0, false),
                    (120.0, 20.0, true),
                    (0.0, 0.0, false),
                    (0.0, 0.0, false),
                ]
            })
            .collect();
        assert!(play(&mut check, &f, &snaps) >= 4);
        check.examine(&f, &Serverbound::Attack(1));
        check.examine(&f, &look(120.0, 0.0));
        check.examine(&f, &Serverbound::ClientTickEnd);
        check.examine(&f, &look(10.0, 0.0));
        let flag = check.examine(&f, &Serverbound::ClientTickEnd).unwrap();
        assert_eq!(flag.fix, Fix::Pass);
        assert_eq!(crate::improbable::weight_of(flag.check), Some(0.5));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_near_limit_turn_reports_a_weak_signal_once() {
        let f = facts();
        let mut check = SnapTurn::default();
        play(&mut check, &f, &[(0.0, 0.0, false), (170.0, 0.0, false)]);
        assert!(check.weak() > 0.0);
        assert_eq!(check.weak(), 0.0);
        play(&mut check, &f, &[(175.0, 0.0, false)]);
        assert_eq!(check.weak(), 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn spinning_is_flagged_and_teleports_and_grace_are_spared() {
        let f = facts();
        let mut check = SnapTurn::default();
        let spin: Vec<(f32, f32, bool)> = (0..40).map(|n| (n as f32 * 170.0, 0.0, false)).collect();
        assert!(play(&mut check, &f, &spin) > 0);
        let mut quiet = facts();
        quiet.grace = true;
        let mut fresh = SnapTurn::default();
        let snaps = [(0.0, 0.0, false), (150.0, 0.0, true), (0.0, 0.0, false)];
        assert_eq!(play(&mut fresh, &quiet, &snaps), 0);
        let mut moved = facts();
        moved.teleport_pending = true;
        fresh.examine(&moved, &look(170.0, 0.0));
        assert_eq!(play(&mut fresh, &f, &[(10.0, 0.0, false)]), 0);
    }
}
