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

const COMMON: &[&str] = &[
    "attack_target",
    "hurt_by_entity",
    "look_target",
    "nearest_players",
    "nearest_visible_player",
    "visible_mobs",
    "walk_target",
];

/// Every memory type the species' brain registers, which the dump lists
/// with `-` while empty.
pub fn registered_memories(kind: EntityType) -> Vec<&'static str> {
    let extra: &[&str] = match kind {
        EntityType::Goat => &[
            "is_panicking",
            "long_jump_cooling_down",
            "long_jump_mid_jump",
            "ram_cooldown_ticks",
            "ram_target",
            "temptation_cooldown_ticks",
        ],
        EntityType::Frog => &[
            "is_in_water",
            "is_pregnant",
            "is_panicking",
            "long_jump_cooling_down",
            "long_jump_mid_jump",
            "nearest_attackable",
            "attack_cooling_down",
            "avoid_target",
        ],
        EntityType::Sniffer => &[
            "is_panicking",
            "sniffer_digging",
            "sniffer_explored_positions",
            "sniffer_sniffing_target",
            "sniff_cooldown",
            "temptation_cooldown_ticks",
        ],
        EntityType::Tadpole | EntityType::Camel | EntityType::CamelHusk => {
            &["is_panicking", "temptation_cooldown_ticks"]
        }
        EntityType::Hoglin | EntityType::Zoglin => &["avoid_target", "pacified"],
        _ => &[],
    };
    let mut out: Vec<&'static str> = COMMON.iter().chain(extra).copied().collect();
    out.sort_unstable();
    out.dedup();
    out
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
            assert!(memories.contains(&"attack_target: -".to_owned()));
        }
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
