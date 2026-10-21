use super::super::facts;
use super::*;
use tuff_core::protocol::play::Movement;

fn to(x: f64, y: f64, z: f64) -> Serverbound {
    Serverbound::Move(Movement {
        horizontal_collision: false,
        position: Some([x, y, z]),
        rotation: None,
        on_ground: false,
    })
}

fn air() -> Terrain {
    Terrain {
        known: true,
        ..Terrain::default()
    }
}

fn solid_ground() -> Terrain {
    Terrain {
        supported: true,
        ..air()
    }
}

#[tuff_core::watchdog]
#[test]
fn soul_sand_slows_the_next_use_bound_unless_soul_speed() {
    let bound_after = |sticky: bool, soul_speed: bool| {
        let mut momentum = UseMomentum::default();
        let mut f = facts();
        f.item_use = Some(super::super::ItemUse {
            item: Item::Bread,
            elapsed: 1,
            remaining: Some(10),
        });
        f.body.efficiency = if soul_speed { 1.0 } else { 0.0 };
        f.terrain = Some(Terrain {
            supported: true,
            sticky,
            ..air()
        });
        let mut x = 0.0;
        let mut last = None;
        for _ in 0..4 {
            f.position[0] = x;
            x += 0.1;
            last = momentum.allowed(
                &f,
                &Movement {
                    horizontal_collision: false,
                    position: Some([x, 64.0, 0.0]),
                    rotation: None,
                    on_ground: true,
                },
                [x, 64.0, 0.0],
            );
        }
        last
    };
    let plain = bound_after(false, false).unwrap();
    assert!(bound_after(true, false).unwrap() < plain);
    assert_eq!(bound_after(true, true), Some(plain));
}

#[tuff_core::watchdog]
#[test]
fn use_bound_takes_friction_from_the_block_underfoot() {
    let bound_on = |friction: f32, grip: f32| {
        let mut momentum = UseMomentum::default();
        let mut f = facts();
        f.item_use = Some(super::super::ItemUse {
            item: Item::Bread,
            elapsed: 1,
            remaining: Some(10),
        });
        f.terrain = Some(Terrain {
            supported: true,
            friction,
            grip,
            traction: friction,
            slippery: friction > 0.9,
            bouncy: friction < 0.9 && friction > 0.7,
            ..air()
        });
        let mut x = 0.0;
        let mut last = None;
        for _ in 0..5 {
            f.position[0] = x;
            x += 0.1;
            last = momentum.allowed(
                &f,
                &Movement {
                    horizontal_collision: false,
                    position: Some([x, 64.0, 0.0]),
                    rotation: None,
                    on_ground: true,
                },
                [x, 64.0, 0.0],
            );
        }
        last
    };
    let stone = bound_on(0.6, 0.6).unwrap();
    let slime = bound_on(0.8, 0.8).unwrap();
    let ice = bound_on(0.98, 0.98).unwrap();
    let blue = bound_on(0.989, 0.989).unwrap();
    assert!(stone < slime && slime < ice && ice < blue);
    let mixed = bound_on(0.6, 0.98).unwrap();
    assert!(stone < mixed);
}

fn run_timer(rate: f32, period_ms: u64, count: u64, lag_ticks: bool) -> usize {
    let mut check = TimerBalance::default();
    let start = Instant::now();
    let mut f = facts();
    f.tick_rate = rate;
    let mut flags = 0;
    for n in 0..count {
        f.now = start + Duration::from_millis(n * period_ms);
        f.tick = if lag_ticks { n * period_ms / 200 } else { n };
        flags += usize::from(check.examine(&f, &to(0.0, 64.0, 0.0)).is_some());
    }
    flags
}

#[tuff_core::watchdog]
#[test]
fn water_and_ladders_hold_while_lava_fire_and_lit_campfires_hurt() {
    assert!(holds(Block::Water.default_state()));
    assert!(holds(Block::Ladder.default_state()));
    assert!(!holds(Block::Lava.default_state()));
    assert!(hurts(Block::Lava.default_state()));
    assert!(hurts(Block::Fire.default_state()));
    assert!(hurts(Block::MagmaBlock.default_state()));
    assert!(!hurts(Block::Stone.default_state()));
    assert!(!hurts(Block::Water.default_state()));
    let campfire = Block::Campfire.default_state();
    assert_eq!(hurts(campfire), campfire.get("lit") == Some("true"));
}

#[tuff_core::watchdog]
#[test]
fn a_client_at_full_rate_is_never_flagged_at_any_tick_rate() {
    for rate in [1.0, 5.0, 20.0, 100.0] {
        assert_eq!(run_timer(rate, 50, 2000, false), 0, "rate {rate}");
    }
}

#[tuff_core::watchdog]
#[test]
fn a_slow_client_on_a_slow_server_is_never_flagged() {
    assert_eq!(run_timer(5.0, 200, 500, false), 0);
    assert_eq!(run_timer(0.5, 2000, 100, false), 0);
}

#[tuff_core::watchdog]
#[test]
fn a_timer_hack_drains_the_budget_and_is_flagged() {
    assert!(run_timer(20.0, 25, 400, false) > 100);
    assert!(run_timer(20.0, 5, 400, false) > 300);
}

#[tuff_core::watchdog]
#[test]
fn a_lag_burst_stays_in_budget_but_a_flood_does_not() {
    let mut check = TimerBalance::default();
    let start = Instant::now();
    let mut f = facts();
    for n in 0..40u64 {
        f.now = start + Duration::from_millis(n * 50);
        assert!(check.examine(&f, &to(0.0, 64.0, 0.0)).is_none());
    }
    let after = start + Duration::from_millis(40 * 50 + 3000);
    for n in 0..60u64 {
        f.now = after + Duration::from_micros(n * 100);
        assert!(check.examine(&f, &to(0.0, 64.0, 0.0)).is_none(), "{n}");
    }
    let mut flagged = false;
    for n in 0..400u64 {
        f.now = after + Duration::from_millis(10) + Duration::from_micros(n * 100);
        flagged |= check.examine(&f, &to(0.0, 64.0, 0.0)).is_some();
    }
    assert!(flagged);
}

#[tuff_core::watchdog]
#[test]
fn repeated_blink_cycles_never_gain_more_than_the_reserve() {
    let mut check = TimerBalance::default();
    let start = Instant::now();
    let mut f = facts();
    let mut now = start;
    let mut accepted = 0u64;
    for _ in 0..12 {
        now += Duration::from_millis(5000);
        for n in 0..400u64 {
            f.now = now + Duration::from_micros(n * 100);
            accepted += u64::from(check.examine(&f, &to(0.0, 64.0, 0.0)).is_none());
        }
        now += Duration::from_millis(40);
    }
    let elapsed_ticks = (now - start).as_millis() as u64 / 50;
    assert!(
        accepted <= elapsed_ticks + 70,
        "{accepted} > {elapsed_ticks}"
    );
}

#[tuff_core::watchdog]
#[test]
fn server_lag_widens_the_burst_allowance() {
    let burst = |lagging: bool| {
        let mut check = TimerBalance::default();
        let start = Instant::now();
        let mut f = facts();
        let mut flags = 0;
        for n in 0..200u64 {
            f.now = start + Duration::from_millis(n * 50);
            f.tick = if lagging { n / 4 } else { n };
            check.examine(&f, &to(0.0, 64.0, 0.0));
        }
        let after = f.now + Duration::from_millis(2000);
        for n in 0..100u64 {
            f.now = after + Duration::from_micros(n * 100);
            f.tick += 1;
            flags += usize::from(check.examine(&f, &to(0.0, 64.0, 0.0)).is_some());
        }
        flags
    };
    assert!(burst(false) > 0);
    assert!(burst(true) < burst(false));
}

#[tuff_core::watchdog]
#[test]
fn unconfirmed_teleports_with_continued_moves_are_flagged() {
    let mut check = TimerBalance::default();
    let mut f = facts();
    f.teleport_pending = true;
    let flags = (0..100)
        .filter(|_| check.examine(&f, &to(0.0, 64.0, 0.0)).is_some())
        .count();
    assert!(flags > 40);
    f.teleport_pending = false;
    assert!(check.examine(&f, &to(0.0, 64.0, 0.0)).is_none());
}

#[tuff_core::watchdog]
#[test]
fn a_near_limit_move_reports_a_weak_signal_once() {
    let mut f = facts();
    f.terrain = Some(solid_ground());
    let mut check = MoveEnvelope::default();
    assert!(check.examine(&f, &to(0.78, 64.0, 0.0)).is_none());
    assert!(check.weak() > 0.0);
    assert_eq!(check.weak(), 0.0);
    assert!(check.examine(&f, &to(0.2, 64.0, 0.0)).is_none());
    assert_eq!(check.weak(), 0.0);
}

#[tuff_core::watchdog]
#[test]
fn horizontal_speed_is_bounded_per_packet() {
    let mut f = facts();
    f.terrain = Some(solid_ground());
    assert!(
        MoveEnvelope::default()
            .examine(&f, &to(0.5, 64.0, 0.0))
            .is_none()
    );
    let flag = MoveEnvelope::default()
        .examine(&f, &to(3.0, 64.0, 0.0))
        .unwrap();
    assert_eq!(flag.fix, Fix::Teleport(f.position));
    f.speed = Some(1);
    assert!(
        MoveEnvelope::default()
            .examine(&f, &to(1.1, 64.0, 0.0))
            .is_none()
    );
    f.speed = None;
    f.velocity = true;
    assert!(
        MoveEnvelope::default()
            .examine(&f, &to(3.0, 64.0, 0.0))
            .is_none()
    );
    f.velocity = false;
    f.may_fly = true;
    assert!(
        MoveEnvelope::default()
            .examine(&f, &to(3.0, 64.0, 0.0))
            .is_none()
    );
}

#[tuff_core::watchdog]
#[test]
fn walking_into_a_solid_block_is_a_phase_but_standing_in_one_is_not() {
    let mut f = facts();
    f.terrain = Some(Terrain {
        phased: true,
        ..solid_ground()
    });
    assert!(
        MoveEnvelope::default()
            .examine(&f, &to(0.1, 64.0, 0.0))
            .is_some()
    );
    f.terrain = Some(Terrain {
        phased: true,
        was_phased: true,
        ..solid_ground()
    });
    assert!(
        MoveEnvelope::default()
            .examine(&f, &to(0.1, 64.0, 0.0))
            .is_none()
    );
}

fn jump_arc(f: &mut Facts, check: &mut AirBound, boost: u8) -> usize {
    f.terrain = Some(solid_ground());
    f.position = [0.0, 64.0, 0.0];
    f.jump_boost = boost;
    assert!(check.examine(f, &to(0.0, 64.0, 0.0)).is_none());
    f.terrain = Some(air());
    let mut vy = JUMP_VELOCITY + JUMP_BOOST_STEP * f64::from(boost);
    let mut flags = 0;
    for _ in 0..40 {
        let next = [0.0, f.position[1] + vy, 0.0];
        flags += usize::from(check.examine(f, &to(next[0], next[1], next[2])).is_some());
        f.position = next;
        vy = (vy - GRAVITY) * DRAG;
    }
    flags
}

#[tuff_core::watchdog]
#[test]
fn a_real_jump_and_fall_is_never_flagged_with_or_without_jump_boost() {
    for boost in 0..4 {
        let mut f = facts();
        let mut check = AirBound::default();
        assert_eq!(jump_arc(&mut f, &mut check, boost), 0, "boost {boost}");
    }
}

#[tuff_core::watchdog]
#[test]
fn hovering_and_climbing_in_the_air_are_flagged() {
    let mut f = facts();
    let mut check = AirBound::default();
    f.terrain = Some(air());
    let hover = (0..30)
        .filter(|_| check.examine(&f, &to(0.0, 64.0, 0.0)).is_some())
        .count();
    assert!(hover > 15);
    let mut check = AirBound::default();
    let mut y = 64.0;
    let rise = (0..30)
        .filter(|_| {
            let flag = check.examine(&f, &to(0.0, y + 0.3, 0.0));
            y += 0.3;
            f.position[1] = y;
            flag.is_some()
        })
        .count();
    assert!(rise > 20);
}

#[tuff_core::watchdog]
#[test]
fn flight_liquids_effects_and_velocity_are_exempt_from_the_air_bound() {
    let exemptions: [fn(&mut Facts); 7] = [
        |f| f.may_fly = true,
        |f| f.flying = true,
        |f| f.levitation = true,
        |f| f.slow_falling = true,
        |f| f.velocity = true,
        |f| f.game_mode = GameMode::Spectator,
        |f| {
            f.terrain = Some(Terrain {
                soft: true,
                ..air()
            })
        },
    ];
    for exempt in exemptions {
        let mut f = facts();
        f.terrain = Some(air());
        exempt(&mut f);
        let mut check = AirBound::default();
        let mut y = 64.0;
        for _ in 0..40 {
            assert!(check.examine(&f, &to(0.0, y + 0.5, 0.0)).is_none());
            y += 0.5;
            f.position[1] = y;
        }
    }
    let mut f = facts();
    let mut check = AirBound::default();
    for _ in 0..40 {
        assert!(
            check
                .examine(&f, &to(0.0, f.position[1] + 0.5, 0.0))
                .is_none()
        );
        f.position[1] += 0.5;
    }
}

#[tuff_core::watchdog]
#[test]
fn traces_show_allowed_actual_and_the_branches_taken() {
    let mut f = facts();
    f.terrain = Some(solid_ground());
    f.jump_boost = 1;
    let mut check = AirBound::default();
    assert!(check.examine(&f, &to(0.0, 64.0, 0.0)).is_none());
    assert_eq!(check.trace(), None);
    f.terrain = Some(air());
    f.push.launch = Some(0.9);
    check.examine(&f, &to(0.0, 64.6, 0.0));
    let trace = check.trace().unwrap();
    assert_eq!(trace.check, "fly");
    assert!((trace.actual - 0.6).abs() < 1e-9);
    assert!((trace.allowed - (0.9 + VERTICAL_SLACK)).abs() < 1e-9);
    for tag in [Tags::JUMP_ENVELOPE, Tags::JUMP_BOOST, Tags::PUSH] {
        assert!(trace.tags.contains(tag), "{trace}");
    }
    assert!(!trace.tags.contains(Tags::BOUNCE));

    let mut envelope = MoveEnvelope::default();
    f.grace = true;
    f.terrain = Some(Terrain {
        slippery: true,
        ..solid_ground()
    });
    assert!(envelope.examine(&f, &to(1.0, 64.0, 0.0)).is_none());
    let trace = envelope.trace().unwrap();
    assert_eq!(trace.check, "move_speed");
    assert!((trace.actual - 1.0).abs() < 1e-9);
    assert!((trace.allowed - SLIPPERY_LIMIT * GRACE_FACTOR).abs() < 1e-9);
    assert_eq!(trace.tags.to_string(), "grace,slippery");
    envelope.examine(&f, &Serverbound::ClientTickEnd);
    assert_eq!(envelope.trace(), None);
}

#[tuff_core::watchdog]
#[test]
fn a_slime_bounce_may_rise_as_high_as_the_fall_was_fast() {
    let mut f = facts();
    let mut check = AirBound::default();
    f.terrain = Some(air());
    for _ in 0..20 {
        check.examine(&f, &to(0.0, f.position[1] - 2.0, 0.0));
        f.position[1] -= 2.0;
    }
    f.terrain = Some(Terrain {
        bouncy: true,
        ..solid_ground()
    });
    assert!(check.examine(&f, &to(0.0, f.position[1], 0.0)).is_none());
    f.terrain = Some(air());
    assert!(
        check
            .examine(&f, &to(0.0, f.position[1] + 2.0, 0.0))
            .is_none()
    );
}
