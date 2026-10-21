//! Custom content providers add at every world start, beside what features
//! registered while loading: items, entities, asset sets,
//! and recipes and advancements as data pack JSON. Providers are asked
//! once, before the world's shared state exists, and what they give is
//! merged into the immutable [`CustomContent`], the recipe set and the
//! advancement tree the world starts with; nothing changes until the next
//! start. A provider whose content clashes with what is already there is
//! left out whole and logged, so one bad feature never stops the server.
//!
//! A shaped or shapeless crafting recipe may name custom items: an
//! ingredient string (in `key` or `ingredients`) or a result `id` that is
//! a custom item id matches or makes that custom item. Every other recipe
//! goes through the data pack recipe parser and takes vanilla items only.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value};
use tuff_vanilla_data::registry::{EntityType, RecipeBookCategory};
use tuff_vanilla_data::{Frozen, Item};

use super::recipes::{CustomIngredient, CustomShaped, CustomShapeless};
use super::{AssetSet, CustomContent, CustomEntityDef, CustomItemDef, Definitions};
use crate::world::advancement::{Advancements, Overlay, OverlayFile};
use crate::world::crafting::{self, DataRecipes, Recipe, RecipeSet};
use crate::world::custom_dimension::ProviderContext;
use tuff_datapack::tags::TagSet;
use tuff_item::load::LoadError;

/// Adds custom content to every world. Asked once per world start.
pub trait ContentProvider: Send + Sync {
    /// Called once per world start, before [`Self::content`], with what
    /// the server resolved at startup.
    fn attach(&self, _ctx: &ProviderContext) {}

    /// The content this provider adds, one entry per owner.
    fn content(&self) -> Vec<ProvidedContent>;
}

/// A data pack file: its id and JSON text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataFile {
    pub id: String,
    pub json: String,
}

/// What one owner (a feature) adds.
#[derive(Debug, Clone, Default)]
pub struct ProvidedContent {
    /// Who adds it; named in conflicts and as the owner of each entry.
    pub owner: String,
    pub items: Vec<CustomItemDef>,
    pub entities: Vec<CustomEntityDef>,
    /// Asset sets by a name the owner picks.
    pub assets: Vec<(String, AssetSet)>,
    /// Recipes as data pack JSON, by recipe id.
    pub recipes: Vec<DataFile>,
    /// Advancements as data pack JSON, by advancement id.
    pub advancements: Vec<DataFile>,
}

impl ProvidedContent {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
            && self.entities.is_empty()
            && self.assets.is_empty()
            && self.recipes.is_empty()
            && self.advancements.is_empty()
    }
}

/// Attaches every provider and collects what they add.
pub fn collect(
    providers: &[Arc<dyn ContentProvider>],
    ctx: &ProviderContext,
) -> Vec<ProvidedContent> {
    providers
        .iter()
        .flat_map(|provider| {
            provider.attach(ctx);
            provider.content()
        })
        .filter(|content| !content.is_empty())
        .collect()
}

/// `name` with a static lifetime, leaked once per distinct name, for the
/// registries' owner fields.
fn owner_name(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(name) = names.get(name) {
        return name;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

fn leak(text: &str) -> &'static str {
    owner_name(text)
}

impl Definitions {
    /// A fresh, unresolved copy of every registered definition.
    fn copy(&self) -> Result<Definitions, LoadError> {
        let mut copy = Definitions::default();
        for (id, def) in self.items.iter() {
            let owner = self.items.registered_by(id).unwrap_or("?");
            copy.items.register(owner, id, (**def).clone())?;
        }
        for (id, def) in self.entities.iter() {
            let owner = self.entities.registered_by(id).unwrap_or("?");
            copy.entities.register(owner, id, (**def).clone())?;
        }
        for (key, set) in self.assets.iter() {
            let owner = self.assets.registered_by(key).unwrap_or("?");
            copy.assets.register(owner, key, (**set).clone())?;
        }
        Ok(copy)
    }

    fn add(&mut self, content: &ProvidedContent) -> Result<(), LoadError> {
        let owner = owner_name(&content.owner);
        for def in &content.items {
            self.register_item(owner, def.clone())?;
        }
        for def in &content.entities {
            self.register_entity(owner, def.clone())?;
        }
        for (key, set) in &content.assets {
            self.register_assets(owner, key.clone(), set.clone())?;
        }
        Ok(())
    }

    /// The resolved content with `provided` added, owner by owner; an
    /// owner whose definitions do not validate or clash is left out and
    /// logged. Returns the content and the owners that went in.
    pub fn with_provided(
        &self,
        provided: Vec<ProvidedContent>,
    ) -> (Arc<CustomContent>, Vec<ProvidedContent>) {
        if provided.is_empty() {
            return (self.content(), provided);
        }
        let mut current = match self.copy() {
            Ok(copy) => copy,
            Err(err) => {
                log::error!("custom content could not be copied: {err}");
                return (self.content(), Vec::new());
            }
        };
        let mut accepted = Vec::new();
        for content in provided {
            let trial = current.copy().and_then(|mut trial| {
                trial.add(&content)?;
                CustomContent::build(&trial)?;
                Ok(trial)
            });
            match trial {
                Ok(trial) => {
                    current = trial;
                    accepted.push(content);
                }
                Err(err) => log::error!("custom content of {} left out: {err}", content.owner),
            }
        }
        match CustomContent::build(&current) {
            Ok(content) => (Arc::new(content), accepted),
            Err(err) => {
                log::error!("custom content left out: {err}");
                (self.content(), Vec::new())
            }
        }
    }
}

/// What provided content adds to the world's startup state.
pub struct Merged {
    pub custom: Arc<CustomContent>,
    pub recipes: Arc<RecipeSet>,
    pub advancements: Arc<dyn Advancements>,
    /// The owners whose content went in.
    pub accepted: Vec<ProvidedContent>,
    /// The ids of the recipes and advancements that parsed.
    pub recipe_ids: Vec<String>,
    pub advancement_ids: Vec<String>,
}

/// Merges `provided` into the features' content, recipes and advancements.
/// A recipe or advancement that does not parse is logged and left out;
/// one with the id of an existing one replaces it.
pub fn merge(
    definitions: &Definitions,
    recipes: &Arc<RecipeSet>,
    advancements: &Arc<dyn Advancements>,
    data_recipes: Option<&Arc<dyn DataRecipes>>,
    provided: Vec<ProvidedContent>,
) -> Merged {
    let (custom, accepted) = definitions.with_provided(provided);
    let tags = TagSet::vanilla();
    let mut added: BTreeMap<Frozen<str>, Arc<dyn Recipe>> = BTreeMap::new();
    let mut files = Vec::new();
    for content in &accepted {
        for file in &content.recipes {
            match recipe(file, &custom, &tags, data_recipes) {
                Ok(recipe) => {
                    added.insert(Frozen::from(file.id.clone()), recipe);
                }
                Err(err) => log::error!("recipe {} of {} left out: {err}", file.id, content.owner),
            }
        }
        files.extend(content.advancements.iter().map(|file| OverlayFile {
            id: file.id.clone(),
            origin: content.owner.clone(),
            json: file.json.clone().into_bytes(),
        }));
    }
    let names: Vec<Frozen<str>> = added.keys().cloned().collect();
    let recipes = if added.is_empty() && Arc::ptr_eq(recipes.custom(), &custom) {
        recipes.clone()
    } else {
        let mut entries: BTreeMap<Frozen<str>, Arc<dyn Recipe>> = recipes
            .entries()
            .map(|(name, recipe)| (name.clone(), recipe.clone()))
            .collect();
        entries.extend(added);
        Arc::new(RecipeSet::new(entries).with_custom(custom.clone()))
    };
    let (advancements, advancement_ids) = if files.is_empty() {
        (advancements.clone(), Vec::new())
    } else {
        advancements.with_overlay(Overlay {
            removed: Vec::new(),
            files,
            unparsed_removes: false,
            predicates: &tuff_datapack::loot::Predicates::vanilla(),
            tags: &tags,
        })
    };
    Merged {
        custom,
        recipe_ids: names.into_iter().map(|name| name.to_string()).collect(),
        advancement_ids,
        recipes,
        advancements,
        accepted,
    }
}

type Object = Map<String, Value>;

fn recipe(
    file: &DataFile,
    custom: &CustomContent,
    tags: &TagSet,
    data_recipes: Option<&Arc<dyn DataRecipes>>,
) -> Result<Arc<dyn Recipe>, String> {
    let value: Value = serde_json::from_str(&file.json).map_err(|err| err.to_string())?;
    let object = value.as_object().ok_or("not an object")?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .map(|kind| kind.strip_prefix("minecraft:").unwrap_or(kind));
    if matches!(kind, Some("crafting_shaped" | "crafting_shapeless"))
        && names_custom(object, custom)
    {
        let name = leak(&file.id);
        return match kind {
            Some("crafting_shaped") => shaped(name, object, custom, tags),
            _ => shapeless(name, object, custom, tags),
        };
    }
    let factory = data_recipes.ok_or("no feature turns data pack recipes into recipes")?;
    let entry = tuff_datapack::recipe::parse(&file.id, file.json.as_bytes(), tags)?;
    Ok(factory.recipe(&entry))
}

fn names_custom(object: &Object, custom: &CustomContent) -> bool {
    let is_custom = |value: &Value| value.as_str().is_some_and(|id| custom.item(id).is_some());
    let result = object
        .get("result")
        .and_then(|result| result.get("id"))
        .is_some_and(is_custom);
    let key = object
        .get("key")
        .and_then(Value::as_object)
        .is_some_and(|key| key.values().any(is_custom));
    let ingredients = object
        .get("ingredients")
        .and_then(Value::as_array)
        .is_some_and(|list| list.iter().any(is_custom));
    result || key || ingredients
}

fn ingredient(
    value: &Value,
    custom: &CustomContent,
    tags: &TagSet,
) -> Result<CustomIngredient, String> {
    if let Some(def) = value.as_str().and_then(|id| custom.item(id)) {
        return Ok(CustomIngredient::custom(def));
    }
    tuff_datapack::recipe::ingredient(value, tags).map(CustomIngredient::Item)
}

fn result(object: &Object, custom: &CustomContent) -> Result<tuff_item::item::ItemStack, String> {
    let result = object
        .get("result")
        .and_then(Value::as_object)
        .ok_or("result is not an object")?;
    let id = result
        .get("id")
        .and_then(Value::as_str)
        .ok_or("result has no id")?;
    let count = match result.get("count") {
        None => 1,
        Some(count) => count
            .as_u64()
            .filter(|count| (1..=99).contains(count))
            .ok_or("result count must be 1 to 99")?,
    };
    if let Some(def) = custom.item(id) {
        return Ok(def.stack(count as u8));
    }
    let id = if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    };
    let item = Item::from_name(&id)
        .filter(|item| *item != Item::Air)
        .ok_or_else(|| format!("unknown result item {id}"))?;
    Ok(crafting::result_stack(
        &tuff_vanilla_data::recipe::RecipeResult {
            item,
            count,
            components: result
                .get("components")
                .map(|components| Frozen::from(components.to_string())),
        },
    ))
}

fn category(object: &Object) -> RecipeBookCategory {
    match object.get("category").and_then(Value::as_str) {
        Some("building") => RecipeBookCategory::CraftingBuildingBlocks,
        Some("equipment") => RecipeBookCategory::CraftingEquipment,
        Some("redstone") => RecipeBookCategory::CraftingRedstone,
        _ => RecipeBookCategory::CraftingMisc,
    }
}

fn group(object: &Object) -> Option<&'static str> {
    object
        .get("group")
        .and_then(Value::as_str)
        .filter(|group| !group.is_empty())
        .map(leak)
}

fn shaped(
    name: &'static str,
    object: &Object,
    custom: &CustomContent,
    tags: &TagSet,
) -> Result<Arc<dyn Recipe>, String> {
    let rows: Vec<&str> = object
        .get("pattern")
        .and_then(Value::as_array)
        .ok_or("pattern is not a list")?
        .iter()
        .map(|row| row.as_str().ok_or("pattern row is not a string"))
        .collect::<Result<_, _>>()?;
    let mut key = Vec::new();
    for (symbol, value) in object
        .get("key")
        .and_then(Value::as_object)
        .ok_or("key is not an object")?
    {
        let mut chars = symbol.chars();
        let (Some(symbol), None) = (chars.next(), chars.next()) else {
            return Err(format!("key symbol {symbol:?} is not one character"));
        };
        if symbol == ' ' {
            return Err("' ' is a reserved symbol".to_owned());
        }
        key.push((symbol, ingredient(value, custom, tags)?));
    }
    let mut recipe = CustomShaped::new(name, &rows, &key, result(object, custom)?)
        .ok_or("the pattern is empty, over 3x3, uneven or names a missing symbol")?;
    recipe.category = category(object);
    recipe.group = group(object);
    Ok(Arc::new(recipe))
}

fn shapeless(
    name: &'static str,
    object: &Object,
    custom: &CustomContent,
    tags: &TagSet,
) -> Result<Arc<dyn Recipe>, String> {
    let ingredients = object
        .get("ingredients")
        .and_then(Value::as_array)
        .ok_or("ingredients is not a list")?
        .iter()
        .map(|value| ingredient(value, custom, tags))
        .collect::<Result<Vec<_>, _>>()?;
    if !(1..=9).contains(&ingredients.len()) {
        return Err(format!(
            "a shapeless recipe takes 1 to 9 ingredients, not {}",
            ingredients.len()
        ));
    }
    let mut recipe = CustomShapeless::new(name, ingredients, result(object, custom)?);
    recipe.category = category(object);
    recipe.group = group(object);
    Ok(Arc::new(recipe))
}

/// The stand-in entity type `name` names, for providers that take text.
pub fn stand_in(name: &str) -> Option<EntityType> {
    let name = if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    };
    EntityType::from_name(&name)
}

#[cfg(test)]
mod tests;
