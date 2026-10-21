//! Cobwebs, sweet berry bushes and powder snow holding back the mobs
//! inside them.

use super::*;
use tuff_vanilla_data::registry::EntityType;

async fn stuck_world() -> (TestDir, TestWorld, RegionHandle) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    view_around(&world, &player, &mut events, chunk).await;
    std::mem::forget(events);
    (dir, world, region)
}

fn column(
    x: i32,
    z: i32,
    block: Block,
    height: i32,
) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = vec![(BlockPos::new(x, SKY, z), Block::Stone.default_state())];
    for y in SKY + 1..=SKY + height {
        blocks.push((BlockPos::new(x, y, z), block.default_state()));
    }
    blocks
}

async fn height_of(region: &RegionHandle, kind: EntityType, x: f64) -> f64 {
    mobs(region, kind)
        .await
        .into_iter()
        .find(|s| (s.position[0] - x).abs() < 0.5 && (s.position[2] - 24.5).abs() < 0.5)
        .unwrap_or_else(|| panic!("no {} at x {x}", kind.name()))
        .position[1]
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cobweb_slows_a_falling_mob_to_a_crawl_and_weaving_less_so() {
    let (_dir, world, region) = stuck_world().await;
    let mut blocks = column(20, 24, Block::Cobweb, 4);
    blocks.extend(column(23, 24, Block::Cobweb, 4));
    blocks.extend(column(26, 24, Block::Air, 4));
    place_blocks(&world, &region, blocks).await;
    let start = (SKY + 4) as f64;
    let calm = || Compound::new().with("NoAI", true);
    summon(&region, EntityType::Pig, calm(), [20.5, start, 24.5]).await;
    let weaving = Compound::new()
        .with("id", "minecraft:weaving")
        .with("amplifier", 0i8)
        .with("duration", 4000);
    summon(
        &region,
        EntityType::Pig,
        calm().with("active_effects", Tag::List(vec![Tag::Compound(weaving)])),
        [23.5, start, 24.5],
    )
    .await;
    summon(&region, EntityType::Pig, calm(), [26.5, start, 24.5]).await;
    world.ticks(21).await;
    let stuck = start - height_of(&region, EntityType::Pig, 20.5).await;
    let woven = start - height_of(&region, EntityType::Pig, 23.5).await;
    let free = start - height_of(&region, EntityType::Pig, 26.5).await;
    let crawl = 0.078_400_001_525_878_9 * 0.050_000_000_745_058_06;
    assert!(
        (stuck - 20.0 * crawl).abs() < 0.01,
        "a cobweb lets it sink {crawl} a tick, not {stuck} in 20 ticks"
    );
    assert!(
        woven > stuck * 2.0 && woven < 1.0,
        "weaving holds less: {woven} against {stuck}"
    );
    assert!((free - 3.0).abs() < 1.0e-6, "the free pig lands: {free}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn berry_bushes_slow_a_sliding_mob_but_not_a_fox() {
    let (_dir, world, region) = stuck_world().await;
    let mut blocks = Vec::new();
    for x in 18..=30 {
        for z in [21, 24, 27] {
            blocks.push((BlockPos::new(x, SKY, z), Block::GrassBlock.default_state()));
        }
    }
    for z in [21, 24] {
        blocks.push((
            BlockPos::new(20, SKY + 1, z),
            Block::SweetBerryBush.default_state(),
        ));
    }
    place_blocks(&world, &region, blocks).await;
    let feet = (SKY + 1) as f64;
    let sliding = || {
        Compound::new().with("NoAI", true).with(
            "Motion",
            Tag::List(vec![Tag::Double(0.5), Tag::Double(0.0), Tag::Double(0.0)]),
        )
    };
    summon(&region, EntityType::Pig, sliding(), [20.5, feet, 24.5]).await;
    summon(&region, EntityType::Fox, sliding(), [20.5, feet, 21.5]).await;
    summon(&region, EntityType::Pig, sliding(), [20.5, feet, 27.5]).await;
    world.ticks(20).await;
    let found = mobs(&region, EntityType::Pig).await;
    let slid = |z: f64| {
        found
            .iter()
            .find(|s| (s.position[2] - z).abs() < 0.5)
            .map(|s| s.position[0] - 20.5)
            .expect("the pig is there")
    };
    let (held, free) = (slid(24.5), slid(27.5));
    let fox = mobs(&region, EntityType::Fox).await[0].position[0] - 20.5;
    assert!(
        held < free * 0.8,
        "the bush holds the pig back ({held}) more than open grass ({free})"
    );
    assert!((fox - free).abs() < 1.0e-6, "foxes slip through: {fox}");
    world.shutdown().await;
}
