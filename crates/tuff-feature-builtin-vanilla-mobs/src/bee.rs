//! Bees: fly, pollinate flowers, breed on flowers, and sting whoever hurts
//! them (then die soon after), go into hives (see `hive`), and grow the
//! crops under them from carried nectar.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};
use tuff_vanilla_data::{damage_type, tag};

use super::attack::MeleeAttack;
use super::control::MoveStyle;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{Breed, LookAtPlayer, RandomLookAround, RandomStroll, Tempt};
use super::navigation::NavKind;
use super::physics::block_pos;
use super::target::HurtByTarget;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{Effect, TickContext, take, take_bool, take_number};
use tuff_core::world::player::Difficulty;
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const FLAG_ROLLING: i8 = 0x02;
const FLAG_STUNG: i8 = 0x04;
const FLAG_NECTAR: i8 = 0x08;
const ANGER_TIME: u8 = data::SPECIES + 1;

/// Ticks after stinging before the bee dies (matches the game's constant).
const STING_DEATH_TICKS: i32 = 1200;
/// Ticks a bee can carry nectar without a hive to bring it to.
const NECTAR_TIMEOUT: i32 = 3600;
const DROWN_TICKS: i32 = 20;
const KNOWN_FLOWER_DELAY: i32 = 600;
const TRAVEL_TICKS: i32 = 2400;
const MAX_BLACKLISTED_HIVES: usize = 3;
const HIVE_STUCK_TICKS: i32 = 60;
const TOO_FAR: f64 = 48.0;
const POLLINATE_RADIUS: i32 = 5;
const POLLINATE_MIN_TICKS: i32 = 400;
const POLLINATE_MAX_TICKS: i32 = 600;
const POLLINATE_STOP_COOLDOWN: i32 = 200;
const POLLINATE_RETRY_MIN: i32 = 20;
const POLLINATE_RETRY_MAX: i32 = 60;

fn bee_grudge(mob: &MobCore, player: &tuff_core::world::entity::NearbyPlayer) -> bool {
    mob.angry_at_player(player.uuid)
}

fn bee_food(item: tuff_vanilla_data::Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::BEE_FOOD, item)
}

fn poison_hook(
    _mob: &MobCore,
    _target: &super::target::TargetView,
    hit: &mut tuff_core::world::entity::EntityHit,
    cx: &mut TickContext,
) {
    let seconds = match *cx.shared.difficulty.borrow() {
        Difficulty::Normal => 10,
        Difficulty::Hard => 18,
        _ => 0,
    };
    if seconds > 0 {
        hit.effects
            .push(ActiveEffect::timed(MobEffect::Poison, seconds * 20, 0));
    }
}

fn sting_hook(mob: &mut MobCore) {
    mob.signal = 1;
    mob.stop_being_angry();
}

fn nearest_flower(mob: &MobCore, cx: &TickContext) -> Option<BlockPos> {
    let origin = block_pos(mob.base.position);
    let mut best: Option<(f64, BlockPos)> = None;
    for dx in -POLLINATE_RADIUS..=POLLINATE_RADIUS {
        for dz in -POLLINATE_RADIUS..=POLLINATE_RADIUS {
            for dy in -POLLINATE_RADIUS..=POLLINATE_RADIUS {
                if dx.abs() + dy.abs() + dz.abs() > POLLINATE_RADIUS {
                    continue;
                }
                let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                let Some(state) = (cx.block)(pos) else {
                    continue;
                };
                if !tuff_datapack::tags::is(tag::block::key::FLOWERS, state.block()) {
                    continue;
                }
                let center = [pos.x as f64 + 0.5, pos.y as f64 + 0.5, pos.z as f64 + 0.5];
                let distance = mob.distance_sq(center);
                if best.is_none_or(|(d, _)| distance < d) {
                    best = Some((distance, pos));
                }
            }
        }
    }
    best.map(|(_, pos)| pos)
}

/// Flies to a flower within five blocks and hovers over it; more than 400
/// ticks of hovering (at most 600 in total) earn the nectar when the goal
/// stops.
#[derive(Debug, Default)]
struct Pollinate {
    target: Option<BlockPos>,
    successful: i32,
    ticks: i32,
    pollinating: bool,
    retry: i32,
}

impl Goal for Pollinate {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.nectar || mob.is_baby() || mob.anger.is_some() || cx.raining {
            return false;
        }
        self.retry -= 1;
        if self.retry > 0 {
            return false;
        }
        self.retry = cx.rng.gen_range(POLLINATE_RETRY_MIN..=POLLINATE_RETRY_MAX);
        self.target = nearest_flower(mob, cx);
        self.target.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !self.pollinating
            || self.target.is_none()
            || mob.nectar
            || mob.is_baby()
            || mob.anger.is_some()
            || cx.raining
        {
            return false;
        }
        self.successful <= POLLINATE_MIN_TICKS || cx.rng.gen_range(0.0..1.0f32) < 0.2
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.successful = 0;
        self.ticks = 0;
        self.pollinating = true;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if let (true, Some(pos)) = (self.successful > POLLINATE_MIN_TICKS, self.target) {
            mob.nectar = true;
            mob.crops_grown = 0;
            mob.flower = Some(pos);
            mob.changed(data::SPECIES);
        }
        self.pollinating = false;
        self.target = None;
        self.retry = POLLINATE_STOP_COOLDOWN;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(pos) = self.target else { return };
        self.ticks += 1;
        if self.ticks > POLLINATE_MAX_TICKS {
            self.pollinating = false;
            return;
        }
        let center = [pos.x as f64 + 0.5, pos.y as f64 + 0.6, pos.z as f64 + 0.5];
        if mob.distance_sq(center) > 1.0 {
            if mob.navigation.is_done() {
                mob.move_to_position(center, 1.0, cx);
            }
            return;
        }
        mob.movement.set_wanted(center, 0.35);
        self.successful += 1;
    }
}

/// Grows the crop under a bee that carries nectar and has a hive.
#[derive(Debug, Default)]
struct GrowCrop;

const CROPS_PER_POLLINATION: i32 = 10;

impl Goal for GrowCrop {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.crops_grown < CROPS_PER_POLLINATION
            && cx.rng.gen_range(0.0..1.0f32) >= 0.3
            && mob.nectar
            && mob
                .hive
                .is_some_and(|h| (cx.block)(h).is_some_and(hive_block))
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx)
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if cx.rng.gen_range(0..30) != 0 {
            return;
        }
        let at = block_pos(mob.base.position);
        for depth in 1..=2 {
            let pos = BlockPos::new(at.x, at.y - depth, at.z);
            let Some(state) = (cx.block)(pos) else {
                continue;
            };
            if !tuff_datapack::tags::is(tag::block::key::BEE_GROWABLES, state.block()) {
                continue;
            }
            let Some(age) = state.get("age").and_then(|a| a.parse::<i32>().ok()) else {
                continue;
            };
            let Some(grown) = state.with("age", &(age + 1).to_string()) else {
                continue;
            };
            cx.effects.push(Effect::SetBlock { pos, state: grown });
            mob.crops_grown += 1;
        }
    }
}

/// Heads for the hive the bee knows (or the nearest with room) when it
/// carries nectar or it is dark or wet, and goes in once close.
#[derive(Debug, Default)]
struct EnterHive {
    retry: i32,
    blacklist: Vec<BlockPos>,
    travelling: i32,
    stuck: i32,
    last_distance: f64,
}

impl EnterHive {
    fn drop_and_blacklist(&mut self, mob: &mut MobCore) {
        if let Some(hive) = mob.hive.take() {
            self.blacklist.push(hive);
            if self.blacklist.len() > MAX_BLACKLISTED_HIVES {
                self.blacklist.remove(0);
            }
        }
        self.retry = 200;
        self.travelling = 0;
        self.stuck = 0;
    }
}

fn hive_block(state: tuff_vanilla_data::BlockState) -> bool {
    matches!(
        state.block(),
        tuff_vanilla_data::Block::Beehive | tuff_vanilla_data::Block::BeeNest
    )
}

fn hive_heading(mob: &MobCore, cx: &TickContext) -> Option<[f64; 3]> {
    let hive = mob
        .hive
        .filter(|hive| (cx.block)(*hive).is_some_and(hive_block))?;
    let threshold = 24.0;
    let to_hive = [
        f64::from(hive.x) + 0.5 - mob.base.position[0],
        f64::from(hive.y) + 0.5 - mob.base.position[1],
        f64::from(hive.z) + 0.5 - mob.base.position[2],
    ];
    let length =
        (to_hive[0] * to_hive[0] + to_hive[1] * to_hive[1] + to_hive[2] * to_hive[2]).sqrt();
    (length >= threshold).then(|| to_hive.map(|c| c / length))
}

fn wants_hive(mob: &MobCore, cx: &TickContext) -> bool {
    mob.cannot_enter_hive <= 0
        && mob.anger.is_none()
        && (mob.nectar
            || mob.ticks_without_nectar > NECTAR_TIMEOUT
            || cx.raining
            || cx.sky_darken >= 4)
        && !mob
            .hive
            .is_some_and(|hive| crate::hive::fire_near(cx.block, hive))
}

impl Goal for EnterHive {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !wants_hive(mob, cx) {
            return false;
        }
        self.retry -= 1;
        if self.retry > 0 {
            return false;
        }
        self.retry = 20;
        if mob
            .hive
            .is_some_and(|h| (cx.block)(h).is_some_and(hive_block))
        {
            return true;
        }
        let found: Vec<BlockPos> = cx
            .pois
            .closest_first(
                block_pos(mob.base.position),
                20,
                crate::world::poi::Occupancy::Any,
                |kind| kind == tuff_vanilla_data::registry::PointOfInterestType::Beehive,
            )
            .into_iter()
            .map(|(pos, _)| pos)
            .filter(|pos| (cx.block)(*pos).is_some_and(hive_block))
            .collect();
        mob.hive = found.iter().copied().find(|p| !self.blacklist.contains(p));
        if mob.hive.is_none() && !found.is_empty() {
            self.blacklist.clear();
            mob.hive = found.first().copied();
        }
        mob.hive.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        wants_hive(mob, cx) && mob.hive.is_some()
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.travelling = 0;
        self.stuck = 0;
        self.last_distance = f64::INFINITY;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(hive) = mob.hive else { return };
        if !(cx.block)(hive).is_some_and(hive_block) {
            mob.hive = None;
            return;
        }
        let center = [
            hive.x as f64 + 0.5,
            hive.y as f64 + 0.5,
            hive.z as f64 + 0.5,
        ];
        if mob.distance_sq(center) < 4.0 {
            cx.state
                .get::<crate::hive::Entries>()
                .push(mob.base.id, hive);
            mob.cannot_enter_hive = 20;
            return;
        }
        self.travelling += 1;
        if self.travelling > TRAVEL_TICKS || mob.distance_sq(center) > TOO_FAR * TOO_FAR {
            self.drop_and_blacklist(mob);
            return;
        }
        if mob.navigation.is_done() {
            if !mob.move_to_position(center, 1.0, cx) {
                self.drop_and_blacklist(mob);
                return;
            }
            let distance = mob.distance_sq(center);
            if distance < self.last_distance {
                self.stuck = 0;
            } else {
                self.stuck += 1;
                if self.stuck > HIVE_STUCK_TICKS {
                    mob.hive = None;
                    self.retry = 200;
                    self.stuck = 0;
                    return;
                }
            }
            self.last_distance = distance;
        }
    }
}

/// Flies back toward the flower the bee remembers once it has gone long
/// enough without nectar.
#[derive(Debug, Default)]
struct GoToKnownFlower {
    travelling: i32,
}

impl Goal for GoToKnownFlower {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(flower) = mob.flower else {
            return false;
        };
        if (cx.block)(flower)
            .is_some_and(|s| !tuff_datapack::tags::is(tag::block::key::FLOWERS, s.block()))
        {
            mob.flower = None;
            return false;
        }
        mob.anger.is_none()
            && mob.ticks_without_nectar > KNOWN_FLOWER_DELAY
            && mob.distance_sq([
                f64::from(flower.x) + 0.5,
                f64::from(flower.y) + 0.5,
                f64::from(flower.z) + 0.5,
            ]) >= 4.0
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx)
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.travelling = 0;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(flower) = mob.flower else { return };
        self.travelling += 1;
        let center = [
            f64::from(flower.x) + 0.5,
            f64::from(flower.y) + 0.5,
            f64::from(flower.z) + 0.5,
        ];
        if self.travelling > TRAVEL_TICKS || mob.distance_sq(center) > TOO_FAR * TOO_FAR {
            mob.flower = None;
            return;
        }
        if mob.navigation.is_done() {
            mob.move_to_position(center, 1.0, cx);
        }
    }
}

#[derive(Debug)]
pub struct Bee {
    has_stung: bool,
    time_since_sting: i32,
}

impl Species for Bee {
    fn debug_value(
        &self,
        core: &MobCore,
        feed: tuff_vanilla_data::registry::DebugSubscription,
        _tick: u64,
    ) -> Option<tuff_core::protocol::play::DebugValue> {
        (feed == tuff_vanilla_data::registry::DebugSubscription::Bees).then(|| {
            tuff_core::protocol::play::DebugValue::Bee {
                hive: core.hive,
                flower: core.flower,
                travel_ticks: 0,
                blacklisted_hives: Vec::new(),
            }
        })
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self {
            has_stung: false,
            time_since_sting: 0,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.omnidirectional_air = true;
        core.navigation.kind = NavKind::Fly;
        core.move_style = MoveStyle::Flying {
            hovers: true,
            max_turn: 20.0,
        };
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Water, -1.0);
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Fire, -1.0);
        core.attack_hook = Some(poison_hook);
        let mut goals = GoalSelector::default();
        goals.add(0, HurtByTarget::new(&[]));
        goals.add(
            1,
            super::target::NearestAttackablePlayer::with_interval(10, true, Some(bee_grudge)),
        );
        goals.add(2, super::target::ResetUniversalAnger::new(true));
        goals.add(1, MeleeAttack::new(1.4, true).with_on_hit(sting_hook));
        goals.add(1, EnterHive::default());
        goals.add(2, Breed::new(1.0));
        goals.add(3, Tempt::new(1.25, bee_food));
        goals.add(4, Pollinate::default());
        goals.add(5, GoToKnownFlower::default());
        goals.add(7, GrowCrop);
        goals.add(5, RandomStroll::flying(1.0, 30).heading(hive_heading));
        goals.add(6, LookAtPlayer::new(6.0));
        goals.add(7, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.has_stung = take_bool(nbt, "HasStung").unwrap_or(false);
        core.nectar = take_bool(nbt, "HasNectar").unwrap_or(false);
        core.flower = take(nbt, "flower_pos", |t| {
            let list = t.as_int_array()?;
            (list.len() == 3).then(|| BlockPos::new(list[0], list[1], list[2]))
        });
        core.hive = take(nbt, "hive_pos", |t| {
            let list = t.as_int_array()?;
            (list.len() == 3).then(|| BlockPos::new(list[0], list[1], list[2]))
        });
        core.cannot_enter_hive = take_number(nbt, "CannotEnterHiveTicks").map_or(0, |n| n as i32);
        if let Some(time) = take_number(nbt, "TicksSincePollination") {
            core.ticks_without_nectar = time as i32;
        }
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        out.insert("HasStung", self.has_stung);
        out.insert("HasNectar", core.nectar);
        out.insert("TicksSincePollination", core.ticks_without_nectar);
        out.insert("CannotEnterHiveTicks", core.cannot_enter_hive);
        if let Some(pos) = core.hive {
            out.insert("hive_pos", Tag::IntArray(vec![pos.x, pos.y, pos.z]));
        }
        if let Some(pos) = core.flower {
            out.insert("flower_pos", Tag::IntArray(vec![pos.x, pos.y, pos.z]));
        }
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut flags = 0i8;
        if core.anger.is_some() && !self.has_stung {
            flags |= FLAG_ROLLING;
        }
        if self.has_stung {
            flags |= FLAG_STUNG;
        }
        if core.nectar {
            flags |= FLAG_NECTAR;
        }
        let anger_time = core.anger.map_or(-1, |a| i64::from(a.ticks));
        vec![
            (data::SPECIES, MetadataValue::Byte(flags)),
            (ANGER_TIME, MetadataValue::VarLong(anger_time)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityBeeLoopAggressive),
            hurt: Some(SoundEvent::EntityBeeHurt),
            death: Some(SoundEvent::EntityBeeDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn is_food<M: tuff_item::item::Mode>(&self, stack: &ItemStack<M>) -> bool {
        bee_food(stack.item())
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.update_anger(false, cx);
        core.cannot_enter_hive = (core.cannot_enter_hive - 1).max(0);
        if core.signal != 0 {
            core.signal = 0;
            if !self.has_stung {
                self.has_stung = true;
                self.time_since_sting = 0;
                core.changed(data::SPECIES);
                core.sound(SoundEvent::EntityBeeSting, 1.0, 1.0, cx);
            }
        }
        if self.has_stung {
            self.time_since_sting += 1;
            let bound = (STING_DEATH_TICKS - self.time_since_sting).clamp(1, STING_DEATH_TICKS);
            if self.time_since_sting % 5 == 0 && cx.rng.gen_range(0..bound) == 0 {
                core.hurt_later(f32::MAX, &damage_type::GENERIC);
            }
        }
        if !core.nectar {
            core.ticks_without_nectar += 1;
        }
        if core.body.in_water {
            core.underwater_ticks += 1;
        } else {
            core.underwater_ticks = 0;
        }
        if core.underwater_ticks > DROWN_TICKS {
            core.hurt_later(1.0, &damage_type::DROWN);
        }
    }

    fn offspring(&self, _partner: &Self, _rng: &mut StdRng) -> Self {
        Self {
            has_stung: false,
            time_since_sting: 0,
        }
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self, _core: &MobCore) -> bool {
        false
    }
}

/// Registers the bee's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Bee, super::load::<Bee>);
    registry.register_spawn_rule(EntityType::Bee, super::spawning::animal_rule);
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use tuff_vanilla_data::Block;

    use super::*;
    use tuff_core::world::entity::EntityBase;

    /// A bee over a stone floor with one dandelion at `flower`.
    fn with_bee(flower: BlockPos, f: impl FnOnce(&mut MobCore, &mut Bee, &mut TickContext)) {
        let shared = tuff_test_support::bare_shared();
        let block = move |pos: BlockPos| {
            Some(if pos == flower {
                Block::Dandelion.default_state()
            } else if pos.y < 64 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = StdRng::seed_from_u64(1);
        let mut effects_out = Vec::new();
        let mut budget = 10_000;
        let pois = crate::world::poi::PoiStore::default();
        let mut cx = TickContext {
            tick: 1,
            dimension: tuff_core::world::dimension::DimensionId::for_tests(0),
            block: &block,
            light: &light,
            sky_darken: 0,
            ambient_light: 0.0,
            monsters_burn: true,
            raining: false,
            biome: &biome,
            container: &|_| None,
            container_info: &|_| Default::default(),
            inhabited_time: &|_| 0,
            min_y: -64,
            max_y: 319,
            players: &[],
            players_in_dimension: 0,
            entities: &[],
            entity_grid: None,
            pistons: &[],
            parts: &[],
            shared: &shared,
            rng: &mut rng,
            effects: &mut effects_out,
            path_budget: &mut budget,
            pois: &pois,
            state: &tuff_core::world::region_api::RegionStateLock::default(),
        };
        // Spawn already hovering over the flower: this is a species-level
        // unit test, so nothing actually applies `movement.set_wanted` to
        // `base.position` the way a real region tick would.
        let position = [
            flower.x as f64 + 0.5,
            flower.y as f64 + 0.6,
            flower.z as f64 + 0.5,
        ];
        let base = EntityBase::new(EntityType::Bee, 1, 1, position, [0.0; 3]);
        let mut core = MobCore::new(base);
        Bee::configure(&mut core);
        let mut bee = Bee::create(cx.rng);
        f(&mut core, &mut bee, &mut cx);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bee_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Bee.data_layout();
        assert_eq!(layout.len(), ANGER_TIME as usize + 1);
        assert_eq!(
            layout[data::SPECIES as usize],
            MetadataValue::Byte(0).type_id() as u8
        );
        assert_eq!(
            layout[ANGER_TIME as usize],
            MetadataValue::VarLong(0).type_id() as u8
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bee_pollinates_a_nearby_flower_and_eventually_loses_the_nectar() {
        let flower = BlockPos::new(2, 65, 0);
        with_bee(flower, |core, bee, cx| {
            let mut pollinate = Pollinate::default();
            assert!(
                pollinate.can_use(core, cx) || {
                    pollinate.retry = 0;
                    pollinate.can_use(core, cx)
                }
            );
            pollinate.start(core, cx);
            for _ in 0..500 {
                pollinate.tick(core, cx);
                bee.ai_step(core, cx);
                if pollinate.successful > POLLINATE_MIN_TICKS {
                    break;
                }
            }
            assert!(!core.nectar, "nectar only arrives when the goal stops");
            pollinate.stop(core, cx);
            assert!(core.nectar, "the bee should have picked up nectar");
            assert_eq!(core.flower, Some(flower));

            core.nectar = false;
            assert!(!wants_hive(core, cx));
            for _ in 0..(NECTAR_TIMEOUT + 1) {
                bee.ai_step(core, cx);
            }
            assert!(
                wants_hive(core, cx),
                "a bee tired of looking for nectar heads home"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bee_under_water_drowns_after_twenty_ticks() {
        with_bee(BlockPos::new(2, 65, 0), |core, bee, cx| {
            core.body.in_water = true;
            for _ in 0..DROWN_TICKS {
                bee.ai_step(core, cx);
            }
            assert!(core.pending_hurts.is_empty());
            bee.ai_step(core, cx);
            assert_eq!(core.pending_hurts.len(), 1);
            core.body.in_water = false;
            bee.ai_step(core, cx);
            assert_eq!(core.underwater_ticks, 0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_hive_blacklist_keeps_three_and_a_known_flower_waits_six_hundred_ticks() {
        with_bee(BlockPos::new(2, 65, 0), |core, _bee, cx| {
            let mut goal = EnterHive::default();
            for x in 0..5 {
                core.hive = Some(BlockPos::new(x, 70, 0));
                goal.drop_and_blacklist(core);
            }
            assert_eq!(goal.blacklist.len(), MAX_BLACKLISTED_HIVES);
            assert_eq!(goal.blacklist[0], BlockPos::new(2, 70, 0));
            assert!(core.hive.is_none());

            let mut known = GoToKnownFlower::default();
            core.flower = Some(BlockPos::new(2, 65, 0));
            core.base.position = [30.5, 65.0, 0.5];
            core.ticks_without_nectar = KNOWN_FLOWER_DELAY;
            assert!(!known.can_use(core, cx));
            core.ticks_without_nectar += 1;
            assert!(known.can_use(core, cx));
            core.flower = Some(BlockPos::new(30, 65, 0));
            assert!(!known.can_use(core, cx), "not a flower anymore");
            assert!(core.flower.is_none());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bee_hovers_but_air_travel_pulls_down_once_gravity_is_on() {
        with_bee(BlockPos::new(100, 65, 0), |core, _, cx| {
            core.base.position = [0.5, 70.0, 0.5];
            core.base.no_gravity = true;
            core.travel([0.0; 3], cx);
            assert!(core.base.velocity[1].abs() < 1e-9);
            core.base.no_gravity = false;
            core.travel([0.0; 3], cx);
            assert!(core.base.velocity[1] < 0.0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bee_and_a_parrot_navigate_by_flight() {
        with_bee(BlockPos::new(100, 65, 0), |core, _, cx| {
            assert_eq!(core.navigation.kind, NavKind::Fly);
            assert!(!core.flying && core.omnidirectional_air);
            assert!(matches!(core.move_style, MoveStyle::Flying { .. }));
            let target = BlockPos::new(core.base.position[0].floor() as i32 + 4, 70, 0);
            let path = core
                .create_path(target, 0, false, cx)
                .expect("a flight path");
            assert_eq!(path.end().map(|n| n.pos()), Some(target));
        });
        let mut parrot = MobCore::new(EntityBase::new(
            EntityType::Parrot,
            2,
            2,
            [0.0; 3],
            [0.0; 3],
        ));
        super::super::parrot::Parrot::configure(&mut parrot);
        assert_eq!(parrot.navigation.kind, NavKind::Fly);
        assert!(matches!(parrot.move_style, MoveStyle::Flying { .. }));
        assert!(!parrot.flying && parrot.omnidirectional_air);
    }

    #[tuff_core::watchdog]
    #[test]
    fn stinging_marks_it_and_it_dies_a_while_later() {
        with_bee(BlockPos::new(100, 65, 0), |core, bee, cx| {
            sting_hook(core);
            bee.ai_step(core, cx);
            assert!(bee.has_stung);
            assert_eq!(bee.time_since_sting, 1);

            for _ in 0..STING_DEATH_TICKS {
                bee.ai_step(core, cx);
            }
            assert!(
                core.pending_hurts.iter().any(|(a, _)| *a == f32::MAX),
                "a stung bee eventually takes lethal damage"
            );
        });
    }
}
