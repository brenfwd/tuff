//! The movement checks the game always makes, whatever the anticheat is
//! set to: a move too far for its packets, one the world would not allow,
//! a body pushed into blocks, and a vehicle held to the same rules.

use super::anticheat::{Walker, walker};
use super::anticheat_vehicles::{board, drive_to, east, paddled, setbacks, track, vehicle_at};
use super::*;
use crate::world::inventory::Inventory;
use tuff_core::protocol::play::{GameMode, Movement, Serverbound};
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, Item};
use tuff_world_model::Face;

fn sent(position: [f64; 3], on_ground: bool) -> Serverbound {
    Serverbound::Move(Movement {
        position: Some(position),
        rotation: None,
        on_ground,
        horizontal_collision: false,
    })
}

async fn move_to(w: &mut Walker, position: [f64; 3], on_ground: bool) {
    w.session.tick += 1;
    w.session.handle_packet(sent(position, on_ground)).await;
    w.session.handle_packet(Serverbound::ClientTickEnd).await;
    w.session.on_tick().await;
}

async fn quiet_walker(world: &TestWorld) -> Walker {
    quiet_walker_at(world, ChunkPos::new(40, 40)).await
}

async fn quiet_walker_at(world: &TestWorld, base: ChunkPos) -> Walker {
    world
        .shared
        .config
        .set_text("tuff.anticheat.enabled", "false")
        .unwrap();
    let mut w = walker(world, 1, base).await;
    w.session.skip_ticks(100);
    w
}

async fn set_back(w: &mut Walker, from: [f64; 3]) -> bool {
    let back = w.session.sync.pending_teleport.is_some();
    if back {
        assert_eq!(w.session.pose.position, from, "put back where it was");
        w.confirm().await;
    }
    back
}

fn wall(w: &Walker, dx: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    (101..=103)
        .map(|y| {
            (
                BlockPos::new(w.x.floor() as i32 + dx, y, w.z.floor() as i32),
                Block::Stone.default_state(),
            )
        })
        .collect()
}

async fn build(w: &mut Walker, blocks: Vec<(BlockPos, tuff_vanilla_data::BlockState)>) {
    for (pos, state) in blocks {
        w.session.client_world_mut().predict(pos, state);
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_teleport_of_fifty_blocks_is_rejected_and_the_player_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker(&world).await;
    let start = w.session.pose.position;
    move_to(&mut w, [start[0] + 50.0, start[1], start[2]], true).await;
    assert!(set_back(&mut w, start).await, "no setback");
    assert!(!w.session.disconnected);
    move_to(&mut w, [start[0] + 0.2, start[1], start[2]], true).await;
    assert_eq!(w.session.pose.position[0], start[0] + 0.2);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_move_pushing_into_a_wall_is_set_back_and_one_beside_it_is_not() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker(&world).await;
    let blocks = wall(&w, 1);
    build(&mut w, blocks).await;
    let start = w.session.pose.position;
    move_to(&mut w, [start[0] + 0.9, start[1], start[2]], true).await;
    assert!(set_back(&mut w, start).await, "phased into the wall");
    move_to(&mut w, [start[0] + 0.1, start[1], start[2]], true).await;
    assert!(!set_back(&mut w, start).await, "walking up to the wall");
    for _ in 0..4 {
        let at = w.session.pose.position;
        move_to(&mut w, [at[0], at[1], at[2] + 0.2], true).await;
    }
    assert!(
        w.session.sync.pending_teleport.is_none(),
        "sliding along it"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claimed_walk_through_a_wall_in_small_steps_is_stopped() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker(&world).await;
    let blocks = wall(&w, 1);
    build(&mut w, blocks).await;
    let start = w.session.pose.position;
    let mut stopped = false;
    for _ in 0..12 {
        let at = w.session.pose.position;
        move_to(&mut w, [at[0] + 0.2, at[1], at[2]], true).await;
        if w.session.sync.pending_teleport.is_some() {
            stopped = true;
            w.confirm().await;
            break;
        }
    }
    assert!(stopped);
    assert!(w.session.pose.position[0] < start[0] + 0.8);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sprint_jumps_elytra_flight_riptide_and_knockback_are_never_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker(&world).await;
    let mut at = w.session.pose.position;
    let mut vy = 0.42;
    for _ in 0..12 {
        at[0] += 0.28;
        at[1] += vy;
        vy = (vy - 0.08) * 0.98;
        let y = at[1].max(101.0);
        at[1] = y;
        move_to(&mut w, at, y <= 101.0).await;
    }
    assert!(w.session.sync.pending_teleport.is_none(), "sprint jump");

    w.session.pose.position = [at[0], 200.0, at[2]];
    let mut at = w.session.pose.position;
    w.session.motion.glide.active = true;
    for tick in 0..20 {
        let speed = 1.0 + 0.12 * f64::from(tick);
        at[0] += speed * 0.9;
        at[1] -= speed * 0.1;
        move_to(&mut w, at, false).await;
    }
    assert!(
        w.session.sync.pending_teleport.is_none(),
        "elytra with a firework"
    );
    w.session.motion.glide.active = false;

    w.session.motion.spin_attack_ticks = 20;
    for _ in 0..10 {
        at[0] += 2.0;
        at[1] += 0.5;
        move_to(&mut w, at, false).await;
    }
    assert!(w.session.sync.pending_teleport.is_none(), "riptide");

    w.session
        .on_event(PlayerEvent::Pushed([1.5, 0.4, 0.0]))
        .await;
    for _ in 0..4 {
        at[0] += 1.5;
        at[1] += 0.4;
        move_to(&mut w, at, false).await;
    }
    assert!(w.session.sync.pending_teleport.is_none(), "knockback");
    assert!(!w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_movement_check_game_rules_switch_the_speed_check_off() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker(&world).await;
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "player_movement_check",
                crate::world::game_rules::RuleValue::Bool(false),
            )
            .unwrap();
    });
    let start = w.session.pose.position;
    move_to(&mut w, [start[0] + 50.0, start[1], start[2]], true).await;
    assert!(w.session.sync.pending_teleport.is_none());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn building_waits_for_a_teleport_to_be_confirmed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker(&world).await;
    w.session.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Stone, 4, MintReason::Test)),
    );
    w.session.pose.pitch = 90.0;
    let floor = BlockPos::new(w.x as i32, 100, w.z as i32);
    let above = BlockPos::new(floor.x - 1, 100, floor.z);
    w.step(0.0, w.floor_y, 0.0).await;
    w.session.teleport(w.session.pose.position);
    w.session
        .handle_packet(Serverbound::UseItemOn {
            hand: 0,
            position: floor,
            face: Face::West,
            cursor: [0.0, 0.5, 0.5],
            sequence: 1,
        })
        .await;
    world.ticks(15).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    assert_ne!(
        block_at(&region, above).await.map(|s| s.block()),
        Some(Block::Stone),
        "placed while the teleport was unconfirmed"
    );
    w.confirm().await;
    w.session
        .handle_packet(Serverbound::UseItemOn {
            hand: 0,
            position: floor,
            face: Face::West,
            cursor: [0.0, 0.5, 0.5],
            sequence: 2,
        })
        .await;
    world.ticks(3).await;
    assert_eq!(
        block_at(&region, above).await.map(|s| s.block()),
        Some(Block::Stone),
        "placed once the teleport was confirmed"
    );
    world.shutdown().await;
}

async fn show(w: &mut Walker, kind: EntityType, dx: f64, entity: i32) {
    let position = [w.x + dx, w.floor_y, w.z];
    w.session
        .on_event(PlayerEvent::Track(crate::world::messages::Track {
            entity,
            kind,
            dimension: w.session.dimension,
            chunk: ChunkPos::of_position(position),
            seq: 1,
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
async fn an_attack_at_seven_blocks_is_ignored_and_one_at_three_lands() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker(&world).await;
    w.session.tick = 1000;
    show(&mut w, EntityType::Zombie, 7.0, 900).await;
    w.step(0.0, w.floor_y, 0.0).await;
    assert!(w.session.view.entities.is_visible(900));
    w.session.handle_packet(Serverbound::Attack(900)).await;
    assert_eq!(w.session.actions.attack_reset_tick, 0, "7 blocks away");
    show(&mut w, EntityType::Zombie, 3.0, 901).await;
    w.step(0.0, w.floor_y, 0.0).await;
    assert!(w.session.view.entities.is_visible(901));
    w.session.handle_packet(Serverbound::Attack(901)).await;
    assert_eq!(w.session.actions.attack_reset_tick, 1000, "3 blocks away");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_driver_adopts_the_position_the_region_held_its_vehicle_at() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker_at(&world, ChunkPos::new(46, 46)).await;
    track(&mut w, &world, Block::BlueIce, None).await;
    let start = [w.x - 19.0, 101.0, w.z];
    board(&mut w, &world, Compound::new(), start).await;
    let start = vehicle_at(&w);
    let far = [start[0] + 60.0, start[1], start[2]];
    drive_to(&mut w, &[far]).await;
    for _ in 0..50 {
        world.ticks(1).await;
        while let Ok(event) = w.events.try_recv() {
            w.session.on_event(event).await;
        }
        if vehicle_at(&w) == start {
            break;
        }
    }
    assert_eq!(vehicle_at(&w), start, "the session followed the claim");
    let seat = w
        .session
        .seat_position(w.session.ride.riding.as_ref().unwrap());
    assert_eq!(w.session.pose.position, seat);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_run_on_blue_ice_is_never_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker_at(&world, ChunkPos::new(47, 47)).await;
    track(&mut w, &world, Block::BlueIce, None).await;
    let start = [w.x - 19.0, 101.0, w.z];
    board(&mut w, &world, Compound::new(), start).await;
    let start = vehicle_at(&w);
    let refused = drive_to(&mut w, &east(start, paddled(0.0, 0.989, 40))).await;
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(setbacks(&mut w), 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn using_an_entity_reaches_six_blocks_from_the_eyes_to_its_box() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = quiet_walker_at(&world, ChunkPos::new(45, 45)).await;
    let range = w.session.entity_interaction_range();
    assert_eq!(range, 3.0);
    let at = |dx: f64| [w.x + dx, w.floor_y, w.z];
    let reach = |w: &Walker, dx: f64| {
        w.session
            .within_entity_reach(EntityType::Cow, at(dx), range)
    };
    assert!(reach(&w, 6.0), "box edge 5.55 away");
    assert!(!reach(&w, 6.7), "box edge 6.25 away");
    assert!(!reach(&w, 9.0), "the old nine blocks");
    w.session.mode.game_mode = GameMode::Creative;
    assert_eq!(w.session.entity_interaction_range(), 5.0);
    world.shutdown().await;
}
