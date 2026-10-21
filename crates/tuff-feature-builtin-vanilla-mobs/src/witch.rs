//! Witches: they wander, look about, turn on players and whoever hurts
//! them, drink defensive potions on themselves, and throw offensive ones
//! at their target. In a raid they throw healing at hurt raiders.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_item::item::DupUnchecked;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::{EquipmentSlot, ModifierOperation};
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect, Potion, SoundEvent};

use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{Float, LookAtPlayer, RandomLookAround, RandomStroll};
use super::monster;
use super::target::{HurtByTarget, NearestAttackablePlayer, TargetView};
use super::{Damage, MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::effects;
use tuff_core::world::entity::{Culprit, TickContext};
use tuff_core::world::projectile::{self, Launch, Owner, Shot, shot_velocity};
use tuff_item::item::{ItemStack, MintReason, PotionContents};

/// Witch data after the mob flags: celebrating (as a raider), drinking.
const CELEBRATING: u8 = data::BABY;
const DRINKING: u8 = data::BABY + 1;

const DRINKING_SPEED_ID: &str = "minecraft:drinking";
const DRINKING_SPEED_MODIFIER: f64 = -0.25;
/// A fallback for a potion's drink time if it somehow has no consumable
/// component (32 ticks, vanilla's own default).
const DEFAULT_DRINK_TICKS: i32 = 32;

const WATER_BREATHING_CHANCE: f32 = 0.15;
const FIRE_RESISTANCE_CHANCE: f32 = 0.15;
const HEALING_CHANCE: f32 = 0.05;
const SWIFTNESS_CHANCE: f32 = 0.5;
const SWIFTNESS_TARGET_DISTANCE_SQ: f64 = 121.0;

const THROW_SPEED: f64 = 1.0;
const ATTACK_INTERVAL: i32 = 60;
const ATTACK_RADIUS: f32 = 10.0;
const ATTACK_RADIUS_SQ: f32 = ATTACK_RADIUS * ATTACK_RADIUS;
const THROW_UNCERTAINTY: f32 = 8.0;
const CLOSE_RANGE: f64 = 2.0;
const CLOSE_POWER: f32 = 0.45;
const FAR_POWER: f32 = 0.75;
const WEAKNESS_RANGE: f64 = 3.0;
const WEAKNESS_CHANCE: f32 = 0.25;
const SLOWNESS_RANGE: f64 = 8.0;

/// Keeps a target at potion range: closes in while out of range or out of
/// sight, otherwise holds still, and throws a splash potion at its target
/// on a 60-tick cooldown once it can see it. The drinking flag (kept in
/// `MobCore::signal` so the species' own drink timer can set it) skips the
/// throw but not the cooldown reset, matching the game.
#[derive(Debug, Default)]
struct ThrowPotion {
    see_time: i32,
    attack_time: i32,
}

impl Goal for ThrowPotion {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx) || !mob.navigation.is_done()
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.see_time = 0;
        self.attack_time = -1;
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.see_time = 0;
        self.attack_time = -1;
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
        self.see_time = if sees { self.see_time + 1 } else { 0 };
        if distance > f64::from(ATTACK_RADIUS_SQ) || self.see_time < 5 {
            mob.move_to_entity(target.position, THROW_SPEED, cx);
        } else {
            mob.navigation.stop();
        }
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        self.attack_time -= 1;
        if self.attack_time != 0 {
            if self.attack_time < 0 {
                self.attack_time = ATTACK_INTERVAL;
            }
        } else if sees {
            throw_potion(mob, &target, cx);
            self.attack_time = ATTACK_INTERVAL;
        }
    }
}

/// A splash potion of `potion` as a fresh item stack.
fn splash_stack(potion: Potion) -> ItemStack {
    let mut stack = ItemStack::mint(Item::SplashPotion, 1, MintReason::Spawn);
    stack.components.potion_contents = Some(Box::new(PotionContents {
        potion: Some(potion),
        ..Default::default()
    }));
    stack
}

/// Vanilla's throw selection: slows a distant target, weakens a close one a
/// quarter of the time, else harms. The dumped data has no way for a mob to
/// see a player's health or active effects, so the branches that key off
/// those (poison above 4 health, skipping an effect the target already has)
/// are left out rather than guessed; they fall through to the harming
/// default like vanilla does when its own checks fail.
fn choose_thrown_potion(horizontal_distance: f64, rng: &mut StdRng) -> Potion {
    if horizontal_distance >= SLOWNESS_RANGE {
        Potion::Slowness
    } else if horizontal_distance <= WEAKNESS_RANGE && rng.r#gen::<f32>() < WEAKNESS_CHANCE {
        Potion::Weakness
    } else {
        Potion::Harming
    }
}

/// Throws a splash potion at `target`, unless it is mid-drink.
fn throw_potion(mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) {
    if mob.signal != 0 {
        return;
    }
    let dx = target.position[0] - mob.base.position[0];
    let dy = target.eye[1] - 1.100_000_023_841_858 - mob.base.position[1];
    let dz = target.position[2] - mob.base.position[2];
    let horizontal = (dx * dx + dz * dz).sqrt();
    let power = if horizontal <= CLOSE_RANGE {
        CLOSE_POWER
    } else {
        FAR_POWER
    };
    let velocity = shot_velocity(
        [dx, dy + horizontal * 0.200_000_002_980_232_24, dz],
        power,
        THROW_UNCERTAINTY,
        cx.rng,
    );
    let eye = mob.eye_position();
    let start = [eye[0], eye[1] - 0.100_000_001_490_116_12, eye[2]];
    let raider_health = cx
        .entities
        .iter()
        .find(|e| e.id == target.id && super::raider::is_raider(e.kind))
        .and_then(|e| e.living)
        .map(|l| l.health);
    let potion = match raider_health {
        Some(health) if health <= 4.0 => Potion::Healing,
        Some(_) => Potion::Regeneration,
        None => choose_thrown_potion(horizontal, cx.rng),
    };
    if raider_health.is_some() {
        mob.target = None;
    }
    let id = cx.shared.next_entity_id();
    let owner = Owner::of(Culprit::entity(&mob.base), None);
    let shot = Shot::Thrown {
        kind: EntityType::SplashPotion,
        item: splash_stack(potion),
        velocity,
    };
    projectile::launch(
        Launch {
            id,
            position: start,
            owner,
            shot,
        },
        cx,
    );
    if !mob.base.silent {
        let pitch = 0.8 + cx.rng.r#gen::<f32>() * 0.4;
        mob.sound(SoundEvent::EntityWitchThrow, 1.0, pitch, cx);
    }
}

fn heal_cooled_down(core: &MobCore, _cx: &TickContext) -> bool {
    core.raider.as_ref().is_none_or(|r| r.heal_cooldown <= 0)
}

/// In a raid, now and then picks the nearest raider in sight (but other
/// witches) to throw a healing potion at.
#[derive(Debug)]
struct HealRaiders;

const HEAL_INTERVAL: i32 = 500;
const HEAL_COOLDOWN: i32 = 200;

impl Goal for HealRaiders {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.raider.as_ref().is_none_or(|r| r.heal_cooldown > 0)
            || !cx.rng.r#gen::<bool>()
            || !super::raider::has_active_raid(mob, cx)
            || cx
                .rng
                .gen_range(0..super::goal::reduced_delay(HEAL_INTERVAL))
                != 0
        {
            return false;
        }
        let range = mob.follow_range();
        let found = cx
            .entities
            .iter()
            .filter(|e| {
                e.id != mob.base.id
                    && super::raider::is_raider(e.kind)
                    && e.kind != EntityType::Witch
                    && e.living.is_some_and(|l| l.alive)
                    && mob.distance_sq(e.position) <= range * range
            })
            .filter_map(|e| super::target::view(cx, e.id))
            .filter(|t| mob.has_line_of_sight(t, cx))
            .min_by(|a, b| {
                mob.distance_sq(a.position)
                    .total_cmp(&mob.distance_sq(b.position))
            });
        match found {
            Some(target) => {
                mob.target = Some(target.id);
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if let Some(raider) = mob.raider.as_mut() {
            raider.heal_cooldown = super::goal::reduced_delay(HEAL_COOLDOWN);
        }
    }
}

#[derive(Debug, Default)]
pub struct Witch {
    drinking: bool,
    using_time: i32,
}

impl Witch {
    /// Whichever defensive potion it should drink this tick, in vanilla's
    /// checked order (each roll only happens once the ones before it
    /// missed, matching the game's `else if` chain).
    fn pick_potion(core: &MobCore, cx: &mut TickContext) -> Option<Potion> {
        if cx.rng.r#gen::<f32>() < WATER_BREATHING_CHANCE
            && core.body.eyes_in_water
            && !core.effects.has(MobEffect::WaterBreathing)
        {
            return Some(Potion::WaterBreathing);
        }
        if cx.rng.r#gen::<f32>() < FIRE_RESISTANCE_CHANCE
            && core.base.on_fire()
            && !core.effects.has(MobEffect::FireResistance)
        {
            return Some(Potion::FireResistance);
        }
        if cx.rng.r#gen::<f32>() < HEALING_CHANCE && core.health < core.max_health() {
            return Some(Potion::Healing);
        }
        if cx.rng.r#gen::<f32>() < SWIFTNESS_CHANCE
            && !core.effects.has(MobEffect::Speed)
            && core
                .target_view(cx)
                .is_some_and(|t| core.distance_sq(t.position) > SWIFTNESS_TARGET_DISTANCE_SQ)
        {
            return Some(Potion::Swiftness);
        }
        None
    }

    fn start_drinking(&mut self, core: &mut MobCore, potion: Potion, cx: &mut TickContext) {
        let mut stack = ItemStack::mint(Item::Potion, 1, MintReason::Spawn);
        stack.components.potion_contents = Some(Box::new(PotionContents {
            potion: Some(potion),
            ..Default::default()
        }));
        self.using_time = stack
            .item
            .consumable()
            .map_or(DEFAULT_DRINK_TICKS, |c| (c.consume_seconds * 20.0) as i32);
        core.equipment.set(EquipmentSlot::Mainhand, Some(stack));
        self.drinking = true;
        core.signal = 1;
        core.changed(DRINKING);
        if !core.base.silent {
            let pitch = 0.8 + cx.rng.r#gen::<f32>() * 0.4;
            core.sound(SoundEvent::EntityWitchDrink, 1.0, pitch, cx);
        }
        core.set_modifier(
            Attribute::MovementSpeed,
            DRINKING_SPEED_ID,
            Some((DRINKING_SPEED_MODIFIER, ModifierOperation::AddValue)),
        );
    }

    fn finish_drinking(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        self.drinking = false;
        core.signal = 0;
        core.changed(DRINKING);
        let drunk = core
            .equipment
            .get(EquipmentSlot::Mainhand)
            .map(DupUnchecked::dup_unchecked);
        core.equipment.set(EquipmentSlot::Mainhand, None);
        if let Some(stack) = drunk
            && stack.item == Item::Potion
            && let Some(contents) = &stack.components.potion_contents
        {
            let scale = stack.item.potion_duration_scale();
            for effect in effects::potion_effects(contents, scale) {
                if effect.effect.is_instantaneous() {
                    // Only instant effect witches ever drink is Healing.
                    if let Some(effects::EffectAction::Heal(amount)) =
                        effects::instant_action(effect.effect, effect.amplifier, 1.0)
                    {
                        core.heal(amount);
                    }
                } else {
                    core.effects.add(effect);
                }
            }
        }
        core.set_modifier(Attribute::MovementSpeed, DRINKING_SPEED_ID, None);
    }
}

impl Species for Witch {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        let mut goals = GoalSelector::default();
        goals.add(1, HurtByTarget::new(&[]));
        goals.add(1, Float::new(core));
        goals.add(2, ThrowPotion::default());
        goals.add(2, RandomStroll::water_avoiding(1.0));
        goals.add(2, HealRaiders);
        goals.add(
            3,
            NearestAttackablePlayer::with_interval(10, true, None).when(heal_cooled_down),
        );
        goals.add(3, LookAtPlayer::new(8.0));
        goals.add(3, RandomLookAround::default());
        super::raider::add_goals(&mut goals);
        core.goals = goals;
        super::raider::make_raider(core, false, SoundEvent::EntityWitchCelebrate);
    }

    fn scale_damage(&self, core: &MobCore, damage: &Damage, amount: f32) -> f32 {
        let mut amount = amount;
        if damage.attacker.is_some_and(|(id, _)| id == core.base.id) {
            amount = 0.0;
        }
        if damage.is("witch_resistant_to") {
            amount *= 0.15;
        }
        amount
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let Some(raider) = core.raider.as_mut() {
            raider.heal_cooldown -= 1;
        }
        if self.drinking {
            self.using_time -= 1;
            if self.using_time <= 0 {
                self.finish_drinking(core, cx);
            }
        } else if let Some(potion) = Self::pick_potion(core, cx) {
            self.start_drinking(core, potion, cx);
        }
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (CELEBRATING, super::illager::celebrating(core)),
            (DRINKING, MetadataValue::Boolean(self.drinking)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        monster::sounds(
            SoundEvent::EntityWitchAmbient,
            SoundEvent::EntityWitchHurt,
            SoundEvent::EntityWitchDeath,
            None,
        )
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Witch, super::load::<Witch>);
    registry.register_spawn_rule(EntityType::Witch, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn witch_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let layout = EntityType::Witch.data_layout();
        assert_eq!(layout.len(), DRINKING as usize + 1);
        assert_eq!(layout[CELEBRATING as usize..], [boolean, boolean]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn witches_resist_magic_and_ignore_their_own_splashes() {
        use tuff_vanilla_data::damage_type;
        crate::tests::with_context(|core, _| {
            let witch = Witch::default();
            let magic = Damage::of(&damage_type::MAGIC);
            assert!((witch.scale_damage(core, &magic, 10.0) - 1.5).abs() < 1e-6);
            let mut own = Damage::of(&damage_type::INDIRECT_MAGIC);
            own.attacker = Some((core.base.id, EntityType::Witch));
            assert_eq!(witch.scale_damage(core, &own, 10.0), 0.0);
            let fall = Damage::of(&damage_type::FALL);
            assert_eq!(witch.scale_damage(core, &fall, 10.0), 10.0);
        });
    }
}
