//! Bees going into hives and coming out with honey, players harvesting a
//! full hive, and hives breaking open.

use super::*;
use crate::world::messages::{BlockOp, BlockOpResult, HandEffect, PlayerLooks};
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType;

async fn outcome(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

const O: i32 = 32;
const HIVE: BlockPos = BlockPos::new(O + 8, SKY + 1, O + 8);
const CENTRE: ChunkPos = ChunkPos::new(2, 2);
const PLAYER: [f64; 3] = [O as f64 + 8.5, SKY as f64 + 1.0, O as f64 + 11.5];

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

fn occupant(nectar: bool, ticks: i32, min: i32) -> Tag {
    Tag::Compound(
        Compound::new()
            .with(
                "entity_data",
                Compound::new()
                    .with("id", "minecraft:bee")
                    .with("HasNectar", nectar)
                    .with("UUID", Tag::uuid(7)),
            )
            .with("ticks_in_hive", ticks)
            .with("min_ticks_in_hive", min),
    )
}

async fn place_hive(region: &RegionHandle, honey: u8, bees: Vec<Tag>) {
    let state = format!("minecraft:beehive[facing=south,honey_level={honey}]");
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes: vec![(HIVE, state.parse().unwrap())],
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: Some(Compound::new().with("bees", Tag::List(bees))),
            },
            reply,
        })
        .await;
    answer.await.unwrap();
}

async fn saved(region: &RegionHandle) -> Option<Compound> {
    let BlockOpResult::Copied(copied) = blocks(
        region,
        BlockOp::Copy {
            positions: vec![HIVE],
        },
    )
    .await
    else {
        panic!("not a copy");
    };
    copied.into_iter().next().flatten().and_then(|(_, nbt)| nbt)
}

async fn bees_inside(region: &RegionHandle) -> usize {
    saved(region)
        .await
        .and_then(|nbt| nbt.get_list("bees").map(<[Tag]>::len))
        .unwrap_or(0)
}

async fn honey(region: &RegionHandle) -> String {
    block_at(region, HIVE)
        .await
        .and_then(|s| s.get("honey_level").map(str::to_owned))
        .unwrap_or_default()
}

async fn grove(
    extra: Vec<(BlockPos, tuff_vanilla_data::BlockState)>,
) -> (
    TestWorld,
    TestDir,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(CENTRE.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &around()).await;
    set_time(&world, 1000);
    let stone = Block::Stone.default_state();
    let mut blocks: Vec<_> = (0..16)
        .flat_map(|x| (0..16).map(move |z| (BlockPos::new(O + x, SKY, O + z), stone)))
        .collect();
    blocks.extend(extra);
    place_blocks(&world, &region, blocks).await;
    region
        .request(player_at(&player, PLAYER, PlayerLooks::default()))
        .await;
    (world, dir, region, player, events)
}

fn used_on_hive(player: &PlayerHandle, item: Item, sequence: i32) -> RegionRequest {
    let mut placing = place(player, HIVE, item, sequence);
    placing.face = tuff_world_model::Face::North;
    RegionRequest::Place(placing)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bee_with_nectar_enters_a_hive_and_leaves_having_raised_the_honey() {
    let (world, _dir, region, _player, _events) = grove(vec![]).await;
    place_hive(&region, 0, vec![]).await;
    let nbt = Compound::new()
        .with("HasNectar", true)
        .with("hive_pos", Tag::IntArray(vec![HIVE.x, HIVE.y, HIVE.z]))
        .with("NoGravity", true);
    summon(
        &region,
        EntityType::Bee,
        nbt,
        [
            HIVE.x as f64 + 0.5,
            HIVE.y as f64 + 1.5,
            HIVE.z as f64 + 1.5,
        ],
    )
    .await;
    for _ in 0..200 {
        if bees_inside(&region).await == 1 {
            break;
        }
        world.ticks(1).await;
    }
    assert_eq!(bees_inside(&region).await, 1, "the bee went in");
    assert!(mobs(&region, EntityType::Bee).await.is_empty());
    let hive = saved(&region).await.unwrap();
    let inside = hive.get_list("bees").unwrap()[0]
        .as_compound()
        .unwrap()
        .clone();
    assert_eq!(inside.get_int("min_ticks_in_hive"), Some(2400));
    assert_eq!(honey(&region).await, "0");

    place_hive(&region, 0, vec![occupant(true, 8, 10)]).await;
    wait_for(&world, &region, 40, |found| {
        found.iter().any(|s| s.kind == EntityType::Bee)
    })
    .await;
    assert_eq!(honey(&region).await, "1");
    assert_eq!(bees_inside(&region).await, 0);
    let bee = mobs(&region, EntityType::Bee).await.remove(0);
    let out = entity_nbt(&region, bee.id).await.unwrap();
    assert_eq!(out.get_bool("HasNectar"), Some(false));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn harvesting_a_full_hive_with_a_bottle_angers_its_bees() {
    let (world, _dir, region, player, mut events) = grove(vec![]).await;
    place_hive(&region, 5, vec![occupant(false, 0, 600)]).await;
    region
        .request(used_on_hive(&player, Item::GlassBottle, 5))
        .await;
    let result = outcome(&mut events, 5).await;
    assert!(
        matches!(
            result,
            ActionOutcome::Used {
                effect: HandEffect::Exchange(Item::HoneyBottle),
                ..
            }
        ),
        "{result:?}"
    );
    assert_eq!(honey(&region).await, "0");
    world.ticks(2).await;
    let bees = mobs(&region, EntityType::Bee).await;
    assert_eq!(bees.len(), 1, "the bee came out");
    assert_eq!(bees_inside(&region).await, 0);
    let nbt = entity_nbt(&region, bees[0].id).await.unwrap();
    assert!(
        nbt.get("angry_at").is_some() && nbt.get("anger_end_time").is_some(),
        "{nbt:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lit_campfire_under_the_hive_keeps_its_bees_calm() {
    let campfire = Block::Campfire.default_state();
    let (world, _dir, region, player, mut events) = grove(vec![]).await;
    place_blocks(
        &world,
        &region,
        [(BlockPos::new(HIVE.x, SKY, HIVE.z), campfire)],
    )
    .await;
    assert_eq!(
        block_at(&region, BlockPos::new(HIVE.x, SKY, HIVE.z))
            .await
            .unwrap()
            .get("lit"),
        Some("true")
    );
    place_hive(&region, 5, vec![occupant(false, 0, 600)]).await;
    region.request(used_on_hive(&player, Item::Shears, 6)).await;
    outcome(&mut events, 6).await;
    world.ticks(2).await;
    assert_eq!(honey(&region).await, "0");
    assert!(mobs(&region, EntityType::Bee).await.is_empty());
    assert_eq!(bees_inside(&region).await, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broken_hive_releases_its_bees_even_at_night() {
    let (world, _dir, region, player, _events) = grove(vec![]).await;
    set_time(&world, 18000);
    place_hive(
        &region,
        2,
        vec![occupant(false, 0, 600), occupant(false, 0, 600)],
    )
    .await;
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: HIVE,
            tool: Some(tuff_item::item::ItemSnapshot::describe(
                Item::DiamondAxe,
                1,
                Default::default(),
            )),
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    world.ticks(2).await;
    assert_eq!(mobs(&region, EntityType::Bee).await.len(), 2);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_next_to_a_hive_drives_its_bees_out() {
    let (world, _dir, region, _player, _events) = grove(vec![]).await;
    set_time(&world, 18000);
    place_hive(&region, 0, vec![occupant(false, 0, 600)]).await;
    assert_eq!(bees_inside(&region).await, 1);
    place_blocks(
        &world,
        &region,
        [(
            BlockPos::new(HIVE.x + 1, HIVE.y, HIVE.z),
            Block::Fire.default_state(),
        )],
    )
    .await;
    wait_for(&world, &region, 60, |found| {
        found.iter().any(|s| s.kind == EntityType::Bee)
    })
    .await;
    assert_eq!(bees_inside(&region).await, 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_set_off_blast_on_a_hive_releases_its_bees() {
    let (world, _dir, region, _player, _events) = grove(vec![]).await;
    set_time(&world, 18000);
    place_hive(&region, 0, vec![occupant(false, 0, 600)]).await;
    region
        .request(RegionRequest::Run(
            crate::world::region_api::RegionTask::new(|world| {
                world.explode(crate::world::explosion::Explosion {
                    dangerous_wither_skull: false,
                    spares_nether_portals: false,
                    center: [
                        HIVE.x as f64 + 0.5,
                        HIVE.y as f64 + 0.5,
                        HIVE.z as f64 + 0.5,
                    ],
                    radius: 2.0,
                    fire: false,
                    blocks: crate::world::explosion::BlockInteraction::Destroy,
                    damage_type: &tuff_vanilla_data::damage_type::EXPLOSION,
                    igniter: Some(crate::world::messages::Igniter {
                        id: 1000,
                        uuid: 1,
                        name: "p".into(),
                    }),
                    culprit: None,
                });
            }),
        ))
        .await;
    world.ticks(3).await;
    assert_eq!(mobs(&region, EntityType::Bee).await.len(), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bee_with_nectar_and_a_hive_grows_the_crop_below_it() {
    let wheat = Block::Wheat.default_state();
    let farmland = Block::Farmland.default_state();
    let mut extra = Vec::new();
    for x in 0..5 {
        for z in 0..5 {
            let (bx, bz) = (O + 2 + x, O + 2 + z);
            extra.push((BlockPos::new(bx, SKY, bz), farmland));
            extra.push((BlockPos::new(bx, SKY + 1, bz), wheat));
        }
    }
    let (world, _dir, region, _player, _events) = grove(extra).await;
    place_hive(&region, 0, vec![]).await;
    summon(
        &region,
        EntityType::Bee,
        Compound::new()
            .with("HasNectar", true)
            .with("hive_pos", Tag::IntArray(vec![HIVE.x, HIVE.y, HIVE.z]))
            .with("CannotEnterHiveTicks", 100000)
            .with("NoGravity", true),
        [O as f64 + 4.5, SKY as f64 + 2.5, O as f64 + 4.5],
    )
    .await;
    let mut grown = false;
    for _ in 0..40 {
        world.ticks(20).await;
        for x in 0..5 {
            for z in 0..5 {
                let at = BlockPos::new(O + 2 + x, SKY + 1, O + 2 + z);
                let age = block_at(&region, at)
                    .await
                    .and_then(|s| s.get("age").map(str::to_owned));
                grown |= age.is_some_and(|a| a != "0");
            }
        }
        if grown {
            break;
        }
    }
    assert!(grown, "a crop below the bee aged");
    world.shutdown().await;
}
