//! Multiface growth (glow lichen, sculk veins): one face on a nearby wall,
//! floor or ceiling, sometimes spreading one more face.

use std::sync::Arc;

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState, Support};

use crate::feature::level::Region;
use crate::feature::predicate::{DIRECTIONS, Fluid, fluid, step};
use crate::feature::tree::shuffled;
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::{self, obj};
use crate::tags::BlockSet;
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:multiface_growth", |_, v| {
        let o = obj(
            v,
            &[
                "block",
                "search_range",
                "can_place_on_floor",
                "can_place_on_ceiling",
                "can_place_on_wall",
                "chance_of_spreading",
                "can_be_placed_on",
            ],
            "multiface_growth",
        )?;
        let block = json::block(o.req("block")?)?;
        if block.kind() != tuff_vanilla_data::BlockKind::Multiface {
            return Err(format!("{block:?} is not a multiface block"));
        }
        let mut directions = Vec::new();
        if o.bool_or("can_place_on_ceiling", false)? {
            directions.push(Facing::Up);
        }
        if o.bool_or("can_place_on_floor", false)? {
            directions.push(Facing::Down);
        }
        if o.bool_or("can_place_on_wall", false)? {
            directions.extend([Facing::North, Facing::East, Facing::South, Facing::West]);
        }
        Ok(Arc::new(MultifaceGrowth {
            block,
            search_range: o.i32_or("search_range", 10)?,
            chance_of_spreading: o.f32_or("chance_of_spreading", 0.5)?,
            directions,
            can_be_placed_on: json::block_set(o.req("can_be_placed_on")?)?,
        }))
    });
}

struct MultifaceGrowth {
    block: Block,
    search_range: i32,
    chance_of_spreading: f32,
    directions: Vec<Facing>,
    can_be_placed_on: BlockSet,
}

fn is_air_or_water(state: BlockState) -> bool {
    is_air(state) || state.block() == Block::Water
}

impl Feature for MultifaceGrowth {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let at_origin = ctx.level.block(origin);
        if !is_air_or_water(at_origin) {
            return false;
        }
        let search = shuffled(&self.directions, ctx.random);
        if self.place_if_possible(ctx, origin, at_origin, &search) {
            return true;
        }
        for &direction in &search {
            let allowed: Vec<Facing> = self
                .directions
                .iter()
                .copied()
                .filter(|d| *d != direction.opposite())
                .collect();
            let placement = shuffled(&allowed, ctx.random);
            for _ in 0..self.search_range {
                let pos = step(origin, direction);
                let state = ctx.level.block(pos);
                if !is_air_or_water(state) && state.block() != self.block {
                    break;
                }
                if self.place_if_possible(ctx, pos, state, &placement) {
                    return true;
                }
            }
        }
        false
    }
}

impl MultifaceGrowth {
    fn place_if_possible(
        &self,
        ctx: &mut PlaceContext,
        pos: BlockPos,
        old: BlockState,
        directions: &[Facing],
    ) -> bool {
        let spreader = Spreader::of(self.block);
        for &direction in directions {
            let neighbour = ctx.level.block(step(pos, direction));
            if !self.can_be_placed_on.contains_state(neighbour) {
                continue;
            }
            let Some(state) = placement_state(self.block, ctx.level, old, pos, direction) else {
                return false;
            };
            ctx.level.set_block(pos, state);
            ctx.level.mark_post_processing(pos);
            if ctx.random.next_float() < self.chance_of_spreading {
                spreader.spread_toward_random_direction(ctx, state, pos, direction);
            }
            return true;
        }
        false
    }
}

pub fn has_face(state: BlockState, face: Facing) -> bool {
    state.get(face.name()) == Some("true")
}

/// Whether a face pointing toward `direction` can hang on `neighbour`.
pub fn can_attach_to(neighbour: BlockState, direction: Facing) -> bool {
    let face = direction.opposite();
    neighbour.is_face_sturdy(face, Support::Full) || neighbour.is_collision_face_full(face)
}

fn is_valid_placement(
    block: Block,
    level: &Region,
    old: BlockState,
    pos: BlockPos,
    direction: Facing,
) -> bool {
    if block.default_state().get(direction.name()).is_none() {
        return false;
    }
    if old.block() == block && has_face(old, direction) {
        return false;
    }
    can_attach_to(level.block(step(pos, direction)), direction)
}

/// `old` with a face toward `direction` added, as a multiface block is
/// placed into it.
pub fn placement_state(
    block: Block,
    level: &Region,
    old: BlockState,
    pos: BlockPos,
    direction: Facing,
) -> Option<BlockState> {
    if !is_valid_placement(block, level, old, pos, direction) {
        return None;
    }
    let base = if old.block() == block {
        old
    } else if fluid(old) == Fluid::Water {
        block.default_state().with("waterlogged", "true")?
    } else {
        block.default_state()
    };
    base.with(direction.name(), "true")
}

#[derive(Clone, Copy)]
enum SpreadType {
    SamePosition,
    SamePlane,
    WrapAround,
}

impl SpreadType {
    const DEFAULT_ORDER: [SpreadType; 3] = [Self::SamePosition, Self::SamePlane, Self::WrapAround];

    fn spread_pos(self, pos: BlockPos, spread: Facing, from: Facing) -> (BlockPos, Facing) {
        match self {
            Self::SamePosition => (pos, spread),
            Self::SamePlane => (step(pos, spread), from),
            Self::WrapAround => (step(step(pos, spread), from), spread.opposite()),
        }
    }
}

/// How a multiface block spreads to neighbouring faces.
pub struct Spreader {
    block: Block,
    sculk: bool,
    types: &'static [SpreadType],
}

impl Spreader {
    pub fn of(block: Block) -> Self {
        Self {
            block,
            sculk: block == Block::SculkVein,
            types: &SpreadType::DEFAULT_ORDER,
        }
    }

    /// A spreader that only adds faces to the block it spreads from.
    pub fn same_position(block: Block) -> Self {
        Self {
            types: &[SpreadType::SamePosition],
            ..Self::of(block)
        }
    }

    fn state_can_be_replaced(
        &self,
        level: &Region,
        source: BlockPos,
        pos: BlockPos,
        direction: Facing,
        existing: BlockState,
    ) -> bool {
        let default = || {
            is_air(existing)
                || existing.block() == self.block
                || (existing.block() == Block::Water && fluid(existing) == Fluid::Water)
        };
        if !self.sculk {
            return default();
        }
        let against = level.block(step(pos, direction)).block();
        if matches!(
            against,
            Block::Sculk | Block::SculkCatalyst | Block::MovingPiston
        ) {
            return false;
        }
        let manhattan =
            (source.x - pos.x).abs() + (source.y - pos.y).abs() + (source.z - pos.z).abs();
        if manhattan == 2 {
            let neighbour = step(source, direction.opposite());
            if level
                .block(neighbour)
                .is_face_sturdy(direction, Support::Full)
            {
                return false;
            }
        }
        let fluid = fluid(existing);
        let water_or_empty = matches!(fluid, Fluid::Empty | Fluid::Water | Fluid::FlowingWater);
        if water_or_empty && !is_fire(existing.block()) {
            return existing.is_replaceable() || default();
        }
        false
    }

    fn can_spread_into(
        &self,
        level: &Region,
        source: BlockPos,
        (pos, face): (BlockPos, Facing),
    ) -> bool {
        let existing = level.block(pos);
        self.state_can_be_replaced(level, source, pos, face, existing)
            && is_valid_placement(self.block, level, existing, pos, face)
    }

    fn other_source(&self, state: BlockState) -> bool {
        self.sculk && state.block() != Block::SculkVein
    }

    fn can_spread_from(&self, state: BlockState, face: Facing) -> bool {
        self.other_source(state) || has_face(state, face)
    }

    /// Where a face of `state` at `pos` toward `from` would spread toward
    /// `spread`, if anywhere.
    fn target(
        &self,
        level: &Region,
        state: BlockState,
        pos: BlockPos,
        from: Facing,
        spread: Facing,
    ) -> Option<(BlockPos, Facing)> {
        if spread.axis() == from.axis() {
            return None;
        }
        if !self.other_source(state) && (!has_face(state, from) || has_face(state, spread)) {
            return None;
        }
        self.types
            .iter()
            .map(|t| t.spread_pos(pos, spread, from))
            .find(|target| self.can_spread_into(level, pos, *target))
    }

    fn spread_to_face(
        &self,
        level: &mut Region,
        (target, face): (BlockPos, Facing),
        post_process: bool,
    ) -> bool {
        let old = level.block(target);
        let Some(new) = placement_state(self.block, level, old, target, face) else {
            return false;
        };
        if post_process {
            level.mark_post_processing(target);
        }
        level.set_block(target, new)
    }

    /// Spreads every face of `state` toward every direction it can; the
    /// number of faces added.
    pub fn spread_all(
        &self,
        level: &mut Region,
        state: BlockState,
        pos: BlockPos,
        post_process: bool,
    ) -> usize {
        let mut count = 0;
        for from in DIRECTIONS {
            if !self.can_spread_from(state, from) {
                continue;
            }
            for spread in DIRECTIONS {
                if let Some(target) = self.target(level, state, pos, from, spread)
                    && self.spread_to_face(level, target, post_process)
                {
                    count += 1;
                }
            }
        }
        count
    }

    /// Whether a face of `state` at `pos` toward `from` can spread
    /// anywhere.
    pub fn can_spread_in_any_direction(
        &self,
        level: &Region,
        state: BlockState,
        pos: BlockPos,
        from: Facing,
    ) -> bool {
        DIRECTIONS
            .into_iter()
            .any(|spread| self.target(level, state, pos, from, spread).is_some())
    }

    /// Spreads from a random face of `state` toward a random direction,
    /// trying every face in a random order.
    pub fn spread_from_random_face(
        &self,
        ctx: &mut PlaceContext,
        state: BlockState,
        pos: BlockPos,
    ) -> bool {
        for from in shuffled(&DIRECTIONS, ctx.random) {
            if self.spread_toward_random_direction(ctx, state, pos, from) {
                return true;
            }
        }
        false
    }

    /// Tries every direction in a random order and spreads from `from` into
    /// the first that works.
    pub fn spread_toward_random_direction(
        &self,
        ctx: &mut PlaceContext,
        state: BlockState,
        pos: BlockPos,
        from: Facing,
    ) -> bool {
        for spread in shuffled(&DIRECTIONS, ctx.random) {
            if let Some(target) = self.target(ctx.level, state, pos, from, spread)
                && self.spread_to_face(ctx.level, target, true)
            {
                return true;
            }
        }
        false
    }
}

fn is_fire(block: Block) -> bool {
    tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::FIRE, block)
}
