//! The per-entity half of the entity tick: each entity's own tick runs
//! against a snapshot of the others with its own random source and path
//! credit, and what it does to the world is applied afterwards in order.

use std::collections::HashMap;

use super::super::far_chunks::FarChunks;
use super::super::{RegionActor, block_at};
use super::player_vehicle;
use crate::world::entity::{
    self, Effect, EntityGrid, EntitySummary, NearbyPlayer, PartBox, Tick, TickContext,
    TrackedEntity,
};
use crate::world::physics::Aabb;
use crate::world::region_api::RegionStateLock;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkMap, ChunkPos};

/// How many ticks of its share an entity may save up for one search. Saved
/// credit lets a mob run a full search every few ticks even when its
/// per-tick share is small, while the region's average spend stays at
/// `PATH_BUDGET` a tick.
pub(super) const PATH_CREDIT_TICKS: u32 = 16;

/// Each entity's slice of the region's path budget this tick: a fixed
/// share of `PATH_BUDGET` by entity count, so what one entity may search
/// never depends on what others searched before it.
#[derive(Clone, Copy)]
pub(super) struct PathCredit {
    pub(super) share: u32,
    pub(super) cap: u32,
}

/// What an entity's own tick produced, computed against the tick's
/// snapshot and applied to the region afterwards in tick order.
pub(super) struct Computed {
    pub(super) from: ChunkPos,
    pub(super) before: [f64; 3],
    pub(super) data_changed: bool,
    pub(super) riders_before: usize,
    pub(super) holder_before: Option<i32>,
    pub(super) base: entity::BaseTick,
    pub(super) outcome: Tick,
    pub(super) effects: Vec<Effect>,
}

/// An entity's own random source for one tick, from the region's draw for
/// the tick and its id, so its rolls do not depend on the tick order.
fn entity_rng(seed: u64, id: i32) -> rand::rngs::StdRng {
    use rand::SeedableRng;
    rand::rngs::StdRng::seed_from_u64(
        seed ^ u64::from(id as u32).wrapping_mul(0x9E37_79B9_7F4A_7C15),
    )
}

impl RegionActor {
    /// The boxes of every part of the entities made of parts.
    pub(in crate::world::region) fn part_boxes(&self) -> Vec<PartBox> {
        self.ents
            .part_owners
            .iter()
            .filter_map(|id| self.ents.entities.get(id))
            .flat_map(|tracked| {
                let owner = tracked.id();
                let kind = tracked.entity.base().kind;
                let positions = tracked.entity.part_positions();
                tracked
                    .entity
                    .parts()
                    .into_iter()
                    .zip(positions)
                    .enumerate()
                    .map(move |(i, (bounds, position))| PartBox {
                        id: owner + 1 + i as i32,
                        owner,
                        kind,
                        bounds,
                        position,
                    })
            })
            .collect()
    }

    /// Whether rain falls on a burning entity, which puts it out.
    fn in_rain(&self, tracked: &TrackedEntity) -> bool {
        tracked.entity.base().fire > 0 && {
            let bounds = tracked.entity.bounds();
            self.rained_on(tracked.entity.base().position, bounds[4])
        }
    }

    pub(super) fn refresh_vehicles(&mut self, nearby: &mut [NearbyPlayer]) {
        let vehicles = self.ents.rider_vehicles.get_or_insert_with(HashMap::new);
        for player in nearby.iter_mut() {
            if vehicles.contains_key(&player.id) {
                player.vehicle = player_vehicle(vehicles, &self.ents.entities, player.id);
            }
        }
    }

    /// Serially, in tick order: drops the entities that do not tick this
    /// tick and does what must happen before any entity's own tick (orbs
    /// merging, a driven vehicle's impulse, riders stopping). Returns the
    /// entities to tick.
    pub(super) fn prepare_entities(&mut self, ids: &[i32]) -> Vec<i32> {
        let mut ready = Vec::with_capacity(ids.len());
        for &id in ids {
            let Some(mut tracked) = self.ents.entities.remove(&id) else {
                continue;
            };
            let from = tracked.chunk();
            if self
                .columns
                .get(&from)
                .is_none_or(|c| c.unloading || (c.viewers.is_empty() && !c.held.is_empty()))
                || (self.sched.frozen && tracked.entity.base().riders.is_empty())
                || !self.simulates(from)
            {
                self.ents.entities.insert(id, tracked);
                continue;
            }
            if tracked.entity.orb().is_some() {
                self.merge_orbs(&mut tracked);
            }
            self.forward_impulse(&tracked);
            if tracked.entity.base().vehicle.is_some() {
                let base = tracked.entity.base_mut();
                base.velocity = [0.0; 3];
                base.fall_distance = 0.0;
            }
            self.ents.entities.insert(id, tracked);
            ready.push(id);
        }
        ready
    }

    /// Runs the own ticks of `order` against the same snapshot, spread
    /// over the compute pool when there are enough of them. Each result
    /// depends only on its entity and the snapshot, so the split changes
    /// nothing.
    pub(super) fn compute_entities(
        &mut self,
        order: &[i32],
        summaries: &[EntitySummary],
        grid: &EntityGrid,
        nearby: &[NearbyPlayer],
        seed: u64,
        credit: PathCredit,
    ) -> HashMap<i32, Computed> {
        use rayon::prelude::*;
        let parts = self.part_boxes();
        let mut ticking: Vec<(i32, bool, TrackedEntity)> = order
            .iter()
            .filter_map(|&id| {
                let tracked = self.ents.entities.remove(&id)?;
                Some((id, self.in_rain(&tracked), tracked))
            })
            .collect();
        let min_len = self
            .shared
            .entity_compute_min_batch
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(1);
        let results: Vec<Computed> = self.with_frame(Some(parts), |frame| {
            let compute = |(_, in_rain, tracked): &mut (i32, bool, TrackedEntity)| {
                frame.compute(tracked, *in_rain, summaries, grid, nearby, seed, credit)
            };
            if ticking.len() < min_len.saturating_mul(2) {
                ticking.iter_mut().map(compute).collect()
            } else {
                let tags = tuff_datapack::tags::current();
                crate::threads::tick_pool().install(|| {
                    ticking
                        .par_iter_mut()
                        .with_min_len(min_len)
                        .map(|t| tuff_datapack::tags::sync_scope(tags.clone(), || compute(t)))
                        .collect()
                })
            }
        });
        let mut computed = HashMap::with_capacity(results.len());
        for ((id, _, tracked), result) in ticking.into_iter().zip(results) {
            self.ents.entities.insert(id, tracked);
            computed.insert(id, result);
        }
        computed
    }

    /// Runs one entity's own tick against the snapshot of the others, with
    /// its own random source and path credit. It changes only the entity;
    /// what it does to the world comes back to be applied in tick order.
    pub(super) fn compute_entity(
        &mut self,
        id: i32,
        summaries: &[EntitySummary],
        grid: &EntityGrid,
        nearby: &[NearbyPlayer],
        seed: u64,
        credit: PathCredit,
    ) -> Option<Computed> {
        let mut tracked = self.ents.entities.remove(&id)?;
        let in_rain = self.in_rain(&tracked);
        let computed = self.with_frame(None, |frame| {
            frame.compute(&mut tracked, in_rain, summaries, grid, nearby, seed, credit)
        });
        self.ents.entities.insert(id, tracked);
        Some(computed)
    }
}

/// What every tick context in one pass shares, built once: the region's
/// world read-only, plus the piston and part boxes and the dimension's
/// state. Entity ticks run against it in parallel.
pub(in crate::world::region) struct Frame<'r> {
    tick: u64,
    dimension: crate::world::dimension::DimensionId,
    columns: &'r ChunkMap<super::super::Column>,
    halo: &'r ChunkMap<tuff_world_model::chunk::Chunk>,
    lighting: &'r Option<Box<crate::world::lighting::Lighting>>,
    far_halves: &'r super::super::halves::Snapshots,
    viewers: &'r HashMap<BlockPos, Vec<crate::world::messages::PlayerHandle>>,
    mob_openers: &'r HashMap<BlockPos, Vec<i32>>,
    pois: &'r crate::world::poi::PoiStore,
    shared: &'r crate::world::Shared,
    far: &'r FarChunks,
    state: &'r RegionStateLock,
    pistons: Vec<Aabb>,
    parts: Vec<PartBox>,
    sky_darken: u8,
    ambient_light: f32,
    monsters_burn: bool,
    raining: bool,
    players_in_dimension: usize,
    min_y: i32,
    max_y: i32,
}

impl<'r> Frame<'r> {
    pub(in crate::world::region) fn new(
        region: &'r RegionActor,
        far: &'r FarChunks,
        state: &'r RegionStateLock,
        parts: Option<Vec<PartBox>>,
    ) -> Self {
        let mut pistons = region.moving_colliders();
        let shared = &region.shared;
        let dimension = shared.dimension(region.dimension);
        let span = f64::from(tuff_world_model::pos::REGION_CHUNKS * 16);
        pistons.extend(dimension.border().collision_slabs(
            (f64::from(region.pos.x) + 0.5) * span,
            (f64::from(region.pos.z) + 0.5) * span,
            span / 2.0 + 64.0,
        ));
        let parts = parts.unwrap_or_else(|| region.part_boxes());
        Self {
            tick: region.sched.tick,
            dimension: region.dimension,
            columns: &region.columns,
            halo: &region.border.halo,
            lighting: &region.light.lighting,
            far_halves: &region.sim.redstone.far_halves,
            viewers: &region.menus.viewers,
            mob_openers: &region.menus.mob_openers,
            pois: &region.chunk_io.pois,
            shared: &region.shared,
            far,
            state,
            pistons,
            parts,
            sky_darken: crate::world::sky_darken(shared, dimension),
            ambient_light: dimension.kind.ambient_light,
            monsters_burn: crate::world::monsters_burn(shared, dimension),
            raining: shared.weather.borrow().rain_level > 0.2,
            players_in_dimension: shared.players.borrow().in_dimension(region.dimension),
            min_y: dimension.shape.min_y,
            max_y: dimension.shape.min_y + dimension.shape.height - 1,
        }
    }

    /// Runs `f` with a tick context over the frame. `ticking` is the
    /// entity whose own parts it should not see.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::world::region) fn run<R>(
        &self,
        summaries: &[EntitySummary],
        grid: Option<&EntityGrid>,
        players: &[NearbyPlayer],
        budget: &mut u32,
        rng: &mut rand::rngs::StdRng,
        ticking: Option<i32>,
        f: impl FnOnce(&mut TickContext) -> R,
    ) -> (R, Vec<Effect>) {
        let far = self.far;
        let tick = self.tick;
        let (columns, halo, lighting) = (self.columns, self.halo, self.lighting);
        let last = std::cell::Cell::new(None::<(ChunkPos, &tuff_world_model::chunk::Chunk)>);
        let block = |pos: BlockPos| {
            let chunk = ChunkPos::of_block(pos);
            let found = match last.get() {
                Some((at, found)) if at == chunk => found,
                _ => {
                    let found = columns
                        .get(&chunk)
                        .map(|c| &c.chunk)
                        .or_else(|| halo.get(&chunk))
                        .or_else(|| far.chunk(chunk, tick))?;
                    last.set(Some((chunk, found)));
                    found
                }
            };
            Some(found.get(pos.x, pos.y, pos.z))
        };
        let light = |pos: BlockPos| lighting.as_ref()?.light_at(pos.x, pos.y, pos.z);
        let biomes = self.shared.dimensions.biomes();
        let biome = |pos: BlockPos| {
            let chunk = ChunkPos::of_block(pos);
            columns
                .get(&chunk)
                .map(|c| &c.chunk)
                .or_else(|| halo.get(&chunk))
                .map(|c| biomes.name(c.biome(pos.x, pos.y, pos.z)))
        };
        let far_halves = self.far_halves;
        let container =
            |pos: BlockPos| super::super::halves::merged_view(columns, halo, far_halves, pos);
        let (viewers, mob_openers) = (self.viewers, self.mob_openers);
        let block_loot = self.shared.block_loot();
        let container_info = |pos: BlockPos| {
            let locked = columns
                .get(&ChunkPos::of_block(pos))
                .and_then(|c| c.meta.block_entities.get(&pos))
                .and_then(|entity| {
                    entity
                        .extra
                        .get(crate::world::block_entity::raw::LOCK)
                        .and_then(tuff_nbt::Tag::as_compound)
                })
                .is_some_and(|lock| !block_loot.lock_admits(lock, None));
            let openers =
                viewers.get(&pos).map_or(0, Vec::len) + mob_openers.get(&pos).map_or(0, Vec::len);
            crate::world::entity::ContainerInfo { locked, openers }
        };
        let inhabited = |pos: ChunkPos| {
            columns
                .get(&pos)
                .map_or(0, |c| c.inhabited_time.max(0) as u64)
        };
        let own_parts;
        let parts = match ticking {
            Some(id) if self.parts.iter().any(|p| p.owner == id) => {
                own_parts = self
                    .parts
                    .iter()
                    .filter(|p| p.owner != id)
                    .cloned()
                    .collect::<Vec<_>>();
                &own_parts[..]
            }
            _ => &self.parts[..],
        };
        let mut effects = Vec::new();
        let mut context = TickContext {
            tick,
            dimension: self.dimension,
            block: &block,
            light: &light,
            sky_darken: self.sky_darken,
            ambient_light: self.ambient_light,
            monsters_burn: self.monsters_burn,
            raining: self.raining,
            biome: &biome,
            container: &container,
            container_info: &container_info,
            inhabited_time: &inhabited,
            min_y: self.min_y,
            max_y: self.max_y,
            players,
            players_in_dimension: self.players_in_dimension,
            entities: summaries,
            entity_grid: grid,
            pistons: &self.pistons,
            parts,
            shared: self.shared,
            rng,
            effects: &mut effects,
            path_budget: budget,
            pois: self.pois,
            state: self.state,
        };
        let result = f(&mut context);
        (result, effects)
    }
}

impl Frame<'_> {
    /// One entity's own tick; see `RegionActor::compute_entity`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn compute(
        &self,
        tracked: &mut TrackedEntity,
        in_rain: bool,
        summaries: &[EntitySummary],
        grid: &EntityGrid,
        nearby: &[NearbyPlayer],
        seed: u64,
        credit: PathCredit,
    ) -> Computed {
        let id = tracked.id();
        let _span = tuff_timings::span!(
            "entity",
            tuff_timings::Tag::Name(tracked.entity.base().kind.name())
        );
        let from = tracked.chunk();
        let before = tracked.entity.base().position;
        let mut data_changed = false;
        let riders_before = tracked.entity.base().riders.len();
        let holder_before = tracked.entity.leash().and_then(|l| l.holder);
        let base = {
            let (columns, halo) = (self.columns, self.halo);
            let block = |pos| block_at(columns, halo, pos);
            entity::base_tick(&mut *tracked.entity, &mut data_changed, in_rain, &block)
        };
        // `base_tick` only sees fire changes it makes itself (lava, fire
        // contact, burnout). A species' own `tick` (e.g. sun burn in
        // `ai_step`) can also set or clear fire, so the edge must be
        // re-checked across that call too, or the flag never reaches
        // viewers until something else happens to resend it.
        let flags_before_tick = tracked.entity.base().flags();
        let (outcome, effects) = if base.destroyed {
            let position = tracked.entity.base().position;
            let spill = tracked
                .entity
                .spilled_contents()
                .into_iter()
                .map(|stack| Effect::DropItem {
                    stack,
                    position,
                    velocity: [0.0; 3],
                    pickup_delay: 10,
                })
                .collect();
            (Tick::Discard, spill)
        } else {
            tracked.ticks += 1;
            let mut budget = tracked
                .path_credit
                .saturating_add(credit.share)
                .min(credit.cap.max(credit.share));
            let mut rng = entity_rng(seed, id);
            let result = self.run(
                summaries,
                Some(grid),
                nearby,
                &mut budget,
                &mut rng,
                Some(id),
                |cx| tracked.entity.tick(cx),
            );
            tracked.path_credit = budget;
            result
        };
        data_changed |= tracked.entity.base().flags() != flags_before_tick;
        Computed {
            from,
            before,
            data_changed,
            riders_before,
            holder_before,
            base,
            outcome,
            effects,
        }
    }
}
