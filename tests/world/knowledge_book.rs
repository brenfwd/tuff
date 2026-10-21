//! Using a knowledge book: it is consumed and teaches every recipe it lists.

use super::*;
use crate::world::inventory::Inventory;
use crate::world::session::PlayerSession;
use tuff_nbt::Tag;

fn book(recipes: &[&str]) -> ItemStack {
    let mut stack = ItemStack::mint(Item::KnowledgeBook, 1, MintReason::Test);
    stack.components.other.insert(
        "minecraft:recipes".to_owned(),
        Tag::List(
            recipes
                .iter()
                .map(|name| Tag::String((*name).to_owned()))
                .collect(),
        ),
    );
    stack
}

async fn read(player: &mut PlayerSession, stack: ItemStack) -> Option<ItemStack> {
    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(stack));
    player
        .handle_packet(crate::protocol::play::Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    player.inventory_mut().slots()[Inventory::hotbar_slot(0)].dup_unchecked()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_knowledge_book_teaches_its_recipes_and_is_consumed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, outbound) = session_with_outbound(&world, 1000);
    std::mem::forget(outbound);
    player.stand_at([0.5, 100.0, 0.5], true);

    let left = read(&mut player, book(&["minecraft:piston", "minecraft:torch"])).await;
    assert!(left.is_none(), "the book is consumed");
    assert!(player.knows_recipe("minecraft:piston"));
    assert!(player.knows_recipe("minecraft:torch"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_or_invalid_knowledge_book_teaches_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, outbound) = session_with_outbound(&world, 1000);
    std::mem::forget(outbound);
    player.stand_at([0.5, 100.0, 0.5], true);

    read(&mut player, book(&[])).await;
    read(
        &mut player,
        book(&["minecraft:piston", "minecraft:nonsense"]),
    )
    .await;
    assert!(!player.knows_recipe("minecraft:piston"));
    world.shutdown().await;
}
