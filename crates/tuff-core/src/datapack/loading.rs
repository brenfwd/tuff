use super::*;

pub(super) fn read_pack(
    backend: &dyn WorldBackend,
    pack: &PackInfo,
    hiders: &Hiders<'_>,
    budget: &mut usize,
) -> io::Result<PackData> {
    match &pack.location {
        Location::BuiltIn => Ok(PackData::default()),
        Location::Feature(feature) => {
            collect(&pack.id, &FeatureFiles(feature), &pack.meta, hiders, budget)
        }
        Location::Directory(prefix) => {
            let files = directory_files(backend, prefix)?;
            collect(&pack.id, &files, &pack.meta, hiders, budget)
        }
        Location::Zip(path) => {
            let bytes = read_archive(backend, path)?;
            let archive = zip::Archive::open(&bytes, MAX_ENTRIES)?;
            collect(&pack.id, &archive, &pack.meta, hiders, budget)
        }
    }
}

/// A pack whose files could not be read.
#[derive(Debug)]
pub struct LoadError {
    pub pack: String,
    pub error: io::Error,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pack {}: {}", self.pack, self.error)
    }
}

pub(super) fn merge_tag(
    builders: &mut BTreeMap<String, Vec<tags::TagEntry>>,
    tag: String,
    bytes: &[u8],
    pack: &str,
) {
    match tags::parse(bytes) {
        Ok((replace, entries)) => {
            let list = builders.entry(tag).or_default();
            if replace {
                list.clear();
            }
            list.extend(entries);
        }
        Err(err) => log::warn!("Couldn't read tag list {tag} from data pack {pack}: {err}"),
    }
}

/// Loads the contents of `selected` (lowest priority first). The data-driven
/// registries are read from the packs unless fixed `registries` are given
/// (they are only read at startup). A pack that cannot be read, or whose
/// registry entries do not validate, fails the whole load; a bad function,
/// tag or recipe is logged and left out, as the game does.
pub fn load(
    backend: &dyn WorldBackend,
    roots: &Roots,
    base: &Base,
    packs: &[PackInfo],
    selected: &[String],
    generation: u64,
    registries: Option<Arc<tuff_item::registries::Registries>>,
) -> Result<Contents, LoadError> {
    crate::world::predicate::install_item_conditions();
    let mut registry_files = tuff_item::registries::Loader::default();
    let mut budget = MAX_TOTAL;
    let mut sources: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut tag_builders: BTreeMap<String, Vec<tags::TagEntry>> = BTreeMap::new();
    let mut registry_tags: Option<BTreeMap<&'static str, tags::Builders>> = None;
    let mut recipe_files: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
    let mut loot_files: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
    let mut predicate_files: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
    let mut item_modifier_files: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
    let mut slot_source_files: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
    let mut advancement_files: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
    let mut structure_files: BTreeMap<String, Arc<[u8]>> = BTreeMap::new();
    let mut test_instance_files: BTreeMap<String, Arc<[u8]>> = BTreeMap::new();
    let mut test_environment_files: BTreeMap<String, Arc<[u8]>> = BTreeMap::new();
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    let chosen: Vec<&PackInfo> = selected
        .iter()
        .filter_map(|id| packs.iter().find(|p| &p.id == id))
        .collect();
    let mut builtin = HiddenBuiltIn::default();
    for (index, pack) in chosen.iter().enumerate() {
        let hiders: Vec<&[meta::FilterPattern]> = chosen[index + 1..]
            .iter()
            .map(|above| above.meta.filter.as_slice())
            .filter(|filter| !filter.is_empty())
            .collect();
        if pack.id == VANILLA {
            let mut vanilla_tags = tags::vanilla_builders();
            builtin.hiders = hiders.clone();
            if builtin.strip_tags(&mut vanilla_tags) {
                registry_tags = Some(vanilla_tags);
            }
        }
        if pack.meta.compatibility() != Compatibility::Compatible && !pack.required() {
            log::warn!(
                "Pack {} was made for a different version ({:?}); loading it anyway",
                pack.id,
                pack.meta.compatibility()
            );
        }
        let data = read_pack(backend, pack, &hiders, &mut budget).map_err(|error| LoadError {
            pack: pack.id.clone(),
            error,
        })?;
        for (function, bytes) in data.functions {
            sources.insert(function, bytes);
        }
        for (tag, bytes) in data.function_tags {
            merge_tag(&mut tag_builders, tag, &bytes, &pack.id);
        }
        for (registry, tag, bytes) in data.tags {
            let builders = registry_tags.get_or_insert_with(tags::vanilla_builders);
            merge_tag(builders.entry(registry).or_default(), tag, &bytes, &pack.id);
        }
        for (recipe, bytes) in data.recipes {
            recipe_files.insert(recipe, (pack.id.clone(), bytes));
        }
        for (table, bytes) in data.loot_tables {
            loot_files.insert(table, (pack.id.clone(), bytes));
        }
        for (predicate, bytes) in data.predicates {
            predicate_files.insert(predicate, (pack.id.clone(), bytes));
        }
        for (modifier, bytes) in data.item_modifiers {
            item_modifier_files.insert(modifier, (pack.id.clone(), bytes));
        }
        for (source, bytes) in data.slot_sources {
            slot_source_files.insert(source, (pack.id.clone(), bytes));
        }
        for (advancement, bytes) in data.advancements {
            advancement_files.insert(advancement, (pack.id.clone(), bytes));
        }
        for (template, bytes) in data.structures {
            structure_files.insert(template, bytes.into());
        }
        for (test, bytes) in data.test_instances {
            test_instance_files.insert(test, bytes.into());
        }
        for (environment, bytes) in data.test_environments {
            test_environment_files.insert(environment, bytes.into());
        }
        for (kind, count) in data.unsupported {
            *unsupported.entry(kind).or_default() += count;
        }
        for (registry, entry, bytes) in data.registries {
            registry_files.insert(registry, entry, &pack.id, bytes);
        }
    }
    let registries = match registries {
        Some(fixed) => fixed,
        None => {
            for registry in tuff_vanilla_data::data_registry::ALL {
                let directory = registry
                    .key
                    .strip_prefix("minecraft:")
                    .unwrap_or(registry.key);
                let kind = format!("{directory}/");
                for id in registry.entries {
                    if builtin.hides(&kind, id, ".json") {
                        registry_files.hide(registry, id);
                    }
                }
            }
            registry_files.freeze()?
        }
    };
    let enchantments = tuff_item::enchantment::from_registries(&registries)?;
    let compiled: Vec<(String, Result<Function, String>)> = sources
        .into_par_iter()
        .map(|(id, bytes)| {
            let result = String::from_utf8(bytes)
                .map_err(|_| "file is not UTF-8".to_owned())
                .and_then(|text| Function::compile_at(&id, &text, roots, base.function_permission));
            (id, result)
        })
        .collect();
    let mut functions = BTreeMap::new();
    for (id, result) in compiled {
        match result {
            Ok(function) => {
                functions.insert(id, Arc::new(function));
            }
            Err(err) => log::warn!("Failed to load function {id}: {err}"),
        }
    }
    let (function_tags, errors) = tags::resolve(&tag_builders, |id| functions.contains_key(id));
    for error in errors {
        log::warn!("{error}");
    }
    let tags_changed = registry_tags.is_some();
    let (tags, tags_packet) = match registry_tags {
        Some(builders) => {
            let (set, errors) =
                tags::TagSet::resolve_all_with(&builders, &|key, id| registries.exists(key, id));
            for error in errors {
                log::warn!("{error}");
            }
            let packet = tags_network_body(&set, &registries);
            (Arc::new(set), packet)
        }
        None if registries.modified() => {
            let set = tags::TagSet::vanilla();
            let packet = tags_network_body(&set, &registries);
            (set, packet)
        }
        None => (tags::TagSet::vanilla(), vanilla_tags_packet()),
    };
    let hidden_recipes: Vec<String> = base
        .recipes
        .entries()
        .map(|(name, _)| name.to_string())
        .filter(|name| builtin.hides(RECIPES, name, ".json"))
        .collect();
    let recipes = (tags_changed || !recipe_files.is_empty() || !hidden_recipes.is_empty())
        .then(|| apply_recipes(base, &tags, tags_changed, &hidden_recipes, recipe_files));
    let predicates = if predicate_files.is_empty() {
        loot::Predicates::vanilla()
    } else {
        Arc::new(loot::Predicates::new(predicate_files))
    };
    let hidden_tables: Vec<&str> = tuff_vanilla_data::loot_table::ALL
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| builtin.hides(LOOT_TABLES, name, ".json"))
        .collect();
    let slot_sources = Arc::new(tuff_datapack::slot_source::SlotSources::new(
        slot_source_files,
        &|name| crate::world::predicate::slot_range(name).is_some(),
    ));
    let loot_tables =
        if loot_files.is_empty() && hidden_tables.is_empty() && slot_sources.ids().next().is_none()
        {
            loot::LootTables::vanilla()
        } else {
            Arc::new(
                load_loot_tables(&tags, &predicates, &hidden_tables, loot_files)
                    .with_slot_sources(slot_sources.clone()),
            )
        };
    let block_loot = match &base.block_loot {
        Some(rules) if !loot_tables.is_vanilla() => rules.with_tables(loot_tables.clone()),
        _ => None,
    };
    let hidden_advancements: Vec<String> = if builtin.hiders.is_empty() {
        Vec::new()
    } else {
        base_advancements(base)
            .names()
            .filter(|name| builtin.hides(ADVANCEMENTS, name, ".json"))
            .map(str::to_owned)
            .collect()
    };
    let advancements = (tags_changed
        || !advancement_files.is_empty()
        || !hidden_advancements.is_empty())
    .then(|| {
        load_advancements(
            base,
            &predicates,
            &tags,
            hidden_advancements,
            advancement_files,
        )
    });
    if !unsupported.is_empty() {
        let kinds: Vec<String> = unsupported
            .iter()
            .map(|(kind, count)| format!("{kind} ({count})"))
            .collect();
        log::warn!(
            "Data pack content not supported yet, ignored: {}",
            kinds.join(", ")
        );
    }
    Ok(Contents {
        generation,
        functions,
        function_tags,
        tags,
        tags_packet,
        recipes,
        loot_tables,
        block_loot,
        item_modifiers: Arc::new(loot::ItemModifiers::new(item_modifier_files, &predicates)),
        predicates,
        slot_sources,
        advancements,
        structures: Arc::new(structure_files),
        test_instances: Arc::new(test_instance_files),
        test_environments: Arc::new(test_environment_files),
        registries,
        enchantments,
    })
}

/// The Update Tags body with data registry entries numbered as clients
/// were sent them.
pub(super) fn tags_network_body(
    set: &tags::TagSet,
    registries: &tuff_item::registries::Registries,
) -> Bytes {
    if !registries.modified() {
        return set.network_body();
    }
    set.network_body_with(&|key, id| registries.get(key)?.index_of(id))
}

/// The features' advancement tree with every advancement a pack defines
/// replaced by the pack's; a file that does not parse removes the
/// advancement, as the highest pack's file is the only one read. Item
/// criteria are indexed by `tags`, so this also runs when only tags changed.
pub(super) fn load_advancements(
    base: &Base,
    predicates: &loot::Predicates,
    tags: &tags::TagSet,
    hidden: Vec<String>,
    files: BTreeMap<String, (String, Vec<u8>)>,
) -> Arc<dyn Advancements> {
    let files = files
        .into_iter()
        .map(|(id, (origin, json))| OverlayFile { id, origin, json })
        .collect();
    base_advancements(base)
        .with_overlay(Overlay {
            removed: hidden,
            files,
            unparsed_removes: true,
            predicates,
            tags,
        })
        .0
}

/// The features' advancements, or the built-in tree when there are none.
pub(super) fn base_advancements(base: &Base) -> Arc<dyn Advancements> {
    base.advancements
        .clone()
        .unwrap_or_else(|| Arc::new(crate::world::advancement::NoAdvancements))
}

/// Parses the loot tables packs define; a file that does not parse leaves
/// its table empty, as the highest pack's file is the only one read.
pub(super) fn load_loot_tables(
    tags: &tags::TagSet,
    predicates: &loot::Predicates,
    hidden: &[&str],
    files: BTreeMap<String, (String, Vec<u8>)>,
) -> loot::LootTables {
    let mut overrides: std::collections::HashMap<String, Option<_>> = files
        .into_par_iter()
        .map(|(id, (pack, bytes))| {
            let table = loot::parse(&bytes, tags, predicates)
                .inspect_err(|err| log::warn!("Couldn't parse data file {id} from {pack}: {err}"))
                .ok();
            (id, table)
        })
        .collect();
    for name in hidden {
        overrides.entry((*name).to_owned()).or_insert(None);
    }
    loot::LootTables::new(overrides)
}

/// The features' recipes with every recipe a pack defines replaced by the
/// pack's; a file that does not parse removes the recipe, as the
/// highest pack's file is the only one read. When packs changed tags,
/// built-in recipes naming a tag whose members changed are rebuilt with
/// the new members.
pub(super) fn apply_recipes(
    base: &Base,
    tags: &tags::TagSet,
    tags_changed: bool,
    hidden: &[String],
    files: BTreeMap<String, (String, Vec<u8>)>,
) -> Arc<RecipeSet> {
    let Some(factory) = &base.data_recipes else {
        log::warn!("No feature turns data pack recipes into recipes; ignoring them");
        return base.recipes.clone();
    };
    let parsed: Vec<(
        String,
        String,
        Result<tuff_vanilla_data::recipe::RecipeEntry, String>,
    )> = files
        .into_par_iter()
        .map(|(id, (pack, bytes))| {
            let result = recipe::parse(&id, &bytes, tags);
            (id, pack, result)
        })
        .collect();
    let mut recipes: BTreeMap<
        tuff_vanilla_data::Frozen<str>,
        Arc<dyn crate::world::crafting::Recipe>,
    > = base
        .recipes
        .entries()
        .map(|(name, recipe)| (name.clone(), recipe.clone()))
        .collect();
    for name in hidden {
        recipes.remove(name.as_str());
    }
    if tags_changed {
        for (name, entry) in tuff_vanilla_data::recipe::ALL {
            if !recipes.contains_key(*name) {
                continue;
            }
            if let Some(entry) = retagged(entry, tags) {
                recipes.insert(entry.name.clone(), factory.recipe(&entry));
            }
        }
    }
    for (id, pack, result) in parsed {
        recipes.remove(id.as_str());
        match result {
            Ok(entry) => {
                let recipe = factory.recipe(&entry);
                recipes.insert(entry.name, recipe);
            }
            Err(err) => log::warn!("Couldn't parse data file {id} from {pack}: {err}"),
        }
    }
    Arc::new(RecipeSet::new(recipes).with_custom(base.recipes.custom().clone()))
}

/// `entry` with its tag ingredients read from `tags`, if any member
/// changed. A tag that no longer resolves leaves the ingredient empty.
pub(super) fn retagged(
    entry: &tuff_vanilla_data::recipe::RecipeEntry,
    tags: &tags::TagSet,
) -> Option<tuff_vanilla_data::recipe::RecipeEntry> {
    use tuff_vanilla_data::recipe::Ingredient;
    let mut changed = false;
    let mut entry = entry.clone();
    entry.recipe.ingredients_mut(&mut |ingredient| {
        if let Ingredient::Tag(tag, items) = ingredient {
            let members = recipe::tag_members(tags, tag).unwrap_or_default();
            if *members != **items {
                *items = members.into();
                changed = true;
            }
        }
    });
    changed.then_some(entry)
}

impl From<tuff_item::registries::RegistryError> for LoadError {
    fn from(err: tuff_item::registries::RegistryError) -> Self {
        Self {
            error: std::io::Error::other(err.to_string()),
            pack: err.pack,
        }
    }
}
