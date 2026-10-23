//! Growing up and breeding: the ageable part (babies, growing up, age
//! locking) and the animal part (love mode from food, finding a partner,
//! the baby and the experience it gives) of mobs that have them.

use rand::Rng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::{ParticleType, SoundEvent};

use super::{Mob, MobCore, Species, data, event};
use tuff_core::protocol::play;
use tuff_core::world::entity::{
    Effect, EntityBase, EntityInteraction, InteractOutcome, RegionEntity, TickContext,
};
use tuff_core::world::messages::HandEffect;
use tuff_core::world::{framed, packets};

pub const BABY_START_AGE: i32 = -24000;
pub const PARENT_AGE_AFTER_BREEDING: i32 = 6000;
pub const LOVE_TICKS: i32 = 600;
const FORCED_AGE_PARTICLE_TICKS: i32 = 40;
const AGE_LOCK_COOLDOWN: i32 = 40;

/// How old an ageable mob is: negative while a baby, counting up to zero;
/// positive after breeding, counting down.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Age {
    pub age: i32,
    pub forced_age: i32,
    pub forced_age_timer: i32,
    pub locked: bool,
    pub lock_cooldown: i32,
    pub never_ages: bool,
}

impl Age {
    pub fn is_baby(&self) -> bool {
        self.age < 0
    }

    /// Sets the age; true if that made it grow up or turn into a baby.
    pub fn set(&mut self, age: i32) -> bool {
        let crossed = (self.age < 0) != (age < 0);
        self.age = age;
        crossed
    }

    pub fn can_age_up(&self) -> bool {
        self.is_baby() && !self.locked && !self.never_ages
    }
}

/// Love mode: ticks left, and the player who fed it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Love {
    pub ticks: i32,
    pub cause: Option<u128>,
}

impl Love {
    pub fn is_in_love(&self) -> bool {
        self.ticks > 0
    }

    pub fn reset(&mut self) {
        self.ticks = 0;
    }
}

/// Seconds a baby grows by when fed, from the ticks it has left.
pub fn speed_up_seconds(ticks_until_adult: i32) -> i32 {
    ((ticks_until_adult / 20) as f32 * 0.1) as i32
}

/// An owner and a sit order, shared by every tameable animal (wolves,
/// cats, and any future type -- parrots, horses -- that tames the same
/// way): a player earns it by chance from an interaction, then commands
/// it to sit and stay instead of following or wandering.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tame {
    pub owner: Option<u128>,
    /// Ordered to sit by the owner (saved as `Sitting`).
    pub sitting: bool,
    /// The synched sitting pose (flag 0x01), driven by the sit goals.
    pub pose: bool,
    /// Tamed with no owner (a sprung skeleton trap horse).
    pub ownerless: bool,
    /// A begging wolf tilts its head (the interested flag).
    pub begging: bool,
}

impl Tame {
    pub fn is_tamed(&self) -> bool {
        self.owner.is_some() || self.ownerless
    }

    /// The shared `TameableAnimal` flags byte: 0x01 sitting, 0x04 tamed.
    pub fn flags_byte(&self) -> i8 {
        (if self.pose { 0x01 } else { 0 }) | (if self.is_tamed() { 0x04 } else { 0 })
    }
}

/// Whether `mob` is a tameable animal nobody has tamed yet.
pub(crate) fn untamed(mob: &MobCore, _cx: &TickContext) -> bool {
    mob.tame.as_ref().is_some_and(|t| !t.is_tamed())
}

impl MobCore {
    /// Makes this an ageable animal, as every farm animal is.
    pub fn make_animal(&mut self) {
        self.age = Some(Age::default());
        self.love = Some(Love::default());
        self.set_path_malus(tuff_vanilla_data::entity::PathType::FireInNeighbor, 16.0);
        self.set_path_malus(tuff_vanilla_data::entity::PathType::Fire, -1.0);
    }

    /// Makes this a tameable animal; species set the flags/owner index
    /// their own `metadata()` returns from `self.tame`.
    pub fn make_tameable(&mut self, flags_index: u8) {
        self.tame = Some(Tame::default());
        self.tame_flags_index = flags_index;
    }

    /// Tells the new owner's `tame_animal` advancement criteria.
    pub fn report_tamed(&self, owner: u128, cx: &mut TickContext) {
        cx.effects.push(Effect::Advancement {
            player: owner,
            event: Box::new(tuff_core::world::messages::AdvancementEvent::TameAnimal(
                self.facts(),
            )),
        });
    }

    /// Tries to tame with the given chance; does nothing if already tamed.
    /// Broadcasts the smoke/heart entity event either way, and orders it to
    /// sit on success, matching every tameable species. Returns whether it
    /// succeeded.
    pub fn try_tame(&mut self, owner: u128, chance: f64, cx: &mut TickContext) -> bool {
        let Some(tame) = &self.tame else {
            return false;
        };
        if tame.is_tamed() {
            return false;
        }
        let succeeded = cx.rng.gen_bool(chance);
        if succeeded {
            let Some(tame) = &mut self.tame else {
                unreachable!("checked above")
            };
            tame.owner = Some(owner);
            tame.sitting = true;
            self.persistence_required = true;
            self.changed(self.tame_flags_index);
            self.changed(self.tame_flags_index + 1);
            self.navigation.stop();
            self.target = None;
            self.report_tamed(owner, cx);
        }
        self.entity_event(
            if succeeded {
                event::TAME_SUCCEEDED
            } else {
                event::TAME_FAILED
            },
            cx,
        );
        succeeded
    }

    pub fn set_sitting(&mut self, sitting: bool) {
        if let Some(tame) = &mut self.tame {
            tame.sitting = sitting;
        }
    }

    pub fn set_sitting_pose(&mut self, pose: bool) {
        let index = self.tame_flags_index;
        if let Some(tame) = &mut self.tame
            && tame.pose != pose
        {
            tame.pose = pose;
            self.changed(index);
        }
    }

    pub fn is_owner(&self, uuid: u128) -> bool {
        self.tame.as_ref().is_some_and(|t| t.owner == Some(uuid))
    }

    /// The flags byte and owner reference every tameable species sends at
    /// its `make_tameable` index.
    pub fn tame_metadata(&self) -> Vec<(u8, tuff_core::protocol::play::MetadataValue)> {
        use tuff_core::protocol::play::MetadataValue;
        let tame = self.tame.as_ref().cloned().unwrap_or_default();
        vec![
            (
                self.tame_flags_index,
                MetadataValue::Byte(tame.flags_byte()),
            ),
            (
                self.tame_flags_index + 1,
                MetadataValue::OptionalUuid(tame.owner),
            ),
        ]
    }

    pub fn set_age(&mut self, age: i32) {
        let Some(current) = &mut self.age else {
            return;
        };
        if current.set(age) {
            self.changed(data::BABY);
            self.refresh_size();
        }
    }

    pub fn set_baby(&mut self, baby: bool) {
        if self.age.is_none() {
            if self.baby != baby {
                self.baby = baby;
                self.changed(data::BABY);
                self.refresh_size();
            }
            return;
        }
        self.set_age(if baby { BABY_START_AGE } else { 0 });
    }

    /// Grows a baby by `seconds`; `forced` when fed, which shows particles
    /// and is remembered for the next time it is a baby.
    pub fn age_up(&mut self, seconds: i32, forced: bool) {
        let Some(age) = &self.age else {
            return;
        };
        let current = age.age;
        let next = (current + seconds * 20).min(0);
        let delta = next - current;
        self.set_age(next);
        let age = self.age.as_mut().expect("checked above");
        if forced {
            age.forced_age += delta;
            if age.forced_age_timer == 0 {
                age.forced_age_timer = FORCED_AGE_PARTICLE_TICKS;
            }
        }
        if age.age == 0 {
            let forced_age = age.forced_age;
            self.set_age(forced_age);
        }
    }

    pub(super) fn tick_age(&mut self, cx: &mut TickContext) {
        let Some(age) = &mut self.age else {
            return;
        };
        if age.forced_age_timer > 0 {
            if age.forced_age_timer % 4 == 0 {
                let position = random_point(&self.base, self.size.width, self.size.height, cx);
                let packet = framed(&play::Particles {
                    particle: ParticleType::HappyVillager.id(),
                    position: [position[0], position[1] + 0.5, position[2]],
                    spread: [0.0; 3],
                    speed: 0.0,
                    count: 1,
                    alternative: false,
                });
                cx.effects.push(Effect::ChunkPackets(packets([packet])));
            }
            let age = self.age.as_mut().expect("checked above");
            age.forced_age_timer -= 1;
        }
        let age = self.age.as_mut().expect("checked above");
        if age.lock_cooldown > 0 {
            age.lock_cooldown -= 1;
        }
        if self.is_alive() {
            let age = self.age.as_ref().expect("checked above");
            if age.can_age_up() {
                let next = age.age + 1;
                self.set_age(next);
            } else if age.age > 0 {
                let next = age.age - 1;
                self.set_age(next);
            }
        }
    }

    pub(super) fn tick_love(&mut self, _cx: &mut TickContext) {
        let aged = self.age.as_ref().is_some_and(|a| a.age != 0);
        if let Some(love) = &mut self.love {
            if aged {
                love.reset();
            }
            if love.ticks > 0 {
                love.ticks -= 1;
            }
        }
    }

    pub fn set_in_love(&mut self, player: Option<u128>, cx: &mut TickContext) {
        if let Some(love) = &mut self.love {
            love.ticks = LOVE_TICKS;
            love.cause = player;
            self.entity_event(event::IN_LOVE, cx);
        }
    }
}

fn random_point(base: &EntityBase, width: f32, height: f32, cx: &mut TickContext) -> [f64; 3] {
    let [x, y, z] = base.position;
    let width = width as f64;
    [
        x + width * (2.0 * cx.rng.r#gen::<f64>() - 1.0),
        y + height as f64 * cx.rng.r#gen::<f64>(),
        z + width * (2.0 * cx.rng.r#gen::<f64>() - 1.0),
    ]
}

/// The shared animal handling of a click: food makes an adult fall in love
/// and a baby grow, a golden dandelion locks or unlocks a baby's age.
pub fn interact<S: Species>(
    mob: &mut Mob<S>,
    interaction: &EntityInteraction,
    cx: &mut TickContext,
) -> InteractOutcome {
    let stack = interaction.stack.as_deref()?;
    let core = &mut mob.core;
    if stack.item() == Item::GoldenDandelion
        && core.is_baby()
        && core.age.as_ref().is_some_and(|a| a.lock_cooldown == 0)
        && !tuff_core::world::entity_in_tag(core.base.kind, "cannot_be_age_locked")
    {
        let age = core.age.as_mut().expect("baby");
        age.locked = !age.locked;
        age.lock_cooldown = AGE_LOCK_COOLDOWN;
        let locked = age.locked;
        core.set_age(BABY_START_AGE);
        core.changed(data::AGE_LOCKED);
        if locked {
            core.persistence_required = true;
        }
        let sound = if locked {
            SoundEvent::ItemGoldenDandelionUse
        } else {
            SoundEvent::ItemGoldenDandelionUnuse
        };
        core.sound_as(sound, play::sound_category::PLAYERS, 1.0, 1.0, cx);
        return Some(HandEffect::Consume(1));
    }
    if core.age.is_some() && stack.item().spawns_entity() == Some(core.base.kind) {
        let species = mob.species.offspring_in(&mob.species, cx);
        return spawn_egg_baby(mob, species, stack, cx);
    }
    if core.love.is_none() || !mob.species.is_food(stack) {
        return None;
    }
    let core = &mut mob.core;
    let age = core.age.as_ref().map_or(0, |a| a.age);
    if age == 0
        && core.love.as_ref().is_some_and(|l| l.ticks <= 0)
        && mob.species.can_fall_in_love(core)
    {
        core.set_in_love(Some(interaction.player_uuid), cx);
        mob.play_eating_sound(cx);
        return Some(HandEffect::Consume(1));
    }
    if core.age.as_ref().is_some_and(Age::can_age_up) {
        core.age_up(speed_up_seconds(-age), true);
        mob.play_eating_sound(cx);
        return Some(HandEffect::Consume(1));
    }
    None
}

/// A spawn egg used on an animal of its own kind: a baby appears where the
/// animal stands, with the egg's components, and the egg is used up.
fn spawn_egg_baby<S: Species, M: tuff_item::item::Mode>(
    mob: &mut Mob<S>,
    species: S,
    stack: &tuff_item::item::ItemStack<M>,
    cx: &mut TickContext,
) -> InteractOutcome {
    let mut base = EntityBase::new(
        mob.core.base.kind,
        cx.shared.next_entity_id(),
        cx.rng.r#gen(),
        mob.core.base.position,
        [0.0; 3],
    );
    base.yaw = 0.0;
    let mut child = Mob::<S>::new(base, cx.rng);
    child.species = species;
    child.species.finalize_offspring(&mut child.core);
    child.core.set_baby(true);
    if !child.core.is_baby() {
        return None;
    }
    child.core.changed.clear();
    let mut child: Box<dyn RegionEntity> = Box::new(child);
    crate::apply_egg_components(&mut child, &stack.components);
    cx.effects.push(Effect::Spawn(child));
    Some(HandEffect::Consume(1))
}

/// Breeds two animals of one species: the baby, the parents' cooldown and
/// the experience orb.
pub fn breed<S: Species>(
    mob: &mut Mob<S>,
    partner: &mut Mob<S>,
    cx: &mut TickContext,
) -> Option<Box<dyn RegionEntity>> {
    if !ready_to_breed(&mob.core) || !ready_to_breed(&partner.core) {
        return None;
    }
    let species = mob.species.offspring_in(&partner.species, cx);
    let kind = mob.core.base.kind;
    Some(Box::new(breed_child(mob, partner, kind, species, cx)))
}

/// Whether an animal can breed right now: alive and in love.
pub fn ready_to_breed(core: &MobCore) -> bool {
    core.love.as_ref().is_some_and(Love::is_in_love) && core.is_alive()
}

/// The baby of `kind` two ready parents (perhaps of different species)
/// have, with the parents' cooldown and the experience orb.
pub fn breed_child<A: Species, B: Species, C: Species>(
    mob: &mut Mob<A>,
    partner: &mut Mob<B>,
    kind: tuff_vanilla_data::registry::EntityType,
    species: C,
    cx: &mut TickContext,
) -> Mob<C> {
    let mut base = EntityBase::new(
        kind,
        cx.shared.next_entity_id(),
        cx.rng.r#gen(),
        mob.core.base.position,
        [0.0; 3],
    );
    base.yaw = 0.0;
    let mut child = Mob::<C>::new(base, cx.rng);
    child.species = species;
    child.species.finalize_offspring(&mut child.core);
    child.core.set_baby(true);
    child.core.changed.clear();
    let cause = [&mob.core, &partner.core]
        .into_iter()
        .find_map(|parent| parent.love.as_ref().and_then(|love| love.cause));
    if let Some(player) = cause {
        cx.effects.push(Effect::Advancement {
            player,
            event: Box::new(tuff_core::world::messages::AdvancementEvent::BredAnimals {
                parent: mob.core.facts(),
                partner: partner.core.facts(),
                child: Some(child.core.facts()),
            }),
        });
    }
    for parent in [&mut mob.core, &mut partner.core] {
        parent.set_age(PARENT_AGE_AFTER_BREEDING);
        if let Some(love) = &mut parent.love {
            love.reset();
        }
    }
    mob.core.entity_event(event::IN_LOVE, cx);
    if cx.shared.game_rules.borrow().bool("mob_drops") {
        cx.effects.push(Effect::Experience {
            position: mob.core.base.position,
            amount: cx.rng.gen_range(1..=7),
        });
    }
    child
}

#[cfg(test)]
mod tests {
    use tuff_vanilla_data::registry::EntityType;

    use crate::target::TargetView;
    use crate::tests::with_context;

    #[tuff_core::watchdog]
    #[test]
    fn the_sit_order_and_the_sitting_pose_are_separate() {
        use crate::goal::Goal;
        with_context(|core, cx| {
            core.make_tameable(18);
            core.base.on_ground = true;
            core.set_sitting(true);
            assert_eq!(core.tame.as_ref().unwrap().flags_byte() & 1, 0);
            let mut sit = crate::goals::Sit;
            assert!(sit.can_use(core, cx));
            sit.start(core, cx);
            assert_eq!(core.tame.as_ref().unwrap().flags_byte() & 1, 1);
            assert!(core.changed.contains(&18));
            sit.stop(core, cx);
            let tame = core.tame.as_ref().unwrap();
            assert!(tame.sitting && tame.flags_byte() & 1 == 0);
            core.base.on_ground = false;
            assert!(!sit.can_use(core, cx), "no pose while airborne");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn try_tame_only_succeeds_at_the_given_chance() {
        with_context(|core, cx| {
            core.make_tameable(18);
            assert!(!core.try_tame(1, 0.0, cx), "never succeeds at chance 0");
            assert!(core.tame.as_ref().unwrap().owner.is_none());

            assert!(core.try_tame(42, 1.0, cx), "always succeeds at chance 1");
            let tame = core.tame.as_ref().unwrap();
            assert_eq!(tame.owner, Some(42));
            assert!(tame.sitting, "a freshly tamed animal sits");
            assert!(core.is_owner(42));
            assert!(core.persistence_required);
            assert!(core.changed.contains(&18) && core.changed.contains(&19));

            // Already tamed: a further attempt never re-rolls the owner.
            assert!(!core.try_tame(7, 1.0, cx));
            assert_eq!(core.tame.as_ref().unwrap().owner, Some(42));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_tamed_animal_is_never_a_valid_attack_target_for_its_owner() {
        with_context(|core, cx| {
            core.make_tameable(18);
            core.try_tame(99, 1.0, cx);
            let target = TargetView {
                id: 5,
                uuid: 99,
                kind: EntityType::Player,
                position: [0.0; 3],
                bounds: [0.0; 6],
                eye: [0.0; 3],
                alive: true,
                creative: false,
                invulnerable: false,
                in_water: false,
                seen_as_enemy: true,
                owner: None,
            };
            assert!(!core.can_attack(&target, cx), "must never target its owner");
        });
    }
}
