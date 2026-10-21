//! Combat rules as pure functions: attribute values from equipment,
//! enchantments and effects; melee damage, crits, sweeps and knockback;
//! enchantment effects evaluated from their data (damage, knockback,
//! protection, post-attack effects); shields, armor, the Resistance effect
//! and difficulty scaling.

use rand::Rng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::damage_type::{self, DamageType, Scaling};
use tuff_vanilla_data::item::{BlocksAttacks, EquipmentSlot};
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect};

use tuff_core::world::damage_type_in_tag;
use tuff_core::world::player::Difficulty;
use tuff_item::item::ItemStack;

use crate::enchant_effects::facts_of;
use tuff_core::world::attributes::applies_in;
pub use tuff_core::world::attributes::{Equipped, Modifiers, knockback, synced_attributes};
use tuff_item::enchantment::{
    self, Conditional, EnchantmentDef, EnchantmentEffects, EntityEffect, HolderSet, Requirement,
    Target, ValueEffect,
};

const ARMOR_DIVIDER: f32 = 25.0;
const MAX_PROTECTION: f32 = 20.0;
const MIN_ARMOR_RATIO: f32 = 0.2;
const BASE_TOUGHNESS: f32 = 2.0;
const FULL_STRENGTH: f32 = 0.9;
const CRIT_MULTIPLIER: f32 = 1.5;
const SPRINT_KNOCKBACK: f32 = 0.5;
/// The fastest a player can move (in multiples of its speed) and still
/// sweep.
const SWEEP_SPEED_FACTOR: f64 = 2.5;
pub const BASE_KNOCKBACK: f64 = 0.4;
/// The push a partially blocked hit gives the shield holder.
pub const SHIELD_KNOCKBACK: f64 = 0.5;
/// The knockback each entity caught by a sweep gets along the attacker's
/// facing.
pub const SWEEP_KNOCKBACK: f32 = 0.4;

/// A mace's smash attack needs more fall distance than this to trigger.
const SMASH_ATTACK_FALL_THRESHOLD: f64 = 1.5;
/// Above this fall distance, the smash sound and Wind Burst's knockback
/// multiplier both step up.
pub const SMASH_ATTACK_HEAVY_THRESHOLD: f64 = 5.0;
/// How far a smash attack's area knockback reaches, and its base strength.
pub const SMASH_KNOCKBACK_RADIUS: f64 = 3.5;
/// Also the fixed upward push a smash's area knockback gives (`0.0,
/// SMASH_KNOCKBACK_POWER, 0.0` on top of the horizontal push).
pub const SMASH_KNOCKBACK_POWER: f64 = 0.7;

/// Armor points (whole) and toughness from equipment.
pub fn armor(equipment: &[Equipped]) -> (f32, f32) {
    let modifiers = Modifiers::equipment(equipment);
    (
        modifiers.value(Attribute::Armor).floor() as f32,
        modifiers.value(Attribute::ArmorToughness) as f32,
    )
}

/// Damage left after armor, which toughness keeps effective against big
/// hits. `weapon` is the item that dealt the hit, whose enchantments
/// (Breach) can reduce the fraction armor stops.
pub fn after_armor(
    damage: f32,
    armor: f32,
    toughness: f32,
    weapon: Option<&ItemStack>,
    context: &DamageContext,
    rng: &mut impl Rng,
) -> f32 {
    let toughness = BASE_TOUGHNESS + toughness / 4.0;
    let effective = (armor - damage / toughness).clamp(armor * MIN_ARMOR_RATIO, MAX_PROTECTION);
    let fraction = effective / ARMOR_DIVIDER;
    let fraction = match weapon {
        Some(weapon) => modify_value(weapon, |e| &e.armor_effectiveness, context, fraction, rng),
        None => fraction,
    }
    .clamp(0.0, 1.0);
    damage * (1.0 - fraction)
}

/// Damage left after enchantment protection points.
pub fn after_protection(damage: f32, protection: f32) -> f32 {
    damage * (1.0 - protection.clamp(0.0, MAX_PROTECTION) / ARMOR_DIVIDER)
}

/// Damage left after the Resistance effect at `amplifier`.
pub fn after_resistance(damage: f32, amplifier: u8) -> f32 {
    let resisted = (amplifier as i32 + 1) * 5;
    (damage * (25 - resisted) as f32 / 25.0).max(0.0)
}

/// A damage amount as the tenths the damage statistics count, when the
/// game counts it at all.
pub fn stat_tenths(amount: f32) -> Option<i32> {
    (amount > 0.0 && amount < 3.402_823_5e37).then(|| (amount * 10.0).round() as i32)
}

/// The durability each armor piece loses from a hit.
pub fn armor_wear(damage: f32) -> u32 {
    damage.max(0.0).div_euclid(4.0).max(1.0) as u32
}

/// A player's damage from a source that scales with difficulty.
pub fn scale_for_difficulty(damage: f32, difficulty: Difficulty) -> f32 {
    match difficulty {
        Difficulty::Peaceful => 0.0,
        Difficulty::Easy => (damage / 2.0 + 1.0).min(damage),
        Difficulty::Normal => damage,
        Difficulty::Hard => damage * 3.0 / 2.0,
    }
}

/// Whether damage of `kind` scales with difficulty for a player hit by
/// something caused by a player (`by_player`) or not.
pub fn scales_with_difficulty(kind: &DamageType, caused_by_living: bool, by_player: bool) -> bool {
    match tuff_core::world::effective_damage_type(kind).scaling {
        Scaling::Always => true,
        Scaling::WhenCausedByLivingNonPlayer => caused_by_living && !by_player,
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

/// The angle in radians between where a shield holder looks (horizontally,
/// by `yaw`) and the direction of the hit's source.
pub fn blocking_angle(holder: [f64; 3], yaw: f32, source: Option<[f64; 3]>) -> f64 {
    let Some(source) = source else {
        return std::f64::consts::PI;
    };
    let yaw = (yaw as f64).to_radians();
    let view = [-yaw.sin(), 0.0, yaw.cos()];
    let (dx, dz) = (source[0] - holder[0], source[2] - holder[2]);
    let length = (dx * dx + dz * dz).sqrt();
    if length < 1.0e-4 {
        return 0.0f64.acos();
    }
    (dx / length * view[0] + dz / length * view[2])
        .clamp(-1.0, 1.0)
        .acos()
}

/// What a raised shield stops of a hit: `None` if the damage type gets
/// past shields, else the damage blocked.
pub fn shield_blocks(
    shield: &BlocksAttacks,
    kind: &DamageType,
    damage: f32,
    angle: f64,
) -> Option<f32> {
    if damage <= 0.0
        || shield
            .bypassed_by
            .is_some_and(|tag| damage_type_in_tag(kind, tag))
    {
        return None;
    }
    Some(shield.blocked_damage(kind.name, damage, angle))
}

/// The direction knockback along an attacker's facing uses.
pub fn facing_knockback_direction(yaw: f32) -> (f64, f64) {
    let yaw = yaw * (std::f32::consts::PI / 180.0);
    (yaw.sin() as f64, -yaw.cos() as f64)
}

/// Fraction of a full attack: 0.0 immediately after the last swing, 1.0
/// once the attack speed's cooldown has elapsed.
pub fn attack_strength(ticks_since_reset: u32, attack_speed: f64) -> f32 {
    let delay = (1.0 / attack_speed * 20.0) as f32;
    ((ticks_since_reset as f32 + 0.5) / delay).clamp(0.0, 1.0)
}

/// A player about to attack.
#[derive(Clone, Copy)]
pub struct Attacker<'a> {
    pub modifiers: Modifiers<'a>,
    pub ticks_since_reset: u32,
    pub fall_distance: f64,
    pub on_ground: bool,
    pub sprinting: bool,
    pub in_water: bool,
    pub climbable: bool,
    /// The horizontal distance squared the client last reported moving in
    /// one tick.
    pub known_speed_sq: f64,
}

impl Attacker<'_> {
    fn weapon(&self) -> Option<&ItemStack> {
        self.modifiers
            .equipment
            .iter()
            .find(|(slot, _)| *slot == EquipmentSlot::Mainhand)
            .map(|(_, stack)| *stack)
    }

    fn has_effect(&self, effect: MobEffect) -> bool {
        self.modifiers.effects.iter().any(|(e, _)| *e == effect)
    }
}

/// What a melee attack does to its target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Melee {
    pub damage: f32,
    pub critical: bool,
    /// Enchantments added damage (the enchanted-hit particles).
    pub magic: bool,
    /// The cooldown had (nearly) run out: a strong hit.
    pub full_strength: bool,
    /// A sprinting full-strength hit, which knocks back harder.
    pub sprint: bool,
    /// Knockback along the attacker's facing on top of the usual push.
    pub knockback: f32,
    /// The attack also sweeps.
    pub sweep: Option<Sweep>,
    /// A mace's smash attack landed: the attacker's fall distance, which
    /// the damage type (`mace_smash`), sounds and area knockback all key
    /// off.
    pub smash: Option<f64>,
}

pub use tuff_core::world::messages::Sweep;

/// A player's melee attack on `victim`, or `None` if it can do no damage
/// at all.
pub fn melee(attacker: &Attacker, victim: EntityType, rng: &mut impl Rng) -> Option<Melee> {
    let modifiers = attacker.modifiers;
    let base = modifiers.value(Attribute::AttackDamage) as f32;
    let strength = attack_strength(
        attacker.ticks_since_reset,
        modifiers.value(Attribute::AttackSpeed),
    );
    let context = DamageContext::melee(victim);
    let weapon = attacker.weapon();
    let enchanted = weapon.map_or(base, |weapon| modify_damage(weapon, &context, base, rng));
    let magic = strength * (enchanted - base);
    let scaled = base * (0.2 + strength * strength * 0.8);
    if scaled <= 0.0 && magic <= 0.0 {
        return None;
    }
    let full_strength = strength > FULL_STRENGTH;
    let sprint = attacker.sprinting && full_strength;
    let critical = full_strength
        && attacker.fall_distance > 0.0
        && !attacker.on_ground
        && !attacker.climbable
        && !attacker.in_water
        && !attacker.has_effect(MobEffect::Blindness)
        && !attacker.sprinting;
    let smashing = attacker.fall_distance > SMASH_ATTACK_FALL_THRESHOLD
        && weapon.is_some_and(|weapon| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::item::key::ENCHANTABLE_MACE,
                weapon.item,
            )
        });
    let smash_bonus = if smashing {
        smash_damage(attacker.fall_distance, weapon, &context, rng)
    } else {
        0.0
    };
    let mut damage = scaled + smash_bonus;
    if critical {
        damage *= CRIT_MULTIPLIER;
    }
    let max_sweep_speed = modifiers.value(Attribute::MovementSpeed) * SWEEP_SPEED_FACTOR;
    let sweeps = full_strength
        && !critical
        && !sprint
        && attacker.on_ground
        && attacker.known_speed_sq < max_sweep_speed * max_sweep_speed
        && weapon.is_some_and(|weapon| {
            tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::SWORDS, weapon.item)
        });
    let sweep = sweeps.then(|| Sweep {
        damage: 1.0 + modifiers.value(Attribute::SweepingDamageRatio) as f32 * damage,
        strength,
    });
    let knockback_attribute = modifiers.value(Attribute::AttackKnockback) as f32;
    let knockback = weapon.map_or(knockback_attribute, |weapon| {
        modify_knockback(weapon, &context, knockback_attribute, rng)
    }) / 2.0
        + if sprint { SPRINT_KNOCKBACK } else { 0.0 };
    Some(Melee {
        damage: damage + magic,
        critical,
        magic: magic > 0.0,
        full_strength,
        sprint,
        knockback,
        sweep,
        smash: smashing.then_some(attacker.fall_distance),
    })
}

/// A mace's smash bonus damage at `fall_distance`: the game's own
/// piecewise curve (not enchantment data -- there is no jar field for it),
/// plus Density's `smash_damage_per_fallen_block` effect per block fallen.
fn smash_damage(
    fall_distance: f64,
    weapon: Option<&ItemStack>,
    context: &DamageContext,
    rng: &mut impl Rng,
) -> f32 {
    let base = if fall_distance <= 3.0 {
        4.0 * fall_distance
    } else if fall_distance <= 8.0 {
        12.0 + 2.0 * (fall_distance - 3.0)
    } else {
        14.0 + fall_distance
    };
    let per_block = weapon.map_or(0.0, |weapon| {
        modify_value(
            weapon,
            |e| &e.smash_damage_per_fallen_block,
            context,
            0.0,
            rng,
        )
    });
    base as f32 + per_block * fall_distance as f32
}

/// Whether a smash attack's area knockback reaches an entity `distance`
/// blocks from the smash, and its power there (`resistance` is the
/// target's knockback resistance): the mace's own formula, radius 3.5,
/// doubled beyond the heavy-smash fall threshold.
pub fn smash_knockback_power(distance: f64, fall_distance: f64, resistance: f64) -> f64 {
    if distance >= SMASH_KNOCKBACK_RADIUS {
        return 0.0;
    }
    let heavy = if fall_distance > SMASH_ATTACK_HEAVY_THRESHOLD {
        2.0
    } else {
        1.0
    };
    ((SMASH_KNOCKBACK_RADIUS - distance) * SMASH_KNOCKBACK_POWER * heavy * (1.0 - resistance))
        .max(0.0)
}

/// The damage a sweep does to one entity it catches.
pub fn sweep_damage(
    weapon: Option<&ItemStack>,
    sweep: Sweep,
    victim: EntityType,
    rng: &mut impl Rng,
) -> f32 {
    let context = DamageContext::melee(victim);
    weapon.map_or(sweep.damage, |weapon| {
        modify_damage(weapon, &context, sweep.damage, rng)
    }) * sweep.strength
}

/// Whether a player at `other` is caught by a sweep of an attacker at
/// `attacker` hitting a player at `target`: its box touches the target's
/// grown by a block sideways and a quarter vertically, and it stands
/// within 3 blocks of the attacker.
pub fn in_sweep(attacker: [f64; 3], target: [f64; 3], other: [f64; 3]) -> bool {
    use tuff_core::world::{PLAYER_HALF_WIDTH, PLAYER_HEIGHT};
    let reach = PLAYER_HALF_WIDTH * 2.0 + 1.0;
    let touches = (other[0] - target[0]).abs() < reach
        && (other[2] - target[2]).abs() < reach
        && other[1] < target[1] + PLAYER_HEIGHT + 0.25
        && other[1] + PLAYER_HEIGHT > target[1] - 0.25;
    touches && tuff_core::world::distance_sq(attacker, other) < 9.0
}

/// Whether a spear's stab from `eye` looking along the unit vector
/// `forward` reaches a player-sized target centred at `target` (its
/// `PLAYER_HEIGHT / 2` point): within the item's `attack_range` measured
/// along the look direction, and no more than a player's half-width plus
/// the range's `hitbox_margin` off to the side. This is a capsule around
/// the look ray, approximating the game's swept entity hit-scan -- it does
/// not trace blocks, so a wall does not stop a stab here.
pub fn in_stab_reach(
    eye: [f64; 3],
    forward: [f64; 3],
    target: [f64; 3],
    range: &tuff_vanilla_data::item::AttackRange,
    creative: bool,
) -> bool {
    let (min, max) = range.reach(creative);
    let to_target = [target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]];
    let along = to_target[0] * forward[0] + to_target[1] * forward[1] + to_target[2] * forward[2];
    if along < ((min - range.hitbox_margin).max(0.0)) as f64
        || along > (max + range.hitbox_margin) as f64
    {
        return false;
    }
    let side = [
        to_target[0] - forward[0] * along,
        to_target[1] - forward[1] * along,
        to_target[2] - forward[2] * along,
    ];
    let side_dist_sq = side[0] * side[0] + side[1] * side[1] + side[2] * side[2];
    let limit = tuff_core::world::PLAYER_HALF_WIDTH + range.hitbox_margin as f64;
    side_dist_sq <= limit * limit
}

/// Who and what a damage-dependent enchantment effect can look at.
#[derive(Debug, Clone, Copy)]
pub struct DamageContext<'a> {
    pub kind: &'a DamageType,
    pub victim: EntityType,
    /// The entity responsible for the damage.
    pub attacker: Option<EntityType>,
    /// The entity that dealt it (a projectile, TNT, or the attacker).
    pub direct_attacker: Option<EntityType>,
    /// The attacker dealt the damage itself.
    pub is_direct: bool,
    /// The direct attacker's fall distance and whether it is flying
    /// (creative flight or an elytra), when known: only the mace's Wind
    /// Burst `minecraft:flags`/`minecraft:movement` requirements read
    /// these, to check its own smash-attack condition.
    pub direct_attacker_fall_distance: Option<f64>,
    pub direct_attacker_flying: Option<bool>,
}

impl DamageContext<'static> {
    /// A player's melee hit on `victim`.
    pub fn melee(victim: EntityType) -> Self {
        Self {
            kind: &damage_type::PLAYER_ATTACK,
            victim,
            attacker: Some(EntityType::Player),
            direct_attacker: Some(EntityType::Player),
            is_direct: true,
            direct_attacker_fall_distance: None,
            direct_attacker_flying: None,
        }
    }
}

impl<'a> DamageContext<'a> {
    /// Damage of `kind` to a player with nothing responsible for it.
    pub fn environment(kind: &'a DamageType) -> Self {
        Self {
            kind,
            victim: EntityType::Player,
            attacker: None,
            direct_attacker: None,
            is_direct: true,
            direct_attacker_fall_distance: None,
            direct_attacker_flying: None,
        }
    }

    /// Records the direct attacker's fall distance and flight state, for
    /// requirements that need them (Wind Burst).
    pub fn with_attacker_state(mut self, fall_distance: f64, flying: bool) -> Self {
        self.direct_attacker_fall_distance = Some(fall_distance);
        self.direct_attacker_flying = Some(flying);
        self
    }
}

/// Each enchantment on `stack` the registry in effect knows, with its level.
fn on_stack<'a>(
    registry: &'a enchantment::EnchantmentRegistry,
    stack: &'a ItemStack,
) -> impl Iterator<Item = (&'a EnchantmentDef, u32)> + 'a {
    enchantment::leveled(registry, stack.components.enchantments.as_ref())
        .map(|(_, def, level)| (def, level))
}

/// Runs a value effect component of every enchantment on `stack` over
/// `value`.
fn modify_value(
    stack: &ItemStack,
    component: impl Fn(&EnchantmentEffects) -> &[Conditional<ValueEffect>],
    context: &DamageContext,
    value: f32,
    rng: &mut impl Rng,
) -> f32 {
    let registry = enchantment::current();
    let mut value = value;
    for (def, level) in on_stack(&registry, stack) {
        for entry in component(&def.effects) {
            if requirements_met(entry.requirements.as_ref(), context, level, rng) {
                value = entry.effect.process(level, rng, value);
            }
        }
    }
    value
}

/// A weapon's damage against `context.victim` with its enchantments'
/// `damage` effects (Sharpness, Smite, ...).
pub fn modify_damage(
    weapon: &ItemStack,
    context: &DamageContext,
    damage: f32,
    rng: &mut impl Rng,
) -> f32 {
    modify_value(weapon, |e| &e.damage, context, damage, rng)
}

/// A weapon's knockback with its enchantments' `knockback` effects.
/// How much ammo a shot of `ammo` from `weapon` uses after its
/// enchantments' `ammo_use` effects (Infinity makes arrows free).
pub fn ammo_use(weapon: &ItemStack, ammo: Item, count: f32) -> f32 {
    let registry = enchantment::current();
    let mut rng = rand::thread_rng();
    let mut value = count;
    for (def, level) in on_stack(&registry, weapon) {
        for entry in &def.effects.ammo_use {
            let applies = enchantment::item_requirement(
                entry.requirements.as_ref(),
                &ItemStack::new(ammo, 1),
                level,
                &mut rng,
            );
            if applies {
                value = entry.effect.process(level, &mut rng, value);
            }
        }
    }
    value
}

/// A victim's equipment drop chance raised by the killing weapon's
/// `equipment_drops` effects aimed from attacker to victim (Looting).
pub fn equipment_drop_chance(
    weapon: &ItemStack,
    context: &DamageContext,
    chance: f32,
    rng: &mut impl Rng,
) -> f32 {
    let registry = enchantment::current();
    let mut chance = chance;
    for (def, level) in on_stack(&registry, weapon) {
        for entry in &def.effects.equipment_drops {
            if entry.enchanted == Target::Attacker
                && entry.affected == Target::Victim
                && requirements_met(entry.requirements.as_ref(), context, level, rng)
            {
                chance = entry.effect.process(level, rng, chance);
            }
        }
    }
    chance
}

pub fn modify_knockback(
    weapon: &ItemStack,
    context: &DamageContext,
    knockback: f32,
    rng: &mut impl Rng,
) -> f32 {
    modify_value(weapon, |e| &e.knockback, context, knockback, rng)
}

/// The protection points enchantments on the equipment give against the
/// damage, from each enchantment's `damage_protection` effects.
pub fn enchantment_protection(
    equipment: &[Equipped],
    context: &DamageContext,
    rng: &mut impl Rng,
) -> f32 {
    let registry = enchantment::current();
    let mut protection = 0.0;
    for (slot, stack) in equipment {
        for (def, level) in on_stack(&registry, stack) {
            if !applies_in(def, *slot) {
                continue;
            }
            for entry in &def.effects.damage_protection {
                if requirements_met(entry.requirements.as_ref(), context, level, rng) {
                    protection = entry.effect.process(level, rng, protection);
                }
            }
        }
    }
    protection
}

/// Whether an enchantment on the equipment makes its wearer immune to the
/// damage, from each enchantment's `damage_immunity` effects (Frost Walker
/// against burning floors).
pub fn immune_to_damage(
    equipment: &[Equipped],
    context: &DamageContext,
    rng: &mut impl Rng,
) -> bool {
    let registry = enchantment::current();
    equipment.iter().any(|(slot, stack)| {
        on_stack(&registry, stack).any(|(def, level)| {
            applies_in(def, *slot)
                && def
                    .effects
                    .damage_immunity
                    .iter()
                    .any(|entry| requirements_met(entry.requirements.as_ref(), context, level, rng))
        })
    })
}

/// Which side of an attack an enchanted item belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Attacker,
    Victim,
}

/// Something a `post_attack` enchantment effect does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PostAttack {
    /// Sets the affected entity on fire for this many seconds.
    Ignite(f32),
    /// Hurts the affected entity; the enchanted item's owner is
    /// responsible.
    Damage {
        kind: &'static DamageType,
        amount: f32,
    },
    /// Wears the enchanted item.
    ItemDamage(u32),
    /// Wind Burst's `minecraft:explode` effect: a block-triggering wind
    /// burst centred `offset` from the attacker that pushes everything near
    /// it by `knockback`.
    Explode {
        radius: f32,
        knockback: f32,
        offset: [f64; 3],
    },
    /// Gives the affected entity a mob effect.
    ApplyMobEffect {
        effect: MobEffect,
        amplifier: u8,
        ticks: i32,
    },
}

/// One post-attack effect to apply: to which side, what, and the slot of
/// the enchanted item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PostAttackEffect {
    pub affected: Side,
    pub slot: EquipmentSlot,
    pub effect: PostAttack,
}

/// The `post_attack` effects of the enchanted items of one side of a hit
/// that landed. `equipment` is that side's equipment (for the attacker,
/// just its main hand).
pub fn post_attack(
    equipment: &[Equipped],
    enchanted: Side,
    context: &DamageContext,
    rng: &mut impl Rng,
) -> Vec<PostAttackEffect> {
    let registry = enchantment::current();
    let mut effects = Vec::new();
    let side = |target: Target| match target {
        Target::Attacker => Some(Side::Attacker),
        Target::Victim => Some(Side::Victim),
        Target::DamagingEntity => None,
    };
    for (slot, stack) in equipment {
        for (def, level) in on_stack(&registry, stack) {
            if !applies_in(def, *slot) {
                continue;
            }
            for entry in &def.effects.post_attack {
                if side(entry.enchanted) != Some(enchanted)
                    || !requirements_met(entry.requirements.as_ref(), context, level, rng)
                {
                    continue;
                }
                let affected = match entry.affected {
                    Target::DamagingEntity if context.is_direct => Some(Side::Attacker),
                    other => side(other),
                };
                let Some(affected) = affected else {
                    continue;
                };
                let mut push = |effect| {
                    effects.push(PostAttackEffect {
                        affected,
                        slot: *slot,
                        effect,
                    })
                };
                entity_effects(&entry.effect, level, rng, &mut push);
            }
        }
    }
    effects
}

fn entity_effects(
    effect: &EntityEffect,
    level: u32,
    rng: &mut impl Rng,
    out: &mut impl FnMut(PostAttack),
) {
    match effect {
        EntityEffect::AllOf(effects) => {
            for effect in effects {
                entity_effects(effect, level, rng, out);
            }
        }
        EntityEffect::Ignite(duration) => out(PostAttack::Ignite(duration.at(level))),
        EntityEffect::DamageEntity {
            damage_type,
            min_damage,
            max_damage,
        } => {
            if let Some(kind) = damage_type::ALL
                .iter()
                .find(|kind| kind.name == damage_type)
            {
                out(PostAttack::Damage {
                    kind,
                    amount: random_between(rng, min_damage.at(level), max_damage.at(level)),
                });
            }
        }
        EntityEffect::ChangeItemDamage(amount) => {
            out(PostAttack::ItemDamage(amount.at(level).max(0.0) as u32))
        }
        EntityEffect::Explode {
            radius,
            knockback_multiplier: Some(knockback),
            offset,
            ..
        } => out(PostAttack::Explode {
            radius: radius.at(level),
            knockback: knockback.at(level),
            offset: *offset,
        }),
        EntityEffect::ApplyMobEffect {
            to_apply,
            min_duration,
            max_duration,
            min_amplifier,
            max_amplifier,
        } => {
            if let Some(effect) = mob_effect_choice(to_apply, rng) {
                let ticks = (random_between(rng, min_duration.at(level), max_duration.at(level))
                    * 20.0)
                    .round() as i32;
                let amplifier =
                    random_between(rng, min_amplifier.at(level), max_amplifier.at(level))
                        .round()
                        .max(0.0) as u8;
                out(PostAttack::ApplyMobEffect {
                    effect,
                    amplifier,
                    ticks,
                });
            }
        }
        _ => {}
    }
}

/// `to_apply` of an `apply_mob_effect` effect: one mob effect, or one of
/// several picked at random.
fn mob_effect_choice(set: &HolderSet, rng: &mut impl Rng) -> Option<MobEffect> {
    match set {
        HolderSet::Ids(ids) if ids.len() == 1 => MobEffect::from_name(&ids[0]),
        HolderSet::Ids(ids) if !ids.is_empty() => {
            MobEffect::from_name(&ids[rng.gen_range(0..ids.len())])
        }
        _ => None,
    }
}

fn random_between(rng: &mut impl Rng, min: f32, max: f32) -> f32 {
    if min >= max {
        min
    } else {
        rng.r#gen::<f32>() * (max - min) + min
    }
}

fn requirements_met(
    requirement: Option<&Requirement>,
    context: &DamageContext,
    level: u32,
    rng: &mut impl Rng,
) -> bool {
    let Some(requirement) = requirement else {
        return true;
    };
    let victim = facts_of(context.victim, [0.0; 3]);
    let attacker = context.attacker.map(|kind| facts_of(kind, [0.0; 3]));
    let direct = context.direct_attacker.map(|kind| {
        let mut facts = facts_of(kind, [0.0; 3]);
        if let Some(flying) = context.direct_attacker_flying {
            facts.flags.flying = flying;
        }
        if context.direct_attacker_fall_distance.is_some() {
            facts.movement = Some([0.0; 3]);
            facts.fall_distance = context.direct_attacker_fall_distance;
        }
        facts
    });
    let damage = crate::enchant_effects::damage_facts(
        context.kind,
        attacker.clone(),
        if context.is_direct {
            attacker.clone()
        } else {
            direct.clone()
        },
    );
    let mut world = crate::enchant_effects::EffectWorld::new(rng);
    world.attacker = attacker.as_ref();
    world.direct_attacker = direct.as_ref();
    world.damage = damage.as_ref();
    crate::enchant_effects::holds(
        Some(requirement),
        Some(&victim),
        victim.position,
        None,
        level,
        &world,
    )
}

#[cfg(test)]
mod tests;
