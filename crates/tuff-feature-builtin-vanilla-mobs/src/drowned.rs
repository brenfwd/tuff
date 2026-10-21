//! How drowned get around: amphibious paths, swimming after targets in the
//! water, heading into water by day, up to the surface and onto beaches by
//! night.

use rand::Rng;
use tuff_vanilla_data::registry::Attribute;

use super::MobCore;
use super::control::MoveStyle;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{MoveToBlock, RandomStroll, bright_outside};
use super::movement::WaterTravel;
use super::navigation::NavKind;
use super::physics::block_pos;
use super::random_pos;
use tuff_core::world::entity::TickContext;
use tuff_vanilla_data::entity::PathType;
use tuff_world_model::BlockPos;

const SWIM_STROKE: f32 = 0.01;

/// Sets up a drowned's navigation and movement goals.
pub fn configure(core: &mut MobCore, goals: &mut GoalSelector) {
    core.set_path_malus(PathType::Water, 0.0);
    core.navigation.kind = NavKind::Amphibious;
    core.move_style = MoveStyle::Drowned {
        searching_for_land: false,
    };
    core.water_travel = WaterTravel::WhileSwimming {
        stroke: SWIM_STROKE,
    };
    if core.attribute_base(Attribute::StepHeight) < 1.0 {
        core.set_attribute_base(Attribute::StepHeight, 1.0);
    }
    goals.add(1, GoToWater::default());
    goals.add(5, beach());
    goals.add(6, SwimUp::default());
    goals.add(7, RandomStroll::with_interval(1.0, RandomStroll::INTERVAL));
}

/// A drowned goes after a target only at night or while the target is in
/// water.
pub fn ok_target(_mob: &MobCore, target: &super::target::TargetView, cx: &TickContext) -> bool {
    !bright_outside(cx) || target.in_water
}

fn sea_level(cx: &TickContext) -> i32 {
    cx.shared.dimension(cx.dimension).sea_level()
}

fn set_searching_for_land(mob: &mut MobCore, searching: bool) {
    if let MoveStyle::Drowned { searching_for_land } = &mut mob.move_style {
        *searching_for_land = searching;
    }
}

/// By day, heads for any water block nearby when out of the water.
#[derive(Debug, Default)]
struct GoToWater {
    wanted: [f64; 3],
}

impl Goal for GoToWater {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !bright_outside(cx) || mob.body.in_water {
            return false;
        }
        let here = block_pos(mob.base.position);
        for _ in 0..10 {
            let pos = BlockPos::new(
                here.x + cx.rng.gen_range(0..20) - 10,
                here.y + 2 - cx.rng.gen_range(0..8),
                here.z + cx.rng.gen_range(0..20) - 10,
            );
            if (cx.block)(pos).is_some_and(|s| s.block() == tuff_vanilla_data::Block::Water) {
                self.wanted = [
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y),
                    f64::from(pos.z) + 0.5,
                ];
                return true;
            }
        }
        false
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.wanted, 1.0, cx);
    }
}

/// Somewhere to climb out onto: two clear blocks over something to stand
/// on.
fn beach_spot(_mob: &MobCore, pos: BlockPos, cx: &TickContext) -> bool {
    let air = |p: BlockPos| (cx.block)(p).is_some_and(tuff_world_model::chunk::is_air);
    air(BlockPos::new(pos.x, pos.y + 1, pos.z))
        && air(BlockPos::new(pos.x, pos.y + 2, pos.z))
        && (cx.block)(pos)
            .is_some_and(|s| s.is_collision_face_full(tuff_vanilla_data::block::Facing::Up))
}

/// By night, swims to a beach near the surface.
#[derive(Debug)]
struct GoToBeach(MoveToBlock);

fn beach() -> GoToBeach {
    GoToBeach(MoveToBlock::new(1.0, 8, 2, beach_spot))
}

impl Goal for GoToBeach {
    fn flags(&self) -> Flags {
        self.0.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.0.can_use(mob, cx)
            && !bright_outside(cx)
            && mob.body.in_water
            && mob.base.position[1] >= f64::from(sea_level(cx) - 3)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.0.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        set_searching_for_land(mob, false);
        self.0.start(mob, cx);
    }

    fn every_tick(&self) -> bool {
        self.0.every_tick()
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.tick(mob, cx);
    }
}

/// By night, swims up towards the surface, looking for land.
#[derive(Debug, Default)]
struct SwimUp {
    stuck: bool,
}

impl SwimUp {
    fn wanted(mob: &MobCore, cx: &TickContext) -> bool {
        !bright_outside(cx)
            && mob.body.in_water
            && mob.base.position[1] < f64::from(sea_level(cx) - 2)
    }

    fn close_to_next_pos(mob: &MobCore) -> bool {
        mob.navigation
            .path
            .as_ref()
            .and_then(|path| path.end())
            .is_some_and(|end| {
                let [x, y, z] = mob.base.position;
                let (dx, dy, dz) = (
                    f64::from(end.x) - x,
                    f64::from(end.y) - y,
                    f64::from(end.z) - z,
                );
                dx * dx + dy * dy + dz * dz < 4.0
            })
    }
}

impl Goal for SwimUp {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        Self::wanted(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        Self::wanted(mob, cx) && !self.stuck
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        set_searching_for_land(mob, true);
        self.stuck = false;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        set_searching_for_land(mob, false);
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let sea_level = sea_level(cx);
        if mob.base.position[1] >= f64::from(sea_level - 1)
            || !(mob.navigation.is_done() || Self::close_to_next_pos(mob))
        {
            return;
        }
        let [x, _, z] = mob.base.position;
        let surface = [x, f64::from(sea_level - 1), z];
        match random_pos::pos_towards(mob, 4, 8, surface, cx) {
            Some(next) => {
                mob.move_to_position(next, 1.0, cx);
            }
            None => self.stuck = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;

    #[tuff_core::watchdog]
    #[test]
    fn a_drowned_swims_only_under_water_and_when_it_wants_to() {
        with_context(|core, cx| {
            configure(core, &mut GoalSelector::default());
            core.body.in_water = true;
            core.body.eyes_in_water = true;
            core.tick_move_control(cx);
            assert!(!core.swimming, "nothing to swim after");
            set_searching_for_land(core, true);
            core.tick_move_control(cx);
            assert!(core.swimming, "currents leave a swimming drowned alone");
            assert!(
                core.changed
                    .contains(&tuff_core::protocol::play::metadata::POSE)
            );
            assert_eq!(
                core.core_metadata(tuff_core::protocol::play::metadata::POSE),
                Some(tuff_core::protocol::play::MetadataValue::Pose(i32::from(
                    tuff_vanilla_data::entity::Pose::Swimming.id()
                )))
            );
            core.body.eyes_in_water = false;
            core.tick_move_control(cx);
            assert!(!core.swimming, "wading is not swimming");
        });
    }
}
