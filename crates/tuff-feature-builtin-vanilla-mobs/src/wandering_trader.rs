//! Wandering traders: merchants that turn up near a player now and then
//! (see `world::region::traders`) with two trader llamas, sell one set of
//! offers that never restocks, drink an invisibility potion at dusk and
//! milk at dawn, run from illagers and zombies, and leave after a while.
//!
//! Its llamas are on leads it holds; they defend it and leave with it
//! through what it publishes in [`Caravans`], keyed by its UUID.

use std::collections::HashMap;

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{CustomStat, EntityType, MobEffect, Potion, SoundEvent};

use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{AvoidPlayer, Float, LookAtPlayer, Panic, RandomStroll};
use super::{Damage, MobCore, MobSounds, Species, data};
use crate::world::trading::{self, MerchantOffer, MerchantView, TradeRequest};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{
    EntityInteraction, InteractOutcome, TickContext, take, take_number,
};
use tuff_core::world::messages::{HandEffect, PlayerEvent};
use tuff_item::item::{ItemStack, MintReason, PotionContents};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const UNHAPPY: u8 = data::SPECIES;
const SPEED: f64 = 0.35;
const HOME_RADIUS: i32 = 16;
/// Ticks a trader spawned by the world stays before it leaves.
pub const DESPAWN_DELAY: i32 = 48_000;
const TRADE_RANGE_SQ: f64 = 16.0 * 16.0;
const INVISIBILITY_TICKS: i64 = 3600;
const DRINK_TICKS: u32 = 32;

const TRADE_SETS: [&str; 3] = [
    "minecraft:wandering_trader/buying",
    "minecraft:wandering_trader/uncommon",
    "minecraft:wandering_trader/common",
];

/// What the region's wandering traders publish each tick for the llamas
/// their leads hold, by the trader's UUID.
#[derive(Default)]
pub struct Caravans {
    traders: HashMap<u128, TraderFacts>,
}

/// A wandering trader as its llamas see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraderFacts {
    pub despawn_delay: i32,
    /// Who last hurt it, and when.
    pub hurt_by: Option<(i32, u64)>,
    seen: u64,
}

impl Caravans {
    /// The trader with this UUID, if it ticked here lately.
    pub fn trader(&self, uuid: u128, now: u64) -> Option<TraderFacts> {
        self.traders
            .get(&uuid)
            .filter(|t| now.saturating_sub(t.seen) <= 1)
            .copied()
    }

    fn publish(&mut self, uuid: u128, despawn_delay: i32, now: u64) {
        let facts = self.traders.entry(uuid).or_insert(TraderFacts {
            despawn_delay,
            hurt_by: None,
            seen: now,
        });
        facts.despawn_delay = despawn_delay;
        facts.seen = now;
        self.traders.retain(|_, t| now.saturating_sub(t.seen) <= 20);
    }
}

/// The player at its trading screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Trader {
    id: i32,
    uuid: u128,
}

#[derive(Debug, Default)]
pub struct WanderingTrader {
    offers: Option<Vec<MerchantOffer>>,
    trading: Option<Trader>,
    despawn_delay: i32,
    wander_target: Option<BlockPos>,
}

impl WanderingTrader {
    /// Sends it towards `pos`, which becomes its home, and starts the
    /// clock on its stay.
    pub fn arrive(&mut self, core: &mut MobCore, pos: BlockPos, despawn_delay: i32) {
        self.despawn_delay = despawn_delay;
        self.wander_target = Some(pos);
        core.bound_origin = Some(pos);
        core.home = Some(super::Home {
            pos,
            radius: HOME_RADIUS,
        });
    }

    pub fn despawn_delay(&self) -> i32 {
        self.despawn_delay
    }

    fn offers(&mut self, rng: &mut StdRng) -> &mut Vec<MerchantOffer> {
        self.offers.get_or_insert_with(|| {
            let mut offers = Vec::new();
            for name in TRADE_SETS {
                trading::add_offers_from_set(name, None, &mut offers, rng);
            }
            offers
        })
    }

    fn view(&self, core: &MobCore) -> MerchantView {
        MerchantView {
            merchant: core.base.id,
            offers: self.offers.clone().unwrap_or_default(),
            level: 1,
            xp: 0,
            regular: false,
            can_restock: false,
        }
    }

    fn title(core: &MobCore) -> TextComponent {
        core.base
            .custom_name
            .as_ref()
            .and_then(TextComponent::from_nbt)
            .unwrap_or_else(|| TextComponent::translate("entity.minecraft.wandering_trader", []))
    }

    fn set_trading(&mut self, core: &mut MobCore, trader: Option<Trader>) {
        self.trading = trader;
        core.signal = trader.map_or(0, |t| t.id);
    }

    fn stop_trading_now(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let Some(trader) = self.trading {
            cx.effects
                .push(tuff_core::world::entity::Effect::TellPlayer {
                    player: trader.id,
                    event: crate::merchant_events::closed(core.base.id),
                });
        }
        self.set_trading(core, None);
    }

    fn notify_trade(&mut self, core: &mut MobCore, offer: usize, cx: &mut TickContext) {
        let Some(offer) = self.offers.as_mut().and_then(|o| o.get_mut(offer)) else {
            return;
        };
        offer.uses += 1;
        core.ambient_sound_time = -super::DEFAULT_AMBIENT_INTERVAL;
        if offer.reward_exp {
            let [x, y, z] = core.base.position;
            cx.effects
                .push(tuff_core::world::entity::Effect::Experience {
                    position: [x, y + 0.5, z],
                    amount: 3 + cx.rng.gen_range(0..4),
                });
        }
    }
}

/// Drinks `item` while `when` holds: the item goes in its hand for the
/// drink, then its effect applies and `sound` plays.
#[derive(Debug)]
struct UseItem {
    item: fn() -> ItemStack,
    sound: SoundEvent,
    when: fn(&MobCore, &TickContext) -> bool,
}

impl Goal for UseItem {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        (self.when)(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.using_item.is_some()
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.equipment
            .set(EquipmentSlot::Mainhand, Some((self.item)()));
        mob.start_using_item();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if mob.using_item.is_some_and(|t| t >= DRINK_TICKS) {
            finish_drinking(mob, cx);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.stop_using_item();
        mob.equipment.set(EquipmentSlot::Mainhand, None);
        mob.sound(self.sound, 1.0, 1.0, cx);
    }
}

fn finish_drinking(mob: &mut MobCore, cx: &mut TickContext) {
    let held = mob.equipment.get(EquipmentSlot::Mainhand).map(|s| s.item);
    let sound = if held == Some(Item::MilkBucket) {
        let changes = mob.effects.clear();
        mob.pending_effects.extend(changes);
        SoundEvent::EntityWanderingTraderDrinkMilk
    } else {
        if let Some(change) = mob.effects.add(ActiveEffect::timed(
            MobEffect::Invisibility,
            INVISIBILITY_TICKS,
            0,
        )) {
            mob.pending_effects.push(change);
        }
        SoundEvent::EntityWanderingTraderDrinkPotion
    };
    mob.sound(sound, 1.0, 1.0, cx);
    mob.stop_using_item();
}

fn invisibility_potion() -> ItemStack {
    let mut stack = ItemStack::mint(Item::Potion, 1, MintReason::Spawn);
    stack.components.potion_contents = Some(Box::new(PotionContents {
        potion: Some(Potion::Invisibility),
        ..Default::default()
    }));
    stack
}

fn milk() -> ItemStack {
    ItemStack::mint(Item::MilkBucket, 1, MintReason::Spawn)
}

fn invisible(mob: &MobCore) -> bool {
    mob.effects.has(MobEffect::Invisibility)
}

fn dusk(mob: &MobCore, cx: &TickContext) -> bool {
    !super::goals::bright_outside(cx) && !invisible(mob)
}

fn dawn(mob: &MobCore, cx: &TickContext) -> bool {
    super::goals::bright_outside(cx) && invisible(mob)
}

/// The trading player, by entity id (kept in `MobCore::signal`), within
/// reach.
fn trading_player(mob: &MobCore, cx: &TickContext) -> Option<[f64; 3]> {
    if mob.signal == 0 {
        return None;
    }
    cx.players
        .iter()
        .find(|p| p.id == mob.signal)
        .map(|p| p.eye())
        .filter(|eye| mob.distance_sq(*eye) <= TRADE_RANGE_SQ)
}

/// Stands still while a player trades with it.
#[derive(Debug, Default)]
struct TradeWithPlayer;

impl Goal for TradeWithPlayer {
    fn flags(&self) -> Flags {
        Flags::JUMP | Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.is_alive()
            && !mob.body.in_water
            && mob.base.on_ground
            && trading_player(mob, cx).is_some()
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

/// Looks at the player trading with it.
#[derive(Debug, Default)]
struct LookAtTradingPlayer;

impl Goal for LookAtTradingPlayer {
    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        trading_player(mob, cx).is_some()
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(eye) = trading_player(mob, cx) {
            mob.look.set_look_at(eye, 10.0, mob.max_head_x_rot);
        }
    }
}

/// Walks towards the place it was sent to (kept in `MobCore::homeward`
/// as long as it has one), ten blocks at a time.
#[derive(Debug)]
struct WanderToPosition {
    stop_distance: f64,
}

impl WanderToPosition {
    fn too_far(mob: &MobCore, pos: BlockPos, distance: f64) -> bool {
        let center = [
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        ];
        mob.distance_sq(center) >= distance * distance
    }
}

impl Goal for WanderToPosition {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.homeward
            && mob
                .bound_origin
                .is_some_and(|pos| Self::too_far(mob, pos, self.stop_distance))
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.homeward = false;
        mob.navigation.stop();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(pos) = mob.bound_origin.filter(|_| mob.homeward) else {
            return;
        };
        if !mob.navigation.is_done() {
            return;
        }
        let target = [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)];
        if !Self::too_far(mob, pos, 10.0) {
            mob.move_to_position(target, SPEED, cx);
            return;
        }
        let [x, y, z] = mob.base.position;
        let delta = [target[0] - x, target[1] - y, target[2] - z];
        let length = delta.iter().map(|d| d * d).sum::<f64>().sqrt();
        if length < 1.0e-4 {
            return;
        }
        let step = delta.map(|d| d / length * 10.0);
        mob.move_to_position([x + step[0], y + step[1], z + step[2]], SPEED, cx);
    }
}

const ZOMBIES: &[EntityType] = &[
    EntityType::Zombie,
    EntityType::Husk,
    EntityType::Drowned,
    EntityType::ZombieVillager,
    EntityType::ZombifiedPiglin,
];

impl Species for WanderingTrader {
    const RANDOM_BABIES: bool = false;

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.age = Some(super::animal::Age::default());
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(
            0,
            UseItem {
                item: invisibility_potion,
                sound: SoundEvent::EntityWanderingTraderDisappeared,
                when: dusk,
            },
        );
        goals.add(
            0,
            UseItem {
                item: milk,
                sound: SoundEvent::EntityWanderingTraderReappeared,
                when: dawn,
            },
        );
        goals.add(1, TradeWithPlayer);
        goals.add(1, AvoidPlayer::entities(ZOMBIES, 8.0, 0.5, 0.5));
        goals.add(
            1,
            AvoidPlayer::entities(&[EntityType::Evoker], 12.0, 0.5, 0.5),
        );
        goals.add(
            1,
            AvoidPlayer::entities(&[EntityType::Vindicator], 8.0, 0.5, 0.5),
        );
        goals.add(1, AvoidPlayer::entities(&[EntityType::Vex], 8.0, 0.5, 0.5));
        goals.add(
            1,
            AvoidPlayer::entities(&[EntityType::Pillager], 15.0, 0.5, 0.5),
        );
        goals.add(
            1,
            AvoidPlayer::entities(&[EntityType::Illusioner], 12.0, 0.5, 0.5),
        );
        goals.add(
            1,
            AvoidPlayer::entities(&[EntityType::Zoglin], 10.0, 0.5, 0.5),
        );
        goals.add(1, Panic::new(0.5));
        goals.add(1, LookAtTradingPlayer);
        goals.add(2, WanderToPosition { stop_distance: 2.0 });
        goals.add(4, super::goals::MoveTowardsHome::new(SPEED));
        goals.add(8, RandomStroll::water_avoiding(SPEED));
        goals.add(9, LookAtPlayer::interact(3.0));
        goals.add(10, super::goals::LookAtMob::new(8.0));
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.offers = take(nbt, "Offers", |t| {
            t.as_compound().map(trading::offers_from_nbt)
        });
        self.despawn_delay = take_number(nbt, "DespawnDelay").map_or(0, |v| v as i32);
        let pos = |tag: &Tag| match tag {
            Tag::IntArray(v) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
            _ => None,
        };
        self.wander_target = take(nbt, "wander_target", pos);
        core.homeward = self.wander_target.is_some();
        if let Some(home) = core.home {
            core.bound_origin = Some(home.pos);
        } else if let Some(target) = self.wander_target {
            core.bound_origin = Some(target);
        }
        if let Some(age) = &mut core.age {
            age.age = age.age.max(0);
        }
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        if let Some(offers) = &self.offers {
            out.insert("Offers", trading::offers_to_nbt(offers));
        }
        out.insert("DespawnDelay", self.despawn_delay);
        if core.homeward
            && let Some(pos) = core.bound_origin
        {
            out.insert("wander_target", vec![pos.x, pos.y, pos.z]);
        }
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(UNHAPPY, MetadataValue::VarInt(0))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(if self.trading.is_some() {
                SoundEvent::EntityWanderingTraderTrade
            } else {
                SoundEvent::EntityWanderingTraderAmbient
            }),
            hurt: Some(SoundEvent::EntityWanderingTraderHurt),
            death: Some(SoundEvent::EntityWanderingTraderDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let now = cx.tick;
        cx.state
            .get::<Caravans>()
            .publish(core.base.uuid, self.despawn_delay, now);
        if self.wander_target.is_some() && !core.homeward {
            self.wander_target = None;
        }
        if self.trading.is_some() && trading_player(core, cx).is_none() {
            self.stop_trading_now(core, cx);
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        if self.despawn_delay > 0 && self.trading.is_none() {
            self.despawn_delay -= 1;
            if self.despawn_delay == 0 {
                core.discarded = true;
            }
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
            || !core.is_alive()
            || self.trading.is_some()
            || core.is_baby()
        {
            return None;
        }
        if !interaction.off_hand {
            cx.effects
                .push(tuff_core::world::entity::Effect::TellPlayer {
                    player: interaction.player,
                    event: Box::new(PlayerEvent::AwardStat(CustomStat::TalkedToVillager, 1)),
                });
        }
        if self.offers(cx.rng).is_empty() {
            return Some(HandEffect::None);
        }
        self.set_trading(
            core,
            Some(Trader {
                id: interaction.player,
                uuid: interaction.player_uuid,
            }),
        );
        cx.effects
            .push(tuff_core::world::entity::Effect::TellPlayer {
                player: interaction.player,
                event: crate::merchant_events::open(Self::title(core), self.view(core)),
            });
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

    fn stop_trading(&mut self, core: &mut MobCore, player: u128) {
        if self.trading.is_some_and(|t| t.uuid == player) {
            self.set_trading(core, None);
        }
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        if let Some((attacker, _)) = damage.attacker {
            let now = cx.tick;
            let mut caravans = cx.state.get::<Caravans>();
            caravans.publish(core.base.uuid, self.despawn_delay, now);
            if let Some(facts) = caravans.traders.get_mut(&core.base.uuid) {
                facts.hurt_by = Some((attacker, now));
            }
        }
    }

    fn died(&mut self, core: &mut MobCore, _damage: &Damage, cx: &mut TickContext) {
        self.stop_trading_now(core, cx);
        cx.state.get::<Caravans>().traders.remove(&core.base.uuid);
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::WanderingTrader, super::load::<WanderingTrader>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mob;
    use tuff_core::world::entity::{EntityBase, RegionEntity};

    #[tuff_core::watchdog]
    #[test]
    fn wandering_trader_data_indices_match_the_dumped_layout() {
        let layout = EntityType::WanderingTrader.data_layout();
        assert_eq!(layout.len(), UNHAPPY as usize + 1);
        assert_eq!(
            layout[UNHAPPY as usize] as i32,
            MetadataValue::VarInt(0).type_id()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn its_offers_come_from_the_three_trade_sets() {
        crate::tests::with_context(|_, cx| {
            let mut trader = WanderingTrader::default();
            let wanted: i64 = TRADE_SETS
                .iter()
                .map(|n| {
                    tuff_vanilla_data::villager::TradeSet::get(n)
                        .expect("set exists")
                        .amount
                })
                .sum();
            let offers = trader.offers(cx.rng);
            assert!(!offers.is_empty());
            assert!(offers.len() as i64 <= wanted, "{} > {wanted}", offers.len());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_leaves_when_its_stay_is_over_unless_trading() {
        crate::tests::with_context(|_, cx| {
            let base = EntityBase::new(
                EntityType::WanderingTrader,
                2,
                2,
                [0.5, 64.0, 0.5],
                [0.0; 3],
            );
            let mut mob = Mob::<WanderingTrader>::new(base, cx.rng);
            mob.core.no_ai = true;
            mob.species
                .arrive(&mut mob.core, BlockPos::new(0, 64, 0), 3);
            mob.species.trading = Some(Trader { id: 9, uuid: 9 });
            mob.species.ai_step(&mut mob.core, cx);
            assert_eq!(
                mob.species.despawn_delay(),
                3,
                "the clock stops while trading"
            );
            mob.species.trading = None;
            let mut gone = false;
            for _ in 0..3 {
                gone |= matches!(mob.tick(cx), tuff_core::world::entity::Tick::Discard);
            }
            assert!(gone);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_drinks_invisibility_at_night_and_milk_by_day() {
        crate::tests::with_context(|core, cx| {
            assert!(dusk(core, cx) != super::super::goals::bright_outside(cx));
            core.equipment
                .set(EquipmentSlot::Mainhand, Some(invisibility_potion()));
            finish_drinking(core, cx);
            assert!(invisible(core));
            core.equipment.set(EquipmentSlot::Mainhand, Some(milk()));
            finish_drinking(core, cx);
            assert!(!invisible(core), "milk clears it");
        });
    }
}
