use super::*;

fn state(name: &str) -> Tag {
    Tag::Compound(Compound::new().with("Name", name))
}

fn level_chunk(version: i32) -> Compound {
    let stone_and_wall = Compound::new()
        .with(
            "Palette",
            vec![
                state("minecraft:air"),
                state("minecraft:grass_path"),
                Tag::Compound(Compound::new().with("Name", "minecraft:brick_wall").with(
                    "Properties",
                    Compound::new().with("east", "true").with("west", "false"),
                )),
                Tag::Compound(
                    Compound::new()
                        .with("Name", "minecraft:cauldron")
                        .with("Properties", Compound::new().with("level", "2")),
                ),
            ],
        )
        .with("Y", 3i8)
        .with("BlockStates", vec![0i64; 256]);
    let mut biomes = vec![1i32; 1024];
    biomes[0] = 2;
    let level = Compound::new()
        .with("xPos", 1)
        .with("zPos", -1)
        .with("Status", "minecraft:full")
        .with("Sections", vec![stone_and_wall])
        .with("Biomes", biomes)
        .with(
            "TileEntities",
            vec![Compound::new().with("id", "minecraft:chest")],
        )
        .with(
            "TileTicks",
            vec![
                Compound::new()
                    .with("i", "minecraft:grass_path")
                    .with("x", 16)
                    .with("y", 5)
                    .with("z", -16)
                    .with("t", 3)
                    .with("p", 0),
                Compound::new()
                    .with("i", "minecraft:stone")
                    .with("x", 31)
                    .with("y", 6)
                    .with("z", -17)
                    .with("t", 1)
                    .with("p", 0),
            ],
        )
        .with(
            "Heightmaps",
            Compound::new().with("MOTION_BLOCKING", vec![5i64 | (2 << 9)]),
        )
        .with(
            "Structures",
            Compound::new()
                .with("Starts", Compound::new())
                .with("References", Compound::new()),
        );
    Compound::new()
        .with("DataVersion", version)
        .with("Level", level)
}

#[tuff_core::watchdog]
#[test]
fn level_chunk_moves_to_the_1_18_layout() {
    let mut root = level_chunk(2400);
    chunk_in(&mut root, -4, true).unwrap();
    assert!(!root.contains_key("Level"));
    assert_eq!(root.get_int("xPos"), Some(1));
    assert!(root.get_list("block_entities").is_some());
    assert!(root.get_list("block_ticks").is_some());
    assert!(
        root.get_compound("structures")
            .unwrap()
            .contains_key("starts")
    );
    let sections = root.get_list("sections").unwrap();
    assert_eq!(sections.len(), 24);
    let section = sections
        .iter()
        .filter_map(Tag::as_compound)
        .find(|s| s.get_byte("Y") == Some(3))
        .unwrap();
    let palette = section
        .get_compound("block_states")
        .unwrap()
        .get_list("palette")
        .unwrap();
    assert_eq!(
        palette[1].as_compound().unwrap().get_str("id"),
        Some("minecraft:dirt_path")
    );
    let wall = palette[2]
        .as_compound()
        .unwrap()
        .get_compound("properties")
        .unwrap();
    assert_eq!(wall.get_str("east"), Some("low"));
    assert_eq!(wall.get_str("west"), Some("none"));
    assert_eq!(
        palette[3].as_compound().unwrap().get_str("id"),
        Some("minecraft:water_cauldron")
    );
    let biomes = section.get_compound("biomes").unwrap();
    assert_eq!(biomes.get_list("palette").unwrap().len(), 1);
    assert!(!root.contains_key("Biomes"));
}

#[tuff_core::watchdog]
#[test]
fn level_chunk_ticks_and_heightmaps_are_upgraded() {
    let mut root = level_chunk(2400);
    chunk_in(&mut root, -4, true).unwrap();
    let ticks = root.get_list("block_ticks").unwrap();
    assert_eq!(
        ticks[0].as_compound().unwrap().get_str("i"),
        Some("minecraft:dirt_path")
    );
    let upgrade = root.get_compound("UpgradeData").unwrap();
    assert_eq!(upgrade.get_list("neighbor_block_ticks").unwrap().len(), 1);
    let motion = root
        .get_compound("Heightmaps")
        .unwrap()
        .get_long_array("MOTION_BLOCKING")
        .unwrap();
    assert_eq!(motion[0], (5 + 64) | ((2 + 64) << 9));
    assert!(root.get_compound("blending_data").is_some());
    assert_eq!(root.get_int("DataVersion"), Some(DATA_VERSION));
}

#[tuff_core::watchdog]
#[test]
fn non_overworld_level_chunk_keeps_sixteen_sections() {
    let mut root = level_chunk(2730);
    chunk_in(&mut root, 0, false).unwrap();
    assert_eq!(root.get_list("sections").unwrap().len(), 16);
    assert!(!root.contains_key("blending_data"));
}

#[tuff_core::watchdog]
#[test]
fn tight_packed_block_states_are_aligned() {
    let mut level = Compound::new();
    let mut palette = vec![state("minecraft:air")];
    for i in 0..9 {
        palette.push(state(&format!("minecraft:stone_{i}")));
    }
    let mut packed = vec![0i64; (4096 * 4usize).div_ceil(64)];
    for index in 0..4096usize {
        let value = (index % 10) as u64;
        let start = index * 4;
        packed[start >> 6] |= (value << (start & 63)) as i64;
    }
    let section = Compound::new()
        .with("Y", 0i8)
        .with("Palette", palette.clone())
        .with("BlockStates", packed);
    level.insert("Sections", vec![section]);
    let mut root = Compound::new()
        .with("DataVersion", 2500)
        .with("Level", level);
    chunk_in(&mut root, 0, false).unwrap();
    let section = root.get_list("sections").unwrap()[0].as_compound().unwrap();
    let data = section
        .get_compound("block_states")
        .unwrap()
        .get_long_array("data")
        .unwrap();
    assert_eq!(data.len(), 4096 / 16);
    assert_eq!((data[0] >> 4) & 15, 1);
    assert_eq!((data[0] >> 36) & 15, 9);
}

#[tuff_core::watchdog]
#[test]
fn proto_chunk_pending_ticks_become_tick_lists() {
    let mut level = level_chunk(2730)
        .remove("Level")
        .and_then(|t| match t {
            Tag::Compound(c) => Some(c),
            _ => None,
        })
        .unwrap();
    level.remove("TileTicks");
    let mut pending = vec![Tag::List(Vec::new()); 24];
    pending[7] = Tag::List(vec![Tag::Short(0x123)]);
    level.insert("ToBeTicked", pending);
    let mut root = Compound::new()
        .with("DataVersion", 2730)
        .with("Level", level);
    chunk_in(&mut root, -4, true).unwrap();
    let tick = root.get_list("block_ticks").unwrap()[0]
        .as_compound()
        .unwrap();
    assert_eq!(tick.get_int("x"), Some(16 + 3));
    assert_eq!(tick.get_int("y"), Some(3 * 16 + 2));
    assert_eq!(tick.get_int("z"), Some(-16 + 1));
}

#[tuff_core::watchdog]
#[test]
fn structure_ids_become_configured_from_chunk_biomes() {
    let section = |biome: &str| {
        Compound::new().with(
            "biomes",
            Compound::new().with("palette", vec![Tag::String(biome.into())]),
        )
    };
    let mut root = Compound::new()
        .with("DataVersion", 2865)
        .with(
            "sections",
            vec![section("minecraft:desert"), section("minecraft:desert")],
        )
        .with(
            "structures",
            Compound::new()
                .with(
                    "starts",
                    Compound::new()
                        .with("Village", Compound::new().with("id", "Village"))
                        .with("Mineshaft", Compound::new().with("id", "INVALID"))
                        .with("Unknown", Compound::new().with("id", "x")),
                )
                .with(
                    "References",
                    Compound::new()
                        .with("Village", vec![1i64, 2])
                        .with("Fortress", vec![3i64])
                        .with("Igloo", Vec::<i64>::new()),
                ),
        );
    chunk_in(&mut root, 0, false).unwrap();
    let structures = root.get_compound("structures").unwrap();
    let starts = structures.get_compound("starts").unwrap();
    assert_eq!(starts.len(), 1);
    let village = starts.get_compound("minecraft:village_desert").unwrap();
    assert_eq!(village.get_str("id"), Some("minecraft:village_desert"));
    let references = structures.get_compound("References").unwrap();
    assert_eq!(
        references.get_long_array("minecraft:village_desert"),
        Some(&[1, 2][..])
    );
    assert_eq!(
        references.get_long_array("minecraft:fortress"),
        Some(&[3][..])
    );
    assert_eq!(references.len(), 2);
}

#[tuff_core::watchdog]
#[test]
fn codec_decode_root_extends_old_overworld_chunks() {
    use tuff_core::storage::codec::ChunkCodec;
    use tuff_core::storage::codec::NbtChunkCodec;
    use tuff_world_model::chunk::ChunkShape;
    let codec = NbtChunkCodec::default();
    let blob = codec.encode_root(level_chunk(2400)).unwrap();
    let sections = |shape| {
        let root = codec.decode_root(&blob, shape).unwrap();
        root.get_list("sections").unwrap().len()
    };
    let overworld = sections(ChunkShape::OVERWORLD);
    let nether = sections(ChunkShape::new(0, 256));
    assert_eq!(overworld, nether + 8);
}

fn bedrock_chunk(status: &str, layer: &[&str]) -> Compound {
    let mut palette = vec![state("minecraft:air")];
    let mut indices = vec![0u64; 4096];
    for (i, name) in layer.iter().enumerate() {
        let slot = match palette
            .iter()
            .position(|s| s.as_compound().unwrap().get_str("Name") == Some(name))
        {
            Some(slot) => slot,
            None => {
                palette.push(state(name));
                palette.len() - 1
            }
        };
        indices[i] = slot as u64;
    }
    let mut packed = vec![0i64; 256];
    for (i, v) in indices.iter().enumerate() {
        packed[i / 16] |= (*v as i64) << ((i % 16) * 4);
    }
    let section = Compound::new()
        .with("Y", 0i8)
        .with("Palette", palette)
        .with("BlockStates", packed);
    Compound::new().with("DataVersion", 2730).with(
        "Level",
        Compound::new()
            .with("Status", status)
            .with("Sections", vec![section])
            .with("Biomes", vec![1i32; 1024]),
    )
}

#[tuff_core::watchdog]
#[test]
fn status_is_predicted_from_blocks_before_surface() {
    let mut root = bedrock_chunk("biomes", &["minecraft:stone"]);
    crate::datafix::legacy::upgrade(&mut root, Fixer::new(2730), -4);
    assert_eq!(root.get_str("Status"), Some("empty"));
    assert_eq!(
        root.get_compound("below_zero_retrogen")
            .unwrap()
            .get_str("target_status"),
        Some("noise")
    );

    let mut root = bedrock_chunk("biomes", &["minecraft:oak_log"]);
    crate::datafix::legacy::upgrade(&mut root, Fixer::new(2730), -4);
    assert_eq!(root.get_str("Status"), Some("liquid_carvers"));

    let mut root = bedrock_chunk("biomes", &[]);
    crate::datafix::legacy::upgrade(&mut root, Fixer::new(2730), -4);
    assert_eq!(root.get_str("Status"), Some("structure_references"));
}

#[tuff_core::watchdog]
#[test]
fn partly_bedrock_chunks_retrogen_below_zero() {
    let mut layer = vec!["minecraft:bedrock"; 255];
    layer.push("minecraft:air");
    let mut root = bedrock_chunk("full", &layer);
    crate::datafix::legacy::upgrade(&mut root, Fixer::new(2730), -4);
    assert_eq!(root.get_str("Status"), Some("empty"));
    let retrogen = root.get_compound("below_zero_retrogen").unwrap();
    assert_eq!(retrogen.get_str("target_status"), Some("heightmaps"));
    assert_eq!(
        retrogen.get_long_array("missing_bedrock"),
        Some(&[0i64, 0, 0, i64::MIN][..])
    );
    assert_eq!(
        root.get_compound("blending_data")
            .unwrap()
            .get_bool("old_noise"),
        Some(true)
    );
}

#[tuff_core::watchdog]
#[test]
fn all_air_floor_does_not_retrogen() {
    let mut root = bedrock_chunk("full", &[]);
    crate::datafix::legacy::upgrade(&mut root, Fixer::new(2730), -4);
    assert!(!root.contains_key("below_zero_retrogen"));
    assert_eq!(root.get_str("Status"), Some("full"));
}

#[tuff_core::watchdog]
#[test]
fn caves_and_cliffs_biome_names_are_renamed_in_chunk_palettes() {
    let section = |biome: &str| {
        Compound::new().with(
            "biomes",
            Compound::new().with("palette", vec![Tag::String(biome.into())]),
        )
    };
    let mut root = Compound::new().with("DataVersion", 2837).with(
        "sections",
        vec![
            section("minecraft:snowy_tundra"),
            section("minecraft:plains"),
        ],
    );
    chunk_in(&mut root, 0, false).unwrap();
    let names: Vec<String> = root
        .get_list("sections")
        .unwrap()
        .iter()
        .map(|s| {
            let palette = s.as_compound().unwrap().get_compound("biomes").unwrap();
            palette.get_list("palette").unwrap()[0]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(names, ["minecraft:snowy_plains", "minecraft:plains"]);
}

#[tuff_core::watchdog]
#[test]
fn deep_warm_ocean_becomes_warm_ocean_in_chunk_palettes() {
    let section = |biome: &str| {
        Compound::new().with(
            "biomes",
            Compound::new().with("palette", vec![Tag::String(biome.into())]),
        )
    };
    let mut root = Compound::new()
        .with("DataVersion", 2842)
        .with("sections", vec![section("minecraft:deep_warm_ocean")]);
    chunk_in(&mut root, 0, false).unwrap();
    let section = root.get_list("sections").unwrap()[0].as_compound().unwrap();
    let palette = section.get_compound("biomes").unwrap();
    assert_eq!(
        palette.get_list("palette").unwrap()[0].as_str(),
        Some("minecraft:warm_ocean")
    );
}

#[tuff_core::watchdog]
#[test]
fn feature_pool_elements_get_placed_feature_ids() {
    let mut root = level_chunk(2700);
    let tree = Compound::new().with("type", "minecraft:tree").with(
        "config",
        Compound::new()
            .with(
                "foliage_placer",
                Compound::new().with("type", "minecraft:pine_foliage_placer"),
            )
            .with("leaves_provider", Compound::new()),
    );
    let piece = |feature: Compound| {
        Compound::new().with("id", "minecraft:jigsaw").with(
            "pool_element",
            Compound::new()
                .with("element_type", "minecraft:feature_pool_element")
                .with("feature", feature),
        )
    };
    let patch = Compound::new().with("type", "minecraft:random_patch").with(
        "config",
        Compound::new().with(
            "state_provider",
            Compound::new()
                .with("type", "minecraft:simple_state_provider")
                .with("state", Compound::new().with("Name", "minecraft:cactus")),
        ),
    );
    let unknown = Compound::new().with("type", "minecraft:ore");
    root.get_compound_mut("Level")
        .unwrap()
        .get_compound_mut("Structures")
        .unwrap()
        .insert(
            "Starts",
            Compound::new().with(
                "Village",
                Compound::new().with("Children", vec![piece(tree), piece(patch), piece(unknown)]),
            ),
        );
    chunk_in(&mut root, -4, true).unwrap();
    let children = root
        .get_compound("structures")
        .unwrap()
        .get_compound("starts")
        .unwrap();
    let children = children
        .values()
        .next()
        .unwrap()
        .as_compound()
        .unwrap()
        .get_list("Children")
        .unwrap();
    let features: Vec<Option<&str>> = children
        .iter()
        .map(|c| {
            c.as_compound()
                .unwrap()
                .get_compound("pool_element")
                .unwrap()
                .get("feature")
                .and_then(Tag::as_str)
        })
        .collect();
    assert_eq!(
        features,
        [Some("minecraft:pine"), Some("minecraft:patch_cactus"), None]
    );
}

#[tuff_core::watchdog]
#[test]
fn old_village_starts_are_dropped_and_new_villages_renamed() {
    let mut root = level_chunk(1900);
    let structures = root
        .get_compound_mut("Level")
        .unwrap()
        .get_compound_mut("Structures")
        .unwrap();
    structures.insert(
        "Starts",
        Compound::new()
            .with("Village", Compound::new().with("id", "Village"))
            .with(
                "New_Village",
                Compound::new().with("id", "minecraft:new_village"),
            ),
    );
    structures.insert(
        "References",
        Compound::new()
            .with("Village", vec![1i64])
            .with("New_Village", vec![2i64]),
    );
    chunk_in(&mut root, -4, true).unwrap();
    let structures = root.get_compound("structures").unwrap();
    let starts = structures.get_compound("starts").unwrap();
    assert_eq!(starts.len(), 1);
    assert!(starts.get_compound("minecraft:village_desert").is_some());
    assert!(
        structures
            .get_compound("References")
            .is_none_or(|refs| refs.is_empty())
    );
}
