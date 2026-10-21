//! Goat horns: using one starts a seven second cooldown.

use super::*;
use crate::world::inventory::Inventory;
use tuff_feature_builtin_vanilla_items::world::session::item_use::ItemUseExt;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn using_a_goat_horn_starts_its_cooldown_and_a_second_use_is_ignored() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 78);
    let slot = Inventory::hotbar_slot(0);
    player.inventory_mut().set_slot(
        slot,
        Some(ItemStack::mint(Item::GoatHorn, 1, MintReason::Test)),
    );
    assert!(!player.on_cooldown(Item::GoatHorn));
    player.use_item(false).await;
    assert!(player.on_cooldown(Item::GoatHorn));
    player.tick += 139;
    assert!(player.on_cooldown(Item::GoatHorn));
    player.tick += 1;
    assert!(!player.on_cooldown(Item::GoatHorn));
    world.shutdown().await;
}
