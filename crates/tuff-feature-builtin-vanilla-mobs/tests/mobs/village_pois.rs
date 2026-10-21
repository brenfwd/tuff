//! Points of interest that outlive the entities claiming them: tickets
//! saved in the vanilla `poi` files, kept while the claimant is unloaded,
//! and beds claimed across a region border.

use super::*;
use crate::world::region_api::RegionTask;
use tuff_core::storage::codec::WorldBackendExt as _;
use tuff_storage::region_file::RegionFile;
use tuff_vanilla_data::registry::{EntityType, PointOfInterestType};
use tuff_world_model::poi::Record;

const DAY: i64 = 3000;

fn set_time(world: &TestWorld, ticks: i64) {
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(ticks));
}

fn villager(home: Option<BlockPos>) -> Compound {
    let mut nbt = Compound::new()
        .with(
            "VillagerData",
            Compound::new()
                .with("type", "minecraft:plains")
                .with("profession", "minecraft:none")
                .with("level", 1),
        )
        .with("PersistenceRequired", true);
    if let Some(home) = home {
        let place = Compound::new().with(
            "value",
            Compound::new()
                .with("dimension", "minecraft:overworld")
                .with("pos", Tag::IntArray(vec![home.x, home.y, home.z])),
        );
        nbt.insert(
            "Brain",
            Compound::new().with("memories", Compound::new().with("minecraft:home", place)),
        );
    }
    nbt
}

fn home_memory(nbt: &Compound) -> Option<BlockPos> {
    let value = nbt
        .get_compound("Brain")?
        .get_compound("memories")?
        .get_compound("minecraft:home")?
        .get_compound("value")?;
    let [x, y, z] = value.get_int_array("pos")? else {
        return None;
    };
    Some(BlockPos::new(*x, *y, *z))
}

/// A floor at `SKY` over the box, walled in, with a bed whose head is at
/// `head` and foot one block south.
fn yard(
    min: [i32; 2],
    max: [i32; 2],
    head: Option<BlockPos>,
) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in min[0] - 1..=max[0] + 1 {
        for z in min[1] - 1..=max[1] + 1 {
            blocks.push((BlockPos::new(x, SKY, z), Block::GrassBlock.default_state()));
            if x < min[0] || x > max[0] || z < min[1] || z > max[1] {
                blocks.push((BlockPos::new(x, SKY + 1, z), Block::Stone.default_state()));
                blocks.push((BlockPos::new(x, SKY + 2, z), Block::Stone.default_state()));
            }
        }
    }
    if let Some(head) = head {
        let bed = Block::RedBed
            .default_state()
            .with("facing", "north")
            .unwrap();
        blocks.push((head, bed.with("part", "head").unwrap()));
        blocks.push((
            BlockPos::new(head.x, head.y, head.z + 1),
            bed.with("part", "foot").unwrap(),
        ));
    }
    blocks
}

/// The point of interest at `pos` as `region` sees it (its own or
/// mirrored from a neighbour).
async fn record(region: &RegionHandle, pos: BlockPos) -> Option<Record> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: RegionTask::new(move |world| {
                let _ = reply.send(world.pois().record(pos));
            }),
        })
        .await;
    answer.await.unwrap()
}

async fn claims(region: &RegionHandle, pos: BlockPos) -> u32 {
    record(region, pos).await.map_or(0, |r| r.claims())
}

/// Ticks until `region` counts `wanted` claims on `pos`.
async fn wait_for_claims(world: &TestWorld, region: &RegionHandle, pos: BlockPos, wanted: u32) {
    for _ in 0..40 {
        if claims(region, pos).await == wanted {
            return;
        }
        world.ticks(1).await;
    }
    assert_eq!(claims(region, pos).await, wanted, "claims on {pos:?}");
}

/// The records saved for the chunk in the world's `poi` folder.
fn stored_pois(dir: &Path, pos: ChunkPos) -> Option<Vec<Record>> {
    let backend = tuff_storage::vanilla::VanillaBackend::new(dir);
    let folder = backend.paths().poi_dir(OVERWORLD);
    let region = pos.file_region();
    let mut file = RegionFile::open(&folder, region.x, region.z, false).unwrap()?;
    let blob = file.read(pos).unwrap()?.into_blob();
    backend.chunk_codec().decode_pois(&blob, pos).unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bed_stays_claimed_while_its_villager_is_unloaded_and_across_restarts() {
    let dir = TestDir::new();
    let head = BlockPos::new(40, SKY + 1, 40);
    let bed_chunk = ChunkPos::of_block(head);
    let villager_chunk = ChunkPos::new(8, 2);
    let standing = [137.5, SKY as f64 + 1.0, 40.5];

    let world = TestWorld::start(dir.path());
    let region = world.region(bed_chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[bed_chunk, villager_chunk]).await;
    set_time(&world, DAY);
    let mut blocks = yard([36, 36], [44, 44], Some(head));
    blocks.extend(yard([132, 36], [140, 44], None));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Villager,
        villager(Some(head)),
        standing,
    )
    .await;
    wait_for_claims(&world, &region, head, 1).await;
    world.shutdown().await;

    let saved = stored_pois(dir.path(), bed_chunk).expect("the bed's chunk has a poi record");
    assert_eq!(
        saved,
        vec![Record {
            free: 0,
            ..Record::new(head, PointOfInterestType::Home)
        }],
        "saved claimed, as vanilla keeps it"
    );

    let world = TestWorld::start(dir.path());
    let region = world.region(bed_chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[bed_chunk]).await;
    set_time(&world, DAY);
    world.ticks(2).await;
    assert!(mobs(&region, EntityType::Villager).await.is_empty());
    assert_eq!(
        claims(&region, head).await,
        1,
        "the unloaded villager keeps its bed"
    );

    summon(
        &region,
        EntityType::Villager,
        villager(None),
        [40.5, SKY as f64 + 1.0, 37.5],
    )
    .await;
    world.ticks(100).await;
    let newcomer = &mobs(&region, EntityType::Villager).await[0];
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData {
            id: newcomer.id,
            reply,
        })
        .await;
    let newcomer = answer.await.unwrap().unwrap();
    assert_eq!(home_memory(&newcomer), None, "a taken bed is not free");

    subscribe(&region, &viewer, &mut events, &[villager_chunk]).await;
    world.ticks(2).await;
    let owner = mobs(&region, EntityType::Villager)
        .await
        .into_iter()
        .find(|v| ChunkPos::of_position(v.position) == villager_chunk)
        .expect("the owner is back");
    assert_eq!(claims(&region, head).await, 1, "coming back takes no more");
    let answer = region
        .ask(move |world| {
            let ids: Vec<i32> = vec![owner.id];
            ids.into_iter().filter(|id| world.kill_entity(*id)).count()
        })
        .await
        .unwrap();
    assert_eq!(answer, 1);
    wait_for_claims(&world, &region, head, 0).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_claim_a_bed_across_a_region_border_and_free_it_when_they_die() {
    let dir = TestDir::new();
    let border = tuff_world_model::pos::REGION_CHUNKS * 16;
    let head = BlockPos::new(border + 3, SKY + 1, 7);
    let west_chunks = [ChunkPos::new(31, 0), ChunkPos::new(30, 0)];
    let east_chunks = [ChunkPos::new(32, 0), ChunkPos::new(33, 0)];

    let world = TestWorld::start(dir.path());
    let west = world.region(west_chunks[0].region()).await;
    let east = world.region(east_chunks[0].region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&west, &viewer, &mut events, &west_chunks).await;
    subscribe(&east, &viewer, &mut events, &east_chunks).await;
    set_time(&world, DAY);
    let (west_blocks, east_blocks): (Vec<_>, Vec<_>) =
        yard([border - 10, 3], [border + 6, 11], Some(head))
            .into_iter()
            .partition(|(pos, _)| pos.x < border);
    place_blocks(&world, &west, west_blocks).await;
    place_blocks(&world, &east, east_blocks).await;
    assert_eq!(
        record(&west, head).await.map(|r| r.kind),
        Some(PointOfInterestType::Home),
        "the west region mirrors the bed"
    );

    summon(
        &west,
        EntityType::Villager,
        villager(None),
        [border as f64 - 6.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(1).await;
    let id = mobs(&west, EntityType::Villager).await[0].id;
    let mut claimed = false;
    for _ in 0..600 {
        let mut nbt = None;
        for region in [&west, &east] {
            let (reply, answer) = oneshot::channel();
            region
                .request(RegionRequest::EntityData { id, reply })
                .await;
            nbt = nbt.or(answer.await.unwrap());
        }
        if nbt.as_ref().and_then(home_memory) == Some(head) {
            claimed = true;
            break;
        }
        world.ticks(1).await;
    }
    assert!(claimed, "the villager never took the bed across the border");
    wait_for_claims(&world, &east, head, 1).await;
    wait_for_claims(&world, &west, head, 1).await;

    for region in [&west, &east] {
        region
            .ask(move |world| {
                let ids: Vec<i32> = vec![id];
                ids.into_iter().filter(|id| world.kill_entity(*id)).count()
            })
            .await
            .unwrap();
    }
    wait_for_claims(&world, &east, head, 0).await;
    wait_for_claims(&world, &west, head, 0).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cat_spawner_fills_a_village_with_occupied_homes_up_to_five_cats() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(2, 2);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    set_time(&world, DAY);
    let heads: Vec<BlockPos> = (0..5)
        .map(|i| BlockPos::new(35 + 3 * i, SKY + 1, 38))
        .collect();
    let mut blocks = yard([34, 36], [50, 44], Some(heads[0]));
    for head in &heads[1..] {
        let bed = Block::RedBed
            .default_state()
            .with("facing", "north")
            .unwrap();
        blocks.push((*head, bed.with("part", "head").unwrap()));
        blocks.push((
            BlockPos::new(head.x, head.y, head.z + 1),
            bed.with("part", "foot").unwrap(),
        ));
    }
    place_blocks(&world, &region, blocks).await;
    for head in &heads {
        summon(
            &region,
            EntityType::Villager,
            villager(Some(*head)),
            [head.x as f64 + 0.5, SKY as f64 + 1.0, 42.5],
        )
        .await;
    }
    for head in &heads {
        wait_for_claims(&world, &region, *head, 1).await;
    }
    for _ in 0..7 {
        region
            .request(RegionRequest::Run(RegionTask::new(|world| {
                tuff_feature_builtin_vanilla_mobs::world::region::cats::spawn_at(
                    world,
                    BlockPos::new(42, SKY + 1, 42),
                );
            })))
            .await;
    }
    world.ticks(2).await;
    assert_eq!(mobs(&region, EntityType::Cat).await.len(), 5);
    world.shutdown().await;
}
