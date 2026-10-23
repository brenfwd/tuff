use super::*;
use rand::RngCore;
use rand::SeedableRng;
use rand::rngs::StdRng;

#[tuff_core::watchdog]
#[test]
fn stacks_without_components_stay_small_and_unallocated() {
    eprintln!(
        "ItemStack {} B, Option<ItemStack> {} B, Components {} B, ComponentPatch {} B",
        size_of::<ItemStack>(),
        size_of::<Option<ItemStack>>(),
        size_of::<Components>(),
        size_of::<ComponentPatch>()
    );
    assert!(size_of::<ComponentPatch>() <= 440);
    #[cfg(not(debug_assertions))]
    assert!(size_of::<ItemStack>() <= 32);
    #[cfg(not(debug_assertions))]
    assert!(size_of::<Option<ItemStack>>() <= 32);
    assert_eq!(size_of::<Components>(), size_of::<usize>());
    let stone = ItemStack::mint(Item::Stone, 64, MintReason::Test);
    assert!(!stone.components.is_allocated());
    assert!(!stone.test_copy().components.is_allocated());
    assert!(!Components::from_nbt(&Compound::new()).is_allocated());
    assert!(!Components::from(ComponentPatch::default()).is_allocated());
    assert!(
        !stone
            .with_components(ComponentPatch::default())
            .components
            .is_allocated()
    );
}

#[tuff_core::watchdog]
#[test]
fn an_emptied_patch_equals_no_patch() {
    let plain = Components::default();
    let mut emptied = Components::from(ComponentPatch {
        damage: Some(3),
        ..Default::default()
    });
    assert!(emptied.is_allocated());
    assert_ne!(emptied, plain);
    assert_eq!(emptied.damage, Some(3));
    emptied.damage = None;
    assert!(emptied.is_allocated());
    assert!(emptied.is_empty());
    assert_eq!(emptied, plain);
    assert_eq!(plain, emptied);
    assert!(!emptied.picture_copy().is_allocated());
    emptied.compact();
    assert!(!emptied.is_allocated());
    assert_eq!(
        format!("{emptied:?}"),
        format!("{:?}", ComponentPatch::default())
    );
}

#[tuff_core::watchdog]
#[test]
fn removing_an_unmodelled_component_never_allocates() {
    let mut plain = Components::default();
    assert_eq!(plain.remove_other("minecraft:rarity"), None);
    assert!(!plain.is_allocated());
    let mut rare = Components::from_nbt(&Compound::new().with("minecraft:rarity", "epic"));
    assert_eq!(
        rare.remove_other("minecraft:rarity"),
        Some(Tag::String("epic".into()))
    );
    assert!(!rare.is_allocated());
}

/// Wraps a seeded RNG and counts how many `u32`/`u64` words it hands
/// out, so tests can tell which branch of `dropped_points` ran without
/// depending on its return value: the per-point loop draws one `u32`
/// (an `f32`) per point, the normal approximation draws two `u64`s
/// (two `f64`s) regardless of `amount`.
struct CountingRng {
    inner: StdRng,
    u32_calls: u32,
    u64_calls: u32,
}

impl RngCore for CountingRng {
    fn next_u32(&mut self) -> u32 {
        self.u32_calls += 1;
        self.inner.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.u64_calls += 1;
        self.inner.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.inner.fill_bytes(dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        self.inner.try_fill_bytes(dest)
    }
}

fn pickaxe(damage: u32) -> ItemStack {
    let mut stack = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test);
    stack.set_damage(damage);
    stack
}

fn enchanted(stack: ItemStack, id: &str, level: i32) -> ItemStack {
    let mut stack = stack;
    stack.components.enchantments = Some(Enchantments::new([(id.to_owned(), level)]));
    stack
}

#[tuff_core::watchdog]
#[test]
fn nbt_round_trip_of_plain_stacks() {
    let stack = ItemStack::mint(Item::Dirt, 12, MintReason::Test);
    assert_eq!(stack.to_nbt().get_str("id"), Some("minecraft:dirt"));
    assert!(!stack.to_nbt().contains_key("components"));
    assert_eq!(
        ItemStack::mint_from_nbt(&stack.to_nbt(), MintReason::Test),
        Some(stack)
    );
    let bare = Compound::new().with("id", "minecraft:stone");
    assert_eq!(
        ItemStack::mint_from_nbt(&bare, MintReason::Test),
        Some(ItemStack::mint(Item::Stone, 1, MintReason::Test))
    );
    assert_eq!(
        ItemStack::mint_from_nbt(&Compound::new().with("id", "modded:x"), MintReason::Test),
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn nbt_round_trip_keeps_typed_raw_and_removed_components() {
    let components = Compound::new()
        .with("minecraft:damage", 3)
        .with("minecraft:max_damage", 2000)
        .with("minecraft:unbreakable", Compound::new())
        .with("minecraft:custom_name", "Digger")
        .with(
            "minecraft:enchantments",
            Compound::new()
                .with("minecraft:unbreaking", 3)
                .with("minecraft:efficiency", 5),
        )
        .with(
            "minecraft:stored_enchantments",
            Compound::new().with("minecraft:mending", 1),
        )
        .with("minecraft:repair_cost", 7)
        .with(
            "minecraft:potion_contents",
            Compound::new().with("potion", "minecraft:swiftness").with(
                "custom_effects",
                vec![
                    Compound::new()
                        .with("id", "minecraft:speed")
                        .with("amplifier", 1i8)
                        .with("duration", 200)
                        .with("show_icon", true),
                ],
            ),
        )
        .with(
            "minecraft:custom_data",
            Compound::new().with("owner", "steve"),
        )
        .with("modded:thing", 4)
        .with("!minecraft:tool", Compound::new());
    let nbt = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test)
        .to_nbt()
        .with("components", components);
    let stack = ItemStack::mint_from_nbt(&nbt, MintReason::Test).unwrap();
    let c = &stack.components;
    assert_eq!(c.damage, Some(3));
    assert_eq!(c.max_damage, Some(2000));
    assert!(c.unbreakable);
    assert_eq!(c.custom_name, Some(Box::new(TextComponent::new("Digger"))));
    assert_eq!(
        c.enchantments.as_ref().unwrap().entries(),
        &[
            ("minecraft:efficiency".to_owned(), 5),
            ("minecraft:unbreaking".to_owned(), 3)
        ]
    );
    assert_eq!(stack.enchantment_level("minecraft:unbreaking"), 3);
    assert_eq!(
        c.stored_enchantments
            .as_ref()
            .unwrap()
            .level("minecraft:mending"),
        1
    );
    assert_eq!(c.repair_cost, Some(7));
    let potion = c.potion_contents.as_ref().unwrap();
    assert_eq!(potion.potion, Some(Potion::Swiftness));
    assert_eq!(potion.custom_effects[0].effect, MobEffect::Speed);
    assert_eq!(potion.custom_effects[0].details.amplifier, 1);
    assert_eq!(potion.custom_effects[0].details.duration, 200);
    assert_eq!(c.other.get_int("modded:thing"), Some(4));
    assert!(c.other.contains_key("minecraft:custom_data"));
    assert_eq!(c.removed, vec!["minecraft:tool".to_owned()]);
    assert!(c.is_removed("minecraft:tool"));

    let saved = stack.to_nbt();
    let reloaded = ItemStack::mint_from_nbt(&saved, MintReason::Test).unwrap();
    assert_eq!(reloaded, stack);
    let saved_components = saved.get_compound("components").unwrap();
    assert_eq!(
        saved_components.get_str("minecraft:custom_name"),
        Some("Digger")
    );
    assert!(saved_components.contains_key("!minecraft:tool"));
    let effect = saved_components
        .get_compound("minecraft:potion_contents")
        .unwrap()
        .get_list("custom_effects")
        .unwrap()[0]
        .as_compound()
        .unwrap()
        .clone();
    assert_eq!(effect.get_byte("amplifier"), Some(1));
    assert!(!effect.contains_key("ambient"));
    assert_eq!(effect.get_bool("show_icon"), Some(true));
}

#[tuff_core::watchdog]
#[test]
fn nbt_round_trip_of_suspicious_stew_effects() {
    let components = Compound::new().with(
        "minecraft:suspicious_stew_effects",
        vec![
            Compound::new()
                .with("id", "minecraft:night_vision")
                .with("duration", 100),
            Compound::new().with("id", "minecraft:hunger"),
        ],
    );
    let nbt = ItemStack::mint(Item::SuspiciousStew, 1, MintReason::Test)
        .to_nbt()
        .with("components", components);
    let stack = ItemStack::mint_from_nbt(&nbt, MintReason::Test).unwrap();
    let effects = stack.components.suspicious_stew_effects.as_ref().unwrap();
    assert_eq!(
        effects,
        &[
            SuspiciousEffect {
                effect: MobEffect::NightVision,
                duration: 100,
            },
            SuspiciousEffect {
                effect: MobEffect::Hunger,
                duration: 160,
            },
        ]
    );

    let saved = stack.to_nbt();
    assert_eq!(
        ItemStack::mint_from_nbt(&saved, MintReason::Test),
        Some(stack)
    );
    let saved_effects = saved
        .get_compound("components")
        .unwrap()
        .get_list("minecraft:suspicious_stew_effects")
        .unwrap();
    // The default duration (160) is omitted, matching the game's
    // lenient-optional-field codec.
    assert!(
        !saved_effects[1]
            .as_compound()
            .unwrap()
            .contains_key("duration")
    );
}

#[tuff_core::watchdog]
#[test]
fn unparseable_typed_components_stay_raw() {
    let components = Compound::new()
        .with("minecraft:damage", "three")
        .with(
            "minecraft:enchantments",
            Compound::new().with("minecraft:sharpness", 0),
        )
        .with("minecraft:potion_contents", "modded:brew")
        .with(
            "minecraft:custom_name",
            Compound::new().with("text", "x").with("color", "nope"),
        );
    let c = Components::from_nbt(&components);
    assert_eq!(c.damage, None);
    assert_eq!(c.enchantments, None);
    assert_eq!(c.potion_contents, None);
    assert_eq!(c.custom_name, None);
    assert_eq!(c.other.len(), 4);
    assert_eq!(c.to_nbt(), components);
}

#[tuff_core::watchdog]
#[test]
fn gear_components_are_typed_and_round_trip() {
    use crate::item::gear::samples;
    let mut stack = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    stack.components.attribute_modifiers = Some(samples::modifiers());
    stack.components.tool = Some(Box::new(samples::tool()));
    stack.components.weapon = Some(samples::weapon());
    let saved = stack.to_nbt();
    assert_eq!(
        ItemStack::mint_from_nbt(&saved, MintReason::Test),
        Some(stack.test_copy())
    );
    assert_eq!(stack.weapon(), Some(samples::weapon()));
    assert_eq!(stack.attribute_modifiers().len(), 3);
    assert!(stack.is_correct_tool_for_drops(tuff_vanilla_data::Block::Stone));
    let kinds: Vec<DataComponentType> = stack
        .components
        .network_added()
        .iter()
        .map(NetworkComponent::kind)
        .collect();
    assert_eq!(
        kinds,
        [
            DataComponentType::AttributeModifiers,
            DataComponentType::Tool,
            DataComponentType::Weapon
        ]
    );

    let broken = Compound::new().with(
        "minecraft:tool",
        Compound::new().with("rules", Tag::List(vec![Tag::String("x".into())])),
    );
    let c = Components::from_nbt(&broken);
    assert_eq!(c.tool, None);
    assert_eq!(c.to_nbt(), broken, "a tool this server cannot read is kept");
    let sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test).with_components(c);
    assert!(sword.tool().is_some(), "and the item default still applies");
}

#[tuff_core::watchdog]
#[test]
fn legacy_enchantment_levels_and_potion_strings_are_read() {
    let legacy = Tag::Compound(
        Compound::new().with("levels", Compound::new().with("minecraft:sharpness", 2)),
    );
    assert_eq!(
        Enchantments::from_nbt(&legacy)
            .unwrap()
            .level("minecraft:sharpness"),
        2
    );
    let contents = PotionContents::from_nbt(&Tag::String("minecraft:water".into())).unwrap();
    assert_eq!(contents.potion, Some(Potion::Water));
    assert_eq!(contents.to_nbt().get_str("potion"), Some("minecraft:water"));
}

#[tuff_core::watchdog]
#[test]
fn damage_tracks_max_damage_and_defaults() {
    let mut stack = pickaxe(0);
    assert_eq!(stack.max_damage(), Some(1561));
    assert!(stack.is_damageable());
    assert_eq!(stack.components.damage, None);
    stack.set_damage(10);
    assert_eq!(stack.components.damage, Some(10));
    stack.set_damage(0);
    assert_eq!(stack.components.damage, None);
    stack.set_damage(5000);
    assert_eq!(stack.damage(), 1561);

    let mut dirt = ItemStack::mint(Item::Dirt, 1, MintReason::Test);
    assert!(!dirt.is_damageable());
    dirt.components.max_damage = Some(10);
    assert!(!dirt.is_damageable());
    dirt.components.damage = Some(0);
    assert!(dirt.is_damageable());
    dirt.set_damage(0);
    assert_eq!(dirt.components.damage, Some(0));

    let mut unbreakable = pickaxe(0);
    unbreakable.components.unbreakable = true;
    assert!(!unbreakable.is_damageable());
    let mut no_max = pickaxe(0);
    no_max.components.removed = vec!["minecraft:max_damage".into()];
    assert_eq!(no_max.max_damage(), None);
    assert!(!no_max.is_damageable());
}

#[tuff_core::watchdog]
#[test]
fn only_a_mostly_intact_elytra_can_glide() {
    let mut elytra = ItemStack::mint(Item::Elytra, 1, MintReason::Test);
    assert!(elytra.can_glide());
    let max = elytra.max_damage().unwrap();
    elytra.set_damage(max - 2);
    assert!(elytra.can_glide(), "one hit left after this one");
    elytra.set_damage(max - 1);
    assert!(!elytra.can_glide(), "the next hit would break it");
    elytra.set_damage(max);
    assert!(!elytra.can_glide());
    assert!(!ItemStack::mint(Item::DiamondChestplate, 1, MintReason::Test).can_glide());
}

#[tuff_core::watchdog]
#[test]
fn hurt_damages_and_breaks_at_max_damage() {
    let mut rng = StdRng::seed_from_u64(1);
    let mut stack = pickaxe(0);
    assert_eq!(stack.hurt(1, &mut rng), Hurt::Damaged);
    assert_eq!(stack.damage(), 1);
    assert_eq!(stack.hurt(0, &mut rng), Hurt::Unchanged);
    stack.set_damage(1559);
    assert_eq!(stack.hurt(1, &mut rng), Hurt::Damaged);
    assert_eq!(stack.hurt(1, &mut rng), Hurt::Broken);
    assert_eq!(stack.count, 0);
    let mut big = pickaxe(1500);
    assert_eq!(big.hurt(500, &mut rng), Hurt::Broken);
    assert_eq!(big.damage(), 1561);

    let mut unbreakable = pickaxe(0);
    unbreakable.components.unbreakable = true;
    assert_eq!(unbreakable.hurt(5, &mut rng), Hurt::Unchanged);
    let mut dirt = ItemStack::mint(Item::Dirt, 1, MintReason::Test);
    assert_eq!(dirt.hurt(5, &mut rng), Hurt::Unchanged);
}

#[tuff_core::watchdog]
#[test]
fn repair_with_experience_spends_two_durability_per_point_capped_by_damage() {
    let stack = pickaxe(10);
    assert!(!stack.wants_mending(), "not enchanted yet");
    let mut stack = enchanted(stack.test_copy(), MENDING, 1);
    assert!(stack.wants_mending());
    assert_eq!(
        stack.repair_with_experience(3),
        3,
        "6 durability, all spent"
    );
    assert_eq!(stack.damage(), 4);
    assert!(stack.wants_mending(), "still damaged");
    assert_eq!(
        stack.repair_with_experience(10),
        2,
        "only 4 damage left, 2 xp needed"
    );
    assert_eq!(stack.damage(), 0);
    assert!(!stack.wants_mending(), "fully repaired");
    assert_eq!(stack.repair_with_experience(5), 0, "nothing left to repair");
    assert_eq!(stack.repair_with_experience(0), 0);

    let mut odd = enchanted(pickaxe(5), MENDING, 1);
    assert_eq!(
        odd.repair_with_experience(10),
        2,
        "5 damage repairs for 2 xp (floor of 5/2), one durability point free"
    );
    assert_eq!(odd.damage(), 0);
}

#[tuff_core::watchdog]
#[test]
fn unbreaking_skips_points_at_the_vanilla_rate() {
    let trials = 20_000u32;
    let rate = |stack: &ItemStack| {
        let mut rng = StdRng::seed_from_u64(7);
        let taken: u32 = (0..trials)
            .map(|_| {
                let mut probe = stack.test_copy();
                probe.set_damage(0);
                u32::from(probe.hurt(1, &mut rng) == Hurt::Damaged)
            })
            .sum();
        taken as f64 / trials as f64
    };
    let tool = enchanted(pickaxe(0), UNBREAKING, 3);
    assert!((rate(&tool) - 0.25).abs() < 0.02, "tool {}", rate(&tool));
    let armor = enchanted(
        ItemStack::mint(Item::IronChestplate, 1, MintReason::Test),
        UNBREAKING,
        1,
    );
    assert!((rate(&armor) - 0.8).abs() < 0.02, "armor {}", rate(&armor));
    let armor3 = enchanted(
        ItemStack::mint(Item::IronChestplate, 1, MintReason::Test),
        UNBREAKING,
        3,
    );
    assert!(
        (rate(&armor3) - 0.7).abs() < 0.02,
        "armor3 {}",
        rate(&armor3)
    );
    assert_eq!(rate(&pickaxe(0)), 1.0);
}

#[tuff_core::watchdog]
#[test]
fn dropped_points_switches_branch_exactly_at_129_points() {
    // chance = 0.5 puts both the expected-drops and expected-survivors
    // thresholds well above 20 for either point count, so the branch
    // taken depends only on `amount` crossing the 128-point line.
    let mut at_128 = CountingRng {
        inner: StdRng::seed_from_u64(3),
        u32_calls: 0,
        u64_calls: 0,
    };
    let dropped_128 = dropped_points(128, 0.5, &mut at_128);
    assert!(dropped_128 <= 128);
    assert_eq!(at_128.u32_calls, 128);
    assert_eq!(at_128.u64_calls, 0);

    let mut at_129 = CountingRng {
        inner: StdRng::seed_from_u64(3),
        u32_calls: 0,
        u64_calls: 0,
    };
    let dropped_129 = dropped_points(129, 0.5, &mut at_129);
    assert!(dropped_129 <= 129);
    assert_eq!(at_129.u32_calls, 0);
    assert_eq!(at_129.u64_calls, 2);
}

#[tuff_core::watchdog]
#[test]
fn dropped_points_stays_on_the_binomial_branch_away_from_the_normal_regime() {
    // n > 128 but chance small enough that the expected number of
    // drops (200 * 0.05 = 10) is still under the 20-point threshold,
    // so this must still roll every point individually.
    let mut rng = CountingRng {
        inner: StdRng::seed_from_u64(5),
        u32_calls: 0,
        u64_calls: 0,
    };
    let dropped = dropped_points(200, 0.05, &mut rng);
    assert!(dropped <= 200);
    assert_eq!(rng.u32_calls, 200);
    assert_eq!(rng.u64_calls, 0);
}

#[tuff_core::watchdog]
#[test]
fn dropped_points_normal_approximation_matches_binomial_mean_and_variance() {
    let amount = 100_000u32;
    let chance = 0.25f32;
    let trials = 2_000;
    let mut rng = StdRng::seed_from_u64(11);
    let samples: Vec<f64> = (0..trials)
        .map(|_| {
            let dropped = dropped_points(amount, chance, &mut rng);
            assert!(dropped <= amount);
            dropped as f64
        })
        .collect();

    let mean = samples.iter().sum::<f64>() / f64::from(trials);
    let variance = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / f64::from(trials);

    let expected_mean = (f64::from(amount) * f64::from(chance)).floor();
    let expected_variance = f64::from(amount) * f64::from(chance) * (1.0 - f64::from(chance));
    let expected_std_dev = expected_variance.sqrt();
    let mean_tolerance = 8.0 * expected_std_dev / (f64::from(trials)).sqrt();

    assert!(
        (mean - expected_mean).abs() < mean_tolerance,
        "mean {mean} too far from expected {expected_mean} (tolerance {mean_tolerance})"
    );
    assert!(
        (variance - expected_variance).abs() / expected_variance < 0.2,
        "variance {variance} too far from expected {expected_variance}"
    );
}

#[tuff_core::watchdog]
#[test]
fn stacks_merge_only_with_equal_components() {
    let plain = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test);
    assert!(plain.stacks_with(&plain.test_copy()));
    assert!(!plain.stacks_with(&pickaxe(3)));
    assert!(!plain.stacks_with(&enchanted(plain.test_copy(), UNBREAKING, 1)));
    assert!(pickaxe(3).stacks_with(&pickaxe(3)));
    let mut sized = ItemStack::mint(Item::Dirt, 1, MintReason::Test);
    assert_eq!(sized.max_stack_size(), 64);
    sized.components.other.insert(MAX_STACK_SIZE, 16);
    assert_eq!(sized.max_stack_size(), 16);
}

#[tuff_core::watchdog]
#[test]
fn network_view_hashes_like_the_client() {
    let hash = |component: NetworkComponent<'_>| component.hash();
    assert_eq!(hash(NetworkComponent::Damage(3)), -499649379);
    assert_eq!(hash(NetworkComponent::Unbreakable), -982207288);
    let enchantments = Enchantments::new([
        ("minecraft:unbreaking".to_owned(), 3),
        ("minecraft:efficiency".to_owned(), 5),
    ]);
    assert_eq!(
        hash(NetworkComponent::Enchantments(&enchantments)),
        -1704577720
    );
    let name = TextComponent::new("Sharp");
    assert_eq!(hash(NetworkComponent::CustomName(&name)), -1209906280);
    let swiftness = PotionContents {
        potion: Some(Potion::Swiftness),
        ..Default::default()
    };
    assert_eq!(
        hash(NetworkComponent::PotionContents(&swiftness)),
        -1164789092
    );
    let custom = PotionContents {
        potion: Some(Potion::Swiftness),
        custom_color: Some(0xFF00FF),
        custom_effects: vec![PotionEffect {
            effect: MobEffect::Speed,
            details: EffectDetails {
                amplifier: 1,
                duration: 200,
                ..Default::default()
            },
        }],
        custom_name: Some("nm".into()),
    };
    assert_eq!(hash(NetworkComponent::PotionContents(&custom)), 1089569010);
    let defaults = PotionContents {
        custom_effects: vec![PotionEffect {
            effect: MobEffect::Speed,
            details: EffectDetails {
                show_icon: false,
                ..Default::default()
            },
        }],
        ..Default::default()
    };
    assert_eq!(
        hash(NetworkComponent::PotionContents(&defaults)),
        -554377097
    );
    assert_eq!(custom.custom_effects[0].canon().hash() as i32, 1887375762);
}

#[tuff_core::watchdog]
#[test]
fn network_view_drops_what_cannot_be_encoded() {
    let mut components = Components::default();
    components
        .other
        .insert("minecraft:custom_data", Compound::new().with("k", 1));
    components.other.insert("minecraft:trim", Compound::new());
    components.other.insert("modded:thing", 1);
    components.other.insert(
        "minecraft:lore",
        Tag::List(vec![Tag::String("line".into())]),
    );
    components.removed = vec!["minecraft:tool".into(), "modded:gone".into()];
    let added = components.network_added();
    assert_eq!(
        added.iter().map(NetworkComponent::kind).collect::<Vec<_>>(),
        vec![DataComponentType::CustomData, DataComponentType::Lore]
    );
    assert_eq!(components.network_removed(), vec![DataComponentType::Tool]);
    let mut received = Components::default();
    for component in added {
        assert!(received.set_network(component));
    }
    received.remove_network(DataComponentType::Tool);
    assert_eq!(
        received.other.get_compound("minecraft:custom_data"),
        components.other.get_compound("minecraft:custom_data")
    );
    assert_eq!(received.removed, vec!["minecraft:tool".to_owned()]);
    assert!(!received.set_network(NetworkComponent::Nbt(DataComponentType::Trim, &Tag::Int(1))));
}

#[tuff_core::watchdog]
#[test]
fn use_effects_default_from_the_item_and_accept_stack_overrides() {
    let mut spear = ItemStack::mint(Item::IronSpear, 1, MintReason::Test);
    let effects = spear.use_effects();
    assert!(effects.can_sprint && !effects.interact_vibrations);
    assert_eq!(effects.speed_multiplier, 1.0);
    spear.components.other.insert(
        "minecraft:use_effects".to_string(),
        Tag::Compound(
            Compound::new()
                .with("interact_vibrations", true)
                .with("speed_multiplier", 0.5_f32),
        ),
    );
    let effects = spear.use_effects();
    assert!(effects.can_sprint && effects.interact_vibrations);
    assert_eq!(effects.speed_multiplier, 0.5);
    let bow = ItemStack::mint(Item::Bow, 1, MintReason::Test).use_effects();
    assert!(!bow.can_sprint && bow.interact_vibrations);
    assert_eq!(bow.speed_multiplier, 0.2);
}

#[tuff_core::watchdog]
#[test]
fn a_stacks_own_use_components_override_the_items_defaults() {
    let mut stone = ItemStack::mint(Item::Stone, 1, MintReason::Test);
    assert_eq!(stone.use_cooldown(), None);
    assert_eq!(stone.use_remainder(), None);
    let mut cooldown = Compound::new();
    cooldown.insert("seconds", 2.5f32);
    cooldown.insert("cooldown_group", "tuff:test");
    stone
        .components
        .other
        .insert("minecraft:use_cooldown", cooldown);
    assert_eq!(
        stone.use_cooldown(),
        Some(StackCooldown {
            ticks: 50,
            group: Some("tuff:test".to_owned())
        })
    );
    let mut remainder = Compound::new();
    remainder.insert("id", "minecraft:bowl");
    remainder.insert("count", 2i32);
    stone
        .components
        .other
        .insert("minecraft:use_remainder", remainder);
    let left = stone.use_remainder().unwrap();
    assert_eq!((left.item, left.count), (Item::Bowl, 2));

    let mut milk = ItemStack::mint(Item::MilkBucket, 1, MintReason::Test);
    let bucket = milk.use_remainder().unwrap();
    assert_eq!(bucket.item, Item::Bucket);
    bucket.burn(crate::item::BurnReason::Test);
    milk.components
        .removed
        .push("minecraft:use_remainder".to_owned());
    assert_eq!(milk.use_remainder(), None);
}

#[tuff_core::watchdog]
#[test]
fn death_protection_comes_from_the_component_not_the_item() {
    let totem = ItemStack::mint(Item::TotemOfUndying, 1, MintReason::Test);
    assert_eq!(totem.death_protection().map(|e| e.len()), Some(4));

    let mut removed = totem.test_copy();
    removed
        .components
        .removed
        .push("minecraft:death_protection".into());
    assert!(removed.death_protection().is_none());

    let mut custom = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    assert!(custom.death_protection().is_none());
    let effect = Compound::new().with("type", "minecraft:clear_all_effects");
    custom.components.other.insert(
        "minecraft:death_protection",
        Compound::new().with("death_effects", Tag::List(vec![Tag::Compound(effect)])),
    );
    assert_eq!(
        custom.death_protection(),
        Some(vec![DeathEffect::ClearAllEffects])
    );
}

#[tuff_core::watchdog]
#[test]
fn spears_stab_and_other_items_whack() {
    assert_eq!(
        ItemStack::mint(Item::Stick, 1, MintReason::Test).attack_animation(),
        (SwingKind::Whack, 6)
    );
    let (kind, duration) =
        ItemStack::mint(Item::WoodenSpear, 1, MintReason::Test).attack_animation();
    assert_eq!(kind, SwingKind::Stab);
    assert!(duration > 6);
}

#[tuff_core::watchdog]
#[test]
fn transform_records_a_burn_and_a_mint_and_keeps_count_and_components() {
    let mut stack = ItemStack::mint(Item::EnchantedBook, 5, MintReason::Command);
    stack.components.repair_cost = Some(3);
    let before = conservation::totals();
    stack.transform(Item::Book, MintReason::Transform, BurnReason::Transform);
    let after = conservation::totals();
    assert_eq!(stack.item(), Item::Book);
    assert_eq!(stack.count(), 5);
    assert_eq!(stack.components.repair_cost, Some(3));
    assert!(!stack.is_picture());
    assert!(after.burned >= before.burned + 5);
    assert!(after.minted >= before.minted + 5);
    stack.burn(BurnReason::Test);

    let mut picture = ItemStack::mint(Item::Map, 1, MintReason::Test);
    picture.transform(
        Item::WoodlandMansionMap,
        MintReason::Transform,
        BurnReason::Transform,
    );
    assert_eq!(picture.item(), Item::WoodlandMansionMap);
    assert!(picture.is_picture());
}
