//! Icebergs, the blue ice under them, and ice spikes.

use std::sync::Arc;

use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::{BlockPredicate, DIRECTIONS, step};
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::{self, obj};
use tuff_item::random::{RandomSource, WorldgenRandom};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:blue_ice", |_, v| {
        obj(v, &[], "blue_ice")?;
        Ok(Arc::new(BlueIce))
    });
    registry.register("minecraft:spike", |_, v| {
        let o = obj(v, &["state", "can_place_on", "can_replace"], "spike")?;
        Ok(Arc::new(Spike {
            state: json::block_state(o.req("state")?)?,
            can_place_on: BlockPredicate::parse(o.req("can_place_on")?)?,
            can_replace: BlockPredicate::parse(o.req("can_replace")?)?,
        }))
    });
    registry.register("minecraft:iceberg", |_, v| {
        let o = obj(v, &["state"], "iceberg")?;
        Ok(Arc::new(Iceberg {
            state: json::block_state(o.req("state")?)?,
        }))
    });
}

struct BlueIce;

impl Feature for BlueIce {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if origin.y > ctx.level.sea_level() - 1 {
            return false;
        }
        let below = BlockPos::new(origin.x, origin.y - 1, origin.z);
        if ctx.level.block(origin).block() != Block::Water
            && ctx.level.block(below).block() != Block::Water
        {
            return false;
        }
        let touches_packed_ice = DIRECTIONS
            .iter()
            .filter(|d| **d != tuff_vanilla_data::block::Facing::Down)
            .any(|d| ctx.level.block(step(origin, *d)).block() == Block::PackedIce);
        if !touches_packed_ice {
            return false;
        }
        let blue_ice = Block::BlueIce.default_state();
        ctx.level.set_block(origin, blue_ice);
        for _ in 0..200 {
            let y_off = ctx.random.next_int_bounded(5) - ctx.random.next_int_bounded(6);
            let xz = if y_off < 2 { 3 + y_off / 2 } else { 3 };
            if xz < 1 {
                continue;
            }
            let dx = ctx.random.next_int_bounded(xz) - ctx.random.next_int_bounded(xz);
            let dz = ctx.random.next_int_bounded(xz) - ctx.random.next_int_bounded(xz);
            let pos = BlockPos::new(origin.x + dx, origin.y + y_off, origin.z + dz);
            let state = ctx.level.block(pos);
            if is_air(state)
                || matches!(state.block(), Block::Water | Block::PackedIce | Block::Ice)
            {
                let next_to_blue = DIRECTIONS
                    .iter()
                    .any(|d| ctx.level.block(step(pos, *d)).block() == Block::BlueIce);
                if next_to_blue {
                    ctx.level.set_block(pos, blue_ice);
                }
            }
        }
        true
    }
}

struct Iceberg {
    state: BlockState,
}

fn is_berg_block(state: BlockState) -> bool {
    matches!(
        state.block(),
        Block::PackedIce | Block::SnowBlock | Block::BlueIce
    )
}

fn ceil(value: f32) -> i32 {
    let truncated = value as i32;
    if value > truncated as f32 {
        truncated + 1
    } else {
        truncated
    }
}

fn sq(v: f64) -> f64 {
    v * v
}

fn half_of(fraction_left: f32, width: i32) -> i32 {
    ceil(fraction_left * width as f32 / 2.0)
}

#[derive(Clone, Copy)]
struct Rotation {
    cos: f64,
    sin: f64,
}

impl Rotation {
    fn new(angle: f64) -> Self {
        Self {
            cos: libm::cos(angle),
            sin: libm::sin(angle),
        }
    }

    /// Below zero inside the rotated ellipse with semi-axes `a` and `c`.
    fn ellipse(self, dx: f64, dz: f64, a: i32, c: i32) -> f64 {
        sq((dx * self.cos - dz * self.sin) / a as f64)
            + sq((dx * self.sin + dz * self.cos) / c as f64)
            - 1.0
    }
}

/// How fast a layer narrows with distance from the waterline.
#[derive(Clone, Copy)]
enum Taper {
    /// Parabolic dome without random draws, used above water by ovals.
    Dome,
    /// Parabolic spire that turns linear for tall bergs.
    Spire,
    /// Linear hull below water.
    Keel,
}

impl Taper {
    fn half_width(self, rng: &mut WorldgenRandom, depth: i32, span: i32, width: i32) -> i32 {
        let total = span as f32;
        let used = match self {
            Taper::Dome => (depth * depth) as f32 / total,
            Taper::Keel => {
                let stretch = 1.0 + rng.next_float() / 2.0;
                depth as f32 / (total * stretch)
            }
            Taper::Spire => {
                let stretch = 3.5 - rng.next_float();
                let tall = span > 15 + rng.next_int_bounded(5);
                if !tall {
                    (depth * depth) as f32 / (total * stretch)
                } else {
                    let step = if depth < 3 + rng.next_int_bounded(6) {
                        depth / 2
                    } else {
                        depth
                    };
                    step as f32 / (total * stretch * 0.4)
                }
            }
        };
        half_of(1.0 - used, width)
    }
}

#[derive(Clone, Copy)]
struct Oval {
    a: i32,
    c: i32,
    rotation: Rotation,
    angle: f64,
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Crown,
    Keel,
}

struct Berg {
    base: BlockPos,
    oval: Option<Oval>,
    /// The drawn oval axes, which also size the notch of round bergs.
    axes: (i32, i32),
    snowy: bool,
    crown: i32,
    keel: i32,
    width: i32,
}

/// One candidate column slice of the berg, relative to its waterline base.
struct Cell {
    dx: i32,
    dy: i32,
    dz: i32,
    span: i32,
    half: i32,
    reach: i32,
}

impl Berg {
    fn roll(rng: &mut WorldgenRandom, base: BlockPos) -> Self {
        let snowy = rng.next_double() > 0.7;
        let angle = rng.next_double() * std::f64::consts::TAU;
        let a = 11 - rng.next_int_bounded(5);
        let c = 3 + rng.next_int_bounded(3);
        let oval = (rng.next_double() > 0.7).then(|| Oval {
            a,
            c,
            rotation: Rotation::new(angle),
            angle,
        });
        let crown = match oval {
            Some(_) => 6 + rng.next_int_bounded(6),
            None => {
                let mut h = 3 + rng.next_int_bounded(15);
                if rng.next_double() > 0.9 {
                    h += 7 + rng.next_int_bounded(19);
                }
                h
            }
        };
        let keel = 18.min(crown + rng.next_int_bounded(11));
        let swell = rng.next_int_bounded(7);
        let width = 11.min(crown + swell - rng.next_int_bounded(5));
        Self {
            base,
            oval,
            axes: (a, c),
            snowy,
            crown,
            keel,
            width,
        }
    }

    fn at(&self, dx: i32, dy: i32, dz: i32) -> BlockPos {
        BlockPos::new(self.base.x + dx, self.base.y + dy, self.base.z + dz)
    }

    fn reach(&self) -> i32 {
        self.oval.map_or(11, |o| o.a)
    }

    /// Sizes the layer `depth` steps from the waterline; `None` when the
    /// column at `dx` lies outside it.
    fn layer(
        &self,
        rng: &mut WorldgenRandom,
        part: Part,
        (dx, dz): (i32, i32),
        depth: i32,
    ) -> Option<Cell> {
        let full = self.reach();
        let cell = match part {
            Part::Crown => {
                let taper = if self.oval.is_some() {
                    Taper::Dome
                } else {
                    Taper::Spire
                };
                let half = taper.half_width(rng, depth, self.crown, self.width);
                let cell = Cell {
                    dx,
                    dy: depth,
                    dz,
                    span: self.crown,
                    half,
                    reach: full,
                };
                if self.oval.is_some() {
                    return Some(cell);
                }
                cell
            }
            Part::Keel => {
                let reach = match self.oval {
                    Some(_) => ceil(
                        full as f32 * (1.0 - (depth * depth) as f32 / (self.keel as f32 * 8.0)),
                    ),
                    None => full,
                };
                let half = Taper::Keel.half_width(rng, depth, self.keel, self.width);
                Cell {
                    dx,
                    dy: -depth,
                    dz,
                    span: self.keel,
                    half,
                    reach,
                }
            }
        };
        (dx < cell.half).then_some(cell)
    }
}

impl Iceberg {
    /// Inside test with a soft rim: returns the distance and the rim
    /// threshold below which the rim never thins out. A NaN distance (from a
    /// zero-width oval axis) counts as inside.
    fn rim(berg: &Berg, rng: &mut WorldgenRandom, cell: &Cell) -> Option<(f64, f64)> {
        if let Some(oval) = berg.oval {
            let left = cell.span - cell.dy;
            let c = if cell.dy > 0 && left <= 3 {
                oval.c + left - 4
            } else {
                oval.c
            };
            let d = oval
                .rotation
                .ellipse(cell.dx as f64, cell.dz as f64, cell.reach, c);
            return if d >= 0.0 { None } else { Some((d, -0.5)) };
        }
        let wobble = rng.next_float().clamp(0.2, 0.8);
        let r2 = sq(cell.half as f64);
        let d = (10.0 * wobble / cell.half as f32) as f64 + sq(cell.dx as f64) + sq(cell.dz as f64)
            - r2;
        if d >= 0.0 {
            return None;
        }
        Some((d, (-6 - rng.next_int_bounded(3)) as f64))
    }

    fn fill(&self, ctx: &mut PlaceContext, berg: &Berg, cell: Cell) {
        let rng = &mut *ctx.random;
        let Some((d, soft)) = Self::rim(berg, rng, &cell) else {
            return;
        };
        if d > soft && rng.next_double() > 0.9 {
            return;
        }
        let pos = berg.at(cell.dx, cell.dy, cell.dz);
        let here = ctx.level.block(pos);
        let in_water = here.block() == Block::Water;
        let open =
            in_water || is_air(here) || matches!(here.block(), Block::SnowBlock | Block::Ice);
        if !open {
            return;
        }
        let oval = berg.oval.is_some();
        let mut snow_ok = !oval || rng.next_double() > 0.05;
        if berg.snowy && !in_water {
            let divisor = if oval { 3 } else { 2 };
            let jitter = rng.next_int_bounded((cell.span / divisor).max(1)) as f64;
            snow_ok &= ((cell.span - cell.dy) as f64) <= jitter + cell.span as f64 * 0.6;
        } else {
            snow_ok = false;
        }
        let placed = if snow_ok {
            Block::SnowBlock.default_state()
        } else {
            self.state
        };
        ctx.level.set_block(pos, placed);
    }

    fn sweep(&self, ctx: &mut PlaceContext, berg: &Berg, part: Part) {
        let reach = berg.reach();
        let depths = match part {
            Part::Crown => 0..berg.crown,
            Part::Keel => 1..berg.keel,
        };
        for dx in -reach..reach {
            for dz in -reach..reach {
                for depth in depths.clone() {
                    if let Some(cell) = berg.layer(ctx.random, part, (dx, dz), depth) {
                        self.fill(ctx, berg, cell);
                    }
                }
            }
        }
    }
}

/// Drops loose berg blocks: anything floating over air, and ice exposed on
/// three or more sides.
fn erode(level: &mut Region, berg: &Berg) {
    let r = berg.oval.map_or(berg.width / 2, |o| o.a);
    let height = berg.crown;
    let air = Block::Air.default_state();
    for dx in -r..=r {
        for dz in -r..=r {
            for dy in 0..=height {
                let pos = berg.at(dx, dy, dz);
                let state = level.block(pos);
                let ice = is_berg_block(state);
                if !ice && state.block() != Block::Snow {
                    continue;
                }
                if is_air(level.block(BlockPos::new(pos.x, pos.y - 1, pos.z))) {
                    level.set_block(pos, air);
                    level.set_block(BlockPos::new(pos.x, pos.y + 1, pos.z), air);
                    continue;
                }
                if ice {
                    let exposed = [(-1, 0), (1, 0), (0, -1), (0, 1)]
                        .into_iter()
                        .filter(|&(ox, oz)| {
                            !is_berg_block(level.block(BlockPos::new(
                                pos.x + ox,
                                pos.y,
                                pos.z + oz,
                            )))
                        })
                        .count();
                    if exposed >= 3 {
                        level.set_block(pos, air);
                    }
                }
            }
        }
    }
}

struct Notch {
    center: (i32, i32),
    rotation: Rotation,
}

fn hollow_layer(level: &mut Region, berg: &Berg, notch: &Notch, half_width: i32, dy: i32) {
    let (oa, oc) = berg.axes;
    let a = half_width + 1 + oa / 3;
    let c = (half_width - 3).min(3) + oc / 2 - 1;
    let fill = if dy < 0 {
        Block::Water.default_state()
    } else {
        Block::Air.default_state()
    };
    for dx in -a..a {
        for dz in -a..a {
            let rel = ((dx - notch.center.0) as f64, (dz - notch.center.1) as f64);
            if notch.rotation.ellipse(rel.0, rel.1, a, c) >= 0.0 {
                continue;
            }
            let pos = berg.at(dx, dy, dz);
            let state = level.block(pos);
            if !is_berg_block(state) && state.block() != Block::SnowBlock {
                continue;
            }
            level.set_block(pos, fill);
            if dy >= 0 {
                let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
                if level.block(above).block() == Block::Snow {
                    level.set_block(above, fill);
                }
            }
        }
    }
}

fn notch_offset(rng: &mut WorldgenRandom, width: i32) -> i32 {
    let half = width / 2;
    let near = rng.next_int_bounded((half - 2).max(1));
    if rng.next_bool() {
        half + 1 - rng.next_int_bounded((width - half - 1).max(1))
    } else {
        near
    }
}

fn cut_notch(ctx: &mut PlaceContext, berg: &Berg) {
    let (width, height) = (berg.width, berg.crown);
    let rng = &mut *ctx.random;
    let sx = if rng.next_bool() { -1 } else { 1 };
    let sz = if rng.next_bool() { -1 } else { 1 };
    let mut ox = notch_offset(rng, width);
    let mut oz = notch_offset(rng, width);
    let angle = match berg.oval {
        Some(oval) => {
            let shared = rng.next_int_bounded((oval.a - 5).max(1));
            (ox, oz) = (shared, shared);
            oval.angle + std::f64::consts::FRAC_PI_2
        }
        None => rng.next_double() * 2.0 * std::f64::consts::PI,
    };
    let notch = Notch {
        center: (sx * ox, sz * oz),
        rotation: Rotation::new(angle),
    };
    for dy in 0..height - 3 {
        let r = Taper::Spire.half_width(ctx.random, dy, height, width);
        hollow_layer(ctx.level, berg, &notch, r, dy);
    }
    let mut dy = -1;
    while dy > -height + ctx.random.next_int_bounded(5) {
        let r = Taper::Keel.half_width(ctx.random, -dy, height, width);
        hollow_layer(ctx.level, berg, &notch, r, dy);
        dy -= 1;
    }
}

impl Feature for Iceberg {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let base = BlockPos::new(origin.x, ctx.level.sea_level(), origin.z);
        let berg = Berg::roll(ctx.random, base);
        self.sweep(ctx, &berg, Part::Crown);
        erode(ctx.level, &berg);
        self.sweep(ctx, &berg, Part::Keel);
        let notch_odds = if berg.oval.is_some() { 0.1 } else { 0.7 };
        if ctx.random.next_double() > notch_odds {
            cut_notch(ctx, &berg);
        }
        true
    }
}

struct Spike {
    state: BlockState,
    can_place_on: BlockPredicate,
    can_replace: BlockPredicate,
}

impl Spike {
    fn replaceable(&self, level: &mut Region, pos: BlockPos) -> bool {
        is_air(level.block(pos)) || self.can_replace.test(level, pos)
    }
}

impl Feature for Spike {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut origin = origin;
        while is_air(ctx.level.block(origin)) && origin.y > ctx.level.min_y() + 2 {
            origin.y -= 1;
        }
        if !self.can_place_on.test(ctx.level, origin) {
            return false;
        }
        origin.y += ctx.random.next_int_bounded(4);
        let height = ctx.random.next_int_bounded(4) + 7;
        let width = height / 4 + ctx.random.next_int_bounded(2);
        if width > 1 && ctx.random.next_int_bounded(60) == 0 {
            origin.y += 10 + ctx.random.next_int_bounded(30);
        }
        for y_off in 0..height {
            let scale = (1.0 - y_off as f32 / height as f32) * width as f32;
            let new_width = ceil(scale);
            for xo in -new_width..=new_width {
                let dx = xo.abs() as f32 - 0.25;
                for zo in -new_width..=new_width {
                    let dz = zo.abs() as f32 - 0.25;
                    let inside = (xo == 0 && zo == 0) || dx * dx + dz * dz <= scale * scale;
                    if !inside {
                        continue;
                    }
                    let edge =
                        xo == -new_width || xo == new_width || zo == -new_width || zo == new_width;
                    if edge && ctx.random.next_float() > 0.75 {
                        continue;
                    }
                    let up = BlockPos::new(origin.x + xo, origin.y + y_off, origin.z + zo);
                    if self.replaceable(ctx.level, up) {
                        ctx.level.set_block(up, self.state);
                    }
                    if y_off != 0 && new_width > 1 {
                        let down = BlockPos::new(origin.x + xo, origin.y - y_off, origin.z + zo);
                        if self.replaceable(ctx.level, down) {
                            ctx.level.set_block(down, self.state);
                        }
                    }
                }
            }
        }
        let pillar = (width - 1).clamp(0, 1);
        for xo in -pillar..=pillar {
            for zo in -pillar..=pillar {
                let mut cursor = BlockPos::new(origin.x + xo, origin.y - 1, origin.z + zo);
                let mut run = if xo.abs() == 1 && zo.abs() == 1 {
                    ctx.random.next_int_bounded(5)
                } else {
                    50
                };
                while cursor.y > 50 {
                    let state = ctx.level.block(cursor);
                    if !is_air(state)
                        && !self.can_replace.test(ctx.level, cursor)
                        && state != self.state
                    {
                        break;
                    }
                    ctx.level.set_block(cursor, self.state);
                    cursor.y -= 1;
                    run -= 1;
                    if run <= 0 {
                        cursor.y -= ctx.random.next_int_bounded(5) + 1;
                        run = ctx.random.next_int_bounded(5);
                    }
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ProtoChunk;
    use crate::surface::GenContext;
    use serde_json::json;
    use tuff_item::random::{Random, XoroshiroRandom};
    use tuff_world_model::chunk::ChunkShape;
    use tuff_world_model::pos::ChunkPos;

    fn ocean() -> Region<'static> {
        let shape = ChunkShape::OVERWORLD;
        let chunks: Vec<ProtoChunk> = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| ChunkPos::new(dx, dz)))
            .map(|pos| ProtoChunk::new(pos, shape))
            .collect();
        let span = GenContext {
            min_y: shape.min_y,
            height: shape.height,
            sea_level: 63,
        };
        let mut region = Region::live(ChunkPos::new(0, 0), chunks, span, 1, 1);
        let water = Block::Water.default_state();
        let stone = Block::Stone.default_state();
        for x in -16..32 {
            for z in -16..32 {
                region.set_block(BlockPos::new(x, 30, z), stone);
                for y in 31..63 {
                    region.set_block(BlockPos::new(x, y, z), water);
                }
            }
        }
        region
    }

    fn features() -> Vec<Arc<dyn Feature>> {
        let mut registry = Registry::builtin();
        [
            json!({"type": "minecraft:iceberg", "state": {"Name": "minecraft:packed_ice"}}),
            json!({"type": "minecraft:iceberg", "state": {"Name": "minecraft:blue_ice"}}),
            json!({"type": "minecraft:blue_ice"}),
            json!({"type": "minecraft:spike", "state": {"Name": "minecraft:packed_ice"},
                "can_place_on": {"type": "minecraft:matching_blocks", "blocks": "minecraft:stone"},
                "can_replace": {"type": "minecraft:matching_blocks", "blocks": ["minecraft:water", "minecraft:packed_ice"]}}),
        ]
        .iter()
        .map(|v| registry.feature(v).unwrap())
        .collect()
    }

    fn hash(region: &Region) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        for y in 0..120 {
            for z in -16..32 {
                for x in -16..32 {
                    h ^= region.block(BlockPos::new(x, y, z)).id() as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
            }
        }
        h
    }

    #[tuff_core::watchdog]
    #[test]
    fn ice_features_place_the_pinned_blocks_for_fixed_seeds() {
        let features = features();
        let mut hashes = Vec::new();
        for seed in 0..6i64 {
            let mut region = ocean();
            let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(seed)));
            for (i, feature) in features.iter().enumerate() {
                for k in 0..3 {
                    let origin = BlockPos::new(2 + 5 * k, 62 - (i as i32 % 3), 3 + 4 * k);
                    let mut ctx = PlaceContext {
                        level: &mut region,
                        random: &mut random,
                    };
                    feature.place(&mut ctx, origin);
                }
            }
            hashes.push(hash(&region));
        }
        assert_eq!(hashes, PINNED);
    }

    #[tuff_core::watchdog]
    #[test]
    fn icebergs_alone_match_pinned_hashes() {
        let features = features();
        let mut h = 0u64;
        for seed in 100..160i64 {
            let mut region = ocean();
            let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(seed)));
            let mut ctx = PlaceContext {
                level: &mut region,
                random: &mut random,
            };
            features[(seed % 2) as usize].place(&mut ctx, BlockPos::new(8, 63, 8));
            h = h.rotate_left(7) ^ hash(&region);
        }
        assert_eq!(h, 2524394771601770865);
    }

    const PINNED: [u64; 6] = [
        16587836838303593383,
        2153888555301100102,
        6122210456344175348,
        10412475054149112966,
        1123884048800877603,
        17086517688845245561,
    ];

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "release timing run"]
    fn iceberg_placement_timing() {
        let features = features();
        let mut region = ocean();
        let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(9)));
        let start = std::time::Instant::now();
        for i in 0..4000 {
            let mut ctx = PlaceContext {
                level: &mut region,
                random: &mut random,
            };
            features[i % 2].place(&mut ctx, BlockPos::new(8, 63, 8));
        }
        eprintln!(
            "ICEBERG_TIMING_MS {}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
