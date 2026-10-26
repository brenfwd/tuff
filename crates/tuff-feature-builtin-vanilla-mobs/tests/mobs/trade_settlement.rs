//! A trade pays at the click and settles when the merchant agrees: dying or
//! losing items in between never makes items.

use super::villagers::{PLAYER_UUID, farmer};
use super::*;
use crate::protocol::play::Serverbound;
use crate::world::inventory::Inventory;
use crate::world::menu::merchant;
use crate::world::session::PlayerSession;
use tuff_feature_builtin_vanilla_mobs::merchant_events::Opened;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::EntityType;

fn click(window_id: i32, slot: i16) -> Serverbound {
    Serverbound::ContainerClick {
        window_id,
        state_id: 0,
        slot,
        button: 0,
        mode: 0,
        changed: Vec::new(),
        carried: None,
    }
}

fn count(session: &PlayerSession, item: Item) -> u32 {
    let held = session.inventory().slots().iter().flatten();
    let menu = session.menu_container().into_iter().flatten().flatten();
    held.chain(session.inventory().carried())
        .chain(menu)
        .filter(|stack| stack.item() == item)
        .map(|stack| u32::from(stack.count()))
        .sum()
}

async fn trading(
    world: &TestWorld,
    wheat: u8,
) -> (
    PlayerSession,
    RegionHandle,
    i32,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(999);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(world, &region, pen(4, 11)).await;
    summon(
        &region,
        EntityType::Villager,
        farmer().with("NoAI", true),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(1).await;
    let track = expect(&mut events, super::track_of(EntityType::Villager)).await;
    let villager = track.entity;
    let (mut player, _outbound) = session_with_outbound(world, PLAYER_UUID as i32);
    player.stand_at([9.5, SKY as f64 + 1.0, 7.5], true);
    player.see_entity(track);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Wheat, wheat, MintReason::Test)),
    );
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(1),
        Some(ItemStack::mint(Item::Diamond, 7, MintReason::Test)),
    );
    player
        .handle_packet(Serverbound::Interact {
            entity: villager,
            off_hand: false,
            location: [0.0; 3],
            sneaking: false,
        })
        .await;
    assert!(
        player
            .handle_events_until(
                |e| matches!(e, PlayerEvent::Run(task) if task.data::<Opened>().is_some())
            )
            .await
    );
    (player, region, villager, events)
}

async fn answered(player: &mut PlayerSession) {
    assert!(
        player
            .handle_events_until(|e| matches!(e, PlayerEvent::TradeAnswered { .. }))
            .await
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dying_before_the_merchant_answers_makes_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _region, _villager, _events) = trading(&world, 40).await;
    let window = player.window_id().unwrap();
    player.handle_packet(Serverbound::SelectTrade(0)).await;
    player
        .handle_packet(click(window, merchant::RESULT as i16))
        .await;
    player.die(&damage_type::GENERIC_KILL, None).await;
    answered(&mut player).await;
    assert_eq!(count(&player, Item::Emerald), 0);
    assert_eq!(count(&player, Item::Wheat), 0);
    assert_eq!(count(&player, Item::Diamond), 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_lost_before_the_merchant_answers_are_not_made_again() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _region, _villager, _events) = trading(&world, 40).await;
    let window = player.window_id().unwrap();
    player.handle_packet(Serverbound::SelectTrade(0)).await;
    player
        .handle_packet(click(window, merchant::RESULT as i16))
        .await;
    player
        .inventory_mut()
        .slot_mut(Inventory::hotbar_slot(1))
        .take()
        .expect("diamonds")
        .burn(BurnReason::Consumed);
    answered(&mut player).await;
    assert_eq!(count(&player, Item::Diamond), 0);
    assert_eq!(count(&player, Item::Emerald), 1);
    assert_eq!(count(&player, Item::Wheat), 20);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_players_trade_payment_slots_drop_with_the_inventory() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _region, _villager, _events) = trading(&world, 40).await;
    player.handle_packet(Serverbound::SelectTrade(0)).await;
    player.die(&damage_type::GENERIC_KILL, None).await;
    assert_eq!(player.window_id(), None, "dying closes the screen");
    assert_eq!(count(&player, Item::Wheat), 0);
    world.shutdown().await;
}
