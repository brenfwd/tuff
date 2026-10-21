//! What a box or a point touches: fluid surfaces and fire blocks, as
//! players, mobs and items feel them. Pure queries over a block lookup.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::{Facing, Support};
use tuff_vanilla_data::fluid::Fluid;
use tuff_vanilla_data::tag::Key;

use tuff_world_model::BlockPos;

/// Ticks a feature's portal ignores a player who just arrived or entered.
pub const PLUGIN_PORTAL_COOLDOWN: u32 = 10;

/// Blocks features claim as their own portals: the built-in behaviour of a
/// block of a claimed kind (an end gateway's travel, its beams and
/// cooldowns) does not apply. Kept as a world service.
#[derive(Default)]
pub struct PortalClaims(std::sync::RwLock<std::sync::Arc<[String]>>);

impl PortalClaims {
    /// Replaces the claimed block ids (`namespace:name`).
    pub fn set(&self, blocks: Vec<String>) {
        if let Ok(mut claimed) = self.0.write() {
            *claimed = blocks.into();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.read().is_ok_and(|claimed| claimed.is_empty())
    }

    /// Whether `block` (a block name, the `minecraft:` namespace optional)
    /// is claimed.
    pub fn claims(&self, block: &str) -> bool {
        let bare = |name: &str| name.strip_prefix("minecraft:").unwrap_or(name).to_owned();
        let block = bare(block);
        self.0
            .read()
            .is_ok_and(|claimed| claimed.iter().any(|name| bare(name) == block))
    }
}

impl crate::world::services::WorldService for PortalClaims {
    fn start(&self, _shared: &std::sync::Arc<crate::world::Shared>) {}
}

/// A portal block a player's body overlaps, as its home region reports
/// it. Which blocks are portals and where they lead is up to features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalContact {
    pub pos: BlockPos,
    pub block: BlockState,
    /// A feature claimed this block as its own portal: the built-in portal
    /// behaviour for the block does not apply.
    pub claimed: bool,
}

/// Whether a block stops movement (the motion-blocking heightmap's rule).
pub fn blocks_motion(state: BlockState) -> bool {
    !tuff_world_model::chunk::is_air(state)
        && (!state.collision_boxes().is_empty()
            || state.is_liquid()
            || state.get("waterlogged") == Some("true"))
}

/// Whether two boxes (`[min x, min y, min z, max x, max y, max z]`)
/// overlap.
pub fn boxes_intersect(a: [f64; 6], b: [f64; 6]) -> bool {
    a[0] < b[3] && a[3] > b[0] && a[1] < b[4] && a[4] > b[1] && a[2] < b[5] && a[5] > b[2]
}

fn up(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y + 1, pos.z)
}

/// The height within its block of the surface of a fluid in `kind` at
/// `pos` (full when the same fluid lies above), or `None` if there is none.
pub fn surface(
    pos: BlockPos,
    kind: Key<Fluid>,
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<f64> {
    let fluid = block(pos)?.fluid_state();
    if !tuff_datapack::tags::is(kind, fluid.fluid()) {
        return None;
    }
    let same_above =
        block(up(pos)).is_some_and(|state| state.fluid_state().fluid().is_same(fluid.fluid()));
    Some(fluid.height(same_above) as f64)
}

/// Whether eyes at `eye` are below the surface of a fluid in `kind` as the
/// camera sees it: a source block under a ceiling sturdy on its underside
/// counts as full.
pub fn eyes_in(
    eye: [f64; 3],
    kind: Key<Fluid>,
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let pos = BlockPos::new(
        eye[0].floor() as i32,
        eye[1].floor() as i32,
        eye[2].floor() as i32,
    );
    let Some(fluid) = block(pos).map(BlockState::fluid_state) else {
        return false;
    };
    if !tuff_datapack::tags::is(kind, fluid.fluid()) {
        return false;
    }
    let above = block(up(pos));
    let height = if fluid.is_source()
        && above.is_some_and(|state| state.is_face_sturdy(Facing::Down, Support::Full))
    {
        1.0
    } else {
        fluid.height(above.is_some_and(|state| state.fluid_state().fluid().is_same(fluid.fluid())))
    };
    eye[1] <= pos.y as f64 + f64::from(height)
}

/// Whether a box (`[min x, min y, min z, max x, max y, max z]`), shrunk by
/// `inset`, reaches below the surface of a fluid in `kind`.
pub fn box_touches(
    bounds: [f64; 6],
    inset: f64,
    kind: Key<Fluid>,
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let min = [bounds[0] + inset, bounds[1] + inset, bounds[2] + inset];
    let max = [bounds[3] - inset, bounds[4] - inset, bounds[5] - inset];
    let range = |axis: usize| min[axis].floor() as i32..=max[axis].ceil() as i32 - 1;
    range(0).any(|x| {
        range(1).any(|y| {
            range(2).any(|z| {
                surface(BlockPos::new(x, y, z), kind, &block)
                    .is_some_and(|height| y as f64 + height >= min[1])
            })
        })
    })
}

/// The damage of the fire blocks a box `[min x, min y, min z, max x,
/// max y, max z]` stands in (their shape is the bottom sixteenth of the
/// block), if any.
pub fn fire_contact(
    bounds: [f64; 6],
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<f32> {
    const INSET: f64 = 1.0e-5;
    let (min, max) = (
        [bounds[0] + INSET, bounds[1] + INSET, bounds[2] + INSET],
        [bounds[3] - INSET, bounds[4] - INSET, bounds[5] - INSET],
    );
    let mut damage: Option<f32> = None;
    for x in min[0].floor() as i32..=max[0].floor() as i32 {
        for y in min[1].floor() as i32..=max[1].floor() as i32 {
            for z in min[2].floor() as i32..=max[2].floor() as i32 {
                let Some(fire) =
                    block(BlockPos::new(x, y, z)).and_then(|s| s.block().fire_damage())
                else {
                    continue;
                };
                if min[1] < y as f64 + 1.0 / 16.0 {
                    damage = Some(damage.map_or(fire, |d| d.max(fire)));
                }
            }
        }
    }
    damage
}

/// Scale of a water current's push per tick.
pub const WATER_CURRENT_SCALE: f64 = 0.014;
/// Scale of a lava current's push per tick where lava is fast, and where
/// it is slow.
pub const LAVA_CURRENT_SCALE: [f64; 2] = [0.007, 0.0023333333333333335];
const MIN_CURRENT_SQ: f64 = 9.999999747378752e-6;
const SLOW_MOVEMENT: f64 = 0.003;
const MIN_PUSH: f64 = 0.0045000000000000005;

/// The flow directions a box sits in, summed over its fluid blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Current {
    pub sum: [f64; 3],
    pub count: u32,
}

impl Current {
    /// Pushes a player's `velocity` along the mean of this current by
    /// `scale`, at least a minimum when the player barely moves.
    pub fn push_player(&self, velocity: &mut [f64; 3], scale: f64) {
        let length_sq: f64 = self.sum.iter().map(|c| c * c).sum();
        if self.count == 0 || length_sq < MIN_CURRENT_SQ {
            return;
        }
        let inverse = 1.0 / f64::from(self.count);
        let mut impulse = self.sum.map(|c| c * inverse).map(|c| c * scale);
        let length = impulse.iter().map(|c| c * c).sum::<f64>().sqrt();
        if velocity[0].abs() < SLOW_MOVEMENT
            && velocity[2].abs() < SLOW_MOVEMENT
            && length < MIN_PUSH
        {
            impulse = if length < 1.0e-5 {
                [0.0; 3]
            } else {
                impulse.map(|c| c / length * MIN_PUSH)
            };
        }
        for axis in 0..3 {
            velocity[axis] += impulse[axis];
        }
    }

    /// Pushes a non-player `velocity` along this current by `scale`.
    pub fn push(&self, velocity: &mut [f64; 3], scale: f64) {
        let length_sq: f64 = self.sum.iter().map(|c| c * c).sum();
        if self.count == 0 || length_sq < MIN_CURRENT_SQ {
            return;
        }
        let length = length_sq.sqrt();
        let mut impulse = self.sum.map(|c| c / length * scale);
        let impulse_length = impulse.iter().map(|c| c * c).sum::<f64>().sqrt();
        if velocity[0].abs() < SLOW_MOVEMENT
            && velocity[2].abs() < SLOW_MOVEMENT
            && impulse_length < MIN_PUSH
        {
            impulse = if impulse_length < 1.0e-5 {
                [0.0; 3]
            } else {
                impulse.map(|c| c / impulse_length * MIN_PUSH)
            };
        }
        for axis in 0..3 {
            velocity[axis] += impulse[axis];
        }
    }
}

/// How deep a box is in water and lava (surface above its bottom, 0 when
/// out) and the currents it feels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Submersion {
    pub water: f64,
    pub lava: f64,
    pub water_current: Current,
    pub lava_current: Current,
}

/// What a box (`[min x, min y, min z, max x, max y, max z]`) feels of the
/// fluids it overlaps; `flow` gives each fluid block's flow direction.
pub fn submersion(
    bounds: [f64; 6],
    block: impl Fn(BlockPos) -> Option<BlockState>,
    flow: impl Fn(BlockPos) -> [f64; 3],
) -> Submersion {
    const INSET: f64 = 0.001;
    let min = [bounds[0] + INSET, bounds[1] + INSET, bounds[2] + INSET];
    let max = [bounds[3] - INSET, bounds[4] - INSET, bounds[5] - INSET];
    let range = |axis: usize| min[axis].floor() as i32..=max[axis].ceil() as i32 - 1;
    let mut out = Submersion::default();
    let mut per_fluid: Vec<(Fluid, f64)> = Vec::new();
    let mut current_height = [0.0f64; 2];
    for x in range(0) {
        for y in range(1) {
            for z in range(2) {
                let pos = BlockPos::new(x, y, z);
                let Some(fluid) = block(pos).map(BlockState::fluid_state) else {
                    continue;
                };
                if fluid.is_empty() {
                    continue;
                }
                let same_above = block(up(pos))
                    .is_some_and(|state| state.fluid_state().fluid().is_same(fluid.fluid()));
                let top = y as f64 + f64::from(fluid.height(same_above));
                if top < min[1] {
                    continue;
                }
                let kind = fluid.fluid();
                let tracked = match per_fluid.iter_mut().find(|(f, _)| *f == kind) {
                    Some(entry) => entry,
                    None => {
                        per_fluid.push((kind, 0.0));
                        per_fluid.last_mut().expect("just pushed")
                    }
                };
                tracked.1 = tracked.1.max(top - bounds[1]);
                let height = tracked.1;
                let (slot, total, current) =
                    if tuff_datapack::tags::is(tuff_vanilla_data::tag::fluid::key::WATER, kind) {
                        (0, &mut out.water, &mut out.water_current)
                    } else if tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::fluid::key::LAVA,
                        kind,
                    ) {
                        (1, &mut out.lava, &mut out.lava_current)
                    } else {
                        continue;
                    };
                *total = total.max(height);
                current_height[slot] = current_height[slot].max(height);
                let mut direction = flow(pos);
                if current_height[slot] < 0.4 {
                    direction = direction.map(|c| c * current_height[slot]);
                }
                for axis in 0..3 {
                    current.sum[axis] += direction[axis];
                }
                current.count += 1;
            }
        }
    }
    out
}

/// What a non-player entity feels of fluids in a tick: its submersion,
/// whether its eyes are in water, and how hard lava currents push here.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Immersion {
    pub submersion: Submersion,
    pub eyes_in_water: bool,
    pub lava_scale: f64,
}

impl Immersion {
    /// Pushes `velocity` along the currents of the fluids it is in.
    pub fn push(&self, velocity: &mut [f64; 3]) {
        if self.submersion.water > 0.0 {
            self.submersion
                .water_current
                .push(velocity, WATER_CURRENT_SCALE);
        }
        if self.submersion.lava > 0.0 {
            self.submersion.lava_current.push(velocity, self.lava_scale);
        }
    }
}

/// What an entity with `bounds` and eyes at `eye` feels of the fluids in
/// its region this tick.
pub fn immersion(
    context: &crate::world::entity::TickContext,
    bounds: [f64; 6],
    eye: [f64; 3],
) -> Immersion {
    let block = context.block;
    let fluids = &context.shared.fluid_behaviors;
    let fast =
        tuff_vanilla_data::fluid::fast_lava(context.shared.dimension(context.dimension).kind.name);
    Immersion {
        submersion: submersion(bounds, block, |pos| fluids.flow(block, pos)),
        eyes_in_water: eyes_in(eye, tuff_vanilla_data::tag::fluid::key::WATER, block),
        lava_scale: LAVA_CURRENT_SCALE[usize::from(!fast)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn water_below(y_top: i32) -> impl Fn(BlockPos) -> Option<BlockState> {
        move |pos: BlockPos| {
            Some(if pos.y <= y_top {
                Block::Water.default_state()
            } else {
                Block::Air.default_state()
            })
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn submersion_measures_depth_and_sums_currents() {
        let bounds = [0.4, 60.2, 0.4, 0.6, 60.45, 0.6];
        let depth = submersion(bounds, water_below(61), |_| [1.0, 0.0, 0.0]);
        assert!((depth.water - 0.8).abs() < 1e-6);
        assert_eq!(depth.lava, 0.0);
        assert_eq!(depth.water_current.count, 1);
        assert_eq!(depth.water_current.sum, [1.0, 0.0, 0.0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn shallow_submersion_weakens_the_current() {
        let bounds = [0.4, 60.7, 0.4, 0.6, 60.95, 0.6];
        let depth = submersion(bounds, water_below(60), |_| [1.0, 0.0, 0.0]);
        let height = f64::from(Block::Water.default_state().fluid_state().own_height()) - 0.7;
        assert!((depth.water - height).abs() < 1e-6);
        assert!((depth.water_current.sum[0] - height).abs() < 1e-6);
    }

    #[tuff_core::watchdog]
    #[test]
    fn out_of_fluid_there_is_nothing() {
        let bounds = [0.4, 70.0, 0.4, 0.6, 70.25, 0.6];
        assert_eq!(
            submersion(bounds, water_below(60), |_| [1.0, 0.0, 0.0]),
            Submersion::default()
        );
    }
}
