use tuff_core::protocol::play::Serverbound;

use super::{Check, Facts, Fix, Flag};

const SWING_GRACE_TICKS: u64 = 3;
const MS_PER_TICK: i32 = 50;

/// An attack the client never follows with an arm swing, which the
/// vanilla client sends right after every attack. The swing may trail by
/// the connection's lag, so the verdict waits that long.
#[derive(Default)]
pub struct AttackSwing {
    pending: Option<u64>,
}

impl Check for AttackSwing {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        if matches!(packet, Serverbound::Punch) {
            self.pending = None;
            return None;
        }
        let lag = (facts.latency_ms.max(0) / MS_PER_TICK) as u64;
        let overdue = self
            .pending
            .is_some_and(|since| facts.tick.saturating_sub(since) > SWING_GRACE_TICKS + lag);
        if overdue {
            self.pending = None;
        }
        if matches!(packet, Serverbound::Attack(_)) && self.pending.is_none() {
            self.pending = Some(facts.tick);
        }
        overdue.then_some(Flag {
            check: "attack_swing",
            weight: 0.5,
            fix: Fix::Pass,
        })
    }
}

/// Attacking oneself.
pub struct SelfHit;

impl Check for SelfHit {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        match packet {
            Serverbound::Attack(id) if *id == facts.entity_id => Flag::cancel("self_hit", 2.0),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    fn at(tick: u64) -> Facts {
        Facts { tick, ..facts() }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_swing_after_the_attack_is_clean() {
        let mut check = AttackSwing::default();
        assert!(check.examine(&at(0), &Serverbound::Attack(5)).is_none());
        assert!(check.examine(&at(0), &Serverbound::Punch).is_none());
        assert!(check.examine(&at(20), &Serverbound::Punch).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_attack_without_a_swing_flags_after_the_grace() {
        let mut check = AttackSwing::default();
        assert!(check.examine(&at(0), &Serverbound::Attack(5)).is_none());
        assert!(check.examine(&at(2), &Serverbound::Attack(5)).is_none());
        let flag = check.examine(&at(4), &Serverbound::Attack(5)).unwrap();
        assert_eq!(flag.check, "attack_swing");
    }

    #[tuff_core::watchdog]
    #[test]
    fn lag_widens_the_swing_grace() {
        let mut check = AttackSwing::default();
        let laggy = |tick| Facts {
            latency_ms: 200,
            ..at(tick)
        };
        assert!(check.examine(&laggy(0), &Serverbound::Attack(5)).is_none());
        assert!(check.examine(&laggy(5), &Serverbound::Punch).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn hitting_oneself_is_cancelled() {
        let mut check = SelfHit;
        let flag = check.examine(&at(0), &Serverbound::Attack(1)).unwrap();
        assert_eq!(flag.fix, Fix::Cancel);
        assert!(check.examine(&at(0), &Serverbound::Attack(2)).is_none());
    }
}
