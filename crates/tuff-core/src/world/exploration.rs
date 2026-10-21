//! Explorer maps: the `exploration_map` item function finds a structure
//! and makes a map centered on it. Finding the structure is slow, so the
//! function only marks the stack; whoever holds it asks the world's
//! [`ExplorationMaps`] service for the map and finishes the stack.

use std::sync::{Arc, OnceLock};

use crate::world::Shared;
use crate::world::dimension::DimensionId;
use crate::world::services::WorldService;
use serde_json::Value;
use tokio::sync::oneshot;
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::MapDecorationType;
use tuff_world_model::BlockPos;

/// The mark on a stack still waiting for its map; never sent to clients.
pub const PENDING: &str = crate::world::drops::EXPLORATION_PENDING;
pub const DECORATIONS: &str = "minecraft:map_decorations";
const TARGET_KEY: &str = "+";

#[derive(Debug, Clone, PartialEq)]
pub struct ExplorationSpec {
    /// A structure id or `#tag`.
    pub destination: String,
    pub decoration: MapDecorationType,
    pub zoom: u8,
    pub search_radius: i32,
    pub skip_existing: bool,
}

impl ExplorationSpec {
    pub fn from_json(function: &Value) -> Option<Self> {
        let decoration = match function.get("decoration") {
            Some(name) => MapDecorationType::from_name(name.as_str()?)?,
            None => MapDecorationType::Mansion,
        };
        Some(Self {
            destination: function
                .get("destination")
                .and_then(Value::as_str)
                .unwrap_or("#minecraft:on_treasure_maps")
                .to_owned(),
            decoration,
            zoom: function
                .get("zoom")
                .and_then(Value::as_u64)
                .unwrap_or(2)
                .min(4) as u8,
            search_radius: function
                .get("search_radius")
                .and_then(Value::as_i64)
                .unwrap_or(50) as i32,
            skip_existing: function
                .get("skip_existing_chunks")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        })
    }

    fn to_nbt(&self) -> Compound {
        Compound::new()
            .with("destination", self.destination.as_str())
            .with("decoration", self.decoration.name())
            .with("zoom", self.zoom as i8)
            .with("search_radius", self.search_radius)
            .with("skip_existing_chunks", self.skip_existing)
    }

    fn from_nbt(nbt: &Compound) -> Option<Self> {
        Some(Self {
            destination: nbt.get_str("destination")?.to_owned(),
            decoration: MapDecorationType::from_name(nbt.get_str("decoration")?)?,
            zoom: nbt.get_byte("zoom")?.clamp(0, 4) as u8,
            search_radius: nbt.get_int("search_radius")?,
            skip_existing: nbt.get_bool("skip_existing_chunks")?,
        })
    }

    /// The structure ids the destination names.
    pub fn structures(&self) -> Vec<&'static str> {
        match self.destination.strip_prefix('#') {
            Some(tag) => tuff_datapack::tags::with(|tags| {
                tags.get("minecraft:worldgen/structure", tag)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|member| structure_named(member))
                    .collect()
            }),
            None => structure_named(&self.destination)
                .map(|name| vec![name])
                .unwrap_or_default(),
        }
    }
}

/// A structure id, built in or from the data packs in scope.
fn structure_named(name: &str) -> Option<&'static str> {
    tuff_vanilla_data::structure::STRUCTURES
        .iter()
        .find(|(id, _)| *id == name)
        .map(|(id, _)| *id)
        .or_else(|| {
            tuff_item::registries::current()
                .exists("minecraft:worldgen/structure", name)
                .filter(|exists| *exists)
                .map(|_| tuff_item::registries::intern(name))
        })
}

pub fn mark(stack: &mut ItemStack, spec: &ExplorationSpec) {
    stack.components.other.insert(PENDING, spec.to_nbt());
}

pub fn pending(stack: &ItemStack) -> Option<ExplorationSpec> {
    ExplorationSpec::from_nbt(stack.components.other.get(PENDING)?.as_compound()?)
}

/// A map made for an explorer map stack.
#[derive(Debug, Clone, PartialEq)]
pub struct ExploredMap {
    pub id: u32,
    pub target: BlockPos,
    pub decoration: MapDecorationType,
}

impl ExploredMap {
    /// Gives the stack its map and the target marker.
    pub fn apply(&self, components: &mut tuff_item::item::Components) {
        components.remove_other(PENDING);
        components.map_id = Some(self.id);
        let marker = Compound::new()
            .with("type", self.decoration.name())
            .with("x", f64::from(self.target.x))
            .with("z", f64::from(self.target.z))
            .with("rotation", 180.0f32);
        let mut decorations = match components.other.get(DECORATIONS) {
            Some(Tag::Compound(existing)) => existing.clone(),
            _ => Compound::new(),
        };
        decorations.insert(TARGET_KEY, marker);
        components.other.insert(DECORATIONS, decorations);
    }
}

/// Starts the search for a rolled explorer map stack's map; the answer
/// is the map, or `None` when no structure was found.
pub fn finish(
    shared: &Arc<Shared>,
    dimension: DimensionId,
    origin: BlockPos,
    stack: &ItemStack,
) -> Option<Answer> {
    let spec = pending(stack)?;
    shared
        .services
        .get::<ExplorationMaps>()?
        .request(dimension, origin, spec)
}

/// The answer to a map request.
pub type Answer = oneshot::Receiver<Option<ExploredMap>>;

/// `None` while the map is still being made; then the map, or `None`
/// inside when there is none.
pub fn poll(answer: &mut Answer) -> Option<Option<ExploredMap>> {
    match answer.try_recv() {
        Ok(found) => Some(found),
        Err(oneshot::error::TryRecvError::Empty) => None,
        Err(oneshot::error::TryRecvError::Closed) => Some(None),
    }
}

/// The answer to a structure search.
pub type Located = oneshot::Receiver<Option<BlockPos>>;

/// `None` while the search runs; then the structure, or `None` inside
/// when there is none.
pub fn poll_located(answer: &mut Located) -> Option<Option<BlockPos>> {
    match answer.try_recv() {
        Ok(found) => Some(found),
        Err(oneshot::error::TryRecvError::Empty) => None,
        Err(oneshot::error::TryRecvError::Closed) => Some(None),
    }
}

pub type Explore = fn(
    &Arc<Shared>,
    DimensionId,
    BlockPos,
    ExplorationSpec,
) -> oneshot::Receiver<Option<ExploredMap>>;

pub type Locate =
    fn(&Arc<Shared>, DimensionId, BlockPos, ExplorationSpec) -> oneshot::Receiver<Option<BlockPos>>;

/// The world's maker of explorer maps, registered by the feature owning
/// maps and found by type by anyone handing them out.
pub struct ExplorationMaps {
    explore: Explore,
    locate: Option<Locate>,
    shared: OnceLock<Arc<Shared>>,
}

impl ExplorationMaps {
    pub fn new(explore: Explore) -> Self {
        Self {
            explore,
            locate: None,
            shared: OnceLock::new(),
        }
    }

    pub fn with_locator(mut self, locate: Locate) -> Self {
        self.locate = Some(locate);
        self
    }

    /// Starts finding the nearest structure of `spec`'s destination within
    /// its search radius; the answer is `None` when there is none.
    pub fn locate(
        &self,
        dimension: DimensionId,
        origin: BlockPos,
        spec: ExplorationSpec,
    ) -> Option<oneshot::Receiver<Option<BlockPos>>> {
        let shared = self.shared.get()?;
        Some((self.locate?)(shared, dimension, origin, spec))
    }

    /// Starts making a map for `spec` searched from `origin`; the answer is
    /// `None` when no structure was found.
    pub fn request(
        &self,
        dimension: DimensionId,
        origin: BlockPos,
        spec: ExplorationSpec,
    ) -> Option<oneshot::Receiver<Option<ExploredMap>>> {
        let shared = self.shared.get()?;
        Some((self.explore)(shared, dimension, origin, spec))
    }
}

impl WorldService for ExplorationMaps {
    fn start(&self, shared: &Arc<Shared>) {
        let _ = self.shared.set(shared.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::MintReason;
    use tuff_vanilla_data::Item;

    #[tuff_core::watchdog]
    #[test]
    fn specs_read_the_functions_defaults_and_survive_the_mark() {
        let spec = ExplorationSpec::from_json(&serde_json::json!({
            "type": "minecraft:exploration_map",
            "destination": "#minecraft:on_plains_village_maps",
            "decoration": "minecraft:village_plains",
            "search_radius": 100
        }))
        .unwrap();
        assert_eq!(spec.decoration, MapDecorationType::VillagePlains);
        assert_eq!(
            (spec.zoom, spec.search_radius, spec.skip_existing),
            (2, 100, true)
        );
        assert!(spec.structures().contains(&"minecraft:village_plains"));
        let defaults = ExplorationSpec::from_json(&serde_json::json!({})).unwrap();
        assert_eq!(defaults.decoration, MapDecorationType::Mansion);
        assert_eq!(defaults.search_radius, 50);

        let mut stack = ItemStack::mint(Item::PlainsVillageMap, 1, MintReason::Test);
        mark(&mut stack, &spec);
        assert_eq!(pending(&stack), Some(spec));
        ExploredMap {
            id: 7,
            target: BlockPos::new(100, 64, -40),
            decoration: MapDecorationType::VillagePlains,
        }
        .apply(&mut stack.components);
        assert!(pending(&stack).is_none());
        assert_eq!(stack.components.map_id, Some(7));
        let target = stack
            .components
            .other
            .get(DECORATIONS)
            .unwrap()
            .as_compound()
            .unwrap();
        let target = target.get("+").unwrap().as_compound().unwrap();
        assert_eq!(target.get_str("type"), Some("minecraft:village_plains"));
        assert_eq!(target.get_double("x"), Some(100.0));
    }
}
