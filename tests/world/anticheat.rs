//! The anti-cheat action checks through the real interceptor chain.

use super::*;
use tuff_core::protocol::play::{Movement, PlayerActionStatus, Serverbound};
use tuff_storage::admin::PermissionLevel;

fn nan_move() -> Serverbound {
    Serverbound::Move(Movement {
        position: Some([f64::NAN, 64.0, 0.0]),
        rotation: None,
        on_ground: true,
        horizontal_collision: false,
    })
}

fn creative_set() -> Serverbound {
    Serverbound::SetCreativeModeSlot {
        slot: 36,
        stack: tuff_core::protocol::codec::CreativeSlot::Empty,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_finite_movement_kicks_a_player_but_not_an_operator() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut op, _out) = session_with_outbound(&world, 1);
    op.handle_packet(nan_move()).await;
    assert!(!op.disconnected);

    let (mut player, _out) = session_with_outbound(&world, 2);
    player.mode.permission = PermissionLevel::All;
    player.handle_packet(nan_move()).await;
    assert!(player.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operator_exemption_can_be_switched_off() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.exemptOperators", "false")
        .unwrap();
    let (mut op, _out) = session_with_outbound(&world, 1);
    op.handle_packet(nan_move()).await;
    assert!(op.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disabled_check_does_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.check.sanity.enabled", "false")
        .unwrap();
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    player.handle_packet(nan_move()).await;
    assert!(!player.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn violations_set_back_first_then_kick_and_decay_with_time() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;

    for round in 0..20u64 {
        player.tick = round * 1000;
        player.handle_packet(creative_set()).await;
        assert!(!player.disconnected, "spaced violations never kick");
    }
    assert!(out.try_recv().is_ok(), "the inventory is resent");

    player.tick = 100_000;
    for _ in 0..6 {
        assert!(!player.disconnected);
        player.handle_packet(creative_set()).await;
    }
    assert!(!player.disconnected);
    for _ in 0..4 {
        player.handle_packet(creative_set()).await;
    }
    assert!(player.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creative_players_may_set_slots_and_a_pitch_past_the_pole_is_clamped() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    player.mode.abilities.instabuild = true;
    for _ in 0..20 {
        player.handle_packet(creative_set()).await;
    }
    assert!(!player.disconnected);

    player
        .handle_packet(Serverbound::Move(Movement {
            position: None,
            rotation: Some((5.0, 400.0)),
            on_ground: true,
            horizontal_collision: false,
        }))
        .await;
    assert!(player.pose.pitch <= 90.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_autoclicker_is_cancelled_before_it_swings() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    for tick in 0..200u64 {
        player.tick = tick;
        for _ in 0..4 {
            player.handle_packet(Serverbound::Punch).await;
        }
        if player.disconnected {
            break;
        }
    }
    assert!(
        player.disconnected,
        "sustained 80 swings per second ends in a kick"
    );
    world.shutdown().await;
}

async fn punch(player: &mut crate::world::session::PlayerSession, tick: u64, count: u64) {
    player.tick = tick;
    for _ in 0..count {
        player.handle_packet(Serverbound::Punch).await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lag_bunched_punches_and_click_ticks_do_not_kick() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    for tick in 0..400u64 {
        punch(&mut player, tick, 2).await;
    }
    for stall in [20u64, 40, 100] {
        let tick = player.tick + stall;
        punch(&mut player, tick, stall * 5 / 2).await;
        for tick in tick + 1..tick + 400 {
            punch(&mut player, tick, 2).await;
        }
    }
    assert!(!player.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_mining_packet_needs_a_matching_start() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    player
        .handle_packet(Serverbound::PlayerAction {
            status: PlayerActionStatus::StopDestroyBlock,
            position: tuff_world_model::BlockPos::new(1, 60, 1),
            face: 1,
            sequence: 1,
        })
        .await;
    assert!(!player.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verified_finish_needs_most_of_the_mining_time() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let ground = BlockPos::new(1, height, 1);
    let eye = [1.5, height as f64 + 2.62, 2.5];
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    let finish = async |events: &mut mpsc::UnboundedReceiver<PlayerEvent>, ticks, seq| {
        region
            .request(RegionRequest::Break {
                player: player.clone(),
                pos: ground,
                tool: None,
                mode: BreakMode::Verified {
                    ticks,
                    eye,
                    miner: player::Miner::plain(true),
                },
                sequence: seq,
            })
            .await;
        expect(events, |event| match event {
            PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
            _ => None,
        })
        .await
    };
    assert!(matches!(
        finish(&mut events, 0, 1).await,
        ActionOutcome::Rejected(_)
    ));
    assert!(matches!(
        finish(&mut events, 5000, 2).await,
        ActionOutcome::Broken { .. }
    ));
    world.shutdown().await;
}

async fn creative_sets_until_kicked(threshold: Option<&str>, limit: usize) -> (bool, usize) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    if let Some(threshold) = threshold {
        world
            .shared
            .config
            .set_text("tuff.anticheat.kickThreshold", threshold)
            .unwrap();
    }
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    let mut sent = 0;
    while sent < limit && !player.disconnected {
        player.handle_packet(creative_set()).await;
        sent += 1;
    }
    world.shutdown().await;
    (player.disconnected, sent)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_kick_threshold_scales_every_limit() {
    let (kicked, base) = creative_sets_until_kicked(None, 1000).await;
    assert!(kicked);
    let (kicked, doubled) = creative_sets_until_kicked(Some("2.0"), 1000).await;
    assert!(kicked);
    assert!(doubled > base * 3 / 2, "{base} vs {doubled}");
    let (kicked, sent) = creative_sets_until_kicked(Some("0"), 1000).await;
    assert!(!kicked);
    assert_eq!(sent, 1000);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_kick_threshold_rejects_bad_values() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let config = &world.shared.config;
    assert!(
        config
            .set_text("tuff.anticheat.kickThreshold", "-1")
            .is_err()
    );
    assert!(
        config
            .set_text("tuff.anticheat.kickThreshold", "NaN")
            .is_err()
    );
    assert!(
        config
            .set_text("tuff.anticheat.kickThreshold", "abc")
            .is_err()
    );
    world.shutdown().await;
}

pub(super) struct Walker {
    pub(super) session: crate::world::session::PlayerSession,
    pub(super) frames: mpsc::UnboundedReceiver<bytes::Bytes>,
    pub(super) events: mpsc::UnboundedReceiver<PlayerEvent>,
    pub(super) handle: PlayerHandle,
    pub(super) floor_y: f64,
    pub(super) x: f64,
    pub(super) z: f64,
}

/// A walker whose steps the prediction engine does not judge: its moves
/// are fixed strides, not the game's physics.
pub(super) async fn walker(world: &TestWorld, id: i32, base: ChunkPos) -> Walker {
    world
        .shared
        .config
        .set_text("tuff.anticheat.check.predict.enabled", "false")
        .unwrap();
    physical_walker(world, id, base).await
}

/// A walker every check judges, the prediction engine included.
pub(super) async fn physical_walker(world: &TestWorld, id: i32, base: ChunkPos) -> Walker {
    let region = world.region(RegionPos::new(1, 1)).await;
    let (mut session, frames) = session_with_outbound(world, id);
    session.mode.permission = PermissionLevel::All;
    let (handle, mut events) = PlayerHandle::new(id);
    let chunks: Vec<_> = (-1..=1)
        .flat_map(|dx| (-1..=1).map(move |dz| ChunkPos::new(base.x + dx, base.z + dz)))
        .collect();
    session.view.subscribed.extend(chunks.iter().copied());
    region
        .request(RegionRequest::Subscribe {
            player: handle.clone(),
            chunks: chunks.clone(),
        })
        .await;
    let mut waiting = chunks.len();
    while waiting > 0 {
        let event = events.recv().await.expect("region events");
        waiting -= usize::from(matches!(event, PlayerEvent::Chunk { .. }));
        session.on_event(event).await;
    }
    while !session.view.queue.is_empty() {
        session.view.unacked_batches = 0;
        session.send_chunk_batch();
    }
    let (x, z) = (
        f64::from(base.x * 16 + 8) + 0.5,
        f64::from(base.z * 16 + 8) + 0.5,
    );
    let floor = BlockPos::new(x as i32, 100, z as i32);
    place_blocks(
        world,
        &region,
        [(floor, tuff_vanilla_data::Block::Stone.default_state())],
    )
    .await;
    while let Ok(event) = events.try_recv() {
        session.on_event(event).await;
    }
    session.pose.position = [x, 101.0, z];
    session.pose.on_ground = true;
    Walker {
        session,
        frames,
        events,
        handle,
        floor_y: 101.0,
        x,
        z,
    }
}

impl Walker {
    pub(super) async fn step(&mut self, dx: f64, y: f64, dz: f64) {
        let [px, _, pz] = self.session.pose.position;
        self.session
            .handle_packet(Serverbound::Move(Movement {
                position: Some([px + dx, y, pz + dz]),
                rotation: None,
                on_ground: false,
                horizontal_collision: false,
            }))
            .await;
        self.session.handle_packet(Serverbound::ClientTickEnd).await;
    }

    pub(super) async fn confirm(&mut self) {
        if let Some(id) = self.session.sync.pending_teleport {
            self.session
                .handle_packet(Serverbound::AcceptTeleportation {
                    id,
                    position: self.session.pose.position,
                    rotation: (0.0, 0.0),
                })
                .await;
        }
    }

    pub(super) async fn jump_and_land(&mut self) {
        let mut vy = 0.42;
        let mut y = self.floor_y;
        for _ in 0..40 {
            y += vy;
            vy = (vy - 0.08) * 0.98;
            if y <= self.floor_y && vy < 0.0 {
                y = self.floor_y;
            }
            self.step(0.0, y, 0.0).await;
            if y == self.floor_y {
                break;
            }
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walking_and_jumping_are_never_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(40, 40)).await;
    for _ in 0..3 {
        w.jump_and_land().await;
        for _ in 0..3 {
            w.step(0.0, w.floor_y, 0.0).await;
        }
    }
    assert!(w.session.sync.pending_teleport.is_none());
    assert!(!w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fly_hack_is_set_back_then_kicked_when_it_keeps_going() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(41, 41)).await;
    let start = w.session.pose.position;
    w.step(0.0, start[1] + 0.5, 0.0).await;
    w.step(0.0, start[1] + 1.0, 0.0).await;
    assert!(
        w.session.sync.pending_teleport.is_some(),
        "set back at once"
    );
    assert!(!w.session.disconnected);
    let mut rounds = 0;
    while !w.session.disconnected && rounds < 100 {
        w.confirm().await;
        let y = w.session.pose.position[1];
        w.step(0.0, y + 0.5, 0.0).await;
        w.step(0.0, y + 1.0, 0.0).await;
        rounds += 1;
    }
    assert!(w.session.disconnected, "kicked after {rounds} rounds");
    assert!(rounds > 3);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hovering_after_a_jump_trips_the_air_check() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(42, 42)).await;
    let mut vy = 0.42;
    let mut y = w.floor_y;
    for _ in 0..8 {
        y += vy;
        vy = (vy - 0.08) * 0.98;
        w.step(0.0, y, 0.0).await;
    }
    assert!(w.session.sync.pending_teleport.is_none());
    for _ in 0..12 {
        w.step(0.0, y, 0.0).await;
    }
    assert!(w.session.sync.pending_teleport.is_some());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flight_abilities_effects_and_pushes_are_not_flagged() {
    use crate::world::effects::ActiveEffect;
    use tuff_vanilla_data::registry::MobEffect;
    let cases: [fn(&mut crate::world::session::PlayerSession); 6] = [
        |s| {
            s.mode.abilities.may_fly = true;
            s.mode.abilities.flying = true;
        },
        |s| s.mode.abilities.instabuild = true,
        |s| s.mode.game_mode = tuff_core::protocol::play::GameMode::Spectator,
        |s| {
            s.body
                .effects
                .add(ActiveEffect::timed(MobEffect::Levitation, 200, 0));
        },
        |s| s.motion.pushed_at = Some(s.tick),
        |s| s.motion.glide.active = true,
    ];
    for (index, case) in cases.into_iter().enumerate() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let mut w = walker(&world, 1, ChunkPos::new(43, 43)).await;
        case(&mut w.session);
        for _ in 0..30 {
            let y = w.session.pose.position[1] + 0.05;
            w.step(0.0, y, 0.0).await;
        }
        assert!(w.session.sync.pending_teleport.is_none(), "case {index}");
        assert!(!w.session.disconnected, "case {index}");
        world.shutdown().await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleporting_across_a_chunk_of_ground_per_packet_is_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(44, 44)).await;
    w.step(0.5, w.floor_y, 0.0).await;
    assert!(w.session.sync.pending_teleport.is_none());
    let before = w.session.pose.position;
    w.step(6.0, w.floor_y, 0.0).await;
    assert!(w.session.sync.pending_teleport.is_some());
    assert_eq!(w.session.pose.position, before);
    let _ = (w.x, w.z);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timer_hack_is_set_back_and_a_normal_rate_is_not() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(45, 45)).await;
    for n in 0..30 {
        w.step(if n % 2 == 0 { 0.1 } else { -0.1 }, w.floor_y, 0.0)
            .await;
        tuff_feature_builtin_vanilla_anticheat::clock::advance(
            &w.session,
            Duration::from_millis(50),
        );
    }
    assert!(w.session.sync.pending_teleport.is_none());
    for _ in 0..300 {
        w.confirm().await;
        w.step(0.01, w.floor_y, 0.0).await;
        if w.session.sync.pending_teleport.is_some() {
            break;
        }
    }
    assert!(w.session.sync.pending_teleport.is_some());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sustained_speed_hack_is_set_back_but_legitimate_speeds_are_not() {
    use crate::world::effects::ActiveEffect;
    use tuff_vanilla_data::registry::MobEffect;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(46, 46)).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let floor: Vec<_> = (0..24)
        .map(|dx| {
            (
                BlockPos::new(w.x as i32 + dx, 100, w.z as i32),
                tuff_vanilla_data::Block::Stone.default_state(),
            )
        })
        .collect();
    place_blocks(&world, &region, floor).await;
    world.ticks(2).await;
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
    for n in 0..24 {
        let step = if n % 8 < 4 { 0.36 } else { 0.28 };
        w.step(step, w.floor_y, 0.0).await;
    }
    assert!(w.session.sync.pending_teleport.is_none(), "sprint hopping");
    for _ in 0..15 {
        w.step(0.66, w.floor_y + 0.1, 0.0).await;
        if w.session.sync.pending_teleport.is_some() {
            break;
        }
    }
    assert!(w.session.sync.pending_teleport.is_some(), "speed hack");
    w.confirm().await;
    w.session
        .body
        .effects
        .add(ActiveEffect::timed(MobEffect::Speed, 2000, 1));
    for _ in 0..20 {
        w.step(0.55, w.floor_y, 0.0).await;
    }
    assert!(w.session.sync.pending_teleport.is_none(), "speed II");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_seconds_allow_a_wider_step_and_a_confirmed_setback_regraces() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(46, 46)).await;
    w.step(1.0, w.floor_y, 0.0).await;
    assert!(
        w.session.sync.pending_teleport.is_none(),
        "inside the grace"
    );
    w.session.tick += 100;
    w.step(1.0, w.floor_y, 0.0).await;
    assert!(w.session.sync.pending_teleport.is_some(), "grace has ended");
    for _ in 0..30 {
        w.step(5.0, w.floor_y, 0.0).await;
    }
    w.confirm().await;
    w.step(1.0, w.floor_y, 0.0).await;
    assert!(
        w.session.sync.pending_teleport.is_none(),
        "regraced after confirm"
    );
    let _ = (w.x, w.z);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finishing_a_different_block_than_started_is_undone_then_kicks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    let action = |status, x| Serverbound::PlayerAction {
        status,
        position: tuff_world_model::BlockPos::new(x, 64, 0),
        face: 1,
        sequence: 1,
    };
    for round in 0..20 {
        player.tick = round * 2;
        player
            .handle_packet(action(PlayerActionStatus::StartDestroyBlock, 0))
            .await;
        player.handle_packet(Serverbound::Punch).await;
        player
            .handle_packet(action(PlayerActionStatus::StopDestroyBlock, 1))
            .await;
        if player.disconnected {
            break;
        }
    }
    assert!(player.disconnected);
    world.shutdown().await;
}

fn start_at(x: i32, z: i32) -> Serverbound {
    Serverbound::PlayerAction {
        status: PlayerActionStatus::StartDestroyBlock,
        position: tuff_world_model::BlockPos::new(x, 0, z),
        face: 1,
        sequence: 1,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sweeping_instant_blocks_at_the_client_rate_is_never_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    for n in 0..600u64 {
        player.tick = if n % 100 < 10 { n - n % 100 + 9 } else { n };
        let k = i32::try_from(n % 25).unwrap();
        player.handle_packet(start_at(k % 5 - 2, k / 5 - 2)).await;
        player.handle_packet(Serverbound::Punch).await;
        player.handle_packet(Serverbound::ClientTickEnd).await;
    }
    assert!(!player.disconnected);
    let alerts = operator_alerts(&world).await;
    assert!(alerts.is_empty(), "{alerts:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nuker_breaking_a_patch_in_one_tick_is_kicked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    player.tick = 100;
    for k in 0..9 {
        player.handle_packet(start_at(k % 3 - 1, k / 3 - 1)).await;
        player.handle_packet(Serverbound::Punch).await;
    }
    assert!(player.disconnected);
    let alerts = operator_alerts(&world).await;
    assert!(
        alerts
            .iter()
            .any(|a| a.contains("kicked by the nuker check")),
        "{alerts:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_wrong_finish_sets_back_and_a_second_soon_after_kicks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1);
    player.mode.permission = PermissionLevel::All;
    let stop = Serverbound::PlayerAction {
        status: PlayerActionStatus::StopDestroyBlock,
        position: tuff_world_model::BlockPos::new(1, 0, 0),
        face: 1,
        sequence: 1,
    };
    for at in [100u64, 160] {
        player.tick = at;
        player.handle_packet(start_at(0, 0)).await;
        player.handle_packet(Serverbound::Punch).await;
        player.handle_packet(Serverbound::ClientTickEnd).await;
        player.tick = at + 10;
        player.handle_packet(stop.clone()).await;
        assert_eq!(player.disconnected, at == 160, "finish at {at}");
    }
    world.shutdown().await;
}

pub(super) async fn operator_alerts(world: &TestWorld) -> Vec<String> {
    let (reply, done) = tokio::sync::oneshot::channel();
    world
        .shared
        .coordinator
        .send(CoordinatorMessage::Sync { reply })
        .await
        .unwrap();
    done.await.unwrap();
    world
        .operator_alerts
        .lock()
        .unwrap()
        .iter()
        .map(|line| line.plain())
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operators_hear_of_throttled_setbacks_and_every_kick() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 7);
    player.mode.permission = PermissionLevel::All;
    player.handle_packet(creative_set()).await;
    player.handle_packet(creative_set()).await;
    assert_eq!(operator_alerts(&world).await.len(), 1);
    player.tick += 20;
    player.handle_packet(creative_set()).await;
    let alerts = operator_alerts(&world).await;
    assert_eq!(alerts.len(), 2, "{alerts:?}");
    assert!(alerts[0].contains("Player7 set back by the"), "{alerts:?}");
    while !player.disconnected {
        player.handle_packet(creative_set()).await;
    }
    let alerts = operator_alerts(&world).await;
    assert_eq!(alerts.len(), 3, "{alerts:?}");
    assert!(alerts[2].contains("Player7 kicked by the"), "{alerts:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operator_alerts_can_be_switched_off() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.alertOperators", "false")
        .unwrap();
    let (mut player, _out) = session_with_outbound(&world, 8);
    player.mode.permission = PermissionLevel::All;
    player.handle_packet(nan_move()).await;
    assert!(player.disconnected);
    assert!(operator_alerts(&world).await.is_empty());
    world.shutdown().await;
}
