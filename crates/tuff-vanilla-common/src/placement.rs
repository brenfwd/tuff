//! Block placement and support: which state a block item places, whether a
//! block can stay where it is, and how a block reacts when a neighbour
//! changes. Everything reads the world through a [`Level`] lookup, so the
//! rules are plain functions of the surrounding blocks.

use tuff_vanilla_data::block::{
    Axis, Face as Attach, Facing, Half, Hinge, Instrument, Part, Shape, Type,
};
use tuff_vanilla_data::tag::block as tags;
use tuff_vanilla_data::{Block, BlockKind, BlockState, Item, Support};

use crate::{block_support, scaffolding, speleothem};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{ChunkShape, is_air};

pub use crate::block_support::chorus_plant_state;
pub use tuff_core::world::placement::*;

const AIR: BlockState = Block::Air.default_state();

/// Ticks between a dirt path or farmland being covered and turning back.
const COVERED_TICK_DELAY: u32 = 1;

/// Items placed by aiming at a water source surface rather than by
/// clicking a block.
pub fn places_on_water(item: Item) -> bool {
    matches!(item, Item::LilyPad | Item::Frogspawn)
}

/// A player's click with `item`, with where the block goes decided.
#[allow(clippy::too_many_arguments)]
pub fn context(
    clicked: BlockPos,
    face: Facing,
    cursor: [f32; 3],
    yaw: f32,
    pitch: f32,
    sneaking: bool,
    item: Item,
    shape: ChunkShape,
    level: Level,
) -> Context {
    let mut context = Context::at_clicked(clicked, face, cursor, yaw, pitch, sneaking, item, shape);
    if item == Item::Scaffolding && scaffolding::is_scaffolding(level(clicked).block()) {
        match scaffolding::extension(clicked, face, context.horizontal(), sneaking, shape, level) {
            Some((pos, direction)) => {
                context.pos = pos;
                context.face = direction;
            }
            None => context.blocked = true,
        }
        return context;
    }
    context.replace_clicked = can_be_replaced(level(clicked), &context);
    if !context.replace_clicked {
        context.pos = step(clicked, face);
    }
    context
}

/// Whether the position to fill can take the block at all.
pub fn can_place(context: &Context, level: Level) -> bool {
    !context.blocked && (context.replace_clicked || can_be_replaced(level(context.pos), context))
}

/// The nearest of the 16 rotations of signs, banners and skulls.
fn rotation(degrees: f32) -> u8 {
    ((degrees * 16.0 / 360.0).round() as i64 & 15) as u8
}

/// Whether placing with `context` may overwrite `existing`.
pub fn can_be_replaced(existing: BlockState, context: &Context) -> bool {
    let block = existing.block();
    let same_item = block.item() == Some(context.item);
    let replaceable = existing.is_replaceable() && !same_item;
    let stack = |count: Option<u8>, max: u8| {
        (!context.sneaking && same_item && count.is_some_and(|c| c < max)) || replaceable
    };
    match block.kind() {
        BlockKind::Slab => {
            let Some(kind) = existing.r#type() else {
                return false;
            };
            if kind == Type::Double || !same_item {
                return false;
            }
            if !context.replace_clicked {
                return true;
            }
            let upper = context.click()[1] > 0.5;
            let face = context.face;
            match kind {
                Type::Bottom => face == Facing::Up || (upper && face.is_horizontal()),
                _ => face == Facing::Down || (!upper && face.is_horizontal()),
            }
        }
        BlockKind::LeafLitter | BlockKind::FlowerBed => {
            stack(existing.segment_amount(), 4) || stack(existing.flower_amount(), 4)
        }
        BlockKind::Candle => stack(existing.candles(), 4),
        BlockKind::SeaPickle => stack(existing.pickles(), 4),
        BlockKind::TurtleEgg => stack(existing.eggs(), 4),
        BlockKind::SnowLayer => {
            let layers = existing.layers().unwrap_or(1);
            if !same_item || layers >= 8 {
                layers == 1
            } else {
                !context.replace_clicked || context.face == Facing::Up
            }
        }
        BlockKind::Vine => faces(existing).count() < 5,
        BlockKind::Multiface => !same_item || faces(existing).count() < 6,
        _ => replaceable,
    }
}

/// The state `context` places, or `None` if nothing can be placed there.
pub fn placement_state(context: &Context, level: Level) -> Option<BlockState> {
    let block = context.item.block()?;
    let pos = context.pos;
    if let Some((wall, attach)) = context.item.wall_block() {
        let wall_state = state_for(wall, context, level);
        return context
            .nearest_for_placement()
            .into_iter()
            .filter(|direction| *direction != attach.opposite())
            .find_map(|direction| {
                let state = if direction == attach {
                    state_for(block, context, level)
                } else {
                    wall_state
                }?;
                can_survive(state, pos, level).then_some(state)
            });
    }
    let state = state_for(block, context, level)?;
    can_survive(state, pos, level).then_some(state)
}

/// The other blocks a placed state brings with it, like a door's upper half.
pub fn companions(state: BlockState, pos: BlockPos) -> Option<(BlockPos, BlockState)> {
    match state.block().kind() {
        BlockKind::Door | BlockKind::DoublePlant if state.half() == Some(Half::Lower) => {
            Some((step(pos, Facing::Up), state.with_half(Half::Upper)?))
        }
        BlockKind::Bed if state.part() == Some(Part::Foot) => {
            Some((step(pos, state.facing()?), state.with_part(Part::Head)?))
        }
        _ => None,
    }
}

/// The part of a two-block structure that must go without drops when
/// `state` is broken by a player who gets no drops from it.
pub fn partner(state: BlockState, pos: BlockPos) -> Option<BlockPos> {
    match state.block().kind() {
        BlockKind::Door | BlockKind::DoublePlant if state.half() == Some(Half::Upper) => {
            Some(step(pos, Facing::Down))
        }
        BlockKind::Bed if state.part() == Some(Part::Foot) => Some(step(pos, state.facing()?)),
        _ => None,
    }
}

fn is_water_source(state: BlockState) -> bool {
    state.fluid_state().fluid() == tuff_vanilla_data::fluid::Fluid::Water
}

/// Any water, source or flowing: what makes a concrete powder block
/// solidify, as opposed to [`is_water_source`]'s stricter check (used for
/// waterlogging, which only takes a full source).
fn is_water_fluid(state: BlockState) -> bool {
    matches!(
        state.fluid_state().fluid(),
        tuff_vanilla_data::fluid::Fluid::Water | tuff_vanilla_data::fluid::Fluid::FlowingWater
    )
}

/// Whether `pos` is water itself, or touches water on any of its six
/// faces: what makes a concrete powder block solidify into concrete,
/// whether by being placed there, falling into it, or a neighbour
/// changing into water beside it.
pub fn touches_water(pos: BlockPos, level: Level) -> bool {
    let here = is_water_fluid(level(pos));
    here || UPDATE_ORDER.iter().any(|&d| {
        if d == Facing::Down && !here {
            return false;
        }
        let neighbour = level(step(pos, d));
        is_water_fluid(neighbour) && !neighbour.is_face_sturdy(d.opposite(), Support::Full)
    })
}

/// The concrete a powder block solidifies into on contact with water, if
/// `block` is a concrete powder block at all. Derived from the block's own
/// name (every dyed variant is `<color>_concrete_powder`, matching
/// `<color>_concrete`) instead of a hand-written per-color table.
pub fn concrete_from_powder(block: Block) -> Option<Block> {
    let concrete_name = block.name().strip_suffix("_powder")?;
    concrete_name.ends_with("_concrete").then_some(())?;
    Block::from_name(concrete_name)
}

fn or_keep(state: BlockState, change: Option<BlockState>) -> BlockState {
    change.unwrap_or(state)
}

/// The block's own choice of state for `context`, before the survival check.
fn state_for(block: Block, context: &Context, level: Level) -> Option<BlockState> {
    let pos = context.pos;
    if let Some(concrete) = concrete_from_powder(block)
        && touches_water(pos, level)
    {
        return Some(concrete.default_state());
    }
    let existing = level(pos);
    let water = is_water_source(existing);
    let base = or_keep(
        block.default_state(),
        block.default_state().with_waterlogged(water),
    );
    let horizontal = context.horizontal();
    let above = step(pos, Facing::Up);
    let horizontal_search = |facing_for: fn(Facing) -> Facing| {
        context
            .nearest_for_placement()
            .into_iter()
            .filter(|d| d.is_horizontal())
            .find_map(|d| {
                let state = base.with_facing(facing_for(d))?;
                can_survive(state, pos, level).then_some(state)
            })
    };
    let stacked = |count: Option<u8>, max: u8, with: fn(BlockState, u8) -> Option<BlockState>| {
        (existing.block() == block)
            .then(|| count.and_then(|c| with(existing, (c + 1).min(max))))
            .flatten()
    };
    if is_chest(block) {
        return chest_state(base, context, level);
    }
    if scaffolding::is_scaffolding(block) {
        return Some(scaffolding::with_support(base, pos, level));
    }
    if speleothem::is_speleothem(block) {
        return speleothem::placement(block, pos, context.pitch, context.sneaking, water, level);
    }
    if let Some(state) = block_support::placement(block, base, context, level) {
        return state;
    }
    Some(match block.kind() {
        BlockKind::Path | BlockKind::Farmland if !can_survive(base, pos, level) => {
            block.base_block()?.default_state()
        }
        BlockKind::Pillar => or_keep(base, base.with_axis(context.face.axis())),
        BlockKind::FacingOpposite | BlockKind::Diode => {
            or_keep(base, base.with_facing(horizontal.opposite()))
        }
        BlockKind::FacingPlayer => {
            let state = or_keep(base, base.with_facing(horizontal));
            let state = or_keep(state, state.with_lit(!water));
            or_keep(
                state,
                state.with_signal_fire(level(step(pos, Facing::Down)).block() == Block::HayBlock),
            )
        }
        BlockKind::Anvil => or_keep(base, base.with_facing(horizontal.clockwise())),
        BlockKind::FacingLookOpposite => {
            or_keep(base, base.with_facing(context.nearest()[0].opposite()))
        }
        BlockKind::Observer => or_keep(base, base.with_facing(context.nearest()[0])),
        BlockKind::FacingClicked | BlockKind::AmethystCluster => {
            or_keep(base, base.with_facing(context.face))
        }
        BlockKind::EndRod => {
            let behind = level(step(pos, context.face.opposite()));
            let facing = if behind.block() == block && behind.facing() == Some(context.face) {
                context.face.opposite()
            } else {
                context.face
            };
            or_keep(base, base.with_facing(facing))
        }
        BlockKind::Hopper => {
            let facing = match context.face.opposite() {
                Facing::Up | Facing::Down => Facing::Down,
                other => other,
            };
            or_keep(base, base.with_facing(facing))
        }
        BlockKind::Crafter => {
            let front = context.nearest()[0].opposite();
            let top = match front {
                Facing::Down => horizontal.opposite(),
                Facing::Up => horizontal,
                _ => Facing::Up,
            };
            or_keep(
                base,
                base.with("orientation", &format!("{}_{}", front.name(), top.name())),
            )
        }
        BlockKind::Jigsaw => {
            let top = if context.face.is_horizontal() {
                Facing::Up
            } else {
                horizontal.opposite()
            };
            let name = format!("{}_{}", context.face.name(), top.name());
            or_keep(base, base.with("orientation", &name))
        }
        BlockKind::Stairs => {
            let half = if context.upper_half() {
                Half::Top
            } else {
                Half::Bottom
            };
            let state = base.with_facing(horizontal)?.with_half(half)?;
            or_keep(state, state.with_shape(stairs_shape(state, pos, level)))
        }
        BlockKind::Slab => {
            if existing.block() == block {
                let double = existing.with_type(Type::Double)?;
                or_keep(double, double.with_waterlogged(false))
            } else if context.upper_half() {
                base.with_type(Type::Top)?
            } else {
                base.with_type(Type::Bottom)?
            }
        }
        BlockKind::Trapdoor => {
            if context.replace_clicked || !context.face.is_horizontal() {
                let half = if context.face == Facing::Up {
                    Half::Bottom
                } else {
                    Half::Top
                };
                base.with_facing(horizontal.opposite())?.with_half(half)?
            } else {
                let half = if context.click()[1] > 0.5 {
                    Half::Top
                } else {
                    Half::Bottom
                };
                base.with_facing(context.face)?.with_half(half)?
            }
        }
        BlockKind::Door => {
            if !context.shape.contains_y(above.y) || !can_be_replaced(level(above), context) {
                return None;
            }
            base.with_facing(horizontal)?
                .with_hinge(hinge(context, level))?
                .with_half(Half::Lower)?
        }
        BlockKind::Bed => {
            if !can_be_replaced(level(step(pos, horizontal)), context) {
                return None;
            }
            base.with_facing(horizontal)?.with_part(Part::Foot)?
        }
        BlockKind::DoublePlant => {
            if !context.shape.contains_y(above.y) || !can_be_replaced(level(above), context) {
                return None;
            }
            if block == Block::TallSeagrass && !is_water_source(level(above)) {
                return None;
            }
            let state = or_keep(base, base.with_half(Half::Lower));
            if block == Block::SmallDripleaf {
                or_keep(state, state.with_facing(horizontal.opposite()))
            } else {
                state
            }
        }
        BlockKind::Seagrass | BlockKind::GrowingPlantUp if !water && block == Block::Kelp => {
            return None;
        }
        BlockKind::Seagrass
            if !(is_water_fluid(existing) && existing.fluid_state().amount() == 8) =>
        {
            return None;
        }
        BlockKind::Propagule => or_keep(base, base.with_age(4)),
        BlockKind::Fence => {
            let mut state = base;
            for direction in Facing::HORIZONTAL {
                let neighbour = level(step(pos, direction));
                let connects = fence_connects(block, neighbour, direction.opposite());
                state = or_keep(state, state.with(direction.name(), bool_name(connects)));
            }
            state
        }
        BlockKind::Bars => {
            let mut state = base;
            for direction in Facing::HORIZONTAL {
                let neighbour = level(step(pos, direction));
                let connects = bars_connect(neighbour, direction.opposite());
                state = or_keep(state, state.with(direction.name(), bool_name(connects)));
            }
            state
        }
        BlockKind::Wall => {
            let connections = Facing::HORIZONTAL
                .map(|direction| wall_connects(level(step(pos, direction)), direction.opposite()));
            wall_state(base, connections, level(above))
        }
        BlockKind::FenceGate => {
            let sides = match horizontal.axis() {
                Axis::Z => [Facing::West, Facing::East],
                _ => [Facing::North, Facing::South],
            };
            let in_wall = sides.iter().any(|side| is_wall(level(step(pos, *side))));
            let state = base.with_facing(horizontal)?;
            or_keep(state, state.with_in_wall(in_wall))
        }
        BlockKind::StandingSign => or_keep(base, base.with_rotation(rotation(context.yaw + 180.0))),
        BlockKind::Skull => or_keep(base, base.with_rotation(rotation(context.yaw))),
        BlockKind::CeilingHangingSign => {
            let up = level(above);
            let facing = from_yaw(context.yaw);
            let mut attached = !up.is_face_sturdy(Facing::Down, Support::Full) || context.sneaking;
            if !context.sneaking {
                let above_axis = match up.block().kind() {
                    BlockKind::WallHangingSign => up.facing().map(Facing::axis),
                    BlockKind::CeilingHangingSign => up
                        .get("rotation")
                        .and_then(|r| r.parse::<u8>().ok())
                        .filter(|r| r % 4 == 0)
                        .map(|r| from_yaw(f32::from(r) * 22.5).axis()),
                    _ => None,
                };
                if above_axis == Some(facing.axis()) {
                    attached = false;
                }
            }
            let rotation = if attached {
                rotation(context.yaw + 180.0)
            } else {
                rotation(facing.opposite().rotation_degrees())
            };
            base.with_attached(attached)?.with_rotation(rotation)?
        }
        BlockKind::WallSkull => context
            .nearest_for_placement()
            .into_iter()
            .filter(|d| d.is_horizontal())
            .find(|d| !can_be_replaced(level(step(pos, *d)), context))
            .and_then(|d| base.with_facing(d.opposite()))?,
        BlockKind::WallHangingSign => context
            .nearest_for_placement()
            .into_iter()
            .filter(|d| d.is_horizontal() && d.axis() != context.face.axis())
            .filter_map(|d| base.with_facing(d.opposite()))
            .find(|state| wall_hanging_sign_can_place(*state, pos, level))?,
        BlockKind::WallTorch
        | BlockKind::WallSturdy
        | BlockKind::WallSign
        | BlockKind::CoralWallFan => horizontal_search(Facing::opposite)?,
        BlockKind::Cocoa => horizontal_search(|d| d)?,
        BlockKind::FaceAttached | BlockKind::Grindstone => {
            context.nearest_for_placement().into_iter().find_map(|d| {
                let state = match d {
                    Facing::Up => base.with_face(Attach::Ceiling)?.with_facing(horizontal)?,
                    Facing::Down => base.with_face(Attach::Floor)?.with_facing(horizontal)?,
                    _ => base.with_face(Attach::Wall)?.with_facing(d.opposite())?,
                };
                can_survive(state, pos, level).then_some(state)
            })?
        }
        BlockKind::Lantern => context
            .nearest_for_placement()
            .into_iter()
            .filter(|d| !d.is_horizontal())
            .find_map(|d| {
                let state = base.with_hanging(d == Facing::Up)?;
                can_survive(state, pos, level).then_some(state)
            })?,
        BlockKind::Rail => {
            let shape = match horizontal.axis() {
                Axis::X => Shape::EastWest,
                _ => Shape::NorthSouth,
            };
            or_keep(base, base.with_shape(shape))
        }
        BlockKind::LeafLitter => stacked(
            existing.segment_amount(),
            4,
            BlockState::with_segment_amount,
        )
        .or_else(|| base.with_facing(horizontal.opposite()))?,
        BlockKind::FlowerBed => {
            stacked(existing.flower_amount(), 4, BlockState::with_flower_amount)
                .or_else(|| base.with_facing(horizontal.opposite()))?
        }
        BlockKind::Candle => {
            stacked(existing.candles(), 4, BlockState::with_candles).unwrap_or(base)
        }
        BlockKind::SeaPickle => {
            stacked(existing.pickles(), 4, BlockState::with_pickles).unwrap_or(base)
        }
        BlockKind::TurtleEgg => stacked(existing.eggs(), 4, BlockState::with_eggs).unwrap_or(base),
        BlockKind::SnowLayer => {
            stacked(existing.layers(), 8, BlockState::with_layers).unwrap_or(base)
        }
        BlockKind::Leaves => {
            let state = or_keep(base, base.with_persistent(true));
            or_keep(state, state.with_distance(leaf_distance(pos, level)))
        }
        BlockKind::Snowy => or_keep(
            base,
            base.with_snowy(tuff_datapack::tags::is(
                tags::key::SNOW,
                level(above).block(),
            )),
        ),
        BlockKind::NoteBlock => or_keep(base, base.with_instrument(note_instrument(pos, level))),
        BlockKind::HugeMushroom => {
            let mut state = base;
            for direction in Facing::ALL {
                let exposed = level(step(pos, direction)).block() != block;
                state = or_keep(state, state.with(direction.name(), bool_name(exposed)));
            }
            state
        }
        BlockKind::MossyCarpet => mossy_carpet_state(base, pos, level, true),
        BlockKind::Vine => {
            let current = if existing.block() == block {
                existing
            } else {
                base
            };
            context
                .nearest_for_placement()
                .into_iter()
                .filter(|d| *d != Facing::Down && current.get(d.name()) == Some("false"))
                .find(|d| vine_supported(block, pos, *d, level))
                .and_then(|d| current.with(d.name(), "true"))
                .or((existing.block() == block).then_some(existing))?
        }
        BlockKind::Multiface => {
            let current = if existing.block() == block {
                existing
            } else {
                base
            };
            context
                .nearest_for_placement()
                .into_iter()
                .filter(|d| current.get(d.name()) == Some("false"))
                .find(|d| attaches_to(pos, *d, level))
                .and_then(|d| current.with(d.name(), "true"))?
        }
        _ => base,
    })
}

trait Rotation {
    fn rotation_degrees(self) -> f32;
}

impl Rotation for Facing {
    fn rotation_degrees(self) -> f32 {
        match self {
            Facing::South => 0.0,
            Facing::West => 90.0,
            Facing::North => 180.0,
            _ => 270.0,
        }
    }
}

fn bool_name(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// The directions a vine or multiface block covers.
fn faces(state: BlockState) -> impl Iterator<Item = Facing> {
    Facing::ALL
        .into_iter()
        .filter(move |d| state.get(d.name()) == Some("true"))
}

fn hinge(context: &Context, level: Level) -> Hinge {
    let pos = context.pos;
    let direction = context.horizontal();
    let above = step(pos, Facing::Up);
    let (left, right) = (direction.counter_clockwise(), direction.clockwise());
    let full = |p: BlockPos| level(p).is_collision_full_block();
    let lower_door = |p: BlockPos| {
        let state = level(p);
        state.block().kind() == BlockKind::Door && state.half() == Some(Half::Lower)
    };
    let balance = -(full(step(pos, left)) as i32) - full(step(above, left)) as i32
        + full(step(pos, right)) as i32
        + full(step(above, right)) as i32;
    let (door_left, door_right) = (lower_door(step(pos, left)), lower_door(step(pos, right)));
    if (door_left && !door_right) || balance > 0 {
        return Hinge::Right;
    }
    if (door_right && !door_left) || balance < 0 {
        return Hinge::Left;
    }
    let (x, _, z) = direction.step();
    let [click_x, _, click_z] = context.click();
    let left = (x >= 0 || click_z >= 0.5)
        && (x <= 0 || click_z <= 0.5)
        && (z >= 0 || click_x <= 0.5)
        && (z <= 0 || click_x >= 0.5);
    if left { Hinge::Left } else { Hinge::Right }
}

fn is_stairs(state: BlockState) -> bool {
    state.block().kind() == BlockKind::Stairs
}

fn stairs_shape(state: BlockState, pos: BlockPos, level: Level) -> Shape {
    let (Some(facing), half) = (state.facing(), state.half()) else {
        return Shape::Straight;
    };
    let can_take_shape = |direction: Facing| {
        let neighbour = level(step(pos, direction));
        !(is_stairs(neighbour) && neighbour.facing() == Some(facing) && neighbour.half() == half)
    };
    let behind = level(step(pos, facing));
    if is_stairs(behind)
        && behind.half() == half
        && let Some(behind_facing) = behind.facing()
        && behind_facing.axis() != facing.axis()
        && can_take_shape(behind_facing.opposite())
    {
        return if behind_facing == facing.counter_clockwise() {
            Shape::OuterLeft
        } else {
            Shape::OuterRight
        };
    }
    let front = level(step(pos, facing.opposite()));
    if is_stairs(front)
        && front.half() == half
        && let Some(front_facing) = front.facing()
        && front_facing.axis() != facing.axis()
        && can_take_shape(front_facing)
    {
        return if front_facing == facing.counter_clockwise() {
            Shape::InnerLeft
        } else {
            Shape::InnerRight
        };
    }
    Shape::Straight
}

/// Blocks fences, walls and panes never join, even with a solid side.
fn refuses_connections(state: BlockState) -> bool {
    let block = state.block();
    block.kind() == BlockKind::Leaves
        || matches!(
            block,
            Block::Barrier
                | Block::CarvedPumpkin
                | Block::JackOLantern
                | Block::Melon
                | Block::Pumpkin
        )
        || tuff_datapack::tags::is(tags::key::SHULKER_BOXES, block)
}

/// Whether a fence gate joins a fence or wall on its `side`.
fn gate_joins(state: BlockState, side: Facing) -> bool {
    state.block().kind() == BlockKind::FenceGate
        && state
            .facing()
            .is_some_and(|f| f.axis() == side.clockwise().axis())
}

fn is_wall(state: BlockState) -> bool {
    tuff_datapack::tags::is(tags::key::WALLS, state.block())
}

/// Whether a fence of `fence` joins `neighbour`, whose side facing the fence
/// is `side`.
fn fence_connects(fence: Block, neighbour: BlockState, side: Facing) -> bool {
    let wooden = |block: Block| tuff_datapack::tags::is(tags::key::WOODEN_FENCES, block);
    let same_fence = tuff_datapack::tags::is(tags::key::FENCES, neighbour.block())
        && wooden(neighbour.block()) == wooden(fence);
    (!refuses_connections(neighbour) && neighbour.is_face_sturdy(side, Support::Full))
        || same_fence
        || gate_joins(neighbour, side)
}

fn bars_connect(neighbour: BlockState, side: Facing) -> bool {
    (!refuses_connections(neighbour) && neighbour.is_face_sturdy(side, Support::Full))
        || neighbour.block().kind() == BlockKind::Bars
        || is_wall(neighbour)
}

fn wall_connects(neighbour: BlockState, side: Facing) -> bool {
    is_wall(neighbour)
        || (!refuses_connections(neighbour) && neighbour.is_face_sturdy(side, Support::Full))
        || neighbour.block().kind() == BlockKind::Bars
        || gate_joins(neighbour, side)
}

/// A wall with the given north, east, south and west connections under
/// `above`, which makes sections it covers tall.
fn wall_state(state: BlockState, connections: [bool; 4], above: BlockState) -> BlockState {
    let mut state = state;
    for (direction, connected) in Facing::HORIZONTAL.into_iter().zip(connections) {
        let side = match (connected, above.covers_wall(Some(direction))) {
            (false, _) => "none",
            (true, true) => "tall",
            (true, false) => "low",
        };
        state = or_keep(state, state.with(direction.name(), side));
    }
    let post = raises_post(state, above);
    or_keep(state, state.with_up(post))
}

fn raises_post(state: BlockState, above: BlockState) -> bool {
    if is_wall(above) && above.up() == Some(true) {
        return true;
    }
    let side = |d: Facing| state.get(d.name()).unwrap_or("none");
    let none = |d: Facing| side(d) == "none";
    let (north, east, south, west) = (
        none(Facing::North),
        none(Facing::East),
        none(Facing::South),
        none(Facing::West),
    );
    let straight = (!north || !south || !west || !east) && north == south && west == east;
    if !straight {
        return true;
    }
    let tall = |a: Facing, b: Facing| side(a) == "tall" && side(b) == "tall";
    if tall(Facing::North, Facing::South) || tall(Facing::East, Facing::West) {
        return false;
    }
    tuff_datapack::tags::is(tags::key::WALL_POST_OVERRIDE, above.block()) || above.covers_wall(None)
}

fn wall_connections(state: BlockState) -> [bool; 4] {
    Facing::HORIZONTAL.map(|d| state.get(d.name()).is_some_and(|side| side != "none"))
}

/// How far leaves at a block in `state` count as being from a log.
pub fn leaf_distance_at(state: BlockState) -> u8 {
    if tuff_datapack::tags::is(tags::key::PREVENTS_NEARBY_LEAF_DECAY, state.block()) {
        0
    } else if state.block().kind() == BlockKind::Leaves {
        state.distance().unwrap_or(7)
    } else {
        7
    }
}

/// The distance leaves at `pos` have from the nearest log.
pub fn leaf_distance(pos: BlockPos, level: Level) -> u8 {
    Facing::ALL
        .into_iter()
        .map(|direction| leaf_distance_at(level(step(pos, direction))))
        .map(|distance| (distance + 1).min(7))
        .min()
        .unwrap_or(7)
}

/// Whether the block at `pos` offset by `direction` has a full face towards
/// `pos`, which vines and lichen can cover.
fn attaches_to(pos: BlockPos, direction: Facing, level: Level) -> bool {
    let neighbour = level(step(pos, direction));
    neighbour.is_face_sturdy(direction.opposite(), Support::Full)
        || neighbour.is_collision_face_full(direction.opposite())
}

fn wall_side(state: BlockState, direction: Facing) -> &'static str {
    state.get(direction.name()).unwrap_or("none")
}

pub fn mossy_carpet_has_faces(state: BlockState) -> bool {
    state.bottom() == Some(true)
        || Facing::HORIZONTAL
            .into_iter()
            .any(|d| wall_side(state, d) != "none")
}

/// A moss carpet's side shapes recomputed from the walls around it.
pub fn mossy_carpet_state(
    mut state: BlockState,
    pos: BlockPos,
    level: Level,
    create_sides: bool,
) -> BlockState {
    let block = state.block();
    let create_sides = create_sides || state.bottom() == Some(true);
    for direction in Facing::HORIZONTAL {
        let mut side = if attaches_to(pos, direction, level) {
            if create_sides {
                "low"
            } else {
                wall_side(state, direction)
            }
        } else {
            "none"
        };
        if side == "low" {
            let above = level(step(pos, Facing::Up));
            if above.block() == block
                && wall_side(above, direction) != "none"
                && above.bottom() != Some(true)
            {
                side = "tall";
            }
            if state.bottom() != Some(true) {
                let below = level(step(pos, Facing::Down));
                if below.block() == block && wall_side(below, direction) == "none" {
                    side = "none";
                }
            }
        }
        state = or_keep(state, state.with(direction.name(), side));
    }
    state
}

fn vine_supported(vine: Block, pos: BlockPos, direction: Facing, level: Level) -> bool {
    if direction == Facing::Down {
        return false;
    }
    if attaches_to(pos, direction, level) {
        return true;
    }
    let above = level(step(pos, Facing::Up));
    direction.is_horizontal()
        && above.block() == vine
        && above.get(direction.name()) == Some("true")
}

/// Whether the block at the side of `pos` given by `direction` offers
/// `support` on the face towards `pos`.
fn supported(pos: BlockPos, direction: Facing, support: Support, level: Level) -> bool {
    level(step(pos, direction)).is_face_sturdy(direction.opposite(), support)
}

fn wall_hanging_sign_can_place(state: BlockState, pos: BlockPos, level: Level) -> bool {
    let Some(facing) = state.facing() else {
        return false;
    };
    let attaches = |side: Facing, attach_face: Facing| {
        let attach = level(step(pos, side));
        if attach.block().kind() == BlockKind::WallHangingSign {
            attach.facing().map(Facing::axis) == Some(facing.axis())
        } else {
            attach.is_face_sturdy(attach_face, Support::Full)
        }
    };
    attaches(facing.clockwise(), facing.counter_clockwise())
        || attaches(facing.counter_clockwise(), facing.clockwise())
}

/// Whether `state` can stay at `pos`.
pub fn can_survive(state: BlockState, pos: BlockPos, level: Level) -> bool {
    let block = state.block();
    match block {
        Block::Fire => {
            return supported(pos, Facing::Down, Support::Full, level)
                || valid_fire_location(pos, level);
        }
        Block::SoulFire => {
            return tuff_datapack::tags::is(
                tags::key::SOUL_FIRE_BASE_BLOCKS,
                level(step(pos, Facing::Down)).block(),
            );
        }
        _ => {}
    }
    if scaffolding::is_scaffolding(block) {
        return scaffolding::can_survive(pos, level);
    }
    if speleothem::is_speleothem(block) {
        return speleothem::can_survive(state, pos, level);
    }
    if let Some(survives) = block_support::can_survive(state, pos, level) {
        return survives;
    }
    let below = || level(step(pos, Facing::Down));
    let above = || level(step(pos, Facing::Up));
    let on_ground = || block.may_place_on(below());
    match block.kind() {
        BlockKind::Plant | BlockKind::FlowerBed | BlockKind::SeaPickle | BlockKind::Seagrass => {
            on_ground()
        }
        BlockKind::Mushroom => {
            on_ground()
                || tuff_datapack::tags::is(
                    tags::key::OVERRIDES_MUSHROOM_LIGHT_REQUIREMENT,
                    below().block(),
                )
        }
        BlockKind::Propagule => match state.hanging() {
            Some(true) => tuff_datapack::tags::is(
                tags::key::SUPPORTS_HANGING_MANGROVE_PROPAGULE,
                above().block(),
            ),
            _ => on_ground(),
        },
        BlockKind::DoublePlant => match state.half() {
            Some(Half::Upper) => {
                let below = below();
                below.block() == block && below.half() == Some(Half::Lower)
            }
            _ => on_ground(),
        },
        BlockKind::LeafLitter | BlockKind::Coral => {
            supported(pos, Facing::Down, Support::Full, level)
        }
        BlockKind::Torch | BlockKind::Candle => {
            supported(pos, Facing::Down, Support::Center, level)
        }
        BlockKind::WallTorch
        | BlockKind::WallSturdy
        | BlockKind::CoralWallFan
        | BlockKind::AmethystCluster => state
            .facing()
            .is_some_and(|f| supported(pos, f.opposite(), Support::Full, level)),
        BlockKind::WallSign => state
            .facing()
            .is_some_and(|f| level(step(pos, f.opposite())).is_solid()),
        BlockKind::StandingSign | BlockKind::Cake => below().is_solid(),
        BlockKind::CeilingHangingSign => supported(pos, Facing::Up, Support::Center, level),
        BlockKind::SporeBlossom => {
            supported(pos, Facing::Up, Support::Center, level) && !is_water_fluid(level(pos))
        }
        BlockKind::HangingRoots => supported(pos, Facing::Up, Support::Full, level),
        BlockKind::Carpet => !is_air(below()),
        BlockKind::MossyCarpet => match state.bottom() {
            Some(false) => {
                let below = below();
                below.block() == block && below.bottom() == Some(true)
            }
            _ => !is_air(below()),
        },
        BlockKind::PressurePlate => {
            supported(pos, Facing::Down, Support::Rigid, level)
                || supported(pos, Facing::Down, Support::Center, level)
        }
        BlockKind::Rail | BlockKind::Diode => supported(pos, Facing::Down, Support::Rigid, level),
        BlockKind::RedstoneWire => {
            supported(pos, Facing::Down, Support::Full, level) || below().block() == Block::Hopper
        }
        BlockKind::FaceAttached => {
            let toward = match state.face() {
                Some(Attach::Ceiling) => Some(Facing::Up),
                Some(Attach::Floor) => Some(Facing::Down),
                _ => state.facing().map(Facing::opposite),
            };
            toward.is_some_and(|d| supported(pos, d, Support::Full, level))
        }
        BlockKind::Door => match state.half() {
            Some(Half::Upper) => below().block() == block,
            _ => supported(pos, Facing::Down, Support::Full, level),
        },
        BlockKind::Lantern => match state.hanging() {
            Some(true) => supported(pos, Facing::Up, Support::Center, level),
            _ => supported(pos, Facing::Down, Support::Center, level),
        },
        BlockKind::SnowLayer => {
            let below = below();
            let ground = below.block();
            !tuff_datapack::tags::is(tags::key::CANNOT_SUPPORT_SNOW_LAYER, ground)
                && (tuff_datapack::tags::is(tags::key::SUPPORT_OVERRIDE_SNOW_LAYER, ground)
                    || below.is_collision_face_full(Facing::Up)
                    || (ground == block && below.layers() == Some(8)))
        }
        BlockKind::Cactus => {
            let clear = Facing::HORIZONTAL.into_iter().all(|d| {
                let side = level(step(pos, d));
                !side.is_solid() && side.block() != Block::Lava
            });
            let ground = below().block();
            clear
                && (ground == block || tuff_datapack::tags::is(tags::key::SUPPORTS_CACTUS, ground))
                && !above().is_liquid()
        }
        BlockKind::SugarCane => {
            let ground = below().block();
            if ground == block {
                return true;
            }
            let ground_pos = step(pos, Facing::Down);
            tuff_datapack::tags::is(tags::key::SUPPORTS_SUGAR_CANE, ground)
                && Facing::HORIZONTAL.into_iter().any(|d| {
                    let side = level(step(ground_pos, d));
                    tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::fluid::key::SUPPORTS_SUGAR_CANE_ADJACENTLY,
                        side.fluid_state().fluid(),
                    ) || tuff_datapack::tags::is(
                        tags::key::SUPPORTS_SUGAR_CANE_ADJACENTLY,
                        side.block(),
                    )
                })
        }
        BlockKind::Bamboo => tuff_datapack::tags::is(tags::key::SUPPORTS_BAMBOO, below().block()),
        BlockKind::Cocoa => state.facing().is_some_and(|f| {
            tuff_datapack::tags::is(tags::key::SUPPORTS_COCOA, level(step(pos, f)).block())
        }),
        BlockKind::GrowingPlantUp | BlockKind::GrowingPlantDown => {
            let growth = if block.kind() == BlockKind::GrowingPlantUp {
                Facing::Up
            } else {
                Facing::Down
            };
            let base = level(step(pos, growth.opposite()));
            let kelp = matches!(block, Block::Kelp | Block::KelpPlant);
            if kelp && tuff_datapack::tags::is(tags::key::CANNOT_SUPPORT_KELP, base.block()) {
                return false;
            }
            base.block().kind() == block.kind() || base.is_face_sturdy(growth, Support::Full)
        }
        BlockKind::Vine => {
            let mut faces = faces(state).peekable();
            faces.peek().is_some() && faces.all(|d| vine_supported(block, pos, d, level))
        }
        BlockKind::Multiface => {
            let mut faces = faces(state).peekable();
            faces.peek().is_some() && faces.all(|d| attaches_to(pos, d, level))
        }
        BlockKind::Path => {
            let above = above();
            !above.is_solid() || above.block().kind() == BlockKind::FenceGate
        }
        BlockKind::Farmland => {
            let above = above();
            !above.is_solid()
                || tuff_datapack::tags::is(tags::key::MAINTAINS_FARMLAND, above.block())
        }
        _ => true,
    }
}

mod chest;
mod fire;
mod growth;
pub use chest::*;
pub use fire::*;
use growth::*;

mod shape;
pub use shape::*;

#[cfg(test)]
mod tests;
