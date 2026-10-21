//! Feature-declared custom content for vanilla clients: items built on a
//! vanilla base item, entity definitions saved as a vanilla stand-in type,
//! and resource pack assets. There are no custom blocks: block-shaped
//! content should be a display entity over a vanilla block, placed from a
//! custom item's `use_on_block`.
//!
//! Features register into [`Definitions`] (`Registries::custom`) during
//! `Content`; `feature::load_all` validates everything once and freezes it
//! into an immutable [`CustomContent`] that `Shared::custom` hands to every
//! actor without messages.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::EntityType;

use tuff_item::item::{Components, ItemStack};
use tuff_item::load::{LoadError, LoadErrorKind};
use tuff_item::registry::Registry;
use tuff_nbt::{Compound, Tag};

pub mod catalog;
pub mod entities;
pub mod items;
pub mod provided;
pub mod recipes;

pub use entities::EntityPart;
pub use items::{CustomItemBehavior, Handled, ItemBehavior};

/// The `custom_data` key core stores its identity compound under.
use tuff_item::item::identity::{IDENTITY_KEY, PRIVATE_KEY};
const CUSTOM_DATA: &str = "minecraft:custom_data";
const ITEM_MODEL: &str = "minecraft:item_model";

/// A custom item: a stack of `base` with `components` on top, tagged with
/// its id in `custom_data` so it survives saves and the wire unchanged.
#[derive(Debug, Clone)]
pub struct CustomItemDef {
    pub id: String,
    pub base: Item,
    /// The `item_model` the pack provides; `None` keeps the base model.
    pub model: Option<String>,
    pub components: Components,
    /// Whether the base item's own behaviour still runs on use.
    pub inherit_behavior: bool,
    /// What using the item does, before (or instead of) the base item.
    pub behavior: Option<ItemBehavior>,
    /// Whether `custom_data` beyond the identity stays on the server.
    pub private_data: bool,
}

impl CustomItemDef {
    pub fn new(id: impl Into<String>, base: Item) -> Self {
        Self {
            id: id.into(),
            base,
            model: None,
            components: Components::default(),
            inherit_behavior: false,
            behavior: None,
            private_data: false,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_components(mut self, components: Components) -> Self {
        self.components = components;
        self
    }

    /// A stack of this item.
    pub fn stack(&self, count: u8) -> ItemStack {
        let mut components = self.components.clone();
        if let Some(model) = &self.model {
            components.other.insert(ITEM_MODEL, model.as_str());
        }
        let mut data = match components.other.remove(CUSTOM_DATA) {
            Some(Tag::Compound(data)) => data,
            _ => Compound::new(),
        };
        let mut identity = Compound::new().with("item", self.id.as_str());
        if self.private_data {
            identity.insert(PRIVATE_KEY, true);
        }
        data.insert(IDENTITY_KEY, identity);
        components.other.insert(CUSTOM_DATA, data);
        if !self.inherit_behavior {
            for kind in items::predicted_use_components(self.base) {
                if !components.other.contains_key(kind.name()) {
                    components.remove_network(kind);
                }
            }
        }
        ItemStack::new(self.base, count).with_components(components)
    }
}

/// The custom item id a stack carries, if any.
pub fn item_id(stack: &ItemStack) -> Option<&str> {
    stack
        .components
        .other
        .get(CUSTOM_DATA)?
        .as_compound()?
        .get(IDENTITY_KEY)?
        .as_compound()?
        .get_str("item")
}

const ENTITY_TAG_PREFIX: &str = "tuff.entity/";

/// The scoreboard tag that marks a stand-in entity as custom entity `id`.
/// Tags save with the entity under its vanilla type, so vanilla opens it as
/// the stand-in and the identity survives any reload.
pub fn entity_tag(id: &str) -> String {
    format!("{ENTITY_TAG_PREFIX}{id}")
}

/// The custom entity id among an entity's tags, if any.
pub fn entity_id(tags: &[String]) -> Option<&str> {
    tags.iter().find_map(|t| t.strip_prefix(ENTITY_TAG_PREFIX))
}

/// A custom entity, saved and sent as the vanilla `stand_in` type.
#[derive(Debug, Clone)]
pub struct CustomEntityDef {
    pub id: String,
    pub stand_in: EntityType,
    /// Display and interaction entities that ride the stand-in.
    pub parts: Vec<entities::EntityPart>,
    /// Hide the stand-in so only the parts are visible.
    pub invisible: bool,
}

impl CustomEntityDef {
    pub fn new(id: impl Into<String>, stand_in: EntityType) -> Self {
        Self {
            id: id.into(),
            stand_in,
            parts: Vec::new(),
            invisible: false,
        }
    }

    pub fn invisible(mut self) -> Self {
        self.invisible = true;
        self
    }

    pub fn with_part(mut self, part: entities::EntityPart) -> Self {
        self.parts.push(part);
        self
    }
}

/// Resource pack files a feature contributes, by path under
/// `assets/<namespace>/`.
#[derive(Debug, Clone)]
pub struct AssetSet {
    pub namespace: String,
    pub files: Vec<(String, Bytes)>,
}

/// Where features register custom content.
pub struct Definitions {
    pub items: Registry<CustomItemDef>,
    pub entities: Registry<CustomEntityDef>,
    pub assets: Registry<AssetSet>,
    resolved: OnceLock<Arc<CustomContent>>,
}

impl Default for Definitions {
    fn default() -> Self {
        Self {
            items: Registry::new("custom_items"),
            entities: Registry::new("custom_entities"),
            assets: Registry::new("custom_assets"),
            resolved: OnceLock::new(),
        }
    }
}

fn other(feature: &'static str, message: String) -> LoadError {
    LoadError {
        feature,
        kind: LoadErrorKind::Other(message),
    }
}

fn check_id(feature: &'static str, what: &str, id: &str) -> Result<(), LoadError> {
    if is_identifier(id) && !id.starts_with("minecraft:") {
        Ok(())
    } else {
        Err(other(
            feature,
            format!("{what} id {id:?} must be a namespaced identifier outside minecraft"),
        ))
    }
}

/// `namespace:path` with the identifier character sets.
pub fn is_identifier(id: &str) -> bool {
    let Some((namespace, path)) = id.split_once(':') else {
        return false;
    };
    !namespace.is_empty()
        && !path.is_empty()
        && namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
        && path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'))
}

pub(crate) fn is_asset_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'))
}

impl Definitions {
    pub fn register_item(
        &mut self,
        feature: &'static str,
        def: CustomItemDef,
    ) -> Result<(), LoadError> {
        check_id(feature, "custom item", &def.id)?;
        if def.base == Item::Air {
            return Err(other(
                feature,
                format!("custom item {} has no base item", def.id),
            ));
        }
        if let Some(model) = &def.model
            && !is_identifier(model)
        {
            return Err(other(
                feature,
                format!(
                    "custom item {} model {model:?} is not an identifier",
                    def.id
                ),
            ));
        }
        if let Some(problem) = def.problem() {
            return Err(other(feature, format!("custom item {}: {problem}", def.id)));
        }
        self.items.register(feature, def.id.clone(), def)
    }

    pub fn register_entity(
        &mut self,
        feature: &'static str,
        def: CustomEntityDef,
    ) -> Result<(), LoadError> {
        check_id(feature, "custom entity", &def.id)?;
        if def.stand_in == EntityType::Player {
            return Err(other(
                feature,
                format!("custom entity {} cannot stand in as a player", def.id),
            ));
        }
        if let Some(bad) = def
            .parts
            .iter()
            .find(|p| !entities::part_kind_allowed(p.kind))
        {
            return Err(other(
                feature,
                format!(
                    "custom entity {} part {:?} must be a display, interaction or marker entity",
                    def.id, bad.kind
                ),
            ));
        }
        if def.parts.len() > entities::MAX_PARTS {
            return Err(other(
                feature,
                format!(
                    "custom entity {} has more than {} parts",
                    def.id,
                    entities::MAX_PARTS
                ),
            ));
        }
        self.entities.register(feature, def.id.clone(), def)
    }

    /// Registers assets under `key`, a feature-chosen name for the set.
    pub fn register_assets(
        &mut self,
        feature: &'static str,
        key: impl Into<String>,
        set: AssetSet,
    ) -> Result<(), LoadError> {
        let key = key.into();
        if set.namespace.is_empty() || !is_identifier(&format!("{}:x", set.namespace)) {
            return Err(other(
                feature,
                format!("asset set {key} namespace {:?} is invalid", set.namespace),
            ));
        }
        if let Some((path, _)) = set.files.iter().find(|(path, _)| !is_asset_path(path)) {
            return Err(other(
                feature,
                format!("asset set {key} path {path:?} is invalid"),
            ));
        }
        self.assets.register(feature, key, set)
    }

    /// The validated content, resolved on first call. Errors name the
    /// features whose definitions clash.
    pub fn resolve(&mut self) -> Result<Arc<CustomContent>, LoadError> {
        if let Some(content) = self.resolved.get() {
            return Ok(content.clone());
        }
        let content = Arc::new(CustomContent::build(self)?);
        self.items.freeze();
        self.entities.freeze();
        self.assets.freeze();
        Ok(self.resolved.get_or_init(|| content).clone())
    }

    /// The resolved content, or empty content if resolution failed or
    /// never ran (registries built without `feature::load_all`).
    pub fn content(&self) -> Arc<CustomContent> {
        self.resolved
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(CustomContent::default()))
    }
}

fn lang_table(assets: &BTreeMap<String, Bytes>) -> HashMap<String, String> {
    let mut table = HashMap::new();
    for (path, bytes) in assets {
        let is_lang = path
            .strip_prefix("assets/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(_, file)| file == "lang/en_us.json");
        if !is_lang {
            continue;
        }
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(bytes) {
            for (key, value) in map {
                if let serde_json::Value::String(value) = value {
                    table.insert(key, value);
                }
            }
        }
    }
    table
}

/// The immutable, validated custom content tables.
#[derive(Debug, Default)]
pub struct CustomContent {
    items: HashMap<String, Arc<CustomItemDef>>,
    entities: HashMap<String, Arc<CustomEntityDef>>,
    assets: BTreeMap<String, Bytes>,
    lang: HashMap<String, String>,
    asset_sets: Vec<crate::custom_pack::AssetEntry>,
}

impl CustomContent {
    fn build(defs: &Definitions) -> Result<Self, LoadError> {
        let items: HashMap<_, _> = defs
            .items
            .iter()
            .map(|(id, def)| (id.to_owned(), def.clone()))
            .collect();
        let entities = defs
            .entities
            .iter()
            .map(|(id, def)| (id.to_owned(), def.clone()))
            .collect();
        let asset_sets: Vec<_> = defs
            .assets
            .iter()
            .map(|(key, set)| crate::custom_pack::AssetEntry {
                key: key.to_owned(),
                feature: defs.assets.registered_by(key).unwrap_or("?"),
                set: set.clone(),
            })
            .collect();
        let assets = crate::custom_pack::merge(&asset_sets).map_err(|conflict| LoadError {
            feature: conflict.second.1,
            kind: LoadErrorKind::Conflict {
                registry: "custom_assets",
                key: format!("{} ({})", conflict.path, conflict.detail),
                first_registered_by: conflict.first.1,
            },
        })?;
        let lang = lang_table(&assets);
        Ok(Self {
            items,
            lang,
            entities,
            assets,
            asset_sets,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.entities.is_empty() && self.assets.is_empty()
    }

    pub fn item(&self, id: &str) -> Option<&Arc<CustomItemDef>> {
        self.items.get(id)
    }

    /// The custom item definition a stack is an instance of.
    pub fn item_of(&self, stack: &ItemStack) -> Option<&Arc<CustomItemDef>> {
        self.items.get(item_id(stack)?)
    }

    pub fn entity(&self, id: &str) -> Option<&Arc<CustomEntityDef>> {
        self.entities.get(id)
    }

    /// The custom entity definition an entity with these tags is an instance of.
    pub fn entity_of(&self, tags: &[String]) -> Option<&Arc<CustomEntityDef>> {
        self.entity(entity_id(tags)?)
    }

    /// The pack's `en_us` translation for a key, from every registered
    /// `lang/en_us.json`.
    pub fn translation(&self, key: &str) -> Option<&str> {
        self.lang.get(key).map(String::as_str)
    }

    /// `text` as plain text, resolving keys against the pack's language
    /// tables before the vanilla one.
    pub fn plain(&self, text: &tuff_item::text::TextComponent) -> String {
        text.plain_with(&|key| self.translation(key))
    }

    /// Custom items in id order, `page_size` per page; a page past the end
    /// is empty. The stacks a catalog menu shows.
    pub fn catalog_page(&self, page: usize, page_size: usize) -> Vec<ItemStack> {
        let mut ids: Vec<&String> = self.items.keys().collect();
        ids.sort();
        ids.into_iter()
            .skip(page.saturating_mul(page_size))
            .take(page_size)
            .map(|id| self.items[id].stack(1))
            .collect()
    }

    /// Number of catalog pages of `page_size` items.
    pub fn catalog_pages(&self, page_size: usize) -> usize {
        self.items.len().div_ceil(page_size.max(1))
    }

    /// Every asset file by its full pack path (`assets/<ns>/...`), sorted.
    pub fn assets(&self) -> &BTreeMap<String, Bytes> {
        &self.assets
    }

    /// The registered asset sets with their owners, in merge order.
    pub fn asset_sets(&self) -> &[crate::custom_pack::AssetEntry] {
        &self.asset_sets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn item_stack_carries_identity_and_model() {
        let def = CustomItemDef::new("ex:ruby", Item::Emerald).with_model("ex:ruby");
        let mut defs = Definitions::default();
        defs.register_item("ex", def).unwrap();
        let content = defs.resolve().unwrap();
        let stack = content.item("ex:ruby").unwrap().stack(3);
        assert_eq!(stack.item, Item::Emerald);
        assert_eq!(stack.count, 3);
        assert_eq!(item_id(&stack), Some("ex:ruby"));
        assert_eq!(content.item_of(&stack).unwrap().id, "ex:ruby");
        assert_eq!(
            stack.components.other.get(ITEM_MODEL).and_then(Tag::as_str),
            Some("ex:ruby")
        );
        assert!(content.item_of(&ItemStack::new(Item::Emerald, 1)).is_none());
        assert!(stack.stacks_with(&content.item("ex:ruby").unwrap().stack(1)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn existing_custom_data_is_kept() {
        let mut components = Components::default();
        components
            .other
            .insert(CUSTOM_DATA, Compound::new().with("charge", 2));
        let stack = CustomItemDef::new("ex:wand", Item::Stick)
            .with_components(components)
            .stack(1);
        let data = stack
            .components
            .other
            .get(CUSTOM_DATA)
            .unwrap()
            .as_compound()
            .unwrap();
        assert!(data.contains_key("charge"));
        assert_eq!(item_id(&stack), Some("ex:wand"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejects_bad_ids_and_bases() {
        let mut defs = Definitions::default();
        assert!(
            defs.register_item("ex", CustomItemDef::new("ruby", Item::Emerald))
                .is_err()
        );
        assert!(
            defs.register_item("ex", CustomItemDef::new("minecraft:ruby", Item::Emerald))
                .is_err()
        );
        assert!(
            defs.register_item("ex", CustomItemDef::new("ex:Ruby", Item::Emerald))
                .is_err()
        );
        assert!(
            defs.register_item("ex", CustomItemDef::new("ex:ruby", Item::Air))
                .is_err()
        );
        defs.register_item("ex", CustomItemDef::new("ex:ruby", Item::Emerald))
            .unwrap();
        let err = defs
            .register_item("other", CustomItemDef::new("ex:ruby", Item::Diamond))
            .unwrap_err();
        assert!(matches!(
            err.kind,
            LoadErrorKind::Conflict {
                first_registered_by: "ex",
                ..
            }
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn lang_tables_and_catalog() {
        use tuff_item::text::TextComponent;
        let mut defs = Definitions::default();
        defs.items
            .register("ex", "ex:b", CustomItemDef::new("ex:b", Item::Paper))
            .unwrap();
        defs.items
            .register("ex", "ex:a", CustomItemDef::new("ex:a", Item::Paper))
            .unwrap();
        defs.register_assets(
            "ex",
            "lang",
            AssetSet {
                namespace: "ex".into(),
                files: vec![(
                    "lang/en_us.json".to_owned(),
                    Bytes::from_static(br#"{"item.ex.ruby":"Ruby of %s"}"#),
                )],
            },
        )
        .unwrap();
        let content = defs.resolve().unwrap();
        let text = TextComponent::translate("item.ex.ruby", [TextComponent::new("Fire")]);
        assert_eq!(content.plain(&text), "Ruby of Fire");
        assert_eq!(text.plain(), "item.ex.ruby");
        assert_eq!(content.catalog_pages(1), 2);
        let first = content.catalog_page(0, 1);
        assert_eq!(content.item_of(&first[0]).unwrap().id, "ex:a");
        assert!(content.catalog_page(2, 1).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn entity_identity_rides_on_tags() {
        let mut defs = Definitions::default();
        defs.register_entity(
            "ex",
            CustomEntityDef::new("ex:golem", EntityType::IronGolem),
        )
        .unwrap();
        let content = defs.resolve().unwrap();
        let tags = vec!["other".to_owned(), entity_tag("ex:golem")];
        assert_eq!(entity_id(&tags), Some("ex:golem"));
        assert_eq!(
            content.entity_of(&tags).unwrap().stand_in,
            EntityType::IronGolem
        );
        assert!(content.entity_of(&[entity_tag("ex:gone")]).is_none());
        assert!(content.entity_of(&["other".to_owned()]).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn entities_and_assets() {
        let mut defs = Definitions::default();
        defs.register_entity(
            "ex",
            CustomEntityDef::new("ex:golem", EntityType::IronGolem),
        )
        .unwrap();
        assert!(
            defs.register_entity("ex", CustomEntityDef::new("ex:p", EntityType::Player))
                .is_err()
        );
        let set = |path: &str| AssetSet {
            namespace: "ex".into(),
            files: vec![(path.to_owned(), Bytes::from_static(b"{}"))],
        };
        assert!(defs.register_assets("ex", "bad", set("../x.json")).is_err());
        assert!(defs.register_assets("ex", "bad", set("/x.json")).is_err());
        defs.register_assets("ex", "main", set("items/ruby.json"))
            .unwrap();
        defs.register_assets("ex", "more", set("models/ruby.json"))
            .unwrap();
        let content = defs.resolve().unwrap();
        assert_eq!(
            content.entity("ex:golem").unwrap().stand_in,
            EntityType::IronGolem
        );
        let paths: Vec<_> = content.assets().keys().cloned().collect();
        assert_eq!(
            paths,
            ["assets/ex/items/ruby.json", "assets/ex/models/ruby.json"]
        );

        let mut clash = Definitions::default();
        clash
            .register_assets("a", "x", set("items/ruby.json"))
            .unwrap();
        clash
            .register_assets("b", "y", set("items/ruby.json"))
            .unwrap();
        let err = clash.resolve().unwrap_err();
        assert!(matches!(
            err.kind,
            LoadErrorKind::Conflict {
                first_registered_by: "a",
                ..
            }
        ));
    }
}
