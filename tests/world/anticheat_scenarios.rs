//! Legitimate movement is never set back and each cheat class that has a
//! check is, through the real interceptor chain.

use super::anticheat::{Walker, walker};
use super::*;
use std::sync::Mutex;
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::protocol::play::{Movement, Serverbound};
use tuff_core::world::session::hooks::{Interceptor, PacketInterceptor, Verdict};
use tuff_item::load::{LoadError, Phase};
use tuff_storage::admin::PermissionLevel;

impl Walker {
    async fn step_ground(&mut self, dx: f64, y: f64, dz: f64) {
        let [px, _, pz] = self.session.pose.position;
        self.session
            .handle_packet(Serverbound::Move(Movement {
                position: Some([px + dx, y, pz + dz]),
                rotation: None,
                on_ground: true,
                horizontal_collision: false,
            }))
            .await;
        self.session.handle_packet(Serverbound::ClientTickEnd).await;
    }

    async fn place(
        &mut self,
        world: &TestWorld,
        blocks: Vec<(BlockPos, tuff_vanilla_data::BlockState)>,
    ) {
        let region = world.region(RegionPos::new(1, 1)).await;
        place_blocks(world, &region, blocks).await;
        world.ticks(2).await;
        while let Ok(event) = self.events.try_recv() {
            self.session.on_event(event).await;
        }
    }

    fn flagged(&self) -> bool {
        self.session.sync.pending_teleport.is_some() || self.session.disconnected
    }
}

fn stone() -> tuff_vanilla_data::BlockState {
    tuff_vanilla_data::Block::Stone.default_state()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lag_spike_burst_and_high_latency_are_not_flagged() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(50, 50)).await;
    w.session.net.latency_ms = 350;
    for n in 0..20 {
        w.step(if n % 2 == 0 { 0.1 } else { -0.1 }, w.floor_y, 0.0)
            .await;
        tuff_feature_builtin_vanilla_anticheat::clock::advance(
            &w.session,
            Duration::from_millis(50),
        );
    }
    tuff_feature_builtin_vanilla_anticheat::clock::advance(&w.session, Duration::from_millis(500));
    for n in 0..9 {
        w.step(if n % 2 == 0 { 0.1 } else { -0.1 }, w.floor_y, 0.0)
            .await;
    }
    assert!(!w.flagged());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_confirmed_server_teleport_and_pushes_are_not_flagged() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(51, 51)).await;
    let target = [w.x + 3.0, w.floor_y, w.z];
    let below = BlockPos::new(target[0] as i32, target[1] as i32 - 1, target[2] as i32);
    w.place(&world, vec![(below, stone())]).await;
    w.session.teleport(target);
    w.confirm().await;
    for n in 0..6 {
        w.step(if n % 2 == 0 { 0.1 } else { -0.1 }, w.floor_y, 0.0)
            .await;
    }
    assert!(!w.flagged());
    w.session.motion.pushed_at = Some(w.session.tick);
    for _ in 0..4 {
        w.step(0.9, w.floor_y + 0.2, 0.0).await;
    }
    assert!(!w.flagged());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gliding_and_riptide_speeds_are_not_flagged() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(52, 52)).await;
    w.session.motion.glide.active = true;
    w.session.pose.pitch = -30.0;
    w.session.pose.position[1] = w.floor_y + 5.0;
    let mut y = w.floor_y + 5.0;
    for _ in 0..20 {
        y += 0.3;
        w.step(1.2, y, 0.3).await;
    }
    assert!(!w.flagged());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claiming_the_ground_while_falling_is_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(53, 53)).await;
    w.session.pose.position[1] = w.floor_y + 30.0;
    w.session.pose.on_ground = false;
    for _ in 0..30 {
        let y = w.session.pose.position[1] - 0.5;
        w.step_ground(0.0, y, 0.0).await;
        if w.flagged() {
            break;
        }
    }
    assert!(w.flagged());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_no_fall_ground_claim_over_air_keeps_the_fall_distance() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(55, 55)).await;
    w.session.pose.position[1] = w.floor_y + 10.0;
    w.session.pose.on_ground = false;
    for _ in 0..6 {
        let y = w.session.pose.position[1] - 0.5;
        w.step_ground(0.0, y, 0.0).await;
        if w.flagged() {
            break;
        }
    }
    assert!(!w.session.pose.on_ground);
    assert!(
        w.session.body.fall.distance >= 2.5,
        "{}",
        w.session.body.fall.distance
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standing_on_a_real_floor_keeps_the_ground_flag() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(57, 57)).await;
    for _ in 0..20 {
        w.step_ground(0.0, w.floor_y, 0.0).await;
    }
    assert!(w.session.pose.on_ground);
    assert!(!w.flagged());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walking_into_a_wall_is_a_phase_and_a_one_block_step_up_is_not_free() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(54, 54)).await;
    let (bx, bz) = (w.x as i32 + 1, w.z as i32);
    w.place(
        &world,
        vec![
            (BlockPos::new(bx, 101, bz), stone()),
            (BlockPos::new(bx, 102, bz), stone()),
        ],
    )
    .await;
    w.session
        .skip_ticks(tuff_core::world::session::mirror::RECENT_TICKS + 1);
    for _ in 0..6 {
        w.step_ground(0.4, w.floor_y, 0.0).await;
        if w.flagged() {
            break;
        }
    }
    assert!(w.flagged(), "phase");
    w.confirm().await;
    let before = w.session.pose.position;
    for n in 1..=6 {
        w.step(0.0, before[1] + f64::from(n), 0.0).await;
    }
    assert!(w.flagged(), "step up");
    world.shutdown().await;
}

struct Probe(Arc<Mutex<Vec<&'static str>>>);

impl PacketInterceptor for Probe {
    fn intercept<'a>(
        &'a self,
        _session: &'a mut tuff_core::world::session::PlayerSession,
        _packet: &'a Serverbound,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Verdict> + Send + 'a>> {
        self.0.lock().unwrap().push("later");
        Box::pin(async { Verdict::Allow })
    }
}

struct Plug(Arc<Mutex<Vec<&'static str>>>);

impl Feature for Plug {
    fn id(&self) -> &'static str {
        "test-after-anticheat"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries().packet_interceptors.push(Interceptor {
                order: -500,
                hook: Arc::new(Probe(self.0.clone())),
            });
        }
        Ok(())
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_anticheat_runs_before_other_interceptors() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(Plug(seen.clone()))]);
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    player
        .handle_packet(Serverbound::Move(Movement {
            position: Some([f64::NAN, 64.0, 0.0]),
            rotation: None,
            on_ground: true,
            horizontal_collision: false,
        }))
        .await;
    assert!(
        seen.lock().unwrap().is_empty(),
        "flagged packet never reaches later hooks"
    );
    let (mut other, _out) = session_with_outbound(&world, 2);
    other.mode.permission = PermissionLevel::All;
    other.handle_packet(Serverbound::SetCarriedItem(1)).await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[test]
fn without_the_feature_no_interceptor_is_registered() {
    let mut registries = tuff_core::registry::Registries::new();
    tuff_core::feature::load_all(&[], &mut registries).unwrap();
    assert!(registries.packet_interceptors.is_empty());
    let mut registries = tuff_core::registry::Registries::new();
    tuff_core::feature::load_all(&vanilla_features(), &mut registries).unwrap();
    assert_eq!(registries.packet_interceptors.len(), 1);
    assert_eq!(registries.packet_interceptors[0].order, -1000);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn off_ground_moves_before_the_world_is_loaded_never_kick_for_flying() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut unknown, _out) = session_with_outbound(&world, 1);
    unknown.sync.load_deadline = None;
    unknown
        .handle_packet(Serverbound::Move(Movement {
            position: Some([0.5, 200.0, 0.5]),
            rotation: None,
            on_ground: false,
            horizontal_collision: false,
        }))
        .await;
    assert!(!unknown.sync.client_floating, "unsent chunks are unknown");
    let (mut loading, _out) = session_with_outbound(&world, 2);
    loading.sync.load_deadline = Some(u64::MAX);
    loading.sync.client_floating = true;
    for _ in 0..200 {
        unknown.on_tick().await;
        loading.on_tick().await;
    }
    assert!(!unknown.disconnected);
    assert!(!loading.disconnected);
    world.shutdown().await;
}
