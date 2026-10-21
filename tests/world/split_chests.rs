//! Double chests whose halves belong to two regions.

use super::*;
use tuff_vanilla_data::block::{Facing, Type as ChestType};

type OpenBlocks = (
    Vec<(BlockPos, usize)>,
    Vec<Option<ItemStack>>,
    Option<(BlockPos, RegionHandle)>,
);

struct Split {
    dir: TestDir,
    world: TestWorld,
    west: RegionHandle,
    east: RegionHandle,
    west_pos: BlockPos,
    east_pos: BlockPos,
}

/// The right half, which comes first in the menu, of a chest facing north
/// is its east block.
fn right_first(split: &Split) -> [BlockPos; 2] {
    [split.east_pos, split.west_pos]
}

async fn split_chest(
    viewers: &mut [(&PlayerHandle, &mut mpsc::UnboundedReceiver<PlayerEvent>)],
) -> Split {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west_chunk = ChunkPos::new(31, 0);
    let east_chunk = ChunkPos::new(32, 0);
    let west = world.region(west_chunk.region()).await;
    let east = world.region(east_chunk.region()).await;
    assert_ne!(west_chunk.region(), east_chunk.region());
    for (player, events) in viewers.iter_mut() {
        subscribe(&west, player, events, &[west_chunk]).await;
        subscribe(&east, player, events, &[east_chunk]).await;
    }
    let generator = Generator::new(1);
    let y = generator.height(511, 1).max(generator.height(512, 1)) + 2;
    let west_pos = BlockPos::new(511, y, 1);
    let east_pos = BlockPos::new(512, y, 1);
    let north = Block::Chest
        .default_state()
        .with_facing(Facing::North)
        .unwrap();
    let [west_state, east_state] =
        [ChestType::Left, ChestType::Right].map(|kind| north.with_type(kind).unwrap());
    assert_eq!(
        placement::chest_partner(west_state, west_pos),
        Some(east_pos)
    );
    assert_eq!(
        placement::chest_partner(east_state, east_pos),
        Some(west_pos)
    );
    place_blocks(&world, &west, [(west_pos, west_state)]).await;
    place_blocks(&world, &east, [(east_pos, east_state)]).await;
    for (region, pos) in [(&west, west_pos), (&east, east_pos)] {
        await_joined(&world, region, pos).await;
    }
    Split {
        dir,
        world,
        west,
        east,
        west_pos,
        east_pos,
    }
}

/// Ticks until a click at `pos` joins both halves, which needs the other
/// region's chunk in this region's halo.
async fn await_joined(world: &TestWorld, region: &RegionHandle, pos: BlockPos) {
    let (probe, mut events) = PlayerHandle::new(999);
    loop {
        world.ticks(1).await;
        region
            .request(RegionRequest::Place(place(&probe, pos, Item::Torch, 1)))
            .await;
        let (parts, _, remote) = blocks(expect(&mut events, container_menu).await);
        let mut positions: Vec<BlockPos> = parts.iter().map(|(pos, _)| *pos).collect();
        if let Some((far, handle)) = remote {
            positions.retain(|p| *p != far);
            handle
                .request(RegionRequest::CloseContainer {
                    player: probe.id,
                    positions: vec![far],
                })
                .await;
        }
        region
            .request(RegionRequest::CloseContainer {
                player: probe.id,
                positions,
            })
            .await;
        if parts.len() == 2 {
            return;
        }
    }
}

fn blocks(open: MenuOpen) -> OpenBlocks {
    let MenuSource::Blocks {
        parts,
        contents,
        remote,
        ..
    } = open.source
    else {
        panic!("a chest is a block container");
    };
    (parts, contents, remote)
}

async fn expect_slots(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    container: BlockPos,
) -> Vec<(usize, Option<ItemStack>)> {
    expect(events, |event| match event {
        PlayerEvent::ContainerSlots { pos, slots, .. } if pos == container => Some(slots),
        _ => None,
    })
    .await
}

/// The slot updates of two containers, which come from two regions and so
/// in either order.
async fn expect_both(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    first: BlockPos,
    second: BlockPos,
) -> [Vec<(usize, Option<ItemStack>)>; 2] {
    let mut found = [None, None];
    while found.iter().any(Option::is_none) {
        let (pos, slots) = expect(events, |event| match event {
            PlayerEvent::ContainerSlots { pos, slots, .. } if pos == first || pos == second => {
                Some((pos, slots))
            }
            _ => None,
        })
        .await;
        found[usize::from(pos == second)] = Some(slots);
    }
    found.map(Option::unwrap)
}

async fn expect_vote(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> bool {
    expect(events, |event| match event {
        PlayerEvent::TransactionPrepared { prepared, .. } => Some(prepared),
        _ => None,
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_double_chest_across_a_region_border_opens_as_one_menu() {
    let (alice, mut alice_events) = PlayerHandle::new(100);
    let (bob, mut bob_events) = PlayerHandle::new(101);
    let split = split_chest(&mut [(&alice, &mut alice_events), (&bob, &mut bob_events)]).await;
    let [right, left] = right_first(&split);

    split
        .west
        .request(RegionRequest::Place(place(
            &alice,
            split.west_pos,
            Item::Torch,
            1,
        )))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::Generic { rows: 6 });
    assert_eq!(
        open.title,
        TextComponent::translate("container.chestDouble", [])
    );
    let (parts, contents, remote) = blocks(open);
    assert_eq!(parts, vec![(right, 27), (left, 27)]);
    assert_eq!(
        contents,
        (0..54).map(|_| None).collect::<Vec<Option<ItemStack>>>()
    );
    assert_eq!(remote.map(|(pos, _)| pos), Some(split.east_pos));

    split
        .east
        .request(RegionRequest::Place(place(
            &bob,
            split.east_pos,
            Item::Torch,
            2,
        )))
        .await;
    let (parts, _, remote) = blocks(expect(&mut bob_events, container_menu).await);
    assert_eq!(parts, vec![(right, 27), (left, 27)]);
    assert_eq!(remote.map(|(pos, _)| pos), Some(split.west_pos));

    let apple = ItemStack::mint(Item::Apple, 3, MintReason::Test);
    split
        .west
        .request(RegionRequest::ContainerTransaction {
            player: alice.clone(),
            window_id: 1,
            changes: set_slot(split.west_pos, 4, None, Some(apple.dup_unchecked())),
            took_result: false,
        })
        .await;
    assert!(expect(&mut alice_events, transaction_answer).await.0);
    assert_eq!(
        expect_slots(&mut bob_events, split.west_pos).await,
        vec![(4, Some(apple.dup_unchecked()))],
        "a viewer who opened the other half sees the change"
    );

    split
        .east
        .request(RegionRequest::RefreshContainer {
            player: bob.clone(),
            pos: split.east_pos,
        })
        .await;
    assert_eq!(
        expect_slots(&mut bob_events, split.east_pos).await.len(),
        27
    );

    split
        .east
        .request(RegionRequest::CloseContainer {
            player: bob.id,
            positions: vec![split.east_pos],
        })
        .await;
    split
        .west
        .request(RegionRequest::CloseContainer {
            player: bob.id,
            positions: vec![split.west_pos],
        })
        .await;
    split
        .east
        .request(RegionRequest::Break {
            player: alice.clone(),
            pos: split.east_pos,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 3,
        })
        .await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ContainerClosed { pos, .. } if pos == split.east_pos => Some(()),
        _ => None,
    })
    .await;
    split.world.shutdown().await;
    drop(split.dir);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transactions_over_both_halves_commit_or_abort_together() {
    let (alice, mut alice_events) = PlayerHandle::new(100);
    let (bob, mut bob_events) = PlayerHandle::new(101);
    let split = split_chest(&mut [(&alice, &mut alice_events), (&bob, &mut bob_events)]).await;
    for (player, events) in [(&alice, &mut alice_events), (&bob, &mut bob_events)] {
        split
            .west
            .request(RegionRequest::Place(place(
                player,
                split.west_pos,
                Item::Torch,
                1,
            )))
            .await;
        expect(events, container_menu).await;
    }

    let stone = ItemStack::mint(Item::Stone, 8, MintReason::Test);
    split
        .west
        .request(RegionRequest::ContainerTransaction {
            player: alice.clone(),
            window_id: 1,
            changes: set_slot(split.west_pos, 0, None, Some(stone.dup_unchecked())),
            took_result: false,
        })
        .await;
    assert!(expect(&mut alice_events, transaction_answer).await.0);

    let moving = |txn| {
        [
            (
                split.west.clone(),
                RegionRequest::PrepareTransaction {
                    player: alice.clone(),
                    window_id: 1,
                    txn,
                    changes: set_slot(split.west_pos, 0, Some(stone.dup_unchecked()), None),
                },
            ),
            (
                split.east.clone(),
                RegionRequest::PrepareTransaction {
                    player: alice.clone(),
                    window_id: 1,
                    txn,
                    changes: set_slot(split.east_pos, 0, None, Some(stone.dup_unchecked())),
                },
            ),
        ]
    };

    for (region, request) in moving(1) {
        region.request(request).await;
    }
    assert!(expect_vote(&mut alice_events).await);
    assert!(expect_vote(&mut alice_events).await);

    split
        .west
        .request(RegionRequest::ContainerTransaction {
            player: bob.clone(),
            window_id: 1,
            changes: set_slot(split.west_pos, 0, Some(stone.dup_unchecked()), None),
            took_result: false,
        })
        .await;
    assert!(
        !expect(&mut bob_events, transaction_answer).await.0,
        "prepared slots are locked"
    );

    for region in [&split.west, &split.east] {
        region
            .request(RegionRequest::ResolveTransaction {
                player: alice.id,
                txn: 1,
                commit: true,
            })
            .await;
    }
    assert_eq!(
        expect_both(&mut bob_events, split.west_pos, split.east_pos).await,
        [vec![(0, None)], vec![(0, Some(stone.dup_unchecked()))]]
    );

    for (region, request) in moving(2) {
        region.request(request).await;
    }
    let votes = [
        expect_vote(&mut alice_events).await,
        expect_vote(&mut alice_events).await,
    ];
    assert_eq!(
        votes.iter().filter(|prepared| **prepared).count(),
        0,
        "stale expectations refuse on both sides"
    );
    for region in [&split.west, &split.east] {
        region
            .request(RegionRequest::ResolveTransaction {
                player: alice.id,
                txn: 2,
                commit: false,
            })
            .await;
    }

    split
        .east
        .request(RegionRequest::PrepareTransaction {
            player: alice.clone(),
            window_id: 1,
            txn: 3,
            changes: set_slot(split.east_pos, 0, Some(stone.dup_unchecked()), None),
        })
        .await;
    assert!(expect_vote(&mut alice_events).await);
    split
        .east
        .request(RegionRequest::ResolveTransaction {
            player: alice.id,
            txn: 3,
            commit: false,
        })
        .await;
    split
        .east
        .request(RegionRequest::ContainerTransaction {
            player: bob.clone(),
            window_id: 1,
            changes: set_slot(split.east_pos, 1, None, Some(stone.dup_unchecked())),
            took_result: false,
        })
        .await;
    assert!(
        expect(&mut bob_events, transaction_answer).await.0,
        "an abort unlocks and changes nothing"
    );
    split
        .east
        .request(RegionRequest::RefreshContainer {
            player: bob.clone(),
            pos: split.east_pos,
        })
        .await;
    let slots = expect(&mut bob_events, |event| match event {
        PlayerEvent::ContainerSlots { pos, slots, .. }
            if pos == split.east_pos && slots.len() == 27 =>
        {
            Some(slots)
        }
        _ => None,
    })
    .await;
    assert_eq!(slots[0], (0, Some(stone.dup_unchecked())));
    split.world.shutdown().await;
    drop(split.dir);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_moves_items_between_halves_in_different_regions() {
    let (bob, mut bob_events) = PlayerHandle::new(101);
    let (outbound, _outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(200);
    let (_packets, packets_rx) = mpsc::channel(8);
    let profile = crate::auth::GameProfile {
        id: 200,
        name: "Mover".to_owned(),
        properties: Vec::new(),
    };
    let split = split_chest(&mut [(&bob, &mut bob_events)]).await;
    let mut session = crate::world::session::PlayerSession::new(
        split.world.shared.clone(),
        200,
        profile,
        crate::protocol::configuration::ClientInformation::default(),
        outbound,
        handle.clone(),
        events,
        packets_rx,
        tuff_storage::admin::PermissionLevel::All,
    );
    split
        .west
        .request(RegionRequest::Place(place(
            &bob,
            split.west_pos,
            Item::Torch,
            1,
        )))
        .await;
    expect(&mut bob_events, container_menu).await;
    split
        .east
        .request(RegionRequest::Place(place(
            &handle,
            split.east_pos,
            Item::Torch,
            1,
        )))
        .await;
    let open = expect(&mut session.conn.events, container_menu).await;
    session.on_event(PlayerEvent::MenuOpen(open)).await;
    assert!(
        session
            .menus
            .menu
            .as_ref()
            .is_some_and(|m| m.remote.is_some())
    );

    let gold = ItemStack::mint(Item::GoldIngot, 4, MintReason::Test);
    let [right, left] = right_first(&split);
    let right_slot = 2;
    let left_slot = 27 + 5;
    let set = |session: &mut crate::world::session::PlayerSession, slot, stack| {
        let before = session
            .menus
            .menu
            .as_ref()
            .unwrap()
            .container
            .dup_unchecked();
        session.menus.menu.as_mut().unwrap().container[slot] = stack;
        before
    };

    let before = set(&mut session, right_slot, Some(gold.dup_unchecked()));
    let inventory = session.inventory.clone();
    session
        .submit_container_changes(before, inventory, Vec::new(), false)
        .await;
    assert_eq!(
        expect_slots(&mut bob_events, right).await,
        vec![(right_slot, Some(gold.dup_unchecked()))]
    );
    let answered = expect(&mut session.conn.events, |event| match event {
        event @ PlayerEvent::Transaction { .. } => Some(event),
        _ => None,
    })
    .await;
    session.on_event(answered).await;

    let before = {
        let menu = session.menus.menu.as_mut().unwrap();
        let before = menu.container.dup_unchecked();
        menu.container[right_slot] = None;
        menu.container[left_slot] = Some(gold.dup_unchecked());
        before
    };
    let inventory = session.inventory.clone();
    session
        .submit_container_changes(before, inventory, Vec::new(), false)
        .await;
    while session
        .menus
        .menu
        .as_ref()
        .is_some_and(|m| m.pending.is_some())
    {
        let event = expect(&mut session.conn.events, |event| match event {
            event @ PlayerEvent::TransactionPrepared { .. } => Some(event),
            _ => None,
        })
        .await;
        session.on_event(event).await;
    }
    assert_eq!(
        expect_both(&mut bob_events, right, left).await,
        [
            vec![(right_slot, None)],
            vec![(5, Some(gold.dup_unchecked()))]
        ]
    );

    split
        .east
        .request(RegionRequest::ContainerTransaction {
            player: bob.clone(),
            window_id: 1,
            changes: set_slot(
                right,
                0,
                None,
                Some(ItemStack::mint(Item::Dirt, 1, MintReason::Test)),
            ),
            took_result: false,
        })
        .await;
    assert!(expect(&mut bob_events, transaction_answer).await.0);
    let before = {
        let menu = session.menus.menu.as_mut().unwrap();
        let before = menu.container.dup_unchecked();
        menu.container[0] = Some(gold.dup_unchecked());
        menu.container[left_slot] = None;
        before
    };
    let inventory = session.inventory.clone();
    session
        .submit_container_changes(before, inventory, Vec::new(), false)
        .await;
    while session
        .menus
        .menu
        .as_ref()
        .is_some_and(|m| m.pending.is_some())
    {
        let event = expect(&mut session.conn.events, |event| match event {
            event @ (PlayerEvent::TransactionPrepared { .. }
            | PlayerEvent::ContainerSlots { .. }) => Some(event),
            _ => None,
        })
        .await;
        session.on_event(event).await;
    }
    let menu = session.menus.menu.as_ref().unwrap();
    assert_eq!(
        menu.container[0],
        Some(ItemStack::mint(Item::Dirt, 1, MintReason::Test)),
        "a refused half restores the menu from both regions"
    );
    assert_eq!(menu.container[left_slot], Some(gold.dup_unchecked()));
    split
        .west
        .request(RegionRequest::RefreshContainer {
            player: bob.clone(),
            pos: left,
        })
        .await;
    let slots = expect(&mut bob_events, |event| match event {
        PlayerEvent::ContainerSlots { pos, slots, .. } if pos == left && slots.len() == 27 => {
            Some(slots)
        }
        _ => None,
    })
    .await;
    assert_eq!(slots[5], (5, Some(gold)), "the prepared half rolled back");
    split.world.shutdown().await;
    drop(split.dir);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_a_half_during_a_prepared_transaction_neither_dupes_nor_loses() {
    async fn dropped_items(region: &RegionHandle) -> usize {
        let (reply, answer) = oneshot::channel();
        region.request(RegionRequest::Entities { reply }).await;
        answer
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind == tuff_vanilla_data::registry::EntityType::Item)
            .count()
    }

    let (alice, mut alice_events) = PlayerHandle::new(100);
    let stone = ItemStack::mint(Item::Stone, 8, MintReason::Test);
    for commit in [true, false] {
        let split = split_chest(&mut [(&alice, &mut alice_events)]).await;
        split
            .west
            .request(RegionRequest::Place(place(
                &alice,
                split.west_pos,
                Item::Torch,
                1,
            )))
            .await;
        expect(&mut alice_events, container_menu).await;
        split
            .west
            .request(RegionRequest::ContainerTransaction {
                player: alice.clone(),
                window_id: 1,
                changes: set_slot(split.west_pos, 0, None, Some(stone.dup_unchecked())),
                took_result: false,
            })
            .await;
        assert!(expect(&mut alice_events, transaction_answer).await.0);
        split
            .west
            .request(RegionRequest::PrepareTransaction {
                player: alice.clone(),
                window_id: 1,
                txn: 1,
                changes: set_slot(split.west_pos, 0, Some(stone.dup_unchecked()), None),
            })
            .await;
        assert!(expect_vote(&mut alice_events).await);
        split
            .west
            .request(RegionRequest::Break {
                player: alice.clone(),
                pos: split.west_pos,
                tool: None,
                mode: BreakMode::Finish,
                sequence: 2,
            })
            .await;
        split.world.settle(2).await;
        split
            .west
            .request(RegionRequest::ResolveTransaction {
                player: alice.id,
                txn: 1,
                commit,
            })
            .await;
        split.world.settle(2).await;
        let after = dropped_items(&split.west).await;
        assert_eq!(
            after,
            1 + usize::from(!commit),
            "the chest item, plus the stone only if the transaction aborted (commit: {commit})"
        );
        split.world.shutdown().await;
        drop(split.dir);
    }
}

async fn stock(
    split: &Split,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    pos: BlockPos,
    stack: ItemStack,
    sequence: i32,
) {
    let region = if pos.x < split.east_pos.x {
        &split.west
    } else {
        &split.east
    };
    region
        .request(RegionRequest::Place(place(
            player,
            pos,
            Item::Stick,
            sequence,
        )))
        .await;
    let open = expect(events, container_menu).await;
    blocks(open);
    region
        .request(RegionRequest::ContainerTransaction {
            player: player.clone(),
            window_id: 1,
            changes: vec![ContainerChange {
                pos,
                slot: 0,
                expected: None,
                new: Some(stack),
            }],
            took_result: false,
        })
        .await;
    assert!(expect(events, transaction_answer).await.0);
    region
        .request(RegionRequest::CloseContainer {
            player: player.id,
            positions: vec![pos],
        })
        .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_comparator_reads_both_halves_across_the_border() {
    let (alice, mut alice_events) = PlayerHandle::new(100);
    let split = split_chest(&mut [(&alice, &mut alice_events)]).await;
    view_around(
        &split.world,
        &alice,
        &mut alice_events,
        ChunkPos::new(32, 0),
    )
    .await;
    let comparator = BlockPos::new(split.east_pos.x + 1, split.east_pos.y, split.east_pos.z);
    let floor = BlockPos::new(comparator.x, comparator.y - 1, comparator.z);
    let powered = Block::Comparator
        .default_state()
        .with_facing(Facing::West)
        .unwrap();
    place_blocks(
        &split.world,
        &split.east,
        [(floor, Block::Stone.default_state()), (comparator, powered)],
    )
    .await;
    stock(
        &split,
        &alice,
        &mut alice_events,
        split.west_pos,
        ItemStack::mint(Item::Stone, 64, MintReason::Test),
        1,
    )
    .await;
    for _ in 0..20 {
        split.world.ticks(1).await;
        if block_at(&split.east, comparator).await.unwrap().powered() == Some(true) {
            split.world.shutdown().await;
            return;
        }
    }
    panic!("the far half's items never powered the comparator");
}

async fn hopper_setup(
    split: &Split,
    alice: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    hopper: BlockPos,
    items: Option<ItemStack>,
) {
    place_blocks(
        &split.world,
        &split.west,
        [(hopper, Block::Hopper.default_state())],
    )
    .await;
    if let Some(items) = items {
        stock(split, alice, events, hopper, items, 7).await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hopper_fills_the_far_half() {
    let (alice, mut events) = PlayerHandle::new(100);
    let split = split_chest(&mut [(&alice, &mut events)]).await;
    stock(
        &split,
        &alice,
        &mut events,
        split.east_pos,
        ItemStack::mint(Item::Apple, 1, MintReason::Test),
        1,
    )
    .await;
    split.world.ticks(3).await;
    let above = BlockPos::new(split.west_pos.x, split.west_pos.y + 1, split.west_pos.z);
    hopper_setup(
        &split,
        &alice,
        &mut events,
        above,
        Some(ItemStack::mint(Item::Apple, 2, MintReason::Test)),
    )
    .await;
    split.world.ticks(40).await;
    split
        .east
        .request(RegionRequest::Place(place(
            &alice,
            split.east_pos,
            Item::Torch,
            2,
        )))
        .await;
    let (_, contents, _) = blocks(expect(&mut events, container_menu).await);
    assert_eq!(
        contents[0],
        Some(ItemStack::mint(Item::Apple, 3, MintReason::Test))
    );
    split.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hopper_pulls_from_the_far_half() {
    let (alice, mut events) = PlayerHandle::new(100);
    let split = split_chest(&mut [(&alice, &mut events)]).await;
    stock(
        &split,
        &alice,
        &mut events,
        split.east_pos,
        ItemStack::mint(Item::Apple, 2, MintReason::Test),
        1,
    )
    .await;
    split.world.ticks(3).await;
    let below = BlockPos::new(split.west_pos.x, split.west_pos.y - 1, split.west_pos.z);
    hopper_setup(&split, &alice, &mut events, below, None).await;
    split.world.ticks(40).await;
    split
        .west
        .request(RegionRequest::Place(place(&alice, below, Item::Torch, 2)))
        .await;
    let open = expect(&mut events, container_menu).await;
    let MenuSource::Blocks { contents, .. } = open.source else {
        panic!("a hopper is a block container");
    };
    assert_eq!(
        contents[0],
        Some(ItemStack::mint(Item::Apple, 2, MintReason::Test))
    );
    split.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_offhand_swap_during_a_refused_take_waits_for_the_answer() {
    let (bob, mut bob_events) = PlayerHandle::new(101);
    let (outbound, _outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(200);
    let (_packets, packets_rx) = mpsc::channel(8);
    let profile = crate::auth::GameProfile {
        id: 200,
        name: "Mover".to_owned(),
        properties: Vec::new(),
    };
    let split = split_chest(&mut [(&bob, &mut bob_events)]).await;
    let mut session = crate::world::session::PlayerSession::new(
        split.world.shared.clone(),
        200,
        profile,
        crate::protocol::configuration::ClientInformation::default(),
        outbound,
        handle.clone(),
        events,
        packets_rx,
        tuff_storage::admin::PermissionLevel::All,
    );
    split
        .west
        .request(RegionRequest::Place(place(
            &bob,
            split.west_pos,
            Item::Torch,
            1,
        )))
        .await;
    expect(&mut bob_events, container_menu).await;
    split
        .east
        .request(RegionRequest::Place(place(
            &handle,
            split.east_pos,
            Item::Torch,
            1,
        )))
        .await;
    let open = expect(&mut session.conn.events, container_menu).await;
    session.on_event(PlayerEvent::MenuOpen(open)).await;

    let gold = ItemStack::mint(Item::GoldIngot, 4, MintReason::Test);
    let right = split.east_pos;
    split
        .east
        .request(RegionRequest::ContainerTransaction {
            player: bob.clone(),
            window_id: 1,
            changes: set_slot(right, 2, None, Some(gold.dup_unchecked())),
            took_result: false,
        })
        .await;
    assert!(expect(&mut bob_events, transaction_answer).await.0);
    session.menus.menu.as_mut().unwrap().container[2] = Some(gold.dup_unchecked());

    split
        .east
        .request(RegionRequest::ContainerTransaction {
            player: bob.clone(),
            window_id: 1,
            changes: set_slot(right, 2, Some(gold.dup_unchecked()), None),
            took_result: false,
        })
        .await;
    assert!(
        expect(&mut bob_events, transaction_answer).await.0,
        "bob holds the gold"
    );

    let before = session
        .menus
        .menu
        .as_ref()
        .unwrap()
        .container
        .dup_unchecked();
    let inventory_before = session.inventory.clone();
    session.menus.menu.as_mut().unwrap().container[2] = None;
    session.inventory.set_slot(36, Some(gold.dup_unchecked()));
    session
        .submit_container_changes(before, inventory_before, Vec::new(), false)
        .await;
    assert!(session.menus.menu.as_ref().unwrap().pending.is_some());
    session
        .on_packet(crate::protocol::play::Serverbound::PlayerAction {
            status: crate::protocol::play::PlayerActionStatus::SwapItemWithOffhand,
            position: BlockPos::new(0, 0, 0),
            face: 0,
            sequence: 0,
        })
        .await;
    while session
        .menus
        .menu
        .as_ref()
        .is_some_and(|m| m.pending.is_some())
    {
        let event = expect(&mut session.conn.events, |event| match event {
            event @ (PlayerEvent::Transaction { .. } | PlayerEvent::TransactionPrepared { .. }) => {
                Some(event)
            }
            _ => None,
        })
        .await;
        session.on_event(event).await;
    }
    session.replay_deferred().await;
    let slots = session.inventory.slots();
    assert_eq!(slots[36], None, "the refusal emptied the hand");
    assert_eq!(
        slots[crate::world::inventory::Inventory::OFFHAND_SLOT],
        None,
        "the swap ran after the refusal, so no gold reached the off hand"
    );
    split.world.shutdown().await;
    drop(split.dir);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_world_save_snapshots_the_player_only_between_transactions() {
    use crate::world::messages::SaveBarrier;
    let (bob, mut bob_events) = PlayerHandle::new(101);
    let (outbound, _outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(200);
    let (_packets, packets_rx) = mpsc::channel(8);
    let profile = crate::auth::GameProfile {
        id: 200,
        name: "Mover".to_owned(),
        properties: Vec::new(),
    };
    let split = split_chest(&mut [(&bob, &mut bob_events)]).await;
    let mut session = crate::world::session::PlayerSession::new(
        split.world.shared.clone(),
        200,
        profile,
        crate::protocol::configuration::ClientInformation::default(),
        outbound,
        handle.clone(),
        events,
        packets_rx,
        tuff_storage::admin::PermissionLevel::All,
    );
    split
        .east
        .request(RegionRequest::Place(place(
            &handle,
            split.east_pos,
            Item::Torch,
            1,
        )))
        .await;
    let open = expect(&mut session.conn.events, container_menu).await;
    session.on_event(PlayerEvent::MenuOpen(open)).await;
    let gold = ItemStack::mint(Item::GoldIngot, 4, MintReason::Test);
    session.menus.menu.as_mut().unwrap().container[2] = Some(gold.dup_unchecked());

    let before = session
        .menus
        .menu
        .as_ref()
        .unwrap()
        .container
        .dup_unchecked();
    let inventory_before = session.inventory.clone();
    session.menus.menu.as_mut().unwrap().container[2] = None;
    session.inventory.set_slot(36, Some(gold.dup_unchecked()));
    session
        .submit_container_changes(before, inventory_before, Vec::new(), false)
        .await;
    assert!(session.inventory_busy());

    let (snapshotted, mut snapshot) = oneshot::channel();
    let (release, released) = oneshot::channel();
    session
        .on_event(PlayerEvent::Save(SaveBarrier {
            snapshotted,
            release: released,
            written: None,
        }))
        .await;
    assert!(
        snapshot.try_recv().is_err(),
        "no snapshot while a transaction is in flight"
    );

    while session.inventory_busy() {
        let event = expect(&mut session.conn.events, |event| match event {
            event @ (PlayerEvent::Transaction { .. } | PlayerEvent::TransactionPrepared { .. }) => {
                Some(event)
            }
            _ => None,
        })
        .await;
        session.on_event(event).await;
    }
    assert!(
        snapshot.try_recv().is_err(),
        "the answer alone does not save"
    );

    let settle = session.settle_save_barrier();
    tokio::pin!(settle);
    tokio::select! {
        _ = &mut settle => panic!("the session must hold still until released"),
        snapped = &mut snapshot => snapped.unwrap(),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut settle)
            .await
            .is_err(),
        "still holding"
    );
    drop(release);
    tokio::time::timeout(Duration::from_secs(2), &mut settle)
        .await
        .expect("released");
    split.world.shutdown().await;
    drop(split.dir);
}
