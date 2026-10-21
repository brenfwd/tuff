//! Region gameplay that reports advancement triggers to the player it
//! concerns: taming, summoning, entity interactions and melee hits.

use super::*;
use crate::world::entity::EntityAttack;
use crate::world::messages::{AdvancementEvent, PlayerLooks};
use crate::world::nearby::{IndexedPlayer, PlayerIndex};
use crate::world::predicate::EntityFacts;
use tuff_core::world::advancement::SessionAdvancements;
use tuff_item::item::ItemSnapshot;
use tuff_vanilla_data::registry::EntityType;

async fn loaded(
    world: &TestWorld,
) -> (
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let floor: Vec<_> = (0..16)
        .flat_map(|x| {
            (0..16).map(move |z| (BlockPos::new(x, SKY - 2, z), Block::Stone.default_state()))
        })
        .collect();
    place_blocks(world, &region, floor).await;
    (region, player, events)
}

async fn reported(
    world: &TestWorld,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
) -> AdvancementEvent {
    ticking_expect(world, events, 20, |event| match event {
        PlayerEvent::Advancement(event) => Some(*event),
        _ => None,
    })
    .await
}

/// Right-clicks `target` holding `item`, returning what was reported.
async fn interact_reporting(
    world: &TestWorld,
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    target: i32,
    item: Item,
) -> Vec<AdvancementEvent> {
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target,
            interaction: interaction(player, Some(item)),
        })
        .await;
    world.ticks(1).await;
    let mut found = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::Advancement(event) = event {
            found.push(*event);
        }
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn building_an_iron_golem_reports_it_summoned_to_players_nearby() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = loaded(&world).await;
    let overworld = world.dimension(crate::storage::level::OVERWORLD);
    world.players.send_replace(Arc::new(PlayerIndex::build([(
        overworld,
        IndexedPlayer {
            handle: player.clone(),
            uuid: 77,
            name: Arc::from("builder"),
            position: [10.5, SKY as f64, 8.5],
            looks: PlayerLooks::default(),
            time_since_rest: 0,
        },
    )])));
    let center = BlockPos::new(8, SKY, 8);
    let iron = Block::IronBlock.default_state();
    place_blocks(
        &world,
        &region,
        vec![
            (BlockPos::new(8, SKY - 1, 8), iron),
            (center, iron),
            (BlockPos::new(9, SKY, 8), iron),
            (BlockPos::new(7, SKY, 8), iron),
        ],
    )
    .await;
    let head = place(&player, center, Item::CarvedPumpkin, 1);
    region.request(RegionRequest::Place(head)).await;
    let event = ticking_expect(&world, &mut events, 20, |event| match event {
        PlayerEvent::Advancement(event)
            if matches!(*event, AdvancementEvent::SummonedEntity(_)) =>
        {
            Some(*event)
        }
        _ => None,
    })
    .await;
    assert!(
        matches!(&event, AdvancementEvent::SummonedEntity(facts) if facts.kind == Some(EntityType::IronGolem)),
        "{event:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn taming_a_wolf_reports_it_to_its_new_owner() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = loaded(&world).await;
    let position = [8.5, SKY as f64 - 1.0, 8.5];
    region
        .request(player_update(&player, position, Some(Item::Bone)))
        .await;
    summon(
        &region,
        EntityType::Wolf,
        Compound::new().with("NoAI", 1i8),
        position,
    )
    .await;
    let wolf = mobs(&region, EntityType::Wolf).await[0].id;
    let mut tamed = None;
    for _ in 0..64 {
        let found =
            interact_reporting(&world, &region, &player, &mut events, wolf, Item::Bone).await;
        tamed = found
            .into_iter()
            .find(|event| matches!(event, AdvancementEvent::TameAnimal(_)));
        if tamed.is_some() {
            break;
        }
    }
    let tamed = tamed.expect("a bone eventually tames the wolf");
    assert!(
        matches!(&tamed, AdvancementEvent::TameAnimal(facts) if facts.kind == Some(EntityType::Wolf)),
        "{tamed:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_used_interaction_reports_the_item_and_entity() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = loaded(&world).await;
    let position = [8.5, SKY as f64 - 1.0, 8.5];
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", 1i8),
        position,
    )
    .await;
    let cow = mobs(&region, EntityType::Cow).await[0].id;
    let found = interact_reporting(&world, &region, &player, &mut events, cow, Item::Wheat).await;
    match found.into_iter().next().expect("the feeding is reported") {
        AdvancementEvent::InteractedWithEntity { item, entity } => {
            assert_eq!(item.map(|stack| stack.item), Some(Item::Wheat));
            assert_eq!(entity.kind, Some(EntityType::Cow));
        }
        other => panic!("{other:?}"),
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_landed_melee_hit_tells_the_attacker_what_it_hit() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = loaded(&world).await;
    let position = [8.5, SKY as f64 - 1.0, 8.5];
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", 1i8),
        position,
    )
    .await;
    let cow = mobs(&region, EntityType::Cow).await[0].id;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: cow,
            attack: EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: position,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 3.0,
                knockback: 0.0,
                weapon: None,
                critical: false,
                magic: false,
                sweep: false,
                part: None,
            },
            sweep: None,
        })
        .await;
    let (kind, dealt, lost) = expect(&mut events, |event| match event {
        PlayerEvent::AttackLanded {
            target_kind,
            dealt,
            health_lost,
            ..
        } => Some((target_kind, dealt, health_lost)),
        _ => None,
    })
    .await;
    assert_eq!((kind, dealt, lost), (EntityType::Cow, 3.0, 3.0));
    world.shutdown().await;
}

fn facts(kind: EntityType) -> EntityFacts {
    EntityFacts {
        kind: Some(kind),
        ..EntityFacts::default()
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sweep_hit_reports_its_victim_to_the_attacker() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = loaded(&world).await;
    let position = [8.5, SKY as f64 - 1.0, 8.5];
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", 1i8),
        position,
    )
    .await;
    let cow = mobs(&region, EntityType::Cow).await[0].id;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: cow,
            attack: EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: position,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 3.0,
                knockback: 0.0,
                weapon: None,
                critical: false,
                magic: false,
                sweep: true,
                part: None,
            },
            sweep: None,
        })
        .await;
    let (kind, taken) = expect(&mut events, |event| match event {
        PlayerEvent::Advancement(event) => match *event {
            AdvancementEvent::HurtEntity { victim, taken, .. } => Some((victim.kind, taken)),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!((kind, taken), (Some(EntityType::Cow), 3.0));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reported_triggers_meet_their_advancements() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 6);
    player.stand_at([0.5, 100.0, 0.5], true);
    let report = |event: AdvancementEvent| PlayerEvent::Advancement(Box::new(event));

    player
        .handle_event(report(AdvancementEvent::TameAnimal(facts(
            EntityType::Wolf,
        ))))
        .await;
    assert!(player.advancement_done("minecraft:husbandry/tame_an_animal"));

    player
        .handle_event(report(AdvancementEvent::SummonedEntity(facts(
            EntityType::IronGolem,
        ))))
        .await;
    assert!(player.advancement_done("minecraft:adventure/summon_iron_golem"));

    player
        .handle_event(report(AdvancementEvent::InteractedWithEntity {
            item: Some(ItemSnapshot::of(&ItemStack::mint(
                Item::Stick,
                1,
                MintReason::Test,
            ))),
            entity: facts(EntityType::Armadillo),
        }))
        .await;
    assert!(!player.advancement_done("minecraft:adventure/brush_armadillo"));
    player
        .handle_event(report(AdvancementEvent::InteractedWithEntity {
            item: Some(ItemSnapshot::of(&ItemStack::mint(
                Item::Brush,
                1,
                MintReason::Test,
            ))),
            entity: facts(EntityType::Armadillo),
        }))
        .await;
    assert!(player.advancement_done("minecraft:adventure/brush_armadillo"));

    player.hurt_entity(None, EntityType::Zombie, [2.0, 100.0, 0.5], 4.0, 4.0);
    assert!(
        !player.advancement_done("minecraft:adventure/shoot_arrow"),
        "a melee hit is no arrow"
    );
    player
        .handle_event(report(AdvancementEvent::HurtEntity {
            victim: facts(EntityType::Zombie),
            damage: crate::world::predicate::DamageFacts {
                kind: &tuff_vanilla_data::damage_type::ARROW,
                source: None,
                direct: Some(facts(EntityType::Arrow)),
            },
            weapon: None,
            dealt: 6.0,
            taken: 6.0,
        }))
        .await;
    assert!(player.advancement_done("minecraft:adventure/shoot_arrow"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reported_block_and_pickup_triggers_meet_their_advancements() {
    use crate::world::messages::Kill;
    use crate::world::predicate::DamageFacts;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 6);
    player.stand_at([0.5, 100.0, 0.5], true);
    let report = |event: AdvancementEvent| PlayerEvent::Advancement(Box::new(event));

    player
        .handle_event(report(AdvancementEvent::ConstructBeacon(1)))
        .await;
    assert!(player.advancement_done("minecraft:nether/create_beacon"));
    assert!(!player.advancement_done("minecraft:nether/create_full_beacon"));
    player
        .handle_event(report(AdvancementEvent::ConstructBeacon(4)))
        .await;
    assert!(player.advancement_done("minecraft:nether/create_full_beacon"));

    let arrow_at = |x: f64| EntityFacts {
        kind: Some(EntityType::Arrow),
        position: [x, 100.0, 0.5],
        ..EntityFacts::default()
    };
    player
        .handle_event(report(AdvancementEvent::TargetHit {
            projectile: arrow_at(10.5),
            signal: 15,
        }))
        .await;
    assert!(!player.advancement_done("minecraft:adventure/bullseye"));
    player
        .handle_event(report(AdvancementEvent::TargetHit {
            projectile: arrow_at(40.5),
            signal: 15,
        }))
        .await;
    assert!(player.advancement_done("minecraft:adventure/bullseye"));

    player
        .handle_event(report(AdvancementEvent::KillMobNearSculkCatalyst(Kill {
            kind: EntityType::Zombie,
            holder: "zombie".into(),
            victim: facts(EntityType::Zombie),
            killing_blow: DamageFacts {
                kind: &tuff_vanilla_data::damage_type::PLAYER_ATTACK,
                source: None,
                direct: None,
            },
        })))
        .await;
    assert!(player.advancement_done("minecraft:adventure/kill_mob_near_sculk_catalyst"));

    player
        .handle_event(report(AdvancementEvent::AvoidVibration))
        .await;
    assert!(player.advancement_done("minecraft:adventure/avoid_vibration"));

    player
        .handle_event(report(AdvancementEvent::CrafterRecipeCrafted {
            recipe: tuff_vanilla_data::Frozen::Static("minecraft:crafter"),
            ingredients: Vec::new(),
        }))
        .await;
    assert!(player.advancement_done("minecraft:adventure/crafters_crafting_crafters"));

    let mut silk = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
    silk.components.enchantments = Some(tuff_item::item::Enchantments::new([(
        "minecraft:silk_touch".to_owned(),
        1,
    )]));
    player
        .handle_event(report(AdvancementEvent::BeeNestDestroyed {
            block: Block::BeeNest.default_state(),
            item: Some(ItemSnapshot::of(&silk)),
            bees: 3,
        }))
        .await;
    assert!(player.advancement_done("minecraft:husbandry/silk_touch_nest"));

    player
        .handle_event(report(AdvancementEvent::ThrownItemPickedUpByEntity {
            item: ItemSnapshot::of(&ItemStack::mint(Item::GoldIngot, 1, MintReason::Test)),
            entity: facts(EntityType::Piglin),
        }))
        .await;
    assert!(player.criterion_done("minecraft:nether/distract_piglin", "distract_piglin"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killing_a_breeze_with_a_breeze_charge_is_blowback_and_creative_ends_a_wind_launch() {
    use crate::world::messages::Kill;
    use crate::world::player::Fall;
    use crate::world::predicate::DamageFacts;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 6);
    let of = |kind| EntityFacts {
        kind: Some(kind),
        ..EntityFacts::default()
    };
    let kill = |victim, direct| Kill {
        kind: victim,
        holder: String::new(),
        victim: of(victim),
        killing_blow: DamageFacts {
            kind: &tuff_vanilla_data::damage_type::WIND_CHARGE,
            source: Some(of(EntityType::Player)),
            direct: Some(of(direct)),
        },
    };
    let blowback = "minecraft:adventure/blowback";
    player.killed_entity(kill(EntityType::Breeze, EntityType::WindCharge));
    player.killed_entity(kill(EntityType::Zombie, EntityType::BreezeWindCharge));
    assert!(!player.advancement_done(blowback));
    player.killed_entity(kill(EntityType::Breeze, EntityType::BreezeWindCharge));
    assert!(player.advancement_done(blowback));

    player.body.fall = Fall::default();
    player.body.fall.explosion_hit(true, 100.0);
    assert!(player.body.fall.has_impulse());
    player.set_game_mode(crate::play::GameMode::Creative).await;
    assert!(!player.body.fall.has_impulse());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wind_charge_launch_and_a_honey_slide_meet_their_advancements() {
    use crate::world::messages::Blast;
    use crate::world::player::Fall;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 6);
    let blast = |source| PlayerEvent::Exploded {
        damage: 0.0,
        blast: Blast {
            center: [0.5, 100.0, 0.5],
            radius: 1.2,
            damage_type: &tuff_vanilla_data::damage_type::WIND_CHARGE,
            igniter: None,
            culprit: None,
            wind: Some(crate::world::explosion::WindBurst {
                knockback: 1.22,
                sound: tuff_vanilla_data::registry::SoundEvent::EntityWindChargeWindBurst,
                source,
                spares_fall: false,
            }),
        },
        push: [0.0, 1.0, 0.0],
        push_sent: true,
    };
    let rockets = "minecraft:adventure/who_needs_rockets";
    let launch =
        async |player: &mut crate::world::session::PlayerSession, source: EntityType, apex: f64| {
            player.stand_at([0.5, 100.0, 0.5], true);
            player.body.fall = Fall::default();
            player.tick_advancements().await;
            player.handle_event(blast(source)).await;
            player.stand_at([0.5, apex, 0.5], false);
            player.tick_advancements().await;
            player.body.fall = Fall::new(0.1);
            player.tick_advancements().await;
        };
    launch(&mut player, EntityType::BreezeWindCharge, 110.0).await;
    assert!(!player.advancement_done(rockets), "a breeze's charge");
    launch(&mut player, EntityType::WindCharge, 104.0).await;
    assert!(!player.advancement_done(rockets), "too low");
    launch(&mut player, EntityType::WindCharge, 108.0).await;
    assert!(player.advancement_done(rockets));

    let slide = "minecraft:adventure/honey_block_slide";
    player.stand_at([1.29, 64.0, 0.5], false);
    player.surroundings.contact.honey_side = Some(Block::HoneyBlock.default_state());
    player.sync.known_movement = [0.0, -0.05, 0.0];
    player.tick = 40;
    player.tick_advancements().await;
    assert!(!player.advancement_done(slide), "too slow to slide");
    player.sync.known_movement = [0.0, -0.2, 0.0];
    player.tick = 41;
    player.tick_advancements().await;
    assert!(!player.advancement_done(slide), "only once a second");
    player.tick = 60;
    player.tick_advancements().await;
    assert!(player.advancement_done(slide));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn riding_a_strider_fifty_blocks_through_lava_meets_its_advancement() {
    use crate::world::messages::Riding;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let base = ChunkPos::new(40, 40);
    let mut w = super::anticheat::walker(&world, 6, base).await;
    let (x0, z0) = (base.x * 16 - 14, base.z * 16 - 14);
    let start = BlockPos::new(x0, 100, z0);
    let end = BlockPos::new(x0 + 36, 100, z0 + 36);
    let lava = Block::Lava.default_state();
    let region = world.region(RegionPos::new(1, 1)).await;
    place_blocks(&world, &region, [(start, lava), (end, lava)]).await;
    while [start, end]
        .iter()
        .any(|pos| w.session.client_world().block_at(*pos) != Some(lava))
    {
        let event = w.events.recv().await.expect("region events");
        w.session.on_event(event).await;
    }
    let ride = |w: &mut super::anticheat::Walker, at: BlockPos| {
        let position = [f64::from(at.x) + 0.5, 100.0, f64::from(at.z) + 0.5];
        w.session.ride.riding = Some(Box::new(Riding {
            vehicle: 9000,
            kind: EntityType::Strider,
            dimension: w.session.dimension_id(),
            position,
            riders: vec![6],
            seat: [0.0, 1.6, 0.0],
            driver: true,
            steers: true,
            no_gravity: false,
            flying: false,
            ridden: None,
            given: Default::default(),
        }));
        w.session.pose.position = [position[0], 101.6, position[2]];
    };
    let name = "minecraft:nether/ride_strider_in_overworld_lava";
    ride(&mut w, start);
    w.session.tick_advancements().await;
    ride(&mut w, end);
    w.session.tick_advancements().await;
    assert!(w.session.advancement_done(name));
    world.shutdown().await;
}

/// The block-use report the region sends after a right click on a block.
async fn used_block(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
) -> (Option<tuff_vanilla_data::BlockState>, bool) {
    expect(events, |event| match event {
        PlayerEvent::Advancement(event) => match *event {
            AdvancementEvent::UsedBlock {
                location, default, ..
            } => Some((location.block, default)),
            _ => None,
        },
        _ => None,
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn right_clicks_that_do_something_report_the_block_used() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = loaded(&world).await;
    let table = BlockPos::new(8, SKY - 1, 8);
    place_blocks(
        &world,
        &region,
        [(table, Block::CraftingTable.default_state())],
    )
    .await;
    region
        .request(RegionRequest::Place(place(&player, table, Item::Stick, 1)))
        .await;
    let (block, default) = used_block(&mut events).await;
    assert_eq!(block.map(|s| s.block()), Some(Block::CraftingTable));
    assert!(default, "the table opened on its own");

    let floor = BlockPos::new(4, SKY - 2, 4);
    region
        .request(RegionRequest::Place(place(&player, floor, Item::Stone, 2)))
        .await;
    let (block, default) = used_block(&mut events).await;
    assert_eq!(block.map(|s| s.block()), Some(Block::Stone));
    assert!(!default, "placing is the item's doing");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_beacon_coming_on_reports_its_levels_to_players_nearby() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = loaded(&world).await;
    let base = (7..=9).flat_map(|x| {
        (7..=9).map(move |z| {
            (
                BlockPos::new(x, SKY - 1, z),
                Block::IronBlock.default_state(),
            )
        })
    });
    place_blocks(&world, &region, base).await;
    world.publish_players("minecraft:overworld", &[(&player, [12.5, SKY as f64, 8.5])]);
    let mut beacon = place(&player, BlockPos::new(8, SKY - 1, 8), Item::Beacon, 1);
    beacon.components = tuff_item::item::Components::from(tuff_item::item::ComponentPatch {
        other: Compound::new().with(
            "minecraft:block_entity_data",
            Compound::new().with("id", "minecraft:beacon"),
        ),
        ..Default::default()
    });
    region.request(RegionRequest::Place(beacon)).await;
    let level = ticking_expect(&world, &mut events, 200, |event| match event {
        PlayerEvent::Advancement(event) => match *event {
            AdvancementEvent::ConstructBeacon(level) => Some(level),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(level, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_a_bee_nest_reports_the_bees_inside() {
    use crate::world::messages::{BlockOp, BlockOpResult, BreakMode};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = loaded(&world).await;
    let nest = BlockPos::new(8, SKY - 1, 8);
    let bee = || Compound::new().with("entity_data", Compound::new().with("id", "minecraft:bee"));
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes: vec![(nest, Block::BeeNest.default_state())],
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: Some(Compound::new().with("bees", vec![bee(), bee(), bee()])),
            },
            reply,
        })
        .await;
    assert!(matches!(answer.await.unwrap(), BlockOpResult::Set(1)));
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: nest,
            tool: Some(ItemStack::mint(Item::Shears, 1, MintReason::Test)),
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    let (block, item, bees) = expect(&mut events, |event| match event {
        PlayerEvent::Advancement(event) => match *event {
            AdvancementEvent::BeeNestDestroyed { block, item, bees } => Some((block, item, bees)),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(block.block(), Block::BeeNest);
    assert_eq!(item.map(|stack| stack.item), Some(Item::Shears));
    assert_eq!(bees, 3);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn picking_up_a_thrown_item_reports_to_the_picker_and_the_thrower() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, picker, mut events) = loaded(&world).await;
    let (thrower, mut thrower_events) = PlayerHandle::new(2000);
    let at = [8.5, SKY as f64 - 1.0, 8.5];
    region.request(update(&picker, 1, at)).await;
    world.publish_players(
        "minecraft:overworld",
        &[(&picker, at), (&thrower, [12.5, SKY as f64 - 1.0, 8.5])],
    );
    let mut item = crate::world::items::ItemEntity::new(
        world.shared.next_entity_id(),
        77,
        ItemStack::mint(Item::GoldIngot, 1, MintReason::Test),
        at,
        [0.0; 3],
        0,
    );
    item.base
        .extra
        .insert(crate::world::items::THROWER, Tag::uuid(2000));
    region
        .request(RegionRequest::SpawnEntity(Box::new(item)))
        .await;
    let (id, offer) = ticking_expect(&world, &mut events, 20, |event| match event {
        PlayerEvent::OfferItem { item, offer, .. } => Some((item, offer)),
        _ => None,
    })
    .await;
    region.internal(RegionInternal::ItemTaken {
        item: id,
        offer,
        player: picker.clone(),
        taken: 1,
    });
    let thrown_by = reported(&world, &mut events).await;
    assert!(
        matches!(&thrown_by, AdvancementEvent::ThrownItemPickedUpByPlayer { item, thrower }
            if item.item == Item::GoldIngot && thrower.kind == Some(EntityType::Player)),
        "{thrown_by:?}"
    );
    let picked_by = reported(&world, &mut thrower_events).await;
    assert!(
        matches!(&picked_by, AdvancementEvent::ThrownItemPickedUpByEntity { entity, .. }
            if entity.kind == Some(EntityType::Player)),
        "{picked_by:?}"
    );
    world.shutdown().await;
}

async fn open_loot_chest(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    x: i32,
    table: &str,
) -> AdvancementEvent {
    let ground = BlockPos::new(x, SKY - 2, 4);
    let mut placing = place(player, ground, Item::Chest, x * 2);
    placing.components.other = Compound::new().with(
        "minecraft:container_loot",
        Compound::new().with("loot_table", table).with("seed", 7i64),
    );
    region.request(RegionRequest::Place(placing)).await;
    region
        .request(RegionRequest::Place(place(
            player,
            BlockPos::new(x, SKY - 1, 4),
            Item::Stick,
            x * 2 + 1,
        )))
        .await;
    expect(events, |event| match event {
        PlayerEvent::Advancement(event) if matches!(*event, AdvancementEvent::GeneratedLoot(_)) => {
            Some(*event)
        }
        _ => None,
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_bastion_chests_meet_war_pigs() {
    use tuff_vanilla_data::loot_table::{ALL, LootTableKind};
    const WAR_PIGS: &str = "minecraft:nether/loot_bastion";
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, handle, mut events) = loaded(&world).await;
    let (mut player, _outbound) = session_with_outbound(&world, 7);
    player.stand_at([0.5, 100.0, 0.5], true);

    let dungeon = open_loot_chest(
        &region,
        &handle,
        &mut events,
        1,
        "minecraft:chests/simple_dungeon",
    )
    .await;
    assert!(
        matches!(&dungeon, AdvancementEvent::GeneratedLoot(table)
            if table == "minecraft:chests/simple_dungeon"),
        "{dungeon:?}"
    );
    player
        .handle_event(PlayerEvent::Advancement(Box::new(dungeon)))
        .await;
    assert!(!player.advancement_done(WAR_PIGS));

    let mut opened = 0;
    for (name, table) in ALL {
        if name.contains("bastion")
            || !matches!(
                table.kind,
                Some(LootTableKind::Chest | LootTableKind::Archaeology)
            )
        {
            continue;
        }
        opened += 1;
        player
            .handle_event(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::GeneratedLoot((*name).to_owned()),
            )))
            .await;
        assert!(!player.advancement_done(WAR_PIGS), "{name} met War Pigs");
    }
    assert!(opened > 30, "only {opened} container tables");

    let bastion = open_loot_chest(
        &region,
        &handle,
        &mut events,
        3,
        "minecraft:chests/bastion_treasure",
    )
    .await;
    player
        .handle_event(PlayerEvent::Advancement(Box::new(bastion)))
        .await;
    assert!(player.advancement_done(WAR_PIGS));
    assert!(player.criterion_done(WAR_PIGS, "loot_bastion_treasure"));
    assert!(!player.criterion_done(WAR_PIGS, "loot_bastion_bridge"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hitting_an_interaction_entity_reports_one_generic_damage() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = loaded(&world).await;
    summon(
        &region,
        EntityType::Interaction,
        Compound::new(),
        [8.5, SKY as f64 - 1.0, 8.5],
    )
    .await;
    let target = mobs(&region, EntityType::Interaction).await[0].id;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target,
            attack: EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: [8.5, SKY as f64 - 1.0, 8.5],
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 7.0,
                knockback: 0.0,
                weapon: None,
                critical: false,
                magic: false,
                sweep: false,
                part: None,
            },
            sweep: None,
        })
        .await;
    let (kind, dealt, taken) = expect(&mut events, |event| match event {
        PlayerEvent::Advancement(event) => match *event {
            AdvancementEvent::HurtEntity {
                victim,
                dealt,
                taken,
                ..
            } => Some((victim.kind, dealt, taken)),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(
        (kind, dealt, taken),
        (Some(EntityType::Interaction), 1.0, 1.0)
    );
    world.shutdown().await;
}
