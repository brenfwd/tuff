use super::*;
use tuff_vanilla_data::Block;

fn walker(position: [f64; 3]) -> Walker<'static> {
    Walker {
        position,
        bounds: [
            position[0] - 0.45,
            position[1],
            position[2] - 0.45,
            position[0] + 0.45,
            position[1] + 1.4,
            position[2] + 0.45,
        ],
        width: 0.9,
        height: 1.4,
        on_ground: true,
        max_up_step: 0.6,
        max_fall_distance: 3,
        can_float: true,
        floats_on: is_floatable,
        stands_on_fluid: stands_on_nothing,
        can_open_doors: false,
        can_pass_doors: true,
        can_walk_over_fences: false,
        in_floatable_fluid: false,
        malus: &[],
        min_y: -64,
        mode: PathMode::Walk,
        in_water: false,
        bound: None,
        capture_debug: false,
        frog_nodes: false,
        sea_level: 63,
    }
}

fn world(extra: impl Fn(BlockPos) -> Option<Block>) -> impl Fn(BlockPos) -> Option<BlockState> {
    move |pos: BlockPos| {
        if pos.x.abs() > 40 || pos.z.abs() > 40 {
            return None;
        }
        let block = extra(pos).unwrap_or(if pos.y < 64 {
            Block::GrassBlock
        } else {
            Block::Air
        });
        Some(block.default_state())
    }
}

fn search(
    from: [f64; 3],
    to: BlockPos,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Option<Path> {
    let mut budget = 10_000;
    find_path(&walker(from), &[to], 32.0, 0, 512, block, &mut budget)
}

#[tuff_core::watchdog]
#[test]
fn open_ground_paths_go_straight() {
    let flat = world(|_| None);
    let path = search([0.5, 64.0, 0.5], BlockPos::new(5, 64, 0), &flat).unwrap();
    assert!(path.reached);
    assert_eq!(path.nodes.len(), 6);
    assert!(path.nodes.iter().all(|n| n.y == 64 && n.z == 0));
}

#[tuff_core::watchdog]
#[test]
fn a_bound_keeps_the_search_within_its_radius() {
    let flat = world(|_| None);
    let mut mob = walker([0.5, 64.0, 0.5]);
    mob.bound = Some((BlockPos::new(0, 64, 0), 25.0));
    let mut budget = 10_000;
    let path = find_path(
        &mob,
        &[BlockPos::new(12, 64, 0)],
        32.0,
        0,
        512,
        &flat,
        &mut budget,
    )
    .unwrap();
    assert!(!path.reached);
    assert!(path.nodes.iter().all(|n| n.x <= 5), "{:?}", path.nodes);
}

#[tuff_core::watchdog]
#[test]
fn walls_are_walked_around_and_fences_never_crossed() {
    let fence = world(|pos| {
        (pos.x == 3 && pos.y == 64 && (-3..=3).contains(&pos.z)).then_some(Block::OakFence)
    });
    let path = search([0.5, 64.0, 0.5], BlockPos::new(6, 64, 0), &fence).unwrap();
    assert!(path.reached);
    assert!(path.nodes.iter().all(|n| n.x != 3 || n.z.abs() > 3));
    assert!(path.nodes.iter().any(|n| n.z.abs() >= 4));
}

#[tuff_core::watchdog]
#[test]
fn one_block_steps_are_jumped_and_two_block_walls_are_not() {
    let step = world(|pos| (pos.x == 2 && pos.y == 64).then_some(Block::Stone));
    let path = search([0.5, 64.0, 0.5], BlockPos::new(2, 65, 0), &step).unwrap();
    assert!(path.reached, "{:?}", path);
    assert_eq!(path.end().unwrap().y, 65);
    let wall = world(|pos| {
        (pos.x == 2 && (64..=65).contains(&pos.y) && pos.z.abs() <= 5).then_some(Block::Stone)
    });
    let path = search([0.5, 64.0, 0.5], BlockPos::new(4, 64, 0), &wall).unwrap();
    assert!(path.reached);
    assert!(path.nodes.iter().any(|n| n.z.abs() > 5));
}

#[tuff_core::watchdog]
#[test]
fn water_costs_more_than_a_detour_on_land() {
    let pond = world(|pos| {
        (pos.y == 63 && (2..=3).contains(&pos.x) && pos.z.abs() <= 1).then_some(Block::Water)
    });
    let path = search([0.5, 64.0, 0.5], BlockPos::new(6, 64, 0), &pond).unwrap();
    assert!(path.reached);
    assert!(
        path.nodes.iter().all(|n| n.kind != PathType::Water),
        "{:?}",
        path.nodes
    );
}

#[tuff_core::watchdog]
#[test]
fn lava_and_fire_are_never_entered() {
    let lava = world(|pos| (pos.y == 63 && pos.x == 2 && pos.z.abs() <= 30).then_some(Block::Lava));
    let path = search([0.5, 64.0, 0.5], BlockPos::new(4, 64, 0), &lava);
    assert!(path.is_none_or(|p| !p.reached || p.nodes.iter().all(|n| n.x != 2 || n.z.abs() > 30)));
}

#[tuff_core::watchdog]
#[test]
fn unknown_blocks_bound_the_search() {
    let edge = world(|_| None);
    let path = search([38.5, 64.0, 0.5], BlockPos::new(45, 64, 0), &edge).unwrap();
    assert!(!path.reached);
    assert!(path.nodes.iter().all(|n| n.x <= 40));
}

#[tuff_core::watchdog]
#[test]
fn searches_spend_the_budget() {
    let flat = world(|_| None);
    let mut budget = 5;
    let path = find_path(
        &walker([0.5, 64.0, 0.5]),
        &[BlockPos::new(20, 64, 0)],
        32.0,
        0,
        512,
        &flat,
        &mut budget,
    )
    .unwrap();
    assert!(!path.reached);
    assert_eq!(budget, 0);
}

/// Water from y 56 to 63 (air above) wherever `pool` holds, stone
/// elsewhere below 64, air above.
fn sea(
    pool: impl Fn(i32, i32) -> bool,
    extra: impl Fn(BlockPos) -> Option<Block>,
) -> impl Fn(BlockPos) -> Option<BlockState> {
    world(move |pos| {
        extra(pos).or_else(|| {
            if pos.y >= 64 {
                None
            } else if pos.y >= 56 && pool(pos.x, pos.z) {
                Some(Block::Water)
            } else {
                Some(Block::Stone)
            }
        })
    })
}

fn swimmer(position: [f64; 3], mode: PathMode) -> Walker<'static> {
    const WATER: &[(PathType, f32)] = &[(PathType::Water, 0.0)];
    Walker {
        bounds: [
            position[0] - 0.25,
            position[1],
            position[2] - 0.25,
            position[0] + 0.25,
            position[1] + 0.3,
            position[2] + 0.25,
        ],
        width: 0.5,
        height: 0.3,
        on_ground: false,
        can_float: false,
        in_water: true,
        malus: WATER,
        mode,
        ..walker(position)
    }
}

fn swim_search(
    from: [f64; 3],
    to: BlockPos,
    mode: PathMode,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Option<Path> {
    let mut budget = 10_000;
    find_path(
        &swimmer(from, mode),
        &[to],
        32.0,
        0,
        512,
        block,
        &mut budget,
    )
}

#[tuff_core::watchdog]
#[test]
fn swimmers_path_through_water_around_walls() {
    let walled = sea(
        |x, z| x.abs() <= 10 && z.abs() <= 10,
        |pos| (pos.x == 0 && pos.z.abs() <= 2 && pos.y < 64).then_some(Block::Stone),
    );
    let swim = PathMode::Swim { breaching: false };
    let path = swim_search([-3.5, 60.0, 0.5], BlockPos::new(3, 60, 0), swim, &walled).unwrap();
    assert!(path.reached, "{:?}", path.nodes);
    assert!(
        path.nodes.iter().any(|n| n.z.abs() >= 3),
        "goes round the wall"
    );
    assert!(path.nodes[1..].iter().all(|n| n.kind == PathType::Water));
    assert!(path.nodes.iter().all(|n| n.x != 0 || n.z.abs() >= 3));
}

#[tuff_core::watchdog]
#[test]
fn swimmers_move_up_down_and_diagonally() {
    let open = sea(|x, z| x.abs() <= 10 && z.abs() <= 10, |_| None);
    let swim = PathMode::Swim { breaching: false };
    let path = swim_search([0.5, 57.0, 0.5], BlockPos::new(0, 62, 0), swim, &open).unwrap();
    assert!(path.reached);
    assert_eq!(path.nodes.len(), 6, "straight up: {:?}", path.nodes);
    let path = swim_search([0.5, 60.0, 0.5], BlockPos::new(4, 60, 4), swim, &open).unwrap();
    assert!(path.reached);
    assert_eq!(path.nodes.len(), 5, "diagonal steps: {:?}", path.nodes);
}

#[tuff_core::watchdog]
#[test]
fn swimmers_never_leave_the_water() {
    let open = sea(|x, z| x.abs() <= 10 && z.abs() <= 10, |_| None);
    let swim = PathMode::Swim { breaching: false };
    let path = swim_search([0.5, 60.0, 0.5], BlockPos::new(0, 66, 0), swim, &open).unwrap();
    assert!(!path.reached);
    assert!(path.nodes.iter().all(|n| n.y <= 63), "{:?}", path.nodes);
    let beach = swim_search([0.5, 60.0, 0.5], BlockPos::new(15, 64, 0), swim, &open);
    assert!(beach.is_none_or(|p| !p.reached && p.nodes.iter().all(|n| n.x <= 10)));
}

#[tuff_core::watchdog]
#[test]
fn breaching_swimmers_may_path_into_the_air_just_above_water() {
    let open = sea(|x, z| x.abs() <= 10 && z.abs() <= 10, |_| None);
    let dolphin = PathMode::Swim { breaching: true };
    let path = swim_search([0.5, 60.0, 0.5], BlockPos::new(3, 64, 0), dolphin, &open).unwrap();
    let end = path.end().unwrap();
    assert_eq!(
        (end.x, end.y, end.z, end.kind),
        (3, 64, 0, PathType::Breach)
    );
    let path = swim_search([0.5, 60.0, 0.5], BlockPos::new(3, 66, 0), dolphin, &open).unwrap();
    assert!(
        path.nodes.iter().all(|n| n.y <= 64),
        "only the layer right above the water"
    );
    let fish = PathMode::Swim { breaching: false };
    let path = swim_search([0.5, 60.0, 0.5], BlockPos::new(3, 64, 0), fish, &open).unwrap();
    assert!(
        path.nodes
            .iter()
            .all(|n| n.kind == PathType::Water || n == &path.nodes[0])
    );
}

#[tuff_core::watchdog]
#[test]
fn amphibious_mobs_swim_up_and_walk_out() {
    let pond = sea(|x, z| x.abs() <= 3 && z.abs() <= 3, |_| None);
    let mut amphibious = swimmer([0.5, 57.0, 0.5], PathMode::Amphibious);
    amphibious.max_up_step = 1.0;
    let mut budget = 10_000;
    let path = find_path(
        &amphibious,
        &[BlockPos::new(7, 64, 0)],
        32.0,
        0,
        512,
        &pond,
        &mut budget,
    )
    .unwrap();
    assert!(path.reached, "{:?}", path.nodes);
    assert_eq!(
        path.nodes[0].y, 57,
        "starts where it floats, not at the surface"
    );
    assert!(
        path.nodes
            .windows(2)
            .any(|w| w[0].x == w[1].x && w[1].y == w[0].y + 1),
        "swims straight up: {:?}",
        path.nodes
    );
}

fn flyer(position: [f64; 3]) -> Walker<'static> {
    Walker {
        bounds: [
            position[0] - 0.25,
            position[1],
            position[2] - 0.25,
            position[0] + 0.25,
            position[1] + 0.5,
            position[2] + 0.25,
        ],
        width: 0.5,
        height: 0.5,
        on_ground: false,
        mode: PathMode::Fly,
        ..walker(position)
    }
}

fn fly_search(
    from: [f64; 3],
    to: BlockPos,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Option<Path> {
    let mut budget = 10_000;
    find_path(&flyer(from), &[to], 32.0, 0, 1024, block, &mut budget)
}

#[tuff_core::watchdog]
#[test]
fn flyers_path_diagonally_through_the_air() {
    let air = world(|_| None);
    let path = fly_search([0.5, 66.0, 0.5], BlockPos::new(4, 70, 4), &air).unwrap();
    assert!(path.reached);
    assert_eq!(path.nodes.len(), 5, "{:?}", path.nodes);
}

#[tuff_core::watchdog]
#[test]
fn flyers_go_over_walls_a_walker_must_go_around() {
    let wall = world(|pos| {
        (pos.x == 3 && (64..=66).contains(&pos.y) && pos.z.abs() <= 20).then_some(Block::Stone)
    });
    let path = fly_search([0.5, 65.0, 0.5], BlockPos::new(6, 65, 0), &wall).unwrap();
    assert!(path.reached);
    assert!(path.nodes.iter().any(|n| n.y >= 67), "{:?}", path.nodes);
    assert!(path.nodes.iter().all(|n| n.x != 3 || n.y >= 67));
}

#[tuff_core::watchdog]
#[test]
fn flyers_see_air_over_a_fence_or_cocoa_as_that_danger() {
    let block = world(|pos| match (pos.x, pos.y, pos.z) {
        (5, 64, 5) => Some(Block::OakFence),
        (7, 64, 5) => Some(Block::Cocoa),
        _ => None,
    });
    let mob = flyer([0.5, 66.0, 0.5]);
    let evaluator = Evaluator::new(&mob, &block);
    assert_eq!(
        evaluator.fly_cell_type(BlockPos::new(5, 65, 5)),
        PathType::Fence
    );
    assert_eq!(
        evaluator.fly_cell_type(BlockPos::new(7, 65, 5)),
        PathType::Cocoa
    );
    assert_eq!(
        evaluator.fly_cell_type(BlockPos::new(0, 70, 0)),
        PathType::Open
    );
}

#[tuff_core::watchdog]
#[test]
fn flyers_start_at_their_rounded_position_and_pay_extra_to_hug_the_ground() {
    let air = world(|_| None);
    let mob = flyer([0.5, 66.6, 0.5]);
    let mut evaluator = Evaluator::new(&mob, &air);
    let start = evaluator.fly_start();
    assert_eq!(evaluator.nodes[start].y, 67);

    let low = flyer([0.5, 64.0, 0.5]);
    let mut evaluator = Evaluator::new(&low, &air);
    let node = evaluator.fly_accepted(0, 64, 0).unwrap();
    assert_eq!(evaluator.nodes[node].kind, PathType::Walkable);
    assert!(evaluator.nodes[node].malus >= 1.0);
}

fn shallow_penalties(frog_nodes: bool) -> Vec<f32> {
    let pond = sea(|x, z| x.abs() <= 3 && z.abs() <= 3, |_| None);
    let mut frog = Walker {
        frog_nodes,
        sea_level: 70,
        ..swimmer([0.5, 57.0, 0.5], PathMode::Amphibious)
    };
    frog.max_up_step = 1.0;
    let mut ev = Evaluator::new(&frog, &pond);
    let start = ev.start().expect("a start");
    let mut out = Vec::new();
    ev.neighbors(start, &mut out);
    out.iter()
        .filter(|n| ev.nodes[**n].kind == PathType::Water)
        .map(|n| ev.nodes[*n].malus)
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn frogs_pay_extra_for_water_deep_below_sea_level() {
    let plain = shallow_penalties(false);
    let frog = shallow_penalties(true);
    assert!(!plain.is_empty());
    assert!(plain.iter().all(|m| *m == 0.0));
    assert!(frog.iter().all(|m| *m >= 1.0), "{frog:?}");
}

#[tuff_core::watchdog]
#[test]
fn frogs_read_the_air_above_a_jump_target_as_open() {
    let pads = world(|pos| (pos.y == 63).then_some(Block::LilyPad));
    let at = |frog_nodes: bool| {
        let w = Walker {
            frog_nodes,
            ..walker([0.5, 64.0, 0.5])
        };
        Evaluator::new(&w, &pads).static_type(0, 64, 0)
    };
    assert_eq!(at(true), PathType::Open);
    assert_ne!(at(false), PathType::Open);
}

#[tuff_core::watchdog]
#[test]
fn a_lava_walker_starts_on_top_of_the_lava_it_is_in() {
    let lake = world(|pos| (pos.y == 63 || pos.y == 64).then_some(Block::Lava));
    let mut mob = walker([0.5, 63.5, 0.5]);
    mob.on_ground = false;
    mob.stands_on_fluid = stands_on_lava;
    let mut budget = 10_000;
    let path = find_path(
        &mob,
        &[BlockPos::new(3, 65, 0)],
        32.0,
        0,
        512,
        &lake,
        &mut budget,
    )
    .expect("a path");
    assert_eq!(path.nodes[0].y, 64);
}

fn maze_block(pos: BlockPos) -> Option<Block> {
    let hash = (pos.x.wrapping_mul(73_856_093) ^ pos.z.wrapping_mul(19_349_663)) as u32;
    let roll = hash.wrapping_mul(2_654_435_761) >> 24;
    match (pos.y, roll % 16) {
        (64, 0..=2) => Some(Block::Stone),
        (64, 3) => Some(Block::OakFence),
        (65, 0) => Some(Block::Stone),
        (63, 4) => Some(Block::Water),
        (64, 5) if roll.is_multiple_of(3) => Some(Block::Stone),
        _ => None,
    }
}

fn fingerprint(path: &Path) -> u64 {
    let mut acc = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |v: u64| {
        acc ^= v;
        acc = acc.wrapping_mul(0x0100_0000_01b3);
    };
    for n in &path.nodes {
        mix(n.x as u64);
        mix(n.y as u64);
        mix(n.z as u64);
    }
    mix(path.reached as u64);
    mix(path.distance_to_target.to_bits() as u64);
    let debug = path.debug.as_ref().expect("debug captured");
    for set in [
        &debug.nodes,
        &debug.open_set,
        &debug.closed_set,
        &debug.targets,
    ] {
        mix(set.len() as u64);
        for d in set {
            for c in d.pos {
                mix(c as u64);
            }
            mix(d.walked_distance.to_bits() as u64);
            mix(d.cost_malus.to_bits() as u64);
            mix(d.f.to_bits() as u64);
            mix(d.closed as u64);
            mix(d.path_type as u64);
        }
    }
    acc
}

fn maze_runs() -> Vec<u64> {
    let maze = world(maze_block);
    let mut prints = Vec::new();
    for (i, targets) in [
        vec![BlockPos::new(20, 64, 17)],
        vec![BlockPos::new(-25, 64, 9), BlockPos::new(14, 64, -22)],
        vec![BlockPos::new(39, 64, 39)],
        vec![
            BlockPos::new(-3, 64, 30),
            BlockPos::new(30, 64, -3),
            BlockPos::new(-30, 64, -30),
        ],
    ]
    .into_iter()
    .enumerate()
    {
        for reach in [0, 2] {
            let mut mob = walker([0.5, 64.0, 0.5]);
            mob.capture_debug = true;
            if i == 3 {
                mob.width = 1.4;
            }
            let mut budget = 100_000;
            let found = find_path(&mob, &targets, 48.0, reach, 2_000, &maze, &mut budget);
            prints.push(found.as_ref().map_or(0, fingerprint) ^ budget as u64);
        }
    }
    prints
}

#[tuff_core::watchdog]
#[test]
fn search_results_on_a_fixed_maze_are_pinned() {
    let prints = maze_runs();
    assert_eq!(prints, PINNED_MAZE_PRINTS);
}

const PINNED_MAZE_PRINTS: [u64; 8] = [
    0xe71c_d552_2b44_d01c,
    0xe71c_d552_2b44_d01c,
    0xb3e2_5cec_1506_edc1,
    0xca71_5b73_f09f_0233,
    0x3415_a2d6_2ed7_9e24,
    0x3415_a2d6_2ed7_9e24,
    0x1b56_73ef_a524_55d1,
    0x1b56_73ef_a524_55d1,
];

#[tuff_core::watchdog]
#[test]
#[ignore = "timing run: cargo test --release -p tuff-feature-builtin-vanilla-mobs maze_search_timing -- --ignored --nocapture"]
fn maze_search_timing() {
    let start = std::time::Instant::now();
    for _ in 0..200 {
        std::hint::black_box(maze_runs());
    }
    eprintln!("maze_search_timing: {:?}", start.elapsed());
}
