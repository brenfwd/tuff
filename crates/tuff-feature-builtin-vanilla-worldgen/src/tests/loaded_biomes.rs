//! Pack-added biomes and pack parameter lists reach biome sources, the
//! generation steps that read biome data, `/locate` and flat worlds.

use std::sync::Arc;

use serde_json::json;
use tuff_core::storage::level::{BiomePoint, DimensionSettings, FlatSettings, OVERWORLD};
use tuff_core::world::generator::{ChunkGenerator, GeneratorData};
use tuff_item::registries::{Loader, Registries};
use tuff_vanilla_data::Block;
use tuff_vanilla_data::data_registry::DataRegistry;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{ChunkShape, biome_id};
use tuff_world_model::pos::ChunkPos;

use crate::feature::level::Region;
use crate::generator::Stages;

fn registries() -> Arc<Registries> {
    let biome = DataRegistry::get("minecraft:worldgen/biome").unwrap();
    let lists =
        DataRegistry::get("minecraft:worldgen/multi_noise_biome_source_parameter_list").unwrap();
    let mut top = vec![json!([]); 11];
    top[10] = json!(["minecraft:freeze_top_layer"]);
    let tundra = json!({
        "has_precipitation": true,
        "temperature": -0.7,
        "downfall": 0.4,
        "effects": {"water_color": 4159204},
        "carvers": ["minecraft:cave"],
        "features": top,
    });
    let mut loader = Loader::default();
    loader.insert(
        biome,
        "a:tundra".into(),
        "p",
        serde_json::to_vec(&tundra).unwrap(),
    );
    loader.insert(
        lists,
        "a:hot".into(),
        "p",
        br#"{"preset":"minecraft:nether"}"#.to_vec(),
    );
    loader.freeze().unwrap()
}

fn overworld() -> DimensionSettings {
    DimensionSettings::vanilla()
        .into_iter()
        .find(|d| d.key == OVERWORLD)
        .unwrap()
}

fn bind(generator: &dyn ChunkGenerator, registries: &Registries) {
    generator.bind(&GeneratorData {
        registries,
        compile_loaded: true,
        shape: ChunkShape::OVERWORLD,
        disabled: Default::default(),
    });
}

fn tundra_id(registries: &Registries) -> u16 {
    let id = registries
        .get("minecraft:worldgen/biome")
        .unwrap()
        .index_of("a:tundra")
        .unwrap() as u16;
    assert!(biome_id("a:tundra").is_none());
    id
}

#[tuff_core::watchdog]
#[test]
fn inline_points_place_a_pack_biome_with_its_climate_and_features() {
    let registries = registries();
    let tundra = tundra_id(&registries);
    let mut settings = overworld();
    settings.biome_preset = None;
    settings.biome_points = vec![BiomePoint {
        biome: "a:tundra".into(),
        ranges: [(-2.0, 2.0); 6],
        offset: 0.0,
    }];
    let vanilla = crate::chunk_generator::vanilla(&settings, 5).unwrap();
    bind(&vanilla, &registries);
    let generator = vanilla.generator();
    assert_eq!(generator.biome_source.possible_biomes(), vec![tundra]);
    assert_eq!(generator.biome_data(tundra).temperature, -0.7);

    let center = ChunkPos::new(0, 0);
    let mut window = Vec::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let pos = ChunkPos::new(dx, dz);
            window.push(generator.generate(pos, ChunkShape::OVERWORLD, Stages::ALL));
        }
    }
    assert!(
        window
            .iter()
            .all(|c| c.biomes().iter().all(|&b| b == tundra))
    );
    let mut region = Region::new(generator, center, window);
    generator.decorate(&mut region);
    let chunk = region
        .into_chunks()
        .into_iter()
        .find(|c| c.pos == center)
        .unwrap();
    let shape = ChunkShape::OVERWORLD;
    let frozen = (0..16)
        .flat_map(|x| (0..16).map(move |z| (x, z)))
        .any(|(x, z)| {
            (shape.min_y..shape.min_y + shape.height).any(|y| {
                let block = chunk.get(x, y, z).block();
                block == Block::Snow || block == Block::Ice
            })
        });
    assert!(frozen, "the cold pack biome froze nothing");

    let (_, name) = crate::locate::biome(
        generator,
        &["a:tundra"],
        BlockPos::new(100, 64, 100),
        ChunkShape::OVERWORLD,
    )
    .unwrap();
    assert_eq!(name, "a:tundra");
}

#[tuff_core::watchdog]
#[test]
fn a_pack_parameter_list_names_its_preset() {
    let registries = registries();
    let mut settings = overworld();
    settings.biome_preset = Some("a:hot".into());
    let vanilla = crate::chunk_generator::vanilla(&settings, 5).unwrap();
    bind(&vanilla, &registries);
    let biomes = vanilla.generator().biome_source.possible_biomes();
    assert!(biomes.contains(&biome_id("minecraft:nether_wastes").unwrap()));
    assert!(!biomes.contains(&biome_id("minecraft:plains").unwrap()));
}

#[tuff_core::watchdog]
#[test]
fn a_flat_world_can_use_a_pack_biome() {
    let registries = registries();
    let tundra = tundra_id(&registries);
    let settings = FlatSettings {
        biome: "a:tundra".into(),
        features: false,
        lakes: false,
        structure_overrides: Some(Vec::new()),
        ..FlatSettings::default()
    };
    let flat = crate::flat::Flat::for_world(&settings, 1, false).unwrap();
    bind(&flat, &registries);
    let chunk = flat.generate(ChunkPos::new(0, 0), ChunkShape::OVERWORLD);
    assert_eq!(chunk.biome(0, 64, 0), tundra);
}

#[tuff_core::watchdog]
#[test]
fn surface_rules_read_a_pack_biome_climate() {
    let registries = registries();
    let tundra = tundra_id(&registries);
    let plains = biome_id("minecraft:plains").unwrap();
    let vanilla = crate::chunk_generator::vanilla(&overworld(), 5).unwrap();
    bind(&vanilla, &registries);
    let surface = &vanilla.generator().surface;
    assert!(surface.temperature(tundra, 0, 64, 0) < 0.15);
    assert!(surface.temperature(plains, 0, 64, 0) > 0.15);
}
