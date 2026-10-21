use super::*;
use tuff_core::world::session::PlayerSession;
use tuff_item::item::ItemStack;
use tuff_vanilla_data::Item;

fn diamonds(session: &PlayerSession) -> u32 {
    session
        .inventory
        .slots()
        .iter()
        .flatten()
        .filter(|stack| stack.item == Item::Diamond)
        .map(|stack| stack.count as u32)
        .sum()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drop_no_region_takes_goes_back_to_the_inventory() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 5);
    world.shutdown().await;
    session.throw_item(ItemStack::new(Item::Diamond, 3)).await;
    assert_eq!(diamonds(&session), 3);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pickup_granted_while_leaving_is_kept() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 5);
    session.conn.handle.send(PlayerEvent::ItemGranted {
        stack: ItemStack::new(Item::Diamond, 4),
        picked_up: true,
    });
    session.leave().await;
    assert_eq!(diamonds(&session), 4);
    assert!(
        !session.conn.handle.send(PlayerEvent::ItemGranted {
            stack: ItemStack::new(Item::Diamond, 1),
            picked_up: true,
        }),
        "a grant after leaving is refused, so the region keeps the item"
    );
    world.shutdown().await;
}
