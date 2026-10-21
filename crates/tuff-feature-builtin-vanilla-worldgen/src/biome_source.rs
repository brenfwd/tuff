//! Biome sources, keyed by resource id so more kinds can be registered.
//! Biomes are the chunk format's `u16` registry ids.

use std::collections::HashMap;
use std::sync::Arc;

use crate::climate::{ClimatePoint6, Parameter, ParameterList, target};
use crate::density::{Context, DensityVolume};
use crate::loaded::biomes::PackBiomes;
use crate::state::Router;
#[cfg(test)]
use tuff_world_model::chunk::biome_id;

/// How a dimension picks biomes at quart (4-block) resolution.
pub trait BiomeSource: Send + Sync {
    /// The biomes of one chunk column, filled the way the game fills a new
    /// chunk. Indices are `qy * 16 + qz * 4 + qx`, from `min_quart_y` up.
    fn fill_chunk(
        &self,
        router: &Router,
        ctx: &mut Context,
        chunk: [i32; 2],
        min_quart_y: i32,
        quart_height: i32,
    ) -> Vec<u16>;

    /// One biome at a quart position, sampled on its own (the game's
    /// uncached resolver). `last` carries search state between calls.
    fn biome_at(
        &self,
        router: &Router,
        ctx: &mut Context,
        last: &mut Option<usize>,
        quart: [i32; 3],
    ) -> u16;

    /// Every biome the source can produce, in the game's order (which
    /// decides the global feature order).
    fn possible_biomes(&self) -> Vec<u16>;
}

/// Builds a source; biome and parameter list names resolve through the
/// world's pack biomes.
pub type BiomeSourceFactory = fn(&BiomeSourceConfig, &PackBiomes) -> Option<Arc<dyn BiomeSource>>;

/// The `biome_source` of a dimension's generator.
#[derive(Debug, Clone, PartialEq)]
pub struct BiomeSourceConfig {
    pub kind: String,
    /// `preset` for multi-noise sources, `biome` for fixed ones.
    pub preset: Option<String>,
    pub biome: Option<String>,
    /// `biomes` (ids or tags) and `scale` for checkerboard sources.
    pub biomes: Vec<String>,
    pub scale: Option<i32>,
    /// Inline climate points of a multi-noise source, used instead of
    /// `preset`.
    pub points: Vec<tuff_core::storage::level::BiomePoint>,
}

/// Biome source kinds by resource id.
pub struct BiomeSources {
    factories: HashMap<String, BiomeSourceFactory>,
}

impl BiomeSources {
    pub fn builtin() -> Self {
        let mut sources = Self {
            factories: HashMap::new(),
        };
        sources.register("minecraft:multi_noise", |config, biomes| {
            if !config.points.is_empty() {
                return Some(Arc::new(MultiNoise::from_points_in(
                    &config.points,
                    biomes,
                )?));
            }
            let preset = config.preset.as_deref()?;
            Some(Arc::new(MultiNoise::from_list(
                biomes.parameter_list(preset)?,
                biomes,
            )?))
        });
        sources.register("minecraft:the_end", |_, _| Some(Arc::new(TheEnd::new())));
        sources.register("minecraft:fixed", |config, biomes| {
            Some(Arc::new(Fixed(biomes.id(config.biome.as_deref()?)?)))
        });
        sources.register("minecraft:checkerboard", |config, biomes| {
            Checkerboard::from_config(config, biomes).map(|s| Arc::new(s) as Arc<dyn BiomeSource>)
        });
        sources
    }

    pub fn register(&mut self, kind: &str, factory: BiomeSourceFactory) {
        self.factories.insert(kind.to_string(), factory);
    }

    pub fn create(&self, config: &BiomeSourceConfig) -> Option<Arc<dyn BiomeSource>> {
        self.create_in(config, PackBiomes::none())
    }

    /// [`Self::create`] with a world's pack biomes.
    pub fn create_in(
        &self,
        config: &BiomeSourceConfig,
        biomes: &PackBiomes,
    ) -> Option<Arc<dyn BiomeSource>> {
        (self.factories.get(&config.kind)?)(config, biomes)
    }
}

pub struct MultiNoise {
    list: ParameterList<u16>,
}

impl MultiNoise {
    pub fn preset(name: &str) -> Option<Self> {
        Self::from_list(
            tuff_vanilla_data::worldgen::parameter_list(name)?,
            PackBiomes::none(),
        )
    }

    fn from_list(
        points: &[tuff_vanilla_data::worldgen::ClimatePoint],
        biomes: &PackBiomes,
    ) -> Option<Self> {
        let entries = points
            .iter()
            .map(|p| Some((ClimatePoint6::from(p), biomes.id(p.biome)?)))
            .collect::<Option<Vec<(ClimatePoint6, u16)>>>()?;
        Some(Self {
            list: ParameterList::new(&entries),
        })
    }

    /// A source built from inline climate points; `None` when a point
    /// names an unknown biome.
    pub fn from_points(points: &[tuff_core::storage::level::BiomePoint]) -> Option<Self> {
        Self::from_points_in(points, PackBiomes::none())
    }

    /// [`Self::from_points`], where points may name pack-added biomes.
    pub fn from_points_in(
        points: &[tuff_core::storage::level::BiomePoint],
        biomes: &PackBiomes,
    ) -> Option<Self> {
        let quantize = |(min, max): (f32, f32)| Parameter {
            min: crate::climate::quantize(min),
            max: crate::climate::quantize(max),
        };
        let entries = points
            .iter()
            .map(|p| {
                let point = ClimatePoint6 {
                    ranges: p.ranges.map(quantize),
                    offset: crate::climate::quantize(p.offset),
                };
                Some((point, biomes.id(&p.biome)?))
            })
            .collect::<Option<Vec<(ClimatePoint6, u16)>>>()?;
        Some(Self {
            list: ParameterList::new(&entries),
        })
    }

    pub fn list(&self) -> &ParameterList<u16> {
        &self.list
    }
}

impl BiomeSource for MultiNoise {
    fn fill_chunk(
        &self,
        router: &Router,
        ctx: &mut Context,
        chunk: [i32; 2],
        min_quart_y: i32,
        quart_height: i32,
    ) -> Vec<u16> {
        let volume = DensityVolume::stepped(
            [4, quart_height, 4],
            [chunk[0] << 4, min_quart_y << 2, chunk[1] << 4],
            [4, 4, 4],
        );
        let samplers = [
            &router.temperature,
            &router.vegetation,
            &router.continents,
            &router.erosion,
            &router.depth,
            &router.ridges,
        ];
        let buffers = samplers.map(|sampler| {
            let mut buffer = ctx.acquire(volume.size());
            sampler.sample_volume(ctx, &mut buffer, &volume);
            buffer
        });
        let mut out = vec![0u16; (16 * quart_height) as usize];
        let mut last = None;
        for section in 0..quart_height / 4 {
            for x in 0..4 {
                for y in 0..4 {
                    let qy = section * 4 + y;
                    for z in 0..4 {
                        let index = volume.index(x, qy, z);
                        let t = target([
                            buffers[0][index],
                            buffers[1][index],
                            buffers[2][index],
                            buffers[3][index],
                            buffers[4][index],
                            buffers[5][index],
                        ]);
                        out[(qy * 16 + z * 4 + x) as usize] = self.list.find(&t, &mut last);
                    }
                }
            }
        }
        for buffer in buffers {
            ctx.release(buffer);
        }
        out
    }

    fn biome_at(
        &self,
        router: &Router,
        ctx: &mut Context,
        last: &mut Option<usize>,
        quart: [i32; 3],
    ) -> u16 {
        let [x, y, z] = quart.map(|q| q << 2);
        let t = target([
            router.temperature.sample_value(ctx, x, y, z),
            router.vegetation.sample_value(ctx, x, y, z),
            router.continents.sample_value(ctx, x, y, z),
            router.erosion.sample_value(ctx, x, y, z),
            router.depth.sample_value(ctx, x, y, z),
            router.ridges.sample_value(ctx, x, y, z),
        ]);
        self.list.find(&t, last)
    }

    fn possible_biomes(&self) -> Vec<u16> {
        let mut biomes = Vec::new();
        for biome in self.list.values() {
            if !biomes.contains(biome) {
                biomes.push(*biome);
            }
        }
        biomes
    }
}

/// The end: the central island, then highlands, midlands, small islands
/// or barrens by the island noise.
pub struct TheEnd {
    end: u16,
    highlands: u16,
    midlands: u16,
    islands: u16,
    barrens: u16,
}

impl TheEnd {
    pub fn new() -> Self {
        let id = |name| tuff_world_model::chunk::biome_id(name).expect("end biome");
        Self {
            end: id("minecraft:the_end"),
            highlands: id("minecraft:end_highlands"),
            midlands: id("minecraft:end_midlands"),
            islands: id("minecraft:small_end_islands"),
            barrens: id("minecraft:end_barrens"),
        }
    }
}

impl Default for TheEnd {
    fn default() -> Self {
        Self::new()
    }
}

impl BiomeSource for TheEnd {
    fn fill_chunk(
        &self,
        router: &Router,
        ctx: &mut Context,
        chunk: [i32; 2],
        min_quart_y: i32,
        quart_height: i32,
    ) -> Vec<u16> {
        let mut out = vec![0u16; (16 * quart_height) as usize];
        let mut last = None;
        for section in 0..quart_height / 4 {
            for x in 0..4 {
                for y in 0..4 {
                    let qy = section * 4 + y;
                    for z in 0..4 {
                        let quart = [(chunk[0] << 2) + x, min_quart_y + qy, (chunk[1] << 2) + z];
                        out[(qy * 16 + z * 4 + x) as usize] =
                            self.biome_at(router, ctx, &mut last, quart);
                    }
                }
            }
        }
        out
    }

    fn biome_at(
        &self,
        router: &Router,
        ctx: &mut Context,
        _last: &mut Option<usize>,
        quart: [i32; 3],
    ) -> u16 {
        let [x, y, z] = quart.map(|q| q << 2);
        let (cx, cz) = ((x >> 4) as i64, (z >> 4) as i64);
        if cx * cx + cz * cz <= 4096 {
            return self.end;
        }
        let wx = ((x >> 4) * 2 + 1) * 8;
        let wz = ((z >> 4) * 2 + 1) * 8;
        let height = router.erosion.sample_value(ctx, wx, y, wz) as f64;
        if height > 0.25 {
            self.highlands
        } else if height >= -0.0625 {
            self.midlands
        } else if height < -0.21875 {
            self.islands
        } else {
            self.barrens
        }
    }

    fn possible_biomes(&self) -> Vec<u16> {
        vec![
            self.end,
            self.highlands,
            self.midlands,
            self.islands,
            self.barrens,
        ]
    }
}

/// A single biome everywhere.
pub struct Fixed(pub u16);

/// Alternating squares of the listed biomes, `4 << scale` blocks wide.
pub struct Checkerboard {
    biomes: Vec<u16>,
    shift: u32,
}

impl Checkerboard {
    pub fn new(biomes: Vec<u16>, scale: i32) -> Option<Self> {
        ((0..=62).contains(&scale) && !biomes.is_empty()).then_some(Self {
            biomes,
            shift: scale as u32 + 2,
        })
    }

    fn from_config(config: &BiomeSourceConfig, known: &PackBiomes) -> Option<Self> {
        let mut biomes = Vec::new();
        for entry in &config.biomes {
            match entry.strip_prefix('#') {
                Some(tag) => {
                    let names = tuff_datapack::tags::with(|tags| {
                        tags.get("minecraft:worldgen/biome", tag)
                            .map_or_else(Vec::new, <[String]>::to_vec)
                    });
                    for name in names {
                        biomes.push(known.id(&name)?);
                    }
                }
                None => biomes.push(known.id(entry)?),
            }
        }
        Self::new(biomes, config.scale.unwrap_or(2))
    }

    fn at(&self, quart_x: i32, quart_z: i32) -> u16 {
        let sum = (quart_x >> self.shift).wrapping_add(quart_z >> self.shift);
        self.biomes[sum.rem_euclid(self.biomes.len() as i32) as usize]
    }
}

impl BiomeSource for Checkerboard {
    fn fill_chunk(
        &self,
        _router: &Router,
        _ctx: &mut Context,
        chunk: [i32; 2],
        _min_quart_y: i32,
        quart_height: i32,
    ) -> Vec<u16> {
        let mut out = vec![0u16; (16 * quart_height) as usize];
        for qy in 0..quart_height {
            for z in 0..4 {
                for x in 0..4 {
                    out[(qy * 16 + z * 4 + x) as usize] =
                        self.at(chunk[0] * 4 + x, chunk[1] * 4 + z);
                }
            }
        }
        out
    }

    fn biome_at(&self, _: &Router, _: &mut Context, _: &mut Option<usize>, quart: [i32; 3]) -> u16 {
        self.at(quart[0], quart[2])
    }

    fn possible_biomes(&self) -> Vec<u16> {
        let mut out = Vec::new();
        for biome in &self.biomes {
            if !out.contains(biome) {
                out.push(*biome);
            }
        }
        out
    }
}

impl BiomeSource for Fixed {
    fn fill_chunk(
        &self,
        _router: &Router,
        _ctx: &mut Context,
        _chunk: [i32; 2],
        _min_quart_y: i32,
        quart_height: i32,
    ) -> Vec<u16> {
        vec![self.0; (16 * quart_height) as usize]
    }

    fn biome_at(&self, _: &Router, _: &mut Context, _: &mut Option<usize>, _: [i32; 3]) -> u16 {
        self.0
    }

    fn possible_biomes(&self) -> Vec<u16> {
        vec![self.0]
    }
}

#[cfg(test)]
mod checkerboard_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn squares_alternate_by_the_scaled_quart_sum() {
        let (a, b, c) = (
            biome_id("minecraft:plains").unwrap(),
            biome_id("minecraft:desert").unwrap(),
            biome_id("minecraft:forest").unwrap(),
        );
        let board = Checkerboard::new(vec![a, b], 0).unwrap();
        assert_eq!(board.at(0, 0), a);
        assert_eq!(board.at(3, 0), a);
        assert_eq!(board.at(4, 0), b);
        assert_eq!(board.at(4, 4), a);
        assert_eq!(board.at(-1, 0), b);
        assert_eq!(board.at(-5, -1), b);
        let board = Checkerboard::new(vec![a, b, c], 1).unwrap();
        assert_eq!(board.at(16, 0), c);
        assert_eq!(board.possible_biomes(), vec![a, b, c]);
        assert!(Checkerboard::new(vec![], 2).is_none());
        assert!(Checkerboard::new(vec![a], 63).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_source_is_built_from_its_config() {
        let config = BiomeSourceConfig {
            kind: "minecraft:checkerboard".into(),
            preset: None,
            biome: None,
            biomes: vec!["minecraft:plains".into(), "minecraft:desert".into()],
            scale: None,
            points: Vec::new(),
        };
        let source = BiomeSources::builtin().create(&config).unwrap();
        assert_eq!(source.possible_biomes().len(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn multi_noise_sources_accept_inline_climate_points() {
        use tuff_core::storage::level::BiomePoint;
        let point = |biome: &str| BiomePoint {
            biome: biome.into(),
            ranges: [(-2.0, 2.0); 6],
            offset: 0.0,
        };
        let config = BiomeSourceConfig {
            kind: "minecraft:multi_noise".into(),
            preset: None,
            biome: None,
            biomes: Vec::new(),
            scale: None,
            points: vec![point("minecraft:plains"), point("minecraft:desert")],
        };
        let source = BiomeSources::builtin().create(&config).unwrap();
        assert_eq!(
            source.possible_biomes(),
            vec![
                biome_id("minecraft:plains").unwrap(),
                biome_id("minecraft:desert").unwrap()
            ]
        );
        let unknown = BiomeSourceConfig {
            points: vec![point("minecraft:nowhere")],
            ..config
        };
        assert!(BiomeSources::builtin().create(&unknown).is_none());
    }
}
