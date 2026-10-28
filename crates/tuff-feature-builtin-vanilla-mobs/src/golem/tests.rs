use super::*;
use crate::Mob;
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{EntityBase, Notice};
use tuff_vanilla_data::Block;

fn click(item: Item) -> EntityInteraction {
    EntityInteraction {
        player: 1,
        player_uuid: 1,
        position: [0.0; 3],
        location: [0.0; 3],
        stack: Some(tuff_item::item::ItemSnapshot::new(item, 1)),
        hand: tuff_core::world::entity::HandOffer::new(
            (Some(tuff_item::item::ItemSnapshot::new(item, 1)))
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

fn mob<S: Species>(kind: EntityType, at: [f64; 3], cx: &mut TickContext) -> Mob<S> {
    Mob::<S>::new(EntityBase::new(kind, 2, 2, at, [0.0; 3]), cx.rng)
}

#[tuff_core::watchdog]
#[test]
fn iron_golem_hits_roll_their_damage_and_throw_the_victim_up() {
    crate::tests::with_context(|_, cx| {
        let golem = mob::<IronGolem>(EntityType::IronGolem, [0.5, 64.0, 0.5], cx);
        let target = TargetView::of_player(&tuff_core::world::entity::NearbyPlayer {
            id: 9,
            uuid: 9,
            name: "p",
            position: [1.5, 64.0, 0.5],
            eye_height: 1.62,
            yaw: 0.0,
            pitch: 0.0,
            held: [None; 2],
            creative: false,
            height: 1.8,
            sneaking: false,
            invisible: false,
            invulnerable: false,
            armor: [None; 4],
            effects: 0,
            movement: [0.0; 3],
            vehicle: None,
            sleeping: None,
            last_hurt_by: None,
            last_hurt_mob: None,
            perchable: false,
            sprinting: false,
        });
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..400 {
            let mut hit = tuff_core::world::entity::EntityHit::new(
                15.0,
                &tuff_vanilla_data::damage_type::MOB_ATTACK,
            );
            iron_golem_hit(&golem.core, &target, &mut hit, cx);
            assert!((7.5..=21.5).contains(&hit.amount), "{}", hit.amount);
            assert_eq!(hit.amount.fract(), 0.5);
            assert_eq!(hit.push, Some([0.0, 0.400_000_005_960_464_5, 0.0]));
            seen.insert(hit.amount as i32);
        }
        assert_eq!(seen.len(), 15, "every roll turns up: {seen:?}");
    });
}

#[tuff_core::watchdog]
#[test]
fn shears_take_a_gifted_poppy_off_a_copper_golem() {
    crate::tests::with_context(|_, cx| {
        let mut golem = mob::<CopperGolem>(EntityType::CopperGolem, [0.5, 64.0, 0.5], cx);
        let (core, species) = (&mut golem.core, &mut golem.species);
        assert_eq!(species.interact(core, &click(Item::Shears), cx), None);
        core.equipment.set(
            EquipmentSlot::Saddle,
            Some(ItemStack::mint(Item::Poppy, 1, MintReason::Test)),
        );
        cx.effects.clear();
        assert_eq!(
            species.interact(core, &click(Item::Shears), cx),
            Some(HandEffect::Damage(1))
        );
        assert!(core.equipment.get(EquipmentSlot::Saddle).is_none());
        assert!(cx.effects.iter().any(|e| matches!(
            e,
            Effect::DropItem { stack, .. } if stack.item() == Item::Poppy
        )));
    });
}

#[tuff_core::watchdog]
#[test]
fn copper_golems_weather_on_schedule_unless_waxed() {
    crate::tests::with_context(|_, cx| {
        let mut golem = mob::<CopperGolem>(EntityType::CopperGolem, [0.5, 64.0, 0.5], cx);
        let (core, species) = (&mut golem.core, &mut golem.species);
        assert!(!species.weather(core, 100, cx));
        let first = species.next_weather;
        assert!((100 + WEATHERING_FROM..=100 + WEATHERING_TO).contains(&first));
        species.weather(core, first - 1, cx);
        assert_eq!(species.oxidation(), 0);
        species.weather(core, first, cx);
        assert_eq!(species.oxidation(), 1);
        assert!(species.next_weather >= first + WEATHERING_FROM);

        assert_eq!(
            species.interact(core, &click(Item::Honeycomb), cx),
            Some(HandEffect::Consume(1))
        );
        assert!(species.waxed());
        assert_eq!(species.interact(core, &click(Item::Honeycomb), cx), None);
        species.weather(core, i64::MAX, cx);
        assert_eq!(species.oxidation(), 1, "wax stops weathering");

        assert_eq!(
            species.interact(core, &click(Item::IronAxe), cx),
            Some(HandEffect::Damage(1))
        );
        assert!(!species.waxed(), "the axe takes the wax first");
        assert_eq!(species.oxidation(), 1);
        assert_eq!(
            species.interact(core, &click(Item::IronAxe), cx),
            Some(HandEffect::Damage(1))
        );
        assert_eq!(species.oxidation(), 0, "then scrapes a stage off");
        assert_eq!(species.interact(core, &click(Item::IronAxe), cx), None);

        species.set_oxidation(core, 2);
        species.heard(core, Notice::Charged);
        assert_eq!(species.oxidation(), 1, "lightning scrapes a stage");
    });
}

#[tuff_core::watchdog]
#[test]
fn copper_golems_wear_a_gifted_poppy_once() {
    crate::tests::with_context(|_, cx| {
        let mut golem = mob::<CopperGolem>(EntityType::CopperGolem, [0.5, 64.0, 0.5], cx);
        golem
            .species
            .heard(&mut golem.core, Notice::Gifted(Item::Poppy));
        let worn = golem
            .core
            .equipment
            .get(EquipmentSlot::Saddle)
            .map(|s| s.item());
        assert_eq!(worn, Some(Item::Poppy));
        assert!(golem.core.equipment.drop_chance(EquipmentSlot::Saddle) > 1.0);
        golem
            .species
            .heard(&mut golem.core, Notice::Gifted(Item::Dandelion));
        let worn = golem
            .core
            .equipment
            .get(EquipmentSlot::Saddle)
            .map(|s| s.item());
        assert_eq!(worn, Some(Item::Poppy), "one flower at a time");
    });
}

#[tuff_core::watchdog]
#[test]
fn snow_golems_leave_a_trail_of_snow() {
    crate::tests::with_context(|_, cx| {
        let mut golem = mob::<SnowGolem>(EntityType::SnowGolem, [0.0, 64.0, 0.0], cx);
        golem.species.ai_step(&mut golem.core, cx);
        let mut placed: Vec<(i32, i32, i32)> = cx
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::SetBlock { pos, state } if state.block() == Block::Snow => {
                    Some((pos.x, pos.y, pos.z))
                }
                _ => None,
            })
            .collect();
        placed.sort();
        assert_eq!(placed, [(-1, 64, -1), (-1, 64, 0), (0, 64, -1), (0, 64, 0)]);
    });
}

#[tuff_core::watchdog]
#[test]
fn golem_data_indices_match_the_dumped_layouts() {
    let byte = MetadataValue::Byte(0).type_id() as u8;
    let iron = EntityType::IronGolem.data_layout();
    let snow = EntityType::SnowGolem.data_layout();
    assert_eq!(
        iron.len(),
        FLAGS as usize + 1,
        "iron golem is not ageable, its flags byte lands right after the shared mob data"
    );
    assert_eq!(iron[FLAGS as usize], byte);
    assert_eq!(
        snow.len(),
        FLAGS as usize + 1,
        "snow golem is not ageable either"
    );
    assert_eq!(snow[FLAGS as usize], byte);
}

#[tuff_core::watchdog]
#[test]
fn copper_golem_data_indices_match_the_dumped_layout() {
    let layout = EntityType::CopperGolem.data_layout();
    assert_eq!(
        layout.len(),
        GOLEM_STATE as usize + 1,
        "copper golem is not ageable, its two fields land right after the shared mob data"
    );
    assert_eq!(
        layout[WEATHER_STATE as usize],
        tuff_vanilla_data::entity::data_serializer::WEATHERING_COPPER_STATE
    );
    assert_eq!(
        layout[GOLEM_STATE as usize],
        tuff_vanilla_data::entity::data_serializer::COPPER_GOLEM_STATE
    );
}

fn lookup_of(blocks: Vec<(BlockPos, BlockState)>) -> impl Fn(BlockPos) -> Option<BlockState> {
    move |pos| blocks.iter().find(|(p, _)| *p == pos).map(|(_, s)| *s)
}

#[tuff_core::watchdog]
#[test]
fn a_pumpkin_on_an_iron_t_assembles_an_iron_golem() {
    let head = BlockPos::new(0, 65, 0);
    let center = BlockPos::new(0, 64, 0);
    let bottom = BlockPos::new(0, 63, 0);
    let arm_a = BlockPos::new(1, 64, 0);
    let arm_b = BlockPos::new(-1, 64, 0);
    let lookup = lookup_of(vec![
        (head, Block::CarvedPumpkin.default_state()),
        (center, Block::IronBlock.default_state()),
        (bottom, Block::IronBlock.default_state()),
        (arm_a, Block::IronBlock.default_state()),
        (arm_b, Block::IronBlock.default_state()),
    ]);
    let assembly = try_assemble(head, lookup).expect("assembles");
    assert_eq!(assembly.kind, EntityType::IronGolem);
    assert_eq!(assembly.spawn, [0.5, 63.0, 0.5]);
    assert_eq!(assembly.consumed.len(), 5);
}

#[tuff_core::watchdog]
#[test]
fn the_other_horizontal_axis_also_assembles_an_iron_golem() {
    let head = BlockPos::new(0, 65, 0);
    let lookup = lookup_of(vec![
        (head, Block::JackOLantern.default_state()),
        (BlockPos::new(0, 64, 0), Block::IronBlock.default_state()),
        (BlockPos::new(0, 63, 0), Block::IronBlock.default_state()),
        (BlockPos::new(0, 64, 1), Block::IronBlock.default_state()),
        (BlockPos::new(0, 64, -1), Block::IronBlock.default_state()),
    ]);
    let assembly = try_assemble(head, lookup).expect("assembles");
    assert_eq!(assembly.kind, EntityType::IronGolem);
}

#[tuff_core::watchdog]
#[test]
fn a_pumpkin_on_two_snow_blocks_assembles_a_snow_golem() {
    let head = BlockPos::new(5, 65, 5);
    let lookup = lookup_of(vec![
        (head, Block::CarvedPumpkin.default_state()),
        (BlockPos::new(5, 64, 5), Block::SnowBlock.default_state()),
        (BlockPos::new(5, 63, 5), Block::SnowBlock.default_state()),
    ]);
    let assembly = try_assemble(head, lookup).expect("assembles");
    assert_eq!(assembly.kind, EntityType::SnowGolem);
    assert_eq!(assembly.spawn, [5.5, 63.0, 5.5]);
    assert_eq!(assembly.consumed.len(), 3);
}

#[tuff_core::watchdog]
#[test]
fn iron_golems_spare_creepers_and_built_ones_spare_players() {
    crate::tests::with_context(|_, cx| {
        let target = |kind| TargetView {
            id: 50,
            uuid: 50,
            kind,
            position: [2.0, 64.0, 0.5],
            bounds: [1.7, 64.0, 0.2, 2.3, 65.8, 0.8],
            eye: [2.0, 65.6, 0.5],
            alive: true,
            creative: false,
            invulnerable: false,
            in_water: false,
            seen_as_enemy: true,
            owner: None,
        };
        let mut golem = mob::<IronGolem>(EntityType::IronGolem, [0.5, 64.0, 0.5], cx);
        assert!(!golem.core.can_attack(&target(EntityType::Creeper), cx));
        assert!(golem.core.can_attack(&target(EntityType::Player), cx));
        let mut nbt = Compound::new().with("PlayerCreated", true);
        golem.species.load(&mut golem.core, &mut nbt);
        assert!(!golem.core.can_attack(&target(EntityType::Player), cx));
        assert!(golem.core.can_attack(&target(EntityType::Zombie), cx));
    });
}

#[tuff_core::watchdog]
#[test]
fn an_incomplete_body_assembles_nothing() {
    let head = BlockPos::new(0, 65, 0);
    let lookup = lookup_of(vec![
        (head, Block::CarvedPumpkin.default_state()),
        (BlockPos::new(0, 64, 0), Block::IronBlock.default_state()),
    ]);
    assert!(try_assemble(head, lookup).is_none());
}

#[tuff_core::watchdog]
#[test]
fn a_plain_pumpkin_is_not_a_head() {
    let head = BlockPos::new(0, 65, 0);
    let lookup = lookup_of(vec![
        (head, Block::Pumpkin.default_state()),
        (BlockPos::new(0, 64, 0), Block::SnowBlock.default_state()),
        (BlockPos::new(0, 63, 0), Block::SnowBlock.default_state()),
    ]);
    assert!(try_assemble(head, lookup).is_none());
}

#[tuff_core::watchdog]
#[test]
fn iron_golems_never_target_creepers() {
    assert!(!IRON_GOLEM_ENEMIES.contains(&EntityType::Creeper));
    assert!(HOSTILE_MOBS.contains(&EntityType::Creeper));
}

#[tuff_core::watchdog]
#[test]
fn a_fresh_snow_golem_has_a_pumpkin_and_default_has_no_iron_golem_flags() {
    let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
    let snow = SnowGolem::create(&mut rng);
    assert!(snow.has_pumpkin);
    let iron = IronGolem::create(&mut rng);
    assert!(!iron.player_created);
}

/// A minimal tick context over a flat world whose biome `with_context`
/// callers can pick, unlike `super::super::tests::with_context` (always
/// `minecraft:plains`), for testing biome-conditioned behaviour.
fn with_biome_context(
    biome: &'static str,
    kind: EntityType,
    f: impl FnOnce(&mut MobCore, &mut TickContext),
) {
    use crate::world::poi::PoiStore;
    let shared = tuff_test_support::bare_shared();
    let block = |pos: BlockPos| {
        Some(if pos.y < 64 {
            tuff_vanilla_data::Block::Stone.default_state()
        } else {
            tuff_vanilla_data::Block::Air.default_state()
        })
    };
    let light = |_: BlockPos| Some((15, 0));
    let biome_fn = |_: BlockPos| Some(biome);
    let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
    let mut effects = Vec::new();
    let mut budget = 10_000;
    let pois = PoiStore::default();
    let mut cx = TickContext {
        tick: 1,
        dimension: tuff_core::world::dimension::DimensionId::for_tests(0),
        block: &block,
        light: &light,
        sky_darken: 0,
        ambient_light: 0.0,
        monsters_burn: true,
        raining: false,
        biome: &biome_fn,
        container: &|_| None,
        container_info: &|_| Default::default(),
        inhabited_time: &|_| 0,
        min_y: -64,
        max_y: 319,
        players: &[],
        players_in_dimension: 0,
        entities: &[],
        entity_grid: None,
        pistons: &[],
        parts: &[],
        shared: &shared,
        rng: &mut rng,
        effects: &mut effects,
        path_budget: &mut budget,
        pois: &pois,
        state: &tuff_core::world::region_api::RegionStateLock::default(),
    };
    let base = tuff_core::world::entity::EntityBase::new(kind, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
    let mut core = MobCore::new(base);
    SnowGolem::configure(&mut core);
    f(&mut core, &mut cx);
}

#[tuff_core::watchdog]
#[test]
fn a_bare_headed_snow_golem_melts_in_a_biome_that_melts_it() {
    with_biome_context("minecraft:desert", EntityType::SnowGolem, |core, cx| {
        let mut golem = SnowGolem { has_pumpkin: false };
        golem.ai_step(core, cx);
        assert!(
            !core.pending_hurts.is_empty(),
            "a bare-headed golem takes melt damage in a hot biome"
        );
    });
}

#[tuff_core::watchdog]
#[test]
fn a_pumpkin_headed_snow_golem_still_melts_here_too() {
    // Matches the game's own logic: the environment attribute check
    // does not look at the pumpkin at all, only at position.
    with_biome_context("minecraft:desert", EntityType::SnowGolem, |core, cx| {
        let mut golem = SnowGolem { has_pumpkin: true };
        golem.ai_step(core, cx);
        assert!(!core.pending_hurts.is_empty());
    });
}

#[tuff_core::watchdog]
#[test]
fn a_snow_golem_does_not_melt_in_a_cold_biome() {
    with_biome_context(
        "minecraft:snowy_plains",
        EntityType::SnowGolem,
        |core, cx| {
            let mut golem = SnowGolem { has_pumpkin: false };
            golem.ai_step(core, cx);
            assert!(core.pending_hurts.is_empty());
        },
    );
}

fn bumper(id: i32, kind: EntityType) -> tuff_core::world::entity::EntitySummary {
    tuff_core::world::entity::EntitySummary {
        id,
        uuid: id as u128,
        kind,
        position: [0.5, 64.0, 0.5],
        velocity: [0.0; 3],
        yaw: 0.0,
        bounds: [0.2, 64.0, 0.2, 0.8, 65.9, 0.8],
        living: Some(tuff_core::world::entity::LivingSummary {
            unpushable: false,
            alive: true,
            target: None,
            age: 0,
            in_love: false,
            panicking: false,
            eye_height: 1.6,
            in_water: false,
            seen_as_enemy: true,
            enemy: true,
            health: 20.0,
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
fn an_iron_golem_sometimes_picks_an_enemy_that_bumps_it_but_never_a_creeper() {
    for (kind, picked) in [(EntityType::Zombie, true), (EntityType::Creeper, false)] {
        crate::tests::with_entities(&[bumper(50, kind)], |_, cx| {
            let mut golem = mob::<IronGolem>(EntityType::IronGolem, [0.5, 64.0, 0.5], cx);
            for _ in 0..400 {
                golem.core.push_entities(cx);
            }
            assert_eq!(golem.core.target.is_some(), picked, "{kind:?}");
        });
    }
}

#[tuff_core::watchdog]
#[test]
fn shearing_a_snow_golem_drops_its_carved_pumpkin() {
    crate::tests::with_context(|_, cx| {
        let mut golem = mob::<SnowGolem>(EntityType::SnowGolem, [0.5, 64.0, 0.5], cx);
        let (core, species) = (&mut golem.core, &mut golem.species);
        cx.effects.clear();
        assert_eq!(
            species.interact(core, &click(Item::Shears), cx),
            Some(HandEffect::Damage(1))
        );
        assert!(cx.effects.iter().any(|e| matches!(
            e,
            Effect::DropItem { stack, position, .. }
                if stack.item() == Item::CarvedPumpkin && position[1] > 64.0
        )));
        cx.effects.clear();
        assert_eq!(species.interact(core, &click(Item::Shears), cx), None);
        assert!(cx.effects.is_empty());
    });
}

fn chest_world(chest: Block, above: Block) -> impl Fn(BlockPos) -> Option<BlockState> {
    move |pos| {
        Some(if pos.y < 64 {
            Block::Stone.default_state()
        } else if pos == BlockPos::new(3, 64, 0) {
            chest.default_state()
        } else if pos == BlockPos::new(3, 65, 0) {
            above.default_state()
        } else {
            Block::Air.default_state()
        })
    }
}

fn run_with_chest(
    above: Block,
    f: impl FnOnce(&mut TransportItems, &mut MobCore, &mut TickContext),
) {
    let world = chest_world(Block::CopperChest, above);
    crate::tests::with_players(&world, &[], &[], |core, cx| {
        CopperGolem::configure(core);
        core.base.position = [2.5, 64.0, 0.5];
        core.size.width = 0.49;
        core.size.height = 0.98;
        f(&mut TransportItems::default(), core, cx);
    });
}

#[tuff_core::watchdog]
#[test]
fn a_copper_golem_skips_locked_chests_and_ones_with_a_block_on_top() {
    run_with_chest(Block::Air, |goal, core, cx| {
        assert_eq!(goal.find_target(core, cx), Some(BlockPos::new(3, 64, 0)));
        cx.container_info = &|_| tuff_core::world::entity::ContainerInfo {
            locked: true,
            openers: 0,
        };
        assert_eq!(goal.find_target(core, cx), None, "locked");
    });
    run_with_chest(Block::Stone, |goal, core, cx| {
        assert_eq!(goal.find_target(core, cx), None, "blocked above");
    });
}

#[tuff_core::watchdog]
#[test]
fn a_copper_golem_queues_behind_a_chest_in_use_and_opens_the_lid_while_working() {
    run_with_chest(Block::Air, |goal, core, cx| {
        cx.container_info = &|_| tuff_core::world::entity::ContainerInfo {
            locked: false,
            openers: 1,
        };
        goal.tick(core, cx);
        assert_eq!(goal.target, Some(BlockPos::new(3, 64, 0)));
        goal.tick(core, cx);
        assert_eq!(goal.stage, Stage::Queuing, "another mob has it open");
        goal.tick(core, cx);
        assert_eq!(goal.stage, Stage::Queuing);

        cx.container_info = &|_| Default::default();
        goal.tick(core, cx);
        assert_eq!(goal.stage, Stage::Travelling);
        goal.tick(core, cx);
        assert_eq!(goal.stage, Stage::Interacting);

        let opens = |cx: &TickContext, wanted: bool| {
            cx.effects
                .iter()
                .filter(|e| matches!(e, Effect::ContainerOpen { open, .. } if *open == wanted))
                .count()
        };
        goal.tick(core, cx);
        assert_eq!(opens(cx, true), 1, "the lid opens on the first tick");
        assert_eq!(core.signal, STATE_GETTING_NO_ITEM);
        for _ in 0..58 {
            goal.tick(core, cx);
        }
        assert_eq!(opens(cx, false), 0);
        goal.tick(core, cx);
        assert_eq!(opens(cx, false), 1, "and shuts when the visit ends");
        assert_eq!(core.signal, STATE_IDLE);
        assert_eq!(goal.target, None);
    });
}

#[tuff_core::watchdog]
#[test]
fn a_panicking_copper_golem_stops_transporting() {
    run_with_chest(Block::Air, |goal, core, cx| {
        goal.cooldown.arm(cx);
        goal.cooldown.set(0);
        assert!(goal.can_use(core, cx));
        core.panicking = true;
        assert!(!goal.can_continue(core, cx));
    });
}

#[tuff_core::watchdog]
#[test]
fn village_strolls_lean_toward_a_villager_who_wants_a_golem() {
    let villager = tuff_core::world::entity::EntitySummary {
        id: 7,
        uuid: 7,
        kind: EntityType::Villager,
        position: [24.5, 64.0, 0.5],
        velocity: [0.0; 3],
        yaw: 0.0,
        bounds: [24.2, 64.0, 0.2, 24.8, 65.9, 0.8],
        living: Some(tuff_core::world::entity::LivingSummary {
            unpushable: false,
            alive: true,
            target: None,
            age: 0,
            in_love: false,
            panicking: false,
            eye_height: 1.62,
            in_water: false,
            seen_as_enemy: false,
            enemy: false,
            health: 20.0,
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
    };
    let floor = |pos: BlockPos| {
        Some(if pos.y < 64 {
            Block::Stone.default_state()
        } else {
            Block::Air.default_state()
        })
    };
    crate::tests::with_world(&floor, &[villager], |core, cx| {
        core.base.position = [0.5, 64.0, 0.5];
        let tick = cx.tick;
        cx.state
            .get::<crate::villager::brain::Facts>()
            .publish_rested(7, tick);
        let mut toward = 0;
        for _ in 0..200 {
            if let Some(target) = StrollInVillage::position(core, cx)
                && target[0] > 0.5
            {
                toward += 1;
            }
        }
        assert!(toward > 150, "{toward} of 200 strolls went toward it");
    });
}

#[tuff_core::watchdog]
#[test]
fn a_snow_golem_waits_a_full_interval_before_its_first_throw() {
    let flat = |pos: tuff_world_model::BlockPos| {
        Some(if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else {
            Block::Air.default_state()
        })
    };
    let target = [tuff_core::world::entity::NearbyPlayer {
        id: 1,
        uuid: 1,
        name: "",
        position: [4.5, 64.0, 0.5],
        eye_height: 1.62,
        yaw: 0.0,
        pitch: 0.0,
        held: [None, None],
        creative: false,
        height: 1.8,
        sneaking: false,
        invisible: false,
        invulnerable: false,
        armor: [None; 4],
        effects: 0,
        movement: [0.0; 3],
        vehicle: None,
        sleeping: None,
        last_hurt_by: None,
        last_hurt_mob: None,
        perchable: false,
        sprinting: false,
    }];
    crate::tests::with_players(&flat, &[], &target, |core, cx| {
        core.base.position = [0.5, 64.0, 0.5];
        core.target = Some(1);
        let mut goal = RangedSnowballAttack::new();
        goal.tick(core, cx);
        assert_eq!(goal.until_attack, SNOW_GOLEM_ATTACK_INTERVAL);
        assert_eq!(goal.see_time, 1);
        for _ in 0..(SNOW_GOLEM_ATTACK_INTERVAL - 1) {
            goal.tick(core, cx);
        }
        assert_eq!(goal.until_attack, 1);
        goal.tick(core, cx);
        assert_eq!(
            goal.until_attack, SNOW_GOLEM_ATTACK_INTERVAL,
            "threw and reset"
        );
    });
}

#[tuff_core::watchdog]
#[test]
fn a_copper_golem_takes_the_whole_stack_and_holds_only_what_the_chest_gives() {
    let nbt = tuff_nbt::Compound::new()
        .with("id", "minecraft:netherite_pickaxe")
        .with("count", tuff_nbt::Tag::Int(1))
        .with(
            "components",
            tuff_nbt::Compound::new().with("minecraft:damage", tuff_nbt::Tag::Int(500)),
        );
    let pick = ItemStack::mint_from_nbt(&nbt, MintReason::Test).expect("a damaged pickaxe");
    assert!(!pick.stacks_with(&ItemStack::mint(
        Item::NetheritePickaxe,
        1,
        MintReason::Test
    )));
    let items: &'static [Option<ItemStack>] =
        Box::leak(vec![Some(pick.test_copy())].into_boxed_slice());
    run_with_chest(Block::Air, |goal, core, cx| {
        cx.container = Box::leak(Box::new(move |_| {
            Some(tuff_item::item::SlotsCow::Borrowed(items))
        }));
        goal.finish(core, cx, BlockPos::new(3, 64, 0));
        assert_eq!(
            core.equipment.get(EquipmentSlot::Mainhand),
            None,
            "the hand waits for what the chest gives up"
        );
        let taken: Vec<_> = cx
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::CopperGolemTake { stack, .. } => Some(stack.test_copy()),
                _ => None,
            })
            .collect();
        assert_eq!(taken, vec![pick.test_copy()], "components ride along");
        let mut golem = CopperGolem::default();
        golem.took_item(core, pick.test_copy());
        assert_eq!(core.equipment.get(EquipmentSlot::Mainhand), Some(&pick));
    });
}

#[tuff_core::watchdog]
#[test]
fn a_fresh_copper_golem_waits_60_to_99_ticks_before_its_first_trip() {
    run_with_chest(Block::Air, |_, core, cx| {
        let cooldown = Cooldown::default();
        let mut goal = TransportItems {
            cooldown: cooldown.clone(),
            ..TransportItems::default()
        };
        let start = cx.tick;
        assert!(!goal.can_use(core, cx), "still on its starting cooldown");
        let wait = cooldown.until() - start;
        assert!((60..100).contains(&wait), "{wait}");
        cx.tick = cooldown.until();
        assert!(goal.can_use(core, cx));
    });
}

#[tuff_core::watchdog]
#[test]
fn a_copper_golem_only_idles_while_the_transport_cooldown_runs() {
    run_with_chest(Block::Air, |_, core, cx| {
        let cooldown = Cooldown::default();
        let mut idle = IdleWander {
            cooldown: cooldown.clone(),
            ..IdleWander::default()
        };
        let mut started = false;
        for _ in 0..40 {
            started |= idle.can_use(core, cx);
        }
        assert!(started, "idles during the starting cooldown");
        cx.tick = cooldown.until();
        for _ in 0..40 {
            assert!(!idle.can_use(core, cx), "no idling once trips may start");
        }
        cooldown.set(cx.tick + 140);
        let mut waits = Vec::new();
        for _ in 0..200 {
            if idle.can_use(core, cx) && idle.target.is_none() {
                waits.push(idle.wait);
            }
        }
        assert!(waits.iter().all(|w| (30..=60).contains(w)), "{waits:?}");
    });
}
