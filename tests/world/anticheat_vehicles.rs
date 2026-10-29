//! A recorded BoatFly flight replayed through the real interceptor chain,
//! and boats the server throws (blasts, pistons) or lets hover.
//! The fixture holds the vertical part of each Move Vehicle the client
//! sent (y and on_ground), from mounting on land at y 63 to floating on
//! water at sea level after the cheat was switched off.

use super::anticheat::{Walker, walker};
use super::*;
use tuff_core::protocol::play::Serverbound;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, Item};

const FLIGHT: &str = include_str!("fixtures/boatfly_trace.txt");
const FREE_FALL: usize = 175;
const TRACE_WATER: f64 = 62.0;
const WATER: i32 = 100;
const POOL_OFFSET: f64 = 6.0;
const VEHICLE: i32 = 5000;

fn flight() -> Vec<(f64, bool)> {
    FLIGHT
        .lines()
        .map(|line| {
            let (y, ground) = line.split_once(' ').expect("y and on_ground");
            (
                y.parse::<f64>().expect("y") - TRACE_WATER + f64::from(WATER),
                ground == "true",
            )
        })
        .collect()
}

pub(super) async fn place(
    w: &mut Walker,
    world: &TestWorld,
    blocks: Vec<(BlockPos, tuff_vanilla_data::BlockState)>,
) {
    let region = world.region(RegionPos::new(1, 1)).await;
    let last = *blocks.last().expect("blocks");
    place_blocks(world, &region, blocks).await;
    for _ in 0..200 {
        while let Ok(event) = w.events.try_recv() {
            w.session.on_event(event).await;
        }
        if w.session.client_world().block_at(last.0) == Some(last.1) {
            return;
        }
        world.ticks(1).await;
    }
    panic!("the client never saw the blocks");
}

/// A walled pool of water at the walker's x plus [`POOL_OFFSET`], with
/// its surface where sea level was in the trace.
async fn pool(w: &mut Walker, world: &TestWorld) {
    let (cx, cz) = ((w.x + POOL_OFFSET) as i32, w.z as i32);
    let mut blocks = Vec::new();
    for dx in -2..=2 {
        for dz in -2..=2 {
            let wall = dx * dx == 4 || dz * dz == 4;
            blocks.push((
                BlockPos::new(cx + dx, WATER - 1, cz + dz),
                Block::Stone.default_state(),
            ));
            let top = if wall { Block::Stone } else { Block::Water };
            blocks.push((BlockPos::new(cx + dx, WATER, cz + dz), top.default_state()));
        }
    }
    place(w, world, blocks).await;
}

async fn mount(w: &mut Walker, at: [f64; 3]) {
    let id = w.session.id;
    let dimension = w.session.dimension_id();
    w.session
        .handle_event(PlayerEvent::Riding(Box::new(
            crate::world::messages::Riding {
                vehicle: VEHICLE,
                kind: EntityType::OakBoat,
                dimension,
                position: at,
                riders: vec![id],
                seat: [0.0, 0.1875, 0.0],
                driver: true,
                steers: false,
                no_gravity: false,
                flying: false,
                ridden: None,
                given: Default::default(),
            },
        )))
        .await;
}

fn vehicle_y(w: &Walker) -> f64 {
    w.session.ride.riding.as_ref().expect("riding").position[1]
}

/// Sends each move, one per session tick, and returns the moves the
/// server refused (their index in `moves`).
async fn drive(w: &mut Walker, x: f64, moves: &[(f64, bool)]) -> Vec<usize> {
    let mut refused = Vec::new();
    for (index, (y, on_ground)) in moves.iter().copied().enumerate() {
        if w.session.disconnected {
            break;
        }
        w.session.tick += 1;
        w.session
            .handle_packet(Serverbound::MoveVehicle {
                position: [x, y, w.z],
                yaw: 0.0,
                pitch: 0.0,
                on_ground,
            })
            .await;
        if w.session
            .ride
            .riding
            .as_ref()
            .is_some_and(|r| r.position[1] != y)
        {
            refused.push(index);
        }
    }
    refused
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_recorded_boatfly_flight_is_set_back_and_kicked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(40, 40)).await;
    let moves = flight();
    let land = [w.x, moves[0].0, w.z];
    assert_eq!(
        land[1],
        f64::from(WATER) + 1.0,
        "the boat starts on the floor"
    );
    mount(&mut w, land).await;
    let x = w.x;
    let refused = drive(&mut w, x, &moves).await;
    let first = *refused.first().expect("the climb is refused");
    assert_eq!(first, 19, "the third climbing move: {refused:?}");
    assert!(
        vehicle_y(&w) < land[1] + 1.0,
        "the boat never got off the floor: {}",
        vehicle_y(&w)
    );
    assert!(w.session.disconnected, "a sustained flight is kicked");
    let last = *refused.last().expect("refused");
    assert_eq!(
        refused,
        (first..=last).collect::<Vec<_>>(),
        "nothing counts again until the client takes the setback"
    );
    assert!(
        last - first > 25 && last - first < 80,
        "kicked after the setback was ignored for long: {refused:?}"
    );
    let setbacks = setbacks(&mut w);
    assert!(
        (2..=5).contains(&setbacks),
        "one setback, then a re-seat every second it is ignored: {setbacks}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_recorded_fall_and_splash_after_boatfly_stops_are_accepted() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(41, 41)).await;
    pool(&mut w, &world).await;
    let moves = flight();
    let x = w.x + POOL_OFFSET;
    let at = [x, moves[FREE_FALL - 1].0, w.z];
    mount(&mut w, at).await;
    let refused = drive(&mut w, x, &moves[FREE_FALL..]).await;
    assert!(refused.is_empty(), "{refused:?}");
    assert!(!w.session.disconnected);
    let floating = vehicle_y(&w);
    assert!(
        (floating - (f64::from(WATER) + 0.5232)).abs() < 1.0e-3,
        "{floating}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boatfly_off_the_water_is_refused_and_paddling_on_it_is_not() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(42, 42)).await;
    pool(&mut w, &world).await;
    let x = w.x + POOL_OFFSET;
    let float = f64::from(WATER) + 0.5233;
    let at = [x, float, w.z];
    mount(&mut w, at).await;
    let bobbing: Vec<_> = (0..40)
        .map(|i| (float + 0.002 * (f64::from(i) * 0.7).sin(), false))
        .collect();
    assert!(drive(&mut w, x, &bobbing).await.is_empty());

    let climb: Vec<_> = (1..=20)
        .map(|i| (vehicle_y(&w) + 0.26 * f64::from(i), false))
        .collect();
    let refused = drive(&mut w, x, &climb).await;
    assert!(!refused.is_empty() && refused[0] < 5, "{refused:?}");
    assert!(vehicle_y(&w) < float + 1.5, "{}", vehicle_y(&w));
    world.shutdown().await;
}

/// Puts the walker on a real boat summoned at `at` with `nbt`, and
/// returns the boat's id.
pub(super) async fn board(w: &mut Walker, world: &TestWorld, nbt: Compound, at: [f64; 3]) -> i32 {
    board_kind(w, world, EntityType::OakBoat, nbt, at).await
}

/// Puts the walker on a real `kind` summoned at `at` with `nbt`, and
/// returns its id.
pub(super) async fn board_kind(
    w: &mut Walker,
    world: &TestWorld,
    kind: EntityType,
    nbt: Compound,
    at: [f64; 3],
) -> i32 {
    let region = world.region(RegionPos::new(1, 1)).await;
    summon(&region, kind, nbt, at).await;
    let boat = mobs(&region, kind)
        .await
        .into_iter()
        .find(|boat| crate::world::distance_sq(boat.position, at) < 1.0e-6)
        .expect("the vehicle")
        .id;
    region
        .request(RegionRequest::InteractEntity {
            player: w.handle.clone(),
            target: boat,
            interaction: interaction(&w.handle, None),
        })
        .await;
    for _ in 0..100 {
        while let Ok(event) = w.events.try_recv() {
            w.session.on_event(event).await;
        }
        if w.session
            .ride
            .riding
            .as_ref()
            .is_some_and(|r| r.vehicle == boat)
        {
            return boat;
        }
        world.ticks(1).await;
    }
    panic!("never got on");
}

/// Ticks the world, handing the walker its events, until `done` holds
/// for what the session knows of its vehicle.
async fn until_given(
    w: &mut Walker,
    world: &TestWorld,
    mut done: impl FnMut(&crate::world::messages::GivenMotion) -> bool,
) -> crate::world::messages::GivenMotion {
    for _ in 0..100 {
        world.ticks(1).await;
        while let Ok(event) = w.events.try_recv() {
            w.session.on_event(event).await;
        }
        let given = w.session.ride.riding.as_ref().expect("riding").given;
        if done(&given) {
            return given;
        }
    }
    panic!("the server never moved the boat");
}

/// The moves a client sends for a boat in the air thrown up at `speed`.
fn thrown(from: f64, speed: f64, moves: usize) -> Vec<(f64, bool)> {
    let (mut y, mut v) = (from, speed);
    (0..moves)
        .map(|_| {
            v -= 0.04;
            y += v;
            (y, false)
        })
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_flung_up_by_a_blast_is_not_refused() {
    for delay in 0..2 {
        fling_by_blast(delay).await;
    }
}

/// The blast lands `delay` ticks later, so the boat and the TNT tick in
/// either order.
async fn fling_by_blast(delay: u64) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(43, 43)).await;
    let (bx, bz) = (w.x + 4.0, w.z);
    let mut floor = Vec::new();
    for dx in -3..=3 {
        for dz in -3..=3 {
            let pos = BlockPos::new(bx as i32 + dx, 100, bz as i32 + dz);
            floor.push((pos, Block::Stone.default_state()));
        }
    }
    place(&mut w, &world, floor).await;
    let at = [bx, 101.6, bz];
    board(
        &mut w,
        &world,
        Compound::new().with("Invulnerable", true),
        at,
    )
    .await;
    world.ticks(delay).await;
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
    while w.frames.try_recv().is_ok() {}
    let region = world.region(RegionPos::new(1, 1)).await;
    summon(
        &region,
        EntityType::Tnt,
        Compound::new().with("fuse", 2i16),
        [bx, 101.0, bz],
    )
    .await;
    let given = until_given(&mut w, &world, |given| given.launch.is_some()).await;
    let (_, velocity) = given.launch.expect("launched");
    assert!(velocity[1] > 0.75, "a strong upward throw: {velocity:?}");
    let motion = tuff_vanilla_data::packet::play::Clientbound::SetEntityMotion.id();
    let mut told = false;
    while let Ok(frame) = w.frames.try_recv() {
        told |= framed_packet_id(&frame) == motion;
    }
    assert!(told, "the client hears the velocity");

    let refused = drive(&mut w, bx, &thrown(at[1], velocity[1], 25)).await;
    assert!(refused.is_empty(), "{refused:?}");
    assert!(!w.session.disconnected);

    let again = thrown(vehicle_y(&w), velocity[1], 5);
    let refused = drive(&mut w, bx, &again).await;
    assert!(!refused.is_empty(), "the throw is used up");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_on_slime_launched_by_a_piston_is_not_refused() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(44, 44)).await;
    let (px, pz) = (w.x as i32 + 4, w.z as i32);
    let piston: tuff_vanilla_data::BlockState = "minecraft:piston[facing=up]".parse().unwrap();
    place(
        &mut w,
        &world,
        vec![
            (BlockPos::new(px, 99, pz), Block::Stone.default_state()),
            (BlockPos::new(px, 100, pz), piston),
            (
                BlockPos::new(px, 101, pz),
                Block::SlimeBlock.default_state(),
            ),
        ],
    )
    .await;
    let x = f64::from(px) + 0.5;
    let at = [x, 102.0, f64::from(pz) + 0.5];
    w.z = at[2];
    board(&mut w, &world, Compound::new(), at).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: crate::world::messages::BlockOp::Set {
                changes: vec![(
                    BlockPos::new(px + 1, 100, pz),
                    Block::RedstoneBlock.default_state(),
                )],
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: None,
            },
            reply,
        })
        .await;
    answer.await.unwrap();
    let given = until_given(&mut w, &world, |given| {
        given.piston.is_some() && given.launch.is_some()
    })
    .await;
    let (_, bounce) = given.launch.expect("bounced");
    assert_eq!(bounce, [0.0, 1.0, 0.0]);
    assert!(
        w.frames
            .try_recv()
            .into_iter()
            .all(|frame| framed_packet_id(&frame)
                != tuff_vanilla_data::packet::play::Clientbound::SetEntityMotion.id()),
        "the client runs the piston itself"
    );

    let mut moves = vec![(at[1] + 0.51, false), (at[1] + 1.02, false)];
    moves.extend(thrown(at[1] + 1.02, 1.0, 25));
    let refused = drive(&mut w, x, &moves).await;
    assert!(refused.is_empty(), "{refused:?}");
    assert!(!w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_without_gravity_hovers_unrefused() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(45, 45)).await;
    let at = [w.x + 4.0, 110.0, w.z];
    board(&mut w, &world, Compound::new().with("NoGravity", true), at).await;
    assert!(w.session.ride.riding.as_ref().expect("riding").no_gravity);
    let hover = vec![(at[1], false); 40];
    let refused = drive(&mut w, at[0], &hover).await;
    assert!(refused.is_empty(), "{refused:?}");

    let climb: Vec<_> = (1..=20)
        .map(|i| (at[1] + 0.26 * f64::from(i), false))
        .collect();
    let refused = drive(&mut w, at[0], &climb).await;
    assert_eq!(
        refused.first(),
        Some(&0),
        "nothing threw it up: {refused:?}"
    );
    world.shutdown().await;
}

pub(super) fn setbacks(w: &mut Walker) -> usize {
    let back = tuff_vanilla_data::packet::play::Clientbound::MoveVehicle.id();
    let mut setbacks = 0;
    while let Ok(frame) = w.frames.try_recv() {
        setbacks += usize::from(framed_packet_id(&frame) == back);
    }
    setbacks
}

pub(super) fn vehicle_at(w: &Walker) -> [f64; 3] {
    w.session.ride.riding.as_ref().expect("riding").position
}

/// Sends one Move Vehicle per session tick to each position and returns
/// the moves the server refused (their index in `to`).
pub(super) async fn drive_to(w: &mut Walker, to: &[[f64; 3]]) -> Vec<usize> {
    let mut refused = Vec::new();
    for (index, position) in to.iter().copied().enumerate() {
        if w.session.disconnected {
            break;
        }
        w.session.tick += 1;
        w.session
            .handle_packet(Serverbound::MoveVehicle {
                position,
                yaw: -90.0,
                pitch: 0.0,
                on_ground: true,
            })
            .await;
        if w.session
            .ride
            .riding
            .as_ref()
            .is_some_and(|r| r.position != position)
        {
            refused.push(index);
        }
    }
    refused
}

/// A client honouring a setback: it answers with the position it was
/// put back to.
pub(super) async fn take_setback(w: &mut Walker) {
    let at = vehicle_at(w);
    w.session
        .handle_packet(Serverbound::MoveVehicle {
            position: at,
            yaw: -90.0,
            pitch: 0.0,
            on_ground: true,
        })
        .await;
}

/// The positions of `steps` moves east from `from`, each the given
/// distance.
pub(super) fn east(from: [f64; 3], steps: impl IntoIterator<Item = f64>) -> Vec<[f64; 3]> {
    let mut at = from;
    steps
        .into_iter()
        .map(|dx| {
            at[0] += dx;
            at
        })
        .collect()
}

/// A boat paddled forward from `speed` for `ticks` on a surface keeping
/// `friction` of its speed each tick.
pub(super) fn paddled(speed: f64, friction: f64, ticks: usize) -> Vec<f64> {
    let mut v = speed;
    (0..ticks)
        .map(|_| {
            v = v.mul_add(friction, 0.04);
            v
        })
        .collect()
}

/// A floor of `floor` blocks at y 100 from 20 blocks west of the walker
/// to 20 east, five wide, with `top` on top of it.
pub(super) async fn track(w: &mut Walker, world: &TestWorld, floor: Block, top: Option<Block>) {
    let (x, z) = (w.x as i32, w.z as i32);
    let mut blocks = Vec::new();
    for dx in -21..=21 {
        for dz in -2..=2 {
            blocks.push((BlockPos::new(x + dx, 100, z + dz), floor.default_state()));
            if let Some(top) = top {
                let side = dz * dz == 4 || dx * dx == 441;
                let block = if side { Block::Stone } else { top };
                blocks.push((BlockPos::new(x + dx, 101, z + dz), block.default_state()));
            }
        }
    }
    place(w, world, blocks).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boats_paddled_across_blue_ice_ice_and_water_are_not_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(46, 46)).await;
    track(&mut w, &world, Block::BlueIce, None).await;
    let start = [w.x - 19.0, 101.0, w.z];
    board(&mut w, &world, Compound::new(), start).await;
    let start = vehicle_at(&w);
    let refused = drive_to(&mut w, &east(start, paddled(0.0, 0.989, 40))).await;
    assert!(refused.is_empty(), "blue ice: {refused:?}");
    assert!(vehicle_at(&w)[0] > start[0] + 20.0, "{:?}", vehicle_at(&w));
    assert!(!w.session.disconnected);
    assert_eq!(setbacks(&mut w), 0);
    world.shutdown().await;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(47, 47)).await;
    track(&mut w, &world, Block::Ice, None).await;
    let start = [w.x - 19.0, 101.0, w.z];
    board(&mut w, &world, Compound::new(), start).await;
    let start = vehicle_at(&w);
    let refused = drive_to(&mut w, &east(start, paddled(0.0, 0.98, 45))).await;
    assert!(refused.is_empty(), "ice: {refused:?}");
    world.shutdown().await;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(48, 48)).await;
    track(&mut w, &world, Block::Stone, Some(Block::Water)).await;
    let start = [w.x - 19.0, f64::from(WATER) + 1.5233, w.z];
    board(&mut w, &world, Compound::new(), start).await;
    let start = vehicle_at(&w);
    let refused = drive_to(&mut w, &east(start, paddled(0.0, 0.9, 80))).await;
    assert!(refused.is_empty(), "water: {refused:?}");
    assert!(!w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sped_up_boat_is_set_back_and_kicked_but_one_that_takes_the_setback_carries_on() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(49, 49)).await;
    track(&mut w, &world, Block::Stone, Some(Block::Water)).await;
    let start = [w.x - 19.0, f64::from(WATER) + 1.5233, w.z];
    board(&mut w, &world, Compound::new(), start).await;
    let start = vehicle_at(&w);
    let cruise = paddled(0.0, 0.9, 30);
    assert!(
        drive_to(&mut w, &east(start, cruise.clone()))
            .await
            .is_empty()
    );
    let from = vehicle_at(&w);
    let refused = drive_to(&mut w, &east(from, [0.9])).await;
    assert_eq!(refused, [0], "twice the water's top speed");
    assert_eq!(setbacks(&mut w), 1);
    take_setback(&mut w).await;
    let at = vehicle_at(&w);
    let refused = drive_to(&mut w, &east(at, paddled(0.3, 0.9, 20))).await;
    assert!(refused.is_empty(), "back on track: {refused:?}");
    assert!(!w.session.disconnected);

    let from = vehicle_at(&w);
    assert_eq!(drive_to(&mut w, &east(from, [0.9])).await, [0]);
    w.session.tick += 30;
    let at = vehicle_at(&w);
    let refused = drive_to(&mut w, &east(at, paddled(0.3, 0.9, 5))).await;
    assert!(
        refused.is_empty(),
        "a client that went quiet never saw the setback: {refused:?}"
    );

    let at = vehicle_at(&w);
    let refused = drive_to(&mut w, &east(at, vec![0.9; 200])).await;
    assert!(!refused.is_empty());
    assert!(w.session.disconnected, "kicked: {refused:?}");
    assert!(vehicle_at(&w)[0] < at[0] + 1.0, "{:?}", vehicle_at(&w));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_driven_through_a_wall_is_set_back_and_kicked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(52, 52)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let wall = w.x as i32 + 3;
    let mut blocks = Vec::new();
    for y in 101..=102 {
        for dz in -2..=2 {
            blocks.push((
                BlockPos::new(wall, y, w.z as i32 + dz),
                Block::Stone.default_state(),
            ));
        }
    }
    place(&mut w, &world, blocks).await;
    let start = [w.x, 101.0, w.z];
    board(&mut w, &world, Compound::new(), start).await;
    let start = vehicle_at(&w);
    let refused = drive_to(&mut w, &east(start, vec![0.1; 200])).await;
    let face = f64::from(wall) - 0.6875;
    let first = *refused.first().expect("the wall stops it");
    assert!(
        start[0] + 0.1 * (first as f64) > face,
        "not before the boat reaches the wall: {first}"
    );
    assert!(vehicle_at(&w)[0] < f64::from(wall), "{:?}", vehicle_at(&w));
    assert!(w.session.disconnected, "kicked");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_galloping_jumping_horse_is_not_set_back_and_a_sped_up_one_is() {
    use tuff_nbt::Tag;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(53, 53)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let speed = 0.3375;
    let nbt = Compound::new()
        .with("Owner", Tag::uuid(77))
        .with("Tame", true)
        .with(
            "equipment",
            Compound::new().with(
                "saddle",
                tuff_item::item::ItemPicture::new(Item::Saddle, 1).to_nbt(),
            ),
        )
        .with(
            "attributes",
            Tag::List(vec![Tag::Compound(
                Compound::new()
                    .with("id", "minecraft:movement_speed")
                    .with("base", speed),
            )]),
        );
    let start = [w.x - 19.0, 101.0, w.z];
    board_kind(&mut w, &world, EntityType::Horse, nbt, start).await;
    let start = vehicle_at(&w);
    let riding = w.session.ride.riding.as_ref().expect("riding");
    assert!(riding.driver);
    let ridden = riding.ridden.expect("a mount's ridden speed");
    assert!((ridden.speed - speed).abs() < 1.0e-6, "{ridden:?}");

    let mut v = 0.0f64;
    let mut steps = Vec::new();
    for _ in 0..20 {
        let d = v + speed * 0.98;
        steps.push(d);
        v = d * 0.546;
    }
    let refused = drive_to(&mut w, &east(start, steps)).await;
    assert!(refused.is_empty(), "galloping: {refused:?}");
    for _ in 0..2 {
        w.session.handle_packet(Serverbound::RidingJump(100)).await;
        let mut steps = vec![v + 0.4 + speed * 0.98];
        v = steps[0] * 0.91;
        for _ in 0..10 {
            let d = v + speed * 0.098;
            steps.push(d);
            v = d * 0.91;
        }
        let moves = east(vehicle_at(&w), steps);
        let refused = drive_to(&mut w, &moves).await;
        assert!(refused.is_empty(), "leaping: {refused:?}");
    }
    assert!(!w.session.disconnected);
    assert_eq!(setbacks(&mut w), 0);

    let moves = east(vehicle_at(&w), vec![1.0; 80]);
    let refused = drive_to(&mut w, &moves).await;
    assert!(!refused.is_empty(), "a third faster than it can run");
    assert!(setbacks(&mut w) > 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_horse_may_jump_but_not_fly() {
    use tuff_nbt::Tag;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(55, 55)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let nbt = Compound::new()
        .with("Owner", Tag::uuid(77))
        .with("Tame", true)
        .with(
            "equipment",
            Compound::new().with(
                "saddle",
                tuff_item::item::ItemPicture::new(Item::Saddle, 1).to_nbt(),
            ),
        )
        .with(
            "attributes",
            Tag::List(vec![Tag::Compound(
                Compound::new()
                    .with("id", "minecraft:jump_strength")
                    .with("base", 1.0),
            )]),
        );
    let start = [w.x - 10.0, 101.0, w.z];
    board_kind(&mut w, &world, EntityType::Horse, nbt, start).await;
    let ground = vehicle_at(&w)[1];
    for _ in 0..2 {
        w.session.handle_packet(Serverbound::RidingJump(100)).await;
        let mut at = vehicle_at(&w);
        let mut path = Vec::new();
        let mut vy = 0.9f64;
        loop {
            at[0] += 0.2;
            at[1] = (at[1] + vy).max(ground);
            path.push(at);
            if at[1] <= ground {
                break;
            }
            vy = (vy - 0.08) * 0.98;
        }
        let refused = drive_to(&mut w, &path).await;
        assert!(refused.is_empty(), "jumping: {refused:?}");
    }
    assert_eq!(setbacks(&mut w), 0);

    let mut at = vehicle_at(&w);
    let mut path = Vec::new();
    for _ in 0..10 {
        at[1] += 0.3;
        path.push(at);
    }
    let refused = drive_to(&mut w, &path).await;
    assert!(refused.contains(&1), "climbing without a jump: {refused:?}");
    assert!(vehicle_at(&w)[1] < ground + 0.7, "{:?}", vehicle_at(&w));
    assert!(setbacks(&mut w) > 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camel_that_flies_or_sprints_past_its_speed_is_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(57, 57)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let nbt = Compound::new().with("Tame", true).with(
        "equipment",
        Compound::new().with(
            "saddle",
            tuff_item::item::ItemPicture::new(Item::Saddle, 1).to_nbt(),
        ),
    );
    let _ = Tag::Byte(0);
    let start = [w.x - 10.0, 101.0, w.z];
    board_kind(&mut w, &world, EntityType::Camel, nbt, start).await;
    assert!(w.session.ride.riding.as_ref().expect("riding").driver);
    let mut at = vehicle_at(&w);
    let mut path = Vec::new();
    for _ in 0..10 {
        at[1] += 0.3;
        path.push(at);
    }
    let refused = drive_to(&mut w, &path).await;
    assert!(
        refused.contains(&1),
        "a camel does not climb the air: {refused:?}"
    );
    assert!(setbacks(&mut w) > 0);
    take_setback(&mut w).await;
    let moves = east(vehicle_at(&w), vec![1.0; 80]);
    let refused = drive_to(&mut w, &moves).await;
    assert!(!refused.is_empty(), "far faster than a camel walks");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_minecart_rider_moving_its_cart_is_reset_now_and_then_and_never_kicked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(54, 54)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let start = [w.x, 101.0, w.z];
    board_kind(&mut w, &world, EntityType::Minecart, Compound::new(), start).await;
    let start = vehicle_at(&w);
    assert!(!w.session.ride.riding.as_ref().expect("riding").driver);
    let refused = drive_to(&mut w, &east(start, vec![0.5; 100])).await;
    assert_eq!(refused.len(), 100, "the server never moves it");
    assert!(!w.session.disconnected);
    let resets = setbacks(&mut w);
    assert!((3..=5).contains(&resets), "{resets}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flood_of_boat_moves_is_set_back_and_kicked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(55, 55)).await;
    track(&mut w, &world, Block::Stone, Some(Block::Water)).await;
    let start = [w.x - 19.0, f64::from(WATER) + 1.5233, w.z];
    board(&mut w, &world, Compound::new(), start).await;
    let mut at = vehicle_at(&w);
    for _ in 0..400 {
        if w.session.disconnected {
            break;
        }
        at[0] += 0.01;
        w.session
            .handle_packet(Serverbound::MoveVehicle {
                position: at,
                yaw: -90.0,
                pitch: 0.0,
                on_ground: false,
            })
            .await;
    }
    assert!(setbacks(&mut w) > 0);
    assert!(w.session.disconnected);
    world.shutdown().await;
}

pub(super) fn saddled_tame() -> Compound {
    use tuff_nbt::Tag;
    Compound::new()
        .with("Owner", Tag::uuid(77))
        .with("Tame", true)
        .with(
            "equipment",
            Compound::new().with(
                "saddle",
                tuff_item::item::ItemPicture::new(Item::Saddle, 1).to_nbt(),
            ),
        )
}

pub(super) async fn mount_at(world: &TestWorld, kind: EntityType, id: i32) -> [f64; 3] {
    let region = world.region(RegionPos::new(1, 1)).await;
    mobs(&region, kind)
        .await
        .iter()
        .find(|m| m.id == id)
        .expect("mount")
        .position
}

/// Ticks the session and world once, then sends one Move Vehicle.
pub(super) async fn ride_step(w: &mut Walker, world: &TestWorld, to: [f64; 3], on_ground: bool) {
    w.session.tick += 1;
    w.session.tick_driving().await;
    world.ticks(1).await;
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
    w.session
        .handle_packet(Serverbound::MoveVehicle {
            position: to,
            yaw: -90.0,
            pitch: 0.0,
            on_ground,
        })
        .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flying_or_speeding_driven_mount_is_held_to_the_server_run_without_enforcement() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.enforce", "false")
        .unwrap();
    let mut w = walker(&world, 1, ChunkPos::new(59, 59)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let start = [w.x - 19.0, 101.0, w.z];
    let id = board_kind(&mut w, &world, EntityType::Horse, saddled_tame(), start).await;
    let ground = mount_at(&world, EntityType::Horse, id).await;
    for _ in 0..5 {
        let here = vehicle_at(&w);
        ride_step(&mut w, &world, here, true).await;
    }
    let mut at = ground;
    for _ in 0..60 {
        at[1] += 0.4;
        ride_step(&mut w, &world, at, false).await;
    }
    let held = mount_at(&world, EntityType::Horse, id).await;
    assert!(
        held[1] < ground[1] + 2.5,
        "flew to {held:?} from {ground:?}"
    );
    assert!(setbacks(&mut w) > 0);

    take_setback(&mut w).await;
    let mut at = mount_at(&world, EntityType::Horse, id).await;
    let before = at[0];
    for _ in 0..60 {
        at[0] += 1.0;
        at[1] = ground[1];
        ride_step(&mut w, &world, at, true).await;
    }
    let held = mount_at(&world, EntityType::Horse, id).await;
    assert!(held[0] - before < 7.0, "ran to {held:?} from x {before}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_honest_ride_matching_the_server_run_is_never_clamped() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.enforce", "false")
        .unwrap();
    let mut w = walker(&world, 1, ChunkPos::new(61, 61)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let start = [w.x - 19.0, 101.0, w.z];
    let id = board_kind(&mut w, &world, EntityType::Horse, saddled_tame(), start).await;
    let _ = setbacks(&mut w);
    for _ in 0..40 {
        ride_step(
            &mut w,
            &world,
            mount_at(&world, EntityType::Horse, id).await,
            true,
        )
        .await;
    }
    assert_eq!(setbacks(&mut w), 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nautilus_driven_past_its_ridden_speed_is_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(51, 51)).await;
    track(&mut w, &world, Block::Stone, None).await;
    let start = [w.x - 10.0, 101.0, w.z];
    board_kind(&mut w, &world, EntityType::Nautilus, saddled_tame(), start).await;
    let ridden = w.session.ride.riding.as_ref().expect("riding").ridden;
    assert!(ridden.is_some_and(|r| r.speed > 0.0), "{ridden:?}");
    let moves = east(vehicle_at(&w), vec![1.0; 80]);
    let refused = drive_to(&mut w, &moves).await;
    assert!(!refused.is_empty(), "far faster than a nautilus swims");
    world.shutdown().await;
}

async fn run_past_the_server_run(
    kind: EntityType,
    nbt: Compound,
    held: Option<Item>,
    chunk: i32,
) -> (f64, usize) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.enforce", "false")
        .unwrap();
    let mut w = walker(&world, 1, ChunkPos::new(chunk, chunk)).await;
    track(&mut w, &world, Block::Stone, None).await;
    if let Some(item) = held {
        w.session.inventory_mut().set_slot(
            tuff_core::world::inventory::Inventory::hotbar_slot(0),
            Some(ItemStack::mint(item, 1, MintReason::Test)),
        );
    }
    let start = [w.x - 19.0, 101.0, w.z];
    let id = board_kind(&mut w, &world, kind, nbt, start).await;
    assert!(w.session.ride.riding.as_ref().expect("riding").driver);
    for _ in 0..5 {
        let here = vehicle_at(&w);
        ride_step(&mut w, &world, here, true).await;
    }
    let _ = setbacks(&mut w);
    let from = mount_at(&world, kind, id).await;
    let mut at = from;
    for _ in 0..60 {
        at[0] += 1.0;
        ride_step(&mut w, &world, at, true).await;
    }
    let to = mount_at(&world, kind, id).await;
    let corrections = setbacks(&mut w);
    world.shutdown().await;
    (to[0] - from[0], corrections)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camel_pig_and_strider_driven_far_past_their_run_are_held_to_it() {
    let (gain, corrections) =
        run_past_the_server_run(EntityType::Camel, saddled_tame(), None, 35).await;
    assert!(gain < 7.0 && corrections > 0, "camel {gain} {corrections}");
    let saddled = Compound::new().with(
        "equipment",
        Compound::new().with(
            "saddle",
            tuff_item::item::ItemPicture::new(Item::Saddle, 1).to_nbt(),
        ),
    );
    let (gain, corrections) = run_past_the_server_run(
        EntityType::Pig,
        saddled.clone(),
        Some(Item::CarrotOnAStick),
        37,
    )
    .await;
    assert!(gain < 7.0 && corrections > 0, "pig {gain} {corrections}");
    let (gain, corrections) = run_past_the_server_run(
        EntityType::Strider,
        saddled,
        Some(Item::WarpedFungusOnAStick),
        38,
    )
    .await;
    assert!(
        gain < 7.0 && corrections > 0,
        "strider {gain} {corrections}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_donkey_and_mule_driven_far_past_their_run_are_held_to_it() {
    for (kind, chunk) in [(EntityType::Donkey, 36), (EntityType::Mule, 37)] {
        let (gain, corrections) = run_past_the_server_run(kind, saddled_tame(), None, chunk).await;
        assert!(
            gain < 7.0 && corrections > 0,
            "{kind:?} {gain} {corrections}"
        );
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_harnessed_happy_ghast_driven_far_past_its_flight_is_held_to_it() {
    let harness = Compound::new().with(
        "equipment",
        Compound::new().with(
            "body",
            tuff_item::item::ItemPicture::new(Item::WhiteHarness, 1).to_nbt(),
        ),
    );
    let (gain, corrections) =
        run_past_the_server_run(EntityType::HappyGhast, harness, None, 39).await;
    assert!(gain < 7.0 && corrections > 0, "ghast {gain} {corrections}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_driven_far_past_what_paddling_gives_is_held_to_the_server_run() {
    let (gain, corrections) =
        run_past_the_server_run(EntityType::OakBoat, Compound::new(), None, 34).await;
    assert!(gain < 7.0 && corrections > 0, "boat {gain} {corrections}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_floating_where_the_server_run_has_it_is_never_clamped() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(33, 33)).await;
    track(&mut w, &world, Block::Stone, Some(Block::Water)).await;
    let start = [w.x - 19.0, f64::from(WATER) + 1.5233, w.z];
    let id = board_kind(&mut w, &world, EntityType::OakBoat, Compound::new(), start).await;
    let _ = setbacks(&mut w);
    for _ in 0..40 {
        let at = mount_at(&world, EntityType::OakBoat, id).await;
        ride_step(&mut w, &world, at, true).await;
    }
    assert_eq!(setbacks(&mut w), 0);
    world.shutdown().await;
}
