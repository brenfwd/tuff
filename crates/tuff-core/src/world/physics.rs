//! How mobs move through the world: box collision against block shapes
//! with stepping up, walking, falling, swimming in water and lava, fall
//! damage and the block factors (friction, speed, jump) under them.
//!
//! Blocks come through a lookup that answers `None` where the region cannot
//! see; those count as solid, so a mob never walks into unknown space.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::Attribute;

use crate::world::contact as fluid;
use tuff_world_model::BlockPos;

pub type Aabb = [f64; 6];
pub use crate::world::entity::BlockLookup;

/// The collision shape of the block at a position.
pub type ShapeLookup<'a> = &'a dyn Fn(BlockPos) -> Shape;

/// A block's boxes in block coordinates and the shift the game gives them
/// at the block's position (bamboo, dripstone).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub boxes: &'static [[f32; 6]],
    pub offset: [f64; 3],
}

impl From<&'static [[f32; 6]]> for Shape {
    fn from(boxes: &'static [[f32; 6]]) -> Self {
        Shape {
            boxes,
            offset: [0.0; 3],
        }
    }
}

impl Shape {
    /// The collision shape of a block state at `pos`; an unknown block is
    /// a full cube.
    pub fn collision(state: Option<BlockState>, pos: BlockPos) -> Shape {
        let offset = state
            .and_then(|s| s.block().shape_offset())
            .filter(|o| o.collision)
            .map_or([0.0; 3], |o| o.at(pos.x, pos.z));
        Shape {
            boxes: shape_of(state),
            offset,
        }
    }

    /// The outline (what the cursor targets) of a block state at `pos`.
    pub fn outline(state: BlockState, pos: BlockPos) -> Shape {
        let offset = state
            .block()
            .shape_offset()
            .filter(|o| o.outline)
            .map_or([0.0; 3], |o| o.at(pos.x, pos.z));
        Shape {
            boxes: state.outline_boxes(),
            offset,
        }
    }

    /// The boxes moved to `pos`, in world coordinates.
    pub fn placed(self, pos: BlockPos) -> impl Iterator<Item = Aabb> {
        let [dx, dy, dz] = self.offset;
        placed_boxes(self.boxes, pos).map(move |b| offset(b, [dx, dy, dz]))
    }
}

const EPSILON: f64 = 1.0e-7;
const FULL_BLOCK: [f32; 6] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
const WATER_FLUIDS: tuff_vanilla_data::tag::Key<tuff_vanilla_data::fluid::Fluid> =
    tuff_vanilla_data::tag::fluid::key::WATER;
const LAVA_FLUIDS: tuff_vanilla_data::tag::Key<tuff_vanilla_data::fluid::Fluid> =
    tuff_vanilla_data::tag::fluid::key::LAVA;
pub const CLIMBING_SPEED: f64 = 0.2;

pub fn block_pos(position: [f64; 3]) -> BlockPos {
    BlockPos::new(
        position[0].floor() as i32,
        position[1].floor() as i32,
        position[2].floor() as i32,
    )
}

pub fn intersects(a: Aabb, b: Aabb) -> bool {
    a[0] < b[3] && a[3] > b[0] && a[1] < b[4] && a[4] > b[1] && a[2] < b[5] && a[5] > b[2]
}

pub fn offset(bounds: Aabb, by: [f64; 3]) -> Aabb {
    [
        bounds[0] + by[0],
        bounds[1] + by[1],
        bounds[2] + by[2],
        bounds[3] + by[0],
        bounds[4] + by[1],
        bounds[5] + by[2],
    ]
}

/// The box grown towards `by` on each axis.
pub fn expand_towards(bounds: Aabb, by: [f64; 3]) -> Aabb {
    let mut out = bounds;
    for axis in 0..3 {
        if by[axis] < 0.0 {
            out[axis] += by[axis];
        } else {
            out[axis + 3] += by[axis];
        }
    }
    out
}

pub fn inflate(bounds: Aabb, by: [f64; 3]) -> Aabb {
    [
        bounds[0] - by[0],
        bounds[1] - by[1],
        bounds[2] - by[2],
        bounds[3] + by[0],
        bounds[4] + by[1],
        bounds[5] + by[2],
    ]
}

/// The collision boxes of a block state at `pos`, in world coordinates; an
/// unknown block is a full cube.
pub fn block_boxes(state: Option<BlockState>, pos: BlockPos) -> impl Iterator<Item = Aabb> {
    Shape::collision(state, pos).placed(pos)
}

/// The collision boxes of a block state in block coordinates, before any
/// per-position offset ([`Shape::collision`] applies it); an unknown block
/// is a full cube.
pub fn shape_of(state: Option<BlockState>) -> &'static [[f32; 6]] {
    match state {
        Some(state) => state.collision_boxes(),
        None => std::slice::from_ref(&FULL_BLOCK),
    }
}

/// Block-relative boxes moved to `pos`.
pub fn placed_boxes(boxes: &'static [[f32; 6]], pos: BlockPos) -> impl Iterator<Item = Aabb> {
    let (x, y, z) = (pos.x as f64, pos.y as f64, pos.z as f64);
    boxes.iter().map(move |b| {
        [
            x + b[0] as f64,
            y + b[1] as f64,
            z + b[2] as f64,
            x + b[3] as f64,
            y + b[4] as f64,
            z + b[5] as f64,
        ]
    })
}

/// Every block collision box meeting `area`.
pub fn colliders(area: Aabb, block: BlockLookup) -> Vec<Aabb> {
    colliders_shaped(area, &|pos| Shape::collision(block(pos), pos))
}

/// Every collision box meeting `area`, with each block's boxes from `shape`.
pub fn colliders_shaped(area: Aabb, shape: ShapeLookup) -> Vec<Aabb> {
    colliders_with(area, shape, &[])
}

/// [`colliders_shaped`] plus the `extra` boxes meeting `area`.
pub fn colliders_with(area: Aabb, shape: ShapeLookup, extra: &[Aabb]) -> Vec<Aabb> {
    let mut out = Vec::new();
    colliders_into(area, shape, extra, &mut out);
    out
}

/// [`colliders_with`] appending to `out`.
fn colliders_into(area: Aabb, shape: ShapeLookup, extra: &[Aabb], out: &mut Vec<Aabb>) {
    let min = |axis: usize| (area[axis] - EPSILON).floor() as i32 - 1;
    let max = |axis: usize| (area[axis + 3] + EPSILON).floor() as i32 + 1;
    for x in min(0)..=max(0) {
        for z in min(2)..=max(2) {
            for y in min(1)..=max(1) {
                let pos = BlockPos::new(x, y, z);
                out.extend(shape(pos).placed(pos).filter(|b| intersects(*b, area)));
            }
        }
    }
    out.extend(extra.iter().filter(|b| intersects(**b, area)));
}

thread_local! {
    static COLLIDERS: std::cell::RefCell<Vec<Aabb>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Runs `f` on the colliders meeting `area`, gathered into a buffer this
/// thread reuses.
fn with_colliders<R>(
    area: Aabb,
    shape: ShapeLookup,
    extra: &[Aabb],
    f: impl FnOnce(&[Aabb]) -> R,
) -> R {
    let mut shapes = COLLIDERS.with(|c| std::mem::take(&mut *c.borrow_mut()));
    shapes.clear();
    colliders_into(area, shape, extra, &mut shapes);
    let result = f(&shapes);
    COLLIDERS.with(|c| *c.borrow_mut() = shapes);
    result
}

/// Whether nothing solid meets `area`.
pub fn no_collision(area: Aabb, block: BlockLookup) -> bool {
    colliders(area, block).is_empty()
}

/// The box `kind` needs free to spawn with its feet at `position`.
pub fn spawn_box(kind: tuff_vanilla_data::registry::EntityType, position: [f64; 3]) -> Aabb {
    kind.spawn_box(position)
}

/// Whether nothing in the way stops `kind` spawning in `bounds`.
pub fn unobstructed_for(
    kind: tuff_vanilla_data::registry::EntityType,
    bounds: Aabb,
    block: BlockLookup,
) -> bool {
    if kind.spawns_in_liquid() {
        no_collision(bounds, block)
    } else {
        unobstructed(bounds, block)
    }
}

/// Whether nothing solid or liquid is in the way of a mob's box.
pub fn unobstructed(bounds: Aabb, block: BlockLookup) -> bool {
    if !no_collision(bounds, block) {
        return false;
    }
    let min = block_pos([bounds[0], bounds[1], bounds[2]]);
    let max = [
        bounds[3].ceil() as i32,
        bounds[4].ceil() as i32,
        bounds[5].ceil() as i32,
    ];
    for x in min.x..max[0] {
        for y in min.y..max[1] {
            for z in min.z..max[2] {
                if block(BlockPos::new(x, y, z)).is_some_and(|s| !s.fluid_state().is_empty()) {
                    return false;
                }
            }
        }
    }
    true
}

fn clip(axis: usize, bounds: Aabb, other: Aabb, mut distance: f64) -> f64 {
    let (a, b) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let overlaps =
        |o: usize| bounds[o + 3] - EPSILON > other[o] && bounds[o] + EPSILON < other[o + 3];
    if !overlaps(a) || !overlaps(b) {
        return distance;
    }
    if distance > 0.0 && other[axis] >= bounds[axis + 3] - EPSILON {
        distance = distance.min(other[axis] - bounds[axis + 3]);
    } else if distance < 0.0 && other[axis + 3] <= bounds[axis] + EPSILON {
        distance = distance.max(other[axis + 3] - bounds[axis]);
    }
    distance
}

fn collide_axis(axis: usize, bounds: Aabb, shapes: &[Aabb], distance: f64) -> f64 {
    if distance.abs() < EPSILON {
        return 0.0;
    }
    shapes
        .iter()
        .fold(distance, |d, shape| clip(axis, bounds, *shape, d))
}

/// Moves a box through `shapes` one axis at a time: y first, then the
/// horizontal axis with the larger movement.
pub fn collide_with_shapes(movement: [f64; 3], bounds: Aabb, shapes: &[Aabb]) -> [f64; 3] {
    if shapes.is_empty() {
        return movement;
    }
    let order = if movement[0].abs() < movement[2].abs() {
        [1, 2, 0]
    } else {
        [1, 0, 2]
    };
    let mut resolved = [0.0; 3];
    for axis in order {
        if movement[axis] != 0.0 {
            resolved[axis] = collide_axis(axis, offset(bounds, resolved), shapes, movement[axis]);
        }
    }
    resolved
}

fn step_heights(bounds: Aabb, shapes: &[Aabb], max_step: f32, skip: f32) -> Vec<f32> {
    let mut heights: Vec<f32> = Vec::new();
    for shape in shapes {
        for y in [shape[1], shape[4]] {
            let relative = (y - bounds[1]) as f32;
            if relative >= 0.0
                && relative != skip
                && relative <= max_step
                && !heights.contains(&relative)
            {
                heights.push(relative);
            }
        }
    }
    heights.sort_by(f32::total_cmp);
    heights
}

/// How far a box at `bounds` really moves when trying to move by
/// `movement`, stepping up onto blocks up to `step_height` high.
pub fn collide(
    bounds: Aabb,
    movement: [f64; 3],
    step_height: f32,
    on_ground: bool,
    block: BlockLookup,
) -> [f64; 3] {
    collide_shaped(bounds, movement, step_height, on_ground, &|pos| {
        Shape::collision(block(pos), pos)
    })
}

/// [`collide`] with each block's collision boxes from `block`, for things
/// some blocks collide with differently (powder snow walkers).
pub fn collide_shaped(
    bounds: Aabb,
    movement: [f64; 3],
    step_height: f32,
    on_ground: bool,
    block: ShapeLookup,
) -> [f64; 3] {
    collide_with(bounds, movement, step_height, on_ground, block, &[])
}

/// [`collide_shaped`] also against `extra` boxes (blocks pistons are
/// moving).
pub fn collide_with(
    bounds: Aabb,
    movement: [f64; 3],
    step_height: f32,
    on_ground: bool,
    block: ShapeLookup,
    extra: &[Aabb],
) -> [f64; 3] {
    let length_sq = movement.iter().map(|v| v * v).sum::<f64>();
    let step = if length_sq == 0.0 {
        movement
    } else {
        with_colliders(expand_towards(bounds, movement), block, extra, |shapes| {
            collide_with_shapes(movement, bounds, shapes)
        })
    };
    let x_hit = movement[0] != step[0];
    let y_hit = movement[1] != step[1];
    let z_hit = movement[2] != step[2];
    let landed = y_hit && movement[1] < 0.0;
    if step_height > 0.0 && (landed || on_ground) && (x_hit || z_hit) {
        let grounded = if landed {
            offset(bounds, [0.0, step[1], 0.0])
        } else {
            bounds
        };
        let mut area = expand_towards(grounded, [movement[0], step_height as f64, movement[2]]);
        if !landed {
            area = expand_towards(area, [0.0, -1.0e-5, 0.0]);
        }
        let horizontal = step[0] * step[0] + step[2] * step[2];
        let stepped = with_colliders(area, block, extra, |shapes| {
            for height in step_heights(grounded, shapes, step_height, step[1] as f32) {
                let stepped = collide_with_shapes(
                    [movement[0], height as f64, movement[2]],
                    grounded,
                    shapes,
                );
                if stepped[0] * stepped[0] + stepped[2] * stepped[2] > horizontal {
                    let down = bounds[1] - grounded[1];
                    return Some([stepped[0], stepped[1] - down, stepped[2]]);
                }
            }
            None
        });
        if let Some(stepped) = stepped {
            return stepped;
        }
    }
    step
}

/// Stops an entity that moved from `start` to where it is now at the
/// `boxes` of blocks pistons are moving, which cell-granular movement does
/// not see: it ends against them with that axis' velocity gone. Returns
/// whether it came down onto one.
pub fn stop_at_moving_blocks(
    base: &mut crate::world::entity::EntityBase,
    start: [f64; 3],
    boxes: &[Aabb],
) -> bool {
    if boxes.is_empty() {
        return false;
    }
    let moved: [f64; 3] = std::array::from_fn(|axis| base.position[axis] - start[axis]);
    let before = offset(base.bounds(), moved.map(|v| -v));
    let area = expand_towards(before, moved);
    let near: Vec<Aabb> = boxes
        .iter()
        .copied()
        .filter(|b| intersects(inflate(area, [EPSILON; 3]), *b))
        .collect();
    let kept = collide_with_shapes(moved, before, &near);
    let mut landed = false;
    for axis in 0..3 {
        if (kept[axis] - moved[axis]).abs() > EPSILON {
            base.position[axis] = start[axis] + kept[axis];
            base.velocity[axis] = 0.0;
            if axis == 1 && moved[1] < 0.0 {
                base.on_ground = true;
                landed = true;
            }
        }
    }
    landed
}

/// The block an entity standing on the ground is supported by: of the
/// blocks just below its feet, the one closest to its center.
pub fn supporting_block(bounds: Aabb, block: BlockLookup) -> Option<BlockPos> {
    let area = [
        bounds[0],
        bounds[1] - 1.0e-6,
        bounds[2],
        bounds[3],
        bounds[1],
        bounds[5],
    ];
    let center = [
        (bounds[0] + bounds[3]) / 2.0,
        bounds[1],
        (bounds[2] + bounds[5]) / 2.0,
    ];
    let mut best: Option<(f64, BlockPos)> = None;
    for x in (area[0] - EPSILON).floor() as i32..=(area[3] + EPSILON).floor() as i32 {
        for z in (area[2] - EPSILON).floor() as i32..=(area[5] + EPSILON).floor() as i32 {
            for y in (area[1] - EPSILON).floor() as i32 - 1..=(area[4] + EPSILON).floor() as i32 {
                let pos = BlockPos::new(x, y, z);
                if !block_boxes(block(pos), pos).any(|b| intersects(b, area)) {
                    continue;
                }
                let block_center = pos.center();
                let distance = (0..3)
                    .map(|a| (block_center[a] - center[a]).powi(2))
                    .sum::<f64>();
                let closer = best.is_none_or(|(d, p)| {
                    distance < d || (distance == d && (pos.y, pos.z, pos.x) > (p.y, p.z, p.x))
                });
                if closer {
                    best = Some((distance, pos));
                }
            }
        }
    }
    best.map(|(_, pos)| pos)
}

/// How deep a box reaches into water and lava, measured from its bottom.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FluidHeights {
    pub water: f64,
    pub lava: f64,
    /// Whether water covers the eye position.
    pub eyes_in_water: bool,
}

pub fn fluid_heights(bounds: Aabb, eye: [f64; 3], block: BlockLookup) -> FluidHeights {
    let bounds = inflate(bounds, [-0.001; 3]);
    let mut heights = FluidHeights::default();
    for x in bounds[0].floor() as i32..bounds[3].ceil() as i32 {
        for y in bounds[1].floor() as i32..bounds[4].ceil() as i32 {
            for z in bounds[2].floor() as i32..bounds[5].ceil() as i32 {
                let pos = BlockPos::new(x, y, z);
                for (kind, height) in [
                    (WATER_FLUIDS, &mut heights.water),
                    (LAVA_FLUIDS, &mut heights.lava),
                ] {
                    let Some(surface) = fluid::surface(pos, kind, block) else {
                        continue;
                    };
                    let top = y as f64 + surface;
                    if top >= bounds[1] {
                        *height = height.max(top - bounds[1]);
                    }
                }
            }
        }
    }
    heights.eyes_in_water = fluid::eyes_in(eye, WATER_FLUIDS, block);
    heights
}

/// `1 - (1 - friction) * modifier`, clamped to 0..=1.
pub fn modified_friction(friction: f32, modifier: f32) -> f32 {
    (1.0 - (1.0 - friction) * modifier).clamp(0.0, 1.0)
}

/// The walking input turned into a velocity change facing `yaw`.
pub fn input_vector(input: [f64; 3], speed: f32, yaw: f32) -> [f64; 3] {
    let length = input.iter().map(|v| v * v).sum::<f64>();
    if length < 1.0e-7 {
        return [0.0; 3];
    }
    let scale = if length > 1.0 {
        speed as f64 / length.sqrt()
    } else {
        speed as f64
    };
    let movement = input.map(|v| v * scale);
    let radians = yaw * (std::f32::consts::PI / 180.0);
    let (sin, cos) = (radians.sin() as f64, radians.cos() as f64);
    [
        movement[0] * cos - movement[2] * sin,
        movement[1],
        movement[2] * cos + movement[0] * sin,
    ]
}

/// The attribute values movement reads.
#[derive(Debug, Clone, Copy)]
pub struct Movement {
    pub speed: f32,
    pub gravity: f64,
    pub step_height: f32,
    pub jump_strength: f32,
    pub water_efficiency: f32,
    pub movement_efficiency: f32,
    pub friction_modifier: f32,
    pub air_drag_modifier: f32,
}

impl Movement {
    pub fn read(value: impl Fn(Attribute) -> f64) -> Self {
        Self {
            speed: value(Attribute::MovementSpeed) as f32,
            gravity: value(Attribute::Gravity),
            step_height: value(Attribute::StepHeight) as f32,
            jump_strength: value(Attribute::JumpStrength) as f32,
            water_efficiency: value(Attribute::WaterMovementEfficiency) as f32,
            movement_efficiency: value(Attribute::MovementEfficiency) as f32,
            friction_modifier: value(Attribute::FrictionModifier) as f32,
            air_drag_modifier: value(Attribute::AirDragModifier) as f32,
        }
    }
}

/// A value from the game's triangle distribution around `mode`.
pub fn triangle(rng: &mut rand::rngs::StdRng, mode: f64, deviation: f64) -> f64 {
    use rand::Rng;
    mode + deviation * (rng.r#gen::<f64>() - rng.r#gen::<f64>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn floor(pos: BlockPos) -> Option<BlockState> {
        Some(if pos.y < 64 {
            Block::Stone.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    fn cow_box(position: [f64; 3]) -> Aabb {
        [
            position[0] - 0.45,
            position[1],
            position[2] - 0.45,
            position[0] + 0.45,
            position[1] + 1.4,
            position[2] + 0.45,
        ]
    }

    #[tuff_core::watchdog]
    #[test]
    fn falling_boxes_land_on_the_floor() {
        let moved = collide(
            cow_box([0.5, 64.5, 0.5]),
            [0.0, -1.0, 0.0],
            0.6,
            false,
            &floor,
        );
        assert!((moved[1] + 0.5).abs() < 1e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn walls_stop_and_low_steps_are_climbed() {
        let wall = |pos: BlockPos| {
            Some(if pos.y < 64 || (pos.x == 2 && pos.y == 64) {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let bounds = cow_box([1.0, 64.0, 0.5]);
        let blocked = collide(bounds, [0.8, 0.0, 0.0], 0.0, true, &wall);
        assert!((blocked[0] - 0.55).abs() < 1e-9, "{blocked:?}");
        let slab = |pos: BlockPos| {
            Some(if pos.y < 64 {
                Block::Stone.default_state()
            } else if pos.x == 2 && pos.y == 64 {
                Block::StoneSlab.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let stepped = collide(bounds, [0.8, 0.0, 0.0], 0.6, true, &slab);
        assert!(
            (stepped[0] - 0.8).abs() < 1e-9 && (stepped[1] - 0.5).abs() < 1e-9,
            "{stepped:?}"
        );
        let too_high = collide(bounds, [0.8, 0.0, 0.0], 0.6, true, &wall);
        assert!((too_high[0] - 0.55).abs() < 1e-9 && too_high[1] == 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unknown_blocks_are_solid() {
        let unknown = |pos: BlockPos| (pos.x < 3).then(|| Block::Air.default_state());
        let moved = collide(
            cow_box([1.0, 64.0, 0.5]),
            [2.0, 0.0, 0.0],
            0.6,
            false,
            &unknown,
        );
        assert!((moved[0] - 1.55).abs() < 1e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn fluid_heights_measure_from_the_feet() {
        let pond = |pos: BlockPos| {
            Some(if pos.y < 63 {
                Block::Stone.default_state()
            } else if pos.y == 63 {
                Block::Water.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let heights = fluid_heights(cow_box([0.5, 63.0, 0.5]), [0.5, 64.3, 0.5], &pond);
        assert!(heights.water > 0.8 && heights.lava == 0.0);
        assert!(!heights.eyes_in_water);
        let heights = fluid_heights(cow_box([0.5, 62.0, 0.5]), [0.5, 63.3, 0.5], &pond);
        assert!(heights.eyes_in_water);
    }

    #[tuff_core::watchdog]
    #[test]
    fn supporting_block_is_the_closest_under_the_feet() {
        let ledge = |pos: BlockPos| {
            Some(if pos.y == 63 && pos.x == 0 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let support = supporting_block(cow_box([1.2, 64.0, 0.5]), &ledge);
        assert_eq!(support, Some(BlockPos::new(0, 63, 0)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn supporting_block_tie_prefers_the_greater_position() {
        let floor = |pos: BlockPos| {
            Some(if pos.y == 63 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let support = supporting_block(cow_box([1.0, 64.0, 0.5]), &floor);
        assert_eq!(support, Some(BlockPos::new(1, 63, 0)));
    }
}
