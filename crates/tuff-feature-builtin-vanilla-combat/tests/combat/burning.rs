//! Burning: rain putting fire out, the player's grace in fire blocks after
//! burning, things in lava and mobs in fire blocks.

use super::*;
use crate::world::block_contact::Contact;
use crate::world::session::{FIRE_IMMUNE_TICKS, PlayerSession};
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

fn session(world: &TestWorld, id: i32) -> PlayerSession {
    let (session, outbound) = session_with_outbound(world, id);
    std::mem::forget(outbound);
    session
}

fn surroundings(player: &PlayerSession, in_fire: Option<f32>, in_rain: bool) -> PlayerEvent {
    PlayerEvent::Surroundings {
        dimension: player.dimension_id(),
        seq: 0,
        in_water: false,
        submersion: Default::default(),
        in_lava: false,
        in_fire,
        climbable: false,
        climb_location: None,
        resets_fall: false,
        landed_on: None,
        portal: None,
        biome: None,
        stepping_on: None,
        contact: Contact {
            in_rain,
            ..Contact::default()
        },
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rain_puts_a_burning_player_out_and_fire_blocks_then_wait_a_second_to_light_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    player.ignite(8.0);
    player.run_tick().await;
    assert_eq!(player.fire_ticks(), 159);
    player.handle_event(surroundings(&player, None, true)).await;
    player.run_tick().await;
    assert_eq!(player.fire_ticks(), -FIRE_IMMUNE_TICKS, "rain put it out");

    player
        .handle_event(surroundings(&player, Some(1.0), false))
        .await;
    for _ in 0..FIRE_IMMUNE_TICKS - 1 {
        player.run_tick().await;
    }
    assert_eq!(player.fire_ticks(), -1, "fire blocks have not lit it yet");
    assert!(player.health() < 20.0, "but they hurt it all along");
    player.run_tick().await;
    assert_eq!(player.fire_ticks(), 160, "now it catches");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_out_of_fire_counts_its_grace_from_the_start_again() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    player.run_tick().await;
    assert_eq!(player.fire_ticks(), -FIRE_IMMUNE_TICKS);
    player
        .handle_event(surroundings(&player, Some(1.0), false))
        .await;
    for _ in 0..10 {
        player.run_tick().await;
    }
    assert_eq!(player.fire_ticks(), -FIRE_IMMUNE_TICKS + 10);
    player
        .handle_event(surroundings(&player, None, false))
        .await;
    player.run_tick().await;
    assert_eq!(player.fire_ticks(), -FIRE_IMMUNE_TICKS);
    world.shutdown().await;
}

async fn entity_nbt(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("entity exists")
}

fn fire_of(nbt: &Compound) -> i16 {
    match nbt.get("Fire") {
        Some(Tag::Short(fire)) => *fire,
        other => panic!("no Fire: {other:?}"),
    }
}

fn health_of(nbt: &Compound) -> f32 {
    match nbt.get("Health") {
        Some(Tag::Float(health)) => *health,
        other => panic!("no Health: {other:?}"),
    }
}

/// Sounds of `sound` in the chunk packets `events` got so far.
fn heard(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, sound: SoundEvent) -> usize {
    let mut count = 0;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            count += packets
                .iter()
                .filter_map(decode_sound)
                .filter(|s| s.sound_event == sound.id())
                .count();
        }
    }
    count
}

async fn burning_world() -> (
    TestDir,
    TestWorld,
    RegionHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    view_around(&world, &player, &mut events, chunk).await;
    std::mem::forget(player);
    (dir, world, region, events)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rain_puts_out_a_burning_mob_under_the_sky_but_not_under_a_roof() {
    let (_dir, world, region, mut events) = burning_world().await;
    let mut blocks = pen(2, 13);
    for x in 9..=13 {
        for z in 2..=13 {
            blocks.push((BlockPos::new(x, SKY + 4, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let burning = || Compound::new().with("NoAI", true).with("Fire", 400i16);
    summon(
        &region,
        EntityType::Pig,
        burning(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    summon(
        &region,
        EntityType::Pig,
        burning(),
        [11.5, SKY as f64 + 1.0, 11.5],
    )
    .await;
    let pigs = mobs(&region, EntityType::Pig).await;
    let (open, roofed) = if pigs[0].position[0] < 8.0 {
        (pigs[0].id, pigs[1].id)
    } else {
        (pigs[1].id, pigs[0].id)
    };
    world.ticks(5).await;
    heard(&mut events, SoundEvent::EntityGenericExtinguishFire);
    world.weather.send_modify(|weather| {
        weather.raining = true;
        weather.rain_level = 1.0;
    });
    world.ticks(5).await;
    assert!(
        fire_of(&entity_nbt(&region, open).await) <= 0,
        "rain put it out"
    );
    assert!(
        fire_of(&entity_nbt(&region, roofed).await) > 0,
        "the roof kept rain off"
    );
    assert_eq!(
        heard(&mut events, SoundEvent::EntityGenericExtinguishFire),
        1
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_in_a_fire_block_is_hurt_and_set_alight() {
    let (_dir, world, region, _events) = burning_world().await;
    let mut blocks = pen(2, 13);
    blocks.push((BlockPos::new(6, SKY + 1, 6), Block::Fire.default_state()));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let pig = mobs(&region, EntityType::Pig).await[0].id;
    world.ticks(3).await;
    let nbt = entity_nbt(&region, pig).await;
    assert!(health_of(&nbt) < 10.0, "{nbt:?}");
    assert!(fire_of(&nbt) > 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn things_burning_in_lava_sizzle() {
    let (_dir, world, region, mut events) = burning_world().await;
    let mut blocks = pen(2, 13);
    blocks.push((BlockPos::new(6, SKY + 1, 6), Block::Lava.default_state()));
    blocks.push((BlockPos::new(9, SKY + 1, 9), Block::Lava.default_state()));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    world.ticks(3).await;
    assert!(
        heard(&mut events, SoundEvent::EntityGenericBurn) >= 1,
        "the pig sizzles"
    );
    summon(
        &region,
        EntityType::Item,
        Compound::new().with(
            "Item",
            tuff_item::item::ItemPicture::new(Item::Dirt, 1).to_nbt(),
        ),
        [9.5, SKY as f64 + 1.0, 9.5],
    )
    .await;
    heard(&mut events, SoundEvent::EntityGenericBurn);
    world.ticks(3).await;
    assert!(
        heard(&mut events, SoundEvent::EntityGenericBurn) >= 1,
        "the item sizzles"
    );
    world.shutdown().await;
}
