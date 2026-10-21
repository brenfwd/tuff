//! The anti-cheat item integrity checks through the real interceptor chain.

use super::*;
use crate::world::inventory::Inventory;
use crate::world::menu::MenuKind;
use crate::world::messages::{MenuOpen, MenuSource};
use crate::world::session::PlayerSession;
use tuff_core::protocol::codec::{CreativeSlot, HashedSlot};
use tuff_core::protocol::play::{GameMode, Serverbound};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_storage::admin::PermissionLevel;

const HOTBAR: i16 = 36;

fn hashed(item: Item, count: i32) -> HashedSlot {
    HashedSlot {
        item: item.id(),
        count,
        added: Vec::new(),
        removed: Vec::new(),
    }
}

fn pickup(
    window_id: i32,
    state_id: i32,
    slot: i16,
    changed: Vec<(i16, Option<HashedSlot>)>,
    carried: Option<HashedSlot>,
) -> Serverbound {
    Serverbound::ContainerClick {
        window_id,
        state_id,
        slot,
        button: 0,
        mode: 0,
        changed,
        carried,
    }
}

fn player(world: &TestWorld, id: i32) -> PlayerSession {
    let (mut session, outbound) = session_with_outbound(world, id);
    std::mem::forget(outbound);
    session.mode.permission = PermissionLevel::All;
    session
}

fn stone_in_hotbar(session: &mut PlayerSession) {
    session.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Stone, 10, MintReason::Test)),
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_click_claiming_more_items_than_it_touched_is_undone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut session = player(&world, 1);
    stone_in_hotbar(&mut session);
    let hotbar = Inventory::hotbar_slot(0);

    let state = session.inventory().state_id();
    session
        .handle_packet(pickup(
            0,
            state,
            HOTBAR,
            vec![(HOTBAR, None)],
            Some(hashed(Item::Stone, 64)),
        ))
        .await;
    assert!(!session.disconnected);
    assert_eq!(
        session.inventory().slots()[hotbar],
        Some(ItemStack::mint(Item::Stone, 10, MintReason::Test)),
        "the dupe click never ran"
    );
    assert_eq!(session.inventory().carried(), None);

    let state = session.inventory().state_id();
    session
        .handle_packet(pickup(
            0,
            state,
            HOTBAR,
            vec![(HOTBAR, None)],
            Some(hashed(Item::Stone, 10)),
        ))
        .await;
    assert_eq!(session.inventory().slots()[hotbar], None);
    assert_eq!(
        session.inventory().carried(),
        Some(&ItemStack::mint(Item::Stone, 10, MintReason::Test)),
        "an honest pickup goes through"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_click_is_not_a_dupe_and_repeated_dupes_kick() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut session = player(&world, 1);
    stone_in_hotbar(&mut session);

    let stale = session.inventory().state_id().wrapping_add(5) & 0x7FFF;
    for _ in 0..20 {
        session
            .handle_packet(pickup(
                0,
                stale,
                HOTBAR,
                vec![(HOTBAR, Some(hashed(Item::Stone, 10)))],
                Some(hashed(Item::Stone, 10)),
            ))
            .await;
    }
    assert!(
        !session.disconnected,
        "a stale view is resynced, not flagged"
    );

    let mut clicks = 0;
    while !session.disconnected && clicks < 20 {
        let state = session.inventory().state_id();
        session
            .handle_packet(pickup(
                0,
                state,
                HOTBAR,
                vec![(HOTBAR, Some(hashed(Item::Stone, 10)))],
                Some(hashed(Item::Stone, 10)),
            ))
            .await;
        clicks += 1;
    }
    assert!(session.disconnected, "a dupe client is kicked");
    assert!(clicks > 3, "one dupe click is only undone ({clicks})");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fakes_kick_verdict_is_dropped_and_it_stays_connected() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut session = player(&world, 1);
    session.fake = Some(tuff_core::world::fake::FakeSpec::new("Fakey"));
    stone_in_hotbar(&mut session);
    for _ in 0..20 {
        let state = session.inventory().state_id();
        session
            .handle_packet(pickup(
                0,
                state,
                HOTBAR,
                vec![(HOTBAR, Some(hashed(Item::Stone, 10)))],
                Some(hashed(Item::Stone, 10)),
            ))
            .await;
    }
    assert!(!session.disconnected, "a fake is never kicked");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn impossible_stack_counts_in_a_click_are_undone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut session = player(&world, 1);
    stone_in_hotbar(&mut session);
    session.mode.abilities.instabuild = true;
    let state = session.inventory().state_id();
    session
        .handle_packet(pickup(
            0,
            state,
            HOTBAR,
            vec![(HOTBAR, None)],
            Some(hashed(Item::Stone, 127)),
        ))
        .await;
    assert_eq!(
        session.inventory().slots()[Inventory::hotbar_slot(0)],
        Some(ItemStack::mint(Item::Stone, 10, MintReason::Test))
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ender_chest_click_cannot_conjure_items() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut session = player(&world, 1);
    session.menus.ender_items[0] = Some(ItemStack::mint(Item::Diamond, 5, MintReason::Test));
    let [x, y, z] = session.pose.position.map(|c| c.floor() as i32);
    let pos = BlockPos::new(x + 1, y, z);
    let region = world.region(ChunkPos::new(x >> 4, z >> 4).region()).await;
    session
        .open_menu(MenuOpen {
            dimension: session.dimension,
            region,
            kind: MenuKind::Generic { rows: 3 },
            title: TextComponent::new("Ender Chest"),
            source: MenuSource::EnderChest { pos },
            stat: None,
        })
        .await;
    let window = session.current_window();
    assert_ne!(window, 0);
    let state = session.menus.menu.as_ref().unwrap().state_id;

    session
        .handle_packet(pickup(
            window,
            state,
            0,
            vec![(0, None)],
            Some(hashed(Item::Diamond, 64)),
        ))
        .await;
    assert_eq!(
        session.menu_container().unwrap()[0],
        Some(ItemStack::mint(Item::Diamond, 5, MintReason::Test))
    );
    assert_eq!(session.inventory().carried(), None);

    let state = session.menus.menu.as_ref().unwrap().state_id;
    session
        .handle_packet(pickup(
            window,
            state,
            0,
            vec![(0, None)],
            Some(hashed(Item::Diamond, 5)),
        ))
        .await;
    assert_eq!(session.menu_container().unwrap()[0], None);
    assert_eq!(
        session.inventory().carried(),
        Some(&ItemStack::mint(Item::Diamond, 5, MintReason::Test))
    );
    world.shutdown().await;
}

fn deep(depth: usize) -> Tag {
    (0..depth).fold(Tag::Compound(Compound::new()), |inner, _| {
        Tag::Compound(Compound::new().with("a", inner))
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tampered_creative_stacks_never_reach_the_inventory() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut session = player(&world, 1);
    session.mode.game_mode = GameMode::Creative;
    session.mode.abilities.instabuild = true;
    let hotbar = Inventory::hotbar_slot(0);
    let set = |stack| Serverbound::SetCreativeModeSlot {
        slot: HOTBAR,
        stack,
    };

    let mut bomb = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    bomb.components
        .other
        .insert("minecraft:custom_data", deep(200));
    session
        .handle_packet(set(CreativeSlot::Stack(tuff_item::item::ItemSnapshot::of(
            &bomb,
        ))))
        .await;
    assert_eq!(session.inventory().slots()[hotbar], None);

    session.handle_packet(set(CreativeSlot::Invalid)).await;
    assert_eq!(session.inventory().slots()[hotbar], None);

    let mut shulker = ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test);
    shulker.components.container = Some(vec![Some(ItemStack::mint(
        Item::Dirt,
        90,
        MintReason::Test,
    ))]);
    session
        .handle_packet(set(CreativeSlot::Stack(tuff_item::item::ItemSnapshot::of(
            &shulker,
        ))))
        .await;
    assert_eq!(session.inventory().slots()[hotbar], None);

    let mut named = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    named
        .components
        .other
        .insert("minecraft:custom_data", deep(8));
    session
        .handle_packet(set(CreativeSlot::Stack(tuff_item::item::ItemSnapshot::of(
            &named.test_copy(),
        ))))
        .await;
    assert_eq!(session.inventory().slots()[hotbar], Some(named));
    assert!(!session.disconnected);
    world.shutdown().await;
}
