//! Lightning: the bolt entity a thunderstorm's strike spawns, and what it
//! does when it lands (damage, ignition, charging a creeper). Where a
//! region rolls for a strike and picks its target lives in
//! [`tuff_core::world::region`]; this module is the effect itself, shared by
//! natural strikes and `/summon minecraft:lightning_bolt`.

use rand::Rng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::EntityType;

use tuff_core::world::entity::{
    Effect, EntityBase, EntityHit, Notice, RegionEntity, Tick, TickContext,
};
use tuff_core::world::messages::AdvancementEvent;
use tuff_core::world::predicate::EntityFacts;
use tuff_nbt::Compound;
use tuff_vanilla_common::placement;
use tuff_world_model::BlockPos;

/// Vanilla's odds: about one roll in this many per ticking chunk per tick
/// while it is thundering there.
pub const STRIKE_CHANCE: i32 = 100_000;
/// How far a strike hurts, ignites and charges things: horizontally, and
/// how much farther up the box reaches.
const DAMAGE_RADIUS: f64 = 3.0;
const DAMAGE_HEIGHT: f64 = 6.0;
const DAMAGE: f32 = 5.0;
const IGNITE_SECONDS: f32 = 8.0;
/// Advancement witnesses: players farther than this from the strike never
/// hear about it.
const ADVANCEMENT_RADIUS: f64 = 256.0;
/// Ticks the bolt stays around; the client synthesizes its own flash and
/// thunder sound purely from seeing the entity spawn, so this only needs
/// to outlive that.
const LIFE: i32 = 2;

pub fn should_strike(rng: &mut impl Rng) -> bool {
    rng.gen_range(0..STRIKE_CHANCE) == 0
}

/// Picks one of the living things standing over `center` (vanilla prefers
/// hitting something alive that can see the sky), or `center` itself when
/// there is none.
pub fn pick_target(center: BlockPos, candidates: &[BlockPos], rng: &mut impl Rng) -> BlockPos {
    if candidates.is_empty() {
        center
    } else {
        candidates[rng.gen_range(0..candidates.len())]
    }
}

/// A lightning bolt: applies its strike once, on its first tick, then
/// lingers briefly before despawning.
#[derive(Debug)]
pub struct LightningBolt {
    base: EntityBase,
    life: i32,
    flashes: Option<i32>,
    visual_only: bool,
    fires: i32,
    hit: Vec<EntityFacts>,
}

impl LightningBolt {
    /// A bolt landing at `target` (the block it strikes), positioned at
    /// that block's bottom centre as the client renders it.
    pub fn new(id: i32, uuid: u128, target: BlockPos) -> Self {
        let position = [
            target.x as f64 + 0.5,
            target.y as f64,
            target.z as f64 + 0.5,
        ];
        let base = EntityBase::new(EntityType::LightningBolt, id, uuid, position, [0.0; 3]);
        Self::build(base)
    }

    fn build(mut base: EntityBase) -> Self {
        base.no_gravity = true;
        base.invulnerable = true;
        Self {
            base,
            life: LIFE,
            flashes: None,
            visual_only: false,
            fires: 0,
            hit: Vec::new(),
        }
    }

    /// A bolt that only flashes: it lights no fire and hurts nothing.
    pub fn visual(id: i32, uuid: u128, target: BlockPos) -> Self {
        Self {
            visual_only: true,
            ..Self::new(id, uuid, target)
        }
    }

    fn strike(&mut self, cx: &mut TickContext) {
        if !self.visual_only
            && matches!(
                *cx.shared.difficulty.borrow(),
                tuff_core::world::player::Difficulty::Normal
                    | tuff_core::world::player::Difficulty::Hard
            )
        {
            self.fires = self.ignite_ground(cx, 4);
        }
        if !self.visual_only {
            self.hit = self.hurt_nearby(cx);
        }
        let [x, y, z] = self.base.position;
        let struck = BlockPos::new(
            x.floor() as i32,
            (y - 1.0e-6).floor() as i32,
            z.floor() as i32,
        );
        cx.effects.push(Effect::LightningStrike { pos: struck });
        clear_copper(struck, cx);
    }

    /// Lights fire at the strike itself, plus up to four nearby offsets,
    /// as vanilla's difficulty-gated `spawnFire` does.
    fn ignite_ground(&self, cx: &mut TickContext, extra: usize) -> i32 {
        let mut lit = 0;
        let air = tuff_vanilla_data::Block::Air.default_state();
        let block_at = |p: BlockPos| (cx.block)(p).unwrap_or(air);
        let strike = BlockPos::new(
            self.base.position[0].floor() as i32,
            self.base.position[1].floor() as i32,
            self.base.position[2].floor() as i32,
        );
        let mut positions = vec![strike];
        for _ in 0..extra {
            positions.push(BlockPos::new(
                strike.x + cx.rng.gen_range(-1..=1),
                strike.y + cx.rng.gen_range(-1..=1),
                strike.z + cx.rng.gen_range(-1..=1),
            ));
        }
        for pos in positions {
            if !tuff_world_model::chunk::is_air(block_at(pos)) {
                continue;
            }
            let fire = placement::fire_state(pos, &block_at);
            if placement::can_survive(fire, pos, &block_at) {
                cx.effects.push(Effect::SetBlock { pos, state: fire });
                lit += 1;
            }
        }
        lit
    }

    /// Hurts and ignites living things in the strike's box, and charges
    /// any creeper (or scrapes any copper golem) there. A villager turns
    /// into a witch and a pig into a zombified piglin instead of getting
    /// hurt, unless in peaceful.
    fn hurt_nearby(&self, cx: &mut TickContext) -> Vec<EntityFacts> {
        let inside = self.strike_box();
        let peaceful =
            *cx.shared.difficulty.borrow() == tuff_core::world::player::Difficulty::Peaceful;
        let converts =
            |kind: EntityType| matches!(kind, EntityType::Villager | EntityType::Pig) && !peaceful;
        let swaps = |kind: EntityType| kind == EntityType::Mooshroom;
        let facts_at = |kind: EntityType, [x0, y0, z0, x1, _, z1]: [f64; 6]| EntityFacts {
            kind: Some(kind),
            position: [(x0 + x1) / 2.0, y0, (z0 + z1) / 2.0],
            ..EntityFacts::default()
        };
        let targets: Vec<(i32, EntityFacts)> = cx
            .players
            .iter()
            .filter(|p| inside(p.bounds()))
            .map(|p| (p.id, facts_at(EntityType::Player, p.bounds())))
            .chain(
                cx.entities
                    .iter()
                    .filter(|e| {
                        e.living.is_some_and(|l| l.alive)
                            && inside(e.bounds)
                            && !converts(e.kind)
                            && !swaps(e.kind)
                    })
                    .map(|e| (e.id, facts_at(e.kind, e.bounds))),
            )
            .collect();
        let mut struck = Vec::with_capacity(targets.len());
        for (target, facts) in targets {
            struck.push(facts);
            let mut hit = EntityHit::new(DAMAGE, &damage_type::LIGHTNING_BOLT);
            hit.projectile = Some(EntityType::LightningBolt);
            hit.position = Some(self.base.position);
            hit.ignite = IGNITE_SECONDS;
            cx.effects.push(Effect::Hurt {
                target,
                hit: Box::new(hit),
            });
        }
        let creepers: Vec<i32> = cx
            .entities
            .iter()
            .filter(|e| {
                (matches!(e.kind, EntityType::Creeper | EntityType::CopperGolem)
                    || converts(e.kind)
                    || swaps(e.kind))
                    && inside(e.bounds)
            })
            .map(|e| e.id)
            .collect();
        for target in creepers {
            cx.effects.push(Effect::Notify {
                target,
                notice: Notice::Charged,
            });
        }
        let cushions: Vec<i32> = cx
            .entities
            .iter()
            .filter(|e| e.kind == EntityType::Cushion && inside(e.bounds))
            .map(|e| e.id)
            .collect();
        for target in cushions {
            cx.effects.push(Effect::Notify {
                target,
                notice: Notice::Struck,
            });
        }
        struck
    }

    fn strike_box(&self) -> impl Fn([f64; 6]) -> bool {
        let [x, y, z] = self.base.position;
        let area = [
            x - DAMAGE_RADIUS,
            y - DAMAGE_RADIUS,
            z - DAMAGE_RADIUS,
            x + DAMAGE_RADIUS,
            y + DAMAGE_HEIGHT + DAMAGE_RADIUS,
            z + DAMAGE_RADIUS,
        ];
        move |b: [f64; 6]| {
            area[0] < b[3]
                && area[3] > b[0]
                && area[1] < b[4]
                && area[4] > b[1]
                && area[2] < b[5]
                && area[5] > b[2]
        }
    }

    /// Tells nearby players' `LightningStrike` advancement criteria about
    /// the strike. Bystanders always come back empty: building another
    /// entity's full facts from here would need it to describe itself,
    /// which the tick context does not expose, so criteria that require a
    /// bystander simply never match (as an evaluable-but-absent field
    /// already does elsewhere in this predicate language).
    fn fire_advancement(&self, cx: &mut TickContext) {
        let lightning = self.facts();
        for player in cx.players {
            if tuff_core::world::distance_sq(player.position, self.base.position)
                > ADVANCEMENT_RADIUS * ADVANCEMENT_RADIUS
            {
                continue;
            }
            cx.effects.push(Effect::Advancement {
                player: player.uuid,
                event: Box::new(AdvancementEvent::LightningStrike {
                    lightning: lightning.clone(),
                    bystanders: Vec::new(),
                }),
            });
        }
    }
}

/// The electric spark particles a strike leaves on scraped copper.
const LEVEL_EVENT_ELECTRIC_SPARK: i32 = 3002;

/// Lightning striking copper (a copper lightning rod) scrapes it back to
/// bare copper, then walks a few random paths through the copper around
/// it, scraping each block it steps on one stage back.
fn clear_copper(struck: BlockPos, cx: &mut TickContext) {
    let Some(state) = (cx.block)(struck) else {
        return;
    };
    let weathering = state.block().weathering().is_some();
    let waxed = state.block().unwaxed().is_some();
    if !weathering && !waxed {
        return;
    }
    let mut changed: std::collections::HashMap<BlockPos, tuff_vanilla_data::BlockState> =
        std::collections::HashMap::new();
    if weathering {
        let mut first = state.block();
        while let Some(younger) = first.unweathered() {
            first = younger;
        }
        let mut scraped = tuff_vanilla_data::growth::with_properties_of(first, state);
        if tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::LIGHTNING_RODS, first) {
            scraped = scraped.with("powered", "true").unwrap_or(scraped);
        }
        changed.insert(struck, scraped);
        cx.effects.push(Effect::PlaceFallenBlock {
            pos: struck,
            state: scraped,
            data: None,
        });
    }
    let walks = cx.rng.gen_range(0..3) + 3;
    for _ in 0..walks {
        let steps = cx.rng.gen_range(0..8) + 1;
        let mut at = struck;
        for _ in 0..steps {
            let Some(next) = scrape_step(at, &mut changed, cx) else {
                break;
            };
            at = next;
        }
    }
}

/// One step of a scraping walk: the first of ten random blocks around
/// `from` that is weathering copper, scraped one stage back.
fn scrape_step(
    from: BlockPos,
    changed: &mut std::collections::HashMap<BlockPos, tuff_vanilla_data::BlockState>,
    cx: &mut TickContext,
) -> Option<BlockPos> {
    for _ in 0..10 {
        let candidate = BlockPos::new(
            from.x + cx.rng.gen_range(-1..=1),
            from.y + cx.rng.gen_range(-1..=1),
            from.z + cx.rng.gen_range(-1..=1),
        );
        let Some(state) = changed
            .get(&candidate)
            .copied()
            .or_else(|| (cx.block)(candidate))
        else {
            continue;
        };
        if state.block().weathering().is_none() {
            continue;
        }
        if let Some(younger) = state.block().unweathered() {
            let scraped = tuff_vanilla_data::growth::with_properties_of(younger, state);
            changed.insert(candidate, scraped);
            cx.effects.push(Effect::SetBlock {
                pos: candidate,
                state: scraped,
            });
        }
        cx.effects
            .push(Effect::ChunkPackets(tuff_core::world::packets([
                tuff_core::world::framed(&tuff_core::protocol::play::LevelEvent {
                    event: LEVEL_EVENT_ELECTRIC_SPARK,
                    position: candidate,
                    data: -1,
                    global: false,
                }),
            ])));
        return Some(candidate);
    }
    None
}

impl RegionEntity for LightningBolt {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn facts(&self) -> EntityFacts {
        EntityFacts {
            kind: Some(EntityType::LightningBolt),
            position: self.base.position,
            blocks_set_on_fire: Some(self.fires),
            entity_struck: self.hit.clone(),
            ..EntityFacts::default()
        }
    }

    fn save(&self, _out: &mut Compound) {}

    fn display_name(&self) -> String {
        "Lightning Bolt".to_owned()
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        let flashes = *self.flashes.get_or_insert_with(|| cx.rng.gen_range(1..=3));
        if self.life == LIFE {
            self.strike(cx);
        }
        self.life -= 1;
        if self.life < 0 {
            if flashes == 0 {
                self.fire_advancement(cx);
                return Tick::Discard;
            }
            if self.life < -cx.rng.gen_range(0..10) {
                self.flashes = Some(flashes - 1);
                self.life = 1;
                if !self.visual_only
                    && matches!(
                        *cx.shared.difficulty.borrow(),
                        tuff_core::world::player::Difficulty::Normal
                            | tuff_core::world::player::Difficulty::Hard
                    )
                {
                    self.ignite_ground(cx, 0);
                }
            }
        }
        Tick::Alive
    }
}

/// Loads a bolt: `/summon minecraft:lightning_bolt` and, if the game ever
/// saves one mid-strike, reload. There is nothing type-specific to read.
pub fn load(base: EntityBase, _nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    Some(Box::new(LightningBolt::build(base)))
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;
    use tuff_core::world::dimension::DimensionId;
    use tuff_core::world::entity::{EntitySummary, LivingSummary, NearbyPlayer};
    use tuff_core::world::poi::PoiStore;

    #[tuff_core::watchdog]
    #[test]
    fn strike_chance_matches_vanillas_one_in_a_hundred_thousand() {
        let mut rng = StdRng::seed_from_u64(7);
        let hits = (0..2_000_000).filter(|_| should_strike(&mut rng)).count();
        // Expected value 20 over this many draws; a generous band keeps
        // this from flaking while still catching a wrong constant.
        assert!((5..=60).contains(&hits), "hits: {hits}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn pick_target_prefers_a_candidate_over_the_surface() {
        let mut rng = StdRng::seed_from_u64(1);
        let center = BlockPos::new(5, 70, 5);
        assert_eq!(pick_target(center, &[], &mut rng), center);
        let candidates = [BlockPos::new(1, 65, 1)];
        assert_eq!(pick_target(center, &candidates, &mut rng), candidates[0]);
    }

    fn living(id: i32, kind: EntityType, position: [f64; 3]) -> EntitySummary {
        EntitySummary {
            id,
            uuid: id as u128,
            kind,
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
                eye_height: 1.6,
                in_water: false,
                seen_as_enemy: true,
                enemy: false,
                health: 20.0,
                tamed: false,
                owner: None,
                chasing: None,
            }),
            pickable: false,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    fn player(id: i32, position: [f64; 3]) -> NearbyPlayer<'static> {
        NearbyPlayer {
            id,
            uuid: id as u128,
            name: "tester",
            position,
            eye_height: 1.6,
            yaw: 0.0,
            pitch: 0.0,
            held: [None, None],
            creative: false,
            height: 1.8,
            sneaking: false,
            invisible: false,
            invulnerable: false,
            armor: [None; 4],
            effects: 0,
            movement: [0.0; 3],
            vehicle: None,
            sleeping: None,
            last_hurt_by: None,
            last_hurt_mob: None,
            perchable: false,
            sprinting: false,
        }
    }

    /// Strikes at `target` with `players`/`entities` around it (a fixed
    /// ground below, so fire can survive) and returns the effects it
    /// queued.
    fn strike_effects(
        target: BlockPos,
        players: &[NearbyPlayer],
        entities: &[EntitySummary],
    ) -> Vec<Effect> {
        run_bolt(target, players, entities, 1).0
    }

    fn run_bolt(
        target: BlockPos,
        players: &[NearbyPlayer],
        entities: &[EntitySummary],
        ticks: usize,
    ) -> (Vec<Effect>, usize) {
        let shared = tuff_test_support::bare_shared();
        let block = move |pos: BlockPos| {
            Some(if pos.y < target.y {
                tuff_vanilla_data::Block::GrassBlock.default_state()
            } else {
                tuff_vanilla_data::Block::Air.default_state()
            })
        };
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = StdRng::seed_from_u64(1);
        let mut effects = Vec::new();
        let mut budget = 10_000;
        let pois = PoiStore::default();
        let mut cx = TickContext {
            tick: 1,
            dimension: DimensionId::for_tests(0),
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
            players,
            players_in_dimension: players.len(),
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
        let mut bolt = LightningBolt::new(1000, 1, target);
        let mut lived = 0;
        for _ in 0..ticks {
            lived += 1;
            if matches!(bolt.tick(&mut cx), Tick::Discard) {
                break;
            }
        }
        (effects, lived)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bolt_flashes_again_before_it_reports_to_advancements() {
        let target = BlockPos::new(0, 70, 0);
        let near = player(1, [0.5, 70.0, 0.5]);
        let advancements = |effects: &[Effect]| {
            effects
                .iter()
                .filter(|e| matches!(e, Effect::Advancement { .. }))
                .count()
        };
        let (early, lived) = run_bolt(target, &[near], &[], 3);
        assert_eq!(advancements(&early), 0);
        assert_eq!(lived, 3, "a bolt always flashes at least once more");
        let near = player(1, [0.5, 70.0, 0.5]);
        let (all, lived) = run_bolt(target, &[near], &[], 200);
        assert!(lived > 3 && lived < 200);
        assert_eq!(advancements(&all), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_strike_ignites_flammable_ground() {
        let target = BlockPos::new(0, 70, 0);
        let effects = strike_effects(target, &[], &[]);
        let fire_at_strike = effects.iter().any(|effect| {
            matches!(effect, Effect::SetBlock { pos, state }
                if *pos == target && state.block() == tuff_vanilla_data::Block::Fire)
        });
        assert!(fire_at_strike, "effects: {effects:#?}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_strike_hurts_whats_close_and_spares_whats_far() {
        let target = BlockPos::new(0, 70, 0);
        let near = player(1, [0.5, 70.0, 0.5]);
        let far = player(2, [50.0, 70.0, 50.0]);
        let effects = strike_effects(target, &[near, far], &[]);
        let hurt_ids: Vec<i32> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Hurt { target, .. } => Some(*target),
                _ => None,
            })
            .collect();
        assert_eq!(hurt_ids, vec![1], "effects: {effects:#?}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_strike_charges_a_nearby_creeper() {
        let target = BlockPos::new(0, 70, 0);
        let creeper = living(5, EntityType::Creeper, [0.5, 70.0, 0.5]);
        let effects = strike_effects(target, &[], &[creeper]);
        let charged = effects.iter().any(|effect| {
            matches!(
                effect,
                Effect::Notify {
                    target: 5,
                    notice: Notice::Charged
                }
            )
        });
        assert!(charged, "effects: {effects:#?}");
        let hurt = effects
            .iter()
            .any(|effect| matches!(effect, Effect::Hurt { target: 5, .. }));
        assert!(hurt, "the same hit that charges it also damages it");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_strike_swaps_a_mooshroom_without_hurting_it() {
        let target = BlockPos::new(0, 70, 0);
        let cow = living(6, EntityType::Mooshroom, [0.5, 70.0, 0.5]);
        let effects = strike_effects(target, &[], &[cow]);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Notify {
                target: 6,
                notice: Notice::Charged
            }
        )));
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Hurt { target: 6, .. }))
        );
    }
}
