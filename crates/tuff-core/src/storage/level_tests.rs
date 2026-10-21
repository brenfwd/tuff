use super::*;
use crate::world::clock::{ClockState, clock_id};
use std::path::Path;
use tuff_storage::files::read_nbt_file;
use tuff_storage::memory::MemoryBackend;
use tuff_storage::test_dir::TestDir;
use tuff_storage::vanilla::VanillaBackend;

#[tuff_core::watchdog]
#[test]
fn inline_multi_noise_biomes_are_read_and_validated() {
    let range = |a: f32, b: f32| Tag::List(vec![Tag::Float(a), Tag::Float(b)]);
    let entry = |temperature: Tag, offset: f32| {
        let parameters = Compound::new()
            .with("temperature", temperature)
            .with("humidity", Tag::Float(0.5))
            .with(
                "continentalness",
                Tag::Compound(
                    Compound::new()
                        .with("min", Tag::Float(-1.0))
                        .with("max", Tag::Float(1.0)),
                ),
            )
            .with("erosion", range(-1.0, 1.0))
            .with("depth", Tag::Float(0.0))
            .with("weirdness", range(-0.5, 0.5))
            .with("offset", Tag::Float(offset));
        Tag::Compound(
            Compound::new()
                .with("biome", Tag::String("minecraft:plains".into()))
                .with("parameters", Tag::Compound(parameters)),
        )
    };
    let source = |entries: Vec<Tag>| Compound::new().with("biomes", Tag::List(entries));
    let points = biome_points(&source(vec![entry(range(-0.5, 0.5), 0.25)]));
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].biome, "minecraft:plains");
    assert_eq!(points[0].ranges[0], (-0.5, 0.5));
    assert_eq!(points[0].ranges[1], (0.5, 0.5));
    assert_eq!(points[0].ranges[2], (-1.0, 1.0));
    assert_eq!(points[0].offset, 0.25);
    assert!(biome_points(&source(vec![entry(range(0.5, -0.5), 0.0)])).is_empty());
    assert!(biome_points(&source(vec![entry(range(-3.0, 0.0), 0.0)])).is_empty());
    assert!(biome_points(&source(vec![entry(Tag::Float(0.0), 1.5)])).is_empty());
}

#[tuff_core::watchdog]
#[test]
fn flat_settings_bound_layer_heights_and_fall_back_to_plains() {
    let parse = |json: &str| FlatSettings::from_json(&serde_json::from_str(json).unwrap());
    let layers = |h: &str| format!(r#"{{"layers":[{{"block":"stone","height":{h}}}]}}"#);
    assert!(parse(&layers("384")).is_some());
    assert!(parse(&layers("385")).is_none());
    assert!(
        parse(r#"{"layers":[{"block":"stone","height":300},{"block":"dirt","height":85}]}"#)
            .is_none()
    );
    let unknown = parse(r#"{"biome":"minecraft:nope"}"#).unwrap();
    assert_eq!(unknown.biome, "minecraft:plains");
}

#[tuff_core::watchdog]
#[test]
fn level_type_switches_a_new_overworld_preset() {
    let mut level =
        LevelData::load(&MemoryBackend::default(), || 1, |_| BlockPos::default()).unwrap();
    assert!(level.set_new_world_type("amplified", "{}"));
    let overworld = level
        .dimensions
        .iter()
        .find(|d| d.key == OVERWORLD)
        .unwrap();
    assert_eq!(overworld.settings.as_deref(), Some("minecraft:amplified"));
    assert!(!level.set_new_world_type("bogus", "{}"));
    assert!(level.set_new_world_type(
        "flat",
        r#"{"biome":"minecraft:desert","layers":[{"block":"minecraft:stone","height":3}]}"#
    ));
    let overworld = level.dimension(OVERWORLD).unwrap();
    assert_eq!(overworld.generator, "minecraft:flat");
    let flat = overworld.flat.clone().unwrap();
    assert_eq!(flat.biome, "minecraft:desert");
    assert_eq!(flat.layers, vec![("minecraft:stone".to_owned(), 3)]);
    assert!(level.set_new_world_type("single_biome_surface", r#"{"biome":"minecraft:desert"}"#));
    let overworld = level.dimension(OVERWORLD).unwrap();
    assert_eq!(overworld.biome_source.as_deref(), Some("minecraft:fixed"));
    assert_eq!(overworld.biome.as_deref(), Some("minecraft:desert"));
    assert!(level.set_new_world_type("debug_all_block_states", "{}"));
    assert_eq!(
        level.dimension(OVERWORLD).unwrap().generator,
        "minecraft:debug"
    );
    assert!(level.set_new_world_type("normal", "{}"));
}

#[tuff_core::watchdog]
#[test]
fn flat_all_dimensions_flattens_every_dimension() {
    let mut level =
        LevelData::load(&MemoryBackend::default(), || 1, |_| BlockPos::default()).unwrap();
    assert!(level.set_new_world_type("flat_all_dimensions", "{}"));
    let layers = |key: &str| {
        let d = level.dimension(key).unwrap();
        assert_eq!(d.generator, "minecraft:flat");
        d.flat.clone().unwrap()
    };
    let overworld = layers(OVERWORLD);
    assert_eq!(overworld.biome, "minecraft:desert");
    assert_eq!(
        overworld.layers,
        vec![
            ("minecraft:bedrock".to_owned(), 1),
            ("minecraft:sandstone".to_owned(), 67)
        ]
    );
    assert_eq!(layers(THE_NETHER).biome, "minecraft:basalt_deltas");
    assert_eq!(
        layers(THE_END).layers[1],
        ("minecraft:end_stone".to_owned(), 3)
    );
}

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/vanilla-26.3"
);

fn vanilla_world() -> (TestDir, VanillaBackend) {
    let dir = TestDir::new();
    let backend = VanillaBackend::new(dir.path().join("world"));
    let paths = backend.paths();
    let copy = |name: &str, to: std::path::PathBuf| {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(Path::new(FIXTURES).join(name), to).unwrap();
    };
    copy("level.dat", paths.level_dat());
    copy(
        "world_gen_settings.dat",
        paths.saved_data(WORLD_GEN_SETTINGS),
    );
    copy("world_clocks.dat", paths.saved_data(WORLD_CLOCKS));
    copy(
        "world_border.dat",
        paths.dimension_data(OVERWORLD, WORLD_BORDER),
    );
    (dir, backend)
}

#[tuff_core::watchdog]
#[test]
fn loads_vanilla_world_files() {
    let (_dir, backend) = vanilla_world();
    let level = LevelData::load(
        &backend,
        || panic!("existing seed"),
        |_| panic!("existing spawn"),
    )
    .unwrap();
    assert_eq!(level.seed, 1);
    assert_eq!(level.game_time, 1199);
    for name in [OVERWORLD, THE_END] {
        let clock = level.clocks.get(clock_id(name).unwrap());
        assert_eq!(
            *clock,
            ClockState {
                total_ticks: 1199,
                ..ClockState::default()
            }
        );
    }
    assert_eq!(level.spawn.pos, BlockPos::new(160, 70, 160));
    assert_eq!(level.difficulty, Difficulty::Easy);
    assert_eq!(level.default_game_mode, GameMode::Survival);
    let keys: Vec<&str> = level.dimensions.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(keys, [OVERWORLD, THE_END, THE_NETHER]);
    let overworld = level.dimension(OVERWORLD).unwrap();
    assert_eq!(overworld.border.size, DEFAULT_BORDER_SIZE);
    assert_eq!(overworld.kind, OVERWORLD);
    assert_eq!(overworld.generator, "minecraft:noise");
    assert_eq!(overworld.settings.as_deref(), Some(OVERWORLD));
    let nether = level.dimension(THE_NETHER).unwrap();
    assert_eq!(nether.settings.as_deref(), Some("minecraft:nether"));
    assert_eq!(nether.border, WorldBorder::default());
}

#[tuff_core::watchdog]
#[test]
fn save_preserves_vanilla_fields() {
    let (_dir, backend) = vanilla_world();
    let paths = backend.paths();
    let mut level = LevelData::load(&backend, || 0, |_| BlockPos::default()).unwrap();
    level.game_time = 5000;
    let overworld = clock_id(OVERWORLD).unwrap();
    level.clocks.get_mut(overworld).set(7000);
    level.clocks.get_mut(overworld).paused = true;
    level.save(&backend, "ignored", "tuff").unwrap();
    let root = read_nbt_file(&paths.level_dat()).unwrap().unwrap();
    let data = root.get_compound("Data").unwrap();
    assert_eq!(data.get_str("LevelName"), Some("world"));
    assert_eq!(data.get_long("Time"), Some(5000));
    let disabled = data
        .get_compound("DataPacks")
        .unwrap()
        .get_list("Disabled")
        .unwrap();
    assert_eq!(disabled.len(), 3);
    assert_eq!(data.get_list("ServerBrands").unwrap().len(), 2);
    assert_eq!(data.get_list("version_history").unwrap().len(), 1);
    assert!(paths.level_dat().with_file_name("level.dat_old").exists());
    let reloaded = LevelData::load(&backend, || 0, |_| BlockPos::default()).unwrap();
    assert_eq!((reloaded.game_time, reloaded.seed), (5000, 1));
    assert_eq!(reloaded.clocks, level.clocks);
    assert_eq!(reloaded.clocks.get(overworld).total_ticks, 7000);
    let clocks = inner(read_nbt_file(&paths.saved_data(WORLD_CLOCKS)).unwrap());
    assert_eq!(
        clocks
            .get_compound(THE_END)
            .unwrap()
            .get_long("total_ticks"),
        Some(1199)
    );
}

#[tuff_core::watchdog]
#[test]
fn creates_new_world_files() {
    let dir = TestDir::new();
    let backend = VanillaBackend::new(dir.path().join("world"));
    let paths = backend.paths();
    let level = LevelData::load(&backend, || 42, |seed| BlockPos::new(seed as i32, 64, 0)).unwrap();
    assert_eq!(level.spawn.pos, BlockPos::new(42, 64, 0));
    level.save(&backend, "world", "tuff").unwrap();
    let reloaded = LevelData::load(&backend, || panic!(), |_| panic!()).unwrap();
    assert_eq!(reloaded.seed, 42);
    assert_eq!(reloaded.spawn, level.spawn);
    let gen_settings = inner(read_nbt_file(&paths.saved_data(WORLD_GEN_SETTINGS)).unwrap());
    assert!(
        gen_settings
            .get_compound("dimensions")
            .unwrap()
            .contains_key(OVERWORLD)
    );
    for dimension in [OVERWORLD, THE_NETHER, THE_END] {
        let border = inner(read_nbt_file(&paths.dimension_data(dimension, WORLD_BORDER)).unwrap());
        assert_eq!(border.get_double("size"), Some(DEFAULT_BORDER_SIZE));
    }
    assert_eq!(reloaded.dimensions.len(), 3);
}

#[tuff_core::watchdog]
#[test]
fn extra_dimensions_and_borders_are_read_from_the_data() {
    let backend = MemoryBackend::default();
    let mut dimensions = default_dimensions();
    dimensions.insert(
        "pack:sky",
        Compound::new()
            .with("type", "pack:sky_type")
            .with("generator", Compound::new().with("type", "pack:islands")),
    );
    backend
        .write(
            Document::SavedData(WORLD_GEN_SETTINGS),
            saved(
                Compound::new()
                    .with("seed", 7i64)
                    .with("dimensions", dimensions),
            ),
        )
        .unwrap();
    backend
        .write(
            Document::DimensionData {
                dimension: THE_NETHER,
                id: WORLD_BORDER,
            },
            saved(Compound::new().with("size", 200.0).with("center_x", 8.0)),
        )
        .unwrap();
    let level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    let keys: Vec<&str> = level.dimensions.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(keys, [OVERWORLD, THE_END, THE_NETHER, "pack:sky"]);
    let sky = level.dimension("pack:sky").unwrap();
    assert_eq!(
        (sky.kind.as_str(), sky.generator.as_str()),
        ("pack:sky_type", "pack:islands")
    );
    assert_eq!(sky.settings, None);
    let nether = level.dimension(THE_NETHER).unwrap();
    assert_eq!(
        (nether.border.size, nether.border.center),
        (200.0, [8.0, 0.0])
    );
    level.save(&backend, "mem", "tuff").unwrap();
    let reloaded = LevelData::load(&backend, || panic!(), |_| panic!()).unwrap();
    let summary = |dimensions: &[DimensionSettings]| -> Vec<_> {
        dimensions
            .iter()
            .map(|d| {
                (
                    d.key.clone(),
                    d.kind.clone(),
                    d.generator.clone(),
                    d.settings.clone(),
                    d.border,
                )
            })
            .collect()
    };
    assert_eq!(summary(&reloaded.dimensions), summary(&level.dimensions));
    assert!(backend.exists(Document::DimensionData {
        dimension: "pack:sky",
        id: WORLD_BORDER
    }));
}

#[tuff_core::watchdog]
#[test]
fn feature_flags_are_saved_and_a_new_world_has_none_yet() {
    let backend = MemoryBackend::default();
    let mut level = LevelData::load(&backend, || 3, |_| BlockPos::default()).unwrap();
    assert_eq!(level.enabled_features(), None, "a new world");
    let flags = [
        "minecraft:trade_rebalance".to_owned(),
        "minecraft:vanilla".to_owned(),
    ];
    level.set_enabled_features(&flags);
    level.save(&backend, "world", "tuff").unwrap();
    let reloaded = LevelData::load(&backend, || 3, |_| BlockPos::default()).unwrap();
    assert_eq!(reloaded.enabled_features(), Some(flags.to_vec()));
}

#[tuff_core::watchdog]
#[test]
fn bonus_chest_is_pending_only_for_a_world_never_started() {
    let backend = MemoryBackend::default();
    backend
        .write(
            Document::SavedData(WORLD_GEN_SETTINGS),
            saved(
                Compound::new()
                    .with("seed", 7i64)
                    .with("bonus_chest", true)
                    .with("dimensions", default_dimensions()),
            ),
        )
        .unwrap();
    let level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    assert!(!level.bonus_chest_pending, "initialized is assumed");
    backend
        .write(
            Document::Level,
            Compound::new().with("Data", Compound::new().with("initialized", false)),
        )
        .unwrap();
    let level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    assert!(level.bonus_chest_pending);
}

#[tuff_core::watchdog]
#[test]
fn generate_structures_is_read_from_the_world_gen_settings() {
    let backend = MemoryBackend::default();
    let level = LevelData::load(&backend, || 3, |_| BlockPos::default()).unwrap();
    assert!(level.dimensions.iter().all(|d| d.generate_structures));
    backend
        .write(
            Document::SavedData(WORLD_GEN_SETTINGS),
            saved(
                Compound::new()
                    .with("seed", 7i64)
                    .with("generate_structures", false)
                    .with("dimensions", default_dimensions()),
            ),
        )
        .unwrap();
    let level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    assert_eq!(level.dimensions.len(), 3);
    assert!(level.dimensions.iter().all(|d| !d.generate_structures));
}

#[tuff_core::watchdog]
#[test]
fn the_property_only_shapes_a_new_world() {
    let backend = MemoryBackend::default();
    let mut level = LevelData::load(&backend, || 3, |_| BlockPos::default()).unwrap();
    level.set_new_world_structures(false);
    assert!(level.dimensions.iter().all(|d| !d.generate_structures));
    level.save(&backend, "w", "b").unwrap();
    let mut level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    assert!(level.dimensions.iter().all(|d| !d.generate_structures));
    level.set_new_world_structures(true);
    assert!(level.dimensions.iter().all(|d| !d.generate_structures));
}

#[tuff_core::watchdog]
#[test]
fn gameplay_properties_only_shape_a_new_world() {
    let backend = MemoryBackend::default();
    let mut level = LevelData::load(&backend, || 3, |_| BlockPos::default()).unwrap();
    level.set_new_world_gameplay(Difficulty::Peaceful, GameMode::Creative, false, false);
    assert_eq!(level.difficulty, Difficulty::Peaceful);
    assert_eq!(level.default_game_mode, GameMode::Creative);
    assert!(!level.game_rules.bool("pvp"));
    level.save(&backend, "w", "b").unwrap();
    let mut level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    level.set_new_world_gameplay(Difficulty::Hard, GameMode::Survival, true, true);
    assert_eq!(level.difficulty, Difficulty::Peaceful);
    assert_eq!(level.default_game_mode, GameMode::Creative);
}

#[tuff_core::watchdog]
#[test]
fn existing_world_takes_changed_difficulty_and_gamemode_once() {
    let backend = MemoryBackend::default();
    let mut level = LevelData::load(&backend, || 3, |_| BlockPos::default()).unwrap();
    level.set_new_world_gameplay(Difficulty::Easy, GameMode::Survival, false, true);
    level.save(&backend, "w", "b").unwrap();
    let mut level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    level.sync_existing_world_defaults(Difficulty::Hard, GameMode::Creative);
    assert_eq!(level.difficulty, Difficulty::Hard);
    assert_eq!(level.default_game_mode, GameMode::Creative);
    level.difficulty = Difficulty::Peaceful;
    level.save(&backend, "w", "b").unwrap();
    let mut level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    assert_eq!(level.default_game_mode, GameMode::Creative);
    level.sync_existing_world_defaults(Difficulty::Hard, GameMode::Creative);
    assert_eq!(level.difficulty, Difficulty::Peaceful);
    level.set_hardcore(true);
    level.sync_existing_world_defaults(Difficulty::Easy, GameMode::Creative);
    assert_eq!(level.difficulty, Difficulty::Hard);
}

#[tuff_core::watchdog]
#[test]
fn existing_world_follows_changed_gameplay_properties() {
    let backend = MemoryBackend::default();
    let mut level = LevelData::load(&backend, || 3, |_| BlockPos::default()).unwrap();
    level.set_new_world_gameplay(Difficulty::Easy, GameMode::Survival, false, true);
    level.save(&backend, "w", "b").unwrap();
    let mut level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    level.sync_existing_world_gameplay(true, false);
    assert!(level.hardcore && level.difficulty_locked);
    assert_eq!(level.difficulty, Difficulty::Hard);
    assert!(!level.game_rules.bool("pvp"));
    level.save(&backend, "w", "b").unwrap();
    let mut level = LevelData::load(&backend, || panic!(), |_| BlockPos::default()).unwrap();
    assert!(level.hardcore && level.difficulty_locked);
    let _ = level
        .game_rules
        .set("pvp", crate::world::game_rules::RuleValue::Bool(true));
    level.sync_existing_world_gameplay(true, false);
    assert!(level.game_rules.bool("pvp"));
    level.sync_existing_world_gameplay(false, false);
    assert!(!level.hardcore && !level.difficulty_locked);
    assert_eq!(level.difficulty, Difficulty::Hard);
}

#[tuff_core::watchdog]
#[test]
fn hardcore_forces_hard_survival() {
    let backend = MemoryBackend::default();
    let mut level = LevelData::load(&backend, || 3, |_| BlockPos::default()).unwrap();
    level.set_new_world_gameplay(Difficulty::Easy, GameMode::Creative, true, true);
    assert_eq!(level.difficulty, Difficulty::Hard);
    assert_eq!(level.default_game_mode, GameMode::Survival);
}

#[tuff_core::watchdog]
#[test]
fn reads_default_game_mode() {
    let backend = MemoryBackend::default();
    backend
        .write(
            Document::Level,
            Compound::new().with("Data", Compound::new().with("GameType", 1)),
        )
        .unwrap();
    let level = LevelData::load(&backend, || 0, |_| BlockPos::default()).unwrap();
    assert_eq!(level.default_game_mode, GameMode::Creative);
}

#[tuff_core::watchdog]
#[test]
fn refuses_newer_worlds() {
    let backend = MemoryBackend::default();
    backend
        .write(
            Document::Level,
            Compound::new().with(
                "Data",
                Compound::new().with("DataVersion", DATA_VERSION + 1),
            ),
        )
        .unwrap();
    assert!(LevelData::load(&backend, || 0, |_| BlockPos::default()).is_err());
}

#[tuff_core::watchdog]
#[test]
fn round_trips_through_any_backend() {
    let backend = MemoryBackend::default();
    let level = LevelData::load(&backend, || 9, |_| BlockPos::new(1, 2, 3)).unwrap();
    level.save(&backend, "mem", "tuff").unwrap();
    let reloaded = LevelData::load(&backend, || panic!(), |_| panic!()).unwrap();
    assert_eq!(reloaded.seed, 9);
    assert_eq!(reloaded.spawn.pos, BlockPos::new(1, 2, 3));
    assert!(backend.exists(Document::DimensionData {
        dimension: OVERWORLD,
        id: WORLD_BORDER
    }));
}
