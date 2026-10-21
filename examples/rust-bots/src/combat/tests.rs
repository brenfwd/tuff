// SPDX-License-Identifier: MIT
use super::*;
use crate::world::tests::Terrain;

fn skill(reaction_ticks: u32, aim_error_degrees: f32, turn_rate: f32) -> Skill {
    Skill {
        reaction_ticks,
        aim_error_degrees,
        turn_rate,
        click_cps_cap: 10.0,
        attack_charge: 0.9,
    }
}

fn spec(until: AttackUntil) -> AttackSpec {
    AttackSpec {
        target: "00000000000000000000000000000001".into(),
        until,
        retreat_health: 0.0,
    }
}

fn at(x: f64, z: f64) -> Vec3 {
    Vec3 { x, y: 64.0, z }
}

fn me(pos: Vec3) -> Me {
    Me {
        pos,
        yaw: 0.0,
        on_ground: true,
        health: 20.0,
        food: 20,
        allow_sprint: true,
        hotbar: vec![None; 9],
        selected: 0,
    }
}

fn target(pos: Vec3) -> Target {
    Target {
        pos,
        alive: true,
        width: 0.6,
        height: 1.95,
    }
}

fn act(c: Combat) -> (Drive, Option<Swing>, Option<u8>) {
    match c {
        Combat::Act {
            drive,
            swing,
            select,
        } => (drive, swing, select),
        other => panic!("expected an act, got {other:?}"),
    }
}

/// Runs a fight against a target circling the bot at 2 blocks, with the
/// bot standing still, and returns (hits, swings).
fn duel(skill: &Skill, ticks: u32, seed: u64) -> (u32, u32) {
    let mut f = Fighter::new(seed);
    let spec = spec(AttackUntil::Death);
    let mut body = me(at(0.5, 0.5));
    for tick in 0..ticks {
        let angle = f64::from(tick) * 0.15;
        let t = target(at(0.5 + 2.0 * angle.cos(), 0.5 + 2.0 * angle.sin()));
        body.on_ground = true;
        let _ = f.tick(&body, Some(&t), true, skill, &spec);
    }
    (f.hits, f.swings)
}

#[test]
fn weapons_rank_by_damage_per_second_and_speed_sets_the_cooldown() {
    assert_eq!(weapon("minecraft:iron_sword"), Some((6.0, 1.6)));
    assert_eq!(weapon("minecraft:stone_axe"), Some((9.0, 0.8)));
    assert_eq!(weapon("minecraft:stick"), None);
    assert_eq!(weapon("minecraft:wooden_pickaxe"), None);
    let hotbar = vec![
        Some("minecraft:dirt".to_owned()),
        Some("minecraft:stone_sword".to_owned()),
        None,
        Some("minecraft:diamond_sword".to_owned()),
        Some("minecraft:wooden_axe".to_owned()),
    ];
    assert_eq!(best_weapon(&hotbar), Some(3));
    assert_eq!(best_weapon(&[None, Some("minecraft:dirt".into())]), None);
    assert_eq!(attack_speed(None), 4.0);
    assert_eq!(attack_speed(Some("minecraft:iron_sword")), 1.6);
}

#[test]
fn reach_is_measured_from_the_eyes_to_the_box() {
    let d = reach_distance(&at(0.0, 0.0), &at(3.0, 0.0), 0.6, 1.95);
    assert!((d - 2.7).abs() < 1e-9, "{d}");
    let above = reach_distance(
        &at(0.0, 0.0),
        &Vec3 {
            x: 0.0,
            y: 70.0,
            z: 0.0,
        },
        0.6,
        1.95,
    );
    assert!((above - (70.0 - 65.62)).abs() < 1e-9, "{above}");
}

#[test]
fn walls_block_sight() {
    let ground = |x: f64, z: f64| Vec3 { x, y: 1.0, z };
    let mut open = Terrain::new(16, 16);
    let t = target(ground(4.5, 0.5));
    assert!(line_of_sight(&mut open, &ground(0.5, 0.5), &t));
    let mut walled = Terrain::new(16, 16);
    for y in 1..4 {
        walled.wall((2, y, 0));
    }
    assert!(!line_of_sight(&mut walled, &ground(0.5, 0.5), &t));
}

#[test]
fn the_bot_reacts_only_after_its_reaction_delay() {
    let mut f = Fighter::new(1);
    let s = skill(3, 0.0, 180.0);
    let spec = spec(AttackUntil::Death);
    let t = target(at(2.5, 0.5));
    for _ in 0..3 {
        let (drive, swing, _) = act(f.tick(&me(at(0.5, 0.5)), Some(&t), true, &s, &spec));
        assert_eq!((drive.forward, swing), (0, None));
    }
    let (drive, _, _) = act(f.tick(&me(at(0.5, 0.5)), Some(&t), true, &s, &spec));
    assert!((wrap(drive.yaw) - -90.0).abs() < 1e-3, "{}", drive.yaw);
}

#[test]
fn a_hidden_target_is_tracked_at_its_last_seen_position() {
    let mut f = Fighter::new(1);
    let s = skill(0, 0.0, 180.0);
    let spec = spec(AttackUntil::Death);
    let _ = f.tick(
        &me(at(0.5, 0.5)),
        Some(&target(at(10.5, 0.5))),
        true,
        &s,
        &spec,
    );
    let Combat::Goal(Goal::Leaf(LeafGoal::Near(n))) = f.tick(
        &me(at(0.5, 0.5)),
        Some(&target(at(0.5, 30.5))),
        false,
        &s,
        &spec,
    ) else {
        panic!("expected a chase");
    };
    assert_eq!((n.pos.x, n.pos.z), (10, 0));
}

#[test]
fn turning_is_capped_by_the_turn_rate() {
    let mut f = Fighter::new(1);
    let s = skill(0, 0.0, 10.0);
    let spec = spec(AttackUntil::Death);
    let t = target(at(0.5, -1.5));
    let (drive, _, _) = act(f.tick(&me(at(0.5, 0.5)), Some(&t), true, &s, &spec));
    assert!((drive.yaw.abs() - 10.0).abs() < 1e-3, "{}", drive.yaw);
}

#[test]
fn swings_wait_for_the_attack_charge_of_the_held_weapon() {
    let s = skill(0, 10.0, 180.0);
    let spec = spec(AttackUntil::Death);
    let t = target(at(0.5, 2.0));
    let mut body = me(at(0.5, 0.5));
    body.allow_sprint = false;
    body.hotbar[0] = Some("minecraft:iron_sword".into());
    let mut f = Fighter::new(7);
    let mut swung = Vec::new();
    for tick in 0..60u32 {
        let (drive, swing, _) = act(f.tick(&body, Some(&t), true, &s, &spec));
        body.on_ground = !drive.jump;
        if swing.is_some() {
            swung.push(tick);
        }
    }
    assert!(swung.len() >= 3, "{swung:?}");
    for pair in swung.windows(2) {
        assert!(pair[1] - pair[0] >= 11, "{swung:?}");
    }
}

#[test]
fn the_bot_switches_to_its_best_weapon_and_restarts_the_charge() {
    let s = skill(0, 0.0, 180.0);
    let spec = spec(AttackUntil::Death);
    let mut body = me(at(0.5, 0.5));
    body.hotbar[4] = Some("minecraft:diamond_sword".into());
    let mut f = Fighter::new(3);
    let (_, swing, select) = act(f.tick(&body, Some(&target(at(0.5, 2.0))), true, &s, &spec));
    assert_eq!((swing, select), (None, Some(4)));
}

#[test]
fn nothing_is_hit_beyond_reach() {
    let s = skill(0, 0.0, 180.0);
    let spec = spec(AttackUntil::Death);
    let mut f = Fighter::new(5);
    for _ in 0..40 {
        let (_, swing, _) = act(f.tick(
            &me(at(0.5, 0.5)),
            Some(&target(at(0.5, 4.2))),
            true,
            &s,
            &spec,
        ));
        assert_eq!(swing, None);
    }
}

#[test]
fn a_crit_jump_swings_only_while_falling() {
    let s = skill(0, 0.0, 180.0);
    let spec = spec(AttackUntil::Death);
    let t = target(at(0.5, 2.0));
    let mut f = Fighter::new(11);
    let mut body = me(at(0.5, 0.5));
    let mut vy = 0.0;
    let mut crits = 0;
    for _ in 0..200 {
        let (drive, swing, _) = act(f.tick(&body, Some(&t), true, &s, &spec));
        let falling = vy < 0.0 && !body.on_ground;
        if swing.is_some() && !body.on_ground {
            assert!(falling, "a swing in the air was not on the way down");
            crits += 1;
        }
        if drive.jump && body.on_ground {
            vy = 0.42;
            body.on_ground = false;
        }
        if !body.on_ground {
            body.pos.y += vy;
            vy = (vy - 0.08) * 0.98;
            if body.pos.y <= 64.0 {
                body.pos.y = 64.0;
                body.on_ground = true;
                vy = 0.0;
            }
        }
    }
    assert!(crits > 0);
}

#[test]
fn a_hit_while_sprinting_releases_sprint_briefly() {
    let mut s = skill(0, 0.0, 180.0);
    s.aim_error_degrees = 10.0;
    let spec = spec(AttackUntil::Death);
    let mut f = Fighter::new(2);
    let mut body = me(at(0.5, 0.5));
    let mut tapped = false;
    for _ in 0..60 {
        let t = target(at(0.5, 3.2));
        let (drive, swing, _) = act(f.tick(&body, Some(&t), true, &s, &spec));
        body.on_ground = true;
        if swing == Some(Swing::Hit) {
            let (next, _, _) = act(f.tick(&body, Some(&t), true, &s, &spec));
            assert!(!next.sprint && next.forward == 0, "{next:?}");
            tapped = true;
            break;
        }
        let _ = drive;
    }
    assert!(tapped);
}

#[test]
fn the_skill_changes_the_hit_rate() {
    let rate = |s: &Skill| {
        let (mut hits, mut swings) = (0, 0);
        for seed in 1..=8 {
            let (h, w) = duel(s, 400, seed);
            hits += h;
            swings += w;
        }
        assert!(swings > 0);
        f64::from(hits) / f64::from(swings)
    };
    let expert = rate(&skill(1, 0.5, 60.0));
    let novice = rate(&skill(10, 15.0, 12.0));
    assert!(expert > 0.9, "{expert}");
    assert!(novice < expert - 0.2, "novice {novice} expert {expert}");
}

#[test]
fn the_same_seed_replays_the_same_fight() {
    let s = skill(4, 6.0, 25.0);
    assert_eq!(duel(&s, 300, 9), duel(&s, 300, 9));
}

#[test]
fn until_conditions_end_the_fight() {
    let s = skill(0, 0.0, 180.0);
    let body = me(at(0.5, 0.5));
    let near = target(at(0.5, 2.0));
    let mut f = Fighter::new(1);
    let ticks = spec(AttackUntil::Ticks(2));
    assert!(matches!(
        f.tick(&body, Some(&near), true, &s, &ticks),
        Combat::Act { .. }
    ));
    assert!(matches!(
        f.tick(&body, Some(&near), true, &s, &ticks),
        Combat::Act { .. }
    ));
    assert_eq!(f.tick(&body, Some(&near), true, &s, &ticks), Combat::Done);
    let death = spec(AttackUntil::Death);
    let dead = Target {
        alive: false,
        ..near.clone()
    };
    assert_eq!(
        Fighter::new(1).tick(&body, Some(&dead), true, &s, &death),
        Combat::Done
    );
    assert!(matches!(
        Fighter::new(1).tick(&body, None, true, &s, &death),
        Combat::Failed(_)
    ));
    let mut fought = Fighter::new(1);
    let _ = fought.tick(&body, Some(&near), true, &s, &death);
    assert_eq!(fought.tick(&body, None, true, &s, &death), Combat::Done);
    let far = spec(AttackUntil::Distance(8.0));
    assert_eq!(
        Fighter::new(1).tick(&body, Some(&target(at(0.5, 12.0))), true, &s, &far),
        Combat::Done
    );
    assert!(matches!(
        Fighter::new(1).tick(&body, None, true, &s, &far),
        Combat::Failed(_)
    ));
}

#[test]
fn low_health_retreats_when_the_spec_allows_it() {
    let s = skill(0, 0.0, 180.0);
    let mut body = me(at(0.5, 0.5));
    body.health = 4.0;
    let mut retreat = spec(AttackUntil::Death);
    retreat.retreat_health = 6.0;
    let t = target(at(0.5, 2.0));
    assert!(matches!(
        Fighter::new(1).tick(&body, Some(&t), true, &s, &retreat),
        Combat::Goal(Goal::Leaf(LeafGoal::RunAway(_)))
    ));
    assert!(matches!(
        Fighter::new(1).tick(&body, Some(&t), true, &s, &spec(AttackUntil::Death)),
        Combat::Act { .. }
    ));
}
