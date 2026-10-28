//! Random ticks, growth and fire driven through the region actors.

use super::*;
use crate::world::game_rules::RuleValue;
use crate::world::messages::{BlockOp, BlockOpResult};

const FLOOR_Y: i32 = 200;

async fn set_blocks(
    region: &RegionHandle,
    changes: Vec<(BlockPos, tuff_vanilla_data::BlockState)>,
) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes,
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: None,
            },
            reply,
        })
        .await;
    assert!(matches!(answer.await.unwrap(), BlockOpResult::Set(_)));
}

/// Loads the 3x3 chunks around `center`, so random ticks run in it.
async fn view_around(
    world: &TestWorld,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    center: ChunkPos,
) {
    let mut chunks = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            chunks.push(ChunkPos::new(center.x + dx, center.z + dz));
        }
    }
    let region = world.region(center.region()).await;
    subscribe(&region, player, events, &chunks).await;
}

fn set_rule(world: &TestWorld, name: &str, value: RuleValue) {
    world.game_rules.send_modify(|rules| {
        rules.set(name, value).expect("a known rule");
    });
}

/// Ticks until `check` holds for the blocks at `positions`, failing after
/// `budget` ticks.
async fn wait_for_blocks(
    world: &TestWorld,
    region: &RegionHandle,
    positions: &[BlockPos],
    budget: u64,
    what: &str,
    check: impl Fn(&[tuff_vanilla_data::BlockState]) -> bool,
) {
    for _ in 0..budget / 5 {
        let mut states = Vec::new();
        for pos in positions {
            states.push(block_at(region, *pos).await.expect("loaded"));
        }
        if check(&states) {
            return;
        }
        world.ticks(5).await;
    }
    panic!("{what} within {budget} ticks");
}

fn square(from: (i32, i32), to: (i32, i32), y: i32) -> Vec<BlockPos> {
    let mut positions = Vec::new();
    for x in from.0..=to.0 {
        for z in from.1..=to.1 {
            positions.push(BlockPos::new(x, y, z));
        }
    }
    positions
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_ticks_ripen_wheat_and_grow_a_sapling_into_a_tree() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_rule(&world, "random_tick_speed", RuleValue::Int(500));
    let (player, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;

    let water = BlockPos::new(86, FLOOR_Y, 86);
    let moist = Block::Farmland.default_state().with_moisture(7).unwrap();
    let mut changes: Vec<_> = square((84, 84), (88, 88), FLOOR_Y)
        .into_iter()
        .map(|pos| {
            (
                pos,
                if pos == water {
                    Block::Water.default_state()
                } else {
                    moist
                },
            )
        })
        .collect();
    let crops: Vec<BlockPos> = square((84, 84), (88, 88), FLOOR_Y + 1)
        .into_iter()
        .filter(|pos| (pos.x, pos.z) != (water.x, water.z))
        .collect();
    changes.extend(crops.iter().map(|pos| (*pos, Block::Wheat.default_state())));
    changes.extend(
        square((90, 90), (94, 94), FLOOR_Y)
            .into_iter()
            .map(|pos| (pos, Block::GrassBlock.default_state())),
    );
    let sapling = BlockPos::new(92, FLOOR_Y + 1, 92);
    changes.push((sapling, Block::OakSapling.default_state()));
    set_blocks(&region, changes).await;

    wait_for_blocks(
        &world,
        &region,
        &crops,
        3000,
        "the wheat ripens",
        |states| {
            states
                .iter()
                .all(|state| state.block() == Block::Wheat && state.age() == Some(7))
        },
    )
    .await;
    wait_for_blocks(
        &world,
        &region,
        &[sapling],
        3000,
        "the sapling grows",
        |states| states[0].block() == Block::OakLog,
    )
    .await;
    assert_eq!(
        block_at(&region, BlockPos::new(92, FLOOR_Y, 92)).await,
        Some(Block::Dirt.default_state()),
        "a tree turns the grass under it to dirt"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaves_decay_once_their_log_is_gone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_rule(&world, "random_tick_speed", RuleValue::Int(200));
    let (player, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;

    let log = BlockPos::new(88, FLOOR_Y, 88);
    let leaves: Vec<BlockPos> = (1..=3)
        .map(|dx| BlockPos::new(88 + dx, FLOOR_Y, 88))
        .collect();
    let mut changes = vec![(log, Block::OakLog.default_state())];
    for (index, pos) in leaves.iter().enumerate() {
        let state = Block::OakLeaves
            .default_state()
            .with_distance(index as u8 + 1)
            .unwrap();
        changes.push((*pos, state));
    }
    set_blocks(&region, changes).await;
    world.ticks(100).await;
    for pos in &leaves {
        assert_eq!(
            block_at(&region, *pos).await.map(|s| s.block()),
            Some(Block::OakLeaves),
            "leaves near a log stay"
        );
    }
    while events.try_recv().is_ok() {}
    set_blocks(&region, vec![(log, Block::Air.default_state())]).await;
    wait_for_blocks(
        &world,
        &region,
        &leaves,
        2000,
        "the leaves decay",
        |states| {
            states
                .iter()
                .all(|state| tuff_world_model::chunk::is_air(*state))
        },
    )
    .await;
    let frames: Vec<_> = leaves
        .iter()
        .flat_map(|pos| (1..=7u8).map(move |distance| (pos, distance)))
        .map(|(pos, distance)| {
            framed(&play::LevelEvent {
                event: play::level_event::BLOCK_BREAK,
                position: *pos,
                data: Block::OakLeaves
                    .default_state()
                    .with_distance(distance)
                    .unwrap()
                    .id() as i32,
                global: false,
            })
        })
        .collect();
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            assert!(
                !frames.iter().any(|f| packets.contains(f)),
                "decaying leaves are silent"
            );
        }
    }
    world.shutdown().await;
}

async fn count_blocks(region: &RegionHandle, positions: &[BlockPos], block: Block) -> usize {
    let mut count = 0;
    for pos in positions {
        if block_at(region, *pos).await.map(|s| s.block()) == Some(block) {
            count += 1;
        }
    }
    count
}

/// Drains every packet queued so far and reports whether any of them was a
/// fire extinguish level event at one of `candidates` (fire spreads, so the
/// block that actually goes out is not fixed to one position).
fn saw_fire_extinguish_sound(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    candidates: &[BlockPos],
) -> bool {
    let expected: Vec<_> = candidates
        .iter()
        .map(|pos| {
            framed(&play::LevelEvent {
                event: play::level_event::SOUND_EXTINGUISH_FIRE,
                position: *pos,
                data: 0,
                global: false,
            })
        })
        .collect();
    let mut seen = false;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            seen |= packets.iter().any(|packet| expected.contains(packet));
        }
    }
    seen
}

/// Rain removes a fire block through the scheduled tick, which never calls
/// out to a sound (the fire block's player-break hook is the only place its
/// own removal plays the extinguish sound; the scheduled tick just removes
/// the block).
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_burns_a_wooden_floor_until_rain_puts_it_out() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_rule(
        &world,
        "fire_spread_radius_around_player",
        RuleValue::Int(-1),
    );
    let (player, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;

    let floor = square((84, 84), (90, 90), FLOOR_Y);
    let above: Vec<BlockPos> = square((83, 83), (91, 91), FLOOR_Y + 1);
    let mut changes: Vec<_> = floor
        .iter()
        .map(|pos| (*pos, Block::OakPlanks.default_state()))
        .collect();
    let fire = BlockPos::new(87, FLOOR_Y + 1, 87);
    changes.push((fire, Block::Fire.default_state()));
    set_blocks(&region, changes).await;

    let mut burnt = false;
    for _ in 0..400 {
        world.ticks(5).await;
        let burning = count_blocks(&region, &above, Block::Fire).await > 0;
        if count_blocks(&region, &floor, Block::OakPlanks).await <= 40 && burning {
            burnt = true;
            break;
        }
        if !burning {
            let mut left = None;
            for pos in &floor {
                if block_at(&region, *pos).await.map(|s| s.block()) == Some(Block::OakPlanks) {
                    left = Some(*pos);
                    break;
                }
            }
            if let Some(pos) = left {
                let relit = BlockPos::new(pos.x, FLOOR_Y + 1, pos.z);
                set_blocks(&region, vec![(relit, Block::Fire.default_state())]).await;
            }
        }
    }
    assert!(burnt, "the fire spreads and burns planks away");

    world.weather.send_modify(|weather| {
        weather.raining = true;
        weather.rain_level = 1.0;
    });
    let mut out = false;
    for _ in 0..200 {
        world.ticks(5).await;
        if count_blocks(&region, &above, Block::Fire).await == 0 {
            out = true;
            break;
        }
    }
    assert!(out, "rain puts the fire out");
    assert!(
        !saw_fire_extinguish_sound(&mut events, &above),
        "rain-doused fire is silent in vanilla, unlike a player breaking it by hand"
    );
    world.shutdown().await;
}

/// A fire block with no flammable neighbour and nothing sturdy under it
/// goes out on its very first scheduled tick (`FireBlock.tick`'s
/// `!isValidFireLocation` branch) -- deterministic, unlike the random
/// neighbour burnout odds, and just as silent as the rain case above.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_burns_out_on_its_own_without_playing_a_sound() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_rule(
        &world,
        "fire_spread_radius_around_player",
        RuleValue::Int(-1),
    );
    let (player, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;

    // Nothing sturdy underneath and no flammable neighbours: the fire is not
    // a valid location, so it is removed on its very first scheduled tick
    // regardless of age.
    let fire = BlockPos::new(87, FLOOR_Y, 87);
    set_blocks(&region, vec![(fire, Block::Fire.default_state())]).await;

    wait_for_blocks(
        &world,
        &region,
        &[fire],
        400,
        "the unsupported fire burns out on its own",
        |states| tuff_world_model::chunk::is_air(states[0]),
    )
    .await;
    assert!(!saw_fire_extinguish_sound(&mut events, &[fire]));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bone_meal_on_grass_grows_grass_and_flowers() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    let lawn = square((80, 80), (95, 95), FLOOR_Y);
    set_blocks(
        &region,
        lawn.iter()
            .map(|pos| (*pos, Block::GrassBlock.default_state()))
            .collect(),
    )
    .await;
    world.ticks(2).await;

    let target = BlockPos::new(88, FLOOR_Y, 88);
    for sequence in 1..=4 {
        region
            .request(RegionRequest::Place(place(
                &player,
                target,
                Item::BoneMeal,
                sequence,
            )))
            .await;
        let outcome = expect(&mut events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: s,
                outcome,
            } if s == sequence => Some(outcome),
            _ => None,
        })
        .await;
        assert!(matches!(outcome, ActionOutcome::Placed { .. }));
        set_blocks(
            &region,
            vec![(
                BlockPos::new(88, FLOOR_Y + 1, 88),
                Block::Air.default_state(),
            )],
        )
        .await;
    }
    let tops = square((80, 80), (95, 95), FLOOR_Y + 1);
    let grass = count_blocks(&region, &tops, Block::ShortGrass).await;
    let mut flowers = 0;
    for pos in &tops {
        if let Some(state) = block_at(&region, *pos).await
            && tuff_vanilla_data::tag::block::SMALL_FLOWERS.contains(&state.block())
        {
            flowers += 1;
        }
    }
    assert!(grass > 10, "{grass} grass");
    assert!(flowers > 0, "no flowers");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crops_need_light_to_be_planted_and_to_stay() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;

    let dark = BlockPos::new(86, FLOOR_Y, 86);
    let lit = BlockPos::new(90, FLOOR_Y, 90);
    let mut changes = Vec::new();
    for ground in [dark, lit] {
        changes.push((ground, Block::Farmland.default_state()));
    }
    for pos in square((85, 85), (87, 87), FLOOR_Y + 2) {
        changes.push((pos, Block::Stone.default_state()));
    }
    for pos in square((85, 85), (87, 87), FLOOR_Y + 1) {
        if (pos.x, pos.z) != (86, 86) {
            changes.push((pos, Block::Stone.default_state()));
        }
    }
    for pos in square((85, 85), (87, 87), FLOOR_Y) {
        if pos != dark {
            changes.push((pos, Block::Stone.default_state()));
        }
    }
    for pos in square((85, 85), (87, 87), FLOOR_Y - 1) {
        changes.push((pos, Block::Stone.default_state()));
    }
    set_blocks(&region, changes).await;
    world.ticks(5).await;

    for (sequence, ground) in [(1, dark), (2, lit)] {
        region
            .request(RegionRequest::Place(place(
                &player,
                ground,
                Item::WheatSeeds,
                sequence,
            )))
            .await;
        let outcome = expect(&mut events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: s,
                outcome,
            } if s == sequence => Some(outcome),
            _ => None,
        })
        .await;
        let planted = matches!(outcome, ActionOutcome::Placed { .. });
        assert_eq!(planted, ground == lit, "seeds at {ground:?}");
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn growth_trips_observers_and_covers_farmland() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;

    let moist = Block::Farmland.default_state().with_moisture(7).unwrap();
    let wheat = BlockPos::new(84, FLOOR_Y + 1, 84);
    let observer = BlockPos::new(85, FLOOR_Y + 1, 84);
    let stem = BlockPos::new(88, FLOOR_Y + 1, 88);
    let fruit = BlockPos::new(89, FLOOR_Y + 1, 88);
    let water = BlockPos::new(88, FLOOR_Y, 90);
    let mut changes: Vec<_> = square((83, 83), (91, 91), FLOOR_Y)
        .into_iter()
        .map(|pos| (pos, moist))
        .collect();
    changes.push((water, Block::Water.default_state()));
    changes.push((wheat, Block::Wheat.default_state()));
    changes.push((observer, "minecraft:observer[facing=west]".parse().unwrap()));
    changes.push((
        stem,
        Block::PumpkinStem.default_state().with_age(7).unwrap(),
    ));
    for blocked in [
        BlockPos::new(87, FLOOR_Y + 1, 88),
        BlockPos::new(88, FLOOR_Y + 1, 87),
        BlockPos::new(88, FLOOR_Y + 1, 89),
    ] {
        changes.push((blocked, Block::Stone.default_state()));
    }
    set_blocks(&region, changes).await;
    world.ticks(5).await;
    assert_eq!(
        block_at(&region, observer).await.unwrap().powered(),
        Some(false)
    );

    region
        .request(RegionRequest::Place(place(
            &player,
            wheat,
            Item::BoneMeal,
            1,
        )))
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(outcome, ActionOutcome::Placed { .. }));
    assert!(block_at(&region, wheat).await.unwrap().age() > Some(0));
    let mut pulsed = false;
    for _ in 0..6 {
        world.ticks(1).await;
        pulsed |= block_at(&region, observer).await.unwrap().powered() == Some(true);
    }
    assert!(pulsed, "the observer sees the wheat grow");

    set_rule(&world, "random_tick_speed", RuleValue::Int(500));
    wait_for_blocks(
        &world,
        &region,
        &[fruit],
        3000,
        "the stem grows a pumpkin",
        |states| states[0].block() == Block::Pumpkin,
    )
    .await;
    world.ticks(2).await;
    assert_eq!(
        block_at(&region, BlockPos::new(89, FLOOR_Y, 88)).await,
        Some(Block::Dirt.default_state()),
        "the pumpkin covers its farmland"
    );
    let under_stem = block_at(&region, BlockPos::new(88, FLOOR_Y, 88))
        .await
        .unwrap();
    assert_eq!(under_stem, moist, "the stem keeps its moist farmland");
    world.shutdown().await;
}

/// Fire that loses its floor goes out without the break effect other
/// blocks show when a shape update removes them.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_losing_its_floor_shows_no_break_particles() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;

    let floor = BlockPos::new(87, FLOOR_Y, 87);
    let fire = BlockPos::new(87, FLOOR_Y + 1, 87);
    set_blocks(
        &region,
        vec![
            (floor, Block::Stone.default_state()),
            (fire, Block::Fire.default_state()),
        ],
    )
    .await;
    world.ticks(2).await;
    while events.try_recv().is_ok() {}

    set_blocks(&region, vec![(floor, Block::Air.default_state())]).await;
    wait_for_blocks(
        &world,
        &region,
        &[fire],
        20,
        "the fire goes out",
        |states| tuff_world_model::chunk::is_air(states[0]),
    )
    .await;
    let breaking = framed(&play::LevelEvent {
        event: play::level_event::BLOCK_BREAK,
        position: fire,
        data: Block::Fire.default_state().id() as i32,
        global: false,
    });
    let mut shown = false;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            shown |= packets.contains(&breaking);
        }
    }
    assert!(!shown, "fire shows no break particles when it goes out");
    world.shutdown().await;
}

/// Fire next to a region border burns the planks on the far side too.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_burns_planks_across_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    set_rule(
        &world,
        "fire_spread_radius_around_player",
        RuleValue::Int(-1),
    );
    let (player, mut events) = PlayerHandle::new(1000);
    let west = world.region(ChunkPos::new(31, 5).region()).await;
    let east = world.region(ChunkPos::new(32, 5).region()).await;
    for (region, xs) in [(&west, [30, 31]), (&east, [32, 33])] {
        let chunks: Vec<ChunkPos> = xs
            .iter()
            .flat_map(|x| (4..=6).map(move |z| ChunkPos::new(*x, z)))
            .collect();
        subscribe(region, &player, &mut events, &chunks).await;
    }

    let far_side = square((512, 84), (515, 90), FLOOR_Y);
    let mut changes: Vec<_> = square((508, 84), (515, 90), FLOOR_Y)
        .into_iter()
        .map(|pos| (pos, Block::OakPlanks.default_state()))
        .collect();
    changes.push((
        BlockPos::new(511, FLOOR_Y + 1, 87),
        Block::Fire.default_state(),
    ));
    let (near, far): (Vec<_>, Vec<_>) = changes.into_iter().partition(|(pos, _)| pos.x < 512);
    set_blocks(&west, near).await;
    set_blocks(&east, far).await;

    let mut burnt = false;
    for _ in 0..400 {
        world.ticks(5).await;
        if count_blocks(&east, &far_side, Block::OakPlanks).await < far_side.len() {
            burnt = true;
            break;
        }
    }
    assert!(
        burnt,
        "the fire burns planks owned by the neighbouring region"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunks_players_watch_count_their_inhabited_time_and_save_it() {
    let dir = TestDir::new();
    let center = ChunkPos::new(8, 8);
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    set_blocks(
        &region,
        vec![(
            BlockPos::new(8 * 16 + 1, FLOOR_Y, 8 * 16 + 1),
            Block::Stone.default_state(),
        )],
    )
    .await;
    world.ticks(40).await;
    world.shutdown().await;
    let root = stored_chunk(dir.path(), center, false).expect("chunk saved");
    assert!(root.get_long("InhabitedTime").unwrap_or(0) >= 20);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lowered_block_tick_cap_spreads_a_burst_over_later_ticks() {
    const CAP: usize = 3;
    const COUNT: usize = 12;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text(
            "tuff.world.scheduledTicks.maxBlockPerTick",
            &CAP.to_string(),
        )
        .unwrap();
    let (player, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    world.ticks(5).await;

    let y = FLOOR_Y + 20;
    let row: Vec<BlockPos> = (0..COUNT as i32)
        .map(|i| BlockPos::new(82 + i, y, 86))
        .collect();
    let sand = Block::Sand.default_state();
    set_blocks(&region, row.iter().map(|pos| (*pos, sand)).collect()).await;

    let mut previous = COUNT;
    let mut fell_in_one_tick = 0;
    let mut ticks_taken = 0;
    while previous > 0 && ticks_taken < 60 {
        world.ticks(1).await;
        ticks_taken += 1;
        let left = count_blocks(&region, &row, Block::Sand).await;
        fell_in_one_tick = fell_in_one_tick.max(previous - left);
        previous = left;
    }
    assert_eq!(previous, 0, "every queued sand block started falling");
    assert!(
        fell_in_one_tick <= CAP,
        "{fell_in_one_tick} ticks ran in one game tick, over the cap {CAP}"
    );
    assert!(
        ticks_taken >= COUNT / CAP,
        "the burst finished in {ticks_taken} ticks, faster than the cap allows"
    );
}
