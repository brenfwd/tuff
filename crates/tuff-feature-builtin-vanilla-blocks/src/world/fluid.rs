//! The game's fluid simulation as plain functions of a [`FluidWorld`]:
//! scheduled fluid ticks spread water and lava down and sideways toward the
//! nearest way down, levels drop with distance, sources form between
//! sources, and lava meeting water or blue ice hardens. The region applies
//! the writes, including those that land in a neighbouring region.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::fluid::{self as data, Fluid, FluidBehaviour, FluidRules, FluidState};
use tuff_vanilla_data::{Block, BlockState};

pub use tuff_core::world::contact::{box_touches, eyes_in, surface};
pub use tuff_core::world::fluid_api::{FluidWorld, Settings};
use tuff_vanilla_common::placement::{self, step};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

/// How far a spread search reaches from the ticking block, horizontally.
pub const REACH: i32 = 5;

/// What a read of an unloaded block sees: nothing passes it. Fluid ticks
/// only run where everything they can reach is loaded, so this is a guard.
const UNLOADED: BlockState = Block::Bedrock.default_state();
const AIR: BlockState = Block::Air.default_state();

/// The game's order for spreading sideways.
const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];
/// The order lava looks for water or blue ice around it: above, then the
/// sides.
const CONTACT_ORDER: [Facing; 5] = [
    Facing::Up,
    Facing::North,
    Facing::South,
    Facing::West,
    Facing::East,
];

fn washed_away(block: Block) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::block::key::WASHED_AWAY_BY_FLUIDS,
        block,
    )
}

fn blocks_flow(block: Block) -> bool {
    tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::BLOCKS_FLUID_FLOW, block)
}

/// Mushrooms need darkness unless their ground makes up for it.
const MUSHROOM_MAX_BRIGHTNESS: u8 = 12;

fn get(world: &(impl FluidWorld + ?Sized), pos: BlockPos) -> BlockState {
    world.block(pos).unwrap_or(UNLOADED)
}

fn up(pos: BlockPos) -> BlockPos {
    step(pos, Facing::Up)
}

fn down(pos: BlockPos) -> BlockPos {
    step(pos, Facing::Down)
}

fn same_above(world: &(impl FluidWorld + ?Sized), pos: BlockPos, fluid: FluidState) -> bool {
    get(world, up(pos))
        .fluid_state()
        .fluid()
        .is_same(fluid.fluid())
}

fn is_water(fluid: FluidState) -> bool {
    tuff_datapack::tags::is(tuff_vanilla_data::tag::fluid::key::WATER, fluid.fluid())
}

/// A block fluids can enter at all: one that holds liquids, or one they
/// wash away (air, plants, torches, other liquids, ...).
pub fn can_hold_any_fluid(state: BlockState) -> bool {
    state.is_liquid_container() || washed_away(state.block())
}

fn can_hold_specific(state: BlockState, fluid: Fluid) -> bool {
    !state.is_liquid_container() || state.can_place_liquid(fluid)
}

fn can_hold(state: BlockState, fluid: Fluid) -> bool {
    can_hold_any_fluid(state) && can_hold_specific(state, fluid)
}

fn is_source_of(rules: &FluidRules, fluid: FluidState) -> bool {
    fluid.is_source() && fluid.fluid().is_same(rules.fluid)
}

fn can_maybe_pass(rules: &FluidRules, from: BlockState, direction: Facing, to: BlockState) -> bool {
    !is_source_of(rules, to.fluid_state())
        && can_hold_any_fluid(to)
        && data::passes_between(from, direction, to)
}

fn can_pass(rules: &FluidRules, from: BlockState, direction: Facing, to: BlockState) -> bool {
    can_maybe_pass(rules, from, direction, to) && can_hold_specific(to, rules.flowing)
}

fn is_hole(rules: &FluidRules, top: BlockState, bottom: BlockState) -> bool {
    data::passes_between(top, Facing::Down, bottom)
        && (bottom.fluid_state().fluid().is_same(rules.fluid) || can_hold(bottom, rules.flowing))
}

fn can_convert(settings: Settings, rules: &FluidRules) -> bool {
    match rules.behaviour {
        FluidBehaviour::Water => settings.water_source_conversion,
        FluidBehaviour::Lava => settings.lava_source_conversion,
    }
}

/// The fluid state `pos` would settle to from its neighbours.
fn new_liquid(
    world: &(impl FluidWorld + ?Sized),
    rules: &FluidRules,
    pos: BlockPos,
    state: BlockState,
) -> FluidState {
    let settings = world.settings();
    let mut highest = 0;
    let mut sources = 0;
    for direction in HORIZONTAL {
        let neighbour = get(world, step(pos, direction));
        let fluid = neighbour.fluid_state();
        if fluid.fluid().is_same(rules.fluid) && data::passes_between(state, direction, neighbour) {
            if fluid.is_source() {
                sources += 1;
            }
            highest = highest.max(fluid.amount());
        }
    }
    if sources >= 2 && can_convert(settings, rules) {
        let below = get(world, down(pos));
        if below.is_solid() || is_source_of(rules, below.fluid_state()) {
            return FluidState::source(rules.fluid, false);
        }
    }
    let above = get(world, up(pos));
    let above_fluid = above.fluid_state();
    if !above_fluid.is_empty()
        && above_fluid.fluid().is_same(rules.fluid)
        && data::passes_between(state, Facing::Up, above)
    {
        return FluidState::flowing(rules.fluid, 8, true);
    }
    let amount = highest - rules.drop_off(settings.fast_lava);
    if amount <= 0 {
        FluidState::EMPTY
    } else {
        FluidState::flowing(rules.fluid, amount, false)
    }
}

fn spread_delay(
    world: &mut (impl FluidWorld + ?Sized),
    rules: &FluidRules,
    pos: BlockPos,
    old: FluidState,
    new: FluidState,
) -> u32 {
    let delay = rules.tick_delay(world.settings().fast_lava);
    if rules.behaviour != FluidBehaviour::Lava
        || old.is_empty()
        || new.is_empty()
        || old.is_falling()
        || new.is_falling()
    {
        return delay;
    }
    let rises = new.height(same_above(world, pos, new)) > old.height(same_above(world, pos, old));
    if rises && world.random(4) != 0 {
        delay * 4
    } else {
        delay
    }
}

/// Runs a scheduled tick of `kind` at `pos`: a flowing fluid settles to
/// what its neighbours give it, then the fluid spreads.
/// What a position marked during generation gets as its chunk goes live:
/// its fluid ticks once, then a block that is not a liquid takes the shape
/// its neighbours give it. The reshaped state is returned for the caller to
/// set without further updates.
pub fn post_process(world: &mut (impl FluidWorld + ?Sized), pos: BlockPos) -> Option<BlockState> {
    let state = world.block(pos)?;
    let fluid = state.fluid_state();
    if !fluid.is_empty() {
        tick(world, pos, fluid.fluid());
    }
    if state.is_liquid_block() {
        return None;
    }
    let level = |p: BlockPos| world.block(p).unwrap_or(Block::Air.default_state());
    if state.block().kind() == tuff_vanilla_data::BlockKind::Mushroom
        && !tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::OVERRIDES_MUSHROOM_LIGHT_REQUIREMENT,
            level(down(pos)).block(),
        )
        && world
            .raw_brightness(pos)
            .is_some_and(|b| b > MUSHROOM_MAX_BRIGHTNESS)
    {
        return Some(AIR);
    }
    let shaped = placement::UPDATE_ORDER
        .into_iter()
        .fold(state, |current, direction| {
            let neighbour = level(step(pos, direction));
            placement::update_shape(current, pos, direction, neighbour, &level)
        });
    (shaped != state).then_some(shaped)
}

pub fn tick(world: &mut (impl FluidWorld + ?Sized), pos: BlockPos, kind: Fluid) {
    let mut state = get(world, pos);
    let mut fluid = state.fluid_state();
    if fluid.fluid() != kind {
        return;
    }
    let Some(rules) = kind.rules() else {
        return;
    };
    if !fluid.is_source() {
        let new = new_liquid(world, rules, pos, state);
        let delay = spread_delay(world, rules, pos, fluid, new);
        if new.is_empty() {
            fluid = new;
            state = AIR;
            world.set(pos, state);
        } else if new != fluid {
            fluid = new;
            state = new.block();
            world.set(pos, state);
            world.schedule(pos, new.fluid(), delay);
        }
    }
    spread(world, rules, pos, state, fluid);
}

fn spread(
    world: &mut (impl FluidWorld + ?Sized),
    rules: &FluidRules,
    pos: BlockPos,
    state: BlockState,
    fluid: FluidState,
) {
    if fluid.is_empty() {
        return;
    }
    let widen = match pour_down(world, rules, pos, state) {
        Ok(()) => feeding_sources(world, rules, pos) >= 3,
        Err(floor) => fluid.is_source() || !is_hole(rules, state, floor),
    };
    if widen {
        flow_sideways(world, rules, pos, state, fluid);
    }
}

/// Moves fluid into the block below if it takes it; otherwise hands back
/// what is down there.
fn pour_down(
    world: &mut (impl FluidWorld + ?Sized),
    rules: &FluidRules,
    pos: BlockPos,
    state: BlockState,
) -> Result<(), BlockState> {
    let target = down(pos);
    let floor = get(world, target);
    if !can_maybe_pass(rules, state, Facing::Down, floor) {
        return Err(floor);
    }
    let incoming = new_liquid(world, rules, target, floor);
    let present = floor.fluid_state();
    let capped = same_above(world, target, present);
    if !present.can_be_replaced_with(incoming.fluid(), Facing::Down, capped)
        || !can_hold_specific(floor, incoming.fluid())
    {
        return Err(floor);
    }
    world.spread_to(target, Facing::Down, incoming);
    Ok(())
}

fn feeding_sources(world: &(impl FluidWorld + ?Sized), rules: &FluidRules, pos: BlockPos) -> usize {
    let mut count = 0;
    for side in HORIZONTAL {
        count += is_source_of(rules, get(world, step(pos, side)).fluid_state()) as usize;
    }
    count
}

/// The order side spreads are applied in (north, south, west, east), as
/// indices into `HORIZONTAL`.
const APPLY_SLOTS: [usize; 4] = [0, 2, 3, 1];

fn flow_sideways(
    world: &mut (impl FluidWorld + ?Sized),
    rules: &FluidRules,
    pos: BlockPos,
    state: BlockState,
    fluid: FluidState,
) {
    let carried = match fluid.is_falling() {
        true => 7,
        false => fluid.amount() - rules.drop_off(world.settings().fast_lava),
    };
    if carried < 1 {
        return;
    }
    let outlets = side_outlets(world, rules, pos, state);
    for slot in APPLY_SLOTS {
        if let Some(next) = outlets[slot] {
            let side = HORIZONTAL[slot];
            world.spread_to(step(pos, side), side, next);
        }
    }
}

/// Distance to a way down that none was found for.
const NO_DROP: u8 = u8::MAX;
/// Columns a side search can touch: within `REACH` of the ticking block.
const GRID: usize = (2 * REACH + 1) as usize;

/// Per-column memo of one side search. The world does not change while a
/// search runs, so this only saves reads.
struct Columns<'a, W: ?Sized> {
    world: &'a W,
    rules: &'a FluidRules,
    centre: BlockPos,
    tops: [Option<BlockState>; GRID * GRID],
    drops: [Option<bool>; GRID * GRID],
}

impl<'a, W: FluidWorld + ?Sized> Columns<'a, W> {
    fn new(world: &'a W, rules: &'a FluidRules, centre: BlockPos) -> Self {
        Self {
            world,
            rules,
            centre,
            tops: [None; GRID * GRID],
            drops: [None; GRID * GRID],
        }
    }

    fn cell(&self, pos: BlockPos) -> Option<usize> {
        let dx = pos.x - self.centre.x + REACH;
        let dz = pos.z - self.centre.z + REACH;
        let span = 0..GRID as i32;
        (span.contains(&dx) && span.contains(&dz)).then(|| dz as usize * GRID + dx as usize)
    }

    fn top(&mut self, pos: BlockPos) -> BlockState {
        match self.cell(pos) {
            Some(i) => *self.tops[i].get_or_insert_with(|| get(self.world, pos)),
            None => get(self.world, pos),
        }
    }

    fn drops_away(&mut self, pos: BlockPos) -> bool {
        let cell = self.cell(pos);
        if let Some(known) = cell.and_then(|i| self.drops[i]) {
            return known;
        }
        let hole = is_hole(self.rules, self.top(pos), get(self.world, down(pos)));
        if let Some(i) = cell {
            self.drops[i] = Some(hole);
        }
        hole
    }

    /// Fewest further steps from `start` (entered heading `entered`) to a
    /// column the fluid can fall through, never stepping straight back,
    /// up to `budget` steps; `NO_DROP` if there is none.
    fn steps_to_drop(&mut self, start: BlockPos, entered: Facing, budget: i32) -> u8 {
        let mut frontier: Vec<(BlockPos, Facing)> = Vec::with_capacity(16);
        let mut upcoming: Vec<(BlockPos, Facing)> = Vec::with_capacity(16);
        frontier.push((start, entered));
        for taken in 1..=budget {
            for &(at, heading) in &frontier {
                let here = self.top(at);
                for side in HORIZONTAL {
                    if side == heading.opposite() {
                        continue;
                    }
                    let next = step(at, side);
                    let there = self.top(next);
                    if !can_pass(self.rules, here, side, there) {
                        continue;
                    }
                    if self.drops_away(next) {
                        return taken as u8;
                    }
                    if !upcoming.contains(&(next, side)) {
                        upcoming.push((next, side));
                    }
                }
            }
            std::mem::swap(&mut frontier, &mut upcoming);
            upcoming.clear();
        }
        NO_DROP
    }
}

/// Which sides the fluid spreads to and as what, indexed like
/// `HORIZONTAL`: only the sides tied for the nearest way down within the
/// slope search distance, or every open side if none is near.
fn side_outlets(
    world: &(impl FluidWorld + ?Sized),
    rules: &FluidRules,
    pos: BlockPos,
    state: BlockState,
) -> [Option<FluidState>; 4] {
    let budget = rules.slope_find_distance(world.settings().fast_lava);
    let mut columns = Columns::new(world, rules, pos);
    let candidates = HORIZONTAL.map(|side| {
        let beside = step(pos, side);
        let there = get(world, beside);
        if !can_maybe_pass(rules, state, side, there) {
            return None;
        }
        let arriving = new_liquid(world, rules, beside, there);
        if !can_hold_specific(there, arriving.fluid()) {
            return None;
        }
        let distance = if columns.drops_away(beside) {
            0
        } else {
            columns.steps_to_drop(beside, side, budget)
        };
        let present = there.fluid_state();
        let accepted = present
            .can_be_replaced_with(arriving.fluid(), side, same_above(world, beside, present))
            .then_some(arriving);
        Some((distance, accepted))
    });
    let nearest = candidates
        .iter()
        .flatten()
        .map(|(distance, _)| *distance)
        .min();
    candidates.map(|candidate| match candidate {
        Some((distance, accepted)) if Some(distance) == nearest => accepted,
        _ => None,
    })
}

/// Puts `fluid`, flowing in `direction`, into `pos` (holding `state`).
pub fn apply_spread(
    world: &mut (impl FluidWorld + ?Sized),
    pos: BlockPos,
    state: BlockState,
    direction: Facing,
    fluid: FluidState,
) {
    let behaviour = fluid.rules().map(|rules| rules.behaviour);
    if behaviour == Some(FluidBehaviour::Lava)
        && direction == Facing::Down
        && is_water(state.fluid_state())
    {
        if state.is_liquid_block() {
            world.set(pos, Block::Stone.default_state());
        }
        world.fizz(pos);
        return;
    }
    if state.is_liquid_container() {
        if fluid.is_source()
            && let Some(placement) = state.place_liquid(fluid.fluid())
        {
            world.place_liquid(pos, placement);
        }
        return;
    }
    if !is_air(state) {
        match behaviour {
            Some(FluidBehaviour::Water) => world.wash_away(pos, state, fluid.block()),
            Some(FluidBehaviour::Lava) => world.fizz(pos),
            None => {}
        }
    }
    world.set(pos, fluid.block());
}

/// Whether a spread computed by a neighbouring region still holds where it
/// lands: the block may have changed since that region last saw it.
pub fn still_flows(
    world: &(impl FluidWorld + ?Sized),
    pos: BlockPos,
    direction: Facing,
    fluid: FluidState,
) -> bool {
    let Some(rules) = fluid.rules() else {
        return true;
    };
    let (Some(from), Some(to)) = (
        world.block(step(pos, direction.opposite())),
        world.block(pos),
    ) else {
        return false;
    };
    let to_fluid = to.fluid_state();
    can_maybe_pass(rules, from, direction, to)
        && can_hold_specific(to, fluid.fluid())
        && to_fluid.can_be_replaced_with(fluid.fluid(), direction, same_above(world, pos, to_fluid))
}

/// Lava touching water hardens (obsidian from a source, else
/// cobblestone); over soul soil, touching blue ice makes basalt. Returns
/// whether it hardened.
fn harden(world: &mut (impl FluidWorld + ?Sized), pos: BlockPos, state: BlockState) -> bool {
    let fluid = state.fluid_state();
    if fluid.rules().map(|rules| rules.behaviour) != Some(FluidBehaviour::Lava) {
        return false;
    }
    let over_soul_soil = get(world, down(pos)).block() == Block::SoulSoil;
    for direction in CONTACT_ORDER {
        let neighbour = get(world, step(pos, direction));
        if is_water(neighbour.fluid_state()) {
            let block = if fluid.is_source() {
                Block::Obsidian
            } else {
                Block::Cobblestone
            };
            world.set(pos, block.default_state());
            world.fizz(pos);
            return true;
        }
        if over_soul_soil && neighbour.block() == Block::BlueIce {
            world.set(pos, Block::Basalt.default_state());
            world.fizz(pos);
            return true;
        }
    }
    false
}

/// A liquid block was placed at `pos`, or a neighbour of it changed: it
/// hardens against water, or schedules its next spread.
pub fn liquid_changed(world: &mut (impl FluidWorld + ?Sized), pos: BlockPos, state: BlockState) {
    if !state.is_liquid_block() || harden(world, pos, state) {
        return;
    }
    schedule_own(world, pos, state);
}

/// A neighbour changed the shape of what is at `pos`: liquid blocks next to
/// a source, and any other block holding a fluid, schedule their fluid.
pub fn shape_changed(
    world: &mut (impl FluidWorld + ?Sized),
    pos: BlockPos,
    state: BlockState,
    neighbour: BlockState,
) {
    let fluid = state.fluid_state();
    if fluid.is_empty() {
        return;
    }
    if state.is_liquid_block() && !fluid.is_source() && !neighbour.fluid_state().is_source() {
        return;
    }
    schedule_own(world, pos, state);
}

fn schedule_own(world: &mut (impl FluidWorld + ?Sized), pos: BlockPos, state: BlockState) {
    let fluid = state.fluid_state();
    if let Some(rules) = fluid.rules() {
        let delay = rules.tick_delay(world.settings().fast_lava);
        world.schedule(pos, fluid.fluid(), delay);
    }
}

/// The direction the fluid at `pos` pushes things in, as the game computes
/// it (a normalized vector; falling fluid next to a wall pulls down).
pub fn flow(block: &dyn Fn(BlockPos) -> Option<BlockState>, pos: BlockPos) -> [f64; 3] {
    let get = |pos: BlockPos| block(pos).unwrap_or(UNLOADED);
    let fluid = get(pos).fluid_state();
    if fluid.is_empty() {
        return [0.0; 3];
    }
    let own = fluid.own_height();
    let affects = |f: FluidState| f.is_empty() || f.fluid().is_same(fluid.fluid());
    let (mut x, mut z) = (0.0f64, 0.0f64);
    for direction in HORIZONTAL {
        let neighbour_pos = step(pos, direction);
        let neighbour = get(neighbour_pos);
        let neighbour_fluid = neighbour.fluid_state();
        if !affects(neighbour_fluid) {
            continue;
        }
        let height = neighbour_fluid.own_height();
        let mut distance = 0.0f32;
        if height == 0.0 {
            if !blocks_flow(neighbour.block()) {
                let below = get(down(neighbour_pos)).fluid_state();
                if affects(below) && below.own_height() > 0.0 {
                    distance = own - (below.own_height() - 0.8888889);
                }
            }
        } else if height > 0.0 {
            distance = own - height;
        }
        if distance != 0.0 {
            let (dx, _, dz) = direction.step();
            x += (dx as f32 * distance) as f64;
            z += (dz as f32 * distance) as f64;
        }
    }
    let mut vector = normalize([x, 0.0, z]);
    if fluid.is_falling() {
        let solid_face = |at: BlockPos, direction: Facing| {
            let state = get(at);
            !state.fluid_state().fluid().is_same(fluid.fluid())
                && !state.is_ice()
                && state.is_face_sturdy(direction, tuff_vanilla_data::Support::Full)
        };
        if HORIZONTAL.iter().any(|d| {
            let side = step(pos, *d);
            solid_face(side, *d) || solid_face(up(side), *d)
        }) {
            vector[1] -= 6.0;
        }
    }
    normalize(vector)
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length < 1.0e-5 {
        [0.0; 3]
    } else {
        [v[0] / length, v[1] / length, v[2] / length]
    }
}

/// The vanilla fluid rules behind core's `FluidBehaviors`.
pub struct VanillaFluids;

impl tuff_core::world::fluid_api::FluidBehaviors for VanillaFluids {
    fn tick(&self, world: &mut dyn FluidWorld, pos: BlockPos, kind: Fluid) {
        tick(world, pos, kind);
    }

    fn flow(&self, block: &dyn Fn(BlockPos) -> Option<BlockState>, pos: BlockPos) -> [f64; 3] {
        flow(block, pos)
    }

    fn post_process(&self, world: &mut dyn FluidWorld, pos: BlockPos) -> Option<BlockState> {
        post_process(world, pos)
    }

    fn liquid_changed(&self, world: &mut dyn FluidWorld, pos: BlockPos, state: BlockState) {
        liquid_changed(world, pos, state);
    }

    fn shape_changed(
        &self,
        world: &mut dyn FluidWorld,
        pos: BlockPos,
        state: BlockState,
        neighbour: BlockState,
    ) {
        shape_changed(world, pos, state, neighbour);
    }

    fn still_flows(
        &self,
        world: &dyn FluidWorld,
        pos: BlockPos,
        direction: Facing,
        fluid: FluidState,
    ) -> bool {
        still_flows(world, pos, direction, fluid)
    }

    fn apply_spread(
        &self,
        world: &mut dyn FluidWorld,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
        fluid: FluidState,
    ) {
        apply_spread(world, pos, state, direction, fluid);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use tuff_core::world::ticks::{self, ChunkTicks, Scheduled};

    const STONE: BlockState = Block::Stone.default_state();

    /// A flat world of stone below y 0 and air above.
    struct World {
        blocks: HashMap<BlockPos, BlockState>,
        ticks: ChunkTicks<Fluid>,
        now: u64,
        order: i64,
        settings: Settings,
        washed: Vec<BlockPos>,
        fizzed: Vec<BlockPos>,
    }

    impl World {
        fn new() -> Self {
            Self {
                blocks: HashMap::new(),
                ticks: ChunkTicks::default(),
                now: 0,
                order: 0,
                settings: Settings {
                    fast_lava: false,
                    water_source_conversion: true,
                    lava_source_conversion: false,
                },
                washed: Vec::new(),
                fizzed: Vec::new(),
            }
        }

        fn at(&self, x: i32, y: i32, z: i32) -> BlockState {
            get(self, BlockPos::new(x, y, z))
        }

        fn put(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
            self.set(BlockPos::new(x, y, z), state);
        }

        fn amount(&self, x: i32, y: i32, z: i32) -> i32 {
            self.at(x, y, z).fluid_state().amount()
        }

        fn run(&mut self, ticks: u64) {
            for _ in 0..ticks {
                self.now += 1;
                let due = ticks::drain(&mut [&mut self.ticks], self.now, ticks::MAX_PER_TICK);
                for scheduled in due {
                    tick(self, scheduled.pos, scheduled.kind);
                }
            }
        }
    }

    impl FluidWorld for World {
        fn spread_to(&mut self, pos: BlockPos, direction: Facing, fluid: FluidState) {
            let state = get(self, pos);
            apply_spread(self, pos, state, direction, fluid);
        }

        fn block(&self, pos: BlockPos) -> Option<BlockState> {
            Some(
                self.blocks
                    .get(&pos)
                    .copied()
                    .unwrap_or(if pos.y < 0 { STONE } else { AIR }),
            )
        }

        fn settings(&self) -> Settings {
            self.settings
        }

        fn random(&mut self, _bound: u32) -> u32 {
            0
        }

        fn set(&mut self, pos: BlockPos, state: BlockState) {
            if get(self, pos) == state {
                return;
            }
            self.blocks.insert(pos, state);
            if state.is_liquid_block() {
                liquid_changed(self, pos, state);
            }
            for direction in Facing::ALL {
                let neighbour = step(pos, direction);
                let current = get(self, neighbour);
                if current.fluid_state().is_empty() {
                    continue;
                }
                liquid_changed(self, neighbour, current);
                let current = get(self, neighbour);
                shape_changed(self, neighbour, current, state);
            }
        }

        fn schedule(&mut self, pos: BlockPos, fluid: Fluid, delay: u32) {
            self.order += 1;
            self.ticks.schedule(Scheduled {
                pos,
                kind: fluid,
                due: self.now + delay as u64,
                priority: 0,
                order: self.order,
            });
        }

        fn wash_away(&mut self, pos: BlockPos, _state: BlockState, _left: BlockState) {
            self.washed.push(pos);
        }

        fn place_liquid(&mut self, pos: BlockPos, placement: &'static data::LiquidPlacement) {
            self.set(pos, placement.result);
            if let Some(fluid) = placement.tick {
                self.schedule(pos, fluid, 5);
            }
        }

        fn fizz(&mut self, pos: BlockPos) {
            self.fizzed.push(pos);
        }
    }

    fn water() -> BlockState {
        Block::Water.default_state()
    }

    fn rough_terrain(fast_lava: bool) -> World {
        let mut world = World::new();
        world.settings.fast_lava = fast_lava;
        let mut seed: u64 = 0x005e_edf1_u64;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        for x in -12..=12 {
            for z in -12..=12 {
                match next() % 11 {
                    0 => world.blocks.insert(BlockPos::new(x, 0, z), STONE),
                    1 => world.blocks.insert(BlockPos::new(x, -1, z), AIR),
                    2 => world
                        .blocks
                        .insert(BlockPos::new(x, 0, z), Block::OakFence.default_state()),
                    _ => None,
                };
            }
        }
        world.put(0, 0, 0, water());
        world.put(5, 0, 3, water());
        world.put(-4, 0, -6, lava());
        world.put(3, 2, -2, water());
        world
    }

    fn fingerprint(world: &World) -> u64 {
        let mut entries: Vec<_> = world
            .blocks
            .iter()
            .map(|(p, s)| (p.x, p.y, p.z, format!("{s:?}")))
            .collect();
        entries.sort();
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in format!("{entries:?}{:?}{:?}", world.washed, world.fizzed).bytes() {
            hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
        }
        hash
    }

    #[tuff_core::watchdog]
    #[test]
    fn rough_terrain_flow_is_pinned() {
        let mut world = rough_terrain(false);
        world.run(400);
        let mut fast = rough_terrain(true);
        fast.run(400);
        assert_eq!(
            (fingerprint(&world), fingerprint(&fast)),
            (15157165516631701237, 9454710729433605327)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "release timing run"]
    fn rough_terrain_flow_timing() {
        let start = std::time::Instant::now();
        for _ in 0..20 {
            let mut world = rough_terrain(false);
            world.run(400);
            let mut fast = rough_terrain(true);
            fast.run(400);
        }
        eprintln!("FLOW_TIMING_MS {}", start.elapsed().as_millis());
    }

    fn lava() -> BlockState {
        Block::Lava.default_state()
    }

    #[tuff_core::watchdog]
    #[test]
    fn water_spreads_seven_blocks_over_flat_ground() {
        let mut world = World::new();
        world.put(0, 0, 0, water());
        world.run(200);
        assert_eq!(world.amount(1, 0, 0), 7);
        assert_eq!(world.amount(7, 0, 0), 1);
        assert_eq!(world.amount(3, 0, -4), 1);
        assert!(world.at(8, 0, 0).fluid_state().is_empty());
        assert!(world.at(4, 0, 4).fluid_state().is_empty());
        assert!(world.at(0, 1, 0).fluid_state().is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn lava_reaches_further_where_it_is_fast() {
        let mut world = World::new();
        world.put(0, 0, 0, lava());
        world.run(600);
        assert_eq!(world.amount(1, 0, 0), 6);
        assert_eq!(world.amount(3, 0, 0), 2);
        assert!(world.at(4, 0, 0).fluid_state().is_empty());

        let mut nether = World::new();
        nether.settings.fast_lava = true;
        nether.put(0, 0, 0, lava());
        nether.run(600);
        assert_eq!(nether.amount(7, 0, 0), 1);
        assert!(nether.at(8, 0, 0).fluid_state().is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn flow_heads_only_for_the_nearest_way_down() {
        let mut world = World::new();
        world.put(2, -1, 0, AIR);
        world.put(0, 0, 0, water());
        world.run(40);
        assert!(!world.at(1, 0, 0).fluid_state().is_empty());
        assert!(world.at(-1, 0, 0).fluid_state().is_empty());
        assert!(world.at(0, 0, 1).fluid_state().is_empty());
        let fall = world.at(2, -1, 0).fluid_state();
        assert!(fall.is_falling() && fall.amount() == 8);
    }

    #[tuff_core::watchdog]
    #[test]
    fn two_sources_fill_the_gap_only_where_the_rule_allows() {
        for (rule, source) in [(true, true), (false, false)] {
            let mut world = World::new();
            world.settings.water_source_conversion = rule;
            world.put(0, 0, 0, water());
            world.put(2, 0, 0, water());
            world.run(100);
            assert_eq!(world.at(1, 0, 0).fluid_state().is_source(), source);
        }
        let mut world = World::new();
        world.put(0, 0, 0, lava());
        world.put(2, 0, 0, lava());
        world.run(300);
        assert!(!world.at(1, 0, 0).fluid_state().is_source());
        world.settings.lava_source_conversion = true;
        world.put(1, 0, 1, STONE);
        world.run(300);
        assert!(world.at(1, 0, 0).fluid_state().is_source());
    }

    #[tuff_core::watchdog]
    #[test]
    fn flowing_water_drains_once_its_source_is_gone() {
        let mut world = World::new();
        world.put(0, 0, 0, water());
        world.run(100);
        world.put(0, 0, 0, AIR);
        world.run(200);
        for x in -8..=8 {
            assert!(world.at(x, 0, 0).fluid_state().is_empty(), "x {x}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn lava_hardens_against_water() {
        let mut world = World::new();
        world.put(0, 0, 0, water());
        world.put(1, 0, 0, lava());
        assert_eq!(world.at(1, 0, 0).block(), Block::Obsidian);

        let mut world = World::new();
        world.put(0, 0, 0, lava());
        world.run(100);
        assert_eq!(world.amount(1, 0, 0), 6);
        world.put(1, 0, 1, water());
        assert_eq!(world.at(1, 0, 0).block(), Block::Cobblestone);
        assert_eq!(world.fizzed.len(), 4, "every flowing neighbour hardens");
        assert_eq!(world.at(0, 0, 1).block(), Block::Cobblestone);

        let mut world = World::new();
        world.put(0, -1, 0, Block::SoulSoil.default_state());
        world.put(1, 0, 0, Block::BlueIce.default_state());
        world.put(0, 0, 0, lava());
        assert_eq!(world.at(0, 0, 0).block(), Block::Basalt);
    }

    #[tuff_core::watchdog]
    #[test]
    fn lava_falling_onto_water_turns_it_to_stone() {
        let mut world = World::new();
        world.put(0, 0, 0, water());
        world.put(0, 1, 0, STONE);
        world.put(0, 2, 0, lava());
        world.put(0, 1, 0, AIR);
        world.run(100);
        assert_eq!(world.at(0, 0, 0).block(), Block::Stone);
    }

    #[tuff_core::watchdog]
    #[test]
    fn flows_wash_plants_away_and_lava_burns_them() {
        let mut world = World::new();
        let torch = Block::Torch.default_state();
        world.put(1, 0, 0, torch);
        world.put(0, 0, 0, water());
        world.run(20);
        assert_eq!(world.washed, vec![BlockPos::new(1, 0, 0)]);
        assert_eq!(world.at(1, 0, 0).block(), Block::Water);

        let mut world = World::new();
        world.put(1, 0, 0, torch);
        world.put(0, 0, 0, lava());
        world.run(100);
        assert!(world.washed.is_empty());
        assert_eq!(world.fizzed, vec![BlockPos::new(1, 0, 0)]);
        assert_eq!(world.at(1, 0, 0).block(), Block::Lava);
    }

    #[tuff_core::watchdog]
    #[test]
    fn waterlogged_blocks_feed_flow_but_flow_never_waterlogs() {
        let mut world = World::new();
        let slab = Block::OakSlab.default_state();
        world.put(0, 0, 0, slab.with_waterlogged(true).unwrap());
        world.schedule(BlockPos::new(0, 0, 0), Fluid::Water, 5);
        world.put(-3, 0, 0, slab);
        world.run(100);
        assert_eq!(world.amount(1, 0, 0), 7);
        assert_eq!(world.amount(-2, 0, 0), 6);
        assert_eq!(world.at(-3, 0, 0), slab);
    }

    #[tuff_core::watchdog]
    #[test]
    fn falling_water_next_to_a_wall_pulls_down() {
        let mut world = World::new();
        world.put(0, 0, 0, Block::Water.default_state().with_level(8).unwrap());
        world.put(1, 0, 0, STONE);
        let [_, y, _] = flow(&|p| world.block(p), BlockPos::new(0, 0, 0));
        assert!(y < -0.9);
        let mut world = World::new();
        world.put(0, 0, 0, Block::Water.default_state().with_level(1).unwrap());
        let [x, _, _] = flow(&|p| world.block(p), BlockPos::new(0, 0, 0));
        assert!(x.abs() < 1e-9, "a lone block flows nowhere on flat ground");
    }
}
