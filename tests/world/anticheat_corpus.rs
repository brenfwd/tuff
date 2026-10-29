//! Replays recorded vanilla-client movement lanes
//! (`fixtures/movement_corpus.bin`, written by the opt-in
//! `real_client_record_movement_corpus` client test) through every check
//! and requires zero alerts and zero setbacks.

use super::*;
use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};
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
    stone: bool,
    floor: Option<i32>,
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
    let board = world
        .shared
        .services
        .get::<tuff_feature_builtin_vanilla_anticheat::service::Board>();
    let mut traces = board.map(|b| {
        b.set_debug("Player1", None, true);
        b.traces()
    });
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
    let stone = setup.stone;
    let recorded_swings = lane.packets.iter().any(|p| matches!(p, Serverbound::Punch));
    let floor = setup.floor;
    let airborne = setup.airborne
        || lane.packets.iter().find_map(|p| match p {
            Serverbound::Move(m) if m.position.is_some() => Some(!m.on_ground),
            _ => None,
        }) == Some(true);
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
    if std::env::var("TUFF_DUMP").as_deref() == Ok(lane.name.as_str()) {
        eprintln!("DUMP {} spawn {:?}", lane.name, lane.spawn);
        for p in lane.packets.iter().take(130) {
            eprintln!("DUMP {:?}", p);
        }
    }
    let start = setup.start.or(lane.spawn).expect("lane has a spawn");
    w.session.pose.position = start;
    w.session.pose.on_ground = !airborne;
    let (yaw, pitch) = setup.facing.unwrap_or((-90.0, 0.0));
    w.session.pose.yaw = yaw;
    w.session.pose.pitch = pitch;
    if stone {
        w.session.inventory.set_hand(
            false,
            Some(ItemStack::mint(Item::Stone, 64, MintReason::Test)),
        );
    }
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
        if let Some(floor) = floor {
            if let Serverbound::Move(m) = packet
                && m.position.is_some_and(|p| p[1] < f64::from(floor) - 1.5)
            {
                break;
            }
            if stone && let Serverbound::UseItemOn { position, face, .. } = packet {
                let at = tuff_core::world::placement::step(
                    *position,
                    tuff_core::world::placement::facing(*face),
                );
                if at.y != floor || cell_overlaps_player(at, w.session.pose.position) {
                    continue;
                }
            }
        }
        w.session.handle_packet(packet.clone()).await;
        if stone && matches!(packet, Serverbound::UseItemOn { .. }) {
            if !recorded_swings {
                w.session.handle_packet(Serverbound::Punch).await;
            }
            world.ticks(1).await;
            while let Ok(event) = w.events.try_recv() {
                w.session.on_event(event).await;
            }
            if std::env::var("TUFF_DUMP").as_deref() == Ok(lane.name.as_str())
                && let Serverbound::UseItemOn { position, face, .. } = packet
            {
                {
                    let at = tuff_core::world::placement::step(
                        *position,
                        tuff_core::world::placement::facing(*face),
                    );
                    let (reply, rx) = tokio::sync::oneshot::channel();
                    region
                        .request(RegionRequest::BlockAt { pos: at, reply })
                        .await;
                    eprintln!(
                        "PLACE {i} {at:?} -> {:?}",
                        rx.await.unwrap().map(|b| b.block())
                    );
                }
            }
        }
        if matches!(packet, Serverbound::ClientTickEnd) {
            tuff_feature_builtin_vanilla_anticheat::clock::advance(
                &w.session,
                std::time::Duration::from_millis(50),
            );
            if w.session.sync.pending_teleport.is_some() {
                eprintln!("ALERTS {:?}", operator_alerts(&world).await);
                let mut recent = Vec::new();
                while let Some(Ok(event)) = traces.as_mut().map(|t| t.try_recv()) {
                    recent.push(event.trace.to_string());
                }
                let from = recent.len().saturating_sub(6);
                for line in &recent[from..] {
                    eprintln!("TRACE {line}");
                }
            }
            assert!(
                w.session.sync.pending_teleport.is_none(),
                "lane {}: setback at packet {i} {:?} pose {:?}",
                lane.name,
                packet,
                w.session.pose.position
            );
            if w.session.disconnected {
                eprintln!("ALERTS {:?}", operator_alerts(&world).await);
            }
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
    replay_base_file("movement_corpus.bin").await;
}

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("tests/world/fixtures")
}

fn read_lanes(file: &str, origin: [f64; 3]) -> Vec<Lane> {
    let path = fixtures_dir().join(file);
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("corpus file missing at {}: {e}", path.display()));
    Lane::decode_all(&bytes, origin).unwrap_or_else(|e| panic!("{file}: {e}"))
}

fn corpus_files() -> Vec<String> {
    corpus_files_in(&fixtures_dir())
}

fn corpus_files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("movement_corpus") && n.ends_with(".bin"))
        .collect();
    names.sort();
    names
}

fn rtt_of(file: &str) -> u64 {
    file.split_once("_rtt")
        .and_then(|(_, rest)| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .unwrap_or(0)
}

/// Every recorded file at one latency profile except the two canonical
/// files, which have their own tests.
async fn replay_latency_profile(rtt: u64) {
    for name in corpus_files() {
        if name == "movement_corpus.bin"
            || name == "movement_corpus_special.bin"
            || rtt_of(&name) != rtt
        {
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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_recorded_latency_corpus_never_flags_rtt0() {
    replay_latency_profile(0).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_recorded_latency_corpus_never_flags_rtt100() {
    replay_latency_profile(100).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_recorded_latency_corpus_never_flags_rtt250() {
    replay_latency_profile(250).await;
}

#[tuff_core::watchdog]
#[test]
fn every_corpus_file_has_a_replayed_latency_profile() {
    for name in corpus_files() {
        assert!(
            [0, 100, 250].contains(&rtt_of(&name)),
            "{name}: no replay test covers this latency profile"
        );
    }
    assert_eq!(rtt_of("movement_corpus_special_rtt250_s3.bin"), 250);
}

/// Prints the manifest body for the fixtures: `name rtt styles ticks` per
/// file plus the footer. Redirect into `corpus_manifest.txt` after recording.
#[tuff_core::watchdog]
#[ignore = "manifest generator, run by hand after recording"]
#[test]
fn print_corpus_manifest() {
    fn style(lane: &str) -> &str {
        let family = lane.split('_').next().unwrap_or(lane);
        match family {
            "combat" | "elytra" | "boat" | "horse" | "bridge" => family,
            _ => lane,
        }
    }
    let (mut ticks_all, mut bytes_all) = (0u64, 0u64);
    for name in corpus_files() {
        let lanes = read_lanes(&name, [0.0; 3]);
        let ticks = lanes
            .iter()
            .flat_map(|lane| &lane.packets)
            .filter(|p| matches!(p, Serverbound::ClientTickEnd))
            .count() as u64;
        let mut styles: Vec<&str> = Vec::new();
        for lane in &lanes {
            let s = style(&lane.name);
            if !styles.contains(&s) {
                styles.push(s);
            }
        }
        styles.sort_by_key(|s| {
            ["elytra", "boat", "horse", "bridge"]
                .iter()
                .position(|k| k == s)
                .unwrap_or(usize::MAX)
        });
        println!("{name} {} {} {ticks}", rtt_of(&name), styles.join(","));
        let bridges = lanes
            .iter()
            .filter(|l| l.name.starts_with("bridge"))
            .count();
        let packets: usize = lanes.iter().map(|l| l.packets.len()).sum();
        eprintln!(
            "LANES {name} total={} bridge={bridges} packets={packets} ticks={ticks}",
            lanes.len()
        );
        ticks_all += ticks;
        bytes_all += std::fs::metadata(fixtures_dir().join(&name)).unwrap().len();
    }
    println!(
        "# {bytes_all} bytes over {ticks_all} ticks = {:.2} bytes/tick; about {:.1} h fits in 5 MB",
        bytes_all as f64 / ticks_all as f64,
        5_000_000.0 / (bytes_all as f64 / ticks_all as f64) / 72000.0
    );
}

#[tuff_core::watchdog]
#[test]
fn corpus_manifest_matches_fixtures() {
    let text = std::fs::read_to_string(fixtures_dir().join("corpus_manifest.txt"))
        .expect("corpus_manifest.txt");
    let mut listed = Vec::new();
    let mut total_ticks = 0u64;
    let mut total_bytes = 0u64;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let cols: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(cols.len(), 4, "name rtt_ms styles ticks: {line}");
        let ticks: u64 = cols[3].parse().expect("ticks");
        let actual = read_lanes(cols[0], [0.0; 3])
            .iter()
            .flat_map(|lane| &lane.packets)
            .filter(|p| matches!(p, Serverbound::ClientTickEnd))
            .count() as u64;
        total_bytes += std::fs::metadata(fixtures_dir().join(cols[0]))
            .unwrap()
            .len();
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
        "corpus: {total_ticks} ticks = {:.3} h of 10 h target, {total_bytes} bytes = {:.2} B/tick",
        total_ticks as f64 / 20.0 / 3600.0,
        total_bytes as f64 / total_ticks as f64
    );
}

async fn replay_base_file(file: &str) {
    let base = ChunkPos::new(46, 46);
    let origin = [base.x * 16 - 10, 100, base.z * 16 + 8];
    let abs = [
        f64::from(origin[0]),
        f64::from(origin[1]),
        f64::from(origin[2]),
    ];
    let lanes = read_lanes(file, abs);
    let canonical = file == "movement_corpus.bin";
    assert!(
        lanes.len() >= if canonical { 11 } else { 1 },
        "{file}: expected all recorded lanes, got {}",
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

/// A block cannot be placed where a player stands, so a recorded placement
/// into the player's own box was refused live and is left out of the replay.
fn cell_overlaps_player(cell: BlockPos, feet: [f64; 3]) -> bool {
    let overlaps = |lo: f64, hi: f64, c: i32| lo < f64::from(c + 1) && hi > f64::from(c);
    overlaps(feet[0] - 0.3, feet[0] + 0.3, cell.x)
        && overlaps(feet[1], feet[1] + 1.8, cell.y)
        && overlaps(feet[2] - 0.3, feet[2] + 0.3, cell.z)
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
    replay_special_file("movement_corpus_special.bin", Some(only)).await;
}

async fn replay_special_file(file: &str, only: Option<&str>) {
    let base = ChunkPos::new(46, 46);
    let origin = [base.x * 16 - 10, 100, base.z * 16 + 8];
    let abs = origin.map(f64::from);
    let lanes = read_lanes(file, abs);
    let canonical = file == "movement_corpus_special.bin";
    assert!(
        lanes.len() >= if canonical { 6 } else { 1 },
        "{file}: expected every special lane"
    );
    for lane in lanes
        .iter()
        .filter(|l| only.is_none_or(|name| l.name == name))
    {
        replay_special_lane(lane, base, origin).await;
    }
}

async fn replay_special_lane(lane: &Lane, base: ChunkPos, origin: [i32; 3]) {
    let name = lane.name.clone();
    {
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
                let placed = lane
                    .packets
                    .iter()
                    .any(|p| matches!(p, Serverbound::UseItemOn { .. }));
                return Setup {
                    placements: if placed {
                        Default::default()
                    } else {
                        bridge_placements(lane, origin)
                    },
                    stone: placed,
                    floor: Some(origin[1]),
                    facing: Some((90.0, 80.0)),
                    ..Setup::default()
                };
            }
            if name.starts_with("mine") {
                let mut pickaxe = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test);
                if name == "mine_instant" {
                    pickaxe.components.enchantments = Some(tuff_item::item::Enchantments::new([(
                        "minecraft:efficiency".to_owned(),
                        5,
                    )]));
                    w.session
                        .body
                        .effects
                        .add(tuff_core::world::effects::ActiveEffect::timed(
                            tuff_vanilla_data::registry::MobEffect::Haste,
                            2400,
                            1,
                        ));
                }
                w.session.inventory.set_hand(false, Some(pickaxe));
                return Setup {
                    facing: Some((-90.0, 0.0)),
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

/// Replays every lane of every corpus file on its own and lists the lanes
/// that flag. With `TUFF_CORPUS_PRUNE=1` it moves them out of the fixture
/// into `target/corpus-flagged/`. `TUFF_CORPUS_DIR=<dir>` reads a scratch
/// directory instead of the fixtures and never prunes; the panic message of
/// each flagged lane (packet index, pose, alerts) goes to stderr. Run after recording new corpus files,
/// then update `corpus_manifest.txt`.
#[tuff_core::watchdog]
#[ignore = "corpus triage tool, run by hand after recording"]
#[test]
fn triage_corpus_lanes() {
    let scratch = std::env::var_os("TUFF_CORPUS_DIR").map(std::path::PathBuf::from);
    let prune = scratch.is_none() && std::env::var("TUFF_CORPUS_PRUNE").as_deref() == Ok("1");
    let dir = scratch.clone().unwrap_or_else(fixtures_dir);
    let only = std::env::var("TUFF_CORPUS_FILES").ok();
    let flagged_dir = fixtures_dir().join("../../../target/corpus-flagged");
    let base = ChunkPos::new(46, 46);
    let origin = [base.x * 16 - 10, 100, base.z * 16 + 8];
    let mut total = 0;
    for file in corpus_files_in(&dir) {
        if only.as_deref().is_some_and(|o| !file.starts_with(o)) {
            continue;
        }
        let bytes = std::fs::read(dir.join(&file)).unwrap();
        let special = file.starts_with("movement_corpus_special");
        let mut kept = Lane::encode_all(&[], [0.0; 3]);
        let mut dropped = kept.clone();
        for chunk in Lane::split_records(&bytes).unwrap() {
            let lane = Lane::decode_record(chunk, origin.map(f64::from)).unwrap();
            let name = lane.name.clone();
            let run = std::thread::spawn(move || {
                let _fixtures = tuff_item::item::conservation::fixture_thread();
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    if special {
                        replay_special_lane(&lane, base, origin).await;
                    } else {
                        replay_lane(&lane, base, origin).await;
                    }
                });
            });
            if run.join().is_ok() {
                kept.extend_from_slice(chunk);
            } else {
                println!("FLAGGED {file} {name}");
                dropped.extend_from_slice(chunk);
                total += 1;
            }
        }
        if prune && dropped.len() > tuff_replay::corpus::MAGIC.len() {
            std::fs::create_dir_all(&flagged_dir).unwrap();
            std::fs::write(flagged_dir.join(&file), dropped).unwrap();
            std::fs::write(fixtures_dir().join(&file), kept).unwrap();
        }
    }
    println!("triage: {total} flagged lane(s)");
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

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_mining_never_flags() {
    for file in [
        "movement_corpus_special_mine.bin",
        "movement_corpus_special_mine_rtt100.bin",
    ] {
        let base = ChunkPos::new(46, 46);
        let abs = [base.x * 16 - 10, 100, base.z * 16 + 8].map(f64::from);
        for lane in read_lanes(file, abs) {
            let (mut starts, mut stops, mut swings, mut tick) = (0, 0, 0, 0);
            let (mut last_swing, mut worst_gap) = (0, 0);
            for p in &lane.packets {
                match p {
                    Serverbound::ClientTickEnd => tick += 1,
                    Serverbound::Punch => {
                        swings += 1;
                        worst_gap = worst_gap.max(tick - last_swing);
                        last_swing = tick;
                    }
                    Serverbound::PlayerAction { status, .. } => match status {
                        PlayerActionStatus::StartDestroyBlock => starts += 1,
                        PlayerActionStatus::StopDestroyBlock => stops += 1,
                        _ => {}
                    },
                    _ => {}
                }
            }
            eprintln!(
                "BREAKS {file} {}: ticks {tick} starts {starts} stops {stops} swings {swings} worst swing gap {worst_gap}",
                lane.name
            );
            assert!(
                starts >= 3 && swings >= 3 && (stops >= 1 || lane.name == "mine_instant"),
                "{file} {}",
                lane.name
            );
        }
    }
    replay_special_file("movement_corpus_special_mine.bin", None).await;
    replay_special_file("movement_corpus_special_mine_rtt100.bin", None).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mining_while_walking_never_flags() {
    for file in [
        "movement_corpus_special_mine_walk.bin",
        "movement_corpus_special_mine_walk_rtt100.bin",
    ] {
        let base = ChunkPos::new(46, 46);
        let abs = [base.x * 16 - 10, 100, base.z * 16 + 8].map(f64::from);
        for lane in read_lanes(file, abs) {
            let start = lane.start().unwrap();
            let (mut furthest, mut stops) = (start[0], 0);
            for p in &lane.packets {
                match p {
                    Serverbound::Move(tuff_core::protocol::play::Movement {
                        position: Some(pos),
                        ..
                    }) => furthest = furthest.max(pos[0]),
                    Serverbound::PlayerAction {
                        status: PlayerActionStatus::StopDestroyBlock,
                        ..
                    } => stops += 1,
                    _ => {}
                }
            }
            assert!(
                furthest - start[0] > 8.0 && stops >= 4,
                "{file} {}: walked {} blocks, {stops} stops",
                lane.name,
                furthest - start[0]
            );
        }
        replay_special_file(file, None).await;
    }
}

#[tuff_core::watchdog]
#[test]
fn a_placement_inside_the_players_box_is_left_out_of_the_replay() {
    let feet = [727.17, 101.0, 744.5];
    assert!(cell_overlaps_player(BlockPos::new(727, 101, 744), feet));
    assert!(cell_overlaps_player(BlockPos::new(726, 102, 744), feet));
    assert!(!cell_overlaps_player(BlockPos::new(726, 100, 744), feet));
    assert!(!cell_overlaps_player(BlockPos::new(728, 101, 744), feet));
    assert!(!cell_overlaps_player(BlockPos::new(727, 103, 744), feet));
}
