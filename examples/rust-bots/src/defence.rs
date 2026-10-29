//! Hitting back at whoever hurt the bot. Free of host calls so the
//! decisions are plain functions.

use crate::combat::Target;

/// Ticks a hit stays worth answering.
pub const MEMORY_TICKS: u64 = 100;

/// Health below which a bot leaves the fight to danger avoidance; zero
/// fights on at any health.
pub const FLEE_BELOW: f32 = 0.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Hurt {
    /// The attacker's uuid as 32 hex digits.
    pub attacker: String,
    /// The plugin tick of the hit.
    pub at: u64,
}

#[derive(Debug, PartialEq)]
pub enum Response {
    /// Attack this uuid.
    Fight(String),
    Ignore,
}

pub struct Situation<'a> {
    pub defend: bool,
    pub now: u64,
    pub health: f32,
    pub flee_below: f32,
    /// Already attacking or shooting something else on the owner's order.
    pub busy: bool,
    /// Whether the attacker is on the bot's side (the same owner's bots).
    pub ally: bool,
    /// The attacker as the server finds it now: none when it is gone.
    pub attacker: Option<&'a Target>,
}

pub fn choose_response(hurt: Option<&Hurt>, s: &Situation) -> Response {
    let Some(hurt) = hurt else {
        return Response::Ignore;
    };
    let stale = s.now.saturating_sub(hurt.at) > MEMORY_TICKS;
    let gone = s.attacker.is_none_or(|a| !a.alive);
    if !s.defend || s.busy || s.ally || stale || gone || s.health < s.flee_below {
        return Response::Ignore;
    }
    Response::Fight(hurt.attacker.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_bots_api::control::Vec3;

    fn target(alive: bool) -> Target {
        Target {
            pos: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            alive,
            width: 0.6,
            height: 1.8,
        }
    }

    fn hurt(at: u64) -> Hurt {
        Hurt {
            attacker: "ab".repeat(16),
            at,
        }
    }

    fn base(t: Option<&Target>) -> Situation<'_> {
        Situation {
            defend: true,
            now: 10,
            health: 20.0,
            flee_below: 0.0,
            busy: false,
            ally: false,
            attacker: t,
        }
    }

    #[test]
    fn defence_fights_back_at_a_fresh_live_attacker() {
        let t = target(true);
        assert_eq!(
            choose_response(Some(&hurt(5)), &base(Some(&t))),
            Response::Fight("ab".repeat(16))
        );
    }

    #[test]
    fn defence_ignores_stale_gone_dead_and_allied_attackers() {
        let t = target(true);
        let dead = target(false);
        let stale = Situation {
            now: 5 + MEMORY_TICKS + 1,
            ..base(Some(&t))
        };
        assert_eq!(choose_response(Some(&hurt(5)), &stale), Response::Ignore);
        assert_eq!(
            choose_response(Some(&hurt(5)), &base(None)),
            Response::Ignore
        );
        assert_eq!(
            choose_response(Some(&hurt(5)), &base(Some(&dead))),
            Response::Ignore
        );
        let ally = Situation {
            ally: true,
            ..base(Some(&t))
        };
        assert_eq!(choose_response(Some(&hurt(5)), &ally), Response::Ignore);
        assert_eq!(choose_response(None, &base(Some(&t))), Response::Ignore);
    }

    #[test]
    fn defence_respects_the_flag_busy_bots_and_the_flee_threshold() {
        let t = target(true);
        let off = Situation {
            defend: false,
            ..base(Some(&t))
        };
        assert_eq!(choose_response(Some(&hurt(5)), &off), Response::Ignore);
        let busy = Situation {
            busy: true,
            ..base(Some(&t))
        };
        assert_eq!(choose_response(Some(&hurt(5)), &busy), Response::Ignore);
        let weak = Situation {
            health: 3.0,
            flee_below: 6.0,
            ..base(Some(&t))
        };
        assert_eq!(choose_response(Some(&hurt(5)), &weak), Response::Ignore);
        let default = Situation {
            health: 1.0,
            flee_below: FLEE_BELOW,
            ..base(Some(&t))
        };
        assert!(matches!(
            choose_response(Some(&hurt(5)), &default),
            Response::Fight(_)
        ));
    }
}
