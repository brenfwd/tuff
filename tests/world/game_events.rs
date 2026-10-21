//! Game events and vibrations: sculk sensors hearing block changes and
//! footsteps, wool muffling them, calibrated sensors filtering by
//! frequency, and the shrieker and warden chains that hang off them.

use super::*;
use crate::world::messages::{BlockOp, BlockOpResult, BreakMode, PlayerLooks};
use tuff_vanilla_data::registry::EntityType;

const Y: i32 = 100;

struct Bench {
    world: TestWorld,
    region: RegionHandle,
    player: PlayerHandle,
    warning: i32,
    _events: mpsc::UnboundedReceiver<PlayerEvent>,
}

async fn bench(dir: &Path) -> Bench {
    bench_in(TestWorld::start(dir)).await
}

async fn bench_in(world: TestWorld) -> Bench {
    let (player, mut events) = PlayerHandle::new(1000);
    view_around(&world, &player, &mut events, ChunkPos::new(0, 0)).await;
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    Bench {
        world,
        region,
        player,
        warning: 0,
        _events: events,
    }
}

fn at(x: i32, z: i32) -> BlockPos {
    BlockPos::new(x, Y, z)
}

impl Bench {
    async fn carry_warning(&mut self) {
        let mut warning = None;
        while let Ok(event) = self._events.try_recv() {
            if let PlayerEvent::WardenTracker(t) = event {
                warning = Some(t.warning_level);
            }
        }
        let looks = PlayerLooks {
            warden_warning: warning.unwrap_or(self.warning) as u8,
            ..Default::default()
        };
        self.warning = looks.warden_warning.into();
        self.region
            .request(player_at(&self.player, [9.5, SKY as f64 + 1.5, 9.5], looks))
            .await;
    }

    async fn break_block(&self, pos: BlockPos, sequence: i32) {
        self.region
            .request(RegionRequest::Break {
                player: self.player.clone(),
                pos,
                tool: None,
                mode: BreakMode::Creative,
                sequence,
            })
            .await;
    }

    async fn phase(&self, pos: BlockPos) -> String {
        block_at(&self.region, pos)
            .await
            .and_then(|s| s.get("sculk_sensor_phase").map(str::to_owned))
            .unwrap_or_default()
    }

    async fn power(&self, pos: BlockPos) -> u8 {
        block_at(&self.region, pos)
            .await
            .and_then(|s| s.power())
            .unwrap_or(0)
    }

    async fn ticks_until_phase(&self, pos: BlockPos, phase: &str, limit: u64) -> bool {
        for _ in 0..limit {
            if self.phase(pos).await == phase {
                return true;
            }
            self.world.ticks(1).await;
        }
        self.phase(pos).await == phase
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sensor_hears_a_block_break_then_cools_down() {
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    let sensor = at(8, 8);
    place_blocks(
        &b.world,
        &b.region,
        [
            (sensor, Block::SculkSensor.default_state()),
            (at(11, 8), Block::Cobblestone.default_state()),
        ],
    )
    .await;
    assert_eq!(b.phase(sensor).await, "inactive");
    b.break_block(at(11, 8), 1).await;
    assert!(b.ticks_until_phase(sensor, "active", 12).await);
    assert_eq!(b.power(sensor).await, 10);
    assert!(b.ticks_until_phase(sensor, "cooldown", 40).await);
    assert_eq!(b.power(sensor).await, 0);
    assert!(b.ticks_until_phase(sensor, "inactive", 20).await);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sensor_ignores_what_is_out_of_range_or_behind_wool() {
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    let far = at(14, 8);
    let walled = at(11, 8);
    let control = at(5, 8);
    let mut blocks = vec![
        (far, Block::SculkSensor.default_state()),
        (walled, Block::SculkSensor.default_state()),
        (control, Block::SculkSensor.default_state()),
        (at(1, 8), Block::Cobblestone.default_state()),
        (at(15, 8), Block::Cobblestone.default_state()),
    ];
    for dy in -1..=1 {
        for dz in -1..=1 {
            blocks.push((
                BlockPos::new(13, Y + dy, 8 + dz),
                Block::WhiteWool.default_state(),
            ));
        }
    }
    place_blocks(&b.world, &b.region, blocks).await;
    b.break_block(at(1, 8), 1).await;
    assert!(b.ticks_until_phase(control, "active", 12).await);
    assert_eq!(b.phase(far).await, "inactive");
    b.world.ticks(60).await;
    assert_eq!(b.phase(control).await, "inactive");
    b.break_block(at(15, 8), 2).await;
    b.world.ticks(12).await;
    assert_eq!(b.phase(walled).await, "inactive");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_calibrated_sensor_hears_only_its_signal_strength() {
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    let facing = |z| {
        Block::CalibratedSculkSensor
            .default_state()
            .with("facing", "north")
            .map(|s| (at(8, z), s))
            .unwrap()
    };
    let filtered = facing(6);
    let open = facing(10);
    place_blocks(
        &b.world,
        &b.region,
        [
            filtered,
            open,
            (at(8, 7), Block::RedstoneBlock.default_state()),
            (at(11, 8), Block::Cobblestone.default_state()),
        ],
    )
    .await;
    b.break_block(at(11, 8), 1).await;
    assert!(b.ticks_until_phase(open.0, "active", 12).await);
    assert_eq!(b.phase(filtered.0).await, "inactive");
    b.world.shutdown().await;
}

fn walking(player: &PlayerHandle, x: f64, sneaking: bool) -> RegionRequest {
    let looks = PlayerLooks {
        eye_height: 1.62,
        height: 1.8,
        sneaking,
        ..Default::default()
    };
    let mut request = player_at(player, [x, f64::from(Y), 8.5], looks);
    if let RegionRequest::Player(update) = &mut request {
        update.grounded = true;
        update.seq = (x * 100.0) as u64;
    }
    request
}

async fn walk(b: &Bench, sneaking: bool) {
    for i in 0..40 {
        b.region
            .request(walking(&b.player, 2.5 + f64::from(i) * 0.25, sneaking))
            .await;
        b.world.ticks(1).await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walking_is_heard_but_sneaking_is_not() {
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    let loud = at(6, 12);
    place_blocks(
        &b.world,
        &b.region,
        [(loud, Block::SculkSensor.default_state())],
    )
    .await;
    walk(&b, true).await;
    b.world.ticks(10).await;
    assert_eq!(b.phase(loud).await, "inactive");
    walk(&b, false).await;
    assert!(b.ticks_until_phase(loud, "active", 30).await);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sneaking_past_a_sensor_reports_the_vibration_avoided() {
    let dir = TestDir::new();
    let mut b = bench(dir.path()).await;
    let sensor = at(6, 12);
    place_blocks(
        &b.world,
        &b.region,
        [(sensor, Block::SculkSensor.default_state())],
    )
    .await;
    walk(&b, true).await;
    let mut avoided = false;
    while let Ok(event) = b._events.try_recv() {
        if let PlayerEvent::Advancement(event) = event
            && matches!(
                *event,
                crate::world::messages::AdvancementEvent::AvoidVibration
            )
        {
            avoided = true;
        }
    }
    assert!(avoided);
    assert_eq!(b.phase(sensor).await, "inactive");
    b.world.shutdown().await;
}

fn pen_blocks() -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in 1..=14 {
        for z in 1..=14 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
            blocks.push((BlockPos::new(x, SKY + 6, z), Block::Stone.default_state()));
            if x == 1 || x == 14 || z == 1 || z == 14 {
                for y in 1..=5 {
                    blocks.push((BlockPos::new(x, SKY + y, z), Block::Stone.default_state()));
                }
            }
        }
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_sensor_clicks_make_a_shrieker_summon_a_warden() {
    let dir = TestDir::new();
    let mut b = bench(dir.path()).await;
    let mut blocks = pen_blocks();
    let shrieker = BlockPos::new(7, SKY + 1, 7);
    blocks.push((
        shrieker,
        Block::SculkShrieker
            .default_state()
            .with("can_summon", "true")
            .unwrap(),
    ));
    let sensor = BlockPos::new(8, SKY + 1, 7);
    blocks.push((sensor, Block::SculkSensor.default_state()));
    for i in 0..4 {
        blocks.push((
            BlockPos::new(10 + (i % 2), SKY + 1, 9 + i / 2),
            Block::Cobblestone.default_state(),
        ));
    }
    place_blocks(&b.world, &b.region, blocks).await;
    b.world.settle(0).await;
    b.carry_warning().await;
    assert_eq!(mobs(&b.region, EntityType::Warden).await.len(), 0);
    for i in 0..4 {
        let pos = BlockPos::new(10 + (i % 2), SKY + 1, 9 + i / 2);
        b.break_block(pos, i).await;
        b.world.ticks(210).await;
        b.carry_warning().await;
    }
    let found = wait_for(&b.world, &b.region, 140, |found| {
        found.iter().any(|s| s.kind == EntityType::Warden)
    })
    .await;
    let id = found
        .iter()
        .find(|s| s.kind == EntityType::Warden)
        .unwrap()
        .id;
    let emerging = with_warden(&b.region, id, |w| w.species.is_emerging()).await;
    assert_eq!(
        emerging,
        Some(true),
        "a summoned warden rises from the ground"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shrieker_without_can_summon_never_calls_a_warden() {
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    let mut blocks = pen_blocks();
    blocks.push((
        BlockPos::new(7, SKY + 1, 7),
        Block::SculkShrieker.default_state(),
    ));
    blocks.push((
        BlockPos::new(8, SKY + 1, 7),
        Block::SculkSensor.default_state(),
    ));
    blocks.push((
        BlockPos::new(10, SKY + 1, 9),
        Block::Cobblestone.default_state(),
    ));
    place_blocks(&b.world, &b.region, blocks).await;
    b.break_block(BlockPos::new(10, SKY + 1, 9), 1).await;
    b.world.ticks(120).await;
    let state = block_at(&b.region, BlockPos::new(7, SKY + 1, 7))
        .await
        .unwrap();
    assert_eq!(state.get("shrieking"), Some("false"));
    assert_eq!(mobs(&b.region, EntityType::Warden).await.len(), 0);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_warden_goes_to_investigate_a_noise() {
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    let mut blocks = pen_blocks();
    blocks.push((
        BlockPos::new(12, SKY + 1, 12),
        Block::Cobblestone.default_state(),
    ));
    place_blocks(&b.world, &b.region, blocks).await;
    summon(
        &b.region,
        EntityType::Warden,
        Compound::new().with("PersistenceRequired", 1i8),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    b.world.ticks(60).await;
    let before = mobs(&b.region, EntityType::Warden).await[0].position;
    b.break_block(BlockPos::new(12, SKY + 1, 12), 1).await;
    b.world.ticks(80).await;
    let after = mobs(&b.region, EntityType::Warden).await[0].position;
    let gap = |p: [f64; 3]| ((p[0] - 12.5).powi(2) + (p[2] - 12.5).powi(2)).sqrt();
    assert!(
        gap(after) < gap(before) - 2.0,
        "warden stayed put: {before:?} -> {after:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_trapdoor_is_heard() {
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    let sensor = at(8, 8);
    let door = at(11, 8);
    let trapdoor = Block::OakTrapdoor.default_state();
    place_blocks(
        &b.world,
        &b.region,
        [
            (sensor, Block::SculkSensor.default_state()),
            (door, trapdoor),
        ],
    )
    .await;
    b.world.ticks(10).await;
    assert_eq!(b.phase(sensor).await, "inactive");
    let open = trapdoor.with("open", "true").unwrap();
    place_blocks(&b.world, &b.region, [(door, open)]).await;
    assert!(b.ticks_until_phase(sensor, "active", 12).await);
    b.world.shutdown().await;
}

fn standing_on(player: &PlayerHandle, position: [f64; 3]) -> RegionRequest {
    match player_at(player, position, PlayerLooks::default()) {
        RegionRequest::Player(mut update) => {
            update.grounded = true;
            RegionRequest::Player(update)
        }
        other => other,
    }
}

async fn darkness_within(b: &mut Bench, ticks: u64) -> bool {
    for _ in 0..ticks {
        while let Ok(event) = b._events.try_recv() {
            if let PlayerEvent::AddEffect(effect) = event
                && effect.effect == tuff_vanilla_data::registry::MobEffect::Darkness
            {
                return true;
            }
        }
        b.world.ticks(1).await;
    }
    false
}

fn summoning_shrieker() -> tuff_vanilla_data::BlockState {
    Block::SculkShrieker
        .default_state()
        .with("can_summon", "true")
        .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stepping_on_a_shrieker_sets_it_off_and_its_answer_brings_darkness() {
    let dir = TestDir::new();
    let mut b = bench(dir.path()).await;
    let mut blocks = pen_blocks();
    let shrieker = BlockPos::new(7, SKY + 1, 7);
    blocks.push((shrieker, summoning_shrieker()));
    place_blocks(&b.world, &b.region, blocks).await;
    b.world.settle(0).await;
    while b._events.try_recv().is_ok() {}
    b.region
        .request(standing_on(&b.player, [7.5, SKY as f64 + 1.5, 7.5]))
        .await;
    b.world.ticks(2).await;
    let state = block_at(&b.region, shrieker).await.unwrap();
    assert_eq!(state.get("shrieking"), Some("true"), "a step sets it off");
    assert!(
        !darkness_within(&mut b, 80).await,
        "no answer before the shriek ends"
    );
    assert!(
        darkness_within(&mut b, 20).await,
        "the first warning's answer"
    );
    let state = block_at(&b.region, shrieker).await.unwrap();
    assert_eq!(state.get("shrieking"), Some("false"));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shrieker_broken_mid_shriek_answers_at_once() {
    let dir = TestDir::new();
    let mut b = bench(dir.path()).await;
    let mut blocks = pen_blocks();
    let shrieker = BlockPos::new(7, SKY + 1, 7);
    blocks.push((shrieker, summoning_shrieker()));
    place_blocks(&b.world, &b.region, blocks).await;
    b.world.settle(0).await;
    while b._events.try_recv().is_ok() {}
    b.region
        .request(standing_on(&b.player, [7.5, SKY as f64 + 1.5, 7.5]))
        .await;
    b.world.ticks(5).await;
    b.break_block(shrieker, 1).await;
    assert!(
        darkness_within(&mut b, 5).await,
        "breaking it ends the shriek"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mounting_and_dismounting_are_heard() {
    use crate::world::messages::{RideAnswer, RideOp, RidePassenger};
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    let sensor = at(8, 8);
    place_blocks(
        &b.world,
        &b.region,
        [(sensor, Block::SculkSensor.default_state())],
    )
    .await;
    summon(
        &b.region,
        EntityType::Cow,
        Compound::new().with("NoAI", true).with("NoGravity", true),
        [11.5, f64::from(Y), 8.5],
    )
    .await;
    b.world.ticks(2).await;
    let cow = mobs(&b.region, EntityType::Cow).await[0].id;
    let (rider, _rider_events) = PlayerHandle::new(3000);
    let passenger = RidePassenger::Player {
        handle: rider,
        uuid: 3000,
    };
    let ask = |op: RideOp| {
        let region = b.region.clone();
        async move { region.ask(move |world| world.ride(op)).await.unwrap() }
    };
    assert_eq!(b.phase(sensor).await, "inactive");
    let mounted = ask(RideOp::Mount {
        passenger: passenger.clone(),
        vehicle: cow,
    })
    .await;
    assert!(matches!(mounted, RideAnswer::Vehicle(v) if v.id == cow));
    assert!(b.ticks_until_phase(sensor, "active", 12).await);
    assert_eq!(b.power(sensor).await, 10);
    assert!(b.ticks_until_phase(sensor, "inactive", 80).await);
    ask(RideOp::Dismount { passenger }).await;
    assert!(b.ticks_until_phase(sensor, "active", 12).await);
    b.world.shutdown().await;
}

async fn saved_listener(b: &Bench, pos: BlockPos) -> Option<tuff_nbt::Compound> {
    let BlockOpResult::Copied(copied) = blocks(
        &b.region,
        BlockOp::Copy {
            positions: vec![pos],
        },
    )
    .await
    else {
        panic!("not a copy");
    };
    copied
        .into_iter()
        .next()
        .flatten()
        .and_then(|(_, nbt)| nbt)
        .and_then(|nbt| nbt.get_compound("listener").cloned())
}

async fn vibration_survives_a_restart(first: TestWorld, second: impl FnOnce() -> TestWorld) {
    let sensor = at(8, 8);
    let b = bench_in(first).await;
    place_blocks(
        &b.world,
        &b.region,
        [
            (sensor, Block::SculkSensor.default_state()),
            (at(14, 8), Block::Cobblestone.default_state()),
        ],
    )
    .await;
    b.break_block(at(14, 8), 1).await;
    let mut saved = None;
    for _ in 0..4 {
        b.world.ticks(1).await;
        saved = saved_listener(&b, sensor).await;
        if saved.is_some() {
            break;
        }
    }
    let saved = saved.expect("an in-flight vibration is stored in the block entity");
    assert_eq!(
        saved
            .get_compound("event")
            .and_then(|e| e.get_str("game_event")),
        Some("minecraft:block_destroy")
    );
    assert_eq!(b.phase(sensor).await, "inactive");
    let Bench {
        world,
        region,
        player,
        warning: _,
        _events,
    } = b;
    world.shutdown().await;
    drop((region, player, _events));

    let b = bench_in(second()).await;
    assert!(b.ticks_until_phase(sensor, "active", 30).await);
    assert!(saved_listener(&b, sensor).await.is_none());
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vibration_in_flight_survives_a_restart_in_the_vanilla_layout() {
    let dir = TestDir::new();
    vibration_survives_a_restart(TestWorld::start(dir.path()), || {
        TestWorld::start(dir.path())
    })
    .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn filling_and_emptying_a_bucket_are_heard() {
    use super::fluids::{floor, pour};
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    floor(&b.region, (0, 12), (0, 12), false).await;
    let sensor = BlockPos::new(8, 201, 8);
    place_blocks(
        &b.world,
        &b.region,
        [(sensor, Block::SculkSensor.default_state())],
    )
    .await;
    b.world.ticks(10).await;
    assert_eq!(b.phase(sensor).await, "inactive");
    b.region
        .request(pour(
            &b.player,
            BlockPos::new(4, 200, 8),
            Item::WaterBucket,
            1,
        ))
        .await;
    assert!(b.ticks_until_phase(sensor, "active", 12).await);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vibration_in_flight_survives_a_restart_in_the_sqlite_backend() {
    let dir = TestDir::new();
    let backend = std::sync::Arc::new(
        tuff_storage::sqlite::SqliteBackend::open(dir.path().join("world.mcserver")).unwrap(),
    );
    let open = || TestWorld::start_with(backend.clone());
    vibration_survives_a_restart(open(), open).await;
}
