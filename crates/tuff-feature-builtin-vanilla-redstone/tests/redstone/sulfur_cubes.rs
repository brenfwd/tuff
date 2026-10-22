//! Sulfur cubes: swallowing and spitting out blocks, flying when hit while
//! holding one, priming and blowing up with TNT inside, being pushed by
//! players, buckets, and dispensers feeding them.

use super::*;
use crate::world::messages::{BucketUse, HandEffect, PlayerLooks};
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::registry::EntityType;

const FLOOR: f64 = SKY as f64 + 1.0;

async fn pen_world() -> (
    TestDir,
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    (dir, world, region, viewer, events)
}

fn holding(item: Item) -> Compound {
    Compound::new().with("Size", 1).with("NoAI", true).with(
        "equipment",
        Compound::new().with("body", ItemStack::mint(item, 1, MintReason::Test).to_nbt()),
    )
}

fn empty() -> Compound {
    Compound::new().with("Size", 1).with("NoAI", true)
}

async fn cube(region: &RegionHandle) -> i32 {
    mobs(region, EntityType::SulfurCube).await[0].id
}

fn body_item(nbt: &Compound) -> Option<String> {
    let body = nbt.get("equipment")?.as_compound()?.get("body")?;
    ItemStack::mint_from_nbt(body.as_compound()?, MintReason::Test)
        .map(|s| s.item().name().to_owned())
}

fn number(nbt: &Compound, key: &str) -> f64 {
    crate::world::entity::take_number(&mut nbt.clone(), key)
        .unwrap_or_else(|| panic!("no {key} in {nbt:?}"))
}

fn motion(nbt: &Compound) -> [f64; 3] {
    let list = nbt.get("Motion").and_then(Tag::as_list).expect("Motion");
    [0, 1, 2].map(|i| list[i].as_double().unwrap())
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cube_swallows_a_block_from_a_player_and_shears_pop_it_out() {
    let (_dir, world, region, viewer, mut events) = pen_world().await;
    summon(&region, EntityType::SulfurCube, empty(), [8.5, FLOOR, 8.5]).await;
    world.ticks(2).await;
    let id = cube(&region).await;
    let effect = interact(&region, &viewer, &mut events, id, Item::Tnt).await;
    assert!(matches!(effect, HandEffect::None), "{effect:?}");
    let nbt = entity_nbt(&region, id).await.unwrap();
    assert_eq!(body_item(&nbt).as_deref(), Some("minecraft:tnt"));
    let effect = interact(&region, &viewer, &mut events, id, Item::Shears).await;
    assert!(matches!(effect, HandEffect::Damage(1)), "{effect:?}");
    wait_for(&world, &region, 20, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Item && s.name == "minecraft:tnt")
    })
    .await;
    let nbt = entity_nbt(&region, id).await.unwrap();
    assert_eq!(body_item(&nbt), None);
    assert_eq!(number(&nbt, "pickup_timer"), 100.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cube_swallows_a_loose_block_it_touches() {
    let (_dir, world, region, _viewer, _events) = pen_world().await;
    summon(&region, EntityType::SulfurCube, empty(), [8.5, FLOOR, 8.5]).await;
    summon(
        &region,
        EntityType::Item,
        Compound::new()
            .with(
                "Item",
                ItemStack::mint(Item::MagmaBlock, 1, MintReason::Test).to_nbt(),
            )
            .with("PickupDelay", 0i16),
        [8.5, FLOOR, 8.5],
    )
    .await;
    wait_for(&world, &region, 40, |found| {
        !found.iter().any(|s| s.kind == EntityType::Item)
    })
    .await;
    let nbt = entity_nbt(&region, cube(&region).await).await.unwrap();
    assert_eq!(body_item(&nbt).as_deref(), Some("minecraft:magma_block"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_punched_cube_holding_a_block_flies_off_unhurt() {
    let (_dir, world, region, viewer, mut events) = pen_world().await;
    summon(
        &region,
        EntityType::SulfurCube,
        holding(Item::OakPlanks),
        [8.5, FLOOR, 5.5],
    )
    .await;
    world.ticks(3).await;
    let id = cube(&region).await;
    let before = number(&entity_nbt(&region, id).await.unwrap(), "Health");
    region
        .request(RegionRequest::AttackEntity {
            attacker: viewer.clone(),
            target: id,
            attack: attack(4.0, None),
            sweep: None,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::AttackLanded { target, .. } if target == id => Some(()),
        _ => None,
    })
    .await;
    let nbt = entity_nbt(&region, id).await.unwrap();
    assert_eq!(number(&nbt, "Health"), before, "a held block takes the hit");
    let [_, vy, vz] = motion(&nbt);
    assert!(vz > 0.3, "it flies away from the attacker: {vz}");
    assert!(vy > 0.15, "and up: {vy}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lit_cube_holding_tnt_blows_up_and_leaves_no_block_behind() {
    let (_dir, world, region, viewer, mut events) = pen_world().await;
    summon(
        &region,
        EntityType::SulfurCube,
        holding(Item::Tnt),
        [8.5, FLOOR, 8.5],
    )
    .await;
    world.ticks(2).await;
    let id = cube(&region).await;
    let effect = interact(&region, &viewer, &mut events, id, Item::FlintAndSteel).await;
    assert!(matches!(effect, HandEffect::Damage(1)), "{effect:?}");
    let nbt = entity_nbt(&region, id).await.unwrap();
    assert_eq!(number(&nbt, "fuse"), 120.0);
    assert_eq!(nbt.get("Invulnerable"), Some(&Tag::Byte(1)));
    world.ticks(110).await;
    assert_eq!(mobs(&region, EntityType::SulfurCube).await.len(), 1);
    wait_for(&world, &region, 20, |found| {
        !found.iter().any(|s| s.kind == EntityType::SulfurCube)
    })
    .await;
    let floor = block_at(&region, BlockPos::new(8, SKY, 8)).await.unwrap();
    assert!(
        tuff_world_model::chunk::is_air(floor),
        "the blast broke the floor"
    );
    world.ticks(5).await;
    assert!(
        !snapshots(&region)
            .await
            .iter()
            .any(|s| s.kind == EntityType::Item && s.name == "minecraft:tnt"),
        "the swallowed TNT went with the cube"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redstone_power_primes_a_cube_holding_tnt() {
    let (_dir, world, region, _viewer, _events) = pen_world().await;
    summon(
        &region,
        EntityType::SulfurCube,
        holding(Item::Tnt),
        [8.5, FLOOR, 8.5],
    )
    .await;
    world.ticks(3).await;
    let id = cube(&region).await;
    assert_eq!(
        number(&entity_nbt(&region, id).await.unwrap(), "fuse"),
        -1.0
    );
    place_blocks(
        &world,
        &region,
        vec![(
            BlockPos::new(9, SKY + 1, 8),
            Block::RedstoneBlock.default_state(),
        )],
    )
    .await;
    world.ticks(3).await;
    let fuse = number(&entity_nbt(&region, id).await.unwrap(), "fuse");
    assert!((100.0..=120.0).contains(&fuse), "primed: {fuse}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hot_cube_burns_what_it_touches() {
    let (_dir, world, region, _viewer, _events) = pen_world().await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("NoAI", true),
        [8.5, FLOOR, 8.5],
    )
    .await;
    summon(
        &region,
        EntityType::SulfurCube,
        holding(Item::MagmaBlock),
        [8.5, FLOOR, 8.5],
    )
    .await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].id;
    let mut hurt = false;
    for _ in 0..20 {
        world.ticks(1).await;
        if number(&entity_nbt(&region, zombie).await.unwrap(), "Health") < 20.0 {
            hurt = true;
            break;
        }
    }
    assert!(hurt, "the zombie in the hot cube was burned");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_walking_into_a_cube_holding_a_block_shoves_it() {
    let (_dir, world, region, viewer, _events) = pen_world().await;
    summon(
        &region,
        EntityType::SulfurCube,
        holding(Item::BlueIce),
        [8.5, FLOOR, 8.5],
    )
    .await;
    world.ticks(10).await;
    let id = cube(&region).await;
    let start = mobs(&region, EntityType::SulfurCube).await[0].position;
    let walking = PlayerLooks {
        movement: [0.0, 0.0, 0.2],
        ..Default::default()
    };
    region
        .request(player_at(&viewer, [8.5, FLOOR, 7.6], walking))
        .await;
    world.ticks(1).await;
    region
        .request(player_at(
            &viewer,
            [8.5, FLOOR, 2.5],
            PlayerLooks::default(),
        ))
        .await;
    let found = wait_for(&world, &region, 20, |found| {
        found
            .iter()
            .any(|s| s.id == id && s.position[2] > start[2] + 0.05)
    })
    .await;
    let moved = found.iter().find(|s| s.id == id).unwrap().position;
    assert!(
        (moved[0] - start[0]).abs() < 0.05,
        "pushed straight along the walk: {moved:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_bucket_scoops_up_a_cube_with_its_block_and_lets_it_out_again() {
    let (_dir, world, region, viewer, mut events) = pen_world().await;
    summon(
        &region,
        EntityType::SulfurCube,
        holding(Item::Tnt),
        [8.5, FLOOR, 8.5],
    )
    .await;
    world.ticks(2).await;
    let id = cube(&region).await;
    let effect = interact(&region, &viewer, &mut events, id, Item::Bucket).await;
    let HandEffect::ExchangeStack(bucket) = effect else {
        panic!("not scooped: {effect:?}");
    };
    assert_eq!(bucket.item(), Item::SulfurCubeBucket);
    let content = bucket
        .components
        .other
        .get_compound("minecraft:sulfur_cube_content")
        .and_then(|nbt| ItemStack::mint_from_nbt(nbt, MintReason::Test))
        .expect("the bucket keeps the block");
    assert_eq!(content.item(), Item::Tnt);
    wait_for(&world, &region, 5, |found| {
        !found.iter().any(|s| s.kind == EntityType::SulfurCube)
    })
    .await;

    let floor = BlockPos::new(5, SKY, 5);
    let eye = [5.5, SKY as f64 + 2.6, 5.5];
    region
        .request(RegionRequest::UseBucket(BucketUse {
            player: viewer.clone(),
            position: [eye[0], eye[1] - 1.62, eye[2]],
            eye,
            look: crate::world::raycast::view_vector(0.0, 90.0),
            reach: 4.5,
            off_hand: false,
            item: Item::SulfurCubeBucket,
            fluid: None,
            mob: Some(EntityType::SulfurCube),
            mob_data: bucket
                .components
                .other
                .get_compound("minecraft:bucket_entity_data")
                .cloned(),
            mob_components: Some(Box::new(bucket.components.other.clone())),
            sneaking: false,
            creative: false,
            sequence: 3,
            hit: None,
        }))
        .await;
    let found = wait_for(&world, &region, 5, |found| {
        found.iter().any(|s| s.kind == EntityType::SulfurCube)
    })
    .await;
    let released = found
        .iter()
        .find(|s| s.kind == EntityType::SulfurCube)
        .unwrap();
    assert_eq!(
        [released.position[0], released.position[2]],
        [5.5, 5.5],
        "above the floor clicked: {floor:?}"
    );
    let nbt = entity_nbt(&region, released.id).await.unwrap();
    assert_eq!(body_item(&nbt).as_deref(), Some("minecraft:tnt"));
    assert_eq!(nbt.get("from_bucket"), Some(&Tag::Byte(1)));
    world.shutdown().await;
}

async fn dispensing_at_a_cube(
    dir: &TestDir,
    stack: ItemStack,
    cube: Compound,
) -> (super::redstone::Bench, i32) {
    use super::redstone::{at, click, dispenser_bench};
    let (mut b, _, button) = dispenser_bench(dir.path(), vec![stack]).await;
    let front = at(86, 1, 86);
    summon(
        &b.region,
        EntityType::SulfurCube,
        cube,
        [86.5, f64::from(front.y), 86.5],
    )
    .await;
    b.world.ticks(2).await;
    let id = mobs(&b.region, EntityType::SulfurCube).await[0].id;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    (b, id)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_feeds_tnt_to_a_cube_in_front_instead_of_lighting_it() {
    let dir = TestDir::new();
    let (b, id) = dispensing_at_a_cube(
        &dir,
        ItemStack::mint(Item::Tnt, 1, MintReason::Test),
        empty(),
    )
    .await;
    let nbt = entity_nbt(&b.region, id).await.unwrap();
    assert_eq!(body_item(&nbt).as_deref(), Some("minecraft:tnt"));
    assert!(
        !snapshots(&b.region)
            .await
            .iter()
            .any(|s| s.kind == EntityType::Tnt),
        "no primed TNT"
    );
    b.world.shutdown().await;
}

/// A button on the dispenser would power the cube in front through it and
/// prime it before the flint and steel fires, so a redstone block behind
/// the dispenser triggers it instead.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispensed_flint_and_steel_lights_a_cube_holding_tnt() {
    use super::redstone::{at, dispenser_bench, set};
    let dir = TestDir::new();
    let (b, _, _) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::FlintAndSteel, 1, MintReason::Test)],
    )
    .await;
    let front = at(86, 1, 86);
    summon(
        &b.region,
        EntityType::SulfurCube,
        holding(Item::Tnt),
        [86.5, f64::from(front.y), 86.5],
    )
    .await;
    b.world.ticks(2).await;
    let id = mobs(&b.region, EntityType::SulfurCube).await[0].id;
    set(&b.region, &[(at(84, 1, 86), "minecraft:redstone_block")]).await;
    b.world.ticks(4).await;
    let fuse = number(&entity_nbt(&b.region, id).await.unwrap(), "fuse");
    assert!((100.0..=120.0).contains(&fuse), "primed: {fuse}");
    assert!(
        tuff_world_model::chunk::is_air(block_at(&b.region, front).await.unwrap()),
        "no fire was lit under the cube"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_empties_a_sulfur_cube_bucket() {
    use super::redstone::{at, click, dispenser_bench};
    let mut bucket = ItemStack::mint(Item::SulfurCubeBucket, 1, MintReason::Test);
    bucket.components.other.insert(
        "minecraft:sulfur_cube_content",
        Tag::Compound(ItemStack::mint(Item::SoulSand, 1, MintReason::Test).to_nbt()),
    );
    let dir = TestDir::new();
    let (mut b, _, button) = dispenser_bench(dir.path(), vec![bucket]).await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let found = mobs(&b.region, EntityType::SulfurCube).await;
    assert_eq!(found.len(), 1, "the cube is let out");
    assert!(found[0].position[0] > f64::from(at(86, 1, 86).x));
    let nbt = entity_nbt(&b.region, found[0].id).await.unwrap();
    assert_eq!(body_item(&nbt).as_deref(), Some("minecraft:soul_sand"));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_killed_adult_splits_into_two_babies() {
    let (_dir, world, region, viewer, mut events) = pen_world().await;
    summon(&region, EntityType::SulfurCube, empty(), [8.5, FLOOR, 8.5]).await;
    world.ticks(2).await;
    let id = cube(&region).await;
    region
        .request(RegionRequest::AttackEntity {
            attacker: viewer.clone(),
            target: id,
            attack: attack(100.0, None),
            sweep: None,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::AttackLanded { target, .. } if target == id => Some(()),
        _ => None,
    })
    .await;
    let found = wait_for(&world, &region, 40, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::SulfurCube && s.id != id)
            .count()
            == 2
    })
    .await;
    for baby in found
        .iter()
        .filter(|s| s.kind == EntityType::SulfurCube && s.id != id)
    {
        let nbt = entity_nbt(&region, baby.id).await.unwrap();
        assert_eq!(number(&nbt, "Size"), 0.0);
        assert!(number(&nbt, "Age") < 0.0, "a baby: {nbt:?}");
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blast_sets_off_a_nearby_cube_holding_tnt_on_a_short_fuse() {
    let (_dir, world, region, _viewer, _events) = pen_world().await;
    summon(
        &region,
        EntityType::SulfurCube,
        holding(Item::Tnt),
        [10.5, FLOOR, 8.5],
    )
    .await;
    summon(
        &region,
        EntityType::SulfurCube,
        holding(Item::Tnt).with("fuse", 3),
        [6.5, FLOOR, 8.5],
    )
    .await;
    world.ticks(2).await;
    let far = mobs(&region, EntityType::SulfurCube)
        .await
        .into_iter()
        .find(|s| s.position[0] > 9.0)
        .unwrap()
        .id;
    world.ticks(4).await;
    let nbt = entity_nbt(&region, far)
        .await
        .expect("it survived the blast");
    let fuse = number(&nbt, "fuse");
    assert!((5.0..45.0).contains(&fuse), "a chain reaction fuse: {fuse}");
    world.shutdown().await;
}

/// The cube's push along z one tick after a player moving 0.2 a tick
/// along z touched it from 1.1 blocks back, on foot beside a pig or
/// riding it.
async fn pushed_by(riding: bool) -> f64 {
    use crate::world::messages::{RideAnswer, RideOp, RidePassenger};
    let (_dir, world, region, viewer, _events) = pen_world().await;
    summon(
        &region,
        EntityType::SulfurCube,
        holding(Item::BlueIce),
        [8.5, FLOOR, 8.5],
    )
    .await;
    let from = [8.5, FLOOR, 7.4];
    summon(&region, EntityType::Pig, empty(), from).await;
    world.ticks(10).await;
    let id = cube(&region).await;
    if riding {
        let pig = mobs(&region, EntityType::Pig).await[0].id;
        let op = RideOp::Mount {
            passenger: RidePassenger::Player {
                handle: viewer.clone(),
                uuid: 77,
            },
            vehicle: pig,
        };
        let answer = region.ask(move |world| world.ride(op)).await.unwrap();
        assert!(matches!(answer, RideAnswer::Vehicle(_)));
    }
    let moving = PlayerLooks {
        movement: [0.0, 0.0, 0.2],
        ..Default::default()
    };
    let at = if riding {
        [from[0], from[1] + 0.85, from[2]]
    } else {
        from
    };
    region.request(player_at(&viewer, at, moving)).await;
    world.ticks(1).await;
    let z = motion(&entity_nbt(&region, id).await.expect("the cube"))[2];
    world.shutdown().await;
    z
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_riding_player_shoves_a_cube_from_its_vehicle_more_gently() {
    let walking = pushed_by(false).await;
    let riding = pushed_by(true).await;
    assert!(walking > 0.0, "a walker shoves it: {walking}");
    assert!(
        riding > 0.0 && riding < walking * 0.7,
        "a rider shoves it from the pig at the vehicle scale: {riding} vs {walking}"
    );
}
