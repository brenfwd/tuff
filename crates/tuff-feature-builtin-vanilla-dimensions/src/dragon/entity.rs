//! The ender dragon: a flying boss that paths over a ring of nodes around
//! the island, strafes players with breath fireballs, perches on the exit
//! portal to breathe fire, heals from nearby end crystals and dies in a
//! 200-tick animation that drops its experience.
//!
//! Clients see eight hitbox parts numbered after the dragon's own id; the
//! dragon keeps their positions so hits and explosions land on a part (the
//! head and neck take full damage, the rest a quarter plus one).

use crate::dragon::fight::FightsExt;
use rand::Rng;
use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::fight::{DragonReport, FightMessage, FightView};
use super::phase::{FlightGraph, FlightHistory, Node, Path, PhaseKind};
use tuff_core::command::coords::wrap_degrees;
use tuff_core::mth;
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{
    AttackOutcome, Culprit, Effect, EntityAttack, EntityBase, EntityHit, NearbyPlayer, Notice,
    RegionEntity, Tick, TickContext, take_number,
};
use tuff_core::world::messages::PlayerId;
use tuff_core::world::{framed, packets};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

pub const PART_COUNT: usize = 8;
const HEAD: usize = 0;
const NECK: usize = 1;
const BODY: usize = 2;
const WING_LEFT: usize = 6;
const WING_RIGHT: usize = 7;
const DATA_HEALTH: u8 = 9;
const DATA_PHASE: u8 = 16;
const DEG: f32 = std::f32::consts::PI / 180.0;
/// Damage taken while perched that makes it take off, as a share of its
/// health.
const SITTING_ALLOWED_DAMAGE: f32 = 0.25;
const WING_DAMAGE: f32 = 5.0;
const HEAD_DAMAGE: f32 = 10.0;
const CRYSTAL_DAMAGE: f32 = 10.0;
const DEATH_TICKS: i32 = 200;
const FIRST_KILL_EXPERIENCE: i32 = 12000;
const EXPERIENCE: i32 = 500;
const HURT_DURATION: i16 = 10;
const DAMAGE_COOLDOWN: i32 = 20;
const AMBIENT_INTERVAL: i32 = 80;
const SOUND_VOLUME: f32 = 5.0;

#[derive(Debug, Default)]
struct Pathing {
    path: Option<Path>,
    target: Option<[f64; 3]>,
}

#[derive(Debug, Default)]
struct Phases {
    holding: Pathing,
    clockwise: bool,
    strafe: Pathing,
    strafe_clockwise: bool,
    fireball_charge: i32,
    /// The player a strafe attacks, by entity id.
    strafe_target: Option<i32>,
    approach: Pathing,
    landing: Option<[f64; 3]>,
    takeoff: Pathing,
    takeoff_first_tick: bool,
    flame_ticks: i32,
    flame_count: i32,
    /// The breath cloud of the current flame, by entity id.
    flame: Option<i32>,
    scanning_time: i32,
    attacking_ticks: i32,
    charge: Option<[f64; 3]>,
    time_since_charge: i32,
    dying: Option<[f64; 3]>,
    dying_time: i32,
    hover: Option<[f64; 3]>,
}

/// One hit on a part: world damage waits for the next tick, which can
/// queue effects.
#[derive(Debug, Clone, Copy)]
struct PendingHurt {
    part: usize,
    amount: f32,
    kind: &'static DamageType,
}

#[derive(Debug)]
pub struct EnderDragon {
    pub base: EntityBase,
    health: f32,
    absorption: f32,
    last_hurt: f32,
    hurt_time: i16,
    death_time: i32,
    sitting_damage: f32,
    phase: PhaseKind,
    phases: Phases,
    history: FlightHistory,
    parts: [[f64; 3]; PART_COUNT],
    yaw_speed: f32,
    in_wall: bool,
    graph: Option<FlightGraph>,
    nearest_crystal: Option<(u128, [f64; 3])>,
    origin: BlockPos,
    ticks: u64,
    ambient_sound_time: i32,
    seen_destroyed: Option<u64>,
    pending: Vec<PendingHurt>,
    /// Its part ids (the next eight after its own) are reserved.
    ids_reserved: bool,
    last_report: Option<(f32, u64)>,
    changed: Vec<u8>,
    impulse: bool,
    /// The player whose hit most recently landed, credited with the kill
    /// if it is the one that brings the dragon down.
    killer: Option<PlayerId>,
}

mod geometry;
mod region;
use geometry::*;

fn max_health() -> f32 {
    EntityType::EnderDragon
        .base_attribute(tuff_vanilla_data::registry::Attribute::MaxHealth)
        .unwrap_or(200.0) as f32
}

impl EnderDragon {
    /// A new dragon at `position`, holding the pattern around `origin`;
    /// `id` starts the block of nine ids it and its parts use.
    pub fn spawned(id: i32, uuid: u128, position: [f64; 3], yaw: f32, origin: BlockPos) -> Self {
        let mut base = EntityBase::new(EntityType::EnderDragon, id, uuid, position, [0.0; 3]);
        base.yaw = yaw;
        let mut dragon = Self::with_base(base);
        dragon.origin = origin;
        dragon.ids_reserved = true;
        dragon.phase = PhaseKind::HoldingPattern;
        dragon
    }

    fn with_base(base: EntityBase) -> Self {
        let position = base.position;
        Self {
            base,
            health: max_health(),
            absorption: 0.0,
            last_hurt: 0.0,
            hurt_time: 0,
            death_time: 0,
            sitting_damage: 0.0,
            phase: PhaseKind::Hover,
            phases: Phases::default(),
            history: FlightHistory::default(),
            parts: [position; PART_COUNT],
            yaw_speed: 0.0,
            in_wall: false,
            graph: None,
            nearest_crystal: None,
            origin: super::fight::ORIGIN,
            ticks: 0,
            ambient_sound_time: 0,
            seen_destroyed: None,
            pending: Vec::new(),
            ids_reserved: false,
            last_report: None,
            changed: Vec::new(),
            impulse: false,
            killer: None,
        }
    }

    pub fn health(&self) -> f32 {
        self.health
    }

    pub fn phase(&self) -> PhaseKind {
        self.phase
    }

    pub fn part_position(&self, part: usize) -> [f64; 3] {
        self.parts[part]
    }

    fn set_health(&mut self, health: f32) {
        let health = health.clamp(0.0, max_health());
        if health != self.health {
            self.health = health;
            self.changed.push(DATA_HEALTH);
        }
    }

    fn is_dead(&self) -> bool {
        self.health <= 0.0
    }

    pub fn set_phase(&mut self, phase: PhaseKind, cx: &mut TickContext) {
        if phase == self.phase {
            return;
        }
        if self.phase == PhaseKind::SittingFlaming
            && let Some(flame) = self.phases.flame.take()
        {
            cx.effects.push(Effect::Notify {
                target: flame,
                notice: Notice::Discard,
            });
        }
        self.phase = phase;
        self.changed.push(DATA_PHASE);
        let p = &mut self.phases;
        match phase {
            PhaseKind::HoldingPattern => p.holding = Pathing::default(),
            PhaseKind::StrafePlayer => {
                p.fireball_charge = 0;
                p.strafe = Pathing::default();
                p.strafe_target = None;
            }
            PhaseKind::LandingApproach => p.approach = Pathing::default(),
            PhaseKind::Landing => p.landing = None,
            PhaseKind::Takeoff => {
                p.takeoff_first_tick = true;
                p.takeoff = Pathing::default();
            }
            PhaseKind::SittingFlaming => {
                p.flame_ticks = 0;
                p.flame_count += 1;
            }
            PhaseKind::SittingScanning => p.scanning_time = 0,
            PhaseKind::SittingAttacking => p.attacking_ticks = 0,
            PhaseKind::ChargingPlayer => {
                p.charge = None;
                p.time_since_charge = 0;
            }
            PhaseKind::Dying => {
                p.dying = None;
                p.dying_time = 0;
            }
            PhaseKind::Hover => p.hover = None,
        }
    }

    fn fly_target(&self) -> Option<[f64; 3]> {
        let p = &self.phases;
        match self.phase {
            PhaseKind::HoldingPattern => p.holding.target,
            PhaseKind::StrafePlayer => p.strafe.target,
            PhaseKind::LandingApproach => p.approach.target,
            PhaseKind::Landing => p.landing,
            PhaseKind::Takeoff => p.takeoff.target,
            PhaseKind::ChargingPlayer => p.charge,
            PhaseKind::Dying => p.dying,
            PhaseKind::Hover => p.hover,
            PhaseKind::SittingFlaming
            | PhaseKind::SittingScanning
            | PhaseKind::SittingAttacking => None,
        }
    }

    fn horizontal_speed(&self) -> f32 {
        let [vx, _, vz] = self.base.velocity;
        (vx * vx + vz * vz).sqrt() as f32
    }

    fn turn_speed(&self) -> f32 {
        let speed = self.horizontal_speed() + 1.0;
        let distance = speed.min(40.0);
        if self.phase == PhaseKind::Landing {
            distance / speed
        } else {
            0.7 / distance / speed
        }
    }

    fn view(cx: &TickContext) -> Option<std::sync::Arc<FightView>> {
        cx.shared.fight(cx.dimension).map(|f| f.view())
    }

    fn crystals_alive(cx: &TickContext) -> bool {
        Self::view(cx).is_some_and(|v| v.alive_crystals > 0)
    }

    /// One above the highest motion-blocking block in a column, from what
    /// the region sees.
    fn ground(x: i32, z: i32, cx: &TickContext) -> i32 {
        for y in (cx.min_y..=cx.max_y).rev() {
            if (cx.block)(BlockPos::new(x, y, z)).is_some_and(blocks_motion) {
                return y + 1;
            }
        }
        cx.min_y
    }

    /// The top of the exit portal's pillar, where the dragon perches.
    fn podium_top(&self, cx: &TickContext) -> BlockPos {
        let y = Self::ground(self.origin.x, self.origin.z, cx);
        BlockPos::new(self.origin.x, y, self.origin.z)
    }

    fn graph(&mut self, cx: &TickContext) -> &FlightGraph {
        if self.graph.is_none() {
            let view = Self::view(cx);
            let graph = FlightGraph::new(|x, z| {
                view.as_ref()
                    .and_then(|v| v.ground.iter().find(|g| g.0 == x && g.1 == z))
                    .map_or_else(|| Self::ground(x, z, cx), |g| g.2)
            });
            self.graph = Some(graph);
        }
        self.graph.as_ref().expect("built above")
    }

    fn closest_node(&mut self, position: [f64; 3], cx: &TickContext) -> usize {
        let crystals = Self::crystals_alive(cx);
        self.graph(cx).closest(position, crystals)
    }

    fn find_path(
        &mut self,
        from: usize,
        to: usize,
        last: Option<Node>,
        cx: &TickContext,
    ) -> Option<Path> {
        let crystals = Self::crystals_alive(cx);
        self.graph(cx).find_path(from, to, last, crystals)
    }

    /// A player who can be fought (not creative or invulnerable), nearest
    /// to `at`, within `range` and in sight of the dragon if asked.
    fn nearest_player<'a>(
        &self,
        at: [f64; 3],
        range: Option<f64>,
        sight: bool,
        extra: impl Fn(&NearbyPlayer) -> bool,
        cx: &'a TickContext,
    ) -> Option<&'a NearbyPlayer<'a>> {
        cx.players
            .iter()
            .filter(|p| !p.invulnerable && !p.creative)
            .filter(|p| range.is_none_or(|r| distance_sq(p.position, self.base.position) <= r * r))
            .filter(|p| extra(p))
            .filter(|p| !sight || self.can_see(p.eye(), cx))
            .min_by(|a, b| distance_sq(a.position, at).total_cmp(&distance_sq(b.position, at)))
    }

    fn eye(&self) -> [f64; 3] {
        let [x, y, z] = self.base.position;
        [
            x,
            y + f64::from(EntityType::EnderDragon.size().eye_height),
            z,
        ]
    }

    fn can_see(&self, to: [f64; 3], cx: &TickContext) -> bool {
        let level = |p: BlockPos| (cx.block)(p).unwrap_or(Block::Air.default_state());
        tuff_core::world::raycast::cast_colliders(self.eye(), to, &level).is_none()
    }

    /// Picks the next node of `path` as a target, somewhere up to 20
    /// blocks above it.
    fn next_target(path: &mut Option<Path>, rng: &mut impl Rng) -> Option<[f64; 3]> {
        let path = path.as_mut()?;
        if path.is_done() {
            return None;
        }
        let node = path.next_node();
        path.advance();
        let y = f64::from(node.y) + f64::from(rng.r#gen::<f32>() * 20.0);
        Some([f64::from(node.x), y, f64::from(node.z)])
    }

    /// Picks the next node along the inner (crystals up) or outer ring,
    /// sometimes turning round and cutting across. Returns the node and
    /// the new direction.
    fn circuit_target(
        from: usize,
        clockwise: bool,
        inner_ring: bool,
        rng: &mut impl Rng,
    ) -> (usize, bool) {
        let turn_round = rng.gen_range(0..8) == 0;
        let clockwise = clockwise != turn_round;
        let step = if clockwise { 1 } else { -1 } + if turn_round { 6 } else { 0 };
        let (first, len) = if inner_ring { (0, 12) } else { (12, 8) };
        let along = (from as i32 + step - first).rem_euclid(len);
        ((first + along) as usize, clockwise)
    }

    /// Plans a fresh path to the next circuit node, skipping the node the
    /// dragon is already at.
    fn plan_circuit(
        &mut self,
        clockwise: bool,
        inner_ring: bool,
        cx: &mut TickContext,
    ) -> (Option<Path>, bool) {
        let from = self.closest_node(self.base.position, cx);
        let (to, clockwise) = Self::circuit_target(from, clockwise, inner_ring, cx.rng);
        let mut path = self.find_path(from, to, None, cx);
        path.iter_mut().for_each(Path::advance);
        (path, clockwise)
    }

    fn phase_tick(&mut self, cx: &mut TickContext) {
        match self.phase {
            PhaseKind::HoldingPattern => self.holding_tick(cx),
            PhaseKind::StrafePlayer => self.strafe_tick(cx),
            PhaseKind::LandingApproach => self.approach_tick(cx),
            PhaseKind::Landing => self.landing_tick(cx),
            PhaseKind::Takeoff => self.takeoff_tick(cx),
            PhaseKind::SittingFlaming => self.flaming_tick(cx),
            PhaseKind::SittingScanning => self.scanning_tick(cx),
            PhaseKind::SittingAttacking => {
                let ticks = self.phases.attacking_ticks;
                self.phases.attacking_ticks += 1;
                if ticks >= 40 {
                    self.set_phase(PhaseKind::SittingFlaming, cx);
                }
            }
            PhaseKind::ChargingPlayer => self.charge_tick(cx),
            PhaseKind::Dying => self.dying_tick(cx),
            PhaseKind::Hover => {
                if self.phases.hover.is_none() {
                    self.phases.hover = Some(self.base.position);
                }
            }
        }
    }

    fn target_far(&self, target: Option<[f64; 3]>) -> bool {
        let distance = target.map_or(0.0, |t| distance_sq(t, self.base.position));
        distance < 100.0 || distance > 22500.0
    }

    /// At the end of a lap the dragon may go and land, or break off to
    /// chase the player nearest the podium. True when it left the circuit.
    fn leave_circuit(&mut self, cx: &mut TickContext) -> bool {
        let podium = self.podium_top(cx);
        let alive = Self::view(cx).map_or(0, |v| v.alive_crystals);
        if cx.rng.gen_range(0..alive + 3) == 0 {
            self.set_phase(PhaseKind::LandingApproach, cx);
            return true;
        }
        let corner = [
            f64::from(podium.x),
            f64::from(podium.y),
            f64::from(podium.z),
        ];
        let Some((id, at)) = self
            .nearest_player(corner, None, false, |_| true, cx)
            .map(|p| (p.id, p.position))
        else {
            return false;
        };
        let center = corner.map(|c| c + 0.5);
        let odds = (distance_sq(center, at) / 512.0 + 2.0) as i32;
        let chase = cx.rng.gen_range(0..odds) == 0 || cx.rng.gen_range(0..alive + 2) == 0;
        if chase {
            self.strafe_player(id, at, cx);
        }
        chase
    }

    fn holding_tick(&mut self, cx: &mut TickContext) {
        if !self.target_far(self.phases.holding.target) {
            return;
        }
        let lap_done = self.phases.holding.path.as_ref().is_some_and(Path::is_done);
        if lap_done && self.leave_circuit(cx) {
            return;
        }
        if lap_done || self.phases.holding.path.is_none() {
            let inner = Self::view(cx).is_some_and(|v| v.alive_crystals >= 0);
            let (path, clockwise) = self.plan_circuit(self.phases.clockwise, inner, cx);
            self.phases.clockwise = clockwise;
            self.phases.holding.path = path;
        }
        if let Some(target) = Self::next_target(&mut self.phases.holding.path, cx.rng) {
            self.phases.holding.target = Some(target);
        }
    }

    fn strafe_player(&mut self, player: i32, position: [f64; 3], cx: &mut TickContext) {
        self.set_phase(PhaseKind::StrafePlayer, cx);
        self.phases.strafe_target = Some(player);
        let current = self.closest_node(self.base.position, cx);
        let target = self.closest_node(position, cx);
        let (x, z) = (position[0].floor(), position[2].floor());
        let (dx, dz) = (x - self.base.position[0], z - self.base.position[2]);
        let offset = (0.4000000059604645 + (dx * dx + dz * dz).sqrt() / 80.0 - 1.0).min(10.0);
        let last = Node::new(x as i32, (position[1] + offset).floor() as i32, z as i32);
        let mut path = self.find_path(current, target, Some(last), cx);
        if let Some(path) = &mut path {
            path.advance();
        }
        self.phases.strafe.path = path;
        if self.phases.strafe.path.is_some()
            && let Some(target) = Self::next_target(&mut self.phases.strafe.path, cx.rng)
        {
            self.phases.strafe.target = Some(target);
        }
    }

    fn strafe_tick(&mut self, cx: &mut TickContext) {
        let target = self
            .phases
            .strafe_target
            .and_then(|id| cx.players.iter().find(|p| p.id == id))
            .map(|p| (p.id, p.position, p.eye(), p.height));
        let Some((player, position, eye, height)) = target else {
            self.set_phase(PhaseKind::HoldingPattern, cx);
            return;
        };
        if self.phases.strafe.path.as_ref().is_some_and(Path::is_done) {
            let (dx, dz) = (
                position[0] - self.base.position[0],
                position[2] - self.base.position[2],
            );
            let offset = (0.4000000059604645 + (dx * dx + dz * dz).sqrt() / 80.0 - 1.0).min(10.0);
            self.phases.strafe.target = Some([position[0], position[1] + offset, position[2]]);
        }
        if self.target_far(self.phases.strafe.target) {
            if self.phases.strafe.path.as_ref().is_none_or(Path::is_done) {
                let inner = Self::crystals_alive(cx);
                let (path, clockwise) = self.plan_circuit(self.phases.strafe_clockwise, inner, cx);
                self.phases.strafe_clockwise = clockwise;
                self.phases.strafe.path = path;
            }
            if let Some(target) = Self::next_target(&mut self.phases.strafe.path, cx.rng) {
                self.phases.strafe.target = Some(target);
            }
        }
        if distance_sq(position, self.base.position) >= 4096.0 {
            self.phases.fireball_charge = (self.phases.fireball_charge - 1).max(0);
            return;
        }
        if !self.can_see(eye, cx) {
            self.phases.fireball_charge = (self.phases.fireball_charge - 1).max(0);
            return;
        }
        self.phases.fireball_charge += 1;
        let aim = normalize([
            position[0] - self.base.position[0],
            0.0,
            position[2] - self.base.position[2],
        ]);
        let yaw = self.base.yaw * DEG;
        let facing = normalize([
            f64::from(mth::sin(f64::from(yaw))),
            0.0,
            f64::from(-mth::cos(f64::from(yaw))),
        ]);
        let angle = (dot(facing, aim) as f32).acos().to_degrees() + 0.5;
        if self.phases.fireball_charge >= 5 && (0.0..10.0).contains(&angle) {
            let view = tuff_core::world::raycast::view_vector(self.base.yaw, self.base.pitch);
            let head = self.parts[HEAD];
            let (_, head_height) = part_size(HEAD);
            let start = [
                head[0] - view[0],
                head[1] + head_height * 0.5 + 0.5,
                head[2] - view[2],
            ];
            let direction = [
                position[0] - start[0],
                position[1] + height * 0.5 - start[1],
                position[2] - start[2],
            ];
            if !self.base.silent {
                let event = framed(&play::LevelEvent {
                    event: play::level_event::SOUND_DRAGON_FIREBALL,
                    position: self.block_pos(),
                    data: 0,
                    global: false,
                });
                cx.effects.push(Effect::ChunkPackets(packets([event])));
            }
            let id = cx.shared.next_entity_id();
            let fireball = super::fireball::DragonFireball::new(
                id,
                start,
                direction,
                Culprit::entity(&self.base),
                cx.rng,
            );
            cx.effects.push(Effect::Spawn(Box::new(fireball)));
            self.phases.fireball_charge = 0;
            if let Some(path) = &mut self.phases.strafe.path {
                path.finish();
            }
            let _ = player;
            self.set_phase(PhaseKind::HoldingPattern, cx);
        }
    }

    fn approach_tick(&mut self, cx: &mut TickContext) {
        if !self.target_far(self.phases.approach.target) {
            return;
        }
        if self.phases.approach.path.as_ref().is_none_or(Path::is_done) {
            let current = self.closest_node(self.base.position, cx);
            let egg = self.podium_top(cx);
            let egg_at = [f64::from(egg.x), f64::from(egg.y), f64::from(egg.z)];
            let target = match self.nearest_player(egg_at, None, false, |_| true, cx) {
                Some(player) => {
                    let aim = normalize([player.position[0], 0.0, player.position[2]]);
                    self.closest_node([-aim[0] * 40.0, 105.0, -aim[2] * 40.0], cx)
                }
                None => self.closest_node([40.0, f64::from(egg.y), 0.0], cx),
            };
            let last = Node::new(egg.x, egg.y, egg.z);
            let mut path = self.find_path(current, target, Some(last), cx);
            if let Some(path) = &mut path {
                path.advance();
            }
            self.phases.approach.path = path;
        }
        if let Some(target) = Self::next_target(&mut self.phases.approach.path, cx.rng) {
            self.phases.approach.target = Some(target);
        }
        if self
            .phases
            .approach
            .path
            .as_ref()
            .is_some_and(Path::is_done)
        {
            self.set_phase(PhaseKind::Landing, cx);
        }
    }

    fn landing_tick(&mut self, cx: &mut TickContext) {
        let target = match self.phases.landing {
            Some(target) => target,
            None => {
                let top = self.podium_top(cx);
                let target = [
                    f64::from(top.x) + 0.5,
                    f64::from(top.y),
                    f64::from(top.z) + 0.5,
                ];
                self.phases.landing = Some(target);
                target
            }
        };
        if distance_sq(target, self.base.position) < 1.0 {
            self.phases.flame_count = 0;
            self.set_phase(PhaseKind::SittingScanning, cx);
        }
    }

    fn takeoff_tick(&mut self, cx: &mut TickContext) {
        if self.phases.takeoff_first_tick || self.phases.takeoff.path.is_none() {
            self.phases.takeoff_first_tick = false;
            let current = self.closest_node(self.base.position, cx);
            let look = self.head_look(cx);
            let toward = self.closest_node([-look[0] * 40.0, 105.0, -look[2] * 40.0], cx);
            let target = if Self::crystals_alive(cx) {
                (toward as i32).rem_euclid(12) as usize
            } else {
                (((toward as i32 - 12) & 7) + 12) as usize
            };
            self.phases.takeoff.path = self.find_path(current, target, None, cx);
            if let Some(path) = &mut self.phases.takeoff.path {
                path.advance();
            }
            if let Some(target) = Self::next_target(&mut self.phases.takeoff.path, cx.rng) {
                self.phases.takeoff.target = Some(target);
            }
            return;
        }
        let egg = self.podium_top(cx);
        let center = [
            f64::from(egg.x) + 0.5,
            f64::from(egg.y) + 0.5,
            f64::from(egg.z) + 0.5,
        ];
        if distance_sq(center, self.base.position) >= 100.0 {
            self.set_phase(PhaseKind::HoldingPattern, cx);
        }
    }

    /// Where the head points: down toward the portal while landing or
    /// taking off, down 45 degrees while perched.
    fn head_look(&self, cx: &TickContext) -> [f64; 3] {
        let pitch = match self.phase {
            PhaseKind::Landing | PhaseKind::Takeoff => {
                let egg = self.podium_top(cx);
                let center = [
                    f64::from(egg.x) + 0.5,
                    f64::from(egg.y) + 0.5,
                    f64::from(egg.z) + 0.5,
                ];
                let distance =
                    ((distance_sq(center, self.base.position).sqrt() as f32) / 4.0).max(1.0);
                -(6.0 / distance) * 1.5 * 5.0
            }
            phase if phase.is_sitting() => -45.0,
            _ => self.base.pitch,
        };
        tuff_core::world::raycast::view_vector(self.base.yaw, pitch)
    }

    fn flaming_tick(&mut self, cx: &mut TickContext) {
        self.phases.flame_ticks += 1;
        if self.phases.flame_ticks >= 200 {
            if self.phases.flame_count >= 4 {
                self.set_phase(PhaseKind::Takeoff, cx);
            } else {
                self.set_phase(PhaseKind::SittingScanning, cx);
            }
            return;
        }
        if self.phases.flame_ticks != 10 {
            return;
        }
        let head = self.parts[HEAD];
        let look = normalize([
            head[0] - self.base.position[0],
            0.0,
            head[2] - self.base.position[2],
        ]);
        let x = head[0] + look[0] * 5.0 / 2.0;
        let z = head[2] + look[2] * 5.0 / 2.0;
        let (_, head_height) = part_size(HEAD);
        let initial = head[1] + head_height * 0.5;
        let mut y = initial;
        loop {
            let pos = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
            if !(cx.block)(pos).is_some_and(tuff_world_model::chunk::is_air) {
                break;
            }
            y -= 1.0;
            if y < 0.0 {
                y = initial;
                break;
            }
        }
        let id = cx.shared.next_entity_id();
        let cloud = tuff_vanilla_common::effect_cloud::AreaEffectCloud::flame(
            id,
            [x, y.floor() + 1.0, z],
            Culprit::entity(&self.base),
            cx.rng,
        );
        self.phases.flame = Some(id);
        cx.effects.push(Effect::Spawn(Box::new(cloud)));
    }

    fn scanning_tick(&mut self, cx: &mut TickContext) {
        self.phases.scanning_time += 1;
        let elapsed = self.phases.scanning_time;
        let level = self.base.position[1];
        let watched = self
            .nearest_player(
                self.base.position,
                Some(20.0),
                true,
                |p| (p.position[1] - level).abs() <= 10.0,
                cx,
            )
            .map(|p| p.position);
        match watched {
            Some(_) if elapsed > 25 => self.set_phase(PhaseKind::SittingAttacking, cx),
            Some(spot) => {
                if !(0.0..=10.0).contains(&self.off_bearing_degrees(spot)) {
                    self.steer_head_toward(spot);
                }
            }
            None if elapsed >= 100 => self.leave_perch_after_scanning(cx),
            None => {}
        }
    }

    fn off_bearing_degrees(&self, spot: [f64; 3]) -> f32 {
        let [px, _, pz] = self.base.position;
        let toward = normalize([spot[0] - px, 0.0, spot[2] - pz]);
        let heading = f64::from(self.base.yaw * DEG);
        let ahead = normalize([
            f64::from(mth::sin(heading)),
            0.0,
            f64::from(-mth::cos(heading)),
        ]);
        (dot(ahead, toward) as f32).acos().to_degrees() + 0.5
    }

    fn steer_head_toward(&mut self, spot: [f64; 3]) {
        let [hx, _, hz] = self.parts[HEAD];
        let (east, south) = (spot[0] - hx, spot[2] - hz);
        let bearing = 180.0 - east.atan2(south).to_degrees() - f64::from(self.base.yaw);
        let error = f64::from(wrap_degrees(bearing as f32)).clamp(-100.0, 100.0) as f32;
        let reach = (1.0 + (east * east + south * south).sqrt() as f32).min(40.0);
        self.yaw_speed = self.yaw_speed * 0.8 + error * (0.7 / reach / reach);
        self.base.yaw += self.yaw_speed;
    }

    fn leave_perch_after_scanning(&mut self, cx: &mut TickContext) {
        let quarry = self
            .nearest_player(self.base.position, Some(150.0), true, |_| true, cx)
            .map(|p| p.position);
        self.set_phase(PhaseKind::Takeoff, cx);
        if let Some(quarry) = quarry {
            self.set_phase(PhaseKind::ChargingPlayer, cx);
            self.phases.charge = Some(quarry);
        }
    }

    fn charge_tick(&mut self, cx: &mut TickContext) {
        let Some(target) = self.phases.charge else {
            self.set_phase(PhaseKind::HoldingPattern, cx);
            return;
        };
        if self.phases.time_since_charge > 0 {
            let time = self.phases.time_since_charge;
            self.phases.time_since_charge += 1;
            if time >= 10 {
                self.set_phase(PhaseKind::HoldingPattern, cx);
                return;
            }
        }
        if self.target_far(Some(target)) {
            self.phases.time_since_charge += 1;
        }
    }

    fn dying_tick(&mut self, cx: &mut TickContext) {
        self.phases.dying_time += 1;
        let target = match self.phases.dying {
            Some(target) => target,
            None => {
                let top = self.podium_top(cx);
                let target = [
                    f64::from(top.x) + 0.5,
                    f64::from(top.y),
                    f64::from(top.z) + 0.5,
                ];
                self.phases.dying = Some(target);
                target
            }
        };
        if self.target_far(Some(target)) {
            self.set_health(0.0);
        } else {
            self.set_health(1.0);
        }
    }

    fn block_pos(&self) -> BlockPos {
        let [x, y, z] = self.base.position;
        BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    }

    fn sound(&self, sound: SoundEvent, cx: &mut TickContext) {
        if self.base.silent {
            return;
        }
        let pitch = 1.0 + (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2;
        let packet = framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::HOSTILE,
            position: self.base.position,
            volume: SOUND_VOLUME,
            pitch,
            seed: cx.rng.r#gen(),
        });
        cx.effects.push(Effect::ChunkPackets(packets([packet])));
    }

    /// Heals from the nearest crystal within 32 blocks of its box, looking
    /// for one every tenth tick or so.
    fn check_crystals(&mut self, cx: &mut TickContext) {
        let view = Self::view(cx);
        let alive = |uuid: u128| {
            cx.entities
                .iter()
                .any(|e| e.uuid == uuid && e.kind == EntityType::EndCrystal)
                || view
                    .as_ref()
                    .is_some_and(|v| v.crystals.iter().any(|(u, _)| *u == uuid))
        };
        if let Some((uuid, _)) = self.nearest_crystal {
            if !alive(uuid) {
                self.nearest_crystal = None;
            } else if self.ticks.is_multiple_of(10) && self.health < max_health() {
                self.set_health(self.health + 1.0);
            }
        }
        if cx.rng.gen_range(0..10) != 0 {
            return;
        }
        let area = inflate(self.base.bounds(), 32.0, 32.0, 32.0);
        let mut found: Vec<(u128, [f64; 3])> = cx
            .entities
            .iter()
            .filter(|e| e.kind == EntityType::EndCrystal && intersects(e.bounds, area))
            .map(|e| (e.uuid, e.position))
            .collect();
        if let Some(view) = &view {
            let (width, height) = {
                let size = EntityType::EndCrystal.size();
                (f64::from(size.width) / 2.0, f64::from(size.height))
            };
            for (uuid, [x, y, z]) in &view.crystals {
                let bounds = [x - width, *y, z - width, x + width, y + height, z + width];
                if intersects(bounds, area) && !found.iter().any(|(u, _)| u == uuid) {
                    found.push((*uuid, [*x, *y, *z]));
                }
            }
        }
        self.nearest_crystal = found.into_iter().min_by(|a, b| {
            distance_sq(a.1, self.base.position).total_cmp(&distance_sq(b.1, self.base.position))
        });
    }

    /// Crystals the fight reported destroyed since the last look: losing
    /// the one it heals from hurts its head, and it may go for the player.
    fn check_destroyed(&mut self, cx: &mut TickContext) {
        let Some(view) = Self::view(cx) else {
            return;
        };
        let latest = view.destroyed.last().map_or(0, |d| d.seq);
        let Some(seen) = self.seen_destroyed.replace(latest) else {
            return;
        };
        for destroyed in view.destroyed.iter().filter(|d| d.seq > seen) {
            let pos = [
                f64::from(destroyed.pos.x),
                f64::from(destroyed.pos.y),
                f64::from(destroyed.pos.z),
            ];
            let player = match destroyed.player {
                Some((id, _)) => cx.players.iter().find(|p| p.id == id),
                None => cx
                    .players
                    .iter()
                    .filter(|p| !p.creative && distance_sq(p.position, pos) <= 64.0 * 64.0)
                    .min_by(|a, b| {
                        distance_sq(a.position, pos).total_cmp(&distance_sq(b.position, pos))
                    }),
            }
            .map(|p| (p.id, p.position, p.invulnerable || p.creative));
            if self
                .nearest_crystal
                .is_some_and(|(uuid, _)| uuid == destroyed.uuid)
            {
                let kind = if player.is_some() {
                    &damage_type::PLAYER_EXPLOSION
                } else {
                    &damage_type::EXPLOSION
                };
                let attacker = player.map(|(id, _, _)| id);
                self.hurt_part(HEAD, CRYSTAL_DAMAGE, kind, attacker, None, cx);
            }
            if self.phase == PhaseKind::HoldingPattern
                && let Some((id, position, protected)) = player
                && !protected
            {
                self.strafe_player(id, position, cx);
            }
        }
    }

    /// The game's damage path for a hit on `part`: the phase may ignore
    /// it, parts other than the head and neck take a quarter plus one, and
    /// only players and explosion-like damage hurt the dragon.
    fn hurt_part(
        &mut self,
        part: usize,
        amount: f32,
        kind: &'static DamageType,
        attacker: Option<PlayerId>,
        from: Option<[f64; 3]>,
        cx: &mut TickContext,
    ) -> Option<f32> {
        if self.phase == PhaseKind::Dying || self.is_dead() {
            return None;
        }
        let mut damage = amount;
        if part != HEAD && part != NECK {
            damage = damage / 4.0 + damage.min(1.0);
        }
        if damage < 0.01 {
            return None;
        }
        if attacker.is_none()
            && !tuff_core::world::damage_type_in_tag(kind, "minecraft:always_hurts_ender_dragons")
        {
            return Some(0.0);
        }
        self.killer = attacker;
        let before = self.health;
        self.really_hurt(damage, kind, from, cx);
        let lost = before - self.health;
        if self.phase.is_sitting() {
            self.sitting_damage += lost;
            if self.sitting_damage > SITTING_ALLOWED_DAMAGE * max_health() {
                self.sitting_damage = 0.0;
                self.set_phase(PhaseKind::Takeoff, cx);
            }
        }
        Some(lost)
    }

    fn soak(&mut self, amount: f32, cx: &mut TickContext) -> f32 {
        let absorbed = amount.min(self.absorption).max(0.0);
        self.absorption -= absorbed;
        if let Some(player) = self.killer
            && let Some(tenths) = tuff_vanilla_common::combat::stat_tenths(absorbed)
        {
            cx.effects.push(Effect::TellPlayer {
                player,
                event: Box::new(tuff_core::world::messages::PlayerEvent::AwardStat(
                    tuff_vanilla_data::registry::CustomStat::DamageDealtAbsorbed,
                    tenths,
                )),
            });
        }
        amount - absorbed
    }

    fn really_hurt(
        &mut self,
        amount: f32,
        kind: &'static DamageType,
        from: Option<[f64; 3]>,
        cx: &mut TickContext,
    ) {
        let full = if self.base.invulnerable_time > 10
            && !tuff_core::world::damage_type_in_tag(kind, "minecraft:bypasses_cooldown")
        {
            if amount <= self.last_hurt {
                return;
            }
            let extra = amount - self.last_hurt;
            self.last_hurt = amount;
            let extra = self.soak(extra, cx);
            self.set_health(self.health - extra);
            false
        } else {
            self.last_hurt = amount;
            self.base.invulnerable_time = DAMAGE_COOLDOWN;
            let amount = self.soak(amount, cx);
            self.set_health(self.health - amount);
            self.hurt_time = HURT_DURATION;
            true
        };
        if full {
            cx.effects.push(Effect::EntityPackets(packets([framed(
                &play::DamageEvent {
                    entity_id: self.base.id,
                    damage_type: tuff_core::world::damage_type_id(kind),
                    cause: None,
                    direct: None,
                    source_position: from,
                },
            )])));
            if !self.phase.is_sitting()
                && let Some(from) = from
                && !tuff_core::world::damage_type_in_tag(kind, "minecraft:no_knockback")
            {
                let direction = normalize([
                    from[0] - self.base.position[0],
                    0.0,
                    from[2] - self.base.position[2],
                ]);
                let v = self.base.velocity;
                self.base.velocity = [
                    v[0] / 2.0 - direction[0] * 0.4,
                    v[1],
                    v[2] / 2.0 - direction[2] * 0.4,
                ];
                self.impulse = true;
            }
            if self.is_dead() {
                self.sound(SoundEvent::EntityGenericDeath, cx);
            } else {
                self.ambient_sound_time = -AMBIENT_INTERVAL;
                self.sound(SoundEvent::EntityEnderDragonHurt, cx);
            }
        }
        if self.is_dead() && !self.phase.is_sitting() {
            self.set_health(1.0);
            self.set_phase(PhaseKind::Dying, cx);
            if let Some(player) = self.killer {
                cx.effects.push(Effect::KillCredit {
                    player,
                    kill: Box::new(self.kill_credit(kind)),
                });
            }
        }
    }

    /// The dragon's death as the killer's statistics and advancements see
    /// it: a player dealt the killing blow of `kind`.
    fn kill_credit(&self, kind: &'static DamageType) -> tuff_core::world::messages::Kill {
        use tuff_core::world::predicate::{DamageFacts, EntityFacts};
        let player = EntityFacts {
            kind: Some(EntityType::Player),
            ..EntityFacts::default()
        };
        tuff_core::world::messages::Kill {
            kind: EntityType::EnderDragon,
            holder: tuff_storage::admin::format_uuid(self.base.uuid),
            victim: EntityFacts {
                kind: Some(EntityType::EnderDragon),
                position: self.base.position,
                ..EntityFacts::default()
            },
            killing_blow: DamageFacts {
                kind,
                source: Some(player.clone()),
                direct: Some(player),
            },
        }
    }

    /// Pushes and hurts what the wings and head sweep through.
    fn sweep(&mut self, cx: &mut TickContext) {
        let body = part_box(self.parts[BODY], BODY);
        let (xm, zm) = ((body[0] + body[3]) / 2.0, (body[2] + body[5]) / 2.0);
        let sitting = self.phase.is_sitting();
        let culprit = Culprit::entity(&self.base);
        let mut pushed: Vec<i32> = Vec::new();
        for wing in [WING_LEFT, WING_RIGHT] {
            let area = inflate(part_box(self.parts[wing], wing), 4.0, 2.0, 4.0);
            let area = [
                area[0],
                area[1] - 2.0,
                area[2],
                area[3],
                area[4] - 2.0,
                area[5],
            ];
            for (id, position) in self.victims(area, cx) {
                let (xd, zd) = (position[0] - xm, position[2] - zm);
                let dd = (xd * xd + zd * zd).max(0.1);
                let push = [xd / dd * 4.0, 0.20000000298023224, zd / dd * 4.0];
                if !sitting && !pushed.contains(&id) {
                    pushed.push(id);
                    // The push rides along with the hit rather than as a
                    // separate effect: both would otherwise reach the
                    // target as their own absolute SetEntityMotion in the
                    // same tick, and whichever the region sends second
                    // would silently wipe out the other's push.
                    let mut hit = EntityHit::new(WING_DAMAGE, &damage_type::MOB_ATTACK);
                    hit.attacker = Some(culprit.clone());
                    hit.position = Some(self.base.position);
                    hit.push = Some(push);
                    cx.effects.push(Effect::Hurt {
                        target: id,
                        hit: Box::new(hit),
                    });
                } else {
                    cx.effects.push(Effect::Push { target: id, push });
                }
            }
        }
        for part in [HEAD, NECK] {
            let area = inflate(part_box(self.parts[part], part), 1.0, 1.0, 1.0);
            for (id, _) in self.victims(area, cx) {
                let mut hit = EntityHit::new(HEAD_DAMAGE, &damage_type::MOB_ATTACK);
                hit.attacker = Some(culprit.clone());
                hit.position = Some(self.base.position);
                cx.effects.push(Effect::Hurt {
                    target: id,
                    hit: Box::new(hit),
                });
            }
        }
    }

    /// Players (not creative) and living entities inside `area`.
    fn victims(&self, area: [f64; 6], cx: &TickContext) -> Vec<(i32, [f64; 3])> {
        let mut found: Vec<(i32, [f64; 3])> = cx
            .players
            .iter()
            .filter(|p| !p.creative && intersects(p.bounds(), area))
            .map(|p| (p.id, p.position))
            .collect();
        found.extend(
            cx.entities
                .iter()
                .filter(|e| e.id != self.base.id && e.living.is_some_and(|l| l.alive))
                .filter(|e| intersects(e.bounds, area))
                .map(|e| (e.id, e.position)),
        );
        found
    }

    /// Breaks what the head, neck and body plough through (unless mob
    /// griefing is off or the block resists dragons); true if something
    /// held.
    fn check_walls(&self, area: [f64; 6], cx: &mut TickContext) -> bool {
        let griefing = cx.shared.rule_bool("mob_griefing");
        let low = area_corner(&area[..3]);
        let high = area_corner(&area[3..]);
        let mut held = false;
        let mut broke = false;
        for x in low[0]..=high[0] {
            for y in low[1]..=high[1] {
                for z in low[2]..=high[2] {
                    let pos = BlockPos::new(x, y, z);
                    match (cx.block)(pos).map(|state| plough_verdict(state, griefing)) {
                        Some(Plough::Holds) => held = true,
                        Some(Plough::Breaks) => {
                            broke = true;
                            cx.effects.push(Effect::SetBlock {
                                pos,
                                state: Block::Air.default_state(),
                            });
                        }
                        Some(Plough::PassesThrough) | None => {}
                    }
                }
            }
        }
        if broke {
            let mut pick = |axis: usize| low[axis] + cx.rng.gen_range(0..=high[axis] - low[axis]);
            let (x, y, z) = (pick(0), pick(1), pick(2));
            let event = framed(&play::LevelEvent {
                event: play::level_event::PARTICLES_DRAGON_BLOCK_BREAK,
                position: BlockPos::new(x, y, z),
                data: 0,
                global: false,
            });
            cx.effects.push(Effect::ChunkPackets(packets([event])));
        }
        held
    }

    fn update_parts(&mut self, cx: &mut TickContext) {
        let tilt = ((self.history.get(5).0 - self.history.get(10).0) as f32) * 10.0 * DEG;
        let (cc_tilt, ss_tilt) = (mth::cos(f64::from(tilt)), mth::sin(f64::from(tilt)));
        let rot = self.base.yaw * DEG;
        let (ss1, cc1) = (mth::sin(f64::from(rot)), mth::cos(f64::from(rot)));
        let at = |dx: f32, dy: f64, dz: f32, base: [f64; 3]| {
            [
                base[0] + f64::from(dx),
                base[1] + dy,
                base[2] + f64::from(dz),
            ]
        };
        let position = self.base.position;
        self.parts[BODY] = at(ss1 * 0.5, 0.0, -cc1 * 0.5, position);
        self.parts[WING_LEFT] = at(cc1 * 4.5, 2.0, ss1 * 4.5, position);
        self.parts[WING_RIGHT] = at(cc1 * -4.5, 2.0, ss1 * -4.5, position);
        if self.hurt_time <= 0 {
            self.sweep(cx);
        }
        let turned = rot - self.yaw_speed * 0.01;
        let (ss2, cc2) = (mth::sin(f64::from(turned)), mth::cos(f64::from(turned)));
        let y_offset = f64::from(self.head_y_offset());
        self.parts[HEAD] = at(
            ss2 * 6.5 * cc_tilt,
            y_offset + f64::from(ss_tilt * 6.5),
            -cc2 * 6.5 * cc_tilt,
            position,
        );
        self.parts[NECK] = at(
            ss2 * 5.5 * cc_tilt,
            y_offset + f64::from(ss_tilt * 5.5),
            -cc2 * 5.5 * cc_tilt,
            position,
        );
        let p1 = self.history.get(5);
        for i in 0..3 {
            let p0 = self.history.get(12 + i * 2);
            let rot = rot + wrap_degrees(p0.1 - p1.1) * DEG;
            let (ss, cc) = (mth::sin(f64::from(rot)), mth::cos(f64::from(rot)));
            let dd = (i + 1) as f32 * 2.0;
            self.parts[3 + i as usize] = at(
                -(ss1 * 1.5 + ss * dd) * cc_tilt,
                (p0.0 - p1.0) - f64::from((dd + 1.5) * ss_tilt) + 1.5,
                (cc1 * 1.5 + cc * dd) * cc_tilt,
                position,
            );
        }
        let walls =
            [HEAD, NECK, BODY].map(|part| self.check_walls(part_box(self.parts[part], part), cx));
        self.in_wall = walls.iter().any(|w| *w);
    }

    fn head_y_offset(&self) -> f32 {
        if self.phase.is_sitting() {
            return -1.0;
        }
        (self.history.get(5).0 - self.history.get(0).0) as f32
    }

    fn fly(&mut self, target: [f64; 3], speed: f32) {
        let here = self.base.position;
        let delta = [0, 1, 2].map(|axis| target[axis] - here[axis]);
        let [dx, dy, dz] = delta;
        let flat = (dx * dx + dz * dz).sqrt();
        let climb = if flat > 0.0 {
            (dy / flat).clamp(-f64::from(speed), f64::from(speed))
        } else {
            dy
        };
        self.base.velocity[1] += climb * 0.01;
        self.base.yaw = wrap_degrees(self.base.yaw);
        let heading = self.heading(self.base.velocity[1]);
        let lined_up = ((dot(heading, normalize(delta)) as f32 + 0.5) / 1.5).max(0.0);
        const EPS: f64 = 9.999999747378752e-6;
        if dx.abs() > EPS || dz.abs() > EPS {
            self.steer_towards(dx, dz);
        }
        let near = (2.0 / (delta.iter().map(|d| d * d).sum::<f64>() + 1.0)) as f32;
        self.push_forward(0.06 * (lined_up * near + (1.0 - near)));
        let damping = if self.in_wall { 0.800000011920929 } else { 1.0 };
        let velocity = self.base.velocity;
        for (position, v) in self.base.position.iter_mut().zip(velocity) {
            *position += v * damping;
        }
        let grip = 0.8 + 0.15 * (dot(normalize(velocity), heading) + 1.0) / 2.0;
        self.base.velocity = [
            velocity[0] * grip,
            velocity[1] * 0.9100000262260437,
            velocity[2] * grip,
        ];
    }

    /// The unit vector the body points along, with `rise` as its vertical
    /// part before normalising.
    fn heading(&self, rise: f64) -> [f64; 3] {
        let yaw = f64::from(self.base.yaw * DEG);
        normalize([f64::from(mth::sin(yaw)), rise, f64::from(-mth::cos(yaw))])
    }

    fn steer_towards(&mut self, dx: f64, dz: f64) {
        let bearing = 180.0 - (dx.atan2(dz) as f32) * 57.295776;
        let error = wrap_degrees(bearing - self.base.yaw).clamp(-50.0, 50.0);
        self.yaw_speed = self.yaw_speed * 0.8 + error * self.turn_speed();
        self.base.yaw += self.yaw_speed * 0.1;
    }

    fn push_forward(&mut self, thrust: f32) {
        let yaw = f64::from(self.base.yaw * DEG);
        self.base.velocity[0] += f64::from(thrust * mth::sin(yaw));
        self.base.velocity[2] += f64::from(-thrust * mth::cos(yaw));
    }

    fn ai_step(&mut self, cx: &mut TickContext) {
        self.check_crystals(cx);
        self.check_destroyed(cx);
        self.base.yaw = wrap_degrees(self.base.yaw);
        self.history.record(self.base.position[1], self.base.yaw);
        let before = self.phase;
        self.phase_tick(cx);
        if self.phase != before {
            self.phase_tick(cx);
        }
        if let Some(target) = self.fly_target() {
            self.fly(target, self.phase.fly_speed());
        }
        self.update_parts(cx);
    }

    fn report(&mut self, cx: &TickContext) {
        let Some(fight) = cx.shared.fight(cx.dimension) else {
            return;
        };
        let due = self
            .last_report
            .is_none_or(|(health, tick)| health != self.health || tick + 20 <= self.ticks);
        if !due {
            return;
        }
        self.last_report = Some((self.health, self.ticks));
        fight.send(FightMessage::Dragon(DragonReport {
            uuid: self.base.uuid,
            health: self.health,
            max_health: max_health(),
            position: self.base.position,
            name: self
                .base
                .custom_name
                .as_ref()
                .and_then(tuff_item::text::TextComponent::from_nbt),
        }));
    }

    fn tick_death(&mut self, cx: &mut TickContext) -> Tick {
        self.last_report = None;
        self.report(cx);
        self.death_time += 1;
        let first = Self::view(cx).is_some_and(|v| !v.previously_killed);
        let experience = if first {
            FIRST_KILL_EXPERIENCE
        } else {
            EXPERIENCE
        };
        let drops = cx.shared.rule_bool("mob_drops");
        if self.death_time > 150 && self.death_time % 5 == 0 && drops {
            cx.effects.push(Effect::Experience {
                position: self.base.position,
                amount: (experience as f32 * 0.08).floor() as i32,
            });
        }
        if self.death_time == 1 && !self.base.silent {
            if cx.shared.rule_bool("global_sound_events") {
                let _ = cx.shared.coordinator.try_send(
                    tuff_core::world::messages::CoordinatorMessage::GlobalLevelEvent {
                        dimension: cx.dimension,
                        event: play::level_event::SOUND_DRAGON_DEATH,
                        pos: self.block_pos(),
                        data: 0,
                    },
                );
            } else {
                let event = framed(&play::LevelEvent {
                    event: play::level_event::SOUND_DRAGON_DEATH,
                    position: self.block_pos(),
                    data: 0,
                    global: false,
                });
                cx.effects.push(Effect::ChunkPackets(packets([event])));
            }
        }
        self.base.position[1] += 0.10000000149011612;
        for part in &mut self.parts {
            part[1] += 0.10000000149011612;
        }
        if self.death_time < DEATH_TICKS {
            return Tick::Alive;
        }
        if drops {
            cx.effects.push(Effect::Experience {
                position: self.base.position,
                amount: (experience as f32 * 0.2).floor() as i32,
            });
        }
        if let Some(fight) = cx.shared.fight(cx.dimension) {
            fight.send(FightMessage::DragonKilled {
                uuid: self.base.uuid,
            });
        }
        Tick::Discard
    }

    fn value(&self, index: u8) -> Option<MetadataValue> {
        Some(match index {
            DATA_HEALTH => MetadataValue::Float(self.health),
            DATA_PHASE => MetadataValue::VarInt(self.phase.id()),
            _ => return None,
        })
    }
}

/// How far a player's eyes may be from the part it hits.
const REACH: f64 = 6.0;

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut dragon = EnderDragon::with_base(base);
    if let Some(health) = take_number(nbt, "Health") {
        dragon.health = (health as f32).clamp(0.0, max_health());
    }
    dragon.absorption = take_number(nbt, "AbsorptionAmount").map_or(0.0, |v| (v as f32).max(0.0));
    if let Some(phase) = take_number(nbt, "DragonPhase") {
        dragon.phase = PhaseKind::from_id(phase as i32);
    }
    dragon.death_time = take_number(nbt, "DragonDeathTime").map_or(0, |v| v as i32);
    dragon.sitting_damage = take_number(nbt, "sitting_damage_received").map_or(0.0, |v| v as f32);
    Some(Box::new(dragon))
}

#[cfg(test)]
mod tests;

enum Plough {
    PassesThrough,
    Holds,
    Breaks,
}

fn plough_verdict(state: BlockState, griefing: bool) -> Plough {
    use tuff_vanilla_data::tag::block::key::{DRAGON_IMMUNE, DRAGON_TRANSPARENT};
    let block = state.block();
    if tuff_world_model::chunk::is_air(state) || tuff_datapack::tags::is(DRAGON_TRANSPARENT, block)
    {
        Plough::PassesThrough
    } else if griefing && !tuff_datapack::tags::is(DRAGON_IMMUNE, block) {
        Plough::Breaks
    } else {
        Plough::Holds
    }
}

fn area_corner(corner: &[f64]) -> [i32; 3] {
    [0, 1, 2].map(|axis| corner[axis].floor() as i32)
}
