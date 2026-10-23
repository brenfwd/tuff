//! Rails joining into curves and slopes, dropping off lost support and
//! switching junctions, and minecarts riding them: curves, slopes, powered
//! rails, activator rails, rider keys, pushes, TNT minecarts, detector
//! rail comparator readings and hoppers under several carts.

use super::*;
use crate::world::entity::RiderInput;
use crate::world::messages::VehicleRequest;
use tuff_vanilla_data::block::Shape;
use tuff_vanilla_data::registry::EntityType;

const RIDE: f64 = 0.0625;

/// Places blocks the way a player's placement does, so rails shape
/// themselves to the rails around them.
async fn put(b: &Bench, blocks: &[(BlockPos, &str)]) {
    place_blocks(
        &b.world,
        &b.region,
        blocks
            .iter()
            .map(|(pos, state)| (*pos, state.parse().unwrap())),
    )
    .await;
}

async fn shape(b: &Bench, pos: BlockPos) -> Option<Shape> {
    state(&b.region, pos).await.shape()
}

fn cart_at(x: f64, z: f64) -> [f64; 3] {
    [x, f64::from(FLOOR_Y + 1) + RIDE, z]
}

fn motion(x: f64, z: f64) -> Compound {
    Compound::new().with(
        "Motion",
        vec![Tag::Double(x), Tag::Double(0.0), Tag::Double(z)],
    )
}

async fn only(b: &Bench, kind: EntityType) -> tuff_core::world::messages::EntitySnapshot {
    let mut found = mobs(&b.region, kind).await;
    assert_eq!(found.len(), 1, "{found:#?}");
    found.remove(0)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placed_rails_join_into_curves_and_slopes() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let rail = "minecraft:rail";
    put(&b, &[(at(86, 1, 86), rail)]).await;
    put(&b, &[(at(87, 1, 86), rail)]).await;
    assert_eq!(shape(&b, at(86, 1, 86)).await, Some(Shape::EastWest));
    assert_eq!(shape(&b, at(87, 1, 86)).await, Some(Shape::EastWest));
    put(&b, &[(at(86, 1, 87), rail)]).await;
    assert_eq!(
        shape(&b, at(86, 1, 86)).await,
        Some(Shape::SouthEast),
        "the end rail bends toward the new one"
    );
    assert_eq!(shape(&b, at(86, 1, 87)).await, Some(Shape::NorthSouth));

    put(
        &b,
        &[(at(89, 1, 86), "minecraft:stone"), (at(89, 2, 86), rail)],
    )
    .await;
    put(&b, &[(at(88, 1, 86), "minecraft:powered_rail")]).await;
    assert_eq!(
        shape(&b, at(88, 1, 86)).await,
        Some(Shape::AscendingEast),
        "a rail beside a higher rail climbs to it"
    );
    assert_eq!(shape(&b, at(89, 2, 86)).await, Some(Shape::EastWest));

    put(&b, &[(at(89, 1, 86), "minecraft:air")]).await;
    b.world.ticks(2).await;
    assert!(
        is_air(state(&b.region, at(88, 1, 86)).await),
        "a slope with nothing to lean on drops"
    );
    let dropped = mobs(&b.region, EntityType::Item).await;
    assert!(!dropped.is_empty(), "it drops as an item");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lever_switches_a_rail_junction() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let rail = "minecraft:rail";
    let junction = at(86, 1, 86);
    for pos in [junction, at(85, 1, 86), at(87, 1, 86), at(86, 1, 87)] {
        put(&b, &[(pos, rail)]).await;
    }
    let lever = at(86, 1, 85);
    set(
        &b.region,
        &[(lever, "minecraft:lever[face=floor,facing=north]")],
    )
    .await;
    let before = shape(&b, junction).await;
    click(&mut b, lever, 1).await;
    b.world.ticks(1).await;
    assert_eq!(
        shape(&b, junction).await,
        Some(Shape::SouthWest),
        "powered, the junction turns west (was {before:?})"
    );
    click(&mut b, lever, 2).await;
    b.world.ticks(1).await;
    assert_eq!(shape(&b, junction).await, Some(Shape::SouthEast));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_minecart_rounds_a_curve() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 92)).await;
    for pos in (82..=86)
        .map(|x| at(x, 1, 86))
        .chain((87..=91).map(|z| at(86, 1, z)))
    {
        put(&b, &[(pos, "minecraft:rail")]).await;
    }
    assert_eq!(shape(&b, at(86, 1, 86)).await, Some(Shape::SouthWest));
    summon(
        &b.region,
        EntityType::Minecart,
        motion(0.3, 0.0),
        cart_at(82.5, 86.5),
    )
    .await;
    b.world.ticks(30).await;
    let cart = only(&b, EntityType::Minecart).await;
    assert!(
        (cart.position[0] - 86.5).abs() < 1.0e-6 && cart.position[2] > 87.5,
        "turned south onto the second leg: {:?}",
        cart.position
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_minecart_climbs_over_a_hill_and_rolls_back_down() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let mut blocks: Vec<(BlockPos, &str)> = (86..=87)
        .map(|x| (at(x, 1, 86), "minecraft:stone"))
        .collect();
    blocks.extend((86..=87).map(|x| (at(x, 2, 86), "minecraft:rail")));
    put(&b, &blocks).await;
    for x in (81..=85).chain(88..=93) {
        put(&b, &[(at(x, 1, 86), "minecraft:rail")]).await;
    }
    assert_eq!(shape(&b, at(85, 1, 86)).await, Some(Shape::AscendingEast));
    assert_eq!(shape(&b, at(88, 1, 86)).await, Some(Shape::AscendingWest));
    summon(
        &b.region,
        EntityType::Minecart,
        motion(0.4, 0.0),
        cart_at(81.5, 86.5),
    )
    .await;
    let mut highest = f64::MIN;
    for _ in 0..60 {
        b.world.ticks(1).await;
        highest = highest.max(only(&b, EntityType::Minecart).await.position[1]);
    }
    let cart = only(&b, EntityType::Minecart).await;
    assert!(
        (highest - (f64::from(FLOOR_Y + 2) + RIDE)).abs() < 1.0e-6,
        "rode over the top: {highest}"
    );
    assert!(
        cart.position[0] > 89.0
            && (cart.position[1] - (f64::from(FLOOR_Y + 1) + RIDE)).abs() < 1.0e-6,
        "came down the far side: {:?}",
        cart.position
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unpowered_powered_rail_brakes_a_cart_to_a_stop() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let mut blocks: Vec<(BlockPos, &str)> = (82..=92)
        .map(|x| (at(x, 1, 86), "minecraft:rail[shape=east_west]"))
        .collect();
    blocks[5].1 = "minecraft:powered_rail[shape=east_west]";
    set(&b.region, &blocks).await;
    summon(
        &b.region,
        EntityType::Minecart,
        motion(0.2, 0.0),
        cart_at(82.5, 86.5),
    )
    .await;
    b.world.ticks(60).await;
    let cart = only(&b, EntityType::Minecart).await;
    assert!(
        (87.0..88.0).contains(&cart.position[0]),
        "stopped on the brake: {:?}",
        cart.position
    );
    b.world.ticks(5).await;
    let still = only(&b, EntityType::Minecart).await;
    assert_eq!(still.position, cart.position);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rider_nudges_a_stopped_cart_with_the_movement_keys() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let (rider, mut rider_events) = PlayerHandle::new(300);
    let blocks: Vec<(BlockPos, &str)> = (82..=92)
        .map(|x| (at(x, 1, 86), "minecraft:rail[shape=east_west]"))
        .collect();
    set(&b.region, &blocks).await;
    summon(
        &b.region,
        EntityType::Minecart,
        Compound::new(),
        cart_at(86.5, 86.5),
    )
    .await;
    let cart = only(&b, EntityType::Minecart).await;
    b.region
        .request(RegionRequest::InteractEntity {
            player: rider.clone(),
            target: cart.id,
            interaction: interaction(&rider, None),
        })
        .await;
    let riding = expect(&mut rider_events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    assert!(riding.steers && !riding.driver);
    for _ in 0..20 {
        b.region
            .request(RegionRequest::Vehicle(VehicleRequest::Drive {
                player: rider.id,
                vehicle: cart.id,
                input: RiderInput {
                    forward: 1.0,
                    strafe: 0.0,
                    jumping: false,
                    yaw: -90.0,
                    ..Default::default()
                },
            }))
            .await;
        b.world.ticks(1).await;
    }
    let moved = only(&b, EntityType::Minecart).await;
    assert!(
        moved.position[0] > 86.6,
        "rolled east, the way the rider faces: {:?}",
        moved.position
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_walking_into_a_cart_pushes_it_away() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let blocks: Vec<(BlockPos, &str)> = (82..=92)
        .map(|x| (at(x, 1, 86), "minecraft:rail[shape=east_west]"))
        .collect();
    set(&b.region, &blocks).await;
    summon(
        &b.region,
        EntityType::Minecart,
        Compound::new(),
        cart_at(86.5, 86.5),
    )
    .await;
    let (walker, _walker_events) = PlayerHandle::new(301);
    b.region
        .request(update(&walker, 1, [86.0, f64::from(FLOOR_Y + 1), 86.5]))
        .await;
    b.world.ticks(10).await;
    let cart = only(&b, EntityType::Minecart).await;
    assert!(
        cart.position[0] > 86.6,
        "pushed east, away from the player: {:?}",
        cart.position
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_powered_activator_rail_throws_a_rider_off() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    set(
        &b.region,
        &[
            (at(86, 1, 86), "minecraft:activator_rail[shape=east_west]"),
            (at(86, 1, 85), "minecraft:redstone_block"),
        ],
    )
    .await;
    b.world.ticks(1).await;
    assert_eq!(state(&b.region, at(86, 1, 86)).await.powered(), Some(true));
    summon(
        &b.region,
        EntityType::Minecart,
        Compound::new(),
        cart_at(86.5, 86.5),
    )
    .await;
    let cart = only(&b, EntityType::Minecart).await;
    let (rider, mut rider_events) = PlayerHandle::new(302);
    b.region
        .request(RegionRequest::InteractEntity {
            player: rider.clone(),
            target: cart.id,
            interaction: interaction(&rider, None),
        })
        .await;
    expect(&mut rider_events, |event| match event {
        PlayerEvent::Riding(_) => Some(()),
        _ => None,
    })
    .await;
    b.world.ticks(1).await;
    expect(&mut rider_events, |event| match event {
        PlayerEvent::Dismounted { vehicle, .. } if vehicle == cart.id => Some(()),
        _ => None,
    })
    .await;
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_activator_rail_lights_a_tnt_minecart_whose_blast_spares_the_track() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (82, 90)).await;
    let mut blocks: Vec<(BlockPos, &str)> = (82..=92)
        .map(|x| (at(x, 1, 86), "minecraft:rail[shape=east_west]"))
        .collect();
    blocks[4].1 = "minecraft:activator_rail[shape=east_west]";
    blocks.push((at(86, 1, 85), "minecraft:redstone_block"));
    set(&b.region, &blocks).await;
    b.world.ticks(1).await;
    assert_eq!(state(&b.region, at(86, 1, 86)).await.powered(), Some(true));
    summon(
        &b.region,
        EntityType::TntMinecart,
        Compound::new(),
        cart_at(86.5, 86.5),
    )
    .await;
    b.world.ticks(2).await;
    let lit = only(&b, EntityType::TntMinecart).await;
    let (reply, answer) = oneshot::channel();
    b.region
        .request(RegionRequest::EntityData { id: lit.id, reply })
        .await;
    let saved = answer.await.unwrap().unwrap();
    assert!(
        saved
            .get_int("fuse")
            .is_some_and(|fuse| (70..80).contains(&fuse)),
        "{saved:?}"
    );
    b.world.ticks(85).await;
    assert!(mobs(&b.region, EntityType::TntMinecart).await.is_empty());
    for x in 82..=92 {
        assert!(
            minecart_rail(state(&b.region, at(x, 1, 86)).await),
            "rail at {x} survives"
        );
        assert_eq!(
            state(&b.region, at(x, 0, 86)).await.block(),
            Block::Stone,
            "the floor under the rail at {x} survives"
        );
    }
    let mut blown = 0;
    for x in 84..=89 {
        for z in [84, 85, 87, 88] {
            if is_air(state(&b.region, at(x, 0, z)).await) {
                blown += 1;
            }
        }
    }
    assert!(blown > 0, "the floor beside the track is blown away");
    b.world.shutdown().await;
}

fn minecart_rail(state: tuff_vanilla_data::BlockState) -> bool {
    state.block().kind() == tuff_vanilla_data::BlockKind::Rail
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_comparator_reads_a_chest_minecart_on_a_detector_rail() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 90)).await;
    let (rail, comparator, wire) = (at(86, 1, 86), at(86, 1, 87), at(86, 1, 88));
    set(
        &b.region,
        &[
            (rail, "minecraft:detector_rail[shape=east_west]"),
            (comparator, "minecraft:comparator[facing=north]"),
            (wire, "minecraft:redstone_wire[north=side,south=side]"),
        ],
    )
    .await;
    b.world.ticks(2).await;
    let stacks = Tag::List(
        (0..3)
            .map(|slot| {
                Tag::Compound(
                    tuff_item::item::ItemPicture::new(Item::Stone, 64)
                        .to_nbt()
                        .with("Slot", slot as i8),
                )
            })
            .collect(),
    );
    summon(
        &b.region,
        EntityType::ChestMinecart,
        Compound::new().with("Items", stacks),
        cart_at(86.5, 86.5),
    )
    .await;
    b.world.ticks(6).await;
    assert_eq!(state(&b.region, rail).await.powered(), Some(true));
    assert_eq!(
        state(&b.region, wire).await.power(),
        Some(2),
        "three full stacks of 27 read 2"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hopper_under_two_carts_pulls_from_either_at_random() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let hopper = at(86, 1, 86);
    set(&b.region, &[(hopper, "minecraft:hopper[facing=down]")]).await;
    for item in [Item::Stone, Item::Dirt] {
        summon(
            &b.region,
            EntityType::ChestMinecart,
            Compound::new().with(
                "Items",
                Tag::List(vec![Tag::Compound(
                    tuff_item::item::ItemPicture::new(item, 64)
                        .to_nbt()
                        .with("Slot", 0i8),
                )]),
            ),
            [86.5, f64::from(FLOOR_Y + 2), 86.5],
        )
        .await;
    }
    b.world.ticks(200).await;
    b.region
        .request(RegionRequest::Place(place(
            &b.player,
            hopper,
            Item::Torch,
            1,
        )))
        .await;
    let open = expect(&mut b.events, container_menu).await;
    let MenuSource::Blocks { contents, .. } = open.source else {
        panic!("a hopper is a block container");
    };
    let contents = stacks(&contents);
    let held: Vec<Item> = contents.into_iter().flatten().map(|s| s.item()).collect();
    assert!(
        held.contains(&Item::Stone) && held.contains(&Item::Dirt),
        "pulled from both carts: {held:?}"
    );
    b.world.shutdown().await;
}
