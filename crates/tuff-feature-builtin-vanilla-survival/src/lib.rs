pub mod advancement;
pub mod world;

/// Registers vanilla experience orbs and the vanilla advancement tree and triggers into core.
pub struct SurvivalFeature;

impl tuff_core::feature::Feature for SurvivalFeature {
    fn id(&self) -> &'static str {
        "vanilla-survival"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() != tuff_item::load::Phase::Content {
            return Ok(());
        }
        let id = cx.feature_id();
        let registries = cx.registries();
        registries.advancement_packs.register(
            id,
            "vanilla",
            &tuff_vanilla_data::advancement::ALL[..],
        )?;
        for kind in tuff_vanilla_data::registry::TriggerType::ALL {
            registries.advancement_triggers.register(
                id,
                kind.name(),
                crate::advancement::trigger::builtin_handler(kind),
            )?;
        }
        registries.entity_types.register(
            tuff_vanilla_data::registry::EntityType::ExperienceOrb,
            crate::world::experience_orb::load,
        );
        Ok(())
    }
}
