//! Conservation checks of hand returns, pickups, entity hand-offs and
//! plugin inventory edits: an injected failure refuses the transfer and
//! quarantines the actors involved without losing or making items.

use super::jukebox::{set, start};
use super::*;
use crate::world::conservation::{Subject, Transfer};
use crate::world::inventory::Inventory;
use crate::world::messages::PlaceOffer;
use crate::world::session::plugin_inventory::InventoryOp;
use tuff_vanilla_data::registry::EntityType;

const SKY: i32 = 150;

async fn items_in(region: &RegionHandle) -> Vec<(i32, [f64; 3])> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .into_iter()
        .filter(|entity| entity.kind == EntityType::Item)
        .map(|entity| (entity.id, entity.position))
        .collect()
}

fn kicked(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> bool {
    let mut kicked = false;
    while let Ok(event) = events.try_recv() {
        kicked |= matches!(event, PlayerEvent::Kick(_));
        assert!(
            !matches!(
                event,
                PlayerEvent::HandReturned { .. } | PlayerEvent::ItemGranted { .. }
            ),
            "a refused transfer reached the player"
        );
    }
    kicked
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hand_return_whose_check_fails_drops_the_offer_and_quarantines_both_sides() {
    let (_dir, world, region, player, mut events) = start().await;
    let shelf = BlockPos::new(84, 200, 84);
    set(
        &region,
        shelf,
        "minecraft:chiseled_bookshelf[facing=south]"
            .parse()
            .unwrap(),
        None,
    )
    .await;
    let mut click = place(&player, shelf, Item::Stick, 1);
    click.item = None;
    click.held = Some(Item::Stick);
    click.held_count = 1;
    click.face = tuff_world_model::Face::South;
    click.cursor = [0.9, 0.1, 1.0];
    click.offer = PlaceOffer::new(
        false,
        vec![(
            Inventory::hotbar_slot(0),
            ItemStack::mint(Item::Stick, 1, MintReason::Test),
        )],
    );
    let before = items_in(&region).await.len();
    world.shared.conservation.fail_next(Transfer::HandReturn);
    region.request(RegionRequest::Place(click)).await;
    world.ticks(2).await;
    assert!(kicked(&mut events), "the player reloads its inventory");
    assert_eq!(
        items_in(&region).await.len(),
        before + 1,
        "the stick stays in the world"
    );
    let conservation = &world.shared.conservation;
    assert!(conservation.quarantined(Subject::Player(player.id)));
    assert!(conservation.quarantined(Subject::Region(world.dimension(OVERWORLD), region.pos)));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pickup_whose_check_fails_leaves_the_item_on_the_ground() {
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
    let (item, offer, from) = expect(&mut events, |event| match event {
        PlayerEvent::OfferItem {
            item,
            offer,
            region,
            ..
        } => Some((item, offer, region)),
        _ => None,
    })
    .await;
    world.shared.conservation.fail_next(Transfer::Pickup);
    from.internal(RegionInternal::ItemTaken {
        item,
        offer,
        player: viewer.clone(),
        taken: 3,
    });
    world.ticks(2).await;
    assert!(kicked(&mut events), "the player is disconnected");
    assert!(
        items_in(&region).await.iter().any(|(id, _)| *id == item),
        "the stones stay on the ground"
    );
    let conservation = &world.shared.conservation;
    assert!(conservation.quarantined(Subject::Player(viewer.id)));
    assert!(conservation.quarantined(Subject::Region(world.dimension(OVERWORLD), region.pos)));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_entity_hand_off_whose_check_fails_keeps_the_entity_and_its_items_here() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    view_around(&world, &player, &mut events, ChunkPos::new(31, 0)).await;
    view_around(&world, &player, &mut events, ChunkPos::new(32, 0)).await;
    let west = world.region(RegionPos::new(0, 0)).await;
    let east = world.region(RegionPos::new(1, 0)).await;
    let floor: Vec<_> = (500..524)
        .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, SKY, z)))
        .map(|pos| (pos, Block::Ice.default_state()))
        .collect();
    let (west_floor, east_floor): (Vec<_>, Vec<_>) =
        floor.into_iter().partition(|(pos, _)| pos.x < 512);
    place_blocks(&world, &west, west_floor).await;
    place_blocks(&world, &east, east_floor).await;
    world.shared.conservation.fail_next(Transfer::EntityHandOff);
    west.request(RegionRequest::SpawnItem {
        stack: ItemStack::mint(Item::Dirt, 5, MintReason::Test),
        position: [511.7, f64::from(SKY + 1), 8.5],
        velocity: [0.6, 0.0, 0.0],
        pickup_delay: 40,
    })
    .await;
    world.ticks(20).await;
    assert_eq!(items_in(&west).await.len(), 1, "the dirt stays in the west");
    assert!(items_in(&east).await.is_empty(), "nothing reached the east");
    assert!(
        world
            .shared
            .conservation
            .quarantined(Subject::Region(world.dimension(OVERWORLD), west.pos))
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_edit_whose_check_fails_changes_nothing_and_quarantines_the_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session_with_outbound(&world, 1);
    session
        .apply_inventory_ops(vec![InventoryOp::Give {
            stack: tuff_item::item::ItemSnapshot::of(&ItemStack::mint(
                Item::Diamond,
                1,
                MintReason::Test,
            )),
            count: 4,
        }])
        .await;
    assert_eq!(session.held_count(Item::Diamond), 4);
    session
        .apply_inventory_ops(vec![InventoryOp::Take {
            item: Item::Diamond,
            count: 1,
            all_or_nothing: true,
        }])
        .await;
    assert_eq!(session.held_count(Item::Diamond), 3, "a checked take runs");
    world.shared.conservation.fail_next(Transfer::Plugin);
    session
        .apply_inventory_ops(vec![
            InventoryOp::Take {
                item: Item::Diamond,
                count: 2,
                all_or_nothing: true,
            },
            InventoryOp::Give {
                stack: tuff_item::item::ItemSnapshot::of(&ItemStack::mint(
                    Item::Emerald,
                    1,
                    MintReason::Test,
                )),
                count: 1,
            },
        ])
        .await;
    assert_eq!(session.held_count(Item::Diamond), 3, "nothing was taken");
    assert_eq!(session.held_count(Item::Emerald), 0, "nothing was given");
    assert!(
        world
            .shared
            .conservation
            .quarantined(Subject::Player(session.id))
    );
    world.shutdown().await;
}
