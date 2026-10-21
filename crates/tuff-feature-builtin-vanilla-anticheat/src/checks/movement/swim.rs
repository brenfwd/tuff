//! The sim client swimming, crawling through one-block gaps and crouching
//! under ceilings too low to stand.

use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_data::{Block, BlockState};

use super::sim::{
    Client, Input, Judge, assert_mostly_exact, deep, hash, liquid, plain, pools, predict_flags,
    roaming, scene, stone, wade,
};
use crate::trace::Tags;

/// [`roaming`] keys, sprinting forward most of the time and looking up or
/// down by up to 75 degrees.
fn swimming(seed: u64) -> impl Fn(usize) -> Input {
    let roam = roaming(seed, true, false);
    move |tick| {
        let r = hash(seed, (tick / 12) as i32, 3, 29);
        let input = roam(tick);
        let forward = input.forward || !r.is_multiple_of(4);
        Input {
            forward,
            back: input.back && !forward,
            sprint: forward && !input.sneak && !r.is_multiple_of(5),
            pitch: ((r >> 8) % 150) as f32 - 74.5,
            ..input
        }
    }
}

fn swims(judge: &Judge) -> usize {
    judge
        .tags
        .iter()
        .filter(|tags| tags.contains(Tags::PREDICTED) && tags.contains(Tags::SWIM))
        .count()
}

/// Drives `mirror` from `seeds` and asserts no flag at all and exact
/// replays; returns how many exact replays swam.
fn assert_swims_cleanly(
    mirror: &ChunkMirror,
    start: [f64; 3],
    seeds: std::ops::Range<u64>,
    input: impl Fn(u64) -> Box<dyn Fn(usize) -> Input>,
    tune: impl Fn(&mut Client, &mut super::super::Facts) + Copy,
) -> usize {
    let mut flagged = Vec::new();
    let mut swum = 0;
    for seed in seeds {
        let judge = wade(mirror, start, 400, input(seed), tune, |_| {});
        swum += swims(&judge);
        assert_mostly_exact(&judge, &format!("seed {seed}"));
        if let Some((n, flag)) = judge.flags.first() {
            flagged.push(format!(
                "seed {seed}: {} flags, first at move {n}: {flag}",
                judge.flags.len()
            ));
        }
    }
    assert!(flagged.is_empty(), "{flagged:#?}");
    swum
}

fn swim_keys(seed: u64) -> Box<dyn Fn(usize) -> Input> {
    Box::new(swimming(seed))
}

/// Deep water over a floor with reefs: pillars, and slabs that leave
/// one-block and half-block gaps under them.
fn reef() -> ChunkMirror {
    let source = liquid(Block::Water, 0);
    let slab = slab("bottom");
    scene(move |x, z| {
        let mut column: Vec<(i32, BlockState)> = [(39, stone())]
            .into_iter()
            .chain((40..65).map(|y| (y, source)))
            .collect();
        match (x / 3 + z / 5) % 5 {
            0 => column.extend((40..44).map(|y| (y, stone()))),
            1 => column.push((42, stone())),
            2 => column.push((41, slab)),
            _ => {}
        }
        column
    })
}

fn slab(kind: &str) -> BlockState {
    Block::StoneSlab
        .default_state()
        .with("type", kind)
        .expect("slab type")
}

#[tuff_core::watchdog]
#[test]
fn sprint_swimming_is_replayed_exactly() {
    let swum = assert_swims_cleanly(
        &deep(Block::Water),
        [0.5, 50.0, 0.5],
        0..10,
        swim_keys,
        plain,
    );
    assert!(swum > 1500, "{swum} exact swims in the ocean");
    let swum = assert_swims_cleanly(&reef(), [0.5, 50.0, 0.5], 0..10, swim_keys, plain);
    assert!(swum > 1200, "{swum} exact swims over the reef");
    let swum = assert_swims_cleanly(
        &pools(Block::Water),
        [0.5, 67.0, 0.5],
        0..20,
        swim_keys,
        plain,
    );
    assert!(swum > 1500, "{swum} exact swims in pools");
}

/// One-block tunnels, a ceiling one and a half blocks up and open ground.
fn burrows() -> ChunkMirror {
    let top = slab("top");
    scene(move |x, z| match (x / 4 + z / 4) % 3 {
        0 => vec![(63, stone()), (65, stone())],
        1 => vec![(63, stone()), (65, top)],
        _ => vec![(63, stone())],
    })
}

#[tuff_core::watchdog]
#[test]
fn crawling_and_crouching_under_low_ceilings_are_replayed_exactly() {
    let prone = |client: &mut Client, _: &mut super::super::Facts| client.prone = true;
    let roam = |seed| -> Box<dyn Fn(usize) -> Input> { Box::new(roaming(seed, true, true)) };
    assert_swims_cleanly(&burrows(), [0.5, 64.0, 0.5], 0..20, roam, prone);
    let crawl = |seed| -> Box<dyn Fn(usize) -> Input> {
        let roam = roaming(seed, false, false);
        Box::new(move |tick| Input {
            sprint: false,
            ..roam(tick)
        })
    };
    let tunnel = scene(|_, _| vec![(63, stone()), (65, stone())]);
    assert_swims_cleanly(&tunnel, [0.5, 64.0, 0.5], 0..5, crawl, prone);
    let low = scene(|_, _| vec![(63, stone()), (65, slab("top"))]);
    assert_swims_cleanly(&low, [0.5, 64.0, 0.5], 0..5, crawl, plain);
}

#[tuff_core::watchdog]
#[test]
fn swim_speed_and_rise_cheats_of_a_few_percent_are_flagged() {
    let mirror = deep(Block::Water);
    let start = [0.5, 50.0, 0.5];
    let run = |pitch: f32, cheat: &dyn Fn(&mut Client)| {
        let input = move |_| Input {
            forward: true,
            sprint: true,
            yaw: 20.0,
            pitch,
            ..Input::default()
        };
        wade(&mirror, start, 200, input, plain, cheat)
    };
    for pitch in [-10.0, 15.0] {
        let honest = run(pitch, &|_| {});
        assert_eq!(predict_flags(&honest), 0, "{pitch}: {:?}", honest.flags);
        assert!(
            swims(&honest) > 150,
            "{pitch}: {} exact swims",
            swims(&honest)
        );
        let speed = run(pitch, &|c| c.boost = 1.04);
        assert!(
            predict_flags(&speed) > 100,
            "{pitch} speed: {} {:?}",
            predict_flags(&speed),
            &speed.flags[..speed.flags.len().min(3)]
        );
    }
    let rise = run(-10.0, &|c| c.velocity[1] += 0.01);
    assert!(
        predict_flags(&rise) > 50,
        "rise: {} {:?}",
        predict_flags(&rise),
        &rise.flags[..rise.flags.len().min(3)]
    );
}
