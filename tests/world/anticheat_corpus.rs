//! Replays recorded vanilla-client movement lanes
//! (`fixtures/movement_corpus.txt`, written by the opt-in
//! `real_client_record_movement_corpus` client test) through every check
//! and requires zero alerts and zero setbacks.

use super::*;
use tuff_core::protocol::play::Serverbound;
use tuff_replay::corpus::Lane;

use super::anticheat::{Walker, operator_alerts, physical_walker};

fn state_of(text: &str) -> tuff_vanilla_data::BlockState {
    let (name, props) = match text.split_once('[') {
        Some((n, rest)) => (n, rest.trim_end_matches(']')),
        None => (text, ""),
    };
    let mut state = tuff_vanilla_data::Block::from_name(name)
        .unwrap_or_else(|| panic!("unknown block {name}"))
        .default_state();
    for pair in props.split(',').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap();
        state = state
            .with(k, v)
            .unwrap_or_else(|| panic!("bad property {pair}"));
    }
    state
}

#[derive(Default)]
struct Setup {
    rockets: Vec<usize>,
    start: Option<[f64; 3]>,
    airborne: bool,
    facing: Option<(f32, f32)>,
    summons: Vec<(&'static str, [f64; 3])>,
    placements: std::collections::BTreeMap<usize, Vec<(BlockPos, tuff_vanilla_data::BlockState)>>,
}

async fn replay_lane(lane: &Lane, base: ChunkPos, origin: [i32; 3]) {
    replay_lane_with(lane, base, origin, |_, _| Setup::default()).await;
}

async fn replay_lane_with(
    lane: &Lane,
    base: ChunkPos,
    origin: [i32; 3],
    prepare: impl FnOnce(&mut Walker, &mut Vec<(BlockPos, tuff_vanilla_data::BlockState)>) -> Setup,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.enforceUncalibrated", "true")
        .unwrap();
    let mut w = physical_walker(&world, 1, base).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let mut blocks = Vec::new();
    for f in &lane.fills {
        let state = state_of(&f.state);
        for x in f.from[0].min(f.to[0])..=f.from[0].max(f.to[0]) {
            for y in f.from[1].min(f.to[1])..=f.from[1].max(f.to[1]) {
                for z in f.from[2].min(f.to[2])..=f.from[2].max(f.to[2]) {
                    blocks.push((
                        BlockPos::new(origin[0] + x, origin[1] + y, origin[2] + z),
                        state,
                    ));
                }
            }
        }
    }
    let setup = prepare(&mut w, &mut blocks);
    let rockets = setup.rockets;
    let placements = setup.placements;
    for (id, position) in &setup.summons {
        let (reply, summoned) = tokio::sync::oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt: tuff_nbt::Compound::new().with("id", *id),
                position: *position,
                fresh: true,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap(), "{id} summons");
    }
    place_blocks(&world, &region, blocks).await;
    world.ticks(2).await;
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
    let start = setup.start.or(lane.spawn).expect("lane has a spawn");
    w.session.pose.position = start;
    w.session.pose.on_ground = !setup.airborne;
    let (yaw, pitch) = setup.facing.unwrap_or((-90.0, 0.0));
    w.session.pose.yaw = yaw;
    w.session.pose.pitch = pitch;
    for (i, packet) in lane.packets.iter().enumerate() {
        if let Some(blocks) = placements.get(&i) {
            place_blocks(&world, &region, blocks.clone()).await;
            world.ticks(1).await;
            while let Ok(event) = w.events.try_recv() {
                w.session.on_event(event).await;
            }
        }
        if rockets.contains(&i) {
            w.session
                .handle_packet(Serverbound::UseItem {
                    off_hand: false,
                    sequence: 0,
                })
                .await;
        }
        w.session.handle_packet(packet.clone()).await;
        if matches!(packet, Serverbound::ClientTickEnd) {
            tuff_feature_builtin_vanilla_anticheat::clock::advance(
                &w.session,
                std::time::Duration::from_millis(50),
            );
            if w.session.sync.pending_teleport.is_some() {
                eprintln!("ALERTS {:?}", operator_alerts(&world).await);
            }
            assert!(
                w.session.sync.pending_teleport.is_none(),
                "lane {}: setback at packet {i} {:?} pose {:?}",
                lane.name,
                packet,
                w.session.pose.position
            );
            assert!(!w.session.disconnected, "lane {}: kicked", lane.name);
        }
    }
    let alerts = operator_alerts(&world).await;
    assert!(alerts.is_empty(), "lane {}: {alerts:?}", lane.name);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_vanilla_movement_never_flags() {
    replay_base_file("movement_corpus.txt").await;
}

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("tests/world/fixtures")
}

fn corpus_files() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("movement_corpus") && n.ends_with(".txt"))
        .collect();
    names.sort();
    names
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_recorded_latency_corpus_never_flags() {
    for name in corpus_files() {
        if name == "movement_corpus.txt" || name == "movement_corpus_special.txt" {
            continue;
        }
        if name.starts_with("movement_corpus_special") {
            replay_special_file(&name, None).await;
        } else {
            replay_base_file(&name).await;
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn corpus_manifest_matches_fixtures() {
    let text = std::fs::read_to_string(fixtures_dir().join("corpus_manifest.txt"))
        .expect("corpus_manifest.txt");
    let mut listed = Vec::new();
    let mut total_ticks = 0u64;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let cols: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(cols.len(), 4, "name rtt_ms styles ticks: {line}");
        let ticks: u64 = cols[3].parse().expect("ticks");
        let actual = std::fs::read_to_string(fixtures_dir().join(cols[0]))
            .unwrap_or_else(|e| panic!("{}: {e}", cols[0]))
            .lines()
            .filter(|l| *l == "T")
            .count() as u64;
        assert_eq!(ticks, actual, "{}: tick count", cols[0]);
        total_ticks += ticks;
        listed.push(cols[0].to_owned());
    }
    listed.sort();
    assert_eq!(
        listed,
        corpus_files(),
        "manifest must list every corpus file"
    );
    println!(
        "corpus: {total_ticks} ticks = {:.3} h of 10 h target",
        total_ticks as f64 / 20.0 / 3600.0
    );
}

async fn replay_base_file(file: &str) {
    let path = fixtures_dir().join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("movement corpus missing at {}: {e}", path.display()));
    let base = ChunkPos::new(46, 46);
    let origin = [base.x * 16 - 10, 100, base.z * 16 + 8];
    let abs = [
        f64::from(origin[0]),
        f64::from(origin[1]),
        f64::from(origin[2]),
    ];
    let lanes = Lane::parse_all(&text, abs).unwrap();
    assert!(
        lanes.len() >= 11,
        "expected all recorded lanes, got {}",
        lanes.len()
    );
    for lane in &lanes {
        replay_lane(lane, base, origin).await;
    }
}

/// Packet indices where a rocket was fired, read off the recording: the
/// client sent no use-item packets into the corpus, but each rocket shows
/// as a sudden gain in horizontal speed.
fn rocket_ticks(lane: &Lane) -> Vec<usize> {
    let mut last: Option<[f64; 3]> = None;
    let mut speed = 0.0_f64;
    let mut quiet = 100;
    let mut out = Vec::new();
    for (i, packet) in lane.packets.iter().enumerate() {
        match packet {
            Serverbound::Move(m) => {
                if let Some(pos) = m.position {
                    if let Some(prev) = last {
                        let now = (pos[0] - prev[0]).hypot(pos[2] - prev[2]);
                        if now > speed + 0.1 && quiet >= 10 {
                            out.push(i);
                            quiet = 0;
                        }
                        speed = now;
                    }
                    last = Some(pos);
                }
            }
            Serverbound::ClientTickEnd => quiet += 1,
            _ => {}
        }
    }
    out
}

/// The blocks the client placed while bridging, each just before the
/// first move that needed it. The corpus holds no use-item packets, but
/// a grounded move whose body overlaps no block is only possible once
/// the client has put one under its feet.
fn bridge_placements(
    lane: &Lane,
    origin: [i32; 3],
) -> std::collections::BTreeMap<usize, Vec<(BlockPos, tuff_vanilla_data::BlockState)>> {
    let stone = state_of("minecraft:stone");
    let floor_y = origin[1];
    let mut solid: std::collections::BTreeSet<(i32, i32)> = std::collections::BTreeSet::new();
    for f in &lane.fills {
        if f.from[1] == 0 {
            for x in f.from[0]..=f.to[0] {
                for z in f.from[2]..=f.to[2] {
                    solid.insert((origin[0] + x, origin[2] + z));
                }
            }
        }
    }
    let mut out: std::collections::BTreeMap<usize, Vec<_>> = std::collections::BTreeMap::new();
    let mut sneaking = false;
    let mut previous_supported = true;
    for (i, packet) in lane.packets.iter().enumerate() {
        if let Serverbound::PlayerInput(bits) = packet {
            sneaking = bits & 32 != 0;
        }
        let Serverbound::Move(m) = packet else {
            continue;
        };
        let Some([x, y, z]) = m.position else {
            continue;
        };
        let columns = |dx: f64, dz: f64| ((x + dx).floor() as i32, (z + dz).floor() as i32);
        let footprint = [
            columns(-0.31, -0.31),
            columns(0.31, -0.31),
            columns(-0.31, 0.31),
            columns(0.31, 0.31),
        ];
        let supported = footprint.iter().any(|c| solid.contains(c));
        let at_floor = (f64::from(floor_y) + 0.999..=f64::from(floor_y) + 1.001).contains(&y);
        if at_floor && !supported && (sneaking || !previous_supported) {
            let under = columns(0.0, 0.0);
            solid.insert(under);
            out.entry(i)
                .or_default()
                .push((BlockPos::new(under.0, floor_y, under.1), stone));
        }
        previous_supported = footprint.iter().any(|c| solid.contains(c));
    }
    out
}

async fn replay_special(only: &str) {
    replay_special_file("movement_corpus_special.txt", Some(only)).await;
}

async fn replay_special_file(file: &str, only: Option<&str>) {
    let path = fixtures_dir().join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("special corpus missing at {}: {e}", path.display()));
    let base = ChunkPos::new(46, 46);
    let origin = [base.x * 16 - 10, 100, base.z * 16 + 8];
    let abs = origin.map(f64::from);
    let lanes = Lane::parse_all(&text, abs).unwrap();
    assert!(lanes.len() >= 6, "{file}: expected every special lane");
    for lane in lanes
        .iter()
        .filter(|l| only.is_none_or(|name| l.name == name))
    {
        let name = lane.name.clone();
        replay_lane_with(lane, base, origin, |w, blocks| {
            if name.starts_with("elytra") {
                w.session.inventory.set_slot(
                    crate::world::inventory::Inventory::armor_slot(
                        tuff_vanilla_data::item::EquipmentSlot::Chest,
                    )
                    .unwrap(),
                    Some(ItemStack::mint(Item::Elytra, 1, MintReason::Test)),
                );
                w.session.inventory.set_hand(
                    false,
                    Some(ItemStack::mint(Item::FireworkRocket, 64, MintReason::Test)),
                );
                return Setup {
                    rockets: rocket_ticks(lane),
                    start: lane.start(),
                    airborne: true,
                    ..Setup::default()
                };
            }
            if name.starts_with("bridge") {
                return Setup {
                    placements: bridge_placements(lane, origin),
                    facing: Some((90.0, 80.0)),
                    ..Setup::default()
                };
            }
            if name.starts_with("boat") {
                let falling = state_of("minecraft:water[level=8]");
                for x in 4..=40 {
                    for y in -8..=-1 {
                        blocks.push((
                            BlockPos::new(origin[0] + x, origin[1] + y, origin[2]),
                            falling,
                        ));
                    }
                }
                return Setup {
                    summons: vec![("minecraft:oak_boat", lane.spawn.unwrap())],
                    ..Setup::default()
                };
            }
            Setup::default()
        })
        .await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_elytra_rockets_never_flags() {
    replay_special("elytra_rockets").await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_boat_water_never_flags() {
    replay_special("boat_water").await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_horse_gallop_never_flags() {
    replay_special("horse_gallop").await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_bridge_sneak_never_flags() {
    replay_special("bridge_sneak").await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_bridge_jump_never_flags() {
    replay_special("bridge_jump").await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_bridge_speed_pulse_never_flags() {
    replay_special("bridge_speed_pulse").await;
}
