//! Nether mobs in regions: ghasts and their fireballs, slimes and magma
//! cubes.

use super::*;
use crate::world::messages::{EntitySnapshot, PlayerLooks};

/// A stone floor at `SKY` over `x0..=x1` and `z0..=z1`, open on every side.
fn floor(x0: i32, x1: i32, z0: i32, z1: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in x0..=x1 {
        for z in z0..=z1 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
        }
    }
    blocks
}
use tuff_vanilla_data::registry::EntityType;

async fn attack_entity(
    region: &RegionHandle,
    player: &PlayerHandle,
    target: i32,
    attack: crate::world::entity::EntityAttack,
) {
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target,
            attack,
            sweep: None,
        })
        .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ghasts_shoot_exploding_fireballs_at_players() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk, ChunkPos::new(0, 1)]).await;
    place_blocks(&world, &region, floor(0, 15, 0, 31)).await;
    summon(
        &region,
        EntityType::Ghast,
        Compound::new(),
        [8.0, SKY as f64 + 3.0, 22.0],
    )
    .await;
    let position = [8.0, SKY as f64 + 1.0, 4.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let mut direct = None;
    let blast = ticking_expect(&world, &mut events, 1200, |event| match event {
        PlayerEvent::EntityHurt(hit) if hit.projectile == Some(EntityType::Fireball) => {
            direct = Some(hit);
            None
        }
        PlayerEvent::Exploded { blast, .. } => Some(blast),
        _ => None,
    })
    .await;
    assert_eq!(blast.damage_type.name, "minecraft:player_explosion");
    assert_eq!(blast.culprit.map(|c| c.kind), Some(EntityType::Ghast));
    if let Some(hit) = direct {
        assert_eq!(hit.kind.name, "minecraft:fireball");
        assert_eq!(hit.amount, 6.0);
        assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Ghast));
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fireball_hit_back_by_a_player_kills_the_ghast() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, floor(2, 13, 2, 30)).await;
    summon(
        &region,
        EntityType::Ghast,
        Compound::new().with("NoAI", true),
        [8.0, SKY as f64 + 1.0, 20.0],
    )
    .await;
    summon(
        &region,
        EntityType::Fireball,
        Compound::new(),
        [8.0, SKY as f64 + 2.0, 6.0],
    )
    .await;
    let position = [8.0, SKY as f64 + 1.0, 4.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    world.ticks(20).await;
    assert_eq!(mobs(&region, EntityType::Ghast).await.len(), 1);
    let fireball = mobs(&region, EntityType::Fireball).await[0].id;
    attack_entity(&region, &player, fireball, attack(0.0, None)).await;
    let found = wait_for(&world, &region, 200, |found| {
        !found.iter().any(|s| s.kind == EntityType::Ghast)
    })
    .await;
    assert!(
        found.iter().any(|s| s.kind == EntityType::ExperienceOrb),
        "the player gets the kill: {found:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn big_slimes_hurt_players_they_touch_and_split_when_killed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Slime,
        Compound::new().with("Size", 3),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let slime = &mobs(&region, EntityType::Slime).await[0];
    assert!((slime.bounds[3] - slime.bounds[0] - 0.52 * 4.0).abs() < 1e-4);
    let position = [9.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 400).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    assert_eq!(hit.amount, 4.0, "a size 4 slime's damage");
    let id = mobs(&region, EntityType::Slime).await[0].id;
    attack_entity(&region, &player, id, attack(100.0, None)).await;
    let found = wait_for(&world, &region, 100, |found| {
        found.iter().filter(|s| s.kind == EntityType::Slime).count() >= 2
    })
    .await;
    for child in found.iter().filter(|s| s.kind == EntityType::Slime) {
        let width = child.bounds[3] - child.bounds[0];
        assert!((width - 0.52 * 2.0).abs() < 1e-4, "half size: {width}");
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tiny_slimes_are_harmless_but_tiny_magma_cubes_are_not() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Slime,
        Compound::new().with("Size", 0),
        [8.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let position = [8.5, SKY as f64 + 1.0, 8.2];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    world.ticks(100).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "tiny slimes do no harm"
        );
    }
    summon(
        &region,
        EntityType::MagmaCube,
        Compound::new().with("Size", 0),
        [8.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    let hit = next_hit(&world, &mut events, 400).await;
    assert_eq!(hit.amount, 3.0, "a tiny magma cube's damage plus two");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hoglins_and_piglins_zombify_outside_the_nether_only() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let nether = world
        .region_in(crate::storage::level::THE_NETHER, chunk.region())
        .await;
    let overworld = world.region(chunk.region()).await;
    for region in [&overworld, &nether] {
        let (player, mut events) = PlayerHandle::new(1000);
        subscribe(region, &player, &mut events, &[chunk]).await;
        place_blocks(&world, region, pen(2, 13)).await;
        summon(
            region,
            EntityType::Hoglin,
            Compound::new().with("NoAI", false).with("Age", -24000),
            [4.5, SKY as f64 + 1.0, 4.5],
        )
        .await;
        let sword = ItemStack::mint(Item::GoldenSword, 1, MintReason::Test).to_nbt();
        summon(
            region,
            EntityType::Piglin,
            Compound::new().with("equipment", Compound::new().with("mainhand", sword)),
            [11.5, SKY as f64 + 1.0, 11.5],
        )
        .await;
    }
    world.ticks(310).await;
    let kinds = |found: Vec<crate::world::messages::EntitySnapshot>| {
        let mut kinds: Vec<EntityType> = found
            .iter()
            .map(|s| s.kind)
            .filter(|k| *k != EntityType::Item)
            .collect();
        kinds.sort_by_key(|k| k.name());
        kinds
    };
    assert_eq!(
        kinds(snapshots(&nether).await),
        [EntityType::Hoglin, EntityType::Piglin]
    );
    assert_eq!(
        kinds(snapshots(&overworld).await),
        [EntityType::Zoglin, EntityType::ZombifiedPiglin]
    );
    for converted in snapshots(&overworld).await {
        if converted.kind == EntityType::Item {
            continue;
        }
        let (reply, answer) = oneshot::channel();
        overworld
            .request(RegionRequest::EntityData {
                id: converted.id,
                reply,
            })
            .await;
        let nbt = answer.await.unwrap().unwrap();
        assert!(
            effect_ids(&nbt).contains(&"minecraft:nausea".to_owned()),
            "{:?} is nauseous",
            converted.kind
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hoglins_throw_players_but_keep_away_from_warped_fungus() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world
        .region_in(crate::storage::level::THE_NETHER, chunk.region())
        .await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Hoglin,
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
    assert!((3.0..9.0).contains(&hit.amount), "{}", hit.amount);
    assert!(hit.knockback.is_none());
    let push = hit.push.expect("thrown");
    assert!(push[1] >= 0.0 && push[0].abs() + push[2].abs() > 0.0);
    place_blocks(
        &world,
        &region,
        [(
            BlockPos::new(8, SKY + 1, 8),
            Block::WarpedFungus.default_state(),
        )],
    )
    .await;
    world.ticks(40).await;
    while events.try_recv().is_ok() {}
    world.ticks(150).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "warped fungus calms it"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hurt_baby_hoglin_flees_instead_of_striking_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world
        .region_in(crate::storage::level::THE_NETHER, chunk.region())
        .await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Hoglin,
        Compound::new().with("Age", -24000),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [8.5, SKY as f64 + 1.0, 8.5],
            PlayerLooks::default(),
        ))
        .await;
    world.ticks(2).await;
    let baby = mobs(&region, EntityType::Hoglin).await[0].id;
    attack_entity(&region, &player, baby, attack(0.5, None)).await;
    while events.try_recv().is_ok() {}
    world.ticks(90).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "a hurt baby hoglin retreats"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hoglins_breed_on_crimson_fungus() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world
        .region_in(crate::storage::level::THE_NETHER, chunk.region())
        .await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    for x in [6.5, 8.5] {
        summon(
            &region,
            EntityType::Hoglin,
            Compound::new(),
            [x, SKY as f64 + 1.0, 7.5],
        )
        .await;
    }
    let creative = PlayerLooks {
        creative: true,
        invulnerable: true,
        ..Default::default()
    };
    region
        .request(player_at(&player, [7.5, SKY as f64 + 1.0, 10.5], creative))
        .await;
    for hoglin in mobs(&region, EntityType::Hoglin).await {
        let effect = interact(
            &region,
            &player,
            &mut events,
            hoglin.id,
            Item::CrimsonFungus,
        )
        .await;
        assert!(matches!(
            effect,
            crate::world::messages::HandEffect::Consume(1)
        ));
    }
    wait_for(&world, &region, 400, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Hoglin)
            .count()
            == 3
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wither_skeletons_hit_with_stone_swords_that_wither() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world
        .region_in(crate::storage::level::THE_NETHER, chunk.region())
        .await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon_fresh(
        &region,
        EntityType::WitherSkeleton,
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let position = [12.5, SKY as f64 + 1.0, 12.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    assert_eq!(
        hit.weapon.map(|w| w.item()),
        Some(Item::StoneSword),
        "armed with a stone sword"
    );
    assert!(
        hit.amount >= 4.0,
        "base damage 4 plus the sword: {}",
        hit.amount
    );
    assert_eq!(
        hit.effects
            .iter()
            .map(|e| (e.effect, e.duration))
            .collect::<Vec<_>>(),
        [(tuff_vanilla_data::registry::MobEffect::Wither, 200)]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn striders_walk_to_lava_and_take_no_damage_standing_in_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = floor(0, 15, 0, 15);
    for x in 10..=13 {
        for z in 10..=13 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Lava.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    region
        .request(player_at(
            &player,
            [1.5, SKY as f64 + 1.0, 14.5],
            PlayerLooks::default(),
        ))
        .await;
    summon_fresh(&region, EntityType::Strider, [2.5, SKY as f64 + 1.0, 2.5]).await;
    let on_lava = |found: &[EntitySnapshot]| {
        found.iter().any(|s| {
            s.kind == EntityType::Strider
                && (10.0..=14.0).contains(&s.position[0])
                && (10.0..=14.0).contains(&s.position[2])
        })
    };
    wait_for(&world, &region, 600, on_lava).await;
    world.ticks(100).await;
    let strider = mobs(&region, EntityType::Strider).await;
    assert_eq!(strider.len(), 1, "the strider survives standing in lava");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn striders_can_be_saddled() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, floor(0, 10, 0, 10)).await;
    summon_fresh(&region, EntityType::Strider, [5.5, SKY as f64 + 1.0, 5.5]).await;
    let strider = mobs(&region, EntityType::Strider).await[0].id;
    let effect = interact(&region, &player, &mut events, strider, Item::Saddle).await;
    assert!(matches!(effect, crate::world::messages::HandEffect::None));
    world.shutdown().await;
}

fn heard_slime_attack(event: &PlayerEvent) -> bool {
    let attack = tuff_vanilla_data::registry::SoundEvent::EntitySlimeAttack;
    let PlayerEvent::ChunkPackets { packets, .. } = event else {
        return false;
    };
    packets.iter().any(|frame| {
        let mut r = framed_payload(frame);
        r.read_var_int().unwrap() == tuff_vanilla_data::packet::play::Clientbound::Sound.id()
            && r.read_var_int().unwrap() == attack.id() as i32 + 1
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slimes_only_make_their_attack_sound_when_a_hit_landed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Slime,
        Compound::new().with("Size", 3),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [9.5, SKY as f64 + 1.0, 7.5],
            PlayerLooks::default(),
        ))
        .await;
    next_hit(&world, &mut events, 400).await;
    world.ticks(5).await;
    while let Ok(event) = events.try_recv() {
        assert!(!heard_slime_attack(&event), "no landed hit was reported");
    }
    let slime = mobs(&region, EntityType::Slime).await[0].id;
    region
        .request(RegionRequest::Notify {
            target: slime,
            notice: crate::world::entity::Notice::HitLanded,
        })
        .await;
    ticking_expect(&world, &mut events, 5, |event| {
        heard_slime_attack(&event).then_some(())
    })
    .await;
    world.shutdown().await;
}
