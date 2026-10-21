//! Ore blobs and scattered ores.

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::BlockState;

use crate::feature::level::Region;
use crate::feature::predicate::{DIRECTIONS, RuleTest, step};
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::{self, Result, obj};
use crate::proto::HeightmapKind;
use tuff_core::mth;
use tuff_item::random::{RandomSource, WorldgenRandom};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:ore", |_, value| {
        Ok(Arc::new(Ore(OreConfig::parse(value)?)))
    });
    registry.register("minecraft:scattered_ore", |_, value| {
        Ok(Arc::new(ScatteredOre(OreConfig::parse(value)?)))
    });
}

struct OreConfig {
    targets: Vec<(RuleTest, BlockState)>,
    size: i32,
    discard_chance: f32,
}

impl OreConfig {
    fn parse(value: &Value) -> Result<Self> {
        let o = obj(
            value,
            &["targets", "size", "discard_chance_on_air_exposure"],
            "ore",
        )?;
        let mut targets = Vec::new();
        for target in o.list("targets")? {
            let t = obj(target, &["target", "state"], "ore target")?;
            targets.push((
                RuleTest::parse(t.req("target")?)?,
                json::block_state(t.req("state")?)?,
            ));
        }
        Ok(Self {
            targets,
            size: o.i32("size")?,
            discard_chance: o.f32_or("discard_chance_on_air_exposure", 0.0)?,
        })
    }

    fn can_place(
        &self,
        state: BlockState,
        level: &Region,
        random: &mut WorldgenRandom,
        test: &RuleTest,
        pos: BlockPos,
    ) -> bool {
        if test.test(state, pos, random) {
            return skip_air_check(random, self.discard_chance) || !adjacent_to_air(level, pos);
        }
        false
    }
}

fn adjacent_to_air(level: &Region, pos: BlockPos) -> bool {
    DIRECTIONS
        .iter()
        .any(|d| is_air(level.block(step(pos, *d))))
}

fn skip_air_check(random: &mut WorldgenRandom, discard: f32) -> bool {
    if discard <= 0.0 {
        return true;
    }
    discard < 1.0 && random.next_float() >= discard
}

struct Ore(OreConfig);

/// The box a blob may touch: its low corner, horizontal and vertical span.
struct Footprint {
    corner: [i32; 3],
    across: i32,
    tall: i32,
}

/// A ball of the blob: its centre and radius; a radius of zero marks a
/// ball swallowed by a bigger neighbour.
#[derive(Clone, Copy)]
struct Ball {
    centre: [f64; 3],
    radius: f64,
}

impl Ore {
    fn footprint(&self, origin: BlockPos) -> Footprint {
        let size = self.0.size as f32;
        let margin = (size / 8.0).ceil() as i32;
        let swell = ((size / 16.0 * 2.0 + 1.0) / 2.0).ceil() as i32;
        Footprint {
            corner: [
                origin.x - margin - swell,
                origin.y - 2 - swell,
                origin.z - margin - swell,
            ],
            across: 2 * (margin + swell),
            tall: 2 * (2 + swell),
        }
    }

    /// Lays `size` balls along the segment between `from` and `to`, fattest
    /// in the middle.
    fn balls(&self, random: &mut WorldgenRandom, from: [f64; 3], to: [f64; 3]) -> Vec<Ball> {
        let size = self.0.size;
        (0..size)
            .map(|i| {
                let t = i as f32 / size as f32;
                let along = |k: usize| from[k] + t as f64 * (to[k] - from[k]);
                let girth = random.next_double() * size as f64 / 16.0;
                let bulge = (mth::sin((std::f32::consts::PI * t) as f64) + 1.0) as f64;
                Ball {
                    centre: [along(0), along(1), along(2)],
                    radius: (bulge * girth + 1.0) / 2.0,
                }
            })
            .collect()
    }
}

/// Drops every ball that a later or earlier one fully contains, comparing
/// pairs in order so a dropped ball no longer swallows others.
fn drop_swallowed(balls: &mut [Ball]) {
    for i in 0..balls.len() {
        for j in i + 1..balls.len() {
            let (a, b) = (balls[i], balls[j]);
            if a.radius <= 0.0 {
                break;
            }
            if b.radius <= 0.0 {
                continue;
            }
            let gap: f64 = (0..3)
                .map(|k| a.centre[k] - b.centre[k])
                .map(|d| d * d)
                .fold(0.0, |s, d| s + d);
            let dr = a.radius - b.radius;
            if dr * dr > gap {
                let loser = if dr > 0.0 { j } else { i };
                balls[loser].radius = 0.0;
            }
        }
    }
}

impl Feature for Ore {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let heading = ctx.random.next_float() * std::f32::consts::PI;
        let mut end_y = || (origin.y + ctx.random.next_int_bounded(3) - 2) as f64;
        let (from_y, to_y) = (end_y(), end_y());
        let footprint = self.footprint(origin);
        if !reaches_ground(ctx.level, footprint.corner, footprint.across) {
            return false;
        }
        let half_length = (self.0.size as f32 / 8.0) as f64;
        let (dx, dz) = (
            (heading as f64).sin() * half_length,
            (heading as f64).cos() * half_length,
        );
        let (ox, oz) = (origin.x as f64, origin.z as f64);
        let mut balls = self.balls(
            ctx.random,
            [ox + dx, from_y, oz + dz],
            [ox - dx, to_y, oz - dz],
        );
        drop_swallowed(&mut balls);
        self.fill(ctx, &balls, &footprint)
    }
}

/// Whether the blob's footprint holds a column whose ocean floor is at or
/// above the blob's lowest layer; only then can it place anything.
pub(crate) fn reaches_ground(
    level: &mut Region,
    [x_start, y_start, z_start]: [i32; 3],
    size_xz: i32,
) -> bool {
    if level
        .ocean_floor_ceiling()
        .is_some_and(|ceiling| y_start > ceiling)
    {
        return false;
    }
    for x in x_start..=x_start + size_xz {
        for z in z_start..=z_start + size_xz {
            if y_start <= level.height(HeightmapKind::OceanFloorWg, x, z) {
                return true;
            }
        }
    }
    false
}

impl Ore {
    fn fill(&self, ctx: &mut PlaceContext, balls: &[Ball], footprint: &Footprint) -> bool {
        let mut any = false;
        let mut visited = Visited::new(footprint.across, footprint.tall);
        let build = (ctx.level.min_y(), ctx.level.max_y());
        for ball in balls.iter().filter(|b| b.radius > 0.0) {
            let [x, y, z] = ball.centre;
            walk_sphere(
                [x, y, z, ball.radius],
                footprint.corner,
                build,
                &mut visited,
                &mut |x, y, z| {
                    let pos = BlockPos::new(x, y, z);
                    if !ctx.level.is_within_write_zone(pos) {
                        return;
                    }
                    let found = ctx.level.block(pos);
                    let hit = self.0.targets.iter().find(|(test, _)| {
                        self.0.can_place(found, ctx.level, ctx.random, test, pos)
                    });
                    if let Some(&(_, ore)) = hit {
                        ctx.level.set_block_raw(pos, ore);
                        any = true;
                    }
                },
            );
        }
        any
    }
}

/// The cells of a blob already handed out, as one z bitmask per (x, y)
/// row; sizes whose rows do not fit a word fall back to a flag per cell.
enum Visited {
    Rows {
        rows: Vec<u64>,
        width: usize,
    },
    Cells {
        flags: Vec<bool>,
        size_xz: i32,
        size_y: i32,
    },
}

impl Visited {
    fn new(size_xz: i32, size_y: i32) -> Self {
        if size_xz < 63 {
            let width = size_xz as usize + 1;
            Self::Rows {
                rows: vec![0; width * (size_y as usize + 1)],
                width,
            }
        } else {
            Self::Cells {
                flags: vec![false; (size_xz * size_y * size_xz) as usize],
                size_xz,
                size_y,
            }
        }
    }
}

/// Calls `visit` for every cell of the sphere `[x, y, z, radius]` inside
/// the build height that no earlier sphere of the blob visited, in x, y,
/// then ascending z order.
fn walk_sphere(
    [xx, yy, zz, r]: [f64; 4],
    [x_start, y_start, z_start]: [i32; 3],
    (min_y, max_y): (i32, i32),
    visited: &mut Visited,
    visit: &mut impl FnMut(i32, i32, i32),
) {
    let x_min = ((xx - r).floor() as i32).max(x_start);
    let y_min = ((yy - r).floor() as i32).max(y_start);
    let z_min = ((zz - r).floor() as i32).max(z_start);
    let x_max = ((xx + r).floor() as i32).max(x_min);
    let y_max = ((yy + r).floor() as i32).max(y_min);
    let z_max = ((zz + r).floor() as i32).max(z_min);
    for x in x_min..=x_max {
        let xd = (x as f64 + 0.5 - xx) / r;
        if xd * xd >= 1.0 {
            continue;
        }
        for y in y_min..=y_max {
            let yd = (y as f64 + 0.5 - yy) / r;
            if xd * xd + yd * yd >= 1.0 || y < min_y || y > max_y {
                continue;
            }
            match visited {
                Visited::Rows { rows, width } => {
                    let c = xd * xd + yd * yd;
                    let mut mask = 0u64;
                    for z in z_min..=z_max {
                        let zd = (z as f64 + 0.5 - zz) / r;
                        let v = c + zd * zd;
                        mask |= ((v < 1.0 || v.is_nan()) as u64) << (z - z_start);
                    }
                    let row = &mut rows[(x - x_start) as usize + (y - y_start) as usize * *width];
                    let mut fresh = mask & !*row;
                    *row |= fresh;
                    while fresh != 0 {
                        let bit = fresh.trailing_zeros() as i32;
                        fresh &= fresh - 1;
                        visit(x, y, z_start + bit);
                    }
                }
                Visited::Cells {
                    flags,
                    size_xz,
                    size_y,
                } => {
                    for z in z_min..=z_max {
                        let zd = (z as f64 + 0.5 - zz) / r;
                        if xd * xd + yd * yd + zd * zd >= 1.0 {
                            continue;
                        }
                        let index = ((x - x_start)
                            + (y - y_start) * *size_xz
                            + (z - z_start) * *size_xz * *size_y)
                            as usize;
                        if flags[index] {
                            continue;
                        }
                        flags[index] = true;
                        visit(x, y, z);
                    }
                }
            }
        }
    }
}

struct ScatteredOre(OreConfig);

fn axis_offset(random: &mut WorldgenRandom, max: i32) -> i32 {
    java_round((random.next_float() - random.next_float()) * max as f32)
}

pub fn java_round(v: f32) -> i32 {
    (v as f64 + 0.5).floor() as i32
}

impl Feature for ScatteredOre {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let tries = ctx.random.next_int_bounded(self.0.size + 1);
        for i in 0..tries {
            let max = i.min(7);
            let dx = axis_offset(ctx.random, max);
            let dy = axis_offset(ctx.random, max);
            let dz = axis_offset(ctx.random, max);
            let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
            let state = ctx.level.block(pos);
            for (test, ore) in &self.0.targets {
                if self.0.can_place(state, ctx.level, ctx.random, test, pos) {
                    ctx.level.set_block(pos, *ore);
                    break;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod walk_tests {
    use super::*;

    struct Lcg(u64);

    impl Lcg {
        fn unit(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    fn reference(
        spheres: &[[f64; 4]],
        [x_start, y_start, z_start]: [i32; 3],
        size_xz: i32,
        size_y: i32,
        (min_y, max_y): (i32, i32),
    ) -> Vec<(i32, i32, i32)> {
        let mut tested = vec![false; (size_xz * size_y * size_xz) as usize];
        let mut out = Vec::new();
        for &[xx, yy, zz, r] in spheres {
            let x_min = ((xx - r).floor() as i32).max(x_start);
            let y_min = ((yy - r).floor() as i32).max(y_start);
            let z_min = ((zz - r).floor() as i32).max(z_start);
            let x_max = ((xx + r).floor() as i32).max(x_min);
            let y_max = ((yy + r).floor() as i32).max(y_min);
            let z_max = ((zz + r).floor() as i32).max(z_min);
            for x in x_min..=x_max {
                let xd = (x as f64 + 0.5 - xx) / r;
                if xd * xd >= 1.0 {
                    continue;
                }
                for y in y_min..=y_max {
                    let yd = (y as f64 + 0.5 - yy) / r;
                    if xd * xd + yd * yd >= 1.0 {
                        continue;
                    }
                    for z in z_min..=z_max {
                        let zd = (z as f64 + 0.5 - zz) / r;
                        if xd * xd + yd * yd + zd * zd >= 1.0 || y < min_y || y > max_y {
                            continue;
                        }
                        let index = ((x - x_start)
                            + (y - y_start) * size_xz
                            + (z - z_start) * size_xz * size_y)
                            as usize;
                        if tested[index] {
                            continue;
                        }
                        tested[index] = true;
                        out.push((x, y, z));
                    }
                }
            }
        }
        out
    }

    #[tuff_core::watchdog]
    #[test]
    fn row_masks_visit_the_same_cells_in_the_same_order() {
        let mut rng = Lcg(3);
        for round in 0..600 {
            let size = [1, 2, 8, 17, 33, 64, 200][round % 7];
            let spread = size as f32 / 8.0;
            let max_radius = ((size as f32 / 16.0 * 2.0 + 1.0) / 2.0).ceil() as i32;
            let spread_ceil = spread.ceil() as i32;
            let size_xz = 2 * (spread_ceil + max_radius);
            let size_y = 2 * (2 + max_radius);
            let origin = [
                (rng.unit() * 4000.0 - 2000.0) as i32,
                (rng.unit() * 400.0 - 200.0) as i32,
                (rng.unit() * 4000.0 - 2000.0) as i32,
            ];
            let start = [
                origin[0] - spread_ceil - max_radius,
                origin[1] - 2 - max_radius,
                origin[2] - spread_ceil - max_radius,
            ];
            let dir = rng.unit() * std::f64::consts::PI;
            let (sin, cos) = (dir.sin(), dir.cos());
            let y_span = [-64, 320];
            let spheres: Vec<[f64; 4]> = (0..size.min(64))
                .map(|i| {
                    let step = i as f64 / size as f64;
                    let r =
                        (((step * std::f64::consts::PI).sin() + 1.0) * rng.unit() * size as f64
                            / 16.0
                            + 1.0)
                            / 2.0;
                    [
                        origin[0] as f64 + sin * spread as f64 * (1.0 - 2.0 * step),
                        origin[1] as f64 + (rng.unit() * 3.0 - 2.0) * (1.0 - step),
                        origin[2] as f64 + cos * spread as f64 * (1.0 - 2.0 * step),
                        r,
                    ]
                })
                .collect();
            let range = (
                y_span[0] + (round as i32 % 3) * 40,
                y_span[1] - (round as i32 % 2) * 300,
            );
            let expected = reference(&spheres, start, size_xz, size_y, range);
            let mut got = Vec::new();
            let mut visited = Visited::new(size_xz, size_y);
            for &sphere in &spheres {
                walk_sphere(sphere, start, range, &mut visited, &mut |x, y, z| {
                    got.push((x, y, z))
                });
            }
            assert_eq!(got, expected, "size {size} round {round}");
        }
    }
}
