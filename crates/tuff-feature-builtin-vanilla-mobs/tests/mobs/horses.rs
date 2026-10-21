//! The horse family in regions: skeleton traps, mount inventories,
//! cross-breeding and llama spit.

use super::*;
use crate::world::entity::EntityAttack;
use crate::world::menu::{MenuKind, MountArmor};
use crate::world::messages::PlayerLooks;
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType;

async fn entity_data(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("the entity exists here")
}

fn tamed() -> Compound {
    Compound::new()
        .with("Owner", Tag::uuid(77))
        .with("Tame", true)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_skeleton_trap_springs_four_tamed_horsemen_when_a_player_comes_near() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(300);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let trap = [4.5, SKY as f64 + 1.0, 4.5];
    summon(
        &region,
        EntityType::SkeletonHorse,
        Compound::new().with("SkeletonTrap", true),
        trap,
    )
    .await;
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 40.5],
            PlayerLooks::default(),
        ))
        .await;
    world.ticks(20).await;
    assert_eq!(mobs(&region, EntityType::SkeletonHorse).await.len(), 1);
    assert!(mobs(&region, EntityType::Skeleton).await.is_empty());

    region
        .request(player_at(
            &player,
            [10.5, SKY as f64 + 1.0, 10.5],
            PlayerLooks::default(),
        ))
        .await;
    let found = wait_for(&world, &region, 120, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Skeleton)
            .count()
            == 4
    })
    .await;
    let horses: Vec<_> = found
        .iter()
        .filter(|s| s.kind == EntityType::SkeletonHorse)
        .collect();
    assert_eq!(horses.len(), 4, "three more horses join the trap one");
    for horse in horses {
        let data = entity_data(&region, horse.id).await;
        assert_eq!(data.get("Tame"), Some(&Tag::Byte(1)), "{data:?}");
        assert_eq!(data.get("SkeletonTrap"), Some(&Tag::Byte(0)));
    }
    world.shutdown().await;
    let saved = stored_entities(dir.path(), chunk);
    let horses: Vec<_> = saved
        .iter()
        .filter(|e| e.get_str("id") == Some("minecraft:skeleton_horse"))
        .collect();
    assert_eq!(horses.len(), 4);
    for horse in horses {
        let riders = horse.get_list("Passengers").expect("each carries a rider");
        assert_eq!(riders.len(), 1);
        let rider = riders[0].as_compound().unwrap();
        assert_eq!(rider.get_str("id"), Some("minecraft:skeleton"));
        assert!(
            rider
                .get("equipment")
                .is_some_and(|e| format!("{e:?}").contains("helmet")),
            "horsemen wear helmets: {rider:?}"
        );
    }
    assert!(
        saved
            .iter()
            .all(|e| e.get_str("id") != Some("minecraft:skeleton")),
        "every skeleton rides"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unsprung_trap_horse_vanishes_after_its_time_runs_out() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(301);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let old_trap = Compound::new()
        .with("SkeletonTrap", true)
        .with("SkeletonTrapTime", 17_998);
    summon(
        &region,
        EntityType::SkeletonHorse,
        old_trap.clone(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    summon(
        &region,
        EntityType::SkeletonHorse,
        old_trap.with("PersistenceRequired", true),
        [9.5, SKY as f64 + 1.0, 9.5],
    )
    .await;
    world.ticks(10).await;
    let left = mobs(&region, EntityType::SkeletonHorse).await;
    assert_eq!(left.len(), 1, "only the persistent trap horse stays");
    assert!(left[0].position[0] > 7.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sneaking_owner_opens_a_chested_donkeys_mount_screen() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(302);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let diamond = ItemStack::mint(Item::Diamond, 2, MintReason::Test);
    let nbt = tamed().with("ChestedHorse", true).with(
        "Items",
        Tag::List(vec![Tag::Compound(diamond.to_nbt().with("Slot", 3i8))]),
    );
    summon(
        &region,
        EntityType::Donkey,
        nbt,
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let donkey = mobs(&region, EntityType::Donkey).await[0].id;

    let mut sneaking = interaction(&player, None);
    sneaking.sneaking = true;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: donkey,
            interaction: sneaking,
        })
        .await;
    let open = expect(&mut events, |event| match event {
        PlayerEvent::MenuOpen(open) => Some(open),
        _ => None,
    })
    .await;
    assert_eq!(
        open.kind,
        MenuKind::Mount {
            columns: 5,
            saddle: true,
            armor: MountArmor::None,
        }
    );
    let MenuSource::Entity { entity, contents } = open.source else {
        panic!("not an entity menu");
    };
    assert_eq!(entity, donkey);
    assert_eq!(contents.len(), 2 + 15);
    assert_eq!(
        contents[2 + 3],
        Some(diamond),
        "chest slots follow saddle and body"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_horse_and_a_donkey_in_love_have_a_mule() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(303);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let in_love = || tamed().with("InLove", 600);
    summon(
        &region,
        EntityType::Horse,
        in_love(),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    summon(
        &region,
        EntityType::Donkey,
        in_love(),
        [8.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    wait_for(&world, &region, 400, |found| {
        found.iter().any(|s| s.kind == EntityType::Mule)
    })
    .await;
    let mule = mobs(&region, EntityType::Mule).await[0].id;
    let data = entity_data(&region, mule).await;
    let Some(Tag::Int(age)) = data.get("Age") else {
        panic!("the foal saves its age: {data:?}");
    };
    assert!(*age < 0, "a baby mule");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hurt_llama_spits_back_at_its_attacker() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(304);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Llama,
        Compound::new(),
        [2.5, SKY as f64 + 1.0, 2.5],
    )
    .await;
    let llama = mobs(&region, EntityType::Llama).await[0].id;
    let at = [13.5, SKY as f64 + 1.0, 13.5];
    region
        .request(player_at(&player, at, PlayerLooks::default()))
        .await;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: llama,
            attack: EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: at,
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
    let llama_uuid = mobs(&region, EntityType::Llama).await[0].uuid;
    let mut spits = HashSet::new();
    let mut heading = None;
    for _ in 0..200 {
        for spit in mobs(&region, EntityType::LlamaSpit).await {
            if spits.insert(spit.id) {
                let data = entity_data(&region, spit.id).await;
                assert_eq!(data.get("Owner"), Some(&Tag::uuid(llama_uuid)), "{data:?}");
                let Some(Tag::List(motion)) = data.get("Motion") else {
                    panic!("spit saves its motion: {data:?}");
                };
                let axis = |i: usize| match motion[i] {
                    Tag::Double(v) => v,
                    _ => panic!("motion is doubles"),
                };
                let to_player = [at[0] - spit.position[0], at[2] - spit.position[2]];
                let length = (to_player[0].powi(2) + to_player[1].powi(2)).sqrt();
                let speed = (axis(0).powi(2) + axis(2).powi(2)).sqrt();
                heading =
                    Some((axis(0) * to_player[0] + axis(2) * to_player[1]) / (length * speed));
            }
        }
        world.ticks(1).await;
    }
    assert_eq!(spits.len(), 1, "a hurt llama spits back once");
    let heading = heading.unwrap();
    assert!(heading > 0.95, "it spits at its attacker: cos {heading}");
    world.shutdown().await;
}

fn dist2d(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn llamas_line_up_behind_a_leashed_llama() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(305);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 14)).await;
    let floor = SKY as f64 + 1.0;
    region
        .request(player_at(
            &player,
            [2.5, floor, 2.5],
            PlayerLooks::default(),
        ))
        .await;
    let held = Compound::new().with("leash", Compound::new().with("UUID", Tag::uuid(77)));
    summon(&region, EntityType::Llama, held, [5.5, floor, 5.5]).await;
    summon(
        &region,
        EntityType::TraderLlama,
        Compound::new(),
        [11.5, floor, 11.5],
    )
    .await;
    summon(
        &region,
        EntityType::Llama,
        Compound::new(),
        [11.5, floor, 3.5],
    )
    .await;
    world.ticks(300).await;
    let head = mobs(&region, EntityType::Llama)
        .await
        .into_iter()
        .find(|s| s.position[2] < 8.0 && s.position[0] < 9.0)
        .expect("the leashed llama");
    let followers = [
        mobs(&region, EntityType::TraderLlama).await[0].position,
        mobs(&region, EntityType::Llama)
            .await
            .into_iter()
            .find(|s| s.id != head.id)
            .unwrap()
            .position,
    ];
    let close = followers
        .iter()
        .filter(|p| dist2d(**p, head.position) < 6.0)
        .count();
    assert!(
        close >= 1,
        "a follower trails the head: {followers:?} {head:?}"
    );
    world.shutdown().await;
}
