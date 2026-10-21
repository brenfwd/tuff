//! Ray casts through blocks the way the game picks what a player looks at:
//! block outline shapes, and optionally fluid surfaces.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;

use tuff_world_model::BlockPos;

/// Blocks by position, as a ray sees them.
pub type Level<'a> = &'a dyn Fn(BlockPos) -> BlockState;

/// Which fluids stop the ray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluidMode {
    None,
    /// Only source blocks (and water held by waterlogged blocks).
    SourceOnly,
    /// Any fluid surface, flowing too.
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub pos: BlockPos,
    pub face: Facing,
    pub location: [f64; 3],
}

/// A fluid in a block state: whether it is lava, its amount (8 for a
/// source or a falling column) and whether it is a source.
pub fn fluid(state: BlockState) -> Option<(bool, u8, bool)> {
    let fluid = state.fluid_state();
    if fluid.is_empty() {
        return None;
    }
    let lava = tuff_datapack::tags::is(tuff_vanilla_data::tag::fluid::key::LAVA, fluid.fluid());
    Some((lava, fluid.amount() as u8, fluid.is_source()))
}

/// The height of the fluid surface in `pos`: full if the same fluid lies
/// above, else amount / 9.
fn fluid_height(pos: BlockPos, lava: bool, amount: u8, level: Level) -> f64 {
    let above = level(BlockPos::new(pos.x, pos.y + 1, pos.z));
    if fluid(above).is_some_and(|(above_lava, _, _)| above_lava == lava) {
        1.0
    } else {
        amount as f64 / 9.0
    }
}

/// Where the segment `from`..`to` first enters `boxes` placed at `pos`, as
/// the fraction along the segment and the face it enters through.
fn clip_boxes(
    from: [f64; 3],
    to: [f64; 3],
    pos: BlockPos,
    boxes: &[[f64; 6]],
) -> Option<(f64, Facing)> {
    let base = [pos.x as f64, pos.y as f64, pos.z as f64];
    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let test = [0, 1, 2].map(|i| from[i] + delta[i] * 0.001);
    let inside = boxes
        .iter()
        .any(|b| (0..3).all(|i| test[i] >= base[i] + b[i] && test[i] < base[i] + b[i + 3]));
    if inside {
        return Some((0.001, nearest(delta).opposite()));
    }
    const EPSILON: f64 = 1.0e-7;
    let mut scale = 1.0;
    let mut best = None;
    for b in boxes {
        let min = [base[0] + b[0], base[1] + b[1], base[2] + b[2]];
        let max = [base[0] + b[3], base[1] + b[4], base[2] + b[5]];
        for axis in 0..3 {
            let (plane, face) = if delta[axis] > EPSILON {
                (min[axis], negative_face(axis))
            } else if delta[axis] < -EPSILON {
                (max[axis], negative_face(axis).opposite())
            } else {
                continue;
            };
            let t = (plane - from[axis]) / delta[axis];
            if !(0.0 < t && t < scale) {
                continue;
            }
            let within = (0..3).filter(|other| *other != axis).all(|other| {
                let at = from[other] + t * delta[other];
                min[other] - EPSILON < at && at < max[other] + EPSILON
            });
            if within {
                scale = t;
                best = Some((t, face));
            }
        }
    }
    best
}

/// The direction closest to `vector`.
fn nearest(vector: [f64; 3]) -> Facing {
    let axis = (0..3)
        .max_by(|a, b| vector[*a].abs().total_cmp(&vector[*b].abs()))
        .unwrap_or(1);
    if vector[axis] > 0.0 {
        negative_face(axis).opposite()
    } else {
        negative_face(axis)
    }
}

fn negative_face(axis: usize) -> Facing {
    match axis {
        0 => Facing::West,
        1 => Facing::Down,
        _ => Facing::North,
    }
}

fn frac(value: f64) -> f64 {
    value - value.floor()
}

fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// The first block (or fluid surface) the segment `from`..`to` hits,
/// by outline shape as a player's aim does.
pub fn cast(from: [f64; 3], to: [f64; 3], fluids: FluidMode, level: Level) -> Option<Hit> {
    traverse(from, to, fluids, Shapes::Outline, level)
}

/// The first block whose collision shape blocks the segment.
pub fn cast_colliders(from: [f64; 3], to: [f64; 3], level: Level) -> Option<Hit> {
    traverse(from, to, FluidMode::None, Shapes::Collision, level)
}

/// The first block whose visual shape blocks the segment (glass, panes,
/// bars and copper grates let it through).
pub fn cast_visual(from: [f64; 3], to: [f64; 3], level: Level) -> Option<Hit> {
    traverse(from, to, FluidMode::None, Shapes::Visual, level)
}

#[derive(Clone, Copy, PartialEq)]
enum Shapes {
    Outline,
    Collision,
    Visual,
}

fn visual_shape(state: BlockState, pos: BlockPos) -> crate::world::physics::Shape {
    use crate::world::physics::Shape;
    let name = state.block().name();
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    let empty = name == "glass"
        || name == "tinted_glass"
        || name == "powder_snow"
        || name.ends_with("_stained_glass")
        || name.ends_with("copper_grate")
        || name.ends_with("glass_pane")
        || name.ends_with("iron_bars")
        || name.ends_with("copper_bars");
    if empty {
        return Shape {
            boxes: &[],
            offset: [0.0; 3],
        };
    }
    if name == "mud" || name == "soul_sand" {
        return Shape {
            boxes: &[[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
            offset: [0.0; 3],
        };
    }
    if name == "snow" || name.ends_with("_fence") {
        return Shape::outline(state, pos);
    }
    Shape::collision(Some(state), pos)
}

fn traverse(
    from: [f64; 3],
    to: [f64; 3],
    fluids: FluidMode,
    shapes: Shapes,
    level: Level,
) -> Option<Hit> {
    if from == to {
        return None;
    }
    let check = |pos: BlockPos| -> Option<Hit> {
        let state = level(pos);
        let shape = match shapes {
            Shapes::Collision => crate::world::physics::Shape::collision(Some(state), pos),
            Shapes::Outline => crate::world::physics::Shape::outline(state, pos),
            Shapes::Visual => visual_shape(state, pos),
        };
        let [dx, dy, dz] = shape.offset;
        let outline: Vec<[f64; 6]> = shape
            .boxes
            .iter()
            .map(|b| {
                let b = b.map(f64::from);
                [
                    b[0] + dx,
                    b[1] + dy,
                    b[2] + dz,
                    b[3] + dx,
                    b[4] + dy,
                    b[5] + dz,
                ]
            })
            .collect();
        let mut hit = clip_boxes(from, to, pos, &outline);
        if let Some((lava, amount, source)) = fluid(state)
            && (fluids == FluidMode::Any || (fluids == FluidMode::SourceOnly && source))
        {
            let height = fluid_height(pos, lava, amount, level);
            let surface = clip_boxes(from, to, pos, &[[0.0, 0.0, 0.0, 1.0, height, 1.0]]);
            if let Some((t, face)) = surface
                && hit.is_none_or(|(best, _)| t < best)
            {
                hit = Some((t, face));
            }
        }
        hit.map(|(t, face)| Hit {
            pos,
            face,
            location: [
                lerp(t, from[0], to[0]),
                lerp(t, from[1], to[1]),
                lerp(t, from[2], to[2]),
            ],
        })
    };
    let to_ = [0, 1, 2].map(|i| lerp(-1.0e-7, to[i], from[i]));
    let from_ = [0, 1, 2].map(|i| lerp(-1.0e-7, from[i], to[i]));
    let mut current = from_.map(|c| c.floor() as i32);
    if let Some(hit) = check(BlockPos::new(current[0], current[1], current[2])) {
        return Some(hit);
    }
    let delta = [0, 1, 2].map(|i| to_[i] - from_[i]);
    let sign = delta.map(|d| {
        if d > 0.0 {
            1
        } else if d < 0.0 {
            -1
        } else {
            0
        }
    });
    let step = [0, 1, 2].map(|i| {
        if sign[i] == 0 {
            f64::MAX
        } else {
            sign[i] as f64 / delta[i]
        }
    });
    let mut t = [0, 1, 2].map(|i| {
        step[i]
            * if sign[i] > 0 {
                1.0 - frac(from_[i])
            } else {
                frac(from_[i])
            }
    });
    while t.iter().any(|t| *t <= 1.0) {
        let axis = if t[0] < t[1] {
            if t[0] < t[2] { 0 } else { 2 }
        } else if t[1] < t[2] {
            1
        } else {
            2
        };
        current[axis] += sign[axis];
        t[axis] += step[axis];
        if let Some(hit) = check(BlockPos::new(current[0], current[1], current[2])) {
            return Some(hit);
        }
    }
    None
}

/// The direction a player looks, from yaw and pitch in degrees.
pub fn view_vector(yaw: f32, pitch: f32) -> [f64; 3] {
    let (yaw, pitch) = ((yaw as f64).to_radians(), (pitch as f64).to_radians());
    [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn world(pos: BlockPos) -> BlockState {
        match pos.y {
            ..=63 => Block::Stone.default_state(),
            64 if pos.x == 2 => Block::Water.default_state(),
            64 if pos.x == 4 => Block::Torch.default_state(),
            _ => Block::Air.default_state(),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn rays_hit_outlines_and_optionally_fluid_sources() {
        let eye = [2.5, 66.0, 0.5];
        let down = [2.5, 60.0, 0.5];
        let hit = cast(eye, down, FluidMode::SourceOnly, &world).unwrap();
        assert_eq!(hit.pos, BlockPos::new(2, 64, 0));
        assert_eq!(hit.face, Facing::Up);
        assert!((hit.location[1] - (64.0 + 8.0 / 9.0)).abs() < 1e-9);
        let hit = cast(eye, down, FluidMode::None, &world).unwrap();
        assert_eq!(hit.pos, BlockPos::new(2, 63, 0));

        let hit = cast([4.5, 66.0, 0.5], [4.5, 60.0, 0.5], FluidMode::None, &world).unwrap();
        assert_eq!(hit.pos, BlockPos::new(4, 64, 0), "torches have an outline");
        let hit = cast([0.5, 64.5, 0.5], [0.5, 64.5, 6.5], FluidMode::None, &world);
        assert_eq!(hit, None);
        let side = cast([-1.5, 63.5, 0.5], [3.5, 63.5, 0.5], FluidMode::None, &world).unwrap();
        assert_eq!(side.face, Facing::West);
        let look = view_vector(0.0, 90.0);
        assert!((look[1] + 1.0).abs() < 1e-9);
    }
}
