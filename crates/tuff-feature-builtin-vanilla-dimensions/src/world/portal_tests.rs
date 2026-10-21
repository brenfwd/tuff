#[tuff_core::watchdog]
#[test]
fn an_arrival_inside_the_floor_is_lifted_onto_it() {
    let stone = Block::Stone.default_state();
    let air = Block::Air.default_state();
    let floor = |p: BlockPos| {
        Some(if p == BlockPos::new(0, 63, 0) {
            stone
        } else {
            air
        })
    };
    let size = [0.6, 1.8];
    let lifted = collision_free([0.5, 63.5, 0.5], size, &floor);
    assert!((lifted[1] - 64.0).abs() < 1e-5, "{lifted:?}");
    assert_eq!([lifted[0], lifted[2]], [0.5, 0.5]);
    assert_eq!(
        collision_free([0.5, 64.0, 0.5], size, &floor),
        [0.5, 64.0, 0.5]
    );
    assert_eq!(
        collision_free([0.5, 63.5, 0.5], [5.0, 1.0], &floor),
        [0.5, 63.5, 0.5],
        "wide bodies stay"
    );
    let unknown = |_| None;
    assert_eq!(
        collision_free([0.5, 63.5, 0.5], size, &unknown),
        [0.5, 63.5, 0.5]
    );
}

#[tuff_core::watchdog]
#[test]
fn an_arrival_against_a_wall_steps_away_from_it() {
    let stone = Block::Stone.default_state();
    let air = Block::Air.default_state();
    let wall = |p: BlockPos| {
        Some(if p.x == 1 && (64..66).contains(&p.y) {
            stone
        } else {
            air
        })
    };
    let moved = collision_free([0.9, 64.0, 0.5], [0.6, 1.8], &wall);
    assert!((moved[0] - 0.7).abs() < 1e-5, "{moved:?}");
    assert_eq!((moved[1], moved[2]), (64.0, 0.5));
}

#[tuff_core::watchdog]
#[test]
fn only_the_first_exit_from_the_end_rolls_the_credits() {
    assert_eq!(end_exit(false, false), EndExit::Platform);
    assert_eq!(end_exit(false, true), EndExit::Platform);
    assert_eq!(end_exit(true, false), EndExit::Credits);
    assert_eq!(end_exit(true, true), EndExit::RespawnPoint);
}

use std::collections::HashMap;

use super::*;

const MIN_Y: i32 = -64;

struct World {
    blocks: HashMap<BlockPos, BlockState>,
    border: f64,
}

impl World {
    fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            border: 30_000_000.0,
        }
    }

    fn get(&self, pos: BlockPos) -> BlockState {
        self.blocks.get(&pos).copied().unwrap_or(AIR)
    }

    fn put(&mut self, x: i32, y: i32, z: i32, block: Block) {
        self.blocks
            .insert(BlockPos::new(x, y, z), block.default_state());
    }

    fn lookup(&self) -> impl Fn(BlockPos) -> BlockState + '_ {
        move |pos| self.get(pos)
    }

    /// A frame with inner `width` x `height` whose bottom-left inner
    /// block is `origin`, walking `axis`.
    fn frame(&mut self, origin: BlockPos, axis: Axis, width: i32, height: i32) {
        let right = axis.right();
        for across in -1..=width {
            for up in -1..=height {
                let edge = across == -1 || across == width || up == -1 || up == height;
                let corner = (across == -1 || across == width) && (up == -1 || up == height);
                if edge && !corner {
                    let pos = step(step(origin, right, across), Facing::Up, up);
                    self.blocks.insert(pos, Block::Obsidian.default_state());
                }
            }
        }
    }

    fn fill_ground(&mut self, top: i32, range: std::ops::RangeInclusive<i32>) {
        for x in range.clone() {
            for z in range.clone() {
                for y in top - 3..=top {
                    self.put(x, y, z, Block::Stone);
                }
            }
        }
    }
}

impl PortalLevel for World {
    fn get(&self, pos: BlockPos) -> Option<BlockState> {
        Some(World::get(self, pos))
    }

    fn set(&mut self, pos: BlockPos, state: BlockState) {
        self.blocks.insert(pos, state);
    }

    fn motion_blocking_height(&self, x: i32, z: i32) -> Option<i32> {
        self.blocks
            .iter()
            .filter(|(pos, state)| pos.x == x && pos.z == z && blocks_motion(**state))
            .map(|(pos, _)| pos.y + 1)
            .max()
            .or(Some(MIN_Y))
    }

    fn min_y(&self) -> i32 {
        MIN_Y
    }

    fn logical_max_y(&self) -> i32 {
        320
    }

    fn inside_border(&self, pos: BlockPos) -> bool {
        (pos.x as f64).abs() < self.border / 2.0 && (pos.z as f64).abs() < self.border / 2.0
    }

    fn clamp_to_border(&self, pos: BlockPos) -> BlockPos {
        let half = self.border / 2.0 - 1e-5;
        BlockPos::new(
            (pos.x as f64).clamp(-half, half).floor() as i32,
            pos.y,
            (pos.z as f64).clamp(-half, half).floor() as i32,
        )
    }
}

fn origin() -> BlockPos {
    BlockPos::new(10, 64, 10)
}

#[tuff_core::watchdog]
#[test]
fn every_valid_frame_size_is_found_on_both_axes() {
    for axis in [Axis::X, Axis::Z] {
        for width in MIN_WIDTH..=MAX_WIDTH {
            for height in MIN_HEIGHT..=MAX_HEIGHT {
                let mut world = World::new();
                world.frame(origin(), axis, width, height);
                let inner = step(step(origin(), axis.right(), width / 2), Facing::Up, 1);
                let shape = find_empty_shape(&world.lookup(), inner, MIN_Y, axis.other())
                    .unwrap_or_else(|| panic!("{axis:?} {width}x{height}"));
                assert_eq!(
                    (shape.axis, shape.bottom_left, shape.width, shape.height),
                    (axis, origin(), width, height)
                );
                assert_eq!(shape.portal_blocks, 0);
                assert_eq!(shape.portal_blocks().count() as i32, width * height);
            }
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn frames_that_are_too_large_or_broken_are_rejected() {
    let level = |world: &World| find_empty_shape(&world.lookup(), origin(), MIN_Y, Axis::X);
    let mut world = World::new();
    world.frame(origin(), Axis::X, MAX_WIDTH + 1, 3);
    assert!(level(&world).is_none());
    let mut world = World::new();
    world.frame(origin(), Axis::X, 2, MAX_HEIGHT + 1);
    assert!(level(&world).is_none());
    let mut world = World::new();
    world.frame(origin(), Axis::X, 1, 3);
    assert!(level(&world).is_none());
    let mut world = World::new();
    world.frame(origin(), Axis::X, 2, 2);
    assert!(level(&world).is_none());

    let mut world = World::new();
    world.frame(origin(), Axis::X, 4, 5);
    let missing = step(step(origin(), Axis::X.right(), 2), Facing::Up, 5);
    world.blocks.remove(&missing);
    assert!(level(&world).is_none(), "gap in the top");
    world.put(missing.x, missing.y, missing.z, Block::CryingObsidian);
    assert!(level(&world).is_none(), "wrong frame block");
    world.put(missing.x, missing.y, missing.z, Block::Obsidian);
    assert!(level(&world).is_some());
    world.put(origin().x, origin().y + 2, origin().z, Block::Stone);
    assert!(level(&world).is_none(), "obstructed inside");
    world.put(origin().x, origin().y + 2, origin().z, Block::Fire);
    assert!(level(&world).is_some(), "fire counts as empty");
}

#[tuff_core::watchdog]
#[test]
fn lighting_fills_the_frame_and_the_portal_stays_complete() {
    let mut world = World::new();
    world.frame(origin(), Axis::Z, 3, 4);
    let click = step(step(origin(), Facing::Down, 1), Facing::Up, 1);
    assert!(lights_portal(&world.lookup(), click, MIN_Y, Facing::North));
    assert!(!lights_portal(
        &world.lookup(),
        BlockPos::new(100, 64, 100),
        MIN_Y,
        Facing::North
    ));
    let shape = find_empty_shape(&world.lookup(), click, MIN_Y, Axis::X).unwrap();
    assert_eq!(shape.axis, Axis::Z);
    for (pos, state) in shape.portal_blocks() {
        world.blocks.insert(pos, state);
    }
    let again = find_any_shape(&world.lookup(), click, MIN_Y, Axis::Z);
    assert!(again.is_complete());
    let portal = Axis::Z.portal_state();
    assert!(portal_survives(
        &world.lookup(),
        click,
        MIN_Y,
        portal,
        Facing::Up,
        portal
    ));
    let corner = step(origin(), Facing::Down, 1);
    world.blocks.remove(&corner);
    assert!(!portal_survives(
        &world.lookup(),
        origin(),
        MIN_Y,
        portal,
        Facing::Down,
        AIR
    ));
    assert!(
        portal_survives(&world.lookup(), origin(), MIN_Y, portal, Facing::East, AIR),
        "changes across the portal plane are ignored"
    );
    let rectangle = portal_rectangle(&world.lookup(), origin()).unwrap();
    assert_eq!(rectangle.min, origin());
    assert_eq!((rectangle.width, rectangle.height), (3, 4));
}

#[tuff_core::watchdog]
#[test]
fn largest_rectangle_finds_the_portal_extent() {
    let mut world = World::new();
    world.frame(origin(), Axis::X, 5, 6);
    let shape = find_empty_shape(&world.lookup(), origin(), MIN_Y, Axis::X).unwrap();
    for (pos, state) in shape.portal_blocks() {
        world.blocks.insert(pos, state);
    }
    let inside = step(step(origin(), Axis::X.right(), 3), Facing::Up, 4);
    let rectangle = portal_rectangle(&world.lookup(), inside).unwrap();
    assert_eq!((rectangle.width, rectangle.height), (5, 6));
    assert_eq!(rectangle.min, step(origin(), Axis::X.right(), 4));
    assert_eq!(rectangle.axis, Axis::X);
    assert_eq!(max_rectangle(&[2, 1, 4, 5, 1, 3, 3]), ((2, 3), 4));
    assert_eq!(max_rectangle(&[2, 3, 3, 0, 1]), ((1, 2), 3));
}

#[tuff_core::watchdog]
#[test]
fn exit_coordinates_scale_eight_to_one() {
    let scale = |from: f64, to: f64, position: [f64; 3]| {
        [
            position[0] * from / to,
            position[1],
            position[2] * from / to,
        ]
    };
    assert_eq!(scale(1.0, 8.0, [800.0, 64.0, -160.0]), [100.0, 64.0, -20.0]);
    assert_eq!(scale(8.0, 1.0, [100.0, 64.0, -20.0]), [800.0, 64.0, -160.0]);
}

#[tuff_core::watchdog]
#[test]
fn arrival_keeps_the_relative_position_and_turns_across_axes() {
    let source = PortalRectangle {
        min: BlockPos::new(0, 64, 0),
        axis: Axis::X,
        width: 4,
        height: 5,
    };
    let offset = relative_position(&source, [2.0, 64.0, 0.5]);
    assert!((offset[0] - 0.5).abs() < 1e-9, "{offset:?}");
    assert_eq!(offset[1], 0.0);
    assert_eq!(offset[2], 0.0);
    let exit = PortalRectangle {
        min: BlockPos::new(100, 40, 20),
        axis: Axis::Z,
        width: 2,
        height: 3,
    };
    let (position, rotation) = arrival(&exit, Axis::X, offset);
    assert_eq!(rotation, 90.0);
    assert_eq!(position, [100.5, 40.0, 21.0]);
    let (same, rotation) = arrival(&source, Axis::X, [0.0, 1.0, 0.25]);
    assert_eq!(rotation, 0.0);
    assert_eq!(same, [0.3, 64.0 + 3.2, 0.75]);
}

#[tuff_core::watchdog]
#[test]
fn closest_prefers_distance_then_lower_blocks() {
    let origin = BlockPos::new(0, 64, 0);
    let candidates = [
        BlockPos::new(3, 64, 0),
        BlockPos::new(0, 66, 0),
        BlockPos::new(0, 62, 0),
        BlockPos::new(-2, 64, 0),
    ];
    assert_eq!(closest(candidates, origin), Some(BlockPos::new(0, 62, 0)));
    assert_eq!(closest([], origin), None);
}

#[tuff_core::watchdog]
#[test]
fn spiral_visits_the_square_once_starting_at_the_center() {
    let center = BlockPos::new(5, 0, -5);
    let visited: Vec<BlockPos> = spiral(center, 2).collect();
    assert_eq!(visited[0], center);
    assert_eq!(visited[1], BlockPos::new(6, 0, -5));
    assert_eq!(visited[2], BlockPos::new(6, 0, -4));
    let unique: HashSet<BlockPos> = visited.iter().copied().collect();
    assert_eq!(unique.len(), visited.len());
    assert!(unique.contains(&BlockPos::new(3, 0, -7)));
    assert!(unique.contains(&BlockPos::new(7, 0, -3)));
    assert_eq!(
        spiral(center, 16).count(),
        spiral(center, 16).collect::<HashSet<_>>().len()
    );
}

#[tuff_core::watchdog]
#[test]
fn portals_are_built_on_the_ground_when_there_is_room() {
    let mut world = World::new();
    world.fill_ground(40, -40..=40);
    let target = BlockPos::new(0, 45, 0);
    let rectangle = create_portal(&mut world, target, Axis::X).unwrap();
    assert_eq!((rectangle.width, rectangle.height), (2, 3));
    assert_eq!(rectangle.min, BlockPos::new(0, 41, 0));
    for (pos, state) in [
        (BlockPos::new(-1, 41, 0), Block::Obsidian),
        (BlockPos::new(2, 43, 0), Block::Obsidian),
        (BlockPos::new(0, 40, 0), Block::Obsidian),
        (BlockPos::new(1, 44, 0), Block::Obsidian),
    ]
    .map(|(pos, block)| (pos, block.default_state()))
    {
        assert_eq!(world.get(pos), state, "{pos:?}");
    }
    assert_eq!(world.get(BlockPos::new(0, 41, 0)), Axis::X.portal_state());
    assert_eq!(world.get(BlockPos::new(1, 43, 0)), Axis::X.portal_state());
    assert!(
        is_air(world.get(BlockPos::new(-1, 40, 1)))
            || world.get(BlockPos::new(-1, 40, 1)).block() == Block::Stone
    );
    let found = find_any_shape(&world.lookup(), BlockPos::new(0, 41, 0), MIN_Y, Axis::X);
    assert!(found.is_complete());
}

#[tuff_core::watchdog]
#[test]
fn the_floating_platform_is_clamped_into_the_world_border() {
    let mut world = World::new();
    world.border = 20.0;
    let rectangle = create_portal(&mut world, BlockPos::new(500, 20, 0), Axis::Z).unwrap();
    assert!(world.inside_border(rectangle.min));
}

#[tuff_core::watchdog]
#[test]
fn portals_float_on_a_platform_above_the_void() {
    let mut world = World::new();
    let target = BlockPos::new(30, 20, -30);
    let rectangle = create_portal(&mut world, target, Axis::Z).unwrap();
    assert_eq!(rectangle.axis, Axis::Z);
    assert_eq!(rectangle.min, BlockPos::new(30, 70, -31));
    for dx in -1..=1 {
        for dz in 0..2 {
            assert_eq!(
                world.get(BlockPos::new(30 + dx, 69, -31 + dz)).block(),
                Block::Obsidian,
                "platform {dx} {dz}"
            );
        }
    }
    assert_eq!(
        world.get(BlockPos::new(30, 70, -31)),
        Axis::Z.portal_state()
    );
    assert_eq!(
        world.get(BlockPos::new(30, 72, -30)),
        Axis::Z.portal_state()
    );
    assert_eq!(
        world.get(BlockPos::new(30, 73, -30)).block(),
        Block::Obsidian
    );
    assert_eq!(
        world.get(BlockPos::new(30, 71, -32)).block(),
        Block::Obsidian
    );
    assert!(find_any_shape(&world.lookup(), rectangle.min, MIN_Y, Axis::Z).is_complete());
    let ceiling = BlockPos::new(0, 300, 0);
    world.fill_ground(200, -20..=20);
    let high = create_portal(&mut world, ceiling, Axis::X).unwrap();
    assert!(high.min.y + 4 < 320, "{high:?}");
}

#[tuff_core::watchdog]
#[test]
fn contact_reports_the_portal_the_body_overlaps() {
    let mut world = World::new();
    let level = |pos: BlockPos| Some(world.get(pos));
    assert_eq!(contact([0.5, 64.0, 0.5], &level), None);
    world
        .blocks
        .insert(BlockPos::new(0, 65, 0), Axis::X.portal_state());
    let level = |pos: BlockPos| Some(world.get(pos));
    assert_eq!(
        contact([0.5, 64.0, 0.5], &level),
        Some(PortalContact {
            kind: PortalKind::Nether,
            pos: BlockPos::new(0, 65, 0)
        })
    );
    assert_eq!(contact([0.5, 66.0, 0.5], &level), None);
    let mut world = World::new();
    world.put(3, 10, 3, Block::EndPortal);
    let level = |pos: BlockPos| Some(world.get(pos));
    assert_eq!(
        contact([3.5, 10.0, 3.5], &level).map(|c| c.kind),
        Some(PortalKind::End)
    );
    assert_eq!(contact([3.5, 10.8, 3.5], &level), None, "above the surface");
    assert_eq!(contact([3.5, 8.0, 3.5], &level), None, "below it");
}

#[tuff_core::watchdog]
#[test]
fn end_portal_ring_completes_around_the_hole() {
    let mut world = World::new();
    let center = BlockPos::new(0, 60, 0);
    let frame = |facing: &str, eye: bool| {
        Block::EndPortalFrame
            .default_state()
            .with("facing", facing)
            .unwrap()
            .with("eye", if eye { "true" } else { "false" })
            .unwrap()
    };
    for i in -1..=1 {
        world
            .blocks
            .insert(BlockPos::new(i, 60, -2), frame("south", true));
        world
            .blocks
            .insert(BlockPos::new(i, 60, 2), frame("north", true));
        world
            .blocks
            .insert(BlockPos::new(-2, 60, i), frame("east", true));
        world
            .blocks
            .insert(BlockPos::new(2, 60, i), frame("west", i != 0));
    }
    assert_eq!(
        end_portal_center(&world.lookup(), BlockPos::new(0, 60, -2)),
        None
    );
    world
        .blocks
        .insert(BlockPos::new(2, 60, 0), frame("west", true));
    for frame in [
        BlockPos::new(0, 60, -2),
        BlockPos::new(2, 60, 1),
        BlockPos::new(-2, 60, -1),
    ] {
        assert_eq!(end_portal_center(&world.lookup(), frame), Some(center));
    }
    world
        .blocks
        .insert(BlockPos::new(2, 60, 0), frame("east", true));
    assert_eq!(
        end_portal_center(&world.lookup(), BlockPos::new(0, 60, -2)),
        None
    );
    assert_eq!(end_portal_blocks(center).count(), 9);
    let platform: Vec<_> = end_platform(BlockPos::new(100, 49, 0)).collect();
    assert_eq!(platform.len(), 100);
    assert!(
        platform
            .iter()
            .filter(|(_, s)| s.block() == Block::Obsidian)
            .all(|(p, _)| p.y == 48)
    );
    assert_eq!(
        platform
            .iter()
            .filter(|(_, s)| s.block() == Block::Obsidian)
            .count(),
        25
    );
}

fn shape_fixture() -> World {
    let mut world = World::new();
    let mut seed: u64 = 0x05ee_d0ff_4a3e;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    for (i, (w, h)) in [(2, 3), (4, 5), (21, 21), (22, 3), (3, 22), (5, 4), (1, 3)]
        .into_iter()
        .enumerate()
    {
        let axis = if i % 2 == 0 { Axis::X } else { Axis::Z };
        world.frame(BlockPos::new(i as i32 * 30, 0, 0), axis, w, h);
    }
    for _ in 0..4000 {
        let pos = BlockPos::new(
            (next() % 220) as i32 - 20,
            (next() % 30) as i32 - 3,
            (next() % 40) as i32 - 20,
        );
        let block = match next() % 5 {
            0 => Block::Air,
            1 => Block::Stone,
            2 => Block::NetherPortal,
            3 => Block::Fire,
            _ => Block::Obsidian,
        };
        world.blocks.insert(pos, block.default_state());
    }
    world
}

fn shape_fingerprint(world: &World) -> u64 {
    let lookup = world.lookup();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |value: i32| {
        hash = (hash ^ value as u32 as u64).wrapping_mul(0x0100_0000_01b3);
    };
    for x in -20..200 {
        for y in -3..27 {
            for z in -4..4 {
                for axis in [Axis::X, Axis::Z] {
                    let s = find_any_shape(&lookup, BlockPos::new(x, y, z), -2, axis);
                    mix(s.axis as i32);
                    mix(s.bottom_left.x);
                    mix(s.bottom_left.y);
                    mix(s.bottom_left.z);
                    mix(s.width);
                    mix(s.height);
                    mix(s.portal_blocks);
                }
            }
        }
    }
    hash
}

#[tuff_core::watchdog]
#[test]
fn frame_scan_results_are_pinned_for_a_noisy_fixture() {
    let world = shape_fixture();
    let start = std::time::Instant::now();
    let hash = shape_fingerprint(&world);
    for _ in 0..4 {
        assert_eq!(shape_fingerprint(&world), hash);
    }
    eprintln!("frame scan x5: {:?} hash {hash:#x}", start.elapsed());
    assert_eq!(hash, 0x86f0_4067_0c48_34d1);
}
