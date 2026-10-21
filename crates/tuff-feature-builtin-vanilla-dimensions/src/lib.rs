pub mod dragon;
pub mod world;

/// Registers the ender dragon into core.
pub struct DimensionsFeature;

impl tuff_core::feature::Feature for DimensionsFeature {
    fn id(&self) -> &'static str {
        "vanilla-dimensions"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let entities = &mut cx.registries().entity_types;
            crate::dragon::register(entities);
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::gateways::GatewayHooks,
            ));
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::portals::PortalBlockHooks,
            ));
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::entity_portals::EntityPortalHooks,
            ));
            cx.registries()
                .services
                .push(std::sync::Arc::new(crate::dragon::fight::FightsFactory));
            cx.registries().session_hooks.push(std::sync::Arc::new(
                crate::world::session::portals::PortalHooks,
            ));
        }
        Ok(())
    }
}
