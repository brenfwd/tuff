//! The brains debug feed for mobs whose vanilla AI is activity driven,
//! built from tuff's goals and shared mob state.

use tuff_core::protocol::play::{DebugBrain, DebugValue};
use tuff_core::world::debug_feeds::{entity_name, uuid_name};
use tuff_vanilla_data::registry::EntityType;

use crate::core::MobCore;

/// Whether vanilla gives this mob a brain, so it shows in the brains feed.
pub fn has_brain(kind: EntityType) -> bool {
    matches!(
        kind,
        EntityType::Allay
            | EntityType::Armadillo
            | EntityType::Axolotl
            | EntityType::Breeze
            | EntityType::Camel
            | EntityType::CamelHusk
            | EntityType::CopperGolem
            | EntityType::Creaking
            | EntityType::Frog
            | EntityType::Goat
            | EntityType::HappyGhast
            | EntityType::Hoglin
            | EntityType::Nautilus
            | EntityType::Piglin
            | EntityType::PiglinBrute
            | EntityType::Sniffer
            | EntityType::Tadpole
            | EntityType::Villager
            | EntityType::Warden
            | EntityType::Zoglin
            | EntityType::ZombieNautilus
    )
}

/// What a species adds to the shared dump.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct BrainState {
    /// Its current activity beyond `core`, if not derived from the target.
    pub activity: Option<&'static str>,
    pub memories: Vec<String>,
    pub anger_level: Option<i32>,
    pub avoid_target: Option<i32>,
}

/// The activity a goal-driven mob shows when its species names none.
pub fn default_activity(core: &MobCore) -> &'static str {
    if core.target.is_some() {
        "fight"
    } else {
        "idle"
    }
}

fn entity_ref(id: i32, names: &dyn Fn(i32) -> Option<String>) -> String {
    names(id).unwrap_or_else(|| format!("#{id}"))
}

/// The memories every brain mob has that tuff tracks on [`MobCore`],
/// formatted like the vanilla dump (`key: value`).
pub fn core_memories(
    core: &MobCore,
    tick: u64,
    names: &dyn Fn(i32) -> Option<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(id) = core.target {
        out.push(format!("attack_target: {}", entity_ref(id, names)));
    }
    if let Some((id, _)) = core.last_hurt_by_mob {
        out.push(format!(
            "hurt_by_entity: {} (ttl: {tick})",
            entity_ref(id, names)
        ));
    }
    if let Some(anger) = core.anger {
        out.push(format!("angry_at: {}", uuid_name(anger.target)));
        if anger.universal {
            out.push("universal_anger: true".to_owned());
        }
    }
    out
}

/// Every memory type the species' brain registers (its sensors' outputs
/// plus the memories its behaviors use), which the dump lists with `-`
/// while empty.
pub fn registered_memories(kind: EntityType) -> &'static [&'static str] {
    match kind {
        EntityType::Goat => &[
            "breed_target",
            "cant_reach_walk_target_since",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "long_jump_cooling_down",
            "long_jump_mid_jump",
            "look_target",
            "mobs",
            "nearest_players",
            "nearest_visible_adult",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "nearest_visible_wanted_item",
            "path",
            "ram_cooldown_ticks",
            "ram_target",
            "temptation_cooldown_ticks",
            "tempting_player",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Frog => &[
            "attack_target",
            "breed_target",
            "cant_reach_walk_target_since",
            "hurt_by",
            "hurt_by_entity",
            "is_in_water",
            "is_panicking",
            "is_pregnant",
            "long_jump_cooling_down",
            "long_jump_mid_jump",
            "look_target",
            "mobs",
            "nearest_attackable",
            "path",
            "temptation_cooldown_ticks",
            "tempting_player",
            "unreachable_tongue_targets",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Tadpole => &[
            "breed_target",
            "cant_reach_walk_target_since",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "look_target",
            "mobs",
            "nearest_players",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "path",
            "temptation_cooldown_ticks",
            "tempting_player",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Camel | EntityType::CamelHusk => &[
            "breed_target",
            "cant_reach_walk_target_since",
            "gaze_cooldown_ticks",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "look_target",
            "mobs",
            "nearest_visible_adult",
            "path",
            "temptation_cooldown_ticks",
            "tempting_player",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Sniffer => &[
            "attack_target",
            "breed_target",
            "cant_reach_walk_target_since",
            "disturbance_location",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "is_sniffing",
            "look_target",
            "mobs",
            "nearest_attackable",
            "nearest_players",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "path",
            "sniff_cooldown",
            "sniffer_digging",
            "sniffer_explored_positions",
            "sniffer_happy",
            "sniffer_sniffing_target",
            "temptation_cooldown_ticks",
            "tempting_player",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Allay => &[
            "cant_reach_walk_target_since",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "item_pickup_cooldown_ticks",
            "liked_noteblock",
            "liked_noteblock_cooldown_ticks",
            "liked_player",
            "look_target",
            "mobs",
            "nearest_players",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "nearest_visible_wanted_item",
            "path",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Axolotl => &[
            "attack_cooling_down",
            "attack_target",
            "breed_target",
            "cant_reach_walk_target_since",
            "has_hunting_cooldown",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "look_target",
            "mobs",
            "nearest_attackable",
            "nearest_visible_adult",
            "path",
            "play_dead_ticks",
            "temptation_cooldown_ticks",
            "tempting_player",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Armadillo => &[
            "breed_target",
            "cant_reach_walk_target_since",
            "danger_detected_recently",
            "gaze_cooldown_ticks",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "look_target",
            "mobs",
            "nearest_visible_adult",
            "path",
            "temptation_cooldown_ticks",
            "tempting_player",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Breeze => &[
            "attack_target",
            "breeze_jump_target",
            "breeze_shoot",
            "cant_reach_walk_target_since",
            "hurt_by",
            "hurt_by_entity",
            "look_target",
            "mobs",
            "nearest_attackable",
            "nearest_players",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "path",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Creaking => &[
            "attack_cooling_down",
            "attack_target",
            "cant_reach_walk_target_since",
            "look_target",
            "mobs",
            "nearest_players",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "path",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Nautilus => &[
            "angry_at",
            "attack_target",
            "attack_target_cooldown",
            "breed_target",
            "cant_reach_walk_target_since",
            "charge_cooldown_ticks",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "look_target",
            "mobs",
            "nearest_players",
            "nearest_visible_adult",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "path",
            "temptation_cooldown_ticks",
            "tempting_player",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::ZombieNautilus => &[
            "angry_at",
            "attack_target",
            "attack_target_cooldown",
            "breed_target",
            "cant_reach_walk_target_since",
            "charge_cooldown_ticks",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "look_target",
            "mobs",
            "nearest_players",
            "nearest_visible_adult",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "path",
            "temptation_cooldown_ticks",
            "tempting_player",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Hoglin => &[
            "attack_cooling_down",
            "attack_target",
            "avoid_target",
            "breed_target",
            "cant_reach_walk_target_since",
            "is_panicking",
            "look_target",
            "mobs",
            "nearest_players",
            "nearest_repellent",
            "nearest_visible_adult",
            "nearest_visible_adult_hoglins",
            "nearest_visible_adult_piglin",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "pacified",
            "path",
            "visible_adult_hoglin_count",
            "visible_adult_piglin_count",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Zoglin => &[
            "attack_cooling_down",
            "attack_target",
            "cant_reach_walk_target_since",
            "look_target",
            "mobs",
            "nearest_players",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "path",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Piglin => &[
            "admiring_disabled",
            "admiring_item",
            "angry_at",
            "ate_recently",
            "attack_cooling_down",
            "attack_target",
            "avoid_target",
            "cant_reach_walk_target_since",
            "celebrate_location",
            "dancing",
            "doors_to_close",
            "hunted_recently",
            "hurt_by",
            "hurt_by_entity",
            "interaction_target",
            "item_pickup_cooldown_ticks",
            "look_target",
            "mobs",
            "nearby_adult_piglins",
            "nearest_player_holding_wanted_item",
            "nearest_players",
            "nearest_repellent",
            "nearest_targetable_player_not_wearing_gold",
            "nearest_visible_adult_piglins",
            "nearest_visible_baby_hoglin",
            "nearest_visible_huntable_hoglin",
            "nearest_visible_nemesis",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "nearest_visible_wanted_item",
            "nearest_visible_zombified",
            "path",
            "ride_target",
            "spear_charge_position",
            "spear_engage_time",
            "spear_fleeing_position",
            "spear_fleeing_time",
            "spear_status",
            "time_trying_to_reach_admire_item",
            "universal_anger",
            "visible_adult_hoglin_count",
            "visible_adult_piglin_count",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::PiglinBrute => &[
            "angry_at",
            "attack_cooling_down",
            "attack_target",
            "cant_reach_walk_target_since",
            "doors_to_close",
            "home",
            "hurt_by",
            "hurt_by_entity",
            "interaction_target",
            "look_target",
            "mobs",
            "nearby_adult_piglins",
            "nearest_players",
            "nearest_visible_adult_piglins",
            "nearest_visible_nemesis",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "nearest_visible_wanted_item",
            "path",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Warden => &[
            "attack_cooling_down",
            "attack_target",
            "cant_reach_walk_target_since",
            "dig_cooldown",
            "disturbance_location",
            "is_emerging",
            "is_sniffing",
            "look_target",
            "nearest_attackable",
            "nearest_players",
            "nearest_visible_nemesis",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "path",
            "recent_projectile",
            "roar_sound_cooldown",
            "roar_sound_delay",
            "roar_target",
            "sniff_cooldown",
            "sonic_boom_cooldown",
            "sonic_boom_sound_cooldown",
            "sonic_boom_sound_delay",
            "touch_cooldown",
            "vibration_cooldown",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::CopperGolem => &[
            "cant_reach_walk_target_since",
            "doors_to_close",
            "gaze_cooldown_ticks",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "look_target",
            "mobs",
            "path",
            "transport_items_cooldown_ticks",
            "unreachable_transport_block_positions",
            "visible_mobs",
            "visited_block_positions",
            "walk_target",
        ],
        EntityType::HappyGhast => &[
            "breed_target",
            "cant_reach_walk_target_since",
            "hurt_by",
            "hurt_by_entity",
            "is_panicking",
            "look_target",
            "mobs",
            "nearest_players",
            "nearest_visible_adult",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "path",
            "temptation_cooldown_ticks",
            "tempting_player",
            "visible_mobs",
            "walk_target",
        ],
        EntityType::Villager => &[
            "breed_target",
            "cant_reach_walk_target_since",
            "golem_detected_recently",
            "home",
            "hurt_by",
            "hurt_by_entity",
            "interaction_target",
            "job_site",
            "last_slept",
            "last_woken",
            "look_target",
            "meeting_point",
            "mobs",
            "nearest_bed",
            "nearest_players",
            "nearest_visible_player",
            "nearest_visible_targetable_player",
            "nearest_visible_targetable_players",
            "nearest_visible_wanted_item",
            "path",
            "potential_job_site",
            "secondary_job_site",
            "visible_mobs",
            "visible_villager_babies",
            "walk_target",
        ],
        _ => &[],
    }
}

pub fn dump(
    core: &MobCore,
    state: BrainState,
    behaviors: Vec<String>,
    tick: u64,
    names: &dyn Fn(i32) -> Option<String>,
) -> DebugValue {
    let mut memories = core_memories(core, tick, names);
    if let Some(id) = state.avoid_target {
        memories.push(format!("avoid_target: {}", entity_ref(id, names)));
    }
    memories.extend(state.memories);
    for key in registered_memories(core.base.kind) {
        let prefix = format!("{key}:");
        if !memories.iter().any(|m| m.starts_with(&prefix)) {
            memories.push(format!("{key}: -"));
        }
    }
    memories.sort();
    let activity = state.activity.unwrap_or_else(|| default_activity(core));
    DebugValue::Brain(Box::new(DebugBrain {
        name: entity_name(&core.base),
        health: core.health,
        max_health: core.max_health(),
        anger_level: state.anger_level.unwrap_or(-1),
        activities: vec!["core".to_owned(), activity.to_owned()],
        behaviors,
        memories,
        ..DebugBrain::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dumped(kind: EntityType, setup: impl FnOnce(&mut MobCore)) -> Vec<String> {
        let mut core = MobCore::new(tuff_core::world::entity::EntityBase::new(
            kind, 1, 1, [0.0; 3], [0.0; 3],
        ));
        setup(&mut core);
        let names = |id: i32| (id == 7).then(|| "Zippy".to_owned());
        match dump(&core, BrainState::default(), Vec::new(), 5, &names) {
            DebugValue::Brain(b) => b.memories,
            other => panic!("{other:?}"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn brain_debug_names_known_targets_and_falls_back_to_the_id() {
        let memories = dumped(EntityType::Goat, |core| {
            core.target = Some(7);
            core.last_hurt_by_mob = Some((9, 0));
        });
        assert!(
            memories.contains(&"attack_target: Zippy".to_owned()),
            "{memories:?}"
        );
        assert!(
            memories.contains(&"hurt_by_entity: #9 (ttl: 5)".to_owned()),
            "{memories:?}"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn brain_debug_lists_every_registered_memory() {
        for (kind, key) in [
            (EntityType::Goat, "ram_cooldown_ticks"),
            (EntityType::Frog, "is_pregnant"),
            (EntityType::Sniffer, "sniffer_digging"),
        ] {
            let memories = dumped(kind, |_| {});
            assert!(
                memories.contains(&format!("{key}: -")),
                "{kind:?} {memories:?}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn registered_memories_cover_every_brain_species() {
        let species = [
            EntityType::Allay,
            EntityType::Armadillo,
            EntityType::Axolotl,
            EntityType::Breeze,
            EntityType::Camel,
            EntityType::CamelHusk,
            EntityType::CopperGolem,
            EntityType::Creaking,
            EntityType::Frog,
            EntityType::Goat,
            EntityType::HappyGhast,
            EntityType::Hoglin,
            EntityType::Nautilus,
            EntityType::Piglin,
            EntityType::PiglinBrute,
            EntityType::Sniffer,
            EntityType::Tadpole,
            EntityType::Villager,
            EntityType::Warden,
            EntityType::Zoglin,
            EntityType::ZombieNautilus,
        ];
        for kind in species {
            assert!(has_brain(kind), "{kind:?}");
            let list = registered_memories(kind);
            assert!(
                list.windows(2).all(|w| w[0] < w[1]),
                "{kind:?} sorted and unique"
            );
            for core in ["walk_target", "look_target", "path"] {
                assert!(list.contains(&core), "{kind:?} {core}");
            }
        }
        let expected: &[(EntityType, &[&str])] = &[
            (
                EntityType::CopperGolem,
                &[
                    "gaze_cooldown_ticks",
                    "hurt_by_entity",
                    "is_panicking",
                    "transport_items_cooldown_ticks",
                    "unreachable_transport_block_positions",
                    "visited_block_positions",
                ],
            ),
            (
                EntityType::HappyGhast,
                &[
                    "is_panicking",
                    "nearest_visible_adult",
                    "nearest_visible_player",
                    "temptation_cooldown_ticks",
                    "tempting_player",
                ],
            ),
            (
                EntityType::Warden,
                &["is_emerging", "roar_target", "dig_cooldown"],
            ),
            (EntityType::Creaking, &["attack_target", "visible_mobs"]),
            (
                EntityType::Goat,
                &["ram_target", "long_jump_mid_jump", "ram_cooldown_ticks"],
            ),
        ];
        for (kind, keys) in expected {
            let list = registered_memories(*kind);
            for key in *keys {
                assert!(list.contains(key), "{kind:?} {key}");
            }
        }
        assert!(!registered_memories(EntityType::Creaking).contains(&"hurt_by_entity"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn brain_state_names_the_species_activity() {
        use crate::Species;
        crate::tests::with_context(|core, _cx| {
            <crate::farm::Goat as Species>::configure(core);
            let goat =
                <crate::farm::Goat as Species>::create(&mut rand::SeedableRng::seed_from_u64(1));
            assert_eq!(goat.brain_state(core).activity, Some("idle"));
            core.target = Some(3);
            let tadpole = <crate::tadpole::Tadpole as Species>::create(
                &mut rand::SeedableRng::seed_from_u64(1),
            );
            assert_eq!(tadpole.brain_state(core).activity, Some("fight"));
            let creaking = <crate::creaking::Creaking as Species>::create(
                &mut rand::SeedableRng::seed_from_u64(1),
            );
            assert_eq!(creaking.brain_state(core).activity, Some("fight"));
            core.target = None;
            assert_eq!(creaking.brain_state(core).activity, Some("idle"));
        });
    }
}
