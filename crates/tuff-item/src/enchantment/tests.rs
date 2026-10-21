use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::json;

use super::*;

fn get(id: &str) -> &'static EnchantmentDef {
    EnchantmentCodec::builtin(id).unwrap()
}

fn rng() -> StdRng {
    StdRng::seed_from_u64(0)
}

#[tuff_core::watchdog]
#[test]
fn every_vanilla_enchantment_decodes() {
    for (name, enchantment) in tuff_vanilla_data::enchantment::ALL {
        let def = EnchantmentCodec::builtin(name).unwrap();
        assert_eq!(def.max_level, enchantment.max_level);
    }
    assert_eq!(builtin().len(), tuff_vanilla_data::enchantment::ALL.len());
}

#[tuff_core::watchdog]
#[test]
fn sharpness_adds_half_a_point_per_level() {
    let damage = &get("minecraft:sharpness").effects.damage;
    let at = |level| modify_value(damage, level, &mut rng(), 0.0, |_| true);
    assert_eq!(at(1), 1.0);
    assert_eq!(at(5), 3.0);
}

#[tuff_core::watchdog]
#[test]
fn protection_needs_its_requirement() {
    let protection = &get("protection").effects.damage_protection;
    assert_eq!(
        protection[0].requirements.as_ref().unwrap().kind.as_ref(),
        "minecraft:damage_source_properties"
    );
    assert_eq!(modify_value(protection, 4, &mut rng(), 0.0, |_| true), 4.0);
    assert_eq!(modify_value(protection, 4, &mut rng(), 0.0, |_| false), 0.0);
}

#[tuff_core::watchdog]
#[test]
fn thorns_damages_the_attacker_and_wears_the_armor() {
    let post_attack = &get("thorns").effects.post_attack;
    let [thorns] = post_attack.as_slice() else {
        panic!("one post_attack effect");
    };
    assert_eq!(thorns.enchanted, Target::Victim);
    assert_eq!(thorns.affected, Target::Attacker);
    let EntityEffect::AllOf(effects) = &thorns.effect else {
        panic!("all_of");
    };
    let EntityEffect::DamageEntity {
        damage_type,
        min_damage,
        max_damage,
    } = &effects[0]
    else {
        panic!("damage_entity");
    };
    assert_eq!(damage_type, "minecraft:thorns");
    assert_eq!((min_damage.at(3), max_damage.at(3)), (1.0, 5.0));
    assert_eq!(
        effects[1],
        EntityEffect::ChangeItemDamage(LevelValue::Constant(2.0))
    );
    assert_eq!(
        thorns.requirements.as_ref().unwrap().kind.as_ref(),
        "minecraft:random_chance"
    );
}

#[tuff_core::watchdog]
#[test]
fn fire_aspect_ignites_for_four_seconds_per_level() {
    let [fire] = get("fire_aspect").effects.post_attack.as_slice() else {
        panic!("one post_attack effect");
    };
    assert_eq!(
        (fire.enchanted, fire.affected),
        (Target::Attacker, Target::Victim)
    );
    let EntityEffect::Ignite(duration) = &fire.effect else {
        panic!("ignite");
    };
    assert_eq!(duration.at(1), 4.0);
    assert_eq!(duration.at(2), 8.0);
}

#[tuff_core::watchdog]
#[test]
fn mending_doubles_repair_per_experience() {
    let repair = &get("mending").effects.repair_with_xp;
    assert_eq!(modify_value(repair, 1, &mut rng(), 3.0, |_| true), 6.0);
}

#[tuff_core::watchdog]
#[test]
fn quick_charge_shortens_charge_time() {
    let effects = &get("quick_charge").effects;
    let charge = effects.crossbow_charge_time.as_ref().unwrap();
    assert_eq!(charge.process(3, &mut rng(), 1.25), 0.5);
    assert_eq!(effects.crossbow_charging_sounds.len(), 3);
    assert_eq!(
        effects.crossbow_charging_sounds[0]
            .start
            .as_ref()
            .unwrap()
            .id,
        "minecraft:item.crossbow.quick_charge_1"
    );
}

#[tuff_core::watchdog]
#[test]
fn frost_walker_freezes_a_clamped_disk() {
    let effects = &get("frost_walker").effects;
    assert_eq!(effects.damage_immunity.len(), 1);
    let EntityEffect::ReplaceDisk {
        radius,
        offset,
        trigger_game_event,
        ..
    } = &effects.location_changed[0].effect
    else {
        panic!("replace_disk");
    };
    assert_eq!(radius.at(2), 4.0);
    assert_eq!(radius.at(20), 16.0);
    assert_eq!(*offset, [0, -1, 0]);
    assert_eq!(trigger_game_event.as_deref(), Some("minecraft:block_place"));
}

#[tuff_core::watchdog]
#[test]
fn level_values_match_vanilla_formulas() {
    let value = |json| level_value(&json).unwrap();
    assert_eq!(
        value(json!({"type": "levels_squared", "added": 1.0})).at(3),
        10.0
    );
    let lookup = value(
        json!({"type": "lookup", "values": [1.2, 1.75], "fallback": {"type": "linear", "base": 1.5, "per_level_above_first": 0.35}}),
    );
    assert_eq!(lookup.at(2), 1.75);
    assert!((lookup.at(3) - 2.2).abs() < 1e-6);
    let fraction = value(
        json!({"type": "fraction", "numerator": 1.0, "denominator": {"type": "linear", "base": 0.0, "per_level_above_first": 1.0}}),
    );
    assert_eq!(fraction.at(1), 0.0);
    assert_eq!(fraction.at(3), 0.5);
    assert_eq!(
        value(json!({"type": "exponent", "base": 2.0, "power": 3.0})).at(1),
        8.0
    );
    assert!(
        level_value(&json!({"type": "clamped", "value": 1.0, "min": 2.0, "max": 1.0})).is_err()
    );
}

#[tuff_core::watchdog]
#[test]
fn remove_binomial_keeps_unbreaking_like_counts_in_range() {
    let effect = value_effect(&json!({"type": "remove_binomial", "chance": 0.5})).unwrap();
    let mut rng = rng();
    for input in [1.0, 10.0, 500.0] {
        let left = effect.process(1, &mut rng, input);
        assert!((0.0..=input).contains(&left));
    }
    let always = value_effect(&json!({"type": "remove_binomial", "chance": 1.0})).unwrap();
    assert_eq!(always.process(1, &mut rng, 7.0), 0.0);
}

#[tuff_core::watchdog]
#[test]
fn pack_json_decodes_through_the_codec() {
    let pack = json!({
        "description": {"translate": "enchantment.test.zap"},
        "supported_items": ["minecraft:diamond_sword", "stick"],
        "primary_items": "#minecraft:enchantable/sword",
        "exclusive_set": "#minecraft:exclusive_set/damage",
        "weight": 3,
        "max_level": 2,
        "min_cost": {"base": 1, "per_level_above_first": 10},
        "max_cost": {"base": 20, "per_level_above_first": 10},
        "anvil_cost": 2,
        "slots": ["mainhand"],
        "effects": {
            "minecraft:damage": [{"effect": {"type": "minecraft:multiply", "factor": 2.0}, "requirements": "test:is_wet"}],
            "minecraft:attributes": [{"id": "test:zap", "attribute": "minecraft:attack_speed", "amount": 0.5, "operation": "add_multiplied_total"}],
            "minecraft:location_changed": [{"effect": {"type": "attribute", "id": "test:zap", "attribute": "minecraft:movement_speed", "amount": 1.0, "operation": "add_value"}}]
        }
    });
    let def = EnchantmentDef::decode(&pack).unwrap();
    assert_eq!(
        def.supported_items,
        ItemSet::Items(vec![Item::DiamondSword, Item::Stick])
    );
    assert_eq!(
        def.primary_items,
        Some(ItemSet::Tag("minecraft:enchantable/sword".into()))
    );
    assert_eq!(
        def.exclusive_set,
        HolderSet::Tag("minecraft:exclusive_set/damage".into())
    );
    assert_eq!(
        modify_value(&def.effects.damage, 1, &mut rng(), 3.0, |_| true),
        6.0
    );
    assert_eq!(
        def.effects.damage[0]
            .requirements
            .as_ref()
            .unwrap()
            .kind
            .as_ref(),
        "minecraft:reference"
    );
    assert_eq!(
        def.effects.attributes[0].operation,
        AttributeOperation::AddMultipliedTotal
    );

    let mut bad = pack.clone();
    bad["effects"]["minecraft:tick"] = json!([{"effect": {"type": "attribute", "id": "a", "attribute": "b", "amount": 1.0, "operation": "add_value"}}]);
    assert!(EnchantmentDef::decode(&bad).is_err());
    let mut bad = pack.clone();
    bad["effects"]["minecraft:nonsense"] = json!([]);
    assert!(EnchantmentDef::decode(&bad).is_err());
    let mut bad = pack;
    bad["weight"] = json!(0);
    assert!(EnchantmentDef::decode(&bad).is_err());
}

#[tuff_core::watchdog]
#[test]
fn vanilla_registry_decodes_every_entry_as_builtin() {
    let registry = crate::registries::Registries::vanilla()
        .decode::<EnchantmentCodec>()
        .unwrap();
    assert!(!registry.modified());
    assert_eq!(registry.len(), tuff_vanilla_data::enchantment::ALL.len());
    assert!(registry.iter().all(|(_, entry)| entry.is_builtin()));
}

#[tuff_core::watchdog]
#[test]
fn every_built_in_provider_decodes_and_names_a_known_enchantment() {
    let data = vanilla();
    assert_eq!(data.providers.len(), 7);
    let single = data.providers.get("minecraft:raid/vindicator_post_wave_5");
    assert_eq!(
        single,
        Some(&EnchantmentProvider::Single {
            enchantment: "minecraft:sharpness".to_owned(),
            level: IntProvider::Constant(2),
        })
    );
    for (_, entry) in data.providers.iter() {
        if let EnchantmentProvider::Single { enchantment, .. } = entry.value() {
            assert!(data.contains(enchantment), "{enchantment}");
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn pack_providers_decode_with_int_providers() {
    let provider = EnchantmentProvider::decode(&json!({
        "type": "minecraft:by_cost",
        "enchantments": "#minecraft:non_treasure",
        "cost": {"type": "minecraft:uniform", "min_inclusive": 3, "max_inclusive": 9}
    }))
    .unwrap();
    let EnchantmentProvider::ByCost { enchantments, cost } = provider else {
        panic!("by_cost");
    };
    assert_eq!(
        enchantments,
        HolderSet::Tag("minecraft:non_treasure".to_owned())
    );
    let mut rng = rng();
    for _ in 0..50 {
        assert!((3..=9).contains(&cost.sample(&mut rng)));
    }
    assert!(EnchantmentProvider::decode(&json!({"type": "minecraft:nope"})).is_err());
}
