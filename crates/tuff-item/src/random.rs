//! The game's world generation random sources, reproduced bit for bit: the
//! 48-bit linear congruential generator, Xoroshiro128++, the positional
//! factories that derive independent streams from coordinates or names, and
//! the decoration seeding world generation layers on top.

use md5::{Digest, Md5};

const GOLDEN_RATIO_64: i64 = -7046029254386353131;
const SILVER_RATIO_64: i64 = 7640891576956012809;

const LCG_MULTIPLIER: i64 = 0x5DEECE66D;
const LCG_MASK: i64 = (1 << 48) - 1;

#[inline]
fn mix_stafford13(z: i64) -> i64 {
    let z = (z ^ ((z as u64) >> 30) as i64).wrapping_mul(-4658895280553007687);
    let z = (z ^ ((z as u64) >> 27) as i64).wrapping_mul(-7723592293110705685);
    z ^ ((z as u64) >> 31) as i64
}

/// A 128-bit seed split into two halves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seed128 {
    pub lo: i64,
    pub hi: i64,
}

impl Seed128 {
    #[inline]
    pub fn upgrade(seed: i64) -> Self {
        let lo = seed ^ SILVER_RATIO_64;
        let hi = lo.wrapping_add(GOLDEN_RATIO_64);
        Self {
            lo: mix_stafford13(lo),
            hi: mix_stafford13(hi),
        }
    }

    /// The seed of a named random sequence: the unmixed upgrade of `seed`,
    /// xored with the hash of `key` when given, then mixed.
    #[inline]
    pub fn for_sequence(seed: i64, key: Option<&str>) -> Self {
        let lo = seed ^ SILVER_RATIO_64;
        let hi = lo.wrapping_add(GOLDEN_RATIO_64);
        let (lo, hi) = match key {
            Some(key) => {
                let hash = Self::from_hash_of(key);
                (lo ^ hash.lo, hi ^ hash.hi)
            }
            None => (lo, hi),
        };
        Self {
            lo: mix_stafford13(lo),
            hi: mix_stafford13(hi),
        }
    }

    /// The MD5 digest of `name`, read as two big-endian longs.
    #[inline]
    pub fn from_hash_of(name: &str) -> Self {
        let digest = Md5::digest(name.as_bytes());
        Self {
            lo: i64::from_be_bytes(digest[..8].try_into().unwrap()),
            hi: i64::from_be_bytes(digest[8..].try_into().unwrap()),
        }
    }
}

/// The game's positional hash of block coordinates.
#[inline]
pub fn position_seed(x: i32, y: i32, z: i32) -> i64 {
    let seed = (x.wrapping_mul(3129871) as i64) ^ (z as i64).wrapping_mul(116129781) ^ y as i64;
    seed.wrapping_mul(seed)
        .wrapping_mul(42317861)
        .wrapping_add(seed.wrapping_mul(11))
        >> 16
}

/// Java's `String.hashCode`, over UTF-16 code units.
#[inline]
pub fn java_string_hash(text: &str) -> i32 {
    text.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(unit as i32)
    })
}

/// Operations every generator offers. The bounded and floating point
/// helpers are defined on top of each generator's primitive output exactly
/// as the game defines them, so they differ between generator kinds.
pub trait RandomSource {
    fn next_int(&mut self) -> i32;
    fn next_int_bounded(&mut self, bound: i32) -> i32;
    fn next_long(&mut self) -> i64;
    fn next_bool(&mut self) -> bool;
    fn next_float(&mut self) -> f32;
    fn next_double(&mut self) -> f64;
    fn next_gaussian(&mut self) -> f64;
    fn set_seed(&mut self, seed: i64);
    fn fork(&mut self) -> Random;
    fn fork_positional(&mut self) -> PositionalRandom;

    #[inline]
    fn consume_count(&mut self, rounds: usize) {
        for _ in 0..rounds {
            self.next_int();
        }
    }

    #[inline]
    fn next_int_between_inclusive(&mut self, min: i32, max: i32) -> i32 {
        self.next_int_bounded(max - min + 1) + min
    }

    #[inline]
    fn next_int_range(&mut self, origin: i32, bound: i32) -> i32 {
        origin + self.next_int_bounded(bound - origin)
    }

    #[inline]
    fn triangle(&mut self, mean: f64, spread: f64) -> f64 {
        mean + spread * (self.next_double() - self.next_double())
    }
}

/// The polar method with one cached value, as the game draws normals.
#[derive(Debug, Clone, Copy, Default)]
struct Gaussian {
    next: Option<f64>,
}

impl Gaussian {
    #[inline]
    fn next(&mut self, uniform: &mut impl FnMut() -> f64) -> f64 {
        if let Some(value) = self.next.take() {
            return value;
        }
        loop {
            let x = 2.0 * uniform() - 1.0;
            let y = 2.0 * uniform() - 1.0;
            let radius = x * x + y * y;
            if radius < 1.0 && radius != 0.0 {
                let multiplier = (-2.0 * radius.ln() / radius).sqrt();
                self.next = Some(y * multiplier);
                return x * multiplier;
            }
        }
    }
}

/// Generators built from a primitive that yields `bits` random bits.
trait BitSource {
    fn next_bits(&mut self, bits: u32) -> i32;
}

#[inline]
fn bit_int_bounded(source: &mut impl BitSource, bound: i32) -> i32 {
    assert!(bound > 0, "bound must be positive");
    if bound & (bound - 1) == 0 {
        return ((bound as i64 * source.next_bits(31) as i64) >> 31) as i32;
    }
    loop {
        let sample = source.next_bits(31);
        let modulo = sample % bound;
        if sample.wrapping_sub(modulo).wrapping_add(bound - 1) >= 0 {
            return modulo;
        }
    }
}

#[inline]
fn bit_long(source: &mut impl BitSource) -> i64 {
    let upper = source.next_bits(32) as i64;
    let lower = source.next_bits(32) as i64;
    (upper << 32).wrapping_add(lower)
}

#[inline]
fn bit_float(source: &mut impl BitSource) -> f32 {
    source.next_bits(24) as f32 * 5.9604645E-8
}

#[inline]
fn bit_double(source: &mut impl BitSource) -> f64 {
    let upper = source.next_bits(26) as i64;
    let lower = source.next_bits(27) as i64;
    ((upper << 27) + lower) as f64 * 1.1102230246251565E-16
}

/// `java.util.Random`'s 48-bit linear congruential generator.
#[derive(Debug, Clone)]
pub struct LegacyRandom {
    seed: i64,
    gaussian: Gaussian,
}

impl LegacyRandom {
    #[inline]
    pub fn new(seed: i64) -> Self {
        Self {
            seed: (seed ^ LCG_MULTIPLIER) & LCG_MASK,
            gaussian: Gaussian::default(),
        }
    }

    #[inline]
    pub fn next(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(LCG_MULTIPLIER).wrapping_add(11) & LCG_MASK;
        (self.seed >> (48 - bits)) as i32
    }
}

impl BitSource for LegacyRandom {
    #[inline]
    fn next_bits(&mut self, bits: u32) -> i32 {
        self.next(bits)
    }
}

impl RandomSource for LegacyRandom {
    #[inline]
    fn next_int(&mut self) -> i32 {
        self.next(32)
    }

    #[inline]
    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        bit_int_bounded(self, bound)
    }

    #[inline]
    fn next_long(&mut self) -> i64 {
        bit_long(self)
    }

    #[inline]
    fn next_bool(&mut self) -> bool {
        self.next(1) != 0
    }

    #[inline]
    fn next_float(&mut self) -> f32 {
        bit_float(self)
    }

    #[inline]
    fn next_double(&mut self) -> f64 {
        bit_double(self)
    }

    #[inline]
    fn next_gaussian(&mut self) -> f64 {
        let mut gaussian = self.gaussian;
        let value = gaussian.next(&mut || self.next_double());
        self.gaussian = gaussian;
        value
    }

    #[inline]
    fn set_seed(&mut self, seed: i64) {
        *self = Self::new(seed);
    }

    #[inline]
    fn fork(&mut self) -> Random {
        Random::Legacy(LegacyRandom::new(self.next_long()))
    }

    #[inline]
    fn fork_positional(&mut self) -> PositionalRandom {
        PositionalRandom::Legacy(self.next_long())
    }
}

/// Xoroshiro128++ with the game's derivation of ints, floats and doubles.
#[derive(Debug, Clone)]
pub struct XoroshiroRandom {
    lo: i64,
    hi: i64,
    gaussian: Gaussian,
}

impl XoroshiroRandom {
    #[inline]
    pub fn new(seed: i64) -> Self {
        Self::from_seed(Seed128::upgrade(seed))
    }

    #[inline]
    pub fn from_seed(seed: Seed128) -> Self {
        Self::from_parts(seed.lo, seed.hi)
    }

    #[inline]
    pub fn from_parts(lo: i64, hi: i64) -> Self {
        let (lo, hi) = if lo | hi == 0 {
            (GOLDEN_RATIO_64, SILVER_RATIO_64)
        } else {
            (lo, hi)
        };
        Self {
            lo,
            hi,
            gaussian: Gaussian::default(),
        }
    }

    #[inline]
    pub fn state(&self) -> (i64, i64) {
        (self.lo, self.hi)
    }

    #[inline]
    fn raw(&mut self) -> i64 {
        let (s0, s1) = (self.lo, self.hi);
        let result = s0.wrapping_add(s1).rotate_left(17).wrapping_add(s0);
        let s2 = s1 ^ s0;
        self.lo = s0.rotate_left(49) ^ s2 ^ (s2 << 21);
        self.hi = s2.rotate_left(28);
        result
    }

    #[inline]
    fn bits(&mut self, bits: u32) -> i64 {
        ((self.raw() as u64) >> (64 - bits)) as i64
    }
}

impl RandomSource for XoroshiroRandom {
    #[inline]
    fn next_int(&mut self) -> i32 {
        self.raw() as i32
    }

    #[inline]
    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        let bound = bound as u32 as u64;
        let mut product = (self.next_int() as u32 as u64) * bound;
        let mut fraction = product & 0xFFFF_FFFF;
        if fraction < bound {
            let threshold = (bound.wrapping_neg() as u32 % bound as u32) as u64;
            while fraction < threshold {
                product = (self.next_int() as u32 as u64) * bound;
                fraction = product & 0xFFFF_FFFF;
            }
        }
        (product >> 32) as i32
    }

    #[inline]
    fn next_long(&mut self) -> i64 {
        self.raw()
    }

    #[inline]
    fn next_bool(&mut self) -> bool {
        self.raw() & 1 != 0
    }

    #[inline]
    fn next_float(&mut self) -> f32 {
        self.bits(24) as f32 * 5.9604645E-8
    }

    #[inline]
    fn next_double(&mut self) -> f64 {
        self.bits(53) as f64 * 1.1102230246251565E-16
    }

    #[inline]
    fn next_gaussian(&mut self) -> f64 {
        let mut gaussian = self.gaussian;
        let value = gaussian.next(&mut || self.next_double());
        self.gaussian = gaussian;
        value
    }

    #[inline]
    fn set_seed(&mut self, seed: i64) {
        *self = Self::new(seed);
    }

    #[inline]
    fn fork(&mut self) -> Random {
        let lo = self.raw();
        let hi = self.raw();
        Random::Xoroshiro(XoroshiroRandom::from_parts(lo, hi))
    }

    #[inline]
    fn fork_positional(&mut self) -> PositionalRandom {
        let lo = self.raw();
        let hi = self.raw();
        PositionalRandom::Xoroshiro { lo, hi }
    }

    #[inline]
    fn consume_count(&mut self, rounds: usize) {
        for _ in 0..rounds {
            self.raw();
        }
    }
}

/// Either generator, for code that works with whichever a world uses.
#[derive(Debug, Clone)]
pub enum Random {
    Legacy(LegacyRandom),
    Xoroshiro(XoroshiroRandom),
}

macro_rules! dispatch {
    ($self:ident, $r:ident => $e:expr) => {
        match $self {
            Random::Legacy($r) => $e,
            Random::Xoroshiro($r) => $e,
        }
    };
}

impl RandomSource for Random {
    #[inline]
    fn next_int(&mut self) -> i32 {
        dispatch!(self, r => r.next_int())
    }
    #[inline]
    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        dispatch!(self, r => r.next_int_bounded(bound))
    }
    #[inline]
    fn next_long(&mut self) -> i64 {
        dispatch!(self, r => r.next_long())
    }
    #[inline]
    fn next_bool(&mut self) -> bool {
        dispatch!(self, r => r.next_bool())
    }
    #[inline]
    fn next_float(&mut self) -> f32 {
        dispatch!(self, r => r.next_float())
    }
    #[inline]
    fn next_double(&mut self) -> f64 {
        dispatch!(self, r => r.next_double())
    }
    #[inline]
    fn next_gaussian(&mut self) -> f64 {
        dispatch!(self, r => r.next_gaussian())
    }
    #[inline]
    fn set_seed(&mut self, seed: i64) {
        dispatch!(self, r => r.set_seed(seed))
    }
    #[inline]
    fn fork(&mut self) -> Random {
        dispatch!(self, r => r.fork())
    }
    #[inline]
    fn fork_positional(&mut self) -> PositionalRandom {
        dispatch!(self, r => r.fork_positional())
    }
    #[inline]
    fn consume_count(&mut self, rounds: usize) {
        dispatch!(self, r => r.consume_count(rounds))
    }
}

/// Which generator a world's noise settings ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Legacy,
    Xoroshiro,
}

impl Algorithm {
    #[inline]
    pub fn new_instance(self, seed: i64) -> Random {
        match self {
            Algorithm::Legacy => Random::Legacy(LegacyRandom::new(seed)),
            Algorithm::Xoroshiro => Random::Xoroshiro(XoroshiroRandom::new(seed)),
        }
    }
}

/// Derives independent generators from positions, names and seeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionalRandom {
    Legacy(i64),
    Xoroshiro { lo: i64, hi: i64 },
}

impl PositionalRandom {
    #[inline]
    pub fn at(&self, x: i32, y: i32, z: i32) -> Random {
        let position = position_seed(x, y, z);
        match *self {
            PositionalRandom::Legacy(seed) => Random::Legacy(LegacyRandom::new(position ^ seed)),
            PositionalRandom::Xoroshiro { lo, hi } => {
                Random::Xoroshiro(XoroshiroRandom::from_parts(position ^ lo, hi))
            }
        }
    }

    #[inline]
    pub fn from_hash_of(&self, name: &str) -> Random {
        match *self {
            PositionalRandom::Legacy(seed) => {
                Random::Legacy(LegacyRandom::new(java_string_hash(name) as i64 ^ seed))
            }
            PositionalRandom::Xoroshiro { lo, hi } => {
                let hash = Seed128::from_hash_of(name);
                Random::Xoroshiro(XoroshiroRandom::from_parts(hash.lo ^ lo, hash.hi ^ hi))
            }
        }
    }

    #[inline]
    pub fn from_seed(&self, seed: i64) -> Random {
        match *self {
            PositionalRandom::Legacy(_) => Random::Legacy(LegacyRandom::new(seed)),
            PositionalRandom::Xoroshiro { lo, hi } => {
                Random::Xoroshiro(XoroshiroRandom::from_parts(seed ^ lo, seed ^ hi))
            }
        }
    }
}

/// The legacy-shaped generator world generation seeds per chunk, feature
/// and carver. It draws its bits from `inner`: straight from the LCG, or the
/// top bits of the inner generator's next long.
#[derive(Debug, Clone)]
pub struct WorldgenRandom {
    inner: Random,
    gaussian: Gaussian,
}

impl WorldgenRandom {
    #[inline]
    pub fn new(inner: Random) -> Self {
        Self {
            inner,
            gaussian: Gaussian::default(),
        }
    }

    #[inline]
    pub fn legacy(seed: i64) -> Self {
        Self::new(Random::Legacy(LegacyRandom::new(seed)))
    }

    #[inline]
    pub fn set_decoration_seed(&mut self, seed: i64, x: i32, z: i32) -> i64 {
        self.set_seed(seed);
        let x_scale = self.next_long() | 1;
        let z_scale = self.next_long() | 1;
        let result = (x as i64)
            .wrapping_mul(x_scale)
            .wrapping_add((z as i64).wrapping_mul(z_scale))
            ^ seed;
        self.set_seed(result);
        result
    }

    #[inline]
    pub fn set_feature_seed(&mut self, seed: i64, index: i32, step: i32) {
        self.set_seed(
            seed.wrapping_add(index as i64)
                .wrapping_add(10000 * step as i64),
        );
    }

    #[inline]
    pub fn set_large_feature_seed(&mut self, seed: i64, x: i32, z: i32) {
        self.set_seed(seed);
        let x_scale = self.next_long();
        let z_scale = self.next_long();
        let result = (x as i64).wrapping_mul(x_scale) ^ (z as i64).wrapping_mul(z_scale) ^ seed;
        self.set_seed(result);
    }

    #[inline]
    pub fn set_large_feature_with_salt(&mut self, seed: i64, x: i32, z: i32, salt: i32) {
        let result = (x as i64)
            .wrapping_mul(341873128712)
            .wrapping_add((z as i64).wrapping_mul(132897987541))
            .wrapping_add(seed)
            .wrapping_add(salt as i64);
        self.set_seed(result);
    }
}

impl BitSource for WorldgenRandom {
    #[inline]
    fn next_bits(&mut self, bits: u32) -> i32 {
        match &mut self.inner {
            Random::Legacy(legacy) => legacy.next(bits),
            other => ((other.next_long() as u64) >> (64 - bits)) as i32,
        }
    }
}

impl RandomSource for WorldgenRandom {
    #[inline]
    fn next_int(&mut self) -> i32 {
        self.next_bits(32)
    }

    #[inline]
    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        bit_int_bounded(self, bound)
    }

    #[inline]
    fn next_long(&mut self) -> i64 {
        bit_long(self)
    }

    #[inline]
    fn next_bool(&mut self) -> bool {
        self.next_bits(1) != 0
    }

    #[inline]
    fn next_float(&mut self) -> f32 {
        bit_float(self)
    }

    #[inline]
    fn next_double(&mut self) -> f64 {
        bit_double(self)
    }

    #[inline]
    fn next_gaussian(&mut self) -> f64 {
        let mut gaussian = self.gaussian;
        let value = gaussian.next(&mut || self.next_double());
        self.gaussian = gaussian;
        value
    }

    #[inline]
    fn set_seed(&mut self, seed: i64) {
        self.inner.set_seed(seed);
    }

    #[inline]
    fn fork(&mut self) -> Random {
        self.inner.fork()
    }

    #[inline]
    fn fork_positional(&mut self) -> PositionalRandom {
        self.inner.fork_positional()
    }
}

/// The 64-bit LCG step biome zoom and some legacy code mix seeds with.
#[inline]
pub fn lcg_next(rval: i64, c: i64) -> i64 {
    rval.wrapping_mul(
        rval.wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407),
    )
    .wrapping_add(c)
}
