use rand::SeedableRng;
use rand::rngs::StdRng;
use tuff_core::world::predicate::PlayerFacts;
use tuff_item::item::Enchantments;
use tuff_vanilla_data::Item;

use super::*;

fn enchanted(item: Item, enchantments: &[(&str, i32)]) -> ItemStack {
    let mut stack = ItemStack::new(item, 1);
    stack.components.enchantments = Some(Enchantments::new(
        enchantments
            .iter()
            .map(|(id, level)| (format!("minecraft:{id}"), *level)),
    ));
    stack
}

/// A world of `floor` blocks under y 64 and air above.
struct Floor(Block);

impl PredicateWorld for Floor {
    fn location(&self, position: [f64; 3]) -> LocationFacts {
        let block = if position[1] < 64.0 {
            self.0
        } else {
            Block::Air
        };
        LocationFacts {
            position,
            block: Some(block.default_state()),
            ..LocationFacts::default()
        }
    }
    fn raining(&self) -> bool {
        false
    }
    fn thundering(&self) -> bool {
        false
    }
    fn clock(&self, _id: &str) -> Option<i64> {
        None
    }
    fn score(&self, _holder: &str, _objective: &str) -> Option<i32> {
        None
    }
    fn team(&self, _holder: &str) -> Option<&str> {
        None
    }
    fn random(&self) -> f32 {
        0.0
    }
    fn random_int(&self, min: i64, _max: i64) -> i64 {
        min
    }
}

fn player(on_ground: bool, food: i32) -> EntityFacts {
    let mut facts = facts_of(EntityType::Player, [0.5, 64.0, 0.5]);
    facts.flags.on_ground = on_ground;
    facts.movement = Some([0.0; 3]);
    facts.player = Some(PlayerFacts {
        game_mode: tuff_core::protocol::play::GameMode::Survival,
        level: 0,
        advancements: None,
        stats: None,
        input: None,
        food: Some((food, 5.0)),
    });
    facts
}

fn body(facts: &EntityFacts) -> Body {
    Body {
        kind: EntityType::Player,
        position: facts.position,
        movement: facts.movement.unwrap_or_default(),
    }
}

#[tuff_core::watchdog]
#[test]
fn damage_source_requirements_read_the_damage() {
    let requirement = Requirement {
        kind: "minecraft:damage_source_properties".into(),
        condition: serde_json::json!({
            "type": "minecraft:damage_source_properties",
            "predicate": {"is_direct": true}
        }),
    };
    let player = facts_of(EntityType::Player, [0.0; 3]);
    let damage = damage_facts(
        &damage_type::PLAYER_ATTACK,
        Some(player.clone()),
        Some(player.clone()),
    )
    .unwrap();
    let mut rng = StdRng::seed_from_u64(1);
    let mut world = EffectWorld::new(&mut rng);
    assert!(!holds(
        Some(&requirement),
        Some(&player),
        [0.0; 3],
        None,
        1,
        &world
    ));
    world.damage = Some(&damage);
    assert!(holds(
        Some(&requirement),
        Some(&player),
        [0.0; 3],
        None,
        1,
        &world
    ));
}

#[tuff_core::watchdog]
#[test]
fn soul_speed_activates_on_soul_sand_and_deactivates_off_it() {
    let boots = enchanted(Item::DiamondBoots, &[("soul_speed", 2)]);
    let worn = [(EquipmentSlot::Feet, &boots)];
    let facts = player(true, 20);
    let mut rng = StdRng::seed_from_u64(1);
    let sand = Floor(Block::SoulSand);
    let mut world = EffectWorld::new(&mut rng).with_world(&sand);
    let on = location_changed(&worn, &facts, &body(&facts), &BTreeSet::new(), &mut world);
    let speed = on
        .modifiers
        .iter()
        .find(|(attribute, ..)| *attribute == Attribute::MovementSpeed)
        .expect("a movement speed modifier");
    assert_eq!(speed.1, "minecraft:enchantment.soul_speed/feet");
    assert!((speed.2 - 0.051).abs() < 1e-6, "{}", speed.2);
    assert!(on.modifiers.iter().any(|(attribute, _, amount, _)| {
        *attribute == Attribute::MovementEfficiency && *amount == 1.0
    }));
    assert!(!on.active.is_empty());

    let mut airborne = facts.clone();
    airborne.flags.on_ground = false;
    let stone = Floor(Block::Stone);
    let mut world = EffectWorld::new(&mut rng).with_world(&stone);
    let jumping = location_changed(&worn, &airborne, &body(&airborne), &on.active, &mut world);
    assert!(
        !jumping.modifiers.is_empty(),
        "an active bonus lasts through a jump"
    );
    let off = location_changed(&worn, &facts, &body(&facts), &on.active, &mut world);
    assert!(off.modifiers.is_empty(), "standing on stone ends it");
    assert!(off.active.is_empty());
}

#[tuff_core::watchdog]
#[test]
fn frost_walker_freezes_a_disk_under_a_grounded_player() {
    let boots = enchanted(Item::DiamondBoots, &[("frost_walker", 2)]);
    let worn = [(EquipmentSlot::Feet, &boots)];
    let mut rng = StdRng::seed_from_u64(1);
    let water = Floor(Block::Water);
    let facts = player(true, 20);
    let change = {
        let mut world = EffectWorld::new(&mut rng).with_world(&water);
        location_changed(&worn, &facts, &body(&facts), &BTreeSet::new(), &mut world)
    };
    let disk = change
        .fired
        .iter()
        .flat_map(|fired| &fired.actions)
        .find_map(|action| match action {
            Action::World(WorldAction::ReplaceDisk {
                radius,
                offset,
                state,
                predicate,
                ..
            }) => Some((*radius, *offset, state.clone(), predicate.clone())),
            _ => None,
        })
        .expect("a disk");
    assert_eq!((disk.0, disk.1), (4, [0, -1, 0]));
    assert_eq!(
        provided_state(&disk.2, &mut rng),
        Block::FrostedIce.default_state().with("age", "0")
    );
    let predicate = disk.3.expect("a predicate");
    let pos = BlockPos::new(0, 63, 0);
    let open_water = |at: BlockPos| {
        Some(if at.y < 64 {
            Block::Water.default_state()
        } else {
            Block::Air.default_state()
        })
    };
    assert!(block_predicate(&predicate, pos, &open_water));
    let covered = |at: BlockPos| {
        Some(if at.y < 64 {
            Block::Water.default_state()
        } else {
            Block::Stone.default_state()
        })
    };
    assert!(!block_predicate(&predicate, pos, &covered));

    let falling = player(false, 20);
    let mut world = EffectWorld::new(&mut rng).with_world(&water);
    let change = location_changed(
        &worn,
        &falling,
        &body(&falling),
        &BTreeSet::new(),
        &mut world,
    );
    assert!(change.fired.is_empty(), "nothing freezes mid-air");
}

#[tuff_core::watchdog]
#[test]
fn lunge_pushes_tires_and_wears_unless_too_hungry() {
    let spear = enchanted(Item::IronSpear, &[("lunge", 2)]);
    let held = [(EquipmentSlot::Mainhand, &spear)];
    let mut rng = StdRng::seed_from_u64(1);
    let world = EffectWorld::new(&mut rng);
    let fed = player(true, 7);
    let fired = run(
        &held,
        |e| &e.post_piercing_attack,
        &fed,
        &body(&fed),
        &world,
    );
    let actions: Vec<&Action> = fired.iter().flat_map(|f| &f.actions).collect();
    assert!(actions.contains(&&Action::ItemDamage(1)));
    assert!(actions.contains(&&Action::Exhaustion(8.0)));
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Impulse { magnitude, .. } if (*magnitude - 0.916).abs() < 1e-6
    )));
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::World(WorldAction::Sound {
            sound: SoundEvent::ItemSpearLunge2,
            ..
        })
    )));
    let hungry = player(true, 6);
    let fired = run(
        &held,
        |e| &e.post_piercing_attack,
        &hungry,
        &body(&hungry),
        &world,
    );
    assert!(fired.is_empty());
}

#[tuff_core::watchdog]
#[test]
fn impulses_follow_the_look_direction() {
    let push = impulse([0.0, 0.0, 1.0], [1.0, 0.0, 1.0], 2.0, 0.0, 0.0);
    assert!(push[0].abs() < 1e-9 && push[1] == 0.0 && (push[2] - 2.0).abs() < 1e-9);
    let push = impulse([0.0, 0.0, 1.0], [1.0, 0.0, 1.0], 1.0, 90.0, 0.0);
    assert!((push[0] + 1.0).abs() < 1e-9 && push[2].abs() < 1e-9);
}

#[tuff_core::watchdog]
#[test]
fn flame_sets_spawned_arrows_alight() {
    let bow = enchanted(Item::Bow, &[("flame", 1)]);
    let mut rng = StdRng::seed_from_u64(1);
    let spawned = projectile_spawned(
        &bow,
        EntityType::Arrow,
        [0.0; 3],
        [0.0, 0.0, 1.0],
        (0.0, 0.0),
        &mut rng,
    );
    assert_eq!(spawned.fire, Some(100.0));
    let plain = ItemStack::new(Item::Bow, 1);
    let spawned = projectile_spawned(
        &plain,
        EntityType::Arrow,
        [0.0; 3],
        [0.0; 3],
        (0.0, 0.0),
        &mut rng,
    );
    assert_eq!(spawned, Spawned::default());
}

#[tuff_core::watchdog]
#[test]
fn soul_speed_ticks_spawn_particles_while_moving_on_soul_blocks() {
    let boots = enchanted(Item::DiamondBoots, &[("soul_speed", 1)]);
    let worn = [(EquipmentSlot::Feet, &boots)];
    let mut rng = StdRng::seed_from_u64(1);
    let sand = Floor(Block::SoulSand);
    let world = EffectWorld::new(&mut rng).with_world(&sand);
    let mut facts = player(true, 20);
    facts.tick_count = Some(10);
    facts.movement = Some([0.1, 0.0, 0.0]);
    let fired = run(&worn, |e| &e.tick, &facts, &body(&facts), &world);
    assert!(fired.iter().flat_map(|f| &f.actions).any(|a| matches!(
        a,
        Action::World(WorldAction::Particles {
            particle: ParticleType::Soul,
            ..
        })
    )));
    facts.tick_count = Some(11);
    let fired = run(&worn, |e| &e.tick, &facts, &body(&facts), &world);
    assert!(fired.is_empty(), "only every fifth tick");
}
