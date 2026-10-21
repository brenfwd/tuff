//! The core side of the Config page: runtime settings, server.properties
//! and game rules as the page lists and edits them.

use std::collections::BTreeMap;

use serde_json::{Value as Json, json};

use crate::config::{ConfigError, HTTP_HOST, HTTP_PORT, Key, Kind, Value};
use crate::property_spec::*;

use crate::world::Shared;
use crate::world::game_rules::{GameRules, RuleValue};
use crate::world::messages::CoordinatorMessage;
use hyper::StatusCode;
use tuff_vanilla_data::game_rule::GameRuleValue;
use tuff_web_ui::config::{ConfigHost, REDACTED, RulePlan};
use tuff_web_ui::host::{Edit, HostError, HostFuture};

/// Settings whose change could end the editor's own panel access.
pub const LOCKOUT_KEYS: [&str; 6] = [
    tuff_web_ui::ENABLED,
    tuff_web_ui::ALLOW_REMOTE,
    tuff_web_ui::HOSTNAMES,
    tuff_web_ui::MIN_OP_LEVEL,
    HTTP_HOST,
    HTTP_PORT,
];

/// Whether changing `name` from `old` to `new` could lock the panel user
/// out: switching the panel or remote access off, raising the minimum
/// level, or moving the listener or its host names.
pub fn locks_out(name: &str, old: &Value, new: &Value) -> bool {
    if old == new {
        return false;
    }
    match name {
        tuff_web_ui::ENABLED | tuff_web_ui::ALLOW_REMOTE => *new == Value::Bool(false),
        tuff_web_ui::MIN_OP_LEVEL => matches!((old, new), (Value::Int(a), Value::Int(b)) if b > a),
        _ => LOCKOUT_KEYS.contains(&name),
    }
}

pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::String => "string",
        Kind::Int => "int",
        Kind::Bool => "bool",
        Kind::Url => "url",
        Kind::Ip => "ip",
        Kind::Port => "port",
    }
}

/// The section a key is listed under: its name without the last segment.
pub fn group(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(head, _)| head)
}

/// One setting as the API lists it; a secret one carries only `set`.
pub fn setting_json(key: &Key, value: &Value) -> Json {
    let mut out = json!({
        "name": key.name,
        "kind": kind_name(key.kind()),
        "description": key.description,
        "hints": key.hints,
        "runtimeOnly": !key.saved,
        "feature": key.feature.unwrap_or("core"),
        "group": group(&key.name),
        "secret": key.secret,
        "textComponent": key.text_component,
        "lockout": LOCKOUT_KEYS.contains(&key.name.as_str()),
    });
    if key.secret {
        out["set"] = json!(!value.to_string().is_empty());
    } else {
        out["value"] = json!(value.to_string());
        out["default"] = json!(key.default.to_string());
    }
    out
}

/// One property as the API lists it.
pub fn property_json(name: &str, spec: Option<&Property>, stored: Option<&String>) -> Json {
    let secret = spec.is_some_and(|s| s.secret);
    let mut out = json!({
        "name": name,
        "known": spec.is_some(),
        "present": stored.is_some(),
        "effect": spec.map_or("saved", |s| s.effect.name()),
        "effectNote": spec.map_or_else(
            || "not a key this version defines; kept in the file".to_owned(),
            |s| s.effect.describe(),
        ),
        "secret": secret,
    });
    match spec.map(|s| s.effect) {
        Some(Effect::Restart { reason } | Effect::Permanent { reason }) => {
            out["reason"] = json!(reason)
        }
        Some(Effect::Unsupported { reason, todo }) => {
            out["reason"] = json!(reason);
            out["todo"] = json!(todo);
        }
        _ => {}
    }
    let (kind, min, max, options) = match spec.map(|s| s.kind) {
        Some(PropertyKind::Bool) => ("bool", None, None, None),
        Some(PropertyKind::Int { min, max }) => ("int", Some(min), Some(max), None),
        Some(PropertyKind::Choice(options)) => ("choice", None, None, Some(options)),
        Some(PropertyKind::Text) | None => ("string", None, None, None),
    };
    out["kind"] = json!(kind);
    if let (Some(min), Some(max)) = (min, max) {
        out["min"] = json!(min);
        out["max"] = json!(max);
    }
    if let Some(options) = options {
        out["options"] = json!(options);
    }
    let value = stored
        .cloned()
        .or_else(|| spec.map(|s| s.default.to_owned()))
        .unwrap_or_default();
    if secret {
        out["set"] = json!(!value.is_empty());
    } else {
        out["value"] = json!(value);
        out["default"] = json!(spec.map(|s| s.default));
    }
    out
}

/// Every defined property, then the file's unknown ones, by name.
pub fn properties_json(stored: &BTreeMap<String, String>) -> Vec<Json> {
    let mut out: Vec<Json> = PROPERTIES
        .iter()
        .map(|spec| property_json(spec.name, Some(spec), stored.get(spec.name)))
        .collect();
    out.extend(
        stored
            .iter()
            .filter(|(name, _)| property(name).is_none())
            .map(|(name, value)| property_json(name, None, Some(value))),
    );
    out
}

/// The enabled game rules with their current values.
pub fn game_rules_json(rules: &GameRules) -> Vec<Json> {
    tuff_vanilla_data::game_rule::ALL
        .iter()
        .filter(|rule| rules.has(rule.name))
        .map(|rule| {
            let value = rules.get(rule.name).map(RuleValue::serialize);
            match rule.value {
                GameRuleValue::Bool(default) => json!({
                    "name": rule.short_name(),
                    "kind": "bool",
                    "default": default.to_string(),
                    "value": value,
                }),
                GameRuleValue::Int { default, min, max } => json!({
                    "name": rule.short_name(),
                    "kind": "int",
                    "default": default.to_string(),
                    "min": min,
                    "max": max,
                    "value": value,
                }),
            }
        })
        .collect()
}

/// A rule value from text, checked against the rule's type and bounds.
pub fn parse_rule(name: &str, text: &str) -> Result<RuleValue, String> {
    let rule = GameRules::rule(name).ok_or_else(|| "no such game rule".to_owned())?;
    let text = text.trim();
    match rule.value {
        GameRuleValue::Bool(_) => match text {
            "true" => Ok(RuleValue::Bool(true)),
            "false" => Ok(RuleValue::Bool(false)),
            _ => Err("must be true or false".to_owned()),
        },
        GameRuleValue::Int { min, max, .. } => match text.parse::<i32>() {
            Ok(n) if (min..=max).contains(&n) => Ok(RuleValue::Int(n)),
            _ => Err(format!("must be a whole number from {min} to {max}")),
        },
    }
}

/// The server.properties entries, after applying `change` when given.
pub async fn properties(
    shared: &Shared,
    change: Option<(String, String)>,
) -> Option<BTreeMap<String, String>> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::ServerProperties { change, reply })
        .await
        .ok()?;
    rx.await.ok()
}

/// Why `value` cannot be set for `name` on this server, beyond its type:
/// enabling the code of conduct needs its folder, as at startup.
pub async fn check_value(shared: &Shared, name: &str, value: &str) -> Result<(), String> {
    if name != "enable-code-of-conduct" || value != "true" {
        return Ok(());
    }
    let Some(dir) = shared.live.network().instance_dir.clone() else {
        return Ok(());
    };
    match tokio::task::spawn_blocking(move || crate::code_of_conduct::read(&dir)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(err) => Err(err.to_string()),
    }
}

/// What to tell whoever set a property beyond that it was saved: the
/// creation-only note on a running world.
pub fn change_note(shared: &Shared, spec: Option<&Property>) -> Option<String> {
    let spec = spec.filter(|s| s.effect == Effect::Creation)?;
    let level_name = shared
        .world_path
        .as_deref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let facts = WorldFacts {
        seed: shared.seed,
        level_name: &level_name,
    };
    Some(format!("Saved. {}", creation_note(spec.name, &facts)))
}

fn redact(text: &str, secret: bool) -> String {
    if secret && !text.is_empty() {
        REDACTED.to_owned()
    } else {
        text.to_owned()
    }
}

fn refused<T>(target: &str, detail: String, message: String) -> Edit<T> {
    Edit {
        target: target.to_owned(),
        detail: Some(detail),
        result: Err(HostError::new(StatusCode::BAD_REQUEST, message)),
    }
}

fn unaudited<T>(target: &str, status: StatusCode, message: &str) -> Edit<T> {
    Edit {
        target: target.to_owned(),
        detail: None,
        result: Err(HostError::new(status, message).quiet_if(true)),
    }
}

fn set_setting(shared: &Shared, name: &str, value: Option<String>, confirm: bool) -> Edit<Json> {
    let store = &shared.config;
    let Some(key) = store.definition(name) else {
        return unaudited(name, StatusCode::NOT_FOUND, "no such setting");
    };
    let text = value.clone().unwrap_or_default();
    let fail = |message: String| {
        let message = if key.secret && !text.is_empty() {
            message.replace(&text, REDACTED)
        } else {
            message
        };
        refused(name, redact(&text, key.secret), message)
    };
    let value = match &value {
        Some(text) if text.len() > MAX_VALUE => {
            return fail(format!("must be at most {MAX_VALUE} bytes"));
        }
        Some(text) => match Value::parse(key.kind(), text.trim()) {
            Ok(value) => value,
            Err(err) => return fail(err.to_string()),
        },
        None => key.default.clone(),
    };
    let old = store.get(name).unwrap_or_else(|| key.default.clone());
    if !confirm && locks_out(name, &old, &value) {
        return Edit {
            target: name.to_owned(),
            detail: None,
            result: Err(HostError::needs_confirm(format!(
                "changing {name} could cut off your panel access; confirm to go ahead"
            ))),
        };
    }
    if let Err(err) = store.set(name, value.clone()) {
        let message = match err {
            ConfigError::Invalid(message) => message,
            other => other.to_string(),
        };
        return fail(message);
    }
    let now = store.get(name).unwrap_or_else(|| value.clone());
    Edit {
        target: name.to_owned(),
        detail: Some(redact(&value.to_string(), key.secret)),
        result: Ok(setting_json(&key, &now)),
    }
}

async fn set_property(shared: &Shared, name: &str, value: Option<String>) -> Edit<Json> {
    let unavailable = || {
        unaudited(
            name,
            StatusCode::SERVICE_UNAVAILABLE,
            "the world is not running",
        )
    };
    let Some(stored) = properties(shared, None).await else {
        return unavailable();
    };
    let spec = property(name);
    if spec.is_none() && !stored.contains_key(name) {
        return unaudited(name, StatusCode::NOT_FOUND, "no such property");
    }
    let secret = spec.is_some_and(|s| s.secret);
    let text = match value {
        Some(text) => text,
        None => spec.map(|s| s.default.to_owned()).unwrap_or_default(),
    };
    let checked = match parse_property(spec, &text) {
        Ok(value) => check_value(shared, name, &value).await.map(|()| value),
        Err(message) => Err(message),
    };
    let value = match checked {
        Ok(value) => value,
        Err(message) => return refused(name, redact(&text, secret), message),
    };
    let Some(now) = properties(shared, Some((name.to_owned(), value.clone()))).await else {
        return unavailable();
    };
    let mut out = property_json(name, spec, now.get(name));
    if let Some(note) = change_note(shared, spec) {
        out["note"] = json!(note);
    }
    Edit {
        target: name.to_owned(),
        detail: Some(redact(&value, secret)),
        result: Ok(out),
    }
}

fn plan_rule(shared: &Shared, name: &str, value: Option<String>) -> Edit<RulePlan> {
    let Some(rule) = GameRules::rule(name) else {
        return unaudited(name, StatusCode::NOT_FOUND, "no such game rule");
    };
    let short = rule.short_name();
    let text = match value {
        Some(text) => text,
        None => match rule.value {
            GameRuleValue::Bool(default) => default.to_string(),
            GameRuleValue::Int { default, .. } => default.to_string(),
        },
    };
    let value = match parse_rule(name, &text) {
        Ok(value) => value,
        Err(message) => return refused(short, text, message),
    };
    let changed = shared.game_rules.borrow().get(name) != Some(value);
    Edit {
        target: short.to_owned(),
        detail: Some(value.serialize()),
        result: Ok(RulePlan {
            line: format!("gamerule {short} {}", value.serialize()),
            changed,
        }),
    }
}

impl ConfigHost for Shared {
    fn settings(&self) -> Vec<Json> {
        self.config
            .entries()
            .iter()
            .map(|(key, value)| setting_json(key, value))
            .collect()
    }

    fn set_setting(&self, name: &str, value: Option<String>, confirm: bool) -> Edit<Json> {
        set_setting(self, name, value, confirm)
    }

    fn properties(&self) -> HostFuture<'_, Option<Vec<Json>>> {
        Box::pin(async move { properties(self, None).await.map(|p| properties_json(&p)) })
    }

    fn set_property<'a>(
        &'a self,
        name: &'a str,
        value: Option<String>,
    ) -> HostFuture<'a, Edit<Json>> {
        Box::pin(set_property(self, name, value))
    }

    fn game_rules(&self) -> Vec<Json> {
        game_rules_json(&self.game_rules.borrow())
    }

    fn plan_rule(&self, name: &str, value: Option<String>) -> Edit<RulePlan> {
        plan_rule(self, name, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn properties_are_sorted_and_parse_by_kind() {
        assert!(PROPERTIES.windows(2).all(|w| w[0].name < w[1].name));
        let level = property("op-permission-level");
        assert_eq!(parse_property(level, " 3 ").as_deref(), Ok("3"));
        assert!(parse_property(level, "5").is_err());
        let difficulty = property("difficulty");
        assert_eq!(parse_property(difficulty, "HARD").as_deref(), Ok("hard"));
        assert!(parse_property(difficulty, "brutal").is_err());
        assert!(parse_property(property("white-list"), "yes").is_err());
        assert!(parse_property(property("motd"), "a\nwhite-list=false").is_err());
        assert_eq!(parse_property(None, " kept ").as_deref(), Ok("kept"));
        assert!(property("rcon.password").unwrap().secret);
    }

    #[tuff_core::watchdog]
    #[test]
    fn secrets_list_only_whether_they_are_set() {
        let spec = property("rcon.password");
        let shown = property_json("rcon.password", spec, Some(&"hunter2".to_owned()));
        assert_eq!(shown["set"], true);
        assert!(!shown.to_string().contains("hunter2"));
        let key = Key::new("p.token", Value::String(String::new()), "").secret();
        let shown = setting_json(&key, &Value::String("hunter2".into()));
        assert_eq!(shown["set"], true);
        assert!(shown.get("value").is_none() && !shown.to_string().contains("hunter2"));
        let plain = setting_json(&Key::new("p.n", Value::Int(1), "n"), &Value::Int(2));
        assert_eq!(
            (plain["value"].as_str(), plain["default"].as_str()),
            (Some("2"), Some("1"))
        );
        assert_eq!(plain["group"], "p");
    }

    #[tuff_core::watchdog]
    #[test]
    fn lockout_changes_need_confirmation() {
        use tuff_web_ui::{ALLOW_REMOTE, ENABLED, HOSTNAMES, MIN_OP_LEVEL};
        assert!(locks_out(ENABLED, &Value::Bool(true), &Value::Bool(false)));
        assert!(!locks_out(ENABLED, &Value::Bool(false), &Value::Bool(true)));
        assert!(locks_out(
            ALLOW_REMOTE,
            &Value::Bool(true),
            &Value::Bool(false)
        ));
        assert!(locks_out(MIN_OP_LEVEL, &Value::Int(3), &Value::Int(4)));
        assert!(!locks_out(MIN_OP_LEVEL, &Value::Int(4), &Value::Int(3)));
        let host = |s: &str| Value::String(s.into());
        assert!(locks_out(HOSTNAMES, &host(""), &host("a.example")));
        assert!(!locks_out(HOSTNAMES, &host("a"), &host("a")));
        assert!(locks_out(HTTP_PORT, &Value::Port(1), &Value::Port(2)));
        assert!(!locks_out("tuff.other", &Value::Int(1), &Value::Int(2)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn rules_parse_against_their_bounds() {
        assert_eq!(
            parse_rule("keep_inventory", "true"),
            Ok(RuleValue::Bool(true))
        );
        assert!(parse_rule("keep_inventory", "1").is_err());
        assert_eq!(parse_rule("respawn_radius", " 4 "), Ok(RuleValue::Int(4)));
        assert!(parse_rule("respawn_radius", "-1").is_err());
        assert!(parse_rule("nope", "1").is_err());
        let rules = game_rules_json(&GameRules::default());
        let keep = rules
            .iter()
            .find(|r| r["name"] == "keep_inventory")
            .unwrap();
        assert_eq!(
            (keep["kind"].as_str(), keep["value"].as_str()),
            (Some("bool"), Some("false"))
        );
    }
}
