use super::super::facts;
use super::*;
use tuff_core::protocol::play::Movement;

fn run(check: &mut SustainedSpeed, f: &mut Facts, steps: &[f64]) -> Option<usize> {
    for (n, d) in steps.iter().enumerate() {
        let target = [f.position[0] + d, f.position[1], f.position[2]];
        let packet = Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some(target),
            rotation: None,
            on_ground: true,
        });
        if check.examine(f, &packet).is_some() {
            return Some(n);
        }
        f.position = target;
    }
    None
}

fn ground() -> Facts {
    let mut f = facts();
    f.terrain = Some(Terrain {
        known: true,
        supported: true,
        ..Terrain::default()
    });
    f
}

#[tuff_core::watchdog]
#[test]
fn a_bot_at_point_six_per_tick_is_flagged_within_a_few_ticks() {
    let hit = run(&mut SustainedSpeed::default(), &mut ground(), &[0.6; 40]);
    assert!(hit.is_some_and(|n| n < 8));
}

#[tuff_core::watchdog]
#[test]
fn sprint_jumping_and_speed_effects_are_not_flagged() {
    let hops: Vec<f64> = (0..200)
        .map(|n| if n % 12 < 5 { 0.48 } else { 0.3 })
        .collect();
    assert!(run(&mut SustainedSpeed::default(), &mut ground(), &hops).is_none());
    let mut f = ground();
    f.speed = Some(1);
    assert!(run(&mut SustainedSpeed::default(), &mut f, &[0.7; 200]).is_none());
    assert!(run(&mut SustainedSpeed::default(), &mut f, &[1.2; 200]).is_some());
}

#[tuff_core::watchdog]
#[test]
fn a_toggled_sneak_keeps_the_walking_limit_until_it_settles() {
    let mut f = ground();
    f.sneaking = true;
    assert!(run(&mut SustainedSpeed::default(), &mut f, &[0.3; 40]).is_some());
    let mut check = SustainedSpeed::default();
    let mut f = ground();
    for n in 0..60 {
        f.sneaking = n % 6 < 3;
        assert!(run(&mut check, &mut f, &[0.45]).is_none());
    }
}

#[tuff_core::watchdog]
#[test]
fn ice_knockback_and_unknown_terrain_reset_the_account() {
    let mut f = ground();
    f.terrain = Some(Terrain {
        slippery: true,
        ..f.terrain.unwrap()
    });
    assert!(run(&mut SustainedSpeed::default(), &mut f, &[0.7; 100]).is_none());
    let mut f = ground();
    f.velocity = true;
    assert!(run(&mut SustainedSpeed::default(), &mut f, &[0.7; 100]).is_none());
    let mut f = ground();
    f.terrain = None;
    assert!(run(&mut SustainedSpeed::default(), &mut f, &[0.7; 100]).is_none());
}

#[tuff_core::watchdog]
#[test]
fn a_knockback_burst_does_not_leave_a_debt() {
    let mut check = SustainedSpeed::default();
    let mut f = ground();
    f.velocity = true;
    assert!(run(&mut check, &mut f, &[0.9; 5]).is_none());
    f.velocity = false;
    assert!(run(&mut check, &mut f, &[0.3; 20]).is_none());
}

#[tuff_core::watchdog]
#[test]
fn grace_relaxes_the_limit_and_an_enormous_move_is_one_light_setback() {
    let to = |x: f64| {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some([x, 64.0, 0.0]),
            rotation: None,
            on_ground: true,
        })
    };
    let air = Terrain {
        known: true,
        ..Terrain::default()
    };
    let mut f = facts();
    f.terrain = Some(air);
    let mut check = MoveEnvelope::default();
    assert!(check.examine(&f, &to(1.0)).is_some());
    f.grace = true;
    assert!(check.examine(&f, &to(1.0)).is_none());
    assert!(check.examine(&f, &to(1.3)).is_some());
    f.terrain = Some(Terrain {
        slippery: true,
        ..air
    });
    assert!(check.examine(&f, &to(1.0)).is_none());
    let huge = check.examine(&f, &to(500.0)).unwrap();
    assert_eq!(huge.weight, 2.0);
}
