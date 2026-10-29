// SPDX-License-Identifier: MIT
use tuff_bots_api::control::*;

use std::cell::RefCell;
use std::rc::Rc;

use super::Face;
use super::double::Double;
use crate::core::{Bots, Event};
use crate::world::Class;
use crate::world::tests::{TableTools, Terrain};

fn spec_at(x: f64, y: f64, z: f64) -> SpawnSpec {
    SpawnSpec {
        name: "Walker".into(),
        model: Model::Classic,
        location: Location {
            dimension: None,
            pos: Vec3 { x, y, z },
            yaw: None,
            pitch: None,
        },
        skill: Skill {
            reaction_ticks: 0,
            aim_error_degrees: 0.0,
            turn_rate: 30.0,
            click_cps_cap: 10.0,
            attack_charge: 0.9,
        },
        policy: Policy {
            allow_break: false,
            allow_place: false,
            allow_sprint: true,
            allow_parkour: false,
            max_fall: 3,
            break_allow: vec![],
            break_deny: vec![],
            throwaway_items: vec![],
            avoid: vec![],
            auto_eat: false,
            defend: false,
        },
        respawn: RespawnPolicy::Owner,
    }
}

fn block(x: i32, y: i32, z: i32) -> Goal {
    Goal::Leaf(LeafGoal::Block(BlockPos { x, y, z }))
}

struct Run {
    bots: Bots,
    host: Double,
    id: BotId,
}

impl Run {
    fn new(terrain: Terrain, goal: Goal) -> Self {
        Self::from(terrain, (0.5, 1.0, 0.5), goal)
    }

    fn from(terrain: Terrain, at: (f64, f64, f64), goal: Goal) -> Self {
        Self::with(terrain, at, Process::Goto(goal))
    }

    fn with(terrain: Terrain, at: (f64, f64, f64), process: Process) -> Self {
        let mut host = Double::new(terrain, at);
        let mut bots = Bots::default();
        let id = bots
            .spawn(&mut host, "a", spec_at(at.0, at.1, at.2))
            .unwrap();
        bots.set_process("a", id, process).unwrap();
        Self { bots, host, id }
    }

    fn trace(&self) -> String {
        format!(
            "{:?} {:?} {:?}",
            self.host.pos, self.host.input, self.host.events
        )
    }

    fn tick(&mut self) {
        self.bots.tick(&mut self.host);
        self.host.physics();
    }

    fn run(&mut self, max: usize) -> usize {
        for n in 0..max {
            self.tick();
            if self.finished() {
                return n;
            }
        }
        max
    }

    fn finished(&self) -> bool {
        self.host
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::ProcessDone | Event::ProcessFailed(_)))
    }

    fn last_event(&self) -> Option<&Event> {
        self.host.events.last().map(|(_, e)| e)
    }
}

#[test]
fn walks_a_flat_run_and_sends_inputs_rarely() {
    let mut run = Run::new(Terrain::new(32, 8), block(20, 1, 0));
    let ticks = run.run(400);
    assert_eq!(run.last_event(), Some(&Event::ProcessDone));
    assert!(ticks < 150, "took {ticks} ticks");
    assert!(run.host.drives < 20, "{} host calls", run.host.drives);
    assert_eq!(run.bots.state("a", run.id).unwrap().process, Process::Idle);
    assert_eq!(run.host.input.forward, 0, "inputs are released");
}

#[test]
fn climbs_stairs() {
    let mut t = Terrain::new(32, 8);
    for step in 1..=4 {
        for x in (step * 3)..32 {
            for z in 0..8 {
                t.set((x, step, z), Class::STANDABLE);
            }
        }
    }
    let mut run = Run::new(t, block(14, 5, 0));
    run.run(600);
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessDone),
        "{}",
        run.trace()
    );
    assert!(run.host.pos.y >= 5.0 - 1e-6);
}

#[test]
fn drops_three_blocks_to_a_lower_floor() {
    let mut t = Terrain::new(32, 8);
    for x in 0..8 {
        for z in 0..8 {
            for y in 0..=3 {
                t.set((x, y, z), Class::STANDABLE);
            }
        }
    }
    let mut run = Run::new(t, block(14, 1, 0));
    run.host.pos.y = 4.0;
    run.bots
        .set_process("a", run.id, Process::Goto(block(14, 1, 0)))
        .unwrap();
    run.run(600);
    assert_eq!(run.last_event(), Some(&Event::ProcessDone));
    assert!(run.host.worst_fall <= 3.0 + 1e-6);
}

#[test]
fn goes_around_a_wall() {
    let mut t = Terrain::new(32, 12);
    for z in 0..9 {
        t.wall((8, 1, z));
        t.wall((8, 2, z));
    }
    let mut run = Run::new(t, block(16, 1, 0));
    run.run(800);
    assert_eq!(run.last_event(), Some(&Event::ProcessDone));
}

#[test]
fn recovers_from_being_pushed_off_the_path() {
    let mut run = Run::new(Terrain::new(32, 12), block(20, 1, 0));
    for _ in 0..25 {
        run.tick();
    }
    assert!(!run.finished());
    run.host.push(0.0, 5.0);
    run.run(600);
    assert_eq!(run.last_event(), Some(&Event::ProcessDone));
}

#[test]
fn reports_an_unreachable_goal() {
    let mut t = Terrain::new(32, 8);
    for z in 0..8 {
        t.wall((6, 1, z));
        t.wall((6, 2, z));
        t.wall((6, 3, z));
        t.wall((6, 4, z));
    }
    let mut run = Run::new(t, block(12, 1, 0));
    run.run(2000);
    assert!(
        matches!(run.last_event(), Some(Event::ProcessFailed(_))),
        "{}",
        run.trace()
    );
}

#[test]
fn reports_stuck_when_the_body_cannot_move() {
    let mut run = Run::from(Terrain::new(32, 16), (8.5, 1.0, 8.5), block(20, 1, 8));
    for _ in 0..2000 {
        run.bots.tick(&mut run.host);
        if run.finished() {
            break;
        }
    }
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessFailed("stuck".into()))
    );
}

#[test]
fn a_goto_fails_when_the_bot_dies() {
    let mut run = Run::new(Terrain::new(32, 8), block(20, 1, 0));
    for _ in 0..5 {
        run.tick();
    }
    run.host.alive = false;
    run.tick();
    assert_eq!(
        run.host
            .events
            .iter()
            .map(|(_, e)| e.clone())
            .collect::<Vec<_>>(),
        vec![
            Event::Spawned,
            Event::Died("unknown".into()),
            Event::ProcessFailed("died".into())
        ]
    );
    assert_eq!(run.bots.state("a", run.id).unwrap().process, Process::Idle);
}

#[test]
fn stopping_releases_held_inputs() {
    let mut run = Run::new(Terrain::new(32, 8), block(20, 1, 0));
    for _ in 0..10 {
        run.tick();
    }
    assert_eq!(run.host.input.forward, 1);
    run.bots.stop("a", run.id).unwrap();
    run.tick();
    assert_eq!(run.host.input.forward, 0);
}

#[test]
fn a_block_change_ahead_replans_around_it() {
    let mut run = Run::new(Terrain::new(32, 12), block(20, 1, 0));
    for _ in 0..20 {
        run.tick();
    }
    for z in 0..6 {
        run.host.terrain.borrow_mut().wall((12, 1, z));
        run.host.terrain.borrow_mut().wall((12, 2, z));
    }
    run.bots.mark_dirty();
    run.run(800);
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessDone),
        "{}",
        run.trace()
    );
}

fn flat(x: f64) -> Vec3 {
    Vec3 { x, y: 1.0, z: 0.5 }
}

#[test]
fn follows_a_moving_target_and_stays_within_range() {
    let spec = FollowSpec {
        target: "t".into(),
        range: 4.0,
    };
    let mut run = Run::with(Terrain::new(64, 8), (0.5, 1.0, 0.5), Process::Follow(spec));
    let mut far = 0;
    for tick in 0..200 {
        run.host.target = Some(flat(12.5 + f64::from(tick) * 0.1));
        run.tick();
        if tick >= 60
            && super::super::follow::distance(&run.host.pos, &flat(12.5 + f64::from(tick) * 0.1))
                > 5.5
        {
            far += 1;
        }
    }
    assert_eq!(far, 0, "{}", run.trace());
    assert!(run.host.pos.x > 20.0, "{}", run.trace());
    assert!(!run.finished(), "following never completes");
}

#[test]
fn a_follower_stops_when_the_target_is_gone() {
    let spec = FollowSpec {
        target: "t".into(),
        range: 3.0,
    };
    let mut run = Run::with(Terrain::new(64, 8), (0.5, 1.0, 0.5), Process::Follow(spec));
    run.host.target = Some(flat(20.5));
    for _ in 0..20 {
        run.tick();
    }
    run.host.target = None;
    run.tick();
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessFailed("target gone".into()))
    );
    assert_eq!(run.bots.state("a", run.id).unwrap().process, Process::Idle);
}

#[test]
fn explores_every_chunk_of_the_disc_then_finishes() {
    let spec = ExploreSpec {
        origin: BlockPos { x: 24, y: 1, z: 24 },
        radius: 16,
    };
    let mut run = Run::with(
        Terrain::new(64, 64),
        (24.5, 1.0, 24.5),
        Process::Explore(spec),
    );
    let ticks = run.run(3000);
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessDone),
        "{ticks} {}",
        run.trace()
    );
}

fn mine_spec(count: u32) -> Process {
    Process::Mine(MineSpec {
        blocks: vec!["iron_ore".into()],
        count,
        radius: 32,
    })
}

#[test]
fn mines_the_requested_ores_nearest_first_and_finishes() {
    let mut t = Terrain::new(32, 8);
    for p in [(6, 1, 3), (3, 1, 2), (10, 1, 5), (12, 1, 6)] {
        t.ore(p);
    }
    let mut run = Run::with(t, (0.5, 1.0, 0.5), mine_spec(3));
    run.run(1500);
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessDone),
        "{}",
        run.trace()
    );
    assert_eq!(run.host.digs.len(), 3);
    assert_eq!(run.host.digs[0], (3, 1, 2));
    assert!(run.host.terrain.borrow().ores.contains(&(12, 1, 6)));
    assert_eq!(run.bots.state("a", run.id).unwrap().process, Process::Idle);
}

#[test]
fn ores_beside_lava_are_left_alone() {
    let mut t = Terrain::new(32, 8);
    t.ore((4, 1, 2));
    t.set((5, 1, 2), Class::DANGER);
    t.ore((8, 1, 2));
    let mut run = Run::with(t, (0.5, 1.0, 0.5), mine_spec(1));
    run.run(1500);
    assert_eq!(run.host.digs, [(8, 1, 2)], "{}", run.trace());
}

#[test]
fn ores_under_gravel_are_left_alone() {
    let mut t = Terrain::new(32, 8);
    t.ore((4, 1, 2));
    t.block((4, 2, 2), "minecraft:gravel", Class::STANDABLE);
    t.ore((8, 1, 2));
    let mut run = Run::with(t, (0.5, 1.0, 0.5), mine_spec(1));
    run.run(1500);
    assert_eq!(run.host.digs, [(8, 1, 2)], "{}", run.trace());
}

#[test]
fn with_no_ore_known_the_miner_tunnels_forward_and_not_into_lava() {
    let mut t = Terrain::new(32, 8);
    for x in 1..=5 {
        t.wall((x, 1, 0));
        t.wall((x, 2, 0));
    }
    t.set((6, 1, 1), Class::DANGER);
    t.wall((6, 1, 0));
    t.wall((6, 2, 0));
    let mut run = Run::with(t, (0.5, 1.0, 0.5), mine_spec(1));
    run.run(1500);
    assert!(run.host.digs.contains(&(5, 2, 0)), "{}", run.trace());
    assert!(!run.host.digs.contains(&(6, 1, 0)), "{}", run.trace());
}

#[test]
fn get_to_block_stops_next_to_the_block_without_digging() {
    let mut t = Terrain::new(32, 8);
    t.ore((9, 1, 2));
    let process = Process::GetToBlock(GetToBlockSpec {
        blocks: vec!["iron_ore".into()],
        radius: 32,
    });
    let mut run = Run::with(t, (0.5, 1.0, 0.5), process);
    run.run(1000);
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessDone),
        "{}",
        run.trace()
    );
    assert!(run.host.digs.is_empty());
    let (x, z) = (run.host.pos.x, run.host.pos.z);
    assert!((x - 9.5).abs() + (z - 2.5).abs() < 2.1, "{x} {z}");
}

#[test]
fn mining_with_no_ore_wanders_to_look_for_more_and_never_digs() {
    let t = Terrain::new(64, 8);
    let mut run = Run::with(t, (0.5, 1.0, 0.5), mine_spec(1));
    run.run(400);
    assert!(run.host.digs.is_empty());
    assert!(
        run.host.pos.x > 8.0 || run.host.pos.z > 4.0,
        "{}",
        run.trace()
    );
    assert!(matches!(
        run.bots.state("a", run.id).unwrap().process,
        Process::Mine(_)
    ));
}

fn builder_policy() -> Policy {
    Policy {
        allow_break: true,
        allow_place: true,
        allow_parkour: true,
        allow_sprint: true,
        throwaway_items: vec!["minecraft:cobblestone".into()],
        ..spec_at(0.0, 0.0, 0.0).policy
    }
}

impl Run {
    /// Lets the bot break and place, with `blocks` cobblestone in hotbar
    /// slot 1 and wool breaking in 5 ticks, stone in 30.
    fn builder(mut self, blocks: u32) -> Self {
        self.bots
            .set_policy("a", self.id, builder_policy())
            .unwrap();
        self.host.hotbar = vec![(1, "minecraft:cobblestone".into(), blocks)];
        self.host.tools = Some(Rc::new(RefCell::new(TableTools(vec![
            ("white_wool", 5),
            ("stone", 30),
            ("cobblestone", 10),
        ]))));
        self
    }
}

/// Two islands on a strip three wide, a void gap at x = 4..=9 between.
fn islands() -> Terrain {
    let mut t = Terrain::new(16, 3);
    for x in 4..=9 {
        for z in 0..3 {
            for y in -39..=0 {
                t.set((x, y, z), Class::PASSABLE);
            }
        }
    }
    t
}

fn finished_with(run: &Run, event: Event) {
    assert_eq!(run.last_event(), Some(&event), "{}", run.trace());
}

#[test]
fn bridges_a_void_gap_spending_one_block_per_cell() {
    let mut run = Run::from(islands(), (2.5, 1.0, 1.5), block(12, 1, 1)).builder(10);
    run.run(2000);
    finished_with(&run, Event::ProcessDone);
    assert_eq!(run.host.carried("minecraft:cobblestone"), 4);
    assert_eq!(run.host.placed.len(), 6);
    assert!(
        run.host
            .placed
            .iter()
            .all(|p| p.1 == 0 && (4..=9).contains(&p.0))
    );
    assert!(run.host.lowest >= 1.0 - 1e-6, "fell to {}", run.host.lowest);
}

#[test]
fn refuses_a_gap_it_has_too_few_blocks_for() {
    let mut run = Run::from(islands(), (2.5, 1.0, 1.5), block(12, 1, 1)).builder(5);
    run.run(2000);
    finished_with(&run, Event::ProcessFailed("no path".into()));
    assert_eq!(run.host.carried("minecraft:cobblestone"), 5);
    assert!(run.host.placed.is_empty());
}

#[test]
fn tunnels_through_a_wool_wall() {
    let mut t = Terrain::new(16, 3);
    for z in 0..3 {
        for y in 1..=4 {
            t.block((6, y, z), "minecraft:white_wool", Class::STANDABLE);
        }
    }
    let mut run = Run::from(t, (1.5, 1.0, 1.5), block(10, 1, 1)).builder(0);
    run.run(1500);
    finished_with(&run, Event::ProcessDone);
    let mut dug = run.host.digs.clone();
    dug.sort();
    assert_eq!(dug, [(6, 1, 1), (6, 2, 1)]);
    assert_eq!(run.host.selected, 0);
}

#[test]
fn pillars_up_three_blocks() {
    let mut run = Run::from(Terrain::new(4, 1), (1.5, 1.0, 0.5), block(1, 4, 0)).builder(3);
    run.run(1000);
    finished_with(&run, Event::ProcessDone);
    assert_eq!(run.host.placed, [(1, 1, 0), (1, 2, 0), (1, 3, 0)]);
    assert_eq!(run.host.carried("minecraft:cobblestone"), 0);
}

#[test]
fn the_break_process_walks_into_reach_and_breaks_each_block() {
    let mut t = Terrain::new(24, 3);
    t.block((15, 1, 1), "minecraft:white_wool", Class::STANDABLE);
    t.block((15, 2, 1), "minecraft:white_wool", Class::STANDABLE);
    let targets = vec![
        BlockPos { x: 15, y: 1, z: 1 },
        BlockPos { x: 15, y: 2, z: 1 },
    ];
    let mut run = Run::with(t, (1.5, 1.0, 1.5), Process::Break(targets)).builder(0);
    run.run(1000);
    finished_with(&run, Event::ProcessDone);
    assert_eq!(run.host.digs, [(15, 1, 1), (15, 2, 1)]);
    let eye = (run.host.pos.x - 15.5).hypot(run.host.pos.z - 1.5);
    assert!(eye < 4.5, "dug from {eye} away");
}

#[test]
fn the_break_process_fails_on_an_unbreakable_block() {
    let mut t = Terrain::new(8, 3);
    t.block((3, 1, 1), "minecraft:bedrock", Class::STANDABLE);
    let targets = vec![BlockPos { x: 3, y: 1, z: 1 }];
    let mut run = Run::with(t, (1.5, 1.0, 1.5), Process::Break(targets)).builder(0);
    run.run(200);
    finished_with(&run, Event::ProcessFailed("unbreakable".into()));
    assert!(run.host.digs.is_empty());
}

#[test]
fn the_place_process_places_the_item_and_checks_the_world() {
    let place = |item: &str| {
        Process::Place(PlaceSpec {
            pos: BlockPos { x: 12, y: 1, z: 1 },
            item: item.into(),
        })
    };
    let mut run = Run::with(Terrain::new(16, 3), (1.5, 1.0, 1.5), place("cobblestone")).builder(2);
    run.run(1000);
    finished_with(&run, Event::ProcessDone);
    assert_eq!(run.host.placed, [(12, 1, 1)]);
    assert_eq!(run.host.carried("minecraft:cobblestone"), 1);

    let mut run = Run::with(Terrain::new(16, 3), (1.5, 1.0, 1.5), place("dirt")).builder(2);
    run.run(100);
    finished_with(&run, Event::ProcessFailed("no item".into()));
}

#[test]
fn bridges_to_an_island_and_breaks_the_wool_wall_on_it() {
    let mut t = islands();
    for z in 0..3 {
        for y in 1..=4 {
            t.block((12, y, z), "minecraft:white_wool", Class::STANDABLE);
        }
    }
    let mut run = Run::from(t, (2.5, 1.0, 1.5), block(14, 1, 1)).builder(8);
    run.run(3000);
    finished_with(&run, Event::ProcessDone);
    assert_eq!(run.host.carried("minecraft:cobblestone"), 2);
    assert_eq!(run.host.digs.len(), 2);
    assert!(run.host.lowest >= 1.0 - 1e-6);
}

fn parkour_run(t: Terrain, goal: Goal) -> Run {
    let mut host = Double::new(t, (0.5, 1.0, 0.5));
    let mut bots = Bots::default();
    let mut spec = spec_at(0.5, 1.0, 0.5);
    spec.policy.allow_parkour = true;
    let id = bots.spawn(&mut host, "a", spec).unwrap();
    bots.set_process("a", id, Process::Goto(goal)).unwrap();
    Run { bots, host, id }
}

#[test]
fn sprint_jumps_a_three_block_gap() {
    let mut t = Terrain::new(32, 1);
    for x in 6..9 {
        for y in -39..=0 {
            t.set((x, y, 0), Class::PASSABLE);
        }
    }
    let mut run = parkour_run(t, block(14, 1, 0));
    run.run(600);
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessDone),
        "{}",
        run.trace()
    );
    assert!(run.host.pos.y >= 1.0 - 1e-6);
}

#[test]
fn sprint_jumps_a_three_block_gap_when_the_plugin_ticks_less_often_than_the_body() {
    for steps in 1..=3 {
        let mut t = Terrain::new(32, 1);
        for x in 6..9 {
            for y in -39..=0 {
                t.set((x, y, 0), Class::PASSABLE);
            }
        }
        let mut run = parkour_run(t, block(14, 1, 0));
        for _ in 0..900 {
            run.bots.tick(&mut run.host);
            for _ in 0..steps {
                run.host.physics();
            }
            if run.finished() {
                break;
            }
        }
        assert_eq!(
            run.last_event(),
            Some(&Event::ProcessDone),
            "{steps} body ticks per plugin tick: {}",
            run.trace()
        );
    }
}

fn gap_with(steps: usize) -> bool {
    let mut t = Terrain::new(32, 1);
    for x in 6..9 {
        for y in -39..=0 {
            t.set((x, y, 0), Class::PASSABLE);
        }
    }
    let mut run = parkour_run(t, block(14, 1, 0));
    for _ in 0..900 {
        run.bots.tick(&mut run.host);
        for _ in 0..steps {
            run.host.physics();
        }
        if run.finished() {
            break;
        }
    }
    run.last_event() == Some(&Event::ProcessDone)
}

#[test]
fn sprint_jumps_a_three_block_gap_at_any_body_tick_rate() {
    for steps in 1..=3 {
        assert!(gap_with(steps), "{steps} body ticks per plugin tick");
    }
}

#[test]
fn walking_on_a_slightly_lowered_top_never_jumps() {
    let spec = spec_at(0.5, 1.0, 0.5);
    let mut terrain = Terrain::new(16, 1);
    let mut exec = super::Executor::new(
        block(8, 1, 0),
        &spec.policy,
        None,
        super::work::Kit {
            throwaway: 0,
            throwaway_slot: None,
        },
    );
    for tick in 0..40 {
        let view = super::View {
            pos: Vec3 {
                x: 0.5 + f64::from(tick) * 0.2,
                y: 0.9375,
                z: 0.5,
            },
            yaw: -90.0,
            on_ground: true,
            in_fluid: false,
            food: 20,
        };
        if let super::Step::Drive(drive) = exec.tick(&view, &spec.skill, &mut terrain) {
            assert!(!drive.jump, "tick {tick} jumped: {drive:?}");
        }
    }
}

#[test]
fn climbs_a_ladder_tower() {
    let mut t = Terrain::new(8, 2);
    for y in 1..=6 {
        t.wall((3, y, 0));
        t.wall((3, y, 1));
        t.set((2, y, 0), Class::PASSABLE.with(Class::CLIMBABLE));
    }
    let mut run = Run::new(t, block(3, 7, 0));
    run.run(600);
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessDone),
        "{}",
        run.trace()
    );
}

#[test]
fn the_last_ore_is_collected_before_finishing() {
    let mut t = Terrain::new(32, 8);
    t.ore((9, 1, 2));
    let mut run = Run::with(t, (0.5, 1.0, 0.5), mine_spec(1));
    run.run(1500);
    assert_eq!(
        run.last_event(),
        Some(&Event::ProcessDone),
        "{}",
        run.trace()
    );
    let (x, z) = (run.host.pos.x, run.host.pos.z);
    assert!((x - 9.5).abs() + (z - 2.5).abs() < 0.9, "{x} {z}");
}

fn build_spec(blocks: &[(i32, i32, i32, &str)]) -> Process {
    Process::Build(BuildSpec {
        origin: BlockPos { x: 0, y: 0, z: 0 },
        blocks: blocks
            .iter()
            .map(|(x, y, z, state)| BuildBlock {
                offset: BlockPos {
                    x: *x,
                    y: *y,
                    z: *z,
                },
                state: (*state).into(),
            })
            .collect(),
    })
}

#[test]
fn the_build_process_makes_a_hollow_cube_from_inventory_blocks() {
    let mut cells = Vec::new();
    for x in 5..8 {
        for y in 1..=3 {
            for z in 1..4 {
                if (x, y, z) != (6, 2, 2) {
                    cells.push((x, y, z, "minecraft:cobblestone"));
                }
            }
        }
    }
    assert_eq!(cells.len(), 26);
    let mut run = Run::with(Terrain::new(16, 8), (1.5, 1.0, 1.5), build_spec(&cells)).builder(30);
    run.run(6000);
    finished_with(&run, Event::ProcessDone);
    let spent = u32::try_from(run.host.placed.len()).unwrap();
    assert!(spent > 26, "the top layer needs a raised stance: {spent}");
    assert_eq!(run.host.carried("minecraft:cobblestone"), 30 - spent);
    let terrain = run.host.terrain.borrow();
    for cell in &run.host.placed {
        let shell = cells.iter().any(|c| (c.0, c.1, c.2) == *cell);
        assert_eq!(terrain.class(*cell).standable(), shell, "{cell:?}");
    }
    for (against, face, _, eye) in &run.host.clicks {
        if *face == Face::Up {
            assert!(*eye > f64::from(against.1) + 1.0, "{against:?} from {eye}");
        }
    }
    for (x, y, z, _) in &cells {
        assert!(
            terrain.class((*x, *y, *z)).standable(),
            "{x} {y} {z} {:?} {:?}",
            run.host.placed,
            run.host.digs
        );
    }
    assert!(terrain.class((6, 2, 2)).passable());
}

#[test]
fn an_upper_half_stair_is_placed_by_clicking_a_side_face_high() {
    let mut t = Terrain::new(16, 8);
    t.wall((6, 1, 2));
    let spec = build_spec(&[(5, 1, 2, "minecraft:oak_stairs[facing=east,half=top]")]);
    let mut run = Run::with(t, (1.5, 1.0, 1.5), spec);
    run.host.hotbar = vec![(1, "minecraft:oak_stairs".into(), 2)];
    run.bots.set_policy("a", run.id, builder_policy()).unwrap();
    run.run(1000);
    finished_with(&run, Event::ProcessDone);
    assert_eq!(run.host.clicks.len(), 1, "{:?}", run.host.clicks);
    let (against, face, cursor_y, _) = run.host.clicks[0];
    assert_eq!((against, face), ((6, 1, 2), Face::West));
    assert!(cursor_y > 0.5, "{cursor_y}");
}

#[test]
fn a_floating_block_gets_scaffolding_that_is_removed_after() {
    let spec = build_spec(&[(6, 4, 2, "minecraft:cobblestone")]);
    let mut run = Run::with(Terrain::new(16, 8), (1.5, 1.0, 1.5), spec).builder(20);
    run.run(6000);
    finished_with(&run, Event::ProcessDone);
    let terrain = run.host.terrain.borrow();
    assert!(terrain.class((6, 4, 2)).standable());
    for cell in run.host.placed.iter().filter(|c| **c != (6, 4, 2)) {
        assert!(terrain.class(*cell).passable(), "scaffold left at {cell:?}");
    }
    drop(terrain);
    assert!(run.host.placed.len() >= 4);
    let spent = u32::try_from(run.host.placed.len()).unwrap();
    assert_eq!(run.host.carried("minecraft:cobblestone"), 20 - spent);
}

#[test]
fn the_build_process_reports_missing_materials() {
    let spec = build_spec(&[
        (3, 1, 1, "minecraft:cobblestone"),
        (4, 1, 1, "minecraft:cobblestone"),
        (5, 1, 1, "minecraft:oak_planks"),
    ]);
    let mut run = Run::with(Terrain::new(16, 4), (1.5, 1.0, 1.5), spec).builder(1);
    run.run(50);
    finished_with(
        &run,
        Event::ProcessFailed("missing materials: 1 cobblestone, 1 oak_planks".into()),
    );
    assert!(run.host.placed.is_empty());
}

#[test]
fn wrong_blocks_are_cleared_when_the_policy_allows_breaking() {
    let mut t = Terrain::new(16, 4);
    t.block((4, 1, 1), "minecraft:white_wool", Class::STANDABLE);
    let spec = build_spec(&[(4, 1, 1, "minecraft:cobblestone")]);
    let mut run = Run::with(t, (1.5, 1.0, 1.5), spec).builder(2);
    run.run(1000);
    finished_with(&run, Event::ProcessDone);
    assert_eq!(run.host.digs, [(4, 1, 1)]);
    assert_eq!(run.host.placed, [(4, 1, 1)]);
}

#[test]
fn a_wrong_facing_stair_is_replaced_and_a_right_one_is_left() {
    let mut t = Terrain::new(16, 4);
    t.block(
        (4, 1, 1),
        "minecraft:oak_stairs[facing=north,half=bottom,shape=straight]",
        Class::STANDABLE,
    );
    t.block(
        (5, 1, 1),
        "minecraft:oak_stairs[facing=east,half=bottom,shape=straight]",
        Class::STANDABLE,
    );
    let spec = build_spec(&[
        (4, 1, 1, "minecraft:oak_stairs[facing=east,half=bottom]"),
        (5, 1, 1, "minecraft:oak_stairs[facing=east,half=bottom]"),
    ]);
    let mut run = Run::with(t, (1.5, 1.0, 1.5), spec).builder(2);
    run.host.hotbar = vec![(1, "minecraft:oak_stairs".into(), 2)];
    run.host.tools = Some(Rc::new(RefCell::new(TableTools(vec![("oak_stairs", 5)]))));
    run.run(1000);
    finished_with(&run, Event::ProcessDone);
    assert_eq!(run.host.digs, [(4, 1, 1)]);
    assert_eq!(run.host.placed, [(4, 1, 1)]);
}
