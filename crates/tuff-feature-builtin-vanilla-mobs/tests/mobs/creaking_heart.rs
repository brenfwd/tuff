//! Creaking hearts: waking at night, spawning and tracking a protector,
//! reporting its distance to comparators, answering hits on it, and
//! removing it at daybreak or when the heart is broken.

use super::*;
use crate::world::messages::PlayerLooks;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::EntityType;

const O: i32 = 32;
const HEART: BlockPos = BlockPos::new(O + 8, SKY + 2, O + 8);
const COMPARATOR: BlockPos = BlockPos::new(O + 9, SKY + 2, O + 8);
const CENTRE: ChunkPos = ChunkPos::new(2, 2);
const PLAYER: [f64; 3] = [O as f64 + 3.5, SKY as f64 + 1.0, O as f64 + 3.5];

fn around() -> Vec<ChunkPos> {
    (1..=3)
        .flat_map(|x| (1..=3).map(move |z| ChunkPos::new(x, z)))
        .collect()
}

fn set_time(world: &TestWorld, ticks: i64) {
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(ticks));
}

fn grove() -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let stone = Block::Stone.default_state();
    let log = Block::PaleOakLog.default_state().with("axis", "y").unwrap();
    let mut blocks = Vec::new();
    for x in 0..16 {
        for z in 0..16 {
            blocks.push((BlockPos::new(O + x, SKY, O + z), stone));
        }
    }
    blocks.push((BlockPos::new(O + 8, SKY + 1, O + 8), log));
    blocks.push((BlockPos::new(O + 8, SKY + 3, O + 8), log));
    blocks.push((
        HEART,
        Block::CreakingHeart
            .default_state()
            .with("axis", "y")
            .unwrap()
            .with("natural", "true")
            .unwrap(),
    ));
    blocks.push((BlockPos::new(O + 9, SKY + 1, O + 8), stone));
    blocks.push((
        COMPARATOR,
        Block::Comparator
            .default_state()
            .with_facing(Facing::West)
            .unwrap(),
    ));
    blocks
}

async fn awake_grove() -> (TestWorld, TestDir, RegionHandle, i32) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(CENTRE.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &around()).await;
    set_time(&world, 18000);
    place_blocks(&world, &region, grove()).await;
    region
        .request(player_at(&player, PLAYER, PlayerLooks::default()))
        .await;
    let found = wait_for(&world, &region, 200, |found| {
        found.iter().any(|s| s.kind == EntityType::Creaking)
    })
    .await;
    let creaking = found
        .iter()
        .find(|s| s.kind == EntityType::Creaking)
        .unwrap()
        .id;
    (world, dir, region, creaking)
}

async fn comparator_powered(world: &TestWorld, region: &RegionHandle, want: bool) -> bool {
    for _ in 0..10 {
        if block_at(region, COMPARATOR).await.unwrap().powered() == Some(want) {
            return true;
        }
        world.ticks(1).await;
    }
    false
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_heart_spawns_a_protector_at_night_and_comparators_read_it() {
    let (world, _dir, region, _) = awake_grove().await;
    let heart = block_at(&region, HEART).await.unwrap();
    assert_eq!(heart.get("creaking_heart_state"), Some("awake"));
    assert!(
        comparator_powered(&world, &region, true).await,
        "the comparator reads the nearby protector"
    );

    set_time(&world, 6000);
    wait_for(&world, &region, 60, |found| {
        found.iter().all(|s| s.kind != EntityType::Creaking)
    })
    .await;
    assert!(
        comparator_powered(&world, &region, false).await,
        "no protector, no signal"
    );
    for _ in 0..30 {
        world.ticks(1).await;
    }
    let heart = block_at(&region, HEART).await.unwrap();
    assert_eq!(heart.get("creaking_heart_state"), Some("dormant"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_the_heart_removes_its_protector() {
    let (world, _dir, region, _) = awake_grove().await;
    place_blocks(&world, &region, [(HEART, Block::Air.default_state())]).await;
    wait_for(&world, &region, 40, |found| {
        found.iter().all(|s| s.kind != EntityType::Creaking)
    })
    .await;
    assert!(mobs(&region, EntityType::Creaking).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_heart_without_its_logs_is_uprooted_and_spawns_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(CENTRE.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &around()).await;
    set_time(&world, 18000);
    let blocks: Vec<_> = grove()
        .into_iter()
        .filter(|(pos, _)| *pos != BlockPos::new(O + 8, SKY + 3, O + 8))
        .collect();
    place_blocks(&world, &region, blocks).await;
    region
        .request(player_at(&player, PLAYER, PlayerLooks::default()))
        .await;
    for _ in 0..80 {
        world.ticks(1).await;
    }
    let heart = block_at(&region, HEART).await.unwrap();
    assert_eq!(heart.get("creaking_heart_state"), Some("uprooted"));
    assert!(mobs(&region, EntityType::Creaking).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_protector_shrugs_off_attacks_while_its_heart_stands() {
    let (world, _dir, region, creaking) = awake_grove().await;
    let (attacker, _events) = PlayerHandle::new(1001);
    region
        .request(RegionRequest::AttackEntity {
            attacker: attacker.clone(),
            target: creaking,
            attack: crate::world::entity::EntityAttack {
                attacker: attacker.id,
                attacker_uuid: 78,
                attacker_position: PLAYER,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 1000.0,
                knockback: 0.0,
                weapon: None,
                critical: false,
                magic: false,
                sweep: false,
                part: None,
            },
            sweep: None,
        })
        .await;
    for _ in 0..30 {
        world.ticks(1).await;
    }
    assert_eq!(mobs(&region, EntityType::Creaking).await.len(), 1);
    world.shutdown().await;
}

fn attack(attacker: &PlayerHandle, target: i32) -> RegionRequest {
    RegionRequest::AttackEntity {
        attacker: attacker.clone(),
        target,
        attack: crate::world::entity::EntityAttack {
            attacker: attacker.id,
            attacker_uuid: 77,
            attacker_position: PLAYER,
            attacker_yaw: 0.0,
            attacker_pitch: 0.0,
            damage: 1.0,
            knockback: 0.0,
            weapon: None,
            critical: false,
            magic: false,
            sweep: false,
            part: None,
        },
        sweep: None,
    }
}

const LOGS: [BlockPos; 2] = [
    BlockPos::new(O + 8, SKY + 1, O + 8),
    BlockPos::new(O + 8, SKY + 3, O + 8),
];

async fn resin_faces(region: &RegionHandle) -> usize {
    let mut faces = 0;
    for log in LOGS {
        for (dx, dy, dz) in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            let at = BlockPos::new(log.x + dx, log.y + dy, log.z + dz);
            let Some(state) = block_at(region, at).await else {
                continue;
            };
            if state.block() == Block::ResinClump {
                faces += ["north", "south", "east", "west", "up", "down"]
                    .iter()
                    .filter(|face| state.get(face) == Some("true"))
                    .count();
            }
        }
    }
    faces
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hitting_the_protector_grows_resin_on_the_heart_logs() {
    let (world, _dir, region, creaking) = awake_grove().await;
    assert_eq!(resin_faces(&region).await, 0);
    let (attacker, _events) = PlayerHandle::new(1001);
    region.request(attack(&attacker, creaking)).await;
    world.ticks(2).await;
    let faces = resin_faces(&region).await;
    assert!((2..=3).contains(&faces), "grew {faces} resin faces");
    for _ in 0..12 {
        world.ticks(1).await;
    }
    region.request(attack(&attacker, creaking)).await;
    world.ticks(2).await;
    assert_eq!(
        resin_faces(&region).await,
        faces,
        "the heart calls out once at a time"
    );
    assert_eq!(mobs(&region, EntityType::Creaking).await.len(), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_the_heart_by_hand_kills_the_protector_and_drops_experience() {
    let (world, _dir, region, creaking) = awake_grove().await;
    let (player, _events) = PlayerHandle::new(1000);
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: HEART,
            tool: None,
            mode: crate::world::messages::BreakMode::Finish,
            sequence: 1,
        })
        .await;
    world.ticks(2).await;
    let nbt = entity_nbt(&region, creaking)
        .await
        .expect("the protector lies dying");
    assert_eq!(nbt.get("Health"), Some(&Tag::Float(0.0)));
    let experience: i32 = wait_for(&world, &region, 1, |_| true)
        .await
        .iter()
        .filter(|s| s.kind == EntityType::ExperienceOrb)
        .map(|s| s.experience)
        .sum();
    assert!((20..=24).contains(&experience), "dropped {experience}");
    for _ in 0..30 {
        world.ticks(1).await;
    }
    assert_eq!(
        mobs(&region, EntityType::Creaking).await.len(),
        1,
        "it twitches longer than a plain death"
    );
    wait_for(&world, &region, 30, |found| {
        found.iter().all(|s| s.kind != EntityType::Creaking)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blowing_up_the_heart_kills_the_protector_with_the_blast() {
    let (world, _dir, region, creaking) = awake_grove().await;
    summon(
        &region,
        EntityType::Tnt,
        Compound::new().with("fuse", 1i16),
        [HEART.x as f64 - 0.5, HEART.y as f64, HEART.z as f64 + 0.5],
    )
    .await;
    wait_for(&world, &region, 10, |found| {
        found.iter().all(|s| s.kind != EntityType::Tnt)
    })
    .await;
    world.ticks(2).await;
    assert!(
        tuff_world_model::chunk::is_air(block_at(&region, HEART).await.unwrap()),
        "the blast took the heart"
    );
    let nbt = entity_nbt(&region, creaking)
        .await
        .expect("the protector lies dying instead of vanishing");
    assert_eq!(nbt.get("Health"), Some(&Tag::Float(0.0)));
    let orbs = wait_for(&world, &region, 1, |_| true)
        .await
        .iter()
        .filter(|s| s.kind == EntityType::ExperienceOrb)
        .count();
    assert_eq!(orbs, 0, "nobody lit it, so nobody earns the heart");
    world.shutdown().await;
}

/// A heart just inside region 0 whose protector stands across the border
/// in region 1: it keeps its protector, the protector keeps its heart's
/// protection, and breaking the heart kills it over there.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_protector_across_a_region_border_stays_bound_to_its_heart() {
    use crate::world::messages::BlockOp;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west_chunks: Vec<ChunkPos> = (30..=31)
        .flat_map(|x| (0..=1).map(move |z| ChunkPos::new(x, z)))
        .collect();
    let east_chunks: Vec<ChunkPos> = (32..=33)
        .flat_map(|x| (0..=1).map(move |z| ChunkPos::new(x, z)))
        .collect();
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&west, &viewer, &mut events, &west_chunks).await;
    let (other, mut other_events) = PlayerHandle::new(1001);
    subscribe(&east, &other, &mut other_events, &east_chunks).await;
    set_time(&world, 18000);
    let heart = BlockPos::new(508, SKY + 2, 8);
    let log = Block::PaleOakLog.default_state().with("axis", "y").unwrap();
    let mut floor = Vec::new();
    for x in 496..528 {
        for z in 0..16 {
            floor.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
        }
    }
    let (west_floor, east_floor): (Vec<_>, Vec<_>) =
        floor.into_iter().partition(|(pos, _)| pos.x < 512);
    place_blocks(&world, &west, west_floor).await;
    place_blocks(&world, &east, east_floor).await;
    place_blocks(
        &world,
        &west,
        vec![
            (BlockPos::new(508, SKY + 1, 8), log),
            (BlockPos::new(508, SKY + 3, 8), log),
        ],
    )
    .await;
    let uuid: u128 = 0xc4ea_4119;
    let (reply, answer) = oneshot::channel();
    west.request(RegionRequest::Blocks {
        op: BlockOp::Set {
            changes: vec![(
                heart,
                Block::CreakingHeart
                    .default_state()
                    .with("axis", "y")
                    .unwrap()
                    .with("natural", "true")
                    .unwrap(),
            )],
            destroy: false,
            keep: false,
            strict: true,
            quiet: false,
            data: Some(Compound::new().with("creaking", Tag::uuid(uuid))),
        },
        reply,
    })
    .await;
    answer.await.unwrap();
    for (region, player, position) in [
        (&west, &viewer, [505.5, SKY as f64 + 1.0, 4.5]),
        (&east, &other, [505.5, SKY as f64 + 1.0, 4.5]),
    ] {
        region
            .request(player_at(player, position, PlayerLooks::default()))
            .await;
    }
    summon(
        &east,
        EntityType::Creaking,
        Compound::new()
            .with("UUID", Tag::uuid(uuid))
            .with("NoAI", true)
            .with("home_pos", Tag::IntArray(vec![heart.x, heart.y, heart.z])),
        [514.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    world.ticks(80).await;
    let creaking = mobs(&east, EntityType::Creaking).await;
    assert_eq!(creaking.len(), 1, "the heart kept its far protector");
    let id = creaking[0].id;
    let nbt = entity_nbt(&east, id).await.unwrap();
    assert!(
        nbt.get("Health").and_then(Tag::as_float).unwrap() > 0.0,
        "the heart still names it"
    );
    assert!(
        mobs(&west, EntityType::Creaking).await.is_empty(),
        "no second protector"
    );
    west.request(RegionRequest::Break {
        player: viewer.clone(),
        pos: heart,
        tool: None,
        mode: crate::world::messages::BreakMode::Finish,
        sequence: 1,
    })
    .await;
    world.ticks(4).await;
    let nbt = entity_nbt(&east, id)
        .await
        .expect("the protector lies dying across the border");
    assert_eq!(nbt.get("Health"), Some(&Tag::Float(0.0)));
    world.shutdown().await;
}
