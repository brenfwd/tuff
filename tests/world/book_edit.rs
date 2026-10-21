//! Edit Book packets: writing a book and quill and signing it.

use super::*;
use crate::world::inventory::Inventory;
use tuff_core::protocol::play::Serverbound;

fn edit(slot: i32, pages: &[&str], title: Option<&str>) -> Serverbound {
    Serverbound::EditBook {
        slot,
        pages: pages.iter().map(|p| p.to_string()).collect(),
        title: title.map(str::to_string),
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editing_and_signing_a_book_rewrites_the_held_stack() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 77);
    let slot = Inventory::hotbar_slot(2);
    player
        .inventory_mut()
        .set_slot(slot, Some(ItemStack::new(Item::WritableBook, 1)));

    player
        .handle_packet(edit(2, &["hello", "world"], None))
        .await;
    let book = player.inventory_mut().slots()[slot].clone().unwrap();
    assert_eq!(book.item, Item::WritableBook);
    assert_eq!(book.components.writable_book.as_ref().unwrap().len(), 2);

    player
        .handle_packet(edit(2, &["hello"], Some("Title")))
        .await;
    let book = player.inventory_mut().slots()[slot].clone().unwrap();
    assert_eq!(book.item, Item::WrittenBook);
    let written = book.components.written_book.as_ref().unwrap();
    assert_eq!(written.title.raw, "Title");
    assert_eq!(written.pages.len(), 1);

    player.handle_packet(edit(2, &["again"], None)).await;
    let book = player.inventory_mut().slots()[slot].clone().unwrap();
    assert_eq!(book.item, Item::WrittenBook, "a signed book stays signed");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_for_a_slot_without_a_book_or_with_oversize_pages_is_ignored() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 78);
    let slot = Inventory::hotbar_slot(0);
    player
        .inventory_mut()
        .set_slot(slot, Some(ItemStack::new(Item::WritableBook, 1)));
    player.handle_packet(edit(1, &["x"], None)).await;
    assert!(player.inventory_mut().slots()[Inventory::hotbar_slot(1)].is_none());
    let long = "a".repeat(1025);
    player.handle_packet(edit(0, &[&long], None)).await;
    let book = player.inventory_mut().slots()[slot].clone().unwrap();
    assert!(book.components.writable_book.is_none());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn using_a_book_sends_open_book_for_that_hand_only() {
    use tuff_core::protocol::play::OpenBook;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut out) = session_with_outbound(&world, 79);
    let use_item = |off_hand| Serverbound::UseItem {
        off_hand,
        sequence: 1,
    };
    let opened = |out: &mut tokio::sync::mpsc::UnboundedReceiver<bytes::Bytes>, off: bool| {
        let expected = tuff_core::world::framed(&OpenBook { off_hand: off });
        let mut found = false;
        while let Ok(frame) = out.try_recv() {
            found |= frame == expected;
        }
        found
    };
    player.handle_packet(use_item(false)).await;
    assert!(!opened(&mut out, false));
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::new(Item::WritableBook, 1)),
    );
    player.handle_packet(use_item(false)).await;
    assert!(opened(&mut out, false));
    player.handle_packet(use_item(true)).await;
    assert!(!opened(&mut out, true));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_signed_book_resolves_its_score_pages_once() {
    use tuff_item::item::decor::{Filterable, WrittenBook};
    use tuff_nbt::{Compound, Tag};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 80);
    player
        .run_command("scoreboard objectives add pts dummy")
        .await;
    player.run_command("scoreboard players set Bob pts 7").await;
    let page = Tag::Compound(Compound::new().with(
        "score",
        Compound::new().with("name", "Bob").with("objective", "pts"),
    ));
    let mut stack = ItemStack::new(Item::WrittenBook, 1);
    stack.components.written_book = Some(Box::new(WrittenBook {
        title: Filterable::plain("T".to_owned()),
        author: "a".to_owned(),
        generation: 0,
        pages: vec![Filterable::plain(page)],
        resolved: false,
    }));
    let slot = Inventory::hotbar_slot(0);
    player.inventory_mut().set_slot(slot, Some(stack));
    player
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    let book = player.inventory_mut().slots()[slot].clone().unwrap();
    let written = book.components.written_book.as_ref().unwrap();
    assert!(written.resolved);
    let json = tuff_item::text::TextComponent::from_nbt(&written.pages[0].raw)
        .unwrap()
        .to_json();
    assert!(json.contains("\"7\"") && !json.contains("score"), "{json}");
    world.shutdown().await;
}
