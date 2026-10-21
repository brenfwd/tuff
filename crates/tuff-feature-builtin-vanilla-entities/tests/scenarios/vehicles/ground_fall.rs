use super::*;

pub(super) async fn load_chunk(
    world: &TestWorld,
    region: &RegionHandle,
    session: &mut crate::world::session::PlayerSession,
) {
    for _ in 0..400 {
        world.ticks(1).await;
        pump(session).await;
        if block_at(region, BlockPos::new(8, 0, 8)).await.is_some() {
            return;
        }
    }
    panic!("chunk never loaded");
}

pub(super) async fn pump(session: &mut crate::world::session::PlayerSession) {
    while let Ok(event) = session.conn.events.try_recv() {
        session.on_event(event).await;
    }
}

async fn mount_on_land_and_wait(kind: EntityType, nbt: Compound) -> (f32, f64) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (mut session, _frames) = session_with_outbound(&world, 100);
    let handle = session.conn.handle.clone();
    session.view.subscribed.insert(chunk);
    region
        .request(RegionRequest::Subscribe {
            player: handle.clone(),
            chunks: vec![chunk],
        })
        .await;
    load_chunk(&world, &region, &mut session).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    session.stand_at([6.5, SKY as f64 + 1.0, 4.5], true);
    session.publish(true).await;
    summon(&region, kind, nbt, [6.5, SKY as f64 + 1.0, 6.5]).await;
    let vehicle = mobs(&region, kind).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: handle.clone(),
            target: vehicle,
            interaction: interaction(&handle, None),
        })
        .await;
    let mut lowest = session.health();
    for _ in 0..60 {
        world.ticks(1).await;
        pump(&mut session).await;
        if session.ride.riding.is_some() {
            session
                .drive([6.5, SKY as f64 + 1.0, 6.5], 0.0, 0.0, true)
                .await;
            let seat = session.pose.position;
            session
                .handle_packet(Serverbound::Move(Movement {
                    position: Some([seat[0], -999.0, seat[2]]),
                    rotation: Some((0.0, 0.0)),
                    on_ground: false,
                    horizontal_collision: false,
                }))
                .await;
            session.handle_packet(Serverbound::ClientTickEnd).await;
        }
        session.run_tick().await;
        lowest = lowest.min(session.health());
    }
    let riding = session.ride.riding.is_some();
    assert!(riding, "the player is on the {kind:?}");
    let fall = session.body.fall.distance;
    world.shutdown().await;
    (lowest, fall)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn riding_a_boat_on_land_does_not_hurt() {
    let (lowest, fall) = mount_on_land_and_wait(EntityType::OakBoat, Compound::new()).await;
    assert_eq!(lowest, 20.0);
    assert_eq!(fall, 0.0);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn riding_a_chest_boat_and_a_raft_on_land_does_not_hurt() {
    for kind in [EntityType::OakChestBoat, EntityType::BambooRaft] {
        let (lowest, _) = mount_on_land_and_wait(kind, Compound::new()).await;
        assert_eq!(lowest, 20.0, "{kind:?}");
    }
}

struct Mount {
    floor: tuff_vanilla_data::Block,
    sprint: bool,
    fall: f64,
    start_y: f64,
    boat_at: [f64; 3],
    walls: bool,
    hole: bool,
    embedded: bool,
}

impl Default for Mount {
    fn default() -> Self {
        Self {
            floor: tuff_vanilla_data::Block::GrassBlock,
            sprint: false,
            fall: 0.0,
            start_y: SKY as f64 + 1.0,
            boat_at: [6.5, SKY as f64 + 1.0, 6.5],
            walls: false,
            hole: false,
            embedded: false,
        }
    }
}

async fn mount_realistically(m: Mount) -> Vec<String> {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (mut session, _frames) = session_with_outbound(&world, 100);
    let handle = session.conn.handle.clone();
    session.view.subscribed.insert(chunk);
    region
        .request(RegionRequest::Subscribe {
            player: handle.clone(),
            chunks: vec![chunk],
        })
        .await;
    load_chunk(&world, &region, &mut session).await;
    let mut blocks = pen(2, 13);
    for (pos, state) in &mut blocks {
        if pos.y == SKY && state.block() == tuff_vanilla_data::Block::GrassBlock {
            *state = m.floor.default_state();
        }
    }
    if m.hole {
        for x in 2..=13 {
            for z in 2..=13 {
                if (x, z) != (6, 6) {
                    blocks.push((
                        BlockPos::new(x, SKY + 1, z),
                        tuff_vanilla_data::Block::Sand.default_state(),
                    ));
                }
            }
        }
    }
    if m.walls {
        for dx in [-1, 1] {
            for dy in 0..2 {
                blocks.push((
                    BlockPos::new(m.boat_at[0].floor() as i32 + dx, SKY + 1 + dy, 6),
                    tuff_vanilla_data::Block::Stone.default_state(),
                ));
            }
        }
    }
    place_blocks(&world, &region, blocks).await;
    let hole = BlockPos::new(6, SKY + 1, 6);
    if m.embedded {
        place_blocks(
            &world,
            &region,
            vec![(hole, tuff_vanilla_data::Block::Sand.default_state())],
        )
        .await;
        session.stand_at([6.5, SKY as f64 + 1.0, 6.5], true);
    } else {
        session.stand_at([6.5, m.start_y, 4.5], m.start_y <= SKY as f64 + 1.0);
    }
    session.body.fall = crate::world::player::Fall::new(m.fall);
    session.pose.sprinting = m.sprint;
    session.publish(true).await;
    if m.embedded {
        for _ in 0..30 {
            world.ticks(1).await;
            pump(&mut session).await;
            session.run_tick().await;
        }
        assert!(session.health() < 20.0, "buried player suffocates");
        place_blocks(
            &world,
            &region,
            vec![(hole, tuff_vanilla_data::Block::Air.default_state())],
        )
        .await;
        session.body.health.heal(20.0, 20.0);
        for _ in 0..10 {
            world.ticks(1).await;
            pump(&mut session).await;
            session.run_tick().await;
        }
    }
    summon(&region, EntityType::OakBoat, Compound::new(), m.boat_at).await;
    let vehicle = mobs(&region, EntityType::OakBoat).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: handle.clone(),
            target: vehicle,
            interaction: interaction(&handle, None),
        })
        .await;
    let mut log = Vec::new();
    let mut last = session.health();
    for tick in 0..200 {
        world.ticks(1).await;
        pump(&mut session).await;
        if session.ride.riding.is_some() {
            let boat = mobs(&region, EntityType::OakBoat).await[0].position;
            session.handle_packet(Serverbound::PlayerInput(0)).await;
            session.drive(boat, 0.0, 0.0, true).await;
            let seat = session.pose.position;
            session
                .handle_packet(Serverbound::Move(Movement {
                    position: Some([seat[0], -999.0, seat[2]]),
                    rotation: Some((0.0, 0.0)),
                    on_ground: false,
                    horizontal_collision: false,
                }))
                .await;
            session
                .handle_packet(Serverbound::Move(Movement {
                    position: None,
                    rotation: Some((10.0, 5.0)),
                    on_ground: false,
                    horizontal_collision: false,
                }))
                .await;
            session.handle_packet(Serverbound::ClientTickEnd).await;
        }
        session.run_tick().await;
        if session.health() != last {
            log.push(format!(
                "tick {tick}: {last} -> {} at {:?} fall {}",
                session.health(),
                session.pose.position,
                session.body.fall.distance
            ));
            last = session.health();
        }
    }
    assert!(session.ride.riding.is_some(), "mounted");
    world.shutdown().await;
    log
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn riding_a_boat_never_hurts_on_any_ground() {
    use tuff_vanilla_data::Block;
    let cases = [
        ("grass", Mount::default()),
        (
            "sand",
            Mount {
                floor: Block::Sand,
                ..Mount::default()
            },
        ),
        (
            "snow",
            Mount {
                floor: Block::SnowBlock,
                ..Mount::default()
            },
        ),
        (
            "ice",
            Mount {
                floor: Block::Ice,
                ..Mount::default()
            },
        ),
        (
            "sprint",
            Mount {
                sprint: true,
                ..Mount::default()
            },
        ),
        (
            "falling",
            Mount {
                fall: 6.0,
                ..Mount::default()
            },
        ),
        (
            "ledge",
            Mount {
                start_y: SKY as f64 + 4.0,
                fall: 3.0,
                ..Mount::default()
            },
        ),
        (
            "walls",
            Mount {
                walls: true,
                ..Mount::default()
            },
        ),
        (
            "embedded",
            Mount {
                hole: true,
                embedded: true,
                boat_at: [6.5, SKY as f64 + 2.0, 6.5],
                ..Mount::default()
            },
        ),
        (
            "hole",
            Mount {
                hole: true,
                start_y: SKY as f64 + 1.0,
                boat_at: [6.5, SKY as f64 + 2.0, 6.5],
                ..Mount::default()
            },
        ),
    ];
    for (name, case) in cases {
        let log = mount_realistically(case).await;
        assert!(log.is_empty(), "{name}: {log:?}");
    }
}

const BOAT_TRACE: &str = include_str!("../fixtures/boat_ride_trace.txt");

async fn replay_boat_trace(embedded: bool) -> Vec<String> {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (mut session, _frames) = session_with_outbound(&world, 100);
    let handle = session.conn.handle.clone();
    session.view.subscribed.insert(chunk);
    region
        .request(RegionRequest::Subscribe {
            player: handle.clone(),
            chunks: vec![chunk],
        })
        .await;
    load_chunk(&world, &region, &mut session).await;
    let mut blocks = pen(2, 13);
    for x in 2..=13 {
        for z in 2..=13 {
            if (x, z) != (6, 6) || embedded {
                blocks.push((
                    BlockPos::new(x, SKY + 1, z),
                    tuff_vanilla_data::Block::Sand.default_state(),
                ));
            }
        }
    }
    place_blocks(&world, &region, blocks).await;
    let origin = [6.5, SKY as f64 + 2.0, 6.5];
    session.stand_at([6.5, SKY as f64 + 1.0, 6.5], true);
    session.publish(true).await;
    if embedded {
        for _ in 0..30 {
            world.ticks(1).await;
            pump(&mut session).await;
            session.run_tick().await;
        }
        place_blocks(
            &world,
            &region,
            vec![(
                BlockPos::new(6, SKY + 1, 6),
                tuff_vanilla_data::Block::Air.default_state(),
            )],
        )
        .await;
        session.body.health.heal(20.0, 20.0);
        for _ in 0..20 {
            world.ticks(1).await;
            pump(&mut session).await;
            session.run_tick().await;
        }
    }
    summon(&region, EntityType::OakBoat, Compound::new(), origin).await;
    let vehicle = mobs(&region, EntityType::OakBoat).await[0].id;
    let mut log = Vec::new();
    let mut last = session.health();
    for (tick, line) in BOAT_TRACE.lines().enumerate() {
        for event in line.split(';').filter(|e| !e.is_empty()) {
            let parts: Vec<&str> = event.split(' ').collect();
            let num = |i: usize| parts[i].parse::<f64>().unwrap();
            match parts[0] {
                "X" => {
                    region
                        .request(RegionRequest::InteractEntity {
                            player: handle.clone(),
                            target: vehicle,
                            interaction: interaction(&handle, None),
                        })
                        .await;
                }
                "P" => {
                    session
                        .handle_packet(Serverbound::PaddleBoat {
                            left: parts[1] == "true",
                            right: parts[2] == "true",
                        })
                        .await;
                }
                "I" => {
                    session
                        .handle_packet(Serverbound::PlayerInput(parts[1].parse().unwrap()))
                        .await;
                }
                "R" => {
                    session
                        .handle_packet(Serverbound::Move(Movement {
                            position: None,
                            rotation: Some((num(1) as f32, num(2) as f32)),
                            on_ground: parts[3] == "true",
                            horizontal_collision: false,
                        }))
                        .await;
                }
                "V" => {
                    session
                        .handle_packet(Serverbound::MoveVehicle {
                            position: [origin[0] + num(1), origin[1] + num(2), origin[2] + num(3)],
                            yaw: num(4) as f32,
                            pitch: num(5) as f32,
                            on_ground: parts[6] == "true",
                        })
                        .await;
                }
                other => panic!("{other}"),
            }
        }
        world.ticks(1).await;
        pump(&mut session).await;
        session.run_tick().await;
        if session.health() != last {
            log.push(format!(
                "tick {tick}: {last} -> {} at {:?} pose {:?} cramped {}",
                session.health(),
                session.pose.position,
                session.current_pose(),
                session.surroundings.contact.cramped
            ));
            last = session.health();
        }
    }
    assert!(session.ride.riding.is_some(), "mounted");
    world.shutdown().await;
    log
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replaying_a_recorded_boat_ride_does_not_hurt() {
    for embedded in [false, true] {
        let log = replay_boat_trace(embedded).await;
        assert!(log.is_empty(), "embedded {embedded}: {log:?}");
    }
}
