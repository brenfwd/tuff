//! A goat's ram: it picks a target, walks to a spot in a straight line
//! from it, pauses with its head lowered, then charges at it.

use rand::Rng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect, SoundEvent};
use tuff_vanilla_data::tag;
use tuff_world_model::BlockPos;

use super::MobCore;
use super::goal::{Flags, Goal};
use super::physics::{block_pos, intersects};
use super::target::{TargetView, view};
use tuff_core::protocol::play;
use tuff_core::world::entity::{Culprit, Effect, EntityHit, TickContext};

const PREPARE_TICKS: u64 = 160;
const RAM_TICKS: u64 = 200;
const HEAD_DOWN_TICKS: u64 = 20;
const MIN_DISTANCE: i32 = 4;
const MAX_DISTANCE: i32 = 7;
const PREPARE_SPEED: f64 = 1.25;
const RAM_SPEED: f64 = 3.0;
const SIGHT: f64 = 16.0;
const EVENT_LOWER_HEAD: i8 = 58;
const EVENT_RAISE_HEAD: i8 = 59;
const REGULAR_HORNS: [&str; 4] = [
    "minecraft:ponder_goat_horn",
    "minecraft:sing_goat_horn",
    "minecraft:seek_goat_horn",
    "minecraft:feel_goat_horn",
];
const SCREAMING_HORNS: [&str; 4] = [
    "minecraft:admire_goat_horn",
    "minecraft:call_goat_horn",
    "minecraft:yearn_goat_horn",
    "minecraft:dream_goat_horn",
];

pub fn horn_instruments(screaming: bool) -> &'static [&'static str; 4] {
    if screaming {
        &SCREAMING_HORNS
    } else {
        &REGULAR_HORNS
    }
}

#[derive(Debug)]
struct Candidate {
    start: BlockPos,
    target_pos: BlockPos,
    target: i32,
}

#[derive(Debug)]
enum Phase {
    Idle,
    Prepare {
        candidate: Candidate,
        reached_at: Option<u64>,
        began: u64,
    },
    Charge {
        direction: [f64; 2],
        began: u64,
        started: bool,
    },
}

#[derive(Debug)]
pub struct RamGoal {
    cooldown: i32,
    screaming: bool,
    phase: Phase,
    horn_broken: bool,
}

impl RamGoal {
    pub fn new(initial_cooldown: i32) -> Self {
        Self {
            cooldown: initial_cooldown,
            screaming: false,
            phase: Phase::Idle,
            horn_broken: false,
        }
    }

    pub fn set_cooldown(&mut self, ticks: i32) {
        self.cooldown = ticks;
    }

    pub fn sync(&mut self, screaming: bool) {
        self.screaming = screaming;
        if self.cooldown > 0 {
            self.cooldown -= 1;
        }
    }

    pub fn take_horn_break(&mut self) -> bool {
        std::mem::take(&mut self.horn_broken)
    }

    fn between_rams(&self) -> (i32, i32) {
        if self.screaming {
            (100, 300)
        } else {
            (600, 6000)
        }
    }

    fn finish(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.entity_event(EVENT_RAISE_HEAD, cx);
        let (low, high) = self.between_rams();
        self.cooldown = cx.rng.gen_range(low..=high);
        self.phase = Phase::Idle;
    }

    fn sound(&self, regular: SoundEvent, screaming: SoundEvent) -> SoundEvent {
        if self.screaming { screaming } else { regular }
    }
}

fn ramable(mob: &MobCore, target: &TargetView, cx: &TickContext) -> bool {
    target.kind != EntityType::Goat
        && (cx.shared.rule_bool("mob_griefing") || target.kind != EntityType::ArmorStand)
        && mob.can_attack(target, cx)
}

fn walkable(mob: &MobCore, pos: BlockPos, cx: &TickContext) -> bool {
    mob.is_stable_destination(pos, cx)
        && (cx.block)(pos).is_some_and(|state| !state.is_solid_render())
        && (cx.block)(BlockPos::new(pos.x, pos.y + 1, pos.z))
            .is_some_and(|state| !state.is_solid_render())
}

fn manhattan(a: BlockPos, b: BlockPos) -> i32 {
    (a.x - b.x).abs() + (a.y - b.y).abs() + (a.z - b.z).abs()
}

fn nearest_target(mob: &MobCore, cx: &TickContext) -> Option<TargetView> {
    let center = mob.base.position;
    let near = |v: &TargetView| {
        let d = [
            v.position[0] - center[0],
            v.position[1] - center[1],
            v.position[2] - center[2],
        ];
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= SIGHT * SIGHT
    };
    let players = cx.players.iter().map(TargetView::of_player);
    let others = cx
        .entities
        .iter()
        .filter_map(|e| e.living.map(|_| e.id))
        .filter_map(|id| view(cx, id));
    players
        .chain(others)
        .filter(|v| near(v) && ramable(mob, v, cx) && mob.has_line_of_sight(v, cx))
        .min_by(|a, b| {
            let d = |v: &TargetView| {
                let p = v.position;
                (p[0] - center[0]).powi(2) + (p[1] - center[1]).powi(2) + (p[2] - center[2]).powi(2)
            };
            d(a).total_cmp(&d(b))
        })
}

fn choose(mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) -> Option<Candidate> {
    let target_pos = block_pos(target.position);
    if !walkable(mob, target_pos, cx) {
        return None;
    }
    let mut options = Vec::new();
    for (dx, dz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
        let mut at = target_pos;
        for _ in 0..MAX_DISTANCE {
            let next = BlockPos::new(at.x + dx, at.y, at.z + dz);
            if !walkable(mob, next, cx) {
                break;
            }
            at = next;
        }
        if manhattan(at, target_pos) >= MIN_DISTANCE {
            options.push(at);
        }
    }
    let own = block_pos(mob.base.position);
    let dist = |p: &BlockPos| {
        let (x, y, z) = (p.x - own.x, p.y - own.y, p.z - own.z);
        i64::from(x) * i64::from(x) + i64::from(y) * i64::from(y) + i64::from(z) * i64::from(z)
    };
    options.sort_by_key(dist);
    let start = options
        .into_iter()
        .find(|pos| mob.path_to_any(&[*pos], 0, cx).is_some())?;
    Some(Candidate {
        start,
        target_pos,
        target: target.id,
    })
}

fn sign(v: i32) -> f64 {
    f64::from(v.signum())
}

impl Goal for RamGoal {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn interruptable(&self) -> bool {
        false
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.cooldown > 0
            || mob
                .love
                .as_ref()
                .is_some_and(super::animal::Love::is_in_love)
        {
            return false;
        }
        nearest_target(mob, cx).is_some()
    }

    fn can_continue(&mut self, _mob: &mut MobCore, cx: &mut TickContext) -> bool {
        match &self.phase {
            Phase::Idle => false,
            Phase::Prepare { began, .. } => cx.tick.saturating_sub(*began) < PREPARE_TICKS,
            Phase::Charge { began, .. } => cx.tick.saturating_sub(*began) < RAM_TICKS,
        }
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.phase = Phase::Idle;
        let Some(target) = nearest_target(mob, cx) else {
            return;
        };
        if let Some(candidate) = choose(mob, &target, cx) {
            self.phase = Phase::Prepare {
                candidate,
                reached_at: None,
                began: cx.tick,
            };
        }
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if matches!(self.phase, Phase::Prepare { .. } | Phase::Idle) {
            mob.entity_event(EVENT_RAISE_HEAD, cx);
            self.cooldown = self.between_rams().0;
        }
        self.phase = Phase::Idle;
        mob.navigation.stop();
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => {}
            Phase::Prepare {
                candidate,
                reached_at,
                began,
            } => {
                self.prepare(mob, candidate, reached_at, began, cx);
            }
            Phase::Charge {
                direction,
                began,
                started,
            } => {
                self.charge(mob, direction, began, started, cx);
            }
        }
    }
}

impl RamGoal {
    fn prepare(
        &mut self,
        mob: &mut MobCore,
        candidate: Candidate,
        mut reached_at: Option<u64>,
        began: u64,
        cx: &mut TickContext,
    ) {
        let Some(target) = view(cx, candidate.target).filter(|t| t.alive) else {
            return;
        };
        mob.move_to_block(candidate.start, 0, PREPARE_SPEED, cx);
        mob.look_at_point(target.position);
        if block_pos(target.position) != candidate.target_pos {
            mob.entity_event(EVENT_RAISE_HEAD, cx);
            mob.navigation.stop();
            if let Some(next) = choose(mob, &target, cx) {
                self.phase = Phase::Prepare {
                    candidate: next,
                    reached_at: None,
                    began,
                };
            }
            return;
        }
        if block_pos(mob.base.position) == candidate.start {
            mob.entity_event(EVENT_LOWER_HEAD, cx);
            let since = *reached_at.get_or_insert(cx.tick);
            if cx.tick - since >= HEAD_DOWN_TICKS {
                let edge = [
                    f64::from(candidate.target_pos.x)
                        + 0.5
                        + 0.5 * sign(candidate.target_pos.x - candidate.start.x),
                    f64::from(candidate.target_pos.y),
                    f64::from(candidate.target_pos.z)
                        + 0.5
                        + 0.5 * sign(candidate.target_pos.z - candidate.start.z),
                ];
                let from = block_pos(mob.base.position);
                let away = [f64::from(from.x) - edge[0], f64::from(from.z) - edge[2]];
                let length = away[0].hypot(away[1]);
                let direction = if length > 0.0 {
                    [away[0] / length, away[1] / length]
                } else {
                    [0.0; 2]
                };
                let sound = self.sound(
                    SoundEvent::EntityGoatPrepareRam,
                    SoundEvent::EntityGoatScreamingPrepareRam,
                );
                let pitch = mob.voice_pitch(cx.rng);
                mob.sound_as(sound, play::sound_category::NEUTRAL, 1.0, pitch, cx);
                mob.navigation.stop();
                mob.move_to_position(edge, RAM_SPEED, cx);
                self.phase = Phase::Charge {
                    direction,
                    began: cx.tick,
                    started: true,
                };
                return;
            }
        }
        self.phase = Phase::Prepare {
            candidate,
            reached_at,
            began,
        };
    }

    fn charge(
        &mut self,
        mob: &mut MobCore,
        direction: [f64; 2],
        began: u64,
        started: bool,
        cx: &mut TickContext,
    ) {
        let bounds = mob.bounds();
        let mut hit_target = None;
        for player in cx.players {
            let view = TargetView::of_player(player);
            if intersects(bounds, view.bounds) && ramable(mob, &view, cx) {
                hit_target = Some(view);
                break;
            }
        }
        if hit_target.is_none() {
            hit_target = cx
                .entities_near(&bounds)
                .into_iter()
                .filter(|e| e.living.is_some())
                .filter_map(|e| view(cx, e.id))
                .find(|v| intersects(bounds, v.bounds) && ramable(mob, v, cx));
        }
        let impact = self.sound(
            SoundEvent::EntityGoatRamImpact,
            SoundEvent::EntityGoatScreamingRamImpact,
        );
        let Some(victim) = hit_target else {
            if hits_horn_breaker(mob, cx) {
                mob.sound_as(impact, play::sound_category::NEUTRAL, 1.0, 1.0, cx);
                self.horn_broken = true;
                self.finish(mob, cx);
                return;
            }
            if mob.navigation.is_done() || !started {
                self.finish(mob, cx);
                return;
            }
            self.phase = Phase::Charge {
                direction,
                began,
                started,
            };
            return;
        };
        let damage = mob.attribute(Attribute::AttackDamage) as f32;
        let level = |effect| {
            mob.effects
                .amplifier(effect)
                .map_or(0, |a| i32::from(a) + 1)
        };
        let boost = 0.25 * (level(MobEffect::Speed) - level(MobEffect::Slowness)) as f32;
        let speed = (mob.attribute(Attribute::MovementSpeed) as f32 * 1.65).clamp(0.2, 3.0) + boost;
        let force = if mob.is_baby() { 1.0 } else { 2.5 };
        let mut hit = EntityHit::new(damage, &damage_type::MOB_ATTACK_NO_AGGRO);
        hit.attacker = Some(Culprit::entity(&mob.base));
        hit.position = Some(mob.base.position);
        hit.knockback = Some((f64::from(speed * force), direction[0], direction[1]));
        cx.effects.push(Effect::Hurt {
            target: victim.id,
            hit: Box::new(hit),
        });
        self.finish(mob, cx);
        mob.sound_as(impact, play::sound_category::NEUTRAL, 1.0, 1.0, cx);
    }
}

fn hits_horn_breaker(mob: &MobCore, cx: &TickContext) -> bool {
    let v = mob.base.velocity;
    let length = v[0].hypot(v[2]);
    let (dx, dz) = if length > 0.0 {
        (v[0] / length, v[2] / length)
    } else {
        (0.0, 0.0)
    };
    let p = mob.base.position;
    let front = block_pos([p[0] + dx, p[1], p[2] + dz]);
    let snaps = |pos: BlockPos| {
        (cx.block)(pos)
            .is_some_and(|s| tuff_datapack::tags::is(tag::block::key::SNAPS_GOAT_HORN, s.block()))
    };
    snaps(front) || snaps(BlockPos::new(front.x, front.y + 1, front.z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::tests::zombie;
    use crate::tests::with_entities;

    #[tuff_core::watchdog]
    #[test]
    fn a_goat_lines_up_waits_with_its_head_down_then_charges() {
        let target = zombie(900, [3.5, 64.0, 0.5]);
        with_entities(&[target], |mob, cx| {
            mob.base.position = [0.5, 64.0, 0.5];
            let mut goal = RamGoal::new(0);
            assert!(goal.can_use(mob, cx));
            goal.start(mob, cx);
            let Phase::Prepare { candidate, .. } = &goal.phase else {
                panic!("no ram prepared");
            };
            assert!(manhattan(candidate.start, candidate.target_pos) >= MIN_DISTANCE);
            let start = candidate.start;
            mob.base.position = [f64::from(start.x) + 0.5, 64.0, f64::from(start.z) + 0.5];
            cx.tick = 100;
            goal.tick(mob, cx);
            assert!(matches!(
                goal.phase,
                Phase::Prepare {
                    reached_at: Some(100),
                    ..
                }
            ));
            cx.tick = 100 + HEAD_DOWN_TICKS;
            goal.tick(mob, cx);
            assert!(matches!(goal.phase, Phase::Charge { .. }));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_goat_only_rams_after_its_cooldown_and_never_another_goat() {
        let mut goat = zombie(901, [3.5, 64.0, 0.5]);
        goat.kind = EntityType::Goat;
        with_entities(&[goat, zombie(902, [30.5, 64.0, 0.5])], |mob, cx| {
            mob.base.position = [0.5, 64.0, 0.5];
            assert!(!RamGoal::new(0).can_use(mob, cx));
            assert!(!RamGoal::new(10).can_use(mob, cx));
        });
        with_entities(&[zombie(903, [3.5, 64.0, 0.5])], |mob, cx| {
            mob.base.position = [0.5, 64.0, 0.5];
            assert!(!RamGoal::new(10).can_use(mob, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn running_into_a_horn_snapping_block_breaks_a_horn() {
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 {
                tuff_vanilla_data::Block::GrassBlock.default_state()
            } else if pos.x == 2 && pos.y == 64 {
                tuff_vanilla_data::Block::Stone.default_state()
            } else {
                tuff_vanilla_data::Block::Air.default_state()
            })
        };
        crate::tests::with_world(&block, &[], |mob, cx| {
            mob.base.position = [1.5, 64.0, 0.5];
            mob.base.velocity = [0.2, 0.0, 0.0];
            assert!(hits_horn_breaker(mob, cx));
            let mut goal = RamGoal::new(0);
            goal.phase = Phase::Charge {
                direction: [1.0, 0.0],
                began: 0,
                started: true,
            };
            goal.tick(mob, cx);
            assert!(goal.take_horn_break());
            assert!(matches!(goal.phase, Phase::Idle));
            assert!(goal.cooldown >= 600);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn horn_instruments_follow_the_goat_kind() {
        assert!(horn_instruments(true)[0].contains("admire"));
        assert!(horn_instruments(false)[0].contains("ponder"));
    }
}
