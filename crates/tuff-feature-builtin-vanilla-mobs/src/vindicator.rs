//! Vindicators: axe-wielding melee illagers that open and break doors only
//! where a raid is on. One named "Johnny" attacks every living creature.

use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::attack::MeleeAttack;
use super::door::{BreakDoor, OpenDoor};
use super::goal::GoalSelector;
use super::goals::{AvoidPlayer, Float, LookAtPlayer, RandomLookAround, RandomStroll};
use super::navigation::NavKind;
use super::physics::block_pos;
use super::raid::Raids;
use super::target::{HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer};
use super::{MobCore, MobSounds, Species};
use super::{illager, raider};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::TickContext;
use tuff_core::world::player::Difficulty;
use tuff_item::item::{ItemStack, MintReason};

#[derive(Debug, Default)]
pub struct Vindicator;

fn is_johnny(core: &MobCore, _cx: &TickContext) -> bool {
    core.base
        .custom_name
        .as_ref()
        .and_then(tuff_item::text::TextComponent::from_nbt)
        .is_some_and(|name| name.plain() == "Johnny")
}

impl Species for Vindicator {
    fn create(_rng: &mut StdRng) -> Self {
        Self
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(1, AvoidPlayer::entities(illager::CREAKING, 8.0, 1.0, 1.2));
        goals.add(
            2,
            raider::RaidOnly::rolled(BreakDoor::on(|d| {
                matches!(d, Difficulty::Normal | Difficulty::Hard)
            })),
        );
        goals.add(3, raider::RaidOnly::new(OpenDoor::new(false)));
        goals.add(4, raider::HoldGroundAttack::new(10.0));
        goals.add(1, HurtByTarget::new(&[]).alerting(illager::RAIDERS));
        goals.add(2, NearestAttackablePlayer::new(true));
        illager::add_villager_targets(&mut goals, 3, true, true);
        goals.add(4, NearestAttackableEntity::any_living(true).when(is_johnny));
        goals.add(5, MeleeAttack::new(1.0, false));
        goals.add(8, RandomStroll::with_interval(0.6, RandomStroll::INTERVAL));
        goals.add(9, LookAtPlayer::new(3.0));
        goals.add(9, RandomLookAround::default());
        raider::add_goals(&mut goals);
        core.goals = goals;
        core.navigation.can_open_doors = true;
        raider::make_raider(core, true, SoundEvent::EntityVindicatorCelebrate);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(illager::CELEBRATING, illager::celebrating(core))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        super::monster::sounds(
            SoundEvent::EntityVindicatorAmbient,
            SoundEvent::EntityVindicatorHurt,
            SoundEvent::EntityVindicatorDeath,
            None,
        )
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.no_ai && core.navigation.kind == NavKind::Ground {
            let here = block_pos(core.base.position);
            core.navigation.can_open_doors = cx.state.get::<Raids>().at(here).is_some();
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.raider.as_ref().is_some_and(|r| r.raid.is_some()) {
            #[expect(
                clippy::disallowed_methods,
                reason = "item a mob spawns, wears or throws (finalize_spawn)"
            )]
            core.equipment.set(
                EquipmentSlot::Mainhand,
                Some(ItemStack::mint(Item::IronAxe, 1, MintReason::Spawn)),
            );
        }
        let difficulty = super::monster::LocalDifficulty::at(cx, core.base.position);
        core.populate_spawn_enchantments(difficulty, cx.rng);
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Vindicator, super::load::<Vindicator>);
    registry.register_spawn_rule(EntityType::Vindicator, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn vindicator_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let layout = EntityType::Vindicator.data_layout();
        assert_eq!(layout.len(), illager::CELEBRATING as usize + 1);
        assert_eq!(layout[illager::CELEBRATING as usize], boolean);
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_a_vindicator_named_johnny_is_johnny() {
        crate::tests::with_context(|core, cx| {
            assert!(!is_johnny(core, cx));
            core.base.custom_name = Some(tuff_nbt::Tag::String("Johnny".into()));
            assert!(is_johnny(core, cx));
            core.base.custom_name = Some(tuff_nbt::Tag::String("Jon".into()));
            assert!(!is_johnny(core, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn vindicators_path_through_doors_only_where_a_raid_is_on() {
        crate::tests::with_context(|core, cx| {
            core.navigation.can_open_doors = true;
            Vindicator.tick(core, cx);
            assert!(!core.navigation.can_open_doors);
            let here = block_pos(core.base.position);
            cx.state.get::<Raids>().start(here);
            Vindicator.tick(core, cx);
            assert!(core.navigation.can_open_doors);
        });
    }
}
