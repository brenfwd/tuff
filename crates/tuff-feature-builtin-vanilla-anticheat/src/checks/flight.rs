use tuff_core::protocol::play::{GameMode, Serverbound};

use super::{Check, Facts, Fix, Flag};
use crate::trace::{Tags, Trace};

const NAME: &str = "fly";
const FLY_HORIZONTAL: f64 = 1.35;
const FLY_VERTICAL: f64 = 0.5;
const GLIDE_HORIZONTAL: f64 = 4.0;
const GLIDE_RISE: f64 = 2.0;
const FIREWORK_RISE: f64 = 3.0;
const GLIDE_FALL: f64 = 4.0;
const RIPTIDE_LIMIT: f64 = 3.8;
const LEVITATION_STEP: f64 = 0.05;
const JUMP_VELOCITY: f64 = 0.42;
const JUMP_BOOST_STEP: f64 = 0.1;
const GRAVITY: f64 = 0.08;
const DRAG: f64 = 0.98;
const FLY_DEFAULT: f64 = 0.05;
const CEILING_MARGIN: f64 = 256.0;
const SLACK: f64 = 0.05;
const GRACE_FACTOR: f64 = 1.5;
const HUGE_MOVE: f64 = 64.0;
const BOOST_CATCH_UP: u64 = 5;
const BOOST_LIFETIME_SPREAD: u64 = 12;

/// Speed envelopes for the states in which free flight is legal: creative
/// and spectator flight, elytra gliding (with fireworks), riptide launches
/// and levitation. Each state grants its own limits; the most generous
/// active one applies, and a player in none of them is left to the
/// survival checks.
#[derive(Default)]
pub struct FlightEnvelope {
    jump: f64,
    boosted: Option<u64>,
    trace: Option<Trace>,
}

struct Limits {
    horizontal: Option<f64>,
    up: f64,
    down: Option<f64>,
    tags: Tags,
}

impl FlightEnvelope {
    fn boosting(&mut self, facts: &Facts) -> bool {
        if facts.firework {
            self.boosted = Some(facts.tick);
            return true;
        }
        let round_trip = u64::try_from(facts.latency_ms.max(0)).unwrap_or(0) * 2;
        let window = round_trip.div_ceil(50) + BOOST_CATCH_UP + BOOST_LIFETIME_SPREAD;
        self.boosted
            .is_some_and(|at| facts.tick.saturating_sub(at) <= window)
    }

    fn limits(&mut self, facts: &Facts, supported: bool, pitch: f32) -> Option<Limits> {
        let flying = facts.flying || facts.game_mode == GameMode::Spectator;
        let levitating = facts.levitation && !flying && !facts.gliding;
        if supported {
            self.jump = JUMP_VELOCITY + JUMP_BOOST_STEP * f64::from(facts.jump_boost);
        } else {
            self.jump = ((self.jump - GRAVITY) * DRAG).max(0.0);
        }
        if !(flying || facts.gliding || facts.riptide || levitating) {
            return None;
        }
        let mut limits = Limits {
            horizontal: Some(0.0),
            up: 0.0,
            down: Some(0.0),
            tags: Tags::empty(),
        };
        let mut grant = |h: Option<f64>, up: f64, down: Option<f64>, tag: Tags| {
            limits.horizontal = match (limits.horizontal, h) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            };
            limits.down = match (limits.down, down) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            };
            limits.up = limits.up.max(up);
            limits.tags = limits.tags.union(tag);
        };
        if flying {
            let scale = (f64::from(facts.fly_speed) / FLY_DEFAULT).max(1.0);
            grant(
                Some(FLY_HORIZONTAL * scale),
                FLY_VERTICAL * scale,
                Some(FLY_VERTICAL * scale),
                Tags::empty(),
            );
        }
        if facts.gliding {
            let boosting = self.boosting(facts);
            let rise = if boosting {
                FIREWORK_RISE
            } else if pitch >= 0.0 {
                0.0
            } else {
                GLIDE_RISE
            };
            let tag = Tags::GLIDE.with(Tags::BOOST, boosting);
            grant(Some(GLIDE_HORIZONTAL), rise, Some(GLIDE_FALL), tag);
        }
        if facts.riptide {
            grant(
                Some(RIPTIDE_LIMIT),
                RIPTIDE_LIMIT,
                Some(RIPTIDE_LIMIT),
                Tags::GLIDE,
            );
        }
        if levitating {
            let rise = LEVITATION_STEP * f64::from(facts.levitation_level).max(1.0);
            grant(None, rise.max(self.jump), None, Tags::LEVITATION);
        }
        Some(limits)
    }
}

impl Check for FlightEnvelope {
    fn trace(&self) -> Option<Trace> {
        self.trace
    }

    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.trace = None;
        let Serverbound::Move(movement) = packet else {
            return None;
        };
        let position = movement.position?;
        let terrain = facts.terrain.unwrap_or_default();
        let supported = terrain.supported && !terrain.soft;
        let pitch = movement
            .rotation
            .map_or(facts.rotation[1], |(_, pitch)| pitch);
        let limits = self.limits(facts, supported, pitch);
        if facts.teleport_pending || facts.riding || facts.confirming {
            return None;
        }
        let limits = limits?;
        let grace = if facts.grace { GRACE_FACTOR } else { 1.0 };
        let h = (position[0] - facts.position[0]).hypot(position[2] - facts.position[2]);
        let dy = position[1] - facts.position[1];
        let push = facts.push.horizontal;
        let launch = facts.push.launch.unwrap_or(0.0);
        let up = limits.up.max(launch) * grace + SLACK;
        let mut excess = 0.0f64;
        let mut allowed = up;
        if let Some(limit) = limits.horizontal {
            let limit = limit * grace + push + SLACK;
            if h - limit > excess {
                excess = h - limit;
                allowed = limit;
            }
            if h > HUGE_MOVE {
                excess = excess.max(HUGE_MOVE);
            }
        }
        if dy - up > excess {
            excess = dy - up;
            allowed = up;
        }
        if let Some(down) = limits.down
            && -dy - (down * grace + SLACK) > excess
        {
            excess = -dy - (down * grace + SLACK);
            allowed = down * grace + SLACK;
        }
        if limits.horizontal.is_some() && position[1] - facts.ceiling > CEILING_MARGIN {
            excess = excess.max(position[1] - facts.ceiling - CEILING_MARGIN);
            allowed = facts.ceiling + CEILING_MARGIN;
        }
        let actual = h.max(dy.abs());
        self.trace = Trace::new(NAME, allowed, actual, limits.tags);
        if excess > 0.0 {
            return Some(Flag {
                check: NAME,
                weight: 1.0 + (excess * 2.0).min(3.0),
                fix: Fix::Teleport(facts.position),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::{Terrain, facts};
    use tuff_core::protocol::play::Movement;

    fn to(x: f64, y: f64, z: f64) -> Serverbound {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some([x, y, z]),
            rotation: None,
            on_ground: false,
        })
    }

    fn air() -> Facts {
        let mut f = facts();
        f.on_ground = false;
        f.terrain = Some(Terrain {
            known: true,
            ..Terrain::default()
        });
        f
    }

    fn step(f: &Facts, dx: f64, dy: f64) -> Option<Flag> {
        let mut check = FlightEnvelope::default();
        let p = f.position;
        check.examine(f, &to(p[0] + dx, p[1] + dy, p[2]))
    }

    #[tuff_core::watchdog]
    #[test]
    fn creative_and_spectator_flight_stay_in_the_fly_speed() {
        let mut f = air();
        f.flying = true;
        assert!(step(&f, 1.1, 0.4).is_none());
        assert!(step(&f, 0.0, -0.4).is_none());
        assert!(step(&f, 2.0, 0.0).is_some());
        assert!(step(&f, 0.0, 1.0).is_some());
        let mut s = air();
        s.game_mode = GameMode::Spectator;
        assert!(step(&s, 1.1, 0.0).is_none());
        assert!(step(&s, 5.0, 0.0).is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn fly_speed_scales_the_envelope() {
        let mut f = air();
        f.flying = true;
        assert!(step(&f, 2.0, 0.0).is_some());
        f.fly_speed = 0.1;
        assert!(step(&f, 2.0, 0.0).is_none());
        assert!(step(&f, 5.0, 0.0).is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn flying_far_above_the_ceiling_flags() {
        let mut f = air();
        f.flying = true;
        f.position = [0.0, 500.0, 0.0];
        assert!(step(&f, 0.0, 0.1).is_none());
        f.position = [0.0, 600.0, 0.0];
        assert!(step(&f, 0.0, 0.1).is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn gliding_cannot_climb_while_pitched_down() {
        let mut g = air();
        g.gliding = true;
        g.rotation = [0.0, 30.0];
        assert!(step(&g, 1.0, 0.5).is_some());
        assert!(step(&g, 1.0, -1.0).is_none());
        g.rotation = [0.0, -40.0];
        assert!(step(&g, 1.0, 0.5).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_boost_outlives_the_server_window_by_latency_and_lifetime_spread() {
        let mut check = FlightEnvelope::default();
        let mut g = air();
        g.gliding = true;
        g.rotation = [0.0, 5.0];
        g.firework = true;
        g.tick = 100;
        assert!(
            check
                .examine(&g, &to(0.0, g.position[1] + 1.6, 0.0))
                .is_none()
        );
        g.firework = false;
        g.tick = 108;
        assert!(
            check
                .examine(&g, &to(0.0, g.position[1] + 1.6, 0.0))
                .is_none()
        );
        g.tick = 140;
        assert!(
            check
                .examine(&g, &to(0.0, g.position[1] + 1.6, 0.0))
                .is_some()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn revoked_flight_leaves_no_envelope() {
        let f = air();
        assert!(step(&f, 5.0, 3.0).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn gliding_with_fireworks_and_riptide_have_their_own_limits() {
        let mut g = air();
        g.gliding = true;
        assert!(step(&g, 3.2, -2.0).is_none());
        assert!(step(&g, 0.0, 2.5).is_some());
        g.firework = true;
        assert!(step(&g, 2.5, 2.5).is_none());
        assert!(step(&g, 6.0, 0.0).is_some());
        let mut r = air();
        r.riptide = true;
        assert!(step(&r, 3.0, 0.5).is_none());
        assert!(step(&r, 5.0, 0.0).is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn levitation_bounds_the_climb() {
        let mut l = air();
        l.levitation = true;
        l.levitation_level = 1;
        let mut check = FlightEnvelope::default();
        let mut ground = l.clone();
        ground.terrain = Some(Terrain {
            known: true,
            supported: true,
            ..Terrain::default()
        });
        check.examine(&ground, &to(0.0, 64.0, 0.0));
        assert!(check.examine(&l, &to(0.0, 64.1, 0.0)).is_none());
        assert!(step(&l, 0.0, 0.3).is_some());
        assert!(step(&l, 0.0, 0.08).is_none());
    }
}
