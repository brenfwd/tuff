//! Villagers: a type (from the biome they were born in), a profession and
//! a level, the offers their profession's trade sets give them, experience
//! from trades, restocking, prices that follow demand and what they gossip
//! about players, food in an eight-slot inventory, and babies.

use tuff_item::item::BurnReason;
pub mod brain;
pub mod gossip;

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{
    CustomStat, EntityType, MobEffect, SoundEvent, VillagerProfession, VillagerType,
};
use tuff_vanilla_data::villager::LEVEL_XP;
use tuff_vanilla_data::{Item, tag};

use self::gossip::Gossips;
use super::goal::GoalSelector;
use super::goals::{Float, LookAtPlayer, RandomLookAround};
use super::{Damage, MobCore, MobSounds, Species, data};
use crate::world::trading::{self, MerchantOffer, MerchantView, TradeRequest};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{Effect, EntityInteraction, InteractOutcome, TickContext, take};
use tuff_core::world::messages::{HandEffect, PlayerEvent};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};

/// Data after the ageable mob's: the head shake timer, then the
/// villager's data and whether it is final.
pub const UNHAPPY: u8 = data::SPECIES;
pub const VILLAGER_DATA: u8 = data::SPECIES + 1;
pub const DATA_FINALIZED: u8 = data::SPECIES + 2;
const POSE: u8 = tuff_core::protocol::play::metadata::POSE;
const SLEEPING_POS: u8 = tuff_core::protocol::play::metadata::SLEEPING_POS;

pub mod event {
    pub const HEARTS: i8 = 12;
    pub const ANGRY: i8 = 13;
    pub const HAPPY: i8 = 14;
    pub const SPLASH: i8 = 42;
}

pub const MIN_LEVEL: i32 = 1;
pub const MAX_LEVEL: i32 = 5;
const INVENTORY_SLOTS: usize = 8;
const UNHAPPY_TICKS: i32 = 40;
const GOSSIP_DECAY_INTERVAL: u64 = 24000;
const HALF_DAY: u64 = 12000;
const RESTOCK_COOLDOWN: u64 = 2400;
const RESTOCKS_PER_DAY: i32 = 2;
const LEVEL_UP_REGENERATION_TICKS: i32 = 200;
const REGENERATION_INTERVAL: i32 = 50;
const TRADE_GOSSIP: i32 = 2;
const EVENT_GOSSIP: i32 = 25;
const CURED_MAJOR_GOSSIP: i32 = 20;
const WITNESS_RANGE: f64 = 16.0;
pub const BREEDING_FOOD_THRESHOLD: i32 = 12;

/// A villager's type, profession and level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VillagerData {
    pub kind: VillagerType,
    pub profession: VillagerProfession,
    pub level: i32,
}

/// Messages entities in one region leave each other, read by the
/// recipient on its own tick. Mail nobody took within a tick (the
/// recipient died or left the region) is dropped.
pub struct Mail<T>(std::collections::HashMap<i32, (u64, Vec<T>)>);

impl<T> Default for Mail<T> {
    fn default() -> Self {
        Self(std::collections::HashMap::new())
    }
}

impl<T> Mail<T> {
    pub fn send(&mut self, tick: u64, to: i32, message: T) {
        self.0.retain(|_, (sent, _)| *sent + 1 >= tick);
        let (sent, messages) = self.0.entry(to).or_insert((tick, Vec::new()));
        *sent = tick;
        messages.push(message);
    }

    pub fn take(&mut self, to: i32) -> Vec<T> {
        self.0
            .remove(&to)
            .map(|(_, messages)| messages)
            .unwrap_or_default()
    }
}

/// What villagers tell each other about everyone.
pub type GossipMail = Mail<gossip::Gossips>;
/// Reputation events a villager heard about someone.
pub type ReputationMail = Mail<(Reputation, u128)>;

impl Default for VillagerData {
    fn default() -> Self {
        Self {
            kind: tuff_vanilla_data::villager::DEFAULT_TYPE,
            profession: VillagerProfession::None,
            level: MIN_LEVEL,
        }
    }
}

impl VillagerData {
    pub fn to_nbt(self) -> Compound {
        Compound::new()
            .with("type", self.kind.name())
            .with("profession", self.profession.name())
            .with("level", self.level)
    }

    pub fn from_nbt(nbt: &Compound) -> Self {
        let default = Self::default();
        Self {
            kind: nbt
                .get_str("type")
                .and_then(VillagerType::from_name)
                .unwrap_or(default.kind),
            profession: nbt
                .get_str("profession")
                .and_then(VillagerProfession::from_name)
                .unwrap_or(default.profession),
            level: nbt.get_int("level").unwrap_or(MIN_LEVEL).max(MIN_LEVEL),
        }
    }

    pub fn metadata(self, kind: EntityType, index: u8) -> MetadataValue {
        MetadataValue::VillagerData {
            serializer: kind.data_serializer(index).unwrap_or(0),
            kind: self.kind.id() as i32,
            profession: self.profession.id() as i32,
            level: self.level,
        }
    }

    pub fn can_level_up(self) -> bool {
        (MIN_LEVEL..MAX_LEVEL).contains(&self.level)
    }

    /// Experience needed to reach the next level.
    pub fn next_level_xp(self) -> i32 {
        if self.can_level_up() {
            LEVEL_XP[self.level as usize]
        } else {
            0
        }
    }
}

/// Reputation events a villager hears about someone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reputation {
    ZombieVillagerCured,
    Trade,
    VillagerHurt,
    VillagerKilled,
}

/// The player at a villager's trading screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trader {
    pub id: i32,
    pub uuid: u128,
    /// Their Hero of the Village amplifier when they started.
    pub hero: Option<u8>,
}

#[derive(Debug, Default)]
pub struct Villager {
    pub data: VillagerData,
    pub finalized: bool,
    /// `None` until it first needs them.
    pub offers: Option<Vec<MerchantOffer>>,
    pub xp: i32,
    pub food_level: i32,
    pub gossips: Gossips,
    pub last_restock: i64,
    pub restocks_today: i32,
    last_restock_check_day: i64,
    pub last_gossip_decay: i64,
    pub inventory: Vec<Option<ItemStack>>,
    pub unhappy: i32,
    pub trading: Option<Trader>,
    last_traded: Option<Trader>,
    regeneration: i32,
    /// Lightning struck it this tick.
    struck: bool,
    pub brain: brain::Brain,
    /// Explorer map offers waiting for their map, added once it is made.
    exploring: Vec<(tuff_core::world::exploration::Answer, MerchantOffer)>,
}

/// The random source of one villager's trade draws: fixed by who it is, its
/// level and how many offers it already has, so the region's own draws
/// never change what it sells.
fn trade_rng(uuid: u128, level: i32, held: usize) -> StdRng {
    use rand::SeedableRng;
    let mixed = (uuid as u64) ^ ((uuid >> 64) as u64);
    let step = (u64::from(level.unsigned_abs()) << 32) | held as u64;
    StdRng::seed_from_u64(mixed.rotate_left(17) ^ step.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

/// A draw from the villager's own random source for cosmetic odds, so
/// they never shift the region's draws.
fn own_roll(uuid: u128, tick: u64, bound: u64) -> u64 {
    let mut z =
        (uuid as u64 ^ (uuid >> 64) as u64).wrapping_add(tick.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) % bound
}

/// Its goals only look around; its brain does the rest.
fn goals() -> GoalSelector {
    let mut goals = GoalSelector::default();
    goals.add(0, Float);
    goals.add(6, LookAtPlayer::new(8.0));
    goals.add(7, RandomLookAround::default());
    goals
}

impl Villager {
    /// Its offers, drawing the first ones when it has none yet.
    pub fn offers(&mut self, core: &MobCore, rng: &mut StdRng) -> &mut Vec<MerchantOffer> {
        if self.offers.is_none() {
            self.offers = Some(Vec::new());
            self.update_trades(core, rng);
        }
        self.offers.as_mut().expect("just set")
    }

    /// Adds the offers of its current level's trade set.
    fn update_trades(&mut self, core: &MobCore, _region_rng: &mut StdRng) {
        let data = self.data;
        let Some(set) = data
            .profession
            .info()
            .trade_sets
            .get((data.level - 1) as usize)
            .copied()
            .flatten()
        else {
            return;
        };
        let offers = self.offers.get_or_insert_with(Vec::new);
        let mut rng = trade_rng(core.base.uuid, data.level, offers.len());
        trading::add_offers_from_set(set.name, Some(data.kind), offers, &mut rng);
    }

    /// Sends explorer map offers off for their maps and adds those that
    /// came back; one whose structure was not found is dropped.
    fn explore_offers(&mut self, core: &MobCore, cx: &TickContext) {
        use tuff_core::world::exploration::{ExplorationMaps, pending, poll};
        let Some(offers) = self.offers.as_mut() else {
            return;
        };
        self.exploring
            .retain_mut(|(answer, offer)| match poll(answer) {
                None => true,
                Some(Some(map)) => {
                    offer
                        .sell
                        .edit_components(|components| map.apply(components));
                    offers.push(offer.clone());
                    false
                }
                Some(None) => false,
            });
        if !offers.iter().any(|offer| pending(&offer.sell).is_some()) {
            return;
        }
        let maps = cx.shared.services.get::<ExplorationMaps>();
        let [x, y, z] = core.base.position;
        let origin =
            tuff_world_model::BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let mut index = 0;
        while index < offers.len() {
            let Some(spec) = pending(&offers[index].sell) else {
                index += 1;
                continue;
            };
            let offer = offers.remove(index);
            if let Some(answer) = maps.and_then(|maps| maps.request(cx.dimension, origin, spec)) {
                self.exploring.push((answer, offer));
            }
        }
    }

    pub fn set_data(&mut self, data: VillagerData, core: &mut MobCore) {
        if data.profession != self.data.profession {
            self.offers = None;
        }
        if data != self.data {
            self.data = data;
            core.changed(VILLAGER_DATA);
        }
    }

    fn set_unhappy(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.unhappy = UNHAPPY_TICKS;
        core.changed(UNHAPPY);
        core.sound(
            SoundEvent::EntityVillagerNo,
            1.0,
            core.voice_pitch(cx.rng),
            cx,
        );
    }

    pub fn view(&self, core: &MobCore) -> MerchantView {
        MerchantView {
            merchant: core.base.id,
            offers: self.offers.clone().unwrap_or_default(),
            level: self.data.level,
            xp: self.xp,
            regular: true,
            can_restock: true,
        }
    }

    /// The name its trading screen shows.
    fn title(&self, core: &MobCore) -> TextComponent {
        if let Some(name) = core
            .base
            .custom_name
            .as_ref()
            .and_then(TextComponent::from_nbt)
        {
            return name;
        }
        let profession = self.data.profession.name();
        let path = profession.strip_prefix("minecraft:").unwrap_or(profession);
        TextComponent::translate(format!("entity.minecraft.villager.{path}"), [])
    }

    /// Reputation and Hero of the Village discounts for the trading
    /// player, sent to their open screen.
    fn update_special_prices(&mut self, core: &MobCore, cx: &mut TickContext) {
        if self.apply_special_prices(core, cx.rng)
            && let Some(trader) = self.trading
        {
            let view = self.view(core);
            cx.effects.push(Effect::TellPlayer {
                player: trader.id,
                event: crate::merchant_events::update(view),
            });
        }
    }

    /// Sets the special prices for the trading player; false when nobody
    /// trades.
    fn apply_special_prices(&mut self, core: &MobCore, rng: &mut StdRng) -> bool {
        let Some(trader) = self.trading else {
            return false;
        };
        let reputation = self.gossips.reputation(trader.uuid);
        let hero = trader.hero.map_or(0.0, |amplifier| {
            (0.3f32 + 0.0625f32 * amplifier as f32) as f64
        });
        let offers = self.offers(core, rng);
        for offer in offers.iter_mut() {
            offer.special_price = 0;
            if reputation != 0 {
                offer.special_price -= (reputation as f32 * offer.price_multiplier).floor() as i32;
            }
            if hero > 0.0 {
                let reduction = (hero * offer.buy.count as f64).floor() as i32;
                offer.special_price -= reduction.max(1);
            }
        }
        true
    }

    fn stop_trading_now(&mut self, core: &MobCore, cx: &mut TickContext) {
        if let Some(trader) = self.trading.take() {
            cx.effects.push(Effect::TellPlayer {
                player: trader.id,
                event: crate::merchant_events::closed(core.base.id),
            });
        }
    }

    fn start_trading(&mut self, trader: Trader, core: &MobCore, cx: &mut TickContext) {
        self.trading = Some(trader);
        self.apply_special_prices(core, cx.rng);
        let view = self.view(core);
        cx.effects.push(Effect::TellPlayer {
            player: trader.id,
            event: crate::merchant_events::open(self.title(core), view),
        });
    }

    /// A trade of `offer` went through: uses, experience, perhaps a new
    /// level, and an experience orb.
    fn notify_trade(&mut self, core: &mut MobCore, offer: usize, cx: &mut TickContext) {
        let Some(offers) = self.offers.as_mut() else {
            return;
        };
        let offer = &mut offers[offer];
        offer.uses += 1;
        let (xp, reward) = (offer.xp, offer.reward_exp);
        core.ambient_sound_time = -super::DEFAULT_AMBIENT_INTERVAL;
        let mut pop = 3 + cx.rng.gen_range(0..4);
        self.xp += xp;
        self.last_traded = self.trading;
        if self.data.can_level_up() && self.xp >= self.data.next_level_xp() {
            let data = VillagerData {
                level: self.data.level + 1,
                ..self.data
            };
            self.set_data(data, core);
            self.update_trades(core, cx.rng);
            self.update_special_prices(core, cx);
            self.regeneration = LEVEL_UP_REGENERATION_TICKS;
            core.changed(tuff_core::protocol::play::metadata::EFFECT_PARTICLES);
            pop += 5;
        }
        if reward {
            let [x, y, z] = core.base.position;
            cx.effects.push(Effect::Experience {
                position: [x, y + 0.5, z],
                amount: pop,
            });
        }
    }

    /// Whether any offer has been used since the last restock.
    fn needs_restock(&self) -> bool {
        self.offers
            .as_ref()
            .is_some_and(|offers| offers.iter().any(MerchantOffer::needs_restock))
    }

    fn allowed_to_restock(&self, now: u64) -> bool {
        self.restocks_today == 0
            || (self.restocks_today < RESTOCKS_PER_DAY
                && now > self.last_restock as u64 + RESTOCK_COOLDOWN)
    }

    /// Whether to restock now: at most twice a day, 2400 ticks apart. A new
    /// day (half a day since the last restock, or the day count moved on)
    /// first catches up on the demand of restocks it missed.
    pub fn should_restock(&mut self, core: &MobCore, cx: &mut TickContext) -> bool {
        let now = cx.tick;
        let day = overworld_day(cx);
        let new_day = now > self.last_restock as u64 + HALF_DAY
            || (self.last_restock_check_day > 0 && day > self.last_restock_check_day);
        self.last_restock_check_day = day;
        if new_day {
            self.last_restock = now as i64;
            self.catch_up_demand(core, cx);
            self.restocks_today = 0;
        }
        self.allowed_to_restock(now) && self.needs_restock()
    }

    fn catch_up_demand(&mut self, core: &MobCore, cx: &mut TickContext) {
        let missed = RESTOCKS_PER_DAY - self.restocks_today;
        let offers = self.offers(core, cx.rng);
        if missed > 0 {
            for offer in offers.iter_mut() {
                offer.uses = 0;
            }
        }
        for _ in 0..missed {
            for offer in offers.iter_mut() {
                offer.update_demand();
            }
        }
        self.update_special_prices(core, cx);
    }

    /// Demand moves with the day's sales and every offer is back in stock.
    pub fn restock(&mut self, core: &MobCore, cx: &mut TickContext) {
        let offers = self.offers(core, cx.rng);
        for offer in offers.iter_mut() {
            offer.update_demand();
            offer.uses = 0;
        }
        self.update_special_prices(core, cx);
        self.last_restock = cx.tick as i64;
        self.restocks_today += 1;
    }

    /// Hears about `source`.
    pub fn reputation_event(
        &mut self,
        core: &MobCore,
        event: Reputation,
        source: u128,
        cx: &mut TickContext,
    ) {
        let kind = |name| gossip::kind(name);
        match event {
            Reputation::ZombieVillagerCured => {
                self.gossips
                    .add(source, kind("major_positive"), CURED_MAJOR_GOSSIP);
                self.gossips
                    .add(source, kind("minor_positive"), EVENT_GOSSIP);
            }
            Reputation::Trade => self.gossips.add(source, kind("trading"), TRADE_GOSSIP),
            Reputation::VillagerHurt => {
                self.gossips
                    .add(source, kind("minor_negative"), EVENT_GOSSIP)
            }
            Reputation::VillagerKilled => {
                self.gossips
                    .add(source, kind("major_negative"), EVENT_GOSSIP)
            }
        }
        if self.trading.is_some_and(|t| t.uuid == source) {
            self.update_special_prices(core, cx);
        }
    }

    /// Food points in its inventory.
    pub fn food_in_inventory(&self) -> i32 {
        self.inventory
            .iter()
            .flatten()
            .filter_map(|s| s.item.villager_food().map(|n| n as i32 * s.count() as i32))
            .sum()
    }

    pub fn can_breed(&self, core: &MobCore) -> bool {
        self.food_level + self.food_in_inventory() >= BREEDING_FOOD_THRESHOLD
            && core.age.as_ref().is_some_and(|a| a.age == 0)
    }

    /// Whether it would pick up `stack`: food, what its profession wants,
    /// or the items every villager takes, if it has room.
    pub fn wants(&self, stack: &ItemStack) -> bool {
        let wanted = tuff_datapack::tags::is(tag::item::key::VILLAGER_PICKS_UP, stack.item)
            || stack.item.villager_food().is_some()
            || self
                .data
                .profession
                .info()
                .requested_items
                .contains(&stack.item);
        wanted && self.room_for(stack) > 0
    }

    /// How many of `stack` its inventory can take.
    pub fn room_for(&self, stack: &ItemStack) -> u32 {
        let max = stack.max_stack_size() as u32;
        self.inventory
            .iter()
            .map(|slot| match slot {
                None => max,
                Some(s) if s.stacks_with(stack) => max.saturating_sub(s.count() as u32),
                Some(_) => 0,
            })
            .sum()
    }

    /// Adds to the inventory; returns what did not fit.
    pub fn add_item(&mut self, mut stack: ItemStack) -> Option<ItemStack> {
        let max = stack.max_stack_size();
        for slot in self.inventory.iter_mut().flatten() {
            if slot.stacks_with(&stack) && slot.count() < max {
                let room = max - slot.count();
                slot.transfer(&mut stack, room);
                if stack.count() == 0 {
                    return None;
                }
            }
        }
        for slot in self.inventory.iter_mut() {
            if slot.is_none() {
                *slot = Some(stack);
                return None;
            }
        }
        Some(stack)
    }

    /// Eats from its inventory until it has twelve food points, then uses
    /// them up (breeding).
    pub fn eat_and_digest(&mut self) {
        for slot in self.inventory.iter_mut() {
            if self.food_level >= BREEDING_FOOD_THRESHOLD {
                break;
            }
            let Some(stack) = slot else {
                continue;
            };
            let Some(nutrition) = stack.item.villager_food() else {
                continue;
            };
            while stack.count() > 0 && self.food_level < BREEDING_FOOD_THRESHOLD {
                self.food_level += nutrition as i32;
                stack.shrink(1, BurnReason::Consumed);
            }
            if stack.count() == 0 {
                *slot = None;
            }
        }
        self.food_level -= BREEDING_FOOD_THRESHOLD;
    }

    fn tell_witnesses(&self, core: &MobCore, murderer: u128, cx: &mut TickContext) {
        let range = WITNESS_RANGE * WITNESS_RANGE;
        let witnesses: Vec<i32> = cx
            .entities
            .iter()
            .filter(|e| e.kind == EntityType::Villager && e.id != core.base.id)
            .filter(|e| e.living.as_ref().is_some_and(|l| l.alive))
            .filter(|e| tuff_core::world::distance_sq(e.position, core.base.position) <= range)
            .filter(|e| {
                let eye = e.living.as_ref().map_or(0.0, |l| f64::from(l.eye_height));
                core.can_see_point([e.position[0], e.position[1] + eye, e.position[2]], cx)
            })
            .map(|e| e.id)
            .collect();
        for target in witnesses {
            cx.state.get::<ReputationMail>().send(
                cx.tick,
                target,
                (Reputation::VillagerKilled, murderer),
            );
        }
    }

    /// Lightning makes a witch of it, except in peaceful.
    fn turn_into_witch(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        if !core.is_alive()
            || *cx.shared.difficulty.borrow() == tuff_core::world::player::Difficulty::Peaceful
        {
            return false;
        }
        let mut witch = super::convert::<super::witch::Witch>(core, EntityType::Witch, false, cx);
        witch.core.left_handed = cx.rng.r#gen::<f32>() < 0.05;
        super::raider::finalize_spawn(&mut witch.core, cx);
        witch.species.finalize_spawn(&mut witch.core, cx);
        witch.core.persistence_required = true;
        witch.core.changed.clear();
        witch.core.equipment.take_changed();
        self.wake_up(core, cx);
        self.stop_trading_now(core, cx);
        self.brain.memories.home = None;
        self.brain.memories.job_site = None;
        self.brain.memories.potential_job_site = None;
        self.brain.memories.meeting_point = None;
        cx.effects.push(Effect::Spawn(Box::new(witch)));
        core.discarded = true;
        true
    }

    fn regeneration_particles(&self) -> MetadataValue {
        let particles = if self.regeneration > 0 {
            let effect = tuff_core::world::effects::ActiveEffect::timed(
                MobEffect::Regeneration,
                self.regeneration as i64,
                0,
            );
            vec![effect.particle()]
        } else {
            Vec::new()
        };
        MetadataValue::Particles(particles)
    }
}

/// Whole overworld days on the day timeline.
fn overworld_day(cx: &TickContext) -> i64 {
    let Some(timeline) = tuff_vanilla_data::timeline::Timeline::get("minecraft:day") else {
        return 0;
    };
    let Some(period) = timeline.period_ticks.filter(|p| *p > 0) else {
        return 0;
    };
    let ticks = tuff_core::world::clock::clock_id(timeline.clock)
        .map_or(0, |id| cx.shared.clocks.borrow().get(id).total_ticks);
    ticks.div_euclid(period as i64)
}

impl Species for Villager {
    fn take_items(&mut self) -> Vec<ItemStack> {
        self.inventory.iter_mut().filter_map(Option::take).collect()
    }

    const RANDOM_BABIES: bool = false;

    fn debug_value(
        &self,
        core: &MobCore,
        feed: tuff_vanilla_data::registry::DebugSubscription,
        _tick: u64,
    ) -> Option<tuff_core::protocol::play::DebugValue> {
        if feed != tuff_vanilla_data::registry::DebugSubscription::Brains {
            return None;
        }
        let memories = &self.brain.memories;
        let mut remembered = Vec::new();
        for (key, place) in [
            ("home", &memories.home),
            ("job_site", &memories.job_site),
            ("meeting_point", &memories.meeting_point),
            ("potential_job_site", &memories.potential_job_site),
        ] {
            if let Some(place) = place {
                let p = place.pos;
                remembered.push(format!("{key}: {} {} {}", p.x, p.y, p.z));
            }
        }
        remembered.sort();
        let mut activities = vec!["core".to_owned()];
        if let Some(activity) = self.brain.activity {
            let name = activity.name();
            activities.push(name.strip_prefix("minecraft:").unwrap_or(name).to_owned());
        }
        let pois = [&memories.job_site, &memories.home, &memories.meeting_point]
            .into_iter()
            .flatten()
            .map(|p| p.pos)
            .collect();
        Some(tuff_core::protocol::play::DebugValue::Brain(Box::new(
            tuff_core::protocol::play::DebugBrain {
                name: tuff_core::world::debug_feeds::entity_name(&core.base),
                profession: self.data.profession.name().to_owned(),
                xp: self.xp,
                health: core.health,
                max_health: core.max_health(),
                inventory: String::new(),
                wants_golem: false,
                anger_level: -1,
                activities,
                behaviors: Vec::new(),
                memories: remembered,
                gossips: self.gossips.debug_lines(),
                pois,
                potential_pois: memories.potential_job_site.iter().map(|p| p.pos).collect(),
            },
        )))
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self {
            inventory: (0..INVENTORY_SLOTS).map(|_| None).collect(),
            ..Self::default()
        }
    }

    fn configure(core: &mut MobCore) {
        core.age = Some(super::animal::Age::default());
        core.set_path_malus(tuff_vanilla_data::entity::PathType::FireInNeighbor, 16.0);
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Fire, -1.0);
        core.navigation.can_open_doors = true;
        core.navigation.can_float = true;
        core.set_required_path_length(48.0);
        core.goals = goals();
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        let data = take(nbt, "VillagerData", |t| {
            t.as_compound().map(VillagerData::from_nbt)
        });
        let finalized = take(nbt, "VillagerDataFinalized", |t| {
            tuff_core::world::entity::number(t).map(|v| v != 0.0)
        })
        .unwrap_or(false);
        if finalized || data.is_some() {
            self.finalized = true;
            self.data = data.unwrap_or_default();
        }
        self.offers = take(nbt, "Offers", |t| {
            t.as_compound().map(trading::offers_from_nbt)
        });
        self.food_level =
            take(nbt, "FoodLevel", tuff_core::world::entity::number).map_or(0, |v| v as i8 as i32);
        self.gossips = take(nbt, "Gossips", Gossips::from_nbt).unwrap_or_default();
        self.xp = take(nbt, "Xp", tuff_core::world::entity::number).map_or(0, |v| v as i32);
        self.last_restock =
            take(nbt, "LastRestock", tuff_core::world::entity::number).map_or(0, |v| v as i64);
        self.last_gossip_decay =
            take(nbt, "LastGossipDecay", tuff_core::world::entity::number).map_or(0, |v| v as i64);
        self.restocks_today =
            take(nbt, "RestocksToday", tuff_core::world::entity::number).map_or(0, |v| v as i32);
        if let Some(items) = take(nbt, "Inventory", |t| t.as_list().map(<[Tag]>::to_vec)) {
            for item in items {
                if let Some(stack) = item.as_compound().and_then(ItemStack::from_storage)
                    && let Some(rest) = self.add_item(stack)
                {
                    rest.burn(tuff_item::item::BurnReason::Discarded);
                }
            }
        }
        if let Some(saved) = take(nbt, "Brain", |t| t.as_compound().cloned()) {
            self.brain.memories = brain::Memories::load(&saved);
        }
        let coord = |nbt: &mut Compound, key| {
            take(nbt, key, tuff_core::world::entity::number).map(|v| v as i32)
        };
        let (x, y, z) = (
            coord(nbt, "SleepingX"),
            coord(nbt, "SleepingY"),
            coord(nbt, "SleepingZ"),
        );
        if let (Some(x), Some(y), Some(z)) = (x, y, z) {
            self.brain.sleeping = Some(tuff_world_model::BlockPos::new(x, y, z));
        }
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        if let Some(offers) = &self.offers {
            out.insert("Offers", trading::offers_to_nbt(offers));
        }
        let items: Vec<Compound> = self
            .inventory
            .iter()
            .flatten()
            .map(ItemStack::to_nbt)
            .collect();
        out.insert("Inventory", items);
        out.insert("VillagerData", self.data.to_nbt());
        out.insert("VillagerDataFinalized", self.finalized);
        out.insert("FoodLevel", self.food_level as i8);
        out.insert("Gossips", self.gossips.to_nbt());
        out.insert("Xp", self.xp);
        out.insert("LastRestock", self.last_restock);
        out.insert("LastGossipDecay", self.last_gossip_decay);
        out.insert("RestocksToday", self.restocks_today);
        out.insert("Brain", self.brain.memories.save());
        if let Some(bed) = self.brain.sleeping {
            out.insert("SleepingX", bed.x);
            out.insert("SleepingY", bed.y);
            out.insert("SleepingZ", bed.z);
        }
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut values = vec![
            (UNHAPPY, MetadataValue::VarInt(self.unhappy)),
            (
                VILLAGER_DATA,
                self.data.metadata(core.kind(), VILLAGER_DATA),
            ),
            (DATA_FINALIZED, MetadataValue::Boolean(self.finalized)),
        ];
        if self.regeneration > 0 {
            values.push((
                tuff_core::protocol::play::metadata::EFFECT_PARTICLES,
                self.regeneration_particles(),
            ));
        }
        if self.brain.sleeping.is_some() {
            for index in [POSE, SLEEPING_POS] {
                values.extend(self.changed_metadata(core, index).map(|v| (index, v)));
            }
        }
        values
    }

    fn changed_metadata(&self, _core: &MobCore, index: u8) -> Option<MetadataValue> {
        use tuff_core::protocol::play::metadata;
        match index {
            metadata::EFFECT_PARTICLES => Some(self.regeneration_particles()),
            POSE => {
                use tuff_vanilla_data::entity::Pose;
                let pose = if self.brain.sleeping.is_some() {
                    Pose::Sleeping
                } else {
                    Pose::Standing
                };
                Some(MetadataValue::Pose(i32::from(pose.id())))
            }
            SLEEPING_POS => Some(MetadataValue::OptionalPosition(self.brain.sleeping)),
            _ => None,
        }
    }

    fn heard(&mut self, _core: &mut MobCore, notice: tuff_core::world::entity::Notice) {
        match notice {
            tuff_core::world::entity::Notice::Courted { by } => self.courted(by),
            tuff_core::world::entity::Notice::BellRung => self.heard_bell(),
            tuff_core::world::entity::Notice::Charged => self.struck = true,
            _ => {}
        }
    }

    fn poi_claims(&self, _core: &MobCore) -> Vec<tuff_world_model::BlockPos> {
        self.brain.memories.claims()
    }

    fn sleeping_in(&self, _core: &MobCore) -> Option<tuff_world_model::BlockPos> {
        self.brain.sleeping
    }

    fn wake_from_bed(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.wake_up(core, cx);
    }

    fn breed(
        mob: &mut super::Mob<Self>,
        partner: &mut super::Mob<Self>,
        cx: &mut TickContext,
    ) -> Option<Box<dyn tuff_core::world::entity::RegionEntity>> {
        breed(mob, partner, cx)
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: if self.brain.sleeping.is_some() {
                None
            } else if self.trading.is_some() {
                Some(SoundEvent::EntityVillagerTrade)
            } else {
                Some(SoundEvent::EntityVillagerAmbient)
            },
            hurt: Some(SoundEvent::EntityVillagerHurt),
            death: Some(SoundEvent::EntityVillagerDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if std::mem::take(&mut self.struck) && self.turn_into_witch(core, cx) {
            return;
        }
        let mail = cx.state.get::<GossipMail>().take(core.base.id);
        for gossips in mail {
            self.hear_gossip(core, &gossips, cx);
        }
        if self.unhappy > 0 {
            self.unhappy -= 1;
            core.changed(UNHAPPY);
        }
        let now = cx.tick as i64;
        if self.last_gossip_decay == 0 {
            self.last_gossip_decay = now;
        } else if now >= self.last_gossip_decay + GOSSIP_DECAY_INTERVAL as i64 {
            self.gossips.decay();
            self.last_gossip_decay = now;
        }
        if self.regeneration > 0 {
            if self.regeneration % REGENERATION_INTERVAL == 0 && core.is_alive() {
                core.health = (core.health + 1.0).min(core.max_health());
                core.changed(data::HEALTH);
            }
            self.regeneration -= 1;
            if self.regeneration == 0 {
                core.changed(tuff_core::protocol::play::metadata::EFFECT_PARTICLES);
            }
        }
        self.explore_offers(core, cx);
        self.think(core, cx);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let Some(trader) = self.last_traded.take() {
            self.reputation_event(core, Reputation::Trade, trader.uuid, cx);
            core.entity_event(event::HAPPY, cx);
        }
        if !core.no_ai
            && cx
                .state
                .get::<crate::raid::Raids>()
                .at(super::physics::block_pos(core.base.position))
                .is_some_and(|raid| raid.is_active() && !raid.is_over())
            && own_roll(core.base.uuid, cx.tick, 100) == 0
        {
            core.entity_event(event::SPLASH, cx);
        }
        if self.trading.is_some() && self.offers.as_ref().is_none_or(Vec::is_empty) {
            self.stop_trading_now(core, cx);
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        if interaction
            .stack
            .as_ref()
            .is_some_and(|s| s.item == Item::VillagerSpawnEgg)
            || self.trading.is_some()
            || self.brain.sleeping.is_some()
        {
            return None;
        }
        if core.is_baby() {
            self.set_unhappy(core, cx);
            return Some(HandEffect::None);
        }
        let no_offers = self.offers(core, cx.rng).is_empty();
        if !interaction.off_hand {
            if no_offers {
                self.set_unhappy(core, cx);
            }
            cx.effects.push(Effect::TellPlayer {
                player: interaction.player,
                event: Box::new(PlayerEvent::AwardStat(CustomStat::TalkedToVillager, 1)),
            });
        }
        if no_offers {
            return Some(HandEffect::None);
        }
        let trader = Trader {
            id: interaction.player,
            uuid: interaction.player_uuid,
            hero: interaction.hero,
        };
        self.start_trading(trader, core, cx);
        Some(HandEffect::None)
    }

    fn is_merchant(&self) -> bool {
        true
    }

    fn trade(&mut self, core: &mut MobCore, request: &TradeRequest, cx: &mut TickContext) -> bool {
        if !core.is_alive() || self.trading.is_none_or(|t| t.uuid != request.player) {
            return false;
        }
        let Some(offers) = self.offers.as_ref() else {
            return false;
        };
        let mut uses: Vec<i32> = offers.iter().map(|o| o.uses).collect();
        for &(index, times) in &request.trades {
            let Some(offer) = offers.get(index) else {
                return false;
            };
            uses[index] += times as i32;
            if times == 0 || uses[index] > offer.max_uses {
                return false;
            }
        }
        for &(index, times) in &request.trades {
            for _ in 0..times {
                self.notify_trade(core, index, cx);
            }
        }
        true
    }

    fn stop_trading(&mut self, _core: &mut MobCore, player: u128) {
        if self.trading.is_some_and(|t| t.uuid == player) {
            self.trading = None;
        }
    }

    fn reputation(
        &mut self,
        core: &mut MobCore,
        event: Reputation,
        source: u128,
        cx: &mut TickContext,
    ) {
        self.reputation_event(core, event, source, cx);
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        self.wake_up(core, cx);
        if let Some((_, uuid)) = damage.player {
            self.reputation_event(core, Reputation::VillagerHurt, uuid, cx);
            core.entity_event(event::ANGRY, cx);
        }
    }

    fn died(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        if let Some((_, uuid)) = damage.player {
            self.tell_witnesses(core, uuid, cx);
        }
        self.wake_up(core, cx);
        if let Some((_, killer)) = damage.attacker
            && super::zombie_villager::infect(self, core, killer, cx)
        {
            self.stop_trading_now(core, cx);
            return;
        }
        self.stop_trading_now(core, cx);
        self.brain.memories.home = None;
        self.brain.memories.job_site = None;
        self.brain.memories.potential_job_site = None;
        self.brain.memories.meeting_point = None;
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.can_pick_up_loot = true;
        if !self.finalized {
            let pos = super::physics::block_pos(core.base.position);
            let biome = (cx.biome)(pos).unwrap_or("minecraft:plains");
            self.data.kind = VillagerType::for_biome(biome);
            self.finalized = true;
        }
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        let mut child = Self::create(rng);
        child.data.kind = if rng.r#gen::<bool>() {
            self.data.kind
        } else {
            partner.data.kind
        };
        child.finalized = true;
        child
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Two villagers who courted long enough: both eat, and a baby comes if
/// a free bed is in reach, which becomes its home.
fn breed(
    mob: &mut super::Mob<Villager>,
    partner: &mut super::Mob<Villager>,
    cx: &mut TickContext,
) -> Option<Box<dyn tuff_core::world::entity::RegionEntity>> {
    let adult = |m: &super::Mob<Villager>| {
        m.core.is_alive() && !m.core.is_baby() && m.species.can_breed(&m.core)
    };
    if !adult(mob) || !adult(partner) {
        return None;
    }
    mob.species.eat_and_digest();
    partner.species.eat_and_digest();
    let origin = super::physics::block_pos(mob.core.base.position);
    let beds: Vec<tuff_world_model::BlockPos> = cx
        .pois
        .closest_first(origin, 48, crate::world::poi::Occupancy::HasSpace, |kind| {
            kind == tuff_vanilla_data::registry::PointOfInterestType::Home
        })
        .into_iter()
        .map(|(pos, _)| pos)
        .collect();
    let bed = beds
        .into_iter()
        .find(|pos| mob.core.path_to_any(&[*pos], 1, cx).is_some());
    let Some(bed) = bed else {
        mob.core.entity_event(event::ANGRY, cx);
        partner.core.entity_event(event::ANGRY, cx);
        return None;
    };
    let roll = cx.rng.r#gen::<f64>();
    let kind = if roll < 0.5 {
        let pos = super::physics::block_pos(mob.core.base.position);
        VillagerType::for_biome((cx.biome)(pos).unwrap_or("minecraft:plains"))
    } else if roll < 0.75 {
        mob.species.data.kind
    } else {
        partner.species.data.kind
    };
    let base = tuff_core::world::entity::EntityBase::new(
        EntityType::Villager,
        cx.shared.next_entity_id(),
        cx.rng.r#gen(),
        mob.core.base.position,
        [0.0; 3],
    );
    let mut child = super::Mob::<Villager>::new(base, cx.rng);
    child.species.data.kind = kind;
    child.species.finalized = true;
    child.species.brain.memories.home = Some(brain::GlobalPos {
        dimension: cx.shared.dimension(cx.dimension).key.clone(),
        pos: bed,
    });
    child.core.set_age(super::animal::BABY_START_AGE);
    child.core.can_pick_up_loot = true;
    for parent in [&mut mob.core, &mut partner.core] {
        parent.set_age(super::animal::PARENT_AGE_AFTER_BREEDING);
    }
    Some(Box::new(child))
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Villager, super::load::<Villager>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::MintReason;

    fn witness(id: i32, position: [f64; 3]) -> tuff_core::world::entity::EntitySummary {
        use tuff_core::world::entity::{EntitySummary, LivingSummary};
        EntitySummary {
            id,
            uuid: id as u128,
            kind: EntityType::Villager,
            position,
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [
                position[0] - 0.3,
                position[1],
                position[2] - 0.3,
                position[0] + 0.3,
                position[1] + 1.95,
                position[2] + 0.3,
            ],
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
    fn only_villagers_in_line_of_sight_hear_of_a_murder() {
        use tuff_vanilla_data::Block;
        let wall_x = 3;
        let block = |pos: tuff_world_model::BlockPos| {
            Some(
                if pos.y < 64 || (pos.x == wall_x && pos.z == 0 && pos.y < 68) {
                    Block::Stone.default_state()
                } else {
                    Block::Air.default_state()
                },
            )
        };
        let herd = [witness(21, [5.5, 64.0, 0.5]), witness(22, [0.5, 64.0, 4.5])];
        crate::tests::with_world(&block, &herd, |core, cx| {
            core.base.position = [0.5, 64.0, 0.5];
            Villager::default().tell_witnesses(core, 99, cx);
            let mut mail = cx.state.get::<ReputationMail>();
            assert!(mail.take(21).is_empty(), "behind the wall");
            assert_eq!(mail.take(22).len(), 1, "in plain view");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn villager_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Villager.data_layout();
        assert_eq!(layout.len(), DATA_FINALIZED as usize + 1);
        assert_eq!(
            layout[UNHAPPY as usize] as i32,
            MetadataValue::VarInt(0).type_id()
        );
        assert_eq!(
            layout[DATA_FINALIZED as usize] as i32,
            MetadataValue::Boolean(false).type_id()
        );
        let value = VillagerData::default().metadata(EntityType::Villager, VILLAGER_DATA);
        assert_eq!(value.type_id(), layout[VILLAGER_DATA as usize] as i32);
        assert_eq!(
            layout[data::BABY as usize] as i32,
            MetadataValue::Boolean(false).type_id()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn villager_data_saves_with_the_game_keys() {
        let data = VillagerData {
            kind: VillagerType::Desert,
            profession: VillagerProfession::Librarian,
            level: 3,
        };
        let nbt = data.to_nbt();
        assert_eq!(nbt.get_str("type"), Some("minecraft:desert"));
        assert_eq!(nbt.get_str("profession"), Some("minecraft:librarian"));
        assert_eq!(VillagerData::from_nbt(&nbt), data);
        assert_eq!(
            VillagerData::from_nbt(&Compound::new()),
            VillagerData::default()
        );
        assert_eq!(data.next_level_xp(), 150);
        assert!(!VillagerData { level: 5, ..data }.can_level_up());
    }

    #[tuff_core::watchdog]
    #[test]
    fn food_fills_the_inventory_and_breeding_eats_it() {
        let mut villager = Villager::create(&mut <StdRng as rand::SeedableRng>::seed_from_u64(1));
        assert!(villager.wants(&ItemStack::mint(Item::Bread, 1, MintReason::Test)));
        assert!(!villager.wants(&ItemStack::mint(Item::Stone, 1, MintReason::Test)));
        assert_eq!(
            villager.add_item(ItemStack::mint(Item::Bread, 3, MintReason::Test)),
            None
        );
        assert_eq!(villager.food_in_inventory(), 12);
        villager.eat_and_digest();
        assert_eq!(villager.food_level, 0);
        assert_eq!(villager.food_in_inventory(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn trades_follow_the_villager_not_the_region_random_source() {
        use rand::SeedableRng;
        crate::tests::with_context(|core, _| {
            let sold = |region_seed: u64| {
                let mut villager = Villager::create(&mut StdRng::seed_from_u64(1));
                villager.data.profession = VillagerProfession::Farmer;
                let mut rng = StdRng::seed_from_u64(region_seed);
                format!("{:?}", villager.offers(core, &mut rng))
            };
            assert_eq!(sold(1), sold(2));
            assert!((0..8).all(|n| sold(n) == sold(1)));
        });
    }
}
