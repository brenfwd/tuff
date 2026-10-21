use std::sync::Arc;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::storage::level::DimensionSettings;
use tuff_core::world::generator::{ChunkGenerator, Void};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{
    BIOME_VOLUME, Chunk, ChunkShape, SECTION_VOLUME, Section, biome_id, plains,
};
use tuff_world_model::pos::ChunkPos;

/// The placeholder for a dimension's settings.
pub fn for_settings(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
    match (settings.generator.as_str(), settings.settings.as_deref()) {
        ("minecraft:noise", Some("minecraft:nether")) => Arc::new(Nether::new(seed)),
        ("minecraft:noise", Some("minecraft:end")) => Arc::new(End::new(seed)),
        ("minecraft:noise", Some(_)) => Arc::new(Overworld::new(seed)),
        _ => Arc::new(Void),
    }
}

/// `(amplitude, x wavelength, z wavelength)` of each wave; `x * z` terms
/// make hills, and the long waves roll whole landscapes up and down.
const WAVES: [(f64, f64, f64); 5] = [
    (14.0, 233.0, 251.0),
    (10.0, 97.0, 89.0),
    (6.0, 41.0, 37.0),
    (3.0, 19.0, 23.0),
    (1.5, 11.0, 13.0),
];

#[derive(Debug, Clone)]
struct Waves {
    phases: [(f64, f64); WAVES.len()],
}

impl Waves {
    fn new(rng: &mut StdRng) -> Self {
        let mut phases = [(0.0, 0.0); WAVES.len()];
        for phase in &mut phases {
            *phase = (
                rng.gen_range(0.0..std::f64::consts::TAU),
                rng.gen_range(0.0..std::f64::consts::TAU),
            );
        }
        Self { phases }
    }

    fn at(&self, x: i32, z: i32) -> f64 {
        let (x, z) = (x as f64, z as f64);
        WAVES
            .iter()
            .zip(&self.phases)
            .map(|(&(amplitude, wx, wz), &(px, pz))| {
                amplitude * (x / wx + px).sin() * (z / wz + pz).cos()
            })
            .sum()
    }
}

fn build(
    shape: ChunkShape,
    biome: u16,
    mut uniform: impl FnMut(i32, i32) -> Option<BlockState>,
    mut block: impl FnMut(i32, i32, i32) -> BlockState,
) -> Chunk {
    let sections = (0..shape.sections())
        .map(|index| {
            let bottom = shape.section_base(index);
            if let Some(state) = uniform(bottom, bottom + 15) {
                return Section::uniform(state, biome);
            }
            let mut blocks = Box::new([Block::Air.default_state(); SECTION_VOLUME]);
            for (i, state) in blocks.iter_mut().enumerate() {
                let (x, y, z) = ((i & 15) as i32, (i >> 8) as i32, ((i >> 4) & 15) as i32);
                *state = block(x, bottom + y, z);
            }
            Section::new(blocks, Box::new([biome; BIOME_VOLUME]))
        })
        .collect();
    Chunk::from_sections(shape, sections, None)
}

/// The layered bedrock/stone/dirt/grass column shaped by a seeded field of
/// sine and cosine waves, with sand and water below sea level.
#[derive(Debug, Clone)]
pub struct Overworld {
    waves: Waves,
}

impl Overworld {
    pub const SEA_LEVEL: i32 = 63;
    const BASE_HEIGHT: f64 = 66.0;
    pub const SOIL_DEPTH: i32 = 3;

    pub fn new(seed: i64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed as u64);
        Self {
            waves: Waves::new(&mut rng),
        }
    }

    /// Y of the topmost solid block in the column.
    pub fn height(&self, x: i32, z: i32) -> i32 {
        (Self::BASE_HEIGHT + self.waves.at(x, z)).round() as i32
    }

    fn column_block(y: i32, height: i32, min_y: i32) -> BlockState {
        let underwater = height < Self::SEA_LEVEL;
        let block = if y == min_y {
            Block::Bedrock
        } else if y < height - Self::SOIL_DEPTH {
            Block::Stone
        } else if y < height {
            if underwater { Block::Sand } else { Block::Dirt }
        } else if y == height {
            if underwater {
                Block::Sand
            } else {
                Block::GrassBlock
            }
        } else if y < Self::SEA_LEVEL {
            Block::Water
        } else {
            Block::Air
        };
        block.default_state()
    }
}

impl ChunkGenerator for Overworld {
    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk {
        let mut heights = [0i32; 256];
        for (i, height) in heights.iter_mut().enumerate() {
            *height = self.height(pos.x * 16 + (i & 15) as i32, pos.z * 16 + (i >> 4) as i32);
        }
        let lowest = *heights.iter().min().unwrap();
        let highest = (*heights.iter().max().unwrap()).max(Self::SEA_LEVEL - 1);
        let min_y = shape.min_y;
        build(
            shape,
            plains(),
            |bottom, top| {
                if bottom > min_y && top < lowest - Self::SOIL_DEPTH {
                    Some(Block::Stone.default_state())
                } else if bottom > highest {
                    Some(Block::Air.default_state())
                } else {
                    None
                }
            },
            |x, y, z| Self::column_block(y, heights[(z << 4 | x) as usize], min_y),
        )
    }

    fn sea_level(&self) -> i32 {
        Self::SEA_LEVEL
    }

    /// On the surface (or water) at the origin.
    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, self.height(0, 0).max(Self::SEA_LEVEL - 1) + 1, 0)
    }
}

/// Netherrack riddled with wave-carved caverns, a lava sea at sea level,
/// and a bedrock floor and roof at the ends of the logical height.
#[derive(Debug, Clone)]
pub struct Nether {
    floor: Waves,
    ceiling: Waves,
    caves: Waves,
}

impl Nether {
    pub const SEA_LEVEL: i32 = 32;
    /// The roof sits at the top of the dimension's logical height.
    pub const ROOF: i32 = 127;
    const FLOOR_BASE: f64 = 30.0;
    const CEILING_BASE: f64 = 92.0;

    pub fn new(seed: i64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed as u64 ^ 0x4E45_5448);
        Self {
            floor: Waves::new(&mut rng),
            ceiling: Waves::new(&mut rng),
            caves: Waves::new(&mut rng),
        }
    }

    /// Top of the ground under the caverns.
    pub fn floor_height(&self, x: i32, z: i32) -> i32 {
        (Self::FLOOR_BASE + self.floor.at(x, z) * 0.4).round() as i32
    }

    /// Bottom of the netherrack hanging from the roof.
    pub fn ceiling_height(&self, x: i32, z: i32) -> i32 {
        (Self::CEILING_BASE + self.ceiling.at(x, z) * 0.4).round() as i32
    }

    fn is_tunnel(&self, x: i32, y: i32, z: i32) -> bool {
        let field = self.caves.at(x, z) + self.caves.at(z * 3, y * 5);
        field > 12.0
    }

    fn block(&self, x: i32, y: i32, z: i32, min_y: i32, floor: i32, ceiling: i32) -> BlockState {
        let open = (y > floor && y < ceiling)
            || (y > min_y + 4 && y < Self::ROOF - 4 && self.is_tunnel(x, y, z));
        let block = if y == min_y || y == Self::ROOF {
            Block::Bedrock
        } else if y > Self::ROOF {
            Block::Air
        } else if open && y < Self::SEA_LEVEL {
            Block::Lava
        } else if open {
            Block::Air
        } else {
            Block::Netherrack
        };
        block.default_state()
    }
}

impl ChunkGenerator for Nether {
    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk {
        let mut floors = [0i32; 256];
        let mut ceilings = [0i32; 256];
        for i in 0..256 {
            let (x, z) = (pos.x * 16 + (i & 15) as i32, pos.z * 16 + (i >> 4) as i32);
            floors[i] = self.floor_height(x, z);
            ceilings[i] = self.ceiling_height(x, z);
        }
        let min_y = shape.min_y;
        let biome = biome_id("minecraft:nether_wastes").unwrap_or_else(plains);
        build(
            shape,
            biome,
            |bottom, _| (bottom > Self::ROOF).then(|| Block::Air.default_state()),
            |x, y, z| {
                let i = (z << 4 | x) as usize;
                self.block(
                    pos.x * 16 + x,
                    y,
                    pos.z * 16 + z,
                    min_y,
                    floors[i],
                    ceilings[i],
                )
            },
        )
    }

    fn sea_level(&self) -> i32 {
        Self::SEA_LEVEL
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, self.floor_height(0, 0) + 1, 0)
    }
}

/// An end stone island around the origin, and the obsidian spawn platform
/// the game builds at (100, 48, 0).
#[derive(Debug, Clone)]
pub struct End {
    edge: Waves,
}

impl End {
    pub const SEA_LEVEL: i32 = 0;
    pub const PLATFORM: BlockPos = BlockPos::new(100, 48, 0);
    const RADIUS: f64 = 90.0;
    const SURFACE: f64 = 64.0;

    pub fn new(seed: i64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed as u64 ^ 0x0045_4E44);
        Self {
            edge: Waves::new(&mut rng),
        }
    }

    /// The island's surface and floor y in a column, if it exists there.
    pub fn island(&self, x: i32, z: i32) -> Option<(i32, i32)> {
        let radius = Self::RADIUS + self.edge.at(x, z);
        let distance = ((x * x + z * z) as f64).sqrt();
        if distance >= radius {
            return None;
        }
        let fraction = distance / radius;
        let thickness = (1.0 - fraction * fraction) * 24.0;
        let top = (Self::SURFACE - fraction * fraction * 8.0).round() as i32;
        Some((top, top - thickness.round() as i32))
    }

    fn platform_block(x: i32, y: i32, z: i32) -> Option<BlockState> {
        let platform = Self::PLATFORM;
        if (x - platform.x).abs() > 2 || (z - platform.z).abs() > 2 {
            return None;
        }
        if y == platform.y {
            Some(Block::Obsidian.default_state())
        } else if y > platform.y && y <= platform.y + 3 {
            Some(Block::Air.default_state())
        } else {
            None
        }
    }
}

impl ChunkGenerator for End {
    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk {
        let mut columns = [None; 256];
        for (i, column) in columns.iter_mut().enumerate() {
            *column = self.island(pos.x * 16 + (i & 15) as i32, pos.z * 16 + (i >> 4) as i32);
        }
        let highest = columns.iter().flatten().map(|c| c.0).max();
        let lowest = columns.iter().flatten().map(|c| c.1).min();
        let overlaps = |chunk: i32, center: i32| {
            (chunk * 16..chunk * 16 + 16).any(|c| (c - center).abs() <= 2)
        };
        let has_platform = overlaps(pos.x, Self::PLATFORM.x) && overlaps(pos.z, Self::PLATFORM.z);
        let biome = biome_id("minecraft:the_end").unwrap_or_else(plains);
        build(
            shape,
            biome,
            |bottom, top| {
                let empty = match (highest, lowest) {
                    (Some(highest), Some(lowest)) => bottom > highest || top < lowest,
                    _ => true,
                };
                let platform = has_platform
                    && (Self::PLATFORM.y..=Self::PLATFORM.y + 3)
                        .any(|y| (bottom..=top).contains(&y));
                (empty && !platform).then(|| Block::Air.default_state())
            },
            |x, y, z| {
                let (ax, az) = (pos.x * 16 + x, pos.z * 16 + z);
                if has_platform && let Some(state) = Self::platform_block(ax, y, az) {
                    return state;
                }
                match columns[(z << 4 | x) as usize] {
                    Some((top, bottom)) if y <= top && y >= bottom => {
                        Block::EndStone.default_state()
                    }
                    _ => Block::Air.default_state(),
                }
            },
        )
    }

    fn sea_level(&self) -> i32 {
        Self::SEA_LEVEL
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(Self::PLATFORM.x, Self::PLATFORM.y + 1, Self::PLATFORM.z)
    }
}

/// Horizontal layers of one block each up to an inclusive top y, air
/// above the last, all in one biome: flat oceans, flooded caves.
#[derive(Debug, Clone)]
pub struct Layered {
    pub layers: Vec<(i32, BlockState)>,
    pub biome: &'static str,
    pub sea_level: i32,
}

impl Layered {
    fn at(&self, y: i32) -> BlockState {
        self.layers
            .iter()
            .find(|(top, _)| y <= *top)
            .map_or(Block::Air.default_state(), |(_, state)| *state)
    }
}

impl ChunkGenerator for Layered {
    fn generate(&self, _pos: ChunkPos, shape: ChunkShape) -> Chunk {
        let biome = biome_id(self.biome).unwrap_or_else(plains);
        build(
            shape,
            biome,
            |bottom, top| {
                let state = self.at(bottom);
                (self.at(top) == state).then_some(state)
            },
            |_, y, _| self.at(y),
        )
    }

    fn sea_level(&self) -> i32 {
        self.sea_level
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, self.layers.last().map_or(0, |(top, _)| top + 1), 0)
    }
}
