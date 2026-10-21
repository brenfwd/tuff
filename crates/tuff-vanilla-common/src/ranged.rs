//! What enchantments do to ranged weapons, read from their effect data:
//! crossbow charge time and sounds, Multishot's spread, Piercing, a
//! trident's Loyalty, Riptide and Channeling; and the shot directions of a
//! multishot volley.

use tuff_vanilla_data::registry::SoundEvent;

use tuff_item::enchantment::{
    self, Conditional, EnchantmentDef, EnchantmentEffects, EntityEffect, HolderSet, SoundRef,
    ValueEffect,
};
use tuff_item::item::ItemStack;

/// A crossbow's charge time in seconds before enchantments.
const CROSSBOW_CHARGE_SECONDS: f32 = 1.25;

fn on_stack<'a>(
    registry: &'a enchantment::EnchantmentRegistry,
    stack: &'a ItemStack,
) -> impl Iterator<Item = (&'a EnchantmentDef, u32)> + 'a {
    enchantment::leveled(registry, stack.components.enchantments.as_ref())
        .map(|(_, def, level)| (def, level))
}

/// Runs a list-shaped value component whose entries need nothing of the
/// world over `value`.
fn item_value(
    stack: &ItemStack,
    component: impl Fn(&EnchantmentEffects) -> &[Conditional<ValueEffect>],
    value: f32,
) -> f32 {
    let registry = enchantment::current();
    let mut rng = rand::thread_rng();
    let mut value = value;
    for (def, level) in on_stack(&registry, stack) {
        for entry in component(&def.effects) {
            if entry.requirements.is_none() {
                value = entry.effect.process(level, &mut rng, value);
            }
        }
    }
    value
}

/// Runs a component that is a single value effect over `value`.
fn item_value_direct(
    stack: &ItemStack,
    component: impl Fn(&EnchantmentEffects) -> Option<&ValueEffect>,
    value: f32,
) -> f32 {
    let registry = enchantment::current();
    let mut rng = rand::thread_rng();
    let mut value = value;
    for (def, level) in on_stack(&registry, stack) {
        if let Some(effect) = component(&def.effects) {
            value = effect.process(level, &mut rng, value);
        }
    }
    value
}

/// The entry of a per-level list component on the stack's highest-level
/// enchantment that has it, at that level (the last entry beyond it).
fn highest_level_entry<T: Clone>(
    stack: &ItemStack,
    component: impl Fn(&EnchantmentEffects) -> &[T],
) -> Option<T> {
    let registry = enchantment::current();
    let (def, level) = on_stack(&registry, stack)
        .filter(|(def, _)| !component(&def.effects).is_empty())
        .max_by_key(|(_, level)| *level)?;
    let entries = component(&def.effects);
    let index = (level.max(1) as usize - 1).min(entries.len() - 1);
    entries.get(index).cloned()
}

fn sound(sound: &Option<SoundRef>) -> Option<SoundEvent> {
    SoundEvent::from_name(&sound.as_ref()?.id)
}

/// Ticks a crossbow takes to load (Quick Charge shortens it).
pub fn crossbow_charge_ticks(crossbow: &ItemStack) -> u32 {
    let seconds = item_value_direct(
        crossbow,
        |e| e.crossbow_charge_time.as_ref(),
        CROSSBOW_CHARGE_SECONDS,
    );
    (seconds * 20.0).floor().max(0.0) as u32
}

/// The sounds a crossbow makes as it starts, gets halfway through and
/// finishes loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChargingSounds {
    pub start: Option<SoundEvent>,
    pub mid: Option<SoundEvent>,
    pub end: Option<SoundEvent>,
}

pub fn charging_sounds(crossbow: &ItemStack) -> ChargingSounds {
    match highest_level_entry(crossbow, |e| &e.crossbow_charging_sounds) {
        Some(entry) => ChargingSounds {
            start: sound(&entry.start),
            mid: sound(&entry.mid),
            end: sound(&entry.end),
        },
        None => ChargingSounds {
            start: Some(SoundEvent::ItemCrossbowLoadingStart),
            mid: Some(SoundEvent::ItemCrossbowLoadingMiddle),
            end: Some(SoundEvent::ItemCrossbowLoadingEnd),
        },
    }
}

/// How many projectiles one load takes (Multishot fires three).
pub fn projectile_count(weapon: &ItemStack) -> usize {
    item_value(weapon, |e| &e.projectile_count, 1.0).max(1.0) as usize
}

/// The widest angle, in degrees, a volley fans out to.
pub fn projectile_spread(weapon: &ItemStack) -> f32 {
    item_value(weapon, |e| &e.projectile_spread, 0.0).max(0.0)
}

/// How many entities an arrow from `weapon` passes through (Piercing).
pub fn piercing(weapon: &ItemStack) -> u8 {
    item_value(weapon, |e| &e.projectile_piercing, 0.0).clamp(0.0, 127.0) as u8
}

/// How hard a thrown trident pulls back to its thrower (Loyalty).
pub fn trident_loyalty(trident: &ItemStack) -> u8 {
    item_value(trident, |e| &e.trident_return_acceleration, 0.0).clamp(0.0, 127.0) as u8
}

/// The strength of a Riptide launch; 0 without Riptide.
pub fn spin_attack_strength(trident: &ItemStack) -> f32 {
    item_value_direct(trident, |e| e.trident_spin_attack_strength.as_ref(), 0.0)
}

/// The sound a Riptide launch makes.
pub fn trident_sound(trident: &ItemStack) -> SoundEvent {
    highest_level_entry(trident, |e| &e.trident_sound)
        .and_then(|sound| SoundEvent::from_name(&sound.id))
        .unwrap_or(SoundEvent::ItemTridentThrow)
}

/// Whether a trident calls down lightning on what it hits in a storm.
pub fn channeling(trident: &ItemStack) -> bool {
    let registry = enchantment::current();
    on_stack(&registry, trident).any(|(def, _)| {
        def.effects
            .post_attack
            .iter()
            .map(|entry| &entry.effect)
            .chain(def.effects.hit_block.iter().map(|entry| &entry.effect))
            .any(summons_lightning)
    })
}

fn summons_lightning(effect: &EntityEffect) -> bool {
    match effect {
        EntityEffect::AllOf(effects) => effects.iter().any(summons_lightning),
        EntityEffect::SummonEntity {
            entity: HolderSet::Ids(ids),
            ..
        } => ids.iter().any(|id| id == "minecraft:lightning_bolt"),
        _ => false,
    }
}

/// The angle, in degrees, each of `count` projectiles fans out to across
/// `spread`: the first straight ahead (for an odd count), then
/// alternating sides further out.
pub fn volley_angles(count: usize, spread: f32) -> Vec<f32> {
    let step = if count <= 1 {
        0.0
    } else {
        2.0 * spread / (count - 1) as f32
    };
    let offset = ((count.saturating_sub(1)) % 2) as f32 * step / 2.0;
    let mut direction = 1.0f32;
    (0..count)
        .map(|i| {
            let angle = offset + direction * i.div_ceil(2) as f32 * step;
            direction = -direction;
            angle
        })
        .collect()
}

/// The unit vector a pitch and yaw (degrees) look along.
pub fn view_vector(pitch: f32, yaw: f32) -> [f64; 3] {
    let (pitch, yaw) = (pitch.to_radians(), -yaw.to_radians());
    let (yc, ys) = (yaw.cos(), yaw.sin());
    let (pc, ps) = (pitch.cos(), pitch.sin());
    [f64::from(ys * pc), f64::from(-ps), f64::from(yc * pc)]
}

/// The direction a crossbow shot leaves along, turned `angle` degrees
/// about the shooter's up direction (Multishot's side bolts).
pub fn volley_direction(pitch: f32, yaw: f32, angle: f32) -> [f64; 3] {
    let view = view_vector(pitch, yaw);
    if angle == 0.0 {
        return view;
    }
    let up = view_vector(pitch - 90.0, yaw);
    rotate(view, up, f64::from(angle).to_radians())
}

/// The direction a mob's crossbow shot at a target along `aim` leaves
/// along, turned `angle` degrees about the aim's own up direction; `up`
/// stands in for the side axis when the aim is straight up or down.
pub fn aimed_volley_direction(aim: [f64; 3], angle: f32, up: [f64; 3]) -> [f64; 3] {
    let length = aim.iter().map(|c| c * c).sum::<f64>().sqrt() as f32;
    let view = aim.map(|c| c as f32 / length);
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let mut right = cross(view, [0.0, 1.0, 0.0]);
    if f64::from(right.iter().map(|c| c * c).sum::<f32>()) <= 1.0e-7 {
        right = cross(view, up.map(|c| c as f32));
    }
    let lift = rotate_axis(view, std::f32::consts::FRAC_PI_2, right);
    rotate_axis(view, angle * 0.017_453_292, lift).map(f64::from)
}

/// `v` turned by `angle` radians about `axis` through the quaternion the
/// axis and half angle make, which is only a pure rotation for a unit
/// axis (the game turns shots about axes it never normalizes).
fn rotate_axis(v: [f32; 3], angle: f32, axis: [f32; 3]) -> [f32; 3] {
    let half = angle * 0.5;
    let sin = half.sin();
    let [qx, qy, qz] = axis.map(|c| c * sin);
    let qw = half.cos();
    let (w2, x2, y2, z2) = (qw * qw, qx * qx, qy * qy, qz * qz);
    let (zw, xy, xz, yw, yz, xw) = (qz * qw, qx * qy, qx * qz, qy * qw, qy * qz, qx * qw);
    let [x, y, z] = v;
    [
        (w2 + x2 - z2 - y2) * x + (-zw + xy - zw + xy) * y + (yw + xz + xz + yw) * z,
        (xy + zw + zw + xy) * x + (y2 - z2 + w2 - x2) * y + (yz + yz - xw - xw) * z,
        (xz - yw + xz - yw) * x + (yz + yz + xw + xw) * y + (z2 - y2 - x2 + w2) * z,
    ]
}

/// Marks a drawn copy that costs nothing: only creative players pick it
/// up again.
pub const INTANGIBLE: &str = "minecraft:intangible_projectile";

/// A free copy of `stack` for a crossbow to load.
pub fn intangible(stack: ItemStack) -> ItemStack {
    let mut stack = stack.with_count(1);
    stack
        .components
        .other
        .insert(INTANGIBLE, tuff_nbt::Compound::new());
    stack
}

/// `v` turned by `angle` radians about the unit `axis`, counter-clockwise
/// looking down the axis.
fn rotate(v: [f64; 3], axis: [f64; 3], angle: f64) -> [f64; 3] {
    let (sin, cos) = angle.sin_cos();
    let dot = v[0] * axis[0] + v[1] * axis[1] + v[2] * axis[2];
    let cross = [
        axis[1] * v[2] - axis[2] * v[1],
        axis[2] * v[0] - axis[0] * v[2],
        axis[0] * v[1] - axis[1] * v[0],
    ];
    [0, 1, 2].map(|i| v[i] * cos + cross[i] * sin + axis[i] * dot * (1.0 - cos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::Enchantments;
    use tuff_vanilla_data::Item;

    fn enchanted(item: Item, enchantments: &[(&str, i32)]) -> ItemStack {
        let mut stack = ItemStack::new(item, 1);
        stack.components.enchantments = Some(Enchantments::new(
            enchantments
                .iter()
                .map(|(id, level)| (format!("minecraft:{id}"), *level)),
        ));
        stack
    }

    #[tuff_core::watchdog]
    #[test]
    fn quick_charge_shortens_loading_and_changes_its_sound() {
        let plain = ItemStack::new(Item::Crossbow, 1);
        assert_eq!(crossbow_charge_ticks(&plain), 25);
        assert_eq!(
            charging_sounds(&plain).start,
            Some(SoundEvent::ItemCrossbowLoadingStart)
        );
        let quick = enchanted(Item::Crossbow, &[("quick_charge", 2)]);
        assert_eq!(crossbow_charge_ticks(&quick), 15);
        let sounds = charging_sounds(&quick);
        assert_eq!(sounds.start, Some(SoundEvent::ItemCrossbowQuickCharge2));
        assert_eq!(sounds.mid, None);
        assert_eq!(sounds.end, Some(SoundEvent::ItemCrossbowLoadingEnd));
        assert_eq!(
            crossbow_charge_ticks(&enchanted(Item::Crossbow, &[("quick_charge", 5)])),
            0
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn multishot_fires_three_across_ten_degrees_each_way() {
        let multishot = enchanted(Item::Crossbow, &[("multishot", 1)]);
        assert_eq!(projectile_count(&multishot), 3);
        assert_eq!(projectile_spread(&multishot), 10.0);
        assert_eq!(volley_angles(3, 10.0), vec![0.0, -10.0, 10.0]);
        assert_eq!(volley_angles(1, 0.0), vec![0.0]);
        assert_eq!(projectile_count(&ItemStack::new(Item::Crossbow, 1)), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn trident_enchantments_read_their_levels() {
        assert_eq!(piercing(&enchanted(Item::Crossbow, &[("piercing", 4)])), 4);
        assert_eq!(
            trident_loyalty(&enchanted(Item::Trident, &[("loyalty", 3)])),
            3
        );
        let riptide = enchanted(Item::Trident, &[("riptide", 3)]);
        assert_eq!(spin_attack_strength(&riptide), 3.0);
        assert_eq!(trident_sound(&riptide), SoundEvent::ItemTridentRiptide3);
        assert_eq!(spin_attack_strength(&ItemStack::new(Item::Trident, 1)), 0.0);
        assert!(channeling(&enchanted(Item::Trident, &[("channeling", 1)])));
    }

    #[tuff_core::watchdog]
    #[test]
    fn impaling_hurts_only_water_creatures_more() {
        use crate::combat::{DamageContext, modify_damage};
        use rand::SeedableRng;
        use tuff_vanilla_data::registry::EntityType;
        let trident = enchanted(Item::Trident, &[("impaling", 5)]);
        let context = |victim| DamageContext {
            kind: &tuff_vanilla_data::damage_type::TRIDENT,
            victim,
            attacker: Some(EntityType::Player),
            direct_attacker: Some(EntityType::Trident),
            is_direct: false,
            direct_attacker_fall_distance: None,
            direct_attacker_flying: None,
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        assert_eq!(
            modify_damage(&trident, &context(EntityType::Cod), 8.0, &mut rng),
            20.5
        );
        assert_eq!(
            modify_damage(&trident, &context(EntityType::Zombie), 8.0, &mut rng),
            8.0
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn side_bolts_turn_about_the_shooters_up_direction() {
        let ahead = volley_direction(0.0, 0.0, 0.0);
        assert!(
            (ahead[2] - 1.0).abs() < 1e-6,
            "yaw 0 looks south: {ahead:?}"
        );
        let turned = volley_direction(0.0, 0.0, 10.0);
        assert!(turned[1].abs() < 1e-6, "level shots stay level");
        let angle = turned[2].acos().to_degrees();
        assert!((angle - 10.0).abs() < 1e-4, "{angle}");
        let other = volley_direction(0.0, 0.0, -10.0);
        assert!((turned[0] + other[0]).abs() < 1e-9, "mirror images");
    }

    #[tuff_core::watchdog]
    #[test]
    fn aimed_side_bolts_fan_out_about_the_aim() {
        let up = [0.0, 1.0, 0.0];
        let ahead = aimed_volley_direction([4.0, 0.0, 0.0], 0.0, up);
        assert!((ahead[0] - 1.0).abs() < 1e-6, "{ahead:?}");
        let turned = aimed_volley_direction([4.0, 0.0, 0.0], 10.0, up);
        assert!(turned[1].abs() < 1e-6, "level shots stay level");
        let angle = turned[0].acos().to_degrees();
        assert!((angle - 10.0).abs() < 1e-3, "{angle}");
        let other = aimed_volley_direction([4.0, 0.0, 0.0], -10.0, up);
        assert!((turned[2] + other[2]).abs() < 1e-6, "mirror images");
        let steep = aimed_volley_direction([0.0, -3.0, 0.0], 10.0, [0.0, 0.0, 1.0]);
        assert!(steep.iter().all(|c| c.is_finite()), "{steep:?}");
    }
}
