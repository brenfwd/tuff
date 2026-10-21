//! Riders of vehicles the server moves (minecarts, mounts their client
//! leaves alone) follow them: the session's position, chunk view and the
//! region it asks about its vehicle track the seat, and the anti-cheat
//! sees nothing to set back.

use super::anticheat::{Walker, walker};
use super::*;
use tuff_core::protocol::play::{Movement, Serverbound};
use tuff_item::item::ItemStack;
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, Item};

async fn pump(w: &mut Walker) {
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
}

async fn board(w: &mut Walker, world: &TestWorld, kind: EntityType) -> i32 {
    let region = world.region(RegionPos::new(1, 1)).await;
    let id = mobs(&region, kind).await.first().expect("the vehicle").id;
    region
        .request(RegionRequest::InteractEntity {
            player: w.handle.clone(),
            target: id,
            interaction: interaction(&w.handle, None),
        })
        .await;
    for _ in 0..100 {
        pump(w).await;
        if w.session
            .ride
            .riding
            .as_ref()
            .is_some_and(|r| r.vehicle == id)
        {
            return id;
        }
        world.ticks(1).await;
    }
    panic!("never got on");
}

fn vehicle_at(w: &Walker) -> [f64; 3] {
    w.session.ride.riding.as_ref().expect("riding").position
}

/// What a riding client sends each tick: its facing and the end of the
/// tick, never a position.
async fn client_tick(w: &mut Walker) {
    w.session.tick += 1;
    w.session
        .handle_packet(Serverbound::Move(Movement {
            position: None,
            rotation: Some((-90.0, 0.0)),
            on_ground: false,
            horizontal_collision: false,
        }))
        .await;
    w.session.handle_packet(Serverbound::ClientTickEnd).await;
}

fn corrections(w: &mut Walker) -> usize {
    use tuff_vanilla_data::packet::play::Clientbound;
    let ids = [
        Clientbound::MoveVehicle.id(),
        Clientbound::PlayerPosition.id(),
    ];
    let mut count = 0;
    while let Ok(frame) = w.frames.try_recv() {
        count += usize::from(ids.contains(&framed_packet_id(&frame)));
    }
    count
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_minecart_rider_follows_its_cart_across_a_chunk_border_unflagged() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(40, 40)).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let z = w.z as i32;
    let mut blocks = Vec::new();
    for x in 642..=670 {
        blocks.push((BlockPos::new(x, 100, z), Block::Stone.default_state()));
        blocks.push((
            BlockPos::new(x, 101, z),
            "minecraft:rail[shape=east_west]".parse().unwrap(),
        ));
    }
    place_blocks(&world, &region, blocks).await;
    world.ticks(1).await;
    pump(&mut w).await;
    let start = [650.5, 101.0625, w.z];
    summon(
        &region,
        EntityType::Minecart,
        Compound::new().with(
            "Motion",
            vec![Tag::Double(0.4), Tag::Double(0.0), Tag::Double(0.0)],
        ),
        start,
    )
    .await;
    board(&mut w, &world, EntityType::Minecart).await;
    assert!(!w.session.ride.riding.as_ref().expect("riding").driver);
    corrections(&mut w);

    let mut seen = vec![vehicle_at(&w)[0]];
    for _ in 0..40 {
        world.ticks(1).await;
        pump(&mut w).await;
        client_tick(&mut w).await;
        seen.push(vehicle_at(&w)[0]);
        if vehicle_at(&w)[0] > 659.0 {
            break;
        }
    }
    let cart = mobs(&region, EntityType::Minecart).await[0].position;
    assert_eq!(vehicle_at(&w), cart, "the session knows where the cart is");
    assert!(cart[0] > 657.0, "rolled into the next chunk: {cart:?}");
    let seat = w
        .session
        .seat_position(w.session.ride.riding.as_ref().unwrap());
    assert_eq!(w.session.pose.position, seat);
    assert!(
        seen.windows(2).filter(|pair| pair[1] > pair[0]).count() > 5,
        "followed tick by tick: {seen:?}"
    );
    assert_eq!(w.session.chunk(), ChunkPos::new(41, 40));
    assert_eq!(
        w.session.view.center,
        ChunkPos::new(41, 40),
        "the view moved"
    );
    assert_eq!(corrections(&mut w), 0, "nothing was set back");
    assert!(!w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pig_rider_without_a_lure_does_not_drive_it_and_follows_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(44, 44)).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let (x, z) = (w.x as i32 + 3, w.z as i32);
    let floor: Vec<_> = (-2..=2)
        .flat_map(|dx| (-2..=2).map(move |dz| (dx, dz)))
        .map(|(dx, dz)| {
            (
                BlockPos::new(x + dx, 100, z + dz),
                Block::Stone.default_state(),
            )
        })
        .collect();
    place_blocks(&world, &region, floor).await;
    let nbt = Compound::new().with(
        "equipment",
        Compound::new().with("saddle", ItemStack::new(Item::Saddle, 1).to_nbt()),
    );
    summon(
        &region,
        EntityType::Pig,
        nbt,
        [f64::from(x) + 0.5, 112.0, f64::from(z) + 0.5],
    )
    .await;
    board(&mut w, &world, EntityType::Pig).await;
    let dimension = w.session.dimension;
    w.session
        .view
        .regions
        .insert((dimension, RegionPos::new(1, 1)), region.clone());
    assert!(w.session.ride.riding.as_ref().expect("riding").steers);

    w.session.tick_driving().await;
    for _ in 0..20 {
        world.ticks(1).await;
        pump(&mut w).await;
        if !w.session.ride.riding.as_ref().expect("riding").driver {
            break;
        }
    }
    assert!(
        !w.session.ride.riding.as_ref().expect("riding").driver,
        "no carrot on a stick: the pig is not the rider's to drive"
    );
    let flipped = vehicle_at(&w);
    world.ticks(3).await;
    pump(&mut w).await;
    let pig = mobs(&region, EntityType::Pig).await[0].position;
    assert!(pig[1] < flipped[1], "still falling: {pig:?} {flipped:?}");
    assert_eq!(vehicle_at(&w), pig, "the rider follows the falling pig");

    w.session
        .inventory
        .set_hand(false, Some(ItemStack::new(Item::CarrotOnAStick, 1)));
    w.session.tick_driving().await;
    for _ in 0..20 {
        world.ticks(1).await;
        pump(&mut w).await;
        if w.session.ride.riding.as_ref().expect("riding").driver {
            break;
        }
    }
    assert!(
        w.session.ride.riding.as_ref().expect("riding").driver,
        "a carrot on a stick hands the pig back to the rider"
    );
    world.shutdown().await;
}
