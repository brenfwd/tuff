//! Skeletons: archers that keep their distance and strafe while they draw,
//! hide from the sun, and fight hand to hand without a bow. Strays shoot
//! slowness arrows; wither skeletons carry stone swords that wither what
//! they hit, and shoot flaming arrows when they hold a bow. Bogged fight the
//! same way but shoot poison arrows, are not afraid of the sun, and grow
//! mushrooms that a pair of shears can harvest. Parched draw as slowly as
//! Bogged and shoot weakness arrows; their own immunity to weakness is not
//! modeled.

use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect, SoundEvent};

use super::attack::MeleeAttack;
use super::goal::GoalSelector;
use super::goals::{LookAtPlayer, RandomLookAround, RandomStroll};
use super::monster::{self, FleeSun, LocalDifficulty, RestrictSun};
use super::ranged::RangedBowAttack;
use super::target::{HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer};
use super::{Mob, MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::drops::EntityFacts;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, TickContext, take_bool, take_number,
};
use tuff_core::world::messages::HandEffect;
use tuff_core::world::player::Difficulty;
use tuff_core::world::projectile::ArrowShot;
use tuff_item::item::{ItemStack, MintReason};

/// Ticks between shots, and on hard.
const ATTACK_INTERVAL: i32 = 40;
const HARD_ATTACK_INTERVAL: i32 = 20;
/// Bogged draw slower than the rest of the family.
const BOGGED_ATTACK_INTERVAL: i32 = 70;
const BOGGED_HARD_ATTACK_INTERVAL: i32 = 50;
const BOW_RANGE: f32 = 15.0;

fn attack_interval(cx: &TickContext) -> i32 {
    if *cx.shared.difficulty.borrow() == Difficulty::Hard {
        HARD_ATTACK_INTERVAL
    } else {
        ATTACK_INTERVAL
    }
}

fn bogged_attack_interval(cx: &TickContext) -> i32 {
    if *cx.shared.difficulty.borrow() == Difficulty::Hard {
        BOGGED_HARD_ATTACK_INTERVAL
    } else {
        BOGGED_ATTACK_INTERVAL
    }
}

const WITHER_TICKS: i64 = 200;
const STRAY_SLOWNESS_TICKS: i64 = 600;
const BOGGED_POISON_TICKS: i64 = 100;
const WITHER_SKELETON_DAMAGE: f64 = 4.0;
const FLAMING_ARROW_SECONDS: f32 = 100.0;
const SHEAR_LOOT_TABLE: &str = "minecraft:shearing/bogged";

fn wither_hook(
    _mob: &MobCore,
    _target: &super::target::TargetView,
    hit: &mut tuff_core::world::entity::EntityHit,
    _cx: &mut TickContext,
) {
    hit.effects
        .push(ActiveEffect::timed(MobEffect::Wither, WITHER_TICKS, 0));
}

fn flaming_arrow(_mob: &MobCore, arrow: &mut ArrowShot) {
    arrow.fire_ticks = (FLAMING_ARROW_SECONDS * 20.0) as i16;
}

fn slowness_arrow(_mob: &MobCore, arrow: &mut ArrowShot) {
    arrow.effects.push(ActiveEffect::timed(
        MobEffect::Slowness,
        STRAY_SLOWNESS_TICKS,
        0,
    ));
}

fn poison_arrow(_mob: &MobCore, arrow: &mut ArrowShot) {
    arrow.effects.push(ActiveEffect::timed(
        MobEffect::Poison,
        BOGGED_POISON_TICKS,
        0,
    ));
}

const PARCHED_WEAKNESS_TICKS: i64 = 600;

fn weakness_arrow(_mob: &MobCore, arrow: &mut ArrowShot) {
    arrow.effects.push(ActiveEffect::timed(
        MobEffect::Weakness,
        PARCHED_WEAKNESS_TICKS,
        0,
    ));
}

/// Bogged and Parched both draw slower than the rest of the family.
fn draws_slowly(kind: EntityType) -> bool {
    matches!(kind, EntityType::Bogged | EntityType::Parched)
}

const SHEARED: u8 = data::BABY;

/// A skeleton, stray, wither skeleton or bogged, by its entity type.
#[derive(Debug, Default)]
pub struct Skeleton {
    sheared: bool,
    freezing_time: i32,
    conversion: Option<i32>,
}

const STRAY_CONVERSION: u8 = data::BABY;
const FREEZE_AFFLICTION_TICKS: i32 = tuff_core::world::freezing::TICKS_TO_FREEZE;
const STRAY_CONVERSION_TICKS: i32 = 300;
const SKELETON_TO_STRAY_EVENT: i32 = 1048;

impl Skeleton {
    fn converts_in_powder_snow(core: &MobCore) -> bool {
        core.kind() == EntityType::Skeleton
    }

    fn tick_conversion(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !Self::converts_in_powder_snow(core)
            || !core.is_alive()
            || core.no_ai
            || core.body.first_tick
        {
            return;
        }
        if core.body.in_powder_snow {
            if let Some(left) = &mut self.conversion {
                *left -= 1;
                if *left < 0 {
                    self.convert_to_stray(core, cx);
                }
                return;
            }
            self.freezing_time += 1;
            if self.freezing_time >= FREEZE_AFFLICTION_TICKS {
                self.conversion = Some(STRAY_CONVERSION_TICKS);
                core.changed(STRAY_CONVERSION);
            }
        } else {
            self.freezing_time = -1;
            if self.conversion.take().is_some() {
                core.changed(STRAY_CONVERSION);
            }
        }
    }

    fn convert_to_stray(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let mut stray: Mob<Skeleton> = super::convert(core, EntityType::Stray, true, cx);
        stray.core.can_pick_up_loot = core.can_pick_up_loot;
        if !core.base.silent {
            monster::level_event(
                super::physics::block_pos(core.base.position),
                SKELETON_TO_STRAY_EVENT,
                0,
                cx,
            );
        }
        cx.effects.push(Effect::Spawn(Box::new(stray)));
        core.discarded = true;
    }
}

impl Species for Skeleton {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        match core.kind() {
            EntityType::WitherSkeleton => {
                core.set_path_malus(tuff_vanilla_data::entity::PathType::Lava, 8.0);
                core.attack_hook = Some(wither_hook);
                core.arrow_hook = Some(flaming_arrow);
            }
            EntityType::Stray => core.arrow_hook = Some(slowness_arrow),
            EntityType::Bogged => core.arrow_hook = Some(poison_arrow),
            EntityType::Parched => core.arrow_hook = Some(weakness_arrow),
            _ => {}
        }
        let mut goals = GoalSelector::default();
        goals.add(1, HurtByTarget::new(&[]));
        goals.add(2, NearestAttackablePlayer::new(true));
        if core.kind() == EntityType::WitherSkeleton {
            goals.add(
                3,
                NearestAttackableEntity::new(&[EntityType::Piglin, EntityType::PiglinBrute], true),
            );
        }
        goals.add(2, RestrictSun);
        goals.add(3, FleeSun::new(1.0));
        goals.add(
            4,
            RangedBowAttack::new(
                1.0,
                if draws_slowly(core.kind()) {
                    bogged_attack_interval
                } else {
                    attack_interval
                },
                BOW_RANGE,
                SoundEvent::EntitySkeletonShoot,
            ),
        );
        goals.add(4, MeleeAttack::new(1.2, false).unless_holding(Item::Bow));
        goals.add(5, RandomStroll::water_avoiding(1.0));
        goals.add(6, LookAtPlayer::new(8.0));
        goals.add(6, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut tuff_nbt::Compound) {
        self.sheared = take_bool(nbt, "sheared").unwrap_or(false);
        self.freezing_time = take_number(nbt, "FreezingTime").map_or(0, |v| v as i32);
        let converting = take_number(nbt, "StrayConversionTime").map_or(-1, |v| v as i32);
        if converting != -1 {
            self.conversion = Some(converting);
        }
    }

    fn save(&self, core: &MobCore, out: &mut tuff_nbt::Compound) {
        out.insert("sheared", self.sheared);
        if Self::converts_in_powder_snow(core) {
            out.insert("StrayConversionTime", self.conversion.unwrap_or(-1));
            out.insert(
                "FreezingTime",
                if core.body.in_powder_snow {
                    self.freezing_time
                } else {
                    -1
                },
            );
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.tick_conversion(core, cx);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        match core.kind() {
            EntityType::Bogged => vec![(SHEARED, MetadataValue::Boolean(self.sheared))],
            EntityType::Skeleton => vec![(
                STRAY_CONVERSION,
                MetadataValue::Boolean(self.conversion.is_some()),
            )],
            _ => Vec::new(),
        }
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        match core.kind() {
            EntityType::WitherSkeleton => monster::sounds(
                SoundEvent::EntityWitherSkeletonAmbient,
                SoundEvent::EntityWitherSkeletonHurt,
                SoundEvent::EntityWitherSkeletonDeath,
                Some(SoundEvent::EntityWitherSkeletonStep),
            ),
            EntityType::Stray => monster::sounds(
                SoundEvent::EntityStrayAmbient,
                SoundEvent::EntityStrayHurt,
                SoundEvent::EntityStrayDeath,
                Some(SoundEvent::EntityStrayStep),
            ),
            EntityType::Bogged => monster::sounds(
                SoundEvent::EntityBoggedAmbient,
                SoundEvent::EntityBoggedHurt,
                SoundEvent::EntityBoggedDeath,
                Some(SoundEvent::EntityBoggedStep),
            ),
            EntityType::Parched => monster::sounds(
                SoundEvent::EntityParchedAmbient,
                SoundEvent::EntityParchedHurt,
                SoundEvent::EntityParchedDeath,
                Some(SoundEvent::EntityParchedStep),
            ),
            _ => monster::sounds(
                SoundEvent::EntitySkeletonAmbient,
                SoundEvent::EntitySkeletonHurt,
                SoundEvent::EntitySkeletonDeath,
                Some(SoundEvent::EntitySkeletonStep),
            ),
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if stack.item() != Item::Shears {
            return None;
        }
        bogged_shear(self, core, play::sound_category::PLAYERS, cx).then_some(HandEffect::Damage(1))
    }

    fn shear(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        bogged_shear(self, core, play::sound_category::BLOCKS, cx)
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let weapon = if core.kind() == EntityType::WitherSkeleton {
            core.set_attribute_base(Attribute::AttackDamage, WITHER_SKELETON_DAMAGE);
            Item::StoneSword
        } else {
            core.populate_spawn_armor(LocalDifficulty::at(cx, core.base.position), cx.rng);
            Item::Bow
        };
        core.equipment.set(
            EquipmentSlot::Mainhand,
            Some(ItemStack::mint(weapon, 1, MintReason::Spawn)),
        );
        let difficulty = LocalDifficulty::at(cx, core.base.position);
        if core.kind() != EntityType::WitherSkeleton {
            core.populate_spawn_enchantments(difficulty, cx.rng);
        }
        core.can_pick_up_loot =
            rand::Rng::r#gen::<f32>(cx.rng) < 0.55 * difficulty.special_multiplier();
        core.wear_halloween_head(cx.rng, super::calendar::halloween_now());
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    for kind in [
        EntityType::Skeleton,
        EntityType::Stray,
        EntityType::WitherSkeleton,
        EntityType::Bogged,
        EntityType::Parched,
    ] {
        registry.register(kind, super::load::<Skeleton>);
    }
    registry.register_spawn_rule(EntityType::Skeleton, super::spawning::monster_rule);
    registry.register_spawn_rule(EntityType::WitherSkeleton, super::spawning::monster_rule);
    registry.register_spawn_rule(EntityType::Stray, super::spawning::monster_under_sky_rule);
    registry.register_spawn_rule(EntityType::Bogged, super::spawning::monster_rule);
    registry.register_spawn_rule(EntityType::Parched, super::spawning::monster_under_sky_rule);
}

/// Shears a bogged's mushrooms off, with the sound in `category`.
fn bogged_shear(
    skeleton: &mut Skeleton,
    core: &mut MobCore,
    category: i32,
    cx: &mut TickContext,
) -> bool {
    if core.kind() != EntityType::Bogged || skeleton.sheared {
        return false;
    }
    core.sound_as(SoundEvent::EntityBoggedShear, category, 1.0, 1.0, cx);
    let facts = EntityFacts {
        kind: Some(core.base.kind),
        tables: Some(cx.shared.loot_tables()),
        ..Default::default()
    };
    let [x, y, z] = core.base.position;
    for drop in cx
        .shared
        .block_loot()
        .entity_drops(SHEAR_LOOT_TABLE, &facts, cx.rng)
    {
        cx.effects.push(Effect::DropItem {
            stack: drop,
            position: [x, y + f64::from(core.size.height), z],
            velocity: [0.0; 3],
            pickup_delay: tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY,
        });
    }
    skeleton.sheared = true;
    core.changed(SHEARED);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_skeleton_in_powder_snow_shakes_then_becomes_a_stray() {
        crate::tests::with_context(|core, cx| {
            core.base.kind = EntityType::Skeleton;
            core.body.first_tick = false;
            core.body.in_powder_snow = true;
            let mut skeleton = Skeleton::default();
            for _ in 0..FREEZE_AFFLICTION_TICKS {
                skeleton.tick_conversion(core, cx);
            }
            assert_eq!(skeleton.conversion, Some(STRAY_CONVERSION_TICKS));
            for _ in 0..=STRAY_CONVERSION_TICKS {
                assert!(cx.effects.iter().all(|e| !matches!(e, Effect::Spawn(_))));
                skeleton.tick_conversion(core, cx);
            }
            assert!(core.discarded);
            assert!(cx.effects.iter().any(|e| matches!(e, Effect::Spawn(_))));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn leaving_powder_snow_stops_the_stray_conversion() {
        crate::tests::with_context(|core, cx| {
            core.base.kind = EntityType::Skeleton;
            core.body.first_tick = false;
            core.body.in_powder_snow = true;
            let mut skeleton = Skeleton {
                conversion: Some(100),
                ..Default::default()
            };
            core.body.in_powder_snow = false;
            skeleton.tick_conversion(core, cx);
            assert_eq!(skeleton.conversion, None);
            assert!(!core.discarded);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn parched_adds_no_data_beyond_the_shared_mob_layout() {
        assert_eq!(
            EntityType::Parched.data_layout().len(),
            data::MOB_FLAGS as usize + 1,
            "a Parched has nothing past the shared mob flags byte"
        );
    }
}
