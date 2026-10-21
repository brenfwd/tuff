//! The fish side of a floating bobber: an idle line rolls a wait, a fish
//! is lured in, swims up leaving a wake, bites, and lets go again.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::ParticleType;
use tuff_world_model::BlockPos;

const DEG: f32 = 0.017_453_292;
const WAKE_STRENGTH: f32 = 0.04;
const BITE_SPLASH_WIDTH: f64 = 0.25;

/// Extra lure chance per tick once the wait drops under each threshold.
const LURE_RAMP: [(i32, f32); 3] = [(20, 0.05), (40, 0.02), (60, 0.01)];

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Line {
    Slack,
    Waiting(i32),
    Approaching { left: i32, heading: f32 },
    Biting(i32),
}

impl Line {
    pub(super) fn fish_on() -> Self {
        Self::Biting(30)
    }

    pub(super) fn biting(self) -> bool {
        matches!(self, Self::Biting(_))
    }

    pub(super) fn fish_close(self) -> bool {
        matches!(self, Self::Biting(_) | Self::Approaching { .. })
    }
}

/// What the world around the bobber looks like to the fish.
pub(super) struct Surroundings<S, W> {
    pub raining: bool,
    pub open_sky: S,
    pub water: W,
}

/// Something the bobber shows or plays for the fish.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Cue {
    Particles {
        kind: ParticleType,
        at: [f64; 3],
        count: i32,
        spread: [f64; 3],
        speed: f32,
    },
    Splash {
        pitch: f32,
        seed: i64,
    },
}

/// A change the bobber itself must make after a tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Turn {
    Bit { sink: f64 },
    Escaped,
}

fn pace<S: Fn(BlockPos) -> bool, W>(
    rng: &mut StdRng,
    above: BlockPos,
    env: &Surroundings<S, W>,
) -> i32 {
    let storm = rng.r#gen::<f32>() < 0.25;
    let boost = i32::from(storm && env.raining && (env.open_sky)(above));
    let shade = rng.r#gen::<f32>() < 0.5;
    let slow = i32::from(shade && !(env.open_sky)(above));
    1 + boost - slow
}

fn lure_chance(left: i32) -> f32 {
    let ramp = LURE_RAMP
        .iter()
        .find(|(limit, _)| left < *limit)
        .map_or(0.0, |&(limit, rate)| (limit - left) as f32 * rate);
    0.15 + ramp
}

fn surface_above(at: [f64; 3], along: (f32, f32), reach: f64) -> [f64; 3] {
    [
        at[0] + f64::from(along.0) * reach,
        at[1].floor() + 1.0,
        at[2] + f64::from(along.1) * reach,
    ]
}

impl Line {
    /// Advances the fish by one tick of the bobber at `at` floating in the
    /// block `cell`.
    pub(super) fn advance<S, W>(
        &mut self,
        at: [f64; 3],
        cell: BlockPos,
        lure: i32,
        env: &Surroundings<S, W>,
        rng: &mut StdRng,
        mut cue: impl FnMut(Cue),
    ) -> Option<Turn>
    where
        S: Fn(BlockPos) -> bool,
        W: Fn([f64; 3]) -> bool,
    {
        let step = pace(rng, BlockPos::new(cell.x, cell.y + 1, cell.z), env);
        let water_under = |p: [f64; 3]| (env.water)([p[0], p[1] - 1.0, p[2]]);
        match *self {
            Self::Biting(n) => {
                if n > 1 {
                    *self = Self::Biting(n - 1);
                    None
                } else {
                    *self = Self::Slack;
                    Some(Turn::Escaped)
                }
            }
            Self::Approaching { left, heading } => {
                let left = left - step;
                if left <= 0 {
                    let pitch = 1.0 + (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.4;
                    cue(Cue::Splash {
                        pitch,
                        seed: rng.r#gen(),
                    });
                    let top = [at[0], at[1] + 0.5, at[2]];
                    let burst = (1.0 + BITE_SPLASH_WIDTH * 20.0) as i32;
                    for kind in [ParticleType::Bubble, ParticleType::Fishing] {
                        cue(Cue::Particles {
                            kind,
                            at: top,
                            count: burst,
                            spread: [BITE_SPLASH_WIDTH, 0.0, BITE_SPLASH_WIDTH],
                            speed: 0.2,
                        });
                    }
                    *self = Self::Biting(rng.gen_range(20..=40));
                    let sink = -0.4 * f64::from(rng.gen_range(0.6f32..1.0));
                    return Some(Turn::Bit { sink });
                }
                let drift = 9.188 * (rng.r#gen::<f64>() - rng.r#gen::<f64>());
                let heading = heading + drift as f32;
                *self = Self::Approaching { left, heading };
                let (s, c) = ((heading * DEG).sin(), (heading * DEG).cos());
                let fish = surface_above(at, (s, c), f64::from(left) * 0.1);
                if water_under(fish) {
                    if rng.r#gen::<f32>() < 0.15 {
                        cue(Cue::Particles {
                            kind: ParticleType::Bubble,
                            at: [fish[0], fish[1] - 0.1, fish[2]],
                            count: 1,
                            spread: [f64::from(s), 0.1, f64::from(c)],
                            speed: 0.0,
                        });
                    }
                    let side = [f64::from(c * WAKE_STRENGTH), f64::from(s * WAKE_STRENGTH)];
                    for sign in [1.0, -1.0] {
                        cue(Cue::Particles {
                            kind: ParticleType::Fishing,
                            at: fish,
                            count: 0,
                            spread: [sign * side[0], 0.01, -sign * side[1]],
                            speed: 1.0,
                        });
                    }
                }
                None
            }
            Self::Waiting(n) => {
                let left = n - step;
                if rng.r#gen::<f32>() < lure_chance(left) {
                    let bearing = rng.gen_range(0.0f32..360.0) * DEG;
                    let range = rng.gen_range(25.0f32..60.0);
                    let spot =
                        surface_above(at, (bearing.sin() * range, bearing.cos() * range), 0.1);
                    if water_under(spot) {
                        cue(Cue::Particles {
                            kind: ParticleType::Splash,
                            at: spot,
                            count: 2 + rng.gen_range(0..2),
                            spread: [0.1, 0.0, 0.1],
                            speed: 0.0,
                        });
                    }
                }
                *self = if left > 0 {
                    Self::Waiting(left)
                } else {
                    let heading = rng.gen_range(0.0f32..360.0);
                    Self::Approaching {
                        left: rng.gen_range(20..=80),
                        heading,
                    }
                };
                None
            }
            Self::Slack => {
                let wait = rng.gen_range(100..=600) - lure;
                if wait > 0 {
                    *self = Self::Waiting(wait);
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    struct Fnv(u64);

    impl std::hash::Hasher for Fnv {
        fn finish(&self) -> u64 {
            self.0
        }

        fn write(&mut self, bytes: &[u8]) {
            for b in bytes {
                self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3);
            }
        }
    }

    fn digest(seed: u64) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = Fnv(0xcbf2_9ce4_8422_2325);
        let mut rng = StdRng::seed_from_u64(seed);
        let env = Surroundings {
            raining: seed.is_multiple_of(2),
            open_sky: |p: BlockPos| (p.x + p.z + seed as i32) % 3 != 0,
            water: |p: [f64; 3]| (p[0].floor() as i32 ^ p[2].floor() as i32) & 3 != 1,
        };
        let mut line = if seed.is_multiple_of(5) {
            Line::fish_on()
        } else {
            Line::Slack
        };
        let at = [7.3, 64.9, -2.6];
        let cell = BlockPos::new(7, 64, -3);
        for tick in 0..4000u32 {
            tick.hash(&mut h);
            let turn = line.advance(at, cell, (seed % 6) as i32 * 20, &env, &mut rng, |c| {
                format!("{c:?}").hash(&mut h)
            });
            format!("{turn:?} {line:?}").hash(&mut h);
        }
        h.finish()
    }

    #[tuff_core::watchdog]
    #[test]
    fn fish_behaviour_is_pinned() {
        let all: Vec<u64> = (0..12).map(digest).collect();
        assert_eq!(all, PINNED);
    }

    const PINNED: [u64; 12] = [
        12287168231052280816,
        2262887422215536076,
        4694511735969000480,
        2994004461118332662,
        12423920870247726395,
        12034765871461895032,
        10121001413009219824,
        17240009982809576067,
        12642062867467231698,
        11283920956411437246,
        10122912780109702125,
        17685783135153695663,
    ];
}
