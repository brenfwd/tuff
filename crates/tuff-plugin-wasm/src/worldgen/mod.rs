//! Dimensions plugins declare and the chunks they generate. The module is
//! probed once at startup for `declare-dimensions`; dimensions the owner
//! did not grant with `cap.dimension` are logged with the command that
//! grants them and left out. Chunks come from a pool of separate instances
//! of the module whose imports all trap, so generation is pure in its
//! arguments, runs in parallel and never contends with the plugin's main
//! sandbox. An instance that traps is discarded and its chunk is empty.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::wit::runtime::Engine;
use tuff_core::world::custom_dimension::{
    BiomeDefinition, DimensionDefinition, DimensionProvider, ProviderContext,
};
use tuff_core::world::generator::ChunkGenerator;
use tuff_vanilla_data::BlockState;
use tuff_world_model::BlockPos;
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::pos::ChunkPos;

use crate::caps::{Grants, Policy};
use crate::sandbox::{Fault, Limits, engine};
use crate::wit::bindings::{self, types::DimensionDecl, types::GeneratedChunk};
use crate::wit::canon::Value;
use crate::wit::runtime::{Host, Runtime};
use crate::wit::value::{self, ToValue};
use crate::{DEFAULT_DIRECTORY, DIRECTORY_KEY, ENABLED_KEY, manifest};

const MAX_IDLE: usize = 16;

pub(crate) struct Stub;

impl Host for Stub {
    fn call(
        &mut self,
        _index: usize,
        _args: Vec<Value<'_>>,
    ) -> value::Result<Option<Box<dyn ToValue>>> {
        Err("world generation may not call the host".into())
    }
}

/// Instances of one module for generating chunks.
pub struct Pool {
    engine: Engine,
    wasm: Vec<u8>,
    limits: Limits,
    max_idle: usize,
    idle: Mutex<Vec<Runtime<Stub>>>,
}

impl Pool {
    pub fn new(wasm: Vec<u8>, limits: Limits) -> Self {
        Self::with_idle(wasm, limits, MAX_IDLE)
    }

    /// A pool that keeps at most `max_idle` finished instances.
    pub fn with_idle(wasm: Vec<u8>, limits: Limits, max_idle: usize) -> Self {
        Self {
            engine: engine(),
            wasm,
            limits,
            max_idle,
            idle: Mutex::new(Vec::new()),
        }
    }

    fn instantiate(&self) -> Result<Runtime<Stub>, Fault> {
        Runtime::new(
            &self.engine,
            crate::wit::schema(),
            &self.wasm,
            Stub,
            self.limits,
            self.limits.bounds(),
        )
    }

    /// Runs `f` on an idle instance, or a new one; an instance that
    /// faulted is dropped, the rest go back.
    pub(crate) fn with<R>(
        &self,
        f: impl FnOnce(&mut Runtime<Stub>) -> Result<R, Fault>,
    ) -> Result<R, Fault> {
        let idle = self.idle.lock().ok().and_then(|mut idle| idle.pop());
        let mut runtime = match idle {
            Some(runtime) => runtime,
            None => self.instantiate()?,
        };
        let result = f(&mut runtime)?;
        if let Ok(mut idle) = self.idle.lock()
            && idle.len() < self.max_idle
        {
            idle.push(runtime);
        }
        Ok(result)
    }

    pub fn declare(&self) -> Result<Vec<DimensionDecl>, Fault> {
        self.with(|runtime| {
            if !runtime.exports(bindings::WORLDGEN_DECLARE_DIMENSIONS) {
                return Ok(Vec::new());
            }
            bindings::worldgen_declare_dimensions(runtime)
        })
    }

    pub fn generate(
        &self,
        dimension: &str,
        seed: i64,
        pos: ChunkPos,
        shape: ChunkShape,
    ) -> Result<GeneratedChunk, String> {
        self.with(|runtime| {
            bindings::worldgen_generate_chunk(
                runtime,
                dimension,
                &seed,
                &pos.x,
                &pos.z,
                &shape.min_y,
                &shape.height,
            )
        })
        .map_err(|fault| fault.0)?
    }
}

struct Declared {
    plugin: String,
    decl: DimensionDecl,
    pool: Arc<Pool>,
    claims_spawn: bool,
}

/// Adds the dimensions plugin modules declare.
pub struct WorldgenProvider {
    directory: Option<PathBuf>,
    limits: Limits,
    declared: OnceLock<Vec<Declared>>,
}

impl WorldgenProvider {
    pub fn new(directory: Option<PathBuf>, limits: Limits) -> Self {
        Self {
            directory,
            limits,
            declared: OnceLock::new(),
        }
    }

    fn declared(&self) -> &[Declared] {
        self.declared.get().map(Vec::as_slice).unwrap_or(&[])
    }

    fn scan(&self, ctx: &ProviderContext) -> Vec<Declared> {
        let (modules, grants) = modules(self.directory.as_deref(), ctx);
        let mut declared = Vec::new();
        for (manifest, bytes) in modules {
            let pool = Arc::new(Pool::new(bytes, self.limits));
            let decls = match pool.declare() {
                Ok(decls) => decls,
                Err(fault) => {
                    log::error!("plugin {} declares no dimensions: {}", manifest.id, fault.0);
                    continue;
                }
            };
            let policy = Policy::new(manifest.capabilities.clone(), grants.get(&manifest.id));
            let first = declared.len();
            for decl in decls {
                match policy.check_dimension(&decl.key) {
                    Ok(()) => declared.push(Declared {
                        plugin: manifest.id.clone(),
                        claims_spawn: manifest.spawn_dimension.as_deref()
                            == Some(decl.key.as_str()),
                        decl,
                        pool: pool.clone(),
                    }),
                    Err(_) => log::warn!(
                        "plugin {} declares dimension {} but was not granted it; grant it with \
                         `tuff plugin grant {} dimension {}` and restart",
                        manifest.id,
                        decl.key,
                        manifest.id,
                        decl.key
                    ),
                }
            }
            if let Some(spawn) = &manifest.spawn_dimension
                && !declared[first..].iter().any(|d| d.claims_spawn)
            {
                log::warn!(
                    "plugin {} claims the spawn dimension {spawn}, which it does not add \
                     (not declared or not granted)",
                    manifest.id
                );
            }
        }
        declared
    }
}

/// The plugin directory: `configured`, else the setting, else the default.
fn directory(configured: Option<&Path>, ctx: &ProviderContext) -> PathBuf {
    if let Some(directory) = configured {
        return directory.to_owned();
    }
    let setting = ctx.settings.get_string(DIRECTORY_KEY);
    let directory = setting
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(DEFAULT_DIRECTORY);
    PathBuf::from(directory)
}

/// Every plugin module in the plugin directory with a readable manifest,
/// sorted by file name, and the owner's grants; nothing when runtime
/// plugins are switched off.
pub(crate) fn modules(
    configured: Option<&Path>,
    ctx: &ProviderContext,
) -> (Vec<(manifest::Manifest, Vec<u8>)>, Grants) {
    if ctx.settings.get_bool(ENABLED_KEY) == Some(false) {
        return (Vec::new(), Grants::default());
    }
    let directory = directory(configured, ctx);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return (Vec::new(), Grants::default());
    };
    let mut files: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.ends_with(".wasm") && !name.starts_with('.'))
        .collect();
    files.sort();
    let rows = ctx.config_store.read_plugin_grants().unwrap_or_else(|err| {
        log::error!("{err}; no plugin capabilities are granted");
        Vec::new()
    });
    let (grants, _) = Grants::from_rows(rows);
    let modules = files
        .into_iter()
        .filter_map(|file| {
            let bytes = std::fs::read(directory.join(&file)).ok()?;
            let manifest = manifest::read(&bytes).ok()?;
            Some((manifest, bytes))
        })
        .collect();
    (modules, grants)
}

impl DimensionProvider for WorldgenProvider {
    fn attach(&self, ctx: &ProviderContext) {
        let declared = self.scan(ctx);
        let _ = self.declared.set(declared);
    }

    fn dimensions(&self) -> Vec<DimensionDefinition> {
        self.declared()
            .iter()
            .map(|d| DimensionDefinition {
                key: d.decl.key.clone(),
                dimension_type: d.decl.dimension_type.clone(),
                type_json: d.decl.type_json.clone(),
                biomes: d
                    .decl
                    .biomes
                    .iter()
                    .map(|b| BiomeDefinition {
                        key: b.key.clone(),
                        json: b.json.clone(),
                    })
                    .collect(),
                natural_spawning: d.decl.natural_spawning,
                claims_spawn: d.claims_spawn,
            })
            .collect()
    }

    fn generator(
        &self,
        dimension: &str,
        seed: i64,
        biomes: &Arc<Biomes>,
    ) -> Arc<dyn ChunkGenerator> {
        let declared = self.declared().iter().find(|d| d.decl.key == dimension);
        Arc::new(WasmGenerator {
            plugin: declared.map(|d| d.plugin.clone()).unwrap_or_default(),
            dimension: dimension.to_owned(),
            seed,
            sea_level: declared.map_or(0, |d| d.decl.sea_level),
            spawn: declared.map_or(BlockPos::new(0, 64, 0), |d| {
                BlockPos::new(d.decl.spawn.x, d.decl.spawn.y, d.decl.spawn.z)
            }),
            pool: declared.map(|d| d.pool.clone()),
            biomes: biomes.clone(),
        })
    }
}

struct WasmGenerator {
    plugin: String,
    dimension: String,
    seed: i64,
    sea_level: i32,
    spawn: BlockPos,
    pool: Option<Arc<Pool>>,
    biomes: Arc<Biomes>,
}

impl WasmGenerator {
    fn build(&self, shape: ChunkShape, made: GeneratedChunk) -> Result<Chunk, String> {
        let volume = shape.height.max(0) as usize * 256;
        if made.blocks.len() != volume {
            return Err(format!(
                "returned {} block indices, expected {volume}",
                made.blocks.len()
            ));
        }
        let palette: Vec<BlockState> = made
            .palette
            .iter()
            .map(|text| {
                crate::world::parse_state(text).ok_or_else(|| format!("unknown block {text}"))
            })
            .collect::<Result<_, _>>()?;
        if let Some(bad) = made.blocks.iter().find(|i| **i as usize >= palette.len()) {
            return Err(format!("block index {bad} is outside the palette"));
        }
        let biome = self
            .biomes
            .id(&made.biome)
            .ok_or_else(|| format!("unknown biome {}", made.biome))?;
        let air = tuff_vanilla_data::Block::Air.default_state();
        let mut chunk = Chunk::uniform(shape, air, biome);
        for (i, index) in made.blocks.iter().enumerate() {
            let state = palette[*index as usize];
            if state == air {
                continue;
            }
            let y = shape.min_y + (i >> 8) as i32;
            chunk.set((i & 15) as i32, y, ((i >> 4) & 15) as i32, state);
        }
        Ok(chunk)
    }
}

impl ChunkGenerator for WasmGenerator {
    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk {
        let made = match &self.pool {
            Some(pool) => pool.generate(&self.dimension, self.seed, pos, shape),
            None => Err("no such dimension".into()),
        };
        match made.and_then(|made| self.build(shape, made)) {
            Ok(chunk) => chunk,
            Err(err) => {
                log::error!(
                    "plugin {} failed to generate {} chunk {},{}: {err}",
                    self.plugin,
                    self.dimension,
                    pos.x,
                    pos.z
                );
                Chunk::empty(shape)
            }
        }
    }

    fn sea_level(&self) -> i32 {
        self.sea_level
    }

    fn spawn(&self) -> BlockPos {
        self.spawn
    }
}

#[cfg(test)]
mod tests;
