//! Llama and Trader Llama: chested equines (see `equine.rs` for taming,
//! feeding and the inventory) whose strength sets their chest size, that
//! spit at whoever hurts them and at untamed wolves.
//!
//! Llamas line up behind a leashed llama into caravans (up to eight links
//! behind a leashed head), tracked in region state by UUID; a trader llama
//! is leashed to its wandering trader by the trader's UUID and defends it.

use std::collections::HashMap;

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::equine::{self, Equine, FLAGS, Feed, MIN_MAX_HEALTH, Profile, RunAroundLikeCrazy};
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{
    Breed, FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll, Tempt,
};
use super::llama_spit::LlamaSpit;
use super::target::{HurtByTarget, NearestAttackableEntity, TargetView};
use super::{Mob, MobCore, MobSounds, Species};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, RegionEntity, RiderInput, TickContext, take_number,
};
use tuff_core::world::menu::MountArmor;
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;

const MAX_STRENGTH: i32 = 5;
const ATTACK_INTERVAL: i32 = 40;
const ATTACK_RADIUS: f64 = 20.0;
/// The speed a slack lead walks a llama at.
const LEAD_SPEED: f64 = 2.0;

fn llama_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::LLAMA_FOOD, item)
}

fn llama_tempt(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::LLAMA_TEMPT_ITEMS, item)
}

fn llama_feed(item: Item) -> Option<Feed> {
    match item {
        Item::Wheat => equine::feed(2.0, 10, 3, false),
        Item::HayBlock => equine::feed(10.0, 90, 6, true),
        _ => None,
    }
}

const LLAMA: Profile = Profile {
    max_temper: 30,
    rears: false,
    grazes: false,
    saddles: false,
    armor: MountArmor::Carpet,
    chested: true,
    feed: llama_feed,
    eats_visibly: false,
    angry: Some(SoundEvent::EntityLlamaAngry),
    eat: Some(SoundEvent::EntityLlamaEat),
    chest_sound: SoundEvent::EntityLlamaChest,
    ..Profile::horse_like(llama_food)
};

fn roll_strength(rng: &mut StdRng) -> u8 {
    let max = if rng.r#gen::<f32>() < 0.04 { 5 } else { 3 };
    1 + rng.gen_range(0..max)
}

/// Spits at its target from range, closing in when it cannot see it.
#[derive(Debug)]
pub struct Spit {
    speed: f64,
    see_time: i32,
    attack_time: i32,
}

impl Spit {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            see_time: 0,
            attack_time: -1,
        }
    }
}

impl Goal for Spit {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx) || !mob.navigation.is_done()
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.see_time = 0;
        self.attack_time = -1;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        let distance = mob.distance_sq(target.position);
        let sees = mob.has_line_of_sight(&target, cx);
        self.see_time = if sees { self.see_time + 1 } else { 0 };
        if distance <= ATTACK_RADIUS * ATTACK_RADIUS && self.see_time >= 5 {
            mob.navigation.stop();
        } else {
            mob.move_to_entity(target.position, self.speed, cx);
        }
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        self.attack_time -= 1;
        if self.attack_time == 0 {
            if !sees {
                return;
            }
            spit(mob, &target, cx);
            self.attack_time = ATTACK_INTERVAL;
        } else if self.attack_time < 0 {
            self.attack_time = ATTACK_INTERVAL;
        }
    }
}

/// Spits once at `target`; `MobCore::signal` remembers it spat, which ends
/// a revenge target.
pub fn spit(mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) {
    let id = cx.shared.next_entity_id();
    let spit = LlamaSpit::at(mob, target, id, cx.rng);
    cx.effects.push(Effect::Spawn(Box::new(spit)));
    let pitch = 1.0 + (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2;
    mob.sound(SoundEvent::EntityLlamaSpit, 1.0, pitch, cx);
    mob.signal = 1;
}

/// Targets whoever hurt it, but only until it has spat at them once.
#[derive(Debug)]
pub struct SpitBack(HurtByTarget);

impl Goal for SpitBack {
    fn flags(&self) -> Flags {
        self.0.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.0.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.signal != 0 {
            mob.signal = 0;
            return false;
        }
        self.0.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.start(mob, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.stop(mob, cx);
    }
}

fn untamed_wolf_nearby(mob: &MobCore, target: &TargetView, cx: &TickContext) -> bool {
    let range = mob.follow_range() * 0.25;
    mob.distance_sq(target.position) <= range * range
        && cx
            .entities
            .iter()
            .find(|e| e.id == target.id)
            .and_then(|e| e.living)
            .is_some_and(|living| !living.tamed)
}

const ZOMBIES: &[EntityType] = &[
    EntityType::Zombie,
    EntityType::Husk,
    EntityType::Drowned,
    EntityType::ZombieVillager,
];
const ILLAGERS: &[EntityType] = &[
    EntityType::Evoker,
    EntityType::Illusioner,
    EntityType::Pillager,
    EntityType::Vindicator,
];

const CARAVAN_SPEED: f64 = 2.1;
const CARAVAN_LIMIT: u32 = 8;

/// What the region's llamas publish each tick so a caravan can find its
/// head and tail, by UUID.
#[derive(Default)]
pub struct LlamaCaravans {
    llamas: HashMap<u128, Link>,
}

#[derive(Debug, Clone, Copy)]
struct Link {
    position: [f64; 3],
    leashed: bool,
    head: Option<u128>,
    seen: u64,
}

impl LlamaCaravans {
    fn fresh(&self, uuid: u128, now: u64) -> Option<&Link> {
        self.llamas
            .get(&uuid)
            .filter(|l| now.saturating_sub(l.seen) <= 1)
    }

    fn publish(&mut self, core: &MobCore, now: u64) {
        let link = self.llamas.entry(core.base.uuid).or_insert(Link {
            position: core.base.position,
            leashed: false,
            head: None,
            seen: now,
        });
        link.position = core.base.position;
        link.leashed = core.leash.is_some();
        link.seen = now;
        if now.is_multiple_of(100) {
            self.llamas.retain(|_, l| now.saturating_sub(l.seen) <= 20);
        }
    }

    fn has_tail(&self, uuid: u128, now: u64) -> bool {
        self.llamas
            .iter()
            .any(|(_, l)| l.head == Some(uuid) && now.saturating_sub(l.seen) <= 1)
    }

    fn first_is_leashed(&self, uuid: u128, counter: u32, now: u64) -> bool {
        if counter > CARAVAN_LIMIT {
            return false;
        }
        let Some(head) = self.fresh(uuid, now).and_then(|l| l.head) else {
            return false;
        };
        match self.fresh(head, now) {
            Some(link) if link.leashed => true,
            Some(_) => self.first_is_leashed(head, counter + 1, now),
            None => false,
        }
    }

    fn set_head(&mut self, uuid: u128, head: Option<u128>) {
        if let Some(link) = self.llamas.get_mut(&uuid) {
            link.head = head;
        }
    }
}

/// Follows the llama ahead in a caravan whose first llama is on a lead.
#[derive(Debug)]
struct FollowCaravan {
    speed: f64,
    check: i32,
    head: Option<u128>,
}

impl FollowCaravan {
    fn new() -> Self {
        Self {
            speed: CARAVAN_SPEED,
            check: 0,
            head: None,
        }
    }
}

impl Goal for FollowCaravan {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let now = cx.tick;
        let me = mob.base.uuid;
        let mut caravans = cx.state.get::<LlamaCaravans>();
        caravans.publish(mob, now);
        if mob.leash.is_some() || self.head.is_some() {
            return false;
        }
        let [x, y, z] = mob.base.position;
        let mut near: Vec<(u128, Link)> = caravans
            .llamas
            .iter()
            .filter(|(uuid, l)| {
                **uuid != me
                    && now.saturating_sub(l.seen) <= 1
                    && (l.position[0] - x).abs() <= 9.9
                    && (l.position[1] - y).abs() <= 5.9
                    && (l.position[2] - z).abs() <= 9.9
            })
            .map(|(uuid, l)| (*uuid, *l))
            .collect();
        near.sort_by_key(|(uuid, _)| *uuid);
        let dist = |l: &Link| {
            let [a, b, c] = l.position;
            (a - x).powi(2) + (b - y).powi(2) + (c - z).powi(2)
        };
        let mut best: Option<(u128, Link, f64)> = None;
        for pass_leashed in [false, true] {
            for (uuid, link) in &near {
                let eligible = if pass_leashed {
                    link.leashed
                } else {
                    link.head.is_some()
                };
                if eligible && !caravans.has_tail(*uuid, now) {
                    let d = dist(link);
                    if best.is_none_or(|(_, _, b)| d <= b) {
                        best = Some((*uuid, *link, d));
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
        let Some((uuid, link, d)) = best else {
            return false;
        };
        if d < 4.0 || (!link.leashed && !caravans.first_is_leashed(uuid, 1, now)) {
            return false;
        }
        caravans.set_head(me, Some(uuid));
        self.head = Some(uuid);
        true
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let now = cx.tick;
        let me = mob.base.uuid;
        let mut caravans = cx.state.get::<LlamaCaravans>();
        caravans.publish(mob, now);
        let Some(head) = self.head else {
            return false;
        };
        let Some(link) = caravans.fresh(head, now) else {
            return false;
        };
        if !caravans.first_is_leashed(me, 0, now) {
            return false;
        }
        let [x, y, z] = mob.base.position;
        let [a, b, c] = link.position;
        if (a - x).powi(2) + (b - y).powi(2) + (c - z).powi(2) > 676.0 {
            if self.speed <= 3.0 {
                self.speed *= 1.2;
                self.check = 40;
                return true;
            }
            if self.check == 0 {
                return false;
            }
        }
        if self.check > 0 {
            self.check -= 1;
        }
        true
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        cx.state
            .get::<LlamaCaravans>()
            .set_head(mob.base.uuid, None);
        self.head = None;
        self.speed = CARAVAN_SPEED;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(head) = self.head else {
            return;
        };
        if matches!(
            mob.leash.as_ref().map(|l| l.anchor),
            Some(tuff_core::world::leash::Anchor::Knot(_))
        ) {
            return;
        }
        let now = cx.tick;
        let Some(link) = cx.state.get::<LlamaCaravans>().fresh(head, now).copied() else {
            return;
        };
        let [x, y, z] = mob.base.position;
        let delta = [
            link.position[0] - x,
            link.position[1] - y,
            link.position[2] - z,
        ];
        let length = delta.iter().map(|d| d * d).sum::<f64>().sqrt();
        if length < 1.0e-7 {
            return;
        }
        let scale = (length - 2.0).max(0.0) / length;
        let target = [
            x + delta[0] * scale,
            y + delta[1] * scale,
            z + delta[2] * scale,
        ];
        mob.move_to_entity(target, self.speed, cx);
    }
}

fn llama_goals(core: &mut MobCore) -> GoalSelector {
    let mut goals = GoalSelector::default();
    goals.add(0, super::goals::Float::new(core));
    goals.add(1, RunAroundLikeCrazy::new(1.2));
    goals.add(2, FollowCaravan::new());
    goals.add(3, Spit::new(1.25));
    goals.add(3, Panic::new(1.2));
    goals.add(4, Breed::new(1.0).with_partners(equine::mates));
    goals.add(5, Tempt::new(1.25, llama_tempt));
    goals.add(6, FollowParent::new(1.0));
    goals.add(7, RandomStroll::water_avoiding(0.7));
    goals.add(8, LookAtPlayer::new(6.0));
    goals.add(9, RandomLookAround::default());
    goals.add(1, SpitBack(HurtByTarget::new(&[])));
    goals.add(
        2,
        NearestAttackableEntity::new(&[EntityType::Wolf], false).filtered(untamed_wolf_nearby),
    );
    goals
}

#[derive(Debug)]
pub struct Llama {
    equine: Equine,
    strength: u8,
    variant: u8,
}

impl Default for Llama {
    fn default() -> Self {
        Self {
            equine: Equine::default(),
            strength: 1,
            variant: 0,
        }
    }
}

impl Llama {
    const PROFILE: Profile = LLAMA;

    fn own_metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![
            (FLAGS + 1, MetadataValue::Boolean(self.equine.has_chest)),
            (FLAGS + 2, MetadataValue::VarInt(self.strength as i32)),
            (FLAGS + 3, MetadataValue::VarInt(self.variant as i32)),
        ]
    }

    fn chest_columns(&self) -> u8 {
        self.strength
    }

    fn refuses_rider(&self, _core: &MobCore) -> bool {
        false
    }

    fn baby_of(&self, partner: &Self, rng: &mut StdRng) -> Self {
        let mut strength = 1 + rng.gen_range(0..self.strength.max(partner.strength) as i32);
        if rng.r#gen::<f32>() < 0.03 {
            strength += 1;
        }
        Self {
            equine: Equine::bred(),
            strength: strength.clamp(1, MAX_STRENGTH) as u8,
            variant: if rng.r#gen::<bool>() {
                self.variant
            } else {
                partner.variant
            },
        }
    }
}

impl Species for Llama {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.tame = Some(super::animal::Tame::default());
        core.set_attribute_base(Attribute::MaxHealth, MIN_MAX_HEALTH);
        core.set_attribute_base(Attribute::MovementSpeed, 0.175);
        core.set_attribute_base(Attribute::JumpStrength, 0.5);
        core.goals = llama_goals(core);
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.strength =
            take_number(nbt, "Strength").map_or(1, |v| (v as i32).clamp(1, MAX_STRENGTH) as u8);
        self.variant = take_number(nbt, "Variant").map_or(0, |v| (v as i32).clamp(0, 3) as u8);
        self.equine.load(core, nbt, &LLAMA);
        let columns = if self.equine.has_chest {
            self.strength
        } else {
            0
        };
        self.equine.load_chest(columns, nbt);
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        self.equine.save(core, out, &LLAMA);
        out.insert("Strength", self.strength as i32);
        out.insert("Variant", self.variant as i32);
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityLlamaAmbient),
            hurt: Some(SoundEvent::EntityLlamaHurt),
            death: Some(SoundEvent::EntityLlamaDeath),
            step: Some(SoundEvent::EntityLlamaStep),
            volume: 0.8,
        }
    }

    equine::equine_species!();

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.equine.tick(core, cx);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let now = cx.tick;
        cx.state.get::<LlamaCaravans>().publish(core, now);
        self.equine.ai_step(core, &LLAMA, None, cx);
    }

    fn lead_speed(&self) -> f64 {
        LEAD_SPEED
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        self.baby_of(partner, rng)
    }

    fn breed_with(
        mob: &mut Mob<Self>,
        partner: &mut dyn RegionEntity,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        if let Some(trader) = partner.downcast_mut::<Mob<TraderLlama>>() {
            return equine::breed_foal(
                mob,
                trader,
                EntityType::Llama,
                |a, b, rng| a.baby_of(&b.0, rng),
                cx,
            );
        }
        let partner = partner.downcast_mut::<Mob<Self>>()?;
        equine::breed_foal(
            mob,
            partner,
            EntityType::Llama,
            |a, b, rng| a.baby_of(b, rng),
            cx,
        )
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.set_attribute_base(Attribute::MaxHealth, equine::roll_health(cx.rng));
        core.health = core.max_health();
        self.strength = roll_strength(cx.rng);
        self.variant = cx.rng.gen_range(0..4);
    }
}

/// Trader Llama: a llama that also panics faster and fights zombies and
/// illagers; its babies are trader llamas.
#[derive(Debug, Default)]
pub struct TraderLlama(Llama, Caravan);

/// A trader llama's own stay, and whether its lead holds it to a
/// wandering trader.
#[derive(Debug)]
pub struct Caravan {
    despawn_delay: i32,
    with_trader: bool,
}

const TRADER_LLAMA_DESPAWN_DELAY: i32 = 47_999;

impl Default for Caravan {
    fn default() -> Self {
        Self {
            despawn_delay: TRADER_LLAMA_DESPAWN_DELAY,
            with_trader: false,
        }
    }
}

/// The wandering trader its lead holds it to, as the region last saw it.
fn leading_trader(
    core: &MobCore,
    cx: &mut TickContext,
) -> Option<super::wandering_trader::TraderFacts> {
    let tuff_core::world::leash::Anchor::Entity(uuid) = core.leash.as_ref()?.anchor else {
        return None;
    };
    let now = cx.tick;
    cx.state
        .get::<super::wandering_trader::Caravans>()
        .trader(uuid, now)
}

impl TraderLlama {
    /// Leashes it to the wandering trader `trader` (UUID `uuid`).
    pub fn follow(&mut self, core: &mut MobCore, trader: i32, uuid: u128) {
        core.leash = Some(tuff_core::world::leash::Leash::tied(
            tuff_core::world::leash::Anchor::Entity(uuid),
            trader,
            false,
        ));
    }

    /// The UUID of what its lead is tied to, if an entity holds it.
    pub fn trader(&self, core: &MobCore) -> Option<u128> {
        match core.leash.as_ref()?.anchor {
            tuff_core::world::leash::Anchor::Entity(uuid) => Some(uuid),
            tuff_core::world::leash::Anchor::Knot(_) => None,
        }
    }

    pub fn despawn_delay(&self) -> i32 {
        self.1.despawn_delay
    }

    fn can_despawn(&self, core: &MobCore) -> bool {
        !equine::is_tame(core)
            && !(core.leash.is_some() && !self.1.with_trader)
            && !core.persistence_required
            && !core.age.as_ref().is_some_and(|a| a.locked)
            && core.base.riders.len() != 1
    }
}

/// Turns on whatever last hurt the wandering trader leading it.
#[derive(Debug, Default)]
struct DefendTrader {
    seen: Option<u64>,
    attacker: Option<i32>,
}

impl Goal for DefendTrader {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some((attacker, at)) = leading_trader(mob, cx).and_then(|t| t.hurt_by) else {
            return false;
        };
        if self.seen == Some(at) {
            return false;
        }
        self.attacker = Some(attacker);
        super::target::view(cx, attacker).is_some_and(|t| mob.can_attack(&t, cx))
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target
            .and_then(|id| super::target::view(cx, id))
            .is_some_and(|t| t.alive && mob.can_attack(&t, cx))
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.target = self.attacker;
        self.seen = leading_trader(mob, cx)
            .and_then(|t| t.hurt_by)
            .map(|(_, at)| at);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = None;
    }
}

impl Species for TraderLlama {
    fn create(rng: &mut StdRng) -> Self {
        Self(Llama::create(rng), Caravan::default())
    }

    fn configure(core: &mut MobCore) {
        Llama::configure(core);
        let goals = &mut core.goals;
        goals.add(1, Panic::new(2.0));
        goals.add(1, DefendTrader::default());
        goals.add(2, NearestAttackableEntity::new(ZOMBIES, true));
        goals.add(2, NearestAttackableEntity::new(ILLAGERS, true));
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.1.despawn_delay = tuff_core::world::entity::take_number(nbt, "DespawnDelay")
            .map_or(TRADER_LLAMA_DESPAWN_DELAY, |v| v as i32);
        self.0.load(core, nbt);
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        self.0.save(core, out);
        out.insert("DespawnDelay", self.1.despawn_delay);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        self.0.metadata(core)
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        self.0.sounds(core)
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        self.0.is_food(stack)
    }

    fn can_fall_in_love(&self, _core: &MobCore) -> bool {
        false
    }

    fn seats(&self, core: &MobCore) -> usize {
        if self.1.with_trader {
            return 0;
        }
        self.0.seats(core)
    }

    fn drivable(&self, core: &MobCore) -> bool {
        self.0.drivable(core)
    }

    fn drive_input(&self, core: &MobCore, input: RiderInput) -> (f32, f32) {
        self.0.drive_input(core, input)
    }

    fn hops_when_ridden(&self) -> bool {
        false
    }

    fn lead_speed(&self) -> f64 {
        LEAD_SPEED
    }

    fn ride_speed(&mut self, core: &mut MobCore, input: &RiderInput, cx: &mut TickContext) -> f32 {
        self.0.ride_speed(core, input, cx)
    }

    fn ridden_speed(&self, core: &MobCore) -> Option<tuff_core::world::entity::RiddenSpeed> {
        self.0.ridden_speed(core)
    }

    fn mount_refused(&self, core: &MobCore, stack: Option<&ItemStack>) -> bool {
        self.0.mount_refused(core, stack)
    }

    fn inventory(&self) -> Option<(&[Option<ItemStack>], tuff_core::world::menu::MenuKind)> {
        self.0.inventory()
    }

    fn inventory_mut(&mut self) -> Option<&mut [Option<ItemStack>]> {
        self.0.inventory_mut()
    }

    fn inventory_opens_for(&self, core: &MobCore, player: i32, sneaking: bool) -> bool {
        self.0.inventory_opens_for(core, player, sneaking)
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        self.0.interact(core, interaction, cx)
    }

    fn dispensed_on(
        &mut self,
        core: &mut MobCore,
        stack: &ItemStack,
        cx: &mut TickContext,
    ) -> Option<tuff_core::world::region_api::DispenseOutcome> {
        self.0.dispensed_on(core, stack, cx)
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.0.tick(core, cx);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.0.ai_step(core, cx);
        let trader = leading_trader(core, cx);
        self.1.with_trader = trader.is_some();
        if self.can_despawn(core) {
            self.1.despawn_delay = match trader {
                Some(trader) => trader.despawn_delay - 1,
                None => self.1.despawn_delay - 1,
            };
            if self.1.despawn_delay <= 0 {
                core.leash = None;
                core.discarded = true;
            }
        }
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &super::Damage, cx: &mut TickContext) {
        self.0.after_hurt(core, damage, cx);
    }

    fn died(&mut self, core: &mut MobCore, damage: &super::Damage, cx: &mut TickContext) {
        self.0.died(core, damage, cx);
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        Self(self.0.baby_of(&partner.0, rng), Caravan::default())
    }

    fn breed_with(
        mob: &mut Mob<Self>,
        partner: &mut dyn RegionEntity,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        let baby =
            |a: &Self, b: &Llama, rng: &mut StdRng| Self(a.0.baby_of(b, rng), Caravan::default());
        let mut child = if let Some(llama) = partner.downcast_mut::<Mob<Llama>>() {
            equine::breed_foal(mob, llama, EntityType::TraderLlama, baby, cx)?
        } else {
            let partner = partner.downcast_mut::<Mob<Self>>()?;
            equine::breed_foal(
                mob,
                partner,
                EntityType::TraderLlama,
                |a, b, rng| baby(a, &b.0, rng),
                cx,
            )?
        };
        if let Some(child) = child.downcast_mut::<Mob<Self>>() {
            child.core.persistence_required = true;
        }
        Some(child)
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.0.finalize_spawn(core, cx);
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Llama, super::load::<Llama>);
    registry.register(EntityType::TraderLlama, super::load::<TraderLlama>);
    registry.register(EntityType::LlamaSpit, super::llama_spit::load);
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;

    use super::*;
    use tuff_core::world::entity::EntityBase;
    use tuff_nbt::Tag;

    #[tuff_core::watchdog]
    #[test]
    fn strength_rolls_stay_in_range() {
        let mut rng = StdRng::seed_from_u64(5);
        for _ in 0..500 {
            let strength = roll_strength(&mut rng);
            assert!((1..=5).contains(&strength), "{strength}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_saddle_given_by_nbt_still_lets_a_tamed_llama_be_driven() {
        let base = EntityBase::new(EntityType::Llama, 1, 1, [0.0; 3], [0.0; 3]);
        let mut nbt = Compound::new().with("Owner", Tag::uuid(9)).with(
            "equipment",
            Compound::new().with("saddle", ItemStack::new(Item::Saddle, 1).to_nbt()),
        );
        let entity = super::super::load::<Llama>(base, &mut nbt).expect("loads");
        assert!(entity.rideable_by_input());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_strong_llamas_chest_has_a_column_per_strength_and_takes_carpets() {
        super::super::tests::with_context(|_, cx| {
            let base = EntityBase::new(EntityType::Llama, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
            let mut nbt = Compound::new()
                .with("Owner", Tag::uuid(77))
                .with("Strength", 4);
            let mut llama = super::super::load::<Llama>(base, &mut nbt).expect("loads");
            let chest = EntityInteraction {
                player: 5,
                player_uuid: 77,
                position: [0.0; 3],
                location: [0.0; 3],
                stack: Some(ItemStack::new(Item::Chest, 1)),
                off_hand: false,
                creative: false,
                sneaking: false,
                riding: false,
                hero: None,
            };
            assert!(llama.mount_refused_by(chest.stack.as_ref()));
            llama.interact(&chest, cx);
            assert_eq!(llama.container().unwrap().len(), 2 + 4 * 3);
            let carpet = EntityInteraction {
                stack: Some(ItemStack::new(Item::RedCarpet, 1)),
                ..chest.clone()
            };
            assert!(llama.mount_refused_by(carpet.stack.as_ref()));
            assert!(llama.interact(&carpet, cx).is_some());
            let saddle = EntityInteraction {
                stack: Some(ItemStack::new(Item::Saddle, 1)),
                ..chest
            };
            assert!(
                !llama.mount_refused_by(saddle.stack.as_ref()),
                "llamas take no saddle"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn llama_hay_heals_and_makes_a_tamed_adult_fall_in_love() {
        super::super::tests::with_context(|core, cx| {
            Llama::configure(core);
            core.tame.as_mut().unwrap().owner = Some(77);
            let mut equine = Equine::default();
            assert!(equine.feed(core, &LLAMA, Item::HayBlock, 77, cx));
            assert!(core.love.as_ref().unwrap().is_in_love());
            assert!(!equine.feed(core, &LLAMA, Item::Sugar, 77, cx));
        });
    }
}
