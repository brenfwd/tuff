//! Iron and snow golems: neutral utility mobs that are never summoned or
//! naturally spawned, only assembled by a player placing a pumpkin head on
//! top of a body built from blocks (see [`try_assemble`]). Copper golems
//! are a third, peaceful utility mob (see [`CopperGolem`] for what of its
//! real behaviour is and is not modeled).

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::tag::block as tags;
use tuff_vanilla_data::{Block, BlockState, Item};

use super::attack::MeleeAttack;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{LookAtPlayer, RandomLookAround, RandomStroll};
use super::target::{HurtByTarget, NearestAttackableEntity, TargetView};
use super::{Damage, MobCore, MobSounds, OwnAirRules, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{
    Culprit, Effect, EntityInteraction, InteractOutcome, RegionEntity, TickContext, take_bool,
};
use tuff_core::world::messages::HandEffect;
use tuff_core::world::projectile::{self, Launch, Owner, Shot, shot_velocity};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;
use tuff_vanilla_common::placement;
use tuff_world_model::BlockPos;

/// Hostile mobs both golems will fight without being provoked. Hand-listed
/// (there is no dumped "is an enemy" flag), matching what implements the
/// game's hostile marker interface; kept as one list shared by both species
/// rather than duplicated.
pub(super) const HOSTILE_MOBS: &[EntityType] = &[
    EntityType::Zombie,
    EntityType::Husk,
    EntityType::Drowned,
    EntityType::ZombieVillager,
    EntityType::Skeleton,
    EntityType::Stray,
    EntityType::WitherSkeleton,
    EntityType::Bogged,
    EntityType::Creeper,
    EntityType::Spider,
    EntityType::CaveSpider,
    EntityType::Enderman,
    EntityType::Endermite,
    EntityType::Silverfish,
    EntityType::Witch,
    EntityType::Blaze,
    EntityType::Ghast,
    EntityType::Slime,
    EntityType::MagmaCube,
    EntityType::ZombifiedPiglin,
    EntityType::PiglinBrute,
    EntityType::Guardian,
    EntityType::ElderGuardian,
    EntityType::Phantom,
    EntityType::Vex,
    EntityType::Vindicator,
    EntityType::Pillager,
    EntityType::Evoker,
    EntityType::Ravager,
    EntityType::Illusioner,
    EntityType::Breeze,
    EntityType::Warden,
    EntityType::Shulker,
    EntityType::Wither,
];

/// Iron golems never pick a fight with a creeper (it would just explode on
/// them); everything else in [`HOSTILE_MOBS`] is fair game.
const IRON_GOLEM_ENEMIES: &[EntityType] = &[
    EntityType::Zombie,
    EntityType::Husk,
    EntityType::Drowned,
    EntityType::ZombieVillager,
    EntityType::Skeleton,
    EntityType::Stray,
    EntityType::WitherSkeleton,
    EntityType::Bogged,
    EntityType::Spider,
    EntityType::CaveSpider,
    EntityType::Enderman,
    EntityType::Endermite,
    EntityType::Silverfish,
    EntityType::Witch,
    EntityType::Blaze,
    EntityType::Ghast,
    EntityType::Slime,
    EntityType::MagmaCube,
    EntityType::ZombifiedPiglin,
    EntityType::PiglinBrute,
    EntityType::Guardian,
    EntityType::ElderGuardian,
    EntityType::Phantom,
    EntityType::Vex,
    EntityType::Vindicator,
    EntityType::Pillager,
    EntityType::Evoker,
    EntityType::Ravager,
    EntityType::Illusioner,
    EntityType::Breeze,
    EntityType::Warden,
    EntityType::Shulker,
    EntityType::Wither,
];

/// The entity data index a golem's own single flags byte lands at. Neither
/// golem is ageable, so the baby/age-locked slots ([`data::BABY`],
/// [`data::AGE_LOCKED`]) do not exist for them and this index belongs to
/// the species instead; verified against the dumped layouts in
/// [`tests::golem_data_indices_match_the_dumped_layouts`].
const FLAGS: u8 = data::BABY;

/// Whether damage of `kind` is fall damage that a fall-damage-immune type
/// (per the dumped `fall_damage_immune` entity tag) should shrug off.
fn immune_to_fall(core: &MobCore, damage: &Damage) -> bool {
    damage.kind == &tuff_vanilla_data::damage_type::FALL
        && tuff_core::world::entity_in_tag(core.kind(), "fall_damage_immune")
}

fn common_goals(core: &mut MobCore) {
    core.goals.add(7, RandomStroll::water_avoiding(1.0));
    core.goals.add(8, LookAtPlayer::new(6.0));
    core.goals.add(8, RandomLookAround::default());
}

// ---------------------------------------------------------------------
// Iron golem
// ---------------------------------------------------------------------

/// Golem "cracked" tiers by remaining health fraction, matching the
/// thresholds the client renders (0.75, 0.5, 0.25): used only to notice a
/// tier change and play the damage sound, since the client derives the
/// crack texture itself from health.
fn crack_tier(core: &MobCore) -> u8 {
    let fraction = core.health / core.max_health();
    if fraction < 0.25 {
        3
    } else if fraction < 0.5 {
        2
    } else if fraction < 0.75 {
        1
    } else {
        0
    }
}

const SPARED: &[EntityType] = &[EntityType::Creeper];
const SPARED_BY_PLAYER_MADE: &[EntityType] = &[EntityType::Creeper, EntityType::Player];

/// What an iron golem never attacks: creepers, and players too when one
/// built it.
fn spared(player_created: bool) -> &'static [EntityType] {
    if player_created {
        SPARED_BY_PLAYER_MADE
    } else {
        SPARED
    }
}

fn iron_grudge(mob: &MobCore, player: &tuff_core::world::entity::NearbyPlayer) -> bool {
    mob.angry_at_player(player.uuid)
}

#[derive(Debug, Default)]
pub struct IronGolem {
    player_created: bool,
    crack_tier: u8,
}

impl Species for IronGolem {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.attack_hook = Some(iron_golem_hit);
        core.never_attacks = SPARED;
        core.retargets_on_push = true;
        let mut goals = GoalSelector::default();
        goals.add(1, MeleeAttack::new(1.0, true));
        goals.add(1, DefendVillage::default());
        goals.add(2, HurtByTarget::new(&[]));
        goals.add(4, super::target::ResetUniversalAnger::new(false));
        goals.add(2, MoveTowardsTarget::default());
        goals.add(2, MoveBackToVillage::default());
        goals.add(
            3,
            super::target::NearestAttackablePlayer::with_interval(10, true, Some(iron_grudge)),
        );
        goals.add(3, NearestAttackableEntity::new(IRON_GOLEM_ENEMIES, false));
        goals.add(4, StrollInVillage::default());
        goals.add(5, OfferFlower::default());
        goals.add(7, LookAtPlayer::new(6.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.player_created = take_bool(nbt, "PlayerCreated").unwrap_or(false);
        core.never_attacks = spared(self.player_created);
        self.crack_tier = crack_tier(core);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("PlayerCreated", self.player_created);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(FLAGS, MetadataValue::Byte(i8::from(self.player_created)))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: None,
            hurt: Some(SoundEvent::EntityIronGolemHurt),
            death: Some(SoundEvent::EntityIronGolemDeath),
            step: Some(SoundEvent::EntityIronGolemStep),
            volume: 1.0,
        }
    }

    fn before_hurt(&mut self, core: &mut MobCore, damage: &Damage, _cx: &mut TickContext) -> bool {
        !immune_to_fall(core, damage)
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.update_anger(true, cx);
    }

    fn after_hurt(&mut self, core: &mut MobCore, _damage: &Damage, cx: &mut TickContext) {
        let tier = crack_tier(core);
        if tier != self.crack_tier {
            self.crack_tier = tier;
            core.sound(SoundEvent::EntityIronGolemDamage, 1.0, 1.0, cx);
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if stack.item() != Item::IronIngot || !core.is_alive() {
            return None;
        }
        let before = core.health;
        core.heal(25.0);
        if core.health == before {
            return None;
        }
        let pitch = 1.0 + (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2;
        core.sound(SoundEvent::EntityIronGolemRepair, 1.0, pitch, cx);
        Some(HandEffect::Consume(1))
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }

    /// Iron golems never lose air anywhere (the game's own air-supply
    /// hook is a no-op for them), so they never drown; modeled as own air
    /// rules with zero damage rather than skipping the common check
    /// entirely, which keeps the "does this type have its own air rules"
    /// data-driven flag and the code in agreement.
    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(OwnAirRules::NEVER_LOSES_AIR)
    }
}

const VILLAGE_STROLL_SPEED: f64 = 0.6;

pub mod iron_event {
    pub const ATTACK: i8 = 4;
    pub const OFFER_FLOWER: i8 = 11;
    pub const STOP_OFFERING: i8 = 34;
}

/// The hit rolls between half and one and a half times its attack damage
/// and throws the victim up.
fn iron_golem_hit(
    mob: &MobCore,
    _target: &TargetView,
    hit: &mut tuff_core::world::entity::EntityHit,
    cx: &mut TickContext,
) {
    let damage = mob.attribute(tuff_vanilla_data::registry::Attribute::AttackDamage) as f32;
    let whole = damage as i32;
    if whole > 0 {
        hit.amount = damage / 2.0 + cx.rng.gen_range(0..whole) as f32;
    }
    hit.push = Some([0.0, 0.400_000_005_960_464_5, 0.0]);
    hit.push_power = Some(1.0);
    mob.entity_event(iron_event::ATTACK, cx);
    mob.sound(SoundEvent::EntityIronGolemAttack, 1.0, 1.0, cx);
}

/// Turns on any player a villager around it holds a grudge against.
#[derive(Debug)]
struct DefendVillage {
    keeping: super::target::Keeping,
    found: Option<i32>,
}

impl Default for DefendVillage {
    fn default() -> Self {
        Self {
            keeping: super::target::Keeping::new(false),
            found: None,
        }
    }
}

impl DefendVillage {
    const REACH: [f64; 3] = [10.0, 8.0, 10.0];
    const RANGE: f64 = 64.0;
}

impl Goal for DefendVillage {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let area = super::physics::inflate(mob.bounds(), Self::REACH);
        let grudges = cx.state.get::<super::villager::brain::Grudges>();
        let mut found = None;
        for villager in cx.entities.iter().filter(|e| {
            e.kind == EntityType::Villager
                && e.living.is_some_and(|l| l.alive)
                && super::physics::intersects(e.bounds, area)
        }) {
            let hated = grudges.of(villager.id, cx.tick);
            for player in cx.players {
                if hated.contains(&player.uuid)
                    && super::physics::intersects(player.bounds(), area)
                    && mob.distance_sq(player.position) <= Self::RANGE * Self::RANGE
                {
                    found = Some(player.id);
                }
            }
        }
        drop(grudges);
        self.found = found;
        let Some(target) = found.and_then(|id| super::target::view(cx, id)) else {
            return false;
        };
        !target.creative && mob.can_attack(&target, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.keeping.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = self.found;
        self.keeping.start();
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.keeping.stop(mob);
    }
}

/// By day, now and then holds out a poppy to a villager or copper golem
/// close by for twenty seconds; a copper golem still beside it at the end
/// takes it.
#[derive(Debug, Default)]
struct OfferFlower {
    to: Option<i32>,
    ticks: i32,
}

impl OfferFlower {
    const TICKS: i32 = 400;
    const CHANCE: u32 = 8000;
    const REACH: [f64; 3] = [6.0, 2.0, 6.0];
}

impl Goal for OfferFlower {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !super::goals::bright_outside(cx) || cx.rng.gen_range(0..Self::CHANCE) != 0 {
            return false;
        }
        let area = super::physics::inflate(mob.bounds(), Self::REACH);
        self.to = cx
            .entities
            .iter()
            .filter(|e| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::entity_type::key::CANDIDATE_FOR_IRON_GOLEM_GIFT,
                    e.kind,
                ) && e.living.is_some_and(|l| l.alive)
                    && super::physics::intersects(e.bounds, area)
                    && mob.distance_sq(e.position) <= 36.0
            })
            .min_by(|a, b| {
                mob.distance_sq(a.position)
                    .total_cmp(&mob.distance_sq(b.position))
            })
            .map(|e| e.id);
        self.to.is_some()
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        self.ticks > 0
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.ticks = self.adjusted_delay(Self::TICKS);
        mob.entity_event(iron_event::OFFER_FLOWER, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.entity_event(iron_event::STOP_OFFERING, cx);
        let area = super::physics::inflate(mob.bounds(), Self::REACH);
        if self.ticks == 0
            && let Some(to) = self.to.take()
            && cx.entities.iter().any(|e| {
                e.id == to
                    && tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::entity_type::key::ACCEPTS_IRON_GOLEM_GIFT,
                        e.kind,
                    )
                    && super::physics::intersects(e.bounds, area)
            })
        {
            cx.effects.push(Effect::Notify {
                target: to,
                notice: tuff_core::world::entity::Notice::Gifted(Item::Poppy),
            });
        }
        self.to = None;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(eye) = self
            .to
            .and_then(|id| super::target::view(cx, id))
            .map(|t| t.eye)
        {
            mob.look.set_look_at(eye, 30.0, 30.0);
        }
        self.ticks -= 1;
    }
}

fn block_of(position: [f64; 3]) -> BlockPos {
    BlockPos::new(
        position[0].floor() as i32,
        position[1].floor() as i32,
        position[2].floor() as i32,
    )
}

fn section_center(section: [i32; 3]) -> [f64; 3] {
    section.map(|s| f64::from(s * 16 + 8))
}

/// Now and then strolls, mostly toward a claimed point of interest in a
/// village section around it, so a golem keeps to its village.
#[derive(Debug, Default)]
struct StrollInVillage {
    target: [f64; 3],
}

impl StrollInVillage {
    const INTERVAL: i32 = 240;
    const SECTION_RADIUS: i32 = 2;
    const POI_RADIUS: i32 = 8;

    const VILLAGER_SCAN_RADIUS: f64 = 32.0;

    fn position(mob: &MobCore, cx: &mut TickContext) -> Option<[f64; 3]> {
        if cx.rng.r#gen::<f32>() < 0.3 {
            return super::random_pos::land_pos(mob, 10, 7, cx);
        }
        let target = if cx.rng.r#gen::<f32>() < 0.7 {
            Self::towards_villager(mob, cx).or_else(|| Self::towards_poi(mob, cx))
        } else {
            Self::towards_poi(mob, cx).or_else(|| Self::towards_villager(mob, cx))
        };
        target.or_else(|| super::random_pos::land_pos(mob, 10, 7, cx))
    }

    fn towards_villager(mob: &MobCore, cx: &mut TickContext) -> Option<[f64; 3]> {
        let area = super::physics::inflate(mob.bounds(), [Self::VILLAGER_SCAN_RADIUS; 3]);
        let tick = cx.tick;
        let facts = cx.state.get::<super::villager::brain::Facts>();
        let wanting: Vec<[f64; 3]> = cx
            .entities
            .iter()
            .filter(|e| e.kind == EntityType::Villager)
            .filter(|e| e.living.as_ref().is_some_and(|l| l.alive))
            .filter(|e| super::physics::intersects(e.bounds, area))
            .filter(|e| facts.wants_golem(e.id, tick))
            .map(|e| e.position)
            .collect();
        drop(facts);
        if wanting.is_empty() {
            return None;
        }
        let target = wanting[cx.rng.gen_range(0..wanting.len())];
        super::random_pos::land_pos_towards(mob, 10, 7, target, cx)
    }

    fn towards_poi(mob: &MobCore, cx: &mut TickContext) -> Option<[f64; 3]> {
        let at = block_of(mob.base.position);
        let middle = [at.x >> 4, at.y >> 4, at.z >> 4];
        let r = Self::SECTION_RADIUS;
        let mut sections = Vec::new();
        for z in middle[2] - r..=middle[2] + r {
            for y in middle[1] - r..=middle[1] + r {
                for x in middle[0] - r..=middle[0] + r {
                    let center = section_center([x, y, z]);
                    if cx.pois.sections_to_village(block_of(center)) == 0 {
                        sections.push(center);
                    }
                }
            }
        }
        if sections.is_empty() {
            return None;
        }
        let center = sections[cx.rng.gen_range(0..sections.len())];
        let pois = cx.pois.closest_first(
            block_of(center),
            Self::POI_RADIUS,
            tuff_core::world::poi::Occupancy::IsOccupied,
            |_| true,
        );
        if pois.is_empty() {
            return None;
        }
        let (poi, _) = pois[cx.rng.gen_range(0..pois.len())];
        let target = [poi.x as f64 + 0.5, poi.y as f64, poi.z as f64 + 0.5];
        super::random_pos::land_pos_towards(mob, 10, 7, target, cx)
    }
}

impl Goal for StrollInVillage {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if cx
            .rng
            .gen_range(0..super::goal::reduced_delay(Self::INTERVAL))
            != 0
        {
            return false;
        }
        match Self::position(mob, cx) {
            Some(target) => {
                self.target = target;
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.target, VILLAGE_STROLL_SPEED, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

/// Outside a village, heads for the nearby section closest to one.
#[derive(Debug, Default)]
struct MoveBackToVillage {
    target: [f64; 3],
}

impl MoveBackToVillage {
    const INTERVAL: i32 = 10;
    const SECTION_RADIUS: i32 = 2;

    fn position(mob: &MobCore, cx: &mut TickContext) -> Option<[f64; 3]> {
        let at = block_of(mob.base.position);
        let target = super::raider::toward_village(cx.pois, at, Self::SECTION_RADIUS)?;
        super::random_pos::pos_towards(mob, 10, 7, target, cx)
    }
}

impl Goal for MoveBackToVillage {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if cx.pois.sections_to_village(block_of(mob.base.position)) <= 1 {
            return false;
        }
        if cx
            .rng
            .gen_range(0..super::goal::reduced_delay(Self::INTERVAL))
            != 0
        {
            return false;
        }
        match Self::position(mob, cx) {
            Some(target) => {
                self.target = target;
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.target, VILLAGE_STROLL_SPEED, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

/// Walks a stretch toward its target while it is within 32 blocks, ahead
/// of the melee chase.
#[derive(Debug, Default)]
struct MoveTowardsTarget {
    wanted: [f64; 3],
}

impl MoveTowardsTarget {
    const SPEED: f64 = 0.9;
    const WITHIN: f64 = 32.0;

    fn target_near(mob: &MobCore, cx: &TickContext) -> Option<[f64; 3]> {
        let target = mob.target_view(cx)?;
        (target.alive && mob.distance_sq(target.position) <= Self::WITHIN * Self::WITHIN)
            .then_some(target.position)
    }
}

impl Goal for MoveTowardsTarget {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(target) = Self::target_near(mob, cx) else {
            return false;
        };
        let towards = super::random_pos::pos_towards_within(
            mob,
            16,
            7,
            target,
            std::f64::consts::FRAC_PI_2,
            cx,
        );
        match towards {
            Some(pos) => {
                self.wanted = pos;
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
            && Self::target_near(mob, cx)
                .is_some_and(|t| mob.distance_sq(t) < Self::WITHIN * Self::WITHIN)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.wanted, Self::SPEED, cx);
    }
}

/// Marks a freshly assembled iron golem as player-created (never attacks a
/// player it wasn't hurt by -- vanilla's own player-created golems still
/// react to being hit; that nuance is not modeled).
pub fn mark_player_created(entity: &mut dyn RegionEntity) {
    if let Some(mob) = entity.downcast_mut::<super::Mob<IronGolem>>() {
        mob.species.player_created = true;
        mob.core.never_attacks = spared(true);
        mob.core.changed(FLAGS);
    }
}

// ---------------------------------------------------------------------
// Snow golem
// ---------------------------------------------------------------------

const SNOW_GOLEM_SPEED: f64 = 1.25;
const SNOW_GOLEM_RANGE_SQ: f64 = 100.0;
const SNOW_GOLEM_ATTACK_INTERVAL: i32 = 20;
/// Biome attribute the data pack carries on biomes hot enough to melt a
/// bare-headed snow golem (a raw biome `attributes` entry).
const MELT_ATTRIBUTE: &str = "minecraft:gameplay/snow_golem_melts";

fn melts_at(cx: &TickContext, position: [f64; 3]) -> bool {
    let pos = super::physics::block_pos(position);
    (cx.biome)(pos).is_some_and(|biome| {
        tuff_vanilla_data::biome::Biome::get(biome).is_some_and(|b| {
            b.attributes
                .iter()
                .any(|(key, value)| *key == MELT_ATTRIBUTE && *value == "true")
        })
    })
}

#[derive(Debug)]
pub struct SnowGolem {
    /// Whether it still wears the pumpkin it was built with; a bare-headed
    /// one is what shearing produces. Defaults to `true`, matching a
    /// freshly assembled or freshly loaded (no saved value) golem.
    has_pumpkin: bool,
}

impl Default for SnowGolem {
    fn default() -> Self {
        Self { has_pumpkin: true }
    }
}

fn throw_snowball(core: &MobCore, target: &TargetView, cx: &mut TickContext) {
    let eye = core.eye_position();
    let start = [
        core.base.position[0],
        eye[1] - 0.100_000_001,
        core.base.position[2],
    ];
    let dx = target.position[0] - core.base.position[0];
    let dz = target.position[2] - core.base.position[2];
    let horizontal = (dx * dx + dz * dz).sqrt();
    let dy = (target.eye[1] - 1.1) - start[1] + horizontal * 0.2;
    let velocity = shot_velocity([dx, dy, dz], 1.6, 12.0, cx.rng);
    let id = cx.shared.next_entity_id();
    let owner = Owner::of(Culprit::entity(&core.base), None);
    #[expect(
        clippy::disallowed_methods,
        reason = "item a mob spawns, wears or throws (throw_snowball)"
    )]
    let item = ItemStack::mint(Item::Snowball, 1, MintReason::Spawn);
    let shot = Shot::Thrown {
        kind: EntityType::Snowball,
        item,
        velocity,
    };
    projectile::launch(
        Launch {
            id,
            position: start,
            owner,
            shot,
        },
        cx,
    );
    let pitch = 0.4 / (cx.rng.r#gen::<f32>() * 0.4 + 0.8);
    core.sound(SoundEvent::EntitySnowGolemShoot, 1.0, pitch, cx);
}

/// Keeps within snowball range of a target and throws at it on a fixed
/// cooldown -- no charge-up, unlike a skeleton's bow.
#[derive(Debug)]
struct RangedSnowballAttack {
    until_attack: i32,
    see_time: i32,
}

impl RangedSnowballAttack {
    fn new() -> Self {
        Self {
            until_attack: -1,
            see_time: 0,
        }
    }
}

impl Goal for RangedSnowballAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
        self.see_time = 0;
        self.until_attack = -1;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        let sees = mob.has_line_of_sight(&target, cx);
        self.see_time = if sees { self.see_time + 1 } else { 0 };
        if mob.distance_sq(target.position) > SNOW_GOLEM_RANGE_SQ || self.see_time < 5 {
            mob.move_to_entity(target.position, SNOW_GOLEM_SPEED, cx);
        } else {
            mob.navigation.stop();
        }
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        self.until_attack -= 1;
        if self.until_attack == 0 {
            if sees {
                throw_snowball(mob, &target, cx);
                self.until_attack = self.adjusted_delay(SNOW_GOLEM_ATTACK_INTERVAL);
            }
        } else if self.until_attack < 0 {
            self.until_attack = self.adjusted_delay(SNOW_GOLEM_ATTACK_INTERVAL);
        }
    }
}

impl Species for SnowGolem {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        let mut goals = GoalSelector::default();
        goals.add(1, RangedSnowballAttack::new());
        goals.add(1, NearestAttackableEntity::new(HOSTILE_MOBS, true));
        core.goals = goals;
        common_goals(core);
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.has_pumpkin = take_bool(nbt, "Pumpkin").unwrap_or(true);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Pumpkin", self.has_pumpkin);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let flags = if self.has_pumpkin { 0x10 } else { 0x00 };
        vec![(FLAGS, MetadataValue::Byte(flags))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: None,
            hurt: Some(SoundEvent::EntitySnowGolemHurt),
            death: Some(SoundEvent::EntitySnowGolemDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn before_hurt(&mut self, core: &mut MobCore, damage: &Damage, _cx: &mut TickContext) -> bool {
        !immune_to_fall(core, damage)
    }

    /// Shears: only ever removes the pumpkin (a player right-clicking with
    /// shears while it has one), dropping the carved pumpkin.
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
        self.shear(core, cx).then_some(HandEffect::Damage(1))
    }

    fn shear(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        if !self.has_pumpkin || !core.is_alive() {
            return false;
        }
        self.has_pumpkin = false;
        core.changed(FLAGS);
        core.sound(SoundEvent::EntitySnowGolemShear, 1.0, 1.0, cx);
        let facts = tuff_core::world::drops::EntityFacts {
            kind: Some(core.base.kind),
            tables: Some(cx.shared.loot_tables()),
            ..Default::default()
        };
        let [x, y, z] = core.base.position;
        for drop in
            cx.shared
                .block_loot()
                .entity_drops("minecraft:shearing/snow_golem", &facts, cx.rng)
        {
            cx.effects.push(Effect::DropItem {
                stack: drop,
                position: [x, y + f64::from(core.size.eye_height), z],
                velocity: [0.0; 3],
                pickup_delay: tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY,
            });
        }
        true
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        // Matches the game: melting is purely a function of position (the
        // `snow_golem_melts` biome attribute), not whether it has a
        // pumpkin -- confirmed against vanilla rather
        // than assumed.
        if core.is_alive() && melts_at(cx, core.base.position) {
            core.hurt_later(1.0, &tuff_vanilla_data::damage_type::ON_FIRE);
        }
        if cx.shared.rule_bool("mob_griefing") {
            leave_snow(core, cx);
        }
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Snow under each corner of a quarter-block square around its feet,
/// wherever there is air and snow could lie.
fn leave_snow(core: &MobCore, cx: &mut TickContext) {
    let snow = Block::Snow.default_state();
    let [x, y, z] = core.base.position;
    let block = cx.block;
    let level = |pos: BlockPos| block(pos).unwrap_or(Block::Air.default_state());
    let mut placed: Vec<BlockPos> = Vec::new();
    for i in 0..4 {
        let dx = f64::from(((i % 2) * 2 - 1) as f32 * 0.25);
        let dz = f64::from((((i / 2) % 2) * 2 - 1) as f32 * 0.25);
        let pos = BlockPos::new(
            (x + dx).floor() as i32,
            y.floor() as i32,
            (z + dz).floor() as i32,
        );
        let free = (cx.block)(pos).is_some_and(tuff_world_model::chunk::is_air);
        if free && !placed.contains(&pos) && placement::can_survive(snow, pos, &level) {
            placed.push(pos);
            cx.effects.push(Effect::SetBlock { pos, state: snow });
            cx.effects.push(Effect::GameEvent {
                event: tuff_core::world::game_event::GameEvent::BlockPlace,
                position: [
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + 0.5,
                    f64::from(pos.z) + 0.5,
                ],
                source: Some(core.base.id),
            });
        }
    }
}

// ---------------------------------------------------------------------
// Copper golem
// ---------------------------------------------------------------------

/// The entity data indices a copper golem's own two fields land at: it is
/// not ageable (baby/age-locked do not exist for it), so species data
/// starts at [`data::BABY`], matching the other golems above; verified
/// against the dumped layout in
/// [`tests::copper_golem_data_indices_match_the_dumped_layout`].
const WEATHER_STATE: u8 = data::BABY;
const GOLEM_STATE: u8 = data::BABY + 1;

/// `WeatheringCopper.WeatherState` ordinals (also the `weather_state` NBT
/// strings' order): unaffected, exposed, weathered, oxidized.
const OXIDIZED: u8 = 3;
const OXIDATION_NAMES: [&str; 4] = ["unaffected", "exposed", "weathered", "oxidized"];

/// `CopperGolemState` ordinals, read out of [`MobCore::signal`] (the same
/// "species state a goal needs to read" scratch field Blaze/Warden/Wither
/// already use): idle, taking an item, failing to take one, depositing
/// one, failing to deposit one.
const STATE_IDLE: i32 = 0;
const STATE_GETTING_ITEM: i32 = 1;
const STATE_GETTING_NO_ITEM: i32 = 2;
const STATE_DROPPING_ITEM: i32 = 3;
const STATE_DROPPING_NO_ITEM: i32 = 4;

/// Chance per tick a fully oxidized, unwaxed copper golem standing fully in
/// air turns into a statue.
const STATUE_CHANCE: f32 = 0.0058;
const STATUE_POSES: [&str; 4] = ["standing", "sitting", "running", "star"];

/// Game ticks between two weathering steps, inclusive range.
const WEATHERING_FROM: i64 = 504_000;
const WEATHERING_TO: i64 = 552_000;
/// `next_weather` while no step is scheduled yet, and while waxed.
const WEATHER_UNSET: i64 = -1;
const WEATHER_WAXED: i64 = -2;

/// A copper golem is peaceful and spends its idle time carrying items
/// between copper chests and regular/trapped chests ([`TransportItems`]).
/// It weathers a stage every 504000 to 552000 game ticks unless waxed; an
/// axe takes the wax or a stage off, and so does lightning. Not modeled:
/// shearing the poppy off. Container locks are also not checked (the
/// generic lock component exists for player-opened containers only).
#[derive(Debug)]
pub struct CopperGolem {
    /// 0 (unaffected) to [`OXIDIZED`] (3).
    oxidation: u8,
    /// The game time of the next weathering step, or [`WEATHER_UNSET`] or
    /// [`WEATHER_WAXED`].
    next_weather: i64,
    /// Items a container gave that the hand had no room for.
    overflow: Vec<ItemStack>,
}

impl Default for CopperGolem {
    fn default() -> Self {
        Self {
            oxidation: 0,
            next_weather: WEATHER_UNSET,
            overflow: Vec::new(),
        }
    }
}

impl CopperGolem {
    pub fn oxidation(&self) -> u8 {
        self.oxidation
    }

    pub fn waxed(&self) -> bool {
        self.next_weather == WEATHER_WAXED
    }

    fn set_oxidation(&mut self, core: &mut MobCore, stage: u8) {
        self.oxidation = stage;
        core.changed(WEATHER_STATE);
    }

    fn level_event(core: &MobCore, event: i32, cx: &mut TickContext) {
        let packet = tuff_core::world::framed(&tuff_core::protocol::play::LevelEvent {
            event,
            position: super::physics::block_pos(core.base.position),
            data: 0,
            global: false,
        });
        cx.effects
            .push(Effect::ChunkPackets(tuff_core::world::packets([packet])));
    }

    /// Steps its weathering on, at game time `now`: true when it was fully
    /// oxidized and unwaxed before the step (it may become a statue).
    fn weather(&mut self, core: &mut MobCore, now: i64, cx: &mut TickContext) -> bool {
        if self.next_weather == WEATHER_WAXED {
            return false;
        }
        if self.next_weather == WEATHER_UNSET {
            self.next_weather = now + cx.rng.gen_range(WEATHERING_FROM..=WEATHERING_TO);
            return false;
        }
        let was_oxidized = self.oxidation == OXIDIZED;
        if now >= self.next_weather && !was_oxidized {
            let next = self.oxidation + 1;
            self.set_oxidation(core, next);
            self.next_weather = if next == OXIDIZED {
                0
            } else {
                self.next_weather + cx.rng.gen_range(WEATHERING_FROM..=WEATHERING_TO)
            };
        }
        was_oxidized
    }

    /// Fully oxidized, unwaxed and standing in air, it has a small per-tick
    /// chance to petrify: drops its held item, then a
    /// [`Block::OxidizedCopperGolemStatue`] takes its place (random pose,
    /// facing the way it last faced) and it discards itself, with no death
    /// event or loot.
    fn try_statue(core: &mut MobCore, cx: &mut TickContext) {
        let pos = super::physics::block_pos(core.base.position);
        let in_air = (cx.block)(pos).is_some_and(tuff_world_model::chunk::is_air);
        if !in_air || cx.rng.r#gen::<f32>() > STATUE_CHANCE {
            return;
        }
        if let Some(stack) = core.equipment.take(EquipmentSlot::Mainhand) {
            core.drop_item(stack, 0.5, cx);
        }
        let facing = placement::from_yaw(core.base.yaw);
        let pose = STATUE_POSES[cx.rng.gen_range(0..STATUE_POSES.len())];
        if let Some(state) = Block::OxidizedCopperGolemStatue
            .default_state()
            .with_facing(facing)
            .and_then(|s| s.with("copper_golem_pose", pose))
        {
            cx.effects.push(Effect::SetBlock { pos, state });
        }
        core.sound(SoundEvent::EntityCopperGolemBecomeStatue, 1.0, 1.0, cx);
        core.discarded = true;
    }
}

impl Species for CopperGolem {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        let mut goals = GoalSelector::default();
        goals.add(0, super::goals::Panic::new(1.5));
        goals.add(5, TransportItems::default());
        core.goals = goals;
        common_goals(core);
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.oxidation = nbt
            .get_str("weather_state")
            .and_then(|s| OXIDATION_NAMES.iter().position(|n| *n == s))
            .unwrap_or(0) as u8;
        self.next_weather = tuff_core::world::entity::take_number(nbt, "next_weather_age")
            .map_or(WEATHER_UNSET, |v| v as i64);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert(
            "weather_state",
            OXIDATION_NAMES[self.oxidation as usize].to_owned(),
        );
        out.insert("next_weather_age", self.next_weather);
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        use tuff_core::protocol::play::level_event;
        let stack = interaction.stack.as_deref()?;
        if stack.item() == Item::Shears
            && core
                .equipment
                .get(EquipmentSlot::Saddle)
                .is_some_and(|antenna| {
                    tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::item::key::SHEARABLE_FROM_COPPER_GOLEM,
                        antenna.item(),
                    )
                })
            && let Some(antenna) = core.equipment.take(EquipmentSlot::Saddle)
        {
            core.sound_as(
                SoundEvent::EntityCopperGolemShear,
                tuff_core::protocol::play::sound_category::PLAYERS,
                1.0,
                1.0,
                cx,
            );
            core.drop_item(antenna, 1.5, cx);
            cx.effects.push(Effect::GameEvent {
                event: tuff_core::world::game_event::GameEvent::Shear,
                position: core.base.position,
                source: Some(interaction.player),
            });
            return Some(HandEffect::Damage(1));
        }
        if stack.item() == Item::Honeycomb && !self.waxed() {
            Self::level_event(core, level_event::PARTICLES_WAX_ON, cx);
            core.sound_as(
                SoundEvent::ItemHoneycombWaxOn,
                tuff_core::protocol::play::sound_category::BLOCKS,
                1.0,
                1.0,
                cx,
            );
            self.next_weather = WEATHER_WAXED;
            return Some(HandEffect::Consume(1));
        }
        if !tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::AXES, stack.item()) {
            return None;
        }
        if self.waxed() {
            core.sound(SoundEvent::ItemAxeScrape, 1.0, 1.0, cx);
            Self::level_event(core, level_event::PARTICLES_WAX_OFF, cx);
            self.next_weather = WEATHER_UNSET;
            return Some(HandEffect::Damage(1));
        }
        if self.oxidation > 0 {
            core.sound(SoundEvent::ItemAxeScrape, 1.0, 1.0, cx);
            Self::level_event(core, level_event::PARTICLES_SCRAPE, cx);
            self.next_weather = WEATHER_UNSET;
            let previous = self.oxidation - 1;
            self.set_oxidation(core, previous);
            return Some(HandEffect::Damage(1));
        }
        None
    }

    fn heard(&mut self, core: &mut MobCore, notice: tuff_core::world::entity::Notice) {
        use tuff_core::world::entity::Notice;
        match notice {
            Notice::Charged if self.oxidation > 0 => {
                self.next_weather = WEATHER_UNSET;
                let previous = self.oxidation - 1;
                self.set_oxidation(core, previous);
            }
            Notice::Gifted(item) if core.equipment.get(EquipmentSlot::Saddle).is_none() => {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "item a mob spawns, wears or throws (heard)"
                )]
                core.equipment.set(
                    EquipmentSlot::Saddle,
                    Some(ItemStack::mint(item, 1, MintReason::Spawn)),
                );
                core.equipment.set_drop_chance(EquipmentSlot::Saddle, 2.0);
            }
            _ => {}
        }
    }

    fn took_item(&mut self, core: &mut MobCore, stack: ItemStack) {
        match core.equipment.get_mut(EquipmentSlot::Mainhand) {
            None => {
                let none = core.equipment.replace(EquipmentSlot::Mainhand, Some(stack));
                debug_assert!(none.is_none());
            }
            Some(held) if held.stacks_with(&stack) => {
                held.join_or_drop(stack, &mut self.overflow);
            }
            Some(_) => self.overflow.push(stack),
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        for stack in std::mem::take(&mut self.overflow) {
            core.drop_item(stack, 0.0, cx);
        }
        if core.is_alive() && self.weather(core, cx.tick as i64, cx) {
            Self::try_statue(core, cx);
        }
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                WEATHER_STATE,
                MetadataValue::Holder {
                    serializer: core.base.kind.data_serializer(WEATHER_STATE).unwrap_or(0),
                    id: i32::from(self.oxidation),
                },
            ),
            (
                GOLEM_STATE,
                MetadataValue::Holder {
                    serializer: core.base.kind.data_serializer(GOLEM_STATE).unwrap_or(0),
                    id: core.signal,
                },
            ),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        let (hurt, death, step) = match self.oxidation {
            2 => (
                SoundEvent::EntityCopperGolemWeatheredHurt,
                SoundEvent::EntityCopperGolemWeatheredDeath,
                SoundEvent::EntityCopperGolemWeatheredStep,
            ),
            3 => (
                SoundEvent::EntityCopperGolemOxidizedHurt,
                SoundEvent::EntityCopperGolemOxidizedDeath,
                SoundEvent::EntityCopperGolemOxidizedStep,
            ),
            _ => (
                SoundEvent::EntityCopperGolemHurt,
                SoundEvent::EntityCopperGolemDeath,
                SoundEvent::EntityCopperGolemStep,
            ),
        };
        MobSounds {
            ambient: None,
            hurt: Some(hurt),
            death: Some(death),
            step: Some(step),
            volume: 1.0,
        }
    }

    fn before_hurt(&mut self, core: &mut MobCore, damage: &Damage, _cx: &mut TickContext) -> bool {
        !immune_to_fall(core, damage)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Sets a freshly assembled or reanimated copper golem's starting
/// oxidation stage.
pub fn mark_copper_oxidation(entity: &mut dyn RegionEntity, stage: u8) {
    if let Some(mob) = entity.downcast_mut::<super::Mob<CopperGolem>>() {
        mob.species.oxidation = stage;
        mob.core.changed(WEATHER_STATE);
    }
}

mod assembly;
pub use assembly::{Assembly, try_assemble};

/// Registers the golems' loaders. None of the three golems has a natural
/// spawn rule: all are only created by [`try_assemble`] (a copper golem
/// also by axe-reanimating a statue, see `region/interact.rs`), summoned,
/// or spawn-egged.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::IronGolem, super::load::<IronGolem>);
    registry.register(EntityType::SnowGolem, super::load::<SnowGolem>);
    registry.register(EntityType::CopperGolem, super::load::<CopperGolem>);
}

mod transport;
use transport::*;

#[cfg(test)]
mod tests;
