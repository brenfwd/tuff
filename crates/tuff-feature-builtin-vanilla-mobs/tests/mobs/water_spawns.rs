//! Natural spawning of water mobs: open ocean, a flooded lush cave, and
//! water ambient mobs despawning away from players.

use super::*;
use crate::world::messages::EntitySnapshot;
use tuff_test_support::placeholder::Layered;
use tuff_vanilla_data::registry::EntityType;

const SEA_LEVEL: i32 = 63;

fn ocean(_: &storage::level::DimensionSettings, _: i64) -> Arc<dyn generator::ChunkGenerator> {
    Arc::new(Layered {
        layers: vec![
            (40, Block::Stone.default_state()),
            (SEA_LEVEL - 1, Block::Water.default_state()),
        ],
        biome: "minecraft:ocean",
        sea_level: SEA_LEVEL,
    })
}

fn lush_cave(_: &storage::level::DimensionSettings, _: i64) -> Arc<dyn generator::ChunkGenerator> {
    Arc::new(Layered {
        layers: vec![
            (0, Block::Clay.default_state()),
            (8, Block::Water.default_state()),
            (80, Block::Stone.default_state()),
        ],
        biome: "minecraft:lush_caves",
        sea_level: SEA_LEVEL,
    })
}

async fn spawned_until(
    world: &TestWorld,
    area: &Area,
    wanted: impl Fn(&EntitySnapshot) -> bool,
    ticks: u32,
) -> Vec<EntitySnapshot> {
    for _ in 0..ticks {
        world.ticks(1).await;
        let found: Vec<EntitySnapshot> =
            area.entities().await.into_iter().filter(&wanted).collect();
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn squid_and_cod_spawn_in_open_ocean_near_the_surface() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), ocean);
    allow_spawning(&world);
    let center = ChunkPos::new(12, 12);
    let (viewer, mut events) = PlayerHandle::new(100);
    let area = Area::subscribe(&world, &viewer, &mut events, &around(center, 4)).await;
    let region = area.owner([center.x as f64 * 16.0, 0.0, center.z as f64 * 16.0]);
    let player = [
        center.x as f64 * 16.0 + 8.5,
        SEA_LEVEL as f64,
        center.z as f64 * 16.0 + 8.5,
    ];
    region.request(player_update(&viewer, player, None)).await;
    let water = [EntityType::Squid, EntityType::Cod, EntityType::Dolphin];
    let mut kinds = HashSet::new();
    for _ in 0..10 {
        let spawned = spawned_until(&world, &area, |s| water.contains(&s.kind), 20).await;
        for mob in &spawned {
            let y = mob.position[1].floor() as i32;
            assert!(
                (SEA_LEVEL - 13..SEA_LEVEL).contains(&y),
                "{mob:?} spawned outside the surface band"
            );
            assert!(
                crate::world::distance_sq(mob.position, player) > 24.0 * 24.0,
                "{mob:?} too close to the player"
            );
            kinds.insert(mob.kind);
        }
        if kinds.contains(&EntityType::Squid) && kinds.contains(&EntityType::Cod) {
            break;
        }
    }
    assert!(
        kinds.contains(&EntityType::Cod),
        "no cod spawned: {kinds:?}"
    );
    assert!(
        kinds.contains(&EntityType::Squid),
        "no squid spawned: {kinds:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn axolotls_and_glow_squid_spawn_in_flooded_lush_caves() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), lush_cave);
    allow_spawning(&world);
    let center = ChunkPos::new(12, 12);
    let (viewer, mut events) = PlayerHandle::new(100);
    let area = Area::subscribe(&world, &viewer, &mut events, &around(center, 4)).await;
    let region = area.owner([center.x as f64 * 16.0, 0.0, center.z as f64 * 16.0]);
    let player = [
        center.x as f64 * 16.0 + 8.5,
        81.0,
        center.z as f64 * 16.0 + 8.5,
    ];
    region.request(player_update(&viewer, player, None)).await;
    let mut kinds = HashSet::new();
    for _ in 0..10 {
        let spawned = spawned_until(
            &world,
            &area,
            |s| matches!(s.kind, EntityType::Axolotl | EntityType::GlowSquid),
            20,
        )
        .await;
        for mob in &spawned {
            let y = mob.position[1].floor() as i32;
            assert!((1..=8).contains(&y), "{mob:?} spawned outside the water");
            if mob.kind == EntityType::Axolotl {
                assert_eq!(y, 1, "axolotls stand on the clay");
            }
            kinds.insert(mob.kind);
        }
        if kinds.len() == 2 {
            break;
        }
    }
    assert!(kinds.contains(&EntityType::Axolotl), "{kinds:?}");
    assert!(kinds.contains(&EntityType::GlowSquid), "{kinds:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn water_ambient_mobs_despawn_beyond_64_blocks() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), ocean);
    let center = ChunkPos::new(12, 12);
    let (viewer, mut events) = PlayerHandle::new(100);
    let area = Area::subscribe(&world, &viewer, &mut events, &around(center, 5)).await;
    let region = area.owner([center.x as f64 * 16.0, 0.0, center.z as f64 * 16.0]);
    let (x, z) = (center.x as f64 * 16.0 + 8.5, center.z as f64 * 16.0 + 8.5);
    region
        .request(player_update(&viewer, [x, SEA_LEVEL as f64, z], None))
        .await;
    world.publish_players(OVERWORLD, &[(&viewer, [x, SEA_LEVEL as f64, z])]);
    summon(
        area.owner([x + 70.0, 55.0, z]),
        EntityType::Cod,
        Compound::new(),
        [x + 70.0, 55.0, z],
    )
    .await;
    summon(
        area.owner([x + 20.0, 55.0, z]),
        EntityType::Cod,
        Compound::new(),
        [x + 20.0, 55.0, z],
    )
    .await;
    summon(
        area.owner([x + 70.0, 55.0, z]),
        EntityType::Squid,
        Compound::new(),
        [x + 70.0, 55.0, z],
    )
    .await;
    world.ticks(3).await;
    let left: Vec<(EntityType, bool)> = area
        .entities()
        .await
        .into_iter()
        .map(|s| (s.kind, s.position[0] > x + 40.0))
        .collect();
    assert!(left.contains(&(EntityType::Cod, false)), "{left:?}");
    assert!(!left.contains(&(EntityType::Cod, true)), "{left:?}");
    assert!(left.contains(&(EntityType::Squid, true)), "{left:?}");
    world.shutdown().await;
}
