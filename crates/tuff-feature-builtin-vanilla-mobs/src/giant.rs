//! The giant: a huge, goal-less monster that only stands where it is put.

use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::{MobCore, MobSounds, Species};

#[derive(Debug, Default)]
pub struct Giant;

impl Species for Giant {
    fn create(_rng: &mut StdRng) -> Self {
        Self
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: None,
            hurt: Some(SoundEvent::EntityHostileHurt),
            death: Some(SoundEvent::EntityHostileDeath),
            step: None,
            volume: 1.0,
        }
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Giant, super::load::<Giant>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::registry::Attribute;

    #[tuff_core::watchdog]
    #[test]
    fn giants_have_their_dumped_attributes_and_no_goals() {
        let layout = EntityType::Giant.data_layout();
        assert_eq!(layout.len(), super::super::data::MOB_FLAGS as usize + 1);
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        let base = tuff_core::world::entity::EntityBase::new(
            EntityType::Giant,
            1,
            1,
            [0.5, 64.0, 0.5],
            [0.0; 3],
        );
        let mob = super::super::Mob::<Giant>::new(base, &mut rng);
        assert_eq!(mob.core.max_health(), 100.0);
        assert_eq!(mob.core.attribute(Attribute::AttackDamage), 50.0);
        assert_eq!(mob.core.attribute(Attribute::MovementSpeed), 0.5);
    }
}
