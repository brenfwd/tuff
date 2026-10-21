//! Hostile mobs in regions: finding and chasing players, hitting them,
//! calling for help, and leaving in peaceful.

use super::*;
use crate::world::entity::{EntityAttack, EntityHit};
use crate::world::messages::PlayerLooks;
use tuff_vanilla_data::registry::EntityType;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombies_chase_and_hit_players_they_see() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let position = [12.5, SKY as f64 + 1.0, 12.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    assert_eq!(hit.amount, 3.0, "a zombie's attack damage");
    let attacker = hit.attacker.expect("the zombie is to blame");
    assert_eq!(attacker.kind, EntityType::Zombie);
    assert_eq!(
        attacker.name.translate.as_deref(),
        Some("entity.minecraft.zombie")
    );
    let zombie = &mobs(&region, EntityType::Zombie).await[0];
    assert!(
        crate::world::distance_sq(zombie.position, position) < 3.0 * 3.0,
        "it walked over: {:?}",
        zombie.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creative_players_are_left_alone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let position = [11.5, SKY as f64 + 1.0, 11.5];
    let creative = PlayerLooks {
        creative: true,
        invulnerable: true,
        ..Default::default()
    };
    region.request(player_at(&player, position, creative)).await;
    world.ticks(300).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "nobody was attacked"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hurt_zombie_strikes_back_and_calls_its_friends() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    for x in [4.5, 11.5] {
        summon(
            &region,
            EntityType::Zombie,
            Compound::new(),
            [x, SKY as f64 + 1.0, 3.5],
        )
        .await;
    }
    world.ticks(2).await;
    let zombies = mobs(&region, EntityType::Zombie).await;
    let position = [8.0, SKY as f64 + 1.0, 12.5];
    let unseen = PlayerLooks {
        invisible: true,
        ..Default::default()
    };
    region.request(player_at(&player, position, unseen)).await;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: zombies[0].id,
            attack: EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: position,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 1.0,
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
    let mut attackers = std::collections::HashSet::new();
    for _ in 0..8 {
        let hit = next_hit(&world, &mut events, 600).await;
        attackers.insert(hit.attacker.unwrap().id);
        if attackers.len() == 2 {
            break;
        }
    }
    assert_eq!(
        attackers,
        zombies.iter().map(|z| z.id).collect(),
        "both zombies came for the unseen attacker"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn monsters_leave_in_peaceful() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("PersistenceRequired", true),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    world.ticks(2).await;
    assert_eq!(mobs(&region, EntityType::Zombie).await.len(), 1);
    world.difficulty.send_replace(Difficulty::Peaceful);
    wait_for(&world, &region, 40, |found| {
        found.iter().all(|s| s.kind != EntityType::Zombie)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombies_burn_in_the_sun_but_not_at_night() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(18000));
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("PersistenceRequired", true),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(300).await;
    assert_eq!(
        mobs(&region, EntityType::Zombie).await.len(),
        1,
        "no fire at night"
    );
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(6000));
    wait_for(&world, &region, 1200, |found| {
        found.iter().all(|s| s.kind != EntityType::Zombie)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombie_villagers_chase_and_hit_players_they_see() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::ZombieVillager,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let position = [12.5, SKY as f64 + 1.0, 12.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    let attacker = hit.attacker.expect("the zombie villager is to blame");
    assert_eq!(attacker.kind, EntityType::ZombieVillager);
    let zombie = &mobs(&region, EntityType::ZombieVillager).await[0];
    assert!(
        crate::world::distance_sq(zombie.position, position) < 3.0 * 3.0,
        "it walked over: {:?}",
        zombie.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombie_villagers_burn_in_the_sun_but_not_at_night() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(18000));
    summon(
        &region,
        EntityType::ZombieVillager,
        Compound::new().with("PersistenceRequired", true),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(300).await;
    assert_eq!(
        mobs(&region, EntityType::ZombieVillager).await.len(),
        1,
        "no fire at night"
    );
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(6000));
    wait_for(&world, &region, 1200, |found| {
        found.iter().all(|s| s.kind != EntityType::ZombieVillager)
    })
    .await;
    world.shutdown().await;
}

fn player_owner(player: &PlayerHandle) -> tuff_core::world::projectile::Owner {
    tuff_core::world::projectile::Owner::of(
        crate::world::entity::Culprit::player(player.id, 77, "Tester"),
        Some(player.clone()),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arrows_stick_in_walls_and_players_pick_them_up() {
    use tuff_feature_builtin_vanilla_combat::projectile::arrow::Arrow;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = roofed_pen(2, 13);
    for y in SKY + 1..=SKY + 3 {
        for z in 2..=13 {
            blocks.push((BlockPos::new(10, y, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let start = [3.5, SKY as f64 + 2.0, 7.5];
    let arrow = Arrow::new(
        world.shared.next_entity_id(),
        start,
        [2.0, 0.1, 0.0],
        player_owner(&player),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    let stuck = wait_for(&world, &region, 100, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Arrow && (s.position[0] - 10.0).abs() < 0.1)
    })
    .await;
    let arrow = stuck.iter().find(|s| s.kind == EntityType::Arrow).unwrap();
    assert!(arrow.position[0] < 10.0, "it stopped at the wall's face");
    world.ticks(10).await;
    region
        .request(player_at(
            &player,
            [9.0, SKY as f64 + 1.0, 7.5],
            PlayerLooks::default(),
        ))
        .await;
    let offered = ticking_expect(&world, &mut events, 40, |event| match event {
        PlayerEvent::OfferItem { item, stack, .. } if item == arrow.id => Some(stack),
        _ => None,
    })
    .await;
    assert_eq!(offered.item, Item::Arrow);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_refusing_a_stuck_arrow_does_not_starve_another() {
    use tuff_feature_builtin_vanilla_combat::projectile::arrow::Arrow;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (first, mut first_events) = PlayerHandle::new(1000);
    let (second, mut second_events) = PlayerHandle::new(1001);
    subscribe(&region, &first, &mut first_events, &[chunk]).await;
    subscribe(&region, &second, &mut second_events, &[chunk]).await;
    let mut blocks = roofed_pen(2, 13);
    for y in SKY + 1..=SKY + 3 {
        for z in 2..=13 {
            blocks.push((BlockPos::new(10, y, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let arrow = Arrow::new(
        world.shared.next_entity_id(),
        [3.5, SKY as f64 + 2.0, 7.5],
        [2.0, 0.1, 0.0],
        player_owner(&first),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    wait_for(&world, &region, 100, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Arrow && (s.position[0] - 10.0).abs() < 0.1)
    })
    .await;
    world.ticks(10).await;
    for (player, z) in [(&first, 7.5), (&second, 7.8)] {
        region
            .request(player_at(
                player,
                [9.0, SKY as f64 + 1.0, z],
                PlayerLooks::default(),
            ))
            .await;
    }
    let players = [&first, &second];
    let mut refuser = None;
    for _ in 0..20 {
        world.ticks(1).await;
        for (index, events) in [&mut first_events, &mut second_events]
            .into_iter()
            .enumerate()
        {
            let mut offer = None;
            while let Ok(event) = events.try_recv() {
                if let PlayerEvent::OfferItem {
                    item,
                    offer: id,
                    region,
                    ..
                } = event
                {
                    offer = Some((item, id, region));
                }
            }
            let Some((item, id, from)) = offer else {
                continue;
            };
            let taken = match refuser {
                None => {
                    refuser = Some(index);
                    0
                }
                Some(r) if r != index => 1,
                Some(_) => 0,
            };
            from.internal(RegionInternal::ItemTaken {
                item,
                offer: id,
                player: players[index].clone(),
                taken,
            });
            if taken == 1 {
                world.shutdown().await;
                return;
            }
        }
    }
    panic!("only the refusing player was ever offered the arrow");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arrows_hurt_the_mobs_they_hit() {
    use tuff_feature_builtin_vanilla_combat::projectile::arrow::Arrow;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", true),
        [9.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(2).await;
    let mut arrow = Arrow::new(
        world.shared.next_entity_id(),
        [3.5, SKY as f64 + 1.8, 7.5],
        [3.0, 0.0, 0.0],
        player_owner(&player),
    );
    arrow.damage = 10.0;
    region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    wait_for(&world, &region, 100, |found| {
        found
            .iter()
            .all(|s| s.kind != EntityType::Cow && s.kind != EntityType::Arrow)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skeletons_shoot_players_they_see() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let bow =
        tuff_item::item::ItemStack::mint(Item::Bow, 1, tuff_item::item::MintReason::Test).to_nbt();
    summon(
        &region,
        EntityType::Skeleton,
        Compound::new().with("equipment", Compound::new().with("mainhand", bow)),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let position = [12.5, SKY as f64 + 1.0, 12.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:arrow");
    assert_eq!(hit.projectile, Some(EntityType::Arrow));
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Skeleton));
    assert!(hit.amount >= 1.0);
    world.shutdown().await;
}

/// What a mob's main hand currently holds, parsed from its saved slot.
async fn main_hand(region: &RegionHandle, id: i32) -> Option<tuff_item::item::ItemStack> {
    let nbt = entity_nbt(region, id).await?;
    let mainhand = nbt.get("equipment")?.as_compound()?.get("mainhand")?;
    tuff_item::item::ItemStack::mint_from_nbt(
        mainhand.as_compound()?,
        tuff_item::item::MintReason::Test,
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn witches_chase_players_into_potion_range() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 14)).await;
    let spawn = [1.5, SKY as f64 + 1.0, 1.5];
    summon(&region, EntityType::Witch, Compound::new(), spawn).await;
    // 11 blocks away: within its 16-block follow range, but past the
    // 10-block potion range, so it has to close in.
    let position = [12.5, SKY as f64 + 1.0, 1.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let found = wait_for(&world, &region, 600, |found| {
        found
            .iter()
            .find(|s| s.kind == EntityType::Witch)
            .is_some_and(|w| crate::world::distance_sq(w.position, position) < 10.5 * 10.5)
    })
    .await;
    let witch = found.iter().find(|s| s.kind == EntityType::Witch).unwrap();
    assert!(
        crate::world::distance_sq(witch.position, spawn) > 0.5 * 0.5,
        "it left its spawn point to chase the player: {:?}",
        witch.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn witches_throw_splash_potions_on_a_sixty_tick_cooldown() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 12)).await;
    summon(
        &region,
        EntityType::Witch,
        Compound::new(),
        [1.5, SKY as f64 + 1.0, 1.5],
    )
    .await;
    // Five blocks away: too close for the game's distance-based Slowness
    // throw, too far for the 25% Weakness roll, so it always harms.
    let position = [6.5, SKY as f64 + 1.0, 1.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let mut seen = std::collections::HashSet::new();
    let mut throws: Vec<(u32, tuff_item::item::ItemStack)> = Vec::new();
    for tick in 0..400u32 {
        for s in snapshots(&region).await {
            if s.kind == EntityType::SplashPotion && seen.insert(s.id) {
                let nbt = entity_nbt(&region, s.id)
                    .await
                    .expect("the just-spawned potion is tracked");
                let item = tuff_item::item::ItemStack::mint_from_nbt(
                    nbt.get("Item")
                        .and_then(tuff_nbt::Tag::as_compound)
                        .expect("a thrown potion carries its item"),
                    tuff_item::item::MintReason::Test,
                )
                .expect("a valid item stack");
                throws.push((tick, item));
            }
        }
        if throws.len() >= 2 {
            break;
        }
        world.ticks(1).await;
    }
    assert!(throws.len() >= 2, "expected two throws, got {throws:?}");
    let gap = throws[1].0 - throws[0].0;
    assert!(
        (55..=65).contains(&gap),
        "60-tick cooldown between throws, got {gap}"
    );
    for (_, item) in &throws {
        assert_eq!(item.item, Item::SplashPotion);
        assert_eq!(
            item.components
                .potion_contents
                .as_ref()
                .and_then(|c| c.potion),
            Some(tuff_vanilla_data::registry::Potion::Harming)
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hurt_witches_drink_a_healing_potion() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 4)).await;
    summon(
        &region,
        EntityType::Witch,
        Compound::new().with("Health", 10.0f32),
        [2.5, SKY as f64 + 1.0, 2.5],
    )
    .await;
    let id = mobs(&region, EntityType::Witch).await[0].id;
    let before = mob_health(&region, id).await;
    assert_eq!(before, 10.0);
    let mut drank = false;
    for _ in 0..2000u32 {
        if let Some(stack) = main_hand(&region, id).await
            && stack.item == Item::Potion
            && stack
                .components
                .potion_contents
                .as_ref()
                .and_then(|c| c.potion)
                == Some(tuff_vanilla_data::registry::Potion::Healing)
        {
            drank = true;
            break;
        }
        world.ticks(1).await;
    }
    assert!(drank, "the witch never drank a healing potion");
    world.ticks(40).await;
    let after = mob_health(&region, id).await;
    assert!(after > before, "healed from {before} to {after}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ender_pearls_carry_their_thrower_to_where_they_land() {
    use tuff_feature_builtin_vanilla_combat::projectile::thrown::Thrown;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let from = [3.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, from, PlayerLooks::default()))
        .await;
    let pearl = Thrown::new(
        EntityType::EnderPearl,
        world.shared.next_entity_id(),
        [3.5, SKY as f64 + 2.5, 7.5],
        [0.6, 0.3, 0.0],
        tuff_item::item::ItemStack::mint(Item::EnderPearl, 1, tuff_item::item::MintReason::Test),
        player_owner(&player),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(pearl)))
        .await;
    let (position, damage) = ticking_expect(&world, &mut events, 200, |event| match event {
        PlayerEvent::PearlTeleport {
            position,
            fall_damage,
            ..
        } => Some((position, fall_damage)),
        _ => None,
    })
    .await;
    assert_eq!(damage, 5.0);
    assert!(position[0] > 5.0, "it flew east: {position:?}");
    assert!((position[1] - (SKY as f64 + 1.0)).abs() < 1.5);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_teleport_lands_on_solid_ground_within_diameter() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let origin = [7.5, SKY as f64 + 5.5, 7.5];
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::RandomTeleport {
            origin,
            diameter: 8.0,
            width: 0.6,
            height: 1.8,
            reply,
        })
        .await;
    let target = answer.await.unwrap().expect("open pen has room to land");
    assert!(
        (SKY as f64 + 1.0..SKY as f64 + 2.0).contains(&target[1]),
        "one block above the floor: {target:?}"
    );
    for axis in [0, 2] {
        assert!(
            (target[axis] - origin[axis]).abs() <= 4.0,
            "within half the diameter: {target:?}"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_teleport_gives_up_with_no_loaded_ground_nearby() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    // Far outside this region's loaded columns and halo: every attempt's
    // first block lookup misses, so the search cannot succeed.
    let origin = [100_000.5, SKY as f64 + 1.0, 100_000.5];
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::RandomTeleport {
            origin,
            diameter: 8.0,
            width: 0.6,
            height: 1.8,
            reply,
        })
        .await;
    assert_eq!(answer.await.unwrap(), None, "nothing is loaded out there");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn splash_potions_affect_players_near_them_and_lingering_ones_leave_a_cloud() {
    use tuff_feature_builtin_vanilla_combat::projectile::thrown::Thrown;
    use tuff_item::item::{ComponentPatch, ItemStack, MintReason, PotionContents};
    use tuff_vanilla_data::registry::{MobEffect, Potion};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let at = [7.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, at, PlayerLooks::default()))
        .await;
    let potion = |item: Item, potion: Potion| {
        ItemStack::mint(item, 1, MintReason::Test).with_components(ComponentPatch {
            potion_contents: Some(Box::new(PotionContents {
                potion: Some(potion),
                ..Default::default()
            })),
            ..Default::default()
        })
    };
    let throw = |item: ItemStack, kind: EntityType| {
        Thrown::new(
            kind,
            world.shared.next_entity_id(),
            [8.5, SKY as f64 + 1.5, 7.5],
            [0.0, -0.3, 0.0],
            item,
            tuff_core::world::projectile::Owner::default(),
        )
    };
    let splash = throw(
        potion(Item::SplashPotion, Potion::LongFireResistance),
        EntityType::SplashPotion,
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(splash)))
        .await;
    let hit = ticking_expect(&world, &mut events, 100, |event| match event {
        PlayerEvent::Potion(hit) => Some(hit),
        _ => None,
    })
    .await;
    assert_eq!(hit.effects.len(), 1);
    assert_eq!(hit.effects[0].effect, MobEffect::FireResistance);
    assert!(
        hit.effects[0].duration > 4000 && hit.effects[0].duration < 9600,
        "scaled by distance: {}",
        hit.effects[0].duration
    );

    let harming = throw(
        potion(Item::SplashPotion, Potion::Harming),
        EntityType::SplashPotion,
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(harming)))
        .await;
    let hurt = next_hit(&world, &mut events, 100).await;
    assert_eq!(hurt.kind.name, "minecraft:indirect_magic");
    assert!(hurt.amount >= 3.0 && hurt.amount <= 6.0);

    let lingering = throw(
        potion(Item::LingeringPotion, Potion::Regeneration),
        EntityType::LingeringPotion,
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(lingering)))
        .await;
    let cloud = ticking_expect(&world, &mut events, 100, |event| match event {
        PlayerEvent::Potion(hit) => Some(hit),
        _ => None,
    })
    .await;
    assert_eq!(cloud.effects[0].effect, MobEffect::Regeneration);
    assert_eq!(
        cloud.effects[0].duration,
        900 / 4,
        "a quarter of the potion"
    );
    world.shutdown().await;
}

/// The mob's saved `Health` tag, for asserting on damage a test can't
/// observe through a player's own event stream (nothing hurt them).
async fn mob_health(region: &RegionHandle, id: i32) -> f32 {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer
        .await
        .unwrap()
        .and_then(|nbt| nbt.get("Health").and_then(|t| t.as_float()))
        .expect("mob has a Health tag")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn splash_water_hurts_water_sensitive_mobs() {
    use tuff_feature_builtin_vanilla_combat::projectile::thrown::Thrown;
    use tuff_item::item::{ComponentPatch, ItemStack, MintReason, PotionContents};
    use tuff_vanilla_data::registry::Potion;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Blaze,
        Compound::new(),
        [7.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    let blaze = mobs(&region, EntityType::Blaze).await;
    let id = blaze[0].id;
    let before = mob_health(&region, id).await;
    let water =
        ItemStack::mint(Item::SplashPotion, 1, MintReason::Test).with_components(ComponentPatch {
            potion_contents: Some(Box::new(PotionContents {
                potion: Some(Potion::Water),
                ..Default::default()
            })),
            ..Default::default()
        });
    let splash = Thrown::new(
        EntityType::SplashPotion,
        world.shared.next_entity_id(),
        [8.0, SKY as f64 + 1.5, 8.0],
        [0.0, -0.3, 0.0],
        water,
        tuff_core::world::projectile::Owner::default(),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(splash)))
        .await;
    world.ticks(20).await;
    let after = mob_health(&region, id).await;
    assert_eq!(before - after, 1.0, "one point of water-contact damage");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn splash_potions_give_mobs_lasting_effects_that_tick() {
    use tuff_feature_builtin_vanilla_combat::projectile::thrown::Thrown;
    use tuff_item::item::{ComponentPatch, ItemStack, MintReason, PotionContents};
    use tuff_vanilla_data::registry::Potion;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new(),
        [7.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    let pig = mobs(&region, EntityType::Pig).await;
    let id = pig[0].id;
    let before = mob_health(&region, id).await;
    let poison =
        ItemStack::mint(Item::SplashPotion, 1, MintReason::Test).with_components(ComponentPatch {
            potion_contents: Some(Box::new(PotionContents {
                potion: Some(Potion::Poison),
                ..Default::default()
            })),
            ..Default::default()
        });
    let splash = Thrown::new(
        EntityType::SplashPotion,
        world.shared.next_entity_id(),
        [8.0, SKY as f64 + 1.5, 8.0],
        [0.0, -0.3, 0.0],
        poison,
        tuff_core::world::projectile::Owner::default(),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(splash)))
        .await;
    // Poison ticks every 25 game ticks at amplifier 0; give it time to land
    // and tick at least once.
    world.ticks(40).await;
    let after = mob_health(&region, id).await;
    assert!(after < before, "poison ticked the pig's health down");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bottles_o_enchanting_break_into_experience_in_the_vanilla_range() {
    use tuff_feature_builtin_vanilla_combat::projectile::thrown::Thrown;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let at = [7.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, at, PlayerLooks::default()))
        .await;
    let bottle = Thrown::new(
        EntityType::ExperienceBottle,
        world.shared.next_entity_id(),
        [7.5, SKY as f64 + 1.5, 7.5],
        [0.0, 0.05, 0.0],
        tuff_item::item::ItemStack::mint(
            Item::ExperienceBottle,
            1,
            tuff_item::item::MintReason::Test,
        ),
        tuff_core::world::projectile::Owner::default(),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(bottle)))
        .await;
    let mut collected = 0;
    for _ in 0..200 {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            if let PlayerEvent::Experience { amount } = event {
                collected += amount;
            }
        }
    }
    assert!((3..=11).contains(&collected), "{collected}");
    let found = snapshots(&region).await;
    assert!(
        !found.iter().any(|s| s.kind == EntityType::ExperienceBottle),
        "the bottle is gone"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creepers_sneak_up_and_explode() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Creeper,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let position = [11.5, SKY as f64 + 1.0, 11.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let damage = ticking_expect(&world, &mut events, 800, |event| match event {
        PlayerEvent::Exploded { damage, .. } => Some(damage),
        _ => None,
    })
    .await;
    assert!(damage > 0.0);
    assert!(
        mobs(&region, EntityType::Creeper).await.is_empty(),
        "it is gone"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flint_and_steel_lights_a_creeper() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Creeper,
        Compound::new().with("NoAI", true),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(2).await;
    let creeper = mobs(&region, EntityType::Creeper).await[0].id;
    let effect = interact(&region, &player, &mut events, creeper, Item::FlintAndSteel).await;
    assert!(matches!(
        effect,
        crate::world::messages::HandEffect::Damage(1)
    ));
    wait_for(&world, &region, 60, |found| {
        found.iter().all(|s| s.kind != EntityType::Creeper)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spiders_climb_walls_to_reach_players_in_the_dark() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 1..=14 {
        for z in 1..=14 {
            blocks.push((BlockPos::new(x, SKY + 8, z), Block::Stone.default_state()));
        }
    }
    for x in 10..=11 {
        for z in 10..=11 {
            for y in SKY + 1..=SKY + 3 {
                blocks.push((BlockPos::new(x, y, z), Block::Stone.default_state()));
            }
        }
    }
    place_blocks(&world, &region, blocks).await;
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(18000));
    summon(
        &region,
        EntityType::Spider,
        Compound::new(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [11.0, SKY as f64 + 4.0, 11.0],
            PlayerLooks::default(),
        ))
        .await;
    let hit = next_hit(&world, &mut events, 1200).await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Spider));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spiders_leave_players_alone_in_daylight() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(6000));
    world.ticks(20).await;
    summon(
        &region,
        EntityType::Spider,
        Compound::new(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [11.5, SKY as f64 + 1.0, 11.5],
            PlayerLooks::default(),
        ))
        .await;
    world.ticks(300).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "the spider stayed neutral"
        );
    }
    world.shutdown().await;
}

/// Looks from a player standing at `from` to the eyes of a mob standing at
/// `at` with eye height `eye`.
fn looking_at(from: [f64; 3], at: [f64; 3], eye: f64) -> PlayerLooks {
    let looks = PlayerLooks::default();
    let (dx, dy, dz) = (
        at[0] - from[0],
        at[1] + eye - (from[1] + looks.eye_height),
        at[2] - from[2],
    );
    let horizontal = (dx * dx + dz * dz).sqrt();
    PlayerLooks {
        yaw: (dz.atan2(dx).to_degrees() - 90.0) as f32,
        pitch: (-dy.atan2(horizontal).to_degrees()) as f32,
        ..looks
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endermen_attack_players_who_stared_at_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 1..=14 {
        for z in 1..=14 {
            blocks.push((BlockPos::new(x, SKY + 5, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let at = [4.5, SKY as f64 + 1.0, 7.5];
    summon(
        &region,
        EntityType::Enderman,
        Compound::new().with("NoAI", false),
        at,
    )
    .await;
    world.ticks(5).await;
    let away = PlayerLooks {
        pitch: 90.0,
        ..Default::default()
    };
    let far_end = [12.5, SKY as f64 + 1.0, 7.5];
    region.request(player_at(&player, far_end, away)).await;
    world.ticks(200).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "neutral at first"
        );
    }
    let enderman = mobs(&region, EntityType::Enderman).await[0].position;
    let from = if enderman[0] < 7.5 {
        far_end
    } else {
        [2.5, SKY as f64 + 1.0, 7.5]
    };
    let eye = f64::from(EntityType::Enderman.size().eye_height);
    for _ in 0..20 {
        let enderman = mobs(&region, EntityType::Enderman).await[0].position;
        region
            .request(player_at(&player, from, looking_at(from, enderman, eye)))
            .await;
        world.ticks(1).await;
    }
    region.request(player_at(&player, from, away)).await;
    let hit = next_hit(&world, &mut events, 800).await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Enderman));
    assert_eq!(hit.amount, 7.0);
    world.shutdown().await;
}

/// A player who keeps a close, aggroed enderman pinned in its gaze (as a
/// real client tracking it with the mouse would) should not see it blink
/// away on literally every game tick: vanilla's own close-range branch has
/// no extra cooldown either, but each teleport lands somewhere the player
/// is no longer precisely aimed at (or that breaks line of sight), so
/// repeats are naturally spread out rather than every tick. This is the
/// full-pipeline counterpart to
/// `mob::enderman::tests::close_stare_teleport_fires_every_full_ai_step_and_no_more`,
/// which pins down the exact per-AI-step gate in isolation.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endermen_do_not_teleport_on_every_single_tick_while_stared_at_up_close() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 1..=14 {
        for z in 1..=14 {
            blocks.push((BlockPos::new(x, SKY + 5, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let at = [4.5, SKY as f64 + 1.0, 7.5];
    summon(
        &region,
        EntityType::Enderman,
        Compound::new().with("NoAI", false),
        at,
    )
    .await;
    world.ticks(5).await;
    let away = PlayerLooks {
        pitch: 90.0,
        ..Default::default()
    };
    let far_end = [12.5, SKY as f64 + 1.0, 7.5];
    region.request(player_at(&player, far_end, away)).await;
    world.ticks(200).await;
    let enderman = mobs(&region, EntityType::Enderman).await[0].position;
    let from = if enderman[0] < 7.5 {
        far_end
    } else {
        [2.5, SKY as f64 + 1.0, 7.5]
    };
    let eye = f64::from(EntityType::Enderman.size().eye_height);
    for _ in 0..20 {
        let enderman = mobs(&region, EntityType::Enderman).await[0].position;
        region
            .request(player_at(&player, from, looking_at(from, enderman, eye)))
            .await;
        world.ticks(1).await;
    }
    // Aggro established (as in endermen_attack_players_who_stared_at_them).
    // Now close the distance to under 4 blocks and keep staring, counting
    // teleports (position changes) over many ticks.
    const TICKS: u32 = 60;
    let mut teleports = 0u32;
    let mut prev = mobs(&region, EntityType::Enderman).await[0].position;
    for _ in 0..TICKS {
        let enderman = mobs(&region, EntityType::Enderman).await[0].position;
        let close = [enderman[0] + 2.0, enderman[1], enderman[2]];
        region
            .request(player_at(&player, close, looking_at(close, enderman, eye)))
            .await;
        world.ticks(1).await;
        let now = mobs(&region, EntityType::Enderman).await[0].position;
        if now != prev {
            teleports += 1;
        }
        prev = now;
    }
    assert!(
        teleports < TICKS,
        "teleported on every single one of {TICKS} ticks"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endermen_teleport_away_from_arrows() {
    use tuff_feature_builtin_vanilla_combat::projectile::arrow::Arrow;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunks: Vec<ChunkPos> = (9..=13)
        .flat_map(|x| (9..=13).map(move |z| ChunkPos::new(x, z)))
        .collect();
    let region = world.region(chunks[0].region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &chunks).await;
    let mut blocks = Vec::new();
    for x in 148..=220 {
        for z in 148..=220 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let at = [184.5, SKY as f64 + 1.0, 184.5];
    summon(
        &region,
        EntityType::Enderman,
        Compound::new().with("NoAI", true),
        at,
    )
    .await;
    world.ticks(2).await;
    let arrow = Arrow::new(
        world.shared.next_entity_id(),
        [178.5, SKY as f64 + 2.0, 184.5],
        [3.0, 0.0, 0.0],
        player_owner(&player),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    let moved = wait_for(&world, &region, 40, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Enderman && s.position != at)
    })
    .await;
    assert_eq!(
        moved
            .iter()
            .filter(|s| s.kind == EntityType::Enderman)
            .count(),
        1,
        "unhurt, it only moved"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mobs_hunt_players_across_region_borders_through_the_player_index() {
    use crate::world::nearby::{IndexedPlayer, PlayerIndex};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (west, east) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    let west_region = world.region(west.region()).await;
    let east_region = world.region(east.region()).await;
    let (viewer, mut viewer_events) = PlayerHandle::new(999);
    subscribe(&west_region, &viewer, &mut viewer_events, &[west]).await;
    subscribe(&east_region, &viewer, &mut viewer_events, &[east]).await;
    let floor = |xs: std::ops::RangeInclusive<i32>| {
        let mut blocks = Vec::new();
        for x in xs {
            for z in 2..=6 {
                blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
                blocks.push((BlockPos::new(x, SKY + 4, z), Block::Stone.default_state()));
            }
            for z in [1, 7] {
                for y in SKY + 1..=SKY + 3 {
                    blocks.push((BlockPos::new(x, y, z), Block::Stone.default_state()));
                }
            }
        }
        blocks
    };
    place_blocks(&world, &west_region, floor(500..=511)).await;
    place_blocks(&world, &east_region, floor(512..=520)).await;
    let (player, mut events) = PlayerHandle::new(1000);
    let position = [518.5, SKY as f64 + 1.0, 4.5];
    let overworld = world.dimension(OVERWORLD);
    world.players.send_replace(Arc::new(PlayerIndex::build([(
        overworld,
        IndexedPlayer {
            handle: player.clone(),
            uuid: 77,
            name: Arc::from("Tester"),
            position,
            looks: PlayerLooks::default(),
            time_since_rest: 0,
        },
    )])));
    summon(
        &west_region,
        EntityType::Zombie,
        Compound::new(),
        [503.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    let hit = next_hit(&world, &mut events, 800).await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Zombie));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawners_spawn_their_mob_while_players_are_near() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let spawn_data = Tag::Compound(
        Compound::new().with("entity", Compound::new().with("id", "minecraft:blaze")),
    );
    let mut op = place(&player, BlockPos::new(8, SKY, 8), Item::Spawner, 1);
    op.components = tuff_item::item::Components::from(tuff_item::item::ComponentPatch {
        other: Compound::new().with(
            "minecraft:block_entity_data",
            Compound::new()
                .with("id", "minecraft:mob_spawner")
                .with("SpawnData", spawn_data),
        ),
        ..Default::default()
    });
    op.game_master = true;
    region.request(RegionRequest::Place(op)).await;
    world.ticks(60).await;
    assert!(
        mobs(&region, EntityType::Blaze).await.is_empty(),
        "nothing spawns without a player in range"
    );
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 4.5],
            PlayerLooks {
                creative: true,
                invulnerable: true,
                ..Default::default()
            },
        ))
        .await;
    let found = wait_for(&world, &region, 200, |found| {
        found.iter().any(|s| s.kind == EntityType::Blaze)
    })
    .await;
    let blaze = found.iter().find(|s| s.kind == EntityType::Blaze).unwrap();
    assert!(crate::world::distance_sq(blaze.position, [8.5, SKY as f64 + 1.0, 8.5]) < 6.0 * 6.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawners_do_nothing_when_the_spawner_blocks_work_rule_is_off() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "spawner_blocks_work",
                crate::world::game_rules::RuleValue::Bool(false),
            )
            .unwrap();
    });
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let spawn_data = Tag::Compound(
        Compound::new().with("entity", Compound::new().with("id", "minecraft:blaze")),
    );
    let mut op = place(&player, BlockPos::new(8, SKY, 8), Item::Spawner, 1);
    op.components = tuff_item::item::Components::from(tuff_item::item::ComponentPatch {
        other: Compound::new().with(
            "minecraft:block_entity_data",
            Compound::new()
                .with("id", "minecraft:mob_spawner")
                .with("SpawnData", spawn_data),
        ),
        ..Default::default()
    });
    op.game_master = true;
    region.request(RegionRequest::Place(op)).await;
    world.ticks(60).await;
    assert!(
        mobs(&region, EntityType::Blaze).await.is_empty(),
        "nothing spawns without a player in range"
    );
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 4.5],
            PlayerLooks {
                creative: true,
                invulnerable: true,
                ..Default::default()
            },
        ))
        .await;
    world.ticks(200).await;
    assert!(mobs(&region, EntityType::Blaze).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawners_dress_their_mob_from_an_equipment_table() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let spawn_data = Compound::new()
        .with("entity", Compound::new().with("id", "minecraft:blaze"))
        .with(
            "equipment",
            Compound::new()
                .with("loot_table", "minecraft:equipment/trial_chamber_melee")
                .with("slot_drop_chances", 0.25f32),
        );
    let mut op = place(&player, BlockPos::new(8, SKY, 8), Item::Spawner, 1);
    op.components = tuff_item::item::Components::from(tuff_item::item::ComponentPatch {
        other: Compound::new().with(
            "minecraft:block_entity_data",
            Compound::new()
                .with("id", "minecraft:mob_spawner")
                .with("SpawnData", Tag::Compound(spawn_data)),
        ),
        ..Default::default()
    });
    op.game_master = true;
    region.request(RegionRequest::Place(op)).await;
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 4.5],
            PlayerLooks {
                creative: true,
                invulnerable: true,
                ..Default::default()
            },
        ))
        .await;
    let found = wait_for(&world, &region, 200, |found| {
        found.iter().any(|s| s.kind == EntityType::Blaze)
    })
    .await;
    let id = found
        .iter()
        .find(|s| s.kind == EntityType::Blaze)
        .unwrap()
        .id;
    let (reply, answer) = tokio::sync::oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    let nbt = answer.await.unwrap().expect("the blaze is there");
    let equipment = nbt
        .get_compound("equipment")
        .expect("dressed from the table");
    assert!(!equipment.is_empty());
    assert!(nbt.get_compound("drop_chances").is_some());
    world.shutdown().await;
}

/// `custom_spawn_rules` replace the mob's own rules (zombies in full
/// daylight here), and the next mob a spawner rolls reaches its viewers so
/// the cage shows it.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawners_follow_custom_spawn_rules_and_show_the_next_mob() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(6000));
    let any_light = Compound::new()
        .with("min_inclusive", 0)
        .with("max_inclusive", 15);
    let rules = Compound::new()
        .with("sky_light_limit", any_light.clone())
        .with("block_light_limit", any_light);
    let data = |health: f32| {
        Compound::new()
            .with(
                "entity",
                Compound::new()
                    .with("id", "minecraft:zombie")
                    .with("Health", health),
            )
            .with("custom_spawn_rules", rules.clone())
    };
    let mut op = place(&player, BlockPos::new(8, SKY, 8), Item::Spawner, 1);
    op.components = tuff_item::item::Components::from(tuff_item::item::ComponentPatch {
        other: Compound::new().with(
            "minecraft:block_entity_data",
            Compound::new()
                .with("id", "minecraft:mob_spawner")
                .with("Delay", 0i16)
                .with("SpawnData", data(20.0))
                .with(
                    "SpawnPotentials",
                    Tag::List(vec![Tag::Compound(
                        Compound::new().with("weight", 1).with("data", data(7.0)),
                    )]),
                ),
        ),
        ..Default::default()
    });
    op.game_master = true;
    region.request(RegionRequest::Place(op)).await;
    world.ticks(2).await;
    while events.try_recv().is_ok() {}
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 4.5],
            PlayerLooks {
                creative: true,
                invulnerable: true,
                ..Default::default()
            },
        ))
        .await;
    wait_for(&world, &region, 100, |found| {
        found.iter().any(|s| s.kind == EntityType::Zombie)
    })
    .await;
    let data_id = tuff_vanilla_data::packet::play::Clientbound::BlockEntityData.id();
    expect(&mut events, |event| match event {
        PlayerEvent::ChunkPackets { pos, packets, .. }
            if pos == chunk && packets.iter().any(|p| framed_packet_id(p) == data_id) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawner_minecarts_spawn_their_mob_while_players_are_near() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let spawn_data = Compound::new().with("entity", Compound::new().with("id", "minecraft:blaze"));
    summon(
        &region,
        EntityType::SpawnerMinecart,
        Compound::new()
            .with("SpawnData", spawn_data)
            .with("Delay", 0i16),
        [8.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    world.ticks(60).await;
    assert!(
        mobs(&region, EntityType::Blaze).await.is_empty(),
        "nothing spawns without a player in range"
    );
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 4.5],
            PlayerLooks {
                creative: true,
                invulnerable: true,
                ..Default::default()
            },
        ))
        .await;
    let found = wait_for(&world, &region, 200, |found| {
        found.iter().any(|s| s.kind == EntityType::Blaze)
    })
    .await;
    assert!(found.iter().any(|s| s.kind == EntityType::Blaze));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blazes_shoot_fireballs_that_set_players_alight() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Blaze,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [12.5, SKY as f64 + 1.0, 12.5],
            PlayerLooks::default(),
        ))
        .await;
    let hit = ticking_expect(&world, &mut events, 800, |event| match event {
        PlayerEvent::EntityHurt(hit) if hit.projectile == Some(EntityType::SmallFireball) => {
            Some(hit)
        }
        _ => None,
    })
    .await;
    assert_eq!(hit.kind.name, "minecraft:fireball");
    assert_eq!(hit.amount, 5.0);
    assert_eq!(hit.ignite, 5.0);
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Blaze));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_barter_gold_and_spare_players_in_gold() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Piglin,
        Compound::new().with("IsImmuneToZombification", true),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let position = [10.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, position, in_gold()))
        .await;
    world.ticks(5).await;
    let piglin = mobs(&region, EntityType::Piglin).await[0].id;
    let effect = interact(&region, &player, &mut events, piglin, Item::GoldIngot).await;
    assert!(matches!(
        effect,
        crate::world::messages::HandEffect::Consume(1)
    ));
    wait_for(&world, &region, 200, |found| {
        found.iter().any(|s| s.kind == EntityType::Item)
    })
    .await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "gold keeps it calm"
        );
    }
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Piglin));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombified_piglins_gang_up_on_whoever_hurts_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    for x in [4.5, 11.5] {
        summon(
            &region,
            EntityType::ZombifiedPiglin,
            Compound::new(),
            [x, SKY as f64 + 1.0, 3.5],
        )
        .await;
    }
    let position = [8.0, SKY as f64 + 1.0, 12.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    world.ticks(200).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "neutral until hurt"
        );
    }
    let piglins = mobs(&region, EntityType::ZombifiedPiglin).await;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: piglins[0].id,
            attack: EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: position,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 1.0,
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
    let mut attackers = std::collections::HashSet::new();
    for _ in 0..8 {
        let hit = next_hit(&world, &mut events, 600).await;
        attackers.insert(hit.attacker.unwrap().id);
        if attackers.len() == 2 {
            break;
        }
    }
    assert_eq!(attackers.len(), 2, "the whole group joined in");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn strays_shoot_slowness_arrows_and_husks_starve() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    world.ticks(20).await;
    summon_fresh(&region, EntityType::Stray, [3.5, SKY as f64 + 1.0, 3.5]).await;
    let position = [12.5, SKY as f64 + 1.0, 12.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = ticking_expect(&world, &mut events, 800, |event| match event {
        PlayerEvent::EntityHurt(hit) if hit.projectile == Some(EntityType::Arrow) => Some(hit),
        _ => None,
    })
    .await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Stray));
    assert_eq!(
        hit.effects
            .iter()
            .map(|e| (e.effect, e.duration))
            .collect::<Vec<_>>(),
        [(tuff_vanilla_data::registry::MobEffect::Slowness, 600)]
    );
    summon(
        &region,
        EntityType::Husk,
        Compound::new(),
        [11.5, SKY as f64 + 1.0, 11.5],
    )
    .await;
    let hit = ticking_expect(&world, &mut events, 800, |event| match event {
        PlayerEvent::EntityHurt(hit)
            if hit
                .attacker
                .as_ref()
                .is_some_and(|a| a.kind == EntityType::Husk) =>
        {
            Some(hit)
        }
        _ => None,
    })
    .await;
    let hunger: Vec<_> = hit.effects.iter().map(|e| (e.effect, e.duration)).collect();
    assert_eq!(
        hunger,
        [(tuff_vanilla_data::registry::MobEffect::Hunger, 140)]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn infested_blocks_release_silverfish_unless_mined_with_silk_touch() {
    use crate::world::messages::BreakMode;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let (plain, silky) = (BlockPos::new(4, SKY + 1, 4), BlockPos::new(10, SKY + 1, 10));
    let mut blocks = roofed_pen(2, 13);
    blocks.push((plain, Block::InfestedStone.default_state()));
    blocks.push((silky, Block::InfestedCobblestone.default_state()));
    place_blocks(&world, &region, blocks).await;
    let mut silk = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test);
    silk.components.enchantments = Some(tuff_item::item::Enchantments::new([(
        "minecraft:silk_touch".to_owned(),
        1,
    )]));
    for (sequence, pos, tool) in [(1, silky, Some(silk)), (2, plain, None)] {
        region
            .request(RegionRequest::Break {
                player: player.clone(),
                pos,
                tool,
                mode: BreakMode::Finish,
                sequence,
            })
            .await;
    }
    let found = wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::Silverfish)
    })
    .await;
    let silverfish: Vec<_> = found
        .iter()
        .filter(|s| s.kind == EntityType::Silverfish)
        .collect();
    assert_eq!(silverfish.len(), 1, "silk touch keeps it in");
    assert!(crate::world::distance_sq(silverfish[0].position, [4.5, SKY as f64 + 1.0, 4.5]) < 1.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hurt_silverfish_wake_their_friends_in_the_walls() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = roofed_pen(2, 13);
    for x in 4..8 {
        blocks.push((
            BlockPos::new(x, SKY + 1, 1),
            Block::InfestedStoneBricks.default_state(),
        ));
    }
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Silverfish,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let id = mobs(&region, EntityType::Silverfish).await[0].id;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: id,
            attack: attack(1.0, None),
            sweep: None,
        })
        .await;
    wait_for(&world, &region, 60, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Silverfish)
            .count()
            >= 2
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn some_ender_pearls_leave_endermites_that_die_of_old_age() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let creative = PlayerLooks {
        creative: true,
        invulnerable: true,
        ..Default::default()
    };
    region
        .request(player_at(&player, [8.5, SKY as f64 + 1.0, 8.5], creative))
        .await;
    for i in 0..150 {
        summon(
            &region,
            EntityType::EnderPearl,
            Compound::new().with("Owner", tuff_nbt::Tag::uuid(77)),
            [
                3.5 + f64::from(i % 10),
                SKY as f64 + 2.5,
                3.5 + f64::from(i / 10) * 0.6,
            ],
        )
        .await;
    }
    let found = wait_for(&world, &region, 60, |found| {
        !found.iter().any(|s| s.kind == EntityType::EnderPearl)
    })
    .await;
    let mites = found
        .iter()
        .filter(|s| s.kind == EntityType::Endermite)
        .count();
    assert!((1..40).contains(&mites), "about one in twenty: {mites}");
    summon(
        &region,
        EntityType::Endermite,
        Compound::new().with("Lifetime", 2398),
        [8.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let before = mobs(&region, EntityType::Endermite).await.len();
    world.ticks(3).await;
    assert_eq!(
        mobs(&region, EntityType::Endermite).await.len(),
        before - 1,
        "the old one is gone"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_pick_up_dropped_gold_one_ingot_at_a_time_and_barter() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Piglin,
        Compound::new()
            .with("IsImmuneToZombification", true)
            .with("CanPickUpLoot", true),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [12.5, SKY as f64 + 1.0, 12.5],
            in_gold(),
        ))
        .await;
    summon(
        &region,
        EntityType::Item,
        Compound::new().with(
            "Item",
            ItemStack::mint(Item::GoldIngot, 2, MintReason::Test).to_nbt(),
        ),
        [9.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let gold = mobs(&region, EntityType::Item).await[0].id;
    let found = wait_for(&world, &region, 500, |found| {
        !found.iter().any(|s| s.id == gold)
            && found.iter().filter(|s| s.kind == EntityType::Item).count() >= 2
    })
    .await;
    let barters = found.iter().filter(|s| s.kind == EntityType::Item).count();
    assert!(barters >= 2, "one barter per ingot: {found:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endermen_hunt_and_kill_nearby_endermites() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Enderman,
        Compound::new(),
        [4.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    summon(
        &region,
        EntityType::Endermite,
        Compound::new(),
        [10.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    wait_for(&world, &region, 600, |found| {
        !found.iter().any(|s| s.kind == EntityType::Endermite)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endermen_ignore_endermites_beyond_their_follow_range() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let far_chunk = ChunkPos::new(5, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk, far_chunk]).await;
    // The follow range attribute is 64 blocks; keep the endermite well
    // outside it so it is never a valid target. Both mobs are NoAI so
    // this isolates the target goal's range check from pathfinding.
    let mut blocks = pen(2, 13);
    for x in 5..=95 {
        blocks.push((BlockPos::new(x, SKY, 7), Block::Stone.default_state()));
    }
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Enderman,
        Compound::new().with("NoAI", true),
        [4.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let start = [90.5, SKY as f64 + 1.0, 7.5];
    summon(
        &region,
        EntityType::Endermite,
        Compound::new().with("NoAI", true),
        start,
    )
    .await;
    world.ticks(200).await;
    let endermite = mobs(&region, EntityType::Endermite).await;
    assert_eq!(endermite.len(), 1, "it never gets attacked or killed");
    assert_eq!(
        endermite[0].position, start,
        "far away, the enderman never comes for it"
    );
    world.shutdown().await;
}

/// The on-fire bit of a framed Set Entity Data packet for `id`, when it is
/// a lone entity-flags update (as sent by `flags_packets`).
fn fire_flag(id: i32, event: &PlayerEvent) -> Option<bool> {
    let PlayerEvent::EntityPackets { entity, packets } = event else {
        return None;
    };
    if *entity != id {
        return None;
    }
    packets.iter().find_map(|frame| {
        let mut r = framed_payload(frame);
        if r.read_var_int().ok()?
            != tuff_vanilla_data::packet::play::Clientbound::SetEntityData.id()
        {
            return None;
        }
        r.read_var_int().ok()?;
        let index = r.read_unsigned_byte().ok()?;
        if index != crate::protocol::play::metadata::ENTITY_FLAGS {
            return None;
        }
        r.read_var_int().ok()?;
        let byte = r.read_byte().ok()?;
        Some(byte & crate::protocol::play::metadata::FLAG_ON_FIRE != 0)
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burning_skeleton_sends_its_on_fire_flag_without_being_hit() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(6000));
    summon(
        &region,
        EntityType::Skeleton,
        Compound::new()
            .with("PersistenceRequired", true)
            .with("NoAI", true),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let skeletons = mobs(&region, EntityType::Skeleton).await;
    assert_eq!(skeletons.len(), 1);
    let id = skeletons[0].id;

    ticking_expect(&world, &mut events, 900, |event| {
        fire_flag(id, &event).filter(|on| *on)
    })
    .await;
    assert_eq!(
        mobs(&region, EntityType::Skeleton).await.len(),
        1,
        "the sun burn does not kill it before the flag arrives"
    );

    // Block the sky so it stops re-igniting, then let its remaining fire
    // burn out; the off edge must reach viewers too.
    place_blocks(
        &world,
        &region,
        (6..=8)
            .flat_map(|x| (6..=8).map(move |z| (x, z)))
            .map(|(x, z)| (BlockPos::new(x, SKY + 4, z), Block::Stone.default_state())),
    )
    .await;
    ticking_expect(&world, &mut events, 200, |event| {
        fire_flag(id, &event).filter(|on| !*on)
    })
    .await;
    assert_eq!(
        mobs(&region, EntityType::Skeleton).await.len(),
        1,
        "the skeleton survives the burn"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cave_spiders_poison_players_they_bite() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::CaveSpider,
        Compound::new(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [5.0, SKY as f64 + 1.0, 5.0],
            PlayerLooks::default(),
        ))
        .await;
    let hit = next_hit(&world, &mut events, 200).await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::CaveSpider));
    assert!(
        hit.effects
            .iter()
            .any(|e| e.effect == tuff_vanilla_data::registry::MobEffect::Poison),
        "a cave spider's bite poisons its target: {:?}",
        hit.effects
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglin_brutes_attack_players_even_in_gold() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::PiglinBrute,
        Compound::new(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    region
        .request(player_at(&player, [5.0, SKY as f64 + 1.0, 5.0], in_gold()))
        .await;
    let hit = next_hit(&world, &mut events, 200).await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::PiglinBrute));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bogged_shoot_poison_arrows_and_can_be_sheared() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    world.ticks(20).await;
    summon_fresh(&region, EntityType::Bogged, [3.5, SKY as f64 + 1.0, 3.5]).await;
    let position = [12.5, SKY as f64 + 1.0, 12.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = ticking_expect(&world, &mut events, 800, |event| match event {
        PlayerEvent::EntityHurt(hit) if hit.projectile == Some(EntityType::Arrow) => Some(hit),
        _ => None,
    })
    .await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Bogged));
    assert_eq!(
        hit.effects
            .iter()
            .map(|e| (e.effect, e.duration))
            .collect::<Vec<_>>(),
        [(tuff_vanilla_data::registry::MobEffect::Poison, 100)]
    );
    let bogged = mobs(&region, EntityType::Bogged).await[0].id;
    let effect = interact(&region, &player, &mut events, bogged, Item::Shears).await;
    assert!(matches!(
        effect,
        crate::world::messages::HandEffect::Damage(1)
    ));
    wait_for(&world, &region, 60, |found| {
        found.iter().any(|s| s.kind == EntityType::Item)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breezes_blast_wind_charges_at_players_in_range() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Breeze,
        Compound::new(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [12.5, SKY as f64 + 1.0, 12.5],
            PlayerLooks::default(),
        ))
        .await;
    let mut direct = None;
    let blast = ticking_expect(&world, &mut events, 200, |event| match event {
        PlayerEvent::EntityHurt(hit) if hit.projectile == Some(EntityType::BreezeWindCharge) => {
            direct = Some(hit);
            None
        }
        PlayerEvent::Exploded { blast, .. } => Some(blast),
        _ => None,
    })
    .await;
    assert_eq!(blast.culprit.map(|c| c.kind), Some(EntityType::Breeze));
    if let Some(hit) = direct {
        assert_eq!(hit.amount, 1.0);
        assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Breeze));
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_neutral_mobs_grudge_survives_saving_and_loading() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let grudge = 0x1234_5678_9abc_def0_u128;
    summon(
        &region,
        EntityType::Enderman,
        Compound::new()
            .with("anger_end_time", 1_000_000i64)
            .with("angry_at", Tag::uuid(grudge)),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    world.ticks(5).await;
    let id = mobs(&region, EntityType::Enderman).await[0].id;
    let nbt = entity_nbt(&region, id).await.unwrap();
    assert_eq!(nbt.get_uuid("angry_at"), Some(grudge));
    let end = nbt.get_long("anger_end_time").unwrap();
    assert!((999_990..=1_000_010).contains(&end), "{end}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_summoned_giant_stays_put_without_goals() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Giant,
        Compound::new().with("PersistenceRequired", true),
        [8.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    world.ticks(40).await;
    let giants = mobs(&region, EntityType::Giant).await;
    assert_eq!(giants.len(), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mobs_see_and_path_to_players_beyond_the_neighbour_halo() {
    use crate::world::nearby::{IndexedPlayer, PlayerIndex};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = ChunkPos::new(31, 0);
    let east = [ChunkPos::new(32, 0), ChunkPos::new(33, 0)];
    let west_region = world.region(west.region()).await;
    let east_region = world.region(east[0].region()).await;
    let (viewer, mut viewer_events) = PlayerHandle::new(999);
    subscribe(&west_region, &viewer, &mut viewer_events, &[west]).await;
    subscribe(&east_region, &viewer, &mut viewer_events, &east).await;
    let corridor = |xs: std::ops::RangeInclusive<i32>| {
        let mut blocks = Vec::new();
        for x in xs {
            for z in 2..=6 {
                blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
                blocks.push((BlockPos::new(x, SKY + 4, z), Block::Stone.default_state()));
            }
            for z in [1, 7] {
                for y in SKY + 1..=SKY + 3 {
                    blocks.push((BlockPos::new(x, y, z), Block::Stone.default_state()));
                }
            }
        }
        blocks
    };
    place_blocks(&world, &west_region, corridor(500..=511)).await;
    place_blocks(&world, &east_region, corridor(512..=535)).await;
    let (player, mut events) = PlayerHandle::new(1000);
    let position = [531.5, SKY as f64 + 1.0, 4.5];
    let overworld = world.dimension(OVERWORLD);
    world.players.send_replace(Arc::new(PlayerIndex::build([(
        overworld,
        IndexedPlayer {
            handle: player.clone(),
            uuid: 77,
            name: Arc::from("Tester"),
            position,
            looks: PlayerLooks::default(),
            time_since_rest: 0,
        },
    )])));
    summon(
        &west_region,
        EntityType::Zombie,
        Compound::new(),
        [506.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    let hit = next_hit(&world, &mut events, 300).await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Zombie));
    world.shutdown().await;
}
