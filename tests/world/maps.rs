//! Maps: making one from an empty map, saving it, drawing terrain on a map
//! in hand with its markers, and banner markers.

use super::*;
use crate::storage::codec::WorldBackendExt as _;
use bytes::Bytes;
use std::collections::HashMap;
use tuff_core::protocol::play::Serverbound;
use tuff_core::world::inventory::Inventory;
use tuff_core::world::session::hooks::SessionTick;
use tuff_feature_builtin_vanilla_maps::render::{Terrain, sample};
use tuff_feature_builtin_vanilla_maps::{MapBanner, MapService};
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::packet::play::Clientbound as Id;
use tuff_vanilla_data::registry::MapDecorationType;
use tuff_world_model::BlockPos;

const OVERWORLD: &str = "minecraft:overworld";

fn service(world: &TestWorld) -> &MapService {
    world
        .shared
        .services
        .get::<MapService>()
        .expect("the maps plugin runs")
}

fn filled(id: u32) -> ItemStack {
    let mut stack = ItemStack::mint(Item::FilledMap, 1, MintReason::Test);
    stack.components.map_id = Some(id);
    stack
}

fn use_item(sequence: i32) -> Serverbound {
    Serverbound::UseItem {
        off_hand: false,
        sequence,
    }
}

#[derive(Debug)]
struct Decoded {
    id: i32,
    scale: i8,
    locked: bool,
    markers: Option<Vec<(i32, i8, i8, i8)>>,
    patch: Option<(u8, u8, u8, u8, Vec<u8>)>,
}

fn decode(frame: &Bytes) -> Option<Decoded> {
    if framed_packet_id(frame) != Id::MapItemData.id() {
        return None;
    }
    let mut r = framed_payload(frame);
    r.read_var_int().unwrap();
    let id = r.read_var_int().unwrap();
    let scale = r.read_byte().unwrap();
    let locked = r.read_bool().unwrap();
    let markers = r.read_bool().unwrap().then(|| {
        (0..r.read_var_int().unwrap())
            .map(|_| {
                let kind = r.read_var_int().unwrap();
                let (x, y, rot) = (
                    r.read_byte().unwrap(),
                    r.read_byte().unwrap(),
                    r.read_byte().unwrap(),
                );
                assert!(!r.read_bool().unwrap(), "no names in these tests");
                (kind, x, y, rot)
            })
            .collect()
    });
    let width = r.read_byte().unwrap() as u8;
    let patch = (width > 0).then(|| {
        let height = r.read_byte().unwrap() as u8;
        let x = r.read_byte().unwrap() as u8;
        let y = r.read_byte().unwrap() as u8;
        let len = r.read_var_int().unwrap() as usize;
        (x, y, width, height, r.read_bytes(len).unwrap().to_vec())
    });
    Some(Decoded {
        id,
        scale,
        locked,
        markers,
        patch,
    })
}

async fn next_map(
    out: &mut mpsc::UnboundedReceiver<Bytes>,
    mut wanted: impl FnMut(&Decoded) -> bool,
) -> Decoded {
    tokio::time::timeout(TICK_TIMEOUT, async {
        loop {
            let frame = out.recv().await.expect("the client connection stays open");
            if let Some(map) = decode(&frame)
                && wanted(&map)
            {
                return map;
            }
        }
    })
    .await
    .expect("a matching Map Data packet arrives")
}

fn hand_map(session: &mut tuff_core::world::session::PlayerSession, id: u32) {
    let slot = Inventory::hotbar_slot(session.inventory().selected());
    session.inventory_mut().set_slot(slot, Some(filled(id)));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_map_makes_a_filled_map_centered_on_the_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    session.pose.position = [200.5, 70.0, -30.5];
    let slot = Inventory::hotbar_slot(session.inventory().selected());
    session
        .inventory_mut()
        .set_slot(slot, Some(ItemStack::mint(Item::Map, 2, MintReason::Test)));

    session.handle_packet(use_item(1)).await;
    assert_eq!(session.inventory().hand(false).unwrap().item, Item::Map);
    assert_eq!(session.inventory().hand(false).unwrap().count(), 1);
    let made = session
        .inventory()
        .slots()
        .iter()
        .flatten()
        .find(|stack| stack.item == Item::FilledMap)
        .expect("the filled map went to the inventory");
    assert_eq!(made.components.map_id, Some(0));
    let data = service(&world).data(0).await.expect("the map exists");
    assert_eq!((data.center_x, data.center_z, data.scale), (256, 0, 0));
    assert_eq!(data.dimension, OVERWORLD);
    assert!(data.tracking_position && !data.unlimited_tracking && !data.locked);
    assert!(data.colors.iter().all(|&c| c == 0));

    session.handle_packet(use_item(2)).await;
    let hand = session.inventory().hand(false).unwrap();
    assert_eq!(
        (hand.item, hand.components.map_id),
        (Item::FilledMap, Some(1)),
        "the last empty map turns into the filled one in hand"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creative_players_keep_their_empty_maps() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    session.mode.abilities.instabuild = true;
    let slot = Inventory::hotbar_slot(session.inventory().selected());
    session
        .inventory_mut()
        .set_slot(slot, Some(ItemStack::mint(Item::Map, 1, MintReason::Test)));
    session.handle_packet(use_item(1)).await;
    assert_eq!(session.inventory().hand(false).unwrap().item, Item::Map);
    assert!(
        session
            .inventory()
            .slots()
            .iter()
            .flatten()
            .any(|stack| stack.item == Item::FilledMap)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maps_and_the_id_counter_survive_a_restart() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let first = service(&world)
        .create(OVERWORLD, 10.0, 10.0, 1, true, false)
        .await
        .unwrap();
    assert_eq!(first, 0);
    let second = service(&world)
        .create("minecraft:the_nether", 500.0, -500.0, 0, false, true)
        .await
        .unwrap();
    assert_eq!(second, 1);
    let locked = service(&world)
        .derive(first, tuff_feature_builtin_vanilla_maps::Derivation::Lock)
        .await
        .unwrap();
    assert_eq!(locked, 2);
    let expected = service(&world).data(second).await.unwrap();
    world.shared.services.flush().await;
    assert!(dir.path().join("data/minecraft/maps/0.dat").exists());
    assert!(dir.path().join("data/minecraft/maps/last_id.dat").exists());
    world.shutdown().await;

    let world = TestWorld::start(dir.path());
    assert_eq!(service(&world).data(second).await, Some(expected));
    let locked_data = service(&world).data(locked).await.unwrap();
    assert!(locked_data.locked && locked_data.scale == 1);
    assert!(service(&world).data(99).await.is_none());
    let next = service(&world)
        .create(OVERWORLD, 0.0, 0.0, 0, true, false)
        .await
        .unwrap();
    assert_eq!(next, 3, "ids continue after the saved counter");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zooming_out_makes_a_blank_larger_map() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let id = service(&world)
        .create(OVERWORLD, 0.0, 0.0, 0, true, false)
        .await
        .unwrap();
    let bigger = service(&world)
        .derive(id, tuff_feature_builtin_vanilla_maps::Derivation::ZoomOut)
        .await
        .unwrap();
    let data = service(&world).data(bigger).await.unwrap();
    assert_eq!((data.scale, data.center_x, data.center_z), (1, 64, 64));
    world.shutdown().await;
}

struct Generated(HashMap<ChunkPos, tuff_world_model::chunk::Chunk>);

impl Terrain for Generated {
    fn state(&self, pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        let chunk = self.0.get(&ChunkPos::of_block(pos))?;
        Some(chunk.get(pos.x, pos.y, pos.z))
    }

    fn surface(&self, x: i32, z: i32) -> Option<i32> {
        (-64..320)
            .rev()
            .find(|&y| {
                self.state(BlockPos::new(x, y, z))
                    .is_some_and(|state| state.block() != Block::Air)
            })
            .map(|y| y + 1)
    }

    fn min_y(&self) -> i32 {
        -64
    }

    fn has_ceiling(&self) -> bool {
        false
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_map_in_hand_gets_terrain_and_the_players_marker() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut out) = session_with_outbound(&world, 1);
    session.pose.position = [8.5, 120.0, 8.5];
    let id = service(&world)
        .create(OVERWORLD, 8.0, 8.0, 0, true, false)
        .await
        .unwrap();
    hand_map(&mut session, id);

    session.run_session_tick_hooks(SessionTick::Late).await;
    let first = next_map(&mut out, |_| true).await;
    assert_eq!((first.id, first.scale, first.locked), (id as i32, 0, false));
    let markers = first.markers.expect("markers are sent first");
    assert_eq!(markers.len(), 1);
    assert_eq!(
        markers[0],
        (MapDecorationType::Player.id() as i32, 17, 17, 0)
    );
    let (x, y, width, height, colors) = first.patch.expect("the whole map is owed");
    assert_eq!((x, y, width, height), (0, 0, 128, 128));
    assert!(colors.iter().all(|&c| c == 0));

    let drawn = next_map(&mut out, |map| map.patch.is_some()).await;
    let (x0, y0, width, _height, colors) = drawn.patch.unwrap();
    assert!(drawn.markers.is_none());
    let pixel = |x: u8, y: u8| colors[(x - x0) as usize + (y - y0) as usize * width as usize];

    let generator = Generator::new(1);
    let terrain = Generated(
        [(-4, 0), (-3, 0), (-4, 1)]
            .into_iter()
            .map(|(cx, cz)| {
                let pos = ChunkPos::new(cx, cz);
                (pos, generator.generate(pos))
            })
            .collect(),
    );
    for (px, py) in [(1u8, 64u8), (17, 70), (1, 90)] {
        let (bx, bz) = (i32::from(px) - 64, i32::from(py) - 64);
        let expected = sample(&terrain, bx, bz, 0).expect("generated");
        let got = pixel(px, py);
        assert_eq!(got >> 2, expected.color, "pixel ({px}, {py})");
        assert!(got & 3 != 3, "only the three terrain brightness levels");
    }
    assert_eq!(world.shared.dimensions.default_id(), session.dimension);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_map_only_carried_is_not_drawn_but_marks_its_holder() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut out) = session_with_outbound(&world, 1);
    let id = service(&world)
        .create(OVERWORLD, 0.0, 0.0, 0, true, false)
        .await
        .unwrap();
    session.inventory_mut().set_slot(10, Some(filled(id)));
    session.run_session_tick_hooks(SessionTick::Late).await;
    let first = next_map(&mut out, |_| true).await;
    assert!(first.markers.is_some_and(|m| m.len() == 1));
    service(&world).sync().await;
    session.run_session_tick_hooks(SessionTick::Late).await;
    service(&world).sync().await;
    assert!(
        out.try_recv().is_err(),
        "nothing changed and nothing is drawn from a pocket"
    );
    assert!(
        service(&world)
            .data(id)
            .await
            .unwrap()
            .colors
            .iter()
            .all(|&c| c == 0)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn locked_maps_are_not_redrawn() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut out) = session_with_outbound(&world, 1);
    let id = service(&world)
        .create(OVERWORLD, 0.0, 0.0, 0, true, false)
        .await
        .unwrap();
    let locked = service(&world)
        .derive(id, tuff_feature_builtin_vanilla_maps::Derivation::Lock)
        .await
        .unwrap();
    hand_map(&mut session, locked);
    session.run_session_tick_hooks(SessionTick::Late).await;
    let first = next_map(&mut out, |_| true).await;
    assert!(first.locked);
    for _ in 0..3 {
        session.run_session_tick_hooks(SessionTick::Late).await;
    }
    service(&world).sync().await;
    assert!(out.try_recv().is_err());
    world.shutdown().await;
}

async fn banner_setup(
    world: &TestWorld,
    pos: BlockPos,
) -> (
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::of_block(pos);
    let region = world.region(chunk.region()).await;
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(world, &region, [(pos, Block::RedBanner.default_state())]).await;
    (region, player, events)
}

async fn click_banner(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    pos: BlockPos,
    id: u32,
    sequence: i32,
) {
    let mut click = place(player, pos, Item::FilledMap, sequence);
    click.components.map_id = Some(id);
    region.request(RegionRequest::Place(click)).await;
    let outcome = expect(events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: seen,
            outcome,
        } if seen == sequence => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(outcome, ActionOutcome::Used { .. }), "{outcome:?}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicking_a_banner_with_a_map_toggles_its_marker() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = BlockPos::new(5, 100, 6);
    let (region, player, mut events) = banner_setup(&world, pos).await;
    let id = service(&world)
        .create(OVERWORLD, 0.0, 0.0, 0, true, false)
        .await
        .unwrap();

    click_banner(&region, &player, &mut events, pos, id, 1).await;
    service(&world).sync().await;
    let data = service(&world).data(id).await.unwrap();
    assert_eq!(
        data.banners,
        vec![MapBanner {
            pos,
            color: tuff_vanilla_data::item::DyeColor::Red,
            name: None
        }]
    );

    click_banner(&region, &player, &mut events, pos, id, 2).await;
    service(&world).sync().await;
    assert!(service(&world).data(id).await.unwrap().banners.is_empty());

    let far = BlockPos::new(500, 100, 6);
    let (region, player, mut events) = banner_setup(&world, far).await;
    click_banner(&region, &player, &mut events, far, id, 3).await;
    service(&world).sync().await;
    assert!(
        service(&world).data(id).await.unwrap().banners.is_empty(),
        "a banner outside the map is refused"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_banner_marker_shows_and_goes_when_the_banner_does() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = BlockPos::new(5, 100, 6);
    let (region, player, mut events) = banner_setup(&world, pos).await;
    let (mut session, mut out) = session_with_outbound(&world, 1);
    session.pose.position = [8.5, 120.0, 8.5];
    let id = service(&world)
        .create(OVERWORLD, 0.0, 0.0, 0, true, false)
        .await
        .unwrap();
    click_banner(&region, &player, &mut events, pos, id, 1).await;
    hand_map(&mut session, id);

    session.run_session_tick_hooks(SessionTick::Late).await;
    let banner = MapDecorationType::BannerRed.id() as i32;
    let shown = next_map(&mut out, |map| {
        map.markers
            .as_ref()
            .is_some_and(|m| m.iter().any(|marker| marker.0 == banner))
    })
    .await;
    let marker = shown
        .markers
        .unwrap()
        .into_iter()
        .find(|marker| marker.0 == banner)
        .unwrap();
    assert_eq!((marker.1, marker.2, marker.3), (11, 13, 8));

    place_blocks(&world, &region, [(pos, Block::Air.default_state())]).await;
    for _ in 0..20 {
        session.run_session_tick_hooks(SessionTick::Late).await;
        world.ticks(1).await;
    }
    let gone = next_map(&mut out, |map| {
        map.markers
            .as_ref()
            .is_some_and(|m| !m.iter().any(|marker| marker.0 == banner))
    })
    .await;
    assert!(gone.markers.is_some());
    assert!(service(&world).data(id).await.unwrap().banners.is_empty());
    world.shutdown().await;
}

mod tables {
    use super::*;
    use tuff_core::world::menu::{MenuKind, cartography};
    use tuff_core::world::messages::{MenuOpen, MenuSource};
    use tuff_core::world::session::PlayerSession;
    use tuff_feature_builtin_vanilla_maps::crafted::post_processing;

    async fn open_table(world: &TestWorld, session: &mut PlayerSession) -> i32 {
        let region = world.region(ChunkPos::new(0, 0).region()).await;
        session
            .open_menu(MenuOpen {
                dimension: session.dimension,
                region,
                kind: MenuKind::Cartography,
                title: TextComponent::translate("container.cartography_table", []),
                source: MenuSource::Station {
                    pos: BlockPos::new(0, 64, 0),
                    bookcases: 0,
                    data: Vec::new(),
                },
                stat: None,
            })
            .await;
        session.window_id().expect("the table opened")
    }

    async fn put(session: &mut PlayerSession, map: ItemStack, additional: ItemStack) {
        let menu = session.menus.menu.as_mut().unwrap();
        menu.container[cartography::MAP] = Some(map);
        menu.container[cartography::ADDITIONAL] = Some(additional);
        session.refresh_station();
        session.run_session_tick_hooks(SessionTick::Late).await;
    }

    fn take(window_id: i32) -> Serverbound {
        Serverbound::ContainerClick {
            window_id,
            state_id: 0,
            slot: cartography::RESULT as i16,
            button: 0,
            mode: 0,
            changed: Vec::new(),
            carried: None,
        }
    }

    fn result(session: &PlayerSession) -> Option<ItemStack> {
        session.menu_container().unwrap()[cartography::RESULT]
            .as_ref()
            .map(tuff_item::item::ItemStack::test_copy)
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn paper_at_a_cartography_table_zooms_a_map_out() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let (mut session, _out) = session_with_outbound(&world, 1);
        let id = service(&world)
            .create(OVERWORLD, 0.0, 0.0, 0, true, false)
            .await
            .unwrap();
        let window = open_table(&world, &mut session).await;
        let mut two = filled(id);
        two.resize(2, MintReason::Test, BurnReason::Test);
        put(
            &mut session,
            two,
            ItemStack::mint(Item::Paper, 3, MintReason::Test),
        )
        .await;
        let shown = result(&session).expect("a result");
        assert_eq!((shown.count(), shown.components.map_id), (1, Some(id)));
        assert_eq!(
            post_processing(&shown),
            Some(tuff_feature_builtin_vanilla_maps::Derivation::ZoomOut)
        );

        session.handle_packet(take(window)).await;
        let container = session.menu_container().unwrap();
        assert_eq!(container[cartography::MAP].as_ref().unwrap().count(), 1);
        assert_eq!(
            container[cartography::ADDITIONAL].as_ref().unwrap().count(),
            2
        );
        assert!(session.inventory().carried().is_some());
        session.run_session_tick_hooks(SessionTick::Late).await;
        let carried = session.inventory().carried().unwrap().test_copy();
        assert_eq!(post_processing(&carried), None);
        let new = carried.components.map_id.unwrap();
        assert_ne!(new, id);
        let data = service(&world).data(new).await.unwrap();
        assert_eq!((data.scale, data.center_x, data.center_z), (1, 64, 64));
        assert!(
            result(&session).is_some(),
            "the remaining inputs make another"
        );
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn glass_locks_and_empty_maps_copy_at_a_cartography_table() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let (mut session, _out) = session_with_outbound(&world, 1);
        let id = service(&world)
            .create(OVERWORLD, 0.0, 0.0, 0, true, false)
            .await
            .unwrap();
        let window = open_table(&world, &mut session).await;
        put(
            &mut session,
            filled(id),
            ItemStack::mint(Item::GlassPane, 1, MintReason::Test),
        )
        .await;
        session.handle_packet(take(window)).await;
        session.run_session_tick_hooks(SessionTick::Late).await;
        let locked = session.inventory().carried().unwrap().test_copy();
        let locked_id = locked.components.map_id.unwrap();
        assert_ne!(locked_id, id);
        assert!(service(&world).data(locked_id).await.unwrap().locked);
        let menu = session.menu_container().unwrap();
        assert!(menu[cartography::MAP].is_none() && menu[cartography::ADDITIONAL].is_none());

        session.inventory_mut().set_carried(None);
        put(
            &mut session,
            locked.test_copy(),
            ItemStack::mint(Item::GlassPane, 1, MintReason::Test),
        )
        .await;
        assert!(result(&session).is_none(), "a locked map stays as it is");
        put(
            &mut session,
            locked,
            ItemStack::mint(Item::Paper, 1, MintReason::Test),
        )
        .await;
        assert!(result(&session).is_none(), "nor can it grow");
        put(
            &mut session,
            filled(id),
            ItemStack::mint(Item::Map, 1, MintReason::Test),
        )
        .await;
        let copies = result(&session).unwrap();
        assert_eq!((copies.count(), copies.components.map_id), (2, Some(id)));
        assert_eq!(post_processing(&copies), None);
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_extended_map_is_made_when_the_crafted_result_is_taken() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let (mut session, _out) = session_with_outbound(&world, 1);
        let id = service(&world)
            .create(OVERWORLD, 0.0, 0.0, 3, true, false)
            .await
            .unwrap();
        let crafted = tuff_feature_builtin_vanilla_maps::crafted::with_post_processing(
            filled(id),
            tuff_feature_builtin_vanilla_maps::Derivation::ZoomOut,
        );
        session.inventory_mut().set_slot(12, Some(crafted));
        session.run_session_tick_hooks(SessionTick::Late).await;
        let made = session.inventory().slots()[12]
            .as_ref()
            .map(tuff_item::item::ItemStack::test_copy)
            .unwrap();
        assert_eq!(post_processing(&made), None);
        let data = service(&world)
            .data(made.components.map_id.unwrap())
            .await
            .unwrap();
        assert_eq!(data.scale, 4);
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn maps_at_the_largest_scale_cannot_be_extended() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let (mut session, _out) = session_with_outbound(&world, 1);
        let region = world.region(ChunkPos::new(0, 0).region()).await;
        session
            .open_menu(MenuOpen {
                dimension: session.dimension,
                region,
                kind: MenuKind::Crafting,
                title: TextComponent::translate("container.crafting", []),
                source: MenuSource::Station {
                    pos: BlockPos::new(0, 64, 0),
                    bookcases: 0,
                    data: Vec::new(),
                },
                stat: None,
            })
            .await;
        for (scale, kept) in [(3, true), (4, false)] {
            let id = service(&world)
                .create(OVERWORLD, 0.0, 0.0, scale, true, false)
                .await
                .unwrap();
            let shown = tuff_feature_builtin_vanilla_maps::crafted::with_post_processing(
                filled(id),
                tuff_feature_builtin_vanilla_maps::Derivation::ZoomOut,
            );
            session.menus.menu.as_mut().unwrap().container[0] = Some(shown);
            session.run_session_tick_hooks(SessionTick::Late).await;
            assert_eq!(
                session.menu_container().unwrap()[0].is_some(),
                kept,
                "scale {scale}"
            );
        }
        world.shutdown().await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explorer_maps_target_marker_comes_from_the_item() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut out) = session_with_outbound(&world, 1);
    session.pose.position = [1000.0, 70.0, 1000.0];
    let id = service(&world)
        .create(OVERWORLD, 0.0, 0.0, 0, true, false)
        .await
        .unwrap();
    let mut map = filled(id);
    map.item = Item::WoodlandMansionMap;
    let target = tuff_feature_builtin_vanilla_maps::StaticMarker {
        key: "+".to_owned(),
        kind: MapDecorationType::Mansion,
        x: 20.0,
        z: -10.0,
        rotation: 180.0,
    };
    map.components.other.insert(
        tuff_feature_builtin_vanilla_maps::service::DECORATIONS,
        tuff_nbt::Compound::new().with("+", target.to_nbt()),
    );
    session.inventory_mut().set_slot(10, Some(map));
    session.run_session_tick_hooks(SessionTick::Late).await;
    let first = next_map(&mut out, |m| m.markers.is_some()).await;
    let markers = first.markers.unwrap();
    assert!(
        markers.contains(&(MapDecorationType::Mansion.id() as i32, 40, -19, 8)),
        "{markers:?}"
    );
    world.shutdown().await;
}

mod explorer {
    use super::*;
    use std::sync::Arc;
    use tuff_core::storage::level::DimensionSettings;
    use tuff_core::world::generator::ChunkGenerator;
    use tuff_feature_builtin_vanilla_loot::exploration::{
        ExplorationMaps, ExplorationSpec, ExploredMap, mark, pending,
    };
    use tuff_world_model::chunk::{Chunk, ChunkShape};

    struct Charted(Arc<dyn ChunkGenerator>);

    const TARGET: BlockPos = BlockPos::new(300, 64, -200);

    impl ChunkGenerator for Charted {
        fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk {
            self.0.generate(pos, shape)
        }

        fn sea_level(&self) -> i32 {
            self.0.sea_level()
        }

        fn spawn(&self) -> BlockPos {
            self.0.spawn()
        }

        fn locate_structure_within(
            &self,
            structures: &[&str],
            _from: BlockPos,
            radius: i32,
            _shape: ChunkShape,
            skip: Option<tuff_core::world::generator::SkipChunk<'_>>,
        ) -> Option<(BlockPos, &'static str)> {
            let chunk = ChunkPos::new(TARGET.x >> 4, TARGET.z >> 4);
            if skip.is_some_and(|skip| skip(chunk)) {
                return None;
            }
            (structures.contains(&"minecraft:mansion") && radius == 100)
                .then_some("minecraft:mansion")
                .or_else(|| {
                    structures
                        .contains(&"minecraft:buried_treasure")
                        .then_some("minecraft:buried_treasure")
                })
                .map(|name| (TARGET, name))
        }

        fn biomes_at(
            &self,
            positions: &[BlockPos],
            _shape: ChunkShape,
        ) -> Vec<Option<&'static str>> {
            positions
                .iter()
                .map(|pos| {
                    Some(if pos.x < 300 {
                        "minecraft:ocean"
                    } else {
                        "minecraft:plains"
                    })
                })
                .collect()
        }
    }

    fn charted(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
        Arc::new(Charted(tuff_test_support::placeholder::for_settings(
            settings, seed,
        )))
    }

    fn spec(destination: &str) -> ExplorationSpec {
        ExplorationSpec::from_json(&serde_json::json!({
            "destination": destination,
            "search_radius": 100,
        }))
        .unwrap()
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn explorer_maps_center_on_the_structure_with_a_water_sketch() {
        let dir = TestDir::new();
        let world =
            TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), charted);
        let maps = world.shared.services.get::<ExplorationMaps>().unwrap();
        let dimension = world.shared.dimensions.default_id();
        let answer = maps
            .request(
                dimension,
                BlockPos::new(0, 64, 0),
                spec("#minecraft:on_woodland_mansion_maps"),
            )
            .unwrap();
        let made: ExploredMap = answer.await.unwrap().expect("the mansion is found");
        assert_eq!(made.target, TARGET);
        assert_eq!(made.decoration, MapDecorationType::Mansion);
        service(&world).sync().await;
        let data = service(&world).data(made.id).await.unwrap();
        assert_eq!(data.scale, 2);
        assert!(data.tracking_position && data.unlimited_tracking);
        assert_eq!((data.center_x, data.center_z), (192, -320));
        let shore = data.colors.iter().filter(|&&c| c >> 2 == 15).count();
        let land = data.colors.iter().filter(|&&c| c >> 2 == 26).count();
        assert!(shore > 0 && land > 0, "{shore} {land}");

        let mut stack = ItemStack::mint(Item::WoodlandMansionMap, 1, MintReason::Test);
        mark(&mut stack, &spec("#minecraft:on_woodland_mansion_maps"));
        assert!(pending(&stack).is_some());
        made.apply(&mut stack.components);
        assert_eq!(stack.components.map_id, Some(made.id));

        let missing = maps
            .request(
                dimension,
                BlockPos::new(0, 64, 0),
                spec("#minecraft:on_ocean_explorer_maps"),
            )
            .unwrap();
        assert_eq!(missing.await.unwrap(), None);
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn skip_existing_chunks_passes_over_structures_in_saved_chunks() {
        use tuff_core::world::chunk_io::{EntityWrite, IoOp};
        let dir = TestDir::new();
        let world =
            TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), charted);
        let maps = world.shared.services.get::<ExplorationMaps>().unwrap();
        let dimension = world.shared.dimensions.default_id();
        let ask = |skip: bool| {
            let mut spec = spec("#minecraft:on_woodland_mansion_maps");
            spec.skip_existing = skip;
            maps.request(dimension, BlockPos::new(0, 64, 0), spec)
                .unwrap()
        };
        assert!(ask(true).await.unwrap().is_some(), "nothing saved yet");

        let key = world.shared.dimension(dimension).key.clone();
        let target = ChunkPos::new(TARGET.x >> 4, TARGET.z >> 4);
        let info = world.shared.dimension(dimension);
        let blob = world
            .shared
            .backend
            .chunk_codec()
            .encode_chunk(
                &Chunk::empty(info.shape),
                &tuff_world_model::chunk_meta::ChunkMeta::generated(),
                target,
                0,
            )
            .unwrap();
        world
            .shared
            .chunk_io
            .get(&world.shared.backend)
            .submit(
                &key,
                vec![IoOp::Write {
                    pos: target,
                    blocks: Some(blob),
                    entities: EntityWrite::Keep,
                    pois: EntityWrite::Keep,
                    ticket: None,
                }],
            )
            .await
            .unwrap();
        assert_eq!(ask(true).await.unwrap(), None, "the chunk is saved");
        assert!(
            ask(false).await.unwrap().is_some(),
            "unless saved chunks count"
        );
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_loot_chest_explorer_map_is_written_into_its_slot_when_found() {
        use crate::SKY;
        let dir = TestDir::new();
        let world =
            TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), charted);
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(999);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        let ground = BlockPos::new(1, SKY, 1);
        let chest = BlockPos::new(1, SKY + 1, 1);
        place_blocks(&world, &region, [(ground, Block::Stone.default_state())]).await;
        let mut placing = place(&viewer, ground, Item::Chest, 1);
        placing.components.other = Compound::new().with(
            "minecraft:container_loot",
            Compound::new()
                .with("loot_table", "minecraft:chests/shipwreck_map")
                .with("seed", 7i64),
        );
        region.request(RegionRequest::Place(placing)).await;
        expect(&mut events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: 1,
                outcome: ActionOutcome::Placed { .. },
            } => Some(()),
            _ => None,
        })
        .await;
        region
            .request(RegionRequest::Place(place(&viewer, chest, Item::Torch, 2)))
            .await;
        let open = expect(&mut events, container_menu).await;
        let MenuSource::Blocks { contents, .. } = open.source else {
            panic!("a chest is a block container");
        };
        let contents = stacks(&contents);
        let slot = contents
            .iter()
            .position(|stack| stack.as_ref().is_some_and(|s| pending(s).is_some()))
            .expect("the table rolled an explorer map waiting for its map");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut finished = None;
        while finished.is_none() && tokio::time::Instant::now() < deadline {
            world.ticks(1).await;
            while let Ok(event) = events.try_recv() {
                if let PlayerEvent::ContainerSlots { slots, .. } = event {
                    finished = finished.or(slots.into_iter().find(|(at, _)| *at == slot));
                }
            }
        }
        let map = finished
            .expect("the map slot was written")
            .1
            .expect("the structure is found, so the slot keeps a map");
        assert!(pending(&map).is_none());
        let id = map.components.map_id.expect("the map has an id");
        let target = map
            .components
            .other
            .get(tuff_feature_builtin_vanilla_maps::service::DECORATIONS)
            .and_then(|d| d.as_compound())
            .and_then(|d| d.get("+"))
            .and_then(|t| t.as_compound())
            .expect("the target marker is on the item");
        assert_eq!(target.get_double("x"), Some(f64::from(TARGET.x)));
        assert_eq!(target.get_double("z"), Some(f64::from(TARGET.z)));
        let data = service(&world).data(id).await.unwrap();
        assert!(data.tracking_position && data.unlimited_tracking);
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cartographer_offers_its_explorer_map_once_the_map_is_made() {
        use crate::{SKY, mobs, pen, summon};
        use tuff_vanilla_data::registry::EntityType;
        let dir = TestDir::new();
        let world =
            TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), charted);
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(999);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(4, 11)).await;
        let mut sell = ItemStack::mint(Item::WoodlandMansionMap, 1, MintReason::Test);
        mark(&mut sell, &spec("#minecraft:on_woodland_mansion_maps"));
        let offer = Compound::new()
            .with(
                "buy",
                Compound::new()
                    .with("id", "minecraft:emerald")
                    .with("count", 14),
            )
            .with("sell", sell.to_nbt())
            .with("maxUses", 12)
            .with("xp", 30);
        let nbt = Compound::new()
            .with(
                "VillagerData",
                Compound::new()
                    .with("type", "minecraft:plains")
                    .with("profession", "minecraft:cartographer")
                    .with("level", 5),
            )
            .with("NoAI", true)
            .with("Offers", Compound::new().with("Recipes", vec![offer]));
        summon(
            &region,
            EntityType::Villager,
            nbt,
            [7.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        let id = mobs(&region, EntityType::Villager).await[0].id;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut made = None;
        while tokio::time::Instant::now() < deadline {
            world.ticks(1).await;
            let (reply, answer) = oneshot::channel();
            region
                .request(RegionRequest::EntityData { id, reply })
                .await;
            let data = answer.await.unwrap().unwrap();
            let recipes = data
                .get_compound("Offers")
                .and_then(|o| o.get_list("Recipes"))
                .unwrap_or(&[])
                .to_vec();
            if let Some(found) = recipes.iter().find_map(|r| {
                let sell = ItemStack::mint_from_nbt(
                    r.as_compound()?.get_compound("sell")?,
                    MintReason::Test,
                )?;
                sell.components.map_id.is_some().then_some(sell)
            }) {
                made = Some(found);
                break;
            }
        }
        let made = made.expect("the explorer map offer came back");
        assert_eq!(made.item, Item::WoodlandMansionMap);
        assert!(pending(&made).is_none());
        let data = service(&world)
            .data(made.components.map_id.unwrap())
            .await
            .unwrap();
        assert!(data.unlimited_tracking);
        world.shutdown().await;
    }
}
