//! Water mobs' own behaviour: pufferfish stings, axolotls hunting, playing
//! dead and breeding.

use super::guardian::water_pool;
use super::*;
use crate::world::messages::{HandEffect, PlayerLooks};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::{EntityType, MobEffect};

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pufferfish_puffs_up_and_stings_a_player_next_to_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(6, 6, 1)).await;
    summon(
        &region,
        EntityType::Pufferfish,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [7.6, SKY as f64 + 1.0, 6.5],
            PlayerLooks::default(),
        ))
        .await;
    let hit = next_hit(&world, &mut events, 100).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Pufferfish));
    assert!(hit.amount >= 2.0, "at least half puffed: {}", hit.amount);
    assert!(
        hit.effects.iter().any(|e| e.effect == MobEffect::Poison),
        "the sting poisons"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn axolotls_hunt_fish_in_the_water() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(2, 10, 3)).await;
    summon(
        &region,
        EntityType::Axolotl,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    summon(
        &region,
        EntityType::TropicalFish,
        Compound::new().with("NoAI", true),
        [7.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let fish = mobs(&region, EntityType::TropicalFish).await[0].id;
    let full = mob_health(&region, fish).await;
    let mut bitten = false;
    for _ in 0..400 {
        world.ticks(1).await;
        if mobs(&region, EntityType::TropicalFish)
            .await
            .iter()
            .all(|f| f.id != fish)
            || mob_health(&region, fish).await < full
        {
            bitten = true;
            break;
        }
    }
    assert!(bitten, "the axolotl swims over and bites the fish");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn axolotls_take_tropical_fish_buckets_and_give_the_water_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(2, 10, 3)).await;
    for x in [4.5, 5.5] {
        summon(
            &region,
            EntityType::Axolotl,
            Compound::new(),
            [x, SKY as f64 + 1.0, 6.5],
        )
        .await;
    }
    for axolotl in mobs(&region, EntityType::Axolotl).await {
        let effect = interact(
            &region,
            &viewer,
            &mut events,
            axolotl.id,
            Item::TropicalFishBucket,
        )
        .await;
        let HandEffect::ExchangeStack(stack) = effect else {
            panic!("expected the empty water bucket back, got {effect:?}");
        };
        assert_eq!(stack.item(), Item::WaterBucket);
    }
    wait_for(&world, &region, 600, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Axolotl)
            .count()
            == 3
    })
    .await;
    world.shutdown().await;
}

/// An open pool of water `depth` deep over `min..=max`, with a stone floor
/// and walls but nothing over it.
fn open_pool(min: i32, max: i32, depth: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in min - 1..=max + 1 {
        for z in min - 1..=max + 1 {
            let inside = (min..=max).contains(&x) && (min..=max).contains(&z);
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
            for y in SKY + 1..=SKY + depth {
                let state = if inside {
                    Block::Water.default_state()
                } else {
                    Block::Stone.default_state()
                };
                blocks.push((BlockPos::new(x, y, z), state));
            }
        }
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dolphin_short_of_breath_surfaces_for_air() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, open_pool(2, 12, 6)).await;
    summon(
        &region,
        EntityType::Dolphin,
        Compound::new().with("Air", 100i16),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let dolphin = mobs(&region, EntityType::Dolphin).await[0].id;
    let mut breathed = false;
    for _ in 0..300 {
        world.ticks(1).await;
        let nbt = entity_nbt(&region, dolphin).await.expect("the dolphin");
        if nbt.get_short("Air").is_some_and(|air| air > 1000) {
            breathed = true;
            break;
        }
    }
    assert!(breathed, "it reached the surface and filled its lungs");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dolphins_give_swimming_players_dolphins_grace() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, open_pool(2, 12, 6)).await;
    summon(
        &region,
        EntityType::Dolphin,
        Compound::new(),
        [5.5, SKY as f64 + 3.0, 7.5],
    )
    .await;
    let looks = PlayerLooks {
        height: 0.6,
        eye_height: 0.4,
        ..PlayerLooks::default()
    };
    region
        .request(player_at(&player, [9.5, SKY as f64 + 3.0, 7.5], looks))
        .await;
    let hit = ticking_expect(&world, &mut events, 200, |event| match event {
        PlayerEvent::Potion(hit) => Some(hit),
        _ => None,
    })
    .await;
    assert!(
        hit.effects
            .iter()
            .any(|e| e.effect == MobEffect::DolphinsGrace && e.duration == 100)
    );
    world.shutdown().await;
}

/// A sand beach at `SKY` over `min..=max`, walled in.
fn beach(min: i32, max: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in min - 1..=max + 1 {
        for z in min - 1..=max + 1 {
            blocks.push((BlockPos::new(x, SKY - 1, z), Block::Stone.default_state()));
            blocks.push((BlockPos::new(x, SKY, z), Block::Sand.default_state()));
            if x < min || x > max || z < min || z > max {
                blocks.push((BlockPos::new(x, SKY + 1, z), Block::Stone.default_state()));
                blocks.push((BlockPos::new(x, SKY + 2, z), Block::Stone.default_state()));
            }
        }
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bred_turtles_lay_eggs_on_their_home_beach() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, beach(2, 13)).await;
    let home = Tag::IntArray(vec![7, SKY + 1, 7]);
    for x in [6.5, 7.5] {
        summon(
            &region,
            EntityType::Turtle,
            Compound::new().with("home_pos", home.clone()),
            [x, SKY as f64 + 1.0, 7.5],
        )
        .await;
    }
    world.ticks(2).await;
    for turtle in mobs(&region, EntityType::Turtle).await {
        let effect = interact(&region, &viewer, &mut events, turtle.id, Item::Seagrass).await;
        assert!(matches!(effect, HandEffect::Consume(1)));
    }
    let mut laid = None;
    'search: for _ in 0..2400 {
        world.ticks(1).await;
        for x in 2..=13 {
            for z in 2..=13 {
                let pos = BlockPos::new(x, SKY + 1, z);
                if block_at(&region, pos)
                    .await
                    .is_some_and(|s| s.block() == Block::TurtleEgg)
                {
                    laid = Some(pos);
                    break 'search;
                }
            }
        }
    }
    let laid = laid.expect("a turtle laid its eggs");
    assert!(
        (laid.x - 7).abs() <= 3 && (laid.z - 7).abs() <= 3,
        "close to home: {laid:?}"
    );
    assert_eq!(
        mobs(&region, EntityType::Turtle).await.len(),
        2,
        "breeding made eggs, not a baby"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_walking_on_turtle_eggs_break_them_unless_sneaking() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let egg = BlockPos::new(5, SKY + 1, 5);
    let mut blocks = beach(2, 13);
    blocks.push((egg, Block::TurtleEgg.default_state().with_eggs(2).unwrap()));
    place_blocks(&world, &region, blocks).await;
    let standing = [5.5, SKY as f64 + 1.4375, 5.5];
    let walk = |sneaking: bool| {
        let mut update = player_at(
            &player,
            standing,
            PlayerLooks {
                sneaking,
                ..PlayerLooks::default()
            },
        );
        if let RegionRequest::Player(update) = &mut update {
            update.grounded = true;
        }
        update
    };
    for _ in 0..400 {
        region.request(walk(true)).await;
    }
    world.ticks(1).await;
    assert_eq!(
        block_at(&region, egg).await.and_then(|s| s.eggs()),
        Some(2),
        "sneaking steps carefully"
    );
    let mut broken = false;
    for _ in 0..2000 {
        region.request(walk(false)).await;
        if block_at(&region, egg).await.and_then(|s| s.eggs()) != Some(2) {
            broken = true;
            break;
        }
    }
    assert!(broken, "walking over the clutch breaks an egg");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tamed_saddled_nautilus_goes_where_its_drivers_client_moves_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(200);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(1, 12, 6)).await;
    let start = [3.5, SKY as f64 + 3.0, 6.5];
    let nbt = Compound::new().with("Owner", Tag::uuid(77)).with(
        "equipment",
        Compound::new().with(
            "saddle",
            tuff_item::item::ItemStack::mint(Item::Saddle, 1, tuff_item::item::MintReason::Test)
                .to_nbt(),
        ),
    );
    summon(&region, EntityType::Nautilus, nbt, start).await;
    let nautilus = mobs(&region, EntityType::Nautilus).await[0].id;
    region
        .request(player_at(&player, start, PlayerLooks::default()))
        .await;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: nautilus,
            interaction: interaction(&player, None),
        })
        .await;
    let riding = expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    assert!(riding.driver, "a tamed, saddled nautilus is driven");
    let drive = |jumping| {
        RegionRequest::Vehicle(crate::world::messages::VehicleRequest::Drive {
            player: 200,
            vehicle: nautilus,
            input: crate::world::entity::RiderInput {
                forward: 1.0,
                strafe: 0.0,
                jumping,
                yaw: -90.0,
                ..Default::default()
            },
        })
    };
    region.request(drive(true)).await;
    world.ticks(20).await;
    let idle = mobs(&region, EntityType::Nautilus).await[0].position;
    assert!(
        crate::world::distance_sq(idle, start) < 0.5,
        "the region never swims a nautilus its client drives: {idle:?}"
    );
    let to = [start[0] + 2.0, start[1], start[2]];
    region
        .request(RegionRequest::Vehicle(
            crate::world::messages::VehicleRequest::Move {
                player: 200,
                vehicle: nautilus,
                position: to,
                yaw: -90.0,
                pitch: 0.0,
                on_ground: false,
            },
        ))
        .await;
    world.ticks(2).await;
    let moved = mobs(&region, EntityType::Nautilus).await[0].position;
    assert_eq!(moved, to, "the driver's client moves it");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombie_nautiluses_charge_into_their_target() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(1, 12, 6)).await;
    summon(
        &region,
        EntityType::ZombieNautilus,
        Compound::new(),
        [3.5, SKY as f64 + 3.0, 6.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [9.5, SKY as f64 + 3.0, 6.5],
            PlayerLooks::default(),
        ))
        .await;
    // A wild one never picks a player on sight; something made it angry.
    let nautilus = mobs(&region, EntityType::ZombieNautilus).await[0].id;
    let target = player.id;
    let at = BlockPos::new(3, SKY, 6);
    region
        .request(RegionRequest::WhenLoaded {
            min: at,
            max: at,
            run: crate::world::region_api::RegionTask::new(move |world| {
                if let Some(mob) = world.entity_mut(nautilus).and_then(|t| {
                    t.entity
                        .downcast_mut::<tuff_feature_builtin_vanilla_mobs::Mob<
                            tuff_feature_builtin_vanilla_mobs::nautilus::ZombieNautilus,
                        >>()
                }) {
                    mob.core.target = Some(target);
                }
            }),
        })
        .await;
    let hit = next_hit(&world, &mut events, 400).await;
    assert_eq!(
        hit.attacker.map(|a| a.kind),
        Some(EntityType::ZombieNautilus)
    );
    assert!(hit.knockback.is_some(), "the charge knocks its target on");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_husk_stomps_a_turtle_egg_clutch_to_bits() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let egg = BlockPos::new(10, SKY + 1, 10);
    let mut blocks = beach(2, 13);
    blocks.push((egg, Block::TurtleEgg.default_state().with_eggs(4).unwrap()));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Husk,
        Compound::new().with("PersistenceRequired", 1i8),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    let mut gone = false;
    for _ in 0..1200 {
        world.ticks(1).await;
        if block_at(&region, egg)
            .await
            .is_none_or(|s| s.block() != Block::TurtleEgg)
        {
            gone = true;
            break;
        }
    }
    assert!(gone, "the husk found the clutch and broke it");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_standing_still_on_turtle_eggs_breaks_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let egg = BlockPos::new(6, SKY + 1, 6);
    let mut blocks = beach(2, 13);
    blocks.push((egg, Block::TurtleEgg.default_state().with_eggs(2).unwrap()));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", 1i8),
        [6.5, SKY as f64 + 1.4375, 6.5],
    )
    .await;
    let mut broken = false;
    for _ in 0..1500 {
        world.ticks(1).await;
        if block_at(&region, egg).await.and_then(|s| s.eggs()) != Some(2) {
            broken = true;
            break;
        }
    }
    assert!(broken, "a pig that never moves still cracks an egg");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_a_clutch_by_hand_takes_one_egg_unless_creative() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let egg = BlockPos::new(6, SKY + 1, 6);
    let mut blocks = beach(2, 13);
    blocks.push((egg, Block::TurtleEgg.default_state().with_eggs(3).unwrap()));
    place_blocks(&world, &region, blocks).await;
    let smash = |mode, sequence| RegionRequest::Break {
        player: player.clone(),
        pos: egg,
        tool: None,
        mode,
        sequence,
    };
    use crate::world::messages::BreakMode;
    region.request(smash(BreakMode::Finish, 1)).await;
    assert_eq!(block_at(&region, egg).await.and_then(|s| s.eggs()), Some(2));
    region.request(smash(BreakMode::Creative, 2)).await;
    assert_eq!(
        block_at(&region, egg).await.map(|s| s.block()),
        Some(Block::Air),
        "creative breaks the whole clutch"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_axolotl_rewards_the_player_who_finishes_its_prey() {
    use crate::world::region_api::RegionTask;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(3, 9, 3)).await;
    summon(
        &region,
        EntityType::Axolotl,
        Compound::new().with("NoAI", 1i8),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Cod,
        Compound::new().with("NoAI", 1i8),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(1).await;
    let axolotl = mobs(&region, EntityType::Axolotl).await[0].id;
    let cod = mobs(&region, EntityType::Cod).await[0].id;
    let at = BlockPos::new(0, SKY, 0);
    region
        .request(RegionRequest::WhenLoaded {
            min: at,
            max: at,
            run: RegionTask::new(move |world| {
                if let Some(mob) = world.entity_mut(axolotl).and_then(|t| {
                    t.entity
                        .downcast_mut::<tuff_feature_builtin_vanilla_mobs::Mob<
                            tuff_feature_builtin_vanilla_mobs::axolotl::Axolotl,
                        >>()
                }) {
                    mob.core.target = Some(cod);
                }
            }),
        })
        .await;
    let position = [6.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    world.ticks(1).await;
    while events.try_recv().is_ok() {}
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: cod,
            attack: crate::world::entity::EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: position,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 100.0,
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
    world.ticks(2).await;
    let mut regen = false;
    let mut cleared = false;
    while let Ok(event) = events.try_recv() {
        match event {
            PlayerEvent::StackEffect { effect, add, cap } => {
                regen = effect == MobEffect::Regeneration && add == 100 && cap == 2400;
            }
            PlayerEvent::RemoveEffect(MobEffect::MiningFatigue) => cleared = true,
            _ => {}
        }
    }
    assert!(regen && cleared, "regen {regen}, fatigue cleared {cleared}");
    world.shutdown().await;
}
