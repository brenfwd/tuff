//! Hoglins and zoglins.
//!
//! Hoglins hunt players they see, throw what they hit into the air, call
//! the adult hoglins around them when hurt, breed on crimson fungus, and
//! flee piglins that outnumber them (babies flee whoever hurt them), and
//! keep away from warped fungus, portals and respawn anchors (which calm
//! them for ten seconds). Outside the nether they turn into zoglins after
//! fifteen seconds. Zoglins attack anything living but creepers and other
//! zoglins.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};

use super::attack::MeleeAttack;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{Breed, Float, FollowParent, LookAtPlayer, RandomLookAround, RandomStroll};
use super::target::{HurtByTarget, NearestAttackablePlayer, TargetView};
use super::{MobCore, MobSounds, Species, data, monster};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{Effect, EntityHit, Notice, TickContext, take_bool, take_number};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const IMMUNE_TO_ZOMBIFICATION: u8 = data::SPECIES;
const ZOGLIN_BABY: u8 = data::BABY;
const ATTACK_EVENT: i8 = 4;
const CONVERSION_TICKS: i32 = 300;
const PACIFIED_TICKS: i32 = 200;
const REPELLENT_RANGE: i32 = 8;
const REPELLENT_RANGE_Y: i32 = 4;
const SENSOR_INTERVAL: u64 = 20;
const ADULT_ATTACK_INTERVAL: i32 = 40;
const BABY_ATTACK_INTERVAL: i32 = 15;
const BABY_ATTACK_DAMAGE: f64 = 0.5;
const ADULT_ATTACK_DAMAGE: f64 = 6.0;
const BABY_CHANCE: f32 = 0.2;
const ADULT_XP: i32 = 5;
const BABY_XP: i32 = 3;
const RETREAT_TICKS: std::ops::RangeInclusive<u64> = 100..=400;
const RETREAT_DISTANCE: f64 = 15.0;
const RETREAT_SPEED: f64 = 1.3;
const PIGLIN_AVOID_RANGE: f64 = 8.0;
const PIGLIN_AVOID_SPEED: f64 = 0.4;

const HURT_TARGET_TICKS: u64 = 200;

fn attack_interval(mob: &MobCore) -> i32 {
    if mob.is_baby() {
        BABY_ATTACK_INTERVAL
    } else {
        ADULT_ATTACK_INTERVAL
    }
}

/// The hit of a hoglin or zoglin: an adult's damage is half its attack
/// plus a random part of it, and it throws its victim up and away.
fn throw_hook(mob: &MobCore, target: &TargetView, hit: &mut EntityHit, cx: &mut TickContext) {
    hit.knockback = None;
    mob.entity_event(ATTACK_EVENT, cx);
    let sound = if mob.kind() == EntityType::Zoglin {
        SoundEvent::EntityZoglinAttack
    } else {
        SoundEvent::EntityHoglinAttack
    };
    let pitch = mob.voice_pitch(cx.rng);
    mob.sound(sound, 1.0, pitch, cx);
    if mob.kind() == EntityType::Hoglin
        && target.kind == EntityType::Piglin
        && piglins_outnumber_hoglins(mob, cx)
    {
        let retreat = Notice::Retreat { from: target.id };
        cx.effects.push(Effect::Notify {
            target: mob.base.id,
            notice: retreat,
        });
        for other in super::piglin::visible_adults(mob, cx, |e| e.kind == EntityType::Hoglin) {
            cx.effects.push(Effect::Notify {
                target: other,
                notice: retreat,
            });
        }
    }
    let attack = hit.amount;
    if mob.is_baby() || attack as i32 <= 0 {
        return;
    }
    hit.amount = attack / 2.0 + cx.rng.gen_range(0..attack as i32) as f32;
    let power = mob.attribute(Attribute::AttackKnockback);
    if power <= 0.0 {
        return;
    }
    let angle = (cx.rng.gen_range(0..21) - 10) as f32;
    let scale = power * f64::from(cx.rng.r#gen::<f32>() * 0.5 + 0.2);
    let vertical = power * f64::from(cx.rng.r#gen::<f32>()) * 0.5;
    let (dx, dz) = (
        target.position[0] - mob.base.position[0],
        target.position[2] - mob.base.position[2],
    );
    let length = (dx * dx + dz * dz).sqrt();
    let (x, z) = if length < 1.0e-4 {
        (0.0, 0.0)
    } else {
        (dx / length * scale, dz / length * scale)
    };
    let (sin, cos) = (f64::from(angle.sin()), f64::from(angle.cos()));
    hit.push = Some([x * cos + z * sin, vertical, z * cos - x * sin]);
    hit.push_power = Some(power);
}

/// Walks away from the repellent it noticed (`MobCore::avoid`).
#[derive(Debug, Default)]
pub(crate) struct AvoidRepellent;

impl Goal for AvoidRepellent {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.avoid.is_some() && mob.navigation.is_done()
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.avoid.is_some() && !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(repellent) = mob.avoid else {
            return;
        };
        let from = [
            f64::from(repellent.x) + 0.5,
            f64::from(repellent.y),
            f64::from(repellent.z) + 0.5,
        ];
        if mob.distance_sq(from) >= f64::from(REPELLENT_RANGE * REPELLENT_RANGE) {
            return;
        }
        for _ in 0..10 {
            if let Some(pos) = super::random_pos::land_pos_away(mob, 16, 7, from, cx) {
                mob.move_to_position(pos, 1.0, cx);
                return;
            }
        }
    }
}

/// The nearest adult piglin in sight closer than eight blocks, for an
/// idle adult hoglin to walk away from.
fn piglin_to_avoid(mob: &MobCore, cx: &TickContext) -> Option<[f64; 3]> {
    if mob.is_baby() || mob.target.is_some() {
        return None;
    }
    super::piglin::visible_adults(mob, cx, |e| e.kind == EntityType::Piglin)
        .into_iter()
        .filter_map(|id| cx.entities.iter().find(|e| e.id == id))
        .map(|e| e.position)
        .filter(|&p| mob.distance_sq(p) < PIGLIN_AVOID_RANGE * PIGLIN_AVOID_RANGE)
        .min_by(|&a, &b| mob.distance_sq(a).total_cmp(&mob.distance_sq(b)))
}

/// An idle adult hoglin slowly walks away from piglins near it.
#[derive(Debug, Default)]
struct AvoidPiglin;

impl Goal for AvoidPiglin {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.navigation.is_done() && piglin_to_avoid(mob, cx).is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_none() && !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(from) = piglin_to_avoid(mob, cx) else {
            return;
        };
        for _ in 0..10 {
            if let Some(pos) = super::random_pos::land_pos_away(mob, 16, 7, from, cx) {
                mob.move_to_position(pos, PIGLIN_AVOID_SPEED, cx);
                return;
            }
        }
    }
}

/// More adult piglins in sight than adult hoglins, itself included; never
/// for a baby.
fn piglins_outnumber_hoglins(mob: &MobCore, cx: &TickContext) -> bool {
    if mob.is_baby() {
        return false;
    }
    let visible = |kind| super::piglin::visible_adults(mob, cx, |e| e.kind == kind).len();
    visible(EntityType::Piglin) > visible(EntityType::Hoglin) + 1
}

fn adult(mob: &MobCore, _cx: &TickContext) -> bool {
    !mob.is_baby()
}

/// Runs from a piglin (or, for a baby, whoever hurt it) for five to
/// twenty seconds; an adult stops once piglins no longer outnumber its
/// kind.
#[derive(Debug, Default)]
struct Retreat {
    pending: Option<i32>,
    from: Option<i32>,
    until: u64,
}

impl Retreat {
    fn take_pending(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(from) = self.pending.take() {
            self.from = Some(from);
            self.until = cx.tick + cx.rng.gen_range(RETREAT_TICKS);
            mob.target = None;
            mob.navigation.stop();
        }
    }
}

impl Goal for Retreat {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.take_pending(mob, cx);
        self.from.is_some() && cx.tick < self.until
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.take_pending(mob, cx);
        if (cx.tick + mob.base.id as u64).is_multiple_of(SENSOR_INTERVAL)
            && !mob.is_baby()
            && !piglins_outnumber_hoglins(mob, cx)
        {
            self.from = None;
        }
        self.from.is_some() && cx.tick < self.until
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.from = None;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.target = None;
        let Some(from) = self
            .from
            .and_then(|id| cx.entities.iter().find(|e| e.id == id))
            .map(|e| e.position)
        else {
            return;
        };
        if mob.distance_sq(from) >= RETREAT_DISTANCE * RETREAT_DISTANCE || !mob.navigation.is_done()
        {
            return;
        }
        if let Some(away) = super::random_pos::pos_away(mob, 16, 7, from, cx) {
            let path = mob.create_path(super::physics::block_pos(away), 0, false, cx);
            mob.move_along(path, RETREAT_SPEED, cx);
        }
    }
}

fn start_retreat(core: &mut MobCore, from: i32) {
    if let Some(retreat) = core.goals.find_mut::<Retreat>() {
        retreat.pending = Some(from);
    }
}

fn nearest_repellent(mob: &MobCore, cx: &TickContext) -> Option<BlockPos> {
    nearest_block(mob, cx, |s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::HOGLIN_REPELLENTS,
            s.block(),
        )
    })
}

/// The nearest block matching `wanted` within eight blocks (four up or
/// down), by block steps.
pub(crate) fn nearest_block(
    mob: &MobCore,
    cx: &TickContext,
    wanted: impl Fn(tuff_vanilla_data::BlockState) -> bool,
) -> Option<BlockPos> {
    let origin = super::physics::block_pos(mob.base.position);
    let mut best: Option<(i32, BlockPos)> = None;
    for dx in -REPELLENT_RANGE..=REPELLENT_RANGE {
        for dz in -REPELLENT_RANGE..=REPELLENT_RANGE {
            for dy in -REPELLENT_RANGE_Y..=REPELLENT_RANGE_Y {
                let distance = dx.abs() + dy.abs() + dz.abs();
                if best.is_some_and(|(d, _)| d <= distance) {
                    continue;
                }
                let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                if (cx.block)(pos).is_some_and(&wanted) {
                    best = Some((distance, pos));
                }
            }
        }
    }
    best.map(|(_, pos)| pos)
}

fn calm(mob: &MobCore, _cx: &TickContext) -> bool {
    mob.signal <= 0 && !mob.love.as_ref().is_some_and(|l| l.is_in_love())
}

fn hoglin_walk_value(mob: &MobCore, pos: BlockPos, cx: &TickContext) -> f32 {
    if mob.avoid.is_some_and(|r| {
        let d = [pos.x - r.x, pos.y - r.y, pos.z - r.z].map(f64::from);
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 64.0
    }) {
        return -1.0;
    }
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    if (cx.block)(below).is_some_and(|s| s.block() == tuff_vanilla_data::Block::CrimsonNylium) {
        10.0
    } else {
        0.0
    }
}

/// Whether hoglins and piglins turn into their zombie kinds where the mob
/// is (outside the nether).
pub fn zombifies_here(cx: &TickContext) -> bool {
    cx.shared
        .dimensions
        .get(cx.dimension)
        .environment()
        .piglins_zombify
}

#[derive(Debug, Default)]
pub struct Hoglin {
    immune_to_zombification: bool,
    time_in_overworld: i32,
    cannot_be_hunted: bool,
    activity: Activity,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Activity {
    #[default]
    Idle,
    Fight,
    Avoid,
}

impl Activity {
    fn of(core: &MobCore) -> Self {
        if core.target.is_some() {
            Self::Fight
        } else if core
            .goals
            .find::<Retreat>()
            .is_some_and(|r| r.from.is_some())
        {
            Self::Avoid
        } else {
            Self::Idle
        }
    }

    fn sound(self, core: &MobCore) -> SoundEvent {
        match self {
            Self::Avoid => SoundEvent::EntityHoglinRetreat,
            Self::Fight => SoundEvent::EntityHoglinAngry,
            Self::Idle if core.avoid.is_some() => SoundEvent::EntityHoglinRetreat,
            Self::Idle => SoundEvent::EntityHoglinAmbient,
        }
    }
}

impl MobCore {
    fn set_hoglin_baby_stats(&mut self) {
        let (damage, xp) = if self.is_baby() {
            (BABY_ATTACK_DAMAGE, BABY_XP)
        } else {
            (ADULT_ATTACK_DAMAGE, ADULT_XP)
        };
        self.set_attribute_base(Attribute::AttackDamage, damage);
        self.xp_reward = xp;
    }
}

impl Species for Hoglin {
    fn brain_state(&self, core: &MobCore) -> crate::brain_debug::BrainState {
        let mut state = crate::brain_debug::BrainState::default();
        if let Some(from) = core.goals.find::<Retreat>().and_then(|r| r.from) {
            state.activity = Some("avoid");
            state.memories.push(format!("avoid_target: #{from}"));
        }
        if core.signal > 0 {
            state.memories.push("pacified: true".to_owned());
        }
        state
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.hostile = true;
        core.xp_reward = ADULT_XP;
        core.attack_hook = Some(throw_hook);
        core.walk_value = Some(hoglin_walk_value);
        let mut goals = GoalSelector::default();
        goals.add(
            1,
            HurtByTarget::new(&[EntityType::Hoglin])
                .expiring(HURT_TARGET_TICKS)
                .alerting(&[EntityType::Hoglin])
                .when(adult),
        );
        goals.add(0, Retreat::default());
        goals.add(2, NearestAttackablePlayer::new(true).when(calm));
        goals.add(0, Float::new(core));
        goals.add(1, AvoidRepellent);
        goals.add(
            2,
            MeleeAttack::new(1.0, true)
                .with_interval(attack_interval)
                .gives_up_unreachable(),
        );
        goals.add(3, Breed::new(0.6).brain());
        goals.add(4, AvoidPiglin);
        goals.add(5, FollowParent::new(0.6));
        goals.add(6, RandomStroll::water_avoiding(0.4));
        goals.add(7, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.immune_to_zombification = take_bool(nbt, "IsImmuneToZombification").unwrap_or(false);
        self.time_in_overworld = take_number(nbt, "TimeInOverworld").map_or(0, |v| v as i32);
        self.cannot_be_hunted = take_bool(nbt, "CannotBeHunted").unwrap_or(false);
        core.set_hoglin_baby_stats();
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("IsImmuneToZombification", self.immune_to_zombification);
        out.insert("TimeInOverworld", self.time_in_overworld);
        out.insert("CannotBeHunted", self.cannot_be_hunted);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(
            IMMUNE_TO_ZOMBIFICATION,
            MetadataValue::Boolean(self.immune_to_zombification),
        )]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let ambient = if core.avoid.is_some() {
            SoundEvent::EntityHoglinRetreat
        } else if core.target.is_some() {
            SoundEvent::EntityHoglinAngry
        } else {
            SoundEvent::EntityHoglinAmbient
        };
        monster::sounds(
            ambient,
            SoundEvent::EntityHoglinHurt,
            SoundEvent::EntityHoglinDeath,
            Some(SoundEvent::EntityHoglinStep),
        )
    }

    fn can_fall_in_love(&self, core: &MobCore) -> bool {
        core.avoid.is_none()
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::HOGLIN_FOOD, stack.item())
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.signal > 0 {
            core.signal -= 1;
        }
        if (cx.tick + core.base.id as u64).is_multiple_of(SENSOR_INTERVAL) {
            core.avoid = nearest_repellent(core, cx);
        }
        if core.avoid.is_some() && core.signal <= 0 {
            core.signal = PACIFIED_TICKS;
            core.target = None;
        }
        if core.is_baby() != (core.attribute_base(Attribute::AttackDamage) == BABY_ATTACK_DAMAGE) {
            core.set_hoglin_baby_stats();
        }
        let converting =
            !self.immune_to_zombification && !core.no_ai && zombifies_here(cx) && core.is_alive();
        if !converting {
            self.time_in_overworld = 0;
            return;
        }
        self.time_in_overworld += 1;
        if self.time_in_overworld > CONVERSION_TICKS {
            let pitch = core.voice_pitch(cx.rng);
            core.sound(SoundEvent::EntityHoglinConvertedToZombified, 1.0, pitch, cx);
            let mut zoglin = super::convert::<Zoglin>(core, EntityType::Zoglin, true, cx);
            zoglin.species.baby = core.is_baby();
            zoglin.core.set_zoglin_baby(zoglin.species.baby);
            zoglin.core.changed.clear();
            zoglin
                .core
                .add_effect(tuff_core::world::effects::ActiveEffect::timed(
                    tuff_vanilla_data::registry::MobEffect::Nausea,
                    super::CONVERSION_NAUSEA,
                    0,
                ));
            cx.effects
                .push(tuff_core::world::entity::Effect::Spawn(Box::new(zoglin)));
            core.discarded = true;
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let activity = Activity::of(core);
        if activity != self.activity {
            self.activity = activity;
            let pitch = core.voice_pitch(cx.rng);
            core.sound(activity.sound(core), 1.0, pitch, cx);
        }
        let aggressive = core.target.is_some();
        core.set_aggressive(aggressive);
    }

    fn heard(&mut self, core: &mut MobCore, notice: Notice) {
        if let Notice::Retreat { from } = notice
            && !core.is_baby()
            && core.is_alive()
        {
            start_retreat(core, from);
        }
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &super::Damage, _cx: &mut TickContext) {
        if core.is_baby()
            && let Some((attacker, _)) = damage.attacker
        {
            start_retreat(core, attacker);
        }
        core.signal = 0;
        if let Some(love) = &mut core.love {
            love.reset();
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if cx.rng.r#gen::<f32>() < BABY_CHANCE {
            core.set_baby(true);
        }
        core.set_hoglin_baby_stats();
    }

    fn offspring(&self, _partner: &Self, _rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn experience(&self, core: &MobCore, rng: &mut StdRng) -> i32 {
        core.xp_reward + core.equipment.experience_bonus(rng)
    }
}

/// Whether a zoglin goes for an entity of this type.
fn zoglin_prey(kind: EntityType) -> bool {
    !matches!(kind, EntityType::Zoglin | EntityType::Creeper)
}

/// Now and then picks the nearest living entity in sight (players
/// included) that a zoglin attacks.
#[derive(Debug, Default)]
struct NearestLivingPrey {
    found: Option<i32>,
}

impl Goal for NearestLivingPrey {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.target.is_some() || cx.rng.gen_range(0..super::goal::reduced_delay(10)) != 0 {
            return false;
        }
        let range = mob.follow_range();
        let players = cx.players.iter().map(|p| p.id);
        let entities = cx.entities.iter().map(|e| e.id);
        self.found = players
            .chain(entities)
            .filter_map(|id| super::target::view(cx, id))
            .filter(|t: &TargetView| {
                zoglin_prey(t.kind)
                    && mob.can_attack(t, cx)
                    && mob.distance_sq(t.position) <= range * range
                    && mob.has_line_of_sight(t, cx)
            })
            .min_by(|a, b| {
                mob.distance_sq(a.position)
                    .total_cmp(&mob.distance_sq(b.position))
            })
            .map(|t| t.id);
        self.found.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let range = mob.follow_range();
        mob.target_view(cx)
            .is_some_and(|t| mob.can_attack(&t, cx) && mob.distance_sq(t.position) <= range * range)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = self.found;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = None;
    }
}

#[derive(Debug, Default)]
pub struct Zoglin {
    baby: bool,
}

impl MobCore {
    fn set_zoglin_baby(&mut self, baby: bool) {
        self.set_baby(baby);
        if baby {
            self.set_attribute_base(Attribute::AttackDamage, BABY_ATTACK_DAMAGE);
        }
    }
}

impl Species for Zoglin {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.attack_hook = Some(throw_hook);
        let mut goals = GoalSelector::default();
        goals.add(1, HurtByTarget::new(&[]).expiring(HURT_TARGET_TICKS));
        goals.add(2, NearestLivingPrey::default());
        goals.add(
            2,
            MeleeAttack::new(1.0, true)
                .with_interval(attack_interval)
                .gives_up_unreachable(),
        );
        goals.add(6, RandomStroll::water_avoiding(0.4));
        goals.add(7, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.baby = take_bool(nbt, "IsBaby").unwrap_or(false);
        core.set_zoglin_baby(self.baby);
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        if core.is_baby() {
            out.insert("IsBaby", true);
        }
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(ZOGLIN_BABY, MetadataValue::Boolean(core.is_baby()))]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let ambient = if core.target.is_some() {
            SoundEvent::EntityZoglinAngry
        } else {
            SoundEvent::EntityZoglinAmbient
        };
        monster::sounds(
            ambient,
            SoundEvent::EntityZoglinHurt,
            SoundEvent::EntityZoglinDeath,
            Some(SoundEvent::EntityZoglinStep),
        )
    }

    fn ai_step(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        let aggressive = core.target.is_some();
        core.set_aggressive(aggressive);
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if cx.rng.r#gen::<f32>() < BABY_CHANCE {
            self.baby = true;
            core.set_zoglin_baby(true);
        }
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Hoglin, super::load::<Hoglin>);
    registry.register(EntityType::Zoglin, super::load::<Zoglin>);
    registry.register_spawn_rule(EntityType::Hoglin, not_on_nether_wart);
}

/// Hoglins never spawn on nether wart blocks.
fn not_on_nether_wart(
    _kind: EntityType,
    level: &super::spawning::SpawnLevel,
    pos: BlockPos,
    _reason: super::spawning::SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    !(level.block)(BlockPos::new(pos.x, pos.y - 1, pos.z))
        .is_some_and(|s| s.block() == tuff_vanilla_data::Block::NetherWartBlock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn hoglin_data_indices_match_the_dumped_layouts() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let hoglin = EntityType::Hoglin.data_layout();
        assert_eq!(hoglin.len(), IMMUNE_TO_ZOMBIFICATION as usize + 1);
        assert_eq!(hoglin[IMMUNE_TO_ZOMBIFICATION as usize], boolean);
        let zoglin = EntityType::Zoglin.data_layout();
        assert_eq!(zoglin.len(), ZOGLIN_BABY as usize + 1);
        assert_eq!(zoglin[ZOGLIN_BABY as usize], boolean);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_hoglin_near_repellent_will_not_fall_in_love() {
        crate::tests::with_context(|core, _| {
            let hoglin = Hoglin::default();
            assert!(hoglin.can_fall_in_love(core));
            core.avoid = Some(BlockPos::new(1, 64, 1));
            assert!(!hoglin.can_fall_in_love(core));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_throw_loses_power_by_the_victims_knockback_resistance() {
        use tuff_core::world::entity::RegionEntity;
        let throw = |resistance: f64| {
            let mut velocity = [0.0; 3];
            crate::tests::with_context(|_, cx| {
                let base = tuff_core::world::entity::EntityBase::new(
                    EntityType::Zombie,
                    5,
                    5,
                    [0.5, 64.0, 0.5],
                    [0.0; 3],
                );
                let mut victim = crate::Mob::<crate::zombie::Zombie>::new(base, cx.rng);
                victim
                    .core
                    .set_attribute_base(Attribute::KnockbackResistance, resistance);
                let mut hit = EntityHit::new(1.0, &tuff_vanilla_data::damage_type::MOB_ATTACK);
                hit.push = Some([0.4, 0.3, 0.2]);
                hit.push_power = Some(1.0);
                victim.hit(&hit, cx);
                velocity = victim.core.base.velocity;
            });
            velocity
        };
        assert!((throw(0.0)[1] - 0.3).abs() < 1.0e-9);
        assert!((throw(0.5)[1] - 0.15).abs() < 1.0e-9);
        assert!(throw(1.0)[1].abs() < 1.0e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn idle_adults_walk_away_from_a_nearby_piglin_only() {
        use crate::piglin::tests::summary;
        let near = [summary(2, EntityType::Piglin, 5.5, 0, true)];
        crate::tests::with_entities(&near, |core, cx| {
            assert_eq!(piglin_to_avoid(core, cx), Some([5.5, 64.0, 0.5]));
            assert!(AvoidPiglin.can_use(core, cx));
            core.target = Some(9);
            assert_eq!(piglin_to_avoid(core, cx), None, "not while fighting");
            core.target = None;
            core.baby = true;
            assert_eq!(piglin_to_avoid(core, cx), None, "babies stay");
        });
        let others = [
            summary(2, EntityType::Piglin, 9.5, 0, true),
            summary(3, EntityType::Piglin, 3.5, -100, true),
            summary(4, EntityType::PiglinBrute, 3.5, 0, true),
        ];
        crate::tests::with_entities(&others, |core, cx| {
            assert_eq!(piglin_to_avoid(core, cx), None);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn entering_a_fight_plays_the_angry_sound_once() {
        crate::tests::with_context(|core, cx| {
            let mut hoglin = Hoglin::default();
            hoglin.ai_step(core, cx);
            assert_eq!(hoglin.activity, Activity::Idle);
            core.target = Some(9);
            hoglin.ai_step(core, cx);
            assert_eq!(hoglin.activity, Activity::Fight);
            assert_eq!(Activity::Fight.sound(core), SoundEvent::EntityHoglinAngry);
            core.target = None;
            hoglin.ai_step(core, cx);
            assert_eq!(hoglin.activity, Activity::Idle);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_hurt_target_is_dropped_after_two_hundred_ticks() {
        use crate::piglin::tests::summary;
        let attacker = [summary(2, EntityType::Zombie, 3.5, 0, true)];
        crate::tests::with_entities(&attacker, |core, cx| {
            core.last_hurt_by_mob = Some((2, cx.tick));
            let mut goal = HurtByTarget::new(&[]).expiring(HURT_TARGET_TICKS);
            goal.start(core, cx);
            assert_eq!(goal.expires_at, cx.tick + HURT_TARGET_TICKS);
            assert!(goal.can_continue(core, cx));
            goal.expires_at = cx.tick;
            assert!(!goal.can_continue(core, cx));
        });
    }
}
