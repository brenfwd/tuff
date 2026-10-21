//! What keeps running while `/tick freeze` holds the game clock: the
//! connection, the players and everything they need, but not the world's
//! own simulation.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::protocol::play::Serverbound;
use crate::world::session::PlayerSession;
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::registry::EntityType;

struct Connected {
    session: PlayerSession,
    outbound: mpsc::UnboundedReceiver<bytes::Bytes>,
    packets: mpsc::Sender<Serverbound>,
}

fn connected(world: &TestWorld, id: i32) -> Connected {
    let (outbound, outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(id);
    let (packets, packets_rx) = mpsc::channel(8);
    let profile = GameProfile {
        id: id as u128,
        name: format!("Player{id}"),
        properties: Vec::new(),
    };
    let mut session = PlayerSession::new(
        world.shared.clone(),
        id,
        profile,
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets_rx,
        PermissionLevel::Owners,
    );
    session.net.keep_alive_interval = Duration::from_millis(50);
    session.net.keep_alive_timeout = Duration::from_secs(2);
    Connected {
        session,
        outbound: outbound_rx,
        packets,
    }
}

fn keep_alive() -> i32 {
    tuff_vanilla_data::packet::play::Clientbound::KeepAlive.id()
}

fn disconnect() -> i32 {
    tuff_vanilla_data::packet::play::Clientbound::Disconnect.id()
}

async fn next_of(
    outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>,
    ids: &[i32],
) -> bytes::Bytes {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let frame = outbound.recv().await.expect("the session is gone");
            if ids.contains(&framed_packet_id(&frame)) {
                return frame;
            }
        }
    })
    .await
    .expect("no such packet arrived")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_with_no_ticks_keeps_its_client_alive() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let Connected {
        mut session,
        mut outbound,
        packets,
    } = connected(&world, 1000);
    let clock = *world.clock.borrow();
    let player = tokio::spawn(async move {
        session.play().await;
        session.disconnected
    });
    for _ in 0..20 {
        let frame = next_of(&mut outbound, &[keep_alive(), disconnect()]).await;
        assert_eq!(
            framed_packet_id(&frame),
            keep_alive(),
            "kicked while frozen"
        );
        let mut payload = framed_payload(&frame);
        payload.read_var_int().unwrap();
        let id = payload.read_long().unwrap();
        packets.send(Serverbound::KeepAlive(id)).await.unwrap();
    }
    assert_eq!(*world.clock.borrow(), clock, "no tick ran");
    assert!(!player.is_finished(), "the session stays connected");
    drop(packets);
    assert!(!player.await.unwrap());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_with_no_ticks_still_times_out_a_silent_client() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let Connected {
        mut session,
        mut outbound,
        packets,
    } = connected(&world, 1000);
    session.net.keep_alive_timeout = Duration::from_millis(100);
    let player = tokio::spawn(async move {
        session.play().await;
        session.disconnected
    });
    next_of(&mut outbound, &[keep_alive()]).await;
    let kick = next_of(&mut outbound, &[disconnect()]).await;
    let mut payload = framed_payload(&kick);
    let body = payload.read_bytes(payload.remaining()).unwrap();
    assert!(
        body.windows(18).any(|w| w == b"disconnect.timeout"),
        "kicked for timing out"
    );
    assert!(player.await.unwrap(), "the silent client was kicked");
    drop(packets);
    world.shutdown().await;
}

fn packet_id(name: tuff_vanilla_data::packet::play::Clientbound) -> i32 {
    name.id()
}

/// A connected client: the session runs its own loop, and the test
/// answers keep-alives and chunk batches as a client would.
struct Client {
    outbound: mpsc::UnboundedReceiver<bytes::Bytes>,
    packets: mpsc::Sender<Serverbound>,
    task: tokio::task::JoinHandle<PlayerSession>,
}

impl Client {
    async fn join(world: &TestWorld, id: i32, position: [f64; 3]) -> Self {
        Self::join_as(world, id, position, play::GameMode::Spectator).await
    }

    async fn join_as(world: &TestWorld, id: i32, position: [f64; 3], mode: play::GameMode) -> Self {
        let Connected {
            mut session,
            outbound,
            packets,
        } = connected(world, id);
        session.mode.game_mode = mode;
        session.mode.abilities = crate::world::player::Abilities::new(mode, false);
        session.stand_at(position, false);
        session.update_view().await;
        let task = tokio::spawn(async move {
            session.play().await;
            session
        });
        Self {
            outbound,
            packets,
            task,
        }
    }

    /// Everything sent so far, answered as a client would.
    async fn read(&mut self) -> Vec<bytes::Bytes> {
        use tuff_vanilla_data::packet::play::Clientbound as C;
        let mut frames = Vec::new();
        while let Ok(frame) = self.outbound.try_recv() {
            let id = framed_packet_id(&frame);
            if id == packet_id(C::KeepAlive) {
                let mut payload = framed_payload(&frame);
                payload.read_var_int().unwrap();
                let value = payload.read_long().unwrap();
                self.packets
                    .send(Serverbound::KeepAlive(value))
                    .await
                    .unwrap();
            } else if id == packet_id(C::ChunkBatchFinished) {
                self.packets
                    .send(Serverbound::ChunkBatchReceived(64.0))
                    .await
                    .unwrap();
            }
            assert_ne!(id, packet_id(C::Disconnect), "kicked while frozen");
            frames.push(frame);
        }
        frames
    }

    async fn walk_to(&self, position: [f64; 3]) {
        self.packets
            .send(Serverbound::Move(play::Movement {
                position: Some(position),
                rotation: None,
                on_ground: false,
                horizontal_collision: false,
            }))
            .await
            .unwrap();
        self.packets.send(Serverbound::ClientTickEnd).await.unwrap();
    }

    async fn leave(self) -> PlayerSession {
        drop(self.packets);
        self.task.await.unwrap()
    }
}

fn chunk_of(frame: &bytes::Bytes) -> Option<ChunkPos> {
    let mut payload = framed_payload(frame);
    let id = payload.read_var_int().unwrap();
    (id == packet_id(tuff_vanilla_data::packet::play::Clientbound::LevelChunkWithLight)).then(
        || {
            let x = payload.read_int().unwrap();
            let z = payload.read_int().unwrap();
            ChunkPos::new(x, z)
        },
    )
}

/// The new x of `entity` after a frame, given the x seen so far.
fn moves_of(frame: &bytes::Bytes, entity: i32, x: Option<f64>) -> Option<f64> {
    use tuff_vanilla_data::packet::play::Clientbound as C;
    let mut payload = framed_payload(frame);
    let id = payload.read_var_int().unwrap();
    if id != C::EntityPositionSync.id() && id != C::MoveEntityPos.id() {
        return None;
    }
    if payload.read_var_int().unwrap() != entity {
        return None;
    }
    if id == C::EntityPositionSync.id() {
        payload.read_var_int().unwrap();
        return Some(payload.read_double().unwrap());
    }
    payload.read_var_int().unwrap();
    let dx = f64::from(payload.read_short().unwrap());
    x.map(|x| x + dx / 4096.0)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_walking_while_frozen_gets_new_chunks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let clock = *world.clock.borrow();
    let mut client = Client::join(&world, 1000, [8.5, 100.0, 8.5]).await;
    let mut seen = HashSet::new();
    let mut x = 8.5;
    for _ in 0..200 {
        if x < 88.5 {
            x += 0.8;
            client.walk_to([x, 100.0, 8.5]).await;
        }
        world.frozen_ticks(1).await;
        seen.extend(client.read().await.iter().filter_map(chunk_of));
        if seen.contains(&ChunkPos::new(7, 0)) {
            break;
        }
    }
    assert!(
        seen.contains(&ChunkPos::new(7, 0)),
        "chunks ahead were sent: {seen:?}"
    );
    assert_eq!(*world.clock.borrow(), clock, "the game stayed frozen");
    let session = client.leave().await;
    assert!(!session.disconnected);
    assert!(session.view.subscribed.contains(&ChunkPos::new(7, 0)));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frozen_entities_hold_still_while_players_see_each_other_move() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let mut watcher = Client::join(&world, 1000, [8.5, 100.0, 8.5]).await;
    let walker = Client::join(&world, 1001, [10.5, 100.0, 8.5]).await;
    world.frozen_ticks(5).await;
    let start = [8.5, 150.0, 8.5];
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::new(Item::Dirt, 1),
            position: start,
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    let item = |found: Vec<crate::world::messages::EntitySnapshot>| {
        found
            .into_iter()
            .find(|e| e.kind == EntityType::Item)
            .map(|e| e.position)
    };
    let mut seen_at = Some(10.5);
    for step in 0..100 {
        walker.walk_to([10.5 + 0.1 * step as f64, 100.0, 8.5]).await;
        world.frozen_ticks(1).await;
        for frame in watcher.read().await.iter() {
            seen_at = moves_of(frame, 1001, seen_at).or(seen_at);
        }
        if seen_at.is_some_and(|x| x > 11.0) {
            break;
        }
    }
    assert!(
        seen_at.is_some_and(|x| x > 11.0),
        "the watcher saw the walker move: {seen_at:?}"
    );
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    assert_eq!(
        item(answer.await.unwrap()),
        Some(start),
        "the item is frozen"
    );

    world.ticks(5).await;
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let fallen = item(answer.await.unwrap()).expect("item");
    assert!(fallen[1] < start[1], "unfreezing lets it fall: {fallen:?}");
    watcher.read().await;
    assert!(!watcher.leave().await.disconnected);
    assert!(!walker.leave().await.disconnected);
    world.shutdown().await;
}

async fn kicked_for(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>, key: &[u8]) {
    let kick = next_of(outbound, &[disconnect()]).await;
    let mut payload = framed_payload(&kick);
    let body = payload.read_bytes(payload.remaining()).unwrap();
    assert!(
        body.windows(key.len()).any(|w| w == key),
        "kicked for {}",
        String::from_utf8_lossy(key)
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_over_the_rate_limit_is_kicked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let Connected {
        mut session,
        mut outbound,
        packets,
    } = connected(&world, 1000);
    session.net.rate_limit = 3;
    let player = tokio::spawn(async move {
        session.play().await;
        session.disconnected
    });
    for _ in 0..20 {
        let _ = packets.send(Serverbound::ClientTickEnd).await;
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let _ = packets.send(Serverbound::ClientTickEnd).await;
    kicked_for(&mut outbound, b"disconnect.exceeded_packet_rate").await;
    assert!(player.await.unwrap());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_over_the_chat_spam_threshold_is_kicked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let Connected {
        mut session,
        mut outbound,
        packets,
    } = connected(&world, 1000);
    session.net.chat_spam = tuff_core::world::session::TickThrottler::new(20, 40);
    session.mode.permission = tuff_storage::admin::PermissionLevel::All;
    let player = tokio::spawn(async move {
        session.play().await;
        session.disconnected
    });
    for _ in 0..3 {
        let _ = packets.send(Serverbound::Chat("hi".into())).await;
    }
    kicked_for(&mut outbound, b"disconnect.spam").await;
    assert!(player.await.unwrap());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_client_is_kicked_after_the_idle_timeout() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let Connected {
        mut session,
        mut outbound,
        packets,
    } = connected(&world, 1000);
    session.net.idle_timeout = Some(Duration::from_millis(150));
    let player = tokio::spawn(async move {
        session.play().await;
        session.disconnected
    });
    kicked_for(&mut outbound, b"multiplayer.disconnect.idling").await;
    assert!(player.await.unwrap());
    drop(packets);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_changed_idle_timeout_reaches_an_online_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let Connected {
        session,
        mut outbound,
        packets,
    } = connected(&world, 1000);
    assert_eq!(session.net.idle_timeout, None);
    let handle = session.conn.handle.clone();
    let mut session = session;
    let player = tokio::spawn(async move {
        session.play().await;
        session.disconnected
    });
    world
        .shared
        .live
        .update_network(|network| network.idle_timeout = Some(Duration::from_millis(150)));
    handle.send(PlayerEvent::LiveSetting(live::LiveChange::Connection));
    kicked_for(&mut outbound, b"multiplayer.disconnect.idling").await;
    assert!(player.await.unwrap());
    drop(packets);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_standing_on_a_plate_presses_it_while_frozen() {
    use crate::world::messages::BlockOp;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let plate = BlockPos::new(8, 100, 8);
    blocks(
        &region,
        BlockOp::Set {
            changes: vec![(
                plate,
                tuff_vanilla_data::registry::Block::OakPressurePlate.default_state(),
            )],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
    let client = Client::join_as(&world, 1000, [8.5, 100.0, 8.5], play::GameMode::Survival).await;
    let mut pressed = false;
    for _ in 0..20 {
        world.frozen_ticks(1).await;
        pressed = block_at(&region, plate)
            .await
            .is_some_and(|s| s.powered() == Some(true));
        if pressed {
            break;
        }
    }
    assert!(pressed, "the plate was pressed while frozen");
    client.leave().await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_map_in_hand_updates_while_frozen() {
    use tuff_core::world::inventory::Inventory;
    use tuff_item::item::ItemStack;
    use tuff_vanilla_data::Item;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let id = world
        .shared
        .services
        .get::<tuff_feature_builtin_vanilla_maps::MapService>()
        .expect("the maps plugin runs")
        .create("minecraft:overworld", 8.0, 8.0, 0, true, false)
        .await
        .unwrap();
    let Connected {
        mut session,
        outbound,
        packets,
    } = connected(&world, 1000);
    session.stand_at([8.5, 120.0, 8.5], false);
    session.update_view().await;
    let slot = Inventory::hotbar_slot(session.inventory().selected());
    let mut stack = ItemStack::new(Item::FilledMap, 1);
    stack.components.map_id = Some(id);
    session.inventory_mut().set_slot(slot, Some(stack));
    let task = tokio::spawn(async move {
        session.play().await;
        session
    });
    let mut client = Client {
        outbound,
        packets,
        task,
    };
    let map_packet = packet_id(tuff_vanilla_data::packet::play::Clientbound::MapItemData);
    let mut maps = 0;
    for _ in 0..40 {
        world.frozen_ticks(1).await;
        maps += client
            .read()
            .await
            .iter()
            .filter(|frame| framed_packet_id(frame) == map_packet)
            .count();
        if maps >= 2 {
            break;
        }
    }
    assert!(maps >= 2, "the held map updated while frozen: {maps}");
    client.leave().await;
    world.shutdown().await;
}
