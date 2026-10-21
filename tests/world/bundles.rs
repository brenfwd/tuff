//! Bundles: filling and emptying by click, choosing a stack, and
//! throwing the contents out by holding use.

use super::*;
use crate::protocol::play::Serverbound;
use crate::world::inventory::Inventory;
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::SoundEvent;

const HOTBAR_WINDOW_SLOT: i16 = 36;

fn bundle_of(stacks: &[(Item, u8)]) -> ItemStack {
    let mut bundle = ItemStack::mint(Item::Bundle, 1, MintReason::Test);
    for (item, count) in stacks {
        bundle.edit_bundle(|contents| {
            contents.try_insert(&mut ItemStack::mint(*item, *count, MintReason::Test))
        });
    }
    bundle
}

fn contents(player: &session::PlayerSession) -> Vec<(Item, u8)> {
    player
        .inventory()
        .hand(false)
        .and_then(|stack| stack.components.bundle_contents.as_ref())
        .map(|c| c.items().iter().map(|s| (s.item, s.count())).collect())
        .unwrap_or_default()
}

fn click(slot: i16, button: i8) -> Serverbound {
    Serverbound::ContainerClick {
        window_id: 0,
        state_id: 0,
        slot,
        button,
        mode: 0,
        changed: Vec::new(),
        carried: None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicking_fills_a_bundle_and_only_the_others_hear_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;

    let (mut player, mut own) = session_with_outbound(&world, 1);
    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(bundle_of(&[])));
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(1),
        Some(ItemStack::mint(Item::Dirt, 30, MintReason::Test)),
    );
    while own.try_recv().is_ok() {}

    player.handle_packet(click(HOTBAR_WINDOW_SLOT + 1, 0)).await;
    player.handle_packet(click(HOTBAR_WINDOW_SLOT, 0)).await;
    assert_eq!(
        player
            .inventory()
            .carried()
            .and_then(|s| s.components.bundle_contents.as_ref())
            .map(|c| c.len()),
        None,
        "the dirt went into the bundle, not onto the cursor"
    );
    assert_eq!(contents(&player), vec![(Item::Dirt, 30)]);

    let insert = SoundEvent::ItemBundleInsert.id();
    let heard_by_viewer = loop {
        let packets = expect(&mut events, |event| match event {
            PlayerEvent::ChunkPackets { packets, .. } => Some(packets),
            _ => None,
        })
        .await;
        if packets
            .iter()
            .filter_map(decode_sound)
            .any(|sound| sound.sound_event == insert)
        {
            break true;
        }
    };
    assert!(heard_by_viewer);
    let heard_by_player = std::iter::from_fn(|| own.try_recv().ok())
        .filter_map(|frame| decode_sound(&frame))
        .any(|sound| sound.sound_event == insert);
    assert!(!heard_by_player);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_selected_stack_is_the_one_that_comes_out_when_used() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;

    let (mut player, _own) = session_with_outbound(&world, 1);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(bundle_of(&[(Item::Stone, 3), (Item::Dirt, 5)])),
    );
    assert_eq!(contents(&player), vec![(Item::Dirt, 5), (Item::Stone, 3)]);

    player
        .handle_packet(Serverbound::SelectBundleItem {
            slot: HOTBAR_WINDOW_SLOT as i32,
            index: 1,
        })
        .await;
    player
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    player.run_tick().await;
    assert_eq!(contents(&player), vec![(Item::Dirt, 5)]);

    for _ in 0..12 {
        player.run_tick().await;
    }
    assert_eq!(contents(&player), Vec::new());
    assert!(player.inventory().hand(false).is_some());
    world.shutdown().await;
}
