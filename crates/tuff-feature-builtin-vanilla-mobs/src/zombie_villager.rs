//! Zombie villagers: zombies that remember the villager they were (type,
//! profession, level, trades, experience and gossip). A zombie that kills
//! a villager on normal (half the time) or hard difficulty turns it into
//! one; a golden apple fed while it is weak starts the cure, and after a
//! few minutes it is a villager again whose curer gets a discount.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::{
    EntityType, MobEffect, SoundEvent, VillagerProfession, VillagerType,
};

use super::villager::gossip::Gossips;
use super::villager::{Reputation, Villager, VillagerData};
use super::zombie::Zombie;
use super::{CONVERSION_NAUSEA, Damage, Mob, MobCore, MobSounds, Species, data, monster};
use crate::world::trading::{self, MerchantOffer};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{
    Effect, EntityBase, EntityInteraction, InteractOutcome, TickContext, take, take_number,
};
use tuff_core::world::messages::HandEffect;
use tuff_core::world::player::Difficulty;
use tuff_nbt::{Compound, Tag};

/// Data after the zombie's: converting, then the villager data and
/// whether it is final.
const CONVERTING: u8 = data::SPECIES + 1;
const VILLAGER_DATA: u8 = data::SPECIES + 2;
const DATA_FINALIZED: u8 = data::SPECIES + 3;

const CONVERSION_MIN: i32 = 3600;
const CONVERSION_SPREAD: i32 = 2401;
const CURE_EVENT: i8 = 16;
const INFECTED_EVENT: i32 = 1026;
const CONVERTED_EVENT: i32 = 1027;
const SPECIAL_BLOCK_RADIUS: i32 = 4;
const MAX_SPECIAL_BLOCKS: i32 = 14;

#[derive(Debug, Default)]
pub struct ZombieVillager {
    zombie: Zombie,
    pub data: VillagerData,
    pub finalized: bool,
    pub offers: Option<Vec<MerchantOffer>>,
    pub gossips: Option<Gossips>,
    pub xp: i32,
    /// Ticks until cured, and who started the cure.
    pub conversion: Option<(i32, Option<u128>)>,
}

impl ZombieVillager {
    fn start_converting(
        &mut self,
        core: &mut MobCore,
        player: Option<u128>,
        time: i32,
        cx: &mut TickContext,
    ) {
        self.conversion = Some((time, player));
        core.changed(CONVERTING);
        if let Some(change) = core.effects.remove(MobEffect::Weakness) {
            core.pending_effects.push(change);
        }
        core.add_effect(ActiveEffect::timed(MobEffect::Strength, time.into(), 0));
        core.entity_event(CURE_EVENT, cx);
    }

    /// How far the cure moves this tick: iron bars and beds nearby
    /// sometimes speed it up.
    fn progress(core: &MobCore, cx: &mut TickContext) -> i32 {
        let mut amount = 1;
        if cx.rng.r#gen::<f32>() >= 0.01 {
            return amount;
        }
        let [x, y, z] = core.base.position.map(|v| v as i32);
        let mut found = 0;
        'scan: for xx in x - SPECIAL_BLOCK_RADIUS..x + SPECIAL_BLOCK_RADIUS {
            for yy in y - SPECIAL_BLOCK_RADIUS..y + SPECIAL_BLOCK_RADIUS {
                for zz in z - SPECIAL_BLOCK_RADIUS..z + SPECIAL_BLOCK_RADIUS {
                    if found >= MAX_SPECIAL_BLOCKS {
                        break 'scan;
                    }
                    let pos = tuff_world_model::BlockPos::new(xx, yy, zz);
                    let Some(state) = (cx.block)(pos) else {
                        continue;
                    };
                    if tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::block::key::SPEEDS_UP_ZOMBIE_VILLAGER_CURING,
                        state.block(),
                    ) {
                        if cx.rng.r#gen::<f32>() < 0.3 {
                            amount += 1;
                        }
                        found += 1;
                    }
                }
            }
        }
        amount
    }

    fn level_event(core: &MobCore, event: i32, cx: &mut TickContext) {
        if core.base.silent {
            return;
        }
        let packet = tuff_core::world::framed(&play::LevelEvent {
            event,
            position: super::physics::block_pos(core.base.position),
            data: 0,
            global: false,
        });
        cx.effects
            .push(Effect::ChunkPackets(tuff_core::world::packets([packet])));
    }

    /// The cure is done: a villager takes its place.
    fn finish(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let mut villager = super::convert::<Villager>(core, EntityType::Villager, false, cx);
        let species = &mut villager.species;
        species.finalized = self.finalized;
        species.data = self.data;
        if let Some(gossips) = self.gossips.take() {
            species.gossips = gossips;
        }
        species.offers = self.offers.take();
        species.xp = self.xp;
        let mut spawned: Mob<Villager> = villager;
        spawned.core.can_pick_up_loot = true;
        spawned
            .core
            .add_effect(ActiveEffect::timed(MobEffect::Nausea, CONVERSION_NAUSEA, 0));
        if let Some((_, Some(player))) = self.conversion {
            spawned.species.reputation_event(
                &spawned.core,
                Reputation::ZombieVillagerCured,
                player,
                cx,
            );
            cx.effects.push(Effect::Advancement {
                player,
                event: Box::new(
                    tuff_core::world::messages::AdvancementEvent::CuredZombieVillager {
                        zombie: core.facts(),
                        villager: spawned.core.facts(),
                    },
                ),
            });
        }
        Self::level_event(core, CONVERTED_EVENT, cx);
        cx.effects.push(Effect::Spawn(Box::new(spawned)));
        core.discarded = true;
    }
}

/// A random profession, as a zombie villager spawns with.
fn random_data(rng: &mut StdRng) -> VillagerData {
    let professions = VillagerProfession::ALL;
    VillagerData {
        profession: professions[rng.gen_range(0..professions.len())],
        ..VillagerData::default()
    }
}

impl Species for ZombieVillager {
    fn create(rng: &mut StdRng) -> Self {
        Self {
            zombie: Zombie::create(rng),
            data: random_data(rng),
            ..Self::default()
        }
    }

    fn configure(core: &mut MobCore) {
        Zombie::configure(core);
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.zombie.load(core, nbt);
        let data = take(nbt, "VillagerData", |t| {
            t.as_compound().map(VillagerData::from_nbt)
        });
        let finalized = take(nbt, "VillagerDataFinalized", |t| {
            tuff_core::world::entity::number(t).map(|v| v != 0.0)
        })
        .unwrap_or(false);
        if finalized || data.is_some() {
            self.finalized = true;
            if let Some(data) = data {
                self.data = data;
            }
        }
        self.offers = take(nbt, "Offers", |t| {
            t.as_compound().map(trading::offers_from_nbt)
        });
        self.gossips = take(nbt, "Gossips", Gossips::from_nbt);
        self.xp = take_number(nbt, "Xp").map_or(0, |v| v as i32);
        let time = take_number(nbt, "ConversionTime").map_or(-1, |v| v as i32);
        let player = take(nbt, "ConversionPlayer", Tag::as_uuid);
        if time != -1 {
            self.conversion = Some((time, player));
        }
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        self.zombie.save(core, out);
        out.insert("VillagerData", self.data.to_nbt());
        out.insert("VillagerDataFinalized", self.finalized);
        if let Some(offers) = &self.offers {
            out.insert("Offers", trading::offers_to_nbt(offers));
        }
        if let Some(gossips) = &self.gossips {
            out.insert("Gossips", gossips.to_nbt());
        }
        let (time, player) = self.conversion.unwrap_or((-1, None));
        out.insert("ConversionTime", time);
        if let Some(player) = player {
            out.insert("ConversionPlayer", Tag::uuid(player));
        }
        out.insert("Xp", self.xp);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut values = self.zombie.metadata(core);
        values.push((
            CONVERTING,
            MetadataValue::Boolean(self.conversion.is_some()),
        ));
        values.push((
            VILLAGER_DATA,
            self.data.metadata(core.kind(), VILLAGER_DATA),
        ));
        values.push((DATA_FINALIZED, MetadataValue::Boolean(self.finalized)));
        values
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        monster::sounds(
            SoundEvent::EntityZombieVillagerAmbient,
            SoundEvent::EntityZombieVillagerHurt,
            SoundEvent::EntityZombieVillagerDeath,
            Some(SoundEvent::EntityZombieVillagerStep),
        )
    }

    fn experience(&self, core: &MobCore, rng: &mut StdRng) -> i32 {
        self.zombie.experience(core, rng)
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.is_alive() {
            return;
        }
        let Some((time, player)) = self.conversion else {
            return;
        };
        let left = time - Self::progress(core, cx);
        self.conversion = Some((left, player));
        if left <= 0 {
            self.finish(core, cx);
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if stack.item != Item::GoldenApple {
            return None;
        }
        if !core.effects.has(MobEffect::Weakness) {
            return Some(HandEffect::None);
        }
        let time = cx.rng.gen_range(0..CONVERSION_SPREAD) + CONVERSION_MIN;
        self.start_converting(core, Some(interaction.player_uuid), time, cx);
        Some(HandEffect::Consume(1))
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !self.finalized {
            let pos = super::physics::block_pos(core.base.position);
            let biome = (cx.biome)(pos).unwrap_or("minecraft:plains");
            self.data.kind = VillagerType::for_biome(biome);
            self.finalized = true;
        }
        self.zombie.finalize_spawn(core, cx);
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        let Some((position, target)) = Zombie::reinforcement_spot(core, damage, cx) else {
            return;
        };
        let base = EntityBase::new(
            core.kind(),
            cx.shared.next_entity_id(),
            cx.rng.r#gen(),
            position,
            [0.0; 3],
        );
        let mut child = Mob::<ZombieVillager>::new(base, cx.rng);
        child.core.target = Some(target);
        child.species.finalize_spawn(&mut child.core, cx);
        Zombie::mark_callee(&mut child.core);
        cx.effects.push(Effect::Spawn(Box::new(child)));
    }

    fn remove_when_far_away(&self) -> bool {
        self.conversion.is_none() && self.xp == 0
    }
}

/// A zombie killed `villager`: on normal difficulty half the time, on
/// hard always, the villager becomes a zombie villager instead of dying.
/// True when it did (the villager is then gone).
pub fn infect(
    villager: &mut Villager,
    core: &mut MobCore,
    killer: EntityType,
    cx: &mut TickContext,
) -> bool {
    if !super::zombie::INFECTING.contains(&killer) {
        return false;
    }
    match *cx.shared.difficulty.borrow() {
        Difficulty::Hard => {}
        Difficulty::Normal if cx.rng.r#gen::<bool>() => return false,
        Difficulty::Normal => {}
        _ => return false,
    }
    let mut zombie = super::convert::<ZombieVillager>(core, EntityType::ZombieVillager, true, cx);
    if core.is_baby() {
        zombie.core.set_zombie_baby(true);
    }
    let species = &mut zombie.species;
    species.zombie.setup_spawn(&mut zombie.core, cx, false);
    zombie.core.health = zombie.core.max_health();
    species.finalized = villager.finalized;
    species.data = villager.data;
    species.gossips = Some(villager.gossips.clone());
    species.offers = Some(villager.offers(core, cx.rng).clone());
    species.xp = villager.xp;
    ZombieVillager::level_event(core, INFECTED_EVENT, cx);
    cx.effects.push(Effect::Spawn(Box::new(zombie)));
    core.discarded = true;
    true
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::ZombieVillager, super::load::<ZombieVillager>);
    registry.register_spawn_rule(EntityType::ZombieVillager, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn zombie_villager_data_indices_match_the_dumped_layout() {
        let layout = EntityType::ZombieVillager.data_layout();
        assert_eq!(layout.len(), DATA_FINALIZED as usize + 1);
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        assert_eq!(layout[CONVERTING as usize], boolean);
        assert_eq!(layout[DATA_FINALIZED as usize], boolean);
        let value = VillagerData::default().metadata(EntityType::ZombieVillager, VILLAGER_DATA);
        assert_eq!(value.type_id(), layout[VILLAGER_DATA as usize] as i32);
    }

    /// `infect()` builds its zombie villager through the generic
    /// `convert()` helper, which is expected to run `Species::configure`
    /// (goal registration, `make_monster`) the same way a fresh spawn
    /// does; this pins that down directly rather than through a
    /// probabilistic AI test.
    #[tuff_core::watchdog]
    #[test]
    fn convert_sets_up_goals_and_hostile() {
        super::super::tests::with_context(|core, cx| {
            let converted: Mob<ZombieVillager> =
                super::super::convert(core, EntityType::ZombieVillager, true, cx);
            assert!(
                converted.core.hostile,
                "converted zombie villager should be hostile"
            );
            assert!(
                !converted.core.goals.is_empty(),
                "converted zombie villager should have goals"
            );
        });
    }
}
