use tuff_core::protocol::play::{GameMode, Movement, Serverbound};

use super::{Check, Facts, Fix, Flag};
use crate::trace::{Tags, Trace};

const TOLERATED_MOVES: u32 = 2;
const DESCENT: f64 = -0.05;

/// An on-ground flag with nothing under the feet in the client's own copy
/// of the world, which is how no-fall keeps the fall distance at zero. The
/// flag is always cleared so the fall keeps counting; weight only builds
/// once the claim persists through a descent, which leaves room for a
/// block removed under the player before the client saw it and for
/// entities (boats, shulkers) the block view cannot see.
#[derive(Default)]
pub struct GroundSupport {
    streak: u32,
    trace: Option<Trace>,
}

impl Check for GroundSupport {
    fn trace(&self) -> Option<Trace> {
        self.trace
    }

    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.trace = None;
        let Serverbound::Move(movement) = packet else {
            return None;
        };
        let exempt = facts.instabuild
            || facts.may_fly
            || facts.flying
            || facts.riding
            || facts.teleport_pending
            || facts.game_mode == GameMode::Spectator;
        if movement.position.is_none() {
            let resting = facts.resting.filter(|t| t.known && !exempt);
            if movement.on_ground && resting.is_some_and(|t| !t.supported && !t.soft) {
                return Some(Flag {
                    check: "ground",
                    weight: 0.0,
                    fix: Fix::Correct(Box::new(Serverbound::Move(Movement {
                        on_ground: false,
                        ..*movement
                    }))),
                });
            }
            return None;
        }
        let terrain = facts.terrain.filter(|t| t.known && !exempt);
        let (Some(position), Some(terrain)) = (movement.position, terrain) else {
            self.streak = 0;
            return None;
        };
        if !movement.on_ground || terrain.supported || terrain.soft {
            self.streak = 0;
            return None;
        }
        self.streak += 1;
        let descending = position[1] - facts.position[1] < DESCENT;
        self.trace = Trace::new(
            "ground",
            f64::from(TOLERATED_MOVES),
            f64::from(self.streak),
            Tags::LOST_GROUND,
        );
        let weight = if descending && self.streak > TOLERATED_MOVES {
            1.0
        } else {
            0.0
        };
        Some(Flag {
            check: "ground",
            weight,
            fix: Fix::Correct(Box::new(Serverbound::Move(Movement {
                on_ground: false,
                ..*movement
            }))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Terrain, facts};
    use super::*;

    fn moved(y: f64, on_ground: bool) -> Serverbound {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some([0.0, y, 0.0]),
            rotation: None,
            on_ground,
        })
    }

    fn terrain(supported: bool) -> Option<Terrain> {
        Some(Terrain {
            known: true,
            supported,
            ..Terrain::default()
        })
    }

    fn cleared(flag: &Flag) -> bool {
        matches!(&flag.fix, Fix::Correct(p) if matches!(**p, Serverbound::Move(Movement { on_ground: false, .. })))
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_unsupported_ground_claim_is_cleared_and_weighs_after_a_tolerance() {
        let mut check = GroundSupport::default();
        let mut f = facts();
        f.terrain = terrain(false);
        let mut weights = Vec::new();
        for n in 0..6 {
            f.position[1] = 100.0 - f64::from(n);
            let flag = check
                .examine(&f, &moved(99.0 - f64::from(n), true))
                .unwrap();
            assert!(cleared(&flag));
            weights.push(flag.weight);
        }
        assert_eq!(weights, [0.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_positionless_ground_claim_over_nothing_is_cleared() {
        let mut check = GroundSupport::default();
        let mut f = facts();
        f.resting = terrain(false);
        let status = Serverbound::Move(Movement {
            horizontal_collision: false,
            position: None,
            rotation: None,
            on_ground: true,
        });
        let flag = check.examine(&f, &status).unwrap();
        assert!(cleared(&flag));
        assert_eq!(flag.weight, 0.0);
        f.resting = terrain(true);
        assert_eq!(check.examine(&f, &status), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn standing_still_unsupported_is_cleared_without_weight() {
        let mut check = GroundSupport::default();
        let mut f = facts();
        f.terrain = terrain(false);
        for _ in 0..20 {
            let flag = check.examine(&f, &moved(64.0, true)).unwrap();
            assert_eq!(flag.weight, 0.0);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn supported_soft_unknown_and_exempt_moves_pass() {
        let mut check = GroundSupport::default();
        let mut f = facts();
        f.terrain = terrain(true);
        assert_eq!(check.examine(&f, &moved(63.0, true)), None);
        f.terrain = Some(Terrain {
            known: true,
            soft: true,
            ..Terrain::default()
        });
        assert_eq!(check.examine(&f, &moved(63.0, true)), None);
        f.terrain = Some(Terrain::default());
        assert_eq!(check.examine(&f, &moved(63.0, true)), None);
        f.terrain = terrain(false);
        assert_eq!(check.examine(&f, &moved(63.0, false)), None);
        f.may_fly = true;
        assert_eq!(check.examine(&f, &moved(63.0, true)), None);
    }
}
