// SPDX-License-Identifier: MIT
use super::*;

fn v(x: f64, y: f64, z: f64) -> Vec3 {
    Vec3 { x, y, z }
}

fn skill(error: f32) -> Skill {
    Skill {
        reaction_ticks: 0,
        aim_error_degrees: error,
        turn_rate: 90.0,
        click_cps_cap: 10.0,
        attack_charge: 0.9,
    }
}

fn me(hotbar: Vec<Option<String>>, selected: u8) -> Me {
    Me {
        pos: v(0.0, 64.0, 0.0),
        yaw: 0.0,
        on_ground: true,
        health: 20.0,
        food: 20,
        allow_sprint: true,
        hotbar,
        selected,
    }
}

fn bow_bar() -> Vec<Option<String>> {
    let mut bar = vec![None; 9];
    bar[2] = Some("minecraft:bow".into());
    bar
}

fn shoot() -> ShootSpec {
    ShootSpec {
        target: "00000000000000000000000000000001".into(),
        until: AttackUntil::Death,
        melee_range: 4.0,
    }
}

fn target(pos: Vec3) -> Target {
    Target {
        pos,
        alive: true,
        width: 0.6,
        height: 1.8,
    }
}

#[test]
fn a_stationary_target_at_twenty_blocks_is_hit_by_the_solved_angle() {
    let eye = eye_of(&v(0.0, 64.0, 0.0));
    let point = v(0.0, 64.0 + 1.8 * AIM_POINT, 20.0);
    let (yaw, pitch, ticks) = lead(&ARROW, &eye, &point, &v(0.0, 0.0, 0.0)).unwrap();
    assert!(yaw.abs() < 0.01, "{yaw}");
    assert!(pitch < 0.0 && pitch > -5.0, "{pitch}");
    assert!(ticks > 5.0 && ticks < 12.0, "{ticks}");
    let (y, _) = flight(&ARROW, f64::from(-pitch), 20.0).unwrap();
    assert!((y - (point.y - eye.y)).abs() < 0.01, "{y}");
}

#[test]
fn a_target_beyond_range_has_no_solution() {
    assert!(solve_angle(&ARROW, 1000.0, 0.0).is_none());
}

#[test]
fn a_target_walking_sideways_is_led_ahead_of_it() {
    let eye = eye_of(&v(0.0, 64.0, 0.0));
    let point = v(0.0, 65.0, 20.0);
    let (still, _, _) = lead(&ARROW, &eye, &point, &v(0.0, 0.0, 0.0)).unwrap();
    let (moving, _, ticks) = lead(&ARROW, &eye, &point, &v(0.2, 0.0, 0.0)).unwrap();
    assert!(moving < still - 1.0, "yaw turns toward the motion");
    let landing = 0.2 * ticks;
    let hit = (-(landing)).atan2(20.0).to_degrees() as f32;
    assert!((moving - hit).abs() < 1.0, "{moving} vs {hit}");
}

#[test]
fn a_target_above_needs_more_pitch_than_one_level() {
    let eye = eye_of(&v(0.0, 64.0, 0.0));
    let (_, level, _) = lead(&ARROW, &eye, &v(0.0, 64.0, 20.0), &v(0.0, 0.0, 0.0)).unwrap();
    let (_, above, _) = lead(&ARROW, &eye, &v(0.0, 74.0, 20.0), &v(0.0, 0.0, 0.0)).unwrap();
    assert!(above < level - 10.0);
}

fn run_archer(
    archer: &mut Archer,
    ticks: usize,
    target_at: Vec3,
    bar: &[Option<String>],
) -> Vec<Shot> {
    (0..ticks)
        .map(|_| {
            archer.tick(
                &me(bar.to_vec(), 2),
                Some(&target(target_at.clone())),
                true,
                &skill(0.0),
                &shoot(),
                true,
            )
        })
        .collect()
}

fn draws(shots: &[Shot]) -> Vec<(usize, Draw)> {
    shots
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s {
            Shot::Act { draw, .. } if *draw != Draw::None => Some((i, *draw)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_archer_draws_twenty_ticks_then_releases_on_aim() {
    let mut archer = Archer::new(1);
    let shots = run_archer(&mut archer, 30, v(0.0, 64.0, 20.0), &bow_bar());
    let pulls = draws(&shots);
    assert_eq!(pulls[0], (0, Draw::Start));
    assert_eq!(pulls[1].1, Draw::Release);
    assert!(pulls[1].0 >= 20, "{pulls:?}");
    assert_eq!(archer.shots, 1);
}

#[test]
fn the_archer_selects_the_bow_first() {
    let mut archer = Archer::new(1);
    let first = archer.tick(
        &me(bow_bar(), 0),
        Some(&target(v(0.0, 64.0, 20.0))),
        true,
        &skill(0.0),
        &shoot(),
        true,
    );
    match first {
        Shot::Act { select, draw, .. } => {
            assert_eq!(select, Some(2));
            assert_eq!(draw, Draw::None);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn inside_melee_range_the_archer_hands_over_and_lets_go() {
    let mut archer = Archer::new(1);
    run_archer(&mut archer, 5, v(0.0, 64.0, 20.0), &bow_bar());
    let close = run_archer(&mut archer, 1, v(0.0, 64.0, 2.0), &bow_bar());
    assert_eq!(close[0], Shot::Melee { release: true });
}

#[test]
fn no_bow_or_no_arrows_fails() {
    let mut archer = Archer::new(1);
    let shots = run_archer(&mut archer, 1, v(0.0, 64.0, 20.0), &vec![None; 9]);
    assert_eq!(shots[0], Shot::Failed("no bow".into()));
    let mut archer = Archer::new(1);
    let out = archer.tick(
        &me(bow_bar(), 2),
        Some(&target(v(0.0, 64.0, 20.0))),
        true,
        &skill(0.0),
        &shoot(),
        false,
    );
    assert_eq!(out, Shot::Failed("no arrows".into()));
}

#[test]
fn a_dead_or_gone_target_ends_the_shoot() {
    let mut archer = Archer::new(1);
    run_archer(&mut archer, 2, v(0.0, 64.0, 20.0), &bow_bar());
    let gone = archer.tick(&me(bow_bar(), 2), None, false, &skill(0.0), &shoot(), true);
    assert_eq!(gone, Shot::Done);
}

fn snowball(hold: u32, at: Option<LookTarget>) -> UseItemSpec {
    UseItemSpec {
        item: "minecraft:snowball".into(),
        look_at: at,
        hold_ticks: hold,
    }
}

fn snow_bar() -> Vec<Option<String>> {
    let mut bar = vec![None; 9];
    bar[4] = Some("minecraft:snowball".into());
    bar
}

#[test]
fn use_item_selects_looks_presses_then_finishes() {
    let mut user = ItemUser::new();
    let spec = snowball(0, Some(LookTarget::Position(v(10.0, 65.0, 0.0))));
    let aim = Some((v(10.0, 65.0, 0.0), v(0.0, 0.0, 0.0)));
    let first = user.tick(&me(snow_bar(), 0), &spec, aim.clone(), &skill(0.0));
    assert!(matches!(
        first,
        Use::Act {
            select: Some(4),
            ..
        }
    ));
    let mut pressed = None;
    for tick in 0..6 {
        if let Use::Act {
            action: UseAction::Press,
            look: Some((yaw, _)),
            ..
        } = user.tick(&me(snow_bar(), 4), &spec, aim.clone(), &skill(0.0))
        {
            pressed = Some((tick, yaw));
            break;
        }
    }
    let (_, yaw) = pressed.expect("pressed");
    assert!(
        (yaw - -90.0).abs() < 2.0 || (yaw - 270.0).abs() < 2.0,
        "{yaw}"
    );
    assert_eq!(
        user.tick(&me(snow_bar(), 4), &spec, aim, &skill(0.0)),
        Use::Done
    );
}

#[test]
fn use_item_holds_then_releases() {
    let mut user = ItemUser::new();
    let spec = snowball(3, None);
    let mut actions = Vec::new();
    for _ in 0..8 {
        match user.tick(&me(snow_bar(), 4), &spec, None, &skill(0.0)) {
            Use::Act { action, .. } => actions.push(action),
            Use::Done => break,
            Use::Failed(e) => panic!("{e}"),
        }
    }
    assert_eq!(
        actions,
        [
            UseAction::Press,
            UseAction::None,
            UseAction::None,
            UseAction::None,
            UseAction::Release
        ]
    );
}

#[test]
fn use_item_without_the_item_fails() {
    let mut user = ItemUser::new();
    let out = user.tick(&me(vec![None; 9], 0), &snowball(0, None), None, &skill(0.0));
    assert!(matches!(out, Use::Failed(_)));
}
