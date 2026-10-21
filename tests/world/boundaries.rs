//! Cross-region behaviour: player and item visibility across region
//! borders, surroundings reports (water, magma, suffocation), and item
//! offers to nearby players.

use super::*;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_stay_visible_across_region_boundaries() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let (far, mut far_events) = PlayerHandle::new(101);
    let (walker, _walker_events) = PlayerHandle::new(102);
    subscribe(&west, &viewer, &mut events, &[ChunkPos::new(31, 0)]).await;
    subscribe(&east, &viewer, &mut events, &[ChunkPos::new(32, 0)]).await;
    subscribe(
        &west,
        &far,
        &mut far_events,
        &[ChunkPos::new(32 - tuff_world_model::pos::REGION_CHUNKS, 0)],
    )
    .await;

    west.request(update(&walker, 1, [510.0, 80.0, 8.0])).await;
    let seen = expect(&mut events, |event| match event {
        PlayerEvent::Track(track) if track.entity == 102 => Some(track),
        _ => None,
    })
    .await;
    assert_eq!(seen.chunk, ChunkPos::new(31, 0));
    assert!(seen.player.is_some());

    let crossed = [514.0, 80.0, 8.0];
    west.request(update(&walker, 2, crossed)).await;
    east.request(update(&walker, 2, crossed)).await;
    for _ in 0..2 {
        let seen = expect(&mut events, |event| match event {
            PlayerEvent::Track(track) if track.entity == 102 => Some(track),
            _ => None,
        })
        .await;
        assert_eq!((seen.chunk, seen.seq), (ChunkPos::new(32, 0), 2));
    }

    east.request(RegionRequest::PlayerRemoved {
        player: 102,
        seq: 3,
    })
    .await;
    expect(&mut events, |event| match event {
        PlayerEvent::Untrack {
            entity: 102,
            seq: 3,
        } => Some(()),
        _ => None,
    })
    .await;
    world.ticks(5).await;
    while let Ok(event) = far_events.try_recv() {
        if let PlayerEvent::Track(track) = &event {
            assert_ne!(
                track.entity, 102,
                "a distant player learned where another is"
            );
        }
        assert!(!matches!(event, PlayerEvent::Untrack { entity: 102, .. }));
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_cross_region_boundaries_and_persist_in_the_new_region() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let (west_chunk, east_chunk) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    subscribe(&west, &viewer, &mut events, &[west_chunk]).await;
    subscribe(&east, &viewer, &mut events, &[east_chunk]).await;
    world.ticks(3).await;

    let generator = Generator::new(1);
    let y = generator.height(511, 8).max(generator.height(513, 8)) + 3;
    west.request(RegionRequest::SpawnItem {
        stack: ItemStack::mint(Item::Dirt, 4, MintReason::Test),
        position: [511.5, y as f64, 8.5],
        velocity: [0.6, 0.0, 0.0],
        pickup_delay: 10_000,
    })
    .await;
    let spawned = expect(&mut events, |event| match event {
        PlayerEvent::Track(track) => Some(track),
        _ => None,
    })
    .await;
    assert_eq!(spawned.chunk, west_chunk);
    world.ticks(40).await;
    let mut arrived = false;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::Track(track) = event
            && track.entity == spawned.entity
            && track.chunk == east_chunk
        {
            arrived = true;
        }
    }
    assert!(arrived, "item never reached the eastern region");
    world.shutdown().await;

    let east_entities =
        stored_chunk(dir.path(), east_chunk, true).expect("east region saved the item");
    let entities = east_entities.get_list("Entities").unwrap();
    assert_eq!(entities.len(), 1);
    let item = entities[0].as_compound().unwrap();
    assert_eq!(item.get_str("id"), Some("minecraft:item"));
    assert!(stored_chunk(dir.path(), west_chunk, true).is_none());
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn support_loss_and_placement_cross_region_borders() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    let (west_chunk, east_chunk) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    subscribe(&west, &player, &mut events, &[west_chunk]).await;
    subscribe(&east, &player, &mut events, &[east_chunk]).await;
    world.ticks(3).await;
    let outcome = async |events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32| {
        expect(events, |event| match event {
            PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
            _ => None,
        })
        .await
    };
    let block_update = async |events: &mut mpsc::UnboundedReceiver<PlayerEvent>, at: BlockPos| {
        expect(events, |event| match event {
            PlayerEvent::ChunkPackets { packets, .. } => packets.iter().find_map(|packet| {
                let update = decode_block_update(packet)?;
                (update.0 == at).then_some(update.1)
            }),
            _ => None,
        })
        .await
    };

    let generator = Generator::new(1);
    let (z, ground) = (0..16)
        .map(|z| (z, generator.height(511, z)))
        .find(|(z, height)| generator.height(512, *z) == *height)
        .expect("a flat spot on the border");
    let west_ground = BlockPos::new(511, ground, z);
    let east_ground = BlockPos::new(512, ground, z);
    // A torch placed by clicking the top of the western ground, which the
    // western region resolves to a position it owns.
    west.request(RegionRequest::Place(place(
        &player,
        west_ground,
        Item::Torch,
        1,
    )))
    .await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Placed { .. }
    ));
    west.request(RegionRequest::Break {
        player: player.clone(),
        pos: BlockPos::new(511, ground + 1, z),
        tool: None,
        mode: BreakMode::Creative,
        sequence: 6,
    })
    .await;
    assert!(matches!(
        outcome(&mut events, 6).await,
        ActionOutcome::Broken { .. }
    ));

    // A bed whose foot is in the west and head in the east, placed looking
    // east along the border; the head reaches the east on its next tick.
    let mut bed = place(&player, west_ground, Item::RedBed, 2);
    bed.yaw = -90.0;
    west.request(RegionRequest::Place(bed)).await;
    assert!(matches!(
        outcome(&mut events, 2).await,
        ActionOutcome::Placed { .. }
    ));
    world.ticks(2).await;
    let head = block_update(&mut events, BlockPos::new(512, ground + 1, z)).await;
    assert_eq!(head.block(), Block::RedBed);
    assert_eq!(head.part(), Some(tuff_vanilla_data::block::Part::Head));

    // Breaking the bed's head in the east removes the foot in the west.
    east.request(RegionRequest::Break {
        player: player.clone(),
        pos: BlockPos::new(512, ground + 1, z),
        tool: None,
        mode: BreakMode::Finish,
        sequence: 3,
    })
    .await;
    assert!(matches!(
        outcome(&mut events, 3).await,
        ActionOutcome::Broken { .. }
    ));
    world.ticks(2).await;
    let foot = block_update(&mut events, BlockPos::new(511, ground + 1, z)).await;
    assert_eq!(
        foot.block(),
        Block::Air,
        "the bed's foot went with its head"
    );

    // Leaf litter placed on the eastern ground through the western region,
    // which forwards it; digging out that ground takes the litter with it,
    // before the break is acknowledged.
    west.request(RegionRequest::Place(place(
        &player,
        east_ground,
        Item::LeafLitter,
        4,
    )))
    .await;
    assert!(matches!(
        outcome(&mut events, 4).await,
        ActionOutcome::Placed { .. }
    ));
    east.request(RegionRequest::Break {
        player: player.clone(),
        pos: east_ground,
        tool: None,
        mode: BreakMode::Finish,
        sequence: 5,
    })
    .await;
    let fallen = block_update(&mut events, BlockPos::new(512, ground + 1, z)).await;
    assert_eq!(fallen.block(), Block::Air, "leaf litter lost its ground");
    assert!(matches!(
        outcome(&mut events, 5).await,
        ActionOutcome::Broken { .. }
    ));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_region_reports_water_around_players() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let generator = Generator::new(1);
    let (x, z) = (0..4000)
        .map(|i| (i * 13, i * 5))
        .find(|&(x, z)| generator.height(x, z) < Overworld::SEA_LEVEL - 3)
        .expect("some ocean");
    let pos = ChunkPos::new(x >> 4, z >> 4);
    let region = world.region(pos.region()).await;
    let (swimmer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &swimmer, &mut events, &[pos]).await;
    let floor = (generator.height(x, z) + 1) as f64;
    let column = [x as f64 + 0.5, 0.0, z as f64 + 0.5];
    for (seq, y, wet, under) in [
        (1, floor, true, true),
        (2, Overworld::SEA_LEVEL as f64 - 0.2, true, false),
        (3, Overworld::SEA_LEVEL as f64 + 1.0, false, false),
    ] {
        region
            .request(update(&swimmer, seq, [column[0], y, column[2]]))
            .await;
        let (in_water, submersion) = expect(&mut events, |event| match event {
            PlayerEvent::Surroundings {
                seq: at,
                in_water,
                submersion,
                ..
            } if at == seq => Some((in_water, submersion)),
            _ => None,
        })
        .await;
        assert_eq!(in_water, wet, "at y = {y}");
        assert_eq!(submersion.eyes, under, "eyes at y = {y}");
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_region_reports_magma_campfires_bushes_and_cactus_around_players() {
    use crate::world::block_contact::Contact;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[pos]).await;
    let y = 150;
    let bush = Block::SweetBerryBush
        .default_state()
        .with("age", "2")
        .unwrap();
    place_blocks(
        &world,
        &region,
        [
            (BlockPos::new(2, y, 2), Block::MagmaBlock.default_state()),
            (BlockPos::new(6, y, 2), Block::Stone.default_state()),
            (
                BlockPos::new(6, y + 1, 2),
                Block::SoulCampfire.default_state(),
            ),
            (BlockPos::new(10, y, 2), Block::GrassBlock.default_state()),
            (BlockPos::new(10, y + 1, 2), bush),
            (BlockPos::new(14, y, 2), Block::Cactus.default_state()),
            (BlockPos::new(0, y + 2, 6), Block::Stone.default_state()),
        ],
    )
    .await;
    let top = y as f64 + 1.0;
    for (seq, position, expected) in [
        (
            1,
            [2.5, top, 2.5],
            Contact {
                hot_floor: true,
                ..Contact::default()
            },
        ),
        (2, [2.5, top + 1.5, 2.5], Contact::default()),
        (
            3,
            [6.5, top + 0.4375, 2.5],
            Contact {
                campfire: Some(2.0),
                ..Contact::default()
            },
        ),
        (
            4,
            [10.5, top, 2.5],
            Contact {
                berry_bush: true,
                ..Contact::default()
            },
        ),
        (
            5,
            [14.5, y as f64 + 0.9375, 2.5],
            Contact {
                cactus: true,
                ..Contact::default()
            },
        ),
        (
            6,
            [13.75, y as f64 + 0.5, 2.5],
            Contact {
                cactus: true,
                ..Contact::default()
            },
        ),
        (
            7,
            [0.5, top, 6.5],
            Contact {
                suffocating: true,
                ..Contact::default()
            },
        ),
        (8, [24.5, top, 2.5], Contact::default()),
    ] {
        region.request(update(&player, seq, position)).await;
        let contact = expect(&mut events, |event| match event {
            PlayerEvent::Surroundings {
                seq: at, contact, ..
            } if at == seq => Some(contact),
            _ => None,
        })
        .await;
        if expected.suffocating {
            assert_eq!(contact.cramped, 2, "inside a block nothing fits");
        }
        let contact = Contact {
            cramped: 0,
            stuck: None,
            ..contact
        };
        assert_eq!(contact, expected, "at {position:?}");
    }
    world.shutdown().await;
}

/// A falling block or a piston can push a suffocating block into a player
/// who never moves, so the region must not wait for that player's next
/// movement report before it starts taking damage.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_landing_on_a_still_player_suffocates_it_promptly() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[pos]).await;
    let y = 150.0;
    let position = [2.5, y, 2.5];
    region.request(update(&player, 1, position)).await;
    expect(&mut events, |event| match event {
        PlayerEvent::Surroundings { seq: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    // Standard eye height (1.62) puts the eyes just inside block y = 151.
    let eye_block = BlockPos::new(2, 151, 2);
    place_blocks(
        &world,
        &region,
        [(eye_block, Block::Gravel.default_state())],
    )
    .await;
    expect(&mut events, |event| match event {
        PlayerEvent::Suffocating { .. } => Some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

/// Water flowing onto (or receding out from under) a stationary player
/// must not wait for its next movement report either -- otherwise a still,
/// dry player that water flows over keeps breathing and is never marked
/// as touching water until it moves.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn water_placed_on_a_still_player_is_reported_without_it_moving() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[pos]).await;
    let y = 150.0;
    let position = [2.5, y, 2.5];
    region.request(update(&player, 1, position)).await;
    expect(&mut events, |event| match event {
        PlayerEvent::Surroundings { seq: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    // The player's feet sit at block y = 150, well within its body box.
    let feet_block = BlockPos::new(2, 150, 2);
    place_blocks(
        &world,
        &region,
        [(feet_block, Block::Water.default_state())],
    )
    .await;
    let (in_water, submersion) = expect(&mut events, |event| match event {
        PlayerEvent::FluidContact {
            in_water,
            submersion,
            ..
        } => Some((in_water, submersion)),
        _ => None,
    })
    .await;
    assert!(in_water, "the still player now touches water");
    assert!(
        submersion.feet,
        "the block at its feet is water: {submersion:?}"
    );

    // Draining the water away must be reported too, without moving.
    place_blocks(&world, &region, [(feet_block, Block::Air.default_state())]).await;
    let (in_water, _) = expect(&mut events, |event| match event {
        PlayerEvent::FluidContact {
            in_water,
            submersion,
            ..
        } => Some((in_water, submersion)),
        _ => None,
    })
    .await;
    assert!(!in_water, "the water is gone");
    world.shutdown().await;
}

/// Lava or fire appearing at a stationary player is reported the same way
/// as water, so it starts burning without moving.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lava_and_fire_placed_on_a_still_player_are_reported_without_it_moving() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[pos]).await;
    let position = [2.5, 150.0, 2.5];
    region.request(update(&player, 1, position)).await;
    expect(&mut events, |event| match event {
        PlayerEvent::Surroundings { seq: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    let feet_block = BlockPos::new(2, 150, 2);
    let contact = |event: PlayerEvent| match event {
        PlayerEvent::FluidContact {
            in_lava, in_fire, ..
        } => Some((in_lava, in_fire)),
        _ => None,
    };

    place_blocks(&world, &region, [(feet_block, Block::Lava.default_state())]).await;
    let (in_lava, _) = expect(&mut events, contact).await;
    assert!(in_lava, "the still player now touches lava");

    place_blocks(&world, &region, [(feet_block, Block::Air.default_state())]).await;
    let (in_lava, _) = expect(&mut events, contact).await;
    assert!(!in_lava, "the lava is gone");

    place_blocks(&world, &region, [(feet_block, Block::Fire.default_state())]).await;
    let (_, in_fire) = expect(&mut events, contact).await;
    assert!(in_fire.is_some(), "the still player now stands in fire");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nearby_players_are_offered_items() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(2, 2);
    let region = world.region(pos.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    let y = Generator::new(1).height(40, 40) + 1;
    let at = [40.5, y as f64, 40.5];
    region.request(update(&viewer, 1, at)).await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 3, MintReason::Test),
            position: at,
            velocity: [0.0; 3],
            pickup_delay: 0,
        })
        .await;
    world.ticks(3).await;
    let (item, offer, stack, from) = expect(&mut events, |event| match event {
        PlayerEvent::OfferItem {
            item,
            offer,
            stack,
            region,
        } => Some((item, offer, stack, region)),
        _ => None,
    })
    .await;
    assert_eq!(stack, ItemStack::mint(Item::Stone, 3, MintReason::Test));
    answer(&from, &viewer, item, offer, 3);
    let granted = expect(&mut events, |event| match event {
        PlayerEvent::ItemGranted { stack, .. } => Some(stack),
        _ => None,
    })
    .await;
    assert_eq!(granted, ItemStack::mint(Item::Stone, 3, MintReason::Test));
    expect(&mut events, |event| match event {
        PlayerEvent::Untrack { entity, .. } if entity == item => Some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

struct Offer {
    item: i32,
    offer: u64,
    region: RegionHandle,
}

fn take_offer(events: &mut tokio::sync::mpsc::UnboundedReceiver<PlayerEvent>) -> Option<Offer> {
    let mut found = None;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::OfferItem {
            item,
            offer,
            region,
            ..
        } = event
        {
            found = Some(Offer {
                item,
                offer,
                region,
            });
        }
    }
    found
}

fn answer(region: &RegionHandle, player: &PlayerHandle, item: i32, offer: u64, taken: u8) {
    region.internal(RegionInternal::ItemTaken {
        item,
        offer,
        player: player.clone(),
        taken,
    });
}

impl Offer {
    fn answer(&self, player: &PlayerHandle, taken: u8) {
        answer(&self.region, player, self.item, self.offer, taken);
    }
}

fn grants(events: &mut tokio::sync::mpsc::UnboundedReceiver<PlayerEvent>) -> u32 {
    let mut count = 0;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::ItemGranted { stack, .. } = event {
            count += u32::from(stack.count());
        }
    }
    count
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusing_player_does_not_starve_another_one_next_to_the_item() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(2, 2);
    let region = world.region(pos.region()).await;
    let (first, mut first_events) = PlayerHandle::new(100);
    let (second, mut second_events) = PlayerHandle::new(101);
    subscribe(&region, &first, &mut first_events, &[pos]).await;
    subscribe(&region, &second, &mut second_events, &[pos]).await;
    let y = Generator::new(1).height(40, 40) + 1;
    let at = [40.5, y as f64, 40.5];
    region.request(update(&first, 1, at)).await;
    region
        .request(update(&second, 1, [40.8, y as f64, 40.5]))
        .await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 3, MintReason::Test),
            position: at,
            velocity: [0.0; 3],
            pickup_delay: 0,
        })
        .await;
    let mut refuser = None;
    for _ in 0..20 {
        world.ticks(1).await;
        let offers = [
            take_offer(&mut first_events),
            take_offer(&mut second_events),
        ];
        let players = [&first, &second];
        for (index, offer) in offers.into_iter().enumerate() {
            let Some(offer) = offer else {
                continue;
            };
            match refuser {
                None => refuser = Some(index),
                Some(r) if r != index => {
                    offer.answer(players[index], 3);
                    world.shutdown().await;
                    return;
                }
                Some(_) => {}
            }
            offer.answer(players[index], 0);
        }
    }
    panic!("only the refusing player was ever offered the item");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_item_is_offered_again_once_there_is_room() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(2, 2);
    let region = world.region(pos.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    let y = Generator::new(1).height(40, 40) + 1;
    let at = [40.5, y as f64, 40.5];
    region.request(update(&viewer, 1, at)).await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 3, MintReason::Test),
            position: at,
            velocity: [0.0; 3],
            pickup_delay: 0,
        })
        .await;
    for _ in 0..5 {
        world.ticks(1).await;
        let offer = take_offer(&mut events).expect("offered every tick while refused");
        offer.answer(&viewer, 0);
    }
    world.ticks(1).await;
    let offer = take_offer(&mut events).expect("offered again");
    offer.answer(&viewer, 3);
    expect(&mut events, |event| match event {
        PlayerEvent::Untrack { entity, .. } if entity == offer.item => Some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_just_across_a_region_border_are_offered() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&west, &viewer, &mut events, &[ChunkPos::new(31, 0)]).await;
    subscribe(&east, &viewer, &mut events, &[ChunkPos::new(32, 0)]).await;
    let y = Generator::new(1)
        .height(511, 8)
        .max(Generator::new(1).height(512, 8))
        + 1;
    for (player_x, item_x) in [(512.3, 511.4), (511.7, 512.6), (512.0, 512.0)] {
        let feet = [player_x, y as f64, 8.5];
        west.request(update(&viewer, 1, feet)).await;
        east.request(update(&viewer, 1, feet)).await;
        let owner = if item_x < 512.0 { &west } else { &east };
        owner
            .request(RegionRequest::SpawnItem {
                stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
                position: [item_x, y as f64, 8.5],
                velocity: [0.0; 3],
                pickup_delay: 0,
            })
            .await;
        let mut taken = false;
        for _ in 0..5 {
            world.ticks(1).await;
            if let Some(offer) = take_offer(&mut events) {
                offer.answer(&viewer, 1);
                taken = true;
                break;
            }
        }
        assert!(
            taken,
            "item at x {item_x} not offered to a player at x {player_x}"
        );
        world.ticks(1).await;
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_answer_to_an_expired_offer_grants_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(2, 2);
    let region = world.region(pos.region()).await;
    let (first, mut first_events) = PlayerHandle::new(100);
    let (second, mut second_events) = PlayerHandle::new(101);
    subscribe(&region, &first, &mut first_events, &[pos]).await;
    subscribe(&region, &second, &mut second_events, &[pos]).await;
    let y = Generator::new(1).height(40, 40) + 1;
    let at = [40.5, y as f64, 40.5];
    region.request(update(&first, 1, at)).await;
    region
        .request(update(&second, 1, [40.8, y as f64, 40.5]))
        .await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 3, MintReason::Test),
            position: at,
            velocity: [0.0; 3],
            pickup_delay: 0,
        })
        .await;
    let players = [&first, &second];
    let mut stale = None;
    for _ in 0..5 {
        world.ticks(1).await;
        stale = take_offer(&mut first_events)
            .map(|offer| (0, offer))
            .or_else(|| take_offer(&mut second_events).map(|offer| (1, offer)));
        if stale.is_some() {
            break;
        }
    }
    let (stale_index, stale) = stale.expect("offered");
    let fresh_index = 1 - stale_index;
    region
        .request(update(players[stale_index], 2, [46.5, y as f64, 40.5]))
        .await;
    let mut fresh = None;
    for _ in 0..40 {
        world.ticks(1).await;
        let events = if fresh_index == 0 {
            &mut first_events
        } else {
            &mut second_events
        };
        fresh = take_offer(events);
        if fresh.is_some() {
            break;
        }
    }
    let fresh = fresh.expect("offered to the other player once the first offer expired");
    assert_ne!(stale.offer, fresh.offer);
    stale.answer(players[stale_index], 3);
    fresh.answer(players[fresh_index], 3);
    world.ticks(5).await;
    let granted = [grants(&mut first_events), grants(&mut second_events)];
    assert_eq!(
        granted[fresh_index], 3,
        "the outstanding offer's answer took the stack"
    );
    assert_eq!(
        granted.iter().sum::<u32>(),
        3,
        "the stale answer took nothing"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_reaching_a_region_without_the_item_grants_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&west, &viewer, &mut events, &[ChunkPos::new(31, 0)]).await;
    subscribe(&east, &viewer, &mut events, &[ChunkPos::new(32, 0)]).await;
    let y = Generator::new(1)
        .height(511, 8)
        .max(Generator::new(1).height(512, 8))
        + 1;
    let feet = [511.7, y as f64, 8.5];
    west.request(update(&viewer, 1, feet)).await;
    east.request(update(&viewer, 1, feet)).await;
    west.request(RegionRequest::SpawnItem {
        stack: ItemStack::mint(Item::Stone, 2, MintReason::Test),
        position: [511.4, y as f64, 8.5],
        velocity: [0.0; 3],
        pickup_delay: 0,
    })
    .await;
    let mut offer = None;
    for _ in 0..5 {
        world.ticks(1).await;
        offer = take_offer(&mut events);
        if offer.is_some() {
            break;
        }
    }
    let offer = offer.expect("offered");
    answer(&east, &viewer, offer.item, offer.offer, 2);
    answer(
        &offer.region,
        &viewer,
        offer.item,
        offer.offer + 1_000_000,
        2,
    );
    world.ticks(3).await;
    assert_eq!(
        grants(&mut events),
        0,
        "no region honoured an answer to another offer"
    );
    offer.answer(&viewer, 2);
    world.ticks(3).await;
    assert_eq!(grants(&mut events), 2);
    offer.answer(&viewer, 2);
    world.ticks(3).await;
    assert_eq!(grants(&mut events), 0, "an answer is honoured once");
    world.shutdown().await;
}
