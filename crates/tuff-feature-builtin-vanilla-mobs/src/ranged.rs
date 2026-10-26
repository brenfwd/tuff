//! Ranged attackers: drawing a bow, shooting arrows at a target, and the
//! goal that keeps a skeleton at range, strafing while it draws.

use rand::Rng;
use tuff_item::item::BurnReason;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::MobCore;
use super::data;
use super::goal::{Flags, Goal};
use super::target::TargetView;
use tuff_core::protocol::play;
use tuff_core::world::entity::{Culprit, TickContext};
use tuff_core::world::projectile::{
    self, ArrowShot, Flight, Launch, Owner, Pickup, Shot, shot_velocity, triangle,
};
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_common::combat;
use tuff_vanilla_common::ranged;

pub use tuff_vanilla_data::item::bow_power;

/// A bow drawn this long shoots at full power.
pub const FULL_DRAW: u32 = 20;
const ARROW_SPEED: f32 = 1.6;
/// How fast a mob's crossbow shoots, arrows and rockets alike.
const CROSSBOW_POWER: f32 = 1.6;
const ARROW_DROP: f64 = 0.100_000_001_490_116_12;
const ROCKET_DROP: f64 = 0.150_000_005_960_464_48;

fn is_arrow<M: tuff_item::item::Mode>(stack: &ItemStack<M>) -> bool {
    tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::ARROWS, stack.item())
}

impl MobCore {
    pub fn holding(&self, item: tuff_vanilla_data::Item) -> bool {
        self.equipment.main_hand().is_some_and(|s| s.item() == item)
            || self
                .equipment
                .get(tuff_vanilla_data::item::EquipmentSlot::Offhand)
                .is_some_and(|s| s.item() == item)
    }

    pub fn start_using_item(&mut self) {
        if self.using_item.is_none() {
            self.using_item = Some(0);
            self.changed(data::LIVING_FLAGS);
        }
    }

    pub fn stop_using_item(&mut self) {
        if self.using_item.take().is_some() {
            self.changed(data::LIVING_FLAGS);
        }
    }

    pub fn tick_using_item(&mut self) {
        if let Some(ticks) = &mut self.using_item {
            *ticks += 1;
        }
    }

    /// The hand holding a `weapon`, the main hand first.
    fn weapon_slot(&self, weapon: Item) -> Option<EquipmentSlot> {
        [EquipmentSlot::Mainhand, EquipmentSlot::Offhand]
            .into_iter()
            .find(|slot| {
                self.equipment
                    .get(*slot)
                    .is_some_and(|s| s.item() == weapon)
            })
    }

    /// Where the ammo `weapon` takes is held (the off hand first): arrows,
    /// and rockets for a crossbow.
    fn ammo_slot(&self, weapon: Item) -> Option<EquipmentSlot> {
        let fits = |s: &ItemStack| {
            is_arrow(s) || (weapon == Item::Crossbow && s.item() == Item::FireworkRocket)
        };
        [EquipmentSlot::Offhand, EquipmentSlot::Mainhand]
            .into_iter()
            .find(|slot| self.equipment.get(*slot).is_some_and(fits))
    }

    /// What `weapon` shoots: the ammo held, else a plain arrow.
    pub fn projectile_for(&self, weapon: Item) -> tuff_item::item::ItemSnapshot {
        self.ammo_slot(weapon)
            .and_then(|slot| self.equipment.get(slot))
            .map(tuff_item::item::ItemSnapshot::of)
            .unwrap_or_else(|| tuff_item::item::ItemSnapshot::new(Item::Arrow, 1))
    }

    /// An arrow of `projectile` leaving `start`, fired from `weapon`.
    fn arrow_from<M: tuff_item::item::Mode, N: tuff_item::item::Mode>(
        &self,
        projectile: &ItemStack<M>,
        weapon: Option<&ItemStack<N>>,
        velocity: [f64; 3],
    ) -> ArrowShot {
        #[expect(
            clippy::disallowed_methods,
            reason = "entity drops or fires an item outside loot tables (arrow_from)"
        )]
        let mut item = ItemStack::mint_like(projectile, 1, MintReason::EntityDrop);
        let free = item.components.remove_other(ranged::INTANGIBLE).is_some();
        let mut arrow = ArrowShot::new(item, velocity);
        if free {
            arrow.pickup = Some(Pickup::CreativeOnly);
        }
        if let Some(weapon) = weapon {
            arrow.pierce = ranged::piercing(weapon);
        }
        arrow.weapon = weapon.map(tuff_item::item::ItemSnapshot::of);
        arrow
    }

    /// Spawns `shot` from `start`, owned by this mob.
    fn launch(&self, start: [f64; 3], mut shot: Shot, cx: &mut TickContext) {
        use tuff_vanilla_common::enchant_effects as fx;
        if let Shot::Arrow(arrow) = &mut shot
            && let Some(weapon) = arrow.weapon.clone()
        {
            let look = (self.base.yaw, self.base.pitch);
            let world = fx::arrow_spawned(arrow, &weapon, start, look, &mut *cx.rng);
            let executor = fx::Executor {
                entity: self.base.id,
                uuid: Some(self.base.uuid),
                kind: self.base.kind,
                position: start,
            };
            fx::push_world(cx, executor, world);
        }
        let owner = Owner::of(Culprit::entity(&self.base), None);
        projectile::launch(
            Launch {
                id: 0,
                position: start,
                owner,
                shot,
            },
            cx,
        );
    }

    /// The crossbow in hand holds something to fire.
    pub fn crossbow_charged(&self) -> bool {
        self.weapon_slot(Item::Crossbow)
            .and_then(|slot| self.equipment.get(slot))
            .and_then(|s| s.components.charged_projectiles.as_ref())
            .is_some_and(|loaded| !loaded.is_empty())
    }

    /// Loads the empty crossbow in hand from the ammo held, else with a
    /// free arrow; Multishot adds free copies. True if it loaded.
    pub fn load_crossbow(&mut self) -> bool {
        let Some(slot) = self.weapon_slot(Item::Crossbow) else {
            return false;
        };
        if self.crossbow_charged() {
            return false;
        }
        let Some(crossbow) = self.equipment.get(slot) else {
            return false;
        };
        let held = self.ammo_slot(Item::Crossbow);
        let ammo = self.projectile_for(Item::Crossbow);
        let shots = ranged::projectile_count(crossbow);
        let first = combat::ammo_use(crossbow, ammo.item(), 1.0)
            .floor()
            .max(0.0) as u8;
        let mut left = ammo.count();
        let mut drawn = Vec::new();
        for i in 0..shots {
            let uses = if i > 0 { 0 } else { first };
            if uses > left {
                continue;
            }
            if uses == 0 {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "entity drops or fires an item outside loot tables (load_crossbow)"
                )]
                let free = ItemStack::mint_like(&ammo, 1, MintReason::EntityDrop);
                drawn.push(ranged::intangible(free));
            } else {
                left -= uses;
                let taken = match held {
                    Some(held) => self.equipment.take_some(held, uses),
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "entity drops or fires an item outside loot tables (load_crossbow)"
                    )]
                    None => Some(ItemStack::mint_like(&ammo, uses, MintReason::EntityDrop)),
                };
                drawn.extend(taken);
            }
        }
        if drawn.is_empty() {
            return false;
        }
        if let Some(crossbow) = self.equipment.get_mut(slot) {
            crossbow.components.charged_projectiles = Some(drawn);
        }
        true
    }

    /// Empties a half-loaded crossbow (a charge cut short).
    fn unload_crossbow(&mut self) {
        if let Some(slot) = self.weapon_slot(Item::Crossbow)
            && let Some(crossbow) = self.equipment.get_mut(slot)
            && let Some(loaded) = crossbow.components.charged_projectiles.take()
        {
            for projectile in loaded {
                projectile.burn(BurnReason::Discarded);
            }
        }
    }

    /// Fires everything the crossbow in hand holds at `target`: arrows
    /// and rockets, Multishot's side bolts fanned out about the aim. Each
    /// shot wears the crossbow (a rocket three times as much).
    pub fn shoot_crossbow(&mut self, target: &TargetView, sound: SoundEvent, cx: &mut TickContext) {
        let Some(slot) = self.weapon_slot(Item::Crossbow) else {
            return;
        };
        let Some(crossbow) = self.equipment.get_mut(slot) else {
            return;
        };
        let projectiles = crossbow
            .components
            .charged_projectiles
            .take()
            .unwrap_or_default();
        if projectiles.is_empty() {
            return;
        }
        let Some(crossbow) = self
            .equipment
            .get(slot)
            .map(tuff_item::item::ItemSnapshot::of)
        else {
            return;
        };
        let difficulty = f32::from(cx.shared.difficulty.borrow().id());
        let uncertainty = 14.0 - difficulty * 4.0;
        let angles = ranged::volley_angles(projectiles.len(), ranged::projectile_spread(&crossbow));
        let up = ranged::view_vector(self.base.pitch - 90.0, self.base.yaw);
        let eye = self.eye_position();
        let height = target.bounds[4] - target.bounds[1];
        for (index, (projectile, angle)) in projectiles.into_iter().zip(angles).enumerate() {
            let rocket = projectile.item() == Item::FireworkRocket;
            let drop = if rocket { ROCKET_DROP } else { ARROW_DROP };
            let start = [self.base.position[0], eye[1] - drop, self.base.position[2]];
            let dx = target.position[0] - start[0];
            let dz = target.position[2] - start[2];
            let distance = (dx * dx + dz * dz).sqrt();
            let dy =
                target.position[1] + height / 3.0 - start[1] + distance * 0.200_000_002_980_232_24;
            let direction = ranged::aimed_volley_direction([dx, dy, dz], angle, up);
            let velocity = shot_velocity(direction, CROSSBOW_POWER, uncertainty, cx.rng);
            let shot = if rocket {
                Shot::Firework {
                    item: projectile,
                    flight: Flight::Shot(velocity),
                }
            } else {
                let mut arrow = self.arrow_from(&projectile, Some(&*crossbow), velocity);
                projectile.burn(BurnReason::Consumed);
                arrow.sound = Some(SoundEvent::ItemCrossbowHit);
                Shot::Arrow(arrow)
            };
            self.launch(start, shot, cx);
            let pitch = if index == 0 {
                1.0
            } else {
                let high = index & 1 == 1;
                1.0 / (cx.rng.r#gen::<f32>() * 0.5 + 1.8) + if high { 0.63 } else { 0.43 }
            };
            self.sound_as(sound, self.sound_category(), 1.0, pitch, cx);
            let wear = if rocket { 3 } else { 1 };
            let broken = self.equipment.get_mut(slot).is_some_and(|crossbow| {
                crossbow.hurt(wear, cx.rng) == tuff_item::item::Hurt::Broken
            });
            if broken {
                if let Some(crossbow) = self.equipment.take(slot) {
                    crossbow.burn(BurnReason::Broken);
                }
                break;
            }
        }
    }

    /// Shoots an arrow from its bow at `target`, aimed a little high for
    /// the drop, spread less on harder difficulties.
    pub fn shoot_arrow(
        &mut self,
        target: &TargetView,
        power: f32,
        sound: SoundEvent,
        cx: &mut TickContext,
    ) {
        let difficulty = f64::from(cx.shared.difficulty.borrow().id());
        let eye = self.eye_position();
        let start = [eye[0], eye[1] - 0.100_000_001_490_116_12, eye[2]];
        let height = target.bounds[4] - target.bounds[1];
        let dx = target.position[0] - self.base.position[0];
        let dy = target.position[1] + height / 3.0 - start[1];
        let dz = target.position[2] - self.base.position[2];
        let distance = (dx * dx + dz * dz).sqrt();
        let uncertainty = 14.0 - difficulty as f32 * 4.0;
        let velocity = shot_velocity(
            [dx, dy + distance * 0.200_000_002_980_232_24, dz],
            ARROW_SPEED,
            uncertainty,
            cx.rng,
        );
        let weapon = self
            .weapon_slot(Item::Bow)
            .and_then(|slot| self.equipment.get(slot))
            .map(tuff_item::item::ItemSnapshot::of);
        let projectile = self.projectile_for(Item::Bow);
        let mut arrow = self.arrow_from(&projectile, weapon.as_deref(), velocity);
        arrow.damage = Some(f64::from(power * 2.0) + triangle(cx.rng, difficulty * 0.11, 0.574_25));
        if arrow.kind() == EntityType::Arrow
            && let Some(hook) = self.arrow_hook
        {
            hook(self, &mut arrow);
        }
        self.launch(start, Shot::Arrow(arrow), cx);
        let pitch = 1.0 / (cx.rng.r#gen::<f32>() * 0.4 + 0.8);
        self.sound(sound, 1.0, pitch, cx);
    }
}

/// Keeps a target at bow range: walks closer while it cannot see it or is
/// too far, strafes around it otherwise, draws for a second and shoots.
#[derive(Debug)]
pub struct RangedBowAttack {
    speed: f64,
    radius_sq: f32,
    min_interval: fn(&TickContext) -> i32,
    sound: SoundEvent,
    see_time: i32,
    clockwise: bool,
    backwards: bool,
    attack_time: i32,
    strafing_time: i32,
}

impl RangedBowAttack {
    pub fn new(
        speed: f64,
        min_interval: fn(&TickContext) -> i32,
        radius: f32,
        sound: SoundEvent,
    ) -> Self {
        Self {
            speed,
            radius_sq: radius * radius,
            min_interval,
            sound,
            see_time: 0,
            clockwise: false,
            backwards: false,
            attack_time: -1,
            strafing_time: -1,
        }
    }
}

impl Goal for RangedBowAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_some() && mob.holding(tuff_vanilla_data::Item::Bow)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        (self.can_use(mob, cx) || !mob.navigation.is_done())
            && mob.holding(tuff_vanilla_data::Item::Bow)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.set_aggressive(true);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.set_aggressive(false);
        self.see_time = 0;
        self.attack_time = -1;
        mob.stop_using_item();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        let distance = mob.distance_sq(target.position);
        let sees = mob.has_line_of_sight(&target, cx);
        if sees != (self.see_time > 0) {
            self.see_time = 0;
        }
        if sees {
            self.see_time += 1;
        } else {
            self.see_time -= 1;
        }
        let radius = f64::from(self.radius_sq);
        if distance > radius || self.see_time < 20 {
            mob.move_to_entity(target.position, self.speed, cx);
            self.strafing_time = -1;
        } else {
            mob.navigation.stop();
            self.strafing_time += 1;
        }
        if self.strafing_time >= 20 {
            if cx.rng.r#gen::<f32>() < 0.3 {
                self.clockwise = !self.clockwise;
            }
            if cx.rng.r#gen::<f32>() < 0.3 {
                self.backwards = !self.backwards;
            }
            self.strafing_time = 0;
        }
        if self.strafing_time > -1 {
            if distance > radius * 0.75 {
                self.backwards = false;
            } else if distance < radius * 0.25 {
                self.backwards = true;
            }
            mob.strafe(
                if self.backwards { -0.5 } else { 0.5 },
                if self.clockwise { 0.5 } else { -0.5 },
            );
            mob.turn_to(target.eye, 30.0, 30.0);
        } else {
            mob.look.set_look_at(target.eye, 30.0, 30.0);
        }
        if let Some(ticks) = mob.using_item {
            if !sees && self.see_time < -60 {
                mob.stop_using_item();
            } else if sees && ticks >= FULL_DRAW {
                mob.stop_using_item();
                mob.shoot_arrow(&target, bow_power(ticks), self.sound, cx);
                self.attack_time = (self.min_interval)(cx);
            }
            return;
        }
        self.attack_time -= 1;
        if self.attack_time <= 0 && self.see_time >= -60 {
            mob.start_using_item();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrossbowState {
    Uncharged,
    Charging,
    Charged,
    ReadyToAttack,
}

const BACK_UP_SPEED: f32 = 0.75;

/// Keeps a target at crossbow range like [`RangedBowAttack`], but with the
/// crossbow's own charge/charged/ready cycle: it loads once out of range
/// or out of sight, waits out a short delay once loaded, then fires as
/// soon as it can see its target again.
#[derive(Debug)]
pub struct RangedCrossbowAttack {
    speed: f64,
    radius_sq: f32,
    item: tuff_vanilla_data::Item,
    sound: SoundEvent,
    state: CrossbowState,
    see_time: i32,
    attack_delay: i32,
    update_path_delay: i32,
    back_up_within: Option<f64>,
    start_sound_played: bool,
    mid_sound_played: bool,
}

impl RangedCrossbowAttack {
    /// Backs away from a target it sees closer than `distance`.
    pub fn backing_up(mut self, distance: f64) -> Self {
        self.back_up_within = Some(distance);
        self
    }

    pub fn new(speed: f64, radius: f32, item: tuff_vanilla_data::Item, sound: SoundEvent) -> Self {
        Self {
            speed,
            radius_sq: radius * radius,
            item,
            sound,
            state: CrossbowState::Uncharged,
            see_time: 0,
            attack_delay: 0,
            update_path_delay: 0,
            back_up_within: None,
            start_sound_played: false,
            mid_sound_played: false,
        }
    }
}

impl RangedCrossbowAttack {
    /// The start and middle loading sounds, a fifth and half way in.
    fn charging_sounds(&mut self, mob: &MobCore, ticks: u32, charge: u32, cx: &mut TickContext) {
        let done = ticks as f32 / charge.max(1) as f32;
        if done < 0.2 {
            self.start_sound_played = false;
            self.mid_sound_played = false;
        }
        let Some(crossbow) = mob
            .weapon_slot(self.item)
            .and_then(|slot| mob.equipment.get(slot))
        else {
            return;
        };
        let sounds = ranged::charging_sounds(crossbow);
        let category = play::sound_category::PLAYERS;
        if done >= 0.2 && !self.start_sound_played {
            self.start_sound_played = true;
            if let Some(sound) = sounds.start {
                mob.sound_as(sound, category, 0.5, 1.0, cx);
            }
        }
        if done >= 0.5 && !self.mid_sound_played {
            self.mid_sound_played = true;
            if let Some(sound) = sounds.mid {
                mob.sound_as(sound, category, 0.5, 1.0, cx);
            }
        }
    }

    /// Loading finished: the crossbow takes its ammo, with the end sound.
    fn loaded(&self, mob: &mut MobCore, cx: &mut TickContext) {
        let end = mob
            .weapon_slot(self.item)
            .and_then(|slot| mob.equipment.get(slot))
            .and_then(|crossbow| ranged::charging_sounds(crossbow).end);
        if mob.load_crossbow()
            && let Some(sound) = end
        {
            let pitch = 1.0 / (cx.rng.r#gen::<f32>() * 0.5 + 1.0) + 0.2;
            mob.sound_as(sound, mob.sound_category(), 1.0, pitch, cx);
        }
    }
}

impl Goal for RangedCrossbowAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_some() && mob.holding(self.item)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        (self.can_use(mob, cx) || !mob.navigation.is_done()) && mob.holding(self.item)
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.set_aggressive(false);
        self.see_time = 0;
        if mob.using_item.is_some() {
            mob.stop_using_item();
            mob.unload_crossbow();
        }
        self.state = CrossbowState::Uncharged;
        mob.signal = 0;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        let sees = mob.has_line_of_sight(&target, cx);
        if sees != (self.see_time > 0) {
            self.see_time = 0;
        }
        if sees {
            self.see_time += 1;
        } else {
            self.see_time -= 1;
        }
        let radius = f64::from(self.radius_sq);
        let out_of_position = (mob.distance_sq(target.position) > radius || self.see_time < 5)
            && self.attack_delay == 0;
        if out_of_position {
            self.update_path_delay -= 1;
            if self.update_path_delay <= 0 {
                let speed = if self.state == CrossbowState::Uncharged {
                    self.speed
                } else {
                    self.speed * 0.5
                };
                mob.move_to_entity(target.position, speed, cx);
                self.update_path_delay = cx.rng.gen_range(20..=40);
            }
        } else {
            self.update_path_delay = 0;
            mob.navigation.stop();
            if sees
                && self
                    .back_up_within
                    .is_some_and(|d| mob.distance_sq(target.position) < d * d)
            {
                mob.strafe(-BACK_UP_SPEED, 0.0);
                mob.base.yaw = mob.head_yaw;
            }
        }
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        match self.state {
            CrossbowState::Uncharged => {
                if !out_of_position {
                    mob.start_using_item();
                    self.state = CrossbowState::Charging;
                    mob.signal = 1;
                }
            }
            CrossbowState::Charging => {
                let charge = mob
                    .weapon_slot(self.item)
                    .and_then(|slot| mob.equipment.get(slot))
                    .map_or(25, ranged::crossbow_charge_ticks);
                match mob.using_item {
                    None => {
                        self.state = CrossbowState::Uncharged;
                        mob.signal = 0;
                    }
                    Some(ticks) if ticks >= charge => {
                        mob.stop_using_item();
                        self.loaded(mob, cx);
                        self.state = CrossbowState::Charged;
                        self.attack_delay = 20 + cx.rng.gen_range(0..20);
                        mob.signal = 0;
                    }
                    Some(ticks) => self.charging_sounds(mob, ticks, charge, cx),
                }
            }
            CrossbowState::Charged => {
                self.attack_delay -= 1;
                if self.attack_delay <= 0 {
                    self.state = CrossbowState::ReadyToAttack;
                }
            }
            CrossbowState::ReadyToAttack => {
                if sees {
                    mob.shoot_crossbow(&target, self.sound, cx);
                    self.state = CrossbowState::Uncharged;
                }
            }
        }
    }
}

/// A drowned throwing its trident: it closes to within `radius` with
/// sight of its target, and throws every `interval` ticks, sooner the
/// closer the target. It keeps its own trident.
#[derive(Debug)]
pub struct TridentAttack {
    speed: f64,
    interval: i32,
    radius: f32,
    attack_time: i32,
    see_time: i32,
}

impl TridentAttack {
    pub fn new(speed: f64, interval: i32, radius: f32) -> Self {
        Self {
            speed,
            interval,
            radius,
            attack_time: -1,
            see_time: 0,
        }
    }

    fn throw(&self, mob: &MobCore, target: &TargetView, cx: &mut TickContext) {
        #[expect(
            clippy::disallowed_methods,
            reason = "entity drops or fires an item outside loot tables (throw)"
        )]
        let stack = mob
            .equipment
            .main_hand()
            .filter(|s| s.item() == tuff_vanilla_data::Item::Trident)
            .map(|held| ItemStack::mint_like(held, 1, MintReason::EntityDrop))
            .unwrap_or_else(|| ItemStack::mint(Item::Trident, 1, MintReason::EntityDrop));
        let eye = mob.eye_position();
        let start = [
            mob.base.position[0],
            eye[1] - 0.100_000_001_490_116_12,
            mob.base.position[2],
        ];
        let height = target.bounds[4] - target.bounds[1];
        let dx = target.position[0] - mob.base.position[0];
        let dy = target.position[1] + height / 3.0 - start[1];
        let dz = target.position[2] - mob.base.position[2];
        let horizontal = (dx * dx + dz * dz).sqrt();
        let difficulty = cx.shared.difficulty.borrow().id();
        let uncertainty = 14.0 - f32::from(difficulty) * 4.0;
        let velocity = shot_velocity([dx, dy + horizontal * 0.2, dz], 1.6, uncertainty, cx.rng);
        mob.launch(start, Shot::Arrow(ArrowShot::new(stack, velocity)), cx);
        let pitch = 1.0 / (cx.rng.r#gen::<f32>() * 0.4 + 0.8);
        mob.sound(SoundEvent::EntityDrownedShoot, 1.0, pitch, cx);
    }
}

impl Goal for TridentAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.equipment
            .main_hand()
            .is_some_and(|s| s.item() == tuff_vanilla_data::Item::Trident)
            && mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx)
            || (mob.target_view(cx).is_some_and(|t| t.alive) && !mob.navigation.is_done())
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.set_aggressive(true);
        mob.start_using_item();
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.see_time = 0;
        self.attack_time = -1;
        mob.stop_using_item();
        mob.set_aggressive(false);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        let distance_sq = mob.distance_sq(target.position);
        let sees = mob.has_line_of_sight(&target, cx);
        self.see_time = if sees { self.see_time + 1 } else { 0 };
        let radius = f64::from(self.radius);
        if distance_sq > radius * radius || self.see_time < 5 {
            mob.move_to_entity(target.position, self.speed, cx);
        } else {
            mob.navigation.stop();
        }
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        self.attack_time -= 1;
        if self.attack_time == 0 {
            if !sees {
                return;
            }
            self.throw(mob, &target, cx);
            self.attack_time = self.interval;
        } else if self.attack_time < 0 {
            self.attack_time = self.interval;
        }
    }
}
