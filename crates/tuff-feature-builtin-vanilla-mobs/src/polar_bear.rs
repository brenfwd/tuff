//! Polar bears: neutral land animals that never breed by feeding (babies
//! only come from a natural spawn group, verified against the vanilla
//! reference), fight back once hurt, and protect nearby cubs by attacking
//! any player while one is close.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::attack::MeleeAttack;
use super::goal::GoalSelector;
use super::goals::{FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll};
use super::physics;
use super::target::{HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer};
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{NearbyPlayer, TickContext};
use tuff_world_model::BlockPos;

const STANDING: u8 = data::SPECIES;
const CUB_RANGE: [f64; 3] = [8.0, 4.0, 8.0];
const ANGER_FOLLOW_INTERVAL: i32 = 10;
const PROTECTIVE_INTERVAL: i32 = 20;

fn animal_experience(rng: &mut StdRng) -> i32 {
    1 + rng.gen_range(0..3)
}

fn is_adult(core: &MobCore, _cx: &TickContext) -> bool {
    !core.is_baby()
}

/// Whether an adult of its kind with a nearby cub should be on alert:
/// mirrors vanilla's protective-mother trigger (any non-baby polar bear
/// within an 8x4x8 box around it that is itself a cub).
fn has_nearby_cub(mob: &MobCore, cx: &TickContext) -> bool {
    if mob.is_baby() {
        return false;
    }
    let area = physics::inflate(mob.bounds(), CUB_RANGE);
    cx.entities.iter().any(|e| {
        e.kind == EntityType::PolarBear
            && e.living.is_some_and(|l| l.alive && l.age < 0)
            && physics::intersects(e.bounds, area)
    })
}

fn grudge(mob: &MobCore, player: &NearbyPlayer) -> bool {
    mob.angry_at_player(player.uuid)
}

#[derive(Debug, Default)]
pub struct PolarBear {
    standing: bool,
}

impl Species for PolarBear {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        goals.add(0, super::goals::Float::new(core));
        goals.add(1, MeleeAttack::new(1.25, true));
        goals.add(1, Panic::new(2.0));
        goals.add(4, FollowParent::new(1.25));
        goals.add(5, RandomStroll::water_avoiding(1.0));
        goals.add(6, LookAtPlayer::new(6.0));
        goals.add(7, RandomLookAround::default());
        goals.add(1, HurtByTarget::new(&[]).alerting(&[EntityType::PolarBear]));
        goals.add(5, super::target::ResetUniversalAnger::new(false));
        goals.add(
            2,
            NearestAttackablePlayer::with_interval(PROTECTIVE_INTERVAL, true, None)
                .when(has_nearby_cub),
        );
        goals.add(
            3,
            NearestAttackablePlayer::with_interval(ANGER_FOLLOW_INTERVAL, true, Some(grudge)),
        );
        goals.add(
            4,
            NearestAttackableEntity::new(&[EntityType::Fox], true).when(is_adult),
        );
        core.goals = goals;
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(STANDING, MetadataValue::Boolean(self.standing))]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(if core.is_baby() {
                SoundEvent::EntityPolarBearAmbientBaby
            } else {
                SoundEvent::EntityPolarBearAmbient
            }),
            hurt: Some(SoundEvent::EntityPolarBearHurt),
            death: Some(SoundEvent::EntityPolarBearDeath),
            step: Some(SoundEvent::EntityPolarBearStep),
            volume: 1.0,
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.update_anger(true, cx);
        let standing = core.aggressive;
        if standing != self.standing {
            if standing {
                core.sound(SoundEvent::EntityPolarBearWarning, 1.0, 1.0, cx);
            }
            self.standing = standing;
            core.changed(STANDING);
        }
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }
}

fn spawn_rule(
    kind: EntityType,
    level: &super::spawning::SpawnLevel,
    pos: BlockPos,
    reason: super::spawning::SpawnReason,
    rng: &mut StdRng,
) -> bool {
    let alternate = (level.biome)(pos).is_some_and(|biome| {
        tuff_datapack::tags::has(
            tuff_vanilla_data::tag::worldgen::biome::key::POLAR_BEARS_SPAWN_ON_ALTERNATE_BLOCKS,
            biome,
        )
    });
    if alternate {
        return level.raw_brightness(pos, 0) > 8
            && (level.block)(BlockPos::new(pos.x, pos.y - 1, pos.z)).is_some_and(|state| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::POLAR_BEARS_SPAWNABLE_ON_ALTERNATE,
                    state.block(),
                )
            });
    }
    super::spawning::animal_rule(kind, level, pos, reason, rng)
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::PolarBear, super::load::<PolarBear>);
    registry.register_spawn_rule(EntityType::PolarBear, spawn_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::world::entity::{EntityBase, EntitySummary, LivingSummary, NearbyPlayer};

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
        let base = EntityBase::new(EntityType::PolarBear, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
        let mut core = MobCore::new(base);
        f(&mut core, &mut cx);
    }

    #[tuff_core::watchdog]
    #[test]
    fn polar_bear_data_index_matches_the_dumped_layout() {
        let layout = EntityType::PolarBear.data_layout();
        assert_eq!(layout.len(), STANDING as usize + 1);
        assert_eq!(
            layout[STANDING as usize],
            MetadataValue::Boolean(false).type_id() as u8
        );
    }

    fn cub(id: i32, position: [f64; 3]) -> EntitySummary {
        EntitySummary {
            id,
            uuid: 2,
            kind: EntityType::PolarBear,
            position,
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [
                position[0] - 0.3,
                position[1],
                position[2] - 0.3,
                position[0] + 0.3,
                position[1] + 0.7,
                position[2] + 0.3,
            ],
            living: Some(LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: -100,
                in_love: false,
                panicking: false,
                eye_height: 0.5,
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
    fn a_bear_with_a_nearby_cub_turns_protective() {
        let cubs = [cub(9, [1.5, 64.0, 0.5])];
        with_entities_context(&cubs, |core, cx| {
            PolarBear::configure(core);
            assert!(has_nearby_cub(core, cx));
        });
        with_entities_context(&[], |core, cx| {
            PolarBear::configure(core);
            assert!(!has_nearby_cub(core, cx));
        });
    }

    fn fox(id: i32, position: [f64; 3]) -> EntitySummary {
        EntitySummary {
            kind: EntityType::Fox,
            ..cub(id, position)
        }
    }

    fn picks_fox(baby: bool) -> bool {
        let foxes = [fox(9, [3.5, 64.0, 0.5])];
        let mut picked = false;
        with_entities_context(&foxes, |core, cx| {
            PolarBear::configure(core);
            core.set_baby(baby);
            let mut goal = NearestAttackableEntity::new(&[EntityType::Fox], true).when(is_adult);
            picked = (0..400).any(|_| {
                use crate::goal::Goal;
                goal.can_use(core, cx)
            });
        });
        picked
    }

    #[tuff_core::watchdog]
    #[test]
    fn adult_bears_hunt_foxes_and_cubs_do_not() {
        assert!(picks_fox(false));
        assert!(!picks_fox(true));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cub_itself_is_never_protective() {
        let cubs = [cub(9, [0.5, 64.0, 0.5])];
        with_entities_context(&cubs, |core, cx| {
            PolarBear::configure(core);
            core.set_baby(true);
            assert!(!has_nearby_cub(core, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn grudge_only_targets_the_bear_is_angry_at() {
        let mut core = {
            let base = tuff_core::world::entity::EntityBase::new(
                EntityType::PolarBear,
                1,
                1,
                [0.0; 3],
                [0.0; 3],
            );
            MobCore::new(base)
        };
        let angry_at = NearbyPlayer {
            id: 1,
            uuid: 42,
            name: "",
            position: [0.0; 3],
            eye_height: 1.62,
            yaw: 0.0,
            pitch: 0.0,
            held: [None, None],
            creative: false,
            height: 1.8,
            sneaking: false,
            invisible: false,
            invulnerable: false,
            armor: [None, None, None, None],
            effects: 0,
            movement: [0.0; 3],
            vehicle: None,
            sleeping: None,
            last_hurt_by: None,
            last_hurt_mob: None,
            perchable: false,
            sprinting: false,
        };
        let stranger = NearbyPlayer {
            uuid: 7,
            ..angry_at
        };
        assert!(!grudge(&core, &angry_at));
        core.anger = Some(crate::target::Anger {
            target: 42,
            ticks: 400,
            universal: false,
        });
        assert!(grudge(&core, &angry_at));
        assert!(!grudge(&core, &stranger));
    }

    #[tuff_core::watchdog]
    #[test]
    fn standing_follows_the_melee_attack_flag() {
        super::super::tests::with_context(|core, cx| {
            PolarBear::configure(core);
            let mut bear = PolarBear::default();
            assert!(!bear.standing);
            core.set_aggressive(true);
            bear.ai_step(core, cx);
            assert!(bear.standing);
            core.set_aggressive(false);
            bear.ai_step(core, cx);
            assert!(!bear.standing);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn spawns_follow_animal_rules_outside_alternate_biomes() {
        use rand::SeedableRng;
        let mut rng = StdRng::seed_from_u64(1);
        let overworld =
            tuff_vanilla_data::dimension_type::DimensionType::get("minecraft:overworld").unwrap();
        let mut check = |ground: tuff_vanilla_data::Block, sky: u8, biome: &'static str| {
            let block = move |p: BlockPos| {
                Some(if p.y == 63 {
                    ground.default_state()
                } else {
                    tuff_vanilla_data::Block::Air.default_state()
                })
            };
            let light = move |_: BlockPos| Some((sky, 0));
            let level = super::super::spawning::SpawnLevel {
                block: &block,
                light: &light,
                sky_darken: 0,
                min_y: -64,
                sea_level: 63,
                dimension: overworld,
                thundering: false,
                biome: &move |_| Some(biome),
                seed: 0,
                surface_slime_chance: 0.0,
                difficulty: tuff_core::world::player::Difficulty::Normal,
                player_within: &|_, _| false,
            };
            spawn_rule(
                EntityType::PolarBear,
                &level,
                BlockPos::new(0, 64, 0),
                super::super::spawning::SpawnReason::Natural,
                &mut rng,
            )
        };
        assert!(check(
            tuff_vanilla_data::Block::GrassBlock,
            15,
            "minecraft:plains"
        ));
        assert!(!check(
            tuff_vanilla_data::Block::GrassBlock,
            4,
            "minecraft:plains"
        ));
        assert!(!check(
            tuff_vanilla_data::Block::Stone,
            15,
            "minecraft:plains"
        ));
    }
}
