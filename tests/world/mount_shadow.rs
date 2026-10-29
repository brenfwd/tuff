use super::anticheat::walker;
use super::anticheat_vehicles::{
    board_kind, mount_at, place, ride_step, saddled_tame, setbacks, track,
};
use super::*;
use tuff_core::protocol::play::Serverbound;
use tuff_replay::corpus::Lane;
use tuff_vanilla_common::shadow::SHADOW_RANGE;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::registry::EntityType;

const RECORDED_HORSE_SPEED: f64 = 0.06;
const HORSE_RIDE: &str = include_str!("fixtures/mount_ride_horse.txt");

/// Replays the recorded real-client horse gallop, `ahead` blocks east of
/// where the client really was, and returns how many moves the server
/// answered with a correction.
async fn replay_horse_ride(chunk: i32, ahead: f64) -> usize {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.enforce", "false")
        .unwrap();
    let mut w = walker(&world, 1, ChunkPos::new(chunk, chunk)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let start = [w.x - 19.0, 101.0, w.z];
    let nbt = saddled_tame().with(
        "attributes",
        tuff_nbt::Tag::List(vec![tuff_nbt::Tag::Compound(
            tuff_nbt::Compound::new()
                .with("id", "minecraft:movement_speed")
                .with("base", RECORDED_HORSE_SPEED),
        )]),
    );
    let id = board_kind(&mut w, &world, EntityType::Horse, nbt, start).await;
    let origin = mount_at(&world, EntityType::Horse, id).await;
    let lane = Lane::parse_all(HORSE_RIDE, origin).unwrap().remove(0);
    w.session.pose.yaw = -90.0;
    let _ = setbacks(&mut w);
    let mut corrections = 0;
    for packet in lane.packets {
        match packet {
            Serverbound::MoveVehicle {
                position: [x, y, z],
                on_ground,
                ..
            } => {
                ride_step(&mut w, &world, [x + ahead, y, z], on_ground).await;
                corrections += setbacks(&mut w);
            }
            other => w.session.handle_packet(other).await,
        }
    }
    world.shutdown().await;
    corrections
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_recorded_horse_gallop_is_never_clamped() {
    assert_eq!(replay_horse_ride(34, 0.0).await, 0);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_recorded_horse_gallop_keeps_a_wide_margin_to_the_shadow_range() {
    let margin = SHADOW_RANGE * 0.6;
    assert_eq!(replay_horse_ride(35, margin).await, 0);
    assert!(replay_horse_ride(38, SHADOW_RANGE + 0.5).await > 0);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_horse_driven_through_a_wall_is_held_in_front_of_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.enforce", "false")
        .unwrap();
    let mut w = walker(&world, 1, ChunkPos::new(36, 36)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let wall_x = w.x as i32 - 10;
    let mut wall = Vec::new();
    for dz in -2..=2 {
        for y in 101..=104 {
            wall.push((
                BlockPos::new(wall_x, y, w.z as i32 + dz),
                Block::Stone.default_state(),
            ));
        }
    }
    place(&mut w, &world, wall).await;
    let start = [w.x - 19.0, 101.0, w.z];
    let id = board_kind(&mut w, &world, EntityType::Horse, saddled_tame(), start).await;
    w.session.pose.yaw = -90.0;
    w.session.handle_packet(Serverbound::PlayerInput(1)).await;
    let _ = setbacks(&mut w);
    let mut at = start;
    let mut corrections = 0;
    for _ in 0..120 {
        at[0] += 0.2;
        ride_step(&mut w, &world, at, true).await;
        corrections += setbacks(&mut w);
    }
    let held = mount_at(&world, EntityType::Horse, id).await;
    assert!(corrections > 0, "{held:?}");
    assert!(held[0] < f64::from(wall_x), "through the wall: {held:?}");
    world.shutdown().await;
}
