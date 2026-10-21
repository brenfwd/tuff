//! The sim client roaming a course where every surface the predictor
//! models lies next to every other: ice, slime, honey, soul sand, webs,
//! water, lava, ladders, stairs, slabs and ledges; and pushing itself out
//! of a block that landed in its body.

use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_data::{Block, BlockState};

use super::sim::{
    Client, Input, Judge, World, assert_mostly_exact, hash, liquid, plain, roaming, scene, stone,
    wade,
};
use crate::trace::Tags;

fn with(block: Block, props: &[(&str, &str)]) -> BlockState {
    props.iter().fold(block.default_state(), |s, (k, v)| {
        s.with(k, v).expect("property")
    })
}

/// Two-block cells, each a surface picked by a hash of the cell, on a
/// stone floor at 63.
fn course(seed: u64) -> ChunkMirror {
    let floors = [
        stone(),
        Block::Ice.default_state(),
        Block::BlueIce.default_state(),
        Block::SlimeBlock.default_state(),
        Block::HoneyBlock.default_state(),
        Block::SoulSand.default_state(),
        Block::Mud.default_state(),
        with(Block::StoneSlab, &[("type", "bottom")]),
        with(
            Block::StoneStairs,
            &[("facing", "east"), ("half", "bottom")],
        ),
        with(
            Block::StoneStairs,
            &[("facing", "north"), ("half", "bottom")],
        ),
    ];
    let water = liquid(Block::Water, 0);
    let lava = liquid(Block::Lava, 0);
    let web = Block::Cobweb.default_state();
    scene(move |x, z| {
        let cell = hash(seed, x / 2, z / 2, 1);
        let mut column = vec![(61, stone()), (62, stone())];
        let facing = ["north", "east", "south", "west"][(cell >> 8) as usize % 4];
        match cell % 24 {
            0 | 1 => column.extend([(62, water), (63, water)]),
            2 => column.extend([(63, lava)]),
            3 => column.extend([(63, stone()), (64, web)]),
            4 => column.extend([(63, stone()), (64, stone())]),
            5 => {
                let ladder = with(Block::Ladder, &[("facing", facing)]);
                column.extend([(63, stone())]);
                column.extend((64..68).map(|y| (y, ladder)));
            }
            6 => column.extend([(63, stone()), (64, stone()), (65, stone())]),
            7 => column.extend([
                (62, stone()),
                (63, liquid(Block::Water, (cell >> 16) as u8 % 8)),
            ]),
            8 => column.extend([(63, stone()), (64, Block::PowderSnow.default_state())]),
            9 => column.extend([
                (63, Block::HoneyBlock.default_state()),
                (64, Block::HoneyBlock.default_state()),
            ]),
            10 => column.extend([
                (63, stone()),
                (64, with(Block::SweetBerryBush, &[("age", "2")])),
            ]),
            11 => column.extend([(63, stone()), (64, Block::Scaffolding.default_state())]),
            12 => column.extend([
                (63, stone()),
                (
                    64,
                    with(
                        Block::OakTrapdoor,
                        &[("facing", facing), ("half", "bottom")],
                    ),
                ),
            ]),
            13 => column.extend([(63, stone()), (64, Block::OakFence.default_state())]),
            14 => column.extend([(63, stone()), (64, with(Block::Snow, &[("layers", "3")]))]),
            15 => column.extend([
                (63, stone()),
                (64, with(Block::StoneSlab, &[("type", "bottom")])),
            ]),
            n => column.push((
                63,
                floors[(n as usize + (cell >> 12) as usize) % floors.len()],
            )),
        }
        if (cell >> 20).is_multiple_of(5) {
            column.push((66, stone()));
        }
        column
    })
}

#[tuff_core::watchdog]
#[test]
fn roaming_a_mixed_course_is_never_flagged() {
    let mut flagged = Vec::new();
    let mut moves = 0;
    for seed in 0..40 {
        let mirror = course(seed);
        let judge = wade(
            &mirror,
            [0.5, 70.0, 0.5],
            600,
            roaming(seed, true, true),
            plain,
            |_| {},
        );
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
    assert!(moves > 15_000, "{moves} moves");
}

/// A stone floor at 63, and gravel at 64 in `cells` when given.
fn floor(cells: &[(i32, i32)]) -> ChunkMirror {
    let mut world = World::new();
    world.tile(|_, _| vec![(63, stone())]);
    for &(x, z) in cells {
        world.set(x, 64, z, Block::Gravel.default_state());
    }
    world.mirror()
}

#[tuff_core::watchdog]
#[test]
fn a_block_landing_in_the_body_is_left_without_a_flag() {
    let open = floor(&[]);
    let mut pushed = 0;
    let mut flagged = Vec::new();
    for case in 0..48u32 {
        let x = 0.05 + f64::from(case % 8) * 0.125;
        let z = 0.1 + f64::from(case / 8) * 0.16;
        let walls: &[(i32, i32)] = match case % 3 {
            0 => &[(0, 0)],
            1 => &[(0, 0), (-1, 0)],
            _ => &[(0, 0), (0, 1), (1, 0)],
        };
        let filled = floor(walls);
        let mut client = Client::new([x, 64.0, z]);
        let mut judge = Judge::new(client.sent);
        let walk = case % 2 == 1;
        for tick in 0..60 {
            let mirror = if tick < 20 { &open } else { &filled };
            let held = Input {
                forward: walk && tick >= 30,
                yaw: (case * 45) as f32,
                ..Input::default()
            };
            judge.hold(&held);
            if let Some(to) = client.tick(mirror, held) {
                judge.report(mirror, to, client.on_ground);
            }
            judge.tick_end();
        }
        pushed += judge
            .tags
            .iter()
            .filter(|t| t.contains(Tags::PUSHED_OUT) && t.contains(Tags::PREDICTED))
            .count();
        assert_mostly_exact(&judge, &format!("case {case} at {x},{z}"));
        if let Some((n, flag)) = judge.flags.first() {
            flagged.push(format!("case {case} at {x},{z} move {n}: {flag}"));
        }
    }
    assert!(flagged.is_empty(), "{flagged:#?}");
    assert!(pushed > 100, "{pushed} pushed out moves");
}

/// Cobwebs over a third of a stone floor, two blocks tall.
fn webs() -> ChunkMirror {
    let web = Block::Cobweb.default_state();
    scene(move |x, z| {
        if (x / 2 + z / 3) % 3 == 0 {
            vec![(63, stone()), (64, web), (65, web)]
        } else {
            vec![(63, stone())]
        }
    })
}

#[tuff_core::watchdog]
#[test]
fn webs_hold_a_weaver_back_less_and_the_predictor_knows() {
    let mirror = webs();
    let weaver = |client: &mut Client, facts: &mut super::super::Facts| {
        client.weaving = true;
        facts.weaving = true;
    };
    let mut stuck = 0;
    let mut unseen = 0;
    for seed in 0..10 {
        let judge = wade(
            &mirror,
            [2.5, 64.0, 0.5],
            400,
            roaming(seed, true, false),
            weaver,
            |_| {},
        );
        assert_mostly_exact(&judge, &format!("seed {seed}"));
        assert!(judge.flags.is_empty(), "seed {seed}: {:#?}", judge.flags);
        stuck += judge
            .tags
            .iter()
            .filter(|t| t.contains(Tags::STUCK) && t.contains(Tags::PREDICTED))
            .count();
        if seed < 3 {
            let hidden = |client: &mut Client, _: &mut super::super::Facts| client.weaving = true;
            let judge = wade(
                &mirror,
                [2.5, 64.0, 0.5],
                200,
                roaming(seed, true, false),
                hidden,
                |_| {},
            );
            unseen += judge.flags.len();
        }
    }
    assert!(stuck > 200, "{stuck} exact moves in webs");
    assert!(
        unseen > 10,
        "an unseen Weaving effect gave only {unseen} flags"
    );
}
