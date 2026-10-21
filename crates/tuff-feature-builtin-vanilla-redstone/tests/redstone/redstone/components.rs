//! Copper bulbs, trapped chests, daylight detectors, lightning rods,
//! rails and tripwire on the redstone bench.

use super::*;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copper_bulb_toggles_on_each_rising_edge() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (bulb, lever) = (at(86, 1, 86), at(87, 1, 86));
    set(
        &b.region,
        &[
            (bulb, "minecraft:copper_bulb"),
            (lever, "minecraft:lever[face=floor,facing=east]"),
        ],
    )
    .await;
    let lit = |s: tuff_vanilla_data::BlockState| (s.lit(), s.powered());
    assert_eq!(
        lit(state(&b.region, bulb).await),
        (Some(false), Some(false))
    );
    click(&mut b, lever, 1).await;
    assert_eq!(lit(state(&b.region, bulb).await), (Some(true), Some(true)));
    click(&mut b, lever, 2).await;
    assert_eq!(
        lit(state(&b.region, bulb).await),
        (Some(true), Some(false)),
        "stays lit when power falls"
    );
    click(&mut b, lever, 3).await;
    assert_eq!(lit(state(&b.region, bulb).await), (Some(false), Some(true)));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_trapped_chest_powers_a_lamp() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (chest, lamp, below, wire) = (at(86, 2, 86), at(87, 2, 86), at(86, 1, 86), at(86, 1, 87));
    set(
        &b.region,
        &[
            (below, "minecraft:stone"),
            (chest, "minecraft:trapped_chest[facing=north]"),
            (lamp, "minecraft:redstone_lamp"),
            (wire, "minecraft:redstone_wire"),
        ],
    )
    .await;
    assert_eq!(state(&b.region, lamp).await.lit(), Some(false));
    b.region
        .request(RegionRequest::Place(place(
            &b.player,
            chest,
            Item::Stick,
            1,
        )))
        .await;
    expect(&mut b.events, container_menu).await;
    b.world.ticks(1).await;
    assert_eq!(state(&b.region, lamp).await.lit(), Some(true));
    assert_eq!(
        state(&b.region, wire).await.power(),
        Some(1),
        "the block below is strongly powered by one viewer"
    );
    b.region
        .request(RegionRequest::CloseContainer {
            player: b.player.id,
            positions: vec![chest],
        })
        .await;
    b.world.ticks(5).await;
    assert_eq!(state(&b.region, lamp).await.lit(), Some(false));
    assert_eq!(state(&b.region, wire).await.power(), Some(0));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_struck_lightning_rod_powers_a_lamp_briefly() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (rod, lamp) = (at(86, 1, 86), at(87, 1, 86));
    set(
        &b.region,
        &[
            (rod, "minecraft:lightning_rod[facing=up]"),
            (lamp, "minecraft:redstone_lamp"),
        ],
    )
    .await;
    summon(
        &b.region,
        tuff_vanilla_data::registry::EntityType::LightningBolt,
        Compound::new(),
        [86.5, f64::from(FLOOR_Y + 2), 86.5],
    )
    .await;
    b.world.ticks(1).await;
    assert_eq!(state(&b.region, rod).await.powered(), Some(true));
    assert_eq!(state(&b.region, lamp).await.lit(), Some(true));
    b.world.ticks(8).await;
    assert_eq!(state(&b.region, rod).await.powered(), Some(false));
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, lamp).await.lit(), Some(false));
    b.world.shutdown().await;
}

fn set_time(world: &TestWorld, ticks: i64) {
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(ticks));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_daylight_detector_follows_the_sun_and_inverts_on_click() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let detector = at(86, 1, 86);
    set(&b.region, &[(detector, "minecraft:daylight_detector")]).await;
    set_time(&b.world, 6000);
    b.world.ticks(20).await;
    assert_eq!(state(&b.region, detector).await.power(), Some(15), "noon");

    set_time(&b.world, 18000);
    b.world.ticks(20).await;
    assert_eq!(
        state(&b.region, detector).await.power(),
        Some(0),
        "midnight"
    );

    click(&mut b, detector, 1).await;
    let inverted = state(&b.region, detector).await;
    assert_eq!(inverted.inverted(), Some(true));
    assert!(
        inverted.power().is_some_and(|power| power > 0),
        "an inverted detector gives power at night at once: {inverted:?}"
    );

    set_time(&b.world, 6000);
    b.world.ticks(20).await;
    assert_eq!(
        state(&b.region, detector).await.power(),
        Some(0),
        "noon inverted"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_powered_rail_passes_power_eight_rails_along() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let rails: Vec<BlockPos> = (82..=92).map(|x| at(x, 1, 86)).collect();
    let lever = at(82, 1, 85);
    let mut blocks: Vec<(BlockPos, &str)> = rails
        .iter()
        .map(|pos| (*pos, "minecraft:powered_rail[shape=east_west]"))
        .collect();
    blocks.push((lever, "minecraft:lever[face=floor,facing=north]"));
    set(&b.region, &blocks).await;
    click(&mut b, lever, 1).await;
    b.world.ticks(1).await;
    let mut powered = Vec::new();
    for pos in &rails {
        powered.push(state(&b.region, *pos).await.powered() == Some(true));
    }
    let expected: Vec<bool> = (0..rails.len()).map(|i| i <= 8).collect();
    assert_eq!(powered, expected, "the lever's rail and eight more");

    click(&mut b, lever, 2).await;
    b.world.ticks(1).await;
    for pos in &rails {
        assert_eq!(state(&b.region, *pos).await.powered(), Some(false));
    }
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_minecart_on_a_detector_rail_lights_a_lamp() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (rail, lamp) = (at(86, 1, 86), at(86, 1, 87));
    set(
        &b.region,
        &[
            (rail, "minecraft:detector_rail[shape=east_west]"),
            (lamp, "minecraft:redstone_lamp"),
        ],
    )
    .await;
    b.world.ticks(2).await;
    assert_eq!(state(&b.region, rail).await.powered(), Some(false));
    summon(
        &b.region,
        tuff_vanilla_data::registry::EntityType::Minecart,
        Compound::new(),
        [86.5, f64::from(FLOOR_Y + 1) + 0.0625, 86.5],
    )
    .await;
    b.world.ticks(2).await;
    assert_eq!(state(&b.region, rail).await.powered(), Some(true));
    assert_eq!(state(&b.region, lamp).await.lit(), Some(true));
    b.world.ticks(40).await;
    assert_eq!(
        state(&b.region, rail).await.powered(),
        Some(true),
        "stays pressed while the cart sits there"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_powered_rail_kicks_a_cart_off_a_wall_and_speeds_it_up() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let mut blocks: Vec<(BlockPos, &str)> = (84..=93)
        .map(|x| (at(x, 1, 86), "minecraft:powered_rail[shape=east_west]"))
        .collect();
    blocks.push((at(83, 1, 86), "minecraft:stone"));
    blocks.push((at(84, 1, 85), "minecraft:redstone_block"));
    set(&b.region, &blocks).await;
    b.world.ticks(1).await;
    assert_eq!(state(&b.region, at(84, 1, 86)).await.powered(), Some(true));
    summon(
        &b.region,
        tuff_vanilla_data::registry::EntityType::Minecart,
        Compound::new(),
        [84.5, f64::from(FLOOR_Y + 1) + 0.0625, 86.5],
    )
    .await;
    b.world.ticks(12).await;
    let cart = mobs(&b.region, tuff_vanilla_data::registry::EntityType::Minecart)
        .await
        .remove(0);
    assert!(
        cart.position[0] > 87.0,
        "the cart rolled east along the powered line: {:?}",
        cart.position
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_on_tripwire_powers_both_hooks() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let (west, east) = (at(84, 1, 86), at(90, 1, 86));
    let lamp = at(82, 1, 86);
    set(
        &b.region,
        &[
            (at(83, 1, 86), "minecraft:stone"),
            (at(91, 1, 86), "minecraft:stone"),
            (lamp, "minecraft:redstone_lamp"),
            (west, "minecraft:tripwire_hook[facing=east]"),
            (east, "minecraft:tripwire_hook[facing=west]"),
        ],
    )
    .await;
    let wires: Vec<(BlockPos, &str)> = (85..90)
        .map(|x| (at(x, 1, 86), "minecraft:tripwire[east=true,west=true]"))
        .collect();
    set(&b.region, &wires).await;
    for hook in [west, east] {
        let hook = state(&b.region, hook).await;
        assert_eq!((hook.attached(), hook.powered()), (Some(true), Some(false)));
    }
    assert_eq!(state(&b.region, at(87, 1, 86)).await.attached(), Some(true));

    b.region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            position: [87.5, f64::from(FLOOR_Y) + 1.3, 86.5],
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    let mut ticks = 0;
    while state(&b.region, west).await.powered() != Some(true) {
        assert!(ticks < 40, "the item trips the wire");
        b.world.ticks(1).await;
        ticks += 1;
    }
    assert_eq!(state(&b.region, east).await.powered(), Some(true));
    assert_eq!(state(&b.region, at(87, 1, 86)).await.powered(), Some(true));
    b.world.ticks(1).await;
    assert_eq!(state(&b.region, lamp).await.lit(), Some(true));

    set(&b.region, &[(at(88, 1, 86), "minecraft:air")]).await;
    b.world.ticks(10).await;
    for hook in [west, east] {
        let hook = state(&b.region, hook).await;
        assert_eq!(
            (hook.attached(), hook.powered()),
            (Some(false), Some(false)),
            "a cut wire detaches the hooks once they recheck"
        );
    }
    b.world.shutdown().await;
}

async fn tripped_span(b: &Bench) -> (BlockPos, BlockPos) {
    let (west, east) = (at(84, 1, 86), at(90, 1, 86));
    set(
        &b.region,
        &[
            (at(83, 1, 86), "minecraft:stone"),
            (at(91, 1, 86), "minecraft:stone"),
            (west, "minecraft:tripwire_hook[facing=east]"),
            (east, "minecraft:tripwire_hook[facing=west]"),
        ],
    )
    .await;
    let wires: Vec<(BlockPos, &str)> = (85..90)
        .map(|x| (at(x, 1, 86), "minecraft:tripwire[east=true,west=true]"))
        .collect();
    set(&b.region, &wires).await;
    b.region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            position: [87.5, f64::from(FLOOR_Y) + 1.3, 86.5],
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    let mut ticks = 0;
    while state(&b.region, west).await.powered() != Some(true) {
        assert!(ticks < 40, "the item trips the wire");
        b.world.ticks(1).await;
        ticks += 1;
    }
    (west, east)
}

async fn break_wire(b: &mut Bench, pos: BlockPos, tool: Option<ItemStack>) {
    b.region
        .request(RegionRequest::Break {
            player: b.player.clone(),
            pos,
            tool,
            mode: BreakMode::Finish,
            sequence: 9,
        })
        .await;
    expect(&mut b.events, |event| match event {
        PlayerEvent::ActionResult { sequence: 9, .. } => Some(()),
        _ => None,
    })
    .await;
    b.world.ticks(1).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_a_tripped_wire_by_hand_keeps_the_hooks_powered() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let (west, _) = tripped_span(&b).await;
    break_wire(&mut b, at(87, 1, 86), None).await;
    assert_eq!(state(&b.region, west).await.powered(), Some(true));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shears_disarm_a_tripwire_without_tripping_the_hooks() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let (west, east) = tripped_span(&b).await;
    let shears = Some(ItemStack::mint(Item::Shears, 1, MintReason::Test));
    break_wire(&mut b, at(87, 1, 86), shears).await;
    assert_eq!(state(&b.region, west).await.powered(), Some(false));
    assert_eq!(state(&b.region, east).await.powered(), Some(false));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tripwire_hook_drops_when_its_wall_goes() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let (wall, hook) = (at(83, 1, 86), at(84, 1, 86));
    set(
        &b.region,
        &[
            (wall, "minecraft:stone"),
            (hook, "minecraft:tripwire_hook[facing=east]"),
        ],
    )
    .await;
    set(&b.region, &[(wall, "minecraft:air")]).await;
    b.world.ticks(2).await;
    assert!(is_air(state(&b.region, hook).await));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_observer_sees_a_barrel_open() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let (barrel, lamp) = (at(85, 1, 86), at(87, 1, 86));
    set(
        &b.region,
        &[
            (barrel, "minecraft:barrel[facing=up]"),
            (at(86, 1, 86), "minecraft:observer[facing=west]"),
            (lamp, "minecraft:redstone_lamp"),
        ],
    )
    .await;
    b.world.ticks(5).await;
    assert_eq!(state(&b.region, lamp).await.lit(), Some(false));
    click(&mut b, barrel, 1).await;
    let mut lit = false;
    for _ in 0..6 {
        b.world.ticks(1).await;
        lit |= state(&b.region, lamp).await.lit() == Some(true);
    }
    assert!(lit, "the observer pulses when the barrel opens");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wind_burst_flips_a_lever_and_presses_a_button() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let (lever, button) = (at(86, 1, 86), at(86, 1, 85));
    set(
        &b.region,
        &[
            (at(85, 1, 86), "minecraft:stone"),
            (at(86, 1, 84), "minecraft:stone"),
            (lever, "minecraft:lever[face=wall,facing=east]"),
            (button, "minecraft:stone_button[face=wall,facing=south]"),
        ],
    )
    .await;
    let nbt = tuff_nbt::Compound::new()
        .with("id", "minecraft:breeze_wind_charge")
        .with(
            "Motion",
            tuff_nbt::Tag::List(vec![
                tuff_nbt::Tag::Double(-0.3),
                tuff_nbt::Tag::Double(0.0),
                tuff_nbt::Tag::Double(0.0),
            ]),
        );
    let (reply, answer) = oneshot::channel();
    b.region
        .request(RegionRequest::Summon {
            nbt,
            position: [88.5, f64::from(FLOOR_Y) + 1.5, 86.5],
            fresh: false,
            reply,
        })
        .await;
    answer.await.unwrap();
    b.world.ticks(12).await;
    assert_eq!(state(&b.region, lever).await.powered(), Some(true));
    assert_eq!(state(&b.region, button).await.powered(), Some(true));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wind_burst_opens_doors_trapdoors_and_gates_but_not_iron() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let (door, iron_door) = (at(86, 1, 86), at(86, 1, 85));
    let (trapdoor, iron_trapdoor, gate) = (at(86, 1, 87), at(86, 1, 84), at(85, 1, 86));
    set(
        &b.region,
        &[
            (door, "minecraft:oak_door[half=lower,facing=east]"),
            (at(86, 2, 86), "minecraft:oak_door[half=upper,facing=east]"),
            (iron_door, "minecraft:iron_door[half=lower,facing=east]"),
            (at(86, 2, 85), "minecraft:iron_door[half=upper,facing=east]"),
            (trapdoor, "minecraft:oak_trapdoor"),
            (iron_trapdoor, "minecraft:iron_trapdoor"),
            (gate, "minecraft:oak_fence_gate"),
        ],
    )
    .await;
    let nbt = tuff_nbt::Compound::new()
        .with("id", "minecraft:breeze_wind_charge")
        .with(
            "Motion",
            tuff_nbt::Tag::List(vec![
                tuff_nbt::Tag::Double(-0.3),
                tuff_nbt::Tag::Double(0.0),
                tuff_nbt::Tag::Double(0.0),
            ]),
        );
    let (reply, answer) = oneshot::channel();
    b.region
        .request(RegionRequest::Summon {
            nbt,
            position: [88.5, f64::from(FLOOR_Y) + 1.5, 86.5],
            fresh: false,
            reply,
        })
        .await;
    answer.await.unwrap();
    b.world.ticks(12).await;
    assert_eq!(state(&b.region, door).await.open(), Some(true));
    assert_eq!(state(&b.region, trapdoor).await.open(), Some(true));
    assert_eq!(state(&b.region, gate).await.open(), Some(true));
    assert_eq!(state(&b.region, iron_door).await.open(), Some(false));
    assert_eq!(state(&b.region, iron_trapdoor).await.open(), Some(false));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_powered_open_waterlogged_trapdoor_ticks_its_water() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let trapdoor = at(86, 1, 86);
    set(
        &b.region,
        &[(
            trapdoor,
            "minecraft:oak_trapdoor[waterlogged=true,half=top]",
        )],
    )
    .await;
    b.world.ticks(20).await;
    let side = at(87, 1, 86);
    let before = state(&b.region, side).await.block();
    set(&b.region, &[(at(86, 1, 85), "minecraft:redstone_block")]).await;
    b.world.ticks(20).await;
    assert!(
        state(&b.region, trapdoor).await.open() == Some(true),
        "the trapdoor opens"
    );
    assert_eq!(before, Block::Air);
    assert_eq!(state(&b.region, side).await.block(), Block::Water);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_players_who_may_build_adjust_a_repeater() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    let repeater = at(86, 1, 86);
    set(&b.region, &[(repeater, "minecraft:repeater[delay=1]")]).await;
    let mut denied = place(&b.player, repeater, Item::Stick, 1);
    denied.item = None;
    denied.held = None;
    denied.may_build = false;
    b.region.request(RegionRequest::Place(denied)).await;
    expect(&mut b.events, |event| match event {
        PlayerEvent::ActionResult { sequence: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(state(&b.region, repeater).await.delay(), Some(1));
    click(&mut b, repeater, 2).await;
    assert_eq!(state(&b.region, repeater).await.delay(), Some(2));
    b.world.shutdown().await;
}
