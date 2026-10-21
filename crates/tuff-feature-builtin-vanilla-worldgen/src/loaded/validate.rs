use super::*;

/// Decodes every pack entry of the worldgen registries this module reads,
/// so a bad one rejects the data pack load instead of failing at bind.
pub fn validate(
    registries: &tuff_item::registries::Registries,
) -> std::result::Result<(), tuff_item::registries::RegistryError> {
    let error =
        |registry, id: &str, pack: Option<&str>, message| tuff_item::registries::RegistryError {
            pack: pack.unwrap_or("vanilla").to_owned(),
            registry,
            id: id.to_owned(),
            message,
        };
    validate_features(registries)
        .map_err(|(registry, id, pack, message)| error(registry, &id, pack.as_deref(), message))?;
    biomes::PackBiomes::from_registries(registries)
        .map_err(|(registry, id, pack, message)| error(registry, &id, pack.as_deref(), message))?;
    let structures = structures::StructureSources::from_registries(registries)
        .map_err(|(registry, id, pack, message)| error(registry, &id, pack.as_deref(), message))?;
    structures::check(&Arc::new(structures)).map_err(|(registry, id, message)| {
        let pack = registries
            .get(registry)
            .and_then(|r| r.entry(&id).and_then(|e| e.value().pack.clone()));
        error(registry, &id, pack.as_deref(), message)
    })?;
    let sources = sources(registries);
    if sources.is_empty() {
        return Ok(());
    }
    let mut linker = Linker::new(sources);
    for key in REGISTRIES {
        let Some(registry) = registries.get(key) else {
            continue;
        };
        for (id, entry) in registry.iter() {
            let raw = entry.value();
            if raw.json.is_none() {
                continue;
            }
            let result = match key {
                NOISE => linker.noise(id).map(drop),
                DENSITY_FUNCTION => linker.function(id).map(drop),
                MATERIAL_RULE => linker.rule(id).map(drop),
                MATERIAL_CONDITION => linker.condition(id).map(drop),
                _ => linker.settings(id).map(drop),
            };
            result.map_err(|message| error(key, id, raw.pack.as_deref(), message))?;
        }
    }
    Ok(())
}

/// Parses every pack configured and placed feature and every placed
/// feature a pack biome names.
pub(super) fn validate_features(
    registries: &tuff_item::registries::Registries,
) -> std::result::Result<(), features::FeatureError> {
    let sources = Arc::new(features::FeatureSources::from_registries(registries)?);
    if sources.is_empty() {
        return Ok(());
    }
    let pack_of = |registry: &str, id: &str| {
        registries
            .get(registry)
            .and_then(|r| r.iter().find(|(i, _)| qualify(i) == id))
            .and_then(|(_, e)| e.value().pack.clone())
    };
    let mut registry = crate::feature::Registry::with_sources(Some(sources.clone()));
    let mut ids: Vec<_> = sources.configured.keys().cloned().collect();
    ids.sort();
    for id in ids {
        registry.feature(&Value::String(id.clone())).map_err(|e| {
            (
                features::CONFIGURED_FEATURE,
                id.clone(),
                pack_of(features::CONFIGURED_FEATURE, &id),
                e,
            )
        })?;
    }
    let mut ids: Vec<_> = sources.placed.keys().cloned().collect();
    ids.sort();
    for id in ids {
        registry.placed(&Value::String(id.clone())).map_err(|e| {
            (
                features::PLACED_FEATURE,
                id.clone(),
                pack_of(features::PLACED_FEATURE, &id),
                e,
            )
        })?;
    }
    let mut biomes: Vec<_> = sources.biomes.iter().collect();
    biomes.sort_by_key(|(biome, _)| **biome);
    for (&biome, steps) in biomes {
        let name = tuff_world_model::chunk::biome_name(biome);
        for id in steps.iter().flatten().filter(|id| !id.starts_with('#')) {
            registry.placed(&Value::String(id.clone())).map_err(|e| {
                (
                    features::BIOME,
                    name.to_owned(),
                    pack_of(features::BIOME, name),
                    e,
                )
            })?;
        }
    }
    if let Some((id, message)) = registry.errors.first() {
        return Err((
            features::CONFIGURED_FEATURE,
            id.clone(),
            None,
            message.clone(),
        ));
    }
    Ok(())
}
