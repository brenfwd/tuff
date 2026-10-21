// SPDX-License-Identifier: MIT
use std::cell::RefCell;
use std::rc::Rc;

use tuff_bots_api::control::{Aabb, BlockPos, Goal, LeafGoal, NearGoal, RunAwayGoal, Vec3, XzGoal};

use super::*;
use crate::world::tests::{Terrain, TerrainSource};
use crate::world::{BlockCache, Class};

fn block(x: i32, y: i32, z: i32) -> Goal {
    Goal::Leaf(LeafGoal::Block(BlockPos { x, y, z }))
}

fn solve(world: &mut dyn Blocks, start: Pos, goal: Goal, params: Params) -> Plan {
    let mut search = Search::new(start, goal, params);
    let moves = default_moves();
    let limits = Limits {
        nodes_per_step: usize::MAX,
        total_nodes: 50_000,
    };
    match search.step(world, &moves, limits) {
        Status::Done(plan) => plan,
        Status::Running => panic!("unbounded step must finish"),
    }
}

fn open(w: i32, d: i32) -> Terrain {
    Terrain::new(w, d)
}

#[test]
fn a_flat_walk_is_the_straight_line() {
    let mut t = open(16, 16);
    let plan = solve(&mut t, (0, 1, 0), block(5, 1, 0), Params::default());
    assert!(plan.complete);
    assert_eq!(plan.path.len(), 6);
    let direct = 5.0 * SPRINT_TICKS;
    assert!((plan.cost - direct).abs() < 1e-9);
}

#[test]
fn diagonals_beat_two_axis_moves() {
    let mut t = open(16, 16);
    let plan = solve(&mut t, (0, 1, 0), block(4, 1, 4), Params::default());
    assert!(plan.complete);
    assert_eq!(plan.path.len(), 5);
}

#[test]
fn diagonals_do_not_cut_corners_through_solid() {
    let mut t = open(16, 16);
    t.wall((1, 1, 0));
    t.wall((1, 2, 0));
    let mut edges = Vec::new();
    Diagonal
        .expand(&mut t, &Params::default(), (0, 1, 0), &mut edges)
        .unwrap();
    assert!(edges.iter().all(|e| e.to != (1, 1, 1)));
}

#[test]
fn stairs_are_climbed_one_block_at_a_time() {
    let mut t = open(16, 4);
    for x in 1..=4 {
        for y in 0..=x {
            t.wall((x, y, 0));
        }
    }
    let plan = solve(&mut t, (0, 1, 0), block(4, 5, 0), Params::default());
    assert!(plan.complete);
    let ys: Vec<i32> = plan.path.iter().map(|p| p.1).collect();
    assert_eq!(ys, vec![1, 2, 3, 4, 5]);
}

#[test]
fn a_two_high_wall_is_not_climbed() {
    let mut t = open(16, 3);
    for z in 0..3 {
        t.wall((3, 1, z));
        t.wall((3, 2, z));
    }
    let plan = solve(&mut t, (0, 1, 1), block(6, 1, 1), Params::default());
    assert!(!plan.complete);
}

#[test]
fn a_one_wide_maze_is_followed_to_the_end() {
    let rows = ["S.#.....", "#.#.###.", "#.#...#.", "#.###.#.", "#.....#G"];
    let mut t = open(8, 5);
    let mut start = (0, 1, 0);
    let mut goal = (0, 1, 0);
    for (z, row) in rows.iter().enumerate() {
        for (x, c) in row.chars().enumerate() {
            let p = (x as i32, 1, z as i32);
            match c {
                '#' => {
                    t.wall(p);
                    t.wall((p.0, 2, p.2));
                }
                'S' => start = p,
                'G' => goal = p,
                _ => {}
            }
        }
    }
    let plan = solve(
        &mut t,
        start,
        block(goal.0, goal.1, goal.2),
        Params::default(),
    );
    assert!(plan.complete);
    assert_eq!(plan.path.last(), Some(&goal));
    assert!(
        plan.path.windows(2).all(|w| {
            (w[0].0 - w[1].0).abs() + (w[0].2 - w[1].2).abs() == 1 && w[0].1 == w[1].1
        })
    );
    assert_eq!(plan.path.len(), 24);
}

#[test]
fn a_gap_in_the_floor_forces_a_detour() {
    let mut t = open(12, 7);
    for z in 0..6 {
        for x in 4..=5 {
            t.set((x, 0, z), Class::PASSABLE);
            for y in -40..0 {
                t.set((x, y, z), Class::PASSABLE);
            }
        }
    }
    let plan = solve(&mut t, (0, 1, 0), block(9, 1, 0), Params::default());
    assert!(plan.complete);
    assert!(plan.path.iter().any(|p| p.2 == 6));
    assert!(plan.path.iter().all(|p| p.1 == 1));
}

#[test]
fn falls_within_max_fall_are_taken_and_longer_ones_are_not() {
    for (drop, max, expect) in [(3, 3, true), (4, 3, false), (4, 5, true)] {
        let mut t = open(8, 3);
        for x in 3..8 {
            for z in 0..3 {
                t.set((x, 0, z), Class::PASSABLE);
                t.set((x, -drop, z), Class::STANDABLE);
                for y in (-drop + 1)..0 {
                    t.set((x, y, z), Class::PASSABLE);
                }
            }
        }
        let params = Params {
            max_fall: max,
            ..Params::default()
        };
        let plan = solve(&mut t, (0, 1, 1), block(6, 1 - drop, 1), params);
        assert_eq!(plan.complete, expect, "drop {drop} max {max}");
    }
}

#[test]
fn a_water_landing_is_free_of_the_fall_limit() {
    let mut t = open(8, 3);
    for x in 3..8 {
        for z in 0..3 {
            t.set((x, 0, z), Class::PASSABLE);
            for y in -9..0 {
                t.set((x, y, z), Class::PASSABLE.with(Class::WATER));
            }
            t.set((x, -10, z), Class::STANDABLE);
        }
    }
    let plan = solve(&mut t, (0, 1, 1), block(6, -9, 1), Params::default());
    assert!(plan.complete);
}

#[test]
fn lava_is_walked_around() {
    let mut t = open(10, 8);
    for z in 0..6 {
        t.set((4, 1, z), Class::DANGER);
    }
    let plan = solve(&mut t, (0, 1, 0), block(8, 1, 0), Params::default());
    assert!(plan.complete);
    assert!(plan.path.iter().all(|p| p.0 != 4 || p.2 >= 6));
    assert!(plan.path.iter().all(|p| !(p.0 == 4 && p.2 < 6)));
}

#[test]
fn a_cell_beside_a_danger_costs_more_than_one_clear_of_it() {
    let mut t = open(8, 8);
    t.set((3, 1, 3), Class::DANGER);
    let mut edges = Vec::new();
    Traverse
        .expand(&mut t, &Params::default(), (3, 1, 1), &mut edges)
        .unwrap();
    let near = edges.iter().find(|e| e.to == (3, 1, 2)).unwrap().cost;
    let far = edges.iter().find(|e| e.to == (2, 1, 1)).unwrap().cost;
    assert!(near > far);
}

#[test]
fn an_unreachable_goal_returns_the_closest_partial_path() {
    let mut t = open(12, 12);
    for i in 7..=9 {
        for y in 1..=2 {
            for (x, z) in [(i, 7), (i, 9), (7, i), (9, i)] {
                t.wall((x, y, z));
            }
        }
    }
    let plan = solve(&mut t, (0, 1, 0), block(8, 1, 8), Params::default());
    assert!(!plan.complete);
    let end = *plan.path.last().unwrap();
    assert!(plan.path.len() > 1);
    assert!(heuristic(&block(8, 1, 8), end) < heuristic(&block(8, 1, 8), (0, 1, 0)) / 2.0);
}

#[test]
fn any_of_picks_the_cheapest_goal() {
    let mut t = open(32, 32);
    let goal = Goal::AnyOf(vec![
        LeafGoal::Block(BlockPos { x: 20, y: 1, z: 0 }),
        LeafGoal::Block(BlockPos { x: 3, y: 1, z: 0 }),
        LeafGoal::Xz(XzGoal { x: 0, z: 25 }),
    ]);
    let plan = solve(&mut t, (0, 1, 0), goal, Params::default());
    assert!(plan.complete);
    assert_eq!(plan.path.last(), Some(&(3, 1, 0)));
}

#[test]
fn the_other_goal_kinds_are_reached() {
    let mut t = open(32, 32);
    let near = Goal::Leaf(LeafGoal::Near(NearGoal {
        pos: BlockPos { x: 10, y: 1, z: 0 },
        radius: 3.0,
    }));
    let plan = solve(&mut t, (0, 1, 0), near, Params::default());
    assert!(plan.complete);
    assert!(plan.path.len() <= 9);
    let get = Goal::Leaf(LeafGoal::GetToBlock(BlockPos { x: 6, y: 1, z: 6 }));
    let plan = solve(&mut t, (0, 1, 0), get, Params::default());
    let end = plan.path.last().unwrap();
    assert_eq!((end.0 - 6).abs() + (end.1 - 1).abs() + (end.2 - 6).abs(), 1);
    let away = Goal::Leaf(LeafGoal::RunAway(RunAwayGoal {
        origin: BlockPos { x: 0, y: 1, z: 0 },
        distance: 6.0,
    }));
    let plan = solve(&mut t, (0, 1, 0), away, Params::default());
    assert!(plan.complete);
    let end = plan.path.last().unwrap();
    assert!(f64::from(end.0 * end.0 + end.2 * end.2) >= 30.0);
}

#[test]
fn a_y_level_goal_climbs_stairs() {
    let mut t = open(16, 4);
    for x in 1..=4 {
        for y in 0..=x {
            t.wall((x, y, 0));
        }
    }
    let plan = solve(
        &mut t,
        (0, 1, 0),
        Goal::Leaf(LeafGoal::YLevel(4)),
        Params::default(),
    );
    assert!(plan.complete);
    assert_eq!(plan.path.last().unwrap().1, 4);
}

#[test]
fn ladders_are_climbed() {
    let mut t = open(8, 4);
    for y in 1..=5 {
        t.set((2, y, 1), Class::PASSABLE.with(Class::CLIMBABLE));
        t.wall((3, y, 1));
    }
    t.wall((2, 0, 1));
    t.wall((2, 5, 2));
    t.set((2, 6, 1), Class::PASSABLE);
    t.wall((2, 5, 2));
    let plan = solve(&mut t, (2, 1, 1), block(2, 6, 2), Params::default());
    assert!(plan.complete, "{:?}", plan.path);
}

#[test]
fn a_policy_avoid_box_is_kept_out_of() {
    let mut t = open(16, 8);
    let params = Params {
        avoid: vec![Aabb {
            min: Vec3 {
                x: 3.0,
                y: 0.0,
                z: 0.0,
            },
            max: Vec3 {
                x: 5.0,
                y: 3.0,
                z: 4.0,
            },
        }],
        ..Params::default()
    };
    let plan = solve(&mut t, (0, 1, 0), block(8, 1, 0), params);
    assert!(plan.complete);
    assert!(
        plan.path
            .iter()
            .all(|p| !((3..5).contains(&p.0) && p.2 < 4))
    );
}

#[test]
fn a_small_node_budget_finds_the_same_path_as_an_unbounded_run() {
    let build = || {
        let mut t = open(20, 20);
        for z in 0..15 {
            t.wall((6, 1, z));
            t.wall((6, 2, z));
            t.wall((12, 1, 19 - z));
            t.wall((12, 2, 19 - z));
        }
        t
    };
    let goal = block(18, 1, 2);
    let whole = solve(&mut build(), (0, 1, 0), goal.clone(), Params::default());
    let mut t = build();
    let moves = default_moves();
    let mut search = Search::new((0, 1, 0), goal, Params::default());
    let limits = Limits {
        nodes_per_step: 7,
        total_nodes: 50_000,
    };
    let mut calls = 0;
    let stepped = loop {
        calls += 1;
        assert!(search.expanded() <= calls * 7);
        if let Status::Done(plan) = search.step(&mut t, &moves, limits) {
            break plan;
        }
    };
    assert!(calls > 3);
    assert_eq!(stepped, whole);
}

#[test]
fn a_starved_read_budget_pauses_the_search_without_changing_its_result() {
    let world = Rc::new(RefCell::new(open(64, 64)));
    let goal = block(40, 1, 33);
    let expected = solve(
        &mut open(64, 64),
        (0, 1, 0),
        goal.clone(),
        Params::default(),
    );
    let mut cache = BlockCache::new(TerrainSource(world));
    let moves = default_moves();
    let mut search = Search::new((0, 1, 0), goal, Params::default());
    let mut ticks = 0;
    let plan = loop {
        ticks += 1;
        assert!(ticks < 500);
        cache.begin_tick(1);
        if let Status::Done(plan) = search.step(&mut cache, &moves, Limits::default()) {
            break plan;
        }
    };
    assert!(ticks > 2);
    assert_eq!(plan, expected);
}

#[test]
fn a_block_event_changes_the_next_plan() {
    let world = Rc::new(RefCell::new(open(32, 8)));
    let mut cache = BlockCache::new(TerrainSource(world.clone()));
    let goal = block(10, 1, 3);
    let before = solve(&mut cache, (0, 1, 3), goal.clone(), Params::default());
    assert_eq!(before.path.len(), 11);
    for y in 1..=2 {
        for z in 0..8 {
            if z != 7 {
                world.borrow_mut().wall((5, y, z));
            }
        }
    }
    let stale = solve(&mut cache, (0, 1, 3), goal.clone(), Params::default());
    assert_eq!(stale, before);
    cache.invalidate_box((5, 0, 0), (5, 3, 7));
    let after = solve(&mut cache, (0, 1, 3), goal, Params::default());
    assert!(after.complete);
    assert!(after.cost > before.cost);
    assert!(after.path.iter().any(|p| p.2 == 7));
}

fn walk(planner: &mut Planner, world: &mut dyn Blocks, mut at: Pos, ticks: usize) -> (Pos, bool) {
    for _ in 0..ticks {
        match planner.tick(at, world) {
            Advance::Waypoint(next) => at = next,
            Advance::Searching => {}
            Advance::Arrived => return (at, true),
            Advance::Failed(_) => return (at, false),
        }
    }
    (at, false)
}

#[test]
fn a_far_goal_is_reached_in_segments() {
    let mut t = open(80, 8);
    let limits = Limits {
        nodes_per_step: 30,
        total_nodes: 120,
    };
    let mut planner = Planner::new(block(70, 1, 3), Params::default()).with_limits(limits);
    let (end, arrived) = walk(&mut planner, &mut t, (0, 1, 3), 2000);
    assert!(arrived, "stopped at {end:?}");
    assert_eq!(end, (70, 1, 3));
}

#[test]
fn a_blocked_path_is_replanned_after_a_block_event() {
    let mut t = open(40, 12);
    let mut planner = Planner::new(block(30, 1, 6), Params::default());
    let mut at = (0, 1, 6);
    for _ in 0..4 {
        if let Advance::Waypoint(next) = planner.tick(at, &mut t) {
            at = next;
        }
    }
    for y in 1..=2 {
        for z in 0..11 {
            t.wall((15, y, z));
        }
    }
    planner.mark_dirty();
    let (end, arrived) = walk(&mut planner, &mut t, at, 400);
    assert!(arrived, "stopped at {end:?}");
    assert!(planner.path().iter().any(|p| p.2 == 11));
}

#[test]
fn a_planner_with_no_way_forward_fails() {
    let mut t = open(10, 10);
    for y in 1..=2 {
        for x in 0..10 {
            t.wall((x, y, 5));
        }
    }
    let mut planner = Planner::new(block(5, 1, 8), Params::default());
    let mut at = (5, 1, 0);
    let mut failed = false;
    for _ in 0..200 {
        match planner.tick(at, &mut t) {
            Advance::Waypoint(next) => at = next,
            Advance::Failed(_) => {
                failed = true;
                break;
            }
            _ => {}
        }
    }
    assert!(failed || at.2 == 4);
}

#[test]
fn fall_time_grows_with_the_drop() {
    assert!(fall_ticks(1) < fall_ticks(3));
    assert!(fall_ticks(3) < fall_ticks(10));
}

fn tools(table: &[(&'static str, u32)]) -> Rc<RefCell<dyn Tools>> {
    Rc::new(RefCell::new(crate::world::tests::TableTools(
        table.to_vec(),
    )))
}

fn breaking(table: &[(&'static str, u32)], deny: &[&str], allow: &[&str]) -> Breaking {
    Breaking {
        allow: allow.iter().map(|s| s.to_string()).collect(),
        deny: deny.iter().map(|s| s.to_string()).collect(),
        tools: tools(table),
    }
}

fn digger() -> Params {
    Params {
        breaking: Some(breaking(&[("stone", 30), ("white_wool", 5)], &[], &[])),
        ..Params::default()
    }
}

fn gap(t: &mut Terrain, xs: std::ops::Range<i32>, d: i32) {
    for x in xs {
        for z in 0..d {
            for y in -39..=0 {
                t.set((x, y, z), Class::PASSABLE);
            }
        }
    }
}

fn parkour() -> Params {
    Params {
        allow_parkour: true,
        ..Params::default()
    }
}

/// A one-wide corridor along x with a three-high stone wall at x = 5.
fn walled_corridor() -> Terrain {
    let mut t = open(12, 1);
    for y in 1..=3 {
        t.wall((5, y, 0));
    }
    t
}

#[test]
fn a_wall_is_tunnelled_only_when_breaking_is_allowed() {
    let plan = solve(
        &mut walled_corridor(),
        (0, 1, 0),
        block(9, 1, 0),
        Params::default(),
    );
    assert!(!plan.complete);
    let plan = solve(&mut walled_corridor(), (0, 1, 0), block(9, 1, 0), digger());
    assert!(plan.complete, "{plan:?}");
    assert!(plan.path.contains(&(5, 1, 0)));
    let walk = 9.0 * SPRINT_TICKS;
    let dig = 2.0 * (30.0 + BREAK_OVERHEAD_TICKS);
    assert!((plan.cost - walk - dig).abs() < 1e-9, "{}", plan.cost);
}

#[test]
fn a_short_detour_beats_breaking_and_a_long_one_does_not() {
    let wall = |gap_z: i32, depth: i32| {
        let mut t = open(12, depth);
        for z in 0..depth {
            if z != gap_z {
                for y in 1..=3 {
                    t.wall((5, y, z));
                }
            }
        }
        t
    };
    let plan = solve(&mut wall(2, 3), (0, 1, 0), block(9, 1, 0), digger());
    assert!(plan.complete);
    assert!(plan.path.contains(&(5, 1, 2)), "{:?}", plan.path);
    let plan = solve(&mut wall(30, 31), (0, 1, 0), block(9, 1, 0), digger());
    assert!(plan.complete);
    assert!(plan.path.contains(&(5, 1, 0)), "{:?}", plan.path);
}

#[test]
fn deny_and_allow_lists_keep_blocks_whole() {
    for b in [
        breaking(&[("stone", 30)], &["minecraft:stone"], &[]),
        breaking(&[("stone", 30)], &[], &["white_wool"]),
        breaking(&[], &[], &[]),
    ] {
        let params = Params {
            breaking: Some(b),
            ..Params::default()
        };
        let plan = solve(&mut walled_corridor(), (0, 1, 0), block(9, 1, 0), params);
        assert!(!plan.complete, "{plan:?}");
    }
    let params = Params {
        breaking: Some(breaking(&[("stone", 30)], &[], &["stone"])),
        ..Params::default()
    };
    let plan = solve(&mut walled_corridor(), (0, 1, 0), block(9, 1, 0), params);
    assert!(plan.complete);
}

#[test]
fn nothing_is_broken_that_lets_sand_or_water_onto_the_bot() {
    let mut sand = open(12, 1);
    sand.wall((5, 1, 0));
    sand.wall((5, 2, 0));
    sand.block((5, 3, 0), "sand", Class::STANDABLE.with(Class::FALLING));
    let plan = solve(&mut sand, (0, 1, 0), block(9, 1, 0), digger());
    assert!(!plan.complete, "{plan:?}");

    let mut wet = open(12, 3);
    for z in 0..3 {
        for y in 1..=3 {
            wet.wall((5, y, z));
        }
    }
    wet.block(
        (6, 1, 0),
        "water",
        Class::PASSABLE.with(Class::WATER).with(Class::FLUID),
    );
    wet.block((6, 1, 1), "stone", Class::STANDABLE);
    let mut edges = Vec::new();
    Traverse
        .expand(&mut wet, &digger(), (4, 1, 0), &mut edges)
        .unwrap();
    assert!(edges.iter().all(|e| e.to != (5, 1, 0)), "{edges:?}");
    Traverse
        .expand(&mut wet, &digger(), (4, 1, 2), &mut edges)
        .unwrap();
    assert!(edges.iter().any(|e| e.to == (5, 1, 2)), "{edges:?}");
}

/// Two islands on a one-wide strip with a void gap of six at x = 4..=9.
fn void_gap() -> Terrain {
    let mut t = open(16, 1);
    for x in 4..=9 {
        for y in -39..=0 {
            t.set((x, y, 0), Class::PASSABLE);
        }
    }
    t
}

#[test]
fn a_void_gap_is_bridged_with_enough_blocks_and_refused_without() {
    let placer = |n| Params {
        throwaway: n,
        ..Params::default()
    };
    let plan = solve(&mut void_gap(), (2, 1, 0), block(12, 1, 0), placer(6));
    assert!(plan.complete, "{plan:?}");
    assert_eq!(plan.placed.last(), Some(&6));
    assert!(plan.path.iter().all(|p| p.1 == 1));
    let plan = solve(&mut void_gap(), (2, 1, 0), block(12, 1, 0), placer(5));
    assert!(!plan.complete);
    let plan = solve(&mut void_gap(), (2, 1, 0), block(12, 1, 0), placer(0));
    assert!(!plan.complete);
}

#[test]
fn pillars_up_three_with_three_blocks() {
    let pillar = |n| Params {
        throwaway: n,
        ..Params::default()
    };
    let plan = solve(&mut open(4, 1), (1, 1, 0), block(1, 4, 0), pillar(3));
    assert!(plan.complete, "{plan:?}");
    let ys: Vec<i32> = plan.path.iter().map(|p| p.1).collect();
    assert_eq!(ys, vec![1, 2, 3, 4]);
    assert_eq!(plan.placed, vec![0, 1, 2, 3]);
    let plan = solve(&mut open(4, 1), (1, 1, 0), block(1, 4, 0), pillar(2));
    assert!(!plan.complete);
}

#[test]
fn a_tunnel_steps_up_and_down_through_solid_rock() {
    let mut t = open(12, 1);
    for x in 3..=8 {
        for y in 1..=6 {
            t.wall((x, y, 0));
        }
    }
    let plan = solve(&mut t, (0, 1, 0), block(5, 2, 0), digger());
    assert!(plan.complete, "{plan:?}");
    let plan = solve(&mut t, (5, 2, 0), block(11, 1, 0), digger());
    assert!(plan.complete, "{plan:?}");
}

fn jumps(t: &mut Terrain, from: Pos, params: &Params) -> Vec<Pos> {
    let mut edges = Vec::new();
    Parkour.expand(t, params, from, &mut edges).unwrap();
    edges.into_iter().map(|e| e.to).collect()
}

#[test]
fn flat_gaps_of_one_to_three_are_jumped() {
    for width in 1..=3 {
        let mut t = open(16, 1);
        gap(&mut t, 3..3 + width, 1);
        let to = (3 + width, 1, 0);
        assert_eq!(
            jumps(&mut t, (2, 1, 0), &parkour()),
            vec![to],
            "gap {width}"
        );
        let plan = solve(&mut t, (0, 1, 0), block(10, 1, 0), parkour());
        assert!(plan.complete, "gap {width}");
        assert!(plan.path.contains(&(2, 1, 0)) && plan.path.contains(&to));
    }
}

#[test]
fn a_four_block_gap_is_not_jumped() {
    let mut t = open(16, 1);
    gap(&mut t, 3..7, 1);
    assert!(jumps(&mut t, (2, 1, 0), &parkour()).is_empty());
    assert!(!solve(&mut t, (0, 1, 0), block(10, 1, 0), parkour()).complete);
}

#[test]
fn parkour_needs_the_policy() {
    let mut t = open(16, 1);
    gap(&mut t, 3..4, 1);
    assert!(jumps(&mut t, (2, 1, 0), &Params::default()).is_empty());
    assert!(!solve(&mut t, (0, 1, 0), block(10, 1, 0), Params::default()).complete);
}

#[test]
fn a_three_block_gap_needs_run_up_and_sprint() {
    let mut t = open(16, 1);
    gap(&mut t, 3..6, 1);
    t.wall((1, 1, 0));
    t.wall((1, 2, 0));
    assert!(jumps(&mut t, (2, 1, 0), &parkour()).is_empty());
    let mut t = open(16, 1);
    gap(&mut t, 3..6, 1);
    let walk = Params {
        allow_sprint: false,
        ..parkour()
    };
    assert!(jumps(&mut t, (2, 1, 0), &walk).is_empty());
}

#[test]
fn one_up_gaps_of_one_and_two_are_jumped() {
    for width in 1..=3 {
        let mut t = open(16, 1);
        gap(&mut t, 3..3 + width, 1);
        for x in 3 + width..16 {
            t.wall((x, 1, 0));
        }
        let got = jumps(&mut t, (2, 1, 0), &parkour());
        if width <= 2 {
            assert_eq!(got, vec![(3 + width, 2, 0)], "gap {width}");
        } else {
            assert!(got.is_empty(), "gap {width}");
        }
    }
}

#[test]
fn a_low_ceiling_blocks_the_jump() {
    let mut t = open(16, 1);
    gap(&mut t, 3..5, 1);
    t.wall((3, 3, 0));
    assert!(jumps(&mut t, (2, 1, 0), &parkour()).is_empty());
}

#[test]
fn lava_is_jumped_over_but_never_entered() {
    let mut t = open(16, 1);
    for x in 3..5 {
        t.set((x, 0, 0), Class::DANGER);
    }
    let plan = solve(&mut t, (0, 1, 0), block(10, 1, 0), parkour());
    assert!(plan.complete);
    assert!(
        plan.path
            .iter()
            .all(|p| !t.class(*p).danger() && !t.class((p.0, p.1 - 1, p.2)).danger())
    );
    assert!(!solve(&mut t, (0, 1, 0), block(10, 1, 0), Params::default()).complete);
}

#[test]
fn a_surface_swim_is_cheaper_than_a_submerged_one() {
    let water = Class::PASSABLE.with(Class::WATER);
    let cost = |deep: bool| {
        let mut t = open(10, 3);
        for x in 3..7 {
            for z in 0..3 {
                t.set((x, 1, z), water);
                if deep {
                    t.set((x, 2, z), water);
                }
            }
        }
        solve(&mut t, (0, 1, 1), block(9, 1, 1), Params::default()).cost
    };
    assert!(cost(true) > cost(false), "{} {}", cost(true), cost(false));
}
