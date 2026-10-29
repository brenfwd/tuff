//! Reach and line of sight for block targets through the real interceptor
//! chain, on the session's copy of the client's world.

use super::anticheat::{Walker, walker};
use super::*;
use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};

async fn strict_walker(world: &TestWorld) -> Walker {
    for check in ["reach", "sight"] {
        world
            .shared
            .config
            .set_text(&format!("tuff.anticheat.check.{check}.kickAbove"), "0")
            .unwrap();
    }
    walker(world, 1, ChunkPos::new(40, 40)).await
}

async fn place(w: &mut Walker, world: &TestWorld, blocks: &[BlockPos]) {
    let region = world.region(RegionPos::new(1, 1)).await;
    let stone = tuff_vanilla_data::Block::Stone.default_state();
    place_blocks(world, &region, blocks.iter().map(|pos| (*pos, stone))).await;
    while blocks
        .iter()
        .any(|pos| w.session.client_world().block_at(*pos) != Some(stone))
    {
        let event = w.events.recv().await.expect("region events");
        w.session.on_event(event).await;
    }
}

fn start_breaking(pos: BlockPos) -> Serverbound {
    Serverbound::PlayerAction {
        status: PlayerActionStatus::StartDestroyBlock,
        position: pos,
        face: 1,
        sequence: 1,
    }
}

fn origin(w: &Walker) -> BlockPos {
    BlockPos::new(w.x.floor() as i32, 100, w.z.floor() as i32)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_a_block_in_reach_and_in_view_passes() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = strict_walker(&world).await;
    let target = BlockPos::new(origin(&w).x + 3, 101, origin(&w).z);
    place(&mut w, &world, &[target]).await;
    w.session.handle_packet(start_breaking(target)).await;
    assert!(!w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_a_block_beyond_reach_is_flagged() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = strict_walker(&world).await;
    let target = BlockPos::new(origin(&w).x + 6, 100, origin(&w).z);
    place(&mut w, &world, &[target]).await;
    w.session.handle_packet(start_breaking(target)).await;
    assert!(w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_a_block_behind_a_wall_is_flagged() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = strict_walker(&world).await;
    let o = origin(&w);
    let target = BlockPos::new(o.x + 3, 101, o.z);
    let mut blocks = vec![target];
    for y in 100..=104 {
        for dz in -1..=1 {
            blocks.push(BlockPos::new(o.x + 2, y, o.z + dz));
        }
    }
    place(&mut w, &world, &blocks).await;
    w.session.handle_packet(start_breaking(target)).await;
    assert!(w.session.disconnected);
    world.shutdown().await;
}

async fn show_zombie(w: &mut Walker, seq: u64, dx: f64) {
    let position = [w.x + dx, w.floor_y, w.z];
    w.session
        .on_event(PlayerEvent::Track(crate::world::messages::Track {
            entity: 900,
            kind: tuff_vanilla_data::registry::EntityType::Zombie,
            dimension: w.session.dimension,
            chunk: ChunkPos::of_position(position),
            seq,
            position,
            range: 128,
            spawn: crate::world::packets([]),
            update: crate::world::packets([]),
            moves: None,
            head: None,
            player: None,
            spectator: false,
            shape: None,
        }))
        .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hitting_where_a_target_just_was_passes_but_not_long_after() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = strict_walker(&world).await;
    w.session.tick += 100;
    w.step(0.0, w.floor_y, 0.0).await;
    show_zombie(&mut w, 1, 2.0).await;
    w.step(0.0, w.floor_y, 0.0).await;
    w.session.tick += 1;
    show_zombie(&mut w, 2, 6.5).await;
    w.step(0.0, w.floor_y, 0.0).await;
    assert!(w.session.view.entities.is_visible(900));
    w.session.handle_packet(Serverbound::Attack(900)).await;
    assert!(!w.session.disconnected, "the client still showed it near");
    w.session.tick += 20;
    w.step(0.0, w.floor_y, 0.0).await;
    w.session.handle_packet(Serverbound::Attack(900)).await;
    assert!(
        w.session.disconnected,
        "out of reach once the trace moved on"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_feed_only_check_escalates_only_through_the_improbable_score() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let config = &world.shared.config;
    config
        .set_text("tuff.anticheat.check.reach.feedOnly", "true")
        .unwrap();
    config
        .set_text("tuff.anticheat.check.improbable.kickAbove", "0")
        .unwrap();
    let mut w = strict_walker(&world).await;
    w.session.tick += 100;
    w.step(0.0, w.floor_y, 0.0).await;
    show_zombie(&mut w, 1, 6.5).await;
    let mut attacks = 0;
    while !w.session.disconnected && attacks < 20 {
        w.session.tick += 2;
        w.session.handle_packet(Serverbound::Attack(900)).await;
        attacks += 1;
    }
    assert!(w.session.disconnected);
    assert!(attacks > 2, "{attacks}");
    let alerts = super::anticheat::operator_alerts(&world).await;
    let last = alerts.last().unwrap();
    assert!(last.contains("kicked by the improbable check"), "{last}");
    world.shutdown().await;
}

async fn turn(w: &mut Walker, yaw: f32) {
    w.session
        .handle_packet(Serverbound::Move(tuff_core::protocol::play::Movement {
            position: None,
            rotation: Some((yaw, 0.0)),
            on_ground: true,
            horizontal_collision: false,
        }))
        .await;
}

async fn aim_flags(world: &TestWorld, name: &str) -> u32 {
    let board = world
        .shared
        .services
        .get::<tuff_feature_builtin_vanilla_anticheat::service::Board>()
        .unwrap();
    board.list().await;
    board
        .player(name)
        .await
        .and_then(|stats| stats.checks.get("aim").map(|c| c.flags))
        .unwrap_or(0)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attacking_without_looking_at_the_target_is_counted_by_aim() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = strict_walker(&world).await;
    w.session.tick += 100;
    w.step(0.0, w.floor_y, 0.0).await;
    show_zombie(&mut w, 1, 2.0).await;
    w.step(0.0, w.floor_y, 0.0).await;

    turn(&mut w, 0.0).await;
    w.session.handle_packet(Serverbound::Attack(900)).await;
    turn(&mut w, -90.0).await;
    w.session.handle_packet(Serverbound::ClientTickEnd).await;
    assert_eq!(aim_flags(&world, "Player1").await, 0);

    w.session.tick += 20;
    turn(&mut w, 90.0).await;
    w.session.handle_packet(Serverbound::ClientTickEnd).await;
    w.session.handle_packet(Serverbound::Attack(900)).await;
    w.session.handle_packet(Serverbound::ClientTickEnd).await;
    assert_eq!(aim_flags(&world, "Player1").await, 1);
    assert!(!w.session.disconnected, "aim only counts");
    world.shutdown().await;
}

fn break_face(pos: BlockPos, face: i8) -> Serverbound {
    Serverbound::PlayerAction {
        status: PlayerActionStatus::StartDestroyBlock,
        position: pos,
        face,
        sequence: 1,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_ray_goes_to_the_targeted_face() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = strict_walker(&world).await;
    let o = origin(&w);
    let target = BlockPos::new(o.x + 3, 101, o.z);
    let wall = BlockPos::new(o.x + 2, 101, o.z);
    place(&mut w, &world, &[target, wall]).await;
    w.session.handle_packet(break_face(target, 1)).await;
    assert!(!w.session.disconnected, "the top face shows over the wall");
    w.session
        .handle_packet(Serverbound::PlayerAction {
            status: PlayerActionStatus::AbortDestroyBlock,
            position: target,
            face: 1,
            sequence: 2,
        })
        .await;
    w.session.tick += 40;
    w.session.handle_packet(Serverbound::Punch).await;
    w.session.handle_packet(break_face(target, 4)).await;
    assert!(w.session.disconnected, "the west face is behind the wall");
    world.shutdown().await;
}

async fn mine(w: &mut Walker, target: BlockPos, took: u64) {
    w.session.handle_packet(break_face(target, 1)).await;
    w.session.handle_packet(Serverbound::Punch).await;
    w.session.tick += took;
    w.session.handle_packet(Serverbound::Punch).await;
    w.session
        .handle_packet(Serverbound::PlayerAction {
            status: PlayerActionStatus::StopDestroyBlock,
            position: target,
            face: 1,
            sequence: 2,
        })
        .await;
    w.session.tick += 10;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finishing_blocks_early_again_and_again_is_fast_break() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.check.fast_break.kickAbove", "0")
        .unwrap();
    let mut w = strict_walker(&world).await;
    w.session.tick += 200;
    let o = origin(&w);
    let target = BlockPos::new(o.x + 2, 101, o.z);
    place(&mut w, &world, &[target]).await;
    for _ in 0..2 {
        place(&mut w, &world, &[target]).await;
        mine(&mut w, target, 160).await;
        assert!(!w.session.disconnected, "stone by hand takes 150 ticks");
    }
    for _ in 0..6 {
        place(&mut w, &world, &[target]).await;
        mine(&mut w, target, 20).await;
        if w.session.disconnected {
            break;
        }
    }
    assert!(w.session.disconnected);
    let alerts = super::anticheat::operator_alerts(&world).await;
    let last = alerts.last().unwrap();
    assert!(
        last.contains("kicked by the fast_break check"),
        "{alerts:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attacking_a_visible_mob_in_reach_passes() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = strict_walker(&world).await;
    w.session.tick += 100;
    w.step(0.0, w.floor_y, 0.0).await;
    show_zombie(&mut w, 1, 2.0).await;
    w.step(0.0, w.floor_y, 0.0).await;
    w.session.handle_packet(Serverbound::Attack(900)).await;
    assert!(!w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attacking_a_mob_beyond_reach_is_flagged() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = strict_walker(&world).await;
    w.session.tick += 100;
    w.step(0.0, w.floor_y, 0.0).await;
    show_zombie(&mut w, 1, 6.5).await;
    w.step(0.0, w.floor_y, 0.0).await;
    w.session.handle_packet(Serverbound::Attack(900)).await;
    assert!(w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attacking_a_mob_through_a_wall_is_flagged() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = strict_walker(&world).await;
    let o = origin(&w);
    let mut blocks = Vec::new();
    for y in 99..=104 {
        for dz in -2..=2 {
            blocks.push(BlockPos::new(o.x + 1, y, o.z + dz));
        }
    }
    place(&mut w, &world, &blocks).await;
    w.session.tick += 100;
    w.step(0.0, w.floor_y, 0.0).await;
    show_zombie(&mut w, 1, 2.5).await;
    w.step(0.0, w.floor_y, 0.0).await;
    w.session.handle_packet(Serverbound::Attack(900)).await;
    assert!(w.session.disconnected);
    world.shutdown().await;
}
