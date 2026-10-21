//! Vanilla world generation, reproduced bit for bit: random sources, noise,
//! density functions, biomes, aquifers, surface rules, carvers, features
//! and structures.

pub mod aquifer;
pub mod beardifier;
pub mod biome_source;
pub mod biome_zoom;
pub mod blended;
pub mod cache;
pub mod carver;
pub mod chunk_generator;
pub mod climate;
pub mod data;
pub mod density;
pub mod feature;
pub mod flat;
pub mod generator;
pub mod java_set;
pub mod json;
pub mod locate;
pub mod noise;
pub mod normal_noise;
pub mod pipeline;
pub mod place;
pub mod proto;
pub mod providers;
pub mod saved;
pub mod spawn;
pub use tuff_vanilla_common::spawn_search;
pub mod loaded;
pub mod state;
pub mod structure;
pub mod surface;
pub mod tags;
pub mod temperature;

#[cfg(test)]
mod tests;

/// Registers the vanilla world generators into core.
pub struct WorldgenFeature;

impl tuff_core::feature::Feature for WorldgenFeature {
    fn id(&self) -> &'static str {
        "vanilla-worldgen"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            tuff_item::registries::add_validator("vanilla-worldgen", loaded::validate);
            let id = cx.feature_id();
            cx.registries()
                .generators
                .fill(id, std::sync::Arc::new(chunk_generator::VanillaGenerators))?;
            cx.registries()
                .services
                .push(std::sync::Arc::new(feature::host::FeatureServiceFactory));
        }
        Ok(())
    }
}
