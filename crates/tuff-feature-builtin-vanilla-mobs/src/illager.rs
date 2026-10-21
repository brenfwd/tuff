//! Illagers and raid mobs: shared spellcasting machinery and target/ally
//! lists. Raid and patrol membership and goals live in `raider`.

use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::goal::{Flags, Goal, GoalSelector};
use super::target::{NearestAttackableEntity, TargetView};
use super::{MobCore, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::TickContext;

pub const CREAKING: &[EntityType] = &[EntityType::Creaking];

/// Every raider type, for `HurtByTarget::alerting` (the game's
/// `Raider.class` filter).
pub const RAIDERS: &[EntityType] = &[
    EntityType::Witch,
    EntityType::Vindicator,
    EntityType::Evoker,
    EntityType::Pillager,
    EntityType::Ravager,
    EntityType::Illusioner,
];

/// The shared raider "is celebrating" flag (index 16).
pub const CELEBRATING: u8 = data::BABY;

pub fn celebrating(core: &MobCore) -> MetadataValue {
    MetadataValue::Boolean(core.raider.as_ref().is_some_and(|r| r.celebrating))
}

/// `SpellcasterIllager`'s spell ids (the Spell metadata byte).
pub mod spell {
    pub const NONE: u8 = 0;
    pub const SUMMON_VEX: u8 = 1;
    pub const FANGS: u8 = 2;
    pub const WOLOLO: u8 = 3;
    pub const DISAPPEAR: u8 = 4;
    pub const BLINDNESS: u8 = 5;
}

/// The villager and iron golem target goals illagers share (village
/// raiders defend nothing here without raids, but still see them as fair
/// targets, matching the game's target selector).
pub fn add_villager_targets(
    goals: &mut GoalSelector,
    priority: u8,
    must_see: bool,
    golem_must_see: bool,
) {
    goals.add(
        priority,
        NearestAttackableEntity::new(
            &[EntityType::Villager, EntityType::WanderingTrader],
            must_see,
        ),
    );
    goals.add(
        priority,
        NearestAttackableEntity::new(&[EntityType::IronGolem], golem_must_see),
    );
}

pub const DEFAULT_WARMUP: i32 = 20;

/// A spellcaster illager's cast: it stops, looks at its target through a
/// warmup, then runs one effect and waits out a cooldown before it may
/// cast again. `MobCore::signal` carries the spell id while casting (0
/// otherwise); it doubles as the Spell metadata byte and the lock that
/// keeps only one spell goal running at a time, the way the game's own
/// casting check does.
pub struct CastSpell {
    spell: u8,
    casting_time: i32,
    interval: i32,
    prepare_sound: Option<SoundEvent>,
    cast_sound: SoundEvent,
    when: Option<fn(&MobCore, &TickContext) -> bool>,
    roll: Option<fn(&MobCore, &mut TickContext) -> bool>,
    perform: fn(&mut MobCore, &TargetView, &mut TickContext),
    warmup: i32,
    casting: i32,
    next_tick: u64,
    /// Never cast twice in a row at the same target, by id, when set.
    last_target: Option<Option<i32>>,
}

impl std::fmt::Debug for CastSpell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CastSpell")
            .field("spell", &self.spell)
            .finish()
    }
}

impl CastSpell {
    pub fn new(
        spell: u8,
        casting_time: i32,
        interval: i32,
        cast_sound: SoundEvent,
        perform: fn(&mut MobCore, &TargetView, &mut TickContext),
    ) -> Self {
        Self {
            spell,
            casting_time,
            interval,
            prepare_sound: None,
            cast_sound,
            when: None,
            roll: None,
            perform,
            warmup: 0,
            casting: 0,
            next_tick: 0,
            last_target: None,
        }
    }

    /// Skips a target it last cast at (the illusioner's blindness).
    pub fn once_per_target(mut self) -> Self {
        self.last_target = Some(None);
        self
    }

    pub fn prepare_sound(mut self, sound: SoundEvent) -> Self {
        self.prepare_sound = Some(sound);
        self
    }

    /// A final `canUse` check that may roll the dice, run once the target
    /// is known to be alive (the evoker's vex-count gate).
    pub fn then(mut self, roll: fn(&MobCore, &mut TickContext) -> bool) -> Self {
        self.roll = Some(roll);
        self
    }

    /// An extra `canUse` check (the illusioner's blindness spell only on
    /// hard, its mirror spell only while not already invisible).
    pub fn when(mut self, when: fn(&MobCore, &TickContext) -> bool) -> Self {
        self.when = Some(when);
        self
    }
}

impl Goal for CastSpell {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.signal != 0 || cx.tick < self.next_tick {
            return false;
        }
        if self.when.is_some_and(|f| !f(mob, cx)) {
            return false;
        }
        if let Some(last) = self.last_target
            && last.is_some()
            && mob.target == last
        {
            return false;
        }
        if !mob.target_view(cx).is_some_and(|t| t.alive) {
            return false;
        }
        self.roll.is_none_or(|f| f(mob, cx))
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        (self.warmup > 0 && mob.target_view(cx).is_some_and(|t| t.alive)) || self.casting > 0
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.warmup = self.adjusted_delay(DEFAULT_WARMUP).max(1);
        self.casting = self.casting_time;
        mob.signal = i32::from(self.spell);
        mob.changed(data::AGE_LOCKED);
        self.next_tick = cx.tick + self.interval as u64;
        if let Some(last) = &mut self.last_target {
            *last = mob.target;
        }
        mob.navigation.stop();
        if let Some(sound) = self.prepare_sound {
            mob.sound(sound, 1.0, 1.0, cx);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.signal = 0;
        mob.changed(data::AGE_LOCKED);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if self.casting > 0 {
            self.casting -= 1;
        }
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        if self.warmup > 0 {
            self.warmup -= 1;
            if self.warmup == 0 {
                (self.perform)(mob, &target, cx);
                mob.sound(self.cast_sound, 1.0, 1.0, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;
    use tuff_core::world::entity::{EntitySummary, LivingSummary};

    #[tuff_core::watchdog]
    #[test]
    fn every_illager_avoids_creakings() {
        with_context(|core, _| {
            let mut check = |configure: fn(&mut MobCore)| {
                configure(core);
                assert!(format!("{:?}", core.goals).contains("Creaking"));
            };
            check(<crate::pillager::Pillager as crate::Species>::configure);
            check(<crate::vindicator::Vindicator as crate::Species>::configure);
            check(<crate::evoker::Evoker as crate::Species>::configure);
            check(<crate::illusioner::Illusioner as crate::Species>::configure);
        });
    }

    fn villager(id: i32) -> EntitySummary {
        EntitySummary {
            id,
            uuid: id as u128,
            kind: EntityType::Villager,
            position: [3.5, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [3.2, 64.0, 0.2, 3.8, 65.95, 0.8],
            living: Some(LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 1.62,
                in_water: false,
                seen_as_enemy: true,
                enemy: false,
                health: 20.0,
                tamed: false,
                owner: None,
                chasing: None,
            }),
            pickable: true,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_once_per_target_spell_waits_for_a_new_target() {
        let targets = [villager(7), villager(8)];
        crate::tests::with_entities(&targets, |mob, cx| {
            let mut spell = CastSpell::new(
                spell::BLINDNESS,
                20,
                0,
                SoundEvent::EntityIllusionerCastSpell,
                |_, _, _| {},
            )
            .once_per_target();
            mob.target = Some(7);
            assert!(spell.can_use(mob, cx));
            spell.start(mob, cx);
            spell.stop(mob, cx);
            assert!(!spell.can_use(mob, cx), "not twice at the same target");
            mob.target = Some(8);
            assert!(spell.can_use(mob, cx));
            spell.start(mob, cx);
            spell.stop(mob, cx);
            mob.target = Some(7);
            assert!(spell.can_use(mob, cx), "back to the first one is fine");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_effect_fires_after_the_warmup_and_the_lock_lasts_the_casting_time() {
        let targets = [villager(7)];
        crate::tests::with_entities(&targets, |mob, cx| {
            static FIRED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let mut spell = CastSpell::new(
                spell::FANGS,
                40,
                0,
                SoundEvent::EntityEvokerCastSpell,
                |_, _, _| {
                    FIRED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
            );
            mob.target = Some(7);
            assert!(spell.can_use(mob, cx));
            spell.start(mob, cx);
            for _ in 0..DEFAULT_WARMUP {
                assert!(spell.can_continue(mob, cx));
                spell.tick(mob, cx);
            }
            assert_eq!(FIRED.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert!(spell.can_continue(mob, cx), "still casting");
            assert_ne!(mob.signal, 0);
            for _ in DEFAULT_WARMUP..40 {
                spell.tick(mob, cx);
            }
            assert!(!spell.can_continue(mob, cx));
            assert_eq!(FIRED.load(std::sync::atomic::Ordering::SeqCst), 1);
        });
    }
}
