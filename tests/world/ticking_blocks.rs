//! Conduits and sculk catalysts running on the region tick.

use super::*;
use crate::world::region_api::RegionTask;
use tuff_vanilla_data::registry::MobEffect;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_framed_conduit_gives_conduit_power_to_a_wet_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut events, &[pos]).await;
    let ground = Generator::new(1).height(8, 9);
    let center = BlockPos::new(8, ground + 1, 9);
    let cells = (-2i32..=2)
        .flat_map(|x| (-2i32..=2).flat_map(move |y| (-2i32..=2).map(move |z| (x, y, z))));
    let shell = cells.map(|(x, y, z)| {
        let inside = x.abs() <= 1 && y.abs() <= 1 && z.abs() <= 1;
        let state = if inside {
            Block::Water.default_state()
        } else {
            Block::Prismarine.default_state()
        };
        (
            BlockPos::new(center.x + x, center.y + y, center.z + z),
            state,
        )
    });
    place_blocks(&world, &region, shell).await;
    place_blocks(&world, &region, [(center, Block::Conduit.default_state())]).await;
    world.publish_players(
        "minecraft:overworld",
        &[(
            &alice,
            [
                f64::from(center.x) + 0.5,
                f64::from(center.y) + 1.0,
                f64::from(center.z) + 0.5,
            ],
        )],
    );
    world.ticks(100).await;
    let effect = expect(&mut events, |event| match event {
        PlayerEvent::GiveEffect { effect, .. } => Some(effect),
        _ => None,
    })
    .await;
    assert_eq!(effect, MobEffect::ConduitPower);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sculk_catalyst_blooms_and_spreads_sculk_from_a_mob_death() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let center = ChunkPos::new(5, 5);
    let region = world.region(center.region()).await;
    let (alice, mut events) = PlayerHandle::new(100);
    view_around(&world, &alice, &mut events, center).await;
    let ground = Generator::new(1).height(88, 89);
    let floor = ground + 20;
    let slab = (84..=92).flat_map(|x| {
        (85..=93).map(move |z| (BlockPos::new(x, floor, z), Block::Stone.default_state()))
    });
    place_blocks(&world, &region, slab).await;
    let catalyst = BlockPos::new(88, floor + 1, 89);
    place_blocks(
        &world,
        &region,
        [(catalyst, Block::SculkCatalyst.default_state())],
    )
    .await;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: catalyst,
            max: catalyst,
            run: RegionTask::new(move |world| {
                let took =
                    tuff_feature_builtin_vanilla_blocks::world::region::catalyst::mob_experience(
                        world,
                        [89.5, f64::from(floor) + 1.0, 89.5],
                        40,
                    );
                let _ = reply.send(took);
            }),
        })
        .await;
    assert!(answer.await.unwrap(), "the catalyst took the experience");
    world.ticks(1).await;
    let lit = block_at(&region, catalyst).await.unwrap();
    assert_eq!(lit.get("bloom"), Some("true"));
    world.ticks(600).await;
    let bloom = block_at(&region, catalyst).await.unwrap();
    assert_eq!(bloom.get("bloom"), Some("false"));
    let mut sculk = 0;
    for x in 84..=92 {
        for z in 85..=93 {
            for y in floor..=floor + 2 {
                let block = block_at(&region, BlockPos::new(x, y, z))
                    .await
                    .map(|s| s.block());
                if matches!(block, Some(Block::Sculk | Block::SculkVein)) {
                    sculk += 1;
                }
            }
        }
    }
    assert!(sculk > 0, "sculk spread over the stone");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicking_redstone_ore_lights_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let center = ChunkPos::new(5, 5);
    let region = world.region(center.region()).await;
    let (alice, mut events) = PlayerHandle::new(100);
    view_around(&world, &alice, &mut events, center).await;
    let ore = BlockPos::new(88, 200, 88);
    place_blocks(&world, &region, [(ore, Block::RedstoneOre.default_state())]).await;
    assert_eq!(block_at(&region, ore).await.unwrap().lit(), Some(false));
    region
        .request(RegionRequest::Place(place(&alice, ore, Item::Stick, 1)))
        .await;
    world.ticks(2).await;
    assert_eq!(block_at(&region, ore).await.unwrap().lit(), Some(true));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn starting_to_mine_redstone_ore_lights_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let center = ChunkPos::new(5, 5);
    let region = world.region(center.region()).await;
    let (alice, mut events) = PlayerHandle::new(100);
    view_around(&world, &alice, &mut events, center).await;
    let ore = BlockPos::new(90, 200, 88);
    place_blocks(
        &world,
        &region,
        [(ore, Block::DeepslateRedstoneOre.default_state())],
    )
    .await;
    assert_eq!(block_at(&region, ore).await.unwrap().lit(), Some(false));
    region
        .request(RegionRequest::Break {
            player: alice.clone(),
            pos: ore,
            tool: None,
            mode: BreakMode::Start {
                eye: [90.5, 201.6, 90.5],
                miner: player::Miner::plain(true),
            },
            sequence: 1,
        })
        .await;
    world.ticks(2).await;
    assert_eq!(block_at(&region, ore).await.unwrap().lit(), Some(true));
    world.shutdown().await;
}

async fn block_entity_generation(region: &RegionHandle, at: BlockPos) -> u64 {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: at,
            max: at,
            run: RegionTask::new(move |world| {
                let _ = reply.send(world.block_entity_generation());
            }),
        })
        .await;
    answer.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placing_a_beacon_changes_the_block_entity_generation_and_stone_does_not() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut events, &[pos]).await;
    let ground = Generator::new(1).height(8, 9);
    let at = BlockPos::new(8, ground + 1, 9);
    let before = block_entity_generation(&region, at).await;
    place_blocks(&world, &region, [(at, Block::Stone.default_state())]).await;
    assert_eq!(block_entity_generation(&region, at).await, before);
    place_blocks(&world, &region, [(at, Block::Beacon.default_state())]).await;
    let placed = block_entity_generation(&region, at).await;
    assert!(
        placed > before,
        "placing a beacon is seen without a rescan delay"
    );
    place_blocks(&world, &region, [(at, Block::Air.default_state())]).await;
    assert!(block_entity_generation(&region, at).await > placed);
    world.shutdown().await;
}
