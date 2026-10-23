//! Vanilla compatibility export/verify: writes a world through the real
//! region actors for the vanilla server to load, and checks it after
//! vanilla has loaded and saved it (see scripts/vanilla-compat.sh).

use super::*;
use crate::storage::codec::WorldBackendExt as _;

fn compat_world_dir() -> std::path::PathBuf {
    std::env::var_os(EXPORT_ENV)
        .unwrap_or_else(|| panic!("run through scripts/vanilla-compat.sh ({EXPORT_ENV} unset)"))
        .into()
}

/// The broken block and a dropped item that `verify_world_after_vanilla`
/// expects vanilla to have kept.
fn compat_edit() -> BlockPos {
    BlockPos::new(-3, Generator::new(EXPORT_SEED).height(-3, 5), 5)
}

/// A floating lava source whose first fluid tick is still pending when the
/// export shuts down, so vanilla must load and keep it.
fn compat_lava() -> BlockPos {
    BlockPos::new(20, Generator::new(EXPORT_SEED).height(20, 20) + 6, 20)
}

/// A chunk the vanilla generator's pipeline finishes before the export
/// stops, leaving the chunks around it in generation for vanilla to go on
/// with. Inside vanilla's force-loaded area.
const COMPAT_GENERATED: ChunkPos = ChunkPos::new(8, 0);

/// Runs the vanilla generator's pipeline on the export, wants
/// [`COMPAT_GENERATED`] and saves the chunks still in generation around
/// it, as the world does when it stops.
async fn export_chunks_in_generation(dir: &std::path::Path) {
    use tuff_core::world::generator::{PipelineContext, ProtoStorage};
    use tuff_storage::backend::WorldBackend;

    let backend: Arc<dyn WorldBackend> = Arc::new(VanillaBackend::new(dir));
    let settings = DimensionSettings::vanilla()
        .into_iter()
        .find(|d| d.key == OVERWORLD)
        .unwrap();
    let generator = Arc::new(
        tuff_feature_builtin_vanilla_worldgen::chunk_generator::vanilla(&settings, EXPORT_SEED)
            .unwrap(),
    );
    let io = tuff_core::world::chunk_io::ChunkIo::start(backend.clone());
    let (_clock_tx, clock) = tokio::sync::watch::channel(0);
    let pipeline = tuff_feature_builtin_vanilla_worldgen::pipeline::GenerationActor::spawn(
        generator,
        PipelineContext {
            shape: ChunkShape::OVERWORLD,
            clock,
            storage: Some(ProtoStorage::new(io, OVERWORLD, backend.chunk_codec())),
            tags: Default::default(),
        },
    );
    let (region, _requests, mut inbox) = crate::world::messages::RegionHandle::new(
        crate::world::dimension::DimensionId::default(),
        COMPAT_GENERATED.region(),
        8,
    );
    assert!(generator::ChunkPipeline::want(
        &pipeline,
        COMPAT_GENERATED,
        region,
        generator::URGENT
    ));
    loop {
        let message = tokio::time::timeout(Duration::from_secs(120), inbox.recv())
            .await
            .expect("the chunk generates")
            .expect("pipeline alive");
        if matches!(message, RegionInternal::Generated { .. }) {
            break;
        }
    }
    let (done, flushed) = oneshot::channel();
    generator::ChunkPipeline::flush(&pipeline, done);
    flushed.await.unwrap();
    for (x, z, status) in [(9, 1, "minecraft:features"), (10, 2, "minecraft:terrain")] {
        let root = stored_chunk(dir, ChunkPos::new(x, z), false).expect("saved in generation");
        assert_eq!(root.get_str("Status"), Some(status));
    }
}

const COMPAT_TNT_OWNER: u128 = 0x1234_5678;
const COMPAT_TNT_FUSE: i16 = 200;

/// Floating primed TNT and an experience orb the export leaves for vanilla
/// to load.
fn compat_entities() -> [(Compound, [f64; 3]); 10] {
    [
        (
            Compound::new()
                .with("id", "minecraft:tnt")
                .with("fuse", COMPAT_TNT_FUSE)
                .with("NoGravity", true)
                .with("CustomName", "compat")
                .with("owner", Tag::uuid(COMPAT_TNT_OWNER)),
            [3.5, 200.0, -6.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:experience_orb")
                .with("Value", 5i16)
                .with("NoGravity", true),
            [5.5, 200.0, -6.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:sheep")
                .with("Color", 11i8)
                .with("Age", -12000)
                .with("NoAI", true)
                .with("NoGravity", true)
                .with("CustomName", "compat_sheep"),
            [7.5, 200.0, -6.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:cow")
                .with("variant", "minecraft:warm")
                .with("NoAI", true)
                .with("NoGravity", true),
            [9.5, 200.0, -6.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:skeleton")
                .with(
                    "equipment",
                    Compound::new().with(
                        "mainhand",
                        tuff_item::item::ItemPicture::new(Item::Bow, 1).to_nbt(),
                    ),
                )
                .with("NoAI", true)
                .with("NoGravity", true),
            [11.5, 200.0, -6.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:zombie")
                .with("IsBaby", true)
                .with("NoAI", true)
                .with("NoGravity", true),
            [13.5, 200.0, -6.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:end_crystal")
                .with("ShowBottom", false)
                .with("beam_target", Tag::IntArray(vec![0, 128, 0])),
            [15.5, 200.0, -6.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:slime")
                .with("Size", 2)
                .with("NoAI", true)
                .with("NoGravity", true),
            [4.5, 200.0, -11.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:hoglin")
                .with("IsImmuneToZombification", true)
                .with("NoAI", true)
                .with("NoGravity", true),
            [10.5, 200.0, -11.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:villager")
                .with(
                    "VillagerData",
                    Compound::new()
                        .with("type", "minecraft:desert")
                        .with("profession", "minecraft:librarian")
                        .with("level", 2),
                )
                .with("Xp", 15)
                .with(
                    "Offers",
                    Compound::new().with(
                        "Recipes",
                        vec![
                            Compound::new()
                                .with(
                                    "buy",
                                    Compound::new()
                                        .with("id", "minecraft:paper")
                                        .with("count", 24),
                                )
                                .with(
                                    "sell",
                                    Compound::new()
                                        .with("id", "minecraft:emerald")
                                        .with("count", 1),
                                )
                                .with("maxUses", 16)
                                .with("uses", 3)
                                .with("priceMultiplier", 0.05f32)
                                .with("xp", 2),
                        ],
                    ),
                )
                .with("NoAI", true)
                .with("NoGravity", true),
            [13.5, 200.0, -11.5],
        ),
    ]
}

/// Where the export places a patterned banner, a sign, a bell and a
/// player head, on top of the terrain.
fn compat_block_entities() -> [(Item, BlockPos); 5] {
    [
        (Item::WhiteBanner, 2),
        (Item::OakSign, 4),
        (Item::Bell, 6),
        (Item::PlayerHead, 8),
        (Item::BrewingStand, 10),
    ]
    .map(|(item, x)| {
        (
            item,
            BlockPos::new(x, Generator::new(EXPORT_SEED).height(x, 7) + 1, 7),
        )
    })
}

const COMPAT_SIGN_TEXT: &str = "Hello vanilla";

/// Writes a world through the real region actors for vanilla to load.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run by scripts/vanilla-compat.sh"]
async fn export_world_for_vanilla() {
    let dir = compat_world_dir();
    let backend = VanillaBackend::new(&dir);
    let level = tuff_core::storage::level::LevelData::load(
        &backend,
        || EXPORT_SEED,
        |seed| Generator::new(seed).spawn(),
    )
    .unwrap();
    level
        .save(&backend, "world", coordinator::SERVER_BRAND)
        .unwrap();

    let world = TestWorld::start(&dir);
    let (viewer, mut events) = PlayerHandle::new(100);
    let chunks: Vec<ChunkPos> = ChunkPos::new(0, 0).view(4);
    let mut by_region: std::collections::HashMap<RegionPos, Vec<ChunkPos>> = Default::default();
    for chunk in &chunks {
        by_region.entry(chunk.region()).or_default().push(*chunk);
    }
    for (region, chunks) in by_region {
        let handle = world.region(region).await;
        subscribe(&handle, &viewer, &mut events, &chunks).await;
    }
    let edit = compat_edit();
    let region = world.region(ChunkPos::of_block(edit).region()).await;
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: edit,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome: ActionOutcome::Broken { .. },
        } => Some(()),
        _ => None,
    })
    .await;
    let placed = compat_block_entities();
    let region = world.region(ChunkPos::of_block(placed[0].1).region()).await;
    for (sequence, (item, at)) in (2..).zip(placed) {
        let mut placing = place(&viewer, BlockPos::new(at.x, at.y - 1, at.z), item, sequence);
        placing.components = match item {
            Item::WhiteBanner => {
                tuff_item::item::Components::from(tuff_item::item::ComponentPatch {
                    banner_patterns: Some(vec![tuff_item::item::decor::BannerLayer {
                        inline: None,
                        pattern: "minecraft:stripe_top".into(),
                        color: tuff_vanilla_data::item::DyeColor::from_name("red").unwrap(),
                    }]),
                    ..Default::default()
                })
            }
            Item::PlayerHead => {
                tuff_item::item::Components::from(tuff_item::item::ComponentPatch {
                    other: Compound::new()
                        .with("minecraft:profile", Compound::new().with("name", "Steve")),
                    ..Default::default()
                })
            }
            _ => Default::default(),
        }
        .into();
        region.request(RegionRequest::Place(placing)).await;
        expect(&mut events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: done,
                outcome: ActionOutcome::Placed { .. },
            } if done == sequence => Some(()),
            _ => None,
        })
        .await;
        if item == Item::OakSign {
            region
                .request(RegionRequest::UpdateSign {
                    player: viewer.clone(),
                    pos: at,
                    front: true,
                    lines: vec![
                        COMPAT_SIGN_TEXT.into(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ],
                    eye: [at.x as f64 + 0.5, at.y as f64 + 1.6, at.z as f64 - 2.0],
                    filtered: None,
                })
                .await;
        }
    }
    for (nbt, position) in compat_entities() {
        let region = world.region(ChunkPos::of_position(position).region()).await;
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt,
                position,
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    let lava = compat_lava();
    let (reply, answer) = oneshot::channel();
    world
        .region(ChunkPos::of_block(lava).region())
        .await
        .request(RegionRequest::Blocks {
            op: crate::world::messages::BlockOp::Set {
                changes: vec![(lava, Block::Lava.default_state())],
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
    world.ticks(3).await;
    world.shutdown().await;
    export_chunks_in_generation(&dir).await;

    let below = BlockPos::new(edit.x, edit.y - 1, edit.z);
    let below_block = Generator::new(EXPORT_SEED)
        .generate(ChunkPos::of_block(below))
        .get(below.x, below.y, below.z)
        .block();
    let commands = [
        "tick freeze".to_string(),
        "forceload add -48 -48 175 47".to_string(),
        format!(
            "execute if block {} {} {} minecraft:air",
            edit.x, edit.y, edit.z
        ),
        format!(
            "execute if block {} {} {} {}",
            below.x,
            below.y,
            below.z,
            below_block.name()
        ),
        "execute if block 0 -64 0 minecraft:bedrock".to_string(),
        format!(
            "execute if block {} {} {} minecraft:lava",
            lava.x, lava.y, lava.z
        ),
        "execute if entity @e[type=minecraft:item]".to_string(),
        "execute if entity @e[type=minecraft:tnt,name=compat,nbt={NoGravity:1b}]".to_string(),
        "execute if data entity @e[type=minecraft:tnt,limit=1] owner".to_string(),
        "execute if data entity @e[type=minecraft:tnt,limit=1] block_state".to_string(),
        "execute if entity @e[type=minecraft:experience_orb,nbt={Value:5s}]".to_string(),
        "execute if entity @e[type=minecraft:sheep,name=compat_sheep,nbt={Color:11b,NoAI:1b}]"
            .to_string(),
        "execute if data entity @e[type=minecraft:sheep,limit=1] Age".to_string(),
        "execute if entity @e[type=minecraft:cow,nbt={variant:\"minecraft:warm\"}]".to_string(),
        "execute if entity @e[type=minecraft:skeleton,nbt={equipment:{mainhand:{id:\"minecraft:bow\"}}}]"
            .to_string(),
        "execute if entity @e[type=minecraft:zombie,nbt={IsBaby:1b}]".to_string(),
        "execute if entity @e[type=minecraft:end_crystal,nbt={ShowBottom:0b,beam_target:[I;0,128,0]}]"
            .to_string(),
        "execute if entity @e[type=minecraft:slime,nbt={Size:2}]".to_string(),
        "execute if entity @e[type=minecraft:hoglin,nbt={IsImmuneToZombification:1b}]".to_string(),
        "execute if entity @e[type=minecraft:villager,nbt={VillagerData:{type:\"minecraft:desert\",profession:\"minecraft:librarian\",level:2},Xp:15,Offers:{Recipes:[{maxUses:16,uses:3,buy:{id:\"minecraft:paper\",count:24}}]}}]"
            .to_string(),
    ]
    .into_iter()
    .chain(placed.map(|(item, at)| {
        let path = match item {
            Item::WhiteBanner => "patterns[0]".to_string(),
            Item::OakSign => {
                format!("{{front_text:{{messages:[\"{COMPAT_SIGN_TEXT}\"]}}}}")
            }
            Item::PlayerHead => "profile.name".to_string(),
            Item::BrewingStand => "total_fuel".to_string(),
            _ => "id".to_string(),
        };
        format!("execute if data block {} {} {} {path}", at.x, at.y, at.z)
    }))
    .collect::<Vec<_>>();
    std::fs::write(
        dir.with_file_name("vanilla-commands.txt"),
        commands.join("\n") + "\n",
    )
    .unwrap();
}

/// Checks the world after vanilla has loaded and saved it.
#[tuff_core::watchdog]
#[test]
#[ignore = "run by scripts/vanilla-compat.sh"]
fn verify_world_after_vanilla() {
    let dir = compat_world_dir();
    let generator = Generator::new(EXPORT_SEED);
    let edit = compat_edit();
    let pos = ChunkPos::of_block(edit);
    let root = stored_chunk(&dir, pos, false).expect("chunk still stored");
    assert_eq!(
        root.get_bool("isLightOn"),
        Some(true),
        "vanilla should have loaded, relit and re-saved the chunk"
    );
    let Decoded::Chunk(chunk, _) = decode_stored(root, pos).unwrap() else {
        panic!("vanilla reset the chunk");
    };
    assert_eq!(chunk.get(edit.x, edit.y, edit.z).block(), Block::Air);
    let expected = generator.generate(pos);
    for (x, z) in [(0, 0), (15, 15), (7, 3)] {
        let y = generator.height(pos.x * 16 + x, pos.z * 16 + z);
        assert_eq!(
            chunk.get(x, y, z),
            expected.get(x, y, z),
            "vanilla regenerated terrain at ({x}, {y}, {z})"
        );
    }
    let level = tuff_core::storage::level::LevelData::load(
        &VanillaBackend::new(&dir),
        || panic!("seed lost"),
        |_| panic!("spawn lost"),
    )
    .unwrap();
    assert_eq!(level.seed, EXPORT_SEED);

    let lava = compat_lava();
    let pos = ChunkPos::of_block(lava);
    let root = stored_chunk(&dir, pos, false).expect("lava chunk still stored");
    let kept = root.get_list("fluid_ticks").is_some_and(|ticks| {
        ticks.iter().filter_map(Tag::as_compound).any(|tick| {
            tick.get_str("i") == Some("minecraft:lava")
                && (tick.get_int("x"), tick.get_int("y"), tick.get_int("z"))
                    == (Some(lava.x), Some(lava.y), Some(lava.z))
        })
    });
    assert!(
        kept,
        "vanilla loaded the pending lava tick and saved it back"
    );

    let (_, at) = &compat_entities()[0];
    let pos = ChunkPos::of_position(*at);
    let entities = stored_chunk(&dir, pos, true).expect("entities still stored");
    let saved = tuff_storage::entities::decode(&entities, pos).unwrap();
    let tnt = saved
        .iter()
        .find(|entity| entity.get_str("id") == Some("minecraft:tnt"))
        .expect("vanilla kept the primed TNT");
    let fuse = tnt.get_short("fuse").unwrap();
    assert!((COMPAT_TNT_FUSE - 10..=COMPAT_TNT_FUSE).contains(&fuse));
    assert_eq!(tnt.get_uuid("owner"), Some(COMPAT_TNT_OWNER));
    let sheep = saved
        .iter()
        .find(|entity| entity.get_str("id") == Some("minecraft:sheep"))
        .expect("vanilla kept the sheep");
    assert_eq!(sheep.get_byte("Color"), Some(11));
    assert!(sheep.get_int("Age").is_some_and(|age| age < 0));
    let (live, _) = tuff_test_support::vanilla_registries()
        .entity_types
        .load_all(saved, || 1);
    assert_eq!(
        live.len(),
        compat_entities().len(),
        "the TNT, the orb, the mobs and the crystal load back from vanilla's save"
    );

    for (item, at) in compat_block_entities() {
        let pos = ChunkPos::of_block(at);
        let root = stored_chunk(&dir, pos, false).expect("chunk still stored");
        let Decoded::Chunk(_, meta) = decode_stored(root, pos).unwrap() else {
            panic!("vanilla reset the chunk");
        };
        let saved = meta
            .raw_block_entities
            .get(&at)
            .cloned()
            .or_else(|| meta.block_entities.get(&at).map(|entity| entity.encode(at)))
            .unwrap_or_else(|| panic!("{item:?} lost its block entity"));
        match item {
            Item::WhiteBanner => assert_eq!(saved.get_list("patterns").map(<[Tag]>::len), Some(1)),
            Item::BrewingStand => assert_eq!(saved.get_int("total_fuel"), Some(0)),
            Item::OakSign => assert_eq!(
                saved
                    .get_compound("front_text")
                    .and_then(|text| text.get_list("messages"))
                    .map(|lines| lines[0].clone()),
                Some(Tag::from(COMPAT_SIGN_TEXT))
            ),
            _ => {}
        }
    }

    for (x, z, status) in [(9, 1, "features"), (10, 2, "terrain")] {
        let pos = ChunkPos::new(x, z);
        let root = stored_chunk(&dir, pos, false).expect("chunk in generation stored");
        assert_eq!(
            root.get_str("Status"),
            Some("minecraft:full"),
            "vanilla finished the chunk saved at {status}"
        );
    }

    let mut decoded = 0;
    for region in [(-1, -1), (-1, 0), (0, -1), (0, 0)]
        .map(|(x, z)| tuff_world_model::pos::FileRegionPos::new(x, z))
    {
        for local in 0..32 * 32 {
            let pos = ChunkPos::new(region.x * 32 + local % 32, region.z * 32 + local / 32);
            if let Some(root) = stored_chunk(&dir, pos, false) {
                decode_stored(root, pos).unwrap_or_else(|err| panic!("{pos:?}: {err}"));
                decoded += 1;
            }
            if let Some(root) = stored_chunk(&dir, pos, true) {
                tuff_storage::entities::decode(&root, pos)
                    .unwrap_or_else(|err| panic!("entities {pos:?}: {err}"));
            }
        }
    }
    assert!(
        decoded > 81,
        "vanilla should have generated chunks around ours"
    );
}
