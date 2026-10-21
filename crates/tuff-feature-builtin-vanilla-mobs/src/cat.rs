//! Cats: tamed by feeding, sat by their owner, and otherwise a plain
//! (if skittish) breedable animal -- see `wolf.rs` for the shared taming
//! machinery this reuses. A tamed cat curls up on its owner's bed while the
//! owner sleeps and, at dawn, may leave a gift.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::item::DyeColor;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::tag;

use super::animal::untamed;
use super::attack::{LeapAtTarget, OcelotAttack};
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{
    AvoidPlayer, Breed, FollowOwner, LookAtPlayer, MoveToBlock, Panic, RandomLookAround,
    RandomStroll, Sit, Tempt,
};
use super::target::NearestAttackableEntity;
use super::voiced_variant::Variants;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::drops::EntityFacts;
use tuff_core::world::entity::{Effect, EntityInteraction, InteractOutcome, TickContext, take};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const TAME_CHANCE: f64 = 1.0 / 3.0;
const HEAL_AMOUNT: f32 = 1.0;

const VARIANT: u8 = data::SPECIES + 2;
const LYING: u8 = data::SPECIES + 3;
const RELAXED: u8 = data::SPECIES + 4;
const COLLAR: u8 = data::SPECIES + 5;
const SOUND_VARIANT: u8 = data::SPECIES + 6;

fn cat_food(item: tuff_vanilla_data::Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::CAT_FOOD, item)
}

#[derive(Debug)]
pub struct Cat {
    variants: Variants,
    collar: DyeColor,
    /// The structure a cat placed by world generation belongs to, until it
    /// is set up as a fresh spawn.
    spawn_structure: Option<String>,
}

pub const SPAWN_STRUCTURE: &str = "tuff:spawn_structure";

const SLEEP_DEEP_TICKS: u32 = 100;
const BED_SETTLE_TICKS: i32 = 16;
const GIFT_ATTRIBUTE: &str = "minecraft:gameplay/cat_waking_up_gift_chance";

fn set_lying(core: &mut MobCore, lying: bool) {
    if core.lying != lying {
        core.lying = lying;
        core.changed(LYING);
    }
}

fn set_relaxed(core: &mut MobCore, relaxed: bool) {
    if core.relaxed != relaxed {
        core.relaxed = relaxed;
        core.changed(RELAXED);
    }
}

fn gift_chance(cx: &TickContext) -> f32 {
    let clocks = cx.shared.clocks.borrow();
    let Some(id) = tuff_core::world::clock::clock_id("minecraft:overworld") else {
        return 0.0;
    };
    let total = clocks.get(id).total_ticks;
    tuff_core::world::clock::timelines_of("minecraft:overworld")
        .filter_map(|timeline| timeline.sample(GIFT_ATTRIBUTE, total)?.as_f64())
        .fold(0.0, |best, chance| best.max(chance as f32))
}

impl MobCore {
    /// Moves to the first spot at or below `target` where it fits and has
    /// footing, unless it is a hazard cats avoid.
    fn teleport_to_standing(&mut self, target: [f64; 3], cx: &mut TickContext) -> bool {
        let half = f64::from(self.size.width) / 2.0;
        let height = f64::from(self.size.height);
        let area_at = |y: f64| {
            [
                target[0] - half,
                y,
                target[2] - half,
                target[0] + half,
                y + height,
                target[2] + half,
            ]
        };
        let mut y = target[1];
        for _ in 0..16 {
            if !super::physics::no_collision(area_at(y - 1.0), cx.block) {
                break;
            }
            y -= 1.0;
        }
        let area = area_at(y);
        if !super::physics::no_collision(area, cx.block)
            || super::physics::no_collision(area_at(y - 0.5), cx.block)
        {
            return false;
        }
        let min = super::physics::block_pos([area[0], area[1] - 1.0, area[2]]);
        let max = super::physics::block_pos([area[3], area[4], area[5]]);
        for x in min.x..=max.x {
            for by in min.y..=max.y {
                for z in min.z..=max.z {
                    let Some(state) = (cx.block)(BlockPos::new(x, by, z)) else {
                        return false;
                    };
                    if tuff_datapack::tags::is(
                        tag::block::key::CAT_DOES_NOT_TELEPORT_TO,
                        state.block(),
                    ) {
                        return false;
                    }
                }
            }
        }
        self.base.position = [target[0], y, target[2]];
        self.base.velocity = [0.0; 3];
        self.navigation.stop();
        self.impulse = true;
        true
    }
}

#[derive(Debug)]
struct LieOnBlock(MoveToBlock);

impl LieOnBlock {
    fn new() -> Self {
        let mut seek = MoveToBlock::new(1.1, 8, 6, |_, pos, cx| {
            let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
            (cx.block)(above).is_none_or(tuff_world_model::chunk::is_air)
                && (cx.block)(pos).is_some_and(|state| {
                    tuff_datapack::tags::is(tag::block::key::CATS_CAN_LIE_ON, state.block())
                })
        });
        seek.vertical_start = -2;
        seek.search_every = Some(40);
        Self(seek)
    }
}

impl Goal for LieOnBlock {
    fn flags(&self) -> Flags {
        self.0.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.tame
            .as_ref()
            .is_some_and(|t| t.is_tamed() && !t.sitting)
            && !mob.lying
            && self.0.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.0.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.start(mob, cx);
        mob.set_sitting(false);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        set_lying(mob, false);
    }

    fn every_tick(&self) -> bool {
        self.0.every_tick()
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.tick(mob, cx);
        mob.set_sitting(false);
        set_lying(mob, self.0.reached());
    }
}

#[derive(Debug, Default)]
struct RelaxOnOwner {
    owner: Option<u128>,
    goal: Option<BlockPos>,
    on_bed: i32,
    owner_slept: u32,
}

impl RelaxOnOwner {
    fn owner_asleep<'a>(
        mob: &MobCore,
        cx: &'a TickContext,
    ) -> Option<&'a tuff_core::world::entity::NearbyPlayer<'a>> {
        let owner = mob.tame.as_ref()?.owner?;
        cx.players
            .iter()
            .find(|p| p.uuid == owner && p.sleeping.is_some())
    }

    fn space_occupied(mob: &MobCore, goal: BlockPos, cx: &TickContext) -> bool {
        let area = super::physics::inflate(
            [
                goal.x as f64,
                goal.y as f64,
                goal.z as f64,
                goal.x as f64 + 1.0,
                goal.y as f64 + 1.0,
                goal.z as f64 + 1.0,
            ],
            [1.0; 3],
        );
        cx.entities.iter().any(|e| {
            e.id != mob.base.id
                && e.kind == EntityType::Cat
                && super::physics::intersects(area, e.bounds)
        })
    }

    fn goal_for(player_pos: [f64; 3], cx: &TickContext) -> Option<BlockPos> {
        let at = super::physics::block_pos(player_pos);
        let state = (cx.block)(at)?;
        if !tuff_datapack::tags::is(tag::block::key::CATS_CAN_LIE_ON, state.block()) {
            return None;
        }
        Some(match state.get("facing") {
            Some("north") => BlockPos::new(at.x, at.y, at.z + 1),
            Some("south") => BlockPos::new(at.x, at.y, at.z - 1),
            Some("west") => BlockPos::new(at.x + 1, at.y, at.z),
            Some("east") => BlockPos::new(at.x - 1, at.y, at.z),
            _ => at,
        })
    }

    fn give_morning_gift(mob: &mut MobCore, cx: &mut TickContext) {
        let from = super::physics::block_pos(mob.base.position);
        let target = [
            f64::from(from.x + cx.rng.gen_range(0..11) - 5) + 0.5,
            f64::from(from.y + cx.rng.gen_range(0..5) - 2),
            f64::from(from.z + cx.rng.gen_range(0..11) - 5) + 0.5,
        ];
        mob.teleport_to_standing(target, cx);
        let table = "minecraft:gameplay/cat_morning_gift";
        let yaw = f64::from(mob.body_yaw).to_radians();
        let [x, y, z] = super::physics::block_pos(mob.base.position)
            .center()
            .map(|c| c.floor());
        for stack in cx.shared.block_loot().roll(table, cx.rng) {
            cx.effects.push(Effect::DropItem {
                stack,
                position: [x + 0.5 - yaw.sin(), y, z + 0.5 + yaw.cos()],
                velocity: [0.0; 3],
                pickup_delay: 10,
            });
        }
    }
}

impl Goal for RelaxOnOwner {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !mob
            .tame
            .as_ref()
            .is_some_and(|t| t.is_tamed() && !t.sitting)
        {
            return false;
        }
        let Some(owner) = Self::owner_asleep(mob, cx) else {
            return false;
        };
        if tuff_core::world::distance_sq(mob.base.position, owner.position) > 100.0 {
            return false;
        }
        let Some(goal) = Self::goal_for(owner.position, cx) else {
            return false;
        };
        if Self::space_occupied(mob, goal, cx) {
            return false;
        }
        self.owner = Some(owner.uuid);
        self.goal = Some(goal);
        true
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(goal) = self.goal else {
            return false;
        };
        mob.tame
            .as_ref()
            .is_some_and(|t| t.is_tamed() && !t.sitting)
            && Self::owner_asleep(mob, cx).is_some()
            && !Self::space_occupied(mob, goal, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.set_sitting(false);
        if let Some(goal) = self.goal {
            mob.move_to_position(goal.center(), 1.1, cx);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        set_lying(mob, false);
        if self.owner_slept >= SLEEP_DEEP_TICKS && cx.rng.r#gen::<f32>() < gift_chance(cx) {
            Self::give_morning_gift(mob, cx);
        }
        self.on_bed = 0;
        self.owner_slept = 0;
        set_relaxed(mob, false);
        mob.navigation.stop();
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(goal) = self.goal else {
            return;
        };
        let Some(owner) = Self::owner_asleep(mob, cx) else {
            return;
        };
        let (owner_pos, slept) = (owner.position, owner.sleeping.unwrap_or(0));
        self.owner_slept = slept;
        mob.set_sitting(false);
        if mob.navigation.is_done() {
            mob.move_to_position(goal.center(), 1.1, cx);
        }
        if tuff_core::world::distance_sq(mob.base.position, owner_pos) < 2.5 {
            self.on_bed += 1;
            if self.on_bed > super::goal::reduced_delay(BED_SETTLE_TICKS) {
                set_lying(mob, true);
                set_relaxed(mob, false);
            } else {
                mob.look.set_look_at(owner_pos, 45.0, 45.0);
                set_relaxed(mob, true);
            }
        } else {
            set_lying(mob, false);
        }
    }
}

fn is_untamed(mob: &MobCore) -> bool {
    mob.tame.as_ref().is_some_and(|t| !t.is_tamed())
}

fn sit_spot(mob: &MobCore, pos: BlockPos, cx: &TickContext) -> bool {
    let tamed_and_free = mob
        .tame
        .as_ref()
        .is_some_and(|t| t.is_tamed() && !t.sitting);
    if !tamed_and_free {
        return false;
    }
    if !(cx.block)(BlockPos::new(pos.x, pos.y + 1, pos.z))
        .is_some_and(|s| s.block() == tuff_vanilla_data::Block::Air)
    {
        return false;
    }
    let Some(state) = (cx.block)(pos) else {
        return false;
    };
    let block = state.block();
    if !tuff_datapack::tags::is(tag::block::key::CATS_CAN_SIT_ON, block) {
        return false;
    }
    if block == tuff_vanilla_data::Block::Furnace {
        return state.get("lit") == Some("true");
    }
    if tuff_datapack::tags::is(tag::block::key::BEDS, block) {
        return state.get("part") != Some("head");
    }
    true
}

/// A tamed cat not ordered to sit walks to a chest, lit furnace or bed
/// nearby and sits on it.
#[derive(Debug)]
struct SitOnBlock(MoveToBlock);

impl SitOnBlock {
    fn new() -> Self {
        Self(MoveToBlock::new(0.8, 8, 1, sit_spot))
    }
}

impl Goal for SitOnBlock {
    fn flags(&self) -> Flags {
        self.0.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.0.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.0.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.start(mob, cx);
        mob.set_sitting_pose(false);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.set_sitting_pose(false);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.tick(mob, cx);
        mob.set_sitting_pose(self.0.reached());
    }
}

impl Species for Cat {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            variants: Variants::default_for(
                crate::pack_variants::cat(),
                crate::pack_variants::cat_sounds(),
            ),
            collar: DyeColor::Red,
            spawn_structure: None,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.make_tameable(data::SPECIES);
        let mut goals = GoalSelector::default();
        goals.add(0, super::goals::Float::new(core));
        goals.add(1, Sit);
        goals.add(2, Panic::new(1.5));
        goals.add(3, Tempt::new(0.6, cat_food));
        goals.add(3, RelaxOnOwner::default());
        goals.add(4, Breed::new(0.8));
        goals.add(4, AvoidPlayer::new(16.0, 0.8, 1.33).only_if(is_untamed));
        goals.add(5, LieOnBlock::new());
        goals.add(5, FollowOwner::with_distances(1.0, 10.0, 5.0));
        goals.add(7, SitOnBlock::new());
        goals.add(6, RandomStroll::water_avoiding(0.8));
        goals.add(7, LookAtPlayer::new(10.0));
        goals.add(8, RandomLookAround::default());
        goals.add(8, LeapAtTarget::new(0.3));
        goals.add(9, OcelotAttack::default());
        goals.add(
            1,
            NearestAttackableEntity::new(&[EntityType::Rabbit], false).when(untamed),
        );
        goals.add(
            1,
            NearestAttackableEntity::new(&[EntityType::Turtle], false)
                .when(untamed)
                .only(super::zombie::baby_on_land),
        );
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.variants.load(
            nbt,
            crate::pack_variants::cat(),
            crate::pack_variants::cat_sounds(),
        );
        self.spawn_structure = take(nbt, SPAWN_STRUCTURE, |t| Some(t.as_str()?.to_owned()));
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
            crate::pack_variants::cat(),
            crate::pack_variants::cat_sounds(),
        );
        out.insert("CollarColor", self.collar.id() as i8);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut values = core.tame_metadata();
        values.push(self.variants.variant_metadata(core, VARIANT));
        values.push((LYING, MetadataValue::Boolean(core.lying)));
        values.push((RELAXED, MetadataValue::Boolean(core.relaxed)));
        values.push((COLLAR, MetadataValue::VarInt(self.collar.id() as i32)));
        values.push(self.variants.sound_metadata(core, SOUND_VARIANT));
        values
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let set = self
            .variants
            .sounds(crate::pack_variants::cat_sounds(), core.is_baby());
        MobSounds {
            ambient: Some(set.ambient),
            hurt: Some(set.hurt),
            death: Some(set.death),
            step: set.step,
            volume: 1.0,
        }
    }

    fn eating_sound(&self, core: &MobCore) -> Option<tuff_vanilla_data::registry::SoundEvent> {
        self.variants
            .sounds(crate::pack_variants::cat_sounds(), core.is_baby())
            .eat
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if (core.lying || core.relaxed)
            && cx.tick.is_multiple_of(5)
            && let Some(purr) = self
                .variants
                .sounds(crate::pack_variants::cat_sounds(), core.is_baby())
                .purr
        {
            let volume = 0.6 + 0.4 * (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>());
            core.sound(purr, volume, 1.0, cx);
        }
    }

    fn ambient_sound_interval(&self) -> i32 {
        120
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        cat_food(stack.item)
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
            if !owned {
                return None;
            }
            if let Some(stack) = stack
                && cat_food(stack.item)
                && core.health < core.max_health()
            {
                core.heal(HEAL_AMOUNT);
                return Some(HandEffect::Consume(1));
            }
            if stack.is_none() {
                let sitting = !core.tame.as_ref().is_some_and(|t| t.sitting);
                core.set_sitting(sitting);
                return Some(HandEffect::None);
            }
            return None;
        }
        if let Some(stack) = stack
            && cat_food(stack.item)
        {
            core.try_tame(interaction.player_uuid, TAME_CHANCE, cx);
            return Some(HandEffect::Consume(1));
        }
        None
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        Self {
            variants: self.variants.inherit(
                &partner.variants,
                Variants::default_for(
                    crate::pack_variants::cat(),
                    crate::pack_variants::cat_sounds(),
                ),
                rng,
            ),
            collar: self.collar,
            spawn_structure: None,
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let structure = self.spawn_structure.take();
        self.variants.finalize_at(
            core,
            crate::pack_variants::cat(),
            crate::pack_variants::cat_sounds(),
            structure.as_deref(),
            cx,
        );
    }

    fn loot_facts(&self, facts: &mut EntityFacts) {
        facts.components.push((
            "minecraft:cat/variant",
            crate::pack_variants::cat()[self.variants.variant]
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

/// Registers the cat's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Cat, super::load::<Cat>);
    registry.register_spawn_rule(EntityType::Cat, super::spawning::animal_rule);
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
    fn cat_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Cat.data_layout();
        assert_eq!(layout.len(), SOUND_VARIANT as usize + 1);
        assert_eq!(
            layout[data::SPECIES as usize],
            MetadataValue::Byte(0).type_id() as u8
        );
        assert_eq!(
            layout[(data::SPECIES + 1) as usize],
            MetadataValue::OptionalUuid(None).type_id() as u8
        );
        assert_eq!(
            layout[LYING as usize],
            MetadataValue::Boolean(false).type_id() as u8
        );
        assert_eq!(
            layout[RELAXED as usize],
            MetadataValue::Boolean(false).type_id() as u8
        );
        assert_eq!(
            layout[COLLAR as usize],
            MetadataValue::VarInt(0).type_id() as u8
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn cat_food_tames_it_and_it_can_then_sit_or_heal() {
        with_context(|core, cx| {
            Cat::configure(core);
            let mut cat = Cat::create(cx.rng);
            let mut tamed = false;
            for _ in 0..50 {
                let effect = cat.interact(
                    core,
                    &interaction(1, Some(tuff_vanilla_data::Item::Cod)),
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

            core.health = core.max_health() - 5.0;
            let effect = cat.interact(
                core,
                &interaction(1, Some(tuff_vanilla_data::Item::Cod)),
                cx,
            );
            assert!(matches!(effect, Some(HandEffect::Consume(1))));
            assert!(core.health > core.max_health() - 5.0);

            let effect = cat.interact(core, &interaction(2, None), cx);
            assert!(effect.is_none(), "a stranger gets no response");
        });
    }
}

#[cfg(test)]
mod pet_death_tests {
    use super::*;
    use crate::tests::with_context;
    use crate::{Damage, instance::Mob};
    use tuff_core::world::entity::{Effect, EntityBase};

    #[tuff_core::watchdog]
    #[test]
    fn a_tamed_cat_reports_its_death_to_its_owner() {
        with_context(|_, cx| {
            let base = EntityBase::new(EntityType::Cat, 7, 7, [0.5, 64.0, 0.5], [0.0; 3]);
            let mut cat = Mob::<Cat>::new(base, cx.rng);
            cat.core.tame = Some(crate::animal::Tame {
                owner: Some(42),
                ..Default::default()
            });
            let damage = Damage {
                player: Some((3, 99)),
                ..Damage::of(&tuff_vanilla_data::damage_type::PLAYER_ATTACK)
            };
            cat.hurt_by(&damage, 1000.0, cx);
            assert!(cx.effects.iter().any(|e| matches!(
                e,
                Effect::PetDied {
                    owner: 42,
                    killer_player: Some(99),
                    ..
                }
            )));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_a_sitting_cat_keeps_a_chest_shut() {
        use tuff_core::world::entity::RegionEntity;
        with_context(|_, cx| {
            let base = EntityBase::new(EntityType::Cat, 7, 7, [0.5, 64.0, 0.5], [0.0; 3]);
            let mut cat = Mob::<Cat>::new(base, cx.rng);
            cat.core.tame = Some(crate::animal::Tame::default());
            assert!(!cat.sits_on_chests());
            cat.core.set_sitting(true);
            assert!(!cat.sits_on_chests(), "ordered but not yet in the pose");
            cat.core.set_sitting_pose(true);
            assert!(cat.sits_on_chests());
        });
    }
}

#[cfg(test)]
mod sit_on_block_tests {
    use super::*;
    use crate::tests::with_world;
    use tuff_vanilla_data::Block;

    fn world(spot: Block) -> impl Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        move |pos| {
            Some(if pos.y < 64 {
                Block::GrassBlock.default_state()
            } else if pos == BlockPos::new(2, 64, 0) {
                spot.default_state()
            } else {
                Block::Air.default_state()
            })
        }
    }

    fn tamed(core: &mut MobCore, sitting: bool) {
        core.tame = Some(crate::animal::Tame {
            owner: Some(1),
            sitting,
            ..Default::default()
        });
        core.base.position = [0.5, 64.0, 0.5];
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_free_tamed_cat_accepts_a_chest_but_not_stone() {
        for (spot, expected) in [(Block::Chest, true), (Block::Stone, false)] {
            with_world(&world(spot), &[], |core, cx| {
                tamed(core, false);
                assert_eq!(sit_spot(core, BlockPos::new(2, 64, 0), cx), expected);
            });
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cat_ordered_to_sit_or_untamed_does_not_use_spots() {
        with_world(&world(Block::Chest), &[], |core, cx| {
            tamed(core, true);
            assert!(!sit_spot(core, BlockPos::new(2, 64, 0), cx));
            core.tame = Some(crate::animal::Tame::default());
            assert!(!sit_spot(core, BlockPos::new(2, 64, 0), cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_unlit_furnace_is_not_a_spot() {
        with_world(&world(Block::Furnace), &[], |core, cx| {
            tamed(core, false);
            let lit = Block::Furnace.default_state();
            assert_eq!(
                sit_spot(core, BlockPos::new(2, 64, 0), cx),
                lit.get("lit") == Some("true")
            );
        });
    }
}

#[cfg(test)]
mod relax_tests {
    use super::*;
    use crate::tests::with_players;
    use tuff_core::world::entity::NearbyPlayer;
    use tuff_vanilla_data::Block;

    fn bedroom(pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        Some(if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else if pos == BlockPos::new(0, 64, 0) {
            Block::RedBed
                .default_state()
                .with("facing", "north")
                .unwrap()
        } else {
            Block::Air.default_state()
        })
    }

    fn sleeper(slept: u32) -> NearbyPlayer<'static> {
        NearbyPlayer {
            id: 1,
            uuid: 9,
            name: "",
            position: [0.5, 64.0, 0.5],
            eye_height: 0.3,
            yaw: 0.0,
            pitch: 0.0,
            held: [None, None],
            creative: false,
            height: 0.6,
            sneaking: false,
            invisible: false,
            invulnerable: false,
            armor: [None; 4],
            effects: 0,
            movement: [0.0; 3],
            vehicle: None,
            sleeping: Some(slept),
            last_hurt_by: None,
            last_hurt_mob: None,
            perchable: false,
            sprinting: false,
        }
    }

    fn tamed(core: &mut MobCore) {
        Cat::configure(core);
        core.tame = Some(crate::animal::Tame {
            owner: Some(9),
            ..Default::default()
        });
        core.set_sitting(false);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_tamed_cat_curls_up_at_the_foot_of_its_sleeping_owners_bed() {
        let owner = [sleeper(100)];
        with_players(&bedroom, &[], &owner, |core, cx| {
            tamed(core);
            core.base.position = [0.5, 64.0, 1.5];
            let mut goal = RelaxOnOwner::default();
            assert!(goal.can_use(core, cx));
            assert_eq!(goal.goal, Some(BlockPos::new(0, 64, 1)));
            goal.start(core, cx);
            for _ in 0..4 {
                goal.tick(core, cx);
            }
            assert!(core.relaxed && !core.lying, "settling in");
            assert!(core.changed.contains(&RELAXED));
            for _ in 0..12 {
                goal.tick(core, cx);
            }
            assert!(core.lying && !core.relaxed, "curled up once settled");
            assert!(core.changed.contains(&LYING));
            goal.stop(core, cx);
            assert!(!core.lying && !core.relaxed);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_tamed_cat_walks_to_a_bed_and_lies_on_it() {
        with_players(&bedroom, &[], &[], |core, cx| {
            tamed(core);
            core.base.position = [3.5, 67.0, 0.5];
            let mut goal = LieOnBlock::new();
            assert!(goal.can_use(core, cx));
            goal.start(core, cx);
            assert!(!core.lying);
            core.base.position = [0.5, 65.0, 0.5];
            goal.tick(core, cx);
            assert!(core.lying);
            goal.stop(core, cx);
            assert!(!core.lying);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_wild_or_sitting_cat_does_not_lie_on_blocks() {
        with_players(&bedroom, &[], &[], |core, cx| {
            tamed(core);
            core.tame.as_mut().unwrap().owner = None;
            core.base.position = [3.5, 64.0, 0.5];
            assert!(!LieOnBlock::new().can_use(core, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cat_ignores_an_awake_owner_and_a_sitting_cat_stays_put() {
        let mut awake = sleeper(0);
        awake.sleeping = None;
        with_players(&bedroom, &[], &[awake], |core, cx| {
            tamed(core);
            assert!(!RelaxOnOwner::default().can_use(core, cx));
        });
        with_players(&bedroom, &[], &[sleeper(100)], |core, cx| {
            tamed(core);
            core.set_sitting(true);
            assert!(!RelaxOnOwner::default().can_use(core, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cat_may_leave_a_gift_after_a_full_night() {
        let owner = [sleeper(100)];
        with_players(&bedroom, &[], &owner, |core, cx| {
            tamed(core);
            let mut goal = RelaxOnOwner::default();
            assert!(goal.can_use(core, cx));
            let mut gifted = false;
            for _ in 0..40 {
                goal.start(core, cx);
                goal.tick(core, cx);
                goal.stop(core, cx);
                if cx
                    .effects
                    .iter()
                    .any(|e| matches!(e, tuff_core::world::entity::Effect::DropItem { .. }))
                {
                    gifted = true;
                    break;
                }
            }
            assert!(gifted, "the 70 percent dawn chance hits within 40 nights");
        });
        let light = [sleeper(10)];
        with_players(&bedroom, &[], &light, |core, cx| {
            tamed(core);
            let mut goal = RelaxOnOwner::default();
            assert!(goal.can_use(core, cx));
            for _ in 0..40 {
                goal.start(core, cx);
                goal.tick(core, cx);
                goal.stop(core, cx);
            }
            assert!(cx.effects.is_empty(), "a short nap earns no gift");
        });
    }
}

#[cfg(test)]
mod gift_track_tests {
    use super::*;

    fn gift(ticks: i64) -> Option<f64> {
        tuff_vanilla_data::timeline::Timeline::get("minecraft:day")
            .unwrap()
            .sample(GIFT_ATTRIBUTE, ticks)?
            .as_f64()
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_overworld_track_gifts_from_dusk_until_just_after_midnight() {
        let chance = |ticks| gift(ticks).map(|v| (v * 10.0).round() / 10.0);
        assert_eq!(chance(100), Some(0.7));
        assert_eq!(chance(12000), Some(0.0));
        assert_eq!(chance(23667), Some(0.7));
        assert_eq!(chance(361), Some(0.7));
        assert_eq!(chance(362), Some(0.0));
        assert_eq!(chance(24000 + 100), chance(100), "the track loops");
        assert_eq!(chance(-1), chance(23999), "negative time wraps");
    }
}
