use std::sync::Arc;

use tuff_core::world::fake::motion::{Body, FakeMotion, Held, MotionState};
use tuff_core::world::physics::Movement;
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_feature_builtin_vanilla_players::GroundMotion;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::pos::ChunkPos;

const FLOOR: i32 = 64;

struct Still;

impl tuff_core::world::fluid_api::FluidBehaviors for Still {}

fn body() -> Body<'static> {
    Body {
        movement: Movement {
            speed: 0.1,
            gravity: 0.08,
            step_height: 0.6,
            jump_strength: 0.42,
            water_efficiency: 0.0,
            movement_efficiency: 0.0,
            friction_modifier: 1.0,
            air_drag_modifier: 1.0,
        },
        food: 20,
        glider: true,
        boosting: false,
        fluids: &Still,
    }
}

struct World {
    chunks: Vec<(ChunkPos, Chunk)>,
}

impl World {
    fn flat() -> Self {
        let mut chunks = Vec::new();
        for x in -4..4 {
            for z in -4..4 {
                chunks.push((ChunkPos::new(x, z), Chunk::empty(ChunkShape::OVERWORLD)));
            }
        }
        let mut world = World { chunks };
        world.fill(-64, 0, 64, 192, |_| Block::Stone.default_state());
        world
    }

    fn fill(&mut self, x0: i32, z0: i32, x1: i32, z1: i32, state: impl Fn(i32) -> BlockState) {
        for x in x0..x1.min(64) {
            for z in z0.max(-64)..z1 {
                for y in 0..FLOOR {
                    self.set(x, y, z, state(y));
                }
            }
        }
    }

    fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        let pos = ChunkPos::of_block(tuff_world_model::BlockPos::new(x, y, z));
        if let Some((_, chunk)) = self.chunks.iter_mut().find(|(p, _)| *p == pos) {
            chunk.set(x, y, z, state);
        }
    }

    fn mirror(self) -> ChunkMirror {
        let mut mirror = ChunkMirror::default();
        for (pos, chunk) in self.chunks {
            mirror.insert(pos, Arc::new(chunk));
        }
        mirror
    }
}

fn run(
    world: &ChunkMirror,
    start: [f64; 3],
    held: Held,
    ticks: usize,
) -> (MotionState, Vec<[f64; 3]>) {
    let mut state = MotionState::new(start);
    let mut path = Vec::new();
    for _ in 0..ticks {
        GroundMotion.tick(&mut state, &held, &body(), world);
        path.push(state.position);
    }
    (state, path)
}

fn forward() -> Held {
    Held {
        forward: 1,
        ..Held::default()
    }
}

fn pace(path: &[[f64; 3]]) -> f64 {
    let n = path.len();
    let (a, b) = (path[n - 2], path[n - 1]);
    (b[2] - a[2]).hypot(b[0] - a[0])
}

#[tuff_core::watchdog]
#[test]
fn walking_and_sprinting_reach_their_terminal_speeds() {
    let world = World::flat().mirror();
    let start = [0.5, f64::from(FLOOR), 0.5];
    let (_, walk) = run(&world, start, forward(), 40);
    assert!((pace(&walk) - 0.2158).abs() < 1.0e-3, "{}", pace(&walk));
    let sprint = Held {
        sprint: true,
        ..forward()
    };
    let (state, run_path) = run(&world, start, sprint, 40);
    assert!(state.sprinting);
    assert!(
        (pace(&run_path) - 0.2806).abs() < 1.0e-3,
        "{}",
        pace(&run_path)
    );
}

#[tuff_core::watchdog]
#[test]
fn a_sprint_jump_covers_more_ground_and_lands_after_twelve_ticks() {
    let world = World::flat().mirror();
    let start = [0.5, f64::from(FLOOR), 0.5];
    let mut state = MotionState::new(start);
    let sprint = Held {
        sprint: true,
        ..forward()
    };
    for _ in 0..20 {
        GroundMotion.tick(&mut state, &sprint, &body(), &world);
    }
    let from = state.position;
    let jump = Held {
        jump: true,
        ..sprint
    };
    let mut peak: f64 = 0.0;
    let mut air = 0;
    loop {
        GroundMotion.tick(&mut state, &jump, &body(), &world);
        peak = peak.max(state.position[1] - f64::from(FLOOR));
        if state.on_ground {
            break;
        }
        air += 1;
    }
    assert!((peak - 1.2522).abs() < 1.0e-3, "{peak}");
    assert_eq!(air, 11);
    let jumped = state.position[2] - from[2];
    assert!(jumped > 12.0 * 0.2806 && jumped < 3.8, "{jumped}");
}

#[tuff_core::watchdog]
#[test]
fn it_steps_up_onto_a_slab() {
    let mut world = World::flat();
    for x in -2..3 {
        for z in 4..30 {
            world.set(x, FLOOR, z, Block::StoneSlab.default_state());
        }
    }
    let world = world.mirror();
    let start = [0.5, f64::from(FLOOR), 0.5];
    let (state, _) = run(&world, start, forward(), 40);
    assert!(state.on_ground);
    assert!(state.position[2] > 5.0, "{:?}", state.position);
    assert!((state.position[1] - (f64::from(FLOOR) + 0.5)).abs() < 1.0e-6);
}

#[tuff_core::watchdog]
#[test]
fn sneaking_stops_at_an_edge_and_walking_does_not() {
    let mut world = World::flat();
    world.fill(-64, 6, 64, 192, |_| Block::Air.default_state());
    let world = world.mirror();
    let start = [0.5, f64::from(FLOOR), 0.5];
    let sneak = Held {
        sneak: true,
        ..forward()
    };
    let (state, _) = run(&world, start, sneak, 120);
    assert!(state.on_ground);
    assert!(
        state.position[2] < 6.3 && state.position[2] > 6.0,
        "{:?}",
        state.position
    );
    let (state, _) = run(&world, start, forward(), 60);
    assert!(state.position[1] < f64::from(FLOOR) - 1.0);
}

#[tuff_core::watchdog]
#[test]
fn ice_glides_further_than_stone_after_the_keys_are_released() {
    let glide = |block: Block| {
        let mut world = World::flat();
        world.fill(-64, 0, 64, 192, |_| block.default_state());
        let world = world.mirror();
        let start = [0.5, f64::from(FLOOR), 0.5];
        let mut state = MotionState::new(start);
        for _ in 0..30 {
            GroundMotion.tick(&mut state, &forward(), &body(), &world);
        }
        let at = state.position[2];
        for _ in 0..60 {
            GroundMotion.tick(&mut state, &Held::default(), &body(), &world);
        }
        state.position[2] - at
    };
    let stone = glide(Block::Stone);
    let ice = glide(Block::Ice);
    assert!(stone < 0.6, "{stone}");
    assert!(ice > 5.0 * stone && ice > 1.2, "{ice} {stone}");
}

#[tuff_core::watchdog]
#[test]
fn a_fall_lands_when_the_free_fall_series_reaches_the_ground() {
    let world = World::flat().mirror();
    for height in [3.0, 10.0] {
        let start = [0.5, f64::from(FLOOR) + height, 0.5];
        let mut expected = 0;
        let (mut y, mut v) = (height, 0.0f64);
        loop {
            expected += 1;
            y += v;
            if y <= 0.0 {
                break;
            }
            v = (v - 0.08) * f64::from(0.98f32);
            if v.abs() < 0.003 {
                v = 0.0;
            }
        }
        let mut state = MotionState::new(start);
        let mut ticks = 0;
        while !state.on_ground {
            GroundMotion.tick(&mut state, &Held::default(), &body(), &world);
            ticks += 1;
            assert!(ticks < 100);
        }
        assert!(
            ticks == expected + 1 || ticks == expected || ticks + 1 == expected,
            "{height}: {ticks} vs {expected}"
        );
        assert!((state.position[1] - f64::from(FLOOR)).abs() < 1.0e-9);
    }
}

#[tuff_core::watchdog]
#[test]
fn knockback_carries_a_standing_body_a_fixed_distance() {
    let world = World::flat().mirror();
    let mut state = MotionState::new([0.5, f64::from(FLOOR), 0.5]);
    for _ in 0..3 {
        GroundMotion.tick(&mut state, &Held::default(), &body(), &world);
    }
    state.velocity = [0.4, 0.4, 0.0];
    let from = state.position[0];
    for _ in 0..60 {
        GroundMotion.tick(&mut state, &Held::default(), &body(), &world);
    }
    let carried = state.position[0] - from;
    assert!((carried - 2.064).abs() < 0.01, "{carried}");
}

fn fluid_world(fluid: Block, top: i32) -> ChunkMirror {
    let mut world = World::flat();
    for x in 0..8 {
        for z in 0..16 {
            for y in FLOOR..top {
                world.set(x, y, z, fluid.default_state());
            }
        }
    }
    world.mirror()
}

fn held_jump() -> Held {
    Held {
        jump: true,
        ..Held::default()
    }
}

#[tuff_core::watchdog]
#[test]
fn a_body_sinks_in_water_and_jumping_floats_it_to_the_surface() {
    let world = fluid_world(Block::Water, FLOOR + 4);
    let start = [0.5, f64::from(FLOOR + 2), 0.5];
    let (sunk, _) = run(&world, start, Held::default(), 120);
    assert!(sunk.on_ground && sunk.position[1] < f64::from(FLOOR) + 0.01);
    let (floated, _) = run(&world, start, held_jump(), 120);
    let surface = f64::from(FLOOR + 4);
    assert!(
        floated.position[1] > surface - 1.2 && floated.position[1] < surface + 0.5,
        "{}",
        floated.position[1]
    );
}

#[tuff_core::watchdog]
#[test]
fn sprinting_under_water_swims_flat_and_faster_than_wading() {
    let world = fluid_world(Block::Water, FLOOR + 5);
    let start = [0.5, f64::from(FLOOR) + 1.0, 0.5];
    let sprint = Held {
        sprint: true,
        ..forward()
    };
    let (state, path) = run(&world, start, sprint, 30);
    assert!(state.swimming && state.swim_pose && state.sprinting);
    assert!(state.position[2] > 3.0, "{}", state.position[2]);
    assert!(pace(&path) > 0.1);
    let (wading, _) = run(&world, start, forward(), 30);
    assert!(!wading.swimming);
    assert!(wading.position[2] < state.position[2]);
}

#[tuff_core::watchdog]
#[test]
fn lava_drags_hard_and_jumping_lifts_out_slowly() {
    let world = fluid_world(Block::Lava, FLOOR + 3);
    let start = [0.5, f64::from(FLOOR), 0.5];
    let (_, path) = run(&world, start, forward(), 20);
    assert!(path[19][2] < 1.5, "{}", path[19][2]);
    let (floated, _) = run(&world, start, held_jump(), 40);
    assert!(floated.position[1] > f64::from(FLOOR) + 1.0);
}

#[tuff_core::watchdog]
#[test]
fn a_ladder_climbs_with_jump_holds_with_sneak_and_slides_down() {
    let mut world = World::flat();
    for y in FLOOR..FLOOR + 10 {
        world.set(0, y, -1, Block::Stone.default_state());
        world.set(
            0,
            y,
            0,
            Block::Ladder
                .default_state()
                .with("facing", "south")
                .expect("facing"),
        );
    }
    let world = world.mirror();
    let start = [0.5, f64::from(FLOOR), 0.5];
    let (up, _) = run(&world, start, held_jump(), 30);
    assert!(
        up.position[1] > f64::from(FLOOR) + 3.0,
        "{}",
        up.position[1]
    );
    let climbing_rate = (up.position[1] - start[1]) / 30.0;
    assert!((climbing_rate - 0.1176).abs() < 0.01, "{climbing_rate}");
    let high = [0.5, f64::from(FLOOR) + 8.0, 0.5];
    let sneak = Held {
        sneak: true,
        ..Held::default()
    };
    let (held, _) = run(&world, high, sneak, 30);
    assert!((held.position[1] - high[1]).abs() < 0.01);
    let (slid, _) = run(&world, high, Held::default(), 20);
    let rate = (high[1] - slid.position[1]) / 20.0;
    assert!((rate - 0.15).abs() < 0.02, "{rate}");
}

#[tuff_core::watchdog]
#[test]
fn a_cobweb_and_powder_snow_hold_a_body_back() {
    let mut world = World::flat();
    for x in 0..2 {
        for z in 4..8 {
            world.set(x, FLOOR, z, Block::Cobweb.default_state());
            world.set(x, FLOOR + 1, z, Block::Cobweb.default_state());
        }
    }
    for x in 4..8 {
        for z in 0..8 {
            world.set(x, FLOOR - 2, z, Block::PowderSnow.default_state());
            world.set(x, FLOOR - 1, z, Block::PowderSnow.default_state());
        }
    }
    let world = world.mirror();
    let start = [0.5, f64::from(FLOOR), 0.5];
    let (webbed, _) = run(&world, start, forward(), 40);
    let (free, _) = run(&world, [2.5, f64::from(FLOOR), 0.5], forward(), 40);
    assert!(
        webbed.position[2] < free.position[2] - 3.0,
        "{} {}",
        webbed.position[2],
        free.position[2]
    );
    let (snowed, _) = run(
        &world,
        [5.5, f64::from(FLOOR) + 6.0, 0.5],
        Held::default(),
        80,
    );
    assert!(
        snowed.position[1] < f64::from(FLOOR) - 0.5,
        "{}",
        snowed.position[1]
    );
    assert!(snowed.position[1] > f64::from(FLOOR) - 2.2);
}

#[tuff_core::watchdog]
#[test]
fn a_bubble_column_lifts_a_body_up_through_the_water() {
    let mut world = World::flat();
    world.set(0, FLOOR - 1, 0, Block::SoulSand.default_state());
    for y in FLOOR..FLOOR + 12 {
        let state = if y == FLOOR + 11 {
            Block::Water.default_state()
        } else {
            Block::BubbleColumn
                .default_state()
                .with("drag", "false")
                .expect("drag")
        };
        world.set(0, y, 0, state);
    }
    let world = world.mirror();
    let (state, _) = run(&world, [0.5, f64::from(FLOOR), 0.5], Held::default(), 40);
    assert!(
        state.position[1] > f64::from(FLOOR) + 5.0,
        "{}",
        state.position[1]
    );
}

#[tuff_core::watchdog]
#[test]
fn a_jump_press_in_the_air_starts_a_glide_that_ends_on_landing_and_needs_an_elytra() {
    let world = World::flat().mirror();
    let start = [0.5, 100.0, 0.5];
    let mut state = MotionState::new(start);
    let dive = Held {
        pitch: 40.0,
        ..Held::default()
    };
    GroundMotion.tick(&mut state, &dive, &body(), &world);
    assert!(!state.gliding);
    let press = Held { jump: true, ..dive };
    GroundMotion.tick(&mut state, &press, &body(), &world);
    assert!(state.gliding);
    for _ in 0..200 {
        GroundMotion.tick(&mut state, &dive, &body(), &world);
        if state.on_ground {
            break;
        }
    }
    assert!(state.on_ground);
    assert!(state.position[2] > 10.0, "{:?}", state.position);
    GroundMotion.tick(&mut state, &dive, &body(), &world);
    assert!(!state.gliding);

    let mut bare = MotionState::new(start);
    let no_elytra = Body {
        glider: false,
        ..body()
    };
    GroundMotion.tick(&mut bare, &dive, &no_elytra, &world);
    GroundMotion.tick(&mut bare, &press, &no_elytra, &world);
    assert!(!bare.gliding);
}

#[tuff_core::watchdog]
#[test]
fn a_firework_boost_carries_a_level_glide_further() {
    let world = World::flat().mirror();
    let level = Held {
        pitch: 0.0,
        ..Held::default()
    };
    let glide = |boosting: bool| {
        let mut state = MotionState::new([0.5, 100.0, 0.5]);
        GroundMotion.tick(&mut state, &level, &body(), &world);
        GroundMotion.tick(
            &mut state,
            &Held {
                jump: true,
                ..level
            },
            &body(),
            &world,
        );
        assert!(state.gliding);
        let boosted = Body { boosting, ..body() };
        for _ in 0..30 {
            GroundMotion.tick(&mut state, &level, &boosted, &world);
        }
        state
    };
    let plain = glide(false);
    let boosted = glide(true);
    assert!(
        boosted.position[2] > plain.position[2] + 10.0,
        "{boosted:?} {plain:?}"
    );
    assert!(boosted.velocity[2] > 1.0, "{boosted:?}");
}

fn trajectory_digest() -> u64 {
    let mut edge = World::flat();
    edge.fill(-64, 6, 64, 192, |_| Block::Air.default_state());
    edge.fill(5, -64, 64, 192, |_| Block::Air.default_state());
    let edge = edge.mirror();
    let water = fluid_world(Block::Water, FLOOR + 4);
    let lava = fluid_world(Block::Lava, FLOOR + 3);
    let flat = World::flat().mirror();
    let mut held = Vec::new();
    for yaw in [-45.0f32, -30.0, 0.0, 20.0, 135.0] {
        for (sneak, sprint, jump, strafe) in [
            (true, false, false, 0),
            (true, false, false, -1),
            (false, true, false, 1),
            (false, true, true, 0),
            (false, false, true, -1),
        ] {
            held.push(Held {
                forward: 1,
                strafe,
                yaw,
                sneak,
                sprint,
                jump,
                ..Held::default()
            });
        }
    }
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    let starts = [
        (&edge, [0.5, f64::from(FLOOR), 0.5]),
        (&water, [0.5, f64::from(FLOOR) + 1.0, 0.5]),
        (&water, [0.5, f64::from(FLOOR + 3), 4.5]),
        (&lava, [0.5, f64::from(FLOOR), 0.5]),
        (&lava, [0.5, f64::from(FLOOR) + 2.5, 0.5]),
        (&flat, [0.5, f64::from(FLOOR), 0.5]),
    ];
    for (world, start) in starts {
        for h in &held {
            let (state, path) = run(world, start, *h, 80);
            for p in path.iter().chain([&state.velocity]) {
                for v in p {
                    digest = (digest ^ v.to_bits()).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
    }
    digest
}

#[tuff_core::watchdog]
#[test]
fn ground_water_lava_and_edge_trajectories_are_pinned() {
    assert_eq!(trajectory_digest(), 7_983_973_558_371_645_011);
}

#[tuff_core::watchdog]
#[test]
#[ignore = "timing"]
fn ground_motion_timing() {
    let start = std::time::Instant::now();
    for _ in 0..5 {
        std::hint::black_box(trajectory_digest());
    }
    eprintln!("ground_motion_timing_ms {}", start.elapsed().as_millis());
}

fn edge_run(gap: Option<(i32, i32)>, held: Held, ticks: usize) -> (MotionState, Vec<[f64; 3]>) {
    let mut world = World::flat();
    world.fill(-64, 6, 64, 192, |_| Block::Air.default_state());
    if let Some((from, to)) = gap {
        for z in from..to {
            for x in -64..64 {
                for y in 0..FLOOR {
                    world.set(x, y, z, Block::Stone.default_state());
                }
            }
        }
    }
    run(&world.mirror(), [0.5, f64::from(FLOOR), 0.5], held, ticks)
}

fn edge_held(sprint: bool) -> Held {
    Held {
        sprint,
        jump_at_edge: true,
        ..forward()
    }
}

#[tuff_core::watchdog]
#[test]
fn edge_jump_takes_off_only_when_the_move_leaves_the_block() {
    for sprint in [false, true] {
        let (_, path) = edge_run(None, edge_held(sprint), 80);
        let lift = path
            .iter()
            .position(|p| p[1] > f64::from(FLOOR) + 0.01)
            .expect("jumped");
        let before = path[lift - 1][2];
        assert!(
            before < 6.3 && path[lift][2] >= 6.3,
            "sprint {sprint}: {:?} -> {:?}",
            path[lift - 1],
            path[lift]
        );
        assert!(
            path[..lift]
                .iter()
                .all(|p| (p[1] - f64::from(FLOOR)).abs() < 1.0e-9)
        );
        assert!(path[lift - 1][2] + (path[lift - 1][2] - path[lift - 2][2]) > 5.9);
    }
}

#[tuff_core::watchdog]
#[test]
fn edge_jump_clears_a_three_block_gap_while_sprinting() {
    let (_, path) = edge_run(Some((9, 40)), edge_held(true), 70);
    assert!(path.iter().all(|p| p[1] >= f64::from(FLOOR) - 1.0e-9));
    assert!(path.iter().any(|p| p[2] > 12.0));
    let last = path.last().unwrap();
    assert!((last[1] - f64::from(FLOOR)).abs() < 1.0e-9, "{last:?}");
}

#[tuff_core::watchdog]
#[test]
fn edge_jump_never_fires_on_a_long_flat_floor() {
    let (_, path) = edge_run(Some((6, 192)), edge_held(true), 100);
    assert!(path.iter().all(|p| p[1] <= f64::from(FLOOR) + 1.0e-9));
}

#[tuff_core::watchdog]
#[test]
fn edge_jump_off_walks_off_the_block() {
    let held = Held {
        jump_at_edge: false,
        ..edge_held(true)
    };
    let (state, _) = edge_run(None, held, 60);
    assert!(state.position[1] < f64::from(FLOOR) - 1.0);
}
