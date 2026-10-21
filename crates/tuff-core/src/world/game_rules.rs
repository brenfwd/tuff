//! Game rules: the registry's rules with the world's current values, kept
//! in `game_rules` saved data as the game writes it (`namespace:rule`
//! keys, byte booleans and int values). Rules from a newer game or a mod
//! are preserved as loaded. A rule that needs a feature flag the world
//! lacks does not exist there and is dropped from its data.

use std::collections::BTreeMap;

use tuff_vanilla_data::game_rule::{GameRule, GameRuleValue};

use tuff_nbt::{Compound, Tag};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleValue {
    Bool(bool),
    Int(i32),
}

impl RuleValue {
    pub fn serialize(self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Int(value) => value.to_string(),
        }
    }

    /// The command result the game reports: 1/0 for booleans.
    pub fn command_result(self) -> i32 {
        match self {
            Self::Bool(value) => i32::from(value),
            Self::Int(value) => value,
        }
    }
}

/// Why [`GameRules::set`] refused a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidRule;

#[derive(Debug, Clone, PartialEq)]
pub struct GameRules {
    values: BTreeMap<&'static str, RuleValue>,
    /// Entries this server does not know, written back untouched.
    unknown: Compound,
}

impl Default for GameRules {
    fn default() -> Self {
        Self::load(&Compound::new())
    }
}

impl GameRules {
    /// Reads the saved data's inner compound, taking defaults for missing
    /// or unreadable rules.
    pub fn load(saved: &Compound) -> Self {
        let mut rules = Self {
            values: BTreeMap::new(),
            unknown: saved.clone(),
        };
        for rule in tuff_vanilla_data::game_rule::ALL {
            if rule.enabled_by_default() {
                rules.enable(rule);
            }
        }
        rules
    }

    /// Fits the rules to a world with the feature flags `features`: rules
    /// they enable appear (with any value the data kept), the rest go.
    pub fn apply_features<S: AsRef<str>>(&mut self, features: &[S]) {
        for rule in tuff_vanilla_data::game_rule::ALL {
            if rule.enabled_by(features) {
                self.enable(rule);
            } else {
                self.values.remove(rule.name);
                self.unknown.remove(rule.name);
            }
        }
    }

    fn enable(&mut self, rule: &'static GameRule) {
        if self.values.contains_key(rule.name) {
            return;
        }
        let stored = self.unknown.remove(rule.name);
        let stored = stored.as_ref();
        let value = match rule.value {
            GameRuleValue::Bool(default) => {
                RuleValue::Bool(stored.and_then(Tag::as_bool).unwrap_or(default))
            }
            GameRuleValue::Int { default, min, max } => RuleValue::Int(
                stored
                    .and_then(Tag::as_int)
                    .filter(|v| (min..=max).contains(v))
                    .unwrap_or(default),
            ),
        };
        self.values.insert(rule.name, value);
    }

    pub fn save(&self) -> Compound {
        let mut out = self.unknown.clone();
        for (name, value) in &self.values {
            match *value {
                RuleValue::Bool(value) => out.insert(*name, value),
                RuleValue::Int(value) => out.insert(*name, value),
            };
        }
        out
    }

    /// A rule the game knows, whether or not this world has it.
    pub fn rule(name: &str) -> Option<&'static GameRule> {
        GameRule::get(name)
    }

    /// Whether this world has the rule.
    pub fn has(&self, name: &str) -> bool {
        Self::rule(name).is_some_and(|rule| self.values.contains_key(rule.name))
    }

    /// The rule's value; `None` for a rule this world does not have.
    pub fn get(&self, name: &str) -> Option<RuleValue> {
        let rule = Self::rule(name)?;
        self.values.get(rule.name).copied()
    }

    pub fn bool(&self, name: &str) -> bool {
        match self.get(name) {
            Some(RuleValue::Bool(value)) => value,
            _ => false,
        }
    }

    pub fn int(&self, name: &str) -> i32 {
        match self.get(name) {
            Some(RuleValue::Int(value)) => value,
            _ => 0,
        }
    }

    /// Sets a rule; `Err` names a rule this world does not have or a
    /// value of the wrong type or out of bounds.
    pub fn set(&mut self, name: &str, value: RuleValue) -> Result<(), InvalidRule> {
        let rule = Self::rule(name)
            .filter(|rule| self.values.contains_key(rule.name))
            .ok_or(InvalidRule)?;
        match (rule.value, value) {
            (GameRuleValue::Bool(_), RuleValue::Bool(_)) => {}
            (GameRuleValue::Int { min, max, .. }, RuleValue::Int(v))
                if (min..=max).contains(&v) => {}
            _ => return Err(InvalidRule),
        }
        self.values.insert(rule.name, value);
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, RuleValue)> + '_ {
        self.values.iter().map(|(name, value)| (*name, *value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn defaults_come_from_the_registry_and_round_trip() {
        let rules = GameRules::default();
        assert!(!rules.bool("keep_inventory"));
        assert!(rules.bool("minecraft:send_command_feedback"));
        assert_eq!(rules.int("random_tick_speed"), 3);
        assert_eq!(rules.int("max_block_modifications"), 32768);
        assert!(
            rules.get("max_minecart_speed").is_none(),
            "feature-gated rules are absent"
        );
        let saved = rules.save();
        assert_eq!(saved.get_bool("minecraft:keep_inventory"), Some(false));
        assert_eq!(saved.get_int("minecraft:respawn_radius"), Some(10));
        assert!(!saved.contains_key("minecraft:max_minecart_speed"));
        assert_eq!(GameRules::load(&saved), rules);
    }

    #[tuff_core::watchdog]
    #[test]
    fn set_validates_and_unknown_rules_survive() {
        let mut saved = Compound::new()
            .with("minecraft:keep_inventory", true)
            .with("minecraft:random_tick_speed", 7)
            .with("mod:custom", "yes")
            .with("minecraft:fall_damage", 12);
        saved.insert("minecraft:respawn_radius", -5);
        let mut rules = GameRules::load(&saved);
        assert!(rules.bool("keep_inventory"));
        assert_eq!(rules.int("random_tick_speed"), 7);
        assert!(
            rules.bool("fall_damage"),
            "a wrongly typed value falls back to the default"
        );
        assert_eq!(rules.int("respawn_radius"), 10, "out of bounds falls back");
        assert_eq!(rules.set("keep_inventory", RuleValue::Bool(false)), Ok(()));
        assert_eq!(
            rules.set("keep_inventory", RuleValue::Int(1)),
            Err(InvalidRule)
        );
        assert_eq!(
            rules.set("random_tick_speed", RuleValue::Int(-1)),
            Err(InvalidRule)
        );
        assert_eq!(rules.set("nope", RuleValue::Bool(true)), Err(InvalidRule));
        assert_eq!(
            rules.set("max_snow_accumulation_height", RuleValue::Int(8)),
            Ok(())
        );
        assert_eq!(
            rules.set("max_snow_accumulation_height", RuleValue::Int(9)),
            Err(InvalidRule)
        );
        let out = rules.save();
        assert_eq!(out.get_str("mod:custom"), Some("yes"));
        assert_eq!(out.get_bool("minecraft:keep_inventory"), Some(false));
        assert_eq!(RuleValue::Bool(true).command_result(), 1);
        assert_eq!(RuleValue::Int(5).serialize(), "5");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_feature_flag_brings_its_rules_and_dropping_it_drops_them() {
        let saved = Compound::new().with("minecraft:max_minecart_speed", 20);
        let mut rules = GameRules::load(&saved);
        assert!(!rules.has("max_minecart_speed"));
        assert_eq!(
            rules.set("max_minecart_speed", RuleValue::Int(9)),
            Err(InvalidRule),
            "no such rule without the flag"
        );
        let flags = ["minecraft:vanilla", "minecraft:minecart_improvements"];
        rules.apply_features(&flags);
        assert_eq!(rules.int("max_minecart_speed"), 20, "the saved value");
        assert_eq!(
            rules.set("max_minecart_speed", RuleValue::Int(1001)),
            Err(InvalidRule)
        );
        assert_eq!(rules.set("max_minecart_speed", RuleValue::Int(30)), Ok(()));
        let out = rules.save();
        assert_eq!(out.get_int("minecraft:max_minecart_speed"), Some(30));
        let mut reloaded = GameRules::load(&out);
        reloaded.apply_features(&flags);
        assert_eq!(reloaded, rules);

        let mut fresh = GameRules::default();
        fresh.apply_features(&flags);
        assert_eq!(fresh.int("max_minecart_speed"), 8, "the default");
        fresh.apply_features(&["minecraft:vanilla"]);
        assert!(!fresh.has("max_minecart_speed"));
        assert!(!fresh.save().contains_key("minecraft:max_minecart_speed"));
    }
}
