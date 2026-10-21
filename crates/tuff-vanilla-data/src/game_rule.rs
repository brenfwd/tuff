//! The built-in `minecraft:game_rule` registry with each rule's value type
//! and default, which the game keeps in code rather than in any report.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameRule {
    pub name: &'static str,
    pub value: GameRuleValue,
    /// The feature flags a world needs for the rule to exist; a world
    /// without them has no such rule and does not save it.
    pub features: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameRuleValue {
    Bool(bool),
    Int { default: i32, min: i32, max: i32 },
}

/// The group a rule belongs to, declared in the order the game sorts them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GameRuleCategory {
    Player,
    Mobs,
    Spawning,
    Drops,
    Updates,
    Chat,
    Misc,
}

impl GameRuleCategory {
    pub const ALL: [GameRuleCategory; 7] = [
        GameRuleCategory::Player,
        GameRuleCategory::Mobs,
        GameRuleCategory::Spawning,
        GameRuleCategory::Drops,
        GameRuleCategory::Updates,
        GameRuleCategory::Chat,
        GameRuleCategory::Misc,
    ];

    pub fn id(self) -> &'static str {
        match self {
            GameRuleCategory::Player => "minecraft:player",
            GameRuleCategory::Mobs => "minecraft:mobs",
            GameRuleCategory::Spawning => "minecraft:spawning",
            GameRuleCategory::Drops => "minecraft:drops",
            GameRuleCategory::Updates => "minecraft:updates",
            GameRuleCategory::Chat => "minecraft:chat",
            GameRuleCategory::Misc => "minecraft:misc",
        }
    }
}

impl GameRule {
    pub fn category(&self) -> GameRuleCategory {
        match self.short_name() {
            "drowning_damage"
            | "elytra_movement_check"
            | "ender_pearls_vanish_on_death"
            | "fall_damage"
            | "fire_damage"
            | "freeze_damage"
            | "immediate_respawn"
            | "keep_inventory"
            | "limited_crafting"
            | "locator_bar"
            | "natural_health_regeneration"
            | "player_movement_check"
            | "players_nether_portal_creative_delay"
            | "players_nether_portal_default_delay"
            | "players_sleeping_percentage"
            | "pvp"
            | "respawn_radius"
            | "spectators_generate_chunks" => GameRuleCategory::Player,
            "forgive_dead_players"
            | "max_entity_cramming"
            | "mob_griefing"
            | "raids"
            | "universal_anger" => GameRuleCategory::Mobs,
            "spawn_mobs"
            | "spawn_monsters"
            | "spawn_patrols"
            | "spawn_phantoms"
            | "spawn_wandering_traders"
            | "spawn_wardens" => GameRuleCategory::Spawning,
            "block_drops"
            | "block_explosion_drop_decay"
            | "entity_drops"
            | "mob_drops"
            | "mob_explosion_drop_decay"
            | "projectiles_can_break_blocks"
            | "tnt_explosion_drop_decay" => GameRuleCategory::Drops,
            "advance_time"
            | "advance_weather"
            | "fire_spread_radius_around_player"
            | "lava_source_conversion"
            | "max_snow_accumulation_height"
            | "random_tick_speed"
            | "spread_vines"
            | "water_source_conversion" => GameRuleCategory::Updates,
            "command_block_output"
            | "log_admin_commands"
            | "send_command_feedback"
            | "show_advancement_messages"
            | "show_death_messages" => GameRuleCategory::Chat,
            "allow_entering_nether_using_portals"
            | "command_blocks_work"
            | "global_sound_events"
            | "max_block_modifications"
            | "max_command_forks"
            | "max_command_sequence_length"
            | "max_minecart_speed"
            | "reduced_debug_info"
            | "spawner_blocks_work"
            | "tnt_explodes" => GameRuleCategory::Misc,
            other => unreachable!("game rule {other} has no category"),
        }
    }

    pub fn get(name: &str) -> Option<&'static GameRule> {
        let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
        ALL.binary_search_by(|rule| {
            let wanted = namespace.bytes().chain(*b":").chain(path.bytes());
            rule.name.bytes().cmp(wanted)
        })
        .ok()
        .map(|index| &ALL[index])
    }

    /// Whether the rule exists in a world with the feature flags `world`.
    pub fn enabled_by<S: AsRef<str>>(&self, world: &[S]) -> bool {
        self.features
            .iter()
            .all(|flag| world.iter().any(|w| w.as_ref() == *flag))
    }

    /// Whether the rule exists with the default feature set.
    pub fn enabled_by_default(&self) -> bool {
        self.features.is_empty()
    }

    /// The name without the `minecraft:` namespace, as `/gamerule` shows it.
    pub fn short_name(&self) -> &'static str {
        self.name.strip_prefix("minecraft:").unwrap_or(self.name)
    }
}

include!(concat!(env!("OUT_DIR"), "/game_rules.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_have_defaults_and_bounds() {
        let keep = GameRule::get("keep_inventory").unwrap();
        assert_eq!(keep.value, GameRuleValue::Bool(false));
        assert!(keep.enabled_by_default());
        let radius = GameRule::get("minecraft:respawn_radius").unwrap();
        assert_eq!(
            radius.value,
            GameRuleValue::Int {
                default: 10,
                min: 0,
                max: i32::MAX
            }
        );
        assert_eq!(
            GameRule::get("max_block_modifications").unwrap().value,
            GameRuleValue::Int {
                default: 32768,
                min: 1,
                max: i32::MAX
            }
        );
        let speed = GameRule::get("max_minecart_speed").unwrap();
        assert!(!speed.enabled_by_default());
        assert_eq!(speed.features, ["minecraft:minecart_improvements"]);
        assert!(speed.enabled_by(&["minecraft:vanilla", "minecraft:minecart_improvements"]));
        assert!(!speed.enabled_by(&["minecraft:vanilla"]));
        assert!(GameRule::get("nope").is_none());
        assert_eq!(keep.category(), GameRuleCategory::Player);
        assert_eq!(
            GameRule::get("spawn_mobs").unwrap().category(),
            GameRuleCategory::Spawning
        );
        assert!(GameRuleCategory::ALL.windows(2).all(|w| w[0] < w[1]));
        assert!(ALL.windows(2).all(|w| w[0].name < w[1].name));
        for rule in ALL {
            rule.category();
        }
    }
}
