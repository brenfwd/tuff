#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "test code is exempt from the door gate"
    )
)]

use tuff_core::world::block_entity::ContainerKind;

pub mod crafting;
pub mod world;

/// Registers the vanilla recipes into core.
pub struct RecipesFeature;

impl tuff_core::feature::Feature for RecipesFeature {
    fn id(&self) -> &'static str {
        "vanilla-recipes"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let id = cx.feature_id();
            crafting::registered::register(&mut cx.registries().recipe_entries, id)?;
            let tickers = &mut cx.registries().block_entity_tickers;
            for kind in [
                ContainerKind::Furnace,
                ContainerKind::BlastFurnace,
                ContainerKind::Smoker,
            ] {
                tickers.register(
                    id,
                    kind,
                    std::sync::Arc::new(world::smelting::FurnaceTicker),
                )?;
            }
            tickers.register(
                id,
                ContainerKind::Campfire,
                std::sync::Arc::new(world::smelting::CampfireTicker),
            )?;
            tickers.register(
                id,
                ContainerKind::BrewingStand,
                std::sync::Arc::new(world::brewing::BrewingTicker),
            )?;
            cx.registries()
                .data_recipes
                .fill(id, std::sync::Arc::new(crafting::registered::PackRecipes))?;
        }
        Ok(())
    }
}
