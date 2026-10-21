//! Anvil menus: taking a priced result, refusing one the player cannot
//! pay for, and the anvil wearing down when used.

use super::*;
use bytes::Bytes;
use rand::{Rng, SeedableRng, rngs::StdRng};
use tuff_core::protocol::play::Serverbound;
use tuff_core::world::menu::{MenuKind, anvil};
use tuff_core::world::messages::{MenuOpen, MenuSource};
use tuff_core::world::player::Experience;
use tuff_core::world::session::{PlayerSession, Station};
use tuff_item::item::DupUnchecked;
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::packet::play::Clientbound as Id;

const ANVIL_AT: BlockPos = BlockPos::new(0, 64, 0);

async fn open_anvil(world: &TestWorld, session: &mut PlayerSession) -> i32 {
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    session
        .open_menu(MenuOpen {
            dimension: session.dimension,
            region,
            kind: MenuKind::Anvil,
            title: TextComponent::translate("container.repair", []),
            source: MenuSource::Station {
                pos: ANVIL_AT,
                bookcases: 0,
                data: Vec::new(),
            },
            stat: None,
        })
        .await;
    session.window_id().expect("the anvil opened")
}

fn take(window_id: i32) -> Serverbound {
    Serverbound::ContainerClick {
        window_id,
        state_id: 0,
        slot: anvil::RESULT as i16,
        button: 0,
        mode: 0,
        changed: Vec::new(),
        carried: None,
    }
}

fn set_level(session: &mut PlayerSession, level: i32) {
    session.body.experience = Experience::new(level, 0.0, 0, 0);
}

fn put(session: &mut PlayerSession, input: ItemStack, addition: Option<ItemStack>) {
    let menu = session.menus.menu.as_mut().unwrap();
    menu.container[anvil::INPUT] = Some(input);
    menu.container[anvil::ADDITION] = addition;
    session.refresh_station();
}

fn cost(session: &PlayerSession) -> i32 {
    match &session.menus.menu.as_ref().unwrap().station {
        Some(Station::Anvil { outcome, .. }) => outcome.cost,
        _ => panic!("an anvil station"),
    }
}

fn data_frames(out: &mut mpsc::UnboundedReceiver<Bytes>) -> usize {
    let mut seen = 0;
    while let Ok(frame) = out.try_recv() {
        if framed_packet_id(&frame) == Id::ContainerSetData.id() {
            seen += 1;
        }
    }
    seen
}

async fn block_at(region: &RegionHandle, pos: BlockPos) -> Block {
    let (reply, rx) = oneshot::channel();
    region.request(RegionRequest::BlockAt { pos, reply }).await;
    rx.await.unwrap().expect("loaded").block()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn taking_a_repair_charges_levels_and_uses_up_the_inputs() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut out) = session_with_outbound(&world, 1);
    let window = open_anvil(&world, &mut session).await;
    let max = Item::IronSword.max_damage().unwrap();
    let mut sword = ItemStack::mint(Item::IronSword, 1, MintReason::Test);
    sword.set_damage(max);
    set_level(&mut session, 10);
    put(
        &mut session,
        sword,
        Some(ItemStack::mint(Item::IronIngot, 7, MintReason::Test)),
    );
    assert_eq!(cost(&session), 5);
    assert!(data_frames(&mut out) >= 1, "the cost is sent as data 0");

    session.handle_packet(take(window)).await;
    assert_eq!(session.body.experience.level, 5);
    let carried = session.inventory().carried().unwrap().dup_unchecked();
    assert_eq!((carried.item, carried.damage()), (Item::IronSword, 0));
    let container = session.menu_container().unwrap();
    assert!(container[anvil::INPUT].is_none());
    assert_eq!(container[anvil::ADDITION].as_ref().unwrap().count(), 2);
    assert!(container[anvil::RESULT].is_none());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn taking_a_rename_keeps_the_addition_and_charges_one_level() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    let window = open_anvil(&world, &mut session).await;
    set_level(&mut session, 3);
    put(
        &mut session,
        ItemStack::mint(Item::Stick, 1, MintReason::Test),
        None,
    );
    assert!(session.menu_container().unwrap()[anvil::RESULT].is_none());
    session.rename_anvil_item("Wand".to_owned());
    assert_eq!(cost(&session), 1);

    session.handle_packet(take(window)).await;
    assert_eq!(session.body.experience.level, 2);
    let carried = session.inventory().carried().unwrap().dup_unchecked();
    assert_eq!(
        carried.components.custom_name.as_ref().unwrap().plain(),
        "Wand"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_with_too_few_levels_cannot_take_the_result() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    let window = open_anvil(&world, &mut session).await;
    let max = Item::IronSword.max_damage().unwrap();
    let mut sword = ItemStack::mint(Item::IronSword, 1, MintReason::Test);
    sword.set_damage(max);
    set_level(&mut session, 2);
    put(
        &mut session,
        sword,
        Some(ItemStack::mint(Item::IronIngot, 7, MintReason::Test)),
    );
    assert_eq!(cost(&session), 5);
    let before = session
        .menu_container()
        .unwrap()
        .iter()
        .map(DupUnchecked::dup_unchecked)
        .collect::<Vec<_>>();

    session.handle_packet(take(window)).await;
    assert_eq!(session.body.experience.level, 2);
    assert!(session.inventory().carried().is_none());
    assert_eq!(session.menu_container().unwrap(), before.as_slice());
    world.shutdown().await;
}

fn seed_rolling(wears: bool) -> StdRng {
    (0u64..)
        .map(StdRng::seed_from_u64)
        .find(|rng| (rng.clone().r#gen::<f32>() < 0.12) == wears)
        .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_anvil_wears_down_stage_by_stage_and_then_breaks_when_the_roll_hits() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let (watcher, mut watcher_events) = PlayerHandle::new(100);
    subscribe(
        &region,
        &watcher,
        &mut watcher_events,
        &[ChunkPos::new(0, 0)],
    )
    .await;
    place_blocks(&world, &region, [(ANVIL_AT, Block::Anvil.default_state())]).await;
    let (mut session, _out) = session_with_outbound(&world, 1);
    let window = open_anvil(&world, &mut session).await;

    let use_once = |session: &mut PlayerSession, wears: bool| {
        session.rng = seed_rolling(wears);
        set_level(session, 5);
        put(
            session,
            ItemStack::mint(Item::Stick, 1, MintReason::Test),
            None,
        );
        session.rename_anvil_item(format!("n{}", session.tick));
    };

    use_once(&mut session, false);
    session.handle_packet(take(window)).await;
    world.ticks(2).await;
    assert_eq!(block_at(&region, ANVIL_AT).await, Block::Anvil);

    for expected in [Block::ChippedAnvil, Block::DamagedAnvil, Block::Air] {
        session.inventory_mut().set_carried(None);
        session.menus.menu.as_mut().unwrap().station = Some(Station::Anvil {
            name: None,
            outcome: tuff_core::world::anvil::AnvilOutcome::NOTHING,
        });
        use_once(&mut session, true);
        session.handle_packet(take(window)).await;
        world.ticks(2).await;
        assert_eq!(block_at(&region, ANVIL_AT).await, expected);
    }
    world.shutdown().await;
}

fn result(session: &PlayerSession) -> Option<ItemStack> {
    session.menu_container().unwrap()[anvil::RESULT].dup_unchecked()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rename_packet_names_the_result_and_adds_a_level_to_a_repair() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    let window = open_anvil(&world, &mut session).await;
    let max = Item::IronSword.max_damage().unwrap();
    let mut sword = ItemStack::mint(Item::IronSword, 1, MintReason::Test);
    sword.set_damage(max);
    set_level(&mut session, 10);
    put(
        &mut session,
        sword,
        Some(ItemStack::mint(Item::IronIngot, 7, MintReason::Test)),
    );
    let repair_only = cost(&session);
    assert!(result(&session).unwrap().components.custom_name.is_none());

    session
        .handle_packet(Serverbound::RenameItem("Bl\u{00A7}ade\u{7}".to_owned()))
        .await;
    assert_eq!(cost(&session), repair_only + 1);
    let named = result(&session).unwrap();
    assert_eq!(
        named.components.custom_name.as_ref().unwrap().plain(),
        "Blade"
    );
    assert_eq!(named.damage(), 0);

    session.handle_packet(take(window)).await;
    assert_eq!(session.body.experience.level, 10 - repair_only - 1);
    let carried = session.inventory().carried().unwrap().dup_unchecked();
    assert_eq!(
        carried.components.custom_name.as_ref().unwrap().plain(),
        "Blade"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blank_rename_packet_clears_an_existing_name_for_one_level() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    let window = open_anvil(&world, &mut session).await;
    let mut stick = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    stick.components.custom_name = Some(Box::new(TextComponent::new("Wand")));
    set_level(&mut session, 3);
    put(&mut session, stick, None);

    session
        .handle_packet(Serverbound::RenameItem("Wand".to_owned()))
        .await;
    assert!(result(&session).is_none(), "the same name changes nothing");

    session
        .handle_packet(Serverbound::RenameItem("  ".to_owned()))
        .await;
    assert_eq!(cost(&session), 1);
    assert!(result(&session).unwrap().components.custom_name.is_none());

    session.handle_packet(take(window)).await;
    assert_eq!(session.body.experience.level, 2);
    let carried = session.inventory().carried().unwrap().dup_unchecked();
    assert_eq!(carried.item, Item::Stick);
    assert!(carried.components.custom_name.is_none());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_overlong_rename_or_one_without_an_anvil_open_is_ignored() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    session
        .handle_packet(Serverbound::RenameItem("Lost".to_owned()))
        .await;
    assert!(session.window_id().is_none());

    open_anvil(&world, &mut session).await;
    set_level(&mut session, 3);
    put(
        &mut session,
        ItemStack::mint(Item::Stick, 1, MintReason::Test),
        None,
    );
    session
        .handle_packet(Serverbound::RenameItem(
            "n".repeat(tuff_core::world::anvil::MAX_NAME_LENGTH + 1),
        ))
        .await;
    assert!(result(&session).is_none());
    session
        .handle_packet(Serverbound::RenameItem(
            "n".repeat(tuff_core::world::anvil::MAX_NAME_LENGTH) + "\u{7}",
        ))
        .await;
    let named = result(&session).unwrap();
    assert_eq!(
        named.components.custom_name.as_ref().unwrap().plain(),
        "n".repeat(tuff_core::world::anvil::MAX_NAME_LENGTH)
    );
    world.shutdown().await;
}
