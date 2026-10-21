//! Piglins and zombified piglins.
//!
//! Piglins attack players who wear no gold, and anyone who hurts them,
//! opens a chest or breaks gold near them. Handed a gold ingot, a piglin
//! admires it for six seconds and then throws its barter (the
//! `piglin_bartering` loot table) towards the nearest player.
//!
//! Zombified piglins are neutral until hurt, then they and every other
//! zombified piglin nearby hold a grudge against the attacker.

use rand::Rng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::attack::MeleeAttack;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{LookAtPlayer, RandomLookAround, RandomStroll};
use super::monster;
use super::ranged::RangedCrossbowAttack;
use super::target::{HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer, TargetView};
use super::{Damage, MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::drops::EntityFacts;
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, NearbyPlayer, TickContext,
};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::{ItemStack, MintReason};

const BARTER_TABLE: &str = "minecraft:gameplay/piglin_bartering";
/// Ticks a piglin admires the gold it was handed.
const ADMIRE_TICKS: i32 = 119;
/// A piglin hurt by a player will not barter for this long.
const ADMIRING_DISABLED_TICKS: u64 = 400;
const MAX_REACH_TICKS: i32 = 200;
const REACH_GIVE_UP_DISABLE_TICKS: u64 = 200;
const ARMOR_CHANCE: f32 = 0.1;
const BABY_CHANCE: f32 = 0.2;
const THROW_SPEED: f64 = 0.300_000_011_920_929;
/// Piglins this close to a player who opens a chest or breaks gold turn on
/// them.
pub const ANGER_RANGE: f64 = 16.0;

fn wears_gold(player: &NearbyPlayer) -> bool {
    player.armor.iter().flatten().any(|item| {
        tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::PIGLIN_SAFE_ARMOR, *item)
    })
}

/// Adult piglins go for players without gold, and for those they hold a
/// grudge against.
fn piglin_prey(mob: &MobCore, player: &NearbyPlayer) -> bool {
    !mob.is_baby() && (!wears_gold(player) || mob.anger.is_some_and(|a| a.target == player.uuid))
}

/// How far a piglin walks for something it wants.
const WANTED_ITEM_RANGE: f64 = 9.0;
const INVENTORY_SLOTS: usize = 8;
const EAT_COOLDOWN: u64 = 200;
const HUNT_DELAY: std::ops::RangeInclusive<u64> = 600..=2400;
const AVOID_ZOMBIFIED_RANGE_SQ: f64 = 36.0;
const AVOID_DISTANCE: f64 = 12.0;

/// What a piglin remembers: its pockets and the timers of its habits.
#[derive(Debug, Default)]
pub struct PiglinBrain {
    pub inventory: Vec<ItemStack>,
    ate_until: u64,
    admiring_disabled_until: u64,
    /// Ticks spent walking to a loved item, and until when walking to
    /// loved items is off after giving up.
    reach_ticks: i32,
    walk_to_loved_disabled_until: u64,
    next_hunt: u64,
    hunting: Option<i32>,
    /// Ticks left admiring the item in its off hand.
    admire: i32,
    /// Items it picked up, with their components, not yet put away.
    received: Vec<ItemStack>,
    /// Fleeing hoglins that outnumber its kind: from whom, and until when.
    retreat: Option<(i32, u64)>,
    /// Celebrating a kill at a spot until a tick, maybe dancing.
    celebrate: Option<Celebration>,
    /// A baby's countdown to looking for a baby hoglin to ride.
    ride_ticker: i32,
    /// The baby hoglin it means to ride, and until when.
    ride: Option<(i32, u64)>,
    dancing_shown: bool,
    /// A fellow piglin called a retreat from this entity.
    retreat_pending: Option<i32>,
    /// A player it can see within sixteen blocks holds something it loves.
    jealous: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Celebration {
    at: [f64; 3],
    until: u64,
    dancing: bool,
}

impl PiglinBrain {
    fn can_add(&self, item: Item) -> bool {
        self.inventory.len() < INVENTORY_SLOTS
            || self
                .inventory
                .iter()
                .any(|s| s.item == item && s.count() < s.item.max_stack_size())
    }

    fn add(&mut self, mut stack: ItemStack) -> Option<ItemStack> {
        for held in &mut self.inventory {
            if held.item == stack.item && held.components == stack.components {
                let room = held.item.max_stack_size().saturating_sub(held.count());
                held.transfer(&mut stack, room);
            }
        }
        if stack.count() == 0 {
            return None;
        }
        if self.inventory.len() < INVENTORY_SLOTS {
            self.inventory.push(stack);
            return None;
        }
        Some(stack)
    }
}

fn is_loved(item: Item) -> bool {
    tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::PIGLIN_LOVED, item)
}

fn is_preferred_weapon(item: Item) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::item::key::PIGLIN_PREFERRED_WEAPONS,
        item,
    )
}

fn holds_loved_offhand(mob: &MobCore) -> bool {
    mob.equipment
        .get(EquipmentSlot::Offhand)
        .is_some_and(|s| is_loved(s.item))
}

/// The slot a piglin would wear or hold `item` in, if it would take it
/// over what is there.
fn replaces_current(mob: &MobCore, item: Item) -> Option<EquipmentSlot> {
    let slot = item
        .equippable()
        .map_or(EquipmentSlot::Mainhand, |e| e.slot);
    if slot == EquipmentSlot::Offhand {
        return None;
    }
    let current = mob.equipment.get(slot).map(|s| s.item);
    let wanted = |i: Item| is_loved(i) || (!mob.is_baby() && is_preferred_weapon(i));
    let new_wanted = wanted(item);
    let current_wanted = current.is_some_and(wanted);
    let armor = item.equippable().is_some();
    let replace = if new_wanted != current_wanted {
        new_wanted
    } else {
        armor && current.is_none()
    };
    replace.then_some(slot)
}

/// Whether a piglin takes this item off the ground now.
fn wants_item(mob: &MobCore, item: Item, cx: &TickContext) -> bool {
    if !mob.can_pick_up_loot
        || mob.piglin.admire > 0
        || !cx.shared.game_rules.borrow().bool("mob_griefing")
    {
        return false;
    }
    let brain = &mob.piglin;
    if (mob.is_baby()
        && tuff_datapack::tags::is(
            tuff_vanilla_data::tag::item::key::IGNORED_BY_PIGLIN_BABIES,
            item,
        ))
        || tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::PIGLIN_REPELLENTS, item)
        || (cx.tick < brain.admiring_disabled_until && mob.target.is_some())
    {
        return false;
    }
    if item == Item::GoldIngot {
        return !holds_loved_offhand(mob);
    }
    let space = brain.can_add(item);
    if item == Item::GoldNugget {
        return space;
    }
    if tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::PIGLIN_FOOD, item) {
        return cx.tick >= brain.ate_until && space;
    }
    if is_loved(item) {
        return !holds_loved_offhand(mob) && space;
    }
    replaces_current(mob, item).is_some()
}

fn is_takeable(
    mob: &MobCore,
    entity: &tuff_core::world::entity::EntitySummary,
    cx: &TickContext,
) -> bool {
    entity
        .item
        .is_some_and(|(item, count, takeable)| takeable && count > 0 && wants_item(mob, item, cx))
}

/// Walks to the nearest wanted item it sees within nine blocks.
#[derive(Debug, Default)]
struct SeekItem {
    item: Option<i32>,
}

impl SeekItem {
    fn nearest(mob: &MobCore, cx: &TickContext) -> Option<i32> {
        cx.entities
            .iter()
            .filter(|e| is_takeable(mob, e, cx))
            .filter(|e| {
                cx.tick >= mob.piglin.walk_to_loved_disabled_until
                    || !e.item.is_some_and(|(item, _, _)| is_loved(item))
            })
            .filter(|e| mob.distance_sq(e.position) <= WANTED_ITEM_RANGE * WANTED_ITEM_RANGE)
            .filter(|e| {
                let [x, y, z] = e.position;
                mob.can_see_point([x, y + (e.bounds[4] - e.bounds[1]) * 0.85, z], cx)
            })
            .min_by(|a, b| {
                mob.distance_sq(a.position)
                    .total_cmp(&mob.distance_sq(b.position))
            })
            .map(|e| e.id)
    }
}

impl Goal for SeekItem {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.is_baby() && mob.target.is_some() {
            return false;
        }
        self.item = Self::nearest(mob, cx);
        self.item.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.piglin.reach_ticks > MAX_REACH_TICKS {
            mob.piglin.reach_ticks = 0;
            mob.piglin.walk_to_loved_disabled_until = cx.tick + REACH_GIVE_UP_DISABLE_TICKS;
            return false;
        }
        self.item
            .and_then(|id| cx.entities.iter().find(|e| e.id == id))
            .is_some_and(|e| {
                is_takeable(mob, e, cx)
                    && mob.distance_sq(e.position) <= WANTED_ITEM_RANGE * WANTED_ITEM_RANGE
            })
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(item) = self
            .item
            .and_then(|id| cx.entities.iter().find(|e| e.id == id))
        else {
            return;
        };
        let position = item.position;
        if item.item.is_some_and(|(item, _, _)| is_loved(item)) {
            mob.piglin.reach_ticks += 1;
        }
        mob.look_at_point(position);
        mob.move_to_position(position, 1.0, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.item = None;
        mob.piglin.reach_ticks = 0;
        mob.navigation.stop();
    }
}

/// Runs from zombified piglins and zoglins that come within six blocks,
/// for five to seven seconds.
#[derive(Debug, Default)]
struct AvoidZombified {
    from: Option<[f64; 3]>,
    until: u64,
}

const NEMESES: &[EntityType] = &[EntityType::WitherSkeleton, EntityType::Wither];

fn grown_up(mob: &MobCore, _cx: &TickContext) -> bool {
    !mob.is_baby()
}

fn visible_nemesis(
    mob: &MobCore,
    e: &tuff_core::world::entity::EntitySummary,
    cx: &TickContext,
) -> bool {
    let eye = e.living.map_or(0.0, |l| f64::from(l.eye_height));
    NEMESES.contains(&e.kind)
        && e.living.is_some_and(|l| l.alive)
        && mob.distance_sq(e.position) <= mob.follow_range() * mob.follow_range()
        && mob.can_see_point([e.position[0], e.position[1] + eye, e.position[2]], cx)
}

fn zombified_near(mob: &MobCore, cx: &TickContext) -> Option<[f64; 3]> {
    cx.entities
        .iter()
        .filter(|e| {
            if mob.is_baby() && visible_nemesis(mob, e, cx) {
                return true;
            }
            matches!(e.kind, EntityType::ZombifiedPiglin | EntityType::Zoglin)
                && e.living.is_some_and(|l| l.alive)
                && mob.distance_sq(e.position) < AVOID_ZOMBIFIED_RANGE_SQ
        })
        .min_by(|a, b| {
            mob.distance_sq(a.position)
                .total_cmp(&mob.distance_sq(b.position))
        })
        .map(|e| e.position)
}

impl Goal for AvoidZombified {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.from = zombified_near(mob, cx);
        self.from.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if let Some(near) = zombified_near(mob, cx) {
            self.from = Some(near);
            self.until = cx.tick + cx.rng.gen_range(100..=140);
        }
        cx.tick < self.until
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.until = cx.tick + cx.rng.gen_range(100..=140);
        mob.target = None;
        mob.anger = None;
        mob.navigation.stop();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.target = None;
        let Some(from) = self.from else {
            return;
        };
        if mob.distance_sq(from) >= AVOID_DISTANCE * AVOID_DISTANCE || !mob.navigation.is_done() {
            return;
        }
        if let Some(away) = super::random_pos::pos_away(mob, 16, 7, from, cx) {
            let path = mob.create_path(super::physics::block_pos(away), 0, false, cx);
            mob.move_along(path, 1.0, cx);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.from = None;
        mob.navigation.stop();
    }
}

fn can_hunt(mob: &MobCore, cx: &TickContext) -> bool {
    !mob.is_baby() && cx.tick >= mob.piglin.next_hunt
}

fn huntable_hoglin(entity: &tuff_core::world::entity::EntitySummary) -> bool {
    entity.living.is_some_and(|l| l.alive && l.age >= 0)
}

/// Stands still while admiring gold.
#[derive(Debug, Default)]
struct Admire;

impl Goal for Admire {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK | Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.piglin.admire > 0
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

const REPELLENT_SCAN_INTERVAL: u64 = 20;
const CROSSBOW_BACK_UP_DISTANCE: f64 = 5.0;
const SENSOR_RANGE: f64 = 16.0;
const RETREAT_TICKS: std::ops::RangeInclusive<u64> = 100..=400;
const CELEBRATE_TICKS: u64 = 300;
const DANCE_CHANCE: f32 = 0.1;
const RIDE_START_INTERVAL: std::ops::RangeInclusive<i32> = 200..=800;
const RIDE_TICKS: std::ops::RangeInclusive<u64> = 200..=600;
const RIDE_WALK_RANGE: f64 = 8.0;
const MAX_RIDE_STACK: usize = 3;

/// Lit soul fire and the other blocks piglins keep away from.
fn is_piglin_repellent(state: tuff_vanilla_data::BlockState) -> bool {
    let block = state.block();
    tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::PIGLIN_REPELLENTS, block)
        && (block != tuff_vanilla_data::Block::SoulCampfire || state.get("lit") == Some("true"))
}

/// Living entities it sees within its sensing range that pass `wanted`
/// and are not babies.
pub(crate) fn visible_adults(
    mob: &MobCore,
    cx: &TickContext,
    wanted: impl Fn(&tuff_core::world::entity::EntitySummary) -> bool,
) -> Vec<i32> {
    cx.entities
        .iter()
        .filter(|e| e.id != mob.base.id && wanted(e))
        .filter(|e| e.living.is_some_and(|l| l.alive && l.age >= 0))
        .filter(|e| mob.distance_sq(e.position) <= SENSOR_RANGE * SENSOR_RANGE)
        .filter(|e| {
            let eye = e.living.map_or(0.0, |l| f64::from(l.eye_height));
            mob.can_see_point([e.position[0], e.position[1] + eye, e.position[2]], cx)
        })
        .map(|e| e.id)
        .collect()
}

/// More adult hoglins in sight than adult piglins, itself included.
fn hoglins_outnumber_piglins(mob: &MobCore, cx: &TickContext) -> bool {
    let hoglins = visible_adults(mob, cx, |e| e.kind == EntityType::Hoglin).len();
    let piglins = visible_adults(mob, cx, |e| {
        matches!(e.kind, EntityType::Piglin | EntityType::PiglinBrute)
    })
    .len();
    hoglins > piglins + 1
}

/// Drops its fight and flees `from` for five to twenty seconds, resting
/// from hunting.
fn start_retreat(core: &mut MobCore, from: i32, cx: &mut TickContext) {
    core.target = None;
    core.anger = None;
    core.navigation.stop();
    core.piglin.retreat = Some((from, cx.tick + cx.rng.gen_range(RETREAT_TICKS)));
    core.piglin.next_hunt = cx.tick + cx.rng.gen_range(HUNT_DELAY);
}

/// Runs from the hoglins it retreats from until they no longer outnumber
/// its kind.
#[derive(Debug, Default)]
struct Retreat;

impl Goal for Retreat {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.piglin.retreat.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if (cx.tick + mob.base.id as u64).is_multiple_of(REPELLENT_SCAN_INTERVAL)
            && !hoglins_outnumber_piglins(mob, cx)
        {
            mob.piglin.retreat = None;
        }
        mob.piglin.retreat.is_some()
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.target = None;
        let Some((from, _)) = mob.piglin.retreat else {
            return;
        };
        let Some(from) = cx
            .entities
            .iter()
            .find(|e| e.id == from)
            .map(|e| e.position)
        else {
            return;
        };
        if mob.distance_sq(from) >= AVOID_DISTANCE * AVOID_DISTANCE || !mob.navigation.is_done() {
            return;
        }
        if let Some(away) = super::random_pos::pos_away(mob, 16, 7, from, cx) {
            let path = mob.create_path(super::physics::block_pos(away), 0, false, cx);
            mob.move_along(path, 1.0, cx);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

/// The game's per-tick random for the dance roll: a fresh linear
/// congruential generator seeded with the game time.
fn time_seeded_float(seed: u64) -> f32 {
    const MULTIPLIER: u64 = 0x5_DEEC_E66D;
    const MASK: u64 = (1 << 48) - 1;
    let state = (seed ^ MULTIPLIER) & MASK;
    let next = (state.wrapping_mul(MULTIPLIER).wrapping_add(0xB)) & MASK;
    (next >> 24) as f32 / (1u32 << 24) as f32
}

/// Its target died: it celebrates where it fell (dancing, now and then,
/// over a hoglin) and rests from hunting after a hoglin kill.
fn celebrate_kills(core: &mut MobCore, cx: &mut TickContext) {
    if core.piglin.celebrate.is_some_and(|c| cx.tick >= c.until) {
        core.piglin.celebrate = None;
    }
    let Some(target) = core.target else {
        return;
    };
    let Some(dead) = cx
        .entities
        .iter()
        .find(|e| e.id == target && e.living.is_some_and(|l| !l.alive))
    else {
        return;
    };
    let hoglin = dead.kind == EntityType::Hoglin;
    if core.piglin.celebrate.is_none() {
        core.piglin.celebrate = Some(Celebration {
            at: dead.position,
            until: cx.tick + CELEBRATE_TICKS,
            dancing: hoglin && time_seeded_float(cx.tick) < DANCE_CHANCE,
        });
    }
    if hoglin {
        core.piglin.next_hunt = cx.tick + cx.rng.gen_range(HUNT_DELAY);
    }
    core.target = None;
    core.anger = None;
}

/// Goes to where its kill fell and stays near it.
#[derive(Debug, Default)]
struct Celebrate;

impl Goal for Celebrate {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.piglin.celebrate.is_some() && mob.target.is_none()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let pitch = mob.voice_pitch(cx.rng);
        mob.sound(SoundEvent::EntityPiglinCelebrate, 1.0, pitch, cx);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(celebration) = mob.piglin.celebrate else {
            return;
        };
        let (close, speed) = if celebration.dancing {
            (4.0, 0.6)
        } else {
            (2.0, 1.0)
        };
        if mob.distance_sq(celebration.at) > close * close && mob.navigation.is_done() {
            mob.move_to_position(celebration.at, speed, cx);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

/// The topmost rider of `id`'s stack, at most three up: where a baby
/// climbs on.
fn top_of_stack(id: i32, cx: &TickContext) -> i32 {
    let mut top = id;
    for _ in 0..MAX_RIDE_STACK {
        match cx.entities.iter().find(|e| e.vehicle == Some(top)) {
            Some(rider) => top = rider.id,
            None => break,
        }
    }
    top
}

/// A baby now and then picks a baby hoglin it sees to ride for a while,
/// and gets off when that time is up, it is hurt or its mount grew up.
fn update_ride(core: &mut MobCore, cx: &mut TickContext) {
    if !core.is_baby() {
        core.piglin.ride = None;
        return;
    }
    let vehicle = core.base.vehicle.as_ref().map(|m| m.id);
    if core.piglin.ride.is_some_and(|(_, until)| cx.tick >= until) {
        core.piglin.ride = None;
    }
    let riding_baby = vehicle.and_then(|id| cx.entities.iter().find(|e| e.id == id));
    if let Some(mount) = riding_baby {
        let baby = matches!(mount.kind, EntityType::Hoglin | EntityType::Piglin)
            && mount.living.is_some_and(|l| l.age < 0);
        let unfit = !mount.living.is_some_and(|l| l.alive && l.age < 0) || core.hurt_time > 0;
        if (baby && core.piglin.ride.is_none()) || (core.piglin.ride.is_some() && unfit) {
            core.piglin.ride = None;
            cx.effects.push(Effect::StopRiding);
        }
    } else if let Some((target, _)) = core.piglin.ride {
        let near = cx
            .entities
            .iter()
            .find(|e| e.id == target)
            .is_some_and(|e| mob_distance_ok(core, e.position));
        if !near {
            core.piglin.ride = None;
        }
    }
    if core.piglin.ride_ticker > 0 {
        core.piglin.ride_ticker -= 1;
        return;
    }
    core.piglin.ride_ticker = cx.rng.gen_range(RIDE_START_INTERVAL);
    if core.piglin.ride.is_some() || vehicle.is_some() {
        return;
    }
    let baby_hoglin = cx
        .entities
        .iter()
        .filter(|e| e.kind == EntityType::Hoglin && e.living.is_some_and(|l| l.alive && l.age < 0))
        .filter(|e| mob_distance_ok(core, e.position))
        .filter(|e| {
            let eye = e.living.map_or(0.0, |l| f64::from(l.eye_height));
            core.can_see_point([e.position[0], e.position[1] + eye, e.position[2]], cx)
        })
        .min_by(|a, b| {
            core.distance_sq(a.position)
                .total_cmp(&core.distance_sq(b.position))
        })
        .map(|e| e.id);
    if let Some(hoglin) = baby_hoglin {
        core.piglin.ride = Some((hoglin, cx.tick + cx.rng.gen_range(RIDE_TICKS)));
    }
}

fn mob_distance_ok(core: &MobCore, at: [f64; 3]) -> bool {
    core.distance_sq(at) <= RIDE_WALK_RANGE * RIDE_WALK_RANGE
}

/// Walks to the baby hoglin it picked and climbs on.
#[derive(Debug, Default)]
struct RideBabyHoglin;

impl Goal for RideBabyHoglin {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.piglin.ride.is_some() && mob.base.vehicle.is_none()
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some((target, _)) = mob.piglin.ride else {
            return;
        };
        let Some(at) = cx
            .entities
            .iter()
            .find(|e| e.id == target)
            .map(|e| e.position)
        else {
            return;
        };
        if mob.distance_sq(at) < 1.0 {
            mob.navigation.stop();
            cx.effects.push(Effect::Mount {
                passenger: mob.base.id,
                vehicle: top_of_stack(target, cx),
            });
        } else if mob.navigation.is_done() {
            mob.move_to_entity_reach(at, 0, 0.8, cx);
        }
    }
}

#[derive(Debug, Default)]
pub struct Piglin {
    immune_to_zombification: bool,
    time_in_overworld: i32,
}

const IMMUNE_TO_ZOMBIFICATION: u8 = data::MOB_FLAGS + 1;
const PIGLIN_BABY: u8 = IMMUNE_TO_ZOMBIFICATION + 1;
const CHARGING_CROSSBOW: u8 = PIGLIN_BABY + 1;
const DANCING: u8 = CHARGING_CROSSBOW + 1;
const CONVERSION_TICKS: i32 = 300;
const CROSSBOW_RANGE: f32 = 15.0;

/// Counts the ticks a piglin spends where piglins zombify and converts
/// it into a zombified piglin after fifteen seconds; true once it has.
fn tick_zombification(
    core: &mut MobCore,
    immune: bool,
    time_in_overworld: &mut i32,
    cx: &mut TickContext,
) -> bool {
    let converting = !immune && !core.no_ai && core.is_alive() && super::hoglin::zombifies_here(cx);
    if !converting {
        *time_in_overworld = 0;
        return false;
    }
    *time_in_overworld += 1;
    if *time_in_overworld <= CONVERSION_TICKS {
        return false;
    }
    if *cx.shared.difficulty.borrow() != tuff_core::world::player::Difficulty::Peaceful {
        let pitch = core.voice_pitch(cx.rng);
        core.sound(SoundEvent::EntityPiglinConvertedToZombified, 1.0, pitch, cx);
    }
    let mut zombified =
        super::convert::<ZombifiedPiglin>(core, EntityType::ZombifiedPiglin, true, cx);
    if core.is_baby() {
        zombified.core.set_zombie_baby(true);
    }
    zombified.core.can_pick_up_loot = core.can_pick_up_loot;
    zombified.core.changed.clear();
    zombified
        .core
        .add_effect(tuff_core::world::effects::ActiveEffect::timed(
            tuff_vanilla_data::registry::MobEffect::Nausea,
            super::CONVERSION_NAUSEA,
            0,
        ));
    cx.effects
        .push(tuff_core::world::entity::Effect::Spawn(Box::new(zombified)));
    core.discarded = true;
    true
}

impl Piglin {
    fn activity_sound(&self, core: &MobCore) -> SoundEvent {
        let converting = !self.immune_to_zombification && self.time_in_overworld > 0;
        if core.target.is_some() {
            SoundEvent::EntityPiglinAngry
        } else if converting || core.piglin.retreat.is_some() {
            SoundEvent::EntityPiglinRetreat
        } else if core.piglin.admire > 0 {
            SoundEvent::EntityPiglinAdmiringItem
        } else if core.piglin.celebrate.is_some() {
            SoundEvent::EntityPiglinCelebrate
        } else if core.piglin.jealous {
            SoundEvent::EntityPiglinJealous
        } else if core.avoid.is_some() {
            SoundEvent::EntityPiglinRetreat
        } else {
            SoundEvent::EntityPiglinAmbient
        }
    }

    /// Takes whatever wanted item lies within reach.
    fn pick_up_items(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.can_pick_up_loot || core.piglin.admire > 0 {
            return;
        }
        let reach = super::physics::inflate(core.bounds(), [1.0, 0.0, 1.0]);
        let Some((id, item, count)) = cx.entities.iter().find_map(|e| {
            let (item, count, _) = e.item?;
            (is_takeable(core, e, cx) && super::physics::intersects(e.bounds, reach))
                .then_some((e.id, item, count))
        }) else {
            return;
        };
        let taken = if item == Item::GoldNugget { count } else { 1 };
        cx.effects.push(Effect::TakeItem {
            item: id,
            count: taken,
        });
        core.navigation.stop();
    }

    /// Puts away what it picked up: admires what it loves, eats food, and
    /// wears, holds or pockets the rest.
    fn put_away(&mut self, core: &mut MobCore, stack: ItemStack, cx: &mut TickContext) {
        let item = stack.item;
        if is_loved(item) {
            self.hold_and_admire(core, stack, cx);
        } else if tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::PIGLIN_FOOD, item)
            && cx.tick >= core.piglin.ate_until
        {
            core.piglin.ate_until = cx.tick + EAT_COOLDOWN;
            stack.burn(tuff_item::item::BurnReason::Consumed);
        } else {
            self.equip_or_store(core, stack, cx);
        }
    }

    /// Wears or holds `stack` if it would take it over what it has,
    /// dropping the old; otherwise it goes in a pocket, and what does not
    /// fit is thrown.
    fn equip_or_store(&mut self, core: &mut MobCore, stack: ItemStack, cx: &mut TickContext) {
        if let Some(slot) = replaces_current(core, stack.item) {
            if let Some(old) = core.equipment.replace(slot, Some(stack)) {
                core.drop_item(old, 0.0, cx);
            }
            core.equipment
                .set_drop_chance(slot, super::equipment::PRESERVED_DROP_CHANCE);
            return;
        }
        if let Some(rest) = core.piglin.add(stack) {
            core.drop_item(rest, 0.0, cx);
        }
    }

    /// Holds `stack` in its off hand (dropping what was there) and
    /// admires it.
    fn hold_and_admire(&mut self, core: &mut MobCore, stack: ItemStack, cx: &mut TickContext) {
        if let Some(held) = core.equipment.replace(EquipmentSlot::Offhand, Some(stack)) {
            core.drop_item(held, 0.0, cx);
        }
        core.equipment.set_drop_chance(
            EquipmentSlot::Offhand,
            super::equipment::PRESERVED_DROP_CHANCE,
        );
        core.piglin.admire = ADMIRE_TICKS;
        core.target = None;
        core.navigation.stop();
    }

    /// The admiring is over: gold is traded for its barter, anything
    /// else it loves is worn or pocketed.
    fn finish_admiring(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(held) = core.equipment.take(EquipmentSlot::Offhand) else {
            return;
        };
        if held.item == Item::GoldIngot {
            if core.is_baby() {
                self.equip_or_store(core, held, cx);
            } else {
                held.burn(tuff_item::item::BurnReason::TradePayment);
                self.barter(core, cx);
            }
        } else {
            self.equip_or_store(core, held, cx);
        }
    }

    /// Throws the barter for the gold it admired towards the nearest
    /// player it sees, or somewhere near itself.
    fn barter(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let facts = EntityFacts {
            kind: Some(core.kind()),
            tables: Some(cx.shared.loot_tables()),
            ..Default::default()
        };
        let items = cx
            .shared
            .block_loot()
            .entity_drops(BARTER_TABLE, &facts, cx.rng);
        let eye = core.eye_position();
        let target = cx
            .players
            .iter()
            .filter(|p| core.can_see_point(p.eye(), cx))
            .map(|p| (tuff_core::world::distance_sq(eye, p.position), p.position))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, position)| position)
            .unwrap_or_else(|| {
                let [x, y, z] = core.base.position;
                [
                    x + f64::from(cx.rng.gen_range(-7..=7)),
                    y + f64::from(cx.rng.gen_range(-3..=3)),
                    z + f64::from(cx.rng.gen_range(-7..=7)),
                ]
            });
        if items.is_empty() {
            return;
        }
        core.swing(cx);
        let [x, y, z] = core.base.position;
        let toward = [target[0] - x, target[1] + 1.0 - y, target[2] - z];
        let length = toward.iter().map(|c| c * c).sum::<f64>().sqrt().max(1.0e-5);
        let velocity = toward.map(|c| c / length * THROW_SPEED);
        for stack in items {
            cx.effects.push(Effect::DropItem {
                stack,
                position: [x, eye[1] - 0.300_000_011_920_929, z],
                velocity,
                pickup_delay: tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY,
            });
        }
    }

    fn drop_inventory(core: &mut MobCore, cx: &mut TickContext) {
        for stack in std::mem::take(&mut core.piglin.inventory) {
            core.drop_item(stack, 0.0, cx);
        }
    }
}

impl Species for Piglin {
    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        let mut goals = GoalSelector::default();
        goals.add(
            1,
            HurtByTarget::new(&[EntityType::Piglin]).alerting(&[EntityType::Piglin]),
        );
        goals.add(
            2,
            NearestAttackableEntity::new(NEMESES, true).when(grown_up),
        );
        goals.add(
            2,
            NearestAttackablePlayer::with_interval(10, true, Some(piglin_prey)),
        );
        goals.add(
            2,
            NearestAttackableEntity::new(&[EntityType::Hoglin], true)
                .when(can_hunt)
                .only(huntable_hoglin),
        );
        core.navigation.can_open_doors = true;
        goals.add(0, Admire);
        goals.add(0, AvoidZombified::default());
        goals.add(3, super::door::OpenDoor::new(true));
        goals.add(1, SeekItem::default());
        goals.add(0, Retreat);
        goals.add(
            3,
            RangedCrossbowAttack::new(
                1.0,
                CROSSBOW_RANGE,
                Item::Crossbow,
                SoundEvent::ItemCrossbowShoot,
            )
            .backing_up(CROSSBOW_BACK_UP_DISTANCE),
        );
        goals.add(4, super::hoglin::AvoidRepellent);
        goals.add(5, RideBabyHoglin);
        goals.add(5, Celebrate);
        goals.add(2, super::spear::SpearUse::standard());
        goals.add(
            3,
            MeleeAttack::new(1.0, false)
                .unless_holding(Item::Crossbow)
                .gives_up_unreachable(),
        );
        goals.add(7, RandomStroll::water_avoiding(0.6));
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut tuff_nbt::Compound) {
        use tuff_core::world::entity::{take_bool, take_number};
        self.immune_to_zombification = take_bool(nbt, "IsImmuneToZombification").unwrap_or(false);
        self.time_in_overworld = take_number(nbt, "TimeInOverworld").map_or(0, |v| v as i32);
        if take_bool(nbt, "IsBaby").unwrap_or(false) {
            core.set_baby(true);
        }
        if let Some(tuff_nbt::Tag::List(list)) = nbt.remove("Inventory") {
            core.piglin.inventory = list
                .iter()
                .filter_map(|tag| ItemStack::from_storage(tag.as_compound()?))
                .take(INVENTORY_SLOTS)
                .collect();
        }
    }

    fn save(&self, core: &MobCore, out: &mut tuff_nbt::Compound) {
        out.insert("IsImmuneToZombification", self.immune_to_zombification);
        out.insert("TimeInOverworld", self.time_in_overworld);
        out.insert("IsBaby", core.is_baby());
        out.insert(
            "Inventory",
            tuff_nbt::Tag::List(
                core.piglin
                    .inventory
                    .iter()
                    .chain(&core.piglin.received)
                    .take(INVENTORY_SLOTS)
                    .map(|stack| tuff_nbt::Tag::Compound(stack.to_nbt()))
                    .collect(),
            ),
        );
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                IMMUNE_TO_ZOMBIFICATION,
                MetadataValue::Boolean(self.immune_to_zombification),
            ),
            (PIGLIN_BABY, MetadataValue::Boolean(core.is_baby())),
            (CHARGING_CROSSBOW, MetadataValue::Boolean(core.signal != 0)),
            (DANCING, MetadataValue::Boolean(core.piglin.dancing_shown)),
        ]
    }

    /// Outside the nether it turns into a zombified piglin after fifteen
    /// seconds, keeping its equipment.
    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if tick_zombification(
            core,
            self.immune_to_zombification,
            &mut self.time_in_overworld,
            cx,
        ) {
            Self::drop_inventory(core, cx);
        }
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        monster::sounds(
            self.activity_sound(core),
            SoundEvent::EntityPiglinHurt,
            SoundEvent::EntityPiglinDeath,
            Some(SoundEvent::EntityPiglinStep),
        )
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.update_anger(false, cx);
        core.piglin.jealous = cx.players.iter().any(|player| {
            !player.creative
                && player.held.iter().flatten().any(|item| is_loved(*item))
                && core.distance_sq(player.position) <= 256.0
                && core.has_line_of_sight(&TargetView::of_player(player), cx)
        });
        for stack in std::mem::take(&mut core.piglin.received) {
            self.put_away(core, stack, cx);
        }
        self.pick_up_items(core, cx);
        if (cx.tick + core.base.id as u64).is_multiple_of(REPELLENT_SCAN_INTERVAL) {
            core.avoid = super::hoglin::nearest_block(core, cx, is_piglin_repellent);
        }
        if let Some(from) = core.piglin.retreat_pending.take() {
            start_retreat(core, from, cx);
        }
        celebrate_kills(core, cx);
        update_ride(core, cx);
        if core.piglin.admire > 0 {
            core.target = None;
            core.piglin.admire -= 1;
            if core.piglin.admire == 0 {
                self.finish_admiring(core, cx);
            }
        }
        if core.is_baby() {
            core.target = None;
        }
        if core
            .piglin
            .retreat
            .is_some_and(|(_, until)| cx.tick >= until)
        {
            core.piglin.retreat = None;
        }
        let dancing = core.piglin.celebrate.is_some_and(|c| c.dancing);
        if dancing != core.piglin.dancing_shown {
            core.piglin.dancing_shown = dancing;
            core.changed(DANCING);
        }
        if core.piglin.next_hunt == 0 {
            core.piglin.next_hunt = cx.tick + cx.rng.gen_range(HUNT_DELAY);
        }
        self.note_hunt(core, cx);
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if stack.item != Item::GoldIngot
            || core.piglin.admire > 0
            || core.is_baby()
            || cx.tick < core.piglin.admiring_disabled_until
        {
            return None;
        }
        let gold = ItemStack::mint(Item::GoldIngot, 1, MintReason::Transform);
        self.hold_and_admire(core, gold, cx);
        Some(HandEffect::Consume(1))
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        if damage
            .attacker
            .is_some_and(|(_, kind)| kind == EntityType::Piglin)
        {
            return;
        }
        core.piglin.celebrate = None;
        if core.piglin.admire > 0 {
            core.piglin.admire = 0;
            if let Some(held) = core.equipment.take(EquipmentSlot::Offhand) {
                if held.item == Item::GoldIngot {
                    held.burn(tuff_item::item::BurnReason::Consumed);
                } else {
                    self.equip_or_store(core, held, cx);
                }
            }
        }
        if damage
            .attacker
            .is_some_and(|(_, kind)| kind == EntityType::Player)
        {
            core.piglin.admiring_disabled_until = cx.tick + ADMIRING_DISABLED_TICKS;
        }
        if core.is_baby()
            && let Some((attacker, _)) = damage.attacker
        {
            self.call_adults(core, attacker, cx);
        }
        if !core.is_baby()
            && let Some((attacker, EntityType::Hoglin)) = damage.attacker
            && hoglins_outnumber_piglins(core, cx)
        {
            start_retreat(core, attacker, cx);
            for other in visible_adults(core, cx, |e| e.kind == EntityType::Piglin) {
                cx.effects.push(Effect::Notify {
                    target: other,
                    notice: tuff_core::world::entity::Notice::Retreat { from: attacker },
                });
            }
        }
    }

    fn took_item(&mut self, core: &mut MobCore, stack: ItemStack) {
        core.piglin.received.push(stack);
    }

    fn heard(&mut self, core: &mut MobCore, notice: tuff_core::world::entity::Notice) {
        if let tuff_core::world::entity::Notice::Retreat { from } = notice
            && !core.is_baby()
            && core.is_alive()
        {
            core.piglin.retreat_pending = Some(from);
        }
    }

    fn died(&mut self, core: &mut MobCore, _damage: &Damage, cx: &mut TickContext) {
        Self::drop_inventory(core, cx);
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.can_pick_up_loot = true;
        core.piglin.next_hunt = cx.tick + cx.rng.gen_range(HUNT_DELAY);
        if !super::spawning::spawning_from_structure(cx) {
            if cx.rng.r#gen::<f32>() < BABY_CHANCE {
                core.set_baby(true);
            } else {
                let weapon = if cx.rng.r#gen::<f32>() < 0.5 {
                    Item::Crossbow
                } else if cx.rng.gen_range(0..10) == 0 {
                    Item::GoldenSpear
                } else {
                    Item::GoldenSword
                };
                core.equipment.set(
                    EquipmentSlot::Mainhand,
                    Some(ItemStack::mint(weapon, 1, MintReason::Spawn)),
                );
            }
        }
        if !core.is_baby() {
            for (slot, item) in [
                (EquipmentSlot::Head, Item::GoldenHelmet),
                (EquipmentSlot::Chest, Item::GoldenChestplate),
                (EquipmentSlot::Legs, Item::GoldenLeggings),
                (EquipmentSlot::Feet, Item::GoldenBoots),
            ] {
                if cx.rng.r#gen::<f32>() < ARMOR_CHANCE {
                    core.equipment
                        .set(slot, Some(ItemStack::mint(item, 1, MintReason::Spawn)));
                }
            }
        }
        core.populate_spawn_enchantments(
            super::monster::LocalDifficulty::at(cx, core.base.position),
            cx.rng,
        );
    }
}

impl Piglin {
    /// A hurt baby cries for the adults near it to deal with its attacker.
    fn call_adults(&mut self, core: &MobCore, attacker: i32, cx: &mut TickContext) {
        let [x, y, z] = core.base.position;
        for other in cx.entities {
            let adult = other.kind == EntityType::Piglin
                && other.id != core.base.id
                && other
                    .living
                    .is_some_and(|l| l.alive && l.age >= 0 && l.target.is_none());
            let near = tuff_core::world::distance_sq(other.position, [x, y, z]) <= 256.0;
            if adult && near {
                cx.effects.push(Effect::Notify {
                    target: other.id,
                    notice: tuff_core::world::entity::Notice::Alert { attacker },
                });
            }
        }
    }

    /// Hunting a hoglin starts a rest from hunting.
    fn note_hunt(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let hoglin = core.target.filter(|id| {
            cx.entities
                .iter()
                .any(|e| e.id == *id && e.kind == EntityType::Hoglin)
        });
        if hoglin != core.piglin.hunting {
            core.piglin.hunting = hoglin;
            if hoglin.is_some() {
                core.piglin.next_hunt = cx.tick + cx.rng.gen_range(HUNT_DELAY);
            }
        }
    }
}

const BRUTE_IMMUNE_TO_ZOMBIFICATION: u8 = data::MOB_FLAGS + 1;
const BRUTE_HOME_RADIUS: i32 = 100;

/// Piglin brutes: always hostile to players (gold does not deter them),
/// never barter, only pick up golden axes, keep to the spot they spawned
/// at, and zombify in the overworld like piglins unless immune.
#[derive(Debug, Default)]
pub struct PiglinBrute {
    immune_to_zombification: bool,
    time_in_overworld: i32,
}

impl PiglinBrute {
    fn take_axe(&self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.can_pick_up_loot
            || core.equipment.get(EquipmentSlot::Mainhand).is_some()
            || !cx.shared.game_rules.borrow().bool("mob_griefing")
        {
            return;
        }
        let reach = super::physics::inflate(core.bounds(), [1.0, 0.0, 1.0]);
        let Some(id) = cx.entities.iter().find_map(|e| {
            let (item, count, takeable) = e.item?;
            (takeable
                && count > 0
                && item == Item::GoldenAxe
                && super::physics::intersects(e.bounds, reach))
            .then_some(e.id)
        }) else {
            return;
        };
        cx.effects.push(Effect::TakeItem { item: id, count: 1 });
    }
}

impl Species for PiglinBrute {
    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = 20;
        let mut goals = GoalSelector::default();
        goals.add(
            1,
            HurtByTarget::new(&[]).alerting(&[EntityType::Piglin, EntityType::PiglinBrute]),
        );
        goals.add(2, NearestAttackablePlayer::new(true));
        goals.add(2, NearestAttackableEntity::new(NEMESES, true));
        goals.add(3, MeleeAttack::new(1.0, false));
        goals.add(7, RandomStroll::water_avoiding(0.6));
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut tuff_nbt::Compound) {
        use tuff_core::world::entity::{take_bool, take_number};
        self.immune_to_zombification = take_bool(nbt, "IsImmuneToZombification").unwrap_or(false);
        self.time_in_overworld = take_number(nbt, "TimeInOverworld").map_or(0, |v| v as i32);
    }

    fn save(&self, _core: &MobCore, out: &mut tuff_nbt::Compound) {
        out.insert("IsImmuneToZombification", self.immune_to_zombification);
        out.insert("TimeInOverworld", self.time_in_overworld);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(
            BRUTE_IMMUNE_TO_ZOMBIFICATION,
            MetadataValue::Boolean(self.immune_to_zombification),
        )]
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        tick_zombification(
            core,
            self.immune_to_zombification,
            &mut self.time_in_overworld,
            cx,
        );
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.take_axe(core, cx);
    }

    fn took_item(&mut self, core: &mut MobCore, stack: ItemStack) {
        if stack.item == Item::GoldenAxe && core.equipment.get(EquipmentSlot::Mainhand).is_none() {
            core.equipment.set(EquipmentSlot::Mainhand, Some(stack));
            core.equipment.set_drop_chance(
                EquipmentSlot::Mainhand,
                super::equipment::PRESERVED_DROP_CHANCE,
            );
            core.persistence_required = true;
        }
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        monster::sounds(
            SoundEvent::EntityPiglinBruteAmbient,
            SoundEvent::EntityPiglinBruteHurt,
            SoundEvent::EntityPiglinBruteDeath,
            Some(SoundEvent::EntityPiglinBruteStep),
        )
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.home = Some(super::Home {
            pos: super::physics::block_pos(core.base.position),
            radius: BRUTE_HOME_RADIUS,
        });
        core.equipment.set(
            EquipmentSlot::Mainhand,
            Some(ItemStack::mint(Item::GoldenAxe, 1, MintReason::Spawn)),
        );
    }
}

/// Piglins and zombified piglins spawn anywhere but on nether wart
/// blocks.
fn not_on_nether_wart(
    _kind: EntityType,
    level: &super::spawning::SpawnLevel,
    pos: tuff_world_model::BlockPos,
    _reason: super::spawning::SpawnReason,
    _rng: &mut rand::rngs::StdRng,
) -> bool {
    let below = tuff_world_model::BlockPos::new(pos.x, pos.y - 1, pos.z);
    !(level.block)(below).is_some_and(|s| s.block() == tuff_vanilla_data::Block::NetherWartBlock)
}

fn zombified_spawn_rule(
    kind: EntityType,
    level: &super::spawning::SpawnLevel,
    pos: tuff_world_model::BlockPos,
    reason: super::spawning::SpawnReason,
    rng: &mut rand::rngs::StdRng,
) -> bool {
    level.difficulty != tuff_core::world::player::Difficulty::Peaceful
        && not_on_nether_wart(kind, level, pos, reason, rng)
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Piglin, super::load::<Piglin>);
    registry.register_spawn_rule(EntityType::Piglin, not_on_nether_wart);
    registry.register(EntityType::ZombifiedPiglin, super::load::<ZombifiedPiglin>);
    registry.register_spawn_rule(EntityType::ZombifiedPiglin, zombified_spawn_rule);
    registry.register(EntityType::PiglinBrute, super::load::<PiglinBrute>);
    registry.register_spawn_rule(EntityType::PiglinBrute, not_on_nether_wart);
}

/// Piglins within 16 blocks of `player` (that can see it, if
/// `only_if_seen`) turn on it: it opened or broke something they guard.
pub fn anger_piglins(
    world: &mut dyn tuff_core::world::region_api::RegionWorld,
    player: tuff_core::world::messages::PlayerId,
    only_if_seen: bool,
) {
    let Some(presence) = world.present_players().into_iter().find(|p| p.id == player) else {
        return;
    };
    let [x, y, z] = presence.position;
    let range = ANGER_RANGE;
    let reach = [
        x - 0.3 - range,
        y - range,
        z - 0.3 - range,
        x + 0.3 + range,
        y + presence.height + range,
        z + 0.3 + range,
    ];
    let eye = [x, y + presence.eye_height, z];
    let piglins: Vec<(i32, [f64; 3])> = world
        .entity_ids()
        .into_iter()
        .filter_map(|id| {
            let tracked = world.entity(id)?;
            let base = tracked.entity.base();
            (base.kind == tuff_vanilla_data::registry::EntityType::Piglin
                && crate::physics::intersects(tracked.entity.bounds(), reach))
            .then(|| {
                let height = f64::from(base.kind.size().eye_height);
                (
                    id,
                    [
                        base.position[0],
                        base.position[1] + height,
                        base.position[2],
                    ],
                )
            })
        })
        .collect();
    for (id, from) in piglins {
        if only_if_seen {
            let level = |pos| {
                world
                    .block(pos)
                    .unwrap_or(tuff_vanilla_data::Block::Stone.default_state())
            };
            if tuff_core::world::raycast::cast_colliders(from, eye, &level).is_some() {
                continue;
            }
        }
        if let Some(tracked) = world.entity_mut(id) {
            tracked
                .entity
                .notice(tuff_core::world::entity::Notice::Alert { attacker: player });
        }
    }
}

mod zombified;
pub use zombified::*;

#[cfg(test)]
#[path = "piglin_tests.rs"]
pub(crate) mod tests;
