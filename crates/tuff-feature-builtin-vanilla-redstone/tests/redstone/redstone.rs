//! Classic redstone circuits driven through the region actors: an
//! inverter, a repeater chain's exact delay, an observer clock, comparator
//! subtraction, quasi-connectivity, a pressure plate opening a door, TNT
//! primed by a lever and wire across a region border.

use super::*;
use crate::world::messages::{BlockOp, BlockOpResult, CopiedTick};
use tuff_world_model::chunk::is_air;

mod components;
mod crafters;
mod dispensers;
mod experimental_wire;
mod impacts;
mod improved_rails;
mod piston_borders;
mod rails;

const FLOOR_Y: i32 = 200;

pub(super) struct Bench {
    pub(super) world: TestWorld,
    pub(super) region: RegionHandle,
    pub(super) player: PlayerHandle,
    pub(super) events: mpsc::UnboundedReceiver<PlayerEvent>,
}

/// A world with the chunks around `centers` loaded and a stone floor at
/// [`FLOOR_Y`] over the blocks `x`, `z` (inclusive) with air above it.
async fn bench(dir: &Path, centers: &[ChunkPos], x: (i32, i32), z: (i32, i32)) -> Bench {
    bench_in(TestWorld::start(dir), centers, x, z).await
}

/// [`bench`] on a given world.
async fn bench_in(world: TestWorld, centers: &[ChunkPos], x: (i32, i32), z: (i32, i32)) -> Bench {
    let (player, mut events) = PlayerHandle::new(100);
    for center in centers {
        view_around(&world, &player, &mut events, *center).await;
    }
    let region = world.region(centers[0].region()).await;
    let mut by_region: std::collections::HashMap<
        RegionPos,
        Vec<(BlockPos, tuff_vanilla_data::BlockState)>,
    > = Default::default();
    for bx in x.0..=x.1 {
        for bz in z.0..=z.1 {
            let column = by_region
                .entry(ChunkPos::of_block(BlockPos::new(bx, 0, bz)).region())
                .or_default();
            column.push((BlockPos::new(bx, FLOOR_Y, bz), Block::Stone.default_state()));
            for y in 1..=4 {
                column.push((
                    BlockPos::new(bx, FLOOR_Y + y, bz),
                    Block::Air.default_state(),
                ));
            }
        }
    }
    for (pos, changes) in by_region {
        let region = world.region(pos).await;
        set_blocks(&region, changes, true).await;
    }
    Bench {
        world,
        region,
        player,
        events,
    }
}

/// Sets blocks as `/fill` does: all at once, then (unless `strict`) their
/// neighbours are told.
async fn set_blocks(
    region: &RegionHandle,
    changes: Vec<(BlockPos, tuff_vanilla_data::BlockState)>,
    strict: bool,
) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes,
                destroy: false,
                keep: false,
                strict,
                quiet: false,
                data: None,
            },
            reply,
        })
        .await;
    answer.await.unwrap();
}

pub(super) async fn set(region: &RegionHandle, blocks: &[(BlockPos, &str)]) {
    let changes = blocks
        .iter()
        .map(|(pos, state)| (*pos, state.parse().unwrap()))
        .collect();
    set_blocks(region, changes, false).await;
}

async fn state(region: &RegionHandle, pos: BlockPos) -> tuff_vanilla_data::BlockState {
    block_at(region, pos).await.unwrap()
}

pub(super) fn at(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, FLOOR_Y + y, z)
}

/// Right-clicks the block at `pos` with an empty hand.
pub(super) async fn click(bench: &mut Bench, pos: BlockPos, sequence: i32) {
    let mut click = place(&bench.player, pos, Item::Stick, sequence);
    click.item = None;
    click.held = None;
    bench.region.request(RegionRequest::Place(click)).await;
    expect(&mut bench.events, |event| match event {
        PlayerEvent::ActionResult { sequence: s, .. } if s == sequence => Some(()),
        _ => None,
    })
    .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_torch_inverts_a_lever_two_ticks_later() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (stone, torch, lever) = (at(86, 1, 86), at(86, 2, 86), at(87, 1, 86));
    set(
        &b.region,
        &[
            (stone, "minecraft:stone"),
            (torch, "minecraft:redstone_torch"),
            (lever, "minecraft:lever[face=wall,facing=east]"),
        ],
    )
    .await;
    assert_eq!(state(&b.region, torch).await.lit(), Some(true));

    click(&mut b, lever, 1).await;
    assert_eq!(state(&b.region, lever).await.powered(), Some(true));
    b.world.ticks(1).await;
    assert_eq!(
        state(&b.region, torch).await.lit(),
        Some(true),
        "not after one tick"
    );
    b.world.ticks(1).await;
    assert_eq!(
        state(&b.region, torch).await.lit(),
        Some(false),
        "off after two"
    );

    click(&mut b, lever, 2).await;
    b.world.ticks(2).await;
    assert_eq!(state(&b.region, torch).await.lit(), Some(true), "back on");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zero_chained_update_limit_drops_the_neighbour_updates() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (stone, torch, lever) = (at(86, 1, 86), at(86, 2, 86), at(87, 1, 86));
    set(
        &b.region,
        &[
            (stone, "minecraft:stone"),
            (torch, "minecraft:redstone_torch"),
            (lever, "minecraft:lever[face=wall,facing=east]"),
        ],
    )
    .await;
    b.world.shared.live.set_max_chained_neighbor_updates(0);
    click(&mut b, lever, 1).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, lever).await.powered(), Some(true));
    assert_eq!(state(&b.region, torch).await.lit(), Some(true));
    b.world.shared.live.set_max_chained_neighbor_updates(-1);
    click(&mut b, lever, 2).await;
    click(&mut b, lever, 3).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, torch).await.lit(), Some(false));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repeater_chain_delays_by_the_sum_of_its_delays() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 90), (84, 88)).await;
    let (source, first, second, lamp) =
        (at(83, 1, 86), at(84, 1, 86), at(85, 1, 86), at(86, 1, 86));
    set(
        &b.region,
        &[
            (first, "minecraft:repeater[facing=west,delay=1]"),
            (second, "minecraft:repeater[facing=west,delay=4]"),
            (lamp, "minecraft:redstone_lamp"),
        ],
    )
    .await;
    set(&b.region, &[(source, "minecraft:redstone_block")]).await;
    for tick in 1..=10 {
        b.world.ticks(1).await;
        let first_on = state(&b.region, first).await.powered() == Some(true);
        let second_on = state(&b.region, second).await.powered() == Some(true);
        let lit = state(&b.region, lamp).await.lit() == Some(true);
        assert_eq!(first_on, tick >= 2, "first repeater at tick {tick}");
        assert_eq!(second_on, tick >= 10, "second repeater at tick {tick}");
        assert_eq!(lit, tick >= 10, "lamp at tick {tick}");
    }

    set(&b.region, &[(source, "minecraft:air")]).await;
    b.world.ticks(10).await;
    assert_eq!(state(&b.region, second).await.powered(), Some(false));
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, lamp).await.lit(),
        Some(false),
        "a lamp turns off four ticks after losing power"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn facing_observers_make_a_clock() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (east, west, lamp) = (at(86, 1, 86), at(87, 1, 86), at(85, 1, 86));
    set(
        &b.region,
        &[
            (lamp, "minecraft:redstone_lamp"),
            (east, "minecraft:observer[facing=east]"),
        ],
    )
    .await;
    set(&b.region, &[(west, "minecraft:observer[facing=west]")]).await;
    let mut rising = Vec::new();
    let mut was_on = false;
    for tick in 1..=40 {
        b.world.ticks(1).await;
        let on = state(&b.region, east).await.powered() == Some(true);
        if on && !was_on {
            rising.push(tick);
        }
        was_on = on;
    }
    assert_eq!(
        rising,
        [2, 8, 14, 20, 26, 32, 38],
        "each observer pulses for two ticks, then waits on the other"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subtracting_comparator_outputs_the_difference() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 90), (80, 90)).await;
    let comparator = at(86, 1, 86);
    let output = at(86, 1, 85);
    set(
        &b.region,
        &[
            (
                comparator,
                "minecraft:comparator[facing=south,mode=subtract]",
            ),
            (output, "minecraft:redstone_wire"),
            (at(85, 1, 86), "minecraft:redstone_wire"),
            (at(84, 1, 86), "minecraft:redstone_wire"),
            (at(83, 1, 86), "minecraft:redstone_wire"),
            (at(82, 1, 86), "minecraft:redstone_block"),
            (at(86, 1, 87), "minecraft:redstone_block"),
        ],
    )
    .await;
    b.world.ticks(6).await;
    assert_eq!(state(&b.region, at(85, 1, 86)).await.power(), Some(13));
    assert_eq!(state(&b.region, comparator).await.powered(), Some(true));
    assert_eq!(
        state(&b.region, output).await.power(),
        Some(2),
        "15 at the back minus 13 at the side"
    );

    set(
        &b.region,
        &[(
            comparator,
            "minecraft:comparator[facing=south,mode=compare]",
        )],
    )
    .await;
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, output).await.power(),
        Some(15),
        "compare passes the back"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropper_is_powered_through_the_block_above_it_only_once_updated() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let dropper = at(86, 1, 86);
    set(&b.region, &[(dropper, "minecraft:dropper[facing=up]")]).await;
    set(&b.region, &[(at(87, 2, 86), "minecraft:redstone_block")]).await;
    b.world.ticks(2).await;
    assert_eq!(
        state(&b.region, dropper).await.triggered(),
        Some(false),
        "power next to the space above does not update the dropper"
    );
    set(&b.region, &[(at(85, 1, 86), "minecraft:stone")]).await;
    assert_eq!(
        state(&b.region, dropper).await.triggered(),
        Some(true),
        "any update makes it notice"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_on_a_pressure_plate_opens_a_door() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (plate, door) = (at(86, 1, 86), at(87, 1, 86));
    set(
        &b.region,
        &[
            (plate, "minecraft:oak_pressure_plate"),
            (door, "minecraft:iron_door[half=lower,facing=east]"),
            (at(87, 2, 86), "minecraft:iron_door[half=upper,facing=east]"),
        ],
    )
    .await;
    assert_eq!(state(&b.region, door).await.open(), Some(false));
    b.region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            position: [86.5, f64::from(FLOOR_Y) + 1.3, 86.5],
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    let mut ticks = 0;
    while state(&b.region, plate).await.powered() != Some(true) {
        if ticks == 39 {
            let (reply, answer) = oneshot::channel();
            b.region.request(RegionRequest::Entities { reply }).await;
            panic!("the item lands on the plate: {:?}", answer.await.unwrap());
        }
        b.world.ticks(1).await;
        ticks += 1;
    }
    assert_eq!(state(&b.region, door).await.open(), Some(true));
    assert_eq!(
        state(&b.region, at(87, 2, 86)).await.open(),
        Some(true),
        "both halves open"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lever_primes_tnt() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (tnt, lever) = (at(86, 1, 86), at(87, 1, 86));
    set(
        &b.region,
        &[
            (tnt, "minecraft:tnt"),
            (lever, "minecraft:lever[face=floor,facing=east]"),
        ],
    )
    .await;
    click(&mut b, lever, 1).await;
    assert!(is_air(state(&b.region, tnt).await), "the TNT block is gone");
    let (reply, answer) = oneshot::channel();
    b.region.request(RegionRequest::Entities { reply }).await;
    let entities = answer.await.unwrap();
    assert!(
        entities
            .iter()
            .any(|e| e.kind == tuff_vanilla_data::registry::EntityType::Tnt),
        "primed TNT took its place"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_carries_power_across_a_region_border() {
    let dir = TestDir::new();
    let b = bench(
        dir.path(),
        &[ChunkPos::new(31, 5), ChunkPos::new(32, 5)],
        (506, 518),
        (84, 88),
    )
    .await;
    let east = b.world.region(ChunkPos::new(32, 0).region()).await;
    let wires: Vec<BlockPos> = (509..=515).map(|x| at(x, 1, 86)).collect();
    for pos in &wires {
        let region = if pos.x < 512 { &b.region } else { &east };
        set(region, &[(*pos, "minecraft:redstone_wire")]).await;
    }
    set(&east, &[(at(516, 1, 86), "minecraft:redstone_lamp")]).await;
    set(&b.region, &[(at(508, 1, 86), "minecraft:redstone_block")]).await;
    let last = *wires.last().unwrap();
    let mut ticks = 0;
    while state(&east, last).await.power() != Some(9) {
        assert!(ticks < 20, "power reaches the far end");
        b.world.ticks(1).await;
        ticks += 1;
    }
    assert_eq!(state(&b.region, at(511, 1, 86)).await.power(), Some(13));
    assert_eq!(state(&east, at(512, 1, 86)).await.power(), Some(12));
    assert_eq!(state(&east, at(516, 1, 86)).await.lit(), Some(true));

    set(&b.region, &[(at(508, 1, 86), "minecraft:air")]).await;
    let mut ticks = 0;
    while state(&east, last).await.power() != Some(0) {
        assert!(ticks < 20, "the wire depowers across the border");
        b.world.ticks(1).await;
        ticks += 1;
    }
    b.world.shutdown().await;
}

/// Opens the container at `pos` for the bench's player and puts `stacks`
/// into its first slots.
async fn fill(b: &mut Bench, pos: BlockPos, stacks: Vec<ItemStack>, sequence: i32) {
    b.region
        .request(RegionRequest::Place(place(
            &b.player,
            pos,
            Item::Stick,
            sequence,
        )))
        .await;
    expect(&mut b.events, container_menu).await;
    let changes = stacks
        .into_iter()
        .enumerate()
        .map(|(slot, stack)| ContainerChange {
            pos,
            slot,
            expected: None,
            new: Some(stack),
        })
        .collect();
    b.region
        .request(RegionRequest::ContainerTransaction {
            player: b.player.clone(),
            window_id: 1,
            changes,
            took_result: false,
        })
        .await;
    assert!(expect(&mut b.events, transaction_answer).await.0);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropper_feeds_a_chest_that_a_comparator_reads() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 90), (84, 88)).await;
    let (dropper, chest, comparator, output) =
        (at(84, 1, 86), at(85, 1, 86), at(86, 1, 86), at(87, 1, 86));
    let lever = at(84, 1, 87);
    set(
        &b.region,
        &[
            (dropper, "minecraft:dropper[facing=east]"),
            (chest, "minecraft:chest[facing=north]"),
            (comparator, "minecraft:comparator[facing=west]"),
            (output, "minecraft:redstone_wire"),
            (lever, "minecraft:lever[face=floor,facing=south]"),
        ],
    )
    .await;
    fill(
        &mut b,
        dropper,
        vec![ItemStack::mint(Item::Cobblestone, 64, MintReason::Test)],
        1,
    )
    .await;
    click(&mut b, lever, 2).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, dropper).await.triggered(), Some(true));
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, output).await.power(),
        Some(1),
        "one item in a chest reads as 1"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_drops_items_and_primes_tnt() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 90), (84, 88)).await;
    let (dispenser, button) = (at(85, 1, 86), at(85, 2, 86));
    set(
        &b.region,
        &[
            (dispenser, "minecraft:dispenser[facing=east]"),
            (button, "minecraft:stone_button[face=floor,facing=north]"),
        ],
    )
    .await;
    fill(
        &mut b,
        dispenser,
        vec![ItemStack::mint(Item::Tnt, 1, MintReason::Test)],
        1,
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let (reply, answer) = oneshot::channel();
    b.region.request(RegionRequest::Entities { reply }).await;
    let entities = answer.await.unwrap();
    assert!(
        entities
            .iter()
            .any(|e| e.kind == tuff_vanilla_data::registry::EntityType::Tnt && e.position[0] > 86.0),
        "primed TNT in front of the dispenser: {entities:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn power_locks_a_hopper() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let hopper = at(86, 1, 86);
    set(&b.region, &[(hopper, "minecraft:hopper")]).await;
    assert_eq!(state(&b.region, hopper).await.enabled(), Some(true));
    set(&b.region, &[(at(87, 1, 86), "minecraft:redstone_block")]).await;
    assert_eq!(state(&b.region, hopper).await.enabled(), Some(false));
    set(&b.region, &[(at(87, 1, 86), "minecraft:air")]).await;
    assert_eq!(state(&b.region, hopper).await.enabled(), Some(true));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_torch_on_its_own_output_burns_out() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (84, 88), (84, 88)).await;
    let (base, torch) = (at(85, 1, 86), at(86, 1, 86));
    set(
        &b.region,
        &[
            (base, "minecraft:stone"),
            (at(86, 2, 86), "minecraft:stone"),
            (at(85, 2, 86), "minecraft:redstone_wire"),
        ],
    )
    .await;
    set(
        &b.region,
        &[(torch, "minecraft:redstone_wall_torch[facing=east]")],
    )
    .await;
    let mut toggles = 0;
    let mut was = true;
    for _ in 0..60 {
        b.world.ticks(1).await;
        let lit = state(&b.region, torch).await.lit() == Some(true);
        if lit != was {
            toggles += 1;
        }
        was = lit;
    }
    assert!(!was, "the torch ends burnt out");
    assert_eq!(toggles, 15, "eight turns off, seven back on");
    b.world.ticks(200).await;
    assert_eq!(
        state(&b.region, torch).await.lit(),
        Some(false),
        "it relights after the restart delay only to burn out again"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_arrow_in_a_target_lights_a_lamp_for_a_second() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 90), (80, 90)).await;
    let (target, lamp) = (at(86, 1, 86), at(87, 1, 86));
    set(
        &b.region,
        &[
            (target, "minecraft:target"),
            (lamp, "minecraft:redstone_lamp"),
        ],
    )
    .await;
    let arrow = tuff_feature_builtin_vanilla_combat::projectile::arrow::Arrow::new(
        b.world.shared.next_entity_id(),
        [86.5, f64::from(FLOOR_Y) + 1.55, 83.5],
        [0.0, 0.0, 1.5],
        Default::default(),
    );
    b.region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    let mut ticks = 0;
    while state(&b.region, target).await.power() == Some(0) {
        assert!(ticks < 20, "the arrow reaches the target");
        b.world.ticks(1).await;
        ticks += 1;
    }
    let power = state(&b.region, target).await.power().unwrap();
    assert!(power >= 13, "a hit near the centre is strong: {power}");
    assert_eq!(state(&b.region, lamp).await.lit(), Some(true));
    b.world.ticks(20).await;
    assert_eq!(state(&b.region, target).await.power(), Some(0));
    b.world.shutdown().await;
}

/// A piston facing east at `(84, 1, 86)` in a bench with room east of it.
async fn piston_bench(dir: &Path) -> Bench {
    bench(dir, &[ChunkPos::new(5, 5)], (82, 101), (84, 88)).await
}

async fn blocks_named(b: &Bench, positions: impl IntoIterator<Item = BlockPos>) -> Vec<Block> {
    let mut blocks = Vec::new();
    for pos in positions {
        blocks.push(state(&b.region, pos).await.block());
    }
    blocks
}

async fn item_count(region: &RegionHandle) -> usize {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .iter()
        .filter(|e| e.kind == tuff_vanilla_data::registry::EntityType::Item)
        .count()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_piston_pushes_a_line_of_blocks_in_two_ticks() {
    let dir = TestDir::new();
    let mut b = piston_bench(dir.path()).await;
    let piston = at(84, 1, 86);
    let mut blocks = vec![(piston, "minecraft:piston[facing=east]")];
    for x in 85..=88 {
        blocks.push((at(x, 1, 86), "minecraft:stone"));
    }
    set(&b.region, &blocks).await;
    while b.events.try_recv().is_ok() {}
    set(&b.region, &[(at(84, 1, 87), "minecraft:redstone_block")]).await;

    b.world.ticks(1).await;
    assert_eq!(state(&b.region, piston).await.extended(), Some(true));
    assert_eq!(
        blocks_named(&b, (85..=89).map(|x| at(x, 1, 86))).await,
        vec![Block::MovingPiston; 5],
        "every block is on its way"
    );
    let block_event = tuff_vanilla_data::packet::play::Clientbound::BlockEvent.id();
    let mut told = false;
    while let Ok(event) = b.events.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event
            && packets.iter().any(|p| framed_packet_id(p) == block_event)
        {
            told = true;
        }
    }
    assert!(told, "viewers got the Block Action that animates the push");

    b.world.ticks(1).await;
    assert_eq!(
        state(&b.region, at(89, 1, 86)).await.block(),
        Block::MovingPiston
    );
    b.world.ticks(1).await;
    assert_eq!(
        state(&b.region, at(85, 1, 86)).await.block(),
        Block::PistonHead
    );
    assert_eq!(
        blocks_named(&b, (86..=89).map(|x| at(x, 1, 86))).await,
        vec![Block::Stone; 4],
        "the line moved one block after two ticks"
    );
    b.world.shutdown().await;
}

async fn push_line(length: i32) -> (bool, Vec<Block>) {
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    let mut blocks = vec![(at(84, 1, 86), "minecraft:piston[facing=east]")];
    for x in 85..85 + length {
        blocks.push((at(x, 1, 86), "minecraft:stone"));
    }
    set(&b.region, &blocks).await;
    set(&b.region, &[(at(84, 1, 87), "minecraft:redstone_block")]).await;
    b.world.ticks(4).await;
    let extended = state(&b.region, at(84, 1, 86)).await.extended() == Some(true);
    let line = blocks_named(&b, (85..=86 + length).map(|x| at(x, 1, 86))).await;
    b.world.shutdown().await;
    (extended, line)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_piston_pushes_twelve_blocks_but_not_thirteen() {
    let (extended, line) = push_line(12).await;
    assert!(extended, "twelve blocks move");
    assert_eq!(line[0], Block::PistonHead);
    assert!(line[1..=12].iter().all(|b| *b == Block::Stone), "{line:?}");
    let (extended, line) = push_line(13).await;
    assert!(!extended, "thirteen blocks are too many");
    assert!(line[..13].iter().all(|b| *b == Block::Stone), "{line:?}");
    assert_eq!(line[13], Block::Air);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_piston_cannot_move_obsidian_or_an_extended_piston() {
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(84, 1, 86), "minecraft:piston[facing=east]"),
            (at(85, 1, 86), "minecraft:stone"),
            (at(86, 1, 86), "minecraft:obsidian"),
            (at(84, 1, 88), "minecraft:piston[facing=east]"),
            (at(85, 1, 88), "minecraft:piston[facing=up,extended=true]"),
            (at(85, 2, 88), "minecraft:piston_head[facing=up]"),
            (at(85, 1, 89), "minecraft:redstone_block"),
        ],
    )
    .await;
    set(
        &b.region,
        &[
            (at(84, 1, 87), "minecraft:redstone_block"),
            (at(83, 1, 88), "minecraft:redstone_block"),
        ],
    )
    .await;
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, at(84, 1, 86)).await.extended(),
        Some(false)
    );
    assert_eq!(state(&b.region, at(85, 1, 86)).await.block(), Block::Stone);
    assert_eq!(
        state(&b.region, at(84, 1, 88)).await.extended(),
        Some(false)
    );
    assert_eq!(
        state(&b.region, at(85, 2, 88)).await.block(),
        Block::PistonHead
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pushed_torch_breaks_and_drops() {
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(84, 1, 86), "minecraft:piston[facing=east]"),
            (at(85, 1, 86), "minecraft:stone"),
            (at(86, 1, 86), "minecraft:torch"),
        ],
    )
    .await;
    set(&b.region, &[(at(84, 1, 87), "minecraft:redstone_block")]).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, at(86, 1, 86)).await.block(), Block::Stone);
    assert_eq!(item_count(&b.region).await, 1, "the torch dropped");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sticky_piston_pulls_its_block_back() {
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    let power = at(84, 1, 87);
    set(
        &b.region,
        &[
            (at(84, 1, 86), "minecraft:sticky_piston[facing=east]"),
            (at(85, 1, 86), "minecraft:stone"),
        ],
    )
    .await;
    set(&b.region, &[(power, "minecraft:redstone_block")]).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, at(86, 1, 86)).await.block(), Block::Stone);
    set(&b.region, &[(power, "minecraft:air")]).await;
    b.world.ticks(4).await;
    assert_eq!(
        blocks_named(&b, (84..=86).map(|x| at(x, 1, 86))).await,
        vec![Block::StickyPiston, Block::Stone, Block::Air]
    );
    assert_eq!(
        state(&b.region, at(84, 1, 86)).await.extended(),
        Some(false)
    );

    set(
        &b.region,
        &[
            (at(84, 1, 86), "minecraft:piston[facing=east]"),
            (at(85, 1, 86), "minecraft:air"),
            (at(86, 1, 86), "minecraft:air"),
        ],
    )
    .await;
    set(&b.region, &[(at(85, 1, 86), "minecraft:stone")]).await;
    set(&b.region, &[(power, "minecraft:redstone_block")]).await;
    b.world.ticks(4).await;
    set(&b.region, &[(power, "minecraft:air")]).await;
    b.world.ticks(4).await;
    assert_eq!(
        blocks_named(&b, (84..=86).map(|x| at(x, 1, 86))).await,
        vec![Block::Piston, Block::Air, Block::Stone],
        "a plain piston leaves its block"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slime_block_drags_its_neighbours_but_not_honey() {
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(84, 2, 86), "minecraft:piston[facing=east]"),
            (at(85, 2, 86), "minecraft:slime_block"),
            (at(85, 3, 86), "minecraft:stone"),
            (at(85, 2, 85), "minecraft:honey_block"),
        ],
    )
    .await;
    set(&b.region, &[(at(84, 2, 87), "minecraft:redstone_block")]).await;
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, at(86, 2, 86)).await.block(),
        Block::SlimeBlock
    );
    assert_eq!(state(&b.region, at(86, 3, 86)).await.block(), Block::Stone);
    assert_eq!(state(&b.region, at(85, 3, 86)).await.block(), Block::Air);
    assert_eq!(
        state(&b.region, at(85, 2, 85)).await.block(),
        Block::HoneyBlock,
        "honey does not stick to slime"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quasi_connectivity_powers_a_piston_from_above() {
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(84, 1, 86), "minecraft:piston[facing=east]"),
            (at(84, 2, 86), "minecraft:glass"),
        ],
    )
    .await;
    set(&b.region, &[(at(84, 3, 86), "minecraft:redstone_block")]).await;
    b.world.ticks(3).await;
    assert_eq!(
        state(&b.region, at(84, 1, 86)).await.extended(),
        Some(false),
        "the redstone block above the space above does not update the piston"
    );
    set(&b.region, &[(at(84, 1, 85), "minecraft:stone")]).await;
    b.world.ticks(3).await;
    assert_eq!(
        state(&b.region, at(84, 1, 86)).await.extended(),
        Some(true),
        "an update makes it read the power two blocks up"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pushed_block_shoves_a_mob_in_front_of_it() {
    use tuff_vanilla_data::registry::EntityType;
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(84, 1, 86), "minecraft:piston[facing=east]"),
            (at(85, 1, 86), "minecraft:stone"),
        ],
    )
    .await;
    summon(
        &b.region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [86.5, f64::from(FLOOR_Y + 1), 86.5],
    )
    .await;
    b.world.ticks(2).await;
    set(&b.region, &[(at(84, 1, 87), "minecraft:redstone_block")]).await;
    b.world.ticks(4).await;
    let pig = mobs(&b.region, EntityType::Pig).await.remove(0);
    assert!(
        pig.position[0] >= 87.4,
        "the pig was pushed out of the stone's way: {pig:?}"
    );
    assert_eq!(state(&b.region, at(86, 1, 86)).await.block(), Block::Stone);
    b.world.shutdown().await;
}

/// A bench with an east-facing dispenser holding `stacks`, a button on top
/// and the dispenser's menu open, so slot changes reach the player.
pub(super) async fn dispenser_bench(
    dir: &Path,
    stacks: Vec<ItemStack>,
) -> (Bench, BlockPos, BlockPos) {
    let mut b = bench(dir, &[ChunkPos::new(5, 5)], (82, 90), (84, 88)).await;
    let (dispenser, button) = (at(85, 1, 86), at(85, 2, 86));
    set(
        &b.region,
        &[
            (dispenser, "minecraft:dispenser[facing=east]"),
            (button, "minecraft:stone_button[face=floor,facing=north]"),
        ],
    )
    .await;
    fill(&mut b, dispenser, stacks, 1).await;
    (b, dispenser, button)
}

/// Waits for the dispenser's slot 0 to change and returns what it holds.
async fn dispenser_slot(b: &mut Bench, dispenser: BlockPos) -> Option<ItemStack> {
    expect(&mut b.events, |event| match event {
        PlayerEvent::ContainerSlots { pos, slots, .. } if pos == dispenser => slots
            .into_iter()
            .find(|(slot, _)| *slot == 0)
            .map(|(_, stack)| stack),
        _ => None,
    })
    .await
}

async fn entity_kinds(
    region: &RegionHandle,
) -> Vec<(tuff_vanilla_data::registry::EntityType, [f64; 3])> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.kind, e.position))
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_pours_a_water_bucket_and_scoops_it_back() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::WaterBucket, 1, MintReason::Test)],
    )
    .await;
    let front = at(86, 1, 86);
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, front).await.block(), Block::Water);
    let left = dispenser_slot(&mut b, dispenser).await;
    assert_eq!(left.map(|s| (s.item, s.count())), Some((Item::Bucket, 1)));

    b.world.ticks(30).await;
    click(&mut b, button, 3).await;
    b.world.ticks(4).await;
    assert!(
        is_air(state(&b.region, front).await),
        "the source was taken"
    );
    let left = dispenser_slot(&mut b, dispenser).await;
    assert_eq!(
        left.map(|s| (s.item, s.count())),
        Some((Item::WaterBucket, 1))
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_bone_meals_a_sapling() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::BoneMeal, 8, MintReason::Test)],
    )
    .await;
    let (soil, sapling) = (at(86, 0, 86), at(86, 1, 86));
    set(
        &b.region,
        &[(soil, "minecraft:dirt"), (sapling, "minecraft:oak_sapling")],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let left = dispenser_slot(&mut b, dispenser).await;
    assert_eq!(left.map(|s| (s.item, s.count())), Some((Item::BoneMeal, 7)));
    let grown = state(&b.region, sapling).await;
    assert!(
        grown.block() != Block::OakSapling || grown.get("stage") == Some("1"),
        "the sapling advanced or grew: {grown:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_shoots_a_snowball() {
    let dir = TestDir::new();
    let (mut b, _, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::Snowball, 4, MintReason::Test)],
    )
    .await;
    click(&mut b, button, 2).await;
    let mut seen = Vec::new();
    for _ in 0..8 {
        b.world.ticks(1).await;
        seen.extend(entity_kinds(&b.region).await);
    }
    assert!(
        seen.iter().any(|(kind, position)| {
            *kind == tuff_vanilla_data::registry::EntityType::Snowball && position[0] > 86.0
        }),
        "a snowball flies east: {seen:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_puts_a_helmet_on_the_player_in_front() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::IronHelmet, 1, MintReason::Test)],
    )
    .await;
    let RegionRequest::Player(mut standing) =
        update(&b.player, 1, [86.5, f64::from(FLOOR_Y + 1), 86.5])
    else {
        unreachable!()
    };
    standing.looks.height = 1.8;
    b.region.request(RegionRequest::Player(standing)).await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let (slot, stack) = expect(&mut b.events, |event| match event {
        PlayerEvent::DispenserEquip { slot, stack } => Some((slot, stack)),
        _ => None,
    })
    .await;
    assert_eq!(slot, tuff_vanilla_data::item::EquipmentSlot::Head);
    assert_eq!((stack.item, stack.count()), (Item::IronHelmet, 1));
    assert_eq!(dispenser_slot(&mut b, dispenser).await, None);
    let entities = entity_kinds(&b.region).await;
    assert!(
        !entities
            .iter()
            .any(|(kind, _)| *kind == tuff_vanilla_data::registry::EntityType::Item),
        "nothing was dropped: {entities:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_places_a_shulker_box() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::RedShulkerBox, 1, MintReason::Test)],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let placed = state(&b.region, at(86, 1, 86)).await;
    assert_eq!(placed.block(), Block::RedShulkerBox);
    assert_eq!(placed.facing(), Some(tuff_vanilla_data::block::Facing::Up));
    assert_eq!(dispenser_slot(&mut b, dispenser).await, None);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_hatches_spawn_eggs_and_sets_carts_on_rails() {
    let dir = TestDir::new();
    let (mut b, _, button) = dispenser_bench(
        dir.path(),
        vec![
            ItemStack::mint(Item::CowSpawnEgg, 1, MintReason::Test),
            ItemStack::mint(Item::Minecart, 1, MintReason::Test),
        ],
    )
    .await;
    set(
        &b.region,
        &[(at(86, 1, 86), "minecraft:rail[shape=east_west]")],
    )
    .await;
    for sequence in [2, 3] {
        click(&mut b, button, sequence).await;
        b.world.ticks(34).await;
    }
    let entities = entity_kinds(&b.region).await;
    for kind in [
        tuff_vanilla_data::registry::EntityType::Cow,
        tuff_vanilla_data::registry::EntityType::Minecart,
    ] {
        assert!(
            entities.iter().any(|(k, p)| *k == kind && p[0] > 86.0),
            "{kind:?} in front: {entities:?}"
        );
    }
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_hatching_a_spawn_egg_is_heard_by_a_sculk_sensor() {
    let dir = TestDir::new();
    let (mut b, _, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::CowSpawnEgg, 1, MintReason::Test)],
    )
    .await;
    let sensor = at(93, 1, 86);
    set(&b.region, &[(sensor, "minecraft:sculk_sensor")]).await;
    b.world.ticks(10).await;
    let quiet = state(&b.region, sensor).await;
    assert_eq!(quiet.get("sculk_sensor_phase"), Some("inactive"));
    click(&mut b, button, 2).await;
    b.world.ticks(40).await;
    let heard = state(&b.region, sensor).await;
    assert_ne!(
        heard.get("sculk_sensor_phase"),
        Some("inactive"),
        "{heard:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_fills_a_glass_bottle_from_water() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::GlassBottle, 1, MintReason::Test)],
    )
    .await;
    set(&b.region, &[(at(86, 1, 86), "minecraft:water")]).await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let filled = dispenser_slot(&mut b, dispenser).await.unwrap();
    assert_eq!(filled.item, Item::Potion);
    assert_eq!(
        filled
            .components
            .potion_contents
            .as_ref()
            .and_then(|c| c.potion),
        Some(tuff_vanilla_data::registry::Potion::Water)
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_shears_a_sheep_and_wears_the_shears() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::Shears, 1, MintReason::Test)],
    )
    .await;
    let (reply, answer) = oneshot::channel();
    b.region
        .request(RegionRequest::Summon {
            nbt: tuff_nbt::Compound::new()
                .with("id", "minecraft:sheep")
                .with("NoAI", 1i8),
            position: [86.5, f64::from(FLOOR_Y + 1), 86.5],
            fresh: false,
            reply,
        })
        .await;
    answer.await.unwrap();
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let worn = dispenser_slot(&mut b, dispenser).await.unwrap();
    assert_eq!((worn.item, worn.damage()), (Item::Shears, 1));
    b.world.ticks(2).await;
    let entities = entity_kinds(&b.region).await;
    assert!(
        entities
            .iter()
            .any(|(kind, _)| *kind == tuff_vanilla_data::registry::EntityType::Item),
        "wool dropped: {entities:?}"
    );
    b.world.shutdown().await;
}

async fn summon_still(b: &Bench, id: &str, position: [f64; 3], extra: &[(&str, i8)]) {
    let mut nbt = tuff_nbt::Compound::new().with("id", id).with("NoAI", 1i8);
    for (key, value) in extra {
        nbt = nbt.with(*key, *value);
    }
    let (reply, answer) = oneshot::channel();
    b.region
        .request(RegionRequest::Summon {
            nbt,
            position,
            fresh: false,
            reply,
        })
        .await;
    answer.await.unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_puts_armor_on_a_mob_that_picks_up_loot() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::IronHelmet, 1, MintReason::Test)],
    )
    .await;
    summon_still(
        &b,
        "minecraft:zombie",
        [86.5, f64::from(FLOOR_Y + 1), 86.5],
        &[("CanPickUpLoot", 1)],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    assert_eq!(dispenser_slot(&mut b, dispenser).await, None);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_keeps_armor_a_mob_will_not_wear() {
    let dir = TestDir::new();
    let (mut b, _, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::IronHelmet, 1, MintReason::Test)],
    )
    .await;
    summon_still(
        &b,
        "minecraft:zombie",
        [86.5, f64::from(FLOOR_Y + 1), 86.5],
        &[("CanPickUpLoot", 0)],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let entities = entity_kinds(&b.region).await;
    assert!(
        entities
            .iter()
            .any(|(kind, _)| *kind == tuff_vanilla_data::registry::EntityType::Item),
        "the helmet dropped: {entities:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_brushes_a_scute_off_an_armadillo() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::Brush, 1, MintReason::Test)],
    )
    .await;
    summon_still(
        &b,
        "minecraft:armadillo",
        [86.5, f64::from(FLOOR_Y + 1), 86.5],
        &[],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let worn = dispenser_slot(&mut b, dispenser).await.unwrap();
    assert_eq!((worn.item, worn.damage()), (Item::Brush, 16));
    b.world.ticks(2).await;
    let entities = entity_kinds(&b.region).await;
    assert!(
        entities
            .iter()
            .any(|(kind, _)| *kind == tuff_vanilla_data::registry::EntityType::Item),
        "a scute dropped: {entities:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_swallows_into_a_sulfur_cube() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::Dirt, 2, MintReason::Test)],
    )
    .await;
    summon_still(
        &b,
        "minecraft:sulfur_cube",
        [86.5, f64::from(FLOOR_Y + 1), 86.5],
        &[],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let left = dispenser_slot(&mut b, dispenser).await;
    assert_eq!(left.map(|s| (s.item, s.count())), Some((Item::Dirt, 1)));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_bottles_and_shears_a_full_beehive() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![
            ItemStack::mint(Item::GlassBottle, 1, MintReason::Test),
            ItemStack::mint(Item::Shears, 1, MintReason::Test),
        ],
    )
    .await;
    let front = at(86, 1, 86);
    set(
        &b.region,
        &[(front, "minecraft:beehive[facing=north,honey_level=5]")],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    let state_now = state(&b.region, front).await;
    assert_eq!(state_now.get("honey_level"), Some("0"));
    let filled = dispenser_slot(&mut b, dispenser).await.unwrap();
    assert_eq!(filled.item, Item::HoneyBottle);

    set(
        &b.region,
        &[(front, "minecraft:beehive[facing=north,honey_level=5]")],
    )
    .await;
    // The dispenser picks a random non-empty slot, so it may drop the
    // honey bottle before it fires the shears.
    for sequence in 3..8 {
        b.world.ticks(30).await;
        click(&mut b, button, sequence).await;
        b.world.ticks(4).await;
        if state(&b.region, front).await.get("honey_level") == Some("0") {
            break;
        }
    }
    assert_eq!(state(&b.region, front).await.get("honey_level"), Some("0"));
    let entities = entity_kinds(&b.region).await;
    let combs = entities
        .iter()
        .filter(|(kind, _)| *kind == tuff_vanilla_data::registry::EntityType::Item)
        .count();
    assert!(combs >= 3, "honeycombs dropped: {entities:?}");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_builds_a_snow_golem_with_a_pumpkin() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::CarvedPumpkin, 1, MintReason::Test)],
    )
    .await;
    set(
        &b.region,
        &[
            (at(86, 0, 86), "minecraft:snow_block"),
            (at(86, -1, 86), "minecraft:snow_block"),
        ],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    assert_eq!(dispenser_slot(&mut b, dispenser).await, None);
    let entities = entity_kinds(&b.region).await;
    assert!(
        entities
            .iter()
            .any(|(kind, _)| *kind == tuff_vanilla_data::registry::EntityType::SnowGolem),
        "a snow golem: {entities:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_summons_the_wither_with_the_last_skull() {
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(
            Item::WitherSkeletonSkull,
            1,
            MintReason::Test,
        )],
    )
    .await;
    let skull = "minecraft:wither_skeleton_skull";
    let sand = "minecraft:soul_sand";
    set(
        &b.region,
        &[
            (at(86, 1, 85), skull),
            (at(86, 1, 87), skull),
            (at(86, 0, 85), sand),
            (at(86, 0, 86), sand),
            (at(86, 0, 87), sand),
            (at(86, -1, 86), sand),
        ],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    assert_eq!(dispenser_slot(&mut b, dispenser).await, None);
    let entities = entity_kinds(&b.region).await;
    assert!(
        entities
            .iter()
            .any(|(kind, _)| *kind == tuff_vanilla_data::registry::EntityType::Wither),
        "a wither: {entities:?}"
    );
    assert!(is_air(state(&b.region, at(86, 0, 86)).await));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_lights_a_nether_portal_with_flint_and_steel() {
    let dir = TestDir::new();
    let (mut b, _, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::FlintAndSteel, 1, MintReason::Test)],
    )
    .await;
    let obsidian = "minecraft:obsidian";
    let mut frame = Vec::new();
    for z in [86, 87] {
        frame.push((at(86, 0, z), obsidian));
        frame.push((at(86, 4, z), obsidian));
    }
    for y in 1..=3 {
        frame.push((at(86, y, 85), obsidian));
        frame.push((at(86, y, 88), obsidian));
    }
    set(&b.region, &frame).await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    for y in 1..=3 {
        for z in [86, 87] {
            let portal = state(&b.region, at(86, y, z)).await;
            assert_eq!(portal.block(), Block::NetherPortal, "at y {y}, z {z}");
            assert_eq!(portal.get("axis"), Some("z"));
        }
    }
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_acts_on_a_block_in_the_neighbouring_region() {
    let dir = TestDir::new();
    let (mut b, east) = border_bench(dir.path()).await;
    let (dispenser, button, front) = (at(511, 1, 86), at(511, 2, 86), at(512, 1, 86));
    set(
        &b.region,
        &[
            (dispenser, "minecraft:dispenser[facing=east]"),
            (button, "minecraft:stone_button[face=floor,facing=north]"),
        ],
    )
    .await;
    set(&east, &[(front, "minecraft:water")]).await;
    fill(
        &mut b,
        dispenser,
        vec![ItemStack::mint(Item::Bucket, 1, MintReason::Test)],
        1,
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(6).await;
    assert!(is_air(state(&east, front).await), "the source was taken");
    let left = dispenser_slot(&mut b, dispenser).await;
    assert_eq!(
        left.map(|s| (s.item, s.count())),
        Some((Item::WaterBucket, 1))
    );
    b.world.shutdown().await;
}

/// A bench straddling the border between regions (0, 0) and (1, 0) at
/// x 512, with the east region's handle.
async fn border_bench(dir: &Path) -> (Bench, RegionHandle) {
    let b = bench(
        dir,
        &[ChunkPos::new(31, 5), ChunkPos::new(32, 5)],
        (506, 518),
        (84, 88),
    )
    .await;
    let east = b.world.region(ChunkPos::new(32, 0).region()).await;
    (b, east)
}

async fn border_line(
    b: &Bench,
    east: &RegionHandle,
    xs: std::ops::RangeInclusive<i32>,
) -> Vec<Block> {
    let mut blocks = Vec::new();
    for x in xs {
        let region = if x < 512 { &b.region } else { east };
        blocks.push(state(region, at(x, 1, 86)).await.block());
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_piston_pushes_a_line_across_a_region_border() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(509, 1, 86), "minecraft:piston[facing=east]"),
            (at(510, 1, 86), "minecraft:stone"),
            (at(511, 1, 86), "minecraft:stone"),
        ],
    )
    .await;
    set(
        &east,
        &[
            (at(512, 1, 86), "minecraft:stone"),
            (at(513, 1, 86), "minecraft:torch"),
        ],
    )
    .await;
    set(&b.region, &[(at(509, 1, 87), "minecraft:redstone_block")]).await;
    b.world.ticks(1).await;
    assert_eq!(
        state(&b.region, at(509, 1, 86)).await.extended(),
        Some(true)
    );
    assert_eq!(
        border_line(&b, &east, 510..=511).await,
        vec![Block::MovingPiston; 2],
        "the piston's own region starts moving at once"
    );
    b.world.ticks(1).await;
    assert_eq!(
        border_line(&b, &east, 512..=513).await,
        vec![Block::MovingPiston; 2],
        "the neighbour starts its part a tick later"
    );
    b.world.ticks(3).await;
    assert_eq!(
        border_line(&b, &east, 509..=514).await,
        vec![
            Block::Piston,
            Block::PistonHead,
            Block::Stone,
            Block::Stone,
            Block::Stone,
            Block::Air
        ]
    );
    assert_eq!(
        item_count(&east).await,
        1,
        "the torch dropped where it stood"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sticky_piston_pulls_a_block_back_across_a_region_border() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    let power = at(511, 1, 87);
    set(
        &b.region,
        &[(at(511, 1, 86), "minecraft:sticky_piston[facing=east]")],
    )
    .await;
    set(&east, &[(at(512, 1, 86), "minecraft:stone")]).await;
    set(&b.region, &[(power, "minecraft:redstone_block")]).await;
    b.world.ticks(5).await;
    assert_eq!(
        border_line(&b, &east, 511..=514).await,
        vec![
            Block::StickyPiston,
            Block::PistonHead,
            Block::Stone,
            Block::Air
        ],
        "the head and the block moved into the neighbour"
    );
    set(&b.region, &[(power, "minecraft:air")]).await;
    b.world.ticks(5).await;
    assert_eq!(
        border_line(&b, &east, 511..=513).await,
        vec![Block::StickyPiston, Block::Stone, Block::Air],
        "the block came back to the border"
    );
    assert_eq!(
        state(&b.region, at(511, 1, 86)).await.extended(),
        Some(false)
    );
    b.world.shutdown().await;
}

/// Sets blocks with their block entity data, as `/clone` pastes them.
async fn paste(region: &RegionHandle, pos: BlockPos, state: &str, nbt: tuff_nbt::Compound) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Paste {
                changes: vec![(pos, state.parse().unwrap(), Some(nbt))],
                strict: false,
            },
            reply,
        })
        .await;
    answer.await.unwrap();
}

fn record(item: Item) -> tuff_nbt::Compound {
    tuff_nbt::Compound::new()
        .with("id", "minecraft:jukebox")
        .with(
            "RecordItem",
            ItemStack::mint(item, 1, MintReason::Test).to_nbt(),
        )
}

/// A bench with a comparator at `(86, 1, 86)` reading west into `(85, 1,
/// 86)` and wire after it; returns the input position and the wire.
async fn comparator_bench(dir: &Path) -> (Bench, BlockPos, BlockPos) {
    let b = bench(dir, &[ChunkPos::new(5, 5)], (82, 90), (84, 88)).await;
    let (comparator, output) = (at(86, 1, 86), at(87, 1, 86));
    set(
        &b.region,
        &[
            (comparator, "minecraft:comparator[facing=west]"),
            (output, "minecraft:redstone_wire"),
        ],
    )
    .await;
    (b, at(85, 1, 86), output)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_comparator_reads_a_jukeboxs_record_and_follows_a_new_one() {
    let dir = TestDir::new();
    let (b, input, output) = comparator_bench(dir.path()).await;
    let jukebox = "minecraft:jukebox[has_record=true]";
    paste(&b.region, input, jukebox, record(Item::MusicDisc11)).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, output).await.power(), Some(11));
    paste(&b.region, input, jukebox, record(Item::MusicDisc13)).await;
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, output).await.power(),
        Some(1),
        "new block entity data alone updates the comparator"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_comparator_reads_a_lecterns_page() {
    let dir = TestDir::new();
    let (b, input, output) = comparator_bench(dir.path()).await;
    let mut book = ItemStack::mint(Item::WritableBook, 1, MintReason::Test).to_nbt();
    book.insert(
        "components",
        tuff_nbt::Compound::new().with(
            "minecraft:writable_book_content",
            tuff_nbt::Compound::new().with(
                "pages",
                tuff_nbt::Tag::List(vec![tuff_nbt::Tag::String("p".into()); 5]),
            ),
        ),
    );
    let nbt = tuff_nbt::Compound::new()
        .with("id", "minecraft:lectern")
        .with("Book", book)
        .with("Page", tuff_nbt::Tag::Int(2));
    paste(&b.region, input, "minecraft:lectern[has_book=true]", nbt).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, output).await.power(), Some(8));
    b.world.shutdown().await;
}

use crate::world::entity::{EntityBase, RegionEntity, Tick, TickContext};
use tuff_vanilla_data::registry::EntityType;

/// A stand-in item frame whose comparator reading the test sets.
#[derive(Debug)]
struct Frame {
    base: EntityBase,
    facing: tuff_vanilla_data::block::Facing,
    signal: Arc<std::sync::atomic::AtomicU8>,
}

impl Frame {
    fn spawn(
        b: &Bench,
        pos: BlockPos,
        facing: tuff_vanilla_data::block::Facing,
        signal: &Arc<std::sync::atomic::AtomicU8>,
    ) -> Box<dyn RegionEntity> {
        let position = [
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.25,
            f64::from(pos.z) + 0.5,
        ];
        let id = b.world.shared.next_entity_id();
        let base = EntityBase::new(EntityType::ItemFrame, id, id as u128, position, [0.0; 3]);
        Box::new(Frame {
            base,
            facing,
            signal: signal.clone(),
        })
    }
}

impl RegionEntity for Frame {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, _out: &mut tuff_nbt::Compound) {}

    fn tick(&mut self, _cx: &mut TickContext) -> Tick {
        Tick::Alive
    }

    fn comparator_frame(&self) -> Option<(tuff_vanilla_data::block::Facing, u8)> {
        Some((
            self.facing,
            self.signal.load(std::sync::atomic::Ordering::Relaxed),
        ))
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_comparator_reads_an_item_frame_through_a_block() {
    use std::sync::atomic::{AtomicU8, Ordering};
    use tuff_vanilla_data::block::Facing;
    let dir = TestDir::new();
    let (b, input, output) = comparator_bench(dir.path()).await;
    set(&b.region, &[(input, "minecraft:stone")]).await;
    let frame = at(84, 1, 86);
    let signal = Arc::new(AtomicU8::new(3));
    b.region
        .request(RegionRequest::SpawnEntity(Frame::spawn(
            &b,
            frame,
            Facing::West,
            &signal,
        )))
        .await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, output).await.power(), Some(3));
    signal.store(7, Ordering::Relaxed);
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, output).await.power(),
        Some(7),
        "turning the frame updates the comparator"
    );
    let other = Arc::new(AtomicU8::new(5));
    b.region
        .request(RegionRequest::SpawnEntity(Frame::spawn(
            &b,
            frame,
            Facing::North,
            &other,
        )))
        .await;
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, output).await.power(),
        Some(7),
        "a frame facing another way is not read"
    );
    b.region
        .request(RegionRequest::SpawnEntity(Frame::spawn(
            &b,
            frame,
            Facing::West,
            &other,
        )))
        .await;
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, output).await.power(),
        Some(0),
        "two frames facing the comparator are not read"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cloned_repeater_fires_on_the_original_schedule_across_a_region_border() {
    let dir = TestDir::new();
    let b = bench(
        dir.path(),
        &[ChunkPos::new(30, 5), ChunkPos::new(33, 5)],
        (485, 533),
        (84, 88),
    )
    .await;
    let (source, original, copy) = (at(486, 1, 86), at(487, 1, 86), at(530, 1, 86));
    let east = b.world.region(ChunkPos::of_block(copy).region()).await;
    let repeater = "minecraft:repeater[facing=west,delay=4]";
    set(&b.region, &[(original, repeater)]).await;
    set(&east, &[(copy, repeater)]).await;
    set(&b.region, &[(source, "minecraft:redstone_block")]).await;
    b.world.ticks(3).await;

    let (reply, answer) = oneshot::channel();
    b.region
        .request(RegionRequest::Blocks {
            op: BlockOp::CopyTicks {
                min: original,
                max: original,
            },
            reply,
        })
        .await;
    let BlockOpResult::Ticks(ticks) = answer.await.unwrap() else {
        panic!("expected ticks");
    };
    assert_eq!(ticks.len(), 1, "{ticks:?}");
    assert_eq!(ticks[0].delay, 5);
    let moved = ticks
        .iter()
        .map(|tick| CopiedTick { pos: copy, ..*tick })
        .collect();
    let (reply, answer) = oneshot::channel();
    east.request(RegionRequest::Blocks {
        op: BlockOp::PasteTicks { ticks: moved },
        reply,
    })
    .await;
    assert!(matches!(answer.await.unwrap(), BlockOpResult::Set(1)));

    for tick in 4..=9 {
        b.world.ticks(1).await;
        let original_on = state(&b.region, original).await.powered() == Some(true);
        let copy_on = state(&east, copy).await.powered() == Some(true);
        assert_eq!(original_on, tick >= 8, "original at tick {tick}");
        assert_eq!(copy_on, original_on, "copy at tick {tick}");
    }
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_sides_of_a_border_push_land_on_the_same_tick() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(509, 1, 86), "minecraft:piston[facing=east]"),
            (at(510, 1, 86), "minecraft:stone"),
            (at(511, 1, 86), "minecraft:stone"),
        ],
    )
    .await;
    set(&east, &[(at(512, 1, 86), "minecraft:stone")]).await;
    set(&b.region, &[(at(509, 1, 87), "minecraft:redstone_block")]).await;
    b.world.ticks(5).await;
    let line = border_line(&b, &east, 509..=513).await;
    assert_eq!(
        line,
        vec![
            Block::Piston,
            Block::PistonHead,
            Block::Stone,
            Block::Stone,
            Block::Stone
        ]
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fast_pulse_across_a_border_leaves_the_pushed_block_behind() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    let power = at(511, 1, 87);
    set(
        &b.region,
        &[(at(511, 1, 86), "minecraft:sticky_piston[facing=east]")],
    )
    .await;
    set(&east, &[(at(512, 1, 86), "minecraft:stone")]).await;
    set(&b.region, &[(power, "minecraft:redstone_block")]).await;
    b.world.ticks(1).await;
    set(&b.region, &[(power, "minecraft:air")]).await;
    b.world.ticks(10).await;
    assert_eq!(
        border_line(&b, &east, 511..=513).await,
        vec![Block::StickyPiston, Block::Air, Block::Stone]
    );
    assert_eq!(
        state(&b.region, at(511, 1, 86)).await.extended(),
        Some(false)
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_creative_player_breaking_the_head_removes_the_piston_without_drops() {
    use crate::world::messages::BreakMode;
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(84, 1, 86), "minecraft:piston[facing=east]"),
            (at(84, 1, 87), "minecraft:redstone_block"),
        ],
    )
    .await;
    b.world.ticks(5).await;
    assert_eq!(
        state(&b.region, at(85, 1, 86)).await.block(),
        Block::PistonHead
    );
    b.region
        .request(RegionRequest::Break {
            player: b.player.clone(),
            pos: at(85, 1, 86),
            tool: None,
            mode: BreakMode::Creative,
            sequence: 1,
        })
        .await;
    b.world.ticks(2).await;
    assert_eq!(
        blocks_named(&b, [at(84, 1, 86), at(85, 1, 86)]).await,
        vec![Block::Air, Block::Air]
    );
    assert_eq!(item_count(&b.region).await, 0);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moving_block_pushes_a_player_presence() {
    let dir = TestDir::new();
    let mut b = piston_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(84, 1, 86), "minecraft:piston[facing=east]"),
            (at(85, 1, 86), "minecraft:stone"),
        ],
    )
    .await;
    b.region
        .request(player_at(
            &b.player,
            [86.5, f64::from(FLOOR_Y + 1), 86.5],
            crate::world::messages::PlayerLooks::default(),
        ))
        .await;
    b.world.ticks(2).await;
    set(&b.region, &[(at(84, 1, 87), "minecraft:redstone_block")]).await;
    let mut pushed = false;
    for _ in 0..6 {
        b.world.ticks(1).await;
        while let Ok(event) = b.events.try_recv() {
            if let PlayerEvent::PistonPushed { delta, .. } = event {
                assert!(delta[0] > 0.0);
                pushed = true;
            }
        }
    }
    assert!(pushed);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_honey_block_carries_a_mob_standing_on_it_sideways() {
    use tuff_vanilla_data::registry::EntityType;
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    set(
        &b.region,
        &[
            (at(84, 2, 86), "minecraft:piston[facing=east]"),
            (at(85, 2, 86), "minecraft:honey_block"),
        ],
    )
    .await;
    summon(
        &b.region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [85.5, f64::from(FLOOR_Y + 3), 86.5],
    )
    .await;
    b.world.ticks(10).await;
    set(&b.region, &[(at(84, 2, 87), "minecraft:redstone_block")]).await;
    b.world.ticks(5).await;
    let pig = mobs(&b.region, EntityType::Pig).await.remove(0);
    assert!(pig.position[0] >= 86.4, "carried east: {pig:?}");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_before_the_border_halo_loads_run_once_it_does() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(30, 5)], (500, 511), (84, 88)).await;
    set(
        &b.region,
        &[
            (at(510, 1, 86), "minecraft:redstone_lamp"),
            (at(509, 1, 86), "minecraft:redstone_block"),
        ],
    )
    .await;
    b.world.ticks(3).await;
    assert_eq!(state(&b.region, at(510, 1, 86)).await.lit(), Some(false));
    view_around(&b.world, &b.player, &mut b.events, ChunkPos::new(33, 5)).await;
    let mut ticks = 0;
    while state(&b.region, at(510, 1, 86)).await.lit() != Some(true) {
        assert!(ticks < 20, "the lamp lights once the halo is in");
        b.world.ticks(1).await;
        ticks += 1;
    }
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_in_the_next_region_presses_a_plate_across_the_border() {
    let dir = TestDir::new();
    let b = bench(
        dir.path(),
        &[ChunkPos::new(31, 5), ChunkPos::new(32, 5)],
        (506, 518),
        (84, 88),
    )
    .await;
    let east = b.world.region(ChunkPos::new(32, 0).region()).await;
    let plate = at(511, 1, 86);
    set(&b.region, &[(plate, "minecraft:oak_pressure_plate")]).await;
    b.world.ticks(2).await;
    let nbt = tuff_nbt::Compound::new()
        .with("id", "minecraft:zombie")
        .with("NoAI", 1i8);
    let (reply, answer) = oneshot::channel();
    east.request(RegionRequest::Summon {
        nbt,
        position: [512.1, f64::from(FLOOR_Y) + 1.0, 86.5],
        fresh: false,
        reply,
    })
    .await;
    answer.await.unwrap();
    let mut ticks = 0;
    while state(&b.region, plate).await.powered() != Some(true) {
        assert!(ticks < 20, "the zombie presses the plate");
        b.world.ticks(1).await;
        ticks += 1;
    }
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_jukebox_takes_a_record_in_hand_and_gives_it_back() {
    let dir = TestDir::new();
    let (mut b, input, output) = comparator_bench(dir.path()).await;
    set(&b.region, &[(input, "minecraft:jukebox")]).await;
    b.world.ticks(2).await;
    b.region
        .request(RegionRequest::Place(place(
            &b.player,
            input,
            Item::MusicDisc11,
            1,
        )))
        .await;
    expect(&mut b.events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome: ActionOutcome::Used { .. },
        } => Some(()),
        _ => None,
    })
    .await;
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, input).await.get("has_record"),
        Some("true")
    );
    assert_eq!(state(&b.region, output).await.power(), Some(11));
    click(&mut b, input, 2).await;
    b.world.ticks(4).await;
    assert_eq!(
        state(&b.region, input).await.get("has_record"),
        Some("false")
    );
    assert_eq!(state(&b.region, output).await.power(), Some(0));
    assert_eq!(item_count(&b.region).await, 1);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lectern_takes_a_book_turns_pages_pulses_and_gives_it_back() {
    use tuff_item::item::decor::Filterable;
    let dir = TestDir::new();
    let (mut b, input, output) = comparator_bench(dir.path()).await;
    set(&b.region, &[(input, "minecraft:lectern[facing=north]")]).await;
    b.world.ticks(2).await;
    let mut put = place(&b.player, input, Item::WritableBook, 1);
    put.components.writable_book = Some(vec![Filterable::plain("p".to_string()); 5]);
    b.region.request(RegionRequest::Place(put)).await;
    expect(&mut b.events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome: ActionOutcome::Used { .. },
        } => Some(()),
        _ => None,
    })
    .await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, input).await.get("has_book"), Some("true"));
    assert_eq!(state(&b.region, output).await.power(), Some(1));

    let mut open = place(&b.player, input, Item::Stick, 2);
    open.item = None;
    open.held = None;
    b.region.request(RegionRequest::Place(open)).await;
    let menu = expect(&mut b.events, |event| match event {
        PlayerEvent::MenuOpen(open) => Some(open),
        _ => None,
    })
    .await;
    assert_eq!(menu.kind, crate::world::menu::MenuKind::Lectern);

    b.region
        .request(RegionRequest::LecternButton {
            player: b.player.clone(),
            pos: input,
            button: 2,
            may_build: true,
        })
        .await;
    b.world.ticks(1).await;
    assert_eq!(state(&b.region, input).await.powered(), Some(true));
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, input).await.powered(), Some(false));
    assert_eq!(state(&b.region, output).await.power(), Some(4));

    b.region
        .request(RegionRequest::LecternButton {
            player: b.player.clone(),
            pos: input,
            button: 3,
            may_build: false,
        })
        .await;
    b.world.ticks(2).await;
    assert_eq!(state(&b.region, input).await.get("has_book"), Some("true"));
    b.region
        .request(RegionRequest::LecternButton {
            player: b.player.clone(),
            pos: input,
            button: 3,
            may_build: true,
        })
        .await;
    b.world.ticks(2).await;
    assert_eq!(state(&b.region, input).await.get("has_book"), Some("false"));
    assert_eq!(state(&b.region, output).await.power(), Some(0));
    expect(&mut b.events, |event| match event {
        PlayerEvent::ItemGranted {
            stack,
            picked_up: false,
        } if stack.item == Item::WritableBook => Some(()),
        _ => None,
    })
    .await;
    b.world.shutdown().await;
}

async fn crafter_bench(dir: &Path) -> (Bench, BlockPos, BlockPos) {
    let mut b = bench(dir, &[ChunkPos::new(5, 5)], (82, 92), (84, 88)).await;
    let (crafter, lever) = (at(85, 1, 86), at(85, 1, 87));
    set(
        &b.region,
        &[
            (crafter, "minecraft:crafter[orientation=east_up]"),
            (lever, "minecraft:lever[face=floor,facing=south]"),
        ],
    )
    .await;
    fill(
        &mut b,
        crafter,
        vec![ItemStack::mint(Item::OakLog, 2, MintReason::Test)],
        1,
    )
    .await;
    (b, crafter, lever)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_powered_crafter_crafts_and_drops_its_result() {
    let dir = TestDir::new();
    let (mut b, crafter, lever) = crafter_bench(dir.path()).await;
    click(&mut b, lever, 2).await;
    b.world.ticks(3).await;
    assert_eq!(item_count(&b.region).await, 0, "the craft waits four ticks");
    b.world.ticks(2).await;
    assert_eq!(item_count(&b.region).await, 1);
    assert_eq!(
        state(&b.region, crafter).await.get("crafting"),
        Some("true"),
        "the crafting cooldown runs six ticks"
    );
    b.world.ticks(8).await;
    assert_eq!(item_count(&b.region).await, 1);
    assert_eq!(
        state(&b.region, crafter).await.get("triggered"),
        Some("true")
    );
    assert_eq!(
        state(&b.region, crafter).await.get("crafting"),
        Some("false")
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crafter_tells_players_nearby_what_it_crafted() {
    let dir = TestDir::new();
    let (mut b, _, lever) = crafter_bench(dir.path()).await;
    b.region
        .request(update(&b.player, 1, [85.5, f64::from(FLOOR_Y + 1), 90.5]))
        .await;
    click(&mut b, lever, 2).await;
    let (recipe, ingredients) = ticking_expect(&b.world, &mut b.events, 20, |event| match event {
        PlayerEvent::Advancement(event) => match *event {
            crate::world::messages::AdvancementEvent::CrafterRecipeCrafted {
                recipe,
                ingredients,
            } => Some((recipe, ingredients)),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(&*recipe, "minecraft:oak_planks");
    assert_eq!(
        ingredients.iter().map(|s| s.item).collect::<Vec<_>>(),
        [Item::OakLog]
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crafter_fills_the_container_it_faces_and_a_comparator_reads_its_slots() {
    let dir = TestDir::new();
    let (mut b, crafter, lever) = crafter_bench(dir.path()).await;
    let (chest, comparator, output) = (at(86, 1, 86), at(87, 1, 86), at(88, 1, 86));
    set(
        &b.region,
        &[
            (chest, "minecraft:chest[facing=north]"),
            (comparator, "minecraft:comparator[facing=west]"),
            (output, "minecraft:redstone_wire"),
        ],
    )
    .await;
    click(&mut b, lever, 2).await;
    b.world.ticks(12).await;
    assert_eq!(item_count(&b.region).await, 0, "the chest took the planks");
    assert_eq!(state(&b.region, output).await.power(), Some(1));
    assert_eq!(
        state(&b.region, crafter).await.get("crafting"),
        Some("false")
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_comparator_counts_a_crafters_filled_and_disabled_slots() {
    let dir = TestDir::new();
    let (b, crafter, _) = crafter_bench(dir.path()).await;
    let (comparator, output) = (at(84, 1, 86), at(83, 1, 86));
    set(
        &b.region,
        &[
            (comparator, "minecraft:comparator[facing=east]"),
            (output, "minecraft:redstone_wire"),
        ],
    )
    .await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, output).await.power(), Some(1));
    b.region
        .request(RegionRequest::CrafterSlotState {
            pos: crafter,
            slot: 4,
            enabled: false,
        })
        .await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, output).await.power(), Some(2));
    b.world.shutdown().await;
}

async fn command_block(
    b: &Bench,
    pos: BlockPos,
    facing: &str,
    command: &str,
    mode: crate::world::messages::CommandMode,
    conditional: bool,
    automatic: bool,
) {
    command_block_in(
        b,
        &b.region,
        pos,
        facing,
        command,
        mode,
        conditional,
        automatic,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn command_block_in(
    b: &Bench,
    region: &RegionHandle,
    pos: BlockPos,
    facing: &str,
    command: &str,
    mode: crate::world::messages::CommandMode,
    conditional: bool,
    automatic: bool,
) {
    use crate::world::messages::CommandMode;
    let name = match mode {
        CommandMode::Redstone => "command_block",
        CommandMode::Auto => "repeating_command_block",
        CommandMode::Sequence => "chain_command_block",
    };
    set(
        region,
        &[(pos, &format!("minecraft:{name}[facing={facing}]"))],
    )
    .await;
    region
        .request(RegionRequest::SetCommandBlock {
            player: b.player.clone(),
            pos,
            edit: crate::world::messages::CommandBlockEdit {
                command: command.to_owned(),
                mode,
                track_output: true,
                conditional,
                automatic,
            },
        })
        .await;
    command_nbt(region, pos).await;
}

async fn settle_commands(b: &Bench, mut done: impl AsyncFnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        b.world.ticks(1).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        if done().await {
            return;
        }
    }
    panic!("the command block run never finished");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_powered_command_block_runs_its_command_once() {
    use crate::world::messages::CommandMode;
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 92), (84, 88)).await;
    let (block, target) = (at(85, 1, 86), at(88, 1, 86));
    let command = format!("setblock 88 {} 86 minecraft:diamond_block", FLOOR_Y + 1);
    command_block(
        &b,
        block,
        "east",
        &command,
        CommandMode::Redstone,
        false,
        false,
    )
    .await;
    b.world.ticks(3).await;
    assert_eq!(state(&b.region, target).await.block(), Block::Air);
    set(&b.region, &[(at(85, 1, 87), "minecraft:redstone_block")]).await;
    settle_commands(&b, async || {
        state(&b.region, target).await.block() == Block::DiamondBlock
    })
    .await;
    set(&b.region, &[(target, "minecraft:air")]).await;
    b.world.ticks(10).await;
    assert_eq!(
        state(&b.region, target).await.block(),
        Block::Air,
        "an impulse block runs once per rising edge"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_conditional_chain_block_follows_only_a_success() {
    use crate::world::messages::CommandMode;
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 92), (84, 88)).await;
    let (head, chain) = (at(84, 1, 86), at(85, 1, 86));
    let (first, second) = (at(88, 1, 85), at(88, 1, 87));
    let ok = format!("setblock 88 {} 85 minecraft:diamond_block", FLOOR_Y + 1);
    let next = format!("setblock 88 {} 87 minecraft:gold_block", FLOOR_Y + 1);
    command_block(&b, head, "east", &ok, CommandMode::Redstone, false, false).await;
    command_block(&b, chain, "east", &next, CommandMode::Sequence, true, true).await;
    set(&b.region, &[(at(84, 1, 87), "minecraft:redstone_block")]).await;
    settle_commands(&b, async || {
        state(&b.region, second).await.block() == Block::GoldBlock
    })
    .await;
    assert_eq!(state(&b.region, first).await.block(), Block::DiamondBlock);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_conditional_chain_block_skips_after_a_failure() {
    use crate::world::messages::CommandMode;
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 92), (84, 88)).await;
    let (head, chain, third) = (at(84, 1, 86), at(85, 1, 86), at(86, 1, 86));
    let (skipped, ran) = (at(88, 1, 85), at(88, 1, 87));
    let bad = "setblock 88 1 85 minecraft:not_a_block".to_owned();
    let skip = format!("setblock 88 {} 85 minecraft:gold_block", FLOOR_Y + 1);
    let run = format!("setblock 88 {} 87 minecraft:iron_block", FLOOR_Y + 1);
    command_block(&b, head, "east", &bad, CommandMode::Redstone, false, false).await;
    command_block(&b, chain, "east", &skip, CommandMode::Sequence, true, true).await;
    command_block(&b, third, "east", &run, CommandMode::Sequence, false, true).await;
    set(&b.region, &[(at(84, 1, 87), "minecraft:redstone_block")]).await;
    settle_commands(&b, async || {
        state(&b.region, ran).await.block() == Block::IronBlock
    })
    .await;
    assert_eq!(state(&b.region, skipped).await.block(), Block::Air);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_editor_turns_a_command_block_into_a_repeating_one() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 92), (84, 88)).await;
    let (block, target) = (at(85, 1, 86), at(88, 1, 86));
    set(
        &b.region,
        &[(block, "minecraft:command_block[facing=east]")],
    )
    .await;
    let command = format!("setblock 88 {} 86 minecraft:diamond_block", FLOOR_Y + 1);
    b.region
        .request(RegionRequest::SetCommandBlock {
            player: b.player.clone(),
            pos: block,
            edit: crate::world::messages::CommandBlockEdit {
                command,
                mode: crate::world::messages::CommandMode::Auto,
                track_output: true,
                conditional: false,
                automatic: true,
            },
        })
        .await;
    b.world.ticks(1).await;
    assert_eq!(
        state(&b.region, block).await.block(),
        Block::RepeatingCommandBlock
    );
    settle_commands(&b, async || {
        state(&b.region, target).await.block() == Block::DiamondBlock
    })
    .await;
    set(&b.region, &[(target, "minecraft:air")]).await;
    settle_commands(&b, async || {
        state(&b.region, target).await.block() == Block::DiamondBlock
    })
    .await;
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dispenser_stands_an_armor_stand_up_in_front_of_it() {
    use tuff_vanilla_data::registry::EntityType;
    let dir = TestDir::new();
    let (mut b, dispenser, button) = dispenser_bench(
        dir.path(),
        vec![ItemStack::mint(Item::ArmorStand, 1, MintReason::Test)],
    )
    .await;
    click(&mut b, button, 2).await;
    b.world.ticks(4).await;
    assert_eq!(dispenser_slot(&mut b, dispenser).await, None);
    let entities = entity_kinds(&b.region).await;
    let stand = entities
        .iter()
        .find(|(kind, _)| *kind == EntityType::ArmorStand)
        .unwrap_or_else(|| panic!("a stand in front: {entities:?}"));
    assert_eq!(stand.1[0], 86.5);
    assert_eq!(stand.1[2], 86.5);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_block_does_nothing_when_command_blocks_work_is_off() {
    use crate::world::messages::CommandMode;
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 92), (84, 88)).await;
    b.world.game_rules.send_modify(|rules| {
        rules
            .set(
                "command_blocks_work",
                crate::world::game_rules::RuleValue::Bool(false),
            )
            .expect("a known rule");
    });
    let (block, target) = (at(85, 1, 86), at(88, 1, 86));
    let command = format!("setblock 88 {} 86 minecraft:diamond_block", FLOOR_Y + 1);
    command_block(
        &b,
        block,
        "east",
        &command,
        CommandMode::Redstone,
        false,
        false,
    )
    .await;
    set(&b.region, &[(at(85, 1, 87), "minecraft:redstone_block")]).await;
    b.world.ticks(15).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    b.world.ticks(2).await;
    assert_eq!(state(&b.region, target).await.block(), Block::Air);
    b.world.shutdown().await;
}

async fn command_border_bench(dir: &Path) -> (Bench, RegionHandle) {
    let b = bench(
        dir,
        &[ChunkPos::new(31, 5), ChunkPos::new(32, 5)],
        (506, 518),
        (84, 90),
    )
    .await;
    let east = b.world.region(RegionPos::new(1, 0)).await;
    (b, east)
}

async fn command_nbt(region: &RegionHandle, pos: BlockPos) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: crate::world::messages::BlockOp::Copy {
                positions: vec![pos],
            },
            reply,
        })
        .await;
    match answer.await.unwrap() {
        crate::world::messages::BlockOpResult::Copied(mut found) => found
            .pop()
            .flatten()
            .and_then(|(_, nbt)| nbt)
            .expect("a command block"),
        other => panic!("{other:?}"),
    }
}

fn setblock(x: i32, z: i32, block: &str) -> String {
    format!("setblock {x} {} {z} minecraft:{block}", FLOOR_Y + 1)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_chain_runs_on_across_a_region_border() {
    use crate::world::messages::CommandMode;
    let dir = TestDir::new();
    let (b, east) = command_border_bench(dir.path()).await;
    let west = b.region.clone();
    let row = |x| at(x, 1, 86);
    command_block_in(
        &b,
        &west,
        row(510),
        "east",
        &setblock(516, 84, "diamond_block"),
        CommandMode::Redstone,
        false,
        false,
    )
    .await;
    command_block_in(
        &b,
        &west,
        row(511),
        "east",
        &setblock(516, 85, "gold_block"),
        CommandMode::Sequence,
        false,
        true,
    )
    .await;
    command_block_in(
        &b,
        &east,
        row(512),
        "east",
        &setblock(516, 87, "iron_block"),
        CommandMode::Sequence,
        true,
        true,
    )
    .await;
    command_block_in(
        &b,
        &east,
        row(513),
        "east",
        &setblock(516, 88, "emerald_block"),
        CommandMode::Sequence,
        false,
        true,
    )
    .await;
    set(&west, &[(at(510, 1, 87), "minecraft:redstone_block")]).await;
    settle_commands(&b, async || {
        state(&east, at(516, 1, 88)).await.block() == Block::EmeraldBlock
    })
    .await;
    assert_eq!(
        state(&east, at(516, 1, 84)).await.block(),
        Block::DiamondBlock
    );
    assert_eq!(state(&east, at(516, 1, 85)).await.block(), Block::GoldBlock);
    assert_eq!(
        state(&east, at(516, 1, 87)).await.block(),
        Block::IronBlock,
        "the conditional block over the border saw its back block succeed"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_conditional_chain_block_over_a_border_skips_after_a_failure() {
    use crate::world::messages::CommandMode;
    let dir = TestDir::new();
    let (b, east) = command_border_bench(dir.path()).await;
    let west = b.region.clone();
    let row = |x| at(x, 1, 86);
    command_block_in(
        &b,
        &west,
        row(511),
        "east",
        "setblock 0 1 0 minecraft:not_a_block",
        CommandMode::Redstone,
        false,
        false,
    )
    .await;
    command_block_in(
        &b,
        &east,
        row(512),
        "east",
        &setblock(516, 85, "gold_block"),
        CommandMode::Sequence,
        true,
        true,
    )
    .await;
    command_block_in(
        &b,
        &east,
        row(513),
        "east",
        &setblock(516, 87, "iron_block"),
        CommandMode::Sequence,
        false,
        true,
    )
    .await;
    set(&west, &[(at(511, 1, 87), "minecraft:redstone_block")]).await;
    settle_commands(&b, async || {
        state(&east, at(516, 1, 87)).await.block() == Block::IronBlock
    })
    .await;
    assert_eq!(state(&east, at(516, 1, 85)).await.block(), Block::Air);
    b.world.shutdown().await;
}

/// An impulse block conditional on a command block in the neighbour
/// region asks that region for its success count before it runs.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_conditional_block_reads_its_back_block_over_a_border() {
    use crate::world::messages::CommandMode;
    let dir = TestDir::new();
    let (b, east) = command_border_bench(dir.path()).await;
    let west = b.region.clone();
    let (good, bad) = (at(511, 1, 85), at(511, 1, 89));
    command_block_in(
        &b,
        &west,
        good,
        "east",
        &setblock(507, 84, "gold_block"),
        CommandMode::Redstone,
        false,
        false,
    )
    .await;
    command_block_in(
        &b,
        &west,
        bad,
        "east",
        "setblock 0 1 0 minecraft:not_a_block",
        CommandMode::Redstone,
        false,
        false,
    )
    .await;
    set(
        &west,
        &[
            (at(510, 1, 85), "minecraft:redstone_block"),
            (at(510, 1, 89), "minecraft:redstone_block"),
        ],
    )
    .await;
    settle_commands(&b, async || {
        state(&west, at(507, 1, 84)).await.block() == Block::GoldBlock
    })
    .await;
    b.world.ticks(5).await;
    assert_eq!(
        command_nbt(&west, bad).await.get_int("SuccessCount"),
        Some(0)
    );

    let (after_good, after_bad) = (at(512, 1, 85), at(512, 1, 89));
    command_block_in(
        &b,
        &east,
        after_good,
        "east",
        &setblock(516, 85, "iron_block"),
        CommandMode::Redstone,
        true,
        false,
    )
    .await;
    command_block_in(
        &b,
        &east,
        after_bad,
        "east",
        &setblock(516, 89, "iron_block"),
        CommandMode::Redstone,
        true,
        false,
    )
    .await;
    set(
        &east,
        &[
            (at(512, 2, 85), "minecraft:redstone_block"),
            (at(512, 2, 89), "minecraft:redstone_block"),
        ],
    )
    .await;
    settle_commands(&b, async || {
        state(&east, at(516, 1, 85)).await.block() == Block::IronBlock
    })
    .await;
    b.world.ticks(5).await;
    assert_eq!(state(&east, at(516, 1, 89)).await.block(), Block::Air);
    assert_eq!(
        command_nbt(&east, after_bad).await.get_bool("conditionMet"),
        Some(false)
    );
    b.world.shutdown().await;
}

/// With command blocks off a block still counts as run: its chain goes
/// on and each block records the tick, though no command runs.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_still_advances_when_command_blocks_work_is_off() {
    use crate::world::messages::CommandMode;
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (82, 92), (84, 88)).await;
    b.world.game_rules.send_modify(|rules| {
        rules
            .set(
                "command_blocks_work",
                crate::world::game_rules::RuleValue::Bool(false),
            )
            .expect("a known rule");
    });
    let (head, chain) = (at(84, 1, 86), at(85, 1, 86));
    command_block(
        &b,
        head,
        "east",
        &setblock(88, 85, "gold_block"),
        CommandMode::Redstone,
        false,
        false,
    )
    .await;
    command_block(
        &b,
        chain,
        "east",
        &setblock(88, 87, "gold_block"),
        CommandMode::Sequence,
        false,
        true,
    )
    .await;
    set(&b.region, &[(at(84, 1, 87), "minecraft:redstone_block")]).await;
    settle_commands(&b, async || {
        command_nbt(&b.region, chain)
            .await
            .get("LastExecution")
            .is_some()
    })
    .await;
    assert!(
        command_nbt(&b.region, head)
            .await
            .get("LastExecution")
            .is_some()
    );
    assert_eq!(state(&b.region, at(88, 1, 85)).await.block(), Block::Air);
    assert_eq!(state(&b.region, at(88, 1, 87)).await.block(), Block::Air);
    b.world.shutdown().await;
}
