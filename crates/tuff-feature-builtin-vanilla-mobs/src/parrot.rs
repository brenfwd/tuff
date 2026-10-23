//! Parrots: tamed with seeds, fly around, sit for their owner, and die if
//! fed a cookie, land on their owner's shoulder, and imitate the sounds of
//! mobs nearby. Cannot breed.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::control::MoveStyle;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{FollowMob, FollowOwner, LookAtPlayer, RandomLookAround, RandomStroll, Sit};
use super::navigation::NavKind;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{Effect, EntityInteraction, InteractOutcome, TickContext, take};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};

const VARIANT: u8 = data::SPECIES + 2;
const TAME_CHANCE: f64 = 0.1;
const VARIANT_COUNT: i32 = 5;
const POISON_TICKS: i64 = 900;
/// Ticks a parrot must have lived (since it spawned or got off a
/// shoulder) before it may land on one.
const RIDE_COOLDOWN: u64 = 100;

/// A tamed parrot touching its owner asks to ride the owner's shoulder;
/// the owner's session decides whether there is room.
#[derive(Debug, Default)]
pub struct LandOnOwnersShoulder;

impl Goal for LandOnOwnersShoulder {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(tame) = &mob.tame else { return false };
        let Some(owner) = tame.owner else {
            return false;
        };
        !tame.sitting
            && mob.tick_count > RIDE_COOLDOWN
            && cx.players.iter().any(|p| p.uuid == owner && p.perchable)
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(tame) = &mob.tame else { return };
        if tame.sitting || mob.leash.is_some() || !mob.is_alive() {
            return;
        }
        let Some(owner) = tame.owner else { return };
        let Some(player) = cx.players.iter().find(|p| p.uuid == owner) else {
            return;
        };
        if tuff_core::world::physics::intersects(mob.bounds(), player.bounds()) {
            cx.effects.push(Effect::SitOnShoulder { player: player.id });
        }
    }
}

const IMITATE_RANGE: f64 = 20.0;
const IMITATE_ODDS: i32 = 400;

/// The sound a parrot makes for a mob of this kind, if it imitates it.
/// A happy ghast is imitated silently.
fn imitation(kind: EntityType) -> Option<Option<SoundEvent>> {
    use SoundEvent as S;
    Some(Some(match kind {
        EntityType::Blaze => S::EntityParrotImitateBlaze,
        EntityType::Bogged => S::EntityParrotImitateBogged,
        EntityType::Breeze => S::EntityParrotImitateBreeze,
        EntityType::CamelHusk => S::EntityParrotImitateCamelHusk,
        EntityType::CaveSpider | EntityType::Spider => S::EntityParrotImitateSpider,
        EntityType::Creaking => S::EntityParrotImitateCreaking,
        EntityType::Creeper => S::EntityParrotImitateCreeper,
        EntityType::Drowned => S::EntityParrotImitateDrowned,
        EntityType::ElderGuardian => S::EntityParrotImitateElderGuardian,
        EntityType::EnderDragon => S::EntityParrotImitateEnderDragon,
        EntityType::Endermite => S::EntityParrotImitateEndermite,
        EntityType::Evoker => S::EntityParrotImitateEvoker,
        EntityType::Ghast => S::EntityParrotImitateGhast,
        EntityType::HappyGhast => return Some(None),
        EntityType::Guardian => S::EntityParrotImitateGuardian,
        EntityType::Hoglin => S::EntityParrotImitateHoglin,
        EntityType::Husk => S::EntityParrotImitateHusk,
        EntityType::Illusioner => S::EntityParrotImitateIllusioner,
        EntityType::MagmaCube => S::EntityParrotImitateMagmaCube,
        EntityType::Parched => S::EntityParrotImitateParched,
        EntityType::Phantom => S::EntityParrotImitatePhantom,
        EntityType::Piglin => S::EntityParrotImitatePiglin,
        EntityType::PiglinBrute => S::EntityParrotImitatePiglinBrute,
        EntityType::Pillager => S::EntityParrotImitatePillager,
        EntityType::Ravager => S::EntityParrotImitateRavager,
        EntityType::Shulker => S::EntityParrotImitateShulker,
        EntityType::Silverfish => S::EntityParrotImitateSilverfish,
        EntityType::Skeleton => S::EntityParrotImitateSkeleton,
        EntityType::Slime => S::EntityParrotImitateSlime,
        EntityType::Stray => S::EntityParrotImitateStray,
        EntityType::Vex => S::EntityParrotImitateVex,
        EntityType::Vindicator => S::EntityParrotImitateVindicator,
        EntityType::Warden => S::EntityParrotImitateWarden,
        EntityType::Witch => S::EntityParrotImitateWitch,
        EntityType::Wither => S::EntityParrotImitateWither,
        EntityType::WitherSkeleton => S::EntityParrotImitateWitherSkeleton,
        EntityType::Zoglin => S::EntityParrotImitateZoglin,
        EntityType::Zombie => S::EntityParrotImitateZombie,
        EntityType::ZombieHorse => S::EntityParrotImitateZombieHorse,
        EntityType::ZombieNautilus => S::EntityParrotImitateZombieNautilus,
        EntityType::ZombieVillager => S::EntityParrotImitateZombieVillager,
        _ => return None,
    }))
}

/// Half the time, the sound of a random imitable mob within 20 blocks.
fn imitate_nearby(core: &MobCore, cx: &mut TickContext) -> Option<SoundEvent> {
    if !core.is_alive() || core.base.silent || cx.rng.gen_range(0..2) != 0 {
        return None;
    }
    let area = tuff_core::world::physics::inflate(core.bounds(), [IMITATE_RANGE; 3]);
    let heard: Vec<Option<SoundEvent>> = cx
        .entities
        .iter()
        .filter(|e| tuff_core::world::physics::intersects(e.bounds, area))
        .filter_map(|e| imitation(e.kind))
        .collect();
    if heard.is_empty() {
        return None;
    }
    heard[cx.rng.gen_range(0..heard.len())]
}

fn parrot_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::PARROT_FOOD, item)
}

fn poisonous(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::PARROT_POISONOUS_FOOD, item)
}

#[derive(Debug)]
pub struct Parrot {
    variant: i32,
}

impl Species for Parrot {
    fn create(rng: &mut StdRng) -> Self {
        Self {
            variant: rng.gen_range(0..VARIANT_COUNT),
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.make_tameable(data::SPECIES);
        core.omnidirectional_air = true;
        core.navigation.kind = NavKind::Fly;
        core.move_style = MoveStyle::Flying {
            hovers: false,
            max_turn: 10.0,
        };
        let mut goals = GoalSelector::default();
        goals.add(0, Sit);
        goals.add(1, FollowOwner::flying(1.0));
        goals.add(3, LandOnOwnersShoulder);
        goals.add(3, FollowMob::new(1.0, 3.0, 7.0));
        goals.add(2, RandomStroll::flying(1.0, RandomStroll::INTERVAL));
        goals.add(3, LookAtPlayer::new(8.0));
        goals.add(4, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        if let Some(variant) = take(nbt, "Variant", |t| match *t {
            Tag::Int(v) => Some(v),
            _ => None,
        }) {
            self.variant = variant.clamp(0, VARIANT_COUNT - 1);
        }
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Variant", self.variant);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut values = core.tame_metadata();
        values.push((VARIANT, MetadataValue::VarInt(self.variant)));
        values
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if cx.rng.gen_range(0..IMITATE_ODDS) == 0
            && let Some(sound) = imitate_nearby(core, cx)
        {
            let pitch = (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0;
            core.sound(sound, 0.7, pitch, cx);
        }
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityParrotAmbient),
            hurt: Some(SoundEvent::EntityParrotHurt),
            death: Some(SoundEvent::EntityParrotDeath),
            step: Some(SoundEvent::EntityParrotStep),
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntityParrotEat)
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        parrot_food(stack.item())
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref();
        let tamed = core.tame.as_ref().is_some_and(|t| t.is_tamed());
        if let Some(stack) = stack {
            if !tamed && parrot_food(stack.item()) {
                core.try_tame(interaction.player_uuid, TAME_CHANCE, cx);
                return Some(HandEffect::Consume(1));
            }
            if poisonous(stack.item()) {
                core.effects
                    .add(ActiveEffect::timed(MobEffect::Poison, POISON_TICKS, 0));
                core.hurt_later(f32::MAX, &damage_type::GENERIC);
                return Some(HandEffect::Consume(1));
            }
        }
        if tamed && core.is_owner(interaction.player_uuid) && core.base.on_ground {
            let sitting = !core.tame.as_ref().is_some_and(|t| t.sitting);
            core.set_sitting(sitting);
            return Some(HandEffect::None);
        }
        None
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Registers the parrot's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Parrot, super::load::<Parrot>);
    registry.register_spawn_rule(EntityType::Parrot, super::spawning::parrot_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;

    fn interaction(owner: u128, item: Option<Item>) -> EntityInteraction {
        EntityInteraction {
            player: 1,
            player_uuid: owner,
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

    fn entity(kind: EntityType, x: f64) -> tuff_core::world::entity::EntitySummary {
        tuff_core::world::entity::EntitySummary {
            id: 9,
            uuid: 99,
            kind,
            position: [x, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [x - 0.3, 64.0, 0.2, x + 0.3, 66.0, 0.8],
            living: None,
            pickable: true,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    fn heard(entities: &[tuff_core::world::entity::EntitySummary]) -> Vec<SoundEvent> {
        let mut sounds = Vec::new();
        crate::tests::with_entities(entities, |core, cx| {
            core.base.position = [0.5, 64.0, 0.5];
            for _ in 0..200 {
                sounds.extend(imitate_nearby(core, cx));
            }
        });
        sounds
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_parrot_imitates_imitable_mobs_within_twenty_blocks_only() {
        let near = heard(&[entity(EntityType::Creeper, 10.0)]);
        assert!(!near.is_empty());
        assert!(
            near.iter()
                .all(|s| *s == SoundEvent::EntityParrotImitateCreeper)
        );
        assert!(heard(&[entity(EntityType::Creeper, 30.0)]).is_empty());
        assert!(heard(&[entity(EntityType::Cow, 5.0)]).is_empty());
        assert!(heard(&[entity(EntityType::HappyGhast, 5.0)]).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn parrot_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Parrot.data_layout();
        assert_eq!(layout.len(), VARIANT as usize + 1);
        assert_eq!(
            layout[data::SPECIES as usize],
            MetadataValue::Byte(0).type_id() as u8
        );
        assert_eq!(
            layout[(data::SPECIES + 1) as usize],
            MetadataValue::OptionalUuid(None).type_id() as u8
        );
        assert_eq!(
            layout[VARIANT as usize],
            MetadataValue::VarInt(0).type_id() as u8
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn seeds_tame_it_then_the_owner_can_sit_it_and_a_cookie_kills_it() {
        with_context(|core, cx| {
            Parrot::configure(core);
            let mut parrot = Parrot::create(cx.rng);

            let mut tamed = false;
            for _ in 0..100 {
                let effect = parrot.interact(core, &interaction(1, Some(Item::WheatSeeds)), cx);
                assert!(matches!(effect, Some(HandEffect::Consume(1))));
                if core.tame.as_ref().unwrap().is_tamed() {
                    tamed = true;
                    break;
                }
            }
            assert!(tamed, "1/10 chance should succeed within 100 attempts");

            let effect = parrot.interact(core, &interaction(2, None), cx);
            assert!(effect.is_none(), "a stranger cannot command it");

            core.base.on_ground = false;
            assert!(parrot.interact(core, &interaction(1, None), cx).is_none());
            core.base.on_ground = true;
            let effect = parrot.interact(core, &interaction(1, None), cx);
            assert!(matches!(effect, Some(HandEffect::None)));
            assert!(!core.tame.as_ref().unwrap().sitting);

            let effect = parrot.interact(core, &interaction(1, Some(Item::Cookie)), cx);
            assert!(matches!(effect, Some(HandEffect::Consume(1))));
            assert!(
                core.pending_hurts.iter().any(|(a, _)| *a == f32::MAX),
                "a cookie is lethal to a parrot"
            );
        });
    }
}
