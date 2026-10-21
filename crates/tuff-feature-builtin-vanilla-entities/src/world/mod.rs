pub mod boat;
pub mod cushion;
pub mod display;
pub mod interaction;
pub mod item_frame;
pub mod lightning;
pub mod minecart;
pub mod ominous_item_spawner;
pub mod painting;
pub mod region;

use tuff_core::world::entity::{Effect, EntityBase, TickContext};
use tuff_core::world::physics;
use tuff_vanilla_data::registry::EntityType;

/// Water animals, which never board a vehicle they bump.
fn swims(kind: EntityType) -> bool {
    matches!(
        kind,
        EntityType::Cod
            | EntityType::Salmon
            | EntityType::Pufferfish
            | EntityType::TropicalFish
            | EntityType::Squid
            | EntityType::GlowSquid
            | EntityType::Dolphin
            | EntityType::Tadpole
    )
}

/// Seats the living things `area` touches that `boards` accepts on the
/// vehicle `base`, while seats are free.
pub(crate) fn pick_up_riders(
    base: &EntityBase,
    seats: usize,
    area: physics::Aabb,
    cx: &mut TickContext,
    boards: impl Fn(EntityType, [f64; 6]) -> bool,
) -> Vec<i32> {
    let taken = base.riders.len() + base.entity_riders.len();
    let mut free = seats.saturating_sub(taken);
    let mut mounted = Vec::new();
    for entity in cx.entities {
        if free == 0 {
            break;
        }
        let alive = entity.living.is_some_and(|l| l.alive);
        if !alive
            || entity.id == base.id
            || entity.vehicle.is_some()
            || swims(entity.kind)
            || !physics::intersects(area, entity.bounds)
            || !boards(entity.kind, entity.bounds)
        {
            continue;
        }
        cx.effects.push(Effect::Mount {
            passenger: entity.id,
            vehicle: base.id,
        });
        mounted.push(entity.id);
        free -= 1;
    }
    mounted
}
