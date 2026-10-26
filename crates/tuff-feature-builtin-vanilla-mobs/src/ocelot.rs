//! Ocelots: wild jungle cats that come to trust a player who feeds them
//! fish (no ownership, unlike wolves/cats -- see `wolf.rs`). They flee
//! players until trusting and hunt chickens and baby turtles.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::attack::LeapAtTarget;
use super::goal::GoalSelector;
use super::goal::{Flags, Goal};
use super::goals::{
    AvoidPlayer, Breed, Float, LookAtPlayer, RandomLookAround, RandomStroll, Tempt,
};
use super::target::NearestAttackableEntity;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{EntityInteraction, InteractOutcome, TickContext, take_bool};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;

/// Broadcast the same way a tameable animal's taming roll is (smoke on
/// failure, hearts on success), but ocelots use their own event ids.
/// Wild cats and ocelots only despawn once this many ticks old.
pub const WILD_DESPAWN_AGE: u64 = 2400;
const TRUST_FAILED: i8 = 40;
const TRUST_SUCCEEDED: i8 = 41;
const TRUST_CHANCE: f64 = 1.0 / 3.0;

fn ocelot_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::OCELOT_FOOD, item)
}

/// Closes in on the target at a speed that depends on how far it is and
/// bites once every second.
#[derive(Debug, Default)]
struct OcelotAttack {
    attack_time: i32,
}

impl Goal for OcelotAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx)
            .is_some_and(|t| t.alive && mob.distance_sq(t.position) <= 225.0)
            && (!mob.navigation.is_done() || self.can_use(mob, cx))
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        let width = f64::from(mob.size.width);
        let melee = width * 2.0 * width * 2.0;
        let distance = mob.distance_sq(target.position);
        let speed = if distance > melee && distance < 16.0 {
            1.33
        } else if distance < 225.0 {
            0.6
        } else {
            0.8
        };
        mob.move_to_entity_reach(target.position, 0, speed, cx);
        self.attack_time = (self.attack_time - 1).max(0);
        if distance > melee || self.attack_time > 0 {
            return;
        }
        self.attack_time = 20;
        mob.swing(cx);
        mob.hurt_target(&target, cx);
    }
}

#[derive(Debug, Default)]
pub struct Ocelot {
    trusting: bool,
}

impl Ocelot {
    fn sync_avoid(&self, core: &mut MobCore) {
        if let Some(avoid) = core.goals.find_mut::<AvoidPlayer>() {
            avoid.set_disabled(self.trusting);
        }
    }
}

impl Species for Ocelot {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        goals.add(1, Float::new(core));
        goals.add(3, Tempt::new(0.6, ocelot_food));
        goals.add(4, AvoidPlayer::new(16.0, 0.8, 1.33));
        goals.add(7, LeapAtTarget::new(0.3));
        goals.add(8, OcelotAttack::default());
        goals.add(9, Breed::new(0.8));
        goals.add(10, RandomStroll::water_avoiding(0.8));
        goals.add(11, LookAtPlayer::new(10.0));
        goals.add(12, RandomLookAround::default());
        goals.add(
            1,
            NearestAttackableEntity::new(&[EntityType::Chicken], false),
        );
        goals.add(
            1,
            NearestAttackableEntity::new(&[EntityType::Turtle], false)
                .only(super::zombie::baby_on_land),
        );
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.trusting = take_bool(nbt, "Trusting").unwrap_or(false);
        self.sync_avoid(core);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Trusting", self.trusting);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(data::SPECIES, MetadataValue::Boolean(self.trusting))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityOcelotAmbient),
            hurt: Some(SoundEvent::EntityOcelotHurt),
            death: Some(SoundEvent::EntityOcelotDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn is_food<M: tuff_item::item::Mode>(&self, stack: &ItemStack<M>) -> bool {
        ocelot_food(stack.item())
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        if self.trusting {
            return None;
        }
        let stack = interaction.stack.as_deref()?;
        if !ocelot_food(stack.item()) {
            return None;
        }
        if cx.rng.gen_bool(TRUST_CHANCE) {
            self.trusting = true;
            self.sync_avoid(core);
            core.persistence_required = true;
            core.changed(data::SPECIES);
            core.entity_event(TRUST_SUCCEEDED, cx);
        } else {
            core.entity_event(TRUST_FAILED, cx);
        }
        Some(HandEffect::Consume(1))
    }

    fn offspring(&self, _partner: &Self, _rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self, core: &MobCore) -> bool {
        !self.trusting && core.tick_count > WILD_DESPAWN_AGE
    }
}

/// Registers the ocelot's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Ocelot, super::load::<Ocelot>);
    registry.register_spawn_rule(EntityType::Ocelot, super::spawning::animal_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;

    fn interaction(item: Option<Item>) -> EntityInteraction {
        EntityInteraction {
            player: 1,
            player_uuid: 1,
            position: [0.0; 3],
            location: [0.0; 3],
            stack: item.map(|item| tuff_item::item::ItemSnapshot::new(item, 1)),
            hand: tuff_core::world::entity::HandOffer::new(
                (item.map(|item| tuff_item::item::ItemSnapshot::new(item, 1)))
                    .as_deref()
                    .map(tuff_item::item::ItemStack::test_copy),
            ),
            off_hand: false,
            creative: false,
            sneaking: false,
            riding: false,
            hero: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn ocelot_data_index_matches_the_dumped_layout() {
        let layout = EntityType::Ocelot.data_layout();
        assert_eq!(layout.len(), data::SPECIES as usize + 1);
        assert_eq!(
            layout[data::SPECIES as usize],
            MetadataValue::Boolean(false).type_id() as u8
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn fish_eventually_earns_trust_but_only_fish() {
        with_context(|core, cx| {
            Ocelot::configure(core);
            let mut ocelot = Ocelot::create(cx.rng);
            assert!(!core.goals.find_mut::<AvoidPlayer>().unwrap().is_disabled());
            assert!(core.goals.find_mut::<OcelotAttack>().is_some());

            let effect = ocelot.interact(core, &interaction(Some(Item::Bone)), cx);
            assert!(effect.is_none(), "bones are not ocelot food");
            assert!(!ocelot.trusting);

            let mut trusted = false;
            for _ in 0..50 {
                let effect = ocelot.interact(core, &interaction(Some(Item::Cod)), cx);
                assert!(matches!(effect, Some(HandEffect::Consume(1))));
                if ocelot.trusting {
                    trusted = true;
                    break;
                }
            }
            assert!(trusted, "1/3 chance should succeed within 50 attempts");
            assert!(core.goals.find_mut::<AvoidPlayer>().unwrap().is_disabled());
            assert!(!ocelot.remove_when_far_away(core));
        });
    }
}
