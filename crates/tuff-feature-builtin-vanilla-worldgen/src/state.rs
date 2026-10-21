//! Everything a dimension's generator derives from the world seed and its
//! noise settings: the positional random factory and the compiled noise
//! router. Built once, then shared read-only by every generation task.

use std::sync::Arc;

use tuff_vanilla_data::worldgen::NoiseSettings;

use crate::data::Loader;
use crate::density::{Compiler, Df, Sampler};
use tuff_item::random::{Algorithm, PositionalRandom, RandomSource};

/// The compiled noise router.
pub struct Router {
    pub temperature: Arc<Sampler>,
    pub vegetation: Arc<Sampler>,
    pub continents: Arc<Sampler>,
    pub erosion: Arc<Sampler>,
    pub depth: Arc<Sampler>,
    pub ridges: Arc<Sampler>,
    pub chunk_surface_level: Arc<Sampler>,
    pub final_density: Arc<Sampler>,
}

pub struct RandomState {
    pub seed: i64,
    pub random: PositionalRandom,
    pub legacy: bool,
    pub router: Router,
    compiler: Compiler,
    loader: Loader,
    options: crate::loaded::Options,
}

impl RandomState {
    pub fn new(settings: &'static NoiseSettings, seed: i64) -> Self {
        Self::with_options(settings, seed, crate::loaded::Options::default())
    }

    /// A state for settings decoded from data packs.
    pub fn with_options(
        settings: &'static NoiseSettings,
        seed: i64,
        options: crate::loaded::Options,
    ) -> Self {
        let algorithm = if settings.legacy_random_source {
            Algorithm::Legacy
        } else {
            Algorithm::Xoroshiro
        };
        let random = algorithm.new_instance(seed).fork_positional();
        let mut compiler = Compiler::new(seed, settings.legacy_random_source, random);
        compiler.set_interpret(options.interpret);
        let mut loader = Loader::default();
        let r = &settings.router;
        let mut compile = |f| Arc::new(compiler.compile(&loader.function(f)));
        let router = Router {
            temperature: compile(r.temperature),
            vegetation: compile(r.vegetation),
            continents: compile(r.continents),
            erosion: compile(r.erosion),
            depth: compile(r.depth),
            ridges: compile(r.ridges),
            chunk_surface_level: compile(r.chunk_surface_level),
            final_density: compile(r.final_density),
        };
        Self {
            seed,
            random,
            legacy: settings.legacy_random_source,
            router,
            compiler,
            loader,
            options,
        }
    }

    /// Compiles another function from the data against this state's
    /// noises and caches.
    pub fn compile(
        &mut self,
        function: &'static tuff_vanilla_data::worldgen::DensityFunction,
    ) -> Arc<Sampler> {
        let function = self.loader.function(function);
        Arc::new(self.compiler.compile(&function))
    }

    pub fn compile_runtime(&mut self, function: &Df) -> Arc<Sampler> {
        Arc::new(self.compiler.compile(function))
    }

    pub fn loader(&mut self) -> &mut Loader {
        &mut self.loader
    }

    pub fn options(&self) -> &crate::loaded::Options {
        &self.options
    }

    pub fn compiler(&mut self) -> &mut Compiler {
        &mut self.compiler
    }

    /// A named positional factory derived from the world random, as the
    /// game makes for aquifers, ore veins and surface rules.
    pub fn random_factory(&self, name: &str) -> PositionalRandom {
        self.random.from_hash_of(name).fork_positional()
    }
}
