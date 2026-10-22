//! Creepers: they walk up to their target, hiss and swell for a second
//! and a half, and explode unless the target backs off. Flint and steel
//! lights them at once; charged creepers blow up twice as big.

use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Item, damage_type};

use super::attack::MeleeAttack;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{AvoidPlayer, Float, LookAtPlayer, RandomLookAround, RandomStroll};
use super::target::{HurtByTarget, NearestAttackablePlayer};
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, TickContext, take_bool, take_number,
};
use tuff_core::world::explosion::{BlockInteraction, Explosion};
use tuff_core::world::messages::HandEffect;
use tuff_nbt::Compound;

const DEFAULT_FUSE: i32 = 30;
const DEFAULT_RADIUS: i32 = 3;
const SWELL_DIR: u8 = data::SPECIES - 2;
const POWERED: u8 = data::SPECIES - 1;
const IGNITED: u8 = data::SPECIES;
/// It starts to swell within 3 blocks and stops beyond 7.
const SWELL_START_SQ: f64 = 9.0;
const SWELL_STOP_SQ: f64 = 49.0;

#[derive(Debug)]
pub struct Creeper {
    swell: i32,
    fuse: i32,
    radius: i32,
    powered: bool,
    ignited: bool,
    /// The swell direction viewers were last told.
    sent_dir: i32,
}

impl Default for Creeper {
    fn default() -> Self {
        Self {
            swell: 0,
            fuse: DEFAULT_FUSE,
            radius: DEFAULT_RADIUS,
            powered: false,
            ignited: false,
            sent_dir: -1,
        }
    }
}

impl Creeper {
    fn sync_dir(&mut self, core: &mut MobCore) {
        if core.signal != self.sent_dir {
            self.sent_dir = core.signal;
            core.changed(SWELL_DIR);
        }
    }
}

/// Swells while its target stays close and in sight.
#[derive(Debug, Default)]
struct Swell {
    target: Option<i32>,
}

impl Goal for Swell {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.signal > 0
            || mob
                .target_view(cx)
                .is_some_and(|t| t.alive && mob.distance_sq(t.position) < SWELL_START_SQ)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
        self.target = mob.target;
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.target = None;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let target = self
            .target
            .and_then(|id| super::target::view(cx, id))
            .filter(|t| t.alive);
        mob.signal = match target {
            Some(t)
                if mob.distance_sq(t.position) <= SWELL_STOP_SQ
                    && mob.has_line_of_sight(&t, cx) =>
            {
                1
            }
            _ => -1,
        };
    }
}

impl Creeper {
    fn spawn_lingering_cloud(&self, core: &MobCore, cx: &mut TickContext) {
        let effects: Vec<_> = core
            .effects
            .iter()
            .map(|e| tuff_item::item::PotionEffect {
                effect: e.effect,
                details: tuff_item::item::EffectDetails {
                    amplifier: e.amplifier,
                    duration: e.duration.clamp(-1, i64::from(i32::MAX)) as i32,
                    ambient: e.ambient,
                    show_particles: e.show_particles,
                    show_icon: e.show_icon,
                    hidden_effect: None,
                },
            })
            .collect();
        if effects.is_empty() {
            return;
        }
        let id = cx.shared.next_entity_id();
        let cloud = tuff_vanilla_common::effect_cloud::AreaEffectCloud::from_effects(
            id,
            core.base.position,
            effects,
            cx.rng,
        );
        cx.effects.push(Effect::Spawn(Box::new(cloud)));
    }

    fn explode(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let rules = cx.shared.game_rules.borrow();
        let blocks = if !rules.bool("mob_griefing") {
            BlockInteraction::Keep
        } else if rules.bool("mob_explosion_drop_decay") {
            BlockInteraction::DestroyWithDecay
        } else {
            BlockInteraction::Destroy
        };
        drop(rules);
        let multiplier = if self.powered { 2.0 } else { 1.0 };
        core.dead = true;
        self.spawn_lingering_cloud(core, cx);
        cx.effects.push(Effect::Explode(Explosion {
            dangerous_wither_skull: false,
            spares_nether_portals: false,
            center: core.base.position,
            radius: self.radius as f32 * multiplier,
            fire: false,
            blocks,
            damage_type: &damage_type::PLAYER_EXPLOSION,
            igniter: None,
            culprit: Some(Box::new(tuff_core::world::entity::Culprit {
                charged: self.powered.then(Default::default),
                ..tuff_core::world::entity::Culprit::entity(&core.base)
            })),
        }));
        core.discarded = true;
    }
}

impl Species for Creeper {
    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.fall_by_health = true;
        core.signal = -1;
        let mut goals = GoalSelector::default();
        goals.add(1, NearestAttackablePlayer::new(true));
        goals.add(2, HurtByTarget::new(&[]));
        goals.add(1, Float::new(core));
        goals.add(2, Swell::default());
        goals.add(
            3,
            AvoidPlayer::entities(&[EntityType::Ocelot, EntityType::Cat], 6.0, 1.0, 1.2),
        );
        goals.add(4, MeleeAttack::new(1.0, false).harmless());
        goals.add(5, RandomStroll::water_avoiding(0.8));
        goals.add(6, LookAtPlayer::new(8.0));
        goals.add(6, RandomLookAround::default());
        core.goals = goals;
    }

    fn heard(&mut self, core: &mut MobCore, notice: tuff_core::world::entity::Notice) {
        if let tuff_core::world::entity::Notice::Charged = notice {
            self.powered = true;
            core.changed(POWERED);
        }
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.powered = take_bool(nbt, "powered").unwrap_or(false);
        self.fuse = take_number(nbt, "Fuse").map_or(DEFAULT_FUSE, |v| v as i16 as i32);
        self.radius =
            take_number(nbt, "ExplosionRadius").map_or(DEFAULT_RADIUS, |v| v as i8 as i32);
        self.ignited = take_bool(nbt, "ignited").unwrap_or(false);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("powered", self.powered);
        out.insert("Fuse", self.fuse as i16);
        out.insert("ExplosionRadius", self.radius as i8);
        out.insert("ignited", self.ignited);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (SWELL_DIR, MetadataValue::VarInt(core.signal)),
            (POWERED, MetadataValue::Boolean(self.powered)),
            (IGNITED, MetadataValue::Boolean(self.ignited)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: None,
            hurt: Some(SoundEvent::EntityCreeperHurt),
            death: Some(SoundEvent::EntityCreeperDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.is_alive() {
            return;
        }
        if self.ignited {
            core.signal = 1;
        }
        if core.signal > 0 && self.swell == 0 {
            core.sound(SoundEvent::EntityCreeperPrimed, 1.0, 0.5, cx);
        }
        self.swell = (self.swell + core.signal).max(0);
        if self.swell >= self.fuse {
            self.swell = self.fuse;
            self.explode(core, cx);
        }
        self.sync_dir(core);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let Some(target) = core.target
            && cx
                .entities
                .iter()
                .any(|e| e.id == target && e.kind == EntityType::Goat)
        {
            core.target = None;
        }
        if let Some(fall) = core.landed_fall.take() {
            self.swell = (self.swell + (fall * 1.5) as i32).min(self.fuse - 5);
        }
        self.sync_dir(core);
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if !tuff_datapack::tags::is(
            tuff_vanilla_data::tag::item::key::CREEPER_IGNITERS,
            stack.item(),
        ) {
            return None;
        }
        let sound = if stack.item() == Item::FireCharge {
            SoundEvent::ItemFirechargeUse
        } else {
            SoundEvent::ItemFlintandsteelUse
        };
        let pitch = rand::Rng::r#gen::<f32>(cx.rng) * 0.4 + 0.8;
        core.sound_as(sound, play::sound_category::HOSTILE, 1.0, pitch, cx);
        self.ignited = true;
        core.changed(IGNITED);
        Some(if stack.is_damageable() {
            HandEffect::Damage(1)
        } else {
            HandEffect::Consume(1)
        })
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Creeper, super::load::<Creeper>);
    registry.register_spawn_rule(EntityType::Creeper, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_lightning_strike_charges_a_creeper() {
        super::super::tests::with_context(|core, _cx| {
            let mut creeper = Creeper::default();
            assert!(!creeper.powered);
            creeper.heard(core, tuff_core::world::entity::Notice::Charged);
            assert!(creeper.powered);
            assert!(
                core.changed.contains(&POWERED),
                "the powered flag goes out to viewers"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_creeper_keeps_away_from_cats_and_ocelots() {
        super::super::tests::with_context(|core, _cx| {
            Creeper::configure(core);
            assert!(core.goals.find::<AvoidPlayer>().is_some());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fall_brings_a_creeper_close_to_exploding() {
        super::super::tests::with_context(|core, cx| {
            let mut creeper = Creeper::default();
            core.landed_fall = Some(2.0);
            creeper.ai_step(core, cx);
            assert_eq!(creeper.swell, 3);
            core.landed_fall = Some(40.0);
            creeper.ai_step(core, cx);
            assert_eq!(creeper.swell, DEFAULT_FUSE - 5, "never all the way");
            assert!(core.landed_fall.is_none());
        });
    }

    fn goat_summary() -> tuff_core::world::entity::EntitySummary {
        tuff_core::world::entity::EntitySummary {
            id: 77,
            uuid: 77,
            kind: EntityType::Goat,
            position: [0.0; 3],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [0.0; 6],
            living: None,
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
    fn a_creeper_drops_a_target_that_is_a_goat() {
        super::super::tests::with_entities(&[goat_summary()], |core, cx| {
            let mut creeper = Creeper::default();
            core.target = Some(77);
            creeper.ai_step(core, cx);
            assert_eq!(core.target, None);
            core.target = Some(5);
            creeper.ai_step(core, cx);
            assert_eq!(core.target, Some(5));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_hunting_creeper_plans_falls_by_its_health() {
        super::super::tests::with_context(|core, _cx| {
            Creeper::configure(core);
            core.health = 20.0;
            assert_eq!(core.max_fall_distance(), 3);
            core.target = Some(1);
            assert_eq!(core.max_fall_distance(), 22);
            core.health = 4.0;
            assert_eq!(core.max_fall_distance(), 6);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_exploding_creeper_with_effects_leaves_a_cloud() {
        use tuff_vanilla_data::registry::MobEffect;
        super::super::tests::with_context(|core, cx| {
            let mut creeper = Creeper::default();
            creeper.explode(core, cx);
            assert!(!cx.effects.iter().any(|e| matches!(e, Effect::Spawn(_))));
            core.add_effect(tuff_core::world::effects::ActiveEffect::timed(
                MobEffect::Speed,
                400,
                0,
            ));
            creeper.explode(core, cx);
            assert!(cx.effects.iter().any(|e| matches!(e, Effect::Spawn(_))));
        });
    }
}
