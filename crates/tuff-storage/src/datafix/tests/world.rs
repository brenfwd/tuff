use std::fs;
use std::path::Path;
use tuff_core::storage::codec::WorldBackendExt as _;

use super::*;
use crate::region_file::{RawChunk, RegionFile};
use crate::test_dir::TestDir;
use tuff_core::storage::level::LevelData;
use tuff_core::world::player::Difficulty;
use tuff_storage::backend::WorldBackend;
use tuff_storage::files::{read_nbt_file, write_nbt_file};
use tuff_storage::vanilla::VanillaBackend;
use tuff_world_model::BlockPos;

const UUID: u128 = 0x069a79f4_44e9_4726_a5be_fca90e38aaf5;
const UUID_FILE: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

fn saved(data: Compound, version: i32) -> Compound {
    Compound::new()
        .with("data", data)
        .with("DataVersion", version)
}

fn legacy_level() -> Compound {
    let rules = Compound::new()
        .with("keepInventory", "true")
        .with("doFireTick", "false")
        .with("doDaylightCycle", "false")
        .with("doWeatherCycle", "false")
        .with("randomTickSpeed", "7")
        .with("disableRaids", "true");
    let world_gen = Compound::new()
        .with("seed", 1234i64)
        .with("generate_features", 1i8)
        .with("bonus_chest", 0i8)
        .with(
            "dimensions",
            Compound::new().with(
                "minecraft:overworld",
                Compound::new().with("type", "minecraft:overworld").with(
                    "generator",
                    Compound::new()
                        .with("type", "minecraft:noise")
                        .with("settings", "minecraft:overworld")
                        .with(
                            "biome_source",
                            Compound::new()
                                .with("type", "minecraft:multi_noise")
                                .with("preset", "minecraft:overworld"),
                        ),
                ),
            ),
        );
    let player = Compound::new()
        .with("UUID", Tag::uuid(UUID))
        .with("DataVersion", V1_21_4);
    let data = Compound::new()
        .with("DataVersion", V1_21_4)
        .with("LevelName", "old")
        .with("Time", 5000i64)
        .with("DayTime", 13000i64)
        .with("Difficulty", 3i8)
        .with("hardcore", 1i8)
        .with("SpawnX", 16)
        .with("SpawnY", 70)
        .with("SpawnZ", -8)
        .with("SpawnAngle", 90.0f32)
        .with("BorderCenterX", 80.0f64)
        .with("BorderSize", 1000.0f64)
        .with("raining", 1i8)
        .with("rainTime", 1200)
        .with("GameRules", rules)
        .with("WorldGenSettings", world_gen)
        .with("WanderingTraderSpawnDelay", 4800)
        .with(
            "DragonFight",
            Compound::new()
                .with("DragonKilled", 1i8)
                .with("PreviouslyKilled", 1i8),
        )
        .with("Player", player);
    Compound::new().with("Data", data)
}

fn write_old_chunk(dir: &Path) {
    let mut chunk = legacy_chunk();
    chunk.insert("xPos", 1);
    chunk.insert("zPos", 2);
    let mut bytes = Vec::new();
    tuff_nbt::NamedTag::new("", chunk)
        .write(&mut bytes)
        .unwrap();
    let mut region = RegionFile::open(dir, 0, 0, true).unwrap().unwrap();
    region
        .write(ChunkPos::new(1, 2), &RawChunk::compress(bytes).unwrap())
        .unwrap();
    region.sync().unwrap();
}

fn old_world(root: &Path) {
    fs::create_dir_all(root).unwrap();
    write_nbt_file(&root.join("level.dat"), legacy_level()).unwrap();
    write_old_chunk(&root.join("region"));
    fs::create_dir_all(root.join("DIM-1/region")).unwrap();
    fs::write(root.join("DIM-1/region/r.0.0.mca"), b"").unwrap();
    fs::create_dir_all(root.join("DIM1/data")).unwrap();
    write_nbt_file(
        &root.join("DIM1/data/raids_end.dat"),
        saved(Compound::new().with("Tick", 9), V1_21_4),
    )
    .unwrap();
    fs::create_dir_all(root.join("data")).unwrap();
    write_nbt_file(
        &root.join("data/scoreboard.dat"),
        saved(Compound::new(), V1_21_4),
    )
    .unwrap();
    write_nbt_file(
        &root.join("data/map_3.dat"),
        saved(Compound::new().with("scale", 1i8), V1_21_4),
    )
    .unwrap();
    write_nbt_file(
        &root.join("data/chunks.dat"),
        saved(Compound::new().with("Forced", vec![5i64]), V1_21_4),
    )
    .unwrap();
    write_nbt_file(&root.join("data/villages.dat"), Compound::new()).unwrap();
    fs::create_dir_all(root.join("playerdata")).unwrap();
    write_nbt_file(
        &root.join(format!("playerdata/{UUID_FILE}.dat")),
        Compound::new()
            .with("DataVersion", V1_21_4)
            .with("FallDistance", 3.0f32),
    )
    .unwrap();
    fs::create_dir_all(root.join("stats")).unwrap();
    fs::write(
        root.join(format!("stats/{UUID_FILE}.json")),
        br#"{"stats":{},"DataVersion":4189}"#,
    )
    .unwrap();
}

#[tuff_core::watchdog]
#[test]
fn pre_split_world_is_moved_and_split() {
    let dir = TestDir::new();
    let root = &dir.path().join("world");
    old_world(root);
    assert!(layout::upgrade_world(root).unwrap());
    for path in [
        "dimensions/minecraft/overworld/region/r.0.0.mca",
        "dimensions/minecraft/the_nether/region/r.0.0.mca",
        "dimensions/minecraft/the_end/data/minecraft/raids.dat",
        "dimensions/minecraft/overworld/data/minecraft/chunk_tickets.dat",
        "data/minecraft/scoreboard.dat",
        "data/minecraft/maps/3.dat",
        "data/minecraft/game_rules.dat",
        "data/minecraft/world_gen_settings.dat",
        "data/minecraft/world_clocks.dat",
        "data/minecraft/weather.dat",
        "data/minecraft/wandering_trader.dat",
        "dimensions/minecraft/the_end/data/minecraft/ender_dragon_fight.dat",
        "dimensions/minecraft/the_nether/data/minecraft/world_border.dat",
    ] {
        assert!(root.join(path).exists(), "{path} is missing");
    }
    assert!(root.join(format!("players/data/{UUID_FILE}.dat")).exists());
    assert!(
        root.join(format!("players/stats/{UUID_FILE}.json"))
            .exists()
    );
    for gone in [
        "region",
        "DIM-1",
        "DIM1",
        "playerdata",
        "stats",
        "data/villages.dat",
    ] {
        assert!(!root.join(gone).exists(), "{gone} should be gone");
    }
    let level = read_nbt_file(&root.join("level.dat")).unwrap().unwrap();
    let data = level.get_compound("Data").unwrap();
    assert_eq!(data.get_int("DataVersion"), Some(DATA_VERSION));
    assert!(!data.contains_key("Player") && !data.contains_key("GameRules"));
    assert_eq!(
        data.get_int_array("singleplayer_uuid").map(<[i32]>::len),
        Some(4)
    );
    let nether_border = read_nbt_file(
        &root.join("dimensions/minecraft/the_nether/data/minecraft/world_border.dat"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        nether_border
            .get_compound("data")
            .unwrap()
            .get_double("center_x"),
        Some(10.0)
    );
    let rules = read_nbt_file(&root.join("data/minecraft/game_rules.dat"))
        .unwrap()
        .unwrap();
    let rules = rules.get_compound("data").unwrap();
    assert_eq!(
        rules.get_bool("minecraft:spectators_generate_chunks"),
        Some(true)
    );

    assert!(!layout::upgrade_world(root).unwrap());
}

#[tuff_core::watchdog]
#[test]
fn upgraded_world_loads_through_the_backend() {
    let dir = TestDir::new();
    let root = &dir.path().join("world");
    old_world(root);
    let backend = VanillaBackend::new(root);
    let _lock = backend.lock().unwrap();
    backend.upgrade().unwrap();
    let level = LevelData::load(
        backend.documents(),
        || panic!("the old seed must be kept"),
        |_| panic!("the old spawn must be kept"),
    )
    .unwrap();
    assert!(!level.new_world);
    assert_eq!(level.seed, 1234);
    assert_eq!(level.spawn.pos, BlockPos::new(16, 70, -8));
    assert_eq!(level.spawn.yaw, 90.0);
    assert_eq!(level.difficulty, Difficulty::Hard);
    assert!(level.game_rules.bool("keep_inventory"));
    assert!(!level.game_rules.bool("raids"));
    assert_eq!(level.game_rules.int("random_tick_speed"), 7);
    assert!(!level.game_rules.bool("advance_time"));
    assert!(!level.game_rules.bool("advance_weather"));
    assert_eq!(level.game_rules.int("fire_spread_radius_around_player"), 0);
    assert!(level.weather.raining);
    assert_eq!(level.weather.rain_time, 1200);
    let overworld = level.dimension("minecraft:overworld").unwrap();
    assert_eq!(overworld.border.center, [80.0, 0.0]);
    assert_eq!(overworld.border.size, 1000.0);
    assert!(overworld.generate_structures);

    let player = backend
        .documents()
        .read(tuff_storage::document::Document::Player(UUID))
        .unwrap()
        .unwrap();
    assert_eq!(player.get_double("fall_distance"), Some(3.0));
    assert_eq!(player.get_int("DataVersion"), Some(DATA_VERSION));
    let tickets = backend
        .documents()
        .read(tuff_storage::document::Document::DimensionData {
            dimension: "minecraft:overworld",
            id: "minecraft:chunk_tickets",
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        tickets
            .get_compound("data")
            .unwrap()
            .get_list("tickets")
            .map(<[Tag]>::len),
        Some(1)
    );

    let mut store = backend
        .open_region(
            "minecraft:overworld",
            tuff_world_model::pos::FileRegionPos::new(0, 0),
        )
        .unwrap();
    let blob = store.read_chunk(ChunkPos::new(1, 2)).unwrap().unwrap();
    let decoded = backend
        .chunk_codec()
        .decode_chunk(&blob, ChunkPos::new(1, 2), ChunkShape::OVERWORLD)
        .unwrap();
    let tuff_world_model::chunk_meta::Decoded::Chunk(chunk, _) = decoded else {
        panic!("expected a full chunk");
    };
    assert_eq!(
        chunk.get(0, -64, 0).block(),
        tuff_vanilla_data::Block::ShortGrass
    );
}

#[tuff_core::watchdog]
#[test]
fn interrupted_upgrade_finishes_on_the_next_run() {
    let dir = TestDir::new();
    let root = &dir.path().join("world");
    old_world(root);
    fs::create_dir_all(root.join("dimensions/minecraft/overworld")).unwrap();
    fs::rename(
        root.join("region"),
        root.join("dimensions/minecraft/overworld/region"),
    )
    .unwrap();
    fs::create_dir_all(root.join("players")).unwrap();
    fs::rename(root.join("playerdata"), root.join("players/data")).unwrap();
    assert!(layout::upgrade_world(root).unwrap());
    assert!(
        root.join("dimensions/minecraft/overworld/region/r.0.0.mca")
            .exists()
    );
    assert!(root.join(format!("players/data/{UUID_FILE}.dat")).exists());
    assert!(root.join("data/minecraft/world_gen_settings.dat").exists());
}

#[tuff_core::watchdog]
#[test]
fn current_and_missing_worlds_are_left_alone() {
    let dir = TestDir::new();
    assert!(!layout::upgrade_world(dir.path()).unwrap());
    write_nbt_file(
        &dir.path().join("level.dat"),
        Compound::new().with("Data", Compound::new().with("DataVersion", DATA_VERSION)),
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("region")).unwrap();
    assert!(!layout::upgrade_world(dir.path()).unwrap());
    assert!(dir.path().join("region").exists());
}

#[tuff_core::watchdog]
#[test]
fn corrupt_level_dat_fails_without_moving_anything() {
    let dir = TestDir::new();
    let root = dir.path();
    fs::create_dir_all(root.join("region")).unwrap();
    fs::write(root.join("level.dat"), b"\x1f\x8b\x08\x00garbage").unwrap();
    assert!(layout::upgrade_world(root).is_err());
    assert!(root.join("region").exists());
}

#[tuff_core::watchdog]
#[test]
fn worlds_before_1_13_are_refused_without_moving_anything() {
    let dir = TestDir::new();
    let root = dir.path().join("world");
    fs::create_dir_all(root.join("region")).unwrap();
    write_nbt_file(
        &root.join("level.dat"),
        Compound::new().with(
            "Data",
            Compound::new().with("DataVersion", MIN_DATA_VERSION - 1),
        ),
    )
    .unwrap();
    let before = tree(&root);
    let err = layout::upgrade_world(&root).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(tree(&root), before);
    assert_eq!(sibling_names(dir.path()), ["world"]);
}

fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn sibling_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tuff_core::watchdog]
#[test]
fn upgrade_leaves_an_untouched_backup() {
    let dir = TestDir::new();
    let root = &dir.path().join("world");
    old_world(root);
    fs::write(root.join("session.lock"), b"lock").unwrap();
    let mut original = tree(root);
    original.retain(|(path, _)| path != "session.lock");
    assert!(layout::upgrade_world(root).unwrap());
    assert_eq!(sibling_names(dir.path()), ["world", "world OUTDATED"]);
    assert_eq!(tree(&dir.path().join("world OUTDATED")), original);
    assert!(!root.join("upgrade_backup.txt").exists());
    assert!(!layout::upgrade_world(root).unwrap());
    assert_eq!(sibling_names(dir.path()), ["world", "world OUTDATED"]);
}

#[tuff_core::watchdog]
#[test]
fn interrupted_upgrade_keeps_the_first_backup() {
    let dir = TestDir::new();
    let root = &dir.path().join("world");
    old_world(root);
    let original = tree(root);
    let backup = layout::back_up(root).unwrap();
    assert_eq!(backup.file_name().unwrap(), "world OUTDATED");
    fs::create_dir_all(root.join("dimensions/minecraft/overworld")).unwrap();
    fs::rename(
        root.join("region"),
        root.join("dimensions/minecraft/overworld/region"),
    )
    .unwrap();
    assert!(layout::upgrade_world(root).unwrap());
    assert_eq!(sibling_names(dir.path()), ["world", "world OUTDATED"]);
    assert_eq!(tree(&dir.path().join("world OUTDATED")), original);
    assert!(!root.join("upgrade_backup.txt").exists());
}

#[tuff_core::watchdog]
#[test]
fn backup_never_overwrites_an_existing_directory() {
    let dir = TestDir::new();
    let root = &dir.path().join("world");
    old_world(root);
    let original = tree(root);
    fs::create_dir_all(dir.path().join("world OUTDATED")).unwrap();
    fs::write(dir.path().join("world OUTDATED/keep.txt"), b"older").unwrap();
    let staging = dir.path().join(".world OUTDATED.partial");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("level.dat"), b"torn").unwrap();
    assert!(layout::upgrade_world(root).unwrap());
    assert_eq!(
        sibling_names(dir.path()),
        ["world", "world OUTDATED", "world OUTDATED (1)"]
    );
    assert_eq!(
        fs::read(dir.path().join("world OUTDATED/keep.txt")).unwrap(),
        b"older"
    );
    assert_eq!(tree(&dir.path().join("world OUTDATED (1)")), original);
}

#[tuff_core::watchdog]
#[test]
fn split_worlds_are_not_backed_up() {
    let dir = TestDir::new();
    let root = &dir.path().join("world");
    fs::create_dir_all(root).unwrap();
    write_nbt_file(
        &root.join("level.dat"),
        Compound::new().with(
            "Data",
            Compound::new().with("DataVersion", level::SPLIT_VERSION),
        ),
    )
    .unwrap();
    assert!(layout::upgrade_world(root).unwrap());
    assert_eq!(sibling_names(dir.path()), ["world"]);
}
