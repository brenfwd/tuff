//! Dolphins: playful, air-breathing swimmers. They surface for air, flop
//! back into water, swim alongside swimming players (granting Dolphin's
//! Grace), leap out of open water, toss floating items about, steer clear
//! of guardians and fight back when hurt. Fed a fish, an adult swims
//! towards the nearest located structure. Floating items are carried in
//! its mouth and thrown back out.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, MapDecorationType, MobEffect, SoundEvent};

use super::attack::MeleeAttack;
use super::control::{LookStyle, MoveStyle, SmoothSwim, wrap_degrees};
use super::goal::{Flags, Goal, GoalSelector, reduced_delay};
use super::goals::{AvoidPlayer, FindWater, LookAtPlayer, RandomLookAround, RandomStroll};
use super::movement::{Sinking, WaterTravel};
use super::navigation::NavKind;
use super::physics::{self, block_pos};
use super::random_pos;
use super::target::{HurtByTarget, box_in_water};
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{
    Culprit, Effect, EntityInteraction, InteractOutcome, NearbyPlayer, TickContext, take_bool,
    take_number,
};
use tuff_core::world::exploration::{ExplorationMaps, ExplorationSpec, Located, poll_located};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const SWIM_SPEED: f64 = 1.0;
const SWIM_INTERVAL: i32 = 10;
const ATTACK_SPEED: f64 = 1.200_000_047_683_715_8;
const ITEM_SPEED: f64 = 1.200_000_047_683_715_8;
const ESCORT_SPEED: f64 = 4.0;
const MAX_AIR: i16 = 4800;
const SURFACE_BELOW_AIR: i16 = 140;
/// Dolphins are ageable (baby/age-locked at 16/17, confirmed against the
/// dumped layout), so their own two fields start at
/// [`data::SPECIES`]: `got_fish` (bool) then `moistness` (int).
const GOT_FISH: u8 = data::SPECIES;
const MOISTNESS: u8 = data::SPECIES + 1;
/// Ticks a dolphin lasts out of water before drying out hurts it.
const MOISTNESS_MAX: i32 = 2400;
const GRACE_TICKS: i64 = 100;
const GUARDIANS: &[EntityType] = &[EntityType::Guardian, EntityType::ElderGuardian];

fn animal_experience(rng: &mut StdRng) -> i32 {
    1 + rng.gen_range(0..3)
}

fn water_at(cx: &TickContext, pos: BlockPos) -> bool {
    (cx.block)(pos).is_some_and(|s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::fluid::key::WATER,
            s.fluid_state().fluid(),
        )
    })
}

/// The horizontal direction a yaw faces, as block steps.
fn facing_step(yaw: f32) -> (i32, i32) {
    match ((f64::from(yaw) / 90.0 + 0.5).floor() as i32) & 3 {
        0 => (0, 1),
        1 => (-1, 0),
        2 => (0, -1),
        _ => (1, 0),
    }
}

/// Swims up to air when its breath runs low, and keeps pushing that way.
#[derive(Debug, Default)]
struct BreathAir;

impl BreathAir {
    fn gives_air(cx: &TickContext, pos: BlockPos) -> bool {
        (cx.block)(pos).is_some_and(|state| {
            (state.fluid_state().is_empty()
                || state.block() == tuff_vanilla_data::Block::BubbleColumn)
                && state.is_land_pathfindable()
        })
    }

    fn find_air(mob: &mut MobCore, cx: &mut TickContext) {
        let here = block_pos(mob.base.position);
        let steps = [(0, 0), (0, -1), (1, 0), (0, 1), (-1, 0)];
        let found = steps.iter().find_map(|(dx, dz)| {
            (0..=8)
                .map(|dy| BlockPos::new(here.x + dx, here.y + dy, here.z + dz))
                .find(|pos| Self::gives_air(cx, *pos))
        });
        let [x, y, z] = mob.base.position;
        let pos = found.unwrap_or_else(|| block_pos([x, y + 8.0, z]));
        mob.move_to_position([pos.x as f64, f64::from(pos.y + 1), pos.z as f64], 1.0, cx);
    }
}

impl Goal for BreathAir {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.base.air < SURFACE_BELOW_AIR
    }

    fn interruptable(&self) -> bool {
        false
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.navigation.stop();
        Self::find_air(mob, cx);
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        Self::find_air(mob, cx);
        let input = [
            f64::from(mob.body.xxa),
            f64::from(mob.body.yya),
            f64::from(mob.body.zza),
        ];
        mob.move_relative(0.02, input);
        let velocity = mob.base.velocity;
        mob.move_self(velocity, cx);
    }
}

/// Whether a player swims (lies flat in water).
fn swimming(player: &NearbyPlayer, cx: &TickContext) -> bool {
    player.height < 1.0 && box_in_water(player.bounds(), cx)
}

/// Escorts the nearest swimming player within ten blocks, keeping it in
/// Dolphin's Grace.
#[derive(Debug, Default)]
struct SwimWithPlayer {
    player: Option<i32>,
}

impl SwimWithPlayer {
    fn grace(mob: &MobCore, player: i32, cx: &mut TickContext) {
        tuff_core::world::effects::apply_potion(
            player,
            true,
            EntityType::Player,
            &[ActiveEffect::timed(
                MobEffect::DolphinsGrace,
                GRACE_TICKS,
                0,
            )],
            &[],
            1.0,
            false,
            Some(Culprit::entity(&mob.base)),
            cx,
        );
    }

    /// The escorted player: id, position, eyes and whether it swims.
    fn player(&self, cx: &TickContext) -> Option<(i32, [f64; 3], [f64; 3], bool)> {
        cx.players
            .iter()
            .find(|p| Some(p.id) == self.player)
            .map(|p| (p.id, p.position, p.eye(), swimming(p, cx)))
    }
}

impl Goal for SwimWithPlayer {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let nearest = cx
            .players
            .iter()
            .map(|p| (mob.distance_sq(p.position), p))
            .filter(|(d, _)| *d <= 100.0)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| p);
        self.player = nearest
            .filter(|p| swimming(p, cx) && mob.target != Some(p.id))
            .map(|p| p.id);
        self.player.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.player(cx)
            .is_some_and(|(_, position, _, swims)| swims && mob.distance_sq(position) < 256.0)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(player) = self.player {
            Self::grace(mob, player, cx);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.player = None;
        mob.navigation.stop();
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some((id, position, eye, swims)) = self.player(cx) else {
            return;
        };
        mob.look
            .set_look_at(eye, mob.max_head_y_rot + 20.0, mob.max_head_x_rot);
        if mob.distance_sq(position) < 6.25 {
            mob.navigation.stop();
        } else {
            mob.move_to_entity(position, ESCORT_SPEED, cx);
        }
        if swims && cx.rng.gen_range(0..6) == 0 {
            Self::grace(mob, id, cx);
        }
    }
}

const FOLLOW_RANGE: f64 = 5.0;
const MOVED_EPSILON: f64 = 1.0e-5;

fn is_boat(kind: EntityType) -> bool {
    let name = kind.name();
    name.ends_with("_boat") || name.ends_with("_raft")
}

fn is_nautilus(kind: EntityType) -> bool {
    matches!(kind, EntityType::Nautilus | EntityType::ZombieNautilus)
}

fn moved_horizontally(player: &NearbyPlayer, cx: &TickContext) -> bool {
    let vehicle_speed = player
        .vehicle
        .and_then(|id| cx.entities.iter().find(|e| e.id == id))
        .map_or(0.0, |v| v.velocity[0].hypot(v.velocity[2]));
    player.movement[0]
        .hypot(player.movement[2])
        .max(vehicle_speed)
        > MOVED_EPSILON
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    GoToEntity,
    GoInDirection,
}

/// Swims alongside a player who steers a boat or a ridden nautilus nearby.
#[derive(Debug)]
struct FollowRiddenEntity {
    kind: fn(EntityType) -> bool,
    following: Option<i32>,
    mode: Follow,
    recalc: i32,
}

impl FollowRiddenEntity {
    fn new(kind: fn(EntityType) -> bool) -> Self {
        Self {
            kind,
            following: None,
            mode: Follow::GoToEntity,
            recalc: 0,
        }
    }

    fn riders<'a>(
        &self,
        mob: &MobCore,
        cx: &'a TickContext,
    ) -> impl Iterator<Item = &'a NearbyPlayer<'a>> {
        let kind = self.kind;
        let [a, b, c, d, e, f] = mob.bounds();
        let near = [
            a - FOLLOW_RANGE,
            b - FOLLOW_RANGE,
            c - FOLLOW_RANGE,
            d + FOLLOW_RANGE,
            e + FOLLOW_RANGE,
            f + FOLLOW_RANGE,
        ];
        cx.players.iter().filter(move |p| {
            p.vehicle.is_some_and(|id| {
                cx.entities
                    .iter()
                    .any(|v| v.id == id && kind(v.kind) && physics::intersects(v.bounds, near))
            })
        })
    }
}

impl Goal for FollowRiddenEntity {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if let Some(p) = self
            .following
            .and_then(|id| cx.players.iter().find(|p| p.id == id))
            && moved_horizontally(p, cx)
        {
            return true;
        }
        self.riders(mob, cx).any(|p| moved_horizontally(p, cx))
    }

    fn can_continue(&mut self, _mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.following
            .and_then(|id| cx.players.iter().find(|p| p.id == id))
            .is_some_and(|p| p.vehicle.is_some() && moved_horizontally(p, cx))
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let first = self.riders(mob, cx).next().map(|p| p.id);
        self.following = first;
        self.recalc = 0;
        self.mode = Follow::GoToEntity;
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.following = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some((position, yaw)) = self
            .following
            .and_then(|id| cx.players.iter().find(|p| p.id == id))
            .map(|p| (p.position, p.yaw))
        else {
            return;
        };
        let speed = if self.mode == Follow::GoInDirection {
            0.01
        } else {
            0.015
        };
        mob.move_relative(
            speed,
            [
                mob.body.xxa.into(),
                mob.body.yya.into(),
                mob.body.zza.into(),
            ],
        );
        mob.move_self(mob.base.velocity, cx);
        self.recalc -= 1;
        if self.recalc > 0 {
            return;
        }
        self.recalc = self.adjusted_delay(10);
        let (dx, dz) = facing_step(yaw);
        let at = block_pos(position);
        let distance = mob.distance_sq(position).sqrt();
        match self.mode {
            Follow::GoToEntity => {
                let behind = BlockPos::new(at.x - dx, at.y - 1, at.z - dz);
                mob.move_to_block(behind, 1, 1.0, cx);
                if distance < 4.0 {
                    self.recalc = 0;
                    self.mode = Follow::GoInDirection;
                }
            }
            Follow::GoInDirection => {
                let ahead = BlockPos::new(at.x + dx * 10, at.y - 1, at.z + dz * 10);
                mob.move_to_block(ahead, 1, 1.0, cx);
                if distance > 12.0 {
                    self.recalc = 0;
                    self.mode = Follow::GoToEntity;
                }
            }
        }
    }
}

/// Now and then leaps out of open water in the way it is heading.
#[derive(Debug)]
struct Jump {
    interval: i32,
    breached: bool,
}

impl Jump {
    const STEPS: [i32; 6] = [0, 1, 4, 5, 6, 7];

    fn new(interval: i32) -> Self {
        Self {
            interval: reduced_delay(interval),
            breached: false,
        }
    }
}

impl Goal for Jump {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if cx.rng.gen_range(0..self.interval) != 0 {
            return false;
        }
        let (sx, sz) = facing_step(mob.base.yaw);
        let here = block_pos(mob.base.position);
        let air = |pos: BlockPos| (cx.block)(pos).is_some_and(tuff_world_model::chunk::is_air);
        Self::STEPS.iter().all(|step| {
            let pos = BlockPos::new(here.x + sx * step, here.y, here.z + sz * step);
            water_at(cx, pos)
                && !(cx.block)(pos).is_some_and(|s| {
                    tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::block::key::BLOCKS_DOLPHIN_JUMP,
                        s.block(),
                    )
                })
                && air(BlockPos::new(pos.x, pos.y + 1, pos.z))
                && air(BlockPos::new(pos.x, pos.y + 2, pos.z))
        })
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        let vy = mob.base.velocity[1];
        (vy * vy >= 0.029_999_999_329_447_746
            || mob.base.pitch == 0.0
            || mob.base.pitch.abs() >= 10.0
            || !mob.body.in_water)
            && !mob.base.on_ground
    }

    fn interruptable(&self) -> bool {
        false
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        let (sx, sz) = facing_step(mob.base.yaw);
        let v = &mut mob.base.velocity;
        v[0] += f64::from(sx) * 0.6;
        v[1] += 0.7;
        v[2] += f64::from(sz) * 0.6;
        mob.impulse = true;
        mob.navigation.stop();
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.base.pitch = 0.0;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let already = self.breached;
        if !already {
            self.breached = water_at(cx, block_pos(mob.base.position));
        }
        if self.breached && !already {
            mob.sound(SoundEvent::EntityDolphinJump, 1.0, 1.0, cx);
        }
        let [vx, vy, vz] = mob.base.velocity;
        if vy * vy < 0.029_999_999_329_447_746 && mob.base.pitch != 0.0 {
            let pitch = mob.base.pitch;
            mob.base.pitch = pitch + 0.2 * wrap_degrees(0.0 - pitch);
        } else if (vx * vx + vy * vy + vz * vz).sqrt() > 9.999_999_747_378_752e-6 {
            let horizontal = (vx * vx + vz * vz).sqrt();
            mob.base.pitch = ((-vy).atan2(horizontal) * 57.295_776_367_187_5) as f32;
        }
    }
}

/// Swims towards the nearest structure of interest after being fed a fish.
#[derive(Default)]
struct SwimToTreasure {
    treasure: Option<BlockPos>,
    searching: Option<Located>,
    next: Option<[f64; 3]>,
    stuck: bool,
}

impl std::fmt::Debug for SwimToTreasure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwimToTreasure")
            .field("treasure", &self.treasure)
            .field("stuck", &self.stuck)
            .finish()
    }
}

impl SwimToTreasure {
    fn arrived(&self, mob: &MobCore) -> bool {
        self.treasure.is_none_or(|treasure| {
            let [x, _, z] = mob.base.position;
            let (dx, dz) = (
                f64::from(treasure.x) + 0.5 - x,
                f64::from(treasure.z) + 0.5 - z,
            );
            dx * dx + dz * dz < 16.0
        })
    }

    fn close_to_next(&self, mob: &MobCore) -> bool {
        self.next.is_some_and(|next| mob.distance_sq(next) < 144.0)
    }

    fn pick_next(mob: &MobCore, center: [f64; 3], cx: &mut TickContext) -> Option<[f64; 3]> {
        let narrow = random_pos::pos_towards_within(mob, 16, 1, center, 0.392_699_098_372_9, cx)
            .or_else(|| {
                random_pos::pos_towards_within(mob, 8, 4, center, std::f64::consts::FRAC_PI_2, cx)
            })?;
        if water_at(cx, block_pos(narrow)) {
            return Some(narrow);
        }
        random_pos::pos_towards_within(mob, 8, 5, center, std::f64::consts::FRAC_PI_2, cx)
            .or(Some(narrow))
    }
}

impl Goal for SwimToTreasure {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn interruptable(&self) -> bool {
        false
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.signal != 0 && mob.base.air >= SURFACE_BELOW_AIR_FOR_TREASURE
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        if self.searching.is_some() {
            return true;
        }
        self.treasure.is_some()
            && !self.arrived(mob)
            && !self.stuck
            && mob.base.air >= SURFACE_BELOW_AIR_FOR_TREASURE
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.stuck = false;
        self.treasure = None;
        self.next = None;
        mob.navigation.stop();
        let [x, y, z] = mob.base.position;
        let origin = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let spec = ExplorationSpec {
            destination: "#minecraft:dolphin_located".to_owned(),
            decoration: MapDecorationType::Mansion,
            zoom: 0,
            search_radius: 50,
            skip_existing: false,
        };
        self.searching = cx
            .shared
            .services
            .get::<ExplorationMaps>()
            .and_then(|maps| maps.locate(cx.dimension, origin, spec));
        self.stuck = self.searching.is_none();
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        let searching = self.searching.take().is_some();
        if !searching && (self.arrived(mob) || self.stuck) {
            mob.signal = 0;
        }
        self.treasure = None;
        self.next = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(answer) = self.searching.as_mut() {
            match poll_located(answer) {
                None => return,
                Some(found) => {
                    self.searching = None;
                    match found {
                        Some(pos) => {
                            self.treasure = Some(pos);
                            mob.entity_event(38, cx);
                        }
                        None => self.stuck = true,
                    }
                }
            }
        }
        let Some(treasure) = self.treasure else {
            return;
        };
        if !(self.close_to_next(mob) || mob.navigation.is_done()) {
            return;
        }
        let center = [
            f64::from(treasure.x) + 0.5,
            f64::from(treasure.y) + 0.5,
            f64::from(treasure.z) + 0.5,
        ];
        let Some(next) = Self::pick_next(mob, center, cx) else {
            self.stuck = true;
            return;
        };
        mob.look
            .set_look_at(next, mob.max_head_y_rot + 20.0, mob.max_head_x_rot);
        self.next = Some(next);
        mob.move_to_position(next, 1.3, cx);
        if cx.rng.gen_range(0..80) == 0 {
            mob.entity_event(38, cx);
        }
    }
}

const SURFACE_BELOW_AIR_FOR_TREASURE: i16 = 100;

fn floating_items(mob: &MobCore, cx: &TickContext) -> Vec<(i32, [f64; 3])> {
    let area = physics::inflate(mob.bounds(), [8.0; 3]);
    cx.entities
        .iter()
        .filter(|e| {
            e.item.is_some_and(|(_, _, free)| free)
                && physics::intersects(area, e.bounds)
                && box_in_water(e.bounds, cx)
        })
        .map(|e| (e.id, e.position))
        .collect()
}

fn holding(mob: &MobCore) -> bool {
    mob.equipment.main_hand().is_some()
}

/// Throws what it carries out of its mouth.
fn spit_out(mob: &mut MobCore, cx: &mut TickContext) -> bool {
    let Some(stack) = mob.equipment.take(EquipmentSlot::Mainhand) else {
        return false;
    };
    let [x, y, z] = mob.base.position;
    let (yaw, pitch) = (mob.base.yaw.to_radians(), mob.base.pitch.to_radians());
    let direction = cx.rng.r#gen::<f32>() * std::f32::consts::TAU;
    let wobble = 0.02 * cx.rng.r#gen::<f32>();
    cx.effects.push(Effect::DropItem {
        stack,
        position: [x, y + f64::from(mob.size.eye_height) - 0.3, z],
        velocity: [
            f64::from(0.3 * -yaw.sin() * pitch.cos() + direction.cos() * wobble),
            f64::from(0.3 * pitch.sin() * 1.5),
            f64::from(0.3 * yaw.cos() * pitch.cos() + direction.sin() * wobble),
        ],
        pickup_delay: 40,
    });
    true
}

/// Swims after floating items to pick them up.
#[derive(Debug, Default)]
struct MoveToItem {
    cooldown: u64,
}

impl Goal for MoveToItem {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.cooldown <= cx.tick && !floating_items(mob, cx).is_empty()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some((_, position)) = floating_items(mob, cx).first() {
            mob.move_to_entity(*position, ITEM_SPEED, cx);
            mob.sound(SoundEvent::EntityDolphinPlay, 1.0, 1.0, cx);
        }
        self.cooldown = 0;
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if spit_out(mob, cx) {
            self.cooldown = cx.tick + cx.rng.gen_range(0..100);
        }
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some((_, position)) = floating_items(mob, cx).first() {
            mob.move_to_entity(*position, ITEM_SPEED, cx);
        }
    }
}

/// Plays with what it carries by throwing it away again.
#[derive(Debug, Default)]
struct PlayWithItems;

impl Goal for PlayWithItems {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        holding(mob)
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        spit_out(mob, cx);
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        spit_out(mob, cx);
    }
}

#[derive(Debug)]
pub struct Dolphin {
    got_fish: bool,
    moistness: i32,
    pickup_after: u64,
}

impl Default for Dolphin {
    fn default() -> Self {
        Self {
            got_fish: false,
            moistness: MOISTNESS_MAX,
            pickup_after: 0,
        }
    }
}

impl Dolphin {
    fn pick_up(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.can_pick_up_loot
            || core.dead
            || holding(core)
            || cx.tick < self.pickup_after
            || !cx.shared.game_rules.borrow().bool("mob_griefing")
        {
            return;
        }
        let reach = physics::inflate(core.bounds(), [1.0, 0.0, 1.0]);
        let Some((id, count)) = cx.entities.iter().find_map(|e| {
            let (_, count, takeable) = e.item?;
            (takeable && count > 0 && physics::intersects(e.bounds, reach)).then_some((e.id, count))
        }) else {
            return;
        };
        cx.effects.push(Effect::TakeItem { item: id, count });
        self.pickup_after = cx.tick + 2;
    }
}

impl Species for Dolphin {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.can_pick_up_loot = true;
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Water, 0.0);
        core.navigation.kind = NavKind::WaterBound { breaching: true };
        core.move_style = MoveStyle::SmoothSwim(SmoothSwim {
            max_turn_x: 85.0,
            max_turn_y: 10.0,
            in_water_speed: 0.02,
            outside_water_speed: 0.1,
            buoyant: true,
        });
        core.look_style = LookStyle::SmoothSwim {
            max_from_center: 10.0,
        };
        core.max_head_x_rot = 1.0;
        core.max_head_y_rot = 1.0;
        core.walk_value = Some(|_, _, _| 0.0);
        core.water_travel = WaterTravel::Swimmer {
            stroke: None,
            sinks: Sinking::WithoutTarget,
        };
        let mut goals = GoalSelector::default();
        goals.add(0, BreathAir);
        goals.add(0, FindWater::steering());
        goals.add(1, SwimToTreasure::default());
        goals.add(1, HurtByTarget::new(GUARDIANS));
        goals.add(2, SwimWithPlayer::default());
        goals.add(4, RandomStroll::swimming(SWIM_SPEED, SWIM_INTERVAL));
        goals.add(4, RandomLookAround::default());
        goals.add(5, LookAtPlayer::new(6.0));
        goals.add(5, Jump::new(10));
        goals.add(6, MeleeAttack::new(ATTACK_SPEED, true));
        goals.add(7, MoveToItem::default());
        goals.add(8, PlayWithItems);
        goals.add(8, FollowRiddenEntity::new(is_boat));
        goals.add(8, FollowRiddenEntity::new(is_nautilus));
        goals.add(9, AvoidPlayer::entities(GUARDIANS, 8.0, 1.0, 1.0));
        core.goals = goals;
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.base.air = MAX_AIR;
        core.base.pitch = 0.0;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.got_fish = take_bool(nbt, "GotFish").unwrap_or(false);
        core.signal = i32::from(self.got_fish);
        self.moistness = take_number(nbt, "Moistness").map_or(MOISTNESS_MAX, |v| v as i32);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("GotFish", self.got_fish);
        out.insert("Moistness", self.moistness);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (GOT_FISH, MetadataValue::Boolean(self.got_fish)),
            (MOISTNESS, MetadataValue::VarInt(self.moistness)),
        ]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(if core.body.in_water {
                SoundEvent::EntityDolphinAmbientWater
            } else {
                SoundEvent::EntityDolphinAmbient
            }),
            hurt: Some(SoundEvent::EntityDolphinHurt),
            death: Some(SoundEvent::EntityDolphinDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn max_air(&self) -> i16 {
        MAX_AIR
    }

    fn refills_air_at_once(&self) -> bool {
        true
    }

    /// Out of water (and rain) it dries out, hurting once its moistness is
    /// gone, and flops about on the ground.
    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.no_ai {
            if core.base.air != MAX_AIR {
                core.base.air = MAX_AIR;
                core.changed(play::metadata::AIR_SUPPLY);
            }
            return;
        }
        if (core.signal != 0) != self.got_fish {
            self.got_fish = core.signal != 0;
            core.changed(GOT_FISH);
        }
        let before = self.moistness;
        if core.body.in_water || cx.raining {
            self.moistness = MOISTNESS_MAX;
        } else {
            self.moistness -= 1;
            if self.moistness <= 0 {
                core.hurt_later(1.0, &damage_type::DRY_OUT);
            }
            if core.base.on_ground {
                let dx = (cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.2;
                let dz = (cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.2;
                let velocity = &mut core.base.velocity;
                velocity[0] += f64::from(dx);
                velocity[1] += 0.5;
                velocity[2] += f64::from(dz);
                core.base.yaw = cx.rng.r#gen::<f32>() * 360.0;
                core.base.on_ground = false;
                core.impulse = true;
            }
        }
        if self.moistness != before {
            core.changed(MOISTNESS);
        }
    }

    /// Babies never fight.
    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.is_baby() {
            core.target = None;
        }
        self.pick_up(core, cx);
    }

    fn took_item(&mut self, core: &mut MobCore, stack: ItemStack) {
        if !holding(core) {
            core.equipment.set(EquipmentSlot::Mainhand, Some(stack));
            core.equipment.set_drop_chance(
                EquipmentSlot::Mainhand,
                super::equipment::PRESERVED_DROP_CHANCE,
            );
            core.persistence_required = true;
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if !tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::FISHES, stack.item()) {
            return None;
        }
        core.sound_as(
            SoundEvent::EntityDolphinEat,
            play::sound_category::NEUTRAL,
            1.0,
            1.0,
            cx,
        );
        if core
            .age
            .as_ref()
            .is_some_and(super::animal::Age::can_age_up)
        {
            let ticks_until_adult = -core.age.as_ref().expect("checked above").age;
            core.age_up(super::animal::speed_up_seconds(ticks_until_adult), true);
        } else {
            self.got_fish = true;
            core.signal = 1;
            core.changed(GOT_FISH);
        }
        Some(HandEffect::Consume(1))
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Dolphin, super::load::<Dolphin>);
    registry.register_spawn_rule(EntityType::Dolphin, super::spawning::surface_water_rule);
}

#[cfg(test)]
mod tests {
    use tuff_vanilla_data::Item;

    use super::*;
    use tuff_item::item::{ItemStack, MintReason};

    fn fish_interaction() -> EntityInteraction {
        EntityInteraction {
            player: 1,
            player_uuid: 1,
            position: [0.0; 3],
            location: [0.0; 3],
            stack: Some(tuff_item::item::ItemSnapshot::describe(
                Item::Cod,
                1,
                Default::default(),
            )),
            hand: tuff_core::world::entity::HandOffer::new(
                (Some(tuff_item::item::ItemSnapshot::describe(
                    Item::Cod,
                    1,
                    Default::default(),
                )))
                .as_deref()
                .map(tuff_item::item::ItemStack::test_copy),
            ),
            off_hand: false,
            creative: false,
            sneaking: false,
            riding: false,
            hero: None,
        }
    }

    fn rider(vehicle: Option<i32>, movement: [f64; 3]) -> NearbyPlayer<'static> {
        NearbyPlayer {
            id: 7,
            uuid: 7,
            name: "",
            position: [3.0, 64.0, 0.5],
            eye_height: 1.62,
            yaw: 0.0,
            pitch: 0.0,
            held: [None, None],
            creative: false,
            height: 1.8,
            sneaking: false,
            invisible: false,
            invulnerable: false,
            armor: [None, None, None, None],
            effects: 0,
            movement,
            vehicle,
            sleeping: None,
            last_hurt_by: None,
            last_hurt_mob: None,
            perchable: false,
            sprinting: false,
        }
    }

    fn vehicle(kind: EntityType, x: f64) -> tuff_core::world::entity::EntitySummary {
        tuff_core::world::entity::EntitySummary {
            id: 9,
            uuid: 99,
            kind,
            position: [x, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [x - 0.7, 64.0, -0.2, x + 0.7, 64.6, 1.2],
            living: None,
            pickable: true,
            item: None,
            vehicle: None,
            carries: true,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    fn follows(kind: EntityType, x: f64, movement: [f64; 3], boats: bool) -> bool {
        let matcher: fn(EntityType) -> bool = if boats { is_boat } else { is_nautilus };
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 {
                tuff_vanilla_data::Block::GrassBlock.default_state()
            } else {
                tuff_vanilla_data::Block::Air.default_state()
            })
        };
        let mut result = false;
        super::super::tests::with_players(
            &block,
            &[vehicle(kind, x)],
            &[rider(Some(9), movement)],
            |core, cx| {
                result = FollowRiddenEntity::new(matcher).can_use(core, cx);
            },
        );
        result
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_dolphin_follows_a_moving_boat_or_nautilus_rider_within_five_blocks() {
        assert!(follows(EntityType::OakBoat, 3.0, [0.0, 0.0, 0.2], true));
        assert!(follows(EntityType::Nautilus, 3.0, [0.2, 0.0, 0.0], false));
        assert!(!follows(EntityType::OakBoat, 3.0, [0.0; 3], true));
        assert!(!follows(EntityType::OakBoat, 30.0, [0.0, 0.0, 0.2], true));
        assert!(!follows(EntityType::OakBoat, 3.0, [0.0, 0.0, 0.2], false));
    }

    #[tuff_core::watchdog]
    #[test]
    fn dolphin_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Dolphin.data_layout();
        assert_eq!(layout.len(), MOISTNESS as usize + 1);
        assert_eq!(
            layout[GOT_FISH as usize],
            MetadataValue::Boolean(false).type_id() as u8
        );
        assert_eq!(
            layout[MOISTNESS as usize],
            MetadataValue::VarInt(0).type_id() as u8
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn feeding_an_adult_fish_marks_it_as_fed_without_breeding() {
        super::super::tests::with_context(|core, cx| {
            Dolphin::configure(core);
            let mut dolphin = Dolphin::default();
            let outcome = dolphin.interact(core, &fish_interaction(), cx);
            assert!(outcome.is_some());
            assert!(dolphin.got_fish);
            assert!(
                core.love.as_ref().is_none_or(|l| !l.is_in_love()),
                "dolphins never enter love mode from feeding"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn feeding_a_baby_fish_ages_it_up_instead() {
        super::super::tests::with_context(|core, cx| {
            Dolphin::configure(core);
            core.set_baby(true);
            let before = core.age.as_ref().unwrap().age;
            let mut dolphin = Dolphin::default();
            dolphin.interact(core, &fish_interaction(), cx);
            assert!(!dolphin.got_fish);
            assert!(core.age.as_ref().unwrap().age > before);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn dolphins_never_breed_by_feeding() {
        super::super::tests::with_context(|core, cx| {
            Dolphin::configure(core);
            let dolphin = Dolphin::default();
            assert!(!dolphin.is_food(&ItemStack::mint(Item::Cod, 1, MintReason::Test)));
            let _ = cx;
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stranded_dolphin_dries_out_but_keeps_its_breath_apart() {
        super::super::tests::with_context(|core, cx| {
            Dolphin::configure(core);
            core.base.air = MAX_AIR;
            let mut dolphin = Dolphin::default();
            for _ in 0..MOISTNESS_MAX {
                dolphin.tick(core, cx);
            }
            assert_eq!(dolphin.moistness, 0);
            assert_eq!(core.base.air, MAX_AIR, "moisture is not its air");
            assert!(!core.pending_hurts.is_empty(), "dry, it starts to hurt");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn yaw_faces_the_game_s_horizontal_directions() {
        assert_eq!(facing_step(0.0), (0, 1));
        assert_eq!(facing_step(90.0), (-1, 0));
        assert_eq!(facing_step(180.0), (0, -1));
        assert_eq!(facing_step(-90.0), (1, 0));
    }

    fn item_summary(at: [f64; 3]) -> tuff_core::world::entity::EntitySummary {
        tuff_core::world::entity::EntitySummary {
            id: 77,
            uuid: 77,
            kind: EntityType::Item,
            position: at,
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [
                at[0] - 0.1,
                at[1],
                at[2] - 0.1,
                at[0] + 0.1,
                at[1] + 0.2,
                at[2] + 0.1,
            ],
            living: None,
            pickable: false,
            item: Some((Item::Stick, 1, true)),
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_dolphin_takes_a_nearby_item_in_its_mouth_and_throws_it_back() {
        let mut dolphin = Dolphin::default();
        let at = [0.5, 64.0, 0.5];
        super::super::tests::with_entities(&[item_summary(at)], |core, cx| {
            Dolphin::configure(core);
            core.base.position = at;
            dolphin.ai_step(core, cx);
            assert!(
                cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::TakeItem { item: 77, count: 1 }))
            );
            dolphin.took_item(core, ItemStack::mint(Item::Stick, 1, MintReason::Test));
            assert!(holding(core));
            assert!(PlayWithItems.can_use(core, cx));
            PlayWithItems.tick(core, cx);
            assert!(!holding(core));
            assert!(cx.effects.iter().any(|e| matches!(
                e,
                Effect::DropItem { pickup_delay: 40, stack, .. } if stack.item() == Item::Stick
            )));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn feeding_a_fish_arms_the_treasure_goal_until_it_gives_up() {
        super::super::tests::with_context(|core, cx| {
            Dolphin::configure(core);
            core.base.air = MAX_AIR;
            let mut dolphin = Dolphin::default();
            let mut goal = SwimToTreasure::default();
            assert!(!goal.can_use(core, cx));
            dolphin.interact(core, &fish_interaction(), cx);
            assert!(goal.can_use(core, cx));
            goal.start(core, cx);
            assert!(goal.stuck, "no locator service registered");
            assert!(!goal.can_continue(core, cx));
            goal.stop(core, cx);
            assert_eq!(core.signal, 0);
            dolphin.tick(core, cx);
            assert!(!dolphin.got_fish);
        });
    }
}
