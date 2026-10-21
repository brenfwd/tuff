use super::*;
use crate::world::messages::{PlayerLooks, RegionInternal};
use tuff_vanilla_data::registry::EntityType;

fn perched() -> PlayerLooks {
    PlayerLooks {
        perchable: true,
        invulnerable: true,
        ..Default::default()
    }
}

async fn tick_until<T>(
    world: &TestWorld,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    ticks: u32,
    mut matches: impl FnMut(PlayerEvent) -> Option<T>,
) -> Option<T> {
    for _ in 0..ticks {
        while let Ok(event) = events.try_recv() {
            if let Some(found) = matches(event) {
                return Some(found);
            }
        }
        world.ticks(1).await;
    }
    None
}

async fn offered(
    world: &TestWorld,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    ticks: u32,
) -> Option<(i32, u64, RegionHandle)> {
    tick_until(world, events, ticks, |event| match event {
        PlayerEvent::OfferShoulder {
            entity,
            offer,
            region,
        } => Some((entity, offer, region)),
        _ => None,
    })
    .await
}

/// Keeps the owner standing inside the (wandering) parrot until it asks.
async fn meet(
    world: &TestWorld,
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    looks: PlayerLooks,
) -> Option<(i32, u64, RegionHandle)> {
    for _ in 0..200 {
        let parrots = mobs(region, EntityType::Parrot).await;
        let parrot = parrots.last()?;
        let [x, y, z] = parrot.position;
        region
            .request(player_at(player, [x, y - 0.5, z], looks))
            .await;
        if let Some(found) = offered(world, events, 1).await {
            return Some(found);
        }
    }
    None
}

async fn granted(
    world: &TestWorld,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
) -> (u64, Option<Box<Compound>>) {
    tick_until(world, events, 20, |event| match event {
        PlayerEvent::ShoulderGranted { offer, entity } => Some((offer, entity)),
        _ => None,
    })
    .await
    .expect("the region settles every answer")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tamed_parrot_touching_its_owner_leaves_the_world_for_a_shoulder() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(200);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let at = [8.5, SKY as f64 + 1.0, 8.5];
    let nbt = Compound::new()
        .with("Owner", Tag::uuid(77))
        .with("Variant", 2)
        .with("NoAI", false);
    summon(&region, EntityType::Parrot, nbt, at).await;
    let (entity, offer, from) = meet(&world, &region, &player, &mut events, perched())
        .await
        .expect("the parrot asks");
    from.internal(RegionInternal::ShoulderTaken {
        entity,
        offer,
        player: player.clone(),
        accepted: false,
    });
    let (settled, saved) = granted(&world, &mut events).await;
    assert_eq!(settled, offer);
    assert!(saved.is_none(), "a refused offer leaves the parrot be");
    assert_eq!(mobs(&region, EntityType::Parrot).await.len(), 1);

    let (entity, offer, from) = meet(&world, &region, &player, &mut events, perched())
        .await
        .expect("the parrot asks again");
    from.internal(RegionInternal::ShoulderTaken {
        entity,
        offer,
        player: player.clone(),
        accepted: true,
    });
    let (settled, saved) = granted(&world, &mut events).await;
    assert_eq!(settled, offer);
    let saved = saved.expect("an accepted offer hands the parrot over");
    assert_eq!(saved.get_str("id"), Some("minecraft:parrot"));
    assert_eq!(saved.get_int("Variant"), Some(2));
    world.ticks(1).await;
    assert!(mobs(&region, EntityType::Parrot).await.is_empty());

    region
        .request(RegionRequest::Summon {
            nbt: *saved,
            position: at,
            fresh: false,
            reply: tokio::sync::oneshot::channel().0,
        })
        .await;
    world.ticks(1).await;
    assert_eq!(
        mobs(&region, EntityType::Parrot).await.len(),
        1,
        "the saved parrot comes back with its uuid free"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parrot_waits_for_a_perchable_owner_and_never_asks_while_sitting() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(200);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let at = [8.5, SKY as f64 + 1.0, 8.5];
    let sitting = Compound::new()
        .with("Owner", Tag::uuid(77))
        .with("Sitting", true);
    summon(&region, EntityType::Parrot, sitting, at).await;
    region.request(player_at(&player, at, perched())).await;
    assert!(
        offered(&world, &mut events, 150).await.is_none(),
        "a sitting parrot stays put"
    );

    summon(
        &region,
        EntityType::Parrot,
        Compound::new().with("Owner", Tag::uuid(77)),
        at,
    )
    .await;
    assert!(
        meet(
            &world,
            &region,
            &player,
            &mut events,
            PlayerLooks::default()
        )
        .await
        .is_none(),
        "no offer while the owner flies, swims or is in powder snow"
    );
    assert!(
        meet(&world, &region, &player, &mut events, perched())
            .await
            .is_some()
    );
    world.shutdown().await;
}
