//! Minecarts in a world with the minecart improvements feature: they sit
//! 0.1 above the rail, viewers get their track steps, and the
//! `max_minecart_speed` game rule caps how fast powered rails get them.
//! A world without the feature keeps the old movement.

use super::*;
use tuff_core::world::game_rules::RuleValue;
use tuff_vanilla_data::packet::play::Clientbound;
use tuff_vanilla_data::registry::EntityType;

const FEATURE_PACK: &str = "minecart_improvements";

fn motion(x: f64) -> Compound {
    Compound::new().with(
        "Motion",
        vec![Tag::Double(x), Tag::Double(0.0), Tag::Double(0.0)],
    )
}

async fn cart(b: &Bench) -> tuff_core::world::messages::EntitySnapshot {
    let mut found = mobs(&b.region, EntityType::Minecart).await;
    assert_eq!(found.len(), 1, "{found:#?}");
    found.remove(0)
}

async fn world(dir: &Path, improved: bool, x: (i32, i32)) -> Bench {
    let world = if improved {
        TestWorld::start_with_packs(dir, &[FEATURE_PACK])
    } else {
        TestWorld::start(dir)
    };
    let centers = [
        ChunkPos::new(5, 5),
        ChunkPos::new(6, 5),
        ChunkPos::new(7, 5),
    ];
    bench_in(world, &centers, x, (84, 88)).await
}

/// Whether any movement report sent to the bench's viewer so far carried
/// track steps.
fn saw_track_steps(b: &mut Bench) -> bool {
    let id = Clientbound::MoveMinecartAlongTrack.id();
    let mut seen = false;
    while let Ok(event) = b.events.try_recv() {
        if let PlayerEvent::Track(track) = event {
            let frames = track
                .update
                .iter()
                .chain(track.moves.iter().flat_map(|m| m.iter()));
            seen |= frames.into_iter().any(|f| framed_packet_id(f) == id);
        }
    }
    seen
}

async fn roll_on_flat_track(improved: bool) -> (f64, f64, bool) {
    let dir = TestDir::new();
    let mut b = world(dir.path(), improved, (80, 95)).await;
    let rails: Vec<(BlockPos, &str)> = (81..=94)
        .map(|x| (at(x, 1, 86), "minecraft:rail[shape=east_west]"))
        .collect();
    set(&b.region, &rails).await;
    while b.events.try_recv().is_ok() {}
    let start = [83.5, f64::from(FLOOR_Y + 1) + 0.0625, 86.5];
    summon(&b.region, EntityType::Minecart, motion(0.2), start).await;
    b.world.ticks(10).await;
    let cart = cart(&b).await;
    let seen = saw_track_steps(&mut b);
    b.world.shutdown().await;
    (cart.position[0], cart.position[1], seen)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_improved_cart_rides_a_tenth_above_the_rail_and_reports_its_steps() {
    let (x, y, seen) = roll_on_flat_track(true).await;
    assert!(x > 84.5, "rolled east: {x}");
    assert!((y - (f64::from(FLOOR_Y + 1) + 0.1)).abs() < 1.0e-9, "{y}");
    assert!(seen, "viewers got Move Minecart Along Track");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_feature_a_cart_keeps_the_old_movement() {
    let (x, y, seen) = roll_on_flat_track(false).await;
    assert!(x > 84.5, "rolled east: {x}");
    assert!(
        (y - (f64::from(FLOOR_Y + 1) + 0.0625)).abs() < 1.0e-9,
        "{y}"
    );
    assert!(!seen, "no track steps for the old movement");
}

/// How far a cart boosted along a powered line gets in 25 ticks.
async fn boosted_distance(improved: bool, max_speed: Option<i32>) -> f64 {
    let dir = TestDir::new();
    let b = world(dir.path(), improved, (80, 127)).await;
    if let Some(speed) = max_speed {
        b.world.game_rules.send_modify(|rules| {
            rules
                .set("max_minecart_speed", RuleValue::Int(speed))
                .unwrap()
        });
    }
    let state: tuff_vanilla_data::BlockState =
        "minecraft:powered_rail[shape=east_west,powered=true]"
            .parse()
            .unwrap();
    let rails = (81..=126).map(|x| (at(x, 1, 86), state)).collect();
    set_blocks(&b.region, rails, true).await;
    let start = [82.5, f64::from(FLOOR_Y + 1) + 0.0625, 86.5];
    summon(&b.region, EntityType::Minecart, motion(0.1), start).await;
    b.world.ticks(25).await;
    let x = cart(&b).await.position[0];
    b.world.shutdown().await;
    x - start[0]
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_game_rule_lets_powered_rails_take_an_improved_cart_past_the_old_cap() {
    let capped = boosted_distance(true, None).await;
    assert!(capped <= 10.5, "8 blocks a second by default: {capped}");
    let fast = boosted_distance(true, Some(40)).await;
    assert!(fast > 12.0, "40 blocks a second: {fast}");
    let old = boosted_distance(false, None).await;
    assert!(old <= 10.5, "the old cap: {old}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cart_passing_a_detector_rail_mid_tick_presses_it() {
    let dir = TestDir::new();
    let b = world(dir.path(), true, (80, 127)).await;
    b.world.game_rules.send_modify(|rules| {
        rules
            .set("max_minecart_speed", RuleValue::Int(200))
            .unwrap()
    });
    let rails: Vec<(BlockPos, tuff_vanilla_data::BlockState)> = (81..=126)
        .map(|x| {
            let rail = if x == 84 {
                "minecraft:detector_rail[shape=east_west]"
            } else {
                "minecraft:rail[shape=east_west]"
            };
            (at(x, 1, 86), rail.parse().unwrap())
        })
        .collect();
    set_blocks(&b.region, rails, true).await;
    let start = [82.5, f64::from(FLOOR_Y + 1) + 0.0625, 86.5];
    summon(&b.region, EntityType::Minecart, motion(2.5), start).await;
    b.world.ticks(2).await;
    let x = cart(&b).await.position[0];
    assert!(x > 86.0, "the cart is past the detector: {x}");
    assert_eq!(state(&b.region, at(84, 1, 86)).await.powered(), Some(true));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fast_improved_cart_picks_up_a_mob_it_swept_past_in_one_tick() {
    let dir = TestDir::new();
    let b = world(dir.path(), true, (80, 127)).await;
    b.world.game_rules.send_modify(|rules| {
        rules
            .set("max_minecart_speed", RuleValue::Int(200))
            .unwrap()
    });
    let rails: Vec<(BlockPos, tuff_vanilla_data::BlockState)> = (81..=126)
        .map(|x| {
            (
                at(x, 1, 86),
                "minecraft:rail[shape=east_west]".parse().unwrap(),
            )
        })
        .collect();
    set_blocks(&b.region, rails, true).await;
    let y = f64::from(FLOOR_Y + 1);
    summon(
        &b.region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [86.5, y, 86.5],
    )
    .await;
    summon(
        &b.region,
        EntityType::Minecart,
        motion(2.5),
        [82.5, y + 0.0625, 86.5],
    )
    .await;
    b.world.ticks(2).await;
    let cart = cart(&b).await;
    assert!(cart.position[0] > 87.0, "past the pig: {:?}", cart.position);
    let carried = vehicle_of_at(&b.region, EntityType::Pig, at(86, 1, 86)).await;
    assert_eq!(carried, Some(cart.id));
    b.world.shutdown().await;
}
