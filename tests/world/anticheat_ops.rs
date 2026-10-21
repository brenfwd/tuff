//! Anti-cheat operations: exemptions by permission node and game mode,
//! log-only checks, the kick message, the violation board with its typed
//! events, resets, and `/anticheat`.

use super::*;
use tuff_core::protocol::play::{GameMode, Movement, Serverbound};
use tuff_core::world::commands::{Actor, run_counted};
use tuff_feature_builtin_vanilla_anticheat::service::{Board, Outcome};
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

async fn grant(world: &TestWorld, uuid: u128, node: &str) {
    world
        .shared
        .permissions
        .set(uuid, "Player", node, Some(true))
        .await
        .unwrap();
}

fn board(world: &TestWorld) -> &Board {
    world
        .shared
        .services
        .get::<Board>()
        .expect("the board runs")
}

async fn flush(world: &TestWorld) {
    board(world).list().await;
}

fn sent_text(out: &mut mpsc::UnboundedReceiver<bytes::Bytes>, needle: &str) -> bool {
    let mut found = false;
    while let Ok(frame) = out.try_recv() {
        found |= frame.windows(needle.len()).any(|w| w == needle.as_bytes());
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_per_check_exemption_skips_only_that_check() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    grant(&world, 31, "tuff.anticheat.exempt.sanity").await;
    let (mut player, _out) = session_with_outbound(&world, 31);
    player.mode.permission = PermissionLevel::All;
    player.handle_packet(nan_move()).await;
    assert!(!player.disconnected, "sanity is exempt");
    while !player.disconnected {
        player.handle_packet(creative_set()).await;
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bypass_node_and_game_mode_switches_exempt_everything() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    grant(&world, 32, "tuff.anticheat.bypass").await;
    let (mut player, _out) = session_with_outbound(&world, 32);
    player.mode.permission = PermissionLevel::All;
    for _ in 0..50 {
        player.handle_packet(creative_set()).await;
    }
    player.handle_packet(nan_move()).await;
    assert!(!player.disconnected);

    world
        .shared
        .config
        .set_text("tuff.anticheat.exemptSpectators", "true")
        .unwrap();
    let (mut spectator, _out) = session_with_outbound(&world, 33);
    spectator.mode.permission = PermissionLevel::All;
    spectator.mode.game_mode = GameMode::Spectator;
    spectator.handle_packet(nan_move()).await;
    assert!(!spectator.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_log_only_check_counts_and_reports_but_never_acts() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.check.creative_slot.action", "log")
        .unwrap();
    let mut events = board(&world).subscribe();
    let (mut player, _out) = session_with_outbound(&world, 34);
    player.mode.permission = PermissionLevel::All;
    for _ in 0..60 {
        player.handle_packet(creative_set()).await;
    }
    assert!(!player.disconnected);
    let event = events.recv().await.unwrap();
    assert_eq!(event.check, "creative_slot");
    assert_eq!(event.outcome, Outcome::Logged);
    assert_eq!(event.player, 34);
    flush(&world).await;
    let stats = board(&world).player("player34").await.unwrap();
    assert_eq!(stats.flags(), 60);
    assert_eq!(stats.kicks(), 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_kick_message_is_configurable_and_never_names_the_check() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut out) = session_with_outbound(&world, 35);
    player.mode.permission = PermissionLevel::All;
    player.handle_packet(nan_move()).await;
    assert!(player.disconnected);
    assert!(sent_text(&mut out, "Kicked for suspicious activity"));

    world
        .shared
        .config
        .set_text("tuff.anticheat.kickMessage", "Goodbye")
        .unwrap();
    let (mut other, mut out) = session_with_outbound(&world, 36);
    other.mode.permission = PermissionLevel::All;
    other.handle_packet(nan_move()).await;
    assert!(other.disconnected);
    let mut frames = Vec::new();
    while let Ok(frame) = out.try_recv() {
        frames.push(frame);
    }
    let has = |needle: &str| {
        frames
            .iter()
            .any(|f| f.windows(needle.len()).any(|w| w == needle.as_bytes()))
    };
    assert!(has("Goodbye"));
    assert!(!has("sanity"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flags_and_kicks_are_counted_per_check_in_the_metrics() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut honest, _out) = session_with_outbound(&world, 41);
    honest.mode.permission = PermissionLevel::All;
    honest
        .handle_packet(Serverbound::Move(Movement {
            position: Some([0.0, 64.0, 0.0]),
            rotation: None,
            on_ground: true,
            horizontal_collision: false,
        }))
        .await;
    flush(&world).await;
    let value = |snapshot: &crate::metrics::Snapshot, name: &str| {
        snapshot.series.get(name).map(|s| s.value)
    };
    let snapshot = world.shared.metrics.collect().await;
    assert_eq!(value(&snapshot, "anticheat.flags"), Some(0));
    assert_eq!(value(&snapshot, "anticheat.kicks"), Some(0));
    assert_eq!(value(&snapshot, "anticheat.flags.creative_slot"), None);

    let (mut player, _out) = session_with_outbound(&world, 42);
    player.mode.permission = PermissionLevel::All;
    let mut sent = 0;
    while !player.disconnected {
        player.handle_packet(creative_set()).await;
        sent += 1;
    }
    let snapshot = world.shared.metrics.collect().await;
    assert_eq!(
        value(&snapshot, "anticheat.flags.creative_slot"),
        Some(sent)
    );
    assert_eq!(value(&snapshot, "anticheat.kicks.creative_slot"), Some(1));
    assert_eq!(value(&snapshot, "anticheat.flags"), Some(sent));
    assert_eq!(value(&snapshot, "anticheat.kicks"), Some(1));
    assert_eq!(value(&snapshot, "anticheat.kicks.sanity"), None);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decay_is_configurable_per_check() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.check.creative_slot.decay", "0")
        .unwrap();
    let (mut player, _out) = session_with_outbound(&world, 37);
    player.mode.permission = PermissionLevel::All;
    for round in 0..40u64 {
        player.tick = round * 1000;
        player.handle_packet(creative_set()).await;
        if player.disconnected {
            break;
        }
    }
    assert!(player.disconnected, "without decay spaced flags add up");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_clears_the_ladder_the_session_holds() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 38);
    player.mode.permission = PermissionLevel::All;
    let mut sent = 0;
    while !player.disconnected {
        player.handle_packet(creative_set()).await;
        sent += 1;
    }
    let (mut player, _out) = session_with_outbound(&world, 38);
    player.mode.permission = PermissionLevel::All;
    for _ in 0..sent - 1 {
        player.handle_packet(creative_set()).await;
    }
    assert!(!player.disconnected);
    flush(&world).await;
    assert!(board(&world).reset("Player38", None).await.is_some());
    for _ in 0..sent - 1 {
        player.handle_packet(creative_set()).await;
    }
    assert!(!player.disconnected, "the reset level starts from zero");
    player.handle_packet(creative_set()).await;
    assert!(player.disconnected);
    world.shutdown().await;
}

async fn runs(session: &mut tuff_core::world::session::PlayerSession, line: &str) -> u32 {
    let source = session.command_source();
    run_counted(Actor::Player(session), source, line).await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn staff_list_reset_and_mute_with_the_command() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut cheater, _out) = session_with_outbound(&world, 39);
    cheater.mode.permission = PermissionLevel::All;
    for _ in 0..3 {
        cheater.handle_packet(creative_set()).await;
    }
    flush(&world).await;

    let (mut staff, _out) = session_with_outbound(&world, 40);
    assert_eq!(runs(&mut staff, "anticheat violations").await, 1);
    assert_eq!(runs(&mut staff, "anticheat violations Player39").await, 1);
    assert_eq!(board(&world).player("Player39").await.unwrap().flags(), 3);
    let now = std::time::Instant::now();
    let all =
        tuff_feature_builtin_vanilla_anticheat::service::totals(&board(&world).list().await, now);
    assert_eq!(all.values().map(|t| t.hour).sum::<u32>(), 3);
    assert_eq!(all.values().map(|t| t.day).sum::<u32>(), 3);
    assert_eq!(
        runs(&mut staff, "anticheat reset Player39 nonsense").await,
        0
    );
    assert_eq!(
        runs(&mut staff, "anticheat reset Player39 creative_slot").await,
        1
    );
    assert!(board(&world).player("Player39").await.is_none());
    assert_eq!(runs(&mut staff, "anticheat reset Player39").await, 0);

    assert_eq!(runs(&mut staff, "anticheat alerts").await, 1);
    assert!(board(&world).muted().contains(&40));
    assert_eq!(runs(&mut staff, "anticheat alerts on").await, 1);
    assert!(!board(&world).muted().contains(&40));

    cheater.tick += 1000;
    for _ in 0..2 {
        cheater.handle_packet(creative_set()).await;
    }
    flush(&world).await;
    staff.mode.permission = PermissionLevel::All;
    assert_eq!(runs(&mut staff, "anticheat violations Player39").await, 0);
    grant(&world, 40, "tuff.anticheat.command.violations").await;
    assert_eq!(runs(&mut staff, "anticheat violations Player39").await, 1);
    assert_eq!(runs(&mut staff, "anticheat reset Player39").await, 0);
    assert_eq!(runs(&mut staff, "anticheat alerts").await, 0);
    assert!(!board(&world).muted().contains(&40));
    assert_eq!(board(&world).player("Player39").await.unwrap().flags(), 2);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn staff_trace_one_players_checks_with_the_debug_command() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = super::anticheat::walker(&world, 43, ChunkPos::new(44, 44)).await;
    let mut traces = board(&world).traces();
    let (mut staff, _out) = session_with_outbound(&world, 44);
    w.step(0.0, w.floor_y, 0.0).await;
    assert!(traces.try_recv().is_err(), "nothing is traced by default");

    assert_eq!(
        runs(&mut staff, "anticheat debug Player43 nonsense").await,
        0
    );
    assert_eq!(runs(&mut staff, "anticheat debug Player43 fly").await, 1);
    let y = w.floor_y;
    w.step(0.0, y + 0.42, 0.0).await;
    let event = traces.try_recv().expect("a fly trace");
    assert_eq!(event.player, 43);
    assert_eq!(event.trace.check, "fly");
    assert!(
        event
            .trace
            .tags
            .contains(tuff_feature_builtin_vanilla_anticheat::trace::Tags::JUMP_ENVELOPE)
    );
    assert!((event.trace.actual - 0.42).abs() < 1e-6);
    assert!(event.trace.allowed >= event.trace.actual);
    while let Ok(event) = traces.try_recv() {
        assert_eq!(event.trace.check, "fly", "only the chosen check");
    }

    assert_eq!(runs(&mut staff, "anticheat debug Player43 off").await, 1);
    w.step(0.0, y + 0.42 + 0.33, 0.0).await;
    assert!(traces.try_recv().is_err(), "tracing stopped");
    world.shutdown().await;
}
