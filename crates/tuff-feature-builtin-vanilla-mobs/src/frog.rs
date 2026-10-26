//! Frogs: a biome-picked variant (data-driven, like cows/wolves/cats),
//! eating small slimes and magma cubes, and breeding on slimeballs.
//! Breeding leaves one parent pregnant: it walks to land beside water and
//! lays frogspawn, which hatches into tadpoles (see `tadpole`). A frog
//! walks up to a size-1 slime or magma cube, shoots its tongue in the
//! UsingTongue pose and eats it.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, Item, tag, variant};

use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{
    Breed, Float, FollowParent, LookAtPlayer, MoveToBlock, Panic, RandomLookAround, RandomStroll,
    Tempt,
};
use super::long_jump::{LongJump, LongJumpSpec, Preference, default_landing};
use super::navigation::NavKind;
use super::path::static_path_type;
use super::physics::block_pos;
use super::target::NearestAttackableEntity;
use super::{Mob, MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{Effect, RegionEntity, TickContext, take, take_bool};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::entity::PathType;
use tuff_world_model::BlockPos;

const TONGUE_TARGET: u8 = data::SPECIES + 1;

const FOOD_KINDS: [EntityType; 2] = [EntityType::Slime, EntityType::MagmaCube];

const FOOD_RANGE: f64 = 10.0;

fn small_cube(e: &tuff_core::world::entity::EntitySummary) -> bool {
    e.bounds[3] - e.bounds[0] < 0.6
}

fn frog_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::FROG_FOOD, item)
}

const SIGNAL_PREGNANT: i32 = 1;
const HORIZONTAL: [(i32, i32); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];

fn pregnant(mob: &MobCore) -> bool {
    mob.signal & SIGNAL_PREGNANT != 0
}

fn supports_spawn(state: BlockState) -> bool {
    tuff_datapack::tags::is(
        tag::fluid::key::SUPPORTS_FROGSPAWN,
        state.fluid_state().fluid(),
    ) || tuff_datapack::tags::is(tag::block::key::SUPPORTS_FROGSPAWN, state.block())
}

fn spawn_site(pos: BlockPos, cx: &TickContext) -> Option<BlockPos> {
    HORIZONTAL.iter().find_map(|&(dx, dz)| {
        let at = BlockPos::new(pos.x + dx, pos.y, pos.z + dz);
        let state = (cx.block)(at)?;
        let above = BlockPos::new(at.x, at.y + 1, at.z);
        (state.collision_boxes().is_empty()
            && supports_spawn(state)
            && (cx.block)(above).is_some_and(tuff_world_model::chunk::is_air))
        .then_some(above)
    })
}

fn shore(_mob: &MobCore, pos: BlockPos, cx: &TickContext) -> bool {
    (cx.block)(pos).is_some_and(|state| !state.collision_boxes().is_empty())
        && spawn_site(pos, cx).is_some()
}

/// A pregnant frog walks to land beside water and lays frogspawn there.
#[derive(Debug)]
struct LayFrogspawn {
    seek: MoveToBlock,
}

impl LayFrogspawn {
    fn new() -> Self {
        Self {
            seek: MoveToBlock::new(1.0, 8, 1, shore),
        }
    }
}

impl Goal for LayFrogspawn {
    fn flags(&self) -> Flags {
        self.seek.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        pregnant(mob) && mob.target.is_none() && self.seek.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        pregnant(mob) && self.seek.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.seek.start(mob, cx);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.seek.tick(mob, cx);
        if mob.body.in_water || !mob.base.on_ground {
            return;
        }
        let here = block_pos(mob.base.position);
        let below = BlockPos::new(here.x, here.y - 1, here.z);
        let Some(site) = spawn_site(below, cx) else {
            return;
        };
        cx.effects.push(Effect::SetBlock {
            pos: site,
            state: Block::Frogspawn.default_state(),
        });
        mob.sound_as(
            SoundEvent::EntityFrogLaySpawn,
            play::sound_category::BLOCKS,
            1.0,
            1.0,
            cx,
        );
        mob.signal &= !SIGNAL_PREGNANT;
    }
}

const LAND_SEARCH_COOLDOWN: u64 = 60;

fn in_water_block(pos: BlockPos, cx: &TickContext) -> bool {
    (cx.block)(pos).is_some_and(|state| {
        tuff_datapack::tags::is(tag::fluid::key::WATER, state.fluid_state().fluid())
    })
}

fn stands_on(pos: BlockPos, cx: &TickContext) -> bool {
    let Some(state) = (cx.block)(pos) else {
        return false;
    };
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    state.fluid_state().is_empty()
        && state.collision_boxes().is_empty()
        && (cx.block)(below).is_some_and(|s| {
            s.is_face_sturdy(
                tuff_vanilla_data::block::Facing::Up,
                tuff_vanilla_data::block::Support::Full,
            )
        })
}

/// A frog swimming with nothing to do heads for the nearest dry spot to
/// stand on, looking again every three seconds.
#[derive(Debug, Default)]
struct TryFindLand {
    ready_at: u64,
}

impl TryFindLand {
    fn find(here: BlockPos, range: i32, cx: &TickContext) -> Option<BlockPos> {
        for distance in 1..=range {
            for dx in -distance..=distance {
                for dy in -(distance - dx.abs())..=(distance - dx.abs()) {
                    let dz = distance - dx.abs() - dy.abs();
                    for dz in if dz == 0 { vec![0] } else { vec![-dz, dz] } {
                        if dx == 0 && dz == 0 {
                            continue;
                        }
                        let pos = BlockPos::new(here.x + dx, here.y + dy, here.z + dz);
                        if stands_on(pos, cx) {
                            return Some(pos);
                        }
                    }
                }
            }
        }
        None
    }
}

impl Goal for TryFindLand {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.target.is_some()
            || !mob.navigation.is_done()
            || cx.tick < self.ready_at
            || !in_water_block(block_pos(mob.base.position), cx)
        {
            return false;
        }
        self.ready_at = cx.tick + LAND_SEARCH_COOLDOWN;
        let (range, speed) = if mob.body.in_water {
            (8, 1.5)
        } else {
            (6, 1.0)
        };
        let Some(pos) = Self::find(block_pos(mob.base.position), range, cx) else {
            return false;
        };
        let target = [
            f64::from(pos.x) + 0.5,
            f64::from(pos.y),
            f64::from(pos.z) + 0.5,
        ];
        mob.move_to_position(target, speed, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_none() && !mob.navigation.is_done()
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

const EATING_DISTANCE: f64 = 1.75;
const EATING_PULL: f64 = 0.75;
const TONGUE_SPEED: f64 = 2.0;
const CATCH_TICKS: i32 = 6;
const EAT_TICKS: i32 = 10;
const REPATH_TICKS: i32 = 10;
const RETHINK_TICKS: u64 = 100;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum TonguePhase {
    #[default]
    Done,
    Approach,
    Catch,
    Eat,
}

/// Walks up to a small cube, shoots the tongue out and eats it.
#[derive(Debug, Default)]
struct ShootTongue {
    phase: TonguePhase,
    timer: i32,
    until_repath: i32,
    next_try: u64,
}

impl Goal for ShootTongue {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if cx.tick < self.next_try || mob.croaking {
            return false;
        }
        let Some(target) = mob.target_view(cx).filter(|t| t.alive) else {
            return false;
        };
        let reachable = mob.distance_sq(target.position) < EATING_DISTANCE * EATING_DISTANCE
            || mob
                .create_path(block_pos(target.position), 0, true, cx)
                .is_some_and(|path| f64::from(path.distance_to_target) < EATING_DISTANCE);
        if !reachable {
            mob.unreachable_until.retain(|&(_, until)| until > cx.tick);
            mob.unreachable_until
                .push((target.id, cx.tick + RETHINK_TICKS));
            mob.target = None;
            self.next_try = cx.tick + RETHINK_TICKS;
        }
        reachable
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some() && self.phase != TonguePhase::Done && !mob.is_panicking()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        mob.set_tongue_target(Some(target.id));
        mob.move_to_entity_reach(target.position, 0, TONGUE_SPEED, cx);
        self.until_repath = REPATH_TICKS;
        self.phase = TonguePhase::Approach;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = None;
        mob.set_tongue_target(None);
        mob.set_pose(None);
        mob.navigation.stop();
        self.phase = TonguePhase::Done;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.set_tongue_target(Some(target.id));
        match self.phase {
            TonguePhase::Approach => {
                if mob.distance_sq(target.position) < EATING_DISTANCE * EATING_DISTANCE {
                    mob.sound_as(
                        SoundEvent::EntityFrogTongue,
                        play::sound_category::NEUTRAL,
                        2.0,
                        1.0,
                        cx,
                    );
                    mob.set_pose(Some(tuff_vanilla_data::entity::Pose::UsingTongue));
                    let from = target.position;
                    let to = mob.base.position;
                    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
                    let length = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2])
                        .sqrt()
                        .max(1.0e-7);
                    cx.effects.push(Effect::Push {
                        target: target.id,
                        push: delta.map(|d| d / length * EATING_PULL),
                    });
                    self.timer = 0;
                    self.phase = TonguePhase::Catch;
                } else if self.until_repath <= 0 {
                    mob.move_to_entity_reach(target.position, 0, TONGUE_SPEED, cx);
                    self.until_repath = REPATH_TICKS;
                } else {
                    self.until_repath -= 1;
                }
            }
            TonguePhase::Catch => {
                let elapsed = self.timer;
                self.timer += 1;
                if elapsed >= CATCH_TICKS {
                    self.phase = TonguePhase::Eat;
                    mob.sound_as(
                        SoundEvent::EntityFrogEat,
                        play::sound_category::NEUTRAL,
                        2.0,
                        1.0,
                        cx,
                    );
                    if target.alive {
                        mob.hurt_target(&target, cx);
                    }
                }
            }
            TonguePhase::Eat => {
                if self.timer >= EAT_TICKS {
                    self.phase = TonguePhase::Done;
                } else {
                    self.timer += 1;
                }
            }
            TonguePhase::Done => {}
        }
    }
}

const CROAK_TICKS: i32 = 60;
const CROAK_CHANCE: f32 = 0.03;

/// Now and then stands still and croaks for a few seconds.
#[derive(Debug, Default)]
struct Croak {
    ticks: i32,
}

impl Goal for Croak {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        !mob.body.in_water
            && !mob.swimming
            && !mob.sleeping
            && mob.base.on_ground
            && mob.navigation.is_done()
            && cx.rng.r#gen::<f32>() < CROAK_CHANCE
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        self.ticks < CROAK_TICKS && !mob.body.in_water
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.ticks = 0;
        mob.set_croaking(true);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.set_croaking(false);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.ticks += 1;
    }
}

fn frog_landing(mob: &MobCore, target: BlockPos, cx: &mut TickContext) -> bool {
    let below = BlockPos::new(target.x, target.y - 1, target.z);
    let above = BlockPos::new(target.x, target.y + 1, target.z);
    let wet = |pos| (cx.block)(pos).is_some_and(|s| !s.fluid_state().is_empty());
    if wet(target) || wet(below) || wet(above) {
        return false;
    }
    let preferred = |pos| {
        (cx.block)(pos).is_some_and(|s| {
            tuff_datapack::tags::is(tag::block::key::FROG_PREFER_JUMP_TO, s.block())
        })
    };
    if preferred(target) || preferred(below) {
        return true;
    }
    let kind = static_path_type(target, cx.block, cx.min_y);
    let kind_below = static_path_type(below, cx.block, cx.min_y);
    if kind == PathType::Trapdoor {
        return true;
    }
    if (cx.block)(target).is_some_and(tuff_world_model::chunk::is_air)
        && kind_below == PathType::Trapdoor
    {
        return true;
    }
    default_landing(mob, target, cx)
}

#[derive(Debug)]
pub struct Frog {
    variant: usize,
}

impl Species for Frog {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self {
            variant: variant::index(crate::pack_variants::frog(), "minecraft:temperate")
                .unwrap_or(0),
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.navigation.kind = NavKind::Amphibious;
        core.navigation.frog_nodes = true;
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(
            1,
            NearestAttackableEntity::new(&FOOD_KINDS, true)
                .only(small_cube)
                .filtered(|mob, t, cx| {
                    !mob.unreachable_until
                        .iter()
                        .any(|&(id, until)| id == t.id && until > cx.tick)
                })
                .searching(|_| [FOOD_RANGE; 3]),
        );
        goals.add(2, ShootTongue::default());
        goals.add(3, Panic::new(2.0));
        goals.add(3, TryFindLand::default());
        goals.add(4, LayFrogspawn::new());
        goals.add(4, Breed::new(1.0).brain().unless(pregnant));
        goals.add(5, Tempt::new(1.0, frog_food));
        goals.add(6, FollowParent::new(1.1));
        goals.add(
            6,
            LongJump::new(LongJumpSpec {
                cooldown: (100, 140),
                height: 2,
                width: 4,
                jump_sound: SoundEvent::EntityFrogLongJump,
                screaming_jump_sound: SoundEvent::EntityFrogLongJump,
                landing_sound: SoundEvent::EntityFrogStep,
                dimension_scale: 1.0,
                preference: Some(Preference {
                    block: |state| {
                        tuff_datapack::tags::is(tag::block::key::FROG_PREFER_JUMP_TO, state.block())
                    },
                    chance: 0.5,
                }),
                landing: frog_landing,
                blocked: pregnant,
            }),
        );
        goals.add(7, Croak::default());
        goals.add(7, RandomStroll::with_interval(1.0, RandomStroll::INTERVAL));
        goals.add(8, LookAtPlayer::new(6.0));
        goals.add(9, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        if take_bool(nbt, "IsPregnant").unwrap_or(false) {
            core.signal |= SIGNAL_PREGNANT;
        }
        if let Some(index) = take(nbt, "variant", |t| {
            variant::index(crate::pack_variants::frog(), t.as_str()?)
        }) {
            self.variant = index;
        }
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        out.insert("variant", crate::pack_variants::frog()[self.variant].name);
        out.insert("IsPregnant", pregnant(core));
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                data::SPECIES,
                MetadataValue::Holder {
                    serializer: core.base.kind.data_serializer(data::SPECIES).unwrap_or(0),
                    id: self.variant as i32,
                },
            ),
            (
                TONGUE_TARGET,
                MetadataValue::OptionalVarInt(core.tongue_target),
            ),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityFrogAmbient),
            hurt: Some(SoundEvent::EntityFrogHurt),
            death: Some(SoundEvent::EntityFrogDeath),
            step: Some(SoundEvent::EntityFrogStep),
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntityFrogEat)
    }

    fn is_food<M: tuff_item::item::Mode>(&self, stack: &ItemStack<M>) -> bool {
        frog_food(stack.item())
    }

    fn breed(
        mob: &mut Mob<Self>,
        partner: &mut Mob<Self>,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        if !super::animal::ready_to_breed(&mob.core)
            || !super::animal::ready_to_breed(&partner.core)
        {
            return None;
        }
        let cause = [&mob.core, &partner.core]
            .into_iter()
            .find_map(|parent| parent.love.as_ref().and_then(|love| love.cause));
        if let Some(player) = cause {
            cx.effects.push(Effect::Advancement {
                player,
                event: Box::new(tuff_core::world::messages::AdvancementEvent::BredAnimals {
                    parent: mob.core.facts(),
                    partner: partner.core.facts(),
                    child: None,
                }),
            });
        }
        mob.core.signal |= SIGNAL_PREGNANT;
        for parent in [&mut mob.core, &mut partner.core] {
            parent.set_age(super::animal::PARENT_AGE_AFTER_BREEDING);
            if let Some(love) = &mut parent.love {
                love.reset();
            }
        }
        mob.core.entity_event(super::event::IN_LOVE, cx);
        if cx.shared.game_rules.borrow().bool("mob_drops") {
            cx.effects.push(Effect::Experience {
                position: mob.core.base.position,
                amount: cx.rng.gen_range(1..=7),
            });
        }
        None
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        Self {
            variant: if rng.r#gen() {
                self.variant
            } else {
                partner.variant
            },
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let pos = super::physics::block_pos(core.base.position);
        let biome = (cx.biome)(pos).unwrap_or("minecraft:plains");
        let candidates = variant::candidates(crate::pack_variants::frog(), biome);
        if !candidates.is_empty() {
            self.variant = candidates[cx.rng.gen_range(0..candidates.len())];
        }
    }

    fn loot_facts(&self, facts: &mut tuff_core::world::drops::EntityFacts) {
        facts.components.push((
            "minecraft:frog/variant",
            crate::pack_variants::frog()[self.variant].name.to_owned(),
        ));
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self, _core: &MobCore) -> bool {
        false
    }
}

/// Registers the frog's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Frog, super::load::<Frog>);
    registry.register_spawn_rule(EntityType::Frog, super::spawning::animal_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_world;
    use tuff_core::protocol::play::metadata::POSE;

    fn grass(pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        Some(if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_frog_croaks_for_sixty_ticks_then_stands_again() {
        with_world(&grass, &[], |core, cx| {
            Frog::configure(core);
            core.base.on_ground = true;
            let mut goal = Croak::default();
            let mut started = false;
            for _ in 0..2000 {
                if goal.can_use(core, cx) {
                    started = true;
                    break;
                }
            }
            assert!(started);
            goal.start(core, cx);
            assert!(core.changed.contains(&POSE));
            assert_eq!(
                core.core_metadata(POSE),
                Some(MetadataValue::Pose(i32::from(
                    tuff_vanilla_data::entity::Pose::Croaking.id()
                )))
            );
            for _ in 0..CROAK_TICKS {
                assert!(goal.can_continue(core, cx));
                goal.tick(core, cx);
            }
            assert!(!goal.can_continue(core, cx));
            goal.stop(core, cx);
            assert_eq!(
                core.core_metadata(POSE),
                Some(MetadataValue::Pose(i32::from(
                    tuff_vanilla_data::entity::Pose::Standing.id()
                )))
            );
        });
    }

    fn lake(pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        Some(if pos.y < 62 {
            Block::GrassBlock.default_state()
        } else if pos.y < 64 && (pos.x.abs() <= 2 && pos.z.abs() <= 2) {
            Block::Water.default_state()
        } else if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_swimming_frog_finds_the_nearest_dry_standing_spot() {
        with_world(&lake, &[], |core, cx| {
            Frog::configure(core);
            core.base.position = [0.5, 62.0, 0.5];
            let here = block_pos(core.base.position);
            let found = TryFindLand::find(here, 8, cx).expect("shore within reach");
            assert!(found.x.abs() >= 3 || found.z.abs() >= 3 || found.y >= 64);
            assert!(stands_on(found, cx));
            assert!(TryFindLand::find(here, 1, cx).is_none());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_the_smallest_cubes_are_prey() {
        let mut e = tuff_core::world::entity::EntitySummary {
            id: 1,
            uuid: 1,
            kind: EntityType::Slime,
            position: [0.0; 3],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [-0.26, 0.0, -0.26, 0.26, 0.52, 0.26],
            living: None,
            pickable: true,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        };
        assert!(small_cube(&e));
        e.bounds = [-0.52, 0.0, -0.52, 0.52, 1.04, 0.52];
        assert!(!small_cube(&e));
    }

    fn slime(id: i32, x: f64) -> tuff_core::world::entity::EntitySummary {
        tuff_core::world::entity::EntitySummary {
            id,
            uuid: id as u128,
            kind: EntityType::Slime,
            position: [x, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [x - 0.26, 64.0, 0.24, x + 0.26, 64.52, 0.76],
            living: Some(tuff_core::world::entity::LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 0.3,
                in_water: false,
                seen_as_enemy: true,
                enemy: false,
                health: 1.0,
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
    fn a_frog_shoots_its_tongue_then_eats_a_cube_in_reach() {
        let prey = [slime(7, 1.5)];
        with_world(&grass, &prey, |core, cx| {
            Frog::configure(core);
            core.target = Some(7);
            let mut goal = ShootTongue::default();
            assert!(goal.can_use(core, cx));
            goal.start(core, cx);
            assert_eq!(core.tongue_target, Some(7));
            assert!(core.changed.contains(&TONGUE_TARGET));
            goal.tick(core, cx);
            assert_eq!(
                core.pose,
                Some(tuff_vanilla_data::entity::Pose::UsingTongue)
            );
            let hurts = |cx: &TickContext| {
                cx.effects
                    .iter()
                    .filter(|e| matches!(e, Effect::Hurt { .. }))
                    .count()
            };
            for _ in 0..CATCH_TICKS {
                goal.tick(core, cx);
                assert_eq!(hurts(cx), 0);
            }
            goal.tick(core, cx);
            assert_eq!(hurts(cx), 1);
            let mut ticks = 0;
            while goal.can_continue(core, cx) {
                goal.tick(core, cx);
                ticks += 1;
                assert!(ticks <= EAT_TICKS);
            }
            assert!(!goal.can_continue(core, cx));
            goal.stop(core, cx);
            assert_eq!(core.tongue_target, None);
            assert_eq!(core.pose, None);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_frog_gives_up_on_a_cube_it_cannot_reach() {
        let prey = [slime(7, 40.5)];
        with_world(&lake, &prey, |core, cx| {
            Frog::configure(core);
            core.target = Some(7);
            let mut goal = ShootTongue::default();
            assert!(!goal.can_use(core, cx));
            assert_eq!(core.target, None);
        });
    }
}
