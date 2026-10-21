//! Baby villagers at play: bouncing on beds and chasing each other.

use rand::Rng;
use tuff_vanilla_data::registry::{EntityType, PointOfInterestType};
use tuff_vanilla_data::tag;

use super::{SPEED, Villager, distance_sq};
use crate::MobCore;
use crate::physics::block_pos;
use crate::raider::is_village;
use crate::world::poi::Occupancy;
use tuff_core::world::entity::TickContext;
use tuff_world_model::BlockPos;

const BED_SEARCH: i32 = 48;
const REACH_BED_TICKS: i32 = 100;
const JUMP_COOLDOWN: i32 = 5;
const MIN_JUMPS: i32 = 3;
const EXTRA_JUMPS: i32 = 4;
const TAG_SPEED: f64 = 0.6;
const TAG_CHANCE: u32 = 10;
const TAG_RANGE_SQ: f64 = 256.0;
const MAX_CHASERS: i32 = 5;

#[derive(Debug, Default)]
pub struct State {
    plan: Option<Bounce>,
    next_choice: u64,
}

#[derive(Debug, Clone, Copy)]
struct Bounce {
    bed: BlockPos,
    time_left: i32,
    jumps_left: i32,
    cooldown: i32,
}

fn jumpable(cx: &TickContext, pos: BlockPos) -> bool {
    (cx.block)(pos).is_some_and(|s| {
        tuff_datapack::tags::is(tag::block::key::VILLAGER_BABIES_CAN_JUMP_ON_BED, s.block())
    })
}

fn on_or_over_bed(cx: &TickContext, core: &MobCore) -> bool {
    let here = block_pos(core.base.position);
    jumpable(cx, here) || jumpable(cx, BlockPos::new(here.x, here.y - 1, here.z))
}

impl Villager {
    /// What a baby does with its day: bounce on a bed, run after another
    /// child, say hello to a villager, wander, or simply stand.
    pub(super) fn play(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.bounce_step(core, cx) {
            return;
        }
        self.interact_with_villagers(core, cx);
        if self.brain.social.interaction.is_some() || !core.navigation.is_done() {
            return;
        }
        if cx.tick < self.brain.play.next_choice {
            return;
        }
        if self.play_tag(core, cx) {
            return;
        }
        match cx.rng.gen_range(0..4) {
            0 => {
                self.brain.play.next_choice = cx.tick + cx.rng.gen_range(20..=40);
            }
            1 => self.stroll(core, cx, SPEED),
            _ => {
                self.start_bounce(core, cx);
            }
        }
    }

    fn play_tag(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        if core
            .chasing
            .is_some_and(|id| core.navigation.is_done() || !cx.entities.iter().any(|e| e.id == id))
        {
            core.chasing = None;
        }
        if cx.rng.gen_range(0..TAG_CHANCE) != 0 {
            return false;
        }
        let kids: Vec<_> = cx
            .entities
            .iter()
            .filter(|e| e.kind == EntityType::Villager && e.id != core.base.id)
            .filter(|e| e.living.as_ref().is_some_and(|l| l.alive && l.age < 0))
            .filter(|e| distance_sq(e.position, core.base.position) <= TAG_RANGE_SQ)
            .collect();
        if kids.is_empty() {
            return false;
        }
        if kids
            .iter()
            .any(|e| e.living.as_ref().and_then(|l| l.chasing) == Some(core.base.id))
        {
            for _ in 0..10 {
                if let Some(target) = crate::random_pos::land_pos(core, 20, 8, cx)
                    .filter(|target| is_village(cx.pois, block_pos(*target)))
                {
                    core.move_to_position(target, TAG_SPEED, cx);
                    break;
                }
            }
            return true;
        }
        let mut chased: Vec<(i32, i32)> = Vec::new();
        for chasing in kids
            .iter()
            .filter_map(|e| e.living.as_ref().and_then(|l| l.chasing))
        {
            match chased.iter_mut().find(|(id, _)| *id == chasing) {
                Some((_, count)) => *count += 1,
                None => chased.push((chasing, 1)),
            }
        }
        chased.sort_by_key(|&(_, count)| count);
        let pick = chased
            .iter()
            .find(|&&(id, count)| count <= MAX_CHASERS && kids.iter().any(|e| e.id == id))
            .and_then(|&(id, _)| kids.iter().find(|e| e.id == id))
            .copied()
            .unwrap_or_else(|| kids[cx.rng.gen_range(0..kids.len())]);
        core.chasing = Some(pick.id);
        core.move_to_entity_reach(pick.position, 1, TAG_SPEED, cx);
        true
    }

    pub(super) fn start_bounce(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        let origin = block_pos(core.base.position);
        let beds: Vec<BlockPos> = cx
            .pois
            .closest_first(origin, BED_SEARCH, Occupancy::Any, |kind| {
                kind == PointOfInterestType::Home
            })
            .into_iter()
            .map(|(pos, _)| pos)
            .take(5)
            .collect();
        let Some(path) = core.path_to_any(&beds, 1, cx) else {
            self.brain.play.next_choice = cx.tick + 40;
            return false;
        };
        let bed = path.target;
        if !jumpable(cx, bed) {
            return false;
        }
        self.brain.play.plan = Some(Bounce {
            bed,
            time_left: REACH_BED_TICKS,
            jumps_left: MIN_JUMPS + cx.rng.gen_range(0..EXTRA_JUMPS),
            cooldown: 0,
        });
        core.move_to_block(bed, 0, SPEED, cx);
        true
    }

    /// True while a bounce plan holds the baby.
    pub(super) fn bounce_step(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(mut plan) = self.brain.play.plan else {
            return false;
        };
        let over = on_or_over_bed(cx, core);
        let finished = !core.is_baby()
            || !jumpable(cx, plan.bed)
            || (!over && plan.time_left <= 0)
            || (over && plan.jumps_left <= 0);
        if finished {
            self.brain.play.plan = None;
            self.brain.play.next_choice = cx.tick + 20;
            return false;
        }
        if !over {
            plan.time_left -= 1;
            if core.navigation.is_done() {
                core.move_to_block(plan.bed, 0, SPEED, cx);
            }
        } else if plan.cooldown > 0 {
            plan.cooldown -= 1;
        } else if jumpable(cx, block_pos(core.base.position)) {
            core.jump.jump = true;
            plan.jumps_left -= 1;
            plan.cooldown = JUMP_COOLDOWN;
        }
        self.brain.play.plan = Some(plan);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::world::entity::EntitySummary;

    fn kid(id: i32, chasing: Option<i32>) -> EntitySummary {
        let mut kid = crate::target::tests::zombie(id, [2.0, 64.0, 0.0]);
        kid.kind = EntityType::Villager;
        let living = kid.living.as_mut().unwrap();
        living.age = -100;
        living.chasing = chasing;
        kid
    }

    fn play_until_tagged(villager: &mut Villager, core: &mut MobCore, cx: &mut TickContext) {
        for _ in 0..400 {
            if villager.play_tag(core, cx) {
                return;
            }
        }
        panic!("never played tag");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_kid_chases_the_one_with_the_fewest_chasers() {
        let kids = [
            kid(51, Some(53)),
            kid(52, Some(54)),
            kid(55, Some(54)),
            kid(53, None),
            kid(54, None),
        ];
        crate::tests::with_entities(&kids, |core, cx| {
            let mut villager = Villager::default();
            play_until_tagged(&mut villager, core, cx);
            assert_eq!(core.chasing, Some(53));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_kid_being_chased_runs_instead_of_chasing() {
        let kids = [kid(51, None), kid(52, Some(1))];
        crate::tests::with_entities(&kids, |core, cx| {
            assert_eq!(core.base.id, 1);
            let mut villager = Villager::default();
            play_until_tagged(&mut villager, core, cx);
            assert_eq!(core.chasing, None);
        });
    }
}
