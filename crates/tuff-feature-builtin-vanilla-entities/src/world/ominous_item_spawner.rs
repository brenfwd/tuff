//! Ominous item spawner: a short-lived, invulnerable, non-physical entity
//! that, after a delay, drops its item or, for a projectile item (arrows,
//! thrown potions, fire charges), shoots it straight down. Trial spawners
//! in ominous mode create them above players and their mobs.

use rand::Rng;
use tuff_core::world::projectile::{self, ArrowShot, Launch, Pickup, Shot};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use tuff_core::protocol::play::{MetadataValue, metadata};
use tuff_core::world::entity::{
    Effect, EntityBase, RegionEntity, Tick, TickContext, take, take_number,
};
use tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY;
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;

const ABOUT_TO_SPAWN_TICKS: i64 = 36;
const SHOT_POWER: f32 = 1.1;
const SHOT_UNCERTAINTY: f32 = 6.0;
const POTION_POWER_SCALE: f32 = 1.25;
const POTION_UNCERTAINTY_SCALE: f32 = 0.5;
const DISPENSE_SPREAD: f64 = 0.114_85;
const SPAWN_ITEM_EVENT: i32 = 3021;
const BLAZE_FIREBALL_EVENT: i32 = 1018;
const WIND_CHARGE_SHOOT_EVENT: i32 = 1051;

#[derive(Debug)]
pub struct OminousItemSpawner {
    pub base: EntityBase,
    pub item: Option<ItemStack>,
    pub spawn_after_ticks: i64,
    ticks: i64,
}

impl OminousItemSpawner {
    pub fn new(
        id: i32,
        uuid: u128,
        position: [f64; 3],
        item: ItemStack,
        spawn_after_ticks: i64,
    ) -> Self {
        let mut base =
            EntityBase::new(EntityType::OminousItemSpawner, id, uuid, position, [0.0; 3]);
        base.no_gravity = true;
        Self {
            base,
            item: Some(item),
            spawn_after_ticks,
            ticks: 0,
        }
    }
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let item = take(nbt, "item", |tag| {
        ItemStack::from_storage(tag.as_compound()?)
    });
    let spawn_after_ticks = take_number(nbt, "spawn_item_after_ticks").unwrap_or(0.0) as i64;
    Some(Box::new(OminousItemSpawner {
        base,
        item,
        spawn_after_ticks,
        ticks: 0,
    }))
}

impl RegionEntity for OminousItemSpawner {
    fn take_items(&mut self) -> Vec<ItemStack> {
        self.item.take().into_iter().collect()
    }

    fn ignores_pistons(&self) -> bool {
        true
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        if let Some(item) = &self.item {
            out.insert("item", item.to_storage());
        }
        out.insert("spawn_item_after_ticks", self.spawn_after_ticks);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(
            metadata::ITEM_STACK,
            MetadataValue::Slot(self.item.as_ref().map(tuff_item::item::ItemSnapshot::of)),
        )]
    }

    fn display_name(&self) -> String {
        "minecraft:ominous_item_spawner".to_owned()
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.ticks += 1;
        if self.ticks == self.spawn_after_ticks - ABOUT_TO_SPAWN_TICKS {
            cx.effects
                .push(Effect::EntityPackets(tuff_core::world::packets([
                    tuff_core::world::framed(&tuff_core::protocol::play::Sound {
                        sound_event: SoundEvent::BlockTrialSpawnerAboutToSpawnItem.id(),
                        category: tuff_core::protocol::play::sound_category::NEUTRAL,
                        position: self.base.position,
                        volume: 1.0,
                        pitch: 1.0,
                        seed: cx.rng.r#gen(),
                    }),
                ])));
        }
        if self.ticks < self.spawn_after_ticks {
            return Tick::Alive;
        }
        if let Some(item) = self.item.take() {
            self.release(item, cx);
        }
        Tick::Discard
    }
}

impl OminousItemSpawner {
    fn release(&self, item: ItemStack, cx: &mut TickContext) {
        let position = self.base.position;
        let down = [0.0, -1.0, 0.0];
        let id = cx.shared.next_entity_id();
        let block = tuff_world_model::BlockPos::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        );
        let level_event = |event: i32, data: i32| {
            Effect::ChunkPackets(tuff_core::world::packets([tuff_core::world::framed(
                &tuff_core::protocol::play::LevelEvent {
                    event,
                    position: block,
                    data,
                    global: false,
                },
            )]))
        };
        match item.item() {
            Item::FireCharge => cx.effects.push(level_event(BLAZE_FIREBALL_EVENT, 0)),
            Item::WindCharge => cx.effects.push(level_event(WIND_CHARGE_SHOOT_EVENT, 0)),
            _ => {}
        }
        let owner =
            projectile::Owner::of(tuff_core::world::entity::Culprit::entity(&self.base), None);
        let shot = match item.item() {
            Item::Arrow | Item::TippedArrow | Item::SpectralArrow => {
                let velocity =
                    projectile::shot_velocity(down, SHOT_POWER, SHOT_UNCERTAINTY, cx.rng);
                let mut arrow = ArrowShot::new(
                    ItemStack::mint_like(&item, 1, tuff_item::item::MintReason::Transform),
                    velocity,
                );
                arrow.pickup = Some(Pickup::Allowed);
                Some(Shot::Arrow(arrow))
            }
            Item::WindCharge => Some(Shot::WindCharge {
                kind: EntityType::WindCharge,
                velocity: down
                    .map(|c| tuff_core::world::physics::triangle(cx.rng, c, DISPENSE_SPREAD)),
            }),
            Item::FireCharge => Some(Shot::SmallFireball {
                direction: down
                    .map(|c| tuff_core::world::physics::triangle(cx.rng, c, DISPENSE_SPREAD)),
            }),
            held => projectile::thrown_kind(held)
                .filter(|_| held != Item::EnderPearl)
                .map(|kind| {
                    let (power, uncertainty) = match held {
                        Item::SplashPotion | Item::LingeringPotion | Item::ExperienceBottle => (
                            SHOT_POWER * POTION_POWER_SCALE,
                            SHOT_UNCERTAINTY * POTION_UNCERTAINTY_SCALE,
                        ),
                        _ => (SHOT_POWER, SHOT_UNCERTAINTY),
                    };
                    Shot::Thrown {
                        kind,
                        item: ItemStack::mint_like(
                            &item,
                            item.count(),
                            tuff_item::item::MintReason::Transform,
                        ),
                        velocity: projectile::shot_velocity(down, power, uncertainty, cx.rng),
                    }
                }),
        };
        let launched = shot.and_then(|shot| {
            let launch = Launch {
                id,
                position,
                owner: owner.clone(),
                shot,
            };
            cx.shared.entity_types.launch(launch, cx.rng)
        });
        match launched {
            Some(entity) => {
                item.burn(tuff_item::item::BurnReason::Transform);
                cx.effects.push(Effect::Spawn(entity));
            }
            None => cx.effects.push(Effect::DropItem {
                stack: item,
                position,
                velocity: [0.0; 3],
                pickup_delay: BLOCK_DROP_PICKUP_DELAY,
            }),
        }
        cx.effects.push(level_event(SPAWN_ITEM_EVENT, 1));
        cx.effects.push(Effect::GameEvent {
            event: tuff_core::world::game_event::GameEvent::EntityPlace,
            position,
            source: Some(id),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_feature_builtin_vanilla_combat::projectile::wind_charge::WindCharge;
    use tuff_item::item::MintReason;
    use tuff_vanilla_data::Item;

    #[tuff_core::watchdog]
    #[test]
    fn drops_its_item_once_the_delay_elapses() {
        let mut spawner = OminousItemSpawner::new(
            1,
            1,
            [0.5, 64.0, 0.5],
            ItemStack::mint(Item::Diamond, 1, MintReason::Test),
            5,
        );
        tuff_test_support::with_tick_context(|cx| {
            for _ in 0..4 {
                let tick = spawner.tick(cx);
                assert!(matches!(tick, Tick::Alive));
                assert!(cx.effects.is_empty());
            }
            let tick = spawner.tick(cx);
            assert!(matches!(tick, Tick::Discard));
            assert!(
                cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::DropItem { .. }))
            );
            assert!(spawner.item.is_none());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fire_charge_goes_off_with_the_blaze_sound_and_a_placed_event() {
        let mut spawner = OminousItemSpawner::new(
            1,
            1,
            [0.5, 64.0, 0.5],
            ItemStack::mint(Item::FireCharge, 1, MintReason::Test),
            1,
        );
        tuff_test_support::with_tick_context(|cx| {
            assert!(matches!(spawner.tick(cx), Tick::Discard));
            let events: Vec<i32> = cx
                .effects
                .iter()
                .filter_map(|e| match e {
                    Effect::ChunkPackets(p) => p.first().cloned(),
                    _ => None,
                })
                .filter_map(|frame| {
                    let mut r = tuff_test_support::framed_payload(&frame);
                    r.read_var_int().ok()?;
                    r.read_int().ok()
                })
                .collect();
            assert_eq!(events, vec![BLAZE_FIREBALL_EVENT, SPAWN_ITEM_EVENT]);
            assert!(cx.effects.iter().any(|e| matches!(
                e,
                Effect::GameEvent {
                    event: tuff_core::world::game_event::GameEvent::EntityPlace,
                    ..
                }
            )));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_spectral_arrow_is_shot_as_a_spectral_arrow() {
        let mut spawner = OminousItemSpawner::new(
            1,
            1,
            [0.5, 64.0, 0.5],
            ItemStack::mint(Item::SpectralArrow, 1, MintReason::Test),
            1,
        );
        tuff_test_support::with_tick_context(|cx| {
            assert!(matches!(spawner.tick(cx), Tick::Discard));
            assert!(cx.effects.iter().any(|e| matches!(
                e,
                Effect::Spawn(shot) if shot.base().kind == EntityType::SpectralArrow
            )));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn shoots_arrows_down_instead_of_dropping_them() {
        let mut spawner = OminousItemSpawner::new(
            1,
            1,
            [0.5, 64.0, 0.5],
            ItemStack::mint(Item::Arrow, 1, MintReason::Test),
            1,
        );
        tuff_test_support::with_tick_context(|cx| {
            assert!(matches!(spawner.tick(cx), Tick::Discard));
            assert!(
                cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::Spawn(shot) if shot.base().velocity[1] < 0.0))
            );
            assert!(
                !cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::DropItem { .. }))
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_wind_charge_is_shot_down_owned_by_the_spawner() {
        let mut spawner = OminousItemSpawner::new(
            1,
            9,
            [0.5, 64.0, 0.5],
            ItemStack::mint(Item::WindCharge, 1, MintReason::Test),
            1,
        );
        tuff_test_support::with_tick_context(|cx| {
            assert!(matches!(spawner.tick(cx), Tick::Discard));
            assert!(
                !cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::DropItem { .. })),
                "not dropped as an item"
            );
            let charge = cx
                .effects
                .iter()
                .find_map(|e| match e {
                    Effect::Spawn(entity) => entity.downcast_ref::<WindCharge>(),
                    _ => None,
                })
                .expect("a wind charge");
            assert_eq!(charge.base.kind, EntityType::WindCharge);
            assert!(charge.base.velocity[1] < 0.0, "heading down");
            assert_eq!(charge.owner.uuid, Some(9));
        });
    }
}
