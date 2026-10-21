//! Zombies that break wooden doors down on hard difficulty.

use rand::Rng;
use tuff_vanilla_data::tag;

use super::MobCore;
use super::goal::{Flags, Goal};
use super::monster::level_event;
use super::physics::block_pos;
use tuff_core::protocol::play;
use tuff_core::world::entity::{Effect, TickContext};
use tuff_core::world::player::Difficulty;
use tuff_core::world::{framed, packets};
use tuff_world_model::BlockPos;

const BREAK_TICKS: i32 = 240;
const ZOMBIE_WOODEN_DOOR: i32 = 1019;
const ZOMBIE_DOOR_CRASH: i32 = 1021;

#[derive(Debug, Default)]
pub struct BreakDoor {
    way: DoorWay,
    break_time: i32,
    last_progress: i32,
    difficulties: Option<fn(Difficulty) -> bool>,
}

fn is_wooden_door(pos: BlockPos, cx: &TickContext) -> bool {
    (cx.block)(pos)
        .is_some_and(|state| tuff_datapack::tags::is(tag::block::key::WOODEN_DOORS, state.block()))
}

fn is_open(pos: BlockPos, cx: &TickContext) -> bool {
    (cx.block)(pos).is_some_and(|state| state.open() == Some(true))
}

fn hard(cx: &TickContext) -> bool {
    *cx.shared.difficulty.borrow() == Difficulty::Hard
}

fn offset_to(mob: &MobCore, door: BlockPos) -> [f32; 2] {
    [
        (f64::from(door.x) + 0.5 - mob.base.position[0]) as f32,
        (f64::from(door.z) + 0.5 - mob.base.position[2]) as f32,
    ]
}

/// A wooden door the mob is walking through, and which side it came from.
#[derive(Debug, Default)]
struct DoorWay {
    pos: Option<BlockPos>,
    entry_side: [f32; 2],
    crossed: bool,
}

impl DoorWay {
    /// Looks for a wooden door the blocked mob is about to walk into: one
    /// at head height on its next couple of path nodes, else right above
    /// its feet.
    fn locate(&mut self, mob: &MobCore, cx: &TickContext) -> bool {
        let Some(path) = mob.navigation.path.as_ref().filter(|p| !p.is_done()) else {
            return false;
        };
        if mob.body.horizontal_collision {
            let [mx, _, mz] = mob.base.position;
            let reachable = |pos: &BlockPos| {
                let (ox, oz) = (f64::from(pos.x) - mx, f64::from(pos.z) - mz);
                ox * ox + oz * oz <= 2.25 && is_wooden_door(*pos, cx)
            };
            let upcoming = &path.nodes[..path.nodes.len().min(path.next + 2)];
            self.pos = upcoming
                .iter()
                .map(|n| BlockPos::new(n.x, n.y + 1, n.z))
                .find(reachable)
                .or_else(|| {
                    let feet = block_pos(mob.base.position);
                    let head = BlockPos::new(feet.x, feet.y + 1, feet.z);
                    is_wooden_door(head, cx).then_some(head)
                });
            return self.pos.is_some();
        }
        false
    }

    fn enter(&mut self, mob: &MobCore) {
        self.crossed = false;
        if let Some(door) = self.pos {
            self.entry_side = offset_to(mob, door);
        }
    }

    fn track(&mut self, mob: &MobCore) {
        if let Some(door) = self.pos {
            let [ex, ez] = self.entry_side;
            let [nx, nz] = offset_to(mob, door);
            if ex * nx + ez * nz < 0.0 {
                self.crossed = true;
            }
        }
    }
}

impl BreakDoor {
    pub fn new() -> Self {
        Self {
            last_progress: -1,
            ..Self::default()
        }
    }

    /// Breaking on the difficulties `allowed` accepts rather than on hard
    /// only.
    pub fn on(allowed: fn(Difficulty) -> bool) -> Self {
        Self {
            difficulties: Some(allowed),
            ..Self::new()
        }
    }

    fn valid_difficulty(&self, cx: &TickContext) -> bool {
        match self.difficulties {
            Some(allowed) => allowed(*cx.shared.difficulty.borrow()),
            None => hard(cx),
        }
    }

    fn progress_packet(mob: &MobCore, pos: BlockPos, stage: i8, cx: &mut TickContext) {
        cx.effects.push(Effect::ChunkPackets(packets([framed(
            &play::BlockDestruction {
                id: mob.base.id,
                position: pos,
                stage,
            },
        )])));
    }
}

impl Goal for BreakDoor {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.way.locate(mob, cx)
            && cx.shared.rule_bool("mob_griefing")
            && self.valid_difficulty(cx)
            && self.way.pos.is_some_and(|door| !is_open(door, cx))
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(door) = self.way.pos else {
            return false;
        };
        let center = [
            f64::from(door.x) + 0.5,
            f64::from(door.y) + 0.5,
            f64::from(door.z) + 0.5,
        ];
        self.break_time <= BREAK_TICKS
            && !self.way.crossed
            && is_wooden_door(door, cx)
            && !is_open(door, cx)
            && mob.distance_sq(center) < 4.0
            && self.valid_difficulty(cx)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.way.enter(mob);
        self.break_time = 0;
        self.last_progress = -1;
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(door) = self.way.pos {
            Self::progress_packet(mob, door, -1, cx);
        }
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(door) = self.way.pos else {
            return;
        };
        self.way.track(mob);
        if cx.rng.gen_range(0..20) == 0 {
            level_event(door, ZOMBIE_WOODEN_DOOR, 0, cx);
            mob.swing(cx);
        }
        self.break_time += 1;
        let progress = (self.break_time as f32 / BREAK_TICKS as f32 * 10.0) as i32;
        if progress != self.last_progress {
            Self::progress_packet(mob, door, progress as i8, cx);
            self.last_progress = progress;
        }
        if self.break_time == BREAK_TICKS && self.valid_difficulty(cx) {
            level_event(door, ZOMBIE_DOOR_CRASH, 0, cx);
            cx.effects.push(Effect::DestroyBlock {
                pos: door,
                drop: false,
            });
        }
    }
}

/// Opens wooden doors on the path as the mob reaches them and, if asked,
/// closes them again once it is through.
#[derive(Debug, Default)]
pub struct OpenDoor {
    close_after: bool,
    way: DoorWay,
    forget: i32,
}

impl OpenDoor {
    pub fn new(close_after: bool) -> Self {
        Self {
            close_after,
            ..Self::default()
        }
    }

    fn set_open(&self, open: bool, cx: &mut TickContext) {
        if let Some(pos) = self.way.pos
            && is_wooden_door(pos, cx)
        {
            cx.effects.push(Effect::UseDoor { pos, open });
        }
    }
}

impl Goal for OpenDoor {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.navigation.can_open_doors && self.way.locate(mob, cx)
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        self.close_after && self.forget > 0 && !self.way.crossed
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.way.enter(mob);
        self.forget = 20;
        self.set_open(true, cx);
    }

    fn stop(&mut self, _mob: &mut MobCore, cx: &mut TickContext) {
        self.set_open(false, cx);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.forget -= 1;
        self.way.track(mob);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::{Path, PathNode};
    use tuff_vanilla_data::Block;
    use tuff_vanilla_data::entity::PathType;

    fn path(nodes: &[[i32; 3]], next: usize) -> Path {
        Path {
            nodes: nodes
                .iter()
                .map(|&[x, y, z]| PathNode {
                    x,
                    y,
                    z,
                    kind: PathType::Walkable,
                })
                .collect(),
            next,
            target: BlockPos::new(9, 64, 0),
            reached: true,
            distance_to_target: 0.0,
            debug: None,
        }
    }

    fn found(door: BlockPos, at: [f64; 3], next: usize, collided: bool) -> Option<BlockPos> {
        let block = move |pos: BlockPos| {
            Some(if pos == door {
                Block::OakDoor.default_state()
            } else if pos.y < 64 {
                Block::GrassBlock.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let mut result = None;
        crate::tests::with_world(&block, &[], |mob, cx| {
            mob.base.position = at;
            mob.body.horizontal_collision = collided;
            mob.navigation.can_open_doors = true;
            mob.navigation.path = Some(path(
                &[[0, 64, 0], [1, 64, 0], [2, 64, 0], [3, 64, 0]],
                next,
            ));
            let mut open = OpenDoor::new(true);
            let used = open.can_use(mob, cx);
            assert_eq!(used, open.way.pos.is_some());
            result = open.way.pos;
        });
        result
    }

    #[tuff_core::watchdog]
    #[test]
    fn doors_are_found_on_the_next_path_nodes_or_above_the_feet() {
        let door = BlockPos::new(2, 65, 0);
        assert_eq!(found(door, [1.0, 64.0, 0.5], 0, true), None);
        assert_eq!(found(door, [1.0, 64.0, 0.5], 1, true), Some(door));
        assert_eq!(found(door, [1.0, 64.0, 0.5], 1, false), None);
        assert_eq!(found(door, [0.4, 64.0, 0.5], 1, true), None);
        assert_eq!(found(door, [0.58, 64.0, 0.5], 1, true), None);
        assert_eq!(found(door, [0.6, 64.0, 0.5], 1, true), Some(door));
        assert_eq!(found(door, [2.5, 64.0, 0.5], 4, true), None);
        assert_eq!(found(door, [2.5, 64.0, 0.5], 3, true), Some(door));
        let above = BlockPos::new(5, 65, 0);
        assert_eq!(found(above, [5.7, 64.0, 0.2], 1, true), Some(above));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_door_counts_as_passed_once_the_mob_crosses_it() {
        crate::tests::with_context(|mob, cx| {
            mob.base.position = [1.0, 64.0, 0.5];
            let mut open = OpenDoor::new(true);
            open.way.pos = Some(BlockPos::new(2, 65, 0));
            open.start(mob, cx);
            let mut trace = Vec::new();
            for x in [1.5, 2.4, 2.5, 2.6, 1.0] {
                mob.base.position[0] = x;
                open.tick(mob, cx);
                trace.push((open.way.crossed, open.can_continue(mob, cx)));
            }
            assert_eq!(
                trace,
                [
                    (false, true),
                    (false, true),
                    (false, true),
                    (true, false),
                    (true, false)
                ]
            );
            let mut breaker = BreakDoor::new();
            breaker.way.pos = Some(BlockPos::new(2, 65, 0));
            mob.base.position[0] = 1.0;
            breaker.start(mob, cx);
            mob.base.position[0] = 2.7;
            breaker.tick(mob, cx);
            assert!(breaker.way.crossed);
        });
    }
}
