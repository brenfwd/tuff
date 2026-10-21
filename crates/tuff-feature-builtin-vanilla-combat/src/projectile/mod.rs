//! Projectiles as region entities: arrows, and thrown snowballs, eggs and
//! ender pearls. They fly on their own each tick and hit what their path
//! crosses: blocks through the region's collision shapes, and the players
//! and living entities the region sees. Hits go out as `Effect::Hurt`, so
//! the victim's owner (a session or the region) applies them. Other
//! features make them through [`tuff_core::world::projectile::Launch`].

pub mod arrow;
mod bite;
pub mod bobber;
pub mod eye;
pub mod fireball;
pub mod firework;
pub mod potion;
pub mod thrown;
pub mod wind_charge;
pub mod wither_skull;

use rand::rngs::StdRng;
use tuff_vanilla_data::registry::EntityType;

use tuff_core::world::entity::RegionEntity;
use tuff_core::world::projectile::{Flight, Launch, Shot};

/// Builds the projectile another feature launches.
fn launch(launch: Launch, rng: &mut StdRng) -> Option<Box<dyn RegionEntity>> {
    let Launch {
        id,
        position,
        owner,
        shot,
    } = launch;
    Some(match shot {
        Shot::Arrow(shot) => Box::new(arrow::Arrow::shot(id, position, owner, shot)),
        Shot::Thrown {
            kind,
            item,
            velocity,
        } => Box::new(thrown::Thrown::new(
            kind, id, position, velocity, item, owner,
        )),
        Shot::Firework { item, flight } => Box::new(match flight {
            Flight::Shot(velocity) => {
                firework::FireworkRocket::shot(id, position, velocity, &item, owner, rng)
            }
            Flight::Launched => firework::FireworkRocket::launched(id, position, &item, owner, rng),
            Flight::Attached { player, lifetime } => {
                firework::FireworkRocket::attached(id, player, position, &item, owner, lifetime)
            }
        }),
        Shot::WindCharge { kind, velocity } => Box::new(if kind == EntityType::BreezeWindCharge {
            wind_charge::WindCharge::breeze(id, position, velocity, owner)
        } else {
            wind_charge::WindCharge::thrown(id, position, velocity, owner)
        }),
        Shot::SmallFireball { direction } => {
            Box::new(fireball::SmallFireball::new(id, position, direction, owner))
        }
        Shot::LargeFireball {
            direction,
            explosion_power,
        } => Box::new(fireball::LargeFireball::new(
            id,
            position,
            direction,
            owner,
            explosion_power,
        )),
        Shot::WitherSkull {
            direction,
            dangerous,
        } => Box::new(wither_skull::WitherSkull::new(
            id, position, direction, owner, dangerous,
        )),
        Shot::EyeOfEnder { item, target } => {
            Box::new(eye::EyeOfEnder::thrown(id, position, item, target, rng))
        }
        Shot::Bobber {
            rotation,
            rod,
            luck,
        } => Box::new(bobber::Bobber::cast(
            id, position, rotation, owner, rod, luck, rng,
        )),
    })
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Arrow, arrow::load);
    registry.register(EntityType::SpectralArrow, arrow::load);
    registry.register(EntityType::Trident, arrow::load);
    registry.register(EntityType::Snowball, thrown::load);
    registry.register(EntityType::Egg, thrown::load);
    registry.register(EntityType::EnderPearl, thrown::load);
    registry.register(EntityType::SplashPotion, thrown::load);
    registry.register(EntityType::LingeringPotion, thrown::load);
    registry.register(EntityType::ExperienceBottle, thrown::load);
    registry.register(EntityType::SmallFireball, fireball::load);
    registry.register(EntityType::FireworkRocket, firework::load);
    registry.register(EntityType::Fireball, fireball::load_large);
    registry.register(EntityType::EyeOfEnder, eye::load);
    registry.register(EntityType::BreezeWindCharge, wind_charge::load);
    registry.register(EntityType::WindCharge, wind_charge::load);
    registry.register(EntityType::WitherSkull, wither_skull::load);
    for kind in [
        EntityType::Arrow,
        EntityType::SpectralArrow,
        EntityType::Trident,
        EntityType::Snowball,
        EntityType::Egg,
        EntityType::EnderPearl,
        EntityType::SplashPotion,
        EntityType::LingeringPotion,
        EntityType::ExperienceBottle,
        EntityType::FireworkRocket,
        EntityType::WindCharge,
        EntityType::BreezeWindCharge,
        EntityType::SmallFireball,
        EntityType::Fireball,
        EntityType::WitherSkull,
        EntityType::EyeOfEnder,
        EntityType::FishingBobber,
    ] {
        registry.register_launcher(kind, launch);
    }
}

#[cfg(test)]
mod layout_tests {
    use tuff_vanilla_data::registry::EntityType;

    use tuff_core::protocol::play::MetadataValue;

    #[tuff_core::watchdog]
    #[test]
    fn projectile_data_indices_match_the_dumped_layouts() {
        let byte = MetadataValue::Byte(0).type_id() as u8;
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let varint = MetadataValue::VarInt(0).type_id() as u8;
        let slot = MetadataValue::Slot(None).type_id() as u8;
        assert_eq!(
            EntityType::Arrow.data_layout()[8..],
            [byte, byte, boolean, varint]
        );
        assert_eq!(
            EntityType::SpectralArrow.data_layout()[8..],
            [byte, byte, boolean]
        );
        for kind in [
            EntityType::Snowball,
            EntityType::Egg,
            EntityType::EnderPearl,
        ] {
            assert_eq!(kind.data_layout()[8..], [slot]);
        }
    }
}

#[cfg(test)]
mod launch_tests {
    use rand::SeedableRng;
    use tuff_vanilla_data::Item;
    use tuff_vanilla_data::registry::{MobEffect, SoundEvent};

    use super::*;
    use tuff_core::world::effects::ActiveEffect;
    use tuff_core::world::entity::{Culprit, Registry};
    use tuff_core::world::projectile::{ArrowShot, Owner, Pickup};
    use tuff_item::item::{ItemStack, MintReason};
    use tuff_world_model::BlockPos;

    fn registry() -> Registry {
        let mut registry = Registry::default();
        register(&mut registry);
        registry
    }

    fn launch(shot: Shot) -> Box<dyn RegionEntity> {
        let mut rng = StdRng::seed_from_u64(7);
        let owner = Owner {
            uuid: Some(42),
            ..Owner::default()
        };
        let launch = Launch {
            id: 5,
            position: [1.0, 70.0, 2.0],
            owner,
            shot,
        };
        registry()
            .launch(launch, &mut rng)
            .expect("combat launches it")
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_shot_launches_as_its_own_type() {
        let velocity = [0.0, 0.5, 1.0];
        let shots = [
            Shot::Arrow(ArrowShot::new(
                ItemStack::mint(Item::Arrow, 1, MintReason::Test),
                velocity,
            )),
            Shot::Arrow(ArrowShot::new(
                ItemStack::mint(Item::SpectralArrow, 1, MintReason::Test),
                velocity,
            )),
            Shot::Arrow(ArrowShot::new(
                ItemStack::mint(Item::Trident, 1, MintReason::Test),
                velocity,
            )),
            Shot::Thrown {
                kind: EntityType::Snowball,
                item: ItemStack::mint(Item::Snowball, 1, MintReason::Test),
                velocity,
            },
            Shot::Firework {
                item: ItemStack::mint(Item::FireworkRocket, 1, MintReason::Test),
                flight: Flight::Shot(velocity),
            },
            Shot::Firework {
                item: ItemStack::mint(Item::FireworkRocket, 1, MintReason::Test),
                flight: Flight::Launched,
            },
            Shot::WindCharge {
                kind: EntityType::BreezeWindCharge,
                velocity,
            },
            Shot::WindCharge {
                kind: EntityType::WindCharge,
                velocity,
            },
            Shot::SmallFireball {
                direction: velocity,
            },
            Shot::LargeFireball {
                direction: velocity,
                explosion_power: 2,
            },
            Shot::WitherSkull {
                direction: velocity,
                dangerous: true,
            },
            Shot::EyeOfEnder {
                item: ItemStack::mint(Item::EnderEye, 1, MintReason::Test),
                target: BlockPos::new(100, 30, 100),
            },
            Shot::Bobber {
                rotation: (0.0, 0.0),
                rod: ItemStack::mint(Item::FishingRod, 1, MintReason::Test),
                luck: 0.0,
            },
        ];
        for shot in shots {
            let kind = shot.kind();
            let entity = launch(shot);
            assert_eq!(entity.base().kind, kind);
            assert_eq!(entity.base().id, 5);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_arrow_shot_carries_what_the_shooter_set() {
        let bow = ItemStack::mint(Item::Bow, 1, MintReason::Test);
        let mut shot = ArrowShot::new(
            ItemStack::mint(Item::Arrow, 1, MintReason::Test),
            [0.0, 0.0, 2.0],
        );
        shot.pickup = Some(Pickup::CreativeOnly);
        shot.crit = true;
        shot.damage = Some(4.5);
        shot.pierce = 2;
        shot.fire_ticks = 100;
        shot.weapon = Some(bow.dup_unchecked());
        shot.sound = Some(SoundEvent::ItemCrossbowHit);
        shot.effects
            .push(ActiveEffect::timed(MobEffect::Slowness, 600, 0));
        let entity = launch(Shot::Arrow(shot));
        let arrow = entity.downcast_ref::<arrow::Arrow>().unwrap();
        assert_eq!(arrow.pickup, Pickup::CreativeOnly);
        assert!(arrow.crit);
        assert_eq!(arrow.damage, 4.5);
        assert_eq!(arrow.pierce, 2);
        assert_eq!(arrow.base.fire, 100);
        assert_eq!(arrow.weapon.as_ref().map(|w| w.item), Some(Item::Bow));
        assert_eq!(arrow.sound, SoundEvent::ItemCrossbowHit);
        assert_eq!(arrow.effects.len(), 1);
        assert_eq!(arrow.owner.uuid, Some(42));
        assert_eq!(arrow.base.velocity, [0.0, 0.0, 2.0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_player_shot_arrow_may_be_picked_up_and_a_mob_shot_one_may_not() {
        let mut rng = StdRng::seed_from_u64(1);
        let shot = |owner: Owner| Launch {
            id: 1,
            position: [0.0, 64.0, 0.0],
            owner,
            shot: Shot::Arrow(ArrowShot::new(
                ItemStack::mint(Item::Arrow, 1, MintReason::Test),
                [0.0; 3],
            )),
        };
        let registry = registry();
        let player = Owner::of(Culprit::player(3, 9, "Shooter"), None);
        let by_player = registry.launch(shot(player), &mut rng).unwrap();
        let by_mob = registry.launch(shot(Owner::default()), &mut rng).unwrap();
        let pickup = |e: &dyn RegionEntity| e.downcast_ref::<arrow::Arrow>().unwrap().pickup;
        assert_eq!(pickup(by_player.as_ref()), Pickup::Allowed);
        assert_eq!(pickup(by_mob.as_ref()), Pickup::Disallowed);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_thrown_trident_is_a_trident_and_its_own_weapon() {
        let trident = ItemStack::mint(Item::Trident, 1, MintReason::Test);
        let entity = launch(Shot::Arrow(ArrowShot::new(trident, [0.0, 0.0, 2.5])));
        let arrow = entity.downcast_ref::<arrow::Arrow>().unwrap();
        assert!(arrow.trident.is_some());
        assert_eq!(arrow.weapon.as_ref().map(|w| w.item), Some(Item::Trident));
    }
}
