use rand::SeedableRng;

use super::*;
use tuff_core::world::entity::EntityBase;

fn load_entity(base: EntityBase, nbt: &mut Compound) -> Box<dyn RegionEntity> {
    let load = match base.kind {
        EntityType::Horse => super::super::load::<Horse>,
        EntityType::Donkey => super::super::load::<Donkey>,
        EntityType::Mule => super::super::load::<Mule>,
        EntityType::SkeletonHorse => super::super::load::<SkeletonHorse>,
        _ => super::super::load::<ZombieHorse>,
    };
    load(base, nbt).expect("loads")
}

fn tamed(kind: EntityType, extra: Compound) -> Box<dyn RegionEntity> {
    let base = EntityBase::new(kind, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
    let mut nbt = extra.with("Owner", Tag::uuid(77));
    load_entity(base, &mut nbt)
}

fn click(item: Item) -> EntityInteraction {
    EntityInteraction {
        player: 5,
        player_uuid: 77,
        position: [0.0; 3],
        location: [0.0; 3],
        stack: Some(tuff_item::item::ItemSnapshot::describe(
            item,
            1,
            Default::default(),
        )),
        hand: tuff_core::world::entity::HandOffer::new(
            (Some(tuff_item::item::ItemSnapshot::describe(
                item,
                1,
                Default::default(),
            )))
            .as_deref()
            .map(tuff_item::item::ItemStack::test_copy),
        ),
        off_hand: false,
        creative: false,
        sneaking: false,
        riding: false,
        hero: None,
    }
}

#[tuff_core::watchdog]
#[test]
fn only_the_undead_horses_never_grow_up() {
    for (kind, expected) in [
        (EntityType::SkeletonHorse, false),
        (EntityType::ZombieHorse, false),
        (EntityType::Horse, true),
    ] {
        let mut core = MobCore::new(EntityBase::new(kind, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]));
        match kind {
            EntityType::SkeletonHorse => SkeletonHorse::configure(&mut core),
            EntityType::ZombieHorse => ZombieHorse::configure(&mut core),
            _ => Horse::configure(&mut core),
        }
        let age = core.age.as_mut().expect("animal");
        age.age = -100;
        assert_eq!(age.can_age_up(), expected, "{kind:?}");
    }
}

#[tuff_core::watchdog]
#[test]
fn a_tamed_saddled_horse_loads_as_drivable() {
    let entity = tamed(
        EntityType::Horse,
        Compound::new().with(
            "equipment",
            Compound::new().with(
                "saddle",
                ItemStack::mint(Item::Saddle, 1, MintReason::Test).to_nbt(),
            ),
        ),
    );
    assert!(entity.rideable_by_input());
    assert_eq!(
        entity
            .container()
            .map(|slots| slots[tuff_core::world::menu::mount::SADDLE]
                .as_ref()
                .map(tuff_item::item::ItemStack::test_copy)),
        Some(Some(ItemStack::mint(Item::Saddle, 1, MintReason::Test))),
        "the screen's saddle slot shows the worn saddle"
    );
}

#[tuff_core::watchdog]
#[test]
fn offspring_attribute_averages_within_the_parents_spread() {
    let mut rng = StdRng::seed_from_u64(1);
    for _ in 0..200 {
        let value = offspring_attribute(15.0, 30.0, MIN_MAX_HEALTH, MAX_MAX_HEALTH, &mut rng);
        assert!(
            (MIN_MAX_HEALTH..=MAX_MAX_HEALTH).contains(&value),
            "{value}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn rolled_attributes_stay_in_their_documented_range() {
    let mut rng = StdRng::seed_from_u64(2);
    for _ in 0..500 {
        let health = roll_health(&mut rng);
        let speed = roll_speed(&mut rng);
        let jump = roll_jump(&mut rng);
        assert!(
            (MIN_MAX_HEALTH..=MAX_MAX_HEALTH).contains(&health),
            "{health}"
        );
        assert!((MIN_SPEED..=MAX_SPEED).contains(&speed), "{speed}");
        assert!((MIN_JUMP..=MAX_JUMP).contains(&jump), "{jump}");
        let zombie_speed = roll_zombie_speed(&mut rng);
        assert!((0.213..=0.285).contains(&zombie_speed), "{zombie_speed}");
        let zombie_jump = roll_zombie_jump(&mut rng);
        assert!((0.5..=0.7).contains(&zombie_jump), "{zombie_jump}");
    }
}

#[tuff_core::watchdog]
#[test]
fn drive_input_halves_backward_and_sideways_motion() {
    let (forward, strafe) = drive_input(RiderInput {
        forward: -1.0,
        strafe: 1.0,
        ..Default::default()
    });
    assert_eq!(forward, -0.25);
    assert_eq!(strafe, 0.5);
}

#[tuff_core::watchdog]
#[test]
fn a_released_jump_scales_with_its_charge() {
    assert_eq!(jump_scale(100), 1.0);
    assert_eq!(jump_scale(90), 1.0);
    assert!((jump_scale(45) - 0.6).abs() < 1.0e-6);
    assert!(jump_scale(1) > 0.4 && jump_scale(1) < 0.41);
}

#[tuff_core::watchdog]
#[test]
fn horses_and_donkeys_court_each_other_but_mules_only_mules() {
    assert!(mates(EntityType::Horse, EntityType::Donkey));
    assert!(mates(EntityType::Donkey, EntityType::Horse));
    assert!(mates(EntityType::Horse, EntityType::Horse));
    assert!(!mates(EntityType::Mule, EntityType::Horse));
    assert!(!mates(EntityType::Llama, EntityType::Horse));
}

#[tuff_core::watchdog]
#[test]
fn feeding_heals_raises_temper_and_golden_food_needs_a_tamed_adult() {
    super::super::tests::with_context(|core, cx| {
        Horse::configure(core);
        let mut equine = Equine::default();
        core.health = 5.0;
        assert!(equine.feed(core, &HORSE, Item::Wheat, 77, cx));
        assert_eq!(core.health, 7.0);
        assert_eq!(equine.temper, 3);
        assert!(equine.mouth_open, "eating opens its mouth");
        core.health = core.max_health();
        assert!(
            equine.feed(core, &HORSE, Item::Sugar, 77, cx),
            "an untamed horse still takes temper from food at full health"
        );
        assert_eq!(equine.temper, 6);
        assert!(
            !equine.feed(core, &HORSE, Item::HayBlock, 77, cx),
            "hay gives no temper, so a healthy adult refuses it"
        );
        equine.feed(core, &HORSE, Item::GoldenCarrot, 77, cx);
        assert!(
            !core.love.as_ref().unwrap().is_in_love(),
            "an untamed horse never falls in love"
        );
        core.tame.as_mut().unwrap().owner = Some(77);
        equine.temper = MAX_TEMPER;
        assert!(equine.feed(core, &HORSE, Item::GoldenCarrot, 77, cx));
        assert!(core.love.as_ref().unwrap().is_in_love());
        assert!(
            !equine.feed(core, &HORSE, Item::Wheat, 77, cx),
            "a tamed horse at full health and max temper refuses wheat"
        );
    });
}

#[tuff_core::watchdog]
#[test]
fn a_fed_foal_grows_up_faster() {
    super::super::tests::with_context(|core, cx| {
        Horse::configure(core);
        core.set_baby(true);
        let mut equine = Equine::default();
        let before = core.age.as_ref().unwrap().age;
        assert!(equine.feed(core, &HORSE, Item::HayBlock, 77, cx));
        assert_eq!(core.age.as_ref().unwrap().age, before + 180 * 20);
    });
}

#[tuff_core::watchdog]
#[test]
fn an_untamed_horse_rears_at_an_offered_item_instead_of_taking_a_rider() {
    super::super::tests::with_context(|_, cx| {
        let base = EntityBase::new(EntityType::Horse, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
        let mut horse = load_entity(base, &mut Compound::new());
        assert!(!horse.mount_refused_by(None), "an empty hand mounts it");
        let stick = click(Item::Stick);
        assert!(horse.mount_refused_by(stick.stack.as_deref()));
        assert_eq!(horse.interact(&stick, cx), Some(HandEffect::None));
        let horse = horse.downcast_ref::<Mob<Horse>>().unwrap();
        assert!(horse.species.equine.is_standing());
    });
}

#[tuff_core::watchdog]
#[test]
fn a_horse_rears_at_random_only_after_its_stand_interval() {
    super::super::tests::with_context(|core, cx| {
        let mut equine = Equine::default();
        for _ in 0..STAND_INTERVAL {
            equine.random_stand(core, &HORSE, None, cx);
            assert!(!equine.is_standing());
        }
        let mut rose = false;
        for _ in 0..200_000 {
            equine.random_stand(core, &HORSE, None, cx);
            rose |= equine.is_standing();
            equine.standing = false;
        }
        assert!(rose, "a horse eventually rears up unprompted");
        let mut skeleton = Equine::default();
        for _ in 0..200_000 {
            skeleton.random_stand(
                core,
                &Profile {
                    rears: false,
                    ..HORSE
                },
                None,
                cx,
            );
            assert!(!skeleton.is_standing());
        }
    });
}

#[tuff_core::watchdog]
#[test]
fn a_rearing_or_grazing_horse_is_immobile() {
    super::super::tests::with_context(|_, cx| {
        let base = EntityBase::new(EntityType::Horse, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
        let mut horse = load_entity(base, &mut Compound::new());
        let stick = click(Item::Stick);
        horse.interact(&stick, cx);
        let horse = horse.downcast_mut::<Mob<Horse>>().unwrap();
        assert!(horse.species.immobile(&horse.core), "rearing freezes it");
        let mut grazing = Horse::default();
        grazing.equine.eating = true;
        assert!(grazing.immobile(&horse.core), "grazing freezes it");
        assert!(!Horse::default().immobile(&horse.core));
    });
}

#[tuff_core::watchdog]
#[test]
fn a_tamed_horse_takes_a_saddle_and_armor_from_a_plain_click() {
    super::super::tests::with_context(|_, cx| {
        let mut horse = tamed(EntityType::Horse, Compound::new());
        let saddle = click(Item::Saddle);
        assert!(horse.mount_refused_by(saddle.stack.as_deref()));
        assert_eq!(horse.interact(&saddle, cx), Some(HandEffect::None));
        assert!(horse.rideable_by_input());
        let armor = click(Item::IronHorseArmor);
        assert!(horse.mount_refused_by(armor.stack.as_deref()));
        assert_eq!(horse.interact(&armor, cx), Some(HandEffect::None));
        let slots = horse.container().unwrap();
        assert_eq!(
            slots[tuff_core::world::menu::mount::BODY]
                .as_ref()
                .map(|s| s.item()),
            Some(Item::IronHorseArmor)
        );
        assert!(
            !horse.mount_refused_by(click(Item::Stick).stack.as_deref()),
            "anything else mounts a tamed horse"
        );
        let mut donkey = tamed(EntityType::Donkey, Compound::new());
        assert!(
            !donkey.mount_refused_by(armor.stack.as_deref()),
            "donkeys wear no armor"
        );
        assert_eq!(donkey.interact(&armor, cx), None);
    });
}

#[tuff_core::watchdog]
#[test]
fn a_removed_donkey_gives_up_its_saddle_and_chest_contents_once() {
    super::super::tests::with_context(|_, cx| {
        let mut donkey = tamed(EntityType::Donkey, Compound::new());
        donkey.interact(&click(Item::Chest), cx);
        let mut slots = donkey.container_mut().unwrap();
        slots[tuff_core::world::menu::mount::SADDLE] =
            Some(ItemStack::mint(Item::Saddle, 1, MintReason::Test));
        slots[4] = Some(ItemStack::mint(Item::Diamond, 3, MintReason::Test));
        let mut taken: Vec<_> = donkey.take_items().iter().map(|s| s.item()).collect();
        taken.sort_by_key(|item| *item as u32);
        let mut want = vec![Item::Saddle, Item::Diamond];
        want.sort_by_key(|item| *item as u32);
        assert_eq!(taken, want);
        assert!(donkey.container().unwrap().iter().all(Option::is_none));
        assert!(donkey.take_items().is_empty());
    });
}

#[tuff_core::watchdog]
#[test]
fn a_donkey_chest_opens_a_fifteen_slot_inventory_that_saves() {
    super::super::tests::with_context(|_, cx| {
        let mut donkey = tamed(EntityType::Donkey, Compound::new());
        assert_eq!(donkey.container().unwrap().len(), 2);
        assert_eq!(
            donkey.interact(&click(Item::Chest), cx),
            Some(HandEffect::Consume(1))
        );
        assert_eq!(donkey.container().unwrap().len(), 2 + 15);
        assert_eq!(
            donkey.container_menu(),
            MenuKind::Mount {
                columns: 5,
                saddle: true,
                armor: MountArmor::None
            }
        );
        donkey.container_mut().unwrap()[4] =
            Some(ItemStack::mint(Item::Diamond, 3, MintReason::Test));
        let saved = tuff_core::world::entity::save(&*donkey);
        assert_eq!(saved.get("ChestedHorse"), Some(&Tag::Byte(1)));
        let mut nbt = saved.clone();
        let base = EntityBase::new(EntityType::Donkey, 2, 2, [0.5, 64.0, 0.5], [0.0; 3]);
        let loaded = load_entity(base, &mut nbt);
        assert_eq!(
            loaded.container().unwrap()[4],
            Some(ItemStack::mint(Item::Diamond, 3, MintReason::Test))
        );
    });
}

#[tuff_core::watchdog]
#[test]
fn a_saddle_put_in_the_screen_is_worn_once_with_its_drop_chance() {
    super::super::tests::with_context(|core, cx| {
        Horse::configure(core);
        core.tame.as_mut().unwrap().owner = Some(77);
        let mut equine = Equine::default();
        equine.inventory.adopt(core);
        let [saddle, _] = core.equipment.mount_slots_mut();
        *saddle = Some(ItemStack::mint(Item::Saddle, 1, MintReason::Test));
        equine.inventory.chest_from_screen();
        equine.inventory.sync(core, cx);
        assert!(saddled(core));
        assert_eq!(core.equipment.drop_chance(EquipmentSlot::Saddle), 2.0);
        let saddle = core.equipment.take(EquipmentSlot::Saddle);
        equine.inventory.sync(core, cx);
        assert!(saddle.is_some());
        assert!(core.equipment.mount_slots()[0].is_none());
    });
}

#[tuff_core::watchdog]
#[test]
fn only_a_sneaking_owner_opens_the_inventory() {
    super::super::tests::with_context(|core, _| {
        Horse::configure(core);
        assert!(!Equine::opens_for(core, 5, true), "untamed");
        core.tame.as_mut().unwrap().owner = Some(77);
        assert!(!Equine::opens_for(core, 5, false), "a plain click");
        assert!(Equine::opens_for(core, 5, true));
    });
}

#[tuff_core::watchdog]
#[test]
fn mounting_an_untamed_horse_eventually_tames_it_or_bucks_it_off() {
    let (handle, _events) = tuff_core::world::messages::PlayerHandle::new(1);
    let mount = |core: &mut MobCore| {
        core.base.riders.push(tuff_core::world::entity::Rider {
            id: 1,
            uuid: 42,
            handle: handle.clone(),
        });
    };
    super::super::tests::with_context(|core, cx| {
        Horse::configure(core);
        mount(core);
        let mut equine = Equine::default();
        let mut bucked = 0;
        for _ in 0..20_000 {
            if is_tame(core) {
                break;
            }
            if core.base.riders.is_empty() {
                bucked += 1;
                mount(core);
            }
            equine.tame_rider(core, &HORSE, cx);
        }
        assert!(is_tame(core), "never tamed after 20000 tries");
        assert!(
            bucked > 0,
            "a mount attempt should fail at least once at temper 0"
        );
    });
}
