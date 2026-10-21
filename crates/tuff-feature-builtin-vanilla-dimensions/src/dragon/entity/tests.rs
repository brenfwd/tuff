use rand::SeedableRng;
use rand::rngs::StdRng;

use super::*;
use tuff_core::world::EYE_HEIGHT;
use tuff_core::world::dimension::DimensionId;

/// A flat end stone island at y 60 with the podium pillar at the
/// origin topping out at y 67.
fn block(pos: BlockPos) -> Option<BlockState> {
    Some(if pos.x == 0 && pos.z == 0 && (60..=67).contains(&pos.y) {
        Block::Bedrock.default_state()
    } else if pos.y <= 60 && pos.y > 40 {
        Block::EndStone.default_state()
    } else {
        Block::Air.default_state()
    })
}

/// Runs `f` with a context over the test island, seeing `players`.
fn with_cx<R>(
    players: &[NearbyPlayer],
    rng: &mut StdRng,
    f: impl FnOnce(&mut TickContext) -> R,
) -> (R, Vec<Effect>) {
    thread_local! {
        static SHARED: &'static tuff_core::world::Shared =
            Box::leak(Box::new(tuff_test_support::bare_shared()));
    }
    let shared = SHARED.with(|s| *s);
    let light = |_: BlockPos| Some((15, 0));
    let biome = |_: BlockPos| Some("minecraft:the_end");
    let mut budget = 0;
    let mut effects = Vec::new();
    let pois = tuff_core::world::poi::PoiStore::default();
    let mut cx = TickContext {
        tick: 0,
        dimension: DimensionId::for_tests(0),
        block: &block,
        light: &light,
        sky_darken: 0,
        ambient_light: 0.0,
        monsters_burn: false,
        raining: false,
        biome: &biome,
        container: &|_| None,
        container_info: &|_| Default::default(),
        inhabited_time: &|_| 0,
        min_y: 0,
        max_y: 255,
        players,
        players_in_dimension: players.len(),
        entities: &[],
        entity_grid: None,
        pistons: &[],
        parts: &[],
        shared,
        rng,
        effects: &mut effects,
        path_budget: &mut budget,
        pois: &pois,
        state: &tuff_core::world::region_api::RegionStateLock::default(),
    };
    let result = f(&mut cx);
    (result, effects)
}

fn dragon() -> EnderDragon {
    EnderDragon::spawned(1, 7, [0.0, 128.0, 0.0], 30.0, BlockPos::new(0, 0, 0))
}

#[tuff_core::watchdog]
#[test]
fn the_head_takes_full_damage_and_other_parts_a_quarter_plus_one() {
    let mut rng = StdRng::seed_from_u64(1);
    let full = max_health();
    let mut head = dragon();
    with_cx(&[], &mut rng, |cx| {
        head.hurt_part(HEAD, 20.0, &damage_type::MOB_ATTACK, None, None, cx);
    });
    assert_eq!(head.health(), full, "a mob's attack does not hurt it");
    with_cx(&[], &mut rng, |cx| {
        head.hurt_part(HEAD, 20.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx);
    });
    assert_eq!(head.health(), full - 20.0);
    let mut body = dragon();
    with_cx(&[], &mut rng, |cx| {
        body.hurt_part(BODY, 20.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx);
    });
    assert_eq!(body.health(), full - 6.0);
    let mut bed = dragon();
    with_cx(&[], &mut rng, |cx| {
        bed.hurt_part(NECK, 30.0, &damage_type::BAD_RESPAWN_POINT, None, None, cx);
    });
    assert_eq!(bed.health(), full - 30.0, "bed explosions hurt it");
}

#[tuff_core::watchdog]
#[test]
fn a_killing_blow_in_flight_starts_the_dying_flight_but_kills_a_perched_dragon() {
    let mut rng = StdRng::seed_from_u64(2);
    let mut flying = dragon();
    let (_, effects) = with_cx(&[], &mut rng, |cx| {
        flying.hurt_part(HEAD, 500.0, &damage_type::PLAYER_ATTACK, Some(7), None, cx);
    });
    assert_eq!(flying.phase(), PhaseKind::Dying);
    assert_eq!(flying.health(), 1.0);
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            Effect::KillCredit { player: 7, kill }
                if kill.kind == EntityType::EnderDragon
        )),
        "the killing blow credits the attacking player"
    );
    let mut perched = dragon();
    with_cx(&[], &mut rng, |cx| {
        perched.set_phase(PhaseKind::SittingScanning, cx);
        perched.hurt_part(HEAD, 500.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx);
    });
    assert_eq!(perched.health(), 0.0);
    let mut ticks = 0;
    loop {
        ticks += 1;
        let (tick, _) = with_cx(&[], &mut rng, |cx| perched.tick(cx));
        if matches!(tick, Tick::Discard) {
            break;
        }
        assert!(ticks < 300, "the death animation ends");
    }
    assert_eq!(ticks, DEATH_TICKS);
}

#[tuff_core::watchdog]
#[test]
fn saved_absorption_soaks_hits_and_credits_the_player() {
    let mut rng = StdRng::seed_from_u64(4);
    let mut dragon = dragon();
    let mut nbt = Compound::new();
    nbt.insert("AbsorptionAmount", 8.0f32);
    let loaded = load(dragon.base.clone(), &mut nbt).unwrap();
    let mut saved = Compound::new();
    loaded.save(&mut saved);
    assert_eq!(take_number(&mut saved, "AbsorptionAmount"), Some(8.0));
    dragon.absorption = 8.0;
    let full = max_health();
    let (lost, effects) = with_cx(&[], &mut rng, |cx| {
        dragon.hurt_part(HEAD, 10.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx)
    });
    assert_eq!(lost, Some(2.0));
    assert_eq!(dragon.health(), full - 2.0);
    assert_eq!(dragon.absorption, 0.0);
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::TellPlayer { player: 1, event }
            if matches!(**event, tuff_core::world::messages::PlayerEvent::AwardStat(
                tuff_vanilla_data::registry::CustomStat::DamageDealtAbsorbed, 80))
    )));
}

#[tuff_core::watchdog]
#[test]
fn a_killing_blow_with_no_player_attacker_gives_no_kill_credit() {
    let mut rng = StdRng::seed_from_u64(4);
    let mut dragon = dragon();
    let (_, effects) = with_cx(&[], &mut rng, |cx| {
        dragon.hurt_part(NECK, 500.0, &damage_type::BAD_RESPAWN_POINT, None, None, cx);
    });
    assert_eq!(dragon.phase(), PhaseKind::Dying);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::KillCredit { .. })),
        "no player attacked, so no kill credit is due"
    );
}

#[tuff_core::watchdog]
#[test]
fn heavy_damage_while_perched_makes_it_take_off() {
    let mut rng = StdRng::seed_from_u64(3);
    let mut dragon = dragon();
    with_cx(&[], &mut rng, |cx| {
        dragon.set_phase(PhaseKind::SittingScanning, cx);
        dragon.hurt_part(HEAD, 40.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx);
    });
    assert!(dragon.phase().is_sitting());
    dragon.base.invulnerable_time = 0;
    with_cx(&[], &mut rng, |cx| {
        dragon.hurt_part(HEAD, 20.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx);
    });
    assert_eq!(dragon.phase(), PhaseKind::Takeoff);
}

#[tuff_core::watchdog]
#[test]
fn it_circles_the_island_lands_on_the_portal_and_takes_off_again() {
    let mut rng = StdRng::seed_from_u64(4);
    let mut dragon = dragon();
    let mut seen = Vec::new();
    let mut landed_at = None;
    for _ in 0..20_000 {
        with_cx(&[], &mut rng, |cx| dragon.tick(cx));
        let [x, y, z] = dragon.base.position;
        assert!(
            (x * x + z * z).sqrt() < 200.0 && (0.0..256.0).contains(&y),
            "strayed to {:?} in {:?}",
            dragon.base.position,
            dragon.phase()
        );
        if seen.last() != Some(&dragon.phase()) {
            seen.push(dragon.phase());
        }
        if dragon.phase() == PhaseKind::SittingScanning && landed_at.is_none() {
            landed_at = Some(dragon.base.position);
        }
        if seen.contains(&PhaseKind::Takeoff) && dragon.phase() == PhaseKind::HoldingPattern {
            break;
        }
    }
    assert!(seen.contains(&PhaseKind::LandingApproach), "{seen:?}");
    assert!(seen.contains(&PhaseKind::Landing), "{seen:?}");
    let landed = landed_at.expect("it perched");
    assert!(
        (landed[0] - 0.5).abs() < 1.0 && (landed[2] - 0.5).abs() < 1.0,
        "perched at {landed:?}"
    );
    assert!((landed[1] - 68.0).abs() < 1.0, "perched at {landed:?}");
    assert_eq!(seen.last(), Some(&PhaseKind::HoldingPattern), "{seen:?}");
}

#[tuff_core::watchdog]
#[test]
fn a_perched_dragon_roars_then_breathes_at_a_nearby_player() {
    let mut rng = StdRng::seed_from_u64(5);
    let mut dragon = dragon();
    dragon.base.position = [0.5, 68.0, 0.5];
    let player = NearbyPlayer {
        id: 50,
        uuid: 50,
        name: "runner",
        position: [8.0, 61.0, 8.0],
        eye_height: 1.62,
        yaw: 0.0,
        pitch: 0.0,
        held: [None, None],
        creative: false,
        height: 1.8,
        sneaking: false,
        invisible: false,
        invulnerable: false,
        armor: [None; 4],
        effects: 0,
        movement: [0.0; 3],
        vehicle: None,
        sleeping: None,
        last_hurt_by: None,
        last_hurt_mob: None,
        perchable: false,
        sprinting: false,
    };
    with_cx(&[player], &mut rng, |cx| {
        dragon.set_phase(PhaseKind::SittingScanning, cx);
    });
    let mut clouds = 0;
    let mut phases = Vec::new();
    for _ in 0..200 {
        let (_, effects) = with_cx(&[player], &mut rng, |cx| dragon.tick(cx));
        clouds += effects
            .iter()
            .filter(|e| matches!(e, Effect::Spawn(entity) if entity.base().kind == EntityType::AreaEffectCloud))
            .count();
        if phases.last() != Some(&dragon.phase()) {
            phases.push(dragon.phase());
        }
    }
    assert_eq!(
        &phases[..3],
        &[
            PhaseKind::SittingScanning,
            PhaseKind::SittingAttacking,
            PhaseKind::SittingFlaming
        ]
    );
    assert_eq!(clouds, 1, "one flame per breath");
}

#[tuff_core::watchdog]
#[test]
fn a_melee_hit_lands_on_each_part_at_its_own_position() {
    let mut rng = StdRng::seed_from_u64(6);
    let mut dragon = dragon();
    with_cx(&[], &mut rng, |cx| dragon.update_parts(cx));
    for part in 0..PART_COUNT {
        let before = dragon.health();
        let target = dragon.parts[part];
        let attack = EntityAttack {
            attacker: 1,
            attacker_uuid: 1,
            attacker_position: [target[0], target[1] - EYE_HEIGHT, target[2]],
            attacker_yaw: 0.0,
            attacker_pitch: 0.0,
            damage: 6.0,
            knockback: 0.0,
            weapon: None,
            critical: false,
            magic: false,
            sweep: false,
            part: Some(part),
        };
        dragon.base.invulnerable_time = 0;
        let (outcome, _) = with_cx(&[], &mut rng, |cx| dragon.attacked(&attack, cx));
        assert!(
            outcome.is_some_and(|o| o.hurt),
            "part {part} at {target:?} did not register a hit"
        );
        assert!(
            dragon.health() < before,
            "part {part} left health unchanged"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn a_melee_hit_out_of_reach_of_the_targeted_part_misses() {
    let mut rng = StdRng::seed_from_u64(60);
    let mut dragon = dragon();
    with_cx(&[], &mut rng, |cx| dragon.update_parts(cx));
    let tail = dragon.parts[5];
    let attack = EntityAttack {
        attacker: 1,
        attacker_uuid: 1,
        attacker_position: [tail[0] + 40.0, tail[1], tail[2]],
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage: 6.0,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part: Some(5),
    };
    let (outcome, _) = with_cx(&[], &mut rng, |cx| dragon.attacked(&attack, cx));
    assert!(
        outcome.is_none(),
        "an attack far outside REACH should not land"
    );
    assert_eq!(dragon.health(), max_health());
}

#[tuff_core::watchdog]
#[test]
fn a_melee_hit_knocks_the_dragon_away_from_the_attacker() {
    let mut rng = StdRng::seed_from_u64(7);
    let mut flying = dragon();
    flying.base.position = [0.0, 128.0, 0.0];
    with_cx(&[], &mut rng, |cx| flying.update_parts(cx));
    let attack = EntityAttack {
        attacker: 1,
        attacker_uuid: 1,
        attacker_position: [5.0, 129.0, 0.0],
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage: 6.0,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part: Some(BODY),
    };
    with_cx(&[], &mut rng, |cx| flying.attacked(&attack, cx));
    let v = flying.base.velocity;
    assert!(
        (-0.41..=-0.39).contains(&v[0]),
        "pushed away from the attacker along x: {v:?}"
    );
    assert!(
        v[2].abs() < 1.0e-9,
        "no z push for an attacker due east: {v:?}"
    );
    assert_eq!(
        v[1], 0.0,
        "vertical velocity is untouched while airborne: {v:?}"
    );

    // Sitting: vanilla's own knockback override refuses any push.
    let mut sitting = dragon();
    sitting.base.position = [0.0, 128.0, 0.0];
    with_cx(&[], &mut rng, |cx| {
        sitting.set_phase(PhaseKind::SittingScanning, cx);
        sitting.update_parts(cx);
        sitting.attacked(&attack, cx);
    });
    assert_eq!(
        sitting.base.velocity, [0.0; 3],
        "a perched dragon takes no knockback"
    );
}

#[tuff_core::watchdog]
#[test]
fn invulnerability_keeps_the_largest_hit_in_the_window_but_resets_after_it() {
    let mut rng = StdRng::seed_from_u64(9);
    let mut dragon = dragon();
    let full = max_health();
    with_cx(&[], &mut rng, |cx| {
        dragon.hurt_part(HEAD, 10.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx)
    });
    assert_eq!(dragon.health(), full - 10.0);
    assert_eq!(dragon.base.invulnerable_time, DAMAGE_COOLDOWN);

    // A weaker follow-up inside the same window adds nothing.
    with_cx(&[], &mut rng, |cx| {
        dragon.hurt_part(NECK, 5.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx)
    });
    assert_eq!(
        dragon.health(),
        full - 10.0,
        "a weaker hit in the same window is ignored"
    );

    // A stronger follow-up in the same window only applies the excess
    // over the largest hit already landed, and does not restart the
    // window.
    with_cx(&[], &mut rng, |cx| {
        dragon.hurt_part(HEAD, 16.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx)
    });
    assert_eq!(
        dragon.health(),
        full - 16.0,
        "only the excess above the window's largest hit lands"
    );
    assert_eq!(
        dragon.base.invulnerable_time, DAMAGE_COOLDOWN,
        "a partial hit does not extend the window"
    );

    // Once the cooldown has decayed to 10 or below, a new hit lands in
    // full and restarts the window (this cannot allow unlimited
    // simultaneous damage: only one part can land a hit per attack
    // message, and world damage is drained once per tick).
    dragon.base.invulnerable_time = 10;
    with_cx(&[], &mut rng, |cx| {
        dragon.hurt_part(HEAD, 9.0, &damage_type::PLAYER_ATTACK, Some(1), None, cx)
    });
    assert_eq!(dragon.health(), full - 16.0 - 9.0);
    assert_eq!(dragon.base.invulnerable_time, DAMAGE_COOLDOWN);
}

#[tuff_core::watchdog]
#[test]
fn the_wing_sweep_combines_its_push_with_the_hit_it_lands() {
    let mut rng = StdRng::seed_from_u64(8);
    let mut dragon = dragon();
    with_cx(&[], &mut rng, |cx| dragon.update_parts(cx));
    let wing = dragon.parts[WING_LEFT];
    let player = NearbyPlayer {
        id: 42,
        uuid: 42,
        name: "wingwalker",
        position: wing,
        eye_height: 1.62,
        yaw: 0.0,
        pitch: 0.0,
        held: [None, None],
        creative: false,
        height: 1.8,
        sneaking: false,
        invisible: false,
        invulnerable: false,
        armor: [None; 4],
        effects: 0,
        movement: [0.0; 3],
        vehicle: None,
        sleeping: None,
        last_hurt_by: None,
        last_hurt_mob: None,
        perchable: false,
        sprinting: false,
    };
    let (_, effects) = with_cx(&[player], &mut rng, |cx| dragon.sweep(cx));
    let hits: Vec<&Effect> = effects
        .iter()
        .filter(|e| matches!(e, Effect::Hurt { target, .. } if *target == 42))
        .collect();
    let pushes: Vec<&Effect> = effects
        .iter()
        .filter(|e| matches!(e, Effect::Push { target, .. } if *target == 42))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "one hit for the player caught in the wing, not two: {effects:?}"
    );
    assert!(
        pushes.is_empty(),
        "the push must not also travel as a separate SetEntityMotion, \
         or it would be silently overwritten by the hit's own \
         knockback packet: {effects:?}"
    );
    let Effect::Hurt { hit, .. } = hits[0] else {
        unreachable!()
    };
    assert!(
        hit.push.is_some(),
        "the wing's push must ride along with the hit"
    );
}

#[tuff_core::watchdog]
#[test]
fn a_sitting_dragons_wing_sweep_only_pushes_it_never_hurts() {
    let mut rng = StdRng::seed_from_u64(11);
    let mut dragon = dragon();
    with_cx(&[], &mut rng, |cx| {
        dragon.set_phase(PhaseKind::SittingScanning, cx);
        dragon.update_parts(cx);
    });
    let wing = dragon.parts[WING_RIGHT];
    let player = NearbyPlayer {
        id: 43,
        uuid: 43,
        name: "wingwalker",
        position: wing,
        eye_height: 1.62,
        yaw: 0.0,
        pitch: 0.0,
        held: [None, None],
        creative: false,
        height: 1.8,
        sneaking: false,
        invisible: false,
        invulnerable: false,
        armor: [None; 4],
        effects: 0,
        movement: [0.0; 3],
        vehicle: None,
        sleeping: None,
        last_hurt_by: None,
        last_hurt_mob: None,
        perchable: false,
        sprinting: false,
    };
    let (_, effects) = with_cx(&[player], &mut rng, |cx| dragon.sweep(cx));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Push { target, .. } if *target == 43)),
        "a sitting dragon still pushes what its wings sweep: {effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Hurt { target, .. } if *target == 43)),
        "a sitting dragon does not deal wing damage: {effects:?}"
    );
}

fn flight_digest(seed: u64, ticks: usize) -> u64 {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut dragon = dragon();
    let player = NearbyPlayer {
        id: 9,
        uuid: 9,
        name: "watcher",
        position: [30.0, 61.0, -20.0],
        eye_height: 1.62,
        yaw: 0.0,
        pitch: 0.0,
        held: [None, None],
        creative: false,
        height: 1.8,
        sneaking: false,
        invisible: false,
        invulnerable: false,
        armor: [None; 4],
        effects: 0,
        movement: [0.0; 3],
        vehicle: None,
        sleeping: None,
        last_hurt_by: None,
        last_hurt_mob: None,
        perchable: false,
        sprinting: false,
    };
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut mix = |v: u64| hash = (hash ^ v).wrapping_mul(0x0100_0000_01b3);
    for _ in 0..ticks {
        with_cx(std::slice::from_ref(&player), &mut rng, |cx| {
            dragon.tick(cx)
        });
        for axis in 0..3 {
            mix(dragon.base.position[axis].to_bits());
            mix(dragon.base.velocity[axis].to_bits());
        }
        mix(u64::from(dragon.base.yaw.to_bits()));
        mix(dragon.phase() as u64);
    }
    hash
}

#[tuff_core::watchdog]
#[test]
fn circuit_targets_cover_both_rings() {
    let mut rng = StdRng::seed_from_u64(11);
    let mut hash = 0u64;
    for current in 0..24 {
        for crystals in [false, true] {
            let (next, clockwise) =
                EnderDragon::circuit_target(current, current % 2 == 0, crystals, &mut rng);
            hash = hash
                .wrapping_mul(31)
                .wrapping_add(next as u64 * 2 + u64::from(clockwise));
        }
    }
    assert_eq!(hash, 193724161864485944, "{hash}");
}

#[tuff_core::watchdog]
#[test]
fn flight_with_a_watcher_is_deterministic() {
    let start = std::time::Instant::now();
    let digest = flight_digest(21, 6000);
    eprintln!("flight_digest took {:?}", start.elapsed());
    assert_eq!(digest, 1530888893334713293, "{digest}");
}

fn scanning_trace(target: [f64; 3], yaw: f32) -> u64 {
    let mut rng = StdRng::seed_from_u64(3);
    let mut dragon = dragon();
    dragon.base.position = [0.5, 68.0, 0.5];
    dragon.base.yaw = yaw;
    let player = NearbyPlayer {
        id: 50,
        uuid: 50,
        name: "runner",
        position: target,
        eye_height: 1.62,
        yaw: 0.0,
        pitch: 0.0,
        held: [None, None],
        creative: false,
        height: 1.8,
        sneaking: false,
        invisible: false,
        invulnerable: false,
        armor: [None; 4],
        effects: 0,
        movement: [0.0; 3],
        vehicle: None,
        sleeping: None,
        last_hurt_by: None,
        last_hurt_mob: None,
        perchable: false,
        sprinting: false,
    };
    with_cx(&[player], &mut rng, |cx| {
        dragon.set_phase(PhaseKind::SittingScanning, cx)
    });
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for _ in 0..24 {
        with_cx(&[player], &mut rng, |cx| dragon.scanning_tick(cx));
        hash = (hash ^ u64::from(dragon.base.yaw.to_bits())).wrapping_mul(0x0100_0000_01b3);
        hash = (hash ^ u64::from(dragon.yaw_speed.to_bits())).wrapping_mul(0x0100_0000_01b3);
    }
    hash = (hash ^ dragon.phase() as u64).wrapping_mul(0x0100_0000_01b3);
    hash
}

#[tuff_core::watchdog]
#[test]
fn a_scanning_dragon_turns_toward_a_player_bit_exactly() {
    let traces = [
        scanning_trace([-12.0, 61.0, 6.0], 30.0),
        scanning_trace([3.0, 61.0, 15.0], -170.0),
        scanning_trace([0.5, 61.0, -18.0], 0.0),
    ];
    assert_eq!(
        traces,
        [
            13572906705588060854,
            11735917124442365055,
            14166626914973066169
        ],
        "{traces:?}"
    );
}

#[tuff_core::watchdog]
#[test]
fn ploughing_through_blocks_breaks_them_in_order_and_pins_the_rng() {
    let mut rng = StdRng::seed_from_u64(9);
    let dragon = dragon();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for area in [
        [-1.5, 58.2, -2.1, 1.7, 61.9, 0.4],
        [3.0, 70.0, 3.0, 4.0, 71.0, 4.0],
        [-0.5, 66.0, -0.5, 0.5, 68.0, 0.5],
    ] {
        let (wall, effects) = with_cx(&[], &mut rng, |cx| dragon.check_walls(area, cx));
        hash = (hash ^ u64::from(wall)).wrapping_mul(0x0100_0000_01b3);
        for effect in &effects {
            let v = match effect {
                Effect::SetBlock { pos, .. } => {
                    ((pos.x as u64) << 40) ^ ((pos.y as u64) << 20) ^ (pos.z as u64)
                }
                Effect::ChunkPackets(_) => 7,
                _ => 1,
            };
            hash = (hash ^ v).wrapping_mul(0x0100_0000_01b3);
        }
        hash = (hash ^ rng.r#gen::<u64>()).wrapping_mul(0x0100_0000_01b3);
    }
    assert_eq!(hash, 8217266932708879297, "{hash}");
}
