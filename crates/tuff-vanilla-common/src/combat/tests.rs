use rand::SeedableRng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Item;

use super::*;
use tuff_item::item::Enchantments;
use tuff_vanilla_data::item::ModifierOperation;

#[tuff_core::watchdog]
#[test]
fn damage_statistics_count_rounded_tenths_of_positive_finite_amounts() {
    assert_eq!(stat_tenths(0.0), None);
    assert_eq!(stat_tenths(-1.0), None);
    assert_eq!(stat_tenths(f32::MAX), None);
    assert_eq!(stat_tenths(0.04), Some(0));
    assert_eq!(stat_tenths(0.05), Some(1));
    assert_eq!(stat_tenths(2.36), Some(24));
}

fn enchanted(item: Item, enchantments: &[(&str, i32)]) -> ItemStack {
    let mut stack = ItemStack::mint(item, 1, MintReason::Test);
    stack.components.enchantments = Some(Enchantments::new(
        enchantments
            .iter()
            .map(|(id, level)| (format!("minecraft:{id}"), *level)),
    ));
    stack
}

fn rng() -> StdRng {
    StdRng::seed_from_u64(7)
}

fn attacker<'a>(equipment: &'a [Equipped<'a>], ticks: u32) -> Attacker<'a> {
    Attacker {
        modifiers: Modifiers::equipment(equipment),
        ticks_since_reset: ticks,
        fall_distance: 0.0,
        on_ground: true,
        sprinting: false,
        in_water: false,
        climbable: false,
        known_speed_sq: 0.0,
    }
}

#[tuff_core::watchdog]
#[test]
fn armor_and_toughness_add_up_from_worn_pieces() {
    let helmet = ItemStack::mint(Item::DiamondHelmet, 1, MintReason::Test);
    let chest = ItemStack::mint(Item::DiamondChestplate, 1, MintReason::Test);
    let sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    let equipment = [
        (EquipmentSlot::Head, &helmet),
        (EquipmentSlot::Chest, &chest),
        (EquipmentSlot::Mainhand, &sword),
    ];
    assert_eq!(armor(&equipment), (11.0, 4.0));
    let misplaced = [(EquipmentSlot::Mainhand, &chest)];
    assert_eq!(armor(&misplaced), (0.0, 0.0));
}

#[tuff_core::watchdog]
#[test]
fn attributes_combine_weapons_effects_and_enchantments() {
    let sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    let equipment = [(EquipmentSlot::Mainhand, &sword)];
    let plain = Modifiers::equipment(&equipment);
    assert_eq!(plain.value(Attribute::AttackDamage), 7.0);
    assert!((plain.value(Attribute::AttackSpeed) - 1.6).abs() < 1e-6);
    let strong = Modifiers {
        equipment: &equipment,
        effects: &[(MobEffect::Strength, 1)],
    };
    assert_eq!(strong.value(Attribute::AttackDamage), 13.0);
    let weak = Modifiers {
        equipment: &[],
        effects: &[(MobEffect::Weakness, 0)],
    };
    assert_eq!(weak.value(Attribute::AttackDamage), 0.0, "clamped at zero");
    let sweeping = enchanted(Item::IronSword, &[("sweeping_edge", 3)]);
    let equipment = [(EquipmentSlot::Mainhand, &sweeping)];
    let ratio = Modifiers::equipment(&equipment).value(Attribute::SweepingDamageRatio);
    assert!((ratio - 0.75).abs() < 1e-6);
    let held = [(EquipmentSlot::Offhand, &sweeping)];
    assert_eq!(
        Modifiers::equipment(&held).value(Attribute::SweepingDamageRatio),
        0.0
    );
    let blast = enchanted(Item::IronChestplate, &[("blast_protection", 4)]);
    let equipment = [(EquipmentSlot::Chest, &blast)];
    let resistance =
        Modifiers::equipment(&equipment).value(Attribute::ExplosionKnockbackResistance);
    assert!((resistance - 0.6).abs() < 1e-6);
    let fast = enchanted(Item::DiamondPickaxe, &[("efficiency", 5)]);
    let equipment = [(EquipmentSlot::Mainhand, &fast)];
    let efficiency = Modifiers::equipment(&equipment).value(Attribute::MiningEfficiency);
    assert_eq!(efficiency, 26.0);
    let aqua = enchanted(Item::IronHelmet, &[("aqua_affinity", 1)]);
    let equipment = [(EquipmentSlot::Head, &aqua)];
    let submerged = Modifiers::equipment(&equipment).value(Attribute::SubmergedMiningSpeed);
    assert!((submerged - 1.0).abs() < 1e-6, "{submerged}");
    let netherite = ItemStack::mint(Item::NetheriteBoots, 1, MintReason::Test);
    let equipment = [(EquipmentSlot::Feet, &netherite)];
    let resistance = Modifiers::equipment(&equipment).value(Attribute::KnockbackResistance);
    assert!((resistance - 0.1).abs() < 1e-6);
}

#[tuff_core::watchdog]
#[test]
fn a_stacks_own_attribute_modifiers_replace_the_items() {
    use tuff_item::item::gear::{AttributeModifier, ModifierDisplay};
    use tuff_vanilla_data::item::EquipmentSlotGroup;
    let mut stick = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    stick.components.attribute_modifiers = Some(vec![AttributeModifier {
        attribute: Attribute::AttackDamage,
        id: "minecraft:spiked".into(),
        amount: 10.0,
        operation: ModifierOperation::AddValue,
        slot: EquipmentSlotGroup::Hand,
        display: ModifierDisplay::Default,
    }]);
    let held = [(EquipmentSlot::Mainhand, &stick)];
    assert_eq!(
        Modifiers::equipment(&held).value(Attribute::AttackDamage),
        11.0
    );
    let worn = [(EquipmentSlot::Head, &stick)];
    assert_eq!(
        Modifiers::equipment(&worn).value(Attribute::AttackDamage),
        1.0
    );
    let mut blunt = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    blunt.components.removed = vec!["minecraft:attribute_modifiers".into()];
    let held = [(EquipmentSlot::Mainhand, &blunt)];
    let modifiers = Modifiers::equipment(&held);
    assert_eq!(modifiers.value(Attribute::AttackDamage), 1.0);
    assert_eq!(modifiers.value(Attribute::AttackSpeed), 4.0);
    let mut plated = ItemStack::mint(Item::LeatherHelmet, 1, MintReason::Test);
    plated.components.attribute_modifiers = Some(vec![AttributeModifier {
        attribute: Attribute::Armor,
        id: "minecraft:plate".into(),
        amount: 8.0,
        operation: ModifierOperation::AddValue,
        slot: EquipmentSlotGroup::Armor,
        display: ModifierDisplay::Hidden,
    }]);
    assert_eq!(armor(&[(EquipmentSlot::Head, &plated)]), (8.0, 0.0));
}

#[tuff_core::watchdog]
#[test]
fn melee_follows_weapon_cooldown_and_crits() {
    let mut rng = rng();
    let fist = attacker(&[], 100);
    let hit = melee(&fist, EntityType::Player, &mut rng).unwrap();
    assert_eq!(hit.damage, 1.0);
    assert!(hit.full_strength && !hit.critical && hit.sweep.is_none());
    let spam = melee(&attacker(&[], 0), EntityType::Player, &mut rng).unwrap();
    assert!(spam.damage < 1.0 && !spam.full_strength);

    let sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    let equipment = [(EquipmentSlot::Mainhand, &sword)];
    let ready = attacker(&equipment, 13);
    let hit = melee(&ready, EntityType::Player, &mut rng).unwrap();
    assert_eq!(hit.damage, 7.0);
    assert_eq!(hit.sweep.unwrap().damage, 1.0);
    assert_eq!(hit.knockback, 0.0);
    let early = melee(&attacker(&equipment, 3), EntityType::Player, &mut rng).unwrap();
    let strength = attack_strength(3, 4.0 - 2.4);
    assert!((early.damage - 7.0 * (0.2 + strength * strength * 0.8)).abs() < 1e-5);
    assert!(early.sweep.is_none());

    let falling = Attacker {
        fall_distance: 1.0,
        on_ground: false,
        ..ready
    };
    let crit = melee(&falling, EntityType::Player, &mut rng).unwrap();
    assert!(crit.critical && crit.sweep.is_none());
    assert_eq!(crit.damage, 10.5);
    for blocked in [
        Attacker {
            in_water: true,
            ..falling
        },
        Attacker {
            climbable: true,
            ..falling
        },
        Attacker {
            sprinting: true,
            ..falling
        },
    ] {
        let hit = melee(&blocked, EntityType::Player, &mut rng).unwrap();
        assert!(!hit.critical);
    }
    let running = Attacker {
        sprinting: true,
        ..ready
    };
    let sprint = melee(&running, EntityType::Player, &mut rng).unwrap();
    assert!(sprint.sprint && sprint.sweep.is_none());
    assert_eq!(sprint.knockback, 0.5);
    let fast = Attacker {
        known_speed_sq: 0.3 * 0.3,
        ..ready
    };
    let hit = melee(&fast, EntityType::Player, &mut rng).unwrap();
    assert!(hit.sweep.is_none(), "too fast to sweep");
    let axe = ItemStack::mint(Item::DiamondAxe, 1, MintReason::Test);
    let equipment = [(EquipmentSlot::Mainhand, &axe)];
    let chop = melee(&attacker(&equipment, 40), EntityType::Player, &mut rng).unwrap();
    assert!(chop.sweep.is_none(), "only swords sweep");
    assert_eq!(chop.damage, 9.0);
}

#[tuff_core::watchdog]
#[test]
fn damage_enchantments_follow_their_targets() {
    let mut rng = rng();
    let sharp = enchanted(Item::IronSword, &[("sharpness", 5), ("knockback", 2)]);
    let equipment = [(EquipmentSlot::Mainhand, &sharp)];
    let hit = melee(&attacker(&equipment, 40), EntityType::Player, &mut rng).unwrap();
    assert_eq!(hit.damage, 6.0 + 3.0);
    assert!(hit.magic);
    assert_eq!(hit.knockback, 1.0);
    let smite = enchanted(Item::IronSword, &[("smite", 5)]);
    let context = DamageContext::melee(EntityType::Player);
    assert_eq!(modify_damage(&smite, &context, 6.0, &mut rng), 6.0);
    let zombie = DamageContext::melee(EntityType::Zombie);
    assert_eq!(modify_damage(&smite, &zombie, 6.0, &mut rng), 18.5);
    let bane = enchanted(Item::IronSword, &[("bane_of_arthropods", 1)]);
    assert_eq!(modify_damage(&bane, &context, 6.0, &mut rng), 6.0);
    let spider = DamageContext::melee(EntityType::Spider);
    assert_eq!(modify_damage(&bane, &spider, 6.0, &mut rng), 8.5);

    let sweeping = enchanted(Item::IronSword, &[("sweeping_edge", 1), ("sharpness", 1)]);
    let equipment = [(EquipmentSlot::Mainhand, &sweeping)];
    let hit = melee(&attacker(&equipment, 40), EntityType::Player, &mut rng).unwrap();
    let sweep = hit.sweep.unwrap();
    assert_eq!(sweep.damage, 1.0 + 0.5 * 6.0);
    assert_eq!(
        sweep_damage(Some(&sweeping), sweep, EntityType::Player, &mut rng),
        5.0
    );
}

#[tuff_core::watchdog]
#[test]
fn post_attack_effects_come_from_the_right_side() {
    let mut rng = rng();
    let fiery = enchanted(Item::IronSword, &[("fire_aspect", 2)]);
    let held = [(EquipmentSlot::Mainhand, &fiery)];
    let context = DamageContext::melee(EntityType::Player);
    let effects = post_attack(&held, Side::Attacker, &context, &mut rng);
    assert_eq!(
        effects,
        vec![PostAttackEffect {
            affected: Side::Victim,
            slot: EquipmentSlot::Mainhand,
            effect: PostAttack::Ignite(8.0),
        }]
    );
    assert!(post_attack(&held, Side::Victim, &context, &mut rng).is_empty());
    let explosion = DamageContext {
        kind: &damage_type::PLAYER_EXPLOSION,
        victim: EntityType::Player,
        attacker: Some(EntityType::Player),
        direct_attacker: Some(EntityType::Tnt),
        is_direct: false,
        direct_attacker_fall_distance: None,
        direct_attacker_flying: None,
    };
    assert!(post_attack(&held, Side::Attacker, &explosion, &mut rng).is_empty());

    let spiky = enchanted(Item::IronChestplate, &[("thorns", 3)]);
    let worn = [(EquipmentSlot::Chest, &spiky)];
    let mut thorns = 0;
    for _ in 0..1000 {
        let effects = post_attack(&worn, Side::Victim, &context, &mut rng);
        if effects.is_empty() {
            continue;
        }
        thorns += 1;
        let [damage, wear] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(damage.affected, Side::Attacker);
        let PostAttack::Damage { kind, amount } = damage.effect else {
            panic!("{damage:?}");
        };
        assert_eq!(kind.name, "minecraft:thorns");
        assert!((1.0..=5.0).contains(&amount));
        assert_eq!(wear.effect, PostAttack::ItemDamage(2));
        assert_eq!(wear.slot, EquipmentSlot::Chest);
    }
    assert!((380..520).contains(&thorns), "45% chance, got {thorns}");
}

#[tuff_core::watchdog]
#[test]
fn bane_of_arthropods_slows_arthropods_but_not_players() {
    let mut rng = rng();
    let bane = enchanted(Item::IronSword, &[("bane_of_arthropods", 5)]);
    let held = [(EquipmentSlot::Mainhand, &bane)];
    let spider = DamageContext::melee(EntityType::Spider);
    let effects = post_attack(&held, Side::Attacker, &spider, &mut rng);
    let slow = effects
        .iter()
        .find(|e| matches!(e.effect, PostAttack::ApplyMobEffect { .. }))
        .unwrap_or_else(|| panic!("no slowness in {effects:?}"));
    assert_eq!(slow.affected, Side::Victim);
    let PostAttack::ApplyMobEffect {
        effect,
        amplifier,
        ticks,
    } = slow.effect
    else {
        unreachable!()
    };
    assert_eq!(effect, MobEffect::Slowness);
    assert_eq!(amplifier, 3);
    assert!((30..=70).contains(&ticks), "got {ticks}");

    let player = DamageContext::melee(EntityType::Player);
    let effects = post_attack(&held, Side::Attacker, &player, &mut rng);
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e.effect, PostAttack::ApplyMobEffect { .. }))
    );
}

#[tuff_core::watchdog]
#[test]
fn armor_formula_matches_the_game() {
    let mut rng = rng();
    let context = DamageContext::melee(EntityType::Player);
    let mut after_armor =
        |damage, armor, toughness| after_armor(damage, armor, toughness, None, &context, &mut rng);
    assert_eq!(after_armor(10.0, 0.0, 0.0), 10.0);
    let full_diamond = after_armor(10.0, 20.0, 8.0);
    assert!((full_diamond - 10.0 * (1.0 - (20.0 - 10.0 / 4.0) / 25.0)).abs() < 1e-5);
    assert!((after_armor(100.0, 20.0, 0.0) - 100.0 * (1.0 - 4.0 / 25.0)).abs() < 1e-3);
    assert_eq!(after_protection(10.0, 30.0), 10.0 * (1.0 - 20.0 / 25.0));
    assert!((after_resistance(10.0, 0) - 8.0).abs() < 1e-6);
    assert_eq!(after_resistance(10.0, 4), 0.0);
    assert_eq!(armor_wear(3.0), 1);
    assert_eq!(armor_wear(9.0), 2);
    assert_eq!(scale_for_difficulty(10.0, Difficulty::Easy), 6.0);
    assert_eq!(scale_for_difficulty(1.0, Difficulty::Easy), 1.0);
    assert_eq!(scale_for_difficulty(10.0, Difficulty::Hard), 15.0);
}

#[tuff_core::watchdog]
#[test]
fn protection_enchantments_follow_their_effect_data() {
    let mut rng = rng();
    let boots = enchanted(
        Item::IronBoots,
        &[("protection", 4), ("feather_falling", 4)],
    );
    let equipment = [(EquipmentSlot::Feet, &boots)];
    let protection = |equipment: &[Equipped], kind, rng: &mut StdRng| {
        enchantment_protection(equipment, &DamageContext::environment(kind), rng)
    };
    assert_eq!(
        protection(&equipment, &damage_type::PLAYER_ATTACK, &mut rng),
        4.0
    );
    assert_eq!(protection(&equipment, &damage_type::FALL, &mut rng), 16.0);
    assert_eq!(
        protection(&equipment, &damage_type::OUT_OF_WORLD, &mut rng),
        0.0
    );
    let held = [(EquipmentSlot::Mainhand, &boots)];
    assert_eq!(protection(&held, &damage_type::FALL, &mut rng), 0.0);
    let chest = enchanted(Item::IronChestplate, &[("blast_protection", 2)]);
    let equipment = [(EquipmentSlot::Chest, &chest)];
    assert_eq!(
        protection(&equipment, &damage_type::EXPLOSION, &mut rng),
        4.0
    );
}

#[tuff_core::watchdog]
#[test]
fn knockback_pushes_away_and_resistance_softens_it() {
    let pushed = knockback([0.0; 3], 0.4, 1.0, 0.0, true, 0.0);
    assert!((pushed[0] + 0.4).abs() < 1e-9 && (pushed[1] - 0.4).abs() < 1e-9);
    let airborne = knockback([0.2, -0.1, 0.0], 0.4, 0.0, 1.0, false, 0.0);
    assert_eq!(airborne, [0.1, -0.1, -0.4]);
    let resisted = knockback([0.0; 3], 0.4, 1.0, 0.0, true, 0.5);
    assert!((resisted[0] + 0.2).abs() < 1e-9 && (resisted[1] - 0.2).abs() < 1e-9);
    assert_eq!(knockback([0.0; 3], 0.4, 1.0, 0.0, true, 1.0), [0.0; 3]);
    let (x, z) = facing_knockback_direction(90.0);
    assert!((x - 1.0).abs() < 1e-6 && z.abs() < 1e-6);
}

#[tuff_core::watchdog]
#[test]
fn sweeps_reach_players_beside_the_target() {
    let attacker = [0.0, 64.0, 0.0];
    let target = [0.0, 64.0, 2.0];
    assert!(in_sweep(attacker, target, [1.0, 64.0, 2.0]));
    assert!(
        !in_sweep(attacker, target, [2.0, 64.0, 2.0]),
        "too far aside"
    );
    assert!(
        !in_sweep(attacker, target, [0.0, 64.0, 3.2]),
        "out of reach"
    );
    assert!(!in_sweep(attacker, target, [1.0, 66.1, 2.0]), "too high");
}

#[tuff_core::watchdog]
#[test]
fn shields_block_in_front_unless_bypassed() {
    let shield = Item::Shield.blocks_attacks().unwrap();
    let front = blocking_angle([0.0, 64.0, 0.0], 0.0, Some([0.0, 64.0, 3.0]));
    let behind = blocking_angle([0.0, 64.0, 0.0], 0.0, Some([0.0, 64.0, -3.0]));
    assert!(front < 0.1 && behind > 3.0);
    assert_eq!(
        shield_blocks(shield, &damage_type::PLAYER_ATTACK, 6.0, front),
        Some(6.0)
    );
    assert_eq!(
        shield_blocks(shield, &damage_type::PLAYER_ATTACK, 6.0, behind),
        Some(0.0)
    );
    assert_eq!(shield_blocks(shield, &damage_type::MAGIC, 6.0, front), None);
    assert_eq!(shield.block_delay_ticks(), 5);
    assert_eq!(shield.disable_ticks(5.0), 100);
}

#[tuff_core::watchdog]
#[test]
fn mace_smash_needs_a_real_fall_and_adds_fall_based_damage() {
    let mut rng = rng();
    let mace = ItemStack::mint(Item::Mace, 1, MintReason::Test);
    let equipment = [(EquipmentSlot::Mainhand, &mace)];
    let falling = Attacker {
        fall_distance: 2.0,
        on_ground: false,
        ..attacker(&equipment, 40)
    };
    let hit = melee(&falling, EntityType::Player, &mut rng).unwrap();
    assert_eq!(hit.smash, Some(2.0));
    assert!(hit.critical, "falling onto a target is also a crit");
    // scaled base (1.0 fist + 5.0 mace) + smash bonus (4.0 * 2.0 fall)
    // = 14.0, *1.5 crit.
    assert_eq!(hit.damage, 21.0);

    let grounded = Attacker {
        fall_distance: 2.0,
        on_ground: true,
        ..attacker(&equipment, 40)
    };
    assert_eq!(
        melee(&grounded, EntityType::Player, &mut rng)
            .unwrap()
            .smash,
        Some(2.0),
        "the attacker's own on_ground state does not gate the smash"
    );
    let short_fall = Attacker {
        fall_distance: 1.0,
        on_ground: false,
        ..attacker(&equipment, 40)
    };
    assert_eq!(
        melee(&short_fall, EntityType::Player, &mut rng)
            .unwrap()
            .smash,
        None,
        "below the 1.5 block threshold"
    );
    let sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    let equipment = [(EquipmentSlot::Mainhand, &sword)];
    let falling_sword = Attacker {
        fall_distance: 2.0,
        on_ground: false,
        ..attacker(&equipment, 40)
    };
    assert_eq!(
        melee(&falling_sword, EntityType::Player, &mut rng)
            .unwrap()
            .smash,
        None,
        "only a mace smashes"
    );
}

#[tuff_core::watchdog]
#[test]
fn density_adds_smash_damage_per_block_fallen() {
    let mut rng = rng();
    let dense = enchanted(Item::Mace, &[("density", 3)]);
    let equipment = [(EquipmentSlot::Mainhand, &dense)];
    let falling = Attacker {
        fall_distance: 2.0,
        on_ground: false,
        ..attacker(&equipment, 40)
    };
    let hit = melee(&falling, EntityType::Player, &mut rng).unwrap();
    // (6.0 base + 4.0*2.0 smash + (0.5 + 0.5*2) * 2.0 density) * 1.5 crit.
    assert_eq!(hit.damage, (6.0 + 8.0 + 3.0) * 1.5);
}

#[tuff_core::watchdog]
#[test]
fn breach_reduces_how_much_armor_stops() {
    let mut rng = rng();
    let context = DamageContext::melee(EntityType::Player);
    let plain = after_armor(10.0, 20.0, 0.0, None, &context, &mut rng);
    let breach = enchanted(Item::Mace, &[("breach", 2)]);
    let pierced = after_armor(10.0, 20.0, 0.0, Some(&breach), &context, &mut rng);
    assert!(pierced > plain, "{pierced} should beat {plain}");
    // fraction 0.6 - (0.15 + 0.15*(2-1)) = 0.3 -> damage * 0.7.
    assert!((pierced - 7.0).abs() < 1e-5);
}

fn value_effect(json: &serde_json::Value, value: f32, level: u32) -> f32 {
    tuff_item::enchantment::value_effect(json)
        .unwrap()
        .process(level, &mut rng(), value)
}

#[tuff_core::watchdog]
#[test]
fn scale_exponentially_and_remove_binomial_value_effects() {
    let scale = serde_json::json!({"type": "minecraft:exponential", "base": 2.0, "exponent": 3.0});
    assert_eq!(value_effect(&scale, 1.5, 1), 12.0);
    let all = serde_json::json!({"type": "minecraft:remove_binomial", "chance": 1.0});
    assert_eq!(value_effect(&all, 5.0, 1), 0.0);
    let none = serde_json::json!({"type": "minecraft:remove_binomial", "chance": 0.0});
    assert_eq!(value_effect(&none, 5.0, 1), 5.0);
    let big = serde_json::json!({"type": "minecraft:remove_binomial", "chance": 0.5});
    let left = value_effect(&big, 1000.0, 1);
    assert!((0.0..=1000.0).contains(&left));
}

#[tuff_core::watchdog]
#[test]
fn wind_burst_explodes_only_while_smashing() {
    let mut rng = rng();
    let gusty = enchanted(Item::Mace, &[("wind_burst", 1)]);
    let held = [(EquipmentSlot::Mainhand, &gusty)];
    let smashing = DamageContext::melee(EntityType::Player).with_attacker_state(2.0, false);
    let effects = post_attack(&held, Side::Attacker, &smashing, &mut rng);
    assert_eq!(
        effects,
        vec![PostAttackEffect {
            affected: Side::Attacker,
            slot: EquipmentSlot::Mainhand,
            effect: PostAttack::Explode {
                radius: 3.5,
                knockback: 1.2,
                offset: [0.0, 0.0, 0.0],
            },
        }]
    );
    let too_short = DamageContext::melee(EntityType::Player).with_attacker_state(1.0, false);
    assert!(post_attack(&held, Side::Attacker, &too_short, &mut rng).is_empty());
    let flying = DamageContext::melee(EntityType::Player).with_attacker_state(2.0, true);
    assert!(post_attack(&held, Side::Attacker, &flying, &mut rng).is_empty());
}

#[tuff_core::watchdog]
#[test]
fn smash_area_knockback_falls_off_with_distance_and_doubles_when_heavy() {
    assert_eq!(smash_knockback_power(4.0, 2.0, 0.0), 0.0, "out of radius");
    let near = smash_knockback_power(1.0, 2.0, 0.0);
    assert!((near - (3.5 - 1.0) * 0.7).abs() < 1e-9);
    let heavy = smash_knockback_power(1.0, 6.0, 0.0);
    assert!((heavy - near * 2.0).abs() < 1e-9);
    let resisted = smash_knockback_power(1.0, 2.0, 0.5);
    assert!((resisted - near * 0.5).abs() < 1e-9);
}

#[tuff_core::watchdog]
#[test]
fn stab_reach_is_a_capsule_along_the_look_direction() {
    let range = Item::WoodenSpear.attack_range().unwrap();
    let eye = [0.0, 65.0, 0.0];
    let forward = [0.0, 0.0, 1.0];
    assert!(in_stab_reach(eye, forward, [0.0, 65.0, 3.0], range, false));
    assert!(
        !in_stab_reach(eye, forward, [0.0, 65.0, 1.0], range, false),
        "closer than min_reach"
    );
    assert!(
        !in_stab_reach(eye, forward, [0.0, 65.0, 5.0], range, false),
        "farther than max_reach"
    );
    assert!(
        in_stab_reach(eye, forward, [0.0, 65.0, 6.0], range, true),
        "creative reach is longer"
    );
    assert!(
        !in_stab_reach(eye, forward, [3.0, 65.0, 3.0], range, false),
        "too far to the side"
    );
    assert!(
        !in_stab_reach(eye, forward, [0.0, 65.0, -3.0], range, false),
        "behind the attacker"
    );
}

#[tuff_core::watchdog]
#[test]
fn looting_raises_the_equipment_drop_chance() {
    let looting = enchanted(Item::IronSword, &[("looting", 3)]);
    let context = DamageContext::melee(EntityType::Zombie);
    let mut rng = rng();
    let boosted = equipment_drop_chance(&looting, &context, 0.085, &mut rng);
    assert!((boosted - 0.115).abs() < 1e-4, "got {boosted}");
    let plain = ItemStack::mint(Item::IronSword, 1, MintReason::Test);
    assert_eq!(
        equipment_drop_chance(&plain, &context, 0.085, &mut rng),
        0.085
    );
}
