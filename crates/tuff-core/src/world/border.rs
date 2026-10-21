//! A dimension's world border: where it is, how it moves, and what the
//! `/worldborder` command may change. The coordinator owns each
//! dimension's border, ticks a moving one and publishes it on a watch that
//! [`crate::world::dimension::Dimension::border`] reads.

use bytes::Bytes;

use crate::protocol::play;
use crate::storage::level::{DEFAULT_BORDER_SIZE, WorldBorder};
use crate::world::framed;
use tuff_world_model::BlockPos;

pub const MAX_SIZE: f64 = DEFAULT_BORDER_SIZE;
pub const MAX_CENTER: f64 = 29_999_984.0;
/// How far from the origin a border edge may reach.
pub const ABSOLUTE_MAX: i32 = 29_999_984;
/// The game's `1.0E-5f` widened to a double.
const EDGE: f64 = 9.999_999_747_378_752e-6;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Extent {
    Static {
        size: f64,
    },
    Moving {
        from: f64,
        to: f64,
        duration: i64,
        remaining: i64,
        size: f64,
        /// The size the edges are at: the one before the last tick.
        previous: f64,
    },
}

impl Extent {
    fn moving(from: f64, to: f64, duration: i64) -> Self {
        if from == to {
            return Self::Static { size: to };
        }
        Self::Moving {
            from,
            to,
            duration,
            remaining: duration,
            size: from,
            previous: from,
        }
    }
}

fn lerp_size(from: f64, to: f64, duration: i64, remaining: i64) -> f64 {
    let progress = (duration - remaining) as f64 / duration as f64;
    if progress < 1.0 {
        from + progress * (to - from)
    } else {
        to
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    pub center: [f64; 2],
    pub damage_per_block: f64,
    pub safe_zone: f64,
    pub warning_blocks: i32,
    /// In ticks.
    pub warning_time: i32,
    /// How far from the origin an edge may reach; `max-world-size`.
    pub absolute_max: i32,
    extent: Extent,
}

impl Default for Border {
    fn default() -> Self {
        Self::from_saved(&WorldBorder::default())
    }
}

/// A `/worldborder` change. Sizes are diameters; times are in ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BorderChange {
    Set {
        size: f64,
        ticks: i64,
    },
    /// Grows by `distance`; with `ticks`, over that long plus what is left
    /// of a running change.
    Add {
        distance: f64,
        ticks: Option<i64>,
    },
    Center([f64; 2]),
    DamageAmount(f64),
    DamageBuffer(f64),
    WarningDistance(i32),
    WarningTime(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderError {
    SameCenter,
    SameSize,
    TooSmall,
    TooBig,
    TooFar,
    SameWarningTime,
    SameWarningDistance,
    SameDamageBuffer,
    SameDamageAmount,
}

impl BorderError {
    pub fn key(self) -> &'static str {
        match self {
            Self::SameCenter => "commands.worldborder.center.failed",
            Self::SameSize => "commands.worldborder.set.failed.nochange",
            Self::TooSmall => "commands.worldborder.set.failed.small",
            Self::TooBig => "commands.worldborder.set.failed.big",
            Self::TooFar => "commands.worldborder.set.failed.far",
            Self::SameWarningTime => "commands.worldborder.warning.time.failed",
            Self::SameWarningDistance => "commands.worldborder.warning.distance.failed",
            Self::SameDamageBuffer => "commands.worldborder.damage.buffer.failed",
            Self::SameDamageAmount => "commands.worldborder.damage.amount.failed",
        }
    }
}

/// What a size change did, for its feedback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resized {
    pub from: f64,
    pub to: f64,
    pub ticks: i64,
}

impl Border {
    pub fn from_saved(saved: &WorldBorder) -> Self {
        let extent = if saved.lerp_time > 0 {
            Extent::moving(saved.size, saved.lerp_target, saved.lerp_time)
        } else {
            Extent::Static { size: saved.size }
        };
        Self {
            center: saved.center,
            damage_per_block: saved.damage_per_block,
            safe_zone: saved.safe_zone,
            warning_blocks: saved.warning_blocks,
            warning_time: saved.warning_time,
            absolute_max: ABSOLUTE_MAX,
            extent,
        }
    }

    pub fn saved(&self) -> WorldBorder {
        WorldBorder {
            center: self.center,
            size: self.size(),
            damage_per_block: self.damage_per_block,
            safe_zone: self.safe_zone,
            warning_blocks: self.warning_blocks,
            warning_time: self.warning_time,
            lerp_time: self.lerp_time(),
            lerp_target: self.lerp_target(),
        }
    }

    /// A fixed border of diameter `size` around `center`.
    pub fn fixed(center: [f64; 2], size: f64) -> Self {
        Self {
            center,
            extent: Extent::Static { size },
            ..Self::default()
        }
    }

    pub fn size(&self) -> f64 {
        match self.extent {
            Extent::Static { size } | Extent::Moving { size, .. } => size,
        }
    }

    pub fn lerp_target(&self) -> f64 {
        match self.extent {
            Extent::Static { size } => size,
            Extent::Moving { to, .. } => to,
        }
    }

    /// Ticks left of the running size change.
    pub fn lerp_time(&self) -> i64 {
        match self.extent {
            Extent::Static { .. } => 0,
            Extent::Moving { remaining, .. } => remaining,
        }
    }

    pub fn is_moving(&self) -> bool {
        matches!(self.extent, Extent::Moving { .. })
    }

    fn edge_size(&self) -> f64 {
        match self.extent {
            Extent::Static { size } => size,
            Extent::Moving { previous, .. } => previous,
        }
    }

    fn edge(&self, center: f64, sign: f64) -> f64 {
        let limit = f64::from(self.absolute_max);
        (center + sign * self.edge_size() / 2.0).clamp(-limit, limit)
    }

    pub fn min_x(&self) -> f64 {
        self.edge(self.center[0], -1.0)
    }

    pub fn max_x(&self) -> f64 {
        self.edge(self.center[0], 1.0)
    }

    pub fn min_z(&self) -> f64 {
        self.edge(self.center[1], -1.0)
    }

    pub fn max_z(&self) -> f64 {
        self.edge(self.center[1], 1.0)
    }

    /// One tick of a running size change. Returns whether anything moved.
    pub fn tick(&mut self) -> bool {
        let Extent::Moving {
            from,
            to,
            duration,
            remaining,
            size,
            ..
        } = self.extent
        else {
            return false;
        };
        let remaining = remaining - 1;
        self.extent = if remaining <= 0 {
            Extent::Static { size: to }
        } else {
            Extent::Moving {
                from,
                to,
                duration,
                remaining,
                size: lerp_size(from, to, duration, remaining),
                previous: size,
            }
        };
        true
    }

    pub fn contains(&self, x: f64, z: f64) -> bool {
        self.contains_within(x, z, 0.0)
    }

    pub fn contains_within(&self, x: f64, z: f64, margin: f64) -> bool {
        x >= self.min_x() - margin
            && x < self.max_x() + margin
            && z >= self.min_z() - margin
            && z < self.max_z() + margin
    }

    /// Whether the block's lowest corner is inside, as block interaction
    /// and pistons ask.
    pub fn contains_block(&self, pos: BlockPos) -> bool {
        self.contains(f64::from(pos.x), f64::from(pos.z))
    }

    /// Whether a box, given by its horizontal corners, lies wholly inside.
    pub fn contains_box(&self, min: [f64; 2], max: [f64; 2]) -> bool {
        self.contains(min[0], min[1]) && self.contains(max[0] - EDGE, max[1] - EDGE)
    }

    /// How far `(x, z)` is from the nearest edge; negative outside.
    pub fn distance(&self, x: f64, z: f64) -> f64 {
        (x - self.min_x())
            .min(self.max_x() - x)
            .min(z - self.min_z())
            .min(self.max_z() - z)
    }

    pub fn clamp(&self, x: f64, z: f64) -> [f64; 2] {
        [
            x.clamp(self.min_x(), self.max_x() - EDGE),
            z.clamp(self.min_z(), self.max_z() - EDGE),
        ]
    }

    /// The block containing the nearest point inside.
    pub fn clamp_block(&self, x: f64, y: i32, z: f64) -> BlockPos {
        let [x, z] = self.clamp(x, z);
        BlockPos::new(x.floor() as i32, y, z.floor() as i32)
    }

    /// The edges entities collide with: whole blocks, rounded outward.
    pub fn collision_bounds(&self) -> ([f64; 2], [f64; 2]) {
        (
            [self.min_x().floor(), self.min_z().floor()],
            [self.max_x().ceil(), self.max_z().ceil()],
        )
    }

    /// Solid slabs beyond each edge within `reach` blocks of `(x, z)`, which
    /// entities inside the border collide with.
    pub fn collision_slabs(&self, x: f64, z: f64, reach: f64) -> Vec<[f64; 6]> {
        const FAR: f64 = 3.0e7;
        const TALL: f64 = 1.0e6;
        let (min, max) = self.collision_bounds();
        let mut slabs = Vec::new();
        if x - reach < min[0] {
            slabs.push([-FAR, -TALL, -FAR, min[0], TALL, FAR]);
        }
        if x + reach > max[0] {
            slabs.push([max[0], -TALL, -FAR, FAR, TALL, FAR]);
        }
        if z - reach < min[1] {
            slabs.push([-FAR, -TALL, -FAR, FAR, TALL, min[1]]);
        }
        if z + reach > max[1] {
            slabs.push([-FAR, -TALL, max[1], FAR, TALL, FAR]);
        }
        slabs
    }

    /// Whether a move of a box `half_width` wide from `from` to `to` pushes
    /// into the border from inside it, which the game's collision forbids.
    /// Within `tolerance` of the edge a move still counts as allowed, as the
    /// game accepts small differences from the client.
    pub fn blocks_move(
        &self,
        from: [f64; 3],
        to: [f64; 3],
        half_width: f64,
        tolerance: f64,
    ) -> bool {
        let (min, max) = self.collision_bounds();
        let inside = |p: [f64; 3], slack: f64| {
            p[0] - half_width >= min[0] - slack
                && p[0] + half_width <= max[0] + slack
                && p[2] - half_width >= min[1] - slack
                && p[2] + half_width <= max[1] + slack
        };
        inside(from, EDGE) && !inside(to, tolerance)
    }

    /// Applies `change` as the command does, refusing one that changes
    /// nothing or is out of range.
    pub fn apply(&mut self, change: BorderChange) -> Result<Option<Resized>, BorderError> {
        match change {
            BorderChange::Set { size, ticks } => self.resize(size, ticks).map(Some),
            BorderChange::Add { distance, ticks } => {
                let ticks = ticks.map_or(0, |ticks| self.lerp_time() + ticks);
                self.resize(self.size() + distance, ticks).map(Some)
            }
            BorderChange::Center([x, z]) => {
                if self.center == [x, z] {
                    return Err(BorderError::SameCenter);
                }
                if x.abs() > MAX_CENTER || z.abs() > MAX_CENTER {
                    return Err(BorderError::TooFar);
                }
                self.center = [x, z];
                Ok(None)
            }
            BorderChange::DamageAmount(amount) => {
                if self.damage_per_block == amount {
                    return Err(BorderError::SameDamageAmount);
                }
                self.damage_per_block = amount;
                Ok(None)
            }
            BorderChange::DamageBuffer(distance) => {
                if self.safe_zone == distance {
                    return Err(BorderError::SameDamageBuffer);
                }
                self.safe_zone = distance;
                Ok(None)
            }
            BorderChange::WarningDistance(blocks) => {
                if self.warning_blocks == blocks {
                    return Err(BorderError::SameWarningDistance);
                }
                self.warning_blocks = blocks;
                Ok(None)
            }
            BorderChange::WarningTime(ticks) => {
                if self.warning_time == ticks {
                    return Err(BorderError::SameWarningTime);
                }
                self.warning_time = ticks;
                Ok(None)
            }
        }
    }

    fn resize(&mut self, size: f64, ticks: i64) -> Result<Resized, BorderError> {
        let current = self.size();
        if current == size {
            return Err(BorderError::SameSize);
        }
        if size < 1.0 {
            return Err(BorderError::TooSmall);
        }
        if size > MAX_SIZE {
            return Err(BorderError::TooBig);
        }
        self.extent = if ticks > 0 {
            Extent::moving(current, size, ticks)
        } else {
            Extent::Static { size }
        };
        Ok(Resized {
            from: current,
            to: size,
            ticks,
        })
    }

    pub fn initialize(&self) -> play::InitializeBorder {
        play::InitializeBorder {
            center: self.center,
            old_size: self.size(),
            new_size: self.lerp_target(),
            lerp_time: self.lerp_time(),
            absolute_max_size: self.absolute_max,
            warning_blocks: self.warning_blocks,
            warning_time: self.warning_time,
        }
    }

    /// The packet that tells clients about `change` once applied, framed.
    pub fn change_packet(&self, change: BorderChange) -> Option<Bytes> {
        Some(match change {
            BorderChange::Set { .. } | BorderChange::Add { .. } if self.is_moving() => {
                framed(&play::SetBorderLerpSize {
                    old_size: self.size(),
                    new_size: self.lerp_target(),
                    lerp_time: self.lerp_time(),
                })
            }
            BorderChange::Set { .. } | BorderChange::Add { .. } => {
                framed(&play::SetBorderSize(self.size()))
            }
            BorderChange::Center(_) => framed(&play::SetBorderCenter {
                center: self.center,
            }),
            BorderChange::WarningDistance(blocks) => {
                framed(&play::SetBorderWarningDistance(blocks))
            }
            BorderChange::WarningTime(ticks) => framed(&play::SetBorderWarningDelay(ticks)),
            BorderChange::DamageAmount(_) | BorderChange::DamageBuffer(_) => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    #[tuff_core::watchdog]
    #[test]
    fn max_world_size_limits_edges_and_the_packet() {
        let border = Border {
            absolute_max: 1000,
            ..Border::default()
        };
        assert_eq!(border.max_x(), 1000.0);
        assert_eq!(border.min_z(), -1000.0);
        assert_eq!(border.initialize().absolute_max_size, 1000);
    }

    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_moving_border_lerps_per_tick_and_its_edges_trail_one_tick() {
        let mut border = Border::fixed([0.0, 0.0], 100.0);
        let resized = border
            .apply(BorderChange::Set {
                size: 50.0,
                ticks: 10,
            })
            .unwrap();
        assert_eq!(
            resized,
            Some(Resized {
                from: 100.0,
                to: 50.0,
                ticks: 10
            })
        );
        assert_eq!((border.size(), border.lerp_time()), (100.0, 10));
        assert!(border.tick());
        assert_eq!((border.size(), border.lerp_time()), (95.0, 9));
        assert_eq!(border.max_x(), 50.0);
        border.tick();
        assert_eq!(border.max_x(), 47.5);
        for _ in 0..8 {
            border.tick();
        }
        assert!(!border.is_moving());
        assert_eq!((border.size(), border.max_x()), (50.0, 25.0));
        assert!(!border.tick());
    }

    #[tuff_core::watchdog]
    #[test]
    fn adding_with_time_extends_a_running_change() {
        let mut border = Border::fixed([0.0, 0.0], 100.0);
        border
            .apply(BorderChange::Set {
                size: 200.0,
                ticks: 20,
            })
            .unwrap();
        for _ in 0..5 {
            border.tick();
        }
        assert_eq!(border.size(), 125.0);
        border
            .apply(BorderChange::Add {
                distance: 10.0,
                ticks: Some(40),
            })
            .unwrap();
        assert_eq!(border.lerp_target(), 135.0);
        assert_eq!(border.lerp_time(), 55);
        border
            .apply(BorderChange::Add {
                distance: 1.0,
                ticks: None,
            })
            .unwrap();
        assert_eq!((border.size(), border.lerp_time()), (126.0, 0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn changes_that_change_nothing_or_go_too_far_are_refused() {
        let mut border = Border::default();
        let refused = |border: &mut Border, change| border.apply(change).unwrap_err();
        assert_eq!(
            refused(
                &mut border,
                BorderChange::Set {
                    size: MAX_SIZE,
                    ticks: 0
                }
            ),
            BorderError::SameSize
        );
        assert_eq!(
            refused(
                &mut border,
                BorderChange::Set {
                    size: 0.5,
                    ticks: 0
                }
            ),
            BorderError::TooSmall
        );
        assert_eq!(
            refused(
                &mut border,
                BorderChange::Add {
                    distance: 1.0,
                    ticks: None
                }
            ),
            BorderError::TooBig
        );
        assert_eq!(
            refused(&mut border, BorderChange::Center([0.0, 0.0])),
            BorderError::SameCenter
        );
        assert_eq!(
            refused(&mut border, BorderChange::Center([3.0e7, 0.0])),
            BorderError::TooFar
        );
        assert_eq!(
            refused(&mut border, BorderChange::DamageAmount(0.2)),
            BorderError::SameDamageAmount
        );
        assert_eq!(
            refused(&mut border, BorderChange::DamageBuffer(5.0)),
            BorderError::SameDamageBuffer
        );
        assert_eq!(
            refused(&mut border, BorderChange::WarningDistance(5)),
            BorderError::SameWarningDistance
        );
        assert_eq!(
            refused(&mut border, BorderChange::WarningTime(300)),
            BorderError::SameWarningTime
        );
        for error in [
            BorderError::SameCenter,
            BorderError::SameSize,
            BorderError::TooSmall,
            BorderError::TooBig,
            BorderError::TooFar,
            BorderError::SameWarningTime,
            BorderError::SameWarningDistance,
            BorderError::SameDamageBuffer,
            BorderError::SameDamageAmount,
        ] {
            assert!(
                tuff_vanilla_data::lang::lookup(error.key()).is_some(),
                "{error:?}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn bounds_and_distances_follow_the_edges() {
        let border = Border::fixed([0.5, 0.5], 100.0);
        assert_eq!((border.min_x(), border.max_x()), (-49.5, 50.5));
        assert!(border.contains_block(BlockPos::new(50, 0, -49)));
        assert!(!border.contains_block(BlockPos::new(51, 0, 0)));
        assert!(!border.contains_block(BlockPos::new(0, 0, -50)));
        assert_eq!(border.distance(40.5, 0.0), 10.0);
        assert_eq!(border.distance(60.5, 0.0), -10.0);
        assert_eq!(
            border.clamp_block(1000.0, 7, -1000.0),
            BlockPos::new(50, 7, -50)
        );
        assert!(border.contains_box([-49.5, 0.0], [50.5, 1.0]));
        assert!(!border.contains_box([-49.6, 0.0], [0.0, 1.0]));
        assert_eq!(border.collision_bounds(), ([-50.0, -50.0], [51.0, 51.0]));
        assert!(border.blocks_move([50.0, 0.0, 0.0], [51.0, 0.0, 0.0], 0.3, 0.0625));
        assert!(!border.blocks_move([50.0, 0.0, 0.0], [50.7, 0.0, 0.0], 0.3, 0.0625));
        assert!(!border.blocks_move([60.0, 0.0, 0.0], [61.0, 0.0, 0.0], 0.3, 0.0625));
        let far = Border::default();
        assert_eq!(far.max_x(), f64::from(ABSOLUTE_MAX));
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_state_round_trips_a_running_change() {
        let mut border = Border::fixed([3.0, -4.0], 100.0);
        border.damage_per_block = 1.5;
        border
            .apply(BorderChange::Set {
                size: 20.0,
                ticks: 40,
            })
            .unwrap();
        border.tick();
        let saved = border.saved();
        assert_eq!(
            (saved.size, saved.lerp_target, saved.lerp_time),
            (98.0, 20.0, 39)
        );
        let loaded = Border::from_saved(&saved);
        assert_eq!(
            (loaded.size(), loaded.lerp_target(), loaded.lerp_time()),
            (98.0, 20.0, 39)
        );
        assert_eq!(loaded.damage_per_block, 1.5);
        let fixed = Border::fixed([0.0, 0.0], 64.0).saved();
        assert_eq!((fixed.lerp_time, fixed.lerp_target), (0, 64.0));
    }
}
