//! A panicking region actor or player session is reported, recovered and
//! leaves no dead handle or ghost player behind.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

use tuff_core::auth::GameProfile;
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::protocol::configuration::ClientInformation;
use tuff_core::protocol::play::Serverbound;
use tuff_core::world::messages::TimedTicket;
use tuff_core::world::region_api::{
    BlockAction, PlayerBlockAction, RegionHooks, RegionWorld, TickPhase,
};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{Interceptor, PacketInterceptor, Verdict};
use tuff_item::load::{LoadError, Phase};
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::Block;

/// Panics on a placed sponge, on a chat "boom", and on every tick of region
/// (0, 0) while `ticking` is set. Each panic names the test that armed it,
/// so its message and the session or region log line it causes cannot be
/// mistaken for another test's failure.
struct Fuse {
    ticking: Arc<AtomicBool>,
    test: String,
}

impl Fuse {
    #[track_caller]
    fn blow(&self, what: &str) -> ! {
        panic!("intentional test panic armed by {}: {what}", self.test);
    }
}

impl RegionHooks for Fuse {
    fn tick(&self, world: &mut dyn RegionWorld, _phase: TickPhase) {
        if world.region_pos() == RegionPos::new(0, 0) && self.ticking.load(Ordering::SeqCst) {
            self.blow("region tick");
        }
    }

    fn player_block_action(&self, _world: &mut dyn RegionWorld, action: &PlayerBlockAction) {
        if action.action == BlockAction::Placed && action.state.block() == Block::Sponge {
            self.blow("sponge placed");
        }
    }
}

impl PacketInterceptor for Fuse {
    fn intercept<'a>(
        &'a self,
        _session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Verdict> + Send + 'a>> {
        if matches!(packet, Serverbound::Chat(text) if text == "boom") {
            self.blow("session packet");
        }
        Box::pin(async { Verdict::Allow })
    }
}

struct FuseFeature {
    ticking: Arc<AtomicBool>,
    test: String,
}

impl Feature for FuseFeature {
    fn id(&self) -> &'static str {
        "test-fuse"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            let fuse = Arc::new(Fuse {
                ticking: self.ticking.clone(),
                test: self.test.clone(),
            });
            let registries = cx.registries();
            registries.region_hooks.push(fuse.clone());
            registries.packet_interceptors.push(Interceptor {
                order: 0,
                hook: fuse,
            });
        }
        Ok(())
    }
}

fn fused(dir: &Path) -> (TestWorld, Arc<AtomicBool>) {
    let ticking = Arc::new(AtomicBool::new(false));
    let test = std::thread::current()
        .name()
        .unwrap_or("supervision")
        .to_owned();
    let world = TestWorld::start_with_features(
        dir,
        vec![Box::new(FuseFeature {
            ticking: ticking.clone(),
            test,
        })],
    );
    (world, ticking)
}

async fn save_all(world: &TestWorld) {
    let (done, saved) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::SaveAll {
            done: Some(done),
            snapshotted: None,
        })
        .await
        .unwrap();
    saved.await.unwrap();
}

fn region_panics(world: &TestWorld) -> i64 {
    world.shared.metrics.core().region_panics.get()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panicking_region_restarts_from_storage_and_disconnects_its_players() {
    let dir = TestDir::new();
    let (world, _) = fused(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &viewer, &mut events, center).await;
    let (other_viewer, mut other_events) = PlayerHandle::new(1001);
    view_around(
        &world,
        &other_viewer,
        &mut other_events,
        ChunkPos::new(100, 100),
    )
    .await;
    let region = world.region(center.region()).await;

    let saved = BlockPos::new(84, 100, 84);
    let unsaved = BlockPos::new(86, 100, 86);
    let stone = Block::Stone.default_state();
    place_blocks(&world, &region, [(saved, stone)]).await;
    save_all(&world).await;
    place_blocks(&world, &region, [(unsaved, stone)]).await;
    assert_eq!(block_at(&region, unsaved).await, Some(stone));

    region
        .request(RegionRequest::Place(place(&viewer, saved, Item::Sponge, 1)))
        .await;
    let reason = expect(&mut events, |event| match event {
        PlayerEvent::Kick(reason) => Some(reason),
        _ => None,
    })
    .await;
    assert!(format!("{reason:?}").contains("disconnect.genericReason"));
    assert_eq!(region_panics(&world), 1);

    world.ticks(3).await;
    assert!(
        other_events
            .try_recv()
            .map_or(true, |event| !matches!(event, PlayerEvent::Kick(_))),
        "players of other regions stay"
    );

    let again = world.region(center.region()).await;
    let (returning, mut returning_events) = PlayerHandle::new(1002);
    view_around(&world, &returning, &mut returning_events, center).await;
    assert_eq!(
        block_at(&again, saved).await,
        Some(stone),
        "saved blocks are back"
    );
    assert_ne!(
        block_at(&again, unsaved).await,
        Some(stone),
        "changes since the last save are lost"
    );
    assert_ne!(
        block_at(&again, BlockPos::new(saved.x, saved.y + 1, saved.z))
            .await
            .map(|s| s.block()),
        Some(Block::Sponge)
    );
    assert_eq!(region_panics(&world), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forced_chunks_load_again_after_a_region_panic() {
    let dir = TestDir::new();
    let (world, _) = fused(dir.path());
    let forced = ChunkPos::new(1, 1);
    let probe = BlockPos::new(20, 100, 20);
    let (reply, changed) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::ForceLoad {
            dimension: world.dimension(OVERWORLD),
            chunks: vec![forced],
            ticket: tuff_core::world::messages::ChunkTicket::Forced,
            on: true,
            reply,
        })
        .await
        .unwrap();
    assert_eq!(changed.await.unwrap(), vec![true]);
    world.ticks(2).await;
    let region = world.region(forced.region()).await;
    assert!(
        block_at(&region, probe).await.is_some(),
        "the forced chunk loads"
    );

    let (viewer, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &viewer, &mut events, center).await;
    let target = BlockPos::new(84, 100, 84);
    place_blocks(&world, &region, [(target, Block::Stone.default_state())]).await;
    region
        .request(RegionRequest::Place(place(
            &viewer,
            target,
            Item::Sponge,
            1,
        )))
        .await;
    expect(&mut events, |event| {
        matches!(event, PlayerEvent::Kick(_)).then_some(())
    })
    .await;
    world.ticks(2).await;
    assert_eq!(region_panics(&world), 1);
    assert!(
        block_at(&region, probe).await.is_some(),
        "the restarted region holds its forced chunks again"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_region_that_keeps_panicking_backs_off_and_comes_back() {
    let dir = TestDir::new();
    let (world, ticking) = fused(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    world.region(RegionPos::new(5, 5)).await;
    ticking.store(true, Ordering::SeqCst);
    world.ticks(60).await;
    let panics = region_panics(&world);
    assert!(
        (4..=6).contains(&panics),
        "restarts back off after a few quick panics, saw {panics}"
    );

    ticking.store(false, Ordering::SeqCst);
    world.ticks(200).await;
    assert_eq!(block_at(&region, BlockPos::new(8, 300, 8)).await, None);
    assert_eq!(region_panics(&world), panics, "the region runs again");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panicking_session_leaves_normally() {
    let dir = TestDir::new();
    let (world, _) = fused(dir.path());
    let id = 1000;
    let (outbound, mut outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(id);
    let (packets, packets_rx) = mpsc::channel(8);
    let session = PlayerSession::new(
        world.shared.clone(),
        id,
        GameProfile {
            id: id as u128,
            name: format!("Player{id}"),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets_rx,
        PermissionLevel::Owners,
    );
    let (done, mut finished) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(session.run(None, done));

    packets
        .send(Serverbound::Chat("boom".into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), finished.wait_for(|done| *done))
        .await
        .expect("the session leaves")
        .expect("the session reports leaving normally");
    running
        .await
        .expect("the session task ends without a panic");

    world.announced().await;
    assert_eq!(
        world.shared.metrics.core().session_panics.get(),
        1,
        "the session's panic is counted once"
    );
    assert_eq!(
        *world.left.lock().unwrap(),
        vec![id],
        "the coordinator drops the player"
    );
    let disconnect = tuff_vanilla_data::packet::play::Clientbound::Disconnect.id();
    let mut kicked = false;
    while let Ok(frame) = outbound_rx.try_recv() {
        if framed_packet_id(&frame) == disconnect {
            let mut payload = framed_payload(&frame);
            let body = payload.read_bytes(payload.remaining()).unwrap();
            let key = b"disconnect.genericReason";
            kicked |= body.windows(key.len()).any(|w| w == key);
        }
    }
    assert!(kicked, "the client is told why");
    let stats = world
        .shared
        .backend
        .documents()
        .read(tuff_storage::document::Document::Stats(id as u128))
        .unwrap();
    assert!(stats.is_some(), "the player's data is saved");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panicking_manager_stops_the_server_and_still_saves_every_region() {
    let dir = TestDir::new();
    let target = BlockPos::new(4, 100, 4);
    let stone = Block::Stone.default_state();
    let world = TestWorld::start(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, ChunkPos::new(0, 0)).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let before = tuff_core::metrics::actor_panics().get();
    world
        .manager
        .send(ManagerMessage::Panic("manager test panic"))
        .await
        .unwrap();
    place_blocks(&world, &region, [(target, stone)]).await;
    let again = world.region(RegionPos::new(0, 0)).await;
    assert_eq!(
        block_at(&again, target).await,
        Some(stone),
        "the manager keeps serving the same region"
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        while world.stops.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the manager asks the server to stop");
    assert!(tuff_core::metrics::actor_panics().get() > before);
    world.shutdown().await;

    let world = TestWorld::start(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, ChunkPos::new(0, 0)).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    assert_eq!(
        block_at(&region, target).await,
        Some(stone),
        "the shutdown after the panic saved the region"
    );
    assert_eq!(world.stops.load(Ordering::SeqCst), 0);
    world.shutdown().await;
}

async fn force_off(world: &TestWorld, ticket: TimedTicket, chunk: ChunkPos) -> bool {
    let (reply, changed) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::ForceLoad {
            dimension: world.dimension(OVERWORLD),
            chunks: vec![chunk],
            ticket: tuff_core::world::messages::ChunkTicket::Timed(ticket),
            on: false,
            reply,
        })
        .await
        .unwrap();
    changed.await.unwrap()[0]
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timed_tickets_hold_chunks_around_the_centre_until_they_lapse() {
    let dir = TestDir::new();
    let (world, _) = fused(dir.path());
    let center = ChunkPos::new(40, 40);
    let hold = |ticket| ManagerMessage::Hold {
        dimension: world.dimension(OVERWORLD),
        center,
        ticket,
    };
    world
        .manager
        .send(hold(TimedTicket::PlayerSpawn))
        .await
        .unwrap();
    world.manager.send(hold(TimedTicket::Portal)).await.unwrap();
    world.ticks(2).await;
    let edge = ChunkPos::new(43, 37);
    let outside = ChunkPos::new(44, 40);
    assert!(!force_off(&world, TimedTicket::PlayerSpawn, outside).await);
    world.ticks(TimedTicket::PlayerSpawn.timeout() + 2).await;
    assert!(
        !force_off(&world, TimedTicket::PlayerSpawn, edge).await,
        "the spawn ticket lapsed"
    );
    assert!(
        force_off(&world, TimedTicket::Portal, edge).await,
        "the portal ticket outlives it"
    );
    world.ticks(TimedTicket::Portal.timeout()).await;
    assert!(!force_off(&world, TimedTicket::Portal, center).await);
    world.shutdown().await;
}
