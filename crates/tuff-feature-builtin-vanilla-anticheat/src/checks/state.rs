use tuff_core::protocol::play::{GameMode, PlayerActionStatus, Serverbound};

use super::{Check, Facts, Flag};
use crate::violations::Bucket;

const SNEAK_BIT: u8 = 0x20;
const STREAK_LIMIT: u32 = 10;
const FALL_STEP: f64 = -0.3;

/// Sprint commands arriving faster than a keyboard can, a second start
/// without a stop, or a start while sneaking.
pub struct SprintSpoof {
    bucket: Bucket,
    sprinting: bool,
}

impl Default for SprintSpoof {
    fn default() -> Self {
        Self {
            bucket: Bucket::new(25.0, 1.25),
            sprinting: false,
        }
    }
}

impl Check for SprintSpoof {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let Serverbound::PlayerCommand(action @ (1 | 2)) = packet else {
            return None;
        };
        if !self.bucket.allow(1.0, facts.tick) {
            return Flag::cancel("sprint", 1.0);
        }
        let start = *action == 1;
        let repeated = start == self.sprinting;
        self.sprinting = start;
        if start && facts.sneaking {
            return Flag::cancel("sprint", 1.0);
        }
        if repeated {
            Flag::cancel("sprint", 0.25)
        } else {
            None
        }
    }
}

/// A sprint start the client's own rules would not allow: too hungry, an
/// item in use, or no forward key held in the last input.
#[derive(Default)]
pub struct SprintLegal {
    keys: Option<u8>,
}

const FORWARD_BIT: u8 = 0x01;
const MIN_SPRINT_FOOD: i32 = 6;

impl Check for SprintLegal {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        match packet {
            Serverbound::PlayerInput(flags) => {
                self.keys = Some(*flags);
                None
            }
            Serverbound::PlayerCommand(1) => {
                let hungry = facts.food <= MIN_SPRINT_FOOD && !facts.may_fly && !facts.instabuild;
                let no_forward = self.keys.is_some_and(|k| k & FORWARD_BIT == 0);
                let weight = f64::from(u8::from(hungry)) * 0.5
                    + f64::from(u8::from(facts.using)) * 0.25
                    + f64::from(u8::from(no_forward)) * 0.25;
                if weight > 0.0 {
                    Flag::cancel("sprint", weight)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Sneak toggles faster than a keyboard can.
pub struct SneakSpoof {
    bucket: Bucket,
    sneaking: bool,
}

impl Default for SneakSpoof {
    fn default() -> Self {
        Self {
            bucket: Bucket::new(25.0, 1.25),
            sneaking: false,
        }
    }
}

impl Check for SneakSpoof {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let Serverbound::PlayerInput(flags) = packet else {
            return None;
        };
        let sneak = flags & SNEAK_BIT != 0;
        if sneak == self.sneaking {
            return None;
        }
        self.sneaking = sneak;
        if self.bucket.allow(1.0, facts.tick) {
            None
        } else {
            Flag::cancel("sneak", 1.0)
        }
    }
}

/// Horizontal motion long after every movement key was released, which
/// the client's own momentum cannot explain: reported keys that disagree
/// with the movement. Only a weak signal for the aggregator.
#[derive(Default)]
pub struct KeyMismatch {
    keys: Option<u8>,
    idle_moves: u32,
    weak: f64,
}

const IDLE_MOVES: u32 = 10;
const IDLE_STEP: f64 = 0.1;
const MOVE_KEYS: u8 = 0x0f;

impl Check for KeyMismatch {
    fn weak(&mut self) -> f64 {
        std::mem::take(&mut self.weak)
    }

    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.weak = 0.0;
        match packet {
            Serverbound::PlayerInput(flags) => {
                self.keys = Some(*flags);
                self.idle_moves = 0;
            }
            Serverbound::Move(movement) => {
                let terrain = facts.terrain.unwrap_or_default();
                let exempt = facts.instabuild
                    || facts.may_fly
                    || facts.flying
                    || facts.riding
                    || facts.velocity
                    || facts.teleport_pending
                    || facts.confirming
                    || facts.push.horizontal > 0.0
                    || facts.game_mode == GameMode::Spectator
                    || !terrain.known
                    || terrain.slippery
                    || terrain.soft
                    || terrain.bouncy;
                let released = self.keys.is_some_and(|k| k & MOVE_KEYS == 0);
                let Some(position) = movement.position.filter(|_| released && !exempt) else {
                    self.idle_moves = 0;
                    return None;
                };
                let h = (position[0] - facts.position[0]).hypot(position[2] - facts.position[2]);
                if h <= IDLE_STEP {
                    return None;
                }
                self.idle_moves += 1;
                if self.idle_moves > IDLE_MOVES {
                    self.weak = 0.25;
                }
            }
            _ => {}
        }
        None
    }
}

/// Claiming to stand on the ground through a long drop where the client's
/// blocks are unknown; known terrain is left to the support check.
#[derive(Default)]
pub struct GroundSpoof {
    streak: u32,
}

impl Check for GroundSpoof {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let Serverbound::Move(movement) = packet else {
            return None;
        };
        let exempt = facts.instabuild
            || facts.may_fly
            || facts.riding
            || facts.teleport_pending
            || facts.game_mode == GameMode::Spectator;
        let known = facts.terrain.is_some_and(|t| t.known);
        let Some(position) = movement.position.filter(|_| !exempt && !known) else {
            self.streak = 0;
            return None;
        };
        let dy = position[1] - facts.position[1];
        if movement.on_ground && dy < FALL_STEP {
            self.streak += 1;
        } else {
            self.streak = 0;
        }
        if self.streak > STREAK_LIMIT {
            Flag::cancel("ground", 1.0)
        } else {
            None
        }
    }
}

/// Actions a client cannot take while a container screen is open.
pub struct PacketOrder;

impl Check for PacketOrder {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        facts.menu?;
        let acting = match packet {
            Serverbound::Attack(_) | Serverbound::UseItemOn { .. } => true,
            Serverbound::PlayerAction { status, .. } => {
                *status == PlayerActionStatus::StartDestroyBlock
            }
            _ => false,
        };
        if acting {
            Flag::cancel("order", 2.0)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;
    use tuff_core::protocol::play::Movement;

    fn moved(y: f64, on_ground: bool) -> Serverbound {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some([0.0, y, 0.0]),
            rotation: None,
            on_ground,
        })
    }

    fn moved_x(x: f64) -> Serverbound {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some([x, 0.0, 0.0]),
            rotation: None,
            on_ground: true,
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn sprint_toggles_at_human_speed_pass() {
        let mut check = SprintSpoof::default();
        let mut f = facts();
        for tick in 0..100u64 {
            f.tick = tick * 4;
            let action = if tick % 2 == 0 { 1 } else { 2 };
            assert_eq!(check.examine(&f, &Serverbound::PlayerCommand(action)), None);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn sprint_flicker_and_sneak_sprint_are_flagged() {
        let mut check = SprintSpoof::default();
        let f = facts();
        let flags = (0..40)
            .filter_map(|n| check.examine(&f, &Serverbound::PlayerCommand(1 + n % 2)))
            .count();
        assert!(flags > 0);
        let mut check = SprintSpoof::default();
        let mut f = facts();
        f.sneaking = true;
        assert!(check.examine(&f, &Serverbound::PlayerCommand(1)).is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn illegal_sprint_starts_are_flagged() {
        let start = Serverbound::PlayerCommand(1);
        let mut check = SprintLegal::default();
        let mut f = facts();
        assert_eq!(check.examine(&f, &start), None);
        check.examine(&f, &Serverbound::PlayerInput(0x01));
        assert_eq!(check.examine(&f, &start), None);
        check.examine(&f, &Serverbound::PlayerInput(0x02));
        assert!(check.examine(&f, &start).is_some());
        check.examine(&f, &Serverbound::PlayerInput(0x01));
        f.food = 6;
        assert!(check.examine(&f, &start).is_some());
        f.food = 20;
        f.using = true;
        assert!(check.examine(&f, &start).is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn moving_with_every_key_released_is_a_weak_signal() {
        let mut check = KeyMismatch::default();
        let mut f = facts();
        f.terrain = Some(super::super::Terrain {
            known: true,
            supported: true,
            ..Default::default()
        });
        check.examine(&f, &Serverbound::PlayerInput(0));
        let mut weak = 0.0;
        for n in 0..20 {
            f.position[0] = f64::from(n) * 0.3;
            check.examine(&f, &moved_x(f64::from(n + 1) * 0.3));
            weak += check.weak();
        }
        assert!(weak > 0.0);
        check.examine(&f, &Serverbound::PlayerInput(0x01));
        for n in 0..20 {
            f.position[0] = f64::from(n) * 0.3;
            check.examine(&f, &moved_x(f64::from(n + 1) * 0.3));
            assert_eq!(check.weak(), 0.0);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn sneak_flicker_is_flagged() {
        let mut check = SneakSpoof::default();
        let f = facts();
        let flags = (0..40u8)
            .filter_map(|n| {
                check.examine(
                    &f,
                    &Serverbound::PlayerInput(if n % 2 == 0 { 0x20 } else { 0 }),
                )
            })
            .count();
        assert!(flags > 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_ground_flag_through_a_long_drop_is_flagged_but_stairs_are_not() {
        let mut check = GroundSpoof::default();
        let mut f = facts();
        let mut flags = 0;
        for n in 0..30 {
            f.position[1] = 100.0 - f64::from(n);
            flags += usize::from(
                check
                    .examine(&f, &moved(99.0 - f64::from(n), true))
                    .is_some(),
            );
        }
        assert!(flags > 10);
        let mut check = GroundSpoof::default();
        for n in 0..200 {
            f.position[1] = 100.0 - f64::from(n / 6) * 0.5;
            let drop = if n % 6 == 5 { -0.5 } else { 0.0 };
            assert_eq!(check.examine(&f, &moved(f.position[1] + drop, true)), None);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn creative_flight_is_exempt_from_ground_checks() {
        let mut check = GroundSpoof::default();
        let mut f = facts();
        f.instabuild = true;
        for n in 0..30 {
            f.position[1] = 100.0 - f64::from(n);
            assert_eq!(check.examine(&f, &moved(99.0 - f64::from(n), true)), None);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn actions_with_a_container_open_are_cancelled() {
        let mut f = facts();
        assert_eq!(PacketOrder.examine(&f, &Serverbound::Attack(1)), None);
        f.menu = Some((1, 27));
        assert!(PacketOrder.examine(&f, &Serverbound::Attack(1)).is_some());
        assert_eq!(PacketOrder.examine(&f, &Serverbound::Punch), None);
    }
}
