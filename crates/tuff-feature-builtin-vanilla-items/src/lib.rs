#![recursion_limit = "512"]
pub use tuff_item::enchantment;
pub mod world;

/// Registers vanilla dropped items into core.
pub struct ItemsFeature;

impl tuff_core::feature::Feature for ItemsFeature {
    fn id(&self) -> &'static str {
        "vanilla-items"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let entities = &mut cx.registries().entity_types;
            entities.register(
                tuff_vanilla_data::registry::EntityType::Item,
                crate::world::items::load,
            );
            let id = cx.feature_id();
            cx.registries().station_rules.fill(
                id,
                std::sync::Arc::new(crate::world::stations::VanillaStations),
            )?;
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::interact::InteractionHooks,
            ));
            cx.registries().session_hooks.push(std::sync::Arc::new(
                crate::world::session::item_use::ItemUseHooks,
            ));
        }
        Ok(())
    }
}
