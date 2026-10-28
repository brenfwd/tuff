//! Natural spawning across region borders: the caps count every region of
//! the dimension, and structure spawn overrides apply in a region that
//! only holds a reference to a start its neighbour owns.

use super::*;
use crate::world::messages::RegionInternal;
use crate::world::region_api::RegionTask;
use tuff_feature_builtin_vanilla_worldgen::chunk_generator::StagedGenerator;
use tuff_feature_builtin_vanilla_worldgen::proto::ProtoChunk;
use tuff_vanilla_data::entity::MobCategory;
use tuff_vanilla_data::registry::EntityType;

const FARM_ANIMALS: [EntityType; 4] = [
    EntityType::Cow,
    EntityType::Pig,
    EntityType::Sheep,
    EntityType::Chicken,
];

async fn animals(area: &Area) -> Vec<crate::world::messages::EntitySnapshot> {
    area.entities()
        .await
        .into_iter()
        .filter(|s| FARM_ANIMALS.contains(&s.kind))
        .collect()
}

/// Runs the next `cycles` animal spawn ticks (every 400th).
async fn spawn_cycles(world: &TestWorld, cycles: u64) {
    for _ in 0..cycles {
        let now = *world.clock.borrow();
        world
            .clock
            .send_modify(|tick| *tick = (now / 400 + 1) * 400 - 1);
        world.ticks(3).await;
    }
}

/// A player at a region border: the creature cap of the 81 loaded chunks
/// around it is 2, while each region's own share would allow 1. Two cows
/// in one region fill the dimension's cap, so the other region spawns no
/// animals; once they are marked persistent they no longer count.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mobs_in_a_neighbour_region_count_towards_the_caps() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    allow_spawning(&world);
    let center = ChunkPos::new(31, 12);
    let (viewer, mut events) = PlayerHandle::new(100);
    let area = Area::subscribe(&world, &viewer, &mut events, &around(center, 4)).await;
    let generator = Generator::new(1);
    let (x, z) = (center.x * 16 + 15, center.z * 16 + 8);
    let player = [
        x as f64 + 0.5,
        generator.height(x, z) as f64 + 1.0,
        z as f64 + 0.5,
    ];
    for region in area.regions() {
        region.request(player_update(&viewer, player, None)).await;
    }
    world.publish_players(OVERWORLD, &[(&viewer, player)]);
    let west = area.owner(player).clone();
    let (cx, cz) = (28 * 16 + 8, 12 * 16 + 8);
    let at = [
        cx as f64 + 0.5,
        generator.height(cx, cz) as f64 + 1.0,
        cz as f64 + 0.5,
    ];
    for _ in 0..2 {
        summon(
            &west,
            EntityType::Cow,
            Compound::new().with("NoAI", true),
            at,
        )
        .await;
    }
    world.ticks(5).await;
    spawn_cycles(&world, 12).await;
    assert_eq!(
        animals(&area).await.len(),
        2,
        "the two cows fill the dimension's creature cap"
    );

    for cow in animals(&area).await {
        let (reply, data) = oneshot::channel();
        west.request(RegionRequest::EntityData { id: cow.id, reply })
            .await;
        let nbt = data.await.unwrap().expect("the cow is in the west region");
        let (id, nbt) = (cow.id, nbt.with("PersistenceRequired", true));
        let done = west.ask(move |world| world.replace_entity(id, nbt)).await;
        assert_eq!(done, Some(true));
    }
    let mut spawned = 0;
    for _ in 0..20 {
        spawn_cycles(&world, 1).await;
        spawned = animals(&area).await.len() - 2;
        if spawned > 0 {
            break;
        }
    }
    assert!(spawned > 0, "persistent cows leave the cap free");
    world.shutdown().await;
}

/// Two cows held beyond 128 blocks of the only player fill the creature cap
/// of the loaded chunks around it, so nothing spawns for the player unless
/// the caps are per player, where only the mobs near them count.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_player_caps_ignore_mobs_far_from_the_player() {
    for per_player in [false, true] {
        let dir = TestDir::new();
        let setting = if per_player { "true" } else { "false" };
        let world = TestWorld::start_with_settings(
            dir.path(),
            Vec::new(),
            &[("tuff.mobs.spawnCap.perPlayer", setting)],
        );
        allow_spawning(&world);
        let center = ChunkPos::new(31, 12);
        let (viewer, mut events) = PlayerHandle::new(100);
        let area = Area::subscribe(&world, &viewer, &mut events, &around(center, 4)).await;
        let far_chunk = ChunkPos::new(10, 12);
        let (far_viewer, mut far_events) = PlayerHandle::new(101);
        let far_area =
            Area::subscribe(&world, &far_viewer, &mut far_events, &around(far_chunk, 1)).await;
        let generator = Generator::new(1);
        let (x, z) = (center.x * 16 + 8, center.z * 16 + 8);
        let player = [
            x as f64 + 0.5,
            generator.height(x, z) as f64 + 1.0,
            z as f64 + 0.5,
        ];
        for region in area.regions() {
            region.request(player_update(&viewer, player, None)).await;
        }
        world.publish_players(OVERWORLD, &[(&viewer, player)]);
        let (fx, fz) = (far_chunk.x * 16 + 8, far_chunk.z * 16 + 8);
        let at = [
            fx as f64 + 0.5,
            generator.height(fx, fz) as f64 + 1.0,
            fz as f64 + 0.5,
        ];
        let far_owner = far_area.owner(at).clone();
        for _ in 0..2 {
            summon(
                &far_owner,
                EntityType::Cow,
                Compound::new().with("NoAI", true),
                at,
            )
            .await;
        }
        world.ticks(5).await;
        let mut spawned = 0;
        for _ in 0..12 {
            spawn_cycles(&world, 1).await;
            spawned = animals(&area).await.len().saturating_sub(2);
            if spawned > 0 {
                break;
            }
        }
        if per_player {
            assert!(spawned > 0, "far mobs do not count against the player");
        } else {
            assert_eq!(spawned, 0, "the dimension cap is full");
        }
        world.shutdown().await;
    }
}

/// Animals spawn around a player in the open, and none once the border
/// leaves every spawn candidate outside it.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn natural_spawning_stays_inside_the_world_border() {
    for fenced in [false, true] {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        allow_spawning(&world);
        if fenced {
            let id = world.dimension("minecraft:overworld");
            world.borders[id.index()].send_replace(tuff_core::world::border::Border::fixed(
                [100000.0, 100000.0],
                10.0,
            ));
        }
        let center = ChunkPos::new(10, 12);
        let (viewer, mut events) = PlayerHandle::new(100);
        let area = Area::subscribe(&world, &viewer, &mut events, &around(center, 4)).await;
        let generator = Generator::new(1);
        let (x, z) = (center.x * 16 + 8, center.z * 16 + 8);
        let player = [
            x as f64 + 0.5,
            generator.height(x, z) as f64 + 1.0,
            z as f64 + 0.5,
        ];
        for region in area.regions() {
            region.request(player_update(&viewer, player, None)).await;
        }
        world.publish_players(OVERWORLD, &[(&viewer, player)]);
        world.ticks(5).await;
        spawn_cycles(&world, 12).await;
        let spawned = animals(&area).await.len();
        if fenced {
            assert_eq!(spawned, 0, "nothing spawns outside the border");
        } else {
            assert!(spawned > 0, "the control run spawns animals");
        }
        world.shutdown().await;
    }
}

/// A stone floor to y 60 under one fortress whose start is in chunk
/// (31, 0), in region (0, 0); a piece and its box reach over the border
/// into chunk (32, 0), which holds a reference to the start.
struct Fortress;

const START: ChunkPos = ChunkPos::new(31, 0);
const REFERENCING: ChunkPos = ChunkPos::new(32, 0);
const IN_PIECE: BlockPos = BlockPos::new(514, 62, 5);
const ON_BRICKS: BlockPos = BlockPos::new(515, 62, 14);
const ON_STONE: BlockPos = BlockPos::new(517, 62, 14);

impl ChunkGenerator for Fortress {
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

impl StagedGenerator for Fortress {
    fn terrain(
        &self,
        pos: ChunkPos,
        shape: ChunkShape,
        _: &[Arc<tuff_feature_builtin_vanilla_worldgen::chunk_generator::StartsBlock>],
    ) -> ProtoChunk {
        let mut proto = ProtoChunk::new(pos, shape);
        for y in shape.min_y..=61 {
            for z in 0..16 {
                for x in 0..16 {
                    proto.set(x, y, z, Block::Stone.default_state());
                }
            }
        }
        proto.finish_terrain();
        proto
    }

    fn decorate(&self, _center: ChunkPos, window: Vec<ProtoChunk>) -> Vec<ProtoChunk> {
        window
    }

    fn finish(&self, mut proto: ProtoChunk) -> generator::Finished {
        let fortress = "minecraft:fortress";
        let structures = if proto.pos == START {
            let piece = |bb: [i32; 6]| {
                Tag::Compound(Compound::new().with("BB", Tag::IntArray(bb.to_vec())))
            };
            Some(
                Compound::new().with("References", Compound::new()).with(
                    "starts",
                    Compound::new().with(
                        fortress,
                        Compound::new()
                            .with("id", fortress)
                            .with("ChunkX", START.x)
                            .with("ChunkZ", START.z)
                            .with(
                                "Children",
                                Tag::List(vec![
                                    piece([500, 61, 0, 516, 70, 10]),
                                    piece([490, 61, 20, 495, 70, 25]),
                                ]),
                            ),
                    ),
                ),
            )
        } else if proto.pos == REFERENCING {
            let packed = (START.x as u32 as i64) | ((START.z as i64) << 32);
            proto.set(
                ON_BRICKS.x & 15,
                ON_BRICKS.y - 1,
                ON_BRICKS.z & 15,
                Block::NetherBricks.default_state(),
            );
            Some(
                Compound::new()
                    .with(
                        "References",
                        Compound::new().with(fortress, Tag::LongArray(vec![packed])),
                    )
                    .with("starts", Compound::new()),
            )
        } else {
            None
        };
        generator::Finished {
            structures,
            ..generator::Finished::new(proto.into_chunk())
        }
    }
}

fn fortress(_: &DimensionSettings, _: i64) -> Arc<dyn ChunkGenerator> {
    Arc::new(Fortress)
}

/// The spawn lists region (1, 0) gives for monsters at `positions`, read
/// on the region.
async fn monster_lists(region: &RegionHandle, positions: Vec<BlockPos>) -> Vec<Vec<EntityType>> {
    let (reply, lists) = oneshot::channel();
    region.internal(RegionInternal::Run(RegionTask::new(move |world| {
        let lists = positions
            .into_iter()
            .map(|pos| {
                tuff_feature_builtin_vanilla_mobs::world::region::spawners::spawn_list(
                    world,
                    pos,
                    "minecraft:plains",
                    MobCategory::Monster,
                )
                .iter()
                .map(|s| s.kind)
                .collect()
            })
            .collect();
        let _ = reply.send(lists);
    })));
    lists.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fortress_spawns_apply_across_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), fortress);
    let (viewer, mut events) = PlayerHandle::new(100);
    let chunks: Vec<ChunkPos> = around(ChunkPos::new(31, 1), 2)
        .into_iter()
        .chain(around(ChunkPos::new(32, 1), 2))
        .collect();
    let area = Area::subscribe(&world, &viewer, &mut events, &chunks).await;
    let east = area.owner(IN_PIECE.center()).clone();
    let (reply, mut ready) = oneshot::channel();
    east.request(RegionRequest::WhenLoaded {
        min: ON_STONE,
        max: ON_STONE,
        run: RegionTask::new(move |_| {
            let _ = reply.send(());
        }),
    })
    .await;
    for _ in 0..400 {
        if ready.try_recv().is_ok() {
            break;
        }
        world.ticks(1).await;
    }
    let mut asked = Vec::new();
    for _ in 0..3 {
        asked = monster_lists(&east, vec![IN_PIECE, ON_BRICKS, ON_STONE]).await;
        if asked[0].contains(&EntityType::Blaze) {
            break;
        }
        world.ticks(2).await;
    }
    assert!(
        asked[0].contains(&EntityType::Blaze),
        "inside a piece of the neighbour's fortress: {:?}",
        asked[0]
    );
    assert!(
        asked[1].contains(&EntityType::Blaze),
        "on nether bricks inside its box: {:?}",
        asked[1]
    );
    assert!(
        !asked[2].contains(&EntityType::Blaze),
        "outside its pieces, off the bricks: {:?}",
        asked[2]
    );
    world.shutdown().await;
}
