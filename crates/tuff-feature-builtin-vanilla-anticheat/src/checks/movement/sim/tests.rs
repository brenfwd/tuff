use super::*;

fn sprint_jumping(yaw: f32) -> impl Fn(usize) -> Input {
    move |_| Input {
        forward: true,
        sprint: true,
        jump: true,
        yaw,
        ..Input::default()
    }
}

fn state(block: Block, props: &[(&str, &str)]) -> BlockState {
    props.iter().fold(block.default_state(), |s, (k, v)| {
        s.with(k, v).expect("property")
    })
}

pub(crate) fn scene(column: impl Fn(i32, i32) -> Vec<(i32, BlockState)>) -> ChunkMirror {
    let mut world = World::new();
    world.tile(column);
    world.mirror()
}

/// Sprint-jumps from 24 starting offsets and headings, each first
/// dropped onto the ground, and returns every flag and how many jumps
/// were made.
fn sprint_jump_all(mirror: &ChunkMirror, ticks: usize) -> (Vec<String>, usize) {
    let mut flags = Vec::new();
    let mut jumps = 0;
    for phase in 0..24 {
        let x = 0.5 + f64::from(phase) * 0.041;
        let yaw = (phase * 37 % 360) as f32;
        let mut client = Client::new([x, 70.0, 0.3]);
        for _ in 0..40 {
            client.tick(mirror, Input::default());
        }
        let judge = drive(mirror, client.position, ticks, sprint_jumping(yaw));
        jumps += judge.jumps;
        assert_mostly_exact(&judge, &format!("phase {phase}"));
        flags.extend(
            judge
                .flags
                .iter()
                .map(|(n, flag)| format!("phase {phase} yaw {yaw} move {n}: {flag}")),
        );
    }
    (flags, jumps)
}

/// Moves a seeded engine may take to lock on.
pub(crate) const SETTLING: usize = 6;

/// The engine replays every move exactly once it has locked on, rather
/// than letting it pass under the bound.
pub(crate) fn assert_mostly_exact(judge: &Judge, label: &str) {
    assert!(
        judge.misses.iter().all(|&n| n < SETTLING),
        "{label}: moves {:?} of {} were not replayed exactly",
        judge.misses,
        judge.judged
    );
}

fn assert_never_flagged(mirror: &ChunkMirror) {
    let (flags, jumps) = sprint_jump_all(mirror, 300);
    assert!(
        flags.is_empty(),
        "{} flags: {:#?}",
        flags.len(),
        &flags[..flags.len().min(5)]
    );
    assert!(jumps > 100, "only {jumps} jumps");
}

#[tuff_core::watchdog]
#[test]
fn sprint_jump_chains_on_flat_ground_are_never_flagged() {
    let mirror = scene(|_, _| vec![(63, stone())]);
    let (flags, jumps) = sprint_jump_all(&mirror, 120);
    assert!(flags.is_empty(), "{flags:#?}");
    assert!(jumps >= 200, "only {jumps} jumps");
}

#[tuff_core::watchdog]
#[test]
fn stepping_off_an_edge_and_landing_on_one_are_never_flagged() {
    let mirror = scene(|x, z| {
        if x % 5 == 0 && z % 5 == 0 {
            vec![(63, stone()), (64, stone())]
        } else {
            vec![(63, stone())]
        }
    });
    assert_never_flagged(&mirror);
    let off_edge = Terrain::probe(&mirror, [0.5, 65.0, 11.0], [0.5, 65.0, 11.42], 0.6);
    assert!(off_edge.supported && !off_edge.underfoot);
    let landing = Terrain::probe(&mirror, [0.5, 65.1213, 11.0], [0.5, 65.0, 11.42], 0.6);
    assert!(landing.supported && !landing.underfoot);
    let hanging = Terrain::probe(&mirror, [0.5, 65.0, 11.5], [0.5, 65.0, 11.9], 0.6);
    assert!(!hanging.supported);
}

#[tuff_core::watchdog]
#[test]
fn sprint_jumping_under_a_two_block_ceiling_is_never_flagged() {
    assert_never_flagged(&scene(|_, _| vec![(63, stone()), (66, stone())]));
    let slab = state(Block::StoneSlab, &[("type", "bottom")]);
    assert_never_flagged(&scene(move |_, _| vec![(63, stone()), (66, slab)]));
}

#[tuff_core::watchdog]
#[test]
fn jumping_up_blocks_and_stairs_is_never_flagged() {
    assert_never_flagged(&scene(|x, z| {
        let h = ((x / 4) + (z / 4)) % 3;
        (0..=h).map(|d| (63 + d, stone())).collect()
    }));
    let stairs = |facing| state(Block::StoneStairs, &[("facing", facing)]);
    assert_never_flagged(&scene(move |x, _| {
        let h = (x % 8).min(8 - x % 8);
        let mut column: Vec<_> = (0..h).map(|d| (63 + d, stone())).collect();
        column.push((63 + h, stairs(if x % 8 < 4 { "east" } else { "west" })));
        column
    }));
}

#[tuff_core::watchdog]
#[test]
fn crossing_slab_and_path_edges_is_never_flagged() {
    let slab = state(Block::StoneSlab, &[("type", "bottom")]);
    assert_never_flagged(&scene(move |x, z| {
        if (x * 7 + z * 3) % 5 < 2 {
            vec![(63, stone()), (64, slab)]
        } else {
            vec![(63, stone())]
        }
    }));
    let path = Block::DirtPath.default_state();
    assert_never_flagged(&scene(move |x, z| {
        if (x * 5 + z * 11) % 7 < 3 {
            vec![(63, path)]
        } else {
            vec![(63, stone())]
        }
    }));
}

pub(crate) fn hash(seed: u64, x: i32, z: i32, salt: u64) -> u64 {
    let mut h = seed
        ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (z as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ salt.wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^ (h >> 33)
}

fn tops() -> Vec<BlockState> {
    let mut tops = vec![
        stone(),
        state(Block::StoneSlab, &[("type", "bottom")]),
        state(Block::StoneSlab, &[("type", "top")]),
        Block::DirtPath.default_state(),
        Block::Farmland.default_state(),
        Block::WhiteCarpet.default_state(),
        Block::SoulSand.default_state(),
        Block::Mud.default_state(),
        Block::Chest.default_state(),
        Block::EnchantingTable.default_state(),
        Block::OakFence.default_state(),
    ];
    for facing in ["north", "east", "south", "west"] {
        for half in ["bottom", "top"] {
            tops.push(state(
                Block::StoneStairs,
                &[("facing", facing), ("half", half)],
            ));
        }
    }
    for layers in ["1", "2", "4", "7"] {
        tops.push(state(Block::Snow, &[("layers", layers)]));
    }
    tops
}

/// Terraces of up to two blocks, a third of the columns topped with a
/// partial block, and a quarter of them under a ceiling two or three
/// blocks above the ground.
fn rough(seed: u64) -> ChunkMirror {
    let tops = tops();
    scene(|x, z| {
        let h = (hash(seed, x / 2, z / 2, 1) % 3) as i32;
        let mut column: Vec<_> = (0..=h).map(|d| (63 + d, stone())).collect();
        if hash(seed, x, z, 2).is_multiple_of(3) {
            let top = tops[(hash(seed, x, z, 3) % tops.len() as u64) as usize];
            column.push((64 + h, top));
        }
        if hash(seed, x / 3, z / 3, 4).is_multiple_of(4) {
            let gap = 2 + (hash(seed, x, z, 5) % 2) as i32;
            column.push((64 + h + gap, stone()));
        }
        column
    })
}

/// Keys that change every 15 ticks: mostly walking forward, often
/// sprinting and jumping, in any direction.
fn wandering(seed: u64) -> impl Fn(usize) -> Input {
    move |tick| {
        let r = hash(seed, (tick / 15) as i32, 0, 9);
        Input {
            forward: !r.is_multiple_of(8),
            sprint: !(r >> 3).is_multiple_of(4),
            jump: !(r >> 5).is_multiple_of(3),
            yaw: ((r >> 8) % 360) as f32,
            ..Input::default()
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn wandering_over_rough_ground_under_low_ceilings_is_never_flagged() {
    let mut flagged = Vec::new();
    let mut jumps = 0;
    for seed in 0..100 {
        let mirror = rough(seed);
        let mut client = Client::new([0.5, 80.0, 0.5]);
        for _ in 0..60 {
            client.tick(&mirror, Input::default());
        }
        let judge = drive(&mirror, client.position, 600, wandering(seed));
        jumps += judge.jumps;
        assert_mostly_exact(&judge, &format!("seed {seed}"));
        if let Some((n, flag)) = judge.flags.first() {
            flagged.push(format!(
                "seed {seed}: {} flags, first at move {n}: {flag}",
                judge.flags.len()
            ));
        }
    }
    assert!(flagged.is_empty(), "{flagged:#?}");
    assert!(jumps > 1000, "only {jumps} jumps");
}

/// Replays scripted vertical steps from standing on flat ground, moving
/// 0.2 blocks along x each move, and returns the moves `fly` flagged.
fn cheat(steps: &[f64]) -> Vec<usize> {
    let mirror = scene(|_, _| vec![(63, stone())]);
    let mut judge = Judge::new([0.5, 64.0, 0.5]);
    judge.report(&mirror, [0.7, 64.0, 0.5], true);
    let mut at = [0.7, 64.0, 0.5];
    for dy in steps {
        at = [at[0] + 0.2, at[1] + dy, at[2]];
        judge.report(&mirror, at, false);
    }
    judge
        .flags
        .iter()
        .filter(|(_, flag)| flag.starts_with("fly"))
        .map(|(n, _)| *n)
        .collect()
}

fn jump(ticks: usize) -> Vec<f64> {
    let mut vy = f64::from(0.42f32);
    (0..ticks)
        .map(|_| {
            let dy = vy;
            vy = (vy - 0.08) * f64::from(0.98f32);
            dy
        })
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn hover_glide_air_jump_and_climb_cheats_are_still_caught() {
    assert!(cheat(&jump(11)).is_empty());
    let hover = [jump(6), vec![0.0; 12]].concat();
    assert!(!cheat(&hover).is_empty(), "hover");
    let glide = [jump(6), vec![-0.03; 12]].concat();
    assert!(!cheat(&glide).is_empty(), "glide");
    let air_jump = [jump(5), jump(5)].concat();
    assert_eq!(cheat(&air_jump).first(), Some(&6), "air jump");
    let high_step = [vec![0.6], jump(4)[1..].to_vec()].concat();
    assert_eq!(cheat(&high_step).first(), Some(&2), "step then jump");
    let spider = vec![0.2; 6];
    assert_eq!(cheat(&spider).first(), Some(&2), "climb");
    let ladder = vec![0.1176; 8];
    assert!(!cheat(&ladder).is_empty(), "slow climb");
}

/// Stalks three blocks tall on every other column of a grid, with
/// pointed dripstone between them, on stone.
fn grove() -> ChunkMirror {
    let bamboo = state(Block::Bamboo, &[("age", "1"), ("leaves", "large")]);
    let dripstone = state(
        Block::PointedDripstone,
        &[("thickness", "tip"), ("vertical_direction", "up")],
    );
    scene(move |x, z| {
        let mut column = vec![(63, stone())];
        if x % 2 == 0 && z % 2 == 0 {
            column.extend((64..67).map(|y| (y, bamboo)));
        } else if x % 4 == 1 && z % 4 == 1 {
            column.push((64, dripstone));
        }
        column
    })
}

#[tuff_core::watchdog]
#[test]
fn sprinting_through_bamboo_and_dripstone_is_never_flagged() {
    let mirror = grove();
    let mut flagged = Vec::new();
    let mut blocked = 0;
    for seed in 0..100 {
        let mut client = Client::new([1.5, 66.0, 1.5]);
        for _ in 0..30 {
            client.tick(&mirror, Input::default());
        }
        let start = client.position;
        let judge = drive_in(&mirror, start, 200, wandering(seed), false);
        blocked += judge.moves;
        if let Some((n, flag)) = judge.flags.first() {
            flagged.push(format!("seed {seed} move {n}: {flag}"));
        }
    }
    assert!(flagged.is_empty(), "{flagged:#?}");
    assert!(blocked > 10_000, "{blocked} moves");
}

#[tuff_core::watchdog]
#[test]
fn stalks_collide_where_the_game_shifts_them() {
    let mirror = grove();
    let stalk = |x: i32, z: i32| {
        let [dx, _, dz] = Block::Bamboo.shape_offset().unwrap().at(x, z);
        [f64::from(x) + 0.5 + dx, 64.0, f64::from(z) + 0.5 + dz]
    };
    for (x, z) in [(0, 0), (2, 4), (6, 2), (-4, 8)] {
        let centre = stalk(x, z);
        let beside = [centre[0] + 0.45, 64.0, centre[2]];
        let into = Terrain::probe(&mirror, beside, centre, 0.6);
        assert!(into.phased && !into.was_phased, "stalk at {x},{z}");
        let mut judge = Judge::new(beside);
        judge.report(&mirror, centre, true);
        assert!(judge.flags.iter().any(|(_, f)| f.starts_with("phase")));
        let away = [centre[0] + 0.45, 64.0, centre[2] + 0.45];
        assert!(
            !Terrain::probe(&mirror, beside, away, 0.6).phased,
            "stalk at {x},{z}"
        );
    }
    let [dx, _, dz] = Block::PointedDripstone.shape_offset().unwrap().at(1, 1);
    let tip = [1.5 + dx, 64.0, 1.5 + dz];
    assert!(Terrain::probe(&mirror, [1.5 + dx, 64.0, 2.4], tip, 0.6).phased);
}

fn walls() -> ChunkMirror {
    let fence = state(Block::OakFence, &[]);
    let bars = state(Block::IronBars, &[("north", "true"), ("south", "true")]);
    scene(move |x, z| {
        let mut column = vec![(63, stone())];
        if x.rem_euclid(32) == 5 {
            column.extend((64..67).map(|y| (y, bars)));
        } else if x.rem_euclid(32) == 9 {
            column.push((64, fence));
        } else if x.rem_euclid(32) == 12 {
            column.push((64, stone()));
        }
        let _ = z;
        column
    })
}

#[tuff_core::watchdog]
#[test]
fn a_block_changed_a_moment_ago_excuses_a_clip_only_inside_the_window() {
    let mut mirror = walls();
    mirror.set_clock(100);
    let pos = BlockPos::new(5, 64, 3);
    mirror.apply(
        ChunkPos::of_block(pos),
        &[(pos, state(Block::IronBars, &[]))],
    );
    let probe =
        |mirror: &ChunkMirror| Terrain::probe(mirror, [5.1, 64.0, 3.5], [5.85, 64.0, 3.5], 0.6);
    let fresh = probe(&mirror);
    assert!(!fresh.clipped && !fresh.phased);
    mirror.set_clock(105);
    assert!(!probe(&mirror).clipped);
    mirror.set_clock(121);
    assert!(probe(&mirror).clipped);
    let far = BlockPos::new(40, 64, 3);
    mirror.apply(ChunkPos::of_block(far), &[(far, stone())]);
    assert!(probe(&mirror).clipped);
}

#[tuff_core::watchdog]
#[test]
fn a_move_through_a_thin_wall_is_a_clip_but_free_moves_are_not() {
    let mirror = walls();
    let through = Terrain::probe(&mirror, [5.1, 64.0, 3.5], [5.85, 64.0, 3.5], 0.6);
    assert!(through.clipped && !through.phased);
    let mut judge = Judge::new([5.1, 64.0, 3.5]);
    judge.report(&mirror, [5.85, 64.0, 3.5], true);
    assert!(judge.flags.iter().any(|(_, f)| f.starts_with("phase")));
    let free = Terrain::probe(&mirror, [1.5, 64.0, 3.5], [4.2, 64.0, 3.5], 0.6);
    assert!(!free.clipped);
    let slide = Terrain::probe(&mirror, [4.6, 64.0, 3.5], [4.6, 64.0, 8.5], 0.6);
    assert!(!slide.clipped);
    let beyond = Terrain::probe(&mirror, [6.8, 64.0, 3.5], [7.4, 64.0, 3.5], 0.6);
    assert!(!beyond.clipped);
    let up = Terrain::probe(&mirror, [1.5, 64.0, 3.5], [1.5, 64.6, 3.5], 0.6);
    assert!(!up.clipped);
}

#[tuff_core::watchdog]
#[test]
fn a_taller_step_height_makes_a_one_block_wall_steppable_not_a_clip() {
    let mirror = walls();
    let (from, to) = ([11.5, 64.0, 3.5], [13.5, 64.0, 3.5]);
    assert!(Terrain::probe(&mirror, from, to, 0.6).clipped);
    assert!(!Terrain::probe(&mirror, from, to, 1.0).clipped);
    let bars = Terrain::probe(&mirror, [5.1, 64.0, 3.5], [5.85, 64.0, 3.5], 1.0);
    assert!(bars.clipped);
}

#[tuff_core::watchdog]
#[test]
fn walking_into_a_fence_or_stepping_up_is_not_a_clip() {
    let mirror = walls();
    let fence = Terrain::probe(&mirror, [8.0, 64.0, 3.5], [8.3, 64.0, 3.5], 0.6);
    assert!(!fence.clipped);
    let over = Terrain::probe(&mirror, [8.0, 64.0, 3.5], [10.0, 64.0, 3.5], 0.6);
    assert!(over.clipped);
}

/// Keys that change every 12 ticks: any direction, sprinting only
/// forward and when neither sneaking nor eating.
pub(crate) fn roaming(seed: u64, sneaks: bool, eats: bool) -> impl Fn(usize) -> Input {
    move |tick| {
        let r = hash(seed, (tick / 12) as i32, 7, 11);
        let sneak = sneaks && (r >> 12).is_multiple_of(3);
        let eating = eats && (r >> 14).is_multiple_of(3);
        let forward = r & 3 != 0;
        Input {
            forward,
            back: !forward && (r >> 2).is_multiple_of(2),
            left: (r >> 3).is_multiple_of(3),
            right: (r >> 3) % 3 == 1,
            sprint: forward && !sneak && !eating && !(r >> 5).is_multiple_of(3),
            jump: (r >> 7).is_multiple_of(3),
            sneak,
            eating,
            yaw: ((r >> 16) % 360) as f32 + 0.25,
            pitch: 0.0,
        }
    }
}

/// Roams `scene` from 20 seeds and asserts no flag at all and exact
/// replays.
fn assert_roams_cleanly(mirror: &ChunkMirror, start: [f64; 3], sneaks: bool, eats: bool) -> usize {
    let mut flagged = Vec::new();
    let mut moves = 0;
    for seed in 0..20 {
        let mut client = Client::new(start);
        for _ in 0..40 {
            client.tick(mirror, Input::default());
        }
        let judge = drive(mirror, client.position, 400, roaming(seed, sneaks, eats));
        moves += judge.moves;
        assert_mostly_exact(&judge, &format!("seed {seed}"));
        if let Some((n, flag)) = judge.flags.first() {
            flagged.push(format!(
                "seed {seed}: {} flags, first at move {n}: {flag}",
                judge.flags.len()
            ));
        }
    }
    assert!(flagged.is_empty(), "{flagged:#?}");
    moves
}

fn patchwork(blocks: Vec<BlockState>) -> ChunkMirror {
    scene(move |x, z| {
        let top = blocks[((x / 3 + z / 3) as usize) % blocks.len()];
        vec![(62, stone()), (63, top)]
    })
}

#[tuff_core::watchdog]
#[test]
fn ice_slime_honey_and_soul_sand_are_replayed_exactly() {
    let ice = patchwork(vec![
        Block::Ice.default_state(),
        Block::PackedIce.default_state(),
        Block::BlueIce.default_state(),
        stone(),
    ]);
    assert!(assert_roams_cleanly(&ice, [0.5, 66.0, 0.5], true, true) > 4000);
    let slow = patchwork(vec![
        Block::HoneyBlock.default_state(),
        Block::SoulSand.default_state(),
        Block::SlimeBlock.default_state(),
        stone(),
    ]);
    assert!(assert_roams_cleanly(&slow, [0.5, 66.0, 0.5], true, true) > 3000);
    let slime = scene(|_, _| vec![(63, Block::SlimeBlock.default_state())]);
    let mut client = Client::new([0.5, 72.0, 0.5]);
    let mut bounces = 0;
    let judge = {
        let start = client.position;
        let mut judge = Judge::new(start);
        for tick in 0..200 {
            let held = Input {
                forward: tick > 40,
                yaw: 30.0,
                ..Input::default()
            };
            judge.hold(&held);
            let before = client.velocity[1];
            if let Some(to) = client.tick(&slime, held) {
                judge.report(&slime, to, client.on_ground);
            }
            bounces += usize::from(before < 0.0 && client.velocity[1] > 0.0);
            judge.tick_end();
        }
        judge
    };
    assert!(bounces >= 3, "{bounces} bounces");
    assert!(judge.flags.is_empty(), "{:#?}", judge.flags);
    assert_mostly_exact(&judge, "slime drop");
}

#[tuff_core::watchdog]
#[test]
fn strafing_sneaking_at_edges_and_eating_are_replayed_exactly() {
    let terraces = scene(|x, z| {
        let h = ((x / 4) + (z / 5)) % 3;
        (0..=h).map(|d| (63 + d, stone())).collect()
    });
    assert!(assert_roams_cleanly(&terraces, [0.5, 70.0, 0.5], true, true) > 4000);
    let ledge = scene(|x, z| {
        if (x / 6 + z / 6) % 2 == 0 {
            vec![(63, stone()), (64, stone())]
        } else {
            vec![(63, stone())]
        }
    });
    let mut client = Client::new([2.5, 66.0, 2.5]);
    for _ in 0..20 {
        client.tick(&ledge, Input::default());
    }
    let judge = drive_in(
        &ledge,
        client.position,
        300,
        |tick| Input {
            forward: true,
            left: tick % 50 < 10,
            sneak: true,
            yaw: if tick < 150 { -90.0 } else { 0.0 },
            ..Input::default()
        },
        false,
    );
    assert!(judge.flags.is_empty(), "{:#?}", judge.flags);
    assert_mostly_exact(&judge, "ledge");
}

#[tuff_core::watchdog]
#[test]
fn backing_off_a_ledge_with_pulsed_sneak_is_replayed_exactly() {
    let ledge = scene(|x, _| {
        if x < 4 {
            vec![(63, stone())]
        } else {
            vec![(50, stone())]
        }
    });
    for (period, off) in [
        (3usize, 1usize),
        (3, 2),
        (4, 2),
        (4, 3),
        (5, 2),
        (6, 3),
        (2, 1),
    ] {
        for lead in 0..period {
            let start = [2.5, 64.0, 0.5];
            let mut client = Client::new(start);
            let mut judge = Judge::new(start);
            for tick in 0..75usize {
                let held = Input {
                    back: true,
                    sneak: (tick + lead) % period >= off,
                    yaw: 90.0,
                    pitch: 80.0,
                    ..Input::default()
                };
                judge.hold(&held);
                if let Some(to) = client.tick(&ledge, held) {
                    judge.report(&ledge, to, client.on_ground);
                }
                judge.tick_end();
            }
            assert!(
                judge.flags.is_empty(),
                "{period} {off} {lead}: {:#?}",
                judge.flags
            );
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn cobwebs_and_ladders_are_replayed_exactly() {
    let web = Block::Cobweb.default_state();
    let webs = scene(move |x, z| {
        if (x * 3 + z * 5) % 7 == 0 {
            vec![(63, stone()), (64, web)]
        } else {
            vec![(63, stone())]
        }
    });
    assert!(assert_roams_cleanly(&webs, [0.5, 66.0, 0.5], false, false) > 2000);
    let ladder = state(Block::Ladder, &[("facing", "east")]);
    let wall = scene(move |x, _| match x % 8 {
        0 => (63..75).map(|y| (y, stone())).collect(),
        1 => [(63, stone())]
            .into_iter()
            .chain((64..74).map(|y| (y, ladder)))
            .collect(),
        _ => vec![(63, stone())],
    });
    let start = [3.5, 64.0, 0.5];
    let judge = drive_in(
        &wall,
        start,
        240,
        |tick| Input {
            forward: tick < 120,
            sneak: (120..160).contains(&tick),
            jump: tick < 20,
            yaw: 90.0,
            ..Input::default()
        },
        false,
    );
    assert!(judge.flags.is_empty(), "{:#?}", judge.flags);
    assert_mostly_exact(&judge, "ladder");
    assert!(
        judge.facts.position[1] > 70.0 || judge.moves > 0,
        "{:?}",
        judge.facts.position
    );
}

/// Drives a client through `ticks` ticks of `input(tick)` after it
/// settled for 40 ticks; `tune` sets up both sides first and `cheat`
/// changes the client before every tick from the 40th.
pub(crate) fn wade(
    mirror: &ChunkMirror,
    start: [f64; 3],
    ticks: usize,
    input: impl Fn(usize) -> Input,
    tune: impl Fn(&mut Client, &mut Facts),
    cheat: impl Fn(&mut Client),
) -> Judge {
    let mut client = Client::new(start);
    let mut judge = Judge::new(start);
    tune(&mut client, &mut judge.facts);
    for _ in 0..40 {
        client.tick(mirror, Input::default());
    }
    judge.facts.position = client.sent;
    for tick in 0..ticks {
        if tick >= 40 {
            cheat(&mut client);
        }
        let held = input(tick);
        judge.hold(&held);
        if let Some(to) = client.tick(mirror, held) {
            judge.report(mirror, to, client.on_ground);
        }
        judge.tick_end();
        let before = client.position;
        client.wrap();
        if before != client.position {
            judge.wrapped(client.sent);
        }
    }
    judge
}

/// [`roaming`] without sprinting, which in water may become a swim.
fn wading(seed: u64) -> impl Fn(usize) -> Input {
    let roam = roaming(seed, true, true);
    move |tick| Input {
        sprint: false,
        ..roam(tick)
    }
}

pub(crate) fn liquid(block: Block, level: u8) -> BlockState {
    block.default_state().with_level(level).expect("level")
}

/// Deep pools, shallows one block over the floor and islands one block
/// above the surface, of `fluid`.
pub(crate) fn pools(fluid: Block) -> ChunkMirror {
    let source = liquid(fluid, 0);
    scene(move |x, z| match (x / 4 + z / 4) % 3 {
        0 => [(60, stone())]
            .into_iter()
            .chain((61..65).map(|y| (y, source)))
            .collect(),
        1 => (60..64)
            .map(|y| (y, stone()))
            .chain([(64, source)])
            .collect(),
        _ => (60..66).map(|y| (y, stone())).collect(),
    })
}

/// Shallow `fluid` falling off along x in every level, walls across it
/// and falls of falling fluid beside them.
fn river(fluid: Block) -> ChunkMirror {
    scene(move |x, z| {
        let mut column = vec![(61, stone()), (62, liquid(fluid, (x % 8) as u8))];
        match z % 16 {
            0 => column.extend((62..65).map(|y| (y, stone()))),
            1 => column.extend((63..67).map(|y| (y, liquid(fluid, 8)))),
            _ => {}
        }
        column
    })
}

/// Wades `mirror` from 20 seeds and asserts no flag at all, exact
/// replays and how many of them went through the fluid.
fn assert_wades_cleanly(
    mirror: &ChunkMirror,
    start: [f64; 3],
    tune: impl Fn(&mut Client, &mut Facts) + Copy,
) -> usize {
    let mut flagged = Vec::new();
    let mut wet = 0;
    for seed in 0..20 {
        let judge = wade(mirror, start, 400, wading(seed), tune, |_| {});
        wet += judge.wet;
        assert_mostly_exact(&judge, &format!("seed {seed}"));
        if let Some((n, flag)) = judge.flags.first() {
            flagged.push(format!(
                "seed {seed}: {} flags, first at move {n}: {flag}",
                judge.flags.len()
            ));
        }
    }
    assert!(flagged.is_empty(), "{flagged:#?}");
    wet
}

pub(crate) fn plain(_: &mut Client, _: &mut Facts) {}

#[tuff_core::watchdog]
#[test]
fn wading_and_diving_through_water_is_replayed_exactly() {
    let wet = assert_wades_cleanly(&pools(Block::Water), [0.5, 67.0, 0.5], plain);
    assert!(wet > 3000, "{wet} exact moves in water");
}

#[tuff_core::watchdog]
#[test]
fn water_currents_and_falls_are_replayed_exactly() {
    let wet = assert_wades_cleanly(&river(Block::Water), [0.5, 64.0, 2.5], plain);
    assert!(wet > 3000, "{wet} exact moves in water");
}

#[tuff_core::watchdog]
#[test]
fn lava_pools_and_currents_are_replayed_exactly() {
    let wet = assert_wades_cleanly(&pools(Block::Lava), [0.5, 67.0, 0.5], plain);
    assert!(wet > 2000, "{wet} exact moves in lava");
    let wet = assert_wades_cleanly(&river(Block::Lava), [0.5, 64.0, 2.5], plain);
    assert!(wet > 2000, "{wet} exact moves in lava");
}

#[tuff_core::watchdog]
#[test]
fn depth_strider_and_dolphins_grace_are_replayed_exactly() {
    let strider = |client: &mut Client, facts: &mut Facts| {
        client.water_efficiency = 1.0;
        facts.body.water_efficiency = 1.0;
    };
    let wet = assert_wades_cleanly(&pools(Block::Water), [0.5, 67.0, 0.5], strider);
    assert!(wet > 3000, "{wet} exact moves with depth strider");
    let dolphins = |client: &mut Client, facts: &mut Facts| {
        client.dolphins = true;
        facts.dolphins_grace = true;
    };
    let wet = assert_wades_cleanly(&river(Block::Water), [0.5, 64.0, 2.5], dolphins);
    assert!(wet > 3000, "{wet} exact moves with dolphin's grace");
}

/// A cheating client on flat stone: `cheat` changes it before every tick.
fn cheating(cheat: impl Fn(&mut Client), input: impl Fn(usize) -> Input) -> Judge {
    let mirror = scene(|_, _| vec![(63, stone())]);
    let mut client = Client::new([0.5, 64.0, 0.5]);
    let mut judge = Judge::new(client.position);
    for tick in 0..200 {
        if tick >= 40 {
            cheat(&mut client);
        }
        let held = input(tick);
        judge.hold(&held);
        if let Some(to) = client.tick(&mirror, held) {
            judge.report(&mirror, to, client.on_ground);
        }
        judge.tick_end();
        let before = client.position;
        client.wrap();
        if before != client.position {
            judge.wrapped(client.sent);
        }
    }
    judge
}

pub(crate) fn predict_flags(judge: &Judge) -> usize {
    judge
        .flags
        .iter()
        .filter(|(_, f)| f.starts_with("predict"))
        .count()
}

#[tuff_core::watchdog]
#[test]
fn speed_bunny_hop_no_slow_and_high_jump_cheats_of_a_few_percent_are_flagged() {
    let sprinting = |_| Input {
        forward: true,
        sprint: true,
        yaw: 20.0,
        ..Input::default()
    };
    let hopping = |_| Input {
        forward: true,
        sprint: true,
        jump: true,
        yaw: 200.0,
        ..Input::default()
    };
    let eating = |_| Input {
        forward: true,
        eating: true,
        yaw: 75.0,
        ..Input::default()
    };
    assert_eq!(predict_flags(&cheating(|_| {}, sprinting)), 0);
    assert_eq!(predict_flags(&cheating(|_| {}, hopping)), 0);
    assert_eq!(predict_flags(&cheating(|_| {}, eating)), 0);
    let speed = cheating(|c| c.boost = 1.04, sprinting);
    assert!(
        predict_flags(&speed) > 100,
        "speed: {:?}",
        &speed.flags[..speed.flags.len().min(3)]
    );
    let hop = cheating(|c| c.boost = 1.05, hopping);
    assert!(
        predict_flags(&hop) > 50,
        "bunny hop: {:?}",
        &hop.flags[..hop.flags.len().min(3)]
    );
    let no_slow = cheating(|c| c.boost = 1.0 / 0.2 * 0.21, eating);
    assert!(
        predict_flags(&no_slow) > 100,
        "no slow: {:?}",
        &no_slow.flags[..no_slow.flags.len().min(3)]
    );
    let high = cheating(|c| c.jump_power = 0.45, hopping);
    assert!(
        high.flags
            .iter()
            .any(|(_, f)| f.starts_with("predict") && f.contains("actual")),
        "high jump: {:?}",
        high.flags
    );
}

/// A deep ocean of `fluid` over a stone floor.
pub(crate) fn deep(fluid: Block) -> ChunkMirror {
    let source = liquid(fluid, 0);
    scene(move |_, _| {
        [(39, stone())]
            .into_iter()
            .chain((40..65).map(|y| (y, source)))
            .collect()
    })
}

#[tuff_core::watchdog]
#[test]
fn liquid_speed_and_rise_cheats_of_a_few_percent_are_flagged() {
    let forward = |_| Input {
        forward: true,
        yaw: 20.0,
        ..Input::default()
    };
    let rising = |_| Input {
        forward: true,
        jump: true,
        yaw: 200.0,
        ..Input::default()
    };
    for fluid in [Block::Water, Block::Lava] {
        let mirror = deep(fluid);
        let start = [0.5, 40.0, 0.5];
        let run = |input: &dyn Fn(usize) -> Input, cheat: &dyn Fn(&mut Client)| {
            wade(&mirror, start, 200, input, plain, cheat)
        };
        for input in [&forward as &dyn Fn(usize) -> Input, &rising] {
            let honest = run(input, &|_| {});
            assert_eq!(predict_flags(&honest), 0, "{fluid:?}: {:?}", honest.flags);
            assert!(honest.wet > 150, "{fluid:?}: {} exact", honest.wet);
            let speed = run(input, &|c| c.boost = 1.04);
            assert!(
                predict_flags(&speed) > 100,
                "{fluid:?} speed: {} {:?}",
                predict_flags(&speed),
                &speed.flags[..speed.flags.len().min(3)]
            );
        }
        let rise = run(&rising, &|c| c.velocity[1] += 0.015);
        assert!(
            predict_flags(&rise) > 50,
            "{fluid:?} rise: {} {:?}",
            predict_flags(&rise),
            &rise.flags[..rise.flags.len().min(3)]
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn dolphins_grace_the_client_gains_or_loses_late_is_not_flagged() {
    let mirror = deep(Block::Water);
    let mut client = Client::new([0.5, 50.0, 0.5]);
    let mut judge = Judge::new(client.position);
    for tick in 0..260 {
        judge.facts.tick = tick;
        judge.facts.dolphins_grace = !(100..180).contains(&tick);
        client.dolphins = !(110..188).contains(&tick);
        let held = Input {
            forward: true,
            jump: tick % 40 < 20,
            yaw: 75.0,
            ..Input::default()
        };
        judge.hold(&held);
        if let Some(to) = client.tick(&mirror, held) {
            judge.report(&mirror, to, client.on_ground);
        }
        judge.tick_end();
        let before = client.position;
        client.wrap();
        if before != client.position {
            judge.wrapped(client.sent);
        }
    }
    assert_eq!(predict_flags(&judge), 0, "{:?}", judge.flags);
    assert!(judge.wet > 240, "{} exact", judge.wet);
}

fn bubbles(drag: bool, open: bool) -> ChunkMirror {
    let source = liquid(Block::Water, 0);
    let column = state(
        Block::BubbleColumn,
        &[("drag", if drag { "true" } else { "false" })],
    );
    scene(move |x, z| {
        let mut cells = vec![(60, stone())];
        let top = if open { 68 } else { 66 };
        let tall = x % 6 < 2 && z % 6 < 2;
        for y in 61..=top {
            cells.push((y, if tall { column } else { source }));
        }
        if !open {
            cells.extend((67..=68).map(|y| (y, source)));
        }
        cells
    })
}

#[tuff_core::watchdog]
#[test]
fn bubble_columns_lift_and_drag_are_replayed_exactly() {
    for (drag, open) in [(false, true), (false, false), (true, true), (true, false)] {
        let wet = assert_wades_cleanly(&bubbles(drag, open), [0.5, 64.0, 0.5], plain);
        assert!(wet > 3000, "drag {drag} open {open}: {wet} exact moves");
    }
}

#[tuff_core::watchdog]
#[test]
fn honey_slides_are_replayed_exactly() {
    let honey = Block::HoneyBlock.default_state();
    let walls = scene(move |x, _| match x % 6 {
        1 => (63..=68).map(|y| (y, honey)).collect(),
        _ => vec![(63, stone())],
    });
    let mut slid = 0;
    for seed in 0..20 {
        let judge = drive_in(
            &walls,
            [2.28, 74.0, 0.5],
            300,
            roaming(seed, true, false),
            false,
        );
        assert!(judge.flags.is_empty(), "seed {seed}: {:#?}", judge.flags);
        assert_mostly_exact(&judge, &format!("seed {seed}"));
        slid += judge.exact;
    }
    assert!(slid > 500, "{slid}");
}

fn drifts() -> ChunkMirror {
    let snow = Block::PowderSnow.default_state();
    scene(move |x, z| match (x / 3 + z / 3) % 3 {
        0 => vec![(62, stone()), (63, snow), (64, snow)],
        1 => vec![(62, stone()), (63, snow)],
        _ => vec![(62, stone()), (63, stone())],
    })
}

#[tuff_core::watchdog]
#[test]
fn powder_snow_sinking_and_climbing_are_replayed_exactly() {
    for walker in [false, true] {
        let tune = move |client: &mut Client, facts: &mut Facts| {
            client.walker = walker;
            facts.snow_walker = walker;
        };
        let mirror = drifts();
        let mut stuck = 0;
        for seed in 0..20 {
            let judge = wade(
                &mirror,
                [6.5, 65.0, 0.5],
                400,
                roaming(seed, true, false),
                tune,
                |_| {},
            );
            assert!(
                judge.flags.is_empty(),
                "walker {walker} seed {seed}: {:#?}",
                judge.flags
            );
            assert_mostly_exact(&judge, &format!("walker {walker} seed {seed}"));
            stuck += judge
                .tags
                .iter()
                .filter(|t| t.contains(crate::trace::Tags::STUCK))
                .count();
        }
        assert!(walker || stuck > 100, "{stuck}");
    }
}

#[tuff_core::watchdog]
#[test]
fn scaffolding_stands_and_lets_a_sneaking_body_down_replayed_exactly() {
    let scaffold = state(
        Block::Scaffolding,
        &[
            ("bottom", "false"),
            ("distance", "0"),
            ("waterlogged", "false"),
        ],
    );
    let towers = scene(move |x, z| {
        let mut column = vec![(62, stone())];
        if x % 5 < 2 && z % 5 < 2 {
            column.extend((63..=70).map(|y| (y, scaffold)));
        } else {
            column.push((63, stone()));
        }
        column
    });
    let mut moves = 0;
    for seed in 0..20 {
        let judge = wade(
            &towers,
            [0.5, 72.0, 0.5],
            400,
            |tick| {
                let held = roaming(seed, true, false)(tick);
                Input {
                    sneak: held.sneak && tick > 0,
                    ..held
                }
            },
            plain,
            |_| {},
        );
        assert!(judge.flags.is_empty(), "seed {seed}: {:#?}", judge.flags);
        assert_mostly_exact(&judge, &format!("seed {seed}"));
        moves += judge.exact;
    }
    assert!(moves > 1000, "{moves}");
}

/// A client that just joined has not landed yet: its first ticks on the
/// floor are airborne, which the seeded engine must accept rather than
/// explain as a slowed input and then drift from.
#[tuff_core::watchdog]
#[test]
fn a_freshly_joined_client_sprinting_off_its_first_tick_is_not_flagged() {
    let mirror = scene(|_, _| vec![(63, stone())]);
    for (frozen, idle, fall) in (0..3).flat_map(|idle| {
        [0, 15, 16, 40]
            .into_iter()
            .flat_map(move |frozen| [(frozen, idle, 0.0), (frozen, idle, -0.0784)])
    }) {
        {
            let mut client = Client::new([0.5, 64.0, 0.5]);
            client.on_ground = false;
            client.state.on_ground = false;
            client.velocity[1] = fall;
            let mut judge = Judge::new(client.position);
            judge.report(&mirror, client.position, false);
            for _ in 0..frozen {
                judge.tick_end();
            }
            for tick in 0..80 {
                let held = if tick < idle {
                    Input::default()
                } else {
                    let turned = (tick - idle + 1).min(3) as f32;
                    Input {
                        forward: true,
                        sprint: tick >= idle + 2,
                        yaw: -30.0 * turned,
                        ..Input::default()
                    }
                };
                judge.hold(&held);
                if let Some(to) = client.tick(&mirror, held) {
                    judge.report(&mirror, to, client.on_ground);
                }
                judge.tick_end();
            }
            assert_eq!(
                predict_flags(&judge),
                0,
                "frozen {frozen} idle {idle} fall {fall}: {:?} {:?}",
                judge.flags,
                judge
                    .tags
                    .iter()
                    .take(4)
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// The server's movement speed while freezing in powder snow, `tick`
/// ticks after the player waded in.
fn frost_speed(tick: usize) -> f64 {
    0.1 - 0.05 * (tick.min(140) as f64 / 140.0)
}

#[tuff_core::watchdog]
#[test]
fn walking_through_powder_snow_at_high_latency_is_not_flagged() {
    let snow = Block::PowderSnow.default_state();
    let mirror = scene(move |_, _| vec![(62, stone()), (63, snow)]);
    let lag = 5;
    for (seed, yaw) in [(0, 0.0), (1, 37.0), (2, 211.0)] {
        let start = [0.5 + f64::from(seed) * 0.13, 63.0, 0.5];
        let mut client = Client::new(start);
        let mut judge = Judge::new(start);
        judge.facts.latency_ms = 250;
        for _ in 0..20 {
            client.tick(&mirror, Input::default());
        }
        judge.facts.position = client.sent;
        for tick in 0..240 {
            judge.facts.tick = 100 + tick as u64;
            judge.facts.body.speed = frost_speed(tick);
            client.speed = frost_speed(tick.saturating_sub(lag));
            let held = Input {
                forward: true,
                sprint: tick % 80 > 40,
                yaw,
                ..Input::default()
            };
            judge.hold(&held);
            if let Some(to) = client.tick(&mirror, held) {
                judge.report(&mirror, to, client.on_ground);
            }
            judge.tick_end();
            let before = client.position;
            client.wrap();
            if before != client.position {
                judge.wrapped(client.sent);
            }
        }
        assert!(judge.flags.is_empty(), "seed {seed}: {:#?}", judge.flags);
    }
}
