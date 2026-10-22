//! A cross-actor item transfer whose conservation check fails is refused:
//! nothing moves, the player is disconnected, the region is quarantined,
//! and after a restart the items are where the last consistent save put
//! them.

use super::*;
use crate::world::conservation::Subject;
use crate::world::messages::MenuSource;

fn journaled(dir: &Path) -> Arc<dyn WorldBackend> {
    tuff_core::storage::journaled(Storage::default().open(dir).unwrap(), dir).unwrap()
}

async fn save(world: &TestWorld) {
    let (done, saved) = oneshot::channel();
    world
        .shared
        .coordinator
        .send(CoordinatorMessage::SaveAll { flush: true, done })
        .await
        .unwrap();
    saved.await.unwrap().unwrap();
}

fn chest_pos() -> BlockPos {
    BlockPos::new(1, Generator::new(1).height(1, 1) + 1, 1)
}

/// Opens the chest for `viewer` and counts the items in it.
async fn items_in_chest(world: &TestWorld, viewer: i32) -> u32 {
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(viewer);
    subscribe(&region, &player, &mut events, &[pos]).await;
    region
        .request(RegionRequest::Place(place(
            &player,
            chest_pos(),
            Item::Torch,
            1,
        )))
        .await;
    let open = expect(&mut events, container_menu).await;
    let MenuSource::Blocks { contents, .. } = open.source else {
        panic!("a chest is a block container");
    };
    contents
        .iter()
        .flatten()
        .map(|stack| u32::from(stack.count()))
        .sum()
}

async fn take_stone(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    stone: &ItemStack,
) -> bool {
    region
        .request(RegionRequest::ContainerTransaction {
            player: player.clone(),
            window_id: 1,
            changes: set_slot(chest_pos(), 0, Some(stone.test_copy()), None),
            pool: Vec::new(),
            took_result: false,
        })
        .await;
    expect(events, transaction_answer).await.0
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transfer_whose_check_fails_moves_nothing_and_the_region_keeps_its_last_save() {
    let dir = TestDir::new();
    let stone = ItemStack::mint(Item::Stone, 5, MintReason::Test);
    {
        let world = TestWorld::start_with(journaled(dir.path()));
        let pos = ChunkPos::new(0, 0);
        let region = world.region(pos.region()).await;
        let (alice, mut events) = PlayerHandle::new(100);
        subscribe(&region, &alice, &mut events, &[pos]).await;
        place_blocks(
            &world,
            &region,
            [(chest_pos(), Block::Chest.default_state())],
        )
        .await;
        region
            .request(RegionRequest::Place(place(
                &alice,
                chest_pos(),
                Item::Torch,
                1,
            )))
            .await;
        expect(&mut events, container_menu).await;
        let put = set_slot(chest_pos(), 0, None, Some(stone.test_copy()));
        region
            .request(RegionRequest::ContainerTransaction {
                player: alice.clone(),
                window_id: 1,
                pool: pool_for(&put),
                changes: put,
                took_result: false,
            })
            .await;
        assert!(expect(&mut events, transaction_answer).await.0);
        save(&world).await;

        world.shared.conservation.fail_next_check();
        region
            .request(RegionRequest::ContainerTransaction {
                player: alice.clone(),
                window_id: 1,
                changes: set_slot(chest_pos(), 0, Some(stone.test_copy()), None),
                pool: Vec::new(),
                took_result: false,
            })
            .await;
        expect(&mut events, |event| match event {
            PlayerEvent::Kick(_) => Some(()),
            _ => None,
        })
        .await;
        let (accepted, contents) = expect(&mut events, transaction_answer).await;
        assert!(!accepted, "the transfer whose check failed is refused");
        assert_eq!(stacks(&contents[0].1)[0], Some(stone.test_copy()));
        assert!(
            world
                .shared
                .conservation
                .quarantined(Subject::Player(alice.id))
        );
        assert!(
            world
                .shared
                .conservation
                .quarantined(Subject::Region(world.dimension(OVERWORLD), pos.region()))
        );
        assert!(
            !take_stone(&region, &alice, &mut events, &stone).await,
            "a quarantined region trades no items"
        );
        assert_eq!(items_in_chest(&world, 101).await, 5);
        save(&world).await;
        world.shutdown().await;
    }
    let world = TestWorld::start_with(journaled(dir.path()));
    assert_eq!(items_in_chest(&world, 102).await, 5, "no item made or lost");
    world.shutdown().await;
}
