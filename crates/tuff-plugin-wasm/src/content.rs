//! Custom content plugins declare at startup: items, entities,
//! asset sets, recipes and advancements. Each module is asked once per
//! world start, on its own instance with every import trapping, so the
//! answer depends on the module alone. Every id (and asset namespace)
//! must be in a namespace the owner granted with `cap.content`; the rest
//! is logged with the command that grants it and left out. Core merges
//! what is left into the world's custom content, recipes and
//! advancements, and leaves out a plugin whose content clashes.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;

use bytes::Bytes;
use tuff_core::custom::provided::{self, ContentProvider, DataFile, ProvidedContent};
use tuff_core::custom::{AssetSet, CustomEntityDef, CustomItemDef, EntityPart};
use tuff_core::world::custom_dimension::ProviderContext;
use tuff_item::item::Components;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::Item;

use crate::caps::Policy;
use crate::manifest::Manifest;
use crate::sandbox::{Fault, Limits, engine};
use crate::wit::bindings::{self, types::ContentDecl};
use crate::wit::canon::Bounds;
use crate::wit::runtime::Runtime;
use crate::worldgen::{Stub, modules};

/// What one `declare-content` answer may move to the host: asset files
/// and JSON documents are larger than ordinary call arguments.
fn bounds() -> Bounds {
    Bounds {
        string: 1 << 20,
        list: 4096,
        bytes: 32 << 20,
        values: 1 << 16,
    }
}

/// Asks plugin modules for the custom content they add.
pub struct WasmContent {
    directory: Option<PathBuf>,
    limits: Limits,
    declared: Mutex<Vec<ProvidedContent>>,
}

impl WasmContent {
    pub fn new(directory: Option<PathBuf>, limits: Limits) -> Self {
        Self {
            directory,
            limits,
            declared: Mutex::new(Vec::new()),
        }
    }

    fn declare(&self, wasm: &[u8]) -> Result<Option<ContentDecl>, Fault> {
        let mut runtime = Runtime::new(
            &engine(),
            crate::wit::schema(),
            wasm,
            Stub,
            self.limits,
            bounds(),
        )?;
        if !runtime.exports(bindings::CONTENT_DECLARE_CONTENT) {
            return Ok(None);
        }
        bindings::content_declare_content(&mut runtime).map(Some)
    }
}

impl ContentProvider for WasmContent {
    fn attach(&self, ctx: &ProviderContext) {
        let (modules, grants) = modules(self.directory.as_deref(), ctx);
        let mut declared = Vec::new();
        for (manifest, bytes) in modules {
            let decl = match self.declare(&bytes) {
                Ok(Some(decl)) => decl,
                Ok(None) => continue,
                Err(fault) => {
                    log::error!("plugin {} declares no content: {}", manifest.id, fault.0);
                    continue;
                }
            };
            let policy = Policy::new(manifest.capabilities.clone(), grants.get(&manifest.id));
            let content = convert(&manifest, &policy, decl);
            if !content.is_empty() {
                declared.push(content);
            }
        }
        if let Ok(mut slot) = self.declared.lock() {
            *slot = declared;
        }
    }

    fn content(&self) -> Vec<ProvidedContent> {
        self.declared
            .lock()
            .map(|declared| declared.clone())
            .unwrap_or_default()
    }
}

/// Keeps what the policy grants and what is well formed.
struct Filter<'a> {
    manifest: &'a Manifest,
    policy: &'a Policy,
    denied: BTreeSet<String>,
}

impl Filter<'_> {
    fn granted(&mut self, id: &str) -> bool {
        if self.policy.check_content(id).is_ok() {
            return true;
        }
        let namespace = id.split_once(':').map_or(id, |(namespace, _)| namespace);
        self.denied.insert(namespace.to_owned());
        false
    }

    fn bad(&self, what: &str, id: &str, why: &str) {
        log::error!(
            "plugin {} declares {what} {id} but {why}; leaving it out",
            self.manifest.id
        );
    }
}

fn convert(manifest: &Manifest, policy: &Policy, decl: ContentDecl) -> ProvidedContent {
    let mut filter = Filter {
        manifest,
        policy,
        denied: BTreeSet::new(),
    };
    let mut content = ProvidedContent {
        owner: manifest.id.clone(),
        ..Default::default()
    };
    for item in decl.items {
        if !filter.granted(&item.id) {
            continue;
        }
        match custom_item(&item) {
            Ok(def) => content.items.push(def),
            Err(why) => filter.bad("item", &item.id, &why),
        }
    }
    for entity in decl.entities {
        if !filter.granted(&entity.id) {
            continue;
        }
        match provided::stand_in(&entity.stand_in) {
            Some(stand_in) => match entity_parts(&entity.parts) {
                Ok(parts) => {
                    let mut def = CustomEntityDef::new(entity.id, stand_in);
                    def.parts = parts;
                    content.entities.push(def);
                }
                Err(why) => filter.bad("entity", &entity.id, &why),
            },
            None => filter.bad(
                "entity",
                &entity.id,
                &format!("{} is no entity type", entity.stand_in),
            ),
        }
    }
    for set in decl.assets {
        if !filter.granted(&set.namespace) {
            continue;
        }
        content.assets.push((
            set.name,
            AssetSet {
                namespace: set.namespace,
                files: set
                    .files
                    .into_iter()
                    .map(|file| (file.path, Bytes::from(file.data)))
                    .collect(),
            },
        ));
    }
    for (files, out) in [
        (decl.recipes, &mut content.recipes),
        (decl.advancements, &mut content.advancements),
    ] {
        for file in files {
            if filter.granted(&file.id) {
                out.push(DataFile {
                    id: file.id,
                    json: file.json,
                });
            }
        }
    }
    for namespace in &filter.denied {
        log::warn!(
            "plugin {} declares content in namespace {namespace} but was not granted it; grant \
             it with `tuff plugin grant {} cap.content {namespace}` and restart",
            manifest.id,
            manifest.id
        );
    }
    content
}

fn entity_parts(parts: &[bindings::types::PartDecl]) -> Result<Vec<EntityPart>, String> {
    parts
        .iter()
        .map(|part| {
            let kind = provided::stand_in(&part.kind)
                .ok_or_else(|| format!("part {} is no entity type", part.kind))?;
            let data = if part.data.trim().is_empty() {
                Compound::new()
            } else {
                match Tag::from_snbt(&part.data).map_err(|err| err.to_string())? {
                    Tag::Compound(compound) => compound,
                    _ => return Err("a part's data is not a compound".to_owned()),
                }
            };
            Ok(EntityPart::new(kind, data))
        })
        .collect()
}

fn custom_item(item: &bindings::types::ItemDecl) -> Result<CustomItemDef, String> {
    let base = Item::from_name(&crate::portal::qualified(&item.base))
        .filter(|base| *base != Item::Air)
        .ok_or_else(|| format!("base {} is no item", item.base))?;
    let mut def = CustomItemDef::new(item.id.clone(), base);
    def.model = item.model.clone();
    if !item.components.trim().is_empty() {
        let components = Tag::from_snbt(&item.components).map_err(|err| err.to_string())?;
        let compound = components
            .as_compound()
            .ok_or("its components are not a compound")?;
        def.components =
            tuff_item::item::ComponentsSnapshot::describing(Components::from_nbt(compound));
    }
    Ok(def)
}

#[cfg(test)]
mod tests;
