use rand::SeedableRng;
use tuff_world_model::BlockPos;

use super::*;

fn rabbit(cx: &mut TickContext) -> crate::Mob<Rabbit> {
    let base = tuff_core::world::entity::EntityBase::new(
        EntityType::Rabbit,
        5,
        5,
        [0.5, 64.0, 0.5],
        [0.0; 3],
    );
    crate::Mob::<Rabbit>::new(base, cx.rng)
}

#[tuff_core::watchdog]
#[test]
fn a_rabbit_moves_in_hops_after_a_landing_pause() {
    crate::tests::with_context(|_, cx| {
        let mut rabbit = rabbit(cx);
        rabbit.core.base.on_ground = true;
        let mut hopped_at = None;
        for step in 0..20 {
            rabbit.core.movement.set_wanted([6.5, 64.0, 0.5], 1.0);
            rabbit.species.custom_ai_step(&mut rabbit.core, cx);
            if step == 0 {
                assert_eq!(
                    rabbit.core.movement.speed_modifier, 0.0,
                    "grounded rabbits stand"
                );
            }
            if rabbit.core.jump.jump {
                hopped_at = Some(step);
                break;
            }
        }
        assert_eq!(hopped_at, Some(10), "the landing delay is ten ticks");
        assert!(
            rabbit.core.movement.speed_modifier > 0.0,
            "it moves while hopping"
        );
        assert!(
            rabbit.core.base.velocity[2] > 0.0,
            "a forward kick along its heading"
        );
        assert_eq!(
            rabbit
                .core
                .attribute(tuff_vanilla_data::registry::Attribute::JumpStrength),
            0.3
        );
    });
}

#[tuff_core::watchdog]
#[test]
fn a_killer_bunny_is_armored_hits_hard_and_is_named() {
    use tuff_vanilla_data::registry::Attribute;
    crate::tests::with_context(|_, cx| {
        let mut bunny = rabbit(cx);
        let damage = bunny.core.attribute(Attribute::AttackDamage);
        bunny.species.variant = RabbitVariant::Evil;
        bunny.species.custom_ai_step(&mut bunny.core, cx);
        assert_eq!(bunny.core.attribute(Attribute::Armor), 8.0);
        assert_eq!(bunny.core.attribute(Attribute::AttackDamage), damage + 5.0);
        assert!(bunny.core.base.custom_name.is_some());
        let mut plain = rabbit(cx);
        plain.species.custom_ai_step(&mut plain.core, cx);
        assert_eq!(plain.core.attribute(Attribute::Armor), 0.0);
        assert!(plain.core.base.custom_name.is_none());
    });
}

#[tuff_core::watchdog]
#[test]
fn a_rabbit_eats_a_stage_off_ripe_carrots_and_then_is_full() {
    use crate::goal::Goal;
    let carrots = tuff_vanilla_data::Block::Carrots
        .default_state()
        .with("age", "7")
        .expect("carrots have an age");
    let world = |pos: BlockPos| {
        Some(if pos.y < 63 {
            tuff_vanilla_data::Block::Stone.default_state()
        } else if pos.y == 63 {
            tuff_vanilla_data::Block::Farmland.default_state()
        } else if pos.y == 64 && pos.x == 0 && pos.z == 0 {
            carrots
        } else {
            tuff_vanilla_data::Block::Air.default_state()
        })
    };
    crate::tests::with_world(&world, &[], |core, cx| {
        let mut goal = super::super::rabbit::RaidGarden::new();
        core.base.position = [0.5, 64.0, 0.5];
        let mut started = false;
        for _ in 0..400 {
            if goal.can_use(core, cx) {
                started = true;
                break;
            }
        }
        assert!(started, "ripe carrots draw it in");
        goal.start(core, cx);
        for _ in 0..5 {
            goal.tick(core, cx);
        }
        assert!(cx.effects.iter().any(|e| matches!(
            e,
            Effect::SetBlock { pos, state }
                if *pos == BlockPos::new(0, 64, 0)
                    && state.get("age") == Some("6")
        )));
        assert_eq!(core.more_carrot_ticks, 40);
        assert!(!goal.can_use(core, cx), "a full rabbit leaves the carrots");
    });
}

#[tuff_core::watchdog]
#[test]
fn mooshrooms_are_always_created_red() {
    for seed in 0..256 {
        let mooshroom = Mooshroom::create(&mut StdRng::seed_from_u64(seed));
        assert_eq!(mooshroom.color, MooshroomColor::Red);
    }
}

#[tuff_core::watchdog]
#[test]
fn lambs_take_the_color_the_servers_recipes_mix() {
    let vanilla = tuff_feature_builtin_vanilla_recipes::crafting::registered::vanilla_set();
    assert_eq!(
        mixed_color(vanilla, DyeColor::Red, DyeColor::White),
        Some(DyeColor::Pink)
    );
    assert_eq!(mixed_color(vanilla, DyeColor::Red, DyeColor::Green), None);
    assert_eq!(
        mixed_color(&RecipeSet::default(), DyeColor::Red, DyeColor::White),
        None
    );
}

fn interaction(item: Item) -> EntityInteraction {
    EntityInteraction {
        player: 1,
        player_uuid: 9,
        position: [0.0; 3],
        location: [0.0; 3],
        stack: Some(ItemStack::mint(item, 5, MintReason::Test)),
        off_hand: false,
        creative: false,
        sneaking: false,
        riding: false,
        hero: None,
    }
}

#[tuff_core::watchdog]
#[test]
fn a_brown_mooshroom_eats_a_flower_and_gives_suspicious_stew() {
    crate::tests::with_context(|core, cx| {
        let mut mooshroom = Mooshroom::create(&mut StdRng::seed_from_u64(1));
        assert!(
            mooshroom
                .interact(core, &interaction(Item::Poppy), cx)
                .is_none(),
            "a red one ignores flowers"
        );
        mooshroom.color = MooshroomColor::Brown;
        let eaten = mooshroom.interact(core, &interaction(Item::Poppy), cx);
        assert!(matches!(eaten, Some(HandEffect::Consume(1))));
        let stew = mooshroom.interact(core, &interaction(Item::Bowl), cx);
        let Some(HandEffect::ExchangeStack(stew)) = stew else {
            panic!("a suspicious stew");
        };
        assert_eq!(stew.item, Item::SuspiciousStew);
        assert_eq!(
            stew.components.suspicious_stew_effects,
            Some(vec![SuspiciousEffect {
                effect: MobEffect::NightVision,
                duration: 100,
            }])
        );
        assert!(matches!(
            mooshroom.interact(core, &interaction(Item::Bowl), cx),
            Some(HandEffect::Exchange(Item::MushroomStew))
        ));
    });
}

#[tuff_core::watchdog]
#[test]
fn lightning_swaps_the_mooshroom_color_once_per_strike() {
    crate::tests::with_context(|core, cx| {
        let mut mooshroom = Mooshroom::create(&mut StdRng::seed_from_u64(1));
        mooshroom.heard(core, tuff_core::world::entity::Notice::Charged);
        mooshroom.tick(core, cx);
        assert_eq!(mooshroom.color, MooshroomColor::Brown);
        mooshroom.tick(core, cx);
        assert_eq!(mooshroom.color, MooshroomColor::Brown);
    });
}
