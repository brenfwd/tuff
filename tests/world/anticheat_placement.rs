//! Placement validity checks against the client's copy of the world.

use super::*;
use tuff_core::protocol::play::Serverbound;
use tuff_world_model::Face;

use super::anticheat::walker;

fn click(pos: BlockPos, face: Face, cursor: [f32; 3]) -> Serverbound {
    Serverbound::UseItemOn {
        hand: 0,
        position: pos,
        face,
        cursor,
        sequence: 0,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicking_the_floor_is_fine_but_clicking_air_kicks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(40, 40)).await;
    w.session.pose.pitch = 90.0;
    let floor = BlockPos::new(w.x as i32, 100, w.z as i32);
    for tick in 0..100u64 {
        w.session.tick = tick * 10;
        w.session
            .handle_packet(click(floor, Face::Up, [0.5, 1.0, 0.5]))
            .await;
        w.session.handle_packet(Serverbound::Punch).await;
        w.session.handle_packet(Serverbound::ClientTickEnd).await;
        assert!(!w.session.disconnected, "legit click {tick} flagged");
    }
    let air = BlockPos::new(w.x as i32, 103, w.z as i32 + 2);
    for tick in 0..40u64 {
        w.session.tick = 1000 + tick;
        w.session
            .handle_packet(click(air, Face::Up, [0.5, 1.0, 0.5]))
            .await;
        if w.session.disconnected {
            break;
        }
    }
    assert!(w.session.disconnected, "air clicks end in a kick");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicking_the_hidden_underside_of_the_floor_kicks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(40, 40)).await;
    w.session.pose.pitch = 90.0;
    let floor = BlockPos::new(w.x as i32, 100, w.z as i32);
    for tick in 0..40u64 {
        w.session.tick = tick;
        w.session
            .handle_packet(click(floor, Face::Down, [0.5, 0.0, 0.5]))
            .await;
        if w.session.disconnected {
            break;
        }
    }
    assert!(w.session.disconnected);
    world.shutdown().await;
}
