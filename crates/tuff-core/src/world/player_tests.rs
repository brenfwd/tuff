use super::*;

#[tuff_core::watchdog]
#[test]
fn experience_levels_cost_the_games_amounts() {
    assert_eq!(Experience::needed_at(0), 7);
    assert_eq!(Experience::needed_at(1), 9);
    assert_eq!(Experience::needed_at(14), 35);
    assert_eq!(Experience::needed_at(15), 37);
    assert_eq!(Experience::needed_at(16), 42);
    assert_eq!(Experience::needed_at(29), 107);
    assert_eq!(Experience::needed_at(30), 112);
    assert_eq!(Experience::needed_at(31), 121);
    let to_level = |level: i32| (0..level).map(Experience::needed_at).sum::<i32>();
    assert_eq!(to_level(15), 315);
    assert_eq!(to_level(30), 1395);
    assert_eq!(to_level(50), 5345);
}

#[tuff_core::watchdog]
#[test]
fn points_carry_over_levels_and_sounds_play_every_fifth_level() {
    let mut xp = Experience::default();
    assert_eq!(xp.give_points(3, 0), None);
    assert_eq!(xp.level, 0);
    assert!((xp.progress - 3.0 / 7.0).abs() < 1e-6);
    assert_eq!(xp.give_points(4, 0), None);
    assert_eq!((xp.level, xp.progress, xp.total), (1, 0.0, 7));
    assert_eq!(
        xp.give_points(1395 - 7, 10),
        Some(5.0 / 30.0 * 0.75),
        "level 5 plays, later levels are within the sound cooldown"
    );
    assert_eq!(xp.level, 30);
    assert!(xp.progress.abs() < 1e-4);
    assert_eq!(xp.total, 1395);
    assert_eq!(xp.death_reward(), 100);
    assert_eq!(xp.give_levels(5, 20), None);
    assert_eq!(xp.give_levels(5, 200), Some(0.75));
    let mut low = Experience::new(4, 0.5, 0, 0);
    assert_eq!(low.give_points(8, 0), Some(5.0 / 30.0 * 0.75));
    assert_eq!(low.level, 5);
    assert_eq!(low.death_reward(), 35);
    low.give_points(-100, 0);
    assert_eq!((low.level, low.progress, low.total), (0, 0.0, 0));
}
use tuff_vanilla_data::damage_type;

#[tuff_core::watchdog]
#[test]
fn destroying_plays_the_hit_sound_every_fourth_tick() {
    let destroying = Destroying::new(BlockPos::new(1, 2, 3), 4, 100);
    let events: Vec<_> = (101..=109).map(|tick| destroying.event(tick)).collect();
    let sound = (level_event::DESTROY_PROGRESS_SOUND, 4);
    let particles = (level_event::DESTROY_PROGRESS, 4);
    assert_eq!(
        events,
        [
            particles, particles, particles, sound, particles, particles, particles, sound,
            particles
        ]
    );
}

#[tuff_core::watchdog]
#[test]
fn destroy_progress_matches_vanilla_mining_times() {
    let progress = |block: Block, tool: Option<Item>| {
        let tool = tool.map(|item| ItemStack::new(item, 1));
        destroy_progress(
            block.default_state(),
            tool.as_ref(),
            &Miner::plain(true),
            false,
        )
    };
    for block in [
        Block::Torch,
        Block::Poppy,
        Block::LeafLitter,
        Block::ShortGrass,
    ] {
        assert!(progress(block, None) >= 1.0, "{block:?} breaks instantly");
    }
    assert!(progress(Block::OakLeaves, Some(Item::Shears)) >= 1.0);
    assert!(progress(Block::OakLeaves, None) < 1.0);
    assert!(progress(Block::Dirt, Some(Item::DiamondShovel)) < 1.0);
    assert_eq!(progress(Block::Dirt, None), 1.0 / 0.5 / 30.0);
    assert_eq!(progress(Block::Stone, None), 1.0 / 1.5 / 100.0);
    assert_eq!(
        progress(Block::Stone, Some(Item::DiamondPickaxe)),
        8.0 / 1.5 / 30.0
    );
    assert_eq!(progress(Block::Bedrock, Some(Item::NetheritePickaxe)), 0.0);

    let shears = ItemStack::new(Item::Shears, 1);
    let shears = Some(&shears);
    let leaves = Block::OakLeaves.default_state();
    assert!(destroy_progress(leaves, shears, &Miner::plain(false), false) < 1.0);
    assert!(destroy_progress(leaves, shears, &Miner::plain(true), true) < 1.0);
}

#[tuff_core::watchdog]
#[test]
fn efficiency_haste_and_fatigue_change_mining_speed() {
    let pickaxe = ItemStack::new(Item::DiamondPickaxe, 1);
    let stone = Block::Stone.default_state();
    let plain = Miner::plain(true);
    let enchanted = Miner {
        efficiency: 26.0,
        ..plain
    };
    let progress = |miner: &Miner| destroy_progress(stone, Some(&pickaxe), miner, false);
    assert_eq!(progress(&enchanted), (8.0 + 26.0) / 1.5 / 30.0);
    let hasted = Miner {
        haste: Some(1),
        ..enchanted
    };
    assert!(
        progress(&hasted) >= 1.0,
        "Efficiency V with Haste II instamines stone"
    );
    let tired = Miner {
        fatigue: Some(0),
        ..plain
    };
    assert!((progress(&tired) - 8.0 * 0.3 / 1.5 / 30.0).abs() < 1e-6);
    let dirt = Block::Dirt.default_state();
    assert_eq!(
        destroy_progress(dirt, Some(&pickaxe), &enchanted, false),
        destroy_progress(dirt, Some(&pickaxe), &plain, false),
        "Efficiency only helps where the tool is fast"
    );
    let aqua = Miner {
        submerged_speed: 1.0,
        ..plain
    };
    assert_eq!(
        destroy_progress(stone, Some(&pickaxe), &aqua, true),
        progress(&plain)
    );
}

#[tuff_core::watchdog]
#[test]
fn eye_in_water_follows_the_water_surface() {
    let water = Block::Water.default_state();
    let world = |pos: BlockPos| {
        Some(if pos.y <= 64 {
            water
        } else {
            Block::Air.default_state()
        })
    };
    assert!(eye_in_water([0.5, 64.5, 0.5], world));
    assert!(!eye_in_water([0.5, 64.95, 0.5], world));
    assert!(!eye_in_water([0.5, 65.5, 0.5], world));
    assert!(eye_in_water([0.5, 63.95, 0.5], world));
    let ceiling = |pos: BlockPos| {
        Some(match pos.y {
            ..=64 => water,
            65 => Block::Stone.default_state(),
            _ => Block::Air.default_state(),
        })
    };
    assert!(
        eye_in_water([0.5, 64.95, 0.5], ceiling),
        "a source under a solid ceiling fills the block"
    );
}

#[tuff_core::watchdog]
#[test]
fn submersion_follows_the_eyes_of_the_pose() {
    let world = |pos: BlockPos| {
        Some(match pos.y {
            ..=63 => Block::Water.default_state(),
            64 if pos.x == 1 => Block::BubbleColumn.default_state(),
            _ => Block::Air.default_state(),
        })
    };
    let standing = submersion([0.5, 63.0, 0.5], 1.62, world);
    assert_eq!(
        standing,
        Submersion {
            eyes: false,
            bubble_column: false,
            feet: true
        }
    );
    let swimming = submersion([0.5, 63.0, 0.5], 0.4, world);
    assert!(swimming.eyes && swimming.holding_breath());
    let bubbles = submersion([1.5, 63.0, 0.5], 1.62, world);
    assert!(bubbles.eyes && bubbles.bubble_column && !bubbles.holding_breath());
}

#[tuff_core::watchdog]
#[test]
fn crack_stage_scales_with_ticks_and_range_is_32_blocks() {
    assert_eq!(crack_stage(0.25, 0), 2);
    assert_eq!(crack_stage(0.25, 1), 5);
    assert_eq!(crack_stage(0.25, 3), 10);
    assert_eq!(crack_stage(0.01, 0), 0);
    let pos = BlockPos::new(0, 64, 0);
    assert!(within_crack_range(pos, [31.9, 64.0, 0.0]));
    assert!(!within_crack_range(pos, [32.0, 64.0, 0.0]));
    assert!(!within_crack_range(pos, [0.0, 64.0, -32.5]));
}

#[tuff_core::watchdog]
#[test]
fn destroying_direction_follows_the_latest_face() {
    let mut destroying = Destroying::new(BlockPos::new(0, 0, 0), 1, 0);
    assert_eq!(destroying.event(1).1, 1);
    destroying.set_face(5);
    assert_eq!(destroying.event(1).1, 5);
    destroying.set_face(-1);
    assert_eq!(destroying.event(1).1, 3);
}

#[tuff_core::watchdog]
#[test]
fn game_modes_grant_vanilla_abilities() {
    let survival = Abilities::new(GameMode::Survival, true);
    assert_eq!(survival.flags(), 0);
    assert!(survival.may_build && !survival.flying);
    assert!(!survival.is_immune_to(&damage_type::FALL));

    let adventure = Abilities::new(GameMode::Adventure, false);
    assert_eq!(adventure.flags(), 0);
    assert!(!adventure.may_build);

    let creative = Abilities::new(GameMode::Creative, false);
    assert_eq!(
        creative.flags(),
        abilities::INVULNERABLE | abilities::MAY_FLY | abilities::INSTABUILD
    );
    assert!(Abilities::new(GameMode::Creative, true).flying);
    let creative = Abilities::new(GameMode::Creative, true);
    assert!(!creative.landed(GameMode::Creative, true).flying);
    assert!(creative.landed(GameMode::Creative, false).flying);
    let spectator = Abilities::new(GameMode::Spectator, false);
    assert!(spectator.landed(GameMode::Spectator, true).flying);
    assert!(creative.is_immune_to(&damage_type::PLAYER_ATTACK));
    assert!(!creative.is_immune_to(&damage_type::OUT_OF_WORLD));

    let spectator = Abilities::new(GameMode::Spectator, false);
    assert_eq!(
        spectator.flags(),
        abilities::INVULNERABLE | abilities::FLYING | abilities::MAY_FLY
    );
    assert!(!spectator.may_build && !spectator.instabuild);
    assert!(spectator.is_immune_to(&damage_type::STARVE));
}

#[tuff_core::watchdog]
#[test]
fn eating_restores_food_and_caps_saturation() {
    let mut food = Food {
        level: 10,
        saturation: 0.0,
        exhaustion: 0.0,
        tick_timer: 0,
    };
    food.eat(4, 2.4);
    assert_eq!((food.level, food.saturation), (14, 2.4));
    food.eat(8, 12.8);
    assert_eq!(food.level, 20);
    assert!((food.saturation - 15.2).abs() < 1e-5);
    food.eat_with_modifier(2, 0.1);
    assert!((food.saturation - 15.6).abs() < 1e-5);
    assert!(!food.needs_food());
}

#[tuff_core::watchdog]
#[test]
fn absorption_soaks_damage_before_health() {
    let mut health = Health::default();
    let mut absorption = 4.0;
    assert_eq!(health.hurt_absorbing(3.0, &mut absorption), Some(3.0));
    assert_eq!((health.value, absorption), (20.0, 1.0));
    health.invulnerable_ticks = 0;
    assert_eq!(health.hurt_absorbing(3.0, &mut absorption), Some(3.0));
    assert_eq!((health.value, absorption), (18.0, 0.0));
}

#[tuff_core::watchdog]
#[test]
fn item_use_effects_start_after_a_fifth_of_the_time() {
    let mut using = UsingItem::new(false, Item::Apple, 32);
    let mut emitted = Vec::new();
    let mut finished_at = None;
    for tick in 0..40 {
        if using.emits_effects() {
            emitted.push(tick);
        }
        if using.tick() {
            finished_at = Some(tick);
            break;
        }
    }
    assert_eq!(emitted, vec![8, 12, 16, 20, 24, 28]);
    assert_eq!(finished_at, Some(31));
    let mut shield = UsingItem::new(true, Item::Shield, UsingItem::HELD);
    assert!(!shield.tick());
}

#[tuff_core::watchdog]
#[test]
fn exhaustion_drains_saturation_before_food() {
    let mut food = Food::default();
    food.add_exhaustion(4.5);
    food.tick(MAX_HEALTH, MAX_HEALTH, Difficulty::Normal, true);
    assert_eq!(food.saturation, 4.0);
    assert_eq!(food.level, 20);
    food.saturation = 0.0;
    food.add_exhaustion(4.5);
    food.tick(MAX_HEALTH, MAX_HEALTH, Difficulty::Normal, true);
    assert_eq!(food.level, 19);
}

#[tuff_core::watchdog]
#[test]
fn peaceful_never_drains_food_and_exhaustion_caps() {
    let mut food = Food {
        saturation: 0.0,
        ..Food::default()
    };
    food.add_exhaustion(100.0);
    assert_eq!(food.exhaustion, 40.0);
    food.tick(MAX_HEALTH, MAX_HEALTH, Difficulty::Peaceful, true);
    assert_eq!(food.level, 20);
}

#[tuff_core::watchdog]
#[test]
fn saturated_full_food_heals_every_ten_ticks() {
    let mut food = Food::default();
    let mut healed = 0.0;
    for _ in 0..10 {
        if let FoodEffect::Heal(amount) = food.tick(10.0, MAX_HEALTH, Difficulty::Normal, true) {
            healed += amount;
        }
    }
    assert!((healed - 5.0 / 6.0).abs() < 1e-6);
    assert_eq!(food.exhaustion, 5.0);
}

#[tuff_core::watchdog]
#[test]
fn food_keeps_its_level_while_hurt_with_natural_regeneration_off() {
    let mut food = Food {
        level: 19,
        saturation: 0.0,
        ..Food::default()
    };
    for _ in 0..400 {
        assert_eq!(
            food.tick(10.0, MAX_HEALTH, Difficulty::Normal, false),
            FoodEffect::None
        );
    }
    assert_eq!(food.level, 19);
    assert_eq!(food.exhaustion, 0.0);
}

#[tuff_core::watchdog]
#[test]
fn unsaturated_food_heals_every_eighty_ticks_until_below_eighteen() {
    let mut food = Food {
        level: 19,
        saturation: 0.0,
        ..Food::default()
    };
    let heals = (0..400)
            .filter(
                |_| matches!(food.tick(10.0, MAX_HEALTH, Difficulty::Normal, true), FoodEffect::Heal(h) if h == 1.0),
            )
            .count();
    assert_eq!(heals, 2);
    assert_eq!(food.level, 17);
}

#[tuff_core::watchdog]
#[test]
fn starvation_stops_at_one_heart_on_normal() {
    let mut food = Food {
        level: 0,
        saturation: 0.0,
        ..Food::default()
    };
    let starves = |food: &mut Food, health, difficulty| {
        (0..80).any(|_| food.tick(health, MAX_HEALTH, difficulty, true) == FoodEffect::Starve)
    };
    assert!(starves(&mut food, 5.0, Difficulty::Normal));
    assert!(!starves(&mut food, 1.0, Difficulty::Normal));
    assert!(starves(&mut food, 1.0, Difficulty::Hard));
    assert!(!starves(&mut food, 9.0, Difficulty::Easy));
}

#[tuff_core::watchdog]
#[test]
fn invulnerability_only_lets_stronger_hits_through() {
    let mut health = Health::default();
    assert_eq!(health.hurt(4.0), Some(4.0));
    assert_eq!(health.hurt(3.0), None);
    assert_eq!(health.hurt(6.0), Some(2.0));
    assert_eq!(health.value, 14.0);
    for _ in 0..10 {
        health.tick();
    }
    assert_eq!(health.hurt(1.0), Some(1.0));
}

fn on(block: Block) -> FallReport {
    FallReport {
        landed_on: Some(block.default_state()),
        ..FallReport::default()
    }
}

fn damage(report: FallReport, seq: u64, fall: &mut Fall) -> Option<f32> {
    fall.surroundings(seq, report).map(|(amount, _)| amount)
}

#[tuff_core::watchdog]
#[test]
fn a_wind_jump_counts_only_the_drop_below_the_burst() {
    let jump = |wind: bool, land: f64| {
        let mut fall = Fall::default();
        let mut y = 64.0;
        fall.step(0.0, true, 1, y);
        fall.explosion_hit(wind, y);
        for _ in 0..10 {
            y += 1.0;
            fall.tick_impulse();
            fall.step(1.0, false, 1, y);
        }
        let mut held = false;
        while y > land {
            y -= 1.0;
            fall.tick_impulse();
            held = fall.step(-1.0, y <= land, 1, y);
        }
        held
    };
    assert!(!jump(true, 64.0), "landing back on the launch block");
    assert!(jump(false, 64.0), "a plain ten block fall");
    assert!(jump(true, 59.0), "five blocks below the burst still hurt");
    let mut fall = Fall::default();
    fall.explosion_hit(true, 64.0);
    for _ in 0..IMPULSE_GRACE {
        fall.tick_impulse();
    }
    fall.step(0.0, true, 1, 64.0);
    assert_eq!(fall.impulse, None, "standing after the grace ends it");
}

#[tuff_core::watchdog]
#[test]
fn fall_damage_starts_after_three_blocks() {
    let mut fall = Fall::default();
    fall.step(-0.4, false, 1, 0.0);
    assert!(!fall.step(0.0, true, 1, 0.0));
    for _ in 0..10 {
        fall.step(-1.0, false, 2, 0.0);
    }
    assert!(fall.step(0.0, true, 2, 0.0));
    assert_eq!(fall.distance, 0.0);
    assert_eq!(damage(on(Block::Stone), 1, &mut fall), None);
    assert_eq!(damage(on(Block::Stone), 2, &mut fall), Some(7.0));
    assert_eq!(damage(on(Block::Stone), 3, &mut fall), None);
}

#[tuff_core::watchdog]
#[test]
fn a_short_fall_is_held_only_in_case_it_lands_on_a_stalagmite() {
    let mut fall = Fall::default();
    fall.step(-3.5, false, 1, 0.0);
    assert!(fall.step(0.0, true, 1, 0.0));
    assert_eq!(damage(on(Block::Stone), 1, &mut fall), None);
    fall.step(-1.0, false, 2, 0.0);
    assert!(fall.step(0.0, true, 2, 0.0));
    let tip = Block::PointedDripstone
        .default_state()
        .with("vertical_direction", "up")
        .and_then(|s| s.with("thickness", "tip"));
    let report = FallReport {
        landed_on: tip,
        ..FallReport::default()
    };
    let (amount, kind) = fall.surroundings(2, report).unwrap();
    assert_eq!(amount, 1.0);
    assert_eq!(kind.name, "minecraft:stalagmite");
}

#[tuff_core::watchdog]
#[test]
fn landing_blocks_soften_or_take_the_fall() {
    for (block, sneaking, expected) in [
        (Block::HayBlock, false, Some(2.0)),
        (Block::HoneyBlock, false, Some(2.0)),
        (Block::WhiteBed, false, Some(3.0)),
        (Block::SlimeBlock, false, None),
        (Block::SlimeBlock, true, Some(10.0)),
        (Block::PowderSnow, false, None),
    ] {
        let mut fall = Fall::default();
        fall.step(-13.0, false, 1, 0.0);
        assert!(fall.step(0.0, true, 1, 0.0));
        let report = FallReport {
            suppressing_bounce: sneaking,
            ..on(block)
        };
        assert_eq!(damage(report, 1, &mut fall), expected, "{block:?}");
    }
}

#[tuff_core::watchdog]
#[test]
fn climbing_or_cobwebs_cancel_the_fall() {
    let mut fall = Fall::default();
    fall.step(-13.0, false, 1, 0.0);
    fall.surroundings(
        1,
        FallReport {
            resets: true,
            ..FallReport::default()
        },
    );
    assert_eq!(fall.distance, 0.0);
    fall.step(-13.0, false, 2, 0.0);
    assert!(fall.step(0.0, true, 2, 0.0));
    let report = FallReport {
        resets: true,
        ..on(Block::Stone)
    };
    assert_eq!(damage(report, 2, &mut fall), None);
}

#[tuff_core::watchdog]
#[test]
fn a_landing_reports_its_fall_distance_once() {
    let mut fall = Fall::default();
    fall.step(-1.25, false, 1, 0.0);
    assert_eq!(fall.take_landed(), None);
    fall.step(0.0, true, 2, 0.0);
    assert_eq!(fall.take_landed(), Some(1.25));
    assert_eq!(fall.take_landed(), None);
    fall.step(0.0, true, 3, 0.0);
    assert_eq!(fall.take_landed(), None, "standing still is no landing");
}

#[tuff_core::watchdog]
#[test]
fn rising_resets_fall_distance() {
    let mut fall = Fall::default();
    for _ in 0..5 {
        fall.step(-1.0, false, 1, 0.0);
        fall.step(0.9, false, 1, 0.0);
    }
    assert!(!fall.step(0.0, true, 1, 0.0));
}

#[tuff_core::watchdog]
#[test]
fn water_cancels_fall_damage() {
    let water = FallReport {
        in_water: true,
        ..FallReport::default()
    };
    let mut fall = Fall::default();
    for _ in 0..10 {
        fall.step(-1.0, false, 1, 0.0);
    }
    assert!(fall.step(-1.0, true, 1, 0.0));
    assert_eq!(damage(water, 1, &mut fall), None);

    for _ in 0..10 {
        fall.step(-1.0, false, 2, 0.0);
    }
    fall.surroundings(2, water);
    assert_eq!(fall.distance, 0.0);
    for _ in 0..10 {
        fall.step(-1.0, false, 3, 0.0);
    }
    assert!(!fall.step(0.0, true, 3, 0.0));
    fall.surroundings(3, on(Block::Stone));

    for _ in 0..10 {
        fall.step(-1.0, false, 4, 0.0);
    }
    assert!(fall.step(0.0, true, 4, 0.0));
    fall.fluid_contact(true);
    assert_eq!(damage(on(Block::Stone), 4, &mut fall), None);
}

#[tuff_core::watchdog]
#[test]
fn lava_touches_only_below_its_surface() {
    let low_lava = Block::Lava.default_state().with_level(6).unwrap();
    let block = |pos: BlockPos| {
        Some(match pos.y {
            64 => low_lava,
            _ => Block::Air.default_state(),
        })
    };
    assert!(touches_lava([0.5, 64.1, 0.5], block));
    assert!(
        !touches_lava([0.5, 64.3, 0.5], block),
        "above a level 2 surface"
    );
    assert!(!touches_water([0.5, 64.1, 0.5], block));
}

#[tuff_core::watchdog]
#[test]
fn touching_water_follows_fluid_height() {
    let water = |level: &str| {
        Block::Water
            .default_state()
            .with("level", level)
            .expect("water level")
    };
    let world = |states: Vec<(BlockPos, BlockState)>| {
        move |pos: BlockPos| {
            Some(
                states
                    .iter()
                    .find(|(at, _)| *at == pos)
                    .map_or(Block::Air.default_state(), |(_, state)| *state),
            )
        }
    };
    let source = world(vec![(BlockPos::new(0, 64, 0), water("0"))]);
    assert!(touches_water([0.5, 64.0, 0.5], &source));
    assert!(touches_water([0.5, 64.88, 0.5], &source));
    assert!(!touches_water([0.5, 64.9, 0.5], &source));
    assert!(touches_water([1.29, 64.0, 0.5], &source));
    assert!(!touches_water([1.31, 64.0, 0.5], &source));
    assert!(touches_water([0.5, 62.5, 0.5], &source));

    let thin = world(vec![(BlockPos::new(0, 64, 0), water("7"))]);
    assert!(touches_water([0.5, 64.1, 0.5], &thin));
    assert!(!touches_water([0.5, 64.2, 0.5], &thin));

    let column = world(vec![
        (BlockPos::new(0, 64, 0), water("7")),
        (BlockPos::new(0, 65, 0), water("8")),
    ]);
    assert!(touches_water([0.5, 64.95, 0.5], &column));

    let kelp = world(vec![(BlockPos::new(0, 64, 0), Block::Kelp.default_state())]);
    assert!(touches_water([0.5, 64.5, 0.5], &kelp));
    let stairs = world(vec![(
        BlockPos::new(0, 64, 0),
        Block::OakStairs
            .default_state()
            .with("waterlogged", "true")
            .expect("waterlogged"),
    )]);
    assert!(touches_water([0.5, 64.5, 0.5], &stairs));
    assert!(!touches_water([0.5, 64.5, 0.5], world(vec![])));
}

#[tuff_core::watchdog]
#[test]
fn ladders_vines_and_open_trapdoors_over_ladders_are_climbable() {
    let world = |states: Vec<(BlockPos, BlockState)>| {
        move |pos: BlockPos| {
            Some(
                states
                    .iter()
                    .find(|(at, _)| *at == pos)
                    .map_or(Block::Air.default_state(), |(_, state)| *state),
            )
        }
    };
    let at = BlockPos::new(0, 64, 0);
    let ladder = Block::Ladder.default_state();
    assert!(on_climbable([0.5, 64.2, 0.5], world(vec![(at, ladder)])));
    assert!(on_climbable(
        [0.5, 64.0, 0.5],
        world(vec![(at, Block::Vine.default_state())])
    ));
    assert!(!on_climbable([0.5, 65.0, 0.5], world(vec![(at, ladder)])));
    let trapdoor = |open: &str| {
        Block::OakTrapdoor
            .default_state()
            .with("open", open)
            .unwrap()
            .with("facing", ladder.get("facing").unwrap())
            .unwrap()
    };
    let below = BlockPos::new(0, 63, 0);
    let open = world(vec![(at, trapdoor("true")), (below, ladder)]);
    assert!(on_climbable([0.5, 64.0, 0.5], open));
    let closed = world(vec![(at, trapdoor("false")), (below, ladder)]);
    assert!(!on_climbable([0.5, 64.0, 0.5], closed));
    let bare = world(vec![(at, trapdoor("true"))]);
    assert!(!on_climbable([0.5, 64.0, 0.5], bare));
}

fn elytra() -> ItemStack {
    ItemStack::new(tuff_vanilla_data::Item::Elytra, 1)
}

#[tuff_core::watchdog]
#[test]
fn gliding_only_starts_while_falling_with_a_usable_elytra_out_of_liquid() {
    let mut glide = Glide::default();
    let chest = elytra();
    assert!(Glide::can_glide(false, false, false, false, Some(&chest)));
    assert!(
        !Glide::can_glide(true, false, false, false, Some(&chest)),
        "on the ground"
    );
    assert!(
        !Glide::can_glide(false, true, false, false, Some(&chest)),
        "riding"
    );
    assert!(
        !Glide::can_glide(false, false, true, false, Some(&chest)),
        "levitating"
    );
    assert!(
        !Glide::can_glide(false, false, false, true, Some(&chest)),
        "creative flying"
    );
    assert!(
        !Glide::can_glide(false, false, false, false, None),
        "bare chest"
    );

    assert!(!glide.try_start(true, true), "starting inside a liquid");
    assert!(!glide.active);
    assert!(glide.try_start(true, false));
    assert!(glide.active);
    assert!(!glide.try_start(true, false), "already gliding");
}

#[tuff_core::watchdog]
#[test]
fn gliding_wears_the_elytra_every_twenty_ticks_and_stops_on_its_own() {
    let mut glide = Glide::default();
    glide.try_start(true, false);
    let wears = (1..=41).filter(|_| glide.tick(true)).count();
    assert_eq!(wears, 2, "one point at tick 20 and one at tick 40");
    assert!(glide.active);
    assert!(
        !glide.tick(false),
        "landing (or losing the elytra) stops it"
    );
    assert!(!glide.active);
    assert!(
        !glide.tick(true),
        "stopped gliders do not keep wearing anything"
    );
}

#[tuff_core::watchdog]
#[test]
fn glide_step_turns_a_level_dive_into_forward_speed() {
    let mut v = glide_step([0.0, 0.0, 0.0], 0.0, 0.0, 0.08);
    assert!((v[1] - (-0.01764)).abs() < 1e-9, "{v:?}");
    assert!((v[2] - 0.001782).abs() < 1e-9, "{v:?}");
    assert_eq!(v[0], 0.0);
    for _ in 0..400 {
        v = glide_step(v, 0.0, 0.0, 0.08);
    }
    assert!(
        v[2] > 0.3,
        "forward speed should build up while diving shallowly: {v:?}"
    );
    let vy_before = v[1];
    let v_next = glide_step(v, 0.0, 0.0, 0.08);
    assert!(
        (v_next[1] - vy_before).abs() < 1e-4,
        "vertical speed should have settled into a shallow, steady descent: {v:?} -> {v_next:?}"
    );
}

#[tuff_core::watchdog]
#[test]
fn glide_step_trades_vertical_speed_for_horizontal_and_back() {
    let dive = glide_step([0.0, -1.0, 1.0], 0.0, 45.0, 0.08);
    assert!(
        dive[1] > -1.0,
        "diving bleeds vertical speed into horizontal: {dive:?}"
    );
    assert!(dive[2] > 1.0, "and gains horizontal speed: {dive:?}");
    let climb = glide_step([0.0, -0.2, 1.0], 0.0, -45.0, 0.08);
    assert!(
        climb[1] > -0.2,
        "pulling up converts speed into lift: {climb:?}"
    );
    assert!(
        climb[2] < 1.0,
        "...at the cost of horizontal speed: {climb:?}"
    );
}

#[tuff_core::watchdog]
#[test]
fn firework_boost_converges_towards_one_point_seven_times_the_look_direction() {
    let look = [0.0, 0.0, 1.0];
    let mut v = [0.0, 0.0, 0.0];
    for _ in 0..40 {
        v = firework_boost(v, look);
    }
    assert!((v[2] - 1.7).abs() < 1e-6, "{v:?}");
    assert_eq!((v[0], v[1]), (0.0, 0.0));
    let one_step = firework_boost([0.0, 0.0, 0.0], look);
    assert!((one_step[2] - 0.85).abs() < 1e-12);
}
