use super::super::predict::Keys;
use super::super::predict::sim::{Client, walking};
use super::super::{ItemUse, NoSlow, facts};
use super::*;
use crate::trace::Tags;
use tuff_core::protocol::play::Movement;

const GROUND_ACCEL: f64 = 0.1;

fn eating(item: Item) -> Option<ItemUse> {
    Some(ItemUse {
        item,
        elapsed: 0,
        remaining: Some(32),
    })
}

/// Feeds moves of the given lengths along x; `use_from` is the move
/// the item use starts on and `air` says which moves are airborne.
fn run(steps: &[f64], use_from: usize, item: Item, air: impl Fn(usize) -> bool) -> Vec<Fix> {
    run_on(BLOCK_FRICTION, None, steps, use_from, item, air)
}

fn run_on(
    friction: f32,
    keys: Option<u8>,
    steps: &[f64],
    use_from: usize,
    item: Item,
    air: impl Fn(usize) -> bool,
) -> Vec<Fix> {
    let mut check = NoSlow::default();
    let mut f = facts();
    let mut fixes = Vec::new();
    if let Some(keys) = keys {
        check.examine(&f, &Serverbound::PlayerInput(keys));
    }
    for (n, d) in steps.iter().enumerate() {
        f.using = n >= use_from;
        f.item_use = if f.using { eating(item) } else { None };
        f.sprinting = n < use_from;
        f.terrain = Some(Terrain {
            known: true,
            supported: !air(n),
            slippery: friction > 0.9,
            bouncy: friction == 0.8,
            friction,
            grip: friction,
            traction: friction,
            ..Terrain::default()
        });
        f.tick = n as u64;
        let target = [f.position[0] + d, f.position[1], f.position[2]];
        let packet = Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some(target),
            rotation: None,
            on_ground: !air(n),
        });
        if let Some(flag) = check.examine(&f, &packet) {
            fixes.push(flag.fix);
        }
        f.position = target;
    }
    fixes
}

fn vanilla(start: f64, moves: usize, friction: f64, accel: f64) -> Vec<f64> {
    let mut h = start;
    (0..moves)
        .map(|_| {
            h = h * friction + accel * USE_INPUT_SCALE * 0.98;
            h
        })
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn eating_after_a_sprint_slows_down_without_a_flag() {
    let mut steps = vec![0.28; 20];
    steps.extend(vanilla(0.28, 100, GROUND_FRICTION, GROUND_ACCEL));
    assert_eq!(run(&steps, 20, Item::CookedBeef, |_| false), Vec::new());
    let mut hop = vec![0.28; 20];
    hop.push(0.48);
    hop.extend(vanilla(
        0.48 * GROUND_FRICTION / AIR_FRICTION,
        11,
        AIR_FRICTION,
        AIR_ACCEL,
    ));
    hop.extend(vanilla(0.1, 60, GROUND_FRICTION, GROUND_ACCEL));
    assert_eq!(
        run(&hop, 21, Item::Bow, |n| (20..32).contains(&n)),
        Vec::new()
    );
}

#[tuff_core::watchdog]
#[test]
fn eating_on_ice_and_slime_slows_down_without_a_flag_and_cheats_are_caught() {
    // Slime keeps at most 0.42 of the momentum it is walked on with,
    // which the replay does not model, so only the reported keys bound
    // a slowed walk there.
    for (friction, carry) in [(0.8_f32, 0.42), (0.98, 1.0), (0.989, 1.0)] {
        let keep = carry * f64::from(friction) * AIR_FRICTION;
        let forward = Some(super::super::predict::Keys::FORWARD);
        let accel = 0.98 * grip_scale(friction);
        let pace = |from: f64, moves: usize, input: f64| {
            let mut h = from;
            (0..moves)
                .map(|_| {
                    h = h * keep + input * accel;
                    h
                })
                .collect::<Vec<_>>()
        };
        let mut steps = pace(0.0, 40, GROUND_ACCEL * SPRINT_FACTOR);
        let sprint = steps[39];
        let slowed = pace(sprint, 100, GROUND_ACCEL * USE_INPUT_SCALE);
        steps.extend(&slowed);
        assert_eq!(
            run_on(friction, forward, &steps, 40, Item::CookedBeef, |_| false),
            Vec::new(),
            "friction {friction}"
        );
        steps.truncate(40);
        if carry < 1.0 {
            steps.extend(pace(sprint, 100, GROUND_ACCEL));
        } else {
            steps.extend(slowed.iter().map(|d| d * 1.15));
        }
        let fixes = run_on(friction, forward, &steps, 40, Item::CookedBeef, |_| false);
        assert_eq!(fixes.first(), Some(&Fix::ResyncUse), "friction {friction}");
    }
}

#[tuff_core::watchdog]
#[test]
fn walking_acceleration_scales_only_above_the_default_friction() {
    assert_eq!(grip_scale(0.6), 1.0);
    assert_eq!(grip_scale(0.4), 1.0);
    assert!((grip_scale(0.8) - 0.421_875).abs() < 1e-6);
    assert!((grip_scale(0.98) - 0.229_497).abs() < 1e-5);
}

#[tuff_core::watchdog]
#[test]
fn a_steady_six_percent_over_the_slowed_pace_is_caught() {
    let mut steps = vec![0.28; 20];
    let slowed = vanilla(0.28, 100, GROUND_FRICTION, GROUND_ACCEL);
    steps.extend(slowed.iter().map(|h| h * 1.06));
    let fixes = run(&steps, 20, Item::CookedBeef, |_| false);
    assert_eq!(fixes.first(), Some(&Fix::ResyncUse), "{fixes:?}");
}

#[tuff_core::watchdog]
#[test]
fn slowed_pace_from_rest_is_legit() {
    let steps: Vec<f64> = (0..40)
        .map(|n| (0..=n).fold(0.0, |h, _| h * 0.546 + 0.0196))
        .collect();
    assert_eq!(run(&steps, 0, Item::Shield, |_| false), Vec::new());
}

#[tuff_core::watchdog]
#[test]
fn walking_under_the_flat_limit_while_eating_is_caught() {
    let mut steps = vec![0.2; 10];
    steps.extend([0.13; 15]);
    let fixes = run(&steps, 10, Item::CookedBeef, |_| false);
    assert_eq!(fixes.first(), Some(&Fix::ResyncUse), "{fixes:?}");
}

#[tuff_core::watchdog]
#[test]
fn the_no_slow_trace_tags_the_prediction_and_the_resync() {
    let mut check = NoSlow::default();
    let mut f = facts();
    f.terrain = Some(Terrain {
        known: true,
        supported: true,
        ..Terrain::default()
    });
    let step = |x: f64| {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some([x, 64.0, 0.0]),
            rotation: None,
            on_ground: true,
        })
    };
    check.examine(&f, &step(0.1));
    f.using = true;
    f.item_use = eating(Item::Bread);
    let mut resync = None;
    for n in 0..20 {
        f.position[0] = 0.1 + f64::from(n) * 0.13;
        let flag = check.examine(&f, &step(f.position[0] + 0.13));
        let trace = check.trace().unwrap();
        assert!(trace.tags.contains(Tags::USE_MOMENTUM));
        if flag.is_some_and(|flag| flag.fix == Fix::ResyncUse) {
            resync = Some(trace);
            break;
        }
    }
    let trace = resync.expect("resynced");
    assert!(trace.tags.contains(Tags::ITEM_RESYNC));
    assert!(trace.actual > trace.allowed);
    f.terrain = None;
    check.examine(&f, &step(f.position[0] + 0.05));
    assert!(check.trace().unwrap().tags.contains(Tags::USE_FLAT));
}

#[tuff_core::watchdog]
#[test]
fn a_few_percent_over_the_slowed_step_is_caught() {
    let mut steps = vec![0.28; 20];
    let slowed = vanilla(0.28, 100, GROUND_FRICTION, GROUND_ACCEL);
    steps.extend(slowed.iter().map(|d| d * 1.15));
    let fixes = run(&steps, 20, Item::CookedBeef, |_| false);
    assert!(!fixes.is_empty(), "{fixes:?}");
}

#[tuff_core::watchdog]
#[test]
fn a_spear_keeps_full_speed() {
    let steps = vec![0.13; 80];
    assert_eq!(run(&steps, 10, Item::IronSpear, |_| false), Vec::new());
}

/// Feeds a simulated client's packets, eating bread on the ground,
/// and returns the tags of every judged move.
fn replay(client: &mut Client, sneak: f32, plan: &[(u8, u32)]) -> Tags {
    let mut check = NoSlow::default();
    let mut f = facts();
    f.using = true;
    f.item_use = eating(Item::Bread);
    f.sneaking = sneak < 1.0;
    f.rotation = [-90.0, 0.0];
    f.position = [client.x, 64.0, client.z];
    f.terrain = Some(Terrain {
        known: true,
        supported: true,
        ..Terrain::default()
    });
    let regime = walking(0.2, sneak, -90.0);
    let mut tags = Tags::empty();
    for &(keys, ticks) in plan {
        assert_eq!(check.examine(&f, &Serverbound::PlayerInput(keys)), None);
        for _ in 0..ticks {
            if client.tick(Keys(keys), &regime).is_some() {
                let target = [client.x, 64.0, client.z];
                let packet = Serverbound::Move(Movement {
                    horizontal_collision: false,
                    position: Some(target),
                    rotation: None,
                    on_ground: true,
                });
                assert_eq!(check.examine(&f, &packet), None);
                tags = tags.union(check.trace().unwrap().tags);
                f.position = target;
                f.tick += 1;
            }
            check.examine(&f, &Serverbound::ClientTickEnd);
        }
    }
    tags
}

#[tuff_core::watchdog]
#[test]
fn eating_while_stopping_starting_and_sneaking_at_an_edge_is_predicted() {
    let walk = Keys::FORWARD;
    let tags = replay(&mut Client::new(), 1.0, &[(walk, 30), (0, 30), (walk, 10)]);
    assert!(tags.contains(Tags::HIDDEN_TICKS), "{tags}");
    let mut client = Client::new();
    client.edge = Some(0.4);
    let along = Keys::FORWARD | Keys::LEFT | Keys::SNEAK;
    let tags = replay(&mut client, 0.3, &[(along, 60), (Keys::SNEAK, 30)]);
    assert!(tags.contains(Tags::EDGE_BACKOFF), "{tags}");
    assert!(
        client.x <= 0.4 && client.z < -0.3,
        "{} {}",
        client.x,
        client.z
    );
}
