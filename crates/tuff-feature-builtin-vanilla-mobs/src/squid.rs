//! Squids and glow squids: passive water mobs that swim in pulses, each
//! stroke of their tentacles pushing them the way their goals chose, flee
//! whatever last hurt them and squirt ink when hit. Glow squids are squids
//! plus a saved "dark ticks remaining" counter and a glow tint; the
//! mechanic that sets that counter (being caught in a bucket) is out of
//! scope here (see project notes), so it only ever counts down from a hit.

use rand::Rng;
use rand::rngs::StdRng;
use std::f32::consts::{PI, TAU};
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::entity::MAX_AIR_SUPPLY;
use tuff_vanilla_data::registry::{EntityType, MobEffect, ParticleType, SoundEvent};

use super::goal::{Flags, Goal, GoalSelector, reduced_delay};
use super::movement::WaterTravel;
use super::physics::block_pos;
use super::target;
use super::{Damage, MobCore, MobSounds, OwnAirRules, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{Effect, TickContext, take_number};
use tuff_core::world::{framed, packets};
use tuff_nbt::Compound;

const FLEE_DISTANCE_SQ: f64 = 100.0;
const FLEE_SPEED: f64 = 3.0;
const FLEE_SLOW_FROM: f64 = 5.0;
const GRAVITY: f64 = 0.08;
const AIR_DRAG: f64 = 0.980_000_019_073_486_3;
/// Broadcast as each tentacle stroke begins, so viewers restart theirs.
const TENTACLE_STROKE: i8 = 19;

/// A squid out of water breathes like a fish (see the project notes' fish.rs
/// note): drains over 300 ticks and takes drowning damage at the common
/// threshold.
fn squid_air_rules() -> OwnAirRules {
    OwnAirRules {
        max: MAX_AIR_SUPPLY as i16,
        in_rain: false,
        damage_kind: &damage_type::DROWN,
        damage: 2.0,
    }
}

fn animal_experience(rng: &mut StdRng) -> i32 {
    1 + rng.gen_range(0..3)
}

fn spawn_ink(core: &MobCore, particle: ParticleType, sound: SoundEvent, cx: &mut TickContext) {
    core.sound(sound, 0.4, 1.0, cx);
    let [x, y, z] = core.base.position;
    let packet = framed(&play::Particles {
        particle: particle.id(),
        position: [x, y + 0.5, z],
        spread: [0.3, 0.3, 0.3],
        speed: 0.1,
        count: 20,
        alternative: false,
    });
    cx.effects.push(Effect::ChunkPackets(packets([packet])));
}

fn random_tentacle_speed(rng: &mut StdRng) -> f32 {
    1.0 / (rng.r#gen::<f32>() + 1.0) * 0.2
}

/// The tentacle stroke that moves a squid: late in each stroke it takes
/// on the velocity its goals chose (`MobCore::drift`), and between
/// strokes it coasts to a stop. Out of water it just falls.
#[derive(Debug)]
struct Tentacles {
    movement: f32,
    speed: f32,
}

impl Tentacles {
    fn new(rng: &mut StdRng) -> Self {
        Self {
            movement: 0.0,
            speed: random_tentacle_speed(rng),
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.movement += self.speed;
        if f64::from(self.movement) > std::f64::consts::TAU {
            self.movement -= TAU;
            if cx.rng.gen_range(0..10) == 0 {
                self.speed = random_tentacle_speed(cx.rng);
            }
            core.entity_event(TENTACLE_STROKE, cx);
        }
        if !core.body.in_water {
            let vy = match core.effects.amplifier(MobEffect::Levitation) {
                Some(amplifier) => 0.05 * f64::from(amplifier + 1),
                None if core.base.no_gravity => core.base.velocity[1],
                None => core.base.velocity[1] - GRAVITY,
            };
            core.base.velocity = [0.0, vy * AIR_DRAG, 0.0];
            return;
        }
        if self.movement < PI {
            let scale = self.movement / PI;
            if f64::from(scale) > 0.75 {
                core.base.velocity = core.drift;
            }
        } else {
            core.base.velocity = core.base.velocity.map(|c| c * 0.9);
        }
        let [vx, _, vz] = core.base.velocity;
        let target = -(vx.atan2(vz) as f32) * 57.295_776;
        core.body_yaw += (target - core.body_yaw) * 0.1;
        core.base.yaw = core.body_yaw;
    }
}

/// Picks a new random push now and then, or at once when it has none or
/// has just come out of the water; stops after long idleness.
#[derive(Debug, Default)]
struct RandomDrift;

impl Goal for RandomDrift {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if mob.no_action_time > 100 {
            mob.drift = [0.0; 3];
            return;
        }
        let has_drift = mob.drift.iter().map(|c| c * c).sum::<f64>() > 9.999_999_747_378_752e-6;
        if cx.rng.gen_range(0..reduced_delay(50)) == 0 || !mob.body.in_water || !has_drift {
            let angle = cx.rng.r#gen::<f32>() * TAU;
            let rise = -0.1 + cx.rng.r#gen::<f32>() * 0.2;
            mob.drift = [
                f64::from(angle.cos() * 0.2),
                f64::from(rise),
                f64::from(angle.sin() * 0.2),
            ];
        }
    }
}

/// Swims away from whoever last hurt it, while in water and close enough.
#[derive(Debug, Default)]
struct Flee;

impl Goal for Flee {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.body.in_water
            && mob
                .last_hurt_by_mob
                .and_then(|(id, _)| target::view(cx, id))
                .is_some_and(|t| mob.distance_sq(t.position) < FLEE_DISTANCE_SQ)
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(attacker) = mob
            .last_hurt_by_mob
            .and_then(|(id, _)| target::view(cx, id))
        else {
            return;
        };
        let [x, y, z] = mob.base.position;
        let mut away = [
            x - attacker.position[0],
            y - attacker.position[1],
            z - attacker.position[2],
        ];
        let state = (cx.block)(block_pos([x + away[0], y + away[1], z + away[2]]));
        let water = state.is_some_and(|s| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::fluid::key::WATER,
                s.fluid_state().fluid(),
            )
        });
        let air = state.is_some_and(tuff_world_model::chunk::is_air);
        if !(water || air) {
            return;
        }
        let length = away.iter().map(|c| c * c).sum::<f64>().sqrt();
        if length > 0.0 {
            let mut speed = FLEE_SPEED;
            if length > FLEE_SLOW_FROM {
                speed -= (length - FLEE_SLOW_FROM) / FLEE_SLOW_FROM;
            }
            if speed > 0.0 {
                away = away.map(|c| c * speed);
            }
        }
        if air {
            away[1] = 0.0;
        }
        mob.drift = away.map(|c| c / 20.0);
    }
}

fn common_goals(core: &mut MobCore) {
    core.make_animal();
    core.water_travel = WaterTravel::Drift;
    let mut goals = GoalSelector::default();
    goals.add(0, RandomDrift);
    goals.add(1, Flee);
    core.goals = goals;
}

#[derive(Debug)]
pub struct Squid {
    tentacles: Tentacles,
}

impl Species for Squid {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(rng: &mut StdRng) -> Self {
        Self {
            tentacles: Tentacles::new(rng),
        }
    }

    fn configure(core: &mut MobCore) {
        common_goals(core);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.tentacles.ai_step(core, cx);
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntitySquidAmbient),
            hurt: Some(SoundEvent::EntitySquidHurt),
            death: Some(SoundEvent::EntitySquidDeath),
            step: None,
            volume: 0.4,
        }
    }

    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(squid_air_rules())
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        if damage.attacker.is_some() {
            spawn_ink(
                core,
                ParticleType::SquidInk,
                SoundEvent::EntitySquidSquirt,
                cx,
            );
        }
    }

    fn remove_when_far_away(&self) -> bool {
        true
    }
}

/// The dark-ticks-remaining index a glow squid's own data occupies: it is
/// ageable (baby/age-locked at 16/17, confirmed against the dumped
/// layout), so its own field starts at [`data::SPECIES`] as usual.
const DARK_TICKS: u8 = data::SPECIES;

#[derive(Debug)]
pub struct GlowSquid {
    tentacles: Tentacles,
    dark_ticks: i32,
}

impl Species for GlowSquid {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(rng: &mut StdRng) -> Self {
        Self {
            tentacles: Tentacles::new(rng),
            dark_ticks: 0,
        }
    }

    fn configure(core: &mut MobCore) {
        common_goals(core);
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.dark_ticks = take_number(nbt, "DarkTicksRemaining").map_or(0, |v| v as i32);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("DarkTicksRemaining", self.dark_ticks);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(DARK_TICKS, MetadataValue::VarInt(self.dark_ticks))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityGlowSquidAmbient),
            hurt: Some(SoundEvent::EntityGlowSquidHurt),
            death: Some(SoundEvent::EntityGlowSquidDeath),
            step: None,
            volume: 0.4,
        }
    }

    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(squid_air_rules())
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.tentacles.ai_step(core, cx);
        if self.dark_ticks > 0 {
            self.dark_ticks -= 1;
            core.changed(DARK_TICKS);
        }
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        if damage.attacker.is_some() {
            self.dark_ticks = 100;
            core.changed(DARK_TICKS);
            spawn_ink(
                core,
                ParticleType::GlowSquidInk,
                SoundEvent::EntityGlowSquidSquirt,
                cx,
            );
        }
    }

    fn remove_when_far_away(&self) -> bool {
        true
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Squid, super::load::<Squid>);
    registry.register(EntityType::GlowSquid, super::load::<GlowSquid>);
    registry.register_spawn_rule(EntityType::Squid, super::spawning::surface_water_rule);
    registry.register_spawn_rule(EntityType::GlowSquid, super::spawning::glow_squid_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::world::entity::{EntityBase, EntitySummary, LivingSummary};
    use tuff_world_model::BlockPos;

    /// A tick context like `super::super::tests::with_context`'s, but with
    /// caller-chosen nearby entities: that helper's fields are borrowed for
    /// its own call scope, so a test cannot reassign `cx.entities`
    /// afterwards to something scoped to its own closure body.
    fn with_entities_context(
        entities: &[EntitySummary],
        f: impl FnOnce(&mut MobCore, &mut TickContext),
    ) {
        let shared = tuff_test_support::bare_shared();
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 {
                tuff_vanilla_data::Block::GrassBlock.default_state()
            } else {
                tuff_vanilla_data::Block::Air.default_state()
            })
        };
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(1);
        let mut effects = Vec::new();
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
            entities,
            entity_grid: None,
            pistons: &[],
            parts: &[],
            shared: &shared,
            rng: &mut rng,
            effects: &mut effects,
            path_budget: &mut budget,
            pois: &pois,
            state: &tuff_core::world::region_api::RegionStateLock::default(),
        };
        let base = EntityBase::new(EntityType::Squid, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
        let mut core = MobCore::new(base);
        f(&mut core, &mut cx);
    }

    #[tuff_core::watchdog]
    #[test]
    fn squid_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let layout = EntityType::Squid.data_layout();
        assert_eq!(layout.len(), data::AGE_LOCKED as usize + 1);
        assert_eq!(layout[data::BABY as usize], boolean);
        assert_eq!(layout[data::AGE_LOCKED as usize], boolean);
    }

    #[tuff_core::watchdog]
    #[test]
    fn glow_squid_data_index_matches_the_dumped_layout() {
        let layout = EntityType::GlowSquid.data_layout();
        assert_eq!(layout.len(), DARK_TICKS as usize + 1);
        assert_eq!(
            layout[DARK_TICKS as usize],
            MetadataValue::VarInt(0).type_id() as u8
        );
    }

    fn attacker(id: i32, position: [f64; 3]) -> EntitySummary {
        EntitySummary {
            id,
            uuid: 2,
            kind: EntityType::Zombie,
            position,
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [
                position[0] - 0.3,
                position[1],
                position[2] - 0.3,
                position[0] + 0.3,
                position[1] + 1.8,
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
    fn a_dry_squid_never_flees() {
        let entities = [attacker(7, [5.0, 64.0, 0.5])];
        with_entities_context(&entities, |core, cx| {
            core.last_hurt_by_mob = Some((7, cx.tick));
            let mut flee = Flee;
            assert!(!flee.can_use(core, cx), "not in water");
            core.body.in_water = true;
            assert!(flee.can_use(core, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_squid_flees_away_from_its_attacker() {
        let entities = [attacker(7, [5.5, 64.0, 0.5])];
        with_entities_context(&entities, |core, cx| {
            core.body.in_water = true;
            core.last_hurt_by_mob = Some((7, cx.tick));
            let mut flee = Flee;
            flee.tick(core, cx);
            assert!(
                core.drift[0] < 0.0,
                "flees the opposite way from the attacker"
            );
            assert_eq!(core.drift[1], 0.0, "stays level fleeing into air");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_squid_pushes_off_late_in_each_stroke_and_coasts_between() {
        super::super::tests::with_context(|core, cx| {
            core.body.in_water = true;
            core.drift = [0.2, 0.0, 0.0];
            let mut tentacles = Tentacles {
                movement: 0.0,
                speed: 0.2,
            };
            tentacles.ai_step(core, cx);
            assert_eq!(core.base.velocity, [0.0; 3], "too early in the stroke");
            for _ in 0..12 {
                tentacles.ai_step(core, cx);
            }
            assert_eq!(
                core.base.velocity,
                [0.2, 0.0, 0.0],
                "pushed at the stroke's end"
            );
            while tentacles.movement < PI {
                tentacles.ai_step(core, cx);
            }
            assert!(
                (core.base.velocity[0] - 0.18).abs() < 1.0e-9,
                "coasts once the stroke is past"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stranded_squid_only_falls() {
        super::super::tests::with_context(|core, cx| {
            core.drift = [0.2, 0.0, 0.0];
            core.base.velocity = [0.3, 0.0, 0.3];
            let mut tentacles = Tentacles {
                movement: 3.0,
                speed: 0.1,
            };
            tentacles.ai_step(core, cx);
            assert_eq!(core.base.velocity[0], 0.0);
            assert_eq!(core.base.velocity[2], 0.0);
            assert!((core.base.velocity[1] + 0.08 * AIR_DRAG).abs() < 1.0e-9);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn glow_squid_squirts_ink_and_darkens_when_hurt_by_something() {
        super::super::tests::with_context(|core, cx| {
            let mut squid = GlowSquid::create(cx.rng);
            let damage = Damage {
                attacker: Some((9, EntityType::Player)),
                ..Damage::of(&damage_type::GENERIC)
            };
            squid.after_hurt(core, &damage, cx);
            assert_eq!(squid.dark_ticks, 100);
            assert!(!cx.effects.is_empty());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn glow_squid_dark_ticks_count_down() {
        super::super::tests::with_context(|core, cx| {
            let mut squid = GlowSquid::create(cx.rng);
            squid.dark_ticks = 2;
            squid.ai_step(core, cx);
            assert_eq!(squid.dark_ticks, 1);
            squid.ai_step(core, cx);
            assert_eq!(squid.dark_ticks, 0);
            squid.ai_step(core, cx);
            assert_eq!(squid.dark_ticks, 0);
        });
    }
}
