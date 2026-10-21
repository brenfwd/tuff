//! Beacon effect choices: which effects each pyramid level unlocks and the
//! encoding the menu data slots use.

use tuff_nbt::Compound;
use tuff_vanilla_data::registry::MobEffect;

pub const BEACON_EFFECTS: [&[MobEffect]; 4] = [
    &[MobEffect::Speed, MobEffect::Haste],
    &[MobEffect::Resistance, MobEffect::JumpBoost],
    &[MobEffect::Strength],
    &[MobEffect::Regeneration],
];

fn required_levels(effect: Option<MobEffect>) -> i32 {
    let Some(effect) = effect else {
        return 0;
    };
    BEACON_EFFECTS
        .iter()
        .position(|tier| tier.contains(&effect))
        .map_or(0, |tier| tier as i32 + 1)
}

pub fn validate_effects(
    primary: Option<MobEffect>,
    secondary: Option<MobEffect>,
    levels: i32,
) -> bool {
    if secondary.is_some() && levels < 4 {
        return false;
    }
    let primary_level = required_levels(primary);
    let secondary_level = required_levels(secondary);
    if primary_level > levels || secondary_level > levels || primary_level >= 4 {
        return false;
    }
    !(secondary_level != 0 && secondary_level < 4 && primary != secondary)
}

pub fn encode_effect(effect: Option<MobEffect>) -> i32 {
    effect.map_or(0, |effect| effect.id() as i32 + 1)
}

pub fn decode_effect(id: i32) -> Option<MobEffect> {
    u32::try_from(id.checked_sub(1)?)
        .ok()
        .and_then(MobEffect::from_id)
}

fn stored_effect(nbt: &Compound, key: &str) -> Option<MobEffect> {
    let effect = MobEffect::from_name(nbt.get_str(key)?)?;
    (required_levels(Some(effect)) > 0).then_some(effect)
}

pub fn levels(nbt: &Compound) -> i32 {
    nbt.get_int("Levels").unwrap_or(0)
}

/// The levels, primary and secondary slots a beacon menu opens with.
pub fn menu_data(nbt: Option<&Compound>) -> Vec<i32> {
    let Some(nbt) = nbt else {
        return vec![0, 0, 0];
    };
    vec![
        levels(nbt),
        encode_effect(stored_effect(nbt, "primary_effect")),
        encode_effect(stored_effect(nbt, "secondary_effect")),
    ]
}

pub fn store_effects(nbt: &mut Compound, primary: Option<MobEffect>, secondary: Option<MobEffect>) {
    for (key, effect) in [("primary_effect", primary), ("secondary_effect", secondary)] {
        match effect {
            Some(effect) => {
                nbt.insert(key, effect.name());
            }
            None => {
                nbt.remove(key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn effects_are_gated_by_pyramid_level() {
        use MobEffect::*;
        assert!(validate_effects(Some(Speed), None, 1));
        assert!(!validate_effects(Some(Resistance), None, 1));
        assert!(validate_effects(Some(Resistance), None, 2));
        assert!(!validate_effects(Some(Strength), Some(Strength), 3));
        assert!(!validate_effects(Some(Regeneration), None, 4));
        assert!(validate_effects(Some(Strength), Some(Regeneration), 4));
        assert!(validate_effects(Some(Speed), Some(Speed), 4));
        assert!(!validate_effects(Some(Speed), Some(Haste), 4));
        assert!(validate_effects(None, None, 0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn effect_encoding_round_trips() {
        assert_eq!(encode_effect(None), 0);
        assert_eq!(decode_effect(0), None);
        let id = encode_effect(Some(MobEffect::Haste));
        assert_eq!(decode_effect(id), Some(MobEffect::Haste));
        assert_eq!(decode_effect(10_000), None);
    }
}
