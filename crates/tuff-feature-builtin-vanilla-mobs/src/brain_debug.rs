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
}

/// The activity a goal-driven mob shows when its species names none.
pub fn default_activity(core: &MobCore) -> &'static str {
    if core.target.is_some() {
        "fight"
    } else {
        "idle"
    }
}

/// The memories every brain mob has that tuff tracks on [`MobCore`],
/// formatted like the vanilla dump (`key: value`).
pub fn core_memories(core: &MobCore, tick: u64) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(id) = core.target {
        out.push(format!("attack_target: #{id}"));
    }
    if let Some((id, _)) = core.last_hurt_by_mob {
        out.push(format!("hurt_by_entity: #{id} (ttl: {tick})"));
    }
    if let Some(anger) = core.anger {
        out.push(format!("angry_at: {}", uuid_name(anger.target)));
        if anger.universal {
            out.push("universal_anger: true".to_owned());
        }
    }
    out
}

pub fn dump(core: &MobCore, state: BrainState, behaviors: Vec<String>, tick: u64) -> DebugValue {
    let mut memories = core_memories(core, tick);
    memories.extend(state.memories);
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
