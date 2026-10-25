//! Vehicles meeting mobs and blocks: boats and carts picking up the mobs
//! they bump, and the special minecarts.

use super::*;
use tuff_vanilla_data::registry::EntityType;

/// What the entity of `kind` rides, read on its region.
async fn vehicle_of(region: &RegionHandle, kind: EntityType) -> Option<i32> {
    vehicle_of_at(region, kind, BlockPos::new(8, SKY, 8)).await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boats_pick_up_the_mobs_they_bump_but_not_fish_or_big_ones() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let y = f64::from(SKY) + 1.0;
    summon(&region, EntityType::OakBoat, Compound::new(), [8.5, y, 8.5]).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [9.6, y, 8.5],
    )
    .await;
    summon(
        &region,
        EntityType::Cod,
        Compound::new().with("NoAI", true),
        [8.5, y, 9.6],
    )
    .await;
    world.ticks(3).await;
    let boat = mobs(&region, EntityType::OakBoat).await.remove(0).id;
    assert_eq!(vehicle_of(&region, EntityType::Pig).await, Some(boat));
    assert_eq!(
        vehicle_of(&region, EntityType::Cod).await,
        None,
        "fish stay out"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moving_minecart_picks_up_a_mob_on_its_rail() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let rail = Block::Rail
        .default_state()
        .with_shape(tuff_vanilla_data::block::Shape::EastWest)
        .unwrap();
    place_blocks(
        &world,
        &region,
        (3..=12).map(|x| (BlockPos::new(x, SKY + 1, 8), rail)),
    )
    .await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [9.5, f64::from(SKY) + 1.0, 8.5],
    )
    .await;
    summon(
        &region,
        EntityType::Minecart,
        Compound::new().with(
            "Motion",
            vec![Tag::Double(0.3), Tag::Double(0.0), Tag::Double(0.0)],
        ),
        [6.5, f64::from(SKY) + 1.0625, 8.5],
    )
    .await;
    world.ticks(15).await;
    let cart = mobs(&region, EntityType::Minecart).await.remove(0).id;
    assert_eq!(vehicle_of(&region, EntityType::Pig).await, Some(cart));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rider_set_down_under_a_low_ceiling_crouches_at_once() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let center = ChunkPos::new(40, 40);
    let region = world.region(center.region()).await;
    let (mut session, _frames) = session_with_outbound(&world, 1000);
    let (handle, mut events) = PlayerHandle::new(1000);
    let chunks: Vec<ChunkPos> = (-1..=1)
        .flat_map(|dx| (-1..=1).map(move |dz| ChunkPos::new(center.x + dx, center.z + dz)))
        .collect();
    session.view.subscribed.extend(chunks.iter().copied());
    region
        .request(RegionRequest::Subscribe {
            player: handle.clone(),
            chunks: chunks.clone(),
        })
        .await;
    let (x, z) = (center.x * 16 + 8, center.z * 16 + 8);
    let feet = 200;
    let top_slab: tuff_vanilla_data::BlockState =
        "minecraft:stone_slab[type=top,waterlogged=false]"
            .parse()
            .unwrap();
    let mut waiting = chunks.len();
    while waiting > 0 {
        let event = events.recv().await.expect("region events");
        if matches!(event, PlayerEvent::Chunk { .. }) {
            waiting -= 1;
        }
        session.on_event(event).await;
    }
    while !session.view.queue.is_empty() {
        session.view.unacked_batches = 0;
        session.send_chunk_batch();
    }
    place_blocks(
        &world,
        &region,
        [
            (BlockPos::new(x, feet - 1, z), Block::Stone.default_state()),
            (BlockPos::new(x, feet + 1, z), top_slab),
        ],
    )
    .await;
    while let Ok(event) = events.try_recv() {
        session.on_event(event).await;
    }
    let spot = [f64::from(x) + 0.5, f64::from(feet), f64::from(z) + 0.5];
    assert_eq!(
        session
            .client_world()
            .block_at(BlockPos::new(x, feet + 1, z)),
        Some(top_slab),
        "the client was sent the ceiling"
    );
    session.ride.riding = Some(Box::new(crate::world::messages::Riding {
        vehicle: 7,
        kind: EntityType::OakBoat,
        dimension: session.dimension_id(),
        position: spot,
        riders: vec![1000],
        seat: [0.0; 3],
        driver: true,
        steers: false,
        no_gravity: false,
        flying: false,
        ridden: None,
        given: Default::default(),
    }));
    session.dismounted(7, spot).await;
    assert_eq!(
        session.current_pose(),
        tuff_vanilla_data::entity::Pose::Crouching
    );
    world.shutdown().await;
}

async fn command_cart(world: &TestWorld, region: &RegionHandle, command: &str) -> i32 {
    place_blocks(world, region, pen(2, 13)).await;
    let rail: tuff_vanilla_data::BlockState =
        "minecraft:activator_rail[powered=true,shape=north_south,waterlogged=false]"
            .parse()
            .unwrap();
    place_blocks(
        world,
        region,
        [
            (
                BlockPos::new(6, SKY + 1, 7),
                Block::RedstoneBlock.default_state(),
            ),
            (BlockPos::new(6, SKY + 1, 6), rail),
        ],
    )
    .await;
    summon(
        region,
        EntityType::CommandBlockMinecart,
        Compound::new().with("Command", command),
        [6.5, f64::from(SKY) + 1.0625, 6.5],
    )
    .await;
    mobs(region, EntityType::CommandBlockMinecart)
        .await
        .remove(0)
        .id
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_block_minecart_keeps_its_success_count_and_last_output() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let cart = command_cart(&world, &region, "time query gametime").await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        world.ticks(1).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let nbt = entity_nbt(&region, cart).await.unwrap();
        if nbt.get_int("SuccessCount") == Some(1) && nbt.get("LastOutput").is_some() {
            world.shutdown().await;
            return;
        }
    }
    panic!("the run was never recorded on the cart");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operators_edit_a_command_block_minecart_from_its_screen() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::CommandBlockMinecart,
        Compound::new(),
        [4.5, f64::from(SKY) + 1.0, 4.5],
    )
    .await;
    let cart = mobs(&region, EntityType::CommandBlockMinecart)
        .await
        .remove(0)
        .id;
    let (mut session, _frames) = session_with_outbound(&world, 1000);
    session.mode.abilities.instabuild = true;
    session.mode.permission = tuff_storage::admin::PermissionLevel::Gamemasters;
    let dimension = session.dimension_id();
    session.region_in(dimension, chunk.region()).await;
    let left = session
        .packet_hooks(crate::protocol::play::Serverbound::SetCommandMinecart {
            entity: cart,
            command: "say hi".into(),
            track_output: false,
        })
        .await;
    assert!(left.is_none(), "the minecart feature takes the packet");
    world.ticks(2).await;
    let nbt = entity_nbt(&region, cart).await.unwrap();
    assert_eq!(nbt.get_str("Command"), Some("say hi"));
    assert_eq!(nbt.get_bool("TrackOutput"), Some(false));
    world.shutdown().await;
}
