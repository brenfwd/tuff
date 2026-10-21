//! The Warden: blind, it hunts by vibration, touch and smell. Anger per
//! suspect builds up from what it hears and smells; once angry at someone
//! it roars and fights them with melee hits and, when it cannot land
//! them, a sonic boom. One summoned by a shrieker emerges from the ground,
//! and a Warden left undisturbed for a minute digs back down and is gone.
//!
//! The behaviour is a small activity state machine run from the species
//! tick (emerge, dig, roar, fight, investigate, sniff, idle, in that
//! order of precedence); only idle strolling is a goal.

use rand::Rng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::entity::{PathType, Pose};
use tuff_vanilla_data::registry::{EntityType, MobEffect, ParticleType, SoundEvent};

use super::goal::GoalSelector;
use super::goals::{Float, RandomStroll};
use super::target::{self, TargetView};
use super::{Damage, MobCore, MobSounds, Species, data, monster};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{Culprit, Effect, EntityHit, Notice, TickContext, take};
use tuff_core::world::messages::PlayerEvent;
use tuff_core::world::{framed, packets};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

/// The client-facing "anger level" data index: Wardens are not ageable,
/// so their own data starts where a baby/age-lock pair would.
const ANGER_LEVEL: u8 = data::SPECIES - 2;

const WARDEN_XP: i32 = 5;
pub const EMERGE_TICKS: i32 = 134;
pub const DIG_TICKS: i32 = 100;
pub const DIG_COOLDOWN: i64 = 1200;
const ROAR_TICKS: i32 = 84;
const ROAR_SOUND_AT: i32 = 25;
const SNIFF_TICKS: i32 = 84;
const SNIFF_COOLDOWN: std::ops::RangeInclusive<i32> = 100..=200;
const DISTURBANCE_TICKS: i32 = 100;
const VIBRATION_COOLDOWN: i32 = 40;
const TOUCH_COOLDOWN: i32 = 20;
const MELEE_COOLDOWN: i32 = 18;

const MAX_ANGER: i32 = 150;
const AGITATED: i32 = 40;
const ANGRY: i32 = 80;
const DEFAULT_ANGER: i32 = 35;
const ON_HURT_ANGER: i32 = ANGRY + 20;
const ROAR_ANGER: i32 = 20;
const ANGER_TICK_INTERVAL: u64 = 20;

const SONIC_BOOM_DAMAGE: f32 = 10.0;
const SONIC_BOOM_TICKS: i32 = 60;
const SONIC_BOOM_FIRE_AT: i32 = 34;
const SONIC_BOOM_COOLDOWN: i32 = 40;
/// Sonic boom cooldown when it picks a new attack target.
const NEW_TARGET_BOOM_COOLDOWN: i32 = 200;
const SONIC_BOOM_XZ: f64 = 15.0;
const SONIC_BOOM_Y: f64 = 20.0;
const SONIC_BOOM_PUSH_XZ: f64 = 2.5;
const SONIC_BOOM_PUSH_Y: f64 = 0.5;
/// Where the boom leaves its body, above its feet.
const CHEST_HEIGHT: f64 = 1.6;

const DARKNESS_TICKS: i64 = 260;
const DARKNESS_RADIUS: f64 = 20.0;
const DARKNESS_INTERVAL: u64 = 120;

const SNIFF_ANGER_XZ: f64 = 6.0;
const SNIFF_ANGER_Y: f64 = 20.0;
const HURT_TARGET_DISTANCE: f64 = 5.0;

const IDLE_SPEED: f64 = 0.5;
const INVESTIGATE_SPEED: f64 = 0.7;
const FIGHT_SPEED: f64 = 1.2;
const SOUND_VOLUME: f32 = 4.0;

/// Activity the Warden is in, the first that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Activity {
    Emerge,
    Dig,
    Roar,
    Fight,
    Investigate,
    Sniff,
    Idle,
}

const EVENT_ATTACK: i8 = 4;
const EVENT_TENDRILS: i8 = 61;
const EVENT_SONIC_BOOM: i8 = 62;

/// `MobCore::signal` while it idles, so its stroll goal may run.
const SIGNAL_IDLE: i32 = 1;

/// The Darkness it and shriekers spread: hidden, unannounced.
pub fn darkness() -> ActiveEffect {
    let mut effect = ActiveEffect::timed(MobEffect::Darkness, DARKNESS_TICKS, 0);
    effect.show_particles = false;
    effect.show_icon = false;
    effect
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Suspect {
    id: i32,
    uuid: u128,
    player: bool,
    anger: i32,
}

/// Anger per suspect, sorted so the one it goes after comes first: angry
/// ones, then players, then by anger.
#[derive(Debug, Default, Clone)]
struct Suspects {
    known: Vec<Suspect>,
    /// Saved suspects not seen since loading, by UUID.
    saved: Vec<(u128, i32)>,
}

impl Suspects {
    fn sort(&mut self) {
        self.known
            .sort_by_key(|s| (s.anger < ANGRY, !s.player, -s.anger));
    }

    fn highest(&self) -> i32 {
        self.known.iter().map(|s| s.anger).max().unwrap_or(0)
    }

    fn anger_at(&self, id: i32) -> i32 {
        self.known
            .iter()
            .find(|s| s.id == id)
            .map_or(0, |s| s.anger)
    }

    fn increase(&mut self, view: &TargetView, amount: i32) -> i32 {
        let anger = match self.known.iter_mut().find(|s| s.id == view.id) {
            Some(suspect) => {
                suspect.anger = (suspect.anger + amount).min(MAX_ANGER);
                suspect.anger
            }
            None => {
                let mut anger = amount.min(MAX_ANGER);
                if let Some(at) = self.saved.iter().position(|(uuid, _)| *uuid == view.uuid) {
                    anger += self.saved.remove(at).1;
                }
                self.known.push(Suspect {
                    id: view.id,
                    uuid: view.uuid,
                    player: view.is_player(),
                    anger,
                });
                anger
            }
        };
        self.sort();
        anger
    }

    fn clear(&mut self, id: i32) {
        self.known.retain(|s| s.id != id);
    }

    /// The periodic decay: everyone's anger drops by one; suspects gone
    /// from view keep theirs by UUID, ones it may not target are dropped.
    fn tick(&mut self, cx: &TickContext, mob: &MobCore) {
        self.saved.retain_mut(|(_, anger)| {
            *anger -= 1;
            *anger > 0
        });
        for (uuid, anger) in std::mem::take(&mut self.saved) {
            match find_uuid(cx, uuid) {
                Some(view) => self.known.push(Suspect {
                    id: view.id,
                    uuid,
                    player: view.is_player(),
                    anger,
                }),
                None => self.saved.push((uuid, anger)),
            }
        }
        let mut kept = Vec::with_capacity(self.known.len());
        for mut suspect in std::mem::take(&mut self.known) {
            let view = target::view(cx, suspect.id);
            match view {
                Some(view) if suspect.anger > 1 && can_target(mob, &view, cx) => {
                    suspect.anger -= 1;
                    kept.push(suspect);
                }
                None if suspect.anger > 1 => self.saved.push((suspect.uuid, suspect.anger - 1)),
                _ => {}
            }
        }
        self.known = kept;
        self.sort();
    }

    fn top(&self, mob: &MobCore, cx: &TickContext) -> Option<TargetView> {
        self.known
            .iter()
            .filter_map(|s| target::view(cx, s.id))
            .find(|view| can_target(mob, view, cx))
    }

    fn load(list: &[Tag]) -> Self {
        let mut suspects = Self::default();
        for entry in list {
            let Some(entry) = entry.as_compound() else {
                continue;
            };
            let uuid = entry.get("uuid").and_then(Tag::as_uuid);
            let anger = entry
                .get("anger")
                .and_then(tuff_core::world::entity::number);
            if let (Some(uuid), Some(anger)) = (uuid, anger)
                && anger >= 0.0
            {
                suspects.saved.push((uuid, anger as i32));
            }
        }
        suspects
    }

    fn save(&self) -> Vec<Compound> {
        self.known
            .iter()
            .map(|s| (s.uuid, s.anger))
            .chain(self.saved.iter().copied())
            .map(|(uuid, anger)| {
                Compound::new()
                    .with("uuid", Tag::uuid(uuid))
                    .with("anger", anger)
            })
            .collect()
    }
}

fn find_uuid(cx: &TickContext, uuid: u128) -> Option<TargetView> {
    if let Some(player) = cx.players.iter().find(|p| p.uuid == uuid) {
        return target::view(cx, player.id);
    }
    let entity = cx.entities.iter().find(|e| e.uuid == uuid)?;
    target::view(cx, entity.id)
}

/// Whether it may be angry at or attack `view` at all: a living, mortal
/// non-Warden that is not in creative or spectator mode or an ally.
fn can_target(mob: &MobCore, view: &TargetView, cx: &TickContext) -> bool {
    view.id != mob.base.id
        && view.alive
        && !view.creative
        && !view.invulnerable
        && view.kind != EntityType::ArmorStand
        && view.kind != EntityType::Warden
        && !mob.allied_to(view, cx)
}

fn closer_than(from: [f64; 3], to: [f64; 3], xz: f64, y: f64) -> bool {
    let (dx, dy, dz) = (to[0] - from[0], to[1] - from[1], to[2] - from[2]);
    dx * dx + dz * dz < xz * xz && dy * dy < y * y
}

fn block_of(position: [f64; 3]) -> BlockPos {
    super::physics::block_pos(position)
}

#[derive(Debug)]
pub struct Warden {
    suspects: Suspects,
    /// Vibrations heard since the last tick: who made them and where.
    heard: Vec<(i32, [i32; 3])>,
    pose: Pose,
    client_anger: i32,
    activity: Activity,
    /// Ticks left emerging from the ground.
    emerging: i32,
    /// Its box is the low one of digging and emerging.
    low_box: bool,
    agitated_sound: bool,
    /// Ticks into digging back down.
    digging: Option<i32>,
    /// Ticks left before it digs down; `None` once it lapsed.
    dig_cooldown: Option<i64>,
    roar_target: Option<i32>,
    roaring: Option<i32>,
    /// Ticks into a sniff.
    sniffing: Option<i32>,
    sniff_cooldown: i32,
    disturbance: Option<(BlockPos, i32)>,
    vibration_cooldown: i32,
    touch_cooldown: i32,
    /// Ticks into a sonic boom.
    booming: Option<i32>,
    boom_cooldown: i32,
    attack_cooling: i32,
    /// Where its target stood when it last set off after it.
    chase_at: Option<BlockPos>,
    /// Brain memories this server does not use, kept as saved.
    other_memories: Compound,
}

impl Warden {
    /// Rises out of the ground: what a shrieker's summon does.
    pub fn emerge(&mut self, core: &mut MobCore) {
        self.emerging = EMERGE_TICKS;
        self.agitated_sound = true;
        self.dig_cooldown = Some(DIG_COOLDOWN);
        self.set_pose(core, Pose::Emerging);
    }

    pub fn is_emerging(&self) -> bool {
        self.emerging > 0
    }

    pub fn is_digging(&self) -> bool {
        self.digging.is_some()
    }

    /// Its anger toward `id`, for tests and commands.
    pub fn anger_at(&self, id: i32) -> i32 {
        self.suspects.anger_at(id)
    }

    fn digging_or_emerging(&self) -> bool {
        self.emerging > 0 || self.digging.is_some()
    }

    fn set_pose(&mut self, core: &mut MobCore, pose: Pose) {
        if self.pose != pose {
            self.pose = pose;
            core.changed(play::metadata::POSE);
        }
    }

    fn refresh_dig_cooldown(&mut self) {
        if self.dig_cooldown.is_some() {
            self.dig_cooldown = Some(DIG_COOLDOWN);
        }
    }

    fn active_anger(&self, core: &MobCore) -> i32 {
        match core.target {
            Some(id) => self.suspects.anger_at(id),
            None => self.suspects.highest(),
        }
    }

    fn angry(&self, core: &MobCore) -> bool {
        self.active_anger(core) >= ANGRY
    }

    fn angry_at(&self, core: &MobCore, cx: &TickContext) -> Option<TargetView> {
        if self.angry(core) {
            self.suspects.top(core, cx)
        } else {
            None
        }
    }

    fn increase_anger(
        &mut self,
        core: &mut MobCore,
        view: &TargetView,
        amount: i32,
        sound: bool,
        cx: &mut TickContext,
    ) {
        if core.no_ai || !can_target(core, view, cx) {
            return;
        }
        self.refresh_dig_cooldown();
        let may_switch = core
            .target
            .is_none_or(|id| !cx.players.iter().any(|p| p.id == id));
        let anger = self.suspects.increase(view, amount);
        if view.is_player() && may_switch && anger >= ANGRY {
            core.target = None;
        }
        if sound && self.pose != Pose::Roaring {
            let listening = if self.active_anger(core) >= AGITATED {
                SoundEvent::EntityWardenListeningAngry
            } else {
                SoundEvent::EntityWardenListening
            };
            let pitch = core.voice_pitch(cx.rng);
            core.sound(listening, 10.0, pitch, cx);
        }
    }

    fn set_disturbance(&mut self, core: &mut MobCore, at: BlockPos, cx: &TickContext) {
        if self.angry_at(core, cx).is_some() || core.target.is_some() {
            return;
        }
        self.refresh_dig_cooldown();
        self.sniff_cooldown = self.sniff_cooldown.max(DISTURBANCE_TICKS);
        self.disturbance = Some((at, DISTURBANCE_TICKS));
        core.navigation.stop();
    }

    fn set_attack_target(&mut self, core: &mut MobCore, id: i32) {
        self.roar_target = None;
        core.target = Some(id);
        self.boom_cooldown = NEW_TARGET_BOOM_COOLDOWN;
    }

    fn hear(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        for (source, at) in std::mem::take(&mut self.heard) {
            if core.no_ai || !core.is_alive() || self.digging_or_emerging() {
                continue;
            }
            let view = target::view(cx, source);
            if view.is_some_and(|v| !can_target(core, &v, cx)) {
                continue;
            }
            self.vibration_cooldown = VIBRATION_COOLDOWN;
            core.entity_event(EVENT_TENDRILS, cx);
            let pitch = core.voice_pitch(cx.rng);
            core.sound(SoundEvent::EntityWardenTendrilClicks, 5.0, pitch, cx);
            if let Some(view) = &view {
                self.increase_anger(core, view, DEFAULT_ANGER, true, cx);
            }
            if !self.angry(core) {
                let active = self.suspects.top(core, cx).map(|v| v.id);
                if active.is_none() || active == Some(source) {
                    self.set_disturbance(core, BlockPos::new(at[0], at[1], at[2]), cx);
                }
            }
        }
    }

    /// Whatever its body bumps into angers it and draws its attention.
    fn touch(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.no_ai || self.touch_cooldown > 0 || self.digging_or_emerging() {
            return;
        }
        let bounds = core.bounds();
        let player = cx
            .players
            .iter()
            .find(|p| super::physics::intersects(bounds, p.bounds()))
            .map(|p| p.id);
        let entity = || {
            cx.entities
                .iter()
                .find(|e| {
                    e.id != core.base.id
                        && e.living.is_some_and(|l| l.alive)
                        && super::physics::intersects(bounds, e.bounds)
                })
                .map(|e| e.id)
        };
        let Some(view) = player.or_else(entity).and_then(|id| target::view(cx, id)) else {
            return;
        };
        self.touch_cooldown = TOUCH_COOLDOWN;
        self.increase_anger(core, &view, DEFAULT_ANGER, true, cx);
        self.set_disturbance(core, block_of(view.position), cx);
    }

    /// The nearest thing it could be angry at within its follow range,
    /// players first, sight or not.
    fn nearest_attackable(&self, core: &MobCore, cx: &TickContext) -> Option<TargetView> {
        let range = core.follow_range();
        let near = |view: &TargetView| {
            let [x, y, z] = view.position;
            let [cx_, cy, cz] = core.base.position;
            (x - cx_).abs() <= range && (y - cy).abs() <= range && (z - cz).abs() <= range
        };
        let closest = |views: Vec<TargetView>| {
            views
                .into_iter()
                .filter(|v| near(v) && can_target(core, v, cx))
                .min_by(|a, b| {
                    core.distance_sq(a.position)
                        .total_cmp(&core.distance_sq(b.position))
                })
        };
        let players = cx
            .players
            .iter()
            .filter_map(|p| target::view(cx, p.id))
            .collect();
        closest(players).or_else(|| {
            closest(
                cx.entities
                    .iter()
                    .filter(|e| e.kind != EntityType::Player)
                    .filter_map(|e| target::view(cx, e.id))
                    .collect(),
            )
        })
    }

    fn pick_activity(&self, core: &MobCore) -> Activity {
        if self.emerging > 0 {
            Activity::Emerge
        } else if self.roar_target.is_none() && self.dig_cooldown.is_none() {
            Activity::Dig
        } else if self.roar_target.is_some() {
            Activity::Roar
        } else if core.target.is_some() {
            Activity::Fight
        } else if self.disturbance.is_some() {
            Activity::Investigate
        } else if self.sniffing.is_some() {
            Activity::Sniff
        } else {
            Activity::Idle
        }
    }

    fn leave(&mut self, core: &mut MobCore, from: Activity, cx: &mut TickContext) {
        match from {
            Activity::Roar => self.stop_roar(core),
            Activity::Sniff => self.stop_sniff(core, cx),
            Activity::Fight => {
                if self.booming.take().is_some() {
                    self.boom_cooldown = SONIC_BOOM_COOLDOWN;
                }
                core.set_aggressive(false);
                self.chase_at = None;
            }
            Activity::Emerge if self.pose == Pose::Emerging => {
                self.set_pose(core, Pose::Standing);
            }
            _ => {}
        }
        core.navigation.stop();
    }

    fn stop_roar(&mut self, core: &mut MobCore) {
        if self.roaring.take().is_none() {
            return;
        }
        if self.pose == Pose::Roaring {
            self.set_pose(core, Pose::Standing);
        }
        if let Some(id) = self.roar_target {
            self.set_attack_target(core, id);
        }
        self.roar_target = None;
    }

    fn stop_sniff(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.sniffing.take().is_none() {
            return;
        }
        if self.pose == Pose::Sniffing {
            self.set_pose(core, Pose::Standing);
        }
        if let Some(view) = self.nearest_attackable(core, cx) {
            if closer_than(
                core.base.position,
                view.position,
                SNIFF_ANGER_XZ,
                SNIFF_ANGER_Y,
            ) {
                self.increase_anger(core, &view, DEFAULT_ANGER, true, cx);
            }
            if self.disturbance.is_none() {
                self.set_disturbance(core, block_of(view.position), cx);
            }
        }
    }

    /// Picks whoever it is angry at to roar at.
    fn set_roar_target(&mut self, core: &MobCore, cx: &TickContext) -> bool {
        if self.roar_target.is_some() || core.target.is_some() {
            return false;
        }
        match self.angry_at(core, cx) {
            Some(view) => {
                self.roar_target = Some(view.id);
                true
            }
            None => false,
        }
    }

    fn look_at_block(core: &mut MobCore, at: BlockPos) {
        core.look.set_look_at(at.center(), 45.0, 90.0);
    }

    fn run(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        match self.activity {
            Activity::Emerge => {
                self.emerging -= 1;
                if self.emerging <= 0 {
                    self.emerging = 0;
                    self.set_pose(core, Pose::Standing);
                }
            }
            Activity::Dig => self.dig(core, cx),
            Activity::Roar => self.roar(core, cx),
            Activity::Fight => self.fight(core, cx),
            Activity::Investigate => {
                if self.set_roar_target(core, cx) {
                    return;
                }
                let Some((at, _)) = self.disturbance else {
                    return;
                };
                Self::look_at_block(core, at);
                let here = block_of(core.base.position);
                let close = {
                    let (dx, dy, dz) = (at.x - here.x, at.y - here.y, at.z - here.z);
                    dx * dx + dy * dy + dz * dz < 4
                };
                if !close && core.navigation.is_done() {
                    let to = BlockPos::new(
                        at.x + cx.rng.gen_range(-1..=1),
                        at.y,
                        at.z + cx.rng.gen_range(-1..=1),
                    );
                    core.move_to_block(to, 2, INVESTIGATE_SPEED, cx);
                }
            }
            Activity::Sniff => {
                if self.set_roar_target(core, cx) {
                    return;
                }
                if let Some(ticks) = &mut self.sniffing {
                    if *ticks == 0 {
                        core.sound(SoundEvent::EntityWardenSniff, 5.0, 1.0, cx);
                    }
                    *ticks += 1;
                    if *ticks >= SNIFF_TICKS {
                        self.stop_sniff(core, cx);
                    }
                }
            }
            Activity::Idle => {
                if self.set_roar_target(core, cx) {
                    return;
                }
                if self.sniff_cooldown <= 0
                    && self.disturbance.is_none()
                    && self.nearest_attackable(core, cx).is_some()
                {
                    self.sniffing = Some(0);
                    self.sniff_cooldown = cx.rng.gen_range(SNIFF_COOLDOWN);
                    core.navigation.stop();
                    self.set_pose(core, Pose::Sniffing);
                }
            }
        }
    }

    fn dig(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.base.vehicle.is_some() {
            cx.effects.push(Effect::StopRiding);
        }
        match &mut self.digging {
            Some(ticks) => {
                *ticks += 1;
                if *ticks >= DIG_TICKS {
                    core.discarded = true;
                }
            }
            None => {
                if core.target.is_some() || !core.navigation.is_done() {
                    return;
                }
                if core.base.on_ground {
                    self.digging = Some(0);
                    self.set_pose(core, Pose::Digging);
                    core.sound(SoundEvent::EntityWardenDig, 5.0, 1.0, cx);
                } else if core.in_liquid() {
                    core.sound(SoundEvent::EntityWardenAgitated, 5.0, 1.0, cx);
                    core.discarded = true;
                }
            }
        }
    }

    fn roar(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(id) = self.roar_target else {
            return;
        };
        match &mut self.roaring {
            None => {
                self.roaring = Some(0);
                core.navigation.stop();
                if let Some(view) = target::view(cx, id) {
                    core.look.set_look_at(view.eye, 30.0, 30.0);
                    self.set_pose(core, Pose::Roaring);
                    self.increase_anger(core, &view, ROAR_ANGER, false, cx);
                } else {
                    self.set_pose(core, Pose::Roaring);
                }
            }
            Some(ticks) => {
                *ticks += 1;
                if *ticks == ROAR_SOUND_AT {
                    core.sound(SoundEvent::EntityWardenRoar, 3.0, 1.0, cx);
                }
                if *ticks >= ROAR_TICKS {
                    self.stop_roar(core);
                }
            }
        }
    }

    fn fight(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.refresh_dig_cooldown();
        let target = core.target_view(cx);
        let valid = target.filter(|t| self.angry(core) && can_target(core, t, cx));
        let Some(target) = valid else {
            if let Some(view) = target
                && !can_target(core, &view, cx)
            {
                self.suspects.clear(view.id);
            }
            core.target = None;
            self.leave(core, Activity::Fight, cx);
            return;
        };
        core.set_aggressive(true);
        core.look.set_look_at(target.eye, 30.0, 30.0);
        let visible = core.has_line_of_sight(&target, cx);
        if visible && core.within_melee_range(&target) {
            core.navigation.stop();
        } else {
            let at = block_of(target.position);
            let moved = self.chase_at.is_none_or(|last| {
                let (dx, dy, dz) = (at.x - last.x, at.y - last.y, at.z - last.z);
                dx * dx + dy * dy + dz * dz > 4
            });
            if core.navigation.is_done() || moved {
                self.chase_at = Some(at);
                core.move_to_entity_reach(target.position, 0, FIGHT_SPEED, cx);
            }
        }
        self.sonic_boom(core, &target, cx);
        if self.attack_cooling <= 0 && visible && core.within_melee_range(&target) {
            self.attack_cooling = MELEE_COOLDOWN;
            core.swing(cx);
            core.entity_event(EVENT_ATTACK, cx);
            let pitch = core.voice_pitch(cx.rng);
            core.sound(SoundEvent::EntityWardenAttackImpact, 10.0, pitch, cx);
            self.boom_cooldown = SONIC_BOOM_COOLDOWN;
            core.hurt_target(&target, cx);
        }
    }

    fn sonic_boom(&mut self, core: &mut MobCore, target: &TargetView, cx: &mut TickContext) {
        let Some(ticks) = &mut self.booming else {
            if self.boom_cooldown <= 0
                && closer_than(
                    core.base.position,
                    target.position,
                    SONIC_BOOM_XZ,
                    SONIC_BOOM_Y,
                )
            {
                self.booming = Some(0);
                self.attack_cooling = SONIC_BOOM_TICKS;
                core.entity_event(EVENT_SONIC_BOOM, cx);
                core.sound(SoundEvent::EntityWardenSonicCharge, 3.0, 1.0, cx);
            }
            return;
        };
        *ticks += 1;
        let at = *ticks;
        core.look.set_look_at(target.position, 30.0, 30.0);
        if at == SONIC_BOOM_FIRE_AT
            && can_target(core, target, cx)
            && closer_than(
                core.base.position,
                target.position,
                SONIC_BOOM_XZ,
                SONIC_BOOM_Y,
            )
        {
            fire_boom(core, target, cx);
        }
        if at >= SONIC_BOOM_TICKS {
            self.booming = None;
            self.boom_cooldown = SONIC_BOOM_COOLDOWN;
        }
    }

    fn spread_darkness(&self, core: &MobCore, cx: &mut TickContext) {
        for player in cx.players {
            if player.creative
                || tuff_core::world::distance_sq(core.base.position, player.position)
                    >= DARKNESS_RADIUS * DARKNESS_RADIUS
            {
                continue;
            }
            cx.effects.push(Effect::TellPlayer {
                player: player.id,
                event: Box::new(PlayerEvent::AddEffect(darkness())),
            });
        }
    }

    fn count_down(&mut self) {
        for timer in [
            &mut self.sniff_cooldown,
            &mut self.vibration_cooldown,
            &mut self.touch_cooldown,
            &mut self.boom_cooldown,
            &mut self.attack_cooling,
        ] {
            *timer = (*timer - 1).max(0);
        }
        if let Some(left) = &mut self.dig_cooldown {
            *left -= 1;
            if *left <= 0 {
                self.dig_cooldown = None;
            }
        }
        if let Some((_, left)) = &mut self.disturbance {
            *left -= 1;
            if *left <= 0 {
                self.disturbance = None;
            }
        }
    }
}

/// The boom itself: a line of particles to the target's eyes, then a hit
/// that ignores armor and throws it back.
fn fire_boom(core: &MobCore, target: &TargetView, cx: &mut TickContext) {
    let [x, y, z] = core.base.position;
    let source = [x, y + CHEST_HEIGHT, z];
    let delta = [
        target.eye[0] - source[0],
        target.eye[1] - source[1],
        target.eye[2] - source[2],
    ];
    let length = delta.iter().map(|d| d * d).sum::<f64>().sqrt();
    let unit = if length > 1.0e-4 {
        delta.map(|d| d / length)
    } else {
        [0.0; 3]
    };
    let steps = length.floor() as i32 + 7;
    let particles = (1..steps).map(|i| {
        let i = f64::from(i);
        framed(&play::Particles {
            particle: ParticleType::SonicBoom.id(),
            position: [
                source[0] + unit[0] * i,
                source[1] + unit[1] * i,
                source[2] + unit[2] * i,
            ],
            spread: [0.0; 3],
            speed: 0.0,
            count: 1,
            alternative: false,
        })
    });
    cx.effects.push(Effect::ChunkPackets(packets(particles)));
    core.sound(SoundEvent::EntityWardenSonicBoom, 3.0, 1.0, cx);
    let mut hit = EntityHit::new(SONIC_BOOM_DAMAGE, &damage_type::SONIC_BOOM);
    hit.attacker = Some(Culprit::entity(&core.base));
    hit.position = Some(core.base.position);
    hit.push = Some([
        unit[0] * SONIC_BOOM_PUSH_XZ,
        unit[1] * SONIC_BOOM_PUSH_Y,
        unit[2] * SONIC_BOOM_PUSH_XZ,
    ]);
    hit.push_power = Some(1.0);
    cx.effects.push(Effect::Hurt {
        target: target.id,
        hit: Box::new(hit),
    });
}

const MEMORIES: &str = "memories";
const DIG_COOLDOWN_KEY: &str = "minecraft:dig_cooldown";
/// Its box height while it digs or emerges.
const LOW_BOX_HEIGHT: f32 = 1.0;
const IS_EMERGING_KEY: &str = "minecraft:is_emerging";
const IS_SNIFFING_KEY: &str = "minecraft:is_sniffing";
const SNIFF_COOLDOWN_KEY: &str = "minecraft:sniff_cooldown";
const VIBRATION_COOLDOWN_KEY: &str = "minecraft:vibration_cooldown";
const TOUCH_COOLDOWN_KEY: &str = "minecraft:touch_cooldown";
const SONIC_BOOM_COOLDOWN_KEY: &str = "minecraft:sonic_boom_cooldown";

fn memory_ttl(memory: &Tag) -> Option<i64> {
    let memory = memory.as_compound()?;
    Some(
        memory
            .get("ttl")
            .and_then(tuff_core::world::entity::number)
            .map_or(i64::MAX, |v| v as i64),
    )
}

fn unit_memory(ttl: Option<i64>) -> Compound {
    let memory = Compound::new().with("value", Compound::new());
    match ttl {
        Some(ttl) => memory.with("ttl", ttl),
        None => memory,
    }
}

impl Species for Warden {
    fn brain_state(&self, _core: &MobCore) -> crate::brain_debug::BrainState {
        let activity = match self.activity {
            Activity::Emerge => "emerge",
            Activity::Dig => "dig",
            Activity::Roar => "roar",
            Activity::Fight => "fight",
            Activity::Investigate => "investigate",
            Activity::Sniff => "sniff",
            Activity::Idle => "idle",
        };
        crate::brain_debug::BrainState {
            activity: Some(activity),
            anger_level: Some(self.client_anger),
            ..Default::default()
        }
    }

    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self {
            suspects: Suspects::default(),
            heard: Vec::new(),
            pose: Pose::Standing,
            client_anger: 0,
            activity: Activity::Idle,
            emerging: 0,
            low_box: false,
            agitated_sound: false,
            digging: None,
            dig_cooldown: Some(DIG_COOLDOWN),
            roar_target: None,
            roaring: None,
            sniffing: None,
            sniff_cooldown: 0,
            disturbance: None,
            vibration_cooldown: 0,
            touch_cooldown: 0,
            booming: None,
            boom_cooldown: 0,
            attack_cooling: 0,
            chase_at: None,
            other_memories: Compound::new(),
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = WARDEN_XP;
        core.navigation.can_float = true;
        core.walk_value = Some(|_, _, _| 0.0);
        for (kind, malus) in [
            (PathType::UnpassableRail, 0.0),
            (PathType::Damaging, 8.0),
            (PathType::PowderSnow, 8.0),
            (PathType::Lava, 8.0),
            (PathType::Fire, 0.0),
            (PathType::FireInNeighbor, 0.0),
        ] {
            core.set_path_malus(kind, malus);
        }
        let mut goals = GoalSelector::default();
        goals.add(0, Float);
        goals.add(
            10,
            RandomStroll::with_interval(IDLE_SPEED, 20)
                .unless(|core, _| core.signal != SIGNAL_IDLE),
        );
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        if let Some(list) = take(nbt, "anger", |t| {
            t.as_compound()?
                .get("suspects")?
                .as_list()
                .map(<[Tag]>::to_vec)
        }) {
            self.suspects = Suspects::load(&list);
        }
        let Some(brain) = take(nbt, "Brain", |t| t.as_compound().cloned()) else {
            return;
        };
        let mut memories = brain.get_compound(MEMORIES).cloned().unwrap_or_default();
        let mut ttl = |key: &str| {
            let ttl = memories.get(key).and_then(memory_ttl);
            memories.remove(key);
            ttl
        };
        let clamp = |ttl: i64| i32::try_from(ttl.max(0)).unwrap_or(i32::MAX);
        self.dig_cooldown = ttl(DIG_COOLDOWN_KEY);
        self.emerging = ttl(IS_EMERGING_KEY).map_or(0, clamp);
        self.sniffing = ttl(IS_SNIFFING_KEY).map(|_| 0);
        self.sniff_cooldown = ttl(SNIFF_COOLDOWN_KEY).map_or(0, clamp);
        self.vibration_cooldown = ttl(VIBRATION_COOLDOWN_KEY).map_or(0, clamp);
        self.touch_cooldown = ttl(TOUCH_COOLDOWN_KEY).map_or(0, clamp);
        self.boom_cooldown = ttl(SONIC_BOOM_COOLDOWN_KEY).map_or(0, clamp);
        if self.emerging > 0 {
            self.pose = Pose::Emerging;
        }
        self.other_memories = memories;
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert(
            "anger",
            Compound::new().with("suspects", self.suspects.save()),
        );
        let mut memories = self.other_memories.clone();
        let mut timed = |key: &str, ticks: i64| {
            if ticks > 0 {
                memories.insert(key, unit_memory(Some(ticks)));
            }
        };
        timed(DIG_COOLDOWN_KEY, self.dig_cooldown.unwrap_or(0));
        timed(IS_EMERGING_KEY, i64::from(self.emerging));
        timed(SNIFF_COOLDOWN_KEY, i64::from(self.sniff_cooldown));
        timed(VIBRATION_COOLDOWN_KEY, i64::from(self.vibration_cooldown));
        timed(TOUCH_COOLDOWN_KEY, i64::from(self.touch_cooldown));
        timed(SONIC_BOOM_COOLDOWN_KEY, i64::from(self.boom_cooldown));
        if self.sniffing.is_some() {
            memories.insert(IS_SNIFFING_KEY, unit_memory(None));
        }
        out.insert("Brain", Compound::new().with(MEMORIES, memories));
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (play::metadata::POSE, MetadataValue::Pose(self.pose as i32)),
            (ANGER_LEVEL, MetadataValue::VarInt(self.client_anger)),
        ]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let anger = self.active_anger(core);
        let ambient = if anger >= ANGRY {
            SoundEvent::EntityWardenAngry
        } else if anger >= AGITATED {
            SoundEvent::EntityWardenAgitated
        } else {
            SoundEvent::EntityWardenAmbient
        };
        let mut sounds = monster::sounds(
            ambient,
            SoundEvent::EntityWardenHurt,
            SoundEvent::EntityWardenDeath,
            Some(SoundEvent::EntityWardenStep),
        );
        if self.pose == Pose::Roaring || self.digging_or_emerging() {
            sounds.ambient = None;
        }
        sounds.volume = SOUND_VOLUME;
        sounds
    }

    fn immobile(&self, _core: &MobCore) -> bool {
        self.digging_or_emerging() || self.roaring.is_some() || self.sniffing.is_some()
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }

    fn heard(&mut self, core: &mut MobCore, notice: Notice) {
        let Notice::Heard { at, source } = notice else {
            return;
        };
        if core.no_ai
            || !core.is_alive()
            || self.digging_or_emerging()
            || self.vibration_cooldown > 0
            || !self.heard.is_empty()
        {
            return;
        }
        self.heard.push((source, at));
    }

    fn intercept_hurt(
        &mut self,
        core: &mut MobCore,
        damage: &Damage,
        _amount: f32,
        cx: &mut TickContext,
    ) -> Option<bool> {
        if self.digging_or_emerging() {
            return (!damage.is("bypasses_invulnerability")).then_some(false);
        }
        if core.no_ai {
            return None;
        }
        let attacker = damage
            .attacker
            .and_then(|(id, _)| target::view(cx, id))
            .filter(|v| v.id != core.base.id);
        if let Some(view) = attacker {
            self.increase_anger(core, &view, ON_HURT_ANGER, false, cx);
            let near =
                core.distance_sq(view.position) < HURT_TARGET_DISTANCE * HURT_TARGET_DISTANCE;
            if core.target.is_none() && (damage.projectile.is_none() || near) {
                self.set_attack_target(core, view.id);
            }
        }
        None
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.unpushable = self.digging_or_emerging();
        if self.low_box != self.digging_or_emerging() {
            self.low_box = !self.low_box;
            core.refresh_size();
            if self.low_box {
                core.size.height = core.size.height.min(LOW_BOX_HEIGHT);
                core.size.eye_height = core.size.eye_height.min(LOW_BOX_HEIGHT);
            }
        }
        if self.agitated_sound {
            self.agitated_sound = false;
            core.sound(SoundEvent::EntityWardenAgitated, 5.0, 1.0, cx);
            core.sound(SoundEvent::EntityWardenEmerge, 5.0, 1.0, cx);
        }
        if !core.is_alive() {
            return;
        }
        self.count_down();
        if core.persistence_required {
            self.refresh_dig_cooldown();
        }
        if core.no_ai {
            return;
        }
        self.hear(core, cx);
        self.touch(core, cx);
        let activity = self.pick_activity(core);
        if activity != self.activity {
            let from = self.activity;
            self.activity = activity;
            self.leave(core, from, cx);
        }
        core.signal = if activity == Activity::Idle {
            SIGNAL_IDLE
        } else {
            0
        };
        self.run(core, cx);
        if core
            .tick_count
            .wrapping_add(core.base.id as u64)
            .is_multiple_of(DARKNESS_INTERVAL)
        {
            self.spread_darkness(core, cx);
        }
        if core.tick_count.is_multiple_of(ANGER_TICK_INTERVAL) {
            self.suspects.tick(cx, core);
            let anger = self.active_anger(core);
            if anger != self.client_anger {
                self.client_anger = anger;
                core.changed(ANGER_LEVEL);
            }
        }
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Warden, super::load::<Warden>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::world::entity::{EntityBase, RegionEntity};

    fn view(id: i32, uuid: u128, player: bool) -> TargetView {
        TargetView {
            id,
            uuid,
            kind: if player {
                EntityType::Player
            } else {
                EntityType::Pig
            },
            position: [0.0; 3],
            bounds: [0.0; 6],
            eye: [0.0; 3],
            alive: true,
            creative: false,
            invulnerable: false,
            in_water: false,
            seen_as_enemy: true,
            owner: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn emerging_and_digging_use_the_low_box() {
        let base = EntityBase::new(EntityType::Warden, 2, 2, [0.5, 64.0, 0.5], [0.0; 3]);
        crate::tests::with_entities(&[], |_, cx| {
            let mut mob = crate::Mob::<Warden>::new(base, cx.rng);
            mob.core.no_ai = true;
            let tall = mob.core.size.height;
            assert!(tall > LOW_BOX_HEIGHT);
            mob.species.emerge(&mut mob.core);
            mob.tick(cx);
            assert_eq!(mob.core.size.height, LOW_BOX_HEIGHT);
            mob.species.emerging = 0;
            mob.tick(cx);
            assert_eq!(mob.core.size.height, tall);
            mob.species.digging = Some(0);
            mob.tick(cx);
            assert_eq!(mob.core.size.height, LOW_BOX_HEIGHT);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_digging_warden_gets_off_its_vehicle() {
        let base = EntityBase::new(EntityType::Warden, 2, 2, [0.5, 64.0, 0.5], [0.0; 3]);
        crate::tests::with_entities(&[], |_, cx| {
            let mut mob = crate::Mob::<Warden>::new(base, cx.rng);
            mob.core.base.vehicle = Some(Default::default());
            mob.species.dig_cooldown = None;
            mob.species.emerging = 0;
            cx.effects.clear();
            mob.tick(cx);
            assert!(cx.effects.iter().any(|e| matches!(e, Effect::StopRiding)));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn angry_players_rank_before_angrier_mobs() {
        let mut suspects = Suspects::default();
        suspects.increase(&view(1, 1, false), 120);
        suspects.increase(&view(2, 2, true), 35);
        assert_eq!(suspects.known[0].id, 1, "only the pig is angry");
        suspects.increase(&view(2, 2, true), 50);
        assert_eq!(suspects.known[0].id, 2, "an angry player comes first");
        assert_eq!(suspects.highest(), 120);
        suspects.increase(&view(1, 1, false), 100);
        assert_eq!(suspects.anger_at(1), MAX_ANGER);
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_anger_adds_to_a_returning_suspect() {
        let saved = vec![Tag::Compound(
            Compound::new().with("uuid", Tag::uuid(9)).with("anger", 70),
        )];
        let mut suspects = Suspects::load(&saved);
        assert_eq!(suspects.increase(&view(4, 9, true), 35), 105);
        assert_eq!(suspects.save().len(), 1);
    }
}
