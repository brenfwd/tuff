// SPDX-License-Identifier: MIT
//! Dimensions made and discarded while the server runs, such as one arena
//! per match. Keys need a `cap.dimension` grant (`game:arena_*` covers
//! `game:arena_1`); a made dimension takes an existing one's type, height,
//! biomes and generator, optionally with a copy of its saved chunks, and
//! is kept in memory until it is discarded, the plugin unloads or the
//! server stops.

pub use crate::types::{ChunkArea, DimensionError};

use crate::host;

/// Makes dimension `key` like `base`, first copying the chunks `base` has
/// saved inside `copy`. Usable as soon as this returns.
pub fn create(key: &str, base: &str, copy: Option<ChunkArea>) -> Result<(), DimensionError> {
    host::create_dimension(key, base, copy)
}

/// Queues discarding a dimension this plugin made; players still in it
/// after 5 seconds go to the world spawn.
pub fn discard(key: &str) -> Result<(), DimensionError> {
    host::discard_dimension(key)
}

/// The dimensions this plugin made and has not discarded.
pub fn list() -> Vec<String> {
    host::runtime_dimensions()
}

use crate::registry::{BiomeDef, DimensionType};
use crate::types::BlockPos;
use crate::{
    AssetFile, AssetSet, Biome, Chunk, Content, CustomEntity, CustomItem, DataFile, Dimension,
};

/// Builds a [`Dimension`] declaration for [`Plugin::dimensions`](crate::Plugin::dimensions).
#[derive(Clone, Debug)]
pub struct DimensionDef {
    key: String,
    type_key: String,
    type_json: String,
    biomes: Vec<Biome>,
    natural_spawning: bool,
    sea_level: i32,
    spawn: BlockPos,
}

impl DimensionDef {
    /// A dimension `key` using the vanilla `minecraft:overworld` type.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            type_key: "minecraft:overworld".into(),
            type_json: String::new(),
            biomes: Vec::new(),
            natural_spawning: false,
            sea_level: 0,
            spawn: BlockPos { x: 0, y: 64, z: 0 },
        }
    }

    /// A custom dimension type given as raw registry JSON.
    pub fn dimension_type_json(
        mut self,
        type_key: impl Into<String>,
        json: impl Into<String>,
    ) -> Self {
        self.type_key = type_key.into();
        self.type_json = json.into();
        self
    }

    /// A vanilla dimension type by key.
    pub fn vanilla_type(mut self, type_key: impl Into<String>) -> Self {
        self.type_key = type_key.into();
        self.type_json.clear();
        self
    }

    /// A custom dimension type registered under `type_key`.
    pub fn dimension_type(mut self, type_key: impl Into<String>, ty: &DimensionType) -> Self {
        self.type_key = type_key.into();
        self.type_json = ty.to_json();
        self
    }

    pub fn biome(mut self, key: &str, biome: BiomeDef) -> Self {
        self.biomes.push(biome.into_biome(key));
        self
    }

    pub fn natural_spawning(mut self, v: bool) -> Self {
        self.natural_spawning = v;
        self
    }

    pub fn sea_level(mut self, v: i32) -> Self {
        self.sea_level = v;
        self
    }

    pub fn spawn(mut self, x: i32, y: i32, z: i32) -> Self {
        self.spawn = BlockPos { x, y, z };
        self
    }

    pub fn build(self) -> Dimension {
        Dimension {
            key: self.key,
            dimension_type: self.type_key,
            type_json: self.type_json,
            biomes: self.biomes,
            natural_spawning: self.natural_spawning,
            sea_level: self.sea_level,
            spawn: self.spawn,
        }
    }
}

impl From<DimensionDef> for Dimension {
    fn from(d: DimensionDef) -> Self {
        d.build()
    }
}

/// Fills one chunk column by block state, managing the palette.
#[derive(Clone, Debug)]
pub struct ChunkBuilder {
    min_y: i32,
    height: i32,
    palette: Vec<String>,
    blocks: Vec<u8>,
    biome: String,
    overflow: bool,
}

impl ChunkBuilder {
    /// An all-air column of `height` layers starting at `min_y`.
    pub fn new(min_y: i32, height: i32) -> Self {
        Self {
            min_y,
            height,
            palette: vec!["minecraft:air".into()],
            blocks: vec![0; height.max(0) as usize * 256],
            biome: "minecraft:plains".into(),
            overflow: false,
        }
    }

    pub fn biome(mut self, key: impl Into<String>) -> Self {
        self.biome = key.into();
        self
    }

    fn index_of(&mut self, state: &str) -> u8 {
        if let Some(i) = self.palette.iter().position(|s| s == state) {
            return i as u8;
        }
        if self.palette.len() >= 256 {
            self.overflow = true;
            return 0;
        }
        self.palette.push(state.into());
        (self.palette.len() - 1) as u8
    }

    fn slot(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let ly = y - self.min_y;
        ((0..16).contains(&x) && (0..16).contains(&z) && (0..self.height).contains(&ly))
            .then(|| ly as usize * 256 + z as usize * 16 + x as usize)
    }

    /// Sets the block at chunk-local `x`, `z` and world `y`; out of range is ignored.
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: &str) -> &mut Self {
        let i = self.index_of(state);
        if let Some(s) = self.slot(x, y, z) {
            self.blocks[s] = i;
        }
        self
    }

    /// Fills the whole 16 by 16 layer at world `y`.
    pub fn layer(&mut self, y: i32, state: &str) -> &mut Self {
        let i = self.index_of(state);
        if let Some(s) = self.slot(0, y, 0) {
            self.blocks[s..s + 256].fill(i);
        }
        self
    }

    /// Fills the layers `y0..=y1`.
    pub fn layers(&mut self, y0: i32, y1: i32, state: &str) -> &mut Self {
        for y in y0..=y1 {
            self.layer(y, state);
        }
        self
    }

    /// Fails when more than 256 distinct states were used.
    pub fn build(self) -> Result<Chunk, String> {
        if self.overflow {
            return Err("chunk palette exceeds 256 states".into());
        }
        Ok(Chunk {
            palette: self.palette,
            blocks: self.blocks,
            biome: self.biome,
        })
    }
}

/// The arguments of one chunk generation.
#[derive(Clone, Copy, Debug)]
pub struct GenArgs<'a> {
    pub dimension: &'a str,
    pub seed: i64,
    pub chunk_x: i32,
    pub chunk_z: i32,
    pub min_y: i32,
    pub height: i32,
}

impl GenArgs<'_> {
    /// An empty builder shaped for this chunk.
    pub fn builder(&self) -> ChunkBuilder {
        ChunkBuilder::new(self.min_y, self.height)
    }
}

type Generator = Box<dyn Fn(&GenArgs) -> Result<Chunk, String>>;

/// Dimensions with their generator closures, filled by
/// [`Plugin::worldgen`](crate::Plugin::worldgen).
#[derive(Default)]
pub struct Worldgen {
    entries: Vec<(Dimension, Generator)>,
}

impl Worldgen {
    /// Adds a dimension and the closure that generates its chunks. The
    /// closure must be deterministic in its arguments.
    pub fn dimension(
        &mut self,
        def: DimensionDef,
        generator: impl Fn(&GenArgs) -> Result<Chunk, String> + 'static,
    ) -> &mut Self {
        self.entries.push((def.build(), Box::new(generator)));
        self
    }

    pub fn declarations(&self) -> Vec<Dimension> {
        self.entries.iter().map(|(d, _)| d.clone()).collect()
    }

    pub fn generate(&self, args: &GenArgs) -> Result<Chunk, String> {
        match self.entries.iter().find(|(d, _)| d.key == args.dimension) {
            Some((_, generator)) => generator(args),
            None => Err(format!("no generator for {}", args.dimension)),
        }
    }
}

/// Builds the [`Content`] a plugin declares.
#[derive(Clone, Debug, Default)]
pub struct ContentBuilder {
    content: Option<Content>,
}

impl ContentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn c(&mut self) -> &mut Content {
        self.content.get_or_insert_with(crate::no_content)
    }

    pub fn item(mut self, item: ItemDef) -> Self {
        self.c().items.push(item.0);
        self
    }

    /// A custom entity shown as vanilla `stand_in`.
    pub fn entity(mut self, id: &str, stand_in: &str) -> Self {
        self.c().entities.push(CustomEntity {
            id: id.into(),
            stand_in: stand_in.into(),
            parts: Vec::new(),
        });
        self
    }

    /// Like [`Self::entity`] with composed parts, as `(entity type, SNBT data)`.
    pub fn entity_with_parts(mut self, id: &str, stand_in: &str, parts: &[(&str, &str)]) -> Self {
        self.c().entities.push(CustomEntity {
            id: id.into(),
            stand_in: stand_in.into(),
            parts: parts
                .iter()
                .map(|(kind, data)| crate::PartDecl {
                    kind: (*kind).into(),
                    data: (*data).into(),
                })
                .collect(),
        });
        self
    }

    /// Resource pack files under `assets/<namespace>/`, as `(path, bytes)`.
    pub fn assets(mut self, name: &str, namespace: &str, files: &[(&str, &[u8])]) -> Self {
        self.c().assets.push(AssetSet {
            name: name.into(),
            namespace: namespace.into(),
            files: files
                .iter()
                .map(|(p, d)| AssetFile {
                    path: (*p).into(),
                    data: d.to_vec(),
                })
                .collect(),
        });
        self
    }

    pub fn recipe(mut self, id: &str, json: impl Into<String>) -> Self {
        self.c().recipes.push(DataFile {
            id: id.into(),
            json: json.into(),
        });
        self
    }

    pub fn advancement(mut self, id: &str, json: impl Into<String>) -> Self {
        self.c().advancements.push(DataFile {
            id: id.into(),
            json: json.into(),
        });
        self
    }

    pub fn build(self) -> Content {
        self.content.unwrap_or_else(crate::no_content)
    }
}

impl From<ContentBuilder> for Content {
    fn from(b: ContentBuilder) -> Self {
        b.build()
    }
}

/// A custom item on a vanilla base item.
#[derive(Clone, Debug)]
pub struct ItemDef(CustomItem);

impl ItemDef {
    pub fn new(id: &str, base: &str) -> Self {
        Self(CustomItem {
            id: id.into(),
            base: base.into(),
            model: None,
            components: String::new(),
        })
    }

    pub fn model(mut self, model: &str) -> Self {
        self.0.model = Some(model.into());
        self
    }

    /// Data components as an SNBT compound.
    pub fn components(mut self, snbt: &str) -> Self {
        self.0.components = snbt.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn dimension_def_builds_declaration() {
        let d = DimensionDef::new("p:sky")
            .dimension_type("p:sky_type", &DimensionType::new().range(0, 256))
            .biome("p:void", BiomeDef::new())
            .spawn(1, 100, 2)
            .build();
        assert_eq!(d.dimension_type, "p:sky_type");
        assert!(d.type_json.contains("\"min_y\":0"));
        assert_eq!((d.spawn.x, d.spawn.y, d.spawn.z), (1, 100, 2));
        assert_eq!(d.biomes[0].key, "p:void");
        assert!(DimensionDef::new("p:a").build().type_json.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn worldgen_routes_closures_by_dimension() {
        let mut w = Worldgen::default();
        w.dimension(DimensionDef::new("p:a"), |a| {
            let mut b = a.builder();
            b.layer(a.min_y, "minecraft:stone");
            b.build()
        })
        .dimension(
            DimensionDef::new("p:b").dimension_type_json("p:t", "{}"),
            |a| Err(format!("b {}", a.chunk_x)),
        );
        let d = w.declarations();
        assert_eq!(d.len(), 2);
        assert_eq!(d[1].type_json, "{}");
        let args = |dimension| GenArgs {
            dimension,
            seed: 1,
            chunk_x: 7,
            chunk_z: 0,
            min_y: 0,
            height: 16,
        };
        assert_eq!(w.generate(&args("p:a")).unwrap().blocks[0], 1);
        assert_eq!(w.generate(&args("p:b")).unwrap_err(), "b 7");
        assert!(w.generate(&args("p:c")).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn chunk_builder_indexes_and_dedups_palette() {
        let mut b = ChunkBuilder::new(-64, 384);
        b.layers(-64, -63, "minecraft:stone")
            .set(3, 10, 5, "minecraft:stone")
            .set(99, 10, 5, "minecraft:dirt");
        let c = b.biome("p:b").build().unwrap();
        assert_eq!(c.blocks.len(), 384 * 256);
        assert_eq!(c.palette[1], "minecraft:stone");
        assert_eq!(c.blocks[(10 + 64) as usize * 256 + 5 * 16 + 3], 1);
        assert_eq!(c.blocks[256], 1);
        assert_eq!(c.biome, "p:b");
    }

    #[tuff_core::watchdog]
    #[test]
    fn chunk_builder_rejects_palette_overflow() {
        let mut b = ChunkBuilder::new(0, 16);
        for i in 0..300 {
            b.set(0, 0, 0, &format!("p:b{i}"));
        }
        assert!(b.build().is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn content_builder_collects() {
        let c = ContentBuilder::new()
            .item(ItemDef::new("p:ruby", "minecraft:emerald").model("p:ruby"))
            .entity("p:e", "minecraft:zombie")
            .recipe("p:r", "{}")
            .build();
        assert_eq!(c.items.len(), 1);
        assert_eq!(c.entities[0].stand_in, "minecraft:zombie");
        assert_eq!(c.recipes.len(), 1);
        assert!(ContentBuilder::new().build().items.is_empty());
    }
}
