//! Crafter menus: the result preview and disabled slots.

use super::*;

async fn put(b: &mut Bench, pos: BlockPos, slot: usize, stack: ItemStack) -> bool {
    b.region
        .request(RegionRequest::ContainerTransaction {
            player: b.player.clone(),
            window_id: 1,
            changes: vec![ContainerChange {
                pos,
                slot,
                expected: None,
                new: snap(Some(&stack)),
            }],
            pool: pool_for(&[ContainerChange {
                pos,
                slot,
                expected: None,
                new: snap(Some(&stack)),
            }]),
            took_result: false,
        })
        .await;
    expect(&mut b.events, transaction_answer).await.0
}

/// The preview the clicker's menu last got for the crafter at `pos`.
async fn preview(b: &mut Bench, pos: BlockPos) -> Option<ItemStack> {
    expect(&mut b.events, |event| match event {
        PlayerEvent::ContainerSlots { pos: at, slots, .. } if at == pos => slots
            .into_iter()
            .find(|(slot, _)| *slot == crate::world::menu::crafter::PREVIEW)
            .map(|(_, stack)| stack.as_deref().map(ItemStack::test_copy)),
        _ => None,
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_clickers_own_menu_gets_the_new_preview() {
    let dir = TestDir::new();
    let (mut b, crafter, _) = crafter_bench(dir.path()).await;
    let shown = preview(&mut b, crafter).await;
    assert_eq!(shown.map(|s| s.item()), Some(Item::OakPlanks));
    assert!(
        put(
            &mut b,
            crafter,
            1,
            ItemStack::mint(Item::Dirt, 1, MintReason::Test)
        )
        .await
    );
    assert_eq!(preview(&mut b, crafter).await, None, "no recipe any more");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_click_into_a_disabled_slot_is_refused() {
    let dir = TestDir::new();
    let (mut b, crafter, _) = crafter_bench(dir.path()).await;
    b.region
        .request(RegionRequest::CrafterSlotState {
            pos: crafter,
            slot: 4,
            enabled: false,
        })
        .await;
    assert!(
        !put(
            &mut b,
            crafter,
            4,
            ItemStack::mint(Item::OakLog, 1, MintReason::Test)
        )
        .await,
        "a client claiming a disabled slot is refused"
    );
    assert!(
        put(
            &mut b,
            crafter,
            5,
            ItemStack::mint(Item::OakLog, 1, MintReason::Test)
        )
        .await
    );
    b.world.shutdown().await;
}
