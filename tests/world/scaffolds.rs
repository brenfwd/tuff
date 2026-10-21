//! Scaffolding used on scaffolding extends the structure (up the column,
//! out along the top, out of a face while sneaking), and mobs stand on
//! scaffolding instead of falling through it.

use super::*;
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::Face;

const SKY: i32 = 150;
const AIR: tuff_vanilla_data::BlockState = Block::Air.default_state();

async fn outcome(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

async fn click(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    target: BlockPos,
    face: Face,
    sneaking: bool,
    sequence: i32,
) -> ActionOutcome {
    let mut placing = place(player, target, Item::Scaffolding, sequence);
    placing.face = face;
    placing.sneaking = sneaking;
    region.request(RegionRequest::Place(placing)).await;
    outcome(events, sequence).await
}

fn is_scaffolding(state: Option<tuff_vanilla_data::BlockState>) -> bool {
    state.is_some_and(|s| s.block() == Block::Scaffolding)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scaffolding_clicked_with_scaffolding_extends_the_structure() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let at = |dy: i32| BlockPos::new(8, SKY + dy, 8);
    place_blocks(&world, &region, [(at(0), Block::Stone.default_state())]).await;
    place_blocks(
        &world,
        &region,
        (1..=3).map(|dy| (at(dy), Block::Scaffolding.default_state())),
    )
    .await;
    world.ticks(4).await;

    let placed = click(&region, &player, &mut events, at(1), Face::North, false, 1).await;
    assert!(matches!(placed, ActionOutcome::Placed { .. }), "{placed:?}");
    assert!(
        is_scaffolding(block_at(&region, at(4)).await),
        "the column grew on top"
    );
    assert_eq!(
        block_at(&region, BlockPos::new(8, SKY + 1, 7)).await,
        Some(AIR)
    );

    let placed = click(&region, &player, &mut events, at(4), Face::Up, false, 2).await;
    assert!(matches!(placed, ActionOutcome::Placed { .. }), "{placed:?}");
    let south = BlockPos::new(8, SKY + 4, 9);
    assert!(
        is_scaffolding(block_at(&region, south).await),
        "the top grew the way the player faces"
    );
    world.ticks(2).await;
    assert_eq!(
        block_at(&region, south).await.and_then(|s| s.distance()),
        Some(1)
    );

    let placed = click(&region, &player, &mut events, at(2), Face::West, true, 3).await;
    assert!(matches!(placed, ActionOutcome::Placed { .. }), "{placed:?}");
    assert!(
        is_scaffolding(block_at(&region, BlockPos::new(7, SKY + 2, 8)).await),
        "sneaking builds out of the clicked face"
    );
    assert_eq!(block_at(&region, at(5)).await, Some(AIR));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scaffolding_reaching_too_far_sideways_places_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(101);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let row: Vec<_> = (2..=9)
        .flat_map(|x| {
            [
                (BlockPos::new(x, SKY, 8), Block::Stone.default_state()),
                (
                    BlockPos::new(x, SKY + 1, 8),
                    Block::Scaffolding.default_state(),
                ),
            ]
        })
        .collect();
    place_blocks(&world, &region, row).await;
    world.ticks(2).await;
    let placed = click(
        &region,
        &player,
        &mut events,
        BlockPos::new(9, SKY + 1, 8),
        Face::West,
        true,
        1,
    )
    .await;
    assert!(
        !matches!(placed, ActionOutcome::Placed { .. }),
        "past seven blocks of scaffolding: {placed:?}"
    );
    assert_eq!(
        block_at(&region, BlockPos::new(1, SKY + 1, 8)).await,
        Some(AIR)
    );
    let placed = click(
        &region,
        &player,
        &mut events,
        BlockPos::new(8, SKY + 1, 8),
        Face::West,
        true,
        2,
    )
    .await;
    assert!(matches!(placed, ActionOutcome::Placed { .. }), "{placed:?}");
    assert!(is_scaffolding(
        block_at(&region, BlockPos::new(1, SKY + 1, 8)).await
    ));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_slides_into_scaffolding_but_stands_on_top_of_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(102);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let mut blocks = Vec::new();
    for x in 2..=14 {
        for z in 2..=14 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
        }
    }
    for z in 2..=14 {
        for dy in 1..=2 {
            blocks.push((
                BlockPos::new(10, SKY + dy, z),
                Block::Scaffolding.default_state(),
            ));
        }
    }
    place_blocks(&world, &region, blocks).await;
    world.ticks(2).await;
    let still = Compound::new().with("NoAI", true);
    summon(
        &region,
        EntityType::Cow,
        still.clone().with(
            "Motion",
            vec![Tag::Double(0.5), Tag::Double(0.0), Tag::Double(0.0)],
        ),
        [9.0, f64::from(SKY) + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Cow,
        still,
        [10.5, f64::from(SKY) + 5.0, 11.5],
    )
    .await;
    world.ticks(40).await;
    let cows = mobs(&region, EntityType::Cow).await;
    let slid = cows
        .iter()
        .find(|c| c.position[2] < 8.0)
        .expect("the sliding cow")
        .position;
    assert!(slid[0] > 9.7, "walked into the scaffolding: {slid:?}");
    let atop = cows
        .iter()
        .find(|c| c.position[2] > 8.0)
        .expect("the dropped cow")
        .position;
    assert!(
        (atop[1] - f64::from(SKY + 3)).abs() < 1.0e-6,
        "landed on top of the scaffolding: {atop:?}"
    );
    world.shutdown().await;
}
