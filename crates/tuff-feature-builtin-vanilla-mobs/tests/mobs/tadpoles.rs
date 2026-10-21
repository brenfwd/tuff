//! The frog life cycle: frogs lay frogspawn after breeding, frogspawn
//! hatches into tadpoles, tadpoles grow into frogs, can be bucketed with
//! their age and speed up on slimeballs.

use super::*;
use crate::world::messages::{ActionOutcome, HandEffect};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::EntityType;

fn up(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y + 1, pos.z)
}

fn open_pool(min: i32, max: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in min - 1..=max + 1 {
        for z in min - 1..=max + 1 {
            let inside = (min..=max).contains(&x) && (min..=max).contains(&z);
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
            let state = if inside {
                Block::Water.default_state()
            } else {
                Block::Stone.default_state()
            };
            blocks.push((BlockPos::new(x, SKY + 1, z), state));
            blocks.push((BlockPos::new(x, SKY + 2, z), Block::Stone.default_state()));
        }
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bred_frogs_lay_frogspawn_on_the_water_edge() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let pond = BlockPos::new(9, SKY, 9);
    let mut blocks = pen(2, 13);
    blocks.push((
        BlockPos::new(pond.x, SKY - 1, pond.z),
        Block::Stone.default_state(),
    ));
    blocks.push((pond, Block::Water.default_state()));
    place_blocks(&world, &region, blocks).await;
    for x in [5.5, 6.5] {
        summon(
            &region,
            EntityType::Frog,
            Compound::new(),
            [x, SKY as f64 + 1.0, 5.5],
        )
        .await;
    }
    world.ticks(2).await;
    for frog in mobs(&region, EntityType::Frog).await {
        let effect = interact(&region, &viewer, &mut events, frog.id, Item::SlimeBall).await;
        assert!(matches!(effect, HandEffect::Consume(1)));
    }
    let mut laid = false;
    for _ in 0..3000 {
        world.ticks(1).await;
        if block_at(&region, up(pond)).await.map(|s| s.block()) == Some(Block::Frogspawn) {
            laid = true;
            break;
        }
    }
    assert!(laid, "a pregnant frog lays frogspawn on the pond");
    assert_eq!(
        mobs(&region, EntityType::Frog).await.len(),
        2,
        "breeding makes no baby frog"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frogspawn_hatches_into_tadpoles() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(5, 5);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let around: Vec<ChunkPos> = (4..=6)
        .flat_map(|x| (4..=6).map(move |z| ChunkPos::new(x, z)))
        .collect();
    subscribe(&region, &viewer, &mut events, &around).await;
    let mut blocks = open_pool(84, 88);
    let spawn = BlockPos::new(86, SKY + 2, 86);
    blocks.push((spawn, Block::Frogspawn.default_state()));
    place_blocks(&world, &region, blocks).await;
    let mut tadpoles = Vec::new();
    for _ in 0..12_100 {
        world.ticks(1).await;
        tadpoles = mobs(&region, EntityType::Tadpole).await;
        if !tadpoles.is_empty() {
            break;
        }
    }
    assert!((2..=5).contains(&tadpoles.len()), "got {}", tadpoles.len());
    assert_ne!(
        block_at(&region, spawn).await.map(|s| s.block()),
        Some(Block::Frogspawn)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_grown_tadpole_turns_into_a_frog() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, open_pool(4, 8)).await;
    summon(
        &region,
        EntityType::Tadpole,
        Compound::new().with("Age", 23_990),
        [6.5, SKY as f64 + 1.5, 6.5],
    )
    .await;
    let found = wait_for(&world, &region, 40, |found| {
        found.iter().any(|s| s.kind == EntityType::Frog)
    })
    .await;
    assert!(found.iter().all(|s| s.kind != EntityType::Tadpole));
    let frog = mobs(&region, EntityType::Frog).await[0].id;
    let nbt = entity_nbt(&region, frog).await.expect("the frog");
    let variant = nbt.get_str("variant").expect("variant");
    assert!(
        tuff_vanilla_data::variant::index(tuff_vanilla_data::variant::FROG, variant).is_some(),
        "{variant}"
    );
    assert_eq!(nbt.get_bool("PersistenceRequired"), Some(true));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tadpole_buckets_keep_age_and_slimeballs_speed_growth() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Tadpole,
        Compound::new().with("Age", 1000).with("NoAI", true),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    let id = mobs(&region, EntityType::Tadpole).await[0].id;
    let fed = interact(&region, &viewer, &mut events, id, Item::SlimeBall).await;
    assert!(matches!(fed, HandEffect::Consume(1)));
    let age = entity_nbt(&region, id)
        .await
        .and_then(|n| n.get_int("Age"))
        .expect("age");
    assert!((3300..=3310).contains(&age), "fed from 1000 to {age}");

    let effect = interact(&region, &viewer, &mut events, id, Item::WaterBucket).await;
    let HandEffect::ExchangeStack(stack) = effect else {
        panic!("expected a tadpole bucket, got {effect:?}");
    };
    assert_eq!(stack.item, Item::TadpoleBucket);
    assert!(mobs(&region, EntityType::Tadpole).await.is_empty());
    let data = bucket_entity_data(&stack).expect("bucket data");
    assert_eq!(data.get_int("Age"), Some(age));

    region
        .request(place_bucket(
            &viewer,
            BlockPos::new(5, SKY, 5),
            Item::TadpoleBucket,
            Some(EntityType::Tadpole),
            Some(data),
            1,
        ))
        .await;
    assert!(matches!(
        bucket_outcome(&mut events, 1).await,
        ActionOutcome::Used { .. }
    ));
    let found = wait_for(&world, &region, 5, |found| {
        found.iter().any(|s| s.kind == EntityType::Tadpole)
    })
    .await;
    let placed = found
        .iter()
        .find(|s| s.kind == EntityType::Tadpole)
        .expect("placed");
    let nbt = entity_nbt(&region, placed.id).await.expect("tadpole");
    assert!(nbt.get_int("Age").is_some_and(|a| a >= age));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn golden_dandelion_locks_tadpole_age() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Tadpole,
        Compound::new().with("Age", 1000).with("NoAI", true),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    let id = mobs(&region, EntityType::Tadpole).await[0].id;
    let used = interact(&region, &viewer, &mut events, id, Item::GoldenDandelion).await;
    assert!(matches!(used, HandEffect::Consume(1)));
    let nbt = entity_nbt(&region, id).await.expect("tadpole");
    assert_eq!(nbt.get_int("Age"), Some(0));
    assert_eq!(nbt.get_bool("AgeLocked"), Some(true));
    world.shutdown().await;
}
