use tuff_core::protocol::play::{GameMode, Serverbound};

use super::movement::UseMomentum;
use super::{Check, Facts, Fix, Flag};
use crate::trace::{Tags, Trace};
use crate::violations::Bucket;

const MENU_REACH: f64 = 12.0;
const USING_STEP: f64 = 0.15;
const USING_TOLERANCE: f64 = 0.6;
const PREDICTED_TOLERANCE: f64 = 0.08;
const SPEED_STEP: f64 = 0.25;
const RESYNC_WINDOW: u64 = 100;

/// Clicking a block container from far beyond where the server closes it.
pub struct ContainerReach;

impl Check for ContainerReach {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        if !matches!(
            packet,
            Serverbound::ContainerClick { .. }
                | Serverbound::ContainerButtonClick { .. }
                | Serverbound::ContainerSlotState { .. }
        ) {
            return None;
        }
        if facts.menu_distance.is_some_and(|d| d > MENU_REACH) {
            return Flag::resync("container", 3.0);
        }
        None
    }
}

/// Moving faster while an item is in use than its slowed input allows:
/// the predicted step where momentum and friction are known, a flat
/// fraction of walking pace elsewhere. The first suspicion only resyncs the
/// item use, since a client that already stopped using looks the same;
/// a repeat soon after is a violation.
#[derive(Default)]
pub struct NoSlow {
    excess: f64,
    resynced_at: Option<u64>,
    momentum: UseMomentum,
    trace: Option<Trace>,
}

impl Check for NoSlow {
    fn trace(&self) -> Option<Trace> {
        self.trace
    }

    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.trace = None;
        self.momentum.observe(packet);
        let Serverbound::Move(movement) = packet else {
            return None;
        };
        let position = movement.position?;
        let predicted = self.momentum.allowed(facts, movement, position);
        if !facts.using
            || facts.teleport_pending
            || facts.riding
            || facts.velocity
            || facts.flying
            || facts.may_fly
            || facts.game_mode == GameMode::Spectator
        {
            self.excess = 0.0;
            return None;
        }
        let boost = 1.0 + SPEED_STEP * facts.speed.map_or(0.0, |a| f64::from(a) + 1.0);
        let h = (position[0] - facts.position[0]).hypot(position[2] - facts.position[2]);
        let allowed = predicted.unwrap_or(USING_STEP * boost + facts.push.horizontal);
        self.excess = (self.excess + h - allowed).max(0.0);
        let tags = Tags::empty()
            .with(Tags::USE_MOMENTUM, predicted.is_some())
            .with(Tags::USE_FLAT, predicted.is_none())
            .with(Tags::SPEED_EFFECT, facts.speed.is_some())
            .with(Tags::PUSH, facts.push.horizontal > 0.0)
            .union(
                self.momentum
                    .path()
                    .map_or(Tags::empty(), |path| path.tags()),
            );
        self.trace = Trace::new("no_slow", allowed, h, tags);
        if self.excess
            > if predicted.is_some() {
                PREDICTED_TOLERANCE
            } else {
                USING_TOLERANCE
            }
        {
            self.excess = 0.0;
            if self
                .resynced_at
                .is_none_or(|at| facts.tick.saturating_sub(at) > RESYNC_WINDOW)
            {
                self.resynced_at = Some(facts.tick);
                self.trace = Trace::new("no_slow", allowed, h, tags.with(Tags::ITEM_RESYNC, true));
                return Some(Flag {
                    check: "no_slow",
                    weight: 0.0,
                    fix: Fix::ResyncUse,
                });
            }
            return Some(Flag {
                check: "no_slow",
                weight: 1.0,
                fix: Fix::Teleport(facts.position),
            });
        }
        None
    }
}

/// Sign and command block edits: their rate, and command blocks by a
/// player who may not edit them.
pub struct EditLimits {
    bucket: Bucket,
}

impl Default for EditLimits {
    fn default() -> Self {
        Self {
            bucket: Bucket::new(6.0, 0.25),
        }
    }
}

impl Check for EditLimits {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        match packet {
            Serverbound::SetCommandBlock { .. } if !facts.may_edit_commands => {
                Flag::cancel("edit", 5.0)
            }
            Serverbound::SignUpdate { .. } | Serverbound::SetCommandBlock { .. }
                if !self.bucket.allow(1.0, facts.tick) =>
            {
                Flag::cancel("edit", 1.0)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;
    use tuff_core::protocol::play::Movement;
    use tuff_world_model::BlockPos;

    fn step(x: f64) -> Serverbound {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some([x, 64.0, 0.0]),
            rotation: None,
            on_ground: true,
        })
    }

    fn click() -> Serverbound {
        Serverbound::ContainerClick {
            window_id: 1,
            state_id: 0,
            slot: 0,
            button: 0,
            mode: 0,
            changed: Vec::new(),
            carried: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn far_containers_are_flagged() {
        let mut f = facts();
        f.menu_distance = Some(5.0);
        assert_eq!(ContainerReach.examine(&f, &click()), None);
        f.menu_distance = Some(40.0);
        assert!(ContainerReach.examine(&f, &click()).is_some());
        assert_eq!(ContainerReach.examine(&f, &step(0.0)), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn full_speed_while_using_is_flagged_but_slow_is_not() {
        let mut f = facts();
        f.using = true;
        let mut slow = NoSlow::default();
        for _ in 0..100 {
            assert_eq!(slow.examine(&f, &step(0.05)), None);
        }
        let mut fast = NoSlow::default();
        let fixes: Vec<Fix> = (0..20)
            .filter_map(|_| fast.examine(&f, &step(0.25)))
            .map(|flag| flag.fix)
            .collect();
        assert_eq!(fixes.first(), Some(&Fix::ResyncUse));
        assert!(matches!(fixes.get(1), Some(Fix::Teleport(_))), "{fixes:?}");
        f.tick = 500;
        let again: Vec<Fix> = (0..10)
            .filter_map(|_| fast.examine(&f, &step(0.25)))
            .map(|flag| flag.fix)
            .collect();
        assert_eq!(again.first(), Some(&Fix::ResyncUse));
        f.using = false;
        let mut idle = NoSlow::default();
        assert!((0..10).all(|_| idle.examine(&f, &step(0.25)).is_none()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn edits_are_rate_limited_and_command_blocks_need_rights() {
        let sign = || Serverbound::SignUpdate {
            position: BlockPos::new(0, 0, 0),
            front: true,
            lines: Vec::new(),
        };
        let mut edits = EditLimits::default();
        let f = facts();
        let flagged = (0..20)
            .filter(|_| edits.examine(&f, &sign()).is_some())
            .count();
        assert!(flagged > 5);
        let command = Serverbound::SetCommandBlock {
            position: BlockPos::new(0, 0, 0),
            command: String::new(),
            mode: 0,
            track_output: false,
            conditional: false,
            automatic: false,
        };
        let mut denied = facts();
        denied.may_edit_commands = false;
        assert_eq!(
            EditLimits::default()
                .examine(&denied, &command)
                .unwrap()
                .weight,
            5.0
        );
        assert_eq!(EditLimits::default().examine(&f, &command), None);
    }
}
