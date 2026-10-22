//! Chiseled bookshelves and shelves driven through a real region: books and
//! items in and out by slot, hotbar swaps on powered shelves and the
//! power chain between neighbours.

use super::jukebox::{prop, saved, set, start, wait_prop};
use super::*;
use crate::world::inventory::Inventory;
use crate::world::messages::{HandEffect, PlaceOffer};
use tuff_nbt::Tag;
use tuff_vanilla_data::BlockState;

const X: i32 = 84;
const Y: i32 = 200;
const Z: i32 = 84;

fn state(text: &str) -> BlockState {
    text.parse().unwrap()
}

fn click(
    player: &PlayerHandle,
    target: BlockPos,
    face: tuff_world_model::Face,
    cursor: [f32; 3],
    held: Option<Item>,
    sequence: i32,
) -> Place {
    let mut click = place(player, target, held.unwrap_or(Item::Stick), sequence);
    click.item = None;
    click.held = held;
    click.held_count = u8::from(held.is_some());
    click.face = face;
    click.cursor = cursor;
    click.offer = offer(held.into_iter().map(|item| (0, item)));
    click
}

/// Moves one of each `(hotbar index, item)` into a click, as a session
/// does for a shelf.
fn offer(items: impl IntoIterator<Item = (usize, Item)>) -> PlaceOffer {
    PlaceOffer::new(
        false,
        items
            .into_iter()
            .map(|(index, item)| {
                (
                    Inventory::hotbar_slot(index),
                    tuff_item::item::ItemStack::mint(item, 1, tuff_item::item::MintReason::Test),
                )
            })
            .collect(),
    )
}

async fn returned(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> (usize, Item) {
    expect(events, |event| match event {
        PlayerEvent::HandReturned { slot, stack, .. } => Some((slot, stack.item)),
        _ => None,
    })
    .await
}

async fn outcome(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

fn slots(nbt: &Compound) -> Vec<(i8, String)> {
    nbt.get_list("Items")
        .unwrap_or(&[])
        .iter()
        .filter_map(Tag::as_compound)
        .map(|entry| {
            (
                entry.get_byte("Slot").unwrap(),
                entry.get_str("id").unwrap().to_owned(),
            )
        })
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chiseled_bookshelf_takes_a_book_in_the_clicked_slot_and_gives_it_back() {
    let (_dir, world, region, player, mut events) = start().await;
    let shelf = BlockPos::new(X, Y, Z);
    set(
        &region,
        shelf,
        state("minecraft:chiseled_bookshelf[facing=south]"),
        None,
    )
    .await;
    let south = tuff_world_model::Face::South;

    region
        .request(RegionRequest::Place(click(
            &player,
            shelf,
            south,
            [0.9, 0.1, 1.0],
            Some(Item::Book),
            1,
        )))
        .await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Used {
            item: Item::Book,
            effect: HandEffect::None,
            ..
        }
    ));
    assert_eq!(
        prop(&region, shelf, "slot_5_occupied").await.unwrap(),
        "true"
    );
    assert_eq!(
        prop(&region, shelf, "slot_0_occupied").await.unwrap(),
        "false"
    );
    let nbt = saved(&region, shelf).await;
    assert_eq!(slots(&nbt), vec![(5, "minecraft:book".to_owned())]);
    assert_eq!(nbt.get_int("last_interacted_slot"), Some(5));

    region
        .request(RegionRequest::Place(click(
            &player,
            shelf,
            tuff_world_model::Face::North,
            [0.9, 0.1, 0.0],
            Some(Item::Book),
            2,
        )))
        .await;
    assert!(
        !matches!(outcome(&mut events, 2).await, ActionOutcome::Used { .. }),
        "the back of the shelf takes no book"
    );

    region
        .request(RegionRequest::Place(click(
            &player,
            shelf,
            south,
            [0.9, 0.1, 1.0],
            None,
            3,
        )))
        .await;
    let granted = expect(&mut events, |event| match event {
        PlayerEvent::ItemGranted { stack, .. } => Some(stack),
        _ => None,
    })
    .await;
    assert!(matches!(
        outcome(&mut events, 3).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(granted.item, Item::Book);
    assert_eq!(
        prop(&region, shelf, "slot_5_occupied").await.unwrap(),
        "false"
    );
    assert!(slots(&saved(&region, shelf).await).is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unpowered_shelf_swaps_the_hand_with_the_clicked_slot() {
    let (_dir, world, region, player, mut events) = start().await;
    let shelf = BlockPos::new(X, Y, Z + 4);
    set(
        &region,
        shelf,
        state("minecraft:oak_shelf[facing=south,powered=false]"),
        None,
    )
    .await;
    let south = tuff_world_model::Face::South;

    region
        .request(RegionRequest::Place(click(
            &player,
            shelf,
            south,
            [0.5, 0.5, 1.0],
            Some(Item::Stick),
            1,
        )))
        .await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(
        slots(&saved(&region, shelf).await),
        vec![(1, "minecraft:stick".to_owned())]
    );

    region
        .request(RegionRequest::Place(click(
            &player,
            shelf,
            south,
            [0.5, 0.5, 1.0],
            Some(Item::Apple),
            2,
        )))
        .await;
    assert_eq!(
        returned(&mut events).await,
        (Inventory::hotbar_slot(0), Item::Stick)
    );
    assert!(matches!(
        outcome(&mut events, 2).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(
        slots(&saved(&region, shelf).await),
        vec![(1, "minecraft:apple".to_owned())]
    );

    region
        .request(RegionRequest::Place(click(
            &player,
            shelf,
            south,
            [0.1, 0.5, 1.0],
            None,
            3,
        )))
        .await;
    outcome(&mut events, 3).await;
    assert_eq!(
        slots(&saved(&region, shelf).await),
        vec![(1, "minecraft:apple".to_owned())],
        "an empty hand on an empty slot swaps nothing"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_powered_shelf_chain_swaps_the_hotbar_row() {
    let (_dir, world, region, player, mut events) = start().await;
    let left = BlockPos::new(X, Y, Z + 8);
    let right = BlockPos::new(X - 1, Y, Z + 8);
    for pos in [left, right] {
        set(
            &region,
            pos,
            state("minecraft:oak_shelf[facing=south,powered=false]"),
            None,
        )
        .await;
        set(
            &region,
            BlockPos::new(pos.x, pos.y - 1, pos.z),
            Block::RedstoneBlock.default_state(),
            None,
        )
        .await;
    }
    wait_prop(&world, &region, left, "powered", "true").await;
    wait_prop(&world, &region, right, "powered", "true").await;
    let parts = [
        prop(&region, left, "side_chain").await.unwrap(),
        prop(&region, right, "side_chain").await.unwrap(),
    ];
    assert!(
        parts == ["left", "right"] || parts == ["right", "left"],
        "{parts:?}"
    );

    let mut swap = click(
        &player,
        left,
        tuff_world_model::Face::South,
        [0.5, 0.5, 1.0],
        Some(Item::Stick),
        1,
    );
    swap.offer = offer([(3, Item::Apple), (8, Item::Stick)]);
    region.request(RegionRequest::Place(swap)).await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Interacted { .. }
    ));
    let all = [saved(&region, left).await, saved(&region, right).await];
    let held: Vec<String> = all.iter().flat_map(slots).map(|(_, id)| id).collect();
    assert!(held.contains(&"minecraft:apple".to_owned()), "{held:?}");
    assert!(held.contains(&"minecraft:stick".to_owned()), "{held:?}");
    world.shutdown().await;
}
