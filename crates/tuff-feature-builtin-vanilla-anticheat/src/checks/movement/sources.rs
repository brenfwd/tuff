//! The sim client moved by what the server and other entities give it:
//! knockback and blasts, levitation, elytra flight with firework boosts,
//! boats and shulkers underfoot, and pushes from mobs.

use tuff_core::protocol::codec::lp_vec3_received;
use tuff_core::world::physics::{self, Aabb};
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_data::registry::EntityType;

use super::super::Nearby;
use super::super::velocity::Impulse;
use super::sim::{
    Client, Input, Judge, SETTLING, assert_mostly_exact, look, predict_flags, scene, stone,
};
use crate::trace::Tags;

fn flat() -> ChunkMirror {
    scene(|_, _| vec![(63, stone())])
}

/// Drives a client for `ticks` ticks; `each` sets the tick's server facts,
/// may move the client between ticks, and returns its input.
fn run(
    mirror: &ChunkMirror,
    start: [f64; 3],
    ticks: usize,
    wrap: bool,
    mut each: impl FnMut(usize, &mut Client, &mut Judge) -> Input,
) -> Judge {
    let mut client = Client::new(start);
    let mut judge = Judge::new(start);
    for tick in 0..ticks {
        judge.facts.tick = tick as u64;
        let held = each(tick, &mut client, &mut judge);
        judge.hold(&held);
        if let Some(to) = client.tick(mirror, held) {
            judge.report(mirror, to, client.on_ground);
        }
        judge.tick_end();
        if wrap {
            let before = client.position;
            client.wrap();
            if before != client.position {
                judge.wrapped(client.sent);
            }
        }
    }
    judge
}

fn tagged(judge: &Judge, tag: Tags) -> usize {
    judge.tags.iter().filter(|t| t.contains(tag)).count()
}

fn assert_clean(judge: &Judge, label: &str) {
    assert_eq!(predict_flags(judge), 0, "{label}: {:?}", judge.flags);
    assert_mostly_exact(judge, label);
}

/// Sprint-jumping in a slow circle.
fn circling(tick: usize) -> Input {
    Input {
        forward: true,
        sprint: true,
        jump: tick % 17 < 9,
        yaw: (tick * 7 % 360) as f32,
        ..Input::default()
    }
}

/// Every 25 ticks the server gives a velocity, which the client applies
/// a few ticks later, horizontally scaled by `scale`; `added` gives blasts.
fn knocked(seed: u64, added: bool, scale: f64) -> Judge {
    let mut seq = 0;
    let mut queued: Vec<(usize, [f64; 3])> = Vec::new();
    run(
        &flat(),
        [0.5, 64.0, 0.5],
        400,
        true,
        |tick, client, judge| {
            if tick >= 30 && tick % 25 == 5 {
                let angle = (seed as f64 + tick as f64) * 0.7;
                let sent = [angle.sin() * 0.45, 0.36, angle.cos() * 0.45];
                let velocity = if added { sent } else { lp_vec3_received(sent) };
                judge.facts.impulses.push(Impulse {
                    seq,
                    velocity,
                    added,
                });
                seq += 1;
                let delay = (seed as usize + tick / 25) % 4;
                let applied = [velocity[0] * scale, velocity[1], velocity[2] * scale];
                queued.push((tick + delay, applied));
            }
            judge.facts.impulses.retain(|i| i.seq + 2 >= seq);
            queued.retain(|&(at, velocity)| {
                if at != tick {
                    return true;
                }
                client.velocity = if added {
                    [0, 1, 2].map(|a| client.velocity[a] + velocity[a])
                } else {
                    velocity
                };
                false
            });
            circling(tick)
        },
    )
}

#[tuff_core::watchdog]
#[test]
fn knockback_and_blasts_are_replayed_exactly() {
    for seed in 0..6 {
        for added in [false, true] {
            let judge = knocked(seed, added, 1.0);
            let label = format!("seed {seed} added {added}");
            assert_clean(&judge, &label);
            assert!(tagged(&judge, Tags::IMPULSE) >= 14, "{label}");
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn amplified_knockback_is_flagged() {
    for added in [false, true] {
        let judge = knocked(1, added, 1.25);
        assert!(
            predict_flags(&judge) >= 10,
            "added {added}: {:?}",
            judge.flags
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn levitation_the_client_gains_and_loses_late_is_replayed_exactly() {
    for delay in 0..4 {
        let judge = run(
            &flat(),
            [0.5, 64.0, 0.5],
            300,
            true,
            |tick, client, judge| {
                let server = (60..160).contains(&tick);
                judge.facts.levitation = server;
                judge.facts.levitation_level = if server { 2 } else { 0 };
                client.levitation = (60 + delay..160 + delay).contains(&tick).then_some(1);
                Input {
                    forward: tick % 40 < 30,
                    jump: tick % 50 < 3,
                    yaw: (tick * 3 % 360) as f32,
                    ..Input::default()
                }
            },
        );
        assert_clean(&judge, &format!("delay {delay}"));
        assert!(judge.peak > 70.0, "peak {}", judge.peak);
        assert!(tagged(&judge, Tags::LEVITATION) > 80, "delay {delay}");
    }
}

/// A glide from high up that weaves and pitches, landing on the ground;
/// the server stops the glide a tick after landing and the client learns
/// of it `late` ticks after that.
fn gliding(late: usize, boost: Option<(usize, usize)>, cheat: f64) -> Judge {
    gliding_with(late, boost, 0, cheat)
}

fn gliding_with(late: usize, boost: Option<(usize, usize)>, extra: usize, cheat: f64) -> Judge {
    let mut landed = None;
    let mut last = [0.0f32; 2];
    run(
        &flat(),
        [0.5, 160.0, 0.5],
        500,
        true,
        |tick, client, judge| {
            if client.on_ground && landed.is_none() && tick > 5 {
                landed = Some(tick);
            }
            let server = landed.is_none_or(|at| tick <= at + 1);
            client.gliding = landed.is_none_or(|at| tick <= at + 1 + late);
            judge.facts.gliding = server;
            let input = Input {
                yaw: (tick as f32 * 2.5).sin() * 40.0 + tick as f32,
                pitch: (tick as f32 * 0.07).sin() * 35.0 + 10.0,
                ..Input::default()
            };
            if let Some((from, to)) = boost {
                judge.facts.firework = (from..to).contains(&tick);
                if (from + 2..to + 2 + extra).contains(&tick) && client.gliding {
                    let look = look(last[0], last[1]);
                    let v = client.velocity;
                    client.velocity =
                        [0, 1, 2].map(|a| v[a] + look[a] * 0.1 + (look[a] * 1.5 - v[a]) * 0.5);
                }
            }
            last = [input.yaw, input.pitch];
            if tick >= 30 && client.gliding && !client.on_ground {
                client.velocity[0] *= cheat;
                client.velocity[2] *= cheat;
            }
            input
        },
    )
}

#[tuff_core::watchdog]
#[test]
fn elytra_flight_and_landing_are_replayed_exactly() {
    for late in [0, 3] {
        let judge = gliding(late, None, 1.0);
        let label = format!("late {late}");
        assert_clean(&judge, &label);
        assert!(tagged(&judge, Tags::GLIDE) > 150, "{label}");
    }
}

#[tuff_core::watchdog]
#[test]
fn firework_boosts_are_replayed_exactly() {
    let judge = gliding(2, Some((40, 70)), 1.0);
    assert_clean(&judge, "boost");
    assert!(tagged(&judge, Tags::BOOST) >= 25, "boost");
}

#[tuff_core::watchdog]
#[test]
fn a_rocket_outliving_the_server_boost_is_replayed() {
    let judge = gliding_with(2, Some((40, 50)), 10, 1.0);
    assert_clean(&judge, "long rocket");
}

#[tuff_core::watchdog]
#[test]
fn firework_boosts_do_not_reseed_the_predictor() {
    let plain = gliding(2, None, 1.0);
    let judge = gliding(2, Some((40, 70)), 1.0);
    assert_eq!(
        tagged(&judge, Tags::SEEDED),
        tagged(&plain, Tags::SEEDED),
        "boost reseeded"
    );
}

#[tuff_core::watchdog]
#[test]
fn elytra_speed_of_a_few_percent_is_flagged() {
    let judge = gliding(0, None, 1.03);
    assert!(predict_flags(&judge) > 10, "{:?}", judge.flags);
}

fn entity(kind: EntityType, at: [f64; 3]) -> Aabb {
    super::super::entity_bounds(kind, at)
}

/// Walks back and forth across a solid entity box resting on the ground.
fn across(solid: Aabb, start: [f64; 3]) -> Judge {
    run(&flat(), start, 300, false, |tick, client, judge| {
        client.solids = vec![solid];
        judge.facts.nearby = vec![Nearby {
            bounds: solid,
            solid: true,
            fuzz: 0.0,
        }];
        Input {
            forward: true,
            jump: tick % 60 == 30,
            sprint: tick % 120 < 60,
            yaw: if tick % 80 < 40 { 0.0 } else { 180.0 },
            ..Input::default()
        }
    })
}

#[tuff_core::watchdog]
#[test]
fn walking_over_boats_and_shulkers_is_replayed_exactly() {
    for (kind, label) in [
        (EntityType::OakBoat, "boat"),
        (EntityType::Shulker, "shulker"),
    ] {
        let solid = entity(kind, [0.5, 64.0, 3.0]);
        let judge = across(solid, [0.5, 64.0, 0.5]);
        assert_clean(&judge, label);
        assert!(
            judge.peak > solid[4] - 1.0e-6,
            "{label}: peak {}",
            judge.peak
        );
    }
}

/// Pushes the client away from `other` the way an overlapping living
/// entity's own tick does.
fn push_from(client: &mut Client, other: Aabb) {
    let body = [
        client.position[0] - 0.3,
        client.position[1],
        client.position[2] - 0.3,
        client.position[0] + 0.3,
        client.position[1] + 1.8,
        client.position[2] + 0.3,
    ];
    if !physics::intersects(body, other) {
        return;
    }
    let mut dx = (other[0] + other[3]) / 2.0 - client.position[0];
    let mut dz = (other[2] + other[5]) / 2.0 - client.position[2];
    let mut d = dx.abs().max(dz.abs());
    if d < 0.01 {
        return;
    }
    d = d.sqrt();
    dx /= d;
    dz /= d;
    let scale = (1.0 / d).min(1.0);
    client.velocity[0] -= dx * scale * 0.05;
    client.velocity[2] -= dz * scale * 0.05;
}

#[tuff_core::watchdog]
#[test]
fn pushes_from_a_mob_in_the_way_are_replayed() {
    let zombie = entity(EntityType::Zombie, [0.7, 64.0, 3.0]);
    let judge = run(
        &flat(),
        [0.5, 64.0, 0.5],
        200,
        false,
        |tick, client, judge| {
            push_from(client, zombie);
            judge.facts.nearby = vec![Nearby {
                bounds: zombie,
                solid: false,
                fuzz: 0.0,
            }];
            Input {
                forward: tick % 60 < 45,
                yaw: if tick % 120 < 60 { 0.0 } else { 180.0 },
                ..Input::default()
            }
        },
    );
    assert_eq!(predict_flags(&judge), 0, "{:?}", judge.flags);
    assert!(tagged(&judge, Tags::ENTITY_PUSH) >= 5, "pushed");
    let missed = judge.misses.iter().filter(|&&n| n >= SETTLING).count();
    assert!(missed <= 2, "misses {:?}", judge.misses);
}

#[tuff_core::watchdog]
#[test]
fn hovering_in_open_air_is_flagged_and_standing_on_a_boat_is_not() {
    let hover = run(&flat(), [0.5, 100.0, 0.5], 60, true, |tick, client, _| {
        if tick >= 10 {
            client.velocity[1] = 0.0;
        }
        Input {
            forward: true,
            yaw: 30.0,
            ..Input::default()
        }
    });
    assert!(predict_flags(&hover) > 20, "{:?}", hover.flags);
    let boat = entity(EntityType::OakBoat, [0.5, 64.0, 0.5]);
    let resting = run(
        &flat(),
        [0.5, 66.0, 0.5],
        80,
        false,
        |tick, client, judge| {
            client.solids = vec![boat];
            judge.facts.nearby = vec![Nearby {
                bounds: boat,
                solid: true,
                fuzz: 0.0,
            }];
            Input {
                jump: tick % 30 == 10,
                ..Input::default()
            }
        },
    );
    assert_clean(&resting, "boat");
}

fn shoved_by(
    kind: tuff_core::world::session::ShoveKind,
    shove: impl Fn(&mut Client),
    spin: usize,
) -> Judge {
    let mut seq = 0;
    run(
        &flat(),
        [0.5, 64.0, 0.5],
        200,
        true,
        |tick, client, judge| {
            if tick >= 30 && tick % 40 == 10 {
                shove(client);
                judge.facts.shoves.push((
                    seq,
                    tuff_core::world::session::Shove {
                        tick: tick as u64,
                        kind,
                    },
                ));
                seq += 1;
            }
            judge.facts.shoves.retain(|(s, _)| *s + 1 >= seq);
            let spinning = tick >= 30 && (tick % 40 + 30 - 10) % 40 < spin + 30 - 10 && spin > 0;
            judge.facts.riptide = spinning;
            circling(tick)
        },
    )
}

#[tuff_core::watchdog]
#[test]
fn piston_shoves_are_replayed_exactly() {
    let delta = [0.25, 0.0, -0.125];
    let judge = shoved_by(
        tuff_core::world::session::ShoveKind::Moved(delta),
        |client| {
            for axis in 0..3 {
                client.position[axis] += delta[axis];
            }
        },
        0,
    );
    assert_clean(&judge, "piston");
}

#[tuff_core::watchdog]
#[test]
fn riptide_launches_are_replayed_exactly() {
    let (strength, yaw, pitch) = (3.0f32, 20.0f32, -35.0f32);
    let judge = shoved_by(
        tuff_core::world::session::ShoveKind::Riptide {
            strength,
            yaw,
            pitch,
        },
        |client| {
            let look = look(yaw, pitch);
            let length = look.iter().map(|c| c * c).sum::<f64>().sqrt();
            for axis in 0..3 {
                client.velocity[axis] += look[axis] * (f64::from(strength) / length);
            }
            if client.on_ground {
                client.position[1] += 1.199_999_928_474_426_3;
                client.on_ground = false;
            }
        },
        20,
    );
    assert_clean(&judge, "riptide");
}

#[tuff_core::watchdog]
#[test]
fn fuzzy_solids_blur_only_near_the_path() {
    let near = Nearby {
        bounds: [1.0, 0.0, 0.0, 2.0, 1.0, 1.0],
        solid: true,
        fuzz: 1.0,
    };
    assert!(near.blurs([0.0, 0.0, 0.5], [0.2, 0.0, 0.5]));
    assert!(!near.blurs([-5.0, 0.0, 0.5], [-4.8, 0.0, 0.5]));
    let sharp = Nearby { fuzz: 0.0, ..near };
    assert!(!sharp.blurs([0.0, 0.0, 0.5], [0.2, 0.0, 0.5]));
}
