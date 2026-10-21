pub mod block_rules;
pub mod falling;
pub mod growth;
pub mod world;

/// Registers vanilla block entities (falling blocks) into core.
pub struct BlocksFeature;

impl tuff_core::feature::Feature for BlocksFeature {
    fn id(&self) -> &'static str {
        "vanilla-blocks"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let entities = &mut cx.registries().entity_types;
            entities.register(
                tuff_vanilla_data::registry::EntityType::FallingBlock,
                crate::falling::load,
            );
            let id = cx.feature_id();
            cx.registries().block_rules.fill(
                id,
                std::sync::Arc::new(crate::block_rules::VanillaBlockRules),
            )?;
            cx.registries().growth_behaviors.fill(
                id,
                std::sync::Arc::new(crate::growth::Behaviours::builtin()),
            )?;
            cx.registries()
                .region_hooks
                .push(std::sync::Arc::new(crate::world::region::trample::Trample));
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::ticking::TickingBlocks,
            ));
            cx.registries()
                .region_hooks
                .push(std::sync::Arc::new(crate::world::region::lectern::Lecterns));
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::impacts::ProjectileImpacts,
            ));
            cx.registries().item_ports.register(
                id,
                tuff_vanilla_data::Block::Jukebox,
                std::sync::Arc::new(crate::world::jukebox::JukeboxPort),
            )?;
            cx.registries().item_ports.register(
                id,
                tuff_vanilla_data::Block::Composter,
                std::sync::Arc::new(crate::world::composter::ComposterPort),
            )?;
            cx.registries().item_ports.register(
                id,
                tuff_vanilla_data::Block::DecoratedPot,
                std::sync::Arc::new(crate::world::composter::PotPort),
            )?;
            cx.registries()
                .fluid_behaviors
                .fill(id, std::sync::Arc::new(crate::world::fluid::VanillaFluids))?;
        }
        Ok(())
    }
}
