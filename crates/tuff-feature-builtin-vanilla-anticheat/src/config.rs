//! `tuff.anticheat.*` settings. A snapshot is cached per player and
//! refreshed every [`REFRESH_TICKS`], so no packet takes the store lock.

use std::collections::HashMap;

use tuff_core::config::{ConfigStore, Key, Value};

use crate::checks::{self, Info};
use crate::improbable;
use crate::violations::Limits;

pub const ENABLED: &str = "tuff.anticheat.enabled";
pub const EXEMPT_OPERATORS: &str = "tuff.anticheat.exemptOperators";
pub const EXEMPT_CREATIVE: &str = "tuff.anticheat.exemptCreative";
pub const EXEMPT_SPECTATORS: &str = "tuff.anticheat.exemptSpectators";
pub const ALERT: &str = "tuff.anticheat.alert";
pub const ALERT_OPERATORS: &str = "tuff.anticheat.alertOperators";
pub const KICK_THRESHOLD: &str = "tuff.anticheat.kickThreshold";
pub const KICK_MESSAGE: &str = "tuff.anticheat.kickMessage";
pub const ENFORCE: &str = "tuff.anticheat.enforce";
pub const ENFORCE_UNCALIBRATED: &str = "tuff.anticheat.enforceUncalibrated";
pub const UNCALIBRATED: &[&str] = &[];
pub const SETBACK_FALL_DAMAGE: &str = "tuff.anticheat.setbackFallDamage";

pub const DEFAULT_KICK_MESSAGE: &str = "Kicked for suspicious activity";
pub const ACTION_SETBACK: &str = "setback";
pub const ACTION_LOG: &str = "log";

pub const REFRESH_TICKS: u64 = 20;

pub fn enabled_key(check: &str) -> String {
    format!("tuff.anticheat.check.{check}.enabled")
}

pub fn feed_only_key(check: &str) -> String {
    format!("tuff.anticheat.check.{check}.feedOnly")
}

pub fn limit_key(check: &str) -> String {
    format!("tuff.anticheat.check.{check}.kickAbove")
}

pub fn decay_key(check: &str) -> String {
    format!("tuff.anticheat.check.{check}.decay")
}

pub fn action_key(check: &str) -> String {
    format!("tuff.anticheat.check.{check}.action")
}

/// A string-typed number key (the store has no float kind).
pub fn number_key(name: &str, default: f64, description: &str) -> Key {
    Key::new(name, Value::String(default.to_string()), description).validated(|value| match value {
        Value::String(text) => match text.trim().parse::<f64>() {
            Ok(n) if n.is_finite() && n >= 0.0 => Ok(()),
            _ => Err("must be a number that is 0 or more".to_owned()),
        },
        _ => Err("must be a number".to_owned()),
    })
}

pub fn read_number(store: &ConfigStore, name: &str) -> Option<f64> {
    store
        .get_string(name)
        .and_then(|text| text.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 0.0)
}

pub fn keys() -> Vec<Key> {
    let mut keys = vec![
        Key::new(
            ENABLED,
            Value::Bool(true),
            "Run the action checks on player packets",
        ),
        Key::new(
            EXEMPT_OPERATORS,
            Value::Bool(true),
            "Skip the action checks for operators",
        ),
        Key::new(
            EXEMPT_CREATIVE,
            Value::Bool(false),
            "Skip the action checks for players in creative mode",
        ),
        Key::new(
            EXEMPT_SPECTATORS,
            Value::Bool(false),
            "Skip the action checks for players in spectator mode",
        ),
        Key::new(
            ALERT,
            Value::Bool(true),
            "Log every setback and kick to the console",
        ),
        Key::new(
            ALERT_OPERATORS,
            Value::Bool(true),
            "Tell online staff holding tuff.anticheat.alerts in chat about setbacks (at most one per check and player each second) and every kick",
        ),
        Key::new(
            SETBACK_FALL_DAMAGE,
            Value::Bool(true),
            "A movement setback down to a lower safe position counts the drop as falling, so the landing hurts as the fall would have",
        ),
        Key::new(
            KICK_MESSAGE,
            Value::String(DEFAULT_KICK_MESSAGE.to_owned()),
            "What a player kicked by the anti-cheat is shown (MiniMessage); it never names the check",
        )
        .text_component(),
        Key::new(
            ENFORCE,
            Value::Bool(false),
            "Let checks set back and kick; off (default), they only count, log and alert",
        ),
        Key::new(
            ENFORCE_UNCALIBRATED,
            Value::Bool(true),
            "Let checks not yet verified on real clients (predict) set back and kick; off, they only count and report",
        ),
        number_key(
            KICK_THRESHOLD,
            0.0,
            "Multiplier on every check's kick limit: 0 (default) never kicks and only sets back and logs, 1.0 is the designed limit, larger is more lenient",
        ),
    ];
    for (name, _) in improbable::FEEDERS {
        keys.push(Key::new(
            &feed_only_key(name),
            Value::Bool(improbable::feeds_only_by_default(name)),
            &format!(
                "The {name} check only feeds the improbable score instead of setting back and climbing its own ladder"
            ),
        ));
    }
    for info in checks::INFOS {
        keys.push(Key::new(
            &enabled_key(info.name),
            Value::Bool(true),
            &format!("Run the {} check", info.name),
        ));
        keys.push(
            Key::new(
                &limit_key(info.name),
                Value::Int(info.kick_above as i64),
                &format!(
                    "Violation level of the {} check above which the player is kicked",
                    info.name
                ),
            )
            .validated(|value| match value {
                Value::Int(n) if *n >= 0 => Ok(()),
                _ => Err("must not be negative".to_owned()),
            }),
        );
        keys.push(number_key(
            &decay_key(info.name),
            info.decay,
            &format!(
                "Violation level of the {} check lost per session tick",
                info.name
            ),
        ));
        keys.push(
            Key::new(
                &action_key(info.name),
                Value::String(ACTION_SETBACK.to_owned()),
                &format!(
                    "What a {} violation does: setback undoes the action and kicks past the limit, log only counts and reports it",
                    info.name
                ),
            )
            .validated(|value| match value {
                Value::String(text) if text == ACTION_SETBACK || text == ACTION_LOG => Ok(()),
                _ => Err(format!("must be {ACTION_SETBACK} or {ACTION_LOG}")),
            }),
        );
    }
    keys.extend(checks::aim::keys());
    keys.extend(checks::fast_break::keys());
    keys
}

#[derive(Clone, Copy, Debug)]
struct PerCheck {
    enabled: bool,
    kick_above: f64,
    decay: f64,
    log_only: bool,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub enabled: bool,
    pub exempt_operators: bool,
    pub exempt_creative: bool,
    pub exempt_spectators: bool,
    pub alert: bool,
    pub alert_operators: bool,
    pub kick_threshold: f64,
    pub kick_message: String,
    pub setback_fall_damage: bool,
    pub enforce: bool,
    pub enforce_uncalibrated: bool,
    pub aim: checks::aim::AimSettings,
    pub fast_break: checks::fast_break::FastBreakSettings,
    feed_only: Vec<&'static str>,
    checks: HashMap<&'static str, PerCheck>,
}

impl Settings {
    pub fn read(store: &ConfigStore) -> Settings {
        let checks = checks::INFOS
            .iter()
            .map(|info| {
                let per = PerCheck {
                    enabled: store.get_bool(&enabled_key(info.name)).unwrap_or(true),
                    kick_above: store
                        .get_int(&limit_key(info.name))
                        .map_or(info.kick_above, |n| n as f64),
                    decay: read_number(store, &decay_key(info.name)).unwrap_or(info.decay),
                    log_only: store
                        .get_string(&action_key(info.name))
                        .is_some_and(|action| action == ACTION_LOG),
                };
                (info.name, per)
            })
            .collect();
        Settings {
            enabled: store.get_bool(ENABLED).unwrap_or(true),
            exempt_operators: store.get_bool(EXEMPT_OPERATORS).unwrap_or(true),
            exempt_creative: store.get_bool(EXEMPT_CREATIVE).unwrap_or(false),
            exempt_spectators: store.get_bool(EXEMPT_SPECTATORS).unwrap_or(false),
            alert: store.get_bool(ALERT).unwrap_or(true),
            alert_operators: store.get_bool(ALERT_OPERATORS).unwrap_or(true),
            kick_threshold: read_number(store, KICK_THRESHOLD).unwrap_or(0.0),
            kick_message: store
                .get_string(KICK_MESSAGE)
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_KICK_MESSAGE.to_owned()),
            feed_only: improbable::FEEDERS
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| {
                    store
                        .get_bool(&feed_only_key(name))
                        .unwrap_or(improbable::feeds_only_by_default(name))
                })
                .collect(),
            setback_fall_damage: store.get_bool(SETBACK_FALL_DAMAGE).unwrap_or(true),
            enforce: store.get_bool(ENFORCE).unwrap_or(false),
            enforce_uncalibrated: store.get_bool(ENFORCE_UNCALIBRATED).unwrap_or(true),
            aim: checks::aim::AimSettings::read(store),
            fast_break: checks::fast_break::FastBreakSettings::read(store),
            checks,
        }
    }

    pub fn feed_only(&self, name: &str) -> bool {
        self.feed_only.contains(&name)
    }

    pub fn check_enabled(&self, name: &str) -> bool {
        self.checks.get(name).is_none_or(|per| per.enabled)
    }

    /// Whether `name`'s violations are only counted and reported, never
    /// undone or kicked for.
    pub fn log_only(&self, name: &str) -> bool {
        !self.enforce || self.gated(name) || self.checks.get(name).is_some_and(|per| per.log_only)
    }

    fn gated(&self, name: &str) -> bool {
        !self.enforce_uncalibrated && UNCALIBRATED.contains(&name)
    }

    pub fn limits(&self, info: &Info) -> Limits {
        let per = self.checks.get(info.name);
        let kick_above = per.map_or(info.kick_above, |per| per.kick_above);
        Limits {
            decay: per.map_or(info.decay, |per| per.decay),
            kick_above: if !self.enforce
                || self.kick_threshold <= 0.0
                || self.gated(info.name)
                || per.is_some_and(|per| per.log_only)
            {
                f64::INFINITY
            } else {
                kick_above * self.kick_threshold
            },
        }
    }
}

/// The settings with one player's exemptions.
#[derive(Clone, Debug)]
pub struct Rules {
    pub settings: Settings,
    pub exempt: crate::permissions::Exemptions,
}

impl std::ops::Deref for Rules {
    type Target = Settings;

    fn deref(&self) -> &Settings {
        &self.settings
    }
}

impl Rules {
    /// Whether check `name` runs for this player.
    pub fn runs(&self, name: &str) -> bool {
        self.settings.check_enabled(name) && !self.exempt.covers(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn uncalibrated_checks_only_log_until_enforcement_is_enabled() {
        let store = ConfigStore::empty();
        for key in keys() {
            store.register(key).unwrap();
        }
        store.set(ENFORCE, Value::Bool(true)).unwrap();
        let settings = Settings::read(&store);
        assert!(!settings.log_only("predict"));
        assert!(!settings.log_only("fly"));
        let info = checks::INFOS.iter().find(|i| i.name == "predict").unwrap();
        assert!(settings.limits(info).kick_above.is_infinite());
        store
            .set(KICK_THRESHOLD, Value::String("1".to_owned()))
            .unwrap();
        let settings = Settings::read(&store);
        assert!(settings.limits(info).kick_above.is_finite());
    }

    #[tuff_core::watchdog]
    #[test]
    fn default_config_never_sets_back_or_kicks() {
        let store = ConfigStore::empty();
        for key in keys() {
            store.register(key).unwrap();
        }
        store
            .set(KICK_THRESHOLD, Value::String("1".to_owned()))
            .unwrap();
        let settings = Settings::read(&store);
        for info in checks::INFOS {
            assert!(settings.log_only(info.name), "{}", info.name);
            assert!(settings.limits(info).kick_above.is_infinite());
        }
        store.set(ENFORCE, Value::Bool(true)).unwrap();
        let settings = Settings::read(&store);
        let info = checks::INFOS.iter().find(|i| i.name == "reach").unwrap();
        assert!(!settings.log_only("reach"));
        assert!(settings.limits(info).kick_above.is_finite());
    }
}
