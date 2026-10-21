//! Nautilus: a tameable water creature fed and bred on fish that dries out
//! on land like a dolphin. Hurt or hungry, it charges at its target in a
//! dash; tamed and saddled, a player rides it through the water, dashing on
//! the jump key and breathing under water while mounted. Zombie nautilus
//! are its hostile undead relative, never tamed or a baby, attacking with
//! the same charge.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_core::world::menu::{MenuKind, MountArmor};
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect, SoundEvent};
use tuff_vanilla_data::{Item, data_registry, tag};

use super::control::{LookStyle, MoveStyle, SmoothSwim, wrap_degrees};
use super::equine::{MountInventory, saddled, try_equip};
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{Breed, Panic, RandomLookAround, SwimStroll, Tempt};
use super::movement::{Sinking, WaterTravel};
use super::navigation::NavKind;
use super::physics;
use super::target::{TargetView, view};
use super::{MobCore, MobSounds, OwnAirRules, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::take;
use tuff_core::world::entity::{
    Culprit, Effect, EntityHit, EntityInteraction, InteractOutcome, RiderInput, TickContext,
};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::{ItemStack, MintReason};

const SWIM_SPEED: f64 = 1.0;
const DRY_OUT_MAX: i16 = 300;
const IN_WATER_SPEED: f32 = 0.011;
const RIDDEN_SPEED_IN_WATER: f32 = 0.0325;
const RIDDEN_SPEED_ON_LAND: f32 = 0.02;
/// A swimming speed is about this many times the ground speed the rider
/// checks compare it with.
const RIDDEN_GROUND_RATIO: f64 = 2.2;
const DASH_MOMENTUM: f64 = 1.2;
/// The tame flags at [`data::SPECIES`] and owner after them come first;
/// whether it is dashing follows.
const DASH: u8 = data::SPECIES + 2;
const DASH_COOLDOWN: i32 = 40;
const DASH_SHOWN: i32 = 35;
const BREATH_TICKS: i64 = 60;
const BREATH_REFRESH: u64 = 40;
const CHARGE_COOLDOWN: u64 = 80;
const MAX_CHARGE_DISTANCE: f64 = 12.0;
const MAX_TARGET_DISTANCE: f64 = 11.0;
const CHARGE_KNOCKBACK: f32 = 2.0;
const ANGER_TICKS: u64 = 400;

fn configure_swimming(core: &mut MobCore) {
    core.set_path_malus(tuff_vanilla_data::entity::PathType::Water, 0.0);
    core.navigation.kind = NavKind::WaterBound { breaching: false };
    core.move_style = MoveStyle::SmoothSwim(SmoothSwim {
        max_turn_x: 85.0,
        max_turn_y: 10.0,
        in_water_speed: IN_WATER_SPEED,
        outside_water_speed: 0.0,
        buoyant: true,
    });
    core.look_style = LookStyle::SmoothSwim {
        max_from_center: 10.0,
    };
    core.walk_value = Some(|_, _, _| 0.0);
    core.water_travel = WaterTravel::Swimmer {
        stroke: None,
        sinks: Sinking::Never,
    };
}

/// A zombie nautilus's variant. It is built on the same ageable/tameable
/// ancestry as [`Nautilus`] (its dumped layout has the same unused
/// baby/age-locked/tame-flags/owner/dash slots), so only this last field
/// -- the network id of its variant, always the registry's first
/// (temperate) entry -- follows them.
const ZOMBIE_VARIANT: u8 = data::SPECIES + 3;

fn nautilus_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::NAUTILUS_FOOD, item)
}

fn nautilus_taming_item(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::NAUTILUS_TAMING_ITEMS, item)
}

const NAUTILUS_FEED_HEAL: f32 = 2.0;

fn food_effect(stack: &ItemStack) -> HandEffect {
    if tuff_datapack::tags::is(tag::item::key::NAUTILUS_BUCKET_FOOD, stack.item) {
        HandEffect::ExchangeStack(Box::new(ItemStack::mint(
            Item::WaterBucket,
            1,
            MintReason::Spawn,
        )))
    } else {
        HandEffect::Consume(1)
    }
}

fn dry_out_rules() -> OwnAirRules {
    OwnAirRules {
        max: DRY_OUT_MAX,
        in_rain: false,
        damage_kind: &damage_type::DRY_OUT,
        damage: 2.0,
    }
}

fn animal_experience(rng: &mut StdRng) -> i32 {
    1 + rng.gen_range(0..3)
}

fn tamed(core: &MobCore) -> bool {
    core.tame.as_ref().is_some_and(|t| t.is_tamed())
}

/// Dashes straight at its target and rams it, then rests four seconds.
#[derive(Debug)]
struct ChargeAttack {
    speed: f64,
    sound: SoundEvent,
    start: [f64; 3],
    velocity: [f64; 3],
    ready_at: u64,
}

impl ChargeAttack {
    fn new(speed: f64, sound: SoundEvent) -> Self {
        Self {
            speed,
            sound,
            start: [0.0; 3],
            velocity: [0.0; 3],
            ready_at: 0,
        }
    }

    fn still_charging(&self, mob: &MobCore, target: &TargetView, cx: &TickContext) -> bool {
        !tamed(mob)
            && cx.tick >= self.ready_at
            && tuff_core::world::distance_sq(mob.base.position, self.start)
                < MAX_CHARGE_DISTANCE * MAX_CHARGE_DISTANCE
            && mob.distance_sq(target.position) < MAX_TARGET_DISTANCE * MAX_TARGET_DISTANCE
            && mob.has_line_of_sight(target, cx)
    }

    /// The first living thing in the way that it may attack, other than
    /// its own passengers.
    fn rammed(&self, mob: &MobCore, cx: &TickContext) -> Option<TargetView> {
        let bounds = mob.bounds();
        let players = cx.players.iter().map(|p| p.id);
        let entities = cx
            .entities
            .iter()
            .filter(|e| e.living.is_some() && e.id != mob.base.id && e.vehicle != Some(mob.base.id))
            .map(|e| e.id);
        players
            .chain(entities)
            .filter_map(|id| view(cx, id))
            .filter(|t| t.alive && physics::intersects(bounds, t.bounds))
            .find(|t| mob.can_attack(t, cx) && !mob.allied_to(t, cx))
    }

    /// Hits `target` for its attack damage, knocking it on along the
    /// charge.
    fn ram(&self, mob: &MobCore, target: &TargetView, cx: &mut TickContext) {
        let mut hit = EntityHit::new(
            mob.attribute(Attribute::AttackDamage) as f32,
            &damage_type::MOB_ATTACK,
        );
        hit.attacker = Some(Culprit::entity(&mob.base));
        hit.position = Some(mob.base.position);
        let level = |effect| {
            mob.effects
                .amplifier(effect)
                .map_or(0.0, |amplifier| f32::from(amplifier) + 1.0)
        };
        let boost = 0.25 * (level(MobEffect::Speed) - level(MobEffect::Slowness));
        let factor = (self.speed as f32 * mob.attribute(Attribute::MovementSpeed) as f32)
            .clamp(0.2, 2.0)
            + boost;
        let [vx, _, vz] = mob.base.velocity;
        hit.knockback = Some((f64::from(factor * CHARGE_KNOCKBACK), -vx, -vz));
        cx.effects.push(Effect::Hurt {
            target: target.id,
            hit: Box::new(hit),
        });
    }
}

impl Goal for ChargeAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        cx.tick >= self.ready_at && !tamed(mob) && mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx)
            .is_some_and(|t| t.alive && self.still_charging(mob, &t, cx))
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.start = mob.base.position;
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        let delta: [f64; 3] = std::array::from_fn(|axis| target.position[axis] - self.start[axis]);
        let length = delta.iter().map(|c| c * c).sum::<f64>().sqrt();
        self.velocity = if length < 1.0e-4 {
            [0.0; 3]
        } else {
            delta.map(|c| c / length * self.speed)
        };
        if self.still_charging(mob, &target, cx) {
            mob.sound(self.sound, 1.0, 1.0, cx);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.ready_at = cx.tick + CHARGE_COOLDOWN;
        mob.target = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.turn_to(target.eye, 360.0, 360.0);
        mob.base.velocity = self.velocity;
        mob.impulse = true;
        if let Some(victim) = self.rammed(mob, cx) {
            self.ram(mob, &victim, cx);
            self.ready_at = cx.tick + CHARGE_COOLDOWN;
            mob.target = None;
        }
    }
}

/// A wild adult nautilus in the water goes after whoever hurt it, and
/// every two or three minutes, half the time, after a pufferfish nearby.
#[derive(Debug, Default)]
struct FindTarget {
    hunt_at: Option<u64>,
    found: Option<i32>,
}

impl Goal for FindTarget {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let breeding = mob.love.as_ref().is_some_and(|l| l.is_in_love());
        if breeding || !mob.body.in_water || mob.is_baby() || tamed(mob) {
            return false;
        }
        if let Some((attacker, at)) = mob.last_hurt_by_mob
            && cx.tick.saturating_sub(at) < ANGER_TICKS
            && let Some(target) = view(cx, attacker)
            && target.in_water
            && mob.can_attack(&target, cx)
        {
            self.found = Some(attacker);
            return true;
        }
        let hunt_at = *self
            .hunt_at
            .get_or_insert_with(|| cx.tick + cx.rng.gen_range(2400..=3600));
        if cx.tick < hunt_at {
            return false;
        }
        self.hunt_at = Some(cx.tick + cx.rng.gen_range(2400..=3600));
        if cx.rng.r#gen::<f32>() < 0.5 {
            return false;
        }
        self.found = cx
            .entities
            .iter()
            .filter(|e| tuff_datapack::tags::is(tag::entity_type::key::NAUTILUS_HOSTILES, e.kind))
            .filter_map(|e| view(cx, e.id))
            .filter(|t| t.in_water && mob.can_attack(t, cx) && mob.has_line_of_sight(t, cx))
            .map(|t| (mob.distance_sq(t.position), t.id))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id);
        self.found.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target
            .and_then(|id| view(cx, id))
            .is_some_and(|t| t.alive && mob.can_attack(&t, cx))
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = self.found;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = None;
    }
}

/// A rider's dash, and the cooldown shown after it.
#[derive(Debug, Default)]
struct Dash {
    cooldown: i32,
    dashing: bool,
    jump_held: bool,
}

impl Dash {
    fn tick(
        &mut self,
        core: &mut MobCore,
        ready_sound: (SoundEvent, SoundEvent),
        cx: &mut TickContext,
    ) {
        if self.dashing && self.cooldown < DASH_SHOWN {
            self.dashing = false;
            core.changed(DASH);
        }
        if self.cooldown > 0 {
            self.cooldown -= 1;
            if self.cooldown == 0 {
                let sound = if core.body.in_water {
                    ready_sound.0
                } else {
                    ready_sound.1
                };
                core.sound(sound, 1.0, 1.0, cx);
            }
        }
    }

    /// Bursts along the rider's look on a fresh press of the jump key.
    fn ride(
        &mut self,
        core: &mut MobCore,
        jumping: bool,
        look: [f64; 3],
        sound: (SoundEvent, SoundEvent),
        cx: &mut TickContext,
    ) {
        let pressed = jumping && !self.jump_held;
        self.jump_held = jumping;
        if !pressed || !saddled(core) || self.cooldown > 0 {
            return;
        }
        let momentum = if core.body.in_water { 1.2 } else { 0.5 };
        let speed = momentum * core.attribute(Attribute::MovementSpeed);
        for (axis, c) in look.iter().enumerate() {
            core.base.velocity[axis] += c * speed;
        }
        core.impulse = true;
        self.cooldown = DASH_COOLDOWN;
        self.dashing = true;
        core.changed(DASH);
        let sound = if core.body.in_water { sound.0 } else { sound.1 };
        core.sound(sound, 1.0, 1.0, cx);
    }
}

/// The way a rider with this yaw and pitch looks.
fn look_vector(yaw: f32, pitch: f32) -> [f64; 3] {
    let (yaw, pitch) = (f64::from(yaw.to_radians()), f64::from(pitch.to_radians()));
    [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]
}

/// A tamed and peaceful nautilus.
#[derive(Debug, Default)]
pub struct Nautilus {
    inventory: MountInventory,
    dash: Dash,
    breathing_rider: bool,
    zombie: bool,
}

impl Nautilus {
    fn ready_sounds(&self) -> (SoundEvent, SoundEvent) {
        if self.zombie {
            (
                SoundEvent::EntityZombieNautilusDashReady,
                SoundEvent::EntityZombieNautilusDashReadyLand,
            )
        } else {
            (
                SoundEvent::EntityNautilusDashReady,
                SoundEvent::EntityNautilusDashReadyLand,
            )
        }
    }

    fn dash_sounds(&self) -> (SoundEvent, SoundEvent) {
        if self.zombie {
            (
                SoundEvent::EntityZombieNautilusDash,
                SoundEvent::EntityZombieNautilusDashLand,
            )
        } else {
            (
                SoundEvent::EntityNautilusDash,
                SoundEvent::EntityNautilusDashLand,
            )
        }
    }

    /// Keeps a riding player breathing under water.
    fn give_breath(&mut self, core: &MobCore, cx: &mut TickContext) {
        let Some(rider) = core.base.riders.first().map(|r| r.id) else {
            self.breathing_rider = false;
            return;
        };
        if self.breathing_rider && !cx.tick.is_multiple_of(BREATH_REFRESH) {
            return;
        }
        self.breathing_rider = true;
        let mut breath = ActiveEffect::timed(MobEffect::BreathOfTheNautilus, BREATH_TICKS, 0);
        breath.ambient = true;
        tuff_core::world::effects::apply_potion(
            rider,
            true,
            EntityType::Player,
            &[breath],
            &[],
            1.0,
            false,
            Some(Culprit::entity(&core.base)),
            cx,
        );
    }
}

impl Species for Nautilus {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.make_tameable(data::SPECIES);
        configure_swimming(core);
        let mut goals = GoalSelector::default();
        goals.add(0, Panic::new(1.6));
        goals.add(0, ChargeAttack::new(0.6, SoundEvent::EntityNautilusDash));
        goals.add(1, Breed::new(0.4).brain());
        goals.add(2, Tempt::new(1.3, nautilus_food));
        goals.add(3, FindTarget::default());
        goals.add(4, SwimStroll::new(SWIM_SPEED));
        goals.add(5, RandomLookAround::default());
        core.goals = goals;
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut values = core.tame_metadata();
        values.push((DASH, MetadataValue::Boolean(self.dash.dashing)));
        values
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.inventory.sync(core, cx);
        self.give_breath(core, cx);
        let ready = self.ready_sounds();
        self.dash.tick(core, ready, cx);
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let on_land = !core.body.in_water;
        MobSounds {
            ambient: Some(if on_land {
                SoundEvent::EntityNautilusAmbientLand
            } else {
                SoundEvent::EntityNautilusAmbient
            }),
            hurt: Some(if on_land {
                SoundEvent::EntityNautilusHurtLand
            } else {
                SoundEvent::EntityNautilusHurt
            }),
            death: Some(if on_land {
                SoundEvent::EntityNautilusDeathLand
            } else {
                SoundEvent::EntityNautilusDeath
            }),
            step: None,
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntityNautilusEat)
    }

    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(dry_out_rules())
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        nautilus_food(stack.item)
    }

    fn seats(&self, core: &MobCore) -> usize {
        usize::from(tamed(core) && !core.is_baby())
    }

    fn mount_refused(&self, core: &MobCore, stack: Option<&ItemStack>) -> bool {
        let Some(stack) = stack else {
            return false;
        };
        if !tamed(core) || core.is_baby() {
            return false;
        }
        nautilus_food(stack.item)
            || stack.equippable().is_some_and(|eq| {
                eq.equip_on_interact
                    && matches!(eq.slot, EquipmentSlot::Saddle | EquipmentSlot::Body)
                    && core.equipment.get(eq.slot).is_none()
            })
    }

    fn inventory(&self) -> Option<(&[Option<ItemStack>], MenuKind)> {
        Some((
            self.inventory.chest(),
            MenuKind::Mount {
                columns: 0,
                saddle: true,
                armor: MountArmor::Nautilus,
            },
        ))
    }

    fn inventory_mut(&mut self) -> Option<&mut [Option<ItemStack>]> {
        Some(self.inventory.chest_from_screen())
    }

    fn inventory_opens_for(&self, core: &MobCore, player: i32, sneaking: bool) -> bool {
        sneaking
            && tamed(core)
            && !core.is_baby()
            && (core.base.riders.is_empty() || core.base.riders.iter().any(|r| r.id == player))
    }

    fn load(&mut self, core: &mut MobCore, _nbt: &mut tuff_nbt::Compound) {
        self.inventory.adopt(core);
    }

    fn drivable(&self, core: &MobCore) -> bool {
        tamed(core) && saddled(core)
    }

    fn ridden_speed(&self, core: &MobCore) -> Option<tuff_core::world::entity::RiddenSpeed> {
        let attribute = core.attribute(Attribute::MovementSpeed);
        Some(tuff_core::world::entity::RiddenSpeed {
            speed: f64::from(RIDDEN_SPEED_IN_WATER) * 4.0 * attribute / RIDDEN_GROUND_RATIO,
            leap: DASH_MOMENTUM * attribute,
            ..Default::default()
        })
    }

    /// Swims where its rider looks: forward along the rider's pitch, half
    /// as fast backing up, turning halfway to the rider's facing each tick.
    fn ride(&mut self, core: &mut MobCore, input: RiderInput, cx: &mut TickContext) -> bool {
        let rider = core.base.riders.first().map(|r| r.id);
        let pitch = cx
            .players
            .iter()
            .find(|p| Some(p.id) == rider)
            .map_or(0.0, |p| p.pitch);
        let (mut forward, mut up) = (0.0, 0.0);
        if input.forward != 0.0 {
            forward = pitch.to_radians().cos();
            up = -pitch.to_radians().sin();
            if input.forward < 0.0 {
                forward *= -0.5;
                up *= -0.5;
            }
        }
        let yaw = core.base.yaw + wrap_degrees(input.yaw - core.base.yaw) * 0.5;
        core.base.yaw = yaw;
        core.head_yaw = yaw;
        core.body_yaw = yaw;
        core.base.pitch = pitch * 0.5;
        let speed = if core.body.in_water {
            RIDDEN_SPEED_IN_WATER
        } else {
            RIDDEN_SPEED_ON_LAND
        } * core.attribute(Attribute::MovementSpeed) as f32;
        core.body.speed = speed;
        core.body.xxa = -input.strafe;
        core.body.yya = up;
        core.body.zza = forward;
        core.jump.jump = false;
        core.tick_jump_control();
        let sounds = self.dash_sounds();
        self.dash.ride(
            core,
            input.jumping,
            look_vector(input.yaw, pitch),
            sounds,
            cx,
        );
        true
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if core.is_baby() {
            return None;
        }
        if tamed(core) {
            if interaction.sneaking {
                return None;
            }
            if tuff_datapack::tags::is(tag::item::key::NAUTILUS_FOOD, stack.item)
                && core.health < core.max_health()
            {
                core.heal(NAUTILUS_FEED_HEAL);
                return Some(food_effect(stack));
            }
            let equipped = try_equip(core, stack, true, cx);
            self.inventory.adopt(core);
            return equipped;
        }
        if !nautilus_taming_item(stack.item) {
            return None;
        }
        core.try_tame(interaction.player_uuid, 1.0 / 3.0, cx);
        Some(food_effect(stack))
    }

    fn offspring(&self, _partner: &Self, _rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }

    fn custom_persistence(&self, core: &MobCore) -> bool {
        tamed(core)
    }
}

/// A hostile, undead zombie nautilus. Fed a taming item it can be tamed,
/// saddled and ridden like the living one.
#[derive(Debug)]
pub struct ZombieNautilus {
    inner: Nautilus,
    warm: bool,
}

impl Default for ZombieNautilus {
    fn default() -> Self {
        Self {
            inner: Nautilus {
                zombie: true,
                ..Nautilus::default()
            },
            warm: false,
        }
    }
}

const TEMPERATE_VARIANT: &str = "minecraft:temperate";
const WARM_VARIANT: &str = "minecraft:warm";

fn coral_biome(biome: &str) -> bool {
    tuff_datapack::tags::has(
        tag::worldgen::biome::key::SPAWNS_CORAL_VARIANT_ZOMBIE_NAUTILUS,
        biome,
    )
}

impl ZombieNautilus {
    fn variant_name(&self) -> &'static str {
        if self.warm {
            WARM_VARIANT
        } else {
            TEMPERATE_VARIANT
        }
    }
}

impl Species for ZombieNautilus {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.make_tameable(data::SPECIES);
        configure_swimming(core);
        let mut goals = GoalSelector::default();
        goals.add(
            0,
            ChargeAttack::new(0.5, SoundEvent::EntityZombieNautilusDash),
        );
        goals.add(1, Tempt::new(0.9, nautilus_taming_item));
        goals.add(2, FindTarget::default());
        goals.add(5, SwimStroll::new(SWIM_SPEED));
        goals.add(6, RandomLookAround::default());
        core.goals = goals;
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.inner.tick(core, cx);
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let biome = (cx.biome)(super::physics::block_pos(core.base.position));
        self.warm = biome.is_some_and(coral_biome);
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut tuff_nbt::Compound) {
        if let Some(name) = take(nbt, "variant", |t| t.as_str().map(str::to_owned)) {
            self.warm = name == WARM_VARIANT;
        }
        self.inner.load(core, nbt);
    }

    fn save(&self, core: &MobCore, out: &mut tuff_nbt::Compound) {
        out.insert("variant", self.variant_name());
        self.inner.save(core, out);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut values = self.inner.metadata(core);
        values.push((
            ZOMBIE_VARIANT,
            MetadataValue::Holder {
                serializer: core.base.kind.data_serializer(ZOMBIE_VARIANT).unwrap_or(0),
                id: data_registry::ZOMBIE_NAUTILUS_VARIANT
                    .entries
                    .iter()
                    .position(|e| *e == self.variant_name())
                    .unwrap_or(0) as i32,
            },
        ));
        values
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let on_land = !core.body.in_water;
        MobSounds {
            ambient: Some(if on_land {
                SoundEvent::EntityZombieNautilusAmbientLand
            } else {
                SoundEvent::EntityZombieNautilusAmbient
            }),
            hurt: Some(if on_land {
                SoundEvent::EntityZombieNautilusHurtLand
            } else {
                SoundEvent::EntityZombieNautilusHurt
            }),
            death: Some(if on_land {
                SoundEvent::EntityZombieNautilusDeathLand
            } else {
                SoundEvent::EntityZombieNautilusDeath
            }),
            step: None,
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntityZombieNautilusEat)
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        nautilus_food(stack.item)
    }

    fn seats(&self, core: &MobCore) -> usize {
        self.inner.seats(core)
    }

    fn mount_refused(&self, core: &MobCore, stack: Option<&ItemStack>) -> bool {
        self.inner.mount_refused(core, stack)
    }

    fn inventory(&self) -> Option<(&[Option<ItemStack>], MenuKind)> {
        self.inner.inventory()
    }

    fn inventory_mut(&mut self) -> Option<&mut [Option<ItemStack>]> {
        self.inner.inventory_mut()
    }

    fn inventory_opens_for(&self, core: &MobCore, player: i32, sneaking: bool) -> bool {
        self.inner.inventory_opens_for(core, player, sneaking)
    }

    fn drivable(&self, core: &MobCore) -> bool {
        self.inner.drivable(core)
    }

    fn ridden_speed(&self, core: &MobCore) -> Option<tuff_core::world::entity::RiddenSpeed> {
        self.inner.ridden_speed(core)
    }

    fn ride(&mut self, core: &mut MobCore, input: RiderInput, cx: &mut TickContext) -> bool {
        self.inner.ride(core, input, cx)
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        self.inner.interact(core, interaction, cx)
    }

    fn experience(&self, core: &MobCore, rng: &mut StdRng) -> i32 {
        core.xp_reward + core.equipment.experience_bonus(rng)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }

    fn custom_persistence(&self, core: &MobCore) -> bool {
        tamed(core)
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Nautilus, super::load::<Nautilus>);
    registry.register(EntityType::ZombieNautilus, super::load::<ZombieNautilus>);
    registry.register_spawn_rule(EntityType::Nautilus, super::spawning::nautilus_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;

    #[tuff_core::watchdog]
    #[test]
    fn nautilus_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let byte = MetadataValue::Byte(0).type_id() as u8;
        let owner = MetadataValue::OptionalUuid(None).type_id() as u8;
        let layout = EntityType::Nautilus.data_layout();
        assert_eq!(layout.len(), (DASH + 1) as usize);
        assert_eq!(
            layout[data::BABY as usize..],
            [boolean, boolean, byte, owner, boolean]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn zombie_nautilus_data_indices_match_the_dumped_layout() {
        let layout = EntityType::ZombieNautilus.data_layout();
        assert_eq!(layout.len(), (data::SPECIES + 4) as usize);
        assert_eq!(
            layout[DASH as usize],
            MetadataValue::Boolean(false).type_id() as u8
        );
        assert_eq!(
            layout[(data::SPECIES + 3) as usize],
            tuff_vanilla_data::entity::data_serializer::ZOMBIE_NAUTILUS_VARIANT
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_saddled_rider_dashes_once_per_press_and_waits_for_the_cooldown() {
        with_context(|core, cx| {
            Nautilus::configure(core);
            core.equipment.set(
                tuff_vanilla_data::item::EquipmentSlot::Saddle,
                Some(ItemStack::mint(Item::Saddle, 1, MintReason::Test)),
            );
            core.body.in_water = true;
            let mut dash = Dash::default();
            let ahead = look_vector(0.0, 0.0);
            let sounds = (
                SoundEvent::EntityNautilusDash,
                SoundEvent::EntityNautilusDashLand,
            );
            dash.ride(core, true, ahead, sounds, cx);
            assert!(dash.dashing);
            assert!(core.base.velocity[2] > 0.2, "{:?}", core.base.velocity);
            core.base.velocity = [0.0; 3];
            dash.ride(core, true, ahead, sounds, cx);
            assert_eq!(core.base.velocity, [0.0; 3], "holding the key is one dash");
            dash.ride(core, false, ahead, sounds, cx);
            dash.ride(core, true, ahead, sounds, cx);
            assert_eq!(core.base.velocity, [0.0; 3], "still cooling down");
            let ready = (
                SoundEvent::EntityNautilusDashReady,
                SoundEvent::EntityNautilusDashReadyLand,
            );
            for _ in 0..DASH_COOLDOWN {
                dash.tick(core, ready, cx);
            }
            assert!(!dash.dashing);
            dash.ride(core, false, ahead, sounds, cx);
            dash.ride(core, true, ahead, sounds, cx);
            assert!(core.base.velocity[2] > 0.2, "ready again");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_zombie_nautilus_can_be_tamed_and_then_ridden() {
        with_context(|core, cx| {
            ZombieNautilus::configure(core);
            let mut zombie = ZombieNautilus::default();
            assert_eq!(zombie.seats(core), 0);
            let interaction = EntityInteraction {
                player: 1,
                player_uuid: 9,
                position: [0.0; 3],
                location: [0.0; 3],
                stack: Some(tuff_item::item::ItemSnapshot::describe(
                    Item::Pufferfish,
                    1,
                    Default::default(),
                )),
                off_hand: false,
                creative: false,
                sneaking: false,
                riding: false,
                hero: None,
            };
            for _ in 0..200 {
                if tamed(core) {
                    break;
                }
                assert!(zombie.interact(core, &interaction, cx).is_some());
            }
            assert!(tamed(core));
            assert_eq!(zombie.seats(core), 1);
            assert!(!zombie.drivable(core), "needs a saddle");
        });
    }

    fn click(item: Item, sneaking: bool) -> EntityInteraction {
        EntityInteraction {
            player: 1,
            player_uuid: 9,
            position: [0.0; 3],
            location: [0.0; 3],
            stack: Some(tuff_item::item::ItemSnapshot::describe(
                item,
                1,
                Default::default(),
            )),
            off_hand: false,
            creative: false,
            sneaking,
            riding: false,
            hero: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_tamed_nautilus_opens_a_saddle_and_armor_screen_for_a_sneaking_click() {
        with_context(|core, cx| {
            Nautilus::configure(core);
            let mut nautilus = Nautilus::default();
            assert!(!nautilus.inventory_opens_for(core, 1, true), "wild");
            for _ in 0..200 {
                if tamed(core) {
                    break;
                }
                nautilus.interact(core, &click(Item::Pufferfish, false), cx);
            }
            assert!(tamed(core));
            assert!(nautilus.inventory_opens_for(core, 1, true));
            assert!(!nautilus.inventory_opens_for(core, 1, false));
            let (slots, kind) = nautilus.inventory().expect("a screen");
            assert!(slots.is_empty());
            assert_eq!(
                kind,
                MenuKind::Mount {
                    columns: 0,
                    saddle: true,
                    armor: MountArmor::Nautilus,
                }
            );
            nautilus.inventory_mut().unwrap();
            core.equipment.mount_slots_mut()[0].replace(ItemStack::mint(
                Item::Saddle,
                1,
                MintReason::Test,
            ));
            nautilus.tick(core, cx);
            assert!(saddled(core), "a saddle placed in the screen is worn");
            assert!(MountArmor::Nautilus.fits(&ItemStack::mint(
                Item::CopperNautilusArmor,
                1,
                MintReason::Test
            )));
            assert!(!MountArmor::Nautilus.fits(&ItemStack::mint(
                Item::Saddle,
                1,
                MintReason::Test
            )));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_tamed_nautilus_equips_a_held_saddle_instead_of_being_mounted() {
        with_context(|core, cx| {
            Nautilus::configure(core);
            let mut nautilus = Nautilus::default();
            for _ in 0..200 {
                if tamed(core) {
                    break;
                }
                nautilus.interact(core, &click(Item::Pufferfish, false), cx);
            }
            let saddle = ItemStack::mint(Item::Saddle, 1, MintReason::Test);
            assert!(nautilus.mount_refused(core, Some(&saddle)));
            assert!(!nautilus.mount_refused(core, None));
            let outcome = nautilus.interact(core, &click(Item::Saddle, false), cx);
            assert_eq!(outcome, Some(HandEffect::Consume(1)));
            assert!(saddled(core));
            assert!(core.equipment.mount_slots()[0].is_some());
            assert!(!nautilus.mount_refused(core, Some(&saddle)), "slot taken");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_tamed_hurt_nautilus_heals_from_its_food() {
        with_context(|core, cx| {
            Nautilus::configure(core);
            let mut nautilus = Nautilus::default();
            for _ in 0..200 {
                if tamed(core) {
                    break;
                }
                nautilus.interact(core, &click(Item::Pufferfish, false), cx);
            }
            core.health = core.max_health();
            let full = nautilus.interact(core, &click(Item::Pufferfish, false), cx);
            assert_ne!(full, Some(HandEffect::Consume(1)));
            core.health = core.max_health() - 5.0;
            let outcome = nautilus.interact(core, &click(Item::Pufferfish, false), cx);
            assert_eq!(outcome, Some(HandEffect::Consume(1)));
            assert_eq!(core.health, core.max_health() - 3.0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_wild_zombie_nautilus_does_not_hunt_unprovoked() {
        with_context(|core, cx| {
            ZombieNautilus::configure(core);
            core.body.in_water = true;
            let mut find = FindTarget::default();
            assert!(!find.can_use(core, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn zombie_nautilus_variant_follows_the_coral_biome_tag() {
        assert!(coral_biome("minecraft:warm_ocean"));
        assert!(!coral_biome("minecraft:plains"));
        assert!(!coral_biome("minecraft:cold_ocean"));
        with_context(|core, _cx| {
            let mut nautilus = ZombieNautilus::default();
            assert_eq!(nautilus.variant_name(), "minecraft:temperate");
            let mut nbt = tuff_nbt::Compound::new().with("variant", "minecraft:warm");
            nautilus.load(core, &mut nbt);
            assert!(nautilus.warm);
            let mut saved = tuff_nbt::Compound::new();
            nautilus.save(core, &mut saved);
            assert_eq!(
                saved.get("variant").and_then(|t| t.as_str()),
                Some("minecraft:warm")
            );
            let warm = data_registry::ZOMBIE_NAUTILUS_VARIANT
                .entries
                .iter()
                .position(|e| *e == "minecraft:warm")
                .unwrap() as i32;
            let id = nautilus
                .metadata(core)
                .into_iter()
                .find_map(|(index, value)| match value {
                    MetadataValue::Holder { id, .. } if index == ZOMBIE_VARIANT => Some(id),
                    _ => None,
                });
            assert_eq!(id, Some(warm));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_charge_rams_a_bystander_in_the_way_but_not_a_passenger() {
        let at = [0.0, 64.0, 0.0];
        let mut passenger = crate::target::tests::zombie(60, at);
        let bystander = crate::target::tests::zombie(61, at);
        passenger.vehicle = Some(0);
        crate::tests::with_entities(&[passenger, bystander], |core, cx| {
            Nautilus::configure(core);
            core.base.position = at;
            core.base.id = 0;
            let charge = ChargeAttack::new(0.6, SoundEvent::EntityNautilusDash);
            let hit = charge.rammed(core, cx).expect("a bystander is in the box");
            assert_eq!(hit.id, 61);
        });
    }
}
