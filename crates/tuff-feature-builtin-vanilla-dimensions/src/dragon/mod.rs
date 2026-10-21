//! The end's boss fight: end crystals, the ender dragon with its phases
//! and breath attacks, and the fight actor that spawns the dragon, keeps
//! its boss bar and builds the exit portal once it dies.

pub mod bottle;
pub mod crystal;
pub mod entity;
pub mod fight;
pub mod fireball;
pub mod phase;

use tuff_vanilla_data::registry::EntityType;

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::EndCrystal, crystal::load);
    registry.register(EntityType::EnderDragon, entity::load);
    registry.register(EntityType::DragonFireball, fireball::load_fireball);
    registry.register(
        EntityType::AreaEffectCloud,
        tuff_vanilla_common::effect_cloud::load_cloud,
    );
}
