#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "test code is exempt from the door gate"
    )
)]

pub mod world;

/// Registers vanilla non-mob entities (lightning, vehicles, ...) into core.
pub struct EntitiesFeature;

impl tuff_core::feature::Feature for EntitiesFeature {
    fn id(&self) -> &'static str {
        "vanilla-entities"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let entities = &mut cx.registries().entity_types;
            use tuff_vanilla_data::registry::EntityType;
            entities.register(EntityType::LightningBolt, crate::world::lightning::load);
            entities.register(
                EntityType::OminousItemSpawner,
                crate::world::ominous_item_spawner::load,
            );
            crate::world::boat::register(entities);
            crate::world::item_frame::register(entities);
            crate::world::painting::register(entities);
            crate::world::cushion::register(entities);
            crate::world::interaction::register(entities);
            crate::world::display::register(entities);
            crate::world::minecart::register(entities);
            cx.registries()
                .config_keys
                .extend(crate::world::item_frame::config_keys());
            cx.registries().session_hooks.push(std::sync::Arc::new(
                crate::world::minecart::CommandMinecartHooks,
            ));
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::lightning::Thunder,
            ));
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::vehicles::VehicleHooks,
            ));
            cx.registries()
                .region_hooks
                .push(std::sync::Arc::new(crate::world::region::TrialHooks));
            cx.registries()
                .region_hooks
                .push(std::sync::Arc::new(crate::world::region::decor::DecorHooks));
        }
        Ok(())
    }
}
