//! The game's table-based trigonometry and small random helpers, which
//! world generation depends on bit for bit.

use std::sync::OnceLock;

use tuff_item::random::RandomSource;

const SIN_SCALE: f64 = 10430.378350470453;

fn table() -> &'static [f32; 65536] {
    static TABLE: OnceLock<Box<[f32; 65536]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Box::new([0f32; 65536]);
        for (i, v) in table.iter_mut().enumerate() {
            *v = (i as f64 / SIN_SCALE).sin() as f32;
        }
        table
    })
}

pub fn sin(x: f64) -> f32 {
    table()[((x * SIN_SCALE) as i64 & 65535) as usize]
}

pub fn cos(x: f64) -> f32 {
    table()[((x * SIN_SCALE + 16384.0) as i64 & 65535) as usize]
}

/// A checksum of the sine table, for comparison with the game's.
pub fn sin_table_crc() -> u32 {
    let mut crc = flate2::Crc::new();
    for v in table().iter() {
        crc.update(&v.to_bits().to_le_bytes());
    }
    crc.sum()
}

pub fn next_int(random: &mut impl RandomSource, min: i32, max: i32) -> i32 {
    if min >= max {
        min
    } else {
        random.next_int_bounded(max - min + 1) + min
    }
}

pub fn random_between_inclusive(random: &mut impl RandomSource, min: i32, max: i32) -> i32 {
    random.next_int_bounded(max - min + 1) + min
}

pub fn random_between(random: &mut impl RandomSource, min: f32, max: f32) -> f32 {
    random.next_float() * (max - min) + min
}

pub fn normal(random: &mut impl RandomSource, mean: f32, deviation: f32) -> f32 {
    mean + random.next_gaussian() as f32 * deviation
}

pub fn clamp_f32(v: f32, min: f32, max: f32) -> f32 {
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}
