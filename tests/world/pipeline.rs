use super::*;
use crate::world::messages::{BlockOp, BlockOpResult};
use tuff_feature_builtin_vanilla_worldgen::chunk_generator::StagedGenerator;
use tuff_feature_builtin_vanilla_worldgen::proto::ProtoChunk;

/// A stone floor to y 60; decorating a chunk puts a gold block on top
/// of every chunk in its window, at an x that says who placed it.
struct Staged;

impl ChunkGenerator for Staged {
    fn generate(&self, _: ChunkPos, shape: ChunkShape) -> tuff_world_model::chunk::Chunk {
        tuff_world_model::chunk::Chunk::empty(shape)
    }

    fn sea_level(&self) -> i32 {
        63
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, 61, 0)
    }

    fn pipeline(
        self: Arc<Self>,
        cx: tuff_core::world::generator::PipelineContext,
    ) -> Option<Arc<dyn tuff_core::world::generator::ChunkPipeline>> {
        Some(Arc::new(
            tuff_feature_builtin_vanilla_worldgen::pipeline::GenerationActor::spawn(self, cx),
        ))
    }
}

impl StagedGenerator for Staged {
    fn terrain(
        &self,
        pos: ChunkPos,
        shape: ChunkShape,
        _: &[Arc<tuff_feature_builtin_vanilla_worldgen::chunk_generator::StartsBlock>],
    ) -> ProtoChunk {
        let mut proto = ProtoChunk::new(pos, shape);
        for y in shape.min_y..=60 {
            for z in 0..16 {
                for x in 0..16 {
                    proto.set(x, y, z, Block::Stone.default_state());
                }
            }
        }
        proto.finish_terrain();
        proto
    }

    fn decorate(&self, center: ChunkPos, mut window: Vec<ProtoChunk>) -> Vec<ProtoChunk> {
        for chunk in &mut window {
            let (dx, dz) = (center.x - chunk.pos.x, center.z - chunk.pos.z);
            chunk.set(
                dx + 1 + 3 * (dz + 1),
                61,
                0,
                Block::GoldBlock.default_state(),
            );
        }
        window
    }

    fn finish(&self, mut proto: ProtoChunk) -> generator::Finished {
        if proto.pos != ChunkPos::new(0, 0) {
            return generator::Finished::new(proto.into_chunk());
        }
        proto.set(1, 61, 1, Block::Chest.default_state());
        proto.set(2, 61, 1, Block::Furnace.default_state());
        proto.set(5, 61, 5, Block::Water.default_state());
        proto.set(8, 61, 8, Block::OakFence.default_state());
        proto.set(9, 61, 8, Block::OakFence.default_state());
        proto.set(12, 61, 12, Block::Lava.default_state());
        let at = |x: i32| Compound::new().with("x", x).with("y", 61).with("z", 1);
        let block_entities = vec![
            at(1)
                .with("id", "minecraft:chest")
                .with("LootTable", "minecraft:chests/ruined_portal")
                .with("LootTableSeed", 42i64),
            at(3).with("id", "minecraft:barrel"),
        ];
        let structures = Compound::new().with("References", Compound::new()).with(
            "starts",
            Compound::new().with(
                "minecraft:ruined_portal",
                Compound::new().with("id", "minecraft:ruined_portal"),
            ),
        );
        generator::Finished {
            block_entities,
            structures: Some(structures),
            post_processing: vec![BlockPos::new(5, 61, 5), BlockPos::new(8, 61, 8)],
            fluid_ticks: vec![
                tuff_feature_builtin_vanilla_worldgen::proto::ScheduledTick {
                    kind: "minecraft:lava",
                    pos: (12, 61, 12),
                    delay: 0,
                },
            ],
            ..generator::Finished::new(proto.into_chunk())
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn generated_structure_block_entities_are_repaired_and_saved() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), staged);
    let region = world.region(RegionPos::new(0, 0)).await;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Get {
                positions: vec![BlockPos::new(1, 61, 1)],
                load: true,
            },
            reply,
        })
        .await;
    tokio::time::timeout(TIMEOUT, answer)
        .await
        .expect("chunk generates")
        .unwrap();
    world.shutdown().await;

    let pos = ChunkPos::new(0, 0);
    let root = stored_chunk(dir.path(), pos, false).expect("chunk saved");
    let stored: Vec<&Compound> = root
        .get_list("block_entities")
        .unwrap()
        .iter()
        .filter_map(Tag::as_compound)
        .collect();
    let at = |x: i32| stored.iter().find(|nbt| nbt.get_int("x") == Some(x));
    let chest = at(1).expect("loot chest kept");
    assert_eq!(chest.get_str("id"), Some("minecraft:chest"));
    assert_eq!(
        chest.get_str("LootTable"),
        Some("minecraft:chests/ruined_portal")
    );
    assert_eq!(chest.get_long("LootTableSeed"), Some(42));
    assert_eq!(
        at(2).and_then(|nbt| nbt.get_str("id")),
        Some("minecraft:furnace"),
        "missing block entity created"
    );
    assert!(at(3).is_none(), "block entity without its block dropped");
    let structures = root.get_compound("structures").expect("structures saved");
    assert!(
        structures
            .get_compound("starts")
            .unwrap()
            .contains_key("minecraft:ruined_portal")
    );
}

/// A generated chunk goes live with its post-processing (the first
/// fluid tick and shape updates of marked blocks) and its scheduled
/// ticks, once its neighbours are loaded.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_chunks_run_their_post_processing_and_ticks() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), staged);
    let mut regions = std::collections::HashMap::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            let pos = BlockPos::new(dx * 16 + 1, 61, dz * 16 + 1);
            let region = ChunkPos::of_block(pos).region();
            if let std::collections::hash_map::Entry::Vacant(e) = regions.entry(region) {
                e.insert(world.region(region).await);
            }
            let (reply, answer) = oneshot::channel();
            regions[&region]
                .request(RegionRequest::Blocks {
                    op: BlockOp::Get {
                        positions: vec![pos],
                        load: true,
                    },
                    reply,
                })
                .await;
            tokio::time::timeout(TIMEOUT, answer)
                .await
                .expect("chunk generates")
                .unwrap();
        }
    }
    let region = &regions[&RegionPos::new(0, 0)];
    let mut live = false;
    for _ in 0..40 {
        let water = block_at(region, BlockPos::new(6, 61, 5)).await;
        let fence = block_at(region, BlockPos::new(8, 61, 8)).await;
        let lava = block_at(region, BlockPos::new(13, 61, 12)).await;
        if water.is_some_and(|s| s.block() == Block::Water)
            && fence.is_some_and(|s| s.get("east") == Some("true"))
            && lava.is_some_and(|s| s.block() == Block::Lava)
        {
            live = true;
            break;
        }
        world.ticks(5).await;
    }
    assert!(live, "post-processing and scheduled ticks ran");
    world.shutdown().await;
}

pub(super) fn staged(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
    if settings.key == OVERWORLD {
        Arc::new(Staged)
    } else {
        tuff_test_support::placeholder::for_settings(settings, seed)
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn regions_install_chunks_the_pipeline_decorated() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), staged);
    let region = world.region(RegionPos::new(0, 0)).await;
    let positions: Vec<BlockPos> = (0..9)
        .flat_map(|x| [BlockPos::new(x, 61, 0), BlockPos::new(32 + x, 61, 16)])
        .collect();
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Get {
                positions: positions.clone(),
                load: true,
            },
            reply,
        })
        .await;
    let states = tokio::time::timeout(TIMEOUT, answer)
        .await
        .expect("chunks generate")
        .unwrap();
    let BlockOpResult::States(states) = states else {
        panic!("expected states");
    };
    for (pos, state) in positions.iter().zip(states) {
        assert_eq!(
            state.map(|s| s.block()),
            Some(Block::GoldBlock),
            "{pos:?} lacks a neighbour's decoration"
        );
    }
    world.shutdown().await;
}

/// The staged test terrain, counting how often each chunk is decorated.
struct Counted;

fn decorations() -> &'static std::sync::Mutex<std::collections::HashMap<ChunkPos, u32>> {
    static COUNTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<ChunkPos, u32>>> =
        std::sync::OnceLock::new();
    COUNTS.get_or_init(Default::default)
}

impl ChunkGenerator for Counted {
    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> tuff_world_model::chunk::Chunk {
        Staged.generate(pos, shape)
    }

    fn sea_level(&self) -> i32 {
        Staged.sea_level()
    }

    fn spawn(&self) -> BlockPos {
        Staged.spawn()
    }

    fn pipeline(
        self: Arc<Self>,
        cx: tuff_core::world::generator::PipelineContext,
    ) -> Option<Arc<dyn tuff_core::world::generator::ChunkPipeline>> {
        Some(Arc::new(
            tuff_feature_builtin_vanilla_worldgen::pipeline::GenerationActor::spawn(self, cx),
        ))
    }
}

impl StagedGenerator for Counted {
    fn terrain(
        &self,
        pos: ChunkPos,
        shape: ChunkShape,
        starts: &[Arc<tuff_feature_builtin_vanilla_worldgen::chunk_generator::StartsBlock>],
    ) -> ProtoChunk {
        Staged.terrain(pos, shape, starts)
    }

    fn decorate(&self, center: ChunkPos, window: Vec<ProtoChunk>) -> Vec<ProtoChunk> {
        *decorations().lock().unwrap().entry(center).or_default() += 1;
        Staged.decorate(center, window)
    }

    fn finish(&self, proto: ProtoChunk) -> generator::Finished {
        generator::Finished::new(proto.into_chunk())
    }
}

fn counted(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
    if settings.key == OVERWORLD {
        Arc::new(Counted)
    } else {
        tuff_test_support::placeholder::for_settings(settings, seed)
    }
}

async fn load_gold(world: &TestWorld, chunk: ChunkPos) {
    let region = world.region(chunk.region()).await;
    let positions: Vec<BlockPos> = (0..9)
        .map(|x| BlockPos::new(chunk.x * 16 + x, 61, chunk.z * 16))
        .collect();
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Get {
                positions: positions.clone(),
                load: true,
            },
            reply,
        })
        .await;
    let BlockOpResult::States(states) = tokio::time::timeout(TIMEOUT, answer)
        .await
        .expect("chunk generates")
        .unwrap()
    else {
        panic!("expected states");
    };
    for (pos, state) in positions.iter().zip(states) {
        assert_eq!(
            state.map(|s| s.block()),
            Some(Block::GoldBlock),
            "{pos:?} lacks a neighbour's decoration"
        );
    }
}

/// Chunks still in generation when the world stops are saved the way the
/// game saves them and picked up again: the next run decorates no chunk a
/// second time, and chunks finished then carry every neighbour's features.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunks_in_generation_are_saved_and_resumed() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), counted);
    load_gold(&world, ChunkPos::new(40, 0)).await;
    world.shutdown().await;
    let status = |x, z| {
        stored_chunk(dir.path(), ChunkPos::new(x, z), false)
            .and_then(|root| root.get_str("Status").map(str::to_owned))
    };
    assert_eq!(status(40, 0).as_deref(), Some("minecraft:full"));
    assert_eq!(status(41, 1).as_deref(), Some("minecraft:features"));
    assert_eq!(status(42, 2).as_deref(), Some("minecraft:terrain"));

    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), counted);
    load_gold(&world, ChunkPos::new(42, 0)).await;
    world.shutdown().await;
    let counts = decorations().lock().unwrap();
    let ours: Vec<_> = counts.iter().filter(|(pos, _)| pos.x >= 38).collect();
    assert!(!ours.is_empty());
    assert!(ours.iter().all(|(_, n)| **n == 1), "{ours:?}");
    assert_eq!(status(42, 0).as_deref(), Some("minecraft:full"));
}
