//! Data pack predicates as commands test them (`predicate=` selector
//! options and `execute if|unless predicate`). What a predicate can see of
//! a player is gathered from its session or its published form; scores and
//! teams come from the scoreboard first. The predicate is then evaluated on
//! the region around the tested position, which reads its own entities,
//! blocks, biomes and light directly, so nothing waits on another session.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::Value;
use tokio::sync::oneshot;
use tuff_vanilla_data::registry::EntityType;

use crate::command::selector::{Candidate, Selector, SelectorSource};
use crate::world::Shared;
use crate::world::dimension::DimensionId;
use crate::world::messages::{PlayerSnapshot, RegionInternal};
use crate::world::predicate::{
    self, Context as PredicateContext, EntityFacts, LocationFacts, PlayerFacts, PredicateWorld,
};
use crate::world::region_api::{RegionTask, RegionWorld};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use super::Context;

/// What a predicate is tested on: the `this` entity (if any) and the
/// position and dimension it is tested at.
#[derive(Clone, Debug)]
pub struct Subject {
    pub dimension: DimensionId,
    pub origin: [f64; 3],
    pub this: Option<This>,
    pub target: Option<This>,
    /// Whether the block at the origin is a loot parameter.
    pub at_block: bool,
}

#[derive(Clone, Debug)]
pub enum This {
    /// Facts gathered already (players).
    Facts(Box<EntityFacts>),
    /// An entity the region at the origin reads itself; `known` stands in
    /// when it is not there.
    Entity { id: i32, known: Box<EntityFacts> },
}

/// What the predicates being tested read, so only that is gathered.
#[derive(Debug, Default)]
struct Needs {
    objectives: BTreeSet<String>,
    fixed_holders: BTreeSet<String>,
    teams: bool,
    saved_form: bool,
    block_entities: bool,
    structures: bool,
    looking_at: bool,
    storages: BTreeSet<String>,
}

impl Needs {
    fn of(conditions: &[Arc<Value>]) -> Self {
        let mut needs = Self::default();
        for condition in conditions {
            needs.walk(condition);
        }
        needs
    }

    fn walk(&mut self, value: &Value) {
        match value {
            Value::Array(items) => items.iter().for_each(|item| self.walk(item)),
            Value::Object(fields) => {
                let kind = fields.get("type").and_then(Value::as_str);
                if kind == Some("minecraft:entity_scores")
                    && let Some(scores) = fields.get("scores").and_then(Value::as_object)
                {
                    self.objectives.extend(scores.keys().cloned());
                }
                if matches!(kind, Some("storage" | "minecraft:storage"))
                    && let Some(id) = fields.get("storage").and_then(Value::as_str)
                {
                    self.storages
                        .insert(tuff_datapack::identifier(id).unwrap_or_default());
                }
                if let Some(score) = fields.get("score").and_then(Value::as_str) {
                    self.objectives.insert(score.to_owned());
                }
                if matches!(kind, Some("fixed" | "minecraft:fixed"))
                    && let Some(name) = fields.get("name").and_then(Value::as_str)
                {
                    self.fixed_holders.insert(name.to_owned());
                }
                for (key, child) in fields {
                    match key.as_str() {
                        "team" | "minecraft:team" => self.teams = true,
                        "structures" => self.structures = true,
                        "looking_at" => self.looking_at = true,
                        "slots"
                        | "minecraft:slots"
                        | "predicates"
                        | "minecraft:predicates"
                        | "food"
                        | "recipes"
                        | "type_specific/sheep"
                        | "type_specific/cube_mob"
                        | "type_specific/raider"
                        | "minecraft:type_specific/sheep"
                        | "minecraft:type_specific/cube_mob"
                        | "minecraft:type_specific/raider" => self.saved_form = true,
                        "nbt" | "minecraft:nbt" if child.is_string() => {
                            self.saved_form = true;
                            self.block_entities = true;
                        }
                        "effects" | "minecraft:effects" => self.saved_form = true,
                        _ => {}
                    }
                    self.walk(child);
                }
            }
            _ => {}
        }
    }
}

/// Scores and teams read from the scoreboard before evaluation.
#[derive(Debug, Default)]
struct Board {
    scores: HashMap<(String, String), i32>,
    teams: HashMap<String, String>,
    storages: HashMap<String, Compound>,
}

async fn read_board(ctx: &Context<'_>, needs: &Needs, holders: Vec<String>) -> Board {
    let mut storages = HashMap::new();
    for id in &needs.storages {
        storages.insert(id.clone(), ctx.shared().command_storage.get(id).await);
    }
    if needs.objectives.is_empty() && !needs.teams {
        return Board {
            storages,
            ..Board::default()
        };
    }
    let mut holders = holders;
    holders.extend(needs.fixed_holders.iter().cloned());
    let objectives: Vec<String> = needs.objectives.iter().cloned().collect();
    let teams = needs.teams;
    ctx.shared()
        .scoreboard
        .read(move |board| {
            let mut found = Board::default();
            for holder in &holders {
                for objective in &objectives {
                    if let Some(score) = board.score(holder, objective) {
                        found
                            .scores
                            .insert((holder.clone(), objective.clone()), score.value);
                    }
                }
                if teams && let Some(team) = board.team_of(holder) {
                    found.teams.insert(holder.clone(), team.name.clone());
                }
            }
            found
        })
        .await
        .map(|board| Board { storages, ..board })
        .unwrap_or_default()
}

/// The world as a predicate reads it on a region: blocks and light the
/// region owns, the shared weather and clocks, the gathered scoreboard.
struct RegionView<'a> {
    region: Option<&'a dyn RegionWorld>,
    shared: &'a Shared,
    dimension: DimensionId,
    sky_darken: u8,
    board: &'a Board,
    block_entities: bool,
    structures: bool,
    rng: RefCell<StdRng>,
}

/// Runs `f` with `region` read as a predicate world, with no scoreboard.
pub(crate) fn with_region_view<R>(
    region: &dyn RegionWorld,
    shared: &Shared,
    dimension: DimensionId,
    seed: u64,
    f: impl FnOnce(&dyn PredicateWorld) -> R,
) -> R {
    let board = Board::default();
    let view = RegionView {
        region: Some(region),
        shared,
        dimension,
        sky_darken: crate::world::sky_darken(shared, shared.dimension(dimension)),
        board: &board,
        block_entities: false,
        structures: false,
        rng: RefCell::new(StdRng::seed_from_u64(seed)),
    };
    f(&view)
}

static FOREIGN_REPORTED: AtomicBool = AtomicBool::new(false);

fn first_report(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::Relaxed)
}

impl RegionView<'_> {
    fn loaded_block(
        &self,
        pos: BlockPos,
    ) -> Option<(&dyn RegionWorld, tuff_vanilla_data::BlockState)> {
        let region = self.region?;
        if !region.owns(pos) {
            if first_report(&FOREIGN_REPORTED) {
                log::warn!(
                    "a predicate read a position owned by another region; it evaluates false (further reports suppressed)"
                );
            }
            return None;
        }
        Some((region, region.block(pos)?))
    }

    fn smokey(&self, pos: BlockPos) -> bool {
        let lit = |state: tuff_vanilla_data::BlockState| {
            state.get("lit") == Some("true")
                && matches!(
                    state.block(),
                    tuff_vanilla_data::Block::Campfire | tuff_vanilla_data::Block::SoulCampfire
                )
        };
        for depth in 1..=5 {
            let below = BlockPos::new(pos.x, pos.y - depth, pos.z);
            let Some((_, state)) = self.loaded_block(below) else {
                return false;
            };
            if lit(state) {
                return true;
            }
            if !state.collision_boxes().is_empty() {
                let under = BlockPos::new(pos.x, pos.y - depth - 1, pos.z);
                return self
                    .loaded_block(under)
                    .is_some_and(|(_, state)| lit(state));
            }
        }
        false
    }
}

impl PredicateWorld for RegionView<'_> {
    fn location(&self, position: [f64; 3]) -> LocationFacts {
        let dimension = self.shared.dimension(self.dimension);
        let mut facts = LocationFacts {
            position,
            dimension: dimension.key.clone(),
            ..LocationFacts::default()
        };
        let pos = BlockPos::containing(position);
        let Some((region, state)) = self.loaded_block(pos) else {
            return facts;
        };
        facts.block = Some(state);
        facts.biome = region.biome_name(pos).map(str::to_owned);
        facts.light = region.light(pos).map(|(sky, block)| {
            let sky = if dimension.kind.has_skylight { sky } else { 0 };
            sky.saturating_sub(self.sky_darken).max(block)
        });
        facts.can_see_sky = region.world_surface(pos.x, pos.z).map(|top| pos.y >= top);
        facts.smokey = Some(self.smokey(pos));
        if self.block_entities {
            facts.block_entity = region.saved_block_entity(pos);
        }
        if self.structures {
            facts.structures = Some(structures_at(region, pos));
        }
        facts
    }

    fn raining(&self) -> bool {
        let kind = self.shared.dimension(self.dimension).kind;
        kind.has_skylight && !kind.has_ceiling && self.shared.weather.borrow().is_raining()
    }

    fn thundering(&self) -> bool {
        self.raining() && self.shared.weather.borrow().is_thundering()
    }

    fn clock(&self, id: &str) -> Option<i64> {
        let clock = crate::world::clock::clock_id(id)?;
        Some(self.shared.clocks.borrow().get(clock).total_ticks)
    }

    fn score(&self, holder: &str, objective: &str) -> Option<i32> {
        self.board
            .scores
            .get(&(holder.to_owned(), objective.to_owned()))
            .copied()
    }

    fn team(&self, holder: &str) -> Option<&str> {
        self.board.teams.get(holder).map(String::as_str)
    }

    fn random(&self) -> f32 {
        self.rng.borrow_mut().r#gen()
    }

    fn storage(&self, id: &str) -> Option<Compound> {
        self.board.storages.get(id).cloned()
    }

    fn attribute(&self, key: &str, position: [f64; 3]) -> Option<Value> {
        let dimension = self.shared.dimension(self.dimension);
        let environment = dimension.environment();
        let clocks = self.shared.clocks.borrow();
        let ticks = |clock: &str| {
            crate::world::clock::clock_id(clock).map_or(0, |id| clocks.get(id).total_ticks)
        };
        let weather = *self.shared.weather.borrow();
        let rain = weather.rain_level;
        let thunder = weather.thunder_level * weather.rain_level;
        let number = |value: f32| serde_json::Number::from_f64(f64::from(value)).map(Value::Number);
        match key {
            "minecraft:gameplay/sky_light_level" => {
                number(environment.sky_light(ticks, rain, thunder))
            }
            "minecraft:gameplay/monsters_burn" => Some(environment.monsters_burn(ticks).into()),
            "minecraft:gameplay/surface_slime_spawn_chance" => {
                number(environment.surface_slime_spawn_chance(ticks))
            }
            "minecraft:gameplay/turtle_egg_hatch_chance" => {
                number(environment.turtle_egg_hatch_chance(ticks))
            }
            "minecraft:gameplay/creaking_active" => Some(environment.creaking_active(ticks).into()),
            "minecraft:gameplay/bees_stay_in_hive" => {
                Some(environment.bees_stay_in_hive(ticks, rain, thunder).into())
            }
            "minecraft:gameplay/piglins_zombify" => Some(environment.piglins_zombify.into()),
            "minecraft:gameplay/respawn_anchor_works" => {
                Some(environment.respawn_anchor_works.into())
            }
            "minecraft:gameplay/can_start_raid" => Some(environment.can_start_raid.into()),
            "minecraft:gameplay/water_evaporates" => Some(environment.water_evaporates.into()),
            "minecraft:gameplay/nether_portal_spawns_piglin" => {
                Some(environment.nether_portal_spawns_piglin.into())
            }
            "minecraft:gameplay/increased_fire_burnout" => {
                let biome = self
                    .location(position)
                    .biome
                    .and_then(|name| tuff_vanilla_data::biome::Biome::get(&name))
                    .and_then(|biome| biome.attribute(key))
                    .and_then(|value| value.as_bool());
                Some(biome.unwrap_or(environment.increased_fire_burnout).into())
            }
            "minecraft:visual/sun_angle" => number(environment.sun_angle(ticks)),
            _ => self
                .location(position)
                .biome
                .and_then(|name| tuff_vanilla_data::biome::Biome::get(&name))
                .and_then(|biome| biome.attribute(key))
                .or_else(|| environment.timeline_attribute(key, ticks)),
        }
    }

    fn random_int(&self, min: i64, max: i64) -> i64 {
        if max <= min {
            min
        } else {
            self.rng.borrow_mut().gen_range(min..=max)
        }
    }
}

/// The structures with a piece containing `pos`: those the chunk refers
/// to whose start (in a chunk this region holds) has a piece there.
fn structures_at(region: &dyn RegionWorld, pos: BlockPos) -> Vec<String> {
    let structures = |chunk: ChunkPos| {
        region
            .chunk_extra(chunk)
            .and_then(|extra| extra.get_compound("structures"))
    };
    let Some(references) = structures(ChunkPos::of_position([
        f64::from(pos.x),
        0.0,
        f64::from(pos.z),
    ]))
    .and_then(|structures| structures.get_compound("References")) else {
        return Vec::new();
    };
    let inside = |piece: &Compound| {
        piece.get_int_array("BB").is_some_and(|bb| {
            bb.len() == 6
                && (bb[0]..=bb[3]).contains(&pos.x)
                && (bb[1]..=bb[4]).contains(&pos.y)
                && (bb[2]..=bb[5]).contains(&pos.z)
        })
    };
    let mut found = Vec::new();
    for (id, keys) in references.iter() {
        let starts = keys
            .as_long_array()
            .unwrap_or(&[])
            .iter()
            .filter_map(|key| {
                let chunk = ChunkPos::new(*key as i32, (*key >> 32) as i32);
                structures(chunk)?.get_compound("starts")?.get_compound(id)
            });
        let hit = starts.into_iter().any(|start| {
            start
                .get_list("Children")
                .unwrap_or(&[])
                .iter()
                .filter_map(tuff_nbt::Tag::as_compound)
                .any(inside)
        });
        if hit {
            found.push(id.to_owned());
        }
    }
    found
}

/// A region entity's facts as its region sees them, with its vehicle,
/// passengers and target; `None` when it is not there.
fn region_entity_facts(region: &dyn RegionWorld, id: i32, saved_form: bool) -> Option<EntityFacts> {
    let mut facts = entity_facts_alone(region, id, saved_form)?;
    let tracked = region.entity(id)?;
    let base = tracked.entity.base();
    if let Some(mount) = &base.vehicle {
        facts.vehicle = entity_facts_alone(region, mount.id, saved_form).map(Box::new);
    }
    facts.passengers = base
        .entity_riders
        .iter()
        .filter_map(|rider| entity_facts_alone(region, *rider, saved_form))
        .collect();
    let players = region.present_players();
    for rider in &base.riders {
        if let Some(player) = players.iter().find(|player| player.id == rider.id) {
            facts
                .passengers
                .push(present_player_facts(region, player, saved_form));
        }
    }
    if let Some(target) = tracked.entity.attack_target() {
        facts.target = entity_facts_alone(region, target, saved_form)
            .or_else(|| {
                players
                    .iter()
                    .find(|player| player.id == target)
                    .map(|player| present_player_facts(region, player, saved_form))
            })
            .map(Box::new);
    }
    Some(facts)
}

/// A player the region sees, as far as the region knows it.
fn present_player_facts(
    region: &dyn RegionWorld,
    player: &crate::world::region_api::PresentPlayer,
    saved_form: bool,
) -> EntityFacts {
    let mut facts = EntityFacts {
        kind: Some(EntityType::Player),
        position: player.position,
        dimension: region.dimension_info().key.clone(),
        ..EntityFacts::default()
    };
    if saved_form && let Some(saved) = player.handle.selector_data() {
        facts.equipment = equipment_of(&saved);
        facts.nbt = Some((*saved).clone());
    }
    if let Some(live) = player.handle.live_facts() {
        facts = facts.with_live(&live);
    }
    facts
}

/// [`region_entity_facts`] without the entities around it.
fn entity_facts_alone(region: &dyn RegionWorld, id: i32, saved_form: bool) -> Option<EntityFacts> {
    let tracked = region.entity(id)?;
    let entity = &*tracked.entity;
    let base = entity.base();
    let mut facts = entity.facts();
    facts.kind = Some(base.kind);
    facts.position = base.position;
    facts.dimension = region.dimension_info().key.clone();
    let pos = BlockPos::containing(base.position);
    facts.biome = region.biome_name(pos).map(str::to_owned);
    facts.flags.on_fire |= base.on_fire();
    facts.flags.on_ground |= base.on_ground;
    facts.flags.in_water = region.block(pos).is_some_and(|state| {
        state
            .fluid_state()
            .fluid()
            .is_same(tuff_vanilla_data::fluid::Fluid::Water)
    });
    facts.stepping_on = region.block(BlockPos::containing([
        base.position[0],
        base.position[1] - 0.2,
        base.position[2],
    ]));
    facts.holder = Some(tuff_storage::admin::format_uuid(base.uuid));
    facts.tags = base.tags.clone();
    facts.tick_count = Some(tracked.ticks);
    facts.movement = Some(base.velocity);
    facts.fall_distance = Some(base.fall_distance);
    if saved_form {
        let mut saved = crate::world::entity::save(entity);
        saved.remove("id");
        if facts.effects.is_empty() {
            facts.effects = effects_of(&saved);
        }
        facts.nbt = Some(saved);
    }
    Some(facts)
}

fn effects_of(saved: &Compound) -> Vec<crate::world::predicate::EffectFact> {
    crate::world::effects::ActiveEffects::from_nbt(saved.get_list("active_effects").unwrap_or(&[]))
        .iter()
        .map(|effect| {
            (
                effect.effect,
                i32::from(effect.amplifier),
                effect.duration,
                effect.ambient,
                effect.show_particles,
            )
        })
        .collect()
}

/// Tests every condition on every subject in one region (or with no
/// region, where nothing is loaded), in order.
fn evaluate<R>(
    region: Option<&dyn RegionWorld>,
    shared: &Shared,
    answer: &Answer<R>,
    subjects: &[Subject],
    board: &Board,
    needs: &Needs,
    seed: u64,
) -> Vec<R> {
    let Some(first) = subjects.first() else {
        return Vec::new();
    };
    let dimension = shared.dimension(first.dimension);
    let view = RegionView {
        region,
        shared,
        dimension: first.dimension,
        sky_darken: crate::world::sky_darken(shared, dimension),
        board,
        block_entities: needs.block_entities || subjects.iter().any(|s| s.at_block),
        structures: needs.structures,
        rng: RefCell::new(StdRng::seed_from_u64(seed)),
    };
    subjects
        .iter()
        .map(|subject| {
            let resolve = |this: &Option<This>| match this {
                None => None,
                Some(This::Facts(facts)) => Some((**facts).clone()),
                Some(This::Entity { id, known }) => Some(
                    region
                        .and_then(|region| region_entity_facts(region, *id, needs.saved_form))
                        .unwrap_or_else(|| (**known).clone()),
                ),
            };
            let mut facts = resolve(&subject.this);
            let target = resolve(&subject.target);
            if needs.looking_at
                && let (Some(region), Some(facts)) = (region, facts.as_mut())
            {
                facts.looking_at = crosshair_entity(region, facts, needs.saved_form);
            }
            let location = subject.at_block.then(|| view.location(subject.origin));
            let context = PredicateContext {
                this: facts.as_ref(),
                target: target.as_ref(),
                origin: subject.origin,
                location: location.as_ref(),
                tool: None,
                enchantment_level: None,
            };
            answer(&context, &view)
        })
        .collect()
}

/// What is asked of each subject, given its context and the world view.
type Answer<R> = Arc<dyn Fn(&PredicateContext<'_>, &dyn PredicateWorld) -> R + Send + Sync>;

/// Tests `conditions` on each subject: one result per condition per
/// subject. Subjects are grouped by the region around their origin, and
/// each group is evaluated on that region.
pub async fn test(
    ctx: &mut Context<'_>,
    conditions: &[Arc<Value>],
    subjects: Vec<Subject>,
) -> Vec<Vec<bool>> {
    let tested: Arc<[Arc<Value>]> = conditions.into();
    let answer: Answer<Vec<bool>> = Arc::new(move |context, world| {
        tested
            .iter()
            .map(|condition| predicate::condition_in(condition, context, Some(world)))
            .collect()
    });
    ask(ctx, conditions, subjects, answer).await
}

/// A number provider's value for `subject`, as `/data modify ... compute`
/// reads it: a float, or an int when `float` is false.
pub async fn compute(
    ctx: &mut Context<'_>,
    provider: Arc<Value>,
    float: bool,
    subject: Subject,
) -> Option<f64> {
    let probes = [provider.clone()];
    let answer: Answer<Option<f64>> = Arc::new(move |context, world| {
        predicate::number_in(&provider, float, context, Some(world))
    });
    ask(ctx, &probes, vec![subject], answer)
        .await
        .into_iter()
        .next()
        .flatten()
}

/// Puts `answer` to each subject on the region around its origin.
async fn ask<R: Send + 'static>(
    ctx: &mut Context<'_>,
    probes: &[Arc<Value>],
    subjects: Vec<Subject>,
    question: Answer<R>,
) -> Vec<R> {
    let needs = Needs::of(probes);
    let holders: Vec<String> = subjects
        .iter()
        .flat_map(|subject| [&subject.this, &subject.target])
        .filter_map(|this| match this {
            Some(This::Facts(facts)) | Some(This::Entity { known: facts, .. }) => {
                facts.holder.clone()
            }
            None => None,
        })
        .collect();
    let board = Arc::new(read_board(ctx, &needs, holders).await);
    let needs = Arc::new(needs);
    let mut groups: BTreeMap<(usize, i32, i32), Vec<usize>> = BTreeMap::new();
    for (index, subject) in subjects.iter().enumerate() {
        let region = ChunkPos::of_position(subject.origin).region();
        groups
            .entry((subject.dimension.index(), region.x, region.z))
            .or_default()
            .push(index);
    }
    let mut results: Vec<Option<R>> = (0..subjects.len()).map(|_| None).collect();
    for indices in groups.into_values() {
        let group: Vec<Subject> = indices.iter().map(|i| subjects[*i].clone()).collect();
        let first = &group[0];
        let seed = ctx.rng().r#gen();
        let region = ctx
            .actor
            .region(
                first.dimension,
                ChunkPos::of_position(first.origin).region(),
            )
            .await;
        let mut answered = None;
        if let Some(region) = region {
            let (reply, answer) = oneshot::channel();
            let (question, board, needs) = (question.clone(), board.clone(), needs.clone());
            let task_group = group.clone();
            let task = RegionTask::new(move |world| {
                let shared = world.shared().clone();
                let world: &dyn RegionWorld = world;
                let _ = reply.send(evaluate(
                    Some(world),
                    &shared,
                    &question,
                    &task_group,
                    &board,
                    &needs,
                    seed,
                ));
            });
            if region.internal(RegionInternal::Run(task)) {
                answered = answer.await.ok();
            }
        }
        let found = answered.unwrap_or_else(|| {
            evaluate(None, ctx.shared(), &question, &group, &board, &needs, seed)
        });
        for (index, result) in indices.into_iter().zip(found) {
            results[index] = Some(result);
        }
    }
    results
        .into_iter()
        .map(|result| result.expect("every subject is answered"))
        .collect()
}

/// A player as predicates see it, from its published state and saved form.
pub fn player_facts(
    shared: &Shared,
    player: &PlayerSnapshot,
    saved: Option<&Compound>,
) -> EntityFacts {
    let state = &player.state;
    let mut facts = EntityFacts {
        kind: Some(EntityType::Player),
        position: state.position,
        dimension: shared.dimension(state.dimension).key.clone(),
        holder: Some(player.name.clone()),
        tags: state.tags.clone(),
        player: Some(PlayerFacts {
            game_mode: state.game_mode,
            level: state.level,
            advancements: state.advancements.clone(),
            stats: None,
            input: None,
            food: None,
        }),
        ..EntityFacts::default()
    };
    facts.flags.sneaking = state.looks.sneaking;
    facts.view = Some(crate::world::predicate::ViewRay {
        eye: [
            state.position[0],
            state.position[1] + state.looks.eye_height,
            state.position[2],
        ],
        yaw: state.looks.yaw,
        pitch: state.looks.pitch,
    });
    if let Some(saved) = saved {
        facts.flags.on_fire = saved.get_short("Fire").is_some_and(|fire| fire > 0);
        facts.flags.on_ground = saved.get_bool("OnGround").unwrap_or(false);
        facts.flags.flying = saved
            .get_compound("abilities")
            .and_then(|abilities| abilities.get_bool("flying"))
            .unwrap_or(false);
        facts.flags.fall_flying = saved.get_bool("FallFlying").unwrap_or(false);
        facts.equipment = equipment_of(saved);
        facts.effects = effects_of(saved);
        facts.nbt = Some(saved.clone());
    }
    facts
}

const LOOKING_AT_RANGE: f64 = 100.0;

/// The nearest entity along a player's view within 100 blocks that it has
/// line of sight to, as its facts.
fn crosshair_entity(
    region: &dyn RegionWorld,
    player: &EntityFacts,
    saved_form: bool,
) -> Option<Box<EntityFacts>> {
    use crate::world::projectile::clip;
    let view = player.view?;
    let direction = crate::world::raycast::view_vector(view.yaw, view.pitch);
    let to = [0, 1, 2].map(|i| view.eye[i] + direction[i] * LOOKING_AT_RANGE);
    let mut best: Option<(f64, EntityFacts, [f64; 3])> = None;
    let mut consider = |bounds: [f64; 6], facts: &dyn Fn() -> EntityFacts| {
        if let Some(hit) = clip(bounds, view.eye, to) {
            let distance: f64 = (0..3).map(|i| (hit[i] - view.eye[i]).powi(2)).sum();
            if best
                .as_ref()
                .is_none_or(|(nearest, ..)| distance < *nearest)
            {
                let eye = [
                    (bounds[0] + bounds[3]) / 2.0,
                    bounds[1] + (bounds[4] - bounds[1]) * 0.85,
                    (bounds[2] + bounds[5]) / 2.0,
                ];
                best = Some((distance, facts(), eye));
            }
        }
    };
    for id in region.entity_ids() {
        if let Some(tracked) = region.entity(id) {
            consider(tracked.entity.bounds(), &|| {
                region_entity_facts(region, id, saved_form).unwrap_or_default()
            });
        }
    }
    for other in region.present_players() {
        let [x, y, z] = other.position;
        let bounds = [x - 0.3, y, z - 0.3, x + 0.3, y + other.height, z + 0.3];
        consider(bounds, &|| present_player_facts(region, &other, saved_form));
    }
    let (_, facts, target_eye) = best?;
    let level = |pos: BlockPos| region.block(pos).unwrap_or_default();
    crate::world::raycast::cast_colliders(view.eye, target_eye, &level)
        .is_none()
        .then(|| Box::new(facts))
}

fn equipment_of(saved: &Compound) -> Vec<(EquipmentSlot, tuff_item::item::ItemSnapshot)> {
    let mut equipment = Vec::new();
    if let Some(held) = saved
        .get_compound("SelectedItem")
        .and_then(|nbt| ItemStack::mint_from_nbt(nbt, MintReason::Template))
    {
        equipment.push((EquipmentSlot::Mainhand, snapshot_template(held)));
    }
    let slots = [
        ("offhand", EquipmentSlot::Offhand),
        ("feet", EquipmentSlot::Feet),
        ("legs", EquipmentSlot::Legs),
        ("chest", EquipmentSlot::Chest),
        ("head", EquipmentSlot::Head),
        ("body", EquipmentSlot::Body),
    ];
    if let Some(worn) = saved.get_compound("equipment") {
        for (name, slot) in slots {
            if let Some(stack) = worn
                .get_compound(name)
                .and_then(|nbt| ItemStack::mint_from_nbt(nbt, MintReason::Template))
            {
                equipment.push((slot, snapshot_template(stack)));
            }
        }
    }
    equipment
}

fn snapshot_template(stack: ItemStack) -> tuff_item::item::ItemSnapshot {
    let snapshot = tuff_item::item::ItemSnapshot::of(&stack);
    stack.burn(tuff_item::item::BurnReason::Template);
    snapshot
}

/// A candidate's facts before its region adds what it knows.
fn known_facts(shared: &Shared, candidate: &Candidate) -> EntityFacts {
    EntityFacts {
        kind: Some(candidate.kind),
        position: candidate.position,
        dimension: shared.dimension(candidate.dimension).key.clone(),
        holder: Some(candidate.score_holder()),
        tags: candidate.tags.clone(),
        nbt: candidate.nbt.clone(),
        ..EntityFacts::default()
    }
}

/// The facts of the player `id` running or targeted by a command: its own
/// session's when the command runs there, else its published form.
pub fn facts_of_player(
    ctx: &Context<'_>,
    snapshots: &[PlayerSnapshot],
    id: i32,
) -> Option<EntityFacts> {
    let snapshot = snapshots.iter().find(|player| player.id == id)?;
    let saved = ctx
        .actor
        .own_selector_nbt(id)
        .or_else(|| snapshot.handle.selector_data().map(|form| (*form).clone()));
    let facts = player_facts(ctx.shared(), snapshot, saved.as_ref());
    let live = ctx
        .actor
        .own_live_facts(id)
        .or_else(|| snapshot.handle.live_facts().map(|live| (*live).clone()));
    Some(match live {
        Some(live) => facts.with_live(&live),
        None => facts,
    })
}

/// Fills in each candidate's `predicate=` results. Only candidates that
/// pass the selector's other options are tested, as the game tests options
/// in turn; a predicate no pack defines fails whether negated or not.
pub async fn apply(
    ctx: &mut Context<'_>,
    selector: &Selector,
    source: &SelectorSource,
    candidates: &mut [Candidate],
    snapshots: &[PlayerSnapshot],
) {
    if selector.predicates.is_empty() {
        return;
    }
    let stored = ctx.shared().datapacks.contents().predicates.clone();
    let found: Vec<Option<Arc<Value>>> = selector
        .predicates
        .iter()
        .map(|(id, _)| stored.get(id))
        .collect();
    let conditions: Vec<Arc<Value>> = found.iter().flatten().cloned().collect();
    let tested: Vec<usize> = (0..candidates.len())
        .filter(|i| selector.matches_besides_predicates(&candidates[*i], source))
        .collect();
    let subjects: Vec<Subject> = tested
        .iter()
        .map(|i| {
            let candidate = &candidates[*i];
            let this = if candidate.is_player() {
                facts_of_player(ctx, snapshots, candidate.id)
                    .map(|facts| This::Facts(Box::new(facts)))
            } else {
                Some(This::Entity {
                    id: candidate.id,
                    known: Box::new(known_facts(ctx.shared(), candidate)),
                })
            };
            Subject {
                dimension: candidate.dimension,
                origin: candidate.position,
                this,
                target: None,
                at_block: false,
            }
        })
        .collect();
    let results = if conditions.is_empty() {
        vec![Vec::new(); subjects.len()]
    } else {
        test(ctx, &conditions, subjects).await
    };
    for (index, outcome) in tested.into_iter().zip(results) {
        let mut outcome = outcome.into_iter();
        candidates[index].predicates = selector
            .predicates
            .iter()
            .zip(&found)
            .map(|((_, inverted), condition)| {
                condition.is_some() && outcome.next().is_some_and(|passed| passed != *inverted)
            })
            .collect();
    }
}

/// The predicate an `execute if predicate` argument names or spells out,
/// with every stored predicate it refers to inlined.
pub fn resolve(
    shared: &Shared,
    argument: &crate::command::args::Value,
) -> Result<Arc<Value>, crate::command::CommandError> {
    use crate::command::CommandError;
    use crate::command::args::Value as Argument;
    let stored = shared.datapacks.contents().predicates.clone();
    match argument {
        Argument::Identifier(id) => stored.get(id).ok_or_else(|| {
            CommandError::dynamic(
                "argument.resource_or_id.no_such_element",
                [id.clone(), "minecraft:predicate".to_owned()],
            )
        }),
        Argument::Nbt(inline) => stored
            .inline(&json_of(inline))
            .map(Arc::new)
            .map_err(|err| CommandError::dynamic("argument.resource_or_id.failed_to_parse", [err])),
        _ => Err(CommandError::simple("command.unknown.argument")),
    }
}

/// An inline predicate's NBT as the JSON pack files hold (bytes stay
/// numbers; predicates read either as booleans).
pub fn json_of(tag: &tuff_nbt::Tag) -> Value {
    use tuff_nbt::Tag;
    let number = |n: f64| serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number);
    match tag {
        Tag::Byte(v) => Value::from(*v),
        Tag::Short(v) => Value::from(*v),
        Tag::Int(v) => Value::from(*v),
        Tag::Long(v) => Value::from(*v),
        Tag::Float(v) => number(f64::from(*v)),
        Tag::Double(v) => number(*v),
        Tag::String(v) => Value::String(v.clone()),
        Tag::ByteArray(v) => Value::Array(v.iter().map(|b| Value::from(*b)).collect()),
        Tag::IntArray(v) => Value::Array(v.iter().map(|i| Value::from(*i)).collect()),
        Tag::LongArray(v) => Value::Array(v.iter().map(|l| Value::from(*l)).collect()),
        Tag::List(items) => Value::Array(items.iter().map(json_of).collect()),
        Tag::Compound(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.to_owned(), json_of(value)))
                .collect(),
        ),
    }
}

/// An entity's facts read on the region at `position`, for a source
/// entity standing elsewhere than the tested position.
async fn locate_entity(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    position: [f64; 3],
    id: i32,
    saved_form: bool,
) -> Option<EntityFacts> {
    let region = ctx
        .actor
        .region(dimension, ChunkPos::of_position(position).region())
        .await?;
    let (reply, answer) = oneshot::channel();
    let task = RegionTask::new(move |world| {
        let _ = reply.send(region_entity_facts(world, id, saved_form));
    });
    if !region.internal(RegionInternal::Run(task)) {
        return None;
    }
    answer.await.ok().flatten()
}

/// Tests `condition` as `execute if predicate` does: at the source's
/// position and dimension, with the entity it runs as (if any) as `this`.
pub async fn test_source(ctx: &mut Context<'_>, condition: Arc<Value>) -> bool {
    let conditions = [condition];
    let subject = source_subject(ctx, &conditions).await;
    test(ctx, &conditions, vec![subject])
        .await
        .first()
        .and_then(|results| results.first().copied())
        .unwrap_or(false)
}

/// The command source as a subject: its position and dimension, with the
/// entity it runs as (if any) as `this`, gathered as `probes` need it.
pub async fn source_subject(ctx: &mut Context<'_>, probes: &[Arc<Value>]) -> Subject {
    let needs = Needs::of(probes);
    let (dimension, origin) = (ctx.source.dimension, ctx.source.position);
    let this = match ctx.source.entity {
        None => None,
        Some(id) => entity_this(ctx, id, dimension, origin, needs.saved_form).await,
    };
    Subject {
        dimension,
        origin,
        this,
        target: None,
        at_block: false,
    }
}

/// The entity `id` as predicate facts, found near `origin` or anywhere.
async fn entity_this(
    ctx: &mut Context<'_>,
    id: i32,
    dimension: DimensionId,
    origin: [f64; 3],
    saved_form: bool,
) -> Option<This> {
    let players = super::targets::players(ctx).await;
    if players.iter().any(|player| player.id == id) {
        return facts_of_player(ctx, &players, id).map(|facts| This::Facts(Box::new(facts)));
    }
    let mut found = locate_entity(ctx, dimension, origin, id, saved_form).await;
    if found.is_none() {
        let entities = super::targets::entities(ctx).await;
        if let Some(entity) = entities.iter().find(|entity| entity.id == id) {
            let (dimension, position) = (entity.dimension, entity.position);
            found = locate_entity(ctx, dimension, position, id, saved_form).await;
        }
    }
    found.map(|facts| This::Facts(Box::new(facts)))
}

/// `subject` with the entity `id` as the `target_entity` parameter.
pub async fn with_target(
    ctx: &mut Context<'_>,
    mut subject: Subject,
    id: i32,
    probes: &[Arc<Value>],
) -> Subject {
    let saved_form = Needs::of(probes).saved_form;
    let (dimension, origin) = (subject.dimension, subject.origin);
    subject.target = entity_this(ctx, id, dimension, origin, saved_form).await;
    subject
}

/// `subject` evaluated at the centre of `pos`, with that block's state and
/// block entity as parameters.
pub fn with_block(mut subject: Subject, pos: [i32; 3]) -> Subject {
    subject.origin = pos.map(|axis| f64::from(axis) + 0.5);
    subject.at_block = true;
    subject
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_foreign_position_is_reported_once() {
        let flag = AtomicBool::new(false);
        assert!(first_report(&flag));
        assert!(!first_report(&flag));
    }
}
