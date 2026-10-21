//! Tadpoles: small bucketable swimmers that hatch from frogspawn, grow for
//! a day (slimeballs speed it up) and then turn into the frog of the biome
//! they grew up in.

use rand::rngs::StdRng;
use tuff_vanilla_data::entity::PathType;
use tuff_vanilla_data::registry::{EntityType, ParticleType, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::control::MoveStyle;
use super::fish;
use super::frog::Frog;
use super::goal::GoalSelector;
use super::goals::{LookAtPlayer, Panic, RandomStroll, Tempt};
use super::movement::{Sinking, WaterTravel};
use super::navigation::NavKind;
use super::{MobCore, MobSounds, OwnAirRules, Species, convert};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, TickContext, take_bool, take_number,
};
use tuff_core::world::messages::HandEffect;
use tuff_core::world::{framed, packets};
use tuff_nbt::Compound;

const FROM_BUCKET: u8 = 16;
const AGE_LOCKED: u8 = 17;
const AGE_LOCK_COOLDOWN: i32 = 40;
const TICKS_TO_BE_FROG: i32 = 24000;
const SWIM_SPEED: f64 = 0.5;
const SWIM_INTERVAL: i32 = 40;
const STROKE: f32 = 0.01;

fn frog_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::FROG_FOOD, item)
}

#[derive(Debug, Default)]
pub struct Tadpole {
    age: i32,
    age_locked: bool,
    lock_cooldown: i32,
}

impl Tadpole {
    pub fn age(&self) -> i32 {
        self.age
    }

    fn grow_up(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let mut frog = convert::<Frog>(core, EntityType::Frog, false, cx);
        frog.species.finalize_spawn(&mut frog.core, cx);
        frog.core.persistence_required = true;
        frog.core.changed.clear();
        core.sound_as(
            SoundEvent::EntityTadpoleGrowUp,
            play::sound_category::NEUTRAL,
            0.15,
            1.0,
            cx,
        );
        cx.effects.push(Effect::Spawn(Box::new(frog)));
        core.discarded = true;
    }

    fn feed(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let left = (TICKS_TO_BE_FROG - self.age).max(0);
        let seconds = ((left / 20) as f32 * 0.1) as i32;
        self.age += seconds * 20;
        let at = core.base.position;
        let packet = framed(&play::Particles {
            particle: ParticleType::HappyVillager.id(),
            position: [at[0], at[1] + 0.2, at[2]],
            spread: [0.0; 3],
            speed: 0.0,
            count: 1,
            alternative: false,
        });
        cx.effects.push(Effect::ChunkPackets(packets([packet])));
        if self.age >= TICKS_TO_BE_FROG {
            self.grow_up(core, cx);
        }
    }
}

impl Species for Tadpole {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.set_path_malus(PathType::Water, 0.0);
        core.navigation.kind = NavKind::WaterBound { breaching: false };
        core.move_style = MoveStyle::Fish;
        core.water_travel = WaterTravel::Swimmer {
            stroke: Some(STROKE),
            sinks: Sinking::WithoutTarget,
        };
        let mut goals = GoalSelector::default();
        goals.add(0, Panic::new(2.0));
        goals.add(1, Tempt::new(1.25, frog_food));
        goals.add(2, RandomStroll::swimming(SWIM_SPEED, SWIM_INTERVAL));
        goals.add(3, LookAtPlayer::new(6.0));
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.age = take_number(nbt, "Age").map_or(0, |a| a as i32);
        self.age_locked = take_bool(nbt, "AgeLocked").unwrap_or(false);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Age", self.age);
        out.insert("AgeLocked", self.age_locked);
        out.insert("FromBucket", true);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (FROM_BUCKET, MetadataValue::Boolean(true)),
            (AGE_LOCKED, MetadataValue::Boolean(self.age_locked)),
        ]
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        fish::flop(core, SoundEvent::EntityTadpoleFlop, cx);
        self.lock_cooldown = (self.lock_cooldown - 1).max(0);
        if self.age_locked || !core.is_alive() {
            return;
        }
        self.age += 1;
        if self.age >= TICKS_TO_BE_FROG {
            self.grow_up(core, cx);
        }
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: None,
            hurt: Some(SoundEvent::EntityTadpoleHurt),
            death: Some(SoundEvent::EntityTadpoleDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        if let Some(stack) = &interaction.stack
            && frog_food(stack.item)
            && !self.age_locked
            && core.is_alive()
        {
            self.feed(core, cx);
            return Some(HandEffect::Consume(1));
        }
        if let Some(stack) = &interaction.stack
            && stack.item == Item::GoldenDandelion
            && self.lock_cooldown == 0
            && !tuff_core::world::entity_in_tag(core.base.kind, "cannot_be_age_locked")
        {
            self.age_locked = !self.age_locked;
            self.age = 0;
            self.lock_cooldown = AGE_LOCK_COOLDOWN;
            core.changed(AGE_LOCKED);
            let sound = if self.age_locked {
                core.persistence_required = true;
                SoundEvent::ItemGoldenDandelionUse
            } else {
                SoundEvent::ItemGoldenDandelionUnuse
            };
            core.sound_as(sound, play::sound_category::PLAYERS, 1.0, 1.0, cx);
            return Some(HandEffect::Consume(1));
        }
        let (age, locked) = (self.age, self.age_locked);
        fish::bucket_interact(
            EntityType::Tadpole,
            core,
            interaction,
            SoundEvent::ItemBucketFillTadpole,
            |nbt| {
                nbt.insert("Age", age);
                nbt.insert("AgeLocked", locked);
            },
            cx,
        )
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }

    fn custom_persistence(&self, _core: &MobCore) -> bool {
        true
    }

    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(fish::fish_air_rules())
    }

    fn experience(&self, _core: &MobCore, _rng: &mut StdRng) -> i32 {
        0
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Tadpole, super::load::<Tadpole>);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn tadpole_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let layout = EntityType::Tadpole.data_layout();
        assert_eq!(layout.len(), AGE_LOCKED as usize + 1);
        assert_eq!(layout[FROM_BUCKET as usize], boolean);
        assert_eq!(layout[AGE_LOCKED as usize], boolean);
    }
}
