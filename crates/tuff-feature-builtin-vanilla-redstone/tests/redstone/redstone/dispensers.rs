//! Dispensers firing rockets, spectral arrows and wind charges, and
//! letting bees out of the hives they harvest.

use super::*;
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType;

/// Ticks one at a time, collecting every entity seen on the way.
async fn watch(b: &Bench, ticks: u32) -> Vec<(EntityType, [f64; 3])> {
    let mut seen = Vec::new();
    for _ in 0..ticks {
        b.world.ticks(1).await;
        seen.extend(entity_kinds(&b.region).await);
    }
    seen
}

async fn entity_nbt(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("entity exists")
}

async fn saved_block_entity(region: &RegionHandle, pos: BlockPos) -> Option<Compound> {
    let BlockOpResult::Copied(copied) = blocks(
        region,
        BlockOp::Copy {
            positions: vec![pos],
        },
    )
    .await
    else {
        panic!("not a copy");
    };
    copied.into_iter().next().flatten().and_then(|(_, nbt)| nbt)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_shoots_a_firework_rocket_straight_out_of_its_face() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::FireworkRocket, 2, MintReason::Test)],
    )
    .await;
    click(&mut b, button, 2).await;
    let seen = watch(&b, 6).await;
    let rockets: Vec<[f64; 3]> = seen
        .iter()
        .filter(|(kind, _)| *kind == EntityType::FireworkRocket)
        .map(|(_, position)| *position)
        .collect();
    assert!(
        rockets.iter().any(|p| p[0] > 87.0),
        "a rocket flies east: {seen:?}"
    );
    let base = f64::from(FLOOR_Y) + 1.5;
    assert!(
        rockets.iter().all(|p| (p[1] - base).abs() < 0.5),
        "a dispensed rocket keeps level instead of climbing: {rockets:?}"
    );
    let left = dispenser_slot(&mut b, dispenser).await;
    assert_eq!(
        left.map(|s| (s.item(), s.count())),
        Some((Item::FireworkRocket, 1))
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_fires_a_spectral_arrow_as_a_spectral_arrow() {
    let dir = TestDir::new();
    let (b, _, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::SpectralArrow, 1, MintReason::Test)],
    )
    .await;
    let mut b = b;
    click(&mut b, button, 2).await;
    let seen = watch(&b, 4).await;
    assert!(
        seen.iter()
            .any(|(kind, _)| *kind == EntityType::SpectralArrow),
        "a spectral arrow flies: {seen:?}"
    );
    assert!(
        !seen.iter().any(|(kind, _)| *kind == EntityType::Arrow),
        "not a plain arrow: {seen:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispensed_wind_charge_bursts_on_a_wall_and_pushes_without_hurting() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::WindCharge, 1, MintReason::Test)],
    )
    .await;
    set(
        &b.region,
        &[
            (at(89, 1, 85), "minecraft:stone"),
            (at(89, 1, 86), "minecraft:stone"),
            (at(89, 2, 86), "minecraft:stone"),
            (at(89, 1, 87), "minecraft:stone"),
        ],
    )
    .await;
    let start = [88.5, f64::from(FLOOR_Y + 1), 87.65];
    super::summon(
        &b.region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        start,
    )
    .await;
    let pig = super::mobs(&b.region, EntityType::Pig).await.remove(0);
    click(&mut b, button, 2).await;
    let seen = watch(&b, 8).await;
    assert!(
        seen.iter().any(|(kind, _)| *kind == EntityType::WindCharge),
        "a wind charge flies: {seen:?}"
    );
    assert!(
        !entity_kinds(&b.region)
            .await
            .iter()
            .any(|(kind, _)| *kind == EntityType::WindCharge),
        "the charge burst"
    );
    let moved = super::mobs(&b.region, EntityType::Pig).await.remove(0);
    assert!(
        moved.position[2] > start[2] + 0.05 || moved.position[1] > start[1] + 0.05,
        "the burst pushed the pig away: {:?} -> {:?}",
        start,
        moved.position
    );
    let nbt = entity_nbt(&b.region, pig.id).await;
    assert_eq!(
        nbt.get("Health"),
        Some(&Tag::Float(10.0)),
        "a burst never hurts"
    );
    assert_eq!(dispenser_slot(&mut b, dispenser).await, None);
    b.world.shutdown().await;
}

fn hive_with_a_bee() -> Compound {
    let bee = Compound::new()
        .with(
            "entity_data",
            Compound::new()
                .with("id", "minecraft:bee")
                .with("Age", -2000)
                .with("UUID", Tag::uuid(7)),
        )
        .with("ticks_in_hive", 500)
        .with("min_ticks_in_hive", 600);
    Compound::new().with("bees", Tag::List(vec![Tag::Compound(bee)]))
}

async fn place_hive(b: &Bench, pos: BlockPos, data: Compound) {
    let changes = vec![(
        pos,
        "minecraft:beehive[facing=south,honey_level=5]"
            .parse()
            .unwrap(),
    )];
    let (reply, answer) = oneshot::channel();
    b.region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes,
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: Some(data),
            },
            reply,
        })
        .await;
    answer.await.unwrap();
}

fn bees_in(saved: Option<Compound>) -> usize {
    saved
        .and_then(|nbt| nbt.get_list("bees").map(<[Tag]>::len))
        .unwrap_or(0)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shears_from_a_dispenser_let_a_full_hives_bees_out_in_front() {
    let dir = TestDir::new();
    let (mut b, _, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::Shears, 1, MintReason::Test)],
    )
    .await;
    let hive = at(86, 1, 86);
    place_hive(&b, hive, hive_with_a_bee()).await;
    assert_eq!(bees_in(saved_block_entity(&b.region, hive).await), 1);
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, hive).await.get("honey_level"), Some("0"));
    let bees = super::mobs(&b.region, EntityType::Bee).await;
    assert_eq!(bees.len(), 1, "the bee came out");
    let bee = &bees[0];
    assert!(
        (bee.position[0] - 86.5).abs() < 0.3 && bee.position[2] > 87.0,
        "the bee leaves through the hive's front: {:?}",
        bee.position
    );
    let nbt = entity_nbt(&b.region, bee.id).await;
    let age = nbt.get_int("Age").expect("a baby's age");
    assert!((-1500..-1490).contains(&age), "it aged inside: {age}");
    assert_eq!(bees_in(saved_block_entity(&b.region, hive).await), 0);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bottled_hive_keeps_its_bees_behind_a_blocked_front() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::GlassBottle, 1, MintReason::Test)],
    )
    .await;
    let hive = at(86, 1, 86);
    place_hive(&b, hive, hive_with_a_bee()).await;
    set(&b.region, &[(at(86, 1, 87), "minecraft:stone")]).await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, hive).await.get("honey_level"), Some("0"));
    let filled = dispenser_slot(&mut b, dispenser).await.unwrap();
    assert_eq!(filled.item(), Item::HoneyBottle);
    assert!(super::mobs(&b.region, EntityType::Bee).await.is_empty());
    assert_eq!(bees_in(saved_block_entity(&b.region, hive).await), 1);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispensed_minecart_keeps_the_name_of_its_item() {
    let dir = TestDir::new();
    let mut stack = ItemStack::mint(Item::Minecart, 1, MintReason::Test);
    stack.components.custom_name = Some(Box::new(tuff_item::text::TextComponent::new("Rusty")));
    let (mut b, _, button) = dispenser_bench(dir.path(), vec![stack]).await;
    set(
        &b.region,
        &[(at(86, 1, 86), "minecraft:rail[shape=east_west]")],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let (reply, answer) = oneshot::channel();
    b.region.request(RegionRequest::Entities { reply }).await;
    let cart = answer
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.kind == EntityType::Minecart)
        .expect("the cart is on the rail");
    let data = entity_nbt(&b.region, cart.id).await;
    assert!(
        format!("{:?}", data.get("CustomName")).contains("Rusty"),
        "{data:?}"
    );
    b.world.shutdown().await;
}
