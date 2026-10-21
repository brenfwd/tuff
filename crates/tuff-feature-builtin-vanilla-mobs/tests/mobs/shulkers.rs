//! Shulkers in regions: clinging to block faces, saving their face and
//! colour, teleporting off a broken block, shooting levitating bullets,
//! splitting when their own bullets hit them and dropping shells.

use super::*;
use crate::world::messages::PlayerLooks;
use tuff_core::world::entity::take_number;
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::{EntityType, MobEffect};

async fn entity_nbt(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("the entity is still there")
}

fn number(nbt: &Compound, key: &str) -> f64 {
    take_number(&mut nbt.clone(), key).unwrap_or_else(|| panic!("{key} is saved"))
}

const EAST: f64 = 5.0;

async fn start(
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
    place_blocks(world, &region, pen(2, 13)).await;
    (region, player, events)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shulkers_cling_to_a_wall_and_keep_their_face_and_colour_when_saved() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, _player, _events) = start(&world).await;
    let y = SKY + 3;
    place_blocks(
        &world,
        &region,
        [(BlockPos::new(9, y, 6), Block::Stone.default_state())],
    )
    .await;
    summon(
        &region,
        EntityType::Shulker,
        Compound::new().with("Color", 3i8),
        [8.2, f64::from(y) + 0.3, 6.7],
    )
    .await;
    world.ticks(3).await;
    let shulker = &mobs(&region, EntityType::Shulker).await[0];
    assert_eq!(
        shulker.position,
        [8.5, f64::from(y), 6.5],
        "snapped to its block"
    );
    let saved = entity_nbt(&region, shulker.id).await;
    assert_eq!(number(&saved, "AttachFace"), EAST, "it clings to the wall");
    assert_eq!(number(&saved, "Color"), 3.0);
    assert_eq!(number(&saved, "Peek"), 0.0);

    let mut copy = saved.clone();
    copy.remove("UUID");
    copy.remove("Pos");
    summon(
        &region,
        EntityType::Shulker,
        copy,
        [13.5, SKY as f64 + 1.0, 12.5],
    )
    .await;
    world.ticks(3).await;
    let loaded = mobs(&region, EntityType::Shulker)
        .await
        .into_iter()
        .find(|s| s.id != shulker.id)
        .expect("the copy loaded");
    let reloaded = entity_nbt(&region, loaded.id).await;
    assert_eq!(
        number(&reloaded, "AttachFace"),
        EAST,
        "its saved face still holds on the wall beside the floor"
    );
    assert_eq!(number(&reloaded, "Color"), 3.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shulker_teleports_when_its_block_is_broken() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, _player, _events) = start(&world).await;
    let pillar = BlockPos::new(8, SKY + 5, 8);
    place_blocks(&world, &region, [(pillar, Block::Stone.default_state())]).await;
    summon(
        &region,
        EntityType::Shulker,
        Compound::new(),
        [8.5, f64::from(SKY + 6), 8.5],
    )
    .await;
    world.ticks(3).await;
    let before = mobs(&region, EntityType::Shulker).await[0].clone();
    assert_eq!(before.position, [8.5, f64::from(SKY + 6), 8.5]);
    place_blocks(&world, &region, [(pillar, Block::Air.default_state())]).await;
    let found = wait_for(&world, &region, 20, |found| {
        found
            .iter()
            .any(|s| s.id == before.id && s.position != before.position)
    })
    .await;
    let after = found.iter().find(|s| s.id == before.id).unwrap();
    for axis in 0..3 {
        assert!(
            (after.position[axis] - before.position[axis]).abs() <= 8.0,
            "within eight blocks: {:?}",
            after.position
        );
    }
    let face = number(&entity_nbt(&region, after.id).await, "AttachFace") as usize;
    let (dx, dy, dz) = tuff_vanilla_data::block::Facing::ALL
        .into_iter()
        .find(|f| f.id() == face)
        .unwrap()
        .step();
    let here = BlockPos::new(
        after.position[0].floor() as i32,
        after.position[1].floor() as i32,
        after.position[2].floor() as i32,
    );
    let held = block_at(
        &region,
        BlockPos::new(here.x + dx, here.y + dy, here.z + dz),
    )
    .await
    .unwrap();
    assert!(
        held.is_collision_face_full(
            tuff_vanilla_data::block::Facing::ALL
                .into_iter()
                .find(|f| f.id() == face)
                .unwrap()
                .opposite()
        ),
        "it clings to a solid face: {held:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shulker_bullets_hurt_and_levitate_players() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, player, mut events) = start(&world).await;
    summon(
        &region,
        EntityType::Shulker,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [11.5, SKY as f64 + 1.0, 5.5],
            PlayerLooks::default(),
        ))
        .await;
    let hit = ticking_expect(&world, &mut events, 600, |event| match event {
        PlayerEvent::EntityHurt(hit) if hit.projectile == Some(EntityType::ShulkerBullet) => {
            Some(hit)
        }
        _ => None,
    })
    .await;
    assert_eq!(hit.kind.name, "minecraft:mob_projectile");
    assert_eq!(hit.amount, 4.0);
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Shulker));
    let levitation = hit
        .effects
        .iter()
        .find(|e| e.effect == MobEffect::Levitation)
        .expect("the bullet levitates");
    assert_eq!(levitation.duration, 200);
    assert_eq!(levitation.amplifier, 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_shulker_hit_by_a_bullet_splits() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, _player, _events) = start(&world).await;
    summon(
        &region,
        EntityType::Shulker,
        Compound::new().with("Peek", 100i8).with("Color", 9i8),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(2).await;
    let shulker = mobs(&region, EntityType::Shulker).await[0].clone();
    summon(
        &region,
        EntityType::ShulkerBullet,
        Compound::new()
            .with("Target", Tag::uuid(shulker.uuid))
            .with("Dir", 0i8)
            .with("Steps", 30)
            .with("TYD", -0.15),
        [7.5, SKY as f64 + 3.2, 7.5],
    )
    .await;
    let found = wait_for(&world, &region, 100, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Shulker)
            .count()
            == 2
    })
    .await;
    let shulkers: Vec<_> = found
        .iter()
        .filter(|s| s.kind == EntityType::Shulker)
        .collect();
    let copy = shulkers.iter().find(|s| s.id != shulker.id).unwrap();
    assert_eq!(copy.position, shulker.position, "the copy stays behind");
    let moved = shulkers.iter().find(|s| s.id == shulker.id).unwrap();
    assert_ne!(moved.position, shulker.position, "the original teleported");
    assert_eq!(number(&entity_nbt(&region, copy.id).await, "Color"), 9.0);
    assert!(
        !found.iter().any(|s| s.kind == EntityType::ShulkerBullet),
        "the bullet is gone"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_shulkers_drop_shells() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, viewer, mut events) = start(&world).await;
    for x in [3, 5, 7, 9, 11] {
        for z in [3, 6, 9] {
            summon(
                &region,
                EntityType::Shulker,
                Compound::new(),
                [f64::from(x) + 0.5, SKY as f64 + 1.0, f64::from(z) + 0.5],
            )
            .await;
        }
    }
    world.ticks(2).await;
    for shulker in mobs(&region, EntityType::Shulker).await {
        region
            .request(RegionRequest::AttackEntity {
                attacker: viewer.clone(),
                target: shulker.id,
                attack: attack(100.0, None),
                sweep: None,
            })
            .await;
        expect(&mut events, |event| match event {
            PlayerEvent::AttackLanded { target, .. } if target == shulker.id => Some(()),
            _ => None,
        })
        .await;
    }
    let found = wait_for(&world, &region, 40, |found| {
        !found.iter().any(|s| s.kind == EntityType::Shulker)
    })
    .await;
    let shells = found
        .iter()
        .filter(|s| s.kind == EntityType::Item && s.name == "minecraft:shulker_shell")
        .count();
    assert!(shells > 0, "half of them drop a shell: {found:#?}");
    world.shutdown().await;
}

/// End city sentries are placed as saved shulkers, which load as live
/// mobs clinging to the floor of their spot.
#[tuff_core::watchdog]
#[test]
fn end_city_sentries_load_as_live_shulkers() {
    let registry = tuff_test_support::vanilla_registries().entity_types.clone();
    let sentry = tuff_feature_builtin_vanilla_worldgen::structure::structure_mob(
        "minecraft:shulker",
        BlockPos::new(3, 70, -4),
        false,
    );
    let mut next = 0;
    let (live, raw) = registry.load_all(vec![sentry], || {
        next += 1;
        next
    });
    assert!(raw.is_empty(), "a sentry is not kept raw");
    let mut live = live;
    let shulker =
        live[0]
            .downcast_mut::<tuff_feature_builtin_vanilla_mobs::Mob<
                tuff_feature_builtin_vanilla_mobs::shulker::Shulker,
            >>()
            .expect("a live shulker");
    assert_eq!(shulker.core.base.position, [3.5, 70.0, -3.5]);
    assert_eq!(
        shulker.species.attach_face(),
        tuff_vanilla_data::block::Facing::Down
    );
    assert_eq!(shulker.species.peek(), 0);
    assert_eq!(shulker.species.color(), None);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hit_breaks_a_shulker_bullet() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, viewer, mut events) = start(&world).await;
    summon(
        &region,
        EntityType::ShulkerBullet,
        Compound::new(),
        [7.5, SKY as f64 + 3.0, 7.5],
    )
    .await;
    world.ticks(1).await;
    let bullet = mobs(&region, EntityType::ShulkerBullet).await[0].clone();
    region
        .request(RegionRequest::AttackEntity {
            attacker: viewer.clone(),
            target: bullet.id,
            attack: attack(1.0, None),
            sweep: None,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::AttackLanded { target, .. } if target == bullet.id => Some(()),
        _ => None,
    })
    .await;
    wait_for(&world, &region, 3, |found| {
        !found.iter().any(|s| s.kind == EntityType::ShulkerBullet)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_opening_shell_pushes_a_mob_resting_on_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, _player, _events) = start(&world).await;
    summon(
        &region,
        EntityType::Shulker,
        Compound::new().with("Peek", 100i8),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoGravity", 1i8),
        [7.5, SKY as f64 + 1.5, 7.5],
    )
    .await;
    let rest = SKY as f64 + 1.9;
    wait_for(&world, &region, 60, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Pig && s.position[1] > rest)
    })
    .await;
    world.shutdown().await;
}
