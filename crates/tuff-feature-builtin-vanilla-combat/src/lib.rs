#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "test code is exempt from the door gate"
    )
)]

pub mod projectile;
pub mod world;

/// Registers vanilla primed TNT and projectiles into core.
pub struct CombatFeature;

impl tuff_core::feature::Feature for CombatFeature {
    fn id(&self) -> &'static str {
        "vanilla-combat"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let entities = &mut cx.registries().entity_types;
            entities.register(
                tuff_vanilla_data::registry::EntityType::Tnt,
                crate::world::explosion::load_tnt,
            );
            crate::projectile::register(entities);
            let id = cx.feature_id();
            cx.registries()
                .tnt
                .fill(id, std::sync::Arc::new(crate::world::explosion::VanillaTnt))?;
            cx.registries().explosion_rules.fill(
                id,
                std::sync::Arc::new(crate::world::explosion_rules::VanillaExplosionRules),
            )?;
            let hooks = std::sync::Arc::new(crate::world::session::attack::CombatHooks);
            cx.registries().player_damage.fill(id, hooks.clone())?;
            cx.registries().session_hooks.push(hooks);
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::melee::CombatRegionHooks,
            ));
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::ranged::RangedRegionHooks,
            ));
            cx.registries().region_hooks.push(std::sync::Arc::new(
                crate::world::region::pearls::PearlHooks,
            ));
            cx.registries().session_hooks.push(std::sync::Arc::new(
                crate::world::region::pearls::PearlSessionHooks,
            ));
        }
        Ok(())
    }
}
