//! Sculk patches: charge cursors wandering out from an origin, turning
//! stone into sculk, spreading veins and growing sensors and shriekers, as
//! a catalyst would during world generation.

use std::sync::Arc;

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState, Support};

use crate::feature::level::Region;
use crate::feature::multiface::{Spreader, can_attach_to, has_face};
use crate::feature::predicate::{DIRECTIONS, Fluid, fluid, step};
use crate::feature::tree::shuffled;
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::obj;
use crate::tags;
use tuff_core::world::features::{SculkCursor, SculkEvent as Event};
use tuff_item::random::{RandomSource, WorldgenRandom};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const MAX_CHARGE: i32 = 1000;
const MAX_CURSORS: usize = 32;
const MAX_SPREAD_SQUARED: i32 = 144;

/// How a spreader behaves: the patch a generator grows, or the one a
/// catalyst grows in a live world.
#[derive(Clone, Copy)]
pub struct Params {
    pub world_gen: bool,
    pub growth_spawn_cost: i32,
    pub no_growth_radius: i32,
    pub charge_decay_rate: i32,
    pub additional_decay_rate: i32,
}

impl Params {
    pub const WORLD_GEN: Self = Self {
        world_gen: true,
        growth_spawn_cost: 50,
        no_growth_radius: 1,
        charge_decay_rate: 5,
        additional_decay_rate: 10,
    };

    pub const LEVEL: Self = Self {
        world_gen: false,
        growth_spawn_cost: 10,
        no_growth_radius: 4,
        charge_decay_rate: 10,
        additional_decay_rate: 5,
    };

    fn replaceable(self) -> &'static crate::tags::BlockSet {
        if self.world_gen {
            tags::sculk_replaceable_world_gen()
        } else {
            tags::sculk_replaceable()
        }
    }
}

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:sculk_patch", |_, v| {
        let o = obj(
            v,
            &[
                "charge_count",
                "amount_per_charge",
                "spread_attempts",
                "growth_rounds",
                "spread_rounds",
            ],
            "sculk_patch",
        )?;
        Ok(Arc::new(SculkPatch {
            charge_count: o.i32("charge_count")?,
            amount_per_charge: o.i32("amount_per_charge")?,
            spread_attempts: o.i32("spread_attempts")?,
            growth_rounds: o.i32("growth_rounds")?,
            spread_rounds: o.i32("spread_rounds")?,
        }))
    });
}

struct SculkPatch {
    charge_count: i32,
    amount_per_charge: i32,
    spread_attempts: i32,
    growth_rounds: i32,
    spread_rounds: i32,
}

impl SculkPatch {
    fn can_spread_from(level: &Region, origin: BlockPos) -> bool {
        let start = level.block(origin);
        if behaviour(start) != Behaviour::Other {
            return true;
        }
        if is_air(start) || (start.block() == Block::Water && fluid(start) == Fluid::Water) {
            return DIRECTIONS
                .iter()
                .any(|d| level.block(step(origin, *d)).is_collision_full_block());
        }
        false
    }
}

impl Feature for SculkPatch {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if !Self::can_spread_from(ctx.level, origin) {
            return false;
        }
        let spreaders = Spreaders::new();
        let mut events = Vec::new();
        let mut cursors: Vec<SculkCursor> = Vec::new();
        for round in 0..self.spread_rounds + self.growth_rounds {
            for _ in 0..self.charge_count {
                let mut charge = self.amount_per_charge;
                while charge > 0 {
                    let current = charge.min(MAX_CHARGE);
                    if cursors.len() < MAX_CURSORS {
                        cursors.push(SculkCursor::new(origin, current));
                    }
                    charge -= current;
                }
            }
            let spread_veins = round < self.spread_rounds;
            for _ in 0..self.spread_attempts {
                if cursors.is_empty() {
                    continue;
                }
                cursors.retain_mut(|cursor| {
                    if cursor.is_unreasonable(origin) {
                        return false;
                    }
                    update_cursor(
                        cursor,
                        ctx.level,
                        &spreaders,
                        Params::WORLD_GEN,
                        origin,
                        ctx.random,
                        spread_veins,
                        &mut events,
                    );
                    cursor.charge > 0
                });
            }
            cursors.clear();
        }
        true
    }
}

pub struct Spreaders {
    veins: Spreader,
    same_space: Spreader,
}

impl Spreaders {
    pub fn new() -> Self {
        Self {
            veins: Spreader::of(Block::SculkVein),
            same_space: Spreader::same_position(Block::SculkVein),
        }
    }
}

impl Default for Spreaders {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    Other,
    Sculk,
    Vein,
}

fn behaviour(state: BlockState) -> Behaviour {
    match state.block() {
        Block::Sculk => Behaviour::Sculk,
        Block::SculkVein => Behaviour::Vein,
        _ => Behaviour::Other,
    }
}

pub fn faces(state: BlockState) -> Vec<Facing> {
    DIRECTIONS
        .into_iter()
        .filter(|d| has_face(state, *d))
        .collect()
}

/// The 18 neighbours sharing a face or an edge, in the game's order.
fn non_corner_neighbours() -> Vec<[i32; 3]> {
    let mut list = Vec::with_capacity(18);
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                if (x == 0 || y == 0 || z == 0) && (x, y, z) != (0, 0, 0) {
                    list.push([x, y, z]);
                }
            }
        }
    }
    list
}

struct Cursor(SculkCursor);

impl std::ops::Deref for Cursor {
    type Target = SculkCursor;

    fn deref(&self) -> &SculkCursor {
        &self.0
    }
}

impl std::ops::DerefMut for Cursor {
    fn deref_mut(&mut self) -> &mut SculkCursor {
        &mut self.0
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "independent inputs of one operation; a parameter struct would be built once"
)]
pub fn update_cursor(
    cursor: &mut SculkCursor,
    level: &mut Region,
    spreaders: &Spreaders,
    params: Params,
    origin: BlockPos,
    random: &mut WorldgenRandom,
    spread_veins: bool,
    events: &mut Vec<Event>,
) {
    let mut wrapped = Cursor(cursor.clone());
    wrapped.update(
        level,
        spreaders,
        params,
        origin,
        random,
        spread_veins,
        events,
    );
    *cursor = wrapped.0;
}

impl Cursor {
    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn update(
        &mut self,
        level: &mut Region,
        spreaders: &Spreaders,
        params: Params,
        origin: BlockPos,
        random: &mut WorldgenRandom,
        spread_veins: bool,
        events: &mut Vec<Event>,
    ) {
        if self.charge <= 0 {
            return;
        }
        if self.update_delay > 0 {
            self.update_delay -= 1;
            return;
        }
        let mut state = level.block(self.pos);
        let mut kind = behaviour(state);
        if spread_veins && self.attempt_spread_vein(level, spreaders, params, kind, state) {
            if kind != Behaviour::Sculk {
                state = level.block(self.pos);
                kind = behaviour(state);
            }
            events.push(Event::Spread(self.pos));
        }
        self.charge = self.attempt_use_charge(
            level,
            spreaders,
            params,
            kind,
            origin,
            random,
            spread_veins,
            events,
        );
        if self.charge <= 0 {
            on_discharged(level, kind, state, self.pos);
            return;
        }
        match valid_movement_pos(level, params, self.pos, random, origin) {
            Some(target) => {
                on_discharged(level, kind, state, self.pos);
                self.pos = target;
                state = level.block(target);
            }
            None if params.world_gen => {
                on_discharged(level, kind, state, self.pos);
                self.charge = 0;
                return;
            }
            None => {}
        }
        if behaviour(state) != Behaviour::Other {
            self.facings = Some(faces(state));
        }
        self.decay_delay = match kind {
            Behaviour::Other => (self.decay_delay - 1).max(0),
            _ => 1,
        };
        self.update_delay = 1;
    }

    fn attempt_spread_vein(
        &self,
        level: &mut Region,
        spreaders: &Spreaders,
        params: Params,
        kind: Behaviour,
        state: BlockState,
    ) -> bool {
        if kind == Behaviour::Other {
            match &self.facings {
                None => {
                    let current = level.block(self.pos);
                    return spreaders.same_space.spread_all(
                        level,
                        current,
                        self.pos,
                        params.world_gen,
                    ) > 0;
                }
                Some(facings) if !facings.is_empty() => {
                    if is_air(state) || fluid(state) == Fluid::Water {
                        return regrow(level, self.pos, state, facings);
                    }
                    return false;
                }
                Some(_) => {}
            }
        }
        spreaders
            .veins
            .spread_all(level, state, self.pos, params.world_gen)
            > 0
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn attempt_use_charge(
        &self,
        level: &mut Region,
        spreaders: &Spreaders,
        params: Params,
        kind: Behaviour,
        origin: BlockPos,
        random: &mut WorldgenRandom,
        spread_veins: bool,
        events: &mut Vec<Event>,
    ) -> i32 {
        match kind {
            Behaviour::Other => {
                if self.decay_delay > 0 {
                    self.charge
                } else {
                    0
                }
            }
            Behaviour::Sculk => self.sculk_use_charge(level, params, origin, random, events),
            Behaviour::Vein => {
                if spread_veins
                    && attempt_place_sculk(level, spreaders, params, self.pos, random, events)
                {
                    self.charge - 1
                } else if random.next_int_bounded(params.charge_decay_rate) == 0 {
                    (self.charge as f32 * 0.5).floor() as i32
                } else {
                    self.charge
                }
            }
        }
    }

    fn sculk_use_charge(
        &self,
        level: &mut Region,
        params: Params,
        origin: BlockPos,
        random: &mut WorldgenRandom,
        events: &mut Vec<Event>,
    ) -> i32 {
        let charge = self.charge;
        if charge == 0 || random.next_int_bounded(params.charge_decay_rate) != 0 {
            return charge;
        }
        let pos = self.pos;
        let (dx, dy, dz) = (pos.x - origin.x, pos.y - origin.y, pos.z - origin.z);
        let dist_sqr =
            (dx as f64) * (dx as f64) + (dy as f64) * (dy as f64) + (dz as f64) * (dz as f64);
        let close = dist_sqr < (params.no_growth_radius * params.no_growth_radius) as f64;
        if close || !can_place_growth(level, pos) {
            if random.next_int_bounded(params.additional_decay_rate) != 0 {
                return charge;
            }
            let penalty = if close {
                1
            } else {
                let outer = dist_sqr.sqrt() as f32 - params.no_growth_radius as f32;
                let outer = outer * outer;
                let reach = (24 - params.no_growth_radius) * (24 - params.no_growth_radius);
                let factor = (outer / reach as f32).min(1.0);
                1.max((charge as f32 * factor * 0.5) as i32)
            };
            return charge - penalty;
        }
        if random.next_int_bounded(params.growth_spawn_cost) < charge {
            let above = step(pos, Facing::Up);
            let growth = if random.next_int_bounded(11) == 0 {
                let s = Block::SculkShrieker.default_state();
                if params.world_gen {
                    s.with("can_summon", "true").unwrap_or(s)
                } else {
                    s
                }
            } else {
                Block::SculkSensor.default_state()
            };
            let growth = if fluid(level.block(above)) != Fluid::Empty {
                growth.with("waterlogged", "true").unwrap_or(growth)
            } else {
                growth
            };
            level.set_block(above, growth);
            events.push(Event::Grew(above, growth.block()));
        }
        0.max(charge - params.growth_spawn_cost)
    }
}

fn can_place_growth(level: &Region, pos: BlockPos) -> bool {
    let above = level.block(step(pos, Facing::Up));
    if !is_air(above) && !(above.block() == Block::Water && fluid(above) == Fluid::Water) {
        return false;
    }
    let inhibitors = tags::sculk_growth_inhibitors();
    let mut count = 0;
    for y in pos.y..=pos.y + 2 {
        for z in pos.z - 4..=pos.z + 4 {
            for x in pos.x - 4..=pos.x + 4 {
                if inhibitors.contains_state(level.block(BlockPos::new(x, y, z))) {
                    count += 1;
                    if count > 2 {
                        return false;
                    }
                }
            }
        }
    }
    true
}

fn regrow(level: &mut Region, pos: BlockPos, existing: BlockState, facings: &[Facing]) -> bool {
    let mut state = Block::SculkVein.default_state();
    let mut any = false;
    for &face in facings {
        if can_attach_to(level.block(step(pos, face)), face) {
            state = state.with(face.name(), "true").unwrap_or(state);
            any = true;
        }
    }
    if !any {
        return false;
    }
    if fluid(existing) != Fluid::Empty {
        state = state.with("waterlogged", "true").unwrap_or(state);
    }
    level.set_block(pos, state);
    true
}

fn on_discharged(level: &mut Region, kind: Behaviour, state: BlockState, pos: BlockPos) {
    if kind != Behaviour::Vein || state.block() != Block::SculkVein {
        return;
    }
    let mut state = state;
    for direction in DIRECTIONS {
        if has_face(state, direction) && level.block(step(pos, direction)).block() == Block::Sculk {
            state = state.with(direction.name(), "false").unwrap_or(state);
        }
    }
    if faces(state).is_empty() {
        state = if fluid(level.block(pos)) == Fluid::Empty {
            Block::Air.default_state()
        } else {
            Block::Water.default_state()
        };
    }
    level.set_block(pos, state);
}

fn attempt_place_sculk(
    level: &mut Region,
    spreaders: &Spreaders,
    params: Params,
    pos: BlockPos,
    random: &mut WorldgenRandom,
    events: &mut Vec<Event>,
) -> bool {
    let state = level.block(pos);
    for support in shuffled(&DIRECTIONS, random) {
        if !has_face(state, support) {
            continue;
        }
        let support_pos = step(pos, support);
        if !params
            .replaceable()
            .contains_state(level.block(support_pos))
        {
            continue;
        }
        let sculk = Block::Sculk.default_state();
        level.set_block(support_pos, sculk);
        events.push(Event::Spread(support_pos));
        spreaders
            .veins
            .spread_all(level, sculk, support_pos, params.world_gen);
        let skip = support.opposite();
        for direction in DIRECTIONS {
            if direction == skip {
                continue;
            }
            let vein_pos = step(support_pos, direction);
            let vein = level.block(vein_pos);
            if vein.block() == Block::SculkVein {
                on_discharged(level, Behaviour::Vein, vein, vein_pos);
            }
        }
        return true;
    }
    false
}

fn has_substrate_access(level: &Region, state: BlockState, pos: BlockPos) -> bool {
    state.block() == Block::SculkVein
        && DIRECTIONS.iter().any(|d| {
            has_face(state, *d)
                && tags::sculk_replaceable().contains_state(level.block(step(pos, *d)))
        })
}

fn is_unobstructed(level: &Region, from: BlockPos, direction: Facing) -> bool {
    !level
        .block(step(from, direction))
        .is_face_sturdy(direction.opposite(), Support::Full)
}

fn is_movement_unobstructed(level: &Region, from: BlockPos, to: BlockPos) -> bool {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    if dx.abs() + dy.abs() + dz.abs() == 1 {
        return true;
    }
    let x = if dx < 0 { Facing::West } else { Facing::East };
    let y = if dy < 0 { Facing::Down } else { Facing::Up };
    let z = if dz < 0 { Facing::North } else { Facing::South };
    if dx == 0 {
        is_unobstructed(level, from, y) || is_unobstructed(level, from, z)
    } else if dy == 0 {
        is_unobstructed(level, from, x) || is_unobstructed(level, from, z)
    } else {
        is_unobstructed(level, from, x) || is_unobstructed(level, from, y)
    }
}

fn valid_movement_pos(
    level: &Region,
    params: Params,
    pos: BlockPos,
    random: &mut WorldgenRandom,
    origin: BlockPos,
) -> Option<BlockPos> {
    let mut found = pos;
    for [ox, oy, oz] in shuffled(&non_corner_neighbours(), random) {
        let neighbour = BlockPos::new(pos.x + ox, pos.y + oy, pos.z + oz);
        let (dx, dz) = (origin.x - neighbour.x, origin.z - neighbour.z);
        if params.world_gen && dx * dx + dz * dz > MAX_SPREAD_SQUARED {
            continue;
        }
        let state = level.block(neighbour);
        if behaviour(state) != Behaviour::Other && is_movement_unobstructed(level, pos, neighbour) {
            found = neighbour;
            if has_substrate_access(level, state, neighbour) {
                break;
            }
        }
    }
    (found != pos).then_some(found)
}
