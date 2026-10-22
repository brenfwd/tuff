use super::*;
use rand::SeedableRng;
use tuff_core::world::entity::EntitySummary;

#[tuff_core::watchdog]
#[test]
fn an_angry_zombified_piglin_speeds_up_and_a_calm_one_does_not() {
    use tuff_vanilla_data::registry::Attribute;
    crate::tests::with_context(|core, cx| {
        let mut piglin = ZombifiedPiglin::default();
        piglin.ai_step(core, cx);
        assert_eq!(
            core.modifier_amount(Attribute::MovementSpeed, ATTACKING_MODIFIER),
            None
        );
        core.anger = Some(crate::target::Anger {
            target: 1,
            ticks: 400,
            universal: false,
        });
        piglin.ai_step(core, cx);
        assert_eq!(
            core.modifier_amount(Attribute::MovementSpeed, ATTACKING_MODIFIER),
            Some(0.05)
        );
        core.anger = None;
        piglin.ai_step(core, cx);
        assert_eq!(
            core.modifier_amount(Attribute::MovementSpeed, ATTACKING_MODIFIER),
            None
        );
    });
}

#[tuff_core::watchdog]
#[test]
fn a_new_target_starts_the_alert_and_anger_sound_timers() {
    crate::tests::with_context(|core, cx| {
        let mut piglin = ZombifiedPiglin::default();
        core.target = Some(7);
        piglin.track_target(core, cx);
        assert!((80..=120).contains(&piglin.next_alert));
        assert!(piglin.first_anger_sound_in <= 20);
    });
}

#[tuff_core::watchdog]
#[test]
fn a_brute_zombifies_after_fifteen_seconds_unless_immune() {
    for immune in [false, true] {
        crate::tests::with_context(|core, cx| {
            let mut brute = PiglinBrute {
                immune_to_zombification: immune,
                time_in_overworld: 0,
            };
            for _ in 0..=CONVERSION_TICKS {
                brute.tick(core, cx);
            }
            let spawned = cx.effects.iter().any(|e| matches!(e, Effect::Spawn(_)));
            assert_eq!(spawned, !immune);
            assert_eq!(core.discarded, !immune);
        });
    }
}

#[tuff_core::watchdog]
#[test]
fn a_brute_keeps_a_home_and_only_takes_a_golden_axe() {
    crate::tests::with_context(|core, cx| {
        let mut brute = PiglinBrute::default();
        brute.finalize_spawn(core, cx);
        assert!(core.home.is_some());
        core.equipment.set(EquipmentSlot::Mainhand, None);
        brute.took_item(core, ItemStack::mint(Item::GoldIngot, 1, MintReason::Test));
        assert!(core.equipment.get(EquipmentSlot::Mainhand).is_none());
        brute.took_item(core, ItemStack::mint(Item::GoldenAxe, 1, MintReason::Test));
        assert_eq!(
            core.equipment
                .get(EquipmentSlot::Mainhand)
                .map(|s| s.item()),
            Some(Item::GoldenAxe)
        );
    });
}

#[tuff_core::watchdog]
#[test]
fn arming_a_zombified_piglin_keeps_a_handed_item() {
    crate::tests::with_context(|core, cx| {
        let fungus = ItemStack::mint(Item::WarpedFungusOnAStick, 1, MintReason::Test);
        core.equipment
            .set(EquipmentSlot::Mainhand, Some(fungus.test_copy()));
        arm_zombified(core, cx);
        assert_eq!(core.equipment.get(EquipmentSlot::Mainhand), Some(&fungus));
        core.equipment.set(EquipmentSlot::Mainhand, None);
        arm_zombified(core, cx);
        assert!(core.equipment.get(EquipmentSlot::Mainhand).is_some());
    });
}

#[tuff_core::watchdog]
#[test]
fn spawned_piglins_may_be_babies_or_carry_a_crossbow_sword_or_spear() {
    let mut babies = 0;
    let mut weapons = std::collections::HashMap::new();
    super::super::tests::with_context(|core, cx| {
        for seed in 0..1000 {
            *cx.rng = rand::rngs::StdRng::seed_from_u64(seed);
            core.equipment = Default::default();
            core.baby = false;
            core.age = None;
            Piglin::default().finalize_spawn(core, cx);
            if core.is_baby() {
                babies += 1;
                assert!(core.equipment.main_hand().is_none());
            } else {
                let weapon = core.equipment.main_hand().unwrap().item();
                *weapons.entry(weapon).or_insert(0) += 1;
            }
        }
    });
    assert!((140..260).contains(&babies), "{babies}");
    let crossbows = weapons[&Item::Crossbow];
    assert!((330..470).contains(&crossbows), "{crossbows}");
    assert!(weapons[&Item::GoldenSword] > weapons[&Item::GoldenSpear]);
}

#[tuff_core::watchdog]
#[test]
fn structure_piglins_skip_the_baby_roll_and_the_spawn_weapon() {
    super::super::tests::with_context(|core, cx| {
        cx.state.get::<crate::spawning::SettingUp>().0 =
            Some(crate::spawning::SpawnReason::Structure);
        for seed in 0..200 {
            *cx.rng = rand::rngs::StdRng::seed_from_u64(seed);
            core.equipment = Default::default();
            core.baby = false;
            core.age = None;
            Piglin::default().finalize_spawn(core, cx);
            assert!(!core.is_baby());
            assert!(core.equipment.main_hand().is_none());
        }
    });
}

#[tuff_core::watchdog]
#[test]
fn pockets_hold_eight_stacks() {
    let mut brain = PiglinBrain::default();
    for _ in 0..8 {
        assert!(
            brain
                .add(ItemStack::mint(Item::GoldNugget, 64, MintReason::Test))
                .is_none()
        );
    }
    assert!(!brain.can_add(Item::Stick));
    assert!(
        brain
            .add(ItemStack::mint(Item::Stick, 1, MintReason::Test))
            .is_some()
    );
}

pub(crate) fn summary(id: i32, kind: EntityType, x: f64, age: i32, alive: bool) -> EntitySummary {
    EntitySummary {
        id,
        uuid: id as u128,
        kind,
        position: [x, 64.0, 0.5],
        velocity: [0.0; 3],
        yaw: 0.0,
        bounds: [x - 0.3, 64.0, 0.2, x + 0.3, 64.9, 0.8],
        living: Some(tuff_core::world::entity::LivingSummary {
            unpushable: false,
            alive,
            target: None,
            age,
            in_love: false,
            panicking: false,
            eye_height: 0.8,
            in_water: false,
            seen_as_enemy: true,
            enemy: false,
            health: 10.0,
            tamed: false,
            owner: None,
            chasing: None,
        }),
        pickable: true,
        item: None,
        vehicle: None,
        carries: false,
        ominous_banner: false,
        blue_sheep: false,
    }
}

#[tuff_core::watchdog]
#[test]
fn babies_run_from_a_wither_skeleton_in_sight_and_adults_do_not() {
    let entities = [summary(2, EntityType::WitherSkeleton, 3.5, 0, true)];
    crate::tests::with_entities(&entities, |core, cx| {
        core.baby = true;
        assert!(AvoidZombified::default().can_use(core, cx));
        core.baby = false;
        assert!(!AvoidZombified::default().can_use(core, cx));
        assert!(!grown_up(core, cx) || visible_nemesis(core, &entities[0], cx));
    });
}

#[tuff_core::watchdog]
#[test]
fn only_adult_piglins_pick_a_wither_skeleton_as_a_target() {
    let entities = [summary(2, EntityType::WitherSkeleton, 3.5, 0, true)];
    crate::tests::with_entities(&entities, |core, cx| {
        let mut goal = NearestAttackableEntity::new(NEMESES, true).when(grown_up);
        core.baby = true;
        assert!(!(0..500).any(|_| goal.can_use(core, cx)));
        core.baby = false;
        assert!((0..500).any(|_| goal.can_use(core, cx)));
    });
}

#[tuff_core::watchdog]
#[test]
fn dance_rolls_match_the_time_seeded_random() {
    assert!((time_seeded_float(0) - 0.730_967_76).abs() < 1e-6);
    assert!((time_seeded_float(42) - 0.727_563_7).abs() < 1e-6);
}

#[tuff_core::watchdog]
#[test]
fn hoglins_outnumber_piglins_counting_itself() {
    let mut entities = vec![
        summary(2, EntityType::Hoglin, 3.5, 0, true),
        summary(3, EntityType::Hoglin, 4.5, -100, true),
    ];
    crate::tests::with_entities(&entities, |core, cx| {
        assert!(!hoglins_outnumber_piglins(core, cx), "babies do not count");
    });
    entities.push(summary(5, EntityType::Hoglin, 6.5, 0, true));
    crate::tests::with_entities(&entities, |core, cx| {
        assert!(hoglins_outnumber_piglins(core, cx));
    });
    entities.push(summary(6, EntityType::PiglinBrute, -3.5, 0, true));
    crate::tests::with_entities(&entities, |core, cx| {
        assert!(!hoglins_outnumber_piglins(core, cx));
    });
}

#[tuff_core::watchdog]
#[test]
fn retreating_drops_the_fight_and_rests_from_hunting() {
    crate::tests::with_context(|core, cx| {
        core.target = Some(9);
        start_retreat(core, 9, cx);
        assert_eq!(core.target, None);
        let (from, until) = core.piglin.retreat.unwrap();
        assert_eq!(from, 9);
        assert!(RETREAT_TICKS.contains(&(until - cx.tick)));
        assert!(core.piglin.next_hunt > cx.tick);
    });
}

#[tuff_core::watchdog]
#[test]
fn a_dead_target_is_celebrated_where_it_fell() {
    let entities = [summary(2, EntityType::Hoglin, 3.5, 0, false)];
    crate::tests::with_entities(&entities, |core, cx| {
        core.target = Some(2);
        celebrate_kills(core, cx);
        assert_eq!(core.target, None);
        let celebration = core.piglin.celebrate.unwrap();
        assert_eq!(celebration.at, [3.5, 64.0, 0.5]);
        assert_eq!(celebration.until, cx.tick + CELEBRATE_TICKS);
        assert_eq!(
            celebration.dancing,
            time_seeded_float(cx.tick) < DANCE_CHANCE
        );
        assert!(core.piglin.next_hunt > cx.tick);
        cx.tick += CELEBRATE_TICKS;
        celebrate_kills(core, cx);
        assert_eq!(core.piglin.celebrate, None);
    });
}

#[tuff_core::watchdog]
#[test]
fn babies_pick_a_baby_hoglin_in_sight_to_ride() {
    let entities = [
        summary(2, EntityType::Hoglin, 6.5, 0, true),
        summary(3, EntityType::Hoglin, 4.5, -100, true),
        summary(4, EntityType::Hoglin, 30.5, -100, true),
    ];
    crate::tests::with_entities(&entities, |core, cx| {
        update_ride(core, cx);
        assert_eq!(core.piglin.ride, None, "adults never ride");
        core.baby = true;
        core.piglin.ride_ticker = 0;
        update_ride(core, cx);
        let (hoglin, until) = core.piglin.ride.unwrap();
        assert_eq!(hoglin, 3);
        assert!(RIDE_TICKS.contains(&(until - cx.tick)));
        assert!(RIDE_START_INTERVAL.contains(&core.piglin.ride_ticker));
    });
}

#[tuff_core::watchdog]
#[test]
fn only_lit_soul_campfires_repel() {
    let campfire = tuff_vanilla_data::Block::SoulCampfire.default_state();
    let lit = campfire.with("lit", "true").unwrap();
    let unlit = campfire.with("lit", "false").unwrap();
    assert!(is_piglin_repellent(lit));
    assert!(!is_piglin_repellent(unlit));
    assert!(is_piglin_repellent(
        tuff_vanilla_data::Block::SoulLantern.default_state()
    ));
    assert!(!is_piglin_repellent(
        tuff_vanilla_data::Block::Lantern.default_state()
    ));
}

#[tuff_core::watchdog]
#[test]
fn a_piglin_gives_up_walking_to_a_loved_item_for_two_hundred_ticks() {
    let mut gold = summary(9, EntityType::Item, 3.5, 0, true);
    gold.item = Some((Item::GoldIngot, 1, true));
    crate::tests::with_entities(&[gold], |core, cx| {
        core.can_pick_up_loot = true;
        let mut seek = SeekItem::default();
        assert!(seek.can_use(core, cx));
        core.piglin.reach_ticks = MAX_REACH_TICKS;
        assert!(seek.can_continue(core, cx));
        core.piglin.reach_ticks += 1;
        assert!(!seek.can_continue(core, cx));
        assert!(!seek.can_use(core, cx));
        cx.tick += REACH_GIVE_UP_DISABLE_TICKS;
        assert!(seek.can_use(core, cx));
    });
}
