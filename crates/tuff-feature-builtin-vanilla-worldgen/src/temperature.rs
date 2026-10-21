//! Biome temperature at a position: the biome's base temperature, the
//! frozen modifier's ice patches, and the cooling with height.

use std::sync::OnceLock;

use tuff_vanilla_data::biome::{Biome, TemperatureModifier};

use crate::noise::Simplex;
use tuff_item::random::{LegacyRandom, Random};

struct Noises {
    temperature: Simplex,
    frozen: [(Simplex, f64, f32); 3],
    info: Simplex,
}

fn noises() -> &'static Noises {
    static NOISES: OnceLock<Noises> = OnceLock::new();
    NOISES.get_or_init(|| {
        let mut frozen_random = Random::Legacy(LegacyRandom::new(3456));
        let a = Simplex::new(&mut frozen_random, true);
        let b = Simplex::new(&mut frozen_random, true);
        let c = Simplex::new(&mut frozen_random, true);
        Noises {
            temperature: Simplex::new(&mut Random::Legacy(LegacyRandom::new(1234)), true),
            frozen: [
                (a, 1.0, 0.14285715),
                (b, 0.5, 0.2857143),
                (c, 0.25, 0.5714286),
            ],
            info: Simplex::new(&mut Random::Legacy(LegacyRandom::new(2345)), true),
        }
    })
}

fn frozen_noise(x: f64, z: f64) -> f32 {
    let mut value = 0.0f32;
    for (noise, frequency, amplitude) in &noises().frozen {
        value += amplitude * noise.get_2d(x * frequency, z * frequency);
    }
    value
}

/// The temperature the game computes for a biome at a block.
pub fn at(biome: u16, x: i32, y: i32, z: i32, sea_level: i32) -> f32 {
    Column::of_biome(biome, x, z, sea_level).at(y)
}

/// The temperature at a block of a biome with the given base temperature.
pub fn at_climate(base: f32, frozen: bool, x: i32, y: i32, z: i32, sea_level: i32) -> f32 {
    Column::new(base, frozen, x, z, sea_level).at(y)
}

/// The temperature of one block column of a biome: the terms that depend
/// only on x and z are computed once for any number of heights.
pub struct Column {
    base: f32,
    x: i32,
    z: i32,
    snow_level: i32,
    cooling: Option<f32>,
}

impl Column {
    pub fn new(base: f32, frozen: bool, x: i32, z: i32, sea_level: i32) -> Self {
        let mut temperature = base;
        if frozen {
            let large = (frozen_noise(x as f64 * 0.05, z as f64 * 0.05) * 7.0) as f64;
            let edge = noises().info.get_2d(x as f64 * 0.2, z as f64 * 0.2) as f64;
            if large + edge < 0.3
                && (noises().info.get_2d(x as f64 * 0.09, z as f64 * 0.09) as f64) < 0.8
            {
                temperature = 0.2;
            }
        }
        Self {
            base: temperature,
            x,
            z,
            snow_level: sea_level + 17,
            cooling: None,
        }
    }

    pub fn of_biome(biome: u16, x: i32, z: i32, sea_level: i32) -> Self {
        Self::of_data(Biome::by_id(biome).expect("known biome"), x, z, sea_level)
    }

    pub fn of_data(data: &Biome, x: i32, z: i32, sea_level: i32) -> Self {
        Self::new(
            data.temperature,
            data.temperature_modifier == Some(TemperatureModifier::Frozen),
            x,
            z,
            sea_level,
        )
    }

    pub fn at(&mut self, y: i32) -> f32 {
        if y > self.snow_level {
            let (x, z) = (self.x, self.z);
            let v = *self.cooling.get_or_insert_with(|| {
                noises()
                    .temperature
                    .get_2d((x as f32 / 8.0) as f64, (z as f32 / 8.0) as f64)
                    * 8.0
            });
            return self.base - (v + y as f32 - self.snow_level as f32) * 0.05 / 40.0;
        }
        self.base
    }
}

/// An upper bound of the height cooling noise term: a 2D simplex value is
/// at most 3 * 70 * max over r of (0.5 - r^2)^4 * sqrt(2) * r, about 2.73,
/// and the term is eight times it.
const COOLING_BOUND: f32 = 22.0;

/// Whether every block column of a biome up to height `max_y` is at least
/// as warm as the freezing and snowing limit, whatever the noise gives.
pub fn never_freezes(biome: u16, max_y: i32, sea_level: i32) -> bool {
    Biome::by_id(biome).is_some_and(|data| data_never_freezes(data, max_y, sea_level))
}

/// [`never_freezes`] for a biome's data.
pub fn data_never_freezes(data: &Biome, max_y: i32, sea_level: i32) -> bool {
    if data.temperature_modifier == Some(TemperatureModifier::Frozen) {
        return false;
    }
    let base = data.temperature;
    let snow_level = sea_level + 17;
    let coldest = base - (COOLING_BOUND + max_y as f32 - snow_level as f32) * 0.05 / 40.0;
    base.min(coldest) >= 0.15
}

pub fn melts_frozen_ocean_iceberg(biome: u16, x: i32, y: i32, z: i32, sea_level: i32) -> bool {
    at(biome, x, y, z, sea_level) > 0.1
}

pub fn cold_enough_to_snow(biome: u16, x: i32, y: i32, z: i32, sea_level: i32) -> bool {
    at(biome, x, y, z, sea_level) < 0.15
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    fn reference(base: f32, frozen: bool, x: i32, y: i32, z: i32, sea_level: i32) -> f32 {
        let mut temperature = base;
        if frozen {
            let large = (frozen_noise(x as f64 * 0.05, z as f64 * 0.05) * 7.0) as f64;
            let edge = noises().info.get_2d(x as f64 * 0.2, z as f64 * 0.2) as f64;
            if large + edge < 0.3
                && (noises().info.get_2d(x as f64 * 0.09, z as f64 * 0.09) as f64) < 0.8
            {
                temperature = 0.2;
            }
        }
        let snow_level = sea_level + 17;
        if y > snow_level {
            let v = noises()
                .temperature
                .get_2d((x as f32 / 8.0) as f64, (z as f32 / 8.0) as f64)
                * 8.0;
            return temperature - (v + y as f32 - snow_level as f32) * 0.05 / 40.0;
        }
        temperature
    }

    #[tuff_core::watchdog]
    #[test]
    fn column_temperatures_match_the_direct_formula_bit_for_bit() {
        let mut rng = Lcg(5);
        for round in 0..3000 {
            let base = [-0.5f32, 0.0, 0.15, 0.2, 0.8, 2.0][round % 6];
            let frozen = round % 4 == 0;
            let sea_level = [63, -64, 0, 100][round % 4];
            let x = rng.next(60000) as i32 - 30000;
            let z = rng.next(60000) as i32 - 30000;
            let mut column = Column::new(base, frozen, x, z, sea_level);
            for _ in 0..4 {
                let y = rng.next(500) as i32 - 100;
                assert_eq!(
                    column.at(y).to_bits(),
                    reference(base, frozen, x, y, z, sea_level).to_bits()
                );
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn never_freezes_biomes_stay_warm_at_every_height_and_column() {
        let mut rng = Lcg(9);
        let ids = tuff_vanilla_data::data_registry::WORLDGEN_BIOME
            .entries
            .len() as u16;
        let mut skipped = 0;
        for biome in 0..ids {
            for (sea_level, max_y) in [(63, 320), (-64, 100), (0, 15), (63, 63)] {
                if !never_freezes(biome, max_y, sea_level) {
                    continue;
                }
                skipped += 1;
                for _ in 0..300 {
                    let x = rng.next(2_000_000) as i32 - 1_000_000;
                    let z = rng.next(2_000_000) as i32 - 1_000_000;
                    let y = rng.next((max_y + 66) as u64) as i32 - 64;
                    assert!(at(biome, x, y, z, sea_level) >= 0.15);
                }
            }
        }
        assert!(skipped > 0);
    }
}
