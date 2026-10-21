use super::*;

fn saddled(kind_nbt: Compound) -> Compound {
    kind_nbt.with(
        "equipment",
        Compound::new().with(
            "saddle",
            tuff_item::item::ItemStack::new(tuff_vanilla_data::Item::Saddle, 1).to_nbt(),
        ),
    )
}

async fn ride_displacement(
    kind: EntityType,
    nbt: Compound,
    input: crate::world::entity::RiderInput,
    ticks: usize,
) -> ([f64; 3], bool) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(300);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let start = [6.5, SKY as f64 + 1.0, 6.5];
    summon(&region, kind, nbt, start).await;
    let mount = mobs(&region, kind).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: mount,
            interaction: interaction(&player, None),
        })
        .await;
    let riding = expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    let steers = riding.steers;
    world.ticks(5).await;
    let target = [start[0], start[1], start[2] + 3.0];
    for _ in 0..ticks {
        region
            .request(RegionRequest::Vehicle(VehicleRequest::Drive {
                player: 300,
                vehicle: mount,
                input,
            }))
            .await;
        region
            .request(RegionRequest::Vehicle(VehicleRequest::Move {
                player: 300,
                vehicle: mount,
                position: target,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            }))
            .await;
        world.ticks(1).await;
    }
    let at = mobs(&region, kind).await[0].position;
    world.shutdown().await;
    (
        [at[0] - start[0], at[1] - start[1], at[2] - start[2]],
        steers,
    )
}

fn holding(item: tuff_vanilla_data::Item) -> crate::world::entity::RiderInput {
    crate::world::entity::RiderInput {
        forward: 0.0,
        yaw: 0.0,
        main_hand: Some(item),
        ..Default::default()
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_saddled_pig_runs_ahead_only_for_a_rider_holding_a_carrot_on_a_stick() {
    let (with, steers) = ride_displacement(
        EntityType::Pig,
        saddled(Compound::new()),
        holding(tuff_vanilla_data::Item::CarrotOnAStick),
        30,
    )
    .await;
    assert!(steers, "a saddled pig takes rider input");
    assert!(with[2] > 1.0, "ran along the rider's facing: {with:?}");
    assert!(with[0].abs() < 0.5, "straight ahead: {with:?}");
    let (without, _) = ride_displacement(
        EntityType::Pig,
        saddled(Compound::new()),
        holding(tuff_vanilla_data::Item::Stick),
        30,
    )
    .await;
    assert!(
        without[2] < with[2],
        "no carrot, no steering: {without:?} vs {with:?}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unsaddled_pig_cannot_be_ridden_and_a_saddle_click_fits_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(300);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let pig = mobs(&region, EntityType::Pig).await[0].id;
    let click = |stack| {
        let mut interaction = interaction(&player, Some(stack));
        interaction.sneaking = false;
        interaction
    };
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: pig,
            interaction: click(tuff_vanilla_data::Item::Saddle),
        })
        .await;
    world.ticks(2).await;
    let data = chest_boat_data(&region, pig).await;
    assert!(
        data.get_compound("equipment")
            .is_some_and(|e| e.get("saddle").is_some()),
        "{data:?}"
    );
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: pig,
            interaction: click(tuff_vanilla_data::Item::Carrot),
        })
        .await;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: pig,
            interaction: interaction(&player, None),
        })
        .await;
    let riding = expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    assert_eq!(riding.vehicle, pig, "only the empty-handed click mounted");
    assert_eq!(riding.riders, vec![300]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boost_reaches_a_ridden_pig_once_at_a_time() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(300);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Pig,
        saddled(Compound::new()),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let pig = mobs(&region, EntityType::Pig).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: pig,
            interaction: interaction(&player, None),
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    for _ in 0..2 {
        region
            .request(RegionRequest::Vehicle(VehicleRequest::Boost {
                player: player.clone(),
                vehicle: pig,
            }))
            .await;
    }
    world.ticks(2).await;
    expect(&mut events, |event| match event {
        PlayerEvent::VehicleBoosted { vehicle } if vehicle == pig => Some(()),
        _ => None,
    })
    .await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::VehicleBoosted { .. }),
            "the second spur is refused while the first runs"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_saddled_strider_is_steered_by_a_warped_fungus_on_a_stick() {
    let (moved, steers) = ride_displacement(
        EntityType::Strider,
        saddled(Compound::new()),
        holding(tuff_vanilla_data::Item::WarpedFungusOnAStick),
        30,
    )
    .await;
    assert!(steers);
    assert!(moved[2] > 0.5, "walked ahead: {moved:?}");
}
