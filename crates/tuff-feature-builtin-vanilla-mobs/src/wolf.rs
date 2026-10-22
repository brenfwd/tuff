//! Wolves: tamed with bones, sat and healed by their owner, retaliate
//! against whoever hurts them (never the owner), and breed on meat.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::item::DyeColor;
use tuff_vanilla_data::registry::{Attribute, EntityType};
use tuff_vanilla_data::tag;

use super::animal::untamed;
use super::attack::{LeapAtTarget, MeleeAttack};
use super::goal::GoalSelector;
use super::goals::AvoidPlayer;
use super::goals::{Breed, FollowOwner, LookAtPlayer, Panic, RandomLookAround, RandomStroll, Sit};
use super::target::{
    HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer, OwnerTarget,
    ResetUniversalAnger,
};
use super::voiced_variant::Variants;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::drops::EntityFacts;
use tuff_core::world::entity::NearbyPlayer;
use tuff_core::world::entity::{EntityInteraction, InteractOutcome, TickContext, take};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};

const PREY: [EntityType; 3] = [EntityType::Sheep, EntityType::Rabbit, EntityType::Fox];
const SKELETONS: [EntityType; 5] = [
    EntityType::Skeleton,
    EntityType::Stray,
    EntityType::WitherSkeleton,
    EntityType::Bogged,
    EntityType::Parched,
];
const ANGER_SYNC_SLACK: i64 = 20;

fn wild(core: &MobCore) -> bool {
    !core.tame.as_ref().is_some_and(|t| t.is_tamed())
}

fn grudge(mob: &MobCore, player: &NearbyPlayer) -> bool {
    mob.angry_at_player(player.uuid)
}

const TAME_CHANCE: f64 = 1.0 / 3.0;
const UNTAMED_MAX_HEALTH: f64 = 8.0;
const TAMED_MAX_HEALTH: f64 = 40.0;
const HEAL_AMOUNT: f32 = 2.0;
const ARMOR_REPAIR_UNIT: f32 = 0.125;
const SHAKE_START_EVENT: i8 = 8;
const SHAKE_CANCEL_EVENT: i8 = 56;

const BEGGING: u8 = data::SPECIES + 2;
const COLLAR: u8 = data::SPECIES + 3;
const ANGER_TIME: u8 = data::SPECIES + 4;
const VARIANT: u8 = data::SPECIES + 5;

fn wolf_food(item: tuff_vanilla_data::Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::WOLF_FOOD, item)
}

#[derive(Debug)]
pub struct Wolf {
    variants: Variants,
    collar: DyeColor,
    wet: bool,
    shaking: bool,
    shake_anim: f32,
    anger_end: i64,
}

const ARMOR_CRACK_FRACTIONS: [f32; 3] = [0.95, 0.69, 0.32];
const BODY_BREAK_EVENT: i8 = 65;

fn armor_crack_tier(damage: u32, max_damage: u32) -> usize {
    let remaining = (max_damage.saturating_sub(damage) / max_damage) as f32;
    ARMOR_CRACK_FRACTIONS
        .iter()
        .filter(|fraction| remaining < **fraction)
        .count()
}

fn armor_tier(stack: Option<&ItemStack>) -> usize {
    stack
        .filter(|stack| stack.is_damageable())
        .and_then(|stack| Some(armor_crack_tier(stack.damage(), stack.max_damage()?)))
        .unwrap_or(0)
}

fn scute_particles(core: &MobCore, cx: &mut TickContext) {
    use tuff_core::protocol::codec::WriteExt;
    let mut options = tuff_wire::PacketWriter::new();
    options.write_item_template(
        ItemStack::mint(
            tuff_vanilla_data::Item::ArmadilloScute,
            1,
            MintReason::Spawn,
        )
        .view(),
    );
    let options = options.into_bytes();
    let [x, y, z] = core.base.position;
    let packet = tuff_core::world::framed(&tuff_core::protocol::play::ParticlesWithOptions {
        particle: tuff_vanilla_data::registry::ParticleType::Item.id(),
        options: &options,
        force: false,
        position: [x, y + 1.0, z],
        spread: [0.2, 0.1, 0.2],
        speed: 0.1,
        count: 20,
    });
    cx.effects
        .push(tuff_core::world::entity::Effect::ChunkPackets(
            tuff_core::world::packets([packet]),
        ));
}

impl Species for Wolf {
    fn absorb_hurt(
        &mut self,
        core: &mut MobCore,
        damage: &super::Damage,
        amount: f32,
        cx: &mut TickContext,
    ) -> bool {
        use tuff_vanilla_data::item::EquipmentSlot;
        if damage.is("bypasses_wolf_armor") {
            return false;
        }
        let Some(armor) = core
            .equipment
            .get_mut(EquipmentSlot::Body)
            .filter(|stack| stack.item() == tuff_vanilla_data::Item::WolfArmor)
        else {
            return false;
        };
        let before = armor_tier(Some(armor));
        let worn = armor.damage() + amount.ceil() as u32;
        let broke = armor.max_damage().is_some_and(|max| worn >= max);
        if broke {
            if let Some(armor) = core.equipment.take(EquipmentSlot::Body) {
                armor.burn(tuff_item::item::BurnReason::Broken);
            }
            core.entity_event(BODY_BREAK_EVENT, cx);
        } else {
            armor.set_damage(worn);
        }
        let after = armor_tier(core.equipment.get(EquipmentSlot::Body));
        if before != after {
            core.sound(
                tuff_vanilla_data::registry::SoundEvent::ItemWolfArmorCrack,
                1.0,
                1.0,
                cx,
            );
            scute_particles(core, cx);
        }
        true
    }

    fn can_shear_equipment(
        &self,
        core: &MobCore,
        interaction: &tuff_core::world::entity::EntityInteraction,
    ) -> bool {
        core.tame
            .as_ref()
            .is_some_and(|t| t.owner == Some(interaction.player_uuid))
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self {
            variants: Variants::default_for(
                crate::pack_variants::wolf(),
                crate::pack_variants::wolf_sounds(),
            ),
            collar: DyeColor::Red,
            wet: false,
            shaking: false,
            shake_anim: 0.0,
            anger_end: -1,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.make_tameable(data::SPECIES);
        core.set_attribute_base(Attribute::MaxHealth, UNTAMED_MAX_HEALTH);
        let mut goals = GoalSelector::default();
        goals.add(1, super::goals::Float::new(core));
        goals.add(1, Panic::new(1.5));
        goals.add(2, Sit);
        goals.add(
            3,
            AvoidPlayer::entities(&[EntityType::Llama], 24.0, 1.5, 1.5).only_if(wild),
        );
        goals.add(4, LeapAtTarget::new(0.4));
        goals.add(5, MeleeAttack::new(1.0, true));
        goals.add(6, FollowOwner::new(1.0));
        goals.add(7, Breed::new(1.0));
        goals.add(8, RandomStroll::water_avoiding(1.0));
        goals.add(9, super::goals::Beg::new(8.0, wolf_food));
        goals.add(10, LookAtPlayer::new(8.0));
        goals.add(10, RandomLookAround::default());
        goals.add(1, OwnerTarget::hurt_by_attacker());
        goals.add(2, OwnerTarget::hurt_target());
        goals.add(3, HurtByTarget::new(&[]).alerting(&[EntityType::Wolf]));
        goals.add(
            4,
            NearestAttackablePlayer::with_interval(10, true, Some(grudge)),
        );
        goals.add(5, NearestAttackableEntity::new(&PREY, false).when(untamed));
        goals.add(
            6,
            NearestAttackableEntity::new(&[EntityType::Turtle], false)
                .when(untamed)
                .only(super::zombie::baby_on_land),
        );
        goals.add(7, NearestAttackableEntity::new(&SKELETONS, false));
        goals.add(8, ResetUniversalAnger::new(true));
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.variants.load(
            nbt,
            crate::pack_variants::wolf(),
            crate::pack_variants::wolf_sounds(),
        );
        if let Some(color) = take(nbt, "CollarColor", |t| {
            let id = match *t {
                Tag::Byte(v) => v as i32,
                Tag::Int(v) => v,
                _ => return None,
            };
            DyeColor::from_id(id.clamp(0, 15) as u32)
        }) {
            self.collar = color;
        }
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        self.variants.save(
            out,
            crate::pack_variants::wolf(),
            crate::pack_variants::wolf_sounds(),
        );
        out.insert("CollarColor", self.collar.id() as i8);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut values = core.tame_metadata();
        values.push((
            BEGGING,
            MetadataValue::Boolean(core.tame.as_ref().is_some_and(|t| t.begging)),
        ));
        values.push((COLLAR, MetadataValue::VarInt(self.collar.id() as i32)));
        values.push((ANGER_TIME, MetadataValue::VarLong(self.anger_end)));
        values.extend(self.variants.metadata(core, VARIANT));
        values
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let set = self
            .variants
            .sounds(crate::pack_variants::wolf_sounds(), core.is_baby());
        MobSounds {
            ambient: Some(match set.growl {
                Some(growl) if core.anger.is_some() => growl,
                _ => set.ambient,
            }),
            hurt: Some(set.hurt),
            death: Some(set.death),
            step: set.step,
            volume: 1.0,
        }
    }

    fn ambient_sound_interval(&self) -> i32 {
        120
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        core_owned_food(stack)
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.wet && !self.shaking && core.navigation.is_done() && core.base.on_ground {
            self.shaking = true;
            self.shake_anim = 0.0;
            core.entity_event(SHAKE_START_EVENT, cx);
        }
        core.update_anger(true, cx);
        self.sync_anger(core, cx.tick);
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.is_alive() {
            return;
        }
        let [x, _, z] = core.base.position;
        let eye = super::physics::block_pos([x, core.eye_position()[1], z]);
        if core.body.in_water || (cx.raining && core.can_see_sky(eye, cx)) {
            self.wet = true;
            if self.shaking {
                core.entity_event(SHAKE_CANCEL_EVENT, cx);
                self.shaking = false;
                self.shake_anim = 0.0;
            }
            return;
        }
        if self.shaking {
            if self.shake_anim == 0.0 {
                let pitch = 1.0 + (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2;
                core.sound(
                    tuff_vanilla_data::registry::SoundEvent::EntityWolfShake,
                    0.4,
                    pitch,
                    cx,
                );
            }
            let before = self.shake_anim;
            self.shake_anim += 0.05;
            if before >= 2.0 {
                self.wet = false;
                self.shaking = false;
                self.shake_anim = 0.0;
            }
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref();
        let owned = core.is_owner(interaction.player_uuid);
        if core.tame.as_ref().is_some_and(|t| t.is_tamed()) {
            if owned
                && let Some(stack) = stack
                && core_owned_food(stack)
                && core.health < core.max_health()
            {
                core.heal(HEAL_AMOUNT);
                return Some(HandEffect::Consume(1));
            }
            if let Some(stack) = stack
                && owned
                && stack.item().dye().is_some()
                && tuff_datapack::tags::is(tag::item::key::WOLF_COLLAR_DYES, stack.item())
            {
                let color = stack.item().dye()?;
                if color == self.collar {
                    return None;
                }
                self.collar = color;
                return Some(HandEffect::Consume(1));
            }
            if owned && let Some(stack) = stack {
                use tuff_vanilla_data::item::EquipmentSlot;
                let sitting = core.tame.as_ref().is_some_and(|t| t.sitting);
                let worn = core.equipment.get(EquipmentSlot::Body).is_some();
                if stack.item() == tuff_vanilla_data::Item::WolfArmor && !worn && !core.is_baby() {
                    let armor = interaction.hand.take(1)?;
                    core.equipment.set(EquipmentSlot::Body, Some(armor));
                    return Some(HandEffect::None);
                }
                if sitting
                    && let Some(armor) = core.equipment.get_mut(EquipmentSlot::Body)
                    && armor.item() == tuff_vanilla_data::Item::WolfArmor
                    && armor.damage() > 0
                    && armor.item().repairable_with().contains(&stack.item())
                {
                    let unit = (armor.max_damage().unwrap_or(0) as f32 * ARMOR_REPAIR_UNIT) as u32;
                    armor.set_damage(armor.damage().saturating_sub(unit));
                    core.sound(
                        tuff_vanilla_data::registry::SoundEvent::ItemWolfArmorRepair,
                        1.0,
                        1.0,
                        cx,
                    );
                    return Some(HandEffect::Consume(1));
                }
            }
            if owned && stack.is_none() {
                let sitting = !core.tame.as_ref().is_some_and(|t| t.sitting);
                core.set_sitting(sitting);
                core.navigation.stop();
                core.target = None;
                return Some(HandEffect::None);
            }
            return None;
        }
        if let Some(stack) = stack
            && stack.item() == tuff_vanilla_data::Item::Bone
            && core.anger.is_none()
        {
            if core.try_tame(interaction.player_uuid, TAME_CHANCE, cx) {
                core.set_attribute_base(Attribute::MaxHealth, TAMED_MAX_HEALTH);
                core.health = core.max_health();
            }
            return Some(HandEffect::Consume(1));
        }
        None
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        Self {
            variants: self.variants.inherit(
                &partner.variants,
                Variants::default_for(
                    crate::pack_variants::wolf(),
                    crate::pack_variants::wolf_sounds(),
                ),
                rng,
            ),
            collar: self.collar,
            wet: false,
            shaking: false,
            shake_anim: 0.0,
            anger_end: -1,
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.variants.finalize(
            core,
            crate::pack_variants::wolf(),
            crate::pack_variants::wolf_sounds(),
            cx,
        );
    }

    fn loot_facts(&self, facts: &mut EntityFacts) {
        facts.components.push((
            "minecraft:wolf/variant",
            crate::pack_variants::wolf()[self.variants.variant]
                .name
                .to_owned(),
        ));
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

impl Wolf {
    fn sync_anger(&mut self, core: &mut MobCore, now: u64) {
        let end = core.anger.map_or(-1, |a| now as i64 + i64::from(a.ticks));
        let stale =
            (end < 0) != (self.anger_end < 0) || (end - self.anger_end).abs() > ANGER_SYNC_SLACK;
        if stale {
            self.anger_end = end;
            core.changed(ANGER_TIME);
        }
    }
}

fn core_owned_food(stack: &ItemStack) -> bool {
    wolf_food(stack.item())
}

/// Registers the wolf's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Wolf, super::load::<Wolf>);
    registry.register_spawn_rule(EntityType::Wolf, super::spawning::wolf_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;
    use tuff_core::protocol::play::MetadataValue;

    fn interaction(owner: u128, item: Option<tuff_vanilla_data::Item>) -> EntityInteraction {
        EntityInteraction {
            player: 1,
            player_uuid: owner,
            position: [0.0; 3],
            location: [0.0; 3],
            stack: item
                .map(|item| tuff_item::item::ItemSnapshot::describe(item, 1, Default::default())),
            hand: tuff_core::world::entity::HandOffer::new(
                (item.map(|item| {
                    tuff_item::item::ItemSnapshot::describe(item, 1, Default::default())
                }))
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
    fn wolf_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Wolf.data_layout();
        assert_eq!(layout.len(), VARIANT as usize + 2);
        assert_eq!(
            layout[data::SPECIES as usize],
            MetadataValue::Byte(0).type_id() as u8
        );
        assert_eq!(
            layout[(data::SPECIES + 1) as usize],
            MetadataValue::OptionalUuid(None).type_id() as u8
        );
        assert_eq!(
            layout[ANGER_TIME as usize],
            MetadataValue::VarLong(0).type_id() as u8
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn wolf_armor_takes_hits_cracks_and_breaks() {
        use tuff_vanilla_data::damage_type;
        use tuff_vanilla_data::item::EquipmentSlot;
        with_context(|core, cx| {
            let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
            let mut wolf = Wolf::create(&mut rng);
            let hit = crate::Damage::of(&damage_type::MOB_ATTACK);
            assert!(
                !wolf.absorb_hurt(core, &hit, 4.0, cx),
                "no armor, no absorb"
            );
            let armor = ItemStack::mint(tuff_vanilla_data::Item::WolfArmor, 1, MintReason::Test);
            let max = armor.max_damage().unwrap();
            core.equipment.set(EquipmentSlot::Body, Some(armor));
            assert!(wolf.absorb_hurt(core, &hit, 3.2, cx));
            let worn = core.equipment.get(EquipmentSlot::Body).unwrap();
            assert_eq!(worn.damage(), 4);
            assert!(!cx.effects.is_empty(), "the first hit cracks it");
            cx.effects.clear();
            let bypass = crate::Damage::of(&damage_type::STARVE);
            assert!(!wolf.absorb_hurt(core, &bypass, 4.0, cx));
            assert_eq!(core.equipment.get(EquipmentSlot::Body).unwrap().damage(), 4);
            assert!(wolf.absorb_hurt(core, &hit, 10.0, cx));
            assert!(cx.effects.is_empty(), "no further tier");
            assert!(wolf.absorb_hurt(core, &hit, max as f32, cx));
            assert!(core.equipment.get(EquipmentSlot::Body).is_none());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_angry_wolf_syncs_its_anger_end_and_refuses_a_bone() {
        with_context(|core, cx| {
            let mut wolf = Wolf::create(cx.rng);
            core.anger = Some(crate::target::Anger {
                target: 5,
                ticks: 400,
                universal: false,
            });
            wolf.sync_anger(core, 1000);
            assert!(
                wolf.metadata(core)
                    .contains(&(ANGER_TIME, MetadataValue::VarLong(1400)))
            );
            let bone = interaction(1, Some(tuff_vanilla_data::Item::Bone));
            assert_eq!(wolf.interact(core, &bone, cx), None);
            core.anger = None;
            wolf.sync_anger(core, 1001);
            assert_eq!(wolf.anger_end, -1);
            assert!(wolf.interact(core, &bone, cx).is_some());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn armor_crack_tiers_use_whole_number_durability_fractions() {
        assert_eq!(armor_crack_tier(0, 64), 0);
        assert_eq!(armor_crack_tier(1, 64), 3);
        assert_eq!(armor_crack_tier(64, 64), 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_the_owner_shears_its_wolf_armor_off() {
        use tuff_core::world::entity::{Effect, RegionEntity};
        use tuff_vanilla_data::item::EquipmentSlot;
        with_context(|_, cx| {
            let base = tuff_core::world::entity::EntityBase::new(
                EntityType::Wolf,
                9,
                9,
                [0.5, 64.0, 0.5],
                [0.0; 3],
            );
            let mut wolf = crate::Mob::<Wolf>::new(base, cx.rng);
            wolf.core.try_tame(1, 1.0, cx);
            let armor = ItemStack::mint(tuff_vanilla_data::Item::WolfArmor, 1, MintReason::Test);
            wolf.core.equipment.set(EquipmentSlot::Body, Some(armor));
            let shears = Some(tuff_vanilla_data::Item::Shears);
            assert_eq!(wolf.interact(&interaction(2, shears), cx), None);
            assert!(wolf.core.equipment.get(EquipmentSlot::Body).is_some());
            cx.effects.clear();
            assert_eq!(
                wolf.interact(&interaction(1, shears), cx),
                Some(HandEffect::Damage(1))
            );
            assert!(wolf.core.equipment.get(EquipmentSlot::Body).is_none());
            let dropped = cx.effects.iter().any(|e| {
                matches!(e, Effect::DropItem { stack, .. } if stack.item() == tuff_vanilla_data::Item::WolfArmor)
            });
            let reported = cx.effects.iter().any(|e| {
                matches!(e, Effect::Advancement { player: 1, event }
                    if matches!(**event, tuff_core::world::messages::AdvancementEvent::ShearedEquipment { .. }))
            });
            assert!(dropped && reported);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bone_tames_a_wolf_and_a_fed_bone_can_still_fail() {
        with_context(|core, cx| {
            Wolf::configure(core);
            let mut wolf = Wolf::create(cx.rng);
            // Deterministic: seed_from_u64(1) rolls a known sequence: keep
            // interacting with bones until one succeeds or we give up.
            let mut tamed = false;
            for _ in 0..50 {
                let effect = wolf.interact(
                    core,
                    &interaction(1, Some(tuff_vanilla_data::Item::Bone)),
                    cx,
                );
                assert!(matches!(effect, Some(HandEffect::Consume(1))));
                if core.tame.as_ref().unwrap().is_tamed() {
                    tamed = true;
                    break;
                }
            }
            assert!(tamed, "1/3 chance should succeed within 50 attempts");
            assert!(core.tame.as_ref().unwrap().sitting);
            assert_eq!(core.max_health() as f64, TAMED_MAX_HEALTH);
            assert_eq!(core.health, core.max_health());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_owner_can_sit_and_heal_a_tamed_wolf() {
        with_context(|core, cx| {
            Wolf::configure(core);
            let mut wolf = Wolf::create(cx.rng);
            core.try_tame(1, 1.0, cx);
            core.set_attribute_base(Attribute::MaxHealth, TAMED_MAX_HEALTH);
            core.health = 10.0;

            let effect = wolf.interact(
                core,
                &interaction(1, Some(tuff_vanilla_data::Item::CookedBeef)),
                cx,
            );
            assert!(matches!(effect, Some(HandEffect::Consume(1))));
            assert!(core.health > 10.0, "cooked beef heals a hurt tamed wolf");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_owner_dyes_the_collar_and_equips_then_repairs_armor() {
        use tuff_vanilla_data::Item;
        use tuff_vanilla_data::item::EquipmentSlot;
        with_context(|core, cx| {
            Wolf::configure(core);
            let mut wolf = Wolf::create(cx.rng);
            core.try_tame(1, 1.0, cx);
            assert!(
                wolf.interact(core, &interaction(2, Some(Item::BlueDye)), cx)
                    .is_none()
            );
            let dyed = wolf.interact(core, &interaction(1, Some(Item::BlueDye)), cx);
            assert!(matches!(dyed, Some(HandEffect::Consume(1))));
            assert_eq!(wolf.collar, DyeColor::Blue);
            assert!(
                wolf.interact(core, &interaction(1, Some(Item::BlueDye)), cx)
                    .is_none()
            );

            let worn = wolf.interact(core, &interaction(1, Some(Item::WolfArmor)), cx);
            assert!(matches!(worn, Some(HandEffect::None)));
            assert!(core.equipment.get(EquipmentSlot::Body).is_some());

            core.equipment
                .get_mut(EquipmentSlot::Body)
                .unwrap()
                .set_damage(40);
            core.set_sitting(false);
            let scute = Some(Item::ArmadilloScute);
            assert!(
                wolf.interact(core, &interaction(1, scute), cx)
                    .is_none_or(|e| !matches!(e, HandEffect::Consume(_)))
            );
            core.set_sitting(true);
            let repaired = wolf.interact(core, &interaction(1, scute), cx);
            assert!(matches!(repaired, Some(HandEffect::Consume(1))));
            assert!(core.equipment.get(EquipmentSlot::Body).unwrap().damage() < 40);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_wet_wolf_shakes_once_it_stands_still_on_the_ground_then_dries() {
        with_context(|core, cx| {
            Wolf::configure(core);
            let mut wolf = Wolf::create(cx.rng);
            core.base.on_ground = true;
            wolf.wet = true;
            wolf.ai_step(core, cx);
            assert!(wolf.shaking);
            for _ in 0..50 {
                wolf.tick(core, cx);
            }
            assert!(!wolf.wet && !wolf.shaking);
            core.body.in_water = true;
            wolf.wet = false;
            wolf.shaking = true;
            wolf.tick(core, cx);
            assert!(wolf.wet && !wolf.shaking);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_the_owner_can_command_a_tamed_wolf_to_sit() {
        with_context(|core, cx| {
            Wolf::configure(core);
            let mut wolf = Wolf::create(cx.rng);
            core.try_tame(1, 1.0, cx);
            assert!(core.tame.as_ref().unwrap().sitting);

            let effect = wolf.interact(core, &interaction(2, None), cx);
            assert!(effect.is_none(), "a stranger cannot command it");
            assert!(core.tame.as_ref().unwrap().sitting);

            let effect = wolf.interact(core, &interaction(1, None), cx);
            assert!(matches!(effect, Some(HandEffect::None)));
            assert!(!core.tame.as_ref().unwrap().sitting, "the owner toggles it");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_wolf_begs_at_a_player_holding_a_bone_and_stops_when_it_is_put_away() {
        use crate::goal::Goal;
        let ground = &|_pos: tuff_world_model::BlockPos| None;
        let mut holder = owner(None, None);
        holder.held[0] = Some(tuff_vanilla_data::Item::Bone);
        let holders = [holder];
        crate::tests::with_players(ground, &[], &holders, |core, cx| {
            Wolf::configure(core);
            let mut beg = crate::goals::Beg::new(8.0, wolf_food);
            assert!(beg.can_use(core, cx));
            beg.start(core, cx);
            assert!(core.tame.as_ref().unwrap().begging);
            assert!(beg.can_continue(core, cx));
            beg.stop(core, cx);
            assert!(!core.tame.as_ref().unwrap().begging);
        });
        let empty = [owner(None, None)];
        crate::tests::with_players(ground, &[], &empty, |core, cx| {
            Wolf::configure(core);
            let mut beg = crate::goals::Beg::new(8.0, wolf_food);
            assert!(!beg.can_use(core, cx));
        });
    }

    fn owner(
        last_hurt_by: Option<(i32, u64)>,
        last_hurt_mob: Option<(i32, u64)>,
    ) -> tuff_core::world::entity::NearbyPlayer<'static> {
        tuff_core::world::entity::NearbyPlayer {
            id: 1,
            uuid: 1,
            name: "",
            position: [0.5, 64.0, 0.5],
            eye_height: 1.62,
            yaw: 0.0,
            pitch: 0.0,
            held: [None; 2],
            creative: false,
            height: 1.8,
            sneaking: false,
            invisible: false,
            invulnerable: false,
            armor: [None; 4],
            effects: 0,
            movement: [0.0; 3],
            vehicle: None,
            sleeping: None,
            last_hurt_by,
            last_hurt_mob,
            perchable: false,
            sprinting: false,
        }
    }

    fn creature(id: i32, kind: EntityType) -> tuff_core::world::entity::EntitySummary {
        tuff_core::world::entity::EntitySummary {
            id,
            uuid: id as u128,
            kind,
            position: [3.5, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [3.2, 64.0, 0.2, 3.8, 65.9, 0.8],
            living: Some(tuff_core::world::entity::LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 1.6,
                in_water: false,
                seen_as_enemy: true,
                enemy: true,
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
    fn a_sitting_wolf_stays_down_only_on_dry_ground_and_gets_up_for_a_hurt_owner() {
        use crate::goal::Goal;
        let calm = [owner(None, None)];
        let hurt = [owner(Some((50, 5)), None)];
        let ground = &|_pos: tuff_world_model::BlockPos| None;
        crate::tests::with_players(ground, &[], &calm, |core, cx| {
            Wolf::configure(core);
            core.tame = Some(crate::animal::Tame {
                owner: Some(1),
                sitting: true,
                ..Default::default()
            });
            core.base.on_ground = true;
            assert!(Sit.can_use(core, cx));
            core.base.on_ground = false;
            assert!(!Sit.can_use(core, cx), "no sitting in mid-air");
            core.base.on_ground = true;
            core.body.in_water = true;
            assert!(!Sit.can_use(core, cx), "no sitting in water");
            core.body.in_water = false;
            core.set_sitting(false);
            assert!(!Sit.can_use(core, cx));
        });
        crate::tests::with_players(ground, &[], &hurt, |core, cx| {
            Wolf::configure(core);
            core.tame = Some(crate::animal::Tame {
                owner: Some(1),
                sitting: true,
                ..Default::default()
            });
            core.base.on_ground = true;
            assert!(!Sit.can_use(core, cx), "it gets up to help a hurt owner");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_untamed_wolf_hunts_prey_and_baby_turtles_but_a_tamed_one_does_not() {
        use crate::goal::Goal;
        let mut baby = creature(53, EntityType::Turtle);
        baby.living.as_mut().unwrap().age = -100;
        let mut adult = creature(52, EntityType::Turtle);
        adult.living.as_mut().unwrap().age = 0;
        let world = [creature(51, EntityType::Sheep), adult, baby];
        let ground = &|_pos: tuff_world_model::BlockPos| None;
        crate::tests::with_players(ground, &world, &[], |core, cx| {
            Wolf::configure(core);
            let hunt = |core: &mut MobCore, cx: &mut TickContext| {
                let mut goal = NearestAttackableEntity::new(&PREY, false).when(untamed);
                (0..400).any(|_| goal.can_use(core, cx))
            };
            assert!(hunt(core, cx));
            let turtles = &[EntityType::Turtle];
            let mut goal = NearestAttackableEntity::new(turtles, false)
                .when(untamed)
                .only(super::super::zombie::baby_on_land);
            assert!((0..400).any(|_| goal.can_use(core, cx)));
            goal.start(core, cx);
            assert_eq!(core.target, Some(53), "only the baby turtle is prey");
            core.target = None;
            core.tame = Some(crate::animal::Tame {
                owner: Some(1),
                ..Default::default()
            });
            assert!(!hunt(core, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_untamed_cat_pounces_on_a_rabbit_within_reach_once_a_second() {
        use crate::attack::OcelotAttack;
        use crate::goal::Goal;
        let world = [creature(51, EntityType::Rabbit)];
        let ground = &|_pos: tuff_world_model::BlockPos| None;
        crate::tests::with_players(ground, &world, &[], |core, cx| {
            crate::cat::Cat::configure(core);
            core.base.position = [3.0, 64.0, 0.5];
            core.target = Some(51);
            let mut goal = OcelotAttack::default();
            assert!(goal.can_use(core, cx));
            let before = cx.effects.len();
            goal.tick(core, cx);
            let bitten = cx.effects.len() - before;
            assert!(bitten > 0, "it bites on the first tick");
            goal.tick(core, cx);
            assert_eq!(cx.effects.len() - before, bitten, "then waits 20 ticks");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_tamed_wolf_defends_its_owner_and_assists_its_attacks() {
        use crate::goal::Goal;
        let zombie = creature(50, EntityType::Zombie);
        let creeper = creature(51, EntityType::Creeper);
        let world = [zombie, creeper];
        let hurt = [owner(Some((50, 5)), None)];
        crate::tests::with_players(
            &|pos| {
                Some(if pos.y < 64 {
                    tuff_vanilla_data::Block::GrassBlock.default_state()
                } else {
                    tuff_vanilla_data::Block::Air.default_state()
                })
            },
            &world,
            &hurt,
            |core, cx| {
                Wolf::configure(core);
                let mut goal = OwnerTarget::hurt_by_attacker();
                assert!(!goal.can_use(core, cx), "an untamed wolf stays out of it");
                core.tame = Some(crate::animal::Tame {
                    owner: Some(1),
                    ..Default::default()
                });
                assert!(goal.can_use(core, cx));
                goal.start(core, cx);
                assert_eq!(core.target, Some(50));
                goal.stop(core, cx);
                assert!(!goal.can_use(core, cx), "the same hurt is answered once");
                core.set_sitting(true);
                assert!(!OwnerTarget::hurt_by_attacker().can_use(core, cx));
            },
        );
        let attacked = [owner(None, Some((51, 2)))];
        crate::tests::with_players(
            &|pos| {
                Some(if pos.y < 64 {
                    tuff_vanilla_data::Block::GrassBlock.default_state()
                } else {
                    tuff_vanilla_data::Block::Air.default_state()
                })
            },
            &world,
            &attacked,
            |core, cx| {
                Wolf::configure(core);
                core.tame = Some(crate::animal::Tame {
                    owner: Some(1),
                    ..Default::default()
                });
                assert!(
                    !OwnerTarget::hurt_target().can_use(core, cx),
                    "never creepers"
                );
            },
        );
        let attacked = [owner(None, Some((50, 2)))];
        crate::tests::with_players(
            &|pos| {
                Some(if pos.y < 64 {
                    tuff_vanilla_data::Block::GrassBlock.default_state()
                } else {
                    tuff_vanilla_data::Block::Air.default_state()
                })
            },
            &world,
            &attacked,
            |core, cx| {
                Wolf::configure(core);
                core.tame = Some(crate::animal::Tame {
                    owner: Some(1),
                    ..Default::default()
                });
                let mut goal = OwnerTarget::hurt_target();
                assert!(goal.can_use(core, cx));
                goal.start(core, cx);
                assert_eq!(core.target, Some(50));
            },
        );
    }
}
