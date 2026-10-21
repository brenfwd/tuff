//! Right-click interactions driven through a real region: tools on blocks,
//! doors, buttons, note blocks, lighting and priming.

use super::*;
use crate::world::messages::HandEffect;
use tuff_vanilla_data::BlockState;

async fn outcome(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

/// Clicking the top of `target` with `item` held (not placed).
fn use_on(player: &PlayerHandle, target: BlockPos, item: Option<Item>, sequence: i32) -> Place {
    let mut click = place(player, target, item.unwrap_or(Item::Stick), sequence);
    click.item = None;
    click.held = item;
    click
}

struct Setup {
    world: TestWorld,
    region: RegionHandle,
    player: PlayerHandle,
    events: mpsc::UnboundedReceiver<PlayerEvent>,
    /// The first air block above the terrain at (1, 1).
    above: BlockPos,
}

async fn setup(dir: &Path) -> Setup {
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let world = TestWorld::start(dir);
    let region = world.region(pos.region()).await;
    view_around(&world, &player, &mut events, pos).await;
    Setup {
        world,
        region,
        player,
        events,
        above: BlockPos::new(1, height + 1, 1),
    }
}

async fn state(region: &RegionHandle, pos: BlockPos) -> BlockState {
    block_at(region, pos).await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn axes_strip_and_hoes_till_with_durability_and_loot() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let log = s.above;
    let rooted = BlockPos::new(log.x + 1, log.y, log.z);
    place_blocks(
        &s.world,
        &s.region,
        [
            (log, "minecraft:oak_log[axis=x]".parse().unwrap()),
            (rooted, Block::RootedDirt.default_state()),
        ],
    )
    .await;

    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            log,
            Some(Item::IronAxe),
            1,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 1).await,
        ActionOutcome::Used {
            item: Item::IronAxe,
            effect: HandEffect::Damage(1),
            ..
        }
    ));
    let stripped = state(&s.region, log).await;
    assert_eq!(stripped.block(), Block::StrippedOakLog);
    assert_eq!(stripped.get("axis"), Some("x"));

    let mut till = use_on(&s.player, rooted, Some(Item::StoneHoe), 2);
    till.face = tuff_world_model::Face::East;
    s.region.request(RegionRequest::Place(till)).await;
    assert!(matches!(
        outcome(&mut s.events, 2).await,
        ActionOutcome::Used { .. }
    ));
    assert_eq!(state(&s.region, rooted).await.block(), Block::Dirt);
    s.world.ticks(2).await;
    let entities = {
        let (reply, answer) = oneshot::channel();
        s.region.request(RegionRequest::Entities { reply }).await;
        answer.await.unwrap()
    };
    assert!(
        entities
            .iter()
            .any(|e| e.kind == tuff_vanilla_data::registry::EntityType::Item),
        "tilling rooted dirt drops hanging roots"
    );

    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            BlockPos::new(log.x, log.y - 1, log.z),
            Some(Item::IronAxe),
            3,
        )))
        .await;
    assert!(
        !matches!(outcome(&mut s.events, 3).await, ActionOutcome::Used { .. }),
        "an axe does nothing to terrain"
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decorated_pots_take_one_matching_item_per_click() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let pot = s.above;
    place_blocks(
        &s.world,
        &s.region,
        [(pot, Block::DecoratedPot.default_state())],
    )
    .await;
    for (sequence, item, inserted) in [
        (1, Item::Brick, true),
        (2, Item::Brick, true),
        (3, Item::Apple, false),
    ] {
        s.region
            .request(RegionRequest::Place(use_on(
                &s.player,
                pot,
                Some(item),
                sequence,
            )))
            .await;
        let result = outcome(&mut s.events, sequence).await;
        assert_eq!(
            matches!(
                result,
                ActionOutcome::Used {
                    effect: HandEffect::Consume(1),
                    ..
                }
            ),
            inserted,
            "{item:?}: {result:?}"
        );
    }
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inserting_into_a_decorated_pot_sends_a_dust_plume() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let pot = s.above;
    place_blocks(
        &s.world,
        &s.region,
        [(pot, Block::DecoratedPot.default_state())],
    )
    .await;
    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            pot,
            Some(Item::Brick),
            1,
        )))
        .await;
    expect(&mut s.events, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } => packets
            .iter()
            .any(|p| {
                framed_packet_id(p)
                    == tuff_vanilla_data::packet::play::Clientbound::LevelParticles.id()
            })
            .then_some(()),
        _ => None,
    })
    .await;
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shears_grow_a_kelp_head_to_max_age() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let kelp = s.above;
    place_blocks(
        &s.world,
        &s.region,
        [(kelp, Block::Kelp.default_state().with_age(3).unwrap())],
    )
    .await;

    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            kelp,
            Some(Item::Shears),
            1,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 1).await,
        ActionOutcome::Used {
            item: Item::Shears,
            effect: HandEffect::Damage(1),
            ..
        }
    ));
    assert_eq!(state(&s.region, kelp).await.get("age"), Some("25"));
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shears_carve_a_pumpkin_and_drop_seeds() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let pumpkin = s.above;
    place_blocks(
        &s.world,
        &s.region,
        [(pumpkin, Block::Pumpkin.default_state())],
    )
    .await;

    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            pumpkin,
            Some(Item::Shears),
            1,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 1).await,
        ActionOutcome::Used {
            item: Item::Shears,
            effect: HandEffect::Damage(1),
            ..
        }
    ));
    let carved = state(&s.region, pumpkin).await;
    assert_eq!(carved.block(), Block::CarvedPumpkin);
    assert_eq!(
        carved.facing(),
        Some(tuff_vanilla_data::block::Facing::North)
    );

    s.world.ticks(2).await;
    let entities = {
        let (reply, answer) = oneshot::channel();
        s.region.request(RegionRequest::Entities { reply }).await;
        answer.await.unwrap()
    };
    assert!(
        entities
            .iter()
            .any(|e| e.kind == tuff_vanilla_data::registry::EntityType::Item),
        "carving a pumpkin drops pumpkin seeds"
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doors_open_both_halves_and_buttons_release_on_time() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let ground = BlockPos::new(s.above.x, s.above.y - 1, s.above.z);
    s.region
        .request(RegionRequest::Place(place(
            &s.player,
            ground,
            Item::OakDoor,
            1,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 1).await,
        ActionOutcome::Placed { .. }
    ));
    let upper = BlockPos::new(s.above.x, s.above.y + 1, s.above.z);
    s.region
        .request(RegionRequest::Place(use_on(&s.player, upper, None, 2)))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 2).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(state(&s.region, s.above).await.open(), Some(true));
    assert_eq!(state(&s.region, upper).await.open(), Some(true));

    let button = BlockPos::new(s.above.x + 1, s.above.y, s.above.z);
    place_blocks(
        &s.world,
        &s.region,
        [(
            button,
            "minecraft:stone_button[face=floor]".parse().unwrap(),
        )],
    )
    .await;
    s.region
        .request(RegionRequest::Place(use_on(&s.player, button, None, 3)))
        .await;
    outcome(&mut s.events, 3).await;
    assert_eq!(state(&s.region, button).await.powered(), Some(true));
    s.world.ticks(10).await;
    assert_eq!(state(&s.region, button).await.powered(), Some(true));
    s.world.ticks(15).await;
    assert_eq!(
        state(&s.region, button).await.powered(),
        Some(false),
        "a stone button releases after 20 ticks"
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn note_blocks_tune_and_play_and_tnt_and_candles_light() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let note = s.above;
    let candle = BlockPos::new(note.x + 1, note.y, note.z);
    let tnt = BlockPos::new(note.x + 2, note.y, note.z);
    let ground = BlockPos::new(note.x, note.y - 1, note.z);
    s.region
        .request(RegionRequest::Place(place(
            &s.player,
            ground,
            Item::NoteBlock,
            1,
        )))
        .await;
    outcome(&mut s.events, 1).await;
    let placed = state(&s.region, note).await;
    assert_eq!(placed.block(), Block::NoteBlock);
    assert_ne!(placed.instrument(), None);
    s.region
        .request(RegionRequest::Place(use_on(&s.player, note, None, 2)))
        .await;
    expect(&mut s.events, |event| match event {
        PlayerEvent::AwardStat(tuff_vanilla_data::registry::CustomStat::TuneNoteblock, 1) => {
            Some(())
        }
        _ => None,
    })
    .await;
    let block_event_id = tuff_vanilla_data::packet::play::Clientbound::BlockEvent.id();
    expect(&mut s.events, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } => packets
            .iter()
            .any(|p| framed_packet_id(p) == block_event_id)
            .then_some(()),
        _ => None,
    })
    .await;
    outcome(&mut s.events, 2).await;
    assert_eq!(state(&s.region, note).await.note(), Some(1));

    place_blocks(
        &s.world,
        &s.region,
        [
            (candle, Block::RedCandle.default_state()),
            (tnt, Block::Tnt.default_state()),
        ],
    )
    .await;
    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            candle,
            Some(Item::FlintAndSteel),
            3,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 3).await,
        ActionOutcome::Used {
            effect: HandEffect::Damage(1),
            ..
        }
    ));
    assert_eq!(state(&s.region, candle).await.lit(), Some(true));
    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            tnt,
            Some(Item::FireCharge),
            4,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 4).await,
        ActionOutcome::Used {
            effect: HandEffect::Consume(1),
            ..
        }
    ));
    assert!(state(&s.region, tnt).await.block() != Block::Tnt);
    s.world.shutdown().await;
}

fn bucket(player: &PlayerHandle, above: BlockPos, item: Item, sequence: i32) -> RegionRequest {
    let (fluid, mob) = match item.bucket() {
        Some(tuff_vanilla_data::item::Bucket::Fluid { fluid, mob }) => (fluid, mob),
        _ => (None, None),
    };
    let eye = [
        above.x as f64 + 0.5,
        above.y as f64 + 2.0,
        above.z as f64 + 0.5,
    ];
    RegionRequest::UseBucket(crate::world::messages::BucketUse {
        player: player.clone(),
        position: [eye[0], eye[1] - 1.62, eye[2]],
        eye,
        look: crate::world::raycast::view_vector(0.0, 90.0),
        reach: 4.5,
        off_hand: false,
        item,
        fluid,
        mob,
        mob_data: None,
        mob_components: None,
        sneaking: false,
        creative: false,
        sequence,
        hit: None,
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bucket_aimed_across_a_region_border_fills_in_the_neighbour() {
    use tuff_world_model::pos::REGION_CHUNKS;
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    view_around(
        &world,
        &player,
        &mut events,
        ChunkPos::new(REGION_CHUNKS, 0),
    )
    .await;
    let near = world.region(RegionPos::new(0, 0)).await;
    let far = world.region(RegionPos::new(1, 0)).await;
    let x = REGION_CHUNKS * 16;
    let pool = BlockPos::new(x, Generator::new(1).height(x, 1) + 1, 1);
    place_blocks(&world, &far, [(pool, Block::Water.default_state())]).await;

    near.request(bucket(&player, pool, Item::Bucket, 1)).await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Used {
            effect: HandEffect::Exchange(Item::WaterBucket),
            ..
        }
    ));
    assert_eq!(state(&far, pool).await.block(), Block::Air);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn buckets_fill_from_sources_and_pour_into_air_and_waterloggable_blocks() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let pool = s.above;
    let slab = BlockPos::new(pool.x + 1, pool.y, pool.z);
    place_blocks(
        &s.world,
        &s.region,
        [
            (pool, Block::Water.default_state()),
            (slab, Block::OakSlab.default_state()),
        ],
    )
    .await;

    s.region
        .request(bucket(&s.player, pool, Item::Bucket, 1))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 1).await,
        ActionOutcome::Used {
            effect: HandEffect::Exchange(Item::WaterBucket),
            ..
        }
    ));
    assert_eq!(state(&s.region, pool).await.block(), Block::Air);

    s.region
        .request(bucket(&s.player, slab, Item::WaterBucket, 2))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 2).await,
        ActionOutcome::Used {
            effect: HandEffect::Emptied(Item::Bucket),
            ..
        }
    ));
    assert_eq!(state(&s.region, slab).await.waterlogged(), Some(true));

    s.region
        .request(bucket(&s.player, slab, Item::Bucket, 3))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 3).await,
        ActionOutcome::Used {
            effect: HandEffect::Exchange(Item::WaterBucket),
            ..
        }
    ));
    assert_eq!(state(&s.region, slab).await.waterlogged(), Some(false));

    let ground = BlockPos::new(pool.x, pool.y - 1, pool.z);
    s.region
        .request(bucket(&s.player, ground, Item::LavaBucket, 4))
        .await;
    outcome(&mut s.events, 4).await;
    assert_eq!(state(&s.region, pool).await.block(), Block::Lava);
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn water_poured_in_the_nether_evaporates() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region_in(THE_NETHER, pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    let floor = BlockPos::new(1, 40, 1);
    let above = BlockPos::new(1, 41, 1);
    place_blocks(
        &world,
        &region,
        [
            (floor, Block::Netherrack.default_state()),
            (above, Block::Air.default_state()),
            (BlockPos::new(1, 42, 1), Block::Air.default_state()),
            (BlockPos::new(1, 43, 1), Block::Air.default_state()),
        ],
    )
    .await;
    region
        .request(bucket(&player, floor, Item::WaterBucket, 1))
        .await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Used {
            effect: HandEffect::Emptied(Item::Bucket),
            ..
        }
    ));
    assert_eq!(state(&region, above).await.block(), Block::Air);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn covered_paths_and_farmland_turn_into_dirt_a_tick_later() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let offset = |pos: BlockPos, dx: i32, dy: i32| BlockPos::new(pos.x + dx, pos.y + dy, pos.z);
    let path = offset(s.above, 0, 3);
    let farmland = offset(s.above, 1, 3);
    let flickered_path = offset(s.above, 2, 3);
    let flickered_farmland = offset(s.above, 3, 3);
    let mut blocks = Vec::new();
    for (pos, block) in [
        (path, Block::DirtPath),
        (farmland, Block::Farmland),
        (flickered_path, Block::DirtPath),
        (flickered_farmland, Block::Farmland),
    ] {
        blocks.push((pos, block.default_state()));
        blocks.push((offset(pos, 0, 1), Block::Air.default_state()));
    }
    place_blocks(&s.world, &s.region, blocks).await;

    s.region
        .request(RegionRequest::Place(place(&s.player, path, Item::Stone, 1)))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 1).await,
        ActionOutcome::Placed { .. }
    ));
    assert_eq!(
        state(&s.region, offset(path, 0, 1)).await.block(),
        Block::Stone
    );
    assert_eq!(state(&s.region, path).await.block(), Block::DirtPath);

    let command = |changes: Vec<(BlockPos, Block)>| {
        let region = s.region.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            region
                .request(RegionRequest::Blocks {
                    op: crate::world::messages::BlockOp::Set {
                        changes: changes
                            .into_iter()
                            .map(|(pos, block)| (pos, block.default_state()))
                            .collect(),
                        destroy: false,
                        keep: false,
                        strict: false,
                        quiet: false,
                        data: None,
                    },
                    reply,
                })
                .await;
            answer.await.unwrap();
        }
    };
    command(vec![
        (offset(farmland, 0, 1), Block::Stone),
        (offset(flickered_path, 0, 1), Block::Stone),
        (offset(flickered_farmland, 0, 1), Block::Stone),
    ])
    .await;
    command(vec![
        (offset(flickered_path, 0, 1), Block::Air),
        (offset(flickered_farmland, 0, 1), Block::Air),
    ])
    .await;
    assert_eq!(state(&s.region, farmland).await.block(), Block::Farmland);

    s.world.ticks(1).await;
    assert_eq!(state(&s.region, path).await.block(), Block::Dirt);
    assert_eq!(state(&s.region, farmland).await.block(), Block::Dirt);
    assert_eq!(
        state(&s.region, flickered_path).await.block(),
        Block::Dirt,
        "a path's tick reverts it even after the cover is gone"
    );
    assert_eq!(
        state(&s.region, flickered_farmland).await.block(),
        Block::Farmland,
        "farmland checks again when its tick runs"
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_item_appears_and_leaves_without_being_collectable() {
    let dir = TestDir::new();
    let s = setup(dir.path()).await;
    let at = s.above;
    s.region
        .request(RegionRequest::SpawnFakeItem {
            stack: tuff_item::item::ItemSnapshot::describe(Item::Stone, 1, Default::default()),
            position: [at.x as f64 + 0.5, at.y as f64 + 3.0, at.z as f64 + 0.5],
        })
        .await;
    let count = |region: RegionHandle| async move {
        let (reply, answer) = oneshot::channel();
        region.request(RegionRequest::Entities { reply }).await;
        answer
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind == tuff_vanilla_data::registry::EntityType::Item)
            .count()
    };
    assert_eq!(count(s.region.clone()).await, 1);
    s.world.ticks(3).await;
    assert_eq!(count(s.region.clone()).await, 0);
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn game_master_blocks_are_placed_only_by_game_masters() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let floor = BlockPos::new(s.above.x, s.above.y - 1, s.above.z);
    s.region
        .request(RegionRequest::Place(place(
            &s.player,
            floor,
            Item::CommandBlock,
            1,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 1).await,
        ActionOutcome::Rejected(_)
    ));
    assert_eq!(state(&s.region, s.above).await.block(), Block::Air);

    let mut op = place(&s.player, floor, Item::CommandBlock, 2);
    op.game_master = true;
    s.region.request(RegionRequest::Place(op)).await;
    assert!(matches!(
        outcome(&mut s.events, 2).await,
        ActionOutcome::Placed { .. }
    ));
    assert_eq!(state(&s.region, s.above).await.block(), Block::CommandBlock);
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flower_pots_take_a_plant_and_give_it_back() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let pot = s.above;
    place_blocks(
        &s.world,
        &s.region,
        [(pot, Block::FlowerPot.default_state())],
    )
    .await;

    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            pot,
            Some(Item::Stone),
            1,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 1).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(state(&s.region, pot).await.block(), Block::FlowerPot);

    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            pot,
            Some(Item::OakSapling),
            2,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 2).await,
        ActionOutcome::Used {
            item: Item::OakSapling,
            effect: HandEffect::Consume(1),
            ..
        }
    ));
    assert_eq!(state(&s.region, pot).await.block(), Block::PottedOakSapling);

    s.region
        .request(RegionRequest::Place(use_on(
            &s.player,
            pot,
            Some(Item::Dandelion),
            3,
        )))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 3).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(state(&s.region, pot).await.block(), Block::PottedOakSapling);

    let mut bare = use_on(&s.player, pot, None, 4);
    bare.held = None;
    s.region.request(RegionRequest::Place(bare)).await;
    expect(&mut s.events, |event| match event {
        PlayerEvent::ItemGranted { stack, .. } => (stack.item == Item::OakSapling).then_some(()),
        _ => None,
    })
    .await;
    assert!(matches!(
        outcome(&mut s.events, 4).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(state(&s.region, pot).await.block(), Block::FlowerPot);
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn right_clicking_cave_vines_picks_the_glow_berries() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let vine = s.above;
    let ripe = Block::CaveVinesPlant
        .default_state()
        .with_berries(true)
        .unwrap();
    place_blocks(&s.world, &s.region, [(vine, ripe)]).await;

    s.region
        .request(RegionRequest::Place(use_on(&s.player, vine, None, 1)))
        .await;
    assert!(matches!(
        outcome(&mut s.events, 1).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(state(&s.region, vine).await.berries(), Some(false));

    s.world.ticks(2).await;
    let (reply, answer) = oneshot::channel();
    s.region.request(RegionRequest::Entities { reply }).await;
    let entities = answer.await.unwrap();
    assert!(
        entities
            .iter()
            .any(|e| e.kind == tuff_vanilla_data::registry::EntityType::Item),
        "picking berries drops glow berries"
    );
    s.world.shutdown().await;
}

fn adventure_components(key: &str, block: &str) -> tuff_item::item::Components {
    let mut predicate = tuff_nbt::Compound::new();
    predicate.insert("blocks", tuff_nbt::Tag::String(block.to_owned()));
    let mut components = tuff_item::item::Components::default();
    components
        .other
        .insert(key, tuff_nbt::Tag::Compound(predicate));
    components
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adventure_placing_needs_a_matching_can_place_on_predicate() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let floor = BlockPos::new(s.above.x, s.above.y - 1, s.above.z);
    let floor_name = state(&s.region, floor).await.block().name().to_owned();
    for (seq, block, placed) in [
        (1, "minecraft:bedrock", false),
        (2, floor_name.as_str(), true),
    ] {
        let mut click = place(&s.player, floor, Item::Stone, seq);
        click.may_build = false;
        click.components = adventure_components("minecraft:can_place_on", block).into();
        s.region.request(RegionRequest::Place(click)).await;
        let result = outcome(&mut s.events, seq).await;
        assert_eq!(matches!(result, ActionOutcome::Placed { .. }), placed);
    }
    assert_eq!(state(&s.region, s.above).await.block(), Block::Stone);

    let mut bare = place(&s.player, floor, Item::Stone, 3);
    bare.may_build = false;
    s.region.request(RegionRequest::Place(bare)).await;
    assert!(matches!(
        outcome(&mut s.events, 3).await,
        ActionOutcome::Rejected(_)
    ));
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adventure_breaking_needs_a_matching_can_break_predicate() {
    let dir = TestDir::new();
    let mut s = setup(dir.path()).await;
    let target = s.above;
    place_blocks(&s.world, &s.region, [(target, Block::Dirt.default_state())]).await;
    let eye = [1.5, target.y as f64 + 1.62, 2.5];
    for (seq, block, broken) in [(1, "minecraft:stone", false), (2, "minecraft:dirt", true)] {
        let mut tool = ItemStack::mint(Item::Stick, 1, MintReason::Test);
        tool.components = adventure_components("minecraft:can_break", block);
        s.region
            .request(RegionRequest::BreakRestricted {
                player: s.player.clone(),
                pos: target,
                tool: Some(tuff_item::item::ItemSnapshot::of(&tool)),
                mode: BreakMode::Verified {
                    ticks: 1000,
                    eye,
                    miner: player::Miner::plain(true),
                },
                sequence: seq,
            })
            .await;
        let result = outcome(&mut s.events, seq).await;
        assert_eq!(!matches!(result, ActionOutcome::Rejected(_)), broken);
    }
    assert_eq!(state(&s.region, target).await.block(), Block::Air);
    s.world.shutdown().await;
}
