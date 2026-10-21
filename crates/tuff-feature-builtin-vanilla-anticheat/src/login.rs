//! Login and relog throttles: too many logins per address or name in a
//! window are refused, and relogging faster than the minimum delay is
//! let through with a warning a few times before the name is held off
//! for a while. A single reconnect after a crash is always admitted.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use tuff_core::config::{ConfigStore, Key, Value};
use tuff_core::permission::Subject;
use tuff_core::world::login_gate::{Attempt, LoginGate, LoginGates};
use tuff_item::text::TextComponent;
use tuff_storage::admin::NameAndId;
use tuff_storage::admin::PermissionLevel;

use crate::config::{EXEMPT_OPERATORS, number_key, read_number};
use crate::permissions;

pub const ENABLED: &str = "tuff.anticheat.login.enabled";
pub const PER_ADDRESS: &str = "tuff.anticheat.login.perAddress";
pub const PER_NAME: &str = "tuff.anticheat.login.perName";
pub const WINDOW: &str = "tuff.anticheat.login.windowSeconds";
pub const RELOG_DELAY: &str = "tuff.anticheat.login.relogDelaySeconds";
pub const RELOG_WARNINGS: &str = "tuff.anticheat.login.relogWarnings";
pub const HOLD: &str = "tuff.anticheat.login.holdSeconds";
pub const EXEMPT_LOOPBACK: &str = "tuff.anticheat.login.exemptLoopback";

const MAX_TRACKED: usize = 4096;

pub fn keys() -> Vec<Key> {
    let count = |name: &str, default: i64, description: &str| {
        Key::new(name, Value::Int(default), description).validated(|value| match value {
            Value::Int(n) if *n >= 0 => Ok(()),
            _ => Err("must not be negative".to_owned()),
        })
    };
    vec![
        Key::new(
            ENABLED,
            Value::Bool(true),
            "Throttle logins per address and name, and quick relogs",
        ),
        count(
            PER_ADDRESS,
            30,
            "Logins one address may make per window (0 is unlimited)",
        ),
        count(
            PER_NAME,
            8,
            "Logins one player name may make per window (0 is unlimited)",
        ),
        number_key(WINDOW, 60.0, "Length of the login window in seconds"),
        number_key(
            RELOG_DELAY,
            2.0,
            "Seconds a player should wait after leaving before logging in again",
        ),
        count(
            RELOG_WARNINGS,
            3,
            "Quick relogs per window that are only warned about before the name is held off",
        ),
        number_key(
            HOLD,
            30.0,
            "Seconds a name that kept relogging too fast is refused for",
        ),
        Key::new(
            EXEMPT_LOOPBACK,
            Value::Bool(true),
            "Never throttle logins from the server's own machine",
        ),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub enabled: bool,
    pub per_address: usize,
    pub per_name: usize,
    pub window: Duration,
    pub relog_delay: Duration,
    pub relog_warnings: usize,
    pub hold: Duration,
    pub exempt_loopback: bool,
    pub exempt_operators: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            enabled: true,
            per_address: 30,
            per_name: 8,
            window: Duration::from_secs(60),
            relog_delay: Duration::from_secs(2),
            relog_warnings: 3,
            hold: Duration::from_secs(30),
            exempt_loopback: true,
            exempt_operators: true,
        }
    }
}

impl Limits {
    pub fn read(store: &ConfigStore) -> Limits {
        let d = Limits::default();
        let count = |name, default: usize| {
            store
                .get_int(name)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(default)
        };
        let seconds = |name, default: Duration| {
            read_number(store, name).map_or(default, Duration::from_secs_f64)
        };
        Limits {
            enabled: store.get_bool(ENABLED).unwrap_or(d.enabled),
            per_address: count(PER_ADDRESS, d.per_address),
            per_name: count(PER_NAME, d.per_name),
            window: seconds(WINDOW, d.window),
            relog_delay: seconds(RELOG_DELAY, d.relog_delay),
            relog_warnings: count(RELOG_WARNINGS, d.relog_warnings),
            hold: seconds(HOLD, d.hold),
            exempt_loopback: store.get_bool(EXEMPT_LOOPBACK).unwrap_or(d.exempt_loopback),
            exempt_operators: store
                .get_bool(EXEMPT_OPERATORS)
                .unwrap_or(d.exempt_operators),
        }
    }
}

#[derive(Default)]
struct Record {
    logins: VecDeque<Instant>,
    strikes: VecDeque<Instant>,
    left: Option<Instant>,
    held_until: Option<Instant>,
}

impl Record {
    fn prune(&mut self, now: Instant, window: Duration) {
        let fresh = |at: &Instant| now.saturating_duration_since(*at) < window;
        while self.logins.front().is_some_and(|at| !fresh(at)) {
            self.logins.pop_front();
        }
        while self.strikes.front().is_some_and(|at| !fresh(at)) {
            self.strikes.pop_front();
        }
    }

    fn idle(&self, now: Instant, window: Duration) -> bool {
        self.logins.is_empty()
            && self.strikes.is_empty()
            && self.held_until.is_none_or(|until| until <= now)
            && self
                .left
                .is_none_or(|at| now.saturating_duration_since(at) >= window)
    }
}

/// What a throttle decided on one login.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Admit,
    /// Admitted, but the player relogged faster than the minimum delay.
    Warn,
    Refuse(&'static str),
}

/// The throttle's state, owned by the world's coordinator.
#[derive(Default)]
pub struct Throttle {
    addresses: HashMap<IpAddr, Record>,
    names: HashMap<String, Record>,
}

fn sweep<K>(map: &mut HashMap<K, Record>, now: Instant, window: Duration) {
    if map.len() >= MAX_TRACKED {
        map.retain(|_, record| {
            record.prune(now, window);
            !record.idle(now, window)
        });
    }
}

impl Throttle {
    pub fn decide(
        &mut self,
        limits: &Limits,
        name: &str,
        address: IpAddr,
        now: Instant,
    ) -> Decision {
        let window = limits.window;
        sweep(&mut self.addresses, now, window);
        sweep(&mut self.names, now, window);
        let name = self.names.entry(name.to_lowercase()).or_default();
        name.prune(now, window);
        let site = self.addresses.entry(address).or_default();
        site.prune(now, window);
        if name.held_until.is_some_and(|until| until > now) {
            return Decision::Refuse("You are reconnecting too often, please wait a moment");
        }
        if limits.per_name > 0 && name.logins.len() >= limits.per_name
            || limits.per_address > 0 && site.logins.len() >= limits.per_address
        {
            return Decision::Refuse("Logging in too fast, please try again in a minute");
        }
        let quick = name
            .left
            .is_some_and(|at| now.saturating_duration_since(at) < limits.relog_delay);
        let mut decision = Decision::Admit;
        if quick {
            name.strikes.push_back(now);
            if name.strikes.len() > limits.relog_warnings {
                name.held_until = Some(now + limits.hold);
                name.strikes.clear();
                return Decision::Refuse("You are reconnecting too often, please wait a moment");
            }
            decision = Decision::Warn;
        }
        name.logins.push_back(now);
        site.logins.push_back(now);
        decision
    }

    pub fn left(&mut self, name: &str, now: Instant) {
        self.names.entry(name.to_lowercase()).or_default().left = Some(now);
    }
}

pub struct ThrottleGates;

impl LoginGates for ThrottleGates {
    fn open(&self) -> Box<dyn LoginGate> {
        Box::new(Throttle::default())
    }
}

impl LoginGate for Throttle {
    fn admit(&mut self, attempt: &Attempt<'_>) -> Result<(), TextComponent> {
        let limits = Limits::read(&attempt.shared.config);
        if !limits.enabled
            || (limits.exempt_loopback && attempt.address.is_loopback())
            || (limits.exempt_operators && attempt.permission > PermissionLevel::All)
        {
            return Ok(());
        }
        let subject = Subject::Player {
            uuid: attempt.user.uuid,
            level: attempt.permission,
        };
        let view = attempt.shared.permissions.view();
        if view.player(attempt.user.uuid).is_some()
            && (view.allows(subject, permissions::BYPASS)
                || view.allows(subject, permissions::LOGIN))
        {
            return Ok(());
        }
        match self.decide(&limits, &attempt.user.name, attempt.address, attempt.now) {
            Decision::Admit => Ok(()),
            Decision::Warn => {
                tuff_core::log::warn!(
                    "{} ({}) relogged within {:.1} s",
                    attempt.user.name,
                    attempt.address,
                    limits.relog_delay.as_secs_f64()
                );
                Ok(())
            }
            Decision::Refuse(reason) => {
                tuff_core::log::warn!(
                    "refused login of {} ({}): {reason}",
                    attempt.user.name,
                    attempt.address
                );
                Err(TextComponent::new(reason))
            }
        }
    }

    fn left(&mut self, user: &NameAndId, _address: IpAddr, now: Instant) {
        Throttle::left(self, &user.name, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> IpAddr {
        IpAddr::from([203, 0, 113, n])
    }

    fn secs(start: Instant, s: f64) -> Instant {
        start + Duration::from_secs_f64(s)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_reconnect_after_a_crash_is_admitted() {
        let limits = Limits::default();
        let mut throttle = Throttle::default();
        let t0 = Instant::now();
        assert_eq!(
            throttle.decide(&limits, "Steve", addr(1), t0),
            Decision::Admit
        );
        throttle.left("Steve", secs(t0, 600.0));
        assert_eq!(
            throttle.decide(&limits, "steve", addr(1), secs(t0, 600.2)),
            Decision::Warn,
            "an instant reconnect is only warned about"
        );
        throttle.left("Steve", secs(t0, 700.0));
        assert_eq!(
            throttle.decide(&limits, "Steve", addr(1), secs(t0, 705.0)),
            Decision::Admit
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn relogging_in_a_loop_is_warned_then_held_off() {
        let limits = Limits::default();
        let mut throttle = Throttle::default();
        let t0 = Instant::now();
        let mut decisions = Vec::new();
        for n in 0..6 {
            let at = secs(t0, f64::from(n));
            decisions.push(throttle.decide(&limits, "Alex", addr(2), at));
            throttle.left("Alex", secs(t0, f64::from(n) + 0.5));
        }
        assert_eq!(decisions[0], Decision::Admit);
        assert_eq!(
            &decisions[1..4],
            &[Decision::Warn, Decision::Warn, Decision::Warn]
        );
        assert!(matches!(decisions[4], Decision::Refuse(_)), "{decisions:?}");
        assert!(matches!(
            throttle.decide(&limits, "Alex", addr(3), secs(t0, 20.0)),
            Decision::Refuse(_)
        ));
        assert_eq!(
            throttle.decide(&limits, "Alex", addr(3), secs(t0, 40.0)),
            Decision::Admit,
            "the hold ends"
        );
        assert_eq!(
            throttle.decide(&limits, "Other", addr(2), secs(t0, 6.0)),
            Decision::Admit
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn login_rate_is_limited_per_name_and_address_within_the_window() {
        let limits = Limits {
            per_address: 3,
            per_name: 2,
            ..Limits::default()
        };
        let mut throttle = Throttle::default();
        let t0 = Instant::now();
        assert_eq!(throttle.decide(&limits, "a", addr(4), t0), Decision::Admit);
        assert_eq!(
            throttle.decide(&limits, "a", addr(4), secs(t0, 10.0)),
            Decision::Admit
        );
        assert!(matches!(
            throttle.decide(&limits, "a", addr(5), secs(t0, 11.0)),
            Decision::Refuse(_)
        ));
        assert_eq!(
            throttle.decide(&limits, "b", addr(4), secs(t0, 12.0)),
            Decision::Admit
        );
        assert!(matches!(
            throttle.decide(&limits, "c", addr(4), secs(t0, 13.0)),
            Decision::Refuse(_)
        ));
        assert_eq!(
            throttle.decide(&limits, "a", addr(4), secs(t0, 61.0)),
            Decision::Admit
        );
        let open = Limits {
            per_address: 0,
            per_name: 0,
            ..Limits::default()
        };
        assert!((0..100).all(|n| throttle.decide(
            &open,
            "z",
            addr(9),
            secs(t0, 100.0 + f64::from(n) * 5.0)
        ) == Decision::Admit));
    }
}
