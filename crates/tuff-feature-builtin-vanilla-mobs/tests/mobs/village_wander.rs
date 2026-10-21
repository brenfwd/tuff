//! Villagers keep to their village: idle strolls lean back toward one, a
//! homeless villager walks to a bed at night, and lightning makes a witch
//! of a villager.

use super::*;
use crate::world::messages::EntitySnapshot;
use crate::world::player::Difficulty;
use crate::world::region_api::RegionTask;
use tuff_vanilla_data::registry::EntityType;

const DAY: i64 = 3000;
const NIGHT: i64 = 13000;

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

/// A grass floor at `SKY` over the box, walled two blocks high, with a
/// bed whose head is at `head`, foot one block south.
fn yard(
    min: [i32; 2],
    max: [i32; 2],
    head: BlockPos,
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
    let bed = Block::RedBed
        .default_state()
        .with("facing", "north")
        .unwrap();
    blocks.push((head, bed.with("part", "head").unwrap()));
    blocks.push((
        BlockPos::new(head.x, head.y, head.z + 1),
        bed.with("part", "foot").unwrap(),
    ));
    blocks
}

async fn claims(region: &RegionHandle, pos: BlockPos) -> u32 {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: RegionTask::new(move |world| {
                let _ = reply.send(world.pois().record(pos).map_or(0, |r| r.claims()));
            }),
        })
        .await;
    answer.await.unwrap()
}

/// A village of one claimed bed at `head` (its owner stands still in the
/// corner at `owner`), with the yard's chunks loaded.
async fn village(
    world: &TestWorld,
    min: [i32; 2],
    max: [i32; 2],
    head: BlockPos,
    owner: [f64; 3],
) -> (RegionHandle, PlayerHandle) {
    let chunks: Vec<ChunkPos> = (min[0] >> 4..=max[0] >> 4)
        .flat_map(|x| (min[1] >> 4..=max[1] >> 4).map(move |z| ChunkPos::new(x, z)))
        .collect();
    let region = world.region(chunks[0].region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &chunks).await;
    place_blocks(world, &region, yard(min, max, head)).await;
    summon(
        &region,
        EntityType::Villager,
        villager(Some(head)).with("NoAI", true),
        owner,
    )
    .await;
    for _ in 0..40 {
        if claims(&region, head).await == 1 {
            break;
        }
        world.ticks(1).await;
    }
    assert_eq!(claims(&region, head).await, 1, "the owner claims its bed");
    (region, viewer)
}

async fn position(region: &RegionHandle, id: i32) -> [f64; 3] {
    mobs(region, EntityType::Villager)
        .await
        .into_iter()
        .find(|s| s.id == id)
        .expect("the villager is there")
        .position
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_villager_outside_its_village_strolls_back_toward_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_time(&world, DAY);
    let head = BlockPos::new(8, SKY + 1, 6);
    let (region, _viewer) =
        village(&world, [0, 0], [79, 79], head, [1.5, SKY as f64 + 1.0, 1.5]).await;
    summon(
        &region,
        EntityType::Villager,
        villager(None),
        [72.5, SKY as f64 + 1.0, 72.5],
    )
    .await;
    world.ticks(2).await;
    let wanderer = mobs(&region, EntityType::Villager)
        .await
        .iter()
        .map(|s| s.id)
        .max()
        .unwrap();
    let mut closest = f64::MAX;
    for _ in 0..120 {
        world.ticks(50).await;
        let [x, _, z] = position(&region, wanderer).await;
        closest = closest.min(x.max(z));
        if closest < 32.0 {
            break;
        }
    }
    assert!(
        closest < 32.0,
        "four sections out, it reaches a section next to the village's: {closest}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_homeless_villager_walks_to_the_closest_bed_at_night() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_time(&world, NIGHT);
    let head = BlockPos::new(4, SKY + 1, 6);
    let (region, _viewer) = village(
        &world,
        [0, 2],
        [31, 13],
        head,
        [1.5, SKY as f64 + 1.0, 12.5],
    )
    .await;
    summon(
        &region,
        EntityType::Villager,
        villager(None),
        [28.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    world.ticks(2).await;
    let homeless = mobs(&region, EntityType::Villager)
        .await
        .iter()
        .map(|s| s.id)
        .max()
        .unwrap();
    let bed = head.center();
    let mut nearest = f64::MAX;
    for _ in 0..30 {
        world.ticks(20).await;
        let at = position(&region, homeless).await;
        let d = ((at[0] - bed[0]).powi(2) + (at[2] - bed[2]).powi(2)).sqrt();
        nearest = nearest.min(d);
        if nearest < 3.0 {
            break;
        }
    }
    assert!(
        nearest < 3.0,
        "it walks up to the claimed bed it cannot have: {nearest}"
    );
}

async fn entity_data(region: &RegionHandle, id: i32) -> Option<Compound> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap()
}

/// What stands in the pen after lightning strikes a villager there: every
/// entity, the villager's data and the witch's, when there is one.
async fn struck(
    difficulty: Difficulty,
) -> (Vec<EntitySnapshot>, Option<Compound>, Option<Compound>) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.difficulty.send_replace(difficulty);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(4, 11)).await;
    let position = [7.5, SKY as f64 + 1.0, 7.5];
    summon(
        &region,
        EntityType::Villager,
        villager(None).with("NoAI", true),
        position,
    )
    .await;
    world.ticks(2).await;
    let villager = mobs(&region, EntityType::Villager).await[0].id;
    summon(
        &region,
        EntityType::LightningBolt,
        Compound::new(),
        position,
    )
    .await;
    world.ticks(10).await;
    let found = snapshots(&region).await;
    let witch = match found.iter().find(|s| s.kind == EntityType::Witch) {
        Some(witch) => entity_data(&region, witch.id).await,
        None => None,
    };
    (found, entity_data(&region, villager).await, witch)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lightning_turns_a_villager_into_a_witch() {
    let (found, villager, data) = struck(Difficulty::Normal).await;
    assert!(villager.is_none(), "the villager is gone");
    assert!(
        !found.iter().any(|s| s.kind == EntityType::Villager),
        "no villager is left"
    );
    let witch = found
        .iter()
        .find(|s| s.kind == EntityType::Witch)
        .expect("a witch takes its place");
    assert!((witch.position[0] - 7.5).abs() < 0.5 && (witch.position[2] - 7.5).abs() < 0.5);
    let data = data.expect("the witch's data");
    assert_eq!(data.get_byte("PersistenceRequired"), Some(1));
    assert_eq!(
        data.get_byte("NoAI"),
        Some(1),
        "it keeps the villager's flags"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lightning_only_hurts_a_villager_in_peaceful() {
    let (found, villager, _) = struck(Difficulty::Peaceful).await;
    assert!(!found.iter().any(|s| s.kind == EntityType::Witch));
    let villager = villager.expect("the villager stays");
    let health = villager.get_float("Health").unwrap();
    assert!(health < 20.0, "the strike hurts it: {health}");
}

async fn pig_struck(difficulty: Difficulty) -> Vec<EntitySnapshot> {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.difficulty.send_replace(difficulty);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(4, 11)).await;
    let position = [7.5, SKY as f64 + 1.0, 7.5];
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        position,
    )
    .await;
    world.ticks(2).await;
    summon(
        &region,
        EntityType::LightningBolt,
        Compound::new(),
        position,
    )
    .await;
    world.ticks(10).await;
    let found = snapshots(&region).await;
    world.shutdown().await;
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lightning_turns_a_pig_into_a_zombified_piglin() {
    let found = pig_struck(Difficulty::Normal).await;
    assert!(!found.iter().any(|s| s.kind == EntityType::Pig));
    assert!(found.iter().any(|s| s.kind == EntityType::ZombifiedPiglin));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lightning_leaves_a_pig_a_pig_in_peaceful() {
    let found = pig_struck(Difficulty::Peaceful).await;
    assert!(!found.iter().any(|s| s.kind == EntityType::ZombifiedPiglin));
}
